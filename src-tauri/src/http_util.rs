//! Protocol-independent HTTP primitives.
//!
//! This layer normalizes user-provided Base URLs into safe request targets, performs ordinary HTTP
//! round trips with bounded response reads, and sanitizes error text.
//!
//! Consumers include `api.rs` for safety, credentials, and connectivity checks;
//! `model_discovery.rs` for provider catalog endpoints; and `web_search/{providers,pipeline}.rs`
//! for host-managed search APIs.

use std::{sync::OnceLock, time::Duration};

use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
    StatusCode, Url,
};
use serde_json::Value;
use std::io::Read;

pub(crate) const API_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) const MAX_ERROR_BODY: usize = 64 * 1024;
pub(crate) const MAX_SUCCESS_BODY: usize = 16 * 1024 * 1024;

pub(crate) fn custom_endpoint_url(
    base_url: &str,
    drop_trailing: &[&str],
    path: &str,
    query: Option<&str>,
) -> Result<Url, String> {
    let mut url = normalized_base_url(base_url)?;
    let mut segments = url
        .path_segments()
        .ok_or_else(|| "Base URL 必须是分层 HTTP URL".to_owned())?
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for candidate in drop_trailing {
        if segments
            .last()
            .is_some_and(|segment| segment.eq_ignore_ascii_case(candidate))
        {
            segments.pop();
        }
    }
    segments.extend(
        path.split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned),
    );
    url.set_path(&format!("/{}", segments.join("/")));
    url.set_query(query);
    Ok(url)
}

/// Normalizes a user-provided Base URL through safety validation and endpoint-suffix removal.
///
/// Enforces host policy: HTTPS except on loopback, no credentials, and removal of pasted endpoint
/// segments. The sidecar must receive this normalized `baseURL`, because it appends protocol paths
/// such as `/responses`, `/messages`, and `/chat/completions` itself.
pub(crate) fn normalized_base_url(base_url: &str) -> Result<Url, String> {
    let mut url = Url::parse(base_url.trim()).map_err(|_| "Base URL 无效".to_owned())?;
    validate_url_security(&url)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Base URL 不得包含用户名或密码".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Base URL 不得包含查询参数或片段".into());
    }

    let mut segments = url
        .path_segments()
        .ok_or_else(|| "Base URL 必须是分层 HTTP URL".to_owned())?
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    strip_known_endpoint(&mut segments);
    if segments.is_empty() {
        url.set_path("/");
    } else {
        url.set_path(&format!("/{}", segments.join("/")));
    }
    Ok(url)
}

/// Removes an endpoint suffix the user pasted along with the base URL.
///
/// Only suffixes that name an endpoint in a protocol Mework speaks are removed.
/// Anything else is a path segment the relay routes on: a base mounted at
/// `https://relay.example.com/tenant/provider` must keep `provider`, or every
/// request lands on the wrong tenant.
fn strip_known_endpoint(segments: &mut Vec<String>) {
    let lower = segments
        .iter()
        .map(|segment| segment.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let strip = if lower.ends_with(&["chat".into(), "completions".into()])
        || lower.ends_with(&["responses".into(), "compact".into()])
    {
        2
    } else if lower.last().is_some_and(|segment| {
        matches!(
            segment.as_str(),
            "models" | "responses" | "messages" | "completions"
        )
    }) {
        1
    } else {
        0
    };
    if strip > 0 {
        segments.truncate(segments.len() - strip);
    }
}

fn validate_url_security(url: &Url) -> Result<(), String> {
    match url.scheme() {
        "https" => {}
        "http" if is_local_network_url(url) => {}
        "http" => return Err("远程 API 必须使用 HTTPS；HTTP 仅允许本机与局域网地址".into()),
        _ => return Err("Base URL 仅支持 HTTPS，或本机与局域网地址上的 HTTP".into()),
    }
    if url.host_str().is_none() {
        return Err("Base URL 缺少主机名".into());
    }
    Ok(())
}

/// Whether plain HTTP is acceptable for this host.
///
/// HTTPS stays mandatory for the public internet, where a plaintext API key is
/// readable by every hop in between. On an address that cannot be routed off the
/// local network that protection buys nothing, while requiring it makes the
/// services people actually run locally — Ollama, LM Studio, an HTTP MCP server
/// on another machine in the house — impossible to configure, because they
/// overwhelmingly ship without TLS.
///
/// This is deliberately an address question, not a trust question: a name that
/// resolves to a private address is not checked here, and no DNS lookup happens.
pub(crate) fn is_local_network_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            // A trailing dot is the same name in absolute form.
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost"
                || domain.ends_with(".localhost")
                // mDNS names only resolve on the local link.
                || domain == "local"
                || domain.ends_with(".local")
        }
        Some(url::Host::Ipv4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        Some(url::Host::Ipv6(address)) => {
            address.is_loopback()
                // fc00::/7 unique-local and fe80::/10 link-local.
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
                || address.to_ipv4_mapped().is_some_and(|mapped| {
                    mapped.is_loopback() || mapped.is_private() || mapped.is_link_local()
                })
        }
        None => false,
    }
}

