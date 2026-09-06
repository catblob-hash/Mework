//! Canonical timeline to AI SDK `ModelMessage[]`.
//!
//! This is the second stage of the `wire_history` projection. The first stage,
//! [`crate::wire_history::canonical_history`], folds editable `ContextItem`s into
//! provider-neutral `CanonicalHistoryBlock`s shared by editing, folds, and branch
//! replay. This stage renders the sole remaining form; the sidecar lets AI SDK
//! translate it for each provider.
//!
//! The shape follows installed `@ai-sdk/provider-utils` type definitions because
//! documentation may diverge from the installed `ai@7` types.
//!
//! ```text
//! user      { role, content: string | Array<TextPart | ImagePart | FilePart> }
//! assistant { role, content: string | Array<TextPart | … | ToolCallPart | ToolResultPart> }
//! tool      { role, content: Array<ToolResultPart> }
//!
//! ToolCallPart   { type: "tool-call",   toolCallId, toolName, input }
//! ToolResultPart { type: "tool-result", toolCallId, toolName, output: ToolResultOutput }
//! ToolResultOutput = { type: "text", value } | { type: "error-text", value } | …
//! ```

use serde_json::{json, Value};

use crate::model::ImageAttachment;
use crate::wire_history::{canonical_history, CanonicalAssistantTurn, CanonicalHistoryBlock};

use super::protocol::{Family, MAX_LINE_BYTES};

/// Wire representation of a tool result.
///
/// Successful and failed results map to `text` and `error-text`, respectively.
/// AI SDK translates the latter to each provider's error representation.
pub(crate) fn tool_output(success: bool, output: &str) -> Value {
    json!({
        "type": if success { "text" } else { "error-text" },
        "value": output,
    })
}

/// Image placeholder.
///
/// The host hydrates actual bytes only after enforcing image count, byte, pixel,
/// and `supports_vision` limits. This layer contains only coordinates and reuses
/// `image_attachments::placeholder`.
///
/// The value occupies an AI SDK `ImagePart` `image` field and uses a `DataUrl`,
/// the only `DataContent` form that carries its MIME type.
///
/// `image_attachments::hydrate_ai_sdk_images` recognizes only this output shape
/// in user messages. It must not recursively search model-controlled tool input
/// or continuation JSON for lookalike values.
pub(crate) fn image_part(image: &ImageAttachment) -> Value {
    json!({
        "type": "image",
        "image": crate::image_attachments::placeholder(image, crate::image_attachments::WireEncoding::DataUrl),
        "mediaType": image.mime,
    })
}

fn user_message(content: &str, images: &[ImageAttachment]) -> Option<Value> {
    let trimmed = content.trim();
    if trimmed.is_empty() && images.is_empty() {
        return None;
    }
    if images.is_empty() {
        // Use string content for text-only messages because it is the most widely
        // exercised provider path.
        return Some(json!({ "role": "user", "content": trimmed }));
    }
    let mut parts = Vec::new();
    if !trimmed.is_empty() {
        parts.push(json!({ "type": "text", "text": trimmed }));
    }
    parts.extend(images.iter().map(image_part));
    Some(json!({ "role": "user", "content": parts }))
}

/// Background task result delivered by the host.
///
/// It is neither a model turn nor user input, so it projects as its own system
/// notification rather than a fabricated `task_wait` exchange.
fn host_notice_message(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| json!({ "role": "user", "content": trimmed }))
}

/// Tool-result images become a labeled user-message payload.
///
/// All providers use this bridge because `openai-compatible` serializes file
/// output as JSON text. The source label is required to associate the images with
/// their originating tool call.
pub(crate) fn push_tool_image_bridge(
    content: &mut Vec<Value>,
    tool_name: &str,
    call_id: &str,
    images: &[ImageAttachment],
) {
    if images.is_empty() {
        return;
    }
    content.push(json!({
        "type": "text",
        "text": crate::wire_history::chat_tool_image_source_label(tool_name, call_id),
    }));
    content.extend(images.iter().map(image_part));
}

/// Wire ID for a persisted tool exchange.
///
/// Local IDs never leave the host. The digest produces valid, unique provider IDs;
/// Anthropic-shaped endpoints (including the Claude Code transcript the sidecar
/// synthesizes) use `toolu_`, while other families use `call_`.
pub(crate) fn wire_tool_id(family: Family, local_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(local_id.as_bytes());
    let suffix = digest
        .iter()
        .take(24)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    match family {
        Family::Anthropic | Family::Bedrock | Family::ClaudeAgent => format!("toolu_{suffix}"),
        _ => format!("call_{suffix}"),
    }
}

