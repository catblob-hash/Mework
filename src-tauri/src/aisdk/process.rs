//! Sidecar process and request multiplexing.
//!
//! **One process, multiplexed.** The parent turn and all subagents share one sidecar and demultiplex
//! by request ID. Per-request sidecars would multiply its substantial idle memory footprint.
//!
//! Threading model:
//!
//! - Each turn worker blocks on its own channel, preserving the synchronous `api.rs` turn loop.
//! - One reader thread exclusively reads, frames, and dispatches stdout by ID.
//! - A `Mutex` serializes stdin writes because an NDJSON record is one line and interleaved writes
//!   produce invalid JSON.
//!
//! When a sidecar exits, all in-flight requests receive a transient error and the next [`run_step`]
//! starts a replacement.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use uuid::Uuid;

use crate::api::ModelEventSink;
use crate::model::{ModelStreamEvent, ModelUsage};
use super::{ModelRequestError, RequestFailure, StreamPartial};

use super::protocol::{
    ErrorKind, SidecarFrame, SidecarUsage, StepEvent, StepRequest, StepResult,
    MAX_LINE_BYTES, PROTOCOL_VERSION,
};

/// Overrides the sidecar executable path for development and `cargo test`; production uses the
/// adjacent `externalBin` executable.
const BINARY_ENV: &str = "MEWORK_AISDK_BIN";

/// Overrides the Node executable for a JavaScript sidecar entry in development. Release builds do
/// not invoke Node.
const NODE_ENV: &str = "MEWORK_AISDK_NODE";

/// Hard limit for one exchange. It allows long reasoning while preventing a trickling upstream from
/// holding a turn indefinitely.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Cancellation probe interval while no frame is readable.
const PROBE_INTERVAL: Duration = Duration::from_millis(500);

struct Registry {
    pending: Mutex<HashMap<String, Sender<SidecarFrame>>>,
    alive: AtomicBool,
    /// Protocol version reported by this sidecar; zero means no handshake yet.
    ///
    /// This is per sidecar instance, not process-global. A restarted sidecar must not inherit a
    /// previous instance's ready version, which would bypass protocol-version validation.
    ready_protocol: AtomicU32,
}

impl Registry {
    fn deliver(&self, frame: SidecarFrame) {
        let Some(id) = frame.request_id() else {
            return;
        };
        let sender = {
            let pending = match self.pending.lock() {
                Ok(pending) => pending,
                Err(poisoned) => poisoned.into_inner(),
            };
            pending.get(id).cloned()
        };
        // An absent recipient is valid: cancellation or timeout may have already ended the request.
        if let Some(sender) = sender {
            let _ = sender.send(frame);
        }
    }

    /// Fails every in-flight request when the sidecar exits so they do not wait for
    /// `TOTAL_TIMEOUT`.
    fn fail_all(&self, message: &str) {
        self.alive.store(false, Ordering::SeqCst);
        let drained = {
            let mut pending = match self.pending.lock() {
                Ok(pending) => pending,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::take(&mut *pending)
        };
        for (id, sender) in drained {
            let _ = sender.send(SidecarFrame::Error {
                id,
                error: super::protocol::StepError {
                    // A crashed sidecar is retryable; the next `run_step` starts a replacement.
                    kind: ErrorKind::Transient,
                    message: message.to_owned(),
                    status: None,
                    retry_after_ms: None,
                },
            });
        }
    }
}

pub(crate) struct Sidecar {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    registry: Arc<Registry>,
    #[cfg(windows)]
    _containment: Option<Containment>,
}

impl Sidecar {
    fn is_alive(&self) -> bool {
        self.registry.alive.load(Ordering::SeqCst)
    }

