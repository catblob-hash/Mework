//! Fetch models.
//!
//! Fetchers are selected by the conversation protocol:
//!
//! - Anthropic Messages uses `/v1/models` with `x-api-key`,
//!   `anthropic-version`, and `?limit=1000`.
//! - The ChatGPT Codex backend uses `GET {base}/models?client_version=…` with
//!   the OAuth session's Bearer token and `chatgpt-account-id`.
//! - Other providers use `GET {base}/models` and an OpenAI-compatible envelope.
//!
//! [`crate::model_registry`] enriches returned IDs with capabilities and limits;
//! unmatched entries fall back to response fields and ID-based inference.

use std::collections::{BTreeMap, BTreeSet};

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Url;
use serde_json::Value;

use crate::api::{authenticated_request, optional_provider_key, parse_http_json, validate_provider};
use crate::model::{ProviderFamily, ApiProvider, ModelCapability, ModelProfile, ReasoningContent};
use crate::model_registry;
use crate::http_util::{custom_endpoint_url, sanitize_error};

/// Upstream catalog shape, selected by conversation protocol rather than vendor identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fetcher {
    Anthropic,
    /// The ChatGPT-subscription Codex backend: one fixed host with its own
    /// query parameter, ordering, and OAuth credential.
    Codex,
    /// The Claude Agent family's built-in registry, answered without any
    /// request: neither the host nor the CLI is asked what models exist.
    ClaudeAgent,
    /// Unconditional fallback.
    OpenAiCompatible,
}

fn select(provider: &ApiProvider) -> Fetcher {
    match provider.family {
        ProviderFamily::Anthropic => Fetcher::Anthropic,
        ProviderFamily::OpenaiCodex => Fetcher::Codex,
        ProviderFamily::ClaudeAgent => Fetcher::ClaudeAgent,
        _ => Fetcher::OpenAiCompatible,
    }
}

// ───────────────────────────── Intermediate form ─────────────────────────────

/// A model parsed from the upstream response before catalog enrichment.
/// `raw` is retained for field inference when the catalog has no match.
#[derive(Debug, Default)]
struct Fetched {
    id: String,
    name: Option<String>,
    raw: Value,
}

impl Fetched {
    fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    fn named(mut self, name: Option<&str>) -> Self {
        self.name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        self
    }

    fn with_raw(mut self, raw: &Value) -> Self {
        self.raw = raw.clone();
        self
    }
}

/// Deduplicate trimmed IDs while preserving the first entry and upstream order.
fn dedup(items: Vec<Fetched>) -> Vec<Fetched> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter_map(|mut item| {
            let id = item.id.trim().to_owned();
            if id.is_empty() || !seen.insert(id.clone()) {
                return None;
            }
            item.id = id;
            Some(item)
        })
        .collect()
}

// ───────────────────────────── HTTP ─────────────────────────────

struct Discovery<'a> {
    provider: &'a ApiProvider,
    /// Address the request is built on. Usually the provider's own `base_url`;
    /// the Codex family substitutes its fixed backend when that is empty.
    base_url: String,
    client: Client,
    key: Option<String>,
    /// Headers beyond the family's credential header (Codex's account id).
    extra_headers: BTreeMap<String, String>,
}

impl Discovery<'_> {
    fn url(&self, drop_trailing: &[&str], path: &str, query: Option<&str>) -> Result<Url, String> {
        custom_endpoint_url(&self.base_url, drop_trailing, path, query)
    }

    fn send(&self, builder: RequestBuilder) -> Result<Value, String> {
        let response = builder.send().map_err(|error| {
            sanitize_error(&format!("获取模型列表失败: {error}"), self.key.as_deref())
        })?;
        parse_http_json(response, self.key.as_deref())
    }

    fn with_extra_headers(&self, mut builder: RequestBuilder) -> RequestBuilder {
        for (name, value) in &self.extra_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder
    }

    fn get(&self, url: Url) -> Result<Value, String> {
        self.send(self.with_extra_headers(authenticated_request(
            self.client.get(url),
            self.provider.family,
            self.key.as_deref(),
        )))
    }

    /// Like [`Self::get`], but forces the wire protocol. The Anthropic catalog
    /// requires `x-api-key` and `anthropic-version` even if the provider family
    /// is configured differently.
    fn get_as(&self, url: Url, format: ProviderFamily) -> Result<Value, String> {
        self.send(self.with_extra_headers(authenticated_request(
            self.client.get(url),
            format,
            self.key.as_deref(),
        )))
    }
}

