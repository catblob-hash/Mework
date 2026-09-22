//! Host-attested conversation templates.
//!
//! A template is a saved message queue that can be replayed as the opening
//! history of a conversation or of a subagent role. Both the capture and the
//! replay are host operations for the reason spelled out in
//! [`crate::conversation_fork`]: a template carries tool cards, and a tool
//! result is only persistable when the host holds a receipt binding it to *that*
//! conversation. Capture names a conversation to copy from, and apply names a
//! template to copy in; neither takes a body from the renderer.
//!
//! Editing a body does, and [`normalize_tool_cards`] is what keeps that door
//! safe: [`instantiate`] re-attests every card for the target conversation, so
//! whatever reaches a template body becomes a card this application later
//! vouches for. Normalization takes the fields only the host can vouch for out
//! of the submission's hands — from the stored record when there is one, as a
//! text-only mint when there is not — leaving the template editor exactly as
//! powerful as the conversation timeline editor, and no stronger.
//!
//! Capture reads settled trunk rows only. A streaming row is the source's
//! current round, half written, and not history a template should claim.

use std::collections::HashMap;

use crate::{
    conversation_fork::{fork_contexts, ForkRequest},
    model::{ContextItem, ToolResult},
    state::AppState,
};

/// The token a template's user messages use to say where the caller's own input
/// belongs. Only `agent_spawn` substitutes it; applying a template to a
/// conversation leaves it verbatim, because there is no input to place.
pub const INPUT_PLACEHOLDER: &str = "{input}";

/// Copies a template body for `target_conversation_id`, rewriting every id and
/// re-attesting every tool result for its new owner.
///
/// The whole body is copied, so the cut point handed to [`fork_contexts`] is the
/// last row. An empty template instantiates to an empty history rather than an
/// error: a template with nothing in it is a legal, if useless, template.
pub fn instantiate(
    state: &AppState,
    workspace_path: &str,
    target_conversation_id: &str,
    contexts: &[ContextItem],
) -> Result<Vec<ContextItem>, String> {
    let Some(last) = contexts.last() else {
        return Ok(Vec::new());
    };
    let mut instantiated = fork_contexts(
        state,
        &ForkRequest {
            workspace_path,
            target_conversation_id,
            through_context_id: last.id(),
        },
        contexts,
        |prefix| format!("{prefix}_{}", uuid::Uuid::new_v4().simple()),
    )?;
    // A fork carries the provider call id over, because a fork happens once into
    // a conversation of its own. A template does not: the same body can be
    // applied twice into one conversation, and then both copies would claim the
    // same id — two `tool_use` blocks with one id in a single request. Cards
    // placed from a template therefore mint a digest from their own fresh id,
    // which is unique by construction. The attestation is unaffected: it is
    // issued over the card's evidence fields, and this is not one of them.
    for context in &mut instantiated {
        if let ContextItem::Tool {
            provider_call_id, ..
        } = context
        {
            *provider_call_id = None;
        }
    }
    Ok(instantiated)
}

