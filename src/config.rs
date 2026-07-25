use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::caldav::CaldavTaskSource;
use crate::dummy::DummyTaskSource;
use crate::task_source::TaskSource;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
    pub project: ProjectConfig,
}

/// Which task supplier to use, and its supplier-specific configuration.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    Caldav(CaldavConfig),
    Dummy,
}

impl SourceConfig {
    pub fn build(self) -> Box<dyn TaskSource> {
        match self {
            SourceConfig::Caldav(cfg) => Box::new(CaldavTaskSource::new(cfg)),
            SourceConfig::Dummy => Box::new(DummyTaskSource),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_caldav_source() {
        let yaml = r#"
source:
  type: caldav
  url: "https://example.com/tasks/"
  username: "myuser"
  token: "mytoken"
project:
  path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match cfg.source {
            SourceConfig::Caldav(caldav) => {
                assert_eq!(caldav.url, "https://example.com/tasks/");
                assert_eq!(caldav.username, "myuser");
                assert_eq!(caldav.token, "mytoken");
            }
            SourceConfig::Dummy => panic!("expected caldav source"),
        }
    }

    #[test]
    fn parses_dummy_source() {
        let yaml = r#"
source:
  type: dummy
project:
  path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.source, SourceConfig::Dummy));
    }
}
