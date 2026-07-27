pub mod caldav;
pub mod dummy;
pub mod github;
pub mod ical;
pub mod jira;

use ical::Task;

/// Metadata gathered while completing a task, made available to
/// [`TaskSource::mark_completed`] so implementers that can record it (e.g.
/// as an issue/VTODO comment) have something to record. Fields are additive
/// and optional — a source that has no natural place to put a given piece
/// of metadata is free to ignore it.
#[derive(Debug, Clone, Default)]
pub struct CompletionMetadata {
    /// URL of the pull request opened for the task's changes, if any.
    pub pr_url: Option<String>,
}

/// A supplier of tasks for auto_worker to work through. Each implementer
/// owns its own configuration and knows how to pick the next task to work
/// on and how to report completion back to the supplier.
pub trait TaskSource {
    /// Return the next task to work on (implementers decide ordering, e.g.
    /// by priority), or `None` if there's nothing to do.
    fn get_next_task(&self) -> anyhow::Result<Option<Task>>;

    /// Report `task` as completed back to the supplier, along with whatever
    /// `metadata` was gathered while completing it.
    fn mark_completed(&self, task: &Task, metadata: &CompletionMetadata) -> anyhow::Result<()>;
}
