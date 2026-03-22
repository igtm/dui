use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::Cli;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Graphite,
    Ember,
    Ocean,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: ThemeName,
    pub show_stopped_by_default: bool,
    pub log_backlog_lines: usize,
    pub show_timestamps: bool,
    pub keymap: KeymapConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: ThemeName::Graphite,
            show_stopped_by_default: false,
            log_backlog_lines: 400,
            show_timestamps: true,
            keymap: KeymapConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct KeymapConfig {
    pub quit: Option<String>,
    pub copy: Option<String>,
    pub toggle_stopped: Option<String>,
    pub start_stop: Option<String>,
    pub restart: Option<String>,
    pub remove: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub theme: ThemeName,
    pub show_stopped_by_default: bool,
    pub log_backlog_lines: usize,
    pub show_timestamps: bool,
    pub keymap: KeymapConfig,
    pub docker_host: Option<String>,
    pub project_filter: Option<String>,
    pub startup_container_query: Option<String>,
}

impl RuntimeConfig {
    pub fn from_sources(cli: Cli, config: AppConfig) -> Self {
        let theme = cli
            .theme
            .as_deref()
            .and_then(ThemeName::from_str)
            .unwrap_or(config.theme);

        Self {
            theme,
            show_stopped_by_default: cli.all || config.show_stopped_by_default,
            log_backlog_lines: config.log_backlog_lines.clamp(50, 10_000),
            show_timestamps: config.show_timestamps,
            keymap: config.keymap,
            docker_host: cli.host,
            project_filter: cli.project,
            startup_container_query: cli.container,
        }
    }
}

impl AppConfig {
    pub fn load(path_override: Option<PathBuf>) -> Result<(Self, Option<PathBuf>)> {
        let Some(path) = path_override.or_else(default_config_path) else {
            return Ok((Self::default(), None));
        };

        if !path.exists() {
            return Ok((Self::default(), Some(path)));
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file at {}", path.display()))?;
        let parsed = Self::parse(&raw)
            .with_context(|| format!("failed to parse config file at {}", path.display()))?;
        Ok((parsed, Some(path)))
    }

    pub fn parse(raw: &str) -> Result<Self> {
        Ok(toml::from_str(raw)?)
    }
}

impl ThemeName {
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "graphite" => Some(Self::Graphite),
            "ember" => Some(Self::Ember),
            "ocean" => Some(Self::Ocean),
            _ => None,
        }
    }
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join("dui").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_toml() {
        let raw = r#"
theme = "ember"
show_stopped_by_default = true
log_backlog_lines = 900
show_timestamps = false

[keymap]
quit = "ctrl+c"
"#;

        let config = AppConfig::parse(raw).expect("config parses");
        assert_eq!(config.theme, ThemeName::Ember);
        assert!(config.show_stopped_by_default);
        assert_eq!(config.log_backlog_lines, 900);
        assert!(!config.show_timestamps);
        assert_eq!(config.keymap.quit.as_deref(), Some("ctrl+c"));
    }

    #[test]
    fn runtime_config_respects_cli_overrides() {
        let cli = Cli {
            config: None,
            host: Some("unix:///tmp/docker.sock".into()),
            all: true,
            project: Some("demo".into()),
            container: Some("api".into()),
            theme: Some("ocean".into()),
        };

        let runtime = RuntimeConfig::from_sources(cli, AppConfig::default());
        assert_eq!(runtime.theme, ThemeName::Ocean);
        assert!(runtime.show_stopped_by_default);
        assert_eq!(runtime.project_filter.as_deref(), Some("demo"));
        assert_eq!(runtime.startup_container_query.as_deref(), Some("api"));
        assert_eq!(
            runtime.docker_host.as_deref(),
            Some("unix:///tmp/docker.sock")
        );
    }
}
