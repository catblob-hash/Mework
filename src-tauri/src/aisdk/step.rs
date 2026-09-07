//! Request construction for one model step: `RunModelRequest` → [`StepRequest`].
//!
//! This module creates a provider-neutral step description for the sidecar's AI
//! SDK translation. Host policy remains here: normalized addresses and stored
//! credentials, image admission checks, and the single native-search gate.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::api::Exchange;
use crate::image_attachments::{
    hydrate_ai_sdk_images, ImageAttachmentStore, MAX_REQUEST_IMAGES, MAX_REQUEST_IMAGE_BYTES,
    MAX_REQUEST_IMAGE_PIXELS,
};
use crate::model::{ContextItem, ReasoningContent, RunModelRequest};

use super::project::project_messages;
use super::protocol::{Family, NativeSearch, StepRequest, ToolSpec};
use super::tools::{enabled_tools, tool_schema};

/// Build the system prompt using host-owned prompt composition rules.
pub(crate) fn combined_system_prompt(request: &RunModelRequest) -> String {
    let mut prompts = Vec::new();
    if !request.system_prompt.trim().is_empty() {
        prompts.push(request.system_prompt.trim().to_owned());
    }
    for context in &request.contexts {
        if let ContextItem::System {
            content,
            local_only: false,
            ..
        } = context
        {
            let content = content.trim();
            if !content.is_empty() && !prompts.iter().any(|prompt| prompt == content) {
                prompts.push(content.to_owned());
            }
        }
    }
    // Wire layer, not `assemble_system_prompt`: the section has to disappear on
    // the very next step after the user approves the plan, must never enter a
    // fork snapshot, and a child agent needs its own shorter variant.
    if request.effective_security_level() == crate::model::SecurityLevel::Plan {
        let key = if request.subagent_depth == 0 {
            crate::prompt_profile::PromptKey::SystemPlanMode
        } else {
            crate::prompt_profile::PromptKey::SystemPlanModeSubagent
        };
        let section = request.prompt_profile.text(key).trim();
        if !section.is_empty() {
            prompts.push(section.to_owned());
        }
    }
    if enabled_tools(request)
        .iter()
        .any(|tool| tool.name == crate::api::WEB_SEARCH_TOOL)
    {
        let boundary = request
            .prompt_profile
            .text(crate::prompt_profile::PromptKey::SystemWebSafety)
            .trim();
        if !boundary.is_empty() {
            prompts.push(boundary.to_owned());
        }
    }
    prompts.join("\n\n")
}

/// Project tool exchanges that have not yet entered `contexts` for this round.
///
/// Each [`Exchange`] produces continuation messages, a matching `tool` result
/// message, then separate `user` messages for image bridges and host notices.
/// Continuations are opaque AI SDK `response.messages`; changing them can make
/// upstream providers reject the request. All provider families use the image
/// bridge: `openai-compatible` serializes file output as costly, unusable base64.
fn exchange_messages(exchanges: &[Exchange], out: &mut Vec<Value>) {
    for exchange in exchanges {
        // A non-array continuation comes from an unmigrated writer; preserving
        // it exposes the error more reliably than silently discarding it.
        match &exchange.continuation {
            Value::Array(messages) => out.extend(messages.iter().cloned()),
            Value::Null => {}
            other => out.push(other.clone()),
        }

        if !exchange.executions.is_empty() {
            let results = exchange
                .executions
                .iter()
                .map(|execution| {
                    json!({
                        "type": "tool-result",
                        "toolCallId": execution.call.id,
                        "toolName": execution.call.name,
                        "output": super::project::tool_output(
                            execution.result.success,
                            &execution.result.output,
                        ),
                    })
                })
                .collect::<Vec<_>>();
            out.push(json!({ "role": "tool", "content": results }));
        }

        let bridge = live_tool_image_bridge(&exchange.executions);
        if !bridge.is_empty() {
            out.push(json!({ "role": "user", "content": bridge }));
        }
        // Deliver background-task notices after their pending tool results, each
        // as a separate user message.
        for notice in &exchange.host_notices {
            let notice = notice.trim();
            if !notice.is_empty() {
                out.push(json!({ "role": "user", "content": notice }));
            }
        }
    }
}

