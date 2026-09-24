//! Speaking through a remote login shell whose dialect is not known in advance.
//!
//! OpenSSH never runs a command directly. sshd hands the command string to the
//! account's login shell — `$SHELL -c` on a Unix machine, the registry's
//! `DefaultShell` on Windows — and that may be bash, zsh, dash, ksh, fish, tcsh,
//! nushell, `cmd.exe` or PowerShell. The host cannot choose it and should not
//! have to know it, so every remote leg gets past it as early as possible:
//!
//! - On a Unix-like machine the whole POSIX script travels inside one fixed
//!   line, [`posix_line`], whose payload every one of those shells reads as the
//!   same literal text. `/bin/sh` is the only program it assumes.
//! - On a Windows machine whose login shell is `cmd.exe` or PowerShell, the
//!   script is PowerShell sent as `-EncodedCommand` base64 ([`powershell_line`]):
//!   letters, digits and `+/=`, which no shell rewrites.
//!
//! Which of the two a machine needs is asked with [`PROBE_COMMAND`], a line
//! every shell can run, and remembered per endpoint for the life of the app.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;

use crate::cancel::CancelSignal;
use crate::run_environment::{self, ShellRunner};

/// How a machine's login shell has to be addressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginShell {
    /// A Unix-like shell: POSIX sh and its relatives, fish, the csh family,
    /// nushell — anything that is not one of the two Windows shells. The
    /// POSIX line is what all of them are sent.
    Posix,
    /// Windows `cmd.exe`, the factory `DefaultShell`.
    Cmd,
    /// Windows PowerShell or PowerShell 7 as `DefaultShell`.
    PowerShell,
}

impl LoginShell {
    /// Whether this is one of the Windows shells, which reach a Windows
    /// filesystem and are sent PowerShell rather than POSIX sh.
    pub fn is_windows(self) -> bool {
        matches!(self, Self::Cmd | Self::PowerShell)
    }
}

/// The line that tells the dialects apart.
///
/// It is a plain command with three plain words, so every shell runs it
/// without complaint, and each family answers differently: `cmd.exe` expands
/// `%OS%` to `Windows_NT`; PowerShell's `echo` is `Write-Output`, which puts
/// each argument on a line of its own and leaves `%OS%` alone; a Unix shell
/// prints the words as they are. A POSIX shell under Git for Windows answers
/// like any other Unix shell, which is right — the POSIX line works there.
pub const PROBE_COMMAND: &str = "echo mework-probe %OS%";

/// How long the probe may take. It is one `echo` on a machine SSH already
/// reached, so this bounds a stalled session, not a slow command.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Reads the probe's stdout. Anything unrecognized — a nushell table, a
/// machine where `%OS%` is unset — is `Posix`: the POSIX line is the one most
/// shells run, and a caller that sees `cmd.exe` answer it can still try again
/// as Windows.
pub fn classify_probe(stdout: &str) -> LoginShell {
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();
    if lines.contains(&"mework-probe Windows_NT") {
        LoginShell::Cmd
    } else if lines.windows(2).any(|pair| pair == ["mework-probe", "%OS%"]) {
        LoginShell::PowerShell
    } else {
        LoginShell::Posix
    }
}

/// The login-shell-neutral form of a POSIX `sh` script.
///
/// The line is `exec /bin/sh -c 'eval "$(printf "PAYLOAD")"'`: the login shell
/// sees one single-quoted word it passes to `/bin/sh` unchanged, and `sh`
/// rebuilds the script with `printf` and evaluates it, so the script's own
/// syntax is only ever read by `sh`. `/bin/sh` is named by its path, as the
/// terminal leg always has, so a login shell with an odd `PATH` still finds it.
/// What makes the single quotes safe in every Unix shell is the payload's
/// alphabet — see [`printf_payload`].
pub fn posix_line(script: &str) -> String {
    format!(
        "exec /bin/sh -c 'eval \"$(printf \"{}\")\"'",
        printf_payload(script)
    )
}

/// Whether an ASCII byte can stand for itself in the payload.
///
/// Every other byte becomes a three-digit octal escape. `'` would end the
/// single quotes the login shell sees; `\` is an escape inside fish's single
/// quotes; `!` and a newline are history and end-of-command to csh even there;
/// `$`, `` ` `` and `"` are live inside the double quotes `sh` reads the format
/// through; `%` and `\` are `printf`'s own; control bytes belong on no command
/// line.
fn is_literal(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
        && !matches!(byte, b'\'' | b'\\' | b'!' | b'$' | b'`' | b'"' | b'%')
}

