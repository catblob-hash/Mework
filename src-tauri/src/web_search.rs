//! Host-side web search.
//!
//! `Native` delegates search to the current conversation model's provider-executed `web_search`
//! tool. Catalog providers call search APIs directly and return normalized title, URL, and content.
//! `web_fetch` consumes the catalog provider's URL-fetching capability.
//!
//! Provider secrets are stored in the operating-system credential store under reserved synthetic
//! provider IDs and reuse the API credential implementation's locking and identity semantics.

pub mod domain_rules;
pub mod pipeline;
pub mod providers;
pub mod readable;

use serde_json::{json, Value};

use crate::model::{ApiKeyStatus, SearchProviderKind};

/// Prefix for synthetic provider IDs. It reserves a namespace because provider ID alone determines
/// credential identity and binding; `storage::validate_shape` rejects ordinary providers using it.
pub const SEARCH_PROVIDER_ID_PREFIX: &str = "search-provider:";

/// Maximum output for one `Native` call. OpenAI reasoning models require at least 8192 tokens
/// to avoid returning incomplete output without prose.
pub const SEARCH_CALL_MAX_OUTPUT_TOKENS: u64 = 8192;

/// A secret slot that a search provider may use.
/// Separate slots reflect independent lifecycles: a self-hosted SearXNG instance changes its
/// Basic Auth credentials together and does not use an API key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSlot {
    ApiKey,
    /// Used only by `searxng` for a self-hosted instance's Basic Auth password.
    BasicAuthPassword,
}

impl CredentialSlot {
    pub const ALL: &'static [Self] = &[Self::ApiKey, Self::BasicAuthPassword];

    pub fn slug(self) -> &'static str {
        match self {
            Self::ApiKey => "apiKey",
            Self::BasicAuthPassword => "basicAuthPassword",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|slot| slot.slug() == slug)
    }

    /// Suffix of the synthetic provider ID. API-key IDs retain their existing identity so users
    /// need not re-enter keys for providers with the same slug.
    fn suffix(self) -> &'static str {
        match self {
            Self::ApiKey => "",
            Self::BasicAuthPassword => ":basic-auth",
        }
    }
}

/// Builds a synthetic provider ID for a catalog credential slot.
pub fn credential_id(kind: SearchProviderKind, slot: CredentialSlot) -> String {
    format!(
        "{SEARCH_PROVIDER_ID_PREFIX}{}{}",
        kind.slug(),
        slot.suffix()
    )
}

/// Catalog names of the two web tools. They are host-derived: the conversation
/// switch decides them, never the persisted enabled-tool list. Mirrored by TS
/// `src/lib/taskTools.ts::WEB_TOOL_NAMES`.
pub const WEB_TOOL_NAMES: [&str; 2] = ["web_search", "web_fetch"];

pub fn is_web_tool_name(name: &str) -> bool {
    WEB_TOOL_NAMES.contains(&name)
}

/// Whether a protocol family supports provider-executed native `web_search`.
/// This table must match `aisdk-service/src/search.ts` exactly. A mismatch silently lets the host
/// accept a request while the sidecar omits the tool.
pub fn family_supports_native_search(family: crate::aisdk::protocol::Family) -> bool {
    use crate::aisdk::protocol::Family;
    match family {
        Family::OpenaiResponses
        // The Codex backend accepts the Responses `web_search` tool (verified live).
        | Family::OpenaiCodex
        | Family::Azure
        | Family::Anthropic
        | Family::Bedrock
        | Family::Google
        | Family::Vertex
        | Family::Xai => true,
        // Chat Completions has no provider-executed tools; generic OpenAI-compatible providers
        // discard them with an `unsupported` warning. Claude Code's built-in tools are all
        // switched off for the agent family, its `WebSearch` included.
        Family::OpenaiChat | Family::OpenaiCompatible | Family::ClaudeAgent => false,
    }
}

