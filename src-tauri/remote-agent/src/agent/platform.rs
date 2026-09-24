//! The parts of the agent that each operating system spells its own way.
//!
//! Unix: the proxy reaches the daemon over a Unix socket in a directory only
//! the account can enter, every session is its own process group, and the
//! daemon leaves the SSH session by `setsid` and a double fork.
//!
//! Windows: the socket is loopback TCP guarded by a secret in a file under the
//! profile, every session is a job object its process is started inside, and
//! the daemon leaves the SSH session's job — which Windows' sshd kills whole
//! when the connection ends — by breaking away from it, or, where the job
//! forbids that, by asking WMI to start it outside any job.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::paths::Paths;
use crate::protocol::SignalKind;

/// A fresh random secret, hex encoded.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system's random source is available");
    hex(&buffer)
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

/// SHA-256 of a file, hex encoded.
pub fn file_digest(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex(&hasher.finalize()))
}

/// The running executable's digest, which names its build.
pub fn self_digest() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the agent executable: {error}"))?;
    file_digest(&exe).map_err(|error| format!("Cannot read {}: {error}", exe.display()))
}

/// The directory tag of a build: its version and the head of its digest.
pub fn build_tag(digest: &str) -> String {
    format!("{}-{}", crate::AGENT_VERSION, &digest[..digest.len().min(12)])
}

/// Writes `contents` to `path` readable by this account alone, replacing any
/// previous file atomically.
pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

// ---------------------------------------------------------------------------
// Locks
// ---------------------------------------------------------------------------

/// An exclusive lock on a file, held until dropped. The operating system
/// releases it when the holder dies, so a crashed daemon never leaves a lock
/// behind that blocks the next one.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Takes the lock if nobody holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::io::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
            let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if locked == 0 {
                return Ok(Some(Self { _file: file }));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            Err(error)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const ERROR_SHARING_VIOLATION: i32 = 32;
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => Ok(Some(Self { _file: file })),
                Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
                Err(error) => Err(error),
            }
        }
    }

    /// Takes the lock, waiting up to `timeout` for its holder to let go.
    pub fn acquire(path: &Path, timeout: Duration) -> io::Result<Option<Self>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(Some(lock));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// ---------------------------------------------------------------------------
// The machine-local socket between proxy and daemon
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub type LocalStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type LocalStream = std::net::TcpStream;

pub struct LocalListener {
    #[cfg(unix)]
    inner: std::os::unix::net::UnixListener,
    #[cfg(windows)]
    inner: std::net::TcpListener,
    cleanup: Vec<std::path::PathBuf>,
}

