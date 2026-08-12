//! 配置加载：defaults → config.toml → settings.json → env (DSS_*)。
//!
//! 优先级：`env (DSS_*) > settings.json > config.toml > defaults`。
//! 配置文件位于 `<data_dir>/config.toml` 与 `<data_dir>/settings.json`；
//! data_dir 本身只由 `DSS_DATA_DIR` 或默认值决定（不参与配置文件加载，避免自引用）。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
use crate::paths;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 17896;
pub const DEFAULT_LLM_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_LLM_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_A2A_TIMEOUT_SECONDS: u64 = 120;
/// Stable authority name for the built-in Agent Registry MCP server.
pub const DEFAULT_AGENT_REGISTRY_NAME: &str = "agent-registry";
/// Canonical endpoint for the built-in Agent Registry MCP server.
pub const DEFAULT_AGENT_REGISTRY_URL: &str = "https://a2a-dev.intern-ai.org.cn/mcp";
/// Default per-run Agent iteration budget when neither settings file specifies one.
pub const DEFAULT_MAX_ITERATIONS: u32 = 100;
/// Smallest useful per-run Agent iteration budget.
pub const MIN_MAX_ITERATIONS: u32 = 1;
/// Hard safety ceiling for a user-configurable per-run Agent iteration budget.
pub const MAX_CONFIGURABLE_ITERATIONS: u32 = 1_000;

/// Validate the shared persisted/API contract for the per-run Agent iteration budget.
pub fn validate_max_iterations(value: u32) -> Result<(), &'static str> {
    if (MIN_MAX_ITERATIONS..=MAX_CONFIGURABLE_ITERATIONS).contains(&value) {
        Ok(())
    } else {
        Err("max_iterations must be between 1 and 1000")
    }
}

/// Provider reasoning effort exposed by the product. The deliberately small
/// set is supported by DeepSeek V4 and the currently recognized OpenAI models.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
    Low,
    #[default]
    High,
    Max,
}

impl ThinkingEffort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// Hot, run-scoped reasoning policy. This controls one provider request and is
/// independent from `max_iterations`, which bounds the Agent/tool loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThinkingSettings {
    pub enabled: bool,
    pub effort: ThinkingEffort,
}

impl Default for ThinkingSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            effort: ThinkingEffort::High,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    /// data_dir 是否为默认值（未设 `DSS_DATA_DIR`）；决定是否允许 SSD 软链。
    pub data_dir_is_default: bool,
    /// 每次 Agent run 的最大 LLM/工具迭代轮数。
    pub max_iterations: u32,
    /// 每个 provider 请求的思考开关和深度。
    pub thinking: ThinkingSettings,
    pub server: ServerSettings,
    pub llm: LlmSettings,
    /// 持久化的多 provider 列表（供设置界面读写）。
    pub providers: Vec<LlmProvider>,
    /// LLM fields whose effective values came from process environment variables.
    pub llm_env_overrides: LlmEnvOverrides,
    /// 日志级别 filter（`DSS_LOG`/`RUST_LOG` 之外来自配置文件的兜底）。
    pub log_level: Option<String>,
    /// MCP server 配置（P7）：启动时尝试连接。
    pub mcp_servers: Vec<McpServerConfig>,
    /// 远端 A2A Agent 配置。这里只保存客户端连接信息；本应用不暴露 A2A server。
    pub a2a_agents: Vec<A2aAgentConfig>,
    /// 记忆系统配置（抽取/巩固/召回）。
    pub memory: MemorySettings,
    /// 日志保留策略（按天 + 按量双限制）。
    pub log: LogSettings,
    /// 数据源 API keys（OPENALEX_API_KEY 等）。工具通过 ToolContext.api_keys 读取。
    pub api_keys: HashMap<String, String>,
}

/// 一个用户显式信任并配置的远端 A2A Agent。
///
/// `bearer_token` 仅写入权限受限的 settings.json；自定义 Debug 永不输出明文。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct A2aAgentConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    pub timeout_seconds: u64,
}

impl Default for A2aAgentConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            endpoint: String::new(),
            enabled: true,
            bearer_token: None,
            timeout_seconds: DEFAULT_A2A_TIMEOUT_SECONDS,
        }
    }
}

impl std::fmt::Debug for A2aAgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2aAgentConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("enabled", &self.enabled)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

/// Skill 发现配置：控制内置 skill 的启用/禁用、是否纳入 claude/codex/cursor 目录、以及自定义目录。
///
/// 持久化在 `settings.json` 的 `"skills"` 键下（`config.toml` 可作为低优先级默认层）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillSettings {
    /// 被禁用的 skill 名称（禁用后不会进入 agent 的 search/list/read）。
    pub disabled: Vec<String>,
    /// 纳入 `~/.claude/skills` 目录。
    pub include_claude: bool,
    /// 纳入 `~/.codex/skills` 目录。
    pub include_codex: bool,
    /// 纳入 `~/.cursor/skills-cursor`（及 `~/.cursor/skills`）目录。
    pub include_cursor: bool,
    /// 额外的自定义 skill 目录（绝对路径）。
    pub custom_dirs: Vec<String>,
}

