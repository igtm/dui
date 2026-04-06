use std::collections::BTreeMap;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Map, Value};

use crate::ansi::strip_ansi;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerRecord {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub image: String,
    pub command: String,
    pub state: String,
    pub status: String,
    pub project: Option<String>,
    pub service: Option<String>,
    pub ports: Vec<PortMapping>,
    pub health: Option<String>,
    pub created: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortMapping {
    pub ip: Option<String>,
    pub private_port: Option<u64>,
    pub public_port: Option<u64>,
    pub typ: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailItem {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerDetails {
    pub overview: Vec<DetailItem>,
    pub env: Vec<DetailItem>,
    pub ports: Vec<DetailItem>,
    pub mounts: Vec<DetailItem>,
    pub health: Vec<DetailItem>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogFilterMode {
    #[default]
    Substring,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub stream: String,
    pub timestamp: Option<String>,
    pub message: String,
    pub plain_message: String,
}

impl ContainerRecord {
    pub fn from_summary_value(value: Value) -> Result<Self> {
        let object = value
            .as_object()
            .context("container summary must be a JSON object")?;

        let id = get_string(object, "Id").unwrap_or_else(|| "unknown".into());
        let names = get_string_array(object, "Names");
        let name = names
            .into_iter()
            .find(|value| !value.trim().is_empty())
            .map(|value| value.trim_start_matches('/').to_string())
            .unwrap_or_else(|| short_id(&id));

        let labels = get_object(object, "Labels");
        let project = labels
            .and_then(|labels| get_string(labels, "com.docker.compose.project"))
            .filter(|value| !value.is_empty());
        let service = labels
            .and_then(|labels| get_string(labels, "com.docker.compose.service"))
            .filter(|value| !value.is_empty());
        let health = labels
            .and_then(|labels| get_string(labels, "com.docker.compose.container-number"))
            .filter(|_| false);

        Ok(Self {
            short_id: short_id(&id),
            id,
            name,
            image: get_string(object, "Image").unwrap_or_else(|| "<unknown>".into()),
            command: get_string(object, "Command").unwrap_or_default(),
            state: get_string(object, "State").unwrap_or_else(|| "unknown".into()),
            status: get_string(object, "Status").unwrap_or_else(|| "unknown".into()),
            project,
            service,
            ports: get_ports(object, "Ports"),
            health,
            created: get_i64(object, "Created"),
        })
    }

    pub fn matches_query(&self, query: &str) -> bool {
        if query.trim().is_empty() {
            return true;
        }

        let query = query.to_ascii_lowercase();
        [
            self.id.as_str(),
            self.short_id.as_str(),
            self.name.as_str(),
            self.image.as_str(),
            self.command.as_str(),
            self.project.as_deref().unwrap_or_default(),
            self.service.as_deref().unwrap_or_default(),
        ]
        .into_iter()
        .any(|candidate| candidate.to_ascii_lowercase().contains(&query))
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.state.as_str(),
            "running" | "restarting" | "paused" | "created"
        )
    }

    pub fn ports_summary(&self) -> String {
        if self.ports.is_empty() {
            return "-".into();
        }

        self.ports
            .iter()
            .map(PortMapping::display)
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn health_label(&self) -> &str {
        self.health.as_deref().unwrap_or("-")
    }
}

impl ContainerDetails {
    pub fn from_inspect_value(summary: &ContainerRecord, value: Value) -> Self {
        let object = match value.as_object() {
            Some(object) => object,
            None => return Self::default(),
        };

        let env = get_path_array_strings(object, &["Config", "Env"])
            .into_iter()
            .map(split_env)
            .map(|(label, value)| DetailItem { label, value })
            .collect::<Vec<_>>();

        let ports = extract_inspect_ports(object);
        let mounts = extract_mounts(object);
        let health = extract_health(object);
        let mut overview = vec![
            DetailItem {
                label: "Name".into(),
                value: summary.name.clone(),
            },
            DetailItem {
                label: "ID".into(),
                value: summary.id.clone(),
            },
            DetailItem {
                label: "Image".into(),
                value: summary.image.clone(),
            },
            DetailItem {
                label: "State".into(),
                value: summary.state.clone(),
            },
            DetailItem {
                label: "Status".into(),
                value: summary.status.clone(),
            },
        ];

        if let Some(project) = &summary.project {
            overview.push(DetailItem {
                label: "Project".into(),
                value: project.clone(),
            });
        }
        if let Some(service) = &summary.service {
            overview.push(DetailItem {
                label: "Service".into(),
                value: service.clone(),
            });
        }
        if !summary.command.is_empty() {
            overview.push(DetailItem {
                label: "Command".into(),
                value: summary.command.clone(),
            });
        }
        if let Some(started_at) = get_path_string(object, &["State", "StartedAt"]) {
            overview.push(DetailItem {
                label: "Started".into(),
                value: started_at,
            });
        }
        if let Some(created) = get_string(object, "Created") {
            overview.push(DetailItem {
                label: "Created".into(),
                value: created,
            });
        }

        Self {
            overview,
            env,
            ports,
            mounts,
            health,
        }
    }

    pub fn items_for_tab(&self, tab: crate::app::DetailTab) -> &[DetailItem] {
        match tab {
            crate::app::DetailTab::Overview => &self.overview,
            crate::app::DetailTab::Env => &self.env,
            crate::app::DetailTab::Ports => &self.ports,
            crate::app::DetailTab::Mounts => &self.mounts,
            crate::app::DetailTab::Health => &self.health,
            crate::app::DetailTab::Logs => &self.overview,
        }
    }
}

impl PortMapping {
    pub fn display(&self) -> String {
        match (self.public_port, self.private_port) {
            (Some(public), Some(private)) => {
                let ip = self
                    .ip
                    .as_deref()
                    .filter(|ip| !ip.is_empty())
                    .unwrap_or("0.0.0.0");
                let protocol = self.typ.as_deref().unwrap_or("tcp");
                format!("{ip}:{public}->{private}/{protocol}")
            }
            (None, Some(private)) => {
                let protocol = self.typ.as_deref().unwrap_or("tcp");
                format!("{private}/{protocol}")
            }
            _ => "-".into(),
        }
    }
}

impl LogEntry {
    pub fn parse(stream: &str, raw: &str) -> Self {
        let trimmed = raw.trim_end_matches('\n').to_string();
        let timestamp_regex =
            Regex::new(r"^(?P<ts>\d{4}-\d{2}-\d{2}T[^\s]+)\s(?P<msg>.*)$").expect("regex compiles");

        if let Some(captures) = timestamp_regex.captures(&trimmed) {
            let message = captures
                .name("msg")
                .map(|value| value.as_str().to_string())
                .unwrap_or_default();
            return Self {
                stream: stream.into(),
                timestamp: captures.name("ts").map(|value| value.as_str().to_string()),
                plain_message: strip_ansi(&message),
                message,
            };
        }

        Self {
            stream: stream.into(),
            timestamp: None,
            plain_message: strip_ansi(&trimmed),
            message: trimmed,
        }
    }

    pub fn display(&self, show_timestamps: bool) -> String {
        if show_timestamps {
            if let Some(timestamp) = &self.timestamp {
                return format!("{timestamp} {}", self.plain_message);
            }
        }
        self.plain_message.clone()
    }

    pub fn display_raw(&self, show_timestamps: bool) -> String {
        if show_timestamps {
            if let Some(timestamp) = &self.timestamp {
                return format!("{timestamp} {}", self.message);
            }
        }
        self.message.clone()
    }
}

pub fn sort_containers(containers: &mut [ContainerRecord]) {
    containers.sort_by(|left, right| {
        right
            .is_running()
            .cmp(&left.is_running())
            .then_with(|| left.project.cmp(&right.project))
            .then_with(|| left.service.cmp(&right.service))
            .then_with(|| left.name.cmp(&right.name))
    });
}

pub fn apply_container_filters<'a>(
    containers: &'a [ContainerRecord],
    show_stopped: bool,
    project_filter: Option<&str>,
    query: Option<&str>,
) -> Vec<&'a ContainerRecord> {
    containers
        .iter()
        .filter(|container| show_stopped || container.is_running())
        .filter(|container| {
            project_filter
                .map(|filter| container.project.as_deref() == Some(filter))
                .unwrap_or(true)
        })
        .filter(|container| {
            query
                .map(|value| container.matches_query(value))
                .unwrap_or(true)
        })
        .collect()
}

fn extract_inspect_ports(object: &Map<String, Value>) -> Vec<DetailItem> {
    let mut rows = Vec::new();

    if let Some(network_settings) = get_path_object(object, &["NetworkSettings", "Ports"]) {
        let mut ordered = BTreeMap::new();
        for (container_port, bindings) in network_settings {
            let value = match bindings {
                Value::Array(bindings) if !bindings.is_empty() => bindings
                    .iter()
                    .filter_map(|binding| binding.as_object())
                    .map(|binding| {
                        let host_ip =
                            get_string(binding, "HostIp").unwrap_or_else(|| "0.0.0.0".into());
                        let host_port =
                            get_string(binding, "HostPort").unwrap_or_else(|| "?".into());
                        format!("{host_ip}:{host_port}")
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => "not published".into(),
            };
            ordered.insert(container_port.clone(), value);
        }

        rows.extend(
            ordered
                .into_iter()
                .map(|(label, value)| DetailItem { label, value }),
        );
    }

    if rows.is_empty() {
        rows.push(DetailItem {
            label: "Ports".into(),
            value: "No published ports".into(),
        });
    }

    rows
}

fn extract_mounts(object: &Map<String, Value>) -> Vec<DetailItem> {
    let mut mounts = get_path_array_objects(object, &["Mounts"])
        .into_iter()
        .map(|mount| {
            let destination = get_string(&mount, "Destination").unwrap_or_else(|| "?".into());
            let source = get_string(&mount, "Source").unwrap_or_else(|| "<anonymous>".into());
            let typ = get_string(&mount, "Type").unwrap_or_else(|| "mount".into());
            let mode = match get_bool(&mount, "RW") {
                Some(true) => "rw",
                Some(false) => "ro",
                None => "?",
            };

            DetailItem {
                label: destination,
                value: format!("{typ} {source} ({mode})"),
            }
        })
        .collect::<Vec<_>>();

    if mounts.is_empty() {
        mounts.push(DetailItem {
            label: "Mounts".into(),
            value: "No mounts".into(),
        });
    }

    mounts
}

fn extract_health(object: &Map<String, Value>) -> Vec<DetailItem> {
    let mut rows = Vec::new();
    if let Some(status) = get_path_string(object, &["State", "Health", "Status"]) {
        rows.push(DetailItem {
            label: "Status".into(),
            value: status,
        });
    }
    if let Some(streak) = get_path_i64(object, &["State", "Health", "FailingStreak"]) {
        rows.push(DetailItem {
            label: "Failing streak".into(),
            value: streak.to_string(),
        });
    }
    if let Some(logs) = get_path_array_objects(object, &["State", "Health", "Log"]).split_last() {
        for entry in logs
            .1
            .iter()
            .rev()
            .take(2)
            .rev()
            .chain(std::iter::once(logs.0))
        {
            let label = get_string(entry, "ExitCode").unwrap_or_else(|| "exit".into());
            let output = get_string(entry, "Output")
                .unwrap_or_default()
                .trim()
                .to_string();
            rows.push(DetailItem {
                label: format!("Log {label}"),
                value: output,
            });
        }
    }
    if rows.is_empty() {
        rows.push(DetailItem {
            label: "Health".into(),
            value: "No health check".into(),
        });
    }
    rows
}

fn split_env(raw: String) -> (String, String) {
    match raw.split_once('=') {
        Some((label, value)) => (label.to_string(), value.to_string()),
        None => (raw, String::new()),
    }
}

fn get_ports(object: &Map<String, Value>, key: &str) -> Vec<PortMapping> {
    object
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|port| PortMapping {
            ip: get_string(port, "IP"),
            private_port: get_u64(port, "PrivatePort"),
            public_port: get_u64(port, "PublicPort"),
            typ: get_string(port, "Type"),
        })
        .collect()
}

fn get_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn get_bool(map: &Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(Value::as_bool)
}

fn get_i64(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key).and_then(Value::as_i64)
}

fn get_u64(map: &Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key).and_then(Value::as_u64)
}

fn get_string_array(map: &Map<String, Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn get_object<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key).and_then(Value::as_object)
}

