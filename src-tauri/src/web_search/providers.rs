//! Wire protocols for ten search providers.
//!
//! Each function turns a keyword or URL into [`SearchResultItem`] values. Shared
//! normalization, filtering, compression, and limits belong to [`super::pipeline`].
//!
//! Provider-specific compatibility behavior is documented next to each implementation.

use std::time::Duration;

use base64::Engine as _;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use url::Url;

use crate::model::{SearchCapability, SearchExecutionConfig, SearchProviderKind, ResolvedSearchProvider};
use crate::http_util::{api_error_message, read_body, sanitize_error};

use super::readable::{self, ReadablePage};
use super::{SearchError, SearchResultItem};

/// Maximum readable upstream response size.
const MAX_RESPONSE_BODY: usize = 8 * 1024 * 1024;
/// Timeout for long-running Exa MCP deep searches.
const EXA_MCP_TIMEOUT: Duration = Duration::from_secs(25);
/// Built-in Jina hosts, which serve as each other's fallback.
const JINA_SEARCH_HOSTS: [&str; 2] = ["https://s.jina.ai", "https://s.jinaai.cn"];
const JINA_READER_HOSTS: [&str; 2] = ["https://r.jina.ai", "https://r.jinaai.cn"];

/// All inputs needed for one provider call. It must own its data because a worker
/// thread may outlive the calling turn.
#[derive(Clone)]
pub(crate) struct ProviderCall {
    pub client: Client,
    pub provider: ResolvedSearchProvider,
    pub execution: SearchExecutionConfig,
    /// Required-key providers must supply this value; optional-key providers use it
    /// when present and make anonymous requests otherwise.
    pub api_key: Option<String>,
    /// Used only by `searxng`.
    pub basic_auth_password: Option<String>,
    /// Whether a directly named fetch target may resolve to a local address.
    /// Set from the conversation's security level; never applies to URLs the
    /// user did not name, such as search results or redirect hops.
    pub allow_local_targets: bool,
}

impl ProviderCall {
    fn max_results(&self) -> usize {
        self.execution.max_results.max(1) as usize
    }

    fn key(&self) -> Result<&str, SearchError> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                SearchError::Config(format!(
                    "Search provider {} does not have an API key configured",
                    self.provider.kind.label()
                ))
            })
    }

    /// Validated configured HTTP(S) host.
    fn host(&self) -> Result<Url, SearchError> {
        parse_api_host(self.provider.kind, &self.provider.api_host)
    }

    /// Appends a path while retaining the host's configured path prefix.
    fn endpoint(&self, path: &str) -> Result<Url, SearchError> {
        join_path(&self.host()?, path)
    }

    fn send(&self, request: RequestBuilder) -> Result<Value, SearchError> {
        let text = self.send_text(request)?;
        serde_json::from_str(&text).map_err(|error| {
            SearchError::Transient(format!(
                "{} returned invalid JSON: {error}",
                self.provider.kind.label()
            ))
        })
    }

    fn send_text(&self, request: RequestBuilder) -> Result<String, SearchError> {
        let label = self.provider.kind.label();
        let key = self.api_key.as_deref();
        let response = request.send().map_err(|error| {
            SearchError::Transient(sanitize_error(&format!("{label} request failed: {error}"), key))
        })?;
        let status = response.status();
        let (body, _truncated) = read_body(response, MAX_RESPONSE_BODY)
            .map_err(|error| SearchError::Transient(sanitize_error(&error, key)))?;
        if !status.is_success() {
            let message = api_error_message(status, &body);
            return Err(SearchError::Transient(sanitize_error(
                &format!("{label} request failed: {message}"),
                key,
            )));
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    fn basic_auth_header(&self) -> Option<String> {
        let username = self.provider.basic_auth_username.trim();
        if username.is_empty() {
            return None;
        }
        let password = self.basic_auth_password.as_deref().unwrap_or_default();
        Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        ))
    }
}

