use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context};

use auto_worker::config::Config;
use auto_worker::git::{commit_changes, prepare_branch, print_git_status};
use auto_worker::ical::Task;

fn main() -> anyhow::Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"));

    let cfg = Config::load(&config_path)?;
    let project = cfg.project;
    let source = cfg.source.build();

    println!("Fetching next task");
    let Some(task) = source.get_next_task()? else {
        println!("No open tasks found. Nothing to do.");
        return Ok(());
    };
    let task = &task;
    println!(
        "Picked task [{}]: {} (priority={:?}, due={:?})",
        task.uid, task.summary, task.priority, task.due
    );

    if !project.path.is_dir() {
        bail!(
            "configured project path {} is not a directory",
            project.path.display()
        );
    }

    let branch = derive_branch_name(task);
    prepare_branch(&project.path, &branch)
        .with_context(|| format!("preparing branch {branch} for task {}", task.uid))?;

    let prompt = build_prompt(task);
    run_claude(&project.path, &prompt)?;

    if project.commit_changes {
        commit_changes(&project.path, &branch, task).with_context(|| {
            format!("committing task {} changes to branch {branch}", task.uid)
        })?;
    } else {
        println!("Skipping git commit (commit_changes is disabled in config).");
    }

    println!("Marking task [{}] as completed", task.uid);
    source
        .mark_completed(task)
        .with_context(|| format!("marking task {} as completed", task.uid))?;

    Ok(())
}

fn build_prompt(task: &Task) -> String {
    let mut prompt = format!("Please complete the following task:\n\n{}\n", task.summary);
    if let Some(description) = &task.description
        && !description.trim().is_empty()
    {
        prompt.push_str(&format!("\nDetails:\n{description}\n"));
    }
    prompt.push_str("\nWork in this project directory to accomplish the task above. Any instructions in the task are about the files and codebase in this directory. If new features are requested, make sure to add sensible tests.");
    prompt
}

fn run_claude(project_dir: &std::path::Path, prompt: &str) -> anyhow::Result<()> {
    println!("Starting Claude session in {}", project_dir.display());

    let status = Command::new("claude")
        .current_dir(project_dir)
        .arg("--print")
        .arg("--dangerously-skip-permissions")
        .arg(prompt)
        .status()
        .context("launching `claude` — is the Claude Code CLI installed and on PATH?")?;

    if !status.success() {
        bail!("claude exited with status {status}");
    }

    print_git_status(project_dir);

    Ok(())
}

/// Derive a git branch name from a task: prefer a sanitized version of the
/// CalDAV UID, falling back to an underscore-separated slug of the
/// description (or summary, if there's no description) when the UID is
/// empty.
fn derive_branch_name(task: &Task) -> String {
    let from_uid = sanitize_branch_component(&task.uid);
    if !from_uid.is_empty() {
        return from_uid;
    }

    let source = task
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .unwrap_or(&task.summary);
    let slug = sanitize_branch_component(source);
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

/// Turn arbitrary text into a lowercase, underscore-separated slug safe to
/// use as a git branch name component (runs of anything that isn't
/// alphanumeric collapse to a single `_`, and leading/trailing `_` are
/// trimmed).
fn sanitize_branch_component(s: &str) -> String {
    let mut result = String::new();
    let mut last_was_sep = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            result.push('_');
            last_was_sep = true;
        }
    }
    while result.ends_with('_') {
        result.pop();
    }
    result
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

    #[test]
    fn branch_name_prefers_uid() {
        let task = task_with("20260724T191500Z-abc123@host.example", "Some summary", None);
        assert_eq!(derive_branch_name(&task), "20260724t191500z_abc123_host_example");
    }

    #[test]
    fn branch_name_falls_back_to_description() {
        let task = task_with("", "Fallback summary", Some("Fix the login bug!!"));
        assert_eq!(derive_branch_name(&task), "fix_the_login_bug");
    }

    #[test]
    fn branch_name_falls_back_to_summary_without_description() {
        let task = task_with("", "Fix the login bug", None);
        assert_eq!(derive_branch_name(&task), "fix_the_login_bug");
    }

    #[test]
    fn branch_name_defaults_to_task_when_everything_empty() {
        let task = task_with("", "", None);
        assert_eq!(derive_branch_name(&task), "task");
    }
}
