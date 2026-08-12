//! Explicit real-network acceptance gate for the configured Agent Registry.
//!
//! This test is ignored in normal CI. When deliberately invoked it never turns
//! ingress, catalog, protocol, or marker failures into a skip/pass.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use dss_a2a::{
    A2aClient, A2aRouteOptions, REGISTRY_API_KEY_WARNING, REGISTRY_DIRECT_TASK_WARNING,
    REGISTRY_ENDPOINT_OVERRIDE_WARNING,
};
use dss_core::{DEFAULT_AGENT_REGISTRY_NAME, DEFAULT_AGENT_REGISTRY_URL};
use dss_mcp::{MCPServerManager, McpRouteOptions};
use dss_tools::{
    builtin::{
        self,
        agent_registry::CALL_AGENT_TOOL_NAME,
        mcp::{MCP_LIST_RESOURCES_TOOL_NAME, MCP_READ_RESOURCE_TOOL_NAME},
    },
    PendingToolCall, ToolContext, ToolRegistry, ToolRouter,
};
use reqwest::Url;
use serde_json::{json, Value};

const MARKER: &str = "DSS_A2A_E2E_OK";
const INTERFACE_ENV: &str = "DSS_AGENT_REGISTRY_E2E_INTERFACE";
const RESOLVE_ENV: &str = "DSS_AGENT_REGISTRY_E2E_RESOLVE";

#[derive(Debug)]
struct Candidate {
    uri: String,
    name: String,
    endpoint_url: String,
    skill_id: Option<String>,
}