/// Whether a protocol family has a server-side page-fetch tool the host can
/// read text out of.
///
/// This is the asymmetry the whole fetch-provider setting exists for. Anthropic
/// is the only family that both exposes fetching as its own server tool and
/// returns the page as plain text: a `web_fetch_result` carries
/// `content.source.{type:"text", media_type:"text/plain", data}`, so the host
/// parses the page itself. Every other family either has no fetch tool at all
/// (OpenAI Responses, xAI, Google) or keeps retrieval internal to its search
/// tool, and those conversations have to borrow a catalog fetch provider.
///
/// Search is deliberately NOT symmetrical with this: no family's *search*
/// results contain readable page text. Anthropic seals them in
/// `encrypted_content`, and Responses never returns them at all.
/// This table must match `aisdk-service/src/search.ts::familySupportsNativeFetch`.
pub fn family_supports_native_fetch(family: crate::aisdk::protocol::Family) -> bool {
    use crate::aisdk::protocol::Family;
    match family {
        Family::Anthropic | Family::Bedrock => true,
        Family::OpenaiResponses
        | Family::OpenaiCodex
        | Family::Azure
        | Family::Google
        | Family::Vertex
        | Family::Xai
        | Family::OpenaiChat
        | Family::OpenaiCompatible
        | Family::ClaudeAgent => false,
    }
}

/// Whether this family spells its native web tools the Messages way, as a
/// versioned `type` on the tool definition.
///
/// Every family that has native web tools at all has exactly one shape for
/// them; only Messages makes the version part of the wire, and only there is
/// there anything for the user to pick. A family outside this table quietly
/// sends its own native tool and ignores whichever version the conversation is
/// carrying — the selection is kept, not rewritten, so returning to a Messages
/// model returns to the version that was chosen.
pub fn family_selects_native_tool_type(family: crate::aisdk::protocol::Family) -> bool {
    use crate::aisdk::protocol::Family;
    match family {
        Family::Anthropic | Family::Bedrock => true,
        Family::OpenaiResponses
        | Family::OpenaiCodex
        | Family::Azure
        | Family::Google
        | Family::Vertex
        | Family::Xai
        | Family::OpenaiChat
        | Family::OpenaiCompatible
        // The agent family makes its own Messages calls inside the CLI, so the
        // host never writes a tool definition for it to carry a version on.
        | Family::ClaudeAgent => false,
    }
}

/// Rewrites a persisted enabled-tool list into the one a trusted request uses.
///
/// Strip first, then derive — the same rule the memory tiers and the
/// task-runtime tools follow, so a name the renderer wrote can never grant web
/// access and a name it forgot can never withhold it.
///
/// `web_search` is granted whenever the switch is on and the conversation named
/// a search backend at all, even when that backend does not currently resolve:
/// an unconfigured or broken backend is a repairable error the model can report
/// to the user, which is more useful than a tool that silently is not there.
/// Naming no backend is the one case that withholds it, because there is
/// nothing to repair and nothing to report — the user said this conversation
/// does not search.
/// `web_fetch` follows the same shape on its own leg: it appears only when a
/// fetch backend actually resolved, so a conversation whose upstream keeps page
/// retrieval inside its search tool shows the model exactly one web tool, the
/// same shape that upstream has.
pub fn apply_web_tools(
    enabled_tools: &mut Vec<String>,
    web_search_enabled: bool,
    resolved: &crate::model::WebSearchSettings,
) {
    enabled_tools.retain(|name| !is_web_tool_name(name));
    if !web_search_enabled {
        return;
    }
    if !resolved.search_withheld {
        enabled_tools.push("web_search".to_owned());
    }
    if resolved.fetch.is_some() {
        enabled_tools.push("web_fetch".to_owned());
    }
}

/// A normalized search or fetch result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResultItem {
    pub title: String,
    pub content: String,
    pub url: String,
    /// Input that produced this result, either a query or URL.
    pub source_input: String,
}

/// A search or fetch failure. The variants distinguish non-retryable configuration errors,
/// retryable transient failures, and cancellation.
#[derive(Debug)]
pub enum SearchError {
    /// Configuration prevents success until changed.
    Config(String),
    /// This attempt failed, but retrying may succeed.
    Transient(String),
    /// The parent sink closed while the turn is settling. Propagate cancellation unchanged rather
    /// than converting it to a tool failure, which would issue another request for a cancelled turn.
    Cancelled(String),
}

