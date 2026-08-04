use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::ToolContext;
use crate::error::ToolError;

/// 工具的对外说明（name + description + JSON Schema parameters）。
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// 参数 JSON Schema（一个对象，描述 properties/type/required）。
    pub parameters: Value,
}

/// 给 LLM 的工具定义（OpenAI tools 数组单项）。与 dss-llm::ToolDef 同构，
/// 这里单独定义以避免 dss-tools 反向依赖 dss-llm。
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl From<ToolSpec> for ToolDef {
    fn from(s: ToolSpec) -> Self {
        ToolDef {
            kind: "function".to_string(),
            function: ToolFunction {
                name: s.name,
                description: s.description,
                parameters: s.parameters,
            },
        }
    }
}

impl From<&ToolSpec> for ToolDef {
    fn from(s: &ToolSpec) -> Self {
        ToolDef {
            kind: "function".to_string(),
            function: ToolFunction {
                name: s.name.clone(),
                description: s.description.clone(),
                parameters: s.parameters.clone(),
            },
        }
    }
}

/// 工具执行结果。
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// 回给 LLM 的文本内容（OpenAI tool message 的 content）。
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// 工具 trait。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 返回工具说明（每次构造，避免 const 限制；JSON Schema 体量小，开销可忽略）。
    fn spec(&self) -> ToolSpec;

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError>;
}

/// 解析工具参数为指定类型；解析失败即 ToolError::InvalidArgs。
/// （trait 里的泛型方法不 dyn-compatible，故提为自由函数。）
pub fn parse_args<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone())
        .map_err(|e| ToolError::InvalidArgs(format!("invalid arguments: {e}")))
}