/// Redirect hops followed before giving up. Gateways in front of a relay usually
/// need one hop to canonicalize a path; more than a few means a loop.
const MAX_REDIRECTS: usize = 5;

/// Follows only same-origin redirects.
///
/// Refusing every redirect breaks ordinary relay deployments, where a gateway
/// canonicalizes `/v1/models` to `/v1/models/` and answers `301`. Following every
/// redirect is worse: request builders attach credentials as `x-api-key`,
/// `x-goog-api-key`, and `api-key`, which `reqwest` does not strip on a
/// cross-host hop, so an upstream that redirects elsewhere would hand a
/// user's key to that host.
///
/// Same-origin means identical scheme, host, and port. A cross-origin hop stops
/// and surfaces the `3xx` itself, exactly as before.
fn same_origin_redirect() -> Policy {
    Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("重定向次数过多");
        }
        let Some(previous) = attempt.previous().last() else {
            return attempt.stop();
        };
        let same_origin = attempt.url().scheme() == previous.scheme()
            && attempt.url().host_str() == previous.host_str()
            && attempt.url().port_or_known_default() == previous.port_or_known_default();
        if same_origin && validate_url_security(attempt.url()).is_ok() {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub(crate) fn client() -> Result<Client, String> {
    // The shared reqwest client retains an Arc-backed connection pool, allowing neighboring
    // requests to reuse TCP/TLS connections.
    static SHARED: OnceLock<Client> = OnceLock::new();
    if let Some(client) = SHARED.get() {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .timeout(API_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(same_origin_redirect())
        .user_agent("Mework/0.1")
        .build()
        .map_err(|error| format!("无法初始化 API 客户端: {error}"))?;
    Ok(SHARED.get_or_init(|| client).clone())
}

pub(crate) fn read_body(mut response: Response, limit: usize) -> Result<(Vec<u8>, bool), String> {
    let mut body = Vec::new();
    response
        .by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| format!("读取 API 响应失败: {error}"))?;
    let overflowed = body.len() > limit;
    if overflowed {
        body.truncate(limit);
    }
    Ok((body, overflowed))
}

pub(crate) fn api_error_message(status: StatusCode, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("error"))
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| text.trim().to_owned());
    if detail.is_empty() {
        format!("API 请求失败（HTTP {}）", status.as_u16())
    } else {
        format!("API 请求失败（HTTP {}）：{detail}", status.as_u16())
    }
}

pub(crate) fn sanitize_error(message: &str, key: Option<&str>) -> String {
    let without_key = match key.filter(|key| !key.is_empty()) {
        Some(key) => message.replace(key, "[REDACTED]"),
        None => message.to_owned(),
    };
    redact_inline_encoded_data(&without_key)
}

