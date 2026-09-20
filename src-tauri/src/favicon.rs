//! Site icons for the timeline's web-search card.
//!
//! The renderer cannot load a remote image at all: the application's CSP is
//! `img-src 'self' data:`, and `connect-src` reaches only the IPC origin. That
//! is deliberate, so this module exists to keep it that way — the host fetches
//! the icon and hands back a `data:` URL, and the page never talks to the site.
//!
//! A third-party icon service would be one line of renderer code instead, and
//! that is exactly why it is not used: it would tell Google or DuckDuckGo every
//! domain the user's searches touched. Fetching from the site itself leaks
//! nothing that the search did not already.
//!
//! Everything here is best-effort. A site with no icon, an icon behind a login,
//! a timeout, or a private-network address all resolve to "no icon", which the
//! card renders as a lettered placeholder. None of it is worth an error.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::web_search::readable::{fetch_guarded, LocalTargetPolicy};

/// Icons above this size are not icons. 512 KiB is generous for a favicon and
/// small enough that a hostile server cannot use this path to fill the disk.
const MAX_ICON_BYTES: usize = 512 * 1024;

/// How long a cached answer — including "this site has no icon" — is reused.
/// Sites change their icon rarely, and a wrong icon for a week is a smaller
/// problem than refetching on every render.
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Image types worth putting in an `<img>`. SVG is deliberately excluded: an SVG
/// is a document that can carry script, and this one is about to be handed to
/// the renderer as a data URL.
const ALLOWED_TYPES: &[(&str, &str)] = &[
    ("image/x-icon", "ico"),
    ("image/vnd.microsoft.icon", "ico"),
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// One cached lookup. `data_url` is absent when the site has no usable icon;
/// that negative answer is cached too, so a site without one is not refetched
/// on every card that mentions it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedIcon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data_url: Option<String>,
    /// Unix seconds. Compared against the wall clock only to expire entries, so
    /// a clock that moves backwards costs at most one extra fetch.
    fetched_at: u64,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Cache file for one host.
///
/// The host is hashed rather than used as a filename: a domain can contain
/// characters that are legal in DNS and illegal in a Windows path, and hashing
/// removes the whole class of traversal and reserved-name problems at once.
fn cache_path(app_data_path: &Path, host: &str) -> PathBuf {
    let digest = format!("{:x}", sha2::Sha256::digest(host.as_bytes()));
    app_data_path
        .join("favicons")
        .join(format!("{digest}.json"))
}

fn read_cache(path: &Path) -> Option<CachedIcon> {
    let cached: CachedIcon = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    let age = now_seconds().saturating_sub(cached.fetched_at);
    (age < CACHE_TTL.as_secs()).then_some(cached)
}

fn write_cache(path: &Path, entry: &CachedIcon) {
    // A cache that cannot be written is not a failure worth reporting: the
    // lookup still answered, it just will not be remembered.
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(encoded) = serde_json::to_string(entry) {
        let _ = fs::write(path, encoded);
    }
}

/// The host part of a URL, lowercased, or `None` when the input is not an
/// ordinary web address.
pub fn icon_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().map(str::to_ascii_lowercase)
}

fn data_url_of(content_type: &str, body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let declared = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let media_type = ALLOWED_TYPES
        .iter()
        .find(|(name, _)| *name == declared)
        .map(|(name, _)| *name)
        // Servers mislabel `.ico` constantly, most often as `text/plain` or
        // `application/octet-stream`. Sniff the two magic numbers that matter
        // rather than discarding a working icon over a bad header.
        .or_else(|| match body {
            [0x00, 0x00, 0x01, 0x00, ..] => Some("image/x-icon"),
            [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
            _ => None,
        })?;
    Some(format!(
        "data:{media_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(body)
    ))
}

/// Icon URLs declared by a page, in document order.
///
/// Deliberately a small scan rather than a parse: this only has to find `href`
/// on a `<link>` whose `rel` mentions `icon`, and a full HTML parse on every
/// site the model searches would cost far more than it is worth.
fn declared_icon_hrefs(html: &str) -> Vec<String> {
    let lowered = html.to_ascii_lowercase();
    let mut hrefs = Vec::new();
    let mut cursor = 0usize;
    while let Some(start) = lowered[cursor..].find("<link").map(|index| cursor + index) {
        let end = lowered[start..]
            .find('>')
            .map(|index| start + index)
            .unwrap_or(lowered.len());
        let tag = &lowered[start..end];
        if tag.contains("rel=") && tag.contains("icon") {
            if let Some(href) = attribute_value(&html[start..end], "href") {
                hrefs.push(href);
            }
        }
        cursor = end.max(start + 1);
        if hrefs.len() >= 4 {
            break;
        }
    }
    hrefs
}

fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lowered = tag.to_ascii_lowercase();
    let at = lowered.find(&format!("{name}="))? + name.len() + 1;
    let rest = tag.get(at..)?;
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let value = rest[1..].split(quote).next()?;
        return (!value.trim().is_empty()).then(|| value.trim().to_owned());
    }
    let value = rest.split_whitespace().next()?;
    (!value.is_empty()).then(|| value.to_owned())
}

