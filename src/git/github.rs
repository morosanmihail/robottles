//! GitHub API helpers for opening a pull request once a task branch has been
//! pushed. Deliberately independent of `task_source::github` (which talks to
//! the *issues* API): a task can come from CalDAV or Jira just as easily as
//! GitHub, but the project it's worked on may still live in a GitHub repo.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

/// A GitHub repository, identified by owner and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

/// Get the `origin` remote's URL for `project_dir`.
pub fn origin_url(project_dir: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(project_dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .context("running `git remote get-url origin`")?;
    if !output.status.success() {
        bail!(
            "`git remote get-url origin` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse `owner/repo` out of a GitHub remote URL. Accepts the HTTPS
/// (`https://github.com/owner/repo`), SSH (`git@github.com:owner/repo`) and
/// `ssh://` forms, with or without a trailing `.git`. Returns `None` for any
/// URL that isn't a `github.com` remote.
pub fn parse_github_remote(url: &str) -> Option<GithubRepo> {
    let trimmed = url.trim().trim_end_matches('/');
    let after_host = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))?;
    let after_host = after_host.strip_suffix(".git").unwrap_or(after_host);

    let (owner, name) = after_host.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubErrorResponse {
    errors: Option<Vec<GithubErrorItem>>,
}

#[derive(Debug, Deserialize)]
struct GithubErrorItem {
    message: Option<String>,
}

/// True if a GitHub API error body describes the "a pull request already
/// exists for this branch" 422, i.e. the branch already has an open PR.
fn already_exists_error(body: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<GithubErrorResponse>(body) else {
        return false;
    };
    parsed.errors.unwrap_or_default().iter().any(|e| {
        e.message
            .as_deref()
            .is_some_and(|m| m.contains("A pull request already exists"))
    })
}

#[derive(Debug, Deserialize)]
struct ExistingPullRequest {
    html_url: String,
}

/// Look up the URL of an existing open PR from `branch` into `base`, if one
/// exists.
fn find_open_pull_request(
    repo: &GithubRepo,
    token: &str,
    branch: &str,
    base: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls?head={}:{}&base={}&state=open",
        repo.owner, repo.name, repo.owner, branch, base
    );
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "robottles")
        .send()
        .with_context(|| format!("looking up existing pull requests at {url}"))?;

    let status = response.status();
    let body = response
        .text()
        .context("reading GitHub pull request lookup response body")?;
    if !status.is_success() {
        bail!("GitHub API returned {status} for {url}: {body}");
    }

    let existing: Vec<ExistingPullRequest> =
        serde_json::from_str(&body).context("parsing GitHub pull request lookup response")?;
    Ok(existing.into_iter().next().map(|pr| pr.html_url))
}

/// Open a pull request from `branch` into `base` on `repo`, titled `title`
/// with description `body`. Returns the PR's web URL. If `branch` already
/// has an open PR into `base` (e.g. because a previous run already opened
/// one for this task), returns that PR's URL instead of failing.
pub fn open_pull_request(
    repo: &GithubRepo,
    token: &str,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls",
        repo.owner, repo.name
    );
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "robottles")
        .json(&serde_json::json!({
            "title": title,
            "head": branch,
            "base": base,
            "body": body,
        }))
        .send()
        .with_context(|| format!("creating pull request at {url}"))?;

    let status = response.status();
    let text = response
        .text()
        .context("reading GitHub pull request response body")?;

    if status.is_success() {
        let pr: PullRequestResponse =
            serde_json::from_str(&text).context("parsing GitHub pull request response")?;
        return Ok(pr.html_url);
    }

    if already_exists_error(&text)
        && let Some(existing) = find_open_pull_request(repo, token, branch, base)?
    {
        return Ok(existing);
    }

    bail!("GitHub API returned {status} for {url}: {text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_remote() {
        let repo = parse_github_remote("https://github.com/owner/repo.git").unwrap();
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn parses_https_remote_without_dot_git_suffix() {
        let repo = parse_github_remote("https://github.com/owner/repo").unwrap();
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn parses_https_remote_with_trailing_slash() {
        let repo = parse_github_remote("https://github.com/owner/repo/").unwrap();
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn parses_scp_style_ssh_remote() {
        let repo = parse_github_remote("git@github.com:owner/repo.git").unwrap();
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn parses_ssh_url_remote() {
        let repo = parse_github_remote("ssh://git@github.com/owner/repo.git").unwrap();
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "repo");
    }

    #[test]
    fn rejects_non_github_remote() {
        assert!(parse_github_remote("https://gitlab.com/owner/repo.git").is_none());
    }

    #[test]
    fn rejects_url_missing_repo_name() {
        assert!(parse_github_remote("https://github.com/owner").is_none());
    }

    #[test]
    fn already_exists_error_detects_github_422_body() {
        let body = r#"{
            "message": "Validation Failed",
            "errors": [
                {"resource": "PullRequest", "code": "custom", "message": "A pull request already exists for owner:my-branch."}
            ]
        }"#;
        assert!(already_exists_error(body));
    }

    #[test]
    fn already_exists_error_ignores_unrelated_errors() {
        let body = r#"{
            "message": "Validation Failed",
            "errors": [
                {"resource": "PullRequest", "code": "custom", "message": "No commits between main and my-branch."}
            ]
        }"#;
        assert!(!already_exists_error(body));
    }

    #[test]
    fn already_exists_error_ignores_unparseable_body() {
        assert!(!already_exists_error("not json"));
    }
}
