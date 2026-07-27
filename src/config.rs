use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::agent::claude::ClaudeRunner;
use crate::agent::lmstudio::LmstudioRunner;
use crate::agent::noop::NoopRunner;
use crate::agent::AgentRunner;
use crate::task_source::caldav::CaldavTaskSource;
use crate::task_source::dummy::DummyTaskSource;
use crate::task_source::github::GithubTaskSource;
use crate::task_source::jira::JiraTaskSource;
use crate::task_source::TaskSource;

/// Top-level config: a named set of targets. Each target is an independent
/// combination of task source, agent and project folder; a single run of
/// the application picks exactly one target to work against (or, in loop
/// execution mode, cycles through all of them).
#[derive(Debug, Deserialize)]
pub struct Config {
    pub targets: BTreeMap<String, TargetConfig>,
    /// How the application runs: once and exit, or forever in a loop.
    /// Defaults to running once.
    #[serde(default)]
    pub execution: ExecutionConfig,
}

/// Whether to run a single task and exit, or keep running forever, picking
/// a task for the next configured target each iteration with a delay in
/// between.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ExecutionConfig {
    #[default]
    Once,
    Loop {
        /// Delay between loop iterations, in seconds. Defaults to 300 (5
        /// minutes).
        #[serde(default = "default_loop_delay_secs")]
        delay_secs: u64,
    },
}

fn default_loop_delay_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct TargetConfig {
    pub source: SourceConfig,
    pub project: ProjectConfig,
}

/// Which task supplier to use, and its supplier-specific configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    Caldav(CaldavConfig),
    Github(GithubConfig),
    Jira(JiraConfig),
    Dummy,
}

impl SourceConfig {
    pub fn build(self) -> Box<dyn TaskSource> {
        match self {
            SourceConfig::Caldav(cfg) => Box::new(CaldavTaskSource::new(cfg)),
            SourceConfig::Github(cfg) => Box::new(GithubTaskSource::new(cfg)),
            SourceConfig::Jira(cfg) => Box::new(JiraTaskSource::new(cfg)),
            SourceConfig::Dummy => Box::new(DummyTaskSource),
        }
    }
}

/// Which agent runner to use to carry out a task's prompt against the
/// project checkout.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RunnerConfig {
    #[default]
    Claude,
    Noop,
    Lmstudio(LmstudioConfig),
}

impl RunnerConfig {
    pub fn build(&self) -> Box<dyn AgentRunner> {
        match self {
            RunnerConfig::Claude => Box::new(ClaudeRunner),
            RunnerConfig::Noop => Box::new(NoopRunner),
            RunnerConfig::Lmstudio(cfg) => Box::new(LmstudioRunner::new(cfg.clone())),
        }
    }
}

/// Configuration for the LM Studio agent runner, which drives a local LM
/// Studio server's OpenAI-compatible API through a tool-calling loop.
#[derive(Debug, Clone, Deserialize)]
pub struct LmstudioConfig {
    /// Base URL of the local LM Studio server's OpenAI-compatible API.
    #[serde(default = "default_lmstudio_base_url")]
    pub base_url: String,
    /// Which model to request from LM Studio. If omitted, LM Studio uses
    /// whichever model is currently loaded.
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum number of tool-calling round trips before giving up.
    #[serde(default = "default_lmstudio_max_iterations")]
    pub max_iterations: u32,
    /// How long to wait for each LM Studio chat-completion request before
    /// giving up, in seconds. Local models can take a long time to finish
    /// thinking, so this defaults high.
    #[serde(default = "default_lmstudio_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_lmstudio_base_url() -> String {
    "http://localhost:1234/v1".to_string()
}

fn default_lmstudio_max_iterations() -> u32 {
    20
}

