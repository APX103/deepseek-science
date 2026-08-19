//! 巩固流水线：candidate → 去重 → 校验 → 晋升。
//!
//! 不依赖 LLM 识别"replace/remove 目标"（那不可靠），而是用确定性规则：
//! 1. source_hash 精确匹配 → 跳过（已存在）
//! 2. BM25 近似重复（相似度 > 阈值）→ 旧 claim supersede，新 claim 替代
//! 3. claim_type 高风险（preference/decision）→ status=Candidate 待审
//! 4. 否则 confidence 达标 → status=Active
//!
//! 每步写 memory_events。详见 docs 设计与计划。

use dss_db::repo::{MemoryFilter, MemoryRow};

use crate::bm25;
use crate::types::{ClaimType, EvidenceRef, MemoryStatus, Origin};

/// 单个待巩固的候选（来自 extract 的输出）。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub body: String,
    pub claim_type: ClaimType,
    pub confidence: f64,
    pub scope: Option<String>,
    pub project_id: Option<String>,
    pub origin: Origin,
}

/// 巩固决策结果（用于决定写入方式 + 写 memory_events）。
#[derive(Debug, Clone)]
pub enum Decision {
    /// 与现有 claim 精确重复，丢弃。
    Duplicate { existing_id: String },
    /// 近似重复，新 claim 替代旧的。
    Supersede { old_id: String },
    /// 新 claim，以该 status 写入。
    Promote { status: MemoryStatus },
}

/// 巩固配置（来自 MemorySettings，1.8 接入）。
#[derive(Debug, Clone)]
pub struct ConsolidateConfig {
    /// 自动晋升为 active 的最低 confidence（低于则进 candidate）。
    pub auto_promote_threshold: f64,
    /// BM25 近似重复判定阈值（归一化分数，0..1）。
    pub dedupe_similarity: f64,
    /// 高风险（preference/decision）一律进 candidate 等审批。
    pub trust_high_risk_approve: bool,
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        Self {
            auto_promote_threshold: 0.5,
            dedupe_similarity: 0.85,
            trust_high_risk_approve: true,
        }
    }
}

/// 对单个候选做去重 + 信任分流决策。
///
/// `existing` = 同 scope/project 下当前 active+candidate 的全部记忆（候选集）。
/// 决策不含 IO，纯函数，便于测试。
pub fn decide(candidate: &Candidate, existing: &[MemoryRow], cfg: &ConsolidateConfig) -> Decision {
    // 1. source_hash 精确匹配 → 重复
    let cand_hash = crate::memory_hash(&candidate.body);
    for m in existing {
        if m.source_hash.as_deref() == Some(cand_hash.as_str())
            && matches!(m.status.as_str(), "active" | "candidate")
        {
            return Decision::Duplicate {
                existing_id: m.id.clone(),
            };
        }
    }

    // 2. 高风险候选必须先进入审批，不能因为相似度而绕过审批并 supersede active。
    if cfg.trust_high_risk_approve && candidate.claim_type.is_high_risk() {
        return Decision::Promote {
            status: MemoryStatus::Candidate,
        };
    }

    // 3. BM25 近似重复 → 找最相似的 active claim
    if !existing.is_empty() {
        // 只对 active 记忆做相似度（candidate/superseded 不参与替代）。
        let active: Vec<MemoryRow> = existing
            .iter()
            .filter(|m| m.status == "active")
            .cloned()
            .collect();
        if !active.is_empty() {
            let scored = bm25::recall(&active, &candidate.body);
            if let Some((best, score)) = scored.first() {
                // BM25 分数无固定上界，用相对归一化：best_score / (best 自评的上界)
                // self_score 用候选 body 当作 query 时旧记忆的自身上界做归一。
                let self_score = bm25::self_score(best, &candidate.body);
                let sim = if self_score > 0.0 {
                    score / self_score
                } else {
                    0.0
                };
                if sim >= cfg.dedupe_similarity {
                    return Decision::Supersede {
                        old_id: best.id.clone(),
                    };
                }
            }
        }
    }

    // 4. confidence 达标 → active，否则也进 candidate
    let status = if candidate.confidence >= cfg.auto_promote_threshold {
        MemoryStatus::Active
    } else {
        MemoryStatus::Candidate
    };
    Decision::Promote { status }
}

