//! macOS: the cell runs under Seatbelt, the kernel sandbox Apple's own
//! programs and Chromium use, entered through `/usr/bin/sandbox-exec`.
//!
//! The profile follows the one Anthropic's sandbox-runtime (Apache-2.0) gives
//! Claude Code's commands, itself derived from Chromium's: everything is
//! denied unless allowed, processes may only inspect and signal processes in
//! the same sandbox, Mach services are an explicit short list, and the only
//! connection allowed out is to the proxy on loopback, which applies the
//! network policy outside the sandbox. It differs where Mework's threat is
//! stricter: the keychain's services are not on the list, credentials are not
//! readable, and files that run outside the sandbox are protected by pattern at
//! any depth, together with the renames and hard links that would get around a
//! path rule.

use std::path::{Path, PathBuf};

use super::rules::Rules;

/// Where the proxy for this cell listens: `127.0.0.1:port`.
#[derive(Clone, Copy, Debug)]
pub struct ProxyPort(pub u16);

/// The Seatbelt profile for one cell.
pub fn profile(rules: &Rules, proxy: ProxyPort) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(PREAMBLE);

    out.push_str("\n; Network: loopback servers of the cell's own, and the proxy.\n");
    out.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
    out.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
    out.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{}\"))\n",
        proxy.0
    ));

    out.push_str("\n; Reading: everything but credentials and devices.\n");
    out.push_str("(allow file-read*)\n");
    // Device files are not data: another terminal's device yields what is
    // typed into it, a packet filter device what crosses the network. Only
    // the harmless ones stay open.
    out.push_str(DEVICES);
    if !rules.deny_read.is_empty() {
        out.push_str("(deny file-read* file-link\n");
        for path in &rules.deny_read {
            out.push_str(&format!("  (subpath {})\n", quote(path)));
        }
        out.push_str(")\n");
        // Listing a directory's entries is still its own permission; knowing
        // that a denied directory exists is harmless and keeps `stat` working.
        out.push_str("(allow file-read-metadata (vnode-type DIRECTORY))\n");
    }
    if !rules.readable.is_empty() {
        out.push_str("(allow file-read*\n");
        for path in &rules.readable {
            out.push_str(&format!("  (subpath {})\n", quote(path)));
        }
        out.push_str(")\n");
    }

    out.push_str("\n; Writing: the workspaces and the cell's own directories.\n");
    out.push_str("(allow file-write*\n");
    for path in &rules.writable {
        out.push_str(&format!("  (subpath {})\n", quote(path)));
    }
    for device in [
        "/dev/null",
        "/dev/zero",
        "/dev/tty",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/dtracehelper",
        "/dev/autofs_nowait",
    ] {
        out.push_str(&format!("  (literal \"{device}\")\n"));
    }
    out.push_str("  (subpath \"/dev/fd\")\n");
    out.push_str(")\n");

    out.push_str("\n; Never writable, even inside a workspace.\n");
    let mut denied_writes: Vec<String> = rules
        .deny_write
        .iter()
        .map(|path| format!("(subpath {})", quote(path)))
        .collect();
    for root in &rules.writable {
        let root = regex_escape(&root.to_string_lossy());
        // `.git` itself — a directory to rename away, a file to point
        // elsewhere — and what in it git would run or obey.
        denied_writes.push(format!("(regex #\"^{root}/(.*/)?\\.git$\")"));
        denied_writes.push(format!(
            "(regex #\"^{root}/(.*/)?\\.git/(.*/)?(hooks|config|config\\.worktree|commondir)(/.*)?$\")"
        ));
        for name in &rules.protected_names {
            if name.starts_with(".git/") {
                continue;
            }
            denied_writes.push(format!("(regex #\"^{root}/(.*/)?{}(/.*)?$\")", regex_escape(name)));
        }
        // A `HEAD` outside `.git` would make its directory a bare
        // repository whose config git honours.
        denied_writes.push(format!(
            "(require-all (regex #\"^{root}/(.*/)?HEAD$\") (require-not (regex #\"^{root}/(.*/)?\\.git/\")))"
        ));
    }
    if !denied_writes.is_empty() {
        out.push_str("(deny file-write* file-link\n");
        for rule in &denied_writes {
            out.push_str(&format!("  {rule}\n"));
        }
        out.push_str(")\n");
    }

    // Renaming a directory that holds a protected path moves the path out
    // from under its rule. Only directories the cell can write matter.
    let ancestors = protected_ancestors(rules);
    if !ancestors.is_empty() {
        out.push_str("(deny file-write-unlink file-write-create\n");
        for path in ancestors {
            out.push_str(&format!("  (literal {})\n", quote(&path)));
        }
        out.push_str(")\n");
    }

    out
}

