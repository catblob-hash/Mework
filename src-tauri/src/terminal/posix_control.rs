//! The zsh, bash and fish half of the host terminal's command barrier.
//!
//! On Windows the barrier is a PSReadLine hook and two named events
//! ([`super::configure_powershell_control`]). This is the same protocol for the
//! shells of a Mac or Linux host: the same authenticated `ready`/`start`/`end`
//! frames on the terminal output, with the host's answer to `start` travelling
//! back through a FIFO instead of an event. The shell's line editor emits
//! `start` when a line is accepted and does not run it until it reads
//! `a <generation>` (the command lease is held) or `r <generation>` (a
//! workspace writer is active, so the line is refused).
//!
//! Everything lives in one private directory per session, created mode `0700`
//! under the user's temporary directory and removed when the session is dropped:
//! the FIFO, and the startup files that install the hooks. zsh reads its
//! startup files from `$ZDOTDIR`, so pointing that at the session directory is
//! how the hooks get in without touching the user's own files; each generated
//! file sources the user's counterpart first, from where zsh would have found
//! it, and then hands `ZDOTDIR` back. bash is given the session's rcfile with
//! `--rcfile` ([`super::bash_control`]), and fish sources the session's script
//! through `--init-command`, after its own configuration.

use crate::host_platform::host_platform;
use std::{
    ffi::{CString, OsString},
    fs,
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use uuid::Uuid;

use super::{
    bash_control::{self, Decision},
    DECISION_TIMEOUT_SECONDS, REFUSED_MESSAGE,
};

/// The shells a host terminal off Windows can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PosixShell {
    Zsh,
    Bash,
    Fish,
}

/// The bash rcfile and fish script inside the session directory.
const BASH_RCFILE: &str = "mework.bash";
const FISH_SCRIPT: &str = "mework.fish";

pub(super) struct ControlFifo {
    shell: PosixShell,
    /// Handed to zsh and bash in the environment; fish has it written into its
    /// script instead (see [`fish_script`]).
    nonce: String,
    directory: PathBuf,
    path: PathBuf,
    /// Opened read-write, which is what makes the FIFO usable from one side:
    /// the open neither blocks waiting for the shell nor fails for want of a
    /// reader, and the pipe outlives each of the shell's one-line reads.
    writer: fs::File,
}

impl ControlFifo {
    pub(super) fn create(shell: PosixShell, nonce: &str) -> Result<Self, String> {
        let directory =
            std::env::temp_dir().join(format!("mework-terminal-{}", Uuid::new_v4().simple()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|error| format!("无法创建终端控制目录：{error}"))?;
        let built = (|| {
            let path = directory.join("control");
            match shell {
                PosixShell::Zsh => write_zsh_startup_files(&directory)?,
                PosixShell::Bash => write_private_file(
                    &directory,
                    BASH_RCFILE,
                    &bash_control::bashrc(Decision::Fifo),
                )?,
                PosixShell::Fish => {
                    write_private_file(&directory, FISH_SCRIPT, &fish_script(nonce, &path)?)?
                }
            }
            make_fifo(&path)?;
            let writer = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .map_err(|error| format!("无法打开终端控制通道：{error}"))?;
            Ok((path, writer))
        })();
        match built {
            Ok((path, writer)) => Ok(Self {
                shell,
                nonce: nonce.to_owned(),
                directory,
                path,
                writer,
            }),
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                Err(error)
            }
        }
    }

    /// The variables the shell's startup reads, and removes, before any user
    /// file runs.
    pub(super) fn environment(&self) -> Vec<(&'static str, OsString)> {
        match self.shell {
            PosixShell::Zsh => self.zsh_environment(),
            PosixShell::Bash => {
                let mut environment = self.channel_environment();
                // Apple's bash 3.2 opens every interactive shell with a notice
                // that zsh is now the default. Someone who picked bash from the
                // menu has already made that choice.
                if host_platform().is_macos() {
                    environment.push(("BASH_SILENCE_DEPRECATION_WARNING", OsString::from("1")));
                }
                environment
            }
            PosixShell::Fish => Vec::new(),
        }
    }

