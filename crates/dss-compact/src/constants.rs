//! Rolling Compact 常量（modules.md §8，已定型，一字不改）。
//!
//! 任何「优化」想法先登记 docs/decisions.md，积累足够回归测试后再议。

/// 1 token ≈ 4 字符（廉价估计；精确 tokenizer 留增强）。
pub const CHARS_PER_TOKEN: usize = 4;

/// kept-available floor（token）：kept-available 至少这么大，否则触发压缩。
pub const KA_FLOOR: usize = 50_000;

/// kept-available ratio：kept-available 目标 = context_window * KB_RATIO。
pub const KB_RATIO: f64 = 0.7;

/// 单个 L1 chunk 的最小 token 数。
pub const MIN_CHUNK_TOKENS: usize = 4096;

/// summary 输出上限（字符）。
pub const OUTPUT_CEILING: usize = 32_000;

/// 触发压缩的阈值比例（已用 / context_window ≥ 此值触发 L1）。
pub const COMPACTION_TRIGGER_RATIO: f64 = 0.75;

/// 硬墙比例（≥ 此值强制更激进压缩）。
pub const HARD_WALL_RATIO: f64 = 0.9;

/// microcompact 触发比例。
pub const MICROCOMPACT_RATIO: f64 = 0.65;

/// 绝对 token 上限（无论如何不超过）。
pub const ABSOLUTE_TOKEN_CEILING: usize = 300_000;

/// pick_next_chunk 失败重试上限（≥ 此值升级 L2）。
pub const PTL_RETRY_CAP: usize = 32;

/// 压缩门除数：summarizer 目标 = chunk_tokens / COMPRESSION_GATE_DIVISOR。
pub const COMPRESSION_GATE_DIVISOR: usize = 3;

/// 默认 context 上限（token）。
pub const DEFAULT_CONTEXT_CEILING: usize = 500_000;

/// 默认 kept-available ratio（kept-available 目标的另一基准）。
pub const DEFAULT_KA_RATIO: f64 = 0.2;

/// microcompact：tool result 超 this 字符数则截断。
pub const MICROCOMPACT_TOOLRESULT_THRESHOLD: usize = 8000;
/// microcompact：截断后的字符数。
pub const MICROCOMPACT_TOOLRESULT_KEEP: usize = 4000;

/// L2 触发：head tokens 下限的常数部分。
pub const L2_HEAD_TOKENS_FLOOR: usize = 8192;
/// L2 触发：head tokens 下限相对 ka 的比例。
pub const L2_HEAD_TOKENS_KA_RATIO: f64 = 0.4;
/// L2 触发：需要至少多少个 L1 summary。
pub const L2_MIN_L1_SUMMARIES: usize = 3;

/// summarizer 门控：重试上限。
pub const SUMMARIZER_RETRY_CAP: usize = 3;
/// summarizer 退化检测：final < best * DEGENERATION_RATIO 则回退。
pub const SUMMARIZER_DEGENERATION_RATIO: f64 = 0.25;
