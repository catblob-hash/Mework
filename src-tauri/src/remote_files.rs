//! The filesystem tools on a workspace that lives on another machine.
//!
//! `ls`, `grep`, `find`, `read`, `write` and `edit` have a host leg in
//! [`crate::tool_executor`] that acts on this filesystem directly. When the
//! workspace a call names sits on a WSL distribution or an SSH machine there is
//! no such filesystem to act on, so the same six tools are expressed as bash
//! scripts and dispatched through that machine's own shell — the transport
//! [`crate::remote_directory`] already uses for the folder picker, generalized in
//! [`crate::run_environment::run_remote_script`].
//!
//! Three rules hold the leg together:
//!
//! * **The machine is the caller's decision, never the path's.** A call acts on
//!   the workspace it addressed by number; a path is resolved *inside* that
//!   workspace, so no spelling of a path can reach a machine the conversation was
//!   not granted.
//! * **Every host-supplied fragment is single-quoted.** The script is authored
//!   here and the model contributes only quoted operands; a path that could
//!   rewrite the script is refused before it is built.
//! * **The remote shell decides where a path leads, and the script checks the
//!   answer.** Confinement is tested against the canonical path — symlinks
//!   already resolved — so a link inside the root pointing out of it is refused
//!   under [`Confinement::Workspace`]. That is intended: the same rule the host
//!   leg's path guard applies.
//!
//! Records taken here are keyed by [`file_read_state::remote_key`], which folds
//! the machine's identity in: one machine's `/srv/app` is not another's, and a
//! `stat` on this host says nothing about either.

use std::path::{Path, PathBuf};
use std::time::Duration;

use globset::Glob;

use crate::{
    cancel::CancelSignal,
    file_read_state::{self, FileReadRecord},
    image_attachments::{is_supported_image, ImageAttachmentStore, MAX_IMAGE_ATTACHMENT_BYTES},
    model::{ImageAttachment, JsonObject},
    prompt_profile::{PromptKey, PromptProfile},
    run_environment::{self, RemoteCommandOutput, ShellRunner},
    tool_executor::{
        apply_edit, optional_bool, optional_string, optional_u64, parse_read_range,
        required_string, slice_text_lines, truncate_chars, unified_diff, write_receipt_note,
        FileGuardContext, FileGuardTouch, FILE_MODIFIED_SINCE_READ, FILE_NOT_READ,
        MAX_LIST_ENTRIES, MAX_PATH_CHARS, MAX_SEARCH_MATCHES, MAX_TEXT_FILE, MAX_WRITE_BYTES,
    },
    workspace_set::ResolvedWorkspace,
};

/// How far a call may reach on the machine.
///
/// Derived by the caller from the security classifier's `ExecutionScope`:
/// a restricted scope confines the call to the workspace root, an unrestricted
/// one lets it name anything the remote user can open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confinement {
    Workspace,
    Machine,
}

/// Everything a remote call needs about where it acts.
pub(crate) struct RemoteWorkspace<'a> {
    pub workspace: &'a ResolvedWorkspace,
    /// `run_environment::env_key(workspace.machine.as_ref())` — the machine's
    /// identity, which is what keeps one machine's records apart from another's.
    pub machine_key: String,
    pub confinement: Confinement,
    pub profile: &'a PromptProfile,
    pub cancel: &'a CancelSignal,
}

/// What a remote `read`, `write` or `edit` hands back.
pub(crate) struct RemoteOutcome {
    pub output: String,
    pub diff: Option<String>,
    pub images: Vec<ImageAttachment>,
    /// The record the run loop commits once the result is final. Never carries
    /// an opened file: there is no local path for the renderer to open.
    pub file_touch: Option<FileGuardTouch>,
}

/// The transport one call dispatches through.
///
/// A trait rather than a direct call so the tests can put a local Bash behind
/// the same scripts the WSL and SSH legs run.
pub(crate) trait RemoteShell {
    fn run(
        &self,
        script: &str,
        stdin: Option<&[u8]>,
        timeout: Duration,
        cancel: &CancelSignal,
    ) -> Result<RemoteCommandOutput, String>;
}

impl RemoteShell for ShellRunner {
    fn run(
        &self,
        script: &str,
        stdin: Option<&[u8]>,
        timeout: Duration,
        cancel: &CancelSignal,
    ) -> Result<RemoteCommandOutput, String> {
        run_environment::run_remote_script(self, script, stdin, timeout, cancel)
    }
}

/// A search may walk a whole checkout over a link that is not fast; a file round
/// trip moves one file and should not wait nearly as long for it.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);
const FILE_TIMEOUT: Duration = Duration::from_secs(60);

/// Exit codes the scripts reserve for conditions the host has wording for.
/// Everything else is reported with the machine's own stderr.
const EXIT_ROOT_MISSING: i32 = 64;
const EXIT_OUTSIDE: i32 = 65;
const EXIT_NOT_FOUND: i32 = 66;
pub(crate) const EXIT_WRONG_KIND: i32 = 67;
pub(crate) const EXIT_TOO_LARGE: i32 = 68;
const EXIT_CHANGED: i32 = 69;
/// `grep`'s own "bad pattern" code, passed through so the host can quote the
/// remote grep's complaint rather than invent one.
const EXIT_BAD_PATTERN: i32 = 2;

/// Lines of a remote `find` listing the host will pull over for `find`'s own
/// matching. Past it the listing is cut and the result says so: a silently
/// shortened listing reads as "no such file".
const MAX_SCANNED_ENTRIES: usize = 200_000;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub(crate) fn run_ls(target: &RemoteWorkspace<'_>, input: &JsonObject) -> Result<String, String> {
    ls_with(&target.workspace.runner, target, input)
}

pub(crate) fn run_grep(target: &RemoteWorkspace<'_>, input: &JsonObject) -> Result<String, String> {
    grep_with(&target.workspace.runner, target, input)
}

pub(crate) fn run_find(target: &RemoteWorkspace<'_>, input: &JsonObject) -> Result<String, String> {
    find_with(&target.workspace.runner, target, input)
}

pub(crate) fn run_read(
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    attachment_store: Option<&ImageAttachmentStore>,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    read_with(
        &target.workspace.runner,
        target,
        input,
        attachment_store,
        file_guard,
    )
}

pub(crate) fn run_write(
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    write_with(&target.workspace.runner, target, input, file_guard)
}

/// The text of one remote file for a tool that scores it rather than shows it: the display
/// path and the content. Images and oversized files are refused the way `read` refuses them.
/// No read record is taken, because nothing of the file is put in front of the model.
pub(crate) fn read_text(
    target: &RemoteWorkspace<'_>,
    path: &str,
) -> Result<(String, String), String> {
    read_text_with(&target.workspace.runner, target, path)
}

pub(crate) fn run_edit(
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    edit_with(&target.workspace.runner, target, input, file_guard)
}

// ---------------------------------------------------------------------------
// Script construction
// ---------------------------------------------------------------------------

/// Whether the script has to find the target already there.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetMode {
    /// The target must exist; a missing one is exit 66.
    Existing,
    /// The target need not exist yet — the nearest existing ancestor is
    /// canonicalized and the remainder re-appended, so confinement is still
    /// decided on a path with every symlink resolved.
    ForWrite,
}

/// Resolution of the requested path against the workspace root.
///
/// `~` is expanded by the script rather than the shell so the operand can stay
/// fully single-quoted: a tilde inside quotes expands to nothing, and a tilde
/// outside them would let the rest of the path out of its quotes.
const RESOLVE_TARGET: &str = r#"case "$REQ" in
"~") T="$HOME" ;;
"~/"*) T="$HOME/${REQ#"~/"}" ;;
/*) T="$REQ" ;;
*) T="$ROOT/$REQ" ;;
esac
"#;

/// Canonicalization and `stat`, in whichever spelling the remote has.
///
/// `realpath -m` is the one tool that answers for a path whose last components
/// do not exist yet; without it the walk-up fallback resolves the deepest
/// existing ancestor and re-appends the rest, which resolves every symlink that
/// could exist and therefore every symlink confinement has to see.
const SHELL_HELPERS: &str = r#"canon_walk() {
_rest=
_cur=$1
while [ ! -d "$_cur" ]; do
_b=$(basename -- "$_cur")
_p=$(dirname -- "$_cur")
if [ "$_p" = "$_cur" ]; then break; fi
if [ -z "$_rest" ]; then _rest=$_b; else _rest=$_b/$_rest; fi
_cur=$_p
done
_base=$(cd -- "$_cur" 2>/dev/null && pwd -P) || return 1
if [ -z "$_rest" ]; then printf '%s\n' "$_base"; return 0; fi
case "$_base" in
*/) printf '%s%s\n' "$_base" "$_rest" ;;
*) printf '%s/%s\n' "$_base" "$_rest" ;;
esac
}
if realpath -m -- / >/dev/null 2>&1; then
canon() { realpath -m -- "$1"; }
elif realpath -- / >/dev/null 2>&1; then
canon() { if [ -e "$1" ]; then realpath -- "$1"; else canon_walk "$1"; fi; }
elif readlink -m -- / >/dev/null 2>&1; then
canon() { readlink -m -- "$1"; }
elif readlink -f -- / >/dev/null 2>&1; then
canon() { if [ -e "$1" ]; then readlink -f -- "$1"; else canon_walk "$1"; fi; }
else
canon() { canon_walk "$1"; }
fi
mtime() { stat -c %Y -- "$1" 2>/dev/null || stat -f %m -- "$1" 2>/dev/null || echo 0; }
fsize() { stat -c %s -- "$1" 2>/dev/null || stat -f %z -- "$1" 2>/dev/null || wc -c < "$1"; }
digits() { case "$1" in ''|*[!0-9]*) printf '0\n' ;; *) printf '%s\n' "$1" ;; esac; }
"#;

