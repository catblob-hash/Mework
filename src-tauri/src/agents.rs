//! Conversation-scoped background subagent runtime backing the `agent` tool
//! group: `agent_spawn`, `send_message`, `followup_task`, `task_wait`, and
//! `task_list`.
//!
//! Only the user can stop a task, through `stop_conversation_task` over IPC;
//! the model has no entry point for stopping another task. `request_stop` is
//! called only by user IPC and targeted host paths such as workflow step cancellation.
//!
//! The host never terminates a task for inactivity. A terminal state may result
//! only from an explicit user stop, a worker return, or cancellation-timeout escalation.
//!
//! Top-level tasks are held by `AppState` per conversation and run in detached
//! threads, so round settlement does not interrupt them. Idle completion can
//! trigger a wake notification. Nested agents (`depth > 0`) retain a private
//! round pool and end at child-round quiescence. Agent identity is
//! durable: the cumulative child transcript is persisted on the latest
//! agent tool context, and a later turn rehydrates it from the conversation
//! timeline to continue the agent.
//!
//! Communication model (after Codex Multi-agent V2 and Claude Code):
//! - parent → child: `send_message` queues without waking; `followup_task`
//!   triggers another turn and is promoted after an in-flight turn finishes;
//! - child → parent: `subagent_update` progress envelopes plus an automatic
//!   final-result envelope, both drained by `task_wait`;
//! - worker → current or future run: the conversation-scoped task surface
//!   (`AppState::task_surface`) drops idle sinks and rejects idle approvals;
//!   see `api.rs` `agent_worker_loop`.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{
    model::{
        canonical_subagent_execution_mode_payload, ContextItem, ImageAttachment, ModelUsage,
        QueuedSubagentMessage, RunModelRequest, SubagentRunKind, SubagentRunRecord, SubagentRunStatus,
        SubagentUpdate,
    },
    prompt_profile::PromptKey,
};

/// Concurrent live agents per turn; spawning or waking beyond it fails softly.
///
/// This is the DEFAULT bound `AgentPool::new` installs, not a global ceiling: a
/// host runtime that owns its own pool states its own bound through
/// [`AgentPool::with_live_limit`]. Nothing outside pool construction should read
/// this constant to describe a live pool — ask that pool for its own
/// [`AgentPool::live_limit`] instead, or the message names a bound the pool is
/// not enforcing.
pub const MAX_LIVE_AGENTS: usize = 8;
/// How many names one `task_wait` call may enumerate.
///
/// Deliberately DECOUPLED from `MAX_LIVE_AGENTS`, which it used to borrow. That
/// coupling was invisible until a pool wanted a larger cap: raising the pool
/// alone would have left `task_wait` unable to name the agents that pool can
/// now run, and the failure would have surfaced as an argument-validation error
/// far from the cause. This bound guards request size only, so it is set to the
/// largest live cap any in-tree pool asks for rather than tracking one of them.
pub const MAX_WAIT_AGENT_NAMES: usize = 16;
pub const WAIT_MIN_TIMEOUT_SECONDS: u64 = 5;
pub const WAIT_MAX_TIMEOUT_SECONDS: u64 = 600;
pub const WAIT_DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub const MAX_AGENT_NAME_CHARS: usize = 32;
/// Poll cadence while `task_wait` blocks; every tick emits a heartbeat so a
/// cancelled run aborts the wait promptly.
pub const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Cadence for the finalize barrier that waits for worker threads to observe
/// their cancellation flags.
const QUIESCENCE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long the finalize barrier spins before it logs and forces terminal
/// statuses. The `thread::scope` join is still authoritative; this only bounds
/// the silent part.
const QUIESCENCE_DEADLINE: Duration = Duration::from_secs(30);

/// The two task-runtime tools. They are never user-selectable: the model needs
/// to be able to wait for and inspect tasks whenever anything can produce one,
/// and a conversation that can spawn a task but cannot collect it is a dead
/// state rather than a configuration. Same shape as the memory tools — kept in
/// the catalog, stripped from every persisted enabled list, re-derived by the
/// trusted request builder. Mirrored by TS `src/lib/taskTools.ts`.
pub const TASK_RUNTIME_TOOL_NAMES: [&str; 2] = ["task_wait", "task_list"];

/// Enabling any of these can put a row in the task list, so either of the two
/// task-runtime tools above becomes reachable.
///
/// Maps onto `orchestration::TaskRef`: `agent_spawn` / `workflow` occupy the
/// agent pool, `playwright` owns browser tabs, `bash` / `powershell` register
/// shell tasks. The remaining address kind, `Terminal`, is opened by the user
/// from the UI rather than by a tool, so it is deliberately not represented
/// here. Search and fetch are ordinary concurrent async tools whose results
/// come back as their own tool result, so they produce no task row and nothing
/// for `task_wait` to address.
pub const TASK_PRODUCING_TOOL_NAMES: [&str; 5] = [
    "agent_spawn",
    "workflow",
    "playwright",
    "bash",
    "powershell",
];

/// Tools the host dispatches at their call site and joins at the end of the
/// round. They are not tasks: they have no task identity, address, `task_wait`,
/// fold, or wake behavior. Their executions may overlap, but every call must
/// settle before the round ends.
pub const ASYNC_TOOL_NAMES: [&str; 2] = ["web_search", "web_fetch"];

pub fn runs_async(name: &str) -> bool {
    ASYNC_TOOL_NAMES.contains(&name)
}

pub fn is_task_runtime_tool_name(name: &str) -> bool {
    TASK_RUNTIME_TOOL_NAMES.contains(&name)
}

pub fn produces_tasks(name: &str) -> bool {
    TASK_PRODUCING_TOOL_NAMES.contains(&name)
}

/// Rewrites a persisted enabled-tool list into the one a trusted request uses.
///
/// Strip first, then derive. A persisted `task_wait` must not survive a
/// conversation that no longer enables anything able to produce a task, and the
/// derived pair must not depend on whether the renderer happened to write those
/// names — the same shape the memory tiers use.
pub fn apply_task_runtime_tools(enabled_tools: &mut Vec<String>) {
    enabled_tools.retain(|name| !is_task_runtime_tool_name(name));
    if enabled_tools.iter().any(|name| produces_tasks(name)) {
        enabled_tools.extend(
            TASK_RUNTIME_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentToolKind {
    Spawn,
    SendMessage,
    FollowupTask,
    Wait,
    List,
}

pub fn agent_tool_kind(name: &str) -> Option<AgentToolKind> {
    match name {
        "agent_spawn" => Some(AgentToolKind::Spawn),
        "send_message" => Some(AgentToolKind::SendMessage),
        "followup_task" => Some(AgentToolKind::FollowupTask),
        "task_wait" => Some(AgentToolKind::Wait),
        "task_list" => Some(AgentToolKind::List),
        _ => None,
    }
}

/// Validates an addressable agent name: short lowercase slug.
pub fn validate_agent_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > MAX_AGENT_NAME_CHARS {
        return Err(format!("A subagent name must be 1–{MAX_AGENT_NAME_CHARS} characters"));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty name");
    if !first.is_ascii_lowercase() {
        return Err("A subagent name must start with a lowercase letter".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("A subagent name may contain only lowercase letters, digits, underscores, and hyphens".into());
    }
    Ok(())
}

/// Live lifecycle of one agent within the current turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentLiveStatus {
    /// A worker thread is executing a child turn.
    Running,
    /// The last child turn finished normally; the agent is continuable.
    Idle,
    /// The last child turn ended for a reason the host could not classify,
    /// including a user-initiated task stop observed mid-turn. The agent
    /// remains continuable via `followup_task`.
    Interrupted,
    /// The last child turn errored. Continuable; retrying may succeed.
    Failed,
    /// Deliberately halted (explicit stop, hook stop, revoked definition).
    Stopped,
    /// Truncated by a host round/tool ceiling. The partial output is valid.
    RoundLimit,
}

impl AgentLiveStatus {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::RoundLimit => "roundLimit",
        }
    }

    /// The profile key of this status's model-visible word.
    pub fn prompt_key(self) -> PromptKey {
        match self {
            Self::Running => PromptKey::TaskStatusRunning,
            Self::Idle => PromptKey::TaskStatusIdle,
            Self::Interrupted => PromptKey::TaskStatusInterrupted,
            Self::Failed => PromptKey::TaskStatusFailed,
            Self::Stopped => PromptKey::TaskStatusStopped,
            Self::RoundLimit => PromptKey::TaskStatusRoundLimit,
        }
    }

    /// The live→persisted mapping must stay 1:1 for every classified state.
    /// Collapsing them here would discard the distinction at the last step and
    /// make the whole split unobservable in a reloaded transcript.
    pub fn persisted(self) -> SubagentRunStatus {
        match self {
            // A persisted Running state has no result, so use Interrupted as
            // the only defensive fallback that does not misrepresent one.
            Self::Running | Self::Interrupted => SubagentRunStatus::Interrupted,
            Self::Idle => SubagentRunStatus::Completed,
            Self::Failed => SubagentRunStatus::Failed,
            Self::Stopped => SubagentRunStatus::Stopped,
            Self::RoundLimit => SubagentRunStatus::RoundLimit,
        }
    }
}

/// The identity of one task incarnation: its generation and a digest of the
/// parameters that started it. `register` mints the first incarnation;
/// a successful `begin_turn` mints each successor; envelopes copy the identity
/// at their point of creation.
///
/// The host supplies this task-parameter digest to the shadow kernel so it can
/// map each envelope to its originating incarnation. Identity is minted only at
/// `register` and `begin_turn`. Results, progress, and cleanup use the identity
/// captured at the child turn's start, and incumbent guards in `complete_turn`,
/// `push_update`, and `RunningTurnGuard` reject stale worker writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskIdentity {
    /// Incarnation generation for this task name. It starts at 1 and increases
    /// monotonically after each successful `begin_turn` in this process.
    pub generation: u64,
    /// Digest of the parameters that started this generation. The first
    /// generation uses the task text; later generations use its follow-up batch
    /// and task name to prevent cross-name mismatches.
    pub params_digest: u64,
}

impl TaskIdentity {
    fn mint<'p>(
        name: &str,
        generation: u64,
        params: impl IntoIterator<Item = &'p str>,
    ) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut hasher);
        for part in params {
            part.hash(&mut hasher);
        }
        TaskIdentity {
            generation,
            params_digest: hasher.finish(),
        }
    }
}

/// One child→parent envelope drained by `task_wait`.
#[derive(Clone, Debug)]
pub struct AgentEnvelope {
    pub agent: String,
    /// Identity of the incarnation that produced this envelope, copied from the
    /// incumbent identity while holding the core lock.
    pub identity: TaskIdentity,
    pub kind: EnvelopeKind,
    pub content: String,
    /// Cost of the child turn this envelope terminates. Present only on
    /// `Result`; an `Update` reports progress, not a completed turn.
    ///
    /// Without this the parent cannot see what a delegation cost and has no
    /// basis for regulating its own fan-out.
    pub metrics: Option<AgentTurnMetrics>,
    /// The value a child handed back through `structured_output`, already
    /// validated against the spawn-time schema. Present only on `Result`, and
    /// only when the spawn supplied an `output_schema`.
    ///
    /// It rides here rather than inside `EnvelopeKind::Result`: that enum is
    /// `Copy` and is matched by value at three sites, so widening the variant
    /// would break them as borrow errors rather than as a missing field.
    pub structured_output: Option<Value>,
}

/// What one child turn consumed, as reported back to the parent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentTurnMetrics {
    pub usage: ModelUsage,
    pub tool_use_count: usize,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeKind {
    /// A `subagent_update` progress message.
    Update,
    /// The final text of a finished child turn.
    Result(SubagentRunStatus),
}

/// One queued instruction. Top-level steer messages preserve the frontend's
/// queue id and timestamp; agent-to-agent messages receive canonical context
/// metadata when the child drains them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxMessage {
    pub id: Option<String>,
    pub content: String,
    pub images: Vec<ImageAttachment>,
    pub created_at: Option<String>,
}

/// Message queue shared by top-level steering and parent→child communication.
/// A handle is attached to `RunModelRequest`, and the model loop drains it
/// before every model round.
#[derive(Debug, Default)]
pub struct AgentMailbox {
    messages: Mutex<Vec<MailboxMessage>>,
}

impl AgentMailbox {
    pub fn push(&self, message: String) {
        self.push_message(MailboxMessage {
            id: None,
            content: message,
            images: Vec::new(),
            created_at: None,
        });
    }

    pub fn push_message(&self, message: MailboxMessage) {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(message);
    }

    pub fn drain(&self) -> Vec<MailboxMessage> {
        std::mem::take(
            &mut *self
                .messages
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    pub fn snapshot(&self) -> Vec<MailboxMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn extend(&self, messages: impl IntoIterator<Item = MailboxMessage>) {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(messages);
    }
}

/// `RunModelRequest` attachment for the child mailbox. Never serialized and
/// compared by identity so the request keeps its derives.
#[derive(Clone, Default)]
pub struct AgentMailboxHandle(pub Option<Arc<AgentMailbox>>);

impl std::fmt::Debug for AgentMailboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "AgentMailboxHandle(attached)"
        } else {
            "AgentMailboxHandle(none)"
        })
    }
}

impl PartialEq for AgentMailboxHandle {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

#[derive(Debug)]
struct AgentCore {
    status: AgentLiveStatus,
    /// Identity of the incumbent task incarnation. `register` mints the first
    /// identity, `begin_turn` mints successors, and envelopes copy it when made.
    identity: TaskIdentity,
    /// At most one explicit-stop timer per incarnation; repeated stops never
    /// move its deadline. Old timers still verify identity under this lock.
    stop_timer: Option<TaskIdentity>,
    /// Cumulative transcript across every child turn, starting with the task.
    contexts: Vec<ContextItem>,
    updates: Vec<SubagentUpdate>,
    outbox: Vec<AgentEnvelope>,
    /// The agent tool call that last created, messaged, or continued the child;
    /// the persisted record is backfilled onto this tool context.
    latest_call_id: String,
    /// Timeline id of the tool card `latest_call_id` produced, recorded when
    /// the parent run minted that card. Round-boundary record checkpoints use
    /// it to find the card of a child that outlived its spawning request —
    /// that request's call→context map died with the request, and without this
    /// the terminal record could only ever reach a card of the current run.
    latest_context_id: Option<String>,
    usage: ModelUsage,
    /// Same tokens as `usage`, but never taken. `take_usage` moves `usage` into
    /// the parent's turn total, so a record built after that point would report
    /// zero — and one that read `usage` directly would double-count if the
    /// record were also folded into the parent. This copy exists solely to be
    /// persisted per agent, and is the value the task sidebar shows.
    lifetime_usage: ModelUsage,
    /// Result of the LATEST child turn, not a cumulative best-known value.
    /// `complete_turn` overwrites it unconditionally, mirroring `status`
    /// (contexts extend; status and this replace), so a follow-up turn that
    /// ended without a structured result cannot leave the previous turn's value
    /// standing as if it described the current one.
    structured_output: Option<Value>,
}

/// Called when `complete_turn` records a terminal envelope with the task name
/// and persisted status. The observer determines whether the conversation is
/// idle before sending a wake notification. Stale or already-settled returns do
/// not notify because the incumbent guard rejects them first.
pub type SettleObserver = Arc<dyn Fn(&str, SubagentRunStatus) + Send + Sync>;

/// Observer slot shared by the pool and all `AgentShared` instances.
/// `Debug` is handwritten because `dyn Fn` cannot derive it.
#[derive(Clone, Default)]
pub struct SettleObserverSlot(Arc<Mutex<Option<SettleObserver>>>);

impl std::fmt::Debug for SettleObserverSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SettleObserverSlot")
    }
}

