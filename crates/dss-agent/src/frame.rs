use serde::Serialize;

/// Latest Run status projected onto an in-memory Frame while the runner is being migrated to the
/// durable Frame/Run/Attempt model. It is not the Frame's durable lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameStatus {
    Processing,
    Completed,
    Failed,
    Success,
    Replaced,
    Cancelled,
    AwaitingPlanApproval,
    /// The plan is durably approved but execution has not yet been accepted.
    AwaitingPlanExecution,
    AwaitingUserResponse,
    /// A possibly-mutating tool has an unknown external outcome. Only explicit reconciliation
    /// may reopen the same Run.
    NeedsReconciliation,
}

impl FrameStatus {
    /// Terminal for one Run. A later Run may reuse the same Frame identity.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            FrameStatus::Completed
                | FrameStatus::Failed
                | FrameStatus::Success
                | FrameStatus::Replaced
                | FrameStatus::Cancelled
                | FrameStatus::NeedsReconciliation
        )
    }
}

/// Durable agent conversation identity. `id`, parentage, and root never change between Runs.
#[derive(Debug)]
pub struct Frame {
    pub id: String,
    pub parent_frame_id: Option<String>,
    pub root_frame_id: Option<String>,
    pub agent_name: String,
    pub status: FrameStatus,
    pub task_summary: String,
}

impl Frame {
    pub fn new_root(task_summary: impl Into<String>) -> Self {
        Self::new_root_with_id(uuid::Uuid::new_v4().to_string(), task_summary)
    }

    pub fn new_root_with_id(id: impl Into<String>, task_summary: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            root_frame_id: Some(id.clone()),
            id,
            parent_frame_id: None,
            agent_name: "MAIN".to_string(),
            status: FrameStatus::Processing,
            task_summary: task_summary.into(),
        }
    }

    /// Update the projected status of the current Run. Terminal states remain sticky within that
    /// Run; `start_run` is the explicit boundary that opens the next Run on the same Frame.
    pub fn set_status(&mut self, status: FrameStatus) {
        if self.status.is_terminal() {
            tracing::warn!(
                frame_id = %self.id,
                from = ?self.status,
                to = ?status,
                "refusing to move terminal frame status"
            );
            return;
        }
        self.status = status;
    }

    /// Start a new logical Run without changing the durable Frame identity or topology.
    pub fn start_run(&mut self, task_summary: impl Into<String>) {
        self.task_summary = task_summary.into();
        self.status = FrameStatus::Processing;
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameStatus};

    #[test]
    fn start_run_reuses_the_same_frame_identity() {
        let mut frame = Frame::new_root("first");
        let root = frame.root_frame_id.clone();
        let old_id = frame.id.clone();
        frame.set_status(FrameStatus::Completed);
        frame.start_run("second");
        assert_eq!(frame.status, FrameStatus::Processing);
        assert_eq!(frame.id, old_id);
        assert_eq!(frame.parent_frame_id, None);
        assert_eq!(frame.root_frame_id, root);
        assert_eq!(frame.task_summary, "second");
    }
}
