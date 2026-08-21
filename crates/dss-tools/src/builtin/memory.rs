//! memory 工具：让 agent 主动检索/读取长期记忆（search_memory / read_memory）。
//!
//! 这两个名字已在 runner.rs:184-198 的检索熔断白名单预留，注册后自动参与
//! retrieval-streak 熔断计数。search_memory 走 BM25 索引（profile + 当前 project），
//! read_memory 取单条 claim 详情 + 证据展开。

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

// ----------------- search_memory -----------------

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

fn default_top_k() -> usize {
    5
}

pub struct SearchMemoryTool;

#[async_trait]
impl Tool for SearchMemoryTool {
    fn effect_class(&self, _args: &serde_json::Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_memory".into(),
            description: "检索长期记忆（跨会话的事实/偏好/决策）。用自然语言或关键词查询，\
                          返回最相关的记忆条目（BM25 召回，含 profile 跨项目记忆 + 当前项目记忆）。\
                          当你需要回忆用户身份、技术栈、过往决策或研究偏好时使用。"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "检索查询（自然语言或关键词，中英文均可）"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "返回条数上限（默认 5，最大 20）",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let Some(store) = ctx.memory.as_ref() else {
            return Ok(ToolOutput::ok("记忆功能未启用。"));
        };
        let a: SearchArgs = parse_args(&args)?;
        let top_k = a.top_k.clamp(1, 20);
        let pid = ctx.project_id.as_deref();
        let hits = store
            .recall_indexed(&a.query, pid, top_k)
            .await
            .map_err(|e| ToolError::Other(format!("memory recall: {e}")))?;
        if hits.is_empty() {
            return Ok(ToolOutput::ok("未找到相关记忆。"));
        }
        // 批量取完整行。
        let mut out = Vec::with_capacity(hits.len());
        for (id, score) in &hits {
            if let Ok(Some(m)) = store.get(id.clone()).await {
                if m.status == "active" {
                    out.push(json!({
                        "id": m.id,
                        "scope": m.scope,
                        "type": m.claim_type,
                        "body": m.body,
                        "confidence": m.confidence,
                        "score": (score * 100.0).round() / 100.0,
                    }));
                }
            }
        }
        let body = serde_json::to_string_pretty(&json!({ "memories": out, "count": out.len() }))
            .unwrap_or_else(|_| "{}".into());
        // 打点召回时间。
        let ids: Vec<String> = hits.iter().map(|(id, _)| id.clone()).collect();
        let _ = store.touch_surfaced(ids).await;
        Ok(ToolOutput::ok(body))
    }
}

// ----------------- read_memory -----------------

#[derive(Deserialize)]
struct ReadArgs {
    id: String,
}

pub struct ReadMemoryTool;

#[async_trait]
impl Tool for ReadMemoryTool {
    fn effect_class(&self, _args: &serde_json::Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_memory".into(),
            description: "按 id 读取单条记忆的完整内容（含证据来源、置信度、生命周期时间线）。\
                          当 search_memory 返回的摘要不够、需要展开细节时使用。"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "记忆 id（mem_ 开头，来自 search_memory 结果）"
                    }
                },
                "required": ["id"]
            }),
        }
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let Some(store) = ctx.memory.as_ref() else {
            return Ok(ToolOutput::ok("记忆功能未启用。"));
        };
        let a: ReadArgs = parse_args(&args)?;
        let m = store
            .get(a.id.clone())
            .await
            .map_err(|e| ToolError::Other(format!("memory get: {e}")))?
            .ok_or_else(|| ToolError::Other(format!("记忆 {id} 不存在", id = a.id)))?;
        let events = store.list_events(a.id.clone()).await.unwrap_or_default();
        let evidence: Vec<serde_json::Value> = m
            .evidence_refs
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<dss_memory::EvidenceRef>>(s).ok())
            .map(|refs| {
                refs.into_iter()
                    .map(|r| json!({ "session_id": r.session_id, "run_id": r.run_id, "seq": format!("{}-{}", r.seq_start, r.seq_end) }))
                    .collect()
            })
            .unwrap_or_default();
        let body = serde_json::to_string_pretty(&json!({
            "memory": {
                "id": m.id,
                "scope": m.scope,
                "type": m.claim_type,
                "body": m.body,
                "confidence": m.confidence,
                "status": m.status,
                "origin": m.origin,
                "superseded_by": m.superseded_by,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
            },
            "evidence": evidence,
            "timeline": events,
        }))
        .unwrap_or_else(|_| "{}".into());
        Ok(ToolOutput::ok(body))
    }
}