/// Validates a configured API host: only credential-free HTTP(S) URLs are accepted,
/// and HTTP is restricted to local-network addresses for self-hosted engines.
pub(crate) fn parse_api_host(kind: SearchProviderKind, host: &str) -> Result<Url, SearchError> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(SearchError::Config(format!(
            "Search provider {} does not have an endpoint configured",
            kind.label()
        )));
    }
    let url = Url::parse(trimmed).map_err(|error| {
        SearchError::Config(format!("{} endpoint is not a valid URL: {error}", kind.label()))
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SearchError::Config(format!(
            "{} endpoint must not include a username or password",
            kind.label()
        )));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if crate::http_util::is_local_network_url(&url) => Ok(url),
        "http" => Err(SearchError::Config(format!(
            "{} endpoint may use HTTP only for a local or private-network address",
            kind.label()
        ))),
        scheme => Err(SearchError::Config(format!(
            "{} endpoint uses an unsupported scheme: {scheme}",
            kind.label()
        ))),
    }
}

fn join_path(host: &Url, path: &str) -> Result<Url, SearchError> {
    let mut base = host.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(path.trim_start_matches('/'))
        .map_err(|error| {
            SearchError::Config(format!("Could not derive an endpoint from {host}: {error}"))
        })
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// First non-empty value from the given fields.
fn first_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| text_of(value, key))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

fn array_of<'a>(value: &'a Value, path: &[&str]) -> &'a [Value] {
    let mut cursor = value;
    for key in path {
        match cursor.get(key) {
            Some(next) => cursor = next,
            None => return &[],
        }
    }
    cursor.as_array().map(Vec::as_slice).unwrap_or_default()
}

// ------------------------------------------------------------------ Dispatch

pub(crate) fn search_keywords(
    call: &ProviderCall,
    query: &str,
) -> Result<Vec<SearchResultItem>, SearchError> {
    if !call.provider.kind.supports(SearchCapability::SearchKeywords) {
        return Err(SearchError::Config(format!(
            "Search provider {} does not support keyword search",
            call.provider.kind.label()
        )));
    }
    match call.provider.kind {
        SearchProviderKind::Zhipu => zhipu_search(call, query),
        SearchProviderKind::Tavily => tavily_search(call, query),
        SearchProviderKind::Searxng => searxng_search(call, query),
        SearchProviderKind::Exa => exa_search(call, query),
        SearchProviderKind::ExaMcp => exa_mcp_search(call, query),
        SearchProviderKind::Bocha => bocha_search(call, query),
        SearchProviderKind::Querit => querit_search(call, query),
        SearchProviderKind::Jina => jina_search(call, query),
        SearchProviderKind::Firecrawl => firecrawl_search(call, query),
        SearchProviderKind::Fetch => Err(SearchError::Config(
            "fetch only retrieves specified web pages; it does not perform keyword searches".to_owned(),
        )),
    }
}

pub(crate) fn fetch_url(
    call: &ProviderCall,
    target: &str,
) -> Result<Vec<SearchResultItem>, SearchError> {
    if !call.provider.kind.supports(SearchCapability::FetchUrls) {
        return Err(SearchError::Config(format!(
            "Search provider {} does not support web fetching",
            call.provider.kind.label()
        )));
    }
    match call.provider.kind {
        SearchProviderKind::Fetch => fetch_local(call, target),
        SearchProviderKind::Jina => jina_reader(call, target),
        SearchProviderKind::Querit => querit_contents(call, target),
        SearchProviderKind::Firecrawl => firecrawl_scrape(call, target),
        other => Err(SearchError::Config(format!(
            "Search provider {} does not support web fetching",
            other.label()
        ))),
    }
}

// ------------------------------------------------------------------ Keyword search

fn tavily_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/search")?;
    let payload = call.send(
        call.client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", call.key()?))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({ "query": query, "max_results": call.max_results() })),
    )?;
    Ok(array_of(&payload, &["results"])
        .iter()
        .take(call.max_results())
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: text_of(item, "content"),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

fn exa_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/search")?;
    let payload = call.send(
        call.client
            .post(url)
            .header("x-api-key", call.key()?)
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "query": query,
                "numResults": call.max_results(),
                "contents": { "text": true }
            })),
    )?;
    Ok(array_of(&payload, &["results"])
        .iter()
        .take(call.max_results())
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: text_of(item, "text"),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

/// Bocha accepts excluded domains upstream as a comma-separated request parameter.
/// Apply the host blacklist as well because it remains authoritative.
fn bocha_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/v1/web-search")?;
    let payload = call.send(
        call.client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", call.key()?))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "query": query,
                "count": call.max_results(),
                "exclude": call.execution.exclude_domains.join(","),
                "summary": true
            })),
    )?;
    if payload.get("code").and_then(Value::as_i64) != Some(200) {
        return Err(SearchError::Transient(format!(
            "Bocha search failed: {}",
            text_of(&payload, "msg")
        )));
    }
    Ok(array_of(&payload, &["data", "webPages", "value"])
        .iter()
        .map(|item| SearchResultItem {
            title: text_of(item, "name"),
            content: first_text(item, &["summary", "snippet"]),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

/// The Zhipu endpoint is a complete path, so use the configured host unchanged.
/// Zhipu has a dedicated credential slot because user-created model providers lack a
/// stable identity to share.
fn zhipu_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.host()?;
    let payload = call.send(
        call.client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", call.key()?))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "search_query": query,
                "search_engine": "search_std",
                "search_intent": false
            })),
    )?;
    Ok(array_of(&payload, &["search_result"])
        .iter()
        .take(call.max_results())
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: text_of(item, "content"),
            url: text_of(item, "link"),
            source_input: query.to_owned(),
        })
        .collect())
}