/// 记忆系统配置。持久化在 `settings.json` 的 `"memory"` 键下。
///
/// 控制抽取/巩固/召回行为。f64 字段使此结构无法 derive Eq，用 PartialEq。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    /// 是否启用记忆抽取/召回（false = 完全关闭记忆功能）。
    pub enabled: bool,
    /// 抽取用的模型（None = 用主模型 settings.llm.model）。
    pub extract_model: Option<String>,
    /// 自动晋升为 active 的最低 confidence（低于则进 candidate 待审）。
    pub auto_promote_threshold: f64,
    /// BM25 近似重复判定阈值（0..1，越高越严格）。
    pub dedupe_similarity: f64,
    /// 高风险（preference/decision）是否一律进 candidate 等审批（true=审批制）。
    pub trust_high_risk_approve: bool,
    /// 是否把高价值 profile 记忆注入 system prefix（默认 false：保护前缀缓存命中）。
    pub always_on: bool,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            extract_model: None,
            auto_promote_threshold: 0.5,
            dedupe_similarity: 0.85,
            trust_high_risk_approve: true,
            always_on: false,
        }
    }
}

/// 日志保留策略（D-T07）。按天 + 按量双限制，先到先清。
///
/// 默认 14 天、10 万条；后台 sweep（启动一次 + 每 6h）幂等执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// 超过此天数的日志自动删除。
    pub retention_days: u32,
    /// 日志总条数上限，超过则删最旧的。
    pub max_rows: u32,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_LOG_RETENTION_DAYS,
            max_rows: DEFAULT_LOG_MAX_ROWS,
        }
    }
}

/// 默认日志保留天数（D-T07）。
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 14;
/// 默认日志最大条数（D-T07）。
pub const DEFAULT_LOG_MAX_ROWS: u32 = 100_000;

/// MCP server 配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub enabled: bool,
}

/// Validate the persisted/public MCP authority contract. Endpoint URLs are
/// configuration, not a secret transport; embedded credentials and query-token
/// forms are rejected so settings readback and diagnostics cannot leak them.
pub fn validate_mcp_servers(servers: &[McpServerConfig]) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for server in servers {
        let name = server.name.trim();
        if name.is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
            || name != server.name
        {
            return Err("each MCP server needs a bounded safe name".into());
        }
        if name.eq_ignore_ascii_case(DEFAULT_AGENT_REGISTRY_NAME)
            && name != DEFAULT_AGENT_REGISTRY_NAME
        {
            return Err(format!(
                "reserved MCP server name must use the canonical spelling: {DEFAULT_AGENT_REGISTRY_NAME}"
            ));
        }
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("duplicate MCP server name: {name}"));
        }
        if server.url.is_empty()
            || server.url.len() > 2_048
            || server.url.chars().any(char::is_control)
        {
            return Err(format!(
                "MCP server {name} URL is empty, too long, or unsafe"
            ));
        }
        let url = url::Url::parse(&server.url)
            .map_err(|_| format!("MCP server {name} URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(format!(
                "MCP server {name} URL must be absolute http or https"
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(format!(
                "MCP server {name} URL credentials, query, and fragment are forbidden"
            ));
        }
    }
    Ok(())
}

/// Backend MCP defaults used by every settings resolver.
///
/// File layers remain authoritative: an explicit list replaces this seed and an explicit empty
/// list opts out completely.
pub fn default_mcp_servers() -> Vec<McpServerConfig> {
    vec![McpServerConfig {
        name: DEFAULT_AGENT_REGISTRY_NAME.to_owned(),
        url: DEFAULT_AGENT_REGISTRY_URL.to_owned(),
        enabled: true,
    }]
}

#[derive(Debug, Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
        }
    }
}

/// LLM provider 配置（OpenAI 兼容；Deepseek 为默认）。
#[derive(Clone)]
pub struct LlmSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

// api_key 不进入 Debug 输出（防日志泄露）。
impl std::fmt::Debug for LlmSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSettings")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_LLM_BASE_URL.to_string(),
            model: DEFAULT_LLM_MODEL.to_string(),
            api_key: None,
        }
    }
}

impl LlmSettings {
    pub fn is_configured(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }
}

/// 单个 LLM provider 条目（持久化层支持多 provider，但运行时只能启用一个）。
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub enabled: bool,
}

// api_key 不进入 Debug 输出（防日志泄露）。
impl std::fmt::Debug for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmProvider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl LlmProvider {
    pub fn is_configured(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }
}

/// Process environment variables that currently take precedence over persisted LLM fields.
/// Values are deliberately represented only as booleans so credentials cannot leak through
/// status APIs or debug output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmEnvOverrides {
    pub base_url: bool,
    pub model: bool,
    pub api_key: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedLlmSettings {
    pub llm: LlmSettings,
    pub env_overrides: LlmEnvOverrides,
}