    fn write_frame(&self, frame: &serde_json::Value) -> Result<(), String> {
        let line = serde_json::to_string(frame).map_err(|error| format!("侧车帧序列化失败: {error}"))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(format!("发往侧车的帧超过 {MAX_LINE_BYTES} 字节上限"));
        }
        let mut stdin = match self.stdin.lock() {
            Ok(stdin) => stdin,
            Err(poisoned) => poisoned.into_inner(),
        };
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("写入侧车失败: {error}"))
    }

    fn register(&self, id: &str) -> Receiver<SidecarFrame> {
        let (sender, receiver) = channel();
        let mut pending = match self.registry.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.insert(id.to_owned(), sender);
        receiver
    }

    fn unregister(&self, id: &str) {
        let mut pending = match self.registry.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.remove(id);
    }

    /// Shuts down the sidecar during application exit. The Job Object is only a fallback.
    pub(crate) fn shutdown(&self) {
        let _ = self.write_frame(&json!({ "v": PROTOCOL_VERSION, "type": "shutdown" }));
        self.registry.alive.store(false, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ------------------------------------------------------------------ Executable path

fn resolve_binary() -> Result<PathBuf, String> {
    if let Ok(value) = std::env::var(BINARY_ENV) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("{BINARY_ENV} 指向的侧车不存在：{}", path.display()));
    }
    // Production places `externalBin` next to the main executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(if cfg!(windows) {
                "mework-aisdk.exe"
            } else {
                "mework-aisdk"
            });
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // In development and tests, `current_exe()` is under `target/debug/`, while the sidecar is in
    // the source tree. Fall back to source-tree artifacts so development and tests do not depend on
    // environment variables or test execution order.
    //
    // Select the newest available artifact. `npm run build` updates `main.mjs`, while
    // `npm run build:sea` updates `mework-aisdk.exe`; a fixed selection order can silently use a
    // stale artifact.
    #[cfg(debug_assertions)]
    if let Some(path) = source_tree_sidecar() {
        return Ok(path);
    }
    Err(format!(
        "找不到 AI SDK 侧车。开发时先在 aisdk-service/ 里 `npm run build`（产出 dist/main.mjs），或 `npm run build:sea` 产出单文件可执行；也可以直接把 {BINARY_ENV} 指向任意一个。"
    ))
}

/// Returns the newest sidecar artifact in the source tree.
///
/// Separate commands update the JavaScript, SEA executable, and staged executable candidates, so a
/// fixed candidate order can silently select a stale artifact. This helper exists only in debug
/// builds because it embeds `CARGO_MANIFEST_DIR`; release builds use adjacent `externalBin`.
#[cfg(debug_assertions)]
pub(crate) fn source_tree_sidecar() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?;
    let exe = if cfg!(windows) { ".exe" } else { "" };
    // `build.rs` derives the triple from Cargo's `TARGET`; do not recompute it here.
    let staged = manifest.join("binaries").join(format!(
        "mework-aisdk-{}{exe}",
        env!("MEWORK_TARGET_TRIPLE")
    ));
    [
        root.join(format!("aisdk-service/dist/mework-aisdk{exe}")),
        root.join("aisdk-service/dist/main.mjs"),
        staged,
    ]
    .into_iter()
    .filter_map(|path| {
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        Some((modified, path))
    })
    .max_by_key(|(modified, _)| *modified)
    .map(|(_, path)| path)
}

/// Builds a command that can actually be spawned.
///
/// Release builds run a single-file executable directly. Development builds produce non-executable
/// `dist/main.mjs`, so JavaScript extensions must run through Node. Select by extension rather than
/// scanning a potentially 90 MiB executable for SEA metadata.
fn command_for(binary: &PathBuf) -> Command {
    let is_script = binary
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "mjs" | "js" | "cjs"));
    if !is_script {
        return Command::new(binary);
    }
    let node = std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_owned());
    let mut command = Command::new(node);
    command.arg(binary);
    command
}

// ------------------------------------------------------------------ Startup

