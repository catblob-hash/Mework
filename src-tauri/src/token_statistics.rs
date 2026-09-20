//! Assembles statistics-card data by combining conversation activity with gateway
//! token-ledger usage. The renderer filters the payload for All / 30d / 7d.
//!
//! Sources remain separate:
//!
//! * **Activity** (message and session counts) is computed from conversation
//!   data on every query, so deleting a conversation removes its messages.
//! * **Token usage** requires a ledger because body rows do not contain usage;
//!   a response reports its token cost only once.
//!
//! Both sources use UTC-hour buckets. The renderer converts local days, hours,
//! streaks, and peak periods because time zone belongs to the user.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::conversation_store::{self, ActivityBucket};
use crate::token_ledger::{self, UsageBucket, UsageEvent};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatistics {
    /// Assembly time. The renderer uses this to calculate today from one clock.
    pub generated_at_ms: i64,
    /// Ledger start time, used by the renderer to decide whether historical backfill is needed.
    pub ledger_started_at_ms: i64,
    pub activity: Vec<ActivityBucket>,
    pub usage: Vec<UsageBucket>,
}

/// Collects statistics. If either source is unavailable, leave only that half
/// empty; the statistics page must remain available.
pub fn collect(anchor: &Path) -> UsageStatistics {
    let mut statistics = UsageStatistics {
        generated_at_ms: chrono::Utc::now().timestamp_millis(),
        ..UsageStatistics::default()
    };
    match conversation_store::store_for(anchor).and_then(|store| store.activity_buckets()) {
        Ok(activity) => statistics.activity = activity,
        Err(error) => eprintln!("对话活动统计不可用：{error}"),
    }
    match token_ledger::store_for(anchor) {
        Ok(ledger) => {
            statistics.ledger_started_at_ms = ledger.started_at_ms().unwrap_or(0);
            match ledger.buckets() {
                Ok(usage) => statistics.usage = usage,
                Err(error) => eprintln!("token 用量统计不可用：{error}"),
            }
        }
        Err(error) => eprintln!("token 流水账不可用：{error}"),
    }
    statistics
}

/// Backfills historical usage once. The caller derives idempotency keys as
/// `turn:<conversationId>:<turnId>`, preventing duplicate counts; events after
/// ledger start are discarded because the gateway already recorded them.
pub fn backfill(anchor: &Path, events: &[UsageEvent]) -> Result<usize, String> {
    token_ledger::store_for(anchor)?.backfill(events)
}
