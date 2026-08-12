//! microcompact：硬墙压力下压缩大型工具结果，以及已经成功落盘的写/改参数（无 LLM 调用）。
//!
//! 作用于 **projection**（给 LLM 的视图），不 mutate 日志。

use crate::constants::{MICROCOMPACT_TOOLRESULT_KEEP, MICROCOMPACT_TOOLRESULT_THRESHOLD};
use dss_llm::ChatMessage;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const A2A_RESULT_SCHEMA: &str = "dss.a2a.tool-result.v1";
const A2A_LLM_VIEW_SCHEMA: &str = "dss.a2a.llm-view.v1";
const A2A_MAX_SEMANTIC_ITEMS: usize = 128;
const A2A_MAX_VISITED_NODES: usize = 4_096;
const A2A_MAX_TRAVERSAL_DEPTH: usize = 16;
const A2A_MAX_TEXT_CHARS: usize = 2_400;
const A2A_MAX_DATA_CHARS: usize = 1_600;

/// 对一组（projection 后的）消息做 microcompact：role=tool 且 content > 阈值 → 截断。
/// 返回新 Vec（不改动输入）。
pub fn microcompact(view: &[ChatMessage]) -> Vec<ChatMessage> {
    let successful_calls: HashSet<String> = view
        .iter()
        .filter(|message| message.role == "tool" && !message.is_error.unwrap_or(false))
        .filter_map(|message| message.tool_call_id.clone())
        .collect();
    view.iter()
        .map(|message| compact_one(message, &successful_calls))
        .collect()
}

