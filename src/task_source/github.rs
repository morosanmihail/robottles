use anyhow::{bail, Context};
use serde::Deserialize;

use super::ical::Task;
use super::{CompletionMetadata, TaskSource};
use crate::config::GithubConfig;

/// A task source backed by open GitHub issues on a single repo. Priority is
/// derived from labels: `priority_labels` in config lists label names from
/// highest to lowest priority, and the highest-priority open issue is
/// returned first.
pub struct GithubTaskSource {
    pub config: GithubConfig,
}

impl GithubTaskSource {
    pub fn new(config: GithubConfig) -> Self {
        Self { config }
    }
}

impl TaskSource for GithubTaskSource {
    fn get_next_task(&self) -> anyhow::Result<Option<Task>> {
        let issues = fetch_open_issues(&self.config)?;
        let mut ranked: Vec<(usize, GithubIssue)> = issues
            .into_iter()
            .map(|issue| (priority_rank(&issue, &self.config.priority_labels), issue))
            .collect();
        sort_by_priority(&mut ranked);
        Ok(ranked.into_iter().next().map(|(_, issue)| issue_to_task(issue)))
    }

    fn mark_completed(&self, task: &Task, metadata: &CompletionMetadata) -> anyhow::Result<()> {
        if let Some(pr_url) = &metadata.pr_url {
            comment_on_issue(&self.config, task, &format!("Opened pull request: {pr_url}"))?;
        }
        close_issue(&self.config, task)
    }
}

/// One issue as returned by the GitHub REST API (only the fields we need).
#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    title: String,
    body: Option<String>,
    url: String,
    created_at: String,
    labels: Vec<GithubLabel>,
    /// Present (with any value) only on pull requests; used to filter PRs
    /// out of the `issues` endpoint, which returns both.
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GithubLabel {
    name: String,
}

/// Rank of `issue` among `priority_labels` (0 = highest priority). Issues
/// with none of the configured priority labels rank last, after every
/// labeled issue.
fn priority_rank(issue: &GithubIssue, priority_labels: &[String]) -> usize {
    issue
        .labels
        .iter()
        .filter_map(|label| {
            priority_labels
                .iter()
                .position(|p| p.eq_ignore_ascii_case(&label.name))
        })
        .min()
        .unwrap_or(priority_labels.len())
}

/// Order ranked issues "most important first": lower rank wins, then
/// earliest created, then lowest issue number as a final tiebreaker.
fn sort_by_priority(ranked: &mut [(usize, GithubIssue)]) {
    ranked.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.number.cmp(&b.number))
    });
}

fn issue_to_task(issue: GithubIssue) -> Task {
    Task {
        uid: issue.number.to_string(),
        summary: issue.title,
        status: Some("NEEDS-ACTION".to_string()),
        priority: None,
        due: None,
        description: issue.body,
        href: issue.url,
    }
}

fn fetch_open_issues(cfg: &GithubConfig) -> anyhow::Result<Vec<GithubIssue>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues?state=open&per_page=100",
        cfg.owner, cfg.repo
    );
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(&url)
        .bearer_auth(&cfg.token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "robottles")
        .send()
        .with_context(|| format!("fetching open issues from {url}"))?;

    let status = response.status();
    let body = response.text().context("reading GitHub issues response body")?;
    if !status.is_success() {
        bail!("GitHub API returned {status} for {url}: {body}");
    }

    let issues: Vec<GithubIssue> =
        serde_json::from_str(&body).context("parsing GitHub issues response")?;

    Ok(issues.into_iter().filter(|issue| issue.pull_request.is_none()).collect())
}

