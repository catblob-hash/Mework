use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    marker::PhantomData,
    path::Path,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use crate::{
    agents::{AgentMailbox, MailboxMessage},
    approval::{ApprovalRegistry, ToolApprovalGrant},
    browser::BrowserRuntime,
    browser_renderer_mount::BrowserRendererMountRegistry,
    document_store::DocumentStore,
    image_attachments::validate_image_list,
    model::{ImageAttachment, JsonObject, SubagentRunRecord, ToolExecutionRequest, ToolResult},
    operation_coordinator::{OperationCoordinator, OperationLease, WorkspaceKey},
    push_events::AppEventHub,
    terminal::TerminalManager,
    tool_prompt::ToolPromptRegistry,
};

const MAX_RECEIPTS: usize = 512;
static RECEIPT_INVOCATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static ACTIVE_RECEIPT_INVOCATIONS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ReceiptKey {
    workspace: String,
    conversation_id: String,
    tool_name: String,
    input: String,
    requested_input: Option<String>,
    result: String,
    subagent_fingerprint: Option<String>,
    /// Present only while a model executor is running. Provisional receipts
    /// are invisible to persistence validation and are removed by their
    /// invocation scope before PostToolUse commits the final result.
    invocation: Option<u64>,
}

#[derive(Default)]
struct ReceiptBook {
    sequence: u64,
    epoch: u64,
    entries: HashMap<ReceiptKey, (u64, ToolResult)>,
}

#[derive(Clone)]
pub struct AppState {
    pub storage_lock: Arc<Mutex<()>>,
    /// Linearizes the discover→native approval→persist→rediscover trust
    /// transaction and revocation with every provider attempt that carries
    /// externally imported project instructions. Approval/revoke take the
    /// write side; an attempt takes the read side only for its final lease
    /// validation through `request.send()`.
    pub project_import_trust_lock: Arc<RwLock<()>>,
    pub document_store: DocumentStore,
    /// Backend-initiated renderer notifications (background write failures and
    /// future push-only signals). One subscriber, bounded backlog; see
    /// `push_events`.
    pub push_events: AppEventHub,
    pub browser: BrowserRuntime,
    /// Process-local authority for the one trusted main renderer document that may mutate browser
    /// presentation or start new browser-owned operations.
    pub(crate) browser_renderer_mounts: Arc<BrowserRendererMountRegistry>,
    pub terminals: Arc<TerminalManager>,
    /// In-flight `bash` / `powershell` tool calls. A shell command's runtime is unpredictable, so
    /// each one registers here for the duration of its call: that is what puts it in the task
    /// sidebar and gives it a stop button of its own, separate from cancelling the model run.
    pub shell_tasks: crate::shell_tasks::ShellTaskRegistry,
    pub(crate) prose_journals: crate::api::ProseJournalRegistry,
    /// Process-local, per-conversation MCP sessions. Connections and resolved
    /// secrets never cross renderer IPC or persistence.
    pub mcp_sessions: crate::mcp::McpSessionManager,
    /// Serializes named-agent definition publication with provider attempts,
    /// lifecycle hooks, and tool side effects. General document I/O keeps its
    /// own mutex; this gate covers only capability authority.
    pub(crate) definition_authority_lock: Arc<RwLock<()>>,
    receipts: Arc<Mutex<ReceiptBook>>,
    /// Signs and checks the per-card proof that a tool result came from this
    /// application's own execution. Installed once the app-data directory is
    /// known; until then an ephemeral key stands in, so a card attested before
    /// installation fails to verify rather than silently passing.
    attestation_key: Arc<RwLock<crate::tool_attestation::AttestationKey>>,
    approvals: Arc<ApprovalRegistry>,
    /// Outstanding in-app approval cards and per-conversation "always allow"
    /// grants. Process-local: a restart re-asks.
    tool_prompts: Arc<ToolPromptRegistry>,
    /// Model-raised `fork` requests awaiting the user's decision. Process-local
    /// like the approval cards; nothing about a request is worth persisting.
    fork_requests: Arc<crate::fork_requests::ForkRequestRegistry>,
    /// request_id → (cancellation flag, owning conversation, steer inbox). The
    /// inbox is deliberately process-local and never crosses persistence or
    /// provider boundaries.
    model_runs: Arc<Mutex<HashMap<String, (Arc<AtomicBool>, String, Arc<AgentMailbox>)>>>,
    /// `(conversation_id, subagent_name)` to the bound execution-mode digest.
    /// A name may use only one execution mode per conversation. Reusing the same mode is idempotent;
    /// a different mode is rejected so an address never silently changes its recipient.
    /// This is process-local because subagents are live process-local objects.
    subagent_execution_modes: Arc<Mutex<HashMap<(String, String), String>>>,
    /// Live workflow runs that accept per-step control, keyed by
    /// `(request_id, run_id)`.
    ///
    /// Keyed by both halves on purpose. A run id alone would let a renderer that
    /// still has an old card on screen address a step in a run belonging to a
    /// different turn; a request id alone cannot distinguish two workflow calls
    /// in the same turn. The value is the set of step indexes the renderer has
    /// asked to skip, which the driver drains as it dispatches.
    workflow_step_controls: Arc<Mutex<HashMap<(String, String), Arc<WorkflowStepControl>>>>,
    /// Host-owned run event hub with a bounded backlog, replay, and settlement slot. A run outlives
    /// individual renderer channels; see the `run_stream` module documentation.
    run_streams: Arc<crate::run_stream::RunStreamHub>,
    hook_sessions: Arc<Mutex<HashSet<String>>>,
    operation_gate: OperationGate,
    /// Per-conversation task runtime, retained across turns until its owner is deleted.
    conversation_tasks: Arc<Mutex<HashMap<String, Arc<ConversationTasks>>>>,
    /// Tombstones also serialize publication and runtime acquisition with deletion.
    /// IDs are unique; a late run must never resurrect a deleted owner's runtime.
    retired_conversations: Arc<Mutex<HashSet<String>>>,
    /// Per-conversation task surface containing sink and approval entry points. Each top-level run
    /// replaces it so detached workers lazily use the latest security level and workspace settings.
    task_surfaces: Arc<Mutex<HashMap<String, Arc<TaskSurface>>>>,
    /// Interrupted workflow notifications claimed at startup, keyed by conversation. Memory holds
    /// only a delivery queue; the manifest's `crashNoticeDelivered` field remains authoritative.
    workflow_restart_notices: Arc<Mutex<HashMap<String, Vec<crate::workflow_store::InterruptedRun>>>>,
    /// Per-conversation shell session: the snapshot each new Bash sources and
    /// the directory the last successful call reported. There is no shell
    /// process behind this — every call spawns a fresh interpreter — so this map
    /// is the entirety of what continues between calls, and it is deliberately
    /// process-local: a restart starts again at the workspace, exactly as Claude
    /// Code does.
    shell_sessions: Arc<Mutex<HashMap<String, ShellSessionState>>>,
    /// In-app update state: the one download that may be in flight and the file it produced,
    /// which is the only file `install_app_update` will launch. Process-local by design — a
    /// restart is the moment an update has either applied or been abandoned.
    pub app_update: Arc<crate::app_update::UpdateSession>,
}

/// What one conversation's shell calls carry forward. See [`AppState::shell_sessions`].
#[derive(Default)]
struct ShellSessionState {
    /// The generated snapshot, once built. `None` after a failed attempt too —
    /// `snapshot_attempted` is what stops a broken rc file being re-run on every
    /// single call.
    snapshot: Option<std::path::PathBuf>,
    snapshot_attempted: bool,
    /// The directory the last successful call ended in, already validated.
    cwd: Option<std::path::PathBuf>,
}

/// Per-conversation task runtime containing a top-level task pool and kernel shadow.
pub struct ConversationTasks {
    pub pool: Arc<crate::agents::AgentPool>,
    pub shadow: Arc<crate::kernel_shadow::KernelShadow>,
}

/// Per-conversation bridge between detached workers and current or future runs. The sink publishes
/// only while a run is unsettled and never fails; worker lifetime is controlled only by task stop.
/// When no run is active, approval cards use the app push channel and wait for an answer. The
/// approval closure receives the caller's own stop flag and must not look one up by conversation.
pub struct TaskSurface {
    pub sink: Arc<crate::api::OwnedModelEventSink>,
    pub approve: Arc<crate::api::OwnedTaskApproval>,
}

