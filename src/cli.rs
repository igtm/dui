use std::path::PathBuf;

use clap::Parser;

#[derive(Clone, Debug, Parser)]
#[command(
    name = "dui",
    version,
    about = "A container-first Docker TUI for local development"
)]
pub struct Cli {
    /// Override the config file path.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Override the Docker endpoint, for example unix:///var/run/docker.sock.
    #[arg(long)]
    pub host: Option<String>,

    /// Start with stopped containers visible.
    #[arg(long)]
    pub all: bool,

    /// Apply an initial Compose project filter.
    #[arg(long)]
    pub project: Option<String>,

    /// Focus a specific container by name prefix or full id on startup.
    #[arg(long)]
    pub container: Option<String>,

    /// Override the configured theme.
    #[arg(long)]
    pub theme: Option<String>,
}
