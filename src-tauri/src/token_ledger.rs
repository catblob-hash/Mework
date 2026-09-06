//! Token ledger for the built-in gateway.
//!
//! The gateway is the host's only provider egress: every model HTTP request passes
//! through `api::post_model_request_with_validator`. Each new caller must obtain a
//! [`GatewayRecorder`] before sending a request.
//!
//! One row represents one successfully parsed HTTP request, not a turn or UI round.
//! Request usage sums to turn usage without double-counting child requests; backfill
//! therefore records only parent turns (see [`TokenLedger::backfill`]).
//!
//! The ledger uses a separate database so conversation-store quarantine and rebuilds
//! cannot erase usage statistics.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::model::ModelUsage;

/// Database file name. Stored next to the anchor file.
pub const DATABASE_FILE_NAME: &str = "token-usage.v1.sqlite3";

/// `PRAGMA user_version`. Version mismatches quarantine and rebuild the database.
pub const STORE_VERSION: i32 = 1;

/// `ledger_meta` key for the start of live recording. Backfill accepts only earlier events.
const META_STARTED_AT: &str = "started_at_ms";

const SCHEMA_SQL: &str = r#"
CREATE TABLE usage_event (
    id                  TEXT PRIMARY KEY,
    occurred_at_ms      INTEGER NOT NULL,
    origin              TEXT NOT NULL,
    conversation_id     TEXT NOT NULL,
    workspace_id        TEXT NOT NULL,
    provider_id         TEXT NOT NULL,
    provider_name       TEXT NOT NULL,
    model_id            TEXT NOT NULL,
    requests            INTEGER NOT NULL,
    input_tokens        INTEGER NOT NULL,
    cached_input_tokens INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    total_tokens        INTEGER NOT NULL,
    CHECK (origin IN ('conversation', 'subagent', 'web_search'))
) STRICT;

CREATE INDEX usage_event_time_idx ON usage_event (occurred_at_ms);

CREATE TABLE ledger_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
"#;

/// Identifies who spent these tokens. Parent and child requests are distinct HTTP
/// requests, so this is classification only and does not participate in deduplication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOrigin {
    /// A top-level conversation turn.
    Conversation,
    /// A request owned by a subagent or workflow step.
    Subagent,
    /// A one-shot native search request dispatched by `web_search`.
    WebSearch,
}

impl UsageOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Subagent => "subagent",
            Self::WebSearch => "web_search",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "subagent" => Self::Subagent,
            "web_search" => Self::WebSearch,
            _ => Self::Conversation,
        }
    }
}

/// A usage record. `id` is the idempotency key: live gateway records use random IDs,
/// while backfill uses derivable IDs (`turn:<conversationId>:<turnId>`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub id: String,
    pub occurred_at_ms: i64,
    pub origin: UsageOrigin,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub model_id: String,
    /// Number of requests represented by a merged record. Live gateway records are
    /// always 1; backfill records one because turn-level data cannot recover the count.
    #[serde(default = "one")]
    pub requests: u32,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

fn one() -> u32 {
    1
}

/// Maximum events in one backfill request. The renderer batches too, but this is the host-side limit.
const MAX_BACKFILL_EVENTS: usize = 2_000;

/// Maximum length of identity fields. They are used only for grouping and display.
const MAX_IDENTITY_LEN: usize = 200;

/// SQLite integers are signed 64-bit. Saturate instead of casting so absurd values
/// cannot wrap negative and cancel valid records during aggregation.
fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn clamp_identity(value: &str) -> String {
    if value.len() <= MAX_IDENTITY_LEN {
        return value.to_owned();
    }
    value.chars().take(MAX_IDENTITY_LEN).collect()
}

impl UsageEvent {
    /// Creates a gateway record. If `total_tokens` is absent, use `input + output`
    /// so providers that omit totals do not leave the statistics card incomplete.
    pub fn from_usage(
        id: String,
        occurred_at_ms: i64,
        origin: UsageOrigin,
        usage: &ModelUsage,
    ) -> Self {
        // Cached input is a subset of input (`ModelUsage::cached_input_tokens`). If
        // input is absent, cached input is the best available lower bound.
        let input = usage
            .input_tokens
            .unwrap_or(0)
            .max(usage.cached_input_tokens.unwrap_or(0));
        let output = usage.output_tokens.unwrap_or(0);
        Self {
            id,
            occurred_at_ms,
            origin,
            conversation_id: String::new(),
            workspace_id: String::new(),
            provider_id: String::new(),
            provider_name: String::new(),
            model_id: String::new(),
            requests: 1,
            input_tokens: input,
            cached_input_tokens: usage.cached_input_tokens.unwrap_or(0),
            output_tokens: output,
            total_tokens: usage
                .total_tokens
                .unwrap_or_else(|| input.saturating_add(output)),
        }
    }

    fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.total_tokens == 0
    }

    /// Clamp renderer-supplied events. Gateway-created events are already within these bounds.
    fn clamped(&self) -> Self {
        Self {
            id: clamp_identity(&self.id),
            occurred_at_ms: self.occurred_at_ms,
            origin: self.origin,
            conversation_id: clamp_identity(&self.conversation_id),
            workspace_id: clamp_identity(&self.workspace_id),
            provider_id: clamp_identity(&self.provider_id),
            provider_name: clamp_identity(&self.provider_name),
            model_id: clamp_identity(&self.model_id),
            requests: self.requests.max(1),
            input_tokens: self.input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
        }
    }
}

/// One cell aggregated by UTC hour and model. The renderer converts it to the local
/// day and hour so changing time zones never requires recomputing the database.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub hour_start_ms: i64,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub origin: UsageOrigin,
    pub requests: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl Default for UsageOrigin {
    fn default() -> Self {
        Self::Conversation
    }
}

/// Process-local connections cached by database path. Callers only have anchor paths,
/// including temporary paths in tests, so identical paths must reuse one connection
/// to preserve WAL semantics.
fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<TokenLedger>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<TokenLedger>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Anchor file path to the ledger database path in the same directory.
pub fn database_path(anchor: &Path) -> PathBuf {
    anchor
        .parent()
        .map(|parent| parent.join(DATABASE_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(DATABASE_FILE_NAME))
}

/// Opens the ledger for an anchor file when necessary.
pub fn store_for(anchor: &Path) -> Result<Arc<TokenLedger>, String> {
    let path = database_path(anchor);
    let mut registry = registry()
        .lock()
        .map_err(|_| "流水账注册表已中毒".to_string())?;
    if let Some(existing) = registry.get(&path) {
        return Ok(Arc::clone(existing));
    }
    let store = Arc::new(TokenLedger::open(&path)?);
    registry.insert(path, Arc::clone(&store));
    Ok(store)
}

/// Closes and discards the connection for an anchor file. `reset:data` must call this
/// before deleting the containing directory.
pub fn close_store_for(anchor: &Path) {
    let path = database_path(anchor);
    if let Ok(mut registry) = registry().lock() {
        registry.remove(&path);
    }
}

pub struct TokenLedger {
    conn: Mutex<Connection>,
}

impl TokenLedger {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建流水账目录：{error}"))?;
        }
        let conn = open_configured(db_path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        match store.ensure_schema() {
            Ok(()) => Ok(store),
            Err(error) => {
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
        let conn = self.lock()?;
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("无法读取流水账版本：{error}"))?;
        if version == STORE_VERSION {
            return Ok(());
        }
        if version != 0 {
            return Err(format!(
                "流水账版本 {version} 与当前实现的 {STORE_VERSION} 不一致"
            ));
        }
        let has_tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'usage_event'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("无法检查流水账结构：{error}"))?;
        if has_tables > 0 {
            return Err("流水账缺少版本标记但已有数据表".into());
        }
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|error| format!("无法建立流水账结构：{error}"))?;
        conn.execute(
            "INSERT INTO ledger_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![META_STARTED_AT, now_ms().to_string()],
        )
        .map_err(|error| format!("无法写入流水账起点：{error}"))?;
        conn.pragma_update(None, "user_version", STORE_VERSION)
            .map_err(|error| format!("无法写入流水账版本：{error}"))?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "流水账连接已中毒".to_string())
    }

    /// The instant gateway recording began. Backfill accepts only older history so a
    /// request cannot be recorded both live and by turn-level backfill.
    pub fn started_at_ms(&self) -> Result<i64, String> {
        let conn = self.lock()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM ledger_meta WHERE key = ?1",
                [META_STARTED_AT],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法读取流水账起点：{error}"))?;
        Ok(raw.and_then(|value| value.parse().ok()).unwrap_or(0))
    }

    /// Backfill cutoff: the earlier of the recording start and the earliest existing
    /// live record. A rolled-back clock can otherwise let turn backfill duplicate a
    /// live request; live UUIDs and derivable `turn:…` IDs distinguish the records.
    fn backfill_cutoff(&self) -> Result<i64, String> {
        let started = self.started_at_ms()?;
        let conn = self.lock()?;
        let earliest_live: Option<i64> = conn
            .query_row(
                "SELECT min(occurred_at_ms) FROM usage_event WHERE id NOT LIKE 'turn:%'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("无法读取流水账最早实时行：{error}"))?
            .flatten();
        Ok(match earliest_live {
            Some(live) => started.min(live),
            None => started,
        })
    }

    /// Writes one record. Primary-key conflicts are ignored because IDs are idempotency keys.
    pub fn record(&self, event: &UsageEvent) -> Result<(), String> {
        if event.is_empty() {
            // Requests that report no usage do not affect totals and must not mark an
            // otherwise inactive hour as active.
            return Ok(());
        }
        let conn = self.lock()?;
        insert_event(&conn, event)?;
        Ok(())
    }

    /// Backfills history once. Events at or after the cutoff are dropped because the
    /// gateway already recorded that interval. Returns the number of inserted records.
    ///
    /// The renderer supplies this data, so bound both IPC payload and transaction size.
    pub fn backfill(&self, events: &[UsageEvent]) -> Result<usize, String> {
        if events.len() > MAX_BACKFILL_EVENTS {
            return Err(format!(
                "一次最多补 {MAX_BACKFILL_EVENTS} 条历史用量，收到 {}",
                events.len()
            ));
        }
        let cutoff = self.backfill_cutoff()?;
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开启流水账事务：{error}"))?;
        let mut written = 0usize;
        for event in events {
            if event.is_empty() || event.occurred_at_ms >= cutoff {
                continue;
            }
            written += insert_event(&tx, &event.clamped())? as usize;
        }
        tx.commit()
            .map_err(|error| format!("无法提交流水账事务：{error}"))?;
        Ok(written)
    }

    /// Aggregates all records by UTC hour, provider, model, and origin. The expected
    /// desktop data volume is small, so the UI can apply its All/30d/7d filters locally.
    pub fn buckets(&self) -> Result<Vec<UsageBucket>, String> {
        let conn = self.lock()?;
        let mut statement = conn
            .prepare(
                "SELECT occurred_at_ms / 3600000 * 3600000 AS hour_start,
                        provider_id,
                        provider_name,
                        model_id,
                        origin,
                        sum(requests),
                        sum(input_tokens),
                        sum(cached_input_tokens),
                        sum(output_tokens),
                        sum(total_tokens)
                 FROM usage_event
                 GROUP BY hour_start, provider_id, provider_name, model_id, origin
                 ORDER BY hour_start",
            )
            .map_err(|error| format!("无法准备流水账聚合：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(UsageBucket {
                    hour_start_ms: row.get(0)?,
                    provider_id: row.get(1)?,
                    provider_name: row.get(2)?,
                    model_id: row.get(3)?,
                    origin: UsageOrigin::parse(&row.get::<_, String>(4)?),
                    requests: row.get::<_, i64>(5)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                    cached_input_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                    total_tokens: row.get::<_, i64>(9)?.max(0) as u64,
                })
            })
            .map_err(|error| format!("无法读取流水账聚合：{error}"))?;
        let mut buckets = Vec::new();
        for row in rows {
            buckets.push(row.map_err(|error| format!("无法读取流水账聚合行：{error}"))?);
        }
        Ok(buckets)
    }
}

