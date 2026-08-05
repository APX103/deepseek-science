use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::StatusCode;
use serde_json::json;

use crate::error::LlmError;
use crate::types::{BoxedEventStream, ChatRequest, LlmClient, LlmResponse, StreamEvent, Usage};

// Keep every network phase bounded without treating long scientific answers as failures.
// `reqwest`'s request timeout spans connect through response-body completion; the
// explicit body idle guards below provide a more useful error when an SSE peer stalls.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const STREAM_RESPONSE_HEADERS_TIMEOUT: Duration = Duration::from_secs(120);
const STREAM_FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(120);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const NON_STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REQUEST_ATTEMPTS: usize = 3;
/// A successful HTTP status can still be followed by a transient content-
/// decoding/body failure before the provider emits any SSE event. Replaying is
/// safe only at that pre-publication boundary; once one event has been yielded,
/// the returned stream is never retried.
const MAX_STREAM_PREFLIGHT_ATTEMPTS: usize = 2;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);
const ERROR_BODY_BYTE_LIMIT: usize = 8 * 1024;
const ERROR_BODY_CHAR_LIMIT: usize = 500;
const MAX_SSE_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SSE_EVENTS_PER_LINE: usize = 256;
const MAX_SSE_EVENTS_PER_RESPONSE: usize = 262_144;
const MAX_SSE_CHUNKS_PER_RESPONSE: usize = 262_144;
const REDACTED_CREDENTIAL: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy)]
struct RequestTimeouts {
    stream_headers: Duration,
    non_stream_headers: Duration,
    stream_total: Duration,
    non_stream_total: Duration,
}

const DEFAULT_REQUEST_TIMEOUTS: RequestTimeouts = RequestTimeouts {
    stream_headers: STREAM_RESPONSE_HEADERS_TIMEOUT,
    // Non-streaming background work (review, compaction, delegation, memory) has its
    // own five-minute budget and must not inherit the interactive stream header cap.
    non_stream_headers: NON_STREAM_TOTAL_TIMEOUT,
    stream_total: STREAM_TOTAL_TIMEOUT,
    non_stream_total: NON_STREAM_TOTAL_TIMEOUT,
};

/// OpenAI 兼容 chat/completions 客户端（Deepseek 特化：`reasoning_content`）。
pub struct OpenAICompatClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

