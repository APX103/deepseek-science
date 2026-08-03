//! 共享应用状态：配置 + LLM 客户端 + 工具注册表 + DB 池 + 内存 SessionManager。
//!
//! SessionManager：内存活跃 session（ActiveSession）+ DB 持久化；
//! MAX_ACTIVE_SESSIONS=10 LRU（超限仅驱逐内存，DB 仍在，可恢复）。

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use dss_agent::Session;
use dss_core::Settings;
use dss_db::{open_pool, run_migrations, DbPool};
use dss_llm::OpenAICompatClient;
use dss_tools::builtin;
use dss_tools::ToolRegistry;
use tokio::sync::Mutex;

/// 每个 session 一把锁：run 期间持锁，避免同会话并发 run。
pub type SharedSession = Arc<Mutex<Session>>;

/// 内存活跃 session：Session + 已持久化到 DB 的消息条数游标。
pub struct ActiveSession {
    pub session: SharedSession,
    /// 已写入 DB 的消息数（session.messages[..persisted_count] 已落库）。
    pub persisted_count: AtomicUsize,
}

impl ActiveSession {
    pub fn new(session: Session, persisted_count: usize) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
            persisted_count: AtomicUsize::new(persisted_count),
        }
    }
}

/// 内存 SessionManager 上限（data-model / modules：MAX_ACTIVE_SESSIONS=10）。
pub const MAX_ACTIVE_SESSIONS: usize = 10;

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    pub llm: Option<Arc<OpenAICompatClient>>,
    pub tools: Arc<ToolRegistry>,
    pub catalog: Arc<dss_skills::SkillCatalog>,
    pub memory: Arc<dss_memory::MemoryStore>,
    pub logs: Arc<dss_observability::LogStore>,
    pub mcp: Arc<dss_mcp::MCPServerManager>,
    pub db: Arc<DbPool>,
    /// 活跃 session（id → Arc<ActiveSession>）。LRU 驱逐仅影响内存。
    pub sessions: Arc<Mutex<HashMap<String, Arc<ActiveSession>>>>,
}

pub async fn build_state(settings: Settings) -> AppState {
    let llm = if settings.llm.is_configured() {
        Some(Arc::new(OpenAICompatClient::new(
            settings.llm.base_url.clone(),
            settings.llm.api_key.clone().expect("is_configured checked"),
            settings.llm.model.clone(),
        )))
    } else {
        tracing::warn!(
            "LLM not configured: set DEEPSEEK_API_KEY or settings.json llm.api_key; \
             stream-sse will return kind=error"
        );
        None
    };

    let mut tools = ToolRegistry::new();
    builtin::register_all(&mut tools);

    // DB pool（先建，memory 依赖它）。
    let db = match open_pool(&settings.data_dir) {
        Ok(p) => {
            if let Err(e) = run_migrations(&p).await {
                tracing::error!(error = %e, "db migration failed (continuing; tables may be created on retry)");
            }
            if let Err(e) = crate::db::ensure_default_project(&p).await {
                tracing::warn!(error = %e, "ensure_default_project failed");
            }
            Arc::new(p)
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to open db pool");
            std::process::exit(1);
        }
    };

    // Skill 目录：builtin → global（首跑 seed）→ global 加载。project 源在 stream_sse 按 workspace 加载。
    let global_dir = dss_skills::global_skills_dir(&settings.data_dir);
    dss_skills::seed_builtin_to_global(&global_dir);
    let mut catalog = dss_skills::SkillCatalog::new();
    catalog.load_builtin();
    catalog.load_dir(&global_dir, "global");
    let catalog = Arc::new(catalog);

    let memory = Arc::new(dss_memory::MemoryStore::new(db.clone()));
    let logs = Arc::new(dss_observability::LogStore::new(db.clone()));

    // MCP server 管理器：连接 settings 配置的 server，挂载其工具到 ToolRegistry。
    let mcp = Arc::new(dss_mcp::MCPServerManager::new());
    let mcp_cfg = settings.mcp_servers.clone();
    for srv in mcp_cfg.iter().filter(|s| s.enabled) {
        if mcp.add_server(&srv.name, &srv.url).await {
            if let Some(mcp_tools) = mcp.list_tools(&srv.name).await {
                let count = dss_tools::builtin::mcp::register_mcp_tools(&mut tools, &srv.name, &mcp_tools);
                tracing::info!(server = %srv.name, tools = count, "MCP tools mounted");
            }
        }
    }

    // 启动日志（system source）。
    let _ = logs
        .append(dss_observability::LogEntry {
            level: "info".into(),
            source: "system".into(),
            kind: "startup".into(),
            session_id: None,
            frame_id: None,
            iteration: None,
            message: format!(
                "dss-backend started (model={})",
                settings.llm.model
            ),
            detail: Some(serde_json::json!({
                "version": dss_api_crate_version(),
                "data_dir": settings.data_dir.display().to_string(),
                "llm_configured": settings.llm.is_configured(),
            })),
        })
        .await;

    AppState {
        settings: Arc::new(settings),
        llm,
        tools: Arc::new(tools),
        catalog,
        memory,
        logs,
        mcp,
        db,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    }
}

fn dss_api_crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