// ───────────────────────────── Response parsing ─────────────────────────────

fn as_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Keys a catalog response may hang its list under. Tried in order at the root
/// and once more one level in, because relays wrap the OpenAI envelope in their
/// own gateway envelope often enough that a single nested hop pays for itself.
/// Anything deeper is a different protocol rather than a wrapper.
const ENVELOPE_KEYS: &[&str] = &["data", "models", "result", "response", "items", "list"];

fn list_under(value: &Value) -> Option<&Value> {
    ENVELOPE_KEYS.iter().find_map(|key| value.get(*key))
}

/// OpenAI-compatible envelope: a keyed list, or a root array.
fn envelope(value: &Value) -> Result<&Vec<Value>, String> {
    let candidate = list_under(value).unwrap_or(value);
    if let Some(items) = candidate.as_array() {
        return Ok(items);
    }
    list_under(candidate)
        .and_then(Value::as_array)
        .ok_or_else(|| "模型列表响应缺少 data 数组".to_owned())
}

/// Keys a catalog entry may use for the model identifier, most specific first.
/// `name` is last because an OpenAI-shaped record uses it as a display label;
/// it only becomes the identifier when the record has nothing else.
const ID_KEYS: &[&str] = &["id", "model", "model_id", "modelId", "slug", "name"];

/// An entry needs an identifier and nothing else. Malformed entries are skipped
/// individually so one record cannot fail the whole fetch.
fn openai_item(value: &Value) -> Option<Fetched> {
    // Relays that list models as bare strings are a real shape, and dropping
    // them reported an empty catalog rather than the models the relay listed.
    if let Some(id) = value.as_str() {
        let id = id.trim();
        return (!id.is_empty()).then(|| Fetched::new(id).with_raw(value));
    }
    let labelled = ID_KEYS
        .iter()
        .take(ID_KEYS.len() - 1)
        .find_map(|key| value.get(*key).and_then(Value::as_str));
    // Google keys the identifier as `name` behind a `models/` prefix, and relays
    // proxying Gemini pass that shape straight through.
    let unlabelled = value
        .get("name")
        .and_then(Value::as_str)
        .map(|name| name.strip_prefix("models/").unwrap_or(name));
    let id = labelled.or(unlabelled)?.trim();
    if id.is_empty() {
        return None;
    }
    // `name` was the identifier in the unlabelled shape, so it must not also
    // become the label; take the display name from the dedicated fields.
    let name = labelled
        .and(as_str(value, "name"))
        .or_else(|| as_str(value, "display_name"))
        .or_else(|| as_str(value, "displayName"));
    Some(Fetched::new(id).named(name).with_raw(value))
}

fn openai_items(value: &Value) -> Result<Vec<Fetched>, String> {
    Ok(envelope(value)?.iter().filter_map(openai_item).collect())
}

// ───────────────────────────── Fetchers ─────────────────────────────

/// Anthropic requires `x-api-key`, `anthropic-version`, and `?limit=1000`.
/// The endpoint defaults to 20 items and supports a maximum page size of 1000.
///
/// Entries are read by the shared projection: Anthropic's `id` plus
/// `display_name` is one of the shapes it already covers, and sharing it means a
/// relay answering this leg gets the same tolerance as the generic one.
fn anthropic(discovery: &Discovery) -> Result<Vec<Fetched>, String> {
    let url = discovery.url(&[], "models", Some("limit=1000"))?;
    let value = discovery.get_as(url, ProviderFamily::Anthropic)?;
    openai_items(&value)
}

/// Model listing for an Anthropic-protocol provider, with a fallback for relays.
///
/// `x-api-key` plus `anthropic-version` is what Anthropic itself requires, but a
/// third-party relay speaking the Messages protocol almost always authenticates
/// its catalog with `Authorization: Bearer` and answers `401` to the Anthropic
/// header shape. Selecting the leg by protocol alone therefore made the official
/// endpoint the only one that could list models.
///
/// The second attempt is the generic OpenAI-compatible leg — including its
/// header shape, not just its URL. Sending `anthropic-version` and `x-api-key`
/// again would reproduce the rejection the first attempt already collected.
/// An empty result warrants the same retry: a gateway that does not recognize
/// the Anthropic headers may answer `200` with no entries rather than an error.
fn anthropic_with_relay_fallback(discovery: &Discovery) -> Result<Vec<Fetched>, String> {
    let native = anthropic(discovery);
    if let Ok(models) = &native {
        if !models.is_empty() {
            return native;
        }
    }
    match open_ai_compatible_as(discovery, ProviderFamily::OpenaiCompatible) {
        Ok(models) if !models.is_empty() => Ok(models),
        // Neither leg produced anything. Report the protocol-native failure,
        // which is the one the user configured for.
        fallback => native.and(fallback),
    }
}

