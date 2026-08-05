use serde::Serialize;

/// Frame 状态机（全量枚举按 modules.md 定义；P1 只流转 Processing/Completed/Failed/Cancelled）。
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
}

impl FrameStatus {
    /// TERMINAL 集合（terminal 状态粘性，只能经 reopen 逃出——P1 尚无 reopen）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            FrameStatus::Completed
                | FrameStatus::Failed
                | FrameStatus::Success
                | FrameStatus::Replaced
                | FrameStatus::Cancelled
        )
    }
}

/// 主/子 frame。P1 最小字段集；token 计数、context 等随阶段补齐。
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
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            root_frame_id: Some(id.clone()),
            id,
            parent_frame_id: None,
            agent_name: "MAIN".to_string(),
            status: FrameStatus::Processing,
            task_summary: task_summary.into(),
        }
    }

    /// terminal 状态粘性守卫：已终止的 frame 不再迁移。
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

    /// Begin a new user turn. A completed/failed/cancelled or awaiting frame
    /// becomes the parent of a fresh processing frame; an already-processing
    /// frame (for example immediately after plan approval) is reused.
    pub fn begin_run(&mut self, task_summary: impl Into<String>) {
        self.task_summary = task_summary.into();
        if self.status == FrameStatus::Processing {
            return;
        }
        let previous = std::mem::replace(&mut self.id, uuid::Uuid::new_v4().to_string());
        self.parent_frame_id = Some(previous);
        self.status = FrameStatus::Processing;
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameStatus};

    #[test]
    fn begin_run_reopens_terminal_frame_as_new_child() {
        let mut frame = Frame::new_root("first");
        let root = frame.root_frame_id.clone();
        let old_id = frame.id.clone();
        frame.set_status(FrameStatus::Completed);
        frame.begin_run("second");
        assert_eq!(frame.status, FrameStatus::Processing);
        assert_ne!(frame.id, old_id);
        assert_eq!(frame.parent_frame_id.as_deref(), Some(old_id.as_str()));
        assert_eq!(frame.root_frame_id, root);
        assert_eq!(frame.task_summary, "second");
    }
}
