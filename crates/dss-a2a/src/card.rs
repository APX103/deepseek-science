use std::collections::HashSet;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::truncate;
use crate::types::{validate_endpoint, ProtocolBinding, ProtocolVersion};
use crate::A2aError;

const MAX_SUMMARY_SKILLS: usize = 32;
const MAX_TENANT_BYTES: usize = 2_048;
const MAX_CARD_LIST_ITEMS: usize = 256;
const MAX_MODE_BYTES: usize = 128;

/// Response media types this client can faithfully expose to the harness and UI.
///
/// The order is the client's preference order and is retained when intersecting it with an
/// Agent Card's defaults.
const CLIENT_ACCEPTED_OUTPUT_MODES: [&str; 3] = ["text/plain", "text/markdown", "application/json"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardRefreshKind {
    Modified,
    NotModified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSkillSummary {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub agent_version: String,
    pub protocol_version: ProtocolVersion,
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<CardSkillSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedInterface {
    pub url: String,
    pub binding: ProtocolBinding,
    pub protocol_version: ProtocolVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedAgentCard {
    pub summary: CardSummary,
    pub selected_interface: SelectedInterface,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardSnapshot {
    pub card_url: String,
    pub fetched_at: String,
    pub sha256: String,
    pub refresh_kind: CardRefreshKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    pub summary: CardSummary,
    pub selected_interface: SelectedInterface,
    pub raw: Value,
}

/// Resolve the public Agent Card location.
///
/// A configured standard well-known card URL is used as-is. Any other configured HTTP(S) URL is
/// treated as an endpoint on the granted origin and resolves to the standard v0.3/v1 discovery
/// path. Query parameters are retained only for an explicitly supplied well-known URL.
pub fn resolve_agent_card_url(endpoint: &str) -> Result<Url, A2aError> {
    let mut url = validate_endpoint(endpoint)?;
    if url.path().trim_end_matches('/') == "/.well-known/agent-card.json" {
        return Ok(url);
    }
    url.set_path("/.well-known/agent-card.json");
    url.set_query(None);
    Ok(url)
}

pub fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Parse one complete, bounded card using an explicit v1 or v0.3 adapter.
pub fn parse_agent_card(
    card_url: &Url,
    raw: Value,
    bearer_configured: bool,
) -> Result<ParsedAgentCard, A2aError> {
    let object = raw
        .as_object()
        .ok_or_else(|| A2aError::InvalidCard("top level must be an object".into()))?;
    if object.contains_key("supportedInterfaces") {
        parse_v1_card(card_url, raw, bearer_configured)
    } else {
        parse_v03_card(card_url, raw, bearer_configured)
    }
}

fn parse_v1_card(
    card_url: &Url,
    raw: Value,
    bearer_configured: bool,
) -> Result<ParsedAgentCard, A2aError> {
    let name = required_string(&raw, "name", 256)?;
    let description = required_string(&raw, "description", 2_048)?;
    let agent_version = required_string(&raw, "version", 128)?;
    validate_v1_capabilities(&raw)?;
    required_string_array(
        &raw,
        "defaultInputModes",
        MAX_CARD_LIST_ITEMS,
        MAX_MODE_BYTES,
    )?;
    // Validate both the required field and that this client can actually request at least one
    // declared response media type. The same helper is used again when constructing the request.
    negotiate_output_modes(&raw)?;
    validate_v1_skills(&raw)?;
    validate_security(&raw, "securityRequirements", bearer_configured)?;

    let interfaces = raw
        .get("supportedInterfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| A2aError::InvalidCard("v1 supportedInterfaces must be an array".into()))?;
    if interfaces.is_empty() {
        return Err(A2aError::InvalidCard(
            "v1 supportedInterfaces must not be empty".into(),
        ));
    }
    for (index, interface) in interfaces.iter().enumerate() {
        validate_v1_interface_fields(interface, index)?;
    }
    let selected = interfaces
        .iter()
        .find_map(|interface| parse_v1_interface(card_url, interface).transpose())
        .transpose()?
        .ok_or_else(|| {
            A2aError::UnsupportedCard(
                "expected JSONRPC or HTTP+JSON with protocolVersion 1.0".into(),
            )
        })?;

    Ok(ParsedAgentCard {
        summary: CardSummary {
            name,
            description,
            agent_version,
            protocol_version: ProtocolVersion::V1,
            streaming: raw
                .pointer("/capabilities/streaming")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            skills: parse_skills(&raw),
        },
        selected_interface: selected,
        raw,
    })
}

fn parse_v1_interface(
    card_url: &Url,
    value: &Value,
) -> Result<Option<SelectedInterface>, A2aError> {
    let Some(object) = value.as_object() else {
        return Err(A2aError::InvalidCard(
            "v1 supportedInterfaces entries must be objects".into(),
        ));
    };
    if object.get("protocolVersion").and_then(Value::as_str) != Some("1.0") {
        return Ok(None);
    }
    let binding = match object.get("protocolBinding").and_then(Value::as_str) {
        Some("JSONRPC") => ProtocolBinding::JsonRpc,
        Some("HTTP+JSON") => ProtocolBinding::HttpJson,
        _ => return Ok(None),
    };
    let url = parse_interface_url(card_url, object.get("url"))?;
    let tenant = match object.get("tenant") {
        None | Some(Value::Null) => None,
        Some(Value::String(value))
            if value.len() <= MAX_TENANT_BYTES && !value.chars().any(char::is_control) =>
        {
            // Tenant is an opaque routing value and must be echoed exactly; never normalize or
            // truncate it. Values unsafe for transport/display make the Card invalid instead.
            Some(value.clone())
        }
        Some(Value::String(_)) => {
            return Err(A2aError::InvalidCard(format!(
                "interface tenant exceeds {MAX_TENANT_BYTES} bytes or contains control characters"
            )))
        }
        Some(_) => {
            return Err(A2aError::InvalidCard(
                "interface tenant must be a string".into(),
            ))
        }
    };
    Ok(Some(SelectedInterface {
        url: url.to_string(),
        binding,
        protocol_version: ProtocolVersion::V1,
        tenant,
    }))
}

fn validate_v1_interface_fields(value: &Value, index: usize) -> Result<(), A2aError> {
    let object = value.as_object().ok_or_else(|| {
        A2aError::InvalidCard(format!("v1 supportedInterfaces[{index}] must be an object"))
    })?;
    for key in ["url", "protocolBinding", "protocolVersion"] {
        if object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_none()
        {
            return Err(A2aError::InvalidCard(format!(
                "v1 supportedInterfaces[{index}].{key} must be a non-empty string"
            )));
        }
    }
    match object.get("tenant") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value))
            if value.len() <= MAX_TENANT_BYTES && !value.chars().any(char::is_control) =>
        {
            Ok(())
        }
        Some(Value::String(_)) => Err(A2aError::InvalidCard(format!(
            "v1 supportedInterfaces[{index}].tenant exceeds {MAX_TENANT_BYTES} bytes or contains control characters"
        ))),
        Some(_) => Err(A2aError::InvalidCard(format!(
            "v1 supportedInterfaces[{index}].tenant must be a string"
        ))),
    }
}

fn parse_v03_card(
    card_url: &Url,
    raw: Value,
    bearer_configured: bool,
) -> Result<ParsedAgentCard, A2aError> {
    let version = raw
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            A2aError::InvalidCard("card is neither v1 nor v0.3 (protocolVersion missing)".into())
        })?;
    if !version.starts_with("0.3") {
        return Err(A2aError::UnsupportedCard(format!(
            "unsupported protocolVersion {version}"
        )));
    }
    let name = required_string(&raw, "name", 256)?;
    let description = optional_string(&raw, "description", 2_048);
    let agent_version = required_string(&raw, "version", 128)?;
    validate_security(&raw, "security", bearer_configured)?;

    let mut candidates: Vec<(Value, Value)> = Vec::new();
    if let Some(url) = raw.get("url") {
        let transport = raw
            .get("preferredTransport")
            .cloned()
            .unwrap_or_else(|| Value::String("JSONRPC".into()));
        candidates.push((url.clone(), transport));
    }
    if let Some(additional) = raw.get("additionalInterfaces").and_then(Value::as_array) {
        for interface in additional {
            if let Some(object) = interface.as_object() {
                candidates.push((
                    object.get("url").cloned().unwrap_or(Value::Null),
                    object.get("transport").cloned().unwrap_or(Value::Null),
                ));
            } else {
                return Err(A2aError::InvalidCard(
                    "v0.3 additionalInterfaces entries must be objects".into(),
                ));
            }
        }
    }
    let selected = candidates
        .iter()
        .find_map(|(url, transport)| {
            let binding = match transport.as_str() {
                Some("JSONRPC") => ProtocolBinding::JsonRpc,
                Some("HTTP+JSON") => ProtocolBinding::HttpJson,
                _ => return None,
            };
            Some(
                parse_interface_url(card_url, Some(url)).map(|url| SelectedInterface {
                    url: url.to_string(),
                    binding,
                    protocol_version: ProtocolVersion::V03,
                    tenant: None,
                }),
            )
        })
        .transpose()?
        .ok_or_else(|| {
            A2aError::UnsupportedCard(
                "expected JSONRPC or HTTP+JSON in v0.3 card interfaces".into(),
            )
        })?;
    // defaultOutputModes is required by v0.3 as well. Intersecting here prevents a card from
    // appearing usable when every response mode it advertises is unsupported locally.
    negotiate_output_modes(&raw)?;

    Ok(ParsedAgentCard {
        summary: CardSummary {
            name,
            description,
            agent_version,
            protocol_version: ProtocolVersion::V03,
            streaming: raw
                .pointer("/capabilities/streaming")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            skills: parse_skills(&raw),
        },
        selected_interface: selected,
        raw,
    })
}

fn validate_v1_capabilities(raw: &Value) -> Result<(), A2aError> {
    let capabilities = raw
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| A2aError::InvalidCard("v1 capabilities must be an object".into()))?;
    for key in ["streaming", "pushNotifications", "extendedAgentCard"] {
        if capabilities
            .get(key)
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(A2aError::InvalidCard(format!(
                "v1 capabilities.{key} must be a boolean"
            )));
        }
    }
    let Some(extensions) = capabilities.get("extensions") else {
        return Ok(());
    };
    let extensions = extensions.as_array().ok_or_else(|| {
        A2aError::InvalidCard("v1 capabilities.extensions must be an array".into())
    })?;
    if extensions.len() > MAX_CARD_LIST_ITEMS {
        return Err(A2aError::InvalidCard(format!(
            "v1 capabilities.extensions exceeds {MAX_CARD_LIST_ITEMS} entries"
        )));
    }
    let mut required = Vec::new();
    for (index, extension) in extensions.iter().enumerate() {
        let object = extension.as_object().ok_or_else(|| {
            A2aError::InvalidCard(format!(
                "v1 capabilities.extensions[{index}] must be an object"
            ))
        })?;
        let uri = object
            .get("uri")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                A2aError::InvalidCard(format!(
                    "v1 capabilities.extensions[{index}].uri must be a non-empty string"
                ))
            })?;
        let is_required = match object.get("required") {
            None => false,
            Some(Value::Bool(required)) => *required,
            Some(_) => {
                return Err(A2aError::InvalidCard(format!(
                    "v1 capabilities.extensions[{index}].required must be a boolean"
                )))
            }
        };
        if object.get("params").is_some_and(|value| !value.is_object()) {
            return Err(A2aError::InvalidCard(format!(
                "v1 capabilities.extensions[{index}].params must be an object"
            )));
        }
        if is_required {
            required.push(sanitize_untrusted(uri, 256));
        }
    }
    if required.is_empty() {
        Ok(())
    } else {
        Err(A2aError::UnsupportedCard(format!(
            "required protocol extensions are not supported: {}",
            required.join(", ")
        )))
    }
}

