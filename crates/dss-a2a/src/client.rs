use std::net::{IpAddr, SocketAddr};
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
    negotiate_output_modes, parse_agent_card, parse_registry_agent_card, resolve_agent_card_url,
    CardRefreshKind,
};
use crate::protocol::{
    build_cancel_task, build_get_task, build_send, parse_cancel_task_response,
    parse_get_task_response, parse_registry_send_response, parse_send_response, OutboundOperation,
    ResponseMeaning, TaskDisposition,
};
use crate::result::{
    A2aAgentRef, A2aRequestRecord, A2aTerminal, A2aToolResult, ResponseFrame, TerminalKind,
    A2A_RESULT_SCHEMA, REGISTRY_DIRECT_TASK_WARNING,
};
use crate::types::{
    validate_config, A2aRouteOptions, InvokeAction, InvokeRequest, ProtocolBinding,
    ProtocolVersion, RegistryInvocationPolicy, MAX_CARD_BYTES, MAX_RESPONSE_BYTES,
    MAX_TOTAL_RESPONSE_BYTES,
};
use crate::{A2aError, CardSnapshot, SelectedInterface};

const MAX_REGISTRY_RESOLVED_ADDRESSES: usize = 16;

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
    route_options: A2aRouteOptions,
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
        Self::with_options_and_route_options(options, A2aRouteOptions::default())
    }

    pub fn with_route_options(route_options: A2aRouteOptions) -> Result<Self, A2aError> {
        Self::with_options_and_route_options(A2aClientOptions::default(), route_options)
    }

    pub fn with_options_and_route_options(
        options: A2aClientOptions,
        route_options: A2aRouteOptions,
    ) -> Result<Self, A2aError> {
        validate_route_options(&route_options)?;
        let http = build_http_client(&options, &route_options, None)?;
        Ok(Self {
            http,
            options,
            route_options,
        })
    }

    async fn registry_pinned_client(
        &self,
        policy: &RegistryInvocationPolicy,
        deadline: Instant,
    ) -> Result<Self, A2aError> {
        let endpoint = policy.descriptor_endpoint();
        let host = endpoint
            .host_str()
            .ok_or_else(|| A2aError::InvalidEndpoint("endpoint has no host".into()))?;
        let addresses = self.resolve_registry_addresses(policy, deadline).await?;
        ensure_before_deadline(deadline)?;
        let http = build_http_client(&self.options, &self.route_options, Some((host, &addresses)))?;
        ensure_before_deadline(deadline)?;
        Ok(Self {
            http,
            options: self.options.clone(),
            route_options: self.route_options.clone(),
        })
    }

    async fn resolve_registry_addresses(
        &self,
        policy: &RegistryInvocationPolicy,
        deadline: Instant,
    ) -> Result<Vec<SocketAddr>, A2aError> {
        ensure_before_deadline(deadline)?;
        let endpoint = policy.descriptor_endpoint();
        let host = endpoint
            .host_str()
            .ok_or_else(|| A2aError::InvalidEndpoint("endpoint has no host".into()))?;
        let port = endpoint.port_or_known_default().ok_or_else(|| {
            A2aError::InvalidEndpoint("Registry endpoint has no usable port".into())
        })?;

        if policy.allows_loopback_http() {
            let address = parse_ip_literal(host)
                .filter(IpAddr::is_loopback)
                .ok_or_else(|| {
                    A2aError::InvalidEndpoint(
                        "test Registry endpoint is not a literal loopback address".into(),
                    )
                })?;
            return Ok(vec![SocketAddr::new(address, port)]);
        }

        if let Some(address) =
            self.route_options
                .resolve
                .iter()
                .find_map(|(route_host, address)| {
                    route_host.eq_ignore_ascii_case(host).then_some(*address)
                })
        {
            let addresses = vec![address];
            validate_registry_public_addresses(&addresses)?;
            ensure_before_deadline(deadline)?;
            return Ok(addresses);
        }

        let lookup_timeout = timeout_within_deadline(self.options.connect_timeout, deadline)?;
        let lookup = tokio::time::timeout(lookup_timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| A2aError::Timeout)?
            .map_err(|_| A2aError::transport("Registry endpoint DNS lookup failed"))?;
        let mut addresses: Vec<SocketAddr> =
            lookup.take(MAX_REGISTRY_RESOLVED_ADDRESSES + 1).collect();
        if addresses.len() > MAX_REGISTRY_RESOLVED_ADDRESSES {
            return Err(A2aError::InvalidEndpoint(format!(
                "Registry endpoint resolved to more than {MAX_REGISTRY_RESOLVED_ADDRESSES} addresses"
            )));
        }
        addresses.sort_unstable();
        addresses.dedup();
        validate_registry_public_addresses(&addresses)?;
        ensure_before_deadline(deadline)?;
        Ok(addresses)
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
        self.refresh_card_with_policy(config, cached, None, None)
            .await
            .map(|(card, _warnings)| card)
    }

    async fn refresh_card_with_policy(
        &self,
        config: &A2aAgentConfig,
        cached: Option<&CardSnapshot>,
        registry_policy: Option<&RegistryInvocationPolicy>,
        deadline: Option<Instant>,
    ) -> Result<(CardSnapshot, Vec<String>), A2aError> {
        validate_config(config)?;
        if let Some(policy) = registry_policy {
            policy.validate_config_binding(config)?;
        }
        let card_url = resolve_agent_card_url(&config.endpoint)?;
        let request_timeout = match deadline {
            Some(deadline) => timeout_within_deadline(self.options.card_timeout, deadline)?,
            None => self.options.card_timeout,
        };
        let mut request = self
            .http
            .get(card_url.clone())
            .header(CACHE_CONTROL, "no-cache")
            .header(ACCEPT, "application/json")
            .timeout(request_timeout);
        if let Some(token) = config.bearer_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(cached) = cached.filter(|cached| cached.card_url == card_url.as_str()) {
            if let Some(etag) = cached
                .etag
                .as_deref()
                .and_then(|value| conditional_header_value(value, config.bearer_token.as_deref()))
            {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = cached
                .last_modified
                .as_deref()
                .and_then(|value| conditional_header_value(value, config.bearer_token.as_deref()))
            {
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
            let (parsed, warnings) =
                parse_card_for_policy(&card_url, cached.raw.clone(), config, registry_policy)?;
            refreshed.fetched_at = Utc::now().to_rfc3339();
            refreshed.refresh_kind = CardRefreshKind::NotModified;
            refreshed.etag = safe_validator_header(headers, ETAG, config.bearer_token.as_deref())
                .or_else(|| {
                    cached.etag.clone().filter(|value| {
                        !contains_bearer_token(value, config.bearer_token.as_deref())
                    })
                });
            refreshed.last_modified =
                safe_validator_header(headers, LAST_MODIFIED, config.bearer_token.as_deref())
                    .or_else(|| {
                        cached.last_modified.clone().filter(|value| {
                            !contains_bearer_token(value, config.bearer_token.as_deref())
                        })
                    });
            refreshed.summary = parsed.summary;
            refreshed.selected_interface = parsed.selected_interface;
            refreshed.raw = parsed.raw;
            return Ok((refreshed, warnings));
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
        let (parsed, warnings) = parse_card_for_policy(&card_url, raw, config, registry_policy)?;
        Ok((
            CardSnapshot {
                card_url: card_url.to_string(),
                fetched_at: Utc::now().to_rfc3339(),
                sha256: sha256_hex(&bytes),
                refresh_kind: CardRefreshKind::Modified,
                etag: safe_validator_header(&headers, ETAG, config.bearer_token.as_deref()),
                last_modified: safe_validator_header(
                    &headers,
                    LAST_MODIFIED,
                    config.bearer_token.as_deref(),
                ),
                summary: parsed.summary,
                selected_interface: parsed.selected_interface,
                raw: parsed.raw,
            },
            warnings,
        ))
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
        self.invoke_with_policy(config, cached, request, None, None)
            .await
    }

    /// Invoke an Agent through an exact, anonymously callable Registry descriptor endpoint.
    ///
    /// This is the only entry point that enables Registry compatibility. It never sends A2A
    /// credentials and records each compatibility decision in `A2aToolResult::warnings`.
    pub async fn invoke_registry_anonymous(
        &self,
        config: &A2aAgentConfig,
        cached: Option<&CardSnapshot>,
        policy: &RegistryInvocationPolicy,
        request: InvokeRequest,
    ) -> A2aToolResult {
        if let Err(error) = validate_config(config)
            .and_then(|_| request.validate())
            .and_then(|_| policy.validate_config_binding(config))
        {
            return invocation_preflight_error(
                config,
                &request,
                TerminalKind::ProtocolError,
                redact_text(&error.to_string(), config),
            );
        }
        let timeout_seconds = effective_timeout_seconds(config, &request);
        let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
        let pinned = match self.registry_pinned_client(policy, deadline).await {
            Ok(client) => client,
            Err(error) => {
                let kind = registry_preflight_terminal_kind(&error);
                return invocation_preflight_error(
                    config,
                    &request,
                    kind,
                    format!(
                        "Registry endpoint validation failed before HTTP access: {}",
                        redact_text(&error.to_string(), config)
                    ),
                );
            }
        };
        pinned
            .invoke_with_policy(config, cached, request, Some(policy), Some(deadline))
            .await
    }

    async fn invoke_with_policy(
        &self,
        config: &A2aAgentConfig,
        cached: Option<&CardSnapshot>,
        request: InvokeRequest,
        registry_policy: Option<&RegistryInvocationPolicy>,
        deadline: Option<Instant>,
    ) -> A2aToolResult {
        let invocation_id = Uuid::new_v4().to_string();
        let message_id = matches!(request.action, InvokeAction::Send | InvokeAction::Submit)
            .then(|| Uuid::new_v4().to_string());
        // A model-provided override may tighten the user's configured budget, never expand it.
        let timeout_seconds = effective_timeout_seconds(config, &request);
        let mut result = empty_result(
            config,
            &request,
            invocation_id.clone(),
            message_id.clone(),
            timeout_seconds,
        );
        let validation = validate_config(config)
            .and_then(|_| request.validate())
            .and_then(|_| {
                registry_policy
                    .map(|policy| policy.validate_config_binding(config))
                    .unwrap_or(Ok(()))
            });
        if let Err(error) = validation {
            result.terminal = A2aTerminal::error(
                TerminalKind::ProtocolError,
                redact_text(&error.to_string(), config),
            );
            return result;
        }

        let deadline =
            deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(timeout_seconds));
        let refresh_timeout = match timeout_within_deadline(self.options.card_timeout, deadline) {
            Ok(timeout) => timeout,
            Err(_) => {
                result.terminal = A2aTerminal::error(
                    TerminalKind::CardRefreshError,
                    "Agent Card refresh timed out",
                );
                return result;
            }
        };
        let (card, card_warnings) = match tokio::time::timeout(
            refresh_timeout,
            self.refresh_card_with_policy(config, cached, registry_policy, Some(deadline)),
        )
        .await
        {
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
        result.warnings.extend(card_warnings);

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
                    result.terminal = operation_http_error(
                        "SendMessage",
                        send_frame.http_status,
                        registry_policy.is_some(),
                    );
                    return result;
                }
                let parsed = if registry_policy.is_some() {
                    parse_registry_send_response(
                        &interface,
                        &send_frame.payload,
                        send_frame.request_id.as_deref(),
                    )
                } else {
                    parse_send_response(
                        &interface,
                        &send_frame.payload,
                        send_frame.request_id.as_deref(),
                    )
                    .map(|meaning| (meaning, false))
                };
                let (meaning, direct_task_fallback) = match parsed {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        result.terminal = protocol_terminal(error, config);
                        return result;
                    }
                };
                if direct_task_fallback {
                    result
                        .warnings
                        .push(REGISTRY_DIRECT_TASK_WARNING.to_string());
                }
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
                        registry_policy.is_some(),
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
                        registry_policy.is_some(),
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
                                registry_policy.is_some(),
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
        registry_anonymous: bool,
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
            let operation_error =
                operation_http_error("GetTask", frame.http_status, registry_anonymous);
            return Err(A2aTerminal {
                task_id: Some(task_id.into()),
                context_id: context_id.map(str::to_string),
                state: state.map(str::to_string),
                ..operation_error
            });
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
        registry_anonymous: bool,
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
            let operation_error =
                operation_http_error("CancelTask", frame.http_status, registry_anonymous);
            return Err(A2aTerminal {
                task_id: Some(task_id.into()),
                context_id: context_id.map(str::to_string),
                ..operation_error
            });
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

fn registry_preflight_terminal_kind(error: &A2aError) -> TerminalKind {
    match error {
        A2aError::Timeout => TerminalKind::Timeout,
        A2aError::Transport(_) => TerminalKind::TransportError,
        _ => TerminalKind::CardRefreshError,
    }
}

fn build_http_client(
    options: &A2aClientOptions,
    route_options: &A2aRouteOptions,
    pinned: Option<(&str, &[SocketAddr])>,
) -> Result<reqwest::Client, A2aError> {
    if pinned.is_some_and(|(_, addresses)| addresses.is_empty()) {
        return Err(A2aError::InvalidEndpoint(
            "Registry endpoint has no validated addresses".into(),
        ));
    }
    let routed =
        route_options.interface.is_some() || !route_options.resolve.is_empty() || pinned.is_some();
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(options.connect_timeout);
    if routed {
        // A proxy can resolve the target hostname again and defeat an explicit host pin or
        // interface route. Explicit routing therefore always creates a direct-only client.
        builder = builder.no_proxy();
    }
    if let Some(interface) = route_options.interface.as_deref() {
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
            target_os = "watchos",
        ))]
        {
            builder = builder.interface(interface);
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
            target_os = "watchos",
        )))]
        {
            let _ = interface;
            return Err(A2aError::InvalidConfig(
                "network interface routing is unsupported on this platform".into(),
            ));
        }
    }
    for (host, address) in &route_options.resolve {
        builder = builder.resolve(host, *address);
    }
    if let Some((host, addresses)) = pinned {
        // This is applied last so the one invocation's validated DNS set supersedes any original
        // route entry for that hostname while retaining the URL hostname for TLS SNI and Host.
        builder = builder.resolve_to_addrs(host, addresses);
    }
    builder.build().map_err(A2aError::transport)
}

fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

fn validate_registry_public_addresses(addresses: &[SocketAddr]) -> Result<(), A2aError> {
    if addresses.is_empty() {
        return Err(A2aError::InvalidEndpoint(
            "Registry endpoint DNS resolution returned no addresses".into(),
        ));
    }
    if addresses
        .iter()
        .any(|address| !is_public_registry_ip(address.ip()))
    {
        return Err(A2aError::InvalidEndpoint(
            "Registry endpoint did not resolve exclusively to public addresses".into(),
        ));
    }
    Ok(())
}

fn is_public_registry_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                || octets[0] >= 240)
        }
        IpAddr::V6(ip) => {
            if let Some(ipv4) = ip.to_ipv4() {
                return is_public_registry_ip(IpAddr::V4(ipv4));
            }
            let segments = ip.segments();
            let allocated_global_unicast = (segments[0] & 0xe000) == 0x2000;
            allocated_global_unicast
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && (segments[0] & 0xffc0) != 0xfec0
                // IETF protocol assignments, including Teredo and benchmarking.
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                // Deprecated transition and retired/documentation allocations.
                && segments[0] != 0x2002
                && segments[0] != 0x3ffe
                && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
        }
    }
}

fn validate_route_options(route_options: &A2aRouteOptions) -> Result<(), A2aError> {
    if route_options.interface.as_deref().is_some_and(|interface| {
        interface.is_empty()
            || interface.len() > 128
            || interface.trim() != interface
            || interface.chars().any(char::is_whitespace)
    }) {
        return Err(A2aError::InvalidConfig(
            "network interface must contain 1 to 128 safe bytes".into(),
        ));
    }
    if route_options.resolve.len() > 64 {
        return Err(A2aError::InvalidConfig(
            "at most 64 route host overrides are supported".into(),
        ));
    }
    let mut normalized_hosts = std::collections::HashSet::new();
    for host in route_options.resolve.keys() {
        if host.is_empty()
            || host.len() > 253
            || host.trim() != host
            || host.chars().any(char::is_control)
            || host
                .chars()
                .any(|character| character.is_whitespace() || "/:@?#[]".contains(character))
        {
            return Err(A2aError::InvalidConfig(
                "route override host must be a bare hostname or IP literal".into(),
            ));
        }
        if !normalized_hosts.insert(host.to_ascii_lowercase()) {
            return Err(A2aError::InvalidConfig(
                "route override hosts must be unique ignoring ASCII case".into(),
            ));
        }
    }
    Ok(())
}

