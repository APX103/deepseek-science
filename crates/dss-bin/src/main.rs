use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dss_llm::{ChatMessage, ChatRequest, LlmClient, LlmResponse, OpenAICompatClient, Usage};
use std::io::Read;
use tracing_subscriber::EnvFilter;

const LLM_ONCE_MAX_PROMPT_BYTES: usize = 256 * 1024;
const LLM_ONCE_PRIMARY_MAX_OUTPUT_TOKENS: u32 = 8_192;
const LLM_ONCE_FALLBACK_MAX_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Parser)]
#[command(name = "dss-backend", version, about = "Deepseek Science backend")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server
    Serve {
        /// Port to listen on (overrides config; default 17896)
        #[arg(long)]
        port: Option<u16>,
        /// Owning desktop process. Packaged launches use this to avoid an orphaned backend.
        #[arg(long, hide = true)]
        parent_pid: Option<u32>,
    },
    /// Internal bounded LLM bridge for isolated interoperability tests.
    #[command(hide = true)]
    LlmOnce,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { port, parent_pid } => {
            let mut settings = dss_core::Settings::load().context("failed to load settings")?;
            if let Some(port) = port {
                settings.server.port = port;
            }

            init_tracing(settings.log_level.as_deref());

            dss_core::paths::ensure_data_dir(&settings.data_dir, settings.data_dir_is_default)
                .context("failed to ensure data dir")?;

            let state = dss_api::build_state(settings)
                .await
                .context("failed to initialize application database")?;

            let addr = format!(
                "{}:{}",
                state.settings.server.host, state.settings.server.port
            );
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("failed to bind {addr}"))?;
            let llm_runtime = state.llm_snapshot().await;

            tracing::info!(
                addr = %addr,
                data_dir = %state.settings.data_dir.display(),
                llm_configured = llm_runtime.is_configured(),
                model = %llm_runtime.settings().model,
                version = dss_api::VERSION,
                "dss-backend listening"
            );
            drop(llm_runtime);

            serve_until_shutdown(listener, state, parent_pid).await?;
            tracing::info!("dss-backend stopped");
            Ok(())
        }
        Commands::LlmOnce => run_llm_once().await,
    }
}

/// Execute one deliberately tool-free model request for the isolated A2A interoperability
/// sidecar. The provider credential remains inside the existing Rust settings/client boundary:
/// the prompt arrives on stdin and stdout contains only the final text plus non-secret audit
/// metadata. In particular, reasoning content and the resolved settings object are never
/// serialized across the process boundary.
async fn run_llm_once() -> Result<()> {
    let prompt = read_bounded_prompt(std::io::stdin().lock())?;
    let settings = dss_core::Settings::load().context("failed to load settings")?;
    let api_key = settings
        .llm
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("LLM is not configured")?;
    let model = settings.llm.model.clone();
    let client = OpenAICompatClient::new(&settings.llm.base_url, api_key, &model);
    let result = execute_llm_once(&client, &model, &prompt).await?;
    println!(
        "{}",
        serde_json::to_string(&llm_once_output_json(&model, result))?
    );
    Ok(())
}

#[derive(Debug)]
struct LlmOnceResult {
    text: String,
    usage: Usage,
    finish_reason: Option<String>,
    llm_call_count: u8,
}

async fn execute_llm_once(
    client: &(impl LlmClient + ?Sized),
    model: &str,
    prompt: &str,
) -> Result<LlmOnceResult> {
    let primary = client
        .chat(llm_once_request(
            model,
            prompt,
            None,
            LLM_ONCE_PRIMARY_MAX_OUTPUT_TOKENS,
        ))
        .await
        .context("primary LLM request failed")?;
    ensure_tool_free(&primary)?;

    if !needs_llm_once_fallback(&primary) {
        return Ok(LlmOnceResult {
            text: primary.text,
            usage: primary.usage,
            finish_reason: primary.finish_reason,
            llm_call_count: 1,
        });
    }

    // DeepSeek V4 can spend the entire output allowance on reasoning and return
    // no final content. Retry exactly once with thinking explicitly disabled.
    // Generic OpenAI-compatible providers ignore this override at serialization.
    let fallback = client
        .chat(llm_once_request(
            model,
            prompt,
            Some(false),
            LLM_ONCE_FALLBACK_MAX_OUTPUT_TOKENS,
        ))
        .await
        .context("thinking-disabled fallback LLM request failed")?;
    ensure_tool_free(&fallback)?;

    let usage = Usage {
        input_tokens: primary
            .usage
            .input_tokens
            .saturating_add(fallback.usage.input_tokens),
        output_tokens: primary
            .usage
            .output_tokens
            .saturating_add(fallback.usage.output_tokens),
    };
    if fallback.text.trim().is_empty() {
        anyhow::bail!("LLM returned no final text after one bounded fallback");
    }
    if fallback.finish_reason.as_deref() == Some("length") {
        anyhow::bail!("LLM fallback exhausted its output limit before a complete final answer");
    }

    Ok(LlmOnceResult {
        text: fallback.text,
        usage,
        finish_reason: fallback.finish_reason,
        llm_call_count: 2,
    })
}