fn validate_v1_skills(raw: &Value) -> Result<(), A2aError> {
    let skills = raw
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| A2aError::InvalidCard("v1 skills must be an array".into()))?;
    if skills.is_empty() {
        return Err(A2aError::InvalidCard("v1 skills must not be empty".into()));
    }
    if skills.len() > MAX_CARD_LIST_ITEMS {
        return Err(A2aError::InvalidCard(format!(
            "v1 skills exceeds {MAX_CARD_LIST_ITEMS} entries"
        )));
    }
    for (index, skill) in skills.iter().enumerate() {
        if !skill.is_object() {
            return Err(A2aError::InvalidCard(format!(
                "v1 skills[{index}] must be an object"
            )));
        }
        for (key, max_chars) in [("id", 128), ("name", 256), ("description", 2_048)] {
            required_string(skill, key, max_chars).map_err(|_| {
                A2aError::InvalidCard(format!(
                    "v1 skills[{index}].{key} must be a non-empty string"
                ))
            })?;
        }
        required_string_array(skill, "tags", MAX_CARD_LIST_ITEMS, 128)
            .map_err(|error| A2aError::InvalidCard(format!("v1 skills[{index}].tags: {error}")))?;
        for key in ["inputModes", "outputModes"] {
            if skill.get(key).is_some() {
                required_string_array(skill, key, MAX_CARD_LIST_ITEMS, MAX_MODE_BYTES).map_err(
                    |error| A2aError::InvalidCard(format!("v1 skills[{index}].{key}: {error}")),
                )?;
            }
        }
    }
    Ok(())
}