/// Complete identity for a gateway record. Construct it once and reuse it throughout
/// a run to avoid repeatedly parsing the same fields.
pub struct GatewayRecorder {
    ledger: Arc<TokenLedger>,
    origin: UsageOrigin,
    conversation_id: String,
    workspace_id: String,
    provider_id: String,
    provider_name: String,
    model_id: String,
}

impl GatewayRecorder {
    /// Returns `None` when `app_data_path` is empty, such as for a one-shot
    /// subagent request or a bare test `AppState`: accounting must never block a run.
    pub fn new(
        app_data_path: &str,
        origin: UsageOrigin,
        conversation_id: &str,
        workspace_id: &str,
        provider_id: &str,
        provider_name: &str,
        model_id: &str,
    ) -> Option<Self> {
        if app_data_path.is_empty() {
            return None;
        }
        let anchor = Path::new(app_data_path).join("document.v1.json");
        match store_for(&anchor) {
            Ok(ledger) => Some(Self {
                ledger,
                origin,
                conversation_id: conversation_id.to_owned(),
                workspace_id: workspace_id.to_owned(),
                provider_id: provider_id.to_owned(),
                provider_name: provider_name.to_owned(),
                model_id: model_id.to_owned(),
            }),
            Err(error) => {
                eprintln!("流水账不可用，本次运行的 token 用量不计入统计：{error}");
                None
            }
        }
    }

