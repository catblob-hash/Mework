//! The conversation's history: one ordered, append-only record the host keeps of
//! everything that happened to a conversation.
//!
//! The timeline is the user's to edit. It says what the conversation holds now,
//! not what happened: a card can be rewritten, deleted, or written by hand as if
//! the model had said it. So whenever the host needs to know what actually
//! happened — whether a result reached the model, what an agent last said, which
//! script an approved dispatch ran — it reads this record instead, and so does the
//! history pane.
//!
//! One record rather than one ledger per direction, because the questions cut
//! across directions. What a call ran with is neither what the model sent (a
//! `PreToolUse` hook may rewrite it, and that rewrite is intended) nor what the
//! next request replays (the timeline's projection of it). Whether an agent's last
//! reply was its final one depends on what came after it — another request, or a
//! `Stop` hook that sent it back to work. Only a single sequence can answer "what
//! came after", so every kind of entry shares one `seq` per conversation:
//!
//! - `request` — a payload put on the wire ([`RequestRecorder::record`]),
//!   immediately before the submit, so a request the transport then loses still
//!   leaves one. Bodies are content-addressed: every round re-sends the whole
//!   history, and storing each verbatim would grow with the square of the
//!   conversation.
//! - `response` — what the model sent back, as the host parsed it, with the usage
//!   the provider reported ([`RequestRecorder::record_response`]). Written before
//!   the host acts on it and waited for.
//! - `hook` — what each hook decided: a block, a halt, a permission, a rewritten
//!   input, an added context ([`record_hooks`]). Written before the decision takes
//!   effect and waited for.
//! - `tool` — the input a call actually runs with, once hooks and approval have had
//!   their say ([`record_tool_call`]). Written before it runs and waited for: this,
//!   not the model's words, is what a later check compares against.
//! - `result` — what a call returned, as the model is handed it
//!   ([`record_tool_result`]).
//! - `edit` / `run` — changes to the trunk timeline, recorded by the store itself in
//!   the transaction that made them: what the user changed, and what a run settled.
//!
//! A child run — a subagent, a workflow step — shares its parent's conversation id
//! and so its store row, and its entries are told apart by `owner`, the name the
//! child was spawned under (see [`owner_of`]).
//!
//! Recording never fails a run and never adds latency to a provider call: entries
//! go to a single background writer through a bounded queue. The entries that
//! recovery reads as evidence are the exception to "never wait" — the request
//! thread waits for them to reach disk, briefly, so a process that dies right after
//! the host acted still left a record of what it acted on.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{sync_channel, SyncSender, TrySendError},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use serde_json::{json, Map, Value};

use crate::{
    aisdk::ParsedModelResponse,
    api::{ToolCall, ToolExecution},
    conversation_store::{
        store_for, ConversationStore, HistoryEntryRecord, HistoryPartRecord, HistoryRequestRecord,
        HISTORY_EVIDENCE_MAX_BYTES, HISTORY_PART_MAX_BYTES,
    },
    hooks::{HookExecution, HookPermissionDecision},
    model::RunModelRequest,
};

/// Part kinds, in the order a request carries them. These are the strings the
/// store's `CHECK` constraint accepts and the renderer switches on.
pub(crate) const PART_SYSTEM: &str = "system";
pub(crate) const PART_SYSTEM_DYNAMIC: &str = "systemDynamic";
pub(crate) const PART_TOOLS: &str = "tools";
pub(crate) const PART_MESSAGE: &str = "message";

/// Request types. `model` is an ordinary conversation round; the other two are
/// the one-shot requests the host mints for the native web tools, which consume
/// tokens and belong to no round.
pub(crate) const KIND_MODEL: &str = "model";
pub(crate) const KIND_SEARCH: &str = "search";
pub(crate) const KIND_FETCH: &str = "fetch";

/// Entry kinds, as the store's `CHECK` constraint names them.
pub(crate) const ENTRY_REQUEST: &str = "request";
pub(crate) const ENTRY_RESPONSE: &str = "response";
pub(crate) const ENTRY_HOOK: &str = "hook";
pub(crate) const ENTRY_TOOL: &str = "tool";
pub(crate) const ENTRY_RESULT: &str = "result";

/// Queue depth. Deep enough that a burst of parallel rounds never reaches it,
/// shallow enough that a stalled writer cannot pin an unbounded amount of
/// projected history in memory.
const QUEUE_DEPTH: usize = 64;

