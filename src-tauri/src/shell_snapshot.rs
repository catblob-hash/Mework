//! The Bash session snapshot, the mechanism that makes Claude Code's
//! one-process-per-call shell feel continuous.
//!
//! Claude Code spawns a brand new `bash` for every tool call — there is no
//! long-lived shell — so nothing a command defines survives on its own. What
//! survives is replayed: once per session it runs the user's shell rc file in a
//! login shell, serializes the resulting `shopt` flags, functions, `set -o`
//! options, aliases, and `PATH` into a script, and every later shell `source`s
//! that script as its first clause. A shell started this way therefore knows the
//! user's aliases and functions without being a login shell, which is why the
//! spawn argv drops `-l` whenever a snapshot exists.
//!
//! The snapshot is generated, not written by hand, and it is generated from the
//! user's own rc file. That makes it untrusted input in the sense that matters:
//! its contents are whatever the user's dotfiles produce. It is never parsed
//! here — it is handed to `source` — so there is nothing to sanitize, but it is
//! also the reason the shell tool no longer clears `BASH_ENV` and friends. A
//! shell that deliberately replays the user's functions cannot also claim to be
//! a sterile environment, and pretending otherwise was the more misleading of
//! the two options.
//!
//! One thing Claude Code puts in its snapshot is deliberately not copied: shims
//! that re-`exec` its own binary as `rg` and guard `pkill` against killing the
//! CLI. Mework bundles no ripgrep and has no such process to protect, so those
//! clauses would be inert at best.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long the generator may take. It runs the user's rc file, which can do
/// arbitrary work; a dotfile that blocks must not hang the first shell call.
const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Directory holding generated snapshots, under the app-data root.
pub(crate) fn snapshot_directory(app_data: &Path) -> PathBuf {
    app_data.join("shell-snapshots")
}

/// The rc file a shell of this flavour reads, chosen the way Claude Code chooses
/// it: by what the interpreter path is called, not by what is installed.
fn rc_file_for(shell_path: &str) -> Option<PathBuf> {
    let home = dirs_home()?;
    let lowered = shell_path.to_ascii_lowercase();
    let name = if lowered.contains("zsh") {
        ".zshrc"
    } else if lowered.contains("bash") {
        ".bashrc"
    } else {
        ".profile"
    };
    Some(home.join(name))
}

fn dirs_home() -> Option<PathBuf> {
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(variable) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// The flavour word that goes in the file name, matching Claude Code's
/// `snapshot-<flavour>-<millis>-<random>.sh`.
fn flavour_of(shell_path: &str) -> &'static str {
    let lowered = shell_path.to_ascii_lowercase();
    if lowered.contains("zsh") {
        "zsh"
    } else if lowered.contains("bash") {
        "bash"
    } else {
        "sh"
    }
}

