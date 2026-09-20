//! Execution pipeline for one search or fetch.
//!
//! 1. Normalize input: trim, remove empty values, enforce limits, and require
//!    absolute http(s) URLs for fetches.
//! 2. Run one provider driver per input concurrently; partial failure is allowed.
//! 3. Merge successful results, raising the first failure only when all fail.
//! 4. Filter by the conversation's domain rules, if it has either list in effect.
//! 5. Truncate content to the conversation's token budget, if it set one.
//!
//! Workers run outside the turn thread and report through a channel. The receive
//! loop probes the parent sink every [`CANCELLATION_PROBE_INTERVAL`] so stopping
//! a turn returns immediately without waiting for slow upstream requests.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;

use crate::api::ModelEventSink;
use crate::api::CANCELLATION_PROBE_INTERVAL;
use crate::http_util::client;
use crate::model::{
    ModelStreamEvent, ResolvedSearchProvider, SearchCapability, SearchDomainFilterMode,
    SearchExecutionConfig, MAX_SEARCH_INPUTS,
};

use super::domain_rules::DomainRules;
use super::providers::{self, ProviderCall};
use super::{SearchError, SearchResultItem};

/// Runs a closure concurrently for each input and returns results in input order.
///
/// This function does not probe the parent sink. Its caller must either be the
/// probe loop or run under one.
pub(crate) fn map_in_parallel<T, F>(inputs: Vec<String>, run: F) -> Vec<Result<T, SearchError>>
where
    T: Send + 'static,
    F: Fn(String) -> Result<T, SearchError> + Send + Sync + 'static,
{
    let run = Arc::new(run);
    let handles: Vec<_> = inputs
        .into_iter()
        .map(|input| {
            let run = Arc::clone(&run);
            std::thread::spawn(move || run(input))
        })
        .collect();
    handles
        .into_iter()
        .map(|handle| {
            handle.join().unwrap_or_else(|_| {
                Err(SearchError::Transient(
                    "Search worker terminated unexpectedly".to_owned(),
                ))
            })
        })
        .collect()
}

/// Runs each input concurrently while probing the parent sink.
///
/// `Err` means the parent sink closed while the turn was settling. Provider
/// failures remain in the returned slots.
fn fan_out_with_probes(
    inputs: Vec<String>,
    event_sink: &ModelEventSink<'_>,
    run: impl Fn(String) -> Result<Vec<SearchResultItem>, SearchError> + Send + Sync + 'static,
) -> Result<Vec<Result<Vec<SearchResultItem>, SearchError>>, String> {
    let total = inputs.len();
    let (sender, receiver) = mpsc::channel();
    let run = Arc::new(run);
    for (index, input) in inputs.into_iter().enumerate() {
        let sender = sender.clone();
        let run = Arc::clone(&run);
        std::thread::spawn(move || {
            let _ = sender.send((index, run(input)));
        });
    }
    drop(sender);

    let mut slots: Vec<Option<Result<Vec<SearchResultItem>, SearchError>>> =
        (0..total).map(|_| None).collect();
    let mut received = 0;
    while received < total {
        match receiver.recv_timeout(CANCELLATION_PROBE_INTERVAL) {
            Ok((index, outcome)) => {
                slots[index] = Some(outcome);
                received += 1;
            }
            // A disconnected sender means a worker terminated before reporting.
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => event_sink(ModelStreamEvent::Ping)?,
        }
    }
    Ok(slots
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                Err(SearchError::Transient(
                    "Search worker terminated unexpectedly".to_owned(),
                ))
            })
        })
        .collect())
}

/// Normalizes URL input.
pub fn normalize_urls(values: &[String]) -> Result<Vec<String>, String> {
    let normalized: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    if normalized.is_empty() {
        return Err("At least one URL is required".to_owned());
    }
    if normalized.len() > MAX_SEARCH_INPUTS {
        return Err(format!(
            "At most {MAX_SEARCH_INPUTS} URLs can be fetched at once"
        ));
    }
    let invalid: Vec<&str> = normalized
        .iter()
        .filter(|value| !providers::is_http_url(value))
        .map(String::as_str)
        .collect();
    if !invalid.is_empty() {
        return Err(format!(
            "Not an absolute HTTP(S) URL: {}",
            invalid.join(", ")
        ));
    }
    Ok(normalized)
}

