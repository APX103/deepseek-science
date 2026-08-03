//! token 估计：`estimate_tokens(text) = len/CHARS_PER_TOKEN`（modules.md / data-model）。

use crate::constants::CHARS_PER_TOKEN;
use dss_llm::ChatMessage;

/// 字符串的 token 估计（向下取整）。
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / CHARS_PER_TOKEN
}

/// 一条消息的 token 估计（content + tool_calls 序列化文本 + role 等开销近似）。
pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    let mut chars = 0usize;
    if let Some(c) = msg.content.as_ref() {
        chars += c.chars().count();
    }
    if let Some(tcs) = msg.tool_calls.as_ref() {
        for tc in tcs.iter() {
            chars += tc.function.name.chars().count();
            chars += tc.function.arguments.chars().count();
        }
    }
    if let Some(id) = msg.tool_call_id.as_ref() {
        chars += id.chars().count();
    }
    // 每条消息固定开销近似（角色 + 结构）。
    chars += 8;
    chars / CHARS_PER_TOKEN
}

/// 多条消息的总 token 估计。
pub fn estimate_messages_tokens(msgs: &[ChatMessage]) -> usize {
    msgs.iter().map(estimate_message_tokens).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("ab"), 0); // 向下取整
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 多字节字符按 char 计数（每个 CJK 字符算 1 char）：4 字符 / 4 = 1。
        assert_eq!(estimate_tokens("你好你好"), 1);
    }

    #[test]
    fn message_tokens_includes_content_and_overhead() {
        let m = ChatMessage::user("abcdefgh"); // 8 chars + 8 overhead = 16 / 4 = 4
        assert_eq!(estimate_message_tokens(&m), 4);
    }
}
