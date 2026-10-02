//! Tools that join a conversation after its first request.
//!
//! Every request declares a tool list, and every protocol puts that list at
//! the head of the prompt, ahead of the system prompt and the history. A tool
//! that joins mid-conversation — the user ticks it in the settings, the run
//! arms the handoff tools, `tool_search` hands out an MCP schema — used to be
//! written into that list, which rewrote the head of the prompt and threw the
//! whole prompt cache away. Here it is appended instead: the transcript records
//! the point it joined at, and each protocol that has a tool-append interface
//! hands the tool over there, at the end of the transcript as it stood.
//!
//! - Anthropic Messages: a mid-conversation `role: "system"` message carrying
//!   `tool_addition` blocks, the tool itself declared with `defer_loading`.
//! - OpenAI Responses (and Azure, and the Codex backend): an `additional_tools`
//!   input item with the tool's definition.
//! - Every other protocol has no such interface; the sidecar folds the tool
//!   back into the declared list, which is what happened before.
//!
//! Whether a model takes the interface at its endpoint is a capability the
//! model declares, as it declares vision: Mework fills it in where it knows
//! ([`known`]), the user where it does not — a relay that may or may not pass
//! the interface on. The sidecar appends only where it is declared.
//!
//! The host's part is protocol-neutral: decide which tools are new, record the
//! point, and project it as one marker message. The sidecar turns the marker
//! into the protocol's own shape (`aisdk-service/src/tool-append.ts`). No text
//! goes with it — the tool simply becomes available.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::model::{ApiProvider, ContextItem, ModelCapability, ModelProfile, ProviderFamily};

/// Whether a conversation on this model, at this provider's endpoint, can take
/// a tool mid-conversation at all: the model declares the `ToolAppend`
/// capability — filled in by Mework where it knows ([`known`]), by the user
/// everywhere else — and its protocol has an append interface to read it for.
///
/// Where it cannot, the conversation's tool surface is fixed from its first
/// request on — the renderer locks it (`src/lib/toolLock.ts`), auto-compact
/// never arms (its four tools would join mid-run) and MCP tool discovery is off
/// (a fetched schema would join mid-run). The sidecar hands an appended tool
/// over through the protocol's interface only where this holds
/// (`StepRequest.tool_append`). Mirrored by TS `appendsTools` in
/// `src/lib/modelCapabilities.ts`.
pub(crate) fn appends_tools(provider: &ApiProvider, model: &ModelProfile) -> bool {
    provider.family.tool_append_takes_effect() && model.has(ModelCapability::ToolAppend)
}

/// What Mework knows about this model taking a tool mid-conversation at this
/// endpoint: `Some` where it knows, `None` where only the user can say. A
/// model is declared `ToolAppend` from this once, when it is fetched or
/// seeded (and once for the models a schema-3 document already held); the
/// declaration is the model's from then on, for the user to change.
///
/// It knows a protocol with no append interface (`false`), the Codex backend
/// (`true`: its address is ChatGPT's or a local stand-in for it), Claude
/// Code's own catalogue for the CLI, and at the vendor's own endpoint
/// ([`crate::host_append::at_vendor_endpoint`]) what the vendor documents:
/// Responses and Azure take `additional_tools` from every model, Anthropic's
/// Messages API takes `tool_addition` from the models in its list. A relay in
/// front of either may or may not pass the interface on, so there — and for a
/// model the vendor's documentation does not cover — it does not know.
pub(crate) fn known(family: ProviderFamily, base_url: &str, model_id: &str) -> Option<bool> {
    let vendor = crate::host_append::at_vendor_endpoint(family, base_url);
    match family {
        ProviderFamily::OpenaiCodex => Some(true),
        ProviderFamily::OpenaiResponses | ProviderFamily::Azure => vendor.then_some(true),
        ProviderFamily::Anthropic => vendor.then(|| anthropic_documents(model_id)).flatten(),
        ProviderFamily::ClaudeAgent => Some(names_model(model_id, CLAUDE_CODE_TOOL_CHANGE_MODELS)),
        ProviderFamily::OpenaiChat
        | ProviderFamily::Google
        | ProviderFamily::Xai
        | ProviderFamily::Bedrock
        | ProviderFamily::Vertex
        | ProviderFamily::OpenaiCompatible => Some(false),
    }
}

/// Models the Messages API takes `tool_addition` from (Anthropic's
/// mid-conversation tool-change documentation).
const ANTHROPIC_TOOL_CHANGE_MODELS: &[&str] = &[
    "fable-5", "mythos-5", "opus-5", "opus-4-8", "sonnet-5-5",
];
/// Earlier Claude models, which the same documentation leaves out: every
/// generation before 4.8, matched whole (no later model will join them).
const ANTHROPIC_EARLIER_MODELS: &[&str] = &[
    "claude-instant", "claude-2", "claude-3", "opus-4", "sonnet-4", "haiku-4", "haiku-3",
];
/// Current models the documentation names as without it, matched as released
/// (a dated snapshot, never a later minor version).
const ANTHROPIC_RELEASES_WITHOUT: &[&str] = &["sonnet-5"];

