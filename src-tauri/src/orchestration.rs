//! Host-side orchestration and agent tools executed by the model run loop
//! itself: the `agent` group (`agent_spawn`), the task group (`task_wait`,
//! `task_list`), and `ask_user`.
//!
//! They never touch the workspace filesystem. Every non-loop entry point
//! (manual execution, model loop, approval dialog)
//! keeps rejecting these names explicitly.

use serde_json::Value;

use crate::agents::{
    self, AgentEnvelope, AgentLiveStatus, EnvelopeKind, WAIT_DEFAULT_TIMEOUT_SECONDS,
    WAIT_MAX_TIMEOUT_SECONDS, WAIT_MIN_TIMEOUT_SECONDS,
};
use crate::model::{validate_agent_type_name, JsonObject, SubagentRunStatus, MAX_AGENT_TYPE_CHARS};
use crate::prompt_profile::{PromptKey, PromptProfile};
use crate::subagent_schema::{self, Schema};

const MAX_TASK_CHARS: usize = 32 * 1024;
/// Bounds one `tasks` entry. Terminal ids and dev-server ids are free-form, so
/// this is a request-size guard rather than an identity rule.
const MAX_TASK_REF_CHARS: usize = 256;
const MAX_LABEL_CHARS: usize = 80;
const MAX_SUBAGENT_UPDATE_CHARS: usize = 4_000;
const MAX_QUESTION_CHARS: usize = 4_000;
const MAX_QUESTIONS: usize = 4;
/// Claude Code asks for headers of at most 12 characters but does not enforce
/// it; the card truncates long ones. This only bounds runaway input.
const MAX_HEADER_CHARS: usize = 120;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;
const MAX_OPTION_CHARS: usize = 120;
const MAX_OPTION_DESCRIPTION_CHARS: usize = 1_000;
const MAX_OPTION_PREVIEW_CHARS: usize = 16 * 1024;
const MAX_SUBAGENT_OUTPUT: usize = 64 * 1024;
/// Size ceiling for the `agent_spawn` schema document itself. `subagent_schema`
/// bounds node count and depth; this bounds the bytes, because the schema is
/// re-sent to the provider on every child round as the injected tool's
/// `input_schema`.
const MAX_OUTPUT_SCHEMA_BYTES: usize = 32 * 1024;
/// Size ceiling for the value a child hands back through `structured_output`.
/// It rides the `Result` envelope into `format_wait_output` and is persisted on
/// `SubagentRunRecord`, so an unbounded value would grow the document.
pub const MAX_STRUCTURED_OUTPUT_BYTES: usize = 32 * 1024;
/// How much of the structured value `format_wait_output` renders inline. The
/// fenced block is built to fit so `truncate_agent_output` — which cuts on a
/// byte boundary and would happily leave an unterminated ``` fence — does not
/// have to cut inside it.
const MAX_RENDERED_STRUCTURED_OUTPUT_BYTES: usize = 8 * 1024;

/// How much of the parent conversation an `agent_spawn` call forks into the
/// child, after Codex Multi-agent V2's `fork_turns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnContextMode {
    /// The child sees only the task text (default).
    None,
    /// The child starts from a sanitized copy of the conversation history.
    Conversation,
}

pub struct AgentSpawnSpec {
    pub task: String,
    /// The child's address, and the name every task surface lists it under.
    ///
    /// Required, and never synthesized: a host-assigned `a1` is an address the
    /// model did not choose and a title that says nothing about the child. The
    /// model names its own children, exactly as it names the run in `workflow`.
    pub name: String,
    pub label: Option<String>,
    /// Optional host-resolved trusted definition name. The model never
    /// supplies the definition's provider, model, prompt, source, or memory
    /// identity.
    pub agent_type: Option<String>,
    pub context: SpawnContextMode,
    /// Optional forced result shape. Holding a [`Schema`] rather than a raw
    /// `Value` is the point: the type can only be built by
    /// [`subagent_schema::compile`], so everything downstream knows the
    /// validity precheck already ran and cannot be skipped.
    pub output_schema: Option<Schema>,
}

/// One addressable task. The bare form is an agent name so that every existing
/// `a1`-style address keeps working; the other kinds carry an explicit prefix
/// because a terminal id and a dev-server id are free-form strings that could
/// otherwise collide with an agent slug.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskRef {
    /// A subagent — everything the model addresses by its agent name.
    Agent(String),
    Terminal(String),
    /// A dev server, by the `serverId` every preview tool already takes, and — in a
    /// conversation with several workspaces — the workspace it runs in, written
    /// `preview:<serverId>@<workspace>`. A `serverId` is a launch.json name, so two
    /// workspaces can both run a `dev`; the number is what tells them apart.
    ///
    /// The preview *page* is deliberately not an address: it is a view of this
    /// process, it dies with it, and it holds nothing the model can wait for.
    Preview {
        server_id: String,
        workspace: Option<u32>,
    },
    /// A running `bash` / `powershell` tool call, by its registry number.
    Shell(String),
    /// A workflow run, by its run id — the name the model gave it. That name is
    /// also the driver's pool name, so the bare form below resolves to the same
    /// task; the prefixed form is what `task_list` prints.
    Workflow(String),
}

impl TaskRef {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if let Some(id) = raw.strip_prefix("terminal:") {
            let id = id.trim();
            if id.is_empty() || id.len() > MAX_TASK_REF_CHARS {
                return Err("terminal: must be followed by a terminal ID".into());
            }
            return Ok(Self::Terminal(id.to_owned()));
        }
        if let Some(address) = raw.strip_prefix("preview:") {
            let address = address.trim();
            if address.is_empty() || address.len() > MAX_TASK_REF_CHARS {
                return Err("preview: must be followed by a serverId".into());
            }
            // Only the last `@`, and only when a workspace number follows it: the
            // serverId is a launch.json name, which may hold an `@` of its own.
            let (server_id, workspace) = match address.rsplit_once('@') {
                Some((server_id, number)) if !server_id.trim().is_empty() => {
                    match number.parse::<u32>() {
                        Ok(number) if number >= 1 => (server_id.trim(), Some(number)),
                        _ => (address, None),
                    }
                }
                _ => (address, None),
            };
            return Ok(Self::Preview {
                server_id: server_id.to_owned(),
                workspace,
            });
        }
        if let Some(id) = raw.strip_prefix("shell:") {
            let id = id.trim();
            if id.is_empty() || id.len() > MAX_TASK_REF_CHARS {
                return Err("shell: must be followed by a command ID".into());
            }
            return Ok(Self::Shell(id.to_owned()));
        }
        if let Some(run_id) = raw.strip_prefix("workflow:") {
            let run_id = run_id.trim();
            if run_id.is_empty() || run_id.len() > MAX_TASK_REF_CHARS {
                return Err("workflow: must be followed by a run ID".into());
            }
            return Ok(Self::Workflow(run_id.to_owned()));
        }
        // Retired addresses receive a specific error rather than agent-name
        // validation, because old timeline entries can still be copied by models.
        if raw.starts_with("web_search:") {
            return Err(
                "web_search is no longer a task: its result is in the receipt of that web_search call, and it has no address to wait for"
                    .into(),
            );
        }
        if raw.starts_with("browser:") {
            return Err(
                "a browser page is no longer a task: the dev server behind it is, and its address is preview:<serverId> from preview_list"
                    .into(),
            );
        }
        agents::validate_agent_name(raw)?;
        Ok(Self::Agent(raw.to_owned()))
    }

    /// The exact string `task_list` prints and `task_wait` accepts back.
    pub fn wire(&self) -> String {
        match self {
            Self::Agent(name) => name.clone(),
            Self::Terminal(id) => format!("terminal:{id}"),
            Self::Preview {
                server_id,
                workspace: None,
            } => format!("preview:{server_id}"),
            Self::Preview {
                server_id,
                workspace: Some(workspace),
            } => format!("preview:{server_id}@{workspace}"),
            Self::Shell(id) => format!("shell:{id}"),
            Self::Workflow(run_id) => format!("workflow:{run_id}"),
        }
    }
}

pub struct TaskWaitSpec {
    /// Empty means "every subagent of this conversation" — deliberately NOT
    /// every task. A bare `task_wait` that also watched terminals and dev
    /// servers would block on a shell the user is typing into, which is never
    /// what the caller meant.
    pub tasks: Vec<TaskRef>,
    pub timeout_seconds: u64,
}