fn open_ai_compatible(discovery: &Discovery) -> Result<Vec<Fetched>, String> {
    let value = discovery.get(discovery.url(&[], "models", None)?)?;
    openai_items(&value)
}

/// The ChatGPT Codex catalog.
///
/// `client_version` is mandatory (the backend answers 400 without it) and gates
/// the list: entries whose `minimal_client_version` is newer than the value sent
/// are withheld, so the pinned constant decides which models a user can see.
/// Entries carry `slug` + `display_name`, which the shared projection already
/// reads, plus `visibility` (`list` for the picker, `hide` for models still
/// served but not advertised — both are usable, so both are kept) and
/// `priority`, the backend's own display order.
fn codex(discovery: &Discovery) -> Result<Vec<Fetched>, String> {
    let query = format!(
        "client_version={}",
        crate::codex_oauth::CODEX_MODELS_CLIENT_VERSION
    );
    let value = discovery.get(discovery.url(&[], "models", Some(&query))?)?;
    let mut items = openai_items(&value)?;
    items.sort_by_key(|item| {
        item.raw
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    Ok(items)
}

/// Like [`open_ai_compatible`], but pins the wire protocol rather than reading it
/// from the provider.
fn open_ai_compatible_as(
    discovery: &Discovery,
    format: ProviderFamily,
) -> Result<Vec<Fetched>, String> {
    let value = discovery.get_as(discovery.url(&[], "models", None)?, format)?;
    openai_items(&value)
}

/// The Claude Agent family's built-in model registry: id, display name, context
/// window, and maximum output tokens, in the order the picker shows them.
///
/// It is a fixed table rather than the CLI's own `supportedModels()` because
/// that list is resolved against `~/.claude`'s account cache, which follows
/// whichever login the user last switched the CLI to and would drift the model
/// list with it. An `[1m]` id is a CLI concept — the same model run under a 1M
/// context budget — and is passed through to the SDK verbatim.
const CLAUDE_AGENT_MODELS: &[(&str, &str, u64, u64)] = &[
    ("claude-fable-5-1", "Claude Fable 5.1", 1_000_000, 128_000),
    ("claude-fable-5", "Claude Fable 5", 1_000_000, 128_000),
    ("claude-opus-5", "Claude Opus 5", 200_000, 128_000),
    (
        "claude-opus-5[1m]",
        "Claude Opus 5 (1M context)",
        1_000_000,
        128_000,
    ),
    ("claude-sonnet-5", "Claude Sonnet 5", 1_000_000, 128_000),
    ("claude-opus-4-8", "Claude Opus 4.8", 200_000, 128_000),
    (
        "claude-opus-4-8[1m]",
        "Claude Opus 4.8 (1M context)",
        1_000_000,
        128_000,
    ),
    ("claude-opus-4-7", "Claude Opus 4.7", 200_000, 128_000),
    (
        "claude-opus-4-7[1m]",
        "Claude Opus 4.7 (1M context)",
        1_000_000,
        128_000,
    ),
    ("claude-opus-4-6", "Claude Opus 4.6", 200_000, 128_000),
    (
        "claude-opus-4-6[1m]",
        "Claude Opus 4.6 (1M context)",
        1_000_000,
        128_000,
    ),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6", 200_000, 128_000),
    (
        "claude-sonnet-4-6[1m]",
        "Claude Sonnet 4.6 (1M context)",
        1_000_000,
        128_000,
    ),
    ("claude-opus-4-5", "Claude Opus 4.5", 200_000, 64_000),
    ("claude-opus-4-1", "Claude Opus 4.1", 200_000, 32_000),
    ("claude-sonnet-4-5", "Claude Sonnet 4.5", 200_000, 64_000),
    ("claude-haiku-4-5", "Claude Haiku 4.5", 200_000, 64_000),
];

/// The Claude Agent family's models, from [`CLAUDE_AGENT_MODELS`].
///
/// Limits and the vision capability travel through `raw` in the field shapes the shared
/// projection reads, so this table wins over [`crate::model_registry`] by the
/// same "upstream declaration beats catalog" rule every other leg obeys: it
/// pushes `claude-opus-5` back to 200k where the catalog says 1M, and it is the
/// only source for an `[1m]` twin, which the catalog necessarily misses.
fn claude_agent() -> Vec<Fetched> {
    CLAUDE_AGENT_MODELS
        .iter()
        .map(|(id, name, context_window, max_output_tokens)| {
            let raw = serde_json::json!({
                "context_window": context_window,
                "max_output_tokens": max_output_tokens,
                "supports_vision": true,
            });
            Fetched::new(*id).named(Some(name)).with_raw(&raw)
        })
        .collect()
}

// ───────────────────────────── Catalog-miss fallback ─────────────────────────────

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

fn u64_at_any(value: &Value, paths: &[&[&str]]) -> Option<u64> {
    paths.iter().find_map(|path| {
        value_at(value, path)
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
            })
            .filter(|value| *value > 0)
    })
}

