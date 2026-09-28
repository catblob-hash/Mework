use std::{
    fs::{self, File},
    io::{Read, Seek},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use globset::Glob;
use regex::RegexBuilder;
use serde_json::{json, Map, Value};
use similar::TextDiff;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

use crate::host_platform::host_platform;
use crate::{
    cancel::CancelSignal,
    file_read_state::{self, FileReadRecord, FileReadRegistry, ScopeRef},
    image_attachments::{is_supported_image, ImageAttachmentStore},
    model::{
        ImageAttachment, JsonObject, ToolExecutionRequest, ToolExecutionResponse,
        ToolResult,
    },
    path_guard::{
        canonical_workspace, existing_path_is_allowed, relative_display,
        resolve_existing_with_scope, resolve_for_write_with_scope,
        secure_open_existing_file_with_scope, ExecutionScope,
    },
    prompt_profile::{PromptKey, PromptProfile},
    search_scope,
    shell_tasks::{ShellOutputSink, ShellOutputStream, ShellTaskOutcome},
    state::AppState,
    storage::atomic_write,
};

const MAX_TOOL_OUTPUT: usize = 64 * 1024;
const MAX_DIFF_OUTPUT: usize = 64 * 1024;
pub(crate) const MAX_TEXT_FILE: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_PATH_CHARS: usize = 4096;
const MAX_COMMAND_CHARS: usize = 64 * 1024;
/// How often a running command checks whether someone asked it to stop. This is what bounds the
/// delay between pressing the stop button and the process tree dying, and — since a command has no
/// deadline of its own — it is the only thing standing between a runaway build and the user.
const SHELL_STOP_POLL: Duration = Duration::from_millis(100);

/// Claude Code's two read-gate refusals, byte for byte. Deliberately not
/// prompt-profile keys: like every other tool error here they are structural,
/// and the model's recovery ("read it, then retry") is the same in any language.
pub(crate) const FILE_NOT_READ: &str = "File has not been read yet. Read it first before writing to it.";
pub(crate) const FILE_MODIFIED_SINCE_READ: &str =
    "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.";

/// The read record one call's file write guards consult. All five guards are
/// unconditional, so there is no policy beside the scope any more — what a call
/// has is a record, or none at all. Absent for direct IPC and tests, where
/// nothing is recorded or checked.
#[derive(Clone, Copy)]
pub(crate) struct FileGuardContext<'a> {
    pub scope: ScopeRef<'a>,
    pub registry: &'a FileReadRegistry,
}

impl FileGuardContext<'_> {
    fn get(&self, path: &Path) -> Option<FileReadRecord> {
        self.registry.get(self.scope, path)
    }

    fn record(&self, path: PathBuf, record: FileReadRecord) {
        self.registry.record(self.scope, path, record);
    }
}

/// The file a successful `read`, `write` or `edit` touched, by canonical path,
/// so the run loop can commit the read record once the result is final and
/// re-check the file after PostToolUse hooks.
pub(crate) struct FileGuardTouch {
    pub path: PathBuf,
    /// What a `read` saw; `None` for the two writers, which recorded their own
    /// result before returning.
    pub read: Option<FileReadRecord>,
}

pub(crate) struct Outcome {
    success: bool,
    output: String,
    images: Vec<ImageAttachment>,
    diff: Option<String>,
    opened_file: Option<PathBuf>,
    file_touch: Option<FileGuardTouch>,
}

impl Outcome {
    fn success(output: String) -> Self {
        Self {
            success: true,
            output,
            images: Vec::new(),
            diff: None,
            opened_file: None,
            file_touch: None,
        }
    }

    fn success_with_images(output: String, images: Vec<ImageAttachment>) -> Self {
        Self {
            success: true,
            output,
            images,
            diff: None,
            opened_file: None,
            file_touch: None,
        }
    }

    fn success_with_diff(output: String, diff: Option<String>) -> Self {
        Self {
            success: true,
            output,
            images: Vec::new(),
            diff,
            opened_file: None,
            file_touch: None,
        }
    }

    fn with_opened_file(mut self, opened_file: PathBuf) -> Self {
        self.opened_file = Some(opened_file);
        self
    }

    fn with_file_touch(mut self, touch: Option<FileGuardTouch>) -> Self {
        self.file_touch = touch;
        self
    }
}

pub(crate) struct VerifiedOpenedFile(PathBuf);

impl VerifiedOpenedFile {
    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
}

pub(crate) struct VerifiedToolExecutionResponse {
    pub result: ToolResult,
    /// Canonical identity returned by the exact verified handle consumed by a
    /// successful `read`. This host-only field is never serialized.
    pub opened_file: Option<VerifiedOpenedFile>,
    /// The file a guarded `read`/`write`/`edit` touched. Host-only, like the
    /// identity above; `None` whenever the call ran without a guard.
    pub file_touch: Option<FileGuardTouch>,
}

/// Workspace-only execution, for tests.
#[cfg(test)]
pub fn execute(request: ToolExecutionRequest, state: &AppState) -> ToolExecutionResponse {
    let scope = ExecutionScope::workspace_only(Path::new(&request.workspace_path));
    execute_with_scope(request, state, scope)
}

/// Executes a request with the filesystem boundary selected by the trusted
/// security classifier, for tests. Callers must still honor `requires_approval`
/// before passing the corresponding scope here; this function enforces the
/// boundary but does not display or verify approval UI itself.
#[cfg(test)]
pub fn execute_with_scope(
    request: ToolExecutionRequest,
    state: &AppState,
    scope: ExecutionScope,
) -> ToolExecutionResponse {
    execute_with_scope_and_attachments(
        request,
        state,
        scope,
        None,
        &crate::workspace_set::WorkspaceSet::default(),
        &PromptProfile::default(),
    )
}

/// Executes with an optional trusted attachment root. Image-producing tools
/// require this root to make their pixels available to later model rounds.
///
/// `runner` is the trusted shell environment resolved by the host. Only `bash` and
/// `powershell` use it. Tests use the default local runner with no injected
/// variables.
///
/// `profile` is the conversation's prompt profile: it words the framing this
/// executor puts around a command's output. A tool run outside a model turn is
/// still written into that conversation, so the IPC path resolves the real one
/// rather than accepting the built-in English default.
pub fn execute_with_scope_and_attachments(
    request: ToolExecutionRequest,
    state: &AppState,
    scope: ExecutionScope,
    app_data: Option<&Path>,
    workspaces: &crate::workspace_set::WorkspaceSet,
    profile: &PromptProfile,
) -> ToolExecutionResponse {
    // Direct IPC and test calls do not belong to a model turn, so they receive an
    // empty signal and no handoff. Their synchronous shells are stopped only by
    // their own task row, and a timed-out one has no pool to be adopted into.
    execute_with_scope_and_attachments_verified(
        request,
        state,
        scope,
        app_data,
        &CancelSignal::default(),
        workspaces,
        None,
        profile,
    )
    .result
}

/// `cancel` is the execution signal minted by the model-turn dispatcher according
/// to ownership. Task turns carry their task flag, and top-level turns carry this
/// run's flag; only the dispatcher knows which owner applies.
///
/// `handoff` is what a shell call whose deadline expires is offered to. Only the
/// model-turn dispatcher can supply one, because adopting a running command means
/// putting it in that turn's task pool; direct IPC passes `None` and its
/// timed-out commands are stopped.
pub(crate) fn execute_with_scope_and_attachments_verified(
    request: ToolExecutionRequest,
    state: &AppState,
    scope: ExecutionScope,
    app_data: Option<&Path>,
    cancel: &CancelSignal,
    workspaces: &crate::workspace_set::WorkspaceSet,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
) -> VerifiedToolExecutionResponse {
    execute_with_scope_and_attachments_guarded(
        request,
        state,
        scope,
        app_data,
        cancel,
        workspaces,
        handoff,
        profile,
        None,
    )
}

/// The model-turn entry: the same as the verified variant, plus the file write
/// guards this turn runs under. Only the run loop has a guard to pass; every
/// other caller keeps the unguarded contract, where `read` records nothing and
/// `write`/`edit` check nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_scope_and_attachments_guarded(
    request: ToolExecutionRequest,
    state: &AppState,
    scope: ExecutionScope,
    app_data: Option<&Path>,
    cancel: &CancelSignal,
    workspaces: &crate::workspace_set::WorkspaceSet,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> VerifiedToolExecutionResponse {
    let started = Instant::now();
    let attachment_store = app_data.map(ImageAttachmentStore::new);
    let result = run_tool(
        &request,
        &scope,
        state,
        attachment_store.as_ref(),
        cancel,
        workspaces,
        app_data,
        handoff,
        profile,
        file_guard,
    )
    .unwrap_or_else(|error| Outcome {
        success: false,
        output: error,
        images: Vec::new(),
        diff: None,
        opened_file: None,
        file_touch: None,
    });
    finish_verified_execution(&request, state, started, result, app_data, profile)
}

fn finish_execution(
    request: &ToolExecutionRequest,
    state: &AppState,
    started: Instant,
    result: Outcome,
    profile: &PromptProfile,
) -> ToolExecutionResponse {
    finish_verified_execution(request, state, started, result, None, profile).result
}

fn finish_verified_execution(
    request: &ToolExecutionRequest,
    state: &AppState,
    started: Instant,
    result: Outcome,
    app_data: Option<&Path>,
    profile: &PromptProfile,
) -> VerifiedToolExecutionResponse {
    let opened_file = result.opened_file;
    let file_touch = result.file_touch;
    let response = ToolExecutionResponse {
        success: result.success,
        output: fit_output(request, result.output, app_data, profile),
        images: result.images,
        diff: result.diff.map(|diff| truncate_diff(&diff, profile)),
        executed_at: Utc::now().to_rfc3339(),
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    };
    state.record_receipt(request, &response);
    VerifiedToolExecutionResponse {
        result: response,
        opened_file: opened_file.map(VerifiedOpenedFile),
        file_touch,
    }
}

/// A result's text as the model receives it. The file tools and the shells
/// bound their own output — a character budget for `ls`, a match count for
/// `find`, a byte budget for `read`, the spill for a command — so the generic
/// cap is for everything else. `grep` bounds its matches and then, like a
/// command, spills what is still too long, Claude Code's 20,000 characters.
fn fit_output(
    request: &ToolExecutionRequest,
    output: String,
    app_data: Option<&Path>,
    profile: &PromptProfile,
) -> String {
    match request.tool_name.as_str() {
        "grep" => crate::tool_output::fit(
            output,
            crate::tool_output::GREP_INLINE_CHARS,
            crate::tool_output::Spill::new(app_data, &request.conversation_id, "grep"),
            profile,
        ),
        "ls" | "find" | "read" => output,
        name if ShellKind::of_tool(name).is_some() => output,
        _ => truncate_output(&output, profile),
    }
}

/// Reads the `workspace` argument a multi-workspace conversation's tools carry.
///
/// Absent is workspace 1. The value is a number rather than a path because a
/// path could name a directory on any machine, and the set of machines a
/// conversation may reach is not something a tool argument gets to widen.
/// A non-integer is refused rather than coerced: the schema states an enum of
/// integers, and silently reading `"2"` or `2.5` as workspace 2 would make the
/// boundary depend on how a provider happened to serialize the call.
pub(crate) fn workspace_argument(input: &JsonObject) -> Result<Option<u32>, String> {    match input.get("workspace") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|value| *value >= 1 && *value <= u32::MAX as u64)
            .map(|value| Some(value as u32))
            .ok_or_else(|| {
                "The workspace parameter must be one of the workspace numbers listed in the Environment section".to_owned()
            }),
        Some(_) => Err(
            "The workspace parameter must be a number naming one of this conversation's workspaces"
                .to_owned(),
        ),
    }
}

/// The remote leg's view of the selected workspace.
///
/// The scope the classifier chose is folded to the one bit that means anything
/// on another machine: whether the call is confined to its workspace. Denied
/// roots are host paths, and a restricted scope's roots are host paths too —
/// none of them exist where this call runs.
fn remote_workspace<'a>(
    workspace: &'a crate::workspace_set::ResolvedWorkspace,
    scope: &ExecutionScope,
    profile: &'a PromptProfile,
    cancel: &'a CancelSignal,
) -> crate::remote_files::RemoteWorkspace<'a> {
    crate::remote_files::RemoteWorkspace {
        workspace,
        machine_key: crate::run_environment::env_key(workspace.machine.as_ref()),
        confinement: match scope {
            ExecutionScope::Restricted { .. } | ExecutionScope::RestrictedExcept { .. } => {
                crate::remote_files::Confinement::Workspace
            }
            ExecutionScope::Unrestricted | ExecutionScope::UnrestrictedExcept { .. } => {
                crate::remote_files::Confinement::Machine
            }
        },
        profile,
        cancel,
    }
}

impl From<crate::remote_files::RemoteOutcome> for Outcome {
    fn from(outcome: crate::remote_files::RemoteOutcome) -> Self {
        Self {
            success: true,
            output: outcome.output,
            images: outcome.images,
            diff: outcome.diff,
            // No handle was opened in this process, so there is no local
            // identity to hand the run loop.
            opened_file: None,
            file_touch: outcome.file_touch,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_tool(
    request: &ToolExecutionRequest,
    scope: &ExecutionScope,
    state: &AppState,
    attachment_store: Option<&ImageAttachmentStore>,
    cancel: &CancelSignal,
    workspaces: &crate::workspace_set::WorkspaceSet,
    app_data: Option<&Path>,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<Outcome, String> {
    // A caller that resolved no set is a caller with one workspace: its own
    // request path. Direct IPC, replayed timeline entries and tests all arrive
    // this way, and every one of them means "act where this conversation acts".
    let fallback;
    let workspaces = if workspaces.is_empty() {
        fallback = crate::workspace_set::WorkspaceSet::local_root(&request.workspace_path);
        &fallback
    } else {
        workspaces
    };
    // Which workspace this call acts in. A conversation with one workspace never
    // sees the parameter and every call lands on workspace 1; a conversation with
    // several has been told their numbers, and the number is the only thing that
    // selects a machine — a path never does.
    let selected = workspaces.select(workspace_argument(&request.input)?)?;
    // The host-side anchor. It is the primary workspace when that is local, and
    // the conversation's local scratch root when it is not: the preview and
    // browser tools are host-machine subsystems and need a real local directory
    // whatever machine the model is addressing.
    let anchor = Path::new(&request.workspace_path);
    // Filesystem tools act inside the selected workspace, so relative arguments
    // resolve against its root rather than against the anchor.
    let workspace = if selected.is_local() {
        Path::new(&selected.root)
    } else {
        anchor
    };
    // Filesystem tools on another machine go through that machine's shell; the
    // path never touches this filesystem, and a language server for such a
    // workspace is started there too. Everything after this match is a
    // host-side tool, or one whose workspace is here.
    //
    // One exception: output this conversation's own tools saved on the host
    // ([`crate::tool_output`]). The model was handed that path to read back,
    // and it exists nowhere but here.
    let names_host_output = app_data
        .is_some_and(|app_data| crate::tool_output::names_host_file(app_data, request));
    if !selected.is_local() && !names_host_output {
        let remote = remote_workspace(selected, scope, profile, cancel);
        match request.tool_name.as_str() {
            "ls" => return crate::remote_files::run_ls(&remote, &request.input).map(Outcome::success),
            "grep" => {
                return crate::remote_files::run_grep(&remote, &request.input).map(Outcome::success)
            }
            "find" => {
                return crate::remote_files::run_find(&remote, &request.input).map(Outcome::success)
            }
            "read" => {
                return crate::remote_files::run_read(
                    &remote,
                    &request.input,
                    attachment_store,
                    file_guard,
                )
                .map(Outcome::from)
            }
            "write" => {
                return crate::remote_files::run_write(&remote, &request.input, file_guard)
                    .map(Outcome::from)
            }
            "edit" => {
                return crate::remote_files::run_edit(&remote, &request.input, file_guard)
                    .map(Outcome::from)
            }
            "lsp" => {
                return crate::remote_lsp::run(
                    &remote,
                    &state.lsp_servers,
                    &request.input,
                    profile.language,
                    &request.conversation_id,
                )
                .map(Outcome::success)
            }
            _ => {}
        }
    }
    match request.tool_name.as_str() {
        "ls" => run_ls(workspace, &request.input, scope, profile).map(Outcome::success),
        "grep" => run_grep(workspace, &request.input, scope, profile).map(Outcome::success),
        name if ShellKind::of_tool(name).is_some() => {
            let kind = ShellKind::of_tool(name).expect("guarded by the arm");
            run_shell(
                selected,
                anchor,
                &request.input,
                kind,
                &request.conversation_id,
                state,
                cancel,
                app_data,
                handoff,
                profile,
                file_guard,
            )
        }
        "write" => run_write(workspace, &request.input, scope, profile, file_guard),
        "edit" => run_edit(workspace, &request.input, scope, profile, file_guard),
        "find" => run_find(workspace, &request.input, scope, profile).map(Outcome::success),
        "read" => run_read(
            workspace,
            &request.input,
            scope,
            attachment_store,
            profile,
            file_guard,
        ),
        "lsp" => run_lsp(workspace, request, scope, state, profile).map(Outcome::success),
        // Fifteen preview tools. Four run entirely host-side against the dev-server registry;
        // the rest resolve a target and act on the conversation's page.
        "preview_start" => run_preview_start(request, state, selected, workspace),
        "preview_stop" => run_preview_stop(request, state, workspaces),
        "preview_list" => run_preview_list(request, state, workspaces),
        "preview_logs" => run_preview_logs(request, state, workspaces),
        "preview_console_logs"
        | "preview_screenshot"
        | "preview_snapshot"
        | "preview_inspect"
        | "preview_click"
        | "preview_fill"
        | "preview_eval"
        | "preview_network"
        | "preview_resize"
        | "preview_dialog" => {
            let tool = crate::browser::PreviewTool::from_tool_name(&request.tool_name)
                .ok_or_else(|| format!("Unknown tool: {}", request.tool_name))?;
            run_preview_page_tool(
                request,
                state,
                tool,
                crate::browser::BrowserToolGrants::default(),
                attachment_store,
            )
        }
        // Resolving the image number needs the run loop's transcript, so the loop calls
        // `execute_browser_upload_image` directly; any other route into this tool has no
        // attachment to upload.
        "preview_upload_image" => Err(
            "preview_upload_image requires the model run loop to resolve the image number from the conversation before execution".into(),
        ),
        // Orchestration tools run inside the host model loop only. Every direct
        // execution path rejects these names instead of faking a success.
        "subagent" | "ask_user" | "fork" | "todo" | "workflow" | "workflow_step" | "TaskCreate"
        | "TaskUpdate" | "TaskGet" | "TaskList" => {
            Err(format!(
                "Orchestration tool {} can only be executed by the in-app model run loop",
                request.tool_name
            ))
        }
        // Child-only tools are explicit so replayed timeline entries report that
        // only a subagent run can execute them, rather than treating them as unknown.
        "subagent_update" | "structured_output" => Err(format!(
            "Child-agent-only tool {} can only be executed by the in-app model run loop during a child agent turn",
            request.tool_name
        )),
        "read_global_memory" | "read_project_memory" | "create_global_memory"
        | "create_project_memory" | "edit_global_memory" | "edit_project_memory" => {
            Err(format!(
                "Long-term memory tool {} can only be executed after the host injects the current model identity",
                request.tool_name
            ))
        }
        name => Err(format!("Unknown tool: {name}")),
    }
}

/// Wall clock a page tool gets before the host stops waiting for it, and the sentence it answers
/// with when it does. `preview_eval` gets the same 30 s as everything else: the source's longer
/// budget belongs to its REPL-shaped `javascript_tool`, which this surface does not have.
const PREVIEW_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// `serverId` resolved to nothing while the pane itself is unavailable.
const PREVIEW_NO_SERVERS: &str = "Server not found. No running servers for this workspace.";
/// Nothing to act on: no dev server, and no page the user opened by hand either.
///
/// The source's wording names a `navigate` tool and a `url` argument to
/// `preview_start`; both belong to its tabbed surface, which this build does not
/// have. Copying it verbatim would advertise two things the model cannot call.
pub(crate) const PREVIEW_PANE_NOT_OPEN: &str = "No preview is open. Use `preview_start` with {\"name\": \"\u{2026}\"} to start a dev server from .mework/launch.json.";

pub(crate) fn preview_stale_server_id(server_id: &str) -> String {
    format!("serverId \"{server_id}\" not found \u{2014} it may be stale or belong to another session. Call preview_list to get current ids.")
}

fn encode_browser_tool_result(value: &serde_json::Value) -> Result<Outcome, String> {
    serde_json::to_string_pretty(value)
        .map(Outcome::success)
        .map_err(|error| format!("Failed to encode browser tool result: {error}"))
}

/// Every dev server this conversation may address: the ones of this workspace, its own or nobody's,
/// then the ones it started in its other workspaces — on this computer or on another machine —
/// which only it may address.
pub(crate) fn preview_servers_for_session(
    state: &AppState,
    workspace: &Path,
    conversation_id: &str,
) -> Vec<crate::preview_servers::PreviewServerSnapshot> {
    let mut servers: Vec<_> = state
        .preview_servers
        .servers_for_worktree(workspace)
        .into_iter()
        .filter(|server| {
            server.session_id.is_none() || server.session_id.as_deref() == Some(conversation_id)
        })
        .collect();
    for server in state.preview_servers.servers_owned_by(conversation_id) {
        if !servers.iter().any(|listed| listed.server_id == server.server_id) {
            servers.push(server);
        }
    }
    servers
}

/// One dev server that this conversation is allowed to address, in whichever workspace. The page
/// tools ask this: a conversation has one page, so any server answering to the id will do.
pub(crate) fn preview_server_for_session(
    state: &AppState,
    workspace: &Path,
    conversation_id: &str,
    server_id: &str,
) -> Option<crate::preview_servers::PreviewServerSnapshot> {
    preview_servers_for_session(state, workspace, conversation_id)
        .into_iter()
        .find(|server| crate::preview::server_ids_match(&server.server_id, server_id))
}

/// One dev server this conversation may address, with the number of the workspace it belongs to.
///
/// The number is there only when the conversation has several workspaces, because only then is
/// it part of the address: `serverId` is a launch.json name, and two workspaces can both have a
/// `dev`. It is also absent for a server the conversation started in a directory that is no longer
/// one of its workspaces.
pub(crate) struct AddressableServer {
    pub workspace: Option<u32>,
    pub server: crate::preview_servers::PreviewServerSnapshot,
}

/// Every dev server this conversation may address, workspace by workspace in the order the
/// conversation numbers them, each workspace's in the order they were started: the ones this
/// conversation started or nobody did, then the ones it started anywhere else.
pub(crate) fn addressable_preview_servers(
    state: &AppState,
    workspaces: &crate::workspace_set::WorkspaceSet,
    conversation_id: &str,
) -> Vec<AddressableServer> {
    let numbered = workspaces.len() > 1;
    let mut listed: Vec<AddressableServer> = Vec::new();
    for entry in workspaces.entries() {
        let Some(key) = crate::preview::registry_key(entry) else {
            continue;
        };
        for server in state.preview_servers.servers_for_worktree(&key) {
            let mine = server
                .session_id
                .as_deref()
                .is_none_or(|owner| owner == conversation_id);
            if mine
                && !listed
                    .iter()
                    .any(|listed| listed.server.handle == server.handle)
            {
                listed.push(AddressableServer {
                    workspace: numbered.then_some(entry.index),
                    server,
                });
            }
        }
    }
    for server in state.preview_servers.servers_owned_by(conversation_id) {
        if !listed
            .iter()
            .any(|listed| listed.server.handle == server.handle)
        {
            listed.push(AddressableServer {
                workspace: None,
                server,
            });
        }
    }
    listed
}

/// One dev server as the model reads it: its `serverId`, the workspace that completes the address
/// when there are several, and what it is doing. The pane's handle and the owning conversation are
/// left out — neither is anything the model can use.
fn preview_server_view(addressable: &AddressableServer) -> Value {
    let server = &addressable.server;
    let mut view = serde_json::Map::new();
    view.insert("serverId".to_owned(), json!(server.server_id));
    if let Some(workspace) = addressable.workspace {
        view.insert("workspace".to_owned(), json!(workspace));
    }
    view.insert("port".to_owned(), json!(server.port));
    view.insert("status".to_owned(), json!(server.status));
    if let Some(machine) = &server.machine {
        view.insert("machine".to_owned(), json!(machine));
    }
    view.insert("startedAt".to_owned(), json!(server.started_at));
    Value::Object(view)
}

/// What a `serverId`, and the `workspace` beside it, address among a conversation's servers.
pub(crate) enum PreviewServerAddress<'a> {
    Found(&'a AddressableServer),
    /// Nothing answers to it in the workspace named; these other workspaces have one.
    Elsewhere(Vec<u32>),
    /// No workspace was named, and more than one has a server answering to it.
    Ambiguous(Vec<u32>),
    Missing,
}

/// Resolves a `serverId`. A `workspace` narrows it to that workspace; without one, the id has to
/// be unambiguous, so a conversation whose workspaces each run a `dev` is asked which.
pub(crate) fn address_preview_server<'a>(
    servers: &'a [AddressableServer],
    server_id: &str,
    workspace: Option<u32>,
) -> PreviewServerAddress<'a> {
    let matching: Vec<&AddressableServer> = servers
        .iter()
        .filter(|candidate| {
            crate::preview::server_ids_match(&candidate.server.server_id, server_id)
        })
        .collect();
    let mut workspaces: Vec<u32> = matching
        .iter()
        .filter_map(|candidate| candidate.workspace)
        .collect();
    workspaces.sort_unstable();
    workspaces.dedup();
    match workspace {
        Some(number) => match matching
            .iter()
            .find(|candidate| candidate.workspace == Some(number))
        {
            Some(found) => PreviewServerAddress::Found(found),
            None if workspaces.is_empty() => PreviewServerAddress::Missing,
            None => PreviewServerAddress::Elsewhere(workspaces),
        },
        None if workspaces.len() > 1 => PreviewServerAddress::Ambiguous(workspaces),
        None => matching
            .first()
            .map_or(PreviewServerAddress::Missing, |found| {
                PreviewServerAddress::Found(found)
            }),
    }
}

/// The refusal for an address that resolved to the wrong workspace or to several, or `None` for
/// one that resolved to a single server or to nothing.
fn preview_address_refusal(
    address: &PreviewServerAddress<'_>,
    server_id: &str,
    workspace: Option<u32>,
) -> Option<String> {
    let list = |numbers: &[u32]| {
        numbers
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match address {
        PreviewServerAddress::Elsewhere(numbers) => Some(match numbers.as_slice() {
            [number] => format!(
                "No server \"{server_id}\" in workspace {}. It runs in workspace {number}; pass that as workspace.",
                workspace.unwrap_or(1)
            ),
            _ => format!(
                "No server \"{server_id}\" in workspace {}. Servers with that id run in workspaces {}; pass one of those as workspace.",
                workspace.unwrap_or(1),
                list(numbers)
            ),
        }),
        PreviewServerAddress::Ambiguous(numbers) => Some(format!(
            "Servers with id \"{server_id}\" run in workspaces {}. Pass workspace to say which.",
            list(numbers)
        )),
        PreviewServerAddress::Found(_) | PreviewServerAddress::Missing => None,
    }
}

/// Whether `server_id` is an attach entry of a workspace the call could mean — the one it named,
/// or any of them. Asked only once no process answered, so a remote read costs nothing on the
/// usual path.
fn preview_attach_entry(
    workspaces: &crate::workspace_set::WorkspaceSet,
    workspace: Option<u32>,
    server_id: &str,
) -> bool {
    workspaces
        .entries()
        .iter()
        .filter(|entry| workspace.is_none_or(|number| entry.index == number))
        .filter_map(crate::preview::workspace_configurations)
        .any(|list| crate::preview::is_attach_entry(&list, server_id))
}

/// The server an absent `serverId` falls back to: the first one for this worktree that is running
/// or on its way there.
pub(crate) fn first_running_preview_server(
    state: &AppState,
    workspace: &Path,
    conversation_id: &str,
) -> Option<crate::preview_servers::PreviewServerSnapshot> {
    crate::preview_servers::running_for_session(
        &preview_servers_for_session(state, workspace, conversation_id),
        Some(conversation_id),
    )
    .into_iter()
    .next()
}

/// Port of the source's `cDr`: an explicit `serverId` validated against this session, else the
/// first running server for the worktree, else the session's own preview page.
///
/// Mework has one page per conversation rather than a tab per server, so what a resolved server
/// decides is only *whether* the call has something to act on — never which surface it lands on.
pub(crate) fn resolve_preview_page_session(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
) -> Result<String, String> {
    let session_id = crate::browser::preview_page_session_id(&request.conversation_id)?;
    if let Some(server_id) = optional_owned_string(&request.input, "serverId", 256)? {
        return preview_server_for_session(state, workspace, &request.conversation_id, &server_id)
            .map(|_| session_id)
            .ok_or_else(|| preview_stale_server_id(&server_id));
    }
    if first_running_preview_server(state, workspace, &request.conversation_id).is_some() {
        return Ok(session_id);
    }
    if !state.browser.is_attached() {
        return Err(PREVIEW_NO_SERVERS.into());
    }
    let status = state.browser.status(&session_id);
    if status.has_page || status.suspended {
        return Ok(session_id);
    }
    Err(PREVIEW_PANE_NOT_OPEN.into())
}

/// Where the pane is, for the timeout sentence. The source names the pane's visibility so the
/// model can tell a hung renderer from a preview nobody is looking at.
pub(crate) fn preview_pane_state(state: &AppState, session_id: &str) -> &'static str {
    let status = state.browser.status(session_id);
    if !status.has_page && !status.suspended {
        "The Browser pane is not open."
    } else if status.open {
        "The Browser pane is currently displayed."
    } else {
        "The Browser pane is currently hidden."
    }
}

/// Runs one preview page tool under the source's 30 s wall clock.
///
/// The call is handed to its own thread because a CDP round trip has no cancellation of its own:
/// a wedged page would otherwise hold this turn open indefinitely. The orphaned thread keeps the
/// page's automation lock until it finishes, which is what makes the next call time out too —
/// exactly the "the pane may be stuck" the message describes.
pub(crate) fn dispatch_preview_page_tool(
    state: &AppState,
    session_id: &str,
    tool: crate::browser::PreviewTool,
    input: &JsonObject,
    grants: crate::browser::BrowserToolGrants,
) -> Result<crate::browser::PreviewToolOutput, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let runtime = state.browser.clone();
    let owned_session = session_id.to_owned();
    let owned_input = input.clone();
    thread::spawn(move || {
        let _ =
            sender.send(runtime.execute_tool_blocking(&owned_session, tool, &owned_input, &grants));
    });
    match receiver.recv_timeout(PREVIEW_TOOL_TIMEOUT) {
        Ok(result) => result,
        // A dropped sender is a panicked worker, not a slow one. Reporting it as the timeout
        // would send the model to `preview_console_logs` for a fault that is in the host.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{tool} failed inside the browser runtime"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "{tool} timed out after {}s. {} The pane may be stuck (modal dialog, navigation hang, or unresponsive renderer). Check preview_console_logs for errors.",
            PREVIEW_TOOL_TIMEOUT.as_secs(),
            preview_pane_state(state, session_id)
        )),
    }
}