/// Compute the response modes advertised in a SendMessage request.
///
/// An Agent Card can declare modes this client does not render semantically. Advertising those
/// would be incorrect, so the request contains only the intersection, in local preference order.
pub(crate) fn negotiate_output_modes(raw: &Value) -> Result<Vec<String>, A2aError> {
    let remote = required_string_array(
        raw,
        "defaultOutputModes",
        MAX_CARD_LIST_ITEMS,
        MAX_MODE_BYTES,
    )?;
    let negotiated: Vec<String> = CLIENT_ACCEPTED_OUTPUT_MODES
        .iter()
        .filter(|local| {
            remote
                .iter()
                .any(|remote| remote.eq_ignore_ascii_case(local))
        })
        .map(|mode| (*mode).to_string())
        .collect();
    if negotiated.is_empty() {
        Err(A2aError::UnsupportedCard(
            "defaultOutputModes has no mode supported by this client".into(),
        ))
    } else {
        Ok(negotiated)
    }
}

fn required_string_array(
    raw: &Value,
    key: &str,
    max_items: usize,
    max_bytes: usize,
) -> Result<Vec<String>, A2aError> {
    let values = raw
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| A2aError::InvalidCard(format!("{key} must be an array")))?;
    if values.is_empty() {
        return Err(A2aError::InvalidCard(format!("{key} must not be empty")));
    }
    if values.len() > max_items {
        return Err(A2aError::InvalidCard(format!(
            "{key} exceeds {max_items} entries"
        )));
    }
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= max_bytes
                    && !value.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                A2aError::InvalidCard(format!(
                    "{key} entries must be non-empty strings of at most {max_bytes} bytes without control characters"
                ))
            })?;
        if !parsed
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(value))
        {
            parsed.push(value.to_string());
        }
    }
    Ok(parsed)
}

