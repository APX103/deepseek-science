//! dss-db: SQLite 存储 + inline 迁移 + 仓储层。
//!
//! P3 schema 子集：projects / sessions / session_messages。
//! memories/artifacts/verification/compaction/frames/logs 留对应阶段。

pub mod error;
pub mod events;
pub mod harness;
pub mod repo;
pub mod schema;

pub use error::DbError;
/// rusqlite 连接类型别名（供外部 crate 的闭包签名使用，避免直接依赖 rusqlite）。
pub use rusqlite::Connection;
pub use schema::{open_pool, run_migrations, ConnObj, DbPool};

/// 默认项目 id。
pub const DEFAULT_PROJECT_ID: &str = "proj_default";