/// 配置文件的部分表示：所有字段可选，用于分层合并。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileSettings {
    /// Missing values preserve the lower-priority layer; an explicit value replaces it.
    max_iterations: Option<u32>,
    /// Member-wise layered so a higher-priority file can override only the
    /// switch or only the effort without resetting the other member.
    #[serde(default)]
    thinking: FileThinkingSettings,
    #[serde(default)]
    server: FileServerSettings,
    #[serde(default)]
    llm: FileLlmSettings,
    #[serde(default)]
    log_level: Option<String>,
    /// `Option` is intentional: an explicit `[]` in the higher-priority file clears inherited
    /// servers, while an absent field leaves the lower layer untouched (mirrors `a2a_agents`).
    mcp_servers: Option<Vec<McpServerConfig>>,
    /// `Option` is intentional: an explicit `[]` in the higher-priority file clears
    /// inherited Agents, while an absent field leaves the lower layer untouched.
    a2a_agents: Option<Vec<A2aAgentConfig>>,
    /// 多 provider 列表。显式 `[]` 可清空继承的 provider；缺失则保留低优先级层。
    /// 为空时回退到 legacy `llm` 对象。
    providers: Option<Vec<FileLlmProvider>>,
    /// 记忆系统配置（整体替换语义：高优先级文件的 memory 对象覆盖低层）。
    #[serde(default)]
    memory: Option<MemorySettings>,
    /// 日志保留策略（整体替换语义：高优先级文件的 log 对象覆盖低层）。
    #[serde(default)]
    log: Option<LogSettings>,
    /// 数据源 API keys（OPENALEX_API_KEY 等）。整体替换语义。
    #[serde(default)]
    api_keys: Option<HashMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileServerSettings {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileThinkingSettings {
    #[serde(default, deserialize_with = "deserialize_present")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    effort: Option<ThinkingEffort>,
}

/// `Option<T>` normally treats an explicit JSON null like an omitted field.
/// Settings use omission for inheritance, so a present null must instead be a
/// type error and cannot silently reset or preserve a lower layer.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileLlmSettings {
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileLlmProvider {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// Minimal wrapper to extract only the `skills` object from a config file, ignoring all other
/// top-level keys. A missing `skills` key deserializes to `None`.
#[derive(Debug, Default, Deserialize)]
struct SkillsFileWrapper {
    #[serde(default)]
    skills: Option<SkillSettings>,
}

impl Settings {
    /// 按优先级加载配置：`defaults → <data_dir>/config.toml → <data_dir>/settings.json → env`。
    pub fn load() -> Result<Self, Error> {
        let (data_dir, data_dir_is_default) = paths::resolve_data_dir()?;

        Self::load_from_data_dir(data_dir, data_dir_is_default)
    }

    /// Reload only the effective LLM settings for this process' existing data directory.
    ///
    /// This follows the exact same file layering and environment-variable precedence as a
    /// backend restart, without changing startup-only server, logging, or MCP state.
    pub fn reload_llm(&self) -> Result<LlmSettings, Error> {
        Self::load_from_data_dir(self.data_dir.clone(), self.data_dir_is_default)
            .map(|settings| settings.llm)
    }

    /// Reload the editable file-backed LLM fallback without applying process environment
    /// overrides. Settings UIs must display this view so saving an unrelated field cannot
    /// accidentally copy an environment-owned runtime value into `settings.json`.
    pub fn reload_persisted_llm(&self) -> Result<LlmSettings, Error> {
        let settings_json = self.data_dir.join("settings.json");
        let candidate = if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?
        } else {
            serde_json::json!({})
        };
        self.resolve_persisted_candidate_llm(candidate)
    }

    /// Reload the editable file-backed A2A Agent list without any network access.
    pub fn reload_persisted_a2a_agents(&self) -> Result<Vec<A2aAgentConfig>, Error> {
        let settings_json = self.data_dir.join("settings.json");
        let candidate = if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?
        } else {
            serde_json::json!({})
        };
        self.resolve_persisted_candidate_a2a_agents(candidate)
    }

    /// Reload the editable file-backed provider list without applying process environment
    /// overrides.
    pub fn reload_persisted_providers(&self) -> Result<Vec<LlmProvider>, Error> {
        let settings_json = self.data_dir.join("settings.json");
        let candidate = if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?
        } else {
            serde_json::json!({})
        };
        self.resolve_persisted_candidate_providers(candidate)
    }

    /// Resolve the file-backed provider list represented by a candidate `settings.json`.
    pub fn resolve_persisted_candidate_providers(
        &self,
        candidate: Value,
    ) -> Result<Vec<LlmProvider>, Error> {
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers = default_mcp_servers();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();
        let mut providers: Vec<LlmProvider> = Vec::new();

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
                &mut providers,
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json.clone(),
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
            &mut providers,
        );
        if providers.is_empty() {
            providers = providers_from_legacy_llm(&llm);
        }
        Ok(providers)
    }

    /// Resolve the file-backed LLM fallback represented by a candidate `settings.json`,
    /// deliberately stopping before environment precedence is applied.
    pub fn resolve_persisted_candidate_llm(&self, candidate: Value) -> Result<LlmSettings, Error> {
        let providers = self.resolve_persisted_candidate_providers(candidate)?;
        // 从 providers 中找启用项；providers 为空时内部已回退 legacy llm，因此这里直接取第一个启用即可。
        Ok(providers
            .into_iter()
            .find(|p| p.enabled)
            .map(|p| LlmSettings {
                base_url: p.base_url,
                model: p.model,
                api_key: p.api_key,
            })
            .unwrap_or_else(LlmSettings::default))
    }

    /// Resolve the file-backed A2A list represented by a candidate `settings.json`.
    /// This mirrors restart layering and deliberately performs no card discovery.
    pub fn resolve_persisted_candidate_a2a_agents(
        &self,
        candidate: Value,
    ) -> Result<Vec<A2aAgentConfig>, Error> {
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers = default_mcp_servers();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();
        let mut providers: Vec<LlmProvider> = Vec::new();

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
                &mut providers,
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json.clone(),
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
            &mut providers,
        );
        Ok(a2a_agents)
    }

    /// Reload the editable file-backed MCP server list without any network connection attempt.
    pub fn reload_persisted_mcp_servers(&self) -> Result<Vec<McpServerConfig>, Error> {
        let settings_json = self.data_dir.join("settings.json");
        let candidate = if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?
        } else {
            serde_json::json!({})
        };
        self.resolve_persisted_candidate_mcp_servers(candidate)
    }

    /// Resolve the file-backed MCP server list represented by a candidate `settings.json`.
    /// Mirrors restart layering (config.toml defaults, settings.json overrides) with no I/O
    /// against the servers themselves.
    pub fn resolve_persisted_candidate_mcp_servers(
        &self,
        candidate: Value,
    ) -> Result<Vec<McpServerConfig>, Error> {
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers = default_mcp_servers();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();
        let mut providers: Vec<LlmProvider> = Vec::new();

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
                &mut providers,
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json.clone(),
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
            &mut providers,
        );
        validate_mcp_servers(&mcp_servers).map_err(|message| Error::ConfigParse {
            path: settings_json,
            message,
        })?;
        Ok(mcp_servers)
    }

    /// Reload the file-backed Skill discovery configuration without any filesystem scan of the
    /// skill directories themselves.
    pub fn reload_persisted_skills(&self) -> Result<SkillSettings, Error> {
        let settings_json = self.data_dir.join("settings.json");
        let candidate = if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?
        } else {
            serde_json::json!({})
        };
        self.resolve_persisted_candidate_skills(candidate)
    }

    /// Resolve the file-backed Skill configuration represented by a candidate `settings.json`.
    /// Layering mirrors restart precedence: `config.toml` supplies defaults, and a `skills`
    /// object present in settings.json (the candidate) overrides it wholesale.
    pub fn resolve_persisted_candidate_skills(
        &self,
        candidate: Value,
    ) -> Result<SkillSettings, Error> {
        let mut skills = SkillSettings::default();

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let wrapper: SkillsFileWrapper =
                toml::from_str(&text).map_err(|e| Error::ConfigParse {
                    path: config_toml.clone(),
                    message: e.to_string(),
                })?;
            if let Some(section) = wrapper.skills {
                skills = section;
            }
        }

        if let Some(section) = candidate.get("skills") {
            skills = serde_json::from_value(section.clone()).map_err(|e| Error::ConfigParse {
                path: self.data_dir.join("settings.json"),
                message: e.to_string(),
            })?;
        }

        Ok(skills)
    }

    /// Resolve the effective LLM configuration a restart would produce if `candidate` were the
    /// complete contents of settings.json. This performs all fallible parsing before callers
    /// durably replace the file.
    pub fn resolve_candidate_llm(&self, candidate: Value) -> Result<ResolvedLlmSettings, Error> {
        let mut llm = self.resolve_persisted_candidate_llm(candidate)?;
        let mut server = ServerSettings::default();
        let mut log_level = None;

        let env_overrides = apply_process_environment(&mut server, &mut llm, &mut log_level);
        Ok(ResolvedLlmSettings { llm, env_overrides })
    }

    /// Resolve the effective provider list a restart would load if `candidate` were the complete
    /// contents of settings.json. Environment overrides are **not** applied to the returned list;
    /// they only affect the final effective `LlmSettings`.
    pub fn resolve_candidate_providers(&self, candidate: Value) -> Result<Vec<LlmProvider>, Error> {
        self.resolve_persisted_candidate_providers(candidate)
    }

    /// Resolve the per-run Agent iteration budget a restart would load if `candidate` were the
    /// complete contents of settings.json. This deliberately mirrors file-layer precedence and
    /// validates both the lower-priority config.toml value and the candidate override.
    pub fn resolve_candidate_max_iterations(&self, candidate: Value) -> Result<u32, Error> {
        let mut max_iterations = DEFAULT_MAX_ITERATIONS;

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_max_iterations(&mut max_iterations, &config_toml)?;
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json.clone(),
                message: e.to_string(),
            })?;
        file.apply_max_iterations(&mut max_iterations, &settings_json)?;

        Ok(max_iterations)
    }

    /// Resolve the reasoning policy a restart would load for a complete
    /// candidate `settings.json`, including member-wise TOML inheritance.
    pub fn resolve_candidate_thinking(&self, candidate: Value) -> Result<ThinkingSettings, Error> {
        let mut thinking = ThinkingSettings::default();

        let config_toml = self.data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_thinking(&mut thinking);
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?;
        file.apply_thinking(&mut thinking);
        Ok(thinking)
    }

    fn load_from_data_dir(data_dir: PathBuf, data_dir_is_default: bool) -> Result<Self, Error> {
        let mut max_iterations = DEFAULT_MAX_ITERATIONS;
        let mut thinking = ThinkingSettings::default();
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers = default_mcp_servers();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();
        let mut providers: Vec<LlmProvider> = Vec::new();
        let mut memory = MemorySettings::default();
        let mut log = LogSettings::default();
        let mut api_keys: HashMap<String, String> = HashMap::new();

        // config.toml（低优先级文件）
        let config_toml = data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            let file_memory = file.memory.clone();
            let file_log = file.log.clone();
            let file_api_keys = file.api_keys.clone();
            file.apply_max_iterations(&mut max_iterations, &config_toml)?;
            file.apply_thinking(&mut thinking);
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
                &mut providers,
            );
            if let Some(m) = file_memory {
                memory = m;
            }
            if let Some(l) = file_log {
                log = l;
            }
            if let Some(k) = file_api_keys {
                api_keys = k;
            }
        }

        // settings.json（高优先级文件）
        let settings_json = data_dir.join("settings.json");
        if settings_json.is_file() {
            let text = std::fs::read_to_string(&settings_json)?;
            let file: FileSettings =
                serde_json::from_str(&text).map_err(|e| Error::ConfigParse {
                    path: settings_json.clone(),
                    message: e.to_string(),
                })?;
            // memory 整体替换语义（高优先级文件的 memory 覆盖低层）。
            let file_memory = file.memory.clone();
            let file_log = file.log.clone();
            let file_api_keys = file.api_keys.clone();
            file.apply_max_iterations(&mut max_iterations, &settings_json)?;
            file.apply_thinking(&mut thinking);
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
                &mut providers,
            );
            if let Some(m) = file_memory {
                memory = m;
            }
            if let Some(l) = file_log {
                log = l;
            }
            if let Some(k) = file_api_keys {
                api_keys = k;
            }
        }

        // 没有显式 providers 列表时，从 legacy llm 对象生成一个兼容 provider。
        if providers.is_empty() {
            providers = providers_from_legacy_llm(&llm);
        }
        // 运行时生效的 LLM 配置：启用列表中的第一个；都没有启用则回退 legacy llm。
        llm = resolve_effective_llm(&providers, &llm);

        // env（最高优先级）
        let llm_env_overrides = apply_process_environment(&mut server, &mut llm, &mut log_level);

        validate_mcp_servers(&mcp_servers).map_err(|message| Error::ConfigParse {
            path: data_dir.join("settings.json"),
            message,
        })?;

        Ok(Settings {
            data_dir,
            data_dir_is_default,
            max_iterations,
            thinking,
            server,
            llm,
            providers,
            llm_env_overrides,
            log_level,
            mcp_servers,
            a2a_agents,
            memory,
            log,
            api_keys,
        })
    }
}

