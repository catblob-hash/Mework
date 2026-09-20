//! Forensic ledger of the requests this host puts on the wire.
//!
//! The conversation timeline records what a run *settled*: the cards the user
//! ended up with. It cannot answer the other question — what did the client
//! actually send? Those are not the same thing. Ephemeral memory blocks are
//! projected into a request and never persisted as cards; memory exchanges and
//! interrupted fragments are persisted as cards and never projected into a
//! request; and one settled run issues one request *per round*, each carrying a
//! longer history than the last. Replaying the timeline through today's
//! projection would answer "what would we send now", which is a different
//! question again once the model, the provider or the prompt profile has moved.
//!
//! So the payload is recorded where it is built, at the moment it goes out:
//!
//! - [`WireAudit`] is assembled alongside the [`StepRequest`] in
//!   [`crate::aisdk::step::build_step_request`], from the messages *before*
//!   image hydration. A hydrated payload carries megabytes of base64 per image;
//!   the pre-hydration form carries the same structure with the attachment
//!   placeholder the timeline itself stores.
//! - Credentials never enter the audit copy at all. The envelope is serialized
//!   from a [`StepRequest`] whose credential-bearing fields are still empty —
//!   `headers` and `agent` are assigned by the caller *after* the build, and
//!   `apiKey` is dropped explicitly — so redaction is a property of when the
//!   copy is taken rather than a filter that has to keep up with the struct.
//! - [`WireRecorder::record`] hands the copy to a single background writer.
//!   Recording must never add latency to a provider call or fail a run, so the
//!   queue is bounded and a full queue drops the row with a diagnostic rather
//!   than blocking the request thread.
//! - [`WireRecorder::record_usage`] catches up with the same row once the
//!   response has parsed. What a request cost is not known when it goes out, so
//!   the row is written first and the provider's counters are attached after.
//!
//! Bodies are content-addressed by the store: every round re-sends the whole
//! history, so storing each message verbatim per request would grow with the
//! square of the conversation. See `conversation_store::WIRE_LEDGER_SCHEMA`.
//!
//! One conversation holds more than one ledger. A child run — a subagent, a
//! workflow step — shares its parent's conversation id, so its requests are
//! recorded against the same store row, and told apart by the name the child was
//! spawned under. Reading them together would answer "what did this session
//! send" with traffic the session never sent.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex, OnceLock,
    },
    thread,
};

use serde_json::Value;

use crate::{
    conversation_store::{store_for, WireRecordedPart, WireRequestRecord, WireUsage},
    model::RunModelRequest,
};

/// Part kinds, in the order a request carries them. These are the strings the
/// store's `CHECK` constraint accepts and the renderer switches on.
pub(crate) const PART_SYSTEM: &str = "system";
pub(crate) const PART_SYSTEM_DYNAMIC: &str = "systemDynamic";
pub(crate) const PART_TOOLS: &str = "tools";
pub(crate) const PART_MESSAGE: &str = "message";

/// Request kinds. `model` is an ordinary conversation round; the other two are
/// the one-shot requests the host mints for the native web tools, which consume
/// tokens and belong to no round.
pub(crate) const KIND_MODEL: &str = "model";
pub(crate) const KIND_SEARCH: &str = "search";
pub(crate) const KIND_FETCH: &str = "fetch";

/// Queue depth. Deep enough that a burst of parallel rounds never reaches it,
/// shallow enough that a stalled writer cannot pin an unbounded amount of
/// projected history in memory.
const QUEUE_DEPTH: usize = 64;