fn llm_once_request(
    model: &str,
    prompt: &str,
    thinking_enabled: Option<bool>,
    max_tokens: u32,
) -> ChatRequest {
    let mut request = ChatRequest::new(
        model,
        vec![
            ChatMessage::system(
                "You are an independent scientific specialist reached through an A2A Task. \
                 Solve the supplied bounded problem carefully. Return a concise Markdown report \
                 with assumptions, calculation, uncertainty, and limitations. Do not claim to \
                 have used tools or sources that were not supplied.",
            ),
            ChatMessage::user(prompt),
        ],
    );
    request.max_tokens = Some(max_tokens);
    request.temperature = Some(0.1);
    request.thinking_enabled = thinking_enabled;
    // This bridge intentionally exposes no local or remote tools.
    request.tools = None;
    request.tool_choice = None;
    request
}

fn ensure_tool_free(response: &LlmResponse) -> Result<()> {
    if !response.tool_calls.is_empty() {
        anyhow::bail!("LLM unexpectedly returned tool calls in tool-free mode");
    }
    Ok(())
}

fn needs_llm_once_fallback(response: &LlmResponse) -> bool {
    response.text.trim().is_empty() || response.finish_reason.as_deref() == Some("length")
}

fn llm_once_output_json(model: &str, result: LlmOnceResult) -> serde_json::Value {
    serde_json::json!({
        "text": result.text,
        "model": model,
        "usage": {
            "input_tokens": result.usage.input_tokens,
            "output_tokens": result.usage.output_tokens,
        },
        "finish_reason": result.finish_reason,
        "llm_call_count": result.llm_call_count,
    })
}

fn read_bounded_prompt(mut reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((LLM_ONCE_MAX_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("failed to read prompt from stdin")?;
    if bytes.len() > LLM_ONCE_MAX_PROMPT_BYTES {
        anyhow::bail!("prompt exceeds {LLM_ONCE_MAX_PROMPT_BYTES} bytes");
    }
    let prompt = String::from_utf8(bytes).context("prompt must be valid UTF-8")?;
    if prompt.trim().is_empty() {
        anyhow::bail!("prompt must not be empty");
    }
    Ok(prompt)
}

async fn serve_until_shutdown(
    listener: tokio::net::TcpListener,
    state: dss_api::state::AppState,
    parent_pid: Option<u32>,
) -> std::io::Result<()> {
    let server = dss_api::serve(listener, state);

    #[cfg(unix)]
    if let Some(parent_pid) = parent_pid {
        tokio::select! {
            result = server => return result,
            () = wait_for_parent_exit(parent_pid) => {
                tracing::warn!(parent_pid, "desktop parent exited; stopping packaged backend");
                return Ok(());
            }
        }
    }

    #[cfg(not(unix))]
    let _ = parent_pid;

    server.await
}

#[cfg(unix)]
async fn wait_for_parent_exit(expected_parent_pid: u32) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        interval.tick().await;
        let current_parent_pid = unsafe { libc::getppid() };
        if current_parent_pid <= 0 || current_parent_pid as u32 != expected_parent_pid {
            return;
        }
    }
}