impl SettleObserverSlot {
    fn set(&self, observer: SettleObserver) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(observer);
        }
    }

    fn notify(&self, name: &str, status: SubagentRunStatus) {
        let observer = self.0.lock().ok().and_then(|slot| slot.clone());
        if let Some(observer) = observer {
            observer(name, status);
        }
    }
}

/// Who asked a task to stop. The distinction is model-visible: only a user's
/// sidebar close makes the delivered result say the user closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOrigin {
    /// The user pressed stop on the task row (IPC `stop_conversation_task`).
    User,
    /// The host cancelled the task for its own reasons — a workflow driver
    /// skipping a step or dropping a race loser.
    Host,
}

/// Shared state of one agent, owned jointly by the pool, the dispatching
/// parent loop and the agent's worker thread.
#[derive(Debug)]
pub struct AgentShared {
    pub name: String,
    pub label: String,
    pub task: String,
    pub mailbox: Arc<AgentMailbox>,
    /// Follow-up tasks are not visible to an in-flight child request. The
    /// worker promotes them only after that child turn has completed.
    pub followups: Arc<AgentMailbox>,
    /// Stop flag shared by value with synchronous blocking operations in the
    /// task turn, including shell processes, hook commands, and approval cards.
    /// Use [`Self::cancel_flag`] to obtain a clone for polling.
    pub cancel: Arc<AtomicBool>,
    /// Whether the stop request came from the user's sidebar rather than from
    /// the host's own scheduling. Only the former makes the settled result say
    /// the task was closed by hand. Set by [`Self::request_stop`].
    stopped_by_user: AtomicBool,
    /// Prototype child request (provider, tools, hooks, depth…) with empty
    /// contexts; each child turn clones it and fills contexts + mailbox.
    pub template: RunModelRequest,
    /// Host-selected role. Part of the signed execution-mode payload, so a
    /// search-group worker's receipt can never be replayed as an ordinary
    /// addressable agent's.
    pub kind: SubagentRunKind,
    core: Mutex<AgentCore>,
    signal: Arc<(Mutex<u64>, Condvar)>,
    /// Settlement observer slot shared with the pool. See [`SettleObserver`].
    settle_observer: SettleObserverSlot,
    /// Shared with the pool so old handles cannot start follow-up incarnations.
    retired: Arc<AtomicBool>,
}

impl AgentShared {
    /// Claims the agent for a new child turn. Returns false when another
    /// worker already runs it, so exactly one thread continues an agent.
    pub fn begin_turn(&self, call_id: &str) -> bool {
        let mut core = self.lock_core();
        if self.retired.load(Ordering::Acquire) || core.status == AgentLiveStatus::Running {
            return false;
        }
        core.status = AgentLiveStatus::Running;
        core.latest_call_id = call_id.to_owned();
        // A new call card has not been minted, so the previous card no longer
        // owns persistence.
        core.latest_context_id = None;
        // A successful claim creates a new incarnation using the follow-up
        // messages that woke it. Both wake paths enqueue the trigger before
        // `begin_turn`; later messages cannot rewrite startup parameters.
        // Lock order is core → followups, matching `record`.
        let trigger = self.followups.snapshot();
        core.identity = TaskIdentity::mint(
            &self.name,
            core.identity.generation + 1,
            trigger.iter().map(|message| message.content.as_str()),
        );
        true
    }

    /// Identity of the incumbent incarnation submitted to the shadow kernel at
    /// wait-start and timeout observation points.
    pub fn identity(&self) -> TaskIdentity {
        self.lock_core().identity
    }

    /// Appends finished child-turn contexts and reports the result to the parent
    /// inbox. `incarnation` is captured by the worker at child-turn start. The
    /// incumbent guard records state and emits an envelope only while that
    /// incarnation remains incumbent and Running. Stale returns and returns
    /// after same-generation terminal settlement retain actual usage but cannot
    /// overwrite terminal state or emit a second terminal envelope.
    pub fn complete_turn(
        &self,
        incarnation: TaskIdentity,
        new_contexts: Vec<ContextItem>,
        final_text: String,
        status: AgentLiveStatus,
        usage: &ModelUsage,
        duration_ms: u64,
        structured_output: Option<Value>,
    ) {
        let settled = {
            let mut core = self.lock_core();
            merge_shared_usage(&mut core.usage, usage);
            merge_shared_usage(&mut core.lifetime_usage, usage);
            if core.identity != incarnation {
                // A follow-up replaced this incarnation. Do not merge its
                // orphaned result into the successor transcript.
                eprintln!(
                    "[agents] 丢弃陈旧化身的迟到结果：{}（第 {} 代已被第 {} 代更替）",
                    self.name, incarnation.generation, core.identity.generation
                );
                return;
            }
            // Counted before the extend so it reflects this turn only, not the
            // cumulative transcript.
            let tool_use_count = new_contexts
                .iter()
                .filter(|context| matches!(context, ContextItem::Tool { .. }))
                .count();
            core.contexts.extend(new_contexts);
            if core.status != AgentLiveStatus::Running {
                // Preserve the transcript after same-generation terminal
                // settlement, but do not replace its status or emit another envelope.
                eprintln!(
                    "[agents] 化身 {} 第 {} 代已被强制终态化,迟到结果不再落状态",
                    self.name, incarnation.generation
                );
                return;
            }
            core.status = status;
            core.structured_output = structured_output.clone();
            let persisted = status.persisted();
            core.outbox.push(AgentEnvelope {
                agent: self.name.clone(),
                identity: incarnation,
                kind: EnvelopeKind::Result(persisted),
                content: final_text,
                metrics: Some(AgentTurnMetrics {
                    usage: usage.clone(),
                    tool_use_count,
                    duration_ms,
                }),
                structured_output,
            });
            persisted
        };
        self.notify();
        // Notify only after a terminal result passes the incumbent guard.
        // Invoke outside the core lock because the observer queries the runtime
        // registry and sends notifications.
        self.settle_observer.notify(&self.name, settled);
    }

    /// Records a `subagent_update` progress message for `task_wait`. The same
    /// incumbent guard as `complete_turn` rejects progress from stale or
    /// settled incarnations.
    pub fn push_update(&self, incarnation: TaskIdentity, content: String, created_at: String) {
        {
            let mut core = self.lock_core();
            if core.identity != incarnation || core.status != AgentLiveStatus::Running {
                return;
            }
            core.updates.push(SubagentUpdate {
                content: content.clone(),
                created_at,
            });
            core.outbox.push(AgentEnvelope {
                agent: self.name.clone(),
                identity: incarnation,
                kind: EnvelopeKind::Update,
                content,
                metrics: None,
                structured_output: None,
            });
        }
        self.notify();
    }

    pub fn status(&self) -> AgentLiveStatus {
        self.lock_core().status
    }

    /// Forces an agent that ignored its cancel flag to a terminal state,
    /// preserving its transcript and emitting a result envelope so the parent
    /// learns why it stopped. Never drops the record — a settled agent must
    /// still be continuable.
    ///
    /// Called only after `await_quiescence_until` expires following a
    /// cancellation request that the worker did not honor. It is unrelated to
    /// inactivity.
    pub(crate) fn settle_after_cancel_timeout(&self) {
        self.settle_cancelled_incarnation(None);
    }

    fn settle_cancelled_incarnation(&self, expected: Option<TaskIdentity>) {
        let settled = {
            let mut core = self.lock_core();
            if core.status != AgentLiveStatus::Running
                || expected.is_some_and(|identity| core.identity != identity
                    || !self.cancel.load(Ordering::Acquire))
            {
                return;
            }
            core.status = AgentLiveStatus::Stopped;
            let identity = core.identity;
            let reason = self.template.prompt_profile.render(
                PromptKey::SubagentForcedStop,
                &[("name", &self.name)],
            );
            core.outbox.push(AgentEnvelope {
                agent: self.name.clone(),
                identity,
                kind: EnvelopeKind::Result(SubagentRunStatus::Stopped),
                content: reason,
                metrics: None,
                structured_output: None,
            });
            SubagentRunStatus::Stopped
        };
        self.notify();
        // Same edge signal as `complete_turn`: this envelope is deliverable, so
        // an idle conversation must wake for it rather than wait for the
        // level-triggered rescan. Raised outside the core lock.
        self.settle_observer.notify(&self.name, settled);
    }

    /// Asks the worker to abort at its next stream event or round boundary.
    ///
    /// `origin` records who asked. `StopOrigin::User` is the sidebar stop, and
    /// only it makes the settled result say the user closed the task; the
    /// workflow driver's own step cancellations are `StopOrigin::Host` and must
    /// not put words in the user's mouth.
    ///
    /// The worker observes this at its next stream event or round boundary. A
    /// synchronous operation such as a `bash` process or hook command polls the
    /// same flag returned by [`Self::cancel_flag`]. See [`crate::cancel`].
    pub fn request_stop(&self, origin: StopOrigin) {
        if origin == StopOrigin::User {
            self.stopped_by_user.store(true, Ordering::Release);
        }
        self.cancel.store(true, Ordering::Release);
        self.notify();
    }

    /// Stop one long-lived conversation task without waiting for its pool to
    /// quiesce. This bounds logical settlement, not the lifetime of a blocked
    /// native thread, and never treats silence as cancellation.
    pub fn stop_task(self: &Arc<Self>, origin: StopOrigin) {
        self.stop_task_with_grace(origin, QUIESCENCE_DEADLINE);
    }

    fn stop_task_with_grace(self: &Arc<Self>, origin: StopOrigin, grace: Duration) {
        let identity = {
            let mut core = self.lock_core();
            if core.status != AgentLiveStatus::Running {
                return;
            }
            if origin == StopOrigin::User {
                self.stopped_by_user.store(true, Ordering::Release);
            }
            self.cancel.store(true, Ordering::Release);
            if core.stop_timer == Some(core.identity) {
                return;
            }
            core.stop_timer = Some(core.identity);
            core.identity
        };
        self.notify();
        let task = Arc::downgrade(self);
        if std::thread::Builder::new().name("task-stop-grace".into()).spawn(move || {
            std::thread::sleep(grace);
            if let Some(task) = task.upgrade() {
                task.settle_cancelled_incarnation(Some(identity));
            }
        }).is_err() {
            // Resource exhaustion must not leave a cancelled task permanently
            // occupying capacity just because its grace timer could not start.
            self.settle_cancelled_incarnation(Some(identity));
        }
    }

    /// Whether the user closed this task from the sidebar, as opposed to the
    /// host cancelling it for its own scheduling reasons.
    pub fn stopped_by_user(&self) -> bool {
        self.stopped_by_user.load(Ordering::Acquire)
    }