/// Runs one capability. Inputs are normalized; outputs are filtered and compressed.
pub fn run_capability(
    capability: SearchCapability,
    provider: &ResolvedSearchProvider,
    execution: &SearchExecutionConfig,
    api_key: Option<String>,
    basic_auth_password: Option<String>,
    inputs: Vec<String>,
    allow_local_targets: bool,
    event_sink: &ModelEventSink<'_>,
) -> Result<Vec<SearchResultItem>, SearchError> {
    // Validate the endpoint before issuing requests so configuration errors are
    // reported once instead of once for every input.
    if provider
        .kind
        .capability(capability)
        .is_some_and(|spec| spec.requires_api_host())
    {
        providers::parse_api_host(provider.kind, &provider.api_host)?;
    }
    let call = ProviderCall {
        client: client().map_err(SearchError::Transient)?,
        provider: provider.clone(),
        execution: execution.clone(),
        api_key,
        basic_auth_password,
        allow_local_targets,
    };
    let outcomes = fan_out_with_probes(inputs, event_sink, move |input| match capability {
        SearchCapability::SearchKeywords => providers::search_keywords(&call, &input),
        SearchCapability::FetchUrls => providers::fetch_url(&call, &input),
    })
    .map_err(SearchError::Cancelled)?;

    let mut merged = Vec::new();
    let mut first_error = None;
    let mut succeeded = false;
    for outcome in outcomes {
        match outcome {
            Ok(items) => {
                succeeded = true;
                merged.extend(items);
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    // Return successful results when any input succeeds; otherwise surface the first failure.
    if !succeeded {
        return Err(first_error.unwrap_or_else(|| {
            SearchError::Transient("Web search returned no results".to_owned())
        }));
    }
    let filtered = apply_domain_filter(merged, execution);
    Ok(apply_compression(filtered, execution))
}

/// Keeps only the results this conversation's domain rules admit.
///
/// The two lists are a choice rather than a pair of switches, so exactly one of
/// them is ever consulted: a result admitted by one and refused by the other
/// would have no obvious answer, and the selector that produced this mode
/// cannot be in both states at once. An allowlist that is empty while its mode
/// is on therefore keeps nothing — that is what an allowlist of nothing means,
/// and quietly reading it as "keep everything" would invert the rule the user
/// switched on.
fn apply_domain_filter(
    mut results: Vec<SearchResultItem>,
    execution: &SearchExecutionConfig,
) -> Vec<SearchResultItem> {
    match execution.domain_filter {
        SearchDomainFilterMode::Off => results,
        SearchDomainFilterMode::Exclude => {
            let blacklist = DomainRules::compile(&execution.exclude_domains);
            results.retain(|item| !blacklist.matches(&item.url));
            results
        }
        SearchDomainFilterMode::Include => {
            let allowlist = DomainRules::compile(&execution.include_domains);
            results.retain(|item| allowlist.matches(&item.url));
            results
        }
    }
}

/// Applies compression by splitting the call-wide token budget evenly across
/// results. A budget of zero is no compression at all.
fn apply_compression(
    mut results: Vec<SearchResultItem>,
    execution: &SearchExecutionConfig,
) -> Vec<SearchResultItem> {
    let limit = execution.compression_cutoff;
    if results.is_empty() || limit == 0 {
        return results;
    }
    let per_result = ((limit as usize) / results.len()).max(1);
    for item in &mut results {
        let sliced = slice_by_tokens(&item.content, per_result);
        if sliced.len() < item.content.len() {
            item.content = format!("{sliced}...");
        }
    }
    results
}

/// Truncates by token budget using the same estimates as `api::estimate_tokens`:
/// ASCII is 1/4 token and non-ASCII is 1/1.6 tokens. Returns a source prefix so
/// truncation is detectable by length.
fn slice_by_tokens(text: &str, limit: usize) -> &str {
    let mut ascii = 0_u64;
    let mut non_ascii = 0_u64;
    for (index, character) in text.char_indices() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
        let tokens = ((ascii as f64 / 4.0) + (non_ascii as f64 / 1.6)).ceil() as usize;
        if tokens > limit {
            return &text[..index];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SearchProviderKind;

    fn item(url: &str, content: &str) -> SearchResultItem {
        SearchResultItem {
            title: "t".to_owned(),
            content: content.to_owned(),
            url: url.to_owned(),
            source_input: "q".to_owned(),
        }
    }

    /// A budget of 0 is "no compression", so it is what these tests pass when
    /// they want the results back untouched.
    fn execution(compression_cutoff: u32) -> SearchExecutionConfig {
        SearchExecutionConfig {
            max_results: 5,
            compression_cutoff,
            domain_filter: SearchDomainFilterMode::Off,
            include_domains: Vec::new(),
            exclude_domains: Vec::new(),
        }
    }

    fn rules(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn url_normalization_rejects_anything_that_is_not_an_absolute_http_url() {
        assert!(normalize_urls(&["https://a.example/x".to_owned()]).is_ok());
        for refused in ["example.com", "file:///etc/passwd", "javascript:alert(1)"] {
            assert!(
                normalize_urls(&[refused.to_owned()]).is_err(),
                "{refused} must be rejected"
            );
        }
    }

    #[test]
    fn cutoff_splits_the_budget_across_results_and_marks_what_it_cut() {
        let results = vec![item("https://a.example", &"x".repeat(400)); 2];
        // A 100-token budget split over two results allows about 200 ASCII characters each.
        let compressed = apply_compression(results, &execution(100));
        for entry in &compressed {
            assert!(
                entry.content.ends_with("..."),
                "truncated content must have an ellipsis"
            );
            assert!(entry.content.chars().count() < 400);
        }
    }

    #[test]
    fn a_budget_leaves_short_content_untouched_and_zero_leaves_everything() {
        let short = vec![item("https://a.example", "tiny")];
        let compressed = apply_compression(short.clone(), &execution(2_000));
        assert_eq!(compressed[0].content, "tiny");

        let long = vec![item("https://a.example", &"x".repeat(4_000))];
        let untouched = apply_compression(long, &execution(0));
        assert_eq!(untouched[0].content.chars().count(), 4_000);
    }

    /// The two lists are read in opposite directions out of the same matcher,
    /// and only the one the mode names is consulted.
    #[test]
    fn the_domain_filter_reads_whichever_list_its_mode_names() {
        let results = vec![
            item("https://ads.example/a", "x"),
            item("https://docs.example/b", "y"),
        ];
        let kept = |config: &SearchExecutionConfig| {
            apply_domain_filter(results.clone(), config)
                .into_iter()
                .map(|entry| entry.url)
                .collect::<Vec<_>>()
        };

        let mut off = execution(0);
        off.exclude_domains = rules(&["*://ads.example/*"]);
        off.include_domains = rules(&["*://docs.example/*"]);
        assert_eq!(
            kept(&off).len(),
            2,
            "a filter that is off reads neither list"
        );

        let mut blocking = off.clone();
        blocking.domain_filter = SearchDomainFilterMode::Exclude;
        assert_eq!(kept(&blocking), vec!["https://docs.example/b".to_owned()]);

        let mut admitting = off.clone();
        admitting.domain_filter = SearchDomainFilterMode::Include;
        assert_eq!(kept(&admitting), vec!["https://docs.example/b".to_owned()]);
    }

    /// An allowlist of nothing admits nothing. Reading an empty list as "keep
    /// everything" would invert the rule the user just switched on.
    #[test]
    fn an_empty_allowlist_keeps_nothing_while_an_empty_blocklist_blocks_nothing() {
        let results = vec![item("https://a.example/x", "x")];
        let mut admitting = execution(0);
        admitting.domain_filter = SearchDomainFilterMode::Include;
        assert!(apply_domain_filter(results.clone(), &admitting).is_empty());

        let mut blocking = execution(0);
        blocking.domain_filter = SearchDomainFilterMode::Exclude;
        assert_eq!(apply_domain_filter(results, &blocking).len(), 1);
    }

    #[test]
    fn token_slicing_never_splits_a_multibyte_character() {
        let text = "中文中文中文";
        for limit in 1..8 {
            let sliced = slice_by_tokens(text, limit);
            assert!(text.starts_with(sliced));
            assert!(sliced.chars().count() * 3 == sliced.len());
        }
    }

    #[test]
    fn a_provider_that_lacks_the_capability_fails_before_any_request() {
        let provider = ResolvedSearchProvider {
            kind: SearchProviderKind::Tavily,
            api_host: "https://api.tavily.com".to_owned(),
            engines: Vec::new(),
            basic_auth_username: String::new(),
        };
        let sink: &ModelEventSink<'_> = &|_| Ok(());
        let error = run_capability(
            SearchCapability::FetchUrls,
            &provider,
            &execution(0),
            Some("k".to_owned()),
            None,
            vec!["https://a.example".to_owned()],
            false,
            sink,
        )
        .expect_err("tavily does not support fetching");
        assert!(matches!(error, SearchError::Config(_)));
    }
}
