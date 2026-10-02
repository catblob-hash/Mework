//! Where something the host adds to a conversation partway through goes.
//!
//! What decides is first what the addition *is*, then what the model and its
//! endpoint can take:
//!
//! - **An instruction** — plan mode switched on or off, the request to hand
//!   off past the auto-compact threshold, the notes a continuation was
//!   handed — is operator-level text. It goes in as an appended system prompt
//!   (`system_append.rs`) where the model and endpoint take a system message
//!   mid-conversation, and in `box` where they do not.
//!
//! Whether a model and endpoint take either kind of append is an attribute of
//! the model: Mework answers it where it knows — the vendor's own endpoint, a
//! protocol with no such interface — and the user answers it everywhere else,
//! a relay above all (`tool_append::known`, `system_append::known`).
//! - **A notice** — a background result, a hook's context, a skill added, a
//!   file that changed — is neutral, and only ever goes in `box`
//!   (`api::deliver_host_notices`). It is never raised to a system message: a
//!   system message outranks the user, and a neutral notice given that rank
//!   reads to the model as an order.
//! - **A tool** joins through the protocol's tool-append interface
//!   (`tool_append.rs`). A model without one keeps the tool surface of its
//!   first request, so the features that add tools mid-conversation are off
//!   there.
//!
//! So an appended system prompt may fall back to `box`, and `box` never falls
//! back to a system message. `box` itself is a tool: every run declares it
//! from its first request (`agents::apply_task_runtime_tools`), so it is there
//! whatever the model can append. A transcript whose previous request did not
//! offer it could only be handed it by a model that appends tools; failing
//! both, an instruction is left with the last resort it always had — the
//! sidecar lifts an appended system prompt into the system prompt.

use std::collections::BTreeSet;

use crate::model::{ProviderFamily, RunModelRequest};

/// Whether `base_url` is the vendor's own endpoint for `family`: Anthropic's
/// API for Anthropic Messages, OpenAI's or Azure's for the OpenAI protocols.
/// An empty address is the provider's default, which is the vendor's own.
///
/// Only there does Mework know what the endpoint takes
/// (`tool_append::known`, `system_append::known`). A relay may speak a
/// protocol without passing its newer shapes on, so what one takes is the
/// user's to say, on the model's own attributes. Mirrored by TS
/// `atVendorEndpoint` in `src/lib/modelCapabilities.ts`.
pub(crate) fn at_vendor_endpoint(family: ProviderFamily, base_url: &str) -> bool {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return true;
    }
    let Some(host) = url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    match family {
        ProviderFamily::Anthropic => host == "api.anthropic.com",
        ProviderFamily::OpenaiResponses
        | ProviderFamily::OpenaiCodex
        | ProviderFamily::Azure
        | ProviderFamily::OpenaiChat => {
            host == "api.openai.com"
                || host == "chatgpt.com"
                || [".openai.azure.com", ".cognitiveservices.azure.com", ".services.ai.azure.com"]
                    .iter()
                    .any(|suffix| host.ends_with(suffix))
        }
        ProviderFamily::ClaudeAgent
        | ProviderFamily::Google
        | ProviderFamily::Xai
        | ProviderFamily::Bedrock
        | ProviderFamily::Vertex
        | ProviderFamily::OpenaiCompatible => false,
    }
}

/// How an instruction added mid-conversation reaches the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstructionCarrier {
    /// A system message at its point (`system_append::card`).
    SystemAppend,
    /// A host notice in `box`.
    Box,
    /// Neither is open: an appended system prompt the sidecar lifts into the
    /// system prompt.
    SystemPrompt,
}

/// The carrier for an instruction this request adds. `previously_offered` is
/// the tool set the transcript's previous request offered (`None` before its
/// first, or after a restart, when this request's tool list is declared whole).
pub(crate) fn instruction_carrier(
    request: &RunModelRequest,
    previously_offered: Option<&BTreeSet<String>>,
) -> InstructionCarrier {
    if crate::system_append::appends_system(&request.provider, &request.model) {
        InstructionCarrier::SystemAppend
    } else if box_reachable(request, previously_offered) {
        InstructionCarrier::Box
    } else {
        InstructionCarrier::SystemPrompt
    }
}

/// Whether a `box` delivery reaches the model without touching the head of the
/// prompt: this request offers `box`, and either the previous one did too or
/// the model takes a tool mid-conversation.
fn box_reachable(request: &RunModelRequest, previously_offered: Option<&BTreeSet<String>>) -> bool {
    let offered = crate::aisdk::tools::enabled_tools(request)
        .iter()
        .any(|tool| tool.name == crate::api::BOX_TOOL);
    offered
        && previously_offered.is_none_or(|previous| {
            previous.contains(crate::api::BOX_TOOL)
                || crate::tool_append::appends_tools(&request.provider, &request.model)
        })
}

/// Model families that read their tool definitions ahead of the system prompt,
/// so its end is the last thing before the messages. From the providers' own
/// documentation and the models' published chat templates (2026-10):
/// Anthropic caches tools, then system, then messages — also on Bedrock and
/// through the Claude Code CLI; OpenAI's hosted models put tools ahead of the
/// developer prompt; Kimi K2 and GLM-4.5 on declare their tools in a turn of
/// their own ahead of the system turn.
const TOOLS_FIRST: &[&str] = &["claude", "gpt", "chatgpt", "o1", "o3", "o4", "codex", "kimi", "moonshot", "glm"];

/// Whether `model_id` names a model that reads its tools before its system
/// prompt ([`TOOLS_FIRST`]). DeepSeek, Qwen and gpt-oss (whose harmony format
/// renders instructions ahead of tools) put the system prompt first; Gemini,
/// Grok and every model not listed say nothing, and count as not — text that
/// differs from one conversation to the next then goes after the tools, never
/// ahead of them.
pub(crate) fn tools_precede_system(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    let segments = id
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.windows(2).any(|pair| pair == ["gpt", "oss"]) {
        return false;
    }
    segments
        .iter()
        .any(|segment| TOOLS_FIRST.iter().any(|family| segment.starts_with(family)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_family_decides_whether_tools_come_first() {
        for model in [
            "claude-opus-5-5",
            "us.anthropic.claude-sonnet-5-5-v1:0",
            "gpt-6.1-sol",
            "gpt-5.5",
            "o3-mini",
            "chatgpt-4o-latest",
            "codex-mini-latest",
            "kimi-k2-0905-preview",
            "moonshot-v1-128k",
            "glm-4.6",
        ] {
            assert!(tools_precede_system(model), "{model}");
        }
        for model in [
            "deepseek-chat",
            "deepseek-v4",
            "qwen3-coder-plus",
            "qwq-32b",
            "openai/gpt-oss-120b",
            "gemini-3-pro",
            "grok-5",
            "model-test",
            "",
        ] {
            assert!(!tools_precede_system(model), "{model}");
        }
    }
}
