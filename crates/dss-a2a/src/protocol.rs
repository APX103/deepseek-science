use reqwest::Url;
use serde_json::{json, Map, Value};

use crate::{A2aError, InvokeRequest, ProtocolBinding, ProtocolVersion, SelectedInterface};

pub(crate) struct OutboundOperation {
    pub url: Url,
    pub method: reqwest::Method,
    pub body: Option<Value>,
    pub request_id: Option<String>,
    pub operation: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponseMeaning {
    Message {
        context_id: Option<String>,
    },
    Task {
        task_id: String,
        context_id: Option<String>,
        state: String,
        disposition: TaskDisposition,
    },
    RemoteError {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskDisposition {
    InProgress,
    Interrupted,
    Success,
    Failure,
}

pub(crate) fn build_send(
    interface: &SelectedInterface,
    request: &InvokeRequest,
    request_id: &str,
    message_id: &str,
    accepted_output_modes: &[String],
) -> Result<OutboundOperation, A2aError> {
    if accepted_output_modes.is_empty() {
        return Err(A2aError::UnsupportedCard(
            "no mutually supported output mode is available".into(),
        ));
    }
    let params = match (interface.protocol_version, interface.binding) {
        (ProtocolVersion::V1, ProtocolBinding::JsonRpc) => {
            build_v1_send_params(interface, request, message_id, accepted_output_modes, true)
        }
        (ProtocolVersion::V1, ProtocolBinding::HttpJson) => {
            build_v1_send_params(interface, request, message_id, accepted_output_modes, false)
        }
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => {
            build_v03_jsonrpc_send_params(request, message_id, accepted_output_modes)
        }
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => {
            build_v03_rest_send_params(request, message_id, accepted_output_modes)
        }
    };
    match interface.binding {
        ProtocolBinding::JsonRpc => {
            let method = match interface.protocol_version {
                ProtocolVersion::V1 => "SendMessage",
                ProtocolVersion::V03 => "message/send",
            };
            Ok(OutboundOperation {
                url: parse_interface_url(interface)?,
                method: reqwest::Method::POST,
                body: Some(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": params
                })),
                request_id: Some(request_id.to_string()),
                operation: method,
            })
        }
        ProtocolBinding::HttpJson => {
            let url = match interface.protocol_version {
                ProtocolVersion::V1 => append_v1_rest_segments(
                    parse_interface_url(interface)?,
                    interface.tenant.as_deref(),
                    &["message:send"],
                )?,
                ProtocolVersion::V03 => {
                    append_segments(parse_interface_url(interface)?, &["v1", "message:send"])?
                }
            };
            Ok(OutboundOperation {
                url,
                method: reqwest::Method::POST,
                body: Some(params),
                request_id: None,
                operation: "message:send",
            })
        }
    }
}

pub(crate) fn build_get_task(
    interface: &SelectedInterface,
    task_id: &str,
    request_id: &str,
) -> Result<OutboundOperation, A2aError> {
    match interface.binding {
        ProtocolBinding::JsonRpc => {
            let method = match interface.protocol_version {
                ProtocolVersion::V1 => "GetTask",
                ProtocolVersion::V03 => "tasks/get",
            };
            let mut params = Map::from_iter([
                ("id".to_string(), Value::String(task_id.to_string())),
                ("historyLength".to_string(), Value::Number(50.into())),
            ]);
            if interface.protocol_version == ProtocolVersion::V1 {
                if let Some(tenant) = &interface.tenant {
                    params.insert("tenant".into(), Value::String(tenant.clone()));
                }
            }
            Ok(OutboundOperation {
                url: parse_interface_url(interface)?,
                method: reqwest::Method::POST,
                body: Some(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": params
                })),
                request_id: Some(request_id.to_string()),
                operation: method,
            })
        }
        ProtocolBinding::HttpJson => {
            let mut url = match interface.protocol_version {
                ProtocolVersion::V1 => append_v1_rest_segments(
                    parse_interface_url(interface)?,
                    interface.tenant.as_deref(),
                    &["tasks", task_id],
                )?,
                ProtocolVersion::V03 => {
                    append_segments(parse_interface_url(interface)?, &["v1", "tasks", task_id])?
                }
            };
            url.query_pairs_mut().append_pair("historyLength", "50");
            Ok(OutboundOperation {
                url,
                method: reqwest::Method::GET,
                body: None,
                request_id: None,
                operation: "tasks/get",
            })
        }
    }
}