fn querit_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/v1/search")?;
    let mut body = json!({ "query": query, "count": call.max_results() });
    if !call.execution.exclude_domains.is_empty() {
        body["filters"] = json!({ "sites": { "exclude": call.execution.exclude_domains } });
    }
    let payload = call.send(
        call.client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", call.key()?))
            .header(CONTENT_TYPE, "application/json")
            .json(&body),
    )?;
    if payload.get("error_code").and_then(Value::as_i64) != Some(200) {
        return Err(SearchError::Transient(format!(
            "Querit search failed: {}",
            text_of(&payload, "error_msg")
        )));
    }
    Ok(array_of(&payload, &["results", "result"])
        .iter()
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: text_of(item, "snippet"),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

fn firecrawl_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/v2/search")?;
    let payload = call.send(firecrawl_auth(
        call,
        call.client.post(url).json(&json!({
            "query": query,
            "limit": call.max_results(),
            "scrapeOptions": { "formats": ["markdown"] }
        })),
    ))?;
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(SearchError::Transient(format!(
            "Firecrawl search failed: {}",
            first_text(&payload, &["error"])
        )));
    }
    Ok(array_of(&payload, &["data", "web"])
        .iter()
        .take(call.max_results())
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: first_text(item, &["markdown", "description"]),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

/// Firecrawl permits anonymous requests. Do not send an empty `Authorization` header,
/// which is authentication failure rather than anonymous access.
fn firecrawl_auth(call: &ProviderCall, request: RequestBuilder) -> RequestBuilder {
    let request = request.header(CONTENT_TYPE, "application/json");
    match call.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => request.header(AUTHORIZATION, format!("Bearer {key}")),
        None => request,
    }
}

/// Jina encodes the query in its search path. Built-in hosts retry against each other
/// instead of relying on a regional routing service.
fn jina_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let key = call.key()?.to_owned();
    let run = |host: &Url| -> Result<Value, SearchError> {
        let url = join_path(host, &urlencode(query))?;
        call.send(
            call.client
                .get(url)
                .header(ACCEPT, "application/json")
                .header(AUTHORIZATION, format!("Bearer {key}")),
        )
    };
    let payload = with_builtin_host_fallback(call, &JINA_SEARCH_HOSTS, run)?;
    let items = {
        let data = array_of(&payload, &["data"]);
        if data.is_empty() {
            array_of(&payload, &["results"])
        } else {
            data
        }
    };
    Ok(items
        .iter()
        .take(call.max_results())
        .map(|item| SearchResultItem {
            title: text_of(item, "title"),
            content: first_text(item, &["content", "description"]),
            url: text_of(item, "url"),
            source_input: query.to_owned(),
        })
        .collect())
}

