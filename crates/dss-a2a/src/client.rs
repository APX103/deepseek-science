use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use dss_core::A2aAgentConfig;
use futures::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE,
    IF_NONE_MATCH, LAST_MODIFIED,
};
use serde_json::{json, Value};
use tokio::time::Instant;
use uuid::Uuid;

use crate::card::{
    negotiate_output_modes, parse_agent_card, resolve_agent_card_url, CardRefreshKind,
};
use crate::protocol::{
    build_cancel_task, build_get_task, build_send, parse_cancel_task_response,
    parse_get_task_response, parse_send_response, OutboundOperation, ResponseMeaning,
    TaskDisposition,
};
use crate::result::{
    A2aAgentRef, A2aRequestRecord, A2aTerminal, A2aToolResult, ResponseFrame, TerminalKind,
    A2A_RESULT_SCHEMA,
};
use crate::types::{
    validate_config, InvokeAction, InvokeRequest, ProtocolBinding, ProtocolVersion, MAX_CARD_BYTES,
    MAX_RESPONSE_BYTES, MAX_TOTAL_RESPONSE_BYTES,
};
use crate::{A2aError, CardSnapshot, SelectedInterface};

#[derive(Debug, Clone)]
pub struct A2aClientOptions {
    pub connect_timeout: Duration,
    pub card_timeout: Duration,
    pub poll_initial: Duration,
    pub poll_max: Duration,
    pub max_polls: u32,
}

impl Default for A2aClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            card_timeout: Duration::from_secs(15),
            poll_initial: Duration::from_millis(250),
            poll_max: Duration::from_secs(2),
            max_polls: 128,
        }
    }
}

#[derive(Clone)]
pub struct A2aClient {
    http: reqwest::Client,
    options: A2aClientOptions,
}