/// Apply process environment precedence and return only provenance, never values.
fn apply_process_environment(
    server: &mut ServerSettings,
    llm: &mut LlmSettings,
    log_level: &mut Option<String>,
) -> LlmEnvOverrides {
    if let Some(host) = std::env::var_os("DSS_HOST") {
        server.host = host.to_string_lossy().into_owned();
    }
    if let Ok(port) = std::env::var("DSS_PORT") {
        if let Ok(port) = port.parse::<u16>() {
            server.port = port;
        }
    }

    let mut overrides = LlmEnvOverrides::default();
    // LLM：DEEPSEEK_API_KEY / DSS_LLM_BASE_URL / DSS_LLM_MODEL 覆盖配置文件。
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        if !key.is_empty() {
            overrides.api_key = true;
            llm.api_key = Some(key);
        }
    }
    if let Ok(base_url) = std::env::var("DSS_LLM_BASE_URL") {
        if !base_url.is_empty() {
            overrides.base_url = true;
            llm.base_url = base_url;
        }
    }
    if let Ok(model) = std::env::var("DSS_LLM_MODEL") {
        if !model.is_empty() {
            overrides.model = true;
            llm.model = model;
        }
    }
    if let Ok(level) = std::env::var("DSS_LOG").or_else(|_| std::env::var("RUST_LOG")) {
        *log_level = Some(level);
    }
    overrides
}

