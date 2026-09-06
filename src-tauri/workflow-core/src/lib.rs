//! Engine-independent workflow decision boundary.
//!
//! This crate contains workflow decisions for cache-key chains ([`chain`]),
//! boundary cloning ([`boundary`]), run budgets ([`budget`]), progress merging
//! ([`progress`]), and structured output ([`schema`]). Script engines and host
//! runtimes supply values through [`StepSource`].
//!
//! Dependencies are intentionally limited to serde, serde_json, and sha2. Adding
//! an engine dependency here would couple decision logic back to that engine.

pub mod boundary;
pub mod budget;
pub mod chain;
pub mod progress;
pub mod schema;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export the schema limits so every named limit is available from the crate
// root without introducing a second definition.
pub use schema::{MAX_SCHEMA_DEPTH, MAX_SCHEMA_NODES};

/// Maximum script size shared by the `script` parameter and persisted `script.js`.
pub const MAX_SCRIPT_BYTES: usize = 524_288;

/// Maximum number of steps dispatched during a run's lifetime.
///
/// This guards against runaway loops; pool admission controls concurrency.
pub const MAX_LIFETIME_STEPS: usize = 1_000;

/// Maximum elements in one array crossing a boundary: script/host, plan items, or JSON parameters.
///
/// Exceeding it is an explicit error, never silent truncation.
pub const MAX_BOUNDARY_ITEMS: usize = 4_096;

/// Number of retries allowed after structured-output validation fails.
///
/// Validation plus this retry/prompt loop is the only structured-output enforcement.
pub const MAX_STRUCTURED_OUTPUT_RETRIES: usize = 5;

/// Maximum progress-ledger rows before trimming.
pub const MAX_PROGRESS_ROWS: usize = 1_000;

/// Target row count after trimming; agent rows are never evicted.
pub const PROGRESS_TRIM_TARGET: usize = 500;

/// Maximum code points in a progress-result preview.
pub const MAX_PREVIEW_CHARS: usize = 400;

/// Maximum number of `log()` narrative lines.
pub const MAX_LOG_MESSAGES: usize = 1_000;

/// Cache-key prefix.
///
/// The `mw1` prefix identifies this project's own key construction and prevents
/// cross-project replay of logs with incompatible key shapes.
pub const CACHE_KEY_PREFIX: &str = "mw1";

/// Milliseconds of inactivity before one step is stalled.
pub const WORKFLOW_STEP_STALL_MS: u64 = 180_000;

/// Retry count allowed after a step stalls.
///
/// This excludes the initial attempt, so five retries produce six total attempts.
pub const WORKFLOW_MAX_STALL_RETRIES: u32 = 5;

/// Request for one subagent step, produced by [`StepSource`] and run by the driver.
///
/// Only `prompt` and schema/model/effort/agent-type/isolation participate in the
/// cache key. Display and scheduling fields must not affect it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Whether this step needs its own isolated workspace. The only permitted
    /// value is `"worktree"`; the host creates a Git worktree checked out from HEAD.
    ///
    /// Isolation participates in the cache key because it changes the visible
    /// workspace. When absent, `canonical_opts` omits it to preserve existing
    /// journal keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_ms: Option<u64>,
}

impl WorkflowStepRequest {
    /// Minimal prompt-only request for tests and plan assembly.
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            label: None,
            phase: None,
            phase_index: None,
            schema: None,
            model: None,
            effort: None,
            agent_type: None,
            isolation: None,
            stall_ms: None,
        }
    }
}

/// Host-computed role requirements passed to the script engine.
///
/// Shared definitions keep host generation and `agent()` enforcement consistent.
/// This is run environment, not step input, and therefore does not enter cache keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepRolePolicy {
    /// Whether every `agent()` call must include `agentType`.
    pub required: bool,
    /// Valid role names. `None` disables name validation when the host cannot read
    /// the set; one I/O failure must not reject a valid script.
    pub known_names: Option<Vec<String>>,
}

impl StepRolePolicy {
    /// Permissive default for fallback mode and non-host callers.
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Tests whether a role name is available for this run. An unknown set permits
    /// all names and leaves final resolution to `resolve_agent_definition`.
    pub fn accepts(&self, name: &str) -> bool {
        match &self.known_names {
            None => true,
            Some(names) => names.iter().any(|known| known == name),
        }
    }
}