fn parse_interface_url(card_url: &Url, value: Option<&Value>) -> Result<Url, A2aError> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or_else(|| A2aError::InvalidCard("interface URL must be a string".into()))?;
    let url = validate_endpoint(raw)?;
    if !same_origin(card_url, &url) {
        return Err(A2aError::CrossOrigin);
    }
    Ok(url)
}

fn validate_security(
    raw: &Value,
    requirements_key: &str,
    bearer_configured: bool,
) -> Result<(), A2aError> {
    let Some(requirements) = raw.get(requirements_key) else {
        return Ok(());
    };
    let Some(requirements) = requirements.as_array() else {
        return Err(A2aError::InvalidCard(format!(
            "{requirements_key} must be an array"
        )));
    };
    if requirements.is_empty() || requirements.iter().any(requirement_is_empty) {
        return Ok(());
    }
    if !bearer_configured {
        return Err(A2aError::UnsupportedCard(
            "Agent requires authentication but no Bearer token is configured".into(),
        ));
    }
    let schemes = raw.get("securitySchemes").and_then(Value::as_object);
    let supported = requirements.iter().any(|requirement| {
        requirement_names(requirement).is_some_and(|names| {
            !names.is_empty()
                && names.iter().all(|name| {
                    schemes
                        .and_then(|map| map.get(*name))
                        .is_some_and(is_bearer_scheme)
                })
        })
    });
    if supported {
        Ok(())
    } else {
        Err(A2aError::UnsupportedCard(
            "Agent authentication requirements are not satisfiable with Bearer auth".into(),
        ))
    }
}

