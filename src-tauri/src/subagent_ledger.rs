//! Host ledger of top-level subagents whose results have not durably reached the model.
//!
//! `<app_data_dir>/subagents/<conversationId>/agent-<name>.json` is written when `agent_spawn`
//! starts a top-level child, and removed once that child's result — or the restart notice that
//! stands in for it — is in the timeline and the write that put it there is confirmed.
//!
//! # Why a ledger
//!
//! A subagent's worker is a thread of this process. When the process exits, whether by a crash or
//! by the user quitting while a child still runs, the worker and its undelivered result go with
//! it, and nothing else on disk says so: the child's card holds at most a recovery checkpoint,
//! which reads `interrupted` whether the child was stopped by the user and reported, or simply
//! died. A file here at startup is therefore exactly an agent the model is still owed an answer
//! about. Startup claims it and the next round boundary tells the model, the way an interrupted
//! workflow run is reported (see `workflow_store::sweep_interrupted_runs`).
//!
//! # What was lost
//!
//! An entry says only that the model is owed an answer. What the agent got as far as saying comes
//! from the conversation's history (`crate::history`), which holds every response the agent
//! received before the host acted on it, and everything that came after: another request, a `Stop`
//! hook sending the agent back to work. An agent whose last response there was its final reply had
//! finished: only the delivery died with the process. That reply is put back — into the agent's
//! card at startup, and to the model at the conversation's next round boundary, as the completed
//! result it is — and nothing wakes the model for it. Any other agent died mid-work, and the model
//! is told so.
//!
//! # Why files
//!
//! Workflow runs already keep their recovery state as small files beside the conversation store;
//! these follow the same layout and the same startup sweep. A write failure only warns: the child
//! still runs, and the worst outcome is the silence this ledger exists to end.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::conversation_store::{ConversationStore, HistoryFilter, HistoryRecord};
use crate::model::SubagentRunRecord;
use crate::history::{
    recorded_message_text, recorded_tool_parts, RecordedToolPart, ENTRY_HOOK, ENTRY_REQUEST,
    ENTRY_RESPONSE,
};
use crate::workflow_store::{document_conversation_ids, validate_path_component, write_private};

const LEDGER_DIRECTORY: &str = "subagents";
/// File-name prefix. Agent names are legal Windows device names (`con`, `aux`), which no
/// extension makes writable; the prefix keeps every name a plain file.
const FILE_PREFIX: &str = "agent-";
const FILE_SUFFIX: &str = ".json";
/// The tool a schema-bound child hands its result back through.
const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";

/// A subagent the previous process was still owed an answer about.
#[derive(Clone, Debug, PartialEq)]
pub struct LostSubagent {
    pub conversation_id: String,
    pub name: String,
    /// The provider call id of its `agent_spawn`, which finds the spawn card when the child
    /// settled before any record of it was written. Absent in entries older than this field.
    pub spawn_call_id: Option<String>,
    /// Its run record as it stood at spawn, without the opening contexts: the execution-mode
    /// receipt, bindings and output schema a record written from nothing would lack.
    pub record: Option<SubagentRunRecord>,
    /// Its final reply, when the history holds one. Set at startup by
    /// [`recover_final_reply`]; such an agent finished and is owed a delivery, not a notice.
    pub recovered: Option<RecoveredReply>,
}

/// A final reply the history holds for an agent whose delivery was lost.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveredReply {
    /// The child run and round that produced it, which the reply's own card id derives from.
    pub request_id: String,
    pub round: usize,
    /// The round's text, joined the way the host joins a round's steps.
    pub text: String,
    /// The value a schema-bound agent handed back, validated against its spawn-time schema.
    pub structured_output: Option<Value>,
}

fn entry_path(app_data_path: &Path, conversation_id: &str, name: &str) -> Option<PathBuf> {
    validate_path_component("会话 id", conversation_id).ok()?;
    crate::agents::validate_agent_name(name).ok()?;
    Some(
        app_data_path
            .join(LEDGER_DIRECTORY)
            .join(conversation_id)
            .join(format!("{FILE_PREFIX}{name}{FILE_SUFFIX}")),
    )
}

