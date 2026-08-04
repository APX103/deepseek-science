//! dss-verify: reviewer 终审（terminal barrier）。
//!
//! modules.md §9「简化为直接 LLM 调用，不 spawn 子 frame」。
//! terminal_barrier：自然完成时单次 LLM review；有 actionable findings → veto（强制再修一轮）。
//! maybe_checkpoint：阈值触发（P6b 最小：暂只实现 terminal barrier；checkpoint 阈值留后续）。

use dss_llm::{ChatMessage, ChatRequest, LlmClient};
use serde::Deserialize;

const REVIEW_SYSTEM: &str = "你是一个严格的 reviewer。审查 agent 的最终输出质量。\
    检查：1) 是否完整回答了用户请求；2) 是否有明显错误/遗漏；3) 格式/可读性。\
    只输出 JSON：\n\
    {\"verdict\":\"pass|warn|fail\",\"findings\":[\"问题1\",\"问题2\"]}\n\
    verdict: pass=质量可接受；warn=有小问题但不阻塞；fail=有明显错误/遗漏需修复。\
    findings 只在 warn/fail 时填（可操作的具体问题）。无 JSON 外多余文字。";

/// 审查裁决。
#[derive(Debug, Clone)]
pub struct Verdict {
    pub pass: bool, // true = pass/warn（可接受）；false = fail（veto）
    pub findings: Vec<String>,
}

/// 终审：对 agent 的最终输出做一次 LLM review。
/// 返回 None 表示跳过（如 LLM 不可用或 review 失败——不阻塞完成）。
pub async fn terminal_barrier(
    llm: &dyn LlmClient,
    model: &str,
    user_prompt: &str,
    final_text: &str,
) -> Option<Verdict> {
    if final_text.trim().is_empty() {
        return None; // 空输出不 review（empty-retry 门已处理）。
    }

    let review_prompt = format!(
        "用户请求：{user_prompt}\n\nAgent 的最终输出：\n{final_text}\n\n请审查并给出裁决（JSON）："
    );
    let req = ChatRequest::new(
        model,
        vec![
            ChatMessage::system(REVIEW_SYSTEM),
            ChatMessage::user(&review_prompt),
        ],
    );

    let resp = llm.chat(req).await.ok()?;
    parse_verdict(&resp.text)
}

/// 解析 `{"verdict":"...","findings":[...]}`。容错：解析失败 → None（不阻塞）。
fn parse_verdict(text: &str) -> Option<Verdict> {
    // 找 JSON 对象（包容前后噪声）。
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let json_str = &text[start..=end];
    #[derive(Deserialize)]
    struct Raw {
        verdict: String,
        #[serde(default)]
        findings: Vec<String>,
    }
    let raw: Raw = serde_json::from_str(json_str).ok()?;
    let pass = matches!(raw.verdict.as_str(), "pass" | "warn");
    let findings = if raw.verdict == "pass" {
        Vec::new()
    } else {
        raw.findings
    };
    Some(Verdict { pass, findings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass() {
        let v = parse_verdict(r#"{"verdict":"pass","findings":[]}"#).unwrap();
        assert!(v.pass);
        assert!(v.findings.is_empty());
    }

    #[test]
    fn parses_fail_with_findings() {
        let v = parse_verdict(r#"结果：{"verdict":"fail","findings":["缺少引用","格式混乱"]} 好"#)
            .unwrap();
        assert!(!v.pass);
        assert_eq!(v.findings.len(), 2);
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(parse_verdict("no json here").is_none());
    }
}
