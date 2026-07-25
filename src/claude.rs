use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};

use crate::agent::AgentRunner;
use crate::git::print_git_status;

/// Runs a task's prompt through the `claude` CLI in `--print` mode against
/// the project checkout.
pub struct ClaudeRunner;

impl AgentRunner for ClaudeRunner {
    fn run(&self, project_dir: &Path, prompt: &str) -> anyhow::Result<()> {
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
}
