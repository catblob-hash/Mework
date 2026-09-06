//! Pure merge ledger for workflow progress cards.
//!
//! # Row identity and order
//!
//! Agent rows are identified by `(kind, index)`. A step transitions from `start`
//! through `progress` to `done` by replacing its original row, so completion timing
//! never changes display order. Log rows have no mergeable identity; each
//! `append_log` call adds a new narrative. Both row types intentionally share an
//! ordered slice, so the S15 driver needs no additional sorting or joining.
//!
//! # Bounded retention
//!
//! `preview` is truncated by Unicode code point on entry rather than UTF-8 byte,
//! giving CJK text the same display budget as ASCII and avoiding split multibyte
//! characters. `message` is limited by row count instead, retaining complete
//! diagnostics rather than arbitrary prefixes.
//!
//! Crossing [`crate::MAX_PROGRESS_ROWS`] trims back to
//! [`crate::PROGRESS_TRIM_TARGET`]: only the oldest Log rows are evicted; Agent rows
//! are never evicted. This event-driven window can grow from 500 back to 1000 before
//! trimming again, keeping recent narrative rows visible between trims. If Agent rows
//! already exceed the target, all evictable Logs are removed and trimming stops; the
//! actual Agent limit is [`crate::MAX_LIFETIME_STEPS`] at the run layer and must not be
//! silently duplicated in the presentation layer.
//!
//! # Skipped sentinel
//!
//! A user-skipped step is encoded as `state = error` plus `skipped = true`, rather
//! than adding a fifth state or setting only a flag. This preserves the reference
//! phase aggregation: the row first counts toward Error, then the sentinel identifies
//! the reason. A flag-only encoding would diverge from its Error count.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Lifecycle state of a progress-card step.
///
/// `Error` represents both actual failures and user skips; inspect
/// [`ProgressRow::skipped`] for the latter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStepState {
    Start,
    Progress,
    Done,
    Error,
}

/// Purpose of a progress-card row.
///
/// Agent rows are mergeable, while Log rows only append; callers must not present a
/// Log row as an upsertable step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressRowKind {
    Agent,
    Log,
}

/// A display record that can be passed directly to the S17 progress card.
///
/// Optional fields omit `null` during serialization to avoid meaningless placeholders
/// in frequent progress events. Missing boolean sentinels deserialize as `false` for
/// compatibility with events produced before those markers existed.
///
/// `Eq` is required because the host's `ModelStreamEvent` carries this row. All fields
/// are strings, integers, or booleans, so equality is structural.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressRow {
    pub kind: ProgressRowKind,
    pub index: usize,
    pub state: WorkflowStepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub skipped: bool,
}

impl ProgressRow {
    /// Minimal Agent-row constructor: Start state with no annotations.
    pub fn agent(index: usize) -> Self {
        Self {
            kind: ProgressRowKind::Agent,
            index,
            state: WorkflowStepState::Start,
            label: None,
            phase: None,
            phase_index: None,
            preview: None,
            message: None,
            cached: false,
            blocked: false,
            skipped: false,
        }
    }
}

/// Ordered progress ledger for one workflow run.
///
/// `next_log_index` provides narrative rows with stable, non-reused display identity;
/// Log rows do not participate in upserts, so the index has no merge semantics.
#[derive(Clone, Debug, Default)]
pub struct ProgressLedger {
    rows: Vec<ProgressRow>,
    next_log_index: usize,
}

