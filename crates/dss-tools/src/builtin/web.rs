//! web 工具：web_search（DuckDuckGo HTML，challenge 时回退 Bing RSS）+
//! fetch_url（抓取转纯文本）。
//!
//! 自建实现（非 Anthropic 服务端工具），不引 scraper crate（最小改动）：
//! HTML 解析用朴素字符串扫描 + uddg URL 解码。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

/// Chrome 桌面 UA（降低被简单反爬拦截的概率）。
const CHROME_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/124.0.0.0 Safari/537.36";

const DDG_SEARCH_URL: &str = "https://html.duckduckgo.com/html/?q=";
const BING_RSS_SEARCH_URL: &str = "https://www.bing.com/search?format=rss&q=";
const CLOUDFLARE_DOH_HOST: &str = "cloudflare-dns.com";
const CLOUDFLARE_DOH_URL: &str = "https://cloudflare-dns.com/dns-query";
const DOH_RESPONSE_CAP: usize = 64 * 1024;
const SEARCH_RESPONSE_CAP: usize = 512 * 1024;
const MAX_RESOLVED_ADDRESSES: usize = 64;
const BLOCKED_HOST_MESSAGE: &str =
    "local, private, reserved, and mixed public/private hosts are blocked";

fn http_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .user_agent(CHROME_UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ToolError::Other(format!("build http client: {e}")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressClass {
    Public,
    /// RFC 2544's 198.18.0.0/15 range is also used by TUN/fake-IP proxies.
    ProxySynthetic,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolutionAction {
    UseResolved,
    UseCloudflareDoh,
    Block,
}

async fn public_http_client(url: &reqwest::Url) -> Result<reqwest::Client, ToolError> {
    let host = url
        .host_str()
        .ok_or_else(|| ToolError::InvalidArgs("URL must include a host".into()))?;
    if is_localhost_name(host) {
        return Err(blocked_host_error());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ToolError::InvalidArgs("URL port is unknown".into()))?;

    // URL parsing normalizes alternative IPv4 spellings (for example 2130706433)
    // before host_str(), so this also covers disguised IP literals. Literals never
    // use the fake-IP compatibility fallback.
    if let Some(ip) = parse_ip_literal(host) {
        let addresses = [SocketAddr::new(ip, port)];
        return match resolution_action(host, classify_ip(ip)) {
            ResolutionAction::UseResolved => pinned_http_client(host, &addresses),
            ResolutionAction::UseCloudflareDoh | ResolutionAction::Block => {
                Err(blocked_host_error())
            }
        };
    }

    let lookup = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| ToolError::Other("DNS lookup timed out; URL blocked".into()))?
    .map_err(|e| ToolError::Other(format!("DNS lookup failed; URL blocked: {e}")))?;
    let mut addresses: Vec<SocketAddr> = lookup.take(MAX_RESOLVED_ADDRESSES + 1).collect();
    if addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err(ToolError::InvalidArgs(
            "DNS returned too many addresses; URL blocked".into(),
        ));
    }
    addresses.sort_unstable();
    addresses.dedup();

    match resolution_action(host, classify_addresses(&addresses)) {
        ResolutionAction::UseResolved => pinned_http_client(host, &addresses),
        ResolutionAction::UseCloudflareDoh => {
            let doh_addresses = resolve_with_cloudflare_doh(host, port).await?;
            pinned_http_client(host, &doh_addresses)
        }
        ResolutionAction::Block => Err(blocked_host_error()),
    }
}

fn pinned_http_client(host: &str, addresses: &[SocketAddr]) -> Result<reqwest::Client, ToolError> {
    if classify_addresses(addresses) != AddressClass::Public {
        return Err(blocked_host_error());
    }

    reqwest::Client::builder()
        // A proxy CONNECT can resolve the hostname again and bypass the validated
        // DNS override. Direct connections are required for the pin to be binding.
        .no_proxy()
        .user_agent(CHROME_UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        // Pin this request to the complete validated set, avoiding a second DNS
        // resolution between the SSRF check and the connection while preserving
        // TLS SNI and the HTTP Host header.
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|e| ToolError::Other(format!("build http client: {e}")))
}

fn blocked_host_error() -> ToolError {
    ToolError::InvalidArgs(BLOCKED_HOST_MESSAGE.into())
}

fn is_localhost_name(host: &str) -> bool {
    let canonical_host = host.trim_end_matches('.').to_ascii_lowercase();
    canonical_host == "localhost" || canonical_host.ends_with(".localhost")
}

fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

fn resolution_action(host: &str, class: AddressClass) -> ResolutionAction {
    match class {
        AddressClass::Public => ResolutionAction::UseResolved,
        AddressClass::ProxySynthetic if parse_ip_literal(host).is_none() => {
            ResolutionAction::UseCloudflareDoh
        }
        AddressClass::ProxySynthetic | AddressClass::Blocked => ResolutionAction::Block,
    }
}

fn classify_addresses(addresses: &[SocketAddr]) -> AddressClass {
    let Some((first, rest)) = addresses.split_first() else {
        return AddressClass::Blocked;
    };
    let class = classify_ip(first.ip());
    if class == AddressClass::Blocked || rest.iter().any(|addr| classify_ip(addr.ip()) != class) {
        AddressClass::Blocked
    } else {
        class
    }
}

fn classify_ip(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
                return AddressClass::ProxySynthetic;
            }
            if v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 192 && o[1] == 88 && o[2] == 99)
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || o[0] >= 240
            {
                AddressClass::Blocked
            } else {
                AddressClass::Public
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return classify_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            let allocated_global_unicast = (s[0] & 0xe000) == 0x2000;
            if !allocated_global_unicast
                || v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || (s[0] & 0xffc0) == 0xfec0
                // IETF protocol assignments, including Teredo and benchmarking.
                || (s[0] == 0x2001 && s[1] <= 0x01ff)
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                // Deprecated 6to4 embeds an IPv4 target and is unsafe for SSRF.
                || s[0] == 0x2002
                // Retired 6bone allocation.
                || s[0] == 0x3ffe
                // Documentation prefix 3fff::/20.
                || (s[0] == 0x3fff && (s[1] & 0xf000) == 0)
            {
                AddressClass::Blocked
            } else {
                AddressClass::Public
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct DnsJsonResponse {
    #[serde(rename = "Status")]
    status: u16,
    #[serde(rename = "TC", default)]
    truncated: bool,
    #[serde(rename = "Answer")]
    answers: Option<Vec<DnsJsonAnswer>>,
}

#[derive(Debug, Deserialize)]
struct DnsJsonAnswer {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

async fn resolve_with_cloudflare_doh(host: &str, port: u16) -> Result<Vec<SocketAddr>, ToolError> {
    let client = cloudflare_doh_client()?;
    let mut ips = query_cloudflare_doh(&client, host, "A").await?;
    ips.extend(query_cloudflare_doh(&client, host, "AAAA").await?);
    validate_doh_ips(ips, port)
}

fn cloudflare_doh_client() -> Result<reqwest::Client, ToolError> {
    let endpoint_addresses = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), 443),
    ];
    reqwest::Client::builder()
        .no_proxy()
        .user_agent(CHROME_UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(CLOUDFLARE_DOH_HOST, &endpoint_addresses)
        .build()
        .map_err(|e| ToolError::Other(format!("build secure DNS client: {e}")))
}

async fn query_cloudflare_doh(
    client: &reqwest::Client,
    host: &str,
    record_type: &str,
) -> Result<Vec<IpAddr>, ToolError> {
    let mut url = reqwest::Url::parse(CLOUDFLARE_DOH_URL)
        .map_err(|e| ToolError::Other(format!("invalid secure DNS endpoint: {e}")))?;
    url.query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", record_type);

    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/dns-json")
        .send()
        .await
        .map_err(|e| ToolError::Other(format!("secure DNS lookup failed; URL blocked: {e}")))?;
    if !response.status().is_success() {
        return Err(ToolError::Other(format!(
            "secure DNS HTTP {}; URL blocked",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > DOH_RESPONSE_CAP as u64)
    {
        return Err(ToolError::Other(
            "secure DNS response too large; URL blocked".into(),
        ));
    }

    let mut body = Vec::with_capacity(4096);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ToolError::Other(format!("secure DNS response failed; URL blocked: {e}")))?
    {
        if body.len().saturating_add(chunk.len()) > DOH_RESPONSE_CAP {
            return Err(ToolError::Other(
                "secure DNS response too large; URL blocked".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    parse_doh_ips(&body)
        .map_err(|e| ToolError::Other(format!("invalid secure DNS response; URL blocked: {e}")))
}

fn parse_doh_ips(body: &[u8]) -> Result<Vec<IpAddr>, String> {
    let response: DnsJsonResponse =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if response.status != 0 {
        return Err(format!("DNS status {}", response.status));
    }
    if response.truncated {
        return Err("truncated DNS response".into());
    }

    let mut ips = Vec::new();
    for answer in response.answers.unwrap_or_default() {
        let expected_v4 = match answer.record_type {
            1 => true,
            28 => false,
            _ => continue,
        };
        let ip = answer
            .data
            .parse::<IpAddr>()
            .map_err(|e| format!("malformed address record {:?}: {e}", answer.data))?;
        if matches!(ip, IpAddr::V4(_)) != expected_v4 {
            return Err(format!(
                "address record type does not match {:?}",
                answer.data
            ));
        }
        ips.push(ip);
    }
    Ok(ips)
}

fn validate_doh_ips(mut ips: Vec<IpAddr>, port: u16) -> Result<Vec<SocketAddr>, ToolError> {
    if ips.is_empty() {
        return Err(ToolError::InvalidArgs(
            "secure DNS returned no A/AAAA records; URL blocked".into(),
        ));
    }
    if ips.len() > MAX_RESOLVED_ADDRESSES {
        return Err(ToolError::InvalidArgs(
            "secure DNS returned too many addresses; URL blocked".into(),
        ));
    }
    if ips
        .iter()
        .any(|ip| classify_ip(*ip) != AddressClass::Public)
    {
        return Err(ToolError::InvalidArgs(
            "secure DNS returned local, private, reserved, synthetic, or mixed addresses; URL blocked"
                .into(),
        ));
    }
    ips.sort_unstable();
    ips.dedup();
    Ok(ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect())
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
            description: "Search the web and return results. Each result has title, url, and a short snippet. Use for finding current information.".into(),
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
        let url = format!("{DDG_SEARCH_URL}{}", urlencoding::encode(&a.query));

        let client = http_client()?;
        // DDG commonly serves WAF/rate-limit pages with either a non-success
        // status or an unfamiliar HTTP-200 body. Both are availability
        // failures, not evidence that the query has zero results.
        let selection = match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match read_search_body(resp, "DuckDuckGo").await {
                    Ok(html) => select_after_ddg(parse_ddg(&html, max)),
                    Err(_) => SearchSelection::UseBingRss,
                }
            }
            Ok(_) | Err(_) => SearchSelection::UseBingRss,
        };

        let results = match selection {
            SearchSelection::Results(results) => results,
            SearchSelection::NoResults => {
                return Ok(ToolOutput::ok(format!(
                    "no results for {:?} (DuckDuckGo returned no matches)",
                    a.query
                )));
            }
            SearchSelection::UseBingRss => {
                let bing_url = format!("{BING_RSS_SEARCH_URL}{}", urlencoding::encode(&a.query));
                let resp = client.get(&bing_url).send().await.map_err(|e| {
                    ToolError::Other(format!("web_search Bing RSS fallback failed: {e}"))
                })?;
                let status = resp.status();
                if !status.is_success() {
                    return Ok(ToolOutput::err(format!(
                        "web_search HTTP {status} from Bing RSS fallback"
                    )));
                }
                let rss = match read_search_body(resp, "Bing RSS fallback").await {
                    Ok(rss) => rss,
                    Err(reason) => return Ok(ToolOutput::err(reason)),
                };
                let results = match parse_bing_rss(&rss, max) {
                    Ok(results) => results,
                    Err(reason) => {
                        return Ok(ToolOutput::err(format!(
                            "web_search Bing RSS fallback returned an invalid feed: {reason}"
                        )));
                    }
                };
                if results.is_empty() {
                    return Ok(ToolOutput::ok(format!(
                        "no results for {:?} (DuckDuckGo requested a challenge; Bing returned no matches)",
                        a.query
                    )));
                }
                results
            }
        };
        let mut out = String::new();
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}\n   {}\n   {}\n\n",
                i + 1,
                r.title,
                r.url,
                r.snippet
            ));
        }
        Ok(ToolOutput::ok(out.trim().to_string()))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Eq, PartialEq)]
