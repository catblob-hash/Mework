//! Durable conversation storage in a process-local SQLite database. The host is the sole writer.
//!
//! Each message occupies one JSON-backed row, updated with row-scoped `UPSERT`s. `run_model`
//! persists every canonical context directly, without renderer round trips or debounce.
//! Streaming prose uses `streaming` status and becomes `settled` when finalized; startup recovery
//! marks stale streaming rows as `interrupted`.
//!
//! User-editable non-conversation configuration remains in the `document.v1.json` anchor file.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::model::{
    ContextItem, Conversation, ConversationBranch, ConversationForkOrigin,
    ConversationHandoffOrigin, ConversationSettings, ConversationWorktree, FileAttachment, ImageAttachment, QueuedMessage, RunTarget,
    UserAbortedTaskRecord,
};
use crate::model::AttachedWorkspace;
use crate::memory_pool::{MemoryPool, PoolKey, PoolKind};

/// Database file name, stored beside the anchor file.
pub const DATABASE_FILE_NAME: &str = "conversations.v1.sqlite3";

/// `PRAGMA user_version`. Every upgrade so far is additive and in place, so a
/// released user's history survives; only a version from the future is quarantined
/// and rebuilt.
///
/// The stamp records what a store was last reconciled against and nothing more.
/// [`ConversationStore::ensure_schema`] repairs by comparing the tables and columns
/// the store actually holds against the shape this build compiles against, on every
/// open. A stamp is not evidence: a store carrying this number can still be missing
/// a column, because a build whose upgrade steps differed, an upgrade that died
/// between two `ALTER`s, and a hand-edited database all leave the number claiming
/// more than the schema delivers. Trusting it is what once let a request ledger
/// without its `owner` column sit behind a current stamp and fail every read and
/// write of it for the remaining life of the store.
pub const STORE_VERSION: i32 = 19;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingForkStart {
    pub workspace_id: String,
    pub conversation_id: String,
    pub prompt_context_id: String,
}

const FORK_START_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pending_fork_start (
    conversation_id TEXT PRIMARY KEY REFERENCES conversation(id) ON DELETE CASCADE,
    prompt_context_id TEXT NOT NULL
) STRICT;";

/// What the local helper model wrote about tool cards, one line per card and
/// kind: `tool_explanation` describes a shell command (or titles a subagent),
/// `tool_error_explanation` says why a failed call failed. A failed command
/// has both. Kept beside the card rather than in it: the card's payload is
/// attested, and a line arriving after the card was saved must not touch it.
const TOOL_EXPLANATION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tool_explanation (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    context_id      TEXT NOT NULL,
    text            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, context_id)
) STRICT;
CREATE TABLE IF NOT EXISTS tool_error_explanation (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    context_id      TEXT NOT NULL,
    text            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, context_id)
) STRICT;";

/// One plan document per conversation. A `plan` write replaces the whole
/// document, so there is no history here; the timeline keeps the calls.
const PLAN_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS conversation_plan (
    conversation_id TEXT PRIMARY KEY REFERENCES conversation(id) ON DELETE CASCADE,
    markdown        TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('draft','approved','rejected')),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;";

/// One row per answered fork request, for the source conversation's task bar.
/// The model never reads this table: `fork` returns before the user decides.
/// A decision only means anything beside the conversation that raised it, so it
/// dies with that conversation; the child it created does not.
const FORK_DECISION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS fork_decision (
    fork_id                TEXT PRIMARY KEY,
    source_conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    workspace_id           TEXT NOT NULL,
    title                  TEXT NOT NULL,
    prompt                 TEXT NOT NULL,
    requested_at           TEXT NOT NULL,
    decided_at             TEXT NOT NULL,
    approved               INTEGER NOT NULL,
    child_conversation_id  TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS fork_decision_source_idx ON fork_decision (source_conversation_id, decided_at);";

/// Saved message queues, reusable as the opening history of a conversation or a
/// subagent role. Templates are global rather than conversation-scoped, so
/// neither table references `conversation` — deleting the conversation a
/// template was captured from must not take the template with it.
///
/// The bodies live here, in the host's own database, for the same reason
/// [`crate::conversation_fork`] copies host-side: a template carries tool cards,
/// and a tool result is only persistable when the host can vouch that this
/// application really executed it. Storing template bodies in the
/// renderer-submitted document would let a forged result enter a conversation
/// through the apply path. Here the renderer only ever names a template.
const TEMPLATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS conversation_template (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    order_key  REAL NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS template_context (
    template_id TEXT NOT NULL REFERENCES conversation_template (id) ON DELETE CASCADE,
    id          TEXT NOT NULL,
    order_key   REAL NOT NULL,
    data        TEXT NOT NULL,
    PRIMARY KEY (template_id, id)
) STRICT;

CREATE INDEX IF NOT EXISTS template_context_order_idx ON template_context (template_id, order_key);";

/// The conversation's history: one ordered, append-only record of everything the
/// host saw happen to it — every request it put on the wire, every response that
/// came back, every hook decision, every tool call as it actually ran and what it
/// returned, and every change to the trunk timeline, whether the user made it or a
/// run settled it. See `crate::history` for what each kind means and when it is
/// written.
///
/// One `seq` per conversation across every kind and every owner, because the
/// questions the record is read for are about order: whether anything followed an
/// agent's last reply, which input a call ran with after the hooks had spoken,
/// what the user changed between two requests. Children — subagents, workflow
/// steps — run under their parent's conversation id and are told apart by
/// `owner`; the trunk's own entries have none.
///
/// Bodies are content-addressed in `history_blob`: every round re-sends the whole
/// history, so keeping each request's messages verbatim would grow with the square
/// of the conversation, while hashing a body once and naming it from each entry
/// that carried it grows with it linearly. The address is a hash within one
/// conversation, so a body dies with its conversation (`ON DELETE CASCADE`) without
/// any reference counting across conversations.
///
/// Nothing here is pruned. The record is the host's evidence of what happened —
/// the timeline is the user's to edit, the record is not — and recovery checks
/// deliveries, approvals and final replies against it, so a retention window would
/// erase exactly what it is read for.
///
/// - `history_part` names, in wire order, the parts a `request` carried.
/// - `history_op` is the delta an `edit` or `run` applied to the trunk, kept as a
///   chain rather than a snapshot per entry for the same reason bodies are
///   addressed: snapshots grow with the square of the conversation.
/// - `history_head` materializes the trunk as of the newest trunk entry, so a new
///   one only has to diff against one table instead of replaying the whole chain.
///   It holds each row's body verbatim instead of a hash: a digest would have to
///   stay stable across toolchain versions to avoid inventing a replacement for
///   every row at once.
const HISTORY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS history_entry (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('request', 'response', 'hook', 'tool', 'result', 'edit', 'run')),
    -- NULL for the conversation's own trunk; the child's address otherwise: an
    -- agent's name, or a workflow step's `<run>/ws<N>`.
    owner           TEXT,
    -- The run this entry belongs to; NULL for an edit.
    request_id      TEXT,
    round           INTEGER,
    -- The provider call id a tool entry, a result or a tool hook is about.
    call_id         TEXT,
    -- The request a response answers; NULL when that request was never written.
    answers         INTEGER,
    -- Small, kind-specific metadata as a JSON object.
    detail          TEXT NOT NULL DEFAULT '{}',
    -- The entry's body in `history_blob`: a request's envelope, a response's
    -- message, a hook's decision, a call's input, a result's output. NULL for a
    -- trunk change, whose bodies are its ops'.
    hash            TEXT,
    PRIMARY KEY (conversation_id, seq)
) STRICT;

CREATE INDEX IF NOT EXISTS history_entry_owner_idx ON history_entry (conversation_id, owner, kind, seq);
CREATE INDEX IF NOT EXISTS history_entry_call_idx ON history_entry (conversation_id, call_id);

CREATE TABLE IF NOT EXISTS history_blob (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    hash            TEXT NOT NULL,
    body            TEXT NOT NULL,
    truncated       INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, hash)
) STRICT;

CREATE TABLE IF NOT EXISTS history_part (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    ordinal         INTEGER NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('system', 'systemDynamic', 'tools', 'message')),
    hash            TEXT NOT NULL,
    -- Wire role of a `message` part, NULL for the other kinds. Kept so the
    -- next request's delta can tell a rewritten message from a deleted one
    -- without reading a single body back.
    role            TEXT,
    PRIMARY KEY (conversation_id, seq, ordinal)
) STRICT;

CREATE TABLE IF NOT EXISTS history_op (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    ordinal         INTEGER NOT NULL,
    op              TEXT NOT NULL CHECK (op IN ('remove', 'insert', 'replace')),
    context_id      TEXT NOT NULL,
    -- Final index of an 'insert'; NULL otherwise.
    position        INTEGER,
    -- Row body for 'insert' and 'replace', in `history_blob`; NULL for 'remove'.
    hash            TEXT,
    PRIMARY KEY (conversation_id, seq, ordinal)
) STRICT;

CREATE INDEX IF NOT EXISTS history_op_context_idx ON history_op (conversation_id, context_id, seq);

CREATE TABLE IF NOT EXISTS history_head (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    context_id      TEXT NOT NULL,
    data            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, position)
) STRICT;";

/// Tables the history replaced. A store that still has any of them is migrated
/// into the history on open and then loses them; see [`migrate_legacy_history`].
const LEGACY_HISTORY_TABLES: [&str; 7] = [
    "wire_request",
    "wire_request_part",
    "wire_blob",
    "wire_response",
    "timeline_event",
    "timeline_op",
    "timeline_head",
];

/// Columns older builds added to the legacy tables after they shipped. The
/// migration reads them, so a legacy table from before one of them is brought up
/// to shape first — only when the table is still there.
const LEGACY_COLUMNS: &[(&str, &str, &str)] = &[
    ("wire_request", "input_tokens", "INTEGER"),
    ("wire_request", "cached_input_tokens", "INTEGER"),
    ("wire_request", "output_tokens", "INTEGER"),
    ("wire_request", "messages_added", "INTEGER"),
    ("wire_request", "messages_removed", "INTEGER"),
    ("wire_request", "owner", "TEXT"),
    ("wire_request_part", "role", "TEXT"),
];

/// Largest body a request part, a tool result or a trunk row is stored whole at.
/// One message can carry an entire file, and the record is an account of what
/// happened rather than a second copy of the workspace: past this the marker
/// stands in for the rest.
pub(crate) const HISTORY_PART_MAX_BYTES: usize = 256 * 1024;

/// Largest body an evidence entry is stored whole at: a response, a hook
/// decision, a call's input, a request's envelope. These are bounded by the
/// model's output rather than by what the workspace holds, and they are read back
/// as evidence — a `workflow` call carries a script of up to
/// `workflow_core::MAX_SCRIPT_BYTES`, whose digest is what a resume is checked
/// against — so the part cap would cut exactly the bodies recovery reads.
pub(crate) const HISTORY_EVIDENCE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Appended to a body in place of the bytes the cap dropped. It is part of the
/// stored body, so the hash covers it too: what reads back is what was hashed.
const HISTORY_TRUNCATION_MARKER: &str = "…（历史记录正文已截断）";

/// Provider-reported usage for one response. Every field is optional: providers
/// disclose different subsets, and an absent counter must read as absent rather
/// than as zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
}

impl HistoryUsage {
    /// True when the provider disclosed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.cached_input_tokens.is_none()
            && self.output_tokens.is_none()
    }

    /// The usage a `detail` object carries under `usage`, when it carries any.
    fn from_detail(detail: &serde_json::Value) -> Option<Self> {
        let usage = serde_json::from_value::<Self>(detail.get("usage")?.clone()).ok()?;
        (!usage.is_empty()).then_some(usage)
    }
}

/// One entry without its body: what the history pane lists.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub seq: i64,
    pub created_at: String,
    /// `request`, `response`, `hook`, `tool`, `result`, `edit` or `run`.
    pub kind: String,
    /// The child agent this entry belongs to; absent on the trunk's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// For a response: the request it answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<i64>,
    /// Kind-specific metadata, as recorded.
    pub detail: serde_json::Value,
    /// What the provider reported. On a response it is its own; on a request it is
    /// the usage of the response that answered it, so the pane can put the cost on
    /// the send that incurred it. Absent where nothing came back or nothing was
    /// disclosed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<HistoryUsage>,
}

/// One part of a recorded request, resolved through its hash to the stored body.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPart {
    pub ordinal: i64,
    pub kind: String,
    pub hash: String,
    /// JSON text: one ModelMessage, a prompt string, or the tool-spec array.
    pub body: String,
    /// UTF-8 length of `body`, so the parts of a request sum to its own `bytes`.
    pub bytes: i64,
    pub truncated: bool,
}

/// One step of a trunk change, with the row as it read before and after.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOp {
    pub ordinal: i64,
    /// `insert`, `remove` or `replace`.
    pub op: String,
    pub context_id: String,
    /// Final index of an insert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// The row after this change; absent on a removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The row as the record last had it before this change; absent on a first
    /// insert, and on a row the record never saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// One entry in full, as the pane reads it when a row opens.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryDetail {
    pub entry: HistoryEntry,
    /// JSON text of the entry's body: a request's redacted envelope, a response's
    /// message, a hook's decision, a call's input, a result's output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub truncated: bool,
    /// The parts a request carried, in wire order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<HistoryPart>,
    /// The steps a trunk change applied, in replay order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<HistoryOp>,
}

/// One entry read back as evidence: its identity, its metadata and its body.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRecord {
    pub seq: i64,
    pub kind: String,
    pub request_id: Option<String>,
    pub round: Option<i64>,
    pub call_id: Option<String>,
    pub answers: Option<i64>,
    pub detail: serde_json::Value,
    pub body: Option<String>,
    /// The body was cut at the size cap and no longer parses.
    pub truncated: bool,
}

impl HistoryRecord {
    /// A string field of `detail`.
    pub fn detail_str(&self, key: &str) -> Option<&str> {
        self.detail.get(key).and_then(serde_json::Value::as_str)
    }

    /// A boolean field of `detail`, false when absent.
    pub fn detail_flag(&self, key: &str) -> bool {
        self.detail
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// Which entries an evidence read wants. `owner` picks whose (`None` is the
/// conversation's own trunk), `kinds` which kinds (empty is every kind), `call_id`
/// the call they are about, and `needle` keeps only bodies containing that literal
/// text.
#[derive(Clone, Copy, Debug, Default)]
pub struct HistoryFilter<'a> {
    pub owner: Option<&'a str>,
    pub kinds: &'a [&'a str],
    pub call_id: Option<&'a str>,
    pub needle: &'a str,
}

/// One part as the recorder hands it over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPartRecord {
    pub kind: String,
    /// Wire role of a message part; `None` for the prompt and tool-spec parts.
    pub role: Option<String>,
    /// Who put this message in the history: `"user"` or `"model"`. Derived by
    /// the recorder, which still has the message as a value, and used only to
    /// count this request's own additions — it is never stored.
    pub author: Option<String>,
    pub body: String,
}

/// One outgoing request as the recorder hands it over. Hashing, dedupe and
/// truncation are the store's business, so the recorder passes bodies verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRequestRecord {
    pub conversation_id: String,
    /// The child agent this request ran as, or `None` for the main session.
    pub owner: Option<String>,
    /// `model`, `search` or `fetch`.
    pub kind: String,
    pub request_id: String,
    pub round: i64,
    pub attempt: i64,
    pub provider_name: String,
    pub family: String,
    pub model_id: String,
    /// Already-redacted envelope, serialised.
    pub envelope: String,
    /// Parts in wire order.
    pub parts: Vec<HistoryPartRecord>,
}

/// Any other entry as the recorder hands it over: a response, a hook decision, a
/// call, a result.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntryRecord {
    pub conversation_id: String,
    pub kind: &'static str,
    pub owner: Option<String>,
    pub request_id: Option<String>,
    pub round: Option<i64>,
    pub call_id: Option<String>,
    pub answers: Option<i64>,
    pub detail: serde_json::Value,
    pub body: Option<String>,
    /// Bytes of `body` stored whole; past this it is cut and marked.
    pub body_cap: usize,
}

/// Lifecycle status for one context row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextStatus {
    /// Prose still being produced this turn. Startup recovery marks it `interrupted` if needed.
    Streaming,
    /// Finalized and immutable for this turn.
    Settled,
}

impl ContextStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Settled => "settled",
        }
    }
}

/// Default gap between ordering keys. Insertions use the midpoint; exhausted precision reindexes the segment.
const ORDER_STEP: f64 = 1.0;

/// One recorded change to the trunk, as the tests read it back.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrunkChangeSummary {
    pub seq: i64,
    /// `edit` or `run`.
    pub kind: String,
    /// `baseline` for the change a conversation's record starts from, `message`
    /// for an edit that only appended what the user typed, `edit` for any other
    /// edit, `run` for a settled run.
    pub source: String,
    pub request_id: Option<String>,
    pub inserted: i64,
    pub removed: i64,
    pub replaced: i64,
    pub row_count: i64,
    pub created_at: String,
}

/// Who changed the trunk. The first change a conversation records is always a
/// `baseline` edit whatever the caller's reason: replay starts from an empty trunk,
/// so the rows that already existed have to enter the chain somewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrunkChange {
    /// A run settled.
    Run,
    /// The renderer committed a change — the user's.
    Edit,
}

/// One trunk row as history sees it: an identity and a body, without the ordering
/// key, status and timestamps that do not change what the timeline said.
struct TimelineRow {
    id: String,
    data: String,
}

enum TimelineOp {
    Remove {
        context_id: String,
    },
    Insert {
        context_id: String,
        position: i64,
        data: String,
    },
    Replace {
        context_id: String,
        data: String,
    },
}