/// Spells `script` as a `printf` format that reproduces it byte for byte.
///
/// Non-ASCII text stays as itself where it can, which keeps a Chinese script
/// at its own size rather than four times it — the line is one argument to the
/// login shell, and Linux caps one argument at 128 KiB. The exception is a
/// character right before an escape: in a GBK, Big5 or Shift_JIS locale a
/// shell reading multibyte text would pair its last byte with the escape's
/// backslash and lose the escape, so such a character is escaped as well, and
/// that decision runs backwards so a run of characters ahead of an escape
/// follows it. A literal ASCII byte a lead byte may swallow is harmless: none
/// of them means anything to the login shell, to `sh`'s double quotes or to
/// `printf`, so it arrives unchanged either way.
fn printf_payload(script: &str) -> String {
    let mut pieces: Vec<(char, bool)> = Vec::with_capacity(script.len());
    let mut next_is_escape = false;
    for c in script.chars().rev() {
        let literal = if c.is_ascii() {
            is_literal(c as u8)
        } else {
            !next_is_escape
        };
        pieces.push((c, literal));
        next_is_escape = !literal;
    }
    let mut out = String::with_capacity(script.len() + script.len() / 4);
    let mut buffer = [0u8; 4];
    for (c, literal) in pieces.into_iter().rev() {
        if literal {
            out.push(c);
        } else {
            for byte in c.encode_utf8(&mut buffer).bytes() {
                let _ = write!(out, "\\{byte:03o}");
            }
        }
    }
    out
}

/// The line that runs a PowerShell script on a Windows machine, whichever of
/// `cmd.exe` and PowerShell its sshd hands the line to.
///
/// `-EncodedCommand` carries the script as base64 of its UTF-16LE text, so the
/// login shell sees nothing it would expand or split. Windows PowerShell 5.1 is
/// asked for by name because it is on every Windows since 7, whether or not
/// PowerShell 7 is the login shell. `cmd.exe` caps a command line at 8191
/// characters; the callers here send scripts a small fraction of that.
pub fn powershell_line(script: &str) -> String {
    powershell_argv(script).join(" ")
}

/// [`powershell_line`] as the program and arguments it names, for a caller
/// that starts PowerShell itself — the agent on a Windows machine — rather
/// than through a login shell.
pub fn powershell_argv(script: &str) -> Vec<String> {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut argv: Vec<String> = [
        "powershell",
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-OutputFormat",
        "Text",
        "-EncodedCommand",
    ]
    .iter()
    .map(|part| (*part).to_owned())
    .collect();
    argv.push(base64::engine::general_purpose::STANDARD.encode(utf16));
    argv
}

/// A PowerShell single-quoted string literal of `text`.
///
/// PowerShell accepts four typographic single quotes as delimiters too, so all
/// five are doubled, not just the ASCII one; nothing else is live between
/// single quotes.
pub fn ps_single_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// What each endpoint's login shell turned out to be, keyed by
/// [`endpoint_key`]. A machine's shell changes only when someone reconfigures
/// it, so the answer is kept until a call made with it fails.
fn cache() -> &'static Mutex<HashMap<String, LoginShell>> {
    static CACHE: OnceLock<Mutex<HashMap<String, LoginShell>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Everything that decides which account on which machine a call logs in to.
fn endpoint_key(runner: &ShellRunner) -> Option<String> {
    match runner {
        ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } => Some(format!("{host}\u{0}{port}\u{0}{identity_file}")),
        ShellRunner::Local { .. } | ShellRunner::Wsl { .. } => None,
    }
}

/// The login shell of the machine `runner` reaches, and whether the answer
/// came from the cache rather than from asking just now.
///
/// A WSL distribution is always `Posix`: `wsl.exe --exec` starts the program
/// itself and no login shell sits in between. An SSH machine is asked once;
/// a probe that cannot reach it fails with SSH's own words.
pub fn login_shell(runner: &ShellRunner) -> Result<(LoginShell, bool), String> {
    let Some(key) = endpoint_key(runner) else {
        return Ok((LoginShell::Posix, false));
    };
    if let Some(shell) = lock_cache().get(&key).copied() {
        return Ok((shell, true));
    }
    let shell = probe(runner)?;
    lock_cache().insert(key, shell);
    Ok((shell, false))
}