/// Fetch one site's icon, or `None` when it has none we can use.
///
/// Tries the conventional `/favicon.ico` first because it is one request and
/// usually right, and only reads the home page when that misses. Every request
/// goes through the search pipeline's guarded fetch, so a domain that resolves
/// to a private address is refused here exactly as it would be in a search.
fn fetch_icon(host: &str) -> Option<String> {
    let direct = fetch_guarded(
        &format!("https://{host}/favicon.ico"),
        LocalTargetPolicy::Deny,
        MAX_ICON_BYTES,
    )
    .ok()
    .and_then(|response| data_url_of(&response.content_type, &response.body));
    if direct.is_some() {
        return direct;
    }
    let page = fetch_guarded(
        &format!("https://{host}/"),
        LocalTargetPolicy::Deny,
        // The icon link is in `<head>`; reading the whole document to find it
        // would make this cost as much as a page fetch.
        128 * 1024,
    )
    .ok()?;
    let html = String::from_utf8_lossy(&page.body);
    for href in declared_icon_hrefs(&html) {
        let Ok(target) = page.url.join(&href) else {
            continue;
        };
        if let Some(data_url) =
            fetch_guarded(target.as_str(), LocalTargetPolicy::Deny, MAX_ICON_BYTES)
                .ok()
                .and_then(|response| data_url_of(&response.content_type, &response.body))
        {
            return Some(data_url);
        }
    }
    None
}

/// Site icon for one host as a `data:` URL, or `None`.
///
/// Never fails: a lookup that cannot answer is a missing icon, and the card
/// falls back to a lettered placeholder.
pub fn site_icon(app_data_path: &Path, host: &str) -> Option<String> {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    // A host with no dot is not a public site; refusing it here keeps the
    // guarded fetch from having to reject `localhost` one layer down.
    if host.is_empty() || !host.contains('.') || host.len() > 253 {
        return None;
    }
    let path = cache_path(app_data_path, &host);
    if let Some(cached) = read_cache(&path) {
        return cached.data_url;
    }
    let data_url = fetch_icon(&host);
    write_cache(
        &path,
        &CachedIcon {
            data_url: data_url.clone(),
            fetched_at: now_seconds(),
        },
    );
    data_url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_yield_a_host() {
        assert_eq!(
            icon_host("https://Docs.Anthropic.com/en/x"),
            Some("docs.anthropic.com".into())
        );
        assert_eq!(icon_host("http://example.com"), Some("example.com".into()));
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "notaurl",
        ] {
            assert_eq!(icon_host(refused), None, "{refused} must not yield a host");
        }
    }

    #[test]
    fn a_host_that_cannot_be_a_public_site_is_refused_before_any_request() {
        let temporary = std::env::temp_dir().join("mework-favicon-test");
        // No dot means not a public name; these must not even reach the fetcher.
        for refused in ["", "localhost", "  ", "."] {
            assert_eq!(
                site_icon(&temporary, refused),
                None,
                "{refused:?} must be refused"
            );
        }
        assert_eq!(site_icon(&temporary, &"a".repeat(300)), None);
    }

    #[test]
    fn a_data_url_is_built_only_for_an_image_type() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert!(data_url_of("image/png", &png)
            .expect("png is an image")
            .starts_with("data:image/png;base64,"));
        // Mislabelled icons are common enough that the magic number decides.
        assert!(data_url_of("text/plain", &[0x00, 0x00, 0x01, 0x00, 0x01])
            .expect("an ICO magic number is an icon")
            .starts_with("data:image/x-icon;base64,"));
        // An SVG is a scriptable document, so it is not an icon no matter what
        // the server says, and unrecognized bytes are not guessed at.
        assert_eq!(data_url_of("image/svg+xml", b"<svg onload=\"x\"/>"), None);
        assert_eq!(data_url_of("text/html", b"<html>"), None);
        assert_eq!(data_url_of("image/png", b""), None);
    }

    #[test]
    fn icon_links_are_found_wherever_rel_puts_the_word() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/style.css">
            <link rel="shortcut icon" href="/a.ico">
            <link rel='ICON' href='/b.png'>
            <link rel=apple-touch-icon href=/c.png>
        </head></html>"#;
        assert_eq!(
            declared_icon_hrefs(html),
            vec![
                "/a.ico".to_owned(),
                "/b.png".to_owned(),
                "/c.png".to_owned()
            ]
        );
        // A stylesheet link is never an icon, whatever else the tag holds.
        assert!(declared_icon_hrefs(r#"<link rel="stylesheet" href="/x.css">"#).is_empty());
    }

    #[test]
    fn each_host_caches_under_its_own_hashed_name() {
        let root = Path::new("/tmp/app");
        let one = cache_path(root, "example.com");
        let two = cache_path(root, "example.org");
        assert_ne!(one, two);
        // The filename must not carry the host: a domain can hold characters
        // that are illegal in a path, and the hash removes the whole question.
        let name = one
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(name.ends_with(".json"));
        assert!(!name.contains("example"));
    }
}