impl std::fmt::Debug for A2aClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2aClient")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl A2aClient {
    pub fn new() -> Result<Self, A2aError> {
        Self::with_options(A2aClientOptions::default())
    }

    pub fn with_options(options: A2aClientOptions) -> Result<Self, A2aError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(options.connect_timeout)
            .build()
            .map_err(A2aError::transport)?;
        Ok(Self { http, options })
    }

    /// Perform a real conditional HTTP revalidation of the configured Agent Card.
    ///
    /// A `304` is accepted only when the caller supplies the corresponding last-known-good card.
    /// Any failure is fail-closed; callers must not silently route with stale metadata.
    pub async fn refresh_card(
        &self,
        config: &A2aAgentConfig,
        cached: Option<&CardSnapshot>,
    ) -> Result<CardSnapshot, A2aError> {
        validate_config(config)?;
        let card_url = resolve_agent_card_url(&config.endpoint)?;
        let mut request = self
            .http
            .get(card_url.clone())
            .header(CACHE_CONTROL, "no-cache")
            .header(ACCEPT, "application/json")
            .timeout(self.options.card_timeout);
        if let Some(token) = config.bearer_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(cached) = cached.filter(|cached| cached.card_url == card_url.as_str()) {
            if let Some(etag) = cached.etag.as_deref().and_then(header_value) {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = cached.last_modified.as_deref().and_then(header_value) {
                request = request.header(IF_MODIFIED_SINCE, last_modified);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| A2aError::card_refresh(redact_text(&error.to_string(), config)))?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let cached = cached
                .filter(|cached| cached.card_url == card_url.as_str())
                .ok_or_else(|| {
                    A2aError::card_refresh("received 304 without a matching validated cache")
                })?;
            let headers = response.headers();
            let mut refreshed = cached.clone();
            // A persisted cache may have been validated by an older application version. Re-run
            // current validation on 304 so newly enforced card requirements cannot be bypassed.
            let parsed = parse_agent_card(
                &card_url,
                cached.raw.clone(),
                config
                    .bearer_token
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
            )?;
            refreshed.fetched_at = Utc::now().to_rfc3339();
            refreshed.refresh_kind = CardRefreshKind::NotModified;
            refreshed.etag = header_string(headers, ETAG).or_else(|| cached.etag.clone());
            refreshed.last_modified =
                header_string(headers, LAST_MODIFIED).or_else(|| cached.last_modified.clone());
            refreshed.summary = parsed.summary;
            refreshed.selected_interface = parsed.selected_interface;
            refreshed.raw = parsed.raw;
            return Ok(refreshed);
        }
        if response.status().is_redirection() {
            return Err(A2aError::card_refresh(
                "redirects are forbidden for Agent Card discovery",
            ));
        }
        if response.status() != reqwest::StatusCode::OK {
            let status = response.status();
            return Err(A2aError::card_refresh(format!(
                "card endpoint returned HTTP {}",
                status.as_u16()
            )));
        }
        let headers = response.headers().clone();
        let bytes = read_bounded_bytes(response, MAX_CARD_BYTES, "Agent Card")
            .await
            .map_err(|error| match error {
                A2aError::ResponseTooLarge { .. } => A2aError::CardTooLarge {
                    limit: MAX_CARD_BYTES,
                },
                other => A2aError::card_refresh(other),
            })?;
        let mut raw: Value = serde_json::from_slice(&bytes)
            .map_err(|error| A2aError::InvalidCard(format!("invalid JSON: {error}")))?;
        redact_value(&mut raw, config.bearer_token.as_deref());
        let parsed = parse_agent_card(
            &card_url,
            raw,
            config
                .bearer_token
                .as_deref()
                .is_some_and(|v| !v.is_empty()),
        )?;
        Ok(CardSnapshot {
            card_url: card_url.to_string(),
            fetched_at: Utc::now().to_rfc3339(),
            sha256: sha256_hex(&bytes),
            refresh_kind: CardRefreshKind::Modified,
            etag: header_string(&headers, ETAG),
            last_modified: header_string(&headers, LAST_MODIFIED),
            summary: parsed.summary,
            selected_interface: parsed.selected_interface,
            raw: parsed.raw,
        })
    }

    /// Refresh the card exactly once, then execute the requested send/submit/GetTask action.
    ///
    /// `Send` preserves the original send-then-poll behavior. `Submit` checkpoints an
    /// in-progress Task immediately, while `GetTask` resumes it without sending another Message.
    /// All complete accepted wire responses remain in order.
    pub async fn invoke(
        &self,
        config: &A2aAgentConfig,
        cached: Option<&CardSnapshot>,
        request: InvokeRequest,
    ) -> A2aToolResult {
        let invocation_id = Uuid::new_v4().to_string();
        let message_id = matches!(request.action, InvokeAction::Send | InvokeAction::Submit)
            .then(|| Uuid::new_v4().to_string());
        // A model-provided override may tighten the user's configured budget, never expand it.
        let timeout_seconds = request
            .timeout_seconds
            .unwrap_or(config.timeout_seconds)
            .min(config.timeout_seconds);
        let mut result = empty_result(
            config,
            &request,
            invocation_id.clone(),
            message_id.clone(),
            timeout_seconds,
        );
        if let Err(error) = validate_config(config).and_then(|_| request.validate()) {
            result.terminal = A2aTerminal::error(
                TerminalKind::ProtocolError,
                redact_text(&error.to_string(), config),
            );
            return result;
        }

        let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
        let refresh_timeout = remaining(deadline).min(self.options.card_timeout);
        let card =
            match tokio::time::timeout(refresh_timeout, self.refresh_card(config, cached)).await {
                Ok(Ok(card)) => card,
                Ok(Err(error)) => {
                    result.terminal = A2aTerminal::error(
                        TerminalKind::CardRefreshError,
                        redact_text(&error.to_string(), config),
                    );
                    return result;
                }
                Err(_) => {
                    result.terminal = A2aTerminal::error(
                        TerminalKind::CardRefreshError,
                        "Agent Card refresh timed out",
                    );
                    return result;
                }
            };
        let interface = card.selected_interface.clone();
        result.card = Some(card);

        let mut meaning = match request.action {
            InvokeAction::Send | InvokeAction::Submit => {
                let accepted_output_modes = match negotiate_output_modes(
                    &result.card.as_ref().expect("card was just stored").raw,
                ) {
                    Ok(modes) => modes,
                    Err(error) => {
                        result.terminal = A2aTerminal::error(
                            TerminalKind::CardRefreshError,
                            redact_text(&error.to_string(), config),
                        );
                        return result;
                    }
                };
                let send = match build_send(
                    &interface,
                    &request,
                    &invocation_id,
                    message_id
                        .as_deref()
                        .expect("send and submit always allocate a message id"),
                    &accepted_output_modes,
                ) {
                    Ok(send) => send,
                    Err(error) => {
                        result.terminal = protocol_terminal(error, config);
                        return result;
                    }
                };
                let send_response = match self
                    .execute_operation(config, &interface, send, deadline)
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        result.terminal = match error {
                            A2aError::ResponseTooLarge { .. }
                            | A2aError::TotalResponseTooLarge { .. } => A2aTerminal::error(
                                TerminalKind::SizeLimit,
                                redact_text(&error.to_string(), config),
                            ),
                            A2aError::Timeout => A2aTerminal::error(
                                TerminalKind::OutcomeUnknown,
                                "SendMessage timed out after it may have reached the remote Agent; it was not retried",
                            ),
                            _ => A2aTerminal::error(
                                TerminalKind::OutcomeUnknown,
                                format!(
                                    "SendMessage outcome is unknown and was not retried: {}",
                                    redact_text(&error.to_string(), config)
                                ),
                            ),
                        };
                        return result;
                    }
                };
                if let Err(error) = push_frame(&mut result, send_response) {
                    result.terminal =
                        A2aTerminal::error(TerminalKind::SizeLimit, error.to_string());
                    return result;
                }
                let send_frame = result.responses.last().expect("send frame was just pushed");
                if !(200..300).contains(&send_frame.http_status) {
                    result.terminal = A2aTerminal::error(
                        TerminalKind::ProtocolError,
                        format!("SendMessage returned HTTP {}", send_frame.http_status),
                    );
                    return result;
                }
                let meaning = match parse_send_response(
                    &interface,
                    &send_frame.payload,
                    send_frame.request_id.as_deref(),
                ) {
                    Ok(meaning) => meaning,
                    Err(error) => {
                        result.terminal = protocol_terminal(error, config);
                        return result;
                    }
                };
                match meaning {
                    ResponseMeaning::Task {
                        task_id,
                        context_id,
                        state,
                        disposition,
                    } => {
                        let expected_task =
                            request.task_id.as_deref().filter(|value| !value.is_empty());
                        if let Some(expected) = expected_task {
                            if expected != task_id {
                                result.terminal = continuation_error_terminal(
                                    &request,
                                    format!(
                                        "SendMessage returned task id {task_id}, expected {expected}"
                                    ),
                                );
                                return result;
                            }
                        }
                        let expected_context = request
                            .context_id
                            .as_deref()
                            .filter(|value| !value.is_empty());
                        let returned_context =
                            context_id.as_deref().filter(|value| !value.is_empty());
                        if let (Some(expected), Some(returned)) =
                            (expected_context, returned_context)
                        {
                            if expected != returned {
                                result.terminal = continuation_error_terminal(
                                    &request,
                                    format!(
                                        "SendMessage returned context id {returned}, expected {expected}"
                                    ),
                                );
                                return result;
                            }
                        }
                        ResponseMeaning::Task {
                            task_id,
                            context_id: context_id.or_else(|| expected_context.map(str::to_string)),
                            state,
                            disposition,
                        }
                    }
                    other => other,
                }
            }
            InvokeAction::GetTask => {
                let task_id = request
                    .task_id
                    .as_deref()
                    .expect("get_task validation requires a task id");
                match self
                    .get_task_and_record(
                        config,
                        &interface,
                        &mut result,
                        task_id,
                        request.context_id.as_deref(),
                        None,
                        &invocation_id,
                        deadline,
                    )
                    .await
                {
                    Ok(meaning) => meaning,
                    Err(terminal) => {
                        result.terminal = terminal;
                        return result;
                    }
                }
            }
            InvokeAction::CancelTask => {
                let task_id = request
                    .task_id
                    .as_deref()
                    .expect("cancel_task validation requires a task id");
                let cancel_request_id = Uuid::new_v4().to_string();
                let meaning = match self
                    .cancel_task_and_record(
                        config,
                        &interface,
                        &mut result,
                        task_id,
                        request.context_id.as_deref(),
                        &cancel_request_id,
                        deadline,
                    )
                    .await
                {
                    Ok(meaning) => meaning,
                    Err(terminal) => {
                        result.terminal = terminal;
                        return result;
                    }
                };
                result.terminal = match meaning {
                    ResponseMeaning::Task {
                        task_id,
                        context_id,
                        state,
                        disposition,
                    } => cancel_terminal(&task_id, context_id.as_deref(), &state, disposition),
                    _ => task_error_terminal(
                        TerminalKind::ProtocolError,
                        task_id,
                        request.context_id.as_deref(),
                        None,
                        "CancelTask returned a non-Task response",
                    ),
                };
                return result;
            }
        };

        let mut poll_interval = self.options.poll_initial;
        let mut polls = 0_u32;
        loop {
            match meaning {
                ResponseMeaning::Message { ref context_id } => {
                    result.terminal = A2aTerminal {
                        kind: TerminalKind::Message,
                        task_id: None,
                        context_id: context_id.clone(),
                        state: None,
                        success: true,
                        error: None,
                    };
                    return result;
                }
                ResponseMeaning::RemoteError { ref message } => {
                    result.terminal = A2aTerminal::error(
                        TerminalKind::ProtocolError,
                        redact_text(message, config),
                    );
                    return result;
                }
                ResponseMeaning::Task {
                    ref task_id,
                    ref context_id,
                    ref state,
                    disposition,
                } => match disposition {
                    TaskDisposition::Success => {
                        result.terminal =
                            task_terminal(task_id, context_id.as_deref(), state, true, None);
                        return result;
                    }
                    TaskDisposition::Interrupted => {
                        result.terminal =
                            task_interrupted_terminal(task_id, context_id.as_deref(), state);
                        return result;
                    }
                    TaskDisposition::Failure => {
                        result.terminal = task_terminal(
                            task_id,
                            context_id.as_deref(),
                            state,
                            false,
                            Some("remote task ended unsuccessfully"),
                        );
                        return result;
                    }
                    TaskDisposition::InProgress => {
                        if request.action == InvokeAction::Submit {
                            result.terminal =
                                task_pending_terminal(task_id, context_id.as_deref(), state);
                            return result;
                        }
                        if polls >= self.options.max_polls || Instant::now() >= deadline {
                            result.terminal = A2aTerminal {
                                kind: TerminalKind::Timeout,
                                task_id: Some(task_id.clone()),
                                context_id: context_id.clone(),
                                state: Some(state.clone()),
                                success: false,
                                error: Some(
                                    "local wait timed out; remote task outcome is unknown".into(),
                                ),
                            };
                            return result;
                        }
                        let sleep_for = poll_interval.min(remaining(deadline));
                        tokio::time::sleep(sleep_for).await;
                        poll_interval = (poll_interval * 2).min(self.options.poll_max);
                        polls += 1;
                        let get_request_id = Uuid::new_v4().to_string();
                        meaning = match self
                            .get_task_and_record(
                                config,
                                &interface,
                                &mut result,
                                task_id,
                                context_id.as_deref(),
                                Some(state),
                                &get_request_id,
                                deadline,
                            )
                            .await
                        {
                            Ok(meaning) => meaning,
                            Err(terminal) => {
                                result.terminal = terminal;
                                return result;
                            }
                        };
                    }
                },
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn get_task_and_record(
        &self,
        config: &A2aAgentConfig,
        interface: &SelectedInterface,
        result: &mut A2aToolResult,
        task_id: &str,
        context_id: Option<&str>,
        state: Option<&str>,
        request_id: &str,
        deadline: Instant,
    ) -> Result<ResponseMeaning, A2aTerminal> {
        let operation = build_get_task(interface, task_id, request_id).map_err(|error| {
            task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                state,
                redact_text(&error.to_string(), config),
            )
        })?;
        let response = self
            .execute_operation(config, interface, operation, deadline)
            .await
            .map_err(|error| match error {
                A2aError::ResponseTooLarge { .. } | A2aError::TotalResponseTooLarge { .. } => {
                    task_error_terminal(
                        TerminalKind::SizeLimit,
                        task_id,
                        context_id,
                        state,
                        redact_text(&error.to_string(), config),
                    )
                }
                A2aError::Timeout => task_error_terminal(
                    TerminalKind::Timeout,
                    task_id,
                    context_id,
                    state,
                    "GetTask timed out; remote task outcome is unknown",
                ),
                _ => task_error_terminal(
                    TerminalKind::TransportError,
                    task_id,
                    context_id,
                    state,
                    redact_text(&error.to_string(), config),
                ),
            })?;
        push_frame(result, response).map_err(|error| {
            task_error_terminal(
                TerminalKind::SizeLimit,
                task_id,
                context_id,
                state,
                error.to_string(),
            )
        })?;

        let frame = result
            .responses
            .last()
            .expect("GetTask frame was just pushed");
        if !(200..300).contains(&frame.http_status) {
            return Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                state,
                format!("GetTask returned HTTP {}", frame.http_status),
            ));
        }
        match parse_get_task_response(interface, &frame.payload, frame.request_id.as_deref()) {
            Ok(ResponseMeaning::Task {
                task_id: next_task_id,
                context_id: next_context_id,
                state: next_state,
                disposition,
            }) => {
                if next_task_id != task_id {
                    return Err(task_error_terminal(
                        TerminalKind::ProtocolError,
                        task_id,
                        context_id,
                        state,
                        format!("GetTask returned task id {next_task_id}, expected {task_id}"),
                    ));
                }
                let expected_context = context_id.filter(|value| !value.is_empty());
                let returned_context = next_context_id.as_deref().filter(|value| !value.is_empty());
                if let (Some(expected), Some(returned)) = (expected_context, returned_context) {
                    if expected != returned {
                        return Err(task_error_terminal(
                            TerminalKind::ProtocolError,
                            task_id,
                            context_id,
                            state,
                            format!("GetTask returned context id {returned}, expected {expected}"),
                        ));
                    }
                }
                Ok(ResponseMeaning::Task {
                    task_id: next_task_id,
                    context_id: next_context_id.or_else(|| expected_context.map(str::to_string)),
                    state: next_state,
                    disposition,
                })
            }
            Ok(ResponseMeaning::RemoteError { message }) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                state,
                redact_text(&message, config),
            )),
            Ok(ResponseMeaning::Message { .. }) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                state,
                "GetTask returned a Message instead of a Task",
            )),
            Err(error) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                state,
                redact_text(&error.to_string(), config),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn cancel_task_and_record(
        &self,
        config: &A2aAgentConfig,
        interface: &SelectedInterface,
        result: &mut A2aToolResult,
        task_id: &str,
        context_id: Option<&str>,
        request_id: &str,
        deadline: Instant,
    ) -> Result<ResponseMeaning, A2aTerminal> {
        let operation = build_cancel_task(interface, task_id, request_id).map_err(|error| {
            task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                None,
                redact_text(&error.to_string(), config),
            )
        })?;
        let response = self
            .execute_operation(config, interface, operation, deadline)
            .await
            .map_err(|error| match error {
                A2aError::ResponseTooLarge { .. } | A2aError::TotalResponseTooLarge { .. } => {
                    task_error_terminal(
                        TerminalKind::SizeLimit,
                        task_id,
                        context_id,
                        None,
                        redact_text(&error.to_string(), config),
                    )
                }
                A2aError::Timeout => task_error_terminal(
                    TerminalKind::OutcomeUnknown,
                    task_id,
                    context_id,
                    None,
                    "CancelTask timed out after it may have reached the remote Agent; it was not retried",
                ),
                _ => task_error_terminal(
                    TerminalKind::OutcomeUnknown,
                    task_id,
                    context_id,
                    None,
                    format!(
                        "CancelTask outcome is unknown and was not retried: {}",
                        redact_text(&error.to_string(), config)
                    ),
                ),
            })?;
        push_frame(result, response).map_err(|error| {
            task_error_terminal(
                TerminalKind::SizeLimit,
                task_id,
                context_id,
                None,
                error.to_string(),
            )
        })?;

        let frame = result
            .responses
            .last()
            .expect("CancelTask frame was just pushed");
        if !(200..300).contains(&frame.http_status) {
            return Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                None,
                format!("CancelTask returned HTTP {}", frame.http_status),
            ));
        }
        match parse_cancel_task_response(interface, &frame.payload, frame.request_id.as_deref()) {
            Ok(ResponseMeaning::Task {
                task_id: next_task_id,
                context_id: next_context_id,
                state,
                disposition,
            }) => {
                if next_task_id != task_id {
                    return Err(task_error_terminal(
                        TerminalKind::ProtocolError,
                        task_id,
                        context_id,
                        None,
                        format!("CancelTask returned task id {next_task_id}, expected {task_id}"),
                    ));
                }
                let expected_context = context_id.filter(|value| !value.is_empty());
                let returned_context = next_context_id.as_deref().filter(|value| !value.is_empty());
                if let (Some(expected), Some(returned)) = (expected_context, returned_context) {
                    if expected != returned {
                        return Err(task_error_terminal(
                            TerminalKind::ProtocolError,
                            task_id,
                            context_id,
                            None,
                            format!(
                                "CancelTask returned context id {returned}, expected {expected}"
                            ),
                        ));
                    }
                }
                Ok(ResponseMeaning::Task {
                    task_id: next_task_id,
                    context_id: next_context_id.or_else(|| expected_context.map(str::to_string)),
                    state,
                    disposition,
                })
            }
            Ok(ResponseMeaning::RemoteError { message }) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                None,
                redact_text(&message, config),
            )),
            Ok(ResponseMeaning::Message { .. }) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                None,
                "CancelTask returned a Message instead of a Task",
            )),
            Err(error) => Err(task_error_terminal(
                TerminalKind::ProtocolError,
                task_id,
                context_id,
                None,
                redact_text(&error.to_string(), config),
            )),
        }
    }

    async fn execute_operation(
        &self,
        config: &A2aAgentConfig,
        interface: &SelectedInterface,
        operation: OutboundOperation,
        deadline: Instant,
    ) -> Result<ResponseFrame, A2aError> {
        let remaining = remaining(deadline);
        if remaining.is_zero() {
            return Err(A2aError::Timeout);
        }
        let mut request = self
            .http
            .request(operation.method, operation.url)
            .header("A2A-Version", interface.protocol_version.wire())
            .timeout(remaining);
        let media_type = match (interface.protocol_version, interface.binding) {
            (ProtocolVersion::V1, ProtocolBinding::HttpJson) => "application/a2a+json",
            _ => "application/json",
        };
        request = request.header(ACCEPT, media_type);
        if operation.body.is_some() {
            request = request.header(CONTENT_TYPE, media_type);
        }
        if let Some(token) = config.bearer_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = operation.body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                A2aError::Timeout
            } else {
                A2aError::transport(redact_text(&error.to_string(), config))
            }
        })?;
        if response.status().is_redirection() {
            return Err(A2aError::Transport(
                "redirects are forbidden for A2A operations".into(),
            ));
        }
        let status = response.status().as_u16();
        let bytes = read_bounded_bytes(response, MAX_RESPONSE_BYTES, operation.operation).await?;
        let (mut payload, valid_json) = lossless_json(&bytes, config.bearer_token.as_deref());
        redact_value(&mut payload, config.bearer_token.as_deref());
        if !valid_json {
            // Preserve the entire bounded body in the frame; version parsing will then surface a
            // clear protocol error after the caller commits the frame to its timeline.
        }
        Ok(ResponseFrame {
            sequence: 0,
            operation: operation.operation.to_string(),
            received_at: Utc::now().to_rfc3339(),
            http_status: status,
            protocol_version: interface.protocol_version,
            binding: interface.binding,
            request_id: operation.request_id,
            payload,
            wire_bytes: bytes.len(),
        })
    }
}