impl LocalListener {
    /// Binds the daemon's socket and publishes the secret a proxy must bring.
    /// Called only while holding the lifetime lock, so a socket file already
    /// there belongs to a daemon that died and is safe to replace.
    pub fn bind(paths: &Paths, token: &str) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let socket = paths.socket();
            let _ = std::fs::remove_file(&socket);
            let inner = std::os::unix::net::UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            write_private_file(&paths.endpoint_file(), token.as_bytes())?;
            Ok(Self {
                inner,
                cleanup: vec![socket, paths.endpoint_file()],
            })
        }
        #[cfg(windows)]
        {
            let inner = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            let port = inner.local_addr()?.port();
            let endpoint = serde_json::json!({
                "port": port,
                "token": token,
                "pid": std::process::id(),
            });
            write_private_file(&paths.endpoint_file(), endpoint.to_string().as_bytes())?;
            Ok(Self {
                inner,
                cleanup: vec![paths.endpoint_file()],
            })
        }
    }

    pub fn accept(&self) -> io::Result<LocalStream> {
        self.inner.accept().map(|(stream, _)| stream)
    }

    /// Removes the socket and secret, so a proxy arriving after the daemon
    /// is gone starts a new one instead of waiting on a dead endpoint.
    pub fn remove_files(&self) {
        for path in &self.cleanup {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Connects to the running daemon, returning the stream and the secret to
/// present on it.
pub fn connect_local(paths: &Paths) -> io::Result<(LocalStream, String)> {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(paths.socket())?;
        let token = std::fs::read_to_string(paths.endpoint_file())?;
        Ok((stream, token.trim().to_owned()))
    }
    #[cfg(windows)]
    {
        let text = std::fs::read_to_string(paths.endpoint_file())?;
        // A daemon holds its lifetime lock until it exits, however it exits.
        // An endpoint whose lock is free was left by a daemon that crashed,
        // and the port it names may since belong to any other program — which
        // would be handed the secret and, worse, the host's requests.
        if let Some(free) = FileLock::try_acquire(&paths.lifetime_lock())? {
            drop(free);
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the daemon that published this endpoint is gone",
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let port = value["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no port"))?;
        let token = value["token"].as_str().unwrap_or_default().to_owned();
        let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        Ok((stream, token))
    }
}

// ---------------------------------------------------------------------------
// Starting the daemon outside the SSH session
// ---------------------------------------------------------------------------

/// Starts `exe daemon` so that nothing tying it to the proxy's SSH session
/// survives: not the session, not the process group, not the controlling
/// terminal, not (on Windows) the session's job object.
pub fn spawn_detached_daemon(exe: &Path, paths: &Paths, extra_args: &[String]) -> io::Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .arg("--root")
        .arg(&paths.root)
        .args(extra_args)
        .current_dir(&paths.home)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setsid, then fork once more and let the middle process exit: the
        // daemon is then no session leader, can never acquire a controlling
        // terminal, and is adopted by init the moment its parent is reaped.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                match libc::fork() {
                    -1 => Err(io::Error::last_os_error()),
                    0 => Ok(()),
                    _ => libc::_exit(0),
                }
            });
        }
        let mut child = command.spawn()?;
        // The middle process exits at once; reaping it leaves no zombie.
        let _ = child.wait();
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        };
        // The proxy's own standard handles are the SSH channel's pipes. A
        // daemon that inherited a copy would hold that channel open after the
        // proxy is gone, so the host would wait for a heartbeat to learn what
        // an end of file should have told it.
        keep_standard_handles_private();
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        match command.spawn() {
            Ok(_) => Ok(()),
            // ERROR_ACCESS_DENIED: the SSH session's job does not allow
            // breaking away. WMI starts processes from its own service, which
            // no session job contains.
            Err(error) if error.raw_os_error() == Some(5) => {
                spawn_through_wmi(exe, paths, extra_args)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(windows)]
fn spawn_through_wmi(exe: &Path, paths: &Paths, extra_args: &[String]) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    let quote = |text: &std::ffi::OsStr| quote_windows_argument(&text.to_string_lossy());
    // WMI cannot hand the process any standard handles, so the daemon opens
    // its log itself.
    let mut command_line = format!(
        "{} daemon {OWN_LOG_FLAG} --root {}",
        quote(exe.as_os_str()),
        quote(paths.root.as_os_str()),
    );
    for argument in extra_args {
        command_line.push(' ');
        command_line.push_str(&quote_windows_argument(argument));
    }
    let ps_quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let script = format!(
        "$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine = {}; CurrentDirectory = {}}}; exit [int]$r.ReturnValue",
        ps_quote(&command_line),
        ps_quote(&paths.home.to_string_lossy()),
    );
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "WMI could not start the agent daemon (status {status})"
        )))
    }
}

/// Stops this process's standard handles from being inherited by the
/// processes it starts. Windows hands a child every inheritable handle, not
/// only the ones it is given as its own standard handles.
#[cfg(windows)]
pub fn keep_standard_handles_private() {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(which) };
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

/// Asks a daemon to write its log itself rather than to the standard handles
/// it was started with; see [`redirect_output_to_log`].
pub const OWN_LOG_FLAG: &str = "--own-log";

/// Points the daemon's standard output and error at its log file. For a
/// daemon started by something that could not pass it any handles (WMI).
pub fn redirect_output_to_log(paths: &Paths) -> io::Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::dup2(log.as_raw_fd(), 1);
            libc::dup2(log.as_raw_fd(), 2);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
        // Left open for the life of the process: `std` looks the standard
        // handles up on every write, so this is where the log now goes.
        let handle = log.into_raw_handle();
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, handle as _);
            SetStdHandle(STD_ERROR_HANDLE, handle as _);
        }
        Ok(())
    }
}