fn default_lmstudio_timeout_secs() -> u64 {
    1800
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaldavConfig {
    /// Full URL of the CalDAV task-list (VTODO) collection.
    pub url: String,
    pub username: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubConfig {
    /// Repository owner (user or org).
    pub owner: String,
    pub repo: String,
    /// GitHub personal access token, used as a bearer token.
    pub token: String,
    /// Label names, highest priority first. An open issue's priority is the
    /// position of the highest-ranked one of these labels it carries;
    /// issues with none of these labels sort last.
    #[serde(default = "default_priority_labels")]
    pub priority_labels: Vec<String>,
}

fn default_priority_labels() -> Vec<String> {
    vec![
        "priority:critical".to_string(),
        "priority:high".to_string(),
        "priority:medium".to_string(),
        "priority:low".to_string(),
    ]
}

#[derive(Debug, Clone, Deserialize)]
pub struct JiraConfig {
    /// Base url of the Jira site, e.g. `https://yourorg.atlassian.net`.
    pub base_url: String,
    /// Id of the board to pull issues from.
    pub board_id: u64,
    /// Email address of the Jira account to authenticate as.
    pub email: String,
    /// Jira API token, used with `email` for basic auth.
    pub api_token: String,
    /// Status name that marks an issue as ready to work on. Defaults to
    /// "To Do".
    #[serde(default = "default_jira_todo_status")]
    pub todo_status: String,
    /// Status an issue is transitioned to when a task is completed, for
    /// human review. Defaults to "In Review".
    #[serde(default = "default_jira_review_status")]
    pub review_status: String,
    /// Priority names, highest priority first. An issue's priority is its
    /// position in this list; issues with an unrecognized or absent
    /// priority sort last.
    #[serde(default = "default_jira_priority_order")]
    pub priority_order: Vec<String>,
}

fn default_jira_todo_status() -> String {
    "To Do".to_string()
}

fn default_jira_review_status() -> String {
    "In Review".to_string()
}

fn default_jira_priority_order() -> Vec<String> {
    vec![
        "Highest".to_string(),
        "High".to_string(),
        "Medium".to_string(),
        "Low".to_string(),
        "Lowest".to_string(),
    ]
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    /// Local folder containing the project Claude should work in.
    pub path: PathBuf,
    /// Whether to commit (and push) Claude's changes to git after it
    /// finishes a task. Defaults to `true`.
    #[serde(default = "default_true")]
    pub commit_changes: bool,
    /// Whether to perform any git operations (default-branch sync, task
    /// branches, commits/pushes) against this target's project folder at
    /// all. Defaults to `true`; set to `false` for a project folder that
    /// isn't a git repository. When `true` (the default) and the project
    /// folder turns out not to be a git repository, the target fails
    /// cleanly rather than running git commands against it.
    #[serde(default = "default_true")]
    pub git_enabled: bool,
    /// Which agent runner carries out the task. Defaults to `claude`.
    #[serde(default)]
    pub agent: RunnerConfig,
    /// If set, automatically open a GitHub pull request for the task branch
    /// after it's pushed. The project's `origin` remote must be a
    /// `github.com` repository. Ignored if `git_enabled` or
    /// `commit_changes` is `false`, or if the agent made no changes.
    #[serde(default)]
    pub pull_request: Option<PullRequestConfig>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestConfig {
    /// GitHub personal access token used to open the pull request. Needs
    /// the `repo` scope (or, for a fine-grained token, "Pull requests"
    /// write access) on the target repository.
    pub token: String,
    /// Base branch to open the pull request against. Defaults to the
    /// repository's default branch (the same one task branches are created
    /// from).
    #[serde(default)]
    pub base_branch: Option<String>,
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

    /// All configured target names, in a stable order, for cycling through
    /// them in loop execution mode.
    pub fn target_names(&self) -> Vec<String> {
        self.targets.keys().cloned().collect()
    }

    /// Look up a target's config by name without consuming the `Config`, so
    /// it can be re-fetched on every iteration of loop execution mode.
    pub fn target(&self, name: &str) -> Option<TargetConfig> {
        self.targets.get(name).cloned()
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
            _ => panic!("expected caldav source"),
        }
    }

    #[test]
    fn parses_github_source() {
        let yaml = r#"
targets:
  main:
    source:
      type: github
      owner: "myorg"
      repo: "myrepo"
      token: "mytoken"
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].source {
            SourceConfig::Github(github) => {
                assert_eq!(github.owner, "myorg");
                assert_eq!(github.repo, "myrepo");
                assert_eq!(github.token, "mytoken");
                assert_eq!(
                    github.priority_labels,
                    vec!["priority:critical", "priority:high", "priority:medium", "priority:low"]
                );
            }
            _ => panic!("expected github source"),
        }
    }

    #[test]
    fn parses_github_source_with_custom_priority_labels() {
        let yaml = r#"
targets:
  main:
    source:
      type: github
      owner: "myorg"
      repo: "myrepo"
      token: "mytoken"
      priority_labels: ["P0", "P1", "P2"]
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].source {
            SourceConfig::Github(github) => {
                assert_eq!(github.priority_labels, vec!["P0", "P1", "P2"]);
            }
            _ => panic!("expected github source"),
        }
    }

    #[test]
    fn parses_jira_source_with_defaults() {
        let yaml = r#"
targets:
  main:
    source:
      type: jira
      base_url: "https://myorg.atlassian.net"
      board_id: 7
      email: "me@example.com"
      api_token: "mytoken"
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].source {
            SourceConfig::Jira(jira) => {
                assert_eq!(jira.base_url, "https://myorg.atlassian.net");
                assert_eq!(jira.board_id, 7);
                assert_eq!(jira.email, "me@example.com");
                assert_eq!(jira.api_token, "mytoken");
                assert_eq!(jira.todo_status, "To Do");
                assert_eq!(jira.review_status, "In Review");
                assert_eq!(
                    jira.priority_order,
                    vec!["Highest", "High", "Medium", "Low", "Lowest"]
                );
            }
            _ => panic!("expected jira source"),
        }
    }

    #[test]
    fn parses_jira_source_with_custom_statuses() {
        let yaml = r#"
targets:
  main:
    source:
      type: jira
      base_url: "https://myorg.atlassian.net"
      board_id: 7
      email: "me@example.com"
      api_token: "mytoken"
      todo_status: "Backlog"
      review_status: "Needs QA"
      priority_order: ["P0", "P1", "P2"]
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].source {
            SourceConfig::Jira(jira) => {
                assert_eq!(jira.todo_status, "Backlog");
                assert_eq!(jira.review_status, "Needs QA");
                assert_eq!(jira.priority_order, vec!["P0", "P1", "P2"]);
            }
            _ => panic!("expected jira source"),
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
    fn git_enabled_defaults_to_true() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.targets["main"].project.git_enabled);
    }

    #[test]
    fn git_enabled_can_be_disabled() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      git_enabled: false
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.targets["main"].project.git_enabled);
    }

    #[test]
    fn pull_request_defaults_to_none() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.targets["main"].project.pull_request.is_none());
    }

    #[test]
    fn pull_request_can_be_configured() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      pull_request:
        token: "ghp_mytoken"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        let pr = cfg.targets["main"].project.pull_request.as_ref().unwrap();
        assert_eq!(pr.token, "ghp_mytoken");
        assert_eq!(pr.base_branch, None);
    }

    #[test]
    fn pull_request_base_branch_can_be_set() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      pull_request:
        token: "ghp_mytoken"
        base_branch: "develop"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        let pr = cfg.targets["main"].project.pull_request.as_ref().unwrap();
        assert_eq!(pr.base_branch.as_deref(), Some("develop"));
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

    #[test]
    fn agent_can_be_set_to_lmstudio_with_defaults() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      agent:
        type: lmstudio
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].project.agent {
            RunnerConfig::Lmstudio(lmstudio) => {
                assert_eq!(lmstudio.base_url, "http://localhost:1234/v1");
                assert_eq!(lmstudio.model, None);
                assert_eq!(lmstudio.max_iterations, 20);
                assert_eq!(lmstudio.timeout_secs, 1800);
            }
            _ => panic!("expected lmstudio agent"),
        }
    }

    #[test]
    fn agent_can_be_set_to_lmstudio_with_custom_settings() {
        let yaml = r#"
targets:
  main:
    source:
      type: dummy
    project:
      path: "/tmp/project"
      agent:
        type: lmstudio
        base_url: "http://192.168.1.50:1234/v1"
        model: "qwen2.5-coder-32b"
        max_iterations: 5
        timeout_secs: 60
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        match &cfg.targets["main"].project.agent {
            RunnerConfig::Lmstudio(lmstudio) => {
                assert_eq!(lmstudio.base_url, "http://192.168.1.50:1234/v1");
                assert_eq!(lmstudio.model.as_deref(), Some("qwen2.5-coder-32b"));
                assert_eq!(lmstudio.max_iterations, 5);
                assert_eq!(lmstudio.timeout_secs, 60);
            }
            _ => panic!("expected lmstudio agent"),
        }
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
    fn execution_defaults_to_once() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        assert!(matches!(cfg.execution, ExecutionConfig::Once));
    }

    #[test]
    fn execution_can_be_set_to_loop_with_default_delay() {
        let yaml = format!(
            "{}\nexecution:\n  mode: loop\n",
            multi_target_yaml()
        );
        let cfg: Config = serde_yaml::from_str(&yaml).unwrap();
        match cfg.execution {
            ExecutionConfig::Loop { delay_secs } => assert_eq!(delay_secs, 300),
            _ => panic!("expected loop execution mode"),
        }
    }

    #[test]
    fn execution_loop_delay_can_be_customized() {
        let yaml = format!(
            "{}\nexecution:\n  mode: loop\n  delay_secs: 60\n",
            multi_target_yaml()
        );
        let cfg: Config = serde_yaml::from_str(&yaml).unwrap();
        match cfg.execution {
            ExecutionConfig::Loop { delay_secs } => assert_eq!(delay_secs, 60),
            _ => panic!("expected loop execution mode"),
        }
    }

    #[test]
    fn target_names_returns_all_configured_targets() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        assert_eq!(cfg.target_names(), vec!["project1", "project2"]);
    }

    #[test]
    fn target_looks_up_by_name_without_consuming_config() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        let target = cfg.target("project2").unwrap();
        assert_eq!(target.project.path, PathBuf::from("/tmp/project2"));
        // `cfg` is still usable afterwards, unlike `into_target`.
        assert!(cfg.target("project1").is_some());
    }

    #[test]
    fn target_returns_none_for_unknown_name() {
        let cfg: Config = serde_yaml::from_str(multi_target_yaml()).unwrap();
        assert!(cfg.target("nope").is_none());
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
