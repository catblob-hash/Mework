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
#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

use crate::{
    browser::{BrowserPngCapture, BrowserSession},
    cancel::CancelSignal,
    image_attachments::{is_supported_image, ImageAttachmentStore},
    model::{ImageAttachment, JsonObject, ToolExecutionRequest, ToolExecutionResponse, ToolResult},
    path_guard::{
        canonical_workspace, existing_path_is_allowed, prepare_secure_write_with_scope,
        relative_display, resolve_existing_with_scope, resolve_for_write_with_scope,
        secure_open_existing_file_with_scope, ExecutionScope, SecureWriteAuthority,
    },
    prompt_profile::{PromptKey, PromptProfile},
    shell_tasks::{ShellOutputSink, ShellOutputStream, ShellTaskOutcome},
    state::AppState,
    storage::atomic_write,
};

const MAX_TOOL_OUTPUT: usize = 64 * 1024;
const MAX_DIFF_OUTPUT: usize = 64 * 1024;
const MAX_TEXT_FILE: u64 = 2 * 1024 * 1024;
const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PATH_CHARS: usize = 4096;
const MAX_COMMAND_CHARS: usize = 64 * 1024;
const MAX_LIST_ENTRIES: usize = 2_000;
const MAX_SEARCH_MATCHES: usize = 1_000;
/// How often a running command checks whether someone asked it to stop. This is what bounds the
/// delay between pressing the stop button and the process tree dying, and — since a command has no
/// deadline of its own — it is the only thing standing between a runaway build and the user.
const SHELL_STOP_POLL: Duration = Duration::from_millis(100);

struct Outcome {
    success: bool,
    output: String,
    images: Vec<ImageAttachment>,
    diff: Option<String>,
    opened_file: Option<PathBuf>,
}

impl Outcome {
    fn success(output: String) -> Self {
        Self {
            success: true,
            output,
            images: Vec::new(),
            diff: None,
            opened_file: None,
        }
    }

    fn success_with_images(output: String, images: Vec<ImageAttachment>) -> Self {
        Self {
            success: true,
            output,
            images,
            diff: None,
            opened_file: None,
        }
    }

    fn success_with_diff(output: String, diff: Option<String>) -> Self {
        Self {
            success: true,
            output,
            images: Vec::new(),
            diff,
            opened_file: None,
        }
    }

    fn with_opened_file(mut self, opened_file: PathBuf) -> Self {
        self.opened_file = Some(opened_file);
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
}

#[allow(dead_code)] // Backward-compatible workspace-only execution entry point.
pub fn execute(request: ToolExecutionRequest, state: &AppState) -> ToolExecutionResponse {
    let scope = ExecutionScope::workspace_only(Path::new(&request.workspace_path));
    execute_with_scope(request, state, scope)
}

/// Executes a request with the filesystem boundary selected by the trusted
/// security classifier. Callers must still honor `requires_approval` before
/// passing the corresponding scope here; this function enforces the boundary
/// but does not display or verify approval UI itself.
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
        &crate::run_environment::ShellRunner::default(),
        &PromptProfile::default(),
    )
}

/// Executes with an optional trusted attachment root. Image-producing tools
/// require this root to make their pixels available to later model rounds.
///
/// `runner` is the trusted shell environment resolved by the host. Only `bash` and
/// `powershell` use it. Tests and backward-compatible callers use the default
/// local runner with no injected variables.
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
    runner: &crate::run_environment::ShellRunner,
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
        runner,
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
    runner: &crate::run_environment::ShellRunner,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
) -> VerifiedToolExecutionResponse {
    let started = Instant::now();
    let attachment_store = app_data.map(ImageAttachmentStore::new);
    let result = run_tool(
        &request,
        &scope,
        state,
        attachment_store.as_ref(),
        cancel,
        runner,
        app_data,
        handoff,
        profile,
    )
    .unwrap_or_else(|error| Outcome {
            success: false,
            output: error,
            images: Vec::new(),
            diff: None,
            opened_file: None,
        });
    finish_verified_execution(&request, state, started, result, profile)
}

fn finish_execution(
    request: &ToolExecutionRequest,
    state: &AppState,
    started: Instant,
    result: Outcome,
    profile: &PromptProfile,
) -> ToolExecutionResponse {
    finish_verified_execution(request, state, started, result, profile).result
}

fn finish_verified_execution(
    request: &ToolExecutionRequest,
    state: &AppState,
    started: Instant,
    result: Outcome,
    profile: &PromptProfile,
) -> VerifiedToolExecutionResponse {
    let opened_file = result.opened_file;
    let response = ToolExecutionResponse {
        success: result.success,
        output: truncate_output(&result.output, profile),
        images: result.images,
        diff: result.diff.map(|diff| truncate_diff(&diff, profile)),
        executed_at: Utc::now().to_rfc3339(),
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    };
    state.record_receipt(request, &response);
    VerifiedToolExecutionResponse {
        result: response,
        opened_file: opened_file.map(VerifiedOpenedFile),
    }
}