/// Environment variables the sidecar may inherit.
///
/// This is an allowlist, not a denylist. AI SDK providers may otherwise discover unrelated local
/// credentials and send them to a user-configured proxy. A denylist becomes stale when upstream
/// adds providers.
///
/// The environment cannot be entirely empty: on Windows, Node requires `SystemRoot` during
/// initialization. The listed variables are process requirements and contain no model credentials.
const INHERITED_ENV: &[&str] = &[
    // Windows process bootstrap: Node crashes during initialization without `SystemRoot`.
    "SystemRoot",
    "windir",
    "SystemDrive",
    "COMSPEC",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    // Temporary directories used by Node and TLS.
    "TEMP",
    "TMP",
    // Equivalent required variables on Unix-like platforms.
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TZ",
];

/// Returns the environment the sidecar actually inherits so tests can assert that unrelated keys
/// do not leak through process setup.
pub(crate) fn inherited_env() -> Vec<(String, String)> {
    INHERITED_ENV
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_owned(), value)))
        .collect()
}

fn spawn() -> Result<Arc<Sidecar>, String> {
    let binary = resolve_binary()?;
    let mut command = command_for(&binary);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (name, value) in inherited_env() {
        command.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 AI SDK 侧车（{}）: {error}", binary.display()))?;

    #[cfg(windows)]
    let containment = match Containment::create() {
        Ok(containment) => {
            let assigned = containment.assign(&child).is_ok();
            assigned.then_some(containment)
        }
        Err(_) => None,
    };

    let stdin = child.stdin.take().ok_or("侧车没有 stdin")?;
    let stdout = child.stdout.take().ok_or("侧车没有 stdout")?;
    let stderr = child.stderr.take();

    let registry = Arc::new(Registry {
        pending: Mutex::new(HashMap::new()),
        alive: AtomicBool::new(true),
        ready_protocol: AtomicU32::new(0),
    });

    // Reader thread: the sole stdout reader.
    {
        let registry = Arc::clone(&registry);
        std::thread::Builder::new()
            .name("aisdk-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            registry.fail_all("AI SDK 侧车已退出");
                            return;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            registry.fail_all(&format!("读取侧车输出失败: {error}"));
                            return;
                        }
                    }
                    if line.len() > MAX_LINE_BYTES {
                        registry.fail_all("侧车帧超过行长上限");
                        return;
                    }
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<SidecarFrame>(trimmed) {
                        // `ready` has no request ID and cannot be delivered to a channel, so the
                        // reader records the handshake state here.
                        Ok(SidecarFrame::Ready { protocol, .. }) => {
                            registry.ready_protocol.store(protocol, Ordering::SeqCst);
                        }
                        Ok(frame) => registry.deliver(frame),
                        Err(error) => {
                            // An undecodable frame indicates protocol mismatch or polluted stdout.
                            // Continuing would only obscure the failure.
                            registry.fail_all(&format!("侧车帧无法解析: {error}"));
                            return;
                        }
                    }
                }
            })
            .map_err(|error| format!("无法启动侧车读线程: {error}"))?;
    }

    // Stderr is diagnostic-only but must be drained: an unread full pipe blocks the sidecar.
    if let Some(stderr) = stderr {
        std::thread::Builder::new()
            .name("aisdk-stderr".into())
            .spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    eprintln!("[aisdk] {line}");
                }
            })
            .ok();
    }

    let sidecar = Arc::new(Sidecar {
        stdin: Mutex::new(stdin),
        child: Mutex::new(child),
        registry,
        #[cfg(windows)]
        _containment: containment,
    });

    // Fail the handshake on protocol mismatch: an incompatible sidecar is worse than no sidecar.
    let hello_id = "hello";
    let receiver = sidecar.register(hello_id);
    // `ready` has no ID, so it cannot reach this receiver. Registration only lets `fail_all`
    // report a sidecar death during the handshake.
    sidecar.write_frame(&json!({ "v": PROTOCOL_VERSION, "type": "hello" }))?;
    let handshake = wait_for_ready(&sidecar, &receiver);
    sidecar.unregister(hello_id);
    handshake?;

    Ok(sidecar)
}