/// The confinement test, on the canonical path so a symlink cannot smuggle a
/// target past it.
const CONFINE_TO_ROOT: &str = r#"case "$C" in
"$ROOT") ;;
"$ROOT"/*) ;;
*) printf '%s\n' "$C" >&2; exit 65 ;;
esac
"#;

/// Marks directories with a trailing slash while keeping one traversal order.
///
/// A symlink to a directory is deliberately not marked and not descended into,
/// which is what the host leg's `WalkDir` with `follow_links(false)` reports.
///
/// One entry per line, which a file name containing a newline would split into
/// two. Accepted, as it is in the folder picker: the split names are shown and
/// nothing downstream trusts them except as a path to name back, where they
/// simply do not resolve.
const MARK_ENTRIES: &str = r#"-exec sh -c 'for p do if [ -d "$p" ] && [ ! -L "$p" ]; then printf "%s/\n" "$p"; else printf "%s\n" "$p"; fi; done' sh {} +"#;

/// The prologue every script shares: enter the root, resolve and canonicalize
/// the requested path, test confinement, then announce both.
///
/// Output line 1 is the canonical root and line 2 the canonical target. The
/// payload follows, and may be arbitrary bytes, so the host splits the header on
/// `\n` as bytes rather than reading the whole answer as text.
pub(crate) fn prologue(
    target: &RemoteWorkspace<'_>,
    path: &str,
    mode: TargetMode,
) -> Result<String, String> {
    let mut script = String::with_capacity(2048);
    script.push_str("set -f\n");
    script.push_str(&format!(
        "cd -- {} || exit {EXIT_ROOT_MISSING}\n",
        quote_root(&target.workspace.root)?
    ));
    script.push_str("ROOT=$(pwd -P)\n");
    script.push_str(&format!("REQ={}\n", quote_operand(path, "path")?));
    script.push_str(RESOLVE_TARGET);
    script.push_str(SHELL_HELPERS);
    if mode == TargetMode::Existing {
        script.push_str(&format!("[ -e \"$T\" ] || exit {EXIT_NOT_FOUND}\n"));
    }
    script.push_str(&format!("C=$(canon \"$T\") || exit {EXIT_NOT_FOUND}\n"));
    script.push_str(&format!("[ -n \"$C\" ] || exit {EXIT_NOT_FOUND}\n"));
    if target.confinement == Confinement::Workspace {
        script.push_str(CONFINE_TO_ROOT);
    }
    script.push_str("printf '%s\\n' \"$ROOT\"\nprintf '%s\\n' \"$C\"\n");
    Ok(script)
}

/// POSIX single-quoting for the workspace root, with a bare `~` prefix left
/// outside the quotes because a remote root is recorded the way the user picked
/// it and may be spelled that way.
fn quote_root(root: &str) -> Result<String, String> {
    check_operand(root, "workspace root")?;
    Ok(run_environment::quote_remote_path(root.trim()))
}

/// POSIX single-quoting for a model-supplied operand. The quoting is what makes
/// the fragment inert; the checks keep out the two things quoting cannot hold —
/// an empty operand and control characters, which include the NUL that no argv
/// can carry.
fn quote_operand(text: &str, label: &str) -> Result<String, String> {
    check_operand(text, label)?;
    Ok(run_environment::sh_single_quote(text))
}

fn check_operand(text: &str, label: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("Parameter {label} cannot be empty"));
    }
    if text.chars().count() > MAX_PATH_CHARS {
        return Err(format!(
            "Parameter {label} exceeds the {MAX_PATH_CHARS}-character limit"
        ));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("Parameter {label} cannot contain control characters"));
    }
    Ok(())
}

/// A search operand is not a path: a regular expression may hold almost
/// anything, so only the characters that cannot survive the trip are refused.
fn quote_search_operand(text: &str, label: &str) -> Result<String, String> {
    if text.contains('\u{0}') {
        return Err(format!("Parameter {label} cannot contain NUL"));
    }
    Ok(run_environment::sh_single_quote(text))
}

// ---------------------------------------------------------------------------
// Result parsing
// ---------------------------------------------------------------------------

/// What the header of every answer carries.
pub(crate) struct Header {
    pub root: String,
    pub canonical: String,
}

/// Splits `count` `\n`-terminated lines off the front of a raw answer.
///
/// Byte-wise on purpose: a `read` payload follows the header and is whatever the
/// file holds.
pub(crate) fn take_lines(bytes: &[u8], count: usize) -> Result<(Vec<String>, &[u8]), String> {
    let mut lines = Vec::with_capacity(count);
    let mut rest = bytes;
    for _ in 0..count {
        let Some(end) = rest.iter().position(|byte| *byte == b'\n') else {
            return Err("The remote machine returned an incomplete result".into());
        };
        lines.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    Ok((lines, rest))
}

pub(crate) fn take_header(bytes: &[u8]) -> Result<(Header, &[u8]), String> {
    let (lines, rest) = take_lines(bytes, 2)?;
    let mut lines = lines.into_iter();
    let root = lines.next().unwrap_or_default();
    let canonical = lines.next().unwrap_or_default();
    if root.is_empty() || canonical.is_empty() {
        return Err("The remote machine did not report where it acted".into());
    }
    Ok((Header { root, canonical }, rest))
}

/// A remote path as the model should see it: relative to the workspace root, or
/// absolute when it lies outside — which only an unconfined call can reach.
fn display_relative(root: &str, path: &str) -> String {
    if path == root {
        return ".".to_owned();
    }
    let prefix = if root.ends_with('/') {
        root.to_owned()
    } else {
        format!("{root}/")
    };
    path.strip_prefix(&prefix)
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_owned())
}

fn base_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("file")
}

// ---------------------------------------------------------------------------
// Exit-code mapping
// ---------------------------------------------------------------------------

/// The tool-specific half of the exit-code table: the two conditions whose
/// wording depends on what the call was trying to do.
pub(crate) struct ExitWording<'a> {
    path: &'a str,
    wrong_kind: String,
    too_large: String,
    /// Whether `grep`'s own exit 2 should read as a bad pattern.
    grep: bool,
}

impl<'a> ExitWording<'a> {
    pub(crate) fn new(path: &'a str) -> Self {
        Self {
            path,
            wrong_kind: format!("{path} is not of the expected kind"),
            too_large: format!(
                "Text file exceeds the {} MiB limit",
                MAX_TEXT_FILE / 1024 / 1024
            ),
            grep: false,
        }
    }

    pub(crate) fn wrong_kind(mut self, message: String) -> Self {
        self.wrong_kind = message;
        self
    }

    pub(crate) fn too_large(mut self, message: String) -> Self {
        self.too_large = message;
        self
    }

    fn grep(mut self) -> Self {
        self.grep = true;
        self
    }
}

pub(crate) fn machine_name(target: &RemoteWorkspace<'_>) -> String {
    if target.workspace.machine_label.trim().is_empty() {
        "the remote machine".to_owned()
    } else {
        target.workspace.machine_label.clone()
    }
}

/// Turns a non-zero exit into the sentence the model reads.
fn exit_message(
    target: &RemoteWorkspace<'_>,
    wording: &ExitWording<'_>,
    output: &RemoteCommandOutput,
) -> String {
    let detail = output.stderr.trim();
    match output.status {
        Some(EXIT_ROOT_MISSING) => {
            let root = &target.workspace.root;
            let base = format!(
                "Workspace {} on {} could not be entered: {root}",
                target.workspace.index,
                machine_name(target)
            );
            if detail.is_empty() {
                base
            } else {
                format!("{base} ({detail})")
            }
        }
        Some(EXIT_OUTSIDE) => {
            let resolved = if detail.is_empty() {
                String::new()
            } else {
                format!(" (it resolves to {detail})")
            };
            format!(
                "{} is outside workspace {} on {}{resolved}. Only a call with full access may name a path outside the workspace.",
                wording.path,
                target.workspace.index,
                machine_name(target)
            )
        }
        Some(EXIT_NOT_FOUND) => format!("No such file or directory: {}", wording.path),
        Some(EXIT_WRONG_KIND) => wording.wrong_kind.clone(),
        Some(EXIT_TOO_LARGE) => wording.too_large.clone(),
        Some(EXIT_CHANGED) => FILE_MODIFIED_SINCE_READ.to_owned(),
        Some(EXIT_BAD_PATTERN) if wording.grep => {
            format!("Invalid regular expression: {detail}")
        }
        status if crate::run_environment::answered_by_non_posix_shell(status, detail) => {
            // The machine's login shell is cmd.exe or PowerShell, so the POSIX
            // line never ran; its own "not recognized" names neither, and in a
            // console code page this host cannot read it says nothing at all.
            let said = crate::run_environment::legible_remote_reply(detail)
                .map(|reply| format!(" The machine said: {reply}"))
                .unwrap_or_default();
            format!(
                "The SSH login shell on {} is not a POSIX shell (the reply came from cmd.exe or PowerShell), so nothing can run in workspace {} until that machine's sshd DefaultShell points at a bash. Tell the user; this is a machine setting, not something a tool call can fix.{said}",
                machine_name(target),
                target.workspace.index
            )
        }
        status => {
            if detail.is_empty() {
                format!("The remote command failed (exit code {status:?})")
            } else {
                detail.to_owned()
            }
        }
    }
}

/// Runs one script and insists on a clean exit.
pub(crate) fn run_script(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    script: &str,
    stdin: Option<&[u8]>,
    timeout: Duration,
    wording: &ExitWording<'_>,
) -> Result<RemoteCommandOutput, String> {
    let output = shell.run(script, stdin, timeout, target.cancel)?;
    if output.status == Some(0) {
        return Ok(output);
    }
    Err(exit_message(target, wording, &output))
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

fn ls_script(target: &RemoteWorkspace<'_>, path: &str, depth: u64) -> Result<String, String> {
    listing_script(target, path, depth, MAX_LIST_ENTRIES)
}

/// The `ls` walk with its cap left open, so a caller that scores the listing rather than
/// showing it can ask for more entries than a person would ever read.
fn listing_script(
    target: &RemoteWorkspace<'_>,
    path: &str,
    depth: u64,
    limit: usize,
) -> Result<String, String> {
    let mut script = prologue(target, path, TargetMode::Existing)?;
    script.push_str(&format!("[ -d \"$C\" ] || exit {EXIT_WRONG_KIND}\n"));
    script.push_str(&format!(
        "{{ find \"$C\" -mindepth 1 -maxdepth {} {MARK_ENTRIES} ; }} 2>/dev/null | head -n {}\n",
        depth + 1,
        limit + 1
    ));
    Ok(script)
}

fn ls_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
) -> Result<String, String> {
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let depth = optional_u64(input, "depth", 1)?;
    if depth > 8 {
        return Err("Recursive depth cannot exceed 8".into());
    }
    let wording = ExitWording::new(&path)
        .wrong_kind(format!("ls target is not a directory: {path}"));
    let output = run_script(
        shell,
        target,
        &ls_script(target, &path, depth)?,
        None,
        SEARCH_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    Ok(render_listing(
        target.profile,
        &header,
        &String::from_utf8_lossy(rest),
        PromptKey::ToolLsLimit,
        PromptKey::ToolLsEmpty,
    ))
}

/// Shared rendering for `ls`: entries relative to the root, capped in the order
/// the remote walked them and then sorted, exactly as the host leg does it.
fn render_listing(
    profile: &PromptProfile,
    header: &Header,
    payload: &str,
    limit_key: PromptKey,
    empty_key: PromptKey,
) -> String {
    let (mut entries, overflowed) = listing_entries(header, payload, MAX_LIST_ENTRIES);
    if overflowed {
        entries.push(profile.render(limit_key, &[("limit", &MAX_LIST_ENTRIES.to_string())]));
    }
    if entries.is_empty() {
        profile.text(empty_key).to_owned()
    } else {
        entries.join("\n")
    }
}

/// The entries of a listing payload, relative to the root and sorted, with whether `limit` cut
/// the walk short. This is what `ls` renders and what [`list_entries`] hands to the scorer.
fn listing_entries(header: &Header, payload: &str, limit: usize) -> (Vec<String>, bool) {
    let mut entries = Vec::new();
    let mut overflowed = false;
    for line in payload.lines().filter(|line| !line.is_empty()) {
        if entries.len() >= limit {
            overflowed = true;
            break;
        }
        let directory = line.ends_with('/');
        let mut display = display_relative(&header.root, line.trim_end_matches('/'));
        if directory {
            display.push('/');
        }
        entries.push(display);
    }
    entries.sort_unstable();
    (entries, overflowed)
}

/// The raw relative entries under `path` on the remote machine, for `find_files`, which scores
/// the listing instead of showing it. Same walk and same directory marking as `ls`; the cap is
/// the scoring cap rather than the reading cap, and nothing is rendered — a "reached the limit"
/// row among the entries would be scored as if it were a path.
pub(crate) fn list_entries(
    target: &RemoteWorkspace<'_>,
    path: &str,
    depth: u64,
) -> Result<Vec<String>, String> {
    list_entries_with(&target.workspace.runner, target, path, depth)
}

fn list_entries_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    path: &str,
    depth: u64,
) -> Result<Vec<String>, String> {
    let limit = crate::decision_tools::listing::MAX_FIND_FILES_ENTRIES;
    let wording =
        ExitWording::new(path).wrong_kind(format!("find_files target is not a directory: {path}"));
    let output = run_script(
        shell,
        target,
        &listing_script(target, path, depth, limit)?,
        None,
        SEARCH_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    Ok(listing_entries(&header, &String::from_utf8_lossy(rest), limit).0)
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

fn grep_script(
    target: &RemoteWorkspace<'_>,
    path: &str,
    pattern: &str,
    case_sensitive: bool,
) -> Result<String, String> {
    let pattern = quote_search_operand(pattern, "pattern")?;
    let case = if case_sensitive { "" } else { " -i" };
    let mut script = prologue(target, path, TargetMode::Existing)?;
    // The host leg matches with Rust's `regex`, whose syntax is Perl-shaped, so
    // a remote grep that speaks PCRE is preferred; exit 2 from the probe means
    // the option is unknown and the extended dialect is the closest match left.
    script.push_str("grep -qP x /dev/null 2>/dev/null\nif [ $? -ne 2 ]; then GP=-P; else GP=-E; fi\n");
    script.push_str(&format!(
        "VERR=$(grep $GP{case} -q -e {pattern} /dev/null 2>&1)\nif [ $? -eq {EXIT_BAD_PATTERN} ]; then printf '%s\\n' \"$VERR\" >&2; exit {EXIT_BAD_PATTERN}; fi\n"
    ));
    // 2049 one-kilobyte blocks is "at most 2 MiB" once `find` has rounded the
    // size up, which is the host leg's own cutoff.
    script.push_str(&format!(
        "if [ -d \"$C\" ]; then\nfind \"$C\" -type f -size -2049k -exec grep $GP{case} -I -n -e {pattern} /dev/null {{}} +\nelse\ngrep $GP{case} -I -n -e {pattern} /dev/null \"$C\"\nfi | head -n {}\n",
        MAX_SEARCH_MATCHES + 1
    ));
    Ok(script)
}

fn grep_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
) -> Result<String, String> {
    let pattern = required_string(input, "pattern", 4096, false)?;
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let case_sensitive = optional_bool(input, "case_sensitive", false)?;
    let wording = ExitWording::new(&path).grep();
    let output = run_script(
        shell,
        target,
        &grep_script(target, &path, &pattern, case_sensitive)?,
        None,
        SEARCH_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    Ok(render_matches(
        target.profile,
        &header,
        &String::from_utf8_lossy(rest),
        &output.stderr,
    ))
}

/// `path:line:text`, with the path relative to the root and the text cut at the
/// same 500 characters the host leg cuts it at.
fn format_match(root: &str, line: &str) -> String {
    let Some((path, rest)) = line.split_once(':') else {
        return truncate_chars(line, 500);
    };
    let Some((number, text)) = rest.split_once(':') else {
        return format!("{}:{}", display_relative(root, path), truncate_chars(rest, 500));
    };
    format!(
        "{}:{number}:{}",
        display_relative(root, path),
        truncate_chars(text, 500)
    )
}

fn render_matches(
    profile: &PromptProfile,
    header: &Header,
    payload: &str,
    stderr: &str,
) -> String {
    let mut matches = Vec::new();
    let mut overflowed = false;
    for line in payload.lines().filter(|line| !line.is_empty()) {
        if matches.len() >= MAX_SEARCH_MATCHES {
            overflowed = true;
            break;
        }
        matches.push(format_match(&header.root, line));
    }
    if overflowed {
        matches.push(profile.render(
            PromptKey::ToolGrepLimit,
            &[("limit", &MAX_SEARCH_MATCHES.to_string())],
        ));
    }
    // Whatever the remote `find`/`grep` could not open is reported the way the
    // host leg reports an unreadable entry, rather than being silently dropped.
    for line in stderr.lines().map(str::trim).filter(|line| !line.is_empty()) {
        matches.push(profile.render(PromptKey::ToolGrepSkipped, &[("error", line)]));
    }
    if matches.is_empty() {
        profile.text(PromptKey::ToolGrepNoMatch).to_owned()
    } else {
        matches.join("\n")
    }
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

/// Whether a glob can be handed to `find -name`, which matches one path
/// component. A query with none of these characters can only ever match a file
/// name, so the remote can pre-filter and the host still applies the real
/// matcher to what comes back.
fn name_only_query(query: &str) -> bool {
    !query.contains('/') && !query.contains("**") && !query.contains('{') && !query.contains('[')
}

fn find_script(target: &RemoteWorkspace<'_>, path: &str, query: &str) -> Result<String, String> {
    let filter = if name_only_query(query) {
        format!(" -name {}", quote_search_operand(query, "query")?)
    } else {
        String::new()
    };
    let mut script = prologue(target, path, TargetMode::Existing)?;
    script.push_str(&format!(
        "{{ find \"$C\" -mindepth 1{filter} {MARK_ENTRIES} ; }} 2>/dev/null | head -n {}\n",
        MAX_SCANNED_ENTRIES + 1
    ));
    Ok(script)
}

fn find_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
) -> Result<String, String> {
    let query = required_string(input, "query", 1024, false)?;
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let matcher = Glob::new(&query)
        .map_err(|error| format!("Invalid glob pattern: {error}"))?
        .compile_matcher();
    let wording = ExitWording::new(&path);
    let output = run_script(
        shell,
        target,
        &find_script(target, &path, &query)?,
        None,
        SEARCH_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    let payload = String::from_utf8_lossy(rest);

    let lines: Vec<&str> = payload.lines().filter(|line| !line.is_empty()).collect();
    let scanned_all = lines.len() <= MAX_SCANNED_ENTRIES;
    let mut found = Vec::new();
    let mut overflowed = false;
    for line in lines.iter().take(MAX_SCANNED_ENTRIES) {
        let directory = line.ends_with('/');
        let absolute = line.trim_end_matches('/');
        // The host leg matches the glob against the path relative to the *find
        // target*, or against the bare file name; both are derivable from the
        // header, so the same query answers the same on either machine.
        let relative = display_relative(&header.canonical, absolute);
        let matched = matcher.is_match(Path::new(&relative))
            || matcher.is_match(Path::new(base_name(absolute)));
        if !matched {
            continue;
        }
        if found.len() >= MAX_LIST_ENTRIES {
            overflowed = true;
            break;
        }
        let mut display = display_relative(&header.root, absolute);
        if directory {
            display.push('/');
        }
        found.push(display);
    }
    found.sort_unstable();
    if overflowed {
        found.push(target.profile.render(
            PromptKey::ToolFindLimit,
            &[("limit", &MAX_LIST_ENTRIES.to_string())],
        ));
    }
    let mut rendered = if found.is_empty() {
        target.profile.text(PromptKey::ToolFindNoMatch).to_owned()
    } else {
        found.join("\n")
    };
    if !scanned_all {
        rendered.push_str(&format!(
            "\n(only the first {MAX_SCANNED_ENTRIES} entries under this path were examined; narrow the path)"
        ));
    }
    Ok(rendered)
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

fn read_script(target: &RemoteWorkspace<'_>, path: &str) -> Result<String, String> {
    let mut script = prologue(target, path, TargetMode::Existing)?;
    script.push_str(&format!("[ -f \"$C\" ] || exit {EXIT_WRONG_KIND}\n"));
    script.push_str("MT=$(digits \"$(mtime \"$C\")\")\nSZ=$(digits \"$(fsize \"$C\")\")\n");
    // The larger of the two caps: the host decides text from image by the first
    // twelve bytes, and an image may be bigger than a text file is allowed to be.
    script.push_str(&format!(
        "if [ \"$SZ\" -gt {MAX_IMAGE_ATTACHMENT_BYTES} ]; then exit {EXIT_TOO_LARGE}; fi\n"
    ));
    script.push_str("printf '%s\\n' \"$MT\"\nprintf '%s\\n' \"$SZ\"\ncat -- \"$C\"\n");
    Ok(script)
}

fn read_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    attachment_store: Option<&ImageAttachmentStore>,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let (start_line, end_line) = parse_read_range(input)?;
    let wording = ExitWording::new(&path)
        .wrong_kind(format!("read target is not a file: {path}"))
        .too_large(format!(
            "File exceeds the {} MiB limit",
            MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
        ));
    let output = run_script(
        shell,
        target,
        &read_script(target, &path)?,
        None,
        FILE_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    let (meta, body) = take_lines(rest, 2)?;
    let modified_ms = seconds_to_ms(&meta[0]);

    if is_supported_image(body) {
        if input.contains_key("start_line") || input.contains_key("end_line") {
            return Err("read does not accept start_line or end_line when reading an image".into());
        }
        if body.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            return Err(format!(
                "Image exceeds the {} MiB limit ({} bytes)",
                MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024,
                body.len()
            ));
        }
        let store = attachment_store.ok_or_else(|| {
            "read requires a trusted image attachment directory to read an image".to_owned()
        })?;
        let image = store.import(base_name(&header.canonical), body)?;
        return Ok(RemoteOutcome {
            output: target.profile.render(
                PromptKey::ToolReadImage,
                &[
                    ("path", &display_relative(&header.root, &header.canonical)),
                    ("mime", &image.mime),
                    ("width", &image.width.to_string()),
                    ("height", &image.height.to_string()),
                    ("bytes", &image.bytes.to_string()),
                ],
            ),
            diff: None,
            images: vec![image],
            file_touch: None,
        });
    }

    if body.len() as u64 > MAX_TEXT_FILE {
        return Err(format!(
            "Text file exceeds the {} MiB limit",
            MAX_TEXT_FILE / 1024 / 1024
        ));
    }
    let content = String::from_utf8(body.to_vec())
        .map_err(|error| format!("Failed to read text file as UTF-8: {error}"))?;
    let slice = slice_text_lines(&content, start_line, end_line, target.profile);
    let record = if slice.whole_file {
        FileReadRecord::full_read(modified_ms, file_read_state::normalize_text(&content))
    } else {
        FileReadRecord::partial_read(modified_ms)
    };
    let key = file_read_state::remote_key(&target.machine_key, &header.canonical);
    Ok(RemoteOutcome {
        output: slice.output,
        diff: None,
        images: Vec::new(),
        // No opened file: nothing on this host answers to a remote path, so the
        // renderer has nothing to open.
        file_touch: file_guard.map(|_| FileGuardTouch {
            path: key,
            read: Some(record),
        }),
    })
}

fn read_text_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    path: &str,
) -> Result<(String, String), String> {
    let wording = ExitWording::new(path)
        .wrong_kind(format!("find_content target is not a file: {path}"))
        .too_large(format!(
            "File exceeds the {} MiB limit",
            MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
        ));
    let output = run_script(
        shell,
        target,
        &read_script(target, path)?,
        None,
        FILE_TIMEOUT,
        &wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    let (_meta, body) = take_lines(rest, 2)?;
    if is_supported_image(body) {
        return Err(format!("{path} is an image; only text files can be scored"));
    }
    if body.len() as u64 > MAX_TEXT_FILE {
        return Err(format!(
            "Text file exceeds the {} MiB limit",
            MAX_TEXT_FILE / 1024 / 1024
        ));
    }
    let content = String::from_utf8(body.to_vec())
        .map_err(|error| format!("Failed to read text file as UTF-8: {error}"))?;
    Ok((display_relative(&header.root, &header.canonical), content))
}

/// The remote clock in whole milliseconds. Seconds are all `stat` promises
/// portably, and the value is only ever compared with another reading of the
/// same clock.
fn seconds_to_ms(seconds: &str) -> i64 {
    seconds
        .trim()
        .parse::<i64>()
        .unwrap_or(0)
        .saturating_mul(1000)
}

// ---------------------------------------------------------------------------
// write and edit
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeState {
    Absent,
    File,
    Other,
}

/// What the first round trip of a write or an edit learned about the target.
struct FileProbe {
    header: Header,
    state: ProbeState,
    modified_ms: i64,
    /// Opaque to the host: the script recomputes it the same way before it
    /// writes, and a mismatch is a file that changed between the two trips.
    /// Modification time alone cannot see a change inside one second, which is
    /// why the checksum is folded in.
    fingerprint: String,
    /// The file's bytes, absent when it is not a file or is past the text cap.
    body: Option<Vec<u8>>,
}

fn probe_script(target: &RemoteWorkspace<'_>, path: &str) -> Result<String, String> {
    let mut script = prologue(target, path, TargetMode::ForWrite)?;
    script.push_str(&format!(
        r#"if [ ! -e "$C" ]; then
printf 'absent\n0\nabsent\nnone\n'
elif [ -f "$C" ]; then
MT=$(digits "$(mtime "$C")")
SZ=$(digits "$(fsize "$C")")
printf 'file\n%s\n%s %s\n' "$MT" "$MT" "$(cksum < "$C")"
if [ "$SZ" -le {MAX_TEXT_FILE} ]; then printf 'text\n'; cat -- "$C"; else printf 'none\n'; fi
else
printf 'other\n0\nother\nnone\n'
fi
"#
    ));
    Ok(script)
}

fn probe_file(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    path: &str,
    wording: &ExitWording<'_>,
) -> Result<FileProbe, String> {
    let output = run_script(
        shell,
        target,
        &probe_script(target, path)?,
        None,
        FILE_TIMEOUT,
        wording,
    )?;
    let (header, rest) = take_header(&output.stdout)?;
    let (meta, rest) = take_lines(rest, 4)?;
    let state = match meta[0].as_str() {
        "absent" => ProbeState::Absent,
        "file" => ProbeState::File,
        _ => ProbeState::Other,
    };
    let fingerprint = meta[2].clone();
    if !fingerprint_is_sane(&fingerprint) {
        return Err("The remote machine reported an unusable file fingerprint".into());
    }
    Ok(FileProbe {
        header,
        state,
        modified_ms: seconds_to_ms(&meta[1]),
        fingerprint,
        body: (meta[3] == "text").then(|| rest.to_vec()),
    })
}

/// The fingerprint goes back into the next script as a quoted operand; it is
/// machine output, not model input, but it is checked anyway so a compromised
/// or merely unusual `cksum` cannot contribute anything but digits.
fn fingerprint_is_sane(fingerprint: &str) -> bool {
    !fingerprint.is_empty()
        && fingerprint.len() <= 128
        && fingerprint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
}

/// The compare-and-swap write: the file must still be exactly what the probe
/// saw, or the write is refused rather than overwriting someone else's change.
fn cas_write_script(
    target: &RemoteWorkspace<'_>,
    path: &str,
    fingerprint: &str,
) -> Result<String, String> {
    let mut script = prologue(target, path, TargetMode::ForWrite)?;
    script.push_str(&format!("FP={}\n", run_environment::sh_single_quote(fingerprint)));
    script.push_str(&format!(
        r#"if [ "$FP" = absent ]; then
if [ -e "$C" ]; then exit {EXIT_CHANGED}; fi
else
if [ ! -f "$C" ]; then exit {EXIT_CHANGED}; fi
CUR="$(digits "$(mtime "$C")") $(cksum < "$C")"
if [ "$CUR" != "$FP" ]; then exit {EXIT_CHANGED}; fi
fi
D=$(dirname -- "$C")
mkdir -p -- "$D" || exit 70
TMP="$D/.mework-write.$$"
cat > "$TMP" || {{ rm -f -- "$TMP"; exit 70; }}
mv -f -- "$TMP" "$C" || {{ rm -f -- "$TMP"; exit 70; }}
printf '%s\n' "$(digits "$(mtime "$C")")"
"#
    ));
    Ok(script)
}

/// Runs the swap and returns the modification time the write left behind.
fn cas_write(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    path: &str,
    fingerprint: &str,
    content: &[u8],
    wording: &ExitWording<'_>,
) -> Result<i64, String> {
    let output = run_script(
        shell,
        target,
        &cas_write_script(target, path, fingerprint)?,
        Some(content),
        FILE_TIMEOUT,
        wording,
    )?;
    let (_, rest) = take_header(&output.stdout)?;
    let (lines, _) = take_lines(rest, 1)?;
    Ok(seconds_to_ms(&lines[0]))
}

/// The read gate of `write` and `edit`, against a record taken on the same
/// machine. Mirrors the host leg's `check_write_gate`, including the stale
/// recovery that lets an edit through when its search text still matches once.
fn check_write_gate(
    guard: &FileGuardContext<'_>,
    key: &Path,
    disk_ms: Option<i64>,
    disk_content: &str,
    find: Option<&str>,
) -> Result<(Option<FileReadRecord>, bool), String> {
    let Some(record) = guard.registry.get(guard.scope, key) else {
        return Err(FILE_NOT_READ.into());
    };
    let Some(disk_ms) = disk_ms else {
        return Ok((Some(record), false));
    };
    if disk_ms <= record.modified_ms {
        return Ok((Some(record), false));
    }
    let normalized = file_read_state::normalize_text(disk_content);
    if record.matches(&normalized) {
        return Ok((Some(record), false));
    }
    if let Some(find) = find {
        let find = file_read_state::normalize_text(find);
        if !find.is_empty() && normalized.matches(find.as_str()).count() == 1 {
            return Ok((Some(record), true));
        }
    }
    Err(FILE_MODIFIED_SINCE_READ.into())
}

fn write_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let content = required_string(input, "content", MAX_WRITE_BYTES, true)?;
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Write content exceeds the {} MiB limit",
            MAX_WRITE_BYTES / 1024 / 1024
        ));
    }
    let wording =
        ExitWording::new(&path).wrong_kind(format!("write target is not a file: {path}"));
    let probe = probe_file(shell, target, &path, &wording)?;
    if probe.state == ProbeState::Other {
        return Err(format!("write target is not a file: {path}"));
    }
    // Diff metadata is best-effort, as it is on the host leg: overwriting a
    // large or non-UTF-8 file stays valid and simply produces no diff.
    let before: Option<(String, bool)> = match probe.state {
        ProbeState::Absent => Some((String::new(), true)),
        _ => probe
            .body
            .as_ref()
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok())
            .map(|text| (text, false)),
    };
    let key = file_read_state::remote_key(&target.machine_key, &probe.header.canonical);
    let mut note = String::new();
    if let Some(guard) = file_guard {
        let existing = match &before {
            Some((existing, false)) => Some(existing.as_str()),
            Some((_, true)) => None,
            None => Some(""),
        };
        if let Some(existing) = existing {
            check_write_gate(&guard, &key, Some(probe.modified_ms), existing, None)?;
        }
        note = write_receipt_note(target.profile, false);
    }
    let written_ms = cas_write(
        shell,
        target,
        &path,
        &probe.fingerprint,
        content.as_bytes(),
        &wording,
    )?;
    let touch = file_guard.map(|guard| {
        // The model wrote every byte, so its copy is the current one.
        guard.registry.record(
            guard.scope,
            key.clone(),
            FileReadRecord::written(
                written_ms,
                file_read_state::normalize_text(&content),
                true,
            ),
        );
        FileGuardTouch {
            path: key.clone(),
            read: None,
        }
    });
    let diff = before.and_then(|(before, created)| unified_diff(&path, &before, &content, created));
    Ok(RemoteOutcome {
        output: format!(
            "{}{note}",
            target.profile.render(
                PromptKey::ToolWriteDone,
                &[("bytes", &content.len().to_string()), ("path", &path)],
            )
        ),
        diff,
        images: Vec::new(),
        file_touch: touch,
    })
}

fn edit_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    input: &JsonObject,
    file_guard: Option<FileGuardContext<'_>>,
) -> Result<RemoteOutcome, String> {
    let path = required_string(input, "path", MAX_PATH_CHARS, false)?;
    let find = required_string(input, "find", MAX_WRITE_BYTES, true)?;
    if find.is_empty() {
        return Err("Parameter find cannot be empty".into());
    }
    let replace = required_string(input, "replace", MAX_WRITE_BYTES, true)?;
    let wording = ExitWording::new(&path).wrong_kind(format!("edit target is not a file: {path}"));
    let probe = probe_file(shell, target, &path, &wording)?;
    match probe.state {
        ProbeState::Absent => return Err(format!("No such file or directory: {path}")),
        ProbeState::Other => return Err(format!("edit target is not a file: {path}")),
        ProbeState::File => {}
    }
    let Some(body) = probe.body.as_ref() else {
        return Err(format!(
            "Text file exceeds the {} MiB limit",
            MAX_TEXT_FILE / 1024 / 1024
        ));
    };
    let content = String::from_utf8(body.clone())
        .map_err(|error| format!("Failed to read text file as UTF-8: {error}"))?;
    let key = file_read_state::remote_key(&target.machine_key, &probe.header.canonical);
    let (previous, stale_recovered) = match file_guard {
        Some(guard) => {
            check_write_gate(&guard, &key, Some(probe.modified_ms), &content, Some(&find))?
        }
        None => (None, false),
    };
    let next = apply_edit(&content, &find, &replace)?;
    if next.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Edited file exceeds the {} MiB limit",
            MAX_WRITE_BYTES / 1024 / 1024
        ));
    }
    let diff = unified_diff(&path, &content, &next, false);
    let written_ms = cas_write(
        shell,
        target,
        &path,
        &probe.fingerprint,
        next.as_bytes(),
        &wording,
    )?;
    let mut note = String::new();
    let touch = file_guard.map(|guard| {
        // After an edit the model knows the file only if it knew it before: a
        // full read it has seen, and no other changes applied on top.
        let in_model_context = !stale_recovered
            && previous
                .as_ref()
                .is_some_and(|record| record.full && record.in_model_context);
        guard.registry.record(
            guard.scope,
            key.clone(),
            FileReadRecord::written(
                written_ms,
                file_read_state::normalize_text(&next),
                in_model_context,
            ),
        );
        note = write_receipt_note(target.profile, stale_recovered);
        FileGuardTouch {
            path: key.clone(),
            read: None,
        }
    });
    Ok(RemoteOutcome {
        output: format!(
            "{}{note}",
            target
                .profile
                .render(PromptKey::ToolEditDone, &[("path", &path)])
        ),
        diff,
        images: Vec::new(),
        file_touch: touch,
    })
}

/// The registry key a remote record is filed under, for callers that need it
/// without running anything.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn record_key(machine_key: &str, canonical_path: &str) -> PathBuf {
    file_read_state::remote_key(machine_key, canonical_path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::file_read_state::{FileReadRegistry, ScopeRef};
    use crate::workspace_set::WorkspaceSet;
    use serde_json::{json, Value};
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // -- pure tests ---------------------------------------------------------

    /// `unwrap_err` needs a `Debug` success type, and `RemoteOutcome` carries a
    /// record type that has none. The refusal is what these tests are after.
    trait Refusal {
        fn refusal(self) -> String;
    }

    impl<T> Refusal for Result<T, String> {
        fn refusal(self) -> String {
            match self {
                Ok(_) => panic!("expected a refusal"),
                Err(error) => error,
            }
        }
    }

    fn workspace_set(root: &str) -> WorkspaceSet {
        WorkspaceSet::local_root(root)
    }

    fn workspace<'a>(
        set: &'a WorkspaceSet,
        profile: &'a PromptProfile,
        cancel: &'a CancelSignal,
        confinement: Confinement,
    ) -> RemoteWorkspace<'a> {
        RemoteWorkspace {
            workspace: set.primary().expect("one workspace"),
            machine_key: "wsl:Ubuntu".to_owned(),
            confinement,
            profile,
            cancel,
        }
    }

    /// The file tools against a real Windows machine over SSH, through the
    /// agent: Git Bash runs the same scripts a Unix machine does, in a
    /// workspace rooted at a Windows path the way the directory picker
    /// records one — whatever shell the account logs in with. Set
    /// `MEWORK_E2E_SSH_WINDOWS_HOST` and run with `--ignored`; the agent is
    /// installed from `src-tauri/remote-agents/`.
    #[test]
    #[ignore]
    fn over_real_ssh_the_file_tools_work_on_a_windows_workspace() {
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let app_data = tempfile::tempdir().unwrap();
        crate::remote_link::install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            host,
            port: 0,
            identity_file: String::new(),
            env: Default::default(),
        };
        let cancel = CancelSignal::default();
        let home = runner
            .run("cygpath -m ~", None, FILE_TIMEOUT, &cancel)
            .unwrap();
        let home = String::from_utf8_lossy(&home.stdout).trim().to_owned();
        assert!(home.contains(":/"), "{home}");
        let root = format!("{home}/mework-e2e-files");
        let quoted = run_environment::sh_single_quote(&root);
        let reset = runner
            .run(&format!("rm -rf -- {quoted} && mkdir -p -- {quoted}"), None, FILE_TIMEOUT, &cancel)
            .unwrap();
        assert_eq!(reset.status, Some(0), "{}", reset.stderr);

        let set = WorkspaceSet::single(root.clone(), runner.clone());
        let profile = PromptProfile::default();
        let target = RemoteWorkspace {
            workspace: set.primary().expect("one workspace"),
            machine_key: "ssh:e2e".to_owned(),
            confinement: Confinement::Workspace,
            profile: &profile,
            cancel: &cancel,
        };
        run_write(
            &target,
            &input(json!({"path": "notes/hello.txt", "content": "hello 中文\nsecond line\n"})),
            None,
        )
        .unwrap();
        let read = run_read(&target, &input(json!({"path": "notes/hello.txt"})), None, None).unwrap();
        assert!(read.output.contains("hello 中文"), "{}", read.output);
        let edited = run_edit(
            &target,
            &input(json!({"path": "notes/hello.txt", "find": "second", "replace": "2nd"})),
            None,
        )
        .unwrap();
        assert!(edited.diff.as_deref().is_some_and(|diff| diff.contains("+2nd line")), "{:?}", edited.diff);
        let listing = run_ls(&target, &input(json!({"depth": 2}))).unwrap();
        assert!(listing.contains("notes/hello.txt"), "{listing}");
        let grep = run_grep(&target, &input(json!({"pattern": "2nd"}))).unwrap();
        assert!(grep.contains("hello.txt"), "{grep}");
        let found = run_find(&target, &input(json!({"query": "*.txt"}))).unwrap();
        assert!(found.contains("notes/hello.txt"), "{found}");
        let beside = run_environment::sh_single_quote(&format!("{home}/mework-e2e-outside.txt"));
        runner.run(&format!("echo secret > {beside}"), None, FILE_TIMEOUT, &cancel).unwrap();
        let outside = run_read(&target, &input(json!({"path": "../mework-e2e-outside.txt"})), None, None).refusal();
        assert!(outside.contains("outside workspace"), "{outside}");

        let cleaned = runner
            .run(&format!("rm -rf -- {quoted} {beside}"), None, FILE_TIMEOUT, &cancel)
            .unwrap();
        assert_eq!(cleaned.status, Some(0));
        crate::remote_link::shutdown();
    }

    #[test]
    fn every_host_supplied_fragment_reaches_the_script_quoted() {
        let set = workspace_set("~/my projects");
        let profile = PromptProfile::default();
        let cancel = CancelSignal::default();
        let target = workspace(&set, &profile, &cancel, Confinement::Workspace);
        let script = ls_script(&target, "it's here", 1).unwrap();
        // A bare `~` stays outside the quotes so the remote shell expands it;
        // everything after it is quoted.
        assert!(script.contains("cd -- ~/'my projects' || exit 64"), "{script}");
        assert!(script.contains(r#"REQ='it'\''s here'"#), "{script}");
        assert!(script.contains("case \"$C\" in"), "confinement is compiled in");
    }

    #[test]
    fn an_unconfined_call_skips_the_root_test_and_a_confined_one_does_not() {
        let set = workspace_set("/srv/app");
        let profile = PromptProfile::default();
        let cancel = CancelSignal::default();
        let confined = workspace(&set, &profile, &cancel, Confinement::Workspace);
        assert!(ls_script(&confined, ".", 1).unwrap().contains("exit 65"));
        let free = workspace(&set, &profile, &cancel, Confinement::Machine);
        assert!(!ls_script(&free, ".", 1).unwrap().contains("exit 65"));
    }

    #[test]
    fn a_path_that_could_rewrite_the_script_never_reaches_it() {
        let set = workspace_set("/srv/app");
        let profile = PromptProfile::default();
        let cancel = CancelSignal::default();
        let target = workspace(&set, &profile, &cancel, Confinement::Workspace);
        assert!(ls_script(&target, "", 1).is_err());
        assert!(ls_script(&target, "a\nrm -rf /", 1).is_err());
        assert!(ls_script(&target, "a\u{0}b", 1).is_err());
        assert!(ls_script(&target, &"x".repeat(MAX_PATH_CHARS + 1), 1).is_err());
    }

    #[test]
    fn a_pattern_may_hold_anything_a_regular_expression_may() {
        assert_eq!(
            quote_search_operand(r"fn\s+\w+", "pattern").unwrap(),
            r"'fn\s+\w+'"
        );
        assert!(quote_search_operand("a\u{0}b", "pattern").is_err());
    }

    #[test]
    fn the_header_is_split_off_a_payload_that_is_not_text() {
        let mut answer = b"/srv/app\n/srv/app/logo.png\n".to_vec();
        answer.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let (header, rest) = take_header(&answer).unwrap();
        assert_eq!(header.root, "/srv/app");
        assert_eq!(header.canonical, "/srv/app/logo.png");
        assert_eq!(rest[0], 0x89, "the payload keeps its raw bytes");
        assert!(take_header(b"/srv/app\n").is_err());
    }

    #[test]
    fn entries_are_shown_relative_to_the_root_with_directories_marked() {
        let profile = PromptProfile::default();
        let header = Header {
            root: "/srv/app".into(),
            canonical: "/srv/app".into(),
        };
        let rendered = render_listing(
            &profile,
            &header,
            "/srv/app/src/\n/srv/app/src/main.rs\n/etc/passwd\n",
            PromptKey::ToolLsLimit,
            PromptKey::ToolLsEmpty,
        );
        // Sorted, root-relative, and a path outside the root keeps its absolute
        // spelling — which only an unconfined call can produce.
        assert_eq!(rendered, "/etc/passwd\nsrc/\nsrc/main.rs");
        assert_eq!(
            render_listing(
                &profile,
                &header,
                "",
                PromptKey::ToolLsLimit,
                PromptKey::ToolLsEmpty
            ),
            profile.text(PromptKey::ToolLsEmpty)
        );
    }

    #[test]
    fn an_overflowing_listing_says_so_on_its_last_line() {
        let profile = PromptProfile::default();
        let header = Header {
            root: "/r".into(),
            canonical: "/r".into(),
        };
        let payload: String = (0..MAX_LIST_ENTRIES + 5)
            .map(|index| format!("/r/f{index:06}\n"))
            .collect();
        let rendered = render_listing(
            &profile,
            &header,
            &payload,
            PromptKey::ToolLsLimit,
            PromptKey::ToolLsEmpty,
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), MAX_LIST_ENTRIES + 1);
        assert_eq!(
            lines[MAX_LIST_ENTRIES],
            profile.render(
                PromptKey::ToolLsLimit,
                &[("limit", &MAX_LIST_ENTRIES.to_string())]
            )
        );
    }

    #[test]
    fn a_match_is_relative_and_its_text_is_cut_at_five_hundred_characters() {
        let long = "x".repeat(600);
        let formatted = format_match("/srv/app", &format!("/srv/app/src/a.rs:12:{long}"));
        assert!(formatted.starts_with("src/a.rs:12:"), "{formatted}");
        assert_eq!(formatted.chars().count(), "src/a.rs:12:".len() + 501);
        assert!(formatted.ends_with('…'));
    }

    #[test]
    fn unreadable_entries_are_reported_rather_than_dropped() {
        let profile = PromptProfile::default();
        let header = Header {
            root: "/r".into(),
            canonical: "/r".into(),
        };
        let rendered = render_matches(&profile, &header, "", "find: '/r/x': Permission denied\n");
        assert_eq!(
            rendered,
            profile.render(
                PromptKey::ToolGrepSkipped,
                &[("error", "find: '/r/x': Permission denied")]
            )
        );
        assert_eq!(
            render_matches(&profile, &header, "", ""),
            profile.text(PromptKey::ToolGrepNoMatch)
        );
    }

    #[test]
    fn only_a_single_component_query_is_pushed_down_to_find() {
        assert!(name_only_query("*.txt"));
        assert!(!name_only_query("sub/**/*.txt"));
        assert!(!name_only_query("**"));
        assert!(!name_only_query("a{b,c}"));
    }

    #[test]
    fn a_fingerprint_is_only_ever_digits_and_spaces() {
        assert!(fingerprint_is_sane("1737 2919 12"));
        assert!(fingerprint_is_sane("absent"));
        assert!(!fingerprint_is_sane(""));
        assert!(!fingerprint_is_sane("1737; rm -rf /"));
        assert!(!fingerprint_is_sane("1'2"));
    }

    #[test]
    fn a_remote_record_is_keyed_by_machine_as_well_as_path() {
        assert_ne!(
            record_key("wsl:Ubuntu", "/srv/app/a.txt"),
            record_key("ssh:m1", "/srv/app/a.txt")
        );
        assert!(file_read_state::is_remote_key(&record_key(
            "wsl:Ubuntu",
            "/srv/app/a.txt"
        )));
    }

    #[test]
    fn the_exit_table_speaks_for_every_reserved_code() {
        let set = workspace_set("/srv/app");
        let profile = PromptProfile::default();
        let cancel = CancelSignal::default();
        let target = workspace(&set, &profile, &cancel, Confinement::Workspace);
        let wording = ExitWording::new("../secret")
            .wrong_kind("ls target is not a directory: ../secret".into())
            .grep();
        let answer = |status: i32, stderr: &str| RemoteCommandOutput {
            status: Some(status),
            stdout: Vec::new(),
            stderr: stderr.to_owned(),
        };
        assert!(exit_message(&target, &wording, &answer(EXIT_ROOT_MISSING, ""))
            .contains("Workspace 1"));
        let outside = exit_message(&target, &wording, &answer(EXIT_OUTSIDE, "/etc/secret"));
        assert!(outside.contains("outside workspace 1"), "{outside}");
        assert!(outside.contains("full access"), "{outside}");
        assert!(outside.contains("/etc/secret"), "{outside}");
        assert!(exit_message(&target, &wording, &answer(EXIT_NOT_FOUND, ""))
            .starts_with("No such file or directory"));
        assert_eq!(
            exit_message(&target, &wording, &answer(EXIT_WRONG_KIND, "")),
            "ls target is not a directory: ../secret"
        );
        assert!(exit_message(&target, &wording, &answer(EXIT_TOO_LARGE, ""))
            .contains("2 MiB limit"));
        assert_eq!(
            exit_message(&target, &wording, &answer(EXIT_CHANGED, "")),
            FILE_MODIFIED_SINCE_READ
        );
        assert_eq!(
            exit_message(&target, &wording, &answer(EXIT_BAD_PATTERN, "trailing backslash")),
            "Invalid regular expression: trailing backslash"
        );
        assert_eq!(
            exit_message(&target, &wording, &answer(3, "cannot open")),
            "cannot open"
        );
        // A Windows machine still logging in through cmd.exe never ran the
        // script; the model is told whose setting that is, with the raw reply.
        let cmd = exit_message(
            &target,
            &wording,
            &answer(
                1,
                "'exec' is not recognized as an internal or external command, operable program or batch file."
            ),
        );
        assert!(cmd.contains("not a POSIX shell"), "{cmd}");
        assert!(cmd.contains("DefaultShell"), "{cmd}");
        assert!(cmd.ends_with("batch file."), "{cmd}");
        // The same reply in GBK is still recognized, and its unreadable text
        // is left out rather than handed to the model.
        let gbk = String::from_utf8_lossy(b"'exec' \xb2\xbb\xca\xc7\xc4\xda\xb2\xbf\r\n");
        let cmd = exit_message(&target, &wording, &answer(1, &gbk));
        assert!(cmd.contains("not a POSIX shell"), "{cmd}");
        assert!(cmd.ends_with("a tool call can fix."), "{cmd}");
    }

    // -- integration through a local Bash -----------------------------------

    /// The same scripts the WSL and SSH legs run, executed by a Bash on this
    /// machine. It stands in for the transport only: nothing about the scripts
    /// is host-specific, so a POSIX shell here answers what a POSIX shell there
    /// would.
    pub(crate) struct LocalBash {
        executable: String,
    }

    impl LocalBash {
        pub(crate) fn find() -> Option<Self> {
            run_environment::local_bash_candidates()
                .into_iter()
                .next()
                .map(|executable| Self { executable })
        }
    }

    impl RemoteShell for LocalBash {
        fn run(
            &self,
            script: &str,
            stdin: Option<&[u8]>,
            _timeout: Duration,
            _cancel: &CancelSignal,
        ) -> Result<RemoteCommandOutput, String> {
            let mut child = Command::new(&self.executable)
                .args(["--noprofile", "--norc", "-c", script])
                .stdin(if stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| format!("failed to start bash: {error}"))?;
            let writer = match (child.stdin.take(), stdin) {
                (Some(mut pipe), Some(bytes)) => {
                    let bytes = bytes.to_vec();
                    Some(std::thread::spawn(move || {
                        let _ = pipe.write_all(&bytes);
                    }))
                }
                (pipe, _) => {
                    drop(pipe);
                    None
                }
            };
            let output = child
                .wait_with_output()
                .map_err(|error| format!("failed to read bash output: {error}"))?;
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            Ok(RemoteCommandOutput {
                status: output.status.code(),
                stdout: output.stdout,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    /// A workspace root that bash can enter, plus the POSIX spelling bash gives
    /// it. On Windows the temporary directory is `C:\...`; MSYS Bash accepts the
    /// forward-slash form and reports its own mount path back.
    pub(crate) struct Fixture {
        _root: tempfile::TempDir,
        pub(crate) shell: LocalBash,
        pub(crate) workspace: String,
        pub(crate) posix_root: String,
    }

    pub(crate) fn fixture() -> Option<Fixture> {
        let Some(shell) = LocalBash::find() else {
            println!("no bash on this machine; skipping the remote-file integration tests");
            return None;
        };
        let root = tempfile::tempdir().expect("temp dir");
        let workspace_dir = root.path().join("ws");
        std::fs::create_dir_all(&workspace_dir).expect("workspace");
        let workspace = workspace_dir.to_string_lossy().replace('\\', "/");
        let posix_root = shell
            .run(
                &format!(
                    "cd -- {} && pwd -P",
                    run_environment::sh_single_quote(&workspace)
                ),
                None,
                FILE_TIMEOUT,
                &CancelSignal::default(),
            )
            .expect("bash resolves the workspace root");
        let posix_root = String::from_utf8_lossy(&posix_root.stdout).trim().to_owned();
        assert!(!posix_root.is_empty(), "bash reported no root");
        Some(Fixture {
            _root: root,
            shell,
            workspace,
            posix_root,
        })
    }

    pub(crate) fn write_fixture_file(fixture: &Fixture, relative: &str, content: &[u8]) {
        let path = std::path::Path::new(&fixture.workspace).join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture parent");
        }
        std::fs::write(path, content).expect("fixture file");
    }

    fn input(value: Value) -> JsonObject {
        value.as_object().expect("object").clone()
    }

    pub(crate) struct Harness {
        set: WorkspaceSet,
        profile: PromptProfile,
        cancel: CancelSignal,
    }

    impl Harness {
        pub(crate) fn new(fixture: &Fixture) -> Self {
            Self {
                set: WorkspaceSet::local_root(fixture.workspace.clone()),
                profile: PromptProfile::default(),
                cancel: CancelSignal::default(),
            }
        }

        pub(crate) fn target(&self, confinement: Confinement) -> RemoteWorkspace<'_> {
            RemoteWorkspace {
                workspace: self.set.primary().expect("one workspace"),
                machine_key: "wsl:Ubuntu".to_owned(),
                confinement,
                profile: &self.profile,
                cancel: &self.cancel,
            }
        }
    }

    #[test]
    fn ls_lists_entries_relative_to_the_root_and_honours_depth() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "a.txt", b"alpha\n");
        write_fixture_file(&fixture, "sub/b.txt", b"beta\n");
        write_fixture_file(&fixture, "sub/deep/c.txt", b"gamma\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        let shallow = ls_with(&fixture.shell, &target, &input(json!({}))).unwrap();
        // `depth` counts the levels below the target the way the host leg's
        // walker counts them: 1 reaches into each immediate subdirectory.
        assert_eq!(shallow, "a.txt\nsub/\nsub/b.txt\nsub/deep/", "{shallow}");

        let deep = ls_with(&fixture.shell, &target, &input(json!({"depth": 2}))).unwrap();
        assert!(deep.contains("sub/deep/c.txt"), "{deep}");

        let scoped = ls_with(&fixture.shell, &target, &input(json!({"path": "sub"}))).unwrap();
        assert_eq!(scoped, "sub/b.txt\nsub/deep/\nsub/deep/c.txt", "{scoped}");

        let not_a_directory =
            ls_with(&fixture.shell, &target, &input(json!({"path": "a.txt"}))).refusal();
        assert_eq!(not_a_directory, "ls target is not a directory: a.txt");
        assert!(ls_with(&fixture.shell, &target, &input(json!({"depth": 9}))).is_err());
    }

    /// `find_files` walks with the same script and the same relative spelling, but takes the
    /// entries rather than the rendering: nothing is sorted into a "limit reached" row, and
    /// nothing stands in for an empty directory, because both would be scored as if they were
    /// paths.
    #[test]
    fn list_entries_hands_back_the_paths_ls_would_have_rendered() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "a.txt", b"alpha\n");
        write_fixture_file(&fixture, "sub/deep/c.txt", b"gamma\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        let entries = list_entries_with(&fixture.shell, &target, ".", 6).unwrap();
        assert_eq!(entries, vec!["a.txt", "sub/", "sub/deep/", "sub/deep/c.txt"]);
        // Depth counts levels below the target exactly as `ls` counts them.
        let shallow = list_entries_with(&fixture.shell, &target, ".", 0).unwrap();
        assert_eq!(shallow, vec!["a.txt", "sub/"]);
        let empty = list_entries_with(&fixture.shell, &target, "sub/deep", 0).unwrap();
        assert_eq!(empty, vec!["sub/deep/c.txt"]);
        let not_a_directory = list_entries_with(&fixture.shell, &target, "a.txt", 1).refusal();
        assert_eq!(not_a_directory, "find_files target is not a directory: a.txt");
    }

    #[test]
    fn grep_is_case_insensitive_by_default_and_reports_relative_matches() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "src/a.rs", b"fn Alpha() {}\nfn beta() {}\n");
        write_fixture_file(&fixture, "src/b.rs", b"nothing here\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        let loose = grep_with(&fixture.shell, &target, &input(json!({"pattern": "alpha"}))).unwrap();
        assert_eq!(loose, "src/a.rs:1:fn Alpha() {}", "{loose}");

        let strict = grep_with(
            &fixture.shell,
            &target,
            &input(json!({"pattern": "alpha", "case_sensitive": true})),
        )
        .unwrap();
        assert_eq!(strict, harness.profile.text(PromptKey::ToolGrepNoMatch));

        let invalid = grep_with(
            &fixture.shell,
            &target,
            &input(json!({"pattern": "a[", "case_sensitive": true})),
        )
        .refusal();
        assert!(
            invalid.starts_with("Invalid regular expression:"),
            "{invalid}"
        );
    }

    #[test]
    fn find_matches_the_same_globs_the_host_leg_matches() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "a.txt", b"a");
        write_fixture_file(&fixture, "sub/b.txt", b"b");
        write_fixture_file(&fixture, "sub/c.md", b"c");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        let text = find_with(&fixture.shell, &target, &input(json!({"query": "*.txt"}))).unwrap();
        assert_eq!(text, "a.txt\nsub/b.txt", "{text}");

        let nested =
            find_with(&fixture.shell, &target, &input(json!({"query": "sub/**"}))).unwrap();
        assert!(nested.contains("sub/b.txt"), "{nested}");
        assert!(nested.contains("sub/c.md"), "{nested}");
        assert!(!nested.contains("a.txt"), "{nested}");

        let none = find_with(&fixture.shell, &target, &input(json!({"query": "*.rs"}))).unwrap();
        assert_eq!(none, harness.profile.text(PromptKey::ToolFindNoMatch));
    }

    #[test]
    fn read_records_a_full_file_and_only_remembers_a_slice_of_a_range() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "notes.txt", b"one\ntwo\nthree\n");
        write_fixture_file(&fixture, "binary.bin", &[b'a', 0xFF, 0xFE, b'b']);
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let registry = FileReadRegistry::default();
        let guard = FileGuardContext {
            scope: ScopeRef {
                id: "c1",
                parent: None,
            },
            registry: &registry,
        };
        let key = record_key(&target.machine_key, &format!("{}/notes.txt", fixture.posix_root));

        let full = read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "notes.txt"})),
            None,
            Some(guard),
        )
        .unwrap();
        assert_eq!(full.output, "one\ntwo\nthree");
        let touch = full.file_touch.expect("a guarded read leaves a record");
        assert_eq!(touch.path, key);
        let record = touch.read.expect("a read records what it saw");
        assert!(record.full, "a whole-file read vouches for the file");
        assert!(record.modified_ms > 0, "the remote clock was read");

        let ranged = read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "notes.txt", "start_line": 2, "end_line": 2})),
            None,
            Some(guard),
        )
        .unwrap();
        assert_eq!(ranged.output, "two");
        assert!(
            !ranged
                .file_touch
                .and_then(|touch| touch.read)
                .expect("record")
                .full,
            "a ranged read vouches for nothing"
        );

        let past_end = read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "notes.txt", "start_line": 40})),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            past_end.output,
            harness.profile.text(PromptKey::ToolReadRangeOutOfBounds)
        );

        let binary = read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "binary.bin"})),
            None,
            None,
        )
        .refusal();
        assert!(binary.contains("UTF-8"), "{binary}");

        let missing = read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "nope.txt"})),
            None,
            None,
        )
        .refusal();
        assert_eq!(missing, "No such file or directory: nope.txt");
    }

    #[test]
    fn write_creates_parents_and_is_gated_on_what_the_conversation_read() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "existing.txt", b"before\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let registry = FileReadRegistry::default();
        let guard = FileGuardContext {
            scope: ScopeRef {
                id: "c1",
                parent: None,
            },
            registry: &registry,
        };

        // A new file needs no prior read, and its parents are made on the way.
        let created = write_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "made/up/new.txt", "content": "fresh\n"})),
            Some(guard),
        )
        .unwrap();
        assert!(created.diff.is_some(), "a created file still carries a diff");
        assert_eq!(
            std::fs::read_to_string(
                std::path::Path::new(&fixture.workspace).join("made/up/new.txt")
            )
            .unwrap(),
            "fresh\n"
        );

        // An existing file the conversation never read is refused.
        let refused = write_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "existing.txt", "content": "after\n"})),
            Some(guard),
        )
        .refusal();
        assert_eq!(refused, FILE_NOT_READ);

        read_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "existing.txt"})),
            None,
            Some(guard),
        )
        .unwrap()
        .file_touch
        .map(|touch| {
            registry.record(guard.scope, touch.path, touch.read.expect("record"));
        })
        .expect("a guarded read hands back a record");

        let written = write_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "existing.txt", "content": "after\n"})),
            Some(guard),
        )
        .unwrap();
        assert!(
            written
                .output
                .contains(harness.profile.text(PromptKey::ToolFileStateCurrent)),
            "{}",
            written.output
        );
        let key = record_key(
            &target.machine_key,
            &format!("{}/existing.txt", fixture.posix_root),
        );
        let record = registry.get(guard.scope, &key).expect("the write recorded");
        assert!(record.matches("after\n"), "the record holds what was written");
    }

    #[test]
    fn edit_applies_once_and_keeps_the_file_s_own_line_endings() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "crlf.txt", b"alpha\r\nbeta\r\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        let edited = edit_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "crlf.txt", "find": "beta", "replace": "gamma"})),
            None,
        )
        .unwrap();
        assert!(edited.diff.is_some());
        assert_eq!(
            std::fs::read(std::path::Path::new(&fixture.workspace).join("crlf.txt")).unwrap(),
            b"alpha\r\ngamma\r\n",
            "a CRLF file stays CRLF"
        );
        assert_eq!(
            edited.output,
            harness.profile.render(PromptKey::ToolEditDone, &[("path", "crlf.txt")]),
            "an unguarded edit adds no receipt note"
        );

        let missing = edit_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "crlf.txt", "find": "nowhere", "replace": "x"})),
            None,
        )
        .refusal();
        assert_eq!(missing, "The exact text to replace was not found");

        let directory = edit_with(
            &fixture.shell,
            &target,
            &input(json!({"path": ".", "find": "a", "replace": "b"})),
            None,
        )
        .refusal();
        assert_eq!(directory, "edit target is not a file: .");
    }

    #[test]
    fn a_file_changed_behind_the_guard_is_refused_unless_the_search_text_still_matches() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "drift.txt", b"one\ntwo\n");
        write_fixture_file(&fixture, "rescue.txt", b"keep\nanchor\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let registry = FileReadRegistry::default();
        let guard = FileGuardContext {
            scope: ScopeRef {
                id: "c1",
                parent: None,
            },
            registry: &registry,
        };
        for name in ["drift.txt", "rescue.txt"] {
            let touch = read_with(
                &fixture.shell,
                &target,
                &input(json!({ "path": name })),
                None,
                Some(guard),
            )
            .unwrap()
            .file_touch
            .expect("a guarded read hands back a record");
            registry.record(guard.scope, touch.path, touch.read.expect("record"));
        }

        // `stat` promises whole seconds, so the file has to land in a later one
        // for the change to be visible at all — which is exactly the window the
        // compare-and-swap fingerprint closes for everything finer than this.
        std::thread::sleep(Duration::from_millis(1_100));
        write_fixture_file(&fixture, "drift.txt", b"something else entirely\n");
        write_fixture_file(&fixture, "rescue.txt", b"rewritten\nanchor\n");

        let refused = edit_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "drift.txt", "find": "two", "replace": "three"})),
            Some(guard),
        )
        .refusal();
        assert_eq!(refused, FILE_MODIFIED_SINCE_READ);

        let recovered = edit_with(
            &fixture.shell,
            &target,
            &input(json!({"path": "rescue.txt", "find": "anchor", "replace": "moored"})),
            Some(guard),
        )
        .unwrap();
        assert!(
            recovered
                .output
                .contains(harness.profile.text(PromptKey::ToolEditStaleRecovered)),
            "{}",
            recovered.output
        );
        let key = record_key(
            &target.machine_key,
            &format!("{}/rescue.txt", fixture.posix_root),
        );
        let record = registry.get(guard.scope, &key).expect("the edit recorded");
        assert!(
            !record.in_model_context,
            "a stale-recovered edit leaves the model holding a copy that is not current"
        );
    }

    #[test]
    fn confinement_refuses_what_lies_outside_the_root_until_a_call_has_full_access() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "inside.txt", b"inside\n");
        // A sibling of the workspace root, inside the temporary directory that
        // is cleaned up with it.
        let outside = std::path::Path::new(&fixture.workspace)
            .parent()
            .expect("parent")
            .join("outside.txt");
        std::fs::write(&outside, b"outside\n").expect("outside file");
        let absolute = format!(
            "{}/outside.txt",
            fixture
                .posix_root
                .rsplit_once('/')
                .map(|(head, _)| head)
                .unwrap_or("")
        );

        let harness = Harness::new(&fixture);
        let confined = harness.target(Confinement::Workspace);
        for path in ["../outside.txt", absolute.as_str()] {
            let refused = read_with(
                &fixture.shell,
                &confined,
                &input(json!({ "path": path })),
                None,
                None,
            )
            .refusal();
            assert!(refused.contains("outside workspace 1"), "{refused}");
            assert!(refused.contains("full access"), "{refused}");
        }

        let free = harness.target(Confinement::Machine);
        for path in ["../outside.txt", absolute.as_str()] {
            let allowed = read_with(
                &fixture.shell,
                &free,
                &input(json!({ "path": path })),
                None,
                None,
            )
            .unwrap();
            assert_eq!(allowed.output, "outside", "{path}");
        }
    }

    #[test]
    fn a_write_whose_fingerprint_no_longer_matches_is_refused_rather_than_applied() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "cas.txt", b"original\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);

        // The real race — a writer between the two round trips — cannot be
        // injected from here, so the swap is run directly with the fingerprint
        // such a writer would have invalidated.
        let stale = cas_write_script(&target, "cas.txt", "1 2 3").unwrap();
        let answer = fixture
            .shell
            .run(&stale, Some(b"replaced\n"), FILE_TIMEOUT, &CancelSignal::default())
            .unwrap();
        assert_eq!(answer.status, Some(EXIT_CHANGED));
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&fixture.workspace).join("cas.txt"))
                .unwrap(),
            "original\n",
            "a refused swap leaves the file alone"
        );

        // The same script with the fingerprint the probe actually took goes
        // through, and reports the time it left behind.
        let probe = probe_file(
            &fixture.shell,
            &target,
            "cas.txt",
            &ExitWording::new("cas.txt"),
        )
        .unwrap();
        assert_eq!(probe.state, ProbeState::File);
        let written = cas_write(
            &fixture.shell,
            &target,
            "cas.txt",
            &probe.fingerprint,
            b"replaced\n",
            &ExitWording::new("cas.txt"),
        )
        .unwrap();
        assert!(written > 0);
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&fixture.workspace).join("cas.txt"))
                .unwrap(),
            "replaced\n"
        );

        // A file that was absent when probed but exists by the time the swap
        // runs is the same refusal from the other side.
        let absent = cas_write_script(&target, "later.txt", "absent").unwrap();
        write_fixture_file(&fixture, "later.txt", b"someone else\n");
        let answer = fixture
            .shell
            .run(&absent, Some(b"mine\n"), FILE_TIMEOUT, &CancelSignal::default())
            .unwrap();
        assert_eq!(answer.status, Some(EXIT_CHANGED));
    }
}
