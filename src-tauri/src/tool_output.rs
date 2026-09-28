//! Tool output too long to hand the model whole.
//!
//! Claude Code's answer, which this follows: past a per-tool size the whole
//! output goes to a file, and the model gets the file's path and a preview —
//! the first 2,000 characters — instead. Nothing is lost, and the model reads
//! or greps the file when the preview is not enough. The sizes are Claude
//! Code's too: 30,000 characters for a shell command, 20,000 for `grep`.
//!
//! The files live under the application data directory, one directory per
//! conversation ([`crate::workspace_dirs::ensure_tool_output_dir`]), and go
//! when the conversation does. A conversation whose workspace is on another
//! machine still gets its files here, on the host that captured the output,
//! so `read` and `grep` serve a path inside that directory on the host
//! whichever workspace the call names ([`host_file`]).

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use crate::prompt_profile::{PromptKey, PromptProfile};

/// A shell command's inline share, in characters.
pub(crate) const SHELL_INLINE_CHARS: usize = 30_000;
/// A `grep` result's inline share, in characters.
pub(crate) const GREP_INLINE_CHARS: usize = 20_000;
/// How much of a spilled output the model sees in place of it.
const PREVIEW_CHARS: usize = 2_000;

/// Where one tool call's overflow would go.
#[derive(Clone, Copy)]
pub(crate) struct Spill<'a> {
    pub app_data: &'a Path,
    pub conversation_id: &'a str,
    /// What the file is named after: the tool, and whatever identifies the
    /// call (a shell command's task id). A random suffix keeps two apart.
    pub stem: &'a str,
}

impl<'a> Spill<'a> {
    /// `None` for a caller with no application data directory or no
    /// conversation — tests and direct execution — which get the output cut
    /// to size instead.
    pub(crate) fn new(
        app_data: Option<&'a Path>,
        conversation_id: &'a str,
        stem: &'a str,
    ) -> Option<Self> {
        let app_data = app_data.filter(|path| !path.as_os_str().is_empty())?;
        if conversation_id.trim().is_empty() {
            return None;
        }
        Some(Self {
            app_data,
            conversation_id,
            stem,
        })
    }
}

/// `text` as the model receives it: itself when it fits in `limit`
/// characters, otherwise the spill notice — or, with nowhere to spill, the
/// first `limit` characters and a line saying the rest was cut.
pub(crate) fn fit(
    text: String,
    limit: usize,
    spill: Option<Spill<'_>>,
    profile: &PromptProfile,
) -> String {
    if text.chars().nth(limit).is_none() {
        return text;
    }
    if let Some(spill) = spill {
        if let Ok(path) = write(spill, &text) {
            return notice(&text, &path, profile);
        }
    }
    let end = text
        .char_indices()
        .nth(limit)
        .map_or(text.len(), |(index, _)| index);
    format!(
        "{}\n{}",
        &text[..end],
        profile.text(PromptKey::ToolOutputTruncated)
    )
}

fn write(spill: Spill<'_>, text: &str) -> Result<PathBuf, String> {
    let directory =
        crate::workspace_dirs::ensure_tool_output_dir(spill.app_data, spill.conversation_id)?;
    let stem = spill
        .stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .take(64)
        .collect::<String>();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let path = directory.join(format!("{stem}-{}.txt", &suffix[..12]));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("Could not save the output: {error}"))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("Could not save the output: {error}"))?;
    Ok(path)
}

/// What the model gets in place of a spilled output. The preview ends at the
/// last line break in its window when there is one past halfway, so it does
/// not stop mid-line — Claude Code's rule.
fn notice(text: &str, path: &Path, profile: &PromptProfile) -> String {
    let window_end = text
        .char_indices()
        .nth(PREVIEW_CHARS)
        .map_or(text.len(), |(index, _)| index);
    let window = &text[..window_end];
    let preview = match window.rfind('\n') {
        Some(end) if window[..end].chars().count() > PREVIEW_CHARS / 2 => &window[..end],
        _ => window,
    };
    profile.render(
        PromptKey::ToolOutputSpilled,
        &[
            ("size", &human_size(text.len() as u64)),
            ("path", &path.display().to_string()),
            ("preview_size", &human_size(PREVIEW_CHARS as u64)),
            ("preview", preview),
        ],
    )
}

