//! Durable conversation storage in a process-local SQLite database. The host is the sole writer.
//!
//! Each message occupies one JSON-backed row, updated with row-scoped `UPSERT`s. `run_model`
//! persists every canonical context directly, without renderer round trips or debounce.
//! Streaming prose uses `streaming` status and becomes `settled` when finalized; startup recovery
//! marks stale streaming rows as `interrupted`.
//!
//! User-editable non-conversation configuration remains in the `document.v1.json` anchor file.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::model::{
    ContextItem, Conversation, ConversationBranch, ConversationSettings, ConversationWorktree,
    ImageAttachment, QueuedMessage, RunTarget, UserAbortedTaskRecord,
};

/// Database file name, stored beside the anchor file.
pub const DATABASE_FILE_NAME: &str = "conversations.v1.sqlite3";

/// `PRAGMA user_version`. Every upgrade so far is additive and in place, so a
/// released user's history survives; only unknown versions are quarantined and rebuilt.
pub const STORE_VERSION: i32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingForkStart {
    pub workspace_id: String,
    pub conversation_id: String,
    pub prompt_context_id: String,
}

const FORK_START_SCHEMA: &str = "CREATE TABLE pending_fork_start (
    conversation_id TEXT PRIMARY KEY REFERENCES conversation(id) ON DELETE CASCADE,
    prompt_context_id TEXT NOT NULL
) STRICT;";

/// One plan document per conversation. A `plan` write replaces the whole
/// document, so there is no history here; the timeline keeps the calls.
const PLAN_SCHEMA: &str = "CREATE TABLE conversation_plan (
    conversation_id TEXT PRIMARY KEY REFERENCES conversation(id) ON DELETE CASCADE,
    markdown        TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('draft','approved','rejected')),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;";

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
CREATE TABLE conversation (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    title        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    order_key    REAL NOT NULL,
    settings     TEXT NOT NULL,
    -- 隔离工作树记录的 JSON；NULL = 跑在工作区根上。
    -- 单独一列而不是塞进 settings：settings 会被预设与工作区快照整份复制，
    -- 而一条工作树路径复制给另一个对话就是错的。
    worktree     TEXT,
    -- 运行地点（RunTarget）的 JSON；NULL = 本机。
    -- 与 worktree 同理单独一列：一条 SSH 机器绑定复制给另一个对话就是错的。
    run_target   TEXT,
    -- NULL = top-level; no foreign key because deleting a parent re-parents children.
    parent_conversation_id TEXT
) STRICT;

CREATE INDEX conversation_workspace_order_idx ON conversation (workspace_id, order_key);

