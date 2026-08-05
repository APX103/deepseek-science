//! dss-agent: Runner 主循环 + Frame 状态机 + Session 骨架。
//!
//! P2：多轮工具循环（tool_use → 执行 → 结果入历史 → 继续）。
//! 门控（max_tokens 续传 / empty-retry / 检索熔断）/ RC 等留待 P2b/P4，详见 docs/modules.md。

mod dsml;
pub mod events;
pub mod frame;
pub mod runner;
pub mod session;

pub use events::{AgentEvent, CompleteKind};
pub use frame::{Frame, FrameStatus};
pub use runner::{RunOutcome, Runner, MAX_ITERATIONS};
pub use session::Session;
