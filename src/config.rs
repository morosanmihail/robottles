use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::agent::claude::ClaudeRunner;
use crate::agent::noop::NoopRunner;
use crate::agent::AgentRunner;
use crate::task_source::caldav::CaldavTaskSource;
use crate::task_source::dummy::DummyTaskSource;
use crate::task_source::TaskSource;

/// Top-level config: a named set of targets. Each target is an independent
/// combination of task source, agent and project folder; a single run of
/// the application picks exactly one target to work against.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub targets: BTreeMap<String, TargetConfig>,
}

#[derive(Debug, Deserialize)]
pub struct TargetConfig {
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

/// Which agent runner to use to carry out a task's prompt against the
/// project checkout.
#[derive(Debug, Default, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RunnerConfig {
    #[default]
    Claude,
    Noop,
}

impl RunnerConfig {
    pub fn build(&self) -> Box<dyn AgentRunner> {
        match self {
            RunnerConfig::Claude => Box::new(ClaudeRunner),
            RunnerConfig::Noop => Box::new(NoopRunner),
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
    /// Whether to commit (and push) Claude's changes to git after it
    /// finishes a task. Defaults to `true`.
    #[serde(default = "default_true")]
    pub commit_changes: bool,
    /// Which agent runner carries out the task. Defaults to `claude`.
    #[serde(default)]
    pub agent: RunnerConfig,
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let cfg: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        Ok(cfg)
    }

    /// Pick a single target to run against by name. If `name` is `None`,
    /// this only succeeds when exactly one target is configured (in which
    /// case it's picked implicitly).
    pub fn into_target(mut self, name: Option<&str>) -> anyhow::Result<TargetConfig> {
        let key = match name {
            Some(name) => name.to_string(),
            None => match self.targets.len() {
                1 => self.targets.keys().next().unwrap().clone(),
                0 => bail!("no targets configured"),
                _ => bail!(
                    "multiple targets configured ({}); specify which one to run against",
                    self.target_names_joined()
                ),
            },
        };
        self.targets.remove(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "no target named '{key}' in config; available targets: {}",
                self.target_names_joined()
            )
        })
    }

    fn target_names_joined(&self) -> String {
        self.targets.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_caldav_source() {
        let yaml = r#"
targets:
  main:
    source:
      type: caldav
      url: "https://example.com/tasks/"
      username: "myuser"
      token: "mytoken"
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].source {
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
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.targets["main"].source, SourceConfig::Dummy));
    }

    #[test]
    fn commit_changes_defaults_to_true() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.targets["main"].project.commit_changes);
    }

    #[test]
    fn commit_changes_can_be_disabled() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      commit_changes: false
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.targets["main"].project.commit_changes);
    }

    #[test]
    fn agent_defaults_to_claude() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            cfg.targets["main"].project.agent,
            RunnerConfig::Claude
        ));
    }

    #[test]
    fn agent_can_be_set_to_noop() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      agent:
        type: noop
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            cfg.targets["main"].project.agent,
            RunnerConfig::Noop
        ));
    }

    fn multi_target_yaml() -> &'static str {
        r#"
targets:
  project1:
    source:
      type: dummy
    project:
      path: "/tmp/project1"
  project2:
    source:
      type: dummy
    project:
      path: "/tmp/project2"
"#
    }

    #[test]
    fn parses_multiple_targets() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        assert_eq!(cfg.targets.len(), 2);
        assert_eq!(
            cfg.targets["project1"].project.path,
            PathBuf::from("/tmp/project1")
        );
        assert_eq!(
            cfg.targets["project2"].project.path,
            PathBuf::from("/tmp/project2")
        );
    }

    #[test]
    fn into_target_picks_named_target() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        let target = cfg.into_target(Some("project2")).unwrap();
        assert_eq!(target.project.path, PathBuf::from("/tmp/project2"));
    }

    #[test]
    fn into_target_errors_on_unknown_name() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        let err = cfg.into_target(Some("nope")).unwrap_err();
        assert!(err.to_string().contains("no target named 'nope'"));
    }

    #[test]
    fn into_target_requires_name_when_ambiguous() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        let err = cfg.into_target(None).unwrap_err();
        assert!(err.to_string().contains("multiple targets configured"));
    }

    #[test]
    fn into_target_defaults_when_only_one_target() {
        let yaml = r#"
targets:
  solo:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        let target = cfg.into_target(None).unwrap();
        assert_eq!(target.project.path, PathBuf::from("/tmp/project"));
    }
}