    /// Records one successful request. Failures are logged because accounting must
    /// never cause a model request to fail.
    pub fn record(&self, usage: &ModelUsage) {
        let mut event = UsageEvent::from_usage(
            uuid::Uuid::new_v4().to_string(),
            now_ms(),
            self.origin,
            usage,
        );
        event.conversation_id = self.conversation_id.clone();
        event.workspace_id = self.workspace_id.clone();
        event.provider_id = self.provider_id.clone();
        event.provider_name = self.provider_name.clone();
        event.model_id = self.model_id.clone();
        if let Err(error) = self.ledger.record(&event) {
            eprintln!("token 用量入账失败：{error}");
        }
    }
}

fn insert_event(conn: &Connection, event: &UsageEvent) -> Result<u32, String> {
    let changed = conn
        .execute(
            "INSERT OR IGNORE INTO usage_event (
                 id, occurred_at_ms, origin, conversation_id, workspace_id,
                 provider_id, provider_name, model_id, requests,
                 input_tokens, cached_input_tokens, output_tokens, total_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                event.id,
                event.occurred_at_ms,
                event.origin.as_str(),
                event.conversation_id,
                event.workspace_id,
                event.provider_id,
                event.provider_name,
                event.model_id,
                i64::from(event.requests),
                saturating_i64(event.input_tokens),
                saturating_i64(event.cached_input_tokens),
                saturating_i64(event.output_tokens),
                saturating_i64(event.total_tokens),
            ],
        )
        .map_err(|error| format!("无法写入 token 用量：{error}"))?;
    Ok(changed as u32)
}

