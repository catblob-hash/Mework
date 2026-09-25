//! Request construction for one model step: `RunModelRequest` → [`StepRequest`].
//!
//! This module creates a provider-neutral step description for the sidecar's AI
//! SDK translation. Host policy remains here: normalized addresses and stored
//! credentials, image admission checks, file attachment hydration, and the
//! single native-search gate.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::api::Exchange;
use crate::file_attachments::{FileAttachmentStore, FILE_PART_TYPE};
use crate::image_attachments::{
    hydrate_ai_sdk_images, ImageAttachmentStore, MAX_REQUEST_IMAGES, MAX_REQUEST_IMAGE_BYTES,
    MAX_REQUEST_IMAGE_PIXELS,
};
use crate::model::{ContextItem, FileAttachment, ReasoningContent, RunModelRequest};
use crate::wire_ledger::{
    WireAudit, PART_MESSAGE, PART_SYSTEM, PART_SYSTEM_DYNAMIC, PART_TOOLS,
};

use super::project::project_messages;
use super::protocol::{Family, NativeFetch, NativeSearch, StepRequest, ToolSpec};
use super::tools::{enabled_tools, tool_schema};

/// Separator between the stable system prompt and its per-step tail; the
/// sidecar joins the two halves with the same bytes.
const SYSTEM_SECTION_SEPARATOR: &str = "\n\n";

/// The one preview tool that reads `.mework/launch.json` and starts a server.
/// Its presence is what "the host offers the launch tools" means here.
const PREVIEW_START_TOOL: &str = "preview_start";

