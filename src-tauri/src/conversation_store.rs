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
    sync::{Arc, Mutex, OnceLock},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::model::{
    ContextItem, Conversation, ConversationBranch, ConversationSettings, ConversationWorktree,
    FileAttachment, ImageAttachment, QueuedMessage, RunTarget, UserAbortedTaskRecord,
};
use crate::model::AttachedWorkspace;

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
/// more than the schema delivers. Trusting it is what let a `wire_request` without
/// `owner` sit behind a current stamp and fail every ledger read and write for the
/// remaining life of the store.
pub const STORE_VERSION: i32 = 15;

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

/// Append-only history of the trunk timeline, kept as a delta chain rather than a
/// copy of the prefix per event: a snapshot per backend request would grow with the
/// square of the conversation, while the changes themselves grow with it linearly.
///
/// `timeline_head` materializes the trunk as of the newest event so a recording only
/// has to diff against one table instead of replaying the whole chain. It holds each
/// row's body verbatim instead of a hash: a digest would have to stay stable across
/// toolchain versions to avoid inventing a replacement for every row at once.
const TIMELINE_HISTORY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS timeline_event (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('baseline', 'run', 'edit')),
    -- The backend request this settled, for 'run' events only.
    request_id      TEXT,
    inserted        INTEGER NOT NULL,
    removed         INTEGER NOT NULL,
    replaced        INTEGER NOT NULL,
    -- Trunk length once this event applied, so the list reads without a replay.
    row_count       INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (conversation_id, seq)
) STRICT;

CREATE TABLE IF NOT EXISTS timeline_op (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    ordinal         INTEGER NOT NULL,
    op              TEXT NOT NULL CHECK (op IN ('remove', 'insert', 'replace')),
    context_id      TEXT NOT NULL,
    -- Final index of an 'insert'; NULL otherwise.
    position        INTEGER,
    -- Row body for 'insert' and 'replace'; NULL for 'remove'.
    data            TEXT,
    PRIMARY KEY (conversation_id, seq, ordinal)
) STRICT;

CREATE TABLE IF NOT EXISTS timeline_head (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    context_id      TEXT NOT NULL,
    data            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, position)
) STRICT;";

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

/// Forensic ledger of what actually left this process, one `wire_request` row per
/// request that went on the wire, with the request's own parts named in wire order.
///
/// Bodies are content-addressed instead of being stored beside the request that
/// sent them because every round of a turn re-sends the entire history: keeping
/// each request's message list verbatim would grow with the square of the
/// conversation, while hashing a body once and naming it from each request that
/// carried it grows with it linearly. A message that survives twenty rounds costs
/// one `wire_blob` row and twenty names.
///
/// The address is a hash within one conversation rather than a global one: the
/// ledger is capped and pruned per conversation, and a body shared across
/// conversations would have to be reference counted across all of them before
/// anything could be dropped. `truncated` lives with the body and not with the
/// part because it describes what was stored, which is what the hash covers.
///
/// A conversation holds one ledger per agent plus the trunk's, separated by
/// `owner` — the child's name — and sharing one `seq` space, because children
/// run under their parent's conversation id and nothing else would keep their
/// traffic apart. Bodies are shared across all of them, which is what a child's
/// replayed system prompt and tool surface cost: one row, however many agents
/// carried them.
const WIRE_LEDGER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wire_request (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('model', 'search', 'fetch')),
    -- The run this request belonged to; empty for host-minted one-shots.
    request_id      TEXT NOT NULL,
    round           INTEGER NOT NULL,
    attempt         INTEGER NOT NULL,
    provider_name   TEXT NOT NULL,
    family          TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    -- Redacted StepRequest with the large, separately-stored fields removed.
    envelope        TEXT NOT NULL,
    part_count      INTEGER NOT NULL,
    bytes           INTEGER NOT NULL,
    -- Provider-reported usage for the response this request got back. NULL
    -- until the response lands, and NULL forever on a request the transport
    -- lost: a request that got no answer has no usage, and a zero would be a
    -- number the record cannot support.
    input_tokens         INTEGER,
    cached_input_tokens  INTEGER,
    output_tokens        INTEGER,
    -- Messages the user added and removed between the request before this one
    -- and this one, counted against the predecessor's own parts when the row
    -- was written. NULL on rows written before the ledger counted them.
    messages_added       INTEGER,
    messages_removed     INTEGER,
    -- Name of the child agent that issued this request, and NULL for the
    -- conversation's own trunk. A child shares its parent's conversation id, so
    -- without this column a subagent's traffic would be filed as the session's
    -- own; with it, one conversation holds one ledger per agent plus the
    -- trunk's, each read on its own. The name and not the call id: a name is
    -- reserved for the whole conversation and is what every later message
    -- addresses the child by, while the provider's call id never reaches the
    -- timeline the renderer matches against.
    owner                TEXT,
    PRIMARY KEY (conversation_id, seq)
) STRICT;

CREATE TABLE IF NOT EXISTS wire_blob (
    conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    hash            TEXT NOT NULL,
    body            TEXT NOT NULL,
    truncated       INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, hash)
) STRICT;

CREATE TABLE IF NOT EXISTS wire_request_part (
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
) STRICT;";

/// Largest body the ledger stores. One message can carry an entire file, and the
/// ledger is an audit trail rather than a second copy of the workspace: past this
/// the marker stands in for what was sent.
const WIRE_BLOB_MAX_BYTES: usize = 256 * 1024;

/// Appended to a body in place of the bytes the cap dropped. It is part of the
/// stored body, so the hash covers it too: what reads back is what was hashed.
const WIRE_BLOB_TRUNCATION_MARKER: &str = "…（请求账本正文已截断）";

/// How many requests one ledger keeps. The ledger answers what the recent turns
/// put on the wire, so it is bounded and pruned on write rather than growing for
/// the lifetime of the conversation.
///
/// The cap is per ledger — the trunk's and each agent's separately — because a
/// shared one would let a chatty child evict the session's own history, and the
/// trunk ledger is the one a reader opens by default.
const WIRE_LEDGER_MAX_REQUESTS: i64 = 300;

/// Ceiling across every ledger of one conversation. The per-ledger cap bounds
/// each agent, but a conversation may spawn agents without limit; this bounds
/// their sum so a long session cannot grow the store without end.
const WIRE_LEDGER_MAX_ROWS: i64 = 3_000;

/// Provider-reported usage for one recorded request. Every field is optional:
/// providers disclose different subsets, and an absent counter must read as
/// absent rather than as zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
}

impl WireUsage {
    /// True when the provider disclosed nothing at all, which is the case the
    /// recorder drops instead of writing a row of NULLs over a row that may
    /// already hold numbers.
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.cached_input_tokens.is_none()
            && self.output_tokens.is_none()
    }
}

/// One recorded request without its bodies: what the ledger list draws a row from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRequestSummary {
    pub seq: i64,
    pub created_at: String,
    pub kind: String,
    pub request_id: String,
    pub round: i64,
    pub attempt: i64,
    pub provider_name: String,
    pub family: String,
    pub model_id: String,
    pub part_count: i64,
    pub bytes: i64,
    /// Absent while the response has not landed, and absent forever on a
    /// request that never got one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<WireUsage>,
    /// Messages the user wrote between the previous request and this one.
    /// Absent on rows written before the ledger counted them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_added: Option<i64>,
    /// Messages that vanished from the history over the same interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_removed: Option<i64>,
    /// Name of the child agent that issued this request, absent on the
    /// conversation's own trunk. What the renderer matches an agent's ledger by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// One part of a recorded request, resolved through its hash to the stored body.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRequestPart {
    pub ordinal: i64,
    pub kind: String,
    pub hash: String,
    /// JSON text: one ModelMessage, a prompt string, or the tool-spec array.
    pub body: String,
    /// UTF-8 length of `body`, so the parts of a request sum to its own `bytes`.
    pub bytes: i64,
    pub truncated: bool,
}

/// One recorded request in full, as the detail view reads it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRequestDetail {
    pub summary: WireRequestSummary,
    /// Redacted StepRequest without messages/system/systemDynamic/tools.
    pub envelope: serde_json::Value,
    pub parts: Vec<WireRequestPart>,
}