enum DdgParseOutcome {
    Results(Vec<SearchResult>),
    Challenge,
    NoResults,
    Unrecognized,
}

#[derive(Debug, Eq, PartialEq)]
enum SearchSelection {
    Results(Vec<SearchResult>),
    NoResults,
    UseBingRss,
}

fn select_after_ddg(outcome: DdgParseOutcome) -> SearchSelection {
    match outcome {
        DdgParseOutcome::Results(results) => SearchSelection::Results(results),
        DdgParseOutcome::Challenge => SearchSelection::UseBingRss,
        DdgParseOutcome::NoResults => SearchSelection::NoResults,
        DdgParseOutcome::Unrecognized => SearchSelection::UseBingRss,
    }
}

/// 朴素解析 DuckDuckGo HTML 结果页：每个结果块以 `class="result__a"` 锚点开头。
fn parse_ddg(html: &str, max: usize) -> DdgParseOutcome {
    if is_ddg_challenge(html) {
        return DdgParseOutcome::Challenge;
    }

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
            out.push(SearchResult {
                title,
                url: target_url,
                snippet,
            });
        }
        pos = a_close + 4;
    }
    if out.is_empty() {
        if is_ddg_explicit_no_results(html) {
            DdgParseOutcome::NoResults
        } else {
            DdgParseOutcome::Unrecognized
        }
    } else {
        DdgParseOutcome::Results(out)
    }
}

