//! dss-core: 类型定义、错误、配置（叶子 crate，无重依赖）。

pub mod error;
pub mod paths;
pub mod settings;
pub mod time;

pub use error::Error;
pub use settings::{
    default_mcp_servers, validate_max_iterations, A2aAgentConfig, LlmEnvOverrides, LlmProvider,
    LlmSettings, McpServerConfig, ResolvedLlmSettings, Settings, SkillSettings, ThinkingEffort,
    ThinkingSettings, DEFAULT_A2A_TIMEOUT_SECONDS, DEFAULT_AGENT_REGISTRY_NAME,
    DEFAULT_AGENT_REGISTRY_URL, DEFAULT_MAX_ITERATIONS, MAX_CONFIGURABLE_ITERATIONS,
    MIN_MAX_ITERATIONS,
};