CREATE TABLE branch (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    fork_context_id TEXT NOT NULL,
    active          INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    order_key       REAL NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;

CREATE TABLE context (
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

CREATE INDEX context_order_idx ON context (conversation_id, branch_id, order_key);
CREATE INDEX context_status_idx ON context (status);

CREATE TABLE queued_message (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    order_key       REAL NOT NULL,
    content         TEXT NOT NULL,
    images          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;

CREATE TABLE aborted_task (
    conversation_id TEXT NOT NULL REFERENCES conversation (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    order_key       REAL NOT NULL,
    data            TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id)
) STRICT;
"#;

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
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建对话库目录：{error}"))?;
        }
        let conn = open_configured(db_path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        match store.ensure_schema() {
            Ok(()) => Ok(store),
            Err(error) => {
                // Quarantine and rebuild an incompatible or corrupt database without blocking startup.
                drop(store);
                quarantine_database(db_path, &error);
                let conn = open_configured(db_path)?;
                let store = Self {
                    conn: Mutex::new(conn),
                };
                store.ensure_schema()?;
                Ok(store)
            }
        }
    }

    fn ensure_schema(&self) -> Result<(), String> {
        let mut conn = self.lock()?;
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("无法读取对话库版本：{error}"))?;
        if version == STORE_VERSION {
            // A current schema already exists. Empty and mismatched stores follow the paths below.
            return Ok(());
        }
        if version == 1 || version == 2 || version == 3 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("无法开启对话库升级事务：{error}"))?;
            if version == 1 {
                tx.execute_batch("ALTER TABLE conversation ADD COLUMN parent_conversation_id TEXT")
                    .map_err(|error| format!("无法升级对话库结构：{error}"))?;
            }
            if version <= 2 {
                tx.execute_batch(FORK_START_SCHEMA).map_err(|error| error.to_string())?;
            }
            tx.execute_batch(PLAN_SCHEMA).map_err(|error| error.to_string())?;
            tx.pragma_update(None, "user_version", STORE_VERSION)
                .map_err(|error| format!("无法写入对话库版本：{error}"))?;
            tx.commit()
                .map_err(|error| format!("无法提交对话库升级事务：{error}"))?;
            return Ok(());
        }
        if version != 0 {
            return Err(format!(
                "对话库版本 {version} 与当前实现的 {STORE_VERSION} 不一致"
            ));
        }
        let has_tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'conversation'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("无法检查对话库结构：{error}"))?;
        if has_tables > 0 {
            return Err("对话库缺少版本标记但已有数据表".into());
        }
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|error| format!("无法建立对话库结构：{error}"))?;
        conn.execute_batch(FORK_START_SCHEMA).map_err(|error| error.to_string())?;
        conn.execute_batch(PLAN_SCHEMA).map_err(|error| error.to_string())?;
        conn.pragma_update(None, "user_version", STORE_VERSION)
            .map_err(|error| format!("无法写入对话库版本：{error}"))?;
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
                "SELECT title, created_at, updated_at, settings, worktree, run_target, parent_conversation_id FROM conversation WHERE id = ?1",
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
        )) = shell
        else {
            return Ok(None);
        };
        let settings: ConversationSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("对话设置无法解析：{error}"))?;
        // An unreadable worktree record falls back to the workspace root, which is the safe target.
        let worktree = worktree_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<ConversationWorktree>(value).ok());
        // An unreadable run target must not fall back to local execution. Bind it to a nonexistent
        // machine so dispatch fails explicitly until the user selects a valid target.
        let run_target = run_target_json.as_deref().map(|value| {
            serde_json::from_str::<RunTarget>(value).unwrap_or(RunTarget::Ssh {
                machine_id: "invalid-run-target".into(),
            })
        });

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
                    "SELECT id, content, images, created_at FROM queued_message
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
                    ))
                })
                .map_err(|error| format!("无法查询排队消息：{error}"))?;
            for row in rows {
                let (id, content, images_json, created_at) =
                    row.map_err(|error| format!("无法读取排队消息：{error}"))?;
                let images: Vec<ImageAttachment> = serde_json::from_str(&images_json)
                    .map_err(|error| format!("排队消息附图无法解析：{error}"))?;
                queued_messages.push(QueuedMessage {
                    id,
                    content,
                    images,
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
            worktree,
            run_target,
            parent_conversation_id,
        }))
    }

    /// Returns the last finalized trunk context available as a fork boundary.
    pub fn last_settled_context_id(&self, conversation_id: &str) -> Result<Option<String>, String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id FROM context WHERE conversation_id = ?1 AND branch_id IS NULL
             AND status = 'settled' ORDER BY order_key DESC, rowid DESC LIMIT 1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法读取已定稿上下文边界：{error}"))
    }

    /// Returns finalized trunk contexts in timeline order, excluding live output.
    pub fn settled_contexts(&self, conversation_id: &str) -> Result<Vec<ContextItem>, String> {
        let conn = self.lock()?;
        read_contexts_with_status(&conn, conversation_id, None, Some(ContextStatus::Settled))
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
        let rows = query.query_map([], |row| Ok(PendingForkStart {
            workspace_id: row.get(0)?, conversation_id: row.get(1)?, prompt_context_id: row.get(2)?,
        })).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
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
                ).map_err(|error| format!("无法更新跨工作区父对话：{error}"))?;
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
    let conn =
        Connection::open(db_path).map_err(|error| format!("无法打开对话库：{error}"))?;
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

