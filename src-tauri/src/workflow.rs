//! The `workflow` tool runs a declarative plan as a pooled task with concurrent subagents.
//!
//! # Architecture
//!
//! The driver occupies one outer [`AgentPool`] slot and returns a derived acknowledgement
//! immediately. `complete_turn` envelopes plan results for delivery through `task_wait` or a
//! round-boundary fold. The driver owns a nested private pool with
//! `with_live_limit(min(16, max(2, parallelism - 2)))`; step subagents use that pool and do
//! not consume the session's `agent_spawn` budget, mailbox, or namespace. Step templates derive
//! from the original depth-0 request, so every step has depth 1. Driver templates never run a
//! model, and non-General tasks reject `send_message` and `followup_task`.
//!
//! Step names are allocated only from the private live pool. Stable slot names make ordinary-step
//! execution-mode payloads byte-stable across runs. Role steps use a separate `ws<N>-as-<role>`
//! namespace so role and ordinary steps do not share a reservation key. A step-level `model` is
//! rejected: roles are the complete model-facing input and user configuration selects their model.
//!
//! # Isolated worktrees
//!
//! A step with `isolation: "worktree"` receives its own worktree at
//! `<workspace>/.mework/worktrees/<runId>/ws<N>` on `mework/wf/<runId>/ws<N>`. Its
//! `workspace_path` is the isolation boundary for file-tool scope, shell CWD, and the safety
//! classifier. The worktree checks out `HEAD`, excluding uncommitted parent changes. Cleanup runs
//! after `await_quiescence`: only unchanged worktrees with no additional commits are removed.
//! Isolation is excluded from execution-mode payloads, so it does not require a separate namespace.
//!
//! # Deadline and cancellation
//!
//! Each run has one absolute [`WORKFLOW_RUN_DEADLINE`] computed before the driver loop. Progress
//! updates may wake the wait but never extend it. Round settlement cancels the outer pool; the
//! shared cancellation flag makes heartbeats and event forwarding fail immediately, ending the
//! driver as Interrupted within one `WAIT_POLL_INTERVAL`. Interrupted results retain step logs for
//! resume and are delivered like any other terminal result; when the user closed the run, the body
//! says so.
//!
//! # Records
//!
//! Worker completion writes synthesized `workflow_step` contexts and plan results through
//! `complete_turn`; `finalize_agent_pool` copies accumulated records into the `workflow` context.
//! Records use `WorkflowStep` and the pool name, but kind-based rejection prevents finished
//! workflows from being continued. `resume_run_id` is the only recovery path.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};

use workflow_core::{StepOutcome, StepProgress, StepSource, WorkflowStepRequest};
use workflow_script::ScriptSpec;

use crate::agents::{AgentLiveStatus, AgentPool, AgentShared};
use crate::api::{
    agent_child_template, failed_tool_execution, install_structured_output, merge_usage,
    new_context_id, reserve_subagent_execution_mode, start_registered_step_worker,
    DangerousToolApproval, ModelEventSink, TaskApproval, ToolCall, ToolExecution,
};
use crate::model::{
    AgentDefinitionBinding, ContextItem, JsonObject, ModelStreamEvent, ModelUsage, ReasoningEffort,
    RunModelRequest, SubagentChannel, SubagentRunKind, SubagentRunRecord, SubagentRunStatus,
    ToolExecutionRequest, ToolResult,
};
use crate::orchestration;
use crate::prompt_profile::{PromptKey, PromptProfile};
use crate::state::AppState;

pub(crate) const WORKFLOW_TOOL: &str = "workflow";
/// Tool name for synthesized step contexts. It has no catalog descriptor or schema and cannot
/// be called by the model; it exists only as a workflow-record transcript anchor.
pub(crate) const WORKFLOW_STEP_TOOL: &str = "workflow_step";

/// The single absolute deadline for an entire run.
///
/// This is not the workflow-core step stall window. It bounds the complete tool call because a
/// sequential tool loop cannot advance before `workflow` returns.
pub(crate) const WORKFLOW_RUN_DEADLINE: Duration = Duration::from_secs(1_800);

/// Private-pool concurrency limit: `min(16, max(2, available_parallelism - 2))`.
fn workflow_live_limit() -> usize {
    let parallelism = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    parallelism.saturating_sub(2).clamp(2, 16)
}