/// 日志级别优先级：`DSS_LOG` > `RUST_LOG` > 配置文件 `log_level` > `info`。
fn init_tracing(config_level: Option<&str>) {
    let filter = std::env::var("DSS_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .or_else(|| config_level.map(str::to_owned))
        .unwrap_or_else(|| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .init();
}

#[cfg(test)]
mod tests {
    use super::{
        execute_llm_once, llm_once_output_json, read_bounded_prompt, Cli,
        LLM_ONCE_FALLBACK_MAX_OUTPUT_TOKENS, LLM_ONCE_MAX_PROMPT_BYTES,
        LLM_ONCE_PRIMARY_MAX_OUTPUT_TOKENS,
    };
    use clap::{CommandFactory, Parser};
    use dss_llm::{
        BoxedEventStream, ChatRequest, LlmClient, LlmError, LlmResponse, ToolCall, Usage,
    };
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct ScriptedLlm {
        responses: Mutex<VecDeque<LlmResponse>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptedLlm {
        async fn chat(&self, req: ChatRequest) -> Result<LlmResponse, LlmError> {
            self.requests.lock().expect("requests lock").push(req);
            Ok(self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("scripted response"))
        }

        fn chat_stream(
            &self,
            _req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<BoxedEventStream, LlmError>> + Send + '_>> {
            Box::pin(async { panic!("llm-once must never stream") })
        }

        fn model(&self) -> &str {
            "scripted-model"
        }
    }

    fn response(
        text: &str,
        finish_reason: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> LlmResponse {
        LlmResponse {
            text: text.into(),
            thinking: Some("must remain private".into()),
            usage: Usage {
                input_tokens,
                output_tokens,
            },
            finish_reason: Some(finish_reason.into()),
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn packaged_parent_pid_is_parsed_without_changing_cli_default() {
        let with_parent = Cli::try_parse_from([
            "dss-backend",
            "serve",
            "--port",
            "17901",
            "--parent-pid",
            "4242",
        ]);
        assert!(with_parent.is_ok());

        let direct_cli = Cli::try_parse_from(["dss-backend", "serve"]);
        assert!(direct_cli.is_ok());
    }

    #[test]
    fn hidden_llm_bridge_is_parseable_but_absent_from_public_help() {
        assert!(Cli::try_parse_from(["dss-backend", "llm-once"]).is_ok());
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("llm-once"));
    }

    #[test]
    fn llm_bridge_prompt_is_nonempty_utf8_and_bounded() {
        assert_eq!(
            read_bounded_prompt(std::io::Cursor::new("scientific prompt")).unwrap(),
            "scientific prompt"
        );
        assert!(read_bounded_prompt(std::io::Cursor::new(" \n ")).is_err());
        assert!(read_bounded_prompt(std::io::Cursor::new(vec![0xff])).is_err());
        assert!(read_bounded_prompt(std::io::Cursor::new(vec![
            b'x';
            LLM_ONCE_MAX_PROMPT_BYTES + 1
        ]))
        .is_err());
    }

    #[tokio::test]
    async fn llm_once_keeps_default_thinking_when_primary_answer_is_complete() {
        let llm = ScriptedLlm::new(vec![response("complete", "stop", 11, 13)]);

        let result = execute_llm_once(&llm, "deepseek-v4-flash", "question")
            .await
            .expect("complete primary response");
        let requests = llm.requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].thinking_enabled, None);
        assert_eq!(
            requests[0].max_tokens,
            Some(LLM_ONCE_PRIMARY_MAX_OUTPUT_TOKENS)
        );
        assert!(requests[0].tools.is_none());
        assert!(requests[0].tool_choice.is_none());
        assert_eq!(result.text, "complete");
        assert_eq!(result.llm_call_count, 1);
        assert_eq!(result.usage.input_tokens, 11);
        assert_eq!(result.usage.output_tokens, 13);
    }

    #[tokio::test]
    async fn llm_once_retries_length_once_without_thinking_and_aggregates_usage() {
        let llm = ScriptedLlm::new(vec![
            response("partial", "length", 10, 8_192),
            response("complete fallback", "stop", 12, 34),
        ]);

        let result = execute_llm_once(&llm, "deepseek-v4-flash", "question")
            .await
            .expect("thinking-disabled fallback");
        let requests = llm.requests();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].thinking_enabled, None);
        assert_eq!(requests[1].thinking_enabled, Some(false));
        assert_eq!(
            requests[1].max_tokens,
            Some(LLM_ONCE_FALLBACK_MAX_OUTPUT_TOKENS)
        );
        assert!(requests[1].tools.is_none());
        assert!(requests[1].tool_choice.is_none());
        assert_eq!(result.text, "complete fallback");
        assert_eq!(result.llm_call_count, 2);
        assert_eq!(result.usage.input_tokens, 22);
        assert_eq!(result.usage.output_tokens, 8_226);

        let output = llm_once_output_json("deepseek-v4-flash", result);
        let keys = output
            .as_object()
            .expect("object output")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            ["finish_reason", "llm_call_count", "model", "text", "usage"]
                .into_iter()
                .collect()
        );
        assert!(output.get("thinking").is_none());
    }

    #[tokio::test]
    async fn llm_once_fails_after_exactly_one_fallback_when_both_answers_are_empty() {
        let llm = ScriptedLlm::new(vec![
            response("", "length", 1, 8_192),
            response("  ", "stop", 2, 4),
        ]);

        let error = execute_llm_once(&llm, "deepseek-v4-flash", "question")
            .await
            .expect_err("two empty answers must fail");

        assert!(error.to_string().contains("no final text"));
        assert_eq!(llm.requests().len(), 2);
    }

    #[tokio::test]
    async fn llm_once_rejects_tool_calls_without_leaking_them() {
        let mut unexpected = response("", "tool_calls", 1, 1);
        unexpected.tool_calls = vec![ToolCall::function("call-1", "unexpected_tool", "{}".into())];
        let llm = ScriptedLlm::new(vec![unexpected]);

        let error = execute_llm_once(&llm, "deepseek-v4-flash", "question")
            .await
            .expect_err("tool calls are forbidden");

        assert_eq!(
            error.to_string(),
            "LLM unexpectedly returned tool calls in tool-free mode"
        );
        assert_eq!(llm.requests().len(), 1);
    }
}