/// Process-local skip requests for one live workflow run.
///
/// Skip only. There is deliberately no retry: `PlanSource` is a one-way state
/// machine whose slots move Pending → InFlight → Done and are never re-opened
/// (`absorb_new_outcomes` accepts an outcome only for an `InFlight` slot), and
/// the cache-key chain latches `diverged` on the first miss. Re-running a
/// settled step would need a second execution of a plan position that the
/// scheduler has already consumed, which the seam cannot express — so the
/// command refuses it rather than accepting it and quietly doing nothing.
#[derive(Default)]
pub struct WorkflowStepControl {
    skipped: Mutex<HashSet<usize>>,
}

impl WorkflowStepControl {
    fn request_skip(&self, step_index: usize) {
        self.skipped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(step_index);
    }

    /// True once, for a step the renderer asked to skip.
    ///
    /// Draining rather than peeking keeps the driver's own bookkeeping the only
    /// record of what it acted on: a request that arrives after the step has
    /// already settled is consumed and dropped, not retained to fire against
    /// whatever occupies that index later.
    pub fn take_skip(&self, step_index: usize) -> bool {
        self.skipped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&step_index)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default)]
pub struct OperationGate {
    coordinator: OperationCoordinator,
}

/// Keeps an IPC/model operation visible to document lifecycle commands until
/// every early-return and async completion path has finished.
#[must_use = "the guard must stay alive for the complete operation"]
pub struct OperationGuard {
    _lease: OperationLease,
}

pub(crate) struct ProvisionalReceiptScope {
    invocation: u64,
    receipts: Arc<Mutex<ReceiptBook>>,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for ProvisionalReceiptScope {
    fn drop(&mut self) {
        ACTIVE_RECEIPT_INVOCATIONS.with(|active| {
            let mut active = active.borrow_mut();
            if active.last().copied() == Some(self.invocation) {
                active.pop();
            } else {
                active.retain(|invocation| *invocation != self.invocation);
            }
        });
        self.receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .retain(|key, _| key.invocation != Some(self.invocation));
    }
}

#[must_use = "the guard must stay alive for the complete mutation"]
pub struct MutationGuard {
    _lease: OperationLease,
}

impl OperationGate {
    /// Entry point for model/tool operations that do not have a workspace
    /// identity. It coexists with workspace readers but blocks every writer.
    pub fn begin_operation(&self) -> Result<OperationGuard, String> {
        self.coordinator
            .try_global_shared(None)
            .map(|lease| OperationGuard { _lease: lease })
            .map_err(|error| format!("共享状态正在变更；新的操作已取消: {error}"))
    }

    pub fn begin_workspace_operation(
        &self,
        workspace: impl Into<WorkspaceKey>,
        conversation_id: Option<String>,
    ) -> Result<OperationGuard, String> {
        self.coordinator
            .try_workspace_shared(workspace, conversation_id)
            .map(|lease| OperationGuard { _lease: lease })
            .map_err(|error| format!("工作区正在变更；新的操作已取消: {error}"))
    }

    pub fn begin_mutation(&self) -> Result<MutationGuard, String> {
        self.coordinator
            .try_global_exclusive(None)
            .map(|lease| MutationGuard { _lease: lease })
            .map_err(|error| format!("仍有模型、工具或 Agent 操作正在进行；变更已取消: {error}"))
    }

    pub fn begin_workspace_mutation(
        &self,
        workspace: impl Into<WorkspaceKey>,
        conversation_id: Option<String>,
    ) -> Result<MutationGuard, String> {
        self.coordinator
            .try_workspace_exclusive(workspace, conversation_id)
            .map(|lease| MutationGuard { _lease: lease })
            .map_err(|error| format!("工作区仍有活动操作；写操作已取消: {error}"))
    }

    /// Nothing in the running application polls for activity — callers acquire a guard and act on
    /// the result — so this stays as the lease-cleanup assertion used by the gate tests.
    #[cfg(test)]
    pub fn has_active_operations(&self) -> bool {
        !self.coordinator.is_idle()
    }
}

impl AppState {
    fn new() -> Self {
        Self {
            storage_lock: Arc::default(),
            project_import_trust_lock: Arc::default(),
            document_store: DocumentStore::default(),
            push_events: AppEventHub::default(),
            browser: BrowserRuntime::default(),
            browser_renderer_mounts: Arc::default(),
            terminals: Arc::default(),
            shell_tasks: crate::shell_tasks::ShellTaskRegistry::default(),
            prose_journals: crate::api::ProseJournalRegistry::default(),
            mcp_sessions: crate::mcp::McpSessionManager::default(),
            definition_authority_lock: Arc::default(),
            receipts: Arc::default(),
            attestation_key: Arc::new(RwLock::new(
                crate::tool_attestation::AttestationKey::ephemeral(),
            )),
            approvals: Arc::default(),
            tool_prompts: Arc::default(),
            fork_requests: Arc::default(),
            model_runs: Arc::default(),
            subagent_execution_modes: Arc::default(),
            workflow_step_controls: Arc::default(),
            run_streams: Arc::default(),
            hook_sessions: Arc::default(),
            operation_gate: OperationGate::default(),
            conversation_tasks: Arc::default(),
            retired_conversations: Arc::default(),
            task_surfaces: Arc::default(),
            workflow_restart_notices: Arc::default(),
            shell_sessions: Arc::default(),
            app_update: Arc::default(),
        }
    }

    /// The loopback development bridge shares the ordinary state. The VM
    /// runtime is inert everywhere, so there is no host surface a development
    /// build could expose that a production build does not.
    #[cfg(feature = "browser-dev")]
    pub(crate) fn browser_dev() -> Self {
        Self::new()
    }

    pub fn operation_gate(&self) -> OperationGate {
        self.operation_gate.clone()
    }

    /// Adopts the durable signing key for this app-data directory. Called once
    /// at startup, before the renderer can ask for anything.
    pub fn install_attestation_key(&self, app_data: &Path) -> Result<(), String> {
        let key = crate::tool_attestation::AttestationKey::load_or_create(app_data)?;
        *self
            .attestation_key
            .write()
            .map_err(|_| "工具回执密钥锁已损坏".to_owned())? = key;
        Ok(())
    }

    /// Issues the proof that one tool card came from this application's own
    /// execution. Empty only if the key lock is poisoned, in which case the
    /// card is quarantined at save time rather than trusted.
    pub fn attest_tool_context(
        &self,
        subject: &crate::tool_attestation::AttestationSubject<'_>,
    ) -> String {
        self.attestation_key
            .read()
            .map(|key| key.attest(subject))
            .unwrap_or_default()
    }

    /// Whether `token` is this process's proof for `subject`. An empty token is
    /// never valid: absence of a proof is not proof.
    pub fn verify_tool_context(
        &self,
        subject: &crate::tool_attestation::AttestationSubject<'_>,
        token: &str,
    ) -> bool {
        !token.is_empty()
            && self
                .attestation_key
                .read()
                .is_ok_and(|key| key.verify(subject, token))
    }










    pub fn begin_operation(&self) -> Result<OperationGuard, String> {
        self.operation_gate.begin_operation()
    }

    /// Gets or creates a live owner's runtime. Deleted owners get an inert,
    /// unregistered runtime so an already-dispatched run cannot resurrect them.
    pub fn conversation_tasks(&self, conversation_id: &str) -> Arc<ConversationTasks> {
        let retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        if retired.contains(conversation_id) {
            let pool = Arc::new(crate::agents::AgentPool::new());
            pool.retire();
            return Arc::new(ConversationTasks {
                shadow: Arc::new(crate::kernel_shadow::KernelShadow::new(
                    crate::agents::MAX_LIVE_AGENTS,
                )),
                pool,
            });
        }
        let mut map = self
            .conversation_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tasks) = map.get(conversation_id) {
            return Arc::clone(tasks);
        }
        let pool = Arc::new(crate::agents::AgentPool::new());
        let shadow = Arc::new(crate::kernel_shadow::KernelShadow::new(
            pool.live_limit().unwrap_or(crate::agents::MAX_LIVE_AGENTS),
        ));
        let hub = self.push_events.clone();
        let runs = Arc::clone(&self.model_runs);
        let cid = conversation_id.to_owned();
        let retired_owners = Arc::clone(&self.retired_conversations);
        pool.set_settle_observer(Arc::new(move |_name, _status| {
            let retired = retired_owners.lock().unwrap_or_else(|p| p.into_inner());
            if retired.contains(&cid) {
                return;
            }
            // Every terminal result is deliverable, whatever ended the task, so
            // no status filter belongs here: a task the user closed wakes the
            // conversation exactly like one that finished on its own.
            let active = runs
                .lock()
                .map(|map| map.values().any(|(_, owner, _)| owner == &cid))
                .unwrap_or(true);
            if !active {
                hub.publish(crate::push_events::AppPushEvent::TaskSettled {
                    conversation_id: cid.clone(),
                });
            }
        }));
        let tasks = Arc::new(ConversationTasks { pool, shadow });
        map.insert(conversation_id.to_owned(), Arc::clone(&tasks));
        tasks
    }