/// `role_required` comes from the host's `api::role_policy` and determines
/// whether this run must name a role.
///
/// In required mode, reject `context` because that property is absent from the
/// model-facing schema; if supplied, it can only have been recalled from memory.
pub fn parse_agent_spawn(
    input: &JsonObject,
    role_required: bool,
) -> Result<AgentSpawnSpec, String> {
    for host_owned in [
        "provider_id",
        "providerId",
        "model_id",
        "modelId",
        "source",
        "source_key",
        "sourceKey",
        "revision",
        "system_prompt",
        "systemPrompt",
        "assembled_system_prompt",
        "assembledSystemPrompt",
        "memory",
        // Execution overrides. They are capability-bearing — a tool allowlist,
        // a reasoning budget and a round ceiling — so they are resolved from the
        // trusted definition or supplied by host code, never read off model
        // JSON. Rejecting them explicitly (rather than ignoring unknown keys)
        // keeps a model that tries from believing it succeeded.
        "tools",
        "disallowed_tools",
        "disallowedTools",
        "effort",
        "max_rounds",
        "maxRounds",
    ] {
        // `null` or a blank string is the field left out; only a value is an
        // attempt at an override.
        if input
            .get(host_owned)
            .is_some_and(|value| !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty()))
        {
            return Err(format!(
                "{host_owned} is host-resolved from the trusted agent_type definition; agent_spawn does not accept this field"
            ));
        }
    }
    // The model-facing key is `prompt`, matching a `workflow` step. Both hold
    // the child-visible self-contained task body. The Rust field remains `task`
    // to distinguish it from adjacent host runtime state.
    let task = required_string(input, "prompt", MAX_TASK_CHARS)?;
    let name = required_string(input, "name", agents::MAX_AGENT_NAME_CHARS)?;
    agents::validate_agent_name(&name)?;
    let label = optional_string(input, "label", MAX_LABEL_CHARS)?;
    let agent_type = optional_string(input, "agent_type", MAX_AGENT_TYPE_CHARS)?;
    if let Some(agent_type) = &agent_type {
        validate_agent_type_name(agent_type)?;
    }
    if role_required {
        if agent_type.is_none() {
            return Err(
                "agent_type is required: the role determines the child agent's model, and omitting it would silently inherit this conversation's model. See the agent_type enum in this tool's schema for valid values.".into(),
            );
        }
        // Only a fork is incompatible with a role; `none`, the default, says
        // nothing a role-bound spawn does not already do.
        if optional_string(input, "context", 16)?.as_deref() == Some("conversation") {
            return Err(
                "agent_spawn has no context field in this turn: when a role is required, a child agent cannot fork conversation history because a fork inherits this conversation's model and is incompatible with naming a role.".into(),
            );
        }
    }
    let context = match optional_string(input, "context", 16)?.as_deref() {
        None | Some("none") => SpawnContextMode::None,
        Some("conversation") => SpawnContextMode::Conversation,
        Some(other) => {
            return Err(format!(
                "invalid context: {other}; expected none or conversation"
            ))
        }
    };
    if agent_type.is_some() && context == SpawnContextMode::Conversation {
        return Err(
            "agent_type cannot be combined with context=conversation; a named child agent must start from its trusted definition".into(),
        );
    }
    let output_schema = parse_output_schema(input)?;
    Ok(AgentSpawnSpec {
        task,
        name,
        label,
        agent_type,
        context,
        output_schema,
    })
}

/// Reads and prechecks the model-facing `schema` argument.
///
/// The wire key is `schema`, matching the `schema` field on a `workflow` step:
/// both name the same thing (a forced result shape compiled by the same
/// `subagent_schema` compiler), so they must not differ only because one lives
/// on `agent_spawn` and the other on a plan step. The Rust field stays
/// `output_schema` because internally it sits next to `structured_output` and
/// the run record, where "output" is the distinguishing word.
///
/// The whole schema-validity check runs HERE, at the spawning parent's call,
/// not later when the child tries to answer. An unsupported keyword is the
/// parent's mistake, and the parent is the only party that can fix it — surfacing
/// it as an `agent_spawn` argument error puts the error where the fix is. It
/// also means a spawned child's schema is known-good, so a failed
/// `structured_output` call can only ever mean the child's value was wrong.
///
/// Explicit JSON `null` reads as absent, matching `optional_string`, which is
/// what every other optional field of `agent_spawn` uses.
fn parse_output_schema(input: &JsonObject) -> Result<Option<Schema>, String> {
    let value = match input.get("schema") {
        None | Some(Value::Null) => return Ok(None),
        // An empty object constrains nothing: it is no schema, not a broken one.
        Some(Value::Object(schema)) if schema.is_empty() => return Ok(None),
        Some(value) => value,
    };
    compile_output_schema(value).map(Some)
}

/// Byte-caps and compiles one `schema` document.
///
/// Shared by the spawn-time parse above and by cross-turn rehydration in
/// `api::build_rehydrated_agent_template`. The persisted conversation document
/// is renderer-writable, so a schema read back from a `SubagentRunRecord` must
/// pass exactly the byte / node / depth / keyword bounds a freshly spawned one
/// does — one shared entry point keeps those two paths incapable of drifting.
pub fn compile_output_schema(value: &Value) -> Result<Schema, String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("could not encode schema argument: {error}"))?;
    if encoded.len() > MAX_OUTPUT_SCHEMA_BYTES {
        return Err(format!(
            "schema must not exceed {MAX_OUTPUT_SCHEMA_BYTES} bytes; got {} bytes",
            encoded.len()
        ));
    }
    subagent_schema::compile(value)
}

/// Validates one child `structured_output` call against the spawn-time schema.
///
/// Separate from a `run_*` sibling on purpose: unlike every other orchestration
/// tool, this one cannot be judged from its input alone — the schema lives on
/// the run, not in the call — so the run loop passes it in.
pub fn parse_structured_output(input: &JsonObject, schema: &Schema) -> Result<Value, String> {
    let value = Value::Object(input.clone());
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| format!("could not encode structured output: {error}"))?;
    if encoded.len() > MAX_STRUCTURED_OUTPUT_BYTES {
        return Err(format!(
            "structured output must not exceed {MAX_STRUCTURED_OUTPUT_BYTES} bytes; got {} bytes; return only the fields required by the schema",
            encoded.len()
        ));
    }
    schema
        .validate(&value)
        .map_err(|errors| Schema::validation_message(&errors))?;
    Ok(value)
}

/// Result text for a run that owed a structured result and finished without
/// one. Distinct from an ordinary failure on purpose: the parent has to be able
/// to tell "the child failed" from "the child answered in the wrong shape".
///
/// The child's own text is kept below the notice rather than discarded — it is
/// usually the answer, just unusable as data, and it is the only diagnostic
/// available for why the run never called the tool.
pub fn missing_structured_output_output(final_text: &str, profile: &PromptProfile) -> String {
    let trimmed = final_text.trim();
    let notice = profile.text(PromptKey::SubagentMissingStructuredOutput);
    if trimmed.is_empty() {
        notice.to_owned()
    } else {
        truncate_agent_output(&format!("{notice}\n\n{trimmed}"), profile)
    }
}

/// The fenced block `format_wait_output` appends for a validated structured
/// result, and the same body is folded into the parent's timeline (S9).
pub fn format_structured_output(value: &Value, profile: &PromptProfile) -> String {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| {
        profile
            .text(PromptKey::SubagentStructuredUnserializable)
            .to_owned()
    });
    // Cut here rather than letting `truncate_agent_output` do it: that cuts on a
    // byte boundary and would leave the fence unterminated.
    let body = if rendered.len() > MAX_RENDERED_STRUCTURED_OUTPUT_BYTES {
        let mut end = MAX_RENDERED_STRUCTURED_OUTPUT_BYTES;
        while end > 0 && !rendered.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n{}",
            &rendered[..end],
            profile.text(PromptKey::SubagentStructuredTruncated)
        )
    } else {
        rendered
    };
    profile.render(PromptKey::SubagentStructuredResultBlock, &[("body", &body)])
}

pub fn parse_task_wait(input: &JsonObject) -> Result<TaskWaitSpec, String> {
    let tasks_filter = match input.get("tasks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => {
            // Bounded by `MAX_WAIT_AGENT_NAMES`, not by the live cap: this is an
            // argument-size guard, and a pool may legitimately run more agents
            // than the default cap.
            if values.len() > agents::MAX_WAIT_AGENT_NAMES {
                return Err(format!(
                    "tasks must not contain more than {} items",
                    agents::MAX_WAIT_AGENT_NAMES
                ));
            }
            let mut refs: Vec<TaskRef> = Vec::with_capacity(values.len());
            for value in values {
                let raw = value
                    .as_str()
                    .map(str::trim)
                    .ok_or_else(|| "each tasks item must be a string".to_owned())?;
                // A blank entry names no task; it is skipped, not refused.
                if raw.is_empty() {
                    continue;
                }
                let parsed = TaskRef::parse(raw)?;
                if !refs.contains(&parsed) {
                    refs.push(parsed);
                }
            }
            refs
        }
        Some(_) => return Err("tasks must be an array of strings".into()),
    };
    // A wait is read for what it plainly asks: zero or less is no timeout given
    // (the default), a fraction rounds, and past either end is held to it.
    let timeout_seconds = match input.get("timeout_seconds") {
        None | Some(Value::Null) => WAIT_DEFAULT_TIMEOUT_SECONDS,
        Some(value) => {
            let seconds = value
                .as_f64()
                .filter(|seconds| seconds.is_finite())
                .ok_or_else(|| "timeout_seconds must be a number of seconds".to_owned())?;
            if seconds <= 0.0 {
                WAIT_DEFAULT_TIMEOUT_SECONDS
            } else {
                (seconds.round() as u64).clamp(WAIT_MIN_TIMEOUT_SECONDS, WAIT_MAX_TIMEOUT_SECONDS)
            }
        }
    };
    Ok(TaskWaitSpec {
        tasks: tasks_filter,
        timeout_seconds,
    })
}

pub fn persisted_status_key(status: SubagentRunStatus) -> PromptKey {
    match status {
        SubagentRunStatus::Completed => PromptKey::TaskStatusCompleted,
        SubagentRunStatus::Interrupted => PromptKey::TaskStatusInterrupted,
        SubagentRunStatus::Failed => PromptKey::TaskStatusFailed,
        SubagentRunStatus::Stopped => PromptKey::TaskStatusStopped,
        SubagentRunStatus::RoundLimit => PromptKey::TaskStatusRoundLimit,
    }
}

/// A dispatch receipt that carries no address, because the caller already knows it.
pub const SPAWN_ACK: &str = "ok";