impl FileLlmProvider {
    fn into_provider(self) -> LlmProvider {
        let name = self.name.unwrap_or_else(|| "DeepSeek".to_string());
        let id = self
            .id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| name.to_lowercase().replace(' ', "_"));
        let id = if id.is_empty() {
            "provider".to_string()
        } else {
            id
        };
        LlmProvider {
            id,
            name,
            base_url: self
                .base_url
                .unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string()),
            model: self.model.unwrap_or_else(|| DEFAULT_LLM_MODEL.to_string()),
            api_key: self.api_key.filter(|k| !k.is_empty()),
            enabled: self.enabled.unwrap_or(true),
        }
    }
}

/// 从 provider 列表中选出当前启用的项；没有则回退到 legacy `llm`。
fn resolve_effective_llm(providers: &[LlmProvider], legacy_llm: &LlmSettings) -> LlmSettings {
    providers
        .iter()
        .find(|p| p.enabled)
        .map(|p| LlmSettings {
            base_url: p.base_url.clone(),
            model: p.model.clone(),
            api_key: p.api_key.clone(),
        })
        .unwrap_or_else(|| legacy_llm.clone())
}

/// 用 legacy `llm` 设置生成一个默认 provider（向后兼容）。
fn providers_from_legacy_llm(legacy_llm: &LlmSettings) -> Vec<LlmProvider> {
    vec![LlmProvider {
        id: "deepseek".to_string(),
        name: "DeepSeek".to_string(),
        base_url: legacy_llm.base_url.clone(),
        model: legacy_llm.model.clone(),
        api_key: legacy_llm.api_key.clone(),
        enabled: true,
    }]
}