fn requirement_is_empty(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.is_empty()
            || object
                .get("schemes")
                .is_some_and(|v| v.as_object().is_some_and(|o| o.is_empty()))
    })
}

fn requirement_names(value: &Value) -> Option<Vec<&str>> {
    let object = value.as_object()?;
    let object = object
        .get("schemes")
        .and_then(Value::as_object)
        .unwrap_or(object);
    object.keys().map(String::as_str).collect::<Vec<_>>().into()
}

fn is_bearer_scheme(value: &Value) -> bool {
    value
        .get("scheme")
        .and_then(Value::as_str)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        || value
            .pointer("/httpAuthSecurityScheme/scheme")
            .and_then(Value::as_str)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
}

fn parse_skills(raw: &Value) -> Vec<CardSkillSummary> {
    raw.get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SUMMARY_SKILLS)
        .filter_map(|value| {
            let object = value.as_object()?;
            Some(CardSkillSummary {
                id: sanitize_untrusted(object.get("id")?.as_str()?, 128),
                name: sanitize_untrusted(object.get("name")?.as_str()?, 256),
                description: object
                    .get("description")
                    .and_then(Value::as_str)
                    .map(|value| sanitize_untrusted(value, 512))
                    .unwrap_or_default(),
                tags: string_array(object.get("tags"), 16, 64),
                input_modes: string_array(object.get("inputModes"), 16, 128),
                output_modes: string_array(object.get("outputModes"), 16, 128),
            })
        })
        .collect()
}

fn string_array(value: Option<&Value>, max_items: usize, max_chars: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| sanitize_untrusted(value, max_chars))
        .filter(|value| seen.insert(value.clone()))
        .take(max_items)
        .collect()
}

fn required_string(raw: &Value, key: &str, max_chars: usize) -> Result<String, A2aError> {
    let value = raw
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| A2aError::InvalidCard(format!("{key} must be a non-empty string")))?;
    Ok(sanitize_untrusted(value, max_chars))
}

fn optional_string(raw: &Value, key: &str, max_chars: usize) -> String {
    raw.get(key)
        .and_then(Value::as_str)
        .map(|value| sanitize_untrusted(value, max_chars))
        .unwrap_or_default()
}