pub(crate) fn build_cancel_task(
    interface: &SelectedInterface,
    task_id: &str,
    request_id: &str,
) -> Result<OutboundOperation, A2aError> {
    let mut params = Map::from_iter([("id".to_string(), Value::String(task_id.to_string()))]);
    if interface.protocol_version == ProtocolVersion::V1 {
        if let Some(tenant) = &interface.tenant {
            params.insert("tenant".into(), Value::String(tenant.clone()));
        }
    }
    match interface.binding {
        ProtocolBinding::JsonRpc => {
            let method = match interface.protocol_version {
                ProtocolVersion::V1 => "CancelTask",
                ProtocolVersion::V03 => "tasks/cancel",
            };
            Ok(OutboundOperation {
                url: parse_interface_url(interface)?,
                method: reqwest::Method::POST,
                body: Some(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": params
                })),
                request_id: Some(request_id.to_string()),
                operation: method,
            })
        }
        ProtocolBinding::HttpJson => {
            let task_cancel = format!("{task_id}:cancel");
            let url = match interface.protocol_version {
                ProtocolVersion::V1 => append_v1_rest_segments(
                    parse_interface_url(interface)?,
                    interface.tenant.as_deref(),
                    &["tasks", task_cancel.as_str()],
                )?,
                ProtocolVersion::V03 => append_segments(
                    parse_interface_url(interface)?,
                    &["v1", "tasks", task_cancel.as_str()],
                )?,
            };
            Ok(OutboundOperation {
                url,
                method: reqwest::Method::POST,
                // Both the v1 RestDispatcher and the v0.3 REST03Adapter take the task id from
                // the path. Sending an id/tenant JSON body is not part of either REST contract.
                body: None,
                request_id: None,
                operation: "tasks/cancel",
            })
        }
    }
}

pub(crate) fn parse_send_response(
    interface: &SelectedInterface,
    payload: &Value,
    expected_request_id: Option<&str>,
) -> Result<ResponseMeaning, A2aError> {
    if let Some(error) = validate_transport(
        interface.binding,
        payload,
        expected_request_id,
        "SendMessage",
    )? {
        return Ok(error);
    }
    let value = success_value(interface.binding, payload);
    match (interface.protocol_version, interface.binding) {
        (ProtocolVersion::V1, _) => parse_v1_send_union(value),
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => parse_v03_direct(value),
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => parse_v03_rest_union(value),
    }
}

pub(crate) fn parse_get_task_response(
    interface: &SelectedInterface,
    payload: &Value,
    expected_request_id: Option<&str>,
) -> Result<ResponseMeaning, A2aError> {
    if let Some(error) =
        validate_transport(interface.binding, payload, expected_request_id, "GetTask")?
    {
        return Ok(error);
    }
    let value = success_value(interface.binding, payload);
    parse_task(value, interface.protocol_version, interface.binding)
}

pub(crate) fn parse_cancel_task_response(
    interface: &SelectedInterface,
    payload: &Value,
    expected_request_id: Option<&str>,
) -> Result<ResponseMeaning, A2aError> {
    if let Some(error) = validate_transport(
        interface.binding,
        payload,
        expected_request_id,
        "CancelTask",
    )? {
        return Ok(error);
    }
    let value = success_value(interface.binding, payload);
    parse_task(value, interface.protocol_version, interface.binding)
}

fn validate_transport(
    binding: ProtocolBinding,
    payload: &Value,
    expected_request_id: Option<&str>,
    operation: &str,
) -> Result<Option<ResponseMeaning>, A2aError> {
    if binding == ProtocolBinding::HttpJson {
        return Ok(None);
    }
    let object = payload
        .as_object()
        .ok_or_else(|| A2aError::Protocol(format!("{operation} response must be an object")))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(A2aError::Protocol(format!(
            "{operation} response has invalid jsonrpc version"
        )));
    }
    let expected = expected_request_id.ok_or_else(|| {
        A2aError::Protocol(format!("{operation} has no local JSON-RPC request id"))
    })?;
    if object.get("id").and_then(Value::as_str) != Some(expected) {
        return Err(A2aError::Protocol(format!(
            "{operation} response id does not match the request"
        )));
    }
    if let Some(error) = object.get("error") {
        let code = error.get("code").and_then(Value::as_i64);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("remote JSON-RPC error");
        return Ok(Some(ResponseMeaning::RemoteError {
            message: match code {
                Some(code) => format!("JSON-RPC error {code}: {message}"),
                None => format!("JSON-RPC error: {message}"),
            },
        }));
    }
    if !object.contains_key("result") {
        return Err(A2aError::Protocol(format!(
            "{operation} response has neither result nor error"
        )));
    }
    Ok(None)
}