/// One part as the recorder hands it over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireRecordedPart {
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
pub struct WireRequestRecord {
    pub conversation_id: String,
    /// The child agent this request ran as, or `None` for the main session. A
    /// child shares the parent's conversation id, so this is the only thing
    /// that keeps its traffic out of the session's own ledger.
    pub owner: Option<String>,
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
    pub parts: Vec<WireRecordedPart>,
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

/// One recorded moment in a conversation's trunk history, as the tests read it back.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEvent {
    pub seq: i64,
    /// `baseline` for the entry a conversation's history starts from, `run` for a
    /// settled backend request, `edit` for a change the renderer committed.
    pub kind: String,
    pub request_id: Option<String>,
    pub inserted: i64,
    pub removed: i64,
    pub replaced: i64,
    pub row_count: i64,
    pub created_at: String,
}

/// Why a history entry is being recorded. The first entry of a conversation is
/// always stored as `baseline` whatever the caller's reason: replay starts from an
/// empty trunk, so the rows that already existed have to enter the chain somewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineEventKind {
    Run,
    Edit,
}

impl TimelineEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Edit => "edit",
        }
    }
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
    attached_workspaces TEXT
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
    TIMELINE_HISTORY_SCHEMA,
    TEMPLATE_SCHEMA,
    WIRE_LEDGER_SCHEMA,
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
    ("wire_request", "input_tokens", "INTEGER"),
    ("wire_request", "cached_input_tokens", "INTEGER"),
    ("wire_request", "output_tokens", "INTEGER"),
    ("wire_request", "messages_added", "INTEGER"),
    ("wire_request", "messages_removed", "INTEGER"),
    ("wire_request", "owner", "TEXT"),
    ("wire_request_part", "role", "TEXT"),
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
}

