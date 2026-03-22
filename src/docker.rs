use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bollard::container::LogOutput;
use bollard::query_parameters::{
    EventsOptionsBuilder, InspectContainerOptionsBuilder, ListContainersOptionsBuilder,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder, RestartContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use bollard::{Docker, models::ContainerInspectResponse, models::ContainerSummary};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::model::{ContainerDetails, ContainerRecord, LogEntry};

#[derive(Clone, Debug)]
pub enum DockerCommand {
    RefreshContainers,
    LoadInspect { id: String },
    WatchLogs { id: String },
    Action { id: String, action: ContainerAction },
}

#[derive(Clone, Copy, Debug)]
pub enum ContainerAction {
    StartStop,
    Restart,
    Remove,
}

#[derive(Debug)]
pub enum DockerEvent {
    Connected(String),
    ContainersUpdated(Vec<ContainerRecord>),
    InspectLoaded {
        id: String,
        details: ContainerDetails,
    },
    LogsReset {
        id: String,
    },
    LogsReady {
        id: String,
    },
    LogChunk {
        id: String,
        entries: Vec<LogEntry>,
    },
    OperationSucceeded(String),
    OperationFailed(String),
}

pub struct DockerManager {
    command_tx: UnboundedSender<DockerCommand>,
    event_rx: UnboundedReceiver<DockerEvent>,
}

impl DockerManager {
    pub async fn spawn(host: Option<String>, log_backlog_lines: usize) -> Result<Self> {
        let docker = connect_docker(host)?;
        let negotiated = docker.clone();
        let docker = negotiated.negotiate_version().await.unwrap_or(docker);

        let version = docker
            .version()
            .await
            .map(|response| {
                response
                    .version
                    .unwrap_or_else(|| "unknown Docker version".into())
            })
            .unwrap_or_else(|_| "Docker connected".into());

        let docker = Arc::new(docker);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let worker = Worker::new(docker.clone(), event_tx.clone(), log_backlog_lines);
        let watch_command_tx = command_tx.clone();
        tokio::spawn(async move {
            worker.run(command_rx).await;
        });

        let watch_event_tx = event_tx.clone();
        tokio::spawn(async move {
            watch_docker_events(docker, watch_command_tx, watch_event_tx).await;
        });

        event_tx
            .send(DockerEvent::Connected(format!("Connected to {version}")))
            .context("failed to publish connected event")?;
        command_tx
            .send(DockerCommand::RefreshContainers)
            .context("failed to queue initial refresh")?;
        Ok(Self {
            command_tx,
            event_rx,
        })
    }

    pub fn send(&mut self, command: DockerCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .context("docker worker is no longer available")
    }

    pub async fn recv(&mut self) -> Option<DockerEvent> {
        self.event_rx.recv().await
    }
}

struct Worker {
    docker: Arc<Docker>,
    event_tx: UnboundedSender<DockerEvent>,
    log_backlog_lines: usize,
    log_task: Option<JoinHandle<()>>,
}

impl Worker {
    fn new(
        docker: Arc<Docker>,
        event_tx: UnboundedSender<DockerEvent>,
        log_backlog_lines: usize,
    ) -> Self {
        Self {
            docker,
            event_tx,
            log_backlog_lines,
            log_task: None,
        }
    }

    async fn run(mut self, mut command_rx: UnboundedReceiver<DockerCommand>) {
        while let Some(command) = command_rx.recv().await {
            match command {
                DockerCommand::RefreshContainers => {
                    if let Err(error) = self.refresh_containers().await {
                        let _ = self.event_tx.send(DockerEvent::OperationFailed(format!(
                            "Refresh failed: {error}"
                        )));
                    }
                }
                DockerCommand::LoadInspect { id } => {
                    if let Err(error) = self.load_inspect(&id).await {
                        let _ = self.event_tx.send(DockerEvent::OperationFailed(format!(
                            "Inspect failed: {error}"
                        )));
                    }
                }
                DockerCommand::WatchLogs { id } => self.watch_logs(id),
                DockerCommand::Action { id, action } => {
                    if let Err(error) = self.perform_action(&id, action).await {
                        let _ = self.event_tx.send(DockerEvent::OperationFailed(format!(
                            "Action failed for {id}: {error}"
                        )));
                    }
                }
            }
        }
    }

    async fn refresh_containers(&self) -> Result<()> {
        let options = ListContainersOptionsBuilder::default().all(true).build();
        let containers = self.docker.list_containers(Some(options)).await?;
        let parsed = containers
            .into_iter()
            .map(parse_summary)
            .collect::<Result<Vec<_>>>()?;
        self.event_tx
            .send(DockerEvent::ContainersUpdated(parsed))
            .context("failed to publish containers")?;
        Ok(())
    }

    async fn load_inspect(&self, id: &str) -> Result<()> {
        let options = InspectContainerOptionsBuilder::default()
            .size(false)
            .build();
        let inspect = self.docker.inspect_container(id, Some(options)).await?;
        let details = parse_details(id, inspect, self.docker.clone()).await?;
        self.event_tx
            .send(DockerEvent::InspectLoaded {
                id: id.to_string(),
                details,
            })
            .context("failed to publish inspect details")?;
        Ok(())
    }

    fn watch_logs(&mut self, id: String) {
        if let Some(task) = self.log_task.take() {
            task.abort();
        }

        let docker = self.docker.clone();
        let event_tx = self.event_tx.clone();
        let backlog = self.log_backlog_lines;
        self.log_task = Some(tokio::spawn(async move {
            let _ = event_tx.send(DockerEvent::LogsReset { id: id.clone() });
            let backlog_options = LogsOptionsBuilder::default()
                .follow(false)
                .stdout(true)
                .stderr(true)
                .timestamps(true)
                .tail(&backlog.to_string())
                .build();

            let mut backlog_stream = docker.logs(&id, Some(backlog_options));
            let mut backlog_entries = Vec::new();
            let mut replay_gate = LogReplayGate::default();
            let mut since = current_unix_seconds();

            while let Some(message) = backlog_stream.next().await {
                match message {
                    Ok(output) => {
                        for entry in parse_log_output(output) {
                            since = log_since_marker(&entry).unwrap_or(since);
                            replay_gate.remember(&entry);
                            backlog_entries.push(entry);
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.send(DockerEvent::OperationFailed(format!(
                            "Backlog fetch failed for {id}: {error}"
                        )));
                        return;
                    }
                }
            }

            if !backlog_entries.is_empty() {
                let _ = event_tx.send(DockerEvent::LogChunk {
                    id: id.clone(),
                    entries: backlog_entries,
                });
            }

            let _ = event_tx.send(DockerEvent::LogsReady { id: id.clone() });

            loop {
                let options = LogsOptionsBuilder::default()
                    .follow(true)
                    .stdout(true)
                    .stderr(true)
                    .timestamps(true)
                    .since(since)
                    .tail("0")
                    .build();

                let mut stream = docker.logs(&id, Some(options));
                let mut saw_bytes = false;

                while let Some(message) = stream.next().await {
                    match message {
                        Ok(output) => {
                            let mut new_entries = Vec::new();
                            for entry in parse_log_output(output) {
                                if !replay_gate.accepts(&entry) {
                                    continue;
                                }
                                since = log_since_marker(&entry).unwrap_or(since);
                                new_entries.push(entry);
                            }

                            if !new_entries.is_empty() {
                                saw_bytes = true;
                                let _ = event_tx.send(DockerEvent::LogChunk {
                                    id: id.clone(),
                                    entries: new_entries,
                                });
                            }
                        }
                        Err(error) => {
                            let _ = event_tx.send(DockerEvent::OperationFailed(format!(
                                "Log stream dropped for {id}: {error}"
                            )));
                            break;
                        }
                    }
                }

                if !saw_bytes {
                    break;
                }

                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }));
    }

    async fn perform_action(&self, id: &str, action: ContainerAction) -> Result<()> {
        match action {
            ContainerAction::StartStop => {
                let inspect = self.docker.inspect_container(id, None).await?;
                let running = serde_json::to_value(inspect)?
                    .get("State")
                    .and_then(Value::as_object)
                    .and_then(|state| state.get("Running"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                if running {
                    let options = StopContainerOptionsBuilder::default().t(10).build();
                    self.docker.stop_container(id, Some(options)).await?;
                    self.event_tx
                        .send(DockerEvent::OperationSucceeded(format!("Stopped {id}")))?;
                } else {
                    self.docker
                        .start_container(
                            id,
                            None::<bollard::query_parameters::StartContainerOptions>,
                        )
                        .await?;
                    self.event_tx
                        .send(DockerEvent::OperationSucceeded(format!("Started {id}")))?;
                }
            }
            ContainerAction::Restart => {
                let options = RestartContainerOptionsBuilder::default().t(10).build();
                self.docker.restart_container(id, Some(options)).await?;
                self.event_tx
                    .send(DockerEvent::OperationSucceeded(format!("Restarted {id}")))?;
            }
            ContainerAction::Remove => {
                let options = RemoveContainerOptionsBuilder::default().force(true).build();
                self.docker.remove_container(id, Some(options)).await?;
                self.event_tx
                    .send(DockerEvent::OperationSucceeded(format!("Removed {id}")))?;
            }
        }
        Ok(())
    }
}

async fn watch_docker_events(
    docker: Arc<Docker>,
    command_tx: UnboundedSender<DockerCommand>,
    event_tx: UnboundedSender<DockerEvent>,
) {
    loop {
        let options = EventsOptionsBuilder::default().build();
        let mut stream = docker.events(Some(options));

        while let Some(item) = stream.next().await {
            match item {
                Ok(_) => {
                    let _ = command_tx.send(DockerCommand::RefreshContainers);
                }
                Err(error) => {
                    let _ = event_tx.send(DockerEvent::OperationFailed(format!(
                        "Docker event stream disconnected: {error}"
                    )));
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn connect_docker(host: Option<String>) -> Result<Docker> {
    match host {
        Some(host) => Docker::connect_with_host(&host)
            .with_context(|| format!("failed to connect to Docker host {host}")),
        None => Docker::connect_with_defaults().context("failed to connect to local Docker"),
    }
}

fn parse_summary(summary: ContainerSummary) -> Result<ContainerRecord> {
    let value = serde_json::to_value(summary)?;
    ContainerRecord::from_summary_value(value)
}

async fn parse_details(
    id: &str,
    inspect: ContainerInspectResponse,
    docker: Arc<Docker>,
) -> Result<ContainerDetails> {
    let summary = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::default().all(true).build(),
        ))
        .await?
        .into_iter()
        .map(parse_summary)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|container| container.id == id)
        .context("container no longer available")?;

    let value = serde_json::to_value(inspect)?;
    Ok(ContainerDetails::from_inspect_value(&summary, value))
}

fn parse_log_output(output: LogOutput) -> Vec<LogEntry> {
    let (stream, message) = match output {
        LogOutput::StdErr { message } => ("stderr", message),
        LogOutput::StdOut { message } => ("stdout", message),
        LogOutput::StdIn { message } => ("stdin", message),
        LogOutput::Console { message } => ("console", message),
    };

    let text = String::from_utf8_lossy(&message);
    split_log_lines(stream, &text)
}

fn split_log_lines(stream: &str, message: &str) -> Vec<LogEntry> {
    if message.is_empty() {
        return Vec::new();
    }

    message
        .split_inclusive('\n')
        .map(|segment| LogEntry::parse(stream, segment))
        .collect()
}

fn log_signature(entry: &LogEntry) -> String {
    format!(
        "{}|{}|{}",
        entry.stream,
        entry.timestamp.as_deref().unwrap_or(""),
        entry.message
    )
}

#[derive(Default)]
struct LogReplayGate {
    last_timestamp: Option<String>,
    seen_at_last_timestamp: HashSet<String>,
    last_untimestamped_signature: Option<String>,
}

impl LogReplayGate {
    fn remember(&mut self, entry: &LogEntry) {
        let signature = log_signature(entry);
        match entry
            .timestamp
            .as_deref()
            .filter(|timestamp| !timestamp.is_empty())
        {
            Some(timestamp) => {
                if self.last_timestamp.as_deref() != Some(timestamp) {
                    self.last_timestamp = Some(timestamp.to_string());
                    self.seen_at_last_timestamp.clear();
                }
                self.seen_at_last_timestamp.insert(signature);
                self.last_untimestamped_signature = None;
            }
            None => {
                self.last_untimestamped_signature = Some(signature);
            }
        }
    }

    fn accepts(&mut self, entry: &LogEntry) -> bool {
        let signature = log_signature(entry);
        match entry
            .timestamp
            .as_deref()
            .filter(|timestamp| !timestamp.is_empty())
        {
            Some(timestamp) => match self.last_timestamp.as_deref() {
                Some(last_timestamp) if timestamp < last_timestamp => false,
                Some(last_timestamp)
                    if timestamp == last_timestamp
                        && self.seen_at_last_timestamp.contains(&signature) =>
                {
                    false
                }
                Some(last_timestamp) if timestamp == last_timestamp => {
                    self.seen_at_last_timestamp.insert(signature);
                    self.last_untimestamped_signature = None;
                    true
                }
                _ => {
                    self.last_timestamp = Some(timestamp.to_string());
                    self.seen_at_last_timestamp.clear();
                    self.seen_at_last_timestamp.insert(signature);
                    self.last_untimestamped_signature = None;
                    true
                }
            },
            None => {
                if self.last_untimestamped_signature.as_deref() == Some(signature.as_str()) {
                    return false;
                }
                self.last_untimestamped_signature = Some(signature);
                true
            }
        }
    }
}

fn log_since_marker(entry: &LogEntry) -> Option<i32> {
    let timestamp = entry.timestamp.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|timestamp| i32::try_from(timestamp.timestamp()).ok())
}

fn current_unix_seconds() -> i32 {
    i32::try_from(chrono::Utc::now().timestamp()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_gate_skips_replayed_entries_for_last_timestamp() {
        let mut gate = LogReplayGate::default();
        let first = LogEntry::parse("stdout", "2026-03-22T10:00:00.100000000Z first\n");
        let second = LogEntry::parse("stdout", "2026-03-22T10:00:00.200000000Z second\n");
        gate.remember(&first);
        gate.remember(&second);

        assert!(!gate.accepts(&first));
        assert!(!gate.accepts(&second));
    }

    #[test]
    fn replay_gate_accepts_newer_entries() {
        let mut gate = LogReplayGate::default();
        let first = LogEntry::parse("stdout", "2026-03-22T10:00:00.200000000Z second\n");
        let newer = LogEntry::parse("stdout", "2026-03-22T10:00:01.000000000Z third\n");
        gate.remember(&first);

        assert!(gate.accepts(&newer));
    }
}