/// One saved template, without its body: what the picker draws a row from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTemplateSummary {
    pub id: String,
    pub name: String,
    pub message_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// Conversation activity within one UTC hour. See [`ConversationStore::activity_buckets`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityBucket {
    pub hour_start_ms: i64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub sessions: u64,
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS conversation (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    title        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    order_key    REAL NOT NULL,
    settings     TEXT NOT NULL,
    -- 隔离工作树记录的 JSON 数组，每个项目工作区至多一条；NULL = 都跑在工作区根上。
    -- 旧版只存一条（工作区 1 的）对象，读取时按数组的一项处理。
    -- 单独一列而不是塞进 settings：settings 会被预设与工作区快照整份复制，
    -- 而一条工作树路径复制给另一个对话就是错的。
    worktree     TEXT,
    -- 运行地点（RunTarget）的 JSON；NULL = 本机。
    -- 与 worktree 同理单独一列：一条 SSH 机器绑定复制给另一个对话就是错的。
    run_target   TEXT,
    -- NULL = top-level; no foreign key because deleting a parent re-parents children.
    parent_conversation_id TEXT,
    -- 最近一次套用的对话预设 ID；'' 或 NULL = 未命名草稿。
    -- 与 worktree 同理单独一列：settings 会被预设与工作区快照整份复制，
    -- 而预设身份跟着复制就会谎称另一个对话也套用过它。允许悬空。
    preset_id    TEXT,
    -- 最近一次套用的对话模板 ID；'' 或 NULL = 未套用模板。
    -- 与 preset_id 同理单独一列，同样是痕迹而非链接：允许悬空，模板删除后
    -- 这里保留 ID，解析不到就当作未套用。
    template_id  TEXT,
    -- JSON array of extra working directories; NULL = none. Its own column for
    -- the same reason as worktree: one grant must not be copied to another chat.
    additional_directories TEXT,
    -- JSON array of `{machine?, path}` — the same grants, each naming the machine
    -- its directory is on. Supersedes additional_directories, which is kept so a
    -- store written by an older build still reports what it granted.
    attached_workspaces TEXT,
    title_settled INTEGER NOT NULL DEFAULT 0,
    -- JSON `{conversationId, number}` naming the conversation a timeline fork
    -- was taken from; NULL = not a fork. A trace like preset_id: may dangle.
    fork_of TEXT,
    -- JSON `{conversationId, number}` naming the conversation an auto-compact
    -- continuation carries on; NULL = not one. A trace like fork_of: may dangle.
    handoff_of TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS conversation_workspace_order_idx ON conversation (workspace_id, order_key);

CREATE TABLE IF NOT EXISTS branch (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    fork_context_id TEXT NOT NULL,
    active          INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    order_key       REAL NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;

CREATE TABLE IF NOT EXISTS context (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    branch_id       TEXT,
    order_key       REAL NOT NULL,
    kind            TEXT NOT NULL,
    status          TEXT NOT NULL,
    round           INTEGER,
    model_turn_id   TEXT,
    data            TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id),
    CHECK (kind IN ('system', 'user', 'assistant', 'reasoning', 'tool')),
    CHECK (status IN ('streaming', 'settled'))
) STRICT;

CREATE INDEX IF NOT EXISTS context_order_idx ON context (conversation_id, branch_id, order_key);
CREATE INDEX IF NOT EXISTS context_status_idx ON context (status);

CREATE TABLE IF NOT EXISTS queued_message (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    order_key       REAL NOT NULL,
    content         TEXT NOT NULL,
    images          TEXT NOT NULL,
    -- JSON array of FileAttachment, like images. Added in v15; the default is
    -- what a row written before then, or by an older build, reads as.
    files           TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;

CREATE TABLE IF NOT EXISTS aborted_task (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    order_key       REAL NOT NULL,
    data            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;
"#;

/// Every creation statement the store is built from, in the order a fresh store
/// runs them. Repair walks the same list, so "what a new store gets" and "what an
/// old store is brought up to" cannot drift apart.
const SCHEMAS: [&str; 7] = [
    SCHEMA_SQL,
    FORK_START_SCHEMA,
    PLAN_SCHEMA,
    FORK_DECISION_SCHEMA,
    TEMPLATE_SCHEMA,
    HISTORY_SCHEMA,
    TOOL_EXPLANATION_SCHEMA,
];

/// Columns bolted onto a table after that table had already shipped, as
/// `(table, column, declaration)`.
///
/// The creation statements above carry these columns too, so a store built today
/// already has them and this list adds nothing; it exists for a store built by an
/// older release, where the table is present but the column is not. Listing them
/// separately is what makes repair idempotent: each one is added only when the
/// store is actually missing it, so the same pass is safe to run against every
/// store on every open, whatever its version stamp claims.
///
/// Order matters only in that a column must not be named before its table is
/// created — [`ConversationStore::ensure_schema`] runs every creation statement
/// first, so every table here exists by the time the list is walked.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("conversation", "parent_conversation_id", "TEXT"),
    ("conversation", "preset_id", "TEXT"),
    ("conversation", "template_id", "TEXT"),
    ("conversation", "additional_directories", "TEXT"),
    ("conversation", "attached_workspaces", "TEXT"),
    // 1 once the title is final: generated by the local helper model or set by
    // the user. The conversation upsert never writes it, so a renderer commit
    // cannot clear it; only `set_title_settled` does.
    ("conversation", "title_settled", "INTEGER NOT NULL DEFAULT 0"),
    ("conversation", "fork_of", "TEXT"),
    ("conversation", "handoff_of", "TEXT"),
    ("queued_message", "files", "TEXT NOT NULL DEFAULT '[]'"),
];

/// Process-local connections cached by database path. Storage operations receive only the anchor path,
/// so reuse by path preserves WAL and `busy_timeout` semantics.
fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<ConversationStore>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<ConversationStore>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Maps an anchor path to its sibling database path.
pub fn database_path(anchor: &Path) -> PathBuf {
    anchor
        .parent()
        .map(|parent| parent.join(DATABASE_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(DATABASE_FILE_NAME))
}

/// Opens the conversation store for an anchor file when necessary.
pub fn store_for(anchor: &Path) -> Result<Arc<ConversationStore>, String> {
    let path = database_path(anchor);
    let mut registry = registry()
        .lock()
        .map_err(|_| "对话库注册表已中毒".to_string())?;
    if let Some(existing) = registry.get(&path) {
        return Ok(Arc::clone(existing));
    }
    let store = Arc::new(ConversationStore::open(&path)?);
    registry.insert(path, Arc::clone(&store));
    Ok(store)
}

/// Closes and discards an anchor's connection. Call before `reset:data` deletes the directory:
/// open file handles prevent deletion on Windows, and a stale connection could write WAL frames back.
pub fn close_store_for(anchor: &Path) {
    let path = database_path(anchor);
    if let Ok(mut registry) = registry().lock() {
        registry.remove(&path);
    }
    BodyCache::for_file(&path).invalidate_all();
}

pub struct ConversationStore {
    conn: Mutex<Connection>,
    bodies: BodyCache,
}

/// The read-through body cache behind [`ConversationStore::conversation`].
///
/// Bodies live in the shared [`MemoryPool`] as high-priority entries, so the
/// pool may unload any of them; a read then falls back to the database and
/// fills the pool again. Every write that can change a [`Conversation`] goes
/// through this store and invalidates the body it touched once the write has
/// committed, so a cached body is never older than the database.
///
/// Reads and writes race without a shared lock: a read notes the body's
/// generation before reading the database and fills the pool only if no write
/// has invalidated the body since. A read that loses the race still returns
/// what it read — as a direct database read would — it just does not keep it.
///
/// Cache and generations belong to the database file, not to one store
/// instance: every instance opened on the same file shares them, so a write
/// through one invalidates what another cached. Opening a file, and closing it
/// for a reset, drop whatever the pool held for it.
struct BodyCache {
    /// Prefix of this file's keys in the process-wide pool.
    scope: String,
    generations: Arc<Mutex<BodyGenerations>>,
}

#[derive(Default)]
struct BodyGenerations {
    /// Bumped by writes that can touch every conversation: startup recovery of
    /// streaming rows, and the re-parenting done by deletion and reordering.
    epoch: u64,
    by_id: HashMap<String, u64>,
}

impl BodyGenerations {
    fn of(&self, conversation_id: &str) -> (u64, u64) {
        (
            self.epoch,
            self.by_id.get(conversation_id).copied().unwrap_or_default(),
        )
    }
}

impl BodyCache {
    fn for_file(db_path: &Path) -> Self {
        static FILES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<BodyGenerations>>>>> =
            OnceLock::new();
        let generations = FILES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(db_path.to_owned())
            .or_default()
            .clone();
        Self {
            scope: Self::scope_of(db_path),
            generations,
        }
    }

    fn scope_of(db_path: &Path) -> String {
        // NUL cannot occur in a path, so no file's scope is a prefix of another's.
        format!("{}\u{0}", db_path.display())
    }

    fn key(&self, conversation_id: &str) -> PoolKey {
        PoolKey::new(
            PoolKind::ConversationBody,
            format!("{}{conversation_id}", self.scope),
        )
    }

    fn lock(&self) -> MutexGuard<'_, BodyGenerations> {
        self.generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cached(&self, conversation_id: &str) -> Option<Arc<Conversation>> {
        MemoryPool::global().get::<Conversation>(&self.key(conversation_id))
    }

    fn generation(&self, conversation_id: &str) -> (u64, u64) {
        self.lock().of(conversation_id)
    }

    /// Keeps a body read at generation `seen`, unless a write has invalidated
    /// it since.
    fn fill(&self, conversation_id: &str, seen: (u64, u64), body: &Arc<Conversation>) {
        let bytes = crate::memory_pool::serialized_bytes(body.as_ref());
        let generations = self.lock();
        if generations.of(conversation_id) == seen {
            MemoryPool::global().insert(self.key(conversation_id), Arc::clone(body), bytes);
        }
    }

    fn invalidate(&self, conversation_id: &str) {
        let mut generations = self.lock();
        *generations
            .by_id
            .entry(conversation_id.to_owned())
            .or_default() += 1;
        MemoryPool::global().remove(&self.key(conversation_id));
    }

    fn invalidate_all(&self) {
        let mut generations = self.lock();
        generations.epoch += 1;
        self.forget_all();
    }

    fn forget_all(&self) {
        let scope = self.scope.as_str();
        MemoryPool::global().retain(|key| {
            !(key.kind == PoolKind::ConversationBody && key.id.starts_with(scope))
        });
    }
}


impl ConversationStore {
    /// Opens the store, setting aside and rebuilding a database this build cannot
    /// use rather than failing startup with it.
    ///
    /// The quarantine covers connecting as well as reconciling. A file damaged
    /// past the header fails at `PRAGMA journal_mode`, before any schema is read,
    /// so a recovery that only guarded the schema step would let that file refuse
    /// every open for as long as it stayed in place — and every caller of
    /// [`store_for`] would keep getting the same error with nothing able to clear
    /// it. Whichever step refuses, the file is set aside intact and replaced.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建对话库目录：{error}"))?;
        }
        match Self::connect(db_path) {
            Ok(store) => Ok(store),
            Err(error) => {
                quarantine_database(db_path, &error);
                Self::connect(db_path)
            }
        }
    }

    /// Connects to the database at `db_path` and brings it up to date, leaving the
    /// file untouched on refusal. Both attempts in [`Self::open`] go through here,
    /// so the retry after a quarantine is the same code path as the first try.
    fn connect(db_path: &Path) -> Result<Self, String> {
        let store = Self {
            conn: Mutex::new(open_configured(db_path)?),
            bodies: BodyCache::for_file(db_path),
        };
        // A file opened afresh may not be the one cached bodies came from (a
        // reset deletes it; recovery replaces it).
        store.bodies.invalidate_all();
        store.ensure_schema()?;
        Ok(store)
    }

    /// Brings the store to the shape this build compiles against and stamps it.
    ///
    /// What gets repaired is decided by what the database actually holds, not by
    /// what its version stamp claims: every table is created when missing and
    /// every column in [`ADDED_COLUMNS`] is added when missing, so the pass is
    /// idempotent and runs on every open — including one whose stamp already reads
    /// current. A ladder keyed on the stamp cannot do this. It repairs only what
    /// the stamp says is outstanding, so a store whose stamp overstates its schema
    /// is never examined again: the read and write paths go on naming a column that
    /// is not there and the store stays broken for good. That is not hypothetical —
    /// it is how a `wire_request` missing `owner` survived behind a stamp already
    /// reading `STORE_VERSION`, failing every ledger read and every ledger write.
    ///
    /// The whole pass is one `BEGIN IMMEDIATE`, so a store is reconciled and stamped
    /// together or left exactly as it was: a process that dies midway leaves nothing
    /// half-upgraded, and the next open repairs from a shape it can still read.
    fn ensure_schema(&self) -> Result<(), String> {
        let mut conn = self.lock()?;
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("无法读取对话库版本：{error}"))?;
        if version > STORE_VERSION {
            // A store written by a later build may carry columns and checks this one
            // cannot honour, and meeting it would be a downgrade rather than the
            // additive repair below. Quarantine and rebuild instead.
            return Err(format!(
                "对话库版本 {version} 与当前实现的 {STORE_VERSION} 不一致"
            ));
        }
        if version == 0 {
            let has_tables: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'conversation'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("无法检查对话库结构：{error}"))?;
            if has_tables > 0 {
                // Tables but no stamp: not a database this application wrote.
                // Repairing it would `ALTER` a stranger's data, so it is set aside
                // untouched rather than reconciled.
                return Err("对话库缺少版本标记但已有数据表".into());
            }
        }
        if version == STORE_VERSION && shape_is_current(&conn)? {
            // Stamp and schema agree, so there is nothing to repair and no reason
            // to take a write lock: the overwhelmingly common open stays a few
            // reads. The stamp alone would not be enough to skip the pass — it is
            // the schema behind it that is being trusted here, not the number.
            return Ok(());
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启对话库升级事务：{error}"))?;
        // Every statement in these is `IF NOT EXISTS`: this builds what a store is
        // missing and leaves what it already has untouched. A fresh store gets all
        // of it, a store already current gets nothing, and a store that lost one
        // table to a failed upgrade gets exactly that table back.
        for schema in SCHEMAS {
            tx.execute_batch(schema)
                .map_err(|error| format!("无法建立对话库结构：{error}"))?;
        }
        let title_settled_is_new = !has_column(&tx, "conversation", "title_settled")?;
        for &(table, column, declaration) in ADDED_COLUMNS {
            if has_column(&tx, table, column)? {
                continue;
            }
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
            ))
            .map_err(|error| format!("无法升级对话库结构：{error}"))?;
        }
        if title_settled_is_new {
            // A conversation that already has a reply is past its first request,
            // which is when a title gets generated; its title is the user's.
            tx.execute_batch(
                "UPDATE conversation SET title_settled = 1 WHERE EXISTS (
                     SELECT 1 FROM context WHERE context.conversation_id = conversation.id AND context.kind = 'assistant')",
            )
            .map_err(|error| format!("无法升级对话库结构：{error}"))?;
        }
        // After every table exists: the migration writes into the history tables
        // the pass above just made sure of.
        migrate_legacy_history(&tx)?;
        if has_column(&tx, "fork_decision", "inherit_context")? {
            // `fork` lost its inherit-context option, so the recorded answer to it is
            // meaningless. Dropping the column in place keeps the rest of each
            // decision, so the task bar still draws old rows. Its presence is checked
            // rather than assumed because a `fork_decision` predating the option is a
            // legal shape on disk, and refusing it would quarantine a readable store.
            tx.execute_batch("ALTER TABLE fork_decision DROP COLUMN inherit_context")
                .map_err(|error| format!("无法升级对话库结构：{error}"))?;
        }
        tx.pragma_update(None, "user_version", STORE_VERSION)
            .map_err(|error| format!("无法写入对话库版本：{error}"))?;
        tx.commit()
            .map_err(|error| format!("无法提交对话库升级事务：{error}"))?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "对话库连接已中毒".to_string())
    }

    /// Runs multiple writes in one `BEGIN IMMEDIATE` transaction: all changes are visible together or not at all.
    fn with_write_tx<T>(
        &self,
        operation: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启对话库事务：{error}"))?;
        let value = operation(&tx)?;
        tx.commit()
            .map_err(|error| format!("无法提交对话库事务：{error}"))?;
        Ok(value)
    }

    // ---------------------------------------------------------------- Read

    /// Lists every conversation in a workspace in sidebar order, bodies and
    /// all. Nothing in the app wants every body of a workspace at once; tests
    /// read them back this way.
    #[cfg(test)]
    pub fn workspace_conversations(&self, workspace_id: &str) -> Result<Vec<Conversation>, String> {
        let ids = self.workspace_conversation_ids(workspace_id)?;
        let mut conversations = Vec::with_capacity(ids.len());
        for id in ids {
            // A malformed row affects only its conversation, not the entire list.
            match self.conversation(&id) {
                Ok(Some(conversation)) => conversations.push(conversation),
                Ok(None) => {}
                Err(error) => eprintln!("对话 {id} 的正文无法装配，已跳过：{error}"),
            }
        }
        Ok(conversations)
    }

    /// Ids of a workspace's conversations, in sidebar order.
    pub(crate) fn workspace_conversation_ids(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT id FROM conversation WHERE workspace_id = ?1 ORDER BY order_key, id")
            .map_err(|error| format!("无法查询对话列表：{error}"))?;
        let rows = statement
            .query_map([workspace_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法查询对话列表：{error}"))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|error| format!("无法读取对话行：{error}"))?);
        }
        Ok(ids)
    }

    /// Maps every existing conversation to its workspace.
    pub fn conversation_workspaces(&self) -> Result<HashMap<String, String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT id, workspace_id FROM conversation")
            .map_err(|error| format!("无法查询对话归属：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("无法查询对话归属：{error}"))?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, workspace_id) = row.map_err(|error| format!("无法读取对话归属：{error}"))?;
            map.insert(id, workspace_id);
        }
        Ok(map)
    }

    /// Aggregates user and assistant messages plus used conversations by UTC hour.
    /// The renderer converts UTC buckets to local dates and hours, so history remains valid after a timezone change.
    pub fn activity_buckets(&self) -> Result<Vec<ActivityBucket>, String> {
        let conn = self.lock()?;
        let mut buckets: HashMap<i64, ActivityBucket> = HashMap::new();
        {
            let mut statement = conn
                .prepare(
                    "SELECT CAST(strftime('%s', created_at) AS INTEGER) / 3600 * 3600000 AS hour_start,
                            kind,
                            count(*)
                     FROM context
                     WHERE kind IN ('user', 'assistant')
                       AND strftime('%s', created_at) IS NOT NULL
                     GROUP BY hour_start, kind",
                )
                .map_err(|error| format!("无法准备消息统计：{error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|error| format!("无法读取消息统计：{error}"))?;
            for row in rows {
                let (hour_start, kind, count) =
                    row.map_err(|error| format!("无法读取消息统计行：{error}"))?;
                let bucket = buckets.entry(hour_start).or_insert_with(|| ActivityBucket {
                    hour_start_ms: hour_start,
                    ..ActivityBucket::default()
                });
                if kind == "user" {
                    bucket.user_messages += count.max(0) as u64;
                } else {
                    bucket.assistant_messages += count.max(0) as u64;
                }
            }
        }
        {
            // Empty conversations do not count as sessions.
            let mut statement = conn
                .prepare(
                    "SELECT CAST(strftime('%s', c.created_at) AS INTEGER) / 3600 * 3600000 AS hour_start,
                            count(*)
                     FROM conversation c
                     WHERE strftime('%s', c.created_at) IS NOT NULL
                       AND EXISTS (
                             SELECT 1 FROM context x
                             WHERE x.conversation_id = c.id AND x.kind = 'user'
                           )
                     GROUP BY hour_start",
                )
                .map_err(|error| format!("无法准备会话统计：{error}"))?;
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .map_err(|error| format!("无法读取会话统计：{error}"))?;
            for row in rows {
                let (hour_start, count) =
                    row.map_err(|error| format!("无法读取会话统计行：{error}"))?;
                buckets
                    .entry(hour_start)
                    .or_insert_with(|| ActivityBucket {
                        hour_start_ms: hour_start,
                        ..ActivityBucket::default()
                    })
                    .sessions += count.max(0) as u64;
            }
        }
        let mut ordered = buckets.into_values().collect::<Vec<_>>();
        ordered.sort_by_key(|bucket| bucket.hour_start_ms);
        Ok(ordered)
    }

    /// A complete conversation body, or `None` when absent: from memory when the
    /// shared pool still holds it, otherwise from the database (which then fills
    /// the pool). This is the one way the host reads a body; see [`BodyCache`].
    pub fn conversation(&self, conversation_id: &str) -> Result<Option<Conversation>, String> {
        Ok(self
            .conversation_shared(conversation_id)?
            .map(|conversation| (*conversation).clone()))
    }

    /// [`Self::conversation`] without the copy, for readers that only look.
    pub fn conversation_shared(
        &self,
        conversation_id: &str,
    ) -> Result<Option<Arc<Conversation>>, String> {
        if let Some(cached) = self.bodies.cached(conversation_id) {
            return Ok(Some(cached));
        }
        let seen = self.bodies.generation(conversation_id);
        let Some(conversation) = self.conversation_from_disk(conversation_id)? else {
            return Ok(None);
        };
        let conversation = Arc::new(conversation);
        self.bodies.fill(conversation_id, seen, &conversation);
        Ok(Some(conversation))
    }

    /// A conversation without its body — no contexts, no branches — with its
    /// metadata, settings, queued messages and aborted tasks, always from the
    /// database. What a document needs to list a conversation whose body is not
    /// loaded (see `attachment_refs::strip_body` for why branches are body).
    pub fn conversation_shell(&self, conversation_id: &str) -> Result<Option<Conversation>, String> {
        self.read_conversation(conversation_id, false)
    }

    /// [`Self::conversation_shell`] for every conversation in a workspace, in
    /// sidebar order.
    pub fn workspace_conversation_shells(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<Conversation>, String> {
        let mut shells = Vec::new();
        for id in self.workspace_conversation_ids(workspace_id)? {
            match self.conversation_shell(&id) {
                Ok(Some(shell)) => shells.push(shell),
                Ok(None) => {}
                Err(error) => eprintln!("对话 {id} 的外壳无法装配，已跳过：{error}"),
            }
        }
        Ok(shells)
    }

    /// Ids of the conversations that hold at least one context, on any branch.
    pub fn conversations_with_contexts(&self) -> Result<HashSet<String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT DISTINCT conversation_id FROM context")
            .map_err(|error| format!("无法查询对话正文：{error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法查询对话正文：{error}"))?;
        rows.collect::<Result<HashSet<_>, _>>()
            .map_err(|error| format!("无法读取对话正文：{error}"))
    }

    /// Whether the shared pool holds this conversation's body now.
    #[cfg(test)]
    pub(crate) fn body_is_cached(&self, conversation_id: &str) -> bool {
        MemoryPool::global().contains(&self.bodies.key(conversation_id))
    }

    /// Assembles a complete conversation body from the database, returning
    /// `None` when absent, without going through the shared memory pool: for a
    /// reader that uses a body once and drops it (the startup scan), where
    /// filling the pool would keep every body it passed over.
    pub(crate) fn conversation_from_disk(
        &self,
        conversation_id: &str,
    ) -> Result<Option<Conversation>, String> {
        self.read_conversation(conversation_id, true)
    }

    fn read_conversation(
        &self,
        conversation_id: &str,
        with_contexts: bool,
    ) -> Result<Option<Conversation>, String> {
        let conn = self.lock()?;
        let shell = conn
            .query_row(
                "SELECT title, created_at, updated_at, settings, worktree, run_target, parent_conversation_id, preset_id, template_id, additional_directories, attached_workspaces, fork_of, handoff_of FROM conversation WHERE id = ?1",
                [conversation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取对话：{error}"))?;
        let Some((
            title,
            created_at,
            updated_at,
            settings_json,
            worktree_json,
            run_target_json,
            parent_conversation_id,
            preset_id,
            template_id,
            additional_directories_json,
            attached_workspaces_json,
            fork_of_json,
            handoff_of_json,
        )) = shell
        else {
            return Ok(None);
        };
        // Settings written while plan mode was a security level move it to the
        // plan-mode setting on the way in.
        let mut settings_value: serde_json::Value = serde_json::from_str(&settings_json)
            .map_err(|error| format!("对话设置无法解析：{error}"))?;
        crate::model::migrate_legacy_plan_level(&mut settings_value);
        let settings: ConversationSettings = serde_json::from_value(settings_value)
            .map_err(|error| format!("对话设置无法解析：{error}"))?;
        // An unreadable worktree record falls back to the workspace root, which is the safe target.
        let worktrees = worktree_json
            .as_deref()
            .map(read_worktrees)
            .unwrap_or_default();
        // An unreadable run target must not fall back to local execution. Bind it to a nonexistent
        // machine so dispatch fails explicitly until the user selects a valid target.
        let run_target = run_target_json.as_deref().map(|value| {
            serde_json::from_str::<RunTarget>(value).unwrap_or(RunTarget::Ssh {
                machine_id: "invalid-run-target".into(),
            })
        });
        // An unreadable list narrows to none: extra directories widen a security
        // boundary, so a record we cannot read must not be guessed at.
        let additional_directories = additional_directories_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .unwrap_or_default();
        let attached_workspaces = attached_workspaces_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<AttachedWorkspace>>(value).ok())
            .unwrap_or_default();
        // A trace, not authority: an unreadable one reads as "not a fork".
        let fork_of = fork_of_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<ConversationForkOrigin>(value).ok());
        // Likewise: an unreadable one only restarts the next handoff's numbering.
        let handoff_of = handoff_of_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<ConversationHandoffOrigin>(value).ok());

        let contexts = if with_contexts {
            read_contexts(&conn, conversation_id, None)?
        } else {
            Vec::new()
        };

        let mut branches = Vec::new();
        if with_contexts {
            let mut statement = conn
                .prepare(
                    "SELECT id, fork_context_id, active, created_at, updated_at
                     FROM branch WHERE conversation_id = ?1 ORDER BY order_key, id",
                )
                .map_err(|error| format!("无法查询分支：{error}"))?;
            let rows = statement
                .query_map([conversation_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|error| format!("无法查询分支：{error}"))?;
            for row in rows {
                let (id, fork_context_id, active, created_at, updated_at) =
                    row.map_err(|error| format!("无法读取分支：{error}"))?;
                let branch_contexts = read_contexts(&conn, conversation_id, Some(&id))?;
                branches.push(ConversationBranch {
                    id,
                    fork_context_id,
                    active,
                    contexts: branch_contexts,
                    created_at,
                    updated_at,
                });
            }
        }

        let mut queued_messages = Vec::new();
        {
            let mut statement = conn
                .prepare(
                    "SELECT id, content, images, files, created_at FROM queued_message
                     WHERE conversation_id = ?1 ORDER BY order_key, id",
                )
                .map_err(|error| format!("无法查询排队消息：{error}"))?;
            let rows = statement
                .query_map([conversation_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|error| format!("无法查询排队消息：{error}"))?;
            for row in rows {
                let (id, content, images_json, files_json, created_at) =
                    row.map_err(|error| format!("无法读取排队消息：{error}"))?;
                let images: Vec<ImageAttachment> = serde_json::from_str(&images_json)
                    .map_err(|error| format!("排队消息附图无法解析：{error}"))?;
                let files: Vec<FileAttachment> = serde_json::from_str(&files_json)
                    .map_err(|error| format!("排队消息附件无法解析：{error}"))?;
                queued_messages.push(QueuedMessage {
                    id,
                    content,
                    images,
                    files,
                    created_at,
                });
            }
        }

        let mut user_aborted_tasks = Vec::new();
        {
            let mut statement = conn
                .prepare(
                    "SELECT data FROM aborted_task WHERE conversation_id = ?1 ORDER BY order_key, id",
                )
                .map_err(|error| format!("无法查询中止任务：{error}"))?;
            let rows = statement
                .query_map([conversation_id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法查询中止任务：{error}"))?;
            for row in rows {
                let data = row.map_err(|error| format!("无法读取中止任务：{error}"))?;
                let record: UserAbortedTaskRecord = serde_json::from_str(&data)
                    .map_err(|error| format!("中止任务记录无法解析：{error}"))?;
                user_aborted_tasks.push(record);
            }
        }

        Ok(Some(Conversation {
            id: conversation_id.to_owned(),
            title,
            created_at,
            updated_at,
            settings,
            contexts,
            queued_messages,
            branches,
            user_aborted_tasks,
            worktrees,
            run_target,
            parent_conversation_id,
            fork_of,
            handoff_of,
            preset_id: preset_id.unwrap_or_default(),
            template_id: template_id.unwrap_or_default(),
            attached_workspaces,
            additional_directories,
        }))
    }

    /// Every saved template, newest first, without bodies. The picker only needs
    /// names and sizes, and a template body is large enough that shipping all of
    /// them to draw a list would be wasteful.
    pub fn templates(&self) -> Result<Vec<ConversationTemplateSummary>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT t.id, t.name, t.created_at, t.updated_at,
                        (SELECT count(*) FROM template_context c WHERE c.template_id = t.id)
                 FROM conversation_template t ORDER BY t.order_key DESC, t.rowid DESC",
            )
            .map_err(|error| format!("无法查询对话模板：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(ConversationTemplateSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    message_count: row.get::<_, i64>(4)?.max(0) as u32,
                })
            })
            .map_err(|error| format!("无法查询对话模板：{error}"))?;
        let mut templates = Vec::new();
        for row in rows {
            templates.push(row.map_err(|error| format!("无法读取对话模板：{error}"))?);
        }
        Ok(templates)
    }

    /// A template's body in timeline order. An unknown id is an empty body rather
    /// than an error: a role may hold a dangling template id, and a role that
    /// seeds nothing is a working role.
    pub fn template_contexts(&self, template_id: &str) -> Result<Vec<ContextItem>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT data FROM template_context WHERE template_id = ?1
                 ORDER BY order_key, rowid",
            )
            .map_err(|error| format!("无法查询模板正文：{error}"))?;
        let rows = statement
            .query_map([template_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法查询模板正文：{error}"))?;
        let mut items = Vec::new();
        for row in rows {
            let data = row.map_err(|error| format!("无法读取模板正文：{error}"))?;
            items.push(
                serde_json::from_str::<ContextItem>(&data)
                    .map_err(|error| format!("模板正文无法解析：{error}"))?,
            );
        }
        Ok(items)
    }

    /// Every image id any stored template body holds, in one pass.
    ///
    /// A template belongs to no conversation, so nothing in the document points
    /// at its attachments; without this the reclaimer would read a captured
    /// tool screenshot — or an image written into a template by hand — as an
    /// orphan and eventually delete the bytes out from under a template that
    /// still renders them.
    pub fn template_image_ids(&self) -> Result<HashSet<String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT data FROM template_context")
            .map_err(|error| format!("无法查询模板图片引用：{error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法查询模板图片引用：{error}"))?;
        let mut ids = HashSet::new();
        for row in rows {
            let data = row.map_err(|error| format!("无法读取模板正文：{error}"))?;
            // A body that no longer parses is a body no reader can render, so it
            // pins nothing; the shape check on write is what keeps that rare.
            if let Ok(context) = serde_json::from_str::<ContextItem>(&data) {
                crate::image_attachments::collect_context_image_ids(
                    std::slice::from_ref(&context),
                    &mut ids,
                );
            }
        }
        Ok(ids)
    }

    /// Every file attachment id any stored template body holds, for the same
    /// reason as [`Self::template_image_ids`]: a template's attachments are
    /// referenced by nothing in the document.
    pub fn template_file_ids(&self) -> Result<HashSet<String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare("SELECT data FROM template_context")
            .map_err(|error| format!("无法查询模板附件引用：{error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法查询模板附件引用：{error}"))?;
        let mut ids = HashSet::new();
        for row in rows {
            let data = row.map_err(|error| format!("无法读取模板正文：{error}"))?;
            if let Ok(context) = serde_json::from_str::<ContextItem>(&data) {
                crate::file_attachments::collect_context_file_ids(
                    std::slice::from_ref(&context),
                    &mut ids,
                );
            }
        }
        Ok(ids)
    }

    /// Writes a template, replacing any body already stored under `id`.
    pub fn put_template(
        &self,
        id: &str,
        name: &str,
        contexts: &[ContextItem],
    ) -> Result<ConversationTemplateSummary, String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启模板写入事务：{error}"))?;
        let created_at: Option<String> = tx
            .query_row(
                "SELECT created_at FROM conversation_template WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法读取对话模板：{error}"))?;
        let now = now();
        let created_at = created_at.unwrap_or_else(|| now.clone());
        let order_key: f64 = tx
            .query_row(
                "SELECT coalesce(max(order_key), -1.0) FROM conversation_template",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("无法计算模板序号：{error}"))?;
        tx.execute(
            "INSERT INTO conversation_template (id, name, created_at, updated_at, order_key)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name,
               updated_at = excluded.updated_at",
            rusqlite::params![id, name, created_at, now, order_key + ORDER_STEP],
        )
        .map_err(|error| format!("无法写入对话模板：{error}"))?;
        write_template_body(&tx, id, contexts)?;
        tx.commit()
            .map_err(|error| format!("无法提交模板写入事务：{error}"))?;
        Ok(ConversationTemplateSummary {
            id: id.to_owned(),
            name: name.to_owned(),
            message_count: contexts.len() as u32,
            created_at,
            updated_at: now,
        })
    }

    /// Replaces a template's body, leaving its name, its creation time and its
    /// position in the list alone.
    ///
    /// The body is the only part the renderer edits, and the only part this
    /// writes: `name` is not this method's to write and `order_key` is
    /// assigned once, at creation, so a rewritten template must not jump to the
    /// front of the picker. An unknown id is an error here rather than an empty
    /// read, because writing a body no template owns would silently seed a
    /// template that the list will never show.
    pub fn put_template_contexts(
        &self,
        id: &str,
        contexts: &[ContextItem],
    ) -> Result<ConversationTemplateSummary, String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启模板写入事务：{error}"))?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT name, created_at FROM conversation_template WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取对话模板：{error}"))?;
        let Some((name, created_at)) = row else {
            return Err(format!("对话模板 {id} 不存在"));
        };
        let now = now();
        tx.execute(
            "UPDATE conversation_template SET updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now],
        )
        .map_err(|error| format!("无法更新对话模板：{error}"))?;
        write_template_body(&tx, id, contexts)?;
        tx.commit()
            .map_err(|error| format!("无法提交模板写入事务：{error}"))?;
        Ok(ConversationTemplateSummary {
            id: id.to_owned(),
            name,
            message_count: contexts.len() as u32,
            created_at,
            updated_at: now,
        })
    }

    /// Deletes a template and its body. Conversations and roles that cite it keep
    /// the dangling id, which reads as "no template" everywhere it is resolved.
    pub fn delete_template(&self, id: &str) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启模板删除事务：{error}"))?;
        tx.execute("DELETE FROM template_context WHERE template_id = ?1", [id])
            .map_err(|error| format!("无法删除模板正文：{error}"))?;
        tx.execute("DELETE FROM conversation_template WHERE id = ?1", [id])
            .map_err(|error| format!("无法删除对话模板：{error}"))?;
        tx.commit()
            .map_err(|error| format!("无法提交模板删除事务：{error}"))
    }

    /// Every recorded change to the trunk, oldest first. Test-only: it reads back
    /// what the recorder wrote.
    #[cfg(test)]
    pub fn trunk_changes(&self, conversation_id: &str) -> Result<Vec<TrunkChangeSummary>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT seq, kind, request_id, detail, created_at FROM history_entry
                 WHERE conversation_id = ?1 AND kind IN ('edit', 'run') ORDER BY seq",
            )
            .map_err(|error| format!("无法查询时间线历史：{error}"))?;
        let rows = statement
            .query_map([conversation_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|error| format!("无法查询时间线历史：{error}"))?;
        let mut changes = Vec::new();
        for row in rows {
            let (seq, kind, request_id, detail, created_at) =
                row.map_err(|error| format!("无法读取时间线历史行：{error}"))?;
            let detail = parse_detail(&detail);
            let count = |key: &str| detail.get(key).and_then(serde_json::Value::as_i64).unwrap_or(0);
            changes.push(TrunkChangeSummary {
                seq,
                kind,
                source: detail
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                request_id,
                inserted: count("inserted"),
                removed: count("removed"),
                replaced: count("replaced"),
                row_count: count("rowCount"),
                created_at,
            });
        }
        Ok(changes)
    }

    /// The trunk as it stood once `seq` applied, rebuilt by folding the chain from
    /// its baseline. A row whose body no longer parses is dropped rather than
    /// failing the whole snapshot: one unreadable card must not hide the history
    /// around it. Test-only: it reads back what the recorder wrote.
    #[cfg(test)]
    pub fn trunk_snapshot(
        &self,
        conversation_id: &str,
        seq: i64,
    ) -> Result<Vec<ContextItem>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT op.op, op.context_id, op.position, stored.body
                 FROM history_op op
                 LEFT JOIN history_blob stored
                   ON stored.conversation_id = op.conversation_id AND stored.hash = op.hash
                 WHERE op.conversation_id = ?1 AND op.seq <= ?2 ORDER BY op.seq, op.ordinal",
            )
            .map_err(|error| format!("无法查询时间线快照：{error}"))?;
        let rows = statement
            .query_map(rusqlite::params![conversation_id, seq], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|error| format!("无法查询时间线快照：{error}"))?;
        let mut trunk: Vec<(String, String)> = Vec::new();
        for row in rows {
            let (op, context_id, position, data) =
                row.map_err(|error| format!("无法读取时间线快照行：{error}"))?;
            match op.as_str() {
                "remove" => trunk.retain(|(id, _)| id != &context_id),
                "insert" => {
                    let at = position
                        .unwrap_or(trunk.len() as i64)
                        .clamp(0, trunk.len() as i64) as usize;
                    trunk.insert(at, (context_id, data.unwrap_or_default()));
                }
                "replace" => {
                    if let Some(entry) = trunk.iter_mut().find(|(id, _)| id == &context_id) {
                        entry.1 = data.unwrap_or_default();
                    }
                }
                _ => {}
            }
        }
        Ok(trunk
            .into_iter()
            .filter_map(|(_, data)| serde_json::from_str::<ContextItem>(&data).ok())
            .collect())
    }

    /// Appends the trunk's current shape to the history. A recording that finds
    /// nothing changed writes no entry: a run that produced no lasting row and a
    /// debounced metadata commit both reach here, and neither is a moment in the
    /// timeline's history.
    pub fn record_trunk_change(
        &self,
        conversation_id: &str,
        change: TrunkChange,
        request_id: Option<&str>,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| record_trunk_change_tx(tx, conversation_id, change, request_id))
    }

    /// Every entry of one owner, oldest first, without bodies: what the history
    /// pane lists. Nothing is ever pruned, so this is the whole record.
    ///
    /// `owners` picks whose: `None` is the conversation's own trunk — its requests
    /// and responses, its hooks and calls, and every change to its timeline — and a
    /// list of child addresses is those children's. Defaulting to the trunk rather
    /// than to everything is deliberate: a pane that asked for the session's
    /// history must not silently start reporting its children's as its own.
    ///
    /// A request carries the usage of the response that answered it, so the cost
    /// sits on the send that incurred it.
    pub fn history_entries(
        &self,
        conversation_id: &str,
        owners: Option<&[String]>,
    ) -> Result<Vec<HistoryEntry>, String> {
        // An agent nobody has addressed yet owns nothing, which is not the same
        // question as "the trunk" and must not be answered with the trunk's rows.
        if owners.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let filter = match owners {
            None => "owner IS NULL".to_owned(),
            Some(list) => {
                let slots = (0..list.len())
                    .map(|index| format!("?{}", index + 2))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("owner IN ({slots})")
            }
        };
        let mut statement = conn
            .prepare(&format!(
                "SELECT seq, created_at, kind, owner, request_id, round, call_id, answers, detail
                 FROM history_entry WHERE conversation_id = ?1 AND {filter} ORDER BY seq"
            ))
            .map_err(|error| format!("无法查询历史记录：{error}"))?;
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&conversation_id];
        for owner in owners.unwrap_or_default() {
            bound.push(owner);
        }
        let rows = statement
            .query_map(bound.as_slice(), read_history_entry)
            .map_err(|error| format!("无法查询历史记录：{error}"))?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|error| format!("无法读取历史记录行：{error}"))?);
        }
        let answered: HashMap<i64, HistoryUsage> = entries
            .iter()
            .filter(|entry| entry.kind == "response")
            .filter_map(|entry| Some((entry.answers?, entry.usage?)))
            .collect();
        for entry in &mut entries {
            if entry.kind == "request" {
                // A request recorded before responses were, carries its own.
                entry.usage = answered.get(&entry.seq).copied().or(entry.usage);
            }
        }
        Ok(entries)
    }

    /// One entry with its body — and a request's parts, a trunk change's steps —
    /// as the pane reads it when a row opens. A body that no longer parses reads as
    /// text instead of failing the entry: the rest is still evidence, and one
    /// corrupt row must not hide it.
    pub fn history_entry(
        &self,
        conversation_id: &str,
        seq: i64,
    ) -> Result<Option<HistoryEntryDetail>, String> {
        let conn = self.lock()?;
        let found = conn
            .query_row(
                "SELECT entry.seq, entry.created_at, entry.kind, entry.owner, entry.request_id,
                        entry.round, entry.call_id, entry.answers, entry.detail,
                        stored.body, stored.truncated
                 FROM history_entry entry
                 LEFT JOIN history_blob stored
                   ON stored.conversation_id = entry.conversation_id AND stored.hash = entry.hash
                 WHERE entry.conversation_id = ?1 AND entry.seq = ?2",
                rusqlite::params![conversation_id, seq],
                |row| {
                    Ok((
                        read_history_entry(row)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法查询历史记录条目：{error}"))?;
        let Some((mut entry, body, truncated)) = found else {
            return Ok(None);
        };
        let mut parts = Vec::new();
        let mut ops = Vec::new();
        match entry.kind.as_str() {
            "request" => {
                let answered = conn
                    .query_row(
                        "SELECT detail FROM history_entry
                         WHERE conversation_id = ?1 AND kind = 'response' AND answers = ?2
                         ORDER BY seq DESC LIMIT 1",
                        rusqlite::params![conversation_id, seq],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| format!("无法查询历史记录条目：{error}"))?;
                if let Some(detail) = answered {
                    entry.usage = HistoryUsage::from_detail(&parse_detail(&detail)).or(entry.usage);
                }
                let mut statement = conn
                    .prepare(
                        "SELECT part.ordinal, part.kind, part.hash, stored.body, stored.truncated
                         FROM history_part part
                         JOIN history_blob stored
                           ON stored.conversation_id = part.conversation_id AND stored.hash = part.hash
                         WHERE part.conversation_id = ?1 AND part.seq = ?2
                         ORDER BY part.ordinal",
                    )
                    .map_err(|error| format!("无法查询请求分段：{error}"))?;
                let rows = statement
                    .query_map(rusqlite::params![conversation_id, seq], |row| {
                        let body: String = row.get(3)?;
                        Ok(HistoryPart {
                            ordinal: row.get(0)?,
                            kind: row.get(1)?,
                            hash: row.get(2)?,
                            // Counted here rather than in the renderer: JavaScript would
                            // count UTF-16 units, and these have to add up to the
                            // request's own `bytes`, which is a sum of UTF-8 lengths.
                            bytes: body.len() as i64,
                            body,
                            truncated: row.get::<_, i64>(4)? != 0,
                        })
                    })
                    .map_err(|error| format!("无法查询请求分段：{error}"))?;
                for row in rows {
                    parts.push(row.map_err(|error| format!("无法读取请求分段行：{error}"))?);
                }
            }
            "edit" | "run" => {
                let mut statement = conn
                    .prepare(
                        "SELECT op.ordinal, op.op, op.context_id, op.position, stored.body
                         FROM history_op op
                         LEFT JOIN history_blob stored
                           ON stored.conversation_id = op.conversation_id AND stored.hash = op.hash
                         WHERE op.conversation_id = ?1 AND op.seq = ?2 ORDER BY op.ordinal",
                    )
                    .map_err(|error| format!("无法查询时间线改动：{error}"))?;
                let rows = statement
                    .query_map(rusqlite::params![conversation_id, seq], |row| {
                        Ok(HistoryOp {
                            ordinal: row.get(0)?,
                            op: row.get(1)?,
                            context_id: row.get(2)?,
                            position: row.get(3)?,
                            body: row.get(4)?,
                            before: None,
                        })
                    })
                    .map_err(|error| format!("无法查询时间线改动：{error}"))?;
                for row in rows {
                    ops.push(row.map_err(|error| format!("无法读取时间线改动行：{error}"))?);
                }
                // What a row said before this change is the body the chain last gave
                // it, so a rewrite can be drawn as a diff and a removal as what went.
                let mut previous = conn
                    .prepare(
                        "SELECT stored.body
                         FROM history_op op
                         JOIN history_blob stored
                           ON stored.conversation_id = op.conversation_id AND stored.hash = op.hash
                         WHERE op.conversation_id = ?1 AND op.context_id = ?2 AND op.seq < ?3
                           AND op.op IN ('insert', 'replace')
                         ORDER BY op.seq DESC, op.ordinal DESC LIMIT 1",
                    )
                    .map_err(|error| format!("无法查询时间线改动：{error}"))?;
                for op in &mut ops {
                    op.before = previous
                        .query_row(rusqlite::params![conversation_id, op.context_id, seq], |row| {
                            row.get::<_, String>(0)
                        })
                        .optional()
                        .map_err(|error| format!("无法查询时间线改动：{error}"))?;
                }
            }
            _ => {}
        }
        Ok(Some(HistoryEntryDetail {
            entry,
            body,
            truncated,
            parts,
            ops,
        }))
    }

    /// Appends one outgoing request to the history and returns the `seq` it was
    /// written under, which is the only handle the response has on this exact
    /// entry. `None` means the conversation has no row in the store, so nothing
    /// was written and nothing can be linked to it later.
    pub fn record_history_request(
        &self,
        record: &HistoryRequestRecord,
    ) -> Result<Option<i64>, String> {
        self.with_write_tx(|tx| record_history_request_tx(tx, record))
    }

    /// Appends one entry — a response, a hook decision, a call, a result — and
    /// returns its `seq`, or `None` when the conversation has no row in the store
    /// (a draft).
    pub fn record_history_entry(&self, record: &HistoryEntryRecord) -> Result<Option<i64>, String> {
        self.with_write_tx(|tx| record_history_entry_tx(tx, record))
    }

    /// Message bodies one owner's requests carried to the model, each distinct body once, in the
    /// order the model first received them. `owner` picks whose (`None` is the conversation's
    /// own), `role` keeps only messages of that wire role, and `needle` keeps only bodies
    /// containing that literal text. Bodies the size cap truncated are left out: they no longer
    /// parse.
    ///
    /// This answers "what did the model receive", which only a request can: a result the host
    /// produced reaches the model when a request carries it, and not before.
    pub fn history_message_bodies(
        &self,
        conversation_id: &str,
        owner: Option<&str>,
        role: Option<&str>,
        needle: &str,
    ) -> Result<Vec<String>, String> {
        let conn = self.lock()?;
        // `min()` picks, per body, the earliest (request, position) that carried it. Positions
        // within one request stay far below the multiplier.
        let mut statement = conn
            .prepare(
                "SELECT stored.body, min(part.seq * 1048576 + part.ordinal) AS first_seen
                 FROM history_blob stored
                 JOIN history_part part
                   ON part.conversation_id = stored.conversation_id AND part.hash = stored.hash
                 JOIN history_entry entry
                   ON entry.conversation_id = part.conversation_id AND entry.seq = part.seq
                 WHERE stored.conversation_id = ?1
                   AND stored.truncated = 0
                   AND instr(stored.body, ?4) > 0
                   AND part.kind = 'message'
                   AND (?3 IS NULL OR part.role = ?3)
                   AND entry.kind = 'request'
                   AND json_extract(entry.detail, '$.type') = 'model'
                   AND entry.owner IS ?2
                 GROUP BY stored.hash
                 ORDER BY first_seen",
            )
            .map_err(|error| format!("无法查询历史记录正文：{error}"))?;
        let rows = statement
            .query_map(
                rusqlite::params![conversation_id, owner, role, needle],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("无法查询历史记录正文：{error}"))?;
        let mut bodies = Vec::new();
        for row in rows {
            bodies.push(row.map_err(|error| format!("无法读取历史记录正文：{error}"))?);
        }
        Ok(bodies)
    }

    /// Entries read back as evidence, oldest first, with their bodies. See
    /// [`HistoryFilter`] for what selects them.
    ///
    /// This is the host's record of what happened. Recovery reads what an agent
    /// answered, what came after it, and what a call ran with here: the timeline is
    /// the user's to edit, and a request holds a response only once a later request
    /// replays it — which a crash can prevent — and then as the timeline projects it.
    pub fn history_records(
        &self,
        conversation_id: &str,
        filter: HistoryFilter<'_>,
    ) -> Result<Vec<HistoryRecord>, String> {
        let conn = self.lock()?;
        let kinds = if filter.kinds.is_empty() {
            String::new()
        } else {
            let slots = (0..filter.kinds.len())
                .map(|index| format!("?{}", index + 5))
                .collect::<Vec<_>>()
                .join(", ");
            format!("AND entry.kind IN ({slots})")
        };
        let mut statement = conn
            .prepare(&format!(
                "SELECT entry.seq, entry.kind, entry.request_id, entry.round, entry.call_id,
                        entry.answers, entry.detail, stored.body, stored.truncated
                 FROM history_entry entry
                 LEFT JOIN history_blob stored
                   ON stored.conversation_id = entry.conversation_id AND stored.hash = entry.hash
                 WHERE entry.conversation_id = ?1
                   AND entry.owner IS ?2
                   AND (?3 IS NULL OR entry.call_id = ?3)
                   AND (?4 = '' OR instr(stored.body, ?4) > 0)
                   {kinds}
                 ORDER BY entry.seq"
            ))
            .map_err(|error| format!("无法查询历史记录：{error}"))?;
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            vec![&conversation_id, &filter.owner, &filter.call_id, &filter.needle];
        for kind in filter.kinds {
            bound.push(kind);
        }
        let rows = statement
            .query_map(bound.as_slice(), |row| {
                Ok(HistoryRecord {
                    seq: row.get(0)?,
                    kind: row.get(1)?,
                    request_id: row.get(2)?,
                    round: row.get(3)?,
                    call_id: row.get(4)?,
                    answers: row.get(5)?,
                    detail: parse_detail(&row.get::<_, String>(6)?),
                    body: row.get(7)?,
                    truncated: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
                })
            })
            .map_err(|error| format!("无法查询历史记录：{error}"))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|error| format!("无法读取历史记录行：{error}"))?);
        }
        Ok(records)
    }

    /// The card that owns agent `name`'s run record on the main timeline: the newest card
    /// already holding a record of that name, else its `agent_spawn` card — found by the
    /// provider call id the spawn carried, then by the name it was given. A child that
    /// settles within its first round never had a record written, so its spawn card is the
    /// only place to put one.
    pub(crate) fn task_card_owner(
        &self,
        conversation_id: &str,
        name: &str,
        spawn_call_id: Option<&str>,
    ) -> Result<Option<String>, String> {
        let conn = self.lock()?;
        let find = |condition: &str, value: &str| -> Result<Option<String>, String> {
            conn.query_row(
                &format!(
                    "SELECT id FROM context
                     WHERE conversation_id = ?1 AND branch_id IS NULL AND json_valid(data)
                       AND {condition} = ?2
                     ORDER BY order_key DESC, rowid DESC LIMIT 1"
                ),
                rusqlite::params![conversation_id, value],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法查找子代理记录所在的卡片：{error}"))
        };
        if let Some(id) = find("json_extract(data, '$.subagent.name')", name)? {
            return Ok(Some(id));
        }
        let spawn = "json_extract(data, '$.toolName') = 'agent_spawn' AND ";
        if let Some(call_id) = spawn_call_id {
            if let Some(id) = find(&format!("{spawn}json_extract(data, '$.providerCallId')"), call_id)? {
                return Ok(Some(id));
            }
        }
        find(&format!("{spawn}json_extract(data, '$.input.name')"), name)
    }

    // ---------------------------------------------------------------- Write

    /// Replaces a complete conversation for creation, forking, explicit renderer edits, or load-time seeding.
    /// Contexts are reindexed in supplied order; replacement runs only when no run is active.
    pub fn put_conversation(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
    ) -> Result<(), String> {
        let written = self.with_write_tx(|tx| {
            put_conversation_tx(tx, workspace_id, conversation, NewRowAt::End)
        });
        self.bodies.invalidate(&conversation.id);
        written
    }

    /// Atomically persists the child and its explicit first-run intent. The child
    /// opens its workspace's list, where a conversation the user starts goes.
    pub fn put_fork_conversation(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
        prompt_context_id: &str,
    ) -> Result<(), String> {
        let written = self.with_write_tx(|tx| {
            if !matches!(conversation.contexts.last(), Some(ContextItem::User { id, .. }) if id == prompt_context_id) {
                return Err("分叉首轮提示标识无效".into());
            }
            put_conversation_tx(tx, workspace_id, conversation, NewRowAt::Top)?;
            tx.execute("INSERT INTO pending_fork_start (conversation_id, prompt_context_id) VALUES (?1, ?2)",
                [&conversation.id, prompt_context_id]).map_err(|error| error.to_string())?;
            Ok(())
        });
        self.bodies.invalidate(&conversation.id);
        written
    }

    pub fn pending_fork_starts(&self) -> Result<Vec<PendingForkStart>, String> {
        let conn = self.lock()?;
        let mut query = conn.prepare("SELECT c.workspace_id, p.conversation_id, p.prompt_context_id
            FROM pending_fork_start p JOIN conversation c ON c.id = p.conversation_id ORDER BY c.created_at, c.id")
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok(PendingForkStart {
                    workspace_id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    prompt_context_id: row.get(2)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    /// Called only after validation and while holding the host's conversation run lease.
    /// The callback establishes a resumable run before the intent is acknowledged.
    pub fn accept_fork_start<T>(
        &self,
        conversation_id: &str,
        expected_prompt: Option<&str>,
        establish: impl FnOnce() -> T,
    ) -> Result<T, String> {
        self.with_write_tx(|tx| {
            let prompt: Option<String> = tx.query_row(
                "SELECT prompt_context_id FROM pending_fork_start WHERE conversation_id = ?1",
                [conversation_id], |row| row.get(0)).optional().map_err(|error| error.to_string())?;
            if let Some(expected) = expected_prompt {
                if prompt.as_deref() != Some(expected) {
                    return Err("分叉首轮已启动或待启动标识无效".into());
                }
                let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM context WHERE conversation_id = ?1 AND id = ?2 AND kind = 'user')",
                    [conversation_id, expected], |row| row.get(0)).map_err(|error| error.to_string())?;
                if !valid { return Err("分叉首轮提示已不存在".into()); }
            }
            // A manual first send consumes the same intent; stopping it must not auto-restart it.
            tx.execute("DELETE FROM pending_fork_start WHERE conversation_id = ?1", [conversation_id])
                .map_err(|error| error.to_string())?;
            Ok(establish())
        })
    }

    /// The conversation's plan document, or `None` when none was ever written.
    pub fn conversation_plan(
        &self,
        conversation_id: &str,
    ) -> Result<Option<crate::model::ConversationPlan>, String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT markdown, status, created_at, updated_at FROM conversation_plan WHERE conversation_id = ?1",
            [conversation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取计划文档：{error}"))?
        .map(|(markdown, status, created_at, updated_at)| {
            // The column carries a CHECK constraint, so an unreadable status
            // means the row was written by something other than this code.
            let status = crate::model::PlanStatus::from_str(&status)
                .ok_or_else(|| format!("计划文档状态 {status} 无法识别"))?;
            Ok(crate::model::ConversationPlan {
                conversation_id: conversation_id.to_owned(),
                markdown,
                status,
                created_at,
                updated_at,
            })
        })
        .transpose()
    }

    /// Replaces the conversation's plan document wholesale.
    pub fn put_conversation_plan(
        &self,
        plan: &crate::model::ConversationPlan,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO conversation_plan (conversation_id, markdown, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                     markdown = excluded.markdown,
                     status = excluded.status,
                     updated_at = excluded.updated_at",
                rusqlite::params![
                    &plan.conversation_id,
                    &plan.markdown,
                    plan.status.as_str(),
                    &plan.created_at,
                    &plan.updated_at,
                ],
            )
            .map_err(|error| format!("无法写入计划文档：{error}"))?;
            Ok(())
        })
    }

    /// Moves an existing plan between draft, approved and rejected. Absent when
    /// no plan was written, which the caller has already refused to act on.
    pub fn set_conversation_plan_status(
        &self,
        conversation_id: &str,
        status: crate::model::PlanStatus,
        updated_at: &str,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            tx.execute(
                "UPDATE conversation_plan SET status = ?2, updated_at = ?3 WHERE conversation_id = ?1",
                rusqlite::params![conversation_id, status.as_str(), updated_at],
            )
            .map_err(|error| format!("无法更新计划文档状态：{error}"))?;
            Ok(())
        })
    }

    /// Records one answered fork request.
    ///
    /// Keyed by `fork_id` and idempotent: a card answers once, but a replay of
    /// the same decision must not put a second row in the task bar.
    pub fn record_fork_decision(
        &self,
        record: &crate::fork_requests::ForkDecisionRecord,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO fork_decision
                     (fork_id, source_conversation_id, workspace_id, title, prompt,
                      requested_at, decided_at, approved, child_conversation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(fork_id) DO UPDATE SET
                     decided_at = excluded.decided_at,
                     approved = excluded.approved,
                     child_conversation_id = excluded.child_conversation_id",
                rusqlite::params![
                    &record.fork_id,
                    &record.source_conversation_id,
                    &record.workspace_id,
                    &record.title,
                    &record.prompt,
                    &record.requested_at,
                    &record.decided_at,
                    record.approved,
                    &record.child_conversation_id,
                ],
            )
            .map_err(|error| format!("无法记录分叉决定：{error}"))?;
            Ok(())
        })
    }

    /// Every fork decision this conversation raised, oldest first.
    pub fn fork_decisions(
        &self,
        source_conversation_id: &str,
    ) -> Result<Vec<crate::fork_requests::ForkDecisionRecord>, String> {
        let conn = self.lock()?;
        let mut query = conn
            .prepare(
                "SELECT fork_id, workspace_id, title, prompt,
                        requested_at, decided_at, approved, child_conversation_id
                 FROM fork_decision WHERE source_conversation_id = ?1
                 ORDER BY decided_at, fork_id",
            )
            .map_err(|error| format!("无法读取分叉决定：{error}"))?;
        let rows = query
            .query_map([source_conversation_id], |row| {
                Ok(crate::fork_requests::ForkDecisionRecord {
                    fork_id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    source_conversation_id: source_conversation_id.to_owned(),
                    title: row.get(2)?,
                    prompt: row.get(3)?,
                    requested_at: row.get(4)?,
                    decided_at: row.get(5)?,
                    approved: row.get(6)?,
                    child_conversation_id: row.get(7)?,
                })
            })
            .map_err(|error| format!("无法读取分叉决定：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取分叉决定：{error}"))
    }

    /// Writes a conversation's own row, queued messages and aborted-task records, leaving every
    /// context and branch row exactly as it is. This is the only write a renderer edit may make
    /// while a run is producing the timeline: [`Self::put_conversation`] would clear the context
    /// table and re-insert a snapshot, and a card the run persisted between that snapshot being
    /// read and being written back would be gone for good — its writer records a fingerprint on
    /// success and never writes it again.
    /// Whether the conversation's title is final (see the `title_settled` column).
    pub fn title_settled(&self, conversation_id: &str) -> Result<bool, String> {
        let conn = self.lock()?;
        conn.query_row("SELECT title_settled FROM conversation WHERE id = ?1", [conversation_id], |row| row.get::<_, i64>(0))
            .optional()
            .map(|value| value.unwrap_or(0) != 0)
            .map_err(|error| format!("无法读取对话标题状态：{error}"))
    }

    pub fn set_title_settled(&self, conversation_id: &str, settled: bool) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute("UPDATE conversation SET title_settled = ?2 WHERE id = ?1", rusqlite::params![conversation_id, settled as i64])
            .map(|_| ())
            .map_err(|error| format!("无法写入对话标题状态：{error}"))
    }

    pub fn put_tool_explanation(&self, conversation_id: &str, context_id: &str, text: &str) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tool_explanation (conversation_id, context_id, text) VALUES (?1, ?2, ?3)
             ON CONFLICT (conversation_id, context_id) DO UPDATE SET text = excluded.text",
            rusqlite::params![conversation_id, context_id, text],
        )
        .map(|_| ())
        .map_err(|error| format!("无法写入命令说明：{error}"))
    }

    pub fn put_tool_error_explanation(&self, conversation_id: &str, context_id: &str, text: &str) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tool_error_explanation (conversation_id, context_id, text) VALUES (?1, ?2, ?3)
             ON CONFLICT (conversation_id, context_id) DO UPDATE SET text = excluded.text",
            rusqlite::params![conversation_id, context_id, text],
        )
        .map(|_| ())
        .map_err(|error| format!("无法写入错误解释：{error}"))
    }

    /// Descriptions by tool card id.
    pub fn tool_explanations(&self, conversation_id: &str) -> Result<HashMap<String, String>, String> {
        self.explanations_in("tool_explanation", conversation_id)
    }

    /// Why each failed call failed, by tool card id.
    pub fn tool_error_explanations(&self, conversation_id: &str) -> Result<HashMap<String, String>, String> {
        self.explanations_in("tool_error_explanation", conversation_id)
    }

    fn explanations_in(&self, table: &str, conversation_id: &str) -> Result<HashMap<String, String>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(&format!("SELECT context_id, text FROM {table} WHERE conversation_id = ?1"))
            .map_err(|error| format!("无法读取工具卡说明：{error}"))?;
        let rows = statement
            .query_map([conversation_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|error| format!("无法读取工具卡说明：{error}"))?;
        rows.collect::<Result<HashMap<_, _>, _>>().map_err(|error| format!("无法读取工具卡说明：{error}"))
    }

    pub fn put_conversation_metadata(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
    ) -> Result<(), String> {
        let written = self.with_write_tx(|tx| {
            put_conversation_row_tx(tx, workspace_id, conversation, NewRowAt::End)?;
            replace_queued_messages_tx(tx, conversation)?;
            replace_aborted_tasks_tx(tx, conversation)
        });
        self.bodies.invalidate(&conversation.id);
        written
    }

    /// Deletes a conversation and its dependent rows, re-parenting children to its parent.
    pub fn delete_conversation(&self, conversation_id: &str) -> Result<(), String> {
        // Children are re-parented, so their bodies change too.
        let deleted = self.with_write_tx(|tx| {
            tx.execute(
                "UPDATE conversation SET parent_conversation_id =
                 (SELECT parent_conversation_id FROM conversation WHERE id = ?1)
                 WHERE parent_conversation_id = ?1",
                [conversation_id],
            )
            .map_err(|error| format!("无法更新子对话归属：{error}"))?;
            tx.execute("DELETE FROM conversation WHERE id = ?1", [conversation_id])
                .map_err(|error| format!("无法删除对话：{error}"))?;
            Ok(())
        });
        self.bodies.invalidate_all();
        deleted
    }

    /// Reorders a workspace's conversations. The caller moves or deletes omitted conversations;
    /// this operation changes only their `order_key` and workspace ownership.
    pub fn set_workspace_order(
        &self,
        workspace_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), String> {
        // Parents left behind in another workspace are cleared, which changes
        // bodies beyond the ones named.
        let reordered = self.with_write_tx(|tx| {
            for (index, id) in ordered_ids.iter().enumerate() {
                tx.execute(
                    "UPDATE conversation SET workspace_id = ?1, order_key = ?2 WHERE id = ?3",
                    rusqlite::params![workspace_id, index as f64 * ORDER_STEP, id],
                )
                .map_err(|error| format!("无法重排对话：{error}"))?;
            }
            // Judge final ownership after the entire batch, preserving parents
            // and children moved together. Do not implicitly move descendants.
            for id in ordered_ids {
                tx.execute(
                    "UPDATE conversation SET parent_conversation_id = NULL
                     WHERE (id = ?1 OR parent_conversation_id = ?1)
                       AND EXISTS (SELECT 1 FROM conversation AS parent
                         WHERE parent.id = conversation.parent_conversation_id
                           AND parent.workspace_id != conversation.workspace_id)",
                    [id],
                )
                .map_err(|error| format!("无法更新跨工作区父对话：{error}"))?;
            }
            Ok(())
        });
        self.bodies.invalidate_all();
        reordered
    }

    /// Appends or updates contexts in place. New rows use the current maximum ordering key;
    /// existing rows update their body and status.
    pub fn upsert_contexts(
        &self,
        conversation_id: &str,
        items: &[ContextItem],
        status: ContextStatus,
    ) -> Result<(), String> {
        self.upsert_contexts_in_sequence(conversation_id, items, status, &[])
    }

    /// Like [`Self::upsert_contexts`], but keeps the rows named by `sequence` in that relative
    /// order. `sequence` is a contiguous stretch of a run's canonical output; it may name rows
    /// that are not being written now, and ids with no row yet are skipped.
    ///
    /// A new row named by the sequence is placed directly after its nearest persisted
    /// predecessor in the sequence rather than at the end, and a persisted row found behind that
    /// predecessor is moved up behind it. Insertion order is not the run's order: streaming prose
    /// is appended as it arrives, while the round's reasoning cards are only minted once the
    /// provider stream has ended. Items the sequence does not name are appended as before.
    pub fn upsert_contexts_in_sequence(
        &self,
        conversation_id: &str,
        items: &[ContextItem],
        status: ContextStatus,
        sequence: &[&str],
    ) -> Result<(), String> {
        if items.is_empty() {
            return Ok(());
        }
        let written = self.with_write_tx(|tx| {
            if !conversation_exists(tx, conversation_id)? {
                // Subagent and temporary conversations have no row; skip writes without requiring callers to classify the run.
                return Ok(());
            }
            let mut pending: HashMap<&str, &ContextItem> =
                items.iter().map(|item| (item.id(), item)).collect();
            let mut placed = std::collections::HashSet::new();
            let mut floor: Option<f64> = None;
            for id in sequence {
                if !placed.insert(*id) {
                    continue;
                }
                let existing = context_order_key_tx(tx, conversation_id, id)?;
                let key = match (pending.remove(id), existing) {
                    (Some(item), Some(key)) => {
                        upsert_context_tx(tx, conversation_id, None, item, status, key)?;
                        key
                    }
                    (Some(item), None) => {
                        let key = match floor {
                            Some(floor) => order_key_after_tx(tx, conversation_id, floor)?,
                            None => next_order_key(tx, conversation_id, None)?,
                        };
                        if !upsert_context_tx(tx, conversation_id, None, item, status, key)? {
                            // The id lives on a branch; the trunk order does not involve it.
                            continue;
                        }
                        key
                    }
                    (None, Some(key)) => key,
                    (None, None) => continue,
                };
                let key = match floor {
                    Some(floor) if key <= floor => {
                        let moved = order_key_after_tx(tx, conversation_id, floor)?;
                        tx.execute(
                            "UPDATE context SET order_key = ?1 WHERE conversation_id = ?2 AND id = ?3",
                            rusqlite::params![moved, conversation_id, id],
                        )
                        .map_err(|error| format!("无法调整上下文顺序：{error}"))?;
                        moved
                    }
                    _ => key,
                };
                floor = Some(key);
            }
            let mut next = next_order_key(tx, conversation_id, None)?;
            for item in items {
                if pending.remove(item.id()).is_none() {
                    continue;
                }
                let appended = upsert_context_tx(tx, conversation_id, None, item, status, next)?;
                if appended {
                    next += ORDER_STEP;
                }
            }
            touch_conversation_tx(tx, conversation_id)?;
            Ok(())
        });
        self.bodies.invalidate(conversation_id);
        written
    }

    /// Atomically revise a task's current card and its superseded holders,
    /// preserving row positions, branch ownership and settlement status. Missing
    /// current cards are retryable and must not destroy an older recovery copy.
    pub(crate) fn update_task_cards_in_place(
        &self,
        conversation_id: &str,
        context_id: &str,
        task_name: &str,
        mut revise: impl FnMut(&mut ContextItem) -> bool,
    ) -> Result<bool, String> {
        let revised = self.with_write_tx(|tx| {
            let data: Option<String> = tx.query_row(
                "SELECT data FROM context WHERE conversation_id = ?1 AND id = ?2 AND branch_id IS NULL",
                rusqlite::params![conversation_id, context_id],
                |row| row.get(0),
            ).optional().map_err(|error| format!("Could not read transcript owner card: {error}"))?;
            let Some(data) = data else { return Ok(false); };
            let mut item: ContextItem = serde_json::from_str(&data)
                .map_err(|error| format!("Could not decode transcript owner card: {error}"))?;
            if !revise(&mut item) { return Ok(false); }
            if item.id() != context_id {
                return Err("A context update cannot replace its row identity.".into());
            }
            let mut updates = vec![item];
            let stale_rows = {
                let mut statement = tx.prepare(
                    "SELECT data FROM context WHERE conversation_id = ?1 AND id != ?2 AND branch_id IS NULL
                     AND CASE WHEN json_valid(data) THEN json_extract(data, '$.subagent.name') ELSE NULL END = ?3",
                ).map_err(|error| format!("Could not locate superseded task cards: {error}"))?;
                let rows = statement.query_map(rusqlite::params![conversation_id, context_id, task_name],
                    |row| row.get::<_, String>(0))
                    .map_err(|error| format!("Could not read superseded task cards: {error}"))?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("Could not collect superseded task cards: {error}"))?
            };
            for data in stale_rows {
                let mut stale: ContextItem = serde_json::from_str(&data)
                    .map_err(|error| format!("Could not decode superseded task card: {error}"))?;
                let id = stale.id().to_owned();
                if !revise(&mut stale) || stale.id() != id {
                    return Err("Could not revise a superseded task card without changing its identity.".into());
                }
                updates.push(stale);
            }
            for item in updates {
                let data = serde_json::to_string(&item)
                    .map_err(|error| format!("Could not encode transcript owner card: {error}"))?;
                tx.execute(
                    "UPDATE context SET data = ?1, updated_at = ?2 WHERE conversation_id = ?3 AND id = ?4",
                    rusqlite::params![data, chrono::Utc::now().to_rfc3339(), conversation_id, item.id()],
                ).map_err(|error| format!("Could not persist transcript owner card: {error}"))?;
            }
            touch_conversation_tx(tx, conversation_id)?;
            Ok(true)
        });
        self.bodies.invalidate(conversation_id);
        revised
    }

    /// Folds committed WAL frames into the main database and fsyncs them. Use only for writes that
    /// must survive an operating-system crash; normal WAL writes already survive process death.
    pub fn flush_durable(&self) -> Result<(), String> {
        let conn = self.lock()?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|error| format!("对话库 WAL 检查点失败：{error}"))
    }

    /// Test helper that corrupts a conversation's first context row.
    #[cfg(test)]
    pub(crate) fn corrupt_context_for_test(&self, conversation_id: &str) -> Result<(), String> {
        let written = self.with_write_tx(|tx| {
            tx.execute(
                "UPDATE context SET data = '{ not json' WHERE conversation_id = ?1
                 AND id = (SELECT id FROM context WHERE conversation_id = ?1
                           ORDER BY order_key, rowid LIMIT 1)",
                [conversation_id],
            )
            .map_err(|error| format!("无法制造坏行：{error}"))?;
            Ok(())
        });
        self.bodies.invalidate(conversation_id);
        written
    }

    /// Startup recovery marks streaming rows left by the previous process as interrupted.
    /// It must run before any new run starts, or it would mark newly created placeholders too.
    pub fn reconcile_streaming(&self) -> Result<usize, String> {
        self.reconcile_streaming_where(None)
    }

    /// Recovers streaming rows for one conversation when its run settles. Finalized contexts replace
    /// streaming rows in place; unfinished prose is marked interrupted immediately.
    pub fn reconcile_streaming_in(&self, conversation_id: &str) -> Result<usize, String> {
        self.reconcile_streaming_where(Some(conversation_id))
    }

    /// Deletes the named rows while they are still `streaming`, returning how many went. A settled
    /// row with one of these ids is left alone: only prose that never became canonical is
    /// disposable, and the caller may not know whether settlement has already claimed the id.
    pub fn discard_streaming_contexts(
        &self,
        conversation_id: &str,
        ids: &[&str],
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        let discarded = self.with_write_tx(|tx| {
            let mut removed = 0usize;
            for id in ids {
                removed += tx
                    .execute(
                        "DELETE FROM context WHERE conversation_id = ?1 AND id = ?2
                         AND status = 'streaming'",
                        rusqlite::params![conversation_id, id],
                    )
                    .map_err(|error| format!("无法丢弃未定稿上下文：{error}"))?;
            }
            Ok(removed)
        });
        self.bodies.invalidate(conversation_id);
        discarded
    }

    fn reconcile_streaming_where(&self, conversation_id: Option<&str>) -> Result<usize, String> {
        let reconciled = self.with_write_tx(|tx| {
            let stale: Vec<(String, String, String)> = {
                let mut statement = tx
                    .prepare(
                        "SELECT conversation_id, id, data FROM context
                         WHERE status = 'streaming' AND (?1 IS NULL OR conversation_id = ?1)",
                    )
                    .map_err(|error| format!("无法查询未定稿上下文：{error}"))?;
                let rows = statement
                    .query_map([conversation_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(|error| format!("无法查询未定稿上下文：{error}"))?;
                let mut stale = Vec::new();
                for row in rows {
                    stale.push(row.map_err(|error| format!("无法读取未定稿上下文：{error}"))?);
                }
                stale
            };
            let count = stale.len();
            for (conversation_id, id, data) in stale {
                let marked = mark_interrupted(&data)?;
                tx.execute(
                    "UPDATE context SET status = 'settled', data = ?1, updated_at = ?2
                     WHERE conversation_id = ?3 AND id = ?4",
                    rusqlite::params![marked, now(), conversation_id, id],
                )
                .map_err(|error| format!("无法回收未定稿上下文：{error}"))?;
            }
            Ok(count)
        });
        match conversation_id {
            Some(conversation_id) => self.bodies.invalidate(conversation_id),
            None => self.bodies.invalidate_all(),
        }
        reconciled
    }

    /// Removes queued messages once they enter a turn, so durable queued rows cannot coexist with
    /// messages already seen by the model.
    pub fn remove_queued_messages(
        &self,
        conversation_id: &str,
        ids: &[String],
    ) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let removed = self.with_write_tx(|tx| {
            for id in ids {
                tx.execute(
                    "DELETE FROM queued_message WHERE conversation_id = ?1 AND id = ?2",
                    rusqlite::params![conversation_id, id],
                )
                .map_err(|error| format!("无法删除排队消息：{error}"))?;
            }
            Ok(())
        });
        self.bodies.invalidate(conversation_id);
        removed
    }
}