fn compact_one(m: &ChatMessage, successful_calls: &HashSet<String>) -> ChatMessage {
    if m.role == "assistant" {
        return compact_successful_file_mutations(m, successful_calls);
    }
    if m.role != "tool" {
        return m.clone();
    }
    let Some(content) = m.content.as_ref() else {
        return m.clone();
    };
    if content.chars().count() <= MICROCOMPACT_TOOLRESULT_THRESHOLD {
        return m.clone();
    }
    if let Some(compacted) = compact_a2a_tool_result(content) {
        let mut out = m.clone();
        out.content = Some(compacted);
        return out;
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

/// A2A results are canonical audit envelopes, not ordinary command stdout. Cutting one at an
/// arbitrary character would produce invalid JSON and often discard the final remote answer
/// because the refreshed Agent Card appears first. Build a bounded, valid projection for the
/// next LLM request instead. The input message is never mutated, so persistence and the UI keep
/// every complete wire response.
fn compact_a2a_tool_result(content: &str) -> Option<String> {
    let source: Value = serde_json::from_str(content).ok()?;
    if source.get("schema").and_then(Value::as_str) != Some(A2A_RESULT_SCHEMA) {
        return None;
    }
    let frames = source.get("responses")?.as_array()?;

    let response_metadata = frames
        .iter()
        .map(|frame| {
            // A compact row still proves that every accepted response existed and preserves its
            // wire order. Verbose timestamps/request ids remain in the canonical envelope.
            json!([
                frame.get("sequence"),
                frame.get("operation"),
                frame.get("http_status"),
                frame.get("protocol_version"),
                frame.get("binding"),
                frame.get("wire_bytes"),
            ])
        })
        .collect::<Vec<_>>();

    // Later polling frames normally contain the terminal report. Traverse them first so a large
    // early progress update cannot crowd the actual answer out of the bounded LLM view.
    let mut semantic = Vec::<Value>::new();
    let mut semantic_index = HashMap::<String, usize>::new();
    let mut visited_nodes = 0_usize;
    for frame in frames.iter().rev() {
        if semantic.len() >= A2A_MAX_SEMANTIC_ITEMS || visited_nodes >= A2A_MAX_VISITED_NODES {
            break;
        }
        let sequence = frame
            .get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if let Some(payload) = frame.get("payload") {
            collect_a2a_semantic_values(
                payload,
                "",
                sequence,
                0,
                &mut visited_nodes,
                &mut semantic,
                &mut semantic_index,
            );
        }
    }

    let card = source
        .get("card")
        .filter(|value| !value.is_null())
        .map(|card| {
            json!({
                "card_url": card.get("card_url"),
                "fetched_at": card.get("fetched_at"),
                "sha256": card.get("sha256"),
                "refresh_kind": card.get("refresh_kind"),
                "summary": card.get("summary"),
                "selected_interface": card.get("selected_interface"),
            })
        });
    let mut projected = json!({
        "schema": A2A_LLM_VIEW_SCHEMA,
        "source_schema": A2A_RESULT_SCHEMA,
        "projection_note": "LLM-only compact view; the canonical session record retains the complete Agent Card and every accepted wire response.",
        "agent": source.get("agent"),
        "registry": source.get("registry"),
        "card": card,
        "request": source.get("request"),
        "response_columns": ["sequence", "operation", "http_status", "protocol_version", "binding", "wire_bytes"],
        "responses": response_metadata,
        "remote_content": semantic,
        "terminal": source.get("terminal"),
        "warnings": source.get("warnings"),
    });

    // Escaping can make serialized JSON larger than the source strings. Remove the lowest
    // priority semantic items (earlier frames were appended later) until the projection fits,
    // while retaining one metadata row for every response and the terminal outcome.
    let target = MICROCOMPACT_TOOLRESULT_THRESHOLD.saturating_sub(256);
    loop {
        let serialized = serde_json::to_string(&projected).ok()?;
        if serialized.chars().count() <= target {
            return Some(serialized);
        }
        let items = projected.get_mut("remote_content")?.as_array_mut()?;
        if items.pop().is_none() {
            return Some(serialized);
        }
        projected["projection_note"] = Value::String(
            "LLM-only compact view; canonical session data retains all responses. Some remote content was omitted from this bounded projection.".into(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_a2a_semantic_values(
    value: &Value,
    path: &str,
    sequence: u64,
    depth: usize,
    visited_nodes: &mut usize,
    output: &mut Vec<Value>,
    index: &mut HashMap<String, usize>,
) {
    if depth > A2A_MAX_TRAVERSAL_DEPTH
        || *visited_nodes >= A2A_MAX_VISITED_NODES
        || output.len() >= A2A_MAX_SEMANTIC_ITEMS
    {
        return;
    }
    *visited_nodes += 1;
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}/{}", json_pointer_escape(key));
                let normalized = key.to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "text" | "url" | "uri" | "raw" | "bytes"
                ) {
                    if let Some(text) = child.as_str() {
                        push_a2a_semantic(
                            &normalized,
                            &child_path,
                            truncated_text(text, A2A_MAX_TEXT_CHARS),
                            sequence,
                            output,
                            index,
                        );
                        continue;
                    }
                }
                if normalized == "data" {
                    let data = serde_json::to_string(child).unwrap_or_else(|_| child.to_string());
                    push_a2a_semantic(
                        "data",
                        &child_path,
                        truncated_text(&data, A2A_MAX_DATA_CHARS),
                        sequence,
                        output,
                        index,
                    );
                    continue;
                }
                if matches!(normalized.as_str(), "state" | "error" | "description") {
                    if let Some(text) = child.as_str() {
                        push_a2a_semantic(
                            &normalized,
                            &child_path,
                            truncated_text(text, 600),
                            sequence,
                            output,
                            index,
                        );
                        continue;
                    }
                }
                collect_a2a_semantic_values(
                    child,
                    &child_path,
                    sequence,
                    depth + 1,
                    visited_nodes,
                    output,
                    index,
                );
            }
        }
        Value::Array(values) => {
            for (position, child) in values.iter().enumerate() {
                collect_a2a_semantic_values(
                    child,
                    &format!("{path}/{position}"),
                    sequence,
                    depth + 1,
                    visited_nodes,
                    output,
                    index,
                );
            }
        }
        _ => {}
    }
}

fn push_a2a_semantic(
    kind: &str,
    path: &str,
    value: String,
    sequence: u64,
    output: &mut Vec<Value>,
    index: &mut HashMap<String, usize>,
) {
    let dedupe_key = format!("{kind}\u{0}{value}");
    if let Some(position) = index.get(&dedupe_key).copied() {
        if let Some(sequences) = output[position]
            .get_mut("response_sequences")
            .and_then(Value::as_array_mut)
        {
            if !sequences.iter().any(|item| item.as_u64() == Some(sequence)) {
                sequences.push(Value::from(sequence));
            }
        }
        return;
    }
    if output.len() >= A2A_MAX_SEMANTIC_ITEMS {
        return;
    }
    index.insert(dedupe_key, output.len());
    output.push(json!({
        "kind": kind,
        "path": path,
        "value": value,
        "response_sequences": [sequence],
    }));
}

fn truncated_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    let kept = value.chars().take(max_chars).collect::<String>();
    format!("{kept}\n[projection truncated: {count} chars total]")
}