/// SearXNG returns titles and links only, so its search path fetches page content.
fn searxng_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let engines = resolve_searxng_engines(call)?;
    let mut url = call.endpoint("/search")?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("language", "auto")
        .append_pair("format", "json")
        .append_pair("engines", &engines.join(","));
    let payload = call.send(searxng_auth(call, call.client.get(url)))?;
    let targets: Vec<String> = array_of(&payload, &["results"])
        .iter()
        .map(|item| text_of(item, "url"))
        .filter(|url| is_http_url(url))
        .take(call.max_results())
        .collect();
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let pages = super::pipeline::map_in_parallel(targets, |target| {
        // A search engine chose these URLs, not the user, so a result that
        // resolves inward is refused however the conversation is configured.
        readable::fetch_readable(&target, readable::LocalTargetPolicy::Deny)
            .map(|page| (target, page))
    });
    let mut results = Vec::new();
    let mut first_error = None;
    for outcome in pages {
        match outcome {
            Ok((target, page)) => {
                if page.content.trim().is_empty() {
                    continue;
                }
                results.push(SearchResultItem {
                    title: page.title,
                    content: page.content,
                    url: page.url.clone().max(target),
                    source_input: query.to_owned(),
                });
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    // Report the first fetch error when no page can be retrieved; an empty result
    // would otherwise look like a successful search with no matches.
    if results.is_empty() {
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok(results)
}

fn resolve_searxng_engines(call: &ProviderCall) -> Result<Vec<String>, SearchError> {
    if !call.provider.engines.is_empty() {
        return Ok(call.provider.engines.clone());
    }
    let url = call.endpoint("/config")?;
    let payload = call.send(searxng_auth(call, call.client.get(url)))?;
    let engines: Vec<String> = array_of(&payload, &["engines"])
        .iter()
        .filter(|engine| {
            let enabled = engine.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let categories = array_of(engine, &["categories"]);
            let has = |name: &str| categories.iter().any(|value| value.as_str() == Some(name));
            enabled && has("general") && has("web")
        })
        .map(|engine| text_of(engine, "name"))
        .filter(|name| !name.is_empty())
        .collect();
    if engines.is_empty() {
        return Err(SearchError::Config(
            "This SearXNG instance has no enabled general web-search engines; configure an engine list in provider settings".to_owned(),
        ));
    }
    Ok(engines)
}

fn searxng_auth(call: &ProviderCall, request: RequestBuilder) -> RequestBuilder {
    match call.basic_auth_header() {
        Some(header) => request.header(AUTHORIZATION, header),
        None => request,
    }
}

/// Exa's MCP endpoint uses a single JSON-RPC `tools/call` POST without an MCP session.
/// It accepts either SSE or a complete JSON response.
fn exa_mcp_search(call: &ProviderCall, query: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.host()?;
    let mut request = call
        .client
        .post(url)
        .timeout(EXA_MCP_TIMEOUT)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "type": "auto",
                    "numResults": call.max_results(),
                    "livecrawl": "fallback"
                }
            }
        }));
    if let Some(key) = call.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()) {
        request = request.header("x-api-key", key);
    }
    let body = call.send_text(request)?;
    Ok(parse_exa_mcp_payload(&body)
        .into_iter()
        .take(call.max_results())
        .map(|(title, url, text)| SearchResultItem {
            title,
            content: text,
            url,
            source_input: query.to_owned(),
        })
        .collect())
}

/// Parses SSE or JSON payloads into `result.content[].text` blocks.
fn parse_exa_mcp_payload(body: &str) -> Vec<(String, String, String)> {
    let mut chunks: Vec<String> = Vec::new();
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Some(text) = mcp_content_text(payload) {
            chunks.push(text);
        }
    }
    if chunks.is_empty() {
        if let Some(text) = mcp_content_text(body) {
            chunks.push(text);
        }
    }
    if chunks.is_empty() && body.contains("Title:") {
        chunks.push(body.to_owned());
    }
    parse_exa_text_chunks(&chunks.join("\n\n"))
}