fn empty_result(
    config: &A2aAgentConfig,
    request: &InvokeRequest,
    invocation_id: String,
    message_id: Option<String>,
    timeout_seconds: u64,
) -> A2aToolResult {
    A2aToolResult {
        schema: A2A_RESULT_SCHEMA.into(),
        agent: A2aAgentRef {
            config_id: config.id.clone(),
            display_name: config.name.clone(),
            configured_endpoint: config.endpoint.clone(),
        },
        card: None,
        request: A2aRequestRecord {
            invocation_id,
            action: request.action,
            message_id,
            skill_id: request.skill_id.clone(),
            task_id: request.task_id.clone(),
            context_id: request.context_id.clone(),
            task: request.task.clone(),
            timeout_seconds,
        },
        responses: Vec::new(),
        terminal: A2aTerminal::error(TerminalKind::ProtocolError, "invocation not started"),
        warnings: Vec::new(),
    }
}

fn push_frame(result: &mut A2aToolResult, mut frame: ResponseFrame) -> Result<(), A2aError> {
    let accepted: usize = result.responses.iter().map(|item| item.wire_bytes).sum();
    if accepted.saturating_add(frame.wire_bytes) > MAX_TOTAL_RESPONSE_BYTES {
        return Err(A2aError::TotalResponseTooLarge {
            limit: MAX_TOTAL_RESPONSE_BYTES,
        });
    }
    frame.sequence = result.responses.len() as u32 + 1;
    result.responses.push(frame);
    Ok(())
}