// api_key 不进入 Debug 输出。
impl fmt::Debug for OpenAICompatClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAICompatClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl OpenAICompatClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("failed to build reqwest client");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> serde_json::Value {
        let deepseek_v4 = is_deepseek_v4_model(&req.model);
        // Without an explicit override, old sessions and cross-provider history can contain
        // assistant tool calls that predate reasoning persistence. DeepSeek rejects such
        // history in thinking mode; disable thinking for the complete request instead of
        // inventing chain-of-thought.
        let v4_thinking_enabled = deepseek_v4
            && req.thinking_enabled.unwrap_or_else(|| {
                !req.messages.iter().any(|message| {
                    message.role == "assistant"
                        && message
                            .tool_calls
                            .as_ref()
                            .is_some_and(|tool_calls| !tool_calls.is_empty())
                        && message
                            .reasoning_content
                            .as_deref()
                            .is_none_or(str::is_empty)
                })
            });
        // Persisted UI-only metadata (usage/error/harness flags) must not be
        // replayed. DeepSeek V4 is the exception for reasoning attached to any
        // assistant message: its thinking-mode protocol requires full assistant
        // reasoning replay on subsequent requests, including final text turns.
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                let is_assistant_tool_call = m.role == "assistant"
                    && m.tool_calls
                        .as_ref()
                        .is_some_and(|tool_calls| !tool_calls.is_empty());
                let mut value = json!({ "role": m.role });
                if let Some(content) = &m.content {
                    value["content"] = json!(content);
                } else if m.role == "assistant" {
                    // DeepSeek V4 thinking-mode tool history requires a
                    // non-null assistant content field. Generic OpenAI
                    // providers retain the prior `null` payload.
                    value["content"] = if deepseek_v4 && is_assistant_tool_call {
                        json!("")
                    } else {
                        serde_json::Value::Null
                    };
                }
                if let Some(tool_calls) = &m.tool_calls {
                    value["tool_calls"] = json!(tool_calls);
                }
                if v4_thinking_enabled && m.role == "assistant" {
                    if let Some(reasoning_content) = &m.reasoning_content {
                        value["reasoning_content"] = json!(reasoning_content);
                    }
                }
                if let Some(tool_call_id) = &m.tool_call_id {
                    value["tool_call_id"] = json!(tool_call_id);
                }
                if let Some(name) = &m.name {
                    value["name"] = json!(name);
                }
                value
            })
            .collect();
        let mut body = json!({
            "model": req.model,
            "messages": messages,
        });
        if deepseek_v4 {
            body["thinking"] = json!({
                "type": if v4_thinking_enabled { "enabled" } else { "disabled" }
            });
        }
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(tools) = &req.tools {
            body["tools"] = serde_json::to_value(tools).unwrap_or(json!([]));
            // DeepSeek V4 thinking mode rejects `tool_choice`. Generic
            // OpenAI-compatible providers retain the previous default-auto
            // behavior.
            if !deepseek_v4 {
                body["tool_choice"] = json!(req
                    .tool_choice
                    .clone()
                    .unwrap_or_else(|| "auto".to_string()));
            }
        }
        if stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({"include_usage": true});
        }
        body
    }

    /// Send through response headers with a bounded retry window. A successful
    /// response is handed to the caller immediately, so response-body/SSE
    /// failures are never replayed.
    async fn send_with_retry(
        &self,
        req: &ChatRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        self.send_with_retry_timeouts(req, stream, DEFAULT_REQUEST_TIMEOUTS)
            .await
    }

    async fn send_with_retry_timeouts(
        &self,
        req: &ChatRequest,
        stream: bool,
        timeouts: RequestTimeouts,
    ) -> Result<reqwest::Response, LlmError> {
        let url = self.chat_completions_url();
        let body = self.build_body(req, stream);
        let (response_headers_timeout, request_timeout) = if stream {
            (timeouts.stream_headers, timeouts.stream_total)
        } else {
            (timeouts.non_stream_headers, timeouts.non_stream_total)
        };

        for attempt in 1..=MAX_REQUEST_ATTEMPTS {
            let send = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .timeout(request_timeout)
                .send();

            match tokio::time::timeout(response_headers_timeout, send).await {
                Ok(Ok(response)) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    if is_retryable_status(status) && attempt < MAX_REQUEST_ATTEMPTS {
                        let delay = retry_delay(response.headers(), attempt);
                        tracing::warn!(
                            attempt,
                            max_attempts = MAX_REQUEST_ATTEMPTS,
                            status = status.as_u16(),
                            delay_ms = delay.as_millis() as u64,
                            "retrying LLM request after retryable HTTP status"
                        );
                        // Do not retain the failed response body during backoff.
                        drop(response);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Self::check_status(response).await;
                }
                Ok(Err(error)) => {
                    if is_retryable_transport(&error) && attempt < MAX_REQUEST_ATTEMPTS {
                        let delay = exponential_backoff(attempt);
                        tracing::warn!(
                            attempt,
                            max_attempts = MAX_REQUEST_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            "retrying LLM request after transport failure"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(LlmError::Transport(error));
                }
                Err(_) => {
                    if attempt < MAX_REQUEST_ATTEMPTS {
                        let delay = exponential_backoff(attempt);
                        tracing::warn!(
                            attempt,
                            max_attempts = MAX_REQUEST_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            "retrying LLM request after response-header timeout"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(LlmError::Request(format!(
                        "timed out waiting for response headers after {}s ({} attempts)",
                        response_headers_timeout.as_secs_f64(),
                        MAX_REQUEST_ATTEMPTS
                    )));
                }
            }
        }

        unreachable!("bounded request retry loop must return")
    }

    async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, LlmError> {
        let status = resp.status();
        if !status.is_success() {
            let message = error_body_snippet(resp).await;
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(resp)
    }
}

fn is_deepseek_v4_model(model: &str) -> bool {
    model.starts_with("deepseek-v4-")
}

#[derive(Debug)]
struct BoundedBody {
    bytes: Vec<u8>,
    truncated: bool,
    read_failed: bool,
}

async fn collect_bounded_body<S, E>(chunks: S, byte_limit: usize) -> BoundedBody
where
    S: Stream<Item = Result<Bytes, E>>,
{
    futures::pin_mut!(chunks);
    let mut bytes = Vec::with_capacity(byte_limit.min(1024));
    let mut truncated = false;
    let mut read_failed = false;

    while let Some(chunk) = chunks.next().await {
        match chunk {
            Ok(chunk) => {
                let remaining = byte_limit.saturating_sub(bytes.len());
                if remaining == 0 {
                    truncated = true;
                    break;
                }
                let take = remaining.min(chunk.len());
                bytes.extend_from_slice(&chunk[..take]);
                if take < chunk.len() || bytes.len() == byte_limit {
                    truncated = true;
                    break;
                }
            }
            Err(_) => {
                read_failed = true;
                break;
            }
        }
    }

    BoundedBody {
        bytes,
        truncated,
        read_failed,
    }
}

async fn error_body_snippet(response: reqwest::Response) -> String {
    let body = collect_bounded_body(response.bytes_stream(), ERROR_BODY_BYTE_LIMIT).await;
    if body.bytes.is_empty() && body.read_failed {
        return "<failed to read error body>".to_string();
    }

    let decoded = String::from_utf8_lossy(&body.bytes);
    let redacted = redact_credentials(&decoded);
    let suffix = if body.read_failed {
        " … [body read failed]"
    } else if body.truncated || redacted.chars().count() > ERROR_BODY_CHAR_LIMIT {
        " … [truncated]"
    } else {
        ""
    };
    truncate_snippet(&redacted, suffix)
}

fn truncate_snippet(value: &str, suffix: &str) -> String {
    if suffix.is_empty() && value.chars().count() <= ERROR_BODY_CHAR_LIMIT {
        return value.to_string();
    }

    let suffix_chars = suffix.chars().count().min(ERROR_BODY_CHAR_LIMIT);
    let keep = ERROR_BODY_CHAR_LIMIT.saturating_sub(suffix_chars);
    let mut snippet: String = value.chars().take(keep).collect();
    snippet.extend(suffix.chars().take(suffix_chars));
    snippet
}

fn redact_credentials(input: &str) -> String {
    let bytes = input.as_bytes();
    let lowercase: Vec<u8> = bytes.iter().map(u8::to_ascii_lowercase).collect();
    let mut ranges = Vec::<Range<usize>>::new();

    for key in [
        b"authorization".as_slice(),
        b"x-api-key".as_slice(),
        b"deepseek_api_key".as_slice(),
        b"openai_api_key".as_slice(),
        b"api_key".as_slice(),
        b"api-key".as_slice(),
        b"apikey".as_slice(),
    ] {
        collect_named_secret_ranges(bytes, &lowercase, key, &mut ranges);
    }
    collect_bearer_ranges(bytes, &lowercase, &mut ranges);
    collect_prefixed_token_ranges(bytes, &lowercase, b"sk-", &mut ranges);

    if ranges.is_empty() {
        return input.to_string();
    }
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged = Vec::<Range<usize>>::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    for range in merged {
        output.push_str(&input[cursor..range.start]);
        output.push_str(REDACTED_CREDENTIAL);
        cursor = range.end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn collect_named_secret_ranges(
    bytes: &[u8],
    lowercase: &[u8],
    key: &[u8],
    ranges: &mut Vec<Range<usize>>,
) {
    let mut search_from = 0;
    while let Some(relative) = find_bytes(&lowercase[search_from..], key) {
        let start = search_from + relative;
        let key_end = start + key.len();
        search_from = key_end;
        if (start > 0 && is_identifier_byte(lowercase[start - 1]))
            || (key_end < lowercase.len() && is_identifier_byte(lowercase[key_end]))
        {
            continue;
        }

        let mut cursor = key_end;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_whitespace() || matches!(bytes[cursor], b'\'' | b'"'))
        {
            cursor += 1;
        }
        if cursor >= bytes.len() || !matches!(bytes[cursor], b':' | b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        let quote = bytes
            .get(cursor)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            cursor += 1;
        }
        let value_start = cursor;
        let value_end = if let Some(quote) = quote {
            bytes[value_start..]
                .iter()
                .position(|byte| *byte == quote)
                .map(|relative| value_start + relative)
                .unwrap_or(bytes.len())
        } else if lowercase[value_start..].starts_with(b"bearer ") {
            let token_start = value_start + b"bearer ".len();
            token_start
                + bytes[token_start..]
                    .iter()
                    .position(|byte| {
                        byte.is_ascii_whitespace() || is_unquoted_secret_delimiter(*byte)
                    })
                    .unwrap_or(bytes.len() - token_start)
        } else {
            value_start
                + bytes[value_start..]
                    .iter()
                    .position(|byte| {
                        byte.is_ascii_whitespace() || is_unquoted_secret_delimiter(*byte)
                    })
                    .unwrap_or(bytes.len() - value_start)
        };
        if value_end > value_start {
            ranges.push(value_start..value_end);
        }
    }
}

fn collect_bearer_ranges(bytes: &[u8], lowercase: &[u8], ranges: &mut Vec<Range<usize>>) {
    let marker = b"bearer";
    let mut search_from = 0;
    while let Some(relative) = find_bytes(&lowercase[search_from..], marker) {
        let start = search_from + relative;
        let marker_end = start + marker.len();
        search_from = marker_end;
        if (start > 0 && is_identifier_byte(lowercase[start - 1]))
            || marker_end >= bytes.len()
            || !bytes[marker_end].is_ascii_whitespace()
        {
            continue;
        }

        let mut token_start = marker_end;
        while token_start < bytes.len() && bytes[token_start].is_ascii_whitespace() {
            token_start += 1;
        }
        let quote = bytes
            .get(token_start)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            token_start += 1;
        }
        let token_end = if let Some(quote) = quote {
            bytes[token_start..]
                .iter()
                .position(|byte| *byte == quote)
                .map(|relative| token_start + relative)
                .unwrap_or(bytes.len())
        } else {
            token_start
                + bytes[token_start..]
                    .iter()
                    .position(|byte| {
                        byte.is_ascii_whitespace() || is_unquoted_secret_delimiter(*byte)
                    })
                    .unwrap_or(bytes.len() - token_start)
        };
        if token_end > token_start {
            ranges.push(token_start..token_end);
        }
    }
}

fn collect_prefixed_token_ranges(
    bytes: &[u8],
    lowercase: &[u8],
    prefix: &[u8],
    ranges: &mut Vec<Range<usize>>,
) {
    let mut search_from = 0;
    while let Some(relative) = find_bytes(&lowercase[search_from..], prefix) {
        let start = search_from + relative;
        let prefix_end = start + prefix.len();
        search_from = prefix_end;
        if start > 0 && is_identifier_byte(lowercase[start - 1]) {
            continue;
        }
        let end = prefix_end
            + bytes[prefix_end..]
                .iter()
                .take_while(|byte| is_token_byte(**byte))
                .count();
        if end > prefix_end {
            ranges.push(start..end);
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty() && needle.len() <= haystack.len())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_unquoted_secret_delimiter(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n' | b',' | b'&' | b';' | b'}' | b']')
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
}

fn is_retryable_transport(error: &reqwest::Error) -> bool {
    !error.is_builder()
        && !error.is_redirect()
        && !error.is_status()
        && !error.is_body()
        && !error.is_decode()
        && (error.is_connect() || error.is_timeout() || error.is_request())
}

fn is_retryable_pre_event_stream_error(error: &LlmError) -> bool {
    match error {
        LlmError::Transport(error) => {
            error.is_body() || error.is_decode() || is_retryable_transport(error)
        }
        LlmError::Stream(message) => {
            message.starts_with("timed out waiting for first SSE body chunk")
        }
        _ => false,
    }
}

fn retry_delay(headers: &HeaderMap, completed_attempt: usize) -> Duration {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| bounded_retry_after(value, unix_now_seconds()))
        .unwrap_or_else(|| exponential_backoff(completed_attempt))
}

fn exponential_backoff(completed_attempt: usize) -> Duration {
    let exponent = completed_attempt.saturating_sub(1).min(10) as u32;
    let factor = 1_u32 << exponent;
    RETRY_BASE_DELAY
        .checked_mul(factor)
        .unwrap_or(MAX_RETRY_DELAY)
        .min(MAX_RETRY_DELAY)
}

/// Parse the standard delta-seconds form and the modern IMF-fixdate HTTP-date
/// form. Past dates retry immediately; all server-provided delays are capped.
fn bounded_retry_after(value: &str, now_unix_seconds: i64) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(MAX_RETRY_DELAY.as_secs())));
    }

    let target = parse_imf_fixdate_unix_seconds(value)?;
    let seconds = target.saturating_sub(now_unix_seconds).max(0) as u64;
    Some(Duration::from_secs(seconds.min(MAX_RETRY_DELAY.as_secs())))
}