    fn channel_environment(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("MEWORK_TERMINAL_CONTROL_NONCE", OsString::from(&self.nonce)),
            (
                "MEWORK_TERMINAL_CONTROL_CHANNEL",
                self.path.clone().into_os_string(),
            ),
        ]
    }

    /// The variables that point a login zsh at this session's startup files.
    ///
    /// A `ZDOTDIR` the app itself inherited is the user's choice of where their
    /// files live, so it travels on for the generated files to honour.
    fn zsh_environment(&self) -> Vec<(&'static str, OsString)> {
        let mut environment = self.channel_environment();
        environment.push(("ZDOTDIR", self.directory.clone().into_os_string()));
        if let Some(user) = std::env::var_os("ZDOTDIR").filter(|value| !value.is_empty()) {
            environment.push(("MEWORK_USER_ZDOTDIR", user));
        }
        environment
    }

    /// What goes before the shell's own arguments. bash takes long options only
    /// ahead of short ones.
    pub(super) fn leading_args(&self) -> Vec<OsString> {
        match self.shell {
            PosixShell::Zsh => Vec::new(),
            PosixShell::Bash => vec![
                OsString::from("--rcfile"),
                self.directory.join(BASH_RCFILE).into_os_string(),
            ],
            // The script's directory is text: `fish_script` refused it otherwise.
            PosixShell::Fish => vec![
                OsString::from("--init-command"),
                OsString::from(format!(
                    "builtin source {}",
                    fish_quote(&self.directory.join(FISH_SCRIPT).to_string_lossy())
                )),
            ],
        }
    }

    /// Answers the `start` frame of `generation`. The generation is part of the
    /// line so a shell that gave up waiting can never take a late answer for the
    /// next command as its own.
    pub(super) fn send(&self, accepted: bool, generation: u64) -> Result<(), String> {
        let line = format!("{} {generation}\n", if accepted { "a" } else { "r" });
        (&self.writer)
            .write_all(line.as_bytes())
            .map_err(|error| format!("无法写入终端控制通道：{error}"))
    }
}

impl Drop for ControlFifo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn make_fifo(path: &Path) -> Result<(), String> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "终端控制通道路径包含 NUL".to_owned())?;
    // SAFETY: `name` is a NUL-terminated path that outlives the call.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(format!(
            "无法创建终端控制通道：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn write_zsh_startup_files(directory: &Path) -> Result<(), String> {
    for (name, body) in [
        (".zshenv", zshenv()),
        (".zprofile", source_user_file(".zprofile")),
        (".zshrc", zshrc()),
    ] {
        write_private_file(directory, name, &body)?;
    }
    Ok(())
}

fn write_private_file(directory: &Path, name: &str, body: &str) -> Result<(), String> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(name))
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .map_err(|error| format!("无法写入终端启动文件 {name}：{error}"))
}

/// Sources one of the user's startup files from where zsh would have read it.
///
/// Inline rather than a function: a file sourced inside a function turns every
/// bare `typeset` in it — `typeset -U path` is in half the dotfiles there are —
/// into a local that vanishes when the function returns. `ZDOTDIR` is the
/// user's while their file runs, because their file may read it, and a file
/// that moves it (a `.zshenv` that sets it, typically) moves where the next one
/// is looked for.
fn source_user_file(name: &str) -> String {
    format!(
        r#"if [[ -f "$__mework_user_zdotdir/{name}" ]]; then
  ZDOTDIR="$__mework_user_zdotdir"
  builtin source "$__mework_user_zdotdir/{name}"
  __mework_user_zdotdir="${{ZDOTDIR:-$HOME}}"
  ZDOTDIR="$__mework_integration_dir"
fi
"#
    )
}

/// The first file zsh reads. The control secrets are taken into shell variables
/// and removed from the environment here, before any user file can start a
/// process that would inherit them.
fn zshenv() -> String {
    format!(
        r#"# Mework terminal integration, generated for one session.
__mework_nonce="${{MEWORK_TERMINAL_CONTROL_NONCE-}}"
__mework_channel="${{MEWORK_TERMINAL_CONTROL_CHANNEL-}}"
__mework_integration_dir="$ZDOTDIR"
__mework_user_zdotdir="${{MEWORK_USER_ZDOTDIR:-$HOME}}"
unset MEWORK_TERMINAL_CONTROL_NONCE MEWORK_TERMINAL_CONTROL_CHANNEL MEWORK_USER_ZDOTDIR
{}"#,
        source_user_file(".zshenv")
    )
}

