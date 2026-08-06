//! 记忆事件类型常量（memory_events.event_type 的取值）。
//!
//! 所有写入路径（store.rs）都通过 record_event 记录这些事件，形成可审计的生命周期时间线。

/// 记忆创建。
pub const EV_CREATED: &str = "created";
/// 候选被批准为 active。
pub const EV_APPROVED: &str = "approved";
/// 候选被拒绝（软删除）。
pub const EV_REJECTED: &str = "rejected";
/// 被新 claim 替代（detail.by = 新 id）。
pub const EV_SUPERSEDED: &str = "superseded";
/// 软删除。
pub const EV_DELETED: &str = "deleted";
/// 被召回（recall 命中）。批量召回时由调用方决定是否逐条记录（高频，可采样）。
pub const EV_SURFACED: &str = "surfaced";
/// 同版本订正 body。
pub const EV_EDITED: &str = "edited";
/// 过期。
pub const EV_EXPIRED: &str = "expired";