/// Build image bridges for this round's tool results. Historical messages share
/// the same bridge so the model receives an identical representation.
fn live_tool_image_bridge(executions: &[crate::api::ToolExecution]) -> Vec<Value> {
    let mut content = Vec::new();
    for execution in executions {
        super::project::push_tool_image_bridge(
            &mut content,
            &execution.call.name,
            &execution.call.id,
            &execution.result.images,
        );
    }
    content
}

fn tool_specs(request: &RunModelRequest) -> Vec<ToolSpec> {
    let supports_vision = request.model.supports_vision();
    enabled_tools(request)
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            // Empty recommendation-layer descriptions are passed through; each
            // provider decides whether to omit them. What a tool *is* rides on
            // the schema's root description, which the profile also owns.
            description: tool.description.clone(),
            input_schema: tool_schema(tool, supports_vision, &request.prompt_profile),
        })
        .collect()
}

/// Apply image admission checks and hydrate placeholders.
///
/// Validate after projection but before sending, when the actual image count,
/// bytes, and pixels are known. Check `supports_vision` first because it is a
/// user-correctable configuration error; the remaining limits are quotas.
#[cfg(test)]
pub(crate) fn hydrate_images_for_test(
    request: &RunModelRequest,
    messages: Vec<Value>,
) -> Result<Vec<Value>, String> {
    hydrate_images(request, messages)
}

fn hydrate_images(request: &RunModelRequest, messages: Vec<Value>) -> Result<Vec<Value>, String> {
    let stats = crate::image_attachments::ai_sdk_placeholder_stats(&messages)?;
    if stats.count == 0 {
        return Ok(messages);
    }
    if !request.model.supports_vision() {
        return Err(format!("模型 {} 未启用图片输入能力", request.model.id));
    }
    if stats.count > MAX_REQUEST_IMAGES {
        return Err(format!(
            "单次模型请求最多包含 {MAX_REQUEST_IMAGES} 张图片，当前为 {} 张",
            stats.count
        ));
    }
    if stats.bytes > MAX_REQUEST_IMAGE_BYTES {
        return Err(format!(
            "单次模型请求图片总量超过 {} MiB 限制",
            MAX_REQUEST_IMAGE_BYTES / 1024 / 1024
        ));
    }
    if stats.pixels > MAX_REQUEST_IMAGE_PIXELS {
        return Err(format!(
            "单次模型请求图片总像素超过 {} MP 限制",
            MAX_REQUEST_IMAGE_PIXELS / 1024 / 1024
        ));
    }
    let store = ImageAttachmentStore::new(Path::new(&request.app_data_path));
    hydrate_ai_sdk_images(&messages, &store)
}

/// Map reasoning effort to the AI SDK 7 vocabulary. Provider-specific mappings
/// and unsupported-level fallback belong to the provider implementation.
fn reasoning_level(effort: crate::model::ReasoningEffort) -> Option<&'static str> {
    use crate::model::ReasoningEffort;
    Some(match effort {
        // Send `none`, rather than omitting the field, because omission selects
        // the provider default and can re-enable explicitly disabled reasoning.
        ReasoningEffort::Disabled => "none",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    })
}

/// Return the `providerOptions` key consumed by a Responses-family provider.
///
/// This is the sole predicate for reasoning-content consumers. Keep the Azure
/// and OpenAI keys explicit rather than relying on Azure's fallback behavior.
/// The timeline uses a separate enum for presentation; its payload-level test
/// ensures that enum remains consistent with this mapping.
fn responses_options_key(family: Family) -> Option<&'static str> {
    match family {
        // Codex is served by `createOpenAI`, so it reads `providerOptions.openai` too.
        Family::OpenaiResponses | Family::OpenaiCodex => Some("openai"),
        Family::Azure => Some("azure"),
        _ => None,
    }
}