fn parse_card_for_policy(
    card_url: &reqwest::Url,
    raw: Value,
    config: &A2aAgentConfig,
    registry_policy: Option<&RegistryInvocationPolicy>,
) -> Result<(crate::ParsedAgentCard, Vec<String>), A2aError> {
    match registry_policy {
        Some(policy) => parse_registry_agent_card(
            card_url,
            raw,
            policy.descriptor_endpoint(),
            policy.allows_loopback_http(),
        ),
        None => parse_agent_card(
            card_url,
            raw,
            config
                .bearer_token
                .as_deref()
                .is_some_and(|token| !token.is_empty()),
        )
        .map(|parsed| (parsed, Vec::new())),
    }
}

fn operation_http_error(operation: &str, status: u16, registry_anonymous: bool) -> A2aTerminal {
    let message = if registry_anonymous && matches!(status, 401 | 403) {
        format!(
            "Registry descriptor declared anonymous access, but {operation} returned HTTP {status}; no credentials were sent and the request was not retried"
        )
    } else {
        format!("{operation} returned HTTP {status}")
    };
    A2aTerminal::error(TerminalKind::ProtocolError, message)
}

fn invocation_preflight_error(
    config: &A2aAgentConfig,
    request: &InvokeRequest,
    kind: TerminalKind,
    message: impl Into<String>,
) -> A2aToolResult {
    let invocation_id = Uuid::new_v4().to_string();
    let timeout_seconds = effective_timeout_seconds(config, request);
    // Preflight failures happen before constructing or sending a Message. Keep
    // `message_id` absent so persisted/UI evidence cannot imply a remote side effect.
    let mut result = empty_result(config, request, invocation_id, None, timeout_seconds);
    result.terminal = A2aTerminal::error(kind, message);
    result
}