/// Families whose reasoning history is only meaningful with the provider's own
/// payload: an Anthropic thinking block needs its signature, a Responses
/// reasoning item its id and ciphertext. Claude Code and Codex replay exactly
/// the blocks they stored; a card without one is omitted rather than replayed
/// as unsigned text the endpoint would reject or discard.
fn replays_signed_reasoning(family: Family) -> bool {
    matches!(
        family,
        Family::Anthropic
            | Family::Bedrock
            | Family::ClaudeAgent
            | Family::OpenaiResponses
            | Family::Azure
    )
}

fn assistant_messages(family: Family, turn: &CanonicalAssistantTurn, out: &mut Vec<Value>) {
    let mut parts: Vec<Value> = Vec::new();

    // Reasoning precedes visible text because providers require this order for
    // replayed signed reasoning blocks.
    let replayed = replays_signed_reasoning(family);
    if replayed {
        for part in &turn.reasoning_replay {
            let mut part = part.clone();
            if let Some(object) = part.as_object_mut() {
                object.insert("type".into(), json!("reasoning"));
            }
            parts.push(part);
        }
    } else if turn.reasoning_present {
        for text in &turn.reasoning {
            parts.push(json!({ "type": "reasoning", "text": text }));
        }
    }
    let content = if turn.content.trim().is_empty() && !(replayed && !turn.reasoning_replay.is_empty()) {
        // Fall back to visible text when manually supplied reasoning lacks the
        // provider-specific ID or signature needed for lossless replay. A turn
        // that replays its signed parts needs no such copy.
        turn.visible_content.as_str()
    } else {
        turn.content.as_str()
    };
    if !content.trim().is_empty() {
        parts.push(json!({ "type": "text", "text": content }));
    }
    for exchange in &turn.tools {
        parts.push(json!({
            "type": "tool-call",
            "toolCallId": wire_tool_id(family, &exchange.local_id),
            "toolName": exchange.tool_name,
            "input": exchange.requested_input,
        }));
    }

    if parts.is_empty() {
        return;
    }
    let mut message = json!({ "role": "assistant", "content": parts });
    // AI SDK drops empty reasoning parts. Message metadata preserves the explicit
    // presence contract without overriding nonempty reasoning or signed families.
    if matches!(family, Family::OpenaiChat | Family::OpenaiCompatible)
        && !turn.tools.is_empty()
        && turn.reasoning_present
        && turn.reasoning.iter().all(String::is_empty)
    {
        message["providerOptions"] = json!({ "openaiCompatible": { "reasoning_content": "" } });
    }
    out.push(message);

    // Tool results form a `tool` message immediately after the calling assistant message.
    if turn.tools.is_empty() {
        return;
    }
    let results = turn
        .tools
        .iter()
        .map(|exchange| {
            json!({
                "type": "tool-result",
                "toolCallId": wire_tool_id(family, &exchange.local_id),
                "toolName": exchange.tool_name,
                "output": tool_output(exchange.result.success, &exchange.result.output),
            })
        })
        .collect::<Vec<_>>();
    out.push(json!({ "role": "tool", "content": results }));

    // Historical tool-result images use the same bridge to preserve visibility
    // across subsequent turns.
    let mut bridge = Vec::new();
    for exchange in &turn.tools {
        push_tool_image_bridge(
            &mut bridge,
            &exchange.tool_name,
            &wire_tool_id(family, &exchange.local_id),
            &exchange.result.images,
        );
    }
    if !bridge.is_empty() {
        out.push(json!({ "role": "user", "content": bridge }));
    }
}

/// Projects one canonical block into `ModelMessage`s appended to `out`.
///
/// Project blocks individually because `wire_history` incrementally caches them;
/// full projection is a fold over this function.
///
/// Do not merge adjacent roles here. The AI SDK Anthropic provider handles that
/// protocol requirement, so cached projections compose with `Vec::extend`.
pub(crate) fn project_block(family: Family, block: &CanonicalHistoryBlock, out: &mut Vec<Value>) {
    match block {
        CanonicalHistoryBlock::User { content, images } => {
            out.extend(user_message(content, images));
        }
        CanonicalHistoryBlock::Assistant(turn) => assistant_messages(family, turn, out),
        CanonicalHistoryBlock::HostNotice { text } => {
            out.extend(host_notice_message(text));
        }
    }
}

