//! Host-local `InstructionsLoaded` hook contract.
//!
//! Claude Code defines this event as asynchronous observability: its matcher
//! runs against `load_reason`, and neither command output nor exit status may
//! block a run or add provider context.  This module intentionally stays
//! separate from the synchronous decision-capable hook pipeline.

use std::{
    collections::HashSet,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    thread,
    time::Duration,
};

use regex::Regex;
use serde::Serialize;

use super::HookCommandRunner;
use crate::model::{HookDefinition, HookEvent};

const MAX_ID_BYTES: usize = 512;
const MAX_PATH_BYTES: usize = 32 * 1024;
const MAX_MATCHER_BYTES: usize = 64 * 1024;
const MAX_GLOB_BYTES: usize = 8 * 1024;
const MAX_TOTAL_GLOB_BYTES: usize = 64 * 1024;
const MAX_GLOBS: usize = 1_024;

pub(crate) const DEFAULT_INSTRUCTIONS_LOADED_QUEUE_CAPACITY: usize = 256;
pub(crate) const INSTRUCTIONS_LOADED_EVENT_NAME: &str = "InstructionsLoaded";
const MAX_ASYNC_HOOK_RUNTIME: Duration = Duration::from_secs(10 * 60);
const DEFAULT_INSTRUCTIONS_LOADED_WORKERS: usize = 4;

/// Mework's permission mode is host-derived from the current run. It is
/// included only because ordinary Mework command hooks already receive it.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InstructionsLoadedPermissionMode {
    Default,
    Plan,
    Auto,
    AcceptEdits,
    DontAsk,
    BypassPermissions,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum InstructionsLoadedMemoryType {
    #[serde(rename = "User")]
    User,
    #[serde(rename = "Project")]
    Project,
    #[serde(rename = "Local")]
    Local,
    #[serde(rename = "Managed")]
    Managed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstructionsLoadedReason {
    SessionStart,
    NestedTraversal,
    PathGlobMatch,
    Include,
}

impl InstructionsLoadedReason {
    pub(crate) fn matcher_value(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::NestedTraversal => "nested_traversal",
            Self::PathGlobMatch => "path_glob_match",
            Self::Include => "include",
        }
    }
}

/// Host-only receipt minted only after the matching durable context-manifest
/// entry has committed. Keeping the absolute verified paths in this opaque
/// type prevents callers from accidentally turning hook provenance into
/// provider or renderer context.
pub(crate) struct ManifestedInstructionLoad {
    file_path: PathBuf,
    memory_type: InstructionsLoadedMemoryType,
    load_reason: InstructionsLoadedReason,
    globs: Option<Vec<String>>,
    trigger_file_path: Option<PathBuf>,
    parent_file_path: Option<PathBuf>,
}

impl ManifestedInstructionLoad {
    pub(crate) fn session_start(
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
    ) -> Self {
        Self::without_reason_metadata(
            file_path,
            memory_type,
            InstructionsLoadedReason::SessionStart,
        )
    }

    pub(crate) fn nested_traversal(
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        trigger_file_path: &Path,
    ) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            memory_type,
            load_reason: InstructionsLoadedReason::NestedTraversal,
            globs: None,
            trigger_file_path: Some(trigger_file_path.to_path_buf()),
            parent_file_path: None,
        }
    }

    pub(crate) fn path_glob_match(
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        globs: &[String],
        trigger_file_path: &Path,
    ) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            memory_type,
            load_reason: InstructionsLoadedReason::PathGlobMatch,
            globs: Some(globs.to_vec()),
            trigger_file_path: Some(trigger_file_path.to_path_buf()),
            parent_file_path: None,
        }
    }

    pub(crate) fn include(
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        parent_file_path: &Path,
    ) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            memory_type,
            load_reason: InstructionsLoadedReason::Include,
            globs: None,
            trigger_file_path: None,
            parent_file_path: Some(parent_file_path.to_path_buf()),
        }
    }

    fn without_reason_metadata(
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        load_reason: InstructionsLoadedReason,
    ) -> Self {
        debug_assert!(matches!(load_reason, InstructionsLoadedReason::SessionStart));
        Self {
            file_path: file_path.to_path_buf(),
            memory_type,
            load_reason,
            globs: None,
            trigger_file_path: None,
            parent_file_path: None,
        }
    }
}

/// Common input captured only from a trusted run request.
///
/// The exact model ID is preserved byte-for-byte and case-sensitively. No
/// provider identifier, digest, renderer value, or model-authored field is
/// accepted as an owner identity.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InstructionsLoadedHostContext {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_path: Option<String>,
    cwd: String,
    #[serde(rename = "model")]
    owner_model_id: String,
    permission_mode: InstructionsLoadedPermissionMode,
    turn_id: String,
}

impl InstructionsLoadedHostContext {
    pub(crate) fn new(
        session_id: impl Into<String>,
        transcript_path: Option<&Path>,
        cwd: &Path,
        owner_model_id: impl Into<String>,
        permission_mode: InstructionsLoadedPermissionMode,
        turn_id: impl Into<String>,
    ) -> Result<Self, InstructionsLoadedInputError> {
        let session_id = validate_exact_id(session_id.into(), "session_id")?;
        let owner_model_id = validate_exact_id(owner_model_id.into(), "owner_model_id")?;
        let turn_id = validate_exact_id(turn_id.into(), "turn_id")?;
        let transcript_path = transcript_path
            .map(|path| absolute_utf8_path(path, "transcript_path"))
            .transpose()?;
        Ok(Self {
            session_id,
            transcript_path,
            cwd: absolute_utf8_path(cwd, "cwd")?,
            owner_model_id,
            permission_mode,
            turn_id,
        })
    }

    pub(crate) fn owner_model_id(&self) -> &str {
        &self.owner_model_id
    }
}