fn run_tool(
    request: &ToolExecutionRequest,
    scope: &ExecutionScope,
    state: &AppState,
    attachment_store: Option<&ImageAttachmentStore>,
    cancel: &CancelSignal,
    runner: &crate::run_environment::ShellRunner,
    app_data: Option<&Path>,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
) -> Result<Outcome, String> {
    let workspace = Path::new(&request.workspace_path);
    match request.tool_name.as_str() {
        "ls" => run_ls(workspace, &request.input, scope, profile).map(Outcome::success),
        "grep" => run_grep(workspace, &request.input, scope, profile).map(Outcome::success),
        "powershell" => run_shell(
            workspace,
            &request.input,
            ShellKind::PowerShell,
            &request.conversation_id,
            state,
            cancel,
            runner,
            app_data,
            handoff,
            profile,
        ),
        "bash" => run_shell(
            workspace,
            &request.input,
            ShellKind::Bash,
            &request.conversation_id,
            state,
            cancel,
            runner,
            app_data,
            handoff,
            profile,
        ),
        "write" => run_write(workspace, &request.input, scope, profile),
        "edit" => run_edit(workspace, &request.input, scope, profile),
        "find" => run_find(workspace, &request.input, scope, profile).map(Outcome::success),
        "read" => run_read(workspace, &request.input, scope, attachment_store, profile),
        // One wire tool, 23 actions. The action decides everything downstream, so it is parsed
        // once here and an unknown one is refused rather than falling through to a page call.
        "playwright" => {
            let action = crate::browser::PlaywrightAction::from_input(&request.input)?;
            match action {
                // Tab actions create, retarget, and destroy sessions, so they address the
                // conversation itself rather than whichever tab the page-level actions act on.
                _ if action.is_tab_action() => {
                    let value = state.browser.execute_agent_tab_tool(
                        &request.conversation_id,
                        action,
                        &request.input,
                    )?;
                    encode_browser_tool_result(&value)
                }
                crate::browser::PlaywrightAction::Screenshot => run_browser_screenshot(
                    workspace,
                    &agent_browser_session(&request.conversation_id, state)?,
                    &request.input,
                    scope,
                    state,
                    attachment_store,
                ),
                crate::browser::PlaywrightAction::FileUpload => run_browser_file_upload(
                    workspace,
                    &agent_browser_session(&request.conversation_id, state)?,
                    &request.input,
                    scope,
                    state,
                ),
                // Resolving the image number needs the run loop's transcript, so the
                // loop calls `execute_browser_upload_image` directly; any other route
                // into this action has no attachment to upload.
                crate::browser::PlaywrightAction::UploadImage => Err(
                    "playwright upload_image requires the model run loop to resolve the image number from the conversation before execution".into(),
                ),
                _ => {
                    let value = state.browser.execute_tool_blocking(
                        &agent_browser_session(&request.conversation_id, state)?,
                        action,
                        &request.input,
                        &crate::browser::BrowserToolGrants::default(),
                    )?;
                    encode_browser_tool_result(&value)
                }
            }
        }
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


/// Session every page-level browser tool of one conversation acts on. Without an explicit
/// `playwright tab_select` this stays the conversation's own page, so the historical
/// one-page-per-conversation behaviour is exactly the default.
fn agent_browser_session(conversation_id: &str, state: &AppState) -> Result<String, String> {
    state.browser.agent_tab_session_id(conversation_id)
}

fn encode_browser_tool_result(value: &serde_json::Value) -> Result<Outcome, String> {
    serde_json::to_string_pretty(value)
        .map(Outcome::success)
        .map_err(|error| format!("Failed to encode browser tool result: {error}"))
}

fn run_browser_screenshot(
    workspace: &Path,
    session_id: &str,
    input: &JsonObject,
    scope: &ExecutionScope,
    state: &AppState,
    attachment_store: Option<&ImageAttachmentStore>,
) -> Result<Outcome, String> {
    let requested = required_string(input, "path", MAX_PATH_CHARS, false)?;
    if Path::new(&requested)
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or(true, |extension| !extension.eq_ignore_ascii_case("png"))
    {
        return Err("playwright screenshot path must end in .png".into());
    }
    let full_page = optional_bool(input, "full_page", false)?;
    let selector = optional_owned_string(input, "selector", MAX_PATH_CHARS)?;
    let element_ref = optional_owned_string(input, "ref", 64)?;
    let browser = state
        .browser
        .session_for_tool(session_id, crate::browser::PlaywrightAction::Screenshot)?;

    // Capturing through CDP can take tens of seconds. Establish filesystem
    // authority only after capture, then retain its complete directory-handle
    // chain across sidecar import and final installation.
    let capture = browser.capture_screenshot_for_tool(
        full_page,
        selector.as_deref(),
        element_ref.as_deref(),
    )?;
    let authority = prepare_secure_write_with_scope(workspace, &requested, scope)?;
    let target = authority.target().to_path_buf();
    if target
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or(true, |extension| !extension.eq_ignore_ascii_case("png"))
    {
        return Err("playwright screenshot resolved target is no longer a PNG path".into());
    }
    let attachment_name = screenshot_attachment_name(&target)?.to_owned();
    let receipt_path = screenshot_receipt_path(workspace, &target)?;
    persist_browser_screenshot(
        &browser,
        authority,
        &receipt_path,
        &attachment_name,
        &capture,
        attachment_store,
    )
}

fn persist_browser_screenshot(
    browser: &BrowserSession,
    authority: SecureWriteAuthority,
    receipt_path: &str,
    attachment_name: &str,
    capture: &BrowserPngCapture,
    attachment_store: Option<&ImageAttachmentStore>,
) -> Result<Outcome, String> {
    // Validate/import before the workspace side effect and browser state update.
    // `authority` stays live throughout the import, so Windows directory and
    // target replacement remains locked during this comparatively expensive
    // sidecar operation.
    let images = attachment_store
        .map(|store| {
            store
                .import(attachment_name, &capture.bytes)
                .map(|image| vec![image])
        })
        .transpose()?
        .unwrap_or_default();
    authority.install(&capture.bytes)?;
    let screenshot = browser.note_screenshot_saved_as(receipt_path, capture);
    let output = serde_json::to_string_pretty(&screenshot)
        .map_err(|error| format!("Failed to encode browser screenshot result: {error}"))?;
    Ok(Outcome::success_with_images(output, images))
}

fn screenshot_attachment_name(target: &Path) -> Result<&str, String> {
    target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "playwright screenshot could not obtain a UTF-8 file name from the target path".to_owned())
}

fn screenshot_receipt_path(workspace: &Path, target: &Path) -> Result<String, String> {
    let workspace = canonical_workspace(workspace)?;
    if let Ok(relative) = target.strip_prefix(&workspace) {
        let mut components = Vec::new();
        for component in relative.components() {
            let std::path::Component::Normal(component) = component else {
                return Err("playwright screenshot could not generate a canonical workspace-relative receipt path".into());
            };
            let component = component
                .to_str()
                .ok_or_else(|| "playwright screenshot receipt path contains a non-UTF-8 name".to_owned())?;
            components.push(component);
        }
        if components.is_empty() {
            return Err("playwright screenshot receipt path cannot point to the workspace root".into());
        }
        return Ok(components.join("/"));
    }

    let mut identity = b"mework-external-screenshot-v1\0".to_vec();
    identity.extend_from_slice(&path_identity_bytes(target));
    let digest = Sha256::digest(&identity);
    let alias = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // The basename is still part of the host path and can itself contain an
    // account, project, or customer identifier. External receipts therefore
    // expose only a stable content-independent path alias plus the format.
    Ok(format!("external-screenshot/sha256-{alias}.png"))
}

#[cfg(windows)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(unix)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(any(windows, unix)))]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

/// Resolves `playwright file_upload` paths against the caller's filesystem scope before the browser
/// runtime sends them to the page. The renderer/model never hands the WebView a raw path; only
/// guard-approved, non-symlink regular files reach `DOM.setFileInputFiles`.
fn run_browser_file_upload(
    workspace: &Path,
    session_id: &str,
    input: &JsonObject,
    scope: &ExecutionScope,
    state: &AppState,
) -> Result<Outcome, String> {
    let requested = parse_upload_paths(input)?;
    let workspace = canonical_workspace(workspace)?;
    let mut resolved = Vec::new();
    for raw in &requested {
        let target = resolve_existing_with_scope(&workspace, raw, scope)?;
        let metadata =
            fs::symlink_metadata(&target).map_err(|error| format!("Failed to read upload file: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("playwright file_upload refuses to upload a symbolic link".into());
        }
        if !metadata.is_file() {
            return Err("playwright file_upload can only upload regular files".into());
        }
        resolved.push(target);
    }
    let value = state
        .browser
        .session_for_tool(session_id, crate::browser::PlaywrightAction::FileUpload)?
        .execute_tool_blocking(
            crate::browser::PlaywrightAction::FileUpload,
            input,
            &crate::browser::BrowserToolGrants {
                screenshot_path: None,
                upload_paths: Some(resolved),
            },
        )?;
    serde_json::to_string_pretty(&value)
        .map(Outcome::success)
        .map_err(|error| format!("Failed to encode browser upload result: {error}"))
}

fn parse_upload_paths(input: &JsonObject) -> Result<Vec<String>, String> {
    let value = input
        .get("paths")
        .ok_or_else(|| "Missing parameter paths".to_owned())?;
    let raw = match value {
        Value::String(single) => vec![single.clone()],
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "paths array may contain only strings".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("paths must be a string or an array of strings".into()),
    };
    if raw.is_empty() || raw.len() > 10 {
        return Err("playwright file_upload requires 1 to 10 files".into());
    }
    for path in &raw {
        if path.trim().is_empty() {
            return Err("paths cannot contain an empty path".into());
        }
        if path.chars().count() > MAX_PATH_CHARS {
            return Err(format!("Upload path exceeds the {MAX_PATH_CHARS}-character limit"));
        }
    }
    Ok(raw)
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
        });
    finish_execution(request, state, started, outcome, profile)
}