// -------------------------------------------------------------------- Internal

fn open_configured(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|error| format!("无法打开对话库：{error}"))?;
    // WAL permits concurrent reads and writes. `NORMAL` preserves committed transactions after
    // process death, though not after power loss.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| format!("无法启用 WAL：{error}"))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| format!("无法设置 synchronous：{error}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| format!("无法启用外键：{error}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| format!("无法设置 busy_timeout：{error}"))?;
    Ok(conn)
}

/// Table names as the creation statements themselves declare them, so the list and
/// the statements cannot drift: a table added to a schema constant is a table this
/// reports, with nothing to keep in step by hand.
fn declared_tables() -> impl Iterator<Item = &'static str> {
    const PREFIX: &str = "CREATE TABLE IF NOT EXISTS ";
    SCHEMAS.into_iter().flat_map(|schema| {
        schema
            .split(PREFIX)
            .skip(1)
            .filter_map(|rest| rest.split_whitespace().next())
    })
}

/// Whether the store already holds everything this build expects of it: every
/// declared table, every column added after its table shipped, and none of the
/// retired ones.
///
/// This is what lets a healthy open stay read-only. It asks the schema and not the
/// version stamp, so answering "yes" is a statement about the database rather than
/// about a number written into it.
fn shape_is_current(conn: &Connection) -> Result<bool, String> {
    for table in declared_tables() {
        if !has_table(conn, table)? {
            return Ok(false);
        }
    }
    for &(table, column, _) in ADDED_COLUMNS {
        if !has_column(conn, table, column)? {
            return Ok(false);
        }
    }
    for table in LEGACY_HISTORY_TABLES {
        // A legacy table still standing is history not yet migrated.
        if has_table(conn, table)? {
            return Ok(false);
        }
    }
    Ok(!has_column(conn, "fork_decision", "inherit_context")?)
}

fn has_table(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .and_then(|mut statement| statement.exists([table]))
        .map_err(|error| format!("无法检查对话库结构：{error}"))
}

/// Whether a table already carries a column, read from the store itself.
///
/// Repair asks this and never the version stamp: the stamp says what a store was
/// last reconciled against, the schema says what it actually holds, and only the
/// second one can decide an `ALTER`. The table name is a constant from this file
/// rather than anything a caller supplies, so it is written into the statement;
/// only the column name is bound.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    conn.prepare(&format!(
        "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
    ))
    .and_then(|mut statement| statement.exists([column]))
    .map_err(|error| format!("无法检查对话库结构：{error}"))
}

