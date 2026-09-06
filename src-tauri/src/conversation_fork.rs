//! Host-attested conversation forking.
//!
//! A branch is an independent conversation that owns a full copy of the history
//! it forked from. Copying is deliberately done here rather than in the
//! renderer because two document invariants forbid a naive client-side copy:
//!
//! 1. Context ids are unique across the whole document
//!    ([`crate::storage::validate_shape`]), so every copied item needs a fresh
//!    id.
//! 2. A tool result is only persistable when the host holds an execution
//!    receipt binding it to *that* conversation. Receipts intentionally do not
//!    move between conversations — `tool_context_and_receipt_cannot_move_between_conversations`
//!    exists to keep a renderer from relocating a result it did not execute.
//!
//! Re-attesting here preserves that guarantee instead of weakening it. The host
//! reads the source contexts from its own committed document, so the copy is
//! known-good output the host itself previously produced and validated; the
//! renderer cannot smuggle a forged result in through this path, because
//! nothing it sends is copied — it only names a source conversation and a cut
//! point.
//!
//! Image attachments are shared by reference. The sidecar store is content
//! addressed and reference counted at save time, so a branch that cites the
//! same attachment id keeps it alive without duplicating bytes.

use std::collections::HashMap;

use crate::{
    model::{Conversation, ContextItem, SubagentRunRecord, ToolExecutionRequest, ToolResult},
    state::AppState,
};

/// Whether `target_conversation_id` may receive a forked history.
///
/// The renderer creates the branch conversation and flushes it to disk *before*
/// calling the host, because the host copies from its own committed document and
/// therefore has to be able to see the target. A guard that rejected every
/// existing target contradicted that: the renderer wrote the conversation to
/// satisfy the host, and the host refused because it was written. Every fork
/// past the first message failed deterministically.
///
/// The guard still exists — it just tests the thing that actually matters. A
/// target that already holds history would have that history silently replaced
/// by the copy, so it is still rejected. An empty target is the expected state.
pub fn validate_fork_target(
    target: Option<&Conversation>,
    target_conversation_id: &str,
) -> Result<(), String> {
    let Some(target) = target else {
        return Ok(());
    };
    if !target.contexts.is_empty() {
        return Err(format!(
            "分支目标对话 {target_conversation_id} 已有历史，无法作为分支目标"
        ));
    }
    Ok(())
}

/// Where the copy stops, expressed as an id rather than an index so a
/// concurrent edit cannot silently shift the cut point.
pub struct ForkRequest<'a> {
    pub workspace_path: &'a str,
    pub target_conversation_id: &'a str,
    /// The last context to include. Everything after it is dropped.
    pub through_context_id: &'a str,
}

