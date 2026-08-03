//! web 工具：web_search（DuckDuckGo HTML 抓取）+ fetch_url（抓取转纯文本）。
//!
//! 自建实现（非 Anthropic 服务端工具），不引 scraper crate（最小改动）：
//! HTML 解析用朴素字符串扫描 + uddg URL 解码。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

/// Chrome 桌面 UA（降低被简单反爬拦截的概率）。
const CHROME_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/124.0.0.0 Safari/537.36";

fn http_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .user_agent(CHROME_UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ToolError::Other(format!("build http client: {e}")))
}

// ---------------- web_search ----------------

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web (DuckDuckGo) and return results. Each result has title, url, and a short snippet. Use for finding current information.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query." },
                    "max_results": { "type": "integer", "description": "Max results to return (default 5)." }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SearchArgs = parse_args(&args)?;
        let max = a.max_results.unwrap_or(5).clamp(1, 20);
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(&a.query)
        );

        let client = http_client()?;
        let resp = client.get(&url).send().await.map_err(|e| {
            ToolError::Other(format!("web_search request failed: {e}"))
        })?;
        let status = resp.status();
        let html = resp.text().await.map_err(|e| {
            ToolError::Other(format!("web_search read body failed: {e}"))
        })?;
        if !status.is_success() {
            return Ok(ToolOutput::err(format!(
                "web_search HTTP {status} (DuckDuckGo may be rate-limiting); try again later"
            )));
        }

        let results = parse_ddg(&html, max);
        if results.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "no results for {:?} (DuckDuckGo may be rate-limiting or returned no matches)",
                a.query
            )));
        }
        let mut out = String::new();
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!("{}. {}\n   {}\n   {}\n\n", i + 1, r.title, r.url, r.snippet));
        }
        Ok(ToolOutput::ok(out.trim().to_string()))
    }
}

struct DdgResult {
    title: String,
    url: String,
    snippet: String,
}

/// 朴素解析 DuckDuckGo HTML 结果页：每个结果块以 `class="result__a"` 锚点开头。
fn parse_ddg(html: &str, max: usize) -> Vec<DdgResult> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && out.len() < max {
        // 找下一个 result__a 锚点
        let Some(rel) = find_subslice(&bytes[pos..], b"result__a") else {
            break;
        };
        let anchor_start = pos + rel;
        // 从锚点往前找最近的 <a
        let a_open = match find_last_before(&bytes[..anchor_start], b"<a") {
            Some(p) => p,
            None => break,
        };
        // 找该锚点的闭合 </a>
        let a_close = match find_subslice(&bytes[anchor_start..], b"</a>") {
            Some(r) => anchor_start + r,
            None => break,
        };
        let anchor_html = &html[a_open..a_close];

        // 提取 href 里的 uddg= 参数（真实目标 URL）
        let target_url = extract_uddg(anchor_html).unwrap_or_default();
        // 锚点内文本（去标签）作为标题
        let title = strip_tags(anchor_html).trim().to_string();

        // snippet：在锚点之后找最近的 result__snippet 块（限定在合理窗口内）
        let snippet = find_snippet(&html[a_close..]);

        if !title.is_empty() {
            out.push(DdgResult {
                title,
                url: target_url,
                snippet,
            });
        }
        pos = a_close + 4;
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_last_before(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let mut found = None;
    let mut start = 0;
    while start + needle.len() <= haystack.len() {
        if let Some(p) = find_subslice(&haystack[start..], needle) {
            found = Some(start + p);
            start = start + p + 1;
        } else {
            break;
        }
    }
    found
}

/// 从锚点 HTML 里提取 `uddg=<encoded>` 的目标 URL（DDG 跳转包装）。
fn extract_uddg(anchor_html: &str) -> Option<String> {
    let key = "uddg=";
    let idx = anchor_html.find(key)?;
    let rest = &anchor_html[idx + key.len()..];
    // uddg 值到下一个 & 或 " 结束。
    let end = rest
        .find(|c: char| c == '&' || c == '"')
        .unwrap_or(rest.len());
    let raw = &rest[..end];
    let decoded = url_decode(raw);
    Some(decoded)
}

/// 朴素 URL 解码（%XX +）。
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 在锚点之后的窗口里找 result__snippet 的文本。
fn find_snippet(rest: &str) -> String {
    let bytes = rest.as_bytes();
    let win = bytes.len().min(4000);
    let window = &rest[..win];
    let Some(rel) = window.find("result__snippet") else {
        return String::new();
    };
    let after = &window[rel..];
    let Some(close) = after.find("</a>").or_else(|| after.find("</td>")) else {
        return String::new();
    };
    strip_tags(&after[..close]).trim().to_string()
}

// ---------------- fetch_url ----------------

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    max_chars: Option<usize>,
}