/// Records that `name` is running in `conversation_id` and owes the model a result, with the call
/// that spawned it and its record as it stands now. The record's contexts are dropped: a forked
/// child opens with the whole parent conversation, and recovery never needs it.
pub fn record_spawned(
    app_data_path: &Path,
    conversation_id: &str,
    name: &str,
    spawn_call_id: &str,
    record: &SubagentRunRecord,
) {
    let Some(path) = entry_path(app_data_path, conversation_id, name) else {
        return;
    };
    let mut skeleton = record.clone();
    skeleton.contexts.clear();
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| {
            let body = json!({
                "name": name,
                "spawnedAt": chrono::Utc::now().to_rfc3339(),
                "callId": spawn_call_id,
                "record": skeleton,
            });
            write_private(&path, body.to_string().as_bytes())
        });
    if let Err(error) = written {
        eprintln!("子代理 {name} 的存活账目写入失败（进程若在它交付前退出，将无法补发通知）：{error}");
    }
}

/// Settles an entry: the model has the agent's result, or a notice in its place, durably.
pub fn settle(app_data_path: &Path, conversation_id: &str, name: &str) {
    let Some(path) = entry_path(app_data_path, conversation_id, name) else {
        return;
    };
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("子代理 {name} 的存活账目无法销账：{error}"),
    }
}

