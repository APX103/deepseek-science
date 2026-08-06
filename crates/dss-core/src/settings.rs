//! 配置加载：defaults → config.toml → settings.json → env (DSS_*)。
//!
//! 优先级：`env (DSS_*) > settings.json > config.toml > defaults`。
//! 配置文件位于 `<data_dir>/config.toml` 与 `<data_dir>/settings.json`；
//! data_dir 本身只由 `DSS_DATA_DIR` 或默认值决定（不参与配置文件加载，避免自引用）。

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

#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    /// data_dir 是否为默认值（未设 `DSS_DATA_DIR`）；决定是否允许 SSD 软链。
    pub data_dir_is_default: bool,
    pub server: ServerSettings,
    pub llm: LlmSettings,
    /// LLM fields whose effective values came from process environment variables.
    pub llm_env_overrides: LlmEnvOverrides,
    /// 日志级别 filter（`DSS_LOG`/`RUST_LOG` 之外来自配置文件的兜底）。
    pub log_level: Option<String>,
    /// MCP server 配置（P7）：启动时尝试连接。
    pub mcp_servers: Vec<McpServerConfig>,
    /// 远端 A2A Agent 配置。这里只保存客户端连接信息；本应用不暴露 A2A server。
    pub a2a_agents: Vec<A2aAgentConfig>,
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

/// MCP server 配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub enabled: bool,
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
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileServerSettings {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileLlmSettings {
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
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

    /// Resolve the file-backed LLM fallback represented by a candidate `settings.json`,
    /// deliberately stopping before environment precedence is applied.
    pub fn resolve_persisted_candidate_llm(&self, candidate: Value) -> Result<LlmSettings, Error> {
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers: Vec<McpServerConfig> = Vec::new();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();

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
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
        );
        Ok(llm)
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
        let mut mcp_servers: Vec<McpServerConfig> = Vec::new();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();

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
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
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
        let mut mcp_servers: Vec<McpServerConfig> = Vec::new();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();

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
            );
        }

        let settings_json = self.data_dir.join("settings.json");
        let file: FileSettings =
            serde_json::from_value(candidate).map_err(|e| Error::ConfigParse {
                path: settings_json,
                message: e.to_string(),
            })?;
        file.apply_to(
            &mut server,
            &mut llm,
            &mut log_level,
            &mut mcp_servers,
            &mut a2a_agents,
        );
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
            skills =
                serde_json::from_value(section.clone()).map_err(|e| Error::ConfigParse {
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

    fn load_from_data_dir(data_dir: PathBuf, data_dir_is_default: bool) -> Result<Self, Error> {
        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers: Vec<McpServerConfig> = Vec::new();
        let mut a2a_agents: Vec<A2aAgentConfig> = Vec::new();

        // config.toml（低优先级文件）
        let config_toml = data_dir.join("config.toml");
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
            );
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
            file.apply_to(
                &mut server,
                &mut llm,
                &mut log_level,
                &mut mcp_servers,
                &mut a2a_agents,
            );
        }

        // env（最高优先级）
        let llm_env_overrides = apply_process_environment(&mut server, &mut llm, &mut log_level);

        Ok(Settings {
            data_dir,
            data_dir_is_default,
            server,
            llm,
            llm_env_overrides,
            log_level,
            mcp_servers,
            a2a_agents,
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

impl FileSettings {
    fn apply_to(
        self,
        server: &mut ServerSettings,
        llm: &mut LlmSettings,
        log_level: &mut Option<String>,
        mcp_servers: &mut Vec<McpServerConfig>,
        a2a_agents: &mut Vec<A2aAgentConfig>,
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
    }
}

#[cfg(test)]
mod tests {
    use super::{A2aAgentConfig, LlmSettings, Settings, DEFAULT_LLM_MODEL};
    use std::path::PathBuf;

    #[test]
    fn llm_defaults_to_current_deepseek_v4_flash_model() {
        let settings = LlmSettings::default();
        assert_eq!(DEFAULT_LLM_MODEL, "deepseek-v4-flash");
        assert_eq!(settings.model, DEFAULT_LLM_MODEL);
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
}
