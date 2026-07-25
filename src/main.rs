use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context};

use auto_worker::config::Config;
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

/// Before Claude starts working, stash any pre-existing changes out of the
/// way (so Claude's diff — and the eventual commit — only reflects its own
/// work) and switch to the dedicated per-task branch (never `main`/`master`),
/// creating it from the current HEAD if it doesn't exist yet.
fn prepare_branch(project_dir: &std::path::Path, branch: &str) -> anyhow::Result<()> {
    if working_tree_dirty(project_dir)? {
        run_git(
            project_dir,
            &[
                "stash",
                "push",
                "--include-untracked",
                "-m",
                &format!("auto-worker: stashed before task branch {branch}"),
            ],
        )?;
        println!("Stashed pre-existing changes before starting task.");
    }

    let current = current_branch(project_dir)?;
    if current == branch {
        // Already on the target branch (e.g. re-run for the same task).
    } else if branch_exists(project_dir, branch)? {
        run_git(project_dir, &["checkout", branch])?;
    } else {
        run_git(project_dir, &["checkout", "-b", branch])?;
    }

    Ok(())
}

/// After Claude has made its changes, commit them (explicitly staging any
/// new, previously-untracked files) on the current branch and push it to
/// `origin`. Skips the commit if there's nothing to commit.
fn commit_changes(project_dir: &std::path::Path, branch: &str, task: &Task) -> anyhow::Result<()> {
    if !working_tree_dirty(project_dir)? {
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

/// True if `git status --porcelain` reports any staged, unstaged, or
/// untracked changes.
fn working_tree_dirty(project_dir: &std::path::Path) -> anyhow::Result<bool> {
    let status = Command::new("git")
        .current_dir(project_dir)
        .args(["status", "--porcelain"])
        .output()
        .context("running `git status --porcelain`")?;
    if !status.status.success() {
        bail!("`git status --porcelain` failed");
    }
    Ok(!status.stdout.is_empty())
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

    /// Run a git command, panicking on failure. Only for test setup, where a
    /// failure means the test fixture itself is broken.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
        assert!(status.success(), "`git {}` failed", args.join(" "));
    }

    /// Set up a work tree with an `origin` remote (a local bare repo) and one
    /// commit on `main`, ready for `prepare_branch`/`commit_changes` tests.
    fn init_repo_with_origin() -> (tempfile::TempDir, tempfile::TempDir) {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "--bare", "-q"]);

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
    fn prepare_branch_stashes_dirty_changes_and_creates_branch() {
        let (_origin, work) = init_repo_with_origin();

        // Dirty the work tree: modify a tracked file and add an untracked one.
        std::fs::write(work.path().join("README.md"), "changed\n").unwrap();
        std::fs::write(work.path().join("untracked.txt"), "scratch\n").unwrap();
        assert!(working_tree_dirty(work.path()).unwrap());

        prepare_branch(work.path(), "task_branch").unwrap();

        assert_eq!(current_branch(work.path()).unwrap(), "task_branch");
        assert!(
            !working_tree_dirty(work.path()).unwrap(),
            "pre-existing changes should have been stashed"
        );
        assert!(!work.path().join("untracked.txt").exists());

        let stash_list = Command::new("git")
            .current_dir(work.path())
            .args(["stash", "list"])
            .output()
            .unwrap();
        assert!(!stash_list.stdout.is_empty(), "expected a stash entry");
    }

    #[test]
    fn prepare_branch_is_noop_on_clean_tree() {
        let (_origin, work) = init_repo_with_origin();

        prepare_branch(work.path(), "task_branch").unwrap();

        assert_eq!(current_branch(work.path()).unwrap(), "task_branch");
        assert!(!working_tree_dirty(work.path()).unwrap());
        let stash_list = Command::new("git")
            .current_dir(work.path())
            .args(["stash", "list"])
            .output()
            .unwrap();
        assert!(stash_list.stdout.is_empty(), "expected no stash entry");
    }

    #[test]
    fn commit_changes_adds_untracked_files_commits_and_pushes() {
        let (origin, work) = init_repo_with_origin();
        prepare_branch(work.path(), "task_branch").unwrap();

        // Simulate Claude producing both a modified tracked file and a new,
        // previously-untracked one.
        std::fs::write(work.path().join("README.md"), "updated by claude\n").unwrap();
        std::fs::write(work.path().join("new_file.txt"), "brand new\n").unwrap();

        let task = task_with("task-uid", "Add a new feature", None);
        commit_changes(work.path(), "task_branch", &task).unwrap();

        assert!(!working_tree_dirty(work.path()).unwrap());

        let log = Command::new("git")
            .current_dir(work.path())
            .args(["log", "-1", "--pretty=%s"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "Complete task: Add a new feature"
        );

        let ls_files = Command::new("git")
            .current_dir(work.path())
            .args(["ls-files"])
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&ls_files.stdout);
        assert!(tracked.contains("new_file.txt"));

        let remote_branches = Command::new("git")
            .current_dir(origin.path())
            .args(["branch", "--list", "task_branch"])
            .output()
            .unwrap();
        assert!(
            !remote_branches.stdout.is_empty(),
            "expected task_branch to have been pushed to origin"
        );
    }

    #[test]
    fn commit_changes_skips_commit_when_nothing_changed() {
        let (_origin, work) = init_repo_with_origin();
        prepare_branch(work.path(), "task_branch").unwrap();

        let before = Command::new("git")
            .current_dir(work.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();

        let task = task_with("task-uid", "Nothing to do", None);
        commit_changes(work.path(), "task_branch", &task).unwrap();

        let after = Command::new("git")
            .current_dir(work.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(before.stdout, after.stdout, "HEAD should not have moved");
    }
}