fn quarantine_database(db_path: &Path, reason: &str) {
    let aux = |base: &Path, suffix: &str| {
        let mut path = base.as_os_str().to_owned();
        path.push(suffix);
        PathBuf::from(path)
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let quarantined = db_path.with_extension(format!("quarantine-{stamp}.sqlite3"));
    if let Err(error) = std::fs::rename(db_path, &quarantined) {
        // Nothing was set aside, so nothing beside it may be removed either:
        // deleting the log of a database still sitting at `db_path` would throw
        // away the history this call exists to keep. The caller reopens the same
        // file, meets the same refusal, and reports it instead of losing it.
        eprintln!("对话库无法封存（{reason}）：{error}");
        return;
    }
    // The write-ahead log travels with the file it belongs to. A store in WAL mode
    // that has never been checkpointed holds nearly everything in the log and
    // almost nothing in the main file, so leaving the log behind would set aside an
    // empty shell and destroy exactly the history the rename was meant to preserve.
    // SQLite finds a log by name, so it has to be renamed to match. The shared
    // memory file is rebuilt from the log on the next open and is simply dropped.
    let _ = std::fs::rename(aux(db_path, "-wal"), aux(&quarantined, "-wal"));
    let _ = std::fs::remove_file(aux(db_path, "-shm"));
    eprintln!("对话库已封存（{reason}）：{}", quarantined.display());
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn conversation_exists(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<bool, String> {
    let found: Option<i64> = tx
        .query_row("SELECT 1 FROM conversation WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| format!("无法检查对话是否存在：{error}"))?;
    Ok(found.is_some())
}

/// Rewrites a template's body inside `tx`: every existing row is dropped and the
/// submitted items are inserted in their own order, so what reads back is exactly
/// what was written. Shared by the two writers of a template body — creation and
/// the renderer's edit — which must never disagree about ordering.
fn write_template_body(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    contexts: &[ContextItem],
) -> Result<(), String> {
    tx.execute("DELETE FROM template_context WHERE template_id = ?1", [id])
        .map_err(|error| format!("无法清空模板正文：{error}"))?;
    for (index, item) in contexts.iter().enumerate() {
        let data =
            serde_json::to_string(item).map_err(|error| format!("模板正文无法序列化：{error}"))?;
        tx.execute(
            "INSERT INTO template_context (template_id, id, order_key, data)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, item.id(), index as f64 * ORDER_STEP, data],
        )
        .map_err(|error| format!("无法写入模板正文：{error}"))?;
    }
    Ok(())
}

fn touch_conversation_tx(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), String> {
    tx.execute(
        "UPDATE conversation SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now(), id],
    )
    .map_err(|error| format!("无法更新对话时间戳：{error}"))?;
    Ok(())
}

fn next_order_key(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    branch_id: Option<&str>,
) -> Result<f64, String> {
    let max: Option<f64> = match branch_id {
        Some(branch) => tx
            .query_row(
                "SELECT max(order_key) FROM context WHERE conversation_id = ?1 AND branch_id = ?2",
                rusqlite::params![conversation_id, branch],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法读取排序键：{error}"))?
            .flatten(),
        None => tx
            .query_row(
                "SELECT max(order_key) FROM context WHERE conversation_id = ?1 AND branch_id IS NULL",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法读取排序键：{error}"))?
            .flatten(),
    };
    Ok(max.map(|value| value + ORDER_STEP).unwrap_or(0.0))
}

/// Ordering key of a main-timeline row, `None` for an id with no such row.
fn context_order_key_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    id: &str,
) -> Result<Option<f64>, String> {
    tx.query_row(
        "SELECT order_key FROM context
         WHERE conversation_id = ?1 AND id = ?2 AND branch_id IS NULL",
        rusqlite::params![conversation_id, id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|error| format!("无法读取排序键：{error}"))
}

/// A key that sorts directly after `floor` on the main timeline: the midpoint to the following
/// row, or one step past `floor` when nothing follows. Once a gap has no representable midpoint
/// left, the rows from the following one onwards are reindexed one step apart, in their current
/// order, to reopen it. Reindexing rather than translating them: adding a step to keys that
/// dense can round two of them together and let `rowid` decide their order.
fn order_key_after_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    floor: f64,
) -> Result<f64, String> {
    let following: Option<f64> = tx
        .query_row(
            "SELECT min(order_key) FROM context
             WHERE conversation_id = ?1 AND branch_id IS NULL AND order_key > ?2",
            rusqlite::params![conversation_id, floor],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取排序键：{error}"))?
        .flatten();
    let Some(following) = following else {
        return Ok(floor + ORDER_STEP);
    };
    let midpoint = floor + (following - floor) / 2.0;
    if floor < midpoint && midpoint < following {
        return Ok(midpoint);
    }
    let suffix: Vec<i64> = {
        let mut statement = tx
            .prepare(
                "SELECT rowid FROM context
                 WHERE conversation_id = ?1 AND branch_id IS NULL AND order_key >= ?2
                 ORDER BY order_key, rowid",
            )
            .map_err(|error| format!("无法读取排序键：{error}"))?;
        let rows = statement
            .query_map(rusqlite::params![conversation_id, following], |row| {
                row.get(0)
            })
            .map_err(|error| format!("无法读取排序键：{error}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|error| format!("无法读取排序键：{error}"))?
    };
    for (index, rowid) in suffix.into_iter().enumerate() {
        tx.execute(
            "UPDATE context SET order_key = ?1 WHERE rowid = ?2",
            rusqlite::params![following + (index as f64 + 1.0) * ORDER_STEP, rowid],
        )
        .map_err(|error| format!("无法腾出排序键：{error}"))?;
    }
    Ok(following)
}

/// Writes one context row and returns whether it was appended, requiring the caller to advance its ordering key.
fn upsert_context_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    branch_id: Option<&str>,
    item: &ContextItem,
    status: ContextStatus,
    order_key: f64,
) -> Result<bool, String> {
    let data = serde_json::to_string(item).map_err(|error| format!("上下文无法序列化：{error}"))?;
    let existing: Option<f64> = tx
        .query_row(
            "SELECT order_key FROM context WHERE conversation_id = ?1 AND id = ?2",
            rusqlite::params![conversation_id, item.id()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取上下文：{error}"))?;
    if existing.is_some() {
        tx.execute(
            "UPDATE context SET data = ?1, status = ?2, round = ?3, model_turn_id = ?4,
             updated_at = ?5 WHERE conversation_id = ?6 AND id = ?7",
            rusqlite::params![
                data,
                status.as_str(),
                item.round().map(|round| round as i64),
                item.model_turn_id(),
                now(),
                conversation_id,
                item.id(),
            ],
        )
        .map_err(|error| format!("无法更新上下文：{error}"))?;
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO context (conversation_id, id, branch_id, order_key, kind, status,
         round, model_turn_id, data, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            conversation_id,
            item.id(),
            branch_id,
            order_key,
            item.kind_str(),
            status.as_str(),
            item.round().map(|round| round as i64),
            item.model_turn_id(),
            data,
            item.created_at(),
            now(),
        ],
    )
    .map_err(|error| format!("无法写入上下文：{error}"))?;
    Ok(true)
}

/// The trunk in timeline order, as history compares it.
fn trunk_rows_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
) -> Result<Vec<TimelineRow>, String> {
    let mut statement = tx
        .prepare(
            "SELECT id, data FROM context
             WHERE conversation_id = ?1 AND branch_id IS NULL ORDER BY order_key, rowid",
        )
        .map_err(|error| format!("无法查询时间线正文：{error}"))?;
    let rows = statement
        .query_map([conversation_id], |row| {
            Ok(TimelineRow {
                id: row.get(0)?,
                data: row.get(1)?,
            })
        })
        .map_err(|error| format!("无法查询时间线正文：{error}"))?;
    let mut trunk = Vec::new();
    for row in rows {
        trunk.push(row.map_err(|error| format!("无法读取时间线正文行：{error}"))?);
    }
    Ok(trunk)
}

/// The trunk as of the newest recorded change.
fn history_head_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
) -> Result<Vec<TimelineRow>, String> {
    let mut statement = tx
        .prepare(
            "SELECT context_id, data FROM history_head
             WHERE conversation_id = ?1 ORDER BY position",
        )
        .map_err(|error| format!("无法查询时间线历史头：{error}"))?;
    let rows = statement
        .query_map([conversation_id], |row| {
            Ok(TimelineRow {
                id: row.get(0)?,
                data: row.get(1)?,
            })
        })
        .map_err(|error| format!("无法查询时间线历史头：{error}"))?;
    let mut head = Vec::new();
    for row in rows {
        head.push(row.map_err(|error| format!("无法读取时间线历史头行：{error}"))?);
    }
    Ok(head)
}

/// Ids that keep their place between two trunks, by longest common subsequence.
/// Rows outside it are re-inserted rather than moved, so a reordered timeline
/// replays into the order it actually had.
fn stable_ids<'a>(head: &'a [TimelineRow], current: &'a [TimelineRow]) -> HashSet<&'a str> {
    let mut stable = HashSet::new();
    let prefix = head
        .iter()
        .zip(current)
        .take_while(|(left, right)| left.id == right.id && left.data == right.data)
        .map(|(left, _)| {
            stable.insert(left.id.as_str());
        })
        .count();
    let remaining = head.len().min(current.len()) - prefix;
    let suffix = (0..remaining)
        .take_while(|offset| {
            let left = &head[head.len() - 1 - offset];
            let right = &current[current.len() - 1 - offset];
            left.id == right.id && left.data == right.data
        })
        .map(|offset| {
            stable.insert(head[head.len() - 1 - offset].id.as_str());
        })
        .count();
    let head = &head[prefix..head.len() - suffix];
    let current = &current[prefix..current.len() - suffix];
    // The trimmed middle is normally a handful of rows. A pathological one would
    // make the table below cost more than the history is worth, and treating it as
    // wholly rewritten stays correct — only more verbose.
    if head.len() * current.len() > 1_000_000 {
        return stable;
    }
    let mut table = vec![0u32; (head.len() + 1) * (current.len() + 1)];
    let stride = current.len() + 1;
    for row in (0..head.len()).rev() {
        for column in (0..current.len()).rev() {
            table[row * stride + column] = if head[row].id == current[column].id {
                table[(row + 1) * stride + column + 1] + 1
            } else {
                table[(row + 1) * stride + column].max(table[row * stride + column + 1])
            };
        }
    }
    let (mut row, mut column) = (0, 0);
    while row < head.len() && column < current.len() {
        if head[row].id == current[column].id {
            stable.insert(head[row].id.as_str());
            row += 1;
            column += 1;
        } else if table[(row + 1) * stride + column] >= table[row * stride + column + 1] {
            row += 1;
        } else {
            column += 1;
        }
    }
    stable
}

/// Emitted in replay order: removals, then insertions by ascending final index,
/// then in-place bodies. Replaying that sequence over the previous trunk rebuilds
/// `current` exactly, so an insertion can name its final position directly.
fn diff_timeline(head: &[TimelineRow], current: &[TimelineRow]) -> Vec<TimelineOp> {
    let stable = stable_ids(head, current);
    let mut ops = Vec::new();
    for row in head {
        if !stable.contains(row.id.as_str()) {
            ops.push(TimelineOp::Remove {
                context_id: row.id.clone(),
            });
        }
    }
    for (position, row) in current.iter().enumerate() {
        if !stable.contains(row.id.as_str()) {
            ops.push(TimelineOp::Insert {
                context_id: row.id.clone(),
                position: position as i64,
                data: row.data.clone(),
            });
        }
    }
    let bodies: HashMap<&str, &str> = head
        .iter()
        .map(|row| (row.id.as_str(), row.data.as_str()))
        .collect();
    for row in current {
        if stable.contains(row.id.as_str())
            && bodies.get(row.id.as_str()) != Some(&row.data.as_str())
        {
            ops.push(TimelineOp::Replace {
                context_id: row.id.clone(),
                data: row.data.clone(),
            });
        }
    }
    ops
}

/// Whether every step of a change only appended rows the user typed: inserts at
/// the tail of the trunk, each of a `user` row. That is sending a message, not
/// editing the context, and the pane draws the two differently.
fn only_appends_user_messages(head: &[TimelineRow], ops: &[TimelineOp]) -> bool {
    !ops.is_empty()
        && ops.iter().all(|op| match op {
            TimelineOp::Insert { position, data, .. } => {
                *position >= head.len() as i64
                    && serde_json::from_str::<serde_json::Value>(data)
                        .ok()
                        .and_then(|row| row.get("kind").and_then(|kind| kind.as_str().map(str::to_owned)))
                        .as_deref()
                        == Some("user")
            }
            TimelineOp::Remove { .. } | TimelineOp::Replace { .. } => false,
        })
}

fn record_trunk_change_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    change: TrunkChange,
    request_id: Option<&str>,
) -> Result<(), String> {
    if !conversation_exists(tx, conversation_id)? {
        // Subagent and temporary conversations have no row, and so no history.
        return Ok(());
    }
    let current = trunk_rows_tx(tx, conversation_id)?;
    let head = history_head_tx(tx, conversation_id)?;
    let ops = diff_timeline(&head, &current);
    if ops.is_empty() {
        return Ok(());
    }
    let first: bool = !tx
        .prepare(
            "SELECT 1 FROM history_entry
             WHERE conversation_id = ?1 AND kind IN ('edit', 'run') LIMIT 1",
        )
        .and_then(|mut statement| statement.exists([conversation_id]))
        .map_err(|error| format!("无法读取时间线历史：{error}"))?;
    let (kind, source) = match change {
        TrunkChange::Edit if only_appends_user_messages(&head, &ops) => ("edit", "message"),
        _ if first => ("edit", "baseline"),
        TrunkChange::Run => ("run", "run"),
        TrunkChange::Edit => ("edit", "edit"),
    };
    let seq = next_history_seq(tx, conversation_id)?;
    let (mut inserted, mut removed, mut replaced) = (0i64, 0i64, 0i64);
    for (ordinal, op) in ops.iter().enumerate() {
        let (name, context_id, position, data) = match op {
            TimelineOp::Remove { context_id } => {
                removed += 1;
                ("remove", context_id, None, None)
            }
            TimelineOp::Insert {
                context_id,
                position,
                data,
            } => {
                inserted += 1;
                ("insert", context_id, Some(*position), Some(data))
            }
            TimelineOp::Replace { context_id, data } => {
                replaced += 1;
                ("replace", context_id, None, Some(data))
            }
        };
        let hash = data
            .map(|data| store_blob_tx(tx, conversation_id, data, HISTORY_EVIDENCE_MAX_BYTES))
            .transpose()?;
        tx.execute(
            "INSERT INTO history_op
             (conversation_id, seq, ordinal, op, context_id, position, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                conversation_id,
                seq,
                ordinal as i64,
                name,
                context_id,
                position,
                hash
            ],
        )
        .map_err(|error| format!("无法写入时间线历史步骤：{error}"))?;
    }
    let detail = serde_json::json!({
        "source": source,
        "inserted": inserted,
        "removed": removed,
        "replaced": replaced,
        // Trunk length once this change applied, so the list reads without a replay.
        "rowCount": current.len() as i64,
    });
    tx.execute(
        "INSERT INTO history_entry
         (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, NULL, NULL, ?6, NULL)",
        rusqlite::params![
            conversation_id,
            seq,
            now(),
            kind,
            request_id,
            detail.to_string(),
        ],
    )
    .map_err(|error| format!("无法写入时间线历史：{error}"))?;
    tx.execute(
        "DELETE FROM history_head WHERE conversation_id = ?1",
        [conversation_id],
    )
    .map_err(|error| format!("无法清空时间线历史头：{error}"))?;
    for (position, row) in current.iter().enumerate() {
        tx.execute(
            "INSERT INTO history_head (conversation_id, position, context_id, data)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, position as i64, row.id, row.data],
        )
        .map_err(|error| format!("无法写入时间线历史头：{error}"))?;
    }
    Ok(())
}

/// The next `seq` of a conversation's history. One sequence across every kind and
/// every owner: order across them is what the record is read for.
fn next_history_seq(tx: &rusqlite::Transaction<'_>, conversation_id: &str) -> Result<i64, String> {
    let previous: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM history_entry WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取历史记录序号：{error}"))?
        .flatten();
    Ok(previous.unwrap_or(0) + 1)
}

/// The body as the record stores it under `cap`, and whether the cap cut it. The
/// cut lands on a UTF-8 boundary so the stored text is still text, and the marker
/// goes inside the stored body so a reader sees why it ends where it does.
fn stored_body(body: &str, cap: usize) -> (String, bool) {
    if body.len() <= cap {
        return (body.to_owned(), false);
    }
    let mut end = cap;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}\n{HISTORY_TRUNCATION_MARKER}", &body[..end]),
        true,
    )
}

/// Lowercase hex SHA-256 of what is actually stored, which is what a reader will
/// be able to verify. Hashing the pre-truncation body would address something no
/// row holds.
fn body_hash(stored: &str) -> String {
    Sha256::digest(stored.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Stores `body` under `cap` once per conversation and returns its address. A body
/// the conversation already holds costs nothing more, however many entries name it.
fn store_blob_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    body: &str,
    cap: usize,
) -> Result<String, String> {
    let (stored, truncated) = stored_body(body, cap);
    let hash = body_hash(&stored);
    tx.execute(
        "INSERT OR IGNORE INTO history_blob (conversation_id, hash, body, truncated)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![conversation_id, hash, stored, i64::from(truncated)],
    )
    .map_err(|error| format!("无法写入历史记录正文：{error}"))?;
    Ok(hash)
}

/// A `detail` column as JSON. A value that no longer parses reads as an empty
/// object: one damaged entry must not fail the whole list.
fn parse_detail(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({}))
}

/// Reads columns 0..=8 — seq, created_at, kind, owner, request_id, round,
/// call_id, answers, detail — as one listed entry. A response's usage is its own;
/// a request's is only what a request migrated from before responses were kept
/// carries, and the list replaces it with its response's when there is one.
fn read_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let kind: String = row.get(2)?;
    let detail = parse_detail(&row.get::<_, String>(8)?);
    let usage = matches!(kind.as_str(), "response" | "request")
        .then(|| HistoryUsage::from_detail(&detail))
        .flatten();
    Ok(HistoryEntry {
        seq: row.get(0)?,
        created_at: row.get(1)?,
        kind,
        owner: row.get(3)?,
        request_id: row.get(4)?,
        round: row.get(5)?,
        call_id: row.get(6)?,
        answers: row.get(7)?,
        detail,
        usage,
    })
}

/// One `message` part as the delta compares it. The address stands in for the
/// body — the comparison never reads a body back — and `author` is known only
/// for the request being written, where the recorder still had the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeltaMessage<'a> {
    hash: &'a str,
    role: Option<&'a str>,
    author: Option<&'a str>,
}

/// How many messages the user added and removed between two consecutive
/// requests, as `(added, removed)`.
///
/// Consecutive requests of one conversation are nearly identical — the next
/// round appends the model's turn to the same prefix — so the alignment strips
/// the common head and tail and looks only at what is left in the middle. That
/// costs a comparison per unchanged message instead of the quadratic table an
/// edit-distance alignment would build over a history that is re-sent in full
/// every round.
///
/// Inside the middle, a message whose role survives in the same relative order
/// is a *rewrite*, not a delete plus an insert: editing a prompt in place must
/// not read as the user having thrown a message away and written another. Roles
/// pair greedily in order, and an unknown role pairs with nothing, since a part
/// whose role could not be read carries no evidence either way.
fn message_delta(before: &[DeltaMessage<'_>], after: &[DeltaMessage<'_>]) -> (i64, i64) {
    let mut head = 0;
    while head < before.len() && head < after.len() && before[head].hash == after[head].hash {
        head += 1;
    }
    let mut tail = 0;
    while tail < before.len() - head
        && tail < after.len() - head
        && before[before.len() - 1 - tail].hash == after[after.len() - 1 - tail].hash
    {
        tail += 1;
    }
    let removed = &before[head..before.len() - tail];
    let added = &after[head..after.len() - tail];

    let mut removed_paired = vec![false; removed.len()];
    let mut added_paired = vec![false; added.len()];
    for (index, message) in added.iter().enumerate() {
        let Some(role) = message.role else {
            continue;
        };
        for (candidate, other) in removed.iter().enumerate() {
            if removed_paired[candidate] || other.role != Some(role) {
                continue;
            }
            removed_paired[candidate] = true;
            added_paired[index] = true;
            break;
        }
    }

    // Only what the *user* typed counts as an addition: every round appends the
    // model's own turn, and counting that would report traffic the host caused.
    let added_count = added
        .iter()
        .zip(&added_paired)
        .filter(|(message, paired)| !**paired && message.author == Some("user"))
        .count() as i64;
    let removed_count = removed_paired.iter().filter(|paired| !**paired).count() as i64;
    (added_count, removed_count)
}

/// The `message` parts the request numbered `seq` carried, in wire order.
///
/// Hashes and roles only, never a body: a round re-sends the whole history, so
/// joining `history_blob` here would make each write read back everything every
/// earlier write stored, and the record would cost the square of the conversation
/// to maintain — the exact cost content addressing exists to avoid.
fn previous_request_messages(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    seq: i64,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut statement = tx
        .prepare(
            "SELECT hash, role FROM history_part
             WHERE conversation_id = ?1 AND seq = ?2 AND kind = 'message'
             ORDER BY ordinal",
        )
        .map_err(|error| format!("无法查询上一条请求的分段：{error}"))?;
    let rows = statement
        .query_map(rusqlite::params![conversation_id, seq], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|error| format!("无法查询上一条请求的分段：{error}"))?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row.map_err(|error| format!("无法读取上一条请求的分段：{error}"))?);
    }
    Ok(messages)
}

/// One part of this request, hashed and ready to insert.
struct StoredPart<'a> {
    kind: &'a str,
    role: Option<&'a str>,
    author: Option<&'a str>,
    body: String,
    truncated: bool,
    hash: String,
}

fn record_history_request_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &HistoryRequestRecord,
) -> Result<Option<i64>, String> {
    let conversation_id = record.conversation_id.as_str();
    let owner = record.owner.as_deref();
    if !conversation_exists(tx, conversation_id)? {
        // A draft or temporary conversation has no row, and so no history. A
        // child does have one — it shares its parent's — and is separated from
        // the trunk by `owner` instead.
        return Ok(None);
    }
    let seq = next_history_seq(tx, conversation_id)?;
    // The request this one is read against is the previous request *of the same
    // owner*. Comparing a child's first payload with whatever the trunk last sent
    // would report the whole of one history as deleted and the whole of the other
    // as written by the user.
    let predecessor: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM history_entry
             WHERE conversation_id = ?1 AND kind = 'request' AND owner IS ?2",
            rusqlite::params![conversation_id, owner],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取历史记录序号：{error}"))?
        .flatten();

    let stored: Vec<StoredPart<'_>> = record
        .parts
        .iter()
        .map(|part| {
            let (body, truncated) = stored_body(&part.body, HISTORY_PART_MAX_BYTES);
            let hash = body_hash(&body);
            StoredPart {
                kind: part.kind.as_str(),
                role: part.role.as_deref(),
                author: part.author.as_deref(),
                body,
                truncated,
                hash,
            }
        })
        .collect();
    // The size of this request as it was sent, not what deduplication made it cost.
    let bytes = stored
        .iter()
        .map(|part| part.body.len() as i64)
        .sum::<i64>();

    // `predecessor` is `None` only for an owner's very first request: nothing is
    // ever pruned, so an owner that has sent anything still has a newest request
    // to compare against. An empty predecessor therefore means "nothing was here
    // before", and everything the user wrote is new.
    let previous_messages = match predecessor {
        Some(previous) => previous_request_messages(tx, conversation_id, previous)?,
        None => Vec::new(),
    };
    let before: Vec<DeltaMessage<'_>> = previous_messages
        .iter()
        .map(|(hash, role)| DeltaMessage {
            hash: hash.as_str(),
            role: role.as_deref(),
            author: None,
        })
        .collect();
    let after: Vec<DeltaMessage<'_>> = stored
        .iter()
        .filter(|part| part.kind == "message")
        .map(|part| DeltaMessage {
            hash: part.hash.as_str(),
            role: part.role,
            author: part.author,
        })
        .collect();
    let (messages_added, messages_removed) = message_delta(&before, &after);

    let envelope = store_blob_tx(
        tx,
        conversation_id,
        &record.envelope,
        HISTORY_EVIDENCE_MAX_BYTES,
    )?;
    let detail = serde_json::json!({
        "type": record.kind,
        "attempt": record.attempt,
        "providerName": record.provider_name,
        "family": record.family,
        "modelId": record.model_id,
        "partCount": stored.len() as i64,
        "bytes": bytes,
        // Messages the user added and removed between the request before this
        // one and this one, counted against the predecessor's own parts.
        "messagesAdded": messages_added,
        "messagesRemoved": messages_removed,
    });
    tx.execute(
        "INSERT INTO history_entry
         (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
         VALUES (?1, ?2, ?3, 'request', ?4, ?5, ?6, NULL, NULL, ?7, ?8)",
        rusqlite::params![
            conversation_id,
            seq,
            now(),
            owner,
            record.request_id,
            record.round,
            detail.to_string(),
            envelope,
        ],
    )
    .map_err(|error| format!("无法写入历史记录：{error}"))?;
    for (ordinal, part) in stored.iter().enumerate() {
        // A body this conversation already sent costs one row in total, however
        // many requests carried it.
        tx.execute(
            "INSERT OR IGNORE INTO history_blob (conversation_id, hash, body, truncated)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                conversation_id,
                part.hash,
                part.body,
                i64::from(part.truncated)
            ],
        )
        .map_err(|error| format!("无法写入历史记录正文：{error}"))?;
        tx.execute(
            "INSERT INTO history_part (conversation_id, seq, ordinal, kind, hash, role)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                conversation_id,
                seq,
                ordinal as i64,
                part.kind,
                part.hash,
                part.role
            ],
        )
        .map_err(|error| format!("无法写入请求分段：{error}"))?;
    }
    Ok(Some(seq))
}

fn record_history_entry_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &HistoryEntryRecord,
) -> Result<Option<i64>, String> {
    let conversation_id = record.conversation_id.as_str();
    if !conversation_exists(tx, conversation_id)? {
        return Ok(None);
    }
    let seq = next_history_seq(tx, conversation_id)?;
    let hash = record
        .body
        .as_deref()
        .map(|body| store_blob_tx(tx, conversation_id, body, record.body_cap))
        .transpose()?;
    tx.execute(
        "INSERT INTO history_entry
         (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            conversation_id,
            seq,
            now(),
            record.kind,
            record.owner,
            record.request_id,
            record.round,
            record.call_id,
            record.answers,
            record.detail.to_string(),
            hash,
        ],
    )
    .map_err(|error| format!("无法写入历史记录：{error}"))?;
    Ok(Some(seq))
}

/// Where a legacy row came from, and so where it sorts among rows written at the
/// same instant: a request before the response that answers it, both before the
/// trunk change the run settled into.
const LEGACY_REQUEST: i64 = 0;
const LEGACY_RESPONSE: i64 = 1;
const LEGACY_TRUNK: i64 = 2;

/// Moves the two ledgers and the trunk history older builds kept apart —
/// `wire_request*` and `wire_blob` (what went out), `wire_response` (what came
/// back), `timeline_event` / `timeline_op` / `timeline_head` (what changed on the
/// trunk) — into the one history, then drops them.
///
/// Rows are interleaved by the time they were written, which each legacy table
/// kept to the millisecond, so the migrated record reads in the order things
/// happened; each legacy table's own order breaks ties. A response keeps its link
/// to the request it answered, and a request keeps the usage the old ledger
/// attached to it. An old trunk change that only appended user rows at the tail is
/// filed as a sent message, the way a new one is.
///
/// Runs inside the schema pass's transaction: a store is migrated whole or not at
/// all, and a legacy table still standing is what marks a store as not yet
/// migrated.
fn migrate_legacy_history(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut present = HashSet::new();
    for table in LEGACY_HISTORY_TABLES {
        if has_table(tx, table)? {
            present.insert(table);
        }
    }
    if present.is_empty() {
        return Ok(());
    }
    let fail = |error: rusqlite::Error| format!("无法迁移旧的请求账本与时间线历史：{error}");
    for &(table, column, declaration) in LEGACY_COLUMNS {
        if present.contains(table) && !has_column(tx, table, column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
            ))
            .map_err(fail)?;
        }
    }
    let live = "conversation_id IN (SELECT id FROM conversation)";

    // One ordering across the three sources, per conversation.
    let mut order: Vec<(String, String, i64, i64)> = Vec::new();
    for (table, source, at) in [
        ("wire_request", LEGACY_REQUEST, "created_at"),
        ("wire_response", LEGACY_RESPONSE, "received_at"),
        ("timeline_event", LEGACY_TRUNK, "created_at"),
    ] {
        if !present.contains(table) {
            continue;
        }
        let mut statement = tx
            .prepare(&format!(
                "SELECT conversation_id, {at}, seq FROM {table} WHERE {live}"
            ))
            .map_err(fail)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })
            .map_err(fail)?;
        for row in rows {
            let (conversation, at, seq) = row.map_err(fail)?;
            order.push((conversation, at, source, seq));
        }
    }
    order.sort();
    let mut next: HashMap<String, i64> = HashMap::new();
    let mut mapped: HashMap<(String, i64, i64), i64> = HashMap::new();
    for (conversation, _, source, old) in &order {
        let seq = match next.get_mut(conversation) {
            Some(seq) => seq,
            None => {
                let base = next_history_seq(tx, conversation)?;
                next.entry(conversation.clone()).or_insert(base)
            }
        };
        mapped.insert((conversation.clone(), *source, *old), *seq);
        *seq += 1;
    }
    tx.execute_batch(
        "CREATE TEMP TABLE history_legacy_seq (
             conversation_id TEXT NOT NULL,
             source          INTEGER NOT NULL,
             old_seq         INTEGER NOT NULL,
             new_seq         INTEGER NOT NULL,
             PRIMARY KEY (conversation_id, source, old_seq)
         )",
    )
    .map_err(fail)?;
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO history_legacy_seq (conversation_id, source, old_seq, new_seq)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(fail)?;
        for ((conversation, source, old), new) in &mapped {
            insert
                .execute(rusqlite::params![conversation, source, old, new])
                .map_err(fail)?;
        }
    }
    let new_seq = |conversation: &str, source: i64, old: i64| {
        mapped.get(&(conversation.to_owned(), source, old)).copied()
    };

    if present.contains("wire_blob") {
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO history_blob (conversation_id, hash, body, truncated)
                 SELECT conversation_id, hash, body, truncated FROM wire_blob WHERE {live}"
            ),
            [],
        )
        .map_err(fail)?;
    }

    if present.contains("wire_request") {
        let rows = {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT conversation_id, seq, created_at, kind, request_id, round, attempt,
                            provider_name, family, model_id, envelope, part_count, bytes,
                            input_tokens, cached_input_tokens, output_tokens,
                            messages_added, messages_removed, owner
                     FROM wire_request WHERE {live}"
                ))
                .map_err(fail)?;
            let rows = statement
                .query_map([], |row| {
                    let usage = HistoryUsage {
                        input_tokens: row.get(13)?,
                        cached_input_tokens: row.get(14)?,
                        output_tokens: row.get(15)?,
                    };
                    let mut detail = serde_json::json!({
                        "type": row.get::<_, String>(3)?,
                        "attempt": row.get::<_, i64>(6)?,
                        "providerName": row.get::<_, String>(7)?,
                        "family": row.get::<_, String>(8)?,
                        "modelId": row.get::<_, String>(9)?,
                        "partCount": row.get::<_, i64>(11)?,
                        "bytes": row.get::<_, i64>(12)?,
                    });
                    for (key, column) in [("messagesAdded", 16), ("messagesRemoved", 17)] {
                        if let Some(count) = row.get::<_, Option<i64>>(column)? {
                            detail[key] = serde_json::json!(count);
                        }
                    }
                    if !usage.is_empty() {
                        detail["usage"] = serde_json::to_value(usage).unwrap_or_default();
                    }
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, Option<String>>(18)?,
                        detail,
                    ))
                })
                .map_err(fail)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(fail)?
        };
        for (conversation, old, created_at, request_id, round, envelope, owner, detail) in rows {
            let Some(seq) = new_seq(&conversation, LEGACY_REQUEST, old) else {
                continue;
            };
            let hash = store_blob_tx(tx, &conversation, &envelope, HISTORY_EVIDENCE_MAX_BYTES)?;
            tx.execute(
                "INSERT INTO history_entry
                 (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
                 VALUES (?1, ?2, ?3, 'request', ?4, ?5, ?6, NULL, NULL, ?7, ?8)",
                rusqlite::params![
                    conversation,
                    seq,
                    created_at,
                    owner,
                    request_id,
                    round,
                    detail.to_string(),
                    hash
                ],
            )
            .map_err(fail)?;
        }
    }

    if present.contains("wire_request_part") {
        tx.execute(
            &format!(
                "INSERT INTO history_part (conversation_id, seq, ordinal, kind, hash, role)
                 SELECT part.conversation_id, moved.new_seq, part.ordinal, part.kind, part.hash, part.role
                 FROM wire_request_part part
                 JOIN temp.history_legacy_seq moved
                   ON moved.conversation_id = part.conversation_id
                  AND moved.source = {LEGACY_REQUEST}
                  AND moved.old_seq = part.seq"
            ),
            [],
        )
        .map_err(fail)?;
    }

    if present.contains("wire_response") {
        let rows = {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT conversation_id, seq, received_at, request_seq, owner, request_id,
                            round, attempt, model_id, finish_reason, raw_finish_reason, hash
                     FROM wire_response WHERE {live}"
                ))
                .map_err(fail)?;
            let rows = statement
                .query_map([], |row| {
                    let mut detail = serde_json::json!({"attempt": row.get::<_, i64>(7)?});
                    for (key, column) in [("modelId", 8), ("finishReason", 9), ("rawFinishReason", 10)] {
                        if let Some(value) = row.get::<_, Option<String>>(column)? {
                            detail[key] = serde_json::json!(value);
                        }
                    }
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(11)?,
                        detail,
                    ))
                })
                .map_err(fail)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(fail)?
        };
        for (conversation, old, received_at, request_seq, owner, request_id, round, hash, detail) in
            rows
        {
            let Some(seq) = new_seq(&conversation, LEGACY_RESPONSE, old) else {
                continue;
            };
            let answers =
                request_seq.and_then(|request| new_seq(&conversation, LEGACY_REQUEST, request));
            tx.execute(
                "INSERT INTO history_entry
                 (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
                 VALUES (?1, ?2, ?3, 'response', ?4, ?5, ?6, NULL, ?7, ?8, ?9)",
                rusqlite::params![
                    conversation,
                    seq,
                    received_at,
                    owner,
                    request_id,
                    round,
                    answers,
                    detail.to_string(),
                    hash
                ],
            )
            .map_err(fail)?;
        }
    }

    if present.contains("timeline_op") {
        let rows = {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT conversation_id, seq, ordinal, op, context_id, position, data
                     FROM timeline_op WHERE {live}"
                ))
                .map_err(fail)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                })
                .map_err(fail)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(fail)?
        };
        for (conversation, old, ordinal, op, context_id, position, data) in rows {
            let Some(seq) = new_seq(&conversation, LEGACY_TRUNK, old) else {
                continue;
            };
            let hash = data
                .as_deref()
                .map(|data| store_blob_tx(tx, &conversation, data, HISTORY_EVIDENCE_MAX_BYTES))
                .transpose()?;
            tx.execute(
                "INSERT INTO history_op (conversation_id, seq, ordinal, op, context_id, position, hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![conversation, seq, ordinal, op, context_id, position, hash],
            )
            .map_err(fail)?;
        }
    }

    if present.contains("timeline_event") {
        let rows = {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT conversation_id, seq, kind, request_id, inserted, removed, replaced,
                            row_count, created_at
                     FROM timeline_event WHERE {live}"
                ))
                .map_err(fail)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        [row.get::<_, i64>(4)?, row.get::<_, i64>(5)?, row.get::<_, i64>(6)?],
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                })
                .map_err(fail)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(fail)?
        };
        for (conversation, old, kind, request_id, [inserted, removed, replaced], row_count, created_at) in
            rows
        {
            let Some(seq) = new_seq(&conversation, LEGACY_TRUNK, old) else {
                continue;
            };
            // Judged from the migrated steps, the way a new change is judged from
            // its own: only tail inserts of user rows is a message that was sent.
            let head_before = row_count - inserted + removed;
            let appended = removed == 0
                && replaced == 0
                && inserted > 0
                && !tx
                    .prepare(
                        "SELECT 1 FROM history_op op
                         LEFT JOIN history_blob stored
                           ON stored.conversation_id = op.conversation_id AND stored.hash = op.hash
                         WHERE op.conversation_id = ?1 AND op.seq = ?2
                           AND (op.op <> 'insert' OR op.position < ?3 OR stored.body IS NULL
                                OR NOT json_valid(stored.body)
                                OR json_extract(stored.body, '$.kind') IS NOT 'user')
                         LIMIT 1",
                    )
                    .and_then(|mut statement| {
                        statement.exists(rusqlite::params![conversation, seq, head_before])
                    })
                    .map_err(fail)?;
            let (kind, source) = match kind.as_str() {
                "edit" if appended => ("edit", "message"),
                "baseline" => ("edit", "baseline"),
                "run" => ("run", "run"),
                _ => ("edit", "edit"),
            };
            let detail = serde_json::json!({
                "source": source,
                "inserted": inserted,
                "removed": removed,
                "replaced": replaced,
                "rowCount": row_count,
            });
            tx.execute(
                "INSERT INTO history_entry
                 (conversation_id, seq, created_at, kind, owner, request_id, round, call_id, answers, detail, hash)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, NULL, NULL, ?6, NULL)",
                rusqlite::params![conversation, seq, created_at, kind, request_id, detail.to_string()],
            )
            .map_err(fail)?;
        }
    }

    if present.contains("timeline_head") {
        tx.execute(
            &format!(
                "INSERT OR REPLACE INTO history_head (conversation_id, position, context_id, data)
                 SELECT conversation_id, position, context_id, data FROM timeline_head WHERE {live}"
            ),
            [],
        )
        .map_err(fail)?;
    }

    tx.execute_batch("DROP TABLE temp.history_legacy_seq")
        .map_err(fail)?;
    for table in LEGACY_HISTORY_TABLES {
        if present.contains(table) {
            tx.execute_batch(&format!("DROP TABLE {table}"))
                .map_err(fail)?;
        }
    }
    Ok(())
}