/// Returns the next unused step name from the private live pool (`ws1`, `ws2`, ...).
///
/// Terminal agents remain in the pool, so names increment by step ordinal within a run and are
/// never reused. Reuse would make `pool.find` and in-flight mappings ambiguous. Across runs, an
/// ordinary step's canonical payload is byte-stable, so re-signing is idempotent and reservation
/// rows remain bounded by `MAX_LIFETIME_STEPS` rather than the number of runs. Naming must inspect
/// the live pool, not the reservation table.
///
/// Role steps use a separate `ws<N>-as-<role>` namespace. Execution-mode receipts are retained by
/// `(conversation_id, name)` for the process lifetime, so the role suffix keeps ordinary and role
/// payloads on independent reservation keys.
///
/// Use `-as-`, not `@`: `validate_agent_name` permits lowercase letters, digits, `_`, and `-`.
/// `validate_agent_type_slug` restricts role names to the same character set.
fn workflow_step_name(pool: &AgentPool, agent_type: Option<&str>) -> String {
    let taken = pool
        .all()
        .into_iter()
        .map(|agent| agent.name.clone())
        .collect::<std::collections::HashSet<_>>();
    let suffix = agent_type
        .map(|role| format!("-as-{role}"))
        .unwrap_or_default();
    for index in 1usize.. {
        let candidate = format!("ws{index}{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unused step name always exists")
}

/// Driver-side record for a dispatched step.
struct StepSlot {
    request: WorkflowStepRequest,
    /// Call ID used by the synthesized `workflow_step` context (`<parent-call>-ws<N>`).
    call_id: String,
    /// Cache key advanced in dispatch order. Started and result log entries use this key, and
    /// replay recomputes it from the same plan position.
    cache_key: String,
    /// Private-pool worker. Spawn failures and replayed steps have none.
    shared: Option<Arc<AgentShared>>,
    /// Presentation-only host binding already published with arguments. A
    /// registration failure has no worker record but must retain this identity.
    published_role: Option<AgentDefinitionBinding>,
    /// Isolated worktree created for a step with `isolation: "worktree"`.
    worktree: Option<crate::git::IsolatedWorktree>,
    /// Worktree release result: `Some(true)` is retained changes, `Some(false)` is removed, and
    /// `None` is unreleased or has no worktree.
    worktree_kept: Option<bool>,
    /// Final result (`None` while in flight or after spawn failure).
    outcome: Option<StepOutcome>,
    /// Whether the complete record was written at the harvest checkpoint. Terminal records are
    /// immutable, so the first persisted copy is authoritative.
    record_persisted: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_workflow_tool(
    outer_pool: &AgentPool,
    shadow: &crate::kernel_shadow::KernelShadow,
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
    event_sink: &ModelEventSink<'_>,
    shadowed_approval: &DangerousToolApproval<'_>,
    hook_allows_permission: bool,
    round: usize,
) -> Result<ToolExecution, String> {
    run_workflow_tool_with_deadline(
        outer_pool,
        shadow,
        request,
        call,
        state,
        event_sink,
        shadowed_approval,
        hook_allows_permission,
        round,
        workflow_live_limit(),
        Instant::now() + WORKFLOW_RUN_DEADLINE,
    )
}

/// Internal entry point with an injectable deadline and live limit for testing deadline and
/// backpressure semantics.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_workflow_tool_with_deadline(
    outer_pool: &AgentPool,
    shadow: &crate::kernel_shadow::KernelShadow,
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
    event_sink: &ModelEventSink<'_>,
    shadowed_approval: &DangerousToolApproval<'_>,
    hook_allows_permission: bool,
    round: usize,
    live_limit: usize,
    deadline: Instant,
) -> Result<ToolExecution, String> {
    let started = Instant::now();
    let mut resume_run_id: Option<String> = None;
    if request.subagent_depth >= 1 {
        // Nested calls are rejected by `enabled_tools` before execution. Keep this defense in
        // depth so subagents cannot create private pools.
        return Ok(failed_tool_execution(
            call,
            "Subagents cannot start workflow; return orchestration to the main agent".into(),
        ));
    }
    if let Some(resume) = call.input.get("resume_run_id") {
        if !resume.is_null() {
            let Some(text) = resume.as_str() else {
                return Ok(failed_tool_execution(
                    call,
                    "resume_run_id must be a string or null".into(),
                ));
            };
            resume_run_id = Some(text.to_owned());
        }
    }
    if let Some(named) = &resume_run_id {
        // Concurrent resume attempts would let two drivers write the same log. Reject a running
        // outer-pool entry with the `workflow:<run_id>` label.
        let label = format!("workflow:{named}");
        let still_running = outer_pool.all().into_iter().any(|agent| {
            agent.kind == SubagentRunKind::WorkflowStep
                && agent.label == label
                && agent.status() == AgentLiveStatus::Running
        });
        if still_running {
            return Ok(failed_tool_execution(
                call,
                format!("Workflow run {named} is still running; use task_wait for it to finish before deciding whether to resume"),
            ));
        }
    }
    let provided_script = match call.input.get("script") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.to_owned()),
        Some(_) => {
            return Ok(failed_tool_execution(
                call,
                "workflow.script must be a string".into(),
            ))
        }
    };
    let script_text = match (provided_script, &resume_run_id) {
        (Some(text), _) => text,
        // Scriptless resume uses the approved bytes pinned in `script.js`. When a script is
        // supplied, its SHA is still checked. The timeline's host-signed `scriptSha256` is an
        // external integrity anchor; comparing disk contents with their own digest is vacuous.
        (None, Some(named)) => {
            let bytes = match crate::workflow_store::read_run_script(
                std::path::Path::new(&request.app_data_path),
                &request.conversation_id,
                named,
            ) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    return Ok(failed_tool_execution(
                        call,
                        format!(
                            "The saved script for workflow run {named} was not found. It may never have existed or may have been removed with the conversation. Provide script to run from the beginning."
                        ),
                    ))
                }
                Err(error) => return Ok(failed_tool_execution(call, error)),
            };
            let receipt = orchestration::workflow_spawn_output(named);
            let approved_digest = request.contexts.iter().find_map(|context| {
                let crate::model::ContextItem::Tool {
                    tool_name,
                    input,
                    result,
                    ..
                } = context
                else {
                    return None;
                };
                if tool_name != WORKFLOW_TOOL {
                    return None;
                }
                if result.output.lines().next() != Some(receipt.as_str()) {
                    return None;
                }
                input
                    .get("scriptSha256")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let disk_digest = crate::workflow_store::hex_digest(&bytes);
            match approved_digest {
                Some(approved) if approved == disk_digest => {}
                Some(_) => {
                    return Ok(failed_tool_execution(
                        call,
                        format!(
                            "The on-disk script for workflow run {named} does not match the fingerprint approved in the timeline. Scriptless resume is rejected; resubmit it with script for fresh approval."
                        ),
                    ))
                }
                None => {
                    return Ok(failed_tool_execution(
                        call,
                        format!(
                            "The approved script fingerprint for workflow run {named} is missing from the timeline (the original dispatch card may have been removed). Resubmit it together with script."
                        ),
                    ))
                }
            }
            match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    return Ok(failed_tool_execution(
                        call,
                        format!("The saved script for workflow run {named} is corrupted (not valid UTF-8), so it cannot be resumed without script"),
                    ))
                }
            }
        }
        (None, None) => {
            return Ok(failed_tool_execution(
                call,
                "workflow.script is required: provide an orchestration script beginning with `export const meta = {…}`".into(),
            ))
        }
    };
    // The model supplies the run name. It is both the session agent-namespace address and the
    // task-surface label.
    let run_name = match call.input.get("name") {
        Some(Value::String(text)) => text.trim().to_owned(),
        Some(Value::Null) | None => {
            return Ok(failed_tool_execution(
                call,
                "workflow.name is required: name this run (begin with a lowercase letter; digits, _ and - are allowed)".into(),
            ))
        }
        Some(_) => return Ok(failed_tool_execution(call, "workflow.name must be a string".into())),
    };
    if let Err(error) = crate::agents::validate_agent_name(&run_name) {
        return Ok(failed_tool_execution(call, error));
    }
    let args = call
        .input
        .get("args")
        .filter(|value| !value.is_null())
        .cloned();
    let token_budget = match call.input.get("token_budget") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(total) if total > 0 => Some(total),
            _ => {
                return Ok(failed_tool_execution(
                    call,
                    "token_budget must be a positive integer (the total tokens permitted for this run)".into(),
                ))
            }
        },
    };
    // Validate byte limits, meta shape, script syntax, argument bounds, and role policy before
    // asking for approval. Step-level `model` is rejected because user configuration maps roles
    // to models.
    let policy = crate::api::role_policy(request, state);
    let role_policy = workflow_core::StepRolePolicy {
        required: policy.required(),
        known_names: policy.known_names(),
    };
    let (meta, spec) = match ScriptSpec::parse(&script_text, args, token_budget, role_policy) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(failed_tool_execution(call, error.to_string())),
    };
    let task = meta.description.clone();
    let plan_name = meta.name.clone();

    // Consume mandatory approval here because the generic pre-dispatch block excludes
    // `intrinsic_mandatory_prompt` tools. One approval covers the entire plan.
    let execution_request = ToolExecutionRequest {
        conversation_id: request.conversation_id.clone(),
        workspace_path: request.workspace_path.clone(),
        tool_name: call.name.clone(),
        input: call.input.clone(),
    };
    let descriptor = request
        .tools
        .iter()
        .find(|tool| tool.name == call.name)
        .ok_or_else(|| format!("Workflow tool {} has no trusted descriptor", call.name))?;
    let decision = crate::security::classify_model_call(
        request.security_level,
        std::path::Path::new(&request.workspace_path),
        std::path::Path::new(&request.app_data_path),
        &execution_request,
    )?;
    if decision.requires_approval && (!hook_allows_permission || decision.mandatory_prompt) {
        match shadowed_approval(&execution_request, descriptor, crate::api::ApprovalRequester::default()) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(failed_tool_execution(
                    call,
                    "The user declined this workflow run; no steps were created".into(),
                ))
            }
            Err(error) => {
                return Ok(failed_tool_execution(
                    call,
                    format!("Could not confirm workflow authorization: {error}"),
                ))
            }
        }
    }

    // Persist before dispatching the first step so recovery covers a crash immediately after
    // startup. The source digest binds resume to the submitted plan bytes.
    let source_body = script_text.into_bytes();
    let run_id = match resume_run_id.clone() {
        Some(run_id) => run_id,
        None => format!("run{}", uuid::Uuid::new_v4().simple()),
    };
    // Run records are conversation-owned data. They remain valid for the conversation lifetime;
    // deleting a conversation removes its run directories, and startup cleanup removes orphans.
    let store = match crate::workflow_store::RunStore::open(
        std::path::Path::new(&request.app_data_path),
        &request.conversation_id,
        &run_id,
        &source_body,
    ) {
        Ok(store) => Some(store),
        Err(error) if resume_run_id.is_some() => {
            // Explicit resume failure must not silently start a new run.
            return Ok(failed_tool_execution(call, error));
        }
        Err(error) => {
            // A fresh run loses recoverability but may still execute.
            eprintln!("工作流运行目录不可用（本次运行将不可恢复）：{error}");
            None
        }
    };
    // `open` can create a missing directory, so an explicit resume must reject a newly created
    // store rather than silently rerunning. Remove that empty store so a retry receives the same
    // result.
    if let (Some(existing), Some(named)) = (&store, &resume_run_id) {
        if existing.is_fresh() {
            let message = format!("Workflow run {named} was not found. It may never have existed or was removed after its retention period. Omit resume_run_id to run from the beginning.");
            if let Some(existing) = store {
                existing.discard_if_fresh();
            }
            return Ok(failed_tool_execution(call, message));
        }
    }
    let journal = store
        .as_ref()
        .map(crate::workflow_store::RunStore::load_journal)
        .unwrap_or_default();
    // The pool check covers only this process. Hold a cross-process driver lock for the worker
    // lifetime; the OS releases it after a process crash, and recovery scanning uses the same lock
    // as its liveness signal.
    let driver_lock = match &store {
        Some(_) => match crate::workflow_store::acquire_driver_lock(
            std::path::Path::new(&request.app_data_path),
            &request.conversation_id,
            &run_id,
        ) {
            Ok(lock) => Some(lock),
            Err(error) => return Ok(failed_tool_execution(call, error)),
        },
        None => None,
    };

    // The driver is a first-class outer-pool task and occupies one slot. Its template never runs
    // a model; it supplies execution-mode and pool-accounting fields. Step templates derive from
    // the original depth-0 request, preserving step depth 1. Workflow names share the permanently
    // reserved conversation-tree namespace with `agent_spawn` and `web_search`.
    let driver_template = agent_child_template(request);
    let history = crate::api::historical_agent_records(&request.contexts);
    let orphan_reserved = state.reserved_subagent_names(&request.conversation_id);
    let mut reserved_names = request
        .subagent_reserved_names
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    reserved_names.extend(orphan_reserved.iter().map(String::as_str));
    let name = run_name;
    if history.contains_key(&name)
        || reserved_names.contains(name.as_str())
        || outer_pool.find(&name).is_some()
    {
        return Ok(failed_tool_execution(
            call,
            format!("Name {name} is permanently reserved in this conversation branch tree; choose another name for this run"),
        ));
    }
    if let Err(error) =
        outer_pool.preflight_registration(&name, &driver_template, AgentLiveStatus::Running)
    {
        return Ok(failed_tool_execution(call, error));
    }
    if let Err(error) =
        reserve_subagent_execution_mode(&driver_template, state, &name, SubagentRunKind::WorkflowStep)
    {
        return Ok(failed_tool_execution(call, error));
    }
    let shared = match outer_pool.register(
        name.clone(),
        format!("workflow:{run_id}"),
        task,
        driver_template,
        SubagentRunKind::WorkflowStep,
        state,
        Vec::new(),
        Vec::new(),
        AgentLiveStatus::Running,
        call.id.clone(),
        None,
    ) {
        Ok(shared) => shared,
        Err(error) => return Ok(failed_tool_execution(call, error)),
    };
    // Verify the driver's identity-to-request binding at the derivation observation point.
    shadow.agent_spawned(&name, shared.identity());
    let worker_shared = Arc::clone(&shared);
    let parent_snapshot = request.clone();
    let worker_call = ToolCall {
        id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    };
    let request_id = request.request_id.clone();
    let resume_named = resume_run_id.is_some();
    // Spawn receipts contain only host-issued addresses. The host alone knows whether this run is
    // recoverable, which determines whether the model should attempt resume after failure.
    let spawn_output = if store.is_some() {
        orchestration::workflow_spawn_output(&run_id)
    } else {
        format!(
            "{}\n{}",
            orchestration::workflow_spawn_output(&run_id),
            request.prompt_profile.text(PromptKey::WorkflowNotRecoverable)
        )
    };
    // The detached worker owns all required data. It resolves the task surface once at startup;
    // idle sinks discard events and idle approvals reject requests.
    let worker_state = state.clone();
    let worker_conversation = request.conversation_id.clone();
    crate::api::start_registered_worker_with(
        Arc::clone(&shared),
        event_sink,
        round,
        &call.id,
        move || {
            // Retain the driver lock until `drive_workflow_task` has persisted the result.
            let _driver_lock = driver_lock;
            let surface = worker_state.task_surface(&worker_conversation);
            let idle_sink = |_: ModelStreamEvent| -> Result<(), String> { Ok(()) };
            // A missing surface occurs only in tests that directly invoke a conversation without
            // a top-level run. It means no approval channel exists and must be reported.
            let no_channel = |_: &ToolExecutionRequest,
                              _: &crate::model::ToolDescriptor,
                              _: crate::api::ApprovalRequester<'_>,
                              _: Option<&std::sync::atomic::AtomicBool>|
             -> Result<bool, String> {
                Err("This conversation has no available approval channel, so this tool call cannot be authorized".into())
            };
            let sink: &ModelEventSink<'_> = match surface.as_ref() {
                Some(surface) => &*surface.sink,
                None => &idle_sink,
            };
            let approve: &TaskApproval<'_> = match surface.as_ref() {
                Some(surface) => &*surface.approve,
                None => &no_channel,
            };
            drive_workflow_task(
                worker_shared,
                parent_snapshot,
                worker_call,
                &worker_state,
                sink,
                approve,
                round,
                spec,
                journal,
                store,
                run_id,
                plan_name,
                resume_named,
                request_id,
                live_limit,
                deadline,
            );
        },
    )?;
    Ok(ToolExecution {
        call,
        result: ToolResult {
            success: true,
            output: spawn_output,
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        },
        subagent: None,
    })
}

