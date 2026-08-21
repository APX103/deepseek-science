use std::path::PathBuf;

use dss_compact::CompactionState;
use dss_llm::ChatMessage;
use dss_tools::PlanState;

use crate::frame::Frame;

/// 会话（P1 内存态；P3 落库恢复）。
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub frame: Frame,
    /// 对话历史（OpenAI 协议消息，随每轮 append-only 追加）。
    pub messages: Vec<ChatMessage>,
    /// Rolling Compact 视图态（P4a）。日志不被 mutate；projection 据此折叠。
    pub compaction: CompactionState,
    /// 决策门计数态（P2b-gates）。
    pub gate_state: GateState,
    /// plan 模式状态（P6）；跨 run 持久化到 sessions.plan_data。
    pub plan: Option<PlanState>,
}

/// Runner 决策门的跨轮计数（modules.md §4；阈值见 runner）。
#[derive(Debug, Clone, Default)]
pub struct GateState {
    /// 空响应（无 tool_use 且非 length 且内容空）连续重试次数。
    pub empty_retry_count: u32,
    /// 连续纯检索轮数（只调检索类工具）。
    pub retrieval_streak: u32,
    /// finish_reason=length 累计次数（max_tokens 续传门三档）。
    pub length_finish_count: u32,
    /// plan_mode 下未生成 plan 的连续重试次数（plan denial 门）。
    pub plan_denial_count: u32,
    /// terminal barrier veto 次数（P6b verify；上限 VETO_CAP=1）。
    pub veto_count: u32,
}

impl Session {
    pub fn new(id: impl Into<String>, workspace: PathBuf) -> Self {
        let id = id.into();
        Self {
            frame: Frame::new_root_with_id(id.clone(), ""),
            id,
            workspace,
            messages: Vec::new(),
            compaction: CompactionState::new(),
            gate_state: GateState::default(),
            plan: None,
        }
    }
}