impl SearchError {
    pub fn message(&self) -> &str {
        match self {
            Self::Config(message) | Self::Transient(message) | Self::Cancelled(message) => message,
        }
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Projects results into the JSON delivered to the model. Each call uses a random prefix plus an
/// ordinal citation ID so multiple searches in one message cannot collide.
pub fn tool_output(items: &[SearchResultItem]) -> Value {
    let prefix = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect::<String>();
    Value::Array(
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                json!({
                    "id": format!("{prefix}-{}", index + 1),
                    "title": item.title,
                    "url": item.url,
                    "content": item.content,
                })
            })
            .collect(),
    )
}

// ----------------------------------------------------------------- Credential commands
//
// Commands accept only catalog kind slugs and known credential slots, preventing orphaned
// credential records with no read path.

fn credential_target(kind_slug: &str, slot_slug: &str) -> Result<String, String> {
    let kind = SearchProviderKind::from_slug(kind_slug)
        .ok_or_else(|| format!("未知的搜索提供商：{kind_slug}"))?;
    let slot = CredentialSlot::from_slug(slot_slug)
        .ok_or_else(|| format!("未知的搜索凭据槽：{slot_slug}"))?;
    if slot == CredentialSlot::BasicAuthPassword && kind != SearchProviderKind::Searxng {
        return Err(format!("{} 没有 Basic Auth 凭据", kind.label()));
    }
    Ok(credential_id(kind, slot))
}

pub fn save_provider_api_key(
    kind_slug: &str,
    slot_slug: &str,
    api_key: &str,
) -> Result<ApiKeyStatus, String> {
    let target = credential_target(kind_slug, slot_slug)?;
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("搜索提供商凭据不能为空".into());
    }
    // Keys enter HTTP authorization headers, where control characters are header-injection primitives.
    if api_key.chars().any(char::is_control) {
        return Err("搜索提供商凭据不能包含控制字符".into());
    }
    crate::api::save_api_key(&target, api_key)
}

pub fn get_provider_key_status(kind_slug: &str, slot_slug: &str) -> Result<ApiKeyStatus, String> {
    crate::api::api_key_status(&credential_target(kind_slug, slot_slug)?)
}

/// Returns plaintext only for the explicit reveal command.
pub fn reveal_provider_api_key(kind_slug: &str, slot_slug: &str) -> Result<String, String> {
    crate::api::reveal_api_key(&credential_target(kind_slug, slot_slug)?)
}

pub fn delete_provider_api_key(kind_slug: &str, slot_slug: &str) -> Result<ApiKeyStatus, String> {
    crate::api::delete_api_key(&credential_target(kind_slug, slot_slug)?)
}