fn run_browser_upload_image(
    request: &ToolExecutionRequest,
    state: &AppState,
    app_data: &Path,
    image: &ImageAttachment,
) -> Result<Outcome, String> {
    let session_id = agent_browser_session(&request.conversation_id, state)?;
    let bytes = ImageAttachmentStore::new(app_data).read_bytes(image)?;
    let file_name = upload_image_file_name(&request.input, image)?;
    let staging = create_upload_image_staging(image)?;
    let file_path = staging.join(&file_name);
    let upload = fs::write(&file_path, &bytes)
        .map_err(|error| format!("Failed to materialize image attachment for upload: {error}"))
        .and_then(|()| {
            state
                .browser
                .session_for_tool(&session_id, crate::browser::PlaywrightAction::UploadImage)?
                .execute_tool_blocking(
                    crate::browser::PlaywrightAction::UploadImage,
                    &request.input,
                    &crate::browser::BrowserToolGrants {
                        screenshot_path: None,
                        upload_paths: Some(vec![file_path.clone()]),
                    },
                )
        });
    // Single-use materialization: whether the page consumed the bytes or the
    // upload failed, nothing may linger in the temp directory.
    let _ = fs::remove_dir_all(&staging);
    let value = upload?;
    let mut receipt = match value {
        Value::Object(object) => object,
        other => Map::from_iter([("result".to_owned(), other)]),
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
    fs::create_dir_all(&staging).map_err(|error| format!("Failed to create image upload staging directory: {error}"))?;
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

    let mut entries = Vec::new();
    let mut overflowed = false;
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(depth as usize + 1)
        .into_iter()
        .filter_entry(|entry| existing_path_is_allowed(scope, entry.path()))
    {
        let entry = entry.map_err(|error| {
            let reason = error.io_error().map(ToString::to_string)
                .unwrap_or_else(|| "directory traversal failed".into());
            format!("Failed to list directory {path}: {reason}")
        })?;
        if entries.len() >= MAX_LIST_ENTRIES {
            overflowed = true;
            break;
        }
        let mut display = relative_display(&workspace, entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type().is_dir() {
            display.push('/');
        }
        entries.push(display);
    }
    entries.sort_unstable();
    if overflowed {
        entries.push(profile.render(
            PromptKey::ToolLsLimit,
            &[("limit", &MAX_LIST_ENTRIES.to_string())],
        ));
    }
    Ok(if entries.is_empty() {
        profile.text(PromptKey::ToolLsEmpty).to_owned()
    } else {
        entries.join("\n")
    })
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
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| format!("Invalid regular expression: {error}"))?;
    let workspace = canonical_workspace(workspace)?;
    let root = resolve_existing_with_scope(&workspace, &path, scope)?;

    let mut matches = Vec::new();
    let mut overflowed = false;
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| existing_path_is_allowed(scope, entry.path()))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                matches.push(profile.render(PromptKey::ToolGrepSkipped, &[("error", &error.to_string())]));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.len() <= MAX_TEXT_FILE => metadata,
            _ => continue,
        };
        let _ = metadata;
        let bytes = match fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        for (line_index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                let relative = display_path(&workspace, entry.path());
                matches.push(format!(
                    "{}:{}:{}",
                    relative,
                    line_index + 1,
                    truncate_chars(line, 500)
                ));
                if matches.len() >= MAX_SEARCH_MATCHES {
                    overflowed = true;
                    break;
                }
            }
        }
        if overflowed {
            break;
        }
    }
    if overflowed {
        matches.push(profile.render(
            PromptKey::ToolGrepLimit,
            &[("limit", &MAX_SEARCH_MATCHES.to_string())],
        ));
    }
    Ok(if matches.is_empty() {
        profile.text(PromptKey::ToolGrepNoMatch).to_owned()
    } else {
        matches.join("\n")
    })
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

    let mut found = Vec::new();
    let mut overflowed = false;
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| existing_path_is_allowed(scope, entry.path()))
    {
        let entry = entry.map_err(|error| format!("Failed to find files: {error}"))?;
        if entry.depth() == 0 {
            continue;
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
        if found.len() >= MAX_LIST_ENTRIES {
            overflowed = true;
            break;
        }
        let mut display = display_path(&workspace, entry.path());
        if entry.file_type().is_dir() {
            display.push('/');
        }
        found.push(display);
    }
    found.sort_unstable();
    if overflowed {
        found.push(profile.render(
            PromptKey::ToolFindLimit,
            &[("limit", &MAX_LIST_ENTRIES.to_string())],
        ));
    }
    Ok(if found.is_empty() {
        profile.text(PromptKey::ToolFindNoMatch).to_owned()
    } else {
        found.join("\n")
    })
}

