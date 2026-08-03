//! extract：每轮末单次 LLM 调用，解析 `emit_memories({append,replace,remove})`，≤5 条/轮。
//!
//! 跳过 harness_notice 与 tool_result 消息（不在抽取输入里）。

use dss_llm::{ChatMessage, ChatRequest, LlmClient, LlmError};
use serde::Deserialize;

const EXTRACT_SYSTEM: &str = "你是记忆抽取助手。从下面的对话中，抽取**值得长期记住的事实/偏好/决策**\
    （用户身份、研究领域偏好、技术栈、重要决策、明确的事实陈述）。\
    不要抽取临时任务细节、工具输出、客套话。\
    只输出一个 JSON 对象，格式严格如下，不要任何解释或多余文字：\n\
    emit_memories({\"append\":[\"记忆1\",\"记忆2\",...],\"replace\":[],\"remove\":[]})\n\
    append 里每条 ≤1000 字符、用陈述句。最多 5 条。若无值得记住的，输出 emit_memories({\"append\":[],\"replace\":[],\"remove\":[]})";

/// 抽取出的记忆操作（P4b 只实现 append；replace/remove 留后续）。
#[derive(Debug, Default, Deserialize)]
pub struct MemOps {
    pub append: Vec<String>,
    #[serde(default)]
    pub _replace: Vec<String>,
    #[serde(default)]
    pub _remove: Vec<String>,
}

/// 从消息历史抽取记忆。返回 append 列表。
pub async fn extract(
    llm: &dyn LlmClient,
    model: &str,
    messages: &[ChatMessage],
) -> Result<Vec<String>, LlmError> {
    // 跳过 harness_notice（P4b 暂无显式标记，跳过 role=tool 的工具结果与空内容）。
    let body: String = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter(|m| m.content.as_ref().map(|c| !c.is_empty()).unwrap_or(false))
        .map(|m| {
            let role = m.role.as_str();
            let content = m.content.clone().unwrap_or_default();
            format!("[{role}] {content}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    if body.trim().is_empty() {
        return Ok(Vec::new());
    }

    let req = ChatRequest::new(
        model,
        vec![
            ChatMessage::system(EXTRACT_SYSTEM),
            ChatMessage::user(format!("=== 对话 ===\n{body}\n=== 结束 ===\n请抽取记忆：")),
        ],
    );
    let resp = llm.chat(req).await?;
    Ok(parse_emit_memories(&resp.text))
}

/// 解析 `emit_memories({...})` 里的 append 列表。容错：找不到则空。
pub fn parse_emit_memories(text: &str) -> Vec<String> {
    // 找 `emit_memories(` 后到匹配的 `)`。
    let key = "emit_memories(";
    let Some(start) = text.find(key) else {
        return Vec::new();
    };
    let json_start = start + key.len();
    // 找最后一个 `}`（粗暴但容错：取 start 之后第一个 `{` 到其后最后一个 `}`）。
    let rest = &text[json_start..];
    let Some(brace_start) = rest.find('{') else {
        return Vec::new();
    };
    // 从 brace_start 找匹配的闭合 }（找最后一个 } 在合理窗口内）。
    let window = &rest[brace_start..];
    let Some(brace_end_rel) = window.rfind('}') else {
        return Vec::new();
    };
    let json_str = &window[..=brace_end_rel];
    let Ok(parsed) = serde_json::from_str::<MemOps>(json_str) else {
        return Vec::new();
    };
    parsed.append.into_iter().take(5).map(|s| s.chars().take(1000).collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emit_memories_append() {
        let text = r#"emit_memories({"append":["用户研究钙钛矿太阳电池","偏好无铅材料"],"replace":[],"remove":[]})"#;
        let ops = parse_emit_memories(text);
        assert_eq!(ops.len(), 2);
        assert!(ops[0].contains("钙钛矿"));
    }

    #[test]
    fn parses_with_surrounding_text() {
        let text = "好的，我来抽取：\nemit_memories({\"append\":[\"a\"]})\n以上。";
        let ops = parse_emit_memories(text);
        assert_eq!(ops, vec!["a".to_string()]);
    }

    #[test]
    fn empty_when_no_marker() {
        assert!(parse_emit_memories("no marker here").is_empty());
    }
}
