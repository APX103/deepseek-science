//! dss-core: 类型定义、错误、配置（叶子 crate，无重依赖）。

pub mod error;
pub mod paths;
pub mod settings;
pub mod time;

pub use error::Error;
pub use settings::{LlmSettings, Settings};
