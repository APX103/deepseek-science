//! extract：每轮末单次 LLM 调用，解析 `emit_memories({"append":[{...}]})`。
//!
//! 升级点（L2 Claim Store）：每条记忆带 claim_type + confidence，便于巩固流水线做信任分流。
//! evidence_refs 由调用方（sessions.rs）在巩固时附上（知道 session/run/seq 范围）。
//! 跳过 harness_notice 与 tool_result 消息（不在抽取输入里）。

use dss_llm::{ChatMessage, ChatRequest, LlmClient, LlmError};
use serde::Deserialize;

use crate::types::ClaimType;

const EXTRACT_SYSTEM: &str =
    "你是记忆抽取助手。从下面的对话中，抽取**值得长期记住的事实/偏好/决策**\
    （用户身份、研究领域偏好、技术栈、重要决策、明确的事实陈述、可复用的工具用法或调试经验）。\
    不要抽取临时任务细节、工具输出、客套话。\n\n\
    对每条记忆，判断其类型并给出置信度：\n\
    - fact：稳定事实（用户身份、技术栈、环境配置）\n\
    - preference：用户偏好/习惯（如喜欢深色主题、倾向某种风格）\n\
    - decision：明确决策（如选用某方案、放弃某路线）\n\
    - procedure：可复用步骤/工具用法\n\
    - repo：仓库相关（架构约定、调试经验）\n\
    - note：其他值得记的笔记\n\
    置信度 0.0-1.0：信息明确且多次出现→高；单次提及且不确定→低。\n\n\
    只输出一个 JSON 对象，格式严格如下，不要任何解释或多余文字：\n\
    emit_memories({\"append\":[{\"body\":\"陈述句\",\"type\":\"fact\",\"confidence\":0.8},...]})\n\
    body 每条 ≤1000 字符、用陈述句。最多 5 条。若无值得记住的，输出 emit_memories({\"append\":[]})";

/// 抽取出的单条记忆（带类型 + 置信度）。evidence_refs 由调用方在巩固时附上。
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedMem {
    pub body: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

impl ExtractedMem {
    /// 解析 claim_type（未提供/无效 → Note）。
    pub fn claim_type(&self) -> ClaimType {
        self.r#type
            .as_deref()
            .map(ClaimType::parse)
            .unwrap_or_default()
    }

    /// confidence 缺省 0.5，钳到 0..1。
    pub fn conf(&self) -> f64 {
        self.confidence.unwrap_or(0.5).clamp(0.0, 1.0)
    }
}

/// 旧 MemOps 兼容结构（解析顶层 JSON 用）。
#[derive(Debug, Default, Deserialize)]
pub struct MemOps {
    #[serde(default)]
    pub append: Vec<ExtractedMem>,
}

/// 从消息历史抽取记忆。返回结构化候选列表。
pub async fn extract(
    llm: &dyn LlmClient,
    model: &str,
    messages: &[ChatMessage],
) -> Result<Vec<ExtractedMem>, LlmError> {
    let body = render_extract_body(messages);

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

fn render_extract_body(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| !m.harness_notice)
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter(|m| m.content.as_ref().map(|c| !c.is_empty()).unwrap_or(false))
        .map(|m| {
            let role = m.role.as_str();
            let content = m.content.clone().unwrap_or_default();
            format!("[{role}] {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析 `emit_memories({...})` 里的 append 列表。容错：找不到则空。
pub fn parse_emit_memories(text: &str) -> Vec<ExtractedMem> {
    // 找 `emit_memories(` 后到匹配的 `)`。
    let key = "emit_memories(";
    let Some(start) = text.find(key) else {
        return Vec::new();
    };
    let json_start = start + key.len();
    let rest = &text[json_start..];
    let Some(brace_start) = rest.find('{') else {
        return Vec::new();
    };
    let window = &rest[brace_start..];
    let Some(brace_end_rel) = window.rfind('}') else {
        return Vec::new();
    };
    let json_str = &window[..=brace_end_rel];
    let Ok(parsed) = serde_json::from_str::<MemOps>(json_str) else {
        // 兼容旧格式：append 是字符串数组（而非对象数组）。
        #[derive(Deserialize)]
        struct Legacy {
            #[serde(default)]
            append: Vec<String>,
        }
        if let Ok(legacy) = serde_json::from_str::<Legacy>(json_str) {
            return legacy
                .append
                .into_iter()
                .take(5)
                .map(|s| ExtractedMem {
                    body: s.chars().take(1000).collect(),
                    r#type: None,
                    confidence: None,
                })
                .collect();
        }
        return Vec::new();
    };
    parsed
        .append
        .into_iter()
        .take(5)
        .map(|m| ExtractedMem {
            body: m.body.chars().take(1000).collect(),
            r#type: m.r#type,
            confidence: m.confidence,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emit_memories_typed() {
        let text = r#"emit_memories({"append":[{"body":"用户研究钙钛矿太阳电池","type":"fact","confidence":0.9},{"body":"偏好无铅材料","type":"preference","confidence":0.7}]})"#;
        let ops = parse_emit_memories(text);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].body, "用户研究钙钛矿太阳电池");
        assert_eq!(ops[0].claim_type(), ClaimType::Fact);
        assert!((ops[0].conf() - 0.9).abs() < 1e-9);
        assert_eq!(ops[1].claim_type(), ClaimType::Preference);
    }

    #[test]
    fn parses_legacy_string_append_format() {
        let text = r#"emit_memories({"append":["用户研究钙钛矿"],"replace":[],"remove":[]})"#;
        let ops = parse_emit_memories(text);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].body, "用户研究钙钛矿");
        assert_eq!(ops[0].claim_type(), ClaimType::Note);
        assert!((ops[0].conf() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn parses_with_surrounding_text() {
        let text = "好的，我来抽取：\nemit_memories({\"append\":[{\"body\":\"a\",\"type\":\"note\",\"confidence\":0.5}]})\n以上。";
        let ops = parse_emit_memories(text);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].body, "a");
    }

    #[test]
    fn empty_when_no_marker() {
        assert!(parse_emit_memories("no marker here").is_empty());
    }

    #[test]
    fn confidence_clamped_and_defaults() {
        let m = ExtractedMem {
            body: "x".into(),
            r#type: None,
            confidence: None,
        };
        assert!((m.conf() - 0.5).abs() < 1e-9);
        let m2 = ExtractedMem {
            body: "x".into(),
            r#type: None,
            confidence: Some(1.7),
        };
        assert!((m2.conf() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn extract_input_excludes_internal_rejected_drafts() {
        let mut rejected = ChatMessage::assistant("REJECTED DRAFT SECRET");
        rejected.harness_notice = true;
        let body = render_extract_body(&[
            ChatMessage::user("visible request"),
            rejected,
            ChatMessage::assistant("visible revised answer"),
        ]);

        assert!(body.contains("visible request"));
        assert!(body.contains("visible revised answer"));
        assert!(!body.contains("REJECTED DRAFT SECRET"));
    }
}