impl FileSettings {
    fn apply_thinking(&self, thinking: &mut ThinkingSettings) {
        if let Some(enabled) = self.thinking.enabled {
            thinking.enabled = enabled;
        }
        if let Some(effort) = self.thinking.effort {
            thinking.effort = effort;
        }
    }

    fn apply_max_iterations(
        &self,
        max_iterations: &mut u32,
        source_path: &std::path::Path,
    ) -> Result<(), Error> {
        let Some(value) = self.max_iterations else {
            return Ok(());
        };
        validate_max_iterations(value).map_err(|message| Error::ConfigParse {
            path: source_path.to_path_buf(),
            message: message.to_string(),
        })?;
        *max_iterations = value;
        Ok(())
    }

    fn apply_to(
        self,
        server: &mut ServerSettings,
        llm: &mut LlmSettings,
        log_level: &mut Option<String>,
        mcp_servers: &mut Vec<McpServerConfig>,
        a2a_agents: &mut Vec<A2aAgentConfig>,
        providers: &mut Vec<LlmProvider>,
    ) {
        if let Some(host) = self.server.host {
            server.host = host;
        }
        if let Some(port) = self.server.port {
            server.port = port;
        }
        if let Some(base_url) = self.llm.base_url {
            llm.base_url = base_url;
        }
        if let Some(model) = self.llm.model {
            llm.model = model;
        }
        if let Some(api_key) = self.llm.api_key {
            if !api_key.is_empty() {
                llm.api_key = Some(api_key);
            }
        }
        if self.log_level.is_some() {
            *log_level = self.log_level;
        }
        // mcp_servers：显式列表覆盖（含空列表清空）；缺省则保留低优先级层。
        if let Some(configured) = self.mcp_servers {
            *mcp_servers = configured;
        }
        if let Some(configured) = self.a2a_agents {
            *a2a_agents = configured;
        }
        if let Some(configured) = self.providers {
            *providers = configured
                .into_iter()
                .map(FileLlmProvider::into_provider)
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        default_mcp_servers, validate_mcp_servers, A2aAgentConfig, LlmSettings, McpServerConfig,
        Settings, ThinkingEffort, ThinkingSettings, DEFAULT_AGENT_REGISTRY_NAME,
        DEFAULT_AGENT_REGISTRY_URL, DEFAULT_LLM_MODEL, DEFAULT_MAX_ITERATIONS,
        MAX_CONFIGURABLE_ITERATIONS, MIN_MAX_ITERATIONS,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "dss-core-settings-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temporary settings directory");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn llm_defaults_to_current_deepseek_v4_flash_model() {
        let settings = LlmSettings::default();
        assert_eq!(DEFAULT_LLM_MODEL, "deepseek-v4-flash");
        assert_eq!(settings.model, DEFAULT_LLM_MODEL);
    }

    #[test]
    fn agent_registry_is_an_opt_out_whole_list_default() {
        let test_dir = TestDir::new("agent-registry-layering");

        let defaults = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load MCP defaults");
        assert_eq!(defaults.mcp_servers.len(), 1);
        assert_eq!(defaults.mcp_servers[0].name, DEFAULT_AGENT_REGISTRY_NAME);
        assert_eq!(defaults.mcp_servers[0].url, DEFAULT_AGENT_REGISTRY_URL);
        assert!(defaults.mcp_servers[0].enabled);
        assert_eq!(
            defaults
                .resolve_persisted_candidate_mcp_servers(serde_json::json!({}))
                .expect("resolve an omitted higher-priority MCP list")
                .len(),
            1
        );

        std::fs::write(
            test_dir.path().join("config.toml"),
            r#"
[[mcp_servers]]
name = "custom"
url = "https://custom.example.invalid/mcp"
enabled = true
"#,
        )
        .expect("write custom MCP config layer");
        std::fs::write(test_dir.path().join("settings.json"), "{}")
            .expect("write omitted MCP settings layer");
        let from_config = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load custom MCP config layer");
        assert_eq!(from_config.mcp_servers.len(), 1);
        assert_eq!(from_config.mcp_servers[0].name, "custom");

        std::fs::write(
            test_dir.path().join("settings.json"),
            r#"{
  "mcp_servers": [{
    "name": "agent-registry",
    "url": "https://user-registry.example.invalid/mcp",
    "enabled": false
  }]
}"#,
        )
        .expect("write same-name MCP settings override");
        let same_name_override = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load same-name MCP override");
        assert_eq!(same_name_override.mcp_servers.len(), 1);
        assert_eq!(
            same_name_override.mcp_servers[0].name,
            DEFAULT_AGENT_REGISTRY_NAME
        );
        assert_eq!(
            same_name_override.mcp_servers[0].url,
            "https://user-registry.example.invalid/mcp"
        );
        assert!(!same_name_override.mcp_servers[0].enabled);

        std::fs::write(
            test_dir.path().join("settings.json"),
            r#"{"mcp_servers":[]}"#,
        )
        .expect("write explicit MCP opt-out");
        let opted_out = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load explicit MCP opt-out");
        assert!(opted_out.mcp_servers.is_empty());
    }

