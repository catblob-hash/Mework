#[cfg(windows)]
use super::{
    configure_windows_managed_child, resolve_windows_stdio_command, resume_windows_managed_child,
    WindowsProcessJob,
};
use super::{inherit_runtime_environment, is_untrusted_display_control, McpError, McpErrorKind};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};
use url::{Host, Url};
use wait_timeout::ChildExt;

const READINESS_SLICE: Duration = Duration::from_millis(25);
const CONNECT_SLICE: Duration = Duration::from_millis(100);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_ARGUMENT_COUNT: usize = 1_024;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_LAUNCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;

static MANAGED_HTTP_POOL: OnceLock<Mutex<ManagedHttpPool>> = OnceLock::new();

#[derive(Default)]
struct ManagedHttpPool {
    ready: HashMap<String, Weak<ManagedHttpSidecar>>,
    starting: HashSet<String>,
    starting_ports: HashMap<u16, String>,
}

#[derive(Clone)]
pub(super) struct ManagedHttpLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub url: String,
}

impl std::fmt::Debug for ManagedHttpLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedHttpLaunch")
            .field("command", &"<redacted>")
            .field("argument_count", &self.args.len())
            .field("environment_count", &self.env.len())
            .field("has_working_directory", &self.cwd.is_some())
            .field("endpoint", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManagedEndpoint {
    url: String,
    host_port: u16,
    path: String,
}

pub(super) struct ManagedHttpSidecar {
    endpoint: ManagedEndpoint,
    child: Mutex<Child>,
    #[cfg(windows)]
    process_job: WindowsProcessJob,
}

impl std::fmt::Debug for ManagedHttpSidecar {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedHttpSidecar")
            .field("endpoint", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ManagedHttpSidecar {
    pub(super) fn url(&self) -> &str {
        &self.endpoint.url
    }

    fn is_running(&self) -> Result<bool, McpError> {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|_| McpError::new(McpErrorKind::Transport, "无法读取受管 MCP 进程状态"))
    }
}

pub(super) fn acquire_managed_http_sidecar(
    launch: ManagedHttpLaunch,
    deadline: Instant,
    operation_cancellation: Option<&AtomicBool>,
    shutdown_cancellation: Option<&AtomicBool>,
) -> Result<Arc<ManagedHttpSidecar>, McpError> {
    let endpoint = validate_launch(&launch)?;
    if cancelled(operation_cancellation, shutdown_cancellation) {
        return Err(cancelled_error());
    }
    if Instant::now() >= deadline {
        return Err(timeout_error());
    }

    let fingerprint = launch_fingerprint(&launch, &endpoint);
    let pool = MANAGED_HTTP_POOL.get_or_init(|| Mutex::new(ManagedHttpPool::default()));
    loop {
        let mut state = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.ready.retain(|_, entry| entry.strong_count() > 0);
        if let Some(sidecar) = state.ready.get(&fingerprint).and_then(Weak::upgrade) {
            if sidecar.is_running()? {
                return Ok(sidecar);
            }
            state.ready.remove(&fingerprint);
        }
        if state.starting.contains(&fingerprint) {
            drop(state);
            wait_for_starting_peer(deadline, operation_cancellation, shutdown_cancellation)?;
            continue;
        }
        if state
            .starting_ports
            .get(&endpoint.host_port)
            .is_some_and(|owner| owner != &fingerprint)
        {
            return Err(McpError::new(
                McpErrorKind::Transport,
                "受管 MCP 监听端口正在被其他服务启动",
            ));
        }
        state.starting.insert(fingerprint.clone());
        state
            .starting_ports
            .insert(endpoint.host_port, fingerprint.clone());
        break;
    }

    let host_port = endpoint.host_port;
    let result = (|| {
        reject_occupied_port(host_port)?;
        spawn_sidecar(
            launch,
            endpoint,
            deadline,
            operation_cancellation,
            shutdown_cancellation,
        )
    })();
    let mut state = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    state.starting.remove(&fingerprint);
    if state
        .starting_ports
        .get(&host_port)
        .is_some_and(|owner| owner == &fingerprint)
    {
        state.starting_ports.remove(&host_port);
    }
    if let Ok(sidecar) = &result {
        state.ready.insert(fingerprint, Arc::downgrade(sidecar));
    }
    result
}

pub(super) fn validate_managed_http_launch(launch: &ManagedHttpLaunch) -> Result<(), McpError> {
    validate_launch(launch).map(|_| ())
}

fn validate_launch(launch: &ManagedHttpLaunch) -> Result<ManagedEndpoint, McpError> {
    if launch.command.trim().is_empty()
        || launch.command.len() > MAX_COMMAND_BYTES
        || launch.command.chars().any(is_forbidden_control)
        || launch.command.contains('\0')
    {
        return Err(invalid_configuration("受管 MCP command 无效"));
    }
    if launch.args.len() > MAX_ARGUMENT_COUNT {
        return Err(invalid_configuration("受管 MCP 参数数量超过安全边界"));
    }
    if launch
        .args
        .iter()
        .try_fold(0usize, |total, argument| total.checked_add(argument.len()))
        .is_none_or(|total| total > MAX_LAUNCH_BYTES)
    {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "受管 MCP 参数超过安全边界",
        ));
    }
    if launch.env.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(invalid_configuration("受管 MCP 环境变量数量超过安全边界"));
    }
    if launch
        .args
        .iter()
        .any(|argument| argument.contains('\0') || argument.chars().any(is_forbidden_control))
        || launch.env.iter().any(|(name, value)| {
            name.is_empty()
                || name.contains(['=', '\0'])
                || name.chars().any(is_forbidden_control)
                || value.contains('\0')
        })
    {
        return Err(invalid_configuration("受管 MCP 启动配置无效"));
    }
    if launch
        .env
        .iter()
        .any(|(name, value)| name.len().saturating_add(value.len()) > MAX_ENVIRONMENT_VALUE_BYTES)
    {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "受管 MCP 环境变量超过安全边界",
        ));
    }
    if let Some(cwd) = launch.cwd.as_deref() {
        if cwd.contains('\0') || cwd.chars().any(is_forbidden_control) {
            return Err(invalid_configuration("受管 MCP cwd 无效"));
        }
        let metadata = std::fs::metadata(cwd)
            .map_err(|_| invalid_configuration("受管 MCP cwd 不存在或不可访问"))?;
        if !metadata.is_dir() {
            return Err(invalid_configuration("受管 MCP cwd 不是目录"));
        }
    }

    if launch.url.len() > MAX_ENVIRONMENT_VALUE_BYTES
        || !launch.url.starts_with("http://127.0.0.1:")
        || launch.url.chars().any(is_forbidden_control)
    {
        return Err(invalid_configuration("受管 MCP URL 无效"));
    }
    let parsed = Url::parse(&launch.url).map_err(|_| invalid_configuration("受管 MCP URL 无效"))?;
    if parsed.scheme() != "http"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host() != Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
    {
        return Err(invalid_configuration(
            "受管 MCP URL 必须是无凭据的 127.0.0.1 HTTP 地址",
        ));
    }
    let host_port = parsed
        .port()
        .filter(|port| *port > 0)
        .ok_or_else(|| invalid_configuration("受管 MCP URL 必须包含有效端口"))?;
    let path = raw_url_path(&launch.url)
        .filter(|path| valid_absolute_path(path))
        .ok_or_else(|| invalid_configuration("受管 MCP path 必须是安全的绝对路径"))?;
    if parsed.path() != path {
        return Err(invalid_configuration(
            "受管 MCP path 不得包含路径归一化绕过",
        ));
    }
    Ok(ManagedEndpoint {
        url: launch.url.clone(),
        host_port,
        path: path.to_owned(),
    })
}