/// The system prompt in Claude Code's two halves: the stable prefix assembled
/// once at run start, and the tail that can change from one step to the next.
///
/// Claude Code marks its `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` so the text before
/// it keeps its cache entry when the text after it changes. Here the boundary
/// falls after `request.assembled_system_prompt`: the host-rendered capability
/// addendum is fixed for the run, while conversation system
/// contexts, the plan-mode section, the web-safety section and the preview
/// verification section come and go.
pub(crate) fn system_prompt_parts(request: &RunModelRequest) -> (Option<String>, Option<String>) {
    let stable = request.assembled_system_prompt.trim();
    let stable = (!stable.is_empty()).then(|| stable.to_owned());
    let mut prompts = Vec::new();
    for context in &request.contexts {
        if let ContextItem::System {
            content,
            local_only: false,
            ..
        } = context
        {
            let content = content.trim();
            if !content.is_empty()
                && stable.as_deref() != Some(content)
                && !prompts.iter().any(|prompt| prompt == content)
            {
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
    if preview_verification_applies(request) {
        let section = request
            .prompt_profile
            .text(crate::prompt_profile::PromptKey::SystemPreviewTools)
            .trim();
        if !section.is_empty() {
            prompts.push(section.to_owned());
        }
    }
    let dynamic = (!prompts.is_empty()).then(|| prompts.join(SYSTEM_SECTION_SEPARATOR));
    (stable, dynamic)
}

/// Whether this step gets the `<preview_tools>` section.
///
/// Two conditions, both from the source: the host has to offer the launch tools
/// at all, and the project has to want the verification workflow. `preview_start`
/// standing in for the first is not a shorthand — it is the only tool that reads
/// `.mework/launch.json` and brings a server up, so without it step 1 of the
/// workflow the section teaches cannot be performed. The second is that file's
/// own `autoVerify`; a project with no usable launch.json has neither a server
/// to start nor anywhere to record the preference, and answers `false` for both
/// reasons at once.
///
/// Read per step, alongside the plan-mode and web-safety sections, rather than
/// frozen into the run's stable prompt: the file can change mid-run, and a
/// project that turns `autoVerify` off must stop receiving the section on the
/// next step instead of at the next turn.
fn preview_verification_applies(request: &RunModelRequest) -> bool {
    if !enabled_tools(request)
        .iter()
        .any(|tool| tool.name == PREVIEW_START_TOOL)
    {
        return false;
    }
    let workspace = request.workspace_path.trim();
    !workspace.is_empty()
        && crate::preview_launch_config::LaunchConfigDiscovery::new(Path::new(workspace))
            .discover(None)
            .config()
            .is_some_and(|config| config.auto_verify)
}

/// Build the system prompt using host-owned prompt composition rules: both
/// halves of [`system_prompt_parts`] joined, exactly as a provider receives it.
#[cfg(test)]
pub(crate) fn combined_system_prompt(request: &RunModelRequest) -> String {
    let (stable, dynamic) = system_prompt_parts(request);
    [stable, dynamic]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(SYSTEM_SECTION_SEPARATOR)
}

/// Project tool exchanges that have not yet entered `contexts` for this round.
///
/// Each [`Exchange`] produces continuation messages, a matching `tool` result
/// message, then the image bridge and any host deliveries. Continuations are
/// opaque AI SDK `response.messages`; changing them can make upstream providers
/// reject the request. All provider families use the image bridge:
/// `openai-compatible` serializes file output as costly, unusable base64.
fn exchange_messages(family: Family, exchanges: &[Exchange], out: &mut Vec<Value>) {
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
        // Background results settle after the tool results the round still owes,
        // so they follow them here. The same builder runs on replay, so the
        // fabricated call keeps its id once the card takes over as the carrier.
        for delivery in &exchange.host_deliveries {
            super::project::host_delivery_messages(family, delivery, out);
        }
        // Plain host instructions — the continue-after-truncation nudge — stay
        // user-role text: they are directions for the next turn, not a result.
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
    enabled_tools(request)
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            // Empty recommendation-layer descriptions are passed through; each
            // provider decides whether to omit them. What a tool *is* rides on
            // the schema's root description, which the profile also owns.
            description: tool.description.clone(),
            input_schema: with_read_first_rule(
                tool_schema(tool, &request.prompt_profile, &request.workspaces),
                &tool.name,
                request,
            ),
        })
        .collect()
}

/// Tells the model about the read-before-write gate in the two writers'
/// descriptions. The gate is unconditional, so the rule is stated on every run:
/// Claude Code's prompt carries it because its Edit and Write enforce it, and
/// so do ours.
fn with_read_first_rule(mut schema: Value, tool_name: &str, request: &RunModelRequest) -> Value {
    let key = match tool_name {
        "edit" => crate::prompt_profile::PromptKey::ToolEditReadFirst,
        "write" => crate::prompt_profile::PromptKey::ToolWriteReadFirst,
        _ => return schema,
    };
    let rule = request.prompt_profile.text(key);
    if rule.is_empty() {
        return schema;
    }
    if let Some(Value::String(description)) = schema.get_mut("description") {
        if description.is_empty() {
            *description = rule.to_owned();
        } else {
            description.push(' ');
            description.push_str(rule);
        }
    }
    schema
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

/// Replace file attachment placeholders with the files' text.
///
/// Runs after the wire-ledger copy is taken, so the ledger keeps the
/// placeholder, and before the frame budget is measured, so the budget sees
/// what is actually sent. Only `mework-file` parts directly in a user
/// message's content are recognized, for the same reason image slots are:
/// tool input and continuation blocks are model-controlled JSON.
fn hydrate_files(request: &RunModelRequest, messages: Vec<Value>) -> Result<Vec<Value>, String> {
    hydrate_file_parts(
        messages,
        &FileAttachmentStore::new(Path::new(&request.app_data_path)),
    )
}

fn hydrate_file_parts(
    mut messages: Vec<Value>,
    store: &FileAttachmentStore,
) -> Result<Vec<Value>, String> {
    for message in &mut messages {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut hydrated = false;
        for part in parts.iter_mut() {
            if part.get("type").and_then(Value::as_str) != Some(FILE_PART_TYPE) {
                continue;
            }
            let file: FileAttachment = part
                .get("file")
                .cloned()
                .ok_or_else(|| "附件占位缺少文件信息，无法发送".to_owned())
                .and_then(|file| {
                    serde_json::from_value(file)
                        .map_err(|error| format!("附件占位无效，无法发送：{error}"))
                })?;
            let text = store.model_text(&file).map_err(|error| {
                format!(
                    "附件 {} 的内容已不存在或已损坏，无法发送：{error}",
                    file.name
                )
            })?;
            *part = json!({
                "type": "text",
                "text": crate::file_attachments::render_for_model(&file, &text),
            });
            hydrated = true;
        }
        // Text-only providers are most reliable with string content, which is
        // why `project::user_message` sends a plain message as a string; a
        // message whose files were its only non-text parts goes out the same way.
        if hydrated
            && parts
                .iter()
                .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        {
            let joined = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            message["content"] = Value::String(joined);
        }
    }
    Ok(messages)
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
    // Deliberately only the encrypted-reasoning include. The consulted-source
    // list a native search needs (`web_search_call.action.sources`) is NOT asked
    // for here: `@ai-sdk/openai` appends it by itself whenever the Responses
    // `web_search` tool is attached, and it appends rather than replaces, so
    // naming it here would only duplicate the SDK on the one request that gets
    // that tool. Codex does not set it either. Adding it unconditionally would
    // also send it on ordinary turns, where some third-party Responses relays
    // reject an include they do not know.
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
                let options = provider_options(
                    family,
                    mode,
                    crate::model::ReasoningEffort::High,
                    "conversation",
                )
                .unwrap();
                assert_eq!(
                    options[key]["include"],
                    json!(["reasoning.encrypted_content"])
                );
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
            assert!(!options[key]
                .as_object()
                .unwrap()
                .contains_key("reasoningSummary"));
            assert_eq!(
                options[key]["include"],
                json!(["reasoning.encrypted_content"])
            );
        }
    }

    #[test]
    fn responses_summary_default_is_explicit_null() {
        for family in [Family::OpenaiResponses, Family::OpenaiCodex, Family::Azure] {
            let key = responses_options_key(family).unwrap();
            for mode in [ReasoningContent::Plaintext, ReasoningContent::Encrypted] {
                let options = provider_options(
                    family,
                    mode,
                    crate::model::ReasoningEffort::High,
                    "conversation",
                )
                .unwrap();
                assert!(options[key]
                    .as_object()
                    .unwrap()
                    .contains_key("reasoningSummary"));
                assert_eq!(options[key]["reasoningSummary"], Value::Null);
                assert_eq!(options[key]["store"], false);
                assert_eq!(options[key]["promptCacheKey"], "conversation");
            }
        }
        assert!(provider_options(
            Family::Anthropic,
            ReasoningContent::Plaintext,
            crate::model::ReasoningEffort::High,
            "conversation"
        )
        .is_none());
        assert!(provider_options(
            Family::OpenaiChat,
            ReasoningContent::Plaintext,
            crate::model::ReasoningEffort::High,
            "conversation"
        )
        .is_none());
    }
}