/// Waits for `ready`. Because it has no request ID, the reader cannot deliver it to a channel;
/// handshake success comes from the registry liveness flag and a bounded wait.
fn wait_for_ready(sidecar: &Arc<Sidecar>, receiver: &Receiver<SidecarFrame>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !sidecar.is_alive() {
            // `fail_all` placed the termination reason on this receiver.
            if let Ok(SidecarFrame::Error { error, .. }) = receiver.try_recv() {
                return Err(format!("AI SDK 侧车握手失败: {}", error.message));
            }
            return Err("AI SDK 侧车在握手期间退出".into());
        }
        let protocol = sidecar.registry.ready_protocol.load(Ordering::SeqCst);
        if protocol != 0 {
            return if protocol == PROTOCOL_VERSION {
                Ok(())
            } else {
                Err(format!(
                    "AI SDK 侧车协议世代不匹配：宿主 {PROTOCOL_VERSION}，侧车 {protocol}"
                ))
            };
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("AI SDK 侧车握手超时".into())
}

// ------------------------------------------------------------------ Global handle

static SIDECAR: OnceLock<Mutex<Option<Arc<Sidecar>>>> = OnceLock::new();

fn handle() -> Result<Arc<Sidecar>, String> {
    let cell = SIDECAR.get_or_init(|| Mutex::new(None));
    let mut slot = match cell.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = slot.as_ref() {
        if existing.is_alive() {
            return Ok(Arc::clone(existing));
        }
        // Replace a dead sidecar so the next request does not reuse a failed handle.
        *slot = None;
    }
    let fresh = spawn()?;
    *slot = Some(Arc::clone(&fresh));
    Ok(fresh)
}

/// Shuts down the sidecar when the application exits.
pub(crate) fn shutdown_sidecar() {
    let Some(cell) = SIDECAR.get() else { return };
    let taken = {
        let mut slot = match cell.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.take()
    };
    if let Some(sidecar) = taken {
        sidecar.shutdown();
    }
}

/// The live sidecar, if one is running. Unlike [`handle`], this never spawns:
/// callers that only want to tell an existing sidecar something have nothing to
/// say to a sidecar that does not exist.
fn existing_handle() -> Option<Arc<Sidecar>> {
    let cell = SIDECAR.get()?;
    let slot = match cell.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.as_ref()
        .filter(|sidecar| sidecar.is_alive())
        .map(Arc::clone)
}

// ------------------------------------------------------------------ Agent sessions

/// Tears down a `claude-agent` CLI session. Fire-and-forget: the frame has no
/// reply, the sidecar treats an unknown session as already released, and a dead
/// sidecar has no session to release. Called from a `Drop` impl, so it must
/// never block or fail.
pub(crate) fn release_session(session: &str) {
    let Some(sidecar) = existing_handle() else { return };
    let _ = sidecar.write_frame(&json!({
        "v": PROTOCOL_VERSION,
        "type": "release",
        "session": session,
    }));
}

// ------------------------------------------------------------------ One step

/// Flushes all reasoning segments into retained partial content in item order, independent of
/// whether and in which order their `reasoning-done` events arrived.
fn finish_partial(
    mut partial: StreamPartial,
    reasoning_buffers: BTreeMap<usize, String>,
) -> StreamPartial {
    for (_, buffer) in reasoning_buffers {
        partial.reasoning.push(buffer);
    }
    partial
}

fn usage_from(usage: &SidecarUsage) -> ModelUsage {    ModelUsage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cache_read_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        // A subset of `output_tokens`, displayed only and never included in totals.
        reasoning_tokens: usage.reasoning_tokens,
    }
}

/// A request submitted to the sidecar but not yet driven to completion.
///
/// Submission and driving must remain separate. Writing the frame to stdin is the authorization
/// fence's linearization point, allowing callers to release named-Agent and external-import read
/// locks before a long stream completes.
pub(crate) struct Submission<'a> {
    sidecar: Arc<Sidecar>,
    id: String,
    receiver: Receiver<SidecarFrame>,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl Drop for Submission<'_> {
    fn drop(&mut self) {
        self.sidecar.unregister(&self.id);
    }
}