fn success_value(binding: ProtocolBinding, payload: &Value) -> &Value {
    match binding {
        ProtocolBinding::JsonRpc => &payload["result"],
        ProtocolBinding::HttpJson => payload,
    }
}

fn parse_v1_send_union(value: &Value) -> Result<ResponseMeaning, A2aError> {
    let object = value
        .as_object()
        .ok_or_else(|| A2aError::Protocol("v1 SendMessage result must be an object".into()))?;
    match (object.get("message"), object.get("task")) {
        (Some(message), None) => {
            parse_message(message, ProtocolVersion::V1, ProtocolBinding::JsonRpc)
        }
        (None, Some(task)) => parse_task(task, ProtocolVersion::V1, ProtocolBinding::JsonRpc),
        _ => Err(A2aError::Protocol(
            "v1 SendMessage result must contain exactly one of message or task".into(),
        )),
    }
}

fn parse_v03_rest_union(value: &Value) -> Result<ResponseMeaning, A2aError> {
    let object = value
        .as_object()
        .ok_or_else(|| A2aError::Protocol("v0.3 REST send response must be an object".into()))?;
    match (object.get("message"), object.get("task")) {
        (Some(message), None) => {
            parse_message(message, ProtocolVersion::V03, ProtocolBinding::HttpJson)
        }
        (None, Some(task)) => parse_task(task, ProtocolVersion::V03, ProtocolBinding::HttpJson),
        _ => Err(A2aError::Protocol(
            "v0.3 REST send response must contain exactly one of message or task".into(),
        )),
    }
}

fn parse_v03_direct(value: &Value) -> Result<ResponseMeaning, A2aError> {
    match value.get("kind").and_then(Value::as_str) {
        Some("message") => parse_message(value, ProtocolVersion::V03, ProtocolBinding::JsonRpc),
        Some("task") => parse_task(value, ProtocolVersion::V03, ProtocolBinding::JsonRpc),
        _ => Err(A2aError::Protocol(
            "v0.3 JSON-RPC send result must be a message or task".into(),
        )),
    }
}

fn parse_message(
    value: &Value,
    version: ProtocolVersion,
    binding: ProtocolBinding,
) -> Result<ResponseMeaning, A2aError> {
    let object = value
        .as_object()
        .ok_or_else(|| A2aError::Protocol("A2A Message must be an object".into()))?;
    if object
        .get("messageId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .is_none()
    {
        return Err(A2aError::Protocol("A2A Message has no messageId".into()));
    }
    let (expected_role, content_field) = match (version, binding) {
        (ProtocolVersion::V1, _) => ("ROLE_AGENT", "parts"),
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => ("agent", "parts"),
        // REST03Adapter uses the v0.3 protobuf and ProtoJSON, not the v0.3 Pydantic JSON-RPC
        // representation. Its enum names and Message.content field therefore match the proto.
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => ("ROLE_AGENT", "content"),
    };
    if object.get("role").and_then(Value::as_str) != Some(expected_role) {
        return Err(A2aError::Protocol(format!(
            "A2A Message role must be {expected_role}"
        )));
    }
    if object
        .get(content_field)
        .and_then(Value::as_array)
        .is_none_or(|parts| parts.is_empty())
    {
        return Err(A2aError::Protocol(format!(
            "A2A Message {content_field} must be a non-empty array"
        )));
    }
    Ok(ResponseMeaning::Message {
        context_id: string_field(value, "contextId"),
    })
}

fn parse_task(
    value: &Value,
    version: ProtocolVersion,
    binding: ProtocolBinding,
) -> Result<ResponseMeaning, A2aError> {
    let task_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| A2aError::Protocol("A2A Task has no id".into()))?
        .to_string();
    let state = value
        .pointer("/status/state")
        .and_then(Value::as_str)
        .ok_or_else(|| A2aError::Protocol("A2A Task has no status.state".into()))?
        .to_string();
    let disposition = task_disposition(version, binding, &state)?;
    Ok(ResponseMeaning::Task {
        task_id,
        context_id: string_field(value, "contextId"),
        state,
        disposition,
    })
}

