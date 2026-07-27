use super::ical::Task;
use super::{CompletionMetadata, TaskSource};

/// A no-op task source, mainly useful for testing/dry-running auto_worker
/// without wiring up a real supplier: it always hands back a single task
/// asking Claude to make no changes.
pub struct DummyTaskSource;

impl TaskSource for DummyTaskSource {
    fn get_next_task(&self) -> anyhow::Result<Option<Task>> {
        Ok(Some(Task {
            uid: "dummy".to_string(),
            summary: "Make no changes, say OK.".to_string(),
            status: None,
            priority: None,
            due: None,
            description: None,
            href: String::new(),
        }))
    }

    fn mark_completed(&self, _task: &Task, _metadata: &CompletionMetadata) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_next_task_returns_the_dummy_task() {
        let task = DummyTaskSource.get_next_task().unwrap().unwrap();
        assert_eq!(task.summary, "Make no changes, say OK.");
    }

    #[test]
    fn mark_completed_is_a_no_op() {
        let task = DummyTaskSource.get_next_task().unwrap().unwrap();
        assert!(DummyTaskSource
            .mark_completed(&task, &CompletionMetadata::default())
            .is_ok());
    }
}