fn run_preview_page_tool(
    request: &ToolExecutionRequest,
    state: &AppState,
    tool: crate::browser::PreviewTool,
    grants: crate::browser::BrowserToolGrants,
    attachment_store: Option<&ImageAttachmentStore>,
) -> Result<Outcome, String> {
    let workspace = Path::new(&request.workspace_path);
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    match dispatch_preview_page_tool(state, &session_id, tool, &request.input, grants)? {
        crate::browser::PreviewToolOutput::Text(text) => Ok(Outcome::success(text)),
        crate::browser::PreviewToolOutput::Image(capture) => {
            preview_screenshot_outcome(capture, attachment_store)
        }
    }
}

/// The screenshot's pixels reach the model as a conversation image attachment, which is the only
/// transport Mework has for them; the text beside it is the frame those pixels are in.
fn preview_screenshot_outcome(
    capture: crate::browser::PreviewScreenshot,
    attachment_store: Option<&ImageAttachmentStore>,
) -> Result<Outcome, String> {
    let store = attachment_store.ok_or_else(|| {
        "preview_screenshot needs the conversation's attachment store to hand back pixels"
            .to_owned()
    })?;
    let bytes = base64::engine::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &capture.data,
    )
    .map_err(|error| format!("preview_screenshot produced undecodable image data: {error}"))?;
    // No extension: the store re-encodes every attachment into its own canonical container, so a
    // name claiming ".jpg" would disagree with the bytes the model is handed.
    let name = format!("preview-{}", Utc::now().format("%Y%m%dT%H%M%S%.3fZ"));
    let image = store.import(&name, &bytes)?;
    let output = serde_json::to_string_pretty(&json!({
        "width": capture.width,
        "height": capture.height,
    }))
    .map_err(|error| format!("Failed to encode screenshot result: {error}"))?;
    Ok(Outcome::success_with_images(output, vec![image]))
}

/// `preview_start`: starts the configured server the model named, hands back the one already
/// answering it, or attaches to the url an entry with no command points at, and points the
/// conversation's page at whichever it was.
///
/// The sentences are the source's, including the autoPort explanation and the attach report. What
/// the source words through its tab machinery — which surface the preview landed on — Mework says
/// in its own voice, because a page per conversation has no tab id to name.
fn run_preview_start(
    request: &ToolExecutionRequest,
    state: &AppState,
    selected: &crate::workspace_set::ResolvedWorkspace,
    workspace: &Path,
) -> Result<Outcome, String> {
    let name = required_string(&request.input, "name", 256, false)?;
    let session_id = crate::browser::preview_page_session_id(&request.conversation_id)?;
    // A workspace on an SSH machine runs its server there, and the conversation's page moves onto
    // that machine's network, so the `localhost` it opens is the machine's.
    if let Some(machine) = &selected.machine {
        let crate::model::RunTarget::Ssh { .. } = machine else {
            return Err(format!(
                "Workspace {} is on WSL, where previews are not supported yet.",
                selected.index
            ));
        };
        let remote = crate::preview_remote::RemoteMachine::new(
            selected.runner.clone(),
            crate::run_environment::env_key(Some(machine)),
            if selected.machine_label.trim().is_empty() {
                "the remote machine".to_owned()
            } else {
                selected.machine_label.clone()
            },
        );
        let started = crate::preview::start_remote(
            &state.preview_servers,
            &remote,
            &selected.root,
            Some(&name),
            Some(&request.conversation_id),
        )?;
        // Readiness is waited out on the machine; the page is pointed at the server once it
        // answers there, so the first thing it loads is the app rather than a refused connection
        // that raced the server a round trip early. A server slower than this is opened anyway,
        // the way a local one is.
        if let crate::preview::PreviewStartOutcome::Server { server, .. } = &started {
            let deadline = std::time::Instant::now() + REMOTE_READY_WAIT;
            while std::time::Instant::now() < deadline
                && state
                    .preview_servers
                    .get(&server.handle)
                    .is_some_and(|live| {
                        live.status == crate::preview_servers::PreviewServerStatus::Starting
                    })
            {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
        let proxy = crate::preview_tunnel::proxy_for(&remote)?;
        state.browser.set_network(
            &session_id,
            Some(crate::browser::PageNetwork {
                proxy,
                machine: remote.key().to_owned(),
            }),
        )?;
        return preview_start_receipt(state, &session_id, started, None, Some(remote.label()));
    }
    let started = crate::preview::start(
        &state.preview_servers,
        workspace,
        Some(&name),
        Some(&request.conversation_id),
    )?;
    // Back on this computer's network, if the page was last on another machine's.
    state.browser.set_network(&session_id, None)?;
    preview_start_receipt(state, &session_id, started, Some(workspace), None)
}

/// How long `preview_start` waits for a server on another machine to answer before it points the
/// page at it.
const REMOTE_READY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What `preview_start` reports, and the page pointed at what it started. `workspace` is the
/// directory whose configuration is re-read for the entry's url — a local one; a remote start
/// passes `machine` instead, and its receipt says where the port is.
///
/// A few sentences and no snapshot: the model named the server, so its id is the name it passed
/// and there is nothing to hand back but whether it worked. The one exception is a name the file
/// repeats, whose entries are numbered — then the id is news, and the receipt leads with it.
fn preview_start_receipt(
    state: &AppState,
    session_id: &str,
    started: crate::preview::PreviewStartOutcome,
    workspace: Option<&Path>,
    machine: Option<&str>,
) -> Result<Outcome, String> {
    let session_id = session_id.to_owned();
    let (server, reused) = match started {
        crate::preview::PreviewStartOutcome::Attached { attached } => {
            return preview_attach_outcome(state, &session_id, &attached);
        }
        crate::preview::PreviewStartOutcome::Server { server, reused } => (server, reused),
    };
    let configured = workspace.map(crate::preview::configurations);
    let entry = configured
        .as_ref()
        .zip(workspace)
        .and_then(|(configured, workspace)| {
            crate::preview::configured_entry(&state.preview_servers, workspace, configured, &server)
        });
    let mut lines = Vec::new();
    lines.extend(repeated_name_notice(&server.server_id, &server.name));
    let port = server.port;
    if reused {
        lines.push(
            "Server was already running and has been reused. No new process was started."
                .to_owned(),
        );
    } else if entry.is_some_and(|entry| entry.port != u32::from(port)) {
        let configured_port = entry.map_or(u32::from(port), |entry| entry.port);
        lines.push(format!(
            "Server started successfully. Configured port {configured_port} was in use, so port {port} was assigned instead (autoPort is enabled). The preview is available at http://localhost:{port}."
        ));
    } else {
        lines.push(format!("Server started successfully on port {port}."));
    }
    if let Some(machine) = machine {
        lines.push(format!(
            "The server runs on {machine}. The preview page uses that machine's network, so http://localhost:{port} in the page is the server there."
        ));
    }

    // A started server whose page still shows about:blank is a preview in name only, and this
    // surface has no navigation tool the model could correct that with.
    let configured_url = entry.and_then(|entry| entry.url.clone());
    let target = configured_url
        .clone()
        .unwrap_or_else(|| format!("http://localhost:{port}"));
    match state.browser.open_preview_at(&session_id, &target) {
        Ok(()) => {
            if let Some(url) = &configured_url {
                lines.push(format!("The preview opened at the configured url {url}."));
            }
        }
        Err(error) => lines.push(match &configured_url {
            Some(url) => format!("The configured url {url} could not be opened."),
            None => format!("The preview pane could not be pointed at {target}: {error}"),
        }),
    }
    Ok(Outcome::success(lines.join("\n")))
}

/// The sentence that hands the model a numbered id, for the entry of a name the file repeats;
/// `None` for every other entry, whose id is simply the name the model passed.
fn repeated_name_notice(server_id: &str, name: &str) -> Option<String> {
    (server_id != name).then(|| {
        format!(
            "This server's serverId is \"{server_id}\": .mework/launch.json has more than one server named \"{name}\", so each is numbered in the order it was first started. Pass \"{server_id}\" to address this one."
        )
    })
}

/// The attach form's receipt: the sentence saying no process is behind it, and where the page
/// went.
///
/// The navigation leg is `preview_start`'s own, so the configured url passes exactly the gate every
/// other preview url passes. A refusal is reported rather than raised, the way the source reports
/// it: the model is told the preview is still blank and can act on that.
fn preview_attach_outcome(
    state: &AppState,
    session_id: &str,
    attached: &crate::preview::PreviewAttachment,
) -> Result<Outcome, String> {
    let mut lines = Vec::new();
    lines.extend(repeated_name_notice(&attached.server_id, &attached.name));
    lines.push(crate::preview::ATTACHED_NOTICE.to_owned());
    let url = &attached.url;
    lines.push(match state.browser.open_preview_at(session_id, url) {
        Ok(()) => format!("The preview opened at the configured url {url}."),
        Err(_) => format!("The configured url {url} could not be opened."),
    });
    Ok(Outcome::success(lines.join("\n")))
}

/// The `workspace` a server-addressing call named, when the conversation has several; with one,
/// there is nothing it could narrow.
fn preview_workspace_argument(
    request: &ToolExecutionRequest,
    workspaces: &crate::workspace_set::WorkspaceSet,
) -> Result<Option<u32>, String> {
    Ok(workspace_argument(&request.input)?.filter(|_| workspaces.len() > 1))
}

/// `preview_stop`: kills one dev server and forgets it, buffered output included.
fn run_preview_stop(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspaces: &crate::workspace_set::WorkspaceSet,
) -> Result<Outcome, String> {
    let server_id = required_string(&request.input, "serverId", 256, false)?;
    let workspace = preview_workspace_argument(request, workspaces)?;
    let servers = addressable_preview_servers(state, workspaces, &request.conversation_id);
    let address = address_preview_server(&servers, &server_id, workspace);
    if let Some(refusal) = preview_address_refusal(&address, &server_id, workspace) {
        return Err(refusal);
    }
    let PreviewServerAddress::Found(found) = address else {
        // An attach entry's id names no process. Reporting it as a missing server would send the
        // model looking for one that never existed.
        if preview_attach_entry(workspaces, workspace, &server_id) {
            return Err(crate::preview::no_stop_for_attachment(&server_id));
        }
        return Err(format!("Server {server_id} not found"));
    };
    if state.preview_servers.stop(&found.server.handle) {
        return Ok(Outcome::success(format!(
            "Server {} stopped",
            found.server.server_id
        )));
    }
    Err(format!("Server {server_id} not found"))
}

/// `preview_list`: every dev server this conversation may address, each by the `serverId` — and,
/// with several workspaces, the `workspace` — the other preview tools take.
///
/// Processes only. An attach entry has none — nothing was started for it — so it is never listed
/// here; `preview_start`'s own receipt is what reports one, and the pane lists it from the
/// configuration file.
fn run_preview_list(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspaces: &crate::workspace_set::WorkspaceSet,
) -> Result<Outcome, String> {
    let servers = addressable_preview_servers(state, workspaces, &request.conversation_id);
    encode_browser_tool_result(&Value::Array(
        servers.iter().map(preview_server_view).collect(),
    ))
}

/// `preview_logs`: one dev server's buffered output, filtered and tail-sliced.
///
/// `serverId` is optional here for the same reason it is on the page tools: the source's own
/// dispatcher falls back to the first running or starting server — here, of the workspace named,
/// or of any when none is.
fn run_preview_logs(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspaces: &crate::workspace_set::WorkspaceSet,
) -> Result<Outcome, String> {
    let requested = optional_owned_string(&request.input, "serverId", 256)?;
    let workspace = preview_workspace_argument(request, workspaces)?;
    let servers = addressable_preview_servers(state, workspaces, &request.conversation_id);
    let resolved = match &requested {
        None => servers.iter().find(|candidate| {
            workspace.is_none_or(|number| candidate.workspace == Some(number))
                && matches!(
                    candidate.server.status,
                    crate::preview_servers::PreviewServerStatus::Running
                        | crate::preview_servers::PreviewServerStatus::Starting
                )
        }),
        Some(server_id) => {
            let address = address_preview_server(&servers, server_id, workspace);
            if let Some(refusal) = preview_address_refusal(&address, server_id, workspace) {
                return Err(refusal);
            }
            match address {
                PreviewServerAddress::Found(found) => Some(found),
                // An attach entry's id belongs to a page, and a page has no process output. The
                // source answers it with the same sentence as an id that resolves to nothing.
                _ if preview_attach_entry(workspaces, workspace, server_id) => None,
                _ => return Ok(Outcome::success("No logs yet.".to_owned())),
            }
        }
    };
    let Some(server) = resolved else {
        return Ok(Outcome::success(
            crate::preview::NO_SERVER_FOR_LOGS.to_owned(),
        ));
    };
    let level = optional_owned_string(&request.input, "level", 32)?;
    if level
        .as_deref()
        .is_some_and(|level| !matches!(level, "all" | "error"))
    {
        return Err("level must be one of \"all\", \"error\"".into());
    }
    let lines = optional_u64(&request.input, "lines", 50)?;
    Ok(Outcome::success(crate::preview::logs(
        &state.preview_servers,
        &server.server.handle,
        &crate::preview_servers::PreviewLogQuery {
            errors_only: level.as_deref() == Some("error"),
            search: optional_owned_string(&request.input, "search", 512)?,
            lines: Some(u32::try_from(lines).unwrap_or(u32::MAX)),
        },
    )))
}

/// Uploads a transcript image attachment to the page's file input. The run
/// loop has already resolved `image_id` to a stored attachment; this side
/// materializes the bytes as a single-use temp file, hands the real path to
/// the browser runtime through grants, and deletes the file afterwards. The
/// model never supplies a filesystem path.
pub(crate) fn execute_browser_upload_image(
    request: &ToolExecutionRequest,
    state: &AppState,
    app_data: &Path,
    image: &ImageAttachment,
    profile: &PromptProfile,
) -> ToolExecutionResponse {
    let started = Instant::now();
    let outcome =
        run_browser_upload_image(request, state, app_data, image).unwrap_or_else(|error| Outcome {
            success: false,
            output: error,
            images: Vec::new(),
            diff: None,
            opened_file: None,
            file_touch: None,
        });
    finish_execution(request, state, started, outcome, profile)
}

fn run_browser_upload_image(
    request: &ToolExecutionRequest,
    state: &AppState,
    app_data: &Path,
    image: &ImageAttachment,
) -> Result<Outcome, String> {
    let workspace = Path::new(&request.workspace_path);
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    let bytes = ImageAttachmentStore::new(app_data).read_bytes(image)?;
    let file_name = upload_image_file_name(&request.input, image)?;
    let staging = create_upload_image_staging(image)?;
    let file_path = staging.join(&file_name);
    let upload = fs::write(&file_path, &bytes)
        .map_err(|error| format!("Failed to materialize image attachment for upload: {error}"))
        .and_then(|()| {
            dispatch_preview_page_tool(
                state,
                &session_id,
                crate::browser::PreviewTool::UploadImage,
                &request.input,
                crate::browser::BrowserToolGrants {
                    upload_paths: Some(vec![file_path.clone()]),
                },
            )
        });
    // Single-use materialization: whether the page consumed the bytes or the
    // upload failed, nothing may linger in the temp directory.
    let _ = fs::remove_dir_all(&staging);
    let crate::browser::PreviewToolOutput::Text(text) = upload? else {
        return Err("preview_upload_image answered with pixels instead of a receipt".into());
    };
    let mut receipt = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => object,
        _ => Map::from_iter([("result".to_owned(), Value::String(text))]),
    };
    receipt.insert(
        "imageId".into(),
        image
            .short_id
            .map_or_else(|| Value::String(image.id.clone()), Value::from),
    );
    receipt.insert("name".into(), Value::String(file_name));
    serde_json::to_string_pretty(&Value::Object(receipt))
        .map(Outcome::success)
        .map_err(|error| format!("Failed to encode image upload result: {error}"))
}

/// Page-visible file name for the upload: the optional `filename` argument or
/// the attachment's own name, mapped onto Windows-safe basename characters.
/// The page reads the on-disk basename via `DOM.setFileInputFiles`, so the
/// sanitized value is also the staging file's real name.
fn upload_image_file_name(input: &JsonObject, image: &ImageAttachment) -> Result<String, String> {
    let requested = optional_owned_string(input, "filename", 128)?;
    let mut name: String = requested
        .as_deref()
        .unwrap_or(&image.name)
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    while name.ends_with(['.', ' ']) {
        name.pop();
    }
    while name.starts_with(['.', ' ']) {
        name.remove(0);
    }
    if name.is_empty() {
        return Err("filename cannot be empty or contain only invalid characters".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        name = format!("image-{name}");
    }
    if !name.contains('.') {
        let extension = match image.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            _ => "webp",
        };
        name = format!("{name}.{extension}");
    }
    if name.len() > 160 {
        return Err("filename exceeds the 160-byte limit".into());
    }
    Ok(name)
}

fn create_upload_image_staging(image: &ImageAttachment) -> Result<PathBuf, String> {
    static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let digest_prefix = image.id.get(..16).unwrap_or(&image.id);
    let staging = std::env::temp_dir().join(format!(
        "mework-upload-image-{}-{sequence}-{digest_prefix}",
        std::process::id()
    ));
    fs::create_dir_all(&staging)
        .map_err(|error| format!("Failed to create image upload staging directory: {error}"))?;
    Ok(staging)
}
fn run_ls(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
) -> Result<String, String> {
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let depth = optional_u64(input, "depth", 1)?;
    if depth > 8 {
        return Err("Recursive depth cannot exceed 8".into());
    }
    let workspace = canonical_workspace(workspace)?;
    let root = resolve_existing_with_scope(&workspace, &path, scope)?;
    if !root.is_dir() {
        return Err(format!("ls target is not a directory: {path}"));
    }
    let rules = search_scope::local_rules(&root);

    // Breadth-first, a level at a time and each directory in name order, so
    // the budget cuts the deepest level reached and a big subtree cannot push
    // its siblings out of the answer.
    let mut listing = search_scope::Listing::new();
    let mut frontier = vec![root.clone()];
    'levels: for level in 1..=(depth as usize + 1) {
        let mut next = Vec::new();
        for directory in &frontier {
            let mut children = match fs::read_dir(directory) {
                Ok(children) => children.filter_map(Result::ok).collect::<Vec<_>>(),
                Err(error) if directory == &root => {
                    return Err(format!("Failed to list directory {path}: {error}"))
                }
                Err(error) => {
                    listing.skipped(format!("{}: {error}", display_path(&workspace, directory)));
                    continue;
                }
            };
            children.sort_by_key(|child| child.file_name());
            for child in children {
                let child_path = child.path();
                if !existing_path_is_allowed(scope, &child_path) {
                    continue;
                }
                // A link to a directory is listed, not followed, and so not
                // marked as one.
                let is_dir = child.file_type().is_ok_and(|kind| kind.is_dir());
                let relative = search_relative(&root, &child_path);
                let collapsed = is_dir && rules.collapses(&relative);
                if !listing.push(
                    level,
                    &display_path(&workspace, &child_path),
                    is_dir,
                    collapsed,
                    profile,
                ) {
                    break 'levels;
                }
                if is_dir && !collapsed {
                    next.push(child_path);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(listing.render(profile))
}

/// A walked path relative to the directory a search started from,
/// `/`-separated: the spelling the ignore rules are written in.
fn search_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn run_grep(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
) -> Result<String, String> {
    let pattern = required_string(input, "pattern", 4096, false)?;
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let case_sensitive = optional_bool(input, "case_sensitive", false)?;
    let page = search_scope::GrepPage::from_input(input)?;
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| format!("Invalid regular expression: {error}"))?;
    let workspace = canonical_workspace(workspace)?;
    let root = resolve_existing_with_scope(&workspace, &path, scope)?;

    let mut search = GrepSearch {
        workspace: &workspace,
        regex: &regex,
        wanted: page.wanted(),
        matches: Vec::new(),
        skipped: Vec::new(),
    };
    if !root.is_dir() {
        // A file named outright is searched whatever the ignore rules say.
        search.file(&root);
    } else {
        match search_scope::local_search_files(&root) {
            search_scope::SearchFiles::Listed(files) => {
                // Git never lists what is behind a link, but its index can
                // still name a path whose directory has since become one;
                // the walk this replaces followed no links, and neither does
                // this.
                let mut linked = std::collections::HashMap::new();
                for file in files {
                    if search_scope::has_linked_parent(&root, &file, &mut linked) {
                        continue;
                    }
                    let file = root.join(file);
                    if existing_path_is_allowed(scope, &file) && !search.file(&file) {
                        break;
                    }
                }
            }
            search_scope::SearchFiles::Walk(rules) => {
                let mut walk = WalkDir::new(&root)
                    .follow_links(false)
                    .sort_by_file_name()
                    .into_iter();
                while let Some(entry) = walk.next() {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            search.skipped.push(error.to_string());
                            continue;
                        }
                    };
                    if entry.depth() == 0 {
                        continue;
                    }
                    let is_dir = entry.file_type().is_dir();
                    if !existing_path_is_allowed(scope, entry.path())
                        || (is_dir && rules.collapses(&search_relative(&root, entry.path())))
                    {
                        if is_dir {
                            walk.skip_current_dir();
                        }
                        continue;
                    }
                    if entry.file_type().is_file() && !search.file(entry.path()) {
                        break;
                    }
                }
            }
        }
    }
    let GrepSearch {
        matches, skipped, ..
    } = search;
    Ok(page.render(matches, skipped, profile))
}

/// One `grep` call's search: the matches found so far, up to what the page
/// needs.
struct GrepSearch<'a> {
    workspace: &'a Path,
    regex: &'a regex::Regex,
    wanted: usize,
    matches: Vec<String>,
    skipped: Vec<String>,
}

impl GrepSearch<'_> {
    /// Searches one file; `false` once the page has all it needs. A link, a
    /// file over 2 MiB and a binary file (a NUL in its first 8 KiB) are
    /// passed over.
    fn file(&mut self, path: &Path) -> bool {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return true;
        };
        if !metadata.is_file() || metadata.len() > MAX_TEXT_FILE {
            return true;
        }
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.skipped
                    .push(format!("{}: {error}", display_path(self.workspace, path)));
                return true;
            }
        };
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return true;
        }
        let content = String::from_utf8_lossy(&bytes);
        let relative = display_path(self.workspace, path);
        for (line_index, line) in content.lines().enumerate() {
            if self.regex.is_match(line) {
                self.matches.push(format!(
                    "{}:{}:{}",
                    relative,
                    line_index + 1,
                    truncate_chars(line, 500)
                ));
                if self.matches.len() >= self.wanted {
                    return false;
                }
            }
        }
        true
    }
}

fn run_find(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
) -> Result<String, String> {
    let query = required_string(input, "query", 1024, false)?;
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let matcher = Glob::new(&query)
        .map_err(|error| format!("Invalid glob pattern: {error}"))?
        .compile_matcher();
    let workspace = canonical_workspace(workspace)?;
    let root = resolve_existing_with_scope(&workspace, &path, scope)?;
    // Only for ranking: `find` hides nothing Git ignores.
    let rules = search_scope::local_rules(&root);

    let mut found = search_scope::FindMatches::default();
    let mut scanned = 0_usize;
    let mut walk = WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = walk.next() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let is_dir = entry.file_type().is_dir();
        if !existing_path_is_allowed(scope, entry.path()) {
            if is_dir {
                walk.skip_current_dir();
            }
            continue;
        }
        scanned += 1;
        if scanned > search_scope::FIND_SCAN_LIMIT {
            found.scan_cut();
            break;
        }
        // Version-control data can be found by name but is not walked.
        if is_dir
            && entry
                .file_name()
                .to_str()
                .is_some_and(search_scope::is_vcs_directory)
        {
            walk.skip_current_dir();
        }
        let relative_to_root = entry.path().strip_prefix(&root).unwrap_or(entry.path());
        let matches = matcher.is_match(relative_to_root)
            || entry
                .path()
                .file_name()
                .is_some_and(|name| matcher.is_match(Path::new(name)));
        if !matches {
            continue;
        }
        let mut display = display_path(&workspace, entry.path());
        if is_dir {
            display.push('/');
        }
        let ignored = rules.ignores(&search_relative(&root, entry.path()), is_dir);
        found.push(display, ignored);
    }
    Ok(found.render(profile))
}

pub(crate) fn display_path(workspace: &Path, path: &Path) -> String {
    let path = relative_display(workspace, path);
    let display = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        if let Some(unc) = display.strip_prefix("//?/UNC/") {
            return format!("//{unc}");
        }
        if let Some(ordinary) = display.strip_prefix("//?/") {
            return ordinary.to_owned();
        }
    }
    display
}

/// `display_path` for a canonical `path` and a workspace spelled however the
/// caller has it: the record keys are canonical, so the workspace is brought
/// to the same form before the prefix is stripped.
pub(crate) fn display_recorded_path(workspace: &Path, path: &Path) -> String {
    match canonical_workspace(workspace) {
        Ok(canonical) => display_path(&canonical, path),
        Err(_) => display_path(workspace, path),
    }
}

/// The `lsp` tool: resolve the file, then ask whichever language server claims
/// it.
///
/// The path guard runs first and its answer is what the server is told about,
/// so a `filePath` that escapes the scope never reaches a child process. The
/// file is opened here only to prove it exists and is readable inside the
/// scope; the contents the server gets are read by [`crate::lsp`] from the same
/// canonical path.
fn run_lsp(
    workspace: &Path,
    request: &ToolExecutionRequest,
    scope: &ExecutionScope,
    state: &AppState,
    profile: &PromptProfile,
) -> Result<String, String> {
    let input = &request.input;
    let operation = required_string(input, "operation", 64, false)?;
    let requested = required_string(input, "filePath", MAX_PATH_CHARS, false)?;
    let line = optional_u64_value(input, "line")?
        .ok_or_else(|| "line is required; it is 1-based, as shown in editors".to_owned())?;
    let character = optional_u64_value(input, "character")?
        .ok_or_else(|| "character is required; it is 1-based, as shown in editors".to_owned())?;
    let query = match input.get("query") {
        None | Some(Value::Null) => None,
        Some(_) => Some(optional_string(input, "query", "", 1024, true)?),
    };

    let call = crate::lsp::parse_call(&operation, requested.clone(), line, character, query)?;
    let (_file, path) = crate::path_guard::secure_open_existing_file_with_scope(
        workspace,
        &requested,
        scope,
    )?;

    let configs = crate::lsp_config::servers_for_workspace(workspace, profile.language);
    crate::lsp::execute(
        &state.lsp_servers,
        &crate::lsp_servers::ServerHost::Local,
        &configs,
        workspace,
        &path,
        &call,
        &request.conversation_id,
        &crate::lsp::LocalFiles {
            path: &path,
            requested: &requested,
        },
    )
}

/// The line range a `read` asked for, validated. `end_line` is `u64::MAX` when
/// the call did not bound it.
pub(crate) fn parse_read_range(input: &JsonObject) -> Result<(u64, u64), String> {
    let start_line = optional_u64(input, "start_line", 1)?;
    let end_line = optional_u64_value(input, "end_line")?;
    if start_line == 0 {
        return Err("start_line must begin at 1".into());
    }
    let end_line = end_line.unwrap_or(u64::MAX);
    if end_line < start_line {
        return Err("end_line cannot be less than start_line".into());
    }
    if end_line != u64::MAX && end_line.saturating_sub(start_line) > 5_000 {
        return Err("A single read cannot exceed 5001 lines".into());
    }
    Ok((start_line, end_line))
}

/// What a text `read` shows for its range.
pub(crate) struct TextSlice {
    pub output: String,
    /// Whether the whole file was in front of the model: no range, and the
    /// line cap did not cut it. Only such a read can vouch for the file.
    pub whole_file: bool,
}

/// Lines a `read` without `end_line` returns: Claude Code's default.
pub(crate) const READ_DEFAULT_LINES: usize = 2_000;
/// Bytes of text one `read` returns, whatever its range; a range past it is
/// cut at a line boundary and says where to continue.
pub(crate) const READ_MAX_BYTES: usize = 60 * 1024;

/// Slices `content` to the validated range: [`READ_DEFAULT_LINES`] when no
/// end was given, and never more than [`READ_MAX_BYTES`] of whole lines.
/// Shared by the host and remote legs so a file reads the same whichever
/// machine it is on.
pub(crate) fn slice_text_lines(
    content: &str,
    start_line: u64,
    end_line: u64,
    profile: &PromptProfile,
) -> Result<TextSlice, String> {
    let lines = content.lines().collect::<Vec<_>>();
    let start_index = (start_line - 1).min(usize::MAX as u64) as usize;
    if start_index >= lines.len() {
        // Only an empty file reaches here from line 1, and that is a full read
        // of it; any other start is a slice that saw nothing.
        return Ok(TextSlice {
            output: profile.text(PromptKey::ToolReadRangeOutOfBounds).to_owned(),
            whole_file: start_line == 1 && end_line == u64::MAX,
        });
    }
    let wanted_end = if end_line == u64::MAX {
        lines.len().min(start_index.saturating_add(READ_DEFAULT_LINES))
    } else {
        (end_line.min(lines.len() as u64)) as usize
    };
    let mut end_index = start_index;
    let mut bytes = 0_usize;
    while end_index < wanted_end {
        let cost = lines[end_index].len() + 1;
        if bytes + cost > READ_MAX_BYTES {
            if end_index == start_index {
                return Err(profile.render(
                    PromptKey::ToolReadLineTooLong,
                    &[
                        ("line", &(end_index + 1).to_string()),
                        (
                            "size",
                            &crate::tool_output::human_size(lines[end_index].len() as u64),
                        ),
                    ],
                ));
            }
            break;
        }
        bytes += cost;
        end_index += 1;
    }
    let mut output = lines[start_index..end_index].join("\n");
    // Short of what was asked for — the byte budget — or, with no end given,
    // short of the file's end.
    let stopped_early =
        end_index < wanted_end || (end_line == u64::MAX && end_index < lines.len());
    if stopped_early {
        output.push_str(&profile.render(
            PromptKey::ToolReadLimit,
            &[
                ("from", &start_line.to_string()),
                ("to", &end_index.to_string()),
                ("total", &lines.len().to_string()),
                ("next", &(end_index + 1).to_string()),
            ],
        ));
    }
    Ok(TextSlice {
        output,
        whole_file: start_line == 1 && end_line == u64::MAX && end_index >= lines.len(),
    })
}