fn put_conversation_tx(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    conversation: &Conversation,
    new_row: NewRowAt,
) -> Result<(), String> {
    put_conversation_row_tx(tx, workspace_id, conversation, new_row)?;

    tx.execute(
        "DELETE FROM context WHERE conversation_id = ?1",
        [conversation.id.as_str()],
    )
    .map_err(|error| format!("无法清空上下文：{error}"))?;
    for (index, item) in conversation.contexts.iter().enumerate() {
        upsert_context_tx(
            tx,
            &conversation.id,
            None,
            item,
            ContextStatus::Settled,
            index as f64 * ORDER_STEP,
        )?;
    }

    tx.execute(
        "DELETE FROM branch WHERE conversation_id = ?1",
        [conversation.id.as_str()],
    )
    .map_err(|error| format!("无法清空分支：{error}"))?;
    for (index, branch) in conversation.branches.iter().enumerate() {
        tx.execute(
            "INSERT INTO branch (conversation_id, id, fork_context_id, active, created_at, updated_at, order_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                conversation.id,
                branch.id,
                branch.fork_context_id,
                i64::from(branch.active),
                branch.created_at,
                branch.updated_at,
                index as f64 * ORDER_STEP,
            ],
        )
        .map_err(|error| format!("无法写入分支：{error}"))?;
        for (position, item) in branch.contexts.iter().enumerate() {
            upsert_context_tx(
                tx,
                &conversation.id,
                Some(&branch.id),
                item,
                ContextStatus::Settled,
                position as f64 * ORDER_STEP,
            )?;
        }
    }

    replace_queued_messages_tx(tx, conversation)?;
    replace_aborted_tasks_tx(tx, conversation)?;
    // In the same transaction as the replace it describes: a history that can
    // outlive a rolled-back write would name a timeline that never existed.
    record_trunk_change_tx(tx, &conversation.id, TrunkChange::Edit, None)
}

/// The `worktree` column: a list of records, or — written before every
/// project workspace could have one — a single record, which is workspace 1's.
/// An unreadable value reads as none, so the conversation runs at its
/// workspace roots rather than somewhere a damaged record points.
fn read_worktrees(value: &str) -> Vec<ConversationWorktree> {
    if let Ok(worktrees) = serde_json::from_str::<Vec<ConversationWorktree>>(value) {
        return worktrees;
    }
    serde_json::from_str::<ConversationWorktree>(value)
        .map(|worktree| vec![worktree])
        .unwrap_or_default()
}

/// Where a conversation the store has not seen before enters its workspace's list.
#[derive(Clone, Copy)]
enum NewRowAt {
    /// After every other one: load-time seeding keeps the document's order, and
    /// the renderer sends the workspace's order itself right after a creation.
    End,
    /// Above every other one, as the sidebar lists a conversation the user just
    /// started: the child of a host-made fork or handover, whose order no one sends.
    Top,
}

/// Inserts or updates the conversation's own row. A new conversation goes where
/// `new_row` says; an existing one keeps its sidebar position.
fn put_conversation_row_tx(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    conversation: &Conversation,
    new_row: NewRowAt,
) -> Result<(), String> {
    let settings = serde_json::to_string(&conversation.settings)
        .map_err(|error| format!("对话设置无法序列化：{error}"))?;
    // NULL rather than `[]` for the common case, so a conversation without one
    // reads the same as it did before worktrees were per workspace.
    let worktree = if conversation.worktrees.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&conversation.worktrees)
                .map_err(|error| format!("对话工作树记录无法序列化：{error}"))?,
        )
    };
    let run_target = conversation
        .run_target
        .as_ref()
        .map(|value| serde_json::to_string(value))
        .transpose()
        .map_err(|error| format!("对话运行地点无法序列化：{error}"))?;
    // NULL rather than `[]` for the common case, so an unset list reads the same
    // as one written before the column existed.
    let additional_directories = if conversation.additional_directories.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&conversation.additional_directories)
                .map_err(|error| format!("对话额外工作目录无法序列化：{error}"))?,
        )
    };
    let attached_workspaces = if conversation.attached_workspaces.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&conversation.attached_workspaces)
                .map_err(|error| format!("对话工作区列表无法序列化：{error}"))?,
        )
    };
    let fork_of = conversation
        .fork_of
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("对话分叉来源无法序列化：{error}"))?;
    let handoff_of = conversation
        .handoff_of
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("对话交接来源无法序列化：{error}"))?;
    let order_key: Option<f64> = tx
        .query_row(
            "SELECT order_key FROM conversation WHERE id = ?1",
            [conversation.id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取对话排序键：{error}"))?;
    let order_key = match order_key {
        Some(existing) => existing,
        None => {
            let (edge, step) = match new_row {
                NewRowAt::End => ("max", ORDER_STEP),
                NewRowAt::Top => ("min", -ORDER_STEP),
            };
            let edge: Option<f64> = tx
                .query_row(
                    &format!("SELECT {edge}(order_key) FROM conversation WHERE workspace_id = ?1"),
                    [workspace_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("无法读取对话排序键：{error}"))?
                .flatten();
            edge.map(|value| value + step).unwrap_or(0.0)
        }
    };
    tx.execute(
        "INSERT INTO conversation (id, workspace_id, title, created_at, updated_at, order_key, settings, worktree, run_target, parent_conversation_id, preset_id, template_id, additional_directories, attached_workspaces, fork_of, handoff_of)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (id) DO UPDATE SET workspace_id = excluded.workspace_id,
           title = excluded.title, created_at = excluded.created_at,
           updated_at = excluded.updated_at, settings = excluded.settings,
           worktree = excluded.worktree, run_target = excluded.run_target,
           parent_conversation_id = excluded.parent_conversation_id,
           preset_id = excluded.preset_id, template_id = excluded.template_id,
           additional_directories = excluded.additional_directories,
           attached_workspaces = excluded.attached_workspaces,
           fork_of = excluded.fork_of, handoff_of = excluded.handoff_of",
        rusqlite::params![
            conversation.id,
            workspace_id,
            conversation.title,
            conversation.created_at,
            conversation.updated_at,
            order_key,
            settings,
            worktree,
            run_target,
            conversation.parent_conversation_id,
            conversation.preset_id,
            conversation.template_id,
            additional_directories,
            attached_workspaces,
            fork_of,
            handoff_of,
        ],
    )
    .map_err(|error| format!("无法写入对话：{error}"))?;
    Ok(())
}

fn replace_queued_messages_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation: &Conversation,
) -> Result<(), String> {
    tx.execute(
        "DELETE FROM queued_message WHERE conversation_id = ?1",
        [conversation.id.as_str()],
    )
    .map_err(|error| format!("无法清空排队消息：{error}"))?;
    for (index, message) in conversation.queued_messages.iter().enumerate() {
        let images = serde_json::to_string(&message.images)
            .map_err(|error| format!("排队消息附图无法序列化：{error}"))?;
        let files = serde_json::to_string(&message.files)
            .map_err(|error| format!("排队消息附件无法序列化：{error}"))?;
        tx.execute(
            "INSERT INTO queued_message (conversation_id, id, order_key, content, images, files, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                conversation.id,
                message.id,
                index as f64 * ORDER_STEP,
                message.content,
                images,
                files,
                message.created_at,
            ],
        )
        .map_err(|error| format!("无法写入排队消息：{error}"))?;
    }
    Ok(())
}

fn replace_aborted_tasks_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation: &Conversation,
) -> Result<(), String> {
    tx.execute(
        "DELETE FROM aborted_task WHERE conversation_id = ?1",
        [conversation.id.as_str()],
    )
    .map_err(|error| format!("无法清空中止任务：{error}"))?;
    for (index, record) in conversation.user_aborted_tasks.iter().enumerate() {
        let data = serde_json::to_string(record)
            .map_err(|error| format!("中止任务记录无法序列化：{error}"))?;
        tx.execute(
            "INSERT INTO aborted_task (conversation_id, id, order_key, data)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation.id, record.id, index as f64 * ORDER_STEP, data],
        )
        .map_err(|error| format!("无法写入中止任务：{error}"))?;
    }
    Ok(())
}