fn open_configured(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|error| format!("无法打开流水账：{error}"))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| format!("无法启用 WAL：{error}"))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| format!("无法设置 synchronous：{error}"))?;
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
    eprintln!("流水账已封存（{reason}）：{}", quarantined.display());
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, cached: u64, output: u64, total: Option<u64>) -> ModelUsage {
        ModelUsage {
            input_tokens: Some(input),
            cached_input_tokens: Some(cached),
            output_tokens: Some(output),
            total_tokens: total,
            reasoning_tokens: None,
        }
    }

    fn event(id: &str, at: i64, tokens: u64) -> UsageEvent {
        UsageEvent {
            id: id.into(),
            occurred_at_ms: at,
            origin: UsageOrigin::Conversation,
            conversation_id: "c1".into(),
            workspace_id: "w1".into(),
            provider_id: "p1".into(),
            provider_name: "Provider".into(),
            model_id: "m1".into(),
            requests: 1,
            input_tokens: tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: tokens,
        }
    }

    fn ledger() -> (tempfile::TempDir, TokenLedger) {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = TokenLedger::open(&dir.path().join(DATABASE_FILE_NAME)).expect("ledger");
        (dir, ledger)
    }

    #[test]
    fn missing_total_falls_back_to_input_plus_output() {
        let event = UsageEvent::from_usage("e1".into(), 0, UsageOrigin::Conversation, &usage(10, 4, 6, None));
        assert_eq!(event.total_tokens, 16);
        assert_eq!(event.cached_input_tokens, 4);
    }

    #[test]
    fn reported_total_wins_over_the_fallback() {
        let event =
            UsageEvent::from_usage("e1".into(), 0, UsageOrigin::Conversation, &usage(10, 0, 6, Some(21)));
        assert_eq!(event.total_tokens, 21);
    }

    #[test]
    fn a_request_that_reported_nothing_takes_no_row() {
        let (_dir, ledger) = ledger();
        ledger
            .record(&UsageEvent::from_usage(
                "e1".into(),
                0,
                UsageOrigin::Conversation,
                &ModelUsage::default(),
            ))
            .expect("record");
        assert!(ledger.buckets().expect("buckets").is_empty());
    }

    #[test]
    fn duplicate_ids_are_idempotent() {
        let (_dir, ledger) = ledger();
        ledger.record(&event("turn:c1:t1", 3_600_000, 100)).expect("first");
        ledger.record(&event("turn:c1:t1", 3_600_000, 100)).expect("second");
        let buckets = ledger.buckets().expect("buckets");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].total_tokens, 100);
        assert_eq!(buckets[0].requests, 1);
    }

    #[test]
    fn buckets_collapse_to_the_utc_hour() {
        let (_dir, ledger) = ledger();
        // Two requests share one UTC hour; the third falls in the next hour.
        ledger.record(&event("a", 3_600_000, 10)).expect("a");
        ledger.record(&event("b", 3_600_000 + 59_000, 20)).expect("b");
        ledger.record(&event("c", 7_200_000, 5)).expect("c");
        let buckets = ledger.buckets().expect("buckets");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].hour_start_ms, 3_600_000);
        assert_eq!(buckets[0].total_tokens, 30);
        assert_eq!(buckets[0].requests, 2);
        assert_eq!(buckets[1].hour_start_ms, 7_200_000);
        assert_eq!(buckets[1].total_tokens, 5);
    }

    #[test]
    fn backfill_refuses_anything_the_gateway_already_saw() {
        let (_dir, ledger) = ledger();
        let cutoff = ledger.started_at_ms().expect("cutoff");
        assert!(cutoff > 0);
        let written = ledger
            .backfill(&[
                event("old", cutoff - 10_000, 7),
                event("boundary", cutoff, 9),
                event("new", cutoff + 10_000, 11),
            ])
            .expect("backfill");
        assert_eq!(written, 1);
        let buckets = ledger.buckets().expect("buckets");
        assert_eq!(buckets.iter().map(|bucket| bucket.total_tokens).sum::<u64>(), 7);
    }

    #[test]
    fn a_cached_only_report_still_counts() {
        // Cached input is a subset of input, so it is the input lower bound when
        // the provider omits input tokens.
        let event = UsageEvent::from_usage(
            "e1".into(),
            0,
            UsageOrigin::Conversation,
            &ModelUsage {
                input_tokens: None,
                cached_input_tokens: Some(100),
                output_tokens: None,
                total_tokens: None,
                reasoning_tokens: None,
            },
        );
        assert_eq!(event.input_tokens, 100);
        assert_eq!(event.total_tokens, 100);
    }

    #[test]
    fn absurd_token_counts_saturate_instead_of_wrapping_negative() {
        let (_dir, ledger) = ledger();
        let cutoff = ledger.started_at_ms().expect("cutoff");
        let mut absurd = event("turn:c1:huge", cutoff - 1_000, 0);
        absurd.input_tokens = u64::MAX;
        absurd.total_tokens = u64::MAX;
        assert_eq!(ledger.backfill(&[absurd]).expect("backfill"), 1);
        let buckets = ledger.buckets().expect("buckets");
        // `as i64` would wrap this to `i64::MIN`, then `.max(0)` would erase the
        // record and let it cancel valid usage during aggregation.
        assert_eq!(buckets[0].total_tokens, i64::MAX as u64);
    }

    #[test]
    fn one_call_cannot_carry_an_unbounded_history() {
        let (_dir, ledger) = ledger();
        let cutoff = ledger.started_at_ms().expect("cutoff");
        let too_many = (0..(MAX_BACKFILL_EVENTS + 1))
            .map(|index| event(&format!("turn:c1:{index}"), cutoff - 1_000, 1))
            .collect::<Vec<_>>();
        assert!(ledger.backfill(&too_many).is_err());
        assert!(ledger.buckets().expect("buckets").is_empty());
    }

    #[test]
    fn a_rolled_back_clock_cannot_make_a_live_request_be_backfilled_too() {
        let (_dir, ledger) = ledger();
        let started = ledger.started_at_ms().expect("cutoff");
        // After a clock rollback, a live gateway record predates the recording start.
        let mut live = event("live-uuid", started - 60_000, 500);
        live.id = "3f7c0a11-live".into();
        ledger.record(&live).expect("live");
        // Reject the matching turn record: using only `started_at_ms` would classify
        // it as history and record the same request twice.
        let written = ledger
            .backfill(&[event("turn:c1:same-request", started - 30_000, 500)])
            .expect("backfill");
        assert_eq!(written, 0);
        assert_eq!(
            ledger
                .buckets()
                .expect("buckets")
                .iter()
                .map(|bucket| bucket.total_tokens)
                .sum::<u64>(),
            500
        );
    }

    #[test]
    fn origins_stay_separate_in_the_same_hour() {
        let (_dir, ledger) = ledger();
        let mut child = event("child", 3_600_000, 40);
        child.origin = UsageOrigin::Subagent;
        ledger.record(&event("parent", 3_600_000, 60)).expect("parent");
        ledger.record(&child).expect("child");
        let buckets = ledger.buckets().expect("buckets");
        assert_eq!(buckets.len(), 2);
        assert_eq!(
            buckets.iter().map(|bucket| bucket.total_tokens).sum::<u64>(),
            100
        );
    }
}