/// The parent-facing tool output of `agent_spawn`.
///
/// The receipt carries only host-minted addresses. `agent_spawn.name` is
/// model-supplied and required by the schema, so echoing it adds no information.
/// Dispatch semantics belong in the `agent_spawn` schema description.
pub fn agent_spawn_output(_name: &str) -> String {
    SPAWN_ACK.to_owned()
}

/// The parent-facing tool output of the `workflow` dispatch.
///
/// A run's id is the name the model gave it, so the ordinary receipt is a bare
/// acknowledgement: echoing an address the caller just typed costs tokens and
/// teaches it nothing. The address is spelled out only when the host could not
/// use the submitted name — a reserved name resolves to a numbered variant, and
/// then the id really is news. A resume likewise echoes, because its id is the
/// run being resumed rather than the name of this dispatch.
pub fn workflow_spawn_output(run_id: &str, submitted_name: &str) -> String {
    if run_id == submitted_name {
        return SPAWN_ACK.to_owned();
    }
    TaskRef::Workflow(run_id.to_owned()).wire()
}

/// The parent-facing output of a background shell dispatch contains only its
/// host-minted task address. Lifecycle and collection behavior are described by
/// the `run_in_background` property.
pub fn shell_background_spawn_output(_tool_name: &str, shell_ref: &str) -> String {
    shell_ref.to_owned()
}

/// The receipt for a command that ran out of foreground time and was adopted by
/// a task slot rather than killed.
///
/// Unlike an explicitly backgrounded command, this one is a surprise: the model
/// asked for a result and is getting an address instead. So the receipt says why
/// the swap happened, not just where the command went.
pub fn shell_timeout_backgrounded_output(
    _tool_name: &str,
    shell_ref: &str,
    timeout: std::time::Duration,
    profile: &PromptProfile,
) -> String {
    profile.render(
        PromptKey::TaskShellTimeoutBackgrounded,
        &[
            ("shell_ref", shell_ref),
            ("seconds", &timeout.as_secs().max(1).to_string()),
        ],
    )
}

/// The envelope body of a finished background shell command. The first line
/// restates the address and exit code because the envelope is rendered under
/// an `[aN · status]` header whose pool name means nothing to the model.
pub fn shell_background_result_text(
    tool_name: &str,
    shell_ref: &str,
    exit_code: Option<i32>,
    output: &str,
    profile: &PromptProfile,
) -> String {
    let exit = match exit_code {
        Some(code) => profile.render(PromptKey::TaskShellExitCode, &[("code", &code.to_string())]),
        None => profile.text(PromptKey::TaskShellExitUnknown).to_owned(),
    };
    let body = if output.trim().is_empty() {
        profile.text(PromptKey::TaskShellNoOutput)
    } else {
        output.trim_end()
    };
    profile.render(
        PromptKey::TaskShellResult,
        &[
            ("shell_ref", shell_ref),
            ("tool_name", tool_name),
            ("exit", &exit),
            ("body", body),
        ],
    )
}

/// Appends the profile's "the user closed this task" sentence to a task result.
///
/// Applied at settlement, not at projection: the folded notification is rendered
/// from the already-minted card body by `wire_history`, which has no profile, so
/// the sentence has to be part of the envelope before the card is minted. It is
/// keyed on who asked for the stop rather than on the terminal status, because
/// `Stopped` also covers host-forced settlement and provider-declared stops.
pub fn append_user_close_note(body: &str, profile: &PromptProfile) -> String {
    let note = profile.text(PromptKey::TaskStoppedByUser);
    if note.trim().is_empty() {
        return body.to_owned();
    }
    if body.trim().is_empty() {
        return note.to_owned();
    }
    format!("{}\n{note}", body.trim_end())
}

/// The envelope body of a background shell command the user stopped. Same shape
/// as a natural exit and carrying the same collected output — a stopped command
/// still produced whatever it printed before it was killed, and throwing that
/// away is the one thing the model cannot recover.
pub fn shell_background_stopped_text(
    tool_name: &str,
    shell_ref: &str,
    output: &str,
    profile: &PromptProfile,
) -> String {
    let body = if output.trim().is_empty() {
        profile.text(PromptKey::TaskShellNoOutput)
    } else {
        output.trim_end()
    };
    profile.render(
        PromptKey::TaskShellStoppedByUser,
        &[
            ("shell_ref", shell_ref),
            ("tool_name", tool_name),
            ("body", body),
        ],
    )
}

/// One `task_wait` result section per drained envelope, bounded overall.
/// One-line cost footer for a finished child turn. Returns `None` when the host
/// recorded nothing measurable, so a legacy or preflight-failed envelope does
/// not render an all-zero line that reads like a real measurement.
fn agent_turn_cost_line(
    metrics: Option<&agents::AgentTurnMetrics>,
    profile: &PromptProfile,
) -> Option<String> {
    let metrics = metrics?;
    let tokens = metrics.usage.total_tokens.or_else(|| {
        match (metrics.usage.input_tokens, metrics.usage.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        }
    });
    if tokens.is_none() && metrics.tool_use_count == 0 && metrics.duration_ms == 0 {
        return None;
    }
    let tokens = tokens
        .map(|value| value.to_string())
        .unwrap_or_else(|| profile.text(PromptKey::TaskCostUnknownTokens).to_owned());
    let tool_uses = metrics.tool_use_count.to_string();
    let duration_ms = metrics.duration_ms.to_string();
    Some(profile.render(
        PromptKey::TaskCostLine,
        &[
            ("tokens", &tokens),
            ("tool_uses", &tool_uses),
            ("duration_ms", &duration_ms),
        ],
    ))
}

/// A non-agent task's state as observed when the wait returned. Terminals and
/// dev servers produce no envelopes — nothing streams from them to the
/// parent — so `task_wait` reports where they stand instead.
pub struct TaskObservation {
    /// Wire address, exactly as `task_list` prints it.
    pub task: String,
    pub label: String,
    pub status_label: String,
    pub detail: Option<String>,
    /// Whether the wait's settled predicate held for this task when it
    /// returned. Only the timeout paragraph reads it: a wait that names both a
    /// child agent and a terminal has to say which is still going; a status
    /// label alone is not a machine-checkable answer.
    pub settled: bool,
}

pub fn format_wait_output(
    envelopes: &[AgentEnvelope],
    statuses: &[(String, AgentLiveStatus)],
    observations: &[TaskObservation],
    timed_out: bool,
    timeout_seconds: u64,
    profile: &PromptProfile,
) -> String {
    let mut output = String::new();
    let results = envelopes
        .iter()
        .filter(|envelope| matches!(envelope.kind, EnvelopeKind::Result(_)))
        .collect::<Vec<_>>();
    if timed_out {
        // Timeout is a state statement, not a failure: tasks continue running.
        // Delivered results and expiry may both be true, so list their addresses
        // separately from the still-pending ones.
        let delivered = results
            .iter()
            .map(|envelope| envelope.agent.clone())
            .collect::<Vec<_>>();
        let pending = statuses
            .iter()
            .filter(|(name, _)| !delivered.iter().any(|done| done == name))
            .map(|(name, _)| name.clone())
            .chain(
                observations
                    .iter()
                    .filter(|observation| !observation.settled)
                    .map(|observation| observation.task.clone()),
            )
            .collect::<Vec<_>>();
        let pending_text = if pending.is_empty() {
            profile.text(PromptKey::TaskWaitPendingFallback).to_owned()
        } else {
            profile.join_list(&pending)
        };
        let seconds = timeout_seconds.to_string();
        let max_seconds = WAIT_MAX_TIMEOUT_SECONDS.to_string();
        let timeout = if delivered.is_empty() {
            profile.render(
                PromptKey::TaskWaitTimeoutAllPending,
                &[
                    ("seconds", &seconds),
                    ("pending", &pending_text),
                    ("max_seconds", &max_seconds),
                ],
            )
        } else {
            let delivered = profile.join_list(&delivered);
            profile.render(
                PromptKey::TaskWaitTimeoutPartial,
                &[
                    ("seconds", &seconds),
                    ("delivered", &delivered),
                    ("pending", &pending_text),
                    ("max_seconds", &max_seconds),
                ],
            )
        };
        output.push_str(&timeout);
    } else if envelopes.is_empty() && observations.is_empty() {
        output.push_str(profile.text(PromptKey::TaskWaitIdle));
    }
    // Render terminal results before progress updates. Accumulated updates can
    // exhaust the 64 KiB output budget and otherwise hide the result that ended
    // the wait. Divide the budget across results first; the global cap is only
    // a final fallback.
    let per_result_budget = MAX_SUBAGENT_OUTPUT / results.len().max(1);
    for envelope in results.iter().copied().chain(
        envelopes
            .iter()
            .filter(|envelope| envelope.kind == EnvelopeKind::Update),
    ) {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        match envelope.kind {
            EnvelopeKind::Update => {
                output.push_str(&format!(
                    "[{} · {}]\n{}",
                    envelope.agent,
                    profile.text(PromptKey::TaskProgressUpdateLabel),
                    envelope.content
                ));
            }
            EnvelopeKind::Result(status) => {
                output.push_str(&truncate_to(
                    &format_result_envelope(envelope, status, profile),
                    per_result_budget,
                    profile,
                ));
            }
        }
    }
    for observation in observations {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!(
            "[{} · {}]",
            observation.task, observation.status_label
        ));
        if !observation.label.is_empty() && observation.label != observation.task {
            output.push_str(&profile.render(
                PromptKey::TaskListRowLabel,
                &[("label", &observation.label)],
            ));
        }
        if let Some(detail) = &observation.detail {
            let preview = compact_preview(detail, 200);
            if !preview.is_empty() {
                output.push('\n');
                output.push_str(&preview);
            }
        }
    }
    if !statuses.is_empty() {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(profile.text(PromptKey::TaskWaitStatusHeading));
        output.push_str(
            &profile.join_list(
                statuses
                    .iter()
                    .map(|(name, status)| format!("{name} {}", profile.text(status.prompt_key()))),
            ),
        );
    }
    truncate_agent_output(&output, profile)
}