fn task_terminal(
    task_id: &str,
    context_id: Option<&str>,
    state: &str,
    success: bool,
    error: Option<&str>,
) -> A2aTerminal {
    A2aTerminal {
        kind: TerminalKind::Task,
        task_id: Some(task_id.into()),
        context_id: context_id.map(str::to_string),
        state: Some(state.into()),
        success,
        error: error.map(str::to_string),
    }
}

fn task_pending_terminal(task_id: &str, context_id: Option<&str>, state: &str) -> A2aTerminal {
    A2aTerminal {
        kind: TerminalKind::TaskPending,
        task_id: Some(task_id.into()),
        context_id: context_id.map(str::to_string),
        state: Some(state.into()),
        success: true,
        error: None,
    }
}

fn task_interrupted_terminal(task_id: &str, context_id: Option<&str>, state: &str) -> A2aTerminal {
    A2aTerminal {
        kind: TerminalKind::TaskInterrupted,
        task_id: Some(task_id.into()),
        context_id: context_id.map(str::to_string),
        state: Some(state.into()),
        // The local A2A operation succeeded. The distinct kind/state says the remote Task is
        // paused rather than complete, and `is_error()` must remain false so the harness can
        // retain the handle and ask for the required continuation.
        success: true,
        error: None,
    }
}