impl ProgressLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges an Agent row by `(kind, index)`.
    ///
    /// Replaces the existing slot or appends a first occurrence, preserving the step's
    /// initial position. Log rows must use [`Self::append_log`]; rejecting them rather
    /// than appending silently exposes misuse through the return value and leaves the
    /// ledger unchanged.
    pub fn upsert(&mut self, mut row: ProgressRow) -> bool {
        if row.kind == ProgressRowKind::Log {
            return false;
        }
        normalize_preview(&mut row);
        if let Some(existing) = self
            .rows
            .iter_mut()
            .find(|existing| existing.kind == row.kind && existing.index == row.index)
        {
            *existing = row;
        } else {
            self.rows.push(row);
        }
        self.enforce_progress_bound();
        true
    }

    /// Appends a complete narrative, evicting the oldest Log rows beyond
    /// [`crate::MAX_LOG_MESSAGES`].
    ///
    /// Enforce the Log-specific count before the total row count, so a log-only ledger
    /// retains its latest 1000 rows without the 1001st temporarily triggering the
    /// cross-type 1000-to-500 compaction policy.
    ///
    /// Returns the appended row with its ledger-assigned monotonic index. The emitter
    /// must send this exact row: reconstructing it would diverge in index and break
    /// the renderer's idempotent merge.
    pub fn append_log(&mut self, message: &str) -> ProgressRow {
        let index = self.next_log_index;
        self.next_log_index = self.next_log_index.saturating_add(1);
        let row = ProgressRow {
            kind: ProgressRowKind::Log,
            index,
            state: WorkflowStepState::Progress,
            label: None,
            phase: None,
            phase_index: None,
            preview: None,
            message: Some(message.to_owned()),
            cached: false,
            blocked: false,
            skipped: false,
        };
        self.rows.push(row.clone());

        let log_count = self
            .rows
            .iter()
            .filter(|row| row.kind == ProgressRowKind::Log)
            .count();
        self.evict_oldest_logs(log_count.saturating_sub(crate::MAX_LOG_MESSAGES));
        self.enforce_progress_bound();
        row
    }

    /// Marks an existing Agent step as skipped by the user.
    ///
    /// Changes only the two aggregation fields. `cached` and `blocked` are orthogonal
    /// facts and must remain visible; missing indices are idempotent.
    pub fn mark_skipped(&mut self, index: usize) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.kind == ProgressRowKind::Agent && row.index == index)
        {
            row.state = WorkflowStepState::Error;
            row.skipped = true;
        }
    }

    pub fn rows(&self) -> &[ProgressRow] {
        &self.rows
    }

    fn enforce_progress_bound(&mut self) {
        // Event-driven rather than sticky: trim to 500 only when the current count
        // exceeds 1000, then allow it to grow again. Sticky compaction would evict
        // every new narrative row once Agent rows exceed the target.
        if self.rows.len() > crate::MAX_PROGRESS_ROWS {
            let excess = self.rows.len().saturating_sub(crate::PROGRESS_TRIM_TARGET);
            self.evict_oldest_logs(excess);
        }
    }

    fn evict_oldest_logs(&mut self, count: usize) {
        let mut remaining = count;
        self.rows.retain(|row| {
            if remaining > 0 && row.kind == ProgressRowKind::Log {
                remaining -= 1;
                false
            } else {
                true
            }
        });
    }
}