    #[test]
    fn mcp_urls_are_public_configuration_not_secret_containers() {
        assert!(validate_mcp_servers(&default_mcp_servers()).is_ok());
        for url in [
            "https://user:secret@example.test/mcp",
            "https://example.test/mcp?api_key=secret",
            "https://example.test/mcp#secret",
            "file:///tmp/mcp.sock",
        ] {
            assert!(
                validate_mcp_servers(&[McpServerConfig {
                    name: "external".into(),
                    url: url.into(),
                    enabled: true,
                }])
                .is_err(),
                "unsafe MCP URL unexpectedly accepted: {url}"
            );
        }
        for name in [" agent-registry", "agent-registry ", "Agent-Registry"] {
            assert!(validate_mcp_servers(&[McpServerConfig {
                name: name.into(),
                url: DEFAULT_AGENT_REGISTRY_URL.into(),
                enabled: true,
            }])
            .is_err());
        }
    }

    #[test]
    fn max_iterations_defaults_and_respects_file_precedence() {
        let test_dir = TestDir::new("max-iterations-layering");

        let defaults = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load defaults");
        assert_eq!(defaults.max_iterations, DEFAULT_MAX_ITERATIONS);

        std::fs::write(
            test_dir.path().join("config.toml"),
            "max_iterations = 240\n",
        )
        .expect("write config layer");
        std::fs::write(test_dir.path().join("settings.json"), "{}")
            .expect("write legacy settings without max_iterations");
        let from_config = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load config layer");
        assert_eq!(from_config.max_iterations, 240);
        assert_eq!(
            from_config
                .resolve_candidate_max_iterations(serde_json::json!({}))
                .expect("resolve candidate missing the higher-priority field"),
            240
        );
        assert_eq!(
            from_config
                .resolve_candidate_max_iterations(serde_json::json!({
                    "max_iterations": 640
                }))
                .expect("resolve candidate override"),
            640
        );

        std::fs::write(
            test_dir.path().join("settings.json"),
            r#"{"max_iterations":320}"#,
        )
        .expect("write settings override");
        let from_settings = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load settings override");
        assert_eq!(from_settings.max_iterations, 320);
    }

