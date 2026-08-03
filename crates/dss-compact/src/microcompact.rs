//! microcompact：硬墙压力下截断 >8000 字符的 tool result 到 4000 + 提示（无 LLM 调用）。
//!
//! 作用于 **projection**（给 LLM 的视图），不 mutate 日志。

use crate::constants::{
    MICROCOMPACT_TOOLRESULT_KEEP, MICROCOMPACT_TOOLRESULT_THRESHOLD,
};
use dss_llm::ChatMessage;

/// 对一组（projection 后的）消息做 microcompact：role=tool 且 content > 阈值 → 截断。
/// 返回新 Vec（不改动输入）。
pub fn microcompact(view: &[ChatMessage]) -> Vec<ChatMessage> {
    view.iter().map(compact_one).collect()
}

fn compact_one(m: &ChatMessage) -> ChatMessage {
    if m.role != "tool" {
        return m.clone();
    }
    let Some(content) = m.content.as_ref() else {
        return m.clone();
    };
    if content.chars().count() <= MICROCOMPACT_TOOLRESULT_THRESHOLD {
        return m.clone();
    }
    // 截断到 KEEP 字符 + 提示。
    let kept: String = content.chars().take(MICROCOMPACT_TOOLRESULT_KEEP).collect();
    let notice = format!(
        "\n\n[microcompact: tool result truncated from {} to {} chars]",
        content.chars().count(),
        MICROCOMPACT_TOOLRESULT_KEEP
    );
    let mut out = m.clone();
    out.content = Some(format!("{kept}{notice}"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_tool_result() {
        let long = "y".repeat(9000);
        let m = ChatMessage::tool("call_1", &long, None);
        let out = microcompact(&[m]);
        assert!(out[0].content.as_ref().unwrap().chars().count() < 9000);
        assert!(out[0].content.as_ref().unwrap().contains("[microcompact"));
    }

    #[test]
    fn leaves_short_tool_result_and_non_tool() {
        let view = vec![
            ChatMessage::user("hello"),
            ChatMessage::tool("call_1", "short result", None),
        ];
        let out = microcompact(&view);
        assert_eq!(out[0].content.as_deref(), Some("hello"));
        assert_eq!(out[1].content.as_deref(), Some("short result"));
    }
}