fn effective_timeout_seconds(config: &A2aAgentConfig, request: &InvokeRequest) -> u64 {
    request
        .timeout_seconds
        .unwrap_or(config.timeout_seconds)
        .min(config.timeout_seconds)
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
        registry: None,
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

fn timeout_within_deadline(limit: Duration, deadline: Instant) -> Result<Duration, A2aError> {
    let remaining = remaining(deadline);
    if remaining.is_zero() {
        return Err(A2aError::Timeout);
    }
    Ok(limit.min(remaining))
}

fn ensure_before_deadline(deadline: Instant) -> Result<(), A2aError> {
    timeout_within_deadline(Duration::MAX, deadline).map(|_| ())
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn safe_validator_header(
    headers: &HeaderMap,
    name: reqwest::header::HeaderName,
    bearer_token: Option<&str>,
) -> Option<String> {
    header_string(headers, name).filter(|value| !contains_bearer_token(value, bearer_token))
}

fn conditional_header_value(value: &str, bearer_token: Option<&str>) -> Option<HeaderValue> {
    if contains_bearer_token(value, bearer_token) {
        return None;
    }
    HeaderValue::from_str(value).ok()
}

fn contains_bearer_token(value: &str, bearer_token: Option<&str>) -> bool {
    bearer_token
        .filter(|token| !token.is_empty())
        .is_some_and(|token| value.contains(token))
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
    use axum::http::HeaderMap as AxumHeaderMap;
    use axum::routing::get;
    use axum::Router;

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

    #[test]
    fn route_options_build_with_interface_and_reject_url_shaped_hosts() {
        let client = A2aClient::with_route_options(A2aRouteOptions {
            interface: Some("fixture-interface".into()),
            resolve: [(
                "agent.example.test".into(),
                "192.0.2.10:443".parse().unwrap(),
            )]
            .into_iter()
            .collect(),
        });
        assert!(client.is_ok());

        let invalid = A2aClient::with_route_options(A2aRouteOptions {
            interface: None,
            resolve: [(
                "https://agent.example.test".into(),
                "192.0.2.10:443".parse().unwrap(),
            )]
            .into_iter()
            .collect(),
        });
        assert!(matches!(invalid, Err(A2aError::InvalidConfig(_))));
    }

    #[test]
    fn registry_address_validation_requires_a_nonempty_all_public_set() {
        let public = [
            "1.1.1.1:443".parse().unwrap(),
            "[2606:4700:4700::1111]:443".parse().unwrap(),
        ];
        assert!(validate_registry_public_addresses(&public).is_ok());
        assert!(validate_registry_public_addresses(&[]).is_err());

        for blocked in [
            "10.0.0.1:443",
            "127.0.0.1:443",
            "169.254.1.1:443",
            "100.64.0.1:443",
            "198.18.0.1:443",
            "192.0.2.1:443",
            "[::1]:443",
            "[fc00::1]:443",
            "[fe80::1]:443",
            "[2001:db8::1]:443",
            "[::ffff:127.0.0.1]:443",
        ] {
            let address = blocked.parse().unwrap();
            assert!(
                validate_registry_public_addresses(&[address]).is_err(),
                "Registry validation unexpectedly accepted {blocked}"
            );
        }

        let mixed = [
            "1.1.1.1:443".parse().unwrap(),
            "10.0.0.1:443".parse().unwrap(),
        ];
        assert!(validate_registry_public_addresses(&mixed).is_err());
    }

    #[tokio::test]
    async fn registry_resolution_validates_existing_route_overrides() {
        let policy = RegistryInvocationPolicy::anonymous("https://agent.example.test/a2a").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let public_address = "1.1.1.1:443".parse().unwrap();
        let public = A2aClient::with_route_options(A2aRouteOptions {
            interface: None,
            resolve: [("agent.example.test".into(), public_address)]
                .into_iter()
                .collect(),
        })
        .unwrap();
        assert_eq!(
            public
                .resolve_registry_addresses(&policy, deadline)
                .await
                .unwrap(),
            vec![public_address]
        );

        let private = A2aClient::with_route_options(A2aRouteOptions {
            interface: None,
            resolve: [("agent.example.test".into(), "10.0.0.1:443".parse().unwrap())]
                .into_iter()
                .collect(),
        })
        .unwrap();
        assert!(private
            .resolve_registry_addresses(&policy, deadline)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn registry_resolution_fails_when_the_single_invocation_deadline_is_exhausted() {
        let policy = RegistryInvocationPolicy::anonymous("https://agent.example.test/a2a").unwrap();
        let client = A2aClient::with_route_options(A2aRouteOptions {
            interface: None,
            resolve: [("agent.example.test".into(), "1.1.1.1:443".parse().unwrap())]
                .into_iter()
                .collect(),
        })
        .unwrap();
        let expired = Instant::now() - Duration::from_secs(1);

        assert_eq!(
            client
                .resolve_registry_addresses(&policy, expired)
                .await
                .unwrap_err(),
            A2aError::Timeout
        );
        assert_eq!(
            timeout_within_deadline(Duration::from_secs(30), expired),
            Err(A2aError::Timeout)
        );
        assert_eq!(
            registry_preflight_terminal_kind(&A2aError::Timeout),
            TerminalKind::Timeout
        );
        assert_eq!(
            registry_preflight_terminal_kind(&A2aError::Transport("dns".into())),
            TerminalKind::TransportError
        );

        let config = A2aAgentConfig {
            id: "registry-agent".into(),
            name: "Registry Agent".into(),
            endpoint: "https://agent.example.test/a2a".into(),
            enabled: true,
            bearer_token: None,
            timeout_seconds: 5,
        };
        let preflight = invocation_preflight_error(
            &config,
            &InvokeRequest::new("marker"),
            TerminalKind::Timeout,
            "resolution timed out",
        );
        assert_eq!(preflight.terminal.kind, TerminalKind::Timeout);
        assert!(preflight.request.message_id.is_none());
    }

    #[tokio::test]
    async fn registry_pin_preserves_the_original_hostname_header() {
        async fn capture_host(headers: AxumHeaderMap) -> String {
            headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/probe", get(capture_host)))
                .await
                .unwrap();
        });
        let pinned = [address];
        let http = build_http_client(
            &A2aClientOptions::default(),
            &A2aRouteOptions::default(),
            Some(("registry-pin.test", &pinned)),
        )
        .unwrap();
        let observed = http
            .get(format!("http://registry-pin.test:{}/probe", address.port()))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(observed, format!("registry-pin.test:{}", address.port()));
        server.abort();
    }
}
