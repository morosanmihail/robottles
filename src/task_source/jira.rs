use anyhow::{bail, Context};
use serde::Deserialize;

use super::ical::Task;
use super::TaskSource;
use crate::config::JiraConfig;

/// A task source backed by a Jira board. Priority is derived from the
/// issue's `priority` field: `priority_order` in config lists priority
/// names from highest to lowest, and the highest-priority issue still in
/// `todo_status` is returned first. Completing a task doesn't resolve the
/// issue — it transitions it to `review_status` for a human to check.
pub struct JiraTaskSource {
    pub config: JiraConfig,
}

impl JiraTaskSource {
    pub fn new(config: JiraConfig) -> Self {
        Self { config }
    }
}

impl TaskSource for JiraTaskSource {
    fn get_next_task(&self) -> anyhow::Result<Option<Task>> {
        let issues = fetch_todo_issues(&self.config)?;
        let mut ranked: Vec<(usize, JiraIssue)> = issues
            .into_iter()
            .map(|issue| (priority_rank(&issue, &self.config.priority_order), issue))
            .collect();
        sort_by_priority(&mut ranked);
        Ok(ranked.into_iter().next().map(|(_, issue)| issue_to_task(issue)))
    }

    fn mark_completed(&self, task: &Task) -> anyhow::Result<()> {
        transition_issue(&self.config, task)
    }
}

/// One issue as returned by the Jira REST API (only the fields we need).
#[derive(Debug, Deserialize)]
struct JiraIssue {
    key: String,
    #[serde(rename = "self")]
    self_url: String,
    fields: JiraFields,
}

#[derive(Debug, Deserialize)]
struct JiraFields {
    summary: String,
    description: Option<String>,
    priority: Option<JiraPriority>,
    created: String,
    status: JiraStatus,
}

#[derive(Debug, Deserialize)]
struct JiraPriority {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    issues: Vec<JiraIssue>,
}

#[derive(Debug, Deserialize)]
struct JiraTransition {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionsResponse {
    transitions: Vec<JiraTransition>,
}

/// Rank of `issue` among `priority_order` (0 = highest priority). Issues
/// with no priority, or one not in `priority_order`, rank last.
fn priority_rank(issue: &JiraIssue, priority_order: &[String]) -> usize {
    issue
        .fields
        .priority
        .as_ref()
        .and_then(|p| priority_order.iter().position(|o| o.eq_ignore_ascii_case(&p.name)))
        .unwrap_or(priority_order.len())
}

/// Order ranked issues "most important first": lower rank wins, then
/// earliest created, then issue key as a final tiebreaker.
fn sort_by_priority(ranked: &mut [(usize, JiraIssue)]) {
    ranked.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb)
            .then_with(|| a.fields.created.cmp(&b.fields.created))
            .then_with(|| a.key.cmp(&b.key))
    });
}

fn issue_to_task(issue: JiraIssue) -> Task {
    Task {
        uid: issue.key,
        summary: issue.fields.summary,
        status: Some(issue.fields.status.name),
        priority: None,
        due: None,
        description: issue.fields.description,
        href: issue.self_url,
    }
}

/// Fetch every issue on the configured board that's currently in
/// `todo_status`.
fn fetch_todo_issues(cfg: &JiraConfig) -> anyhow::Result<Vec<JiraIssue>> {
    let url = format!(
        "{}/rest/agile/1.0/board/{}/issue",
        cfg.base_url.trim_end_matches('/'),
        cfg.board_id
    );
    let jql = format!("status = \"{}\"", cfg.todo_status);

    let client = reqwest::blocking::Client::new();
    let response = client
        .get(&url)
        .basic_auth(&cfg.email, Some(&cfg.api_token))
        .query(&[("jql", jql.as_str()), ("fields", "summary,description,priority,created,status")])
        .send()
        .with_context(|| format!("fetching board issues from {url}"))?;

    let status = response.status();
    let body = response.text().context("reading Jira search response body")?;
    if !status.is_success() {
        bail!("Jira API returned {status} for {url}: {body}");
    }

    let parsed: JiraSearchResponse =
        serde_json::from_str(&body).context("parsing Jira search response")?;
    Ok(parsed.issues)
}