fn run_read(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    attachment_store: Option<&ImageAttachmentStore>,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<Outcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let (start_line, end_line) = parse_read_range(input)?;

    let (mut file, file_path) = secure_open_existing_file_with_scope(workspace, &path, scope)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Failed to read file metadata: {error}"))?;
    let mut bytes = Vec::with_capacity(12);
    std::io::Read::by_ref(&mut file)
        .take(12)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Failed to inspect file {}: {error}", file_path.display()))?;
    if is_supported_image(&bytes) {
        if input.contains_key("start_line") || input.contains_key("end_line") {
            return Err("read does not accept start_line or end_line when reading an image".into());
        }
        if metadata.len() > crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64 {
            return Err(format!(
                "Image exceeds the {} MiB limit ({} bytes)",
                crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024,
                metadata.len()
            ));
        }
        file.take(
            u64::try_from(crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1)
                .saturating_sub(bytes.len() as u64),
        )
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Failed to read file {}: {error}", file_path.display()))?;
        if bytes.len() > crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES {
            return Err(format!(
                "Image exceeds the {} MiB limit (at least {} bytes)",
                crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024,
                bytes.len()
            ));
        }
        let store = attachment_store.ok_or_else(|| {
            "read requires a trusted image attachment directory to read an image".to_owned()
        })?;
        let name = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image");
        let image = store.import(name, &bytes)?;
        return Ok(Outcome::success_with_images(
            profile.render(
                PromptKey::ToolReadImage,
                &[
                    ("path", &display_path(workspace, &file_path)),
                    ("mime", &image.mime),
                    ("width", &image.width.to_string()),
                    ("height", &image.height.to_string()),
                    ("bytes", &image.bytes.to_string()),
                ],
            ),
            vec![image],
        )
        .with_opened_file(file_path));
    }
    let content = read_text_file_handle(&mut file)?;
    // The record takes the modification time the handle reported before the
    // bytes were read. If the file changes between the two, the record is
    // older than the disk and the next edit reads as stale — the safe side.
    let modified_ms = file_read_state::modified_ms_of(&metadata);
    let slice = slice_text_lines(&content, start_line, end_line, profile)?;
    // A full record needs the whole file in front of the model. Anything less
    // is remembered, but vouches for nothing about the parts it did not show.
    let record = if slice.whole_file {
        FileReadRecord::full_read(modified_ms, file_read_state::normalize_text(&content))
    } else {
        FileReadRecord::partial_read(modified_ms)
    };
    Ok(Outcome::success(slice.output)
        .with_file_touch(read_touch(file_guard, &file_path, record))
        .with_opened_file(file_path))
}

/// What a `read` hands back for the run loop to commit once the result is
/// final, or nothing when the call runs without a guard.
fn read_touch(
    file_guard: Option<FileGuardContext<'_>>,
    path: &Path,
    record: FileReadRecord,
) -> Option<FileGuardTouch> {
    file_guard.map(|_| FileGuardTouch {
        path: path.to_path_buf(),
        read: Some(record),
    })
}

/// Whole milliseconds of `path`'s modification time right now, for the record a
/// writer leaves behind. A file that cannot be stat'ed after its own write is
/// remembered at the epoch: the next stat can only be newer, so the next edit
/// asks for a read rather than trusting a record nobody could take.
fn modified_ms_after_write(path: &Path) -> i64 {
    file_read_state::modified_ms(path).unwrap_or(0)
}

/// The read-gate half of `write` and `edit`: whether an existing file may be
/// overwritten given what this conversation remembers reading.
///
/// Returns the record the file had, so the caller can decide what the model
/// still knows after the write. `find` is the edit's search text; with it, a
/// stale file whose text still contains that exact text once is let through
/// as Claude Code's stale recovery, and the caller is told so.
fn check_write_gate(
    guard: &FileGuardContext<'_>,
    path: &Path,
    disk_content: &str,
    find: Option<&str>,
) -> Result<(Option<FileReadRecord>, bool), String> {
    let Some(record) = guard.get(path) else {
        return Err(FILE_NOT_READ.into());
    };
    let Some(disk_ms) = file_read_state::modified_ms(path) else {
        return Ok((Some(record), false));
    };
    if disk_ms <= record.modified_ms {
        return Ok((Some(record), false));
    }
    let normalized = file_read_state::normalize_text(disk_content);
    // A touched-but-identical file — a save with no change, a checkout of the
    // same text — is not stale: the model's copy is still the current one.
    if record.matches(&normalized) {
        return Ok((Some(record), false));
    }
    if let Some(find) = find {
        let find = file_read_state::normalize_text(find);
        if !find.is_empty() && normalized.matches(find.as_str()).count() == 1 {
            return Ok((Some(record), true));
        }
    }
    Err(FILE_MODIFIED_SINCE_READ.into())
}

/// The note a writer appends to its receipt. Claude Code adds "(file state is
/// current in your context — no need to Read it back)" after every write so
/// the model does not re-read what it just wrote; the stale-recovery variant
/// instead warns that the file holds changes the model has not seen.
pub(crate) fn write_receipt_note(profile: &PromptProfile, stale_recovered: bool) -> String {
    if stale_recovered {
        profile.text(PromptKey::ToolEditStaleRecovered).to_owned()
    } else {
        profile.text(PromptKey::ToolFileStateCurrent).to_owned()
    }
}

fn run_write(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<Outcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let content = required_string(input, "content", MAX_WRITE_BYTES, true)?;
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Write content exceeds the {} MiB limit",
            MAX_WRITE_BYTES / 1024 / 1024
        ));
    }
    let file_path = resolve_for_write_with_scope(workspace, &path, scope)?;
    // Diff metadata is best-effort for `write`: overwriting a large or non-UTF-8
    // file was valid before diffs existed and must remain valid. A missing target
    // is represented as an empty old side with `/dev/null` in the diff header.
    let before = match fs::metadata(&file_path) {
        Ok(metadata) if metadata.is_file() => read_text_file(&file_path)
            .ok()
            .map(|content| (content, false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some((String::new(), true)),
        _ => None,
    };
    // A new file needs no prior read; an existing one is gated on what the
    // conversation remembers of it. A file that exists but could not be read as
    // text is gated on its time alone: there is no text to rescue it by.
    let mut note = String::new();
    if let Some(guard) = file_guard {
        let existing = match &before {
            Some((existing, false)) => Some(existing.as_str()),
            Some((_, true)) => None,
            None => Some(""),
        };
        if let Some(existing) = existing {
            check_write_gate(&guard, &file_path, existing, None)?;
        }
        note = write_receipt_note(profile, false);
    }
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create parent directory: {error}"))?;
    }
    atomic_write(&file_path, content.as_bytes())?;
    let touch = file_guard.map(|guard| {
        // The model wrote every byte, so its copy is the current one.
        guard.record(
            file_path.clone(),
            FileReadRecord::written(
                modified_ms_after_write(&file_path),
                file_read_state::normalize_text(&content),
                true,
            ),
        );
        FileGuardTouch {
            path: file_path.clone(),
            read: None,
        }
    });
    let diff = before.and_then(|(before, created)| unified_diff(&path, &before, &content, created));
    Ok(Outcome::success_with_diff(
        format!(
            "{}{note}",
            profile.render(
                PromptKey::ToolWriteDone,
                &[("bytes", &content.len().to_string()), ("path", &path)],
            )
        ),
        diff,
    )
    .with_file_touch(touch))
}

fn run_edit(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<Outcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let find = required_string(input, "find", MAX_WRITE_BYTES, true)?;
    if find.is_empty() {
        return Err("Parameter find cannot be empty".into());
    }
    let replace = required_string(input, "replace", MAX_WRITE_BYTES, true)?;
    let file_path = resolve_existing_with_scope(workspace, &path, scope)?;
    if !file_path.is_file() {
        return Err(format!(
            "edit target is not a file: {}",
            file_path.display()
        ));
    }
    let content = read_text_file(&file_path)?;
    let (previous, stale_recovered) = match file_guard {
        Some(guard) => check_write_gate(&guard, &file_path, &content, Some(&find))?,
        None => (None, false),
    };
    let next = apply_edit(&content, &find, &replace)?;
    if next.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Edited file exceeds the {} MiB limit",
            MAX_WRITE_BYTES / 1024 / 1024
        ));
    }
    let diff = unified_diff(&path, &content, &next, false);
    atomic_write(&file_path, next.as_bytes())?;
    let mut note = String::new();
    let touch = file_guard.map(|guard| {
        // After an edit the model knows the file only if it knew it before:
        // a full read it has seen, and no other changes applied on top. A
        // stale-recovered edit or a file never read leaves it holding a copy
        // that is not the current one.
        let in_model_context = !stale_recovered
            && previous
                .as_ref()
                .is_some_and(|record| record.full && record.in_model_context);
        guard.record(
            file_path.clone(),
            FileReadRecord::written(
                modified_ms_after_write(&file_path),
                file_read_state::normalize_text(&next),
                in_model_context,
            ),
        );
        note = write_receipt_note(profile, stale_recovered);
        FileGuardTouch {
            path: file_path.clone(),
            read: None,
        }
    });
    Ok(Outcome::success_with_diff(
        format!(
            "{}{note}",
            profile.render(PromptKey::ToolEditDone, &[("path", &path)])
        ),
        diff,
    )
    .with_file_touch(touch))
}

/// Replaces exactly one occurrence of `find` in `content`.
///
/// The raw text is tried first, so a file with mixed line endings is left
/// exactly as it was outside the replaced span. Only when that finds nothing
/// does the edit fall back to Claude Code's rule: match with byte-order mark
/// and carriage returns stripped from both sides, then write the result back
/// in the file's own convention — CRLF when CRLF wins the vote over the first
/// 4096 characters, a byte-order mark when the file had one. Without that
/// fallback a CRLF file could only be edited with find text spelled in CRLF,
/// which is not how a model that saw it through `read` would spell it.
pub(crate) fn apply_edit(content: &str, find: &str, replace: &str) -> Result<String, String> {
    let raw_occurrences = content.matches(find).count();
    if raw_occurrences == 1 {
        return Ok(content.replacen(find, replace, 1));
    }
    if raw_occurrences > 1 {
        return Err(format!(
            "The search text occurs {raw_occurrences} times; edit requires exactly one match"
        ));
    }
    let has_bom = content.starts_with('\u{feff}');
    let normalized_find = file_read_state::normalize_text(find);
    if !has_bom && !content.contains('\r') && normalized_find == find {
        return Err("The exact text to replace was not found".into());
    }
    let normalized = file_read_state::normalize_text(content);
    let occurrences = normalized.matches(normalized_find.as_str()).count();
    if occurrences == 0 {
        return Err("The exact text to replace was not found".into());
    }
    if occurrences > 1 {
        return Err(format!(
            "The search text occurs {occurrences} times; edit requires exactly one match"
        ));
    }
    let edited = normalized.replacen(
        normalized_find.as_str(),
        &file_read_state::normalize_text(replace),
        1,
    );
    let edited = if crlf_dominant(content) {
        edited.replace('\n', "\r\n")
    } else {
        edited
    };
    Ok(if has_bom {
        format!("\u{feff}{edited}")
    } else {
        edited
    })
}

/// Claude Code's line-ending vote: CRLF when the first 4096 characters hold
/// more CRLF than bare LF line ends.
pub(crate) fn crlf_dominant(content: &str) -> bool {
    let sample: String = content.chars().take(4096).collect();
    let crlf = sample.matches("\r\n").count();
    let lf = sample.matches('\n').count() - crlf;
    crlf > lf
}

pub(crate) fn unified_diff(path: &str, before: &str, after: &str, created: bool) -> Option<String> {
    if before == after {
        return None;
    }
    // Header values are accepted verbatim by `similar`; keep a malicious or
    // unusual filename from injecting extra unified-diff control lines.
    let display_path = path.replace('\\', "/").replace(['\r', '\n', '\t'], " ");
    let old_header = if created { "/dev/null" } else { &display_path };
    let text_diff = TextDiff::from_lines(before, after);
    Some(
        text_diff
            .unified_diff()
            .context_radius(3)
            .header(old_header, &display_path)
            .to_string(),
    )
}

/// The shell a shell-tool call runs in: the backend its tool is named for.
pub(crate) use crate::shell_backend::ShellBackend as ShellKind;

/// What a shell call in `workspace` starts for `kind`: the program the
/// machine's probe found, or `None` on the host, whose own resolvers (with
/// their fallbacks) pick the interpreter at launch.
///
/// A backend the machine does not have is refused here. The schema already
/// lists only the workspaces whose machine has the tool's shell, so this is
/// what a manual call, or a machine that lost the shell since, runs into.
pub(crate) fn shell_program(
    workspace: &crate::workspace_set::ResolvedWorkspace,
    kind: ShellKind,
) -> Result<Option<String>, String> {
    let Some(shell) = workspace.shell(kind) else {
        let machine = if workspace.machine_label.trim().is_empty() {
            "this machine".to_owned()
        } else {
            workspace.machine_label.clone()
        };
        let others = workspace
            .shells
            .iter()
            .map(|shell| shell.backend.tool_name())
            .collect::<Vec<_>>();
        return Err(if others.is_empty() {
            format!(
                "Workspace {} ({machine}) has no {} and no other shell Mework can run",
                workspace.index,
                kind.display_name()
            )
        } else {
            format!(
                "Workspace {} ({machine}) has no {}; use one of these tools there instead: {}",
                workspace.index,
                kind.display_name(),
                others.join(", ")
            )
        });
    };
    Ok((!workspace.is_local()).then(|| shell.path.clone()))
}

/// Validates the one required shell parameter. Shared with the background leg
/// in the run loop, which parses before it spawns anything.
pub(crate) fn parse_shell_command(input: &JsonObject) -> Result<String, String> {
    required_string(input, "command", MAX_COMMAND_CHARS, false)
}

/// How long a foreground command runs before the host stops waiting for it.
/// Claude Code's default, and its ceiling.
pub(crate) const SHELL_DEFAULT_TIMEOUT: Duration = Duration::from_millis(120_000);
pub(crate) const SHELL_MAX_TIMEOUT: Duration = Duration::from_millis(600_000);

/// The deadline this call runs under, clamped like Claude Code clamps it:
/// `min(requested or default, maximum)`. An absent, zero, or negative value is
/// the default rather than an error — a bad number should not cost the model a
/// round, and the ceiling is the real protection.
pub(crate) fn parse_shell_timeout(input: &JsonObject) -> Duration {
    let requested = input.get("timeout").and_then(|value| {
        value
            .as_f64()
            // A model that sends "120000" instead of 120000 means the same thing.
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    });
    match requested {
        Some(millis) if millis.is_finite() && millis > 0.0 => {
            Duration::from_millis(millis as u64).min(SHELL_MAX_TIMEOUT)
        }
        _ => SHELL_DEFAULT_TIMEOUT,
    }
}

/// Whether the call explicitly selects the background leg. Only `true` selects
/// it; anything else is the ordinary synchronous call.
pub(crate) fn shell_run_in_background_requested(input: &JsonObject) -> bool {
    input.get("run_in_background") == Some(&Value::Bool(true))
}

/// A spawned shell process together with the job object that can kill its
/// whole tree. Handed to the background worker thread; `ShellJob` is `Send`.
pub(crate) struct SpawnedShell {
    pub child: ShellChild,
    pub job: ShellJob,
}

/// The process a shell call runs: a child of this host, or a process the
/// agent runs for it on an SSH machine ([`crate::remote_link`]).
///
/// Both answer the same questions the call asks — its output pipes, whether it
/// has ended, how to end it — so the synchronous leg, the hand-off to the task
/// surface at the deadline and the background leg treat them alike. A remote
/// process whose link drops is simply still running as far as this side
/// knows; its output resumes when the link does.
pub(crate) enum ShellChild {
    Local(std::process::Child),
    Remote {
        child: crate::remote_link::AgentChild,
        /// Set once a kill was requested, which bounds how long a later wait
        /// may take to hear it confirmed.
        killed: bool,
    },
}

/// How long a remote kill is waited on before the call moves on. The agent
/// carries out a kill that reaches it late, and reclaims the process by itself
/// if the host never comes back; the call does not need to wait for either.
const REMOTE_KILL_CONFIRMATION: Duration = Duration::from_secs(5);

impl ShellChild {
    pub(crate) fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Local(child) => child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
            Self::Remote { child, .. } => child
                .process
                .take_stdout()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        }
    }

    pub(crate) fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Local(child) => child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
            Self::Remote { child, .. } => child
                .process
                .take_stderr()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        }
    }

    /// The process's input. A remote process's writer queues what it is
    /// given and delivers it in order across reconnects.
    pub(crate) fn take_stdin(&mut self) -> Option<Box<dyn std::io::Write + Send>> {
        match self {
            Self::Local(child) => child
                .stdin
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Write + Send>),
            Self::Remote { child, .. } => {
                Some(Box::new(child.process.stdin()) as Box<dyn std::io::Write + Send>)
            }
        }
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::Local(child) => child.try_wait(),
            Self::Remote { .. } => self.wait_timeout(Duration::ZERO),
        }
    }

    /// How long an ended process ran, as the machine that ran it measured it: the agent reports
    /// that with the exit. `None` for a child of this host, whose span the registry measures
    /// itself, for a process still running, and for a remote kill whose exit never arrived.
    pub(crate) fn measured_runtime(&self) -> Option<Duration> {
        match self {
            Self::Local(_) => None,
            Self::Remote { child, .. } => child
                .process
                .try_wait()
                .ok()
                .flatten()
                .and_then(|exit| exit.runtime_ms)
                .map(Duration::from_millis),
        }
    }

    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::Local(child) => ChildExt::wait_timeout(child, timeout),
            Self::Remote { child, .. } => Ok(child
                .process
                .wait_timeout(timeout)?
                .map(|exit| crate::remote_link::exit_status(&exit))),
        }
    }

    pub(crate) fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::Local(child) => child.wait(),
            Self::Remote { child, killed } => {
                if !*killed {
                    return child
                        .process
                        .wait()
                        .map(|exit| crate::remote_link::exit_status(&exit));
                }
                match child.process.wait_timeout(REMOTE_KILL_CONFIRMATION) {
                    Ok(Some(exit)) => Ok(crate::remote_link::exit_status(&exit)),
                    // Not confirmed yet — the link is down, or the tree takes
                    // a moment to die. Report it as killed; the agent finishes
                    // the job either way.
                    Ok(None) | Err(_) => Ok(crate::remote_link::exit_status(
                        &remote_agent::protocol::ExitInfo {
                            code: None,
                            signal: Some(9),
                            reason: remote_agent::protocol::ExitReason::Signalled,
                            ends: Default::default(),
                            runtime_ms: None,
                        },
                    )),
                }
            }
        }
    }

}

/// Claude Code's PowerShell prologue, byte for byte.
///
/// Note how little it does: an `Out-File` encoding default, `$OutputEncoding`,
/// and plain-text rendering — the last two only under `FullLanguage`, because a
/// constrained runspace cannot assign them. It does **not** touch the
/// `[Console]` encodings, the file cmdlets' own defaults, `$ProgressPreference`,
/// or the console width. Those omissions are load-bearing for parity and are
/// also why `Get-Content` on a BOM-less UTF-8 file returns mojibake under
/// Windows PowerShell 5.1, and why a formatted table still wraps and elides at
/// 120 columns. Surfaces that cannot live with that — hooks and the interactive
/// terminal — use `powershell_host` instead.
///
/// The trailing space is Claude Code's; it is kept so the composed script is
/// identical rather than merely equivalent.
const POWERSHELL_PROLOGUE: &str = "try { $PSDefaultParameterValues['Out-File:Encoding'] = 'utf8' } catch {}; if ($ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage') { try { $OutputEncoding = [System.Text.UTF8Encoding]::new() } catch {}; if ($null -ne $PSStyle) { try { $PSStyle.OutputRendering = 'PlainText' } catch {} } }; ";

/// Statement keywords that must be the first thing in a PowerShell script.
/// Prepending the prologue in front of any of them is a parse error, so Claude
/// Code detects them and skips the prologue rather than breaking the command.
const POWERSHELL_MUST_LEAD: [&str; 9] = [
    "using namespace",
    "using module",
    "using assembly",
    "param",
    "begin",
    "process",
    "end",
    "clean",
    "dynamicparam",
];

/// Whether the prologue has to be skipped because the caller's command must
/// syntactically come first. Leading attribute or type syntax (`[CmdletBinding()]`,
/// `[Parameter(...)]`) counts too: those bind to a following `param` block.
pub(crate) fn powershell_command_must_lead(command: &str) -> bool {
    let trimmed = command.trim_start();
    if trimmed.starts_with('[') {
        return true;
    }
    let lowered = trimmed.to_ascii_lowercase();
    POWERSHELL_MUST_LEAD.iter().any(|keyword| {
        lowered.strip_prefix(keyword).is_some_and(|rest| {
            // `param (` and `process {` lead; `parameters` and `ending` do not.
            rest.is_empty()
                || rest.starts_with(|c: char| c.is_whitespace())
                || rest.starts_with('(')
                || rest.starts_with('{')
        })
    })
}

/// Quotes a host path for a PowerShell single-quoted string literal.
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The script the `powershell` tool actually runs: prologue, the caller's
/// command, then Claude Code's epilogue.
///
/// The epilogue is what makes the exit code and the working directory survive.
/// `$LASTEXITCODE` is the status of the last *native* process, which is `$null`
/// when the command only ran cmdlets, so `$?` is the fallback; `$host.SetShouldExit`
/// is used under `FullLanguage` because `exit` inside a `-Command` string does
/// not always reach the process. `(Get-Location).Path` is written with no
/// trailing newline for the host to read back as the next call's directory.
///
/// Only the spawned argument is rewritten. The command recorded for the
/// shell-task row, echoed to the user, and classified by `security` stays the
/// text the caller wrote.
pub(crate) fn powershell_tool_script(command: &str, cwd_file: &Path) -> String {
    let prologue = if powershell_command_must_lead(command) {
        ""
    } else {
        POWERSHELL_PROLOGUE
    };
    let cwd_target = powershell_single_quote(&cwd_file.to_string_lossy());
    format!(
        "{prologue}{command}\n\
         ; $_ec = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
         ; (Get-Location).Path | Out-File -FilePath {cwd_target} -Encoding utf8 -NoNewline\n\
         ; if ($ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage') {{ $host.SetShouldExit($_ec) }} else {{ exit $_ec }}"
    )
}

/// [`powershell_tool_script`] for the Windows sandbox, whose account cannot
/// list the directories above a workspace in the user's profile. Windows
/// PowerShell will not make a directory its location unless it can list
/// every ancestor, so the script roots a drive of its own at the workspace
/// and starts there; relative paths then resolve as they would anywhere else.
/// The directory it reports is the provider's real path, not the drive's.
fn sandboxed_powershell_tool_script(command: &str, cwd_file: &Path, workspace_root: &Path, start_dir: &Path) -> String {
    let script = powershell_tool_script(command, cwd_file).replacen(
        "(Get-Location).Path | Out-File",
        "(Get-Location).ProviderPath | Out-File",
        1,
    );
    // A script that must begin with its own statement (`using`, `param`)
    // keeps the drive's root as its location rather than lose its lead.
    if powershell_command_must_lead(command) {
        return script;
    }
    let relative = start_dir
        .strip_prefix(workspace_root)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "$null = New-PSDrive -Name MeworkWorkspace -PSProvider FileSystem -Root {root} -Scope Global\n\
         ; Set-Location -LiteralPath {location}\n\
         ; {script}",
        root = powershell_single_quote(&workspace_root.to_string_lossy()),
        location = powershell_single_quote(&format!("MeworkWorkspace:\\{relative}")),
    )
}

/// Quotes one fragment for a POSIX single-quoted shell word.
fn posix_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Whether the command supplies its own stdin, in which case Claude Code does
/// not append `< /dev/null`. A heredoc or an explicit input redirect would
/// otherwise be silently starved.
fn bash_command_reads_stdin(command: &str) -> bool {
    command.contains("<<") || command.contains('<')
}

/// The script the `bash` tool actually runs, in Claude Code's order and joined
/// with ` && ` exactly as it joins them.
///
/// Every clause earns its place. Sourcing the session snapshot is what restores
/// the user's aliases, functions, `shopt` state, and PATH into a shell that was
/// not started as a login shell. `shopt -u extglob` undoes a setting the
/// snapshot may have captured that changes how later words parse. The `unsetenv`
/// unalias clears a name some rc files define as a function that shadows the
/// builtin. `TEMP`/`TMP` are exported on Windows because Git Bash inherits the
/// Windows values and native children expect them.
///
/// The caller's command runs under `eval` so it is parsed by the shell rather
/// than by argv splitting, and `pwd -P` writes the physical directory the call
/// ended in. Because the clauses are joined with `&&`, a command that fails
/// never reaches the `pwd`, so a failed call cannot move the session directory.
pub(crate) fn bash_tool_script(
    command: &str,
    snapshot: Option<&Path>,
    temp_dir: Option<&Path>,
    cwd_file: &Path,
) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if let Some(snapshot) = snapshot {
        clauses.push(format!(
            "source {} 2>/dev/null || true",
            posix_single_quote(&bash_path(snapshot))
        ));
    }
    clauses.push("shopt -u extglob 2>/dev/null || true".to_owned());
    clauses.push(
        "{ \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true"
            .to_owned(),
    );
    if let Some(temp_dir) = temp_dir {
        let quoted = posix_single_quote(&bash_path(temp_dir));
        clauses.push(format!("export TEMP={quoted} TMP={quoted}"));
    }
    let quoted_command = posix_single_quote(command);
    if bash_command_reads_stdin(command) {
        clauses.push(format!("eval {quoted_command}"));
    } else {
        clauses.push(format!("eval {quoted_command} < /dev/null"));
    }
    // Claude Code writes a bare `pwd -P` here. That is not enough on Windows:
    // Git Bash resolves through MSYS mounts, so a directory under the Windows
    // temp folder comes back as `/tmp/…` — no drive letter, nothing a host-side
    // `/c/…` rewrite can undo, and `Path::is_absolute` rejects it. `pwd -W` is
    // the MinGW builtin that prints the native `C:/…` form; it fails on a real
    // POSIX bash, where `pwd -P` was already correct, so the fallback costs
    // nothing there.
    clauses.push(format!(
        "{{ pwd -W 2>/dev/null || pwd -P; }} >| {}",
        posix_single_quote(&bash_path(cwd_file))
    ));
    clauses.join(" && ")
}

/// The script the `zsh` and `sh` tools run on the host: the caller's command
/// under `eval`, then the directory it ended in, joined with `&&` as the Bash
/// script joins them so a failed command cannot move the session.
///
/// There is no snapshot to source: the snapshot is Bash's (its generator uses
/// `declare` and `shopt`), so zsh runs as a login shell instead — the fallback
/// Bash itself takes when it has none — and `sh` runs plain.
pub(crate) fn posix_tool_script(command: &str, cwd_file: &Path) -> String {
    let quoted_command = posix_single_quote(command);
    let eval = if bash_command_reads_stdin(command) {
        format!("eval {quoted_command}")
    } else {
        format!("eval {quoted_command} < /dev/null")
    };
    format!(
        "{eval} && pwd -P >| {}",
        posix_single_quote(&cwd_file.to_string_lossy())
    )
}

/// A local login shell's script with `PATH` put back in the application's order
/// once the profile has run (macOS), recording in `env` the order to restore.
///
/// Both login legs — Bash without a snapshot, and zsh — run `/etc/profile` or
/// `/etc/zprofile`, and so `path_helper`, which would otherwise hand the command
/// Apple's `/usr/bin/python3`, `git` and `java` stubs ahead of the Homebrew, nvm
/// and pyenv builds the application's `PATH` lists first. `-l` itself stays:
/// the profile also sets up what no inherited environment carries (functions,
/// and on a machine whose login-shell probe failed, the `PATH` entries
/// themselves), and dropping it would lose those to fix an order the repair
/// already fixes. A `PATH` the run environment configures is the one the shell
/// starts with, so it is the order restored.
fn in_login_path_order(script: String, env: &mut Vec<(String, String)>) -> String {
    let Some(repaired) = crate::shell_snapshot::in_application_path_order(&script) else {
        return script;
    };
    let path = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var("PATH").ok());
    let Some(path) = path else {
        return script;
    };
    env.push((
        crate::shell_snapshot::APPLICATION_PATH_ENVIRONMENT_NAME.to_owned(),
        path,
    ));
    repaired
}

/// Rewrites a Windows path into the `/c/...` form Git Bash understands. A
/// backslash path reaching `source` or a redirect would be read as escapes.
fn bash_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/{drive}{}", &text[2..]);
    }
    text
}

/// Everything a local shell call needs that is not the command itself: where it
/// starts, where it reports having ended up, and the session state it restores.
///
/// This is the whole of Claude Code's "session" for a shell tool. There is no
/// long-lived shell process behind it — every call spawns a new interpreter —
/// so continuity is exactly these three things and nothing else. Shell
/// variables, functions defined mid-call, `umask`, and traps do not survive,
/// which is what the tool description tells the model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShellSession<'a> {
    /// Snapshot of the user's interactive shell state, sourced by each new Bash.
    /// `None` when one could not be built; the plan then falls back to `-l`.
    pub snapshot: Option<&'a Path>,
    /// Where the interpreter writes the directory it finished in, for the host
    /// to validate and adopt as the next call's starting point.
    pub cwd_file: &'a Path,
    /// `TEMP`/`TMP` for a Windows Bash child, which inherits neither from MSYS.
    pub temp_dir: Option<&'a Path>,
}

/// One call's session scaffolding, owning the paths [`ShellSession`] borrows.
///
/// The cwd file is per call rather than per conversation: two calls in the same
/// round run concurrently, and a shared file would let the slower one's
/// directory overwrite the faster one's. `Drop` removes it, so a call that
/// unwinds before reading it back does not leave a stale path for the next call
/// to adopt.
pub(crate) struct ShellCallContext {
    snapshot: Option<PathBuf>,
    cwd_file: PathBuf,
    temp_dir: Option<PathBuf>,
}

impl ShellCallContext {
    pub(crate) fn session(&self) -> ShellSession<'_> {
        ShellSession {
            snapshot: self.snapshot.as_deref(),
            cwd_file: &self.cwd_file,
            temp_dir: self.temp_dir.as_deref(),
        }
    }

    pub(crate) fn cwd_file(&self) -> &Path {
        &self.cwd_file
    }
}

impl Drop for ShellCallContext {
    fn drop(&mut self) {
        // Idempotent: `adopt_reported_cwd` already removes it on the happy path.
        let _ = std::fs::remove_file(&self.cwd_file);
    }
}