fn raw_url_path(url: &str) -> Option<&str> {
    let authority = url.find("://")?.checked_add(3)?;
    let path = url[authority..].find('/')?.checked_add(authority)?;
    Some(&url[path..])
}

fn valid_absolute_path(path: &str) -> bool {
    if !path.starts_with('/')
        || path.contains("//")
        || path.contains('\\')
        || path.chars().any(is_forbidden_control)
    {
        return false;
    }
    let Some(decoded) = percent_decode_path(path) else {
        return false;
    };
    !decoded.contains("//")
        && !decoded.contains('\\')
        && !decoded.chars().any(is_forbidden_control)
        && !decoded
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = decode_hex(*bytes.get(index + 1)?)?;
        let low = decode_hex(*bytes.get(index + 2)?)?;
        let value = (high << 4) | low;
        // Separators and dot segments must be literal so validation cannot be
        // bypassed by a server/router that decodes them before routing.
        if matches!(value, b'%' | b'/' | b'\\' | b'.') {
            return None;
        }
        decoded.push(value);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn is_forbidden_control(character: char) -> bool {
    is_untrusted_display_control(character)
}

fn reject_occupied_port(port: u16) -> Result<(), McpError> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map(drop)
        .map_err(|_| McpError::new(McpErrorKind::Transport, "受管 MCP 监听端口已被占用"))
}