/// One outgoing request, in the shape the ledger stores it.
///
/// Held behind an [`Arc`] from the moment it is built: a retry re-sends the same
/// bytes, and cloning a whole projected history per attempt would cost more than
/// the request it is recording.
pub(crate) struct WireAudit {
    /// The `StepRequest` minus the separately-stored parts and minus every
    /// credential-bearing field.
    pub envelope: Value,
    /// `(part kind, body)` in wire order: system prompt, its per-step tail, the
    /// tool specs, then one entry per projected message.
    pub parts: Vec<(&'static str, Value)>,
}

impl WireAudit {
    fn string_field(&self, key: &str) -> String {
        self.envelope
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

/// One queued item, already detached from the request thread.
///
/// The two variants are ordered by the queue itself: it is a FIFO with a single
/// writer, so a request's row is always inserted before the usage that names it
/// is handled.
enum Entry {
    Request {
        app_data_path: String,
        conversation_id: String,
        owner: Option<String>,
        kind: &'static str,
        request_id: String,
        round: i64,
        attempt: i64,
        provider_name: String,
        audit: Arc<WireAudit>,
        /// Identity of the row this entry will write. The writer fills it in
        /// after the insert; a usage entry later reads the same cell.
        ///
        /// A row needs an identity because nothing else names one. `seq` is
        /// assigned by the store, and `(request_id, round, attempt)` is not
        /// unique: a `pause_turn` continuation or an output resume builds a
        /// fresh recorder for the same round, whose attempts start over at one.
        seq: Arc<OnceLock<i64>>,
    },
    Usage {
        app_data_path: String,
        conversation_id: String,
        /// Filled by the request entry that came before this one. Still empty
        /// means that request never became a row — a full queue or a failed
        /// write — so this usage has nowhere to land and is dropped.
        seq: Arc<OnceLock<i64>>,
        usage: WireUsage,
    },
}

/// Ledger handle for one built step. Every request path must obtain one before
/// sending, the same way the gateway's token ledger makes each caller choose an
/// owner; a new send path that skips it is a send nobody can audit.
pub(crate) struct WireRecorder {
    app_data_path: String,
    conversation_id: String,
    /// Which of the conversation's ledgers this recorder writes to: the name of
    /// the child agent for a subagent or a workflow step, `None` for the
    /// session's own trunk.
    owner: Option<String>,
    kind: &'static str,
    request_id: String,
    round: i64,
    provider_name: String,
    audit: Arc<WireAudit>,
    /// Attempts of *this* step. A transient failure re-posts the same payload,
    /// and a ledger that showed one row for three sends would under-report the
    /// traffic it exists to explain.
    attempts: AtomicU32,
    /// Identity of the row the last [`Self::record`] queued, so the usage that
    /// comes back can be attached to the attempt that earned it. A retry
    /// replaces it: only the attempt that produced a response has usage, and
    /// that is always the newest one.
    last_row: Mutex<Option<Arc<OnceLock<i64>>>>,
}

/// Which of a conversation's ledgers a request belongs to, or `None` when it
/// belongs to none of them and must not be recorded at all.
///
/// `Some(None)` is the conversation's own trunk. `Some(Some(name))` is the child
/// agent of that name: a child shares its parent's conversation and therefore its
/// store row, so the name it was spawned under is the only thing separating the
/// two. The name and not the call id — the provider's call id never survives into
/// the timeline, which stores a hash of it, so a ledger keyed by one could not be
/// matched to an agent from the renderer at all, while the name is the very
/// address `send_message` and `followup_task` reach the child by.
///
/// `None` is a child that nothing names: the host-minted native web request,
/// whose template raises the depth without taking a name. It is dropped rather
/// than filed under the trunk, which is read as "what this session sent" — a
/// one-shot minted inside a child would be a false answer to that.
///
/// A request may also carry its ledger explicitly. A workflow step is named
/// `ws1`, `ws2`… inside its run's private pool, so the name alone would file the
/// first steps of every run in the conversation under one ledger; its driver
/// sets [`RunModelRequest::wire_ledger_owner`] to a run-scoped address instead,
/// and that wins over the name.
pub(crate) fn ledger_owner(request: &RunModelRequest) -> Option<Option<String>> {
    if let Some(owner) = request.wire_ledger_owner.as_deref() {
        return Some(Some(owner.to_owned()));
    }
    match request.subagent_name.as_deref() {
        Some(name) => Some(Some(name.to_owned())),
        None if request.subagent_depth == 0 => Some(None),
        None => None,
    }
}

impl WireRecorder {
    /// Returns `None` when there is nowhere to record: a bare test `AppState`
    /// with no data directory, or a request no ledger owns — see
    /// [`ledger_owner`].
    pub(crate) fn for_request(
        request: &RunModelRequest,
        kind: &'static str,
        round: usize,
        audit: WireAudit,
    ) -> Option<Self> {
        if request.app_data_path.is_empty() {
            return None;
        }
        let owner = ledger_owner(request)?;
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
            last_row: Mutex::new(None),
        })
    }