pub struct ConversationStore {
    conn: Mutex<Connection>,
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
        };
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
        for &(table, column, declaration) in ADDED_COLUMNS {
            if has_column(&tx, table, column)? {
                continue;
            }
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
            ))
            .map_err(|error| format!("无法升级对话库结构：{error}"))?;
        }
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

    /// Lists every conversation in a workspace in sidebar order.
    pub fn workspace_conversations(&self, workspace_id: &str) -> Result<Vec<Conversation>, String> {
        let ids = {
            let conn = self.lock()?;
            let mut statement = conn
                .prepare(
                    "SELECT id FROM conversation WHERE workspace_id = ?1 ORDER BY order_key, id",
                )
                .map_err(|error| format!("无法查询对话列表：{error}"))?;
            let rows = statement
                .query_map([workspace_id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法查询对话列表：{error}"))?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(|error| format!("无法读取对话行：{error}"))?);
            }
            ids
        };
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

    /// Assembles a complete conversation body, returning `None` when absent.
    pub fn conversation(&self, conversation_id: &str) -> Result<Option<Conversation>, String> {
        let conn = self.lock()?;
        let shell = conn
            .query_row(
                "SELECT title, created_at, updated_at, settings, worktree, run_target, parent_conversation_id, preset_id, template_id, additional_directories, attached_workspaces FROM conversation WHERE id = ?1",
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
        )) = shell
        else {
            return Ok(None);
        };
        let settings: ConversationSettings = serde_json::from_str(&settings_json)
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

        let contexts = read_contexts(&conn, conversation_id, None)?;

        let mut branches = Vec::new();
        {
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

    /// Every recorded change to the trunk timeline, oldest first. Test-only: it
    /// reads back what the recorder wrote.
    #[cfg(test)]
    pub fn timeline_events(&self, conversation_id: &str) -> Result<Vec<TimelineEvent>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT seq, kind, request_id, inserted, removed, replaced, row_count, created_at
                 FROM timeline_event WHERE conversation_id = ?1 ORDER BY seq",
            )
            .map_err(|error| format!("无法查询时间线历史：{error}"))?;
        let rows = statement
            .query_map([conversation_id], |row| {
                Ok(TimelineEvent {
                    seq: row.get(0)?,
                    kind: row.get(1)?,
                    request_id: row.get(2)?,
                    inserted: row.get(3)?,
                    removed: row.get(4)?,
                    replaced: row.get(5)?,
                    row_count: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .map_err(|error| format!("无法查询时间线历史：{error}"))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(|error| format!("无法读取时间线历史行：{error}"))?);
        }
        Ok(events)
    }

    /// The trunk as it stood once `seq` applied, rebuilt by folding the chain from
    /// its baseline. A row whose body no longer parses is dropped rather than
    /// failing the whole snapshot: one unreadable card must not hide the history
    /// around it. Test-only: it reads back what the recorder wrote.
    #[cfg(test)]
    pub fn timeline_snapshot(
        &self,
        conversation_id: &str,
        seq: i64,
    ) -> Result<Vec<ContextItem>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT op, context_id, position, data FROM timeline_op
                 WHERE conversation_id = ?1 AND seq <= ?2 ORDER BY seq, ordinal",
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

    /// Appends the trunk's current shape to its history. A recording that finds
    /// nothing changed writes no event: a run that produced no lasting row and a
    /// debounced metadata commit both reach here, and neither is a moment in the
    /// timeline's history.
    pub fn record_timeline_event(
        &self,
        conversation_id: &str,
        kind: TimelineEventKind,
        request_id: Option<&str>,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| record_timeline_event_tx(tx, conversation_id, kind, request_id))
    }

    /// Every recorded request of one ledger, oldest first. The list is bounded by
    /// retention, so it needs no limit of its own.
    ///
    /// `owners` selects which ledger: `None` is the conversation's own trunk, and
    /// a list of child-agent names is those agents'. Defaulting to the trunk
    /// rather than to everything is deliberate: a pane that asked for the
    /// session's requests must not silently start reporting its children's as
    /// the session's own.
    pub fn wire_requests(
        &self,
        conversation_id: &str,
        owners: Option<&[String]>,
    ) -> Result<Vec<WireRequestSummary>, String> {
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
                "SELECT seq, created_at, kind, request_id, round, attempt,
                        provider_name, family, model_id, part_count, bytes,
                        input_tokens, cached_input_tokens, output_tokens,
                        messages_added, messages_removed, owner
                 FROM wire_request WHERE conversation_id = ?1 AND {filter} ORDER BY seq"
            ))
            .map_err(|error| format!("无法查询请求账本：{error}"))?;
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&conversation_id];
        for owner in owners.unwrap_or_default() {
            bound.push(owner);
        }
        let rows = statement
            .query_map(bound.as_slice(), |row| {
                Ok(WireRequestSummary {
                    seq: row.get(0)?,
                    created_at: row.get(1)?,
                    kind: row.get(2)?,
                    request_id: row.get(3)?,
                    round: row.get(4)?,
                    attempt: row.get(5)?,
                    provider_name: row.get(6)?,
                    family: row.get(7)?,
                    model_id: row.get(8)?,
                    part_count: row.get(9)?,
                    bytes: row.get(10)?,
                    usage: read_wire_usage(row, 11)?,
                    messages_added: row.get(14)?,
                    messages_removed: row.get(15)?,
                    owner: row.get(16)?,
                })
            })
            .map_err(|error| format!("无法查询请求账本：{error}"))?;
        let mut requests = Vec::new();
        for row in rows {
            requests.push(row.map_err(|error| format!("无法读取请求账本行：{error}"))?);
        }
        Ok(requests)
    }

    /// One recorded request with its envelope and ordered parts. An envelope that
    /// no longer parses reads as `null` instead of failing the whole request: the
    /// parts beside it are still evidence, and one corrupt row must not hide them.
    pub fn wire_request(
        &self,
        conversation_id: &str,
        seq: i64,
    ) -> Result<Option<WireRequestDetail>, String> {
        let conn = self.lock()?;
        let found = conn
            .query_row(
                "SELECT seq, created_at, kind, request_id, round, attempt,
                        provider_name, family, model_id, part_count, bytes, envelope,
                        input_tokens, cached_input_tokens, output_tokens,
                        messages_added, messages_removed, owner
                 FROM wire_request WHERE conversation_id = ?1 AND seq = ?2",
                rusqlite::params![conversation_id, seq],
                |row| {
                    Ok((
                        WireRequestSummary {
                            seq: row.get(0)?,
                            created_at: row.get(1)?,
                            kind: row.get(2)?,
                            request_id: row.get(3)?,
                            round: row.get(4)?,
                            attempt: row.get(5)?,
                            provider_name: row.get(6)?,
                            family: row.get(7)?,
                            model_id: row.get(8)?,
                            part_count: row.get(9)?,
                            bytes: row.get(10)?,
                            usage: read_wire_usage(row, 12)?,
                            messages_added: row.get(15)?,
                            messages_removed: row.get(16)?,
                            owner: row.get(17)?,
                        },
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法查询请求账本条目：{error}"))?;
        let Some((summary, envelope)) = found else {
            return Ok(None);
        };
        let mut statement = conn
            .prepare(
                "SELECT part.ordinal, part.kind, part.hash, stored.body, stored.truncated
                 FROM wire_request_part part
                 JOIN wire_blob stored
                   ON stored.conversation_id = part.conversation_id AND stored.hash = part.hash
                 WHERE part.conversation_id = ?1 AND part.seq = ?2
                 ORDER BY part.ordinal",
            )
            .map_err(|error| format!("无法查询请求账本分段：{error}"))?;
        let rows = statement
            .query_map(rusqlite::params![conversation_id, seq], |row| {
                let body: String = row.get(3)?;
                Ok(WireRequestPart {
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
            .map_err(|error| format!("无法查询请求账本分段：{error}"))?;
        let mut parts = Vec::new();
        for row in rows {
            parts.push(row.map_err(|error| format!("无法读取请求账本分段行：{error}"))?);
        }
        Ok(Some(WireRequestDetail {
            summary,
            envelope: serde_json::from_str(&envelope).unwrap_or(serde_json::Value::Null),
            parts,
        }))
    }

    /// Appends one outgoing request to the conversation's wire ledger and returns
    /// the `seq` it was written under, which is the only handle a later usage
    /// report has on this exact row. `None` means the conversation has no row in
    /// the store, so nothing was written and nothing can be attached later.
    pub fn record_wire_request(&self, record: &WireRequestRecord) -> Result<Option<i64>, String> {
        self.with_write_tx(|tx| record_wire_request_tx(tx, record))
    }

    /// Attaches the usage a request's response reported to the row that request
    /// wrote. A no-op when the row is gone or was never written: the ledger must
    /// never fail a run, and a usage with nowhere to land is not an error.
    pub fn record_wire_usage(
        &self,
        conversation_id: &str,
        seq: i64,
        usage: &WireUsage,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            tx.execute(
                "UPDATE wire_request
                 SET input_tokens = ?3, cached_input_tokens = ?4, output_tokens = ?5
                 WHERE conversation_id = ?1 AND seq = ?2",
                rusqlite::params![
                    conversation_id,
                    seq,
                    usage.input_tokens,
                    usage.cached_input_tokens,
                    usage.output_tokens,
                ],
            )
            .map_err(|error| format!("无法写入请求账本用量：{error}"))?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- Write

    /// Replaces a complete conversation for creation, forking, explicit renderer edits, or load-time seeding.
    /// Contexts are reindexed in supplied order; replacement runs only when no run is active.
    pub fn put_conversation(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| put_conversation_tx(tx, workspace_id, conversation))
    }

    /// Atomically persists the child and its explicit first-run intent.
    pub fn put_fork_conversation(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
        prompt_context_id: &str,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            if !matches!(conversation.contexts.last(), Some(ContextItem::User { id, .. }) if id == prompt_context_id) {
                return Err("分叉首轮提示标识无效".into());
            }
            put_conversation_tx(tx, workspace_id, conversation)?;
            tx.execute("INSERT INTO pending_fork_start (conversation_id, prompt_context_id) VALUES (?1, ?2)",
                [&conversation.id, prompt_context_id]).map_err(|error| error.to_string())?;
            Ok(())
        })
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
    pub fn put_conversation_metadata(
        &self,
        workspace_id: &str,
        conversation: &Conversation,
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
            put_conversation_row_tx(tx, workspace_id, conversation)?;
            replace_queued_messages_tx(tx, conversation)?;
            replace_aborted_tasks_tx(tx, conversation)
        })
    }

    /// Deletes a conversation and its dependent rows, re-parenting children to its parent.
    pub fn delete_conversation(&self, conversation_id: &str) -> Result<(), String> {
        self.with_write_tx(|tx| {
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
        })
    }

    /// Reorders a workspace's conversations. The caller moves or deletes omitted conversations;
    /// this operation changes only their `order_key` and workspace ownership.
    pub fn set_workspace_order(
        &self,
        workspace_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), String> {
        self.with_write_tx(|tx| {
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
        })
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
        self.with_write_tx(|tx| {
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
        })
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
        self.with_write_tx(|tx| {
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
        })
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
        self.with_write_tx(|tx| {
            tx.execute(
                "UPDATE context SET data = '{ not json' WHERE conversation_id = ?1
                 AND id = (SELECT id FROM context WHERE conversation_id = ?1
                           ORDER BY order_key, rowid LIMIT 1)",
                [conversation_id],
            )
            .map_err(|error| format!("无法制造坏行：{error}"))?;
            Ok(())
        })
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
        self.with_write_tx(|tx| {
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
        })
    }

    fn reconcile_streaming_where(&self, conversation_id: Option<&str>) -> Result<usize, String> {
        self.with_write_tx(|tx| {
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
        })
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
        self.with_write_tx(|tx| {
            for id in ids {
                tx.execute(
                    "DELETE FROM queued_message WHERE conversation_id = ?1 AND id = ?2",
                    rusqlite::params![conversation_id, id],
                )
                .map_err(|error| format!("无法删除排队消息：{error}"))?;
            }
            Ok(())
        })
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

/// The trunk as of the newest recorded event.
fn timeline_head_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
) -> Result<Vec<TimelineRow>, String> {
    let mut statement = tx
        .prepare(
            "SELECT context_id, data FROM timeline_head
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

fn record_timeline_event_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    kind: TimelineEventKind,
    request_id: Option<&str>,
) -> Result<(), String> {
    if !conversation_exists(tx, conversation_id)? {
        // Subagent and temporary conversations have no row, and so no history.
        return Ok(());
    }
    let current = trunk_rows_tx(tx, conversation_id)?;
    let head = timeline_head_tx(tx, conversation_id)?;
    let ops = diff_timeline(&head, &current);
    if ops.is_empty() {
        return Ok(());
    }
    let previous: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM timeline_event WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取时间线历史序号：{error}"))?
        .flatten();
    let seq = previous.unwrap_or(0) + 1;
    let kind = if previous.is_none() {
        "baseline"
    } else {
        kind.as_str()
    };
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
        tx.execute(
            "INSERT INTO timeline_op
             (conversation_id, seq, ordinal, op, context_id, position, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                conversation_id,
                seq,
                ordinal as i64,
                name,
                context_id,
                position,
                data
            ],
        )
        .map_err(|error| format!("无法写入时间线历史步骤：{error}"))?;
    }
    tx.execute(
        "INSERT INTO timeline_event
         (conversation_id, seq, kind, request_id, inserted, removed, replaced, row_count, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            conversation_id,
            seq,
            kind,
            request_id,
            inserted,
            removed,
            replaced,
            current.len() as i64,
            now(),
        ],
    )
    .map_err(|error| format!("无法写入时间线历史：{error}"))?;
    tx.execute(
        "DELETE FROM timeline_head WHERE conversation_id = ?1",
        [conversation_id],
    )
    .map_err(|error| format!("无法清空时间线历史头：{error}"))?;
    for (position, row) in current.iter().enumerate() {
        tx.execute(
            "INSERT INTO timeline_head (conversation_id, position, context_id, data)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, position as i64, row.id, row.data],
        )
        .map_err(|error| format!("无法写入时间线历史头：{error}"))?;
    }
    Ok(())
}

/// The body as the ledger stores it, and whether the cap cut it. The cut lands on
/// a UTF-8 boundary so the stored text is still text, and the marker goes inside
/// the stored body so a reader sees why it ends where it does.
fn stored_wire_body(body: &str) -> (String, bool) {
    if body.len() <= WIRE_BLOB_MAX_BYTES {
        return (body.to_owned(), false);
    }
    let mut end = WIRE_BLOB_MAX_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}\n{WIRE_BLOB_TRUNCATION_MARKER}", &body[..end]),
        true,
    )
}

/// Lowercase hex SHA-256 of what is actually stored, which is what a reader will
/// be able to verify. Hashing the pre-truncation body would address something no
/// row holds.
fn wire_body_hash(stored: &str) -> String {
    Sha256::digest(stored.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Reads the three usage columns starting at `offset` as one optional value: a
/// row whose provider disclosed nothing must read as "no usage" rather than as a
/// usage of three absent numbers, which the renderer would have to special-case.
fn read_wire_usage(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Option<WireUsage>> {
    let usage = WireUsage {
        input_tokens: row.get(offset)?,
        cached_input_tokens: row.get(offset + 1)?,
        output_tokens: row.get(offset + 2)?,
    };
    Ok((!usage.is_empty()).then_some(usage))
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
fn wire_message_delta(before: &[DeltaMessage<'_>], after: &[DeltaMessage<'_>]) -> (i64, i64) {
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
/// joining `wire_blob` here would make each write read back everything every
/// earlier write stored, and the ledger would cost the square of the
/// conversation to maintain — the exact cost content addressing exists to avoid.
fn previous_wire_messages(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    seq: i64,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut statement = tx
        .prepare(
            "SELECT hash, role FROM wire_request_part
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
struct StoredWirePart<'a> {
    kind: &'a str,
    role: Option<&'a str>,
    author: Option<&'a str>,
    body: String,
    truncated: bool,
    hash: String,
}

fn record_wire_request_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &WireRequestRecord,
) -> Result<Option<i64>, String> {
    let conversation_id = record.conversation_id.as_str();
    let owner = record.owner.as_deref();
    if !conversation_exists(tx, conversation_id)? {
        // A draft or temporary conversation has no row, and so no ledger. A
        // child does have one — it shares its parent's — and is separated from
        // the trunk by `owner` instead.
        return Ok(None);
    }
    // The number is the conversation's, not the ledger's: `seq` is what the part
    // and blob tables key on, and two ledgers handing out the same one would file
    // an agent's parts against a trunk request.
    let previous: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM wire_request WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取请求账本序号：{error}"))?
        .flatten();
    let seq = previous.unwrap_or(0) + 1;
    // The request this one is read against is the previous request *of the same
    // ledger*. Comparing a child's first payload with whatever the trunk last
    // sent would report the whole of one history as deleted and the whole of the
    // other as written by the user.
    let predecessor: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM wire_request WHERE conversation_id = ?1 AND owner IS ?2",
            rusqlite::params![conversation_id, owner],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取请求账本序号：{error}"))?
        .flatten();

    let stored: Vec<StoredWirePart<'_>> = record
        .parts
        .iter()
        .map(|part| {
            let (body, truncated) = stored_wire_body(&part.body);
            let hash = wire_body_hash(&body);
            StoredWirePart {
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

    // `predecessor` is `None` only for a ledger's very first row: `seq` only
    // grows and pruning only drops rows older than the one being written, so a
    // ledger that has ever recorded anything still has a newest row to compare
    // against. An empty predecessor therefore means "nothing was here before",
    // and everything the user wrote is new.
    let previous_messages = match predecessor {
        Some(previous) => previous_wire_messages(tx, conversation_id, previous)?,
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
    let (messages_added, messages_removed) = wire_message_delta(&before, &after);

    tx.execute(
        "INSERT INTO wire_request
         (conversation_id, seq, created_at, kind, request_id, round, attempt,
          provider_name, family, model_id, envelope, part_count, bytes,
          messages_added, messages_removed, owner)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            conversation_id,
            seq,
            now(),
            record.kind,
            record.request_id,
            record.round,
            record.attempt,
            record.provider_name,
            record.family,
            record.model_id,
            record.envelope,
            stored.len() as i64,
            bytes,
            messages_added,
            messages_removed,
            owner,
        ],
    )
    .map_err(|error| format!("无法写入请求账本：{error}"))?;
    for (ordinal, part) in stored.iter().enumerate() {
        // A body this conversation already sent costs one row in total, however
        // many requests carried it.
        tx.execute(
            "INSERT OR IGNORE INTO wire_blob (conversation_id, hash, body, truncated)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                conversation_id,
                part.hash,
                part.body,
                i64::from(part.truncated)
            ],
        )
        .map_err(|error| format!("无法写入请求账本正文：{error}"))?;
        tx.execute(
            "INSERT INTO wire_request_part (conversation_id, seq, ordinal, kind, hash, role)
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
        .map_err(|error| format!("无法写入请求账本分段：{error}"))?;
    }
    prune_wire_ledger_tx(tx, conversation_id, owner)?;
    Ok(Some(seq))
}

/// The `seq` at or below which rows fall outside a retention window, or `None`
/// when the window is not full yet. `owner` scopes it to one ledger; passing the
/// whole conversation counts across all of them.
fn wire_retention_cutoff(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    owner: Option<Option<&str>>,
    keep: i64,
) -> Result<Option<i64>, String> {
    let scoped = owner.is_some();
    let owner = owner.flatten();
    let (filter, slot) = if scoped {
        ("AND owner IS ?2", "?3")
    } else {
        ("", "?2")
    };
    let bound: Vec<&dyn rusqlite::ToSql> = if scoped {
        vec![&conversation_id, &owner, &keep]
    } else {
        vec![&conversation_id, &keep]
    };
    // `OFFSET keep` lands on the newest row that is one past the window, so what
    // comes back is a cutoff rather than a count: everything at or below it goes.
    tx.query_row(
        &format!(
            "SELECT seq FROM wire_request WHERE conversation_id = ?1 {filter}
             ORDER BY seq DESC LIMIT 1 OFFSET {slot}"
        ),
        bound.as_slice(),
        |row| row.get(0),
    )
    .optional()
    .map_err(|error| format!("无法读取请求账本保留窗口：{error}"))
}

/// Drops everything a retention window pushed out, in the same transaction as the
/// write that pushed it. Each ledger is capped on its own so a child cannot evict
/// the session's history, and the conversation is capped across all of them so an
/// unbounded number of children cannot grow the store without end. A body outlives
/// the requests that named it only until the last of them is gone.
fn prune_wire_ledger_tx(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: &str,
    owner: Option<&str>,
) -> Result<(), String> {
    let mut pruned = false;
    if let Some(cutoff) =
        wire_retention_cutoff(tx, conversation_id, Some(owner), WIRE_LEDGER_MAX_REQUESTS)?
    {
        tx.execute(
            "DELETE FROM wire_request_part WHERE conversation_id = ?1 AND seq IN
             (SELECT seq FROM wire_request
              WHERE conversation_id = ?1 AND owner IS ?2 AND seq <= ?3)",
            rusqlite::params![conversation_id, owner, cutoff],
        )
        .map_err(|error| format!("无法清理请求账本分段：{error}"))?;
        tx.execute(
            "DELETE FROM wire_request WHERE conversation_id = ?1 AND owner IS ?2 AND seq <= ?3",
            rusqlite::params![conversation_id, owner, cutoff],
        )
        .map_err(|error| format!("无法清理请求账本：{error}"))?;
        pruned = true;
    }
    if let Some(cutoff) = wire_retention_cutoff(tx, conversation_id, None, WIRE_LEDGER_MAX_ROWS)? {
        tx.execute(
            "DELETE FROM wire_request_part WHERE conversation_id = ?1 AND seq <= ?2",
            rusqlite::params![conversation_id, cutoff],
        )
        .map_err(|error| format!("无法清理请求账本分段：{error}"))?;
        tx.execute(
            "DELETE FROM wire_request WHERE conversation_id = ?1 AND seq <= ?2",
            rusqlite::params![conversation_id, cutoff],
        )
        .map_err(|error| format!("无法清理请求账本：{error}"))?;
        pruned = true;
    }
    if !pruned {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM wire_blob WHERE conversation_id = ?1 AND hash NOT IN
         (SELECT hash FROM wire_request_part WHERE conversation_id = ?1)",
        [conversation_id],
    )
    .map_err(|error| format!("无法清理请求账本正文：{error}"))?;
    Ok(())
}

fn put_conversation_tx(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    conversation: &Conversation,
) -> Result<(), String> {
    put_conversation_row_tx(tx, workspace_id, conversation)?;

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
    record_timeline_event_tx(tx, &conversation.id, TimelineEventKind::Edit, None)
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

/// Inserts or updates the conversation's own row. A new conversation goes to the end of its
/// workspace; an existing one keeps its sidebar position.
fn put_conversation_row_tx(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    conversation: &Conversation,
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
            let max: Option<f64> = tx
                .query_row(
                    "SELECT max(order_key) FROM conversation WHERE workspace_id = ?1",
                    [workspace_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("无法读取对话排序键：{error}"))?
                .flatten();
            max.map(|value| value + ORDER_STEP).unwrap_or(0.0)
        }
    };
    tx.execute(
        "INSERT INTO conversation (id, workspace_id, title, created_at, updated_at, order_key, settings, worktree, run_target, parent_conversation_id, preset_id, template_id, additional_directories, attached_workspaces)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET workspace_id = excluded.workspace_id,
           title = excluded.title, created_at = excluded.created_at,
           updated_at = excluded.updated_at, settings = excluded.settings,
           worktree = excluded.worktree, run_target = excluded.run_target,
           parent_conversation_id = excluded.parent_conversation_id,
           preset_id = excluded.preset_id, template_id = excluded.template_id,
           additional_directories = excluded.additional_directories,
           attached_workspaces = excluded.attached_workspaces",
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

        let events = store.timeline_events("conv_history").expect("events");
        assert_eq!(events.len(), 3, "one entry per committed change");
        assert_eq!(events[0].kind, "baseline");
        assert_eq!(events[0].inserted, 2);
        assert_eq!(events[1].kind, "edit");
        assert_eq!((events[1].inserted, events[1].removed), (1, 0));
        assert_eq!((events[2].removed, events[2].replaced), (1, 1));

        let seeded = store
            .timeline_snapshot("conv_history", events[0].seq)
            .expect("baseline snapshot");
        assert_eq!(
            seeded.iter().map(ContextItem::id).collect::<Vec<_>>(),
            ["ctx_a", "ctx_b"]
        );

        let before_edit = store
            .timeline_snapshot("conv_history", events[1].seq)
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
            .timeline_snapshot("conv_history", events[2].seq)
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
            .record_timeline_event("conv_quiet", TimelineEventKind::Run, Some("req_1"))
            .expect("record");

        let events = store.timeline_events("conv_quiet").expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "baseline");
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

        let events = store.timeline_events("conv_moved").expect("events");
        assert_eq!(events.len(), 2);
        let moved = store
            .timeline_snapshot("conv_moved", events[1].seq)
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
            .record_timeline_event("conv_run", TimelineEventKind::Run, Some("req_7"))
            .expect("record");

        let events = store.timeline_events("conv_run").expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "run");
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
        store.lock().unwrap().execute_batch("DROP TABLE pending_fork_start; DROP TABLE conversation_plan; DROP TABLE fork_decision; DROP TABLE timeline_event; DROP TABLE timeline_op; DROP TABLE timeline_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE wire_request_part; DROP TABLE wire_blob; DROP TABLE wire_request; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 2;").unwrap();
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
        store.lock().unwrap().execute_batch("DROP TABLE conversation_plan; DROP TABLE fork_decision; DROP TABLE timeline_event; DROP TABLE timeline_op; DROP TABLE timeline_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE wire_request_part; DROP TABLE wire_blob; DROP TABLE wire_request; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 3;").unwrap();
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
            .execute_batch("DROP TABLE fork_decision; DROP TABLE timeline_event; DROP TABLE timeline_op; DROP TABLE timeline_head; DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE wire_request_part; DROP TABLE wire_blob; DROP TABLE wire_request; ALTER TABLE conversation DROP COLUMN preset_id; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 4;")
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
                "DROP TABLE template_context; DROP TABLE conversation_template; DROP TABLE wire_request_part; DROP TABLE wire_blob; DROP TABLE wire_request; ALTER TABLE conversation DROP COLUMN template_id; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 7;",
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

    fn wire_record(conversation_id: &str, parts: &[(&str, &str)]) -> WireRequestRecord {
        wire_record_of(
            conversation_id,
            parts
                .iter()
                .map(|(kind, body)| WireRecordedPart {
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
    fn wire_record_of(conversation_id: &str, parts: Vec<WireRecordedPart>) -> WireRequestRecord {
        WireRequestRecord {
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
    fn wire_child_record(
        conversation_id: &str,
        owner: &str,
        parts: &[(&str, &str)],
    ) -> WireRequestRecord {
        WireRequestRecord {
            owner: Some(owner.into()),
            ..wire_record(conversation_id, parts)
        }
    }

    /// The trunk's ledger, which is what the conversation's own pane reads.
    fn trunk_requests(store: &ConversationStore, conversation_id: &str) -> Vec<WireRequestSummary> {
        store.wire_requests(conversation_id, None).expect("ledger")
    }

    fn wire_message(role: &str, author: &str, body: &str) -> WireRecordedPart {
        WireRecordedPart {
            kind: "message".into(),
            role: Some(role.into()),
            author: Some(author.into()),
            body: body.into(),
        }
    }

    fn row_count(store: &ConversationStore, query: &str) -> i64 {
        store
            .lock()
            .expect("lock")
            .query_row(query, [], |row| row.get(0))
            .expect("count")
    }

    /// The renderer reads these rows as JSON, so the names and the absences are
    /// part of the contract: an undisclosed counter must be missing rather than
    /// present and null, which is what the panel distinguishes on.
    #[test]
    fn a_wire_summary_serialises_the_shape_the_panel_reads() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_json"))
            .expect("seed");
        store
            .record_wire_request(&wire_record_of(
                "conv_json",
                vec![wire_message("user", "user", "{\"role\":\"user\"}")],
            ))
            .expect("record");
        store
            .record_wire_usage(
                "conv_json",
                1,
                &WireUsage {
                    input_tokens: Some(12),
                    cached_input_tokens: None,
                    output_tokens: Some(3),
                },
            )
            .expect("attach usage");

        let mut summary = trunk_requests(&store, "conv_json").remove(0);
        summary.created_at = "2026-08-25T00:00:00.000Z".into();
        assert_eq!(
            serde_json::to_value(&summary).expect("serialise"),
            serde_json::json!({
                "seq": 1,
                "createdAt": "2026-08-25T00:00:00.000Z",
                "kind": "model",
                "requestId": "req_a",
                "round": 0,
                "attempt": 0,
                "providerName": "anthropic",
                "family": "messages",
                "modelId": "claude-opus-5",
                "partCount": 1,
                "bytes": 15,
                "usage": { "inputTokens": 12, "outputTokens": 3 },
                "messagesAdded": 1,
                "messagesRemoved": 0,
            })
        );

        // A request whose response never landed carries no `usage` key at all.
        store
            .record_wire_request(&wire_record("conv_json", &[("message", "{}")]))
            .expect("record");
        let unanswered = trunk_requests(&store, "conv_json").remove(1);
        let unanswered = serde_json::to_value(&unanswered).expect("serialise");
        assert!(unanswered.get("usage").is_none());
        assert_eq!(unanswered["messagesRemoved"], serde_json::json!(1));
    }

    /// The ledger is an append-only list: one row per request that went out,
    /// numbered from one, carrying what identified the request rather than what
    /// it said.
    #[test]
    fn wire_ledger_records_a_request_and_numbers_it_from_one() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_wire"))
            .expect("seed");

        let parts = [
            ("system", "you are a cat"),
            ("message", "{\"role\":\"user\"}"),
        ];
        store
            .record_wire_request(&wire_record("conv_wire", &parts))
            .expect("first request");
        let mut second = wire_record("conv_wire", &parts[..1]);
        second.request_id = "req_b".into();
        second.round = 3;
        second.attempt = 1;
        second.kind = "search".into();
        store.record_wire_request(&second).expect("second request");

        let recorded = trunk_requests(&store, "conv_wire");
        assert_eq!(recorded.len(), 2);
        assert_eq!(
            recorded.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [1, 2],
            "序号从 1 开始并逐条递增"
        );
        assert_eq!(recorded[0].kind, "model");
        assert_eq!(recorded[0].request_id, "req_a");
        assert_eq!((recorded[0].round, recorded[0].attempt), (0, 0));
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
        assert_eq!((recorded[1].round, recorded[1].attempt), (3, 1));
        assert_eq!(recorded[1].part_count, 1);
    }

    /// The point of content addressing: a turn's second round re-sends the first
    /// round's messages, and the ledger must not pay for them twice. Each request
    /// still names every part it carried.
    #[test]
    fn wire_ledger_stores_a_repeated_body_once_and_names_it_twice() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_dedupe"))
            .expect("seed");

        let shared = ("message", "{\"role\":\"user\",\"content\":\"hi\"}");
        store
            .record_wire_request(&wire_record("conv_dedupe", &[("system", "rules"), shared]))
            .expect("first round");
        store
            .record_wire_request(&wire_record(
                "conv_dedupe",
                &[
                    ("system", "rules"),
                    shared,
                    ("message", "{\"role\":\"assistant\"}"),
                ],
            ))
            .expect("second round");

        assert_eq!(
            row_count(&store, "SELECT count(*) FROM wire_blob"),
            3,
            "重复的正文只占一行，三段不同正文就是三行"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM wire_request_part"),
            5,
            "每次请求仍然点名自己带过的每一段"
        );
        let detail = store
            .wire_request("conv_dedupe", 2)
            .expect("read")
            .expect("row");
        assert_eq!(detail.parts[1].body, shared.1, "共享正文读回的是原文");
        assert_eq!(
            detail.parts[1].hash,
            store
                .wire_request("conv_dedupe", 1)
                .expect("read")
                .expect("row")
                .parts[1]
                .hash,
            "同一段正文在两次请求里是同一个地址"
        );
    }

    /// The detail view reads a request the way it went out: the parts in wire
    /// order, and the envelope that carried them.
    #[test]
    fn wire_ledger_reads_parts_in_wire_order_with_their_envelope() {
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
            .record_wire_request(&wire_record("conv_detail", &parts))
            .expect("record");

        let detail = store
            .wire_request("conv_detail", 1)
            .expect("read")
            .expect("row");
        assert_eq!(detail.summary.seq, 1);
        assert_eq!(detail.summary.part_count, 4);
        assert_eq!(
            detail.envelope,
            serde_json::json!({ "maxOutputTokens": 4096 })
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
        assert_eq!(store.wire_request("conv_detail", 7).expect("read"), None);

        // A corrupt envelope costs its own field, not the parts recorded beside it.
        store
            .lock()
            .expect("lock")
            .execute(
                "UPDATE wire_request SET envelope = 'not json' WHERE conversation_id = 'conv_detail'",
                [],
            )
            .expect("corrupt");
        let corrupt = store
            .wire_request("conv_detail", 1)
            .expect("read")
            .expect("row");
        assert_eq!(corrupt.envelope, serde_json::Value::Null);
        assert_eq!(corrupt.parts.len(), 4);
    }

    #[test]
    fn wire_ledger_ignores_a_conversation_it_has_no_row_for() {
        let (_dir, store) = temp_store();
        store
            .record_wire_request(&wire_record("subagent_only", &[("message", "{}")]))
            .expect("库里没有行的对话不入账也不报错");
        assert!(trunk_requests(&store, "subagent_only").is_empty());
        assert_eq!(row_count(&store, "SELECT count(*) FROM wire_request"), 0);
        assert_eq!(row_count(&store, "SELECT count(*) FROM wire_blob"), 0);
    }

    /// A child runs under its parent's conversation id, so the only thing keeping
    /// its traffic out of the session's own ledger is `owner`. Read the trunk and
    /// a child never appears; read the child and the trunk never does.
    #[test]
    fn a_child_turns_requests_form_a_ledger_of_their_own() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_owned"))
            .expect("seed");
        store
            .record_wire_request(&wire_record("conv_owned", &[("message", "{\"t\":1}")]))
            .expect("trunk");
        store
            .record_wire_request(&wire_child_record(
                "conv_owned",
                "reviewer",
                &[("message", "{\"c\":1}")],
            ))
            .expect("child");
        store
            .record_wire_request(&wire_child_record(
                "conv_owned",
                "reviewer",
                &[("message", "{\"c\":2}")],
            ))
            .expect("child again");
        store
            .record_wire_request(&wire_record("conv_owned", &[("message", "{\"t\":2}")]))
            .expect("trunk again");

        let trunk = trunk_requests(&store, "conv_owned");
        assert_eq!(
            trunk.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![1, 4],
            "主干账本只有主干自己发出的请求"
        );
        assert!(trunk.iter().all(|row| row.owner.is_none()));

        let owners = ["reviewer".to_owned()];
        let child = store
            .wire_requests("conv_owned", Some(&owners))
            .expect("child ledger");
        assert_eq!(
            child.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![2, 3],
            "子代理账本只有它自己发出的请求"
        );
        assert_eq!(child[0].owner.as_deref(), Some("reviewer"));
        assert_eq!(
            store
                .wire_request("conv_owned", 2)
                .expect("read")
                .expect("row")
                .summary
                .owner
                .as_deref(),
            Some("reviewer"),
            "逐条读回来也带着归属"
        );
        assert!(
            store
                .wire_requests("conv_owned", Some(&[]))
                .expect("no owners")
                .is_empty(),
            "还没有被寻址过的代理什么都没发出去，不能拿主干的行搪塞"
        );
    }

    /// The request a payload is read against is the previous one *of the same
    /// ledger*. Against the trunk's, a child's first payload would report the
    /// session's whole history as deleted.
    #[test]
    fn a_child_request_is_read_against_its_own_predecessor() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_delta_owned"))
            .expect("seed");
        store
            .record_wire_request(&wire_record_of(
                "conv_delta_owned",
                vec![wire_message("user", "user", "{\"role\":\"user\",\"t\":1}")],
            ))
            .expect("trunk");
        store
            .record_wire_request(&WireRequestRecord {
                owner: Some("writer".into()),
                ..wire_record_of(
                    "conv_delta_owned",
                    vec![wire_message("user", "user", "{\"role\":\"user\",\"c\":1}")],
                )
            })
            .expect("child");

        let owners = ["writer".to_owned()];
        let child = store
            .wire_requests("conv_delta_owned", Some(&owners))
            .expect("child ledger");
        assert_eq!(
            (child[0].messages_added, child[0].messages_removed),
            (Some(1), Some(0)),
            "子代理的第一条载荷之前什么都没有，不得把主干的历史算成被删"
        );
    }

    /// Each ledger is capped on its own. A child that runs for hundreds of rounds
    /// must not push the session's own history out from under the reader.
    #[test]
    fn a_chatty_child_does_not_evict_the_trunks_history() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_shared_cap"))
            .expect("seed");
        store
            .record_wire_request(&wire_record(
                "conv_shared_cap",
                &[("message", "{\"trunk\":1}")],
            ))
            .expect("trunk");
        for index in 1..=(WIRE_LEDGER_MAX_REQUESTS + 5) {
            let unique = format!("{{\"child\":{index}}}");
            store
                .record_wire_request(&wire_child_record(
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
            store
                .wire_requests("conv_shared_cap", Some(&owners))
                .expect("child ledger")
                .len(),
            WIRE_LEDGER_MAX_REQUESTS as usize,
            "子代理自己的账本照常按上限收窄"
        );
    }

    /// Everything written before the column existed is trunk traffic: children
    /// were not recorded at all until it did.
    #[test]
    fn version_twelve_upgrades_in_place_and_reads_old_rows_as_the_trunks() {
        let (dir, store) = temp_store();
        let mut source = conversation("released_v12");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        store
            .record_wire_request(&wire_record(
                &source.id,
                &[("message", "{\"role\":\"user\",\"content\":\"旧的\"}")],
            ))
            .expect("seed ledger");
        store
            .lock()
            .expect("lock")
            .execute_batch(
                "ALTER TABLE wire_request DROP COLUMN owner; PRAGMA user_version = 12;",
            )
            .expect("downgrade to v12");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("upgrade");
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION, "升级必须写入当前版本号");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "v12 升级必须保留既有对话历史"
        );
        let carried = trunk_requests(&store, &source.id);
        assert_eq!(carried.len(), 1, "旧行必须留在主干账本里");
        assert_eq!(carried[0].owner, None);
        store
            .record_wire_request(&wire_child_record(
                &source.id,
                "late-agent",
                &[("message", "{\"role\":\"user\",\"content\":\"子的\"}")],
            ))
            .expect("write child ledger after upgrade");
        let owners = ["late-agent".to_owned()];
        assert_eq!(
            store
                .wire_requests(&source.id, Some(&owners))
                .expect("child ledger")
                .len(),
            1,
            "升级后必须能按归属写入与读取"
        );
        assert_eq!(
            trunk_requests(&store, &source.id).len(),
            1,
            "升级后主干账本不得被子代理的行污染"
        );
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

    /// A store can carry a current stamp and still be missing a column: a build
    /// whose upgrade steps differed, an upgrade that died between two `ALTER`s, a
    /// database edited by hand. The stamp is not evidence of shape, so the open
    /// path repairs from the schema it can read rather than the number it is told.
    #[test]
    fn a_store_stamped_current_but_missing_owner_is_repaired_on_open() {
        let (dir, store) = temp_store();
        let mut source = conversation("stamped_current");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        store
            .record_wire_request(&wire_record(
                &source.id,
                &[("message", "{\"role\":\"user\",\"content\":\"旧的\"}")],
            ))
            .expect("seed ledger");
        // The column goes and the stamp stays — the one shape a version ladder can
        // never see, because it only repairs what the stamp admits is outstanding.
        store
            .lock()
            .expect("lock")
            .execute_batch("ALTER TABLE wire_request DROP COLUMN owner")
            .expect("drop owner behind a current stamp");
        let stamped: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(stamped, STORE_VERSION, "前提是版本号仍然自称当前");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("repair");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "补列不得动到既有对话历史"
        );
        let carried = trunk_requests(&store, &source.id);
        assert_eq!(carried.len(), 1, "补列前写下的行必须留在主干账本里");
        assert_eq!(carried[0].owner, None);
        store
            .record_wire_request(&wire_child_record(
                &source.id,
                "late-agent",
                &[("message", "{\"role\":\"user\",\"content\":\"子的\"}")],
            ))
            .expect("write child ledger after repair");
        let owners = ["late-agent".to_owned()];
        assert_eq!(
            store
                .wire_requests(&source.id, Some(&owners))
                .expect("child ledger")
                .len(),
            1,
            "补列后必须能按归属写入与读取"
        );
        assert!(
            std::fs::read_dir(dir.path()).expect("directory").all(|entry| {
                !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("quarantine-")
            }),
            "缺一列是可就地修复的，不该封存整个对话库"
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
            "timeline_event",
            "timeline_op",
            "timeline_head",
            "conversation_template",
            "template_context",
            "wire_request",
            "wire_blob",
            "wire_request_part",
        ] {
            assert!(
                declared.contains(&expected),
                "{expected} 必须能从建表语句里读出来：{declared:?}"
            );
        }
        assert_eq!(declared.len(), 16, "读出的表名与实际建的表不符：{declared:?}");
        for &(table, _, _) in ADDED_COLUMNS {
            assert!(
                declared.contains(&table),
                "{table} 不在建表语句里，补列会打在一张不存在的表上"
            );
        }
    }



    /// A body past the cap is stored cut, and the address is the address of what
    /// was stored: a reader can hash the text they were given and get the hash the
    /// ledger holds.
    #[test]
    fn an_oversized_wire_body_is_stored_cut_and_addressed_as_stored() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_big"))
            .expect("seed");
        // Three-byte characters, so the cap does not land on a boundary.
        let huge = "漢".repeat(WIRE_BLOB_MAX_BYTES / 3 + 16);
        assert!(huge.len() > WIRE_BLOB_MAX_BYTES);
        store
            .record_wire_request(&wire_record("conv_big", &[("message", huge.as_str())]))
            .expect("record");

        let detail = store
            .wire_request("conv_big", 1)
            .expect("read")
            .expect("row");
        let part = &detail.parts[0];
        assert!(part.truncated, "超限正文必须标记为已截断");
        assert!(part.body.ends_with(WIRE_BLOB_TRUNCATION_MARKER));
        assert!(
            part.body.starts_with(&huge[..WIRE_BLOB_MAX_BYTES - 1]),
            "截断落在字符边界上，前面的正文原样保留"
        );
        assert_eq!(
            part.body.len(),
            WIRE_BLOB_MAX_BYTES - 1 + 1 + WIRE_BLOB_TRUNCATION_MARKER.len()
        );
        assert_eq!(
            part.hash,
            wire_body_hash(&part.body),
            "哈希对应的是落库的正文"
        );
        assert_ne!(part.hash, wire_body_hash(&huge));
        assert_eq!(detail.summary.bytes, part.body.len() as i64);
    }

    /// The ledger is bounded per conversation: a long-lived conversation keeps the
    /// recent requests, and the bodies only the pruned ones named go with them.
    #[test]
    fn wire_ledger_keeps_the_newest_requests_and_drops_bodies_nothing_names() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_cap"))
            .expect("seed");
        let overflow = 5;
        for index in 1..=(WIRE_LEDGER_MAX_REQUESTS + overflow) {
            let unique = format!("{{\"round\":{index}}}");
            store
                .record_wire_request(&wire_record(
                    "conv_cap",
                    &[("system", "共享提示"), ("message", unique.as_str())],
                ))
                .expect("record");
        }

        let recorded = trunk_requests(&store, "conv_cap");
        assert_eq!(recorded.len(), WIRE_LEDGER_MAX_REQUESTS as usize);
        assert_eq!(recorded.first().expect("oldest").seq, overflow + 1);
        assert_eq!(
            recorded.last().expect("newest").seq,
            WIRE_LEDGER_MAX_REQUESTS + overflow
        );
        for seq in 1..=overflow {
            assert_eq!(
                store.wire_request("conv_cap", seq).expect("read"),
                None,
                "第 {seq} 条已被保留上限挤出"
            );
        }
        assert_eq!(
            row_count(
                &store,
                "SELECT count(*) FROM wire_blob WHERE body = '{\"round\":1}'"
            ),
            0,
            "被挤出的请求独有的正文不得留成孤儿"
        );
        assert_eq!(
            row_count(
                &store,
                "SELECT count(*) FROM wire_blob WHERE body = '共享提示'"
            ),
            1,
            "还有请求点名的共享正文必须留下"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM wire_blob"),
            WIRE_LEDGER_MAX_REQUESTS + 1,
            "留下的正文恰好是活着的请求点名的那些"
        );
        assert_eq!(
            row_count(&store, "SELECT count(*) FROM wire_request_part"),
            WIRE_LEDGER_MAX_REQUESTS * 2
        );
    }

    /// Usage arrives after the row it belongs to was already written, so the
    /// only thing that can be wrong is which row it lands on.
    #[test]
    fn wire_usage_lands_on_the_row_that_earned_it() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_usage"))
            .expect("seed");
        store
            .record_wire_request(&wire_record("conv_usage", &[("message", "{}")]))
            .expect("first");
        store
            .record_wire_request(&wire_record("conv_usage", &[("message", "{}")]))
            .expect("second");

        let usage = WireUsage {
            input_tokens: Some(1200),
            cached_input_tokens: Some(1000),
            output_tokens: Some(48),
        };
        store
            .record_wire_usage("conv_usage", 1, &usage)
            .expect("attach usage");

        let recorded = trunk_requests(&store, "conv_usage");
        assert_eq!(recorded[0].usage, Some(usage), "用量落在自己的那一行上");
        assert_eq!(recorded[1].usage, None, "别的行不得被顺带写上用量");
        assert_eq!(
            store
                .wire_request("conv_usage", 1)
                .expect("read")
                .expect("row")
                .summary
                .usage,
            Some(usage),
            "详情读到的用量与列表一致"
        );
    }

    /// A provider that discloses one counter and not the others reads as one
    /// counter and two absences, never as zeros.
    #[test]
    fn a_partly_disclosed_wire_usage_keeps_its_absences() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_partial"))
            .expect("seed");
        store
            .record_wire_request(&wire_record("conv_partial", &[("message", "{}")]))
            .expect("record");
        store
            .record_wire_usage(
                "conv_partial",
                1,
                &WireUsage {
                    output_tokens: Some(7),
                    ..WireUsage::default()
                },
            )
            .expect("attach usage");

        let recorded = trunk_requests(&store, "conv_partial");
        assert_eq!(
            recorded[0].usage,
            Some(WireUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: Some(7),
            })
        );
    }

    /// Retention can drop a row between the send and the response. The ledger
    /// must not turn that into a failed run.
    #[test]
    fn wire_usage_for_a_row_that_is_gone_is_a_no_op() {
        let (_dir, store) = temp_store();
        store
            .put_conversation("ws", &conversation("conv_orphan"))
            .expect("seed");
        store
            .record_wire_request(&wire_record("conv_orphan", &[("message", "{}")]))
            .expect("record");
        store
            .record_wire_usage(
                "conv_orphan",
                404,
                &WireUsage {
                    input_tokens: Some(9),
                    ..WireUsage::default()
                },
            )
            .expect("补记到不存在的行不得报错");
        store
            .record_wire_usage(
                "missing_conversation",
                1,
                &WireUsage {
                    input_tokens: Some(9),
                    ..WireUsage::default()
                },
            )
            .expect("补记到不存在的对话不得报错");

        assert_eq!(
            trunk_requests(&store, "conv_orphan")[0].usage,
            None,
            "写偏的用量不得落到别的行上"
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
        let first = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = wire_message(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        store
            .record_wire_request(&wire_record_of(
                "conv_add",
                vec![first.clone(), answer.clone()],
            ))
            .expect("first request");
        store
            .record_wire_request(&wire_record_of("conv_add", vec![first, answer, second]))
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
        let asked = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"跑一下\"}");
        let called = wire_message(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":[{\"type\":\"tool-call\"}]}",
        );
        // A `user` role the recorder attributed to the model: the host handing
        // back the result of a call the model made.
        let returned = wire_message(
            "user",
            "model",
            "{\"role\":\"user\",\"content\":[{\"type\":\"tool-result\"}]}",
        );
        store
            .record_wire_request(&wire_record_of("conv_loop", vec![asked.clone()]))
            .expect("first request");
        store
            .record_wire_request(&wire_record_of("conv_loop", vec![asked, called, returned]))
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
        let first = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = wire_message(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        store
            .record_wire_request(&wire_record_of(
                "conv_delete",
                vec![first.clone(), answer, second.clone()],
            ))
            .expect("first request");
        store
            .record_wire_request(&wire_record_of("conv_delete", vec![first, second]))
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
        let first = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"一\"}");
        let answer = wire_message(
            "assistant",
            "model",
            "{\"role\":\"assistant\",\"content\":\"答\"}",
        );
        let second = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"二\"}");
        let rewritten = wire_message("user", "user", "{\"role\":\"user\",\"content\":\"二改\"}");
        store
            .record_wire_request(&wire_record_of(
                "conv_edit",
                vec![first.clone(), answer.clone(), second],
            ))
            .expect("first request");
        store
            .record_wire_request(&wire_record_of("conv_edit", vec![first, answer, rewritten]))
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
            .record_wire_request(&wire_record_of(
                "conv_first",
                vec![
                    WireRecordedPart {
                        kind: "system".into(),
                        role: None,
                        author: None,
                        body: "规则".into(),
                    },
                    wire_message("user", "user", "{\"role\":\"user\",\"content\":\"一\"}"),
                    wire_message(
                        "assistant",
                        "model",
                        "{\"role\":\"assistant\",\"content\":\"答\"}",
                    ),
                    wire_message("user", "user", "{\"role\":\"user\",\"content\":\"二\"}"),
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
    fn wire_message_delta_strips_the_common_head_and_tail_before_pairing_roles() {
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
            wire_message_delta(
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
            wire_message_delta(
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
            wire_message_delta(
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
            wire_message_delta(
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
            wire_message_delta(&[before("x", None)], &[after("y", None, "user")]),
            (1, 1),
            "读不出角色的分段不与任何东西配对"
        );
    }

    #[test]
    fn version_ten_upgrades_in_place_and_gains_usage_and_delta_columns() {
        let (dir, store) = temp_store();
        let mut source = conversation("released_v10");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        let carried_over = "{\"role\":\"user\",\"content\":\"旧的\"}";
        store
            .record_wire_request(&wire_record(&source.id, &[("message", carried_over)]))
            .expect("seed ledger");
        store
            .lock()
            .expect("lock")
            .execute_batch(
                "ALTER TABLE wire_request DROP COLUMN input_tokens;
                 ALTER TABLE wire_request DROP COLUMN cached_input_tokens;
                 ALTER TABLE wire_request DROP COLUMN output_tokens;
                 ALTER TABLE wire_request DROP COLUMN messages_added;
                 ALTER TABLE wire_request DROP COLUMN messages_removed;
                 ALTER TABLE wire_request DROP COLUMN owner;
                 ALTER TABLE wire_request_part DROP COLUMN role;
                 ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 10;",
            )
            .expect("downgrade to v10");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("upgrade");
        let version: i32 = store
            .lock()
            .expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, STORE_VERSION, "升级必须写入当前版本号");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "v10 升级必须保留既有对话历史"
        );
        let carried = trunk_requests(&store, &source.id);
        assert_eq!(carried.len(), 1, "v10 升级必须保留既有账本行");
        assert_eq!(
            (
                carried[0].usage,
                carried[0].messages_added,
                carried[0].messages_removed
            ),
            (None, None, None),
            "第 11 版之前写下的行没有这些数字，读回来必须是空"
        );

        store
            .record_wire_request(&wire_record_of(
                &source.id,
                vec![
                    // The same body the pre-upgrade row carried, so the delta
                    // aligns against a row whose `role` column is NULL because
                    // the column did not exist when it was written.
                    WireRecordedPart {
                        kind: "message".into(),
                        role: Some("user".into()),
                        author: Some("user".into()),
                        body: carried_over.into(),
                    },
                    wire_message("user", "user", "{\"role\":\"user\",\"content\":\"新的\"}"),
                ],
            ))
            .expect("write ledger after upgrade");
        store
            .record_wire_usage(
                &source.id,
                2,
                &WireUsage {
                    input_tokens: Some(11),
                    cached_input_tokens: None,
                    output_tokens: Some(3),
                },
            )
            .expect("attach usage after upgrade");
        let recorded = trunk_requests(&store, &source.id);
        assert_eq!(
            recorded[1].usage,
            Some(WireUsage {
                input_tokens: Some(11),
                cached_input_tokens: None,
                output_tokens: Some(3),
            }),
            "升级后必须能写入并读回用量"
        );
        assert_eq!(
            (recorded[1].messages_added, recorded[1].messages_removed),
            (Some(1), Some(0)),
            "升级后必须能写入并读回增删计数"
        );
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
            "v10 升级不得隔离原数据库"
        );
    }

    #[test]
    fn version_nine_upgrades_in_place_and_gains_the_wire_ledger() {
        let (dir, store) = temp_store();
        let mut source = conversation("released_v9");
        source.contexts.push(user("history", "保留的历史"));
        store.put_conversation("ws", &source).expect("seed history");
        store
            .lock()
            .expect("lock")
            .execute_batch(
                "DROP TABLE wire_request_part; DROP TABLE wire_blob; DROP TABLE wire_request; ALTER TABLE conversation DROP COLUMN additional_directories; PRAGMA user_version = 9;",
            )
            .expect("downgrade to v9");
        drop(store);

        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).expect("upgrade");
        assert_eq!(
            store
                .conversation(&source.id)
                .expect("read history")
                .expect("preserved row")
                .contexts,
            source.contexts,
            "v9 升级必须保留既有对话历史"
        );
        store
            .record_wire_request(&wire_record(
                &source.id,
                &[("message", "{\"role\":\"user\"}")],
            ))
            .expect("write ledger after upgrade");
        let recorded = trunk_requests(&store, &source.id);
        assert_eq!(recorded.len(), 1, "升级后必须能写入请求账本");
        assert_eq!(
            store
                .wire_request(&source.id, recorded[0].seq)
                .expect("read")
                .expect("row")
                .parts[0]
                .body,
            "{\"role\":\"user\"}",
            "升级后必须能读回请求正文"
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
            "v9 升级不得隔离原数据库"
        );
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