/// Builds the scaffolding for one local shell call.
///
/// Only local calls get one. WSL and SSH keep the plain invocation — Claude Code
/// has no remote runner to copy — so they report no directory and source no
/// snapshot, and their sessions are empty.
pub(crate) fn shell_call_context(
    kind: ShellKind,
    runner: &crate::run_environment::ShellRunner,
    conversation_id: &str,
    app_data: Option<&Path>,
    state: &AppState,
    sandboxed: bool,
) -> ShellCallContext {
    // A sandboxed shell can write nowhere but its workspaces and a directory
    // of its conversation's own, so that is where it reports its directory.
    let directory = if sandboxed {
        sandbox_exchange_dir(conversation_id).unwrap_or_else(std::env::temp_dir)
    } else {
        std::env::temp_dir()
    };
    let cwd_file = directory.join(format!(
        "mework-{}-{}-cwd",
        std::process::id(),
        SHELL_CWD_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let local = matches!(runner, crate::run_environment::ShellRunner::Local { .. });
    // PowerShell needs no snapshot: `-NoProfile` is Claude Code's choice there,
    // and there is no serialized interactive state to replay into it.
    let snapshot = match (local, kind, app_data) {
        (true, ShellKind::Bash, Some(app_data)) => crate::run_environment::local_bash_candidates()
            .first()
            .and_then(|shell| state.shell_snapshot(conversation_id, app_data, shell)),
        _ => None,
    };
    // Git Bash inherits neither TEMP nor TMP from MSYS, and native children
    // started from it expect both.
    let temp_dir = (local && host_platform().is_windows() && matches!(kind, ShellKind::Bash))
        .then(|| std::env::temp_dir());
    ShellCallContext {
        snapshot,
        cwd_file,
        temp_dir,
    }
}

/// The directory a conversation's sandboxed local shells share with the host:
/// the one place outside its workspaces the sandbox lets them write, for the
/// files the host reads back after a call. Private to the account, and one per
/// conversation, so one conversation's sandbox cannot plant a file another's
/// host call will read.
fn sandbox_exchange_dir(conversation_id: &str) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(conversation_id.as_bytes()));
    let directory = std::env::temp_dir().join(format!("mework-sandbox-{}", &digest[..16]));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory).ok()?;
    // Canonical, because the sandbox's rules compare real paths (`/var` is
    // `/private/var` on macOS).
    std::fs::canonicalize(&directory).ok()
}

/// Distinguishes the cwd files of two calls in the same round and the same
/// millisecond. Process-local, so it need only be unique within this run.
static SHELL_CWD_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Shell-call launch plan: executable candidates, argv, and injected environment.
/// Produced by the pure [`shell_launch_plan`] function so argv construction is testable
/// without spawning a process.
#[derive(Debug)]
struct ShellLaunchPlan {
    candidates: Vec<String>,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// Apply interpreter hardening only for local calls. Once wrapped by
    /// `wsl.exe` or `ssh`, these variables would affect only a remote process;
    /// remote hardening belongs in command wrapping.
    local_hardening: bool,
}

/// Converts a run environment, shell kind, and command into a launch plan.
///
/// - Local: Claude Code's exact invocation. Bash gets `-c <script>` when a
///   snapshot exists and `-c -l <script>` when it does not — the login shell is
///   the fallback for the state the snapshot would otherwise have restored.
///   Every login leg (that one and zsh's) repairs the `PATH` order `path_helper`
///   breaks on macOS; see [`in_login_path_order`].
///   PowerShell gets `-NoProfile -NonInteractive -ExecutionPolicy Bypass
///   -Command <script>`. Note the absent `-NoLogo`: Claude Code does not pass
///   it to the tool, only to its static parser, and parity is the point here.
/// - WSL: argv must be exactly `wsl.exe -d <distro> --cd <workspace> --exec
///   /usr/bin/env K=V… bash --noprofile --norc -c <command>` so the command
///   reaches bash intact without an intermediate shell.
/// - SSH: invoke OpenSSH with `BatchMode=yes`; every host-composed fragment is
///   POSIX-single-quoted. Only for machines the agent does not serve:
///   [`spawn_shell_process`] hands the others to [`crate::remote_link`] first.
///
/// Only the local legs carry a session. Claude Code has no remote runner, so
/// there is nothing to copy for WSL and SSH: they keep the plain invocation,
/// restore no snapshot, and do not track a directory across calls.
///
/// The launch-plan tests pin this exact argument order.
///
/// PowerShell is unavailable in remote environments because WSL and SSH provide
/// POSIX user spaces; silently passing PowerShell syntax to Bash would be misleading.
fn shell_launch_plan(
    workspace_root: &str,
    kind: ShellKind,
    program: Option<&str>,
    command: &str,
    runner: &crate::run_environment::ShellRunner,
    session: ShellSession<'_>,
) -> Result<ShellLaunchPlan, String> {
    use crate::run_environment::{self, ShellRunner};
    use crate::shell_backend::{is_registered, remote_command_argv, MachineOs};
    let env = &runner.normalized_env()?;
    let program = program.unwrap_or(kind.default_program());
    match runner {
        ShellRunner::Local { .. } => {
            if !is_registered(MachineOs::host(), kind) {
                return Err(format!(
                    "This workspace is on {}, where the {} tool is unavailable; use another shell tool instead",
                    crate::environment_prompt::host_os_name(),
                    kind.tool_name()
                ));
            }
            let candidates: Vec<String> = match kind {
                ShellKind::PowerShell => run_environment::local_powershell_candidates(),
                // Resolved to an absolute path instead of named: a bare `bash`
                // on Windows is either the WSL launcher or nothing at all. See
                // `run_environment::local_bash_candidates`.
                ShellKind::Bash => run_environment::local_bash_candidates(),
                ShellKind::Zsh | ShellKind::Sh => run_environment::local_program_path(kind.id())
                    .into_iter()
                    .collect(),
            };
            if candidates.is_empty() {
                return Err(match kind {
                    ShellKind::Bash => "No native Bash was found locally. Install Git for Windows or MSYS2, or change this conversation's run environment to WSL. The System32 WSL launcher is not used as local Bash because it executes commands in another machine's filesystem and network.".into(),
                    ShellKind::PowerShell => "No PowerShell was found locally. Install PowerShell 7 (https://aka.ms/powershell), or use the bash tool instead.".to_owned(),
                    ShellKind::Zsh | ShellKind::Sh => format!(
                        "No {} was found on this machine's PATH; use another shell tool instead",
                        kind.display_name()
                    ),
                });
            }
            let mut plan_env: Vec<(String, String)> =
                env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let args: Vec<String> = match kind {
                ShellKind::PowerShell => vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    // The execution policy gates `.ps1` files, not `-Command`, so
                    // it never stood between the model and anything; leaving it
                    // in place only turns every script invocation on a default
                    // Windows install into "running scripts is disabled".
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-Command".into(),
                    powershell_tool_script(command, session.cwd_file),
                ],
                ShellKind::Bash => {
                    let script = bash_tool_script(
                        command,
                        session.snapshot,
                        session.temp_dir,
                        session.cwd_file,
                    );
                    if session.snapshot.is_some() {
                        vec!["-c".into(), script]
                    } else {
                        vec![
                            "-c".into(),
                            "-l".into(),
                            in_login_path_order(script, &mut plan_env),
                        ]
                    }
                }
                ShellKind::Zsh => vec![
                    "-l".into(),
                    "-c".into(),
                    in_login_path_order(
                        posix_tool_script(command, session.cwd_file),
                        &mut plan_env,
                    ),
                ],
                ShellKind::Sh => vec!["-c".into(), posix_tool_script(command, session.cwd_file)],
            };
            Ok(ShellLaunchPlan {
                candidates,
                args,
                env: plan_env,
                local_hardening: true,
            })
        }
        ShellRunner::Wsl { distro, .. } => {
            if !is_registered(MachineOs::Wsl, kind) {
                return Err(format!(
                    "This workspace is in WSL, where the {} tool is unavailable; use another shell tool instead",
                    kind.tool_name()
                ));
            }
            run_environment::validate_wsl_distro_name(distro)?;
            Ok(ShellLaunchPlan {
                candidates: vec!["wsl.exe".into()],
                args: run_environment::wsl_exec_args(
                    distro,
                    workspace_root,
                    env,
                    remote_command_argv(kind, program, command),
                ),
                // Keep discovery and execution aligned. New WSL emits UTF-8, while
                // older versions use this variable as a switch; command output bypasses it.
                env: vec![("WSL_UTF8".into(), "1".into())],
                local_hardening: false,
            })
        }
        ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } => {
            let args = match kind.dialect() {
                crate::shell_backend::ScriptDialect::Posix => run_environment::ssh_exec_args(
                    host,
                    *port,
                    identity_file,
                    workspace_root,
                    env,
                    &remote_command_argv(kind, program, command),
                ),
                // `cmd.exe` or PowerShell answers the login, and both pass the
                // base64 of `-EncodedCommand` through untouched; the directory
                // and the variable table are set inside the script.
                crate::shell_backend::ScriptDialect::PowerShell => {
                    let mut args =
                        run_environment::ssh_connection_args(host, *port, identity_file);
                    let enter = if workspace_root.trim().is_empty() {
                        String::new()
                    } else {
                        format!(
                            "Set-Location -LiteralPath {}\n",
                            crate::remote_shell::ps_single_quote(workspace_root)
                        )
                    };
                    args.push(crate::remote_shell::powershell_line(&format!(
                        "{}{enter}{}",
                        run_environment::powershell_env_prologue(env),
                        crate::shell_backend::remote_powershell_command(command)
                    )));
                    args
                }
            };
            Ok(ShellLaunchPlan {
                candidates: run_environment::ssh_client_candidates(),
                args,
                env: Vec::new(),
                local_hardening: false,
            })
        }
    }
}

/// The spawn half of a shell call: resolves the interpreter, applies the
/// child's environment, and starts the process in the session's directory.
/// Shared by the synchronous leg and the background leg — a command that never
/// spawned is not a task, so both legs spawn before registering anything.
///
/// `runner` is the trusted environment resolved by the host. Local calls inject
/// its variables; WSL and SSH package the command into one wrapper invocation.
/// Killing the local wrapper process tree does not guarantee SSH descendants
/// exit — which is why an SSH machine the agent serves runs the command
/// through the agent instead, where a kill reaches the whole process group on
/// the machine and a dropped connection does not end the command.
///
/// `start_dir` is where this call begins. It is the workspace on the first call
/// of a conversation and the directory the previous call reported afterwards,
/// which is the whole of how `cd` persists — the shell itself does not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_shell_process(
    anchor: &Path,
    workspace_root: &str,
    start_dir: &Path,
    kind: ShellKind,
    program: Option<&str>,
    command: &str,
    runner: &crate::run_environment::ShellRunner,
    session: ShellSession<'_>,
    profile: &PromptProfile,
    sandbox: Option<&remote_agent::protocol::SandboxSpec>,
) -> Result<SpawnedShell, String> {
    let anchor = canonical_workspace(anchor)?;
    // A start directory that has gone away must not fail the call: fall back to
    // the workspace, exactly as a fresh conversation would begin.
    let start_dir = if start_dir.is_dir() {
        start_dir.to_path_buf()
    } else {
        anchor.clone()
    };
    if let Some(sandbox) = sandbox {
        return spawn_sandboxed_shell(&start_dir, workspace_root, kind, program, command, runner, session, sandbox);
    }
    // An SSH machine the agent serves runs the command itself: the backend's
    // own invocation, started in the workspace by the agent rather than by a
    // login shell, and not tied to any SSH connection's lifetime.
    if matches!(runner, crate::run_environment::ShellRunner::Ssh { .. }) {
        let argv = crate::shell_backend::remote_command_argv(
            kind,
            program.unwrap_or(kind.default_program()),
            command,
        );
        if let Some(spawned) = crate::remote_link::spawn(
            runner,
            argv,
            Some(workspace_root),
            remote_agent::protocol::StdinMode::Null,
            kind.tool_name(),
        ) {
            return spawned.map(|child| SpawnedShell {
                child: ShellChild::Remote {
                    child,
                    killed: false,
                },
                job: ShellJob::create(),
            });
        }
    }
    let plan = shell_launch_plan(workspace_root, kind, program, command, runner, session)?;

    let mut last_not_found = None;
    for executable in &plan.candidates {
        let mut process = Command::new(executable);
        // Injected environment variables enter first; subsequent host sanitation
        // and fixed values must override every configured name.
        for (key, value) in &plan.env {
            process.env(key, value);
        }
        // A spawned command inherits this process's environment, which under a
        // development launcher carries the browser-dev bridge's bearer token.
        // Nothing the model or the user runs needs it.
        for name in crate::child_environment::private_child_environment_names() {
            process.env_remove(&name);
        }
        if plan.local_hardening {
            let inherited = std::env::vars_os()
                .filter(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case("NO_PROXY"))
                .map(|(name, value)| {
                    let name = name
                        .into_string()
                        .map_err(|_| "Invalid NO_PROXY variable name")?;
                    let value = value
                        .into_string()
                        .map_err(|_| "NO_PROXY must contain valid Unicode")?;
                    Ok((name, value))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>, &str>>()?;
            let configured = plan.env.iter().cloned().collect();
            let proxy = crate::child_environment::normalized_proxy_bypass(
                &inherited,
                &configured,
                host_platform().is_windows(),
            )?;
            for name in inherited.keys() {
                process.env_remove(name);
            }
            process.envs(proxy);
            match kind {
                // Claude Code's PowerShell defaults, each applied only when the
                // parent does not already carry the name. `FORCE_COLOR` present
                // anywhere suppresses the `NO_COLOR` default rather than fighting
                // it. `SHELL` is explicitly cleared: a POSIX shell path confuses
                // tools that find it in a PowerShell session.
                ShellKind::PowerShell => {
                    for (key, value) in
                        child_text_defaults(&plan.env, |name| std::env::var_os(name).is_some())
                    {
                        process.env(key, value);
                    }
                    for (key, value) in [
                        ("GIT_TERMINAL_PROMPT", "0"),
                        ("GIT_ASKPASS", ""),
                        ("GCM_INTERACTIVE", "never"),
                    ] {
                        let configured = plan.env.iter().any(|(name, _)| name == key);
                        if !configured && std::env::var_os(key).is_none() {
                            process.env(key, value);
                        }
                    }
                    process.env_remove("SHELL");
                }
                // Claude Code sets nothing defensive for Bash. The hardening that
                // used to live here — clearing BASH_ENV, SHELLOPTS, exported
                // BASH_FUNC_* functions, and forcing the pagers — is gone on
                // purpose: the snapshot this shell sources already replays the
                // user's own functions and aliases, so pretending the environment
                // is sterile would be theatre. `SHELL` and `GIT_EDITOR` are the
                // two Claude Code does set.
                ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh => {
                    process.env("SHELL", executable).env("GIT_EDITOR", "true");
                }
            }
        }
        process
            .args(&plan.args)
            .current_dir(&start_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            process.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own session, so stopping it can target the whole process group. Without this
            // the group is Mework's own and a group-wide kill would take down the app.
            unsafe {
                process.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        match process.spawn() {
            Ok(child) => {
                // Created before the assignment and held until the process is reaped: everything
                // the command spawns joins the job automatically, which is what lets a stop kill
                // the whole tree at once instead of racing its parent links.
                let job = ShellJob::create();
                job.assign(&child);
                return Ok(SpawnedShell {
                    child: ShellChild::Local(child),
                    job,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_not_found = Some(error);
            }
            Err(error) => return Err(format!("Failed to start {executable}: {error}")),
        }
    }
    Err(format!(
        "No usable {} executable found (tried: {}){}",
        kind.display_name(),
        profile.join_list(&plan.candidates),
        last_not_found
            .map(|error| format!(": {error}"))
            .unwrap_or_default()
    ))
}

/// [`spawn_shell_process`] in the conversation's sandbox: the same command
/// line and variables a local call would have, or the backend's own
/// invocation on a WSL or SSH machine, started by the agent on that machine
/// inside the conversation's cell. It never falls back to running outside it.
#[allow(clippy::too_many_arguments)]
fn spawn_sandboxed_shell(
    start_dir: &Path,
    workspace_root: &str,
    kind: ShellKind,
    program: Option<&str>,
    command: &str,
    runner: &crate::run_environment::ShellRunner,
    session: ShellSession<'_>,
    sandbox: &remote_agent::protocol::SandboxSpec,
) -> Result<SpawnedShell, String> {
    use crate::run_environment::ShellRunner;
    let mut sandbox = sandbox.clone();
    let sandboxed = match runner {
        ShellRunner::Local { .. } => {
            let plan = shell_launch_plan(workspace_root, kind, program, command, runner, session)?;
            let executable = plan
                .candidates
                .iter()
                .find(|candidate| Path::new(candidate).is_file())
                .or_else(|| plan.candidates.first())
                .cloned()
                .ok_or_else(|| format!("No usable {} executable found", kind.display_name()))?;
            let mut env: std::collections::BTreeMap<String, String> = plan.env.iter().cloned().collect();
            let mut env_remove: Vec<String> = crate::child_environment::private_child_environment_names()
                .into_iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            match kind {
                ShellKind::PowerShell => {
                    for (key, value) in child_text_defaults(&plan.env, |name| std::env::var_os(name).is_some()) {
                        env.insert(key.into(), value.into());
                    }
                    for (key, value) in [("GIT_TERMINAL_PROMPT", "0"), ("GIT_ASKPASS", ""), ("GCM_INTERACTIVE", "never")] {
                        if !plan.env.iter().any(|(name, _)| name == key) && std::env::var_os(key).is_none() {
                            env.insert(key.into(), value.into());
                        }
                    }
                    env_remove.push("SHELL".into());
                }
                ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh => {
                    env.insert("SHELL".into(), executable.clone());
                    env.insert("GIT_EDITOR".into(), "true".into());
                }
            }
            // The call's own files: the directory it reports is written where
            // the host reads it back, and the snapshot it sources lives in the
            // host's data, which the sandbox otherwise cannot read.
            if let Some(directory) = session.cwd_file.parent() {
                sandbox.policy.writable.push(directory.to_string_lossy().into_owned());
            }
            // The one file, not its directory: that holds every conversation's.
            if let Some(snapshot) = session.snapshot {
                sandbox.policy.readable.push(snapshot.to_string_lossy().into_owned());
            }
            let mut argv = vec![executable];
            match kind {
                ShellKind::PowerShell if host_platform().is_windows() => {
                    let root = canonical_workspace(Path::new(workspace_root))?;
                    let mut args = plan.args;
                    if let Some(script) = args.last_mut() {
                        *script = sandboxed_powershell_tool_script(command, session.cwd_file, &root, start_dir);
                    }
                    argv.extend(args);
                }
                _ => argv.extend(plan.args),
            }
            crate::remote_link::SandboxedCommand {
                argv,
                cwd: Some(start_dir.to_string_lossy().into_owned()),
                env,
                env_remove,
                label: kind.tool_name().to_owned(),
            }
        }
        ShellRunner::Wsl { .. } | ShellRunner::Ssh { .. } => {
            let mut env = runner.normalized_env()?;
            env.retain(|name, _| !crate::run_environment::is_shell_startup_env_name(name));
            crate::remote_link::SandboxedCommand {
                argv: crate::shell_backend::remote_command_argv(
                    kind,
                    program.unwrap_or(kind.default_program()),
                    command,
                ),
                cwd: Some(workspace_root.to_owned()),
                env,
                env_remove: Vec::new(),
                label: kind.tool_name().to_owned(),
            }
        }
    };
    let child = crate::remote_link::spawn_in_sandbox(runner, &sandbox, sandboxed)?;
    Ok(SpawnedShell {
        child: ShellChild::Remote { child, killed: false },
        job: ShellJob::create(),
    })
}

/// Text defaults a local command gets when nobody set them: the model writes
/// every command assuming UTF-8 and plain output, and Windows gives a child
/// neither by itself. Python encodes a pipe with the ANSI code page unless
/// `PYTHONIOENCODING` says otherwise, and a tool that forces colour despite
/// writing to a pipe would hand the model escape sequences. They are defaults
/// only: a name the run environment configures, or one already present in the
/// launcher's environment, is left alone, and `NO_COLOR` is never set against
/// an explicit `FORCE_COLOR`.
fn child_text_defaults(
    configured: &[(String, String)],
    inherited: impl Fn(&str) -> bool,
) -> Vec<(&'static str, &'static str)> {
    let present = |name: &str| configured.iter().any(|(key, _)| key == name) || inherited(name);
    let mut defaults = Vec::new();
    if !present("PYTHONIOENCODING") {
        defaults.push(("PYTHONIOENCODING", "utf-8:surrogateescape"));
    }
    if !present("NO_COLOR") && !present("FORCE_COLOR") {
        defaults.push(("NO_COLOR", "1"));
    }
    defaults
}

/// Largest cwd file the host will read. Claude Code caps the same read; the
/// file holds one path, and anything larger means something else wrote it.
const MAX_CWD_FILE: usize = 64 * 1024;

/// Turns a Git Bash `/c/Users/...` path back into `C:\Users\...`.
///
/// This is the inverse of [`bash_path`], and it is why cwd tracking works for
/// Bash on Windows at all: `pwd -P` prints the MSYS form, and `Path::is_absolute`
/// is false for it because it has no drive prefix — so without this the reported
/// directory would be rejected on every single call and `cd` would silently
/// never persist. PowerShell needs none of this; `(Get-Location).Path` is
/// already native.
///
/// Only the `/<letter>/` shape is rewritten. A genuine POSIX path on a POSIX
/// host is absolute already and passes through untouched.
fn windows_native_path(reported: &str) -> String {
    if !host_platform().is_windows() {
        return reported.to_owned();
    }
    let bytes = reported.as_bytes();
    let drive_shaped = bytes.len() >= 3
        && bytes[0] == b'/'
        && (bytes[1] as char).is_ascii_alphabetic()
        && bytes[2] == b'/';
    if !drive_shaped {
        return reported.to_owned();
    }
    let drive = (bytes[1] as char).to_ascii_uppercase();
    format!("{drive}:\\{}", reported[3..].replace('/', "\\"))
}

/// Reads back the directory a finished call reported, validating it before the
/// host adopts it as the next call's starting point.
///
/// The file is written by the interpreter itself as the last clause of the
/// script, and only when everything before it succeeded, so a failed command
/// cannot move the session. It is deleted whether or not it validates: a stale
/// file would otherwise be read back by the next call.
///
/// Claude Code's checks are all here — absolute, no dot segments, not a device
/// path, not a network path, exists, is a directory — plus one Mework adds: the
/// result must stay inside the workspace. That is a deliberate divergence. Every
/// other tool in this host is confined to the workspace by `path_guard`, and the
/// security classifier reasons about commands relative to a workspace root, so
/// letting one `cd ..` move the anchor would quietly disarm both. A command may
/// still `cd` anywhere it likes *within* a call; only the directory that carries
/// over is confined.
pub(crate) fn adopt_reported_cwd(cwd_file: &Path, workspace: &Path) -> Option<PathBuf> {
    let raw = std::fs::read(cwd_file).ok();
    let _ = std::fs::remove_file(cwd_file);
    let raw = raw?;
    if raw.len() > MAX_CWD_FILE {
        return None;
    }
    let text = String::from_utf8_lossy(&raw);
    // PowerShell writes UTF-8 with a BOM under Windows PowerShell 5.1, which is
    // the only UTF-8 its writers can produce.
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return None;
    }
    let reported = PathBuf::from(windows_native_path(text));
    if !reported.is_absolute() {
        return None;
    }
    // `\\?\`, `\\.\`, and UNC roots are not directories this host will anchor a
    // conversation to, however real they are.
    let display = reported.to_string_lossy();
    if display.starts_with(r"\\") || display.starts_with("//") {
        return None;
    }
    if reported.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    // Canonicalized before the containment test so a symlink cannot point out of
    // the workspace while spelling itself as if it were inside. `canonicalize`
    // returns the `\\?\` form on Windows, and both sides go through it so the
    // prefixes match.
    let resolved = std::fs::canonicalize(&reported).ok()?;
    if !resolved.is_dir() {
        return None;
    }
    let workspace = std::fs::canonicalize(workspace).ok()?;
    resolved.starts_with(&workspace).then_some(resolved)
}

/// Removes a UTF-8 sequence the byte cap cut in half, so the lossy decoder does
/// not turn the lone lead byte at the end into U+FFFD. Bytes that are invalid
/// for any other reason are left for the decoder to replace where they are.
pub(crate) fn trim_incomplete_utf8_tail(bytes: &mut Vec<u8>) {
    if let Err(error) = std::str::from_utf8(bytes) {
        if error.error_len().is_none() {
            bytes.truncate(error.valid_up_to());
        }
    }
}

fn run_shell(
    workspace: &crate::workspace_set::ResolvedWorkspace,
    anchor: &Path,
    input: &JsonObject,
    kind: ShellKind,
    conversation_id: &str,
    state: &AppState,
    cancel: &CancelSignal,
    app_data: Option<&Path>,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<Outcome, String> {
    let runner = &workspace.runner;
    let command = parse_shell_command(input)?;
    // The background leg lives in the model run loop (it needs the turn's
    // agent pool). A `run_in_background` that reaches this executor came from
    // a path with no pool — direct IPC re-execution, or a defense-in-depth
    // miss — and silently running it synchronously would misreport what the
    // caller asked for.
    if shell_run_in_background_requested(input) {
        return Err("run_in_background is available only inside a model turn; remove this parameter and run the command again".into());
    }
    // Check cancellation before spawning. A cancellation in the dispatch-to-spawn
    // window must prevent side effects; the shared signal still permits at most one
    // subsequent polling interval for a race after spawning.
    if cancel.cancelled() {
        return Err("The run or task was stopped, so the command did not start; do not retry directly, and first confirm the user's intent".into());
    }
    let program = shell_program(workspace, kind)?;
    let context = shell_call_context(
        kind,
        runner,
        conversation_id,
        app_data,
        state,
        workspace.sandbox.is_some(),
    );
    // `cd` only persists for local calls, and only inside the workspace that
    // recorded it. A remote call begins at its workspace root every time.
    let start_dir = if workspace.is_local() {
        state
            .shell_cwd(conversation_id)
            .unwrap_or_else(|| PathBuf::from(&workspace.root))
    } else {
        anchor.to_path_buf()
    };
    let local_anchor = if workspace.is_local() {
        Path::new(&workspace.root)
    } else {
        anchor
    };
    // Taken before the spawn: a file whose time moved past this is one the
    // command could have touched.
    let command_started_ms = file_read_state::now_ms();
    let mut spawned = spawn_shell_process(
        local_anchor,
        &workspace.root,
        &start_dir,
        kind,
        program.as_deref(),
        &command,
        runner,
        context.session(),
        profile,
        workspace.sandbox.as_ref(),
    )?;
    // Registration starts here and not before: a command that never spawned is not a
    // task. The guard's `Drop` retires the row however this call ends; the wait below
    // is what tells it how the command actually ended.
    let mut guard =
        match state
            .shell_tasks
            .try_register(
                conversation_id,
                kind.tool_name(),
                &command,
                false,
                Some(&workspace.root),
            )
        {
            Ok(guard) => guard,
            Err(error) => {
                kill_shell_child(&mut spawned.child, &spawned.job);
                let _ = spawned.child.wait();
                return Err(error);
            }
        };
    // `cancel` is owned by its dispatching turn: a top-level run uses its own
    // flag and a task turn uses its task flag. Do not re-check a conversation-wide
    // current run, which may be unrelated. Direct IPC has an empty signal and is
    // controlled only by its registered task row.
    let cancelled = || cancel.cancelled();
    let timeout = parse_shell_timeout(input);
    // What an output too long to return is saved as, should it come to that.
    let spill_stem = format!("{}-{}", kind.tool_name(), guard.shell_task_id());
    let spill = crate::tool_output::Spill::new(app_data, conversation_id, &spill_stem);
    let run = ShellRun::start(spawned, &guard, Some(timeout))?;
    match run.wait(
        &mut guard,
        &cancelled,
        Some(Instant::now() + timeout),
        profile,
    )? {
        ShellWaitOutcome::Settled(result) => {
            // Read back after the process is reaped, so the file is complete. The
            // interpreter only reaches its `pwd` clause when everything before it
            // succeeded, so a failed command leaves no file and cannot move the
            // session.
            if let Some(cwd) = adopt_reported_cwd(context.cwd_file(), local_anchor) {
                state.set_shell_cwd(conversation_id, cwd);
            }
            // The hint goes after the fitting, so it is never what gets cut
            // or saved away.
            let mut output = crate::tool_output::fit(
                result.output,
                crate::tool_output::SHELL_INLINE_CHARS,
                spill,
                profile,
            );
            if let Some(hint) = stale_read_hint(
                file_guard,
                local_anchor,
                &command,
                command_started_ms,
                profile,
            ) {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&hint);
            }
            Ok(Outcome {
                success: result.success,
                output,
                images: Vec::new(),
                diff: None,
                opened_file: None,
                file_touch: None,
            })
        }
        // The deadline expired with the command still running. Offer it to the
        // task surface before killing it: the work is real and the model asked
        // for it. A caller with nowhere to put it falls through to a stop.
        ShellWaitOutcome::DeadlineReached(run) => {
            let refused = match handoff {
                Some(handoff) => match handoff(run, guard, context) {
                    // The command outlives this call now, so its directory is
                    // not read back — the same rule Claude Code applies to every
                    // backgrounded command, and for the same reason: it would
                    // land at an arbitrary later moment on whatever the model
                    // ran next.
                    ShellHandoffOutcome::Adopted(receipt) => {
                        return Ok(Outcome {
                            success: true,
                            output: receipt,
                            images: Vec::new(),
                            diff: None,
                            opened_file: None,
                            file_touch: None,
                        })
                    }
                    ShellHandoffOutcome::Refused(run, guard, context) => (run, guard, context),
                },
                None => (run, guard, context),
            };
            let (run, mut guard, context) = refused;
            let result = run.stop(&mut guard, profile)?;
            // A killed command may still have written its directory before the
            // deadline; reading it back would adopt a directory from a call the
            // model was told did not finish.
            drop(context);
            Ok(Outcome {
                success: false,
                output: crate::tool_output::fit(
                    result.output,
                    crate::tool_output::SHELL_INLINE_CHARS,
                    spill,
                    profile,
                ),
                images: Vec::new(),
                diff: None,
                opened_file: None,
                file_touch: None,
            })
        }
    }
}

/// Claude Code's `WRITE_COMMAND_MARKERS`: the commands that rewrite files in
/// place, and the only ones after which read files are re-checked. Verbatim,
/// so the two hosts warn after the same commands and stay quiet after the
/// same ones — an `echo >` gets no hint in either.
const FORMATTER_COMMAND_MARKERS: &[&str] = &[
    "--write",
    "--fix",
    "--in-place",
    "--auto-correct",
    r"\brun\s+format\b",
    r"\brun\s+fix\b",
    r"\b(yarn|pnpm)\s+format\b",
    r"\blint:file\b",
    r"\blint:fix\b",
    r"\bblack\b",
    r"\bisort\b",
    r"\bruff\s+format\b",
    r"\bcargo\s+(fmt|fix)\b",
    r"\brustfmt\b",
    r"\bgo\s+fmt\b",
    r"\bterraform\s+fmt\b",
    r"\bdprint\s+fmt\b",
    r"\bswiftformat\b",
    r"\bphpcbf\b",
];

/// How many touched files the hint names before it says "and N more".
const STALE_READ_HINT_NAMED_FILES: usize = 5;

pub(crate) fn looks_like_formatter_command(command: &str) -> bool {
    static MARKERS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    MARKERS
        .get_or_init(|| {
            regex::Regex::new(&FORMATTER_COMMAND_MARKERS.join("|"))
                .expect("formatter markers are a fixed, valid pattern")
        })
        .is_match(command)
}

/// Claude Code's `staleReadFileStateHint`: after a formatter-looking command,
/// the previously read files whose modification time moved past both the
/// record and the command's start. Report-only — the record is not refreshed,
/// so the next edit of such a file still trips the stale check, which is the
/// point: the model is told to read first.
fn stale_read_hint(
    file_guard: Option<FileGuardContext<'_>>,
    workspace: &Path,
    command: &str,
    command_started_ms: i64,
    profile: &PromptProfile,
) -> Option<String> {
    let guard = file_guard?;
    if !looks_like_formatter_command(command) {
        return None;
    }
    let touched = guard
        .registry
        .recorded_times(guard.scope)
        .into_iter()
        .filter(|(path, recorded_ms)| {
            file_read_state::modified_ms(path)
                .is_some_and(|now_ms| now_ms > *recorded_ms && now_ms > command_started_ms)
        })
        .map(|(path, _)| display_recorded_path(workspace, &path))
        .collect::<Vec<_>>();
    if touched.is_empty() {
        return None;
    }
    let mut files = touched
        .iter()
        .take(STALE_READ_HINT_NAMED_FILES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if touched.len() > STALE_READ_HINT_NAMED_FILES {
        files.push_str(&profile.render(
            PromptKey::ToolShellStaleReadMore,
            &[(
                "count",
                &(touched.len() - STALE_READ_HINT_NAMED_FILES).to_string(),
            )],
        ));
    }
    Some(profile.render(
        PromptKey::ToolShellStaleReadHint,
        &[("count", &touched.len().to_string()), ("files", &files)],
    ))
}

/// How this call ended, which is not the same question as whether the command succeeded.
#[derive(Clone, Copy, PartialEq)]
enum ShellCompletion {
    Exited,
    Stopped,
    /// The deadline expired and nothing could adopt the run, so it was killed.
    /// Distinct from `Stopped` because no person decided this, and the model
    /// should be told to consider `run_in_background` rather than to check with
    /// the user before retrying.
    TimedOut,
}

/// How a collected shell process ended, seen from the two stop channels. The
/// background worker maps these onto pool statuses: `Exited` reports its exit
/// code, while `StoppedByUser` and `Cancelled` both report the output collected
/// before the kill. All three fold as deliverable results.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ShellProcessEnd {
    Exited,
    /// The shell-task registry stop flag — the sidebar stop button, or a
    /// whole-conversation cancel via `stop_conversation`.
    StoppedByUser,
    /// The extra probe — the agent pool's cancel flag at turn end (background
    /// leg), or the synchronous leg's ownership signal (`RunModelRequest::round_cancellation`):
    /// the run's own flag for a top-level round, the task's own flag for a task
    /// round (a subagent or workflow step the user stopped from the sidebar).
    Cancelled,
    /// The deadline expired and no task slot was free to adopt the run, so it
    /// was killed. Only the synchronous leg can produce this; a backgrounded
    /// command has no deadline left to reach.
    TimedOut,
}

/// Everything the background leg needs from a finished command: the same
/// formatted output the synchronous leg returns, plus the raw facts the task
/// envelope reports.
pub(crate) struct ShellProcessResult {
    pub end: ShellProcessEnd,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    /// See [`ShellChild::measured_runtime`].
    pub runtime: Option<Duration>,
}

/// What happened when a timed-out run was offered to the task surface.
pub(crate) enum ShellHandoffOutcome {
    /// A task slot took it. The string is the receipt the model gets in place of
    /// the output the command has not finished producing.
    Adopted(String),
    /// Nothing could take it — no free task slot, or a caller with no task
    /// surface at all. The run comes back so the caller can stop it.
    Refused(
        ShellRun,
        crate::shell_tasks::ShellTaskGuard,
        ShellCallContext,
    ),
}

/// Offered a running command whose deadline expired, together with the row that
/// owns its stop button.
///
/// Taking both by value is the point: adopting the command means adopting
/// responsibility for finishing it and for retiring its row, and a borrow could
/// not express that. The executor keeps no way to reach the run afterwards.
pub(crate) type ShellHandoff<'a> = dyn Fn(ShellRun, crate::shell_tasks::ShellTaskGuard, ShellCallContext) -> ShellHandoffOutcome
    + 'a;

/// A spawned command with its two pipe readers already draining it.
///
/// This exists so a running command can change owners. A synchronous call that
/// reaches its deadline does not kill its process — it hands the whole run to a
/// background worker — and that is only possible if the child, the job object
/// that can kill its tree, and the two reader threads travel together as one
/// value. Splitting them would leave the pipes being drained by threads the new
/// owner cannot join.
pub(crate) struct ShellRun {
    child: ShellChild,
    job: ShellJob,
    /// Carried only so a timed-out result can name the deadline it missed.
    timeout: Option<Duration>,
    stdout_thread: thread::JoinHandle<CapturedStream>,
    stderr_thread: thread::JoinHandle<CapturedStream>,
}

/// Why [`ShellRun::wait`] returned.
pub(crate) enum ShellWaitOutcome {
    /// The command reached a terminal state and was reported to the guard.
    Settled(ShellProcessResult),
    /// The deadline passed while the command was still running. The process is
    /// untouched and still draining; the caller decides whether to background it
    /// or stop it. The guard has *not* been finished, because the command has
    /// not ended.
    DeadlineReached(ShellRun),
}

impl ShellRun {
    /// Takes both pipes and starts draining them. Must happen promptly after
    /// spawn: a command that fills a pipe buffer with nobody reading it blocks.
    pub(crate) fn start(
        spawned: SpawnedShell,
        guard: &crate::shell_tasks::ShellTaskGuard,
        timeout: Option<Duration>,
    ) -> Result<Self, String> {
        let SpawnedShell { mut child, job } = spawned;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| "Failed to capture command standard output".to_owned())?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| "Failed to capture command standard error".to_owned())?;
        let stdout_thread = collect_pipe(stdout, guard.output_sink(ShellOutputStream::Stdout));
        let stderr_thread = collect_pipe(stderr, guard.output_sink(ShellOutputStream::Stderr));
        Ok(ShellRun {
            child,
            job,
            timeout,
            stdout_thread,
            stderr_thread,
        })
    }

    /// Drives the command to a terminal state, or to `deadline`.
    ///
    /// Polled rather than parked on the child's exit: a stop request and a
    /// deadline are both exits a thread parked in `wait()` could never notice.
    /// The interval is what bounds how long the process outlives either one.
    pub(crate) fn wait(
        mut self,
        guard: &mut crate::shell_tasks::ShellTaskGuard,
        extra_stop: &dyn Fn() -> bool,
        deadline: Option<Instant>,
        profile: &PromptProfile,
    ) -> Result<ShellWaitOutcome, String> {
        let (status, end) = loop {
            match self
                .child
                .wait_timeout(SHELL_STOP_POLL)
                .map_err(|error| format!("Failed while waiting for command: {error}"))?
            {
                Some(status) => break (status, ShellProcessEnd::Exited),
                None => {
                    let end = if guard.stop_requested() {
                        ShellProcessEnd::StoppedByUser
                    } else if extra_stop() {
                        ShellProcessEnd::Cancelled
                    } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        // Deliberately not a kill. The command is still running
                        // and still draining into both the model's capture and
                        // the task page; handing it on loses nothing.
                        return Ok(ShellWaitOutcome::DeadlineReached(self));
                    } else {
                        continue;
                    };
                    // `child.kill()` reaches only the shell itself. A shell that spawned anything —
                    // which is the whole reason a command runs long — would leave those children
                    // running, still holding the workspace and still writing to pipes nobody reads.
                    kill_shell_child(&mut self.child, &self.job);
                    let status = self
                        .child
                        .wait()
                        .map_err(|error| format!("Failed to terminate command: {error}"))?;
                    break (status, end);
                }
            }
        };
        self.settle(guard, status, end, profile)
    }

    /// Drives an already-running command to its end on a background worker.
    ///
    /// Deliberately deadline-free: reaching a deadline is what makes a command
    /// *become* a background task, so one that already is has none left to hit.
    pub(crate) fn settle_in_background(
        self,
        guard: &mut crate::shell_tasks::ShellTaskGuard,
        cancel_probe: &dyn Fn() -> bool,
        profile: &PromptProfile,
    ) -> Result<ShellProcessResult, String> {
        match self.wait(guard, cancel_probe, None, profile)? {
            ShellWaitOutcome::Settled(result) => Ok(result),
            ShellWaitOutcome::DeadlineReached(_) => {
                unreachable!("a run waited without a deadline cannot reach one")
            }
        }
    }

    /// Kills the command and everything it started, then settles it. Used when a
    /// deadline expires and nothing can adopt the run — direct IPC has no task
    /// pool, and a turn whose task slots are all busy has nowhere to put it.
    pub(crate) fn stop(
        mut self,
        guard: &mut crate::shell_tasks::ShellTaskGuard,
        profile: &PromptProfile,
    ) -> Result<ShellProcessResult, String> {
        kill_shell_child(&mut self.child, &self.job);
        let status = self
            .child
            .wait()
            .map_err(|error| format!("Failed to terminate command: {error}"))?;
        match self.settle(guard, status, ShellProcessEnd::TimedOut, profile)? {
            ShellWaitOutcome::Settled(result) => Ok(result),
            ShellWaitOutcome::DeadlineReached(_) => {
                unreachable!("settle never reports a deadline")
            }
        }
    }

    /// Records the outcome and joins the readers. Split out of `wait` because a
    /// backgrounded run re-enters here through its own `wait` on the new thread.
    fn settle(
        self,
        guard: &mut crate::shell_tasks::ShellTaskGuard,
        status: ExitStatus,
        end: ShellProcessEnd,
        profile: &PromptProfile,
    ) -> Result<ShellWaitOutcome, String> {
        let completion = match end {
            ShellProcessEnd::Exited => ShellCompletion::Exited,
            ShellProcessEnd::StoppedByUser | ShellProcessEnd::Cancelled => ShellCompletion::Stopped,
            ShellProcessEnd::TimedOut => ShellCompletion::TimedOut,
        };
        // Reported from the real exit, before anything below can fail: every `?` from here on would
        // otherwise drop the guard with no outcome, and the finished row would say a person stopped a
        // command that had in fact run to completion.
        guard.finish(
            match completion {
                // A timed-out command is a failure of the command, not a stop:
                // nobody pressed anything, and the row should not read as if
                // someone had.
                ShellCompletion::Stopped => ShellTaskOutcome::Stopped,
                ShellCompletion::TimedOut => ShellTaskOutcome::Failed,
                ShellCompletion::Exited if status.success() => ShellTaskOutcome::Succeeded,
                ShellCompletion::Exited => ShellTaskOutcome::Failed,
            },
            status.code(),
        );
        let runtime = self.child.measured_runtime();
        if let Some(runtime) = runtime {
            guard.measured_runtime(runtime);
        }
        // Joined after the tree is down, never before: these threads end when the last copy of the
        // write handle closes, and a surviving grandchild holds one. That is why the kill has to
        // cover the whole tree — otherwise the command is "stopped" and this still blocks.
        let stdout = self.stdout_thread.join().map_err(|_| {
            "The command standard-output reader thread terminated unexpectedly".to_owned()
        })?;
        let stderr = self.stderr_thread.join().map_err(|_| {
            "The command standard-error reader thread terminated unexpectedly".to_owned()
        })?;
        let output = format_process_output(
            status,
            completion,
            &stdout,
            &stderr,
            self.timeout,
            profile,
        );
        Ok(ShellWaitOutcome::Settled(ShellProcessResult {
            end,
            success: status.success() && completion == ShellCompletion::Exited,
            exit_code: status.code(),
            output,
            runtime,
        }))
    }
}

pub(crate) fn collect_shell_process(
    spawned: SpawnedShell,
    guard: &mut crate::shell_tasks::ShellTaskGuard,
    extra_stop: &dyn Fn() -> bool,
    profile: &PromptProfile,
) -> Result<ShellProcessResult, String> {
    // No deadline: this is the background leg and the leg used when nothing can
    // adopt a handed-off command, so the only exits are the command's own and a
    // stop.
    match ShellRun::start(spawned, guard, None)?.wait(guard, extra_stop, None, profile)? {
        ShellWaitOutcome::Settled(result) => Ok(result),
        ShellWaitOutcome::DeadlineReached(_) => {
            unreachable!("a run started without a deadline cannot reach one")
        }
    }
}

/// Kills the command and everything it started, falling back to the direct kill when the tree kill
/// is unavailable. Leaving grandchildren alive is the failure mode that makes a "stopped" command
/// keep writing to the workspace.
pub(crate) fn kill_shell_child(child: &mut ShellChild, job: &ShellJob) {
    match child {
        ShellChild::Local(child) => kill_process_tree_or_child(child, job),
        // The agent signals the whole process group on the machine; nothing
        // on this host belongs to the command.
        ShellChild::Remote { child, killed } => {
            child.process.kill();
            *killed = true;
        }
    }
}

pub(crate) fn kill_process_tree_or_child(child: &mut std::process::Child, job: &ShellJob) {
    // The job object is first because it is the only atomic option: it kills every process in
    // the tree in one call, including ones that were already orphaned. `taskkill /T` walks
    // live parent links instead, so anything whose parent it killed a moment earlier is no
    // longer reachable and survives — still holding the inherited stdout handle, which keeps
    // the output collector blocked long after the command was supposedly stopped.
    if job.terminate() {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let process_id = child.id().to_string();
        let killed = Command::new("taskkill.exe")
            .args(["/PID", process_id.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status()
            .is_ok_and(|status| status.success());
        if killed {
            return;
        }
    }
    #[cfg(unix)]
    {
        // `run_shell` made the command its own session leader, so its process-group id is its
        // pid and the negative target reaches every descendant that did not deliberately leave
        // the group. Unlike the terminal's pty, a plain `Command` gets no group for free.
        let killed = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) } == 0;
        if killed {
            return;
        }
    }
    let _ = child.kill();
}

/// Windows job object holding one shell command and everything it spawns, so the whole tree can be
/// terminated in a single call. On other platforms this is an inert placeholder — the process group
/// established in `run_shell` plays the same role there.
pub(crate) struct ShellJob {
    #[cfg(windows)]
    handle: Option<windows_sys::Win32::Foundation::HANDLE>,
    /// The process group the assigned command leads, registered so that
    /// quitting Mework ends it the way closing the job does on Windows.
    #[cfg(unix)]
    group: std::sync::OnceLock<crate::process_groups::GroupRegistration>,
}

// The handle is owned solely by this struct and only ever used from the thread running the
// command; Windows handles are process-wide values, not thread-affine.
#[cfg(windows)]
unsafe impl Send for ShellJob {}

impl ShellJob {
    /// Creates an empty job. A failure here is not fatal: the kill path falls back to `taskkill`.
    pub(crate) fn create() -> Self {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };

            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Self { handle: None };
            }
            // Kill-on-close is the backstop: if this call ends without reaching the explicit
            // terminate — a panic, an early `?` — dropping the handle still takes the tree with
            // it rather than leaking processes that outlive the app's interest in them.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
            Self {
                handle: Some(handle),
            }
        }
        #[cfg(not(windows))]
        Self {
            #[cfg(unix)]
            group: std::sync::OnceLock::new(),
        }
    }

    /// Puts a freshly spawned command in the job. Everything it starts afterwards is added by
    /// Windows automatically, which is what makes the later terminate cover the whole tree.
    pub(crate) fn assign(&self, _child: &std::process::Child) {
        // Every local command Mework starts here leads its own process group
        // (`setsid` before exec), which is also what the exit path ends.
        #[cfg(unix)]
        if let Some(registration) = crate::process_groups::register(_child.id()) {
            let _ = self.group.set(registration);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

            if let Some(handle) = self.handle {
                unsafe { AssignProcessToJobObject(handle, _child.as_raw_handle() as _) };
            }
        }
    }

    /// Terminates every process in the job. Returns false when there is no job to terminate, so
    /// the caller falls back to the per-process kills.
    pub(crate) fn terminate(&self) -> bool {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;

            if let Some(handle) = self.handle {
                return unsafe { TerminateJobObject(handle, 1) } != 0;
            }
        }
        false
    }
}

