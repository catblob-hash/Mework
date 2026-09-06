//! Result-domain blacklist.
//!
//! A rule is either a match pattern (`<all_urls>` or `scheme://host/path`) or a slash-delimited,
//! case-insensitive regex over origin, path, and query. `*` matches HTTP or HTTPS schemes, any host,
//! subdomains as `*.example.com`, and arbitrary path text.
//!
//! Invalid rules are ignored rather than failing a search. Rust `regex` rejects lookaround patterns,
//! which follow the same ignore path.

use regex::Regex;
use url::Url;

const SUPPORTED_SCHEMES: [&str; 2] = ["http", "https"];

#[derive(Debug)]
enum MatchPattern {
    AllUrls,
    Parts {
        scheme: String,
        host: String,
        path: String,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Blacklist {
    patterns: Vec<MatchPattern>,
    regexes: Vec<Regex>,
}

impl Blacklist {
    pub(crate) fn compile(rules: &[String]) -> Self {
        let mut compiled = Self::default();
        for raw in rules {
            let rule = raw.trim();
            if rule.is_empty() {
                continue;
            }
            if rule.len() >= 2 && rule.starts_with('/') && rule.ends_with('/') {
                if let Ok(regex) = Regex::new(&format!("(?i){}", &rule[1..rule.len() - 1])) {
                    compiled.regexes.push(regex);
                }
                continue;
            }
            if let Some(pattern) = parse_match_pattern(rule) {
                compiled.patterns.push(pattern);
            }
        }
        compiled
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.patterns.is_empty() && self.regexes.is_empty()
    }

    /// Returns whether a result should be discarded. Keep unparseable URLs because the blacklist
    /// excludes known unwanted sources; it is not a second URL validator.
    pub(crate) fn blocks(&self, url: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        let Ok(parsed) = Url::parse(url) else {
            return false;
        };
        let target = format!(
            "{}{}{}",
            parsed.origin().ascii_serialization(),
            parsed.path(),
            parsed
                .query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        if self.regexes.iter().any(|regex| regex.is_match(&target)) {
            return true;
        }
        self.patterns
            .iter()
            .any(|pattern| matches_pattern(pattern, &parsed))
    }
}

fn is_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn parse_match_pattern(rule: &str) -> Option<MatchPattern> {
    if rule == "<all_urls>" {
        return Some(MatchPattern::AllUrls);
    }
    let (scheme, rest) = rule.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let scheme_is_valid = scheme == "*"
        || (scheme
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && scheme.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
            }));
    if !scheme_is_valid {
        return None;
    }
    // Search results can only use HTTP(S); reject an `ftp://` rule that can never match.
    if scheme != "*" && !SUPPORTED_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let host = host.to_ascii_lowercase();
    let host_is_valid = host == "*"
        || host
            .strip_prefix("*.")
            .unwrap_or(&host)
            .split('.')
            .all(is_label);
    if !host_is_valid {
        return None;
    }
    Some(MatchPattern::Parts {
        scheme,
        host,
        path: path.to_owned(),
    })
}

fn matches_scheme(pattern: &str, scheme: &str) -> bool {
    if pattern == "*" {
        return SUPPORTED_SCHEMES.contains(&scheme);
    }
    pattern == scheme
}

fn matches_host(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    pattern == host
}

fn matches_path(pattern: &str, path: &str) -> bool {
    if pattern == "/*" {
        return true;
    }
    let mut segments = pattern.split('*');
    let first = segments.next().unwrap_or_default();
    let rest = segments.collect::<Vec<_>>();
    if rest.is_empty() {
        return path == first;
    }
    if !path.starts_with(first) {
        return false;
    }
    let mut position = first.len();
    for part in &rest[..rest.len() - 1] {
        match path[position..].find(part) {
            Some(offset) => position += offset + part.len(),
            None => return false,
        }
    }
    path[position..].ends_with(rest[rest.len() - 1])
}

fn matches_pattern(pattern: &MatchPattern, url: &Url) -> bool {
    let scheme = url.scheme();
    match pattern {
        MatchPattern::AllUrls => SUPPORTED_SCHEMES.contains(&scheme),
        MatchPattern::Parts {
            scheme: scheme_pattern,
            host: host_pattern,
            path: path_pattern,
        } => {
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            let path = format!(
                "{}{}",
                url.path(),
                url.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            );
            matches_scheme(scheme_pattern, scheme)
                && matches_host(host_pattern, &host)
                && matches_path(path_pattern, &path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn all_urls_blocks_every_http_result() {
        let list = Blacklist::compile(&rules(&["<all_urls>"]));
        assert!(list.blocks("https://example.com/a"));
        assert!(list.blocks("http://example.com/a"));
    }

    #[test]
    fn a_host_pattern_matches_the_domain_and_its_subdomains_only_with_a_star_prefix() {
        let exact = Blacklist::compile(&rules(&["https://example.com/*"]));
        assert!(exact.blocks("https://example.com/anything?q=1"));
        assert!(!exact.blocks("https://docs.example.com/anything"));

        let subdomains = Blacklist::compile(&rules(&["https://*.example.com/*"]));
        assert!(subdomains.blocks("https://example.com/x"));
        assert!(subdomains.blocks("https://docs.example.com/x"));
        // Compare suffixes at a label boundary so `notexample.com` cannot match.
        assert!(!subdomains.blocks("https://notexample.com/x"));
    }

    #[test]
    fn a_star_scheme_covers_http_and_https_but_not_other_schemes() {
        let list = Blacklist::compile(&rules(&["*://example.com/*"]));
        assert!(list.blocks("http://example.com/x"));
        assert!(list.blocks("https://example.com/x"));
        assert!(!list.blocks("ftp://example.com/x"));
    }

    #[test]
    fn a_path_wildcard_matches_prefix_middle_and_suffix() {
        let list = Blacklist::compile(&rules(&["https://example.com/blog/*/draft"]));
        assert!(list.blocks("https://example.com/blog/2026/draft"));
        assert!(!list.blocks("https://example.com/blog/2026/published"));
    }

    #[test]
    fn a_slash_delimited_rule_is_a_case_insensitive_regex_over_origin_path_and_query() {
        let list = Blacklist::compile(&rules(&[r"/EXAMPLE\.com\/login/"]));
        assert!(list.blocks("https://example.com/login"));
        assert!(!list.blocks("https://example.com/home"));
    }

    #[test]
    fn a_malformed_rule_is_ignored_instead_of_failing_the_whole_search() {
        // Missing `://`, invalid host, and a Rust-unsupported lookaround.
        let list = Blacklist::compile(&rules(&[
            "example.com",
            "https://exa mple.com/*",
            "/(?=x)/",
            "",
            "   ",
        ]));
        assert!(list.is_empty());
        assert!(!list.blocks("https://example.com/x"));
    }

    #[test]
    fn a_result_url_that_cannot_be_parsed_is_kept() {
        let list = Blacklist::compile(&rules(&["<all_urls>"]));
        assert!(!list.blocks("not a url"));
    }
}