fn mcp_content_text(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let text = array_of(&value, &["result", "content"])
        .iter()
        .map(|item| text_of(item, "text"))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

fn parse_exa_text_chunks(raw: &str) -> Vec<(String, String, String)> {
    let mut items = Vec::new();
    for chunk in raw.split("\n\n") {
        let lines: Vec<&str> = chunk.split('\n').collect();
        let mut title = String::new();
        let mut url = String::new();
        let mut text = String::new();
        let mut text_start = None;
        for (index, line) in lines.iter().enumerate() {
            if let Some(rest) = line.strip_prefix("Title:") {
                title = rest.trim().to_owned();
            } else if let Some(rest) = line.strip_prefix("URL:") {
                url = rest.trim().to_owned();
            } else if let Some(rest) = line.strip_prefix("Text:") {
                if text_start.is_none() {
                    text_start = Some(index);
                    text = rest.trim().to_owned();
                }
            }
        }
        if let Some(start) = text_start {
            let rest = lines[start + 1..].join("\n");
            if !rest.trim().is_empty() {
                text = if text.is_empty() {
                    rest
                } else {
                    format!("{text}\n{rest}")
                };
            }
        }
        if !title.is_empty() || !url.is_empty() || !text.is_empty() {
            items.push((title, url, text));
        }
    }
    items
}

// ------------------------------------------------------------------ Fetch

fn fetch_local(call: &ProviderCall, target: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let policy = if call.allow_local_targets {
        readable::LocalTargetPolicy::AllowDirect
    } else {
        readable::LocalTargetPolicy::Deny
    };
    let ReadablePage { title, content, url } = readable::fetch_readable(target, policy)?;
    Ok(vec![SearchResultItem {
        title,
        content,
        url,
        source_input: target.to_owned(),
    }])
}

fn jina_reader(call: &ProviderCall, target: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    // Reader expects the raw URL appended to the host; encoding changes its path semantics.
    let key = call
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned);
    let run = |host: &Url| -> Result<Value, SearchError> {
        let url = Url::parse(&format!(
            "{}/{target}",
            host.as_str().trim_end_matches('/')
        ))
        .map_err(|error| SearchError::Config(format!("Could not construct Jina Reader URL: {error}")))?;
        let mut request = call
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .header("X-Retain-Images", "none");
        if let Some(key) = key.as_deref() {
            request = request.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        call.send(request)
    };
    let payload = with_builtin_host_fallback(call, &JINA_READER_HOSTS, run)?;
    let data = payload.get("data").unwrap_or(&payload);
    let content = first_text(data, &["content", "text"]);
    if content.is_empty() {
        return Err(SearchError::Transient(format!(
            "Jina Reader returned no content for {target}"
        )));
    }
    let title = text_of(data, "title");
    let url = text_of(data, "url");
    Ok(vec![SearchResultItem {
        title: if title.is_empty() { target.to_owned() } else { title },
        content,
        url: if url.is_empty() { target.to_owned() } else { url },
        source_input: target.to_owned(),
    }])
}

fn querit_contents(call: &ProviderCall, target: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/v1/contents")?;
    let payload = call.send(
        call.client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", call.key()?))
            .header(CONTENT_TYPE, "application/json")
            // Querit returns a title only when metadata is explicitly requested.
            .json(&json!({ "urls": [target], "format": "markdown", "extrasMeta": true })),
    )?;
    if payload.get("error_code").and_then(Value::as_i64) != Some(200) {
        return Err(SearchError::Transient(format!(
            "Querit fetch failed: {}",
            text_of(&payload, "error_msg")
        )));
    }
    let page = array_of(&payload, &["results"])
        .first()
        .cloned()
        .unwrap_or(Value::Null);
    let content = text_of(&page, "content");
    if content.is_empty() {
        return Err(SearchError::Transient(format!(
            "Querit returned no content for {target}"
        )));
    }
    let title = page
        .get("extrasMeta")
        .map(|meta| text_of(meta, "title"))
        .unwrap_or_default();
    let url = text_of(&page, "url");
    Ok(vec![SearchResultItem {
        title: if title.is_empty() { target.to_owned() } else { title },
        content,
        url: if url.is_empty() { target.to_owned() } else { url },
        source_input: target.to_owned(),
    }])
}

fn firecrawl_scrape(call: &ProviderCall, target: &str) -> Result<Vec<SearchResultItem>, SearchError> {
    let url = call.endpoint("/v2/scrape")?;
    let payload = call.send(firecrawl_auth(
        call,
        call.client
            .post(url)
            .json(&json!({ "url": target, "formats": ["markdown"] })),
    ))?;
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(SearchError::Transient(format!(
            "Firecrawl fetch failed: {}",
            first_text(&payload, &["error"])
        )));
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let content = text_of(&data, "markdown");
    if content.is_empty() {
        return Err(SearchError::Transient(format!(
            "Firecrawl returned no content for {target}"
        )));
    }
    let metadata = data.get("metadata").cloned().unwrap_or(Value::Null);
    // Firecrawl declares title as `string | string[]`; accept both forms.
    let title = match metadata.get("title") {
        Some(Value::String(title)) => title.trim().to_owned(),
        Some(Value::Array(titles)) => titles
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => String::new(),
    };
    let url = text_of(&metadata, "sourceURL");
    Ok(vec![SearchResultItem {
        title: if title.is_empty() { target.to_owned() } else { title },
        content,
        url: if url.is_empty() { target.to_owned() } else { url },
        source_input: target.to_owned(),
    }])
}

// ------------------------------------------------------------------ Utilities

/// Retry a failed request against the paired built-in host only. User-configured hosts
/// must not be silently redirected.
fn with_builtin_host_fallback(
    call: &ProviderCall,
    builtin: &[&str],
    run: impl Fn(&Url) -> Result<Value, SearchError>,
) -> Result<Value, SearchError> {
    let host = call.host()?;
    let configured = host.as_str().trim_end_matches('/').to_owned();
    let first = run(&host);
    let Err(error) = first else {
        return first;
    };
    // Retrying cannot fix configuration errors, and cancellation must propagate unchanged.
    if !matches!(error, SearchError::Transient(_)) {
        return Err(error);
    }
    let Some(alternate) = builtin
        .iter()
        .find(|candidate| !candidate.eq_ignore_ascii_case(&configured))
        .filter(|_| builtin.iter().any(|candidate| candidate.eq_ignore_ascii_case(&configured)))
    else {
        return Err(error);
    };
    let alternate = parse_api_host(call.provider.kind, alternate)?;
    run(&alternate)
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

pub(crate) fn is_http_url(value: &str) -> bool {
    Url::parse(value.trim()).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_api_host_keeps_its_own_path_prefix_when_an_endpoint_is_appended() {
        let host = Url::parse("https://gateway.example/tavily").expect("test url");
        assert_eq!(
            join_path(&host, "/search").expect("join").as_str(),
            "https://gateway.example/tavily/search"
        );
        let bare = Url::parse("https://api.tavily.com").expect("test url");
        assert_eq!(
            join_path(&bare, "/search").expect("join").as_str(),
            "https://api.tavily.com/search"
        );
    }

    #[test]
    fn http_is_only_allowed_on_loopback() {
        assert!(parse_api_host(SearchProviderKind::Searxng, "http://localhost:8080").is_ok());
        assert!(parse_api_host(SearchProviderKind::Searxng, "http://127.0.0.1:8080").is_ok());
        assert!(parse_api_host(SearchProviderKind::Searxng, "http://searx.example").is_err());
        assert!(parse_api_host(SearchProviderKind::Tavily, "https://user:pw@api.tavily.com").is_err());
        assert!(parse_api_host(SearchProviderKind::Tavily, "").is_err());
    }

    #[test]
    fn the_exa_mcp_payload_parses_from_sse_lines_and_from_a_plain_body() {
        let sse = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"Title: A\\nURL: https://a.example\\nText: alpha\"}]}}\ndata: [DONE]\n";
        let parsed = parse_exa_mcp_payload(sse);
        assert_eq!(
            parsed,
            vec![(
                "A".to_owned(),
                "https://a.example".to_owned(),
                "alpha".to_owned()
            )]
        );

        let plain = "{\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"Title: B\\nURL: https://b.example\\nText: beta\\nmore\"}]}}";
        let parsed = parse_exa_mcp_payload(plain);
        assert_eq!(parsed[0].0, "B");
        assert_eq!(parsed[0].2, "beta\nmore");
    }

    #[test]
    fn a_query_is_percent_encoded_for_the_jina_search_path() {
        assert_eq!(urlencode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn first_text_walks_the_fallback_chain() {
        let value = json!({ "summary": "  ", "snippet": " s " });
        assert_eq!(first_text(&value, &["summary", "snippet"]), "s");
        assert_eq!(first_text(&value, &["nope"]), "");
    }

    // ------------------------------------------------------- Wire protocol fixtures
    //
    // These tests use real HTTP against a local `TcpListener`, including raw request
    // lines, authentication headers, request bodies, and response parsing. Credentials
    // are held directly in `ProviderCall`, so user configuration is untouched.

    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serves one fixed JSON response and returns the raw received request.
    fn serve_once(body: &'static str) -> (std::net::SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("read timeout");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received).into_owned();
                let Some((headers, rest)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if rest.len() >= length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("write response");
            stream.flush().ok();
            String::from_utf8_lossy(&received).into_owned()
        });
        (address, handle)
    }

    fn call_against(
        address: std::net::SocketAddr,
        kind: SearchProviderKind,
        api_key: Option<&str>,
    ) -> ProviderCall {
        ProviderCall {
            client: crate::http_util::client().expect("client"),
            provider: ResolvedSearchProvider {
                kind,
                api_host: format!("http://{address}"),
                engines: Vec::new(),
                basic_auth_username: String::new(),
            },
            execution: SearchExecutionConfig {
                max_results: 2,
                exclude_domains: Vec::new(),
                compression: Default::default(),
            },
            api_key: api_key.map(str::to_owned),
            basic_auth_password: None,
            // These fixtures serve from loopback, so the direct-fetch paths under
            // test must be allowed to reach it.
            allow_local_targets: true,
        }
    }

    #[test]
    fn tavily_sends_a_bearer_key_and_maps_its_result_rows() {
        let (address, server) = serve_once(
            r#"{"query":"q","request_id":"r","response_time":1,"results":[
                {"title":" A ","content":" alpha ","url":"https://a.example"},
                {"title":"B","content":"beta","url":"https://b.example"},
                {"title":"C","content":"gamma","url":"https://c.example"}
            ]}"#,
        );
        let call = call_against(address, SearchProviderKind::Tavily, Some("sk-tavily"));
        let results = tavily_search(&call, "q").expect("tavily search");
        let request = server.join().expect("fixture thread");

        assert!(request.starts_with("POST /search "), "{request}");
        assert!(
            request.contains("Authorization: Bearer sk-tavily")
                || request.contains("authorization: Bearer sk-tavily"),
            "the authorization header must be sent on the wire: {request}"
        );
        assert!(request.contains("\"max_results\":2"), "{request}");
        // Enforce `max_results` locally even if the upstream exceeds the requested limit.
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].content, "alpha");
        assert_eq!(results[0].url, "https://a.example");
        assert_eq!(results[0].source_input, "q");
    }

    #[test]
    fn a_missing_required_key_fails_before_any_request_is_sent() {
        // No listener exists, proving validation must prevent any connection attempt.
        let call = call_against("127.0.0.1:1".parse().unwrap(), SearchProviderKind::Tavily, None);
        let error = tavily_search(&call, "q").expect_err("a missing API key must prevent the request");
        assert!(matches!(error, SearchError::Config(_)), "{error:?}");
    }

    #[test]
    fn firecrawl_stays_anonymous_without_a_key_and_reads_its_web_rows() {
        let (address, server) = serve_once(
            r#"{"success":true,"data":{"web":[
                {"title":"A","markdown":"alpha","url":"https://a.example"},
                {"title":"B","description":"beta","url":"https://b.example"}
            ]}}"#,
        );
        let call = call_against(address, SearchProviderKind::Firecrawl, None);
        let results = firecrawl_search(&call, "q").expect("firecrawl search");
        let request = server.join().expect("fixture thread");

        assert!(request.starts_with("POST /v2/search "), "{request}");
        // Anonymous access must omit `Authorization`; an empty header is an auth failure.
        assert!(
            !request.to_ascii_lowercase().contains("authorization:"),
            "anonymous requests must not include an authorization header: {request}"
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "alpha");
        // Fall back to `description` when markdown is absent.
        assert_eq!(results[1].content, "beta");
    }

    #[test]
    fn an_upstream_status_code_becomes_a_transient_failure_not_a_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = "{\"error\":\"rate limited\"}";
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        let call = call_against(address, SearchProviderKind::Firecrawl, None);
        let error = firecrawl_search(&call, "q").expect_err("429 is not success");
        let _ = server.join();
        assert!(matches!(error, SearchError::Transient(_)), "{error:?}");
    }

    #[test]
    fn bocha_reports_its_in_band_error_code_instead_of_returning_nothing() {
        let (address, server) = serve_once(
            r#"{"code":403,"msg":"quota exhausted","data":{"queryContext":{"originalQuery":"q"},"webPages":{"value":[]}}}"#,
        );
        let call = call_against(address, SearchProviderKind::Bocha, Some("sk-bocha"));
        let error = bocha_search(&call, "q").expect_err("an in-band error code must become a failure");
        let _ = server.join();
        // Bocha uses HTTP 200 with `code != 200` for failures; treating it as an
        // empty result would hide exhausted quota.
        assert!(error.message().contains("quota exhausted"), "{error:?}");
    }
}