/// How long a thread waits for an evidence entry to reach disk before acting
/// anyway. The wait is what makes the record evidence: a reply the host delivered,
/// a call it dispatched, a hook decision it honoured, is on disk before any of that
/// happens. Past this the writer is stalled, and holding a run hostage to it would
/// trade the run for its audit trail.
const EVIDENCE_WRITE_WAIT: Duration = Duration::from_secs(5);

/// One outgoing request, in the shape the record stores it.
///
/// Held behind an [`Arc`] from the moment it is built: a retry re-sends the same
/// bytes, and cloning a whole projected history per attempt would cost more than
/// the request it is recording.
pub(crate) struct RequestAudit {
    /// The `StepRequest` minus the separately-stored parts and minus every
    /// credential-bearing field.
    pub envelope: Value,
    /// `(part kind, body)` in wire order: system prompt, its per-step tail, the
    /// tool specs, then one entry per projected message.
    pub parts: Vec<(&'static str, Value)>,
}

impl RequestAudit {
    fn string_field(&self, key: &str) -> String {
        self.envelope
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

/// One queued write, already detached from the thread that asked for it. The
/// queue is a FIFO with a single writer, so a request's row is always inserted
/// before the response that names it.
enum Job {
    Request {
        app_data_path: String,
        conversation_id: String,
        owner: Option<String>,
        kind: &'static str,
        request_id: String,
        round: i64,
        attempt: i64,
        provider_name: String,
        audit: Arc<RequestAudit>,
        /// Identity of the entry this job will write. The writer fills it in
        /// after the insert; the response that answers it reads the same cell.
        ///
        /// An entry needs an identity because nothing else names one. `seq` is
        /// assigned by the store, and `(request_id, round, attempt)` is not
        /// unique: a `pause_turn` continuation or an output resume builds a fresh
        /// recorder for the same round, whose attempts start over at one.
        seq: Arc<OnceLock<i64>>,
    },
    Entry {
        app_data_path: String,
        record: HistoryEntryRecord,
        /// The request this entry answers, filled by the job before it. Still
        /// empty means that request never became an entry — a full queue or a
        /// failed write — and the entry is written without the link.
        answers: Option<Arc<OnceLock<i64>>>,
        /// Tells a waiting thread the entry is on disk.
        written: Option<SyncSender<()>>,
    },
}

/// Recorder for one built step. Every request path must obtain one before
/// sending, the same way the gateway's token ledger makes each caller choose an
/// owner; a new send path that skips it is a send nobody can account for.
pub(crate) struct RequestRecorder {
    app_data_path: String,
    conversation_id: String,
    /// Whose entries these are: the child agent's address for a subagent or a
    /// workflow step, `None` for the session's own trunk.
    owner: Option<String>,
    kind: &'static str,
    request_id: String,
    round: i64,
    provider_name: String,
    audit: Arc<RequestAudit>,
    /// Attempts of *this* step. A transient failure re-posts the same payload,
    /// and a record that showed one entry for three sends would under-report the
    /// traffic it exists to explain.
    attempts: AtomicU32,
    /// Identity of the entry the last [`Self::record`] queued, so the response
    /// that comes back is linked to the attempt that earned it. A retry replaces
    /// it: only the attempt that produced a response has one, and that is always
    /// the newest.
    last_request: Mutex<Option<Arc<OnceLock<i64>>>>,
}

/// Whose entries a run's traffic is, or `None` when it belongs to nobody's and
/// must not be recorded at all.
///
/// `Some(None)` is the conversation's own trunk. `Some(Some(name))` is the child
/// agent of that name: a child shares its parent's conversation and therefore its
/// store row, so the name it was spawned under is the only thing separating the
/// two. The name and not the call id — the provider's call id never survives into
/// the timeline, which stores a hash of it, so entries keyed by one could not be
/// matched to an agent from the renderer at all, while the name is the very
/// address `task_wait` reaches the child by.
///
/// `None` is a child that nothing names: the host-minted native web request,
/// whose template raises the depth without taking a name. It is dropped rather
/// than filed under the trunk, which is read as "what this session did" — a
/// one-shot minted inside a child would be a false answer to that.
///
/// A request may also carry its owner explicitly. A workflow step is named `ws1`,
/// `ws2`… inside its run's private pool, so the name alone would file the first
/// steps of every run in the conversation under one owner; its driver sets
/// [`RunModelRequest::history_owner`] to a run-scoped address instead, and that
/// wins over the name.
pub(crate) fn owner_of(request: &RunModelRequest) -> Option<Option<String>> {
    if let Some(owner) = request.history_owner.as_deref() {
        return Some(Some(owner.to_owned()));
    }
    match request.subagent_name.as_deref() {
        Some(name) => Some(Some(name.to_owned())),
        None if request.subagent_depth == 0 => Some(None),
        None => None,
    }
}

impl RequestRecorder {
    /// Returns `None` when there is nowhere to record: a bare test `AppState` with
    /// no data directory, or a request nobody owns — see [`owner_of`].
    pub(crate) fn for_request(
        request: &RunModelRequest,
        kind: &'static str,
        round: usize,
        audit: RequestAudit,
    ) -> Option<Self> {
        if request.app_data_path.is_empty() {
            return None;
        }
        let owner = owner_of(request)?;
        Some(Self {
            app_data_path: request.app_data_path.clone(),
            conversation_id: request.conversation_id.clone(),
            owner,
            kind,
            request_id: request.request_id.clone(),
            round: round as i64,
            provider_name: request.provider.name.clone(),
            audit: Arc::new(audit),
            attempts: AtomicU32::new(0),
            last_request: Mutex::new(None),
        })
    }

    /// Records one attempt at putting this payload on the wire. Called
    /// immediately before the submit, so an entry exists for a request that the
    /// transport then loses — which is exactly the case the record is read for.
    pub(crate) fn record(&self) {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        let seq = Arc::new(OnceLock::new());
        let job = Job::Request {
            app_data_path: self.app_data_path.clone(),
            conversation_id: self.conversation_id.clone(),
            owner: self.owner.clone(),
            kind: self.kind,
            request_id: self.request_id.clone(),
            round: self.round,
            attempt: attempt as i64,
            provider_name: self.provider_name.clone(),
            audit: Arc::clone(&self.audit),
            seq: Arc::clone(&seq),
        };
        let queued = writer().try_send(job).is_ok();
        if !queued {
            eprintln!("历史记录积压，本次请求未记入历史记录");
        }
        if let Ok(mut slot) = self.last_request.lock() {
            // A dropped request has no entry, and the previous attempt's entry is
            // not this attempt's: leaving the old identity in place would link
            // this response to the wrong send.
            *slot = queued.then_some(seq);
        }
    }

    /// Records what the model sent back for the last [`Self::record`], with the
    /// usage the provider reported for it, and returns once it is on disk (or the
    /// wait gives up). Called once per parsed response, before the host acts on it.
    ///
    /// The usage lives here and not on the request because it is a property of
    /// the answer: a request that got none has no usage, and a failed attempt's
    /// entry stays without one, which is the truth about it.
    pub(crate) fn record_response(&self, parsed: &ParsedModelResponse) {
        let attempt = i64::from(self.attempts.load(Ordering::Relaxed));
        let answers = self
            .last_request
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(Arc::clone));
        let mut detail = Map::new();
        detail.insert("attempt".into(), json!(attempt));
        if let Some(model) = &parsed.model {
            detail.insert("modelId".into(), json!(model));
        }
        if let Some(reason) = &parsed.stop_reason {
            detail.insert("finishReason".into(), json!(reason));
        }
        if let Some(reason) = &parsed.raw_stop_reason {
            detail.insert("rawFinishReason".into(), json!(reason));
        }
        let usage = usage_value(&parsed.usage);
        if !usage.is_empty() {
            detail.insert("usage".into(), Value::Object(usage));
        }
        let record = HistoryEntryRecord {
            conversation_id: self.conversation_id.clone(),
            kind: ENTRY_RESPONSE,
            owner: self.owner.clone(),
            request_id: Some(self.request_id.clone()),
            round: Some(self.round),
            call_id: None,
            answers: None,
            detail: Value::Object(detail),
            body: Some(response_message(parsed).to_string()),
            body_cap: HISTORY_EVIDENCE_MAX_BYTES,
        };
        submit(&self.app_data_path, record, answers, true);
    }
}

/// Provider usage as the record stores it: only the counters the provider
/// disclosed, so an absent one reads as absent rather than as zero. A count too
/// large to represent reads as undisclosed rather than as a wrapped-around number.
fn usage_value(usage: &crate::model::ModelUsage) -> Map<String, Value> {
    let mut fields = Map::new();
    for (key, value) in [
        ("inputTokens", usage.input_tokens),
        ("cachedInputTokens", usage.cached_input_tokens),
        ("outputTokens", usage.output_tokens),
    ] {
        if let Some(count) = value.and_then(|value| i64::try_from(value).ok()) {
            fields.insert(key.into(), json!(count));
        }
    }
    fields
}

/// A parsed response as one assistant message, in the AI SDK shape request parts
/// store messages in, so the same readers ([`recorded_tool_parts`],
/// [`recorded_message_text`]) serve both. It is the host's parse, not the
/// provider's frames: the text the host settled, the reasoning it showed, and the
/// calls it went on to handle, in that order.
pub(crate) fn response_message(parsed: &ParsedModelResponse) -> Value {
    let mut content = Vec::new();
    for reasoning in &parsed.reasoning {
        if !reasoning.trim().is_empty() {
            content.push(json!({"type": "reasoning", "text": reasoning}));
        }
    }
    if !parsed.text.is_empty() {
        content.push(json!({"type": "text", "text": parsed.text}));
    }
    for call in &parsed.calls {
        content.push(json!({
            "type": "tool-call",
            "toolCallId": call.id,
            "toolName": call.name,
            "input": Value::Object(call.input.clone()),
        }));
    }
    json!({"role": "assistant", "content": content})
}

/// Where a run's entries go, or `None` when it has no record: no data directory,
/// or nobody owns its traffic.
fn run_scope(request: &RunModelRequest) -> Option<(String, Option<String>)> {
    if request.app_data_path.is_empty() {
        return None;
    }
    Some((request.app_data_path.clone(), owner_of(request)?))
}

fn permission_label(decision: HookPermissionDecision) -> &'static str {
    match decision {
        HookPermissionDecision::Allow => "allow",
        HookPermissionDecision::Ask => "ask",
        HookPermissionDecision::Defer => "defer",
        HookPermissionDecision::Deny => "deny",
    }
}

/// Records what the hooks of one event decided, and returns once it is on disk.
/// Called as soon as the hooks have run, before the host honours any of it.
///
/// A hook's rewrite of a tool input, its block, its halt and the context it adds
/// are all things the host is meant to act on. They go in the record as what
/// happened, so a later reader can tell a rewrite the host carried out from a
/// change nobody authorised — and can tell a reply a `Stop` hook sent back to work
/// from a final one, even when the process died before the continuation left.
///
/// `call_id` names the call a tool hook ran for, and `matcher` is what the hook's
/// matcher was tested against (the tool name, or the `SessionStart` source).
pub(crate) fn record_hooks(
    request: &RunModelRequest,
    round: usize,
    call_id: Option<&str>,
    matcher: Option<&str>,
    executions: &[HookExecution],
) {
    let Some((app_data_path, owner)) = run_scope(request) else {
        return;
    };
    for execution in executions {
        let decision = &execution.decision;
        let mut detail = Map::new();
        detail.insert("event".into(), json!(crate::hooks::event_label(execution.event)));
        detail.insert("hookId".into(), json!(execution.id));
        detail.insert("hookName".into(), json!(execution.name));
        if let Some(matcher) = matcher {
            detail.insert("matcher".into(), json!(matcher));
        }
        detail.insert("success".into(), json!(execution.result.success));
        detail.insert("blocked".into(), json!(decision.blocked));
        detail.insert("halted".into(), json!(decision.halt));
        detail.insert("interrupted".into(), json!(decision.interrupt));
        if let Some(permission) = decision.permission_decision {
            detail.insert("permission".into(), json!(permission_label(permission)));
        }
        detail.insert("rewroteInput".into(), json!(decision.updated_input.is_some()));
        detail.insert(
            "addedContext".into(),
            json!(decision.additional_context.is_some()),
        );
        let mut body = Map::new();
        body.insert("output".into(), json!(execution.result.output));
        if let Some(reason) = &decision.reason {
            body.insert("reason".into(), json!(reason));
        }
        if let Some(message) = &decision.system_message {
            body.insert("systemMessage".into(), json!(message));
        }
        if let Some(context) = &decision.additional_context {
            body.insert("additionalContext".into(), json!(context));
        }
        if let Some(updated) = &decision.updated_input {
            body.insert("updatedInput".into(), updated.clone());
        }
        let record = HistoryEntryRecord {
            conversation_id: request.conversation_id.clone(),
            kind: ENTRY_HOOK,
            owner: owner.clone(),
            request_id: Some(request.request_id.clone()),
            round: Some(round as i64),
            call_id: call_id.map(str::to_owned),
            answers: None,
            detail: Value::Object(detail),
            body: Some(Value::Object(body).to_string()),
            body_cap: HISTORY_EVIDENCE_MAX_BYTES,
        };
        submit(&app_data_path, record, None, true);
    }
}

/// Records the input `call` actually runs with — after every hook rewrite, before
/// it runs — and returns once it is on disk.
///
/// This is the anchor a later check compares against, not the input the model
/// sent: a `PreToolUse` or `PermissionRequest` hook that rewrites an input is doing
/// what it was configured to do, and a check against the model's own words would
/// report that as tampering. `requested` is what the model sent, kept beside the
/// effective input when the two differ so the rewrite itself stays visible.
/// `denied` is set when the call was refused instead of run.
pub(crate) fn record_tool_call(
    request: &RunModelRequest,
    round: usize,
    call: &ToolCall,
    requested: &crate::model::JsonObject,
    denied: Option<&str>,
) {
    let Some((app_data_path, owner)) = run_scope(request) else {
        return;
    };
    let rewritten = requested != &call.input;
    let mut detail = Map::new();
    detail.insert("name".into(), json!(call.name));
    detail.insert("rewritten".into(), json!(rewritten));
    if let Some(reason) = denied {
        detail.insert("denied".into(), json!(reason));
    }
    let mut body = Map::new();
    body.insert("input".into(), Value::Object(call.input.clone()));
    if rewritten {
        body.insert("requestedInput".into(), Value::Object(requested.clone()));
    }
    let record = HistoryEntryRecord {
        conversation_id: request.conversation_id.clone(),
        kind: ENTRY_TOOL,
        owner,
        request_id: Some(request.request_id.clone()),
        round: Some(round as i64),
        call_id: Some(call.id.clone()),
        answers: None,
        detail: Value::Object(detail),
        body: Some(Value::Object(body).to_string()),
        body_cap: HISTORY_EVIDENCE_MAX_BYTES,
    };
    submit(&app_data_path, record, None, true);
}

/// Records what a call returned, as the model will be handed it: after any
/// `PostToolUse` hook has had its say. Not waited for — nothing acts on it before
/// the next request carries it out, and that request is recorded too.
pub(crate) fn record_tool_result(request: &RunModelRequest, round: usize, execution: &ToolExecution) {
    let Some((app_data_path, owner)) = run_scope(request) else {
        return;
    };
    let mut detail = Map::new();
    detail.insert("name".into(), json!(execution.call.name));
    detail.insert("success".into(), json!(execution.result.success));
    if !execution.result.images.is_empty() {
        detail.insert("images".into(), json!(execution.result.images.len()));
    }
    let record = HistoryEntryRecord {
        conversation_id: request.conversation_id.clone(),
        kind: ENTRY_RESULT,
        owner,
        request_id: Some(request.request_id.clone()),
        round: Some(round as i64),
        call_id: Some(execution.call.id.clone()),
        answers: None,
        detail: Value::Object(detail),
        body: Some(json!({"output": execution.result.output}).to_string()),
        body_cap: HISTORY_PART_MAX_BYTES,
    };
    submit(&app_data_path, record, None, false);
}

/// Hands one entry to the writer. An evidence entry (`wait`) returns once it is on
/// disk or the wait gives up; the rest return at once. A full queue is written from
/// this thread instead of dropping the entry: unlike a request's projected history,
/// an entry is small, and it may be the one recovery reads.
fn submit(
    app_data_path: &str,
    record: HistoryEntryRecord,
    answers: Option<Arc<OnceLock<i64>>>,
    wait: bool,
) {
    let (written, done) = if wait {
        let (sender, receiver) = sync_channel(1);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let job = Job::Entry {
        app_data_path: app_data_path.to_owned(),
        record,
        answers,
        written,
    };
    match writer().try_send(job) {
        Ok(()) => {
            if let Some(done) = done {
                if done.recv_timeout(EVIDENCE_WRITE_WAIT).is_err() {
                    eprintln!("历史记录写入迟迟未完成，宿主先行处理（写入仍在排队）");
                }
            }
        }
        Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
            if let Job::Entry {
                app_data_path,
                record,
                answers,
                ..
            } = job
            {
                write_entry(&app_data_path, record, answers.as_deref());
            }
        }
    }
}

fn writer() -> &'static SyncSender<Job> {
    static WRITER: OnceLock<SyncSender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (sender, receiver) = sync_channel::<Job>(QUEUE_DEPTH);
        let spawned = thread::Builder::new()
            .name("mework-history".into())
            .spawn(move || {
                // One writer, so entries land in the order they were handed over
                // without the request thread ever waiting on the store's lock.
                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Request {
                            app_data_path,
                            conversation_id,
                            owner,
                            kind,
                            request_id,
                            round,
                            attempt,
                            provider_name,
                            audit,
                            seq,
                        } => write_request(
                            &app_data_path,
                            &conversation_id,
                            owner.as_deref(),
                            kind,
                            &request_id,
                            round,
                            attempt,
                            &provider_name,
                            &audit,
                            &seq,
                        ),
                        Job::Entry {
                            app_data_path,
                            record,
                            answers,
                            written,
                        } => {
                            write_entry(&app_data_path, record, answers.as_deref());
                            if let Some(written) = written {
                                // The waiting thread may have stopped waiting.
                                let _ = written.try_send(());
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            eprintln!("历史记录写入线程无法启动，本次会话不会留下历史记录");
        }
        sender
    })
}

/// Wire role of a message part, as the store keeps it beside the hash.
fn message_role(body: &Value) -> Option<String> {
    body.get("role").and_then(Value::as_str).map(str::to_owned)
}

/// Who put this message in the history.
///
/// The wire role alone cannot answer it. The Anthropic wire format carries tool
/// results inside a `user` message, but that message is the host handing the
/// model back the calls the model itself made — counting it as something the
/// person typed would report an edit on every round of every tool loop. A `user`
/// message is only a person's when it carries something other than tool results.
fn message_author(body: &Value) -> &'static str {
    if body.get("role").and_then(Value::as_str) != Some("user") {
        return "model";
    }
    let only_tool_results = body
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get("type").and_then(Value::as_str) == Some("tool-result"))
        });
    if only_tool_results {
        "model"
    } else {
        "user"
    }
}