/// Forgets the endpoint's login shell, so the next call asks again. Called when
/// a call made with a remembered answer fails: the machine may have been
/// reconfigured since.
pub fn forget_login_shell(runner: &ShellRunner) {
    if let Some(key) = endpoint_key(runner) {
        lock_cache().remove(&key);
    }
}

/// Records what a call just proved about the endpoint's login shell.
pub fn remember_login_shell(runner: &ShellRunner, shell: LoginShell) {
    if let Some(key) = endpoint_key(runner) {
        lock_cache().insert(key, shell);
    }
}

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, LoginShell>> {
    cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs [`PROBE_COMMAND`] on the machine. Exit 255 is OpenSSH's own: the
/// machine was never reached, and its message is the answer.
fn probe(runner: &ShellRunner) -> Result<LoginShell, String> {
    let output = run_environment::run_ssh_line(
        runner,
        PROBE_COMMAND,
        PROBE_TIMEOUT,
        &CancelSignal::default(),
    )?;
    if output.status == Some(255) {
        let detail = output.stderr.trim();
        return Err(if detail.is_empty() {
            "SSH 没能连上这台机器（退出码 255）".to_owned()
        } else {
            detail.to_owned()
        });
    }
    Ok(classify_probe(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Reverses [`printf_payload`] the way `printf` does, so a test can say
    /// which script a line carries.
    pub(crate) fn decode_posix_line(line: &str) -> String {
        let payload = line
            .strip_prefix("exec /bin/sh -c 'eval \"$(printf \"")
            .and_then(|rest| rest.strip_suffix("\")\"'"))
            .unwrap_or_else(|| panic!("not a POSIX line: {line}"));
        let bytes = payload.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                let digits = std::str::from_utf8(&bytes[index + 1..index + 4]).unwrap();
                out.push(u8::from_str_radix(digits, 8).unwrap());
                index += 4;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_probe_tells_the_three_families_apart() {
        assert_eq!(
            classify_probe("mework-probe Windows_NT\r\n"),
            LoginShell::Cmd
        );
        assert_eq!(
            classify_probe("mework-probe\r\n%OS%\r\n"),
            LoginShell::PowerShell
        );
        assert_eq!(classify_probe("mework-probe %OS%\n"), LoginShell::Posix);
        // A login script that prints on a non-interactive login does not hide
        // the answer.
        assert_eq!(
            classify_probe("Welcome to devbox\nmework-probe Windows_NT\r\n"),
            LoginShell::Cmd
        );
        // Anything unrecognized is sent the POSIX line.
        assert_eq!(classify_probe(""), LoginShell::Posix);
        assert_eq!(
            classify_probe("╭───┬──────────────╮\n│ 0 │ mework-probe │\n│ 1 │ %OS%         │\n"),
            LoginShell::Posix
        );
        assert!(LoginShell::Cmd.is_windows());
        assert!(LoginShell::PowerShell.is_windows());
        assert!(!LoginShell::Posix.is_windows());
    }

    #[test]
    fn the_posix_line_carries_any_script_unchanged() {
        let scripts = [
            "pwd",
            "cd -- '/srv/it'\\''s here' && pwd",
            "printf '%s\\n' \"$HOME\" `date` !! 100% \\\\",
            "cat <<'EOF'\nline one\n\tline two\r\nEOF\n",
            "echo 中文路径 ✓ émoji 🎉 \"引号\"",
            "",
        ];
        for script in scripts {
            let line = posix_line(script);
            assert_eq!(decode_posix_line(&line), script, "{line}");
        }
    }

    /// What sshd does with the line — `$SHELL -c LINE` — done here with every
    /// login shell this machine has. Each must hand `sh` the same script, so
    /// each prints the same bytes and exits with the script's own status.
    #[cfg(unix)]
    #[test]
    fn every_login_shell_on_this_machine_runs_the_line_as_sh_would() {
        use std::process::{Command, Stdio};
        let script = concat!(
            "printf '%s|' \"it's\" 'say \"hi\"' 'bang!' '$HOME' 'back\\slash' '100%' '中文 ✓'\n",
            "cat <<'EOF'\n",
            "  kept $AS `IS` !\n",
            "EOF\n",
            "exit 7\n",
        );
        let expected = "it's|say \"hi\"|bang!|$HOME|back\\slash|100%|中文 ✓|  kept $AS `IS` !\n";
        let line = posix_line(script);
        let mut ran = Vec::new();
        for shell in [
            "/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash", "/bin/ksh", "/bin/csh",
            "/bin/tcsh", "/usr/bin/fish", "/opt/homebrew/bin/fish", "/usr/local/bin/fish",
        ] {
            if !std::path::Path::new(shell).is_file() {
                continue;
            }
            let output = Command::new(shell)
                .arg("-c")
                .arg(&line)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                expected,
                "{shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.status.code(), Some(7), "{shell}");
            ran.push(shell);
        }
        assert!(ran.contains(&"/bin/sh"), "{ran:?}");
    }

    /// The whole point of the payload: nothing in it means anything to the
    /// login shell inside single quotes, to `sh` inside double quotes, or to
    /// `printf` except the escapes themselves.
    #[test]
    fn the_payload_alphabet_is_inert_in_every_shell() {
        let line = posix_line("echo 'a' \"b\" $c `d` !e %f \\g\nh\ti\r中\"文");
        let payload = &line["exec /bin/sh -c 'eval \"$(printf \"".len()..line.len() - "\")\"'".len()];
        for (index, byte) in payload.bytes().enumerate() {
            assert!(
                !matches!(byte, b'\'' | b'!' | b'$' | b'`' | b'"' | b'%' | b'\n' | b'\r' | b'\t'),
                "byte {byte:#x} at {index} in {payload}"
            );
            if byte == b'\\' {
                assert!(
                    payload.as_bytes()[index + 1..index + 4]
                        .iter()
                        .all(|digit| (b'0'..=b'7').contains(digit)),
                    "{payload}"
                );
            }
        }
    }

    /// A non-ASCII character is kept as itself except right before an escape,
    /// where a GBK-reading shell would pair its last byte with the backslash.
    #[test]
    fn a_multibyte_character_never_sits_right_before_an_escape() {
        assert_eq!(printf_payload("中文 ok"), "中文 ok");
        // `文` is followed by an escaped `"`, and `中` by the escaped `文`.
        assert_eq!(
            printf_payload("中文\""),
            "\\344\\270\\255\\346\\226\\207\\042"
        );
        assert_eq!(printf_payload("中 文\""), "中 \\346\\226\\207\\042");
        let payload = printf_payload("a中\nb文'c");
        for (index, _) in payload.match_indices('\\') {
            let before = payload[..index].chars().next_back();
            assert!(before.is_none_or(|c| c.is_ascii()), "{payload}");
        }
    }

    #[test]
    fn a_powershell_line_is_base64_of_the_utf16_script() {
        let script = "Write-Output '中文'";
        let line = powershell_line(script);
        let encoded = line
            .strip_prefix(
                "powershell -NoLogo -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand ",
            )
            .unwrap();
        assert!(encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), script);
    }

    #[test]
    fn a_powershell_literal_doubles_every_single_quote_it_accepts() {
        assert_eq!(ps_single_quote("C:/Users/dev"), "'C:/Users/dev'");
        assert_eq!(ps_single_quote("it's"), "'it''s'");
        assert_eq!(ps_single_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(ps_single_quote("$env:HOME `n"), "'$env:HOME `n'");
    }

    #[test]
    fn only_ssh_endpoints_are_probed_and_remembered() {
        assert_eq!(
            login_shell(&ShellRunner::Wsl {
                agent_shell: Default::default(),
                distro: "Ubuntu".into(),
                env: Default::default(),
            }),
            Ok((LoginShell::Posix, false))
        );
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: "remembered@probe.invalid".into(),
            port: 2201,
            identity_file: String::new(),
            env: Default::default(),
        };
        remember_login_shell(&runner, LoginShell::Cmd);
        assert_eq!(login_shell(&runner), Ok((LoginShell::Cmd, true)));
        forget_login_shell(&runner);
        assert!(lock_cache().get(&endpoint_key(&runner).unwrap()).is_none());
    }
}
