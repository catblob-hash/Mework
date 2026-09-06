//! Fetch a local page and extract readable text.
//!
//! This deliberately conservative extractor removes non-content subtrees, strips
//! remaining tags, decodes entities, and collapses whitespace. It avoids DOM
//! dependencies and malformed-page failures at the cost of retaining some
//! navigation and footer text.
//!
//! Targets from models and search results are untrusted. Requests allow only
//! HTTP(S) URLs without userinfo, reject every private-resolution result, and pin
//! the client connection to an approved address to prevent DNS rebinding.
//! Redirects are followed manually so each hop receives the same checks.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use url::{Host, Url};

use super::SearchError;

/// Maximum number of redirects to follow.
const MAX_REDIRECTS: usize = 5;
/// Byte cap for one fetched page.
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
/// Character cap after extraction and before cutoff compression.
const MAX_PAGE_CHARS: usize = 200_000;
const PAGE_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Browser-like user agent required by sites that return empty shells otherwise.
const PAGE_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub(crate) struct ReadablePage {
    pub title: String,
    pub content: String,
    pub url: String,
}

/// Whether this fetch may reach an address that is not publicly routable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalTargetPolicy {
    /// Refuse every private address. This is the rule for any URL the user did
    /// not name: a search result, or a redirect chosen by the page.
    Deny,
    /// Accept a private address that the caller named directly. Reaching a
    /// development server on this machine is the whole point of the request.
    AllowDirect,
}

/// Whether an address is private or otherwise unsafe for host-side fetching.
fn is_private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // CGNAT and benchmarking ranges are not publicly routable.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                || (v4.octets()[0] == 198 && (18..20).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 is unique-local and fe80::/10 is link-local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private_address(IpAddr::V4(mapped)))
        }
    }
}

/// Validate a target URL and resolve its host to an approved address.
fn resolve_public_target(url: &Url, policy: LocalTargetPolicy) -> Result<SocketAddr, SearchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SearchError::Config(format!(
            "Only HTTP(S) URLs can be fetched: {url}"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SearchError::Config(
            "Fetch URL must not include a username or password".to_owned(),
        ));
    }
    let host = url
        .host()
        .ok_or_else(|| SearchError::Config(format!("Fetch URL has no host: {url}")))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| SearchError::Config(format!("Fetch URL has no port: {url}")))?;
    let candidates: Vec<SocketAddr> = match host {
        Host::Ipv4(address) => vec![SocketAddr::new(IpAddr::V4(address), port)],
        Host::Ipv6(address) => vec![SocketAddr::new(IpAddr::V6(address), port)],
        Host::Domain(domain) => (domain, port)
            .to_socket_addrs()
            .map_err(|error| SearchError::Transient(format!("Could not resolve {domain}: {error}")))?
            .collect(),
    };
    if candidates.is_empty() {
        return Err(SearchError::Transient(format!(
            "{url} did not resolve to any address"
        )));
    }
    // Reject the entire target if any result is private. Choosing one public
    // address would let a rebinding attacker choose the connection target.
    if let Some(private) = candidates
        .iter()
        .find(|address| is_private_address(address.ip()))
    {
        if policy == LocalTargetPolicy::Deny {
            return Err(SearchError::Config(format!(
                "Refused to fetch a target resolving to a private-network address ({}): {url}",
                private.ip()
            )));
        }
    }
    Ok(candidates[0])
}

fn pinned_client(host: &str, address: SocketAddr) -> Result<Client, SearchError> {
    Client::builder()
        .timeout(PAGE_TIMEOUT)
        .connect_timeout(PAGE_CONNECT_TIMEOUT)
        // Validate every redirect hop manually.
        .redirect(Policy::none())
        .user_agent(PAGE_USER_AGENT)
        .resolve(host, address)
        .build()
        .map_err(|error| SearchError::Transient(format!("Could not initialize fetch client: {error}")))
}

