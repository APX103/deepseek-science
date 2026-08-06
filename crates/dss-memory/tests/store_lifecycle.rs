//! Claim Store 端到端集成测试：用真实 SQLite pool 验证 store 全生命周期 +
//! consolidate 编排 + recall 索引召回。

use std::sync::Arc;

use dss_db::{open_pool, repo::NewMemory, run_migrations};
use dss_memory::consolidate::{promote_candidates, ConsolidateConfig};
use dss_memory::extract::ExtractedMem;
use dss_memory::retention::{sweep, RetentionConfig};
use dss_memory::types::{ClaimType, Origin};
use dss_memory::MemoryStore;

async fn fresh_store() -> MemoryStore {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(open_pool(dir.path()).unwrap());
    run_migrations(&pool).await.unwrap();
    MemoryStore::new(pool)
}

fn mk_extracted(body: &str, ct: ClaimType, conf: f64) -> ExtractedMem {
    ExtractedMem {
        body: body.into(),
        r#type: Some(ct.as_str().into()),
        confidence: Some(conf),
    }
}

// ============ store 写入 + 审计事件 ============

#[tokio::test]
async fn append_full_writes_created_event() {
    let store = fresh_store().await;
    let row = store
        .append_full(NewMemory {
            id: "mem_test00000001",
            body: "用户使用 Rust",
            scope: Some("profile"),
            status: Some("active"),
            origin: Some(Origin::Explicit.as_str()),
            claim_type: Some("fact"),
            confidence: Some(0.9),
            source_hash: Some("abc123"),
            ..Default::default()
        })
        .await
        .unwrap();

    let events = store.list_events(row.id.clone()).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "created");
}

#[tokio::test]
async fn soft_delete_sets_status_and_writes_event() {
    let store = fresh_store().await;
    let row = store
        .append_full(NewMemory {
            id: "mem_test00000002",
            body: "x",
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();

    store.soft_delete(row.id.clone()).await.unwrap();

    let after = store.get(row.id.clone()).await.unwrap().unwrap();
    assert_eq!(after.status, "deleted");
    assert!(after.deleted_at.is_some());

    let events = store.list_events(row.id).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "deleted"));
}

#[tokio::test]
async fn supersede_marks_old_and_links() {
    let store = fresh_store().await;
    let old = store
        .append_full(NewMemory {
            id: "mem_old000000001",
            body: "旧事实",
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();
    let new = store
        .append_full(NewMemory {
            id: "mem_new000000001",
            body: "新事实",
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();

    store
        .supersede(old.id.clone(), new.id.clone())
        .await
        .unwrap();

    let old_after = store.get(old.id.clone()).await.unwrap().unwrap();
    assert_eq!(old_after.status, "superseded");
    assert_eq!(old_after.superseded_by.as_deref(), Some(new.id.as_str()));

    let events = store.list_events(old.id).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "superseded"));
}

#[tokio::test]
async fn update_status_maps_approve_reject_event_names() {
    let store = fresh_store().await;
    let row = store
        .append_full(NewMemory {
            id: "mem_test00000003",
            body: "偏好深色主题",
            status: Some("candidate"),
            claim_type: Some("preference"),
            ..Default::default()
        })
        .await
        .unwrap();

    // approve → event 名应为 approved
    store
        .update_status(row.id.clone(), "active", Some("user"))
        .await
        .unwrap();
    let events = store.list_events(row.id.clone()).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "approved"));

    // reject (status=deleted) → event 名应为 rejected
    store
        .update_status(row.id.clone(), "deleted", Some("user"))
        .await
        .unwrap();
    let events = store.list_events(row.id.clone()).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "rejected"));
}

#[tokio::test]
async fn edit_body_recomputes_source_hash() {
    let store = fresh_store().await;
    let row = store
        .append_full(NewMemory {
            id: "mem_test00000004",
            body: "原始内容",
            status: Some("active"),
            source_hash: Some("oldhash"),
            ..Default::default()
        })
        .await
        .unwrap();

    store
        .edit_body(row.id.clone(), "修改后的内容".into(), Some("user"))
        .await
        .unwrap();

    let after = store.get(row.id.clone()).await.unwrap().unwrap();
    assert_eq!(after.body, "修改后的内容");
    // source_hash 应被重算（不再是 oldhash）
    assert_ne!(after.source_hash.as_deref(), Some("oldhash"));
    assert_eq!(
        after.source_hash.as_deref(),
        Some(dss_memory::memory_hash("修改后的内容").as_str())
    );

    let events = store.list_events(row.id).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "edited"));
}

