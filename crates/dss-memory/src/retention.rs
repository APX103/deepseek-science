//! Retention policy：基于使用率和时效的软遗忘。
//!
//! 区分两种遗忘（文章 §5）：
//! - 软遗忘（本模块）：降低召回概率 → 时间衰减 + 低使用率淘汰。
//! - 硬删除：保留给显式用户删除/隐私请求（soft_delete 已实现，走 memory_events 审计）。
//!
//! 本模块的 sweep 是幂等的、可周期执行（启动 + 定时）。不会删数据，只改 status：
//! - active 且 valid_until 已过 → expired
//! - active 且 last_surfaced 超 N 天 且 confidence 低 → candidate（降权，等人工清理）
//!
//! 软删除超 M 天的行可硬删 purge（本版本不自动 purge，只标记，留审计）。
//!
//! 时间判断用 ISO8601 字符串字典序比较（等价于时间序），避免引入 chrono 依赖。
//! now_iso() 由调用方传入（避免本 crate 依赖时钟）。

use dss_db::repo::MemoryFilter;

use crate::MemoryStore;

/// Retention 配置。
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// 候选/过期记忆超此天数未 surfaced → 可硬删（本版本仅标记，不执行）。
    pub stale_days: i64,
    /// 低使用率淘汰：last_surfaced 超 stale_days 且 confidence 低于此值 → 降为 candidate。
    pub low_usage_confidence: f64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            stale_days: 90,
            low_usage_confidence: 0.3,
        }
    }
}

/// sweep 统计（供 observability）。
#[derive(Debug, Clone, Default)]
pub struct RetentionStats {
    pub expired: usize,
    pub demoted_to_candidate: usize,
    pub errors: usize,
}

/// 执行一次 retention sweep。幂等，可周期调用。
///
/// `now_iso` = 当前时间的 RFC3339 字符串（调用方提供，避免本 crate 依赖时钟/chrono）。
/// `stale_cutoff_iso` = now - stale_days 的 RFC3339 字符串。
///
/// 不做硬删（保留审计；硬删由独立 maintenance 任务或用户显式触发）。
pub async fn sweep(
    store: &MemoryStore,
    cfg: &RetentionConfig,
    now_iso: &str,
    stale_cutoff_iso: &str,
) -> RetentionStats {
    let mut stats = RetentionStats::default();

    let rows = match store
        .list_filtered(MemoryFilter {
            project_id: None,
            entity: None,
            status: Some("active"),
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "retention sweep: load active failed");
            stats.errors += 1;
            return stats;
        }
    };

    for m in rows {
        // 1. valid_until 已过 → expired（ISO8601 字典序 = 时间序）。
        if let Some(until) = m.valid_until.as_deref() {
            if until < now_iso {
                match store
                    .update_status(m.id.clone(), "expired", Some("retention"))
                    .await
                {
                    Ok(_) => stats.expired += 1,
                    Err(e) => {
                        tracing::warn!(error = %e, mid = %m.id, "retention: expire failed");
                        stats.errors += 1;
                    }
                }
                continue;
            }
        }
        // 2. 低使用率：长期未召回 + 低置信 → 降为 candidate（不再召回）。
        // last_surfaced 为 None（从未召回）或早于 cutoff → 视为 stale。
        let stale = m
            .last_surfaced_at
            .as_deref()
            .map(|t| t < stale_cutoff_iso)
            .unwrap_or(true);
        if stale && m.confidence < cfg.low_usage_confidence {
            match store
                .update_status(m.id.clone(), "candidate", Some("retention"))
                .await
            {
                Ok(_) => stats.demoted_to_candidate += 1,
                Err(e) => {
                    tracing::warn!(error = %e, mid = %m.id, "retention: demote failed");
                    stats.errors += 1;
                }
            }
        }
    }
    stats
}