/// Driver worker: runs the full plan on an outer-pool worker and envelopes its result or failure
/// through `complete_turn`. `task_wait` or the pool pipeline delivers it. Cancellation is checked
/// before every forwarded event, including heartbeats.
#[allow(clippy::too_many_arguments)]
fn drive_workflow_task(
    shared: Arc<AgentShared>,
    parent: RunModelRequest,
    parent_call: ToolCall,
    state: &AppState,
    event_sink: &ModelEventSink<'_>,
    approve_dangerous_tool: &TaskApproval<'_>,
    round: usize,
    spec: ScriptSpec,
    journal: crate::workflow_store::Journal,
    store: Option<crate::workflow_store::RunStore>,
    run_id: String,
    plan_name: String,
    resume_named: bool,
    request_id: String,
    live_limit: usize,
    deadline: Instant,
) {
    let run_started = Instant::now();
    // Bind output accounting to this worker incarnation so the current-incarnation guard rejects
    // stale writes.
    let incarnation = shared.identity();
    // Once outer settlement sets cancellation, forwarding any event, including a heartbeat,
    // fails and aborts the private pool.
    let cancel_shared = Arc::clone(&shared);
    let guarded_sink = move |event: ModelStreamEvent| -> Result<(), String> {
        if cancel_shared.cancel.load(Ordering::Acquire) {
            return Err("Workflow stopped".into());
        }
        event_sink(event)
    };
    let guarded_sink: &ModelEventSink<'_> = &guarded_sink;
    let workflow_call_id = parent_call.id.clone();
    // Wrap step events in the workflow call ID so the renderer sees nested read-only transcripts
    // beneath the workflow card.
    let child_sink = |event: ModelStreamEvent| -> Result<(), String> {
        if matches!(&event, ModelStreamEvent::Ping) {
            return guarded_sink(ModelStreamEvent::Ping);
        }
        #[cfg(debug_assertions)]
        if matches!(&event, ModelStreamEvent::DebugRequestBody { .. }) {
            return Ok(());
        }
        guarded_sink(ModelStreamEvent::SubagentEvent {
            round,
            call_id: workflow_call_id.clone(),
            event: Box::new(event),
        })
    };
    let child_sink: &ModelEventSink<'_> = &child_sink;

    let pool = AgentPool::with_live_limit(live_limit);
    let mut slots: Vec<StepSlot> = Vec::new();
    let mut chain = workflow_core::chain::CacheKeyChain::new();
    // Only top-level rounds support per-step control. Subagents have no registered request ID,
    // and an empty ID would let unrelated runs share the same control entry.
    let step_control =
        (!request_id.is_empty()).then(|| state.register_workflow_run(&request_id, &run_id));
    // Construct one progress ledger outside `drive`. Worktree-release logs occur after `drive`
    // returns, and a shared ledger keeps log indices monotonic.
    let mut progress_card = ProgressEmitter::new(
        guarded_sink,
        round,
        parent_call.id.clone(),
        run_id.clone(),
    );
    // Create the rquickjs engine on this worker thread because it is not `Send`; failure occurs
    // before dispatching any step.
    let manifest = store.as_ref().map(|store| RunManifest {
        store,
        run_id: &run_id,
        plan_name: &plan_name,
        started_at: Utc::now().to_rfc3339(),
        resumed: resume_named,
    });
    // Write `status:"running"` at startup. After a crash, recovery scanning claims a manifest
    // left in that state.
    if let Some(manifest) = &manifest {
        manifest.write("running", None, &slots);
    }
    let drive_result = match spec.start() {
        Err(error) => Err(DriveAbort::Run(error.to_string())),
        Ok(mut source) => std::thread::scope(|scope| {
            drive(
                scope,
                &pool,
                &parent,
                &parent_call,
                state,
                guarded_sink,
                child_sink,
                approve_dangerous_tool,
                round,
                &mut source,
                deadline,
                live_limit,
                &mut slots,
                &mut chain,
                &journal,
                store.as_ref(),
                manifest.as_ref(),
                &run_id,
                step_control.as_deref(),
                &mut progress_card,
            )
        }),
    };
    // Remove the control registration once the driver stops consuming skip requests.
    if let Some(control) = &step_control {
        state.unregister_workflow_run(&request_id, &run_id, control);
    }
    // Always wait for quiescence. Abort paths already cancel the pool; scope joins workers and
    // this settles pool state for accounting.
    pool.await_quiescence();
    // Aggregate each step's usage into both driver core usage, consumed by outer finalization, and
    // lifetime usage for record display.
    let mut run_usage = ModelUsage::default();
    for slot in slots.iter() {
        if let Some(step_shared) = &slot.shared {
            merge_usage(&mut run_usage, &step_shared.take_usage());
        }
    }
    // Harvest any settled-but-unconsumed step with its real terminal state so synthesized audit
    // results match nested records.
    for index in 0..slots.len() {
        if slots[index].outcome.is_none() {
            if let Some(step_shared) = slots[index].shared.clone() {
                slots[index].outcome =
                    Some(harvest_outcome(
                        index,
                        &slots[index].request,
                        &step_shared,
                        &parent.prompt_profile,
                    ));
            }
        }
    }
    release_worktrees(&mut slots, &parent.workspace_path, &mut progress_card);

    let (status, live_status, output) = match drive_result {
        Ok(done) => (SubagentRunStatus::Completed, AgentLiveStatus::Idle, done),
        Err(DriveAbort::Run(reason)) => (SubagentRunStatus::Failed, AgentLiveStatus::Failed, reason),
        // Distinguish task cancellation from host-channel failure. Both are delivered; the
        // cancellation body says the user closed the run when that is what happened, while a
        // channel failure is Failed so its recovery hint reads as a fault rather than a choice.
        Err(DriveAbort::SinkDead(detail)) => {
            if shared.cancel.load(Ordering::Acquire) {
                let aborted = parent
                    .prompt_profile
                    .text(PromptKey::WorkflowAbortedCancelled)
                    .to_owned();
                (
                    SubagentRunStatus::Interrupted,
                    AgentLiveStatus::Interrupted,
                    if shared.stopped_by_user() {
                        crate::orchestration::append_user_close_note(
                            &aborted,
                            &parent.prompt_profile,
                        )
                    } else {
                        aborted
                    },
                )
            } else {
                (
                    SubagentRunStatus::Failed,
                    AgentLiveStatus::Failed,
                    parent.prompt_profile.render(
                        PromptKey::WorkflowAbortedChannel,
                        &[("detail", detail.as_str())],
                    ),
                )
            }
        }
    };
    let external_bodies = match &manifest {
        Some(manifest) => persist_run(manifest, status, &slots),
        None => std::collections::HashSet::new(),
    };
    // Successful output is the plan return value and must remain structurally consumable. Only
    // failed runs include recovery guidance, and only a healthy journal can promise reliable
    // resume.
    let output = if status == SubagentRunStatus::Completed {
        output
    } else {
        match &store {
            Some(store) if !store.journal_degraded() => {
                format!(
                    "{output}\n\n{}",
                    resume_hint(&parent.prompt_profile, &run_id, &journal, resume_named)
                )
            }
            Some(_) => format!(
                "{output}\n\n{}",
                parent
                    .prompt_profile
                    .render(PromptKey::WorkflowResumeDegraded, &[("run_id", run_id.as_str())])
            ),
            None => format!(
                "{output}\n\n{}",
                parent.prompt_profile.text(PromptKey::WorkflowNotRecoverable)
            ),
        }
    };
    let contexts = slots
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            synthesize_step_context(
                slot,
                index,
                round,
                external_bodies.contains(&index).then_some(run_id.as_str()),
                &parent.conversation_id,
                state,
                &parent.prompt_profile,
            )
        })
        .collect::<Vec<_>>();
    let duration_ms = run_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    shared.complete_turn(
        incarnation,
        contexts,
        output,
        live_status,
        &run_usage,
        duration_ms,
        None,
    );
    // The current-incarnation guard may reject this write. Report the status that was actually
    // recorded; terminal delta delivery is best-effort.
    let applied = shared.status();
    let _ = event_sink(ModelStreamEvent::SubagentDelta {
        round,
        call_id: parent_call.id.clone(),
        channel: SubagentChannel::Status,
        delta: applied.wire().into(),
    });
}

/// Driver-loop aborts: a run failure becomes a failed tool result, while a dead sink must
/// propagate as `Err` to terminate the model run.
enum DriveAbort {
    Run(String),
    SinkDead(String),
}

