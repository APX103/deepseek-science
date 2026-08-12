//! Bounded MCP Streamable-HTTP client.
//!
//! The currently deployed Registry negotiates the legacy `2024-11-05` lifecycle:
//! initialize -> optional `Mcp-Session-Id` -> notifications/initialized.  The
//! client retains the negotiated capabilities and exposes typed tools and
//! Resources APIs while accepting either JSON or SSE responses.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_RESOURCE_LIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_PAGES: usize = 32;
const MAX_RESOURCES: usize = 1_024;
const MAX_STRING_BYTES: usize = 16 * 1024;
pub const MAX_MCP_TOOLS: usize = 30;
pub const MAX_MCP_TOOL_LIST_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc error ({code}): {message}")]
    Rpc { code: i64, message: String },
    #[error("invalid response: {0}")]
    Invalid(String),
    #[error("MCP server does not advertise Resources capability")]
    ResourcesUnsupported,
    #[error("MCP response exceeds {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("MCP resources/list aggregate exceeds {limit} bytes")]
    ResourceListTooLarge { limit: usize },
    #[error("MCP tools/list exceeds {limit} entries")]
    ToolCountExceeded { limit: usize },
    #[error("MCP tools/list aggregate exceeds {limit} bytes")]
    ToolListTooLarge { limit: usize },
    #[error("MCP tool reported an error: {0}")]
    ToolReported(String),
    #[error("unsupported MCP tool content type: {0}")]
    UnsupportedToolContent(String),
}

/// Runtime-only network routing. Hostnames remain in URLs (and therefore TLS
/// SNI/certificate verification); `resolve` supplies socket destinations only.
#[derive(Debug, Clone, Default)]
pub struct McpRouteOptions {
    pub interface: Option<String>,
    pub resolve: HashMap<String, SocketAddr>,
}

/// One MCP tool definition.
#[derive(Debug, Clone, Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "McpToolAnnotations::is_empty")]
    pub annotations: McpToolAnnotations,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct McpToolAnnotations {
    #[serde(
        default,
        rename = "readOnlyHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    #[serde(
        default,
        rename = "destructiveHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
    #[serde(
        default,
        rename = "idempotentHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent_hint: Option<bool>,
}

impl McpToolAnnotations {
    pub fn is_empty(&self) -> bool {
        self.read_only_hint.is_none()
            && self.destructive_hint.is_none()
            && self.idempotent_hint.is_none()
    }