/// 巩固统计（供 observability）。
#[derive(Debug, Clone, Default)]
pub struct ConsolidateStats {
    pub promoted_active: usize,
    pub promoted_candidate: usize,
    pub superseded: usize,
    pub duplicates: usize,
    pub errors: usize,
}

/// 把 extract 的输出批量巩固进 store。
///
/// 流程：取候选集 → 逐条 decide → 按决策写入（append_full / supersede）。
/// evidence_refs 附加到每条新写入的记忆。这是 sessions.rs 后台抽取任务的高层入口。
pub async fn promote_candidates(
    store: &crate::MemoryStore,
    extracted: Vec<crate::extract::ExtractedMem>,
    project_id: Option<String>,
    evidence: &[EvidenceRef],
    cfg: &ConsolidateConfig,
) -> ConsolidateStats {
    use dss_db::repo::NewMemory;

    let mut stats = ConsolidateStats::default();
    if extracted.is_empty() {
        return stats;
    }

    // FK 防御：project_id 对应的 project 可能已被删除（sessions.project_id 是 ON DELETE SET NULL，
    // 但抽取拿到的可能是 run 持久化前的旧值）。无效 project_id 会因 FK 约束让 INSERT 静默失败。
    // 降级为 profile（跨项目），保证记忆不丢；证据链里仍保留 session_id 可追溯。
    let project_id = match project_id.as_deref() {
        Some(pid) if !pid.is_empty() => match store.project_exists(pid.to_string()).await {
            Ok(true) => project_id,
            Ok(false) => {
                tracing::warn!(
                    project_id = pid,
                    "consolidate: project not found, falling back to profile scope"
                );
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "consolidate: project_exists check failed, falling back to profile");
                None
            }
        },
        _ => project_id,
    };

    // 候选集 = 同 scope 的 active+candidate 记忆（去重/替代判定用）。
    let existing = match store
        .list_filtered(MemoryFilter {
            project_id: project_id.as_deref(),
            entity: None,
            status: None,
        })
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter(|m| matches!(m.status.as_str(), "active" | "candidate"))
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(error = %e, "consolidate: load existing failed");
            stats.errors = extracted.len();
            return stats;
        }
    };

    let evidence_json = if evidence.is_empty() {
        None
    } else {
        Some(crate::store::evidence_refs_json(evidence))
    };

    for em in extracted {
        let claim_type = em.claim_type();
        let conf = em.conf();
        let candidate = Candidate {
            body: em.body.clone(),
            claim_type,
            confidence: conf,
            scope: Some(if project_id.is_some() {
                "project".into()
            } else {
                "profile".into()
            }),
            project_id: project_id.clone(),
            origin: Origin::Auto,
        };
        let decision = decide(&candidate, &existing, cfg);
        match decision {
            Decision::Duplicate { .. } => stats.duplicates += 1,
            Decision::Supersede { old_id } => {
                // 写新 claim（active），旧 claim supersede。
                let id = crate::types::gen_id("mem_");
                let hash = crate::memory_hash(&em.body);
                let status = if claim_type.is_high_risk() && cfg.trust_high_risk_approve {
                    "candidate"
                } else {
                    "active"
                };
                let new = store
                    .append_full(NewMemory {
                        id: &id,
                        body: &em.body,
                        scope: candidate.scope.as_deref(),
                        project_id: project_id.as_deref(),
                        confidence: Some(conf),
                        claim_type: Some(claim_type.as_str()),
                        status: Some(status),
                        origin: Some(Origin::Auto.as_str()),
                        evidence_refs: evidence_json.as_deref(),
                        source_hash: Some(&hash),
                        ..Default::default()
                    })
                    .await;
                match new {
                    Ok(row) => {
                        if let Err(e) = store.supersede(old_id, row.id).await {
                            tracing::warn!(error = %e, "consolidate: supersede failed");
                            stats.errors += 1;
                        } else {
                            stats.superseded += 1;
                            if status == "candidate" {
                                stats.promoted_candidate += 1;
                            } else {
                                stats.promoted_active += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "consolidate: append (supersede new) failed");
                        stats.errors += 1;
                    }
                }
            }
            Decision::Promote { status } => {
                let id = crate::types::gen_id("mem_");
                let hash = crate::memory_hash(&em.body);
                let st = status.as_str();
                if let Err(e) = store
                    .append_full(NewMemory {
                        id: &id,
                        body: &em.body,
                        scope: candidate.scope.as_deref(),
                        project_id: project_id.as_deref(),
                        confidence: Some(conf),
                        claim_type: Some(claim_type.as_str()),
                        status: Some(st),
                        origin: Some(Origin::Auto.as_str()),
                        evidence_refs: evidence_json.as_deref(),
                        source_hash: Some(&hash),
                        ..Default::default()
                    })
                    .await
                {
                    tracing::warn!(error = %e, "consolidate: append (promote) failed");
                    stats.errors += 1;
                } else if status == MemoryStatus::Active {
                    stats.promoted_active += 1;
                } else {
                    stats.promoted_candidate += 1;
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(body: &str, ct: ClaimType, conf: f64) -> Candidate {
        Candidate {
            body: body.into(),
            claim_type: ct,
            confidence: conf,
            scope: Some("project".into()),
            project_id: None,
            origin: Origin::Auto,
        }
    }

    fn row(id: &str, body: &str, hash: Option<&str>) -> MemoryRow {
        MemoryRow {
            id: id.into(),
            entity: "project".into(),
            scope: Some("project".into()),
            entity_type: "note".into(),
            body: body.into(),
            project_id: None,
            confidence: 0.5,
            created_at: "t".into(),
            updated_at: "t".into(),
            last_surfaced_at: None,
            status: "active".into(),
            claim_type: "fact".into(),
            evidence_refs: None,
            origin: "auto".into(),
            superseded_by: None,
            valid_from: None,
            valid_until: None,
            deleted_at: None,
            source_hash: hash.map(|h| h.into()),
        }
    }

    #[test]
    fn exact_duplicate_is_dropped() {
        let body = "用户使用 Rust 编程语言";
        let h = crate::memory_hash(body);
        let existing = vec![row("m1", body, Some(&h))];
        let d = decide(
            &cand(body, ClaimType::Fact, 0.9),
            &existing,
            &Default::default(),
        );
        assert!(matches!(d, Decision::Duplicate { existing_id } if existing_id == "m1"));
    }

    #[test]
    fn high_risk_goes_to_candidate() {
        let d = decide(
            &cand("用户偏好深色主题", ClaimType::Preference, 0.95),
            &[],
            &Default::default(),
        );
        assert!(matches!(
            d,
            Decision::Promote {
                status: MemoryStatus::Candidate
            }
        ));
    }

    #[test]
    fn low_confidence_fact_goes_to_candidate() {
        let d = decide(
            &cand("可能用了某个库", ClaimType::Fact, 0.2),
            &[],
            &Default::default(),
        );
        assert!(matches!(
            d,
            Decision::Promote {
                status: MemoryStatus::Candidate
            }
        ));
    }

    #[test]
    fn confident_fact_is_promoted_active() {
        let d = decide(
            &cand("项目用 tokio 运行时", ClaimType::Fact, 0.8),
            &[],
            &Default::default(),
        );
        assert!(matches!(
            d,
            Decision::Promote {
                status: MemoryStatus::Active
            }
        ));
    }

    #[test]
    fn near_duplicate_supersedes_old() {
        // 两条高度相似的记忆，第二条应替代第一条
        let old = row("m1", "用户的编程语言是 Rust", None);
        let existing = vec![old];
        let d = decide(
            &cand("用户使用 Rust 编程语言", ClaimType::Fact, 0.7),
            &existing,
            &ConsolidateConfig {
                dedupe_similarity: 0.3, // 降低阈值确保触发
                ..Default::default()
            },
        );
        assert!(matches!(d, Decision::Supersede { old_id } if old_id == "m1"));
    }

    #[test]
    fn similar_high_risk_memory_waits_for_approval_before_superseding() {
        let existing = vec![row("m1", "用户偏好深色主题", None)];
        let d = decide(
            &cand("用户仍然偏好深色主题", ClaimType::Preference, 0.99),
            &existing,
            &ConsolidateConfig {
                dedupe_similarity: 0.01,
                ..Default::default()
            },
        );
        assert!(matches!(
            d,
            Decision::Promote {
                status: MemoryStatus::Candidate
            }
        ));
    }
}