fn unix_now_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// Parse RFC 7231's preferred `Sun, 06 Nov 1994 08:49:37 GMT` representation.
fn parse_imf_fixdate_unix_seconds(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 29
        || bytes[3] != b','
        || bytes[4] != b' '
        || bytes[7] != b' '
        || bytes[11] != b' '
        || bytes[16] != b' '
        || bytes[19] != b':'
        || bytes[22] != b':'
        || bytes[25] != b' '
        || &bytes[26..29] != b"GMT"
    {
        return None;
    }

    let day = parse_ascii_number(&bytes[5..7])?;
    let month = match &bytes[8..11] {
        b"Jan" => 1,
        b"Feb" => 2,
        b"Mar" => 3,
        b"Apr" => 4,
        b"May" => 5,
        b"Jun" => 6,
        b"Jul" => 7,
        b"Aug" => 8,
        b"Sep" => 9,
        b"Oct" => 10,
        b"Nov" => 11,
        b"Dec" => 12,
        _ => return None,
    };
    let year = i64::from(parse_ascii_number(&bytes[12..16])?);
    let hour = parse_ascii_number(&bytes[17..19])?;
    let minute = parse_ascii_number(&bytes[20..22])?;
    let second = parse_ascii_number(&bytes[23..25])?;
    if day == 0 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    days.checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))
}

fn parse_ascii_number(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(byte - b'0'))
    })
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