/// Only an explicit DDG no-results component is authoritative. An arbitrary
/// HTTP-200 HTML page without `result__a` can be a WAF, regional interstitial,
/// or future markup revision and must fall back rather than silently lying.
fn is_ddg_explicit_no_results(html: &str) -> bool {
    let lowercase = html.to_ascii_lowercase();
    lowercase.contains("no-results__message")
        || lowercase.contains("class=\"no-results\"")
        || lowercase.contains("class='no-results'")
}

async fn read_search_body(mut response: reqwest::Response, source: &str) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > SEARCH_RESPONSE_CAP as u64)
    {
        return Err(format!(
            "web_search {source} response too large (content-length exceeds {SEARCH_RESPONSE_CAP} byte cap)"
        ));
    }

    let mut body = Vec::with_capacity(32 * 1024);
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|error| format!("web_search read {source} body failed: {error}"))?;
        let Some(chunk) = chunk else {
            break;
        };
        append_search_chunk(&mut body, &chunk)?;
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn append_search_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), String> {
    if body.len().saturating_add(chunk.len()) > SEARCH_RESPONSE_CAP {
        return Err(format!(
            "web_search response too large (stream exceeds {SEARCH_RESPONSE_CAP} byte cap)"
        ));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// DDG 的反机器人页当前仍返回 HTTP 200；用其结构化 challenge 标记识别，
/// 避免把 CAPTCHA/anomaly 页当成合法的空搜索结果。
fn is_ddg_challenge(html: &str) -> bool {
    let lowercase = html.to_ascii_lowercase();
    lowercase.contains("duckduckgo.com/anomaly.js")
        || lowercase.contains("data-testid=\"anomaly-modal\"")
        || (lowercase.contains("id=\"challenge-form\"")
            && lowercase.contains("name=\"challenge-submit\""))
        || lowercase.contains("unfortunately, bots use duckduckgo too")
}

/// 解析 Bing `format=rss` 响应。只接受完整 RSS channel；HTML challenge 或
/// 截断 XML 会返回错误，而不是伪装成空结果。
fn parse_bing_rss(rss: &str, max: usize) -> Result<Vec<SearchResult>, &'static str> {
    if !(rss.contains("<rss ")
        && rss.contains("<channel>")
        && rss.contains("</channel>")
        && rss.contains("</rss>"))
    {
        return Err("missing complete rss/channel envelope");
    }

    let mut out = Vec::new();
    let mut rest = rss;
    while out.len() < max {
        let Some(item_start) = rest.find("<item>") else {
            break;
        };
        let item_and_rest = &rest[item_start + "<item>".len()..];
        let Some(item_end) = item_and_rest.find("</item>") else {
            return Err("unterminated item");
        };
        let item = &item_and_rest[..item_end];
        let title = rss_text(extract_xml_element(item, "title").ok_or("item is missing title")?);
        let url = rss_value(extract_xml_element(item, "link").ok_or("item is missing link")?);
        if title.is_empty() || url.is_empty() {
            return Err("item has an empty title or link");
        }
        let snippet = extract_xml_element(item, "description")
            .map(rss_text)
            .unwrap_or_default();
        out.push(SearchResult {
            title,
            url,
            snippet,
        });
        rest = &item_and_rest[item_end + "</item>".len()..];
    }
    Ok(out)
}

fn extract_xml_element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

fn rss_value(value: &str) -> String {
    let value = value.trim();
    let value = value
        .strip_prefix("<![CDATA[")
        .and_then(|value| value.strip_suffix("]]>"))
        .unwrap_or(value);
    decode_entities(value).trim().to_string()
}

fn rss_text(value: &str) -> String {
    let decoded = rss_value(value);
    collapse_whitespace(&strip_tags(&decoded))
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
    let end = rest.find(['&', '"']).unwrap_or(rest.len());
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

#[cfg(test)]
mod search_tests {
    use super::{
        append_search_chunk, parse_bing_rss, parse_ddg, select_after_ddg, DdgParseOutcome,
        SearchSelection, BING_RSS_SEARCH_URL, SEARCH_RESPONSE_CAP,
    };

    #[test]
    fn ddg_anomaly_and_captcha_pages_select_bing_rss() {
        let pages = [
            r#"<form id="challenge-form" action="//duckduckgo.com/anomaly.js?sv=html">
                    <button name="challenge-submit">Submit</button>
                </form>"#,
            r#"<div data-testid="anomaly-modal">Please complete the challenge</div>"#,
            r#"<div>Unfortunately, bots use DuckDuckGo too.</div>"#,
        ];

        for page in pages {
            assert_eq!(
                select_after_ddg(parse_ddg(page, 5)),
                SearchSelection::UseBingRss
            );
        }
    }

    #[test]
    fn challenge_wins_over_any_result_shaped_markup() {
        let page = r#"
            <form id="challenge-form" action="//duckduckgo.com/anomaly.js">
                <button name="challenge-submit">Submit</button>
            </form>
            <a class="result__a" href="/?uddg=https%3A%2F%2Fexample.com">decoy</a>
        "#;

        assert_eq!(parse_ddg(page, 5), DdgParseOutcome::Challenge);
    }

    #[test]
    fn only_explicit_empty_pages_are_authoritative_and_unknown_html_falls_back() {
        let empty =
            r#"<html><body><div class="no-results__message">No results.</div></body></html>"#;
        assert_eq!(
            select_after_ddg(parse_ddg(empty, 5)),
            SearchSelection::NoResults
        );
        for unknown in [
            "<html><body>No results about captcha or anomaly handling.</body></html>",
            "<html><body>regional consent page</body></html>",
            "<html><body><main>new result markup</main></body></html>",
        ] {
            assert_eq!(
                select_after_ddg(parse_ddg(unknown, 5)),
                SearchSelection::UseBingRss
            );
        }

        let result = r#"
            <a class="result__a"
               href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&amp;rut=abc">
                Example Documentation
            </a>
            <a class="result__snippet">An example result.</a>
        "#;
        match select_after_ddg(parse_ddg(result, 5)) {
            SearchSelection::Results(results) => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].title, "Example Documentation");
                assert_eq!(results[0].url, "https://example.com/docs");
            }
            selection => panic!("unexpected selection: {selection:?}"),
        }
    }

    #[test]
    fn parses_bing_rss_items_and_honors_limit() {
        let rss = r#"<?xml version="1.0" encoding="utf-8"?>
            <rss version="2.0"><channel>
                <title>Bing: example</title>
                <item>
                    <title>Rust &amp; Safety</title>
                    <link>https://example.com/?a=1&amp;b=2</link>
                    <description><![CDATA[Fast <b>and</b> safe]]></description>
                </item>
                <item>
                    <title><![CDATA[Second result]]></title>
                    <link><![CDATA[https://example.org/second]]></link>
                </item>
            </channel></rss>"#;

        let results = parse_bing_rss(rss, 20).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust & Safety");
        assert_eq!(results[0].url, "https://example.com/?a=1&b=2");
        assert_eq!(results[0].snippet, "Fast and safe");
        assert_eq!(results[1].title, "Second result");
        assert_eq!(results[1].snippet, "");

        assert_eq!(parse_bing_rss(rss, 1).unwrap().len(), 1);
    }

    #[test]
    fn bing_rss_parser_rejects_html_and_truncated_or_invalid_items() {
        assert!(parse_bing_rss("<html>challenge</html>", 5).is_err());
        assert!(parse_bing_rss(
            r#"<rss version="2.0"><channel><item><title>x</title></channel></rss>"#,
            5
        )
        .is_err());
        assert!(parse_bing_rss(
            r#"<rss version="2.0"><channel><item><link>https://example.com</link></item></channel></rss>"#,
            5
        )
        .is_err());

        let empty = r#"<rss version="2.0"><channel></channel></rss>"#;
        assert!(parse_bing_rss(empty, 5).unwrap().is_empty());
    }

    #[test]
    fn bing_rss_fallback_endpoint_is_https_search_feed() {
        let url = reqwest::Url::parse(BING_RSS_SEARCH_URL).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("www.bing.com"));
        assert_eq!(url.path(), "/search");
        assert!(url.query().unwrap().contains("format=rss"));
    }

    #[test]
    fn search_response_body_cap_accepts_n_and_rejects_n_plus_one() {
        let mut exact = Vec::new();
        append_search_chunk(&mut exact, &vec![b'x'; SEARCH_RESPONSE_CAP]).unwrap();
        assert_eq!(exact.len(), SEARCH_RESPONSE_CAP);
        assert!(append_search_chunk(&mut exact, b"x").is_err());

        let mut overflow = vec![b'x'; SEARCH_RESPONSE_CAP - 1];
        assert!(append_search_chunk(&mut overflow, b"xx").is_err());
        assert_eq!(overflow.len(), SEARCH_RESPONSE_CAP - 1);
    }
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
            return Ok(ToolOutput::err(format!(
                "only http/https allowed, got {}",
                parsed.scheme()
            )));
        }

        let mut current = parsed;
        let mut resp = {
            let mut final_response = None;
            for _ in 0..=5 {
                let client = public_http_client(&current).await?;
                let response = client
                    .get(current.clone())
                    .send()
                    .await
                    .map_err(|e| ToolError::Other(format!("fetch_url request failed: {e}")))?;
                if !response.status().is_redirection() {
                    final_response = Some(response);
                    break;
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ToolError::Other("redirect missing Location header".into()))?;
                current = current
                    .join(location)
                    .map_err(|e| ToolError::Other(format!("invalid redirect URL: {e}")))?;
                if !matches!(current.scheme(), "http" | "https") {
                    return Ok(ToolOutput::err("redirect to non-http(s) URL blocked"));
                }
            }
            final_response.ok_or_else(|| ToolError::Other("too many redirects (max 5)".into()))?
        };
        let status = resp.status();
        // 限制响应体大小：先看 content-length，再读上限。
        let cap: usize = 256 * 1024; // 256KB
        let body = match resp.content_length() {
            Some(n) if (n as usize) > cap => {
                return Ok(ToolOutput::err(format!(
                    "response too large ({n} bytes > {cap} cap)"
                )));
            }
            _ => {
                let mut bytes = Vec::with_capacity(cap.min(32 * 1024));
                while let Some(chunk) = resp
                    .chunk()
                    .await
                    .map_err(|e| ToolError::Other(format!("fetch_url read body: {e}")))?
                {
                    if bytes.len().saturating_add(chunk.len()) > cap {
                        return Ok(ToolOutput::err(format!(
                            "response too large (stream exceeded {cap} byte cap)"
                        )));
                    }
                    bytes.extend_from_slice(&chunk);
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

#[cfg(test)]
mod security_tests {
    use super::{
        classify_addresses, classify_ip, is_localhost_name, parse_doh_ips, parse_ip_literal,
        resolution_action, validate_doh_ips, AddressClass, ResolutionAction, CLOUDFLARE_DOH_HOST,
        CLOUDFLARE_DOH_URL, MAX_RESOLVED_ADDRESSES,
    };
    use std::net::{IpAddr, SocketAddr};

    fn socket(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }

    #[test]
    fn classifies_blocked_public_and_proxy_synthetic_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.88.99.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "64:ff9b:1::1",
            "100::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3ffe::1",
            "3fff::1",
            "5f00::1",
            "::ffff:127.0.0.1",
        ] {
            assert_eq!(
                classify_ip(ip.parse().unwrap()),
                AddressClass::Blocked,
                "accepted {ip}"
            );
        }
        for ip in [
            "198.18.0.0",
            "198.18.0.1",
            "198.19.255.255",
            "::198.18.0.1",
            "::ffff:198.18.0.1",
        ] {
            assert_eq!(
                classify_ip(ip.parse().unwrap()),
                AddressClass::ProxySynthetic,
                "did not recognize {ip}"
            );
        }
        for ip in [
            "1.1.1.1",
            "198.17.255.255",
            "198.20.0.0",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert_eq!(
                classify_ip(ip.parse().unwrap()),
                AddressClass::Public,
                "blocked {ip}"
            );
        }
    }

    #[test]
    fn classifies_only_uniform_nonempty_address_sets() {
        assert_eq!(classify_addresses(&[]), AddressClass::Blocked);
        assert_eq!(
            classify_addresses(&[socket("1.1.1.1"), socket("8.8.8.8")]),
            AddressClass::Public
        );
        assert_eq!(
            classify_addresses(&[socket("198.18.0.1"), socket("198.19.0.1")]),
            AddressClass::ProxySynthetic
        );
        for addresses in [
            vec![socket("1.1.1.1"), socket("127.0.0.1")],
            vec![socket("1.1.1.1"), socket("198.18.0.1")],
            vec![socket("198.18.0.1"), socket("10.0.0.1")],
            vec![socket("10.0.0.1")],
        ] {
            assert_eq!(classify_addresses(&addresses), AddressClass::Blocked);
        }
    }

    #[test]
    fn proxy_synthetic_fallback_is_hostname_only() {
        assert_eq!(
            resolution_action("example.com", AddressClass::ProxySynthetic),
            ResolutionAction::UseCloudflareDoh
        );
        assert_eq!(
            resolution_action("198.18.0.1", AddressClass::ProxySynthetic),
            ResolutionAction::Block
        );
        assert_eq!(
            resolution_action("::ffff:198.18.0.1", AddressClass::ProxySynthetic),
            ResolutionAction::Block
        );
        assert_eq!(
            resolution_action("example.com", AddressClass::Public),
            ResolutionAction::UseResolved
        );
        assert_eq!(
            resolution_action("example.com", AddressClass::Blocked),
            ResolutionAction::Block
        );
    }

    #[test]
    fn url_parser_normalizes_disguised_ip_literals_before_routing() {
        let url = reqwest::Url::parse("http://2130706433/").unwrap();
        let host = url.host_str().unwrap();
        assert_eq!(host, "127.0.0.1");
        let ip = parse_ip_literal(host).unwrap();
        assert_eq!(
            resolution_action(host, classify_ip(ip)),
            ResolutionAction::Block
        );

        let url = reqwest::Url::parse("http://[::ffff:198.18.0.1]/").unwrap();
        let host = url.host_str().unwrap();
        assert!(host.starts_with('[') && host.ends_with(']'));
        let ip = parse_ip_literal(host).unwrap();
        assert_eq!(
            resolution_action(host, classify_ip(ip)),
            ResolutionAction::Block
        );
    }

    #[test]
    fn localhost_names_are_blocked_case_insensitively_with_trailing_dot() {
        for host in ["localhost", "LOCALHOST.", "api.localhost", "API.LOCALHOST."] {
            assert!(is_localhost_name(host), "accepted {host}");
        }
        assert!(!is_localhost_name("localhost.example"));
        assert!(!is_localhost_name("example.com"));
    }

    #[test]
    fn parses_only_well_formed_doh_address_records() {
        let body = br#"{
            "Status": 0,
            "Answer": [
                {"type": 5, "data": "alias.example.com."},
                {"type": 1, "data": "104.20.23.154"},
                {"type": 28, "data": "2606:4700:10::6814:179a"}
            ]
        }"#;
        assert_eq!(
            parse_doh_ips(body).unwrap(),
            vec![
                "104.20.23.154".parse::<IpAddr>().unwrap(),
                "2606:4700:10::6814:179a".parse::<IpAddr>().unwrap(),
            ]
        );
        assert!(parse_doh_ips(br#"{"Status": 3}"#).is_err());
        assert!(parse_doh_ips(br#"{"Status": 0, "TC": true}"#).is_err());
        assert!(
            parse_doh_ips(br#"{"Status": 0, "Answer": [{"type": 1, "data": "bad"}]}"#).is_err()
        );
        assert!(parse_doh_ips(
            br#"{"Status": 0, "Answer": [{"type": 1, "data": "2606:4700::1"}]}"#
        )
        .is_err());
        assert!(parse_doh_ips(b"not json").is_err());
    }

    #[test]
    fn doh_validation_is_nonempty_public_only_and_preserves_port() {
        let validated = validate_doh_ips(
            vec![
                "2606:4700:4700::1111".parse().unwrap(),
                "1.1.1.1".parse().unwrap(),
                "1.1.1.1".parse().unwrap(),
            ],
            8443,
        )
        .unwrap();
        assert_eq!(validated.len(), 2);
        assert!(validated.iter().all(|address| address.port() == 8443));

        assert!(validate_doh_ips(vec![], 443).is_err());
        assert!(validate_doh_ips(vec!["127.0.0.1".parse().unwrap()], 443).is_err());
        assert!(validate_doh_ips(vec!["198.18.0.1".parse().unwrap()], 443).is_err());
        assert!(validate_doh_ips(
            vec!["1.1.1.1".parse().unwrap(), "192.168.1.1".parse().unwrap()],
            443
        )
        .is_err());
        assert!(validate_doh_ips(
            vec!["1.1.1.1".parse().unwrap(); MAX_RESOLVED_ADDRESSES + 1],
            443
        )
        .is_err());
    }

    #[test]
    fn secure_dns_endpoint_is_fixed_to_cloudflare_hostname() {
        let url = reqwest::Url::parse(CLOUDFLARE_DOH_URL).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(CLOUDFLARE_DOH_HOST));
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