/// Claims every entry left by the previous process in a live conversation, and removes the
/// entries of conversations that no longer exist.
///
/// Entries are only read here. They are settled after their notice is delivered and confirmed, so
/// a crash before that delivers it again on the next startup.
pub fn sweep_lost_subagents(
    app_data_path: &Path,
    document: &crate::model::AppDocument,
) -> Vec<LostSubagent> {
    let live = document_conversation_ids(document);
    let Ok(conversations) = fs::read_dir(app_data_path.join(LEDGER_DIRECTORY)) else {
        return Vec::new();
    };
    let mut lost = Vec::new();
    for conversation in conversations.flatten() {
        let conversation_path = conversation.path();
        let Some(conversation_id) = conversation.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !conversation_path.is_dir() {
            continue;
        }
        if !live.contains(conversation_id.as_str()) {
            if let Err(error) = fs::remove_dir_all(&conversation_path) {
                eprintln!("无法清理已删除会话 {conversation_id} 的子代理账目：{error}");
            }
            continue;
        }
        let Ok(entries) = fs::read_dir(&conversation_path) else {
            continue;
        };
        let mut found = entries
            .flatten()
            .filter_map(|entry| {
                let file_name = entry.file_name();
                let name = file_name
                    .to_str()?
                    .strip_prefix(FILE_PREFIX)?
                    .strip_suffix(FILE_SUFFIX)?
                    .to_owned();
                crate::agents::validate_agent_name(&name).ok()?;
                // An unreadable body still names an agent the model is owed an answer about;
                // it only loses what could put a finished one back.
                let body = fs::read(entry.path())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .unwrap_or(Value::Null);
                Some(LostSubagent {
                    conversation_id: conversation_id.clone(),
                    spawn_call_id: body
                        .get("callId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned),
                    record: body
                        .get("record")
                        .and_then(|record| serde_json::from_value(record.clone()).ok()),
                    recovered: None,
                    name,
                })
            })
            .collect::<Vec<_>>();
        found.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        lost.extend(found);
    }
    lost
}

/// How many `Stop` continuations the host grants one run before it ends the run instead
/// (`api::run_model`'s `stop_continuations` limit).
const STOP_CONTINUATION_LIMIT: usize = 3;

/// Whether the `Stop` hooks among `records` sent the agent back to work. One firing records one
/// entry per hook; the host continues when one of them blocked and none halted.
fn stop_hooks_continued(records: &[&HistoryRecord]) -> bool {
    let stops = records
        .iter()
        .filter(|record| record.kind == ENTRY_HOOK && record.detail_str("event") == Some("Stop"))
        .collect::<Vec<_>>();
    !stops.iter().any(|record| record.detail_flag("halted"))
        && stops
            .iter()
            .any(|record| record.detail_flag("blocked") && !record.detail_flag("halted"))
}

/// The final reply the history holds for `lost`, if its last response was one.
///
/// A response is final when the host ended the child's run on it: a reply with no tool calls that
/// neither paused (`pause_turn`) nor ran out of output tokens — or, for a schema-bound child, a
/// round whose `structured_output` call the spawn-time schema accepts — and after which nothing
/// took the agent further. What came after is what tells a reply the run ended on from one it went
/// on past: another request of the same agent (a nudge, a continuation), or a `Stop` hook that
/// blocked the stop and so sent the agent back to work. The hook's decision is on disk before the
/// host acts on it, so a continuation the process died before sending still counts — unless the
/// run had already used every continuation it is granted, when the host ends it instead.
///
/// The text is the whole round's, its steps' non-empty texts joined by a newline, as the host
/// joins a round that paused or was continued.
pub fn recover_final_reply(
    store: &ConversationStore,
    lost: &LostSubagent,
) -> Option<RecoveredReply> {
    let records = store
        .history_records(
            &lost.conversation_id,
            HistoryFilter {
                owner: Some(&lost.name),
                kinds: &[ENTRY_REQUEST, ENTRY_RESPONSE, ENTRY_HOOK],
                ..HistoryFilter::default()
            },
        )
        .ok()?;
    let at = records
        .iter()
        .rposition(|record| record.kind == ENTRY_RESPONSE)?;
    let last = &records[at];
    if last.truncated {
        return None;
    }
    let after = records[at + 1..].iter().collect::<Vec<_>>();
    if after.iter().any(|record| record.kind == ENTRY_REQUEST) {
        return None;
    }
    if stop_hooks_continued(&after) {
        let granted = records[..at]
            .iter()
            .filter(|record| record.request_id == last.request_id)
            .fold(Vec::<Vec<&HistoryRecord>>::new(), |mut rounds, record| {
                match rounds.last_mut() {
                    Some(round) if round[0].round == record.round => round.push(record),
                    _ => rounds.push(vec![record]),
                }
                rounds
            })
            .iter()
            .filter(|round| stop_hooks_continued(round))
            .count();
        if granted < STOP_CONTINUATION_LIMIT {
            return None;
        }
    }
    let raw = last.detail_str("rawFinishReason");
    let finish = last.detail_str("finishReason");
    if raw == Some("pause_turn")
        || finish == Some("pause_turn")
        || raw == Some("max_tokens")
        || finish == Some("length")
    {
        return None;
    }
    let body = last.body.as_deref()?;
    let calls = recorded_tool_parts(body)
        .into_iter()
        .filter_map(|part| match part {
            RecordedToolPart::Call { name, input, .. } => Some((name, input)),
            RecordedToolPart::Result { .. } => None,
        })
        .collect::<Vec<_>>();
    let schema = lost
        .record
        .as_ref()
        .and_then(|record| record.output_schema.as_ref());
    let structured_output = match schema {
        None if calls.is_empty() => None,
        None => return None,
        Some(document) => {
            let schema = crate::orchestration::compile_output_schema(document).ok()?;
            // The host keeps the last accepted value of the round.
            let accepted = calls
                .iter()
                .filter(|(name, _)| name == STRUCTURED_OUTPUT_TOOL)
                .filter_map(|(_, input)| {
                    crate::orchestration::parse_structured_output(input.as_object()?, &schema).ok()
                })
                .last()?;
            Some(accepted)
        }
    };
    let text = records
        .iter()
        .filter(|record| {
            record.kind == ENTRY_RESPONSE
                && record.request_id == last.request_id
                && record.round == last.round
        })
        .filter_map(|record| record.body.as_deref())
        .map(recorded_message_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some(RecoveredReply {
        request_id: last.request_id.clone()?,
        round: usize::try_from(last.round?).ok()?,
        text,
        structured_output,
    })
}

/// The newest text agent `name` wrote, as the history received it — its final words before the
/// process died, whether or not any request ever carried them anywhere.
pub fn last_received_text(
    store: &ConversationStore,
    conversation_id: &str,
    name: &str,
) -> Option<String> {
    store
        .history_records(
            conversation_id,
            HistoryFilter {
                owner: Some(name),
                kinds: &[ENTRY_RESPONSE],
                ..HistoryFilter::default()
            },
        )
        .ok()?
        .iter()
        .rev()
        .filter(|response| !response.truncated)
        .filter_map(|response| response.body.as_deref())
        .map(recorded_message_text)
        .find(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_with(conversation_ids: &[&str]) -> crate::model::AppDocument {
        let mut document = crate::catalog::default_document();
        let template = document.workspaces[0].conversations[0].clone();
        document.workspaces[0].conversations = conversation_ids
            .iter()
            .map(|id| {
                let mut conversation = template.clone();
                conversation.id = (*id).into();
                conversation
            })
            .collect();
        document
    }

    fn record(name: &str) -> SubagentRunRecord {
        SubagentRunRecord {
            kind: Default::default(),
            name: Some(name.into()),
            label: None,
            inherits_model_memory: false,
            fork_model_binding: None,
            agent_definition: None,
            execution_mode_receipt: "receipt".into(),
            task: "review".into(),
            status: crate::model::SubagentRunStatus::Interrupted,
            contexts: vec![crate::model::ContextItem::User {
                id: "ctx_task".into(),
                content: "review".into(),
                images: Vec::new(),
                files: Vec::new(),
                created_at: "2026-09-30T00:00:00Z".into(),
            }],
            updates: Vec::new(),
            structured_output: None,
            output_schema: None,
            usage: Default::default(),
        }
    }

    fn spawn(directory: &Path, conversation_id: &str, name: &str) {
        record_spawned(
            directory,
            conversation_id,
            name,
            &format!("call-{name}"),
            &record(name),
        );
    }

    /// An entry survives until it is settled, so a process that dies first leaves it for the
    /// next startup to claim; a conversation deleted meanwhile takes its entries with it.
    #[test]
    fn an_unsettled_entry_is_claimed_at_startup_and_a_settled_one_is_not() {
        let directory = tempfile::tempdir().unwrap();
        spawn(directory.path(), "convlive", "reviewer");
        spawn(directory.path(), "convlive", "con");
        spawn(directory.path(), "convlive", "finished");
        settle(directory.path(), "convlive", "finished");
        spawn(directory.path(), "convgone", "orphan");

        let lost = sweep_lost_subagents(directory.path(), &document_with(&["convlive"]));
        let claimed = |name: &str| LostSubagent {
            conversation_id: "convlive".into(),
            name: name.into(),
            spawn_call_id: Some(format!("call-{name}")),
            // The opening contexts stay out of the file.
            record: Some(SubagentRunRecord {
                contexts: Vec::new(),
                ..record(name)
            }),
            recovered: None,
        };
        assert_eq!(lost, vec![claimed("con"), claimed("reviewer")]);
        assert!(
            !directory.path().join(LEDGER_DIRECTORY).join("convgone").exists(),
            "已删除会话的账目随之清理"
        );
        // Claiming does not settle: a second startup before delivery claims the same entries.
        assert_eq!(
            sweep_lost_subagents(directory.path(), &document_with(&["convlive"])).len(),
            2
        );
        settle(directory.path(), "convlive", "reviewer");
        settle(directory.path(), "convlive", "con");
        assert!(sweep_lost_subagents(directory.path(), &document_with(&["convlive"])).is_empty());
    }

    /// An entry written before it carried the spawn call and record still names a lost agent.
    #[test]
    fn an_entry_without_call_or_record_is_still_claimed() {
        let directory = tempfile::tempdir().unwrap();
        let path = entry_path(directory.path(), "convlive", "reviewer").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{"name":"reviewer","spawnedAt":"2026-09-29T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            sweep_lost_subagents(directory.path(), &document_with(&["convlive"])),
            vec![LostSubagent {
                conversation_id: "convlive".into(),
                name: "reviewer".into(),
                spawn_call_id: None,
                record: None,
                recovered: None,
            }]
        );
    }

    #[test]
    fn identifiers_that_cannot_be_path_components_are_never_written() {
        let directory = tempfile::tempdir().unwrap();
        spawn(directory.path(), "../escape", "reviewer");
        spawn(directory.path(), "convlive", "../escape");
        assert!(!directory.path().join(LEDGER_DIRECTORY).exists());
        assert!(!directory.path().join("escape").exists());
    }
}
