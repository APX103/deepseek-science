//! OpenAlex 学术论文检索工具：search_papers + fetch_paper。
//!
//! OpenAlex 是开放的学术图谱 API（https://api.openalex.org）。
//! 有 api_key 走 Bearer auth，没有也能用（OpenAlex 礼仪池，靠 User-Agent）。
//! 逻辑参考 Axiom（Python）的 openalex.py，用 Rust reqwest 实现。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

const OPENALEX_BASE: &str = "https://api.openalex.org";
const OPENALEX_KEY: &str = "OPENALEX_API_KEY";

fn http_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .user_agent("deepseek-science/0.1 (https://github.com/deepseek-science)")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ToolError::Other(format!("build http client: {e}")))
}

/// 构造请求头：有 api_key 用 Bearer，否则仅 UA（OpenAlex 礼仪池）。
fn auth_headers(ctx: &ToolContext) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(key) = ctx.api_keys.get(OPENALEX_KEY) {
        if !key.is_empty() {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
        }
    }
    headers
}

// ----------------- search_papers -----------------

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    year_from: Option<u32>,
    #[serde(default)]
    sort: Option<String>,
}

fn default_max_results() -> usize {
    10
}

pub struct SearchPapersTool;

#[async_trait]
impl Tool for SearchPapersTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_papers".into(),
            description: "检索学术论文（OpenAlex）。返回标题、作者、年份、DOI、被引数、摘要。\
                          用于文献综述和查找权威来源。\
                          sort 选项：relevance_score:desc（默认，相关度）/ \
                          cited_by_count:desc（最多引用）/ publication_date:desc（最新）。"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "检索词（如 \"multi-agent LLM orchestration\"）"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "最多返回数（默认 10，最大 25）",
                        "default": 10
                    },
                    "year_from": {
                        "type": "integer",
                        "description": "只返回此年份之后的论文（如 2022）"
                    },
                    "sort": {
                        "type": "string",
                        "enum": ["relevance_score:desc", "cited_by_count:desc", "publication_date:desc"],
                        "description": "排序（默认 relevance_score:desc）"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SearchArgs = parse_args(&args)?;
        let max = a.max_results.clamp(1, 25);
        let sort = a.sort.as_deref().unwrap_or("relevance_score:desc");
        let client = http_client()?;
        let mut req = client
            .get(format!("{OPENALEX_BASE}/works"))
            .query(&[
                ("search", a.query.as_str()),
                ("per-page", &max.to_string()[..]),
            ])
            .query(&[("sort", sort)])
            .headers(auth_headers(ctx));
        if let Some(yf) = a.year_from {
            let filter = format!("from_publication_date:{yf}-01-01");
            req = req.query(&[("filter", filter.as_str())]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ToolError::Other(format!("OpenAlex request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolOutput::ok(format!(
                "OpenAlex 搜索失败（HTTP {status}）: {}",
                text.chars().take(500).collect::<String>()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Other(format!("OpenAlex parse: {e}")))?;
        let results = data
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        if results.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "未找到与 '{}' 相关的论文。",
                a.query
            )));
        }
        let mut out = format!(
            "OpenAlex 搜索 '{}'（{} 条，sort={}）：\n\n",
            a.query,
            results.len(),
            sort
        );
        for (i, w) in results.iter().enumerate() {
            let title = w
                .get("display_name")
                .or_else(|| w.get("title"))
                .and_then(|t| t.as_str())
                .unwrap_or("(无标题)");
            let authors = format_authors(w.get("authorships"));
            let year = w
                .get("publication_year")
                .and_then(|y| y.as_u64())
                .map(|y| y.to_string())
                .unwrap_or_else(|| "?".into());
            let doi = w
                .get("doi")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .replace("https://doi.org/", "");
            let cited = w
                .get("cited_by_count")
                .and_then(|c| c.as_u64())
                .unwrap_or(0);
            let abstract_text = reconstruct_abstract(w.get("abstract_inverted_index"));
            out.push_str(&format!("{}. {}\n", i + 1, title));
            out.push_str(&format!("   作者: {}\n", authors));
            out.push_str(&format!(
                "   年份: {} | 被引: {} | DOI: {}\n",
                year, cited, doi
            ));
            if !abstract_text.is_empty() {
                out.push_str(&format!(
                    "   摘要: {}\n",
                    abstract_text.chars().take(300).collect::<String>()
                ));
            }
            out.push('\n');
        }
        Ok(ToolOutput::ok(out))
    }
}