/// `70.3 KB`, `2 KB`, `1.4 MB`: decimal units, one decimal place when it
/// says something.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} bytes");
    }
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0} {}", UNITS[unit])
    } else {
        format!("{rounded:.1} {}", UNITS[unit])
    }
}

/// The spilled file `requested` names, when it is one of this conversation's:
/// an absolute path whose canonical form is a file directly inside the
/// conversation's output directory. Anything else — another conversation's
/// file, a path that climbs out, a link — is `None`, and the call is judged
/// and run as it would have been.
pub(crate) fn host_file(
    app_data: &Path,
    conversation_id: &str,
    requested: &str,
) -> Option<PathBuf> {
    let requested = Path::new(requested.trim());
    if !requested.is_absolute() {
        return None;
    }
    let directory = crate::workspace_dirs::tool_output_dir(app_data, conversation_id).ok()?;
    let directory = fs::canonicalize(directory).ok()?;
    let canonical = fs::canonicalize(requested).ok()?;
    (canonical.parent() == Some(directory.as_path()) && canonical.is_file()).then_some(canonical)
}

/// Whether `request` is a `read` or `grep` of this conversation's spilled
/// output, which is served on the host whatever workspace it names.
pub(crate) fn names_host_file(
    app_data: &Path,
    request: &crate::model::ToolExecutionRequest,
) -> bool {
    if !matches!(request.tool_name.as_str(), "read" | "grep") {
        return false;
    }
    request
        .input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| host_file(app_data, &request.conversation_id, path).is_some())
}

/// The file a spill notice names, for tests that go on to read it.
#[cfg(test)]
pub(crate) fn saved_path(notice: &str) -> Option<String> {
    notice
        .lines()
        .find_map(|line| line.split_once("Full output saved to: "))
        .map(|(_, path)| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_output_is_left_alone() {
        let profile = PromptProfile::builtin_english();
        let app_data = tempfile::tempdir().unwrap();
        let spill = Spill::new(Some(app_data.path()), "conversation", "bash-1");
        assert_eq!(fit("short".into(), 10, spill, &profile), "short");
        assert!(!app_data.path().join("tool-output").exists());
    }

    #[test]
    fn a_long_output_is_saved_whole_and_previewed() {
        let profile = PromptProfile::builtin_english();
        let app_data = tempfile::tempdir().unwrap();
        let text = (0..5_000)
            .map(|n| format!("line {n}\n"))
            .collect::<String>();
        let spill = Spill::new(Some(app_data.path()), "conversation", "bash-7");
        let notice = fit(text.clone(), 30_000, spill, &profile);
        assert!(
            notice.starts_with("<persisted-output>\nOutput too large ("),
            "{notice}"
        );
        let path = saved_path(&notice).expect("the notice names the file");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("bash-7-"));
        assert!(notice.contains("line 0\n"), "{notice}");
        assert!(!notice.contains("line 4999"), "{notice}");
        assert!(notice.ends_with("</persisted-output>"), "{notice}");
        // The preview stops at a line break.
        let preview = notice.split("Preview (first 2 KB):\n").nth(1).unwrap();
        let last_preview_line = preview.lines().nth_back(2).unwrap();
        assert!(
            last_preview_line.starts_with("line "),
            "{last_preview_line}"
        );
        assert_eq!(
            host_file(app_data.path(), "conversation", &path),
            Some(fs::canonicalize(&path).unwrap())
        );
        assert_eq!(
            host_file(app_data.path(), "another conversation", &path),
            None
        );
    }

    #[test]
    fn with_nowhere_to_spill_the_output_is_cut() {
        let profile = PromptProfile::builtin_english();
        let cut = fit("中文".repeat(20), 10, None, &profile);
        assert!(cut.starts_with(&"中文".repeat(5)), "{cut}");
        assert!(
            cut.ends_with(profile.text(PromptKey::ToolOutputTruncated)),
            "{cut}"
        );
    }

    #[test]
    fn sizes_read_like_claude_codes() {
        assert_eq!(human_size(512), "512 bytes");
        assert_eq!(human_size(2_000), "2 KB");
        assert_eq!(human_size(70_312), "70.3 KB");
        assert_eq!(human_size(1_400_000), "1.4 MB");
    }
}
