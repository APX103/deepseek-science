//! OpenAlex 学术论文检索工具：search_papers + fetch_paper。
//!
//! OpenAlex 是开放的学术图谱 API（https://api.openalex.org）。
//! API key 使用官方的 `api_key` 查询参数；匿名访问只适合小额试用，稳定使用需要 key。
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
const DEFAULT_SORT: &str = "relevance_score:desc";
const ALLOWED_SORTS: [&str; 3] = [DEFAULT_SORT, "cited_by_count:desc", "publication_date:desc"];

fn http_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .user_agent("deepseek-science/0.1 (https://github.com/deepseek-science)")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ToolError::Other(format!("build http client: {e}")))
}

fn endpoint_url(base_url: &str, segments: &[&str]) -> Result<reqwest::Url, ToolError> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|_| ToolError::Other("invalid OpenAlex base URL".into()))?;
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| ToolError::Other("invalid OpenAlex base URL".into()))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(url)
}

fn append_api_key(url: &mut reqwest::Url, ctx: &ToolContext) {
    if let Some(key) = ctx
        .api_keys
        .get(OPENALEX_KEY)
        .map(String::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        url.query_pairs_mut().append_pair("api_key", key);
    }
}

fn request_error(error: &reqwest::Error) -> ToolError {
    // reqwest errors can contain the request URL. Since the documented auth mechanism places the
    // key in that URL, expose only a coarse failure category to logs and tool results.
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection"
    } else {
        "transport"
    };
    ToolError::Other(format!("OpenAlex request failed ({category})"))
}

fn parse_error() -> ToolError {
    ToolError::Other("OpenAlex returned an invalid JSON response".into())
}

fn http_error(operation: &str, status: reqwest::StatusCode) -> ToolOutput {
    // Do not reflect the response body: upstream error pages may echo request metadata.
    ToolOutput::err(format!(
        "OpenAlex {operation}失败（HTTP {}）",
        status.as_u16()
    ))
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

struct ValidatedSearchArgs {
    query: String,
    max_results: usize,
    year_from: Option<u32>,
    sort: String,
}

impl SearchArgs {
    fn validate(self) -> Result<ValidatedSearchArgs, ToolError> {
        let query = self.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidArgs(
                "query must contain at least one non-whitespace character".into(),
            ));
        }

        let sort = self.sort.as_deref().unwrap_or(DEFAULT_SORT);
        if !ALLOWED_SORTS.contains(&sort) {
            return Err(ToolError::InvalidArgs(format!(
                "unsupported sort `{sort}`; expected one of {}",
                ALLOWED_SORTS.join(", ")
            )));
        }

        Ok(ValidatedSearchArgs {
            query: query.to_owned(),
            max_results: self.max_results.clamp(1, 25),
            year_from: self.year_from,
            sort: sort.to_owned(),
        })
    }
}

fn default_max_results() -> usize {
    10
}

fn build_search_request(
    client: &reqwest::Client,
    base_url: &str,
    ctx: &ToolContext,
    args: &ValidatedSearchArgs,
) -> Result<reqwest::Request, ToolError> {
    let mut url = endpoint_url(base_url, &["works"])?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("search", &args.query);
        query.append_pair("per_page", &args.max_results.to_string());
        query.append_pair("sort", &args.sort);
        if let Some(year_from) = args.year_from {
            query.append_pair(
                "filter",
                &format!("from_publication_date:{year_from}-01-01"),
            );
        }
    }
    append_api_key(&mut url, ctx);
    client
        .get(url)
        .build()
        .map_err(|_| ToolError::Other("failed to build OpenAlex request".into()))
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
                        "default": 10,
                        "minimum": 1,
                        "maximum": 25
                    },
                    "year_from": {
                        "type": "integer",
                        "description": "只返回此年份起（含该年）的论文（如 2022）"
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
        let a = parse_args::<SearchArgs>(&args)?.validate()?;
        let client = http_client()?;
        let request = build_search_request(&client, OPENALEX_BASE, ctx, &a)?;
        let resp = client
            .execute(request)
            .await
            .map_err(|error| request_error(&error))?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(http_error("搜索", status));
        }
        let data: Value = resp.json().await.map_err(|_| parse_error())?;
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
            a.sort
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