/// Completed result of one step.
///
/// `value == None` represents a skipped or terminally failed step. It records
/// only `started` and never enters the cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcome {
    /// Global step index in dispatch order. It matches the flattened
    /// [`StepProgress::Run`] vectors and advances the cache-key chain.
    pub index: usize,
    /// `Some` on success; `None` after a skip or terminal error.
    pub value: Option<Value>,
    /// Whether the result came from journal replay instead of execution.
    pub cached: bool,
    /// Terminal-error message, if any.
    pub error: Option<String>,
    /// Total tokens consumed by this step. Replayed steps have `None`, preventing
    /// duplicate budget charges after recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// Result of [`StepSource::advance`].
#[derive(Clone, Debug, PartialEq)]
pub enum StepProgress {
    /// Dispatch these new steps immediately.
    ///
    /// An empty vector means wait for in-flight work. If no work is in flight,
    /// the driver terminates with [`WorkflowError::Deadlock`].
    Run(Vec<WorkflowStepRequest>),
    /// The run completed with this final value.
    Done(Value),
}

/// Workflow decision-layer error.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkflowError {
    /// Rejected before startup because validation or shape checks failed.
    Invalid(String),
    /// Exceeded a named runtime limit.
    CapExceeded(String),
    /// The source produced no steps or completion while the driver has no in-flight work.
    Deadlock(String),
    /// The run budget is exhausted.
    BudgetExhausted(String),
    /// A runtime script failure, such as an uncaught exception, boundary-cloning
    /// error, or synchronous time-slice overrun. Unlike [`WorkflowError::Invalid`],
    /// the script has already started and may be eligible for recovery guidance.
    Script(String),
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::Invalid(message) => write!(formatter, "Pre-start validation failed: {message}"),
            WorkflowError::CapExceeded(message) => write!(formatter, "Runtime limit exceeded: {message}"),
            WorkflowError::Deadlock(message) => write!(formatter, "Workflow deadlock: {message}"),
            WorkflowError::BudgetExhausted(message) => {
                write!(formatter, "Workflow budget exhausted: {message}")
            }
            WorkflowError::Script(message) => write!(formatter, "Script execution failed: {message}"),
        }
    }
}

impl std::error::Error for WorkflowError {}

/// Source of workflow steps, implemented by the script engine and consumed by the driver.
///
/// Driver contract:
///
/// 1. The first call receives an empty slice.
/// 2. Call again after at least one result settles. `previous` contains all completed
///    results in completion order and grows monotonically.
/// 3. Requests in [`StepProgress::Run`] receive increasing global indexes and advance
///    the cache-key chain in vector order.
/// 4. An empty `Run` with no in-flight work terminates as [`WorkflowError::Deadlock`].
pub trait StepSource {
    fn advance(&mut self, previous: &[StepOutcome]) -> Result<StepProgress, WorkflowError>;

    /// Drains narrative lines produced by `log()` since the last call. The driver
    /// sends them to the progress ledger after each [`StepSource::advance`]; the
    /// ledger is the sole trimming point.
    fn drain_logs(&mut self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_limit_stays_pinned_to_its_reviewed_literal_value() {
        // Pin every named limit because persisted logs and UI rely on stable values.
        assert_eq!(MAX_SCRIPT_BYTES, 524_288);
        assert_eq!(MAX_LIFETIME_STEPS, 1_000);
        assert_eq!(MAX_BOUNDARY_ITEMS, 4_096);
        assert_eq!(MAX_SCHEMA_NODES, 100_000);
        assert_eq!(MAX_SCHEMA_DEPTH, 10_000);
        assert_eq!(MAX_STRUCTURED_OUTPUT_RETRIES, 5);
        assert_eq!(MAX_PROGRESS_ROWS, 1_000);
        assert_eq!(PROGRESS_TRIM_TARGET, 500);
        assert_eq!(MAX_PREVIEW_CHARS, 400);
        assert_eq!(MAX_LOG_MESSAGES, 1_000);
        assert_eq!(CACHE_KEY_PREFIX, "mw1");
        assert_eq!(WORKFLOW_STEP_STALL_MS, 180_000);
        assert_eq!(WORKFLOW_MAX_STALL_RETRIES, 5);
    }
}
