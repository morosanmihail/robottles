use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub caldav: CaldavConfig,
    pub project: ProjectConfig,
}

#[derive(Debug, Deserialize)]
pub struct CaldavConfig {
    /// Full URL of the CalDAV task-list (VTODO) collection.
    pub url: String,
    pub username: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    /// Local folder containing the project Claude should work in.
    pub path: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let cfg: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        Ok(cfg)
    }
}
