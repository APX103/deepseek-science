//! dss-memory: L2 Claim Store + BM25 recall（k1=1.5 CJK）+ LLM extract + 巩固流水线。
//!
//! 分层：profile（跨项目）/ project（项目内）。session/workspace scope 留后续。
//! 每条记忆是可更新的 Claim（带 status/evidence_refs/source_hash/superseded_by），
//! 非扁平文本。

pub mod bm25;
pub mod consolidate;
pub mod events;
pub mod extract;
pub mod recall;
pub mod retention;
pub mod store;
pub mod types;

pub use recall::{recall, render_recall_block};
pub use store::{evidence_refs_json, MemoryStore};
pub use types::{gen_id, memory_hash, ClaimType, EvidenceRef, MemoryStatus, Origin};