/// Releases every isolated worktree created by this run.
///
/// This runs outside `drive`, after `await_quiescence`, so every completion, failure, timeout, or
/// cancellation follows the same cleanup path. [`crate::git::release_isolated_worktree`] removes
/// a worktree only when it has no changes or additional commits. A release failure is recorded as
/// retained rather than turning an otherwise successful plan into a failure. Emit a progress log
/// only for retained worktrees because it provides the coordinates needed to locate that work.
fn release_worktrees(
    slots: &mut [StepSlot],
    workspace_path: &str,
    progress_card: &mut ProgressEmitter<'_>,
) {
    for index in 0..slots.len() {
        let Some(worktree) = slots[index].worktree.clone() else {
            continue;
        };
        let removed =
            crate::git::release_isolated_worktree(std::path::Path::new(workspace_path), &worktree)
                .unwrap_or(false);
        slots[index].worktree_kept = Some(!removed);
        if !removed {
            let label = step_display_label(&slots[index].request, index);
            progress_card.log(&format!(
                "The isolated worktree for {label} has changes and was kept: {} (branch {})",
                worktree.path.display(),
                worktree.branch
            ));
        }
    }
}

/// Writes the run summary (`manifest.json`) at startup, after each harvest batch, and at
/// termination. The initial timestamp remains unchanged across all writes.
struct RunManifest<'a> {
    store: &'a crate::workflow_store::RunStore,
    run_id: &'a str,
    plan_name: &'a str,
    /// RFC3339 startup time, unchanged across all writes.
    started_at: String,
    resumed: bool,
}

impl RunManifest<'_> {
    fn write(&self, status: &str, finished_at: Option<String>, slots: &[StepSlot]) {
        let entries = slots.iter().enumerate().map(|(index, slot)| {
            let cached = slot.outcome.as_ref().is_some_and(|outcome| outcome.cached);
            (
                index,
                json!({
                    "index": index,
                    "label": step_display_label(&slot.request, index),
                    "cacheKey": slot.cache_key,
                    "cached": cached,
                    "settled": slot.outcome.is_some(),
                    "error": slot.outcome.as_ref().and_then(|outcome| outcome.error.clone()),
                }),
            )
        });
        let mut manifest = json!({
            "runId": self.run_id,
            "scriptName": self.plan_name,
            "sourceSha256": self.store.source_digest(),
            "status": status,
            "startedAt": self.started_at,
            "resumed": self.resumed,
            "steps": crate::workflow_store::manifest_steps(entries),
        });
        if let Some(finished_at) = finished_at {
            manifest["finishedAt"] = Value::String(finished_at);
        }
        self.store.write_manifest(&manifest);
    }
}

/// Persists the run summary and per-step records.
///
/// Summaries are in dispatch order, not completion order. Replayed records already exist; terminal
/// records persisted at harvest are immutable and are not rewritten. The returned set identifies
/// steps with a complete disk record, allowing only those timeline contexts to externalize bodies.
fn persist_run(
    manifest: &RunManifest<'_>,
    status: SubagentRunStatus,
    slots: &[StepSlot],
) -> std::collections::HashSet<usize> {
    let store = manifest.store;
    let mut on_disk = std::collections::HashSet::new();
    for (index, slot) in slots.iter().enumerate() {
        if slot.record_persisted {
            on_disk.insert(index);
            continue;
        }
        let cached = slot.outcome.as_ref().is_some_and(|outcome| outcome.cached);
        if cached || slot.shared.is_none() {
            if store.step_exists(index) {
                on_disk.insert(index);
            }
            continue;
        }
        if let Ok(record) = serde_json::to_value(slot.shared.as_ref().map(|shared| shared.record()))
        {
            if store.write_step(index, &record) {
                on_disk.insert(index);
            }
        }
    }
    manifest.write(
        status_wire(status),
        Some(Utc::now().to_rfc3339()),
        slots,
    );
    on_disk
}

/// Builds the recovery hint appended to failed runs.
///
/// The bracketed run ID can be extracted mechanically for `resume_run_id`. Repeated starts with no
/// result uniquely distinguish recurring host crashes from a slow step and are reported only after
/// a failed resume.
fn resume_hint(
    profile: &PromptProfile,
    run_id: &str,
    journal: &crate::workflow_store::Journal,
    was_resume: bool,
) -> String {
    let mut hint = profile.render(PromptKey::WorkflowResumeHint, &[("run_id", run_id)]);
    if was_resume {
        // Start counts explain a failed resume only; an initial run necessarily has a count of 1.
        let repeated = journal
            .respawn_diagnostics()
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .count();
        if repeated > 0 {
            hint.push_str(&profile.render(
                PromptKey::WorkflowResumeRepeatedWarning,
                &[("count", &repeated.to_string())],
            ));
        }
    }
    hint
}

/// Extracts a run ID from failed output. Tests and the UI share this single format definition.
#[cfg(test)]
pub(crate) fn parse_resume_run_id(output: &str) -> Option<&str> {
    let start = output.find("This run's id is [")? + "This run's id is [".len();
    let rest = &output[start..];
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Converts driver state transitions into ledger entries and emits them.
///
/// The renderer also merges progress, but this ledger deduplicates and truncates before IPC and
/// determines transitions such as `blocked` that require the previous state. Progress is
/// decorative: sink failure is discovered by the wait-loop heartbeat, not by progress emission.
struct ProgressEmitter<'a> {
    sink: &'a ModelEventSink<'a>,
    round: usize,
    call_id: String,
    run_id: String,
    ledger: workflow_core::progress::ProgressLedger,
}

impl<'a> ProgressEmitter<'a> {
    fn new(
        sink: &'a ModelEventSink<'a>,
        round: usize,
        call_id: String,
        run_id: String,
    ) -> Self {
        Self {
            sink,
            round,
            call_id,
            run_id,
            ledger: workflow_core::progress::ProgressLedger::new(),
        }
    }

    /// Merges and emits a row. Rows rejected by the ledger are not emitted.
    fn upsert(&mut self, row: workflow_core::progress::ProgressRow) {
        if !self.ledger.upsert(row.clone()) {
            return;
        }
        let _ = (self.sink)(ModelStreamEvent::WorkflowProgress {
            round: self.round,
            call_id: self.call_id.clone(),
            run_id: self.run_id.clone(),
            entry: Box::new(row),
        });
    }

    /// Emits a `log()` narrative row. The ledger allocates its monotonic index and enforces the
    /// row limit.
    fn log(&mut self, message: &str) {
        let row = self.ledger.append_log(message);
        let _ = (self.sink)(ModelStreamEvent::WorkflowProgress {
            round: self.round,
            call_id: self.call_id.clone(),
            run_id: self.run_id.clone(),
            entry: Box::new(row),
        });
    }

    /// Enters a state for one step. Labels and phases are rewritten intentionally so a renderer
    /// that missed earlier events can still render a named running step.
    fn step(
        &mut self,
        index: usize,
        request: &WorkflowStepRequest,
        state: workflow_core::progress::WorkflowStepState,
        flags: StepFlags,
        message: Option<String>,
    ) {
        let mut row = workflow_core::progress::ProgressRow::agent(index);
        row.state = state;
        row.label = Some(step_display_label(request, index));
        row.phase = request.phase.clone();
        row.phase_index = request.phase_index;
        row.cached = flags.cached;
        row.blocked = flags.blocked;
        row.message = message;
        self.upsert(row);
    }

    /// Marks a step as skipped through the ledger. Skipping is encoded as `Error` plus `skipped`,
    /// not as a fifth state, so that rule remains centralized.
    fn skip(&mut self, index: usize, request: &WorkflowStepRequest) {
        self.ledger.mark_skipped(index);
        let mut row = workflow_core::progress::ProgressRow::agent(index);
        row.state = workflow_core::progress::WorkflowStepState::Error;
        row.skipped = true;
        row.label = Some(step_display_label(request, index));
        row.phase = request.phase.clone();
        row.phase_index = request.phase_index;
        row.message = Some(SKIPPED_STEP_REASON.to_owned());
        let _ = (self.sink)(ModelStreamEvent::WorkflowProgress {
            round: self.round,
            call_id: self.call_id.clone(),
            run_id: self.run_id.clone(),
            entry: Box::new(row),
        });
    }
}

/// Shared explanation for skipped steps in results and cards.
const SKIPPED_STEP_REASON: &str = "The user skipped this step";

/// Three orthogonal step facts passed as named fields rather than positional booleans.
#[derive(Clone, Copy, Default)]
struct StepFlags {
    cached: bool,
    blocked: bool,
}