/// Submits a model step to the sidecar. Returning means its frame has been written.
pub(crate) fn submit(request: &StepRequest) -> Result<Submission<'static>, RequestFailure> {
    let sidecar = handle().map_err(|error| RequestFailure::bare(ModelRequestError::Fatal(error)))?;
    let id = Uuid::new_v4().simple().to_string();
    // Register before writing so an extremely fast reply cannot arrive before the reader has a
    // recipient and be discarded.
    let receiver = sidecar.register(&id);
    let submission = Submission {
        sidecar: Arc::clone(&sidecar),
        id,
        receiver,
        _lifetime: std::marker::PhantomData,
    };
    let frame = json!({
        "v": PROTOCOL_VERSION,
        "type": "step",
        "id": submission.id,
        "payload": request,
    });
    sidecar
        .write_frame(&frame)
        .map_err(|error| RequestFailure::bare(ModelRequestError::Api(error)))?;
    Ok(submission)
}

/// Drives a submitted step, translating sidecar events to host events.
///
/// It blocks, retains already-streamed partial content on failure, and expresses cancellation through
/// the `event_sink` result.
pub(crate) fn drive_submission(
    submission: &Submission<'_>,
    round: usize,
    event_sink: &ModelEventSink<'_>,
) -> Result<StepResult, RequestFailure> {
    drive(
        &submission.sidecar,
        &submission.id,
        round,
        event_sink,
        &submission.receiver,
    )
}

/// Submits and drives a step in one call.
///
/// Test-only: production must use [`submit`] and [`drive_submission`] separately to release
/// authorization read locks between frame submission and stream completion. `cfg(test)` makes
/// accidental production use a compile error.
#[cfg(test)]
pub(crate) fn run_step(
    request: &StepRequest,
    round: usize,
    event_sink: &ModelEventSink<'_>,
) -> Result<StepResult, RequestFailure> {
    let submission = submit(request)?;
    drive_submission(&submission, round, event_sink)
}