/// JSON DTO written only to local hook stdin.
///
/// It deliberately implements serialization but not deserialization, keeping
/// the renderer, provider, and model outside the event-construction boundary.
/// It also contains no instruction body or provider identifier.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InstructionsLoadedInput {
    #[serde(flatten)]
    host: InstructionsLoadedHostContext,
    hook_event_name: &'static str,
    file_path: String,
    memory_type: InstructionsLoadedMemoryType,
    load_reason: InstructionsLoadedReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    globs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_file_path: Option<String>,
}

impl InstructionsLoadedInput {
    pub(crate) fn session_start(
        host: &InstructionsLoadedHostContext,
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
    ) -> Result<Self, InstructionsLoadedInputError> {
        Self::without_reason_metadata(
            host,
            file_path,
            memory_type,
            InstructionsLoadedReason::SessionStart,
        )
    }

    pub(crate) fn nested_traversal(
        host: &InstructionsLoadedHostContext,
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        trigger_file_path: &Path,
    ) -> Result<Self, InstructionsLoadedInputError> {
        Ok(Self {
            host: host.clone(),
            hook_event_name: INSTRUCTIONS_LOADED_EVENT_NAME,
            file_path: absolute_utf8_path(file_path, "file_path")?,
            memory_type,
            load_reason: InstructionsLoadedReason::NestedTraversal,
            globs: None,
            trigger_file_path: Some(absolute_utf8_path(trigger_file_path, "trigger_file_path")?),
            parent_file_path: None,
        })
    }

    pub(crate) fn path_glob_match(
        host: &InstructionsLoadedHostContext,
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        globs: Vec<String>,
        trigger_file_path: &Path,
    ) -> Result<Self, InstructionsLoadedInputError> {
        validate_globs(&globs)?;
        Ok(Self {
            host: host.clone(),
            hook_event_name: INSTRUCTIONS_LOADED_EVENT_NAME,
            file_path: absolute_utf8_path(file_path, "file_path")?,
            memory_type,
            load_reason: InstructionsLoadedReason::PathGlobMatch,
            globs: Some(globs),
            trigger_file_path: Some(absolute_utf8_path(trigger_file_path, "trigger_file_path")?),
            parent_file_path: None,
        })
    }

    pub(crate) fn include(
        host: &InstructionsLoadedHostContext,
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        parent_file_path: &Path,
    ) -> Result<Self, InstructionsLoadedInputError> {
        Ok(Self {
            host: host.clone(),
            hook_event_name: INSTRUCTIONS_LOADED_EVENT_NAME,
            file_path: absolute_utf8_path(file_path, "file_path")?,
            memory_type,
            load_reason: InstructionsLoadedReason::Include,
            globs: None,
            trigger_file_path: None,
            parent_file_path: Some(absolute_utf8_path(parent_file_path, "parent_file_path")?),
        })
    }

    fn without_reason_metadata(
        host: &InstructionsLoadedHostContext,
        file_path: &Path,
        memory_type: InstructionsLoadedMemoryType,
        load_reason: InstructionsLoadedReason,
    ) -> Result<Self, InstructionsLoadedInputError> {
        debug_assert!(matches!(load_reason, InstructionsLoadedReason::SessionStart));
        Ok(Self {
            host: host.clone(),
            hook_event_name: INSTRUCTIONS_LOADED_EVENT_NAME,
            file_path: absolute_utf8_path(file_path, "file_path")?,
            memory_type,
            load_reason,
            globs: None,
            trigger_file_path: None,
            parent_file_path: None,
        })
    }

    pub(crate) fn load_reason(&self) -> InstructionsLoadedReason {
        self.load_reason
    }

    pub(crate) fn file_path(&self) -> &str {
        &self.file_path
    }

    pub(crate) fn to_local_hook_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

#[derive(Clone)]
pub(crate) struct InstructionsLoadedMatcher {
    pattern: Option<Regex>,
}

impl InstructionsLoadedMatcher {
    pub(crate) fn compile(value: Option<&str>) -> Result<Self, InstructionsLoadedMatcherError> {
        let value = value.map(str::trim).unwrap_or("");
        if value.is_empty() || value == "*" {
            return Ok(Self { pattern: None });
        }
        if value.len() > MAX_MATCHER_BYTES {
            return Err(InstructionsLoadedMatcherError::TooLong);
        }
        let pattern = Regex::new(value)
            .map_err(|_| InstructionsLoadedMatcherError::InvalidRegularExpression)?;
        Ok(Self {
            pattern: Some(pattern),
        })
    }