fn normalize_preview(row: &mut ProgressRow) {
    let Some(preview) = row.preview.as_ref() else {
        return;
    };
    if preview.chars().count() <= crate::MAX_PREVIEW_CHARS {
        return;
    }
    let mut trimmed: String = preview
        .chars()
        .take(crate::MAX_PREVIEW_CHARS.saturating_sub(1))
        .collect();
    if crate::MAX_PREVIEW_CHARS > 0 {
        trimmed.push('…');
    }
    row.preview = Some(trimmed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log_messages(ledger: &ProgressLedger) -> Vec<&str> {
        ledger
            .rows()
            .iter()
            .filter(|row| row.kind == ProgressRowKind::Log)
            .map(|row| row.message.as_deref().expect("log has a message"))
            .collect()
    }

    #[test]
    fn an_upserted_step_keeps_one_row_and_its_first_position_through_the_lifecycle() {
        let mut ledger = ProgressLedger::new();
        assert!(ledger.upsert(ProgressRow::agent(7)));
        assert!(ledger.upsert(ProgressRow::agent(8)));

        let mut progress = ProgressRow::agent(7);
        progress.state = WorkflowStepState::Progress;
        progress.preview = Some("working".into());
        assert!(ledger.upsert(progress));
        assert!(ledger.upsert(ProgressRow::agent(9)));

        let mut done = ProgressRow::agent(7);
        done.state = WorkflowStepState::Done;
        done.preview = Some("finished".into());
        assert!(ledger.upsert(done));

        assert_eq!(ledger.rows().len(), 3);
        assert_eq!(ledger.rows()[0].index, 7);
        assert_eq!(ledger.rows()[0].state, WorkflowStepState::Done);
        assert_eq!(ledger.rows()[0].preview.as_deref(), Some("finished"));
        assert_eq!(
            ledger.rows().iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
    }

    #[test]
    fn every_appended_log_is_new_and_only_the_latest_message_window_is_retained() {
        let mut ledger = ProgressLedger::new();
        for index in 0..(crate::MAX_LOG_MESSAGES + 50) {
            ledger.append_log(&format!("log-{index}"));
        }

        let messages = log_messages(&ledger);
        assert_eq!(messages.len(), crate::MAX_LOG_MESSAGES);
        assert_eq!(messages.first().copied(), Some("log-50"));
        assert_eq!(
            messages.last().copied(),
            Some(format!("log-{}", crate::MAX_LOG_MESSAGES + 49).as_str())
        );
        assert_eq!(
            ledger.rows().first().map(|row| row.index),
            Some(50),
            "被驱逐的 Log 索引不得复用"
        );
    }

    #[test]
    fn progress_trimming_evicts_only_old_logs_even_when_agents_exceed_the_target() {
        // 600 Agent rows plus 600 appended Logs: the 401st Log reaches 1001
        // total rows and triggers a 1000-to-500 trim. Agent rows cannot be evicted,
        // so all 401 evictable Logs are removed. The non-sticky window then regrows,
        // retaining the remaining 199 Logs.
        let mut agent_heavy = ProgressLedger::new();
        for index in 0..600 {
            agent_heavy.upsert(ProgressRow::agent(index));
        }
        for index in 0..600 {
            agent_heavy.append_log(&format!("agent-heavy-{index}"));
        }
        let agents: Vec<_> = agent_heavy
            .rows()
            .iter()
            .filter(|row| row.kind == ProgressRowKind::Agent)
            .collect();
        assert_eq!(agents.len(), 600);
        assert!(agents.iter().enumerate().all(|(index, row)| row.index == index));
        let messages = log_messages(&agent_heavy);
        assert_eq!(messages.len(), 199, "裁剪后的窗口允许重新增长");
        assert_eq!(messages.first().copied(), Some("agent-heavy-401"));
        assert_eq!(messages.last().copied(), Some("agent-heavy-599"));
        assert_eq!(agent_heavy.rows().len(), 799);

        // 300 Agent rows plus 900 Logs: the 701st Log triggers trimming, evicting
        // the oldest 501 Logs and leaving 500 rows (300 Agent plus 200 Log). The
        // subsequent 199 Logs continue to accumulate.
        let mut mixed = ProgressLedger::new();
        for index in 0..300 {
            mixed.upsert(ProgressRow::agent(index));
        }
        for index in 0..900 {
            mixed.append_log(&format!("mixed-{index}"));
        }
        let agents: Vec<_> = mixed
            .rows()
            .iter()
            .filter(|row| row.kind == ProgressRowKind::Agent)
            .collect();
        assert_eq!(agents.len(), 300);
        assert!(agents.iter().enumerate().all(|(index, row)| row.index == index));
        let messages = log_messages(&mixed);
        assert_eq!(messages.len(), 399);
        assert_eq!(messages.first().copied(), Some("mixed-501"));
        assert_eq!(messages.last().copied(), Some("mixed-899"));
        assert_eq!(mixed.rows().len(), 699);
    }

    #[test]
    fn marking_a_step_skipped_preserves_orthogonal_flags_and_ignores_missing_indices() {
        let mut ledger = ProgressLedger::new();
        let mut row = ProgressRow::agent(4);
        row.state = WorkflowStepState::Progress;
        row.cached = true;
        row.blocked = true;
        ledger.upsert(row);

        ledger.mark_skipped(4);
        assert_eq!(ledger.rows()[0].state, WorkflowStepState::Error);
        assert!(ledger.rows()[0].skipped);
        assert!(ledger.rows()[0].cached);
        assert!(ledger.rows()[0].blocked);

        let before = ledger.rows().to_vec();
        ledger.mark_skipped(999);
        assert_eq!(ledger.rows(), before);
    }

    #[test]
    fn preview_trimming_counts_unicode_code_points_and_preserves_exact_limits() {
        let mut ledger = ProgressLedger::new();
        let mut over = ProgressRow::agent(0);
        over.preview = Some("汉".repeat(401));
        ledger.upsert(over);
        let trimmed = ledger.rows()[0].preview.as_deref().expect("preview");
        assert_eq!(trimmed.chars().count(), 400);
        assert_eq!(trimmed.chars().filter(|ch| *ch == '汉').count(), 399);
        assert!(trimmed.ends_with('…'));

        let exact = "字".repeat(400);
        let mut exact_row = ProgressRow::agent(1);
        exact_row.preview = Some(exact.clone());
        ledger.upsert(exact_row);
        assert_eq!(ledger.rows()[1].preview.as_deref(), Some(exact.as_str()));

        let mixed = format!("{}{}", "a".repeat(200), "中".repeat(201));
        let mut mixed_row = ProgressRow::agent(2);
        mixed_row.preview = Some(mixed);
        ledger.upsert(mixed_row);
        let trimmed = ledger.rows()[2].preview.as_deref().expect("mixed preview");
        assert_eq!(trimmed.chars().count(), 400);
        assert_eq!(trimmed.chars().take(200).collect::<String>(), "a".repeat(200));
        assert_eq!(trimmed.chars().filter(|ch| *ch == '中').count(), 199);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn serde_uses_lowercase_states_camel_case_fields_and_false_boolean_defaults() {
        let mut row = ProgressRow::agent(3);
        row.state = WorkflowStepState::Error;
        row.phase_index = Some(2);
        let encoded = serde_json::to_value(&row).expect("row serializes");
        assert_eq!(encoded["kind"], json!("agent"));
        assert_eq!(encoded["state"], json!("error"));
        assert_eq!(encoded["phaseIndex"], json!(2));

        let decoded: ProgressRow = serde_json::from_value(json!({
            "kind": "agent",
            "index": 3,
            "state": "start",
            "phaseIndex": 2
        }))
        .expect("defaults deserialize");
        assert!(!decoded.cached);
        assert!(!decoded.blocked);
        assert!(!decoded.skipped);
    }

    #[test]
    fn a_log_row_passed_to_upsert_is_rejected_without_changing_the_ledger() {
        let mut ledger = ProgressLedger::new();
        ledger.upsert(ProgressRow::agent(1));
        let before = ledger.rows().to_vec();
        let log = ProgressRow {
            kind: ProgressRowKind::Log,
            index: 42,
            state: WorkflowStepState::Progress,
            label: None,
            phase: None,
            phase_index: None,
            preview: Some("must not be normalized".into()),
            message: Some("must not be appended".into()),
            cached: false,
            blocked: false,
            skipped: false,
        };

        assert!(!ledger.upsert(log));
        assert_eq!(ledger.rows(), before);
    }

    #[test]
    fn a_row_with_all_optional_fields_none_omits_every_optional_json_key() {
        let encoded = serde_json::to_value(ProgressRow::agent(0)).expect("row serializes");
        let object = encoded.as_object().expect("row serializes as an object");

        for key in ["label", "phase", "phaseIndex", "preview", "message"] {
            assert!(!object.contains_key(key), "optional key {key} must be omitted");
        }
    }

    #[test]
    fn progress_and_done_states_serialize_to_their_lowercase_line_names() {
        assert_eq!(
            serde_json::to_value(WorkflowStepState::Progress).expect("progress serializes"),
            json!("progress")
        );
        assert_eq!(
            serde_json::to_value(WorkflowStepState::Done).expect("done serializes"),
            json!("done")
        );
    }

    #[test]
    fn append_log_preserves_a_ten_thousand_character_message_without_trimming() {
        let mut ledger = ProgressLedger::new();
        let message = "x".repeat(10_000);

        ledger.append_log(&message);

        let stored = ledger.rows()[0].message.as_deref().expect("log message");
        assert_eq!(stored, message);
        assert_eq!(stored.chars().count(), 10_000);
    }

    #[test]
    fn appending_the_same_log_text_repeatedly_creates_distinct_rows() {
        let mut ledger = ProgressLedger::new();

        ledger.append_log("same text");
        ledger.append_log("same text");
        ledger.append_log("same text");

        assert_eq!(ledger.rows().len(), 3);
        assert_eq!(log_messages(&ledger), vec!["same text", "same text", "same text"]);
        assert_eq!(
            ledger.rows().iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn marking_an_agent_skipped_does_not_touch_a_log_with_the_same_index() {
        let mut ledger = ProgressLedger::new();
        ledger.append_log("index collision");
        ledger.upsert(ProgressRow::agent(0));
        let log_before = ledger.rows()[0].clone();

        ledger.mark_skipped(0);

        assert_eq!(ledger.rows()[0], log_before);
        assert_eq!(ledger.rows()[0].kind, ProgressRowKind::Log);
        assert_eq!(ledger.rows()[0].state, WorkflowStepState::Progress);
        assert!(!ledger.rows()[0].skipped);
        assert_eq!(ledger.rows()[1].kind, ProgressRowKind::Agent);
        assert_eq!(ledger.rows()[1].state, WorkflowStepState::Error);
        assert!(ledger.rows()[1].skipped);
    }

    #[test]
    fn preview_trimming_counts_combining_marks_as_separate_code_points() {
        let mut ledger = ProgressLedger::new();
        let mut row = ProgressRow::agent(0);
        row.preview = Some("e\u{301}".repeat(400));

        ledger.upsert(row);

        let preview = ledger.rows()[0].preview.as_deref().expect("trimmed preview");
        assert_eq!(preview.chars().count(), 400);
        assert_eq!(preview, format!("{}e…", "e\u{301}".repeat(199)));
        assert!(preview.ends_with("e…"), "the final grapheme cluster is split");
    }

    #[test]
    fn interleaved_agents_and_logs_keep_insertion_order_when_an_agent_is_updated() {
        let mut ledger = ProgressLedger::new();
        ledger.upsert(ProgressRow::agent(0));
        ledger.append_log("first log");
        ledger.upsert(ProgressRow::agent(1));
        ledger.append_log("second log");

        let mut updated = ProgressRow::agent(0);
        updated.state = WorkflowStepState::Done;
        updated.preview = Some("updated in place".into());
        ledger.upsert(updated);

        assert_eq!(
            ledger
                .rows()
                .iter()
                .map(|row| (row.kind, row.index))
                .collect::<Vec<_>>(),
            vec![
                (ProgressRowKind::Agent, 0),
                (ProgressRowKind::Log, 0),
                (ProgressRowKind::Agent, 1),
                (ProgressRowKind::Log, 1),
            ]
        );
        assert_eq!(ledger.rows()[0].state, WorkflowStepState::Done);
        assert_eq!(ledger.rows()[0].preview.as_deref(), Some("updated in place"));
        assert_eq!(ledger.rows()[1].message.as_deref(), Some("first log"));
        assert_eq!(ledger.rows()[3].message.as_deref(), Some("second log"));
    }
}
