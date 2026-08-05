//! dss-tools: 工具系统——Tool trait / ToolRegistry / ToolRouter / 内置工具。
//!
//! P2a 范围：文件工具（read/write/edit/list）、bash、ask_user。
//! 路径穿越防护、并发执行（JoinSet）、per-call 超时（30s）。

pub mod builtin;
pub mod context;
pub mod error;
mod process;
pub mod router;
mod sandbox;
pub mod spec;

pub use context::{
    HistoryCheckpoint, HistoryCheckpointState, PendingAsk, PendingAskOption, PlanState, PlanStep,
    SecureWorkspace, ToolContext, WorkspaceEntry,
};
pub use error::ToolError;
pub use router::{
    parse_arguments, PendingToolCall, ToolRegistry, ToolRegistryError, ToolResult, ToolRouter,
};
pub use spec::{Tool, ToolBatchPolicy, ToolDef, ToolOutput, ToolSpec};