/// Normalizes a renderer-edited body so no card claims more than this
/// application can vouch for.
///
/// Editing a template requires sending a body, and [`instantiate`] re-attests
/// every card in it for the target conversation: a card that reaches a template
/// body becomes, on the next Apply, a card this application says it ran. So the
/// evidence fields are never taken from the submission.
///
/// - A card whose id matches a stored tool card and is identical to it is kept
///   verbatim: a captured template's real results, diffs, images, durations,
///   subagent records and attestations survive a round trip untouched.
/// - A card whose id matches a stored card but differs is a rewrite. The stored
///   record keeps `success`, `diff`, `executed_at`, `duration_ms`, `round`,
///   `model_turn_id`, `subagent` and `created_at`; only the tool name, input,
///   output text and images come from the submission.
/// - A card the template never had is a placement, minted as a hand-written
///   call: text only, never a diff, images, or a duration it did not spend.
///
/// A rewrite is refused outright when the user could only have meant to forge:
/// swapping the tool name, adding or reordering images, or editing a card that
/// carries a subagent record. Dropping a card stays legal, and so does anything
/// at all done to the prose around it: prose attests to nothing.
///
/// Rewritten and placed cards come out with `requested_input: None` (the card no
/// longer answers a model request) and an empty `attestation`: [`instantiate`]
/// re-issues a token for the target conversation, and a stale one bound to
/// another conversation is a lie on disk. This is the same trade the timeline
/// editor makes through `conversations::attest_edited_tool_context` and
/// `conversations::attest_inserted_tool_context`, and no stronger.
pub fn normalize_tool_cards(
    stored: &[ContextItem],
    submitted: Vec<ContextItem>,
) -> Result<Vec<ContextItem>, String> {
    let known: HashMap<&str, &ContextItem> = stored
        .iter()
        .filter(|item| matches!(item, ContextItem::Tool { .. }))
        .map(|item| (item.id(), item))
        .collect();
    let mut normalized = Vec::with_capacity(submitted.len());
    for item in submitted {
        if !matches!(item, ContextItem::Tool { .. }) {
            normalized.push(item);
            continue;
        }
        let unchanged = match &item {
            ContextItem::Tool { id, .. } => known
                .get(id.as_str())
                .is_some_and(|stored| **stored == item),
            _ => false,
        };
        if unchanged {
            normalized.push(item);
            continue;
        }
        let ContextItem::Tool {
            id,
            tool_name,
            input,
            result: submitted_result,
            created_at,
            ..
        } = item
        else {
            unreachable!("上面已经确认这是工具卡");
        };
        let Some(&stored_card) = known.get(id.as_str()) else {
            // 模板里从来没有这张卡：宿主只认下文本，diff、图片与耗时都是它没花过的。
            normalized.push(ContextItem::Tool {
                id,
                tool_name,
                round: None,
                model_turn_id: None,
                // 模板卡不是某次 provider 交换：同一个模板可以在一段对话里套用
                // 多次，照搬 provider call id 会让两张卡认领同一个线上 id。
                provider_call_id: None,
                requested_input: None,
                input,
                result: ToolResult {
                    success: true,
                    output: submitted_result.output,
                    images: Vec::new(),
                    diff: None,
                    executed_at: String::new(),
                    duration_ms: 0,
                },
                subagent: None,
                attestation: String::new(),
                created_at,
            });
            continue;
        };
        let ContextItem::Tool {
            tool_name: stored_tool_name,
            round: stored_round,
            model_turn_id: stored_model_turn_id,
            result: stored_result,
            subagent: stored_subagent,
            created_at: stored_created_at,
            ..
        } = stored_card
        else {
            unreachable!("known 只收工具卡");
        };
        if stored_subagent.is_some() {
            return Err("带有子代理记录的工具卡不能手动编辑".into());
        }
        if tool_name != *stored_tool_name {
            return Err(format!(
                "工具卡 {id}（{tool_name}）的工具名称与模板里已保存的记录不一致"
            ));
        }
        // 与 `attest_edited_tool_context` 相同的构造顺序：先比图片、再改 output，
        // 这样 ordered-subset 比较器顺带守住了宿主拥有的每一个结果字段。
        let mut edited_result = stored_result.clone();
        edited_result.images = submitted_result.images;
        if !crate::state::tool_result_is_exact_or_image_removal(stored_result, &edited_result) {
            return Err(format!(
                "工具卡 {id}（{tool_name}）的结果图片只能按原顺序保留或删除"
            ));
        }
        edited_result.output = submitted_result.output;
        normalized.push(ContextItem::Tool {
            id,
            tool_name,
            round: *stored_round,
            model_turn_id: stored_model_turn_id.clone(),
            // 同上：套用出来的卡不认领任何一次真实调用的 id。
            provider_call_id: None,
            requested_input: None,
            input,
            result: edited_result,
            // 上面的检查已经证明它没有子代理记录，这里保持存储卡的口径。
            subagent: None,
            attestation: String::new(),
            created_at: stored_created_at.clone(),
        });
    }
    Ok(normalized)
}

/// How many `{input}` placeholders the template's user messages hold in total.
fn placeholder_count(contexts: &[ContextItem]) -> usize {
    contexts
        .iter()
        .filter_map(|context| match context {
            ContextItem::User { content, .. } => Some(content),
            _ => None,
        })
        .map(|content| content.matches(INPUT_PLACEHOLDER).count())
        .sum()
}

