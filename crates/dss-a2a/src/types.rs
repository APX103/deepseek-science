use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use dss_core::A2aAgentConfig;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::A2aError;

pub const MAX_AGENT_COUNT: usize = 16;
pub const MAX_CARD_BYTES: usize = 256 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BYTES: usize = 2_048;
pub(crate) const MAX_TASK_BYTES: usize = 256 * 1024;
pub(crate) const MIN_TIMEOUT_SECONDS: u64 = 5;
pub(crate) const MAX_TIMEOUT_SECONDS: u64 = 300;

/// Optional, runtime-only socket routing for A2A HTTP traffic.
///
/// Resolution overrides keep the original URL hostname intact, so TLS SNI and the HTTP Host
/// header continue to identify the configured Agent. They are deliberately client construction
/// data and are never persisted into an Agent configuration or Agent Card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct A2aRouteOptions {
    pub interface: Option<String>,
    pub resolve: HashMap<String, SocketAddr>,
}

/// Explicit provenance assertion for an anonymous Agent endpoint read from a Registry.
///
/// Construct this only after the caller has bound an exact Registry Resource to a descriptor and
/// validated that the descriptor declares anonymous authentication. The A2A client independently
/// enforces HTTPS, exact endpoint identity, same-origin card discovery, and its narrow card repair
/// rules; this policy never carries credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryInvocationPolicy {
    descriptor_endpoint: Url,
    allow_loopback_http: bool,
}

impl RegistryInvocationPolicy {
    pub fn anonymous(descriptor_endpoint: impl AsRef<str>) -> Result<Self, A2aError> {
        let descriptor_endpoint = validate_endpoint(descriptor_endpoint.as_ref())?;
        if descriptor_endpoint.scheme() != "https" {
            return Err(A2aError::InvalidEndpoint(
                "Registry-derived A2A endpoints must use HTTPS".into(),
            ));
        }
        validate_registry_production_host(&descriptor_endpoint)?;
        Ok(Self {
            descriptor_endpoint,
            allow_loopback_http: false,
        })
    }

    /// Construct an anonymous policy for a hermetic local integration fixture.
    ///
    /// This API is absent from production builds unless the explicit `test-support` feature is
    /// enabled, and even then it accepts only an HTTP endpoint whose host is a literal loopback
    /// address. It cannot authorize a hostname, wildcard bind address, or remote cleartext Agent.
    #[cfg(any(test, feature = "test-support"))]
    pub fn anonymous_loopback_for_testing(
        descriptor_endpoint: impl AsRef<str>,
    ) -> Result<Self, A2aError> {
        let descriptor_endpoint = validate_endpoint(descriptor_endpoint.as_ref())?;
        let literal_loopback = descriptor_endpoint
            .host_str()
            .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
        if descriptor_endpoint.scheme() != "http" || !literal_loopback {
            return Err(A2aError::InvalidEndpoint(
                "test Registry endpoints must use HTTP with a literal loopback host".into(),
            ));
        }
        Ok(Self {
            descriptor_endpoint,
            allow_loopback_http: true,
        })
    }

    pub fn descriptor_endpoint(&self) -> &Url {
        &self.descriptor_endpoint
    }

    pub(crate) fn allows_loopback_http(&self) -> bool {
        self.allow_loopback_http
    }