fn wait_for_starting_peer(
    deadline: Instant,
    operation_cancellation: Option<&AtomicBool>,
    shutdown_cancellation: Option<&AtomicBool>,
) -> Result<(), McpError> {
    if cancelled(operation_cancellation, shutdown_cancellation) {
        return Err(cancelled_error());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(timeout_error());
    }
    thread::sleep(READINESS_SLICE.min(remaining));
    Ok(())
}

fn spawn_sidecar(
    launch: ManagedHttpLaunch,
    endpoint: ManagedEndpoint,
    deadline: Instant,
    operation_cancellation: Option<&AtomicBool>,
    shutdown_cancellation: Option<&AtomicBool>,
) -> Result<Arc<ManagedHttpSidecar>, McpError> {
    #[cfg(windows)]
    let process_job = WindowsProcessJob::new()?;
    #[cfg(windows)]
    let executable =
        resolve_windows_stdio_command(&launch.command, &launch.env, launch.cwd.as_deref())?;
    #[cfg(not(windows))]
    let executable = std::path::PathBuf::from(&launch.command);

    let mut command = Command::new(&executable);
    command
        .args(&launch.args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    inherit_runtime_environment(&mut command);
    command.envs(&launch.env);
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    configure_child_process(&mut command);

    let mut child = command.spawn().map_err(|error| {
        McpError::new(
            McpErrorKind::Transport,
            format!("无法启动受管 MCP 进程：{}", error.kind()),
        )
    })?;
    #[cfg(windows)]
    if let Err(error) = process_job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    #[cfg(windows)]
    if let Err(error) = resume_windows_managed_child(&child) {
        process_job.terminate();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    let sidecar = Arc::new(ManagedHttpSidecar {
        endpoint,
        child: Mutex::new(child),
        #[cfg(windows)]
        process_job,
    });
    wait_until_ready(
        &sidecar,
        deadline,
        operation_cancellation,
        shutdown_cancellation,
    )?;
    Ok(sidecar)
}

fn wait_until_ready(
    sidecar: &Arc<ManagedHttpSidecar>,
    deadline: Instant,
    operation_cancellation: Option<&AtomicBool>,
    shutdown_cancellation: Option<&AtomicBool>,
) -> Result<(), McpError> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, sidecar.endpoint.host_port);
    loop {
        if cancelled(operation_cancellation, shutdown_cancellation) {
            return Err(cancelled_error());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(timeout_error());
        }
        {
            let mut child = sidecar
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if child
                .try_wait()
                .map_err(|_| McpError::new(McpErrorKind::Transport, "无法读取受管 MCP 进程状态"))?
                .is_some()
            {
                return Err(McpError::new(
                    McpErrorKind::Transport,
                    "受管 MCP 进程在就绪前退出",
                ));
            }
        }
        let connect_budget = CONNECT_SLICE.min(deadline.saturating_duration_since(now));
        if TcpStream::connect_timeout(&address.into(), connect_budget).is_ok() {
            return Ok(());
        }
        thread::sleep(READINESS_SLICE.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn launch_fingerprint(launch: &ManagedHttpLaunch, endpoint: &ManagedEndpoint) -> String {
    let mut hasher = Sha256::new();
    fingerprint_field(&mut hasher, b"managed-http-v1");
    fingerprint_field(&mut hasher, launch.command.as_bytes());
    for argument in &launch.args {
        fingerprint_field(&mut hasher, argument.as_bytes());
    }
    for (name, value) in &launch.env {
        fingerprint_field(&mut hasher, name.as_bytes());
        fingerprint_field(&mut hasher, value.as_bytes());
    }
    fingerprint_field(
        &mut hasher,
        launch.cwd.as_deref().unwrap_or_default().as_bytes(),
    );
    fingerprint_field(&mut hasher, endpoint.url.as_bytes());
    hex_digest(hasher.finalize().as_slice())
}

fn fingerprint_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(windows)]
fn configure_child_process(command: &mut Command) {
    configure_windows_managed_child(command);
}

#[cfg(unix)]
fn configure_child_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: this only changes the child process group between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(any(windows, unix)))]
fn configure_child_process(_command: &mut Command) {}