impl FetchArgs {
    fn validate(self) -> Result<String, ToolError> {
        let mut doi = self.doi.trim();
        for prefix in [
            "https://doi.org/",
            "http://doi.org/",
            "https://dx.doi.org/",
            "http://dx.doi.org/",
            "doi:",
        ] {
            if doi
                .get(..prefix.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
            {
                doi = doi[prefix.len()..].trim();
                break;
            }
        }
        if doi.is_empty() {
            return Err(ToolError::InvalidArgs(
                "doi must contain at least one non-whitespace character".into(),
            ));
        }
        Ok(doi.to_owned())
    }
}

fn build_fetch_request(
    client: &reqwest::Client,
    base_url: &str,
    ctx: &ToolContext,
    doi: &str,
) -> Result<reqwest::Request, ToolError> {
    let resource = format!("doi:{doi}");
    let mut url = endpoint_url(base_url, &["works", &resource])?;
    append_api_key(&mut url, ctx);
    client
        .get(url)
        .build()
        .map_err(|_| ToolError::Other("failed to build OpenAlex request".into()))
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
        let doi = parse_args::<FetchArgs>(&args)?.validate()?;
        let client = http_client()?;
        let request = build_fetch_request(&client, OPENALEX_BASE, ctx, &doi)?;
        let resp = client
            .execute(request)
            .await
            .map_err(|error| request_error(&error))?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(http_error("获取论文", status));
        }
        let w: Value = resp.json().await.map_err(|_| parse_error())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn context_with_key(key: &str) -> ToolContext {
        ToolContext::new(std::env::temp_dir())
            .with_api_keys(HashMap::from([(OPENALEX_KEY.to_owned(), key.to_owned())]))
    }

    fn query_parameters(request: &reqwest::Request) -> HashMap<String, String> {
        request.url().query_pairs().into_owned().collect()
    }

    #[test]
    fn search_validation_trims_query_defaults_sort_and_clamps_product_limit() {
        let default = parse_args::<SearchArgs>(&json!({ "query": "  graph learning  " }))
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(default.query, "graph learning");
        assert_eq!(default.max_results, 10);
        assert_eq!(default.sort, DEFAULT_SORT);

        for (provided, expected) in [(0, 1), (1, 1), (25, 25), (26, 25), (usize::MAX, 25)] {
            let validated = SearchArgs {
                query: "q".into(),
                max_results: provided,
                year_from: None,
                sort: None,
            }
            .validate()
            .unwrap();
            assert_eq!(validated.max_results, expected, "provided {provided}");
        }
    }

    #[test]
    fn search_validation_rejects_empty_query_invalid_sort_and_non_usize_limits() {
        for query in ["", "   ", "\n\t", "\u{2003}"] {
            let result = SearchArgs {
                query: query.into(),
                max_results: 10,
                year_from: None,
                sort: None,
            }
            .validate();
            assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
        }

        let invalid_sort = SearchArgs {
            query: "q".into(),
            max_results: 10,
            year_from: None,
            sort: Some("display_name:asc".into()),
        }
        .validate();
        assert!(matches!(invalid_sort, Err(ToolError::InvalidArgs(_))));

        for value in [json!(-1), json!(1.5)] {
            let parsed = parse_args::<SearchArgs>(&json!({
                "query": "q",
                "max_results": value,
            }));
            assert!(matches!(parsed, Err(ToolError::InvalidArgs(_))));
        }
    }