fn bool_at_any(value: &Value, paths: &[&[&str]]) -> Option<bool> {
    paths
        .iter()
        .find_map(|path| value_at(value, path).and_then(Value::as_bool))
}

fn explicit_vision(raw: &Value) -> Option<bool> {
    bool_at_any(
        raw,
        &[
            &["supports_vision"],
            &["capabilities", "vision"],
            &["capabilities", "image_input"],
        ],
    )
}

fn declares_image_input(raw: &Value) -> bool {
    raw.get("input_modalities")
        .or_else(|| raw.pointer("/architecture/input_modalities"))
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .any(|item| matches!(item, "image" | "vision"))
        })
}

/// Capabilities explicitly declared by the upstream override the built-in
/// catalog because the relay knows what its model route supports.
fn declared_capabilities(raw: &Value) -> BTreeSet<ModelCapability> {
    let mut capabilities = BTreeSet::new();
    if explicit_vision(raw) == Some(true) || declares_image_input(raw) {
        capabilities.insert(ModelCapability::ImageRecognition);
    }
    capabilities
}

/// Explicitly denied capabilities override catalog values and inference so the
/// UI does not offer an operation that this route rejects.
fn denied_capabilities(raw: &Value) -> BTreeSet<ModelCapability> {
    let mut denied = BTreeSet::new();
    if explicit_vision(raw) == Some(false) {
        denied.insert(ModelCapability::ImageRecognition);
    }
    denied
}

/// Infer capabilities from the ID only when the built-in catalog has no match.
fn guessed_capabilities(id: &str) -> BTreeSet<ModelCapability> {
    let mut capabilities = BTreeSet::new();
    let lower = id.to_ascii_lowercase();
    if ["gpt-4o", "gpt-4.1", "claude-3", "claude-4", "vision"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        capabilities.insert(ModelCapability::ImageRecognition);
    }
    capabilities
}

const CONTEXT_WINDOW_PATHS: &[&[&str]] = &[
    &["limit", "context"],
    &["limits", "context"],
    &["context_window"],
    &["contextWindow"],
    &["context_length"],
    &["max_context_length"],
    &["max_input_tokens"],
    &["maxInputTokens"],
    &["limits", "context_window"],
    &["capabilities", "context_window"],
];

const MAX_OUTPUT_PATHS: &[&[&str]] = &[
    &["limit", "output"],
    &["limits", "output"],
    &["max_output_tokens"],
    &["maxOutputTokens"],
    &["max_completion_tokens"],
    &["max_tokens"],
    &["limits", "max_output_tokens"],
    &["capabilities", "max_output_tokens"],
];

// ───────────────────────────── Projection ─────────────────────────────