/// Copies `contexts` up to and including `through_context_id`, rewriting every
/// id and re-attesting every tool result against `target_conversation_id`.
pub fn fork_contexts(
    state: &AppState,
    request: &ForkRequest<'_>,
    contexts: &[ContextItem],
    mut new_id: impl FnMut(&str) -> String,
) -> Result<Vec<ContextItem>, String> {
    let cut = contexts
        .iter()
        .position(|context| context.id() == request.through_context_id)
        .ok_or_else(|| {
            format!(
                "分支起点 {} 不在源对话中",
                request.through_context_id
            )
        })?;

    // `modelTurnId` groups the items one model round emitted. It is a local
    // association, not a provider id, so it is remapped consistently rather
    // than carried over: the branch is a separate session and must not claim
    // the source's turn identities.
    let mut turn_ids = HashMap::<String, String>::new();
    let mut forked = Vec::with_capacity(cut + 1);

    for context in &contexts[..=cut] {
        let id = new_id("ctx");
        let remap = |turn: &Option<String>, turn_ids: &mut HashMap<String, String>| {
            turn.as_ref().map(|source| {
                turn_ids
                    .entry(source.clone())
                    .or_insert_with(|| new_turn_id())
                    .clone()
            })
        };

        forked.push(match context {
            ContextItem::System {
                content,
                local_only,
                hook_execution,
                created_at,
                ..
            } => ContextItem::System {
                id,
                content: content.clone(),
                local_only: *local_only,
                hook_execution: hook_execution.clone(),
                created_at: created_at.clone(),
            },
            ContextItem::User {
                content,
                images,
                created_at,
                ..
            } => ContextItem::User {
                id,
                content: content.clone(),
                images: images.clone(),
                created_at: created_at.clone(),
            },
            ContextItem::Assistant {
                content,
                round,
                model_turn_id,
                interrupted,
                created_at,
                ..
            } => ContextItem::Assistant {
                id,
                content: content.clone(),
                round: *round,
                model_turn_id: remap(model_turn_id, &mut turn_ids),
                interrupted: *interrupted,
                sources: Vec::new(),
                created_at: created_at.clone(),
            },
            ContextItem::Reasoning {
                content,
                form,
                round,
                model_turn_id,
                interrupted,
                duration_ms,
                tokens,
                replay,
                created_at,
                ..
            } => ContextItem::Reasoning {
                id,
                content: content.clone(),
                // Preserve the card's original form. A fork may use a different
                // model, but moving the card does not change the model that produced it.
                form: *form,
                round: *round,
                model_turn_id: remap(model_turn_id, &mut turn_ids),
                interrupted: *interrupted,
                // Preserve metadata so reasoning cards with only encrypted content remain
                // visible in the timeline after the fork.
                duration_ms: *duration_ms,
                tokens: *tokens,
                // The signed payload stays with the card: a fork on the same
                // model replays it exactly like the source conversation would.
                replay: replay.clone(),
                created_at: created_at.clone(),
            },
            ContextItem::Tool {
                tool_name,
                round,
                model_turn_id,
                requested_input,
                input,
                result,
                subagent,
                created_at,
                ..
            } => {
                // The copy is only persistable if the host vouches for it in the
                // destination conversation. This is the same attestation the
                // original execution produced, re-issued for the new owner.
                attest(
                    state,
                    request,
                    tool_name,
                    input,
                    requested_input.as_ref(),
                    result,
                    subagent.as_ref(),
                );
                // The token is bound to the conversation and the card id, both
                // of which the fork changes, so the original cannot be carried
                // over — it is re-issued for the copy in its new home.
                let attestation =
                    state.attest_tool_context(&crate::tool_attestation::AttestationSubject {
                        conversation_id: request.target_conversation_id,
                        context_id: &id,
                        tool_name,
                        input,
                        requested_input: requested_input.as_ref(),
                        result,
                        subagent: subagent.as_ref(),
                    });
                ContextItem::Tool {
                    id,
                    tool_name: tool_name.clone(),
                    round: *round,
                    model_turn_id: remap(model_turn_id, &mut turn_ids),
                    requested_input: requested_input.clone(),
                    input: input.clone(),
                    result: result.clone(),
                    subagent: subagent.clone(),
                    attestation,
                    created_at: created_at.clone(),
                }
            }
        });
    }

    Ok(forked)
}

fn new_turn_id() -> String {
    format!("turn_{}", uuid::Uuid::new_v4().simple())
}

