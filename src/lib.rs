mod app;
mod cli;
mod config;
mod docker;
mod model;
mod ui;

use anyhow::Result;
use clap::Parser;

use app::{App, UiCommand};
use cli::Cli;
use config::{AppConfig, RuntimeConfig};
use docker::DockerManager;
use ui::{ClipboardHandle, TerminalEvent, TerminalEvents, TerminalHandle};

pub use app::DetailTab;
pub use config::ThemeName;
pub use model::{ContainerDetails, ContainerRecord, DetailItem, LogEntry, LogFilterMode};

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let (config, config_path) = AppConfig::load(cli.config.clone())?;
    let runtime = RuntimeConfig::from_sources(cli, config);

    let mut terminal = TerminalHandle::enter()?;
    let mut events = TerminalEvents::spawn();
    let mut clipboard = ClipboardHandle::new();
    let mut docker =
        DockerManager::spawn(runtime.docker_host.clone(), runtime.log_backlog_lines).await?;
    let mut app = App::new(runtime, config_path);

    for command in app.bootstrap_commands() {
        if dispatch_command(&mut app, &mut docker, &mut clipboard, command)? {
            return Ok(());
        }
    }

    loop {
        app.set_viewport(terminal.viewport()?);
        terminal.draw(|frame| ui::render(frame, &app))?;

        tokio::select! {
            maybe_event = events.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                let commands = match event {
                    TerminalEvent::Key(key) => app.handle_key(key),
                    TerminalEvent::Mouse(mouse) => app.handle_mouse(mouse),
                    TerminalEvent::Resize => Vec::new(),
                };

                for command in commands {
                    if dispatch_command(&mut app, &mut docker, &mut clipboard, command)? {
                        return Ok(());
                    }
                }
            }
            maybe_message = docker.recv() => {
                let Some(message) = maybe_message else {
                    break;
                };
                for command in app.apply_docker_event(message) {
                    if dispatch_command(&mut app, &mut docker, &mut clipboard, command)? {
                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}

fn dispatch_command(
    app: &mut App,
    docker: &mut DockerManager,
    clipboard: &mut ClipboardHandle,
    command: UiCommand,
) -> Result<bool> {
    match command {
        UiCommand::Quit => Ok(true),
        UiCommand::Copy(text) => {
            match clipboard.copy(&text) {
                Ok(()) => app.set_status(format!("Copied {} bytes to the clipboard", text.len())),
                Err(error) => app.set_error(format!("Clipboard copy failed: {error}")),
            }
            Ok(false)
        }
        UiCommand::Docker(command) => {
            docker.send(command)?;
            Ok(false)
        }
        UiCommand::SetStatus(message) => {
            app.set_status(message);
            Ok(false)
        }
    }
}
