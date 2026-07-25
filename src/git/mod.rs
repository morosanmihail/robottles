use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};

use crate::task_source::ical::Task;

/// True if `project_dir` is inside a git working tree.
pub fn is_git_repo(project_dir: &Path) -> bool {
    Command::new("git")
        .current_dir(project_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn print_git_status(project_dir: &Path) {
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

/// Stash any pre-existing changes out of the way (so Claude's diff — and the
/// eventual commit — only reflects its own work) if the tree is dirty.
fn stash_if_dirty(project_dir: &Path, reason: &str) -> anyhow::Result<()> {
    if working_tree_dirty(project_dir)? {
        run_git(
            project_dir,
            &[
                "stash",
                "push",
                "--include-untracked",
                "-m",
                &format!("auto-worker: stashed before {reason}"),
            ],
        )?;
        println!("Stashed pre-existing changes before {reason}.");
    }
    Ok(())
}

/// Before picking a task or starting an agent run, put the repo's default
/// branch (`main`/`master`/`develop`, or whatever `origin/HEAD` points at) in
/// a known-good state: check it out and pull any remote changes. Any
/// pre-existing dirty changes are stashed out of the way first. Returns the
/// name of the default branch.
pub fn sync_default_branch(project_dir: &Path) -> anyhow::Result<String> {
    let default = default_branch(project_dir)?;
    stash_if_dirty(project_dir, "syncing default branch")?;

    if current_branch(project_dir)? != default {
        run_git(project_dir, &["checkout", &default])?;
    }

    if has_remote(project_dir, "origin")? {
        run_git(project_dir, &["pull", "--ff-only", "origin", &default])?;
        println!("Pulled latest changes for {default} from origin.");
    }

    Ok(default)
}

/// True if `project_dir` has a remote configured with the given name.
fn has_remote(project_dir: &Path, name: &str) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .current_dir(project_dir)
        .args(["remote"])
        .output()
        .context("running `git remote`")?;
    if !output.status.success() {
        bail!(
            "`git remote` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line == name))
}

/// Before Claude starts working, stash any pre-existing changes out of the
/// way (so Claude's diff — and the eventual commit — only reflects its own
/// work) and switch to the dedicated per-task branch (never `main`/`master`),
/// creating it from the current HEAD if it doesn't exist yet.
pub fn prepare_branch(project_dir: &Path, branch: &str) -> anyhow::Result<()> {
    stash_if_dirty(project_dir, &format!("starting task branch {branch}"))?;

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
pub fn commit_changes(project_dir: &Path, branch: &str, task: &Task) -> anyhow::Result<()> {
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
pub fn working_tree_dirty(project_dir: &Path) -> anyhow::Result<bool> {
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

pub fn current_branch(project_dir: &Path) -> anyhow::Result<String> {
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

/// Determine the repository's main branch: the remote `origin/HEAD` target
/// branch if one is configured, otherwise the first of `main`, `master`,
/// `develop` that exists locally.
pub fn default_branch(project_dir: &Path) -> anyhow::Result<String> {
    let symbolic_ref = Command::new("git")
        .current_dir(project_dir)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .context("running `git symbolic-ref refs/remotes/origin/HEAD`")?;
    if symbolic_ref.status.success() {
        let reference = String::from_utf8_lossy(&symbolic_ref.stdout)
            .trim()
            .to_string();
        if let Some(name) = reference.strip_prefix("refs/remotes/origin/") {
            return Ok(name.to_string());
        }
    }

    for candidate in ["main", "master", "develop"] {
        if branch_exists(project_dir, candidate)? {
            return Ok(candidate.to_string());
        }
    }

    bail!("could not determine default branch (no origin/HEAD and none of main/master/develop exist)")
}

/// If the agent made no changes on the task branch, discard it: switch back
/// to the repo's default branch and delete the now-unused task branch.
pub fn cleanup_unused_branch(project_dir: &Path, branch: &str) -> anyhow::Result<()> {
    let default = default_branch(project_dir)?;
    if default == branch {
        return Ok(());
    }

    run_git(project_dir, &["checkout", &default])?;
    run_git(project_dir, &["branch", "-D", branch])?;
    println!("No changes made; deleted branch {branch} and switched back to {default}.");

    Ok(())
}

pub fn branch_exists(project_dir: &Path, branch: &str) -> anyhow::Result<bool> {
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

pub fn run_git(project_dir: &Path, args: &[&str]) -> anyhow::Result<()> {
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

    /// Run a git command, panicking on failure. Only for test setup, where a
    /// failure means the test fixture itself is broken.
    fn git(dir: &Path, args: &[&str]) {
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
    fn is_git_repo_true_for_git_working_tree() {
        let (_origin, work) = init_repo_with_origin();
        assert!(is_git_repo(work.path()));
    }

    #[test]
    fn is_git_repo_false_for_non_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(dir.path()));
    }

    #[test]
    fn default_branch_falls_back_to_local_main() {
        let (_origin, work) = init_repo_with_origin();
        assert_eq!(default_branch(work.path()).unwrap(), "main");
    }

    #[test]
    fn cleanup_unused_branch_deletes_branch_and_returns_to_default() {
        let (_origin, work) = init_repo_with_origin();
        prepare_branch(work.path(), "task_branch").unwrap();
        assert!(!working_tree_dirty(work.path()).unwrap());

        cleanup_unused_branch(work.path(), "task_branch").unwrap();

        assert_eq!(current_branch(work.path()).unwrap(), "main");
        assert!(!branch_exists(work.path(), "task_branch").unwrap());
    }

    #[test]
    fn sync_default_branch_checks_out_main_and_pulls_remote_changes() {
        let (origin, work) = init_repo_with_origin();
        prepare_branch(&work.path(), "task_branch").unwrap();
        assert_eq!(current_branch(work.path()).unwrap(), "task_branch");

        // Simulate a teammate pushing a new commit to origin/main while this
        // clone was off on the task branch.
        let other_clone = tempfile::tempdir().unwrap();
        git(
            other_clone.path(),
            &["clone", "-q", origin.path().to_str().unwrap(), "."],
        );
        git(other_clone.path(), &["config", "user.email", "test@example.com"]);
        git(other_clone.path(), &["config", "user.name", "Test User"]);
        std::fs::write(other_clone.path().join("README.md"), "from teammate\n").unwrap();
        git(other_clone.path(), &["commit", "-q", "-am", "teammate update"]);
        git(other_clone.path(), &["push", "-q"]);

        let default = sync_default_branch(work.path()).unwrap();

        assert_eq!(default, "main");
        assert_eq!(current_branch(work.path()).unwrap(), "main");
        assert_eq!(
            std::fs::read_to_string(work.path().join("README.md")).unwrap(),
            "from teammate\n"
        );
    }

    #[test]
    fn sync_default_branch_stashes_dirty_changes_before_switching() {
        let (_origin, work) = init_repo_with_origin();
        prepare_branch(&work.path(), "task_branch").unwrap();

        std::fs::write(work.path().join("README.md"), "dirty change\n").unwrap();
        assert!(working_tree_dirty(work.path()).unwrap());

        sync_default_branch(work.path()).unwrap();

        assert_eq!(current_branch(work.path()).unwrap(), "main");
        assert!(!working_tree_dirty(work.path()).unwrap());

        let stash_list = Command::new("git")
            .current_dir(work.path())
            .args(["stash", "list"])
            .output()
            .unwrap();
        assert!(!stash_list.stdout.is_empty(), "expected a stash entry");
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