/// One `[name · status]` result section **without** the cost footer: header,
/// body, optional structured fence. Shared by `task_wait` output and the fold
/// required by S9, so the two renderings of the same envelope cannot drift.
///
/// Cost rendering belongs to the caller: `task_wait` uses one prose line while
/// background notifications use structured `<usage>`, so the same measurements
/// do not appear twice in one text body.
fn format_result_envelope_body(
    envelope: &AgentEnvelope,
    status: SubagentRunStatus,
    profile: &PromptProfile,
) -> String {
    format_result_body(
        &envelope.agent,
        status,
        &envelope.content,
        envelope.structured_output.as_ref(),
        profile,
    )
}

fn format_result_body(
    agent: &str,
    status: SubagentRunStatus,
    content: &str,
    structured_output: Option<&Value>,
    profile: &PromptProfile,
) -> String {
    let content = if content.trim().is_empty() {
        profile.text(PromptKey::TaskNoTextResult)
    } else {
        content.trim()
    };
    let mut output = format!(
        "[{} · {}]\n{}",
        agent,
        profile.text(persisted_status_key(status)),
        content
    );
    if let Some(structured) = structured_output {
        output.push('\n');
        output.push_str(&format_structured_output(structured, profile));
    }
    output
}

/// The notification a recovered final reply is delivered with: the one a fold of the same
/// completed result would have carried, without the cost the previous process took with it.
pub fn format_recovered_result_notification(
    agent: &str,
    content: &str,
    structured_output: Option<&Value>,
    profile: &PromptProfile,
) -> String {
    truncate_agent_output(
        &format_result_body(
            agent,
            SubagentRunStatus::Completed,
            content,
            structured_output,
            profile,
        ),
        profile,
    )
}

/// One `[name · status]` result section: body, optional structured fence,
/// optional cost footer. This is the `task_wait` rendering and it must stay
/// byte-identical across refactors — the split above exists for the fold's
/// structured `<usage>`, not to change what a wait returns.
fn format_result_envelope(
    envelope: &AgentEnvelope,
    status: SubagentRunStatus,
    profile: &PromptProfile,
) -> String {
    let mut output = format_result_envelope_body(envelope, status, profile);
    if let Some(cost) = agent_turn_cost_line(envelope.metrics.as_ref(), profile) {
        output.push('\n');
        output.push_str(&cost);
    }
    output
}

/// The machine-readable status a background-task notification carries. Spelled
/// exactly like the persisted `SubagentRunStatus` wire value so the model reads
/// the same token it would see anywhere else in the protocol.
pub fn task_status_wire(status: SubagentRunStatus) -> &'static str {
    match status {
        SubagentRunStatus::Completed => "completed",
        SubagentRunStatus::Interrupted => "interrupted",
        SubagentRunStatus::Failed => "failed",
        SubagentRunStatus::Stopped => "stopped",
        SubagentRunStatus::RoundLimit => "roundLimit",
    }
}

/// The one-line `<summary>` of a background-task completion. Mirrors upstream
/// Claude Code's `Agent "…" finished` / `… failed` shape, with the task address
/// (`a1` / `shell:3` / `workflow:…` / `web_search:…`) as the subject.
pub fn task_notification_summary(
    task: &str,
    status: SubagentRunStatus,
    profile: &PromptProfile,
) -> String {
    let key = match status {
        SubagentRunStatus::Completed => PromptKey::TaskNotificationCompleted,
        SubagentRunStatus::Failed => PromptKey::TaskNotificationFailed,
        SubagentRunStatus::RoundLimit => PromptKey::TaskNotificationRoundLimit,
        SubagentRunStatus::Interrupted => PromptKey::TaskNotificationInterrupted,
        SubagentRunStatus::Stopped => PromptKey::TaskNotificationStopped,
    };
    profile.render(key, &[("task", task)])
}

/// The `<usage>` triple of a finished child turn: total tokens (when the
/// provider reported any), tool calls, wall clock. `None` when the envelope
/// carries no metrics at all — an absent element beats a row of zeroes.
pub fn task_notification_usage(
    metrics: Option<&agents::AgentTurnMetrics>,
) -> Option<(Option<u64>, usize, u64)> {
    let metrics = metrics?;
    let tokens = metrics.usage.total_tokens.or_else(|| {
        match (metrics.usage.input_tokens, metrics.usage.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        }
    });
    if tokens.is_none() && metrics.tool_use_count == 0 && metrics.duration_ms == 0 {
        return None;
    }
    Some((tokens, metrics.tool_use_count, metrics.duration_ms))
}

/// The host-authored notification folded into the parent timeline for a
/// terminal result that no `task_wait` drained. It shares the same body builder
/// as `task_wait`, so the two renderings cannot drift.
///
/// Host authorship is determined by the `ctx_agent-result_` context-id prefix,
/// not body text. Background notifications omit the prose cost line because
/// they carry the same values in structured `<usage>`.
pub fn format_undrained_result_notification(
    envelope: &AgentEnvelope,
    profile: &PromptProfile,
) -> String {
    let EnvelopeKind::Result(status) = envelope.kind else {
        unreachable!("the fold claims terminal Result envelopes only");
    };
    truncate_agent_output(
        &format_result_envelope_body(envelope, status, profile),
        profile,
    )
}

/// One row of `task_list`.
pub struct TaskListRow {
    /// Wire address `task_wait` accepts back.
    pub task: String,
    pub label: String,
    pub status_label: String,
    /// The agent's task text, the terminal's cwd, or the page URL.
    pub detail: String,
    pub latest_update: Option<String>,
}

/// Section header a group of rows renders under. The groups are fixed rather
/// than derived from the rows so an empty conversation still tells the model
/// which kinds of task exist.
pub struct TaskListGroup {
    pub title: String,
    pub rows: Vec<TaskListRow>,
}

pub fn format_task_list(groups: &[TaskListGroup], profile: &PromptProfile) -> String {
    let total = groups.iter().map(|group| group.rows.len()).sum::<usize>();
    if total == 0 {
        return profile.text(PromptKey::TaskListEmpty).to_owned();
    }
    let total = total.to_string();
    let mut output = profile.render(PromptKey::TaskListTotal, &[("total", &total)]);
    for group in groups {
        if group.rows.is_empty() {
            continue;
        }
        output.push_str(&format!("\n\n{}", group.title));
        for row in &group.rows {
            let detail_preview = compact_preview(&row.detail, 120);
            output.push_str(&format!("\n- {} · {}", row.task, row.status_label));
            if !row.label.is_empty() && row.label != row.task {
                output.push_str(
                    &profile.render(PromptKey::TaskListRowLabel, &[("label", &row.label)]),
                );
            }
            if !detail_preview.is_empty() {
                output.push_str(&format!("\n  {detail_preview}"));
            }
            if let Some(update) = &row.latest_update {
                let update_preview = compact_preview(update, 120);
                if !update_preview.is_empty() {
                    output.push_str(&profile.render(
                        PromptKey::TaskListLatestUpdate,
                        &[("update", &update_preview)],
                    ));
                }
            }
        }
    }
    truncate_agent_output(&output, profile)
}

fn compact_preview(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut preview: String = compact.chars().take(limit.saturating_sub(1)).collect();
    preview.push('…');
    preview
}

/// Bounds every agent tool output like `subagent_result_output`.
pub fn truncate_agent_output(output: &str, profile: &PromptProfile) -> String {
    truncate_to(output, MAX_SUBAGENT_OUTPUT, profile)
}

/// Byte-bounded truncation on a UTF-8 boundary, using the marker from
/// `truncate_agent_output`. A per-result budget keeps one large result from
/// pushing every later result out of a multi-task `task_wait` answer.
fn truncate_to(output: &str, budget: usize, profile: &PromptProfile) -> String {
    if output.len() <= budget {
        return output.to_owned();
    }
    let mut end = budget;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n{}",
        &output[..end],
        profile.text(PromptKey::TaskOutputTruncated)
    )
}

/// A validated `ask_user` call: what the question card shows and what the
/// answers are matched against when the result is worded.
#[derive(Debug)]
pub struct QuestionItemSpec {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOptionSpec>,
    pub multi_select: bool,
}

#[derive(Debug)]
pub struct QuestionOptionSpec {
    pub label: String,
    pub description: String,
    pub preview: Option<String>,
}

#[derive(Debug)]
pub struct QuestionSpec {
    pub questions: Vec<QuestionItemSpec>,
}

fn parse_question_item(value: &Value, index: usize) -> Result<QuestionItemSpec, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("questions item {} must be an object", index + 1))?;
    let question = required_string(object, "question", MAX_QUESTION_CHARS)
        .map_err(|error| format!("questions item {}: {error}", index + 1))?;
    let header = required_string(object, "header", MAX_HEADER_CHARS)
        .map_err(|error| format!("questions item {}: {error}", index + 1))?;
    let options = parse_question_options(object.get("options"))
        .map_err(|error| format!("questions item {}: {error}", index + 1))?;
    // Claude Code's schema defaults a missing `multiSelect` to false.
    let multi_select = match object.get("multiSelect") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            format!(
                "questions item {}: multiSelect must be a boolean",
                index + 1
            )
        })?,
    };
    Ok(QuestionItemSpec {
        question,
        header,
        options,
        multi_select,
    })
}