    /// Only explicit remote claims can relax the fail-safe mutation policy. Read-only is safe
    /// unless contradicted by destructive=true; mutating tools need both idempotent=true and
    /// destructive=false. Missing or ambiguous annotations remain one-attempt/exclusive.
    pub fn is_retry_safe(&self) -> bool {
        (self.read_only_hint == Some(true) && self.destructive_hint != Some(true))
            || (self.idempotent_hint == Some(true) && self.destructive_hint == Some(false))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerCapabilities {
    pub tools: bool,
    pub resources: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerMetadata {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub capabilities: McpServerCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

pub struct MCPClient {
    http: reqwest::Client,
    base_url: String,
    session_id: Mutex<Option<String>>,
    metadata: Mutex<Option<McpServerMetadata>>,
    connected: AtomicBool,
    next_id: AtomicU64,
}

impl MCPClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::try_new_with_route_options(base_url, McpRouteOptions::default())
            .unwrap_or_else(|error| panic!("failed to build MCP client: {error}"))
    }

    pub fn try_new_with_route_options(
        base_url: impl Into<String>,
        options: McpRouteOptions,
    ) -> Result<Self, McpError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let parsed_url = reqwest::Url::parse(&base_url)
            .map_err(|_| McpError::Invalid("MCP endpoint is not a valid URL".into()))?;
        if !matches!(parsed_url.scheme(), "http" | "https") || parsed_url.host_str().is_none() {
            return Err(McpError::Invalid(
                "MCP endpoint must be absolute http or https".into(),
            ));
        }
        if !parsed_url.username().is_empty()
            || parsed_url.password().is_some()
            || parsed_url.query().is_some()
            || parsed_url.fragment().is_some()
        {
            return Err(McpError::Invalid(
                "MCP endpoint credentials, query, and fragment are forbidden".into(),
            ));
        }
        let routed = options.interface.is_some() || !options.resolve.is_empty();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none());
        if routed {
            // Do not let an environment proxy re-resolve the hostname and bypass
            // the explicit socket/interface route. The URL hostname remains
            // unchanged for HTTP Host and TLS SNI/certificate verification.
            builder = builder.no_proxy();
        }
        if let Some(interface) = options.interface.as_deref() {
            builder = apply_interface(builder, interface)?;
        }
        for (host, address) in options.resolve {
            if host.trim().is_empty() {
                return Err(McpError::Invalid(
                    "route override hostname must not be empty".into(),
                ));
            }
            builder = builder.resolve(&host, address);
        }
        Ok(Self {
            http: builder.build()?,
            base_url,
            session_id: Mutex::new(None),
            metadata: Mutex::new(None),
            connected: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn req_builder(&self) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(&self.base_url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Ok(session) = self.session_id.lock() {
            if let Some(session) = session.as_ref() {
                request = request.header("Mcp-Session-Id", session);
            }
        }
        request
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id();
        let body = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        let response = self.req_builder().json(&body).send().await?;
        self.capture_session(&response)?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let status = response.status();
        let bytes = read_bounded(response, MAX_RESPONSE_BYTES).await?;
        let text = String::from_utf8(bytes)
            .map_err(|_| McpError::Invalid("response body is not UTF-8".into()))?;
        if !status.is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {status}: {}",
                truncate(&text, 300)
            )));
        }
        let value = parse_response_value(&text, content_type.as_deref())?;
        let object = value
            .as_object()
            .ok_or_else(|| McpError::Invalid("response not an object".into()))?;
        if object.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(McpError::Invalid(format!(
                "response id does not match request {id}"
            )));
        }
        result_or_error(object)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = json!({"jsonrpc":"2.0", "method":method, "params":params});
        let response = self.req_builder().json(&body).send().await?;
        self.capture_session(&response)?;
        let status = response.status();
        let bytes = read_bounded(response, MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            return Err(McpError::Transport(format!(
                "HTTP {status}: {}",
                truncate(&text, 300)
            )));
        }
        if bytes.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(McpError::Invalid(
                "notification response body must be empty".into(),
            ));
        }
        Ok(())
    }

    fn capture_session(&self, response: &Response) -> Result<(), McpError> {
        if let Some(header) = response.headers().get("Mcp-Session-Id") {
            let session = header
                .to_str()
                .map_err(|_| McpError::Invalid("MCP session id is not valid ASCII".into()))?;
            validate_identifier(session, "MCP session id", 2_048)?;
            if let Ok(mut stored) = self.session_id.lock() {
                *stored = Some(session.to_owned());
            }
        }
        Ok(())
    }

    pub async fn connect(&self) -> Result<(), McpError> {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name":"deepseek-science", "version":"0.1.0"}
                }),
            )
            .await?;
        let protocol_version = required_string(&result, "protocolVersion", 64)?;
        if protocol_version != PROTOCOL_VERSION {
            return Err(McpError::Invalid(format!(
                "server negotiated unsupported protocol version {protocol_version}"
            )));
        }
        let capabilities = result
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| McpError::Invalid("initialize capabilities missing".into()))?;
        let server_info = result
            .get("serverInfo")
            .and_then(Value::as_object)
            .ok_or_else(|| McpError::Invalid("initialize serverInfo missing".into()))?;
        let metadata = McpServerMetadata {
            protocol_version,
            server_name: required_object_string(server_info, "name", 256)?,
            server_version: required_object_string(server_info, "version", 128)?,
            capabilities: McpServerCapabilities {
                tools: capabilities.get("tools").is_some_and(Value::is_object),
                resources: capabilities.get("resources").is_some_and(Value::is_object),
            },
        };
        self.notify("notifications/initialized", json!({})).await?;
        *self
            .metadata
            .lock()
            .map_err(|_| McpError::Invalid("metadata lock poisoned".into()))? = Some(metadata);
        self.connected.store(true, Ordering::Release);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn metadata(&self) -> Option<McpServerMetadata> {
        self.metadata.lock().ok().and_then(|value| value.clone())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let result = self.rpc("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Invalid("tools/list result.tools not array".into()))?;
        if tools.len() > MAX_MCP_TOOLS {
            return Err(McpError::ToolCountExceeded {
                limit: MAX_MCP_TOOLS,
            });
        }
        let mut aggregate_bytes = 0usize;
        let mut output = Vec::with_capacity(tools.len());
        for tool in tools {
            aggregate_bytes = checked_tool_list_total(aggregate_bytes, tool)?;
            match parse_tool(tool) {
                Ok(tool) => output.push(tool),
                Err(error) => {
                    tracing::warn!(error = %error, "skipping invalid MCP tool definition")
                }
            }
        }
        Ok(output)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let result = self
            .rpc("tools/call", json!({"name":name, "arguments":arguments}))
            .await?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Invalid("tools/call result.content not array".into()))?;
        let is_error = match result.get("isError") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| McpError::Invalid("tools/call isError must be boolean".into()))?,
        };
        let mut parts = Vec::new();
        let mut aggregate_bytes = 0usize;
        for item in content {
            let content_type = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| McpError::Invalid("tool content type must be a string".into()))?;
            if content_type != "text" {
                return Err(McpError::UnsupportedToolContent(content_type.to_owned()));
            }
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| McpError::Invalid("text tool content needs text".into()))?;
            aggregate_bytes = aggregate_bytes
                .checked_add(text.len())
                .and_then(|total| total.checked_add(if parts.is_empty() { 0 } else { 1 }))
                .filter(|total| *total <= MAX_RESOURCE_CONTENT_BYTES)
                .ok_or(McpError::ResponseTooLarge {
                    limit: MAX_RESOURCE_CONTENT_BYTES,
                })?;
            parts.push(text.to_owned());
        }
        let output = parts.join("\n");
        if is_error {
            return Err(McpError::ToolReported(if output.is_empty() {
                "remote MCP tool returned no text details".into()
            } else {
                output
            }));
        }
        if parts.is_empty() {
            return Err(McpError::Invalid(
                "tools/call returned no supported text content".into(),
            ));
        }
        Ok(output)
    }

    /// List all Resources in deterministic server order, following bounded pagination.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        self.require_resources()?;
        let mut output = Vec::new();
        let mut aggregate_bytes = 0usize;
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_RESOURCE_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({"cursor":cursor}))
                .unwrap_or_else(|| json!({}));
            let result = self.rpc("resources/list", params).await?;
            let resources = result
                .get("resources")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    McpError::Invalid("resources/list result.resources not array".into())
                })?;
            for value in resources {
                if output.len() >= MAX_RESOURCES {
                    return Err(McpError::Invalid(format!(
                        "resources/list exceeds {MAX_RESOURCES} entries"
                    )));
                }
                // Bound the complete cross-page catalog before cloning any Resource strings
                // into the returned collection. Counting each raw Resource's compact JSON also
                // prevents ignored extension fields from bypassing the aggregate memory budget.
                aggregate_bytes = checked_resource_list_total(aggregate_bytes, value)?;
                output.push(parse_resource(value)?);
            }
            let next = optional_string(&result, "nextCursor", 2_048)?;
            match next {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.clone()) {
                        return Err(McpError::Invalid(
                            "resources/list cursor cycle detected".into(),
                        ));
                    }
                    cursor = Some(next);
                }
                _ => return Ok(output),
            }
        }
        Err(McpError::Invalid(format!(
            "resources/list exceeds {MAX_RESOURCE_PAGES} pages"
        )))
    }

    pub async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContent>, McpError> {
        self.require_resources()?;
        if uri.is_empty() || uri.len() > MAX_STRING_BYTES || uri.chars().any(char::is_control) {
            return Err(McpError::Invalid("resource URI is empty or unsafe".into()));
        }
        let result = self.rpc("resources/read", json!({"uri":uri})).await?;
        let contents = result
            .get("contents")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Invalid("resources/read result.contents not array".into()))?;
        if contents.len() > 64 {
            return Err(McpError::Invalid(
                "resources/read returned too many content items".into(),
            ));
        }
        contents
            .iter()
            .map(|value| parse_resource_content(value, uri))
            .collect()
    }

    fn require_resources(&self) -> Result<(), McpError> {
        match self.metadata() {
            Some(metadata) if metadata.capabilities.resources => Ok(()),
            Some(_) => Err(McpError::ResourcesUnsupported),
            None => Err(McpError::Invalid("MCP client is not connected".into())),
        }
    }
}