fn task_disposition(
    version: ProtocolVersion,
    binding: ProtocolBinding,
    state: &str,
) -> Result<TaskDisposition, A2aError> {
    let disposition = match (version, binding) {
        (ProtocolVersion::V1, _) => match state {
            "TASK_STATE_UNSPECIFIED" | "TASK_STATE_SUBMITTED" | "TASK_STATE_WORKING" => {
                TaskDisposition::InProgress
            }
            "TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED" => {
                TaskDisposition::Interrupted
            }
            "TASK_STATE_COMPLETED" => TaskDisposition::Success,
            "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED" => {
                TaskDisposition::Failure
            }
            _ => {
                return Err(A2aError::Protocol(format!(
                    "unsupported v1 Task state {state}"
                )))
            }
        },
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => match state {
            "unknown" | "submitted" | "working" => TaskDisposition::InProgress,
            "input-required" | "auth-required" => TaskDisposition::Interrupted,
            "completed" => TaskDisposition::Success,
            "failed" | "canceled" | "rejected" => TaskDisposition::Failure,
            _ => {
                return Err(A2aError::Protocol(format!(
                    "unsupported v0.3 Task state {state}"
                )))
            }
        },
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => match state {
            "TASK_STATE_UNSPECIFIED" | "TASK_STATE_SUBMITTED" | "TASK_STATE_WORKING" => {
                TaskDisposition::InProgress
            }
            "TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED" => {
                TaskDisposition::Interrupted
            }
            "TASK_STATE_COMPLETED" => TaskDisposition::Success,
            "TASK_STATE_FAILED" | "TASK_STATE_CANCELLED" | "TASK_STATE_REJECTED" => {
                TaskDisposition::Failure
            }
            _ => {
                return Err(A2aError::Protocol(format!(
                    "unsupported v0.3 REST Task state {state}"
                )))
            }
        },
    };
    Ok(disposition)
}

fn build_v1_send_params(
    interface: &SelectedInterface,
    request: &InvokeRequest,
    message_id: &str,
    accepted_output_modes: &[String],
    include_tenant: bool,
) -> Value {
    let mut message = Map::from_iter([
        ("messageId".into(), Value::String(message_id.into())),
        ("role".into(), Value::String("ROLE_USER".into())),
        ("parts".into(), json!([{"text": request.task}])),
    ]);
    add_continuation(&mut message, request);
    if let Some(skill_id) = &request.skill_id {
        message.insert("metadata".into(), json!({"skillId": skill_id}));
    }
    let mut params = Map::from_iter([
        ("message".into(), Value::Object(message)),
        (
            "configuration".into(),
            json!({
                "acceptedOutputModes": accepted_output_modes,
                "historyLength": 50,
                "returnImmediately": true
            }),
        ),
    ]);
    if include_tenant {
        if let Some(tenant) = &interface.tenant {
            params.insert("tenant".into(), Value::String(tenant.clone()));
        }
    }
    Value::Object(params)
}

fn build_v03_jsonrpc_send_params(
    request: &InvokeRequest,
    message_id: &str,
    accepted_output_modes: &[String],
) -> Value {
    let mut message = Map::from_iter([
        ("messageId".into(), Value::String(message_id.into())),
        ("role".into(), Value::String("user".into())),
        (
            "parts".into(),
            json!([{"kind":"text", "text": request.task}]),
        ),
    ]);
    add_continuation(&mut message, request);
    if let Some(skill_id) = &request.skill_id {
        message.insert("metadata".into(), json!({"skillId": skill_id}));
    }
    json!({
        "message": message,
        "configuration": {
            "acceptedOutputModes": accepted_output_modes,
            "historyLength": 50,
            "blocking": false
        }
    })
}

fn build_v03_rest_send_params(
    request: &InvokeRequest,
    message_id: &str,
    accepted_output_modes: &[String],
) -> Value {
    let mut message = Map::from_iter([
        ("messageId".into(), Value::String(message_id.into())),
        ("role".into(), Value::String("ROLE_USER".into())),
        ("content".into(), json!([{"text": request.task}])),
    ]);
    add_continuation(&mut message, request);
    if let Some(skill_id) = &request.skill_id {
        message.insert("metadata".into(), json!({"skillId": skill_id}));
    }
    json!({
        "message": message,
        "configuration": {
            "acceptedOutputModes": accepted_output_modes,
            "historyLength": 50,
            "blocking": false
        }
    })
}

fn add_continuation(message: &mut Map<String, Value>, request: &InvokeRequest) {
    if let Some(task_id) = &request.task_id {
        message.insert("taskId".into(), Value::String(task_id.clone()));
    }
    if let Some(context_id) = &request.context_id {
        message.insert("contextId".into(), Value::String(context_id.clone()));
    }
}