    /// Records one attempt at putting this payload on the wire. Called
    /// immediately before the submit, so a row exists for a request that the
    /// transport then loses — which is exactly the case the ledger is read for.
    pub(crate) fn record(&self) {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        let seq = Arc::new(OnceLock::new());
        let entry = Entry::Request {
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
        let queued = writer().try_send(entry).is_ok();
        if !queued {
            eprintln!("请求账本积压，本次请求未记入历史面板");
        }
        if let Ok(mut slot) = self.last_row.lock() {
            // A dropped request has no row, and the previous attempt's row is
            // not this attempt's: leaving the old identity in place would file
            // this response's usage against the wrong send.
            *slot = queued.then_some(seq);
        }
    }

    /// Attaches what the provider reported for the response to the row the last
    /// [`Self::record`] wrote. Called once per parsed response, beside the
    /// gateway ledger, because the same thing is true of both: a failed attempt
    /// has no usage, and the numbers only exist once a response has parsed.
    pub(crate) fn record_usage(&self, usage: &crate::model::ModelUsage) {
        let usage = WireUsage {
            input_tokens: wire_token_count(usage.input_tokens),
            cached_input_tokens: wire_token_count(usage.cached_input_tokens),
            output_tokens: wire_token_count(usage.output_tokens),
        };
        if usage.is_empty() {
            // Nothing disclosed. A row of NULLs says the same thing and costs a
            // queue slot plus a write to say it.
            return;
        }
        let Some(seq) = self
            .last_row
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(Arc::clone))
        else {
            return;
        };
        let entry = Entry::Usage {
            app_data_path: self.app_data_path.clone(),
            conversation_id: self.conversation_id.clone(),
            seq,
            usage,
        };
        if writer().try_send(entry).is_err() {
            eprintln!("请求账本积压，本次请求的用量未记入历史面板");
        }
    }
}

/// Providers report counts that cannot be negative; the store holds signed
/// integers. A count too large to represent reads as undisclosed rather than as
/// a wrapped-around number the panel would draw.
fn wire_token_count(value: Option<u64>) -> Option<i64> {
    value.and_then(|value| i64::try_from(value).ok())
}

fn writer() -> &'static SyncSender<Entry> {
    static WRITER: OnceLock<SyncSender<Entry>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (sender, receiver) = sync_channel::<Entry>(QUEUE_DEPTH);
        let spawned = thread::Builder::new()
            .name("mework-wire-ledger".into())
            .spawn(move || {
                // One writer, so rows land in send order without the request
                // thread ever waiting on the store's connection mutex.
                while let Ok(entry) = receiver.recv() {
                    match entry {
                        Entry::Request {
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
                        Entry::Usage {
                            app_data_path,
                            conversation_id,
                            seq,
                            usage,
                        } => write_usage(&app_data_path, &conversation_id, &seq, &usage),
                    }
                }
            });
        if spawned.is_err() {
            eprintln!("请求账本写入线程无法启动，历史面板将不会记录本次会话的请求");
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
    audit: &WireAudit,
    seq_slot: &OnceLock<i64>,
) {
    let anchor = Path::new(app_data_path).join("document.v1.json");
    let store = match store_for(&anchor) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("对话库不可用，本次请求未记入历史面板：{error}");
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
            WireRecordedPart {
                kind: (*kind).to_owned(),
                role,
                author,
                body: text,
            }
        })
        .collect();
    let record = WireRequestRecord {
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
    match store.record_wire_request(&record) {
        Ok(Some(seq)) => {
            // The row now exists and has a number; a usage entry queued behind
            // this one can name it.
            let _ = seq_slot.set(seq);
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("对话 {conversation_id} 的请求账本写入失败：{error}");
        }
    }
}

fn write_usage(app_data_path: &str, conversation_id: &str, seq: &OnceLock<i64>, usage: &WireUsage) {
    let Some(seq) = seq.get() else {
        // The request this usage belongs to never became a row, so there is
        // nothing to attach it to. Silent: the drop was already reported where
        // it happened.
        return;
    };
    let anchor = Path::new(app_data_path).join("document.v1.json");
    let store = match store_for(&anchor) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("对话库不可用，本次请求的用量未记入历史面板：{error}");
            return;
        }
    };
    if let Err(error) = store.record_wire_usage(conversation_id, *seq, usage) {
        eprintln!("对话 {conversation_id} 的请求账本用量写入失败：{error}");
    }
}