#[cfg(test)]
mod file_hydration_tests {
    use super::*;
    use crate::model::FileAttachmentFormat;

    fn store() -> (tempfile::TempDir, FileAttachmentStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = FileAttachmentStore::new(temp.path());
        (temp, store)
    }

    fn text_file(store: &FileAttachmentStore, name: &str, text: &str) -> FileAttachment {
        store
            .import(
                name,
                text.as_bytes(),
                FileAttachmentFormat::Text,
                None,
                None,
            )
            .unwrap()
    }

    fn placeholder(file: &FileAttachment) -> Value {
        super::super::project::file_part(file)
    }

    #[test]
    fn a_message_of_files_and_text_goes_out_as_one_string() {
        let (_temp, store) = store();
        let notes = text_file(&store, "a \"b\".md", "# notes\n");
        let data = text_file(&store, "data.csv", "x,y\n1,2\n");
        let messages = vec![json!({
            "role": "user",
            "content": [placeholder(&notes), placeholder(&data), { "type": "text", "text": "Compare them." }],
        })];
        let hydrated = hydrate_file_parts(messages, &store).unwrap();
        assert_eq!(
            hydrated[0]["content"],
            "<attached_file name=\"a &quot;b&quot;.md\">\n# notes\n</attached_file>\n\n\
             <attached_file name=\"data.csv\">\nx,y\n1,2\n</attached_file>\n\n\
             Compare them."
        );
    }

