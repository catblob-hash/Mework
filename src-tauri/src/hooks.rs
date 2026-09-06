use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use regex::Regex;
use serde_json::Value;
use uuid::Uuid;
use wait_timeout::ChildExt;

use crate::{
    cancel::CancelSignal,
    model::{HookDefinition, HookEvent, ToolResult},
};

#[allow(dead_code)]
pub(crate) mod instructions_loaded;

const MAX_HOOK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const RUN_HOOK_BUDGET: Duration = Duration::from_secs(5 * 60);
const MAX_HOOK_OUTPUT: usize = 64 * 1024;
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(500);
/// How often a running hook command re-checks its cancellation signal.
/// A hook has minutes of budget; without this slice, user stop waits out the
/// whole remainder.
const CANCELLATION_POLL: Duration = Duration::from_millis(100);
pub(crate) const HOOK_EVENT_ENV: &str = "MEWORK_HOOK_EVENT";
pub(crate) const LEGACY_HOOK_EVENT_ENV: &str = "NAIWORD_HOOK_EVENT";
#[cfg(windows)]
const PROCESS_TREE_KILL_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookPermissionDecision {
    Allow,
    Ask,
    Defer,
    Deny,
}

#[derive(Clone, Debug, Default)]
pub struct HookDecision {
    pub blocked: bool,
    pub halt: bool,
    pub interrupt: bool,
    pub reason: Option<String>,
    pub additional_context: Option<String>,
    pub system_message: Option<String>,
    pub permission_decision: Option<HookPermissionDecision>,
    pub updated_input: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct HookExecution {
    pub execution_id: String,
    pub id: String,
    pub name: String,
    pub event: HookEvent,
    pub result: ToolResult,
    pub decision: HookDecision,
}

#[derive(Clone, Debug)]
pub enum HookRunUpdate {
    Started {
        execution_id: String,
        hook: HookDefinition,
    },
    Completed(HookExecution),
}

#[derive(Clone, Debug)]
pub struct HookBudget {
    remaining: Duration,
}

/// Result returned by the local hook command backend.
#[derive(Clone, Debug)]
pub struct HookCommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub type HookCommandRunner = Arc<
    dyn Fn(&HookDefinition, &Value, Duration) -> Result<HookCommandOutput, String> + Send + Sync,
>;

impl Default for HookBudget {
    fn default() -> Self {
        Self {
            remaining: RUN_HOOK_BUDGET,
        }
    }
}

#[cfg(test)]
pub fn execute_event(
    workspace: &Path,
    hooks: &[HookDefinition],
    event: HookEvent,
    matcher_value: Option<&str>,
    input: &Value,
    budget: &mut HookBudget,
    observer: &(dyn Fn(HookRunUpdate) -> Result<(), String> + Send + Sync),
) -> Result<Vec<HookExecution>, String> {
    execute_event_with_environment(
        workspace,
        hooks,
        event,
        matcher_value,
        input,
        budget,
        &[],
        observer,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn execute_event_with_environment(
    workspace: &Path,
    hooks: &[HookDefinition],
    event: HookEvent,
    matcher_value: Option<&str>,
    input: &Value,
    budget: &mut HookBudget,
    environment: &[(OsString, OsString)],
    observer: &(dyn Fn(HookRunUpdate) -> Result<(), String> + Send + Sync),
) -> Result<Vec<HookExecution>, String> {
    let runner = command_runner_with_environment(workspace, environment, CancelSignal::default());
    execute_event_with_runner(hooks, event, matcher_value, input, budget, runner, observer)
}

/// Builds the same local-shell runner used by the synchronous lifecycle
/// pipeline so observability-only hooks do not acquire a second execution
/// implementation.
///
/// `cancellation` is constructed by the dispatch point for this execution's
/// owner (`RunModelRequest::round_cancellation`). It combines either the task's
/// signal or the top-level run's signal. Any configured source terminates a
/// waiting hook command instead of allowing its full timeout budget; an empty
/// signal permits the full budget.
pub(crate) fn command_runner_with_environment(
    workspace: &Path,
    environment: &[(OsString, OsString)],
    cancellation: CancelSignal,
) -> HookCommandRunner {
    let workspace = workspace.to_path_buf();
    let environment = environment.to_vec();
    Arc::new(move |hook, input, remaining_budget| {
        run_command(
            &workspace,
            hook,
            input,
            remaining_budget,
            &environment,
            &cancellation,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub fn execute_event_with_runner(
    hooks: &[HookDefinition],
    event: HookEvent,
    matcher_value: Option<&str>,
    input: &Value,
    budget: &mut HookBudget,
    runner: HookCommandRunner,
    observer: &(dyn Fn(HookRunUpdate) -> Result<(), String> + Send + Sync),
) -> Result<Vec<HookExecution>, String> {
    let matching = hooks
        .iter()
        .filter(|hook| {
            hook.enabled
                && hook.event == event
                && hook_matches(event, hook.matcher.as_deref(), matcher_value)
        })
        .cloned()
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(Vec::new());
    }
    if budget.remaining.is_zero() {
        return matching
            .into_iter()
            .map(|hook| budget_exhausted(hook, observer))
            .collect::<Result<Vec<_>, _>>();
    }

    let started = Instant::now();
    let input = input.clone();
    let remaining = budget.remaining;
    let mut workers = Vec::with_capacity(matching.len());
    for hook in matching {
        let execution_id = Uuid::new_v4().to_string();
        observer(HookRunUpdate::Started {
            execution_id: execution_id.clone(),
            hook: hook.clone(),
        })?;
        let input = input.clone();
        let runner = runner.clone();
        workers.push(thread::spawn(move || {
            execute_one(hook, execution_id, input, remaining, runner)
        }));
    }

    let mut executions = Vec::with_capacity(workers.len());
    for worker in workers {
        let execution = worker
            .join()
            .map_err(|_| "Hook execution thread terminated unexpectedly".to_owned())?;
        observer(HookRunUpdate::Completed(execution.clone()))?;
        executions.push(execution);
    }
    budget.remaining = budget.remaining.saturating_sub(started.elapsed());
    Ok(executions)
}

pub fn event_label(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "SessionStart",
        HookEvent::InstructionsLoaded => "InstructionsLoaded",
        HookEvent::UserPromptSubmit => "UserPromptSubmit",
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PermissionRequest => "PermissionRequest",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::Stop => "Stop",
    }
}

fn hook_matches(event: HookEvent, matcher: Option<&str>, value: Option<&str>) -> bool {
    if matches!(event, HookEvent::UserPromptSubmit | HookEvent::Stop) {
        return true;
    }
    let matcher = matcher.map(str::trim).unwrap_or("");
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    let Some(value) = value else {
        return false;
    };
    Regex::new(matcher).is_ok_and(|pattern| pattern.is_match(value))
}

fn budget_exhausted(
    hook: HookDefinition,
    observer: &(dyn Fn(HookRunUpdate) -> Result<(), String> + Send + Sync),
) -> Result<HookExecution, String> {
    let execution_id = Uuid::new_v4().to_string();
    observer(HookRunUpdate::Started {
        execution_id: execution_id.clone(),
        hook: hook.clone(),
    })?;
    let execution = HookExecution {
        execution_id,
        id: hook.id,
        name: hook.name,
        event: hook.event,
        result: ToolResult {
            success: false,
            output: "The cumulative hook execution time for this turn reached the 5-minute limit".into(),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: 0,
        },
        decision: HookDecision::default(),
    };
    observer(HookRunUpdate::Completed(execution.clone()))?;
    Ok(execution)
}

fn execute_one(
    hook: HookDefinition,
    execution_id: String,
    input: Value,
    remaining_budget: Duration,
    runner: HookCommandRunner,
) -> HookExecution {
    let started = Instant::now();
    let outcome = runner(&hook, &input, remaining_budget);
    let (success, output, decision) = match outcome {
        Ok(command) => interpret_output(hook.event, command),
        Err(error) => (false, error, HookDecision::default()),
    };
    HookExecution {
        execution_id,
        id: hook.id,
        name: hook.name,
        event: hook.event,
        result: ToolResult {
            success,
            output: truncate_output(&output),
            images: Vec::new(),
            diff: None,
            executed_at: Utc::now().to_rfc3339(),
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        },
        decision,
    }
}

fn run_command(
    workspace: &Path,
    hook: &HookDefinition,
    input: &Value,
    remaining_budget: Duration,
    environment: &[(OsString, OsString)],
    cancellation: &CancelSignal,
) -> Result<HookCommandOutput, String> {
    // A cancellation between scheduling and spawning must prevent the command
    // from starting. Polling after spawn covers the remaining spawn race.
    if cancellation.cancelled() {
        return Err("The run or task was stopped; the hook command was not started".into());
    }
    let workspace = fs::canonicalize(workspace)
        .map_err(|error| format!("Hook workspace does not exist or cannot be accessed: {error}"))?;
    if !workspace.is_dir() {
        return Err("Hook workspace must be a directory".into());
    }
    #[cfg(windows)]
    let command = hook
        .command_windows
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&hook.command);
    #[cfg(not(windows))]
    let command = hook.command.as_str();
    if command.trim().is_empty() {
        return Err("Hook command must not be empty".into());
    }

    #[cfg(windows)]
    let candidates: &[(&str, &[&str])] = &[
        (
            "pwsh",
            &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
        ),
        (
            "powershell",
            &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
        ),
    ];
    #[cfg(not(windows))]
    let candidates: &[(&str, &[&str])] = &[("bash", &["-lc"])];

    let serialized =
        serde_json::to_vec(input).map_err(|error| format!("Could not serialize hook input: {error}"))?;
    // The payload above is UTF-8 and the reply is parsed as UTF-8 JSON, but a
    // redirected Windows PowerShell defaults both console encodings to the OEM
    // code page. Today the two errors cancel out — the hook reads UTF-8 as GBK
    // and writes it back as GBK — so a hook that only echoes its input looks
    // correct while anything it composes itself comes back as mojibake. Pinning
    // all three encodings fixes both directions at once; fixing only the output
    // side would turn that accidental round-trip into visible corruption.
    #[cfg(windows)]
    let spawned_command = crate::powershell_host::wrap_command(command);
    #[cfg(not(windows))]
    let spawned_command = command.to_owned();
    let mut last_not_found = None;
    for (executable, arguments) in candidates {
        let mut process = Command::new(executable);
        process
            .args(*arguments)
            .arg(spawned_command.as_str())
            .current_dir(&workspace)
            .env(HOOK_EVENT_ENV, event_label(hook.event))
            .env(LEGACY_HOOK_EVENT_ENV, event_label(hook.event))
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            process.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }

        // Check again immediately before spawn. Cancellation during path
        // canonicalization, serialization, or shell fallback must prevent launch.
        if cancellation.cancelled() {
            return Err("The run or task was stopped; the hook command was not started".into());
        }

        match process.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take().ok_or("Could not capture hook standard output")?;
                let stderr = child.stderr.take().ok_or("Could not capture hook standard error")?;
                let stdout_collector = collect_pipe(stdout);
                let stderr_collector = collect_pipe(stderr);
                // Write stdin on a detached thread. A hook that never reads a
                // payload larger than the pipe buffer must not block host polling
                // or its timeout. Broken pipes are normal when a hook exits early;
                // the command's exit status and output determine its result.
                if let Some(mut stdin) = child.stdin.take() {
                    let payload = serialized.clone();
                    drop(thread::spawn(move || {
                        let _ = stdin.write_all(&payload);
                    }));
                }
                let timeout = Duration::from_millis(hook.timeout_ms)
                    .min(MAX_HOOK_TIMEOUT)
                    .min(remaining_budget);
                // Poll in slices so either run or task cancellation terminates
                // the hook within one polling interval instead of its full timeout.
                let mut waited = Duration::ZERO;
                let status = loop {
                    let slice = CANCELLATION_POLL.min(timeout.saturating_sub(waited));
                    match child.wait_timeout(slice) {
                        Ok(Some(status)) => break status,
                        Ok(None) => {
                            waited += slice;
                            if cancellation.cancelled() {
                                terminate_process_tree(&mut child);
                                let (stdout, stderr) =
                                    finish_output(stdout_collector, stderr_collector);
                                return Err(with_captured_output(
                                    "The run or task was stopped; the hook command was terminated".into(),
                                    stdout,
                                    stderr,
                                ));
                            }
                            if waited >= timeout {
                                terminate_process_tree(&mut child);
                                let (stdout, stderr) =
                                    finish_output(stdout_collector, stderr_collector);
                                return Err(with_captured_output(
                                    format!("Hook exceeded its {} ms timeout limit", timeout.as_millis()),
                                    stdout,
                                    stderr,
                                ));
                            }
                        }
                        Err(error) => {
                            terminate_process_tree(&mut child);
                            let (stdout, stderr) =
                                finish_output(stdout_collector, stderr_collector);
                            return Err(with_captured_output(
                                format!("Failed while waiting for hook completion: {error}"),
                                stdout,
                                stderr,
                            ));
                        }
                    }
                };
                let (stdout, stderr) = finish_output(stdout_collector, stderr_collector);
                return Ok(HookCommandOutput {
                    code: status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_not_found = Some(error);
            }
            Err(error) => return Err(format!("Could not start hook shell {executable}: {error}")),
        }
    }
    Err(format!(
        "No executable shell was found for the hook{}",
        last_not_found
            .map(|error| format!(": {error}"))
            .unwrap_or_default()
    ))
}

fn interpret_output(event: HookEvent, command: HookCommandOutput) -> (bool, String, HookDecision) {
    let display = format_streams(&command.stdout, &command.stderr, command.code);
    if event == HookEvent::InstructionsLoaded {
        // InstructionsLoaded is observability-only. Even exit 2, universal
        // `continue: false`, and additionalContext must stay local and cannot
        // affect the run or a later provider turn.
        return (command.code == 0, display, HookDecision::default());
    }
    if command.code == 2 {
        let reason = nonempty(&command.stderr).unwrap_or_else(|| "A hook blocked the current action".into());
        return (
            false,
            display,
            HookDecision {
                blocked: true,
                reason: Some(reason),
                permission_decision: Some(HookPermissionDecision::Deny),
                ..HookDecision::default()
            },
        );
    }
    if command.code != 0 {
        return (false, display, HookDecision::default());
    }
    let stdout = command.stdout.trim();
    if stdout.is_empty() {
        return (true, display, HookDecision::default());
    }
    match serde_json::from_str::<Value>(stdout) {
        Ok(value) if value.is_object() => {
            let decision = parse_json_decision(event, &value);
            (true, display, decision)
        }
        _ if matches!(event, HookEvent::SessionStart | HookEvent::UserPromptSubmit) => (
            true,
            display,
            HookDecision {
                additional_context: nonempty(stdout),
                ..HookDecision::default()
            },
        ),
        _ if event == HookEvent::Stop => (
            false,
            format!("Stop hook must return JSON; received non-JSON stdout\n\n{display}"),
            HookDecision::default(),
        ),
        _ => (true, display, HookDecision::default()),
    }
}

fn parse_json_decision(event: HookEvent, value: &Value) -> HookDecision {
    let raw_specific = value.get("hookSpecificOutput").and_then(Value::as_object);
    let specific = raw_specific.filter(|object| {
        object
            .get("hookEventName")
            .and_then(Value::as_str)
            .is_some_and(|name| name == event_label(event))
    });
    let mut decision = HookDecision {
        blocked: value.get("decision").and_then(Value::as_str) == Some("block"),
        halt: value.get("continue").and_then(Value::as_bool) == Some(false),
        reason: value
            .get("reason")
            .or_else(|| value.get("stopReason"))
            .and_then(Value::as_str)
            .and_then(nonempty),
        additional_context: specific
            .and_then(|object| object.get("additionalContext"))
            .and_then(Value::as_str)
            .and_then(nonempty),
        system_message: value
            .get("systemMessage")
            .and_then(Value::as_str)
            .and_then(nonempty),
        ..HookDecision::default()
    };

    if event == HookEvent::PreToolUse && raw_specific.is_none() {
        decision.permission_decision = match value.get("decision").and_then(Value::as_str) {
            Some("approve") => Some(HookPermissionDecision::Allow),
            Some("block") => Some(HookPermissionDecision::Deny),
            _ => None,
        };
    }

    if let Some(permission) = (event == HookEvent::PreToolUse)
        .then(|| {
            specific
                .and_then(|object| object.get("permissionDecision"))
                .and_then(Value::as_str)
        })
        .flatten()
    {
        decision.permission_decision = match permission {
            "allow" => match specific.and_then(|object| object.get("updatedInput")) {
                None => Some(HookPermissionDecision::Allow),
                Some(updated @ Value::Object(_)) => {
                    decision.updated_input = Some(updated.clone());
                    Some(HookPermissionDecision::Allow)
                }
                Some(_) => {
                    decision.system_message =
                        Some("PreToolUse hook updatedInput is not an object; ignored the allow decision".into());
                    None
                }
            },
            "ask" => match specific.and_then(|object| object.get("updatedInput")) {
                None => Some(HookPermissionDecision::Ask),
                Some(updated @ Value::Object(_)) => {
                    decision.updated_input = Some(updated.clone());
                    Some(HookPermissionDecision::Ask)
                }
                Some(_) => {
                    decision.system_message = Some(
                        "PreToolUse hook updatedInput is not an object; ignored the input rewrite but still requires user confirmation"
                            .into(),
                    );
                    Some(HookPermissionDecision::Ask)
                }
            },
            "defer" if event == HookEvent::PreToolUse => {
                // Claude Code only honors `defer` for non-interactive `claude
                // -p` sessions. Mework is an interactive host, so retain the
                // decision for observability but ignore all side effects and
                // continue through the ordinary permission flow.
                decision.additional_context = None;
                decision.system_message = Some(
                    "PreToolUse hook returned defer; Mework ignores it for interactive runs and continues through the ordinary permission flow"
                        .into(),
                );
                Some(HookPermissionDecision::Defer)
            }
            "deny" => {
                decision.blocked = true;
                Some(HookPermissionDecision::Deny)
            }
            _ => None,
        };
        if decision.permission_decision != Some(HookPermissionDecision::Defer) {
            decision.reason = decision.reason.or_else(|| {
                specific
                    .and_then(|object| object.get("permissionDecisionReason"))
                    .and_then(Value::as_str)
                    .and_then(nonempty)
            });
        }
    }

    if let Some(permission) = (event == HookEvent::PermissionRequest)
        .then(|| {
            specific
                .and_then(|object| object.get("decision"))
                .and_then(Value::as_object)
        })
        .flatten()
    {
        match permission.get("behavior").and_then(Value::as_str) {
            Some("allow") => {
                let updated_input = permission.get("updatedInput");
                let updated_permissions = permission.get("updatedPermissions");
                let input_valid =
                    updated_input.is_none() || updated_input.is_some_and(Value::is_object);
                let permissions_valid = updated_permissions.is_none()
                    || updated_permissions.is_some_and(permission_updates_shape_valid);
                if input_valid && permissions_valid {
                    decision.permission_decision = Some(HookPermissionDecision::Allow);
                    decision.updated_input = updated_input.cloned();
                    if updated_permissions.is_some() {
                        decision.system_message = Some(
                            "PermissionRequest hook returned updatedPermissions; Mework permits only this call and will not persist hook output as permissions"
                                .into(),
                        );
                    }
                } else {
                    decision.system_message = Some(
                        "PermissionRequest hook has an invalid allow field type; fell back to the ordinary permission flow".into(),
                    );
                }
            }
            Some("deny") => {
                decision.blocked = true;
                decision.interrupt = permission
                    .get("interrupt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                decision.permission_decision = Some(HookPermissionDecision::Deny);
                decision.reason = decision.reason.or_else(|| {
                    permission
                        .get("message")
                        .and_then(Value::as_str)
                        .and_then(nonempty)
                });
            }
            _ => {}
        }
    }
    if decision.blocked && decision.reason.is_none() {
        decision.reason = Some("A hook blocked the current action".into());
    }
    decision
}

fn permission_updates_shape_valid(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    entries.iter().all(|entry| {
        let Some(entry) = entry.as_object() else {
            return false;
        };
        let destination_valid = entry
            .get("destination")
            .and_then(Value::as_str)
            .is_some_and(|destination| {
                matches!(
                    destination,
                    "session" | "localSettings" | "projectSettings" | "userSettings"
                )
            });
        if !destination_valid {
            return false;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("addRules" | "replaceRules" | "removeRules") => {
                entry
                    .get("behavior")
                    .and_then(Value::as_str)
                    .is_some_and(|behavior| matches!(behavior, "allow" | "ask" | "deny"))
                    && entry
                        .get("rules")
                        .and_then(Value::as_array)
                        .is_some_and(|rules| {
                            rules.iter().all(|rule| {
                                rule.as_object().is_some_and(|rule| {
                                    rule.get("toolName")
                                        .and_then(Value::as_str)
                                        .is_some_and(|name| !name.is_empty())
                                        && rule.get("ruleContent").map_or(true, Value::is_string)
                                })
                            })
                        })
            }
            Some("setMode") => entry
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| {
                    matches!(
                        mode,
                        "default"
                            | "manual"
                            | "auto"
                            | "acceptEdits"
                            | "dontAsk"
                            | "bypassPermissions"
                            | "plan"
                    )
                }),
            Some("addDirectories" | "removeDirectories") => entry
                .get("directories")
                .and_then(Value::as_array)
                .is_some_and(|directories| directories.iter().all(Value::is_string)),
            _ => false,
        }
    })
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn format_streams(stdout: &str, stderr: &str, code: i32) -> String {
    let mut output = String::new();
    if !stdout.trim().is_empty() {
        output.push_str(stdout);
    }
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push_str("\n[stderr]\n");
        }
        output.push_str(stderr);
    }
    if output.trim().is_empty() {
        format!("Command finished with exit code {code}")
    } else {
        output
    }
}

#[derive(Clone, Debug, Default)]
struct PipeCapture {
    bytes: Vec<u8>,
    truncated: bool,
    error: Option<String>,
}

struct PipeCollector {
    capture: Arc<Mutex<PipeCapture>>,
    done: Receiver<()>,
}

impl PipeCollector {
    fn snapshot_until(self, deadline: Instant) -> (PipeCapture, bool) {
        let completed = match self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        let capture = self
            .capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        (capture, completed)
    }
}

fn collect_pipe<R: Read + Send + 'static>(mut pipe: R) -> PipeCollector {
    let capture = Arc::new(Mutex::new(PipeCapture::default()));
    let thread_capture = Arc::clone(&capture);
    let (done_tx, done) = mpsc::sync_channel(1);
    drop(thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let mut captured = thread_capture
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let remaining = MAX_HOOK_OUTPUT.saturating_sub(captured.bytes.len());
                    let keep = remaining.min(read);
                    captured.bytes.extend_from_slice(&buffer[..keep]);
                    captured.truncated |= keep < read;
                }
                Err(error) => {
                    thread_capture
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .error = Some(error.to_string());
                    break;
                }
            }
        }
        let _ = done_tx.send(());
    }));
    PipeCollector { capture, done }
}