impl Drop for ShellJob {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;

            if let Some(handle) = self.handle.take() {
                unsafe { CloseHandle(handle) };
            }
        }
    }
}

/// Bytes of a command's stream kept from its start, and from its end. What
/// the model gets is cut to [`tool_output::SHELL_INLINE_CHARS`] and the rest
/// saved to a file; these bound that file. A build that prints for an hour
/// keeps both what it said first and the error it died on, and says how much
/// fell between.
const CAPTURE_HEAD_BYTES: usize = 4 * 1024 * 1024;
const CAPTURE_TAIL_BYTES: usize = 4 * 1024 * 1024;

/// One stream of a command as captured for the model.
#[derive(Default)]
pub(crate) struct CapturedStream {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    /// Bytes that fell between the head and the tail.
    omitted: u64,
}

impl CapturedStream {
    #[cfg(test)]
    pub(crate) fn whole(bytes: &[u8]) -> Self {
        let mut captured = Self::default();
        captured.push(bytes);
        captured
    }

    fn push(&mut self, mut bytes: &[u8]) {
        let room = CAPTURE_HEAD_BYTES.saturating_sub(self.head.len());
        if room > 0 {
            let taken = bytes.len().min(room);
            self.head.extend_from_slice(&bytes[..taken]);
            bytes = &bytes[taken..];
        }
        self.tail.extend(bytes);
        let excess = self.tail.len().saturating_sub(CAPTURE_TAIL_BYTES);
        if excess > 0 {
            self.tail.drain(..excess);
            self.omitted += excess as u64;
        }
    }

    /// The stream as text, with the omission marked where it happened.
    fn text(&self, profile: &PromptProfile) -> String {
        if self.omitted == 0 {
            let mut bytes = self.head.clone();
            bytes.extend(self.tail.iter());
            return crate::console_text::decode_console_text(&bytes);
        }
        // Both cuts can land inside a character: the head's end loses its
        // partial character, the tail's start its continuation bytes.
        let mut head = self.head.clone();
        let head_len = head.len();
        trim_incomplete_utf8_tail(&mut head);
        let tail = self.tail.iter().copied().collect::<Vec<_>>();
        let skip = tail
            .iter()
            .take(3)
            .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
            .count();
        let omitted = self.omitted + (head_len - head.len()) as u64 + skip as u64;
        format!(
            "{}\n{}\n{}",
            crate::console_text::decode_console_text(&head),
            profile.render(
                PromptKey::ToolShellOutputOmitted,
                &[("size", &crate::tool_output::human_size(omitted))],
            ),
            crate::console_text::decode_console_text(&tail[skip..])
        )
    }
}

/// Drains one pipe to end-of-stream.
///
/// Two readers, one per pipe, because a command that fills its stderr buffer while nobody reads it
/// deadlocks. The captured stream is what the *model* gets — its start and its end, see
/// [`CAPTURE_HEAD_BYTES`]; the sink is what a *person* watching the task page gets, live.
///
/// Captured bytes pass through untouched; the watcher sink decodes its copy incrementally. Trailing
/// whitespace used to be stripped here, which only earned its keep while the PowerShell console was
/// widened to thousands of columns and padded every formatted row out to that width; Claude Code
/// widens nothing and trims nothing, so neither does this.
fn collect_pipe<R: Read + Send + 'static>(
    mut pipe: R,
    mut sink: ShellOutputSink,
) -> thread::JoinHandle<CapturedStream> {
    thread::spawn(move || {
        let mut captured = CapturedStream::default();
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let text = &chunk[..read];
                    sink.append(text);
                    captured.push(text);
                }
            }
        }
        sink.finish();
        captured
    })
}

/// Line endings as the model should see them. PowerShell ends every line it
/// writes with CRLF while a native program in the same command writes LF, and
/// a `\r` the model copies into an `edit` never matches the LF file it came
/// from. Only the pair is folded; a bare `\r` is a progress redraw and stays.
fn normalize_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("\r\n") {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Drops the run of blank lines a command sometimes opens with, matching Claude
/// Code's `/^(\s*\n)+/`. Indentation on the first line that carries anything is
/// content and stays.
fn strip_leading_blank_lines(text: &str) -> &str {
    let mut rest = text;
    loop {
        let Some(end) = rest.find('\n') else {
            return rest;
        };
        if !rest[..end].trim().is_empty() {
            return rest;
        }
        rest = &rest[end + 1..];
    }
}

/// The command's output as the model receives it, in Claude Code's shape.
///
/// A successful call is stdout then stderr, joined by a newline, with no labels
/// around either. The `[stderr]` heading this used to print is gone: Claude Code
/// keeps the two streams as separate fields and concatenates them in exactly
/// this order, so the grouping survives while the decoration does not.
///
/// A failed call flips to Claude Code's `ShellError` rendering — `Exit code N`,
/// then stderr, then stdout — because the status is the first thing that
/// explains the failure and a long stdout would otherwise bury it. The
/// interpreter line went with the labels; the exit code carries more.
///
/// The two streams are still captured separately, which Claude Code does not do
/// (it points both at one file). That is kept on purpose: it is what lets the
/// task page dim stderr and stream live output, a surface Claude Code has no
/// equivalent of. It does not change what the model sees, because Claude Code's
/// own model-facing text groups stderr after stdout too.
///
/// Length is not decided here: the caller fits the whole text to the shell's
/// inline share with [`crate::tool_output::fit`], saving the rest to a file.
fn format_process_output(
    status: ExitStatus,
    completion: ShellCompletion,
    stdout: &CapturedStream,
    stderr: &CapturedStream,
    timeout: Option<Duration>,
    profile: &PromptProfile,
) -> String {
    // Claude Code trims the two streams differently, and the asymmetry is real:
    // stdout loses only its leading blank lines and its trailing whitespace, so
    // a command's own indentation and interior padding survive, while stderr is
    // trimmed at both ends because it is a diagnostic, not data.
    let stdout = normalize_line_endings(&stdout.text(profile)).into_owned();
    let stdout = strip_leading_blank_lines(&stdout).trim_end().to_owned();
    let stderr = normalize_line_endings(&stderr.text(profile)).trim().to_owned();
    let mut parts: Vec<String> = Vec::new();

    match completion {
        ShellCompletion::TimedOut => {
            let seconds = timeout.map_or(0, |timeout| timeout.as_secs().max(1));
            parts.push(profile.render(
                PromptKey::ToolShellTimedOut,
                &[("seconds", &seconds.to_string())],
            ));
        }
        // A stop means a person decided this should not finish, so retrying it
        // verbatim is exactly the wrong response.
        ShellCompletion::Stopped => {
            parts.push(profile.text(PromptKey::ToolShellUserAborted).to_owned());
        }
        ShellCompletion::Exited if !status.success() => {
            let code = status.code().map_or_else(
                || profile.text(PromptKey::ToolShellExitUnknown).to_owned(),
                |code| code.to_string(),
            );
            parts.push(profile.render(PromptKey::ToolShellExitCode, &[("code", &code)]));
        }
        ShellCompletion::Exited => {}
    }

    // A failing command leads with its status and its diagnostics; a succeeding
    // one leads with what it produced.
    let failed = completion != ShellCompletion::Exited || !status.success();
    if failed {
        parts.extend([stderr, stdout].into_iter().filter(|part| !part.is_empty()));
    } else {
        parts.extend([stdout, stderr].into_iter().filter(|part| !part.is_empty()));
    }
    if parts.is_empty() {
        // A silent success still has to say something, or the round reads as if
        // the tool produced nothing at all.
        return profile.render(PromptKey::ToolShellCompleted, &[("code", "0")]);
    }
    parts.join("\n")
}

fn read_text_file(path: &Path) -> Result<String, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("Failed to read file metadata: {error}"))?;
    if metadata.len() > MAX_TEXT_FILE {
        return Err(format!(
            "Text file exceeds the {} MiB limit",
            MAX_TEXT_FILE / 1024 / 1024
        ));
    }
    let mut content = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut content))
        .map_err(|error| format!("Failed to read text file as UTF-8: {error}"))?;
    Ok(content)
}

