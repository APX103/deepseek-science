use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { port } => {
            let mut settings = dss_core::Settings::load().context("failed to load settings")?;
            if let Some(port) = port {
                settings.server.port = port;
            }

            init_tracing(settings.log_level.as_deref());

            dss_core::paths::ensure_data_dir(&settings.data_dir, settings.data_dir_is_default)
                .context("failed to ensure data dir")?;

            let state = dss_api::build_state(settings).await;

            let addr = format!("{}:{}", state.settings.server.host, state.settings.server.port);
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("failed to bind {addr}"))?;

            tracing::info!(
                addr = %addr,
                data_dir = %state.settings.data_dir.display(),
                llm_configured = state.llm.is_some(),
                model = %state.settings.llm.model,
                version = dss_api::VERSION,
                "dss-backend listening"
            );

            dss_api::serve(listener, state).await?;
            tracing::info!("dss-backend stopped");
            Ok(())
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