fn finish_output(stdout: PipeCollector, stderr: PipeCollector) -> (String, String) {
    let deadline = Instant::now() + PIPE_DRAIN_GRACE;
    let (stdout, stdout_complete) = stdout.snapshot_until(deadline);
    let (stderr, stderr_complete) = stderr.snapshot_until(deadline);
    (
        render_capture(stdout, stdout_complete, "stdout"),
        render_capture(stderr, stderr_complete, "stderr"),
    )
}

fn render_capture(capture: PipeCapture, complete: bool, stream: &str) -> String {
    let mut output = String::from_utf8_lossy(&capture.bytes).into_owned();
    if capture.truncated {
        output.push_str(&format!("\n… {stream} exceeded the read limit; remaining output was discarded"));
    }
    if let Some(error) = capture.error {
        output.push_str(&format!("\n… failed to read {stream}: {error}"));
    }
    if !complete {
        output.push_str(&format!("\n… {stream} was still open after the process ended; stopped waiting"));
    }
    output
}

fn with_captured_output(message: String, stdout: String, stderr: String) -> String {
    let output = format_streams(&stdout, &stderr, -1);
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        message
    } else {
        truncate_output(&format!("{message}\n\n{output}"))
    }
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let taskkill = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("taskkill.exe");
        if let Ok(mut killer) = Command::new(taskkill)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            match killer.wait_timeout(PROCESS_TREE_KILL_GRACE) {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = killer.kill();
                    let _ = killer.wait();
                }
            }
        }
    }
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn truncate_output(value: &str) -> String {
    if value.len() <= MAX_HOOK_OUTPUT {
        return value.to_owned();
    }
    const NOTICE: &str = "\n… hook output was truncated to 64 KiB";
    let mut end = MAX_HOOK_OUTPUT.saturating_sub(NOTICE.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{NOTICE}", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(event: HookEvent, command: &str) -> HookDefinition {
        HookDefinition {
            id: "test-hook".into(),
            name: "Test hook".into(),
            event,
            matcher: None,
            command: command.into(),
            command_windows: None,
            status_message: None,
            enabled: true,
            timeout_ms: 5_000,
        }
    }

    #[test]
    fn user_prompt_plain_stdout_becomes_context() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "$input | Out-Null; Write-Output 'branch: feature/hooks'";
        #[cfg(not(windows))]
        let command = "cat >/dev/null; printf 'branch: feature/hooks'";
        let executions = execute_event(
            directory.path(),
            &[hook(HookEvent::UserPromptSubmit, command)],
            HookEvent::UserPromptSubmit,
            None,
            &json!({"prompt":"test"}),
            &mut HookBudget::default(),
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            executions[0].decision.additional_context.as_deref(),
            Some("branch: feature/hooks")
        );
    }

    #[test]
    fn custom_environment_is_inherited_by_hook_commands() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "$input | Out-Null; Write-Output $env:MEWORK_TEST_DEP";
        #[cfg(not(windows))]
        let command = "cat >/dev/null; printf '%s' \"$MEWORK_TEST_DEP\"";
        let environment = vec![(
            OsString::from("MEWORK_TEST_DEP"),
            OsString::from("managed-dependency-visible"),
        )];
        let executions = execute_event_with_environment(
            directory.path(),
            &[hook(HookEvent::UserPromptSubmit, command)],
            HookEvent::UserPromptSubmit,
            None,
            &json!({"prompt":"test"}),
            &mut HookBudget::default(),
            &environment,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            executions[0].decision.additional_context.as_deref(),
            Some("managed-dependency-visible")
        );
    }

    #[test]
    fn exit_two_blocks_and_uses_stderr_reason() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "[Console]::Error.Write('blocked by policy'); exit 2";
        #[cfg(not(windows))]
        let command = "printf 'blocked by policy' >&2; exit 2";
        let executions = execute_event(
            directory.path(),
            &[hook(HookEvent::PreToolUse, command)],
            HookEvent::PreToolUse,
            Some("Bash"),
            &json!({"tool_name":"Bash"}),
            &mut HookBudget::default(),
            &|_| Ok(()),
        )
        .unwrap();
        assert!(executions[0].decision.blocked);
        assert_eq!(
            executions[0].decision.reason.as_deref(),
            Some("blocked by policy")
        );
    }

    /// Cancellation must interrupt an already-running hook rather than wait for
    /// its minutes-long budget. The marker barrier proves the command spawned
    /// before the flag was raised, so only post-spawn polling can satisfy it.
    #[test]
    fn a_raised_cancellation_kills_a_running_hook_instead_of_waiting_out_its_timeout() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command =
            "$input | Out-Null; New-Item -ItemType File -Path hook-started | Out-Null; Start-Sleep -Seconds 20";
        #[cfg(not(windows))]
        let command = "cat >/dev/null; touch hook-started; sleep 20";
        let mut definition = hook(HookEvent::PreToolUse, command);
        // This exceeds the assertion window, so completing by timeout fails.
        definition.timeout_ms = 120_000;

        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner = command_runner_with_environment(
            directory.path(),
            &[],
            CancelSignal::from_flag(Arc::clone(&flag)),
        );

        let marker = directory.path().join("hook-started");
        let raiser = thread::spawn(move || {
            // The generous deadline makes the marker a condition rather than a
            // timing race on slow machines.
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if marker.exists() {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                    return true;
                }
                thread::sleep(Duration::from_millis(25));
            }
            false
        });

        let started = Instant::now();
        let executions = execute_event_with_runner(
            &[definition],
            HookEvent::PreToolUse,
            Some("Bash"),
            &json!({"tool_name":"Bash"}),
            &mut HookBudget::default(),
            runner,
            &|_| Ok(()),
        )
        .unwrap();
        assert!(raiser.join().unwrap(), "钩子命令从未跑起来（marker 没出现）");

        assert!(
            started.elapsed() < Duration::from_secs(15),
            "被停止的钩子必须当场收尾，而不是跑满 20 秒：{:?}",
            started.elapsed()
        );
        assert!(!executions[0].result.success);
        assert!(
            executions[0].result.output.contains("The run or task was stopped"),
            "{}",
            executions[0].result.output
        );
        // Cancellation must not be reported as a hook timeout.
        assert!(
            !executions[0].result.output.contains("timeout limit"),
            "{}",
            executions[0].result.output
        );
    }

    /// A command dispatched after cancellation must never start.
    ///
    /// The result must state that it was not started, and the marker must not
    /// appear; this distinguishes the pre-spawn guard from immediate termination
    /// after spawning.
    #[test]
    fn a_hook_dispatched_after_cancellation_never_starts() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "$input | Out-Null; New-Item -ItemType File -Path hook-started | Out-Null";
        #[cfg(not(windows))]
        let command = "cat >/dev/null; touch hook-started";
        let definition = hook(HookEvent::PreToolUse, command);
        let runner = command_runner_with_environment(
            directory.path(),
            &[],
            CancelSignal::from_flag(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        );

        let executions = execute_event_with_runner(
            &[definition],
            HookEvent::PreToolUse,
            Some("Bash"),
            &json!({"tool_name":"Bash"}),
            &mut HookBudget::default(),
            runner,
            &|_| Ok(()),
        )
        .unwrap();
        assert!(!executions[0].result.success);
        assert!(
            executions[0].result.output.contains("hook command was not started"),
            "{}",
            executions[0].result.output
        );
        // Allow marker persistence before checking that no command started.
        thread::sleep(Duration::from_millis(500));
        assert!(
            !directory.path().join("hook-started").exists(),
            "已停止之后派发的钩子命令仍然跑了起来"
        );
    }

    /// A hook that does not read a pipe-buffer-exceeding stdin payload must not
    /// block the host write. The bounded worker-thread test ensures a broken
    /// implementation fails instead of hanging the test suite.
    #[test]
    fn a_hook_that_ignores_a_large_stdin_payload_still_times_out() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "Start-Sleep -Seconds 20";
        #[cfg(not(windows))]
        let command = "sleep 20";
        let mut definition = hook(HookEvent::PreToolUse, command);
        definition.timeout_ms = 1_000;
        let runner =
            command_runner_with_environment(directory.path(), &[], CancelSignal::default());

        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let outcome = execute_event_with_runner(
                &[definition],
                HookEvent::PreToolUse,
                Some("Bash"),
                &json!({"tool_name":"Bash","content":"x".repeat(2 * 1024 * 1024)}),
                &mut HookBudget::default(),
                runner,
                &|_| Ok(()),
            );
            let _ = result_tx.send(outcome);
        });

        let executions = result_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("宿主卡死在钩子 stdin 的阻塞写入上（2 MiB 输入、钩子不读）")
            .unwrap();
        worker.join().unwrap();
        assert!(!executions[0].result.success);
        assert!(
            executions[0].result.output.contains("timeout limit"),
            "{}",
            executions[0].result.output
        );
    }

    /// A hook that does not read stdin still reports its own output.
    ///
    /// Broken-pipe writes are expected when a hook exits or ignores stdin. The
    /// payload exceeds the pipe buffer so the test exercises that behavior.
    #[test]
    fn a_hook_that_never_reads_stdin_still_reports_its_own_output() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let command = "Write-Output 'ignored stdin fine'";
        #[cfg(not(windows))]
        let command = "printf 'ignored stdin fine'";
        let definition = hook(HookEvent::UserPromptSubmit, command);
        let runner =
            command_runner_with_environment(directory.path(), &[], CancelSignal::default());

        let executions = execute_event_with_runner(
            &[definition],
            HookEvent::UserPromptSubmit,
            None,
            &json!({"prompt":"x".repeat(2 * 1024 * 1024)}),
            &mut HookBudget::default(),
            runner,
            &|_| Ok(()),
        )
        .unwrap();
        assert!(
            executions[0].result.success,
            "{}",
            executions[0].result.output
        );
        assert!(
            executions[0]
                .decision
                .additional_context
                .as_deref()
                .is_some_and(|context| context.contains("ignored stdin fine")),
            "钩子自己的输出必须原样进入结果：{:?}",
            executions[0].decision.additional_context
        );
    }

    #[test]
    fn pre_tool_json_can_rewrite_input_and_add_context() {
        let decision = parse_json_decision(
            HookEvent::PreToolUse,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "updatedInput": {"path":"safe.txt"},
                    "additionalContext": "redirected to safe path"
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Allow)
        );
        assert_eq!(decision.updated_input, Some(json!({"path":"safe.txt"})));
        assert_eq!(
            decision.additional_context.as_deref(),
            Some("redirected to safe path")
        );
    }

    #[test]
    fn pre_tool_json_preserves_an_explicit_ask_decision() {
        let decision = parse_json_decision(
            HookEvent::PreToolUse,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "ask",
                    "updatedInput": {"path":"review-first.txt"}
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Ask)
        );
        assert_eq!(
            decision.updated_input,
            Some(json!({"path":"review-first.txt"}))
        );
        assert!(!decision.blocked);
    }

    #[test]
    fn pre_tool_defer_is_observable_but_ignored_in_interactive_runs() {
        let decision = parse_json_decision(
            HookEvent::PreToolUse,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "defer",
                    "permissionDecisionReason": "not shown",
                    "updatedInput": {"path":"must-not-apply.txt"},
                    "additionalContext": "must not reach the model"
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Defer)
        );
        assert_eq!(decision.updated_input, None);
        assert_eq!(decision.additional_context, None);
        assert!(decision
            .system_message
            .as_deref()
            .is_some_and(|message| message.contains("Mework ignores it for interactive runs")));
    }

    #[test]
    fn permission_request_uses_nested_allow_and_updated_input() {
        let decision = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedInput": {"command":"npm test"}
                    }
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Allow)
        );
        assert_eq!(decision.updated_input, Some(json!({"command":"npm test"})));
    }

    #[test]
    fn permission_request_deny_interrupt_is_distinct_from_an_ordinary_denial() {
        let interrupted = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "deny",
                        "message": "stop this turn",
                        "interrupt": true
                    }
                }
            }),
        );
        assert!(interrupted.blocked);
        assert!(interrupted.interrupt);
        assert_eq!(interrupted.reason.as_deref(), Some("stop this turn"));

        let ordinary = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "deny",
                        "message": "deny only"
                    }
                }
            }),
        );
        assert!(ordinary.blocked);
        assert!(!ordinary.interrupt);
    }

    #[test]
    fn malformed_updated_input_never_authorizes_the_original_call() {
        for invalid in [json!("bad"), json!([{"command":"unsafe"}]), Value::Null] {
            let pre_tool = parse_json_decision(
                HookEvent::PreToolUse,
                &json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "allow",
                        "updatedInput": invalid
                    }
                }),
            );
            assert_eq!(pre_tool.permission_decision, None);
            assert_eq!(pre_tool.updated_input, None);

            let permission_request = parse_json_decision(
                HookEvent::PermissionRequest,
                &json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PermissionRequest",
                        "decision": {
                            "behavior": "allow",
                            "updatedInput": invalid
                        }
                    }
                }),
            );
            assert_eq!(permission_request.permission_decision, None);
            assert_eq!(permission_request.updated_input, None);
        }
    }

    #[test]
    fn malformed_pre_tool_ask_keeps_the_restrictive_prompt() {
        let decision = parse_json_decision(
            HookEvent::PreToolUse,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "ask",
                    "updatedInput": "not-an-object"
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Ask)
        );
        assert_eq!(decision.updated_input, None);
        assert!(decision
            .system_message
            .as_deref()
            .is_some_and(|message| message.contains("still requires user confirmation")));
    }

    #[test]
    fn malformed_updated_permissions_never_authorize_the_current_call() {
        for invalid in [
            json!("bad"),
            json!([{"type":"addRules","destination":"session"}]),
            json!([{
                "type":"setMode",
                "destination":"session",
                "mode":"unrestricted"
            }]),
            json!([{
                "type":"addRules",
                "destination":"projectSettings",
                "behavior":"allow",
                "rules":[{"toolName":42}]
            }]),
        ] {
            let decision = parse_json_decision(
                HookEvent::PermissionRequest,
                &json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PermissionRequest",
                        "decision": {
                            "behavior": "allow",
                            "updatedPermissions": invalid
                        }
                    }
                }),
            );
            assert_eq!(decision.permission_decision, None);
        }

        let valid_but_not_persisted = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedPermissions": [{
                            "type":"addRules",
                            "destination":"session",
                            "behavior":"allow",
                            "rules":[{"toolName":"Bash","ruleContent":"npm test"}]
                        }]
                    }
                }
            }),
        );
        assert_eq!(
            valid_but_not_persisted.permission_decision,
            Some(HookPermissionDecision::Allow)
        );
        assert!(valid_but_not_persisted
            .system_message
            .as_deref()
            .is_some_and(|message| message.contains("will not persist hook output as permissions")));
    }

    #[test]
    fn permission_update_shape_accepts_every_documented_operation_and_destination() {
        let decision = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedPermissions": [
                            {
                                "type":"addRules",
                                "destination":"session",
                                "behavior":"allow",
                                "rules":[{"toolName":"Bash","ruleContent":"npm test"}]
                            },
                            {
                                "type":"replaceRules",
                                "destination":"localSettings",
                                "behavior":"ask",
                                "rules":[{"toolName":"Edit"}]
                            },
                            {
                                "type":"removeRules",
                                "destination":"projectSettings",
                                "behavior":"deny",
                                "rules":[{"toolName":"Read","ruleContent":"./generated/**"}]
                            },
                            {
                                "type":"setMode",
                                "destination":"userSettings",
                                "mode":"manual"
                            },
                            {
                                "type":"addDirectories",
                                "destination":"session",
                                "directories":["../shared"]
                            },
                            {
                                "type":"removeDirectories",
                                "destination":"localSettings",
                                "directories":["../legacy"]
                            }
                        ]
                    }
                }
            }),
        );
        assert_eq!(
            decision.permission_decision,
            Some(HookPermissionDecision::Allow)
        );
    }

    #[test]
    fn hook_specific_permission_fields_are_bound_to_the_current_event_schema() {
        let pre_tool_shape_during_permission_request = parse_json_decision(
            HookEvent::PermissionRequest,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "permissionDecision": "allow",
                    "updatedInput": {"command":"must-not-apply"}
                }
            }),
        );
        assert_eq!(
            pre_tool_shape_during_permission_request.permission_decision,
            None
        );
        assert_eq!(pre_tool_shape_during_permission_request.updated_input, None);

        let permission_request_shape_during_pre_tool = parse_json_decision(
            HookEvent::PreToolUse,
            &json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "decision": {
                        "behavior": "allow",
                        "updatedInput": {"command":"must-not-apply"}
                    }
                }
            }),
        );
        assert_eq!(
            permission_request_shape_during_pre_tool.permission_decision,
            None
        );
        assert_eq!(permission_request_shape_during_pre_tool.updated_input, None);
    }

    #[test]
    fn deprecated_pre_tool_approve_is_scoped_to_pre_tool_use() {
        let pre_tool = parse_json_decision(HookEvent::PreToolUse, &json!({"decision":"approve"}));
        assert_eq!(
            pre_tool.permission_decision,
            Some(HookPermissionDecision::Allow)
        );

        let permission_request =
            parse_json_decision(HookEvent::PermissionRequest, &json!({"decision":"approve"}));
        assert_eq!(permission_request.permission_decision, None);
    }

    #[test]
    fn matcher_is_a_regular_expression() {
        let mut matching = hook(HookEvent::PreToolUse, "exit 0");
        matching.matcher = Some("^(read|write)$".into());
        assert!(hook_matches(
            HookEvent::PreToolUse,
            matching.matcher.as_deref(),
            Some("read")
        ));
        assert!(!hook_matches(
            HookEvent::PreToolUse,
            matching.matcher.as_deref(),
            Some("bash")
        ));
    }

    #[test]
    fn instructions_loaded_ignores_exit_and_every_decision_output() {
        for command in [
            HookCommandOutput {
                code: 2,
                stdout: String::new(),
                stderr: "must not block".into(),
            },
            HookCommandOutput {
                code: 0,
                stdout: r#"{"continue":false,"hookSpecificOutput":{"hookEventName":"InstructionsLoaded","additionalContext":"must not reach provider"}}"#.into(),
                stderr: String::new(),
            },
        ] {
            let (_, _, decision) = interpret_output(HookEvent::InstructionsLoaded, command);
            assert!(!decision.blocked);
            assert!(!decision.halt);
            assert_eq!(decision.additional_context, None);
            assert_eq!(decision.system_message, None);
        }
    }
}