/// One argument of a Windows command line, quoted so that the C runtime's
/// parser (`CommandLineToArgvW`) hands it back unchanged: backslashes are
/// literal except in front of a quote, where they and the quote are escaped.
pub fn quote_windows_argument(argument: &str) -> String {
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for c in argument.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => quoted.extend(std::iter::repeat('\\').take(backslashes * 2 + 1)),
            _ => quoted.extend(std::iter::repeat('\\').take(backslashes)),
        }
        backslashes = 0;
        quoted.push(c);
    }
    // The closing quote must not be escaped by a trailing backslash.
    quoted.extend(std::iter::repeat('\\').take(backslashes * 2));
    quoted.push('"');
    quoted
}

// ---------------------------------------------------------------------------
// Sessions' process trees
// ---------------------------------------------------------------------------

/// Makes a session's process the root of a tree the daemon can end whole.
/// On Unix: a new session (hence process group) with the signal dispositions
/// a login shell would have. On Windows: a new process group, created
/// suspended so that [`ProcessTree::adopt`] can put it in its job before it
/// runs a single instruction — a child it started first would escape the job
/// — and then [`resume_suspended`] lets it go.
pub fn prepare_child(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                reset_signal_dispositions();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
}

/// Lets a process [`prepare_child`] started suspended run. A new process has
/// exactly one thread, found here by its owner's id: `std` keeps the handle
/// to it private.
#[cfg(windows)]
pub fn resume_suspended(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut resumed = 0;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                if unsafe { ResumeThread(thread) } != u32::MAX {
                    resumed += 1;
                }
                unsafe { CloseHandle(thread) };
            }
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    if resumed == 0 {
        return Err(io::Error::other("found no thread of the new process to resume"));
    }
    Ok(())
}

/// The daemon ignores `SIGHUP` and handles `SIGTERM`/`SIGINT` itself; a child
/// must start with the defaults, or a shell it runs would ignore the hangup
/// that ends a terminal session. Async-signal-safe: called between fork and
/// exec.
#[cfg(unix)]
pub fn reset_signal_dispositions() {
    unsafe {
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM, libc::SIGQUIT, libc::SIGPIPE] {
            libc::signal(signal, libc::SIG_DFL);
        }
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
    }
}

/// Everything a session started, addressable as one.
pub struct ProcessTree {
    pid: u32,
    #[cfg(windows)]
    job: Option<windows_sys::Win32::Foundation::HANDLE>,
    /// The root process itself, for the one case the job cannot cover: a
    /// process that could not be put in it.
    #[cfg(windows)]
    process: Option<windows_sys::Win32::Foundation::HANDLE>,
}

// Both handles are process-wide kernel handles, usable from any thread.
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
#[cfg(windows)]
unsafe impl Sync for ProcessTree {}

impl ProcessTree {
    /// Takes ownership of the tree rooted at `pid`, which [`prepare_child`]
    /// (or the pseudo terminal's own `setsid`) made a process group leader.
    #[cfg(unix)]
    pub fn adopt(pid: u32) -> Self {
        Self { pid }
    }