    pub(crate) fn matches(&self, reason: InstructionsLoadedReason) -> bool {
        self.pattern
            .as_ref()
            .map_or(true, |pattern| pattern.is_match(reason.matcher_value()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstructionsLoadedMatcherError {
    TooLong,
    InvalidRegularExpression,
}

impl fmt::Display for InstructionsLoadedMatcherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => formatter.write_str("InstructionsLoaded matcher exceeds its limit"),
            Self::InvalidRegularExpression => {
                formatter.write_str("InstructionsLoaded matcher is not a valid regular expression")
            }
        }
    }
}

impl std::error::Error for InstructionsLoadedMatcherError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstructionsLoadedDispatchOutcome {
    NotMatched,
    Enqueued,
    QueueFull,
    QueueClosed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InstructionsLoadedDispatchMetrics {
    pub enqueued: u64,
    pub completed: u64,
    pub handler_failures: u64,
    pub handler_panics: u64,
    pub dropped_queue_full: u64,
    pub dropped_queue_closed: u64,
}

#[derive(Default)]
struct AtomicDispatchMetrics {
    enqueued: AtomicU64,
    completed: AtomicU64,
    handler_failures: AtomicU64,
    handler_panics: AtomicU64,
    dropped_queue_full: AtomicU64,
    dropped_queue_closed: AtomicU64,
}

/// Opaque invocation passed to the host-local command executor.
///
/// The generic hook payload lets the eventual runtime carry an immutable
/// command definition without coupling this boundary to the decision-capable
/// `HookEvent` enum.
pub(crate) struct InstructionsLoadedInvocation<H> {
    hook: H,
    input: InstructionsLoadedInput,
}

impl<H> InstructionsLoadedInvocation<H> {
    pub(crate) fn into_parts(self) -> (H, InstructionsLoadedInput) {
        (self.hook, self.input)
    }
}

/// Bounded, non-waiting dispatcher for observability-only hook work.
///
/// `try_dispatch` never waits for a hook process, queue capacity, or handler
/// result. A fixed worker pool lets matching hooks for one event run in
/// parallel without spawning an unbounded thread per event. Mework bounds the
/// official parallel-hook behavior to four workers as a host safety extension.
/// Panics are isolated so one hook cannot permanently disable later
/// observations.
pub(crate) struct AsyncInstructionsLoadedDispatcher<H> {
    senders: Vec<SyncSender<InstructionsLoadedInvocation<H>>>,
    next_sender: Arc<AtomicU64>,
    metrics: Arc<AtomicDispatchMetrics>,
}

impl<H> Clone for AsyncInstructionsLoadedDispatcher<H> {
    fn clone(&self) -> Self {
        Self {
            senders: self.senders.clone(),
            next_sender: Arc::clone(&self.next_sender),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

impl<H: Send + 'static> AsyncInstructionsLoadedDispatcher<H> {
    pub(crate) fn new(
        queue_capacity: usize,
        handler: impl Fn(InstructionsLoadedInvocation<H>) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Result<Self, InstructionsLoadedDispatcherError> {
        if queue_capacity == 0 {
            return Err(InstructionsLoadedDispatcherError::ZeroQueueCapacity);
        }
        let worker_count = DEFAULT_INSTRUCTIONS_LOADED_WORKERS.min(queue_capacity);
        let metrics = Arc::new(AtomicDispatchMetrics::default());
        let handler = Arc::new(handler);
        let mut senders = Vec::with_capacity(worker_count);
        let base_capacity = queue_capacity / worker_count;
        let capacity_remainder = queue_capacity % worker_count;
        for worker_index in 0..worker_count {
            let worker_capacity = base_capacity + usize::from(worker_index < capacity_remainder);
            let (sender, receiver) = mpsc::sync_channel(worker_capacity);
            let worker_metrics = Arc::clone(&metrics);
            let handler = Arc::clone(&handler);
            thread::Builder::new()
                .name(format!("mework-instructions-loaded-{worker_index}"))
                .spawn(move || {
                    while let Ok(invocation) = receiver.recv() {
                        let result = catch_unwind(AssertUnwindSafe(|| handler(invocation)));
                        match result {
                            Ok(Ok(())) => {
                                worker_metrics.completed.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(Err(())) => {
                                worker_metrics
                                    .handler_failures
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            Err(_) => {
                                worker_metrics
                                    .handler_panics
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                })
                .map_err(|_| InstructionsLoadedDispatcherError::WorkerSpawnFailed)?;
            senders.push(sender);
        }
        Ok(Self {
            senders,
            next_sender: Arc::new(AtomicU64::new(0)),
            metrics,
        })
    }

    pub(crate) fn try_dispatch(
        &self,
        matcher: &InstructionsLoadedMatcher,
        hook: H,
        input: InstructionsLoadedInput,
    ) -> InstructionsLoadedDispatchOutcome {
        if !matcher.matches(input.load_reason()) {
            return InstructionsLoadedDispatchOutcome::NotMatched;
        }
        let start =
            (self.next_sender.fetch_add(1, Ordering::Relaxed) % self.senders.len() as u64) as usize;
        let mut invocation = InstructionsLoadedInvocation { hook, input };
        let mut saw_full = false;
        self.metrics.enqueued.fetch_add(1, Ordering::Relaxed);
        for offset in 0..self.senders.len() {
            let sender = &self.senders[(start + offset) % self.senders.len()];
            match sender.try_send(invocation) {
                Ok(()) => {
                    return InstructionsLoadedDispatchOutcome::Enqueued;
                }
                Err(TrySendError::Full(returned)) => {
                    invocation = returned;
                    saw_full = true;
                }
                Err(TrySendError::Disconnected(returned)) => {
                    invocation = returned;
                }
            }
        }
        self.metrics.enqueued.fetch_sub(1, Ordering::Relaxed);
        if saw_full {
            self.metrics
                .dropped_queue_full
                .fetch_add(1, Ordering::Relaxed);
            InstructionsLoadedDispatchOutcome::QueueFull
        } else {
            self.metrics
                .dropped_queue_closed
                .fetch_add(1, Ordering::Relaxed);
            InstructionsLoadedDispatchOutcome::QueueClosed
        }
    }

    pub(crate) fn metrics(&self) -> InstructionsLoadedDispatchMetrics {
        InstructionsLoadedDispatchMetrics {
            enqueued: self.metrics.enqueued.load(Ordering::Relaxed),
            completed: self.metrics.completed.load(Ordering::Relaxed),
            handler_failures: self.metrics.handler_failures.load(Ordering::Relaxed),
            handler_panics: self.metrics.handler_panics.load(Ordering::Relaxed),
            dropped_queue_full: self.metrics.dropped_queue_full.load(Ordering::Relaxed),
            dropped_queue_closed: self.metrics.dropped_queue_closed.load(Ordering::Relaxed),
        }
    }
}

/// Keeps verified host paths in the local hook execution namespace.
#[derive(Clone)]
pub(crate) struct InstructionsLoadedPathMode;

impl InstructionsLoadedPathMode {
    pub(crate) fn host() -> Self {
        Self
    }

    fn map(&self, verified_host_path: &Path) -> Option<PathBuf> {
        Some(verified_host_path.to_path_buf())
    }
}

#[derive(Clone)]
struct PreparedInstructionsLoadedHook {
    definition: HookDefinition,
    matcher: InstructionsLoadedMatcher,
}

/// Per-model-run bridge from committed manifest receipts to the local command
/// runner. Dispatch uses a bounded `try_send`; it never waits for a command,
/// output, exit status, or decision payload.
pub(crate) struct InstructionsLoadedRunDispatcher {
    host: InstructionsLoadedHostContext,
    paths: InstructionsLoadedPathMode,
    hooks: Vec<PreparedInstructionsLoadedHook>,
    dispatcher: AsyncInstructionsLoadedDispatcher<HookDefinition>,
}

/// Maps loaded project instructions into `InstructionsLoaded` hook inputs.
///
/// The mapping records why an instruction was loaded. `nested_traversal` and `path_glob_match` require a host-validated triggering file; `path_glob_match` also requires its path rule; `include` requires its parent instruction path. Missing data is an error because hooks must not receive incomplete facts.
pub(crate) fn manifested_instruction_loads(
    sources: &[&crate::project_memory::ProjectMemorySource],
    trigger_verified_path: Option<&Path>,
) -> Result<Vec<ManifestedInstructionLoad>, String> {
    use crate::project_memory::{ProjectMemoryReason, ProjectMemoryScope};

    let mut manifested = Vec::with_capacity(sources.len());
    for source in sources
        .iter()
        .copied()
        .filter(|source| source.content.is_some())
    {
        let reason = match source.reason() {
            ProjectMemoryReason::Imported => InstructionsLoadedReason::Include,
            ProjectMemoryReason::NestedTraversal => InstructionsLoadedReason::NestedTraversal,
            ProjectMemoryReason::RuleWithPaths => InstructionsLoadedReason::PathGlobMatch,
            ProjectMemoryReason::StartupHierarchy
            | ProjectMemoryReason::LocalOverride
            | ProjectMemoryReason::RuleWithoutPaths => InstructionsLoadedReason::SessionStart,
        };
        let memory_type = match source.instruction_scope() {
            ProjectMemoryScope::Managed => InstructionsLoadedMemoryType::Managed,
            ProjectMemoryScope::User | ProjectMemoryScope::UserRule => {
                InstructionsLoadedMemoryType::User
            }
            ProjectMemoryScope::AncestorLocal | ProjectMemoryScope::WorkspaceLocal => {
                InstructionsLoadedMemoryType::Local
            }
            ProjectMemoryScope::Ancestor
            | ProjectMemoryScope::Workspace
            | ProjectMemoryScope::AncestorRule
            | ProjectMemoryScope::WorkspaceRule
            | ProjectMemoryScope::Import => InstructionsLoadedMemoryType::Project,
        };
        let load = match reason {
            InstructionsLoadedReason::SessionStart => {
                ManifestedInstructionLoad::session_start(source.verified_path(), memory_type)
            }
            InstructionsLoadedReason::NestedTraversal => {
                let Some(trigger) = trigger_verified_path else {
                    return Err(format!(
                        "{} 指令缺少宿主验证的触发文件路径；已拒绝派发不完整的加载事实",
                        source.safe_label()
                    ));
                };
                ManifestedInstructionLoad::nested_traversal(
                    source.verified_path(),
                    memory_type,
                    trigger,
                )
            }
            InstructionsLoadedReason::PathGlobMatch => {
                let Some(trigger) = trigger_verified_path else {
                    return Err(format!(
                        "{} 指令缺少宿主验证的触发文件路径；已拒绝派发不完整的加载事实",
                        source.safe_label()
                    ));
                };
                if source.instruction_path_patterns().is_empty() {
                    return Err(format!(
                        "{} 的 path_glob_match 缺少路径规则；已拒绝派发不完整的加载事实",
                        source.safe_label()
                    ));
                }
                ManifestedInstructionLoad::path_glob_match(
                    source.verified_path(),
                    memory_type,
                    source.instruction_path_patterns(),
                    trigger,
                )
            }
            InstructionsLoadedReason::Include => {
                let Some(parent) = source.verified_parent_path() else {
                    return Err(format!(
                        "{} 的 include 缺少宿主验证的父指令路径；已拒绝派发不完整的加载事实",
                        source.safe_label()
                    ));
                };
                ManifestedInstructionLoad::include(source.verified_path(), memory_type, parent)
            }
        };
        manifested.push(load);
    }
    Ok(manifested)
}

impl InstructionsLoadedRunDispatcher {
    pub(crate) fn new(
        hooks: &[HookDefinition],
        host: InstructionsLoadedHostContext,
        paths: InstructionsLoadedPathMode,
        runner: HookCommandRunner,
    ) -> Option<Self> {
        let hooks = hooks
            .iter()
            .filter(|hook| hook.enabled && hook.event == HookEvent::InstructionsLoaded)
            .filter_map(|definition| {
                InstructionsLoadedMatcher::compile(definition.matcher.as_deref())
                    .ok()
                    .map(|matcher| PreparedInstructionsLoadedHook {
                        definition: definition.clone(),
                        matcher,
                    })
            })
            .collect::<Vec<_>>();
        if hooks.is_empty() {
            return None;
        }
        let dispatcher = AsyncInstructionsLoadedDispatcher::new(
            DEFAULT_INSTRUCTIONS_LOADED_QUEUE_CAPACITY,
            move |invocation: InstructionsLoadedInvocation<HookDefinition>| {
                let (hook, input) = invocation.into_parts();
                let input = serde_json::to_value(input).map_err(|_| ())?;
                let remaining = Duration::from_millis(hook.timeout_ms)
                    .min(MAX_ASYNC_HOOK_RUNTIME)
                    .max(Duration::from_millis(1));
                // InstructionsLoaded is observation-only. Stderr/stdout, exit
                // 2, and JSON decisions are intentionally discarded and
                // cannot reach provider state. A runner/spawn/timeout failure
                // is retained only in the dispatcher's local metrics.
                runner(&hook, &input, remaining).map(|_| ()).map_err(|_| ())
            },
        )
        .ok()?;
        Some(Self {
            host,
            paths,
            hooks,
            dispatcher,
        })
    }

    pub(crate) fn dispatch_manifested(&self, loads: &[ManifestedInstructionLoad]) {
        for load in loads {
            let Some(input) = self.input_for(load) else {
                continue;
            };
            let mut execution_identities = HashSet::new();
            for hook in &self.hooks {
                if !hook.matcher.matches(input.load_reason()) {
                    continue;
                }
                if !execution_identities
                    .insert(instructions_loaded_execution_identity(&hook.definition))
                {
                    continue;
                }
                let _ = self.dispatcher.try_dispatch(
                    &hook.matcher,
                    hook.definition.clone(),
                    input.clone(),
                );
            }
        }
    }

    fn input_for(&self, load: &ManifestedInstructionLoad) -> Option<InstructionsLoadedInput> {
        let file_path = self.paths.map(&load.file_path)?;
        match load.load_reason {
            InstructionsLoadedReason::SessionStart => {
                InstructionsLoadedInput::session_start(&self.host, &file_path, load.memory_type)
                    .ok()
            }
            InstructionsLoadedReason::NestedTraversal => {
                let trigger = self.paths.map(load.trigger_file_path.as_deref()?)?;
                InstructionsLoadedInput::nested_traversal(
                    &self.host,
                    &file_path,
                    load.memory_type,
                    &trigger,
                )
                .ok()
            }
            InstructionsLoadedReason::PathGlobMatch => {
                let trigger = self.paths.map(load.trigger_file_path.as_deref()?)?;
                InstructionsLoadedInput::path_glob_match(
                    &self.host,
                    &file_path,
                    load.memory_type,
                    load.globs.clone()?,
                    &trigger,
                )
                .ok()
            }
            InstructionsLoadedReason::Include => {
                let parent = self.paths.map(load.parent_file_path.as_deref()?)?;
                InstructionsLoadedInput::include(&self.host, &file_path, load.memory_type, &parent)
                    .ok()
            }
        }
    }
}

fn instructions_loaded_execution_identity(hook: &HookDefinition) -> String {
    #[cfg(windows)]
    let command = hook
        .command_windows
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&hook.command);
    #[cfg(not(windows))]
    let command = hook.command.as_str();
    command.trim().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstructionsLoadedDispatcherError {
    ZeroQueueCapacity,
    WorkerSpawnFailed,
}

impl fmt::Display for InstructionsLoadedDispatcherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroQueueCapacity => {
                formatter.write_str("InstructionsLoaded queue capacity must be positive")
            }
            Self::WorkerSpawnFailed => {
                formatter.write_str("InstructionsLoaded worker could not be started")
            }
        }
    }
}

impl std::error::Error for InstructionsLoadedDispatcherError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstructionsLoadedInputError {
    EmptyField(&'static str),
    SurroundingWhitespace(&'static str),
    ControlCharacter(&'static str),
    FieldTooLong(&'static str),
    PathNotAbsolute(&'static str),
    PathNotUtf8(&'static str),
    EmptyGlobs,
    TooManyGlobs,
    GlobsTooLarge,
    InvalidGlob,
}

impl fmt::Display for InstructionsLoadedInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(label) => write!(formatter, "{label} cannot be empty"),
            Self::SurroundingWhitespace(label) => {
                write!(formatter, "{label} cannot contain surrounding whitespace")
            }
            Self::ControlCharacter(label) => {
                write!(formatter, "{label} cannot contain control characters")
            }
            Self::FieldTooLong(label) => write!(formatter, "{label} exceeds its size limit"),
            Self::PathNotAbsolute(label) => write!(formatter, "{label} must be absolute"),
            Self::PathNotUtf8(label) => write!(formatter, "{label} is not valid UTF-8"),
            Self::EmptyGlobs => formatter.write_str("path_glob_match requires at least one glob"),
            Self::TooManyGlobs => formatter.write_str("path_glob_match has too many globs"),
            Self::GlobsTooLarge => {
                formatter.write_str("path_glob_match globs exceed their total size limit")
            }
            Self::InvalidGlob => formatter.write_str("path_glob_match contains an invalid glob"),
        }
    }
}

impl std::error::Error for InstructionsLoadedInputError {}

fn validate_exact_id(
    value: String,
    label: &'static str,
) -> Result<String, InstructionsLoadedInputError> {
    if value.is_empty() {
        return Err(InstructionsLoadedInputError::EmptyField(label));
    }
    if value.trim() != value {
        return Err(InstructionsLoadedInputError::SurroundingWhitespace(label));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(InstructionsLoadedInputError::FieldTooLong(label));
    }
    if value.chars().any(char::is_control) {
        return Err(InstructionsLoadedInputError::ControlCharacter(label));
    }
    Ok(value)
}

fn absolute_utf8_path(
    value: &Path,
    label: &'static str,
) -> Result<String, InstructionsLoadedInputError> {
    let value = value
        .to_str()
        .ok_or(InstructionsLoadedInputError::PathNotUtf8(label))?;
    if !Path::new(value).is_absolute() {
        return Err(InstructionsLoadedInputError::PathNotAbsolute(label));
    }
    if value.is_empty() {
        return Err(InstructionsLoadedInputError::EmptyField(label));
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(InstructionsLoadedInputError::FieldTooLong(label));
    }
    if value.chars().any(|character| character == '\0') {
        return Err(InstructionsLoadedInputError::ControlCharacter(label));
    }
    Ok(value.to_owned())
}

fn validate_globs(globs: &[String]) -> Result<(), InstructionsLoadedInputError> {
    if globs.is_empty() {
        return Err(InstructionsLoadedInputError::EmptyGlobs);
    }
    if globs.len() > MAX_GLOBS {
        return Err(InstructionsLoadedInputError::TooManyGlobs);
    }
    let total_bytes = globs
        .iter()
        .try_fold(0_usize, |total, glob| total.checked_add(glob.len()))
        .ok_or(InstructionsLoadedInputError::GlobsTooLarge)?;
    if total_bytes > MAX_TOTAL_GLOB_BYTES {
        return Err(InstructionsLoadedInputError::GlobsTooLarge);
    }
    if globs.iter().any(|glob| {
        glob.is_empty() || glob.len() > MAX_GLOB_BYTES || glob.chars().any(char::is_control)
    }) {
        return Err(InstructionsLoadedInputError::InvalidGlob);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Condvar, Mutex},
        time::{Duration, Instant},
    };

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("mework-instructions-loaded")
            .join(name)
    }

    fn host() -> InstructionsLoadedHostContext {
        InstructionsLoadedHostContext::new(
            "conversation-1",
            None,
            &absolute("workspace"),
            "Kimi-K3",
            InstructionsLoadedPermissionMode::Default,
            "turn-1",
        )
        .unwrap()
    }

    #[test]
    fn session_start_serializes_the_official_contract_without_provider_content() {
        let input = InstructionsLoadedInput::session_start(
            &host(),
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&input.to_local_hook_json().unwrap()).unwrap();

        assert_eq!(value["session_id"], "conversation-1");
        assert!(value.get("transcript_path").is_none());
        assert_eq!(
            value["cwd"],
            absolute("workspace").to_string_lossy().as_ref()
        );
        assert_eq!(value["hook_event_name"], "InstructionsLoaded");
        assert_eq!(value["model"], "Kimi-K3");
        assert_eq!(value["permission_mode"], "default");
        assert_eq!(value["turn_id"], "turn-1");
        assert_eq!(
            value["file_path"],
            absolute("workspace/MEWORK.md").to_string_lossy().as_ref()
        );
        assert_eq!(value["memory_type"], "Project");
        assert_eq!(value["load_reason"], "session_start");
        assert!(value.get("globs").is_none());
        assert!(value.get("trigger_file_path").is_none());
        assert!(value.get("parent_file_path").is_none());
        for forbidden in [
            "provider",
            "provider_id",
            "content",
            "instruction_content",
            "model_hash",
        ] {
            assert!(value.get(forbidden).is_none());
        }
    }

    #[test]
    fn reason_specific_factories_make_invalid_metadata_states_unrepresentable() {
        let nested = InstructionsLoadedInput::nested_traversal(
            &host(),
            &absolute("workspace/src/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
            &absolute("workspace/src/lib.rs"),
        )
        .unwrap();
        let nested = serde_json::to_value(nested).unwrap();
        assert_eq!(nested["load_reason"], "nested_traversal");
        assert!(nested.get("trigger_file_path").is_some());
        assert!(nested.get("globs").is_none());
        assert!(nested.get("parent_file_path").is_none());

        let path_match = InstructionsLoadedInput::path_glob_match(
            &host(),
            &absolute("workspace/.mework/rules/rust.md"),
            InstructionsLoadedMemoryType::Project,
            vec!["src/**/*.rs".into(), "tests/**/*.rs".into()],
            &absolute("workspace/src/lib.rs"),
        )
        .unwrap();
        let path_match = serde_json::to_value(path_match).unwrap();
        assert_eq!(path_match["load_reason"], "path_glob_match");
        assert_eq!(path_match["globs"], json!(["src/**/*.rs", "tests/**/*.rs"]));
        assert!(path_match.get("trigger_file_path").is_some());
        assert!(path_match.get("parent_file_path").is_none());

        let include = InstructionsLoadedInput::include(
            &host(),
            &absolute("shared/policy.md"),
            InstructionsLoadedMemoryType::Managed,
            &absolute("workspace/MEWORK.md"),
        )
        .unwrap();
        let include = serde_json::to_value(include).unwrap();
        assert_eq!(include["load_reason"], "include");
        assert!(include.get("parent_file_path").is_some());
        assert!(include.get("trigger_file_path").is_none());
        assert!(include.get("globs").is_none());
    }

    #[test]
    fn trusted_input_rejects_relative_paths_mutated_ids_and_empty_globs() {
        assert_eq!(
            InstructionsLoadedHostContext::new(
                "conversation-1",
                None,
                Path::new("relative"),
                "kimi-k3",
                InstructionsLoadedPermissionMode::Default,
                "turn-1",
            )
            .err(),
            Some(InstructionsLoadedInputError::PathNotAbsolute("cwd"))
        );
        assert_eq!(
            InstructionsLoadedHostContext::new(
                "conversation-1",
                None,
                &absolute("workspace"),
                " kimi-k3",
                InstructionsLoadedPermissionMode::Default,
                "turn-1",
            )
            .err(),
            Some(InstructionsLoadedInputError::SurroundingWhitespace(
                "owner_model_id"
            ))
        );
        assert_eq!(
            InstructionsLoadedInput::path_glob_match(
                &host(),
                &absolute("workspace/rule.md"),
                InstructionsLoadedMemoryType::Local,
                Vec::new(),
                &absolute("workspace/src/lib.rs"),
            )
            .err(),
            Some(InstructionsLoadedInputError::EmptyGlobs)
        );
    }

    #[test]
    fn matcher_is_precompiled_and_runs_only_against_load_reason() {
        let lazy = InstructionsLoadedMatcher::compile(Some("^(path_glob_match|nested_traversal)$"))
            .unwrap();
        assert!(lazy.matches(InstructionsLoadedReason::PathGlobMatch));
        assert!(lazy.matches(InstructionsLoadedReason::NestedTraversal));
        assert!(!lazy.matches(InstructionsLoadedReason::SessionStart));

        let all = InstructionsLoadedMatcher::compile(Some("*")).unwrap();
        assert!(all.matches(InstructionsLoadedReason::Include));
        assert_eq!(
            InstructionsLoadedMatcher::compile(Some("(")).err(),
            Some(InstructionsLoadedMatcherError::InvalidRegularExpression)
        );
    }

    #[test]
    fn dispatch_never_waits_for_hook_work_and_drops_only_at_the_bounded_edge() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let handler_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (completed_tx, completed_rx) = mpsc::sync_channel(2);
        let dispatcher = AsyncInstructionsLoadedDispatcher::new(1, move |invocation| {
            let (hook, _) = invocation.into_parts();
            if hook == "first" {
                entered_tx.send(()).unwrap();
                let (locked, changed) = &*handler_gate;
                let released = locked.lock().unwrap();
                drop(changed.wait_while(released, |released| !*released).unwrap());
            }
            completed_tx.send(hook).unwrap();
            Ok(())
        })
        .unwrap();
        let matcher = InstructionsLoadedMatcher::compile(None).unwrap();
        let event = InstructionsLoadedInput::session_start(
            &host(),
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )
        .unwrap();

        assert_eq!(
            dispatcher.try_dispatch(&matcher, "first", event.clone()),
            InstructionsLoadedDispatchOutcome::Enqueued
        );
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            dispatcher.try_dispatch(&matcher, "second", event.clone()),
            InstructionsLoadedDispatchOutcome::Enqueued
        );
        let started = Instant::now();
        assert_eq!(
            dispatcher.try_dispatch(&matcher, "third", event),
            InstructionsLoadedDispatchOutcome::QueueFull
        );
        assert!(started.elapsed() < Duration::from_millis(100));

        let (locked, changed) = &*gate;
        *locked.lock().unwrap() = true;
        changed.notify_all();
        assert_eq!(
            completed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "first"
        );
        assert_eq!(
            completed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "second"
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while dispatcher.metrics().completed < 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        let metrics = dispatcher.metrics();
        assert_eq!(metrics.enqueued, 2);
        assert_eq!(metrics.completed, 2);
        assert_eq!(metrics.dropped_queue_full, 1);
        assert_eq!(metrics.handler_panics, 0);
    }

    #[test]
    fn handler_panic_is_local_and_does_not_disable_later_events() {
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let dispatcher = AsyncInstructionsLoadedDispatcher::new(4, move |invocation| {
            let (hook, _) = invocation.into_parts();
            if hook == "panic" {
                panic!("test panic");
            }
            completed_tx.send(hook).unwrap();
            Ok(())
        })
        .unwrap();
        let matcher = InstructionsLoadedMatcher::compile(None).unwrap();
        let event = InstructionsLoadedInput::session_start(
            &host(),
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )
        .unwrap();

        assert_eq!(
            dispatcher.try_dispatch(&matcher, "panic", event.clone()),
            InstructionsLoadedDispatchOutcome::Enqueued
        );
        assert_eq!(
            dispatcher.try_dispatch(&matcher, "later", event),
            InstructionsLoadedDispatchOutcome::Enqueued
        );
        assert_eq!(
            completed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "later"
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while (dispatcher.metrics().handler_panics == 0 || dispatcher.metrics().completed == 0)
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        assert_eq!(dispatcher.metrics().handler_panics, 1);
        assert_eq!(dispatcher.metrics().completed, 1);
    }

    #[test]
    fn a_non_matching_event_is_not_enqueued() {
        let dispatcher = AsyncInstructionsLoadedDispatcher::new(
            1,
            |_invocation: InstructionsLoadedInvocation<()>| Ok(()),
        )
        .unwrap();
        let matcher = InstructionsLoadedMatcher::compile(Some("^include$")).unwrap();
        let event = InstructionsLoadedInput::session_start(
            &host(),
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )
        .unwrap();

        assert_eq!(
            dispatcher.try_dispatch(&matcher, (), event),
            InstructionsLoadedDispatchOutcome::NotMatched
        );
        assert_eq!(
            dispatcher.metrics(),
            InstructionsLoadedDispatchMetrics::default()
        );
    }

    fn definition(matcher: Option<&str>) -> HookDefinition {
        HookDefinition {
            id: "instructions-observer".into(),
            name: "Instructions observer".into(),
            event: HookEvent::InstructionsLoaded,
            matcher: matcher.map(str::to_owned),
            command: "observe".into(),
            command_windows: None,
            status_message: None,
            enabled: true,
            timeout_ms: 1_000,
        }
    }

    #[test]
    fn run_dispatcher_emits_exact_reason_fields_and_matches_only_load_reason() {
        let (sent, received) = mpsc::sync_channel(8);
        let runner: HookCommandRunner = Arc::new(move |_hook, input, _remaining| {
            sent.send(input.clone()).unwrap();
            Ok(crate::hooks::HookCommandOutput {
                code: 2,
                stdout: r#"{"continue":false,"decision":"block"}"#.into(),
                stderr: "ignored exit and decision".into(),
            })
        });
        let dispatcher = InstructionsLoadedRunDispatcher::new(
            &[definition(Some(
                "^(session_start|include|nested_traversal|path_glob_match)$",
            ))],
            host(),
            InstructionsLoadedPathMode::host(),
            runner,
        )
        .unwrap();
        let workspace = absolute("workspace");
        let loads = vec![
            ManifestedInstructionLoad::session_start(
                &workspace.join("MEWORK.md"),
                InstructionsLoadedMemoryType::Project,
            ),
            ManifestedInstructionLoad::nested_traversal(
                &workspace.join("src/MEWORK.md"),
                InstructionsLoadedMemoryType::Project,
                &workspace.join("src/lib.rs"),
            ),
            ManifestedInstructionLoad::path_glob_match(
                &workspace.join(".mework/rules/rust.md"),
                InstructionsLoadedMemoryType::Local,
                &["src/**/*.rs".into(), "tests/**/*.rs".into()],
                &workspace.join("src/lib.rs"),
            ),
            ManifestedInstructionLoad::include(
                &absolute("managed/imported.md"),
                InstructionsLoadedMemoryType::Managed,
                &absolute("managed/MEWORK.md"),
            ),
        ];

        dispatcher.dispatch_manifested(&loads);
        let values = (0..loads.len())
            .map(|_| received.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect::<Vec<_>>();
        let values = values
            .into_iter()
            .map(|value| (value["load_reason"].as_str().unwrap().to_owned(), value))
            .collect::<HashMap<_, _>>();

        assert!(values.contains_key("session_start"));
        let nested = &values["nested_traversal"];
        assert_eq!(
            nested["trigger_file_path"],
            workspace.join("src/lib.rs").to_string_lossy().as_ref()
        );
        let path_glob = &values["path_glob_match"];
        assert_eq!(path_glob["globs"], json!(["src/**/*.rs", "tests/**/*.rs"]));
        assert_eq!(
            path_glob["trigger_file_path"],
            workspace.join("src/lib.rs").to_string_lossy().as_ref()
        );
        let include = &values["include"];
        assert_eq!(
            include["parent_file_path"],
            absolute("managed/MEWORK.md").to_string_lossy().as_ref()
        );
        assert!(values.values().all(|value| value["model"] == "Kimi-K3"));
    }

    #[test]
    fn run_dispatcher_matcher_cannot_match_file_path_or_memory_type() {
        let (sent, received) = mpsc::sync_channel(1);
        let runner: HookCommandRunner = Arc::new(move |_hook, input, _remaining| {
            sent.send(input.clone()).unwrap();
            Ok(crate::hooks::HookCommandOutput {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let dispatcher = InstructionsLoadedRunDispatcher::new(
            &[definition(Some("MEWORK\\.md|Project"))],
            host(),
            InstructionsLoadedPathMode::host(),
            runner,
        )
        .unwrap();
        dispatcher.dispatch_manifested(&[ManifestedInstructionLoad::session_start(
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )]);

        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(
            dispatcher.dispatcher.metrics(),
            InstructionsLoadedDispatchMetrics::default()
        );
    }

    #[test]
    fn runner_failure_is_local_metrics_only_and_never_a_run_decision() {
        let runner: HookCommandRunner =
            Arc::new(move |_hook, _input, _remaining| Err("local test runner failure".into()));
        let dispatcher = InstructionsLoadedRunDispatcher::new(
            &[definition(None)],
            host(),
            InstructionsLoadedPathMode::host(),
            runner,
        )
        .unwrap();
        dispatcher.dispatch_manifested(&[ManifestedInstructionLoad::session_start(
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )]);

        let deadline = Instant::now() + Duration::from_secs(1);
        while dispatcher.dispatcher.metrics().handler_failures == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        let metrics = dispatcher.dispatcher.metrics();
        assert_eq!(metrics.enqueued, 1);
        assert_eq!(metrics.handler_failures, 1);
        assert_eq!(metrics.completed, 0);
    }

    #[test]
    fn matching_hooks_run_in_parallel_on_a_fixed_worker_pool() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let runner_gate = Arc::clone(&gate);
        let (slow_entered_tx, slow_entered_rx) = mpsc::sync_channel(1);
        let (fast_completed_tx, fast_completed_rx) = mpsc::sync_channel(1);
        let runner: HookCommandRunner = Arc::new(move |hook, _input, _remaining| {
            if hook.command == "slow" {
                slow_entered_tx.send(()).unwrap();
                let (released, changed) = &*runner_gate;
                let released = released.lock().unwrap();
                drop(changed.wait_while(released, |released| !*released).unwrap());
            } else if hook.command == "fast" {
                fast_completed_tx.send(()).unwrap();
            }
            Ok(crate::hooks::HookCommandOutput {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let mut slow = definition(None);
        slow.id = "slow".into();
        slow.command = "slow".into();
        let mut fast = definition(None);
        fast.id = "fast".into();
        fast.command = "fast".into();
        let dispatcher = InstructionsLoadedRunDispatcher::new(
            &[slow, fast],
            host(),
            InstructionsLoadedPathMode::host(),
            runner,
        )
        .unwrap();

        dispatcher.dispatch_manifested(&[ManifestedInstructionLoad::session_start(
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )]);
        slow_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let fast_result = fast_completed_rx.recv_timeout(Duration::from_secs(1));

        let (released, changed) = &*gate;
        *released.lock().unwrap() = true;
        changed.notify_all();
        fast_result.expect("the fast matching hook must not wait behind the slow hook");
    }

    #[test]
    fn duplicate_platform_command_keeps_first_definition_even_when_timeouts_differ() {
        let (sent, received) = mpsc::sync_channel(3);
        let runner: HookCommandRunner = Arc::new(move |hook, _input, _remaining| {
            sent.send((
                instructions_loaded_execution_identity(hook),
                hook.timeout_ms,
            ))
            .unwrap();
            Ok(crate::hooks::HookCommandOutput {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let mut first = definition(None);
        #[cfg(windows)]
        {
            first.command = "fallback-first".into();
            first.command_windows = Some("observe-platform".into());
        }
        let mut duplicate = first.clone();
        duplicate.id = "duplicate-id".into();
        duplicate.name = "Duplicate display name".into();
        duplicate.timeout_ms = 2_000;
        #[cfg(windows)]
        {
            duplicate.command = "different-unused-fallback".into();
        }
        let mut different_command = first.clone();
        different_command.id = "different-command".into();
        #[cfg(not(windows))]
        {
            different_command.command = "observe-other".into();
        }
        #[cfg(windows)]
        {
            different_command.command_windows = Some("observe-other".into());
        }
        let dispatcher = InstructionsLoadedRunDispatcher::new(
            &[first, duplicate, different_command],
            host(),
            InstructionsLoadedPathMode::host(),
            runner,
        )
        .unwrap();

        dispatcher.dispatch_manifested(&[ManifestedInstructionLoad::session_start(
            &absolute("workspace/MEWORK.md"),
            InstructionsLoadedMemoryType::Project,
        )]);
        let mut executions = vec![
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
        ];
        executions.sort_unstable();
        let mut expected = vec![
            (
                if cfg!(windows) {
                    "observe-platform".to_owned()
                } else {
                    "observe".to_owned()
                },
                1_000,
            ),
            ("observe-other".to_owned(), 1_000),
        ];
        expected.sort_unstable();
        assert_eq!(executions, expected);
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