#[allow(clippy::too_many_arguments)]
fn write_request(
    app_data_path: &str,
    conversation_id: &str,
    owner: Option<&str>,
    kind: &'static str,
    request_id: &str,
    round: i64,
    attempt: i64,
    provider_name: &str,
    audit: &RequestAudit,
    seq_slot: &OnceLock<i64>,
) {
    let store = match history_store(app_data_path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("对话库不可用，本次请求未记入历史记录：{error}");
            return;
        }
    };
    // Serializing the projected history is O(history); it happens here rather
    // than on the request thread precisely because it is not free.
    let parts = audit
        .parts
        .iter()
        .map(|(kind, body)| {
            let text = match body {
                Value::String(text) => text.clone(),
                other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
            };
            // Only a message has a role or an author; the prompt and the tool
            // specs are the host's own framing, which no one edits between
            // rounds and which the delta therefore never looks at.
            let (role, author) = if *kind == PART_MESSAGE {
                (message_role(body), Some(message_author(body).to_owned()))
            } else {
                (None, None)
            };
            HistoryPartRecord {
                kind: (*kind).to_owned(),
                role,
                author,
                body: text,
            }
        })
        .collect();
    let record = HistoryRequestRecord {
        conversation_id: conversation_id.to_owned(),
        owner: owner.map(str::to_owned),
        kind: kind.to_owned(),
        request_id: request_id.to_owned(),
        round,
        attempt,
        provider_name: provider_name.to_owned(),
        family: audit.string_field("family"),
        model_id: audit.string_field("modelId"),
        envelope: serde_json::to_string(&audit.envelope).unwrap_or_else(|_| "{}".to_owned()),
        parts,
    };
    match store.record_history_request(&record) {
        Ok(Some(seq)) => {
            // The entry now exists and has a number; the response queued behind
            // this job can name it.
            let _ = seq_slot.set(seq);
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("对话 {conversation_id} 的请求未能记入历史记录：{error}");
        }
    }
}