fn display_path(workspace: &Path, path: &Path) -> String {
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

fn run_read(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    attachment_store: Option<&ImageAttachmentStore>,
    profile: &PromptProfile,
) -> Result<Outcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
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
        let store =
            attachment_store.ok_or_else(|| "read requires a trusted image attachment directory to read an image".to_owned())?;
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
    let lines = content.lines().collect::<Vec<_>>();
    let start_index = (start_line - 1).min(usize::MAX as u64) as usize;
    if start_index >= lines.len() {
        return Ok(
            Outcome::success(profile.text(PromptKey::ToolReadRangeOutOfBounds).to_owned())
                .with_opened_file(file_path),
        );
    }
    let end_index = if end_line == u64::MAX {
        lines.len().min(start_index.saturating_add(5_001))
    } else {
        (end_line.min(lines.len() as u64)) as usize
    };
    let mut output = lines[start_index..end_index].join("\n");
    if end_line == u64::MAX && end_index < lines.len() {
        output.push_str(&profile.render(
            PromptKey::ToolReadLimit,
            &[("limit", "5001")],
        ));
    }
    Ok(Outcome::success(output).with_opened_file(file_path))
}

fn run_write(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
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
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("Failed to create parent directory: {error}"))?;
    }
    atomic_write(&file_path, content.as_bytes())?;
    let diff = before.and_then(|(before, created)| unified_diff(&path, &before, &content, created));
    Ok(Outcome::success_with_diff(
        profile.render(
            PromptKey::ToolWriteDone,
            &[("bytes", &content.len().to_string()), ("path", &path)],
        ),
        diff,
    ))
}

fn run_edit(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    profile: &PromptProfile,
) -> Result<Outcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let find = required_string(input, "find", MAX_WRITE_BYTES, true)?;
    if find.is_empty() {
        return Err("Parameter find cannot be empty".into());
    }
    let replace = required_string(input, "replace", MAX_WRITE_BYTES, true)?;
    let file_path = resolve_existing_with_scope(workspace, &path, scope)?;
    if !file_path.is_file() {
        return Err(format!("edit target is not a file: {}", file_path.display()));
    }
    let content = read_text_file(&file_path)?;
    let occurrences = content.match_indices(&find).count();
    if occurrences == 0 {
        return Err("The exact text to replace was not found".into());
    }
    if occurrences != 1 {
        return Err(format!(
            "The search text occurs {occurrences} times; edit requires exactly one match"
        ));
    }
    let next = content.replacen(&find, &replace, 1);
    if next.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Edited file exceeds the {} MiB limit",
            MAX_WRITE_BYTES / 1024 / 1024
        ));
    }
    let diff = unified_diff(&path, &content, &next, false);
    atomic_write(&file_path, next.as_bytes())?;
    Ok(Outcome::success_with_diff(
        profile.render(PromptKey::ToolEditDone, &[("path", &path)]),
        diff,
    ))
}