pub struct FetchUrlTool;

#[async_trait]
impl Tool for FetchUrlTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fetch_url".into(),
            description: "Fetch a web page and return its text content (HTML stripped to plain text). Use for reading a specific URL's content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch (http/https)." },
                    "max_chars": { "type": "integer", "description": "Max chars of text to return (default 8000)." }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: FetchArgs = parse_args(&args)?;
        let max_chars = a.max_chars.unwrap_or(8000).clamp(200, 50000);
        // 基本校验 URL
        let parsed = reqwest::Url::parse(&a.url)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid url: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Ok(ToolOutput::err(format!("only http/https allowed, got {}", parsed.scheme())));
        }

        let client = http_client()?;
        let resp = client.get(parsed).send().await.map_err(|e| {
            ToolError::Other(format!("fetch_url request failed: {e}"))
        })?;
        let status = resp.status();
        // 限制响应体大小：先看 content-length，再读上限。
        let cap: usize = 256 * 1024; // 256KB
        let body = match resp.content_length() {
            Some(n) if (n as usize) > cap => {
                return Ok(ToolOutput::err(format!("response too large ({n} bytes > {cap} cap)")));
            }
            _ => {
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| ToolError::Other(format!("fetch_url read body: {e}")))?;
                if bytes.len() > cap {
                    return Ok(ToolOutput::err(format!(
                        "response too large ({} bytes > {cap} cap)",
                        bytes.len()
                    )));
                }
                String::from_utf8_lossy(&bytes).to_string()
            }
        };
        if !status.is_success() {
            return Ok(ToolOutput::err(format!("fetch_url HTTP {status}")));
        }

        let text = html_to_text(&body);
        let truncated = truncate_chars_safe(&text, max_chars);
        Ok(ToolOutput::ok(truncated))
    }
}

// ---------------- HTML → 纯文本 ----------------

/// 极简 HTML→文本：移除 script/style/注释，剥标签，解码基本实体，压空白。
/// 不追求完美，够 LLM 阅读即可（最小改动，不引 html5ever）。
pub fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    // 移除 script/style 块（含内容）
    s = remove_blocks(&s, "<script", "</script>");
    s = remove_blocks(&s, "<style", "</style>");
    s = remove_blocks(&s, "<!--", "-->");
    s = remove_blocks(&s, "<noscript", "</noscript>");

    // 换行标签转换行
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // 解码基本实体
    let out = decode_entities(&out);
    // 压缩空白
    collapse_whitespace(&out)
}

fn remove_blocks(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.to_lowercase().find(&open.to_lowercase()) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        match after.to_lowercase().find(&close.to_lowercase()) {
            Some(end) => rest = &after[end + close.len()..],
            None => {
                // 未闭合：丢弃剩余
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
}

/// 剥所有标签（含属性），保留标签间的文本，不解码实体。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_chars_safe(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated]");
    out
}

// 极简 URL 编码（仅 query 用，query 参数本身）。
// reqwest 不导出 encoding，自写一个最小版避免引新 crate。
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(b as char);
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{:02X}", b)),
            }
        }
        out
    }
}