fn write_entry(
    app_data_path: &str,
    mut record: HistoryEntryRecord,
    answers: Option<&OnceLock<i64>>,
) {
    // Empty when the request's entry was dropped or failed; the entry is still
    // written, because it may be the one recovery reads.
    record.answers = answers.and_then(|cell| cell.get().copied());
    let store = match history_store(app_data_path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("对话库不可用，本条{}未记入历史记录：{error}", record.kind);
            return;
        }
    };
    if let Err(error) = store.record_history_entry(&record) {
        eprintln!(
            "对话 {} 的{}未能记入历史记录：{error}",
            record.conversation_id, record.kind
        );
    }
}

// ---- Reading the record back as evidence ----------------------------------
//
// The timeline is the user's to edit; the record is what actually happened.
// Bodies are messages exactly as recorded: compact JSON, in the AI SDK shape the
// sidecar takes.

/// The store the record lives in, for a reader outside the writer thread.
pub(crate) fn history_store(app_data_path: &str) -> Result<Arc<ConversationStore>, String> {
    if app_data_path.is_empty() {
        return Err("no app data directory".into());
    }
    store_for(&Path::new(app_data_path).join("document.v1.json"))
}

/// One tool exchange part of a recorded message.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordedToolPart {
    Call {
        id: String,
        name: String,
        input: Value,
    },
    Result {
        id: String,
        name: String,
        text: String,
    },
}