fn unified_diff(path: &str, before: &str, after: &str, created: bool) -> Option<String> {
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

#[derive(Clone, Copy)]
pub(crate) enum ShellKind {
    PowerShell,
    Bash,
}

impl ShellKind {
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            ShellKind::PowerShell => "powershell",
            ShellKind::Bash => "bash",
        }
    }

    pub(crate) fn from_tool_name(name: &str) -> Option<Self> {
        match name {
            "powershell" => Some(ShellKind::PowerShell),
            "bash" => Some(ShellKind::Bash),
            _ => None,
        }
    }
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
    pub child: std::process::Child,
    pub job: ShellJob,
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
) -> ShellCallContext {
    let cwd_file = std::env::temp_dir().join(format!(
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
    let temp_dir = (local && cfg!(windows) && matches!(kind, ShellKind::Bash))
        .then(|| std::env::temp_dir());
    ShellCallContext {
        snapshot,
        cwd_file,
        temp_dir,
    }
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
///   PowerShell gets `-NoProfile -NonInteractive -ExecutionPolicy Bypass
///   -Command <script>`. Note the absent `-NoLogo`: Claude Code does not pass
///   it to the tool, only to its static parser, and parity is the point here.
/// - WSL: argv must be exactly `wsl.exe -d <distro> --cd <workspace> --exec
///   /usr/bin/env K=V… bash --noprofile --norc -c <command>` so the command
///   reaches bash intact without an intermediate shell.
/// - SSH: invoke OpenSSH with `BatchMode=yes`; every host-composed fragment is
///   POSIX-single-quoted.
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
    workspace: &Path,
    kind: ShellKind,
    command: &str,
    runner: &crate::run_environment::ShellRunner,
    session: ShellSession<'_>,
) -> Result<ShellLaunchPlan, String> {
    use crate::run_environment::{self, ShellRunner};
    let env = &runner.normalized_env()?;
    match runner {
        ShellRunner::Local { .. } => {
            let candidates: Vec<String> = match kind {
                ShellKind::PowerShell => run_environment::local_powershell_candidates(),
                // Resolved to an absolute path instead of named: a bare `bash`
                // on Windows is either the WSL launcher or nothing at all. See
                // `run_environment::local_bash_candidates`.
                ShellKind::Bash => run_environment::local_bash_candidates(),
            };
            if candidates.is_empty() {
                return Err(match kind {
                    ShellKind::Bash => "No native Bash was found locally. Install Git for Windows or MSYS2, or change this conversation's run environment to WSL. The System32 WSL launcher is not used as local Bash because it executes commands in another machine's filesystem and network.".into(),
                    ShellKind::PowerShell => "No PowerShell was found locally. Install PowerShell 7 (https://aka.ms/powershell), or use the bash tool instead.".to_owned(),
                });
            }
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
                    let script =
                        bash_tool_script(command, session.snapshot, session.temp_dir, session.cwd_file);
                    if session.snapshot.is_some() {
                        vec!["-c".into(), script]
                    } else {
                        vec!["-c".into(), "-l".into(), script]
                    }
                }
            };
            Ok(ShellLaunchPlan {
                candidates,
                args,
                env: env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                local_hardening: true,
            })
        }
        ShellRunner::Wsl { distro, .. } => {
            if matches!(kind, ShellKind::PowerShell) {
                return Err(
                    "This conversation runs in WSL, where the powershell tool is unavailable; use the bash tool instead".into(),
                );
            }
            run_environment::validate_wsl_distro_name(distro)?;
            Ok(ShellLaunchPlan {
                candidates: vec!["wsl.exe".into()],
                args: run_environment::wsl_shell_args(distro, workspace, env, command),
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
            remote_cwd,
            ..
        } => {
            if matches!(kind, ShellKind::PowerShell) {
                return Err(
                    "This conversation runs on an SSH remote machine, where the powershell tool is unavailable; use the bash tool instead"
                        .into(),
                );
            }
            Ok(ShellLaunchPlan {
                candidates: run_environment::ssh_client_candidates(),
                args: run_environment::ssh_shell_args(
                    host,
                    *port,
                    identity_file,
                    remote_cwd,
                    env,
                    command,
                ),
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
/// Killing the local wrapper process tree does not guarantee SSH descendants exit.
///
/// `start_dir` is where this call begins. It is the workspace on the first call
/// of a conversation and the directory the previous call reported afterwards,
/// which is the whole of how `cd` persists — the shell itself does not.
pub(crate) fn spawn_shell_process(
    workspace: &Path,
    start_dir: &Path,
    kind: ShellKind,
    command: &str,
    runner: &crate::run_environment::ShellRunner,
    session: ShellSession<'_>,
    profile: &PromptProfile,
) -> Result<SpawnedShell, String> {
    let workspace = canonical_workspace(workspace)?;
    // A start directory that has gone away must not fail the call: fall back to
    // the workspace, exactly as a fresh conversation would begin.
    let start_dir = if start_dir.is_dir() {
        start_dir.to_path_buf()
    } else {
        workspace.clone()
    };
    let plan = shell_launch_plan(&workspace, kind, command, runner, session)?;

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
                    let name = name.into_string().map_err(|_| "Invalid NO_PROXY variable name")?;
                    let value = value.into_string().map_err(|_| "NO_PROXY must contain valid Unicode")?;
                    Ok((name, value))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>, &str>>()?;
            let configured = plan.env.iter().cloned().collect();
            let proxy = crate::child_environment::normalized_proxy_bypass(
                &inherited, &configured, cfg!(windows),
            )?;
            for name in inherited.keys() { process.env_remove(name); }
            process.envs(proxy);
            match kind {
                // Claude Code's PowerShell defaults, each applied only when the
                // parent does not already carry the name. `FORCE_COLOR` present
                // anywhere suppresses the `NO_COLOR` default rather than fighting
                // it. `SHELL` is explicitly cleared: a POSIX shell path confuses
                // tools that find it in a PowerShell session.
                ShellKind::PowerShell => {
                    for (key, value) in child_text_defaults(&plan.env, |name| {
                        std::env::var_os(name).is_some()
                    }) {
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
                ShellKind::Bash => {
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
                return Ok(SpawnedShell { child, job });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_not_found = Some(error);
            }
            Err(error) => return Err(format!("Failed to start {executable}: {error}")),
        }
    }
    Err(format!(
        "No usable {} executable found (tried: {}){}",
        match kind {
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Bash => "Bash",
        },
        profile.join_list(&plan.candidates),
        last_not_found
            .map(|error| format!(": {error}"))
            .unwrap_or_default()
    ))
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
    if !cfg!(windows) {
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
    if reported
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::CurDir))
    {
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
    workspace: &Path,
    input: &JsonObject,
    kind: ShellKind,
    conversation_id: &str,
    state: &AppState,
    cancel: &CancelSignal,
    runner: &crate::run_environment::ShellRunner,
    app_data: Option<&Path>,
    handoff: Option<&ShellHandoff<'_>>,
    profile: &PromptProfile,
) -> Result<Outcome, String> {
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
    let context = shell_call_context(kind, runner, conversation_id, app_data, state);
    let start_dir = state
        .shell_cwd(conversation_id)
        .unwrap_or_else(|| workspace.to_path_buf());
    let mut spawned = spawn_shell_process(
        workspace,
        &start_dir,
        kind,
        &command,
        runner,
        context.session(),
        profile,
    )?;
    // Registration starts here and not before: a command that never spawned is not a
    // task. The guard's `Drop` retires the row however this call ends; the wait below
    // is what tells it how the command actually ended.
    let mut guard = match state
        .shell_tasks
        .try_register(conversation_id, kind.tool_name(), &command, false) {
        Ok(guard) => guard,
        Err(error) => {
            kill_process_tree_or_child(&mut spawned.child, &spawned.job);
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
            if let Some(cwd) = adopt_reported_cwd(context.cwd_file(), workspace) {
                state.set_shell_cwd(conversation_id, cwd);
            }
            Ok(Outcome {
                success: result.success,
                output: result.output,
                images: Vec::new(),
                diff: None,
                opened_file: None,
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
                output: result.output,
                images: Vec::new(),
                diff: None,
                opened_file: None,
            })
        }
    }
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
}

/// What happened when a timed-out run was offered to the task surface.
pub(crate) enum ShellHandoffOutcome {
    /// A task slot took it. The string is the receipt the model gets in place of
    /// the output the command has not finished producing.
    Adopted(String),
    /// Nothing could take it — no free task slot, or a caller with no task
    /// surface at all. The run comes back so the caller can stop it.
    Refused(ShellRun, crate::shell_tasks::ShellTaskGuard, ShellCallContext),
}

/// Offered a running command whose deadline expired, together with the row that
/// owns its stop button.
///
/// Taking both by value is the point: adopting the command means adopting
/// responsibility for finishing it and for retiring its row, and a borrow could
/// not express that. The executor keeps no way to reach the run afterwards.
pub(crate) type ShellHandoff<'a> = dyn Fn(
        ShellRun,
        crate::shell_tasks::ShellTaskGuard,
        ShellCallContext,
    ) -> ShellHandoffOutcome
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
    child: std::process::Child,
    job: ShellJob,
    /// Carried only so a timed-out result can name the deadline it missed.
    timeout: Option<Duration>,
    stdout_thread: thread::JoinHandle<(Vec<u8>, bool)>,
    stderr_thread: thread::JoinHandle<(Vec<u8>, bool)>,
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
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture command standard output".to_owned())?;
        let stderr = child
            .stderr
            .take()
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
                    kill_process_tree_or_child(&mut self.child, &self.job);
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
        kill_process_tree_or_child(&mut self.child, &self.job);
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
        // Joined after the tree is down, never before: these threads end when the last copy of the
        // write handle closes, and a surviving grandchild holds one. That is why the kill has to
        // cover the whole tree — otherwise the command is "stopped" and this still blocks.
        let (stdout, stdout_truncated) = self.stdout_thread.join().map_err(|_| {
            "The command standard-output reader thread terminated unexpectedly".to_owned()
        })?;
        let (stderr, stderr_truncated) = self.stderr_thread.join().map_err(|_| {
            "The command standard-error reader thread terminated unexpectedly".to_owned()
        })?;
        let output = format_process_output(
            status,
            completion,
            &stdout,
            &stderr,
            stdout_truncated,
            stderr_truncated,
            self.timeout,
            profile,
        );
        Ok(ShellWaitOutcome::Settled(ShellProcessResult {
            end,
            success: status.success() && completion == ShellCompletion::Exited,
            exit_code: status.code(),
            output,
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
fn kill_process_tree_or_child(child: &mut std::process::Child, job: &ShellJob) {
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
}

// The handle is owned solely by this struct and only ever used from the thread running the
// command; Windows handles are process-wide values, not thread-affine.
#[cfg(windows)]
unsafe impl Send for ShellJob {}

impl ShellJob {
    /// Creates an empty job. A failure here is not fatal: the kill path falls back to `taskkill`.
    fn create() -> Self {
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
        Self {}
    }

    /// Puts a freshly spawned command in the job. Everything it starts afterwards is added by
    /// Windows automatically, which is what makes the later terminate cover the whole tree.
    fn assign(&self, _child: &std::process::Child) {
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
    fn terminate(&self) -> bool {
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

/// Drains one pipe to end-of-stream.
///
/// Two readers, one per pipe, because a command that fills its stderr buffer while nobody reads it
/// deadlocks. The captured vector is what the *model* gets and stops growing at `MAX_TOOL_OUTPUT`;
/// the sink is what a *person* watching the task page gets and keeps receiving past that cap, so a
/// long build stays watchable after its result has been truncated.
///
/// Captured bytes pass through untouched; the watcher sink decodes its copy incrementally. Trailing
/// whitespace used to be stripped here, which only earned its keep while the PowerShell console was
/// widened to thousands of columns and padded every formatted row out to that width; Claude Code
/// widens nothing and trims nothing, so neither does this.
fn collect_pipe<R: Read + Send + 'static>(
    mut pipe: R,
    mut sink: ShellOutputSink,
) -> thread::JoinHandle<(Vec<u8>, bool)> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut truncated = false;
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let text = &chunk[..read];
                    sink.append(text);
                    let remaining = MAX_TOOL_OUTPUT.saturating_sub(captured.len());
                    captured.extend_from_slice(&text[..text.len().min(remaining)]);
                    if text.len() > remaining {
                        truncated = true;
                    }
                }
            }
        }
        sink.finish();
        if truncated {
            trim_incomplete_utf8_tail(&mut captured);
        }
        (captured, truncated)
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
        let Some(end) = rest.find('\n') else { return rest };
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
fn format_process_output(
    status: ExitStatus,
    completion: ShellCompletion,
    stdout: &[u8],
    stderr: &[u8],
    stdout_truncated: bool,
    stderr_truncated: bool,
    timeout: Option<Duration>,
    profile: &PromptProfile,
) -> String {
    // Claude Code trims the two streams differently, and the asymmetry is real:
    // stdout loses only its leading blank lines and its trailing whitespace, so
    // a command's own indentation and interior padding survive, while stderr is
    // trimmed at both ends because it is a diagnostic, not data.
    let stdout =
        normalize_line_endings(&crate::console_text::decode_console_text(stdout)).into_owned();
    let stdout = strip_leading_blank_lines(&stdout).trim_end().to_owned();
    let stderr = normalize_line_endings(&crate::console_text::decode_console_text(stderr))
        .trim()
        .to_owned();
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
    if stdout_truncated || stderr_truncated {
        parts.push(profile.text(PromptKey::ToolShellOutputTruncated).to_owned());
    }
    if parts.is_empty() {
        // A silent success still has to say something, or the round reads as if
        // the tool produced nothing at all.
        return profile.render(PromptKey::ToolShellCompleted, &[("code", "0")]);
    }
    parts.join("\n")
}

fn read_text_file(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Failed to read file metadata: {error}"))?;
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

fn required_string(
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
        return Err(format!("Parameter {key} exceeds the {max_chars}-character limit"));
    }
    Ok(value.to_owned())
}

fn optional_string(
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

fn optional_u64(input: &Map<String, Value>, key: &str, default: u64) -> Result<u64, String> {
    optional_u64_value(input, key).map(|value| value.unwrap_or(default))
}

fn optional_u64_value(input: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
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

fn optional_bool(input: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
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
    format!("{}\n{}", &value[..end], profile.text(PromptKey::ToolOutputTruncated))
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

fn truncate_chars(value: &str, limit: usize) -> String {
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
                &crate::run_environment::ShellRunner::default(),
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
            &crate::run_environment::ShellRunner::default(),
            &PromptProfile::builtin_english(),
        );
        assert!(result.success, "{}", result.output);

        let snapshots: Vec<_> = fs::read_dir(crate::shell_snapshot::snapshot_directory(&app_data))
            .expect("the snapshot directory exists after the first call")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(snapshots.len(), 1, "one snapshot per conversation, not one per call");
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
            &crate::run_environment::ShellRunner::default(),
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
        assert!(result.output.contains("timed out after 1s"), "{}", result.output);
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
        let clamp = |value: Value| parse_shell_timeout(&object(json!({"command": "x", "timeout": value})));
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
        if cfg!(windows) {
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
        assert!(run_edit(directory.path(), &ambiguous, &scope, &profile).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha beta beta");

        let exact = object(json!({
            "path": "sample.txt",
            "find": "alpha ",
            "replace": "first "
        }));
        let outcome = run_edit(directory.path(), &exact, &scope, &profile).unwrap();
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
        assert_eq!(overwritten.output, "Wrote 25 bytes to existing.txt");
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
            ("workflow", json!({"script": "export const meta = { name: \"x\", description: \"d\" }\nreturn 1"})),
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
            assert!(result.output.contains("Child-agent-only"), "{}", result.output);
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
        assert!(run(
            "powershell",
            json!({"command":"Write-Output MEWORK_POWERSHELL_E2E"}),
        )
        .contains("MEWORK_POWERSHELL_E2E"));
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

        assert_eq!(seen.len(), 1, "the running command must be exactly one task");
        assert_eq!(seen[0].tool_name, "bash");
        assert!(seen[0].command.contains("MEWORK_SHELL_TASK_DONE"));
        assert!(!seen[0].stopping);
        assert!(seen[0].conversation_id == "conversation-test");
        assert!(seen[0].outcome.is_none(), "a running row has no outcome yet");
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
        assert!(result.output.contains("run_in_background"), "{}", result.output);
        // A rejected call never spawns and leaves no task row.
        assert!(state.shell_tasks.task_snapshots("conversation-test").is_empty());
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
            result.output.contains("<error>Command was aborted before completion</error>"),
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
        match shell_launch_plan(workspace.path(), ShellKind::Bash, "echo hi", &runner, with_snapshot) {
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
                assert_eq!(bash.candidates, vec!["bash"]);
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
                let bare = ShellSession { snapshot: None, cwd_file: &cwd_file, temp_dir: None };
                let fallback =
                    shell_launch_plan(workspace.path(), ShellKind::Bash, "echo hi", &runner, bare)
                        .unwrap();
                assert_eq!(fallback.args[..2], ["-c", "-l"]);
                assert_eq!(fallback.args.len(), 3);
            }
            // A Windows host with no native Bash installed: the plan refuses with an
            // actionable message instead of handing the command to the launcher.
            Err(error) => {
                assert!(cfg!(windows), "{error}");
                assert!(error.contains("No native Bash was found"), "{error}");
            }
        }

        match shell_launch_plan(
            workspace.path(),
            ShellKind::PowerShell,
            "echo hi",
            &runner,
            with_snapshot,
        ) {
            Ok(ps) => {
                // `-NoLogo` is deliberately absent: Claude Code passes it only to
                // its static parser, never to the tool.
                assert_eq!(
                    ps.args[..5],
                    ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]
                );
                assert_eq!(ps.args[5], powershell_tool_script("echo hi", &cwd_file));
                assert_eq!(ps.args.len(), 6);
                // PowerShell 7 is preferred and Windows PowerShell 5.1 is last:
                // 5.1 is the one that reads BOM-less UTF-8 files as ANSI.
                let first = ps.candidates.first().expect("a candidate").to_ascii_lowercase();
                assert!(first.contains("pwsh") || ps.candidates.len() == 1, "{first}");
            }
            Err(error) => {
                assert!(cfg!(windows), "{error}");
                assert!(error.contains("No PowerShell was found"), "{error}");
            }
        }
    }

    #[test]
    fn child_text_defaults_yield_to_configured_and_inherited_values() {
        let none: Vec<(String, String)> = Vec::new();
        assert_eq!(
            child_text_defaults(&none, |_| false),
            vec![("PYTHONIOENCODING", "utf-8:surrogateescape"), ("NO_COLOR", "1")]
        );
        // A run-environment value is the user's choice, whatever it is.
        let configured = vec![("PYTHONIOENCODING".to_owned(), "gbk".to_owned())];
        assert_eq!(child_text_defaults(&configured, |_| false), vec![("NO_COLOR", "1")]);
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
    fn command_output_is_not_reshaped_on_its_way_to_the_model() {
        let profile = PromptProfile::default();
        let padded = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            b"\n  \r\nName       \r\nvalue  \n",
            b"",
            false,
            false,
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
            &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'\n'],
            b"",
            false,
            false,
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
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        let profile = PromptProfile::builtin_english();
        let output = format_process_output(
            ExitStatus::from_raw(0),
            ShellCompletion::Exited,
            b"PS\r\nNODE\nprogress 10%\rprogress 20%\r\n",
            b"warn\r\n",
            false,
            false,
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
            b"partial progress\n",
            b"fatal: not a git repository\n",
            false,
            false,
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
            b"on main\n",
            b"note\n",
            false,
            false,
            None,
            &profile,
        );
        assert_eq!(ok, "on main\nnote");

        // A silent success still has to say something, or the round reads as if
        // the tool produced nothing at all.
        let silent = format_process_output(
            exit_status(0),
            ShellCompletion::Exited,
            b"",
            b"",
            false,
            false,
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
            b"building...\n",
            b"",
            false,
            false,
            Some(Duration::from_millis(120_000)),
            &profile,
        );
        assert!(output.starts_with("Command timed out after 120s"), "{output}");
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
                &AppState::default(), ExecutionScope::workspace_only(&workspace), None,
                &crate::run_environment::ShellRunner::default(), &PromptProfile::builtin_english(),
            );
            assert!(!result.success);
            assert!(result.output.contains(path), "{}", result.output);
            assert!(!result.output.contains("LocalCache"), "{}", result.output);
            assert!(!result.output.contains(&directory.path().to_string_lossy().to_string()), "{}", result.output);
        }
    }

    #[test]
    fn shell_launch_plans_normalize_proxy_bypass_for_each_target() {
        use crate::run_environment::ShellRunner;
        let directory = tempfile::tempdir().unwrap();
        let cwd_file = directory.path().join("cwd");
        let bare = ShellSession { snapshot: None, cwd_file: &cwd_file, temp_dir: None };
        let env = [("No_Proxy".into(), " localhost;127.0.0.1 localhost ".into())]
            .into_iter().collect::<std::collections::BTreeMap<String, String>>();
        let runners = [
            ShellRunner::Local { env: env.clone() },
            ShellRunner::Wsl { distro: "Ubuntu".into(), env: env.clone() },
            ShellRunner::Ssh { host: "user@host".into(), port: 22,
                identity_file: String::new(), remote_cwd: String::new(), env },
        ];
        for (index, runner) in runners.iter().enumerate() {
            let plan = shell_launch_plan(directory.path(), ShellKind::Bash, "true", runner, bare).unwrap();
            if index == 0 {
                assert!(plan.env.contains(&("NO_PROXY".into(), "localhost,127.0.0.1".into())));
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
            distro: "Ubuntu".into(),
            env: [("FOO".to_owned(), "a b".to_owned())].into_iter().collect(),
        };

        // Remote legs carry no session: Claude Code has no remote runner to copy,
        // so they source no snapshot and report no directory.
        let cwd_file = workspace.path().join("cwd");
        let bare = ShellSession { snapshot: None, cwd_file: &cwd_file, temp_dir: None };
        let plan =
            shell_launch_plan(workspace.path(), ShellKind::Bash, "echo hi", &runner, bare).unwrap();
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

        let error =
            shell_launch_plan(workspace.path(), ShellKind::PowerShell, "echo hi", &runner, bare)
                .unwrap_err();
        assert!(error.contains("bash"), "{error}");
    }

    #[test]
    fn ssh_launch_plan_wraps_the_command_and_rejects_powershell() {
        use crate::run_environment::ShellRunner;
        let workspace = tempfile::tempdir().unwrap();
        let runner = ShellRunner::Ssh {
            host: "user@devbox".into(),
            port: 0,
            identity_file: String::new(),
            remote_cwd: String::new(),
            env: Default::default(),
        };

        let cwd_file = workspace.path().join("cwd");
        let bare = ShellSession { snapshot: None, cwd_file: &cwd_file, temp_dir: None };
        let plan =
            shell_launch_plan(workspace.path(), ShellKind::Bash, "pwd", &runner, bare).unwrap();
        assert!(plan.candidates.contains(&"ssh".to_owned()));
        assert_eq!(plan.args[..4], ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        assert_eq!(&plan.args[4..6], &["--", "user@devbox"]);
        assert!(!plan.local_hardening);

        assert!(
            shell_launch_plan(workspace.path(), ShellKind::PowerShell, "pwd", &runner, bare).is_err()
        );
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
            &runner,
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
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(!result.success);
        assert!(result.output.contains("<error>Command was aborted before completion</error>"), "{}", result.output);
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
            &crate::run_environment::ShellRunner::default(),
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
        assert!(result.output.contains("command did not start"), "{}", result.output);
        assert!(
            state.shell_tasks.task_snapshots("conversation-test").is_empty(),
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
            assert!(orchestrator_state.cancel_model_run("run-unrelated").unwrap());
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
            &crate::run_environment::ShellRunner::default(),
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
        assert!(result.output.contains("<error>Command was aborted before completion</error>"), "{}", result.output);
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
            &crate::run_environment::ShellRunner::default(),
            None,
            &PromptProfile::builtin_english(),
        )
        .result;
        assert!(stopper.join().unwrap(), "The command was never registered as a task row");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "A task-level stop must kill the command within one polling interval, not wait for 20 seconds: {:?}",
            started.elapsed()
        );
        assert!(!result.success);
        assert!(result.output.contains("<error>Command was aborted before completion</error>"), "{}", result.output);
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
                &crate::run_environment::ShellRunner::default(),
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
        assert!(!read.output.contains('\r'), "CRLF is folded for the model:\n{}", read.output);
    }

    /// `powershell` and `bash` use separate dispatch arms, so each must independently
    /// prove that a task-level stop reaches its running command.
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
            &crate::run_environment::ShellRunner::default(),
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
        assert!(result.output.contains("<error>Command was aborted before completion</error>"), "{}", result.output);
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
            &crate::run_environment::ShellRunner::default(),
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
            &crate::run_environment::ShellRunner::default(),
            &PromptProfile::builtin_english(),
        );
        assert!(!result.success);
        assert!(
            result.output.contains("Image exceeds the 5 MiB limit"),
            "{}",
            result.output
        );
        assert!(
            !result.output.contains("Text file"),
            "{}",
            result.output
        );
        assert!(result.images.is_empty());
    }
    #[test]
    fn screenshot_persistence_holds_authority_through_sidecar_import_and_reports_relative_path() {
        let workspace = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let scope = ExecutionScope::workspace_only(workspace.path());
        let authority =
            prepare_secure_write_with_scope(workspace.path(), "./shots/page.png", &scope).unwrap();
        let target = authority.target().to_path_buf();
        let receipt = screenshot_receipt_path(workspace.path(), &target).unwrap();
        let browser = BrowserSession::new("screenshot-persistence");
        let capture = BrowserPngCapture {
            bytes: TEST_PNG.to_vec(),
            width: 1,
            height: 1,
            full_page: false,
        };
        let store = ImageAttachmentStore::new(app_data.path());

        let outcome = persist_browser_screenshot(
            &browser,
            authority,
            &receipt,
            "page.png",
            &capture,
            Some(&store),
        )
        .unwrap();

        assert_eq!(receipt, "shots/page.png");
        assert_eq!(fs::read(target).unwrap(), TEST_PNG);
        assert_eq!(outcome.images.len(), 1);
        let output: Value = serde_json::from_str(&outcome.output).unwrap();
        assert_eq!(
            output.get("path").and_then(Value::as_str),
            Some("shots/page.png")
        );
        assert_eq!(
            browser.status().screenshot_path.as_deref(),
            Some("shots/page.png")
        );
        let absolute_workspace = fs::canonicalize(workspace.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(!outcome.output.contains(&absolute_workspace));
    }

    #[test]
    fn screenshot_install_failure_does_not_update_browser_state() {
        let workspace = tempfile::tempdir().unwrap();
        let scope = ExecutionScope::workspace_only(workspace.path());
        let authority =
            prepare_secure_write_with_scope(workspace.path(), "shots/page.png", &scope).unwrap();
        let target = authority.target().to_path_buf();
        fs::write(&target, b"attacker").unwrap();
        let browser = BrowserSession::new("screenshot-failed-install");
        let capture = BrowserPngCapture {
            bytes: TEST_PNG.to_vec(),
            width: 1,
            height: 1,
            full_page: false,
        };

        let error = match persist_browser_screenshot(
            &browser,
            authority,
            "shots/page.png",
            "page.png",
            &capture,
            None,
        ) {
            Err(error) => error,
            Ok(_) => panic!("late target replacement must fail"),
        };

        assert!(
            error.contains("was created by another process")
                || error.contains("without replacement"),
            "{error}"
        );
        assert_eq!(fs::read(target).unwrap(), b"attacker");
        assert!(browser.status().screenshot_path.is_none());
    }

    #[test]
    fn screenshot_staging_failure_preserves_existing_file_and_browser_state() {
        let workspace = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let target = workspace.path().join("shots/page.png");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let old = b"complete-old-screenshot-bytes";
        fs::write(&target, old).unwrap();
        let scope = ExecutionScope::workspace_only(workspace.path());
        let mut authority =
            prepare_secure_write_with_scope(workspace.path(), "shots/page.png", &scope).unwrap();
        authority.inject_temporary_write_failure_after(7);
        let browser = BrowserSession::new("screenshot-staging-failure");
        let capture = BrowserPngCapture {
            bytes: TEST_PNG.to_vec(),
            width: 1,
            height: 1,
            full_page: false,
        };
        let store = ImageAttachmentStore::new(app_data.path());

        let error = match persist_browser_screenshot(
            &browser,
            authority,
            "shots/page.png",
            "page.png",
            &capture,
            Some(&store),
        ) {
            Err(error) => error,
            Ok(_) => panic!("injected staging failure must abort screenshot persistence"),
        };

        assert!(error.contains("Test injection"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), old);
        assert!(browser.status().screenshot_path.is_none());
    }

    #[test]
    fn screenshot_receipt_canonicalizes_workspace_relative_paths() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("shots")).unwrap();
        let scope = ExecutionScope::workspace_only(workspace.path());
        let target =
            resolve_for_write_with_scope(workspace.path(), "./shots/../shots/page.png", &scope)
                .unwrap();

        let receipt = screenshot_receipt_path(workspace.path(), &target).unwrap();

        assert_eq!(receipt, "shots/page.png");
        assert!(!receipt.contains(".."));
        assert!(!receipt.contains('\\'));
        assert!(!receipt.contains(&workspace.path().to_string_lossy().into_owned()));
    }

    #[test]
    fn screenshot_receipt_uses_a_stable_non_leaking_alias_for_external_paths() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("private-host-location");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let target = fs::canonicalize(&outside)
            .unwrap()
            .join("acme-private-customer.png");

        let first = screenshot_receipt_path(&workspace, &target).unwrap();
        let second = screenshot_receipt_path(&workspace, &target).unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("external-screenshot/sha256-"));
        assert!(first.ends_with(".png"));
        assert!(!first.contains(&root.path().to_string_lossy().into_owned()));
        assert!(!first.contains("private-host-location"));
        assert!(!first.contains("example-user"));
        assert!(!first.contains("customer"));
    }

    #[test]
    fn screenshot_attachment_name_uses_only_the_basename_of_a_deep_path() {
        let deep = "segment".repeat(48);
        let target = Path::new(&deep).join("nested").join("screen.png");
        assert!(target.to_string_lossy().len() > 256);
        assert_eq!(screenshot_attachment_name(&target).unwrap(), "screen.png");
    }
}