fn cancel_terminal(
    task_id: &str,
    context_id: Option<&str>,
    state: &str,
    disposition: TaskDisposition,
) -> A2aTerminal {
    if matches!(
        state,
        "TASK_STATE_CANCELED" | "TASK_STATE_CANCELLED" | "canceled"
    ) {
        return task_terminal(task_id, context_id, state, true, None);
    }
    match disposition {
        TaskDisposition::InProgress => task_pending_terminal(task_id, context_id, state),
        TaskDisposition::Success => task_terminal(task_id, context_id, state, true, None),
        TaskDisposition::Interrupted => task_interrupted_terminal(task_id, context_id, state),
        TaskDisposition::Failure => task_terminal(
            task_id,
            context_id,
            state,
            false,
            Some("remote cancellation ended unsuccessfully"),
        ),
    }
}

fn task_error_terminal(
    kind: TerminalKind,
    task_id: &str,
    context_id: Option<&str>,
    state: Option<&str>,
    error: impl Into<String>,
) -> A2aTerminal {
    A2aTerminal {
        kind,
        task_id: Some(task_id.into()),
        context_id: context_id.map(str::to_string),
        state: state.map(str::to_string),
        success: false,
        error: Some(error.into()),
    }
}

fn continuation_error_terminal(request: &InvokeRequest, error: impl Into<String>) -> A2aTerminal {
    A2aTerminal {
        kind: TerminalKind::ProtocolError,
        task_id: request.task_id.clone(),
        context_id: request.context_id.clone(),
        state: None,
        success: false,
        error: Some(error.into()),
    }
}