fn checked_resource_list_total(current: usize, value: &Value) -> Result<usize, McpError> {
    let resource_bytes = serde_json::to_vec(value)
        .map_err(|error| McpError::Invalid(format!("resource cannot be serialized: {error}")))?
        .len();
    current
        .checked_add(resource_bytes)
        .filter(|total| *total <= MAX_RESOURCE_LIST_BYTES)
        .ok_or(McpError::ResourceListTooLarge {
            limit: MAX_RESOURCE_LIST_BYTES,
        })
}

fn checked_tool_list_total(current: usize, value: &Value) -> Result<usize, McpError> {
    // Count the complete compact definition, including extension fields, before cloning the
    // schema into the run-time catalog. Otherwise an MCP server could hide an unbounded payload
    // beside an apparently small inputSchema and inflate every model request that mounts it.
    let definition_bytes = serde_json::to_vec(value)
        .map_err(|error| McpError::Invalid(format!("tool cannot be serialized: {error}")))?
        .len();
    current
        .checked_add(definition_bytes)
        .filter(|total| *total <= MAX_MCP_TOOL_LIST_BYTES)
        .ok_or(McpError::ToolListTooLarge {
            limit: MAX_MCP_TOOL_LIST_BYTES,
        })
}

fn parse_tool(value: &Value) -> Result<McpTool, McpError> {
    let name = required_string(value, "name", 256)?;
    let description =
        optional_display_string(value, "description", MAX_STRING_BYTES)?.unwrap_or_default();
    let input_schema = value
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({}));
    validate_tool_input_schema(&input_schema)
        .map_err(|message| McpError::Invalid(format!("tool {name} inputSchema: {message}")))?;
    let annotations = parse_tool_annotations(value.get("annotations"))?;
    Ok(McpTool {
        name,
        description,
        input_schema,
        annotations,
    })
}