fn get_path_object<'a>(
    map: &'a Map<String, Value>,
    path: &[&str],
) -> Option<&'a Map<String, Value>> {
    get_path_value(map, path).and_then(Value::as_object)
}

fn get_path_array_strings(map: &Map<String, Value>, path: &[&str]) -> Vec<String> {
    get_path_value(map, path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn get_path_array_objects(map: &Map<String, Value>, path: &[&str]) -> Vec<Map<String, Value>> {
    get_path_value(map, path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_object().cloned())
        .collect()
}

fn get_path_string(map: &Map<String, Value>, path: &[&str]) -> Option<String> {
    get_path_value(map, path)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn get_path_i64(map: &Map<String, Value>, path: &[&str]) -> Option<i64> {
    get_path_value(map, path).and_then(Value::as_i64)
}

fn get_path_value<'a>(map: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut current = map.get(*first)?;
    for segment in rest {
        current = current.as_object()?.get(*segment)?;
    }
    Some(current)
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_summary_json() {
        let value = json!({
            "Id": "0123456789abcdef",
            "Names": ["/api-1"],
            "Image": "ghcr.io/example/api:latest",
            "Command": "cargo run",
            "Created": 1710000000,
            "State": "running",
            "Status": "Up 10 minutes",
            "Labels": {
                "com.docker.compose.project": "sample",
                "com.docker.compose.service": "api"
            },
            "Ports": [
                {
                    "IP": "0.0.0.0",
                    "PrivatePort": 3000,
                    "PublicPort": 3000,
                    "Type": "tcp"
                }
            ]
        });

        let container = ContainerRecord::from_summary_value(value).expect("summary parses");
        assert_eq!(container.name, "api-1");
        assert_eq!(container.project.as_deref(), Some("sample"));
        assert_eq!(container.service.as_deref(), Some("api"));
        assert_eq!(container.ports_summary(), "0.0.0.0:3000->3000/tcp");
        assert!(container.matches_query("ghcr"));
    }

    #[test]
    fn sorts_running_before_stopped() {
        let mut containers = vec![
            ContainerRecord {
                id: "b".into(),
                short_id: "b".into(),
                name: "stopped".into(),
                image: "img".into(),
                command: String::new(),
                state: "exited".into(),
                status: "Exited".into(),
                project: Some("demo".into()),
                service: Some("worker".into()),
                ports: Vec::new(),
                health: None,
                created: None,
            },
            ContainerRecord {
                id: "a".into(),
                short_id: "a".into(),
                name: "running".into(),
                image: "img".into(),
                command: String::new(),
                state: "running".into(),
                status: "Up".into(),
                project: Some("demo".into()),
                service: Some("api".into()),
                ports: Vec::new(),
                health: None,
                created: None,
            },
        ];

        sort_containers(&mut containers);
        assert_eq!(containers[0].name, "running");
    }

    #[test]
    fn extracts_inspect_details() {
        let summary = ContainerRecord {
            id: "abc".into(),
            short_id: "abc".into(),
            name: "api-1".into(),
            image: "ghcr.io/example/api:latest".into(),
            command: "cargo run".into(),
            state: "running".into(),
            status: "Up 1 minute".into(),
            project: Some("demo".into()),
            service: Some("api".into()),
            ports: Vec::new(),
            health: None,
            created: None,
        };

        let inspect = json!({
            "Created": "2026-03-22T01:02:03Z",
            "Config": {
                "Env": ["RUST_LOG=info", "PORT=3000"]
            },
            "Mounts": [
                {
                    "Type": "bind",
                    "Source": "/tmp/src",
                    "Destination": "/app",
                    "RW": true
                }
            ],
            "State": {
                "StartedAt": "2026-03-22T01:03:04Z",
                "Health": {
                    "Status": "healthy",
                    "FailingStreak": 0,
                    "Log": [
                        {"ExitCode": "0", "Output": "ok"}
                    ]
                }
            },
            "NetworkSettings": {
                "Ports": {
                    "3000/tcp": [
                        {"HostIp": "0.0.0.0", "HostPort": "3000"}
                    ]
                }
            }
        });

        let details = ContainerDetails::from_inspect_value(&summary, inspect);
        assert!(details.overview.iter().any(|row| row.label == "Project"));
        assert_eq!(details.env.len(), 2);
        assert_eq!(details.mounts[0].label, "/app");
        assert_eq!(details.health[0].value, "healthy");
    }

    #[test]
    fn parses_log_timestamp_prefix() {
        let entry = LogEntry::parse(
            "stdout",
            "2026-03-22T12:00:00.123456789Z listening on http://0.0.0.0:3000\n",
        );
        assert_eq!(
            entry.timestamp.as_deref(),
            Some("2026-03-22T12:00:00.123456789Z")
        );
        assert_eq!(entry.message, "listening on http://0.0.0.0:3000");
    }
}