fn protocol_terminal(error: A2aError, config: &A2aAgentConfig) -> A2aTerminal {
    let kind = match error {
        A2aError::ResponseTooLarge { .. } | A2aError::TotalResponseTooLarge { .. } => {
            TerminalKind::SizeLimit
        }
        A2aError::Timeout => TerminalKind::Timeout,
        _ => TerminalKind::ProtocolError,
    };
    A2aTerminal::error(kind, redact_text(&error.to_string(), config))
}

async fn read_bounded_bytes(
    response: reqwest::Response,
    limit: usize,
    operation: &str,
) -> Result<Vec<u8>, A2aError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(A2aError::ResponseTooLarge {
            operation: operation.into(),
            limit,
        });
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(A2aError::transport)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(A2aError::ResponseTooLarge {
                operation: operation.into(),
                limit,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn lossless_json(bytes: &[u8], token: Option<&str>) -> (Value, bool) {
    match serde_json::from_slice(bytes) {
        Ok(value) => (value, true),
        Err(error) => {
            let (safe_bytes, redacted) = redact_bytes(bytes, token);
            (
                json!({
                "_invalid_json": true,
                "_parse_error": error.to_string(),
                "_raw_base64": base64::engine::general_purpose::STANDARD.encode(safe_bytes),
                "_raw_redacted": redacted,
                }),
                false,
            )
        }
    }
}

fn redact_bytes(bytes: &[u8], token: Option<&str>) -> (Vec<u8>, bool) {
    let Some(needle) = token
        .filter(|token| !token.is_empty())
        .map(str::as_bytes)
        .filter(|token| !token.is_empty())
    else {
        return (bytes.to_vec(), false);
    };
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0_usize;
    let mut redacted = false;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(needle) {
            output.extend_from_slice(b"<redacted>");
            cursor += needle.len();
            redacted = true;
        } else {
            output.push(bytes[cursor]);
            cursor += 1;
        }
    }
    (output, redacted)
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
}

fn redact_text(value: &str, config: &A2aAgentConfig) -> String {
    match config
        .bearer_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        Some(token) => value.replace(token, "<redacted>"),
        None => value.to_string(),
    }
}