// Howard Hinnant's civil-date conversion, yielding days since Unix epoch.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[async_trait::async_trait]
impl LlmClient for OpenAICompatClient {
    async fn chat(&self, req: ChatRequest) -> Result<LlmResponse, LlmError> {
        let resp = self.send_with_retry(&req, false).await?;
        let body: serde_json::Value = resp.json().await?;

        let choice = &body["choices"][0];
        let message = &choice["message"];
        let text = message["content"].as_str().unwrap_or_default().to_string();
        let thinking = message["reasoning_content"].as_str().map(|s| s.to_string());
        Ok(LlmResponse {
            text,
            thinking,
            usage: parse_usage(&body["usage"]),
            finish_reason: choice["finish_reason"].as_str().map(|s| s.to_string()),
            tool_calls: parse_tool_calls(&message["tool_calls"]),
        })
    }

    fn chat_stream(&self, req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        Box::pin(async move {
            for attempt in 1..=MAX_STREAM_PREFLIGHT_ATTEMPTS {
                let resp = self.send_with_retry(&req, true).await?;
                let mut events = sse_event_stream(Box::pin(resp.bytes_stream()));
                match events.next().await {
                    Some(Ok(first)) => {
                        // The first provider event is the replay boundary. Put it
                        // back in front of the remaining stream without changing
                        // the public event order.
                        return Ok(Box::pin(
                            futures::stream::once(async move { Ok(first) }).chain(events),
                        ) as BoxedEventStream);
                    }
                    Some(Err(error))
                        if attempt < MAX_STREAM_PREFLIGHT_ATTEMPTS
                            && is_retryable_pre_event_stream_error(&error) =>
                    {
                        let delay = exponential_backoff(attempt);
                        tracing::warn!(
                            attempt,
                            max_attempts = MAX_STREAM_PREFLIGHT_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            error = %error,
                            "retrying LLM stream before its first event"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Some(Err(error)) => return Err(error),
                    None if attempt < MAX_STREAM_PREFLIGHT_ATTEMPTS => {
                        let delay = exponential_backoff(attempt);
                        tracing::warn!(
                            attempt,
                            max_attempts = MAX_STREAM_PREFLIGHT_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            "retrying LLM stream after empty response body"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    None => {
                        return Err(LlmError::Stream(
                            "SSE response ended before its first event".into(),
                        ));
                    }
                }
            }
            unreachable!("stream preflight loop always returns")
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}

fn parse_usage(v: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: v["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        output_tokens: v["completion_tokens"].as_u64().unwrap_or(0) as u32,
    }
}

/// 解析非流式响应 message.tool_calls 数组。
fn parse_tool_calls(v: &serde_json::Value) -> Vec<crate::types::ToolCall> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?.to_string();
            let fn_obj = &tc["function"];
            let name = fn_obj["name"].as_str()?.to_string();
            let arguments = fn_obj["arguments"].as_str().unwrap_or("").to_string();
            Some(crate::types::ToolCall {
                id,
                kind: "function".to_string(),
                function: crate::types::FunctionCall { name, arguments },
            })
        })
        .collect()
}

struct SseState {
    chunks: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    /// 原始字节缓冲；按 `\n`（ASCII，不会出现在多字节字符内部）切行，
    /// 避免 UTF-8 字符跨 chunk 被截断。
    buf: Vec<u8>,
    /// 已从完整行解析出、待 yield 的事件。
    pending: VecDeque<StreamEvent>,
    /// 是否已见 `[DONE]`。
    done: bool,
    /// Whether at least one response-body chunk has arrived. The first chunk
    /// gets a shorter phase-specific guard; every later chunk resets the idle guard.
    received_chunk: bool,
    raw_bytes: usize,
    events_seen: usize,
    chunks_seen: usize,
    first_chunk_timeout: Duration,
    idle_timeout: Duration,
}

/// 把字节流解析为 OpenAI SSE 事件流。
fn sse_event_stream(
    chunks: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
) -> BoxedEventStream {
    sse_event_stream_with_timeouts(chunks, STREAM_FIRST_CHUNK_TIMEOUT, STREAM_IDLE_TIMEOUT)
}

fn sse_event_stream_with_timeouts(
    chunks: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    first_chunk_timeout: Duration,
    idle_timeout: Duration,
) -> BoxedEventStream {
    let state = SseState {
        chunks,
        buf: Vec::new(),
        pending: VecDeque::new(),
        done: false,
        received_chunk: false,
        raw_bytes: 0,
        events_seen: 0,
        chunks_seen: 0,
        first_chunk_timeout,
        idle_timeout,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(ev) = state.pending.pop_front() {
                return Some((Ok(ev), state));
            }
            if state.done {
                return None;
            }

            // 取一整行（含 \n 前的内容）。
            if let Some(pos) = state.buf.iter().position(|&b| b == b'\n') {
                if pos.saturating_add(1) > MAX_SSE_LINE_BYTES {
                    return Some((
                        Err(LlmError::Stream(format!(
                            "SSE line exceeds {MAX_SSE_LINE_BYTES} byte cap"
                        ))),
                        state,
                    ));
                }
                let line: Vec<u8> = state.buf.drain(..=pos).collect();
                match parse_sse_line(&line) {
                    LineOutcome::Skip => continue,
                    LineOutcome::Done => {
                        state.done = true;
                        continue;
                    }
                    LineOutcome::Events(evs) => {
                        if evs.len() > MAX_SSE_EVENTS_PER_LINE
                            || state
                                .events_seen
                                .checked_add(evs.len())
                                .is_none_or(|total| total > MAX_SSE_EVENTS_PER_RESPONSE)
                        {
                            return Some((
                                Err(LlmError::Stream(
                                    "SSE event stream exceeds its configured boundary".into(),
                                )),
                                state,
                            ));
                        }
                        state.events_seen += evs.len();
                        state.pending.extend(evs);
                        continue;
                    }
                    LineOutcome::Error(e) => return Some((Err(e), state)),
                }
            }

            let phase_timeout = if state.received_chunk {
                state.idle_timeout
            } else {
                state.first_chunk_timeout
            };
            let next_chunk = tokio::time::timeout(phase_timeout, state.chunks.next()).await;
            match next_chunk {
                Err(_) => {
                    let phase = if state.received_chunk {
                        "SSE body idle"
                    } else {
                        "first SSE body chunk"
                    };
                    return Some((
                        Err(LlmError::Stream(format!(
                            "timed out waiting for {phase} after {}s",
                            phase_timeout.as_secs()
                        ))),
                        state,
                    ));
                }
                Ok(Some(Ok(bytes))) => {
                    state.received_chunk = true;
                    state.chunks_seen = state.chunks_seen.saturating_add(1);
                    if state.chunks_seen > MAX_SSE_CHUNKS_PER_RESPONSE {
                        return Some((
                            Err(LlmError::Stream(
                                "SSE response contains too many body chunks".into(),
                            )),
                            state,
                        ));
                    }
                    let Some(raw_bytes) =
                        checked_bounded_add(state.raw_bytes, bytes.len(), MAX_SSE_RESPONSE_BYTES)
                    else {
                        return Some((
                            Err(LlmError::Stream(format!(
                                "SSE response exceeds {MAX_SSE_RESPONSE_BYTES} byte cap"
                            ))),
                            state,
                        ));
                    };
                    if !chunk_lines_fit(state.buf.len(), &bytes, MAX_SSE_LINE_BYTES) {
                        return Some((
                            Err(LlmError::Stream(format!(
                                "SSE line exceeds {MAX_SSE_LINE_BYTES} byte cap"
                            ))),
                            state,
                        ));
                    }
                    state.raw_bytes = raw_bytes;
                    state.buf.extend_from_slice(&bytes);
                }
                Ok(Some(Err(e))) => return Some((Err(LlmError::Transport(e)), state)),
                // 上游 EOF：缓冲里无换行的残余按一行处理一次后终止。
                Ok(None) => {
                    if state.buf.is_empty() {
                        return None;
                    }
                    if state.buf.len() > MAX_SSE_LINE_BYTES {
                        return Some((
                            Err(LlmError::Stream(format!(
                                "SSE line exceeds {MAX_SSE_LINE_BYTES} byte cap"
                            ))),
                            state,
                        ));
                    }
                    let line = std::mem::take(&mut state.buf);
                    state.done = true;
                    match parse_sse_line(&line) {
                        LineOutcome::Events(evs) => {
                            if evs.len() > MAX_SSE_EVENTS_PER_LINE
                                || state
                                    .events_seen
                                    .checked_add(evs.len())
                                    .is_none_or(|total| total > MAX_SSE_EVENTS_PER_RESPONSE)
                            {
                                return Some((
                                    Err(LlmError::Stream(
                                        "SSE event stream exceeds its configured boundary".into(),
                                    )),
                                    state,
                                ));
                            }
                            state.events_seen += evs.len();
                            state.pending.extend(evs);
                        }
                        LineOutcome::Error(e) => return Some((Err(e), state)),
                        _ => {}
                    }
                }
            }
        }
    }))
}

fn checked_bounded_add(current: usize, additional: usize, cap: usize) -> Option<usize> {
    current
        .checked_add(additional)
        .filter(|total| *total <= cap)
}

/// Before copying a network chunk into the persistent buffer, verify every
/// partial/complete line against the cap. At this point `prefix_len` is the
/// unterminated suffix from prior chunks, so a huge newline-free chunk cannot
/// grow the buffer first and fail only afterwards.
fn chunk_lines_fit(prefix_len: usize, chunk: &[u8], cap: usize) -> bool {
    let mut current = prefix_len;
    for byte in chunk {
        let Some(next) = current.checked_add(1) else {
            return false;
        };
        current = next;
        if current > cap {
            return false;
        }
        if *byte == b'\n' {
            current = 0;
        }
    }
    true
}

enum LineOutcome {
    Skip,
    Done,
    Events(Vec<StreamEvent>),
    Error(LlmError),
}

/// 解析一行 SSE：`data: {json}` 或 `data: [DONE]`。
fn parse_sse_line(line: &[u8]) -> LineOutcome {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\n', '\r']);
    if line.is_empty() || line.starts_with(':') {
        return LineOutcome::Skip;
    }
    let Some(payload) = line.strip_prefix("data:") else {
        return LineOutcome::Skip;
    };
    let payload = payload.trim_start();
    if payload == "[DONE]" {
        return LineOutcome::Done;
    }

    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => return LineOutcome::Error(LlmError::Stream(format!("invalid SSE JSON: {e}"))),
    };