fn drive(
    sidecar: &Arc<Sidecar>,
    id: &str,
    round: usize,
    event_sink: &ModelEventSink<'_>,
    receiver: &Receiver<SidecarFrame>,
) -> Result<StepResult, RequestFailure> {
    let mut partial = StreamPartial::default();
    let mut reasoning_buffers: BTreeMap<usize, String> = BTreeMap::new();
    let mut announced: Vec<String> = Vec::new();
    let mut cancelling: Option<String> = None;
    let started = Instant::now();
    // Do not use an inactivity timeout. Encrypted reasoning protocols can remain silent after
    // `reasoning-start` throughout a healthy long reasoning phase, making silence indistinguishable
    // from a stalled upstream. Heartbeats, disconnects, error frames, and HTTP failures provide
    // explicit liveness signals; `TOTAL_TIMEOUT` is the sole absolute exchange limit.

    loop {
        if started.elapsed() > TOTAL_TIMEOUT {
            // Cancel on timeout rather than only stop receiving: otherwise the sidecar continues an
            // unbounded upstream request, billing and retaining in-flight and heartbeat state.
            let _ = sidecar.write_frame(&json!({
                "v": PROTOCOL_VERSION,
                "type": "cancel",
                "id": id,
            }));
            return Err(RequestFailure {
                error: ModelRequestError::Api("单次模型交换超过 30 分钟上限".into()),
                retry_after_ms: None,
                partial: finish_partial(partial, reasoning_buffers),
            });
        }

        let frame = match receiver.recv_timeout(PROBE_INTERVAL) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => {
                // Probe cancellation even when no frame is readable.
                if cancelling.is_none() {
                    if let Err(reason) = event_sink(ModelStreamEvent::Ping) {
                        cancel(sidecar, id, &mut cancelling, reason);
                    }
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(RequestFailure {
                    error: ModelRequestError::Api("侧车通道已断开".into()),
                    retry_after_ms: None,
                    partial: finish_partial(partial, reasoning_buffers),
                });
            }
        };
        match frame {
            SidecarFrame::Ready { .. } => {
                // The reader consumes `ready` because it has no request ID. This branch preserves
                // exhaustive matching if a future protocol adds an ID.
            }
            SidecarFrame::Event { event, .. } => {
                // A cancelling request no longer forwards content to the renderer, but continues
                // reading until the sidecar sends a terminal frame.
                if cancelling.is_some() {
                    continue;
                }
                if let Err(reason) = forward(
                    event,
                    round,
                    event_sink,
                    &mut partial,
                    &mut reasoning_buffers,
                    &mut announced,
                ) {
                    cancel(sidecar, id, &mut cancelling, reason);
                }
            }
            SidecarFrame::Done { result, .. } => {
                if let Some(reason) = cancelling {
                    // Cancellation wins even if the sidecar later reports success.
                    return Err(RequestFailure {
                        error: ModelRequestError::EventSink(reason),
                        retry_after_ms: None,
                        partial: finish_partial(partial, reasoning_buffers),
                    });
                }
                return Ok(result);
            }
            SidecarFrame::Error { error, .. } => {
                if let Some(reason) = cancelling {
                    return Err(RequestFailure {
                        error: ModelRequestError::EventSink(reason),
                        retry_after_ms: None,
                        partial: finish_partial(partial, reasoning_buffers),
                    });
                }
                let message = describe(&error);
                let mapped = match error.kind {
                    ErrorKind::Transient => ModelRequestError::Api(message),
                    ErrorKind::Permanent => ModelRequestError::Fatal(message),
                    // Sidecar cancellation only follows a host `cancel` frame. Without a matching
                    // sink failure, treat it as non-retryable.
                    ErrorKind::Cancelled => ModelRequestError::Fatal(message),
                };
                return Err(RequestFailure {
                    error: mapped,
                    retry_after_ms: error.retry_after_ms,
                    partial: finish_partial(partial, reasoning_buffers),
                });
            }
        }
    }
}

/// Converts a sidecar error into host-visible text.
///
/// Preserve the upstream status code so server failures and invalid model names remain distinguishable
/// in the UI, retry notices, and diagnostics.
fn describe(error: &super::protocol::StepError) -> String {
    match error.status {
        Some(status) => format!("HTTP {status}: {}", error.message),
        None => error.message.clone(),
    }
}

fn cancel(sidecar: &Arc<Sidecar>, id: &str, cancelling: &mut Option<String>, reason: String) {
    if cancelling.is_some() {
        return;
    }
    let _ = sidecar.write_frame(&json!({
        "v": PROTOCOL_VERSION,
        "type": "cancel",
        "id": id,
    }));
    *cancelling = Some(reason);
}

