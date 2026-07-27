use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use log::info;

use super::AgentRunner;
use crate::git::print_git_status;

/// Maximum time to let a single `claude` invocation run before killing it.
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Polling interval used while waiting for the child process to finish.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Runs a task's prompt through the `claude` CLI in `--print` mode against
/// the project checkout.
pub struct ClaudeRunner;

impl AgentRunner for ClaudeRunner {
    fn run(&self, project_dir: &Path, prompt: &str) -> anyhow::Result<()> {
        info!("Starting Claude session in {}", project_dir.display());

        let mut command = Command::new("claude");
        command
            .current_dir(project_dir)
            .arg("--print")
            .arg("--dangerously-skip-permissions")
            .arg(prompt);

        let status = run_with_timeout(&mut command, TIMEOUT)
            .context("launching `claude` — is the Claude Code CLI installed and on PATH?")?;

        if !status.success() {
            bail!("claude exited with status {status}");
        }

        print_git_status(project_dir);

        Ok(())
    }
}

/// Spawns `command` and waits for it to finish, killing it and returning an
/// error if it is still running after `timeout`.
fn run_with_timeout(command: &mut Command, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let mut child = command.spawn()?;
    let start = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }

        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("claude did not finish within {}s and was killed", timeout.as_secs());
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_timeout_returns_status_when_process_finishes_in_time() {
        let mut command = Command::new("true");
        let status = run_with_timeout(&mut command, Duration::from_secs(5)).unwrap();
        assert!(status.success());
    }

    #[test]
    fn run_with_timeout_kills_process_that_runs_too_long() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let err = run_with_timeout(&mut command, Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("did not finish within"));
    }
}