#[allow(clippy::too_many_arguments)]
fn drive<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    pool: &'env AgentPool,
    parent: &RunModelRequest,
    parent_call: &ToolCall,
    state: &'env AppState,
    event_sink: &ModelEventSink<'_>,
    child_sink: &'env ModelEventSink<'env>,
    approve_dangerous_tool: &'env TaskApproval<'env>,
    round: usize,
    source: &mut dyn StepSource,
    deadline: Instant,
    live_limit: usize,
    slots: &'env mut Vec<StepSlot>,
    chain: &mut workflow_core::chain::CacheKeyChain,
    journal: &crate::workflow_store::Journal,
    store: Option<&crate::workflow_store::RunStore>,
    manifest: Option<&RunManifest<'_>>,
    run_id: &str,
    step_control: Option<&crate::state::WorkflowStepControl>,
    progress_card: &mut ProgressEmitter<'_>,
) -> Result<String, DriveAbort> {
    let mut outcomes: Vec<StepOutcome> = Vec::new();
    let mut in_flight: HashMap<String, usize> = HashMap::new();
    // Indices skipped while in flight. The heartbeat closure has only an immutable borrow, so this
    // set provides interior mutability.
    let skipped_in_flight: std::sync::Mutex<std::collections::HashSet<usize>> =
        std::sync::Mutex::new(std::collections::HashSet::new());
    // Capacity is backpressure, not failure. Queue excess dispatches so plan output does not vary
    // with host CPU count.
    let mut queued: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let abort = |pool: &AgentPool, reason: DriveAbort| -> DriveAbort {
        // Cancel before returning because scope joins all workers; cancellation after joining
        // cannot constrain the tool's actual return time.
        pool.cancel_all();
        reason
    };
    loop {
        if Instant::now() >= deadline {
            return Err(abort(pool, DriveAbort::Run(timeout_message(&parent.prompt_profile, slots))));
        }
        let progress = match source.advance(&outcomes) {
            Ok(progress) => progress,
            Err(error) => return Err(abort(pool, DriveAbort::Run(error.to_string()))),
        };
        // Emit narrative logs immediately after `advance` so `log()` remains adjacent to the
        // `agent()` requests produced in the same batch.
        for message in source.drain_logs() {
            progress_card.log(&message);
        }
        let requests = match progress {
            StepProgress::Done(value) => {
                // Once the plan returns, running steps cannot affect its result. Cancel them
                // before returning because `scope` joins workers on return; otherwise a losing
                // `Promise.race` branch can outlive the deadline and make a completed run appear
                // interrupted. Final harvesting records their real terminal states.
                let abandoned = pool
                    .all()
                    .into_iter()
                    .filter(|agent| agent.status() == AgentLiveStatus::Running)
                    .map(|agent| agent.label.clone())
                    .collect::<Vec<_>>();
                if !abandoned.is_empty() {
                    let steps = parent.prompt_profile.join_list(&abandoned);
                    progress_card.log(&parent.prompt_profile.render(
                        PromptKey::WorkflowLosersCancelled,
                        &[("count", &abandoned.len().to_string()), ("steps", &steps)],
                    ));
                }
                pool.cancel_all();
                return serde_json::to_string(&value).map_err(|error| {
                    abort(
                        pool,
                        DriveAbort::Run(format!("Workflow result could not be serialized: {error}")),
                    )
                });
            }
            StepProgress::Run(requests) => requests,
        };
        if requests.is_empty() && in_flight.is_empty() && queued.is_empty() {
            return Err(abort(
                pool,
                DriveAbort::Run("Workflow deadlock: no steps are in flight and the plan source produced no more steps".into()),
            ));
        }
        for step in requests {
            let index = slots.len();
            let step_call_id = format!("{}-ws{}", parent_call.id, index + 1);
            // Advance and consult the cache chain in dispatch order. No scheduling decision may
            // intervene, or replay would query a different position.
            let cache_key = chain.advance(&step);
            // Call `consult` exactly once per step, including misses. A miss closes the
            // one-way replay gate for all following steps.
            let journaled = journal.result(&cache_key).cloned();
            let replay = if chain.consult(journaled.is_some()) {
                journaled
            } else {
                None
            };
            slots.push(StepSlot {
                request: step,
                call_id: step_call_id,
                cache_key,
                shared: None,
                published_role: None,
                worktree: None,
                worktree_kept: None,
                outcome: None,
                record_persisted: false,
            });
            match replay {
                Some(value) => {
                    // Replays have no worker, queue entry, or live-pool cost.
                    let outcome = StepOutcome {
                        index,
                        value: Some(value),
                        cached: true,
                        error: None,
                        tokens: None,
                    };
                    // A replay enters Done directly because it has no intermediate state.
                    progress_card.step(
                        index,
                        &slots[index].request,
                        workflow_core::progress::WorkflowStepState::Done,
                        StepFlags {
                            cached: true,
                            blocked: false,
                        },
                        None,
                    );
                    outcomes.push(outcome.clone());
                    slots[index].outcome = Some(outcome);
                }
                None => {
                    // Show queued steps immediately; an invisible queued step is
                    // indistinguishable from a nonexistent one.
                    progress_card.step(
                        index,
                        &slots[index].request,
                        workflow_core::progress::WorkflowStepState::Start,
                        StepFlags {
                            cached: false,
                            blocked: true,
                        },
                        None,
                    );
                    queued.push_back(index);
                }
            }
        }
        // Start queued steps in dispatch order while capacity permits. Check the deadline inside
        // the pump so a large batch cannot issue new steps after expiry.
        while in_flight.len() < live_limit {
            if Instant::now() >= deadline {
                return Err(abort(pool, DriveAbort::Run(timeout_message(&parent.prompt_profile, slots))));
            }
            let Some(index) = queued.pop_front() else {
                break;
            };
            // Apply a skip before dispatch. A never-started skipped step must not emit a partial
            // transcript or consume a request.
            if step_control.is_some_and(|control| control.take_skip(index)) {
                progress_card.skip(index, &slots[index].request);
                let outcome = StepOutcome {
                    index,
                    value: None,
                    cached: false,
                    error: Some(SKIPPED_STEP_REASON.to_owned()),
                    tokens: None,
                };
                // Skipped steps have no cacheable result. They and later dependent steps rerun
                // on resume; caching a manual intervention would make it part of the plan.
                chain.mark_diverged();
                outcomes.push(outcome.clone());
                slots[index].outcome = Some(outcome);
                continue;
            }
            // Create an isolated worktree before registration because it is the step workspace.
            // Use the step ordinal, not the pool name: names are allocated by actual start order
            // and spawn failures do not consume one.
            //
            // Worktree creation failure is a null result for this step, not a run failure.
            let prepared = match slots[index].request.isolation.as_deref() {
                None => Ok(None),
                Some(_) => crate::git::create_isolated_worktree(
                    std::path::Path::new(&parent.workspace_path),
                    run_id,
                    &format!("ws{}", index + 1),
                )
                .map(Some)
                .map_err(|error| format!("Could not create an isolated worktree for this step: {error}")),
            };
            let mut published_role = None;
            let spawned = match prepared {
                Err(reason) => SpawnOutcome::StepFailed(reason),
                Ok(worktree) => {
                    slots[index].worktree = worktree;
                    spawn_step(
                        scope,
                        pool,
                        parent,
                        state,
                        child_sink,
                        approve_dangerous_tool,
                        round,
                        &slots[index].request,
                        slots[index].worktree.as_ref(),
                        index,
                        &slots[index].call_id,
                        &mut published_role,
                    )
                }
            };
            slots[index].published_role = published_role;
            match spawned {
                SpawnOutcome::Spawned(shared) => {
                    // Persist `Started` at dispatch so a crash after starting but before return
                    // remains diagnosable.
                    if let Some(store) = store {
                        store.append(&crate::workflow_store::JournalLine::Started {
                            key: slots[index].cache_key.clone(),
                            agent_id: shared.name.clone(),
                        });
                    }
                    // Dequeueing clears `blocked`, which distinguishes waiting for capacity from
                    // a running step.
                    progress_card.step(
                        index,
                        &slots[index].request,
                        workflow_core::progress::WorkflowStepState::Progress,
                        StepFlags::default(),
                        None,
                    );
                    in_flight.insert(shared.name.clone(), index);
                    slots[index].shared = Some(shared);
                }
                SpawnOutcome::StepFailed(reason) => {
                    // Spawn failure is a null result for this step. The plan source decides
                    // whether to short-circuit.
                    progress_card.step(
                        index,
                        &slots[index].request,
                        workflow_core::progress::WorkflowStepState::Error,
                        StepFlags::default(),
                        Some(reason.clone()),
                    );
                    let outcome = StepOutcome {
                        index,
                        value: None,
                        cached: false,
                        error: Some(reason),
                        tokens: None,
                    };
                    chain.mark_diverged();
                    outcomes.push(outcome.clone());
                    slots[index].outcome = Some(outcome);
                }
                SpawnOutcome::SinkDead { error, registered } => {
                    // Retain a partially registered worker so final harvesting can record its
                    // real terminal state.
                    slots[index].shared = registered;
                    return Err(abort(pool, DriveAbort::SinkDead(error)));
                }
            }
        }
        if in_flight.is_empty() {
            // Let the source consume this batch of immediate failures or replays.
            continue;
        }
        let watched = in_flight
            .keys()
            .filter_map(|name| pool.find(name))
            .collect::<Vec<_>>();
        // Heartbeats make cancellation observable. The renderer ignores empty status deltas, but
        // a dead sink aborts the wait within one poll interval. In-flight skips use this same
        // observation point because the driver otherwise blocks in `wait_activity_until`.
        let heartbeat = || {
            if let Some(control) = step_control {
                for (name, index) in in_flight.iter() {
                    if !control.take_skip(*index) {
                        continue;
                    }
                    // Cancel only the selected worker. `cancel_all` would cancel the entire fan-out.
                    // This is the driver's own scheduling decision, not the user closing a task, so
                    // the step's result must not claim it was closed by hand.
                    if let Some(shared) = pool.find(name) {
                        shared.request_stop(crate::agents::StopOrigin::Host);
                        skipped_in_flight
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(*index);
                    }
                }
            }
            event_sink(ModelStreamEvent::SubagentDelta {
                round,
                call_id: parent_call.id.clone(),
                channel: SubagentChannel::Status,
                delta: String::new(),
            })
        };
        let wait = match pool.wait_activity_until(&watched, deadline, &heartbeat) {
            Ok(wait) => wait,
            Err(error) => return Err(abort(pool, DriveAbort::SinkDead(error))),
        };
        if wait.timed_out && Instant::now() >= deadline {
            return Err(abort(pool, DriveAbort::Run(timeout_message(&parent.prompt_profile, slots))));
        }
        // Use envelopes only as wake-up signals; the record is the authoritative result.
        let finished = in_flight
            .iter()
            .filter_map(|(name, index)| {
                let shared = pool.find(name)?;
                (shared.status() != AgentLiveStatus::Running).then(|| (name.clone(), *index))
            })
            .collect::<Vec<_>>();
        let harvested_any = !finished.is_empty();
        for (name, index) in finished {
            in_flight.remove(&name);
            let shared = slots[index]
                .shared
                .clone()
                .expect("an in-flight slot keeps its shared handle");
            let outcome = harvest_outcome(index, &slots[index].request, &shared, &parent.prompt_profile);
            let was_skipped = skipped_in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&index);
            if was_skipped {
                // A user-skipped in-flight step ends by cancellation. Its stopped result is
                // redundant to the user, who initiated the skip.
                progress_card.skip(index, &slots[index].request);
            } else {
                // A terminal value is Done; otherwise the step is Error. Missing values cannot
                // be replayed and must not appear completed.
                let state = if outcome.value.is_some() {
                    workflow_core::progress::WorkflowStepState::Done
                } else {
                    workflow_core::progress::WorkflowStepState::Error
                };
                progress_card.step(
                    index,
                    &slots[index].request,
                    state,
                    StepFlags::default(),
                    outcome.error.clone(),
                );
            }
            journal_outcome(store, chain, &slots[index].cache_key, &name, &outcome);
            slots[index].outcome = Some(outcome.clone());
            // Persist the complete immutable terminal record at harvest, rather than waiting for
            // run finalization, so a later crash cannot lose its transcript or usage.
            if let Some(store) = store {
                if let Ok(record) = serde_json::to_value(shared.record()) {
                    slots[index].record_persisted = store.write_step(index, &record);
                }
            }
            outcomes.push(outcome);
        }
        // Update the running manifest after each harvest batch for crash recovery.
        if let (Some(manifest), true) = (manifest, harvested_any) {
            manifest.write("running", None, slots);
        }
    }
}

