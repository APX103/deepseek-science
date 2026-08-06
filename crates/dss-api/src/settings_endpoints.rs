//! Persisted application settings with one atomically replaced LLM+A2A runtime snapshot.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::state::{AppRuntimeSnapshot, AppState, LlmRuntimeSnapshot};
use dss_a2a::{
    A2aRuntimeSnapshot, AgentRuntime, AgentRuntimeStatus, ProtocolBinding, ProtocolVersion,
};
use dss_core::A2aAgentConfig;

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

fn is_false(value: &bool) -> bool {
    !*value
}

async fn ensure_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        // Avoid a permissive creation window before the exact permission
        // check below, including for parents created recursively.
        builder.mode(0o700);
    }
    builder.create(path).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }

    Ok(())
}

async fn write_private_settings(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let data_dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "settings path must have a parent directory",
        )
    })?;
    ensure_private_directory(data_dir).await?;

    // Keep the temporary file beside the destination so rename is atomic. A
    // unique create-new path also prevents following or truncating a stale
    // symlink left at a predictable temporary path.
    let tmp = data_dir.join(format!(".settings.json.{}.tmp", Uuid::new_v4()));
    let write_result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }

        let mut file = options.open(&tmp).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // `mode(0o600)` is filtered by the process umask. Set the exact
            // mode through the already-open handle before writing secrets.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .await?;
        }
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);

        tokio::fs::rename(&tmp, path).await?;
        #[cfg(unix)]
        {
            // The rename is already the linearization point. Directory fsync makes it durable
            // across sudden power loss; failure is logged but cannot turn a completed rename
            // into a disk/runtime split.
            match tokio::fs::File::open(data_dir).await {
                Ok(directory) => {
                    if let Err(error) = directory.sync_all().await {
                        tracing::warn!(%error, path = %data_dir.display(), "settings directory fsync failed after atomic rename");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, path = %data_dir.display(), "settings directory open failed after atomic rename");
                }
            }
        }
        Ok(())
    }
    .await;

    if let Err(write_error) = write_result {
        return match tokio::fs::remove_file(&tmp).await {
            Ok(()) => Err(write_error),
            Err(cleanup_error) if cleanup_error.kind() == io::ErrorKind::NotFound => {
                Err(write_error)
            }
            Err(cleanup_error) => Err(io::Error::new(
                cleanup_error.kind(),
                format!(
                    "{write_error}; additionally failed to remove temporary settings file: {cleanup_error}"
                ),
            )),
        };
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    #[serde(default)]
    id: String,
    name: String,
    base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    api_key_masked: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2aInterfaceSettings {
    url: String,
    protocol_binding: String,
    protocol_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct A2aCardSettings {
    name: String,
    description: String,
    version: String,
    protocol_version: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    supported_interfaces: Vec<A2aInterfaceSettings>,
}

/// Public/editable A2A settings shape. Plaintext Bearer material is accepted only on POST and
/// omitted from every response; runtime/card fields are diagnostics and are never persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aAgentSettings {
    id: String,
    name: String,
    endpoint: String,
    enabled: bool,
    timeout_seconds: u64,
    #[serde(default)]
    bearer_token_masked: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bearer_token: Option<String>,
    /// Explicit three-state credential update: omitted/false preserves, true removes, and a
    /// non-empty `bearer_token` replaces. Never returned as true by GET/POST responses.
    #[serde(default, skip_serializing_if = "is_false")]
    clear_bearer_token: bool,
    #[serde(default)]
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refreshed_at: Option<String>,
    #[serde(default)]
    tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    card_summary: Option<A2aCardSettings>,
}

/// Editable Skill discovery settings mirrored from/to `settings.json`'s `skills` object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillSettingsPayload {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    include_claude: bool,
    #[serde(default)]
    include_codex: bool,
    #[serde(default)]
    include_cursor: bool,
    #[serde(default)]
    custom_dirs: Vec<String>,
}

impl From<&dss_core::SkillSettings> for SkillSettingsPayload {
    fn from(value: &dss_core::SkillSettings) -> Self {
        Self {
            disabled: value.disabled.clone(),
            include_claude: value.include_claude,
            include_codex: value.include_codex,
            include_cursor: value.include_cursor,
            custom_dirs: value.custom_dirs.clone(),
        }
    }
}

impl SkillSettingsPayload {
    /// Normalize submitted skill settings: trim and drop empty names/paths, de-duplicate.
    fn to_config(&self) -> dss_core::SkillSettings {
        fn clean(values: &[String]) -> Vec<String> {
            let mut seen = std::collections::HashSet::new();
            values
                .iter()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .filter(|v| seen.insert(v.clone()))
                .collect()
        }
        dss_core::SkillSettings {
            disabled: clean(&self.disabled),
            include_claude: self.include_claude,
            include_codex: self.include_codex,
            include_cursor: self.include_cursor,
            custom_dirs: clean(&self.custom_dirs),
        }
    }
}

/// Editable MCP server entry mirrored from/to `settings.json`'s `mcp_servers` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSettings {
    name: String,
    url: String,
    #[serde(default)]
    enabled: bool,
    /// Live connection state (diagnostic; never persisted). Present only on responses.
    #[serde(default)]
    connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_count: Option<usize>,
}