pub(crate) fn redact_inline_encoded_data(message: &str) -> String {
    let mut data_url_redacted = String::with_capacity(message.len());
    let mut cursor = 0;
    while let Some(relative_start) = message[cursor..].find("data:image/") {
        let start = cursor + relative_start;
        data_url_redacted.push_str(&message[cursor..start]);
        let Some(relative_marker) = message[start..].find(";base64,") else {
            data_url_redacted.push_str("data:image/");
            cursor = start + "data:image/".len();
            continue;
        };
        if relative_marker > 96 {
            data_url_redacted.push_str("data:image/");
            cursor = start + "data:image/".len();
            continue;
        }
        let payload_start = start + relative_marker + ";base64,".len();
        let mut payload_len = 0;
        let mut has_encoded_byte = false;
        let mut padding_started = false;
        for byte in &message.as_bytes()[payload_start..] {
            if is_inline_encoded_byte(*byte) {
                if padding_started && *byte != b'=' {
                    break;
                }
                has_encoded_byte = true;
                padding_started |= *byte == b'=';
                payload_len += 1;
            } else if byte.is_ascii_whitespace() {
                payload_len += 1;
            } else {
                break;
            }
        }
        if !has_encoded_byte {
            data_url_redacted.push_str(&message[start..payload_start]);
            cursor = payload_start;
            continue;
        }
        data_url_redacted.push_str("<image data URL redacted>");
        cursor = payload_start + payload_len;
    }
    data_url_redacted.push_str(&message[cursor..]);

    let bytes = data_url_redacted.as_bytes();
    let mut encoded_redacted = String::with_capacity(data_url_redacted.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !is_inline_encoded_byte(bytes[cursor]) {
            let next = cursor
                + data_url_redacted[cursor..]
                    .chars()
                    .next()
                    .unwrap()
                    .len_utf8();
            encoded_redacted.push_str(&data_url_redacted[cursor..next]);
            cursor = next;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && is_inline_encoded_byte(bytes[cursor]) {
            cursor += 1;
        }
        if cursor - start >= 256 {
            encoded_redacted.push_str("<large encoded data redacted>");
        } else {
            encoded_redacted.push_str(&data_url_redacted[start..cursor]);
        }
    }
    encoded_redacted
}

fn is_inline_encoded_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(url: &str) -> bool {
        is_local_network_url(&Url::parse(url).expect("fixture URL parses"))
    }

    /// This predicate decides where a plaintext API key may travel, so both
    /// directions are pinned: everything it accepts is unroutable off the local
    /// network, and nothing public may slip through a spelling of a local name.
    #[test]
    fn local_network_covers_loopback_private_and_link_local() {
        for url in [
            "http://localhost:11434",
            "http://LOCALHOST:11434",
            "http://api.localhost/v1",
            "http://localhost./v1",
            "http://127.0.0.1:11434",
            "http://127.42.0.8/v1",
            "http://10.0.0.5/v1",
            "http://172.16.3.9/v1",
            "http://172.31.255.1/v1",
            "http://192.168.1.10:11434",
            "http://169.254.10.1/v1",
            "http://ollama.local:11434",
            "http://[::1]:11434",
            "http://[fd00::1]/v1",
            "http://[fe80::1]/v1",
            "http://[::ffff:192.168.1.10]/v1",
        ] {
            assert!(local(url), "expected local network: {url}");
        }
    }

    #[test]
    fn local_network_rejects_public_hosts() {
        for url in [
            "https://api.openai.com/v1",
            "http://example.com/v1",
            "http://93.184.216.34/v1",
            // Adjacent to but outside 172.16/12.
            "http://172.15.0.1/v1",
            "http://172.32.0.1/v1",
            // A public name that merely mentions a local one.
            "http://localhost.example.com/v1",
            "http://notlocal/v1",
            "http://[2606:4700::1111]/v1",
        ] {
            assert!(!local(url), "expected public host: {url}");
        }
    }

    #[test]
    fn plaintext_http_is_accepted_only_on_the_local_network() {
        assert!(normalized_base_url("http://192.168.1.10:11434/v1").is_ok());
        assert!(normalized_base_url("http://localhost:11434/v1").is_ok());
        assert!(normalized_base_url("https://api.openai.com/v1").is_ok());
        let error = normalized_base_url("http://api.openai.com/v1").unwrap_err();
        assert!(error.contains("HTTPS"), "{error}");
    }
}