fn json_pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// Once a write/edit result succeeded, replaying an entire multi-kilobyte script in every later
/// model request is redundant: the canonical history still has it and the model can read the
/// current file. Failed mutations retain exact arguments because they may be needed to diagnose
/// and retry safely.
fn compact_successful_file_mutations(
    message: &ChatMessage,
    successful_calls: &HashSet<String>,
) -> ChatMessage {
    let Some(calls) = message.tool_calls.as_ref() else {
        return message.clone();
    };
    let mut out = message.clone();
    let Some(out_calls) = out.tool_calls.as_mut() else {
        return out;
    };

    for (original, projected) in calls.iter().zip(out_calls.iter_mut()) {
        if !successful_calls.contains(&original.id)
            || !matches!(original.function.name.as_str(), "write_file" | "edit_file")
            || original.function.arguments.chars().count() <= MICROCOMPACT_TOOLRESULT_THRESHOLD
        {
            continue;
        }
        let Ok(mut args) = serde_json::from_str::<serde_json::Value>(&original.function.arguments)
        else {
            continue;
        };
        let Some(object) = args.as_object_mut() else {
            continue;
        };
        let path = object
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the written file")
            .to_string();
        let mut compacted_any = false;
        for field in ["content", "old_string", "new_string"] {
            let Some(value) = object.get_mut(field) else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            let chars = text.chars().count();
            if chars <= MICROCOMPACT_TOOLRESULT_KEEP {
                continue;
            }
            *value = serde_json::Value::String(format!(
                "[microcompact: {field} omitted after successful {}; {chars} chars; use read_file on {path} for current content]",
                original.function.name
            ));
            compacted_any = true;
        }
        if compacted_any {
            if let Ok(arguments) = serde_json::to_string(&args) {
                projected.function.arguments = arguments;
            }
        }
    }
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

    #[test]
    fn compacts_large_a2a_result_to_valid_semantic_json_without_touching_canonical_input() {
        let repeated = "## Remote conclusion 🧪\n\n**validated**\n".repeat(180);
        let canonical = json!({
            "schema": A2A_RESULT_SCHEMA,
            "agent": {"config_id":"nuclear", "display_name":"Nuclear specialist"},
            "registry": {
                "server": "agent-registry",
                "resource_uri": "agent://nuclear",
                "resource_name": "Nuclear specialist"
            },
            "card": {
                "card_url":"http://127.0.0.1/.well-known/agent-card.json",
                "summary":{"name":"Specialist", "description":"science"},
                "selected_interface":{"url":"http://127.0.0.1/a2a"},
                "raw":{"large_untrusted_card":"x".repeat(5000)}
            },
            "request":{"task":"Review fast-reactor evidence"},
            "responses":[
                {"sequence":1,"operation":"SendMessage","received_at":"now","http_status":200,"wire_bytes":9000,
                 "payload":{"result":{"task":{"status":{"state":"working","message":{"parts":[{"text":"checkpoint"}]}}}}}},
                {"sequence":2,"operation":"GetTask","received_at":"later","http_status":200,"wire_bytes":9000,
                 "payload":{"result":{"task":{"status":{"state":"completed","message":{"parts":[{"text":repeated}]}}}}}}
            ],
            "terminal":{"kind":"task","state":"completed","success":true},
            "warnings":[]
        })
        .to_string();
        assert!(canonical.chars().count() > MICROCOMPACT_TOOLRESULT_THRESHOLD);
        let message = ChatMessage::tool("a2a-call", &canonical, Some("a2a_agent_nuclear".into()));

        let output = microcompact(std::slice::from_ref(&message));
        let projected_text = output[0].content.as_deref().unwrap();
        let projected: Value = serde_json::from_str(projected_text).expect("valid compact JSON");
        assert_eq!(projected["schema"], A2A_LLM_VIEW_SCHEMA);
        assert_eq!(projected["source_schema"], A2A_RESULT_SCHEMA);
        assert_eq!(projected["responses"].as_array().unwrap().len(), 2);
        assert_eq!(projected["terminal"]["state"], "completed");
        assert_eq!(projected["registry"]["server"], "agent-registry");
        assert_eq!(projected["registry"]["resource_uri"], "agent://nuclear");
        assert!(projected_text.contains("Remote conclusion"));
        assert!(!projected_text.contains("large_untrusted_card"));
        assert!(projected_text.chars().count() < MICROCOMPACT_TOOLRESULT_THRESHOLD);
        assert_eq!(message.content.as_deref(), Some(canonical.as_str()));
    }

    #[test]
    fn a2a_projection_represents_every_response_even_when_semantic_content_is_huge() {
        let responses = (1..=40)
            .map(|sequence| {
                json!({
                    "sequence": sequence,
                    "operation": "GetTask",
                    "received_at": "now",
                    "http_status": 200,
                    "wire_bytes": 10_000,
                    "payload": {"result":{"message":{"parts":[{"text":"z".repeat(3000)}]}}}
                })
            })
            .collect::<Vec<_>>();
        let canonical = json!({
            "schema": A2A_RESULT_SCHEMA,
            "agent": {"display_name":"Many frames"},
            "card": null,
            "request": {"task":"bounded"},
            "responses": responses,
            "terminal":{"kind":"task","state":"completed","success":true}
        })
        .to_string();
        let output = microcompact(&[ChatMessage::tool("a2a", &canonical, None)]);
        let projected: Value = serde_json::from_str(output[0].content.as_deref().unwrap()).unwrap();
        assert_eq!(projected["responses"].as_array().unwrap().len(), 40);
        assert_eq!(projected["terminal"]["success"], true);
    }

    #[test]
    fn compacts_large_successful_write_arguments_but_keeps_canonical_input_unchanged() {
        let original_content = "print('science')\n".repeat(600);
        let call = dss_llm::ToolCall::function(
            "write-1",
            "write_file",
            serde_json::json!({"path":"analysis.py","content":original_content}).to_string(),
        );
        let assistant = ChatMessage::assistant_tool_calls(vec![call]);
        let mut result = ChatMessage::tool(
            "write-1",
            "wrote analysis.py",
            Some("write_file".to_string()),
        );
        result.is_error = Some(false);

        let out = microcompact(&[assistant.clone(), result]);
        let projected_args = &out[0].tool_calls.as_ref().unwrap()[0].function.arguments;
        assert!(projected_args.contains("analysis.py"));
        assert!(projected_args.contains("omitted after successful write_file"));
        assert!(projected_args.len() < original_content.len());
        assert!(assistant.tool_calls.as_ref().unwrap()[0]
            .function
            .arguments
            .contains("print('science')"));
    }

    #[test]
    fn failed_write_keeps_exact_arguments_for_diagnosis() {
        let original_content = "x".repeat(9000);
        let call = dss_llm::ToolCall::function(
            "write-failed",
            "write_file",
            serde_json::json!({"path":"analysis.py","content":original_content}).to_string(),
        );
        let assistant = ChatMessage::assistant_tool_calls(vec![call]);
        let mut result = ChatMessage::tool(
            "write-failed",
            "write failed",
            Some("write_file".to_string()),
        );
        result.is_error = Some(true);

        let original_args = assistant.tool_calls.as_ref().unwrap()[0]
            .function
            .arguments
            .clone();
        let out = microcompact(&[assistant, result]);
        assert_eq!(
            out[0].tool_calls.as_ref().unwrap()[0].function.arguments,
            original_args
        );
    }
}