    /// Idempotently destroys a conversation's process-local task owner. Call
    /// immediately after durable deletion, even if snapshot synchronization fails.
    /// Ordinary TurnEnd/TurnCancel and workspace moves must never use this path.
    pub(crate) fn retire_conversation_tasks(&self, conversation_id: &str) {
        let mut retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        retired.insert(conversation_id.to_owned());
        let tasks = self.conversation_tasks.lock().unwrap_or_else(|p| p.into_inner())
            .remove(conversation_id);
        self.task_surfaces.lock().unwrap_or_else(|p| p.into_inner()).remove(conversation_id);
        self.workflow_restart_notices.lock().unwrap_or_else(|p| p.into_inner()).remove(conversation_id);
        // The map lock is already released. Cancellation only sets owned flags;
        // it never waits for worker completion (which may itself try to publish).
        if let Some(tasks) = tasks {
            tasks.pool.retire();
        }
        let _ = self.cancel_conversation_model_run(conversation_id);
    }

    /// Retires only deleted IDs, not conversations moved between workspaces.
    pub(crate) fn retire_removed_conversation_tasks(&self, previous: &crate::model::AppDocument, next: &crate::model::AppDocument) {
        let remaining: HashSet<&str> = next.workspaces.iter()
            .flat_map(|workspace| &workspace.conversations)
            .map(|conversation| conversation.id.as_str()).collect();
        for conversation in previous.workspaces.iter().flat_map(|workspace| &workspace.conversations) {
            if !remaining.contains(conversation.id.as_str()) {
                self.retire_conversation_tasks(&conversation.id);
            }
        }
    }

    /// Returns an existing task runtime without creating one. Wake checks and task stops need no
    /// empty pool for a conversation without tasks.
    pub fn existing_conversation_tasks(&self, conversation_id: &str) -> Option<Arc<ConversationTasks>> {
        self.conversation_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(conversation_id)
            .cloned()
    }

    /// Rechecks whether an idle conversation needs a `TaskSettled` wake. Every run exit path must
    /// invoke this because a result can settle after the final round decision. Duplicate wakes are
    /// harmless because the renderer deduplicates by conversation.
    pub fn recheck_task_wake(&self, conversation_id: &str) {
        let retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        if retired.contains(conversation_id) {
            return;
        }
        let Some(tasks) = self.existing_conversation_tasks(conversation_id) else {
            return;
        };
        if tasks.pool.has_undrained_foldable_results()
            && !self.conversation_model_run_active(conversation_id)
        {
            self.push_events
                .publish(crate::push_events::AppPushEvent::TaskSettled {
                    conversation_id: conversation_id.to_owned(),
                });
        }
    }

