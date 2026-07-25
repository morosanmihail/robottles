use std::path::Path;

use crate::agent::AgentRunner;

/// An agent runner that does nothing, mainly useful for testing/dry-running
/// auto_worker without invoking a real coding agent.
pub struct NoopRunner;

impl AgentRunner for NoopRunner {
    fn run(&self, _project_dir: &Path, _prompt: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_is_a_no_op() {
        assert!(NoopRunner.run(Path::new("/nonexistent"), "prompt").is_ok());
    }
}
