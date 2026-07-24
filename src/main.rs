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