fn read_text_file_handle(file: &mut File) -> Result<String, String> {
    file.rewind()
        .map_err(|error| format!("Failed to rewind text file handle: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_TEXT_FILE.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Failed to read text file handle: {error}"))?;
    if bytes.len() as u64 > MAX_TEXT_FILE {
        return Err(format!(
            "Text file exceeds the {} MiB limit",
            MAX_TEXT_FILE / 1024 / 1024
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("Failed to read text file as UTF-8: {error}"))
}

pub(crate) fn required_string(
    input: &Map<String, Value>,
    key: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<String, String> {
    let value = input
        .get(key)
        .ok_or_else(|| format!("Missing parameter {key}"))?
        .as_str()
        .ok_or_else(|| format!("Parameter {key} must be a string"))?;
    if !allow_empty && value.trim().is_empty() {
        return Err(format!("Parameter {key} cannot be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "Parameter {key} exceeds the {max_chars}-character limit"
        ));
    }
    Ok(value.to_owned())
}

pub(crate) fn optional_string(
    input: &Map<String, Value>,
    key: &str,
    default: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<String, String> {
    if !input.contains_key(key) || input.get(key).is_some_and(Value::is_null) {
        return Ok(default.to_owned());
    }
    required_string(input, key, max_chars, allow_empty)
}

fn optional_owned_string(
    input: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    if !input.contains_key(key) || input.get(key).is_some_and(Value::is_null) {
        return Ok(None);
    }
    required_string(input, key, max_chars, false).map(Some)
}

pub(crate) fn optional_u64(input: &Map<String, Value>, key: &str, default: u64) -> Result<u64, String> {
    optional_u64_value(input, key).map(|value| value.unwrap_or(default))
}

pub(crate) fn optional_u64_value(input: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| format!("Parameter {key} must be a non-negative integer"))
}

pub(crate) fn optional_bool(input: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_bool()
        .ok_or_else(|| format!("Parameter {key} must be a boolean"))
}

pub fn truncate_output(value: &str, profile: &PromptProfile) -> String {
    if value.len() <= MAX_TOOL_OUTPUT {
        return value.to_owned();
    }
    let mut end = MAX_TOOL_OUTPUT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n{}",
        &value[..end],
        profile.text(PromptKey::ToolOutputTruncated)
    )
}

fn truncate_diff(value: &str, profile: &PromptProfile) -> String {
    let marker = format!("\n{}", profile.text(PromptKey::ToolDiffTruncated));
    if value.len() <= MAX_DIFF_OUTPUT {
        return value.to_owned();
    }
    let mut end = MAX_DIFF_OUTPUT.saturating_sub(marker.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &value[..end])
}

pub(crate) fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let result = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const TEST_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[cfg(unix)]
    fn link_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    fn object(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    fn request(workspace: &Path, tool_name: &str, input: Value) -> ToolExecutionRequest {
        ToolExecutionRequest {
            conversation_id: "conversation-test".into(),
            workspace_path: workspace.to_string_lossy().into_owned(),
            tool_name: tool_name.into(),
            input: object(input),
        }
    }

    /// A synthetic exit status, so output formatting can be tested without
    /// spawning a process for every case.
    fn exit_status(code: i32) -> ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(code as u32)
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(code << 8)
        }
    }

    /// The tool's PowerShell script is Claude Code's, and Claude Code's prologue
    /// is deliberately thin. This pins what it does *not* do as tightly as what
    /// it does: the omissions are the parity, and each one is a live defect this
    /// project had already fixed once and gave back on purpose.
    #[test]
    fn the_powershell_script_is_claude_codes_prologue_and_epilogue() {
        let cwd_file = Path::new(r"C:\Temp\mework-1-0-cwd");
        let script = powershell_tool_script("Write-Output \"UTF-8 test\"", cwd_file);

        // What it does: an Out-File default, and — only under FullLanguage,
        // because a constrained runspace cannot assign them — $OutputEncoding
        // and plain-text rendering.
        assert!(script.starts_with(
            "try { $PSDefaultParameterValues['Out-File:Encoding'] = 'utf8' } catch {}; "
        ));
        assert!(script.contains("$ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage'"));
        assert!(script.contains("$OutputEncoding = [System.Text.UTF8Encoding]::new()"));
        assert!(script.contains("$PSStyle.OutputRendering = 'PlainText'"));

        // What it does not do. `[Console]` encodings are absent, so a redirected
        // pipe still uses the OEM code page; the file cmdlets keep their own
        // defaults, so Get-Content on a BOM-less UTF-8 file still returns
        // mojibake under 5.1; the console is not widened, so a formatted table
        // still wraps and elides at 120 columns.
        assert!(!script.contains("[Console]::OutputEncoding"));
        assert!(!script.contains("[Console]::InputEncoding"));
        assert!(!script.contains("BufferSize"));
        assert!(!script.contains("$ProgressPreference"));
        for cmdlet in ["Get-Content", "Set-Content", "Select-String"] {
            assert!(!script.contains(&format!("'{cmdlet}'")), "{cmdlet}");
        }

        // The epilogue is what makes the exit code and the directory survive.
        // $LASTEXITCODE is null when no native process ran, so $? is the
        // fallback, and the path is written with no trailing newline.
        assert!(script.contains(
            "$_ec = if ($null -ne $LASTEXITCODE) { $LASTEXITCODE } elseif ($?) { 0 } else { 1 }"
        ));
        assert!(script.contains(
            r"(Get-Location).Path | Out-File -FilePath 'C:\Temp\mework-1-0-cwd' -Encoding utf8 -NoNewline"
        ));
        assert!(script.contains("$host.SetShouldExit($_ec) } else { exit $_ec }"));

        // The caller's text is handed over verbatim, between the two.
        assert!(script.contains("Write-Output \"UTF-8 test\"\n"));
    }

    /// Prepending anything in front of a statement that must lead the script is
    /// a parse error, so the prologue steps aside instead of breaking the call.
    #[test]
    fn the_powershell_prologue_yields_to_statements_that_must_come_first() {
        for command in [
            "param($x)",
            "  using namespace System.IO",
            "using module Foo",
            "begin { }",
            "dynamicparam { }",
            "[CmdletBinding()] param()",
        ] {
            assert!(powershell_command_must_lead(command), "{command}");
            let script = powershell_tool_script(command, Path::new("/tmp/cwd"));
            assert!(script.starts_with(command), "{command}");
        }
        // Words that merely start the same way are ordinary commands.
        for command in ["parameters", "ending", "processes -Name x", "Get-Item"] {
            assert!(!powershell_command_must_lead(command), "{command}");
        }
    }

    /// Bash gets Claude Code's clause chain, and the order is load-bearing: the
    /// snapshot restores the user's state before anything runs, and the `pwd`
    /// is last so a failed command cannot move the session directory.
    #[test]
    fn the_bash_script_sources_its_snapshot_and_reports_where_it_ended() {
        let script = bash_tool_script(
            "git status",
            Some(Path::new(r"C:\data\shell-snapshots\snap.sh")),
            Some(Path::new(r"C:\Temp")),
            Path::new(r"C:\Temp\mework-1-0-cwd"),
        );
        let clauses: Vec<&str> = script.split(" && ").collect();
        assert_eq!(
            clauses[0],
            "source '/c/data/shell-snapshots/snap.sh' 2>/dev/null || true"
        );
        assert_eq!(clauses[1], "shopt -u extglob 2>/dev/null || true");
        assert!(clauses[2].contains(r"\builtin unalias -- 'unsetenv'"));
        assert_eq!(clauses[3], "export TEMP='/c/Temp' TMP='/c/Temp'");
        assert_eq!(clauses[4], "eval 'git status' < /dev/null");
        // `pwd -W` first because Git Bash's `pwd -P` resolves through MSYS
        // mounts and can return a driveless `/tmp/…`; see `bash_tool_script`.
        assert_eq!(
            clauses[5],
            "{ pwd -W 2>/dev/null || pwd -P; } >| '/c/Temp/mework-1-0-cwd'"
        );

        // A command with its own input redirect is not starved of stdin.
        let heredoc = bash_tool_script("cat <<'EOF'\nhi\nEOF", None, None, Path::new("/tmp/cwd"));
        assert!(!heredoc.contains("< /dev/null"));
        // Without a snapshot there is no source clause; the caller compensates
        // by spawning a login shell instead.
        assert!(!heredoc.contains("source "));

        // An apostrophe in the command survives POSIX single-quoting.
        let quoted = bash_tool_script("echo 'it'\\''s'", None, None, Path::new("/tmp/cwd"));
        assert!(quoted.contains(r#"eval 'echo '"'"'it'"'"'\'"'"''"'"'s'"'"'' < /dev/null"#));
    }

    /// Claude Code's whole notion of a shell "session" is this: no process
    /// survives, but the directory does. A `cd` in one call is where the next
    /// one starts.
    ///
    /// This spawns real interpreters, so it is also the only place the
    /// `/c/...` → `C:\...` rewrite is exercised end to end — without it
    /// `pwd -P` reports a path `is_absolute` rejects and `cd` silently never
    /// persists.
    #[test]
    fn a_successful_cd_is_where_the_next_command_starts() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path().join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let state = AppState::default();
        let run = |tool: &str, command: &str| {
            execute_with_scope_and_attachments(
                request(directory.path(), tool, json!({"command": command})),
                &state,
                ExecutionScope::workspace_only(directory.path()),
                Some(app_data.as_path()),
                &crate::workspace_set::WorkspaceSet::default(),
                &PromptProfile::builtin_english(),
            )
        };

        #[cfg(windows)]
        let (tool, enter, report) = ("powershell", "Set-Location nested", "(Get-Location).Path");
        #[cfg(not(windows))]
        let (tool, enter, report) = ("bash", "cd nested", "pwd -P");

        let moved = run(tool, enter);
        assert!(moved.success, "{}", moved.output);
        let where_am_i = run(tool, report);
        assert!(where_am_i.success, "{}", where_am_i.output);
        assert!(
            where_am_i.output.to_lowercase().contains("nested"),
            "the second call must start where the first one ended: {}",
            where_am_i.output
        );

        // A failed command must not move the session. The interpreter only
        // reaches its `pwd` clause when everything before it succeeded, so there
        // is nothing to read back.
        let failed = run(tool, "cd nonexistent-directory-xyz");
        assert!(!failed.success, "{}", failed.output);
        let still_there = run(tool, report);
        assert!(
            still_there.output.to_lowercase().contains("nested"),
            "a failed command must leave the session directory alone: {}",
            still_there.output
        );

        // Bash on Windows is the case the rewrite exists for: `pwd -P` prints
        // `/c/...`, which `is_absolute` rejects. Run it for real wherever a
        // native Bash is installed, because a unit test on the string alone
        // would not have caught the rewrite being skipped.
        #[cfg(windows)]
        if !crate::run_environment::local_bash_candidates().is_empty() {
            let deeper = nested.join("deeper");
            fs::create_dir_all(&deeper).unwrap();
            let moved = run("bash", "cd deeper");
            assert!(moved.success, "{}", moved.output);
            let reported = run("bash", "pwd -P");
            assert!(
                reported.output.to_lowercase().contains("deeper"),
                "a Git Bash `pwd -P` must survive the /c/ rewrite: {}",
                reported.output
            );
        }
    }

    /// On macOS both login legs — Bash without a snapshot, and zsh — restore the
    /// application's `PATH` order after `path_helper`, starting from the
    /// configured `PATH` when there is one. The snapshot leg and `sh` start no
    /// login shell and are left exactly as they were.
    #[test]
    fn login_legs_restore_the_application_path_order() {
        use crate::run_environment::ShellRunner;
        use crate::shell_snapshot::APPLICATION_PATH_ENVIRONMENT_NAME;
        let workspace = tempfile::tempdir().unwrap();
        let cwd_file = workspace.path().join("cwd");
        let snapshot = workspace.path().join("snap.sh");
        let configured = "/configured/bin:/usr/bin";
        let runner = ShellRunner::Local {
            env: [("PATH".to_owned(), configured.to_owned())]
                .into_iter()
                .collect(),
        };
        for (kind, snapshot, login) in [
            (ShellKind::Bash, None, true),
            (ShellKind::Zsh, None, true),
            (ShellKind::Bash, Some(snapshot.as_path()), false),
            (ShellKind::Sh, None, false),
        ] {
            let session = ShellSession {
                snapshot,
                cwd_file: &cwd_file,
                temp_dir: None,
            };
            // A host without this shell has nothing to launch.
            let Ok(plan) = shell_launch_plan(
                &workspace.path().to_string_lossy(),
                kind,
                None,
                "echo hi",
                &runner,
                session,
            ) else {
                continue;
            };
            let repaired = login && host_platform().is_macos();
            let script = plan.args.last().unwrap();
            assert_eq!(
                script.contains(APPLICATION_PATH_ENVIRONMENT_NAME),
                repaired,
                "{kind:?}: {script}"
            );
            assert!(script.contains("eval 'echo hi'"), "{kind:?}: {script}");
            let handoff = plan
                .env
                .iter()
                .find(|(name, _)| name == APPLICATION_PATH_ENVIRONMENT_NAME)
                .map(|(_, value)| value.as_str());
            assert_eq!(handoff, repaired.then_some(configured), "{kind:?}");
        }
    }

    /// zsh and sh keep the same session as bash: the directory a successful
    /// `cd` ended in is where the next call starts, and a failed one moves
    /// nothing.
    #[cfg(not(windows))]
    #[test]
    fn zsh_and_sh_carry_the_directory_between_calls_too() {
        let local = crate::machine_shells::local();
        for tool in ["zsh", "sh"] {
            if local.get(ShellKind::of_tool(tool).unwrap()).is_none() {
                continue;
            }
            let directory = tempfile::tempdir().unwrap();
            let app_data = directory.path().join("app-data");
            fs::create_dir_all(&app_data).unwrap();
            fs::create_dir_all(directory.path().join("nested")).unwrap();
            let state = AppState::default();
            let run = |command: &str| {
                execute_with_scope_and_attachments(
                    request(directory.path(), tool, json!({"command": command})),
                    &state,
                    ExecutionScope::workspace_only(directory.path()),
                    Some(app_data.as_path()),
                    &crate::workspace_set::WorkspaceSet::default(),
                    &PromptProfile::builtin_english(),
                )
            };
            assert!(run("cd nested").success, "{tool}");
            let here = run("pwd -P");
            assert!(here.output.ends_with("nested"), "{tool}: {}", here.output);
            assert!(!run("cd nonexistent-directory-xyz").success, "{tool}");
            assert!(run("pwd -P").output.ends_with("nested"), "{tool}");
        }
    }

    /// The snapshot is what lets a non-login shell know the user's aliases and
    /// functions. It is built once per conversation and sourced by every later
    /// call; if it stopped being produced, commands would still run and nothing
    /// would fail, so this asserts the artifact directly.
    #[cfg(not(windows))]
    #[test]
    fn the_first_bash_call_builds_a_session_snapshot_that_later_calls_source() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path().join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        let state = AppState::default();
        let result = execute_with_scope_and_attachments(
            request(directory.path(), "bash", json!({"command": "printf ok"})),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            Some(app_data.as_path()),
            &crate::workspace_set::WorkspaceSet::default(),
            &PromptProfile::builtin_english(),
        );
        assert!(result.success, "{}", result.output);

        let snapshots: Vec<_> = fs::read_dir(crate::shell_snapshot::snapshot_directory(&app_data))
            .expect("the snapshot directory exists after the first call")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            snapshots.len(),
            1,
            "one snapshot per conversation, not one per call"
        );
        let body = fs::read_to_string(snapshots[0].path()).unwrap();
        // The clauses that make a fresh shell resemble the user's own.
        assert!(body.starts_with("# Snapshot file"), "{body}");
        assert!(body.contains("unalias -a 2>/dev/null || true"), "{body}");
        assert!(body.contains("shopt -s expand_aliases"), "{body}");
        assert!(body.contains("export PATH="), "{body}");
    }

    /// A deadline with nowhere to hand the command off stops it and says so.
    /// Direct IPC is exactly that case: it has no task pool, so `handoff` is
    /// `None` and the run cannot be adopted.
    #[test]
    fn a_timed_out_command_with_no_task_surface_is_stopped_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let started = Instant::now();
        let result = execute_with_scope_and_attachments_verified(
            request(
                directory.path(),
                "bash",
                // Far longer than the deadline, so finishing on its own cannot
                // pass this test.
                json!({"command": "sleep 20", "timeout": 1200}),
            ),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &CancelSignal::default(),
            &crate::workspace_set::WorkspaceSet::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;

        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the deadline must end the call rather than let it run its full 20s: {:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(
            result.output.contains("timed out after 1s"),
            "{}",
            result.output
        );
        // A timeout is not a stop: nobody decided it, so the model is pointed at
        // `run_in_background` instead of at the user.
        assert!(
            !result.output.contains("aborted before completion"),
            "a timeout must not be reported as a user abort: {}",
            result.output
        );
        // The row says the command failed rather than that a person stopped it.
        let rows = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Some(ShellTaskOutcome::Failed));
    }

    /// The clamp is the real protection, not the parameter. A model asking for a
    /// day gets the ceiling, and a nonsense value gets the default rather than
    /// costing the call a round.
    #[test]
    fn the_requested_timeout_is_clamped_to_claude_codes_bounds() {
        let clamp =
            |value: Value| parse_shell_timeout(&object(json!({"command": "x", "timeout": value})));
        assert_eq!(clamp(json!(5_000)), Duration::from_millis(5_000));
        assert_eq!(clamp(json!(99_999_999)), SHELL_MAX_TIMEOUT);
        // A string is what a model sends when it forgets the type.
        assert_eq!(clamp(json!("30000")), Duration::from_millis(30_000));
        for nonsense in [json!(0), json!(-1), json!("soon"), json!(null)] {
            assert_eq!(clamp(nonsense.clone()), SHELL_DEFAULT_TIMEOUT, "{nonsense}");
        }
        // Absent is the default too.
        assert_eq!(
            parse_shell_timeout(&object(json!({"command": "x"}))),
            SHELL_DEFAULT_TIMEOUT
        );
    }

    /// A Git Bash `pwd -P` is not a Windows path, and `is_absolute` is false for
    /// it. Without the rewrite the reported directory is rejected on every call
    /// and the session silently never moves.
    #[test]
    fn a_git_bash_path_is_rewritten_before_it_is_judged_absolute() {
        if host_platform().is_windows() {
            assert_eq!(windows_native_path("/c/Users/a/b"), r"C:\Users\a\b");
            assert!(Path::new(&windows_native_path("/c/Users/a/b")).is_absolute());
            // A path that is already native passes through.
            assert_eq!(windows_native_path(r"C:\Users\a"), r"C:\Users\a");
            // `/tmp` is a real MSYS mount but names no drive, so it is left
            // alone and then fails the workspace-containment test.
            assert_eq!(windows_native_path("/tmp/x"), "/tmp/x");
        } else {
            assert_eq!(windows_native_path("/c/Users/a/b"), "/c/Users/a/b");
        }
    }

    #[test]
    fn edit_requires_exactly_one_match() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        fs::write(&path, "alpha beta beta").unwrap();

        let ambiguous = object(json!({
            "path": "sample.txt",
            "find": "beta",
            "replace": "gamma"
        }));
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        assert!(run_edit(directory.path(), &ambiguous, &scope, &profile, None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha beta beta");

        let exact = object(json!({
            "path": "sample.txt",
            "find": "alpha ",
            "replace": "first "
        }));
        let outcome = run_edit(directory.path(), &exact, &scope, &profile, None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first beta beta");
        let diff = outcome.diff.expect("successful edit returns a diff");
        assert!(
            diff.starts_with("--- sample.txt\n+++ sample.txt\n"),
            "{diff}"
        );
        assert!(diff.contains("-alpha beta beta"), "{diff}");
        assert!(diff.contains("+first beta beta"), "{diff}");
    }

    #[test]
    fn write_and_edit_receipts_substitute_the_placeholders_a_profile_asks_for() {
        // The built-in receipt is a bare acknowledgement, but the host keeps
        // supplying the file and its size, so a profile that names them reads
        // values rather than a literal `{path}`.
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.txt"), "alpha\n").unwrap();
        let scope = ExecutionScope::workspace_only(directory.path());
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(
            PromptKey::ToolWriteDone,
            "wrote {bytes} bytes to {path}".to_owned(),
        );
        overrides.insert(PromptKey::ToolEditDone, "edited {path}".to_owned());
        let profile = PromptProfile::from_file(
            "test".into(),
            "Test".into(),
            crate::model::ResolvedLanguage::EnUs,
            overrides,
            Vec::new(),
        );

        let written = run_write(
            directory.path(),
            &object(json!({"path": "sample.txt", "content": "alpha beta\n"})),
            &scope,
            &profile,
            None,
        )
        .unwrap();
        assert_eq!(written.output, "wrote 11 bytes to sample.txt");

        let edited = run_edit(
            directory.path(),
            &object(json!({"path": "sample.txt", "find": "beta", "replace": "gamma"})),
            &scope,
            &profile,
            None,
        )
        .unwrap();
        assert_eq!(edited.output, "edited sample.txt");

        let builtin = run_edit(
            directory.path(),
            &object(json!({"path": "sample.txt", "find": "gamma", "replace": "delta"})),
            &scope,
            &PromptProfile::builtin_english(),
            None,
        )
        .unwrap();
        assert_eq!(builtin.output, "ok");
    }

    #[test]
    fn write_returns_diffs_for_creates_and_overwrites_without_changing_its_summary() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("existing.txt"),
            "heading\nold value\nfooter\n",
        )
        .unwrap();
        let state = AppState::default();

        let overwritten = execute(
            request(
                directory.path(),
                "write",
                json!({"path":"existing.txt","content":"heading\nnew value\nfooter\n"}),
            ),
            &state,
        );
        assert!(overwritten.success, "{}", overwritten.output);
        assert_eq!(overwritten.output, "ok");
        let diff = overwritten.diff.expect("overwrite returns a diff");
        assert!(
            diff.starts_with("--- existing.txt\n+++ existing.txt\n"),
            "{diff}"
        );
        assert!(diff.contains("-old value"), "{diff}");
        assert!(diff.contains("+new value"), "{diff}");

        let created = execute(
            request(
                directory.path(),
                "write",
                json!({"path":"created.txt","content":"created\n"}),
            ),
            &state,
        );
        assert!(created.success, "{}", created.output);
        let diff = created.diff.expect("create returns a diff");
        assert!(
            diff.starts_with("--- /dev/null\n+++ created.txt\n"),
            "{diff}"
        );
        assert!(diff.contains("+created"), "{diff}");
    }

    #[test]
    fn write_keeps_working_when_the_old_file_cannot_be_diffed_as_text() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("binary.txt"), [0xff, 0xfe, 0xfd]).unwrap();
        fs::write(
            directory.path().join("oversized.txt"),
            vec![b'x'; MAX_TEXT_FILE as usize + 1],
        )
        .unwrap();
        let state = AppState::default();

        for name in ["binary.txt", "oversized.txt"] {
            let result = execute(
                request(
                    directory.path(),
                    "write",
                    json!({"path":name,"content":"replacement"}),
                ),
                &state,
            );
            assert!(result.success, "{name}: {}", result.output);
            assert_eq!(result.diff, None, "{name} must omit an inaccurate diff");
            assert_eq!(
                fs::read_to_string(directory.path().join(name)).unwrap(),
                "replacement"
            );
        }
    }

    #[test]
    fn write_diff_metadata_is_capped_at_64_kib() {
        let directory = tempfile::tempdir().unwrap();
        let old = format!("{}\n", "a".repeat(70_000));
        let new = format!("{}\n", "b".repeat(70_000));
        fs::write(directory.path().join("large-line.txt"), old).unwrap();

        let result = execute(
            request(
                directory.path(),
                "write",
                json!({"path":"large-line.txt","content":new}),
            ),
            &AppState::default(),
        );

        assert!(result.success, "{}", result.output);
        let diff = result.diff.expect("changed text returns a diff");
        assert!(
            diff.len() <= MAX_DIFF_OUTPUT,
            "diff was {} bytes",
            diff.len()
        );
        assert!(diff.ends_with("… diff truncated"), "{diff}");
    }

    #[test]
    fn default_execution_keeps_the_legacy_workspace_boundary() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside.txt");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(&outside, "outside secret").unwrap();

        let result = execute(
            request(&workspace, "read", json!({"path":outside})),
            &AppState::default(),
        );

        assert!(!result.success);
        assert!(result.output.contains("outside the trusted roots"));
    }

    #[test]
    fn unrestricted_execution_can_read_and_write_absolute_outside_paths() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let existing = outside.join("existing.txt");
        let created = outside.join("created.txt");
        fs::write(&existing, "outside secret").unwrap();
        let state = AppState::default();

        let read = execute_with_scope(
            request(&workspace, "read", json!({"path":existing})),
            &state,
            ExecutionScope::Unrestricted,
        );
        let write = execute_with_scope(
            request(
                &workspace,
                "write",
                json!({"path":created,"content":"approved outside write"}),
            ),
            &state,
            ExecutionScope::Unrestricted,
        );

        assert!(read.success);
        assert_eq!(read.output, "outside secret");
        assert!(write.success);
        assert_eq!(
            fs::read_to_string(outside.join("created.txt")).unwrap(),
            "approved outside write"
        );
    }

    #[test]
    fn restricted_execution_can_use_workspace_and_app_data_roots() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::write(app_data.join("cache.txt"), "app cache").unwrap();
        let state = AppState::default();
        let scope = ExecutionScope::restricted([workspace.clone(), app_data.clone()]);

        let read = execute_with_scope(
            request(
                &workspace,
                "read",
                json!({"path":app_data.join("cache.txt")}),
            ),
            &state,
            scope.clone(),
        );
        let write = execute_with_scope(
            request(
                &workspace,
                "write",
                json!({"path":app_data.join("new.txt"),"content":"new app data"}),
            ),
            &state,
            scope,
        );

        assert!(read.success);
        assert_eq!(read.output, "app cache");
        assert!(write.success);
        assert_eq!(
            fs::read_to_string(app_data.join("new.txt")).unwrap(),
            "new app data"
        );
    }

    #[test]
    fn denied_memory_root_is_hidden_from_direct_and_recursive_file_tools() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let memory = app_data.join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&memory).unwrap();
        fs::write(app_data.join("ordinary.txt"), "ordinary app data").unwrap();
        fs::write(
            memory.join("memory.v1.sqlite3"),
            "CROSS_MODEL_MEMORY_SENTINEL",
        )
        .unwrap();
        fs::write(
            memory.join("memory.v1.sqlite3-wal"),
            "CROSS_MODEL_WAL_SENTINEL",
        )
        .unwrap();
        fs::write(
            memory.join("memory.v1.sqlite3-shm"),
            "CROSS_MODEL_SHM_SENTINEL",
        )
        .unwrap();
        let scope = ExecutionScope::Unrestricted.denying([memory.clone()]);
        let state = AppState::default();

        for name in [
            "memory.v1.sqlite3",
            "memory.v1.sqlite3-wal",
            "memory.v1.sqlite3-shm",
        ] {
            let result = execute_with_scope(
                request(&workspace, "read", json!({"path":memory.join(name)})),
                &state,
                scope.clone(),
            );
            assert!(!result.success, "{name} was readable: {}", result.output);
            assert!(!result.output.contains("SENTINEL"));
        }

        let listed = execute_with_scope(
            request(&workspace, "ls", json!({"path":app_data,"depth":4})),
            &state,
            scope.clone(),
        );
        assert!(listed.success, "{}", listed.output);
        assert!(listed.output.contains("ordinary.txt"));
        assert!(!listed.output.contains("memory"));
        assert!(!listed.output.contains("sqlite"));

        let grepped = execute_with_scope(
            request(
                &workspace,
                "grep",
                json!({"path":app_data,"pattern":"CROSS_MODEL_"}),
            ),
            &state,
            scope.clone(),
        );
        assert!(grepped.success, "{}", grepped.output);
        assert_eq!(grepped.output, "No matches found");

        let found = execute_with_scope(
            request(
                &workspace,
                "find",
                json!({"path":app_data,"query":"*sqlite*"}),
            ),
            &state,
            scope.clone(),
        );
        assert!(found.success, "{}", found.output);
        assert_eq!(found.output, "No matching files");

        let write = execute_with_scope(
            request(
                &workspace,
                "write",
                json!({"path":memory.join("memory.v1.sqlite3"),"content":"tamper"}),
            ),
            &state,
            scope.clone(),
        );
        assert!(!write.success);
        assert_eq!(
            fs::read_to_string(memory.join("memory.v1.sqlite3")).unwrap(),
            "CROSS_MODEL_MEMORY_SENTINEL"
        );

        let alias = workspace.join("memory-alias");
        if link_directory(&memory, &alias).is_ok() {
            let aliased = execute_with_scope(
                request(
                    &workspace,
                    "read",
                    json!({"path":"memory-alias/memory.v1.sqlite3"}),
                ),
                &state,
                scope,
            );
            assert!(!aliased.success, "alias bypassed deny: {}", aliased.output);
        }
    }

    #[test]
    fn denied_nonexistent_memory_root_cannot_be_created_by_write_or_alias() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let memory = app_data.join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        let scope = ExecutionScope::Unrestricted.denying([memory.clone()]);
        let state = AppState::default();

        let direct = execute_with_scope(
            request(
                &workspace,
                "write",
                json!({
                    "path": memory.join("nested").join("memory.v1.sqlite3"),
                    "content": "tamper"
                }),
            ),
            &state,
            scope.clone(),
        );
        assert!(!direct.success, "{}", direct.output);
        assert!(!memory.exists());

        let edit = execute_with_scope(
            request(
                &workspace,
                "edit",
                json!({
                    "path": memory.join("memory.v1.sqlite3"),
                    "find": "old",
                    "replace": "new"
                }),
            ),
            &state,
            scope.clone(),
        );
        assert!(!edit.success);
        assert!(!memory.exists());

        let alias = workspace.join("app-data-alias");
        if link_directory(&app_data, &alias).is_ok() {
            let aliased = execute_with_scope(
                request(
                    &workspace,
                    "write",
                    json!({
                        "path": "app-data-alias/memory/memory.v1.sqlite3",
                        "content": "tamper"
                    }),
                ),
                &state,
                scope,
            );
            assert!(!aliased.success, "alias bypassed deny: {}", aliased.output);
            assert!(!memory.exists());
        }
    }

    #[test]
    fn unrestricted_scope_does_not_bypass_argument_validation() {
        let directory = tempfile::tempdir().unwrap();
        let result = execute_with_scope(
            request(
                directory.path(),
                "write",
                json!({"path":"missing-content.txt"}),
            ),
            &AppState::default(),
            ExecutionScope::Unrestricted,
        );

        assert!(!result.success);
        assert!(result.output.contains("Missing parameter content"));
        assert!(!directory.path().join("missing-content.txt").exists());
    }

    #[test]
    fn orchestration_entries_are_rejected_outside_the_model_loop() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        for (tool, input) in [
            ("ask_user", json!({"question": "Continue?"})),
            ("subagent", json!({"task": "do work"})),
            (
                "workflow",
                json!({"script": "export const meta = { name: \"x\", description: \"d\" }\nreturn 1"}),
            ),
            ("workflow_step", json!({"step": "step-1"})),
            ("todo", json!({"items": []})),
            (
                "TaskCreate",
                json!({"subject":"ship","description":"finish work"}),
            ),
            (
                "TaskUpdate",
                json!({"taskId":"task-1","status":"completed"}),
            ),
            ("TaskGet", json!({"taskId":"task-1"})),
            ("TaskList", json!({})),
        ] {
            let result = execute(request(directory.path(), tool, input), &state);
            assert!(!result.success, "{tool} must not execute here");
            assert!(
                result.output.contains("in-app model run loop"),
                "{}",
                result.output
            );
        }
    }

    /// Child-only tools must report that only subagent runs can execute them rather
    /// than being described as unknown tools.
    #[test]
    fn child_only_tools_are_rejected_by_name_not_as_unknown_tools() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        for (tool, input) in [
            ("subagent_update", json!({"message": "progress"})),
            ("structured_output", json!({"verdict": "ok"})),
        ] {
            let result = execute(request(directory.path(), tool, input), &state);
            assert!(!result.success, "{tool} must not execute here");
            assert!(
                result.output.contains("in-app model run loop"),
                "{}",
                result.output
            );
            assert!(
                result.output.contains("Child-agent-only"),
                "{}",
                result.output
            );
            assert!(!result.output.contains("Unknown tool"), "{}", result.output);
        }
    }

    #[test]
    fn all_catalog_tools_execute_in_one_workspace_flow() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let run = |tool_name: &str, input: Value| {
            let result = execute(request(directory.path(), tool_name, input), &state);
            assert!(result.success, "{tool_name} failed: {}", result.output);
            result.output
        };

        run(
            "write",
            json!({"path":"nested/probe.txt","content":"alpha MEWORK_TOOL_E2E omega\n"}),
        );
        assert!(run("read", json!({"path":"nested/probe.txt"})).contains("MEWORK_TOOL_E2E"));
        run(
            "edit",
            json!({"path":"nested/probe.txt","find":"alpha","replace":"beta"}),
        );
        assert!(run(
            "grep",
            json!({"path":"nested","pattern":"beta MEWORK_TOOL_E2E","case_sensitive":true}),
        )
        .contains("nested/probe.txt:1"));
        assert!(run("find", json!({"path":".","query":"*.txt"})).contains("nested/probe.txt"));
        assert!(run("ls", json!({"path":".","depth":2})).contains("nested/probe.txt"));
        if host_platform().is_windows() {
            assert!(run(
                "powershell",
                json!({"command":"Write-Output MEWORK_POWERSHELL_E2E"}),
            )
            .contains("MEWORK_POWERSHELL_E2E"));
        } else {
            let refused = execute(
                request(
                    directory.path(),
                    "powershell",
                    json!({"command":"Write-Output MEWORK_POWERSHELL_E2E"}),
                ),
                &state,
            );
            assert!(!refused.success, "{}", refused.output);
            assert!(
                refused.output.contains("has no PowerShell"),
                "{}",
                refused.output
            );
            // The host's other POSIX shells run commands the way bash does.
            let local = crate::machine_shells::local();
            for (tool, marker) in [("zsh", "MEWORK_ZSH_E2E"), ("sh", "MEWORK_SH_E2E")] {
                if local.get(ShellKind::of_tool(tool).unwrap()).is_some() {
                    assert!(
                        run(tool, json!({"command": format!("printf {marker}")})).contains(marker),
                        "{tool}"
                    );
                }
            }
        }
        assert!(
            run("bash", json!({"command":"printf MEWORK_BASH_E2E"})).contains("MEWORK_BASH_E2E")
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("nested/probe.txt")).unwrap(),
            "beta MEWORK_TOOL_E2E omega\n"
        );
    }

    /// A shell command must be addressable while it runs, and must still be *there* once it does
    /// not. Before this, the round simply blocked inside `wait_timeout` and nothing anywhere knew a
    /// command existed; then for a while the row existed but deleted itself at the exact moment it
    /// could have said how the command went.
    #[test]
    fn a_running_command_is_a_task_of_its_own_conversation_and_becomes_a_finished_row() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let registry = state.shell_tasks.clone();
        // Poll until the command ends rather than using a fixed observation window,
        // so process creation under parallel test load cannot exhaust the window.
        let runner_directory = directory.path().to_path_buf();
        let runner_state = state.clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner_finished = Arc::clone(&finished);
        let runner = std::thread::spawn(move || {
            let result = execute(
                request(
                    &runner_directory,
                    "bash",
                    json!({"command": "sleep 1.2; printf MEWORK_SHELL_TASK_DONE"}),
                ),
                &runner_state,
            );
            runner_finished.store(true, std::sync::atomic::Ordering::Release);
            result
        });

        let mut seen = Vec::new();
        loop {
            let snapshots = registry.task_snapshots("conversation-test");
            if !snapshots.is_empty() {
                seen = snapshots;
                break;
            }
            if finished.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let result = runner.join().unwrap();

        assert_eq!(
            seen.len(),
            1,
            "the running command must be exactly one task"
        );
        assert_eq!(seen[0].tool_name, "bash");
        assert!(seen[0].command.contains("MEWORK_SHELL_TASK_DONE"));
        assert!(!seen[0].stopping);
        assert!(seen[0].conversation_id == "conversation-test");
        assert!(
            seen[0].outcome.is_none(),
            "a running row has no outcome yet"
        );
        assert!(result.success, "{}", result.output);

        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1, "the row survives as a finished one");
        assert_eq!(finished[0].shell_task_id, seen[0].shell_task_id);
        // Reported from the real exit, not inferred from the guard dropping: a `Drop` with no
        // outcome records a stop, and a command that ran to completion must never read that way.
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Succeeded));
        assert_eq!(finished[0].exit_code, Some(0));
        assert!(finished[0].ended_at.is_some());
        assert!(!state
            .shell_tasks
            .is_running("conversation-test", &finished[0].shell_task_id));
    }

    /// A command that exits non-zero is finished, not stopped: the sidebar paints those differently
    /// and "a person killed this" is a claim about a person.
    #[test]
    fn a_failing_command_finishes_with_its_exit_code() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();

        let result = execute(
            request(directory.path(), "bash", json!({"command": "exit 3"})),
            &state,
        );
        assert!(!result.success, "{}", result.output);

        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Failed));
        assert_eq!(finished[0].exit_code, Some(3));
    }

    /// Background execution requires a model turn with a task pool for delivery.
    /// Direct IPC or manual re-execution must reject it rather than silently
    /// degrading an immediate-return request into unbounded synchronous blocking.
    #[test]
    fn run_in_background_is_refused_outside_a_model_turn() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let result = execute(
            request(
                directory.path(),
                "bash",
                json!({"command": "printf hi", "run_in_background": true}),
            ),
            &state,
        );
        assert!(!result.success);
        assert!(
            result.output.contains("run_in_background"),
            "{}",
            result.output
        );
        // A rejected call never spawns and leaves no task row.
        assert!(state
            .shell_tasks
            .task_snapshots("conversation-test")
            .is_empty());
    }

    /// The point of the task row: pressing stop has to end the command promptly, and the
    /// model has to be told a person stopped it rather than that it merely failed.
    #[test]
    fn stopping_a_command_kills_it_promptly_and_says_a_person_did_it() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let registry = state.shell_tasks.clone();
        let stopper = std::thread::spawn(move || {
            for _ in 0..600 {
                let snapshots = registry.task_snapshots("conversation-test");
                if let Some(task) = snapshots.first() {
                    assert!(registry.request_stop("conversation-test", &task.shell_task_id));
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        });

        let started = Instant::now();
        let result = execute(
            request(
                directory.path(),
                "bash",
                // Far longer than the poll interval and far longer than the assertion
                // below, so finishing on its own cannot pass this test.
                json!({"command": "sleep 20"}),
            ),
            &state,
        );
        assert!(stopper.join().unwrap(), "the task never became visible");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a stopped command must die on the stop request rather than run its full 20s: {:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("<error>Command was aborted before completion</error>"),
            "{}",
            result.output
        );
        // A stop and a timeout both end a command early and must stay
        // distinguishable: one was a person's decision and the other is a hint
        // to use `run_in_background`.
        assert!(
            !result.output.contains("timed out"),
            "a stop must not be reported as a timeout: {}",
            result.output
        );
        // The row that survives says a person did it. Reading "failed" here would send the model
        // straight back to retrying the thing someone just killed.
        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Stopped));
        assert!(!finished[0].stopping, "a dead process is not winding down");
    }

    /// Launch plans across run environments and shells. Local argv is pinned because
    /// changing it changes every synchronous and background command invocation.
    #[test]
    fn local_launch_plan_matches_claude_codes_invocation() {
        use crate::run_environment::ShellRunner;
        let workspace = tempfile::tempdir().unwrap();
        let runner = ShellRunner::Local {
            env: [("FOO".to_owned(), "bar".to_owned())].into_iter().collect(),
        };
        let snapshot = workspace.path().join("snap.sh");
        let cwd_file = workspace.path().join("cwd");
        let with_snapshot = ShellSession {
            snapshot: Some(&snapshot),
            cwd_file: &cwd_file,
            temp_dir: None,
        };

        // Only the invocation is pinned here. The Local interpreter is now resolved
        // rather than named — a bare `bash` on Windows is either the WSL launcher or
        // nothing — so the selection rule is pinned by its own unit tests in
        // `run_environment`.
        match shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::Bash,
            None,
            "echo hi",
            &runner,
            with_snapshot,
        ) {
            Ok(bash) => {
                // A snapshot replaces the login shell: `-l` would re-run the
                // profile on every call to recover what the snapshot already has.
                assert_eq!(bash.args.len(), 2);
                assert_eq!(bash.args[0], "-c");
                assert!(bash.args[1].starts_with("source "));
                assert!(bash.args[1].contains("eval 'echo hi' < /dev/null"));
                assert_eq!(bash.env, vec![("FOO".to_owned(), "bar".to_owned())]);
                assert!(bash.local_hardening);
                assert!(!bash.candidates.is_empty());
                #[cfg(not(windows))]
                {
                    assert_eq!(bash.candidates.len(), 1);
                    let candidate = std::path::Path::new(&bash.candidates[0]);
                    assert!(
                        candidate.is_absolute() && candidate.ends_with("bash"),
                        "{candidate:?}"
                    );
                }
                #[cfg(windows)]
                for candidate in &bash.candidates {
                    let lowered = candidate.to_ascii_lowercase().replace('/', "\\");
                    assert!(lowered.ends_with("bash.exe"), "{candidate}");
                    assert!(
                        !lowered.contains("\\windows\\"),
                        "a Local run must never dispatch to the WSL launcher: {candidate}"
                    );
                }

                // Without a snapshot the login shell is the fallback for the
                // state it would have restored.
                let bare = ShellSession {
                    snapshot: None,
                    cwd_file: &cwd_file,
                    temp_dir: None,
                };
                let fallback = shell_launch_plan(
                    &workspace.path().to_string_lossy(),
                    ShellKind::Bash,
                    None,
                    "echo hi",
                    &runner,
                    bare,
                )
                .unwrap();
                assert_eq!(fallback.args[..2], ["-c", "-l"]);
                assert_eq!(fallback.args.len(), 3);
            }
            // A Windows host with no native Bash installed: the plan refuses with an
            // actionable message instead of handing the command to the launcher.
            Err(error) => {
                assert!(host_platform().is_windows(), "{error}");
                assert!(error.contains("No native Bash was found"), "{error}");
            }
        }

        match shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::PowerShell,
            None,
            "echo hi",
            &runner,
            with_snapshot,
        ) {
            Ok(ps) => {
                // `-NoLogo` is deliberately absent: Claude Code passes it only to
                // its static parser, never to the tool.
                assert_eq!(
                    ps.args[..5],
                    [
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command"
                    ]
                );
                assert_eq!(ps.args[5], powershell_tool_script("echo hi", &cwd_file));
                assert_eq!(ps.args.len(), 6);
                // PowerShell 7 is preferred and Windows PowerShell 5.1 is last:
                // 5.1 is the one that reads BOM-less UTF-8 files as ANSI.
                let first = ps
                    .candidates
                    .first()
                    .expect("a candidate")
                    .to_ascii_lowercase();
                assert!(
                    first.contains("pwsh") || ps.candidates.len() == 1,
                    "{first}"
                );
            }
            Err(error) if host_platform().is_windows() => {
                assert!(error.contains("No PowerShell was found"), "{error}");
            }
            Err(error) => {
                // A Mac or Linux host is a POSIX workspace: the refusal names
                // the machine and the tool to use instead, not an installer.
                assert!(
                    error.contains(crate::environment_prompt::host_os_name())
                        && error.contains("use another shell tool"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn child_text_defaults_yield_to_configured_and_inherited_values() {
        let none: Vec<(String, String)> = Vec::new();
        assert_eq!(
            child_text_defaults(&none, |_| false),
            vec![
                ("PYTHONIOENCODING", "utf-8:surrogateescape"),
                ("NO_COLOR", "1")
            ]
        );
        // A run-environment value is the user's choice, whatever it is.
        let configured = vec![("PYTHONIOENCODING".to_owned(), "gbk".to_owned())];
        assert_eq!(
            child_text_defaults(&configured, |_| false),
            vec![("NO_COLOR", "1")]
        );
        // So is a value already in the launcher's environment.
        assert_eq!(
            child_text_defaults(&none, |name| name == "NO_COLOR"),
            vec![("PYTHONIOENCODING", "utf-8:surrogateescape")]
        );
        // `NO_COLOR` against an explicit `FORCE_COLOR` would contradict the user.
        let forced = vec![("FORCE_COLOR".to_owned(), "1".to_owned())];
        assert_eq!(
            child_text_defaults(&forced, |_| false),
            vec![("PYTHONIOENCODING", "utf-8:surrogateescape")]
        );
    }

    /// Output reaches the model as the command wrote it, apart from the two
    /// trims Claude Code applies at the edges. Per-line trailing whitespace used
    /// to be stripped, which only earned its keep while the PowerShell console
    /// was widened to thousands of columns and padded every formatted row out to
    /// that width. Claude Code widens nothing and strips nothing per line.
    #[test]
    fn a_read_without_an_end_returns_two_thousand_lines_and_says_where_to_go_on() {
        let directory = tempfile::tempdir().unwrap();
        let text = (1..=2_500).map(|n| format!("line {n}\n")).collect::<String>();
        fs::write(directory.path().join("long.txt"), &text).unwrap();
        let state = AppState::default();

        let first = execute(request(directory.path(), "read", json!({"path":"long.txt"})), &state);
        assert!(first.success, "{}", first.output);
        assert!(first.output.starts_with("line 1\nline 2\n"));
        assert!(first.output.contains("\nline 2000\n"), "{}", &first.output[first.output.len() - 200..]);
        assert!(!first.output.contains("line 2001"));
        assert!(
            first.output.ends_with("… showing lines 1–2000 of 2500. Continue with start_line=2001."),
            "{}",
            &first.output[first.output.len() - 200..]
        );

        // A range that is all there comes back whole, with nothing appended.
        let rest = execute(
            request(directory.path(), "read", json!({"path":"long.txt","start_line":2001,"end_line":2500})),
            &state,
        );
        assert!(rest.success, "{}", rest.output);
        assert!(rest.output.starts_with("line 2001\n"));
        assert!(rest.output.ends_with("line 2500"), "{}", &rest.output[rest.output.len() - 100..]);
    }

    #[test]
    fn a_read_stops_at_its_byte_budget_on_a_line_boundary() {
        let directory = tempfile::tempdir().unwrap();
        // A thousand bytes a line with its newline: 61 fit in 60 KiB.
        let row = "x".repeat(999);
        let text = (0..100).map(|_| format!("{row}\n")).collect::<String>();
        fs::write(directory.path().join("wide.txt"), &text).unwrap();
        fs::write(directory.path().join("one-line.min.js"), "y".repeat(70_000)).unwrap();
        let state = AppState::default();

        let read = execute(
            request(directory.path(), "read", json!({"path":"wide.txt","start_line":1,"end_line":100})),
            &state,
        );
        assert!(read.success, "{}", read.output);
        assert_eq!(read.output.lines().filter(|line| *line == row).count(), 61);
        assert!(
            read.output.ends_with("… showing lines 1–61 of 100. Continue with start_line=62."),
            "{}",
            &read.output[read.output.len() - 100..]
        );

        let refused = execute(
            request(directory.path(), "read", json!({"path":"one-line.min.js"})),
            &state,
        );
        assert!(!refused.success);
        assert_eq!(
            refused.output,
            "Line 1 alone is 70 KB, more than one read can return. Use grep to find the part you need."
        );
    }

    #[test]
    fn a_stream_past_its_capture_keeps_its_start_and_its_end() {
        let profile = PromptProfile::builtin_english();
        let mut captured = CapturedStream::default();
        // The head ends inside a three-byte character and the tail starts
        // inside another; neither half is left with a broken one.
        let wide = "中".repeat(CAPTURE_HEAD_BYTES / 3 + 1);
        captured.push(wide.as_bytes());
        captured.push(&vec![b'-'; 998]);
        captured.push("终".repeat(CAPTURE_TAIL_BYTES / 3 + 1).as_bytes());
        let text = captured.text(&profile);
        let (head, rest) = text.split_once('\n').unwrap();
        let (marker, tail) = rest.split_once('\n').unwrap();
        assert!(head.chars().all(|character| character == '中'));
        assert_eq!(head.len(), CAPTURE_HEAD_BYTES / 3 * 3);
        assert!(tail.chars().all(|character| character == '终'));
        assert_eq!(tail.len(), CAPTURE_TAIL_BYTES / 3 * 3);
        assert_eq!(marker, "[… 1 KB of output omitted …]");

        let short = CapturedStream::whole(b"all of it");
        assert_eq!(short.text(&profile), "all of it");
    }

    #[test]
    fn a_command_whose_output_is_too_long_hands_back_a_file_and_a_preview() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let run = |tool: &str, input: Value| {
            execute_with_scope_and_attachments(
                request(directory.path(), tool, input),
                &state,
                ExecutionScope::restricted([
                    fs::canonicalize(directory.path()).unwrap(),
                    fs::canonicalize(app_data.path()).unwrap(),
                ]),
                Some(app_data.path()),
                &crate::workspace_set::WorkspaceSet::default(),
                &PromptProfile::builtin_english(),
            )
        };
        let result = run(
            "bash",
            json!({"command": "i=0; while [ $i -lt 5000 ]; do echo \"row $i of the output\"; i=$((i+1)); done"}),
        );
        assert!(result.success, "{}", result.output);
        assert!(result.output.starts_with("<persisted-output>\nOutput too large ("), "{}", result.output);
        assert!(result.output.contains("row 0 of the output"));
        assert!(!result.output.contains("row 4999"));
        let path = crate::tool_output::saved_path(&result.output).expect("a saved file");
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("row 0 of the output\n"));
        assert!(saved.ends_with("row 4999 of the output"));

        // The rest is a `read` away.
        let tail = run("read", json!({"path": path, "start_line": 4999}));
        assert!(tail.success, "{}", tail.output);
        assert_eq!(tail.output, "row 4998 of the output\nrow 4999 of the output");

        // `grep` spills past its own, smaller share.
        fs::write(
            directory.path().join("many.txt"),
            (0..300).map(|n| format!("needle {n} {}\n", "z".repeat(200))).collect::<String>(),
        )
        .unwrap();
        let grepped = run("grep", json!({"pattern": "needle"}));
        assert!(grepped.success, "{}", grepped.output);
        assert!(grepped.output.starts_with("<persisted-output>"), "{}", &grepped.output[..200]);
        let saved = fs::read_to_string(crate::tool_output::saved_path(&grepped.output).unwrap()).unwrap();
        assert_eq!(saved.lines().filter(|line| line.contains(":needle ")).count(), 250);
        assert!(saved.ends_with("pass offset=250 for the next page, or narrow the pattern or path."));
    }

    /// A conversation whose workspace is on another machine still has its
    /// spilled output here, and a `read` of it is served here rather than
    /// sent to a machine that has never seen the file.
    #[test]
    fn saved_output_is_read_on_the_host_whatever_workspace_the_call_names() {
        let anchor = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let notice = crate::tool_output::fit(
            "saved line\n".repeat(10),
            5,
            crate::tool_output::Spill::new(Some(app_data.path()), "conversation-test", "bash-1"),
            &PromptProfile::builtin_english(),
        );
        let path = crate::tool_output::saved_path(&notice).unwrap();
        let unreachable = crate::run_environment::ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: "mework-test-host.invalid".into(),
            port: 0,
            identity_file: String::new(),
            env: Default::default(),
        };
        let state = AppState::default();
        let read = execute_with_scope_and_attachments(
            request(anchor.path(), "read", json!({"path": path, "end_line": 2})),
            &state,
            ExecutionScope::restricted([
                fs::canonicalize(anchor.path()).unwrap(),
                fs::canonicalize(app_data.path()).unwrap(),
            ]),
            Some(app_data.path()),
            &crate::workspace_set::WorkspaceSet::single("/home/dev/app", unreachable),
            &PromptProfile::builtin_english(),
        );
        assert!(read.success, "{}", read.output);
        assert_eq!(read.output, "saved line\nsaved line");
    }

    #[test]
    fn ls_walks_breadth_first_and_leaves_ignored_directories_unexpanded() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        // No Git here (unless the temporary directory sits in a work tree),
        // so the dependency names stand in.
        // Thirteen characters an entry: 4,000 of them are past the budget.
        for index in 0..4_000 {
            let path = root.join(format!("big/sub{index:04}"));
            fs::create_dir_all(&path).unwrap();
        }
        fs::create_dir_all(root.join("node_modules/react")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "\n").unwrap();
        fs::write(root.join("z-last.txt"), "\n").unwrap();
        let state = AppState::default();
        let listed = execute(request(root, "ls", json!({"depth": 2})), &state);
        assert!(listed.success, "{}", listed.output);
        // Every top-level entry made it, although `big/` alone could fill the
        // budget: the cut falls on the deepest level reached.
        for entry in ["big/", "src/", "z-last.txt"] {
            assert!(listed.output.lines().any(|line| line == entry), "{entry}");
        }
        if !listed.output.contains("node_modules/ (ignored)") {
            // A work tree around the temporary directory decides otherwise.
            return;
        }
        assert!(!listed.output.contains("node_modules/react"));
        // The second level is the one cut, and the answer says so.
        assert!(listed.output.contains("big/sub0000/"));
        assert!(listed.output.contains("listing cut at the 40000-character limit; it is complete to depth 0."));
        assert!(listed.output.chars().count() < search_scope::LS_BUDGET_CHARS + 1_000);
    }

    #[test]
    fn command_output_is_not_reshaped_on_its_way_to_the_model() {
        let profile = PromptProfile::default();
        let padded = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            &CapturedStream::whole(b"\n  \r\nName       \r\nvalue  \n"),
            &CapturedStream::whole(b""),
            None,
            &profile,
        );
        // Leading blank lines and the trailing newline go; CRLF folds to LF —
        // that is about the model copying text into an `edit`, not about width.
        // The padding *inside* the output survives, because a command that
        // printed it meant to.
        assert_eq!(padded, "Name       \nvalue");
    }

    #[cfg(windows)]
    #[test]
    fn gbk_stdout_is_decoded_for_model_facing_text() {
        if crate::console_text::system_ansi_code_page() != Some(936) {
            return;
        }
        let profile = PromptProfile::default();
        let output = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            &CapturedStream::whole(&[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'\n']),
            &CapturedStream::whole(b""),
            None,
            &profile,
        );
        assert_eq!(output, "中文测试");
    }

    #[test]
    fn incomplete_utf8_tail_is_dropped_but_other_invalid_bytes_stay() {
        let mut cut = "中文".as_bytes()[..4].to_vec();
        trim_incomplete_utf8_tail(&mut cut);
        assert_eq!(cut, "中".as_bytes());

        let mut whole = "中文".as_bytes().to_vec();
        trim_incomplete_utf8_tail(&mut whole);
        assert_eq!(whole, "中文".as_bytes());

        // A stray byte in the middle is the decoder's to replace in place.
        let mut stray = b"a\xffb".to_vec();
        trim_incomplete_utf8_tail(&mut stray);
        assert_eq!(stray, b"a\xffb");
    }

    #[test]
    fn process_output_folds_crlf_for_the_model() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        let profile = PromptProfile::builtin_english();
        let output = format_process_output(
            ExitStatus::from_raw(0),
            ShellCompletion::Exited,
            &CapturedStream::whole(b"PS\r\nNODE\nprogress 10%\rprogress 20%\r\n"),
            &CapturedStream::whole(b"warn\r\n"),
            None,
            &profile,
        );
        assert_eq!(output, "PS\nNODE\nprogress 10%\rprogress 20%\nwarn");
    }

    /// A failing command leads with its status, then its diagnostics, then what
    /// it managed to print. That order is Claude Code's `ShellError` rendering,
    /// and it exists because a long stdout would otherwise bury the one line
    /// that explains the failure.
    #[test]
    fn a_failed_command_leads_with_its_exit_code_and_stderr() {
        let profile = PromptProfile::builtin_english();
        let output = format_process_output(
            exit_status(2),
            ShellCompletion::Exited,
            &CapturedStream::whole(b"partial progress\n"),
            &CapturedStream::whole(b"fatal: not a git repository\n"),
            None,
            &profile,
        );
        assert_eq!(
            output,
            "Exit code 2\nfatal: not a git repository\npartial progress"
        );

        // A successful command puts what it produced first and does not
        // announce a status nobody needs.
        let ok = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            &CapturedStream::whole(b"on main\n"),
            &CapturedStream::whole(b"note\n"),
            None,
            &profile,
        );
        assert_eq!(ok, "on main\nnote");

        // A silent success still has to say something, or the round reads as if
        // the tool produced nothing at all.
        let silent = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            &CapturedStream::whole(b""),
            &CapturedStream::whole(b""),
            None,
            &profile,
        );
        assert_eq!(silent, "Command finished (exit code 0)");
    }

    /// A deadline that nothing could adopt is not a stop: nobody decided it, and
    /// the model should be pointed at `run_in_background` rather than told to
    /// check with the user before retrying.
    #[test]
    fn a_timed_out_command_says_so_and_names_its_deadline() {
        let profile = PromptProfile::builtin_english();
        let output = format_process_output(
            exit_status(1),
            ShellCompletion::TimedOut,
            &CapturedStream::whole(b"building...\n"),
            &CapturedStream::whole(b""),
            Some(Duration::from_millis(120_000)),
            &profile,
        );
        assert!(
            output.starts_with("Command timed out after 120s"),
            "{output}"
        );
        assert!(output.contains("run_in_background"), "{output}");
        // Whatever it printed before the kill is still delivered.
        assert!(output.ends_with("building..."), "{output}");
    }

    #[test]
    fn missing_ls_target_returns_a_non_leaking_tool_failure() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("MSIX/LocalCache/workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("file"), "body").unwrap();
        for path in ["missing/nested", "file"] {
            let result = execute_with_scope_and_attachments(
                request(&workspace, "ls", json!({"path":path})),
                &AppState::default(),
                ExecutionScope::workspace_only(&workspace),
                None,
                &crate::workspace_set::WorkspaceSet::default(),
                &PromptProfile::builtin_english(),
            );
            assert!(!result.success);
            assert!(result.output.contains(path), "{}", result.output);
            assert!(!result.output.contains("LocalCache"), "{}", result.output);
            assert!(
                !result
                    .output
                    .contains(&directory.path().to_string_lossy().to_string()),
                "{}",
                result.output
            );
        }
    }

    #[test]
    fn shell_launch_plans_normalize_proxy_bypass_for_each_target() {
        use crate::run_environment::ShellRunner;
        let directory = tempfile::tempdir().unwrap();
        let cwd_file = directory.path().join("cwd");
        let bare = ShellSession {
            snapshot: None,
            cwd_file: &cwd_file,
            temp_dir: None,
        };
        let env = [("No_Proxy".into(), " localhost;127.0.0.1 localhost ".into())]
            .into_iter()
            .collect::<std::collections::BTreeMap<String, String>>();
        let runners = [
            ShellRunner::Local { env: env.clone() },
            ShellRunner::Wsl {
                agent_shell: Default::default(),
                distro: "Ubuntu".into(),
                env: env.clone(),
            },
            ShellRunner::Ssh {
                agent_shell: Default::default(),
                host: "user@host".into(),
                port: 22,
                identity_file: String::new(),
                env,
            },
        ];
        for (index, runner) in runners.iter().enumerate() {
            let plan = shell_launch_plan(
                &directory.path().to_string_lossy(),
                ShellKind::Bash,
                None,
                "true",
                runner,
                bare,
            )
            .unwrap();
            if index == 0 {
                assert!(plan
                    .env
                    .contains(&("NO_PROXY".into(), "localhost,127.0.0.1".into())));
                assert!(!plan.env.iter().any(|(name, _)| name == "No_Proxy"));
            } else {
                let args = plan.args.join(" ");
                assert!(args.contains("NO_PROXY=localhost,127.0.0.1"), "{args}");
                assert!(args.contains("no_proxy=localhost,127.0.0.1"), "{args}");
                assert!(!args.contains("No_Proxy="), "{args}");
            }
        }
    }

    #[test]
    fn wsl_launch_plan_wraps_the_command_and_rejects_powershell() {
        use crate::run_environment::ShellRunner;
        let workspace = tempfile::tempdir().unwrap();
        let runner = ShellRunner::Wsl {
            agent_shell: Default::default(),
            distro: "Ubuntu".into(),
            env: [("FOO".to_owned(), "a b".to_owned())].into_iter().collect(),
        };

        // Remote legs carry no session: Claude Code has no remote runner to copy,
        // so they source no snapshot and report no directory.
        let cwd_file = workspace.path().join("cwd");
        let bare = ShellSession {
            snapshot: None,
            cwd_file: &cwd_file,
            temp_dir: None,
        };
        let plan = shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::Bash,
            None,
            "echo hi",
            &runner,
            bare,
        )
        .unwrap();
        assert_eq!(plan.candidates, vec!["wsl.exe"]);
        let workspace_arg = workspace.path().to_string_lossy().into_owned();
        assert_eq!(
            plan.args,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                workspace_arg.as_str(),
                "--exec",
                "/usr/bin/env",
                "FOO=a b",
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                "echo hi",
            ]
        );
        assert_eq!(plan.env, vec![("WSL_UTF8".to_owned(), "1".to_owned())]);
        assert!(!plan.local_hardening);

        let error = shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::PowerShell,
            None,
            "echo hi",
            &runner,
            bare,
        )
        .unwrap_err();
        assert!(error.contains("powershell") && error.contains("WSL"), "{error}");

        // zsh and sh reach the distribution the same way, each with its own
        // no-startup-files flags.
        let plan = shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::Zsh,
            Some("/usr/bin/zsh"),
            "echo hi",
            &runner,
            bare,
        )
        .unwrap();
        assert_eq!(
            plan.args[plan.args.len() - 4..],
            ["/usr/bin/zsh", "-f", "-c", "echo hi"]
        );
    }

    #[test]
    fn ssh_launch_plan_wraps_the_command_in_each_backends_own_invocation() {
        use crate::run_environment::ShellRunner;
        let workspace = tempfile::tempdir().unwrap();
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: "user@devbox".into(),
            port: 0,
            identity_file: String::new(),
            env: Default::default(),
        };

        let cwd_file = workspace.path().join("cwd");
        let bare = ShellSession {
            snapshot: None,
            cwd_file: &cwd_file,
            temp_dir: None,
        };
        let plan = shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::Bash,
            None,
            "pwd",
            &runner,
            bare,
        )
        .unwrap();
        assert!(plan.candidates.contains(&"ssh".to_owned()));
        assert_eq!(
            plan.args[..4],
            ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
        );
        assert_eq!(&plan.args[4..6], &["--", "user@devbox"]);
        assert!(!plan.local_hardening);

        assert_eq!(
            plan.args.last().unwrap(),
            &crate::remote_shell::posix_line(&format!(
                "cd {} || exit 1; exec bash --noprofile --norc -c 'pwd'",
                crate::run_environment::quote_remote_path(&workspace.path().to_string_lossy())
            ))
        );

        // A Windows machine's PowerShell travels base64-encoded, which its
        // login shell — cmd.exe or PowerShell — passes through untouched.
        let plan = shell_launch_plan(
            &workspace.path().to_string_lossy(),
            ShellKind::PowerShell,
            None,
            "Get-Location",
            &runner,
            bare,
        )
        .unwrap();
        let line = plan.args.last().unwrap();
        assert!(line.starts_with("powershell -NoLogo"), "{line}");
        assert!(line.contains("-EncodedCommand "), "{line}");
    }

    /// Configured variables must reach local commands. What the host no longer
    /// does is override them: the pager forcing, the `BASH_ENV` scrubbing and
    /// the exported-function stripping are gone, because Claude Code sets none
    /// of it and the shell now sources a snapshot of the user's own rc file
    /// anyway. A shell that deliberately replays the user's functions cannot
    /// also claim to be a sterile environment.
    #[test]
    fn local_runner_env_reaches_the_command_unmodified() {
        use crate::run_environment::ShellRunner;
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let runner = ShellRunner::Local {
            env: [
                ("MEWORK_RUN_ENV_PROBE".to_owned(), "probe-value".to_owned()),
                ("GIT_PAGER".to_owned(), "definitely-not-cat".to_owned()),
            ]
            .into_iter()
            .collect(),
        };
        let result = execute_with_scope_and_attachments(
            request(
                directory.path(),
                "bash",
                json!({"command": "printf '%s|%s' \"$MEWORK_RUN_ENV_PROBE\" \"$GIT_PAGER\""}),
            ),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &crate::workspace_set::WorkspaceSet::single(
                directory.path().to_string_lossy().into_owned(),
                runner,
            ),
            &PromptProfile::builtin_english(),
        );
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("probe-value|definitely-not-cat"),
            "the user's configured value is what the command sees, including for names the host used to force: {}",
            result.output
        );
    }

    /// A command has no deadline of its own: `npm install`, a test suite and a build all arrive
    /// through this tool and all routinely run for minutes. The old 30-second cap killed them
    /// mid-flight with no warning anywhere — the catalog never told the model a limit existed — so
    /// what the model saw was a build that "failed" at exactly 30 seconds, every time.
    ///
    /// 35 seconds is deliberately just past that former cap: this test fails if anyone reintroduces
    /// a wall-clock limit at or below it.
    #[test]
    fn a_command_may_run_past_the_deadline_that_used_to_kill_it() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();

        let started = Instant::now();
        let result = execute(
            request(
                directory.path(),
                "bash",
                json!({"command": "sleep 35; printf MEWORK_NO_SHELL_DEADLINE"}),
            ),
            &state,
        );

        assert!(
            started.elapsed() >= Duration::from_secs(35),
            "the command must have been allowed to run to completion: {:?}",
            started.elapsed()
        );
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("MEWORK_NO_SHELL_DEADLINE"),
            "the command ran to its end and its output survived: {}",
            result.output
        );
        assert!(
            !result.output.contains("deadline"),
            "nothing may report a deadline: {}",
            result.output
        );
    }

    /// Cancelling the run has to reach the command it started. The cancellation flag alone
    /// does not: the tool call is already blocked inside its own child process.
    #[test]
    fn cancelling_the_conversation_stops_its_running_commands() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let registry = state.shell_tasks.clone();
        let canceller = std::thread::spawn(move || {
            for _ in 0..600 {
                if registry.stop_conversation("conversation-test") > 0 {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        });

        let started = Instant::now();
        let result = execute(
            request(directory.path(), "bash", json!({"command": "sleep 20"})),
            &state,
        );
        assert!(canceller.join().unwrap(), "the task never became visible");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("<error>Command was aborted before completion</error>"),
            "{}",
            result.output
        );
    }

    /// A cancellation between dispatch and spawn must prevent the command from
    /// starting. The result must return immediately, say it did not start, and
    /// leave no registry row because unspawned commands are not tasks.
    #[test]
    fn a_command_dispatched_after_cancellation_never_spawns() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let (cancellation, _inbox) = state
            .begin_model_run("request-late", "conversation-test")
            .unwrap();
        assert!(state.cancel_model_run("request-late").unwrap());
        // The dispatcher mints this signal from the top-level run's own flag.
        let signal = CancelSignal::from_flag(cancellation);

        let started = Instant::now();
        let result = execute_with_scope_and_attachments_verified(
            request(directory.path(), "bash", json!({"command": "sleep 20"})),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &signal,
            &crate::workspace_set::WorkspaceSet::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "A stopped dispatch must return immediately: {:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(
            result.output.contains("command did not start"),
            "{}",
            result.output
        );
        assert!(
            state
                .shell_tasks
                .task_snapshots("conversation-test")
                .is_empty(),
            "An unspawned command must not leave a task row"
        );
    }

    /// Cancelling an unrelated foreground run must not stop a task-owned command.
    /// The command must survive run cancellation and end only when its task flag is set.
    #[test]
    fn an_unrelated_runs_cancellation_does_not_stop_a_tasks_command() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let task = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = CancelSignal::from_flag(Arc::clone(&task));

        // Wait for registration before cancelling the unrelated run, then observe
        // survival across multiple polling intervals before setting the task flag.
        let registry = state.shell_tasks.clone();
        let orchestrator_state = state.clone();
        let orchestrator = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let row = loop {
                if let Some(row) = registry.task_snapshots("conversation-test").pop() {
                    break row;
                }
                if Instant::now() >= deadline {
                    return Err("The command was never registered as a task row".to_owned());
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            let (_run_flag, _inbox) = orchestrator_state
                .begin_model_run("run-unrelated", "conversation-test")
                .unwrap();
            assert!(orchestrator_state
                .cancel_model_run("run-unrelated")
                .unwrap());
            // Observe continuously rather than sampling once: a mistaken stop must
            // traverse polling, tree kill, wait, and `guard.finish`, so every sample
            // must remain running across many polling intervals.
            let survived = (0..12).all(|_| {
                std::thread::sleep(Duration::from_millis(100));
                registry.is_running("conversation-test", &row.shell_task_id)
            });
            task.store(true, Ordering::Release);
            Ok(survived)
        });

        let result = execute_with_scope_and_attachments_verified(
            request(directory.path(), "bash", json!({"command": "sleep 20"})),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &signal,
            &crate::workspace_set::WorkspaceSet::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;
        let survived = orchestrator.join().unwrap().unwrap();
        assert!(
            survived,
            "Cancelling an unrelated foreground run killed the task command (cross-talk)"
        );
        // Task-flag shutdown must be recorded as stopped, not failed.
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("<error>Command was aborted before completion</error>"),
            "{}",
            result.output
        );
        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Stopped));
    }

    /// A task-level stop must reach commands running in that task's own turn.
    /// Only the task flag is set here; neither run-level cancellation nor the task-row
    /// stop flag reaches this command.
    #[test]
    fn a_task_level_stop_reaches_the_command_that_task_is_running() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let task = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = CancelSignal::from_flag(Arc::clone(&task));

        // Use registration as a barrier before stopping. A broad deadline avoids
        // treating process-start latency under load as a test failure.
        let registry = state.shell_tasks.clone();
        let stopper = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if !registry.task_snapshots("conversation-test").is_empty() {
                    task.store(true, Ordering::Release);
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        });

        let started = Instant::now();
        let result = execute_with_scope_and_attachments_verified(
            request(directory.path(), "bash", json!({"command": "sleep 20"})),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &signal,
            &crate::workspace_set::WorkspaceSet::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;
        assert!(
            stopper.join().unwrap(),
            "The command was never registered as a task row"
        );

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "A task-level stop must kill the command within one polling interval, not wait for 20 seconds: {:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("<error>Command was aborted before completion</error>"),
            "{}",
            result.output
        );
        // A user-initiated stop must not be reported as failure, which would invite
        // an automatic retry of the command the user stopped.
        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Stopped));
    }

    /// Pins the encoding behaviour Claude Code parity gives back, so nobody
    /// mistakes it for an unnoticed regression.
    ///
    /// This project fixed all of it once. `Get-Content` on a BOM-less UTF-8
    /// source returned GBK mojibake with the newline after a comment swallowed,
    /// `Format-Table` elided every `FullName` to `...`, and a `Select-String`
    /// hit was split mid-word at column 120. The fix was three `[Console]`
    /// encodings, per-cmdlet UTF-8 defaults, and a 4096-column screen buffer.
    ///
    /// Claude Code's prologue has none of that, and the user's decision was
    /// byte-for-byte parity, so it is gone. The assertions below are therefore
    /// deliberately weak about *content*: what they still guarantee is that the
    /// call succeeds, that CRLF is folded, and that nothing is silently dropped.
    /// A future change that restores correct decoding should tighten this test
    /// rather than delete it — the exact-equality assertions it used to make are
    /// preserved in the comment above.
    #[cfg(windows)]
    #[test]
    fn powershell_encoding_limits_are_inherited_from_claude_code() {
        let directory = tempfile::tempdir().unwrap();
        let source = "// 殉爆连锁：一圈能量爆点\n      if (E.beams) {\n      }\n";
        fs::write(directory.path().join("mechs.js"), source).unwrap();
        let state = AppState::default();
        let run = |command: &str| {
            execute_with_scope_and_attachments_verified(
                request(directory.path(), "powershell", json!({"command": command})),
                &state,
                ExecutionScope::workspace_only(directory.path()),
                None,
                &CancelSignal::default(),
                &crate::workspace_set::WorkspaceSet::default(),
                None,
                &PromptProfile::builtin_english(),
            )
            .result
        };

        // The call works; only the decoding is lossy. Under Windows PowerShell
        // 5.1 the CJK comment comes back as mojibake and may swallow the newline
        // after it, which is why nothing here asserts the text round-trips.
        let read = run("Get-Content mechs.js -Tail 3");
        assert!(read.success, "{}", read.output);
        assert!(read.output.contains("if (E.beams)"), "{}", read.output);
        // ASCII is unaffected by the code page, so a pure-ASCII file still
        // round-trips exactly and would catch a wholesale breakage of the read
        // path. It has to be its own file: in `mechs.js` the mojibake swallows
        // the newline after the comment, so `-Tail` counts a different number
        // of lines than the file has.
        fs::write(directory.path().join("ascii.txt"), "alpha\nbeta\n").unwrap();
        let ascii = run("Get-Content ascii.txt");
        assert!(ascii.success, "{}", ascii.output);
        assert_eq!(ascii.output, "alpha\nbeta");

        // CRLF folding is a model-facing concern, not a console one, so it
        // survives the parity change and is still guaranteed.
        assert!(
            !read.output.contains('\r'),
            "CRLF is folded for the model:\n{}",
            read.output
        );
    }

    /// `powershell` and `bash` use separate dispatch arms, so each must independently
    /// prove that a task-level stop reaches its running command. The PowerShell arm
    /// only runs a command on a Windows host.
    #[cfg(windows)]
    #[test]
    fn a_task_level_stop_reaches_the_command_the_powershell_arm_is_running() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let task = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = CancelSignal::from_flag(Arc::clone(&task));

        let registry = state.shell_tasks.clone();
        let stopper = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if !registry.task_snapshots("conversation-test").is_empty() {
                    task.store(true, Ordering::Release);
                    // Measure from the stop request: slow spawning is not slow stop
                    // response, and must not consume the assertion window.
                    return Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            None
        });

        let result = execute_with_scope_and_attachments_verified(
            request(
                directory.path(),
                "powershell",
                json!({"command": "Start-Sleep -Seconds 20"}),
            ),
            &state,
            ExecutionScope::workspace_only(directory.path()),
            None,
            &signal,
            &crate::workspace_set::WorkspaceSet::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;
        let stopped_at = stopper
            .join()
            .unwrap()
            .expect("The command was never registered as a task row");

        assert!(
            stopped_at.elapsed() < Duration::from_secs(10),
            "A task-level stop must kill the PowerShell command within one polling interval: {:?}",
            stopped_at.elapsed()
        );
        assert!(!result.success);
        assert!(
            result
                .output
                .contains("<error>Command was aborted before completion</error>"),
            "{}",
            result.output
        );
        let finished = state.shell_tasks.task_snapshots("conversation-test");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].outcome, Some(ShellTaskOutcome::Stopped));
    }

    /// A shell that spawned children is the whole reason a command runs long. Killing only
    /// the shell would leave them holding the workspace and writing to pipes nobody reads.
    #[test]
    fn stopping_a_command_kills_the_children_it_spawned() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("grandchild-still-running");
        let state = AppState::default();
        let registry = state.shell_tasks.clone();
        let stopper = std::thread::spawn(move || {
            for _ in 0..600 {
                let snapshots = registry.task_snapshots("conversation-test");
                if let Some(task) = snapshots.first() {
                    return registry.request_stop("conversation-test", &task.shell_task_id);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        });

        let started = Instant::now();
        execute(
            request(
                directory.path(),
                "bash",
                // The grandchild outlives the shell unless the whole tree is killed, and proves
                // it by touching the marker after the parent is long gone. Its stdio is detached
                // so it cannot hold the pipes open and stall the collector instead.
                json!({
                    "command":
                        "(sleep 3; touch grandchild-still-running) >/dev/null 2>&1 </dev/null & sleep 20"
                }),
            ),
            &state,
        );
        assert!(stopper.join().unwrap(), "the task never became visible");
        // The kill must also be prompt: a tree that dies only when the command would have
        // finished anyway is not a stop button.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "stopping took {:?}",
            started.elapsed()
        );

        // Past the grandchild's own sleep, so a surviving one has had its chance to write.
        std::thread::sleep(Duration::from_secs(5));
        assert!(
            !marker.exists(),
            "the grandchild outlived the stop and kept working"
        );
    }

    #[test]
    fn read_image_returns_external_attachment_metadata_before_receipt() {
        let workspace = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("pixel.png"), TEST_PNG).unwrap();
        let request = request(workspace.path(), "read", json!({"path":"pixel.png"}));
        let scope = ExecutionScope::workspace_only(workspace.path());
        let state = AppState::default();
        let result = execute_with_scope_and_attachments(
            request.clone(),
            &state,
            scope,
            Some(app_data.path()),
            &crate::workspace_set::WorkspaceSet::default(),
            &PromptProfile::builtin_english(),
        );
        assert!(result.success, "{}", result.output);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime, "image/png");
        assert!(ImageAttachmentStore::new(app_data.path())
            .data_url(&result.images[0])
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(state.has_receipt(
            &request.workspace_path,
            &request.conversation_id,
            &request.tool_name,
            &request.input,
            &result,
        ));
    }

    #[test]
    fn read_oversized_supported_image_reports_the_image_limit() {
        let workspace = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let path = workspace.path().join("oversized.png");
        let mut file = File::create(&path).unwrap();
        std::io::Write::write_all(&mut file, b"\x89PNG\r\n\x1a\n\0\0\0\r").unwrap();
        file.set_len(crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64 + 1)
            .unwrap();
        drop(file);

        let request = request(workspace.path(), "read", json!({"path":"oversized.png"}));
        let scope = ExecutionScope::workspace_only(workspace.path());
        let result = execute_with_scope_and_attachments(
            request,
            &AppState::default(),
            scope,
            Some(app_data.path()),
            &crate::workspace_set::WorkspaceSet::default(),
            &PromptProfile::builtin_english(),
        );
        assert!(!result.success);
        assert!(
            result.output.contains("Image exceeds the 5 MiB limit"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("Text file"), "{}", result.output);
        assert!(result.images.is_empty());
    }
    #[test]
    fn a_preview_screenshot_reaches_the_model_as_a_conversation_attachment() {
        let app_data = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(app_data.path());
        let capture = crate::browser::PreviewScreenshot {
            data: base64::engine::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                TEST_PNG,
            ),
            width: 1,
            height: 1,
        };

        let outcome = preview_screenshot_outcome(capture, Some(&store)).unwrap();

        assert_eq!(outcome.images.len(), 1);
        let output: Value = serde_json::from_str(&outcome.output).unwrap();
        assert_eq!(output.get("width").and_then(Value::as_u64), Some(1));
        assert_eq!(output.get("height").and_then(Value::as_u64), Some(1));
        // No workspace path is involved at all: the pixels never touch the user's checkout.
        assert!(!outcome.output.contains(".png"));
    }

    /// The refusals the source answers with when nothing resolves. Getting one of them wrong
    /// tells the model to call a tool that will not help.
    #[test]
    fn an_unresolvable_server_id_answers_with_the_ported_refusals() {
        let workspace = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let request = |input: Value| ToolExecutionRequest {
            conversation_id: "preview-target".into(),
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            tool_name: "preview_snapshot".into(),
            input: input.as_object().unwrap().clone(),
        };

        let stale = resolve_preview_page_session(
            &request(json!({"serverId": "preview-gone"})),
            &state,
            workspace.path(),
        )
        .unwrap_err();
        assert_eq!(stale, preview_stale_server_id("preview-gone"));
        assert!(stale.starts_with("serverId \"preview-gone\" not found "));
        assert!(stale.ends_with("Call preview_list to get current ids."));

        // No servers and no attached pane: the source's non-gated answer.
        let nothing = resolve_preview_page_session(&request(json!({})), &state, workspace.path())
            .unwrap_err();
        assert_eq!(nothing, PREVIEW_NO_SERVERS);
        assert!(PREVIEW_PANE_NOT_OPEN.starts_with("No preview is open."));
        assert!(PREVIEW_PANE_NOT_OPEN.ends_with("from .mework/launch.json."));

        // A tab session is not a conversation, so it can never address another surface.
        let mut tabbed = request(json!({}));
        tabbed.conversation_id = "preview-target#tab_9f3c".into();
        assert!(
            resolve_preview_page_session(&tabbed, &state, workspace.path())
                .unwrap_err()
                .contains("not a tab session")
        );
    }

    /// `preview_start`'s own description promises that an entry with a url and no
    /// command attaches to an already-running server. This is that promise end to
    /// end: the file on disk, the resolution, the start, and the text the model
    /// reads — which used to be the registry's "has no command" refusal.
    #[test]
    fn an_attach_configuration_reports_an_attachment_instead_of_refusing_it() {
        let workspace = tempfile::tempdir().unwrap();
        let mework = workspace.path().join(".mework");
        std::fs::create_dir_all(&mework).unwrap();
        std::fs::write(
            mework.join("launch.json"),
            r#"{"configurations":[{"name":"docs","url":"https://example.com/docs"}]}"#,
        )
        .unwrap();
        let state = AppState::default();

        let local = crate::workspace_set::WorkspaceSet::local_root(
            workspace.path().to_string_lossy().into_owned(),
        );
        let started = run_preview_start(
            &request(workspace.path(), "preview_start", json!({"name": "docs"})),
            &state,
            local.select(None).unwrap(),
            workspace.path(),
        )
        .unwrap();

        // Nothing but the attach sentence and where the page went: the id is the name the model
        // passed, so it is not repeated back.
        assert_eq!(
            started.output,
            format!(
                "{}\nThe configured url https://example.com/docs could not be opened.",
                crate::preview::ATTACHED_NOTICE
            )
        );
        assert!(!started.output.contains("has no command"));
        // Nothing was spawned, and the navigation leg is the same one every other
        // preview url goes through — which in a test has no window to open.
        assert!(state.preview_servers.servers().is_empty());

        // The name is a page, not a process, and both id-taking tools say so.
        let stop_refusal = run_preview_stop(
            &request(
                workspace.path(),
                "preview_stop",
                json!({ "serverId": "docs" }),
            ),
            &state,
            &local,
        )
        .err()
        .expect("an attach entry is not a process");
        assert_eq!(stop_refusal, crate::preview::no_stop_for_attachment("docs"));
        let logs = run_preview_logs(
            &request(
                workspace.path(),
                "preview_logs",
                json!({ "serverId": "docs" }),
            ),
            &state,
            &local,
        )
        .unwrap();
        assert_eq!(logs.output, crate::preview::NO_SERVER_FOR_LOGS);
    }

    /// Two workspaces that each run a `dev`: the list says which workspace each is in, the bare
    /// id is refused as ambiguous, and the workspace number settles it. A name only one entry has
    /// comes back as a bare outcome; a name the file repeats comes back with its numbered id.
    #[cfg(unix)]
    #[test]
    fn servers_are_addressed_by_their_name_and_the_workspace_they_run_in() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let dev = r#"{"name":"dev","runtimeExecutable":"/bin/sh","runtimeArgs":["-c","sleep 30"],"port":0,"autoPort":true}"#;
        let docs = r#"{"name":"docs","url":"https://example.com/docs"}"#;
        for (root, entries) in [
            (first.path(), format!("{dev},{docs},{docs}")),
            (second.path(), dev.to_owned()),
        ] {
            let directory = root.join(".mework");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("launch.json"),
                format!(r#"{{"configurations":[{entries}]}}"#),
            )
            .unwrap();
        }
        let attached = |root: &Path| crate::model::AttachedWorkspace {
            machine: None,
            path: root.to_string_lossy().into_owned(),
        };
        let workspaces = crate::workspace_set::WorkspaceSet::resolve(
            &crate::model::ExecutionEnvironmentAssets::default(),
            &attached(first.path()),
            &[attached(second.path())],
        )
        .unwrap();
        let state = AppState::default();
        let call = |tool: &str, input: Value| request(first.path(), tool, input);
        let start = |name: &str, number: u32| {
            let selected = workspaces.select(Some(number)).unwrap();
            run_preview_start(
                &call(
                    "preview_start",
                    json!({ "name": name, "workspace": number }),
                ),
                &state,
                selected,
                Path::new(&selected.root),
            )
            .unwrap()
            .output
        };

        for number in [1, 2] {
            let receipt = start("dev", number);
            assert!(
                receipt.starts_with("Server started successfully"),
                "{receipt}"
            );
            assert!(!receipt.contains("serverId"), "{receipt}");
        }
        let numbered = start("docs", 1);
        assert!(
            numbered.starts_with("This server's serverId is \"docs-1\""),
            "{numbered}"
        );

        let listed: Value = serde_json::from_str(
            &run_preview_list(&call("preview_list", json!({})), &state, &workspaces)
                .unwrap()
                .output,
        )
        .unwrap();
        let listed = listed.as_array().unwrap();
        let addresses: Vec<(&Value, &Value)> = listed
            .iter()
            .map(|server| (&server["serverId"], &server["workspace"]))
            .collect();
        assert_eq!(
            addresses,
            [(&json!("dev"), &json!(1)), (&json!("dev"), &json!(2))]
        );
        for hidden in ["handle", "sessionId", "name", "cwd"] {
            assert!(listed[0].get(hidden).is_none(), "{hidden}: {}", listed[0]);
        }

        let ambiguous = run_preview_stop(
            &call("preview_stop", json!({ "serverId": "dev" })),
            &state,
            &workspaces,
        )
        .err()
        .expect("the call is refused");
        assert_eq!(
            ambiguous,
            "Servers with id \"dev\" run in workspaces 1, 2. Pass workspace to say which."
        );
        let stopped = run_preview_stop(
            &call("preview_stop", json!({ "serverId": "dev", "workspace": 2 })),
            &state,
            &workspaces,
        )
        .unwrap();
        assert_eq!(stopped.output, "Server dev stopped");
        let elsewhere = run_preview_logs(
            &call("preview_logs", json!({ "serverId": "dev", "workspace": 2 })),
            &state,
            &workspaces,
        )
        .err()
        .expect("the call is refused");
        assert_eq!(
            elsewhere,
            "No server \"dev\" in workspace 2. It runs in workspace 1; pass that as workspace."
        );
        // One is left, so the bare id is enough again.
        let logs = run_preview_logs(
            &call("preview_logs", json!({ "serverId": "dev" })),
            &state,
            &workspaces,
        )
        .unwrap();
        assert_eq!(logs.output, "No logs yet.");
        let attach = run_preview_stop(
            &call("preview_stop", json!({ "serverId": "docs-1" })),
            &state,
            &workspaces,
        )
        .err()
        .expect("the call is refused");
        assert_eq!(attach, crate::preview::no_stop_for_attachment("docs-1"));
        state.preview_servers.stop_all();
    }

    // ---- File write guards ---------------------------------------------------

    fn guard(registry: &FileReadRegistry) -> FileGuardContext<'_> {
        FileGuardContext {
            scope: ScopeRef {
                id: "conversation-test",
                parent: None,
            },
            registry,
        }
    }

    /// Reads `name` through the tool and commits the record the way the run
    /// loop does once the result is final.
    fn read_and_commit(workspace: &Path, name: &str, guard: FileGuardContext<'_>) {
        let scope = ExecutionScope::workspace_only(workspace);
        let outcome = run_read(
            workspace,
            &object(json!({ "path": name })),
            &scope,
            None,
            &PromptProfile::builtin_english(),
            Some(guard),
        )
        .unwrap();
        let touch = outcome.file_touch.expect("a guarded read reports its file");
        guard.record(touch.path, touch.read.expect("a read carries its record"));
    }

    #[test]
    fn the_read_gate_refuses_unread_files_and_lets_new_ones_through() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a.txt"), "alpha\n").unwrap();
        let registry = FileReadRegistry::default();
        let guard = guard(&registry);
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        let edit = object(json!({ "path": "a.txt", "find": "alpha", "replace": "beta" }));
        assert_eq!(
            run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).err().unwrap(),
            FILE_NOT_READ
        );
        let overwrite = object(json!({ "path": "a.txt", "content": "beta\n" }));
        assert_eq!(
            run_write(directory.path(), &overwrite, &scope, &profile, Some(guard)).err().unwrap(),
            FILE_NOT_READ
        );
        assert_eq!(fs::read_to_string(directory.path().join("a.txt")).unwrap(), "alpha\n");
        // A file that does not exist yet needs no read.
        let create = object(json!({ "path": "new.txt", "content": "fresh\n" }));
        let created =
            run_write(directory.path(), &create, &scope, &profile, Some(guard)).unwrap();
        assert!(created.output.ends_with(profile.text(PromptKey::ToolFileStateCurrent)));
        // And the write recorded itself, so an edit follows without a read.
        let follow_up = object(json!({ "path": "new.txt", "find": "fresh", "replace": "newer" }));
        run_edit(directory.path(), &follow_up, &scope, &profile, Some(guard)).unwrap();
        assert_eq!(fs::read_to_string(directory.path().join("new.txt")).unwrap(), "newer\n");

        read_and_commit(directory.path(), "a.txt", guard);
        let edited = run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).unwrap();
        assert!(edited.output.ends_with(profile.text(PromptKey::ToolFileStateCurrent)));
        assert_eq!(fs::read_to_string(directory.path().join("a.txt")).unwrap(), "beta\n");
    }

    #[test]
    fn a_write_that_creates_a_file_records_the_text_the_model_supplied() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        let registry = FileReadRegistry::default();
        let guard = guard(&registry);
        let scope = ExecutionScope::workspace_only(directory.path());
        // The read gate is unconditional now, so a file that does not exist yet
        // is the only way a writer legitimately reaches one nothing has read.
        let create = object(json!({ "path": "a.txt", "content": "alpha\n" }));
        let written = run_write(
            directory.path(),
            &create,
            &scope,
            &PromptProfile::builtin_english(),
            Some(guard),
        )
        .unwrap();
        assert!(written.output.ends_with(
            PromptProfile::builtin_english().text(PromptKey::ToolFileStateCurrent)
        ));
        let record = guard.get(&fs::canonicalize(&path).unwrap()).unwrap();
        assert!(record.full);
        assert!(
            record.in_model_context,
            "a file this run wrote outright holds exactly the text the model supplied"
        );
    }

    #[test]
    fn a_stale_edit_is_refused_unless_the_text_is_unchanged_or_the_find_text_still_applies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let registry = FileReadRegistry::default();
        let guard = guard(&registry);
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        read_and_commit(directory.path(), "a.txt", guard);
        let canonical = fs::canonicalize(&path).unwrap();
        let fresh = guard.get(&canonical).unwrap();

        // The disk moved on but holds exactly what was read: not stale.
        guard.record(
            canonical.clone(),
            FileReadRecord {
                modified_ms: fresh.modified_ms - 1_000,
                ..fresh.clone()
            },
        );
        let edit = object(json!({ "path": "a.txt", "find": "two", "replace": "2" }));
        let edited = run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).unwrap();
        assert!(edited.output.ends_with(profile.text(PromptKey::ToolFileStateCurrent)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n2\nthree\n");

        // Someone else changed the file, but the find text still matches once:
        // Claude Code's stale recovery applies the edit and says so.
        let now = guard.get(&canonical).unwrap();
        guard.record(
            canonical.clone(),
            FileReadRecord::full_read(now.modified_ms - 1_000, "one\n2\nthree\nold\n".into()),
        );
        let edit = object(json!({ "path": "a.txt", "find": "three", "replace": "3" }));
        let edited = run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).unwrap();
        assert!(edited.output.ends_with(profile.text(PromptKey::ToolEditStaleRecovered)));
        assert!(!guard.get(&canonical).unwrap().in_model_context);

        // Changed on disk and the find text is gone: read it again.
        let now = guard.get(&canonical).unwrap();
        guard.record(
            canonical.clone(),
            FileReadRecord::full_read(now.modified_ms - 1_000, "something else\n".into()),
        );
        let edit = object(json!({ "path": "a.txt", "find": "missing", "replace": "x" }));
        assert_eq!(
            run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).err().unwrap(),
            FILE_MODIFIED_SINCE_READ
        );
        // `write` has no recovery: a changed file is always refused.
        let overwrite = object(json!({ "path": "a.txt", "content": "anything\n" }));
        assert_eq!(
            run_write(directory.path(), &overwrite, &scope, &profile, Some(guard)).err().unwrap(),
            FILE_MODIFIED_SINCE_READ
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n2\n3\n");
    }

    #[test]
    fn a_ranged_read_satisfies_the_read_gate_but_cannot_rescue_a_stale_check() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let registry = FileReadRegistry::default();
        let guard = guard(&registry);
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        let outcome = run_read(
            directory.path(),
            &object(json!({ "path": "a.txt", "start_line": 2, "end_line": 2 })),
            &scope,
            None,
            &profile,
            Some(guard),
        )
        .unwrap();
        let touch = outcome.file_touch.unwrap();
        let record = touch.read.unwrap();
        assert!(!record.full);
        assert!(record.content.is_none());
        guard.record(touch.path.clone(), record);
        let edit = object(json!({ "path": "a.txt", "find": "two", "replace": "2" }));
        run_edit(directory.path(), &edit, &scope, &profile, Some(guard)).unwrap();
        // Age the (now full, post-edit) record and drop it to a slice again: a
        // stale slice cannot vouch for the whole file, so the write is refused
        // even though the disk still holds what the model wrote.
        let written = guard.get(&touch.path).unwrap();
        guard.record(
            touch.path.clone(),
            FileReadRecord::partial_read(written.modified_ms - 1_000),
        );
        let overwrite = object(json!({ "path": "a.txt", "content": "x\n" }));
        assert_eq!(
            run_write(directory.path(), &overwrite, &scope, &profile, Some(guard)).err().unwrap(),
            FILE_MODIFIED_SINCE_READ
        );
    }

    #[test]
    fn edit_matches_lf_find_text_in_a_crlf_file_and_writes_crlf_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dos.txt");
        fs::write(&path, "\u{feff}alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        let edit = object(json!({ "path": "dos.txt", "find": "beta\ngamma", "replace": "b\ng\nh" }));
        run_edit(directory.path(), &edit, &scope, &profile, None).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}alpha\r\nb\r\ng\r\nh\r\n"
        );
        // A raw match leaves the rest of the file byte for byte, mixed endings
        // included.
        fs::write(&path, "one\r\ntwo\nthree\r\n").unwrap();
        let edit = object(json!({ "path": "dos.txt", "find": "two", "replace": "2" }));
        run_edit(directory.path(), &edit, &scope, &profile, None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\r\n2\nthree\r\n");
        // Ambiguity is still ambiguity after normalization.
        fs::write(&path, "x\r\nx\r\n").unwrap();
        let edit = object(json!({ "path": "dos.txt", "find": "x\n", "replace": "y\n" }));
        assert!(run_edit(directory.path(), &edit, &scope, &profile, None)
            .err()
            .unwrap()
            .contains("occurs 2 times"));
    }

    #[test]
    fn an_unguarded_call_records_and_checks_nothing() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a.txt"), "alpha\n").unwrap();
        let scope = ExecutionScope::workspace_only(directory.path());
        let profile = PromptProfile::builtin_english();
        let read = run_read(
            directory.path(),
            &object(json!({ "path": "a.txt" })),
            &scope,
            None,
            &profile,
            None,
        )
        .unwrap();
        assert!(read.file_touch.is_none());
        let edit = object(json!({ "path": "a.txt", "find": "alpha", "replace": "beta" }));
        let edited = run_edit(directory.path(), &edit, &scope, &profile, None).unwrap();
        assert_eq!(edited.output, "ok");
        assert!(edited.file_touch.is_none());
    }

    #[test]
    fn formatter_commands_are_recognised_by_the_same_markers_as_the_source() {
        for command in [
            "prettier --write src/",
            "eslint . --fix",
            "cargo fmt",
            "cargo fix --allow-dirty",
            "npm run format",
            "pnpm format",
            "black .",
            "ruff format app",
            "gofmt -w . && go fmt ./...",
            "sed --in-place 's/a/b/' f",
        ] {
            assert!(looks_like_formatter_command(command), "{command}");
        }
        for command in ["cargo test", "git status", "echo hi > file", "npm run build", "cat a.txt"] {
            assert!(!looks_like_formatter_command(command), "{command}");
        }
    }

    #[test]
    fn a_formatter_command_names_the_read_files_it_changed() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt"] {
            fs::write(workspace.join(name), "x\n").unwrap();
        }
        fs::write(workspace.join("untouched.txt"), "x\n").unwrap();
        // Written well before "the command" starts below, so its time can move
        // past its record without ever moving past the command.
        File::options()
            .write(true)
            .open(workspace.join("untouched.txt"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let registry = FileReadRegistry::default();
        let guard = guard(&registry);
        let profile = PromptProfile::builtin_english();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt", "untouched.txt"] {
            read_and_commit(workspace, name, guard);
        }
        // Age every record so the seven files count as changed after "the
        // command" ran. The untouched one is aged too and must still not
        // appear: its time did not move past the command's start.
        for (path, record) in registry.full_records(guard.scope) {
            registry.record(
                guard.scope,
                path,
                FileReadRecord {
                    modified_ms: record.modified_ms - 10_000,
                    ..record
                },
            );
        }
        let started = file_read_state::now_ms() - 5_000;
        let hint = stale_read_hint(Some(guard), workspace, "cargo fmt", started, &profile)
            .expect("seven changed files produce a hint");
        assert!(hint.starts_with("[This command modified 7 file(s)"), "{hint}");
        assert!(hint.contains("a.txt, b.txt, c.txt, d.txt, e.txt and 2 more"), "{hint}");
        assert!(!hint.contains("untouched"), "{hint}");
        assert!(hint.ends_with("Call read before editing.]"), "{hint}");
        assert!(
            stale_read_hint(Some(guard), workspace, "cargo test", started, &profile).is_none(),
            "only formatter-looking commands hint"
        );
        // Without a guard at all — direct IPC, or a test — there is no record
        // to compare against, so there is nothing to hint about.
        assert!(stale_read_hint(None, workspace, "cargo fmt", started, &profile).is_none());
    }
}