    pub(crate) fn validate_config_binding(&self, config: &A2aAgentConfig) -> Result<(), A2aError> {
        let configured_endpoint = validate_endpoint(&config.endpoint)?;
        if configured_endpoint != self.descriptor_endpoint {
            return Err(A2aError::InvalidConfig(
                "Registry invocation endpoint differs from the validated descriptor endpoint"
                    .into(),
            ));
        }
        if config.bearer_token.is_some() {
            return Err(A2aError::InvalidConfig(
                "Registry-anonymous invocation must not carry A2A credentials".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolVersion {
    V1,
    V03,
}

impl ProtocolVersion {
    pub fn wire(self) -> &'static str {
        match self {
            Self::V1 => "1.0",
            Self::V03 => "0.3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolBinding {
    JsonRpc,
    HttpJson,
}

/// Selects whether an invocation sends a new Message or resumes an existing remote Task.
///
/// `Send` is the serde/default behavior so requests persisted before this field was introduced
/// retain their original semantics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvokeAction {
    #[default]
    Send,
    /// Send one Message and return as soon as the Agent returns an in-progress Task handle.
    Submit,
    /// Issue GetTask without sending a new Message, then poll while the Task remains in progress.
    GetTask,
    /// Issue CancelTask exactly once without sending a new Message or polling.
    CancelTask,
}

/// A self-contained request that the local harness gives to one configured remote Agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeRequest {
    #[serde(default)]
    pub action: InvokeAction,
    #[serde(default)]
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl InvokeRequest {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            action: InvokeAction::Send,
            task: task.into(),
            skill_id: None,
            task_id: None,
            context_id: None,
            timeout_seconds: None,
        }
    }

    /// Construct a request that sends once and checkpoints an in-progress Task for later resume.
    pub fn submit(task: impl Into<String>) -> Self {
        Self {
            action: InvokeAction::Submit,
            ..Self::new(task)
        }
    }

    /// Construct a pure GetTask request. No Message is sent to the remote Agent.
    pub fn get_task(task_id: impl Into<String>) -> Self {
        Self {
            action: InvokeAction::GetTask,
            task: String::new(),
            skill_id: None,
            task_id: Some(task_id.into()),
            context_id: None,
            timeout_seconds: None,
        }
    }

    /// Construct a pure CancelTask request. No Message is sent to the remote Agent.
    pub fn cancel_task(task_id: impl Into<String>) -> Self {
        Self {
            action: InvokeAction::CancelTask,
            task: String::new(),
            skill_id: None,
            task_id: Some(task_id.into()),
            context_id: None,
            timeout_seconds: None,
        }
    }

    pub fn validate(&self) -> Result<(), A2aError> {
        if matches!(self.action, InvokeAction::Send | InvokeAction::Submit)
            && self.task.trim().is_empty()
        {
            return Err(A2aError::InvalidConfig("task must not be empty".into()));
        }
        if self.task.len() > MAX_TASK_BYTES {
            return Err(A2aError::InvalidConfig(format!(
                "task exceeds {MAX_TASK_BYTES} bytes"
            )));
        }
        for (label, value) in [
            ("skill_id", self.skill_id.as_deref()),
            ("task_id", self.task_id.as_deref()),
            ("context_id", self.context_id.as_deref()),
        ] {
            if value.is_some_and(|v| v.len() > 512 || v.chars().any(char::is_control)) {
                return Err(A2aError::InvalidConfig(format!(
                    "{label} is too long or contains control characters"
                )));
            }
        }
        if matches!(
            self.action,
            InvokeAction::GetTask | InvokeAction::CancelTask
        ) && self
            .task_id
            .as_deref()
            .is_none_or(|task_id| task_id.trim().is_empty())
        {
            return Err(A2aError::InvalidConfig(
                "task_id is required for get_task and cancel_task".into(),
            ));
        }
        if let Some(timeout) = self.timeout_seconds {
            validate_timeout(timeout)?;
        }
        Ok(())
    }
}

fn validate_registry_production_host(endpoint: &Url) -> Result<(), A2aError> {
    let host = endpoint
        .host_str()
        .ok_or_else(|| A2aError::InvalidEndpoint("endpoint has no host".into()))?;
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');
    if bare_host.parse::<std::net::IpAddr>().is_ok() {
        return Err(A2aError::InvalidEndpoint(
            "Registry-derived production endpoints must use a DNS hostname, not an IP literal"
                .into(),
        ));
    }
    let canonical = bare_host.trim_end_matches('.').to_ascii_lowercase();
    let local_name = ["localhost", "local", "internal"]
        .iter()
        .any(|suffix| canonical == *suffix || canonical.ends_with(&format!(".{suffix}")))
        || canonical == "home.arpa"
        || canonical.ends_with(".home.arpa");
    if local_name {
        return Err(A2aError::InvalidEndpoint(
            "Registry-derived production endpoints must not use a local hostname".into(),
        ));
    }
    Ok(())
}

pub fn validate_config(config: &A2aAgentConfig) -> Result<(), A2aError> {
    if config.id.trim().is_empty() || config.id.len() > 128 {
        return Err(A2aError::InvalidConfig(
            "id must contain 1 to 128 bytes".into(),
        ));
    }
    if config.id.chars().any(char::is_control) {
        return Err(A2aError::InvalidConfig(
            "id must not contain control characters".into(),
        ));
    }
    if config.name.trim().is_empty() || config.name.len() > 256 {
        return Err(A2aError::InvalidConfig(
            "name must contain 1 to 256 bytes".into(),
        ));
    }
    if config.name.chars().any(char::is_control) {
        return Err(A2aError::InvalidConfig(
            "name must not contain control characters".into(),
        ));
    }
    validate_timeout(config.timeout_seconds)?;
    validate_endpoint(&config.endpoint)?;
    if config.bearer_token.as_deref().is_some_and(|token| {
        token.is_empty() || token.len() > 16 * 1024 || token.contains('\r') || token.contains('\n')
    }) {
        return Err(A2aError::InvalidConfig(
            "Bearer token is empty, too long, or contains a newline".into(),
        ));
    }
    Ok(())
}

pub fn validate_configs(configs: &[A2aAgentConfig]) -> Result<(), A2aError> {
    if configs.len() > MAX_AGENT_COUNT {
        return Err(A2aError::InvalidConfig(format!(
            "at most {MAX_AGENT_COUNT} A2A Agents may be configured"
        )));
    }
    let mut ids = HashSet::with_capacity(configs.len());
    let mut names = HashSet::with_capacity(configs.len());
    let mut tools = HashSet::with_capacity(configs.len());
    for config in configs {
        validate_config(config)?;
        if !ids.insert(config.id.clone()) {
            return Err(A2aError::InvalidConfig(format!(
                "duplicate A2A Agent id: {}",
                config.id
            )));
        }
        if !names.insert(config.name.trim().to_lowercase()) {
            return Err(A2aError::InvalidConfig(format!(
                "duplicate A2A Agent name: {}",
                config.name
            )));
        }
        let tool = stable_tool_name(&config.id);
        if !tools.insert(tool.clone()) {
            return Err(A2aError::InvalidConfig(format!(
                "A2A Agent ids produce the same tool name: {tool}"
            )));
        }
    }
    Ok(())
}

pub fn stable_tool_name(config_id: &str) -> String {
    let mut readable = String::with_capacity(32);
    let mut previous_underscore = false;
    for ch in config_id.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            Some('_')
        };
        if let Some(ch) = normalized {
            if ch == '_' && previous_underscore {
                continue;
            }
            readable.push(ch);
            previous_underscore = ch == '_';
        }
        if readable.len() >= 28 {
            break;
        }
    }
    let readable = readable.trim_matches('_');
    let digest = ring::digest::digest(&ring::digest::SHA256, config_id.as_bytes());
    let suffix = digest
        .as_ref()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if readable.is_empty() {
        format!("a2a_agent_{suffix}")
    } else {
        format!("a2a_agent_{readable}_{suffix}")
    }
}

pub(crate) fn validate_endpoint(endpoint: &str) -> Result<Url, A2aError> {
    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(A2aError::InvalidEndpoint(format!(
            "endpoint must contain 1 to {MAX_ENDPOINT_BYTES} bytes"
        )));
    }
    let url = Url::parse(endpoint)
        .map_err(|_| A2aError::InvalidEndpoint("endpoint is not a valid URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(A2aError::InvalidEndpoint(
            "only http and https endpoints are supported".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(A2aError::InvalidEndpoint("endpoint has no host".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(A2aError::InvalidEndpoint(
            "credentials in endpoint URLs are forbidden".into(),
        ));
    }
    if url.fragment().is_some() {
        return Err(A2aError::InvalidEndpoint(
            "endpoint fragments are forbidden".into(),
        ));
    }
    Ok(url)
}

pub(crate) fn validate_timeout(timeout: u64) -> Result<(), A2aError> {
    if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&timeout) {
        return Err(A2aError::InvalidConfig(format!(
            "timeout_seconds must be between {MIN_TIMEOUT_SECONDS} and {MAX_TIMEOUT_SECONDS}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(id: &str) -> A2aAgentConfig {
        A2aAgentConfig {
            id: id.into(),
            name: format!("Agent {id}"),
            endpoint: "http://127.0.0.1:9999".into(),
            enabled: true,
            bearer_token: None,
            timeout_seconds: 120,
        }
    }

    #[test]
    fn stable_names_are_ascii_bounded_and_collision_resistant() {
        let a = stable_tool_name("A-B");
        let b = stable_tool_name("A_B");
        assert_ne!(a, b);
        assert!(a.starts_with("a2a_agent_a_b_"));
        assert!(a.len() <= 64);
        assert!(a.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    }

    #[test]
    fn config_validation_rejects_unsafe_endpoints_and_duplicates() {
        let mut bad = config("a");
        bad.endpoint = "file:///tmp/card.json".into();
        assert!(validate_config(&bad).is_err());

        let duplicate = vec![config("a"), config("a")];
        assert!(validate_configs(&duplicate).is_err());
    }

    #[test]
    fn registry_policy_requires_exact_credential_free_https_binding() {
        assert!(RegistryInvocationPolicy::anonymous("http://agent.example.test/a2a").is_err());
        let policy = RegistryInvocationPolicy::anonymous("https://agent.example.test/a2a").unwrap();
        let mut bound = config("registry-agent");
        bound.endpoint = "https://agent.example.test/a2a".into();
        bound.bearer_token = None;
        assert!(policy.validate_config_binding(&bound).is_ok());

        bound.endpoint = "https://agent.example.test/other".into();
        assert!(policy.validate_config_binding(&bound).is_err());
        bound.endpoint = "https://agent.example.test/a2a".into();
        bound.bearer_token = Some("must-not-forward".into());
        assert!(policy.validate_config_binding(&bound).is_err());
    }

    #[test]
    fn production_registry_policy_rejects_ip_literals_and_local_names() {
        for endpoint in [
            "https://127.0.0.1/a2a",
            "https://[2606:4700:4700::1111]/a2a",
            "https://localhost/a2a",
            "https://agent.local/a2a",
            "https://service.internal/a2a",
            "https://printer.home.arpa/a2a",
        ] {
            assert!(
                RegistryInvocationPolicy::anonymous(endpoint).is_err(),
                "production policy unexpectedly accepted {endpoint}"
            );
        }
        assert!(RegistryInvocationPolicy::anonymous("https://agent.example.test/a2a").is_ok());
    }

    #[test]
    fn test_registry_policy_is_limited_to_literal_loopback_http() {
        assert!(RegistryInvocationPolicy::anonymous_loopback_for_testing(
            "http://127.0.0.1:9999/a2a"
        )
        .is_ok());
        assert!(
            RegistryInvocationPolicy::anonymous_loopback_for_testing("http://[::1]:9999/a2a")
                .is_ok()
        );
        for endpoint in [
            "http://localhost:9999/a2a",
            "http://0.0.0.0:9999/a2a",
            "http://192.0.2.1:9999/a2a",
            "https://127.0.0.1:9999/a2a",
        ] {
            assert!(
                RegistryInvocationPolicy::anonymous_loopback_for_testing(endpoint).is_err(),
                "test policy unexpectedly accepted {endpoint}"
            );
        }
    }

    #[test]
    fn action_validation_is_backward_compatible_and_get_task_requires_an_id() {
        let legacy: InvokeRequest = serde_json::from_value(serde_json::json!({
            "task": "continue the analysis"
        }))
        .unwrap();
        assert_eq!(legacy.action, InvokeAction::Send);
        assert!(legacy.validate().is_ok());

        let mut resume = InvokeRequest::get_task("task-1");
        assert!(resume.task.is_empty());
        assert!(resume.validate().is_ok());
        resume.task_id = Some("  ".into());
        assert!(resume.validate().is_err());

        let resume_without_task: InvokeRequest = serde_json::from_value(serde_json::json!({
            "action": "get_task",
            "task_id": "task-2"
        }))
        .unwrap();
        assert!(resume_without_task.task.is_empty());
        assert!(resume_without_task.validate().is_ok());

        let cancel_without_task: InvokeRequest = serde_json::from_value(serde_json::json!({
            "action": "cancel_task",
            "task_id": "task-3"
        }))
        .unwrap();
        assert!(cancel_without_task.task.is_empty());
        assert!(cancel_without_task.validate().is_ok());
    }
}