/// Build provider-specific options for Responses-family providers.
///
/// Always set `store: false`: Mework owns context, and `store: true` rewrites
/// replayed items as upstream references. Always request encrypted replay data,
/// independently of the model's visible reasoning representation. Explicit include
/// also covers proxy model names not recognized by the SDK.
/// History replays stored reasoning items through the cards' own signed parts.
///
/// `prompt_cache_key` is the conversation id, as Codex keys its prompt cache
/// by thread: every turn of one conversation shares a prefix, and the key lets
/// the upstream route them to the same cache.
fn provider_options(
    family: Family,
    _reasoning_content: ReasoningContent,
    reasoning_effort: crate::model::ReasoningEffort,
    conversation_id: &str,
) -> Option<Value> {
    let key = responses_options_key(family)?;
    let mut options = json!({ "store": false });
    // The SDK defaults `reasoning.summary` to `detailed` whenever an effort other
    // than `none` is sent; Codex omits the field, so send an explicit null to
    // suppress it. With reasoning disabled the SDK never emits a summary and the
    // request must stay free of every reasoning dial.
    if !matches!(reasoning_effort, crate::model::ReasoningEffort::Disabled) {
        options["reasoningSummary"] = Value::Null;
    }
    options["include"] = json!(["reasoning.encrypted_content"]);
    let cache_key = conversation_id.trim();
    if !cache_key.is_empty() {
        options["promptCacheKey"] = json!(cache_key);
    }
    Some(json!({ key: options }))
}

#[cfg(test)]
mod responses_defaults_tests {
    use super::*;

    #[test]
    fn responses_all_modes_request_encrypted_replay() {
        for family in [Family::OpenaiResponses, Family::OpenaiCodex, Family::Azure] {
            for mode in [ReasoningContent::Plaintext, ReasoningContent::Encrypted] {
                let key = responses_options_key(family).unwrap();
                let options = provider_options(family, mode, crate::model::ReasoningEffort::High, "conversation").unwrap();
                assert_eq!(options[key]["include"], json!(["reasoning.encrypted_content"]));
            }
        }
    }

    #[test]
    fn disabled_reasoning_sends_no_summary_dial() {
        for family in [Family::OpenaiResponses, Family::OpenaiCodex, Family::Azure] {
            let key = responses_options_key(family).unwrap();
            let options = provider_options(
                family,
                ReasoningContent::Plaintext,
                crate::model::ReasoningEffort::Disabled,
                "conversation",
            )
            .unwrap();
            assert!(!options[key].as_object().unwrap().contains_key("reasoningSummary"));
            assert_eq!(options[key]["include"], json!(["reasoning.encrypted_content"]));
        }
    }

    #[test]
    fn responses_summary_default_is_explicit_null() {
        for family in [Family::OpenaiResponses, Family::OpenaiCodex, Family::Azure] {
            let key = responses_options_key(family).unwrap();
            for mode in [ReasoningContent::Plaintext, ReasoningContent::Encrypted] {
                let options = provider_options(family, mode, crate::model::ReasoningEffort::High, "conversation").unwrap();
                assert!(options[key].as_object().unwrap().contains_key("reasoningSummary"));
                assert_eq!(options[key]["reasoningSummary"], Value::Null);
                assert_eq!(options[key]["store"], false);
                assert_eq!(options[key]["promptCacheKey"], "conversation");
            }
        }
        assert!(provider_options(Family::Anthropic, ReasoningContent::Plaintext, crate::model::ReasoningEffort::High, "conversation").is_none());
        assert!(provider_options(Family::OpenaiChat, ReasoningContent::Plaintext, crate::model::ReasoningEffort::High, "conversation").is_none());
    }
}

/// Select the reasoning representation sent to the sidecar.
///
/// Only families with a consumer receive this field. The model attribute is
/// always determinate on disk, so there is no default policy to duplicate here.
fn wire_reasoning_content(family: Family, reasoning_content: ReasoningContent) -> Option<&'static str> {
    responses_options_key(family)?;
    Some(match reasoning_content {
        ReasoningContent::Plaintext => "plaintext",
        ReasoningContent::Encrypted => "encrypted",
    })
}