/// Fetch a page and extract readable text.
///
/// `policy` applies to the target the caller named. Every redirect hop is
/// resolved under [`LocalTargetPolicy::Deny`] regardless: the page chose those,
/// not the user, and a public URL that redirects to `127.0.0.1` is the ordinary
/// shape of a server-side request forgery.
pub(crate) fn fetch_readable(
    target: &str,
    policy: LocalTargetPolicy,
) -> Result<ReadablePage, SearchError> {
    let mut url = Url::parse(target.trim())
        .map_err(|error| SearchError::Config(format!("Invalid fetch URL {target}: {error}")))?;
    let mut hop_policy = policy;
    for _ in 0..=MAX_REDIRECTS {
        let address = resolve_public_target(&url, hop_policy)?;
        hop_policy = LocalTargetPolicy::Deny;
        let host = url
            .host_str()
            .ok_or_else(|| SearchError::Config(format!("Fetch URL has no host: {url}")))?
            .to_owned();
        let client = pinned_client(&host, address)?;
        let response = client
            .get(url.clone())
            .send()
            .map_err(|error| SearchError::Transient(format!("Fetch {url} failed: {error}")))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    SearchError::Transient(format!("{url} returned {status} without a Location header"))
                })?;
            url = url.join(location).map_err(|error| {
                SearchError::Transient(format!("{url} has an invalid redirect target: {error}"))
            })?;
            continue;
        }
        if status != StatusCode::OK && !status.is_success() {
            return Err(SearchError::Transient(format!("Fetch {url} returned {status}")));
        }
        let bytes = response
            .bytes()
            .map_err(|error| SearchError::Transient(format!("Failed to read content from {url}: {error}")))?;
        let html = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PAGE_BYTES)]);
        let (title, content) = extract(&html);
        if content.trim().is_empty() {
            return Err(SearchError::Transient(format!("{url} has no extractable content")));
        }
        return Ok(ReadablePage {
            title: if title.trim().is_empty() {
                url.to_string()
            } else {
                title
            },
            content,
            url: url.to_string(),
        });
    }
    Err(SearchError::Transient(format!(
        "Fetch redirects exceeded {MAX_REDIRECTS} hops: {target}"
    )))
}

/// Subtrees that cannot contain readable text.
const DROPPED_ELEMENTS: [&str; 6] = ["script", "style", "noscript", "svg", "iframe", "template"];
/// Block-level elements converted to line breaks.
const BLOCK_ELEMENTS: [&str; 22] = [
    "address", "article", "aside", "blockquote", "br", "div", "footer", "h1", "h2", "h3", "h4",
    "h5", "h6", "header", "hr", "li", "nav", "ol", "p", "pre", "section", "tr",
];

/// Convert HTML to a title and readable text.
pub(crate) fn extract(html: &str) -> (String, String) {
    let without_comments = strip_between(html, "<!--", "-->");
    let title = first_element_text(&without_comments, "title");
    let mut body = without_comments;
    for element in DROPPED_ELEMENTS {
        body = strip_element(&body, element);
    }
    // Remove `head` and `title` only when their closing tags exist: implicit
    // `</head>` is valid HTML, and removing the rest of that page would erase it.
    for element in ["head", "title"] {
        body = strip_element_when_closed(&body, element);
    }
    let mut text = String::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            let Some(end) = body[index..].find('>') else {
                break;
            };
            let tag = &body[index + 1..index + end];
            let name = tag_name(tag);
            if BLOCK_ELEMENTS.contains(&name.as_str()) {
                text.push('\n');
            }
            index += end + 1;
            continue;
        }
        let next = body[index..].find('<').map_or(body.len(), |at| index + at);
        text.push_str(&body[index..next]);
        index = next;
    }
    (decode_entities(&title), collapse(&decode_entities(&text)))
}

/// Convert `<div class="x">` and `</div>` to `div`.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn strip_between(source: &str, open: &str, close: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        output.push_str(&rest[..start]);
        match rest[start + open.len()..].find(close) {
            Some(end) => rest = &rest[start + open.len() + end + close.len()..],
            // An unclosed segment consumes the remaining source.
            None => return output,
        }
    }
    output.push_str(rest);
    output
}

fn strip_element(source: &str, element: &str) -> String {
    let open = format!("<{element}");
    let close = format!("</{element}>");
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    loop {
        let Some(start) = find_ascii_case_insensitive(rest, &open) else {
            output.push_str(rest);
            return output;
        };
        // `<scriptish>` is not `<script>`: the opening name must be followed by
        // whitespace, `>`, or `/`.
        let after = rest[start + open.len()..].chars().next();
        if !matches!(after, Some(character) if character.is_whitespace() || character == '>' || character == '/')
        {
            let advance = start + open.len();
            output.push_str(&rest[..advance]);
            rest = &rest[advance..];
            continue;
        }
        output.push_str(&rest[..start]);
        output.push('\n');
        match find_ascii_case_insensitive(&rest[start..], &close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => return output,
        }
    }
}

/// Remove an element only when both its opening and closing tags exist.
///
/// Unlike an unclosed script, an implicit `</head>` is valid HTML and must not
/// cause the remainder of the page to be discarded.
fn strip_element_when_closed(source: &str, element: &str) -> String {
    let open = format!("<{element}");
    let close = format!("</{element}>");
    if find_ascii_case_insensitive(source, &open).is_none()
        || find_ascii_case_insensitive(source, &close).is_none()
    {
        return source.to_owned();
    }
    strip_element(source, element)
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack_lower = haystack.to_ascii_lowercase();
    haystack_lower.find(&needle.to_ascii_lowercase())
}