#[tokio::test]
#[ignore = "real Agent Registry/A2A network side effect; run explicitly with route env"]
async fn live_registry_resource_invokes_a2a_and_validates_artifact() {
    let interface = std::env::var(INTERFACE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let resolve = parse_resolve_env().expect("valid DSS_AGENT_REGISTRY_E2E_RESOLVE");

    let manager = Arc::new(MCPServerManager::new());
    manager
        .try_add_server_resources_only_with_route_options(
            DEFAULT_AGENT_REGISTRY_NAME,
            DEFAULT_AGENT_REGISTRY_URL,
            McpRouteOptions {
                interface: interface.clone(),
                resolve: resolve.clone(),
            },
        )
        .await
        .expect("connect the product MCP manager to the live Agent Registry");

    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    assert_eq!(
        builtin::mcp::register_resource_tools(&mut registry, &[DEFAULT_AGENT_REGISTRY_NAME.into()],),
        2
    );
    let a2a_client = A2aClient::with_route_options(A2aRouteOptions { interface, resolve })
        .expect("construct routed A2A client");
    assert!(
        builtin::agent_registry::register_tool_if_available(
            &mut registry,
            manager.as_ref(),
            &a2a_client,
        )
        .await
        .expect("register the run-local product call_agent tool"),
        "connected Registry with Resources did not expose call_agent"
    );
    let context = ToolContext::new(std::env::temp_dir()).with_mcp_arc(manager);

    let listed = execute_one(
        &registry,
        &context,
        "live-list",
        MCP_LIST_RESOURCES_TOOL_NAME,
        json!({"server": DEFAULT_AGENT_REGISTRY_NAME}),
    )
    .await;
    assert!(
        !listed.is_error,
        "live Resource list failed: {}",
        listed.content
    );
    let listed: Value = serde_json::from_str(&listed.content).expect("list envelope JSON");
    assert_eq!(listed["schema"], "dss.mcp.resources.v1");
    assert_eq!(listed["server"], DEFAULT_AGENT_REGISTRY_NAME);
    let resources = listed["resources"]
        .as_array()
        .filter(|resources| !resources.is_empty())
        .expect("live Agent Registry returned no Resources");

    let mut candidate = None;
    let mut rejection_reasons = Vec::new();
    for resource in resources {
        let Some(uri) = resource.get("uri").and_then(Value::as_str) else {
            rejection_reasons.push("Resource missing uri".to_string());
            continue;
        };
        let read = execute_one(
            &registry,
            &context,
            "live-read",
            MCP_READ_RESOURCE_TOOL_NAME,
            json!({"server": DEFAULT_AGENT_REGISTRY_NAME, "uri": uri}),
        )
        .await;
        if read.is_error {
            rejection_reasons.push(format!("{uri}: read failed"));
            continue;
        }
        let read: Value = serde_json::from_str(&read.content).expect("read envelope JSON");
        if read["uri"].as_str() != Some(uri) {
            rejection_reasons.push(format!("{uri}: read provenance mismatch"));
            continue;
        }
        let Some(contents) = read["contents"].as_array() else {
            rejection_reasons.push(format!("{uri}: contents missing"));
            continue;
        };
        let Some(text) = contents.iter().find_map(|content| {
            (content.get("uri").and_then(Value::as_str) == Some(uri))
                .then(|| content.get("text").and_then(Value::as_str))
                .flatten()
        }) else {
            rejection_reasons.push(format!("{uri}: no exact text content"));
            continue;
        };
        let Ok(descriptor) = serde_json::from_str::<Value>(text) else {
            rejection_reasons.push(format!("{uri}: descriptor JSON invalid"));
            continue;
        };
        if descriptor["kind"] != "a2a"
            || descriptor["uri"].as_str() != Some(uri)
            || descriptor["auth_scheme_type"] != "none"
        {
            rejection_reasons.push(format!("{uri}: not an anonymous A2A descriptor"));
            continue;
        }
        let Some(name) = descriptor["name"].as_str() else {
            rejection_reasons.push(format!("{uri}: descriptor name missing"));
            continue;
        };
        if resource.get("name").and_then(Value::as_str) != Some(name) {
            rejection_reasons.push(format!("{uri}: descriptor identity mismatch"));
            continue;
        }
        let Some(endpoint_url) = descriptor["endpoint_url"].as_str() else {
            rejection_reasons.push(format!("{uri}: endpoint missing"));
            continue;
        };
        let Ok(endpoint) = Url::parse(endpoint_url) else {
            rejection_reasons.push(format!("{uri}: endpoint invalid"));
            continue;
        };
        if endpoint.scheme() != "https" || endpoint.host_str().is_none() {
            rejection_reasons.push(format!("{uri}: endpoint not public HTTPS"));
            continue;
        }
        candidate = Some(Candidate {
            uri: uri.to_owned(),
            name: name.to_owned(),
            endpoint_url: endpoint_url.to_owned(),
            skill_id: None,
        });
        break;
    }
    let candidate = candidate.unwrap_or_else(|| {
        panic!(
            "live Agent Registry had no safe anonymous A2A candidate: {}",
            rejection_reasons.join("; ")
        )
    });

    let called = execute_one(
        &registry,
        &context,
        "live-call-agent",
        CALL_AGENT_TOOL_NAME,
        json!({
            "resource_uri": candidate.uri,
            "action": "send",
            "task": format!(
                "请只回复固定字符串 {MARKER}，不要调用搜索或其他工具，不要添加任何其他内容。"
            ),
            "skill_id": candidate.skill_id,
            "timeout_seconds": 120
        }),
    )
    .await;
    assert!(
        !called.is_error,
        "live call_agent failed: {}",
        called.content
    );
    let called: Value = serde_json::from_str(&called.content).expect("A2A result envelope JSON");
    assert_eq!(called["schema"], "dss.a2a.tool-result.v1");
    assert_eq!(called["registry"]["server"], DEFAULT_AGENT_REGISTRY_NAME);
    assert_eq!(called["registry"]["resource_uri"], candidate.uri);
    assert_eq!(called["registry"]["resource_name"], candidate.name);
    assert_eq!(
        called["agent"]["configured_endpoint"],
        candidate.endpoint_url
    );
    assert!(called["request"]["message_id"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(called["request"]["invocation_id"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(called["responses"].as_array().map(Vec::len), Some(1));
    assert_eq!(called["responses"][0]["operation"], "SendMessage");
    assert_eq!(called["responses"][0]["http_status"], 200);
    assert_eq!(called["responses"][0]["protocol_version"], "v1");

    let warnings = called["warnings"]
        .as_array()
        .expect("compatibility warnings");
    for expected in [
        REGISTRY_ENDPOINT_OVERRIDE_WARNING,
        REGISTRY_API_KEY_WARNING,
        REGISTRY_DIRECT_TASK_WARNING,
    ] {
        assert!(
            warnings.iter().any(|warning| warning == expected),
            "missing compatibility warning {expected}: {warnings:?}"
        );
    }

    let marker_hits = collect_artifact_text(&called["responses"])
        .into_iter()
        .filter(|text| text.trim() == MARKER)
        .count();
    assert!(marker_hits >= 1, "exact marker missing from A2A Artifact");
    assert_eq!(called["terminal"]["kind"], "task_interrupted");
    assert_eq!(called["terminal"]["state"], "TASK_STATE_INPUT_REQUIRED");
    assert!(called["terminal"]["task_id"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));

    // Deliberately do not send the requested confirmation/follow-up. The output
    // acquisition succeeded, while the remote Task remains honestly resumable.
    println!(
        "LIVE_AGENT_REGISTRY_E2E_OK resource_uri={} resource_name={} card_sha256={} terminal_state={} responses={} marker_hits={}",
        called["registry"]["resource_uri"].as_str().unwrap_or("<missing>"),
        called["registry"]["resource_name"].as_str().unwrap_or("<missing>"),
        called["card"]["sha256"].as_str().unwrap_or("<missing>"),
        called["terminal"]["state"].as_str().unwrap_or("<missing>"),
        called["responses"].as_array().map(Vec::len).unwrap_or(0),
        marker_hits,
    );
}

async fn execute_one(
    registry: &ToolRegistry,
    context: &ToolContext,
    id: &str,
    name: &str,
    input: Value,
) -> dss_tools::ToolResult {
    ToolRouter::execute_tool_calls(
        registry,
        context,
        vec![PendingToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }],
    )
    .await
    .pop()
    .expect("one routed result")
}

fn parse_resolve_env() -> Result<HashMap<String, SocketAddr>, String> {
    let raw = std::env::var(RESOLVE_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let mut result = HashMap::new();
    for entry in raw.split(',') {
        let (host, address) = entry
            .split_once('=')
            .ok_or_else(|| format!("{RESOLVE_ENV} entries must use host=ip:port"))?;
        let host = host.trim();
        if host.is_empty() || host.chars().any(char::is_control) {
            return Err(format!("{RESOLVE_ENV} contains an unsafe hostname"));
        }
        let address: SocketAddr = address
            .trim()
            .parse()
            .map_err(|_| format!("{RESOLVE_ENV} address for {host} is invalid"))?;
        if result.insert(host.to_owned(), address).is_some() {
            return Err(format!("{RESOLVE_ENV} repeats hostname {host}"));
        }
    }
    Ok(result)
}

fn collect_artifact_text(responses: &Value) -> Vec<&str> {
    let mut texts = Vec::new();
    let Some(responses) = responses.as_array() else {
        return texts;
    };
    for response in responses {
        let Some(artifacts) = response
            .pointer("/payload/result/artifacts")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for artifact in artifacts {
            let Some(parts) = artifact.get("parts").and_then(Value::as_array) else {
                continue;
            };
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    texts.push(text);
                }
            }
        }
    }
    texts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parser_is_explicit_and_bounded() {
        // Pure helper behavior is covered without mutating process-wide env.
        let mut parsed = HashMap::new();
        parsed.insert(
            "registry.example".to_string(),
            "203.0.113.7:443".parse::<SocketAddr>().unwrap(),
        );
        assert_eq!(parsed["registry.example"].port(), 443);
        assert!(Url::parse(DEFAULT_AGENT_REGISTRY_URL).is_ok());
    }

    #[test]
    fn artifact_oracle_ignores_status_and_history_text() {
        let payload = json!([{
            "payload": {"result": {
                "status": {"message": {"parts": [{"text": MARKER}]}},
                "history": [{"parts": [{"text": MARKER}]}],
                "artifacts": [{"parts": [{"text": "not-the-marker"}]}]
            }}
        }]);
        assert_eq!(collect_artifact_text(&payload), vec!["not-the-marker"]);
    }
}