fn read_contexts(
    conn: &Connection,
    conversation_id: &str,
    branch_id: Option<&str>,
) -> Result<Vec<ContextItem>, String> {
    let mut statement = conn
        .prepare(
            "SELECT data FROM context WHERE conversation_id = ?1 AND branch_id IS ?2
             ORDER BY order_key, rowid",
        )
        .map_err(|error| format!("无法查询上下文：{error}"))?;
    let rows = statement
        .query_map(rusqlite::params![conversation_id, branch_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("无法查询上下文：{error}"))?;
    let mut items = Vec::new();
    for row in rows {
        let data = row.map_err(|error| format!("无法读取上下文：{error}"))?;
        let item: ContextItem =
            serde_json::from_str(&data).map_err(|error| format!("上下文无法解析：{error}"))?;
        items.push(item);
    }
    Ok(items)
}

/// Marks assistant or reasoning context JSON as interrupted. Other kinds have no `interrupted`
/// field and never exist in `streaming` status.
fn mark_interrupted(data: &str) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(data).map_err(|error| format!("上下文无法解析：{error}"))?;
    let kind = value.get("kind").and_then(serde_json::Value::as_str);
    if matches!(kind, Some("assistant") | Some("reasoning")) {
        if let Some(object) = value.as_object_mut() {
            object.insert("interrupted".into(), serde_json::Value::Bool(true));
        }
    }
    serde_json::to_string(&value).map_err(|error| format!("上下文无法序列化：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolResult, UserAbortedTaskMetrics};

    fn temp_store() -> (tempfile::TempDir, ConversationStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("open");
        (dir, store)
    }

    fn settings() -> ConversationSettings {
        serde_json::from_value(serde_json::json!({
            "enabledTools": [],
        }))
        .expect("settings")
    }

    fn conversation(id: &str) -> Conversation {
        Conversation {
            id: id.into(),
            title: "t".into(),
            created_at: "2026-08-25T00:00:00.000Z".into(),
            updated_at: "2026-08-25T00:00:00.000Z".into(),
            settings: settings(),
            contexts: Vec::new(),
            queued_messages: Vec::new(),
            branches: Vec::new(),
            user_aborted_tasks: Vec::new(),
            worktrees: Vec::new(),
            run_target: None,
            parent_conversation_id: None,
            fork_of: None,
            handoff_of: None,
            preset_id: String::new(),
            template_id: String::new(),
            attached_workspaces: Vec::new(),
            additional_directories: Vec::new(),
        }
    }

    fn user(id: &str, content: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: content.into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:01.000Z".into(),
        }
    }

    fn user_at(id: &str, created_at: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: "hi".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: created_at.into(),
        }
    }

    fn assistant_at(id: &str, created_at: &str) -> ContextItem {
        ContextItem::Assistant {
            id: id.into(),
            content: "ok".into(),
            round: None,
            model_turn_id: None,
            interrupted: false,
            sources: Vec::new(),
            created_at: created_at.into(),
        }
    }

    fn cached(store: &ConversationStore, id: &str) -> bool {
        MemoryPool::global().contains(&store.bodies.key(id))
    }

    /// A read keeps the body in memory; the next read comes from there.
    #[test]
    fn a_read_fills_the_pool_and_the_next_read_is_served_from_it() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("c1");
        conversation.contexts.push(user("u1", "hello"));
        store.put_conversation("ws", &conversation).unwrap();
        assert!(!cached(&store, "c1"), "a write leaves nothing behind to go stale");
        let first = store.conversation_shared("c1").unwrap().unwrap();
        assert!(cached(&store, "c1"));
        let second = store.conversation_shared("c1").unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// Unloaded from memory, a body is read from the database again.
    #[test]
    fn an_unloaded_body_falls_back_to_the_database() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("c1");
        conversation.contexts.push(user("u1", "hello"));
        store.put_conversation("ws", &conversation).unwrap();
        let first = store.conversation_shared("c1").unwrap().unwrap();
        MemoryPool::global().remove(&store.bodies.key("c1"));
        let again = store.conversation_shared("c1").unwrap().unwrap();
        assert!(!Arc::ptr_eq(&first, &again));
        assert_eq!(*first, *again);
        assert!(cached(&store, "c1"));
    }

    /// Every write path that changes a body drops the cached copy, so a read
    /// after it sees the database, never the body from before the write.
    #[test]
    fn every_body_write_invalidates_the_cached_copy() {
        let (_dir, store) = temp_store();
        let mut base = conversation("c1");
        base.contexts.push(user("u1", "hello"));
        base.queued_messages.push(QueuedMessage {
            id: "q1".into(),
            content: "later".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:02.000Z".into(),
        });
        store.put_conversation("ws", &base).unwrap();

        let writes: Vec<(&str, Box<dyn Fn(&ConversationStore)>)> = vec![
            (
                "upsert",
                Box::new(|store| {
                    store
                        .upsert_contexts("c1", &[user("u2", "streamed")], ContextStatus::Streaming)
                        .unwrap()
                }),
            ),
            (
                "reconcile one",
                Box::new(|store| {
                    store.reconcile_streaming_in("c1").unwrap();
                }),
            ),
            (
                "metadata",
                Box::new(|store| {
                    let mut renamed = store.conversation("c1").unwrap().unwrap();
                    renamed.title = "renamed".into();
                    store.put_conversation_metadata("ws", &renamed).unwrap()
                }),
            ),
            (
                "dequeue",
                Box::new(|store| store.remove_queued_messages("c1", &["q1".into()]).unwrap()),
            ),
            (
                "discard",
                Box::new(|store| {
                    store
                        .upsert_contexts("c1", &[user("u3", "draft")], ContextStatus::Streaming)
                        .unwrap();
                    store.conversation_shared("c1").unwrap();
                    store.discard_streaming_contexts("c1", &["u3"]).unwrap();
                }),
            ),
            (
                "reconcile all",
                Box::new(|store| {
                    store.reconcile_streaming().unwrap();
                }),
            ),
            (
                "reorder",
                Box::new(|store| store.set_workspace_order("ws", &["c1".into()]).unwrap()),
            ),
            (
                "replace",
                Box::new(|store| {
                    let mut replaced = store.conversation("c1").unwrap().unwrap();
                    replaced.contexts.push(user("u4", "edited"));
                    store.put_conversation("ws", &replaced).unwrap()
                }),
            ),
        ];
        for (name, write) in writes {
            store.conversation_shared("c1").unwrap();
            assert!(cached(&store, "c1"), "{name}: primed");
            write(&store);
            assert!(!cached(&store, "c1"), "{name}: the write invalidates");
            assert_eq!(
                store.conversation("c1").unwrap(),
                store.conversation_from_disk("c1").unwrap(),
                "{name}: the next read matches the database"
            );
        }
        store.delete_conversation("c1").unwrap();
        assert!(store.conversation("c1").unwrap().is_none());
    }

    /// A read that raced a write returns what it read but does not keep it: the
    /// write's invalidation came after the read noted the generation.
    #[test]
    fn a_read_that_raced_a_write_is_not_kept() {
        let (_dir, store) = temp_store();
        store.put_conversation("ws", &conversation("c1")).unwrap();
        let seen = store.bodies.generation("c1");
        let stale = Arc::new(store.conversation_from_disk("c1").unwrap().unwrap());
        store
            .upsert_contexts("c1", &[user("u1", "new")], ContextStatus::Settled)
            .unwrap();
        store.bodies.fill("c1", seen, &stale);
        assert!(!cached(&store, "c1"));
        assert_eq!(store.conversation("c1").unwrap().unwrap().contexts.len(), 1);
    }

    /// Stores on different files never share cached bodies, even for the same
    /// id.
    #[test]
    fn stores_on_different_files_keep_apart() {
        let (_dir_a, a) = temp_store();
        let (_dir_b, b) = temp_store();
        let mut first = conversation("same");
        first.title = "a".into();
        let mut second = conversation("same");
        second.title = "b".into();
        a.put_conversation("ws", &first).unwrap();
        b.put_conversation("ws", &second).unwrap();
        assert_eq!(a.conversation("same").unwrap().unwrap().title, "a");
        assert_eq!(b.conversation("same").unwrap().unwrap().title, "b");
    }

    /// Two stores on one file share one cache: a write through either
    /// invalidates what the other read.
    #[test]
    fn stores_on_one_file_share_their_invalidations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DATABASE_FILE_NAME);
        let a = ConversationStore::open(&path).unwrap();
        a.put_conversation("ws", &conversation("c1")).unwrap();
        let b = ConversationStore::open(&path).unwrap();
        assert!(b.conversation("c1").unwrap().unwrap().contexts.is_empty());
        a.upsert_contexts("c1", &[user("u1", "new")], ContextStatus::Settled)
            .unwrap();
        assert_eq!(b.conversation("c1").unwrap().unwrap().contexts.len(), 1);
    }

    /// The point of recording history at all: a row the user later deleted, and a
    /// row they later rewrote, both still read the way they did at the time. A
    /// snapshot rebuilt from the current timeline could not do this.
    /// The worktree column holds the list, and still reads the single record
    /// it held when only workspace 1 could have a worktree.
    #[test]
    fn the_worktree_column_reads_the_list_and_the_legacy_single_record() {
        let record = r#"{"path":"/w/a","branch":"mework/conv/a","baseOid":"abc"}"#;
        assert_eq!(read_worktrees(record).len(), 1);
        assert_eq!(read_worktrees(&format!("[{record},{record}]")).len(), 2);
        assert!(read_worktrees("not json").is_empty());
    }

    #[test]
    fn an_earlier_snapshot_keeps_rows_a_later_edit_removed_and_rewrote() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("conv_history");
        conversation.contexts.push(user("ctx_a", "first"));
        conversation.contexts.push(user("ctx_b", "second"));
        store.put_conversation("ws", &conversation).expect("seed");

        conversation.contexts.push(user("ctx_c", "third"));
        store.put_conversation("ws", &conversation).expect("append");

        // Delete the middle row and rewrite the first, as a renderer edit does.
        conversation.contexts = vec![user("ctx_a", "rewritten"), user("ctx_c", "third")];
        store.put_conversation("ws", &conversation).expect("edit");

        let events = store.trunk_changes("conv_history").expect("events");
        assert_eq!(events.len(), 3, "one entry per committed change");
        assert_eq!((events[0].kind.as_str(), events[0].source.as_str()), ("edit", "message"));
        assert_eq!(events[0].inserted, 2);
        assert_eq!(
            (events[1].kind.as_str(), events[1].source.as_str()),
            ("edit", "message"),
            "appending what the user typed is sending, not editing"
        );
        assert_eq!((events[1].inserted, events[1].removed), (1, 0));
        assert_eq!(events[2].source, "edit");
        assert_eq!((events[2].removed, events[2].replaced), (1, 1));

        let seeded = store
            .trunk_snapshot("conv_history", events[0].seq)
            .expect("baseline snapshot");
        assert_eq!(
            seeded.iter().map(ContextItem::id).collect::<Vec<_>>(),
            ["ctx_a", "ctx_b"]
        );

        let before_edit = store
            .trunk_snapshot("conv_history", events[1].seq)
            .expect("snapshot before the edit");
        assert_eq!(
            before_edit.iter().map(ContextItem::id).collect::<Vec<_>>(),
            ["ctx_a", "ctx_b", "ctx_c"],
            "the deleted row is still in the history that preceded its deletion"
        );
        let ContextItem::User { content, .. } = &before_edit[0] else {
            panic!("expected a user row");
        };
        assert_eq!(content, "first", "the rewrite must not reach back in time");

        let newest = store
            .trunk_snapshot("conv_history", events[2].seq)
            .expect("newest snapshot");
        assert_eq!(
            newest.iter().map(ContextItem::id).collect::<Vec<_>>(),
            ["ctx_a", "ctx_c"]
        );
        let ContextItem::User { content, .. } = &newest[0] else {
            panic!("expected a user row");
        };
        assert_eq!(content, "rewritten");
    }

    /// A commit that leaves the trunk exactly as it was is not a moment in its
    /// history. Without this, every debounced metadata write would add a row.
    #[test]
    fn recording_an_unchanged_timeline_writes_no_entry() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("conv_quiet");
        conversation.contexts.push(user("ctx_a", "first"));
        store.put_conversation("ws", &conversation).expect("seed");
        store.put_conversation("ws", &conversation).expect("resave");
        store
            .record_trunk_change("conv_quiet", TrunkChange::Run, Some("req_1"))
            .expect("record");

        let events = store.trunk_changes("conv_quiet").expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, "message");
    }

    /// Reordering has to replay as a reorder, not as a body swap: the rows keep
    /// their ids, so a diff that only compared positions pairwise would rewrite
    /// both bodies and leave the order wrong.
    #[test]
    fn a_reordered_timeline_replays_in_the_order_it_had() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("conv_moved");
        conversation.contexts.push(user("ctx_a", "a"));
        conversation.contexts.push(user("ctx_b", "b"));
        conversation.contexts.push(user("ctx_c", "c"));
        store.put_conversation("ws", &conversation).expect("seed");

        conversation.contexts = vec![user("ctx_c", "c"), user("ctx_a", "a"), user("ctx_b", "b")];
        store
            .put_conversation("ws", &conversation)
            .expect("reorder");

        let events = store.trunk_changes("conv_moved").expect("events");
        assert_eq!(events.len(), 2);
        let moved = store
            .trunk_snapshot("conv_moved", events[1].seq)
            .expect("snapshot");
        assert_eq!(
            moved.iter().map(ContextItem::id).collect::<Vec<_>>(),
            ["ctx_c", "ctx_a", "ctx_b"]
        );
    }

    /// A run's entry names the request it settled, so the history reads as a list
    /// of calls rather than of anonymous changes.
    #[test]
    fn a_run_entry_names_its_request() {
        let (_dir, store) = temp_store();
        let mut conversation = conversation("conv_run");
        conversation.contexts.push(user("ctx_a", "ask"));
        store.put_conversation("ws", &conversation).expect("seed");
        store
            .upsert_contexts(
                "conv_run",
                &[assistant("ctx_reply", "answer", 0)],
                ContextStatus::Settled,
            )
            .expect("reply");
        store
            .record_trunk_change("conv_run", TrunkChange::Run, Some("req_7"))
            .expect("record");

        let events = store.trunk_changes("conv_run").expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "run");
        assert_eq!(events[1].source, "run");
        assert_eq!(events[1].request_id.as_deref(), Some("req_7"));
        assert_eq!(events[1].row_count, 2);
    }

    #[test]
    fn fork_start_survives_reopen_and_is_consumed_only_at_acceptance() {
        let (dir, store) = temp_store();
        let mut child = conversation("fork_child");
        child.contexts.push(user("fork_prompt", "answer once"));
        store
            .put_fork_conversation("ws", &child, "fork_prompt")
            .unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store.pending_fork_starts().unwrap(),
            vec![PendingForkStart {
                workspace_id: "ws".into(),
                conversation_id: child.id.clone(),
                prompt_context_id: "fork_prompt".into(),
            }]
        );
        assert!(store
            .accept_fork_start(&child.id, Some("wrong_prompt"), || panic!(
                "must not establish"
            ))
            .is_err());
        assert_eq!(store.pending_fork_starts().unwrap().len(), 1);
        let mut established = false;
        store
            .accept_fork_start(&child.id, Some("fork_prompt"), || {
                established = true;
            })
            .unwrap();
        assert!(established);
        assert!(store.pending_fork_starts().unwrap().is_empty());
        assert!(store
            .accept_fork_start(&child.id, Some("fork_prompt"), || panic!("duplicate run"))
            .is_err());
        assert_eq!(
            store
                .conversation(&child.id)
                .unwrap()
                .unwrap()
                .contexts
                .len(),
            1
        );
    }

    #[test]
    fn a_fork_child_opens_its_workspace_list_and_keeps_its_place_when_saved_again() {
        let (_dir, store) = temp_store();
        store.put_conversation("ws", &conversation("first")).unwrap();
        store.put_conversation("ws", &conversation("second")).unwrap();
        let mut child = conversation("fork_child");
        child.contexts.push(user("prompt", "hello"));
        store.put_fork_conversation("ws", &child, "prompt").unwrap();
        store.put_conversation("ws", &conversation("seeded")).unwrap();
        store.put_conversation("ws", &child).unwrap();
        assert_eq!(
            store.workspace_conversation_ids("ws").unwrap(),
            ["fork_child", "first", "second", "seeded"]
        );
        // The first conversation of a workspace has no neighbour to go above.
        let mut lone = conversation("lone_child");
        lone.contexts.push(user("prompt", "hello"));
        store.put_fork_conversation("empty", &lone, "prompt").unwrap();
        assert_eq!(store.workspace_conversation_ids("empty").unwrap(), ["lone_child"]);
    }

    #[test]
    fn fork_start_creation_is_atomic_and_delete_cascades() {
        let (_dir, store) = temp_store();
        let mut child = conversation("fork_child");
        child.contexts.push(user("prompt", "hello"));
        store.lock().unwrap().execute_batch("CREATE TRIGGER fail_intent BEFORE INSERT ON pending_fork_start BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(store.put_fork_conversation("ws", &child, "prompt").is_err());
        assert!(store.conversation(&child.id).unwrap().is_none());
        store
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_intent")
            .unwrap();
        store.put_fork_conversation("ws", &child, "prompt").unwrap();
        assert_eq!(store.pending_fork_starts().unwrap().len(), 1);
        store.delete_conversation(&child.id).unwrap();
        assert!(store.pending_fork_starts().unwrap().is_empty());
    }

    #[test]
    fn version_two_keeps_history_and_manual_run_consumes_fork_intent() {
        let (dir, store) = temp_store();
        let mut child = conversation("history");
        child.contexts.push(user("prompt", "hello"));
        store.put_conversation("ws", &child).unwrap();
        store.lock().unwrap().execute_batch("DROP TABLE pending_fork_start; DROP TABLE conversation_plan; DROP TABLE fork_decision; DROP TABLE history_op; DROP TABLE history_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE history_part; DROP TABLE history_blob; DROP TABLE history_entry; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 2;").unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store
                .conversation(&child.id)
                .unwrap()
                .unwrap()
                .contexts
                .len(),
            1
        );
        assert!(store.pending_fork_starts().unwrap().is_empty());
        child.id = "pending".into();
        store.put_fork_conversation("ws", &child, "prompt").unwrap();
        store.accept_fork_start(&child.id, None, || ()).unwrap();
        assert!(store.pending_fork_starts().unwrap().is_empty());
    }

    #[test]
    fn version_three_keeps_history_and_gains_the_plan_table() {
        let (dir, store) = temp_store();
        let mut child = conversation("history");
        child.contexts.push(user("prompt", "hello"));
        store.put_conversation("ws", &child).unwrap();
        store.lock().unwrap().execute_batch("DROP TABLE conversation_plan; DROP TABLE fork_decision; DROP TABLE history_op; DROP TABLE history_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE history_part; DROP TABLE history_blob; DROP TABLE history_entry; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 3;").unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store
                .conversation(&child.id)
                .unwrap()
                .unwrap()
                .contexts
                .len(),
            1
        );
        assert_eq!(store.conversation_plan(&child.id).unwrap(), None);
        let plan = crate::model::ConversationPlan {
            conversation_id: child.id.clone(),
            markdown: "# Plan".into(),
            status: crate::model::PlanStatus::Draft,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        store.put_conversation_plan(&plan).unwrap();
        assert_eq!(store.conversation_plan(&child.id).unwrap(), Some(plan));
    }

    /// The plan is one row per conversation: rewriting it keeps the moment it
    /// was first written, approval moves only the status, and deleting the
    /// conversation takes the plan with it.
    #[test]
    fn a_conversation_plan_is_rewritten_in_place_and_dies_with_its_conversation() {
        let (_dir, store) = temp_store();
        let source = conversation("planned");
        store.put_conversation("ws", &source).unwrap();
        assert_eq!(store.conversation_plan(&source.id).unwrap(), None);

        let mut plan = crate::model::ConversationPlan {
            conversation_id: source.id.clone(),
            markdown: "# Draft".into(),
            status: crate::model::PlanStatus::Draft,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        store.put_conversation_plan(&plan).unwrap();

        // A rewrite carries a fresh `created_at`; the stored one does not move.
        let rewritten = crate::model::ConversationPlan {
            markdown: "# Revised".into(),
            created_at: "2026-02-02T00:00:00Z".into(),
            updated_at: "2026-02-02T00:00:00Z".into(),
            ..plan.clone()
        };
        store.put_conversation_plan(&rewritten).unwrap();
        plan.markdown = "# Revised".into();
        plan.updated_at = "2026-02-02T00:00:00Z".into();
        assert_eq!(
            store.conversation_plan(&source.id).unwrap(),
            Some(plan.clone())
        );

        store
            .set_conversation_plan_status(
                &source.id,
                crate::model::PlanStatus::Approved,
                "2026-03-03T00:00:00Z",
            )
            .unwrap();
        plan.status = crate::model::PlanStatus::Approved;
        plan.updated_at = "2026-03-03T00:00:00Z".into();
        assert_eq!(store.conversation_plan(&source.id).unwrap(), Some(plan));

        // A status the enum cannot produce is refused by the column itself.
        assert!(store
            .lock()
            .unwrap()
            .execute(
                "UPDATE conversation_plan SET status = 'whatever' WHERE conversation_id = ?1",
                [&source.id],
            )
            .is_err());

        store.delete_conversation(&source.id).unwrap();
        assert_eq!(store.conversation_plan(&source.id).unwrap(), None);
    }

    fn fork_decision(
        fork_id: &str,
        source: &str,
        decided_at: &str,
        approved: bool,
    ) -> crate::fork_requests::ForkDecisionRecord {
        crate::fork_requests::ForkDecisionRecord {
            fork_id: fork_id.into(),
            workspace_id: "ws".into(),
            source_conversation_id: source.into(),
            title: "继续做 B".into(),
            prompt: "继续做 B\n第二行".into(),
            requested_at: "2026-09-05T00:00:00Z".into(),
            decided_at: decided_at.into(),
            approved,
            child_conversation_id: approved.then(|| "child".to_owned()),
        }
    }

    /// The task bar reads this table after a reload, so the rows have to be
    /// there — and they have to go when the conversation that raised them does.
    #[test]
    fn fork_decisions_survive_reopen_and_die_with_their_source() {
        let (dir, store) = temp_store();
        let source = conversation("forker");
        let other = conversation("bystander");
        for row in [&source, &other] {
            store.put_conversation("ws", row).unwrap();
        }
        let approved = fork_decision("fork_b", &source.id, "2026-09-05T00:02:00Z", true);
        let declined = fork_decision("fork_a", &source.id, "2026-09-05T00:01:00Z", false);
        store.record_fork_decision(&approved).unwrap();
        store.record_fork_decision(&declined).unwrap();
        store
            .record_fork_decision(&fork_decision(
                "fork_c",
                &other.id,
                "2026-09-05T00:03:00Z",
                true,
            ))
            .unwrap();

        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        // Oldest first, and only this conversation's decisions.
        assert_eq!(
            store.fork_decisions(&source.id).unwrap(),
            vec![declined, approved]
        );

        // Answering the same card twice replaces the row rather than doubling it.
        let reanswered = fork_decision("fork_a", &source.id, "2026-09-05T01:00:00Z", true);
        store.record_fork_decision(&reanswered).unwrap();
        let rows = store.fork_decisions(&source.id).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| *row == reanswered));

        store.delete_conversation(&source.id).unwrap();
        assert!(store.fork_decisions(&source.id).unwrap().is_empty());
        assert_eq!(store.fork_decisions(&other.id).unwrap().len(), 1);
    }

    #[test]
    fn version_four_keeps_history_and_gains_the_fork_decision_table() {
        let (dir, store) = temp_store();
        let mut source = conversation("history");
        source.contexts.push(user("prompt", "hello"));
        store.put_conversation("ws", &source).unwrap();
        store
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE fork_decision; DROP TABLE history_op; DROP TABLE history_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE history_part; DROP TABLE history_blob; DROP TABLE history_entry; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 4;")
            .unwrap();
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store
                .conversation(&source.id)
                .unwrap()
                .unwrap()
                .contexts
                .len(),
            1
        );
        let record = fork_decision("fork_a", &source.id, "2026-09-05T00:01:00Z", true);
        store.record_fork_decision(&record).unwrap();
        assert_eq!(store.fork_decisions(&source.id).unwrap(), vec![record]);
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("quarantine-")
        }));
    }

    #[test]
    fn version_one_upgrades_in_place_without_quarantine() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE_NAME);
        let conn = Connection::open(&path).expect("open v1");
        // The released v1 conversation schema, without the parent column.
        conn.execute_batch(
            "CREATE TABLE conversation (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                order_key REAL NOT NULL,
                settings TEXT NOT NULL,
                worktree TEXT,
                run_target TEXT
             ) STRICT;",
        )
        .expect("v1 conversation schema");
        let (_, remaining_schema) = SCHEMA_SQL
            .split_once(") STRICT;")
            .expect("conversation table terminator");
        conn.execute_batch(remaining_schema)
            .expect("other v1 tables");
        let source = conversation("released_history");
        conn.execute(
            "INSERT INTO conversation
             (id, workspace_id, title, created_at, updated_at, order_key, settings)
             VALUES (?1, 'ws', ?2, ?3, ?4, 0, ?5)",
            rusqlite::params![
                source.id,
                source.title,
                source.created_at,
                source.updated_at,
                serde_json::to_string(&source.settings).expect("settings JSON"),
            ],
        )
        .expect("v1 row");
        conn.pragma_update(None, "user_version", 1)
            .expect("v1 version");
        drop(conn);

        let store = ConversationStore::open(&path).expect("upgrade");
        let loaded = store
            .conversation(&source.id)
            .expect("read")
            .expect("preserved row");
        assert_eq!(loaded.parent_conversation_id, None);
        assert_eq!(loaded, source);
        // Every table added after v1 exists after one open, not just the newest.
        assert!(store
            .pending_fork_starts()
            .expect("fork intents")
            .is_empty());
        assert_eq!(store.conversation_plan(&source.id).expect("plan"), None);
        assert!(store
            .fork_decisions(&source.id)
            .expect("fork decisions")
            .is_empty());
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION);
        assert!(std::fs::read_dir(dir.path())
            .expect("directory")
            .all(|entry| {
                !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("quarantine-")
            }));
    }

    #[test]
    fn parent_conversation_id_round_trips_on_insert_and_update() {
        let (_dir, store) = temp_store();
        let mut source = conversation("child");
        source.parent_conversation_id = Some("parent".into());
        store.put_conversation("ws", &source).expect("insert");
        assert_eq!(
            store.conversation("child").expect("read").expect("child"),
            source
        );
        source.parent_conversation_id = Some("grandparent".into());
        store.put_conversation("ws", &source).expect("update");
        assert_eq!(
            store.conversation("child").expect("read").expect("child"),
            source
        );
        source.parent_conversation_id = None;
        store.put_conversation("ws", &source).expect("clear parent");
        assert_eq!(
            store.conversation("child").expect("read").expect("child"),
            source
        );
    }

    #[test]
    fn fork_origin_round_trips_through_metadata_writes_and_clears() {
        let (_dir, store) = temp_store();
        let mut fork = conversation("fork");
        fork.fork_of = Some(ConversationForkOrigin {
            conversation_id: "origin".into(),
            number: 3,
        });
        store.put_conversation("ws", &fork).expect("insert");
        assert_eq!(store.conversation("fork").expect("read").expect("fork"), fork);
        // A title that follows the origin is a metadata write, and so is the
        // rename that detaches the fork from it.
        fork.title = "renamed".into();
        fork.fork_of = None;
        store
            .put_conversation_metadata("ws", &fork)
            .expect("metadata");
        assert_eq!(store.conversation("fork").expect("read").expect("fork"), fork);
    }

    #[test]
    fn handoff_origin_round_trips_through_inserts_and_metadata_writes() {
        let (_dir, store) = temp_store();
        let mut continuation = conversation("continuation");
        continuation.handoff_of = Some(ConversationHandoffOrigin {
            conversation_id: "origin".into(),
            number: 2,
        });
        store.put_conversation("ws", &continuation).expect("insert");
        assert_eq!(
            store.conversation("continuation").expect("read").expect("row"),
            continuation
        );
        // A rename is a metadata write and keeps the trace: the next handoff
        // still numbers under the same origin.
        continuation.title = "renamed".into();
        store
            .put_conversation_metadata("ws", &continuation)
            .expect("metadata");
        assert_eq!(
            store.conversation("continuation").expect("read").expect("row"),
            continuation
        );
    }

    #[test]
    fn preset_id_round_trips_on_insert_and_update() {
        let (_dir, store) = temp_store();
        let mut source = conversation("presetted");
        source.preset_id = "preset_codex".into();
        store.put_conversation("ws", &source).expect("insert");
        assert_eq!(
            store.conversation("presetted").expect("read").expect("row"),
            source
        );
        // The second write takes ON CONFLICT DO UPDATE SET, not the INSERT column list.
        source.preset_id = "preset_claude".into();
        store.put_conversation("ws", &source).expect("update");
        assert_eq!(
            store.conversation("presetted").expect("read").expect("row"),
            source
        );
        source.preset_id = String::new();
        store.put_conversation("ws", &source).expect("clear preset");
        assert_eq!(
            store.conversation("presetted").expect("read").expect("row"),
            source
        );
    }

    #[test]
    fn a_template_round_trips_its_name_count_and_ordered_body() {
        let (_dir, store) = temp_store();
        let body = vec![
            user("template_first", "first"),
            user("template_second", "second"),
        ];
        store
            .put_template("template_a", "开场模板", &body)
            .expect("write template");

        let templates = store.templates().expect("list templates");
        assert_eq!(templates.len(), 1, "应只读回刚写入的一条模板");
        assert_eq!(templates[0].id, "template_a", "模板身份必须原样保留");
        assert_eq!(templates[0].name, "开场模板", "模板名称必须原样保留");
        assert_eq!(templates[0].message_count, 2, "模板消息数必须与写入数一致");
        assert_eq!(
            store.template_contexts("template_a").expect("read body"),
            body,
            "模板正文必须按写入顺序原样读回"
        );
    }

    #[test]
    fn rewriting_a_template_replaces_its_body_and_keeps_created_at() {
        let (_dir, store) = temp_store();
        let original = store
            .put_template("template_a", "旧名称", &[user("old", "old body")])
            .expect("write original");
        let replacement = vec![user("new", "new body")];
        let rewritten = store
            .put_template("template_a", "新名称", &replacement)
            .expect("rewrite template");

        assert_eq!(
            rewritten.created_at, original.created_at,
            "覆盖模板不得改写最初创建时间"
        );
        let templates = store.templates().expect("list templates");
        assert_eq!(templates.len(), 1, "同一模板 ID 不得新增第二行");
        assert_eq!(templates[0].message_count, 1, "覆盖后的正文不得追加旧消息");
        assert_eq!(
            store
                .template_contexts("template_a")
                .expect("read replacement"),
            replacement,
            "覆盖后的正文只能包含新消息"
        );
    }

    #[test]
    fn deleting_a_template_removes_its_body_and_dangling_ids_read_as_empty() {
        let (_dir, store) = temp_store();
        store
            .put_template(
                "template_a",
                "待删除模板",
                &[user("template_message", "删除我")],
            )
            .expect("write template");

        store
            .delete_template("template_a")
            .expect("delete template");
        assert!(
            store.templates().expect("list templates").is_empty(),
            "删除后模板行必须消失"
        );
        assert_eq!(
            store
                .template_contexts("template_a")
                .expect("read deleted body"),
            Vec::<ContextItem>::new(),
            "删除后模板正文必须一并消失"
        );
        assert_eq!(
            store
                .template_contexts("never_seen")
                .expect("read dangling body"),
            Vec::<ContextItem>::new(),
            "从未存在的悬空模板 ID 必须读作空正文"
        );
    }

    #[test]
    fn template_id_round_trips_on_insert_and_update() {
        let (_dir, store) = temp_store();
        let mut source = conversation("templated");
        source.template_id = "template_first".into();
        store.put_conversation("ws", &source).expect("insert");
        assert_eq!(
            store.conversation("templated").expect("read").expect("row"),
            source,
            "插入时必须保存模板痕迹"
        );
        // The second write takes ON CONFLICT DO UPDATE SET, not the INSERT column list.
        source.template_id = "template_second".into();
        store.put_conversation("ws", &source).expect("update");
        assert_eq!(
            store.conversation("templated").expect("read").expect("row"),
            source,
            "更新时必须替换模板痕迹"
        );
        source.template_id = String::new();
        store
            .put_conversation("ws", &source)
            .expect("clear template");
        assert_eq!(
            store.conversation("templated").expect("read").expect("row"),
            source,
            "清空时必须保存空模板痕迹"
        );
    }

    #[test]
    fn version_seven_upgrades_in_place_and_gains_templates() {
        let (dir, store) = temp_store();
        let mut source = conversation("released_v7");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        store
            .lock()
            .expect("lock")
            .execute_batch(
                "DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE history_part; DROP TABLE history_blob; DROP TABLE history_entry; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 7;",
            )
            .expect("downgrade to v7");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("upgrade");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "v7 升级必须保留既有对话历史"
        );
        let template = store
            .put_template(
                "template_after_upgrade",
                "升级后模板",
                &[user("opening", "开场")],
            )
            .expect("write template after upgrade");
        assert_eq!(template.message_count, 1, "升级后必须能写入模板");
        assert_eq!(
            store
                .template_contexts("template_after_upgrade")
                .expect("read template after upgrade"),
            vec![user("opening", "开场")],
            "升级后必须能读取模板正文"
        );
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION, "升级必须写入当前版本号");
        assert!(
            std::fs::read_dir(dir.path())
                .expect("directory")
                .all(|entry| {
                    !entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .contains("quarantine-")
                }),
            "v7 升级不得隔离原数据库"
        );
    }

    fn request_record(conversation_id: &str, parts: &[(&str, &str)]) -> HistoryRequestRecord {
        request_record_of(
            conversation_id,
            parts
                .iter()
                .map(|(kind, body)| HistoryPartRecord {
                    kind: (*kind).to_owned(),
                    role: None,
                    author: None,
                    body: (*body).to_owned(),
                })
                .collect(),
        )
    }

    /// The same request with parts that carry the role and author the recorder
    /// derives, which is all the message delta ever looks at.
    fn request_record_of(
        conversation_id: &str,
        parts: Vec<HistoryPartRecord>,
    ) -> HistoryRequestRecord {
        HistoryRequestRecord {
            conversation_id: conversation_id.into(),
            owner: None,
            kind: "model".into(),
            request_id: "req_a".into(),
            round: 0,
            attempt: 0,
            provider_name: "anthropic".into(),
            family: "messages".into(),
            model_id: "claude-opus-5".into(),
            envelope: serde_json::json!({ "maxOutputTokens": 4096 }).to_string(),
            parts,
        }
    }

    /// The same request as a named child agent issued it.
    fn child_request_record(
        conversation_id: &str,
        owner: &str,
        parts: &[(&str, &str)],
    ) -> HistoryRequestRecord {
        HistoryRequestRecord {
            owner: Some(owner.into()),
            ..request_record(conversation_id, parts)
        }
    }

    fn message_part(role: &str, author: &str, body: &str) -> HistoryPartRecord {
        HistoryPartRecord {
            kind: "message".into(),
            role: Some(role.into()),
            author: Some(author.into()),
            body: body.into(),
        }
    }

    /// A response the way the recorder hands one over: answering `answers`, with
    /// whatever usage the provider disclosed.
    fn response_record(
        conversation_id: &str,
        owner: Option<&str>,
        answers: Option<i64>,
        usage: Option<HistoryUsage>,
        body: String,
    ) -> HistoryEntryRecord {
        let mut detail = serde_json::json!({
            "attempt": 1,
            "modelId": "claude-opus-5",
            "finishReason": "stop",
            "rawFinishReason": "end_turn",
        });
        if let Some(usage) = usage {
            detail["usage"] = serde_json::to_value(usage).expect("usage");
        }
        HistoryEntryRecord {
            conversation_id: conversation_id.into(),
            kind: "response",
            owner: owner.map(str::to_owned),
            request_id: Some("req_a".into()),
            round: Some(2),
            call_id: None,
            answers,
            detail,
            body: Some(body),
            body_cap: HISTORY_EVIDENCE_MAX_BYTES,
        }
    }

    /// A request entry with its detail read out, the way the pane reads one.
    #[derive(Debug)]
    struct RequestRow {
        seq: i64,
        created_at: String,
        kind: String,
        request_id: Option<String>,
        round: Option<i64>,
        attempt: Option<i64>,
        provider_name: String,
        family: String,
        model_id: String,
        part_count: i64,
        bytes: i64,
        usage: Option<HistoryUsage>,
        messages_added: Option<i64>,
        messages_removed: Option<i64>,
        owner: Option<String>,
    }

    fn request_rows(
        store: &ConversationStore,
        conversation_id: &str,
        owners: Option<&[String]>,
    ) -> Vec<RequestRow> {
        store
            .history_entries(conversation_id, owners)
            .expect("history")
            .into_iter()
            .filter(|entry| entry.kind == "request")
            .map(|entry| {
                let text = |key: &str| {
                    entry.detail[key]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                };
                let number = |key: &str| entry.detail[key].as_i64();
                RequestRow {
                    seq: entry.seq,
                    created_at: entry.created_at.clone(),
                    kind: text("type"),
                    request_id: entry.request_id.clone(),
                    round: entry.round,
                    attempt: number("attempt"),
                    provider_name: text("providerName"),
                    family: text("family"),
                    model_id: text("modelId"),
                    part_count: number("partCount").unwrap_or_default(),
                    bytes: number("bytes").unwrap_or_default(),
                    usage: entry.usage,
                    messages_added: number("messagesAdded"),
                    messages_removed: number("messagesRemoved"),
                    owner: entry.owner.clone(),
                }
            })
            .collect()
    }

    /// The trunk's requests, which is what the conversation's own pane reads.
    fn trunk_requests(store: &ConversationStore, conversation_id: &str) -> Vec<RequestRow> {
        request_rows(store, conversation_id, None)
    }

    fn entry_detail(store: &ConversationStore, conversation_id: &str, seq: i64) -> HistoryEntryDetail {
        store
            .history_entry(conversation_id, seq)
            .expect("read")
            .expect("entry")
    }

    fn row_count(store: &ConversationStore, query: &str) -> i64 {
        store
            .lock()
            .expect("lock")
            .query_row(query, [], |row| row.get(0))
            .expect("count")
    }

    /// The renderer reads these entries as JSON, so the names and the absences are
    /// part of the contract: an undisclosed counter must be missing rather than
    /// present and null, which is what the pane distinguishes on. A request carries
    /// the usage of the response that answered it.
    #[test]
    fn a_history_entry_serialises_the_shape_the_panel_reads() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_json"))
            .expect("seed");
        store
            .record_history_request(&request_record_of(
                "conv_json",
                vec![message_part("user", "user", "{\"role\":\"user\"}")],
            ))
            .expect("record");
        let usage = HistoryUsage {
            input_tokens: Some(12),
            cached_input_tokens: None,
            output_tokens: Some(3),
        };
        store
            .record_history_entry(&response_record(
                "conv_json",
                None,
                Some(1),
                Some(usage),
                "{\"role\":\"assistant\"}".into(),
            ))
            .expect("respond");

        let mut entries = store.history_entries("conv_json", None).expect("history");
        for entry in &mut entries {
            entry.created_at = "2026-08-25T00:00:00.000Z".into();
        }
        assert_eq!(
            serde_json::to_value(&entries).expect("serialise"),
            serde_json::json!([
                {
                    "seq": 1,
                    "createdAt": "2026-08-25T00:00:00.000Z",
                    "kind": "request",
                    "requestId": "req_a",
                    "round": 0,
                    "detail": {
                        "type": "model",
                        "attempt": 0,
                        "providerName": "anthropic",
                        "family": "messages",
                        "modelId": "claude-opus-5",
                        "partCount": 1,
                        "bytes": 15,
                        "messagesAdded": 1,
                        "messagesRemoved": 0,
                    },
                    "usage": { "inputTokens": 12, "outputTokens": 3 },
                },
                {
                    "seq": 2,
                    "createdAt": "2026-08-25T00:00:00.000Z",
                    "kind": "response",
                    "requestId": "req_a",
                    "round": 2,
                    "answers": 1,
                    "detail": {
                        "attempt": 1,
                        "modelId": "claude-opus-5",
                        "finishReason": "stop",
                        "rawFinishReason": "end_turn",
                        "usage": { "inputTokens": 12, "outputTokens": 3 },
                    },
                    "usage": { "inputTokens": 12, "outputTokens": 3 },
                },
            ])
        );

        // A request no response answered carries no `usage` key at all.
        store
            .record_history_request(&request_record("conv_json", &[("message", "{}")]))
            .expect("record");
        let unanswered = store
            .history_entries("conv_json", None)
            .expect("history")
            .remove(2);
        let unanswered = serde_json::to_value(&unanswered).expect("serialise");
        assert!(unanswered.get("usage").is_none());
        assert_eq!(unanswered["detail"]["messagesRemoved"], serde_json::json!(1));
    }

    /// Responses are kept per owner and read back whole — a body past the part cap
    /// included, since what it holds is what recovery checks — and go with their
    /// conversation.
    #[test]
    fn responses_are_kept_per_owner_whole_and_die_with_their_conversation() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_in"))
            .expect("seed");
        let script = "x".repeat(HISTORY_PART_MAX_BYTES + 1024);
        let trunk_body = serde_json::json!({"role": "assistant", "content": [
            {"type": "tool-call", "toolCallId": "c1", "toolName": "workflow", "input": {"script": script}}
        ]})
        .to_string();
        assert_eq!(
            store
                .record_history_entry(&response_record("conv_in", None, Some(7), None, trunk_body.clone()))
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            store
                .record_history_entry(&response_record(
                    "conv_in",
                    Some("reviewer"),
                    None,
                    None,
                    "{\"role\":\"assistant\"}".into()
                ))
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            store
                .record_history_entry(&response_record("conv_draft", None, None, None, "{}".into()))
                .unwrap(),
            None,
            "草稿没有历史记录"
        );

        let responses = |owner: Option<&str>, needle: &str| {
            store
                .history_records(
                    "conv_in",
                    HistoryFilter {
                        owner,
                        kinds: &["response"],
                        needle,
                        ..HistoryFilter::default()
                    },
                )
                .unwrap()
        };
        let trunk = responses(None, "");
        assert_eq!(trunk.len(), 1);
        assert_eq!(trunk[0].body.as_deref(), Some(trunk_body.as_str()), "超过分段上限的回复原样保存");
        assert!(!trunk[0].truncated);
        assert_eq!(trunk[0].answers, Some(7));
        assert_eq!((trunk[0].round, trunk[0].detail["attempt"].as_i64()), (Some(2), Some(1)));
        assert_eq!(trunk[0].detail_str("finishReason"), Some("stop"));
        assert_eq!(trunk[0].detail_str("rawFinishReason"), Some("end_turn"));
        assert_eq!(responses(Some("reviewer"), "").len(), 1);
        assert!(responses(None, "\"toolCallId\":\"c2\"").is_empty());
        assert_eq!(responses(None, "\"toolCallId\":\"c1\"").len(), 1);

        store.delete_conversation("conv_in").unwrap();
        assert!(responses(None, "").is_empty());
        assert!(responses(Some("reviewer"), "").is_empty());
        assert_eq!(row_count(&store, "SELECT count(*) FROM history_blob"), 0);
    }

    /// The record is append-only: one entry per request that went out, numbered
    /// from one, carrying what identified the request rather than what it said.
    #[test]
    fn requests_are_numbered_from_one_and_carry_what_identified_them() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_wire"))
            .expect("seed");

        let parts = [
            ("system", "you are a cat"),
            ("message", "{\"role\":\"user\"}"),
        ];
        store
            .record_history_request(&request_record("conv_wire", &parts))
            .expect("first request");
        let mut second = request_record("conv_wire", &parts[..1]);
        second.request_id = "req_b".into();
        second.round = 3;
        second.attempt = 1;
        second.kind = "search".into();
        store.record_history_request(&second).expect("second request");

        let recorded = trunk_requests(&store, "conv_wire");
        assert_eq!(recorded.len(), 2);
        assert_eq!(
            recorded.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [1, 2],
            "序号从 1 开始并逐条递增"
        );
        assert_eq!(recorded[0].kind, "model");
        assert_eq!(recorded[0].request_id.as_deref(), Some("req_a"));
        assert_eq!((recorded[0].round, recorded[0].attempt), (Some(0), Some(0)));
        assert_eq!(recorded[0].provider_name, "anthropic");
        assert_eq!(recorded[0].family, "messages");
        assert_eq!(recorded[0].model_id, "claude-opus-5");
        assert_eq!(recorded[0].part_count, 2);
        assert_eq!(
            recorded[0].bytes,
            parts.iter().map(|(_, body)| body.len() as i64).sum::<i64>(),
            "bytes 记的是这次请求发出去的大小"
        );
        assert!(!recorded[0].created_at.is_empty());
        assert_eq!(recorded[1].kind, "search");
        assert_eq!((recorded[1].round, recorded[1].attempt), (Some(3), Some(1)));
        assert_eq!(recorded[1].part_count, 1);
    }

    /// The point of content addressing: a turn's second round re-sends the first
    /// round's messages, and the record must not pay for them twice. Each request
    /// still names every part it carried.
    #[test]
    fn a_repeated_body_is_stored_once_and_named_twice() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_dedupe"))
            .expect("seed");

        let shared = ("message", "{\"role\":\"user\",\"content\":\"hi\"}");
        store
            .record_history_request(&request_record("conv_dedupe", &[("system", "rules"), shared]))
            .expect("first round");
        store
            .record_history_request(&request_record(
                "conv_dedupe",
                &[
                    ("system", "rules"),
                    shared,
                    ("message", "{\"role\":\"assistant\"}"),
                ],
            ))
            .expect("second round");

        assert_eq!(
            row_count(&store, "SELECT count(*) FROM history_blob"),
            4,
            "重复的正文只占一行：三段不同正文加上两次请求共用的信封"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM history_part"),
            5,
            "每次请求仍然点名自己带过的每一段"
        );
        let detail = entry_detail(&store, "conv_dedupe", 2);
        assert_eq!(detail.parts[1].body, shared.1, "共享正文读回的是原文");
        assert_eq!(
            detail.parts[1].hash,
            entry_detail(&store, "conv_dedupe", 1).parts[1].hash,
            "同一段正文在两次请求里是同一个地址"
        );
    }

    /// The detail view reads a request the way it went out: the parts in wire
    /// order, and the envelope that carried them.
    #[test]
    fn a_request_reads_back_its_parts_in_wire_order_with_its_envelope() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_detail"))
            .expect("seed");
        let parts = [
            ("system", "静态提示"),
            ("systemDynamic", "当前时间"),
            ("tools", "[{\"name\":\"shell\"}]"),
            ("message", "{\"role\":\"user\"}"),
        ];
        store
            .record_history_request(&request_record("conv_detail", &parts))
            .expect("record");

        let detail = entry_detail(&store, "conv_detail", 1);
        assert_eq!(detail.entry.seq, 1);
        assert_eq!(detail.entry.detail["partCount"], serde_json::json!(4));
        assert_eq!(
            detail.body.as_deref(),
            Some(serde_json::json!({ "maxOutputTokens": 4096 }).to_string().as_str())
        );
        assert_eq!(
            detail
                .parts
                .iter()
                .map(|part| (part.ordinal, part.kind.as_str(), part.body.as_str()))
                .collect::<Vec<_>>(),
            parts
                .iter()
                .enumerate()
                .map(|(ordinal, (kind, body))| (ordinal as i64, *kind, *body))
                .collect::<Vec<_>>(),
            "分段按上线顺序读回"
        );
        assert!(detail.parts.iter().all(|part| !part.truncated));
        assert!(detail.ops.is_empty());
        assert_eq!(store.history_entry("conv_detail", 7).expect("read"), None);

        // A corrupt detail costs its own field, not the parts recorded beside it.
        store
            .lock()
            .expect("lock")
            .execute(
                "UPDATE history_entry SET detail = 'not json' WHERE conversation_id = 'conv_detail'",
                [],
            )
            .expect("corrupt");
        let corrupt = entry_detail(&store, "conv_detail", 1);
        assert_eq!(corrupt.entry.detail, serde_json::json!({}));
        assert_eq!(corrupt.parts.len(), 4);
    }

    #[test]
    fn a_conversation_without_a_row_records_nothing() {
        let (_dir, store) = temp_store();
        store
            .record_history_request(&request_record("subagent_only", &[("message", "{}")]))
            .expect("库里没有行的对话不入账也不报错");
        assert!(trunk_requests(&store, "subagent_only").is_empty());
        assert_eq!(row_count(&store, "SELECT count(*) FROM history_entry"), 0);
        assert_eq!(row_count(&store, "SELECT count(*) FROM history_blob"), 0);
    }

    /// A child runs under its parent's conversation id, so the only thing keeping
    /// its entries out of the session's own is `owner`. Read the trunk and a child
    /// never appears; read the child and the trunk never does.
    #[test]
    fn a_childs_entries_are_its_own() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_owned"))
            .expect("seed");
        store
            .record_history_request(&request_record("conv_owned", &[("message", "{\"t\":1}")]))
            .expect("trunk");
        store
            .record_history_request(&child_request_record(
                "conv_owned",
                "reviewer",
                &[("message", "{\"c\":1}")],
            ))
            .expect("child");
        store
            .record_history_request(&child_request_record(
                "conv_owned",
                "reviewer",
                &[("message", "{\"c\":2}")],
            ))
            .expect("child again");
        store
            .record_history_request(&request_record("conv_owned", &[("message", "{\"t\":2}")]))
            .expect("trunk again");

        let trunk = trunk_requests(&store, "conv_owned");
        assert_eq!(
            trunk.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![1, 4],
            "主干只有主干自己发出的请求"
        );
        assert!(trunk.iter().all(|row| row.owner.is_none()));

        let owners = ["reviewer".to_owned()];
        let child = request_rows(&store, "conv_owned", Some(&owners));
        assert_eq!(
            child.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![2, 3],
            "子代理只有它自己发出的请求"
        );
        assert_eq!(child[0].owner.as_deref(), Some("reviewer"));
        assert_eq!(
            entry_detail(&store, "conv_owned", 2).entry.owner.as_deref(),
            Some("reviewer"),
            "逐条读回来也带着归属"
        );
        assert!(
            store
                .history_entries("conv_owned", Some(&[]))
                .expect("no owners")
                .is_empty(),
            "还没有被寻址过的代理什么都没发出去，不能拿主干的行搪塞"
        );
    }

    /// The request a payload is read against is the previous one *of the same
    /// owner*. Against the trunk's, a child's first payload would report the
    /// session's whole history as deleted.
    #[test]
    fn a_child_request_is_read_against_its_own_predecessor() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_delta_owned"))
            .expect("seed");
        store
            .record_history_request(&request_record_of(
                "conv_delta_owned",
                vec![message_part("user", "user", "{\"role\":\"user\",\"t\":1}")],
            ))
            .expect("trunk");
        store
            .record_history_request(&HistoryRequestRecord {
                owner: Some("writer".into()),
                ..request_record_of(
                    "conv_delta_owned",
                    vec![message_part("user", "user", "{\"role\":\"user\",\"c\":1}")],
                )
            })
            .expect("child");

        let owners = ["writer".to_owned()];
        let child = request_rows(&store, "conv_delta_owned", Some(&owners));
        assert_eq!(
            (child[0].messages_added, child[0].messages_removed),
            (Some(1), Some(0)),
            "子代理的第一条载荷之前什么都没有，不得把主干的历史算成被删"
        );
    }

    /// A child that runs for hundreds of rounds adds to its own entries only; the
    /// trunk's stay exactly as they were, and so does every one of the child's.
    #[test]
    fn a_chatty_child_does_not_evict_the_trunks_history() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_shared_cap"))
            .expect("seed");
        store
            .record_history_request(&request_record(
                "conv_shared_cap",
                &[("message", "{\"trunk\":1}")],
            ))
            .expect("trunk");
        let rounds = 305;
        for index in 1..=rounds {
            let unique = format!("{{\"child\":{index}}}");
            store
                .record_history_request(&child_request_record(
                    "conv_shared_cap",
                    "looper",
                    &[("message", unique.as_str())],
                ))
                .expect("child");
        }

        let trunk = trunk_requests(&store, "conv_shared_cap");
        assert_eq!(
            trunk.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![1],
            "子代理再吵也不得挤掉主干的行"
        );
        let owners = ["looper".to_owned()];
        assert_eq!(
            request_rows(&store, "conv_shared_cap", Some(&owners)).len(),
            rounds as usize,
            "子代理自己的记录也一条不删"
        );
    }

    /// Every kind of entry takes its number from one sequence, so "what came after"
    /// has an answer across kinds — the question recovery asks. Evidence reads pick
    /// entries by owner, kind, call and body.
    #[test]
    fn every_kind_of_entry_shares_one_sequence_and_reads_back_as_evidence() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_all");
        store.put_conversation("ws", &source).expect("seed");
        store
            .record_history_request(&request_record("conv_all", &[("message", "{}")]))
            .expect("request");
        store
            .record_history_entry(&response_record(
                "conv_all",
                None,
                Some(1),
                None,
                serde_json::json!({"role": "assistant", "content": [
                    {"type": "tool-call", "toolCallId": "c1", "toolName": "shell", "input": {"command": "ls"}}
                ]})
                .to_string(),
            ))
            .expect("response");
        let entry = |kind: &'static str, detail: serde_json::Value, body: serde_json::Value| {
            HistoryEntryRecord {
                conversation_id: "conv_all".into(),
                kind,
                owner: None,
                request_id: Some("req_a".into()),
                round: Some(0),
                call_id: Some("c1".into()),
                answers: None,
                detail,
                body: Some(body.to_string()),
                body_cap: HISTORY_EVIDENCE_MAX_BYTES,
            }
        };
        store
            .record_history_entry(&entry(
                "hook",
                serde_json::json!({"event": "PreToolUse", "rewroteInput": true}),
                serde_json::json!({"output": "", "updatedInput": {"command": "ls -a"}}),
            ))
            .expect("hook");
        store
            .record_history_entry(&entry(
                "tool",
                serde_json::json!({"name": "shell", "rewritten": true}),
                serde_json::json!({"input": {"command": "ls -a"}, "requestedInput": {"command": "ls"}}),
            ))
            .expect("tool");
        store
            .record_history_entry(&entry(
                "result",
                serde_json::json!({"name": "shell", "success": true}),
                serde_json::json!({"output": "a b"}),
            ))
            .expect("result");
        source.contexts.push(user("ctx_next", "再来"));
        store.put_conversation("ws", &source).expect("send");

        let entries = store.history_entries("conv_all", None).expect("history");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.seq, entry.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, "request"),
                (2, "response"),
                (3, "hook"),
                (4, "tool"),
                (5, "result"),
                (6, "edit"),
            ],
            "所有种类共用一个序号"
        );
        assert_eq!(entries[5].detail["source"], serde_json::json!("message"));

        let about_the_call = store
            .history_records(
                "conv_all",
                HistoryFilter {
                    call_id: Some("c1"),
                    ..HistoryFilter::default()
                },
            )
            .expect("call");
        assert_eq!(
            about_the_call
                .iter()
                .map(|record| record.kind.as_str())
                .collect::<Vec<_>>(),
            ["hook", "tool", "result"]
        );
        let ran = store
            .history_records(
                "conv_all",
                HistoryFilter {
                    kinds: &["tool"],
                    call_id: Some("c1"),
                    ..HistoryFilter::default()
                },
            )
            .expect("tool");
        assert_eq!(
            crate::history::recorded_tool_input(ran[0].body.as_deref().expect("body")),
            Some(serde_json::json!({"command": "ls -a"})),
            "执行时的输入是钩子改写之后的"
        );
        assert!(ran[0].detail_flag("rewritten"));
        assert!(store
            .history_records(
                "conv_all",
                HistoryFilter {
                    owner: Some("someone"),
                    ..HistoryFilter::default()
                },
            )
            .expect("owner")
            .is_empty());
    }

    /// The tables the history replaced, as older builds shipped them.
    const LEGACY_SCHEMA: &str = "CREATE TABLE timeline_event (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('baseline', 'run', 'edit')),
        request_id TEXT,
        inserted INTEGER NOT NULL,
        removed INTEGER NOT NULL,
        replaced INTEGER NOT NULL,
        row_count INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (conversation_id, seq)
    ) STRICT;
    CREATE TABLE timeline_op (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        ordinal INTEGER NOT NULL,
        op TEXT NOT NULL CHECK (op IN ('remove', 'insert', 'replace')),
        context_id TEXT NOT NULL,
        position INTEGER,
        data TEXT,
        PRIMARY KEY (conversation_id, seq, ordinal)
    ) STRICT;
    CREATE TABLE timeline_head (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        context_id TEXT NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (conversation_id, position)
    ) STRICT;
    CREATE TABLE wire_request (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('model', 'search', 'fetch')),
        request_id TEXT NOT NULL,
        round INTEGER NOT NULL,
        attempt INTEGER NOT NULL,
        provider_name TEXT NOT NULL,
        family TEXT NOT NULL,
        model_id TEXT NOT NULL,
        envelope TEXT NOT NULL,
        part_count INTEGER NOT NULL,
        bytes INTEGER NOT NULL,
        input_tokens INTEGER,
        cached_input_tokens INTEGER,
        output_tokens INTEGER,
        messages_added INTEGER,
        messages_removed INTEGER,
        owner TEXT,
        PRIMARY KEY (conversation_id, seq)
    ) STRICT;
    CREATE TABLE wire_blob (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        hash TEXT NOT NULL,
        body TEXT NOT NULL,
        truncated INTEGER NOT NULL,
        PRIMARY KEY (conversation_id, hash)
    ) STRICT;
    CREATE TABLE wire_request_part (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        ordinal INTEGER NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('system', 'systemDynamic', 'tools', 'message')),
        hash TEXT NOT NULL,
        role TEXT,
        PRIMARY KEY (conversation_id, seq, ordinal)
    ) STRICT;
    CREATE TABLE wire_response (
        conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        received_at TEXT NOT NULL,
        request_seq INTEGER,
        owner TEXT,
        request_id TEXT NOT NULL,
        round INTEGER NOT NULL,
        attempt INTEGER NOT NULL,
        model_id TEXT,
        finish_reason TEXT,
        raw_finish_reason TEXT,
        hash TEXT NOT NULL,
        PRIMARY KEY (conversation_id, seq)
    ) STRICT;";

    /// Takes a store back to before the history: its tables gone, the legacy ones
    /// in their place.
    fn install_legacy_tables(store: &ConversationStore) {
        store
            .lock()
            .expect("lock")
            .execute_batch(&format!(
                "DROP TABLE history_op; DROP TABLE history_part; DROP TABLE history_head;
                 DROP TABLE history_entry; DROP TABLE history_blob; {LEGACY_SCHEMA}"
            ))
            .expect("legacy tables");
    }

    fn quarantined_nothing(dir: &Path) -> bool {
        std::fs::read_dir(dir).expect("directory").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains("quarantine-")
        })
    }

    /// A store from before the history keeps what its two ledgers and its trunk
    /// history held: every row moves into the one record, interleaved in the order
    /// it was written, with a response still naming its request and a request still
    /// carrying the usage the old ledger gave it. The legacy tables go. A legacy
    /// table still standing is what says the store is not migrated, whatever its
    /// stamp claims.
    #[test]
    fn legacy_ledgers_and_trunk_history_migrate_into_one_record_in_the_order_they_happened() {
        let (dir, store) = temp_store();
        let mut source = conversation("legacy");
        source.contexts = vec![
            user("ask", "问"),
            assistant("reply", "答", 0),
            user("again", "再问"),
        ];
        store.put_conversation("ws", &source).expect("seed");
        install_legacy_tables(&store);
        {
            let conn = store.lock().expect("lock");
            let rows: Vec<(String, String)> = conn
                .prepare("SELECT id, data FROM context WHERE conversation_id = 'legacy' ORDER BY order_key")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let at = |second: u32| format!("2026-09-01T00:00:{second:02}.000Z");
            let event = |seq: i64, kind: &str, request: Option<&str>, row_count: i64, second: u32| {
                conn.execute(
                    "INSERT INTO timeline_event VALUES ('legacy', ?1, ?2, ?3, 1, 0, 0, ?4, ?5)",
                    rusqlite::params![seq, kind, request, row_count, at(second)],
                )
                .unwrap();
                let position = row_count - 1;
                conn.execute(
                    "INSERT INTO timeline_op VALUES ('legacy', ?1, 0, 'insert', ?2, ?3, ?4)",
                    rusqlite::params![seq, rows[position as usize].0, position, rows[position as usize].1],
                )
                .unwrap();
            };
            event(1, "baseline", None, 1, 0);
            conn.execute_batch(
                r#"INSERT INTO wire_blob VALUES
                     ('legacy', 'h_sys', '规则', 0),
                     ('legacy', 'h_ask', '{"role":"user","content":"问"}', 0),
                     ('legacy', 'h_child', '{"role":"user","content":"子"}', 0),
                     ('legacy', 'h_reply', '{"role":"assistant","content":[{"type":"text","text":"答"}]}', 0);
                   INSERT INTO wire_request VALUES
                     ('legacy', 1, '2026-09-01T00:00:01.000Z', 'model', 'req_1', 0, 1, 'anthropic',
                      'messages', 'claude-opus-5', '{"maxOutputTokens":4096}', 2, 30, 100, 80, 7, 1, 0, NULL),
                     ('legacy', 2, '2026-09-01T00:00:02.000Z', 'model', 'req_c', 0, 1, 'anthropic',
                      'messages', 'claude-opus-5', '{}', 1, 10, NULL, NULL, NULL, 1, 0, 'reviewer');
                   INSERT INTO wire_request_part VALUES
                     ('legacy', 1, 0, 'system', 'h_sys', NULL),
                     ('legacy', 1, 1, 'message', 'h_ask', 'user'),
                     ('legacy', 2, 0, 'message', 'h_child', 'user');
                   INSERT INTO wire_response VALUES
                     ('legacy', 1, '2026-09-01T00:00:03.000Z', 1, NULL, 'req_1', 0, 1,
                      'claude-opus-5', 'stop', 'end_turn', 'h_reply');"#,
            )
            .unwrap();
            event(2, "run", Some("req_1"), 2, 4);
            event(3, "edit", None, 3, 5);
            for (position, (id, data)) in rows.iter().enumerate() {
                conn.execute(
                    "INSERT INTO timeline_head VALUES ('legacy', ?1, ?2, ?3)",
                    rusqlite::params![position as i64, id, data],
                )
                .unwrap();
            }
            conn.pragma_update(None, "user_version", 18).unwrap();
        }
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("migrate");
        {
            let conn = store.lock().expect("lock");
            for table in LEGACY_HISTORY_TABLES {
                assert!(!has_table(&conn, table).unwrap(), "{table} 迁移后必须删掉");
            }
            let version: i32 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, STORE_VERSION);
        }
        let trunk = store.history_entries("legacy", None).expect("history");
        assert_eq!(
            trunk
                .iter()
                .map(|entry| (entry.seq, entry.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "edit"), (2, "request"), (4, "response"), (5, "run"), (6, "edit")],
            "按写入时间交错排成一条序列；子代理的请求占 3 号但不在主干里"
        );
        assert_eq!(trunk[0].detail["source"], serde_json::json!("baseline"));
        assert_eq!(trunk[3].detail["source"], serde_json::json!("run"));
        assert_eq!(trunk[3].request_id.as_deref(), Some("req_1"));
        assert_eq!(
            trunk[4].detail["source"],
            serde_json::json!("message"),
            "只在末尾追加用户消息的旧改动读作发送"
        );
        assert_eq!(trunk[2].answers, Some(2), "回复仍指向它回答的那次请求");
        assert_eq!(
            trunk[1].usage,
            Some(HistoryUsage {
                input_tokens: Some(100),
                cached_input_tokens: Some(80),
                output_tokens: Some(7),
            }),
            "旧账本记在请求上的用量保留"
        );
        assert_eq!(trunk[1].detail["messagesAdded"], serde_json::json!(1));

        let request = entry_detail(&store, "legacy", 2);
        assert_eq!(request.body.as_deref(), Some("{\"maxOutputTokens\":4096}"));
        assert_eq!(
            request
                .parts
                .iter()
                .map(|part| part.body.as_str())
                .collect::<Vec<_>>(),
            ["规则", "{\"role\":\"user\",\"content\":\"问\"}"]
        );
        let reply = store
            .history_records(
                "legacy",
                HistoryFilter {
                    kinds: &["response"],
                    ..HistoryFilter::default()
                },
            )
            .expect("responses");
        assert_eq!(
            crate::history::recorded_message_text(reply[0].body.as_deref().unwrap()),
            "答"
        );
        assert_eq!(reply[0].detail_str("rawFinishReason"), Some("end_turn"));

        let owners = ["reviewer".to_owned()];
        let child = request_rows(&store, "legacy", Some(&owners));
        assert_eq!(child.iter().map(|row| row.seq).collect::<Vec<_>>(), vec![3]);

        assert_eq!(
            store
                .trunk_snapshot("legacy", 5)
                .expect("snapshot")
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            ["ask", "reply"]
        );
        // The head came along, so an unchanged trunk records nothing new.
        store
            .record_trunk_change("legacy", TrunkChange::Edit, None)
            .expect("record");
        assert_eq!(store.trunk_changes("legacy").expect("changes").len(), 3);
        assert!(quarantined_nothing(dir.path()), "迁移不得隔离原数据库");
    }

    /// A legacy request ledger written before it counted anything — no usage, no
    /// delta, no owner, no roles — migrates as the trunk's, with those absences
    /// intact, even behind a stamp that already reads current. New requests then
    /// read against it as before.
    #[test]
    fn a_legacy_ledger_from_before_its_counters_migrates_with_its_absences() {
        let (dir, store) = temp_store();
        let mut source = conversation("legacy_v10");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        install_legacy_tables(&store);
        let carried_over = "{\"role\":\"user\",\"content\":\"旧的\"}";
        // The address the old ledger gave it, which is the one a new request computes.
        let old_hash = body_hash(carried_over);
        store
            .lock()
            .expect("lock")
            .execute_batch(&format!(
                "ALTER TABLE wire_request DROP COLUMN input_tokens;
                 ALTER TABLE wire_request DROP COLUMN cached_input_tokens;
                 ALTER TABLE wire_request DROP COLUMN output_tokens;
                 ALTER TABLE wire_request DROP COLUMN messages_added;
                 ALTER TABLE wire_request DROP COLUMN messages_removed;
                 ALTER TABLE wire_request DROP COLUMN owner;
                 ALTER TABLE wire_request_part DROP COLUMN role;
                 INSERT INTO wire_blob VALUES ('legacy_v10', '{old_hash}', '{carried_over}', 0);
                 INSERT INTO wire_request VALUES ('legacy_v10', 1, '2026-09-01T00:00:01.000Z', 'model',
                   'req_old', 0, 1, 'anthropic', 'messages', 'claude-opus-5', '{{}}', 1, 26);
                 INSERT INTO wire_request_part VALUES ('legacy_v10', 1, 0, 'message', '{old_hash}');
                 PRAGMA user_version = {STORE_VERSION};"
            ))
            .expect("legacy v10 ledger");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("migrate");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "迁移必须保留既有对话历史"
        );
        let carried = trunk_requests(&store, &source.id);
        assert_eq!(carried.len(), 1, "旧行必须留在主干里");
        assert_eq!(
            (
                carried[0].usage,
                carried[0].messages_added,
                carried[0].messages_removed,
                carried[0].owner.clone()
            ),
            (None, None, None, None),
            "旧行没有的数字读回来必须是空"
        );

        store
            .record_history_request(&request_record_of(
                &source.id,
                vec![
                    // The body the legacy row carried, whose role was never kept.
                    message_part("user", "user", carried_over),
                    message_part("user", "user", "{\"role\":\"user\",\"content\":\"新的\"}"),
                ],
            ))
            .expect("write after migration");
        let recorded = trunk_requests(&store, &source.id);
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(1), Some(0)),
            "迁移后必须能写入并读回增删计数"
        );
        let owners = ["late-agent".to_owned()];
        store
            .record_history_request(&child_request_record(
                &source.id,
                "late-agent",
                &[("message", "{\"role\":\"user\",\"content\":\"子的\"}")],
            ))
            .expect("write child after migration");
        assert_eq!(request_rows(&store, &source.id, Some(&owners)).len(), 1);
        assert_eq!(trunk_requests(&store, &source.id).len(), 2, "主干不被子代理的行污染");
        assert!(quarantined_nothing(dir.path()), "迁移不得隔离原数据库");
    }

    fn queued_with_files(id: &str) -> QueuedMessage {
        QueuedMessage {
            id: id.into(),
            content: String::new(),
            images: Vec::new(),
            files: vec![FileAttachment {
                id: "a".repeat(64),
                name: "report.pdf".into(),
                format: crate::model::FileAttachmentFormat::Pdf,
                bytes: 2048,
                tokens: 300,
                pages: Some(4),
            }],
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }
    }

    #[test]
    fn queued_message_files_round_trip() {
        let (_dir, store) = temp_store();
        let mut source = conversation("queued_files");
        source.queued_messages = vec![
            queued_with_files("queued_1"),
            QueuedMessage {
                id: "queued_2".into(),
                content: "text only".into(),
                images: Vec::new(),
                files: Vec::new(),
                created_at: "2026-08-25T00:00:05.000Z".into(),
            },
        ];
        store.put_conversation("ws", &source).expect("put");
        let loaded = store
            .conversation(&source.id)
            .expect("read")
            .expect("present");
        assert_eq!(loaded.queued_messages, source.queued_messages);
    }

    /// v15 added `queued_message.files`. A v14 store keeps its queue, reads
    /// each old row as carrying no files, and can store files afterwards.
    #[test]
    fn version_fourteen_upgrades_in_place_and_reads_old_queue_rows_without_files() {
        let (dir, store) = temp_store();
        let mut source = conversation("released_v14");
        source.contexts.push(user("history", "保留的历史"));
        source.queued_messages = vec![QueuedMessage {
            id: "queued_old".into(),
            content: "排队中".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }];
        store.put_conversation("ws", &source).expect("seed");
        store
            .lock()
            .expect("lock")
            .execute_batch(
                "ALTER TABLE queued_message DROP COLUMN files; PRAGMA user_version = 14;",
            )
            .expect("downgrade to v14");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("upgrade");
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION, "升级必须写入当前版本号");
        let loaded = store
            .conversation(&source.id)
            .expect("read")
            .expect("preserved row");
        assert_eq!(
            loaded.contexts, source.contexts,
            "v14 升级必须保留既有对话历史"
        );
        assert_eq!(
            loaded.queued_messages, source.queued_messages,
            "旧行按无附件读出"
        );

        source.queued_messages.push(queued_with_files("queued_new"));
        store
            .put_conversation("ws", &source)
            .expect("write files after upgrade");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read")
                .expect("present")
                .queued_messages,
            source.queued_messages
        );
    }

    /// The same rule for a whole table: repair is driven by the shape on disk, so a
    /// table lost behind a current stamp comes back without touching the rest.
    #[test]
    fn a_store_stamped_current_but_missing_a_table_is_rebuilt_on_open() {
        let (dir, store) = temp_store();
        let mut source = conversation("stamped_current_table");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        store
            .lock()
            .expect("lock")
            .execute_batch("DROP TABLE conversation_plan")
            .expect("drop a table behind a current stamp");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("repair");
        assert_eq!(
            store.conversation_plan(&source.id).expect("plan"),
            None,
            "丢掉的表必须重新建起来，而不是让每次读计划都失败"
        );
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "重建一张表不得动到既有对话历史"
        );
    }

    /// Quarantine sets a store aside; it must not empty it on the way. A store in
    /// WAL mode that was never checkpointed keeps its rows in the log and only a
    /// header in the main file, so the log has to travel with the file it belongs
    /// to — SQLite finds a log by name — or what gets filed away is an empty shell.
    #[test]
    fn quarantine_carries_the_write_ahead_log_with_the_file_it_sets_aside() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE_NAME);
        std::fs::write(&path, b"main").expect("main file");
        std::fs::write(
            dir.path().join(format!("{DATABASE_FILE_NAME}-wal")),
            b"log",
        )
        .expect("log file");
        std::fs::write(
            dir.path().join(format!("{DATABASE_FILE_NAME}-shm")),
            b"shared",
        )
        .expect("shared memory file");

        quarantine_database(&path, "测试");

        assert!(!path.exists(), "原路径必须腾空给重建");
        let mut filed: Vec<String> = std::fs::read_dir(dir.path())
            .expect("directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        filed.sort();
        assert_eq!(
            filed.len(),
            2,
            "封存的是库与日志两份，共享内存文件可重建、不留：{filed:?}"
        );
        let main = filed
            .iter()
            .find(|name| name.ends_with(".sqlite3"))
            .expect("封存的库");
        let log = filed
            .iter()
            .find(|name| name.ends_with(".sqlite3-wal"))
            .expect("封存的日志");
        assert_eq!(
            log,
            &format!("{main}-wal"),
            "SQLite 按名字找日志：日志必须跟着改名，否则封存下来的只是个空壳"
        );
        assert_eq!(
            std::fs::read(dir.path().join(log)).expect("read log"),
            b"log",
            "日志内容必须原样搬过去"
        );
    }

    /// A file this build cannot read is set aside and replaced, however current its
    /// stamp reads. The stamp lives in page one and survives damage to everything
    /// after it, so a corrupt store can still present itself as up to date; what
    /// decides is whether the schema can actually be read back. Trusting the stamp
    /// here would leave every read failing with `database disk image is malformed`
    /// for as long as the file stayed in place.
    #[test]
    fn a_corrupt_store_behind_a_current_stamp_is_quarantined_and_rebuilt() {
        let (dir, store) = temp_store();
        let mut source = conversation("corrupted");
        source.contexts.push(user("history", "损坏前的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        drop(store);

        let path = dir.path().join(DATABASE_FILE_NAME);
        let mut bytes = std::fs::read(&path).expect("read store");
        assert!(bytes.len() > 4096 * 2, "夹具至少要有几页才谈得上损坏");
        // Page one carries the header and the version stamp; everything after it,
        // the schema page included, is wiped.
        for byte in bytes.iter_mut().skip(4096) {
            *byte = 0;
        }
        std::fs::write(&path, &bytes).expect("corrupt store");

        let store = ConversationStore::open(&path).expect("rebuild after quarantine");
        assert_eq!(
            store.conversation(&source.id).expect("read"),
            None,
            "重建出来的必须是一个空库"
        );
        store
            .put_conversation("ws", &conversation("after"))
            .expect("重建后必须能正常写入");
        let quarantined: Vec<String> = std::fs::read_dir(dir.path())
            .expect("directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.contains("quarantine-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "损坏的库必须原样留一份在旁边，而不是当场丢掉：{quarantined:?}"
        );
    }


    /// Repair leans on two things at once: every creation statement being safe to
    /// re-run, and the table names being readable out of those same statements. A
    /// `CREATE TABLE` written without `IF NOT EXISTS` breaks both — it fails the
    /// pass on any store that already has the table, and it drops out of the shape
    /// check that decides whether a store needs repairing at all, which is exactly
    /// how a missing piece goes unnoticed behind a current stamp.
    #[test]
    fn every_creation_statement_is_idempotent_and_names_itself() {
        for schema in SCHEMAS {
            assert_eq!(
                schema.matches("CREATE TABLE ").count(),
                schema.matches("CREATE TABLE IF NOT EXISTS ").count(),
                "建表语句必须写成 IF NOT EXISTS，否则修复会在已有该表的库上失败：{schema}"
            );
            assert_eq!(
                schema.matches("CREATE INDEX ").count(),
                schema.matches("CREATE INDEX IF NOT EXISTS ").count(),
                "建索引语句同理：{schema}"
            );
        }
        let declared: Vec<&str> = declared_tables().collect();
        for expected in [
            "conversation",
            "branch",
            "context",
            "queued_message",
            "aborted_task",
            "pending_fork_start",
            "conversation_plan",
            "fork_decision",
            "conversation_template",
            "template_context",
            "history_entry",
            "history_blob",
            "history_part",
            "history_op",
            "history_head",
            "tool_explanation",
            "tool_error_explanation",
        ] {
            assert!(
                declared.contains(&expected),
                "{expected} 必须能从建表语句里读出来：{declared:?}"
            );
        }
        assert_eq!(declared.len(), 17, "读出的表名与实际建的表不符：{declared:?}");
        for table in LEGACY_HISTORY_TABLES {
            assert!(
                !declared.contains(&table),
                "{table} 已并入历史记录，不得再建"
            );
        }
        for &(table, _, _) in ADDED_COLUMNS {
            assert!(
                declared.contains(&table),
                "{table} 不在建表语句里，补列会打在一张不存在的表上"
            );
        }
    }



    /// A body past the cap is stored cut, and the address is the address of what
    /// was stored: a reader can hash the text they were given and get the hash the
    /// record holds.
    #[test]
    fn an_oversized_part_is_stored_cut_and_addressed_as_stored() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_big"))
            .expect("seed");
        // Three-byte characters, so the cap does not land on a boundary.
        let huge = "漢".repeat(HISTORY_PART_MAX_BYTES / 3 + 16);
        assert!(huge.len() > HISTORY_PART_MAX_BYTES);
        store
            .record_history_request(&request_record("conv_big", &[("message", huge.as_str())]))
            .expect("record");

        let detail = entry_detail(&store, "conv_big", 1);
        let part = &detail.parts[0];
        assert!(part.truncated, "超限正文必须标记为已截断");
        assert!(part.body.ends_with(HISTORY_TRUNCATION_MARKER));
        assert!(
            part.body.starts_with(&huge[..HISTORY_PART_MAX_BYTES - 1]),
            "截断落在字符边界上，前面的正文原样保留"
        );
        assert_eq!(
            part.body.len(),
            HISTORY_PART_MAX_BYTES - 1 + 1 + HISTORY_TRUNCATION_MARKER.len()
        );
        assert_eq!(part.hash, body_hash(&part.body), "哈希对应的是落库的正文");
        assert_ne!(part.hash, body_hash(&huge));
        assert_eq!(
            detail.entry.detail["bytes"],
            serde_json::json!(part.body.len() as i64)
        );
    }

    /// The record is what recovery reads deliveries, approvals and final replies
    /// from, so nothing ages out of it: a long-lived conversation keeps its first
    /// request and every body it named.
    #[test]
    fn the_history_keeps_every_request_and_every_body() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_cap"))
            .expect("seed");
        let requests: i64 = 3_005;
        for index in 1..=requests {
            let unique = format!("{{\"round\":{index}}}");
            store
                .record_history_request(&request_record(
                    "conv_cap",
                    &[("system", "共享提示"), ("message", unique.as_str())],
                ))
                .expect("record");
        }

        let recorded = trunk_requests(&store, "conv_cap");
        assert_eq!(recorded.len(), requests as usize);
        assert_eq!(recorded.first().expect("oldest").seq, 1);
        assert!(
            store.history_entry("conv_cap", 1).expect("read").is_some(),
            "最早的请求仍可读回"
        );
        assert_eq!(
            row_count(
                &store,
                "SELECT count(*) FROM history_blob WHERE body = '{\"round\":1}'"
            ),
            1,
            "最早那条请求独有的正文仍在"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM history_blob"),
            requests + 2,
            "共享提示与共用的信封各存一份，其余每条请求各一份"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM history_part"),
            requests * 2
        );
    }

    /// Usage belongs to the response, and the list puts it on the request that
    /// response answered — and on no other.
    #[test]
    fn usage_lands_on_the_request_its_response_answered() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_usage"))
            .expect("seed");
        store
            .record_history_request(&request_record("conv_usage", &[("message", "{}")]))
            .expect("first");
        store
            .record_history_request(&request_record("conv_usage", &[("message", "{}")]))
            .expect("second");

        let usage = HistoryUsage {
            input_tokens: Some(1200),
            cached_input_tokens: Some(1000),
            output_tokens: Some(48),
        };
        store
            .record_history_entry(&response_record("conv_usage", None, Some(1), Some(usage), "{}".into()))
            .expect("respond");

        let recorded = trunk_requests(&store, "conv_usage");
        assert_eq!(recorded[0].usage, Some(usage), "用量落在它回答的那次请求上");
        assert_eq!(recorded[1].usage, None, "别的请求不得被顺带写上用量");
        assert_eq!(
            entry_detail(&store, "conv_usage", 1).entry.usage,
            Some(usage),
            "详情读到的用量与列表一致"
        );
    }

    /// A provider that discloses one counter and not the others reads as one
    /// counter and two absences, never as zeros.
    #[test]
    fn a_partly_disclosed_usage_keeps_its_absences() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_partial"))
            .expect("seed");
        store
            .record_history_request(&request_record("conv_partial", &[("message", "{}")]))
            .expect("record");
        store
            .record_history_entry(&response_record(
                "conv_partial",
                None,
                Some(1),
                Some(HistoryUsage {
                    output_tokens: Some(7),
                    ..HistoryUsage::default()
                }),
                "{}".into(),
            ))
            .expect("respond");

        let recorded = trunk_requests(&store, "conv_partial");
        assert_eq!(
            recorded[0].usage,
            Some(HistoryUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: Some(7),
            })
        );
    }

    /// A response whose request never became an entry — a full queue, a failed
    /// write — is still recorded, and its usage lands on no request.
    #[test]
    fn a_response_without_its_request_puts_its_usage_nowhere() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_orphan"))
            .expect("seed");
        store
            .record_history_request(&request_record("conv_orphan", &[("message", "{}")]))
            .expect("record");
        let usage = Some(HistoryUsage {
            input_tokens: Some(9),
            ..HistoryUsage::default()
        });
        store
            .record_history_entry(&response_record("conv_orphan", None, None, usage, "{}".into()))
            .expect("unlinked response");
        store
            .record_history_entry(&response_record("conv_orphan", None, Some(404), usage, "{}".into()))
            .expect("response to a request that is not there");

        assert_eq!(
            trunk_requests(&store, "conv_orphan")[0].usage,
            None,
            "写偏的用量不得落到别的请求上"
        );
    }

    /// The plain case the panel exists to show: between two rounds the person
    /// typed one more message.
    #[test]
    fn a_user_message_appended_since_the_last_request_counts_as_one_addition() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_add"))
            .expect("seed");
        let first = message_part("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = message_part(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = message_part("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        store
            .record_history_request(&request_record_of(
                "conv_add",
                vec![first.clone(), answer.clone()],
            ))
            .expect("first request");
        store
            .record_history_request(&request_record_of("conv_add", vec![first, answer, second]))
            .expect("second request");

        let recorded = trunk_requests(&store, "conv_add");
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(1), Some(0))
        );
    }

    /// Every round appends the model's own turn, and the Anthropic wire format
    /// puts tool results in a `user` message. Neither is the person editing the
    /// history, so a tool loop must read as no change at all.
    #[test]
    fn the_models_own_turn_counts_as_neither_an_addition_nor_a_removal() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_loop"))
            .expect("seed");
        let asked = message_part("user", "user", "{\"role\":\"user\",\"content\":\"跑一下\"}");
        let called = message_part(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":[{\"type\":\"tool-call\"}]}",
        );
        // A `user` role the recorder attributed to the model: the host handing
        // back the result of a call the model made.
        let returned = message_part(
            "user",
            "model",
            "{\"role\":\"user\",\"content\":[{\"type\":\"tool-result\"}]}",
        );
        store
            .record_history_request(&request_record_of("conv_loop", vec![asked.clone()]))
            .expect("first request");
        store
            .record_history_request(&request_record_of("conv_loop", vec![asked, called, returned]))
            .expect("second request");

        let recorded = trunk_requests(&store, "conv_loop");
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn a_message_the_user_deleted_counts_as_one_removal() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_delete"))
            .expect("seed");
        let first = message_part("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = message_part(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = message_part("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        store
            .record_history_request(&request_record_of(
                "conv_delete",
                vec![first.clone(), answer, second.clone()],
            ))
            .expect("first request");
        store
            .record_history_request(&request_record_of("conv_delete", vec![first, second]))
            .expect("second request");

        let recorded = trunk_requests(&store, "conv_delete");
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(0), Some(1))
        );
    }

    /// Editing a message in place is one message that changed, not one thrown
    /// away and one written. The role in the same position is what says so.
    #[test]
    fn a_rewritten_user_message_counts_as_neither() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_edit"))
            .expect("seed");
        let first = message_part("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = message_part(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = message_part("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        let rewritten = message_part("user", "user", "{\"role\":\"user\",\"content\":\"二改\"}");
        store
            .record_history_request(&request_record_of(
                "conv_edit",
                vec![first.clone(), answer.clone(), second],
            ))
            .expect("first request");
        store
            .record_history_request(&request_record_of("conv_edit", vec![first, answer, rewritten]))
            .expect("second request");

        let recorded = trunk_requests(&store, "conv_edit");
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(0), Some(0))
        );
    }

    /// Nothing preceded the first request, so everything the person had written
    /// by then is what they added.
    #[test]
    fn the_first_recorded_request_counts_every_user_message_as_new() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_first"))
            .expect("seed");
        store
            .record_history_request(&request_record_of(
                "conv_first",
                vec![
                    HistoryPartRecord {
                        kind: "system".into(),
                        role: None,
                        author: None,
                        body: "规则".into(),
                    },
                    message_part("user", "user", "{\"role\":\"user\",\"content\":\"一\"}"),
                    message_part(
                        "assistant",
                        "model",
                        "{\"role\":\"assistant\",\"content\":\"答\"}",
                    ),
                    message_part("user", "user", "{\"role\":\"user\",\"content\":\"二\"}"),
                ],
            ))
            .expect("first request");

        let recorded = trunk_requests(&store, "conv_first");
        assert_eq!(
            (recorded[0].messages_added, recorded[0].messages_removed),
            (Some(2), Some(0)),
            "系统提示与模型的回合都不是人写的"
        );
    }

    /// The alignment itself, without a database: consecutive requests share a
    /// long head and tail, and only what is left between them is compared.
    #[test]
    fn message_delta_strips_the_common_head_and_tail_before_pairing_roles() {
        fn before<'a>(hash: &'a str, role: Option<&'a str>) -> DeltaMessage<'a> {
            DeltaMessage {
                hash,
                role,
                author: None,
            }
        }
        fn after<'a>(hash: &'a str, role: Option<&'a str>, author: &'a str) -> DeltaMessage<'a> {
            DeltaMessage {
                hash,
                role,
                author: Some(author),
            }
        }

        let history = [
            before("a", Some("user")),
            before("b", Some("assistant")),
            before("c", Some("user")),
        ];
        assert_eq!(
            message_delta(
                &history,
                &[
                    after("a", Some("user"), "user"),
                    after("b", Some("assistant"), "model"),
                    after("c", Some("user"), "user"),
                ]
            ),
            (0, 0),
            "一模一样的两次请求没有增删"
        );
        assert_eq!(
            message_delta(
                &history,
                &[
                    after("a", Some("user"), "user"),
                    after("b", Some("assistant"), "model"),
                    after("c", Some("user"), "user"),
                    after("d", Some("assistant"), "model"),
                    after("e", Some("user"), "user"),
                ]
            ),
            (1, 0),
            "尾部追加时只有人写的那条算新增"
        );
        assert_eq!(
            message_delta(
                &history,
                &[
                    after("a", Some("user"), "user"),
                    after("c", Some("user"), "user"),
                ]
            ),
            (0, 1),
            "公共后缀先剥掉，删掉的是中间那条"
        );
        assert_eq!(
            message_delta(
                &history,
                &[
                    after("a", Some("user"), "user"),
                    after("b2", Some("assistant"), "model"),
                    after("c", Some("user"), "user"),
                ]
            ),
            (0, 0),
            "同角色同位置改写只算一次改写"
        );
        assert_eq!(
            message_delta(&[before("x", None)], &[after("y", None, "user")]),
            (1, 1),
            "读不出角色的分段不与任何东西配对"
        );
    }

    /// The first change a conversation records is its baseline — unless it only
    /// appended what the user typed, which is a message like any other.
    #[test]
    fn a_first_change_that_is_not_a_sent_message_is_the_baseline() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_baseline");
        source.contexts = vec![user("ctx_a", "问"), assistant("ctx_b", "答", 0)];
        store.put_conversation("ws", &source).expect("seed");
        let changes = store.trunk_changes("conv_baseline").expect("changes");
        assert_eq!(
            (changes[0].kind.as_str(), changes[0].source.as_str()),
            ("edit", "baseline")
        );
    }

    /// A trunk change opens with each row as it read before and after, so a
    /// rewrite can be drawn as a diff and a removal as what went.
    #[test]
    fn a_trunk_change_reads_back_each_row_before_and_after() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_ops");
        source.contexts = vec![user("ctx_a", "原话"), user("ctx_b", "要删的")];
        store.put_conversation("ws", &source).expect("seed");
        source.contexts = vec![user("ctx_a", "改过的")];
        store.put_conversation("ws", &source).expect("edit");

        let changes = store.trunk_changes("conv_ops").expect("changes");
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].source, "edit", "删与改是编辑，不是发送");
        let detail = entry_detail(&store, "conv_ops", changes[1].seq);
        let by_op = |op: &str| {
            detail
                .ops
                .iter()
                .find(|entry| entry.op == op)
                .unwrap_or_else(|| panic!("{op}"))
        };
        let removed = by_op("remove");
        assert_eq!(removed.context_id, "ctx_b");
        assert!(removed.body.is_none());
        assert!(removed.before.as_deref().is_some_and(|body| body.contains("要删的")));
        let replaced = by_op("replace");
        assert!(replaced.before.as_deref().is_some_and(|body| body.contains("原话")));
        assert!(replaced.body.as_deref().is_some_and(|body| body.contains("改过的")));
    }

    #[test]
    fn version_five_upgrades_in_place_and_reads_a_blank_preset() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE_NAME);
        let conn = Connection::open(&path).expect("open v5");
        // The released v5 conversation schema, without the preset column.
        conn.execute_batch(
            "CREATE TABLE conversation (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                order_key REAL NOT NULL,
                settings TEXT NOT NULL,
                worktree TEXT,
                run_target TEXT,
                parent_conversation_id TEXT
             ) STRICT;",
        )
        .expect("v5 conversation schema");
        let (_, remaining_schema) = SCHEMA_SQL
            .split_once(") STRICT;")
            .expect("conversation table terminator");
        conn.execute_batch(remaining_schema)
            .expect("other v5 tables");
        // v5 already shipped all three side tables; the upgrade must not recreate them.
        conn.execute_batch(FORK_START_SCHEMA)
            .expect("v5 fork start table");
        conn.execute_batch(PLAN_SCHEMA).expect("v5 plan table");
        conn.execute_batch(FORK_DECISION_SCHEMA)
            .expect("v5 fork decision table");
        let mut source = conversation("released_v5");
        source.parent_conversation_id = Some("released_parent".into());
        conn.execute(
            "INSERT INTO conversation
             (id, workspace_id, title, created_at, updated_at, order_key, settings, parent_conversation_id)
             VALUES (?1, 'ws', ?2, ?3, ?4, 0, ?5, ?6)",
            rusqlite::params![
                source.id,
                source.title,
                source.created_at,
                source.updated_at,
                serde_json::to_string(&source.settings).expect("settings JSON"),
                source.parent_conversation_id,
            ],
        )
        .expect("v5 row");
        conn.pragma_update(None, "user_version", 5)
            .expect("v5 version");
        drop(conn);

        let store = ConversationStore::open(&path).expect("upgrade");
        let loaded = store
            .conversation(&source.id)
            .expect("read")
            .expect("preserved row");
        assert_eq!(loaded.preset_id, "");
        assert_eq!(loaded, source);
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION);
        assert!(std::fs::read_dir(dir.path())
            .expect("directory")
            .all(|entry| {
                !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("quarantine-")
            }));
    }

    #[test]
    fn deleting_a_parent_reparents_children_to_the_grandparent() {
        let (_dir, store) = temp_store();
        let grandparent = conversation("grandparent");
        let mut parent = conversation("parent");
        parent.parent_conversation_id = Some(grandparent.id.clone());
        let mut child = conversation("child");
        child.parent_conversation_id = Some(parent.id.clone());
        for source in [&grandparent, &parent, &child] {
            store.put_conversation("ws", source).expect("put");
        }
        store.delete_conversation("parent").expect("delete parent");
        assert!(store.conversation("parent").expect("read parent").is_none());
        assert_eq!(
            store
                .conversation("child")
                .expect("read")
                .expect("child")
                .parent_conversation_id,
            Some("grandparent".into())
        );
        store
            .delete_conversation("grandparent")
            .expect("delete root");
        assert_eq!(
            store
                .conversation("child")
                .expect("read")
                .expect("child")
                .parent_conversation_id,
            None
        );
    }

    #[test]
    fn activity_buckets_collapse_messages_to_the_utc_hour() {
        let (_dir, store) = temp_store();
        let mut first = conversation("c1");
        first.created_at = "2026-08-25T03:10:00.000Z".into();
        store.put_conversation("ws", &first).expect("put");
        store
            .upsert_contexts(
                "c1",
                &[
                    user_at("u1", "2026-08-25T03:10:00.000Z"),
                    assistant_at("a1", "2026-08-25T03:59:59.000Z"),
                    user_at("u2", "2026-08-25T04:00:00.000Z"),
                ],
                ContextStatus::Settled,
            )
            .expect("contexts");

        let buckets = store.activity_buckets().expect("buckets");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].user_messages, 1);
        assert_eq!(buckets[0].assistant_messages, 1);
        assert_eq!(buckets[0].sessions, 1);
        assert_eq!(buckets[1].user_messages, 1);
        assert_eq!(buckets[1].sessions, 0);
        // Buckets must be exactly one hour apart to prevent second/millisecond unit errors.
        assert_eq!(
            buckets[1].hour_start_ms - buckets[0].hour_start_ms,
            3_600_000
        );
    }

    #[test]
    fn a_conversation_nobody_ever_spoke_in_is_not_a_session() {
        let (_dir, store) = temp_store();
        let mut empty = conversation("c_empty");
        empty.created_at = "2026-08-25T03:10:00.000Z".into();
        store.put_conversation("ws", &empty).expect("put");
        // A conversation without a user message is not a session.
        store
            .upsert_contexts(
                "c_empty",
                &[assistant_at("a1", "2026-08-25T03:20:00.000Z")],
                ContextStatus::Settled,
            )
            .expect("contexts");

        let buckets = store.activity_buckets().expect("buckets");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].sessions, 0);
        assert_eq!(buckets[0].assistant_messages, 1);
    }

    fn assistant(id: &str, content: &str, round: usize) -> ContextItem {
        ContextItem::Assistant {
            id: id.into(),
            content: content.into(),
            round: Some(round),
            model_turn_id: Some("turn-1".into()),
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-08-25T00:00:02.000Z".into(),
        }
    }

    fn tool(id: &str) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "read".into(),
            round: Some(1),
            model_turn_id: Some("turn-1".into()),
            provider_call_id: None,
            requested_input: None,
            input: serde_json::from_value(serde_json::json!({"path": "a.txt"})).expect("input"),
            result: ToolResult {
                success: true,
                output: "ok".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-08-25T00:00:03.000Z".into(),
                duration_ms: 1,
            },
            subagent: None,
            notice: None,
            attestation: "sig".into(),
            created_at: "2026-08-25T00:00:03.000Z".into(),
        }
    }

    /// An unreadable run target must not silently become local: it could execute a remote-intended
    /// command locally. Bind it to a nonexistent machine so dispatch fails until the user selects a target.
    #[test]
    fn a_corrupt_run_target_does_not_silently_become_local() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_corrupt");
        source.run_target = Some(RunTarget::Ssh {
            machine_id: "m1".into(),
        });
        store.put_conversation("ws", &source).expect("put");

        {
            let conn = store.lock().expect("lock");
            conn.execute(
                "UPDATE conversation SET run_target = ?1 WHERE id = ?2",
                rusqlite::params![r#"{"kind":"ssh"}"#, "conv_corrupt"],
            )
            .expect("corrupt the row");
        }

        let loaded = store
            .conversation("conv_corrupt")
            .expect("read")
            .expect("present");
        match loaded.run_target {
            Some(RunTarget::Ssh { machine_id }) => {
                assert_ne!(machine_id, "m1", "损坏的记录不得复活成原来的机器");
            }
            other => panic!("损坏的运行地点不得变成本机或 WSL：{other:?}"),
        }
    }

    #[test]
    fn round_trips_a_conversation() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_a");
        source.contexts = vec![user("ctx_u", "hi"), assistant("ctx_a", "hello", 1)];
        source.run_target = Some(RunTarget::Wsl {
            distro: "Ubuntu".into(),
        });
        source.additional_directories = vec!["D:/shared/lib".into(), "D:/docs".into()];
        source.queued_messages = vec![QueuedMessage {
            id: "queued_1".into(),
            content: "later".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }];
        source.user_aborted_tasks = vec![UserAbortedTaskRecord {
            id: "task_1".into(),
            source_kind: "shell".into(),
            source_identity: "shell:1".into(),
            label: "l".into(),
            detail: "d".into(),
            metrics: UserAbortedTaskMetrics {
                child_count: None,
                tokens: None,
                tool_count: None,
                elapsed_ms: Some(5),
            },
            started_at: "2026-08-25T00:00:00.000Z".into(),
            ended_at: "2026-08-25T00:00:05.000Z".into(),
            reason: "user".into(),
        }];
        store.put_conversation("ws", &source).expect("put");

        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(loaded, source);
    }

    #[test]
    fn appends_run_output_without_the_renderer() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "partial", 1)],
                ContextStatus::Streaming,
            )
            .expect("streaming");
        store
            .upsert_contexts("conv_a", &[tool("ctx_tool_1")], ContextStatus::Settled)
            .expect("tool");

        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded
                .contexts
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec!["ctx_u", "ctx_a", "ctx_tool_1"]
        );
    }

    fn reasoning(id: &str) -> ContextItem {
        ContextItem::Reasoning {
            id: id.into(),
            content: Some("thinking".into()),
            form: Some(crate::model::ReasoningForm::Plaintext),
            round: Some(1),
            model_turn_id: Some("turn-1".into()),
            interrupted: false,
            duration_ms: None,
            tokens: None,
            replay: None,
            created_at: "2026-08-25T00:00:01.500Z".into(),
        }
    }

    fn context_ids(store: &ConversationStore, conversation_id: &str) -> Vec<String> {
        store
            .conversation(conversation_id)
            .expect("read")
            .expect("present")
            .contexts
            .iter()
            .map(|context| context.id().to_owned())
            .collect()
    }

    fn order_key(store: &ConversationStore, conversation_id: &str, id: &str) -> f64 {
        let conn = store.lock().expect("lock");
        conn.query_row(
            "SELECT order_key FROM context WHERE conversation_id = ?1 AND id = ?2",
            rusqlite::params![conversation_id, id],
            |row| row.get(0),
        )
        .expect("order key")
    }

    /// The run's first round: the journal has appended the streamed prose before the
    /// round's reasoning card exists, and the reasoning card has no persisted predecessor
    /// to sit behind. Writing the prose with its place in the run's order pulls it back
    /// behind the reasoning; the tool card then follows the prose.
    #[test]
    fn a_sequence_moves_streamed_prose_behind_reasoning_minted_after_it() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "let me read", 1)],
                ContextStatus::Streaming,
            )
            .expect("streamed prose");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[reasoning("ctx_r")],
                ContextStatus::Settled,
                &["ctx_r"],
            )
            .expect("reasoning");
        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_a", "ctx_r"]
        );
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[assistant("ctx_a", "let me read the file", 1)],
                ContextStatus::Settled,
                &["ctx_r", "ctx_a"],
            )
            .expect("settled prose");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[tool("ctx_tool_1")],
                ContextStatus::Settled,
                &["ctx_a", "ctx_tool_1"],
            )
            .expect("tool");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_r", "ctx_a", "ctx_tool_1"]
        );
        assert_eq!(store.reconcile_streaming().expect("reconcile"), 0);
        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert!(matches!(
            &loaded.contexts[2],
            ContextItem::Assistant { content, interrupted: false, .. } if content == "let me read the file"
        ));
    }

    /// A later round: the previous round's last card is a persisted predecessor, so a new
    /// reasoning card is filed directly behind it — ahead of the prose the journal already
    /// appended — and the prose keeps its key.
    #[test]
    fn a_sequence_files_a_new_row_behind_its_predecessor_without_moving_the_rest() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts(
                "conv_a",
                &[user("ctx_u", "hi"), tool("ctx_tool_1")],
                ContextStatus::Settled,
            )
            .expect("previous round");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "done", 2)],
                ContextStatus::Streaming,
            )
            .expect("streamed prose");
        let prose_key = order_key(&store, "conv_a", "ctx_a");

        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[reasoning("ctx_r")],
                ContextStatus::Settled,
                &["ctx_tool_1", "ctx_r"],
            )
            .expect("reasoning");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[assistant("ctx_a", "done.", 2)],
                ContextStatus::Settled,
                &["ctx_r", "ctx_a"],
            )
            .expect("settled prose");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_tool_1", "ctx_r", "ctx_a"]
        );
        let tool_key = order_key(&store, "conv_a", "ctx_tool_1");
        let reasoning_key = order_key(&store, "conv_a", "ctx_r");
        assert!(tool_key < reasoning_key && reasoning_key < prose_key);
        assert_eq!(order_key(&store, "conv_a", "ctx_a"), prose_key);
    }

    /// The journal merges every reasoning segment into the round's first row; settlement
    /// splits later segments into their own cards, which must land between the first
    /// segment and the prose, not after the prose.
    #[test]
    fn later_reasoning_segments_land_between_the_first_segment_and_the_prose() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[reasoning("ctx_r"), assistant("ctx_a", "so", 1)],
                ContextStatus::Streaming,
            )
            .expect("streamed rows");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[
                    reasoning("ctx_r"),
                    reasoning("ctx_r_1"),
                    reasoning("ctx_r_2"),
                ],
                ContextStatus::Settled,
                &["ctx_r", "ctx_r_1", "ctx_r_2"],
            )
            .expect("segments");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[assistant("ctx_a", "so it is", 1)],
                ContextStatus::Settled,
                &["ctx_r_2", "ctx_a"],
            )
            .expect("prose");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_r", "ctx_r_1", "ctx_r_2", "ctx_a"]
        );
    }

    /// Two neighbours whose keys have no representable midpoint left: the rows from the
    /// following one onwards are reindexed to reopen the gap, and the order survives.
    #[test]
    fn an_exhausted_gap_is_reopened_by_reindexing_the_rows_behind_it() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts(
                "conv_a",
                &[
                    user("ctx_u", "hi"),
                    tool("ctx_tool_1"),
                    assistant("ctx_a", "done", 2),
                    user("ctx_u2", "next"),
                ],
                ContextStatus::Settled,
            )
            .expect("rows");
        let tool_key = order_key(&store, "conv_a", "ctx_tool_1");
        {
            let conn = store.lock().expect("lock");
            conn.execute(
                "UPDATE context SET order_key = ?1 WHERE conversation_id = 'conv_a' AND id = 'ctx_a'",
                [tool_key + f64::EPSILON * tool_key.max(1.0)],
            )
            .expect("close the gap");
        }
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[reasoning("ctx_r")],
                ContextStatus::Settled,
                &["ctx_tool_1", "ctx_r"],
            )
            .expect("reasoning");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_tool_1", "ctx_r", "ctx_a", "ctx_u2"]
        );
        let keys = ["ctx_u", "ctx_tool_1", "ctx_r", "ctx_a", "ctx_u2"]
            .map(|id| order_key(&store, "conv_a", id));
        assert!(
            keys.windows(2).all(|pair| pair[0] < pair[1]),
            "keys must stay strictly increasing: {keys:?}"
        );
    }

    /// Reopening a gap must not translate the rows behind it by a step: keys that dense
    /// round together after the addition, and `rowid` then decides between two rows
    /// whose insertion order is the reverse of their timeline order. Here `between`
    /// (1 + 3·2⁻⁵²) was inserted after `n50` (1 + 2⁻⁵⁰) but sorts before it; both would
    /// land on 2 + 2⁻⁵⁰ if shifted by one.
    #[test]
    fn reindexing_an_exhausted_gap_keeps_dense_neighbours_in_order() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts(
                "conv_a",
                &[
                    user("ctx_floor", "hi"),
                    user("ctx_n50", "a"),
                    user("ctx_between", "b"),
                    user("ctx_n52", "c"),
                ],
                ContextStatus::Settled,
            )
            .expect("rows");
        {
            let conn = store.lock().expect("lock");
            for (id, key) in [
                ("ctx_floor", 1.0),
                ("ctx_n52", 1.0 + 2f64.powi(-52)),
                ("ctx_between", 1.0 + 3.0 * 2f64.powi(-52)),
                ("ctx_n50", 1.0 + 2f64.powi(-50)),
            ] {
                conn.execute(
                    "UPDATE context SET order_key = ?1 WHERE conversation_id = 'conv_a' AND id = ?2",
                    rusqlite::params![key, id],
                )
                .expect("place the row");
            }
        }
        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_floor", "ctx_n52", "ctx_between", "ctx_n50"]
        );

        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[reasoning("ctx_r")],
                ContextStatus::Settled,
                &["ctx_floor", "ctx_r"],
            )
            .expect("reasoning");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_floor", "ctx_r", "ctx_n52", "ctx_between", "ctx_n50"]
        );
        let keys = ["ctx_floor", "ctx_r", "ctx_n52", "ctx_between", "ctx_n50"]
            .map(|id| order_key(&store, "conv_a", id));
        assert!(
            keys.windows(2).all(|pair| pair[0] < pair[1]),
            "keys must stay strictly increasing: {keys:?}"
        );
    }

    /// Sequence ids without a row are skipped, repeated ids count once, and items the
    /// sequence does not name are appended like an ordinary upsert.
    #[test]
    fn a_sequence_tolerates_unpersisted_ids_repeats_and_unnamed_items() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "done", 1)],
                ContextStatus::Streaming,
            )
            .expect("streamed prose");
        store
            .upsert_contexts_in_sequence(
                "conv_a",
                &[reasoning("ctx_r"), tool("ctx_tool_1")],
                ContextStatus::Settled,
                &["ctx_never_written", "ctx_u", "ctx_u", "ctx_r"],
            )
            .expect("write");

        assert_eq!(
            context_ids(&store, "conv_a"),
            vec!["ctx_u", "ctx_r", "ctx_a", "ctx_tool_1"]
        );
    }

    #[test]
    fn streaming_prose_is_finalized_in_place() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "par", 1)],
                ContextStatus::Streaming,
            )
            .expect("partial");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "partial answer", 1)],
                ContextStatus::Streaming,
            )
            .expect("grown");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "partial answer done", 1)],
                ContextStatus::Settled,
            )
            .expect("settled");

        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(loaded.contexts.len(), 1);
        match &loaded.contexts[0] {
            ContextItem::Assistant {
                content,
                interrupted,
                ..
            } => {
                assert_eq!(content, "partial answer done");
                assert!(!interrupted);
            }
            other => panic!("unexpected context {other:?}"),
        }
        assert_eq!(store.reconcile_streaming().expect("reconcile"), 0);
    }

    #[test]
    fn boot_reconcile_marks_unfinished_prose_interrupted() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "half written", 1)],
                ContextStatus::Streaming,
            )
            .expect("streaming");

        assert_eq!(store.reconcile_streaming().expect("reconcile"), 1);
        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        match &loaded.contexts[1] {
            ContextItem::Assistant {
                content,
                interrupted,
                ..
            } => {
                assert_eq!(content, "half written");
                assert!(interrupted, "留在盘上的半截正文必须被标成中断片段");
            }
            other => panic!("unexpected context {other:?}"),
        }
        // Idempotent: the second startup finds no new streaming rows.
        assert_eq!(store.reconcile_streaming().expect("reconcile"), 0);
    }

    /// Discarding is for prose no canonical card claimed when its round
    /// settled — a failed attempt's reasoning that the retried attempt never
    /// repeated. It only ever removes rows still in `streaming`: settlement
    /// claims a row by id, and the same id must survive once it has.
    #[test]
    fn discarding_removes_streaming_rows_and_spares_settled_ones() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_a"))
            .expect("put");
        store
            .upsert_contexts("conv_a", &[user("ctx_u", "hi")], ContextStatus::Settled)
            .expect("user");
        store
            .upsert_contexts(
                "conv_a",
                &[reasoning("ctx_r"), assistant("ctx_a", "half written", 1)],
                ContextStatus::Streaming,
            )
            .expect("streaming");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_done", "settled", 1)],
                ContextStatus::Settled,
            )
            .expect("settled");

        assert_eq!(
            store
                .discard_streaming_contexts(
                    "conv_a",
                    &["ctx_r", "ctx_a", "ctx_done", "ctx_u", "ctx_never_written"],
                )
                .expect("discard"),
            2
        );
        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded
                .contexts
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec!["ctx_u", "ctx_done"]
        );
        // Nothing is left for reconciliation to mark, and the discarded ids can
        // be written again by a later attempt.
        assert_eq!(store.reconcile_streaming().expect("reconcile"), 0);
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_a", "written again", 1)],
                ContextStatus::Streaming,
            )
            .expect("rewritten");
        assert_eq!(
            store
                .conversation("conv_a")
                .expect("read")
                .expect("present")
                .contexts
                .len(),
            3
        );
        assert_eq!(
            store
                .discard_streaming_contexts("conv_a", &[])
                .expect("empty"),
            0
        );
    }

    /// The metadata write is what a renderer edit gets while a run owns the
    /// timeline. It carries the conversation's own row, queue and aborted-task
    /// records, and leaves every context row — including one persisted after
    /// the snapshot the edit was built from, and one still streaming — exactly
    /// where the run put it.
    #[test]
    fn metadata_write_leaves_the_timeline_rows_untouched() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_a");
        source.contexts = vec![user("ctx_u", "hi")];
        source.branches = vec![ConversationBranch {
            id: "branch_1".into(),
            fork_context_id: "ctx_u".into(),
            active: false,
            contexts: vec![assistant("ctx_branch", "other suffix", 1)],
            created_at: "2026-08-25T00:00:06.000Z".into(),
            updated_at: "2026-08-25T00:00:06.000Z".into(),
        }];
        store.put_conversation("ws", &source).expect("put");
        // The renderer builds its edit from this snapshot...
        let snapshot = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        // ...while the run persists a settled card and a streaming row after it.
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_after_snapshot", "done", 1)],
                ContextStatus::Settled,
            )
            .expect("settled after snapshot");
        store
            .upsert_contexts(
                "conv_a",
                &[assistant("ctx_live", "half written", 2)],
                ContextStatus::Streaming,
            )
            .expect("streaming after snapshot");

        let mut edit = snapshot.clone();
        edit.title = "renamed while running".into();
        edit.settings.include_app_data_path = true;
        edit.queued_messages = vec![QueuedMessage {
            id: "queued_1".into(),
            content: "later".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }];
        // The edit also proposes a different timeline; that part must not land.
        edit.contexts = vec![user("ctx_u", "edited")];
        edit.branches.clear();
        store
            .put_conversation_metadata("ws", &edit)
            .expect("metadata write");

        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(loaded.title, "renamed while running");
        assert!(loaded.settings.include_app_data_path);
        assert_eq!(loaded.queued_messages, edit.queued_messages);
        assert_eq!(
            loaded
                .contexts
                .iter()
                .map(ContextItem::id)
                .collect::<Vec<_>>(),
            vec!["ctx_u", "ctx_after_snapshot", "ctx_live"]
        );
        assert!(matches!(
            &loaded.contexts[0],
            ContextItem::User { content, .. } if content == "hi"
        ));
        assert_eq!(loaded.branches, source.branches);
        // The streaming row is still streaming: settlement will replace it in
        // place, and a crash will still find it to mark as interrupted.
        assert_eq!(store.reconcile_streaming().expect("reconcile"), 1);
    }

    /// The metadata write also creates the row a brand-new conversation needs,
    /// so the sidebar position logic is shared with the full write.
    #[test]
    fn metadata_write_creates_a_missing_conversation_row() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_first"))
            .expect("put");
        store
            .put_conversation_metadata("ws", &conversation("conv_second"))
            .expect("metadata write");
        let ids = store
            .workspace_conversations("ws")
            .expect("list")
            .into_iter()
            .map(|conversation| conversation.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["conv_first", "conv_second"]);
    }

    #[test]
    fn writes_for_an_unknown_conversation_are_ignored() {
        let (_dir, store) = temp_store();
        store
            .upsert_contexts(
                "conv_missing",
                &[user("ctx_u", "hi")],
                ContextStatus::Settled,
            )
            .expect("no-op");
        assert!(store.conversation("conv_missing").expect("read").is_none());
    }

    #[test]
    fn deleting_a_conversation_removes_every_dependent_row() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_a");
        source.contexts = vec![user("ctx_u", "hi")];
        source.queued_messages = vec![QueuedMessage {
            id: "queued_1".into(),
            content: "later".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }];
        store.put_conversation("ws", &source).expect("put");
        store.delete_conversation("conv_a").expect("delete");

        assert!(store.conversation("conv_a").expect("read").is_none());
        let conn = store.lock().expect("lock");
        let contexts: i64 = conn
            .query_row("SELECT count(*) FROM context", [], |row| row.get(0))
            .expect("count");
        let queued: i64 = conn
            .query_row("SELECT count(*) FROM queued_message", [], |row| row.get(0))
            .expect("count");
        assert_eq!((contexts, queued), (0, 0));
    }

    #[test]
    fn title_settled_survives_upserts_and_explanations_die_with_the_conversation() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_t");
        source.contexts = vec![user("ctx_u", "hi")];
        store.put_conversation("ws", &source).expect("put");
        assert!(!store.title_settled("conv_t").expect("read"));
        store.set_title_settled("conv_t", true).expect("settle");
        // A renderer commit rewrites the row but never the flag.
        source.title = "renamed".into();
        store.put_conversation_metadata("ws", &source).expect("metadata");
        store.put_conversation("ws", &source).expect("put again");
        assert!(store.title_settled("conv_t").expect("read"));
        assert!(!store.title_settled("missing").expect("read missing"));

        store.put_tool_explanation("conv_t", "tool_1", "列出文件").expect("explain");
        store.put_tool_explanation("conv_t", "tool_1", "列出目录").expect("replace");
        let explanations = store.tool_explanations("conv_t").expect("explanations");
        assert_eq!(explanations.get("tool_1").map(String::as_str), Some("列出目录"));
        // A failed command keeps its description and gains a reason beside it.
        store.put_tool_error_explanation("conv_t", "tool_1", "没有安装 pnpm").expect("explain error");
        assert_eq!(store.tool_explanations("conv_t").expect("explanations").get("tool_1").map(String::as_str), Some("列出目录"));
        let errors = store.tool_error_explanations("conv_t").expect("errors");
        assert_eq!(errors.get("tool_1").map(String::as_str), Some("没有安装 pnpm"));
        store.delete_conversation("conv_t").expect("delete");
        assert!(store.tool_explanations("conv_t").expect("after delete").is_empty());
        assert!(store.tool_error_explanations("conv_t").expect("after delete").is_empty());
    }

    #[test]
    fn adding_title_settled_settles_conversations_that_already_have_replies() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE_NAME);
        {
            let store = ConversationStore::open(&path).expect("open");
            let mut answered = conversation("answered");
            answered.contexts = vec![user("u1", "hi"), assistant("a1", "hello", 1)];
            store.put_conversation("ws", &answered).expect("put");
            let mut fresh = conversation("fresh");
            fresh.contexts = vec![user("u2", "hi")];
            store.put_conversation("ws", &fresh).expect("put");
            // Make it look like a store from before the column existed.
            let conn = store.lock().expect("lock");
            conn.execute_batch("ALTER TABLE conversation DROP COLUMN title_settled; PRAGMA user_version = 15;")
                .expect("drop column");
        }
        let store = ConversationStore::open(&path).expect("reopen");
        assert!(store.title_settled("answered").expect("answered"));
        assert!(!store.title_settled("fresh").expect("fresh"));
    }

    #[test]
    fn workspace_moves_detach_only_final_cross_workspace_parent_edges() {
        for (destination, moving, expected) in [
            ("b", vec!["p"], [None, None, Some("c")]),
            ("b", vec!["c"], [None, None, None]),
            ("b", vec!["p", "c"], [None, Some("p"), None]),
            ("a", vec!["g", "c", "p"], [None, Some("p"), Some("c")]),
        ] {
            let (_dir, store) = temp_store();
            for (id, parent) in [("p", None), ("c", Some("p")), ("g", Some("c"))] {
                let mut item = conversation(id);
                item.parent_conversation_id = parent.map(str::to_owned);
                store.put_conversation("a", &item).unwrap();
            }
            store
                .set_workspace_order(
                    destination,
                    &moving.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                )
                .unwrap();
            for (index, id) in ["p", "c", "g"].iter().enumerate() {
                let item = store.conversation(id).unwrap().unwrap();
                assert_eq!(
                    item.parent_conversation_id.as_deref(),
                    expected[index],
                    "{destination}: {moving:?}, {id}"
                );
            }
            let remaining = store.workspace_conversations("a").unwrap();
            assert_eq!(
                remaining.len(),
                if destination == "a" {
                    3
                } else {
                    3 - moving.len()
                }
            );
        }
    }

    #[test]
    fn workspace_order_follows_the_recorded_sequence() {
        let (_dir, store) = temp_store();
        for id in ["conv_a", "conv_b", "conv_c"] {
            store
                .put_conversation("ws", &conversation(id))
                .expect("put");
        }
        store
            .set_workspace_order("ws", &["conv_c".into(), "conv_a".into(), "conv_b".into()])
            .expect("reorder");
        let ids = store
            .workspace_conversations("ws")
            .expect("list")
            .into_iter()
            .map(|conversation| conversation.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["conv_c", "conv_a", "conv_b"]);
    }

    #[test]
    fn branch_contexts_stay_out_of_the_trunk() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_a");
        source.contexts = vec![user("ctx_u", "hi")];
        source.branches = vec![ConversationBranch {
            id: "branch_1".into(),
            fork_context_id: "ctx_u".into(),
            active: false,
            contexts: vec![assistant("ctx_branch", "other suffix", 1)],
            created_at: "2026-08-25T00:00:06.000Z".into(),
            updated_at: "2026-08-25T00:00:06.000Z".into(),
        }];
        store.put_conversation("ws", &source).expect("put");

        let loaded = store
            .conversation("conv_a")
            .expect("read")
            .expect("present");
        assert_eq!(loaded.contexts.len(), 1);
        assert_eq!(loaded.branches[0].contexts.len(), 1);
        assert_eq!(loaded, source);
    }
}