/// Clamp the provider's minimum output budget.
///
/// OpenAI Responses rejects `max_output_tokens` below 16. Clamp by family so
/// valid `max_tokens: 1` Chat Completions requests retain their user-selected
/// budget. The ChatGPT Codex backend rejects the parameter outright
/// (`Unsupported parameter: max_output_tokens`), so that family sends none; the
/// sidecar dialect strips it again as the last line of defense.
fn clamp_output_budget(family: Family, budget: Option<u64>) -> Option<u64> {
    const RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;
    let budget = budget?;
    Some(match family {
        Family::OpenaiCodex => return None,
        Family::OpenaiResponses | Family::Azure => budget.max(RESPONSES_MIN_OUTPUT_TOKENS),
        _ => budget.max(1),
    })
}

/// Collect required provider-family settings and return a repairable error for
/// each missing setting before the request reaches an ambiguous upstream 404.
pub(crate) fn family_settings(provider: &crate::model::ApiProvider) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for setting in provider.family.known_settings() {
        if let Some(value) = provider.family_settings.get(setting) {
            let value = value.trim();
            if !value.is_empty() {
                out.insert(setting.wire_name().to_owned(), value.to_owned());
            }
        }
    }
    for setting in provider.family.required_settings() {
        if !out.contains_key(setting.wire_name()) {
            return Err(format!(
                "提供商 {} 还缺一项 {} 才能发出请求；请在提供商设置里补上",
                provider.name,
                setting.slug()
            ));
        }
    }
    Ok(out)
}

/// Convert a host-validated address for the sidecar.
///
/// Vertex and Bedrock may omit it because their SDK endpoints derive from
/// `project`/`location`/`region`; other families validate an empty address earlier.
pub(crate) fn sidecar_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub(crate) fn build_step_request(
    request: &RunModelRequest,
    exchanges: &[Exchange],
    history: Vec<Value>,
    base_url: &str,
    api_key: Option<String>,
    max_steps: u32,
) -> Result<StepRequest, String> {
    let mut messages = Vec::new();
    let family = Family::for_format(request.provider.family);
    // Ephemeral contexts precede history because they apply only to this request
    // and must not become conversation history.
    if !request.ephemeral_contexts.is_empty() {
        messages.extend(project_messages(family, &request.ephemeral_contexts));
    }
    messages.extend(history);
    exchange_messages(exchanges, &mut messages);

    let messages = hydrate_images(request, messages)?;
    super::project::enforce_frame_budget(&messages)?;

    let system = combined_system_prompt(request);
    Ok(StepRequest {
        family,
        base_url: sidecar_base_url(base_url),
        api_key,
        headers: BTreeMap::new(),
        settings: family_settings(&request.provider)?,
        model_id: request.model.id.clone(),
        system: (!system.trim().is_empty()).then_some(system),
        messages,
        tools: tool_specs(request),
        tool_choice: None,
        max_steps,
        max_output_tokens: clamp_output_budget(family, request.model.max_output_tokens),
        temperature: None,
        reasoning: reasoning_level(request.reasoning_effort),
        reasoning_content: wire_reasoning_content(family, request.model.reasoning_content),
        provider_options: provider_options(
            family,
            request.model.reasoning_content,
            request.reasoning_effort,
            &request.conversation_id,
        ),
        native_search: native_search(request),
        // Minted per run by the caller (`SessionLease`), never per step: the
        // sidecar keys its parked CLI session by it.
        agent: None,
    })
}

/// Gate the native `web_search` server tool to the one-shot request constructed
/// by `run_web_search`; ordinary conversations never receive it.
fn native_search(request: &RunModelRequest) -> Option<NativeSearch> {
    request.native_search_call.then(|| NativeSearch {
        max_uses: request.web_search.max_searches_per_call,
        previous_call_ids: Vec::new(),
    })
}