/// Six lowercase base-36 characters, as Claude Code's
/// `Math.random().toString(36).substring(2, 8)` produces.
fn random_suffix() -> String {
    // Two unrelated clocks are mixed so two snapshots minted in the same
    // millisecond still differ; the value only has to be unique, not secret.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(std::process::id() as u64)
        .wrapping_add(1_442_695_040_888_963_407);
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    (0..6)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ALPHABET[(state % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

/// Quotes a path for a POSIX single-quoted shell word.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Rewrites a Windows path into the `/c/...` form Git Bash understands.
pub(crate) fn bash_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/{drive}{}", &text[2..]);
    }
    text
}

/// The generator script, run once per session by a login shell.
///
/// The emitted snapshot matches Claude Code's clause for clause: unalias
/// everything first so an alias cannot be frozen inside a function definition,
/// then `shopt`, then one `eval` per function, then `set -o`, then
/// `expand_aliases`, then the aliases, then `PATH`.
///
/// Two details are load-bearing. Functions are emitted one `eval` per function
/// with `printf %q` so a body that no longer parses drops only itself instead of
/// aborting the whole `source`, and `%q` needs no command substitution at source
/// time. Aliases defined as `winpty` wrappers are dropped under MSYS/Cygwin
/// because `winpty` cannot attach to a pipe and every command wrapped in one
/// would fail.
fn generator_script(snapshot_file: &Path, rc_file: Option<&Path>) -> String {
    let target = quote(&bash_path(snapshot_file));
    let source_rc = match rc_file {
        Some(rc) => format!("source {} < /dev/null", quote(&bash_path(rc))),
        None => "# No user config file to source".to_owned(),
    };
    format!(
        r##"SNAPSHOT_FILE={target}
{source_rc}

# First, create/clear the snapshot file
echo "# Snapshot file" >| "$SNAPSHOT_FILE"

# When this file is sourced, we first unalias to avoid conflicts with functions.
# Aliases get "frozen" inside function definitions at definition time, which can
# cause unexpected behavior when functions use commands that conflict with aliases.
echo "# Unset all aliases to avoid conflicts with functions" >> "$SNAPSHOT_FILE"
echo "unalias -a 2>/dev/null || true" >> "$SNAPSHOT_FILE"

echo "# Shopt" >> "$SNAPSHOT_FILE"
shopt -p | head -n 1000 >> "$SNAPSHOT_FILE"

echo "# Functions" >> "$SNAPSHOT_FILE"
# One eval per function so a body that no longer parses (rc=2, not fatal in
# non-POSIX bash) drops only itself. The %q literal needs no fork at source time,
# unlike a base64 command substitution.
declare -F | cut -d' ' -f3 | grep -vE '^_[^_]' | while read -r func; do
  printf 'eval %q > /dev/null 2>&1\n' "$(declare -f "$func")" >> "$SNAPSHOT_FILE"
done

echo "# Shell Options" >> "$SNAPSHOT_FILE"
set -o | grep "on" | awk '{{print "set -o " $1}}' | head -n 1000 >> "$SNAPSHOT_FILE"
echo "shopt -s expand_aliases" >> "$SNAPSHOT_FILE"

echo "# Aliases" >> "$SNAPSHOT_FILE"
if [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
  alias | grep -v "='winpty " | sed 's/^alias //g' | sed 's/^/alias -- /' | head -n 1000 >> "$SNAPSHOT_FILE"
else
  alias | sed 's/^alias //g' | sed 's/^/alias -- /' | head -n 1000 >> "$SNAPSHOT_FILE"
fi

echo "# Path" >> "$SNAPSHOT_FILE"
echo "export PATH='$PATH'" >> "$SNAPSHOT_FILE"

# Exit silently on success, only report errors
if [ ! -f "$SNAPSHOT_FILE" ]; then
  echo "Error: Snapshot file was not created at $SNAPSHOT_FILE" >&2
  exit 1
fi
"##
    )
}

/// Builds one snapshot and returns its path. `None` means the shell must fall
/// back to a login shell (`-l`), which is what Claude Code does when its own
/// snapshot is missing: strictly worse, because a login shell re-runs the
/// profile on every call, but never wrong.
///
/// Failure is deliberately quiet. A user whose `.bashrc` exits non-zero should
/// still be able to run commands, so a generator that fails leaves the caller
/// with the fallback rather than an error the model has to interpret.
pub(crate) fn build(app_data: &Path, shell_path: &str) -> Option<PathBuf> {
    let directory = snapshot_directory(app_data);
    if std::fs::create_dir_all(&directory).is_err() {
        return None;
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let file = directory.join(format!(
        "snapshot-{}-{millis}-{}.sh",
        flavour_of(shell_path),
        random_suffix()
    ));
    let rc = rc_file_for(shell_path).filter(|path| path.is_file());
    let script = generator_script(&file, rc.as_deref());

    let mut command = Command::new(shell_path);
    command
        .args(["-c", "-l", script.as_str()])
        // The generator runs the user's rc file, which may inspect these.
        .env("SHELL", shell_path)
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    // The rc file is arbitrary user code, so the wait is bounded and a snapshot
    // that has not appeared by the deadline is abandoned along with its process.
    let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    // The exit status is not the criterion: an rc file that ends non-zero still
    // produces a usable snapshot, and Claude Code likewise only checks the file.
    file.is_file().then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_become_git_bash_paths() {
        assert_eq!(bash_path(Path::new(r"C:\Users\a\b.sh")), "/c/Users/a/b.sh");
        assert_eq!(bash_path(Path::new("/already/posix")), "/already/posix");
        // A relative path has no drive letter to rewrite.
        assert_eq!(bash_path(Path::new(r"rel\path")), "rel/path");
    }

    #[test]
    fn a_quoted_path_survives_an_apostrophe() {
        assert_eq!(quote("it's"), r#"'it'"'"'s'"#);
    }

    /// The suffix only has to distinguish two snapshots minted back to back.
    #[test]
    fn random_suffixes_are_six_base36_characters_and_differ() {
        let first = random_suffix();
        let second = random_suffix();
        assert_eq!(first.len(), 6);
        assert!(first.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(first, second);
    }

    #[test]
    fn the_rc_file_is_chosen_by_interpreter_name() {
        let home = dirs_home().expect("a home directory");
        assert_eq!(rc_file_for("/usr/bin/zsh"), Some(home.join(".zshrc")));
        assert_eq!(rc_file_for(r"C:\Program Files\Git\bin\bash.exe"), Some(home.join(".bashrc")));
        assert_eq!(rc_file_for("/bin/dash"), Some(home.join(".profile")));
    }

    /// The emitted snapshot has to unalias before it defines functions, or an
    /// alias is frozen into a function body at definition time.
    #[test]
    fn the_generator_unaliases_before_it_replays_functions() {
        let script = generator_script(Path::new("/tmp/s.sh"), None);
        let unalias = script.find("unalias -a").expect("unalias clause");
        let functions = script.find("# Functions").expect("functions clause");
        assert!(unalias < functions);
        assert!(script.contains("# No user config file to source"));
    }

    /// A `winpty` alias cannot attach to a pipe, so replaying one would break
    /// every command it wraps.
    #[test]
    fn winpty_aliases_are_dropped_under_msys() {
        let script = generator_script(Path::new("/tmp/s.sh"), Some(Path::new("/home/u/.bashrc")));
        assert!(script.contains(r#"grep -v "='winpty ""#));
        assert!(script.contains("source '/home/u/.bashrc' < /dev/null"));
    }

    /// One `eval` per function: a body that stopped parsing must not take the
    /// rest of the snapshot down with it.
    #[test]
    fn functions_are_replayed_one_eval_at_a_time() {
        let script = generator_script(Path::new("/tmp/s.sh"), None);
        assert!(script.contains("printf 'eval %q > /dev/null 2>&1\\n'"));
    }
}