/// What Anthropic documents about this model taking a system message — and
/// so a tool change, which rides in one — mid-conversation at its own
/// endpoint: `Some(true)` for the models in its list, `Some(false)` for the
/// Claude models it leaves out, `None` for a model it says nothing about.
/// Anthropic documents system messages and tool changes for the same models,
/// so `system_append::known` asks this too.
pub(crate) fn anthropic_documents(model_id: &str) -> Option<bool> {
    if names_model(model_id, ANTHROPIC_TOOL_CHANGE_MODELS) {
        Some(true)
    } else if names_model(model_id, ANTHROPIC_EARLIER_MODELS)
        || names_release(model_id, ANTHROPIC_RELEASES_WITHOUT)
    {
        Some(false)
    } else {
        None
    }
}

/// Models Claude Code itself appends tools for (`mid_conv_tool_change` in the
/// CLI's model catalogue): the Messages list without Sonnet. The bundled CLI
/// is pinned, so its catalogue is a fact: every other model is `false`.
const CLAUDE_CODE_TOOL_CHANGE_MODELS: &[&str] = &["fable-5", "mythos-5", "opus-5", "opus-4-8"];

/// The id as [`names_model`] reads it: case, `.` for `-` and Claude Code's
/// `[1m]` suffix do not matter.
fn normalized_model_id(model_id: &str) -> String {
    model_id
        .to_ascii_lowercase()
        .replace('.', "-")
        .trim_end_matches("[1m]")
        .to_owned()
}

/// Whether `model_id` names one of `families`, followed by whatever `rest_ok`
/// accepts, the family appearing whole (at the start or after a `-`).
fn names_model_with(model_id: &str, families: &[&str], rest_ok: impl Fn(&str) -> bool) -> bool {
    let id = normalized_model_id(model_id);
    families.iter().any(|family| {
        id.match_indices(family).any(|(at, _)| {
            let before_ok = at == 0 || id.as_bytes()[at - 1] == b'-';
            before_ok && rest_ok(&id[at + family.len()..])
        })
    })
}

/// Whether `model_id` names one of `families`: the family appears whole,
/// followed by the end of the id or another `-` segment (a minor version, a
/// date).
fn names_model(model_id: &str, families: &[&str]) -> bool {
    names_model_with(model_id, families, |rest| rest.is_empty() || rest.starts_with('-'))
}