/// The user's `.zshrc`, then the hooks. Installing them last means they wrap
/// whatever line editor the user's configuration built, instead of being
/// replaced by it.
fn zshrc() -> String {
    format!(
        r#"# macOS's /etc/zshrc names the history file after ZDOTDIR, which is this
# session's scratch directory until the user's files have run.
if [[ "${{HISTFILE-}}" == "$__mework_integration_dir"/* ]]; then
  HISTFILE="$__mework_user_zdotdir/.zsh_history"
fi
{source_zshrc}
# Hand ZDOTDIR back: .zlogin and every nested zsh read the user's own files.
if [[ "$__mework_user_zdotdir" == "$HOME" ]]; then
  unset ZDOTDIR
else
  export ZDOTDIR="$__mework_user_zdotdir"
fi
unset __mework_integration_dir __mework_user_zdotdir

typeset -gi __mework_generation=0 __mework_active=0 __mework_pending=0

__mework_control() {{
  emulate -L zsh
  builtin printf '\033]633;Mework;v1;%s;%s;%s\007' "$__mework_nonce" "$1" "$2"
}}

# A `start` whose answer was never read — Ctrl-C cut the wait short — may still
# have been granted, and the host holds that lease until it hears `end`.
__mework_close_pending() {{
  emulate -L zsh
  if (( __mework_pending )); then
    __mework_control end "$__mework_pending"
    __mework_pending=0
  fi
}}

# Every prompt closes the command the last accepted line started, or says the
# shell is idle. The host ignores all but the first `ready`.
__mework_precmd() {{
  emulate -L zsh
  __mework_close_pending
  if (( __mework_active )); then
    __mework_control end "$__mework_active"
    __mework_active=0
  else
    __mework_control ready 0
  fi
}}

__mework_await_decision() {{
  emulate -L zsh
  local reply
  while IFS= builtin read -r -t {timeout} reply < "$__mework_channel"; do
    case "$reply" in
      ("a $1") return 0 ;;
      ("r $1") return 1 ;;
    esac
  done
  return 2
}}

# A line that runs something is announced and held until the host answers.
# Continuation lines of a line already started pass straight through, and a
# refused line stays in the buffer so pressing Enter again retries it.
__mework_accept() {{
  emulate -L zsh
  if (( __mework_active == 0 )) && [[ -n "${{BUFFER//[[:space:]]/}}" ]]; then
    local decision=0
    __mework_close_pending
    (( ++__mework_generation ))
    __mework_pending=$__mework_generation
    __mework_control start "$__mework_generation"
    __mework_await_decision "$__mework_generation" || decision=$?
    __mework_pending=0
    if (( decision == 0 )); then
      __mework_active=$__mework_generation
    else
      # No answer at all: whatever the host decided, this line will not run.
      if (( decision != 1 )); then
        __mework_control end "$__mework_generation"
      fi
      zle -I
      builtin print -ru2 -- "{refused}"
      return 0
    fi
  fi
  zle "__mework_original_$WIDGET" -- "$@"
}}

() {{
  emulate -L zsh
  local widget
  for widget in accept-line accept-and-hold accept-line-and-down-history accept-and-infer-next-history; do
    zle -A "$widget" "__mework_original_$widget"
    zle -N "$widget" __mework_accept
  done
}}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __mework_precmd
"#,
        source_zshrc = source_user_file(".zshrc"),
        timeout = DECISION_TIMEOUT_SECONDS,
        refused = REFUSED_MESSAGE,
    )
}

/// A fish single-quoted word. Inside one only `\` and `'` are special.
fn fish_quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The fish half, sourced by `--init-command` after the user's configuration.
///
/// The nonce and the FIFO's path are written into the script rather than
/// passed in the environment: fish reads its configuration before
/// `--init-command` runs, and anything that configuration started would
/// otherwise have inherited them.
///
/// Enter is bound in the default and vi insert modes to a function that, for a
/// line that runs something, emits `start` and reads the FIFO before letting
/// `execute` have the line. fish's `read` has no timeout, and the wait does
/// without one: the host answers every `start` it reads while it lives, and
/// once it is gone the FIFO has no writer left, so `read` ends. fish sets up
/// its key bindings at the first prompt and again whenever the bindings in use
/// change, each time finishing with `fish_user_key_bindings`; wrapping that
/// function keeps Enter bound on top of whatever the user's own bindings do.
fn fish_script(nonce: &str, channel: &Path) -> Result<String, String> {
    let channel = channel
        .to_str()
        .ok_or_else(|| format!("终端控制通道路径无法表示为文本：{}", channel.display()))?;
    Ok(format!(
        r#"# Mework terminal integration, generated for one session.
set -g __mework_nonce {nonce}
set -g __mework_channel {channel}
set -g __mework_generation 0
set -g __mework_active 0
set -g __mework_pending 0

function __mework_control
    builtin printf '%s' \e"]633;Mework;v1;$__mework_nonce;$argv[1];$argv[2]"\a
end

# Every prompt closes the command the last accepted line started, or says the
# shell is idle. The host ignores all but the first `ready`.
function __mework_prompt --on-event fish_prompt
    if test $__mework_pending -ne 0
        __mework_control end $__mework_pending
        set -g __mework_pending 0
    end
    if test $__mework_active -ne 0
        __mework_control end $__mework_active
        set -g __mework_active 0
    else
        __mework_control ready 0
    end
end

function __mework_await_decision
    while read -l reply <$__mework_channel
        switch $reply
            case "a $argv[1]"
                return 0
            case "r $argv[1]"
                return 1
        end
    end
    return 2
end

# Announces generation N+1 and waits for the host. A wait that was cut short
# may still have been granted, so its generation is closed first.
function __mework_ask
    if test $__mework_pending -ne 0
        __mework_control end $__mework_pending
        set -g __mework_pending 0
    end
    set -g __mework_generation (math $__mework_generation + 1)
    set -g __mework_pending $__mework_generation
    __mework_control start $__mework_generation
    __mework_await_decision $__mework_generation
    set -l decision $status
    set -g __mework_pending 0
    if test $decision -eq 0
        set -g __mework_active $__mework_generation
        return 0
    end
    if test $decision -ne 1
        # No answer: whatever the host decided, this line is not going to run.
        __mework_control end $__mework_generation
    end
    return 1
end

# What Enter looks at and what it does with the line, apart so tests can stand
# in for the line editor.
function __mework_line_runs_something
    commandline | string match -qr '\S'
end

function __mework_run_line
    commandline -f execute
end

function __mework_hold_line
    if functions -q __fish_echo
        __fish_echo builtin printf '%s\n' '{refused}'
    else
        builtin printf '\n%s\n' '{refused}' >&2
        commandline -f repaint
    end
end

# Enter. A line that runs something is announced and held until the host
# answers. While a command is active — its line was incomplete, so Enter only
# added a newline to it — Enter passes straight through, and a refused line
# stays on the command line so pressing Enter again retries it.
function __mework_execute
    if test $__mework_active -eq 0; and __mework_line_runs_something
        if not __mework_ask
            __mework_hold_line
            return 0
        end
    end
    __mework_run_line
end

# fish 4 names keys; fish 3 binds the bytes a key sends.
function __mework_bind_keys
    set -l keys \r \n
    set -l major (string split -m 1 . -- $version)[1]
    if string match -qr '^[0-9]+$' -- $major; and test $major -ge 4
        set keys enter ctrl-j
    end
    for mode in default insert
        for key in $keys
            bind -M $mode $key __mework_execute
        end
    end
end

if functions -q fish_user_key_bindings
    functions --copy fish_user_key_bindings __mework_user_key_bindings
end
function fish_user_key_bindings
    if functions -q __mework_user_key_bindings
        __mework_user_key_bindings
    end
    __mework_bind_keys
end
__mework_bind_keys
"#,
        nonce = fish_quote(nonce),
        channel = fish_quote(channel),
        refused = REFUSED_MESSAGE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_directory_is_private_and_removed_with_the_channel() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        for (shell, names) in [
            (PosixShell::Zsh, &[".zshenv", ".zprofile", ".zshrc"][..]),
            (PosixShell::Bash, &[BASH_RCFILE][..]),
            (PosixShell::Fish, &[FISH_SCRIPT][..]),
        ] {
            let fifo = ControlFifo::create(shell, "n0nce").unwrap();
            let directory = fifo.directory.clone();
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert!(fs::metadata(&fifo.path).unwrap().file_type().is_fifo());
            for name in names {
                let file = directory.join(name);
                assert!(file.is_file(), "{name}");
                assert_eq!(
                    fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            drop(fifo);
            assert!(!directory.exists());
        }
    }

    #[test]
    fn each_shell_is_pointed_at_its_own_startup_files() {
        let zsh = ControlFifo::create(PosixShell::Zsh, "n0nce").unwrap();
        assert!(zsh.leading_args().is_empty());
        let names = |fifo: &ControlFifo| {
            fifo.environment()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert!(names(&zsh).contains(&"ZDOTDIR"));
        assert!(names(&zsh).contains(&"MEWORK_TERMINAL_CONTROL_NONCE"));

        let bash = ControlFifo::create(PosixShell::Bash, "n0nce").unwrap();
        assert_eq!(
            bash.leading_args(),
            [
                OsString::from("--rcfile"),
                bash.directory.join(BASH_RCFILE).into_os_string()
            ]
        );
        assert!(names(&bash).contains(&"MEWORK_TERMINAL_CONTROL_CHANNEL"));
        assert!(!names(&bash).contains(&"ZDOTDIR"));

        // fish has its secrets written into its script, never the environment.
        let fish = ControlFifo::create(PosixShell::Fish, "n0nce").unwrap();
        assert!(fish.environment().is_empty());
        let args = fish.leading_args();
        assert_eq!(args[0], OsString::from("--init-command"));
        assert_eq!(
            args[1].to_string_lossy(),
            format!(
                "builtin source '{}'",
                fish.directory.join(FISH_SCRIPT).display()
            )
        );
    }

    #[test]
    fn fish_words_survive_quotes_and_backslashes() {
        assert_eq!(fish_quote("plain"), "'plain'");
        assert_eq!(fish_quote(r"it's a\b"), r"'it\'s a\\b'");
    }

    #[test]
    fn the_fish_script_holds_its_own_secrets_and_binds_enter_after_the_user() {
        let script = fish_script("n0nce", Path::new("/tmp/mework-terminal-x/control")).unwrap();
        assert!(script.starts_with("# Mework terminal integration"));
        assert!(script.contains("set -g __mework_nonce 'n0nce'"));
        assert!(script.contains("set -g __mework_channel '/tmp/mework-terminal-x/control'"));
        assert!(!script.contains("MEWORK_TERMINAL_CONTROL"));
        assert!(script.contains("function __mework_prompt --on-event fish_prompt"));
        assert!(script.contains("read -l reply <$__mework_channel"));
        assert!(script.contains(REFUSED_MESSAGE));
        // The user's own key bindings run first and Enter is bound after them,
        // at startup and at every later reload of the bindings.
        let copy = script
            .find("functions --copy fish_user_key_bindings __mework_user_key_bindings")
            .unwrap();
        let wrapper = script.find("function fish_user_key_bindings").unwrap();
        let wrapped = &script[wrapper..];
        assert!(copy < wrapper);
        assert!(
            wrapped.find("__mework_user_key_bindings").unwrap()
                < wrapped.find("__mework_bind_keys").unwrap()
        );
        assert!(script.trim_end().ends_with("__mework_bind_keys"));
        for mode in ["default", "insert"] {
            assert!(script.contains(mode), "{mode}");
        }
        assert!(script.contains(r"set -l keys \r \n"));
        assert!(script.contains("set keys enter ctrl-j"));
    }

    #[test]
    fn answers_name_their_generation_and_never_block() {
        let fifo = ControlFifo::create(PosixShell::Zsh, "n0nce").unwrap();
        fifo.send(true, 3).unwrap();
        fifo.send(false, 4).unwrap();
        let mut reader = fs::File::open(&fifo.path).unwrap();
        let mut buffer = [0_u8; 16];
        let read = std::io::Read::read(&mut reader, &mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"a 3\nr 4\n");
    }

    #[test]
    fn user_files_are_sourced_at_top_level_and_zdotdir_is_handed_back() {
        let rc = zshrc();
        assert!(rc.contains(r#"builtin source "$__mework_user_zdotdir/.zshrc""#));
        let source = rc.find("builtin source").unwrap();
        let first_function = rc.find("() {").unwrap();
        assert!(
            source < first_function,
            "user files must not run inside a function"
        );
        assert!(rc.find("unset ZDOTDIR").unwrap() < first_function);
        let env = zshenv();
        assert!(
            env.find("unset MEWORK_TERMINAL_CONTROL_NONCE").unwrap()
                < env.find("builtin source").unwrap(),
            "the secrets leave the environment before any user file runs"
        );
    }

    /// Drives a real login zsh through the generated files: its first prompt
    /// says `ready`, an accepted line waits for the host and then runs, and a
    /// refused line does not run at all.
    #[test]
    fn a_real_zsh_waits_for_the_host_before_running_a_line() {
        use std::process::{Command, Stdio};
        let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
        else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join(".zshrc"),
            "typeset -U path\nMEWORK_USER_RC_LOADED=1\n",
        )
        .unwrap();
        let fifo = ControlFifo::create(PosixShell::Zsh, "n0nce").unwrap();
        fifo.send(true, 1).unwrap();
        fifo.send(false, 2).unwrap();
        // `zle` only exists in an interactive shell with a line editor, so
        // the hooks are exercised directly: `precmd`, then the widget body
        // with `zle` stubbed to record what it would have run.
        let script = r#"
zle() { if [[ $1 == __mework_original_* ]]; then print -r -- "RUN:$BUFFER"; fi }
__mework_precmd
BUFFER='echo first' WIDGET=accept-line __mework_accept
__mework_precmd
BUFFER='echo second' WIDGET=accept-line __mework_accept
print -r -- "RC:$MEWORK_USER_RC_LOADED ZDOTDIR:${ZDOTDIR-unset}"
"#;
        // zsh reads `$ZDOTDIR/.zshenv` by itself, even for `-c`; `.zshrc` is
        // only read by an interactive shell, so it is sourced here.
        let mut command = Command::new(zsh);
        command
            .args([
                "-c",
                &format!(
                    "source {dir}/.zshrc 2>/dev/null; {script}",
                    dir = fifo.directory.display()
                ),
            ])
            .env("HOME", home.path())
            .env_remove("MEWORK_USER_ZDOTDIR")
            .stdin(Stdio::null());
        for (name, value) in fifo.environment() {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let frame = |kind: &str, generation: u64| {
            format!("\x1b]633;Mework;v1;n0nce;{kind};{generation}\x07")
        };
        assert!(stdout.starts_with(&frame("ready", 0)), "{stdout:?}");
        let started = stdout.find(&frame("start", 1)).expect("first start");
        let ran = stdout.find("RUN:echo first").expect("accepted line runs");
        assert!(started < ran);
        assert!(stdout.contains(&frame("end", 1)), "{stdout:?}");
        assert!(stdout.contains(&frame("start", 2)), "{stdout:?}");
        assert!(!stdout.contains("RUN:echo second"), "{stdout:?}");
        assert!(stderr.contains("Mework 未执行该命令"), "{stderr:?}");
        assert!(stdout.contains("RC:1 ZDOTDIR:unset"), "{stdout:?}");
    }

    /// The bash rcfile against a real bash, with readline's side stubbed: the
    /// first prompt says `ready`, an accepted line waits for the host and is
    /// then committed with `accept-line`, and a refused line is left in place.
    /// On macOS this is bash 3.2, which cannot show the gate its line.
    #[test]
    fn a_real_bash_waits_for_the_host_before_running_a_line() {
        use std::process::{Command, Stdio};
        let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
        else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join(".bash_profile"),
            "declare -a MEWORK_USER_LIST=(one two)\nalias printf=false read=false\n",
        )
        .unwrap();
        let fifo = ControlFifo::create(PosixShell::Bash, "n0nce").unwrap();
        fifo.send(true, 1).unwrap();
        fifo.send(false, 2).unwrap();
        // `bind` needs a line editor, so the commit is stubbed to record what
        // readline would have been told to do with the line.
        let script = r#"
__mework_commit_with() { builtin printf 'COMMIT:%s\n' "$1"; }
__mework_prompt
READLINE_LINE='echo first' __mework_gate accept-line
__mework_prompt
READLINE_LINE='echo second' __mework_gate accept-line
builtin printf 'LIST:%s NONCE:%s CHANNEL:%s\n' "${#MEWORK_USER_LIST[@]}" "${MEWORK_TERMINAL_CONTROL_NONCE-unset}" "${MEWORK_TERMINAL_CONTROL_CHANNEL-unset}"
"#;
        let mut command = Command::new(bash);
        command
            .args([
                "-c",
                &format!(
                    "source '{}' 2>/dev/null; {script}",
                    fifo.directory.join(BASH_RCFILE).display()
                ),
            ])
            .env("HOME", home.path())
            .stdin(Stdio::null());
        for (name, value) in fifo.environment() {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let frame = |kind: &str, generation: u64| {
            format!("\x1b]633;Mework;v1;n0nce;{kind};{generation}\x07")
        };
        assert!(stdout.starts_with(&frame("ready", 0)), "{stdout:?}");
        let started = stdout.find(&frame("start", 1)).expect("first start");
        let committed = stdout
            .find("COMMIT:accept-line")
            .expect("accepted line runs");
        assert!(started < committed);
        assert!(stdout.contains(&frame("end", 1)), "{stdout:?}");
        assert!(stdout.contains(&frame("start", 2)), "{stdout:?}");
        assert_eq!(
            stdout.matches("COMMIT:accept-line").count(),
            1,
            "{stdout:?}"
        );
        assert!(stderr.contains("Mework 未执行该命令"), "{stderr:?}");
        // The profile ran at top level (its array is still there), its aliases
        // did not reach the hooks, and the secrets left the environment.
        assert!(
            stdout.contains("LIST:2 NONCE:unset CHANNEL:unset"),
            "{stdout:?}"
        );
    }

    /// The fish script against a real fish, when there is one, with the line
    /// editor's side stubbed the same way.
    #[test]
    fn a_real_fish_waits_for_the_host_before_running_a_line() {
        use std::process::{Command, Stdio};
        let Some(fish) = [
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
            "/bin/fish",
        ]
        .into_iter()
        .find(|path| Path::new(path).is_file()) else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let fifo = ControlFifo::create(PosixShell::Fish, "n0nce").unwrap();
        fifo.send(true, 1).unwrap();
        fifo.send(false, 2).unwrap();
        let script = r#"
function __mework_line_runs_something; true; end
function __mework_run_line; echo RUN; end
function __mework_hold_line; echo HELD >&2; end
__mework_prompt
__mework_execute
__mework_prompt
__mework_execute
echo "NONCE:$MEWORK_TERMINAL_CONTROL_NONCE."
"#;
        let init = fifo.leading_args()[1].clone();
        let output = Command::new(fish)
            .arg("-c")
            .arg({
                let mut command = init;
                command.push(format!(" 2>/dev/null; {script}"));
                command
            })
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let frame = |kind: &str, generation: u64| {
            format!("\x1b]633;Mework;v1;n0nce;{kind};{generation}\x07")
        };
        assert!(
            stdout.starts_with(&frame("ready", 0)),
            "{stdout:?} {stderr:?}"
        );
        let started = stdout.find(&frame("start", 1)).expect("first start");
        let ran = stdout.find("RUN").expect("accepted line runs");
        assert!(started < ran);
        assert!(stdout.contains(&frame("end", 1)), "{stdout:?}");
        assert!(stdout.contains(&frame("start", 2)), "{stdout:?}");
        assert_eq!(stdout.matches("RUN").count(), 1, "{stdout:?}");
        assert!(stderr.contains("HELD"), "{stderr:?}");
        assert!(stdout.contains("NONCE:."), "{stdout:?}");
    }
}