fn sanitize_untrusted(value: &str, max_chars: usize) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    truncate(sanitized.trim(), max_chars)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid_v1_card() -> Value {
        json!({
            "name": "Physics",
            "description": "Specialist",
            "version": "2",
            "supportedInterfaces": [
                {"url":"https://example.test/rpc","protocolBinding":"JSONRPC","protocolVersion":"1.0"}
            ],
            "capabilities": {"streaming": false},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/markdown", "application/json"],
            "skills": [{
                "id": "analysis",
                "name": "Analysis",
                "description": "Analyze a physics problem",
                "tags": ["physics"]
            }]
        })
    }

    #[test]
    fn resolves_base_and_direct_card_urls() {
        assert_eq!(
            resolve_agent_card_url("http://localhost:8080/a2a")
                .unwrap()
                .as_str(),
            "http://localhost:8080/.well-known/agent-card.json"
        );
        assert_eq!(
            resolve_agent_card_url("https://example.test/.well-known/agent-card.json?tenant=alpha")
                .unwrap()
                .as_str(),
            "https://example.test/.well-known/agent-card.json?tenant=alpha"
        );
    }

    #[test]
    fn v1_selects_first_supported_same_origin_interface() {
        let card_url = Url::parse("https://example.test/.well-known/agent-card.json").unwrap();
        let mut card = valid_v1_card();
        card["supportedInterfaces"] = json!([
            {"url":"https://example.test/grpc","protocolBinding":"GRPC","protocolVersion":"1.0"},
            {"url":"https://example.test/rpc","protocolBinding":"JSONRPC","protocolVersion":"1.0"}
        ]);
        let parsed = parse_agent_card(&card_url, card, false).unwrap();
        assert_eq!(parsed.selected_interface.binding, ProtocolBinding::JsonRpc);
        assert_eq!(parsed.selected_interface.url, "https://example.test/rpc");
    }

    #[test]
    fn v03_card_has_separate_shape_and_cross_origin_fails() {
        let card_url = Url::parse("https://example.test/.well-known/agent-card.json").unwrap();
        let parsed = parse_agent_card(
            &card_url,
            json!({
                "name":"Legacy", "description":"", "version":"1", "protocolVersion":"0.3.0",
                "url":"https://example.test/rpc", "preferredTransport":"JSONRPC", "skills":[],
                "defaultOutputModes":["text/plain"]
            }),
            false,
        )
        .unwrap();
        assert_eq!(parsed.summary.protocol_version, ProtocolVersion::V03);

        let cross_origin = json!({
            "name":"Bad", "version":"1", "protocolVersion":"0.3.0",
            "url":"https://evil.test/rpc", "preferredTransport":"JSONRPC"
        });
        assert_eq!(
            parse_agent_card(&card_url, cross_origin, false).unwrap_err(),
            A2aError::CrossOrigin
        );
    }

    #[test]
    fn v1_rejects_missing_required_card_fields() {
        let card_url = Url::parse("https://example.test/.well-known/agent-card.json").unwrap();
        for key in [
            "description",
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "skills",
        ] {
            let mut card = valid_v1_card();
            card.as_object_mut().unwrap().remove(key);
            assert!(
                matches!(
                    parse_agent_card(&card_url, card, false),
                    Err(A2aError::InvalidCard(_))
                ),
                "missing {key} should invalidate a v1 card"
            );
        }
    }

    #[test]
    fn v1_rejects_invalid_required_skill_fields() {
        let card_url = Url::parse("https://example.test/.well-known/agent-card.json").unwrap();
        let mut card = valid_v1_card();
        card["skills"][0]
            .as_object_mut()
            .unwrap()
            .remove("description");
        assert!(matches!(
            parse_agent_card(&card_url, card, false),
            Err(A2aError::InvalidCard(_))
        ));
    }

    #[test]
    fn required_v1_extensions_are_rejected_but_optional_extensions_are_allowed() {
        let card_url = Url::parse("https://example.test/.well-known/agent-card.json").unwrap();
        let mut required = valid_v1_card();
        required["capabilities"]["extensions"] = json!([{
            "uri": "https://example.test/extensions/lab-context",
            "required": true
        }]);
        assert!(matches!(
            parse_agent_card(&card_url, required, false),
            Err(A2aError::UnsupportedCard(message))
                if message.contains("lab-context")
        ));

        let mut optional = valid_v1_card();
        optional["capabilities"]["extensions"] = json!([{
            "uri": "https://example.test/extensions/optional",
            "required": false
        }]);
        assert!(parse_agent_card(&card_url, optional, false).is_ok());
    }

    #[test]
    fn output_modes_are_the_intersection_in_local_preference_order() {
        let mut card = valid_v1_card();
        card["defaultOutputModes"] = json!([
            "application/octet-stream",
            "APPLICATION/JSON",
            "Text/Markdown"
        ]);
        assert_eq!(
            negotiate_output_modes(&card).unwrap(),
            vec!["text/markdown", "application/json"]
        );

        card["defaultOutputModes"] = json!(["image/tiff"]);
        assert!(matches!(
            negotiate_output_modes(&card),
            Err(A2aError::UnsupportedCard(_))
        ));
    }
}