/// Reads a secret for a search. Absence is not an error because several catalog providers support
/// anonymous access; the driver decides whether a key is required.
pub fn stored_secret(kind: SearchProviderKind, slot: CredentialSlot) -> Option<String> {
    crate::api::api_key_status(&credential_id(kind, slot))
        .ok()
        .filter(|status| status.configured)
        .and_then(|_| crate::api::reveal_api_key(&credential_id(kind, slot)).ok())
        .map(|secret| secret.trim().to_owned())
        .filter(|secret| !secret.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_provider_ids_are_distinct_and_namespaced() {
        let mut ids = Vec::new();
        for kind in SearchProviderKind::CATALOG.iter().copied() {
            for slot in CredentialSlot::ALL.iter().copied() {
                ids.push(credential_id(kind, slot));
            }
        }
        assert_eq!(ids.len(), 20);
        // Each provider-slot pair has one stable, distinct identity.
        let unique = ids.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), ids.len());
        for id in &ids {
            // `storage::validate_shape` reserves this namespace; assert that generated IDs use it.
            assert!(id.starts_with(SEARCH_PROVIDER_ID_PREFIX));
        }
        // API-key IDs have no suffix to retain existing identities.
        assert_eq!(
            credential_id(SearchProviderKind::Tavily, CredentialSlot::ApiKey),
            "search-provider:tavily"
        );
    }

    /// Compares every host capability decision with the sidecar table.
    /// The test reads `search.ts` so a one-sided family addition fails here rather than silently
    /// producing a search request without a provider tool.
    #[test]
    fn the_native_search_capability_table_matches_the_sidecar() {
        use crate::aisdk::protocol::Family;

        let search_ts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("aisdk-service")
            .join("src")
            .join("search.ts");
        let source = std::fs::read_to_string(&search_ts)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", search_ts.display()));
        let body_start = source
            .find("export function familySupportsNativeSearch")
            .expect("familySupportsNativeSearch is missing from the sidecar");
        let body = &source[body_start..];
        let body_end = body
            .find(
                "
}",
            )
            .expect("function body has no ending");
        let body = &body[..body_end];

        let all = [
            (Family::OpenaiResponses, "openai-responses"),
            (Family::OpenaiCodex, "openai-codex"),
            (Family::OpenaiChat, "openai-chat"),
            (Family::Anthropic, "anthropic"),
            (Family::ClaudeAgent, "claude-agent"),
            (Family::Google, "google"),
            (Family::Xai, "xai"),
            (Family::Azure, "azure"),
            (Family::Bedrock, "bedrock"),
            (Family::Vertex, "vertex"),
            (Family::OpenaiCompatible, "openai-compatible"),
        ];
        for (family, slug) in all {
            let sidecar_says = body.contains(&format!("family === \"{slug}\""));
            assert_eq!(
                family_supports_native_search(family),
                sidecar_says,
                "{slug}: host and sidecar disagree about native-search support"
            );
        }
        // Require both enabled and disabled families so matching constant tables cannot pass.
        assert!(family_supports_native_search(Family::Anthropic));
        assert!(!family_supports_native_search(Family::OpenaiChat));
    }

    /// The fetch capability table has the same one-sided-drift hazard as the
    /// search one: if the host believes a family can fetch and the sidecar does
    /// not attach the tool, the request succeeds and silently retrieves nothing.
    #[test]
    fn the_native_fetch_capability_table_matches_the_sidecar() {
        use crate::aisdk::protocol::Family;

        let search_ts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("aisdk-service")
            .join("src")
            .join("search.ts");
        let source = std::fs::read_to_string(&search_ts)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", search_ts.display()));
        let body_start = source
            .find("export function familySupportsNativeFetch")
            .expect("familySupportsNativeFetch is missing from the sidecar");
        let body = &source[body_start..];
        let body_end = body
            .find(
                "
}",
            )
            .expect("function body has no ending");
        let body = &body[..body_end];

        let all = [
            (Family::OpenaiResponses, "openai-responses"),
            (Family::OpenaiCodex, "openai-codex"),
            (Family::OpenaiChat, "openai-chat"),
            (Family::Anthropic, "anthropic"),
            (Family::ClaudeAgent, "claude-agent"),
            (Family::Google, "google"),
            (Family::Xai, "xai"),
            (Family::Azure, "azure"),
            (Family::Bedrock, "bedrock"),
            (Family::Vertex, "vertex"),
            (Family::OpenaiCompatible, "openai-compatible"),
        ];
        for (family, slug) in all {
            let sidecar_says = body.contains(&format!("family === \"{slug}\""));
            assert_eq!(
                family_supports_native_fetch(family),
                sidecar_says,
                "{slug}: host and sidecar disagree about native-fetch support"
            );
        }
        // Fetching is the narrower capability: a family can search server-side
        // without being able to fetch, and that asymmetry is the reason the
        // fetch-provider setting exists at all.
        assert!(family_supports_native_fetch(Family::Anthropic));
        assert!(!family_supports_native_fetch(Family::OpenaiResponses));
        assert!(family_supports_native_search(Family::OpenaiResponses));
    }

    /// Every Messages tool version the host can name must be one the sidecar
    /// can actually build. The AI SDK drops a provider-defined tool it cannot
    /// map, with a warning rather than an error, so a version listed on only
    /// one side would reach the model as no web tool at all — the same
    /// one-sided-drift hazard as the two capability tables, one level down.
    #[test]
    fn the_native_tool_versions_match_the_sidecar() {
        use crate::model::{NativeFetchTool, NativeSearchTool};

        let search_ts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("aisdk-service")
            .join("src")
            .join("search.ts");
        let source = std::fs::read_to_string(&search_ts)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", search_ts.display()));

        for version in NativeSearchTool::ALL {
            assert!(
                source.contains(&format!("{}:", version.wire_type())),
                "{}: the host offers a search version the sidecar cannot build",
                version.wire_type()
            );
        }
        for version in NativeFetchTool::ALL {
            assert!(
                source.contains(&format!("{}:", version.wire_type())),
                "{}: the host offers a fetch version the sidecar cannot build",
                version.wire_type()
            );
        }
        // The fallback each table is keyed against is the basic version, and it
        // is the default a conversation carries until the user picks otherwise.
        assert_eq!(
            NativeSearchTool::default().wire_type(),
            "web_search_20250305"
        );
        assert_eq!(NativeFetchTool::default().wire_type(), "web_fetch_20250910");
        assert!(source.contains("\"web_search_20250305\","));
        assert!(source.contains("\"web_fetch_20250910\","));
    }

    /// An unknown version reads as the default instead of failing the document,
    /// so a conversation written by a build that knew another version still
    /// opens here.
    #[test]
    fn an_unknown_native_tool_version_falls_back_to_the_default() {
        use crate::model::{ConversationWebSearchSettings, NativeFetchTool, NativeSearchTool};

        let settings: ConversationWebSearchSettings = serde_json::from_value(serde_json::json!({
            "nativeSearchTool": "web_search_20990101",
            "nativeFetchTool": null,
        }))
        .expect("an unknown version must not fail the conversation");
        assert_eq!(settings.native_search_tool, NativeSearchTool::default());
        assert_eq!(settings.native_fetch_tool, NativeFetchTool::default());

        let chosen: ConversationWebSearchSettings = serde_json::from_value(serde_json::json!({
            "nativeSearchTool": "web_search_20260209",
            "nativeFetchTool": "web_fetch_20260209",
        }))
        .expect("a known version round-trips");
        assert_eq!(
            chosen.native_search_tool,
            NativeSearchTool::WebSearch20260209
        );
        assert_eq!(chosen.native_fetch_tool, NativeFetchTool::WebFetch20260209);
        assert_eq!(
            serde_json::to_value(chosen.native_search_tool).unwrap(),
            serde_json::json!("web_search_20260209")
        );
    }

    /// The pair is derived, never taken from the persisted list — the same
    /// strip-then-derive rule the memory tiers and task-runtime tools follow.
    #[test]
    fn the_web_tools_are_stripped_then_derived_from_the_switch() {
        use crate::model::{
            ResolvedSearchProvider, SearchBackend, SearchFetchBackend, SearchProviderKind,
            WebSearchSettings,
        };

        let settings = |fetch: Option<SearchFetchBackend>| WebSearchSettings {
            backend: Some(SearchBackend::Native),
            fetch,
            ..Default::default()
        };
        let derive = |names: &[&str], enabled: bool, fetch: Option<SearchFetchBackend>| {
            let mut tools = names.iter().map(|name| (*name).to_owned()).collect();
            apply_web_tools(&mut tools, enabled, &settings(fetch));
            tools
        };

        // Switched off, a persisted name is removed rather than honoured: the
        // renderer cannot grant web access by writing a tool name.
        assert_eq!(
            derive(&["read", "web_search", "web_fetch"], false, None),
            vec!["read"]
        );

        // Switched on, the pair appears even though the renderer never wrote it.
        assert_eq!(
            derive(&["read"], true, Some(SearchFetchBackend::Native)),
            vec!["read", "web_search", "web_fetch"]
        );

        // With no fetch backend the model sees exactly one web tool — the shape
        // DeepSeek and OpenAI actually have, where retrieval lives inside search.
        assert_eq!(derive(&["read"], true, None), vec!["read", "web_search"]);

        // A catalog fetch provider is a fetch backend like any other.
        assert_eq!(
            derive(
                &[],
                true,
                Some(SearchFetchBackend::Provider(ResolvedSearchProvider {
                    kind: SearchProviderKind::Jina,
                    api_host: "https://r.jina.ai".into(),
                    engines: Vec::new(),
                    basic_auth_username: String::new(),
                }))
            ),
            vec!["web_search", "web_fetch"]
        );

        // Idempotent: deriving over an already-derived list adds nothing.
        let mut twice = derive(&["read"], true, Some(SearchFetchBackend::Native));
        apply_web_tools(
            &mut twice,
            true,
            &settings(Some(SearchFetchBackend::Native)),
        );
        assert_eq!(twice, vec!["read", "web_search", "web_fetch"]);
    }

    /// Withholding the search tool is the one thing a resolved-to-nothing
    /// backend does NOT do, so the two cases are asserted against each other.
    #[test]
    fn a_withheld_search_leg_leaves_the_fetch_tool_standing_alone() {
        use crate::model::{SearchFetchBackend, WebSearchSettings};

        let derive = |withheld: bool, fetch: Option<SearchFetchBackend>| {
            let mut tools = vec!["read".to_owned()];
            apply_web_tools(
                &mut tools,
                true,
                &WebSearchSettings {
                    backend: None,
                    search_withheld: withheld,
                    fetch,
                    ..Default::default()
                },
            );
            tools
        };

        // A backend that did not resolve still hands the tool over: the call
        // fails with something the user can act on, which beats a tool that is
        // silently absent.
        assert_eq!(derive(false, None), vec!["read", "web_search"]);

        // Naming no search backend is not a failure to report. The conversation
        // keeps whatever fetch leg it named, and nothing else.
        assert_eq!(
            derive(true, Some(SearchFetchBackend::Native)),
            vec!["read", "web_fetch"]
        );

        // Both legs off, with web access still on, is a conversation whose
        // settings say plainly that it has no web tools.
        assert_eq!(derive(true, None), vec!["read"]);
    }

    #[test]
    fn a_credential_command_refuses_a_catalog_outsider() {
        assert!(save_provider_api_key("brave", "apiKey", "secret").is_err());
        assert!(get_provider_key_status("openai", "apiKey").is_err());
        assert!(reveal_provider_api_key("", "apiKey").is_err());
        assert!(delete_provider_api_key("deepseek", "apiKey").is_err());
        // Slot names are also a closed set.
        assert!(save_provider_api_key("tavily", "password", "secret").is_err());
        // Basic Auth belongs only to SearXNG, preventing credentials no consumer can read.
        assert!(save_provider_api_key("tavily", "basicAuthPassword", "secret").is_err());
        assert!(credential_target("searxng", "basicAuthPassword").is_ok());
    }

    #[test]
    fn a_provider_api_key_must_be_a_single_header_safe_line() {
        for refused in ["sk-a\nb", "sk-a\rb", "sk-a\tb", "sk-a\0b"] {
            assert!(
                save_provider_api_key("tavily", "apiKey", refused).is_err(),
                "{refused:?} must be refused"
            );
        }
        assert!(save_provider_api_key("tavily", "apiKey", "   ").is_err());
    }

    #[test]
    fn the_tool_output_carries_a_per_call_citation_prefix() {
        let items = vec![
            SearchResultItem {
                title: "A".into(),
                content: "a".into(),
                url: "https://a.example".into(),
                source_input: "q".into(),
            },
            SearchResultItem {
                title: "B".into(),
                content: "b".into(),
                url: "https://b.example".into(),
                source_input: "q".into(),
            },
        ];
        let first = tool_output(&items);
        let second = tool_output(&items);
        let id_of = |value: &Value, index: usize| {
            value[index]["id"]
                .as_str()
                .expect("id is a string")
                .to_owned()
        };
        assert!(id_of(&first, 0).ends_with("-1"));
        assert!(id_of(&first, 1).ends_with("-2"));
        // Citation IDs share a prefix within a call and never collide across calls.
        let prefix = |id: String| id.split('-').next().unwrap_or_default().to_owned();
        assert_eq!(prefix(id_of(&first, 0)), prefix(id_of(&first, 1)));
        assert_ne!(prefix(id_of(&first, 0)), prefix(id_of(&second, 0)));
    }
}
