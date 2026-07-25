pub mod claude;
pub mod noop;

use std::path::Path;

/// Something that can carry out a task's prompt against a project checkout.
/// Implementers decide how (or whether) the work actually happens.
pub trait AgentRunner {
    fn run(&self, project_dir: &Path, prompt: &str) -> anyhow::Result<()>;
}