fn quarantine_database(db_path: &Path, reason: &str) {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let quarantined = db_path.with_extension(format!("quarantine-{stamp}.sqlite3"));
    let _ = std::fs::rename(db_path, &quarantined);
    for suffix in ["-wal", "-shm"] {
        let mut aux = db_path.as_os_str().to_owned();
        aux.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(aux));
    }
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
            .query_map(rusqlite::params![conversation_id, following], |row| row.get(0))
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
    let data = serde_json::to_string(item)
        .map_err(|error| format!("上下文无法序列化：{error}"))?;
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
    replace_aborted_tasks_tx(tx, conversation)
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
    let worktree = conversation
        .worktree
        .as_ref()
        .map(|value| serde_json::to_string(value))
        .transpose()
        .map_err(|error| format!("对话工作树记录无法序列化：{error}"))?;
    let run_target = conversation
        .run_target
        .as_ref()
        .map(|value| serde_json::to_string(value))
        .transpose()
        .map_err(|error| format!("对话运行地点无法序列化：{error}"))?;
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
        "INSERT INTO conversation (id, workspace_id, title, created_at, updated_at, order_key, settings, worktree, run_target, parent_conversation_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET workspace_id = excluded.workspace_id,
           title = excluded.title, created_at = excluded.created_at,
           updated_at = excluded.updated_at, settings = excluded.settings,
           worktree = excluded.worktree, run_target = excluded.run_target,
           parent_conversation_id = excluded.parent_conversation_id",
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
        tx.execute(
            "INSERT INTO queued_message (conversation_id, id, order_key, content, images, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                conversation.id,
                message.id,
                index as f64 * ORDER_STEP,
                message.content,
                images,
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
    read_contexts_with_status(conn, conversation_id, branch_id, None)
}

fn read_contexts_with_status(
    conn: &Connection,
    conversation_id: &str,
    branch_id: Option<&str>,
    status: Option<ContextStatus>,
) -> Result<Vec<ContextItem>, String> {
    let mut statement = conn
        .prepare(
            "SELECT data FROM context WHERE conversation_id = ?1 AND branch_id IS ?2
             AND (?3 IS NULL OR status = ?3) ORDER BY order_key, rowid",
        )
        .map_err(|error| format!("无法查询上下文：{error}"))?;
    let rows = statement
        .query_map(
            rusqlite::params![conversation_id, branch_id, status.map(ContextStatus::as_str)],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("无法查询上下文：{error}"))?;
    let mut items = Vec::new();
    for row in rows {
        let data = row.map_err(|error| format!("无法读取上下文：{error}"))?;
        let item: ContextItem = serde_json::from_str(&data)
            .map_err(|error| format!("上下文无法解析：{error}"))?;
        items.push(item);
    }
    Ok(items)
}