/// The tool calls and tool results one recorded message carried, in order. A body that is not a
/// message with a parts array has none.
pub(crate) fn recorded_tool_parts(body: &str) -> Vec<RecordedToolPart> {
    let Ok(message) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let Some(parts) = message.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            let id = part.get("toolCallId")?.as_str()?.to_owned();
            let name = part.get("toolName")?.as_str()?.to_owned();
            match part.get("type")?.as_str()? {
                "tool-call" => Some(RecordedToolPart::Call {
                    id,
                    name,
                    input: part.get("input").cloned().unwrap_or(Value::Null),
                }),
                "tool-result" => Some(RecordedToolPart::Result {
                    id,
                    name,
                    text: tool_output_text(part.get("output")),
                }),
                _ => None,
            }
        })
        .collect()
}

/// The visible text of one recorded message: its string content, or its text parts joined.
pub(crate) fn recorded_message_text(body: &str) -> String {
    let Ok(message) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The input a recorded `tool` entry says its call ran with.
pub(crate) fn recorded_tool_input(body: &str) -> Option<Value> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("input")
        .cloned()
}

/// A tool result's output as text: `{type: "text", value}` and its `error-text` twin carry it
/// directly, `content` carries text items among media, and anything else is its JSON.
fn tool_output_text(output: Option<&Value>) -> String {
    let Some(output) = output else {
        return String::new();
    };
    match output.get("value") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => output.to_string(),
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recorded_messages_read_back_their_tool_parts_and_text() {
        let call = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "dispatching"},
                {"type": "tool-call", "toolCallId": "c1", "toolName": "workflow",
                 "input": {"name": "sweep", "script": "return 1"}}
            ]
        })
        .to_string();
        assert_eq!(
            recorded_tool_parts(&call),
            vec![RecordedToolPart::Call {
                id: "c1".into(),
                name: "workflow".into(),
                input: json!({"name": "sweep", "script": "return 1"}),
            }]
        );
        assert_eq!(recorded_message_text(&call), "dispatching");

        let results = json!({
            "role": "tool",
            "content": [
                {"type": "tool-result", "toolCallId": "c1", "toolName": "workflow",
                 "output": {"type": "text", "value": "ok"}},
                {"type": "tool-result", "toolCallId": "c2", "toolName": "read",
                 "output": {"type": "content", "value": [{"type": "text", "text": "a"}, {"type": "media"}]}},
                {"type": "tool-result", "toolCallId": "c3", "toolName": "structured",
                 "output": {"type": "json", "value": {"k": 1}}}
            ]
        })
        .to_string();
        let texts = recorded_tool_parts(&results)
            .into_iter()
            .map(|part| match part {
                RecordedToolPart::Result { text, .. } => text,
                RecordedToolPart::Call { .. } => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["ok", "a", "{\"k\":1}"]);

        assert!(recorded_tool_parts("not json").is_empty());
        assert_eq!(recorded_message_text(&json!({"role": "user", "content": "hi"}).to_string()), "hi");
        assert_eq!(
            recorded_tool_input(&json!({"input": {"script": "x"}}).to_string()),
            Some(json!({"script": "x"}))
        );
    }
}