/// Writes a step terminal state to the journal. Only steps with values write `Result` entries.
///
/// Null results, including skips, terminal errors, and missing structured output, retain only the
/// `Started` entry. They must rerun on resume, as must all later steps after the one-way cache gate
/// diverges. `mark_diverged` keeps that property local to this function; caching a null result
/// would permanently preserve a failure as the plan answer.
fn journal_outcome(
    store: Option<&crate::workflow_store::RunStore>,
    chain: &mut workflow_core::chain::CacheKeyChain,
    cache_key: &str,
    agent_id: &str,
    outcome: &StepOutcome,
) {
    let Some(value) = outcome.value.clone() else {
        chain.mark_diverged();
        // A diagnostic `Settled` line is not cached. It distinguishes a real step failure from a
        // crash after `Started`, which otherwise have identical journal shapes.
        if let Some(store) = store {
            store.append(&crate::workflow_store::JournalLine::Settled {
                key: cache_key.to_owned(),
                agent_id: agent_id.to_owned(),
                error: outcome.error.clone(),
            });
        }
        return;
    };
    if let Some(store) = store {
        store.append(&crate::workflow_store::JournalLine::Result {
            key: cache_key.to_owned(),
            agent_id: agent_id.to_owned(),
            result: value,
        });
    }
}

fn timeout_message(profile: &PromptProfile, slots: &[StepSlot]) -> String {
    let unfinished = slots.iter().filter(|slot| slot.outcome.is_none()).count();
    profile.render(
        PromptKey::WorkflowTimeout,
        &[
            ("seconds", &WORKFLOW_RUN_DEADLINE.as_secs().to_string()),
            ("unfinished", &unfinished.to_string()),
        ],
    )
}

/// Converts a completed step's pool state into a result consumable by the plan source.
///
/// Schema-bound steps accept only `structured_output`; ordinary steps use the final assistant
/// text. Every non-completed state becomes a null result with an error.
fn harvest_outcome(
    index: usize,
    request: &WorkflowStepRequest,
    shared: &Arc<AgentShared>,
    profile: &PromptProfile,
) -> StepOutcome {
    let record = shared.record();
    let (value, error) = match record.status {
        SubagentRunStatus::Completed => {
            if request.schema.is_some() {
                match record.structured_output.clone() {
                    Some(value) => (Some(value), None),
                    None => (None, Some(profile.text(PromptKey::WorkflowStepNoStructured).to_owned())),
                }
            } else {
                (Some(Value::String(final_assistant_text(&record))), None)
            }
        }
        other => (
            None,
            Some(profile.render(
                PromptKey::WorkflowStepEndedWith,
                &[("status", status_wire(other))],
            )),
        ),
    };
    // Account provider-reported totals when available, otherwise sum input and output. Failed
    // steps count because the budget constrains actual cost, not successful output.
    let tokens = record.usage.total_tokens.or_else(|| {
        match (record.usage.input_tokens, record.usage.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            (Some(input), None) => Some(input),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        }
    });
    StepOutcome {
        index,
        value,
        cached: false,
        error,
        tokens,
    }
}

/// Wire name for `SubagentRunStatus`, used in status deltas and error text.
///
/// This follows serde wire names, including the `roundLimit` exception. It is presentation logic
/// specific to this module rather than a record-type contract.
fn status_wire(status: SubagentRunStatus) -> &'static str {
    match status {
        SubagentRunStatus::Completed => "completed",
        SubagentRunStatus::Interrupted => "interrupted",
        SubagentRunStatus::Failed => "failed",
        SubagentRunStatus::Stopped => "stopped",
        SubagentRunStatus::RoundLimit => "roundLimit",
    }
}