/// Marks assistant or reasoning context JSON as interrupted. Other kinds have no `interrupted`
/// field and never exist in `streaming` status.
fn mark_interrupted(data: &str) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(data)
        .map_err(|error| format!("上下文无法解析：{error}"))?;
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
            "systemPrompt": "",
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
            worktree: None,
            run_target: None,
            parent_conversation_id: None,
        }
    }

    fn user(id: &str, content: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: content.into(),
            images: Vec::new(),
            created_at: "2026-08-25T00:00:01.000Z".into(),
        }
    }

    fn user_at(id: &str, created_at: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: "hi".into(),
            images: Vec::new(),
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

    #[test]
    fn fork_start_survives_reopen_and_is_consumed_only_at_acceptance() {
        let (dir, store) = temp_store();
        let mut child = conversation("fork_child");
        child.contexts.push(user("fork_prompt", "answer once"));
        store.put_fork_conversation("ws", &child, "fork_prompt").unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(store.pending_fork_starts().unwrap(), vec![PendingForkStart {
            workspace_id: "ws".into(), conversation_id: child.id.clone(), prompt_context_id: "fork_prompt".into(),
        }]);
        assert!(store.accept_fork_start(&child.id, Some("wrong_prompt"), || panic!("must not establish")).is_err());
        assert_eq!(store.pending_fork_starts().unwrap().len(), 1);
        let mut established = false;
        store.accept_fork_start(&child.id, Some("fork_prompt"), || { established = true; }).unwrap();
        assert!(established);
        assert!(store.pending_fork_starts().unwrap().is_empty());
        assert!(store.accept_fork_start(&child.id, Some("fork_prompt"), || panic!("duplicate run")).is_err());
        assert_eq!(store.conversation(&child.id).unwrap().unwrap().contexts.len(), 1);
    }

    #[test]
    fn fork_start_creation_is_atomic_and_delete_cascades() {
        let (_dir, store) = temp_store();
        let mut child = conversation("fork_child");
        child.contexts.push(user("prompt", "hello"));
        store.lock().unwrap().execute_batch("CREATE TRIGGER fail_intent BEFORE INSERT ON pending_fork_start BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(store.put_fork_conversation("ws", &child, "prompt").is_err());
        assert!(store.conversation(&child.id).unwrap().is_none());
        store.lock().unwrap().execute_batch("DROP TRIGGER fail_intent").unwrap();
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
        store.lock().unwrap().execute_batch("DROP TABLE pending_fork_start; DROP TABLE conversation_plan; PRAGMA user_version = 2;").unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(store.conversation(&child.id).unwrap().unwrap().contexts.len(), 1);
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
        store.lock().unwrap().execute_batch("DROP TABLE conversation_plan; PRAGMA user_version = 3;").unwrap();
        drop(store);
        let store = ConversationStore::open(&dir.path().join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(store.conversation(&child.id).unwrap().unwrap().contexts.len(), 1);
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
        assert_eq!(store.conversation_plan(&source.id).unwrap(), Some(plan.clone()));

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
        conn.execute_batch(remaining_schema).expect("other v1 tables");
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
        conn.pragma_update(None, "user_version", 1).expect("v1 version");
        drop(conn);

        let store = ConversationStore::open(&path).expect("upgrade");
        let loaded = store.conversation(&source.id).expect("read").expect("preserved row");
        assert_eq!(loaded.parent_conversation_id, None);
        assert_eq!(loaded, source);
        // Every table added after v1 exists after one open, not just the newest.
        assert!(store.pending_fork_starts().expect("fork intents").is_empty());
        assert_eq!(store.conversation_plan(&source.id).expect("plan"), None);
        let version: i32 = store.lock().expect("lock")
            .query_row("PRAGMA user_version", [], |row| row.get(0)).expect("version");
        assert_eq!(version, STORE_VERSION);
        assert!(std::fs::read_dir(dir.path()).expect("directory").all(|entry| {
            !entry.expect("entry").file_name().to_string_lossy().contains("quarantine-")
        }));
    }

    #[test]
    fn parent_conversation_id_round_trips_on_insert_and_update() {
        let (_dir, store) = temp_store();
        let mut source = conversation("child");
        source.parent_conversation_id = Some("parent".into());
        store.put_conversation("ws", &source).expect("insert");
        assert_eq!(store.conversation("child").expect("read").expect("child"), source);
        source.parent_conversation_id = Some("grandparent".into());
        store.put_conversation("ws", &source).expect("update");
        assert_eq!(store.conversation("child").expect("read").expect("child"), source);
        source.parent_conversation_id = None;
        store.put_conversation("ws", &source).expect("clear parent");
        assert_eq!(store.conversation("child").expect("read").expect("child"), source);
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
            store.conversation("child").expect("read").expect("child").parent_conversation_id,
            Some("grandparent".into())
        );
        store.delete_conversation("grandparent").expect("delete root");
        assert_eq!(
            store.conversation("child").expect("read").expect("child").parent_conversation_id,
            None
        );
    }

    #[test]
    fn settled_context_helpers_skip_streaming_and_branch_rows() {
        let (_dir, store) = temp_store();
        let mut source = conversation("conv_a");
        source.branches.push(ConversationBranch {
            id: "branch".into(),
            fork_context_id: "ctx_u".into(),
            active: false,
            contexts: vec![assistant("branch_a", "branch output", 1)],
            created_at: source.created_at.clone(),
            updated_at: source.updated_at.clone(),
        });
        store.put_conversation("ws", &source).expect("put");
        assert_eq!(store.last_settled_context_id("conv_a").expect("empty trunk"), None);
        assert!(store.settled_contexts("conv_a").expect("empty trunk").is_empty());
        let settled = vec![user("ctx_u", "hi"), assistant("ctx_a", "done", 1)];
        store.upsert_contexts("conv_a", &settled, ContextStatus::Settled).expect("settled");
        store.upsert_contexts(
            "conv_a", &[assistant("ctx_live", "partial", 2)], ContextStatus::Streaming,
        ).expect("streaming");
        assert_eq!(
            store.last_settled_context_id("conv_a").expect("boundary"),
            Some("ctx_a".into())
        );
        assert_eq!(store.settled_contexts("conv_a").expect("settled contexts"), settled);
        assert_eq!(store.conversation("conv_a").expect("read").expect("present").contexts.len(), 3);
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
        assert_eq!(buckets[1].hour_start_ms - buckets[0].hour_start_ms, 3_600_000);
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
        source.queued_messages = vec![QueuedMessage {
            id: "queued_1".into(),
            content: "later".into(),
            images: Vec::new(),
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

        let loaded = store.conversation("conv_a").expect("read").expect("present");
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

        let loaded = store.conversation("conv_a").expect("read").expect("present");
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
        assert_eq!(context_ids(&store, "conv_a"), vec!["ctx_u", "ctx_a", "ctx_r"]);
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
        let loaded = store.conversation("conv_a").expect("read").expect("present");
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
                &[reasoning("ctx_r"), reasoning("ctx_r_1"), reasoning("ctx_r_2")],
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
                &[user("ctx_u", "hi"), tool("ctx_tool_1"), assistant("ctx_a", "done", 2), user("ctx_u2", "next")],
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

        let loaded = store.conversation("conv_a").expect("read").expect("present");
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
        let loaded = store.conversation("conv_a").expect("read").expect("present");
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
                &[
                    reasoning("ctx_r"),
                    assistant("ctx_a", "half written", 1),
                ],
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
        let loaded = store.conversation("conv_a").expect("read").expect("present");
        assert_eq!(
            loaded.contexts.iter().map(ContextItem::id).collect::<Vec<_>>(),
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
            store.conversation("conv_a").expect("read").expect("present").contexts.len(),
            3
        );
        assert_eq!(
            store.discard_streaming_contexts("conv_a", &[]).expect("empty"),
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
        let snapshot = store.conversation("conv_a").expect("read").expect("present");
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
        edit.settings.system_prompt = "be brief".into();
        edit.queued_messages = vec![QueuedMessage {
            id: "queued_1".into(),
            content: "later".into(),
            images: Vec::new(),
            created_at: "2026-08-25T00:00:04.000Z".into(),
        }];
        // The edit also proposes a different timeline; that part must not land.
        edit.contexts = vec![user("ctx_u", "edited")];
        edit.branches.clear();
        store
            .put_conversation_metadata("ws", &edit)
            .expect("metadata write");

        let loaded = store.conversation("conv_a").expect("read").expect("present");
        assert_eq!(loaded.title, "renamed while running");
        assert_eq!(loaded.settings.system_prompt, "be brief");
        assert_eq!(loaded.queued_messages, edit.queued_messages);
        assert_eq!(
            loaded.contexts.iter().map(ContextItem::id).collect::<Vec<_>>(),
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
            .upsert_contexts("conv_missing", &[user("ctx_u", "hi")], ContextStatus::Settled)
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
            store.set_workspace_order(destination, &moving.iter().map(|id| id.to_string()).collect::<Vec<_>>()).unwrap();
            for (index, id) in ["p", "c", "g"].iter().enumerate() {
                let item = store.conversation(id).unwrap().unwrap();
                assert_eq!(item.parent_conversation_id.as_deref(), expected[index], "{destination}: {moving:?}, {id}");
            }
            let remaining = store.workspace_conversations("a").unwrap();
            assert_eq!(remaining.len(), if destination == "a" { 3 } else { 3 - moving.len() });
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

        let loaded = store.conversation("conv_a").expect("read").expect("present");
        assert_eq!(loaded.contexts.len(), 1);
        assert_eq!(loaded.branches[0].contexts.len(), 1);
        assert_eq!(loaded, source);
    }
}