fn redact_value(value: &mut Value, token: Option<&str>) {
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return;
    };
    match value {
        Value::String(text) => *text = text.replace(token, "<redacted>"),
        Value::Array(items) => {
            for item in items {
                redact_value(item, Some(token));
            }
        }
        Value::Object(object) => {
            let previous = std::mem::take(object);
            for (key, mut item) in previous {
                redact_value(&mut item, Some(token));
                let base_key = key.replace(token, "<redacted>");
                let mut safe_key = base_key.clone();
                let mut collision = 1_u32;
                while object.contains_key(&safe_key) {
                    safe_key = format!("{base_key}#redacted-{collision}");
                    collision = collision.saturating_add(1);
                }
                object.insert(safe_key, item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_schema_and_secret_redaction_are_stable() {
        let config = A2aAgentConfig {
            id: "agent-a".into(),
            name: "Agent A".into(),
            endpoint: "http://127.0.0.1:9999".into(),
            enabled: true,
            bearer_token: Some("super-secret".into()),
            timeout_seconds: 120,
        };
        let mut value = json!({
            "nested":["Bearer super-secret"],
            "super-secret": "key is sensitive",
            "<redacted>": "collision must not discard either value"
        });
        redact_value(&mut value, config.bearer_token.as_deref());
        assert!(!value.to_string().contains("super-secret"));
        assert_eq!(value.as_object().unwrap().len(), 3);
        let invalid = b"not-json super-secret tail";
        let (safe_invalid, valid) = lossless_json(invalid, config.bearer_token.as_deref());
        assert!(!valid);
        assert!(!safe_invalid.to_string().contains("super-secret"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(safe_invalid["_raw_base64"].as_str().unwrap())
            .unwrap();
        assert!(!String::from_utf8_lossy(&decoded).contains("super-secret"));
        assert_eq!(safe_invalid["_raw_redacted"], true);
        let result = empty_result(
            &config,
            &InvokeRequest::new("task"),
            "i".into(),
            Some("m".into()),
            120,
        );
        assert_eq!(result.schema, A2A_RESULT_SCHEMA);
        assert!(!result.to_json().contains("super-secret"));
    }

    #[test]
    fn legacy_request_records_deserialize_as_send_with_a_message_id() {
        let record: A2aRequestRecord = serde_json::from_value(json!({
            "invocation_id": "invocation-1",
            "message_id": "message-1",
            "task": "legacy request",
            "timeout_seconds": 30
        }))
        .unwrap();

        assert_eq!(record.action, InvokeAction::Send);
        assert_eq!(record.message_id.as_deref(), Some("message-1"));
    }
}