/// Whether `model_id` names one of `families` as released: followed by the
/// end of the id or a dated snapshot (`-YYYYMMDD`), never a minor version.
fn names_release(model_id: &str, families: &[&str]) -> bool {
    names_model_with(model_id, families, |rest| {
        rest.is_empty()
            || rest
                .strip_prefix('-')
                .is_some_and(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

/// The `kind` in the marker's context id (`ctx_tools-added_<uuid>`).
pub(crate) const CONTEXT_KIND: &str = "tools-added";

/// The key under the marker message's `providerOptions` that names it as one.
/// The sidecar recognises the marker by it and never passes it on.
const MARKER_OPTIONS_KEY: &str = "mework";
const MARKER_TOOLS_KEY: &str = "toolAddition";

/// The tools a context records as having joined at its point, if it is a
/// marker.
pub(crate) fn added_tools(context: &ContextItem) -> &[String] {
    match context {
        ContextItem::System { tools_added, .. } => tools_added,
        _ => &[],
    }
}

/// Every tool the transcript has already appended somewhere.
pub(crate) fn appended<'a>(contexts: impl IntoIterator<Item = &'a ContextItem>) -> HashSet<String> {
    contexts
        .into_iter()
        .flat_map(|context| added_tools(context).iter().cloned())
        .collect()
}

/// The tools this request offers that are new since the previous one.
///
/// `previous` is the set the previous request of this transcript offered.
/// Without it — the first request, or the first after a restart — nothing
/// counts as new: whatever is offered goes into the declared list, which is
/// exactly what the previous request would have declared had it been known. A
/// tool the transcript already appended is never new again; its marker keeps
/// handing it over from where it stands.
pub(crate) fn newly_offered(
    offered: &[String],
    previous: Option<&BTreeSet<String>>,
    appended: &HashSet<String>,
) -> Vec<String> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    offered
        .iter()
        .filter(|name| !previous.contains(*name) && !appended.contains(*name))
        .filter(|name| seen.insert(name.as_str()))
        .cloned()
        .collect()
}

/// The transcript record of `tools` joining here.
///
/// Local-only: the text is for a person reading an export, never for the
/// model, and the renderer draws no row for it. What the model gets is the
/// protocol's own tool addition, projected from `tools_added`.
pub(crate) fn marker(id: String, tools: Vec<String>, created_at: String) -> ContextItem {
    ContextItem::System {
        id,
        content: format!("Tools added: {}", tools.join(", ")),
        local_only: true,
        hook_execution: None,
        tools_added: tools,
        created_at,
    }
}

/// The marker as a `ModelMessage`: a system message with no text whose
/// provider options name the tools. The sidecar replaces it with the
/// protocol's tool addition, or drops it where the protocol has none.
pub(crate) fn marker_message(tools: &[String]) -> Value {
    json!({
        "role": "system",
        "content": "",
        "providerOptions": { MARKER_OPTIONS_KEY: { MARKER_TOOLS_KEY: tools } },
    })
}

/// The tool set each conversation's last request offered, so the first
/// request of the next run can tell a tool the user just enabled from one the
/// declared list already had.
///
/// Process-local on purpose. Losing it (a restart) costs one request whose
/// declared list takes a newly enabled tool in — the old behaviour, correct
/// and uncached — and nothing else; persisting it would put a second record of
/// the tool set beside the transcript for the two to disagree about.
///
/// Clones share one map, as every other registry on `AppState` does.
#[derive(Clone, Default)]
pub(crate) struct OfferedToolRegistry {
    offered: Arc<Mutex<HashMap<String, BTreeSet<String>>>>,
}

impl OfferedToolRegistry {
    pub(crate) fn last(&self, conversation_id: &str) -> Option<BTreeSet<String>> {
        self.offered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(conversation_id)
            .cloned()
    }

    pub(crate) fn record(&self, conversation_id: &str, offered: BTreeSet<String>) {
        self.offered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(conversation_id.to_owned(), offered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// The fixture both sides of the mirror check (`modelCapabilities.test.ts`):
    /// family, base URL, model, then what Mework knows about appending a tool
    /// ([`known`]) and a system prompt (`system_append::known`) there. `None`
    /// is the user's to say.
    const APPEND_FIXTURE: &[(ProviderFamily, &str, &str, Option<bool>, Option<bool>)] = {
        use crate::model::ProviderFamily::*;
        &[
            (Anthropic, "https://api.anthropic.com/v1", "claude-opus-5-5", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-opus-5-20260201", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-opus-4-8", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-opus-4-5", Some(false), Some(false)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-5-5", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-5", Some(false), Some(false)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-5-20260115", Some(false), Some(false)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-4.5", Some(false), Some(false)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-fable-5-1", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-mythos-5", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-haiku-4-5", Some(false), Some(false)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-3-5-sonnet-20241022", Some(false), Some(false)),
            (Anthropic, "https://API.Anthropic.com", "claude-opus-5-5", Some(true), Some(true)),
            (Anthropic, "", "claude-opus-5-5", Some(true), Some(true)),
            (Anthropic, "https://api.anthropic.com/v1", "claude-haiku-5", None, None),
            (Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-5-6", None, None),
            (Anthropic, "https://relay.example.com/v1", "claude-opus-5-5", None, None),
            (Anthropic, "https://relay.example.com/v1", "claude-haiku-4-5", None, None),
            (ClaudeAgent, "", "claude-opus-5-5[1m]", Some(true), Some(false)),
            (ClaudeAgent, "", "claude-sonnet-5-5", Some(false), Some(false)),
            (ClaudeAgent, "", "claude-fable-5", Some(true), Some(false)),
            (OpenaiResponses, "https://api.openai.com/v1", "gpt-5.5", Some(true), Some(true)),
            (OpenaiResponses, "https://relay.example.com/v1", "gpt-5.5", None, Some(true)),
            (OpenaiCodex, "", "gpt-5.6-codex", Some(true), Some(true)),
            (OpenaiCodex, "http://127.0.0.1:9000/codex", "gpt-5.6-codex", Some(true), Some(true)),
            (Azure, "https://mine.openai.azure.com/openai", "gpt-5.4", Some(true), Some(true)),
            (Azure, "https://gateway.example.com/openai", "gpt-5.4", None, Some(true)),
            (OpenaiChat, "https://api.openai.com/v1", "gpt-5.5", Some(false), Some(true)),
            (OpenaiChat, "https://api.deepseek.com", "deepseek-v4", Some(false), None),
            (OpenaiCompatible, "https://api.deepseek.com", "deepseek-v4", Some(false), None),
            (Google, "", "gemini-3-pro", Some(false), Some(false)),
            (Xai, "https://api.x.ai/v1", "grok-5", Some(false), None),
            (Bedrock, "", "anthropic.claude-opus-5-5", Some(false), Some(false)),
            (Vertex, "", "gemini-3-flash", Some(false), Some(false)),
        ]
    };

    #[test]
    fn mework_knows_the_vendors_endpoints_and_leaves_the_rest_to_the_user() {
        for (family, base_url, model, tool, system) in APPEND_FIXTURE {
            assert_eq!(known(*family, base_url, model), *tool, "tool: {family:?} {base_url} {model}");
            assert_eq!(
                crate::system_append::known(*family, base_url, model),
                *system,
                "system: {family:?} {base_url} {model}"
            );
        }
    }

    #[test]
    fn the_declared_capability_decides_where_the_protocol_can_append() {
        let provider = |family, base_url: &str| ApiProvider {
            id: "p".into(),
            name: "P".into(),
            enabled: true,
            family,
            base_url: base_url.into(),
            family_settings: Default::default(),
            endpoint_base_urls: Default::default(),
            notes: String::new(),
            models: Vec::new(),
            active_model_id: None,
        };
        let model = |id: &str, declared: bool| ModelProfile {
            id: id.into(),
            name: String::new(),
            group: String::new(),
            context_window: None,
            max_output_tokens: None,
            capabilities: if declared {
                [ModelCapability::ToolAppend].into()
            } else {
                Default::default()
            },
            reasoning_content: Default::default(),
            prompt_cache: true,
            cache_ttl_minutes: None,
        };
        // A relay appends exactly when the model says it does, and so does
        // Anthropic's own endpoint: the declaration is the model's, whoever
        // filled it in.
        let relay = provider(ProviderFamily::Anthropic, "https://relay.example.com/v1");
        assert!(appends_tools(&relay, &model("claude-opus-5-5", true)));
        assert!(!appends_tools(&relay, &model("claude-opus-5-5", false)));
        let anthropic = provider(ProviderFamily::Anthropic, "https://api.anthropic.com/v1");
        assert!(!appends_tools(&anthropic, &model("claude-opus-5-5", false)));
        // A protocol without the interface has nothing to append through.
        let chat = provider(ProviderFamily::OpenaiCompatible, "https://relay.example.com/v1");
        assert!(!appends_tools(&chat, &model("deepseek-v4", true)));
    }

    /// Every family Mework could ever answer "yes" for reads the capability,
    /// and every family it always answers "no" for leaves it idle.
    #[test]
    fn mework_declares_the_capability_only_where_it_takes_effect() {
        for (family, base_url, model, tool, _) in APPEND_FIXTURE {
            if *tool == Some(true) || tool.is_none() {
                assert!(family.tool_append_takes_effect(), "{family:?} {base_url} {model}");
            }
        }
        for (family, base_url, model, _, system) in APPEND_FIXTURE {
            if *system == Some(true) || system.is_none() {
                assert!(family.system_append_takes_effect(), "{family:?} {base_url} {model}");
            }
        }
    }

    #[test]
    fn nothing_is_new_without_a_previous_request() {
        assert!(newly_offered(&names(&["read", "write"]), None, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_tool_is_new_once_and_in_offered_order() {
        let previous = BTreeSet::from(["read".to_owned()]);
        let offered = names(&["read", "handoff", "create_handoff_note"]);
        assert_eq!(
            newly_offered(&offered, Some(&previous), &HashSet::new()),
            names(&["handoff", "create_handoff_note"])
        );
        // Already appended: its marker hands it over; no second one.
        let appended = HashSet::from(["handoff".to_owned()]);
        assert_eq!(
            newly_offered(&offered, Some(&previous), &appended),
            names(&["create_handoff_note"])
        );
    }

    #[test]
    fn a_withdrawn_tool_is_not_new() {
        let previous = BTreeSet::from(["read".to_owned(), "plan".to_owned()]);
        assert!(newly_offered(&names(&["read"]), Some(&previous), &HashSet::new()).is_empty());
    }

    #[test]
    fn the_marker_is_local_and_projects_as_an_empty_system_message() {
        let marker = marker("ctx_tools-added_1".into(), names(&["handoff"]), "t".into());
        assert!(matches!(marker, ContextItem::System { local_only: true, .. }));
        assert_eq!(added_tools(&marker), names(&["handoff"]).as_slice());
        assert_eq!(
            marker_message(&names(&["handoff"])),
            json!({
                "role": "system",
                "content": "",
                "providerOptions": { "mework": { "toolAddition": ["handoff"] } },
            })
        );
    }

    #[test]
    fn the_registry_keeps_the_last_set_per_conversation() {
        let registry = OfferedToolRegistry::default();
        assert!(registry.last("c").is_none());
        registry.record("c", BTreeSet::from(["read".to_owned()]));
        registry.record("c", BTreeSet::from(["write".to_owned()]));
        assert_eq!(registry.last("c"), Some(BTreeSet::from(["write".to_owned()])));
    }
}