    /// Stop flag passed by value to synchronous blocking points in the task turn.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    pub fn latest_call_id(&self) -> String {
        self.lock_core().latest_call_id.clone()
    }

    pub fn latest_context_id(&self) -> Option<String> {
        self.lock_core().latest_context_id.clone()
    }

    /// Records the timeline id of the tool card `call_id` produced, so a later
    /// run can still find the card once the minting run's call→context map is
    /// gone. A stale call id is ignored: a newer call already owns persistence.
    pub fn record_call_context(&self, call_id: &str, context_id: &str) {
        let mut core = self.lock_core();
        if core.latest_call_id == call_id {
            core.latest_context_id = Some(context_id.to_owned());
        }
    }

    /// Makes a queue-only message the latest persistence owner without changing
    /// the agent's live status or waking a worker.
    pub fn record_message_call(&self, call_id: &str) {
        let mut core = self.lock_core();
        core.latest_call_id = call_id.to_owned();
        core.latest_context_id = None;
    }

    /// Moves follow-up tasks into the child-visible inbox at the boundary
    /// between child turns.
    pub fn activate_followups(&self) {
        self.mailbox.extend(self.followups.drain());
    }

    pub fn transcript(&self) -> Vec<ContextItem> {
        self.lock_core().contexts.clone()
    }

    pub fn latest_update(&self) -> Option<String> {
        self.lock_core()
            .updates
            .last()
            .map(|update| update.content.clone())
    }

    /// Builds the persisted record snapshot for the conversation document.
    pub fn record(&self) -> SubagentRunRecord {
        self.record_locked(&self.lock_core())
    }

    /// A recovery-only copy of this incarnation's settled rounds. Hold the
    /// identity/owner lock THROUGH persistence, not just while taking a snapshot:
    /// otherwise a late database write could overwrite a successor's card.
    /// The callback must not re-enter AgentShared or emit lifecycle events.
    pub(crate) fn checkpoint_transcript(
        &self,
        incarnation: TaskIdentity,
        live: &[ContextItem],
        persist: impl FnOnce(&str, SubagentRunRecord) -> bool,
    ) -> Option<bool> {
        let core = self.lock_core();
        if core.identity != incarnation || core.status != AgentLiveStatus::Running {
            return None;
        }
        let Some(context_id) = core.latest_context_id.as_deref() else {
            return Some(false);
        };
        let mut record = self.record_locked(&core);
        let mut ids = record.contexts.iter().map(|item| item.id().to_owned()).collect::<HashSet<_>>();
        record.contexts.extend(live.iter().filter(|item| ids.insert(item.id().to_owned())).cloned());
        Some(persist(context_id, record))
    }

    fn record_locked(&self, core: &AgentCore) -> SubagentRunRecord {
        let inherits_model_memory = self.template.inherits_parent_model_memory;
        SubagentRunRecord {
            kind: self.kind,
            name: Some(self.name.clone()),
            // Only managed tasks persist their address: `workflow:<runId>`
            // remains model-visible through `task_list` after the run ends.
            // Ordinary agents use their name; persisting a duplicate label adds
            // signed bytes to every record.
            label: (self.kind == SubagentRunKind::WorkflowStep && !self.label.is_empty())
                .then(|| self.label.clone()),
            inherits_model_memory,
            fork_model_binding: inherits_model_memory
                .then(|| self.template.fork_model_binding.clone())
                .flatten(),
            agent_definition: (!inherits_model_memory)
                .then(|| self.template.agent_definition_binding.clone())
                .flatten(),
            execution_mode_receipt: self
                .template
                .subagent_execution_mode_receipt
                .clone()
                .unwrap_or_default(),
            task: self.task.clone(),
            status: core.status.persisted(),
            contexts: core.contexts.clone(),
            updates: core.updates.clone(),
            structured_output: core.structured_output.clone(),
            usage: core.lifetime_usage.clone(),
            // The raw schema document, not the compiled `Schema`: the record is
            // renderer-writable, so rehydration re-compiles it under the same
            // bounds as a fresh spawn instead of trusting a persisted artifact.
            output_schema: self
                .template
                .output_schema
                .as_ref()
                .map(|schema| schema.as_value().clone()),
            queued_messages: self
                .mailbox
                .snapshot()
                .into_iter()
                .map(|message| QueuedSubagentMessage {
                    content: message.content,
                    trigger_turn: false,
                })
                .chain(
                    self.followups
                        .snapshot()
                        .into_iter()
                        .map(|message| QueuedSubagentMessage {
                            content: message.content,
                            trigger_turn: true,
                        }),
                )
                .collect(),
        }
    }

    pub fn take_usage(&self) -> ModelUsage {
        std::mem::take(&mut self.lock_core().usage)
    }

    /// Restores the per-agent token total a rehydrated record carries.
    ///
    /// Deliberately seeds only `lifetime_usage`, never `usage`: the parent
    /// already billed these tokens in the turn that spent them, and `usage` is
    /// what the next `take_usage` folds into the parent's total, so seeding it
    /// would charge the same tokens a second time on every continuation.
    pub fn restore_lifetime_usage(&self, usage: ModelUsage) {
        self.lock_core().lifetime_usage = usage;
    }

    /// Tokens this agent alone has consumed across every turn, unaffected by
    /// `take_usage`. This is the figure persisted on the record.
    ///
    /// Test-only: the production snapshot reads `core.lifetime_usage` directly,
    /// so leaving this getter in the non-test build only adds a `dead_code`
    /// warning. Delete the attribute the day a production caller appears.
    #[cfg(test)]
    pub fn lifetime_usage(&self) -> ModelUsage {
        self.lock_core().lifetime_usage.clone()
    }

    fn drain_outbox(&self) -> Vec<AgentEnvelope> {
        std::mem::take(&mut self.lock_core().outbox)
    }

    /// Puts drained-but-undelivered envelopes back at the head of the outbox,
    /// keeping their relative order. Only an aborted wait calls this: it
    /// drained these to buffer them, and the outbox is their sole copy.
    fn restore_envelopes(&self, envelopes: Vec<AgentEnvelope>) {
        let mut core = self.lock_core();
        let mut restored = envelopes;
        restored.append(&mut core.outbox);
        core.outbox = restored;
    }

    /// Removes and returns terminal Result envelopes the parent never drained.
    ///
    /// Selective on purpose, unlike `drain_outbox`: `Update` envelopes stay
    /// collectable by a later `task_wait`. Every terminal result is claimed and
    /// every terminal result is deliverable — a task the user closed reports
    /// whatever it produced, exactly like one that finished on its own.
    /// Removal occurs under the core mutex.
    /// IS the one-shot claim: whichever of `task_wait`'s drain or this call
    /// wins the lock delivers the envelope, and the other never sees it.
    pub(crate) fn take_undelivered_results(&self) -> Vec<AgentEnvelope> {
        let mut core = self.lock_core();
        let (delivered, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut core.outbox)
            .into_iter()
            .partition(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)));
        core.outbox = kept;
        delivered
    }

    /// Whether the outbox contains a terminal result that still owes delivery
    /// and therefore requires a delivery round. Every terminal status qualifies:
    /// there is no discard projection, so a stopped task's result folds and
    /// wakes like any other. This only peeks; `take_undelivered_results`
    /// performs one-time claiming.
    pub(crate) fn has_undrained_foldable_results(&self) -> bool {
        self.lock_core()
            .outbox
            .iter()
            .any(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
    }

    fn notify(&self) {
        let (generation, condvar) = &*self.signal;
        *generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        condvar.notify_all();
    }

    fn lock_core(&self) -> std::sync::MutexGuard<'_, AgentCore> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Ensures a worker thread that unwinds (panic or early exit) leaves its agent
/// in a terminal state so `finalize` never waits forever. The same incumbent
/// guard as `complete_turn` lets a guard clean up only its own incarnation; a
/// stale worker must not interrupt an incarnation replaced by a follow-up.
pub struct RunningTurnGuard {
    shared: Arc<AgentShared>,
    incarnation: TaskIdentity,
    defused: bool,
}

impl RunningTurnGuard {
    pub fn new(shared: Arc<AgentShared>) -> Self {
        let incarnation = shared.identity();
        Self {
            shared,
            incarnation,
            defused: false,
        }
    }

    pub fn defuse(mut self) {
        self.defused = true;
    }
}

impl Drop for RunningTurnGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }
        // A panicking worker is a host error. Record it as Failed with an
        // envelope and reason, and notify the settlement observer. It must not
        // masquerade as `Interrupted`, which reads as a deliberate user stop.
        // The incumbent guard cleans up only this incarnation.
        if std::thread::panicking() {
            let settled = {
                let mut core = self.shared.lock_core();
                if core.status == AgentLiveStatus::Running && core.identity == self.incarnation {
                    core.status = AgentLiveStatus::Failed;
                    let persisted = AgentLiveStatus::Failed.persisted();
                    core.outbox.push(AgentEnvelope {
                        agent: self.shared.name.clone(),
                        identity: self.incarnation,
                        kind: EnvelopeKind::Result(persisted),
                        content: self
                            .shared
                            .template
                            .prompt_profile
                            .text(PromptKey::SubagentWorkerPanic)
                            .to_owned(),
                        metrics: None,
                        structured_output: None,
                    });
                    Some(persisted)
                } else {
                    None
                }
            };
            self.shared.notify();
            if let Some(settled) = settled {
                // A second panic during unwinding aborts the process. Isolate
                // observer failures with `catch_unwind`; failed notifications
                // have a level-triggered rescan fallback.
                let shared = Arc::clone(&self.shared);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    shared.settle_observer.notify(&shared.name, settled);
                }));
            }
            return;
        }
        {
            let mut core = self.shared.lock_core();
            if core.status == AgentLiveStatus::Running && core.identity == self.incarnation {
                core.status = AgentLiveStatus::Interrupted;
            }
        }
        self.shared.notify();
    }
}

#[derive(Debug)]
pub struct WaitOutcome {
    pub envelopes: Vec<AgentEnvelope>,
    pub timed_out: bool,
}

/// What ends a wait. The two modes differ only in whether a progress update
/// counts: `task_wait` is the model's blocking primitive and must come back
/// with the result, while the workflow driver reads envelopes as wake signals
/// and harvests authoritative outcomes from the records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitMode {
    AnyActivity,
    TerminalResult,
}

/// Envelopes a wait has drained but cannot yet deliver (progress updates under
/// `WaitMode::TerminalResult`). The outbox is their only copy, so whatever this
/// guard still holds when it drops goes back — which covers the abort leg and
/// an unwind alike. The unwind case is not theoretical: `heartbeat` bottoms out
/// in a caller-supplied event sink, and the `Fn` bound promises nothing about
/// panics.
struct BufferedEnvelopes<'a> {
    pool: &'a AgentPool,
    watched: &'a [Arc<AgentShared>],
    envelopes: Vec<AgentEnvelope>,
}

impl BufferedEnvelopes<'_> {
    /// Hands the buffer to a returning wait, leaving the guard empty so its
    /// drop restores nothing.
    fn take(&mut self) -> Vec<AgentEnvelope> {
        std::mem::take(&mut self.envelopes)
    }
}

impl Drop for BufferedEnvelopes<'_> {
    fn drop(&mut self) {
        let envelopes = std::mem::take(&mut self.envelopes);
        self.pool.restore_envelopes(self.watched, envelopes);
    }
}

/// Registry of a conversation's tasks (S11): the top-level runtime is
/// conversation-scoped, while child runs retain private per-turn pools. Holds no
/// join handles — workers are detached threads that communicate through the
/// status signal; `await_quiescence` remains available for private pools that
/// still tear down with their owner.
pub struct AgentPool {
    agents: Mutex<Vec<Arc<AgentShared>>>,
    retired: Arc<AtomicBool>,
    signal: Arc<(Mutex<u64>, Condvar)>,
    /// Settlement observer slot shared by every `AgentShared` registered in this pool.
    settle_observer: SettleObserverSlot,
    /// `None` lifts the live-agent cap entirely. The retired search dispatcher
    /// owned a private pool whose only members were its own executors, and its
    /// fan-out was already bounded by the host-enforced effort budget, so the
    /// generic cap would only cut that budget short. Every other pool keeps
    /// `MAX_LIVE_AGENTS`, which protects a user's own `agent_spawn` fan-out.
    /// There is no `Default` impl on purpose: an unbounded pool must be an
    /// explicit decision at the construction site.
    max_live: Option<usize>,
}

impl AgentPool {
    pub fn new() -> Self {
        Self::with_limit(Some(MAX_LIVE_AGENTS))
    }

    /// Uncapped pool. The retired search dispatcher was the last
    /// production consumer; tests keep it for fixtures that must never trip
    /// the live-agent cap.
    #[cfg(test)]
    pub fn unbounded() -> Self {
        Self::with_limit(None)
    }

    /// Pool with an explicit bounded live-agent cap.
    ///
    /// `new()` (the product default) is the only other constructor, so a host
    /// runtime that wants a DIFFERENT bound — the Workflow runtime's private
    /// pool, sized from `available_parallelism` — would otherwise have to go
    /// uncapped and count permits on its own side. That leaves the bound
    /// unenforced at `validate_registration_availability`, the one seam every
    /// registration passes through, so it is not equivalent.
    ///
    /// A request for 0 is raised to 1. A pool that can never run anything is
    /// never what a caller meant, and turning an arithmetic slip in a caller's
    /// sizing expression into a permanently unsatisfiable pool would surface as
    /// a stalled run rather than as an error.
    pub fn with_live_limit(max_live: usize) -> Self {
        Self::with_limit(Some(max_live.max(1)))
    }

    /// The live-agent cap this pool actually enforces, or `None` when it is
    /// uncapped. Message text that quotes a limit must read it from here.
    pub fn live_limit(&self) -> Option<usize> {
        self.max_live
    }

    fn with_limit(max_live: Option<usize>) -> Self {
        Self {
            agents: Mutex::new(Vec::new()),
            retired: Arc::new(AtomicBool::new(false)),
            signal: Arc::new((Mutex::new(0), Condvar::new())),
            settle_observer: SettleObserverSlot::default(),
            max_live,
        }
    }

    /// Installs the settlement observer, replacing any prior observer. Existing
    /// `AgentShared` instances observe it immediately through the shared slot.
    pub fn set_settle_observer(&self, observer: SettleObserver) {
        self.settle_observer.set(observer);
    }