fn parse_question_options(value: Option<&Value>) -> Result<Vec<QuestionOptionSpec>, String> {
    match value {
        Some(Value::Array(values)) => {
            if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&values.len()) {
                return Err(format!(
                    "options must contain {MIN_OPTIONS} to {MAX_OPTIONS} items"
                ));
            }
            let mut options = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let object = value
                    .as_object()
                    .ok_or_else(|| format!("options item {} must be an object", index + 1))?;
                let label = required_string(object, "label", MAX_OPTION_CHARS)
                    .map_err(|error| format!("options item {}: {error}", index + 1))?;
                let description =
                    required_string(object, "description", MAX_OPTION_DESCRIPTION_CHARS)
                        .map_err(|error| format!("options item {}: {error}", index + 1))?;
                let preview = optional_string(object, "preview", MAX_OPTION_PREVIEW_CHARS)
                    .map_err(|error| format!("options item {}: {error}", index + 1))?;
                options.push(QuestionOptionSpec {
                    label,
                    description,
                    preview,
                });
            }
            Ok(options)
        }
        _ => Err("options must be an array of objects".into()),
    }
}

/// Claude Code's steer for a call with a question that offers fewer than two
/// options, verbatim.
const FEWER_THAN_TWO_OPTIONS: &str = "This call included a question with fewer than 2 options, so it was rejected and the person never saw it. A question with a single option has no decision in it. Do not retry this call and do not invent a filler second option. Instead, state the one path you were going to offer as the approach you are taking, then continue with the task. If this call also contained questions with 2 to 4 options (each with distinct labels), you may re-ask those questions alone in a new call. Ask a question only when the person has at least two genuinely distinct choices.";

pub fn parse_question(input: &JsonObject) -> Result<QuestionSpec, String> {
    if let Some(value) = input.get("questions") {
        let values = value
            .as_array()
            .ok_or_else(|| "questions must be an array of objects".to_owned())?;
        if values.is_empty() {
            return Err("questions must contain at least 1 item".into());
        }
        if values.len() > MAX_QUESTIONS {
            return Err(format!(
                "questions must not contain more than {MAX_QUESTIONS} items"
            ));
        }
        // Claude Code rejects a call with a one-option question before the
        // user sees it, and tells the model not to pad it with a filler.
        let too_few_options = values.iter().any(|value| {
            value
                .get("options")
                .and_then(Value::as_array)
                .is_some_and(|options| options.len() < MIN_OPTIONS)
        });
        let questions = values
            .iter()
            .enumerate()
            .map(|(index, value)| parse_question_item(value, index))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                if too_few_options {
                    format!("{FEWER_THAN_TWO_OPTIONS}\n\n{error}")
                } else {
                    error
                }
            })?;
        let mut seen_questions = std::collections::HashSet::new();
        let unique = questions.iter().all(|question| {
            let mut labels = std::collections::HashSet::new();
            seen_questions.insert(question.question.as_str())
                && question
                    .options
                    .iter()
                    .all(|option| labels.insert(option.label.as_str()))
        });
        if !unique {
            return Err(
                "Question texts must be unique, option labels must be unique within each question"
                    .into(),
            );
        }
        return Ok(QuestionSpec { questions });
    }

    // Legacy persisted/tool calls used top-level question/options. Keep them
    // executable for old streamed calls while every new schema uses the
    // Claude Code questions structure above.
    let question = required_string(input, "question", MAX_QUESTION_CHARS)?;
    let options = match input.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                let label = value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "each options item must be a non-empty string".to_owned())?;
                Ok(QuestionOptionSpec {
                    label: label.to_owned(),
                    description: String::new(),
                    preview: None,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        Some(_) => return Err("options must be an array of strings".into()),
    };
    Ok(QuestionSpec {
        questions: vec![QuestionItemSpec {
            question,
            header: "Question".into(),
            options,
            multi_select: false,
        }],
    })
}

/// Validates a child-only progress update and returns the message that must be
/// forwarded to the parent subagent stream. The tool itself is injected only
/// into child requests and never appears in the persisted global catalog.
pub fn parse_subagent_update(input: &JsonObject) -> Result<String, String> {
    required_string(input, "message", MAX_SUBAGENT_UPDATE_CHARS)
}

pub fn run_subagent_update(input: &JsonObject, profile: &PromptProfile) -> Result<String, String> {
    parse_subagent_update(input)?;
    Ok(profile.text(PromptKey::SubagentUpdateAck).to_owned())
}

/// The parent-facing tool output of a completed subagent run.
/// Parent-visible text when a child turn ends with a terminal request failure.
///
/// Keep the failure reason and any text produced before it: the reason
/// distinguishes failures such as a 429 from an empty response, while the text
/// remains genuine output.
pub fn subagent_failure_output(final_text: &str, reason: &str, profile: &PromptProfile) -> String {
    let reason = reason.trim();
    let reason = if reason.is_empty() {
        profile.text(PromptKey::SubagentFailedUnknownReason)
    } else {
        reason
    };
    let text = final_text.trim();
    let failure = profile.render(PromptKey::SubagentFailed, &[("reason", reason)]);
    if text.is_empty() {
        return failure;
    }
    format!("{}\n\n{failure}", subagent_result_output(text, profile))
}

pub fn subagent_result_output(final_text: &str, profile: &PromptProfile) -> String {
    let text = final_text.trim();
    if text.is_empty() {
        return profile.text(PromptKey::SubagentNoTextResult).to_owned();
    }
    if text.len() <= MAX_SUBAGENT_OUTPUT {
        return text.to_owned();
    }
    let mut end = MAX_SUBAGENT_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n{}",
        &text[..end],
        profile.text(PromptKey::SubagentResultTruncated)
    )
}