fn parse_tool_annotations(value: Option<&Value>) -> Result<McpToolAnnotations, McpError> {
    let Some(value) = value else {
        return Ok(McpToolAnnotations::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| McpError::Invalid("tool annotations must be an object".into()))?;
    Ok(McpToolAnnotations {
        read_only_hint: optional_annotation_bool(object, "readOnlyHint")?,
        destructive_hint: optional_annotation_bool(object, "destructiveHint")?,
        idempotent_hint: optional_annotation_bool(object, "idempotentHint")?,
    })
}

fn optional_annotation_bool(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, McpError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| McpError::Invalid(format!("tool annotation {key} must be boolean")))
        })
        .transpose()
}

fn validate_tool_input_schema(schema: &Value) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| "must be an object".to_string())?;
    match object.get("type") {
        None => {}
        Some(Value::String(kind)) if kind == "object" => {}
        Some(Value::String(_)) => return Err("top-level type must be object".into()),
        Some(_) => return Err("top-level type must be a string".into()),
    }
    validate_schema_object(object, true)
}

fn validate_schema_object(
    object: &serde_json::Map<String, Value>,
    top_level: bool,
) -> Result<(), String> {
    const ALLOWED_KEYWORDS: &[&str] = &[
        "type",
        "title",
        "description",
        "default",
        "examples",
        "enum",
        "const",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "format",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
    ];

    for key in object.keys() {
        if !ALLOWED_KEYWORDS.contains(&key.as_str()) {
            return Err(format!("unsupported JSON Schema keyword {key}"));
        }
    }

    if let Some(kind) = object.get("type") {
        let kind = kind
            .as_str()
            .ok_or_else(|| "type must be a string".to_string())?;
        if !matches!(
            kind,
            "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
        ) {
            return Err(format!("unsupported JSON Schema type {kind}"));
        }
        if top_level && kind != "object" {
            return Err("top-level type must be object".into());
        }
    }

    let properties = match object.get("properties") {
        None => None,
        Some(Value::Object(properties)) => Some(properties),
        Some(_) => return Err("properties must be an object".into()),
    };
    if let Some(properties) = properties {
        for (name, property) in properties {
            let property = property
                .as_object()
                .ok_or_else(|| format!("property {name} schema must be an object"))?;
            validate_schema_object(property, false)?;
        }
    }

    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| "required must be an array".to_string())?;
        let properties = properties.ok_or_else(|| "required needs properties".to_string())?;
        let mut seen = HashSet::new();
        for name in required {
            let name = name
                .as_str()
                .ok_or_else(|| "required entries must be strings".to_string())?;
            if !seen.insert(name) {
                return Err(format!("required contains duplicate property {name}"));
            }
            if !properties.contains_key(name) {
                return Err(format!("required property {name} is not in properties"));
            }
        }
    }

    if let Some(additional) = object.get("additionalProperties") {
        if !additional.is_boolean() {
            return Err("additionalProperties must be boolean".into());
        }
    }
    if let Some(items) = object.get("items") {
        let items = items
            .as_object()
            .ok_or_else(|| "items must be a schema object".to_string())?;
        validate_schema_object(items, false)?;
    }
    validate_schema_keyword_types(object)
}

