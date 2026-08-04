//! 配置加载：defaults → config.toml → settings.json → env (DSS_*)。
//!
//! 优先级：`env (DSS_*) > settings.json > config.toml > defaults`。
//! 配置文件位于 `<data_dir>/config.toml` 与 `<data_dir>/settings.json`；
//! data_dir 本身只由 `DSS_DATA_DIR` 或默认值决定（不参与配置文件加载，避免自引用）。

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::Error;
use crate::paths;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 17896;
pub const DEFAULT_LLM_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_LLM_MODEL: &str = "deepseek-chat";

#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    /// data_dir 是否为默认值（未设 `DSS_DATA_DIR`）；决定是否允许 SSD 软链。
    pub data_dir_is_default: bool,
    pub server: ServerSettings,
    pub llm: LlmSettings,
    /// 日志级别 filter（`DSS_LOG`/`RUST_LOG` 之外来自配置文件的兜底）。
    pub log_level: Option<String>,
    /// MCP server 配置（P7）：启动时尝试连接。
    pub mcp_servers: Vec<McpServerConfig>,
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
    #[serde(default)]
    mcp_servers: Vec<McpServerConfig>,
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

impl Settings {
    /// 按优先级加载配置：`defaults → <data_dir>/config.toml → <data_dir>/settings.json → env`。
    pub fn load() -> Result<Self, Error> {
        let (data_dir, data_dir_is_default) = paths::resolve_data_dir()?;

        let mut server = ServerSettings::default();
        let mut llm = LlmSettings::default();
        let mut log_level = None;
        let mut mcp_servers: Vec<McpServerConfig> = Vec::new();

        // config.toml（低优先级文件）
        let config_toml = data_dir.join("config.toml");
        if config_toml.is_file() {
            let text = std::fs::read_to_string(&config_toml)?;
            let file: FileSettings = toml::from_str(&text).map_err(|e| Error::ConfigParse {
                path: config_toml.clone(),
                message: e.to_string(),
            })?;
            file.apply_to(&mut server, &mut llm, &mut log_level, &mut mcp_servers);
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
            file.apply_to(&mut server, &mut llm, &mut log_level, &mut mcp_servers);
        }

        // env（最高优先级）
        if let Some(host) = std::env::var_os("DSS_HOST") {
            server.host = host.to_string_lossy().into_owned();
        }
        if let Ok(port) = std::env::var("DSS_PORT") {
            if let Ok(port) = port.parse::<u16>() {
                server.port = port;
            }
        }
        // LLM：DEEPSEEK_API_KEY / DSS_LLM_BASE_URL / DSS_LLM_MODEL 覆盖配置文件。
        if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
            if !key.is_empty() {
                llm.api_key = Some(key);
            }
        }
        if let Ok(base_url) = std::env::var("DSS_LLM_BASE_URL") {
            if !base_url.is_empty() {
                llm.base_url = base_url;
            }
        }
        if let Ok(model) = std::env::var("DSS_LLM_MODEL") {
            if !model.is_empty() {
                llm.model = model;
            }
        }
        if let Ok(level) = std::env::var("DSS_LOG").or_else(|_| std::env::var("RUST_LOG")) {
            log_level = Some(level);
        }

        Ok(Settings {
            data_dir,
            data_dir_is_default,
            server,
            llm,
            log_level,
            mcp_servers,
        })
    }
}

impl FileSettings {
    fn apply_to(
        self,
        server: &mut ServerSettings,
        llm: &mut LlmSettings,
        log_level: &mut Option<String>,
        mcp_servers: &mut Vec<McpServerConfig>,
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
        // mcp_servers：非空则覆盖（后文件覆盖前文件）。
        if !self.mcp_servers.is_empty() {
            *mcp_servers = self.mcp_servers;
        }
    }
}