/// Post `body` as a comment on `task`'s issue (`href` is the issue's API url).
fn comment_on_issue(cfg: &GithubConfig, task: &Task, body: &str) -> anyhow::Result<()> {
    let url = format!("{}/comments", task.href);
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(&url)
        .bearer_auth(&cfg.token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "robottles")
        .json(&serde_json::json!({ "body": body }))
        .send()
        .with_context(|| format!("commenting on GitHub issue at {url}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        bail!("commenting on issue {} failed: {status}: {text}", task.uid);
    }

    Ok(())
}

/// Close `task`'s issue on GitHub (`href` is the issue's API url).
fn close_issue(cfg: &GithubConfig, task: &Task) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .patch(&task.href)
        .bearer_auth(&cfg.token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "robottles")
        .json(&serde_json::json!({ "state": "closed" }))
        .send()
        .with_context(|| format!("closing GitHub issue at {}", task.href))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        bail!("closing issue {} failed: {status}: {text}", task.uid);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_with(number: u64, title: &str, created_at: &str, labels: &[&str]) -> GithubIssue {
        GithubIssue {
            number,
            title: title.to_string(),
            body: None,
            url: format!("https://api.github.com/repos/o/r/issues/{number}"),
            created_at: created_at.to_string(),
            labels: labels.iter().map(|l| GithubLabel { name: l.to_string() }).collect(),
            pull_request: None,
        }
    }

    fn priority_labels() -> Vec<String> {
        vec![
            "priority:critical".to_string(),
            "priority:high".to_string(),
            "priority:medium".to_string(),
            "priority:low".to_string(),
        ]
    }

    #[test]
    fn priority_rank_matches_configured_label_case_insensitively() {
        let issue = issue_with(1, "Bug", "2026-01-01T00:00:00Z", &["Priority:High", "bug"]);
        assert_eq!(priority_rank(&issue, &priority_labels()), 1);
    }

    #[test]
    fn priority_rank_is_last_when_unlabeled() {
        let issue = issue_with(1, "Bug", "2026-01-01T00:00:00Z", &["bug"]);
        assert_eq!(priority_rank(&issue, &priority_labels()), priority_labels().len());
    }

    #[test]
    fn priority_rank_uses_the_most_important_matching_label() {
        let issue = issue_with(1, "Bug", "2026-01-01T00:00:00Z", &["priority:low", "priority:critical"]);
        assert_eq!(priority_rank(&issue, &priority_labels()), 0);
    }

    #[test]
    fn sort_by_priority_orders_by_rank_then_created_then_number() {
        let labels = priority_labels();
        let mut ranked = vec![
            issue_with(3, "Low prio", "2026-01-01T00:00:00Z", &["priority:low"]),
            issue_with(1, "Later critical", "2026-02-01T00:00:00Z", &["priority:critical"]),
            issue_with(2, "Earlier critical", "2026-01-15T00:00:00Z", &["priority:critical"]),
            issue_with(4, "Unlabeled", "2026-01-01T00:00:00Z", &[]),
        ]
        .into_iter()
        .map(|issue| (priority_rank(&issue, &labels), issue))
        .collect::<Vec<_>>();

        sort_by_priority(&mut ranked);

        let order: Vec<u64> = ranked.iter().map(|(_, issue)| issue.number).collect();
        assert_eq!(order, vec![2, 1, 3, 4]);
    }

    #[test]
    fn issue_to_task_maps_fields() {
        let issue = issue_with(42, "Fix the thing", "2026-01-01T00:00:00Z", &["priority:high"]);
        let task = issue_to_task(issue);
        assert_eq!(task.uid, "42");
        assert_eq!(task.summary, "Fix the thing");
        assert_eq!(task.href, "https://api.github.com/repos/o/r/issues/42");
    }

    #[test]
    fn parses_issues_response_and_filters_out_pull_requests() {
        let body = r#"[
            {"number": 1, "title": "Issue one", "body": null, "url": "https://api.github.com/repos/o/r/issues/1", "created_at": "2026-01-01T00:00:00Z", "labels": [{"name": "priority:high"}]},
            {"number": 2, "title": "A PR", "body": null, "url": "https://api.github.com/repos/o/r/issues/2", "created_at": "2026-01-01T00:00:00Z", "labels": [], "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/2"}}
        ]"#;
        let issues: Vec<GithubIssue> = serde_json::from_str(body).unwrap();
        let open: Vec<_> = issues.into_iter().filter(|i| i.pull_request.is_none()).collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].number, 1);
    }
}