fn validate_schema_keyword_types(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    for key in ["title", "description", "pattern", "format"] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return Err(format!("{key} must be a string"));
        }
    }
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        if object.get(key).is_some_and(|value| !value.is_number()) {
            return Err(format!("{key} must be a number"));
        }
    }
    for key in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
    ] {
        if object
            .get(key)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(format!("{key} must be an unsigned integer"));
        }
    }
    if object
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("uniqueItems must be boolean".into());
    }
    if object.get("enum").is_some_and(|value| !value.is_array()) {
        return Err("enum must be an array".into());
    }
    if object
        .get("examples")
        .is_some_and(|value| !value.is_array())
    {
        return Err("examples must be an array".into());
    }
    Ok(())
}

fn parse_resource(value: &Value) -> Result<McpResource, McpError> {
    Ok(McpResource {
        uri: required_string(value, "uri", MAX_STRING_BYTES)?,
        name: required_string(value, "name", 1_024)?,
        title: optional_string(value, "title", 1_024)?,
        description: optional_display_string(value, "description", MAX_STRING_BYTES)?,
        mime_type: optional_string(value, "mimeType", 256)?,
        size: value
            .get("size")
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| McpError::Invalid("resource size must be unsigned".into()))
            })
            .transpose()?,
    })
}

fn parse_resource_content(
    value: &Value,
    requested_uri: &str,
) -> Result<McpResourceContent, McpError> {
    let uri = required_string(value, "uri", MAX_STRING_BYTES)?;
    if uri != requested_uri {
        return Err(McpError::Invalid(
            "resources/read content URI does not match the requested URI".into(),
        ));
    }
    let text = optional_content_string(value, "text", MAX_RESOURCE_CONTENT_BYTES)?;
    let blob = optional_string(value, "blob", MAX_RESOURCE_CONTENT_BYTES)?;
    if text.is_some() == blob.is_some() {
        return Err(McpError::Invalid(
            "resource content must contain exactly one of text or blob".into(),
        ));
    }
    if let Some(blob) = blob.as_deref() {
        BASE64_STANDARD
            .decode(blob)
            .map_err(|_| McpError::Invalid("resource blob is not valid base64".into()))?;
    }
    Ok(McpResourceContent {
        uri,
        mime_type: optional_string(value, "mimeType", 256)?,
        text,
        blob,
    })
}

async fn read_bounded(mut response: Response, limit: usize) -> Result<Vec<u8>, McpError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(McpError::ResponseTooLarge { limit });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(McpError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_response_value(text: &str, content_type: Option<&str>) -> Result<Value, McpError> {
    if content_type.is_some_and(|value| value.contains("text/event-stream")) {
        let mut last = None;
        for line in text.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim();
                if !payload.is_empty() {
                    if let Ok(value) = serde_json::from_str(payload) {
                        last = Some(value);
                    }
                }
            }
        }
        last.ok_or_else(|| McpError::Invalid("SSE response had no parseable data".into()))
    } else {
        serde_json::from_str(text)
            .map_err(|error| McpError::Invalid(format!("non-JSON body: {error}")))
    }
}