    #[test]
    fn max_iterations_accepts_only_the_shared_inclusive_range() {
        let test_dir = TestDir::new("max-iterations-boundaries");
        for value in [
            MIN_MAX_ITERATIONS,
            DEFAULT_MAX_ITERATIONS,
            MAX_CONFIGURABLE_ITERATIONS,
        ] {
            std::fs::write(
                test_dir.path().join("settings.json"),
                format!(r#"{{"max_iterations":{value}}}"#),
            )
            .expect("write boundary setting");
            let loaded = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
                .expect("load valid boundary");
            assert_eq!(loaded.max_iterations, value);
        }

        for value in [0, MAX_CONFIGURABLE_ITERATIONS + 1] {
            let settings_path = test_dir.path().join("settings.json");
            std::fs::write(&settings_path, format!(r#"{{"max_iterations":{value}}}"#))
                .expect("write invalid boundary setting");
            let error = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
                .expect_err("invalid max_iterations must fail configuration loading");
            let message = error.to_string();
            assert!(message.contains(&settings_path.display().to_string()));
            assert!(message.contains("max_iterations must be between 1 and 1000"));
        }
    }

    #[test]
    fn max_iterations_type_errors_identify_the_source_file() {
        let test_dir = TestDir::new("max-iterations-types");
        let settings_path = test_dir.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"max_iterations":"100"}"#)
            .expect("write wrong JSON type");
        let error = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect_err("a string max_iterations must fail");
        assert!(error
            .to_string()
            .contains(&settings_path.display().to_string()));

        std::fs::remove_file(&settings_path).expect("remove invalid settings layer");
        let config_path = test_dir.path().join("config.toml");
        std::fs::write(&config_path, "max_iterations = 0\n").expect("write invalid config layer");
        let error = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect_err("an out-of-range config value must fail");
        let message = error.to_string();
        assert!(message.contains(&config_path.display().to_string()));
        assert!(message.contains("max_iterations must be between 1 and 1000"));
    }

    #[test]
    fn thinking_defaults_and_layers_members_independently() {
        let test_dir = TestDir::new("thinking-layering");
        let defaults = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load thinking defaults");
        assert_eq!(defaults.thinking, ThinkingSettings::default());

        std::fs::write(
            test_dir.path().join("config.toml"),
            "[thinking]\nenabled = false\neffort = \"low\"\n",
        )
        .expect("write lower thinking layer");
        std::fs::write(
            test_dir.path().join("settings.json"),
            r#"{"thinking":{"enabled":true}}"#,
        )
        .expect("write partial higher thinking layer");

        let loaded = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
            .expect("load layered thinking");
        assert_eq!(
            loaded.thinking,
            ThinkingSettings {
                enabled: true,
                effort: ThinkingEffort::Low,
            }
        );
        assert_eq!(
            loaded
                .resolve_candidate_thinking(serde_json::json!({
                    "thinking": { "effort": "max" }
                }))
                .expect("resolve candidate member override"),
            ThinkingSettings {
                enabled: false,
                effort: ThinkingEffort::Max,
            }
        );
    }

    #[test]
    fn thinking_rejects_invalid_or_null_members_with_source() {
        let test_dir = TestDir::new("thinking-invalid");
        let settings_path = test_dir.path().join("settings.json");
        for body in [
            r#"{"thinking":{"effort":"medium"}}"#,
            r#"{"thinking":{"effort":null}}"#,
            r#"{"thinking":{"enabled":null}}"#,
            r#"{"thinking":null}"#,
        ] {
            std::fs::write(&settings_path, body).expect("write invalid thinking setting");
            let error = Settings::load_from_data_dir(test_dir.path().to_path_buf(), false)
                .expect_err("invalid thinking setting must fail");
            assert!(error
                .to_string()
                .contains(&settings_path.display().to_string()));
        }
    }

    #[test]
    fn a2a_debug_never_exposes_bearer_token() {
        let token = ["fixture", "secret"].join("-");
        let config = A2aAgentConfig {
            bearer_token: Some(token.clone()),
            ..A2aAgentConfig::default()
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains(&token));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn higher_priority_explicit_empty_a2a_list_clears_inherited_agents() {
        let data_dir =
            std::env::temp_dir().join(format!("dss-a2a-layering-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("create temp settings directory");
        std::fs::write(
            data_dir.join("config.toml"),
            r#"
[[a2a_agents]]
id = "inherited"
name = "Inherited Agent"
endpoint = "https://agent.example"
enabled = true
timeout_seconds = 120
"#,
        )
        .expect("write config.toml");
        std::fs::write(data_dir.join("settings.json"), r#"{"a2a_agents":[]}"#)
            .expect("write settings.json");

        let loaded = Settings::load_from_data_dir(PathBuf::from(&data_dir), false)
            .expect("load layered settings");
        assert!(loaded.a2a_agents.is_empty());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn legacy_llm_object_maps_to_single_enabled_provider() {
        let data_dir = std::env::temp_dir().join(format!("dss-legacy-llm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("create temp settings directory");
        std::fs::write(
            data_dir.join("settings.json"),
            r#"{"llm":{"base_url":"https://legacy.example","model":"legacy-model","api_key":"secret"}}"#,
        )
        .expect("write settings.json");

        let loaded = Settings::load_from_data_dir(PathBuf::from(&data_dir), false)
            .expect("load legacy settings");
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0].name, "DeepSeek");
        assert_eq!(loaded.providers[0].base_url, "https://legacy.example");
        assert_eq!(loaded.providers[0].model, "legacy-model");
        assert!(loaded.providers[0].enabled);
        assert_eq!(loaded.llm.base_url, "https://legacy.example");
        assert_eq!(loaded.llm.model, "legacy-model");
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn providers_list_takes_precedence_over_legacy_llm() {
        let data_dir =
            std::env::temp_dir().join(format!("dss-providers-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("create temp settings directory");
        std::fs::write(
            data_dir.join("settings.json"),
            r#"{
  "llm": {"base_url":"https://legacy.example","model":"legacy-model"},
  "providers": [
    {"id":"custom","name":"Custom","base_url":"https://custom.example","model":"custom-model","enabled":true}
  ]
}"#,
        )
        .expect("write settings.json");

        let loaded = Settings::load_from_data_dir(PathBuf::from(&data_dir), false)
            .expect("load provider settings");
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0].id, "custom");
        assert_eq!(loaded.providers[0].name, "Custom");
        assert_eq!(loaded.llm.base_url, "https://custom.example");
        assert_eq!(loaded.llm.model, "custom-model");
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn first_enabled_provider_becomes_effective_llm() {
        let data_dir =
            std::env::temp_dir().join(format!("dss-enabled-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("create temp settings directory");
        std::fs::write(
            data_dir.join("settings.json"),
            r#"{
  "providers": [
    {"id":"off","name":"Off","base_url":"https://off.example","model":"off-model","enabled":false},
    {"id":"on","name":"On","base_url":"https://on.example","model":"on-model","enabled":true}
  ]
}"#,
        )
        .expect("write settings.json");

        let loaded = Settings::load_from_data_dir(PathBuf::from(&data_dir), false)
            .expect("load provider settings");
        assert_eq!(loaded.llm.base_url, "https://on.example");
        assert_eq!(loaded.llm.model, "on-model");
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