/// Places a subagent's task into its seeded template history.
///
/// Exactly one `{input}` across all of the template's user messages substitutes
/// in place, and nothing is appended. Zero placeholders append the task as a
/// final user message, which is also what a child with no template gets. Two or
/// more are ignored and likewise append: several placeholders give no single
/// answer to which one the caller meant, and quietly filling all of them would
/// repeat the task as many times as the template happens to mention it.
///
/// `new_id` and `created_at` are supplied by the caller so this stays a pure
/// function over the seeding decision, which is the part worth testing.
pub fn seed_with_task(
    contexts: Vec<ContextItem>,
    task: &str,
    new_id: impl FnOnce(&str) -> String,
    created_at: String,
) -> Vec<ContextItem> {
    let mut contexts = contexts;
    if placeholder_count(&contexts) == 1 {
        for context in &mut contexts {
            if let ContextItem::User { content, .. } = context {
                if content.contains(INPUT_PLACEHOLDER) {
                    *content = content.replacen(INPUT_PLACEHOLDER, task, 1);
                    return contexts;
                }
            }
        }
    }
    contexts.push(ContextItem::User {
        id: new_id("subagent-task"),
        content: task.to_owned(),
        images: Vec::new(),
        created_at,
    });
    contexts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::default_document,
        model::{ImageAttachment, ToolResult},
        storage::validate_save_transition,
    };

    /// Puts `contexts` into a second conversation and asks whether the document
    /// would save. This is the gate a renderer-side copy fails.
    fn saves_into_a_second_conversation(
        contexts: Vec<ContextItem>,
        state: &AppState,
    ) -> Result<(), String> {
        let previous = default_document();
        let mut next = previous.clone();
        let mut target = previous.workspaces[0].conversations[0].clone();
        target.id = "conv_template_target".into();
        target.contexts = contexts;
        target.branches.clear();
        next.workspaces[0].conversations.push(target);
        validate_save_transition(&previous, &next, state).map(|_| ())
    }

    #[test]
    fn an_instantiated_template_carries_tool_cards_the_target_can_persist() {
        let document = default_document();
        let workspace = &document.workspaces[0];
        let body = &workspace.conversations[0].contexts;
        assert!(
            body.iter().any(|c| matches!(c, ContextItem::Tool { .. })),
            "夹具无效：模板正文必须含工具卡，否则这条测试什么也没证明"
        );
        let state = AppState::default();

        let instantiated = instantiate(&state, &workspace.path, "conv_template_target", body)
            .expect("模板套用不应失败");

        assert_eq!(instantiated.len(), body.len(), "整份模板都要复制");
        assert!(
            instantiated
                .iter()
                .zip(body)
                .all(|(copy, source)| copy.id() != source.id()),
            "每条上下文都必须换用新身份"
        );
        saves_into_a_second_conversation(instantiated, &state)
            .expect("重新签发过凭证的模板正文必须可落盘");
    }

    #[test]
    fn the_same_body_without_re_attestation_is_refused() {
        // The point of routing capture and apply through the host. Copying the
        // rows verbatim is what a renderer-side implementation would do, and it
        // is exactly the transition the receipt boundary exists to reject — so
        // the re-attestation above is load-bearing, not ceremony.
        let document = default_document();
        let body = document.workspaces[0].conversations[0].contexts.clone();
        let error = saves_into_a_second_conversation(body, &AppState::default())
            .expect_err("未经宿主重新签发的工具卡不得进入另一个对话");
        assert!(!error.is_empty(), "拒绝必须带出原因");
    }

    #[test]
    fn an_empty_template_instantiates_to_an_empty_history() {
        let instantiated =
            instantiate(&AppState::default(), "/ws", "conv_target", &[]).expect("空模板是合法模板");
        assert!(instantiated.is_empty());
    }

    fn user(id: &str, content: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: content.into(),
            images: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn assistant(id: &str, content: &str) -> ContextItem {
        ContextItem::Assistant {
            id: id.into(),
            content: content.into(),
            round: None,
            model_turn_id: None,
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn seeded(contexts: Vec<ContextItem>, task: &str) -> Vec<ContextItem> {
        seed_with_task(
            contexts,
            task,
            |prefix| format!("{prefix}_new"),
            "2026-01-01T00:00:00Z".into(),
        )
    }

    fn bodies(contexts: &[ContextItem]) -> Vec<&str> {
        contexts
            .iter()
            .map(|context| match context {
                ContextItem::User { content, .. } | ContextItem::Assistant { content, .. } => {
                    content.as_str()
                }
                _ => "",
            })
            .collect()
    }

    #[test]
    fn one_placeholder_substitutes_in_place_and_appends_nothing() {
        let seeded = seeded(
            vec![user("a", "Review this: {input}"), assistant("b", "Sure.")],
            "the diff",
        );
        assert_eq!(bodies(&seeded), ["Review this: the diff", "Sure."]);
    }

    #[test]
    fn the_placeholder_may_sit_in_any_user_message() {
        let seeded = seeded(
            vec![
                user("a", "You are a reviewer."),
                assistant("b", "Understood."),
                user("c", "Now review {input} carefully."),
            ],
            "PR 12",
        );
        assert_eq!(
            bodies(&seeded),
            [
                "You are a reviewer.",
                "Understood.",
                "Now review PR 12 carefully."
            ]
        );
    }

    #[test]
    fn no_placeholder_appends_the_task_as_the_final_user_message() {
        let seeded = seeded(vec![user("a", "You are a reviewer.")], "review this");
        assert_eq!(bodies(&seeded), ["You are a reviewer.", "review this"]);
        assert!(matches!(seeded[1], ContextItem::User { .. }));
    }

    #[test]
    fn two_placeholders_are_ignored_and_the_task_is_appended_verbatim() {
        // Both survive untouched: ignoring the mechanism means leaving the text
        // alone, not blanking placeholders the user can still see and fix.
        let seeded = seeded(
            vec![user("a", "{input}"), user("b", "again: {input}")],
            "the task",
        );
        assert_eq!(bodies(&seeded), ["{input}", "again: {input}", "the task"]);
    }

    #[test]
    fn two_placeholders_in_one_message_are_ignored_too() {
        // The rule counts across the whole template, not per message.
        let seeded = seeded(vec![user("a", "{input} and {input}")], "the task");
        assert_eq!(bodies(&seeded), ["{input} and {input}", "the task"]);
    }

    #[test]
    fn a_placeholder_outside_a_user_message_does_not_count() {
        // Only user messages carry the caller's input, so an assistant example
        // that happens to mention the token is prose, not a slot.
        let seeded = seeded(
            vec![
                user("a", "Review {input}."),
                assistant("b", "I saw {input}."),
            ],
            "PR 12",
        );
        assert_eq!(bodies(&seeded), ["Review PR 12.", "I saw {input}."]);
    }

    #[test]
    fn an_empty_template_reduces_to_the_task_alone() {
        let seeded = seeded(Vec::new(), "just the task");
        assert_eq!(bodies(&seeded), ["just the task"]);
    }

    fn template_body() -> Vec<ContextItem> {
        default_document().workspaces[0].conversations[0]
            .contexts
            .clone()
    }

    fn edited_tool_card(card: &ContextItem, edit: impl FnOnce(&mut ContextItem)) -> ContextItem {
        let mut edited = card.clone();
        edit(&mut edited);
        edited
    }

    fn tool_input(value: serde_json::Value) -> crate::model::JsonObject {
        serde_json::from_value(value).expect("object literal")
    }

    fn tool_count(body: &[ContextItem]) -> usize {
        body.iter()
            .filter(|item| matches!(item, ContextItem::Tool { .. }))
            .count()
    }

    fn image(name: &str) -> ImageAttachment {
        ImageAttachment {
            id: format!("{name:0>64}"),
            name: format!("{name}.png"),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: 20,
            short_id: None,
        }
    }

    fn subagent_record() -> crate::model::SubagentRunRecord {
        serde_json::from_value(serde_json::json!({
            "task": "child",
            "status": "completed",
            "contexts": [],
            "updates": [],
        }))
        .expect("最小子代理记录")
    }

    const STORED_DIFF: &str = "--- 宿主记录的 diff\n+++ b\n";

    /// A stored card carrying every host-owned field, so a test can name each
    /// one the normalization rule must preserve or drop.
    fn recorded_tool_card(id: &str) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "write".into(),
            round: Some(3),
            model_turn_id: Some("turn_3".into()),
            provider_call_id: None,
            requested_input: Some(tool_input(serde_json::json!({"path": "/tmp/model"}))),
            input: tool_input(serde_json::json!({"path": "/tmp/x", "content": "y"})),
            result: ToolResult {
                success: false,
                output: "宿主记录的输出".into(),
                images: vec![image("a"), image("b"), image("c")],
                diff: Some(STORED_DIFF.into()),
                executed_at: "2026-01-01T00:00:00Z".into(),
                duration_ms: 42,
            },
            subagent: None,
            attestation: "另一个对话的凭证".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    /// A submitted card the template never had, claiming every field a
    /// hand-written call could not honestly carry.
    fn placed_tool_card(id: &str) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "shell".into(),
            round: Some(9),
            model_turn_id: Some("turn_forged".into()),
            provider_call_id: None,
            requested_input: Some(tool_input(serde_json::json!({"command": "rm -rf build"}))),
            input: tool_input(serde_json::json!({"command": "rm -rf build"})),
            result: ToolResult {
                success: false,
                output: "已删除 build".into(),
                images: vec![image("d")],
                diff: Some("伪造的 diff".into()),
                executed_at: "2099-01-01T00:00:00Z".into(),
                duration_ms: 12_345,
            },
            subagent: None,
            attestation: "提交方伪造的凭证".into(),
            created_at: "2026-02-02T00:00:00Z".into(),
        }
    }

    /// What `placed_tool_card` normalizes to: identity and text, nothing else.
    fn minted_tool_card(id: &str) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "shell".into(),
            round: None,
            model_turn_id: None,
            provider_call_id: None,
            requested_input: None,
            input: tool_input(serde_json::json!({"command": "rm -rf build"})),
            result: ToolResult {
                success: true,
                output: "已删除 build".into(),
                images: Vec::new(),
                diff: None,
                executed_at: String::new(),
                duration_ms: 0,
            },
            subagent: None,
            attestation: String::new(),
            created_at: "2026-02-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn an_untouched_body_round_trips_byte_identical() {
        let stored = vec![user("ctx_u", "hi"), recorded_tool_card("ctx_recorded")];
        let ContextItem::Tool {
            result,
            attestation,
            ..
        } = &stored[1]
        else {
            panic!("夹具无效：这里必须是一张工具卡");
        };
        assert!(
            result.diff.is_some()
                && !result.images.is_empty()
                && !attestation.is_empty()
                && result.duration_ms > 0,
            "夹具无效：存储卡要带 diff、图片、耗时和凭证，原样回传才证明得了东西"
        );

        let normalized = normalize_tool_cards(&stored, stored.clone()).expect("原样提交必须通过");
        assert_eq!(normalized, stored, "未改动的正文必须逐字节原样保留");
    }

    #[test]
    fn an_untouched_captured_card_keeps_its_subagent_record() {
        // 捕获下来的子代理记录是真跑过的证据，原样提交时不得被规范化洗掉。
        let mut card = recorded_tool_card("ctx_child");
        if let ContextItem::Tool { subagent, .. } = &mut card {
            *subagent = Some(subagent_record());
        }
        let stored = vec![card];
        let normalized = normalize_tool_cards(&stored, stored.clone()).expect("原样提交必须通过");
        assert_eq!(normalized, stored, "捕获下来的子代理记录必须原样保留");
    }

    #[test]
    fn an_untouched_template_body_normalizes_to_itself() {
        let body = template_body();
        assert!(tool_count(&body) > 0, "夹具无效：正文里要有工具卡");
        let normalized = normalize_tool_cards(&body, body.clone()).expect("原样提交必须通过");
        assert_eq!(normalized, body);
    }

    #[test]
    fn dropping_a_tool_card_passes_normalization() {
        let body = template_body();
        let tools = tool_count(&body);
        assert!(tools > 0, "夹具无效：正文里必须有工具卡可删");
        let trimmed = body
            .iter()
            .filter(|item| !matches!(item, ContextItem::Tool { .. }))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(trimmed.len(), body.len() - tools);
        let normalized =
            normalize_tool_cards(&body, trimmed.clone()).expect("删掉工具卡是允许的方向");
        assert_eq!(normalized, trimmed, "没动的卡不该被规范化改写");
    }

    #[test]
    fn editing_prose_passes_normalization() {
        let body = template_body();
        let mut submitted = body.clone();
        let mut edited = 0;
        for item in &mut submitted {
            match item {
                ContextItem::User { content, .. } | ContextItem::Assistant { content, .. } => {
                    *content = format!("{content}（改过的散文）");
                    edited += 1;
                }
                _ => {}
            }
        }
        assert!(edited > 0, "夹具无效：正文里要有散文可改");
        let normalized =
            normalize_tool_cards(&body, submitted.clone()).expect("散文改什么都不该拦");
        assert_eq!(normalized, submitted, "散文原样回传");
    }

    #[test]
    fn inserting_a_user_message_passes_normalization() {
        let body = template_body();
        let tool_position = body
            .iter()
            .position(|item| matches!(item, ContextItem::Tool { .. }))
            .expect("夹具无效：正文必须含工具卡");
        let mut submitted = body.clone();
        submitted.insert(tool_position, user("ctx_inserted", "新加的一句"));
        assert!(matches!(submitted[tool_position], ContextItem::User { .. }));
        let normalized =
            normalize_tool_cards(&body, submitted.clone()).expect("插入一条用户消息不该拦");
        assert_eq!(normalized, submitted);
    }

    #[test]
    fn reordering_contexts_passes_normalization() {
        let body = template_body();
        let mut submitted = body.clone();
        submitted.reverse();
        assert_ne!(submitted, body, "夹具无效：正文要能被排出一个不同的顺序");
        let normalized =
            normalize_tool_cards(&body, submitted.clone()).expect("重排顺序不该拦");
        assert_eq!(normalized, submitted);
    }

    #[test]
    fn a_rewrite_keeps_the_stored_evidence_and_takes_the_submitted_text() {
        let stored = vec![recorded_tool_card("ctx_recorded")];
        let rewritten = edited_tool_card(&stored[0], |item| match item {
            ContextItem::Tool {
                input,
                result,
                created_at,
                ..
            } => {
                *input = tool_input(serde_json::json!({"path": "/tmp/rewritten", "content": "z"}));
                result.output = "用户改写的输出".into();
                *created_at = "2099-12-31T00:00:00Z".into();
                // 提交方声称的每一项宿主证据都必须被无视。
                result.success = true;
                result.diff = Some("伪造的 diff".into());
                result.executed_at = "2099-01-01T00:00:00Z".into();
                result.duration_ms = 999_999;
            }
            _ => panic!("夹具无效：这里必须是一张工具卡"),
        });
        let normalized = normalize_tool_cards(&stored, vec![rewritten]).expect("改写是允许的方向");
        assert_eq!(
            normalized,
            vec![ContextItem::Tool {
                id: "ctx_recorded".into(),
                tool_name: "write".into(),
                round: Some(3),
                model_turn_id: Some("turn_3".into()),
                provider_call_id: None,
                requested_input: None,
                input: tool_input(serde_json::json!({"path": "/tmp/rewritten", "content": "z"})),
                result: ToolResult {
                    success: false,
                    output: "用户改写的输出".into(),
                    images: vec![image("a"), image("b"), image("c")],
                    diff: Some(STORED_DIFF.into()),
                    executed_at: "2026-01-01T00:00:00Z".into(),
                    duration_ms: 42,
                },
                subagent: None,
                attestation: String::new(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }]
        );
    }

    #[test]
    fn a_rewrite_clears_attestation_and_requested_input() {
        // 存储卡带着 requestedInput 与绑定别的对话的凭证；提交方又各补上一份自己的。
        // 两者都必须清空：改写后的卡不再回应模型请求，而一张绑定别的对话的凭证
        // 落在盘上就是一句谎。
        let stored = vec![recorded_tool_card("ctx_recorded")];
        let rewritten = edited_tool_card(&stored[0], |item| match item {
            ContextItem::Tool {
                result,
                requested_input,
                attestation,
                ..
            } => {
                result.output = "只改了输出".into();
                *requested_input = Some(tool_input(serde_json::json!({"path": "/tmp/model"})));
                *attestation = "提交方伪造的凭证".into();
            }
            _ => panic!("夹具无效：这里必须是一张工具卡"),
        });
        let normalized = normalize_tool_cards(&stored, vec![rewritten]).expect("改写是允许的方向");
        let ContextItem::Tool {
            requested_input,
            attestation,
            ..
        } = &normalized[0]
        else {
            panic!("夹具无效：规范化后仍然是一张工具卡");
        };
        assert!(requested_input.is_none(), "改写后的卡不再回应模型请求");
        assert!(
            attestation.is_empty(),
            "改写的卡不得携带绑定别的对话的凭证"
        );
    }

    #[test]
    fn a_placed_card_is_minted_text_only() {
        let stored = vec![recorded_tool_card("ctx_recorded")];
        let normalized = normalize_tool_cards(&stored, vec![placed_tool_card("ctx_placed")])
            .expect("放置新卡是允许的方向");
        assert_eq!(normalized, vec![minted_tool_card("ctx_placed")]);
    }

    #[test]
    fn a_placed_card_is_minted_without_stored_context_too() {
        // 模板空着的时候同样能放卡：没有任何存储记录可查，纯粹是铸造。
        let normalized =
            normalize_tool_cards(&[], vec![placed_tool_card("ctx_placed")]).expect("空模板也能放卡");
        assert_eq!(normalized, vec![minted_tool_card("ctx_placed")]);
    }

    #[test]
    fn a_placed_card_cannot_claim_a_subagent_record() {
        let mut placed = placed_tool_card("ctx_placed");
        if let ContextItem::Tool { subagent, .. } = &mut placed {
            *subagent = Some(subagent_record());
        }
        let normalized = normalize_tool_cards(&[], vec![placed]).expect("放置新卡是允许的方向");
        let ContextItem::Tool { subagent, .. } = &normalized[0] else {
            panic!("夹具无效：规范化后仍然是一张工具卡");
        };
        assert!(subagent.is_none(), "手写的卡不得伪造一条子代理记录");
    }

    #[test]
    fn rewriting_a_card_that_carries_a_subagent_record_is_refused() {
        let mut card = recorded_tool_card("ctx_child");
        if let ContextItem::Tool { subagent, .. } = &mut card {
            *subagent = Some(subagent_record());
        }
        let stored = vec![card];
        let rewritten = edited_tool_card(&stored[0], |item| match item {
            ContextItem::Tool { result, .. } => result.output = "改过的输出".into(),
            _ => panic!("夹具无效：这里必须是一张工具卡"),
        });
        let error = normalize_tool_cards(&stored, vec![rewritten])
            .expect_err("带子代理记录的工具卡不能手动编辑");
        assert!(error.contains("子代理"), "拒绝信息要点明原因：{error}");
    }

    #[test]
    fn changing_a_cards_tool_name_is_refused() {
        let stored = vec![recorded_tool_card("ctx_recorded")];
        let substituted = edited_tool_card(&stored[0], |item| match item {
            ContextItem::Tool { tool_name, .. } => *tool_name = "bash".into(),
            _ => panic!("夹具无效：这里必须是一张工具卡"),
        });
        let error = normalize_tool_cards(&stored, vec![substituted]).expect_err("换工具名必须被拒");
        assert!(
            error.contains("ctx_recorded") && error.contains("工具名称"),
            "拒绝信息要指出是哪张卡、为什么：{error}"
        );
    }

    #[test]
    fn rewriting_images_must_be_an_ordered_subset() {
        let stored = vec![recorded_tool_card("ctx_recorded")];
        let ContextItem::Tool { result, .. } = &stored[0] else {
            panic!("夹具无效：这里必须是一张工具卡");
        };
        let images = result.images.clone();
        let with_images = |images: Vec<ImageAttachment>| {
            edited_tool_card(&stored[0], |item| match item {
                ContextItem::Tool { result, .. } => result.images = images,
                _ => panic!("夹具无效：这里必须是一张工具卡"),
            })
        };

        // 允许：按原顺序保留一个子集，或整组删掉。
        for retained in [vec![images[0].clone(), images[2].clone()], Vec::new()] {
            let normalized = normalize_tool_cards(&stored, vec![with_images(retained.clone())])
                .expect("按原顺序删除图片是允许的方向");
            let ContextItem::Tool { result, .. } = &normalized[0] else {
                panic!("夹具无效：规范化后仍然是一张工具卡");
            };
            assert_eq!(result.images, retained);
        }

        // 拒绝：重排、新增，或改动图片自身的元数据。
        let mut changed_metadata = images[0].clone();
        changed_metadata.name = "改过的名字.png".into();
        for invalid in [
            vec![images[1].clone(), images[0].clone(), images[2].clone()],
            vec![images[2].clone(), images[0].clone()],
            vec![
                images[0].clone(),
                images[1].clone(),
                images[2].clone(),
                image("d"),
            ],
            vec![changed_metadata],
        ] {
            assert!(
                normalize_tool_cards(&stored, vec![with_images(invalid)]).is_err(),
                "改写不得新增、重排或改动工具结果图片"
            );
        }
    }
}