/// Projects a canonical timeline into AI SDK `ModelMessage[]`.
pub(crate) fn project_messages(family: Family, contexts: &[crate::model::ContextItem]) -> Vec<Value> {
    let mut messages = Vec::new();
    for block in canonical_history(contexts) {
        project_block(family, &block, &mut messages);
    }
    messages
}

/// Serialization-size guard for the complete message array.
///
/// Host and sidecar both limit a single NDJSON line. Reject oversized histories
/// before writing the step frame rather than restarting the sidecar on a protocol
/// violation.
pub(crate) fn enforce_frame_budget(messages: &[Value]) -> Result<(), String> {
    let bytes = serde_json::to_vec(messages).map(|encoded| encoded.len()).unwrap_or(usize::MAX);
    // Reserve space for the envelope, tool definitions, system prompt, and options.
    let budget = MAX_LINE_BYTES / 2;
    if bytes > budget {
        return Err(format!(
            "本次请求的对话历史序列化后为 {} MiB，超过单帧 {} MiB 的预算；请先压缩或分支该对话",
            bytes / 1024 / 1024,
            budget / 1024 / 1024
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(content: &str, reasoning: &str) -> CanonicalAssistantTurn {
        CanonicalAssistantTurn {
            content: content.into(),
            reasoning: if reasoning.is_empty() { Vec::new() } else { vec![reasoning.into()] },
            reasoning_present: !reasoning.is_empty(),
            reasoning_replay: Vec::new(),
            visible_content: content.into(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn a_plain_user_turn_projects_as_a_string_content() {
        let message = user_message("你好", &[]).expect("非空用户消息");
        assert_eq!(message["role"], "user");
        // Text-only messages use string content because it is the most exercised
        // provider path.
        assert_eq!(message["content"], "你好");
    }

    #[test]
    fn an_empty_user_turn_projects_to_nothing() {
        // Empty user messages must not reach upstream providers, which may reject them.
        assert!(user_message("   ", &[]).is_none());
    }

    #[test]
    fn a_tool_exchange_becomes_a_call_part_and_a_following_tool_message() {
        let mut turn = assistant("好的", "");
        turn.tools.push(crate::wire_history::CanonicalToolExchange {
            local_id: "call_1".into(),
            tool_name: "ls".into(),
            requested_input: serde_json::from_str(r#"{"path":"src"}"#).unwrap(),
            result: crate::model::ToolResult {
                success: true,
                output: "src/main.rs".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-08-27T00:00:00Z".into(),
                duration_ms: 1,
            },
        });

        let mut out = Vec::new();
        assistant_messages(Family::Anthropic, &turn, &mut out);
        assert_eq!(out.len(), 2, "一条 assistant + 一条 tool");
        assert_eq!(out[0]["role"], "assistant");
        let call = out[0]["content"]
            .as_array()
            .expect("assistant 内容是数组")
            .iter()
            .find(|part| part["type"] == "tool-call")
            .expect("必须有 tool-call 部件");
        // Wire IDs are digests of local IDs with family-specific prefixes.
        let wire_id = wire_tool_id(Family::Anthropic, "call_1");
        assert!(wire_id.starts_with("toolu_"), "{wire_id}");
        assert_eq!(call["toolCallId"], wire_id);
        assert_eq!(call["toolName"], "ls");
        assert_eq!(call["input"]["path"], "src");

        // A result must immediately follow its call and use the same ID.
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["content"][0]["toolCallId"], wire_id);
        assert_eq!(out[1]["content"][0]["output"]["type"], "text");
    }

    #[test]
    fn a_failed_tool_result_is_marked_as_an_error_not_as_prose() {
        // Failed results must not be represented as successful text.
        assert_eq!(tool_output(false, "权限不足")["type"], "error-text");
        assert_eq!(tool_output(true, "ok")["type"], "text");
    }

    #[test]
    fn reasoning_precedes_the_visible_answer() {
        let mut out = Vec::new();
        assistant_messages(Family::OpenaiChat, &assistant("答案", "先想一想"), &mut out);
        let parts = out[0]["content"].as_array().expect("数组");
        assert_eq!(parts[0]["type"], "reasoning");
        assert_eq!(parts[1]["type"], "text");
    }

    #[test]
    fn an_oversized_history_is_refused_with_a_repairable_message() {
        let big = vec![json!({ "role": "user", "content": "x".repeat(MAX_LINE_BYTES) })];
        let error = enforce_frame_budget(&big).expect_err("超预算必须被拒绝");
        assert!(error.contains("超过单帧"), "{error}");
        // Ordinary histories must remain within the budget.
        assert!(enforce_frame_budget(&[json!({ "role": "user", "content": "hi" })]).is_ok());
    }
}