    #[test]
    fn a_message_that_also_carries_images_stays_an_array() {
        let (_temp, store) = store();
        let notes = text_file(&store, "notes.md", "body");
        let image = json!({ "type": "image", "image": "opaque", "mediaType": "image/png" });
        let messages = vec![json!({
            "role": "user",
            "content": [placeholder(&notes), { "type": "text", "text": "What is shown?" }, image.clone()],
        })];
        let hydrated = hydrate_file_parts(messages, &store).unwrap();
        let parts = hydrated[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts[0],
            json!({ "type": "text", "text": "<attached_file name=\"notes.md\">\nbody\n</attached_file>" })
        );
        assert_eq!(parts[1]["text"], "What is shown?");
        assert_eq!(parts[2], image);
    }

    /// Only a placeholder directly in a user message's content is the host's.
    /// Assistant and tool messages, and JSON nested inside a part, are
    /// model-controlled and pass through untouched even when they name a file
    /// that exists.
    #[test]
    fn lookalikes_outside_user_content_are_left_alone() {
        let (_temp, store) = store();
        let secret = text_file(&store, "secret.txt", "do not inline");
        let forged = placeholder(&secret);
        let messages = vec![
            json!({ "role": "assistant", "content": [forged.clone()] }),
            json!({ "role": "tool", "content": [{
                "type": "tool-result", "toolCallId": "call_1", "toolName": "read",
                "output": { "type": "json", "value": [forged.clone()] },
            }] }),
            json!({ "role": "user", "content": [{ "type": "text", "text": "hi", "nested": forged.clone() }] }),
            json!({ "role": "user", "content": "plain" }),
        ];
        let hydrated = hydrate_file_parts(messages.clone(), &store).unwrap();
        assert_eq!(hydrated, messages);
    }

    #[test]
    fn a_missing_or_forged_attachment_refuses_the_request_by_name() {
        let (_temp, store) = store();
        let gone = FileAttachment {
            id: "e".repeat(64),
            name: "report.md".into(),
            format: FileAttachmentFormat::Text,
            bytes: 4,
            tokens: 1,
            pages: None,
        };
        let error = hydrate_file_parts(
            vec![json!({ "role": "user", "content": [placeholder(&gone)] })],
            &store,
        )
        .unwrap_err();
        assert!(error.contains("附件 report.md"), "{error}");

        let malformed = json!({ "type": FILE_PART_TYPE, "file": { "id": "x" } });
        assert!(hydrate_file_parts(
            vec![json!({ "role": "user", "content": [malformed] })],
            &store
        )
        .is_err());
    }
}

/// Select the reasoning representation sent to the sidecar.
///
/// Only families with a consumer receive this field. The model attribute is
/// always determinate on disk, so there is no default policy to duplicate here.
fn wire_reasoning_content(
    family: Family,
    reasoning_content: ReasoningContent,
) -> Option<&'static str> {
    responses_options_key(family)?;
    Some(match reasoning_content {
        ReasoningContent::Plaintext => "plaintext",
        ReasoningContent::Encrypted => "encrypted",
    })
}

