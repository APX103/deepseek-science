//! 共享应用状态：配置 + LLM 客户端 + 工具注册表 + DB 池 + 内存 SessionManager。
//!
//! SessionManager：内存活跃 session（ActiveSession）+ DB 持久化；
//! MAX_ACTIVE_SESSIONS=10 LRU（超限仅驱逐内存，DB 仍在，可恢复）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use dss_a2a::{
    validate_config as validate_a2a_config, A2aClient, A2aRuntimeSnapshot, AgentRuntime,
    AgentRuntimeStatus,
};
use dss_agent::Session;
use dss_core::{LlmEnvOverrides, LlmSettings, Settings, ThinkingSettings};
use dss_db::{open_pool, run_migrations, DbPool};
use dss_llm::OpenAICompatClient;
use dss_tools::builtin;
use dss_tools::ToolRegistry;
use futures::future::join_all;
use tokio::sync::{watch, Mutex, Notify, RwLock};

/// 每个 session 一把锁：run 期间持锁，避免同会话并发 run。
pub type SharedSession = Arc<Mutex<Session>>;

const RUN_STATE_RUNNING: u8 = 0;
const RUN_STATE_CANCEL_REQUESTED: u8 = 1;
const RUN_STATE_TERMINAL: u8 = 2;

/// Explicit lifecycle control for one accepted session run.
///
/// The SSE connection is only a transport. Keeping cancellation in application
/// state lets the UI wait until the session mutex has actually been released
/// before it enables another prompt.
pub struct ActiveRunControl {
    state: AtomicU8,
    cancel_tx: watch::Sender<bool>,
    finished: AtomicBool,
    finished_notify: Notify,
    persistence: StdMutex<RunPersistenceState>,
}

/// Persistence starts pending and becomes committed only after the atomic run
/// transaction and in-memory cursor update both succeed. Pending is deliberately
/// not treated as success: a panicked/aborted worker must never release a cached
/// successful terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPersistenceState {
    Pending,
    Committed,
    Failed(String),
}

const MAX_RECENT_RUN_CONTROLS: usize = 64;
const MAX_PRE_CANCELLED_RUNS: usize = 64;

type RunKey = (String, String);

#[derive(Default)]
struct RunControlRegistryInner {
    controls: HashMap<RunKey, Arc<ActiveRunControl>>,
    control_order: VecDeque<RunKey>,
    pre_cancelled: VecDeque<RunKey>,
}

/// Process-wide run registry keyed by `(session_id, client_run_id)`.
///
/// The client-generated id closes the stop-before-accept race: a cancellation
/// that reaches the server first is retained, and the matching stream request
/// is rejected when it later attempts to register. Keeping this outside
/// `ActiveSession` also makes the ordering robust when a session is restored
/// concurrently by two requests.
#[derive(Default)]
pub struct RunControlRegistry {
    inner: StdMutex<RunControlRegistryInner>,
}

impl RunControlRegistry {
    /// Register an accepted run. Returns false when an earlier cancellation for
    /// this exact run id won the race.
    pub fn register(&self, sid: &str, run_id: &str, control: Arc<ActiveRunControl>) -> bool {
        let key = (sid.to_owned(), run_id.to_owned());
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(index) = inner.pre_cancelled.iter().position(|item| item == &key) {
            inner.pre_cancelled.remove(index);
            return false;
        }

        inner.controls.insert(key.clone(), control);
        if let Some(index) = inner.control_order.iter().position(|item| item == &key) {
            inner.control_order.remove(index);
        }
        inner.control_order.push_back(key);
        while inner.control_order.len() > MAX_RECENT_RUN_CONTROLS {
            let Some(index) = inner.control_order.iter().position(|candidate| {
                inner
                    .controls
                    .get(candidate)
                    .is_some_and(|control| control.is_finished())
            }) else {
                // Active runs are never evicted merely to satisfy the history bound.
                break;
            };
            if let Some(expired) = inner.control_order.remove(index) {
                inner.controls.remove(&expired);
            }
        }
        true
    }

    /// Look up one exact run for cancellation. When the stream request has not
    /// registered yet, retain a bounded pre-cancellation marker for it.
    pub fn find_or_pre_cancel(&self, sid: &str, run_id: &str) -> Option<Arc<ActiveRunControl>> {
        let key = (sid.to_owned(), run_id.to_owned());
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(control) = inner.controls.get(&key) {
            return Some(control.clone());
        }
        if !inner.pre_cancelled.iter().any(|item| item == &key) {
            inner.pre_cancelled.push_back(key);
            while inner.pre_cancelled.len() > MAX_PRE_CANCELLED_RUNS {
                inner.pre_cancelled.pop_front();
            }
        }
        None
    }
}