/// Merge model data in descending authority:
///
/// 1. Fields, capabilities, and limits declared by the upstream response.
/// 2. The built-in catalog ([`crate::model_registry`]).
/// 3. ID-based inference, only on a catalog miss.
///
/// Explicit upstream capability denials are removed last.
fn finish(provider: &ApiProvider, fetched: Vec<Fetched>) -> Vec<ModelProfile> {
    fetched
        .into_iter()
        .filter(|item| crate::model::validate_model_id(&item.id).is_ok())
        .map(|item| {
            let resolved = model_registry::resolve(&item.id);

            let mut capabilities = BTreeSet::<ModelCapability>::new();
            capabilities.extend(declared_capabilities(&item.raw));
            match &resolved {
                Some(resolution) => capabilities.extend(resolution.capabilities.iter().copied()),
                None => capabilities.extend(guessed_capabilities(&item.id)),
            }
            for denied in denied_capabilities(&item.raw) {
                capabilities.remove(&denied);
            }

            let context_window = u64_at_any(&item.raw, CONTEXT_WINDOW_PATHS)
                .or_else(|| resolved.as_ref().and_then(|value| value.context_window));
            let max_output_tokens = u64_at_any(&item.raw, MAX_OUTPUT_PATHS)
                .or_else(|| resolved.as_ref().and_then(|value| value.max_output_tokens));

            // Omit a display name that equals the ID so `display_name()` falls
            // back to the ID, matching manually added models.
            let name = item
                .name
                .or_else(|| resolved.as_ref().map(|value| value.name.clone()))
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty() && *value != item.id)
                .unwrap_or_default();

            ModelProfile {
                id: item.id.clone(),
                name,
                // Use the provider name when the catalog cannot derive a group;
                // an upstream fallback would expose an internal provider ID.
                group: model_registry::derive_model_group_name(&item.id)
                    .unwrap_or_else(|| provider.name.trim().to_owned()),
                context_window,
                max_output_tokens,
                capabilities,
                // A relay cannot infer reasoning shape from a model name, so
                // write the protocol's own default explicitly: the families with
                // a request-side control return ciphertext, the rest readable
                // text. The value is a model attribute the user can override.
                reasoning_content: if provider.family.reasoning_content_takes_effect() {
                    ReasoningContent::Encrypted
                } else {
                    ReasoningContent::Plaintext
                },
            }
        })
        .collect()
}

// ───────────────────────────── Entry point ─────────────────────────────

/// Fetch models.
///
/// Pressing the button always performs one GET after the Base URL validates.
/// Provider enablement and key presence do not preempt the upstream request;
/// callers retain the existing model list when fetching fails.
pub fn fetch_models(provider: &ApiProvider) -> Result<Vec<ModelProfile>, String> {
    validate_provider(provider)?;
    let fetched = match select(provider) {
        Fetcher::Anthropic => anthropic_with_relay_fallback(&discovery_for(provider)?),
        Fetcher::Codex => codex(&discovery_for(provider)?),
        Fetcher::OpenAiCompatible => open_ai_compatible(&discovery_for(provider)?),
        // No request at all: the list is a built-in registry.
        Fetcher::ClaudeAgent => Ok(claude_agent()),
    }?;
    Ok(finish(provider, dedup(fetched)))
}

/// Address and credential for one catalog fetch.
///
/// Key families read the optional stored key and the configured address. The
/// Codex family reads its OAuth session instead — there is no anonymous catalog
/// on that backend, so "not signed in" is a real error here rather than a GET
/// without `Authorization` — and substitutes its fixed backend for an empty
/// address, the same way `provider_base_url` does for conversations.
fn discovery_for(provider: &ApiProvider) -> Result<Discovery<'_>, String> {
    let client = crate::http_util::client()?;
    if provider.family == ProviderFamily::OpenaiCodex {
        let credentials = crate::codex_oauth::host().credentials(&provider.id)?;
        let base_url = crate::api::provider_base_url(provider)?
            .map(|url| url.to_string())
            .unwrap_or_else(|| provider.base_url.clone());
        return Ok(Discovery {
            provider,
            base_url,
            client,
            key: Some(credentials.access_token.to_string()),
            extra_headers: crate::codex_oauth::request_headers(&credentials),
        });
    }
    Ok(Discovery {
        provider,
        base_url: provider.base_url.clone(),
        client,
        key: optional_provider_key(provider),
        extra_headers: BTreeMap::new(),
    })
}

/// Project an OpenAI-compatible `GET /models` response directly into models.
/// This test-only entry isolates parsing from HTTP.
#[cfg(test)]
fn parse_openai_compatible(
    provider: &ApiProvider,
    value: &Value,
) -> Result<Vec<ModelProfile>, String> {
    Ok(finish(provider, dedup(openai_items(value)?)))
}

#[cfg(test)]
mod tests;