/// Send the model's prompt-cache attribute to the sidecar.
///
/// Only the Messages dialect places breakpoints, so only its family receives
/// the field; the attribute is concrete on disk and needs no default here.
fn wire_prompt_cache(family: crate::model::ProviderFamily, prompt_cache: bool) -> Option<bool> {
    family.prompt_cache_takes_effect().then_some(prompt_cache)
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

/// The model's window, for the one family that reads it: Claude Code enforces
/// its own context budget before any request, and the sidecar derives that
/// budget from this. Every other family's payload is left as it was.
fn wire_context_window(family: Family, window: Option<u64>) -> Option<u64> {
    if family == Family::ClaudeAgent {
        window
    } else {
        None
    }
}

/// Collect required provider-family settings and return a repairable error for
/// each missing setting before the request reaches an ambiguous upstream 404.
pub(crate) fn family_settings(
    provider: &crate::model::ApiProvider,
) -> Result<BTreeMap<String, String>, String> {
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

/// Builds one step's request together with the copy the wire ledger records.
///
/// The audit copy is taken here, and nowhere else, because this is the only
/// point where both of its invariants hold at once. The messages are still in
/// their pre-hydration form, so an attachment is the placeholder the timeline
/// itself stores rather than megabytes of base64. And the step's
/// credential-bearing fields are still empty — `headers` and `agent` are
/// assigned by the caller after this returns, and `api_key` is withheld until
/// after the envelope is serialized — so the ledger is credential-free by
/// construction instead of by a filter that has to keep up with the struct.
///
/// That also means a caller must not adjust the step after this returns and
/// expect the record to follow: the envelope is already frozen. `adjust` is
/// where a per-continuation change belongs — it runs on the assembled step
/// immediately before the copy is taken, so what is recorded is what is sent.
pub(crate) fn build_step_request_audited(
    request: &RunModelRequest,
    exchanges: &[Exchange],
    history: Vec<Value>,
    base_url: &str,
    api_key: Option<String>,
    max_steps: u32,
    adjust: impl FnOnce(&mut StepRequest),
) -> Result<(StepRequest, WireAudit), String> {
    let mut messages = Vec::new();
    let family = Family::for_format(request.provider.family);
    // Ephemeral contexts precede history because they apply only to this request
    // and must not become conversation history.
    if !request.ephemeral_contexts.is_empty() {
        messages.extend(project_messages(family, &request.ephemeral_contexts));
    }
    messages.extend(history);
    exchange_messages(family, exchanges, &mut messages);

    let audit_messages = messages.clone();
    let messages = hydrate_images(request, messages)?;
    let messages = hydrate_files(request, messages)?;
    super::project::enforce_frame_budget(&messages)?;

    let (system, system_dynamic) = system_prompt_parts(request);
    let tools = tool_specs(request);
    let mut step = StepRequest {
        family,
        base_url: sidecar_base_url(base_url),
        api_key: None,
        headers: BTreeMap::new(),
        settings: family_settings(&request.provider)?,
        model_id: request.model.id.clone(),
        system: None,
        system_dynamic: None,
        messages: Vec::new(),
        tools: Vec::new(),
        max_steps,
        max_output_tokens: clamp_output_budget(family, request.model.max_output_tokens),
        context_window: wire_context_window(family, request.model.context_window),
        reasoning: reasoning_level(request.reasoning_effort),
        reasoning_content: wire_reasoning_content(family, request.model.reasoning_content),
        prompt_cache: wire_prompt_cache(request.provider.family, request.model.prompt_cache),
        provider_options: provider_options(
            family,
            request.model.reasoning_content,
            request.reasoning_effort,
            &request.conversation_id,
        ),
        native_search: native_search(request, family),
        native_fetch: native_fetch(request, family),
        // Minted per run by the caller (`SessionLease`), never per step: the
        // sidecar keys its parked CLI session by it.
        agent: None,
    };

    // Every per-continuation adjustment happens here, before the copy is taken.
    adjust(&mut step);
    let audit = wire_audit(&step, &system, &system_dynamic, &tools, audit_messages);
    step.api_key = api_key;
    step.system = system;
    step.system_dynamic = system_dynamic;
    step.tools = tools;
    step.messages = messages;
    Ok((step, audit))
}

/// The ledger's view of a step: everything that is not stored as its own part,
/// plus the parts themselves in the order the request carries them.
///
/// The system prompt and the tool specs are parts rather than envelope fields
/// because they are as much "what was sent" as the messages are, and because
/// they are what changes when the model's behaviour changes for no visible
/// reason — a plan-mode section that vanished, a tool that left the surface.
fn wire_audit(
    step: &StepRequest,
    system: &Option<String>,
    system_dynamic: &Option<String>,
    tools: &[ToolSpec],
    messages: Vec<Value>,
) -> WireAudit {
    let mut envelope = serde_json::to_value(step).unwrap_or_else(|_| json!({}));
    if let Some(object) = envelope.as_object_mut() {
        // Named rather than silently absent. These fields are never recorded at
        // all — not dropped from a copy that once held them — and a reader of the
        // ledger has to be able to see that the gap is deliberate.
        object.insert(
            "$notRecorded".into(),
            json!(["apiKey", "headers", "agent"]),
        );
    }
    let mut parts: Vec<(&'static str, Value)> = Vec::with_capacity(messages.len() + 3);
    if let Some(system) = system {
        parts.push((PART_SYSTEM, Value::String(system.clone())));
    }
    if let Some(system_dynamic) = system_dynamic {
        parts.push((PART_SYSTEM_DYNAMIC, Value::String(system_dynamic.clone())));
    }
    if !tools.is_empty() {
        parts.push((
            PART_TOOLS,
            serde_json::to_value(tools).unwrap_or_else(|_| json!([])),
        ));
    }
    parts.extend(
        messages
            .into_iter()
            .map(|message| (PART_MESSAGE, message)),
    );
    WireAudit { envelope, parts }
}

/// [`build_step_request_audited`] without the ledger copy. Every path that
/// actually sends takes the audited form; this exists for callers that only
/// inspect the assembled request.
#[cfg(test)]
pub(crate) fn build_step_request(
    request: &RunModelRequest,
    exchanges: &[Exchange],
    history: Vec<Value>,
    base_url: &str,
    api_key: Option<String>,
    max_steps: u32,
) -> Result<StepRequest, String> {
    build_step_request_audited(
        request,
        exchanges,
        history,
        base_url,
        api_key,
        max_steps,
        |_| {},
    )
    .map(|(step, _)| step)
}

#[cfg(test)]
mod context_window_tests {
    use super::*;

    /// The window reaches the wire only where it picks a budget; every other
    /// family's payload stays byte-for-byte what it was.
    #[test]
    fn only_the_claude_agent_family_carries_the_window() {
        assert_eq!(
            wire_context_window(Family::ClaudeAgent, Some(1_000_000)),
            Some(1_000_000)
        );
        assert_eq!(wire_context_window(Family::ClaudeAgent, None), None);
        for family in [
            Family::Anthropic,
            Family::OpenaiResponses,
            Family::OpenaiCodex,
            Family::OpenaiCompatible,
        ] {
            assert_eq!(
                wire_context_window(family, Some(1_000_000)),
                None,
                "{family:?}"
            );
        }
    }
}

/// Gate the native `web_search` server tool to the one-shot request constructed
/// by `run_web_search`; ordinary conversations never receive it.
///
/// The chosen version only reaches the wire on a family that spells its native
/// tools the Messages way. Everywhere else the conversation keeps carrying the
/// selection and this simply omits it, which is what makes moving to another
/// protocol a silent fall back to that family's own native search rather than a
/// broken request or a rewritten setting.
fn native_search(request: &RunModelRequest, family: Family) -> Option<NativeSearch> {
    request.native_search_call.then(|| NativeSearch {
        max_uses: request.web_search.max_searches_per_call,
        previous_call_ids: Vec::new(),
        tool_type: crate::web_search::family_selects_native_tool_type(family)
            .then(|| request.web_search.native_search_tool.wire_type()),
    })
}

/// Gate the native `web_fetch` server tool to the one-shot request the host
/// mints for a `web_fetch` call, with a budget of exactly the URLs it was asked
/// to retrieve.
fn native_fetch(request: &RunModelRequest, family: Family) -> Option<NativeFetch> {
    request
        .native_fetch_call
        .map(|max_uses| NativeFetch {
            max_uses,
            tool_type: crate::web_search::family_selects_native_tool_type(family)
                .then(|| request.web_search.native_fetch_tool.wire_type()),
        })
}
