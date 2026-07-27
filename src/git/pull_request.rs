//! Opening a pull request for a completed task's branch. Builds on the raw
//! GitHub API bindings in [`crate::git::github`] with the task-specific title
//! and body formatting, and the "which remote/repo" plumbing.

use std::path::Path;

use anyhow::Context;

use crate::config::PullRequestConfig;
use crate::git::github::{open_pull_request, origin_url, parse_github_remote};
use crate::task_source::ical::Task;

/// Open a GitHub pull request for a just-pushed task branch. The project's
/// `origin` remote must point at a `github.com` repository; anything else
/// (a different git host, or no `origin` at all) is reported as an error
/// for the caller to log rather than treat as fatal.
pub fn open_task_pull_request(
    project_dir: &Path,
    branch: &str,
    base: &str,
    task: &Task,
    pr_config: &PullRequestConfig,
) -> anyhow::Result<String> {
    let remote = origin_url(project_dir).context("reading origin remote url")?;
    let repo = parse_github_remote(&remote)
        .with_context(|| format!("origin remote '{remote}' is not a github.com repository"))?;
    open_pull_request(
        &repo,
        &pr_config.token,
        branch,
        base,
        &pr_title(task),
        &pr_body(task),
    )
    .with_context(|| format!("opening pull request for {}/{}", repo.owner, repo.name))
}

/// Title for the pull request opened for a completed task.
pub fn pr_title(task: &Task) -> String {
    format!("Complete task: {}", task.summary)
}

/// Body for the pull request opened for a completed task: a note that it's
/// automated, followed by the task description (if any). Deliberately omits
/// the task's id/href — those are internal to the task source, not useful
/// context for a PR reviewer.
pub fn pr_body(task: &Task) -> String {
    let mut body = format!("Automated pull request for task `{}`.", task.summary);
    if let Some(description) = &task.description
        && !description.trim().is_empty()
    {
        body.push_str(&format!("\n\n---\n\n{description}"));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(uid: &str, summary: &str, description: Option<&str>) -> Task {
        Task {
            uid: uid.to_string(),
            summary: summary.to_string(),
            status: None,
            priority: None,
            due: None,
            description: description.map(str::to_string),
            href: String::new(),
        }
    }

    /// Run a git command, panicking on failure. Only for test setup, where a
    /// failure means the test fixture itself is broken.
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
        assert!(status.success(), "`git {}` failed", args.join(" "));
    }

    /// Set up a work tree with an `origin` remote (a local bare repo) and one
    /// commit on `main`.
    fn init_repo_with_origin() -> (tempfile::TempDir, tempfile::TempDir) {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "--bare", "-q", "-b", "main"]);

        let work = tempfile::tempdir().unwrap();
        git(work.path(), &["init", "-q"]);
        git(work.path(), &["config", "user.email", "test@example.com"]);
        git(work.path(), &["config", "user.name", "Test User"]);
        std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
        git(work.path(), &["add", "-A"]);
        git(work.path(), &["commit", "-q", "-m", "initial commit"]);
        git(work.path(), &["branch", "-M", "main"]);
        git(
            work.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(work.path(), &["push", "-q", "-u", "origin", "main"]);

        (origin, work)
    }

    #[test]
    fn pr_title_uses_task_summary() {
        let task = task_with("42", "Fix the login bug", None);
        assert_eq!(pr_title(&task), "Complete task: Fix the login bug");
    }

    #[test]
    fn pr_body_includes_summary_and_description() {
        let mut task = task_with("42", "Fix the login bug", Some("Details here"));
        task.href = "https://github.com/o/r/issues/42".to_string();
        let body = pr_body(&task);
        assert!(body.contains("task `Fix the login bug`"));
        assert!(body.contains("Details here"));
    }

    #[test]
    fn pr_body_omits_uid_and_source() {
        let mut task = task_with("42", "Fix the login bug", None);
        task.href = "https://github.com/o/r/issues/42".to_string();
        let body = pr_body(&task);
        assert!(!body.contains("42"));
        assert!(!body.contains("Source:"));
        assert!(!body.contains("https://github.com/o/r/issues/42"));
    }

    #[test]
    fn open_task_pull_request_errors_cleanly_on_non_github_remote() {
        let (_origin, work) = init_repo_with_origin();
        let task = task_with("42", "Fix the login bug", None);
        let pr_config = PullRequestConfig {
            token: "unused".to_string(),
            base_branch: None,
        };
        let err = open_task_pull_request(work.path(), "task_branch", "main", &task, &pr_config)
            .unwrap_err();
        assert!(err.to_string().contains("not a github.com repository"));
    }
}