/// The `sandbox-exec` invocation that runs `program` under `profile`.
pub fn argv(profile: &str, program: &[String]) -> Vec<String> {
    let mut argv = vec!["/usr/bin/sandbox-exec".to_owned(), "-p".to_owned(), profile.to_owned()];
    argv.extend(program.iter().cloned());
    argv
}

/// Directories inside a writable root that contain a path the cell may not
/// write or read.
fn protected_ancestors(rules: &Rules) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for protected in rules.deny_write.iter().chain(&rules.deny_read) {
        for ancestor in protected.ancestors().skip(1) {
            if rules.writable.iter().any(|root| ancestor.starts_with(root) && ancestor != root) {
                out.push(ancestor.to_path_buf());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A path as an SBPL string literal.
fn quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        if matches!(character, '"' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
    out
}

/// Text matched literally inside an SBPL `#"…"` regex.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for character in text.chars() {
        if matches!(
            character,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '"'
        ) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

const PREAMBLE: &str = r#"(version 1)
(deny default (with message "mework-sandbox"))

; Processes: children inherit the sandbox; only the sandbox's own processes
; can be inspected, signalled or debugged.
(allow process-exec)
(allow process-fork)
(allow process-info* (target same-sandbox))
(allow signal (target same-sandbox))
(allow mach-priv-task-port (target same-sandbox))

(allow user-preference-read)

; Mach services, by name. Not the keychain's (com.apple.SecurityServer,
; com.apple.securityd.xpc): nothing in the sandbox may read a secret from it.
; trustd answers certificate questions only, which TLS needs.
(allow mach-lookup
  (global-name "com.apple.audio.systemsoundserver")
  (global-name "com.apple.distributed_notifications@Uv3")
  (global-name "com.apple.FontObjectsServer")
  (global-name "com.apple.fonts")
  (global-name "com.apple.logd")
  (global-name "com.apple.lsd.mapdb")
  (global-name "com.apple.PowerManagement.control")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.coreservices.launchservicesd")
  (global-name "com.apple.trustd")
  (global-name "com.apple.trustd.agent")
)

(allow ipc-posix-shm)
(allow ipc-posix-sem)

(allow iokit-open
  (iokit-registry-entry-class "IOSurfaceRootUserClient")
  (iokit-registry-entry-class "RootDomainUserClient")
  (iokit-user-client-class "IOSurfaceSendRight")
)
(allow iokit-get-properties)

(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))

(allow sysctl-read
  (sysctl-name "hw.activecpu")
  (sysctl-name "hw.busfrequency_compat")
  (sysctl-name "hw.byteorder")
  (sysctl-name "hw.cacheconfig")
  (sysctl-name "hw.cachelinesize_compat")
  (sysctl-name "hw.cpufamily")
  (sysctl-name "hw.cpufrequency")
  (sysctl-name "hw.cpufrequency_compat")
  (sysctl-name "hw.cputype")
  (sysctl-name "hw.l1dcachesize_compat")
  (sysctl-name "hw.l1icachesize_compat")
  (sysctl-name "hw.l2cachesize_compat")
  (sysctl-name "hw.l3cachesize_compat")
  (sysctl-name "hw.logicalcpu")
  (sysctl-name "hw.logicalcpu_max")
  (sysctl-name "hw.machine")
  (sysctl-name "hw.memsize")
  (sysctl-name "hw.model")
  (sysctl-name "hw.ncpu")
  (sysctl-name "hw.nperflevels")
  (sysctl-name "hw.packages")
  (sysctl-name "hw.pagesize_compat")
  (sysctl-name "hw.pagesize")
  (sysctl-name "hw.physicalcpu")
  (sysctl-name "hw.physicalcpu_max")
  (sysctl-name "hw.tbfrequency_compat")
  (sysctl-name "hw.vectorunit")
  (sysctl-name "kern.argmax")
  (sysctl-name "kern.bootargs")
  (sysctl-name "kern.hostname")
  (sysctl-name "kern.maxfiles")
  (sysctl-name "kern.maxfilesperproc")
  (sysctl-name "kern.maxproc")
  (sysctl-name "kern.ngroups")
  (sysctl-name "kern.osproductversion")
  (sysctl-name "kern.osrelease")
  (sysctl-name "kern.ostype")
  (sysctl-name "kern.osvariant_status")
  (sysctl-name "kern.osversion")
  (sysctl-name "kern.secure_kernel")
  (sysctl-name "kern.tcsm_available")
  (sysctl-name "kern.tcsm_enable")
  (sysctl-name "kern.usrstack64")
  (sysctl-name "kern.version")
  (sysctl-name "kern.willshutdown")
  (sysctl-name "machdep.cpu.brand_string")
  (sysctl-name "machdep.ptrauth_enabled")
  (sysctl-name "security.mac.lockdown_mode_state")
  (sysctl-name "sysctl.proc_cputype")
  (sysctl-name "vm.loadavg")
  (sysctl-name-prefix "hw.optional.arm")
  (sysctl-name-prefix "hw.optional.arm.")
  (sysctl-name-prefix "hw.optional.armv8_")
  (sysctl-name-prefix "hw.perflevel")
  (sysctl-name-prefix "kern.proc.all")
  (sysctl-name-prefix "kern.proc.pgrp.")
  (sysctl-name-prefix "kern.proc.pid.")
  (sysctl-name-prefix "machdep.cpu.")
  (sysctl-name-prefix "net.routetable.")
)
(allow sysctl-write (sysctl-name "kern.tcsm_enable"))

(allow distributed-notification-post)

(allow file-ioctl
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/dtracehelper")
  (literal "/dev/tty")
)
"#;

/// `/dev`, closed but for what every process needs. Pseudo terminals are not
/// among them: macOS numbers them in one namespace for the whole account, and
/// a sandbox allowed to open one it created could open the user's own
/// terminals just as well. A cell's commands run on pipes; programs that need
/// a terminal of their own (`script`, `expect`) do not run here.
const DEVICES: &str = r#"(deny file-read* file-write* (subpath "/dev"))
(allow file-read*
  (literal "/dev")
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/dtracehelper")
  (literal "/dev/tty")
  (literal "/dev/stdin")
  (literal "/dev/stdout")
  (literal "/dev/stderr")
  (subpath "/dev/fd")
)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules {
            writable: vec![PathBuf::from("/Users/ada/project"), PathBuf::from("/private/tmp/cell")],
            deny_read: vec![PathBuf::from("/Users/ada/.ssh")],
            readable: vec![PathBuf::from("/Users/ada/.mework/skills")],
            deny_write: vec![PathBuf::from("/Users/ada/.bashrc"), PathBuf::from("/Users/ada/project/bin")],
            protected_names: super::super::rules::PROTECTED_NAMES.to_vec(),
            bare_roots: Vec::new(),
        }
    }

    #[test]
    fn the_profile_confines_writes_reads_and_the_network() {
        let text = profile(&rules(), ProxyPort(41234));
        assert!(text.starts_with("(version 1)\n(deny default"));
        assert!(text.contains("(allow network-outbound (remote ip \"localhost:41234\"))"));
        assert!(!text.contains("(allow network*)"));
        assert!(!text.contains("com.apple.SecurityServer\")"));
        assert!(text.contains("(subpath \"/Users/ada/.ssh\")"));
        assert!(text.contains("(subpath \"/Users/ada/project\")"));
        assert!(text.contains(r#"(regex #"^/Users/ada/project/(.*/)?\.mework(/.*)?$")"#));
        assert!(text.contains(r#"(regex #"^/Users/ada/project/(.*/)?\.git$")"#));
        // The directory holding a protected path cannot be renamed away.
        assert!(text.contains("(literal \"/Users/ada/project\")") == false);
        assert!(text.contains("(subpath \"/Users/ada/project/bin\")"));
        // Parentheses balance, or sandbox-exec refuses the profile.
        let open = text.matches('(').count() - text.matches("\\(").count();
        let close = text.matches(')').count() - text.matches("\\)").count();
        assert_eq!(open, close);
    }

    #[test]
    fn quoting_and_regex_escaping_keep_paths_literal() {
        assert_eq!(quote(Path::new("/a \"b\"\\c")), r#""/a \"b\"\\c""#);
        assert_eq!(regex_escape("/a.b(c)+"), r"/a\.b\(c\)\+");
    }

    /// The profile is accepted by the real `sandbox-exec`, and does what it
    /// says: the workspace is writable, a protected file in it and everything
    /// outside it are not, credentials cannot be read, and nothing but the
    /// proxy port can be reached.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_enforces_the_profile() {
        let base = tempfile::tempdir().unwrap();
        let base_path = super::super::rules::canonical(base.path());
        let workspace = base_path.join("ws");
        let secret = base_path.join("secret");
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("key"), "hunter2").unwrap();
        std::fs::write(workspace.join(".git/config"), "[core]\n").unwrap();
        let rules = Rules {
            writable: vec![workspace.clone()],
            deny_read: vec![secret.clone()],
            readable: Vec::new(),
            deny_write: Vec::new(),
            protected_names: super::super::rules::PROTECTED_NAMES.to_vec(),
            bare_roots: Vec::new(),
        };
        let script = format!(
            "cd '{ws}' && echo ok > file && echo wrote; \
             echo x >> .git/config 2>/dev/null || echo config-protected; \
             echo x > .git/hooks/pre-commit 2>/dev/null || echo hooks-protected; \
             mkdir -p sub/.mework 2>/dev/null || echo mework-protected; \
             echo x > HEAD 2>/dev/null || echo head-protected; \
             echo x > '{base}/outside' 2>/dev/null || echo outside-protected; \
             cat '{secret}/key' 2>/dev/null || echo secret-protected; \
             /usr/bin/nc -z -G 2 1.1.1.1 443 2>/dev/null || echo network-blocked; \
             for t in /dev/ttys*; do head -c0 \"$t\" 2>/dev/null && echo TTY-OPEN; done; echo ttys-closed; \
             echo x > /dev/null && echo devnull-ok",
            ws = workspace.display(),
            base = base_path.display(),
            secret = secret.display()
        );
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-p")
            .arg(profile(&rules, ProxyPort(1)))
            .arg("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for expected in [
            "wrote",
            "config-protected",
            "hooks-protected",
            "mework-protected",
            "head-protected",
            "outside-protected",
            "secret-protected",
            "network-blocked",
            "ttys-closed",
            "devnull-ok",
        ] {
            assert!(stdout.contains(expected), "{expected} missing; stdout: {stdout} stderr: {stderr}");
        }
        assert!(!stdout.contains("TTY-OPEN"), "a terminal device was readable: {stdout}");
        assert_eq!(std::fs::read_to_string(workspace.join("file")).unwrap(), "ok\n");
        assert_eq!(std::fs::read_to_string(workspace.join(".git/config")).unwrap(), "[core]\n");
    }
}