fn final_assistant_text(record: &SubagentRunRecord) -> String {
    record
        .contexts
        .iter()
        .rev()
        .find_map(|context| match context {
            ContextItem::Assistant { content, .. } if !content.trim().is_empty() => {
                Some(content.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// The three possible outcomes of starting a step.
///
/// `StepFailed` is local to one step and supplies a null result to the plan source. `SinkDead`
/// affects the entire run and must abort immediately. A partially registered worker returns its
/// handle so final harvesting can record its true terminal state.
enum SpawnOutcome {
    Spawned(Arc<AgentShared>),
    StepFailed(String),
    SinkDead {
        error: String,
        registered: Option<Arc<AgentShared>>,
    },
}

#[allow(clippy::too_many_arguments)]
fn spawn_step<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    pool: &'env AgentPool,
    parent: &RunModelRequest,
    state: &'env AppState,
    child_sink: &'env ModelEventSink<'env>,
    approve_dangerous_tool: &'env TaskApproval<'env>,
    round: usize,
    step: &WorkflowStepRequest,
    worktree: Option<&crate::git::IsolatedWorktree>,
    step_index: usize,
    step_call_id: &str,
    published_role: &mut Option<AgentDefinitionBinding>,
) -> SpawnOutcome {
    let name = workflow_step_name(pool, step.agent_type.as_deref());
    let label = step_display_label(step, step_index);
    let mut template = agent_child_template(parent);
    // Apply the role before effort and schema. `configure_named_agent_template` replaces the
    // template wholesale, so step-level overrides must follow it.
    if let Some(agent_type) = step.agent_type.as_deref() {
        if let Err(error) = crate::api::apply_named_role_to_fresh_template(
            parent,
            state,
            &mut template,
            agent_type,
        ) {
            return SpawnOutcome::StepFailed(error);
        }
    }
    // An isolated step's worktree is its trusted workspace. File-tool scope, shell CWD, and Git
    // status derive from it, so assign it after role application for the same reason as other
    // step-level overrides.
    if let Some(worktree) = worktree {
        template.workspace_path = worktree.path.to_string_lossy().into_owned();
    }
    if let Some(effort) = step.effort.as_deref() {
        match parse_step_effort(effort) {
            Ok(effort) => template.reasoning_effort = effort,
            Err(error) => return SpawnOutcome::StepFailed(error),
        }
    }
    if let Some(schema) = &step.schema {
        // Use the same byte-limit and compilation path as spawn and continuation; reject this
        // step when compilation fails.
        match orchestration::compile_output_schema(schema)
            .and_then(|compiled| install_structured_output(&mut template, compiled))
        {
            Ok(()) => {},
            Err(error) => return SpawnOutcome::StepFailed(error),
        }
    }
    let initial_contexts = vec![ContextItem::User {
        id: new_context_id("workflow-step-task"),
        content: step.prompt.clone(),
        images: Vec::new(),
        created_at: Utc::now().to_rfc3339(),
    }];
    template.contexts = Vec::new();
    template.context_load_actor_name = Some(name.clone());
    if let Err(error) = pool.preflight_registration(&name, &template, AgentLiveStatus::Running) {
        return SpawnOutcome::StepFailed(error);
    }
    if let Err(error) =
        reserve_subagent_execution_mode(&template, state, &name, SubagentRunKind::WorkflowStep)
    {
        return SpawnOutcome::StepFailed(error);
    }
    // Announce the synthesized step context before worker events so the renderer has a transcript
    // anchor.
    if let Err(error) = child_sink(ModelStreamEvent::ToolCallAnnounced {
        round,
        call_id: step_call_id.to_owned(),
        tool_name: WORKFLOW_STEP_TOOL.to_owned(),
        // Filled in by the run's event sink, which is the only layer that
        // knows the conversation.
        context_id: String::new(),
    }) {
        return SpawnOutcome::SinkDead {
            error,
            registered: None,
        };
    }
    // Immediately follow the announcement with identity input. `ToolCallAnnounced` cannot carry
    // a step's label, phase, role, or prompt, while the renderer reads all of them from tool input.
    // The role is from the applied template binding, so it reflects the role that actually runs the
    // step. Prior failure paths have emitted no event; after this point only registration can fail.
    if let Err(error) = child_sink(ModelStreamEvent::ToolCallArgumentsReady {
        round,
        call_id: step_call_id.to_owned(),
        input: step_identity_input(
            step,
            &label,
            Some(&step.prompt),
            worktree,
            None,
            template.agent_definition_binding.as_ref(),
        ),
    }) {
        return SpawnOutcome::SinkDead {
            error,
            registered: None,
        };
    }
    *published_role = template.agent_definition_binding.clone();
    let shared = match pool.register(
        name,
        label,
        step.prompt.clone(),
        template,
        SubagentRunKind::WorkflowStep,
        state,
        initial_contexts,
        Vec::new(),
        AgentLiveStatus::Running,
        step_call_id.to_owned(),
        None,
    ) {
        Ok(shared) => shared,
        Err(error) => return SpawnOutcome::StepFailed(error),
    };
    if let Err(error) = start_registered_step_worker(
        scope,
        Arc::clone(&shared),
        state,
        child_sink,
        approve_dangerous_tool,
        round,
        step_call_id,
    ) {
        // Registration succeeded but its Running delta could not be sent. The guard has marked
        // the pool state Interrupted, so return the handle for accounting.
        return SpawnOutcome::SinkDead {
            error,
            registered: Some(shared),
        };
    }
    SpawnOutcome::Spawned(shared)
}

#[cfg(test)]
pub(crate) fn assert_failed_step_retains_published_role(parent: &RunModelRequest, state: &AppState) {
    let pool = AgentPool::with_live_limit(1);
    let mut blocker = agent_child_template(parent);
    crate::api::apply_named_role_to_fresh_template(parent, state, &mut blocker, "reviewer").unwrap();
    let observed = std::sync::Mutex::new(None);
    let sink = |event: ModelStreamEvent| -> Result<(), String> {
        if let ModelStreamEvent::ToolCallArgumentsReady { input, .. } = event {
            assert_eq!(input.get("role"), Some(&json!("reviewer")));
            *observed.lock().unwrap() = Some(input);
            // Deterministically exhaust capacity after preflight and after the
            // identity event, but before the real registration attempt.
            pool.register("blocker".into(), "blocker".into(), "hold capacity".into(),
                blocker.clone(), SubagentRunKind::WorkflowStep, state, Vec::new(),
                Vec::new(), AgentLiveStatus::Running, "blocker-call".into(), None).unwrap();
        }
        Ok(())
    };
    let approve = |_: &ToolExecutionRequest, _: &crate::model::ToolDescriptor,
        _: crate::api::ApprovalRequester<'_>, _: Option<&std::sync::atomic::AtomicBool>|
        -> Result<bool, String> { Ok(true) };
    let mut step = WorkflowStepRequest::from_prompt("review the host");
    step.agent_type = Some("reviewer".into());
    let mut published_role = None;
    let outcome = std::thread::scope(|scope| spawn_step(scope, &pool, parent, state,
        &sink, &approve, 1, &step, None, 0, "parent-ws1", &mut published_role));
    let SpawnOutcome::StepFailed(error) = outcome else {
        panic!("registration must fail after the identity event");
    };
    let streamed = observed.into_inner().unwrap().expect("identity event was emitted");
    let mut slot = StepSlot {
        request: step, call_id: "parent-ws1".into(), cache_key: "test-key".into(),
        shared: None, published_role, worktree: None, worktree_kept: None,
        outcome: Some(StepOutcome { index: 0, value: None, cached: false,
            error: Some(error), tokens: None }), record_persisted: false,
    };
    let settled = synthesize_step_context(&slot, 0, 1, None, &parent.conversation_id,
        state, &parent.prompt_profile);
    let ContextItem::Tool { input, result, subagent, .. } = settled else { panic!("tool card") };
    assert!(!result.success);
    assert!(subagent.is_none());
    assert_eq!(input.get("role"), streamed.get("role"));
    assert_eq!(input.get("roleModelId"), streamed.get("roleModelId"));
    slot.published_role = None;
    let ContextItem::Tool { input, .. } = synthesize_step_context(&slot, 0, 1, None,
        &parent.conversation_id, state, &parent.prompt_profile) else { panic!("tool card") };
    assert!(input.get("role").is_none());
    assert!(input.get("roleModelId").is_none());
}

/// Returns a step's display name.
///
/// The fallback order is explicit label, phase, then step ordinal. Use the ordinal rather than a
/// pool name because pool names follow actual start order and failed spawns do not consume one;
/// the ordinal is stable across streamed and final records.
fn step_display_label(step: &WorkflowStepRequest, index: usize) -> String {
    step.label
        .clone()
        .or_else(|| step.phase.clone())
        .unwrap_or_else(|| format!("ws{}", index + 1))
}

/// Builds the tool input that identifies a step: label, phase, optional prompt, and worktree.
///
/// Streaming and final records share this construction to keep their labels and phases stable. A
/// prompt is included only when no nested record can carry it. `kept` is unknown until cleanup;
/// worktree path and branch are the coordinates for locating retained work. `role` is the resolved
/// host binding, not the requested `agentType`, so the task surface displays the actual role.
fn step_identity_input(
    step: &WorkflowStepRequest,
    label: &str,
    prompt: Option<&str>,
    worktree: Option<&crate::git::IsolatedWorktree>,
    kept: Option<bool>,
    role: Option<&AgentDefinitionBinding>,
) -> JsonObject {
    let mut input = JsonObject::new();
    input.insert("label".into(), Value::String(label.to_owned()));
    if let Some(role) = role {
        input.insert("role".into(), Value::String(role.name.clone()));
        input.insert("roleModelId".into(), Value::String(role.model_id.clone()));
    }
    if let Some(phase) = &step.phase {
        input.insert("phase".into(), Value::String(phase.clone()));
    }
    if let Some(phase_index) = step.phase_index {
        input.insert("phaseIndex".into(), json!(phase_index));
    }
    if let Some(prompt) = prompt {
        input.insert("task".into(), Value::String(prompt.to_owned()));
    }
    if let Some(worktree) = worktree {
        input.insert(
            "worktree".into(),
            Value::String(worktree.path.display().to_string()),
        );
        input.insert(
            "worktreeBranch".into(),
            Value::String(worktree.branch.clone()),
        );
        if let Some(kept) = kept {
            input.insert("worktreeKept".into(), Value::Bool(kept));
        }
    }
    input
}

fn parse_step_effort(effort: &str) -> Result<ReasoningEffort, String> {
    serde_json::from_value(Value::String(effort.to_owned()))
        .map_err(|_| format!("Invalid workflow step effort value: {effort}"))
}

/// Character limit for the timeline preview of an externalized step body.
///
/// Full bodies reside in `steps/<index>.json` and are fetched on demand. Enlarging this preview
/// would reintroduce full transcript data into the document.
const STEP_OUTPUT_PREVIEW_CHARS: usize = 600;

/// Creates a timeline preview for an externalized body. Preserve short output unchanged.
fn step_output_preview(profile: &PromptProfile, full: &str) -> String {
    match full.char_indices().nth(STEP_OUTPUT_PREVIEW_CHARS) {
        None => full.to_owned(),
        Some((byte_offset, _)) => format!(
            "{}{}",
            &full[..byte_offset],
            profile.text(PromptKey::WorkflowStepPreviewTruncated)
        ),
    }
}

/// Synthesizes one `workflow_step` tool context.
///
/// `external_body_run` identifies a run whose complete record is confirmed on disk. In that case,
/// the context retains input metadata, an output preview, and retrieval coordinates while omitting
/// the nested record; disk is the sole full body copy. Without it, the inline nested record remains
/// the only complete body and must be retained.
fn synthesize_step_context(
    slot: &StepSlot,
    index: usize,
    round: usize,
    external_body_run: Option<&str>,
    conversation_id: &str,
    state: &AppState,
    profile: &PromptProfile,
) -> ContextItem {
    let record = slot.shared.as_ref().map(|shared| shared.record());
    // Include the prompt when no nested record carries it: unstarted, replayed, and externalized
    // steps rely on input as its sole document representation.
    let mut input = step_identity_input(
        &slot.request,
        &step_display_label(&slot.request, index),
        (record.is_none() || external_body_run.is_some()).then_some(slot.request.prompt.as_str()),
        slot.worktree.as_ref(),
        slot.worktree_kept,
        // Prefer the actual worker record. If registration failed after its
        // identity event, preserve the host-resolved presentation snapshot.
        record
            .as_ref()
            .and_then(|record| record.agent_definition.as_ref())
            .or(slot.published_role.as_ref()),
    );
    let (success, output) = match &slot.outcome {
        Some(outcome) => match (&outcome.value, &outcome.error) {
            (Some(value), _) => (
                true,
                serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
            ),
            (None, Some(error)) => (false, error.clone()),
            (None, None) => (false, profile.text(PromptKey::WorkflowStepNoResult).to_owned()),
        },
        None => (false, profile.text(PromptKey::WorkflowStepNotStarted).to_owned()),
    };
    let (output, subagent) = match external_body_run {
        Some(run_id) => {
            input.insert("runId".into(), Value::String(run_id.to_owned()));
            input.insert("stepIndex".into(), json!(index));
            // Without a nested record, retain terminal status and a body fingerprint in the shell
            // for the drawer list and disk-copy audit.
            let status = record
                .as_ref()
                .map(|record| status_wire(record.status))
                .unwrap_or(if success { "completed" } else { "failed" });
            input.insert("status".into(), Value::String(status.to_owned()));
            // Retain usage and tool count in the shell. They are small metadata and should not
            // require on-demand IPC merely to display the timeline row.
            if let Some(record) = record.as_ref() {
                if let Ok(usage) = serde_json::to_value(&record.usage) {
                    input.insert("usage".into(), usage);
                }
                // Match the renderer's definition: count transcript tool contexts, excluding
                // synthesized task-user turns and supplemental progress rows.
                input.insert(
                    "toolUseCount".into(),
                    json!(record
                        .contexts
                        .iter()
                        .filter(|context| matches!(context, ContextItem::Tool { .. }))
                        .count()),
                );
            }
            input.insert("outputBytes".into(), json!(output.len()));
            input.insert(
                "outputSha256".into(),
                Value::String(crate::workflow_store::hex_digest(output.as_bytes())),
            );
            (step_output_preview(profile, &output), None)
        }
        None => (output, record),
    };
    let result = ToolResult {
        success,
        output,
        images: Vec::new(),
        diff: None,
        executed_at: Utc::now().to_rfc3339(),
        duration_ms: 0,
    };
    // Step cards require attestations at save time because they round-trip through the renderer
    // and contain mutable nested records.
    let attestation = state.attest_tool_context(&crate::tool_attestation::AttestationSubject {
        conversation_id,
        context_id: &slot.call_id,
        tool_name: WORKFLOW_STEP_TOOL,
        input: &input,
        requested_input: None,
        result: &result,
        subagent: subagent.as_ref(),
    });
    ContextItem::Tool {
        // The context ID is the step call ID, preserving the same transcript anchor used to route
        // nested live events after final persistence and reload.
        id: slot.call_id.clone(),
        tool_name: WORKFLOW_STEP_TOOL.to_owned(),
        round: Some(round),
        model_turn_id: None,
        requested_input: None,
        input,
        result,
        subagent,
        attestation,
        created_at: Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_step_name_generator_skips_names_held_by_the_live_pool() {
        let pool = AgentPool::unbounded();
        assert_eq!(workflow_step_name(&pool, None), "ws1");
    }

    #[test]
    fn a_role_step_takes_its_own_slot_namespace() {
        let pool = AgentPool::unbounded();
        // A plain step and a role step never collide on one reservation key.
        // The execution-mode receipt is held per (conversation, name) FOREVER,
        // so reusing `ws1` for both would make the second run of a plan fail
        // with "permanently reserved to another execution mode".
        assert_eq!(workflow_step_name(&pool, None), "ws1");
        assert_eq!(
            workflow_step_name(&pool, Some("reviewer")),
            "ws1-as-reviewer"
        );
        // And the composed name has to survive the pool's own name rules, or
        // `preflight_registration` would reject every role step.
        assert!(crate::agents::validate_agent_name("ws1-as-reviewer").is_ok());
        assert!(crate::agents::validate_agent_name("ws12-as-code-reviewer").is_ok());
    }

    #[test]
    fn step_effort_parses_the_ladder_and_rejects_strays() {
        assert!(parse_step_effort("low").is_ok());
        assert!(parse_step_effort("medium").is_ok());
        assert!(parse_step_effort("high").is_ok());
        assert!(parse_step_effort("xhigh").is_ok());
        assert!(parse_step_effort("max").is_err());
    }

    #[test]
    fn the_live_limit_stays_inside_the_specified_window() {
        let limit = workflow_live_limit();
        assert!((2..=16).contains(&limit), "{limit}");
    }

    /// A step that starts repeatedly without producing a result must be reported in failed resume
    /// output. A `started` count greater than 1 with no `result` distinguishes recurring host
    /// crashes from a merely slow step, and is meaningful only for failed resumes.
    #[test]
    fn a_step_that_started_repeatedly_without_ever_finishing_says_so_on_a_failed_resume() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::workflow_store::RunStore::open(
            directory.path(),
            "conv1",
            "run1",
            b"{\"name\":\"p\"}",
        )
        .unwrap();
        // Three starts without a result represent repeated host crashes on one step.
        for _ in 0..3 {
            store.append(&crate::workflow_store::JournalLine::Started {
                key: "mw1:doomed".into(),
                agent_id: "ws1".into(),
            });
        }
        // A single unfinished start is slow, not repeated.
        store.append(&crate::workflow_store::JournalLine::Started {
            key: "mw1:merely-slow".into(),
            agent_id: "ws2".into(),
        });
        let journal = store.load_journal();

        let profile = PromptProfile::builtin_english();
        let resumed = resume_hint(&profile, "run1", &journal, true);
        assert!(
            resumed.contains("1 steps started repeatedly without ever producing a result"),
            "{resumed}"
        );
        // An initial run always has count 1, so it provides no diagnostic value.
        let first_run = resume_hint(&profile, "run1", &journal, false);
        assert!(!first_run.contains("started repeatedly"), "{first_run}");
        // Both paths need a run ID to provide a recovery entry point.
        for hint in [&resumed, &first_run] {
            assert_eq!(parse_resume_run_id(hint), Some("run1"), "{hint}");
        }
    }

    /// Unnamed steps need distinct fallback labels so each task-panel row remains identifiable.
    fn unnamed_step(prompt: &str) -> WorkflowStepRequest {
        WorkflowStepRequest::from_prompt(prompt)
    }

    #[test]
    fn step_labels_fall_back_through_phase_to_a_distinct_ordinal() {
        let mut labelled = unnamed_step("a");
        labelled.label = Some("审查一".into());
        labelled.phase = Some("Review".into());
        assert_eq!(step_display_label(&labelled, 0), "审查一");

        // A phase is the fallback without a label; steps in the same phase share a group.
        let mut phased = unnamed_step("b");
        phased.phase = Some("Review".into());
        assert_eq!(step_display_label(&phased, 3), "Review");

        // Without either, use a distinct ordinal.
        assert_eq!(step_display_label(&unnamed_step("c"), 0), "ws1");
        assert_ne!(
            step_display_label(&unnamed_step("c"), 0),
            step_display_label(&unnamed_step("d"), 1)
        );
    }

    /// Streamed identity and the final context must use the same label and phase for a step.
    #[test]
    fn the_streamed_identity_and_the_settled_context_agree() {
        let mut step = unnamed_step("检查调度器");
        step.phase = Some("Review".into());
        step.phase_index = Some(2);

        let label = step_display_label(&step, 0);
        let streamed = step_identity_input(&step, &label, Some(&step.prompt), None, None, None);
        let settled = step_identity_input(&step, &label, None, None, None, None);

        for input in [&streamed, &settled] {
            assert_eq!(input.get("label"), Some(&Value::String("Review".into())));
            assert_eq!(input.get("phase"), Some(&Value::String("Review".into())));
            assert_eq!(input.get("phaseIndex"), Some(&json!(2)));
        }
        // During streaming, input is the sole prompt carrier; nested records make a duplicate
        // unnecessary after settlement.
        assert_eq!(
            streamed.get("task"),
            Some(&Value::String("检查调度器".into()))
        );
        assert_eq!(settled.get("task"), None);
    }

    /// Both paths include worktree coordinates, but only final cleanup can determine whether the
    /// worktree was retained.
    #[test]
    fn the_worktree_coordinates_stream_immediately_but_its_fate_only_settles_at_the_end() {
        let mut step = unnamed_step("并行改文件");
        step.isolation = Some("worktree".into());
        let worktree = crate::git::IsolatedWorktree {
            path: std::path::PathBuf::from("/repo/.mework/worktrees/run1/ws1"),
            branch: "mework/wf/run1/ws1".into(),
            base_oid: "deadbeef".into(),
        };
        let label = step_display_label(&step, 0);

        let streamed = step_identity_input(
            &step,
            &label,
            Some(&step.prompt),
            Some(&worktree),
            None,
            None,
        );
        let settled = step_identity_input(&step, &label, None, Some(&worktree), Some(true), None);

        for input in [&streamed, &settled] {
            assert_eq!(
                input.get("worktree"),
                Some(&Value::String(worktree.path.display().to_string()))
            );
            assert_eq!(
                input.get("worktreeBranch"),
                Some(&Value::String("mework/wf/run1/ws1".into()))
            );
        }
        assert_eq!(streamed.get("worktreeKept"), None);
        assert_eq!(settled.get("worktreeKept"), Some(&Value::Bool(true)));

        // Non-isolated steps must not gain worktree keys; their presence controls worktree-row
        // rendering.
        let plain = unnamed_step("普通步骤");
        let plain_input = step_identity_input(&plain, "ws1", None, None, None, None);
        assert!(plain_input.get("worktree").is_none());
        assert!(plain_input.get("worktreeBranch").is_none());
        assert!(plain_input.get("worktreeKept").is_none());
    }

    /// An unphased plan must not gain a phase key; otherwise the renderer would show the ordinal
    /// fallback label as a phase heading.
    #[test]
    fn an_unphased_step_carries_no_phase_keys() {
        let step = unnamed_step("无阶段");
        let input =
            step_identity_input(&step, &step_display_label(&step, 0), None, None, None, None);
        assert_eq!(input.get("label"), Some(&Value::String("ws1".into())));
        assert!(input.get("phase").is_none());
        assert!(input.get("phaseIndex").is_none());
    }

    /// The resolved role binding belongs in the shell from the first frame. Externalized nested
    /// records are unavailable to the task surface without on-demand IPC.
    #[test]
    fn the_bound_role_rides_the_shell_from_the_first_frame() {
        let step = unnamed_step("审查");
        let label = step_display_label(&step, 0);
        let binding = AgentDefinitionBinding {
            source: crate::model::AgentDefinitionSource::User,
            source_key: String::new(),
            name: "reviewer".into(),
            revision: 1,
            memory_epoch: 1,
            provider_id: "provider-x".into(),
            model_id: "sonnet-5".into(),
            memory: crate::model::AgentDefinitionMemory::None,
            scope_key: String::new(),
            configuration_receipt: "ab".repeat(32),
            receipt_version: 1,
        };

        let streamed = step_identity_input(
            &step,
            &label,
            Some(&step.prompt),
            None,
            None,
            Some(&binding),
        );
        let settled = step_identity_input(&step, &label, None, None, None, Some(&binding));
        for input in [&streamed, &settled] {
            assert_eq!(input.get("role"), Some(&Value::String("reviewer".into())));
            assert_eq!(
                input.get("roleModelId"),
                Some(&Value::String("sonnet-5".into()))
            );
        }

        // Roleless steps must not gain empty keys: their presence selects role display versus
        // model fallback in the renderer.
        let roleless = step_identity_input(&step, &label, None, None, None, None);
        assert!(roleless.get("role").is_none());
        assert!(roleless.get("roleModelId").is_none());
    }
}