    #[test]
    fn search_request_uses_documented_parameters_and_never_authorization_header() {
        let ctx = context_with_key("dummy-openalex-key");
        let args = SearchArgs {
            query: "量子 agent & safety".into(),
            max_results: 99,
            year_from: Some(2022),
            sort: Some("cited_by_count:desc".into()),
        }
        .validate()
        .unwrap();
        let client = http_client().unwrap();
        let request = build_search_request(&client, "https://example.test", &ctx, &args).unwrap();
        let query = query_parameters(&request);

        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(request.url().path(), "/works");
        assert_eq!(
            query.get("search").map(String::as_str),
            Some("量子 agent & safety")
        );
        assert_eq!(query.get("per_page").map(String::as_str), Some("25"));
        assert!(!query.contains_key("per-page"));
        assert_eq!(
            query.get("sort").map(String::as_str),
            Some("cited_by_count:desc")
        );
        assert_eq!(
            query.get("filter").map(String::as_str),
            Some("from_publication_date:2022-01-01")
        );
        assert_eq!(
            query.get("api_key").map(String::as_str),
            Some("dummy-openalex-key")
        );
        assert!(request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());
    }

    #[test]
    fn blank_key_is_omitted_from_search_request() {
        let ctx = context_with_key("   ");
        let args = SearchArgs {
            query: "test".into(),
            max_results: 1,
            year_from: None,
            sort: None,
        }
        .validate()
        .unwrap();
        let client = http_client().unwrap();
        let request = build_search_request(&client, "https://example.test", &ctx, &args).unwrap();

        assert!(!query_parameters(&request).contains_key("api_key"));
    }

    #[test]
    fn fetch_validation_normalizes_supported_doi_forms_and_rejects_empty_values() {
        for (provided, expected) in [
            (" 10.1000/AbC ", "10.1000/AbC"),
            ("https://doi.org/10.1000/abc", "10.1000/abc"),
            ("HTTPS://DOI.ORG/10.1000/abc", "10.1000/abc"),
            ("http://dx.doi.org/10.1000/abc", "10.1000/abc"),
            ("doi:10.1000/abc", "10.1000/abc"),
        ] {
            assert_eq!(
                FetchArgs {
                    doi: provided.into()
                }
                .validate()
                .unwrap(),
                expected
            );
        }

        for provided in ["", "   ", "https://doi.org/  ", "DOI:\t"] {
            let result = FetchArgs {
                doi: provided.into(),
            }
            .validate();
            assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
        }
    }

    #[test]
    fn fetch_request_encodes_doi_and_uses_query_key_without_authorization_header() {
        let ctx = context_with_key("dummy-openalex-key");
        let client = http_client().unwrap();
        let request = build_fetch_request(
            &client,
            "https://example.test",
            &ctx,
            "10.1000/path?version#fragment",
        )
        .unwrap();
        let serialized = request.url().as_str();
        let query = query_parameters(&request);

        assert!(serialized.contains("/works/doi:10.1000%2Fpath%3Fversion%23fragment"));
        assert_eq!(
            query.get("api_key").map(String::as_str),
            Some("dummy-openalex-key")
        );
        assert_eq!(query.len(), 1);
        assert!(request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());
    }

    #[test]
    fn non_success_http_statuses_are_error_outputs_without_response_details() {
        for (operation, status) in [
            ("搜索", reqwest::StatusCode::UNAUTHORIZED),
            ("搜索", reqwest::StatusCode::TOO_MANY_REQUESTS),
            ("获取论文", reqwest::StatusCode::NOT_FOUND),
            ("获取论文", reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let output = http_error(operation, status);
            assert!(output.is_error);
            assert_eq!(
                output.content,
                format!("OpenAlex {operation}失败（HTTP {}）", status.as_u16())
            );
            assert!(!output.content.contains("dummy-openalex-key"));
        }
    }

    #[test]
    fn response_helpers_handle_partial_authors_and_sparse_abstracts() {
        let authorships = json!([
            { "author": { "display_name": "Ada" } },
            { "author": { "display_name": "Lin" } },
            { "author": { "display_name": "Grace" } },
            { "author": { "display_name": "Edsger" } }
        ]);
        assert_eq!(format_authors(Some(&authorships)), "Ada, Lin, Grace et al.");
        assert_eq!(format_authors(Some(&Value::Null)), "unknown");

        let inverted = json!({
            "research": [4],
            "Open": [0],
            "science": [2],
            "ignored": ["not-a-position"]
        });
        assert_eq!(
            reconstruct_abstract(Some(&inverted)),
            "Open science research"
        );
        assert!(reconstruct_abstract(Some(&Value::Null)).is_empty());
    }
}
