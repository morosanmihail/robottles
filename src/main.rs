use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};

use robottles::config::{Config, ExecutionConfig, TargetConfig};
use robottles::git::{
    cleanup_unused_branch, commit_changes, prepare_branch, sync_default_branch,
    working_tree_dirty,
};
use robottles::task_source::ical::Task;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"));
    let target_name = args.next();

    let cfg = Config::load(&config_path)?;

    match cfg.execution.clone() {
        ExecutionConfig::Once => {
            let target = cfg
                .into_target(target_name.as_deref())
                .context("selecting target to run against")?;
            run_task(target)
        }
        ExecutionConfig::Loop { delay_secs } => run_loop(&cfg, Duration::from_secs(delay_secs)),
    }
}

/// Repeatedly picks a task for the next configured target (cycling through
/// all targets in a stable order) and runs it, sleeping `delay` between
/// iterations, until the process is manually stopped. Errors from an
/// individual iteration are logged rather than aborting the loop, since a
/// transient failure on one target shouldn't stop the others from being
/// worked on.
fn run_loop(cfg: &Config, delay: Duration) -> anyhow::Result<()> {
    let target_names = cfg.target_names();
    if target_names.is_empty() {
        bail!("no targets configured");
    }

    let mut iteration = 0usize;
    loop {
        let name = &target_names[next_target_index(target_names.len(), iteration)];
        println!("=== Loop iteration: target '{name}' ===");
        let target = cfg
            .target(name)
            .unwrap_or_else(|| panic!("target '{name}' vanished from config"));
        if let Err(err) = run_task(target) {
            eprintln!("Error running task for target '{name}': {err:?}");
        }
        iteration += 1;

        println!("Sleeping {}s before next iteration", delay.as_secs());
        std::thread::sleep(delay);
    }
}

/// Which target index to use on a given loop iteration, cycling round-robin
/// through `target_count` configured targets.
fn next_target_index(target_count: usize, iteration: usize) -> usize {
    iteration % target_count
}

/// Fetches and completes a single task for one target.
fn run_task(target: TargetConfig) -> anyhow::Result<()> {
    let project = target.project;
    let source = target.source.build();

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

    sync_default_branch(&project.path)
        .with_context(|| format!("syncing default branch in {}", project.path.display()))?;

    let branch = derive_branch_name(task);
    prepare_branch(&project.path, &branch)
        .with_context(|| format!("preparing branch {branch} for task {}", task.uid))?;

    let prompt = build_prompt(task);
    let runner = project.agent.build();
    runner.run(&project.path, &prompt)?;

    if working_tree_dirty(&project.path)? {
        if project.commit_changes {
            commit_changes(&project.path, &branch, task).with_context(|| {
                format!("committing task {} changes to branch {branch}", task.uid)
            })?;
        } else {
            println!("Skipping git commit (commit_changes is disabled in config).");
        }
    } else {
        println!("Agent made no changes; cleaning up branch {branch}.");
        cleanup_unused_branch(&project.path, &branch).with_context(|| {
            format!("cleaning up unused branch {branch} for task {}", task.uid)
        })?;
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
    fn next_target_index_cycles_round_robin() {
        assert_eq!(next_target_index(3, 0), 0);
        assert_eq!(next_target_index(3, 1), 1);
        assert_eq!(next_target_index(3, 2), 2);
        assert_eq!(next_target_index(3, 3), 0);
        assert_eq!(next_target_index(3, 4), 1);
    }

    #[test]
    fn next_target_index_with_single_target_always_zero() {
        assert_eq!(next_target_index(1, 0), 0);
        assert_eq!(next_target_index(1, 5), 0);
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
