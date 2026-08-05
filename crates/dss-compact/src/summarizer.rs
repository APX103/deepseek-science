//! summarizer 门控（modules.md §8）：
//! 目标长度 = chunk_tokens / COMPRESSION_GATE_DIVISOR；≤ SUMMARIZER_RETRY_CAP 次重试；
//! 退化检测：final < best*SUMMARIZER_DEGENERATION_RATIO 且过短 → 回退到 best。
//!
//! 用传入的 `&dyn LlmClient` 做一次非流式 summary 调用。

use crate::constants::{
    COMPRESSION_GATE_DIVISOR, OUTPUT_CEILING, SUMMARIZER_DEGENERATION_RATIO, SUMMARIZER_RETRY_CAP,
};
use dss_llm::{ChatMessage, ChatRequest, LlmClient, LlmError};

const SUMMARY_SYSTEM: &str = "你是一个上下文压缩助手。把下面这段对话历史压缩成一段紧凑的摘要，\
     保留：用户意图、已做的关键决策、工具调用结果要点、未解决的问题、重要文件/数据引用。\
     不要编造，不要加新信息。直接输出摘要正文，不要解释。";

/// summarize 一段 chunk（消息切片）。返回 summary 文本。
///
/// 门控：最多重试 SUMMARIZER_RETRY_CAP 次；保留最长（best）的一次；若最终 final 明显退化
/// （< best * ratio）则用 best。
pub async fn summarize_chunk(
    llm: &dyn LlmClient,
    model: &str,
    chunk: &[ChatMessage],
) -> Result<String, LlmError> {
    // 把 chunk 拼成一段文本（role: content 简化）。
    let body: String = chunk
        .iter()
        .map(|m| {
            let role = m.role.as_str();
            let content = m.content.clone().unwrap_or_default();
            format!("[{role}] {content}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let chunk_tokens = body.chars().count() / 4;
    let target_chars = (chunk_tokens / COMPRESSION_GATE_DIVISOR) * 4;
    let target_chars = target_chars.min(OUTPUT_CEILING);

    let mut best: Option<String> = None;
    for _attempt in 0..SUMMARIZER_RETRY_CAP {
        let prompt = format!(
            "目标长度约 {target_chars} 字符。\n\n=== 待压缩历史 ===\n{body}\n=== 结束 ===\n\n请输出摘要："
        );
        let req = ChatRequest::new(
            model,
            vec![
                ChatMessage::system(SUMMARY_SYSTEM),
                ChatMessage::user(&prompt),
            ],
        );
        let resp = llm.chat(req).await?;
        // OUTPUT_CEILING is a local invariant, not merely a prompt suggestion: providers can
        // ignore requested lengths, and an oversized summary would defeat hard-wall compaction.
        let candidate: String = resp.text.chars().take(OUTPUT_CEILING).collect();
        let candidate_chars = candidate.chars().count();
        // 更新 best（取较长者，作为退化回退点）。
        match &best {
            Some(b) if candidate_chars <= b.chars().count() => {}
            _ => best = Some(candidate.clone()),
        }
        // 达到目标长度（±宽松）即接受。
        if candidate_chars >= target_chars / 2 {
            return Ok(candidate);
        }
    }
    // 用 best（或最后一次）；若 best 明显退化则回退。
    Ok(best.unwrap_or_default())
}

/// 退化检测：final 过短则视为退化（供调用方决策）。
#[allow(dead_code)]
pub fn is_degenerate(final_text: &str, best: &str) -> bool {
    let f = final_text.chars().count();
    let b = best.chars().count();
    let threshold = ((b as f64) * SUMMARIZER_DEGENERATION_RATIO) as usize;
    b > 0 && f < threshold
}
