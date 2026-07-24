use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context};

use auto_worker::caldav;
use auto_worker::config::Config;
use auto_worker::ical::Task;

fn main() -> anyhow::Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"));

    let cfg = Config::load(&config_path)?;

    println!("Fetching open tasks from {}", cfg.caldav.url);
    let mut tasks = caldav::fetch_open_tasks(&cfg.caldav)?;

    if tasks.is_empty() {
        println!("No open tasks found. Nothing to do.");
        return Ok(());
    }

    sort_by_priority(&mut tasks);

    let task = &tasks[0];
    println!(
        "Picked task [{}]: {} (priority={:?}, due={:?})",
        task.uid, task.summary, task.priority, task.due
    );

    if !cfg.project.path.is_dir() {
        bail!(
            "configured project path {} is not a directory",
            cfg.project.path.display()
        );
    }

    let prompt = build_prompt(task);
    run_claude(&cfg.project.path, &prompt)?;

    let branch = derive_branch_name(task);
    commit_to_branch(&cfg.project.path, &branch, task)
        .with_context(|| format!("committing task {} changes to branch {branch}", task.uid))?;

    println!("Marking task [{}] as completed in CalDAV", task.uid);
    caldav::mark_completed(&cfg.caldav, task)
        .with_context(|| format!("marking task {} as completed in CalDAV", task.uid))?;

    Ok(())
}

/// Order tasks "most important first": lower PRIORITY number wins (RFC 5545:
/// 1 = highest, 9 = lowest, 0/absent = unspecified and sorts last among
/// prioritized tasks), then earliest DUE date, then summary as a tiebreaker.
fn sort_by_priority(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        let pa = a.priority.filter(|&p| p > 0).unwrap_or(u32::MAX);
        let pb = b.priority.filter(|&p| p > 0).unwrap_or(u32::MAX);
        pa.cmp(&pb)
            .then_with(|| {
                let da = a.due.as_deref().unwrap_or("9999999999999");
                let db = b.due.as_deref().unwrap_or("9999999999999");
                da.cmp(db)
            })
            .then_with(|| a.summary.cmp(&b.summary))
    });
}

fn build_prompt(task: &Task) -> String {
    let mut prompt = format!("Please complete the following task from my CalDAV task list.\n\nTitle: {}\n", task.summary);
    if let Some(due) = &task.due {
        prompt.push_str(&format!("Due: {due}\n"));
    }
    if let Some(priority) = task.priority {
        prompt.push_str(&format!("Priority: {priority}\n"));
    }
    if let Some(description) = &task.description
        && !description.trim().is_empty()
    {
        prompt.push_str(&format!("\nDetails:\n{description}\n"));
    }
    prompt.push_str("\nWork in this project directory to accomplish the task above.");
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

fn print_git_status(project_dir: &std::path::Path) {
    match Command::new("git")
        .current_dir(project_dir)
        .arg("status")
        .output()
    {
        Ok(output) => {
            println!("--- git status ---");
            print!("{}", String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(e) => eprintln!("failed to run `git status`: {e}"),
    }
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

/// After Claude has made its changes, commit them on a dedicated branch
/// (never on `main`/`master`) and push that branch to `origin`. Skips the
/// commit if there's nothing to commit.
fn commit_to_branch(project_dir: &std::path::Path, branch: &str, task: &Task) -> anyhow::Result<()> {
    let current = current_branch(project_dir)?;
    if current == branch {
        // Already on the target branch (e.g. re-run for the same task).
    } else if branch_exists(project_dir, branch)? {
        run_git(project_dir, &["checkout", branch])?;
    } else {
        run_git(project_dir, &["checkout", "-b", branch])?;
    }

    let status = Command::new("git")
        .current_dir(project_dir)
        .args(["status", "--porcelain"])
        .output()
        .context("running `git status --porcelain`")?;
    if !status.status.success() {
        bail!("`git status --porcelain` failed");
    }
    if status.stdout.is_empty() {
        println!("No changes to commit on branch {branch}.");
        return Ok(());
    }

    run_git(project_dir, &["add", "-A"])?;
    run_git(
        project_dir,
        &["commit", "-m", &format!("Complete task: {}", task.summary)],
    )?;
    run_git(project_dir, &["push", "-u", "origin", branch])?;
    println!("Pushed branch {branch} to origin.");

    Ok(())
}

fn current_branch(project_dir: &std::path::Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(project_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running `git rev-parse --abbrev-ref HEAD`")?;
    if !output.status.success() {
        bail!(
            "`git rev-parse --abbrev-ref HEAD` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn branch_exists(project_dir: &std::path::Path, branch: &str) -> anyhow::Result<bool> {
    let status = Command::new("git")
        .current_dir(project_dir)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .context("running `git show-ref`")?;
    Ok(status.success())
}

fn run_git(project_dir: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .current_dir(project_dir)
        .args(args)
        .status()
        .with_context(|| format!("running `git {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`git {}` exited with status {status}", args.join(" "));
    }
    Ok(())
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