impl ActiveRunControl {
    pub fn new() -> (Arc<Self>, watch::Receiver<bool>) {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        (
            Arc::new(Self {
                state: AtomicU8::new(RUN_STATE_RUNNING),
                cancel_tx,
                finished: AtomicBool::new(false),
                finished_notify: Notify::new(),
                persistence: StdMutex::new(RunPersistenceState::Pending),
            }),
            cancel_rx,
        )
    }

    /// Returns false once a terminal event has already been committed. In that
    /// case the caller should let the normal completion reach the UI.
    pub fn request_cancel(&self) -> bool {
        loop {
            match self.state.load(Ordering::Acquire) {
                RUN_STATE_CANCEL_REQUESTED => return true,
                RUN_STATE_TERMINAL => return false,
                RUN_STATE_RUNNING => {
                    if self
                        .state
                        .compare_exchange(
                            RUN_STATE_RUNNING,
                            RUN_STATE_CANCEL_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.cancel_tx.send_replace(true);
                        return true;
                    }
                }
                _ => return false,
            }
        }
    }

    /// Linearizes a terminal event against a concurrent Stop request.
    pub fn mark_terminal(&self) -> bool {
        self.state
            .compare_exchange(
                RUN_STATE_RUNNING,
                RUN_STATE_TERMINAL,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn finish(&self) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            self.finished_notify.notify_waiters();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub async fn wait_finished(&self) {
        loop {
            let notified = self.finished_notify.notified();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Record a terminal storage failure for the SSE relay. Runner terminal
    /// events are intentionally held until `finish`; this lets the relay replace
    /// a successful-looking completion with an explicit error when the atomic
    /// run transaction did not commit.
    pub fn mark_persistence_committed(&self) {
        let mut state = self
            .persistence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*state, RunPersistenceState::Pending) {
            *state = RunPersistenceState::Committed;
        }
    }

    pub fn set_persistence_error(&self, message: impl Into<String>) {
        let mut state = self
            .persistence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*state, RunPersistenceState::Pending) {
            *state = RunPersistenceState::Failed(message.into());
        }
    }

    pub fn persistence_state(&self) -> RunPersistenceState {
        self.persistence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

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

#[cfg(test)]
mod active_run_tests {
    use super::{build_state, ActiveRunControl, RunControlRegistry, RunPersistenceState};

    #[tokio::test]
    async fn cancellation_waits_for_explicit_finish_ack() {
        let (control, mut cancel_rx) = ActiveRunControl::new();
        assert!(control.request_cancel());
        cancel_rx
            .changed()
            .await
            .expect("cancel sender remains alive");
        assert!(*cancel_rx.borrow());

        let waiter = tokio::spawn({
            let control = control.clone();
            async move { control.wait_finished().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        control.finish();
        waiter.await.expect("finish waiter");
    }

    #[tokio::test]
    async fn committed_terminal_event_wins_over_late_stop() {
        let (control, _cancel_rx) = ActiveRunControl::new();
        assert!(control.mark_terminal());
        assert!(!control.request_cancel());
        control.finish();
        control.wait_finished().await;
    }

    #[test]
    fn persistence_terminal_state_is_monotonic() {
        let (committed, _cancel_rx) = ActiveRunControl::new();
        committed.mark_persistence_committed();
        committed.set_persistence_error("late error");
        assert_eq!(
            committed.persistence_state(),
            RunPersistenceState::Committed
        );

        let (failed, _cancel_rx) = ActiveRunControl::new();
        failed.set_persistence_error("first error");
        failed.mark_persistence_committed();
        assert_eq!(
            failed.persistence_state(),
            RunPersistenceState::Failed("first error".into())
        );
    }

    #[test]
    fn pre_cancelled_run_can_never_register_late() {
        let registry = RunControlRegistry::default();
        assert!(registry.find_or_pre_cancel("session", "run-a").is_none());
        let (control, _cancel_rx) = ActiveRunControl::new();
        assert!(!registry.register("session", "run-a", control));
    }

    #[test]
    fn cancellation_is_scoped_to_the_exact_client_run_id() {
        let registry = RunControlRegistry::default();
        let (control, _cancel_rx) = ActiveRunControl::new();
        assert!(registry.register("session", "run-a", control.clone()));

        assert!(registry.find_or_pre_cancel("session", "run-b").is_none());
        assert!(
            control.mark_terminal(),
            "the unrelated run was not cancelled"
        );
        assert!(!registry
            .find_or_pre_cancel("session", "run-a")
            .expect("registered run")
            .request_cancel());
    }

    #[tokio::test]
    async fn migration_failure_prevents_application_startup() {
        let data_dir = std::env::temp_dir().join(format!(
            "deepseek-science-bad-schema-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&data_dir).expect("create test data directory");
        let database = rusqlite::Connection::open(data_dir.join("dss.db"))
            .expect("create incompatible database");
        database
            .execute_batch("CREATE VIEW projects AS SELECT 'incompatible' AS id;")
            .expect("install incompatible legacy object");
        drop(database);

        let result = build_state(dss_core::Settings {
            data_dir: data_dir.clone(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: dss_core::settings::ServerSettings::default(),
            llm: dss_core::LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: dss_core::LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await;
        assert!(
            matches!(result, Err(dss_db::DbError::Sqlite(_))),
            "schema migration errors must fail startup"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }
}

/// 内存 SessionManager 上限（data-model / modules：MAX_ACTIVE_SESSIONS=10）。
pub const MAX_ACTIVE_SESSIONS: usize = 10;
const MCP_STARTUP_BUDGET: Duration = Duration::from_secs(8);
const MCP_SERVER_CONNECT_BUDGET: Duration = Duration::from_secs(7);

/// One coherent runtime LLM configuration. Replacing the enclosing `Arc` makes the
/// client, model, base URL, and configured state visible as a single snapshot.
pub struct LlmRuntimeSnapshot {
    settings: LlmSettings,
    thinking: ThinkingSettings,
    client: Option<Arc<OpenAICompatClient>>,
    revision: u64,
    env_overrides: LlmEnvOverrides,
}

impl LlmRuntimeSnapshot {
    pub fn new(
        settings: LlmSettings,
        thinking: ThinkingSettings,
        revision: u64,
        env_overrides: LlmEnvOverrides,
    ) -> Self {
        let client = settings
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty())
            .map(|key| {
                Arc::new(
                    OpenAICompatClient::new(
                        settings.base_url.clone(),
                        key.to_owned(),
                        settings.model.clone(),
                    )
                    .with_thinking_settings(thinking),
                )
            });
        Self {
            settings,
            thinking,
            client,
            revision,
            env_overrides,
        }
    }

    pub fn settings(&self) -> &LlmSettings {
        &self.settings
    }

    pub fn thinking(&self) -> ThinkingSettings {
        self.thinking
    }

    pub fn client(&self) -> Option<&Arc<OpenAICompatClient>> {
        self.client.as_ref()
    }

    pub fn is_configured(&self) -> bool {
        self.client.is_some()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn overridden_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::with_capacity(3);
        if self.env_overrides.base_url {
            fields.push("base_url");
        }
        if self.env_overrides.model {
            fields.push("model");
        }
        if self.env_overrides.api_key {
            fields.push("api_key");
        }
        fields
    }
}

/// One pointer contains every hot-editable capability used to accept a run. A run clones this
/// Arc once, so a settings save can never mix an old LLM, A2A catalog, or data-source keys.
pub struct AppRuntimeSnapshot {
    revision: u64,
    llm: Arc<LlmRuntimeSnapshot>,
    a2a: Arc<A2aRuntimeSnapshot>,
    api_keys: Arc<HashMap<String, String>>,
    max_iterations: u32,
}

impl AppRuntimeSnapshot {
    pub fn new(
        revision: u64,
        llm: Arc<LlmRuntimeSnapshot>,
        a2a: Arc<A2aRuntimeSnapshot>,
        api_keys: Arc<HashMap<String, String>>,
        max_iterations: u32,
    ) -> Self {
        debug_assert_eq!(llm.revision(), revision);
        debug_assert_eq!(a2a.revision, revision);
        debug_assert!(dss_core::validate_max_iterations(max_iterations).is_ok());
        Self {
            revision,
            llm,
            a2a,
            api_keys,
            max_iterations,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn llm(&self) -> &Arc<LlmRuntimeSnapshot> {
        &self.llm
    }

    pub fn a2a(&self) -> &Arc<A2aRuntimeSnapshot> {
        &self.a2a
    }

    pub fn api_keys(&self) -> &HashMap<String, String> {
        self.api_keys.as_ref()
    }

    /// Per-run Agent iteration ceiling captured atomically with the other hot settings.
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    /// Packaged-app capability token. `None` keeps direct CLI launches backwards compatible.
    pub(crate) api_token: Option<Arc<str>>,
    /// A save builds a full replacement before the final pointer swap. Runs hold the read lock
    /// only long enough to clone one coherent LLM+A2A/data-source snapshot.
    pub(crate) runtime: Arc<RwLock<Arc<AppRuntimeSnapshot>>>,
    /// Serializes settings root read/merge/atomic-write so two saves cannot lose each other.
    pub(crate) settings_save_lock: Arc<Mutex<()>>,
    /// Shared no-redirect transport; it contains no Agent credentials or mutable card state.
    pub a2a_client: Arc<A2aClient>,
    /// Base tool registry: built-in tools only. Runs overlay per-run A2A tools and the current
    /// MCP tools on top of this immutable base.
    pub tools: Arc<ToolRegistry>,
    /// Skill catalog behind a lock so a settings save can hot-rebuild discovery (built-in
    /// enable/disable, external claude/codex/cursor dirs, custom dirs) without a restart.
    pub catalog: Arc<RwLock<Arc<dss_skills::SkillCatalog>>>,
    pub memory: Arc<dss_memory::MemoryStore>,
    pub logs: Arc<dss_observability::LogStore>,
    /// MCP manager + MCP-augmented tool registry, behind a lock so a settings save can reconnect
    /// servers and remount tools without a restart.
    pub mcp_runtime: Arc<RwLock<McpRuntime>>,
    pub db: Arc<DbPool>,
    /// 活跃 session（id → Arc<ActiveSession>）。LRU 驱逐仅影响内存。
    pub sessions: Arc<Mutex<HashMap<String, Arc<ActiveSession>>>>,
    /// Cold restoration is rare and serialized so check/load/insert is one
    /// single-flight operation. Callers receive the same Arc for one sid.
    pub(crate) session_restore_lock: Arc<Mutex<()>>,
    /// Exact-id cancellation registry shared across session restore races.
    pub(crate) run_controls: Arc<RunControlRegistry>,
}

impl AppState {
    pub async fn runtime_snapshot(&self) -> Arc<AppRuntimeSnapshot> {
        self.runtime.read().await.clone()
    }

    pub async fn llm_snapshot(&self) -> Arc<LlmRuntimeSnapshot> {
        self.runtime.read().await.llm().clone()
    }

    pub async fn a2a_snapshot(&self) -> Arc<A2aRuntimeSnapshot> {
        self.runtime.read().await.a2a().clone()
    }

    /// Clone the current skill catalog pointer. Runs and the `/api/skills` endpoint read this so a
    /// concurrent settings save can swap in a rebuilt catalog atomically.
    pub async fn catalog_snapshot(&self) -> Arc<dss_skills::SkillCatalog> {
        self.catalog.read().await.clone()
    }

    /// Rebuild the skill catalog from the given discovery configuration and publish it. Used after
    /// a settings save so built-in toggles, external directories, and custom dirs take effect
    /// without restarting the backend.
    pub async fn rebuild_catalog(&self, skills: &dss_core::SkillSettings) {
        let catalog = build_skill_catalog(&self.settings.data_dir, skills);
        *self.catalog.write().await = Arc::new(catalog);
    }

    /// Clone the current MCP runtime (manager + tools). Reads both together so a run never mixes a
    /// new tool registry with an old manager.
    pub async fn mcp_runtime_snapshot(&self) -> McpRuntime {
        self.mcp_runtime.read().await.clone()
    }

    /// Reconnect the given MCP servers and publish a rebuilt runtime. Used after a settings save so
    /// MCP server changes take effect without restarting the backend.
    pub async fn rebuild_mcp(&self, servers: &[dss_core::McpServerConfig]) {
        let runtime = build_mcp_runtime(self.tools.clone(), servers).await;
        *self.mcp_runtime.write().await = runtime;
    }

    /// Refresh discovery before publishing the run's tool catalog, so a restarted application
    /// still shows remote skills while the model decides whether delegation is useful. This is
    /// only a bounded catalog refresh; each actual invocation independently performs the
    /// mandatory call-time refresh again.
    pub async fn runtime_snapshot_with_refreshed_a2a(&self) -> Arc<AppRuntimeSnapshot> {
        const CATALOG_REFRESH_BUDGET: Duration = Duration::from_secs(5);

        let captured = self.runtime_snapshot().await;
        if captured.a2a().enabled().next().is_none() {
            return captured;
        }
        let Ok(refreshed) = tokio::time::timeout(
            CATALOG_REFRESH_BUDGET,
            captured.a2a().refresh_all(self.a2a_client.as_ref()),
        )
        .await
        else {
            tracing::warn!(
                budget_secs = CATALOG_REFRESH_BUDGET.as_secs(),
                "A2A run-catalog refresh budget exhausted; using the previous snapshot"
            );
            return captured;
        };
        let replacement = Arc::new(AppRuntimeSnapshot::new(
            captured.revision(),
            captured.llm().clone(),
            Arc::new(refreshed),
            captured.api_keys.clone(),
            captured.max_iterations(),
        ));
        let mut slot = self.runtime.write().await;
        if Arc::ptr_eq(&captured, &slot) {
            *slot = replacement.clone();
            replacement
        } else {
            // A settings save won the race; never reintroduce cards/config from an old revision.
            slot.clone()
        }
    }
}

/// MCP manager paired with the tool registry that mounts its dynamic tools on top of the
/// built-in base. Both are replaced together on a settings save so a run always sees a coherent
/// (manager, tools) pair.
#[derive(Clone)]
pub struct McpRuntime {
    pub manager: Arc<dss_mcp::MCPServerManager>,
    /// Built-in tools plus mounted `mcp__{server}__{tool}` tools.
    pub tools: Arc<ToolRegistry>,
}

/// Connect the enabled MCP servers and mount their tools on top of `base_tools`. The canonical
/// Agent Registry is a Resources-only authority: its advertised MCP Tools are neither listed nor
/// mounted. Other explicitly configured MCP servers keep the normal Tools behavior. Connection
/// attempts run concurrently within per-server and global budgets, then capabilities are mounted
/// deterministically. Resource discovery tools appear only when at least one completed connection
/// advertises Resources. An unreachable server is simply skipped (persisted config still applies),
/// matching A2A's offline-tolerant behavior.
pub async fn build_mcp_runtime(
    base_tools: Arc<ToolRegistry>,
    servers: &[dss_core::McpServerConfig],
) -> McpRuntime {
    build_mcp_runtime_with_budgets(
        base_tools,
        servers,
        MCP_STARTUP_BUDGET,
        MCP_SERVER_CONNECT_BUDGET,
    )
    .await
}

async fn build_mcp_runtime_with_budgets(
    base_tools: Arc<ToolRegistry>,
    servers: &[dss_core::McpServerConfig],
    startup_budget: Duration,
    per_server_budget: Duration,
) -> McpRuntime {
    let manager = Arc::new(dss_mcp::MCPServerManager::new());
    let mut tools = base_tools.snapshot();
    let enabled_servers: Vec<_> = servers.iter().filter(|server| server.enabled).collect();

    // Poll every configured connection attempt concurrently. A slow or unreachable default
    // Registry must not consume the whole startup budget before an independent ordinary MCP
    // server gets its first poll. Each attempt and the overall batch are independently bounded.
    let attempts = enabled_servers.iter().map(|srv| {
        let manager = manager.clone();
        async move {
            let resources_only = srv.name == dss_core::DEFAULT_AGENT_REGISTRY_NAME;
            let connect_one = async {
                if resources_only {
                    manager.add_server_resources_only(&srv.name, &srv.url).await
                } else {
                    manager.add_server(&srv.name, &srv.url).await
                }
            };
            if tokio::time::timeout(per_server_budget, connect_one)
                .await
                .is_err()
            {
                tracing::warn!(
                    server = %srv.name,
                    budget_secs = per_server_budget.as_secs_f64(),
                    "MCP server connect budget exhausted"
                );
            }
        }
    });
    if tokio::time::timeout(startup_budget, join_all(attempts))
        .await
        .is_err()
    {
        tracing::warn!(
            budget_secs = startup_budget.as_secs_f64(),
            "MCP connect budget exhausted; continuing without remaining servers"
        );
    }

    // Derive both dynamic Tools and the Resources authority from the completed manager snapshot,
    // in configuration order. This keeps model-visible schemas deterministic even though the
    // network attempts above race by design.
    let mut resource_servers = Vec::new();
    for srv in enabled_servers {
        let resources_only = srv.name == dss_core::DEFAULT_AGENT_REGISTRY_NAME;
        let Some(info) = manager.server_info(&srv.name).await else {
            continue;
        };
        if !info.connected {
            continue;
        }
        if info.resources {
            resource_servers.push(srv.name.clone());
        }
        if resources_only {
            tracing::info!(server = %srv.name, "MCP Registry connected for Resources only");
        } else if let Some(mcp_tools) = manager.list_tools(&srv.name).await {
            let count =
                dss_tools::builtin::mcp::register_mcp_tools(&mut tools, &srv.name, &mcp_tools);
            tracing::info!(server = %srv.name, tools = count, "MCP tools mounted");
        }
    }
    let resource_tool_count =
        dss_tools::builtin::mcp::register_resource_tools(&mut tools, &resource_servers);
    if resource_tool_count > 0 {
        tracing::info!(
            servers = resource_servers.len(),
            tools = resource_tool_count,
            "MCP Resources tools mounted"
        );
    }
    McpRuntime {
        manager,
        tools: Arc::new(tools),
    }
}

#[cfg(test)]
mod mcp_runtime_tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use serde_json::{json, Value};

    const SESSION_ID: &str = "concurrent-startup-fixture";

    #[derive(Clone, Copy)]
    struct Fixture {
        delay: Duration,
    }

    async fn mcp_fixture(
        State(fixture): State<Fixture>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Response {
        if !fixture.delay.is_zero() {
            tokio::time::sleep(fixture.delay).await;
        }
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method != "initialize"
            && headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                != Some(SESSION_ID)
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        match method {
            "initialize" => (
                [("mcp-session-id", SESSION_ID)],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id(&request),
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {}},
                        "serverInfo": {"name": "startup-fixture", "version": "1"}
                    }
                })),
            )
                .into_response(),
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            "tools/list" => rpc_result(
                &request,
                json!({
                    "tools": [{
                        "name": "fast_tool",
                        "description": "ordinary MCP tool",
                        "inputSchema": {"type": "object"}
                    }]
                }),
            ),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    fn rpc_id(request: &Value) -> Value {
        request.get("id").cloned().unwrap_or(Value::Null)
    }

    fn rpc_result(request: &Value, result: Value) -> Response {
        Json(json!({
            "jsonrpc": "2.0",
            "id": rpc_id(request),
            "result": result,
        }))
        .into_response()
    }

    async fn spawn_fixture(delay: Duration) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind startup fixture");
        let address = listener.local_addr().expect("startup fixture address");
        let app = Router::new()
            .route("/mcp", post(mcp_fixture))
            .with_state(Fixture { delay });
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve startup fixture");
        });
        format!("http://{address}/mcp")
    }

    #[tokio::test]
    async fn slow_registry_does_not_starve_fast_ordinary_mcp_capabilities() {
        let slow_registry = spawn_fixture(Duration::from_secs(5)).await;
        let fast_ordinary = spawn_fixture(Duration::ZERO).await;
        let mut base = ToolRegistry::new();
        builtin::register_all(&mut base);
        let started = std::time::Instant::now();
        let runtime = build_mcp_runtime_with_budgets(
            Arc::new(base),
            &[
                dss_core::McpServerConfig {
                    name: dss_core::DEFAULT_AGENT_REGISTRY_NAME.into(),
                    url: slow_registry,
                    enabled: true,
                },
                dss_core::McpServerConfig {
                    name: "fast-ordinary".into(),
                    url: fast_ordinary,
                    enabled: true,
                },
            ],
            Duration::from_millis(750),
            Duration::from_millis(500),
        )
        .await;

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(runtime
            .manager
            .server_info(dss_core::DEFAULT_AGENT_REGISTRY_NAME)
            .await
            .is_none());
        let ordinary = runtime
            .manager
            .server_info("fast-ordinary")
            .await
            .expect("fast ordinary server connected");
        assert!(ordinary.connected);
        assert!(ordinary.resources);
        assert!(runtime
            .tools
            .get(&dss_mcp::mcp_tool_name("fast-ordinary", "fast_tool"))
            .is_some());
        assert_eq!(
            runtime
                .tools
                .get(builtin::mcp::MCP_LIST_RESOURCES_TOOL_NAME)
                .expect("resource discovery survives slow Registry")
                .spec()
                .parameters["properties"]["server"]["enum"],
            json!(["fast-ordinary"])
        );
    }
}

/// Build a skill catalog from persisted discovery settings for the given data directory.
pub fn build_skill_catalog(
    data_dir: &std::path::Path,
    skills: &dss_core::SkillSettings,
) -> dss_skills::SkillCatalog {
    let global_dir = dss_skills::global_skills_dir(data_dir);
    let custom_dirs: Vec<std::path::PathBuf> = skills
        .custom_dirs
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .collect();
    dss_skills::build_catalog(
        &global_dir,
        skills.include_claude,
        skills.include_codex,
        skills.include_cursor,
        &custom_dirs,
        &skills.disabled,
    )
}

pub async fn build_state(settings: Settings) -> Result<AppState, dss_db::DbError> {
    // Read once during startup. The value is deliberately never serialized or logged.
    let api_token = std::env::var("DSS_API_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .map(Arc::<str>::from);

    let a2a_client =
        Arc::new(A2aClient::new().map_err(|error| dss_db::DbError::Other(error.to_string()))?);
    let llm_runtime = Arc::new(LlmRuntimeSnapshot::new(
        settings.llm.clone(),
        settings.thinking,
        0,
        settings.llm_env_overrides,
    ));
    if !llm_runtime.is_configured() {
        tracing::warn!(
            "LLM not configured: set DEEPSEEK_API_KEY or settings.json llm.api_key; \
             stream-sse will return kind=error"
        );
    }
    let a2a_runtime = Arc::new(initial_a2a_snapshot(0, &settings.a2a_agents));
    let runtime = Arc::new(AppRuntimeSnapshot::new(
        0,
        llm_runtime.clone(),
        a2a_runtime,
        Arc::new(settings.api_keys.clone()),
        settings.max_iterations,
    ));

    // 基础工具集（仅内置工具）。MCP 动态工具挂载到可热重建的 mcp_runtime 上，不污染这个基座。
    let mut base = ToolRegistry::new();
    builtin::register_all(&mut base);
    let tools = Arc::new(base);

    // DB pool（先建，memory 依赖它）。
    let pool = open_pool(&settings.data_dir)?;
    run_migrations(&pool).await?;
    crate::db::ensure_default_project(&pool).await?;
    let db = Arc::new(pool);

    // Skill 目录：builtin → global（首跑 seed）→ 可选 claude/codex/cursor + custom。project 源在
    // stream_sse 按 workspace 叠加。禁用集合过滤 agent 可见性。配置来自 settings.json 的 `skills`。
    let global_dir = dss_skills::global_skills_dir(&settings.data_dir);
    dss_skills::seed_builtin_to_global(&global_dir);
    let skill_settings = settings.reload_persisted_skills().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load skill settings; using defaults");
        dss_core::SkillSettings::default()
    });
    let catalog = build_skill_catalog(&settings.data_dir, &skill_settings);
    let catalog = Arc::new(RwLock::new(Arc::new(catalog)));

    let memory = Arc::new(dss_memory::MemoryStore::new(db.clone()));
    let logs = Arc::new(dss_observability::LogStore::new(db.clone()));

    // MCP：连接 settings.json 配置的 server 并把工具挂到 base 之上；结果放进可热重建的 mcp_runtime。
    // 保存 MCP 设置时会重连并原子替换这个指针，无需重启。
    let mcp_runtime = build_mcp_runtime(tools.clone(), &settings.mcp_servers).await;
    let mcp_runtime = Arc::new(RwLock::new(mcp_runtime));

    // 启动日志（system source）。
    let _ = logs
        .append(dss_observability::LogEntry {
            level: "info".into(),
            source: "system".into(),
            kind: "startup".into(),
            session_id: None,
            frame_id: None,
            iteration: None,
            message: format!("dss-backend started (model={})", settings.llm.model),
            detail: Some(serde_json::json!({
                "version": dss_api_crate_version(),
                "data_dir": settings.data_dir.display().to_string(),
                "llm_configured": settings.llm.is_configured(),
                "a2a_agent_count": settings.a2a_agents.len(),
            })),
        })
        .await;

    // 后台保留策略 sweep（D-T07）：启动跑一次 + 每 6h 循环。
    // 同时激活 memory retention（crates/dss-memory retention::sweep 幂等）。
    spawn_retention_loop(logs.clone(), memory.clone(), settings.log.clone());

    Ok(AppState {
        settings: Arc::new(settings),
        api_token,
        runtime: Arc::new(RwLock::new(runtime)),
        settings_save_lock: Arc::new(Mutex::new(())),
        a2a_client,
        tools,
        catalog,
        memory,
        logs,
        mcp_runtime,
        db,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        session_restore_lock: Arc::new(Mutex::new(())),
        run_controls: Arc::new(RunControlRegistry::default()),
    })
}

/// 后台保留策略 sweep（D-T07）。
///
/// 启动跑一次 + 每 6 小时循环：
/// - **日志**：按天删（`ts < now - retention_days`）+ 按量删（超过 `max_rows` 删最旧的）。
/// - **记忆**：激活 `dss-memory` 的 retention sweep（valid_until 过期 → expired；
///   长期未召回 + 低置信 → candidate）。幂等。
///
/// 每次 sweep 结果写一条 `source=system, kind=retention_sweep` 日志（observability）。
/// 配置取启动时的快照；settings 热更新后保留任务沿用旧值（接受，memory retention 同模式）。
fn spawn_retention_loop(
    logs: Arc<dss_observability::LogStore>,
    memory: Arc<dss_memory::MemoryStore>,
    log_cfg: dss_core::settings::LogSettings,
) {
    use std::time::Duration as StdDuration;

    const SWEEP_INTERVAL: StdDuration = StdDuration::from_secs(6 * 60 * 60); // 6h

    tokio::spawn(async move {
        // 启动后稍等，避开启动日志洪峰与 MCP 连接竞争。
        tokio::time::sleep(StdDuration::from_secs(15)).await;

        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        // interval 首次 tick 立即返回（即启动首轮）；上面已 sleep 15s。
        ticker.tick().await;

        loop {
            run_one_retention_sweep(&logs, &memory, &log_cfg).await;
            ticker.tick().await;
        }
    });
}

/// 跑一次保留策略 sweep：日志 prune + memory retention，结果写一条 system 日志。
async fn run_one_retention_sweep(
    logs: &Arc<dss_observability::LogStore>,
    memory: &Arc<dss_memory::MemoryStore>,
    log_cfg: &dss_core::settings::LogSettings,
) {
    use chrono::{Duration, Utc};

    let now = Utc::now();
    let now_iso = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let log_before_iso = now - Duration::days(log_cfg.retention_days as i64);

    // 1) 日志 prune（D-T07：按天 + 按量）。
    let log_stats = logs
        .prune(
            log_before_iso.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            log_cfg.max_rows,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "log prune failed");
            dss_db::repo::PruneStats::default()
        });

    // 2) memory retention sweep（激活幂等 sweep）。
    let mem_cfg = dss_memory::retention::RetentionConfig::default();
    let mem_stale_iso = (now - Duration::days(mem_cfg.stale_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mem_stats =
        dss_memory::retention::sweep(memory.as_ref(), &mem_cfg, &now_iso, &mem_stale_iso).await;

    // 3) 结果写一条 system 日志（observability）。
    let _ = logs
        .append(dss_observability::LogEntry {
            level: "info".into(),
            source: "system".into(),
            kind: "retention_sweep".into(),
            session_id: None,
            frame_id: None,
            iteration: None,
            message: format!(
                "retention sweep: logs pruned {} (age) + {} (count); memory expired {} demoted {} errors {}",
                log_stats.by_age,
                log_stats.by_count,
                mem_stats.expired,
                mem_stats.demoted_to_candidate,
                mem_stats.errors,
            ),
            detail: Some(serde_json::json!({
                "logs_by_age": log_stats.by_age,
                "logs_by_count": log_stats.by_count,
                "memory_expired": mem_stats.expired,
                "memory_demoted": mem_stats.demoted_to_candidate,
                "memory_errors": mem_stats.errors,
            })),
        })
        .await;
}

fn initial_a2a_snapshot(revision: u64, configs: &[dss_core::A2aAgentConfig]) -> A2aRuntimeSnapshot {
    let agents = configs
        .iter()
        .cloned()
        .map(|config| {
            let validation_error = validate_a2a_config(&config)
                .err()
                .map(|error| error.to_string());
            let status = if !config.enabled {
                AgentRuntimeStatus::Disabled
            } else if validation_error.is_some() {
                AgentRuntimeStatus::Invalid
            } else {
                AgentRuntimeStatus::Unchecked
            };
            AgentRuntime {
                config,
                status,
                card: None,
                last_error: validation_error,
                last_refreshed_at: None,
            }
        })
        .collect();
    A2aRuntimeSnapshot { revision, agents }
}

fn dss_api_crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
