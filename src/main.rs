use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use log::{error, info, warn};

use robottles::config::{Config, ExecutionConfig, TargetConfig};
use robottles::git::branch_name::derive_branch_name;
use robottles::git::pull_request::open_task_pull_request;
use robottles::git::{
    cleanup_unused_branch, commit_changes, is_git_repo, prepare_branch, sync_default_branch,
    working_tree_dirty,
};
use robottles::task_source::ical::Task;

/// The configured project path has `git_enabled` set (the default) but isn't
/// actually a git repository. Distinguished from other task-running errors
/// so `main` can exit cleanly for it in one-shot mode instead of surfacing a
/// nonzero exit code, while loop mode just logs and moves on like any other
/// per-iteration error.
#[derive(Debug)]
struct NotAGitRepo(PathBuf);

impl fmt::Display for NotAGitRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "configured project path {} is not a git repository, but git_enabled is true for this target",
            self.0.display()
        )
    }
}

impl std::error::Error for NotAGitRepo {}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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
            match run_task(target) {
                Err(err) if err.downcast_ref::<NotAGitRepo>().is_some() => {
                    warn!("{err}");
                    Ok(())
                }
                other => other,
            }
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
        info!("=== Loop iteration: target '{name}' ===");
        let target = cfg
            .target(name)
            .unwrap_or_else(|| panic!("target '{name}' vanished from config"));
        if let Err(err) = run_task(target) {
            error!("running task for target '{name}': {err:?}");
        }
        iteration += 1;

        info!("Sleeping {}s before next iteration", delay.as_secs());
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

    info!("Fetching next task");
    let Some(task) = source.get_next_task()? else {
        info!("No open tasks found. Nothing to do.");
        return Ok(());
    };
    let task = &task;
    info!(
        "Picked task [{}]: {} (priority={:?}, due={:?})",
        task.uid, task.summary, task.priority, task.due
    );

    if !project.path.is_dir() {
        bail!(
            "configured project path {} is not a directory",
            project.path.display()
        );
    }

    if project.git_enabled && !is_git_repo(&project.path) {
        return Err(NotAGitRepo(project.path.clone()).into());
    }

    let branch = derive_branch_name(task);

    let default_branch = if project.git_enabled {
        let default_branch = sync_default_branch(&project.path)
            .with_context(|| format!("syncing default branch in {}", project.path.display()))?;
        prepare_branch(&project.path, &branch)
            .with_context(|| format!("preparing branch {branch} for task {}", task.uid))?;
        Some(default_branch)
    } else {
        info!("Skipping git operations (git_enabled is disabled in config).");
        None
    };

    let prompt = build_prompt(task);
    let runner = project.agent.build();
    runner.run(&project.path, &prompt)?;

    if project.git_enabled {
        if working_tree_dirty(&project.path)? {
            if project.commit_changes {
                commit_changes(&project.path, &branch, task).with_context(|| {
                    format!("committing task {} changes to branch {branch}", task.uid)
                })?;

                if let Some(pr_config) = &project.pull_request {
                    let base = pr_config
                        .base_branch
                        .clone()
                        .or_else(|| default_branch.clone())
                        .unwrap_or_else(|| "main".to_string());
                    match open_task_pull_request(&project.path, &branch, &base, task, pr_config) {
                        Ok(url) => info!("Opened pull request: {url}"),
                        Err(err) => {
                            error!("failed to open pull request for branch {branch}: {err:#}")
                        }
                    }
                }
            } else {
                info!("Skipping git commit (commit_changes is disabled in config).");
            }
        } else {
            info!("Agent made no changes; cleaning up branch {branch}.");
            cleanup_unused_branch(&project.path, &branch).with_context(|| {
                format!("cleaning up unused branch {branch} for task {}", task.uid)
            })?;
        }
    }

    info!("Marking task [{}] as completed", task.uid);
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

#[cfg(test)]
mod tests {
    use super::*;
    use robottles::config::{ProjectConfig, RunnerConfig, SourceConfig};

    /// A dummy-source, noop-agent target against `path`, so `run_task` can be
    /// exercised end to end without a real task supplier or coding agent.
    fn dummy_target(path: PathBuf, git_enabled: bool) -> TargetConfig {
        TargetConfig {
            source: SourceConfig::Dummy,
            project: ProjectConfig {
                path,
                commit_changes: true,
                git_enabled,
                agent: RunnerConfig::Noop,
                pull_request: None,
            },
        }
    }

    /// Run a git command, panicking on failure. Only for test setup, where a
    /// failure means the test fixture itself is broken.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
        assert!(status.success(), "`git {}` failed", args.join(" "));
    }

    /// Set up a work tree with an `origin` remote (a local bare repo) and one
    /// commit on `main`, ready for `run_task` tests with `git_enabled: true`.
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
    fn run_task_with_git_disabled_succeeds_in_non_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dummy_target(dir.path().to_path_buf(), false);
        run_task(target).unwrap();
    }

    #[test]
    fn run_task_with_git_enabled_fails_cleanly_in_non_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dummy_target(dir.path().to_path_buf(), true);
        let err = run_task(target).unwrap_err();
        assert!(
            err.downcast_ref::<NotAGitRepo>().is_some(),
            "expected a NotAGitRepo error, got: {err:?}"
        );
    }

    #[test]
    fn run_task_with_git_enabled_runs_git_operations_in_a_real_repo() {
        let (_origin, work) = init_repo_with_origin();
        let target = dummy_target(work.path().to_path_buf(), true);
        run_task(target).unwrap();
        // Dummy task makes no changes, so run_task should have cleaned up
        // the task branch and left the repo back on `main`.
        assert_eq!(
            robottles::git::current_branch(work.path()).unwrap(),
            "main"
        );
    }

    #[test]
    fn run_task_with_git_disabled_skips_git_operations_in_a_real_repo() {
        let (_origin, work) = init_repo_with_origin();
        let starting_branch = robottles::git::current_branch(work.path()).unwrap();
        let target = dummy_target(work.path().to_path_buf(), false);
        run_task(target).unwrap();
        // No branch should have been created/switched to.
        assert_eq!(
            robottles::git::current_branch(work.path()).unwrap(),
            starting_branch
        );
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

}
