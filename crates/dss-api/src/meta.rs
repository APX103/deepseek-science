//! 元信息端点：GET /api/skills、GET /api/templates、GET /api/templates/{id}。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::state::AppState;

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

#[derive(Serialize)]
pub struct SkillItem {
    pub name: String,
    pub description: String,
    pub source: String,
    pub enabled: bool,
}

/// `GET /api/skills`：返回 catalog 里的全部 skill。
pub async fn list_skills(State(state): State<AppState>) -> Json<Vec<SkillItem>> {
    let items = state
        .catalog
        .list()
        .into_iter()
        .map(|s| SkillItem {
            name: s.name.clone(),
            description: s.description.clone(),
            source: s.source.clone(),
            enabled: true,
        })
        .collect();
    Json(items)
}

/// 内置 templates（include_dir! 嵌入）。
static TEMPLATES: include_dir::Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../crates/dss-skills/templates");

#[derive(Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub documentclass: String,
    pub columns: u8,
}

/// `GET /api/templates`：列出可用模板。
pub async fn list_templates() -> Json<Vec<TemplateInfo>> {
    let mut out = Vec::new();
    for f in TEMPLATES.files() {
        if let Some(stem) = f.path().file_stem().and_then(|s| s.to_str()) {
            if let Some(ext) = f.path().extension().and_then(|s| s.to_str()) {
                if ext == "tex" {
                    out.push(TemplateInfo {
                        id: stem.to_string(),
                        name: stem.to_string(),
                        description: format!("{stem} LaTeX template"),
                        documentclass: "article".into(),
                        columns: 1,
                    });
                }
            }
        }
    }
    Json(out)
}

/// `GET /api/templates/{id}`：模板 .tex 纯文本。
pub async fn get_template(
    Path(id): Path<String>,
) -> Result<String, (StatusCode, Json<Value>)> {
    let f = TEMPLATES
        .files()
        .find(|f| f.path().file_stem().and_then(|s| s.to_str()) == Some(id.as_str()))
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "template not found"))?;
    std::str::from_utf8(f.contents())
        .map(|s| s.to_string())
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("template utf8: {e}")))
}