// ============ consolidate promote_candidates 编排 ============

#[tokio::test]
async fn promote_dedupes_exact_duplicate() {
    let store = fresh_store().await;
    let evidence = vec![];
    let cfg = ConsolidateConfig::default();

    // 第一次抽取
    promote_candidates(
        &store,
        vec![mk_extracted("用户使用 Rust 编程语言", ClaimType::Fact, 0.9)],
        None,
        &evidence,
        &cfg,
    )
    .await;
    // 第二次抽同样内容
    let stats = promote_candidates(
        &store,
        vec![mk_extracted("用户使用 Rust 编程语言", ClaimType::Fact, 0.9)],
        None,
        &evidence,
        &cfg,
    )
    .await;

    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.promoted_active, 0);
}

#[tokio::test]
async fn promote_routes_high_risk_to_candidate() {
    let store = fresh_store().await;
    let cfg = ConsolidateConfig::default();

    let stats = promote_candidates(
        &store,
        vec![mk_extracted(
            "用户偏好深色主题",
            ClaimType::Preference,
            0.95,
        )],
        None,
        &[],
        &cfg,
    )
    .await;

    assert_eq!(stats.promoted_candidate, 1);
    assert_eq!(stats.promoted_active, 0);

    // candidate 不应被召回
    let hits = store.recall_indexed("深色主题", None, 5).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn promote_promotes_confident_fact_active() {
    let store = fresh_store().await;
    let cfg = ConsolidateConfig::default();

    let stats = promote_candidates(
        &store,
        vec![mk_extracted("项目使用 tokio 运行时", ClaimType::Fact, 0.85)],
        None,
        &[],
        &cfg,
    )
    .await;

    assert_eq!(stats.promoted_active, 1);
    // active 应能召回
    let hits = store.recall_indexed("tokio 运行时", None, 5).await.unwrap();
    assert!(!hits.is_empty());
}

#[tokio::test]
async fn promote_attaches_evidence_refs() {
    let store = fresh_store().await;
    let cfg = ConsolidateConfig::default();
    let evidence = vec![dss_memory::EvidenceRef {
        session_id: "sess_abc".into(),
        run_id: Some("run_1".into()),
        seq_start: 1,
        seq_end: 10,
    }];

    promote_candidates(
        &store,
        vec![mk_extracted("重要决策", ClaimType::Fact, 0.9)],
        None,
        &evidence,
        &cfg,
    )
    .await;

    let mems = store.list(None, None).await.unwrap();
    assert_eq!(mems.len(), 1);
    let refs = mems[0].evidence_refs.as_deref().unwrap();
    assert!(refs.contains("sess_abc"));
    assert!(refs.contains("run_1"));
}

// ============ recall 索引 ============

#[tokio::test]
async fn recall_only_returns_active_excluding_candidate_superseded() {
    let store = fresh_store().await;
    // active
    store
        .append_full(NewMemory {
            id: "mem_active0000001",
            body: "用户研究钙钛矿太阳电池",
            scope: Some("profile"),
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();
    // candidate（同主题，不应召回）
    store
        .append_full(NewMemory {
            id: "mem_cand00000001",
            body: "钙钛矿偏好无铅",
            scope: Some("profile"),
            status: Some("candidate"),
            ..Default::default()
        })
        .await
        .unwrap();

    let hits = store.recall_indexed("钙钛矿", None, 5).await.unwrap();
    let ids: Vec<_> = hits.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"mem_active0000001"));
    assert!(!ids.contains(&"mem_cand00000001"));
}

#[tokio::test]
async fn recall_index_invalidates_after_write_and_rebuilds() {
    let store = fresh_store().await;
    store
        .append_full(NewMemory {
            id: "mem_a00000000001",
            body: "用户喜欢 Rust",
            scope: Some("profile"),
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();
    // 首次召回 → 构建索引
    let hits1 = store.recall_indexed("Rust", None, 5).await.unwrap();
    assert!(!hits1.is_empty());

    // 写入新记忆 → 索引应失效
    store
        .append_full(NewMemory {
            id: "mem_b00000000001",
            body: "用户也喜欢 Tokio",
            scope: Some("profile"),
            status: Some("active"),
            ..Default::default()
        })
        .await
        .unwrap();
    // 再次召回 → 应重建索引并包含新记忆
    let hits2 = store.recall_indexed("Tokio", None, 5).await.unwrap();
    assert!(hits2.iter().any(|(id, _)| id == "mem_b00000000001"));
}

// ============ retention sweep ============

#[tokio::test]
async fn promote_falls_back_to_profile_when_project_missing() {
    // FK 防御：无效 project_id 会因 FK 约束静默失败。
    // promote_candidates 应把无效 project 降级为 profile，保证记忆不丢。
    let store = fresh_store().await;
    let cfg = ConsolidateConfig::default();

    let stats = promote_candidates(
        &store,
        vec![mk_extracted("项目用 tokio", ClaimType::Fact, 0.9)],
        Some("proj_does_not_exist".into()),
        &[],
        &cfg,
    )
    .await;

    assert_eq!(stats.errors, 0, "no FK failure should occur");
    assert_eq!(stats.promoted_active, 1, "memory must be written");
    let mems = store.list(None, None).await.unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].scope.as_deref(), Some("profile"));
    assert!(mems[0].project_id.is_none());
}

#[tokio::test]
async fn retention_expires_past_valid_until() {
    let store = fresh_store().await;
    let past = "2020-01-01T00:00:00Z";
    // valid_until 已过
    store
        .append_full(NewMemory {
            id: "mem_exp000000001",
            body: "过时事实",
            scope: Some("profile"),
            status: Some("active"),
            valid_until: Some(past),
            ..Default::default()
        })
        .await
        .unwrap();
    // valid_until 在未来，不应过期
    store
        .append_full(NewMemory {
            id: "mem_ok0000000001",
            body: "有效事实",
            scope: Some("profile"),
            status: Some("active"),
            valid_until: Some("2099-01-01T00:00:00Z"),
            ..Default::default()
        })
        .await
        .unwrap();

    let stats = sweep(
        &store,
        &RetentionConfig::default(),
        "2026-08-06T00:00:00Z",
        "2026-05-08T00:00:00Z",
    )
    .await;

    assert_eq!(stats.expired, 1);
    let exp = store.get("mem_exp000000001".into()).await.unwrap().unwrap();
    assert_eq!(exp.status, "expired");
    let ok = store.get("mem_ok0000000001".into()).await.unwrap().unwrap();
    assert_eq!(ok.status, "active");
}

#[tokio::test]
async fn retention_demotes_low_usage_low_confidence() {
    let store = fresh_store().await;
    // 长期未召回(last_surfaced=None → stale) + 低置信 → 降级
    store
        .append_full(NewMemory {
            id: "mem_low000000001",
            body: "低价值低置信",
            scope: Some("profile"),
            status: Some("active"),
            confidence: Some(0.1),
            ..Default::default()
        })
        .await
        .unwrap();
    // 低置信但高价值不会被这条规则动（confidence 阈值 0.3）。0.5 > 0.3 不降级。
    store
        .append_full(NewMemory {
            id: "mem_mid000000001",
            body: "中置信",
            scope: Some("profile"),
            status: Some("active"),
            confidence: Some(0.5),
            ..Default::default()
        })
        .await
        .unwrap();

    let stats = sweep(
        &store,
        &RetentionConfig::default(),
        "2026-08-06T00:00:00Z",
        "2026-05-08T00:00:00Z",
    )
    .await;

    assert_eq!(stats.demoted_to_candidate, 1);
    let low = store.get("mem_low000000001".into()).await.unwrap().unwrap();
    assert_eq!(low.status, "candidate");
    let mid = store.get("mem_mid000000001".into()).await.unwrap().unwrap();
    assert_eq!(mid.status, "active");
}

#[tokio::test]
async fn retention_is_idempotent() {
    let store = fresh_store().await;
    store
        .append_full(NewMemory {
            id: "mem_exp000000002",
            body: "过时",
            scope: Some("profile"),
            status: Some("active"),
            valid_until: Some("2020-01-01T00:00:00Z"),
            ..Default::default()
        })
        .await
        .unwrap();

    let cfg = RetentionConfig::default();
    let now = "2026-08-06T00:00:00Z";
    let cutoff = "2026-05-08T00:00:00Z";
    let s1 = sweep(&store, &cfg, now, cutoff).await;
    let s2 = sweep(&store, &cfg, now, cutoff).await;
    // 第一次过期 1 条；第二次该条已是 expired（不在 active 扫描集），不再处理。
    assert_eq!(s1.expired, 1);
    assert_eq!(s2.expired, 0);
}