fn required_string(input: &JsonObject, key: &str, max_chars: usize) -> Result<String, String> {
    let value = input
        .get(key)
        .ok_or_else(|| format!("missing required argument {key}"))?
        .as_str()
        .ok_or_else(|| format!("argument {key} must be a string"))?
        .trim();
    if value.is_empty() {
        return Err(format!("argument {key} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("argument {key} exceeds length limit {max_chars}"));
    }
    Ok(value.to_owned())
}

/// An optional argument; absent, `null` and blank all mean it was left out.
/// Models that fill every optional parameter send `""` for those.
fn optional_string(
    input: &JsonObject,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(_) => required_string(input, key, max_chars).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn agent_spawn_rejects_forged_assembled_prompt() {
        for key in ["assembled_system_prompt", "assembledSystemPrompt"] {
            let mut input = object(json!({"name": "worker", "prompt": "Inspect the code"}));
            input.insert(key.into(), json!("forged host prompt"));
            let error = parse_agent_spawn(&input, false).err().unwrap();
            assert!(error.contains(key), "{error}");
            assert!(error.contains("host-resolved"), "{error}");
        }
    }

    fn english_profile() -> PromptProfile {
        PromptProfile::builtin_english()
    }

    /// Parses `agent_spawn` in fallback mode, where a role is optional.
    ///
    /// Most cases validate parsing independently of role policy; required mode
    /// has dedicated `required_mode_*` cases.
    fn parse_fallback(input: &JsonObject) -> Result<AgentSpawnSpec, String> {
        parse_agent_spawn(input, false)
    }

    /// Formatting does not inspect identity; test envelopes use a first-generation
    /// placeholder identity.
    fn test_task_identity() -> agents::TaskIdentity {
        agents::TaskIdentity {
            generation: 1,
            params_digest: 0,
        }
    }

    /// The cost footer is appended per envelope, so it has to be inside what
    /// `truncate_agent_output` bounds. If truncation ever stops being the last
    /// step, a maximal child result plus footers would blow past the cap.
    #[test]
    fn the_cost_footer_stays_inside_the_output_cap() {
        let profile = english_profile();
        let metrics = agents::AgentTurnMetrics {
            usage: crate::model::ModelUsage {
                input_tokens: Some(1_000),
                cached_input_tokens: None,
                output_tokens: Some(2_000),
                total_tokens: Some(3_000),
                reasoning_tokens: None,
            },
            tool_use_count: 42,
            duration_ms: 1_234,
        };
        let envelopes: Vec<_> = ["a1", "a2"]
            .iter()
            .map(|name| AgentEnvelope {
                agent: (*name).to_owned(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
                content: "x".repeat(MAX_SUBAGENT_OUTPUT),
                metrics: Some(metrics.clone()),
                structured_output: None,
            })
            .collect();

        let rendered = format_wait_output(
            &envelopes,
            &[("a1".into(), agents::AgentLiveStatus::Idle)],
            &[],
            false,
            60,
            &profile,
        );
        assert!(rendered.len() <= MAX_SUBAGENT_OUTPUT + "\n… output truncated".len());

        // And with room to spare the footer is actually present and shaped.
        let small = format_wait_output(
            &[AgentEnvelope {
                agent: "a1".into(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
                content: "conclusion".into(),
                metrics: Some(metrics),
                structured_output: None,
            }],
            &[],
            &[],
            false,
            60,
            &profile,
        );
        assert!(
            small.contains("(This turn's cost: 3000 tokens · 42 tool calls · 1234 ms)"),
            "{small}"
        );

        // A legacy envelope carrying nothing measurable renders no footer at
        // all, rather than an all-zero line that reads like a measurement.
        let legacy = format_wait_output(
            &[AgentEnvelope {
                agent: "a1".into(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
                content: "conclusion".into(),
                metrics: None,
                structured_output: None,
            }],
            &[],
            &[],
            false,
            60,
            &profile,
        );
        assert!(!legacy.contains("This turn's cost"), "{legacy}");
    }

    #[test]
    fn agent_spawn_requires_a_nonempty_task_and_validates_the_label() {
        assert!(parse_fallback(&object(json!({}))).is_err());
        assert!(parse_fallback(&object(json!({"prompt":"   ", "name":"task-check"}))).is_err());
        // Name is required, so the negative case includes a valid one to ensure
        // `is_err()` exercises the label limit rather than missing `name`.
        let oversized_label = "x".repeat(81);
        let error = parse_fallback(&object(
            json!({"prompt":"ok","name":"label-check","label":oversized_label}),
        ))
        .err()
        .expect("label bound");
        assert!(error.contains("label"), "{error}");
    }

    #[test]
    fn questions_accept_multiple_items_and_legacy_input() {
        let spec = parse_question(&object(json!({
            "questions": [
                {
                    "question": "Which approach should we choose?",
                    "header": "Approach",
                    "options": [
                        {"label": "Approach A", "description": "Keep changes minimal"},
                        {"label": "Approach B", "description": "Refactor completely"}
                    ],
                    "multiSelect": false
                },
                {
                    "question": "Which capabilities should be enabled?",
                    "header": "Capabilities",
                    "options": [
                        {"label": "Search", "description": "Full-text search"},
                        {"label": "Export", "description": "Export files", "preview": "preview"}
                    ],
                    "multiSelect": true
                }
            ]
        })))
        .unwrap();
        assert_eq!(spec.questions.len(), 2);
        assert_eq!(spec.questions[0].options[0].label, "Approach A");
        assert!(spec.questions[1].multi_select);

        let legacy = parse_question(&object(json!({
            "question": "Continue?",
            "options": ["Continue"]
        })))
        .unwrap();
        assert_eq!(legacy.questions.len(), 1);
        assert!(parse_question(&object(json!({"question":"?","options":["", "x"]}))).is_err());
        assert!(parse_question(&object(json!({"question":"?","options":"not an array"}))).is_err());
        assert!(parse_question(&object(json!({
            "questions": [{
                "question": "?",
                "header": "Options",
                "options": [
                    {"label":"a","description":"a"},
                    {"label":"b","description":"b"},
                    {"label":"c","description":"c"},
                    {"label":"d","description":"d"},
                    {"label":"e","description":"e"}
                ],
                "multiSelect": false
            }]
        })))
        .is_err());
        assert!(parse_question(&object(json!({"questions":[]}))).is_err());
        assert!(parse_question(&object(json!({
            "questions": [
                {"question":"1"},{"question":"2"},{"question":"3"},
                {"question":"4"},{"question":"5"}
            ]
        })))
        .is_err());
    }

    /// The whole point of siting the precheck in `parse_agent_spawn`: an
    /// unsupported keyword is the SPAWNING call's error, reported to the party
    /// that can fix it, not a mysterious child failure four turns later.
    #[test]
    fn an_unsupported_schema_keyword_is_rejected_when_the_spawn_is_parsed() {
        let error = parse_fallback(&object(json!({
            "prompt": "review changes",
            "name": "schema-review",
            "schema": {
                "type": "object",
                "properties": {"verdict": {"type": "string", "pattern": "^ok$"}}
            }
        })))
        .err()
        .expect("pattern is outside the supported subset");
        assert!(error.contains("pattern"), "{error}");
    }

    #[test]
    fn output_schema_is_optional_absent_and_null_alike() {
        // Use one valid name so schema absence, null, and object are the only
        // varying inputs.
        for input in [
            json!({"prompt": "t", "name": "schema-optional"}),
            json!({"prompt": "t", "name": "schema-optional", "schema": null}),
        ] {
            let spec = parse_fallback(&object(input)).unwrap();
            assert!(spec.output_schema.is_none());
        }
        let spec = parse_fallback(&object(json!({
            "prompt": "t",
            "name": "schema-optional",
            "schema": {"type": "object", "properties": {"a": {"type": "string"}}}
        })))
        .unwrap();
        assert!(spec.output_schema.is_some());
    }

    #[test]
    fn output_schema_is_bounded_in_bytes_as_well_as_in_nodes() {
        let mut properties = serde_json::Map::new();
        let mut index = 0;
        while serde_json::to_vec(&properties).unwrap().len() <= MAX_OUTPUT_SCHEMA_BYTES {
            properties.insert(format!("field_{index}"), json!({"type": "string"}));
            index += 1;
        }
        let error = parse_fallback(&object(json!({
            "prompt": "t",
            "name": "schema-size",
            "schema": {"type": "object", "properties": Value::Object(properties)}
        })))
        .err()
        .expect("byte bound");
        assert!(error.contains("must not exceed"), "{error}");
        assert!(
            error.contains(&MAX_OUTPUT_SCHEMA_BYTES.to_string()),
            "{error}"
        );
    }

    #[test]
    fn a_structured_result_is_validated_and_bounded_before_it_is_accepted() {
        let schema = crate::subagent_schema::compile(&json!({
            "type": "object",
            "properties": {"verdict": {"type": "string"}, "notes": {"type": "string"}},
            "required": ["verdict"]
        }))
        .unwrap();
        assert_eq!(
            parse_structured_output(&object(json!({"verdict": "ok"})), &schema).unwrap(),
            json!({"verdict": "ok"})
        );
        let error =
            parse_structured_output(&object(json!({"verdict": 1})), &schema).expect_err("typed");
        assert!(error.contains("/verdict"), "{error}");

        let oversized = parse_structured_output(
            &object(json!({
                "verdict": "ok",
                "notes": "x".repeat(MAX_STRUCTURED_OUTPUT_BYTES)
            })),
            &schema,
        )
        .expect_err("byte bound");
        assert!(
            oversized.contains(&MAX_STRUCTURED_OUTPUT_BYTES.to_string()),
            "{oversized}"
        );
    }

    /// `truncate_agent_output` cuts on a byte boundary, so a fence it cuts
    /// through would reach the model unterminated. The block is therefore built
    /// to fit rather than trimmed afterwards.
    #[test]
    fn the_structured_block_is_rendered_inside_the_wait_output_and_stays_well_fenced() {
        let profile = english_profile();
        let envelopes = vec![AgentEnvelope {
            agent: "a1".into(),
            identity: test_task_identity(),
            kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
            content: "final conclusion".into(),
            metrics: None,
            structured_output: Some(json!({"verdict": "ok", "score": 7})),
        }];
        let output = format_wait_output(&envelopes, &[], &[], false, 60, &profile);
        assert!(output.contains("[a1 · completed]\nfinal conclusion"));
        assert!(output.contains("Structured result:\n```json"));
        assert!(output.contains("\"verdict\": \"ok\""));
        assert_eq!(output.matches("```").count(), 2, "fence must be balanced");

        let huge = json!({"notes": "x".repeat(MAX_RENDERED_STRUCTURED_OUTPUT_BYTES)});
        let rendered = format_structured_output(&huge, &profile);
        assert!(rendered.contains("truncated"));
        assert_eq!(rendered.matches("```").count(), 2);
        assert!(rendered.ends_with("```"));
    }

    #[test]
    fn a_run_that_owed_a_structured_result_reports_that_distinctly_from_its_prose() {
        let profile = english_profile();
        let empty = missing_structured_output_output("   ", &profile);
        assert!(empty.contains("never called structured_output"));
        let with_text = missing_structured_output_output("  I think it is fine  ", &profile);
        assert!(with_text.contains("not the structured result"));
        assert!(
            with_text.ends_with("I think it is fine"),
            "the child's own text is diagnostic and must survive: {with_text}"
        );
        // Bounded by `truncate_agent_output`, which cuts at the cap and then
        // appends its own marker — so the ceiling is the cap plus that marker,
        // not the cap itself.
        let bounded = missing_structured_output_output(&"x".repeat(MAX_SUBAGENT_OUTPUT), &profile);
        assert!(
            bounded.ends_with("… output truncated"),
            "{}",
            &bounded[..80]
        );
        assert!(
            bounded.len() < MAX_SUBAGENT_OUTPUT + 64,
            "{}",
            bounded.len()
        );
    }

    #[test]
    fn subagent_update_requires_a_bounded_nonempty_message() {
        let profile = english_profile();
        assert!(parse_subagent_update(&object(json!({}))).is_err());
        assert!(parse_subagent_update(&object(json!({"message":"  "}))).is_err());
        let input = object(json!({"message":" completed read-only review "}));
        assert_eq!(
            parse_subagent_update(&input).unwrap(),
            "completed read-only review"
        );
        assert_eq!(
            run_subagent_update(&input, &profile).unwrap(),
            "Progress note delivered to the parent agent."
        );
        assert!(parse_subagent_update(&object(json!({
            "message": "x".repeat(MAX_SUBAGENT_UPDATE_CHARS + 1)
        })))
        .is_err());
    }

    #[test]
    fn agent_spawn_validates_task_name_and_context_mode() {
        assert!(parse_fallback(&object(json!({}))).is_err());
        // Name is required; the host no longer assigns one automatically, so a
        // spawn without a name is an argument error rather than a fallback to `a1`.
        assert!(parse_fallback(&object(json!({"prompt":"review API"}))).is_err());
        let spec =
            parse_fallback(&object(json!({"prompt":"review API","name":"api-review"}))).unwrap();
        assert_eq!(spec.task, "review API");
        assert_eq!(spec.name, "api-review");
        assert_eq!(spec.agent_type, None);
        assert_eq!(spec.context, SpawnContextMode::None);

        let spec = parse_fallback(&object(json!({
            "prompt": "review API",
            "name": "review-api",
            "label": "review",
            "context": "conversation"
        })))
        .unwrap();
        assert_eq!(spec.name, "review-api");
        assert_eq!(spec.label.as_deref(), Some("review"));
        assert_eq!(spec.agent_type, None);
        assert_eq!(spec.context, SpawnContextMode::Conversation);

        assert!(parse_fallback(&object(json!({"prompt":"x","name":"Bad"}))).is_err());
        assert!(
            parse_fallback(&object(json!({"prompt":"x","name":"a","context":"fork"}))).is_err()
        );
    }

    /// Required mode rejects missing `agent_type` and excludes `context`.
    ///
    /// Error text must identify the enum requirement and explain that `context`
    /// is absent because conversation forks use this conversation's model and
    /// cannot be combined with an explicitly named role.
    #[test]
    fn required_mode_rejects_a_spawn_without_a_role_and_drops_context_entirely() {
        let required = |input: Value| parse_agent_spawn(&object(input), true);

        let error = required(json!({"prompt": "review API", "name": "role-check"}))
            .err()
            .unwrap();
        assert!(error.contains("agent_type"), "{error}");
        assert!(error.contains("required"), "{error}");

        // A complete role parses normally with the ordinary `none` context.
        let spec = required(json!({
            "prompt": "review API",
            "name": "role-check",
            "agent_type": "code-reviewer"
        }))
        .unwrap();
        assert_eq!(spec.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(spec.context, SpawnContextMode::None);

        // `context` is absent from this round's schema. A fork is refused —
        // it cannot carry a role — but the default it would otherwise take,
        // spelled out or left blank, changes nothing and is ignored.
        let error = required(json!({
            "prompt": "x",
            "name": "role-check",
            "agent_type": "code-reviewer",
            "context": "conversation"
        }))
        .err()
        .unwrap();
        assert!(error.contains("context"), "{error}");
        for context in [json!("none"), json!(""), Value::Null] {
            let spec = required(json!({
                "prompt": "x",
                "name": "role-check",
                "agent_type": "code-reviewer",
                "context": context
            }))
            .unwrap();
            assert_eq!(spec.context, SpawnContextMode::None, "{context}");
        }

        // In fallback mode a role remains optional and forking still works.
        // Name remains required in both modes.
        assert!(parse_fallback(&object(json!({
            "prompt": "review API",
            "name": "role-check"
        })))
        .is_ok());
        assert!(parse_fallback(&object(json!({
            "prompt": "x",
            "name": "role-check",
            "context": "conversation"
        })))
        .is_ok());
    }

    #[test]
    fn agent_spawn_takes_an_agent_type_as_free_text_and_leaves_resolution_to_the_host() {
        let spec = parse_fallback(&object(json!({
            "prompt": "review API",
            "name": "review-api",
            "agent_type": "code-reviewer"
        })))
        .unwrap();
        assert_eq!(spec.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(spec.context, SpawnContextMode::None);

        // A role name is free text, so nothing about its SHAPE is decided here.
        // Whether any of these names a real role is `resolve_agent_definition`'s
        // question, and answering it at parse time would only mean refusing a
        // legitimately-named role with a worse error than the one that knows
        // which roles exist.
        for accepted in ["CodeReviewer", "code/reviewer", "review agent", "对抗式审查"] {
            let spec = parse_fallback(&object(json!({
                "prompt": "x",
                "name": "review-api",
                "agent_type": accepted
            })))
            .unwrap();
            assert_eq!(spec.agent_type.as_deref(), Some(accepted));
        }

        // What is still refused is what cannot name anything at all. Every case
        // includes a valid `name` so `is_err()` exercises the agent_type rather
        // than the missing-name validation. A blank one is not among them: it
        // is the field left out.
        for invalid in ["review\ner", &"x".repeat(MAX_AGENT_TYPE_CHARS + 1)] {
            assert!(parse_fallback(&object(json!({
                "prompt": "x",
                "name": "review-api",
                "agent_type": invalid
            })))
            .is_err());
        }
        assert!(parse_fallback(&object(json!({
            "prompt": "x",
            "name": "review-api",
            "agent_type": "code-reviewer",
            "context": "conversation"
        })))
        .err()
        .unwrap()
        .contains("cannot be combined with context=conversation"));

        for host_owned in [
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","provider_id":"p"}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","modelId":"m"}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","systemPrompt":"forged"}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","memory":"user"}),
            // S7 (f) requires each rejected execution override, in either
            // spelling, to return the host-owned message rather than being
            // silently dropped as an unknown key.
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","tools":["read_file"]}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","disallowed_tools":["run_command"]}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","disallowedTools":["run_command"]}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","effort":"xhigh"}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","max_rounds":200}),
            json!({"prompt":"x","name":"review-api","agent_type":"reviewer","maxRounds":200}),
        ] {
            assert!(parse_fallback(&object(host_owned))
                .err()
                .unwrap()
                .contains("host-resolved"));
        }
        // A host-owned field left `null` or blank is no override at all, and an
        // empty `schema` constrains nothing; neither is refused.
        let spec = parse_fallback(&object(json!({
            "prompt": "x", "name": "review-api", "label": "", "agent_type": "",
            "model_id": null, "effort": "", "schema": {}
        })))
        .unwrap();
        assert_eq!(spec.label, None);
        assert_eq!(spec.agent_type, None);
        assert!(spec.output_schema.is_none());
    }

    /// A bare address is an agent name so every existing `a1` keeps working;
    /// the other kinds need their prefix, because a terminal id is free-form
    /// and would otherwise be indistinguishable from a slug.
    #[test]
    fn task_refs_round_trip_through_their_wire_form() {
        for raw in [
            "a1",
            "terminal:host-1",
            "preview:dev",
            "preview:dev@2",
            "workflow:run0a1b",
        ] {
            assert_eq!(TaskRef::parse(raw).unwrap().wire(), raw);
        }
        // A dev server's workspace follows the last `@`, and only a number is one.
        assert_eq!(
            TaskRef::parse("preview:web-2@3").unwrap(),
            TaskRef::Preview {
                server_id: "web-2".into(),
                workspace: Some(3),
            }
        );
        for (raw, server_id) in [
            ("preview:api@edge", "api@edge"),
            ("preview:api@0", "api@0"),
            ("preview:@2", "@2"),
        ] {
            assert_eq!(
                TaskRef::parse(raw).unwrap(),
                TaskRef::Preview {
                    server_id: server_id.into(),
                    workspace: None,
                },
                "{raw}"
            );
        }
        // Terminal ids may hold characters an agent name never could.
        assert_eq!(
            TaskRef::parse("terminal:Term_A#2").unwrap(),
            TaskRef::Terminal("Term_A#2".into())
        );
        assert_eq!(
            TaskRef::parse("workflow:run0a1b").unwrap(),
            TaskRef::Workflow("run0a1b".into())
        );
        // But a bare address is still held to the agent-name rules.
        assert!(TaskRef::parse("A1").is_err());
        assert!(TaskRef::parse("terminal:").is_err());
        assert!(TaskRef::parse("preview:  ").is_err());
        assert!(TaskRef::parse("workflow:").is_err());
    }

    #[test]
    fn task_wait_bounds_timeout_and_dedupes_names() {
        let spec = parse_task_wait(&object(json!({}))).unwrap();
        assert!(spec.tasks.is_empty());
        assert_eq!(spec.timeout_seconds, WAIT_DEFAULT_TIMEOUT_SECONDS);

        let spec = parse_task_wait(&object(json!({
            "tasks": ["a1", "a2", "a1", "terminal:t1"],
            "timeout_seconds": 30
        })))
        .unwrap();
        assert_eq!(
            spec.tasks,
            vec![
                TaskRef::Agent("a1".into()),
                TaskRef::Agent("a2".into()),
                TaskRef::Terminal("t1".into())
            ]
        );
        assert_eq!(spec.timeout_seconds, 30);

        // A timeout is held to the range, a fraction rounds, zero is the default,
        // and a blank task entry is skipped.
        let timeout = |value: Value| {
            parse_task_wait(&object(json!({"timeout_seconds": value})))
                .unwrap()
                .timeout_seconds
        };
        assert_eq!(timeout(json!(1)), WAIT_MIN_TIMEOUT_SECONDS);
        assert_eq!(timeout(json!(601)), WAIT_MAX_TIMEOUT_SECONDS);
        assert_eq!(timeout(json!(30.5)), 31);
        assert_eq!(timeout(json!(0)), WAIT_DEFAULT_TIMEOUT_SECONDS);
        assert!(parse_task_wait(&object(json!({"timeout_seconds": "soon"}))).is_err());
        assert_eq!(
            parse_task_wait(&object(json!({"tasks": ["", " a1 "]}))).unwrap().tasks,
            vec![TaskRef::Agent("a1".into())]
        );
        assert!(parse_task_wait(&object(json!({"tasks": "a1"}))).is_err());
        // A retired address must receive its specific error, not agent-name
        // validation, because models can copy it from historical timelines.
        let Err(retired) = parse_task_wait(&object(json!({"tasks": ["web_search:search-1"]})))
        else {
            panic!("web_search: is a retired address and must be rejected");
        };
        assert!(
            retired.contains("web_search is no longer a task"),
            "{retired}"
        );
    }

    /// S7 (h) requires the name-count bound to exceed `MAX_LIVE_AGENTS`: a
    /// Workflow runtime with a raised private-pool cap must let `task_wait` name
    /// every agent it can spawn. The strict inequality and concrete over-cap array
    /// keep the limits independently enforced, rather than failing in a pool test.
    #[test]
    fn task_wait_names_more_agents_than_the_default_live_cap() {
        assert!(
            agents::MAX_WAIT_AGENT_NAMES > agents::MAX_LIVE_AGENTS,
            "task_wait must be able to name every agent a raised pool can run"
        );
        let names = (1..=agents::MAX_WAIT_AGENT_NAMES)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>();
        let spec = parse_task_wait(&object(json!({ "tasks": names }))).unwrap();
        assert_eq!(spec.tasks.len(), agents::MAX_WAIT_AGENT_NAMES);
        assert!(spec.tasks.len() > agents::MAX_LIVE_AGENTS);

        let too_many = (1..=agents::MAX_WAIT_AGENT_NAMES + 1)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>();
        let error = parse_task_wait(&object(json!({ "tasks": too_many })))
            .err()
            .unwrap();
        assert!(error.contains(&format!(
            "tasks must not contain more than {} items",
            agents::MAX_WAIT_AGENT_NAMES
        )));
    }

    #[test]
    fn wait_output_formats_envelopes_statuses_and_timeouts() {
        let profile = english_profile();
        let envelopes = vec![
            AgentEnvelope {
                agent: "a1".into(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Update,
                content: "scan completed".into(),
                metrics: None,
                structured_output: None,
            },
            AgentEnvelope {
                agent: "a2".into(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
                content: "final conclusion".into(),
                metrics: None,
                structured_output: None,
            },
        ];
        let statuses = vec![
            ("a1".to_owned(), AgentLiveStatus::Running),
            ("a2".to_owned(), AgentLiveStatus::Idle),
        ];
        let output = format_wait_output(&envelopes, &statuses, &[], false, 60, &profile);
        assert!(output.contains("[a1 · progress update]\nscan completed"));
        assert!(output.contains("[a2 · completed]\nfinal conclusion"));
        assert!(output.contains("Current status:a1 running, a2 finished its turn"));

        // Timeout is a state statement and may coexist with accumulated progress
        // and delivered results. Address pending tasks separately to avoid
        // contradicting results in the same output.
        let timeout = format_wait_output(&envelopes, &statuses, &[], true, 45, &profile);
        assert!(timeout.contains("The 45-second wait expired"), "{timeout}");
        assert!(timeout.contains("the results of a2 are below"), "{timeout}");
        assert!(timeout.contains("a1 are still running"), "{timeout}");
        assert!(
            !timeout.contains("No task is running"),
            "a timeout that carries updates must not claim there were none: {timeout}"
        );
        assert!(
            timeout.contains("[a1 · progress update]\nscan completed"),
            "{timeout}"
        );

        // The no-result branch remains a state statement.
        let empty_timeout = format_wait_output(&[], &statuses, &[], true, 45, &profile);
        assert!(
            empty_timeout.contains("a1, a2 have not produced a result yet"),
            "{empty_timeout}"
        );
        assert!(
            empty_timeout.contains("still running in the background"),
            "{empty_timeout}"
        );

        let idle = format_wait_output(&[], &[], &[], false, 60, &profile);
        assert!(idle.contains("No task is running"));
    }

    /// Terminal results render before accumulated progress updates. Individual
    /// updates may exhaust the 64 KiB output budget; ordering results first
    /// ensures the result that ended the wait remains visible.
    #[test]
    fn a_flood_of_updates_cannot_truncate_away_the_result_that_ended_the_wait() {
        let profile = english_profile();
        let mut envelopes = (0..20)
            .map(|index| AgentEnvelope {
                agent: "a1".into(),
                identity: test_task_identity(),
                kind: EnvelopeKind::Update,
                content: format!("progress {index} ").repeat(400),
                metrics: None,
                structured_output: None,
            })
            .collect::<Vec<_>>();
        let update_bytes = envelopes
            .iter()
            .map(|envelope| envelope.content.len())
            .sum::<usize>();
        assert!(
            update_bytes > MAX_SUBAGENT_OUTPUT,
            "fixture must exceed the budget for this test to exercise truncation: {update_bytes}"
        );
        envelopes.push(AgentEnvelope {
            agent: "a1".into(),
            identity: test_task_identity(),
            kind: EnvelopeKind::Result(SubagentRunStatus::Completed),
            content: "final conclusion".into(),
            metrics: None,
            structured_output: None,
        });

        let output = format_wait_output(&envelopes, &[], &[], false, 60, &profile);
        assert!(
            output.contains("[a1 · completed]\nfinal conclusion"),
            "the result that ended the wait must survive truncation"
        );
        assert!(
            output.contains("… output truncated"),
            "the updates were truncated"
        );
    }

    /// Terminals and dev servers stream nothing to the parent, so waiting on
    /// one reports where it stands. Without this the wait would render as the
    /// "nothing to collect" case and the caller could not tell the two apart.
    #[test]
    fn wait_output_reports_non_agent_tasks_as_observations() {
        let profile = english_profile();
        let observations = vec![TaskObservation {
            task: "terminal:t1".into(),
            label: "build".into(),
            status_label: "running".into(),
            detail: Some("npm   run\nbuild".into()),
            settled: false,
        }];
        let output = format_wait_output(&[], &[], &observations, false, 60, &profile);
        assert!(
            output.contains("[terminal:t1 · running] (build)"),
            "{output}"
        );
        // Whitespace is collapsed so a multi-line detail stays one preview line.
        assert!(output.contains("npm run build"), "{output}");
        assert!(!output.contains("No task is running"), "{output}");
    }

    /// S9 requires the folded notification and `task_wait` rendering of an
    /// envelope to share one body builder, so the two surfaces cannot drift.
    /// `task_wait` differs from its notification only by the cost line. The
    /// notification carries those values as structured `<usage>`, so require
    /// `wait == notification + cost line` to prevent either side from drifting.
    #[test]
    fn undrained_result_notification_mirrors_the_wait_output_shape() {
        let profile = english_profile();
        let envelope = AgentEnvelope {
            agent: "a1".into(),
            identity: test_task_identity(),
            kind: EnvelopeKind::Result(SubagentRunStatus::RoundLimit),
            content: "  partial conclusion  ".into(),
            metrics: Some(agents::AgentTurnMetrics {
                usage: crate::model::ModelUsage {
                    total_tokens: Some(88),
                    ..Default::default()
                },
                tool_use_count: 3,
                duration_ms: 1200,
            }),
            structured_output: Some(json!({"verdict": "partial"})),
        };
        let notification = format_undrained_result_notification(&envelope, &profile);
        assert!(
            notification.contains("[a1 · round limit reached]\npartial conclusion"),
            "{notification}"
        );
        assert!(notification.contains("Structured result"), "{notification}");
        assert!(notification.contains("\"verdict\""), "{notification}");
        assert!(
            !notification.contains("This turn's cost"),
            "the cost moved into the structured <usage>: {notification}"
        );
        // Structured cost carries the same three measurements.
        assert_eq!(
            task_notification_usage(envelope.metrics.as_ref()),
            Some((Some(88), 3, 1200))
        );
        assert_eq!(
            task_status_wire(SubagentRunStatus::RoundLimit),
            "roundLimit"
        );
        let wait = format_wait_output(&[envelope], &[], &[], false, 60, &profile);
        assert_eq!(
            wait,
            format!("{notification}\n(This turn's cost: 88 tokens · 3 tool calls · 1200 ms)")
        );
        assert!(
            !notification.contains("automatically delivered by the host"),
            "the fold header is gone for good: {notification}"
        );
    }

    #[test]
    fn task_list_formats_grouped_rows_with_previews() {
        let profile = english_profile();
        assert!(format_task_list(&[], &profile).contains("no tasks yet"));
        // An empty group must not print its header — a conversation with only
        // Agents should not show an empty terminal section.
        assert!(format_task_list(
            &[TaskListGroup {
                title: "Terminals".into(),
                rows: vec![],
            }],
            &profile
        )
        .contains("no tasks yet"));
        let groups = vec![
            TaskListGroup {
                title: "Subagents".into(),
                rows: vec![TaskListRow {
                    task: "a1".into(),
                    label: "review".into(),
                    status_label: "interrupted".into(),
                    detail: "review   src directory\nand report results".into(),
                    latest_update: Some("half complete".into()),
                }],
            },
            TaskListGroup {
                title: "Terminals".into(),
                rows: vec![TaskListRow {
                    task: "terminal:t1".into(),
                    label: "build".into(),
                    status_label: "running".into(),
                    detail: "C:/work".into(),
                    latest_update: None,
                }],
            },
        ];
        let output = format_task_list(&groups, &profile);
        assert!(output.contains("2 tasks in total"), "{output}");
        assert!(
            output.contains("- a1 · interrupted (review)"),
            "{output}"
        );
        assert!(
            output.contains("review src directory and report results"),
            "{output}"
        );
        assert!(output.contains("Latest update: half complete"), "{output}");
        assert!(output.contains("Terminals"), "{output}");
        assert!(
            output.contains("- terminal:t1 · running (build)"),
            "{output}"
        );
    }

    #[test]
    fn subagent_output_reports_empty_results_and_truncates() {
        let profile = english_profile();
        assert!(subagent_result_output("  ", &profile).contains("returned no text"));
        assert_eq!(subagent_result_output("answer", &profile), "answer");
        let long = "x".repeat(64 * 1024 + 1);
        let truncated = subagent_result_output(&long, &profile);
        assert!(
            truncated.ends_with("… subagent result truncated"),
            "{truncated}"
        );
        assert!(truncated.len() <= 64 * 1024 + 64);
    }
}