    let mut events = Vec::new();

    // usage 末包（choices 为空、usage 非 null）。
    if v["usage"].is_object() {
        events.push(StreamEvent::Usage(parse_usage(&v["usage"])));
    }

    if let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) {
        let delta = &choice["delta"];
        if let Some(t) = delta["reasoning_content"].as_str() {
            if !t.is_empty() {
                events.push(StreamEvent::Thinking(t.to_string()));
            }
        }
        if let Some(t) = delta["content"].as_str() {
            if !t.is_empty() {
                events.push(StreamEvent::Text(t.to_string()));
            }
        }
        // 流式 tool_calls：每个 delta 带一个 tool_calls 数组，按 index 累积。
        if let Some(arr) = delta["tool_calls"].as_array() {
            for tc in arr {
                let index = tc["index"].as_u64().unwrap_or(0) as u32;
                let id = tc["id"].as_str().map(|s| s.to_string());
                let name = tc["function"]["name"].as_str().map(|s| s.to_string());
                let arguments = tc["function"]["arguments"].as_str().map(|s| s.to_string());
                events.push(StreamEvent::ToolCallDelta(crate::types::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                }));
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            events.push(StreamEvent::Finish {
                reason: Some(reason.to_string()),
            });
        }
    }

    LineOutcome::Events(events)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use reqwest::StatusCode;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    use super::{
        bounded_retry_after, checked_bounded_add, chunk_lines_fit, collect_bounded_body,
        is_retryable_status, retry_delay, sse_event_stream_with_timeouts, OpenAICompatClient,
        RequestTimeouts, ERROR_BODY_BYTE_LIMIT, ERROR_BODY_CHAR_LIMIT, MAX_REQUEST_ATTEMPTS,
        MAX_RETRY_DELAY, MAX_SSE_LINE_BYTES, RETRY_BASE_DELAY,
    };
    use crate::{
        ChatMessage, ChatRequest, LlmClient, LlmError, StreamEvent, ToolCall, ToolDef, Usage,
    };

    enum TestServerAction {
        Respond(String),
        DelayRespond(Duration, String),
        Disconnect,
    }

    async fn spawn_test_server(
        actions: Vec<TestServerAction>,
    ) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&request_count);
        let handle = tokio::spawn(async move {
            for action in actions {
                let (mut socket, _) = listener.accept().await.expect("accept test request");
                server_count.fetch_add(1, Ordering::SeqCst);
                read_test_request(&mut socket).await;
                match action {
                    TestServerAction::Respond(response) => {
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .expect("write test response");
                        socket.shutdown().await.expect("close test response");
                    }
                    TestServerAction::DelayRespond(delay, response) => {
                        tokio::time::sleep(delay).await;
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .expect("write delayed test response");
                        socket.shutdown().await.expect("close delayed response");
                    }
                    TestServerAction::Disconnect => {
                        socket.shutdown().await.expect("disconnect test request");
                    }
                }
            }
        });
        (format!("http://{address}"), request_count, handle)
    }

    async fn read_test_request(socket: &mut TcpStream) {
        const MAX_TEST_REQUEST_BYTES: usize = 1024 * 1024;
        let mut request = Vec::new();
        let header_end = loop {
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let mut chunk = [0_u8; 4096];
            let read = socket.read(&mut chunk).await.expect("read test request");
            assert!(read > 0, "test client closed before request headers");
            request.extend_from_slice(&chunk[..read]);
            assert!(
                request.len() <= MAX_TEST_REQUEST_BYTES,
                "test request exceeded limit"
            );
        };

        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let expected_length = header_end + content_length;
        while request.len() < expected_length {
            let mut chunk = [0_u8; 4096];
            let read = socket.read(&mut chunk).await.expect("read test body");
            assert!(read > 0, "test client closed before request body");
            request.extend_from_slice(&chunk[..read]);
        }
    }

    fn test_response(
        status: &str,
        content_type: &str,
        extra_headers: &[(&str, &str)],
        body: &str,
    ) -> String {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in extra_headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        response
    }

    fn test_request() -> ChatRequest {
        ChatRequest::new("generic-model", vec![ChatMessage::user("hello")])
    }

    fn successful_chat_body() -> &'static str {
        r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#
    }

    #[test]
    fn generic_openai_persisted_ui_metadata_is_not_sent_upstream() {
        let client = OpenAICompatClient::new("https://example.com", "secret", "model");
        let mut message = ChatMessage::assistant("answer");
        message.reasoning_content = Some("private reasoning".into());
        message.usage = Some(Usage {
            input_tokens: 12,
            output_tokens: 34,
        });
        message.is_error = Some(true);
        let body = client.build_body(&ChatRequest::new("model", vec![message]), false);
        let sent = &body["messages"][0];
        assert_eq!(sent["content"], "answer");
        assert!(sent.get("reasoning_content").is_none());
        assert!(sent.get("usage").is_none());
        assert!(sent.get("is_error").is_none());
    }

    #[test]
    fn deepseek_v4_tool_history_uses_thinking_compatible_payload() {
        let client =
            OpenAICompatClient::new("https://api.deepseek.com", "secret", "deepseek-v4-flash");
        let mut message = ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "call-1",
            "read_file",
            r#"{"path":"paper.md"}"#.into(),
        )]);
        message.reasoning_content = Some("I need to inspect the paper.".into());
        message.usage = Some(Usage::default());

        let mut req = ChatRequest::new("deepseek-v4-flash", vec![message]);
        req.tools = Some(vec![ToolDef::function(
            "read_file",
            "Read a file",
            json!({"type": "object"}),
        )]);
        req.tool_choice = Some("auto".into());

        let body = client.build_body(&req, false);
        let sent = &body["messages"][0];
        assert_eq!(body["thinking"], json!({"type": "enabled"}));
        assert!(body.get("tool_choice").is_none());
        assert_eq!(sent["content"], "");
        assert_eq!(sent["reasoning_content"], "I need to inspect the paper.");
        assert!(sent.get("usage").is_none());
    }

    #[test]
    fn deepseek_v4_replays_reasoning_from_final_assistant_turns() {
        let client =
            OpenAICompatClient::new("https://api.deepseek.com", "secret", "deepseek-v4-flash");
        let mut final_answer = ChatMessage::assistant("The analysis is complete.");
        final_answer.reasoning_content = Some("I verified the evidence.".into());

        let body = client.build_body(
            &ChatRequest::new(
                "deepseek-v4-flash",
                vec![final_answer, ChatMessage::user("What should we test next?")],
            ),
            false,
        );

        assert_eq!(body["messages"][0]["content"], "The analysis is complete.");
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            "I verified the evidence."
        );
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn deepseek_v4_disables_thinking_for_legacy_tool_history_without_reasoning() {
        let client =
            OpenAICompatClient::new("https://api.deepseek.com", "secret", "deepseek-v4-flash");
        let legacy_tool_turn = ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "legacy-call",
            "read_file",
            r#"{"path":"old.md"}"#.into(),
        )]);
        let req = ChatRequest::new(
            "deepseek-v4-flash",
            vec![
                ChatMessage::user("old request"),
                legacy_tool_turn,
                ChatMessage::tool("legacy-call", "old result", Some("read_file".into())),
                ChatMessage::user("continue safely"),
            ],
        );

        let body = client.build_body(&req, false);
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert_eq!(body["messages"][1]["content"], "");
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn deepseek_v4_explicit_override_disables_thinking_and_reasoning_replay() {
        let client =
            OpenAICompatClient::new("https://api.deepseek.com", "secret", "deepseek-v4-flash");
        let mut previous_answer = ChatMessage::assistant("A previous final answer.");
        previous_answer.reasoning_content = Some("Private reasoning from that turn.".into());
        let mut req = ChatRequest::new(
            "deepseek-v4-flash",
            vec![
                previous_answer,
                ChatMessage::user("Try again without thinking."),
            ],
        );
        req.thinking_enabled = Some(false);

        let body = client.build_body(&req, false);

        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert_eq!(body["messages"][0]["content"], "A previous final answer.");
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn deepseek_v4_explicit_enable_overrides_legacy_history_fallback() {
        let client =
            OpenAICompatClient::new("https://api.deepseek.com", "secret", "deepseek-v4-flash");
        let legacy_tool_turn = ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "legacy-call",
            "read_file",
            r#"{"path":"old.md"}"#.into(),
        )]);
        let mut req = ChatRequest::new("deepseek-v4-flash", vec![legacy_tool_turn]);
        req.thinking_enabled = Some(true);

        let body = client.build_body(&req, false);

        assert_eq!(body["thinking"], json!({"type": "enabled"}));
    }

    #[tokio::test]
    async fn non_stream_request_uses_its_own_header_budget_without_retry() {
        let delayed = test_response("200 OK", "application/json", &[], successful_chat_body());
        let (base_url, request_count, server) =
            spawn_test_server(vec![TestServerAction::DelayRespond(
                Duration::from_millis(60),
                delayed,
            )])
            .await;
        let client = OpenAICompatClient::new(base_url, "secret", "generic-model");
        let timeouts = RequestTimeouts {
            stream_headers: Duration::from_millis(10),
            non_stream_headers: Duration::from_millis(200),
            stream_total: Duration::from_millis(500),
            non_stream_total: Duration::from_millis(500),
        };

        let response = client
            .send_with_retry_timeouts(&test_request(), false, timeouts)
            .await
            .expect("non-stream response inside its independent header budget");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.await.expect("test server task");
    }

    #[test]
    fn generic_openai_tool_payload_keeps_previous_shape() {
        let client = OpenAICompatClient::new("https://example.com", "secret", "gpt-compatible");
        let mut message = ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "call-1",
            "read_file",
            "{}".into(),
        )]);
        message.reasoning_content = Some("provider-specific metadata".into());

        let mut req = ChatRequest::new("gpt-compatible", vec![message]);
        req.thinking_enabled = Some(false);
        req.tools = Some(vec![ToolDef::function(
            "read_file",
            "Read a file",
            json!({"type": "object"}),
        )]);

        let body = client.build_body(&req, false);
        let sent = &body["messages"][0];
        assert!(body.get("thinking").is_none());
        assert_eq!(body["tool_choice"], "auto");
        assert!(sent["content"].is_null());
        assert!(sent.get("reasoning_content").is_none());
    }

    #[test]
    fn retry_policy_covers_only_transient_statuses_and_bounds_retry_after() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_status(
                StatusCode::from_u16(status).expect("valid status")
            ));
        }
        for status in [400, 401, 403, 404, 422, 501] {
            assert!(!is_retryable_status(
                StatusCode::from_u16(status).expect("valid status")
            ));
        }

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("999"));
        assert_eq!(retry_delay(&headers, 1), MAX_RETRY_DELAY);
        assert_eq!(
            bounded_retry_after("Sun, 06 Nov 1994 08:49:38 GMT", 784_111_777),
            Some(Duration::from_secs(1))
        );
        assert_eq!(bounded_retry_after("not-a-delay", 0), None);
        assert_eq!(retry_delay(&HeaderMap::new(), 1), RETRY_BASE_DELAY);
    }

    #[tokio::test]
    async fn oversized_error_body_collection_stops_at_byte_limit() {
        const CHUNK_SIZE: usize = 1024;
        let poll_count = Arc::new(AtomicUsize::new(0));
        let chunks = stream::unfold(Arc::clone(&poll_count), |poll_count| async move {
            poll_count.fetch_add(1, Ordering::SeqCst);
            Some((
                Ok::<Bytes, Infallible>(Bytes::from(vec![b'x'; CHUNK_SIZE])),
                poll_count,
            ))
        });

        let body = collect_bounded_body(chunks, ERROR_BODY_BYTE_LIMIT).await;

        assert_eq!(body.bytes.len(), ERROR_BODY_BYTE_LIMIT);
        assert!(body.truncated);
        assert!(!body.read_failed);
        assert_eq!(
            poll_count.load(Ordering::SeqCst),
            ERROR_BODY_BYTE_LIMIT / CHUNK_SIZE
        );
    }

    #[tokio::test]
    async fn non_stream_retries_transport_and_status_before_headers() {
        let retryable = test_response(
            "503 Service Unavailable",
            "application/json",
            &[("Retry-After", "0")],
            r#"{"error":"temporarily unavailable"}"#,
        );
        let success = test_response("200 OK", "application/json", &[], successful_chat_body());
        let (base_url, request_count, server) = spawn_test_server(vec![
            TestServerAction::Disconnect,
            TestServerAction::Respond(retryable),
            TestServerAction::Respond(success),
        ])
        .await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let response = client
            .chat(test_request())
            .await
            .expect("chat should recover");

        assert_eq!(response.text, "ok");
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retryable_status_stops_after_three_total_attempts() {
        let unavailable = || {
            TestServerAction::Respond(test_response(
                "503 Service Unavailable",
                "application/json",
                &[("Retry-After", "0")],
                r#"{"error":"still unavailable"}"#,
            ))
        };
        let (base_url, request_count, server) =
            spawn_test_server(vec![unavailable(), unavailable(), unavailable()]).await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let error = client
            .chat(test_request())
            .await
            .expect_err("third retryable response should be final");

        assert!(matches!(error, LlmError::Api { status: 503, .. }));
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), MAX_REQUEST_ATTEMPTS);
    }

    #[tokio::test]
    async fn non_stream_does_not_retry_non_transient_status_and_truncates_body() {
        let error_body = format!(
            "diagnostic: invalid request\n{}TAIL_MUST_NOT_APPEAR",
            "x".repeat(ERROR_BODY_BYTE_LIMIT * 4)
        );
        let bad_request = test_response("400 Bad Request", "text/plain", &[], &error_body);
        let (base_url, request_count, server) =
            spawn_test_server(vec![TestServerAction::Respond(bad_request)]).await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let error = client
            .chat(test_request())
            .await
            .expect_err("bad request must not retry");

        match error {
            LlmError::Api { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message.chars().count(), ERROR_BODY_CHAR_LIMIT);
                assert!(message.contains("diagnostic: invalid request"));
                assert!(message.ends_with("[truncated]"));
                assert!(!message.contains("TAIL_MUST_NOT_APPEAR"));
            }
            other => panic!("unexpected error: {other}"),
        }
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn error_snippet_redacts_common_bearer_and_api_key_shapes() {
        let bearer_secret = "bearer-secret-123456789";
        let named_secret = "named-secret-123456789";
        let camel_secret = "camel-secret-123456789";
        let header_secret = "header-secret-123456789";
        let env_secret = "env-secret-123456789";
        let prefixed_secret = "sk-proj-direct-secret-123456789";
        let error_body = format!(
            "invalid credentials; diagnostic code AUTH-42\n\
             Authorization: Bearer {bearer_secret}\n\
             api_key={named_secret}\n\
             apiKey: {camel_secret}\n\
             x-api-key: {header_secret}\n\
             DEEPSEEK_API_KEY={env_secret}\n\
             rejected token {prefixed_secret}"
        );
        let unauthorized = test_response("401 Unauthorized", "text/plain", &[], &error_body);
        let (base_url, request_count, server) =
            spawn_test_server(vec![TestServerAction::Respond(unauthorized)]).await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let error = client
            .chat(test_request())
            .await
            .expect_err("unauthorized response should be surfaced");
        let rendered = error.to_string();

        assert!(rendered.contains("HTTP 401"));
        assert!(rendered.contains("diagnostic code AUTH-42"));
        assert!(rendered.contains("[REDACTED]"));
        for secret in [
            bearer_secret,
            named_secret,
            camel_secret,
            header_secret,
            env_secret,
            prefixed_secret,
        ] {
            assert!(!rendered.contains(secret), "credential leaked: {secret}");
        }
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_retries_status_before_returning_sse() {
        let rate_limited = test_response(
            "429 Too Many Requests",
            "application/json",
            &[("Retry-After", "0")],
            r#"{"error":"slow down"}"#,
        );
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let success = test_response("200 OK", "text/event-stream", &[], sse_body);
        let (base_url, request_count, server) = spawn_test_server(vec![
            TestServerAction::Respond(rate_limited),
            TestServerAction::Respond(success),
        ])
        .await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let mut events = client
            .chat_stream(test_request())
            .await
            .expect("stream should recover");

        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Text(text))) if text == "ok"
        ));
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stream_retries_body_failure_before_first_event_once() {
        // A successful header followed by an abruptly truncated body reproduces
        // reqwest's pre-event body/decode failure. No model event has crossed
        // the caller boundary yet, so one replay is safe.
        let truncated = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Length: 128\r\n",
            "Content-Type: text/event-stream\r\n",
            "Connection: close\r\n\r\n"
        )
        .to_string();
        let recovered = test_response(
            "200 OK",
            "text/event-stream",
            &[],
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        );
        let (base_url, request_count, server) = spawn_test_server(vec![
            TestServerAction::Respond(truncated),
            TestServerAction::Respond(recovered),
        ])
        .await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");

        let mut events = client
            .chat_stream(test_request())
            .await
            .expect("pre-event body failure should recover");
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Text(text))) if text == "recovered"
        ));
        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Finish { reason: Some(reason) })) if reason == "stop"
        ));
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn established_stream_error_after_event_is_never_retried() {
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            "data: not-json\n\n"
        );
        let success = test_response("200 OK", "text/event-stream", &[], sse_body);
        let (base_url, request_count, server) =
            spawn_test_server(vec![TestServerAction::Respond(success)]).await;
        let client = OpenAICompatClient::new(base_url, "test-key", "generic-model");
        let mut events = client
            .chat_stream(test_request())
            .await
            .expect("response headers establish the stream");

        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Text(text))) if text == "partial"
        ));
        assert!(matches!(
            events.next().await,
            Some(Err(LlmError::Stream(message))) if message.contains("invalid SSE JSON")
        ));
        server.await.expect("test server task");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_times_out_waiting_for_first_body_chunk() {
        let chunks = stream::pending::<Result<Bytes, reqwest::Error>>();
        let mut events = sse_event_stream_with_timeouts(
            Box::pin(chunks),
            Duration::from_millis(20),
            Duration::from_secs(1),
        );

        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("test guard elapsed")
            .expect("timeout event");
        let error = event.expect_err("first body chunk should time out");
        assert!(
            matches!(error, LlmError::Stream(ref message) if message.contains("first SSE body chunk"))
        );
    }

    #[tokio::test]
    async fn stream_times_out_after_body_becomes_idle() {
        let first = stream::once(async {
            Ok::<_, reqwest::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            ))
        });
        let stalled = stream::pending::<Result<Bytes, reqwest::Error>>();
        let mut events = sse_event_stream_with_timeouts(
            Box::pin(first.chain(stalled)),
            Duration::from_secs(1),
            Duration::from_millis(20),
        );

        assert!(matches!(
            events.next().await,
            Some(Ok(StreamEvent::Text(text))) if text == "hello"
        ));
        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("test guard elapsed")
            .expect("timeout event");
        let error = event.expect_err("idle body should time out");
        assert!(
            matches!(error, LlmError::Stream(ref message) if message.contains("SSE body idle"))
        );
    }

    #[test]
    fn sse_byte_boundaries_accept_n_and_reject_n_plus_one_without_copying() {
        assert_eq!(checked_bounded_add(7, 3, 10), Some(10));
        assert_eq!(checked_bounded_add(7, 4, 10), None);
        assert!(chunk_lines_fit(3, b"1234567", 10));
        assert!(!chunk_lines_fit(3, b"12345678", 10));
        assert!(chunk_lines_fit(9, b"\nx", 10));
        assert!(!chunk_lines_fit(10, b"x\n", 10));
    }

    #[tokio::test]
    async fn stream_rejects_oversized_unterminated_and_single_data_lines() {
        for bytes in [vec![b'x'; MAX_SSE_LINE_BYTES + 1], {
            let mut line = b"data: ".to_vec();
            line.resize(MAX_SSE_LINE_BYTES + 1, b'x');
            line.push(b'\n');
            line
        }] {
            let chunks = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(bytes))]);
            let mut events = sse_event_stream_with_timeouts(
                Box::pin(chunks),
                Duration::from_secs(1),
                Duration::from_secs(1),
            );
            let error = events
                .next()
                .await
                .expect("boundary error")
                .expect_err("oversized SSE line must fail closed");
            assert!(
                matches!(error, LlmError::Stream(ref message) if message.contains("SSE line exceeds"))
            );
        }
    }
}