/// Transition `task`'s issue to `review_status` (`href` is the issue's API
/// url). Looks up the available transitions first, since Jira identifies
/// transitions by id rather than target status name.
fn transition_issue(cfg: &JiraConfig, task: &Task) -> anyhow::Result<()> {
    let transitions = fetch_transitions(cfg, &task.href)?;
    let transition_id = find_transition_id(&transitions, &cfg.review_status).ok_or_else(|| {
        let available: Vec<&str> = transitions.iter().map(|t| t.name.as_str()).collect();
        anyhow::anyhow!(
            "no transition to '{}' available for issue {} (available: {})",
            cfg.review_status,
            task.uid,
            available.join(", ")
        )
    })?;

    let client = reqwest::blocking::Client::new();
    let transitions_url = format!("{}/transitions", task.href);
    let response = client
        .post(&transitions_url)
        .basic_auth(&cfg.email, Some(&cfg.api_token))
        .json(&serde_json::json!({ "transition": { "id": transition_id } }))
        .send()
        .with_context(|| format!("transitioning Jira issue at {transitions_url}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        bail!("transitioning issue {} to '{}' failed: {status}: {text}", task.uid, cfg.review_status);
    }

    Ok(())
}

fn fetch_transitions(cfg: &JiraConfig, issue_url: &str) -> anyhow::Result<Vec<JiraTransition>> {
    let url = format!("{issue_url}/transitions");
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(&url)
        .basic_auth(&cfg.email, Some(&cfg.api_token))
        .send()
        .with_context(|| format!("fetching available transitions from {url}"))?;

    let status = response.status();
    let body = response.text().context("reading Jira transitions response body")?;
    if !status.is_success() {
        bail!("Jira API returned {status} for {url}: {body}");
    }

    let parsed: JiraTransitionsResponse =
        serde_json::from_str(&body).context("parsing Jira transitions response")?;
    Ok(parsed.transitions)
}

/// Find the id of the transition leading to `target_status`, matched
/// case-insensitively against the transition's name.
fn find_transition_id(transitions: &[JiraTransition], target_status: &str) -> Option<String> {
    transitions
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(target_status))
        .map(|t| t.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_with(key: &str, created: &str, priority: Option<&str>) -> JiraIssue {
        JiraIssue {
            key: key.to_string(),
            self_url: format!("https://example.atlassian.net/rest/api/2/issue/{key}"),
            fields: JiraFields {
                summary: format!("Summary for {key}"),
                description: None,
                priority: priority.map(|p| JiraPriority { name: p.to_string() }),
                created: created.to_string(),
                status: JiraStatus { name: "To Do".to_string() },
            },
        }
    }

    fn priority_order() -> Vec<String> {
        vec!["Highest".to_string(), "High".to_string(), "Medium".to_string(), "Low".to_string(), "Lowest".to_string()]
    }

    #[test]
    fn priority_rank_matches_configured_name_case_insensitively() {
        let issue = issue_with("A-1", "2026-01-01T00:00:00.000+0000", Some("high"));
        assert_eq!(priority_rank(&issue, &priority_order()), 1);
    }

    #[test]
    fn priority_rank_is_last_when_unset() {
        let issue = issue_with("A-1", "2026-01-01T00:00:00.000+0000", None);
        assert_eq!(priority_rank(&issue, &priority_order()), priority_order().len());
    }

    #[test]
    fn priority_rank_is_last_when_unrecognized() {
        let issue = issue_with("A-1", "2026-01-01T00:00:00.000+0000", Some("Weird"));
        assert_eq!(priority_rank(&issue, &priority_order()), priority_order().len());
    }

    #[test]
    fn sort_by_priority_orders_by_rank_then_created_then_key() {
        let order = priority_order();
        let mut ranked = vec![
            issue_with("A-3", "2026-01-01T00:00:00.000+0000", Some("Low")),
            issue_with("A-1", "2026-02-01T00:00:00.000+0000", Some("Highest")),
            issue_with("A-2", "2026-01-15T00:00:00.000+0000", Some("Highest")),
            issue_with("A-4", "2026-01-01T00:00:00.000+0000", None),
        ]
        .into_iter()
        .map(|issue| (priority_rank(&issue, &order), issue))
        .collect::<Vec<_>>();

        sort_by_priority(&mut ranked);

        let order: Vec<&str> = ranked.iter().map(|(_, issue)| issue.key.as_str()).collect();
        assert_eq!(order, vec!["A-2", "A-1", "A-3", "A-4"]);
    }

    #[test]
    fn issue_to_task_maps_fields() {
        let issue = issue_with("PROJ-42", "2026-01-01T00:00:00.000+0000", Some("High"));
        let task = issue_to_task(issue);
        assert_eq!(task.uid, "PROJ-42");
        assert_eq!(task.summary, "Summary for PROJ-42");
        assert_eq!(task.status.as_deref(), Some("To Do"));
        assert_eq!(task.href, "https://example.atlassian.net/rest/api/2/issue/PROJ-42");
    }

    #[test]
    fn parses_search_response() {
        let body = r#"{"issues": [
            {"key": "A-1", "self": "https://example.atlassian.net/rest/api/2/issue/1",
             "fields": {"summary": "Do the thing", "description": "details", "priority": {"name": "High"}, "created": "2026-01-01T00:00:00.000+0000", "status": {"name": "To Do"}}}
        ]}"#;
        let parsed: JiraSearchResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.issues.len(), 1);
        assert_eq!(parsed.issues[0].key, "A-1");
        assert_eq!(parsed.issues[0].fields.description.as_deref(), Some("details"));
    }

    #[test]
    fn find_transition_id_matches_case_insensitively() {
        let transitions = vec![
            JiraTransition { id: "11".to_string(), name: "To Do".to_string() },
            JiraTransition { id: "21".to_string(), name: "in review".to_string() },
        ];
        assert_eq!(find_transition_id(&transitions, "In Review"), Some("21".to_string()));
    }

    #[test]
    fn find_transition_id_returns_none_when_missing() {
        let transitions = vec![JiraTransition { id: "11".to_string(), name: "Done".to_string() }];
        assert_eq!(find_transition_id(&transitions, "In Review"), None);
    }
}