fn attest(
    state: &AppState,
    request: &ForkRequest<'_>,
    tool_name: &str,
    input: &crate::model::JsonObject,
    requested_input: Option<&crate::model::JsonObject>,
    result: &ToolResult,
    subagent: Option<&SubagentRunRecord>,
) {
    let execution = ToolExecutionRequest {
        conversation_id: request.target_conversation_id.to_owned(),
        workspace_path: request.workspace_path.to_owned(),
        tool_name: tool_name.to_owned(),
        input: input.clone(),
    };
    match subagent {
        Some(subagent) => {
            state.record_context_subagent_receipt(&execution, result, requested_input, subagent)
        }
        None => state.record_context_receipt(&execution, result, requested_input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::default_document, storage::validate_save_transition};

    fn sequential_ids() -> impl FnMut(&str) -> String {
        let mut next = 0usize;
        move |prefix| {
            next += 1;
            format!("{prefix}_forked_{next}")
        }
    }

    #[test]
    fn fork_copies_history_through_the_cut_point_with_fresh_ids() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let workspace = &document.workspaces[0];
        let source = &workspace.conversations[0];
        let state = AppState::default();

        let forked = fork_contexts(
            &state,
            &ForkRequest {
                workspace_path: &workspace.path,
                target_conversation_id: "conv_branch",
                through_context_id: "ctx_welcome_tool",
            },
            &source.contexts,
            sequential_ids(),
        )
        .unwrap();

        // Everything through the tool call is copied; the assistant reply after
        // it is not.
        assert_eq!(forked.len(), 4);
        assert!(forked
            .iter()
            .all(|context| context.id().starts_with("ctx_forked_")));
        assert!(matches!(forked[3], ContextItem::Tool { .. }));

        // Content is preserved verbatim, only identity changes.
        let (ContextItem::User { content, .. }, ContextItem::User { content: source_content, .. }) =
            (&forked[1], &source.contexts[1])
        else {
            panic!("seed index 1 must be a user context");
        };
        assert_eq!(content, source_content);
    }

    #[test]
    fn forked_tool_results_are_attested_for_the_branch_and_persist() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut next = previous.clone();
        let workspace_path = previous.workspaces[0].path.clone();
        let state = AppState::default();

        let forked = fork_contexts(
            &state,
            &ForkRequest {
                workspace_path: &workspace_path,
                target_conversation_id: "conv_branch",
                through_context_id: "ctx_welcome_tool",
            },
            &previous.workspaces[0].conversations[0].contexts,
            sequential_ids(),
        )
        .unwrap();

        let mut branch = previous.workspaces[0].conversations[0].clone();
        branch.id = "conv_branch".into();
        branch.contexts = forked;
        branch.branches.clear();
        next.workspaces[0].conversations.push(branch);

        // The copied tool result carries a host receipt bound to the branch, so
        // the whole branch saves. Without `fork_contexts` this is exactly the
        // transition `tool_context_and_receipt_cannot_move_between_conversations`
        // rejects.
        validate_save_transition(&previous, &next, &state).expect("forked branch must persist");
    }

    #[test]
    fn fork_target_may_already_exist_when_the_renderer_flushed_it_empty() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let mut target = document.workspaces[0].conversations[0].clone();
        target.id = "conv_branch".into();
        target.contexts.clear();
        target.branches.clear();

        // This is exactly the state `branchFromUserContext` puts on disk before
        // it calls the host: the branch exists and is empty. Rejecting it made
        // every fork past the first message fail deterministically.
        validate_fork_target(Some(&target), "conv_branch")
            .expect("an empty flushed target is the expected state, not a duplicate");
        // A target the host has never seen is equally fine.
        validate_fork_target(None, "conv_branch").expect("a missing target must be accepted");
    }

    #[test]
    fn fork_target_that_already_holds_history_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let mut target = document.workspaces[0].conversations[0].clone();
        target.id = "conv_branch".into();
        assert!(
            !target.contexts.is_empty(),
            "fixture must carry history for this guard to mean anything"
        );

        // The copy replaces `contexts` wholesale, so a target with history would
        // silently lose it. That is what the guard is actually for.
        let error = validate_fork_target(Some(&target), "conv_branch")
            .expect_err("a target holding history must be rejected");
        assert!(error.contains("conv_branch"), "error must name the target: {error}");
    }

    #[test]
    fn fork_rejects_a_cut_point_outside_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let workspace = &document.workspaces[0];
        let state = AppState::default();

        let error = fork_contexts(
            &state,
            &ForkRequest {
                workspace_path: &workspace.path,
                target_conversation_id: "conv_branch",
                through_context_id: "ctx_not_in_this_conversation",
            },
            &workspace.conversations[0].contexts,
            sequential_ids(),
        )
        .unwrap_err();

        assert!(error.contains("ctx_not_in_this_conversation"));
    }

    #[test]
    fn fork_remaps_model_turn_ids_consistently_without_reusing_the_source_identity() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let workspace = &document.workspaces[0];
        let state = AppState::default();
        let mut contexts = workspace.conversations[0].contexts.clone();
        for context in &mut contexts {
            if let ContextItem::Reasoning { model_turn_id, .. }
            | ContextItem::Tool { model_turn_id, .. } = context
            {
                *model_turn_id = Some("turn_source".into());
            }
        }

        let forked = fork_contexts(
            &state,
            &ForkRequest {
                workspace_path: &workspace.path,
                target_conversation_id: "conv_branch",
                through_context_id: "ctx_welcome_tool",
            },
            &contexts,
            sequential_ids(),
        )
        .unwrap();

        let turn_ids = forked
            .iter()
            .filter_map(|context| match context {
                ContextItem::Reasoning { model_turn_id, .. }
                | ContextItem::Tool { model_turn_id, .. } => model_turn_id.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(turn_ids.len(), 2);
        // One round stays one round in the branch, under a new local identity.
        assert_eq!(turn_ids[0], turn_ids[1]);
        assert_ne!(turn_ids[0], "turn_source");
    }
}