fn first_element_text(source: &str, element: &str) -> String {
    let open = format!("<{element}");
    let close = format!("</{element}>");
    let Some(start) = find_ascii_case_insensitive(source, &open) else {
        return String::new();
    };
    let Some(open_end) = source[start..].find('>') else {
        return String::new();
    };
    let content_start = start + open_end + 1;
    match find_ascii_case_insensitive(&source[content_start..], &close) {
        Some(end) => source[content_start..content_start + end].trim().to_owned(),
        None => String::new(),
    }
}

fn decode_entities(source: &str) -> String {
    if !source.contains('&') {
        return source.to_owned();
    }
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        let tail = &rest[start..];
        // Cap entity length to forms such as `&#x1F600;`; longer input is plain `&`.
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            output.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix('#')
                .and_then(|number| match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => number.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(character) => {
                output.push(character);
                rest = &tail[end + 1..];
            }
            None => {
                output.push('&');
                rest = &tail[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

/// Trim lines, collapse consecutive blank lines, and cap total length.
fn collapse(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if lines.last().is_some_and(|last| last.is_empty()) {
                continue;
            }
            lines.push("");
        } else {
            lines.push(trimmed);
        }
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let joined = lines.join("\n");
    if joined.chars().count() <= MAX_PAGE_CHARS {
        return joined;
    }
    joined.chars().take(MAX_PAGE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_drops_scripts_and_keeps_block_structure() {
        let (title, content) = extract(
            "<html><head><title> Hello &amp; Goodbye </title><style>body{color:red}</style></head>\
             <body><script>var x = '<p>not text</p>';</script>\
             <p>First line</p><div>Second<br>line</div><span>tail</span></body></html>",
        );
        assert_eq!(title, "Hello & Goodbye");
        // Block elements leave a blank line between paragraphs; `<br>` only
        // creates one line break, while inline tags create none.
        assert_eq!(content, "First line\n\nSecond\nline\ntail");
    }

    #[test]
    fn an_unclosed_head_leaves_the_page_intact() {
        // An implicit `</head>` is valid HTML and must preserve page content.
        let (title, content) = extract("<html><head><title>T</title><body><p>kept</p></body></html>");
        assert_eq!(title, "T");
        assert_eq!(content, "kept");
    }

    #[test]
    fn an_unclosed_script_swallows_the_rest_rather_than_leaking_code() {
        // An unclosed script makes the remaining structure untrustworthy.
        let (_, content) = extract("<p>kept</p><script>var a = 1;");
        assert_eq!(content, "kept");
    }

    #[test]
    fn a_tag_that_merely_starts_with_a_dropped_name_is_not_dropped() {
        let (_, content) = extract("<p>before</p><scriptish>after</scriptish>");
        assert!(content.contains("before"));
        assert!(content.contains("after"));
    }

    #[test]
    fn numeric_and_named_entities_both_decode() {
        let (_, content) = extract("<p>&#72;&#x69; &quot;there&quot; &nosuchentity; &</p>");
        assert_eq!(content, "Hi \"there\" &nosuchentity; &");
    }

    #[test]
    fn private_and_rebinding_ranges_are_refused() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            let host = if address.contains(':') {
                format!("http://[{address}]/x")
            } else {
                format!("http://{address}/x")
            };
            let url = Url::parse(&host).expect("test url");
            assert!(
                matches!(
                    resolve_public_target(&url, LocalTargetPolicy::Deny),
                    Err(SearchError::Config(_))
                ),
                "{address} must be refused"
            );
            // The same address is reachable when the caller named it directly.
            assert!(
                resolve_public_target(&url, LocalTargetPolicy::AllowDirect).is_ok(),
                "{address} must be reachable when directly specified"
            );
        }
    }

    #[test]
    fn a_public_literal_address_resolves_without_dns() {
        let url = Url::parse("https://93.184.216.34/x").expect("test url");
        let address =
            resolve_public_target(&url, LocalTargetPolicy::Deny).expect("a public literal address is usable");
        assert_eq!(address.port(), 443);
    }

    #[test]
    fn a_credentialed_or_non_http_target_is_refused() {
        for target in ["ftp://example.com/x", "https://user:pass@example.com/x"] {
            let url = Url::parse(target).expect("test url");
            assert!(matches!(
                resolve_public_target(&url, LocalTargetPolicy::Deny),
                Err(SearchError::Config(_))
            ));
        }
    }
}
