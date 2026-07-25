pub mod caldav;
pub mod dummy;
pub mod github;
pub mod ical;
pub mod jira;

use ical::Task;

/// A supplier of tasks for auto_worker to work through. Each implementer
/// owns its own configuration and knows how to pick the next task to work
/// on and how to report completion back to the supplier.
pub trait TaskSource {
    /// Return the next task to work on (implementers decide ordering, e.g.
    /// by priority), or `None` if there's nothing to do.
    fn get_next_task(&self) -> anyhow::Result<Option<Task>>;

    /// Report `task` as completed back to the supplier.
    fn mark_completed(&self, task: &Task) -> anyhow::Result<()>;
}