impl From<&dss_core::McpServerConfig> for McpServerSettings {
    fn from(value: &dss_core::McpServerConfig) -> Self {
        Self {
            name: value.name.clone(),
            url: value.url.clone(),
            enabled: value.enabled,
            connected: false,
            tool_count: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettingsPayload {
    providers: Vec<ProviderSettings>,
    model: String,
    default_workspace: String,
    #[serde(default)]
    restart_required: bool,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    overridden_fields: Vec<String>,
    #[serde(default)]
    a2a_agents: Vec<A2aAgentSettings>,
    #[serde(default)]
    skills: SkillSettingsPayload,
    #[serde(default)]
    mcp_servers: Vec<McpServerSettings>,
    /// 数据源 API keys（GET 时脱敏；POST 时前端回传，None/空值保留后端旧值）。
    /// 和 providers.api_key 同样的 mask 机制。
    #[serde(default)]
    api_keys_masked: std::collections::HashMap<String, String>,
    #[serde(default)]
    api_keys: Option<std::collections::HashMap<String, String>>,
}

fn public_settings(
    state: &AppState,
    runtime: &AppRuntimeSnapshot,
    persisted_providers: &[dss_core::LlmProvider],
    persisted_llm: &dss_core::LlmSettings,
    persisted_skills: &dss_core::SkillSettings,
    persisted_mcp: &[dss_core::McpServerConfig],
) -> AppSettingsPayload {
    let llm = runtime.llm();
    let effective_model = persisted_providers
        .iter()
        .find(|p| p.enabled)
        .map(|p| p.model.clone())
        .unwrap_or_else(|| persisted_llm.model.clone());
    AppSettingsPayload {
        providers: persisted_providers
            .iter()
            .map(|p| ProviderSettings {
                id: p.id.clone(),
                name: p.name.clone(),
                base_url: p.base_url.clone(),
                model: Some(p.model.clone()),
                api_key_masked: if p.is_configured() {
                    "••••••••".into()
                } else {
                    String::new()
                },
                api_key: None,
                enabled: p.enabled,
            })
            .collect(),
        model: effective_model,
        default_workspace: state
            .settings
            .data_dir
            .join("workspaces")
            .display()
            .to_string(),
        restart_required: false,
        revision: runtime.revision(),
        overridden_fields: llm
            .overridden_fields()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        a2a_agents: runtime.a2a().agents.iter().map(public_a2a_agent).collect(),
        skills: SkillSettingsPayload::from(persisted_skills),
        mcp_servers: persisted_mcp.iter().map(McpServerSettings::from).collect(),
        api_keys_masked: state
            .settings
            .api_keys
            .iter()
            .map(|(k, v)| {
                let masked = if v.is_empty() {
                    String::new()
                } else {
                    "••••••••".into()
                };
                (k.clone(), masked)
            })
            .collect(),
        api_keys: None,
    }
}

/// Overlay live MCP connection state (connected + tool count) onto the persisted server list.
async fn enrich_mcp_status(state: &AppState, payload: &mut AppSettingsPayload) {
    let manager = state.mcp_runtime_snapshot().await.manager;
    for server in &mut payload.mcp_servers {
        if let Some(info) = manager.server_info(&server.name).await {
            server.connected = info.connected;
            server.tool_count = Some(info.tools.len());
        } else {
            server.connected = false;
            server.tool_count = None;
        }
    }
}

fn public_a2a_agent(agent: &AgentRuntime) -> A2aAgentSettings {
    let card_summary = agent.card.as_ref().map(|card| A2aCardSettings {
        name: card.summary.name.clone(),
        description: card.summary.description.clone(),
        version: card.summary.agent_version.clone(),
        protocol_version: protocol_version_label(card.summary.protocol_version).into(),
        skills: card
            .summary
            .skills
            .iter()
            .map(|skill| {
                if skill.description.is_empty() {
                    skill.name.clone()
                } else {
                    format!("{} — {}", skill.name, skill.description)
                }
            })
            .collect(),
        supported_interfaces: vec![A2aInterfaceSettings {
            url: card.selected_interface.url.clone(),
            protocol_binding: protocol_binding_label(card.selected_interface.binding).into(),
            protocol_version: protocol_version_label(card.selected_interface.protocol_version)
                .into(),
        }],
    });
    A2aAgentSettings {
        id: agent.config.id.clone(),
        name: agent.config.name.clone(),
        endpoint: agent.config.endpoint.clone(),
        enabled: agent.config.enabled,
        timeout_seconds: agent.config.timeout_seconds,
        bearer_token_masked: if agent
            .config
            .bearer_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
        {
            "••••••••".into()
        } else {
            String::new()
        },
        bearer_token: None,
        clear_bearer_token: false,
        status: match agent.status {
            AgentRuntimeStatus::Unchecked => "unchecked",
            AgentRuntimeStatus::Ready => "ready",
            AgentRuntimeStatus::Offline => "unreachable",
            AgentRuntimeStatus::Invalid => "invalid",
            AgentRuntimeStatus::Unsupported => "unsupported",
            AgentRuntimeStatus::Disabled => "disabled",
        }
        .into(),
        last_error: agent.last_error.clone(),
        last_refreshed_at: agent.last_refreshed_at.clone(),
        tool_name: agent.tool_name(),
        card_summary,
    }
}

fn protocol_version_label(version: ProtocolVersion) -> &'static str {
    match version {
        ProtocolVersion::V1 => "1.0",
        ProtocolVersion::V03 => "0.3",
    }
}

fn protocol_binding_label(binding: ProtocolBinding) -> &'static str {
    match binding {
        ProtocolBinding::JsonRpc => "JSONRPC",
        ProtocolBinding::HttpJson => "HTTP+JSON",
    }
}

pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<AppSettingsPayload>, (StatusCode, Json<Value>)> {
    let runtime = state.runtime_snapshot().await;
    let persisted_providers = state
        .settings
        .reload_persisted_providers()
        .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let persisted_llm = state
        .settings
        .reload_persisted_llm()
        .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let persisted_skills = state
        .settings
        .reload_persisted_skills()
        .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let persisted_mcp = state
        .settings
        .reload_persisted_mcp_servers()
        .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let mut payload = public_settings(
        &state,
        &runtime,
        &persisted_providers,
        &persisted_llm,
        &persisted_skills,
        &persisted_mcp,
    );
    enrich_mcp_status(&state, &mut payload).await;
    Ok(Json(payload))
}

pub async fn save_settings(
    State(state): State<AppState>,
    Json(payload): Json<AppSettingsPayload>,
) -> Result<Json<AppSettingsPayload>, (StatusCode, Json<Value>)> {
    // 校验 provider 列表：必须有且仅有一个启用；名称非空且不重复；base_url/model 合法。
    let enabled_count = payload.providers.iter().filter(|p| p.enabled).count();
    if enabled_count != 1 {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            &format!("exactly one provider must be enabled (found {enabled_count})"),
        ));
    }
    if payload.providers.is_empty() {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "at least one provider must be configured",
        ));
    }
    let mut seen_provider_names = std::collections::HashSet::new();
    for provider in &payload.providers {
        let name = provider.name.trim();
        if name.is_empty() {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "each provider needs a name",
            ));
        }
        if !seen_provider_names.insert(name.to_lowercase()) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("duplicate provider name: {name}"),
            ));
        }
        let base_url = provider.base_url.trim();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("provider \"{name}\" base_url must use http:// or https://"),
            ));
        }
        let model = provider
            .model
            .as_deref()
            .unwrap_or(payload.model.as_str())
            .trim();
        if model.is_empty() {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("provider \"{name}\" model must not be empty"),
            ));
        }
    }

    // Keep root read/merge/write serialized, but do network discovery without blocking runs from
    // cloning the previous immutable snapshot.
    let save_guard = state.settings_save_lock.clone().lock_owned().await;
    let current_runtime = state.runtime_snapshot().await;
    if payload.revision != current_runtime.revision() {
        return Err(json_error(
            StatusCode::CONFLICT,
            "settings changed since this form was loaded; reload before saving",
        ));
    }

    ensure_private_directory(&state.settings.data_dir)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let path = state.settings.data_dir.join("settings.json");
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            json_error(
                StatusCode::BAD_REQUEST,
                &format!("existing settings.json is invalid: {e}"),
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => {
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &e.to_string(),
            ))
        }
    };
    if !root.is_object() {
        root = json!({});
    }
    let persisted_a2a_before = state
        .settings
        .resolve_persisted_candidate_a2a_agents(root.clone())
        .map_err(|error| json_error(StatusCode::BAD_REQUEST, &error.to_string()))?;
    let mut a2a_configs = Vec::with_capacity(payload.a2a_agents.len());
    for submitted in &payload.a2a_agents {
        let submitted_token = submitted
            .bearer_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
        if submitted.clear_bearer_token && submitted_token.is_some() {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "bearer_token and clear_bearer_token cannot be submitted together",
            ));
        }
        let preserved_token = persisted_a2a_before
            .iter()
            // Credentials are authority-bound. Reusing the same local id for a different
            // endpoint must never forward the old secret to the newly entered host/path.
            .find(|existing| {
                existing.id == submitted.id && existing.endpoint.trim() == submitted.endpoint.trim()
            })
            .and_then(|existing| existing.bearer_token.clone());
        a2a_configs.push(A2aAgentConfig {
            id: submitted.id.trim().to_owned(),
            name: submitted.name.trim().to_owned(),
            endpoint: submitted.endpoint.trim().to_owned(),
            enabled: submitted.enabled,
            bearer_token: if submitted.clear_bearer_token {
                None
            } else {
                submitted_token.or(preserved_token)
            },
            timeout_seconds: submitted.timeout_seconds,
        });
    }
    dss_a2a::validate_configs(&a2a_configs)
        .map_err(|error| json_error(StatusCode::BAD_REQUEST, &error.to_string()))?;

    let mut mcp_configs: Vec<dss_core::McpServerConfig> =
        Vec::with_capacity(payload.mcp_servers.len());
    let mut seen_mcp_names = std::collections::HashSet::new();
    for submitted in &payload.mcp_servers {
        let name = submitted.name.trim().to_owned();
        if name.is_empty() {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "each MCP server needs a name",
            ));
        }
        let url = submitted.url.trim().to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("MCP server \"{name}\" url must use http:// or https://"),
            ));
        }
        if !seen_mcp_names.insert(name.to_lowercase()) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("duplicate MCP server name: {name}"),
            ));
        }
        mcp_configs.push(dss_core::McpServerConfig {
            name,
            url,
            enabled: submitted.enabled,
        });
    }

    let persisted_providers_before = state
        .settings
        .resolve_persisted_candidate_providers(root.clone())
        .map_err(|error| json_error(StatusCode::BAD_REQUEST, &error.to_string()))?;

    // Build the provider list to persist, preserving api_key by id when the UI did not submit one.
    let mut provider_configs: Vec<dss_core::LlmProvider> =
        Vec::with_capacity(payload.providers.len());
    let mut seen_provider_ids = std::collections::HashSet::new();
    for submitted in &payload.providers {
        let submitted_key = submitted
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        let preserved_key = persisted_providers_before
            .iter()
            .find(|existing| existing.id == submitted.id)
            .and_then(|existing| existing.api_key.clone());
        let id = if submitted.id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            submitted.id.trim().to_owned()
        };
        if !seen_provider_ids.insert(id.clone()) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!("duplicate provider id: {id}"),
            ));
        }
        provider_configs.push(dss_core::LlmProvider {
            id,
            name: submitted.name.trim().to_owned(),
            base_url: submitted.base_url.trim().to_owned(),
            model: submitted
                .model
                .as_deref()
                .unwrap_or(payload.model.as_str())
                .trim()
                .to_owned(),
            api_key: submitted_key.or(preserved_key),
            enabled: submitted.enabled,
        });
    }

    let object = root.as_object_mut().expect("object assigned above");
    let selected_provider = provider_configs
        .iter()
        .find(|p| p.enabled)
        .expect("exactly one enabled provider validated above");
    let mut llm = object.remove("llm").unwrap_or_else(|| json!({}));
    if !llm.is_object() {
        llm = json!({});
    }
    let llm_object = llm.as_object_mut().expect("object assigned above");
    llm_object.insert(
        "base_url".into(),
        Value::String(selected_provider.base_url.clone()),
    );
    llm_object.insert(
        "model".into(),
        Value::String(selected_provider.model.clone()),
    );
    if let Some(key) = selected_provider.api_key.clone() {
        llm_object.insert("api_key".into(), Value::String(key));
    }
    object.insert("llm".into(), llm);
    object.insert(
        "providers".into(),
        serde_json::to_value(&provider_configs)
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?,
    );
    object.insert(
        "a2a_agents".into(),
        serde_json::to_value(&a2a_configs)
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?,
    );
    let skills_config = payload.skills.to_config();
    object.insert(
        "skills".into(),
        serde_json::to_value(&skills_config)
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?,
    );
    object.insert(
        "mcp_servers".into(),
        serde_json::to_value(&mcp_configs)
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?,
    );
    // api_keys：前端回传的值里，mask 占位（••••••••）保留后端旧值，其余（含空=清空）写入。
    // 未出现在 payload.api_keys 里的旧 key 也保留（不丢用户已存的 key）。
    let mut merged_api_keys = state.settings.api_keys.clone();
    if let Some(submitted) = &payload.api_keys {
        for (k, v) in submitted {
            if v == "••••••••" {
                // mask 占位：保留旧值（不动 merged_api_keys）
                continue;
            }
            if v.is_empty() {
                merged_api_keys.remove(k);
            } else {
                merged_api_keys.insert(k.clone(), v.clone());
            }
        }
    }
    object.insert(
        "api_keys".into(),
        serde_json::to_value(&merged_api_keys)
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?,
    );

    let bytes = serde_json::to_vec_pretty(&root)
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // Resolve exactly what a restart would load, and construct the complete next snapshot,
    // before the durable rename. Once the file is replaced, only the infallible pointer swap
    // remains, so disk and runtime cannot diverge through a post-write parse/reload failure.
    let next_revision = current_runtime.revision().checked_add(1).ok_or_else(|| {
        json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime settings revision overflow",
        )
    })?;
    let persisted_providers = state
        .settings
        .resolve_candidate_providers(root.clone())
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let persisted = state
        .settings
        .resolve_persisted_candidate_llm(root.clone())
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let resolved = state
        .settings
        .resolve_candidate_llm(root.clone())
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let persisted_a2a = state
        .settings
        .resolve_persisted_candidate_a2a_agents(root.clone())
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let persisted_skills = state
        .settings
        .resolve_persisted_candidate_skills(root.clone())
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let persisted_mcp = state
        .settings
        .resolve_persisted_candidate_mcp_servers(root)
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let llm_replacement = Arc::new(LlmRuntimeSnapshot::new(
        resolved.llm,
        next_revision,
        resolved.env_overrides,
    ));
    let a2a_unrefreshed = A2aRuntimeSnapshot::unrefreshed(next_revision, persisted_a2a)
        .map_err(|e| json_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    // Card failures stay as per-Agent diagnostics; valid offline configurations still persist.
    let a2a_replacement = Arc::new(a2a_unrefreshed.refresh_all(&state.a2a_client).await);
    let replacement = Arc::new(AppRuntimeSnapshot::new(
        next_revision,
        llm_replacement,
        a2a_replacement,
    ));

    // Linearize durable replacement and run snapshot capture. Existing runs already own their
    // old Arc; new runs wait until disk and runtime both represent this revision.
    let runtime_slot = state.runtime.clone();
    let replacement_for_commit = replacement.clone();
    tokio::spawn(async move {
        // The owned settings guard and independent task make the commit non-cancellable once it
        // starts. Dropping the HTTP request can no longer leave a completed rename without the
        // matching runtime pointer swap, nor let a second save overlap this commit.
        let _save_guard = save_guard;
        let mut runtime_slot = runtime_slot.write().await;
        write_private_settings(&path, &bytes).await?;
        *runtime_slot = replacement_for_commit;
        Ok::<(), io::Error>(())
    })
    .await
    .map_err(|error| {
        json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("settings commit task failed: {error}"),
        )
    })?
    .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;

    // Skill discovery and MCP are not part of the atomic LLM/A2A runtime snapshot; rebuild them
    // after the durable write so built-in toggles, external/custom dirs, and MCP server changes
    // apply without a restart.
    state.rebuild_catalog(&persisted_skills).await;
    state.rebuild_mcp(&persisted_mcp).await;

    let mut payload = public_settings(
        &state,
        &replacement,
        &persisted_providers,
        &persisted,
        &persisted_skills,
        &persisted_mcp,
    );
    enrich_mcp_status(&state, &mut payload).await;
    Ok(Json(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dss_core::settings::ServerSettings;
    use dss_core::{LlmEnvOverrides, LlmSettings, Settings};
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            Self(std::env::temp_dir().join(format!("dss-settings-{label}-{}", Uuid::new_v4())))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const INITIAL_BASE_URL: &str = "https://initial.example.invalid";
    const INITIAL_MODEL: &str = "initial-model";

    fn rerun_without_llm_env(test_name: &str) -> bool {
        const CHILD_ENV: &str = "DSS_SETTINGS_HOT_RELOAD_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            return false;
        }

        let output = std::process::Command::new(
            std::env::current_exe().expect("resolve current test executable"),
        )
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DSS_LLM_BASE_URL")
        .env_remove("DSS_LLM_MODEL")
        .output()
        .expect("rerun test with isolated LLM environment");
        assert!(
            output.status.success(),
            "isolated child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    fn rerun_with_llm_env_overrides(test_name: &str) -> bool {
        const CHILD_ENV: &str = "DSS_SETTINGS_ENV_OVERRIDE_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            return false;
        }

        let output = std::process::Command::new(
            std::env::current_exe().expect("resolve current test executable"),
        )
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("DSS_LLM_BASE_URL", "https://environment.example.invalid")
        .env("DSS_LLM_MODEL", "environment-model")
        .env("DEEPSEEK_API_KEY", ["environment", "credential"].join("-"))
        .output()
        .expect("rerun test with isolated LLM environment overrides");
        assert!(
            output.status.success(),
            "isolated child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    async fn build_test_state(test_dir: &TestDir, configured: bool) -> AppState {
        let credential = configured.then(|| ["test", "credential"].join("-"));
        let mut llm_json = json!({
            "base_url": INITIAL_BASE_URL,
            "model": INITIAL_MODEL,
        });
        if let Some(value) = credential.as_ref() {
            llm_json
                .as_object_mut()
                .expect("LLM test settings are an object")
                .insert("api_key".into(), Value::String(value.clone()));
        }
        let bytes = serde_json::to_vec_pretty(&json!({ "llm": llm_json }))
            .expect("serialize initial settings");
        write_private_settings(&test_dir.path().join("settings.json"), &bytes)
            .await
            .expect("write initial settings");

        crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            server: ServerSettings::default(),
            llm: LlmSettings {
                base_url: INITIAL_BASE_URL.into(),
                model: INITIAL_MODEL.into(),
                api_key: credential.clone(),
            },
            providers: vec![dss_core::LlmProvider {
                id: "deepseek".to_string(),
                name: "DeepSeek".to_string(),
                base_url: INITIAL_BASE_URL.into(),
                model: INITIAL_MODEL.into(),
                api_key: credential,
                enabled: true,
            }],
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state")
    }

    fn payload(base_url: &str, model: &str, api_key: Option<String>) -> AppSettingsPayload {
        AppSettingsPayload {
            providers: vec![ProviderSettings {
                id: "deepseek".to_string(),
                name: "DeepSeek".into(),
                base_url: base_url.into(),
                model: Some(model.into()),
                api_key_masked: String::new(),
                api_key,
                enabled: true,
            }],
            model: model.into(),
            default_workspace: String::new(),
            restart_required: true,
            revision: 0,
            overridden_fields: Vec::new(),
            a2a_agents: Vec::new(),
            skills: SkillSettingsPayload::default(),
            mcp_servers: Vec::new(),
            api_keys_masked: std::collections::HashMap::new(),
            api_keys: None,
        }
    }

    fn a2a_payload(id: &str, token: Option<String>, enabled: bool) -> A2aAgentSettings {
        A2aAgentSettings {
            id: id.into(),
            name: format!("Agent {id}"),
            endpoint: "http://127.0.0.1:9".into(),
            enabled,
            timeout_seconds: 30,
            bearer_token_masked: String::new(),
            bearer_token: token,
            clear_bearer_token: false,
            status: String::new(),
            last_error: None,
            last_refreshed_at: None,
            tool_name: String::new(),
            card_summary: None,
        }
    }

    #[tokio::test]
    async fn save_hot_swaps_one_coherent_snapshot_and_masks_the_credential() {
        if rerun_without_llm_env("save_hot_swaps_one_coherent_snapshot_and_masks_the_credential") {
            return;
        }

        const NEXT_BASE_URL: &str = "https://next.example.invalid";
        const NEXT_MODEL: &str = "next-model";
        let test_dir = TestDir::new("hot-swap");
        let state = build_test_state(&test_dir, true).await;
        let started_run_snapshot = state.llm_snapshot().await;

        let Json(saved) = save_settings(
            State(state.clone()),
            Json(payload(
                NEXT_BASE_URL,
                NEXT_MODEL,
                Some(["replacement", "credential"].join("-")),
            )),
        )
        .await
        .expect("save settings");

        let current = state.llm_snapshot().await;
        assert!(!Arc::ptr_eq(&started_run_snapshot, &current));
        assert_eq!(started_run_snapshot.settings().base_url, INITIAL_BASE_URL);
        assert_eq!(started_run_snapshot.settings().model, INITIAL_MODEL);
        assert_eq!(
            started_run_snapshot
                .client()
                .expect("initial client")
                .base_url(),
            INITIAL_BASE_URL
        );
        assert_eq!(current.settings().base_url, NEXT_BASE_URL);
        assert_eq!(current.settings().model, NEXT_MODEL);
        assert_eq!(
            current.client().expect("replacement client").base_url(),
            NEXT_BASE_URL
        );
        assert!(current.is_configured());

        assert!(!saved.restart_required);
        assert_eq!(saved.revision, 1);
        assert!(saved.overridden_fields.is_empty());
        assert_eq!(saved.providers[0].base_url, NEXT_BASE_URL);
        assert_eq!(saved.providers[0].model.as_deref(), Some(NEXT_MODEL));
        assert!(saved.providers[0].api_key.is_none());
        assert!(!saved.providers[0].api_key_masked.is_empty());
        let public_json = serde_json::to_value(&saved).expect("serialize public settings");
        assert!(public_json["providers"][0].get("api_key").is_none());

        let restarted = state
            .settings
            .reload_llm()
            .expect("reload effective settings");
        assert_eq!(current.settings().base_url, restarted.base_url);
        assert_eq!(current.settings().model, restarted.model);
        assert_eq!(current.is_configured(), restarted.is_configured());

        let Json(fetched) = get_settings(State(state)).await.expect("get settings");
        assert_eq!(fetched.providers[0].base_url, NEXT_BASE_URL);
        assert_eq!(fetched.model, NEXT_MODEL);
        assert_eq!(fetched.revision, 1);
        assert!(!fetched.restart_required);
    }

    #[tokio::test]
    async fn blank_credential_preserves_existing_configuration() {
        if rerun_without_llm_env("blank_credential_preserves_existing_configuration") {
            return;
        }

        let test_dir = TestDir::new("preserve-credential");
        let state = build_test_state(&test_dir, true).await;
        let Json(saved) = save_settings(
            State(state.clone()),
            Json(payload(
                "https://preserved.example.invalid",
                "preserved-model",
                Some("   ".into()),
            )),
        )
        .await
        .expect("save settings without replacing credential");

        assert!(state.llm_snapshot().await.is_configured());
        assert!(!saved.providers[0].api_key_masked.is_empty());
        assert!(saved.providers[0].api_key.is_none());
        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(test_dir.path().join("settings.json"))
                .await
                .expect("read settings file"),
        )
        .expect("parse settings file");
        assert!(persisted["llm"].get("api_key").is_some());
    }

    #[tokio::test]
    async fn save_can_configure_an_unconfigured_runtime_without_restart() {
        if rerun_without_llm_env("save_can_configure_an_unconfigured_runtime_without_restart") {
            return;
        }

        let test_dir = TestDir::new("configure-runtime");
        let state = build_test_state(&test_dir, false).await;
        assert!(!state.llm_snapshot().await.is_configured());

        let Json(saved) = save_settings(
            State(state.clone()),
            Json(payload(
                "https://configured.example.invalid",
                "configured-model",
                Some(["new", "credential"].join("-")),
            )),
        )
        .await
        .expect("configure runtime");

        assert!(state.llm_snapshot().await.is_configured());
        assert!(!saved.restart_required);
        assert!(saved.providers[0].api_key.is_none());
        let Json(config) = crate::config(State(state)).await;
        assert!(config.llm_configured);
        assert_eq!(config.base_url, "https://configured.example.invalid");
        assert_eq!(config.model, "configured-model");
        assert_eq!(config.revision, 1);
        assert!(config.overridden_fields.is_empty());
    }

    #[tokio::test]
    async fn failed_save_keeps_the_previous_runtime_snapshot() {
        if rerun_without_llm_env("failed_save_keeps_the_previous_runtime_snapshot") {
            return;
        }

        let test_dir = TestDir::new("failed-save-runtime");
        let state = build_test_state(&test_dir, true).await;
        let before = state.llm_snapshot().await;
        let settings_path = test_dir.path().join("settings.json");
        tokio::fs::remove_file(&settings_path)
            .await
            .expect("remove settings file");
        tokio::fs::create_dir(&settings_path)
            .await
            .expect("replace settings file with a directory");

        let _error = save_settings(
            State(state.clone()),
            Json(payload(
                "https://must-not-apply.example.invalid",
                "must-not-apply",
                None,
            )),
        )
        .await
        .expect_err("save must fail");

        let after = state.llm_snapshot().await;
        assert!(Arc::ptr_eq(&before, &after));
        assert_eq!(after.settings().base_url, INITIAL_BASE_URL);
        assert_eq!(after.settings().model, INITIAL_MODEL);
    }

    #[tokio::test]
    async fn concurrent_stale_saves_reject_one_without_losing_the_committed_update() {
        if rerun_without_llm_env(
            "concurrent_stale_saves_reject_one_without_losing_the_committed_update",
        ) {
            return;
        }

        let test_dir = TestDir::new("serialized-saves");
        let state = build_test_state(&test_dir, true).await;
        let first = save_settings(
            State(state.clone()),
            Json(payload(
                "https://first.example.invalid",
                "first-model",
                None,
            )),
        );
        let second = save_settings(
            State(state.clone()),
            Json(payload(
                "https://second.example.invalid",
                "second-model",
                None,
            )),
        );
        let (first_result, second_result) = tokio::join!(first, second);
        let (saved, rejected) = match (first_result, second_result) {
            (Ok(saved), Err(rejected)) | (Err(rejected), Ok(saved)) => (saved.0, rejected),
            other => panic!("expected one committed save and one stale conflict: {other:?}"),
        };
        assert!(!saved.restart_required);
        assert_eq!(saved.revision, 1);
        assert_eq!(rejected.0, StatusCode::CONFLICT);

        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(test_dir.path().join("settings.json"))
                .await
                .expect("read settings file"),
        )
        .expect("parse settings file");
        let runtime = state.llm_snapshot().await;
        assert_eq!(runtime.revision(), 1);
        assert_eq!(
            persisted["llm"]["base_url"].as_str(),
            Some(runtime.settings().base_url.as_str())
        );
        assert_eq!(
            persisted["llm"]["model"].as_str(),
            Some(runtime.settings().model.as_str())
        );
        assert!(runtime.is_configured());
    }

    #[tokio::test]
    async fn environment_overrides_match_restart_precedence_after_save() {
        if rerun_with_llm_env_overrides("environment_overrides_match_restart_precedence_after_save")
        {
            return;
        }

        let test_dir = TestDir::new("environment-overrides");
        let state = build_test_state(&test_dir, false).await;
        let Json(saved) = save_settings(
            State(state.clone()),
            Json(payload(
                "https://persisted.example.invalid",
                "persisted-model",
                None,
            )),
        )
        .await
        .expect("save settings beneath environment overrides");

        let runtime = state.llm_snapshot().await;
        assert_eq!(
            runtime.settings().base_url,
            "https://environment.example.invalid"
        );
        assert_eq!(runtime.settings().model, "environment-model");
        assert!(runtime.is_configured());
        assert_eq!(
            saved.providers[0].base_url,
            "https://persisted.example.invalid"
        );
        assert_eq!(saved.model, "persisted-model");
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.overridden_fields, ["base_url", "model", "api_key"]);
        assert!(!saved.restart_required);
        assert!(saved.providers[0].api_key.is_none());

        let Json(config) = crate::config(State(state.clone())).await;
        assert_eq!(config.revision, saved.revision);
        assert_eq!(config.base_url, "https://environment.example.invalid");
        assert_eq!(config.model, "environment-model");
        assert_eq!(config.overridden_fields, ["base_url", "model", "api_key"]);

        let restarted = state
            .settings
            .reload_llm()
            .expect("reload restart settings");
        assert_eq!(runtime.settings().base_url, restarted.base_url);
        assert_eq!(runtime.settings().model, restarted.model);
        assert_eq!(runtime.is_configured(), restarted.is_configured());

        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(test_dir.path().join("settings.json"))
                .await
                .expect("read settings file"),
        )
        .expect("parse settings file");
        assert_eq!(
            persisted["llm"]["base_url"].as_str(),
            Some("https://persisted.example.invalid")
        );
        assert_eq!(persisted["llm"]["model"].as_str(), Some("persisted-model"));
    }

    #[tokio::test]
    async fn key_only_save_does_not_persist_environment_owned_base_or_model() {
        if rerun_with_llm_env_overrides(
            "key_only_save_does_not_persist_environment_owned_base_or_model",
        ) {
            return;
        }

        let test_dir = TestDir::new("environment-key-only");
        let state = build_test_state(&test_dir, false).await;
        let Json(mut editable) = get_settings(State(state.clone()))
            .await
            .expect("get persisted editable settings");

        assert_eq!(editable.providers[0].base_url, INITIAL_BASE_URL);
        assert_eq!(editable.model, INITIAL_MODEL);
        assert!(editable.providers[0].api_key_masked.is_empty());
        editable.providers[0].api_key = Some(["fallback", "credential"].join("-"));

        let Json(saved) = save_settings(State(state.clone()), Json(editable))
            .await
            .expect("save only fallback credential");
        assert_eq!(saved.providers[0].base_url, INITIAL_BASE_URL);
        assert_eq!(saved.model, INITIAL_MODEL);
        assert!(!saved.providers[0].api_key_masked.is_empty());

        let runtime = state.llm_snapshot().await;
        assert_eq!(
            runtime.settings().base_url,
            "https://environment.example.invalid"
        );
        assert_eq!(runtime.settings().model, "environment-model");
        assert_eq!(
            runtime.overridden_fields(),
            ["base_url", "model", "api_key"]
        );

        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(test_dir.path().join("settings.json"))
                .await
                .expect("read settings file"),
        )
        .expect("parse settings file");
        assert_eq!(
            persisted["llm"]["base_url"].as_str(),
            Some(INITIAL_BASE_URL)
        );
        assert_eq!(persisted["llm"]["model"].as_str(), Some(INITIAL_MODEL));

        std::env::remove_var("DSS_LLM_BASE_URL");
        std::env::remove_var("DSS_LLM_MODEL");
        std::env::remove_var("DEEPSEEK_API_KEY");
        let restarted = state
            .settings
            .reload_llm()
            .expect("reload without environment overrides");
        assert_eq!(restarted.base_url, INITIAL_BASE_URL);
        assert_eq!(restarted.model, INITIAL_MODEL);
        assert!(restarted.is_configured());
    }

    #[tokio::test]
    async fn invalid_candidate_is_rejected_before_disk_or_runtime_changes() {
        if rerun_without_llm_env("invalid_candidate_is_rejected_before_disk_or_runtime_changes") {
            return;
        }

        let test_dir = TestDir::new("invalid-candidate");
        let state = build_test_state(&test_dir, true).await;
        let settings_path = test_dir.path().join("settings.json");
        let invalid_candidate = br#"{
  "server": "not-an-object",
  "llm": {
    "base_url": "https://unchanged.example.invalid",
    "model": "unchanged-model"
  }
}"#;
        tokio::fs::write(&settings_path, invalid_candidate)
            .await
            .expect("install structurally invalid current settings");
        let before_disk = tokio::fs::read(&settings_path)
            .await
            .expect("read invalid settings bytes");
        let before_runtime = state.llm_snapshot().await;

        let error = save_settings(
            State(state.clone()),
            Json(payload(
                "https://must-not-persist.example.invalid",
                "must-not-persist",
                None,
            )),
        )
        .await
        .expect_err("candidate validation must fail before persistence");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            tokio::fs::read(&settings_path)
                .await
                .expect("read settings after rejected save"),
            before_disk
        );
        let after_runtime = state.llm_snapshot().await;
        assert!(Arc::ptr_eq(&before_runtime, &after_runtime));
        assert_eq!(after_runtime.revision(), 0);
        assert_eq!(after_runtime.settings().model, INITIAL_MODEL);
    }

    #[tokio::test]
    async fn a2a_agents_hot_swap_mask_secrets_preserve_unknown_keys_and_clear_explicitly() {
        if rerun_without_llm_env(
            "a2a_agents_hot_swap_mask_secrets_preserve_unknown_keys_and_clear_explicitly",
        ) {
            return;
        }

        let test_dir = TestDir::new("a2a-hot-swap");
        let state = build_test_state(&test_dir, true).await;
        let settings_path = test_dir.path().join("settings.json");
        let mut root: Value = serde_json::from_str(
            &tokio::fs::read_to_string(&settings_path)
                .await
                .expect("read initial settings"),
        )
        .expect("parse initial settings");
        root.as_object_mut()
            .unwrap()
            .insert("future_feature".into(), json!({"keep": true}));
        write_private_settings(&settings_path, &serde_json::to_vec_pretty(&root).unwrap())
            .await
            .expect("install unknown root key");

        let secret = ["a2a", "fixture", "secret"].join("-");
        let old_runtime = state.runtime_snapshot().await;
        let mut first = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        first.a2a_agents = vec![a2a_payload("nuclear", Some(secret.clone()), false)];
        let Json(saved) = save_settings(State(state.clone()), Json(first))
            .await
            .expect("save disabled A2A Agent without network discovery");

        assert_eq!(saved.revision, 1);
        assert_eq!(saved.a2a_agents.len(), 1);
        assert_eq!(saved.a2a_agents[0].status, "disabled");
        assert!(saved.a2a_agents[0].bearer_token.is_none());
        assert!(!saved.a2a_agents[0].bearer_token_masked.is_empty());
        assert!(saved.a2a_agents[0].tool_name.starts_with("a2a_agent_"));
        let public_json = serde_json::to_value(&saved).unwrap();
        assert!(public_json["a2a_agents"][0].get("bearer_token").is_none());
        assert!(!public_json.to_string().contains(&secret));

        let current_runtime = state.runtime_snapshot().await;
        assert!(!Arc::ptr_eq(&old_runtime, &current_runtime));
        assert!(old_runtime.a2a().agents.is_empty());
        assert_eq!(current_runtime.a2a().agents.len(), 1);
        assert_eq!(
            current_runtime.a2a().agents[0]
                .config
                .bearer_token
                .as_deref(),
            Some(secret.as_str())
        );

        // Sending the public response back with no new token must preserve the private value.
        let Json(saved_again) = save_settings(State(state.clone()), Json(saved))
            .await
            .expect("preserve omitted Bearer token");
        assert_eq!(saved_again.revision, 2);
        assert!(!saved_again.a2a_agents[0].bearer_token_masked.is_empty());

        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(&settings_path)
                .await
                .expect("read persisted settings"),
        )
        .expect("parse persisted settings");
        assert_eq!(persisted["future_feature"]["keep"], true);
        assert_eq!(
            persisted["a2a_agents"][0]["bearer_token"].as_str(),
            Some(secret.as_str())
        );

        let mut clear_token = saved_again;
        clear_token.a2a_agents[0].clear_bearer_token = true;
        let Json(token_cleared) = save_settings(State(state.clone()), Json(clear_token))
            .await
            .expect("explicitly clear Bearer token");
        assert_eq!(token_cleared.revision, 3);
        assert!(token_cleared.a2a_agents[0].bearer_token_masked.is_empty());
        assert!(state.a2a_snapshot().await.agents[0]
            .config
            .bearer_token
            .is_none());
        let persisted: Value = serde_json::from_str(
            &tokio::fs::read_to_string(&settings_path)
                .await
                .expect("read settings after token clear"),
        )
        .expect("parse settings after token clear");
        assert!(persisted["a2a_agents"][0].get("bearer_token").is_none());

        let mut clear = token_cleared;
        clear.a2a_agents.clear();
        let Json(cleared) = save_settings(State(state.clone()), Json(clear))
            .await
            .expect("explicitly clear A2A Agents");
        assert_eq!(cleared.revision, 4);
        assert!(cleared.a2a_agents.is_empty());
        assert!(state.a2a_snapshot().await.agents.is_empty());
        let restarted = state
            .settings
            .reload_persisted_a2a_agents()
            .expect("reload cleared A2A list");
        assert!(restarted.is_empty());
    }

    #[tokio::test]
    async fn changing_a2a_endpoint_never_forwards_the_previous_bearer() {
        if rerun_without_llm_env("changing_a2a_endpoint_never_forwards_the_previous_bearer") {
            return;
        }

        let test_dir = TestDir::new("a2a-endpoint-authority");
        let state = build_test_state(&test_dir, true).await;
        let secret = ["old", "endpoint", "secret"].join("-");
        let mut first = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        first.a2a_agents = vec![a2a_payload("authority", Some(secret.clone()), false)];
        let Json(mut saved) = save_settings(State(state.clone()), Json(first))
            .await
            .expect("save original endpoint");
        assert!(!saved.a2a_agents[0].bearer_token_masked.is_empty());

        saved.a2a_agents[0].endpoint = "http://127.0.0.1:8".into();
        let Json(changed) = save_settings(State(state.clone()), Json(saved))
            .await
            .expect("change endpoint without a replacement credential");
        assert!(changed.a2a_agents[0].bearer_token_masked.is_empty());
        assert!(state.a2a_snapshot().await.agents[0]
            .config
            .bearer_token
            .is_none());
        let persisted = tokio::fs::read_to_string(test_dir.path().join("settings.json"))
            .await
            .expect("read changed settings");
        assert!(!persisted.contains(&secret));
    }

    #[tokio::test]
    async fn invalid_or_duplicate_a2a_configuration_changes_neither_disk_nor_runtime() {
        if rerun_without_llm_env(
            "invalid_or_duplicate_a2a_configuration_changes_neither_disk_nor_runtime",
        ) {
            return;
        }
        let test_dir = TestDir::new("a2a-invalid");
        let state = build_test_state(&test_dir, true).await;
        let settings_path = test_dir.path().join("settings.json");
        let before_disk = tokio::fs::read(&settings_path).await.unwrap();
        let before_runtime = state.runtime_snapshot().await;

        let mut invalid = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        invalid.a2a_agents = vec![
            a2a_payload("duplicate", None, false),
            a2a_payload("duplicate", None, false),
        ];
        let error = save_settings(State(state.clone()), Json(invalid))
            .await
            .expect_err("duplicate ids must be rejected");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(tokio::fs::read(&settings_path).await.unwrap(), before_disk);
        assert!(Arc::ptr_eq(
            &before_runtime,
            &state.runtime_snapshot().await
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_uses_private_permissions_under_permissive_umask() {
        const CHILD_ENV: &str = "DSS_SETTINGS_PERMISSIONS_TEST_CHILD";

        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("umask 000; exec \"$@\"")
                .arg("sh")
                .arg(std::env::current_exe().expect("resolve current test executable"))
                .arg("write_uses_private_permissions_under_permissive_umask")
                .arg("--nocapture")
                .env(CHILD_ENV, "1")
                .output()
                .expect("run permissions test under a permissive umask");
            assert!(
                output.status.success(),
                "child test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        use std::os::unix::fs::PermissionsExt;

        let test_dir = TestDir::new("permissions");
        std::fs::create_dir_all(test_dir.path()).expect("create permissive test directory");
        std::fs::set_permissions(test_dir.path(), std::fs::Permissions::from_mode(0o777))
            .expect("make test directory permissive");
        let settings_path = test_dir.path().join("settings.json");
        std::fs::write(&settings_path, b"old secret").expect("create old settings file");
        std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o666))
            .expect("make old settings file permissive");

        let runtime = tokio::runtime::Runtime::new().expect("create Tokio runtime");
        runtime
            .block_on(write_private_settings(&settings_path, b"new secret"))
            .expect("write private settings");

        let directory_mode = std::fs::metadata(test_dir.path())
            .expect("read directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&settings_path)
            .expect("read settings metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        assert_eq!(
            std::fs::read(&settings_path).expect("read settings file"),
            b"new secret"
        );
    }

    #[tokio::test]
    async fn write_cleans_temporary_file_when_rename_fails() {
        let test_dir = TestDir::new("cleanup");
        let settings_path = test_dir.path().join("settings.json");
        tokio::fs::create_dir_all(&settings_path)
            .await
            .expect("create destination directory that makes rename fail");

        write_private_settings(&settings_path, b"secret")
            .await
            .expect_err("renaming a file over a directory must fail");

        let mut entries = tokio::fs::read_dir(test_dir.path())
            .await
            .expect("list test directory");
        while let Some(entry) = entries.next_entry().await.expect("read directory entry") {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !(name.starts_with(".settings.json.") && name.ends_with(".tmp")),
                "temporary settings file was not cleaned up: {name}"
            );
        }
    }

    #[tokio::test]
    async fn save_and_read_back_multiple_providers_with_single_enabled() {
        if rerun_without_llm_env("save_and_read_back_multiple_providers_with_single_enabled") {
            return;
        }

        let test_dir = TestDir::new("multi-provider");
        let state = build_test_state(&test_dir, true).await;

        let mut multi = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        multi.providers = vec![
            ProviderSettings {
                id: "deepseek".to_string(),
                name: "DeepSeek".into(),
                base_url: INITIAL_BASE_URL.into(),
                model: Some(INITIAL_MODEL.into()),
                api_key_masked: "••••••••".into(),
                api_key: None,
                enabled: false,
            },
            ProviderSettings {
                id: "openai".to_string(),
                name: "OpenAI".into(),
                base_url: "https://api.openai.com".into(),
                model: Some("gpt-4o".into()),
                api_key_masked: String::new(),
                api_key: Some(["openai", "credential"].join("-")),
                enabled: true,
            },
        ];

        let Json(saved) = save_settings(State(state.clone()), Json(multi))
            .await
            .expect("save multiple providers");
        assert_eq!(saved.providers.len(), 2);
        let enabled = saved
            .providers
            .iter()
            .filter(|p| p.enabled)
            .collect::<Vec<_>>();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "OpenAI");
        assert_eq!(enabled[0].model, Some("gpt-4o".to_string()));

        let runtime = state.llm_snapshot().await;
        assert_eq!(runtime.settings().base_url, "https://api.openai.com");
        assert_eq!(runtime.settings().model, "gpt-4o");
        assert!(runtime.is_configured());

        let Json(fetched) = get_settings(State(state)).await.expect("get settings");
        assert_eq!(fetched.providers.len(), 2);
        assert_eq!(fetched.providers.iter().filter(|p| p.enabled).count(), 1);
    }

    #[tokio::test]
    async fn save_rejects_zero_or_multiple_enabled_providers() {
        if rerun_without_llm_env("save_rejects_zero_or_multiple_enabled_providers") {
            return;
        }

        let test_dir = TestDir::new("provider-selection");
        let state = build_test_state(&test_dir, true).await;

        let mut none_enabled = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        none_enabled.providers[0].enabled = false;
        let err = save_settings(State(state.clone()), Json(none_enabled))
            .await
            .expect_err("zero enabled providers must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        let mut both_enabled = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        both_enabled.providers.push(ProviderSettings {
            id: "second".to_string(),
            name: "Second".into(),
            base_url: "https://second.example".into(),
            model: Some("second-model".into()),
            api_key_masked: String::new(),
            api_key: None,
            enabled: true,
        });
        let err = save_settings(State(state.clone()), Json(both_enabled))
            .await
            .expect_err("two enabled providers must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_rejects_duplicate_provider_names() {
        if rerun_without_llm_env("save_rejects_duplicate_provider_names") {
            return;
        }

        let test_dir = TestDir::new("duplicate-provider-names");
        let state = build_test_state(&test_dir, true).await;

        let mut dup = payload(INITIAL_BASE_URL, INITIAL_MODEL, None);
        dup.providers.push(ProviderSettings {
            id: "other".to_string(),
            name: "DeepSeek".into(),
            base_url: "https://other.example".into(),
            model: Some("other-model".into()),
            api_key_masked: String::new(),
            api_key: None,
            enabled: false,
        });
        let err = save_settings(State(state.clone()), Json(dup))
            .await
            .expect_err("duplicate provider names must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
}