    /// Creates and registers a new agent. Fails when the name is taken or the
    /// live-agent cap is reached.
    pub(crate) fn preflight_registration(
        &self,
        name: &str,
        template: &RunModelRequest,
        status: AgentLiveStatus,
    ) -> Result<(), String> {
        validate_agent_name(name)?;
        validate_child_memory_mode(template)?;
        let agents = self.lock_agents();
        if self.retired.load(Ordering::Acquire) {
            return Err("对话已删除，不能注册任务".into());
        }
        Self::validate_registration_availability(&agents, name, status, self.max_live)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register(
        &self,
        name: String,
        label: String,
        task: String,
        template: RunModelRequest,
        kind: SubagentRunKind,
        state: &crate::state::AppState,
        initial_contexts: Vec<ContextItem>,
        initial_updates: Vec<SubagentUpdate>,
        status: AgentLiveStatus,
        call_id: String,
        // Structured result carried over from the persisted record when this
        // registration is a rehydration. Without it, continuing an agent whose
        // last turn returned a structured value would write `None` back on the
        // next `record()` and silently drop it from the document.
        initial_structured_output: Option<Value>,
    ) -> Result<Arc<AgentShared>, String> {
        validate_agent_name(&name)?;
        validate_child_memory_mode(&template)?;
        // `kind` is part of the execution-mode payload, so a search-group
        // worker name cannot be reused as an ordinary addressable agent. Reserve
        // it here, at the point the name is actually occupied, to avoid a
        // reserved-but-unregistered window.
        let canonical_mode = canonical_subagent_execution_mode_payload(
            &template.conversation_id,
            &name,
            kind,
            template.inherits_parent_model_memory,
            template.fork_model_binding.as_ref(),
            template.agent_definition_binding.as_ref(),
        )?;
        let mut agents = self.lock_agents();
        if self.retired.load(Ordering::Acquire) {
            return Err("对话已删除，不能注册任务".into());
        }
        state.reserve_subagent_execution_mode(&template.conversation_id, &name, &canonical_mode)?;
        Self::validate_registration_availability(&agents, &name, status, self.max_live)?;
        // Registration mints the first identity from the task text. Rehydration
        // follows the same path; generation need only be monotonic during one
        // `AgentShared` lifetime.
        let identity = TaskIdentity::mint(&name, 1, [task.as_str()]);
        let shared = Arc::new(AgentShared {
            name,
            label,
            task,
            mailbox: Arc::new(AgentMailbox::default()),
            followups: Arc::new(AgentMailbox::default()),
            cancel: Arc::new(AtomicBool::new(false)),
            stopped_by_user: AtomicBool::new(false),
            template,
            kind,
            core: Mutex::new(AgentCore {
                status,
                identity,
                stop_timer: None,
                contexts: initial_contexts,
                updates: initial_updates,
                outbox: Vec::new(),
                latest_call_id: call_id,
                latest_context_id: None,
                usage: ModelUsage::default(),
                lifetime_usage: ModelUsage::default(),
                structured_output: initial_structured_output,
            }),
            signal: Arc::clone(&self.signal),
            settle_observer: self.settle_observer.clone(),
            retired: Arc::clone(&self.retired),
        });
        agents.push(Arc::clone(&shared));
        Ok(shared)
    }

    /// Non-destructive wake snapshot for the main-thread kernel observer. Use
    /// each envelope's identity, not the agent's current incarnation: an older
    /// result can still await delivery while a follow-up is already running.
    pub(crate) fn undelivered_result_identities(&self) -> Vec<(String, TaskIdentity)> {
        self.all().into_iter().flat_map(|agent| {
            agent.lock_core().outbox.iter()
                .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
                .map(|envelope| (envelope.agent.clone(), envelope.identity))
                .collect::<Vec<_>>()
        }).collect()
    }

    /// Claims every agent's undrained terminal results, in registration order.
    /// See `AgentShared::take_undelivered_results` for the claim semantics
    /// (all `Result` envelopes; `Update` envelopes stay for `task_wait`).
    pub fn take_undelivered_results(&self) -> Vec<AgentEnvelope> {
        self.lock_agents()
            .iter()
            .flat_map(|agent| agent.take_undelivered_results())
            .collect()
    }

    /// Whether the pool contains a completed, failed, or round-limit result
    /// that follows the fold projection and requires a delivery round.
    pub fn has_undrained_foldable_results(&self) -> bool {
        self.lock_agents()
            .iter()
            .any(|agent| agent.has_undrained_foldable_results())
    }

    pub fn find(&self, name: &str) -> Option<Arc<AgentShared>> {
        self.lock_agents()
            .iter()
            .find(|agent| agent.name == name)
            .cloned()
    }

    /// The agent whose latest owning tool call is `call_id` — the reverse of
    /// `latest_call_id`, used when the run loop mints that call's tool card and
    /// wants to hand the card's timeline id back to the agent.
    pub fn find_by_latest_call(&self, call_id: &str) -> Option<Arc<AgentShared>> {
        // Deliberately snapshots the roster before probing: `latest_call_id`
        // takes each agent's core lock, and holding the roster lock across that
        // would stall every `register`/`find` behind whichever core lock a
        // worker thread happens to hold.
        self.all()
            .into_iter()
            .find(|agent| agent.latest_call_id() == call_id)
    }

    /// Finds a managed task by its model-visible address label, such as
    /// `workflow:<runId>` or `shell:<id>`. Task-stop commands use this because
    /// pool names are internal and the UI only has addresses.
    ///
    /// Only managed-task kinds qualify. Ordinary agent labels are free-form and
    /// may resemble a task address; resolve those agents by [`Self::find`].
    pub fn find_by_label(&self, label: &str) -> Option<Arc<AgentShared>> {
        self.lock_agents()
            .iter()
            .find(|agent| {
                matches!(
                    agent.kind,
                    crate::model::SubagentRunKind::WorkflowStep
                        | crate::model::SubagentRunKind::ShellCommand
                ) && agent.label == label
            })
            .cloned()
    }

    /// Test helper that waits, without cancellation, for every pool entry to
    /// leave Running. Detached workers require this explicit wait; it panics on timeout.
    #[cfg(test)]
    pub fn wait_settled_for_tests(&self, timeout: std::time::Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .lock_agents()
                .iter()
                .all(|agent| agent.status() != AgentLiveStatus::Running)
            {
                return;
            }
            if Instant::now() >= deadline {
                panic!("池内任务超时未收束（wait_settled_for_tests）");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    pub fn all(&self) -> Vec<Arc<AgentShared>> {
        self.lock_agents().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.lock_agents().is_empty()
    }

    /// True when one more agent may transition to `Running`.
    pub fn can_run_one_more(&self) -> bool {
        self.max_live
            .is_none_or(|limit| Self::running_count(&self.lock_agents()) < limit)
    }

    /// Picks the first free auto name (`a1`, `a2`, …) also avoiding names used
    /// by earlier turns of this conversation.
    pub fn auto_name(&self, taken_elsewhere: &dyn Fn(&str) -> bool) -> String {
        let agents = self.lock_agents();
        for index in 1usize.. {
            let candidate = format!("a{index}");
            if agents.iter().any(|agent| agent.name == candidate) || taken_elsewhere(&candidate) {
                continue;
            }
            return candidate;
        }
        unreachable!("an unused auto name always exists")
    }

    /// Blocks until a watched agent produces an envelope, every watched agent
    /// is already non-running with nothing pending, or the deadline passes.
    /// `heartbeat` runs every poll tick; its error aborts the wait (used to
    /// observe run cancellation). `task_wait` does NOT use this shape — see
    /// `wait_results_until`, which blocks through progress to the result.
    ///
    /// The retired `agent_wait` tool was the last production consumer; every
    /// caller that remains loops, and a loop must hold one absolute deadline
    /// rather than restart a `Duration` budget per iteration, so it calls the
    /// `_until` forms directly. Tests keep this form for the single-shot
    /// waits where a relative timeout reads better.
    #[cfg(test)]
    pub fn wait_activity(
        &self,
        watched: &[Arc<AgentShared>],
        timeout: Duration,
        heartbeat: &dyn Fn() -> Result<(), String>,
    ) -> Result<WaitOutcome, String> {
        self.wait_activity_until(watched, Instant::now() + timeout, heartbeat)
    }

    /// Relative-timeout form of `wait_results_until`, for the same reason
    /// `wait_activity` exists: a single-shot test wait reads better with one.
    #[cfg(test)]
    pub fn wait_results(
        &self,
        watched: &[Arc<AgentShared>],
        timeout: Duration,
        heartbeat: &dyn Fn() -> Result<(), String>,
    ) -> Result<WaitOutcome, String> {
        self.wait_results_until(watched, Instant::now() + timeout, heartbeat)
    }

    /// Absolute-deadline form. A caller that loops must use this: passing a
    /// fresh `Duration` on each iteration restarts the budget, so a child
    /// emitting a steady trickle of updates could hold the parent forever.
    ///
    /// `WaitMode::AnyActivity` returns for any envelope. Its only production
    /// consumer is the workflow driver: envelopes wake it while the authoritative
    /// result remains in the record.
    pub(crate) fn wait_activity_until(
        &self,
        watched: &[Arc<AgentShared>],
        deadline: Instant,
        heartbeat: &dyn Fn() -> Result<(), String>,
    ) -> Result<WaitOutcome, String> {
        self.wait_until(watched, deadline, WaitMode::AnyActivity, heartbeat)
    }

    /// `task_wait` blocks until every observed task supplies a terminal result.
    /// Progress updates do not end the wait; they are drained, accumulated, and
    /// returned with the final outcome so they are not delivered twice.
    ///
    /// It returns when every observed task has a terminal result, every observed
    /// task is non-Running, or the deadline expires. The non-Running path is
    /// necessary because `RunningTurnGuard::drop` can set `Interrupted` without
    /// emitting an envelope.
    pub(crate) fn wait_results_until(
        &self,
        watched: &[Arc<AgentShared>],
        deadline: Instant,
        heartbeat: &dyn Fn() -> Result<(), String>,
    ) -> Result<WaitOutcome, String> {
        self.wait_until(watched, deadline, WaitMode::TerminalResult, heartbeat)
    }

    fn wait_until(
        &self,
        watched: &[Arc<AgentShared>],
        deadline: Instant,
        mode: WaitMode,
        heartbeat: &dyn Fn() -> Result<(), String>,
    ) -> Result<WaitOutcome, String> {
        // Envelopes drained before the wait can end: progress updates and
        // terminal results from staggered siblings. The outbox is their only
        // copy, so every return path must return them; the guard restores any
        // unreturned envelopes during cancellation or panic unwinding.
        let mut buffered = BufferedEnvelopes {
            pool: self,
            watched,
            envelopes: Vec::new(),
        };
        // Observed task names that supplied terminal results during this wait.
        // Accumulate across iterations because staggered sibling results may
        // already be in `buffered`.
        let mut delivered = HashSet::<String>::new();
        loop {
            let drained = watched
                .iter()
                .flat_map(|agent| agent.drain_outbox())
                .collect::<Vec<_>>();
            for envelope in &drained {
                if matches!(envelope.kind, EnvelopeKind::Result(_)) {
                    delivered.insert(envelope.agent.clone());
                }
            }
            let settles = match mode {
                WaitMode::AnyActivity => !drained.is_empty(),
                // An observed task is settled when it emitted a terminal result
                // or is no longer Running. Check the latter per task so a task
                // that can never emit another envelope does not delay the wait.
                WaitMode::TerminalResult => watched.iter().all(|agent| {
                    delivered.contains(&agent.name) || agent.status() != AgentLiveStatus::Running
                }),
            };
            buffered.envelopes.extend(drained);
            if settles {
                if mode == WaitMode::TerminalResult {
                    // Drain again because the prior drain and this status check
                    // are not one snapshot; a result may have reached the outbox
                    // between them.
                    buffered
                        .envelopes
                        .extend(watched.iter().flat_map(|agent| agent.drain_outbox()));
                }
                return Ok(WaitOutcome {
                    envelopes: buffered.take(),
                    timed_out: false,
                });
            }
            if watched
                .iter()
                .all(|agent| agent.status() != AgentLiveStatus::Running)
            {
                // Drain again because `complete_turn` and cancellation-timeout
                // settlement write state and the result envelope in one core
                // critical section, but this drain and the status check are not
                // one snapshot. Once no task is running, only `followup_task`
                // can wake an idle task, so no further envelopes can arrive.
                buffered
                    .envelopes
                    .extend(watched.iter().flat_map(|agent| agent.drain_outbox()));
                return Ok(WaitOutcome {
                    envelopes: buffered.take(),
                    timed_out: false,
                });
            }
            if Instant::now() >= deadline {
                // On expiry, return every envelope already collected. Results
                // and expiry may both be true, allowing the renderer to separate
                // delivered addresses from still-running ones.
                return Ok(WaitOutcome {
                    envelopes: buffered.take(),
                    timed_out: true,
                });
            }
            // A failed heartbeat aborts the round. `buffered` restores drained
            // updates to the outbox because this path returns no outcome and
            // `updates` feeds only `task_list` and records, not the inbox.
            heartbeat()?;
            let (generation, condvar) = &*self.signal;
            let guard = generation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = condvar
                .wait_timeout(guard, WAIT_POLL_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Returns envelopes drained by an aborted wait to their owners' outboxes,
    /// ahead of anything that owner produced since. Order is preserved **per
    /// agent**, which is the order that means anything: a single child's
    /// updates stay in sequence. The interleaving *between* agents is not
    /// restored — the buffer is regrouped by owner, so a later wait sees each
    /// agent's updates in `watched` order rather than in wall-clock order.
    fn restore_envelopes(&self, watched: &[Arc<AgentShared>], envelopes: Vec<AgentEnvelope>) {
        if envelopes.is_empty() {
            return;
        }
        let mut by_agent: HashMap<String, Vec<AgentEnvelope>> = HashMap::new();
        for envelope in envelopes {
            by_agent
                .entry(envelope.agent.clone())
                .or_default()
                .push(envelope);
        }
        for (name, envelopes) in by_agent {
            if let Some(agent) = watched.iter().find(|agent| agent.name == name) {
                agent.restore_envelopes(envelopes);
            }
        }
    }

    /// Ends the owner's lifetime, not a turn or a user task stop. Registration
    /// and retirement linearize under the roster lock. Never joins a worker.
    pub(crate) fn retire(&self) {
        let agents = {
            let agents = self.lock_agents();
            self.retired.store(true, Ordering::Release);
            agents.clone()
        };
        for agent in agents {
            agent.cancel.store(true, Ordering::Release);
        }
        self.notify();
    }

    /// Requests cancellation of every running agent.
    pub fn cancel_all(&self) {
        for agent in self.lock_agents().iter() {
            agent.cancel.store(true, Ordering::Release);
        }
        self.notify();
    }

    /// Cancels stragglers and blocks until no agent is `Running`. Worker
    /// threads observe their flag at the next stream event or round boundary;
    /// the enclosing `thread::scope` then joins them.
    ///
    /// The deadline bounds the SILENT SPIN and restores observability. It does
    /// NOT abandon a worker: the enclosing `std::thread::scope` still joins
    /// every thread regardless. Model HTTP waits observe the flag within a
    /// probe interval (header waits via `send_request_with_cancellation_probes`,
    /// body reads via `BridgedBody`); an approval wait and the two shell/hook
    /// legs of a tool execution poll the same flag too (the approval closure
    /// takes it directly, the other two through
    /// [`crate::cancel::CancelSignal`]). What still holds turn exit is a
    /// synchronous call with no probe of its own — an MCP request, a browser
    /// action. What escalation buys there is a logged reason and a terminal
    /// status on the record instead of an unexplained hang.
    pub fn await_quiescence(&self) {
        self.await_quiescence_until(Instant::now() + QUIESCENCE_DEADLINE);
    }

    fn await_quiescence_until(&self, deadline: Instant) {
        self.cancel_all();
        loop {
            if self
                .lock_agents()
                .iter()
                .all(|agent| agent.status() != AgentLiveStatus::Running)
            {
                return;
            }
            if Instant::now() >= deadline {
                let stuck = self
                    .lock_agents()
                    .iter()
                    .filter(|agent| agent.status() == AgentLiveStatus::Running)
                    .map(|agent| agent.name.clone())
                    .collect::<Vec<_>>();
                eprintln!(
                    "[agents] {} 个子代理在 {:?} 内没有响应取消标志，强制标记终态：{}",
                    stuck.len(),
                    QUIESCENCE_DEADLINE,
                    stuck.join("、")
                );
                // Re-issue the flag in case a worker was mid-round when the
                // first pass ran, then force the status so the record is
                // preserved with a reason rather than lost.
                self.cancel_all();
                for agent in self.lock_agents().iter() {
                    if agent.status() == AgentLiveStatus::Running {
                        agent.settle_after_cancel_timeout();
                    }
                }
                self.notify();
                return;
            }
            let (generation, condvar) = &*self.signal;
            let guard = generation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = condvar
                .wait_timeout(guard, QUIESCENCE_POLL_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn notify(&self) {
        let (generation, condvar) = &*self.signal;
        *generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        condvar.notify_all();
    }

    fn running_count(agents: &[Arc<AgentShared>]) -> usize {
        agents
            .iter()
            .filter(|agent| agent.status() == AgentLiveStatus::Running)
            .count()
    }

    fn validate_registration_availability(
        agents: &[Arc<AgentShared>],
        name: &str,
        status: AgentLiveStatus,
        max_live: Option<usize>,
    ) -> Result<(), String> {
        if agents.iter().any(|agent| agent.name == name) {
            return Err(format!("Subagent name {name} is already in use"));
        }
        if let Some(limit) = max_live {
            if status == AgentLiveStatus::Running && Self::running_count(agents) >= limit {
                return Err(format!(
                    "No more than {limit} subagents can run at once; collect the finished ones with task_wait first"
                ));
            }
        }
        Ok(())
    }

    fn lock_agents(&self) -> std::sync::MutexGuard<'_, Vec<Arc<AgentShared>>> {
        self.agents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Enforces the ordinary / conversation-fork / trusted-named-agent boundary at
/// the last shared registration point before a child can run or be persisted.
fn validate_child_memory_mode(template: &RunModelRequest) -> Result<(), String> {
    let inherits_parent = template.inherits_parent_model_memory;
    let fork_binding = template.fork_model_binding.as_ref();
    let named_binding = template.agent_definition_binding.as_ref();

    if named_binding.is_some() && (inherits_parent || fork_binding.is_some()) {
        return Err("A named subagent cannot inherit parent model memory or carry a conversation-fork binding".into());
    }
    if inherits_parent != fork_binding.is_some() {
        return Err("A conversation fork must have both an explicit inheritance marker and a host-generated exact model binding".into());
    }
    if let Some(binding) = fork_binding {
        if binding.provider_id != template.provider.id || binding.model_id != template.model.id {
            return Err("The conversation fork's exact provider/model binding does not match the child request".into());
        }
        if binding.system_prompt_snapshot.is_empty()
            || binding.system_prompt_snapshot.len() > 1024 * 1024
            || !crate::model::is_lower_hex_digest(&binding.system_prompt_receipt)
        {
            return Err("The conversation fork's system-prompt snapshot or receipt has an invalid format".into());
        }
        if let Some(receipt) = binding.memory_snapshot_receipt.as_deref() {
            if !crate::model::is_lower_hex_digest(receipt) {
                return Err("The conversation fork's memory-snapshot receipt has an invalid format".into());
            }
        }
    }
    Ok(())
}

fn merge_shared_usage(total: &mut ModelUsage, delta: &ModelUsage) {
    let add = |total: &mut Option<u64>, delta: Option<u64>| {
        if let Some(delta) = delta {
            *total = Some(total.unwrap_or(0) + delta);
        }
    };
    add(&mut total.input_tokens, delta.input_tokens);
    add(&mut total.cached_input_tokens, delta.cached_input_tokens);
    add(&mut total.output_tokens, delta.output_tokens);
    add(&mut total.total_tokens, delta.total_tokens);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::model::{
        AgentDefinitionBinding, AgentDefinitionMemory, AgentDefinitionSource, ProviderFamily,
        ApiProvider, ForkModelBinding, ResolvedLanguage, ModelProfile, SecurityLevel,
    };

    /// The derivation rule the trusted request builder applies, in isolation.
    ///
    /// `trusted_run_request` itself is `#[cfg(not(test))]`, so this is the only
    /// place the rule can be pinned. Three properties matter and each has bitten
    /// the memory tiers at some point: a stale persisted name must not survive a
    /// conversation with no producer, the pair must appear even when the
    /// renderer never wrote it, and re-deriving twice must not duplicate.
    #[test]
    fn task_runtime_tools_are_stripped_then_derived_from_producers() {
        let derive = |names: &[&str]| {
            let mut enabled = names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
            apply_task_runtime_tools(&mut enabled);
            enabled
        };

        // No producer: a persisted pair is removed rather than honoured.
        assert_eq!(derive(&["read", "task_wait", "task_list"]), vec!["read"]);
        assert_eq!(derive(&[]), Vec::<String>::new());

        // Every producer derives the pair on its own, without the renderer
        // having to name it. The list is the address space of `TaskRef`.
        for producer in TASK_PRODUCING_TOOL_NAMES {
            assert_eq!(
                derive(&[producer]),
                vec![producer.to_owned(), "task_wait".to_owned(), "task_list".to_owned()],
                "{producer} must derive the task-runtime pair"
            );
        }

        // A non-producer never derives it, however many are enabled.
        assert_eq!(derive(&["read", "write", "ls", "grep"]), vec!["read", "write", "ls", "grep"]);
        assert_eq!(
            derive(&["web_search", "web_fetch"]),
            vec!["web_search", "web_fetch"]
        );

        // Idempotent: deriving over an already-derived list adds nothing.
        let mut enabled = derive(&["agent_spawn"]);
        apply_task_runtime_tools(&mut enabled);
        assert_eq!(enabled, vec!["agent_spawn", "task_wait", "task_list"]);
    }

    fn template() -> RunModelRequest {
        RunModelRequest {
            provider: ApiProvider {
                id: "p".into(),
                name: "P".into(),
                enabled: true,
                family: ProviderFamily::OpenaiResponses,
                base_url: "http://127.0.0.1:1".into(),
                family_settings: Default::default(),
                endpoint_base_urls: Default::default(),
                notes: String::new(),
                models: Vec::new(),
                active_model_id: None,
            },
            web_search: Default::default(),
            native_search_call: false,
            run_environment: Default::default(),
            prompt_profile: Default::default(),
            global_memory_enabled: false,
            project_memory_enabled: false,
            skills: Vec::new(),
            model: ModelProfile {
                id: "m".into(),
                name: String::new(),
                group: String::new(),
                context_window: None,
                max_output_tokens: None,
                capabilities: Default::default(),
                reasoning_content: Default::default(),
            },
            reasoning_effort: Default::default(),
            conversation_id: "conv".into(),
            workspace_id: "workspace".into(),
            memory_context_id: None,
            project_memory_context_id: None,
            agent_definition_binding: None,
            inherits_parent_model_memory: false,
            fork_model_binding: None,
            subagent_execution_mode_receipt: None,
            subagent_reserved_names: Vec::new(),
            memory_run_id: None,
            context_load_actor_name: Some("test-agent".into()),
            workspace_path: ".".into(),
            system_prompt: String::new(),
            enabled_tools: Vec::new(),
            contexts: Vec::new(),
            ephemeral_contexts: Vec::new(),
            tools: Vec::new(),
            active_hooks: Vec::new(),
            security_level: SecurityLevel::FullAccess,
            app_data_path: ".".into(),
            mcp_servers: Vec::new(),
            mcp_bindings: Vec::new(),
            subagent_depth: 1,
            request_id: String::new(),
                subagent_name: None,
                subagent_call_id: None,
            agent_mailbox: AgentMailboxHandle::default(),
            steer_mailbox: AgentMailboxHandle::default(),
            task_cancel: crate::cancel::CancelSignal::default(),
            run_cancel: crate::cancel::CancelSignal::default(),
            output_schema: None,
        }
    }

    fn register_result(
        pool: &AgentPool,
        name: &str,
        status: AgentLiveStatus,
    ) -> Result<Arc<AgentShared>, String> {
        let template = template();
        pool.register(
            name.into(),
            name.into(),
            "task".into(),
            template,
            SubagentRunKind::General,
            &AppState::default(),
            Vec::new(),
            Vec::new(),
            status,
            format!("call-{name}"),
            None,
        )
    }

    fn register(pool: &AgentPool, name: &str, status: AgentLiveStatus) -> Arc<AgentShared> {
        let template = template();
        pool.register(
            name.into(),
            name.into(),
            "task".into(),
            template,
            SubagentRunKind::General,
            &AppState::default(),
            Vec::new(),
            Vec::new(),
            status,
            format!("call-{name}"),
            None,
        )
        .unwrap()
    }

    fn register_template(
        pool: &AgentPool,
        name: &str,
        template: RunModelRequest,
    ) -> Arc<AgentShared> {
        pool.register(
            name.into(),
            name.into(),
            "task".into(),
            template,
            SubagentRunKind::General,
            &AppState::default(),
            Vec::new(),
            Vec::new(),
            AgentLiveStatus::Idle,
            format!("call-{name}"),
            None,
        )
        .unwrap()
    }

    #[test]
    fn retirement_racing_registration_never_leaves_an_uncancelled_task() {
        for _ in 0..16 {
            let pool = Arc::new(AgentPool::new());
            let start = Arc::new(std::sync::Barrier::new(2));
            let worker_pool = Arc::clone(&pool);
            let worker_start = Arc::clone(&start);
            let worker = std::thread::spawn(move || {
                worker_start.wait();
                register_result(&worker_pool, "racing", AgentLiveStatus::Running)
            });
            start.wait();
            pool.retire();
            if let Ok(agent) = worker.join().unwrap() {
                assert!(agent.cancel.load(Ordering::Acquire));
            }
            assert!(pool.preflight_registration("late", &template(), AgentLiveStatus::Running).is_err());
            assert!(register_result(&pool, "late", AgentLiveStatus::Running).is_err());
        }
    }

    #[test]
    fn wake_snapshot_peeks_envelope_identity_without_claiming_results() {
        let pool = AgentPool::new();
        let agent = register(&pool, "a1", AgentLiveStatus::Running);
        let settled_identity = agent.identity();
        agent.complete_turn(settled_identity, Vec::new(), "done".into(),
            AgentLiveStatus::Idle, &ModelUsage::default(), 1, None);
        assert!(agent.begin_turn("followup"));
        assert_ne!(agent.identity(), settled_identity);
        let expected = vec![("a1".to_owned(), settled_identity)];
        assert_eq!(pool.undelivered_result_identities(), expected);
        assert_eq!(pool.undelivered_result_identities(), expected);
        assert_eq!(pool.take_undelivered_results().len(), 1);
        assert!(pool.undelivered_result_identities().is_empty());
    }

    #[test]
    fn deletion_blocks_late_settlement_and_old_pool_registration() {
        let directory = tempfile::tempdir().unwrap();
        let anchor = directory.path().join("document.v1.json");
        let state = AppState::default();
        let document = crate::catalog::default_document(directory.path());
        state.document_store.acquire_process_authority(&anchor).unwrap();
        state.document_store.commit(&anchor, document.clone()).unwrap();
        let workspace = &document.workspaces[0];
        let conversation = &workspace.conversations[0];
        crate::conversations::store(&anchor).unwrap()
            .put_conversation(&workspace.id, conversation).unwrap();
        let tasks = state.conversation_tasks(&conversation.id);
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = Arc::clone(&wakes);
        state.push_events.subscribe(tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                let event: Value = serde_json::from_str(&json)?;
                if event["type"] == "taskSettled" {
                    received.fetch_add(1, Ordering::SeqCst);
                }
            }
            Ok(())
        }));
        let running = register(&tasks.pool, "running", AgentLiveStatus::Running);
        let done = register(&tasks.pool, "done", AgentLiveStatus::Running);
        done.complete_turn(done.identity(), Vec::new(), "earlier".into(),
            AgentLiveStatus::Idle, &ModelUsage::default(), 1, None);
        assert!(tasks.pool.has_undrained_foldable_results());
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "observer must be live before deletion");
        let other = state.conversation_tasks("survivor");
        let survivor = register(&other.pool, "survivor", AgentLiveStatus::Running);
        let (release, wait) = std::sync::mpsc::channel();
        let late = Arc::clone(&running);
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            late.complete_turn(late.identity(), Vec::new(), "late".into(),
                AgentLiveStatus::Idle, &ModelUsage::default(), 1, None);
        });
        crate::conversations::delete(&state, &anchor, &workspace.id, &conversation.id).unwrap();
        assert!(running.cancel.load(Ordering::Acquire));
        assert!(!survivor.cancel.load(Ordering::Acquire));
        release.send(()).unwrap();
        worker.join().unwrap();
        state.recheck_task_wake(&conversation.id);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "late completion must not wake a deleted owner");
        assert!(!state.wake_pending_conversations().contains(&conversation.id));
        assert!(register_result(&tasks.pool, "late", AgentLiveStatus::Running).is_err());
        assert!(!done.begin_turn("late-followup"));
        assert!(!running.begin_turn("late-followup"));
    }

    #[test]
    fn agent_names_are_validated_as_short_slugs() {
        assert!(validate_agent_name("a1").is_ok());
        assert!(validate_agent_name("review-api_2").is_ok());
        assert!(validate_agent_name("").is_err());
        assert!(validate_agent_name("1a").is_err());
        assert!(validate_agent_name("Agent").is_err());
        assert!(validate_agent_name("名字").is_err());
        assert!(validate_agent_name(&"a".repeat(33)).is_err());
    }

    /// Task-address resolution accepts only managed tasks. An ordinary agent's
    /// free-form label may resemble `workflow:<runId>`, but must never cause a
    /// task-sidebar stop control to stop that agent.
    #[test]
    fn a_task_address_lookup_never_matches_an_ordinary_agents_display_label() {
        let pool = AgentPool::new();
        // An ordinary agent whose display label resembles a task address.
        pool.register(
            "a1".into(),
            "workflow:decoy".into(),
            "task".into(),
            template(),
            SubagentRunKind::General,
            &AppState::default(),
            Vec::new(),
            Vec::new(),
            AgentLiveStatus::Running,
            "call-decoy".into(),
            None,
        )
        .unwrap();
        // A genuine managed-task entry.
        pool.register(
            "a2".into(),
            "workflow:real".into(),
            "task".into(),
            template(),
            SubagentRunKind::WorkflowStep,
            &AppState::default(),
            Vec::new(),
            Vec::new(),
            AgentLiveStatus::Running,
            "call-real".into(),
            None,
        )
        .unwrap();

        assert!(
            pool.find_by_label("workflow:decoy").is_none(),
            "普通子代理的展示名不是任务地址"
        );
        let found = pool
            .find_by_label("workflow:real")
            .expect("托管任务必须按地址可寻");
        assert_eq!(found.name, "a2");
        // Ordinary agents remain addressable by name.
        assert!(pool.find("a1").is_some());
    }

    /// Only terminal settlement that passes the incumbent guard notifies the
    /// observer. A stale return from a superseded worker must not wake an idle
    /// conversation.
    #[test]
    fn settle_observer_fires_only_for_applied_terminals() {
        let pool = AgentPool::new();
        let observed: Arc<Mutex<Vec<(String, SubagentRunStatus)>>> = Arc::default();
        let sink = Arc::clone(&observed);
        pool.set_settle_observer(Arc::new(move |name, status| {
            sink.lock().unwrap().push((name.to_owned(), status));
        }));
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let incumbent = shared.identity();
        shared.complete_turn(
            incumbent,
            Vec::new(),
            "done".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            1,
            None,
        );
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[("a1".to_owned(), SubagentRunStatus::Completed)]
        );

        // The incumbent guard discards the superseded incarnation's late result;
        // it must not notify the observer again.
        shared.followups.push("next".into());
        assert!(shared.begin_turn("call-2"));
        shared.complete_turn(
            incumbent,
            Vec::new(),
            "stale".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            1,
            None,
        );
        assert_eq!(observed.lock().unwrap().len(), 1);
    }

    /// A panicking worker must leave a Failed envelope with a reason and notify
    /// the settlement observer. It must not remain Running or become envelope-free
    /// `Interrupted`, which would misreport a host fault as a user stop.
    #[test]
    fn panicking_worker_settles_as_failed_with_envelope() {
        let pool = AgentPool::new();
        let observed: Arc<Mutex<Vec<(String, SubagentRunStatus)>>> = Arc::default();
        let sink = Arc::clone(&observed);
        pool.set_settle_observer(Arc::new(move |name, status| {
            sink.lock().unwrap().push((name.to_owned(), status));
        }));
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let worker_shared = Arc::clone(&shared);
        let join = std::thread::spawn(move || {
            let _guard = RunningTurnGuard::new(worker_shared);
            panic!("boom");
        });
        assert!(join.join().is_err());
        assert_eq!(shared.status(), AgentLiveStatus::Failed);
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[("a1".to_owned(), SubagentRunStatus::Failed)]
        );
        let envelopes = shared.drain_outbox();
        assert_eq!(envelopes.len(), 1);
        assert!(matches!(
            envelopes[0].kind,
            EnvelopeKind::Result(SubagentRunStatus::Failed)
        ));
        assert!(envelopes[0].content.contains("panic"));
    }

    /// Cleanup after a terminal result must not overwrite it; a second envelope
    /// or repeated observer notification is prohibited.
    #[test]
    fn panic_after_terminal_state_does_not_overwrite() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "done".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            0,
            None,
        );
        let worker_shared = Arc::clone(&shared);
        let join = std::thread::spawn(move || {
            let _guard = RunningTurnGuard::new(worker_shared);
            panic!("late boom");
        });
        assert!(join.join().is_err());
        assert_eq!(shared.status(), AgentLiveStatus::Idle);
        let envelopes = shared.drain_outbox();
        assert_eq!(envelopes.len(), 1);
        assert!(matches!(
            envelopes[0].kind,
            EnvelopeKind::Result(SubagentRunStatus::Completed)
        ));
    }

    /// The persisted record must carry a schema-bound run's raw schema
    /// document: cross-turn rehydration re-compiles it, so dropping it here is
    /// silently dropping the constraint on every later continuation.
    #[test]
    fn the_record_persists_the_output_schema_document() {
        let pool = AgentPool::new();
        let document = serde_json::json!({
            "type": "object",
            "properties": {"verdict": {"type": "string"}},
            "required": ["verdict"],
            "additionalProperties": false
        });
        let mut bound = template();
        bound.output_schema = Some(crate::subagent_schema::compile(&document).unwrap());
        let shared = register_template(&pool, "a1", bound);
        assert_eq!(shared.record().output_schema, Some(document));

        let unbound = register(&pool, "a2", AgentLiveStatus::Idle);
        assert_eq!(unbound.record().output_schema, None);
    }

    /// S9 requires a one-shot claim: the fold takes terminal results exactly
    /// once and leaves progress updates for a later `task_wait`. Every terminal
    /// status is claimed and every result is deliverable — including the
    /// `Interrupted` a task-level stop produces.
    #[test]
    fn undrained_terminal_results_are_claimed_once_and_updates_stay() {
        let pool = AgentPool::new();
        let done = register(&pool, "a1", AgentLiveStatus::Running);
        done.push_update(done.identity(), "进展 1".into(), "2026-07-14T00:00:00Z".into());
        done.complete_turn(
            done.identity(),
            Vec::new(),
            "最终结论".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            0,
            None,
        );
        let interrupted = register(&pool, "a2", AgentLiveStatus::Running);
        interrupted.complete_turn(
            interrupted.identity(),
            Vec::new(),
            "（子代理运行被中断）".into(),
            AgentLiveStatus::Interrupted,
            &ModelUsage::default(),
            0,
            None,
        );
        // A genuine failure — a preflight rejection, a provider error — is a
        // deliverable, like every other terminal result.
        let failed = register(&pool, "a3", AgentLiveStatus::Running);
        failed.complete_turn(
            failed.identity(),
            Vec::new(),
            "（命名 Agent 运行前校验失败：定义已被禁用）".into(),
            AgentLiveStatus::Failed,
            &ModelUsage::default(),
            0,
            None,
        );

        let claimed = pool.take_undelivered_results();
        assert_eq!(claimed.len(), 3);
        assert_eq!(claimed[0].agent, "a1");
        assert!(matches!(
            claimed[0].kind,
            EnvelopeKind::Result(SubagentRunStatus::Completed)
        ));
        assert_eq!(claimed[1].agent, "a2");
        assert!(matches!(
            claimed[1].kind,
            EnvelopeKind::Result(SubagentRunStatus::Interrupted)
        ));
        assert_eq!(claimed[2].agent, "a3");
        assert!(matches!(
            claimed[2].kind,
            EnvelopeKind::Result(SubagentRunStatus::Failed)
        ));
        assert!(
            pool.take_undelivered_results().is_empty(),
            "the claim is one-shot"
        );

        // The update stayed behind for `task_wait` — the fold's only
        // exclusion; the interrupted result was claimed for delivery.
        let outcome = pool
            .wait_activity(&pool.all(), Duration::from_secs(5), &|| Ok(()))
            .unwrap();
        assert_eq!(outcome.envelopes.len(), 1);
        assert!(matches!(outcome.envelopes[0].kind, EnvelopeKind::Update));

        // And whatever `task_wait` drained can never be folded afterwards.
        assert!(pool.take_undelivered_results().is_empty());
    }

    #[test]
    fn registration_enforces_unique_names_and_the_live_cap() {
        let pool = AgentPool::new();
        register(&pool, "a1", AgentLiveStatus::Running);
        let duplicate_template = template();
        assert!(pool
            .register(
                "a1".into(),
                "a1".into(),
                "task".into(),
                duplicate_template,
                SubagentRunKind::General,
                &AppState::default(),
                Vec::new(),
                Vec::new(),
                AgentLiveStatus::Idle,
                "call".into(),
                None,
            )
            .unwrap_err()
            .contains("already in use"));
        for index in 2..=MAX_LIVE_AGENTS {
            register(&pool, &format!("a{index}"), AgentLiveStatus::Running);
        }
        assert!(!pool.can_run_one_more());
        let overflow_template = template();
        let error = pool
            .register(
                "overflow".into(),
                "overflow".into(),
                "task".into(),
                overflow_template,
                SubagentRunKind::General,
                &AppState::default(),
                Vec::new(),
                Vec::new(),
                AgentLiveStatus::Running,
                "call".into(),
                None,
            )
            .unwrap_err();
        assert!(error.contains("No more than"));
        // Idle agents do not count against the running cap.
        register(&pool, "idle", AgentLiveStatus::Idle);
    }

    /// S7 (g) requires the cap at `validate_registration_availability`, which
    /// `preflight_registration` reaches before the caller signs an
    /// execution-mode receipt. A caller-side permit counter over `unbounded()`
    /// would let the refusal land after the reservation row was already written.
    #[test]
    fn a_custom_live_limit_is_enforced_at_preflight_not_only_at_register() {
        let pool = AgentPool::with_live_limit(3);
        assert_eq!(pool.live_limit(), Some(3));
        for index in 1..=3 {
            register(&pool, &format!("a{index}"), AgentLiveStatus::Running);
        }
        assert!(!pool.can_run_one_more());

        let error = pool
            .preflight_registration("a4", &template(), AgentLiveStatus::Running)
            .unwrap_err();
        assert!(error.contains("No more than 3 subagents"), "{error}");

        // Raised, not lifted: the same pool still admits an idle registration,
        // and a larger cap admits what the default would have refused.
        register(&pool, "idle", AgentLiveStatus::Idle);
        let raised = AgentPool::with_live_limit(MAX_LIVE_AGENTS + 8);
        for index in 1..=MAX_LIVE_AGENTS + 8 {
            register(&raised, &format!("b{index}"), AgentLiveStatus::Running);
        }
        assert!(!raised.can_run_one_more());

        // A zero request is raised to one rather than producing a pool that can
        // never run anything.
        let floor = AgentPool::with_live_limit(0);
        assert_eq!(floor.live_limit(), Some(1));
        register(&floor, "c1", AgentLiveStatus::Running);
        assert!(floor
            .preflight_registration("c2", &template(), AgentLiveStatus::Running)
            .is_err());
    }

    /// A raised cap is RAISED, not removed: the ninth concurrent agent (which
    /// the default cap of 8 refuses) registers, and the seventeenth still
    /// fails softly with the Chinese cap message. `workflow.rs` sizes its
    /// private pool this way.
    #[test]
    fn a_raised_live_cap_admits_nine_and_refuses_seventeen() {
        const RAISED: usize = 16;
        let pool = AgentPool::with_live_limit(RAISED);
        for index in 1..=MAX_LIVE_AGENTS {
            register(&pool, &format!("a{index}"), AgentLiveStatus::Running);
        }
        // The ninth registration would have been refused by the default cap.
        register(&pool, "a9", AgentLiveStatus::Running);
        for index in MAX_LIVE_AGENTS + 2..=RAISED {
            register(&pool, &format!("a{index}"), AgentLiveStatus::Running);
        }
        assert!(!pool.can_run_one_more());
        let error = pool
            .preflight_registration("a17", &template(), AgentLiveStatus::Running)
            .unwrap_err();
        assert!(error.contains("No more than 16 subagents"), "{error}");
    }

    #[test]
    fn auto_names_skip_registry_and_history() {
        let pool = AgentPool::new();
        register(&pool, "a1", AgentLiveStatus::Idle);
        let history = |name: &str| name == "a2";
        assert_eq!(pool.auto_name(&history), "a3");
    }

    #[test]
    fn begin_turn_claims_exclusively_and_guard_interrupts_on_unwind() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Idle);
        assert!(shared.begin_turn("call-2"));
        assert!(!shared.begin_turn("call-3"));
        assert_eq!(shared.latest_call_id(), "call-2");

        let guard = RunningTurnGuard::new(Arc::clone(&shared));
        drop(guard);
        assert_eq!(shared.status(), AgentLiveStatus::Interrupted);

        assert!(shared.begin_turn("call-4"));
        RunningTurnGuard::new(Arc::clone(&shared)).defuse();
        assert_eq!(shared.status(), AgentLiveStatus::Running);
    }

    #[test]
    fn wait_returns_immediately_when_nothing_can_produce_activity() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Idle);
        let outcome = pool
            .wait_activity(&[Arc::clone(&shared)], Duration::from_secs(30), &|| Ok(()))
            .unwrap();
        assert!(!outcome.timed_out);
        assert!(outcome.envelopes.is_empty());
    }

    /// `register` mints the first identity, each successful `begin_turn` mints
    /// a successor, and envelopes copy their identity when produced. A late
    /// envelope therefore retains its originating generation.
    #[test]
    fn task_identity_rotates_per_incarnation_and_stamps_envelopes() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let first = shared.identity();
        assert_eq!(first.generation, 1);
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "第一代结论".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            1,
            None,
        );
        shared.followups.push("再深入一层".into());
        assert!(shared.begin_turn("call-2"));
        let second = shared.identity();
        assert_eq!(second.generation, 2);
        assert_ne!(first, second, "化身更替必须重铸身份");
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "第二代结论".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            1,
            None,
        );
        let envelopes = shared.take_undelivered_results();
        assert_eq!(envelopes.len(), 2);
        assert_eq!(envelopes[0].identity, first, "旧代信封归旧代");
        assert_eq!(envelopes[1].identity, second, "新代信封归新代");
    }

    /// Cancellation-timeout settlement also stamps its terminal envelope with
    /// the incumbent identity.
    #[test]
    fn forced_settlement_stamps_the_current_incarnation() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let identity = shared.identity();
        shared.settle_after_cancel_timeout();
        let envelopes = shared.drain_outbox();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].identity, identity);
    }

    /// A late `complete_turn` after cancellation-timeout settlement must not
    /// overwrite terminal state or emit a second envelope for the incarnation.
    /// Its transcript and usage remain recorded.
    #[test]
    fn late_result_after_forced_settlement_keeps_the_stopped_verdict() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let incarnation = shared.identity();
        shared.settle_after_cancel_timeout();
        assert_eq!(shared.status(), AgentLiveStatus::Stopped);

        shared.complete_turn(
            incarnation,
            vec![ContextItem::Assistant {
                id: "ctx_late".into(),
                content: "迟到的结论".into(),
                round: None,
                model_turn_id: None,
                interrupted: false,
                sources: Vec::new(),
                created_at: "2026-08-13T00:00:00Z".into(),
            }],
            "迟到的结论".into(),
            AgentLiveStatus::Interrupted,
            &ModelUsage {
                total_tokens: Some(7),
                ..Default::default()
            },
            5,
            None,
        );
        assert_eq!(shared.status(), AgentLiveStatus::Stopped, "既定终态不得被推翻");
        let envelopes = shared.drain_outbox();
        assert_eq!(envelopes.len(), 1, "只允许强制终态化那只信封:{envelopes:?}");
        assert!(matches!(
            envelopes[0].kind,
            EnvelopeKind::Result(SubagentRunStatus::Stopped)
        ));
        // Actual output and cost remain accounted for.
        assert_eq!(shared.record().contexts.len(), 1);
        assert_eq!(shared.lifetime_usage().total_tokens, Some(7));
    }

    /// A late `complete_turn` from an incarnation superseded by a follow-up
    /// must not stamp its result with the successor identity or rewind the
    /// successor's Running state and transcript.
    #[test]
    fn stale_worker_result_after_followup_is_discarded() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let old_incarnation = shared.identity();
        // Cancellation-timeout settlement terminates the old incarnation before
        // a follow-up wakes its successor.
        shared.settle_after_cancel_timeout();
        shared.followups.push("再试一次".into());
        assert!(shared.begin_turn("call-2"));
        let new_incarnation = shared.identity();
        assert_ne!(old_incarnation, new_incarnation);

        // The incumbent guard rejects every write from the stale worker.
        shared.complete_turn(
            old_incarnation,
            vec![ContextItem::Assistant {
                id: "ctx_orphan".into(),
                content: "孤儿结论".into(),
                round: None,
                model_turn_id: None,
                interrupted: false,
                sources: Vec::new(),
                created_at: "2026-08-13T00:00:00Z".into(),
            }],
            "孤儿结论".into(),
            AgentLiveStatus::Idle,
            &ModelUsage {
                total_tokens: Some(3),
                ..Default::default()
            },
            5,
            None,
        );
        assert_eq!(shared.status(), AgentLiveStatus::Running, "新化身不得被回卷");
        let envelopes = shared.drain_outbox();
        assert_eq!(envelopes.len(), 1, "不得出现盖着新代身份的孤儿信封");
        assert_eq!(envelopes[0].identity, old_incarnation);
        assert!(shared.record().contexts.is_empty(), "孤儿转写不得混入新化身");
        // Tokens were actually spent and remain accounted for.
        assert_eq!(shared.lifetime_usage().total_tokens, Some(3));

        // A stale worker's cleanup guard must not affect the successor.
        drop(RunningTurnGuard {
            shared: Arc::clone(&shared),
            incarnation: old_incarnation,
            defused: false,
        });
        assert_eq!(shared.status(), AgentLiveStatus::Running);

        // Progress from the stale incarnation is rejected as well.
        shared.push_update(old_incarnation, "旧代进度".into(), "2026-08-13T00:00:01Z".into());
        assert!(shared.drain_outbox().is_empty());
    }

    /// Forced terminal settlement is a deliverable result like any other: its
    /// reason rides the envelope to the model, and it raises the same settlement
    /// edge as `complete_turn` so an idle conversation wakes for it.
    #[test]
    fn forced_settlement_delivers_its_reason_and_raises_the_wake_edge() {
        let pool = AgentPool::new();
        let observed: Arc<Mutex<Vec<(String, SubagentRunStatus)>>> = Arc::default();
        let sink = Arc::clone(&observed);
        pool.set_settle_observer(Arc::new(move |name, status| {
            sink.lock().unwrap().push((name.to_owned(), status));
        }));
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.settle_after_cancel_timeout();
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[("a1".to_owned(), SubagentRunStatus::Stopped)],
            "强制停止也必须发结算边沿，否则空闲会话永远等不到唤醒"
        );
        assert!(
            pool.has_undrained_foldable_results(),
            "强制停止的结果同样要求一轮交付"
        );
        let claimed = shared.take_undelivered_results();
        assert_eq!(claimed.len(), 1);
        assert!(claimed[0].content.contains("did not wind down"), "{}", claimed[0].content);
        assert!(claimed[0].content.contains("followup_task"), "{}", claimed[0].content);
    }

    /// Delivery-round probing counts every terminal result. A task the user
    /// closed owes the same delivery round as one that finished on its own.
    #[test]
    fn foldable_results_probe_counts_every_terminal_status() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        assert!(!pool.has_undrained_foldable_results());

        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "被停".into(),
            AgentLiveStatus::Stopped,
            &ModelUsage::default(),
            1,
            None,
        );
        assert!(pool.has_undrained_foldable_results(), "停止结果也要求交付轮");
        pool.take_undelivered_results();

        shared.followups.push("继续".into());
        assert!(shared.begin_turn("call-2"));
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "被断".into(),
            AgentLiveStatus::Interrupted,
            &ModelUsage::default(),
            1,
            None,
        );
        assert!(pool.has_undrained_foldable_results(), "中断结果也要求交付轮");
        pool.take_undelivered_results();

        shared.followups.push("再来".into());
        assert!(shared.begin_turn("call-3"));
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "完成".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            1,
            None,
        );
        assert!(pool.has_undrained_foldable_results(), "done 结果必须触发交付轮");

        pool.take_undelivered_results();
        assert!(!pool.has_undrained_foldable_results(), "认领后探针归零");
    }

    /// Only the user's sidebar close may put words in the user's mouth. A host
    /// stop — a workflow driver skipping a step or dropping a race loser — sets
    /// the same cancel flag but must never make the result claim a person did it.
    #[test]
    fn a_host_stop_does_not_claim_the_user_closed_the_task() {
        let pool = AgentPool::new();
        let host_stopped = register(&pool, "a1", AgentLiveStatus::Running);
        host_stopped.request_stop(StopOrigin::Host);
        assert!(host_stopped.cancel.load(Ordering::Acquire), "仍然要真的取消");
        assert!(!host_stopped.stopped_by_user(), "宿主调度的取消不是用户关闭");

        let user_stopped = register(&pool, "a2", AgentLiveStatus::Running);
        user_stopped.request_stop(StopOrigin::User);
        assert!(user_stopped.cancel.load(Ordering::Acquire));
        assert!(user_stopped.stopped_by_user());
    }

    #[test]
    fn single_task_stop_reclaims_capacity_without_pool_quiescence() {
        let pool = AgentPool::new();
        let agents: Vec<_> = (0..MAX_LIVE_AGENTS)
            .map(|index| register(&pool, &format!("a{index}"), AgentLiveStatus::Running))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        pool.set_settle_observer(Arc::new(move |_, status| { tx.send(status).unwrap(); }));
        let stubborn = &agents[0];
        let identity = stubborn.identity();
        stubborn.stop_task_with_grace(StopOrigin::User, Duration::from_millis(10));
        // A duplicate must neither extend the deadline nor create another result.
        stubborn.stop_task_with_grace(StopOrigin::User, Duration::from_secs(60));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), SubagentRunStatus::Stopped);
        assert!(stubborn.stopped_by_user());
        assert_eq!(stubborn.status(), AgentLiveStatus::Stopped);
        assert!(register_result(&pool, "replacement", AgentLiveStatus::Running).is_ok());
        stubborn.complete_turn(identity, Vec::new(), "late worker".into(), AgentLiveStatus::Idle,
            &ModelUsage::default(), 1, None);
        assert_eq!(stubborn.status(), AgentLiveStatus::Stopped);
        assert_eq!(stubborn.take_undelivered_results().len(), 1);
        assert!(rx.try_recv().is_err());
        for untouched in &agents[1..] {
            assert_eq!(untouched.status(), AgentLiveStatus::Running);
        }
    }

    #[test]
    fn stale_stop_timer_cannot_settle_a_new_incarnation() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let old = shared.identity();
        shared.request_stop(StopOrigin::User);
        shared.complete_turn(old, Vec::new(), "cooperative exit".into(), AgentLiveStatus::Stopped,
            &ModelUsage::default(), 1, None);
        shared.take_undelivered_results();
        shared.settle_cancelled_incarnation(Some(old));
        assert!(shared.take_undelivered_results().is_empty());
        assert!(shared.begin_turn("next"));
        // Even if the new incarnation is cancelled too, the old timer may not
        // consume its independently granted grace period.
        shared.request_stop(StopOrigin::User);
        shared.settle_cancelled_incarnation(Some(old));
        assert_eq!(shared.status(), AgentLiveStatus::Running);
        assert!(shared.take_undelivered_results().is_empty());
        shared.settle_cancelled_incarnation(Some(shared.identity()));
        assert_eq!(shared.status(), AgentLiveStatus::Stopped);
        assert_eq!(shared.take_undelivered_results().len(), 1);
    }

    #[test]
    fn checkpoint_transcript_holds_incarnation_and_owner_until_persistence_returns() {
        let pool = AgentPool::new();
        let shared = register(&pool, "lock", AgentLiveStatus::Running);
        shared.record_call_context("call-lock", "owner-card");
        let live = ContextItem::Assistant { id: "round-copy".into(), content: "settled round".into(),
            round: Some(1), model_turn_id: None, interrupted: false, sources: Vec::new(),
            created_at: "2026-09-05T00:00:00Z".into() };
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| shared.checkpoint_transcript(shared.identity(), &[live.clone(), live], |owner, record| {
                assert_eq!(owner, "owner-card");
                assert_eq!(record.status, SubagentRunStatus::Interrupted);
                assert_eq!(record.contexts.len(), 1, "the recovery copy deduplicates context ids");
                entered_tx.send(()).unwrap();
                release.wait();
                true
            }));
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let identity_locked = shared.core.try_lock().is_err();
            release.wait();
            assert_eq!(worker.join().unwrap(), Some(true));
            assert!(identity_locked, "a successor cannot pass the core guard while an older write is in flight");
        });
        assert_eq!(shared.status(), AgentLiveStatus::Running);
        assert!(shared.transcript().is_empty());
        assert!(shared.take_undelivered_results().is_empty());
    }

    #[test]
    fn quiescence_escalates_instead_of_spinning_silently() {
        let pool = AgentPool::new();
        let stubborn = register(&pool, "a1", AgentLiveStatus::Running);
        stubborn.push_update(stubborn.identity(), "有进展".into(), "2026-07-28T00:00:00Z".into());
        stubborn.drain_outbox();

        // This worker never observes its cancel flag — the case that used to
        // spin the finalize barrier forever with nothing visible.
        let started = Instant::now();
        pool.await_quiescence_until(started + Duration::from_millis(200));
        let elapsed = started.elapsed();

        assert!(elapsed < Duration::from_secs(2), "spun for {elapsed:?}");
        assert!(stubborn.cancel.load(Ordering::Acquire), "cancel re-issued");
        assert_eq!(stubborn.status(), AgentLiveStatus::Stopped);
        // Escalation forces a terminal status; it must never drop the record.
        let record = stubborn.record();
        assert_eq!(record.status, SubagentRunStatus::Stopped);
        // Existing progress survives. The stop reason is not copied into
        // `updates`: it rides the deliverable result envelope to the model.
        assert_eq!(record.updates.len(), 1);
        assert_eq!(record.updates[0].content, "有进展");
        let claimed = stubborn.take_undelivered_results();
        assert_eq!(claimed.len(), 1);
        assert!(claimed[0].content.contains("did not wind down"));
    }

    /// A child that never reports `subagent_update`, including a Responses turn
    /// producing only encrypted reasoning invisible to the host, must survive
    /// arbitrarily long inactivity.
    #[test]
    fn a_silent_agent_is_never_settled_by_the_host() {
        let pool = AgentPool::new();
        let agents: Vec<_> = (0..MAX_LIVE_AGENTS)
            .map(|index| register(&pool, &format!("a{index}"), AgentLiveStatus::Running))
            .collect();
        // At capacity.
        assert!(register_result(&pool, "extra", AgentLiveStatus::Running).is_err());

        // None has ever reported progress, including through `push_update`.
        let outcome = pool
            .wait_activity(&agents, Duration::from_millis(50), &|| Ok(()))
            .unwrap();

        assert!(outcome.timed_out, "沉默不产生信封,等待只能超时");
        assert!(outcome.envelopes.is_empty());
        for agent in &agents {
            assert_eq!(
                agent.status(),
                AgentLiveStatus::Running,
                "沉默不再是终态化的理由"
            );
        }
        // Capacity is released only by an explicit stop or a worker return.
        assert!(register_result(&pool, "extra", AgentLiveStatus::Running).is_err());
    }

    #[test]
    fn one_deadline_holds_across_iterations_even_while_a_child_keeps_talking() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);

        let chatty = Arc::clone(&shared);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_signal = Arc::clone(&stop);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                // A steady trickle of progress. Under a caller that re-derives
                // the timeout each iteration this would extend the wait forever.
                while !stop_signal.load(Ordering::Acquire) {
                    chatty.push_update(chatty.identity(), "仍在工作".into(), "2026-07-28T00:00:00Z".into());
                    std::thread::sleep(Duration::from_millis(20));
                }
            });

            let deadline = Instant::now() + Duration::from_millis(400);
            let mut rounds = 0;
            while Instant::now() < deadline {
                let outcome = pool
                    .wait_activity_until(&[Arc::clone(&shared)], deadline, &|| Ok(()))
                    .unwrap();
                rounds += 1;
                if outcome.timed_out {
                    break;
                }
            }
            stop.store(true, Ordering::Release);

            let overshoot = Instant::now().saturating_duration_since(deadline);
            assert!(rounds > 1, "the child should have produced several batches");
            assert!(
                overshoot < WAIT_POLL_INTERVAL * 4,
                "deadline overshot by {overshoot:?}"
            );
        });
    }

    /// The property that makes async fan-out worth having, and that nothing
    /// else guards: spawning N children in one round and collecting them with a
    /// single `task_wait` costs the slowest child, not their sum. Synchronous
    /// spawn would serialize these, because tool calls inside one model round
    /// run strictly one after another.
    #[test]
    fn one_wait_collects_a_parallel_fan_out_in_slowest_child_time() {
        const CHILD_WORK: Duration = Duration::from_millis(300);
        let pool = AgentPool::new();
        let agents: Vec<_> = ["a1", "a2", "a3"]
            .iter()
            .map(|name| register(&pool, name, AgentLiveStatus::Running))
            .collect();

        let started = Instant::now();
        std::thread::scope(|scope| {
            for shared in &agents {
                let shared = Arc::clone(shared);
                scope.spawn(move || {
                    std::thread::sleep(CHILD_WORK);
                    shared.complete_turn(
                        shared.identity(),
                        Vec::new(),
                        format!("{} 完成", shared.name),
                        AgentLiveStatus::Idle,
                        &ModelUsage::default(),
                        CHILD_WORK.as_millis() as u64,
                        None,
                    );
                });
            }

            let mut collected = Vec::new();
            while collected.len() < agents.len() {
                let outcome = pool
                    .wait_activity(&agents, Duration::from_secs(30), &|| Ok(()))
                    .unwrap();
                collected.extend(
                    outcome
                        .envelopes
                        .into_iter()
                        .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_))),
                );
            }
            assert_eq!(collected.len(), 3);
            for envelope in &collected {
                let metrics = envelope.metrics.as_ref().expect("result carries metrics");
                assert_eq!(metrics.duration_ms, CHILD_WORK.as_millis() as u64);
            }
        });

        let elapsed = started.elapsed();
        assert!(
            elapsed < CHILD_WORK * 2,
            "fan-out serialized: {elapsed:?} for three {CHILD_WORK:?} children"
        );
    }

    #[test]
    fn wait_drains_updates_and_results_and_heartbeat_errors_abort() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.push_update(shared.identity(), "进展 1".into(), "2026-07-14T00:00:00Z".into());
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "最终结论".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            0,
            None,
        );
        let outcome = pool
            .wait_activity(&[Arc::clone(&shared)], Duration::from_secs(30), &|| Ok(()))
            .unwrap();
        assert_eq!(outcome.envelopes.len(), 2);
        assert_eq!(outcome.envelopes[0].kind, EnvelopeKind::Update);
        assert!(matches!(
            outcome.envelopes[1].kind,
            EnvelopeKind::Result(SubagentRunStatus::Completed)
        ));

        // Envelopes were drained; with the agent idle the next wait returns
        // immediately instead of blocking.
        let outcome = pool
            .wait_activity(&[Arc::clone(&shared)], Duration::from_secs(30), &|| Ok(()))
            .unwrap();
        assert!(outcome.envelopes.is_empty());

        assert!(shared.begin_turn("call-2"));
        let error = pool
            .wait_activity(&[shared], Duration::from_secs(30), &|| {
                Err("模型运行已停止".into())
            })
            .unwrap_err();
        assert_eq!(error, "模型运行已停止");
    }

    #[test]
    fn wait_times_out_when_a_running_agent_stays_silent() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let started = Instant::now();
        let outcome = pool
            .wait_activity(&[shared], Duration::from_millis(300), &|| Ok(()))
            .unwrap();
        assert!(outcome.timed_out);
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    /// `task_wait` blocks for terminal results. Progress updates do not end the
    /// wait, but are returned with the final outcome rather than discarded.
    #[test]
    fn a_result_wait_blocks_through_updates_until_the_result_lands() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);

        let child = Arc::clone(&shared);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for index in 1..=3 {
                    child.push_update(
                        child.identity(),
                        format!("进展 {index}"),
                        "2026-08-26T00:00:00Z".into(),
                    );
                    std::thread::sleep(WAIT_POLL_INTERVAL);
                }
                child.complete_turn(
                    child.identity(),
                    Vec::new(),
                    "最终结论".into(),
                    AgentLiveStatus::Idle,
                    &ModelUsage::default(),
                    0,
                    None,
                );
            });

            let outcome = pool
                .wait_results(&[Arc::clone(&shared)], Duration::from_secs(30), &|| Ok(()))
                .unwrap();
            assert!(!outcome.timed_out, "the result landed inside the deadline");
            assert_eq!(
                outcome
                    .envelopes
                    .iter()
                    .filter(|envelope| envelope.kind == EnvelopeKind::Update)
                    .count(),
                3,
                "updates buffered while blocked must come back with the result"
            );
            assert!(matches!(
                outcome.envelopes.last().expect("a result envelope").kind,
                EnvelopeKind::Result(SubagentRunStatus::Completed)
            ));
        });
    }

    /// Invariant: when a wait returns, observed-task outboxes contain no
    /// terminal result; otherwise the model receives it only through next-round
    /// fold. The production path drains again after the non-Running check because
    /// draining and checking status are not one snapshot.
    #[test]
    fn a_settled_task_leaves_no_result_in_the_outbox_for_the_fold() {
        for round in 0..100 {
            let pool = AgentPool::new();
            let shared = register(&pool, "a1", AgentLiveStatus::Running);
            let child = Arc::clone(&shared);
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    child.complete_turn(
                        child.identity(),
                        Vec::new(),
                        "最终结论".into(),
                        AgentLiveStatus::Idle,
                        &ModelUsage::default(),
                        0,
                        None,
                    );
                });
                let outcome = pool
                    .wait_results(&[Arc::clone(&shared)], Duration::from_secs(30), &|| Ok(()))
                    .unwrap();
                assert!(
                    outcome
                        .envelopes
                        .iter()
                        .any(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_))),
                    "round {round}: the wait returned without a result that had already landed"
                );
            });
            assert!(
                shared.take_undelivered_results().is_empty(),
                "round {round}: a result was left in the outbox for the fold to pick up"
            );
        }
    }

    /// When `task_wait` names multiple tasks, the first terminal result does not
    /// end the wait. Staggered completion distinguishes this from single-result
    /// return behavior.
    #[test]
    fn a_result_wait_blocks_until_every_watched_task_delivers() {
        let pool = AgentPool::new();
        let watched = ["a1", "a2", "a3"]
            .map(|name| register(&pool, name, AgentLiveStatus::Running))
            .to_vec();

        std::thread::scope(|scope| {
            for (index, shared) in watched.iter().enumerate() {
                let child = Arc::clone(shared);
                scope.spawn(move || {
                    std::thread::sleep(WAIT_POLL_INTERVAL * (index as u32 + 1));
                    child.complete_turn(
                        child.identity(),
                        Vec::new(),
                        format!("结论 {index}"),
                        AgentLiveStatus::Idle,
                        &ModelUsage::default(),
                        0,
                        None,
                    );
                });
            }

            let outcome = pool
                .wait_results(&watched, Duration::from_secs(30), &|| Ok(()))
                .unwrap();
            assert!(!outcome.timed_out, "三份结果都在期限内落地");
            let results = outcome
                .envelopes
                .iter()
                .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
                .map(|envelope| envelope.agent.clone())
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 3, "一次等待要把三份结果一起交出来：{results:?}");
            for name in ["a1", "a2", "a3"] {
                assert!(results.iter().any(|agent| agent == name), "缺 {name}：{results:?}");
            }
        });
        for shared in &watched {
            assert!(
                shared.take_undelivered_results().is_empty(),
                "等到全部之后 outbox 里不该还留着给 fold 的结果"
            );
        }
    }

    /// On expiry, return already-collected results rather than nothing. Results
    /// and expiry can both be true, so timeout output separates the two address sets.
    #[test]
    fn a_timed_out_multi_task_wait_returns_the_results_it_already_has() {
        let pool = AgentPool::new();
        let done = register(&pool, "a1", AgentLiveStatus::Running);
        let never = register(&pool, "a2", AgentLiveStatus::Running);
        done.complete_turn(
            done.identity(),
            Vec::new(),
            "先落地的那个".into(),
            AgentLiveStatus::Idle,
            &ModelUsage::default(),
            0,
            None,
        );

        let outcome = pool
            .wait_results(
                &[Arc::clone(&done), Arc::clone(&never)],
                Duration::from_millis(300),
                &|| Ok(()),
            )
            .unwrap();
        assert!(outcome.timed_out, "a2 永远不完成，等待必须走期限那条腿");
        let results = outcome
            .envelopes
            .iter()
            .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "已经拿到的那份不能因为到期就被丢掉");
        assert_eq!(results[0].agent, "a1");
    }

    /// A task already terminal whose result was claimed elsewhere counts as
    /// settled individually, rather than delaying the entire wait to its deadline.
    #[test]
    fn a_finished_task_does_not_hold_up_a_wait_for_its_running_sibling() {
        let pool = AgentPool::new();
        let finished = register(&pool, "a1", AgentLiveStatus::Idle);
        let running = register(&pool, "a2", AgentLiveStatus::Running);
        let child = Arc::clone(&running);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(WAIT_POLL_INTERVAL);
                child.complete_turn(
                    child.identity(),
                    Vec::new(),
                    "跑完了".into(),
                    AgentLiveStatus::Idle,
                    &ModelUsage::default(),
                    0,
                    None,
                );
            });
            let outcome = pool
                .wait_results(
                    &[Arc::clone(&finished), Arc::clone(&running)],
                    Duration::from_secs(30),
                    &|| Ok(()),
                )
                .unwrap();
            assert!(!outcome.timed_out);
            assert_eq!(
                outcome
                    .envelopes
                    .iter()
                    .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
                    .count(),
                1,
                "只有那个真的跑完的任务投了信封"
            );
        });
    }

    /// The deadline leg has to hand back the same buffer the result leg does.
    /// Dropping it would make a timed-out wait lose progress outright: the
    /// outbox is the only copy, and `updates` feeds `task_list`/`record`
    /// rather than refilling the mailbox.
    #[test]
    fn a_timed_out_result_wait_still_hands_back_buffered_updates() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.push_update(
            shared.identity(),
            "只有进展".into(),
            "2026-08-26T00:00:00Z".into(),
        );

        let outcome = pool
            .wait_results(&[shared], Duration::from_millis(300), &|| Ok(()))
            .unwrap();
        assert!(outcome.timed_out, "no result ever landed");
        assert_eq!(outcome.envelopes.len(), 1, "the update must not be swallowed");
        assert_eq!(outcome.envelopes[0].kind, EnvelopeKind::Update);
    }

    /// The abort leg returns no outcome at all, so its buffer has to go back
    /// into the outbox instead — otherwise cancelling a run silently destroys
    /// progress the children already reported.
    #[test]
    fn an_aborted_result_wait_restores_the_updates_it_buffered() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.push_update(
            shared.identity(),
            "进展".into(),
            "2026-08-26T00:00:00Z".into(),
        );

        let error = pool
            .wait_results(&[Arc::clone(&shared)], Duration::from_secs(30), &|| {
                Err("模型运行已停止".into())
            })
            .unwrap_err();
        assert_eq!(error, "模型运行已停止");

        let outcome = pool
            .wait_activity(&[shared], Duration::from_secs(30), &|| Ok(()))
            .unwrap();
        assert_eq!(outcome.envelopes.len(), 1, "the buffered update came back");
        assert_eq!(outcome.envelopes[0].kind, EnvelopeKind::Update);
    }

    #[test]
    fn quiescence_interrupts_running_agents_and_records_persist() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        let worker = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || {
                // Simulates a worker observing its cancellation flag.
                while !shared.cancel.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let guard = RunningTurnGuard::new(shared);
                drop(guard);
            }
        });
        pool.await_quiescence();
        worker.join().unwrap();
        assert_eq!(shared.status(), AgentLiveStatus::Interrupted);
        let record = shared.record();
        assert_eq!(record.status, SubagentRunStatus::Interrupted);
        assert_eq!(record.name.as_deref(), Some("a1"));
    }

    #[test]
    fn persisted_child_modes_are_explicit_and_mutually_distinct() {
        let pool = AgentPool::new();

        let ordinary = register_template(&pool, "ordinary", template()).record();
        assert!(!ordinary.inherits_model_memory);
        assert!(ordinary.fork_model_binding.is_none());
        assert!(ordinary.agent_definition.is_none());

        let mut fork_template = template();
        let raw_fork_provider_id = "provider/raw:主/精确";
        let raw_fork_model_id = "vendor/kimi-k3:推理/長模型";
        fork_template.provider.id = raw_fork_provider_id.into();
        fork_template.model.id = raw_fork_model_id.into();
        // Fork identity is explicit and independent of whether a lease has
        // already been reacquired for this particular turn. The keyed receipt
        // authenticates the rendered snapshot but is never model identity.
        fork_template.inherits_parent_model_memory = true;
        fork_template.fork_model_binding = Some(ForkModelBinding {
            provider_id: raw_fork_provider_id.into(),
            model_id: raw_fork_model_id.into(),
            memory_language: ResolvedLanguage::EnUs,
            memory_tool_names: vec!["memory_read".into()],
            system_prompt_snapshot: "trusted fork system prompt".into(),
            system_prompt_receipt: "cd".repeat(32),
            memory_snapshot_receipt: Some("ab".repeat(32)),
            binding_receipt: "ef".repeat(32),
            receipt_version: 1,
        });
        let fork = register_template(&pool, "fork", fork_template).record();
        assert!(fork.inherits_model_memory);
        assert_eq!(
            fork.fork_model_binding.as_ref().map(|binding| (
                binding.provider_id.as_str(),
                binding.model_id.as_str(),
                binding.memory_language,
                binding.memory_tool_names.as_slice(),
            )),
            Some((
                raw_fork_provider_id,
                raw_fork_model_id,
                ResolvedLanguage::EnUs,
                ["memory_read".to_owned()].as_slice(),
            ))
        );
        assert!(fork.agent_definition.is_none());
        let persisted_fork = serde_json::to_value(&fork).unwrap();
        assert_eq!(
            persisted_fork["forkModelBinding"]["providerId"],
            raw_fork_provider_id
        );
        assert_eq!(
            persisted_fork["forkModelBinding"]["modelId"],
            raw_fork_model_id
        );

        let raw_model_id = "vendor/named-kimi:推理/長模型";
        let binding = AgentDefinitionBinding {
            source: AgentDefinitionSource::Project,
            source_key: "workspace/原始:项目".into(),
            name: "reviewer".into(),
            revision: 7,
            memory_epoch: 3,
            provider_id: "provider/raw:主".into(),
            model_id: raw_model_id.into(),
            memory: AgentDefinitionMemory::Project,
            scope_key: "workspace/原始:项目".into(),
            configuration_receipt: "ef".repeat(32),
            receipt_version: 1,
        };
        let mut named_template = template();
        named_template.agent_definition_binding = Some(binding.clone());
        let named = register_template(&pool, "named", named_template).record();
        assert!(!named.inherits_model_memory);
        assert!(named.fork_model_binding.is_none());
        assert_eq!(named.agent_definition.as_ref(), Some(&binding));

        let persisted = serde_json::to_value(&named).unwrap();
        assert_eq!(
            persisted["agentDefinition"]["modelId"],
            serde_json::Value::String(raw_model_id.into())
        );
        let restored: SubagentRunRecord = serde_json::from_value(persisted).unwrap();
        assert_eq!(
            restored
                .agent_definition
                .expect("named definition binding")
                .model_id,
            raw_model_id
        );
    }

    #[test]
    fn registration_rejects_ambiguous_or_incomplete_child_memory_modes() {
        let pool = AgentPool::new();

        let mut missing_binding = template();
        missing_binding.inherits_parent_model_memory = true;
        let error = pool
            .register(
                "missing".into(),
                "missing".into(),
                "task".into(),
                missing_binding,
                SubagentRunKind::General,
                &AppState::default(),
                Vec::new(),
                Vec::new(),
                AgentLiveStatus::Idle,
                "call-missing".into(),
                None,
            )
            .unwrap_err();
        assert!(error.contains("host-generated exact model binding"), "{error}");

        let mut ambiguous = template();
        ambiguous.inherits_parent_model_memory = true;
        ambiguous.fork_model_binding = Some(ForkModelBinding {
            provider_id: ambiguous.provider.id.clone(),
            model_id: ambiguous.model.id.clone(),
            memory_language: ResolvedLanguage::ZhCn,
            memory_tool_names: Vec::new(),
            system_prompt_snapshot: "trusted fork system prompt".into(),
            system_prompt_receipt: "cd".repeat(32),
            memory_snapshot_receipt: None,
            binding_receipt: "ef".repeat(32),
            receipt_version: 1,
        });
        ambiguous.agent_definition_binding = Some(AgentDefinitionBinding {
            source: AgentDefinitionSource::User,
            source_key: String::new(),
            name: "reviewer".into(),
            revision: 1,
            memory_epoch: 1,
            provider_id: ambiguous.provider.id.clone(),
            model_id: ambiguous.model.id.clone(),
            memory: AgentDefinitionMemory::None,
            scope_key: String::new(),
            configuration_receipt: "ef".repeat(32),
            receipt_version: 1,
        });
        let error = pool
            .register(
                "ambiguous".into(),
                "ambiguous".into(),
                "task".into(),
                ambiguous,
                SubagentRunKind::General,
                &AppState::default(),
                Vec::new(),
                Vec::new(),
                AgentLiveStatus::Idle,
                "call-ambiguous".into(),
                None,
            )
            .unwrap_err();
        assert!(error.contains("cannot inherit parent model memory"));
    }

    /// A name can bind only one execution mode within a conversation.
    ///
    /// Names are addressable identities for `send_message` and `followup_task`.
    /// Rebinding `a1` to a different inheritance, fork, or named-definition
    /// binding would silently change the recipient. The in-process registry
    /// therefore maintains a one-to-one name-to-mode mapping. Both registrations
    /// must share one `AppState` for this test to exercise that registry.
    #[test]
    fn a_name_binds_to_exactly_one_execution_mode_per_conversation() {
        let state = AppState::default();
        let pool = AgentPool::new();
        pool.register(
            "a1".into(),
            "a1".into(),
            "task".into(),
            template(),
            SubagentRunKind::General,
            &state,
            Vec::new(),
            Vec::new(),
            AgentLiveStatus::Idle,
            "call-a1".into(),
            None,
        )
        .expect("first registration reserves the name");

        // Same name and mode is idempotent, including agent rehydration.
        let pool = AgentPool::new();
        pool.register(
            "a1".into(),
            "a1".into(),
            "task".into(),
            template(),
            SubagentRunKind::General,
            &state,
            Vec::new(),
            Vec::new(),
            AgentLiveStatus::Idle,
            "call-a1-again".into(),
            None,
        )
        .expect("the same name with the same mode is idempotent");

        // A name with a different mode is rejected because its definition
        // binding changes the payload bytes.
        let mut substituted = template();
        substituted.agent_definition_binding = Some(AgentDefinitionBinding {
            source: AgentDefinitionSource::User,
            source_key: String::new(),
            name: "reviewer".into(),
            revision: 1,
            memory_epoch: 1,
            provider_id: substituted.provider.id.clone(),
            model_id: substituted.model.id.clone(),
            memory: AgentDefinitionMemory::None,
            scope_key: String::new(),
            configuration_receipt: "ef".repeat(32),
            receipt_version: 1,
        });
        let pool = AgentPool::new();
        let error = pool
            .register(
                "a1".into(),
                "a1".into(),
                "task".into(),
                substituted,
                SubagentRunKind::General,
                &state,
                Vec::new(),
                Vec::new(),
                AgentLiveStatus::Idle,
                "call-a1-substituted".into(),
                None,
            )
            .unwrap_err();
        assert!(error.contains("已绑定到另一种执行模式"), "{error}");
        assert!(pool.is_empty(), "被拒绝的注册不能留下活代理");
    }
    #[test]
    fn usage_accumulates_across_turns_and_is_taken_once() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Running);
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "回合 1".into(),
            AgentLiveStatus::Idle,
            &ModelUsage {
                input_tokens: Some(10),
                cached_input_tokens: Some(6),
                output_tokens: Some(4),
                total_tokens: Some(14),
                reasoning_tokens: None,
            },
            0,
            None,
        );
        assert!(shared.begin_turn("call-2"));
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "回合 2".into(),
            AgentLiveStatus::Idle,
            &ModelUsage {
                input_tokens: Some(1),
                cached_input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
                reasoning_tokens: None,
            },
            0,
            None,
        );
        let usage = shared.take_usage();
        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.cached_input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(6));
        assert_eq!(usage.total_tokens, Some(17));
        assert_eq!(shared.take_usage(), ModelUsage::default());

        // The per-agent figure survives the drain that zeroed the parent-facing
        // counter. Without the separate accumulator the record would persist
        // zeros, because `finalize_agent_pool` takes before it builds records.
        assert_eq!(shared.lifetime_usage().total_tokens, Some(17));
        assert_eq!(shared.record().usage.total_tokens, Some(17));
    }

    /// Restoring a record must not re-bill the parent. `restore_lifetime_usage`
    /// seeds only the never-taken copy, so a continuation reports the agent's
    /// full history while `take_usage` still yields just this turn's tokens.
    #[test]
    fn restored_usage_reaches_the_record_without_re_billing_the_parent() {
        let pool = AgentPool::new();
        let shared = register(&pool, "a1", AgentLiveStatus::Idle);
        shared.restore_lifetime_usage(ModelUsage {
            input_tokens: Some(100),
            cached_input_tokens: Some(20),
            output_tokens: Some(50),
            total_tokens: Some(150),
            reasoning_tokens: None,
        });
        assert_eq!(shared.record().usage.total_tokens, Some(150));
        assert_eq!(shared.take_usage(), ModelUsage::default());

        assert!(shared.begin_turn("call-2"));
        shared.complete_turn(
            shared.identity(),
            Vec::new(),
            "续跑".into(),
            AgentLiveStatus::Idle,
            &ModelUsage {
                input_tokens: Some(5),
                cached_input_tokens: Some(1),
                output_tokens: Some(3),
                total_tokens: Some(8),
                reasoning_tokens: None,
            },
            0,
            None,
        );
        // The parent is billed for this turn alone...
        assert_eq!(shared.take_usage().total_tokens, Some(8));
        // ...while the record carries the agent's whole history.
        assert_eq!(shared.record().usage.total_tokens, Some(158));
    }
}
