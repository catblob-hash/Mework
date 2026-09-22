//! The answer route for Git Bash on a Windows host.
//!
//! Git Bash runs on the MSYS runtime, which can wait on neither of the named
//! Win32 events PowerShell uses nor a FIFO a native process created — an MSYS
//! FIFO exists only inside the runtime. What both sides share is the file
//! system, so the host answers `start` with a file: `reply-<generation>` in the
//! session's private directory, holding `a` (the command lease is held) or `r`
//! (a workspace writer is active). It is written under a temporary name and
//! renamed into place, so the shell never reads half of one, and the bash hook
//! polls for it ([`super::bash_control`]).
//!
//! Compiled everywhere so the shell side can be exercised by a real bash in
//! tests; only Windows launches with it.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use super::bash_control::{self, Decision};

/// The rcfile's name inside the session directory.
const RCFILE: &str = "mework.bash";

#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct ControlReplyFiles {
    directory: PathBuf,
    /// `directory` as the shell spells it: a drive-letter path with forward
    /// slashes, which the MSYS runtime accepts wherever it takes a path and
    /// which needs no escaping inside a shell word.
    shell_directory: String,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl ControlReplyFiles {
    pub(super) fn create() -> Result<Self, String> {
        let directory =
            std::env::temp_dir().join(format!("mework-terminal-{}", Uuid::new_v4().simple()));
        let shell_directory = shell_path(&directory)?;
        // On Windows the directory inherits the ACL of the user's own temporary
        // directory, which only that user can read.
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut builder = fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        builder
            .create(&directory)
            .map_err(|error| format!("无法创建终端控制目录：{error}"))?;
        let control = Self {
            directory,
            shell_directory,
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(control.directory.join(RCFILE))
            .and_then(|mut file| {
                std::io::Write::write_all(
                    &mut file,
                    bash_control::bashrc(Decision::ReplyFiles).as_bytes(),
                )
            })
            .map_err(|error| format!("无法写入终端启动文件 {RCFILE}：{error}"))?;
        Ok(control)
    }

    /// The variables the rcfile reads, and removes, before any user file runs.
    pub(super) fn environment(&self, nonce: &str) -> Vec<(&'static str, OsString)> {
        vec![
            ("MEWORK_TERMINAL_CONTROL_NONCE", OsString::from(nonce)),
            (
                "MEWORK_TERMINAL_CONTROL_CHANNEL",
                OsString::from(&self.shell_directory),
            ),
        ]
    }

    /// What goes before bash's own arguments: long options must lead.
    pub(super) fn leading_args(&self) -> Vec<OsString> {
        vec![
            OsString::from("--rcfile"),
            OsString::from(format!("{}/{RCFILE}", self.shell_directory)),
        ]
    }

    /// Answers the `start` frame of `generation`.
    ///
    /// By the time the host reads `start` for one generation the shell has
    /// stopped waiting for every earlier one, so their files are removed here
    /// rather than by the shell, which could only do it by starting `rm`.
    pub(super) fn send(&self, accepted: bool, generation: u64) -> Result<(), String> {
        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("reply-"))
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let staged = self.directory.join(format!("reply-{generation}.tmp"));
        fs::write(&staged, if accepted { "a\n" } else { "r\n" })
            .and_then(|()| fs::rename(&staged, self.directory.join(format!("reply-{generation}"))))
            .map_err(|error| format!("无法写入终端命令确认文件：{error}"))
    }
}

impl Drop for ControlReplyFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// A path as MSYS bash reads it. A verbatim prefix would not survive, and a
/// path Windows cannot express as Unicode cannot be written into a script.
fn shell_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("终端控制目录路径无法表示为文本：{}", path.display()))?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    Ok(text.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_reach_the_shell_with_forward_slashes() {
        assert_eq!(
            shell_path(Path::new(
                r"C:\Users\Ada Lovelace\AppData\Local\Temp\mework-terminal-1"
            ))
            .unwrap(),
            "C:/Users/Ada Lovelace/AppData/Local/Temp/mework-terminal-1"
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\C:\Temp\mework-terminal-1")).unwrap(),
            "C:/Temp/mework-terminal-1"
        );
    }

    #[test]
    fn a_reply_replaces_the_earlier_ones_and_the_directory_goes_with_the_channel() {
        let files = ControlReplyFiles::create().unwrap();
        let directory = files.directory.clone();
        assert!(directory.join(RCFILE).is_file());
        files.send(true, 1).unwrap();
        files.send(false, 2).unwrap();
        assert!(!directory.join("reply-1").exists());
        assert_eq!(
            fs::read_to_string(directory.join("reply-2")).unwrap(),
            "r\n"
        );
        assert!(!directory.join("reply-2.tmp").exists());
        let args = files.leading_args();
        assert_eq!(args[0], OsString::from("--rcfile"));
        assert!(args[1].to_string_lossy().ends_with("/mework.bash"));
        assert!(!args[1].to_string_lossy().contains('\\'));
        drop(files);
        assert!(!directory.exists());
    }

    /// The Git Bash rcfile against a real bash: the hooks are driven directly
    /// with readline's `bind` stubbed, while the host answers through files.
    /// macOS's bash 3.2 exercises the `sleep` fallback of the poll.
    #[cfg(unix)]
    #[test]
    fn a_real_bash_reads_its_answer_from_reply_files() {
        use std::{
            io::Read,
            process::{Command, Stdio},
            time::Duration,
        };
        let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
        else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join(".bash_profile"), "MEWORK_USER_PROFILE=1\n").unwrap();
        let files = ControlReplyFiles::create().unwrap();
        let script = r#"
__mework_commit_with() { builtin printf 'COMMIT:%s\n' "$1"; }
__mework_prompt
READLINE_LINE='echo first' __mework_gate accept-line
__mework_prompt
READLINE_LINE='echo second' __mework_gate accept-line
builtin printf 'PROFILE:%s NONCE:%s\n' "${MEWORK_USER_PROFILE-}" "${MEWORK_TERMINAL_CONTROL_NONCE-unset}"
"#;
        let mut command = Command::new(bash);
        command
            .args([
                "-c",
                &format!(
                    "source '{}/{RCFILE}' 2>/dev/null; {script}",
                    files.shell_directory
                ),
            ])
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in files.environment("n0nce") {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        // Play the host: answer each `start` as it appears on the output.
        let mut stdout = child.stdout.take().unwrap();
        let mut seen = Vec::new();
        let mut answered = 0;
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stdout.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&seen).into_owned();
            for generation in [1_u64, 2] {
                if answered < generation
                    && text.contains(&format!("\x1b]633;Mework;v1;n0nce;start;{generation}\x07"))
                {
                    std::thread::sleep(Duration::from_millis(50));
                    files.send(generation == 1, generation).unwrap();
                    answered = generation;
                }
            }
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&seen);
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
        // The refused line is committed with something that leaves it in place:
        // nothing at all, or on bash before 4.4 a move back to its end.
        assert_eq!(
            stdout.matches("COMMIT:accept-line").count(),
            1,
            "{stdout:?}"
        );
        assert!(
            stdout.contains(r#"COMMIT:"""#) || stdout.contains("COMMIT:end-of-line"),
            "{stdout:?}"
        );
        assert!(stderr.contains("Mework 未执行该命令"), "{stderr:?}");
        assert!(stdout.contains("PROFILE:1 NONCE:unset"), "{stdout:?}");
    }
}