    /// Puts the process in a fresh job, so that ending the job ends
    /// everything the process started. `process` is borrowed; the tree keeps
    /// its own copy of it.
    ///
    /// The job is not kill-on-close, which keeps Windows in step with Unix:
    /// ending a session ends its tree, but a session released after its
    /// process exited leaves alone what that process deliberately left
    /// running — a server started in the background with its output
    /// redirected — just as a Unix process group outlives its leader.
    ///
    /// Nothing may break away from it, though. Cygwin, and with it MSYS2 and
    /// Git Bash, starts every Windows program with `CREATE_BREAKAWAY_FROM_JOB`
    /// whenever its own job allows that: under a job that did, every `node`
    /// or `python` a Bash session ran would be outside the tree, and stopping
    /// the session — a dev server, a command that timed out — would stop
    /// Bash and leave the program running.
    #[cfg(windows)]
    pub fn adopt(pid: u32, process: windows_sys::Win32::Foundation::HANDLE) -> Self {
        use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS};
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let own = {
            let mut copy = std::ptr::null_mut();
            let duplicated = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    process,
                    GetCurrentProcess(),
                    &mut copy,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            (duplicated != 0 && !copy.is_null()).then_some(copy)
        };
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        // A new job has no limits, and none are set: in particular neither
        // breakaway limit.
        let job = (!job.is_null()).then_some(job).and_then(|job| {
            let assigned = unsafe { AssignProcessToJobObject(job, process) };
            if assigned == 0 {
                unsafe { CloseHandle(job) };
                return None;
            }
            Some(job)
        });
        Self { pid, job, process: own }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Sends `signal` to the whole tree. Returns whether anything was
    /// signalled.
    pub fn signal(&self, signal: SignalKind) -> bool {
        #[cfg(unix)]
        {
            let number = match signal {
                SignalKind::Interrupt => libc::SIGINT,
                SignalKind::Terminate => libc::SIGTERM,
                SignalKind::Hangup => libc::SIGHUP,
                SignalKind::Kill => libc::SIGKILL,
            };
            let group = -(self.pid as libc::pid_t);
            if unsafe { libc::kill(group, number) } == 0 {
                return true;
            }
            // Not a group leader after all (it failed to `setsid`): the
            // process alone is still better than nothing.
            unsafe { libc::kill(self.pid as libc::pid_t, number) == 0 }
        }
        #[cfg(windows)]
        {
            // Windows has no signal a console-less process can be sent short
            // of termination; every kind ends the tree. (A terminal's shell
            // is interrupted the way a person would: by typing Ctrl-C.)
            let _ = signal;
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            use windows_sys::Win32::System::Threading::TerminateProcess;
            if let Some(job) = self.job {
                if unsafe { TerminateJobObject(job, 1) } != 0 {
                    return true;
                }
            }
            match self.process {
                Some(process) => unsafe { TerminateProcess(process, 1) != 0 },
                None => false,
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        for handle in [self.job.take(), self.process.take()].into_iter().flatten() {
            unsafe { CloseHandle(handle) };
        }
    }
}

// ---------------------------------------------------------------------------
// The daemon's own signals
// ---------------------------------------------------------------------------

static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the daemon was asked to stop by a signal.
pub fn stop_requested() -> bool {
    STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// `SIGTERM`/`SIGINT` ask the daemon to end its sessions and exit; `SIGHUP`,
/// which the end of an SSH session can still deliver to a stray group, is
/// ignored.
pub fn install_daemon_signal_handlers() {
    #[cfg(unix)]
    {
        extern "C" fn request_stop(_: libc::c_int) {
            STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            let handler = request_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGINT, handler);
        }
    }
}

/// Makes the daemon inherit the orphans of its sessions (Linux), so a
/// process a session's shell double-forked is still reaped by someone who
/// knows about it rather than piling up under init.
pub fn become_subreaper() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

/// Reaps any child that is not a session's direct process: the orphans a
/// subreaper inherits. Sessions' own processes are waited on by their own
/// threads through `std`, which reaps by pid.
pub fn reap_orphans() {
    #[cfg(target_os = "linux")]
    {
        // Only reap processes whose parent is this daemon and that no
        // session thread waits for: waiting on `-1` would steal a session's
        // exit status. The kernel offers no "adopted only" filter, so the
        // orphans are found by elimination in /proc.
        let own = std::process::id();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // Fields after the parenthesized command name: state, ppid, pgrp, session…
            let Some(after) = stat.rfind(')').map(|at| &stat[at + 2..]) else {
                continue;
            };
            let mut fields = after.split_whitespace();
            let state = fields.next().unwrap_or_default();
            let ppid: u32 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let pgrp: i32 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            // A session's own process leads its group (pgrp == pid); an
            // adopted orphan joined some other group.
            if ppid == own && state == "Z" && pgrp != pid {
                unsafe {
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_argument_survives_the_c_runtime_parser() {
        assert_eq!(quote_windows_argument(r"C:\Users\ada\.mework\remote"), r#""C:\Users\ada\.mework\remote""#);
        // A trailing backslash would otherwise escape the closing quote.
        assert_eq!(quote_windows_argument(r"C:\"), r#""C:\\""#);
        assert_eq!(quote_windows_argument(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_argument(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_windows_argument(""), r#""""#);
    }
}