    /// Recovers level-triggered wakes after renderer adoption. `TaskSettled` is an evictable edge
    /// event, so rescan idle pools with deliverable results. Pending restart notices also require a wake.
    pub fn wake_pending_conversations(&self) -> Vec<String> {
        let retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        let entries: Vec<(String, Arc<ConversationTasks>)> = self
            .conversation_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(conversation_id, tasks)| (conversation_id.clone(), Arc::clone(tasks)))
            .collect();
        let mut pending: Vec<String> = entries
            .into_iter()
            .filter(|(conversation_id, tasks)| {
                tasks.pool.has_undrained_foldable_results()
                    && !self.conversation_model_run_active(conversation_id)
            })
            .map(|(conversation_id, _)| conversation_id)
            .collect();
        for conversation_id in self.conversations_with_restart_notices() {
            if !pending.contains(&conversation_id)
                && !self.conversation_model_run_active(&conversation_id)
            {
                pending.push(conversation_id);
            }
        }
        pending.retain(|conversation_id| !retired.contains(conversation_id));
        pending
    }

    /// The Bash snapshot this conversation's shells source, building it on first
    /// use. A failed build is remembered so a broken rc file is not re-run on
    /// every call; the caller then falls back to a login shell.
    ///
    /// Only Bash has one. PowerShell needs no snapshot: `-NoProfile` is Claude
    /// Code's choice there and there is nothing to replay.
    pub(crate) fn shell_snapshot(
        &self,
        conversation_id: &str,
        app_data: &Path,
        shell_path: &str,
    ) -> Option<std::path::PathBuf> {
        {
            let sessions = self
                .shell_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(session) = sessions.get(conversation_id) {
                if session.snapshot_attempted {
                    // A snapshot that was deleted underneath us is worse than
                    // none: `source` would fail and the shell would silently run
                    // without the user's aliases. Rebuild instead.
                    match &session.snapshot {
                        Some(path) if path.is_file() => return Some(path.clone()),
                        None => return None,
                        Some(_) => {}
                    }
                }
            }
        }
        // Built outside the lock: it spawns a login shell that runs the user's
        // rc file, and holding the map across that would stall every other
        // conversation's shell call.
        let built = crate::shell_snapshot::build(app_data, shell_path);
        let mut sessions = self
            .shell_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions.entry(conversation_id.to_owned()).or_default();
        session.snapshot_attempted = true;
        session.snapshot = built.clone();
        built
    }

    /// Where this conversation's next shell call starts, or `None` before its
    /// first successful call.
    pub(crate) fn shell_cwd(&self, conversation_id: &str) -> Option<std::path::PathBuf> {
        self.shell_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(conversation_id)
            .and_then(|session| session.cwd.clone())
    }

    /// Adopts the directory a call reported. The caller validates it first; this
    /// only records it.
    pub(crate) fn set_shell_cwd(&self, conversation_id: &str, cwd: std::path::PathBuf) {
        self.shell_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(conversation_id.to_owned())
            .or_default()
            .cwd = Some(cwd);
    }

    /// Drops the shell session of every conversation not in `retained`, and
    /// deletes the snapshots they owned. Called after a document save, the one
    /// event that can make a conversation go away.
    pub(crate) fn forget_shell_sessions<'a>(&self, retained: impl IntoIterator<Item = &'a str>) {
        let retained = retained.into_iter().collect::<HashSet<_>>();
        let dropped: Vec<std::path::PathBuf> = {
            let mut sessions = self
                .shell_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut dropped = Vec::new();
            sessions.retain(|conversation_id, session| {
                if retained.contains(conversation_id.as_str()) {
                    return true;
                }
                if let Some(path) = session.snapshot.take() {
                    dropped.push(path);
                }
                false
            });
            dropped
        };
        // Removed after the lock: a snapshot is a small generated file and a
        // failed unlink is not worth holding every other conversation for.
        for path in dropped {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Queues startup-recovered interrupted workflow notifications by conversation.
    pub fn seed_workflow_restart_notices(        &self,
        notices: Vec<crate::workflow_store::InterruptedRun>,
    ) {
        if notices.is_empty() {
            return;
        }
        let mut map = self
            .workflow_restart_notices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for notice in notices {
            map.entry(notice.conversation_id.clone())
                .or_default()
                .push(notice);
        }
    }

    /// Drains all pending interruption notices once for round-boundary delivery. A failed delivery
    /// is recovered from the manifest on the next startup scan.
    pub fn take_workflow_restart_notices(
        &self,
        conversation_id: &str,
    ) -> Vec<crate::workflow_store::InterruptedRun> {
        self.workflow_restart_notices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(conversation_id)
            .unwrap_or_default()
    }

    /// Lists conversations with pending interruption notices as part of wake-level recovery.
    pub fn conversations_with_restart_notices(&self) -> Vec<String> {
        self.workflow_restart_notices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, notices)| !notices.is_empty())
            .map(|(conversation_id, _)| conversation_id.clone())
            .collect()
    }

    /// Registers or replaces a conversation task surface. Detached workers lazily resolve it and
    /// therefore use the latest security level and workspace configuration.
    pub fn register_task_surface(&self, conversation_id: &str, surface: Arc<TaskSurface>) {
        let retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        if retired.contains(conversation_id) {
            return;
        }
        self.task_surfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(conversation_id.to_owned(), surface);
    }

    /// Returns the current task surface for a conversation, if one exists.
    pub fn task_surface(&self, conversation_id: &str) -> Option<Arc<TaskSurface>> {
        self.task_surfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(conversation_id)
            .cloned()
    }

    pub fn begin_mutation(&self) -> Result<MutationGuard, String> {
        self.operation_gate.begin_mutation()
    }

    /// Mirrors [`OperationGate::has_active_operations`] for the state-level tests.
    #[cfg(test)]
    pub fn has_active_operations(&self) -> bool {
        self.operation_gate.has_active_operations()
    }

    /// Returns true exactly once per in-process conversation session.
    pub fn begin_hook_session(&self, conversation_id: &str) -> bool {
        self.hook_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(conversation_id.to_owned())
    }

    pub fn begin_model_run(
        &self,
        request_id: &str,
        conversation_id: &str,
    ) -> Result<(Arc<AtomicBool>, Arc<AgentMailbox>), String> {
        let retired = self.retired_conversations.lock().unwrap_or_else(|p| p.into_inner());
        if retired.contains(conversation_id) {
            return Err("对话已删除，不能开始模型运行".into());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let steer_inbox = Arc::new(AgentMailbox::default());
        let mut runs = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if runs.contains_key(request_id) {
            return Err("模型运行 ID 已在使用中".into());
        }
        if runs.values().any(|(_, owner, _)| owner == conversation_id) {
            return Err("这个对话已有模型运行正在进行".into());
        }
        runs.insert(
            request_id.to_owned(),
            (
                cancellation.clone(),
                conversation_id.to_owned(),
                steer_inbox.clone(),
            ),
        );
        Ok((cancellation, steer_inbox))
    }

    pub fn cancel_model_run(&self, request_id: &str) -> Result<bool, String> {
        let run = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(request_id)
            .map(|(cancellation, _, _)| cancellation.clone());
        let Some(cancellation) = run else {
            return Ok(false);
        };
        // Set only this run's flag. Its synchronous legs receive the same `Arc` at dispatch and
        // poll it before and during execution. Do not sweep conversation command rows: they can
        // belong to background commands, cross-turn tasks, or manually rerun commands.
        cancellation.store(true, Ordering::Release);
        Ok(true)
    }

    /// Cancels the current run addressed by conversation rather than request ID, so reloads do not
    /// disable the stop button. Repeated cancellation is idempotent.
    pub fn cancel_conversation_model_run(&self, conversation_id: &str) -> Result<bool, String> {
        let request_id = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(_, (_, owner, _))| owner == conversation_id)
            .map(|(request_id, _)| request_id.clone());
        match request_id {
            Some(request_id) => self.cancel_model_run(&request_id),
            None => Ok(false),
        }
    }

    /// Run event hub for buffering, subscription, and settlement.
    pub fn run_streams(&self) -> &crate::run_stream::RunStreamHub {
        &self.run_streams
    }

    /// True while any model run owned by this conversation is alive.
    pub fn conversation_model_run_active(&self, conversation_id: &str) -> bool {
        self.model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(|(_, owner, _)| owner == conversation_id)
    }

    pub fn steer_model_run(
        &self,
        request_id: &str,
        message_id: String,
        content: String,
        images: Vec<ImageAttachment>,
        created_at: String,
    ) -> Result<(), String> {
        let content = content.trim();
        if message_id.trim().is_empty() || message_id.len() > 128 {
            return Err("排队消息 ID 无效".into());
        }
        if content.is_empty() && images.is_empty() {
            return Err("引导消息的文字与图片不能同时为空".into());
        }
        if content.chars().count() > 100_000 {
            return Err("引导消息不能超过 100000 个字符".into());
        }
        chrono::DateTime::parse_from_rfc3339(&created_at)
            .map_err(|_| "排队消息时间无效".to_owned())?;
        validate_image_list(&images, "引导消息")?;
        let inbox = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(request_id)
            .map(|(_, _, inbox)| inbox.clone())
            .ok_or_else(|| "模型回合已经结束；消息仍保留在队列中".to_owned())?;
        inbox.push_message(MailboxMessage {
            id: Some(message_id),
            content: content.to_owned(),
            images,
            created_at: Some(created_at),
        });
        Ok(())
    }

    /// Opens a live workflow run to per-step control for as long as it drives.
    ///
    /// The driver holds the returned handle and unregisters on the way out, so
    /// a command naming a finished run finds nothing and says so, instead of
    /// parking a skip that no one will ever drain.
    pub fn register_workflow_run(
        &self,
        request_id: &str,
        run_id: &str,
    ) -> Arc<WorkflowStepControl> {
        let control = Arc::new(WorkflowStepControl::default());
        self.workflow_step_controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((request_id.to_owned(), run_id.to_owned()), control.clone());
        control
    }

    /// Closes the run to control. Identity-checked against the handle the
    /// driver was given: a run that has already been replaced under the same
    /// key belongs to a newer driver, and removing it would silence that one.
    pub fn unregister_workflow_run(
        &self,
        request_id: &str,
        run_id: &str,
        control: &Arc<WorkflowStepControl>,
    ) {
        let key = (request_id.to_owned(), run_id.to_owned());
        let mut controls = self
            .workflow_step_controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if controls
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, control))
        {
            controls.remove(&key);
        }
    }

    /// Records a renderer's request to skip one step of a live workflow run.
    ///
    /// Refuses rather than no-ops when the run is not live: a card left on
    /// screen after its turn ended must learn that its buttons no longer
    /// address anything, not watch a click disappear.
    pub fn workflow_step_control(
        &self,
        request_id: &str,
        run_id: &str,
        step_index: usize,
        action: &str,
    ) -> Result<(), String> {
        if action != "skip" {
            // Retry is not merely unimplemented — see `WorkflowStepControl`.
            return Err(format!("不支持的步骤操作：{action}"));
        }
        if step_index >= workflow_core::MAX_LIFETIME_STEPS {
            return Err("步骤下标超出运行上限".into());
        }
        let control = self
            .workflow_step_controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(request_id.to_owned(), run_id.to_owned()))
            .cloned()
            .ok_or_else(|| "这次工作流运行已经结束；步骤无法再被跳过".to_owned())?;
        control.request_skip(step_index);
        Ok(())
    }

    /// Binds a subagent name to an execution mode, or verifies its existing binding.
    /// Reuse with the same mode is idempotent; a different mode is rejected because names are
    /// addressable identities. Store a digest because `canonical_mode` may be 2 MiB.
    pub fn reserve_subagent_execution_mode(
        &self,
        conversation_id: &str,
        name: &str,
        canonical_mode: &str,
    ) -> Result<(), String> {
        if conversation_id.is_empty()
            || conversation_id.trim() != conversation_id
            || conversation_id.len() > 256
            || conversation_id.chars().any(char::is_control)
        {
            return Err("子代理执行身份的会话标识无效".into());
        }
        let mut name_chars = name.chars();
        if name.len() > 32
            || !name_chars
                .next()
                .is_some_and(|value| value.is_ascii_lowercase())
            || !name_chars.all(|value| {
                value.is_ascii_lowercase() || value.is_ascii_digit() || "_-".contains(value)
            })
        {
            return Err("子代理执行身份的名字无效".into());
        }
        if canonical_mode.is_empty() || canonical_mode.len() > 2 * 1024 * 1024 {
            return Err("子代理执行模式载荷超出宿主限制".into());
        }
        let digest = format!("{:x}", Sha256::digest(canonical_mode.as_bytes()));
        let mut reservations = self
            .subagent_execution_modes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match reservations.entry((conversation_id.to_owned(), name.to_owned())) {
            std::collections::hash_map::Entry::Occupied(existing) => {
                if existing.get() != &digest {
                    return Err(format!(
                        "子代理名字 {name} 在本次会话中已绑定到另一种执行模式"
                    ));
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(digest);
            }
        }
        Ok(())
    }

    /// Lists every subagent name reserved during this conversation.
    /// Derived naming must include these in-process reservations: a run can end before its tool
    /// result persists, leaving a name absent from durable and branch-level reservation records.
    pub fn reserved_subagent_names(&self, conversation_id: &str) -> Vec<String> {
        self.subagent_execution_modes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .filter(|(owner, _)| owner == conversation_id)
            .map(|(_, name)| name.clone())
            .collect()
    }

    pub fn finish_model_run(&self, request_id: &str, cancellation: &Arc<AtomicBool>) {        let mut runs = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let conversation_id = if runs
            .get(request_id)
            .is_some_and(|(current, _, _)| Arc::ptr_eq(current, cancellation))
        {
            runs.remove(request_id)
                .map(|(_, conversation_id, _)| conversation_id)
        } else {
            None
        };
        drop(runs);
        // A card raised by this run can no longer be honoured. A prompt can
        // already be open — approval is requested before execution, not after.
        // Scoped to this run's own cards: see `finish_model_run_checked`.
        if let Some(conversation_id) = conversation_id.as_deref() {
            self.tool_prompts
                .cancel_run_prompts(conversation_id, request_id);
        }
        // `run_model` uses `finish_model_run_checked`, which removes this
        // registry entry only for the matching generation. This generic path is
        // reserved for failures that occur before a turn is under way, so it
        // must not consume a bounded finished-turn tombstone.
        let _ = conversation_id;
    }

    /// Natural model completion uses a synchronous, generation-bound cleanup
    /// receipt. The registry entry stays live until cleanup has been attempted,
    /// preventing a new same-conversation run from publishing authority while an
    /// old cleanup is still in flight.
    pub fn finish_model_run_checked(
        &self,
        request_id: &str,
        cancellation: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        let conversation_id = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(request_id)
            .and_then(|(current, conversation_id, _)| {
                Arc::ptr_eq(current, cancellation).then(|| conversation_id.clone())
            });
        let Some(conversation_id) = conversation_id else {
            return Err("模型运行代次已变化；拒绝签发回合清理确认。".into());
        };

        let mut runs = self
            .model_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if runs
            .get(request_id)
            .is_some_and(|(current, _, _)| Arc::ptr_eq(current, cancellation))
        {
            runs.remove(request_id);
        }
        drop(runs);

        // A card this run raised on its own thread has nothing left to
        // authorize once the run is over, and that thread is already unwinding.
        // Retract those — and only those.
        //
        // An S11 task (workflow step, subagent, or pooled tool) outlives the
        // turn that may have started it. Its card ends only with an answer, its
        // own stop flag, or the timeout; another turn ending must not signal a
        // fabricated user rejection to its still-running worker.
        self.tool_prompts
            .cancel_run_prompts(&conversation_id, request_id);

        Ok(())
    }

    pub fn issue_tool_approval(
        &self,
        request: &ToolExecutionRequest,
        run_environment: &str,
    ) -> Result<ToolApprovalGrant, String> {
        self.approvals.issue_tool_approval(request, run_environment)
    }

    /// The in-app approval card's registry. Prompts live here rather than in
    /// `ApprovalRegistry` because they are transient UI state, not a nonce
    /// that authorizes an execution.
    pub fn tool_prompts(&self) -> &Arc<ToolPromptRegistry> {
        &self.tool_prompts
    }

    /// The tray of model-raised fork requests; see `fork_requests`.
    pub fn fork_requests(&self) -> &Arc<crate::fork_requests::ForkRequestRegistry> {
        &self.fork_requests
    }

    pub fn consume_tool_approval(
        &self,
        nonce: &str,
        request: &ToolExecutionRequest,
        run_environment: &str,
    ) -> Result<(), String> {
        self.approvals.consume_tool_approval(nonce, request, run_environment)
    }

    pub fn authorize_workspace(&self, path: &Path) -> Result<String, String> {
        self.approvals.authorize_workspace(path)
    }

    pub fn require_workspace_authorization(&self, path: &Path) -> Result<(), String> {
        self.approvals.require_workspace_authorization(path)
    }

    pub fn workspace_key(path: &Path) -> Option<String> {
        ApprovalRegistry::workspace_key(path)
    }

    pub fn record_receipt(&self, request: &ToolExecutionRequest, result: &ToolResult) {
        self.record_context_receipt(request, result, None);
    }

    pub fn record_context_receipt(
        &self,
        request: &ToolExecutionRequest,
        result: &ToolResult,
        requested_input: Option<&JsonObject>,
    ) {
        let unbound_key = requested_input.is_some().then(|| {
            provisional_receipt_key(receipt_key(
                &request.workspace_path,
                &request.conversation_id,
                &request.tool_name,
                &request.input,
                None,
                result,
            ))
        });
        let key = provisional_receipt_key(receipt_key(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            requested_input,
            result,
        ));
        let mut book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.sequence = book.sequence.wrapping_add(1);
        let sequence = book.sequence;
        if let Some(unbound_key) = unbound_key {
            book.entries.remove(&unbound_key);
        }
        book.entries.insert(key, (sequence, result.clone()));
        trim_receipts(&mut book);
    }

    /// Marks executor receipts on the current thread as provisional for one
    /// model tool invocation. They remain useful to executor code paths but
    /// cannot authorize persistence and are removed exactly when this scope
    /// exits. The run loop records the post-hook final result afterwards.
    pub(crate) fn begin_provisional_receipts(&self) -> ProvisionalReceiptScope {
        let invocation = RECEIPT_INVOCATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        ACTIVE_RECEIPT_INVOCATIONS.with(|active| active.borrow_mut().push(invocation));
        ProvisionalReceiptScope {
            invocation,
            receipts: self.receipts.clone(),
            _not_send: PhantomData,
        }
    }

    #[cfg(test)]
    /// Records a tool result together with the exact recursive child-agent
    /// audit snapshot that is allowed to enter the persisted timeline. The
    /// fingerprint commits nested tool cards and sidecars too; a normal tool
    /// receipt deliberately cannot attest a renderer-supplied subagent
    /// transcript, role, task, status, update, or queued message.
    pub fn record_subagent_receipt(
        &self,
        request: &ToolExecutionRequest,
        result: &ToolResult,
        subagent: &SubagentRunRecord,
    ) {
        self.record_context_subagent_receipt(request, result, None, subagent);
    }

    pub fn record_context_subagent_receipt(
        &self,
        request: &ToolExecutionRequest,
        result: &ToolResult,
        requested_input: Option<&JsonObject>,
        subagent: &SubagentRunRecord,
    ) {
        let unbound_subagent_key = requested_input.and_then(|_| {
            subagent_receipt_key(
                &request.workspace_path,
                &request.conversation_id,
                &request.tool_name,
                &request.input,
                None,
                result,
                subagent,
            )
            .map(provisional_receipt_key)
        });
        let ordinary_key = provisional_receipt_key(receipt_key(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            requested_input,
            result,
        ));
        let unbound_ordinary_key = requested_input.is_some().then(|| {
            provisional_receipt_key(receipt_key(
                &request.workspace_path,
                &request.conversation_id,
                &request.tool_name,
                &request.input,
                None,
                result,
            ))
        });
        let Some(key) = subagent_receipt_key(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            requested_input,
            result,
            subagent,
        )
        .map(provisional_receipt_key) else {
            return;
        };
        let mut book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.sequence = book.sequence.wrapping_add(1);
        let sequence = book.sequence;
        book.entries.remove(&ordinary_key);
        if let Some(unbound_ordinary_key) = unbound_ordinary_key {
            book.entries.remove(&unbound_ordinary_key);
        }
        if let Some(unbound_subagent_key) = unbound_subagent_key {
            book.entries.remove(&unbound_subagent_key);
        }
        book.entries.insert(key, (sequence, result.clone()));
        trim_receipts(&mut book);
    }

    #[cfg(test)]
    pub fn has_receipt(
        &self,
        workspace_path: &str,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        result: &ToolResult,
    ) -> bool {
        self.has_context_receipt(
            workspace_path,
            conversation_id,
            tool_name,
            input,
            None,
            result,
        )
    }

    pub fn has_context_receipt(
        &self,
        workspace_path: &str,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        requested_input: Option<&JsonObject>,
        result: &ToolResult,
    ) -> bool {
        let key = receipt_key(
            workspace_path,
            conversation_id,
            tool_name,
            input,
            requested_input,
            result,
        );
        let book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries
            .get(&key)
            .is_some_and(|(_, recorded)| recorded == result)
    }

    #[cfg(test)]
    pub fn has_subagent_receipt(
        &self,
        workspace_path: &str,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        result: &ToolResult,
        subagent: &SubagentRunRecord,
    ) -> bool {
        self.has_context_subagent_receipt(
            workspace_path,
            conversation_id,
            tool_name,
            input,
            None,
            result,
            subagent,
        )
    }

    pub fn has_context_subagent_receipt(
        &self,
        workspace_path: &str,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        requested_input: Option<&JsonObject>,
        result: &ToolResult,
        subagent: &SubagentRunRecord,
    ) -> bool {
        let Some(key) = subagent_receipt_key(
            workspace_path,
            conversation_id,
            tool_name,
            input,
            requested_input,
            result,
            subagent,
        ) else {
            return false;
        };
        let book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries
            .get(&key)
            .is_some_and(|(_, recorded)| recorded == result)
    }

    #[cfg(test)]
    pub fn has_receipt_in_any_workspace(
        &self,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        result: &ToolResult,
    ) -> bool {
        self.has_context_receipt_in_any_workspace(conversation_id, tool_name, input, None, result)
    }

    pub fn has_context_receipt_in_any_workspace(
        &self,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        requested_input: Option<&JsonObject>,
        result: &ToolResult,
    ) -> bool {
        let input = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
        let requested_input = requested_input
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".into()));
        let serialized_result = serde_json::to_string(result).unwrap_or_default();
        let book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries.iter().any(|(key, (_, recorded))| {
            key.invocation.is_none()
                && key.conversation_id == conversation_id
                && key.tool_name == tool_name
                && key.input == input
                && key.requested_input == requested_input
                && key.result == serialized_result
                && key.subagent_fingerprint.is_none()
                && recorded == result
        })
    }

    #[cfg(test)]
    pub fn has_subagent_receipt_in_any_workspace(
        &self,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        result: &ToolResult,
        subagent: &SubagentRunRecord,
    ) -> bool {
        self.has_context_subagent_receipt_in_any_workspace(
            conversation_id,
            tool_name,
            input,
            None,
            result,
            subagent,
        )
    }

    pub fn has_context_subagent_receipt_in_any_workspace(
        &self,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        requested_input: Option<&JsonObject>,
        result: &ToolResult,
        subagent: &SubagentRunRecord,
    ) -> bool {
        let input = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
        let requested_input = requested_input
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".into()));
        let serialized_result = serde_json::to_string(result).unwrap_or_default();
        let Some(subagent_fingerprint) = subagent_receipt_fingerprint(subagent) else {
            return false;
        };
        let book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries.iter().any(|(key, (_, recorded))| {
            key.invocation.is_none()
                && key.conversation_id == conversation_id
                && key.tool_name == tool_name
                && key.input == input
                && key.requested_input == requested_input
                && key.result == serialized_result
                && key.subagent_fingerprint.as_deref() == Some(subagent_fingerprint.as_str())
                && recorded == result
        })
    }

    pub fn has_context_receipt_or_image_removal(
        &self,
        workspace_path: Option<&str>,
        conversation_id: &str,
        tool_name: &str,
        input: &JsonObject,
        requested_input: Option<&JsonObject>,
        result: &ToolResult,
        subagent: Option<&SubagentRunRecord>,
    ) -> bool {
        let expected_workspace = workspace_path.map(|workspace_path| {
            receipt_key(
                workspace_path,
                conversation_id,
                tool_name,
                input,
                requested_input,
                result,
            )
            .workspace
        });
        let input = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
        let requested_input = requested_input
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".into()));
        let subagent_fingerprint = match subagent {
            Some(subagent) => {
                let Some(fingerprint) = subagent_receipt_fingerprint(subagent) else {
                    return false;
                };
                Some(fingerprint)
            }
            None => None,
        };
        let book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries.iter().any(|(key, (_, recorded))| {
            key.invocation.is_none()
                && expected_workspace
                    .as_ref()
                    .map_or(true, |workspace| key.workspace == *workspace)
                && key.conversation_id == conversation_id
                && key.tool_name == tool_name
                && key.input == input
                && key.requested_input == requested_input
                && key.subagent_fingerprint == subagent_fingerprint
                && key.result == serde_json::to_string(recorded).unwrap_or_default()
                && tool_result_is_exact_or_image_removal(recorded, result)
        })
    }

    pub fn clear_receipts(&self) {
        let mut book = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        book.entries.clear();
        book.epoch = book.epoch.wrapping_add(1);
        drop(book);
        self.approvals.clear();
        self.tool_prompts.clear();
        self.hook_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

fn receipt_key(
    workspace_path: &str,
    conversation_id: &str,
    tool_name: &str,
    input: &JsonObject,
    requested_input: Option<&JsonObject>,
    result: &ToolResult,
) -> ReceiptKey {
    let path = Path::new(workspace_path);
    let workspace = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let workspace = workspace.to_lowercase();
    let input = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
    let requested_input =
        requested_input.map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".into()));
    ReceiptKey {
        workspace,
        conversation_id: conversation_id.to_owned(),
        tool_name: tool_name.to_owned(),
        input,
        requested_input,
        result: serde_json::to_string(result).unwrap_or_default(),
        subagent_fingerprint: None,
        invocation: None,
    }
}

fn provisional_receipt_key(mut key: ReceiptKey) -> ReceiptKey {
    key.invocation = ACTIVE_RECEIPT_INVOCATIONS.with(|active| active.borrow().last().copied());
    key
}

fn subagent_receipt_key(
    workspace_path: &str,
    conversation_id: &str,
    tool_name: &str,
    input: &JsonObject,
    requested_input: Option<&JsonObject>,
    result: &ToolResult,
    subagent: &SubagentRunRecord,
) -> Option<ReceiptKey> {
    let mut key = receipt_key(
        workspace_path,
        conversation_id,
        tool_name,
        input,
        requested_input,
        result,
    );
    key.subagent_fingerprint = Some(subagent_receipt_fingerprint(subagent)?);
    Some(key)
}

fn subagent_receipt_fingerprint(subagent: &SubagentRunRecord) -> Option<String> {
    let serialized = serde_json::to_vec(subagent).ok()?;
    Some(format!("{:x}", Sha256::digest(serialized)))
}

pub(crate) fn tool_result_is_exact_or_image_removal(
    attested: &ToolResult,
    candidate: &ToolResult,
) -> bool {
    if attested == candidate {
        return true;
    }
    if candidate.images.len() >= attested.images.len() {
        return false;
    }

    let mut attested_without_images = attested.clone();
    let attested_images = std::mem::take(&mut attested_without_images.images);
    let mut candidate_without_images = candidate.clone();
    let candidate_images = std::mem::take(&mut candidate_without_images.images);
    if attested_without_images != candidate_without_images {
        return false;
    }

    let mut attested_index = 0;
    for candidate_image in candidate_images {
        let Some(relative_index) = attested_images[attested_index..]
            .iter()
            .position(|attested_image| attested_image == &candidate_image)
        else {
            return false;
        };
        attested_index += relative_index + 1;
    }
    true
}

fn trim_receipts(book: &mut ReceiptBook) {
    if book.entries.len() <= MAX_RECEIPTS {
        return;
    }
    let mut remove_count = book.entries.len() - MAX_RECEIPTS;
    let provisional = book
        .entries
        .keys()
        .filter(|key| key.invocation.is_some())
        .cloned()
        .take(remove_count)
        .collect::<Vec<_>>();
    for key in provisional {
        if book.entries.remove(&key).is_some() {
            remove_count -= 1;
        }
    }
    if remove_count == 0 {
        return;
    }
    // Evict fairly by conversation bucket: remove the oldest entry from the largest bucket rather
    // than globally oldest. A tool flood may reclaim only its own evidence window; ties remove older entries first.
    for _ in 0..remove_count {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for key in book.entries.keys() {
            *counts.entry(key.conversation_id.as_str()).or_default() += 1;
        }
        let Some(largest) = counts.values().copied().max() else {
            break;
        };
        let victim = book
            .entries
            .iter()
            .filter(|(key, _)| counts[key.conversation_id.as_str()] == largest)
            .min_by_key(|(_, (sequence, _))| *sequence)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            break;
        };
        book.entries.remove(&victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ContextItem, ModelUsage, SubagentRunKind, SubagentRunStatus, SubagentUpdate,
    };
    use chrono::Utc;
    use serde_json::json;
    use std::{sync::Barrier, thread};



    #[test]
    fn execution_receipts_are_bound_to_the_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let request = ToolExecutionRequest {
            conversation_id: "conversation-a".into(),
            workspace_path: directory.path().to_string_lossy().into_owned(),
            tool_name: "read".into(),
            input: serde_json::from_value(json!({"path":"README.md"})).unwrap(),
        };
        let result = ToolResult {
            success: true,
            output: "contents".into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 1,
        };
        let state = AppState::default();
        state.record_receipt(&request, &result);

        assert!(state.has_receipt(
            &request.workspace_path,
            "conversation-a",
            &request.tool_name,
            &request.input,
            &result
        ));
        assert!(!state.has_receipt(
            &request.workspace_path,
            "conversation-b",
            &request.tool_name,
            &request.input,
            &result
        ));
        assert!(state.has_receipt_in_any_workspace(
            "conversation-a",
            &request.tool_name,
            &request.input,
            &result
        ));
        assert!(!state.has_receipt_in_any_workspace(
            "conversation-b",
            &request.tool_name,
            &request.input,
            &result
        ));
    }

    /// Receipt capacity eviction is fair by conversation bucket: a flood reclaims only its own
    /// evidence window rather than evicting another conversation's needed receipt.
    #[test]
    fn a_receipt_flood_in_one_conversation_does_not_evict_anothers_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().to_string_lossy().into_owned();
        let result = ToolResult {
            success: true,
            output: "contents".into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 1,
        };
        let state = AppState::default();
        let quiet = ToolExecutionRequest {
            conversation_id: "conversation-quiet".into(),
            workspace_path: workspace.clone(),
            tool_name: "read".into(),
            input: serde_json::from_value(json!({"path":"README.md"})).unwrap(),
        };
        state.record_receipt(&quiet, &result);

        // All flood requests belong to the other conversation.
        for index in 0..(MAX_RECEIPTS + 64) {
            let noisy = ToolExecutionRequest {
                conversation_id: "conversation-noisy".into(),
                workspace_path: workspace.clone(),
                tool_name: "read".into(),
                input: serde_json::from_value(json!({ "path": format!("file-{index}.md") }))
                    .unwrap(),
            };
            state.record_receipt(&noisy, &result);
        }

        assert!(
            state.has_receipt(
                &workspace,
                "conversation-quiet",
                &quiet.tool_name,
                &quiet.input,
                &result
            ),
            "洪峰会话必须回收自己的回执，安静会话的证据不受牵连"
        );
    }

    #[test]
    fn context_receipts_bind_optional_model_requested_input() {
        let directory = tempfile::tempdir().unwrap();
        let request = ToolExecutionRequest {
            conversation_id: "conversation-requested-input".into(),
            workspace_path: directory.path().to_string_lossy().into_owned(),
            tool_name: "read".into(),
            input: serde_json::from_value(json!({"path":"executed.md"})).unwrap(),
        };
        let requested_input = serde_json::from_value(json!({"path":"original.md"})).unwrap();
        let forged_input = serde_json::from_value(json!({"path":"forged.md"})).unwrap();
        let result = ToolResult {
            success: true,
            output: "contents".into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 1,
        };
        let state = AppState::default();

        state.record_receipt(&request, &result);
        assert!(!state.has_context_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            Some(&requested_input),
            &result,
        ));

        state.record_context_receipt(&request, &result, Some(&requested_input));
        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
        ));
        assert!(state.has_context_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            Some(&requested_input),
            &result,
        ));
        assert!(state.has_context_receipt_in_any_workspace(
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            Some(&requested_input),
            &result,
        ));
        assert!(!state.has_context_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            Some(&forged_input),
            &result,
        ));
    }

    #[test]
    fn provisional_model_receipts_are_invisible_and_do_not_remove_concurrent_direct_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let request = ToolExecutionRequest {
            conversation_id: "conversation-post-tool-block".into(),
            workspace_path: directory.path().to_string_lossy().into_owned(),
            tool_name: "read".into(),
            input: serde_json::from_value(json!({"path":"blocked.png"})).unwrap(),
        };
        let image = ImageAttachment {
            id: "a".repeat(64),
            name: "blocked.png".into(),
            mime: "image/png".into(),
            width: 32,
            height: 16,
            bytes: 123,
            short_id: None,
        };
        let success = ToolResult {
            success: true,
            output: "captured".into(),
            images: vec![image],
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 1,
        };
        let blocked = ToolResult {
            success: false,
            output: "blocked by PostToolUse".into(),
            images: Vec::new(),
            diff: None,
            executed_at: success.executed_at.clone(),
            duration_ms: success.duration_ms,
        };
        let direct = ToolResult {
            success: true,
            output: "concurrent direct result".into(),
            images: Vec::new(),
            diff: None,
            executed_at: success.executed_at.clone(),
            duration_ms: 2,
        };
        let state = AppState::default();

        // This is the executor's provisional success, recorded before the
        // PostToolUse hook gets to replace the model-visible result.
        let provisional = state.begin_provisional_receipts();
        state.record_receipt(&request, &success);
        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &success,
        ));
        assert!(!state.has_receipt_in_any_workspace(
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &success,
        ));

        let direct_state = state.clone();
        let direct_request = request.clone();
        let direct_result = direct.clone();
        thread::spawn(move || direct_state.record_receipt(&direct_request, &direct_result))
            .join()
            .unwrap();
        drop(provisional);
        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &success,
        ));
        assert!(state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &direct,
        ));

        state.record_receipt(&request, &blocked);
        assert!(state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &blocked,
        ));
    }

    #[test]
    fn interleaved_model_receipt_scopes_remove_only_their_own_provisional_entries() {
        let directory = tempfile::tempdir().unwrap();
        let request = ToolExecutionRequest {
            conversation_id: "conversation-interleaved-model-receipts".into(),
            workspace_path: directory.path().to_string_lossy().into_owned(),
            tool_name: "read".into(),
            input: serde_json::from_value(json!({"path":"same.png"})).unwrap(),
        };
        let result = |output: &str| ToolResult {
            success: true,
            output: output.into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 1,
        };
        let provisional_a = result("provisional-a");
        let provisional_b = result("provisional-b");
        let final_a = result("final-a");
        let final_b = result("final-b");
        let state = Arc::new(AppState::default());
        let both_started = Arc::new(Barrier::new(2));
        let allow_a_to_finish = Arc::new(Barrier::new(2));

        let thread_state = state.clone();
        let thread_request = request.clone();
        let thread_started = both_started.clone();
        let thread_finish = allow_a_to_finish.clone();
        let thread_provisional = provisional_a.clone();
        let thread_final = final_a.clone();
        let call_a = thread::spawn(move || {
            let scope = thread_state.begin_provisional_receipts();
            thread_state.record_receipt(&thread_request, &thread_provisional);
            thread_started.wait();
            thread_finish.wait();
            drop(scope);
            thread_state.record_receipt(&thread_request, &thread_final);
        });

        let scope_b = state.begin_provisional_receipts();
        state.record_receipt(&request, &provisional_b);
        both_started.wait();
        drop(scope_b);
        state.record_receipt(&request, &final_b);
        allow_a_to_finish.wait();
        call_a.join().unwrap();

        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &provisional_a,
        ));
        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &provisional_b,
        ));
        assert!(state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &final_a,
        ));
        assert!(state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &final_b,
        ));
    }

    #[test]
    fn subagent_receipts_bind_the_exact_audit_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let request = ToolExecutionRequest {
            conversation_id: "conversation-agent".into(),
            workspace_path: directory.path().to_string_lossy().into_owned(),
            tool_name: "web_search".into(),
            input: serde_json::from_value(json!({
                "objective": "Summarize the current state of https://example.com/report"
            }))
            .unwrap(),
        };
        let result = ToolResult {
            success: true,
            output: "structured report".into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 12,
        };
        let subagent = SubagentRunRecord {
            kind: SubagentRunKind::WorkflowStep,
            name: None,
            label: None,
            inherits_model_memory: false,
            fork_model_binding: None,
            agent_definition: None,
            task: "Inspect the report".into(),
            status: SubagentRunStatus::Completed,
            contexts: vec![ContextItem::Assistant {
                id: "research-answer".into(),
                content: "verified".into(),
                round: Some(1),
                model_turn_id: Some("research-turn".into()),
                interrupted: false,
                sources: Vec::new(),
                created_at: "2026-07-24T00:00:00Z".into(),
            }],
            updates: vec![SubagentUpdate {
                content: "captured the page".into(),
                created_at: "2026-07-24T00:00:00Z".into(),
            }],
            queued_messages: Vec::new(),
            execution_mode_receipt: String::new(),
            structured_output: None,
            output_schema: None,
            usage: ModelUsage::default(),
        };
        let state = AppState::default();

        state.record_receipt(&request, &result);
        assert!(!state.has_subagent_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
            &subagent,
        ));

        state.record_subagent_receipt(&request, &result, &subagent);
        assert!(!state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
        ));
        assert!(state.has_subagent_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
            &subagent,
        ));
        assert!(state.has_subagent_receipt_in_any_workspace(
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
            &subagent,
        ));

        let mut forged = subagent;
        forged.task = "renderer-supplied task".into();
        assert!(!state.has_subagent_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
            &forged,
        ));
    }

    #[test]
    fn model_run_cancellation_is_scoped_and_cleaned_up() {
        let state = AppState::default();
        let (cancellation, steer_inbox) = state.begin_model_run("run-a", "conversation-a").unwrap();

        assert!(!cancellation.load(Ordering::Acquire));
        assert!(state.conversation_model_run_active("conversation-a"));
        assert!(!state.conversation_model_run_active("conversation-b"));
        state
            .steer_model_run(
                "run-a",
                "queued-1".into(),
                "  补充要求  ".into(),
                Vec::new(),
                "2026-07-24T00:00:00Z".into(),
            )
            .unwrap();
        assert_eq!(
            steer_inbox.drain(),
            [MailboxMessage {
                id: Some("queued-1".into()),
                content: "补充要求".into(),
                images: Vec::new(),
                created_at: Some("2026-07-24T00:00:00Z".into()),
            }]
        );
        assert!(state.cancel_model_run("run-a").unwrap());
        assert!(cancellation.load(Ordering::Acquire));
        assert!(!state.cancel_model_run("run-b").unwrap());

        state.finish_model_run("run-a", &cancellation);
        assert!(!state.cancel_model_run("run-a").unwrap());
        assert!(!state.conversation_model_run_active("conversation-a"));
    }

    #[test]
    fn steer_model_run_rejects_aggregate_image_list_budgets_before_delivery() {
        fn image(index: usize, bytes: u64, width: u32, height: u32) -> ImageAttachment {
            ImageAttachment {
                id: format!("{:064x}", index + 1),
                name: format!("steer-{index}.png"),
                mime: "image/png".into(),
                width,
                height,
                bytes,
                short_id: None,
            }
        }

        let state = AppState::default();
        let (_, inbox) = state
            .begin_model_run("run-image-budget", "conversation-image-budget")
            .unwrap();
        let oversized_bytes = (0..5)
            .map(|index| {
                image(
                    index,
                    crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64,
                    1,
                    1,
                )
            })
            .collect();
        let error = state
            .steer_model_run(
                "run-image-budget",
                "queued-image-bytes".into(),
                String::new(),
                oversized_bytes,
                "2026-07-24T00:00:00Z".into(),
            )
            .unwrap_err();
        assert!(error.contains("引导消息"));
        assert!(error.contains("20 MiB"));
        assert!(inbox.drain().is_empty());

        let oversized_pixels = (0..5).map(|index| image(index, 1, 4096, 4096)).collect();
        let error = state
            .steer_model_run(
                "run-image-budget",
                "queued-image-pixels".into(),
                String::new(),
                oversized_pixels,
                "2026-07-24T00:00:00Z".into(),
            )
            .unwrap_err();
        assert!(error.contains("引导消息"));
        assert!(error.contains("64 MP"));
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn retirement_racing_runtime_acquisition_never_resurrects_the_mapping() {
        for _ in 0..16 {
            let state = AppState::default();
            let start = Arc::new(std::sync::Barrier::new(2));
            let worker_state = state.clone();
            let worker_start = Arc::clone(&start);
            let worker = std::thread::spawn(move || {
                worker_start.wait();
                worker_state.conversation_tasks("retired-owner")
            });
            start.wait();
            state.retire_conversation_tasks("retired-owner");
            let _old_runtime = worker.join().unwrap();
            assert!(state.existing_conversation_tasks("retired-owner").is_none());
            assert!(state.begin_model_run("late-run", "retired-owner").is_err());
        }
    }

    #[test]
    fn model_runs_reject_duplicate_ids_and_parallel_owners() {
        let state = AppState::default();
        let (first, _) = state.begin_model_run("run-a", "conversation-a").unwrap();

        assert!(state
            .begin_model_run("run-a", "conversation-b")
            .err()
            .unwrap()
            .contains("ID"));
        assert!(state
            .begin_model_run("run-b", "conversation-a")
            .err()
            .unwrap()
            .contains("已有"));
        assert!(state.begin_model_run("run-b", "conversation-b").is_ok());

        state.finish_model_run("run-a", &first);
        assert!(state.begin_model_run("run-c", "conversation-a").is_ok());
    }

    #[test]
    fn model_run_cancellation_publishes_the_stop_without_unregistering_the_generation() {
        let state = AppState::default();
        let (cancellation, _) = state.begin_model_run("run-a", "conversation-a").unwrap();

        assert!(state.conversation_model_run_active("conversation-a"));
        assert!(state.cancel_model_run("run-a").unwrap());
        assert!(cancellation.load(Ordering::Acquire));
        // A cancellation acknowledgement only publishes the stop request. The
        // generation remains registered until its worker has actually exited.
        assert!(state.conversation_model_run_active("conversation-a"));
        state.finish_model_run("run-a", &cancellation);
        assert!(!state.conversation_model_run_active("conversation-a"));
    }

    /// Cancelling a run sets only that run's flag and does not sweep conversation command rows.
    /// Those rows may belong to background commands, cross-turn tasks, or manual reruns. Each
    /// run's own synchronous legs stop through `RunModelRequest::round_cancellation`.
    #[test]
    fn cancelling_a_run_does_not_sweep_the_conversations_command_rows() {
        let state = AppState::default();
        let (_cancellation, _) = state.begin_model_run("run-a", "conversation-a").unwrap();
        let sync_row = state
            .shell_tasks
            .register("conversation-a", "bash", "sleep 20", false);
        let background_row = state
            .shell_tasks
            .register("conversation-a", "bash", "npm run watch", true);

        assert!(state.cancel_model_run("run-a").unwrap());
        assert!(
            !sync_row.stop_requested(),
            "同步登记行不归运行级取消清扫——它可能属于一个存活的任务或一次手动重跑"
        );
        assert!(!background_row.stop_requested(), "后台登记行更不归它");
    }

    #[test]
    fn operation_guards_track_nested_activity_and_early_drops() {
        let state = AppState::default();
        assert!(!state.has_active_operations());

        let first = state.begin_operation().unwrap();
        assert!(state.has_active_operations());
        {
            let cloned_state = state.clone();
            let _second = cloned_state.begin_operation().unwrap();
            assert!(state.has_active_operations());
        }
        assert!(state.has_active_operations());
        assert!(state.begin_mutation().is_err());

        drop(first);
        assert!(!state.has_active_operations());

        let mutation = state.begin_mutation().unwrap();
        assert!(state.has_active_operations());
        assert!(state.begin_operation().is_err());
        assert!(state.begin_mutation().is_err());
        drop(mutation);
        assert!(!state.has_active_operations());
        drop(state.begin_operation().unwrap());
    }

    #[test]
    fn workspace_guards_allow_independent_repositories_but_block_legacy_writers() {
        let gate = OperationGate::default();
        let global_reader = gate.begin_operation().unwrap();
        let workspace_reader = gate
            .begin_workspace_operation(
                WorkspaceKey::from("workspace-a"),
                Some("conversation-a".into()),
            )
            .unwrap();
        assert!(gate
            .begin_workspace_mutation(WorkspaceKey::from("workspace-b"), None)
            .is_err());
        drop(global_reader);
        drop(workspace_reader);

        let first_writer = gate
            .begin_workspace_mutation(WorkspaceKey::from("workspace-a"), None)
            .unwrap();
        let second_writer = gate
            .begin_workspace_mutation(WorkspaceKey::from("workspace-b"), None)
            .unwrap();
        assert!(gate.begin_operation().is_err());
        assert!(gate
            .begin_workspace_operation(WorkspaceKey::from("workspace-a"), None)
            .is_err());
        drop(first_writer);
        drop(second_writer);
        assert!(!gate.has_active_operations());
    }
}