fn result_or_error(object: &serde_json::Map<String, Value>) -> Result<Value, McpError> {
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::Invalid("response jsonrpc must equal 2.0".into()));
    }
    if let Some(error) = object.get("error") {
        return Err(McpError::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-1),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        });
    }
    object
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Invalid("response has neither result nor error".into()))
}

/// Backwards-compatible public parser used by existing tests/callers.
pub enum ResponsePayload {
    Result(Value),
}

pub fn parse_response(text: &str, content_type: Option<&str>) -> Result<ResponsePayload, McpError> {
    let value = parse_response_value(text, content_type)?;
    let object = value
        .as_object()
        .ok_or_else(|| McpError::Invalid("response not an object".into()))?;
    Ok(ResponsePayload::Result(result_or_error(object)?))
}

fn required_string(value: &Value, key: &str, max: usize) -> Result<String, McpError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::Invalid(format!("{key} must be a string")))
        .and_then(|value| {
            validate_identifier(value, key, max)?;
            Ok(value.to_owned())
        })
}

fn required_object_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<String, McpError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::Invalid(format!("{key} must be a string")))
        .and_then(|value| {
            validate_identifier(value, key, max)?;
            Ok(value.to_owned())
        })
}

fn optional_string(value: &Value, key: &str, max: usize) -> Result<Option<String>, McpError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if value.len() <= max && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(McpError::Invalid(format!("{key} must be a bounded string"))),
    }
}

/// Display metadata may contain the whitespace conventionally used by Markdown,
/// but still rejects non-whitespace control characters.
fn optional_display_string(
    value: &Value,
    key: &str,
    max: usize,
) -> Result<Option<String>, McpError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if value.len() <= max
                && !value.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                }) =>
        {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(McpError::Invalid(format!(
            "{key} must be a bounded display string"
        ))),
    }
}

/// Resource text is opaque UTF-8 content. `serde_json` has already established
/// valid UTF-8, so only the protocol byte ceiling applies here.
fn optional_content_string(
    value: &Value,
    key: &str,
    max: usize,
) -> Result<Option<String>, McpError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= max => Ok(Some(value.clone())),
        Some(_) => Err(McpError::Invalid(format!(
            "{key} must be a bounded UTF-8 string"
        ))),
    }
}

fn validate_identifier(value: &str, label: &str, max: usize) -> Result<(), McpError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(McpError::Invalid(format!(
            "{label} must be a bounded non-empty string without control characters"
        )));
    }
    Ok(())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        value.chars().take(max).chain(['…']).collect()
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn apply_interface(
    builder: reqwest::ClientBuilder,
    interface: &str,
) -> Result<reqwest::ClientBuilder, McpError> {
    if interface.trim().is_empty() || interface.chars().any(char::is_control) {
        return Err(McpError::Invalid(
            "network interface is empty or unsafe".into(),
        ));
    }
    Ok(builder.interface(interface))
}