impl Drop for ManagedHttpSidecar {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.process_job.terminate();
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait_timeout(SHUTDOWN_TIMEOUT);
    }
}

fn cancelled(
    operation_cancellation: Option<&AtomicBool>,
    shutdown_cancellation: Option<&AtomicBool>,
) -> bool {
    operation_cancellation.is_some_and(|flag| flag.load(Ordering::Acquire))
        || shutdown_cancellation.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn invalid_configuration(message: &'static str) -> McpError {
    McpError::new(McpErrorKind::InvalidConfiguration, message)
}

fn cancelled_error() -> McpError {
    McpError::new(McpErrorKind::Cancelled, "受管 MCP 启动已取消")
}

fn timeout_error() -> McpError {
    McpError::new(McpErrorKind::Timeout, "受管 MCP 启动超时")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn launch(url: &str) -> ManagedHttpLaunch {
        ManagedHttpLaunch {
            command: "sidecar-secret-command".into(),
            args: vec!["--secret-argument".into()],
            env: BTreeMap::from([("API_TOKEN".into(), "secret-value".into())]),
            cwd: None,
            url: url.into(),
        }
    }

    fn helper_launch(port: u16, mode: &str) -> ManagedHttpLaunch {
        ManagedHttpLaunch {
            command: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "managed_http_sidecar_process_helper".into(),
                "--nocapture".into(),
            ],
            env: BTreeMap::from([
                ("MEWORK_MANAGED_HTTP_TEST_MODE".into(), mode.into()),
                ("MEWORK_MANAGED_HTTP_TEST_PORT".into(), port.to_string()),
            ]),
            cwd: None,
            url: format!("http://127.0.0.1:{port}/api/mcp"),
        }
    }

    fn available_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    fn wait_for_port_release(port: u16) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => {
                    drop(listener);
                    return;
                }
                Err(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("managed sidecar did not release its port: {error}"),
            }
        }
    }

    #[test]
    fn managed_http_sidecar_process_helper() {
        let Ok(mode) = std::env::var("MEWORK_MANAGED_HTTP_TEST_MODE") else {
            return;
        };
        if mode == "exit" {
            return;
        }
        let port = std::env::var("MEWORK_MANAGED_HTTP_TEST_PORT")
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
        if mode == "mcp" {
            for connection in listener.incoming() {
                let Ok(mut stream) = connection else {
                    return;
                };
                handle_test_mcp_http_request(&mut stream);
            }
            return;
        }
        for connection in listener.incoming() {
            if connection.is_err() {
                return;
            }
        }
    }

    fn handle_test_mcp_http_request(stream: &mut TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut request = Vec::new();
        let mut chunk = [0u8; 2_048];
        let mut expected = None;
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
                Err(_) => return,
            }
            if expected.is_none() {
                if let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected = Some(header_end + 4 + content_length);
                }
            }
            if expected.is_some_and(|expected| request.len() >= expected) {
                break;
            }
        }
        let header_end = request
            .windows(4)
            .position(|value| value == b"\r\n\r\n")
            .unwrap();
        let request_line = String::from_utf8_lossy(&request[..header_end])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        if request_line.starts_with("DELETE ") {
            if let Ok(marker) = std::env::var("MEWORK_MANAGED_HTTP_DELETE_MARKER") {
                let _ = std::fs::write(marker, b"deleted");
            }
            write_test_http_response(stream, 200, None, false);
            return;
        }
        let body = serde_json::from_slice::<serde_json::Value>(&request[header_end + 4..])
            .unwrap_or(serde_json::Value::Null);
        let method = body
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let id = body.get("id").cloned();
        match (method, id) {
            ("initialize", Some(id)) => write_test_http_response(
                stream,
                200,
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": super::super::LATEST_PROTOCOL_VERSION,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "managed-test", "version": "1" }
                    }
                })),
                true,
            ),
            ("tools/list", Some(id)) => write_test_http_response(
                stream,
                200,
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": [] }
                })),
                false,
            ),
            ("notifications/initialized", None) => {
                write_test_http_response(stream, 202, None, false)
            }
            _ => write_test_http_response(stream, 400, None, false),
        }
    }

    fn write_test_http_response(
        stream: &mut TcpStream,
        status: u16,
        body: Option<serde_json::Value>,
        session: bool,
    ) {
        let body = body
            .map(|value| serde_json::to_vec(&value).unwrap())
            .unwrap_or_default();
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            _ => "Bad Request",
        };
        let session_header = if session {
            "Mcp-Session-Id: managed-test\r\n"
        } else {
            ""
        };
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{session_header}Connection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }

    #[test]
    fn process_lifecycle_starts_reuses_and_releases_the_port_on_last_drop() {
        let port = available_port();
        let launch = helper_launch(port, "listen");
        let first = acquire_managed_http_sidecar(
            launch.clone(),
            Instant::now() + Duration::from_secs(5),
            None,
            None,
        )
        .unwrap();
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok());

        let second = acquire_managed_http_sidecar(
            launch,
            Instant::now() + Duration::from_secs(5),
            None,
            None,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        drop(first);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok());
        drop(second);
        wait_for_port_release(port);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    }

    #[test]
    fn managed_transport_initializes_over_http_and_deletes_before_sidecar_drop() {
        let port = available_port();
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("delete.marker");
        let mut launch = helper_launch(port, "mcp");
        launch.env.insert(
            "MEWORK_MANAGED_HTTP_DELETE_MARKER".into(),
            marker.to_string_lossy().into_owned(),
        );
        let server = super::super::RuntimeMcpServer {
            artifact_id: "managed-artifact".into(),
            server_id: "managed-server".into(),
            name: "Managed Server".into(),
            description: String::new(),
            disabled_tools: Vec::new(),
            confirm_every_call_tools: Vec::new(),
            request_timeout: None,
            transport: super::super::RuntimeMcpTransport::ManagedHttp {
                command: launch.command,
                args: launch.args,
                env: launch.env,
                cwd: launch.cwd,
                url: launch.url,
                headers: BTreeMap::new(),
            },
        };
        let options = super::super::McpClientOptions {
            request_timeout: Duration::from_secs(3),
            shutdown_timeout: Duration::from_secs(1),
            discovery_timeout: Duration::from_secs(5),
            max_total_schema_bytes: 1024 * 1024,
        };
        let (mut connection, session) = super::super::initialize_connection(
            &server,
            options,
            Some(Instant::now() + Duration::from_secs(5)),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            session.protocol_version,
            super::super::LATEST_PROTOCOL_VERSION
        );
        assert!(super::super::list_tools(
            &mut connection,
            options.request_timeout,
            Some(Instant::now() + Duration::from_secs(3)),
        )
        .unwrap()
        .is_empty());
        drop(connection);
        wait_for_port_release(port);
        assert_eq!(std::fs::read(marker).unwrap(), b"deleted");
    }

    #[test]
    fn child_exit_before_readiness_is_reported_without_leaking_the_port() {
        let port = available_port();
        let error = acquire_managed_http_sidecar(
            helper_launch(port, "exit"),
            Instant::now() + Duration::from_secs(5),
            None,
            None,
        )
        .err()
        .expect("helper should exit before readiness");
        assert_eq!(error.kind, McpErrorKind::Transport);
        assert!(!error.message.contains(&port.to_string()));
        wait_for_port_release(port);
    }

    #[test]
    fn endpoint_requires_exact_loopback_http_and_safe_absolute_path() {
        for address in [
            "https://127.0.0.1:3210/mcp",
            "http://localhost:3210/mcp",
            "http://2130706433:3210/mcp",
            "http://127.0.0.1/mcp",
            "http://user@127.0.0.1:3210/mcp",
            "http://127.0.0.1:3210/mcp?secret=1",
            "http://127.0.0.1:3210/mcp#fragment",
            "http://127.0.0.1:3210/../mcp",
            "http://127.0.0.1:3210/api//mcp",
            "http://127.0.0.1:3210/api/%2e%2e/mcp",
            "http://127.0.0.1:3210/api/%252e%252e/mcp",
            "http://127.0.0.1:3210/api%2fmcp",
            "http://127.0.0.1:3210/api%5cmcp",
            "http://127.0.0.1:3210/api/%00/mcp",
            "http://127.0.0.1:3210/api/%E2%81%A5/mcp",
        ] {
            assert!(validate_launch(&launch(address)).is_err(), "{address}");
        }
        let endpoint = validate_launch(&launch("http://127.0.0.1:3210/api/mcp")).unwrap();
        assert_eq!(endpoint.host_port, 3210);
        assert_eq!(endpoint.path, "/api/mcp");
    }

    #[test]
    fn occupied_port_is_rejected_before_any_process_is_spawned() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = acquire_managed_http_sidecar(
            launch(&format!("http://127.0.0.1:{port}/mcp")),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Transport);
        assert!(!error.message.contains(&port.to_string()));
    }

    #[test]
    fn cancellation_and_shutdown_are_observed_before_spawn_and_while_waiting() {
        let cancelled = AtomicBool::new(true);
        for (operation, shutdown) in [(Some(&cancelled), None), (None, Some(&cancelled))] {
            let error = acquire_managed_http_sidecar(
                launch("http://127.0.0.1:3210/mcp"),
                Instant::now() + Duration::from_secs(1),
                operation,
                shutdown,
            )
            .err()
            .expect("cancelled launch");
            assert_eq!(error.kind, McpErrorKind::Cancelled);

            let error = wait_for_starting_peer(
                Instant::now() + Duration::from_secs(1),
                operation,
                shutdown,
            )
            .unwrap_err();
            assert_eq!(error.kind, McpErrorKind::Cancelled);
        }
    }

    #[test]
    fn debug_and_validation_errors_do_not_expose_launch_secrets() {
        let launch = launch("http://127.0.0.1:3210/mcp");
        let debug = format!("{launch:?}");
        for secret in [
            "sidecar-secret-command",
            "--secret-argument",
            "secret-value",
            "127.0.0.1",
        ] {
            assert!(!debug.contains(secret));
        }

        let mut invalid = launch;
        invalid.url = "http://user:password@example.com/mcp?token=secret".into();
        let error = validate_launch(&invalid).unwrap_err().to_string();
        for secret in ["user", "password", "example.com", "token"] {
            assert!(!error.contains(secret));
        }
    }

    #[test]
    fn fingerprint_changes_for_launch_environment_cwd_and_endpoint() {
        let base = launch("http://127.0.0.1:3210/mcp");
        let endpoint = validate_launch(&base).unwrap();
        let fingerprint = launch_fingerprint(&base, &endpoint);

        let mut changed = base.clone();
        changed.env.insert("OTHER".into(), "value".into());
        assert_ne!(
            fingerprint,
            launch_fingerprint(&changed, &validate_launch(&changed).unwrap())
        );

        let changed = launch("http://127.0.0.1:3211/mcp");
        assert_ne!(
            fingerprint,
            launch_fingerprint(&changed, &validate_launch(&changed).unwrap())
        );

        let directory = tempfile::tempdir().unwrap();
        let mut changed = base;
        changed.cwd = Some(directory.path().to_string_lossy().into_owned());
        assert_ne!(
            fingerprint,
            launch_fingerprint(&changed, &validate_launch(&changed).unwrap())
        );
    }
}
