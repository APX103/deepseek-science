use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// ask_user 工具的一个候选项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// ask_user 工具挂起的提问。Runner 检测到后转 AwaitingUserResponse，
/// 并把此结构放进 complete.pending_ask 推给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAsk {
    pub question: String,
    /// 候选项（可空，表示自由文本回复）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<PendingAskOption>,
    /// 短标题（前端展示用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

/// 工具运行时共享态。
///
/// P2a 最小集：workspace（文件工具根 + bash cwd）+ pending_ask（ask_user 挂起）。
/// 后续阶段按 modules.md 扩展（frame/artifact_store/venv/api_keys/skill_catalog/...）。
#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub pending_ask: Arc<Mutex<Option<PendingAsk>>>,
    /// skill 目录（P5a：让 search_skills/list_skills/skill 工具可用）。
    pub skill_catalog: Arc<dss_skills::SkillCatalog>,
}

impl ToolContext {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            pending_ask: Arc::new(Mutex::new(None)),
            skill_catalog: Arc::new(dss_skills::SkillCatalog::new()),
        }
    }

    pub fn with_skill_catalog(mut self, catalog: dss_skills::SkillCatalog) -> Self {
        self.skill_catalog = Arc::new(catalog);
        self
    }

    /// 把相对 path 解析到 workspace 内的绝对路径，做路径穿越防护。
    /// 返回 Err 表示逃逸 workspace。
    pub fn resolve_in_workspace(&self, rel: &str) -> Result<PathBuf, crate::error::ToolError> {
        let trimmed = rel.trim();
        if trimmed.is_empty() {
            return Err(crate::error::ToolError::PathEscape("empty path".into()));
        }
        let p = std::path::Path::new(trimmed);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace.join(trimmed)
        };
        // canonicalize 要求路径存在；不存在时用 lexical 规范化判定。
        match abs.canonicalize() {
            Ok(c) => {
                if c.starts_with(&self.workspace) {
                    Ok(c)
                } else {
                    Err(crate::error::ToolError::PathEscape(rel.into()))
                }
            }
            Err(_) => {
                // 目标尚不存在（write 场景常见）：lexically normalize 后再判定前缀。
                let norm = lexically_resolve(&abs);
                if norm.starts_with(&self.workspace) {
                    Ok(norm)
                } else {
                    Err(crate::error::ToolError::PathEscape(rel.into()))
                }
            }
        }
    }
}

/// 词法规范化：消解 `..` / `.`，不访问文件系统。
fn lexically_resolve(p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}