#[cfg(not(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
)))]
fn apply_interface(
    _builder: reqwest::ClientBuilder,
    _interface: &str,
) -> Result<reqwest::ClientBuilder, McpError> {
    Err(McpError::Invalid(
        "network interface binding is unsupported on this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_json_result() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let ResponsePayload::Result(value) =
            parse_response(body, Some("application/json")).unwrap();
        assert!(value["tools"].as_array().unwrap().is_empty());
    }

    #[test]
    fn parse_sse_takes_last_data() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":2}}\n\n";
        let ResponsePayload::Result(value) =
            parse_response(body, Some("text/event-stream")).unwrap();
        assert_eq!(value["a"], 2);
    }

    #[test]
    fn parse_error() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"bad"}}"#;
        assert!(matches!(
            parse_response(body, Some("application/json")),
            Err(McpError::Rpc { code: -32600, .. })
        ));
    }

    #[test]
    fn resource_description_allows_markdown_whitespace_but_rejects_other_controls() {
        let description = "# Agent\r\n\n- first\n\t- nested";
        let resource = parse_resource(&json!({
            "uri": "agent://fixture",
            "name": "fixture",
            "description": description
        }))
        .unwrap();
        assert_eq!(resource.description.as_deref(), Some(description));

        for control in ['\0', '\u{000b}', '\u{0085}'] {
            let value = json!({
                "uri": "agent://fixture",
                "name": "fixture",
                "description": format!("unsafe{control}description")
            });
            assert!(matches!(parse_resource(&value), Err(McpError::Invalid(_))));
        }
    }

    #[test]
    fn resource_list_aggregate_budget_accepts_exact_boundary_and_rejects_next_byte() {
        let resource = json!({
            "uri": "agent://fixture",
            "name": "fixture",
            "description": "serialized budget boundary"
        });
        let resource_bytes = serde_json::to_vec(&resource).unwrap().len();
        let exact_start = MAX_RESOURCE_LIST_BYTES - resource_bytes;

        assert_eq!(
            checked_resource_list_total(exact_start, &resource).unwrap(),
            MAX_RESOURCE_LIST_BYTES
        );
        assert!(matches!(
            checked_resource_list_total(exact_start + 1, &resource),
            Err(McpError::ResourceListTooLarge {
                limit: MAX_RESOURCE_LIST_BYTES
            })
        ));
        assert!(matches!(
            checked_resource_list_total(usize::MAX, &resource),
            Err(McpError::ResourceListTooLarge {
                limit: MAX_RESOURCE_LIST_BYTES
            })
        ));
    }

    #[test]
    fn tool_list_aggregate_budget_accepts_exact_boundary_and_rejects_next_byte() {
        let tool = json!({
            "name": "fixture",
            "description": "serialized budget boundary",
            "inputSchema": {"type": "object"}
        });
        let tool_bytes = serde_json::to_vec(&tool).unwrap().len();
        let exact_start = MAX_MCP_TOOL_LIST_BYTES - tool_bytes;

        assert_eq!(
            checked_tool_list_total(exact_start, &tool).unwrap(),
            MAX_MCP_TOOL_LIST_BYTES
        );
        assert!(matches!(
            checked_tool_list_total(exact_start + 1, &tool),
            Err(McpError::ToolListTooLarge {
                limit: MAX_MCP_TOOL_LIST_BYTES
            })
        ));
        assert!(matches!(
            checked_tool_list_total(usize::MAX, &tool),
            Err(McpError::ToolListTooLarge {
                limit: MAX_MCP_TOOL_LIST_BYTES
            })
        ));
    }

    #[test]
    fn resource_text_is_only_utf8_and_byte_bounded() {
        let text = "# Markdown\r\n\tcontent\n\0opaque";
        let content = parse_resource_content(
            &json!({"uri": "agent://fixture", "text": text}),
            "agent://fixture",
        )
        .unwrap();
        assert_eq!(content.text.as_deref(), Some(text));

        let oversized = "x".repeat(MAX_RESOURCE_CONTENT_BYTES + 1);
        assert!(matches!(
            parse_resource_content(
                &json!({"uri": "agent://fixture", "text": oversized}),
                "agent://fixture"
            ),
            Err(McpError::Invalid(_))
        ));
    }

    #[test]
    fn identifiers_remain_control_strict() {
        for identifier in ["agent://fixture\n", "fixture\t", "session\r"] {
            assert!(matches!(
                validate_identifier(identifier, "identifier", 128),
                Err(McpError::Invalid(_))
            ));
        }
        assert!(validate_identifier("agent://fixture", "identifier", 128).is_ok());

        for value in [
            json!({"uri": "agent://fixture\n", "name": "fixture"}),
            json!({"uri": "agent://fixture", "name": "fixture\t"}),
        ] {
            assert!(matches!(parse_resource(&value), Err(McpError::Invalid(_))));
        }
        assert!(matches!(
            optional_string(&json!({"nextCursor": "page\r2"}), "nextCursor", 128),
            Err(McpError::Invalid(_))
        ));
    }
}
