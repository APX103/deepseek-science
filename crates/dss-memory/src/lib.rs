//! dss-memory: 三层记忆（profile/project）+ BM25 recall（k1=1.5 CJK）+ LLM extract。
//!
//! P4b：profile/project 两层；frame scope 留后续。

pub mod bm25;
pub mod extract;
pub mod recall;
pub mod store;

pub use recall::{recall, render_recall_block};
pub use store::MemoryStore;