// ----------------- fetch_paper -----------------

#[derive(Deserialize)]
struct FetchArgs {
    doi: String,
}

pub struct FetchPaperTool;

#[async_trait]
impl Tool for FetchPaperTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fetch_paper".into(),
            description: "按 DOI 取单篇论文的完整详情（OpenAlex）。返回标题、作者、期刊、\
                          被引数、类型、完整摘要。"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "doi": {
                        "type": "string",
                        "description": "DOI（如 10.1145/3292500.3330703，可带 https://doi.org/ 前缀）"
                    }
                },
                "required": ["doi"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: FetchArgs = parse_args(&args)?;
        let doi = a.doi.replace("https://doi.org/", "").trim().to_string();
        let client = http_client()?;
        let resp = client
            .get(format!("{OPENALEX_BASE}/works/doi:{doi}"))
            .headers(auth_headers(ctx))
            .send()
            .await
            .map_err(|e| ToolError::Other(format!("OpenAlex request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolOutput::ok(format!(
                "获取 DOI {doi} 失败（HTTP {status}）: {}",
                text.chars().take(500).collect::<String>()
            )));
        }
        let w: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Other(format!("OpenAlex parse: {e}")))?;
        let title = w
            .get("display_name")
            .and_then(|t| t.as_str())
            .unwrap_or("(无标题)");
        let authors = format_authors(w.get("authorships"));
        let abstract_text = reconstruct_abstract(w.get("abstract_inverted_index"));
        let venue = w
            .get("primary_location")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.get("display_name"))
            .and_then(|d| d.as_str())
            .unwrap_or("?");
        let year = w
            .get("publication_year")
            .and_then(|y| y.as_u64())
            .map(|y| y.to_string())
            .unwrap_or_else(|| "?".into());
        let cited = w
            .get("cited_by_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0);
        let kind = w.get("type").and_then(|t| t.as_str()).unwrap_or("?");
        let out = format!(
            "标题: {title}\n作者: {authors}\n期刊: {venue} ({year})\nDOI: {doi}\n被引: {cited}\n类型: {kind}\n摘要: {abstract_text}"
        );
        Ok(ToolOutput::ok(out))
    }
}

// ----------------- helpers -----------------

/// 格式化作者列表（取前 3，超过加 et al.）。
fn format_authors(authorships: Option<&Value>) -> String {
    let Some(arr) = authorships.and_then(|a| a.as_array()) else {
        return "unknown".into();
    };
    let names: Vec<String> = arr
        .iter()
        .take(3)
        .filter_map(|a| {
            a.get("author")
                .and_then(|au| au.get("display_name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .collect();
    let mut s = if names.is_empty() {
        "unknown".to_string()
    } else {
        names.join(", ")
    };
    if arr.len() > 3 {
        s.push_str(" et al.");
    }
    s
}

/// OpenAlex 摘要是 inverted index（{word: [positions]}），重建为文本。
fn reconstruct_abstract(inverted: Option<&Value>) -> String {
    let Some(obj) = inverted.and_then(|v| v.as_object()) else {
        return String::new();
    };
    // 收集 (position, word) 对，按 position 排序拼接。
    let mut positions: Vec<(usize, &str)> = Vec::new();
    for (word, idxs) in obj {
        if let Some(arr) = idxs.as_array() {
            for idx in arr {
                if let Some(i) = idx.as_u64() {
                    positions.push((i as usize, word.as_str()));
                }
            }
        }
    }
    positions.sort_by_key(|(i, _)| *i);
    positions
        .iter()
        .map(|(_, w)| *w)
        .collect::<Vec<_>>()
        .join(" ")
}