fn forward(
    event: StepEvent,
    round: usize,
    event_sink: &ModelEventSink<'_>,
    partial: &mut StreamPartial,
    reasoning_buffers: &mut BTreeMap<usize, String>,
    announced: &mut Vec<String>,
) -> Result<(), String> {
    match event {
        StepEvent::TextDelta { delta } => {
            partial.text.push_str(&delta);
            event_sink(ModelStreamEvent::TextDelta { round, delta })
        }
        StepEvent::ReasoningStart { item, form } => {
            reasoning_buffers.entry(item).or_default();
            event_sink(ModelStreamEvent::ReasoningStart { round, item, form })
        }
        StepEvent::ReasoningDelta { item, delta } => {
            reasoning_buffers.entry(item).or_default().push_str(&delta);
            event_sink(ModelStreamEvent::ReasoningDelta { round, item, delta })
        }
        StepEvent::ReasoningDone { item, duration_ms } => {
            // Keep completed and open items in the same ordinal-indexed collection until
            // failure settlement; completion order is not item identity.
            event_sink(ModelStreamEvent::ReasoningDone {
                round,
                item,
                duration_ms,
            })
        }
        StepEvent::ToolCallAnnounced { call_id, tool_name } => {
            if announced.iter().any(|seen| seen == &call_id) {
                return Ok(());
            }
            announced.push(call_id.clone());
            event_sink(ModelStreamEvent::ToolCallAnnounced {
                round,
                call_id,
                tool_name,
                // The running event sink supplies this because only it knows the conversation.
                context_id: String::new(),
            })
        }
        StepEvent::ToolCall {
            call_id,
            input,
            ..
        } => {
            let input = input.as_object().cloned().unwrap_or_default();
            event_sink(ModelStreamEvent::ToolCallArgumentsReady {
                round,
                call_id,
                input,
            })
        }
        StepEvent::Usage { usage } => {
            let usage = usage_from(&usage);
            if usage == ModelUsage::default() {
                return Ok(());
            }
            event_sink(ModelStreamEvent::UsageUpdated { round, usage })
        }
        // Sources are attached during settlement to the assistant card or search envelope, so a
        // dropped live event cannot lose source facts.
        StepEvent::Source(_) => Ok(()),
        StepEvent::Heartbeat => event_sink(ModelStreamEvent::Ping),
    }
}

#[cfg(test)]
mod reasoning_partial_tests {
    use super::*;

    #[test]
    fn failed_interleaved_reasoning_preserves_item_order() {
        for finish_zero in [false, true] {
            let mut partial = StreamPartial::default();
            let mut buffers = BTreeMap::new();
            let mut announced = Vec::new();
            let forwarded = std::sync::Mutex::new(Vec::new());
            let sink = |event| {
                forwarded.lock().unwrap().push(event);
                Ok(())
            };
            let mut events = vec![
                StepEvent::ReasoningStart { item: 0, form: None },
                StepEvent::ReasoningDelta { item: 0, delta: "A".into() },
                StepEvent::ReasoningStart { item: 1, form: None },
                StepEvent::ReasoningDelta { item: 1, delta: "B".into() },
                StepEvent::ReasoningDelta { item: 0, delta: "a".into() },
                StepEvent::ReasoningDone { item: 1, duration_ms: Some(1) },
            ];
            if finish_zero {
                events.push(StepEvent::ReasoningDone { item: 0, duration_ms: Some(2) });
            }
            for event in events {
                forward(event, 3, &sink, &mut partial, &mut buffers, &mut announced).unwrap();
            }
            assert_eq!(finish_partial(partial, buffers).reasoning, ["Aa", "B"]);
            let forwarded = forwarded.lock().unwrap();
            assert!(matches!(&forwarded[3], ModelStreamEvent::ReasoningDelta { round: 3, item: 1, delta } if delta == "B"));
            assert!(matches!(&forwarded[5], ModelStreamEvent::ReasoningDone { round: 3, item: 1, .. }));
        }
    }
}

// ------------------------------------------------------------------ Parent exit containment

#[cfg(windows)]
struct Containment {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: A Job Object handle may cross threads; this type writes it only during creation and
// closes it only in `Drop`.
#[cfg(windows)]
unsafe impl Send for Containment {}
#[cfg(windows)]
unsafe impl Sync for Containment {}

#[cfg(windows)]
impl Containment {
    /// A local analogue of `git.rs::ProcessContainment`. The Git implementation is private and tied
    /// to its command tree; this sidecar needs only to prevent a large `node.exe` from surviving the
    /// host process.
    fn create() -> Result<Self, String> {
        use std::{ffi::c_void, mem::size_of, ptr};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(format!("无法创建侧车作业对象: {}", std::io::Error::last_os_error()));
        }
        let containment = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                containment.handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(format!("无法配置侧车作业对象: {}", std::io::Error::last_os_error()));
        }
        Ok(containment)
    }

    fn assign(&self, child: &Child) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        let assigned = unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as _) };
        if assigned == 0 {
            return Err(format!(
                "无法将侧车加入受控作业对象: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Containment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
