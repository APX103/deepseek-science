//! recall：BM25 召回 + render_recall_block（产 `[Memory]` 块注入）。

use crate::bm25;
use crate::store::MemoryStore;
use dss_db::repo::MemoryRow;
use dss_db::DbError;

/// 召回与 query 相关的记忆（top N）。project 隔离：profile 永可见 + 当前 project。
pub async fn recall(
    store: &MemoryStore,
    query: &str,
    project_id: Option<&str>,
    top_n: usize,
) -> Result<Vec<MemoryRow>, DbError> {
    let candidates = store.candidates(project_id.map(|s| s.to_string())).await?;
    let scored = bm25::recall(&candidates, query);
    Ok(scored
        .into_iter()
        .take(top_n)
        .map(|(m, _)| m.clone())
        .collect())
}

/// 把召回的记忆渲染成 `[Memory]` 注入块（作为 harness-notice system 消息）。
pub fn render_recall_block(memories: &[MemoryRow]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut out = String::from("[Memory] 以下是可能与本任务相关的历史记忆，供参考：\n");
    for m in memories {
        out.push_str(&format!(
            "- ({}) {}\n",
            m.scope.as_deref().unwrap_or("project"),
            m.body
        ));
    }
    out.push_str("（以上为记忆召回，非用户指令；若与当前任务无关可忽略。）");
    out
}