fn parse_interface_url(interface: &SelectedInterface) -> Result<Url, A2aError> {
    Url::parse(&interface.url)
        .map_err(|_| A2aError::Protocol("selected interface URL became invalid".into()))
}

fn append_segments(mut url: Url, segments: &[&str]) -> Result<Url, A2aError> {
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| A2aError::Protocol("interface URL cannot be a base URL".into()))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    url.set_query(None);
    Ok(url)
}

fn append_v1_rest_segments(
    mut url: Url,
    tenant: Option<&str>,
    segments: &[&str],
) -> Result<Url, A2aError> {
    if let Some(tenant) = tenant {
        url = append_segments(url, &[tenant])?;
    }
    append_segments(url, segments)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn interface(version: ProtocolVersion, binding: ProtocolBinding) -> SelectedInterface {
        SelectedInterface {
            url: "http://127.0.0.1:9999/a2a".into(),
            binding,
            protocol_version: version,
            tenant: Some("tenant-a".into()),
        }
    }

    #[test]
    fn request_adapters_do_not_mix_v1_and_v03_shapes() {
        let request = InvokeRequest::new("hello");
        let accepted_output_modes = vec!["text/markdown".to_string()];
        let v1 = build_send(
            &interface(ProtocolVersion::V1, ProtocolBinding::JsonRpc),
            &request,
            "request-1",
            "message-1",
            &accepted_output_modes,
        )
        .unwrap()
        .body
        .unwrap();
        assert_eq!(v1["method"], "SendMessage");
        assert_eq!(v1["params"]["message"]["role"], "ROLE_USER");
        assert_eq!(v1["params"]["tenant"], "tenant-a");
        assert_eq!(
            v1["params"]["configuration"]["acceptedOutputModes"],
            json!(["text/markdown"])
        );

        let v1_rest_send = build_send(
            &interface(ProtocolVersion::V1, ProtocolBinding::HttpJson),
            &request,
            "request-rest",
            "message-rest",
            &accepted_output_modes,
        )
        .unwrap();
        assert_eq!(v1_rest_send.url.path(), "/a2a/tenant-a/message:send");
        assert!(v1_rest_send.body.as_ref().unwrap().get("tenant").is_none());

        let v1_rest_get = build_get_task(
            &interface(ProtocolVersion::V1, ProtocolBinding::HttpJson),
            "task-1",
            "request-rest",
        )
        .unwrap();
        assert_eq!(v1_rest_get.url.path(), "/a2a/tenant-a/tasks/task-1");
        assert_eq!(
            v1_rest_get.url.query_pairs().collect::<Vec<_>>(),
            vec![("historyLength".into(), "50".into())]
        );

        let v1_cancel = build_cancel_task(
            &interface(ProtocolVersion::V1, ProtocolBinding::JsonRpc),
            "task-1",
            "cancel-1",
        )
        .unwrap()
        .body
        .unwrap();
        assert_eq!(v1_cancel["method"], "CancelTask");
        assert_eq!(v1_cancel["params"]["id"], "task-1");
        assert_eq!(v1_cancel["params"]["tenant"], "tenant-a");

        let v1_rest_cancel = build_cancel_task(
            &interface(ProtocolVersion::V1, ProtocolBinding::HttpJson),
            "task-1",
            "cancel-rest",
        )
        .unwrap();
        assert_eq!(v1_rest_cancel.method, reqwest::Method::POST);
        assert_eq!(
            v1_rest_cancel.url.path(),
            "/a2a/tenant-a/tasks/task-1:cancel"
        );
        assert!(v1_rest_cancel.body.is_none());

        let v03 = build_send(
            &interface(ProtocolVersion::V03, ProtocolBinding::JsonRpc),
            &request,
            "request-2",
            "message-2",
            &accepted_output_modes,
        )
        .unwrap()
        .body
        .unwrap();
        assert_eq!(v03["method"], "message/send");
        assert_eq!(v03["params"]["message"]["role"], "user");
        assert!(v03["params"]["message"].get("kind").is_none());
        assert!(v03["params"].get("tenant").is_none());
        assert_eq!(
            v03["params"]["configuration"]["acceptedOutputModes"],
            json!(["text/markdown"])
        );

        let v03_rest = build_send(
            &interface(ProtocolVersion::V03, ProtocolBinding::HttpJson),
            &request,
            "request-03-rest",
            "message-03-rest",
            &accepted_output_modes,
        )
        .unwrap();
        assert_eq!(v03_rest.url.path(), "/a2a/v1/message:send");
        let v03_rest_body = v03_rest.body.unwrap();
        assert_eq!(v03_rest_body["message"]["role"], "ROLE_USER");
        assert_eq!(
            v03_rest_body["message"]["content"],
            json!([{"text":"hello"}])
        );
        assert!(v03_rest_body["message"].get("parts").is_none());
        assert!(v03_rest_body.get("request").is_none());

        let v03_cancel = build_cancel_task(
            &interface(ProtocolVersion::V03, ProtocolBinding::JsonRpc),
            "task-1",
            "cancel-03",
        )
        .unwrap()
        .body
        .unwrap();
        assert_eq!(v03_cancel["method"], "tasks/cancel");
        assert!(v03_cancel["params"].get("tenant").is_none());

        let v03_rest_cancel = build_cancel_task(
            &interface(ProtocolVersion::V03, ProtocolBinding::HttpJson),
            "task-1",
            "cancel-03-rest",
        )
        .unwrap();
        assert_eq!(v03_rest_cancel.url.path(), "/a2a/v1/tasks/task-1:cancel");
        assert!(v03_rest_cancel.body.is_none());
    }

    #[test]
    fn request_builder_rejects_an_empty_output_mode_intersection() {
        let result = build_send(
            &interface(ProtocolVersion::V1, ProtocolBinding::JsonRpc),
            &InvokeRequest::new("hello"),
            "request-1",
            "message-1",
            &[],
        );
        assert!(matches!(result, Err(A2aError::UnsupportedCard(_))));
    }

    #[test]
    fn response_adapters_require_their_own_union_shape() {
        let v1_interface = interface(ProtocolVersion::V1, ProtocolBinding::JsonRpc);
        let response = json!({
            "jsonrpc":"2.0", "id":"r1",
            "result":{"message":{"messageId":"m", "role":"ROLE_AGENT", "parts":[{"text":"ok"}]}}
        });
        assert!(matches!(
            parse_send_response(&v1_interface, &response, Some("r1")).unwrap(),
            ResponseMeaning::Message { .. }
        ));

        let direct_v03 = json!({
            "kind":"message", "messageId":"m", "role":"agent",
            "parts":[{"kind":"text", "text":"ok"}]
        });
        assert!(parse_v1_send_union(&direct_v03).is_err());
        assert!(parse_v03_direct(&direct_v03).is_ok());

        let v03_rest_interface = interface(ProtocolVersion::V03, ProtocolBinding::HttpJson);
        let rest_message = json!({
            "message": {
                "messageId":"m-rest", "role":"ROLE_AGENT",
                "content":[{"text":"ok"}]
            }
        });
        assert!(matches!(
            parse_send_response(&v03_rest_interface, &rest_message, None).unwrap(),
            ResponseMeaning::Message { .. }
        ));
        assert!(
            parse_send_response(&v03_rest_interface, &json!({"message": direct_v03}), None)
                .is_err()
        );

        let rest_task = json!({
            "id":"task-rest", "contextId":"context-rest",
            "status":{"state":"TASK_STATE_COMPLETED"}
        });
        assert!(matches!(
            parse_get_task_response(&v03_rest_interface, &rest_task, None).unwrap(),
            ResponseMeaning::Task {
                disposition: TaskDisposition::Success,
                ..
            }
        ));
        let canceled_rest_task = json!({
            "id":"task-rest", "contextId":"context-rest",
            "status":{"state":"TASK_STATE_CANCELLED"}
        });
        assert!(matches!(
            parse_cancel_task_response(&v03_rest_interface, &canceled_rest_task, None).unwrap(),
            ResponseMeaning::Task {
                disposition: TaskDisposition::Failure,
                ..
            }
        ));
    }

    #[test]
    fn unspecified_task_states_remain_resumable() {
        assert_eq!(
            task_disposition(
                ProtocolVersion::V1,
                ProtocolBinding::JsonRpc,
                "TASK_STATE_UNSPECIFIED"
            )
            .unwrap(),
            TaskDisposition::InProgress
        );
        assert_eq!(
            task_disposition(ProtocolVersion::V03, ProtocolBinding::JsonRpc, "unknown").unwrap(),
            TaskDisposition::InProgress
        );
        assert_eq!(
            task_disposition(
                ProtocolVersion::V03,
                ProtocolBinding::HttpJson,
                "TASK_STATE_UNSPECIFIED"
            )
            .unwrap(),
            TaskDisposition::InProgress
        );
    }
}
