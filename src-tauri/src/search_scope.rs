//! What the file tools leave out, and how they say so.
//!
//! Claude Code answers "should a search see ignored files?" per tool rather
//! than once, and `ls`, `grep` and `find` follow it:
//!
//! * `grep` searches what Git would show — tracked files, and untracked ones
//!   that are not ignored. A hit under `target/` or `node_modules/` is noise.
//! * `ls` lists an ignored directory but does not expand it. The model learns
//!   `node_modules/` exists without reading forty thousand entries of it.
//! * `find` hides nothing, because a name lookup is often after exactly the
//!   ignored file — `.env`, a generated header. Ignored matches are listed
//!   last and marked, so they cannot crowd the rest off the first page.
//!
//! "Ignored" is Git's own answer, from `git ls-files`, on this machine and on
//! a remote one alike, so a query answers the same on either. Two cases have
//! no Git answer:
//!
//! * The directory a call names is itself ignored. The model asked for it by
//!   name, so nothing below it is hidden: `ls target/debug` lists
//!   `target/debug`. This also covers a home directory kept as a dotfiles
//!   repository whose `.gitignore` is `*`, where every project below would
//!   otherwise read as ignored — and where `git ls-files --directory` fails
//!   outright.
//! * No Git work tree, or no Git. Then the directory names in
//!   [`DEPENDENCY_DIRECTORIES`] stand in for an ignore file.
//!
//! Version-control metadata (`.git` and its kin) is never walked into by any
//! of the three, whatever the rules say.
//!
//! The renderers here are shared by the host leg ([`crate::tool_executor`])
//! and the remote one ([`crate::remote_files`]), so a listing reads the same
//! whichever machine it came from.

use std::{
    collections::HashSet,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde_json::Value;
use wait_timeout::ChildExt;

use crate::{
    model::JsonObject,
    prompt_profile::{PromptKey, PromptProfile},
};

/// Version-control metadata directories, after Claude Code's `grep`
/// exclusions. Listed, never entered.
pub(crate) const VCS_DIRECTORIES: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// What stands in for an ignore file outside a Git work tree: directory
/// names that hold installed dependencies or build output.
///
/// Claude Code's old `LS` tool kept a list like this. `packages`, `bin` and
/// `env` are left off it: each is as often a project's own source (a
/// monorepo's `packages/`, a scripts `bin/`, an `env/` of configuration) as
/// something to skip. Inside a Git work tree this list is not consulted.
pub(crate) const DEPENDENCY_DIRECTORIES: &[&str] = &[
    "node_modules",
    "bower_components",
    "vendor",
    "venv",
    ".venv",
    ".tox",
    "__pycache__",
    "target",
    "build",
    "_build",
    ".build",
    "dist",
    "dist-newstyle",
    "obj",
    "deps",
    ".gradle",
    ".dart_tool",
    ".pub-cache",
    ".deno",
    ".next",
    ".nuxt",
    ".svelte-kit",
];

/// How many characters of entries one `ls` returns. Claude Code's `LS` used
/// the same figure; the walk is breadth-first, so what the budget cuts is the
/// deepest level reached, never a shallower one.
pub(crate) const LS_BUDGET_CHARS: usize = 40_000;

/// Lines of a remote `ls` the host pulls over. The machine sorts its listing
/// by depth before cutting it, so these are the shallowest entries; every
/// rendered entry costs at least two characters of the budget, so the budget
/// always binds first.
pub(crate) const LS_REMOTE_LINES: usize = LS_BUDGET_CHARS / 2 + 1;

/// Matches one `find` returns, with the total. Claude Code's `Glob` figure.
pub(crate) const FIND_LIMIT: usize = 100;

/// Entries a `find` examines before it stops counting. The total it reports
/// is then a floor, and the result says so.
pub(crate) const FIND_SCAN_LIMIT: usize = 200_000;

/// `grep`'s page when the call names none, and the most one call may ask
/// for. Claude Code's `Grep` defaults to 250 as well.
pub(crate) const GREP_DEFAULT_LIMIT: usize = 250;
pub(crate) const GREP_MAX_LIMIT: usize = 1_000;
/// How far into the matches a page may start. A search is re-run for every
/// page, so a deep offset is a long walk to throw most of away.
pub(crate) const GREP_MAX_OFFSET: usize = 100_000;

pub(crate) fn is_vcs_directory(name: &str) -> bool {
    VCS_DIRECTORIES.contains(&name)
}

pub(crate) fn is_dependency_directory(name: &str) -> bool {
    DEPENDENCY_DIRECTORIES.contains(&name)
}

// ---------------------------------------------------------------------------
// Ignore rules
// ---------------------------------------------------------------------------

/// The ignored entries below one directory, as Git lists them: paths
/// relative to that directory, `/`-separated. A directory stands for
/// everything under it.
#[derive(Debug, Default)]
pub(crate) struct IgnoredSet(HashSet<String>);

impl IgnoredSet {
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = String>) -> Self {
        Self(
            entries
                .into_iter()
                .map(|entry| {
                    let entry = entry.trim_start_matches("./");
                    entry.trim_end_matches('/').to_owned()
                })
                .filter(|entry| !entry.is_empty() && entry != ".")
                .collect(),
        )
    }

    /// Whether `relative` or any directory above it was listed.
    pub(crate) fn covers(&self, relative: &str) -> bool {
        let mut prefix = relative;
        loop {
            if self.0.contains(prefix) {
                return true;
            }
            match prefix.rfind('/') {
                Some(end) => prefix = &prefix[..end],
                None => return false,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// Which rule decides what is ignored below one directory.
#[derive(Debug)]
pub(crate) enum IgnoreRules {
    /// Git's own answer for this work tree.
    Git(IgnoredSet),
    /// The directory was named although it is itself ignored: nothing below
    /// it is hidden.
    NamedRoot,
    /// No Git answer: [`DEPENDENCY_DIRECTORIES`] stand in.
    DependencyNames,
}

impl IgnoreRules {
    /// Whether the entry at `relative` — below the directory the rules were
    /// taken for, `/`-separated — is ignored. For a file only the directories
    /// above it count under the name list: a script called `build` is not a
    /// build directory.
    pub(crate) fn ignores(&self, relative: &str, is_dir: bool) -> bool {
        match self {
            Self::Git(set) => set.covers(relative),
            Self::NamedRoot => false,
            Self::DependencyNames => {
                let mut components = relative.split('/').collect::<Vec<_>>();
                if !is_dir {
                    components.pop();
                }
                components.into_iter().any(is_dependency_directory)
            }
        }
    }

    /// Whether a directory the walk reached is left unexpanded: version
    /// control always, anything else by the rules.
    pub(crate) fn collapses(&self, relative: &str) -> bool {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        is_vcs_directory(name) || self.ignores(relative, true)
    }
}

/// What `grep` searches below one directory.
pub(crate) enum SearchFiles {
    /// The files Git would show, relative to the directory, in Git's order.
    Listed(Vec<String>),
    /// No list: walk the directory, skipping what the rules collapse.
    Walk(IgnoreRules),
}

/// The rules for `root` on this machine. Anything short of a clean answer
/// from Git — no Git, not a work tree, a repository Git refuses to touch —
/// falls back to the name list rather than failing the call.
pub(crate) fn local_rules(root: &Path) -> IgnoreRules {
    match root_ignore_state(root) {
        RootState::Ignored => IgnoreRules::NamedRoot,
        RootState::Outside => IgnoreRules::DependencyNames,
        RootState::Tracked => match git(
            root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "--",
                ".",
            ],
        ) {
            Some(GitAnswer {
                code: Some(0),
                stdout,
            }) => IgnoreRules::Git(IgnoredSet::from_entries(split_nul(&stdout))),
            _ => IgnoreRules::DependencyNames,
        },
    }
}

/// What `grep` searches below `root` on this machine.
pub(crate) fn local_search_files(root: &Path) -> SearchFiles {
    match root_ignore_state(root) {
        RootState::Ignored => SearchFiles::Walk(IgnoreRules::NamedRoot),
        RootState::Outside => SearchFiles::Walk(IgnoreRules::DependencyNames),
        RootState::Tracked => match git(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                ".",
            ],
        ) {
            Some(GitAnswer {
                code: Some(0),
                stdout,
            }) => {
                let mut files = split_nul(&stdout);
                // An unmerged path is listed once per stage, and the stages
                // are adjacent in Git's order.
                files.dedup();
                SearchFiles::Listed(files)
            }
            _ => SearchFiles::Walk(IgnoreRules::DependencyNames),
        },
    }
}

enum RootState {
    /// Inside a work tree and not ignored.
    Tracked,
    /// Inside a work tree and ignored.
    Ignored,
    /// No work tree, no Git, or no answer in time.
    Outside,
}

fn root_ignore_state(root: &Path) -> RootState {
    match git(root, &["check-ignore", "-q", "."]).and_then(|answer| answer.code) {
        Some(0) => RootState::Ignored,
        Some(1) => RootState::Tracked,
        _ => RootState::Outside,
    }
}

/// Whether a directory on the way from `root` to `relative` is a link (or,
/// on Windows, a junction). `seen` remembers the answer per directory, since
/// a file list asks about the same few over and over.
pub(crate) fn has_linked_parent(
    root: &Path,
    relative: &str,
    seen: &mut std::collections::HashMap<String, bool>,
) -> bool {
    let Some((parent, _)) = relative.rsplit_once('/') else {
        return false;
    };
    if let Some(linked) = seen.get(parent) {
        return *linked;
    }
    let linked = has_linked_parent(root, parent, seen)
        || std::fs::symlink_metadata(root.join(parent)).map_or(true, |metadata| is_link(&metadata));
    seen.insert(parent.to_owned(), linked);
    linked
}

#[cfg(not(windows))]
fn is_link(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

/// Git's answers are cheap (`ls-files` over a 34 GB `target/` takes
/// milliseconds), so a slow one means something is wrong, and the name list
/// is the better answer than a stalled tool.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// A file list this long is a monorepo the walk handles as well.
const GIT_MAX_OUTPUT: usize = 32 * 1024 * 1024;

/// Variables that would point Git at some other repository than the one the
/// directory is in.
const GIT_ENVIRONMENT: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

struct GitAnswer {
    code: Option<i32>,
    stdout: Vec<u8>,
}

/// One read-only Git command in `root`, or `None` when there is no Git, it
/// could not start, it overran the time or output cap.
///
/// `core.fsmonitor` is forced off: a repository's own configuration can name
/// a monitor command, and listing a checkout must not run anything it
/// brought along.
fn git(root: &Path, args: &[&str]) -> Option<GitAnswer> {
    let program = crate::environment_tools::resolve_on_path("git")?;
    let mut command = Command::new(program);
    command
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotepath=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in GIT_ENVIRONMENT {
        command.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Drained on its own thread so a long list cannot fill the pipe while the
    // wait below is parked; past the cap it keeps draining and discards.
    let reader = thread::spawn(move || {
        let mut kept = Vec::new();
        let mut overflowed = false;
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if kept.len() + read > GIT_MAX_OUTPUT {
                        overflowed = true;
                    } else {
                        kept.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
        (kept, overflowed)
    });
    let status = match child.wait_timeout(GIT_TIMEOUT) {
        Ok(Some(status)) => status,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
    };
    let (stdout, overflowed) = reader.join().ok()?;
    if overflowed {
        return None;
    }
    Some(GitAnswer {
        code: status.code(),
        stdout,
    })
}

// ---------------------------------------------------------------------------
// Remote answers
// ---------------------------------------------------------------------------

/// Splits the ignore section off a remote `ls` or `find` answer: the mode
/// line, the ignored entries one per line as `git ls-files` prints them, then
/// an empty line.
pub(crate) fn take_remote_rules(bytes: &[u8]) -> Result<(IgnoreRules, &[u8]), String> {
    let incomplete = || "The remote machine returned an incomplete result".to_owned();
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(incomplete)?;
    let mode = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let mut rest = &bytes[end + 1..];
    let mut entries = Vec::new();
    loop {
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(incomplete)?;
        let line = &rest[..end];
        rest = &rest[end + 1..];
        if line.is_empty() {
            break;
        }
        entries.push(unquote_git_path(&String::from_utf8_lossy(line)));
    }
    let rules = match mode.trim() {
        "git" => IgnoreRules::Git(IgnoredSet::from_entries(entries)),
        "root" => IgnoreRules::NamedRoot,
        "names" => IgnoreRules::DependencyNames,
        other => {
            return Err(format!(
                "The remote machine reported an unknown ignore mode: {other}"
            ))
        }
    };
    Ok((rules, rest))
}

/// A path as `git ls-files` prints it without `-z`: bare, or — when it holds
/// a control character, a quote or a backslash — C-quoted. `core.quotepath`
/// is off, so bytes past ASCII arrive as they are.
pub(crate) fn unquote_git_path(line: &str) -> String {
    let Some(inner) = line
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return line.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        if byte != b'\\' || index >= bytes.len() {
            out.push(byte);
            continue;
        }
        let escaped = bytes[index];
        index += 1;
        match escaped {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'0'..=b'7' => {
                let mut value = u32::from(escaped - b'0');
                for _ in 0..2 {
                    match bytes.get(index) {
                        Some(digit @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(digit - b'0');
                            index += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `path` relative to `base`, `/`-separated, for the ignore rules: both are
/// canonical spellings from the same machine.
pub(crate) fn relative_below(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    path.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
        .trim_end_matches('/')
        .to_owned()
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

/// An `ls` answer under construction: entries arrive breadth-first, and the
/// budget cuts the deepest level reached.
pub(crate) struct Listing {
    budget: usize,
    used: usize,
    entries: Vec<(String, String)>,
    level: usize,
    /// The level the budget (or the machine's line cap) cut into.
    cut_at: Option<usize>,
    collapsed: usize,
    skipped: Vec<String>,
}

impl Listing {
    pub(crate) fn new() -> Self {
        Self {
            budget: LS_BUDGET_CHARS,
            used: 0,
            entries: Vec::new(),
            level: 0,
            cut_at: None,
            collapsed: 0,
            skipped: Vec::new(),
        }
    }

    /// Adds one entry at `level` (1 is directly inside the listed directory).
    /// `false` once the budget is spent, and the caller stops walking.
    pub(crate) fn push(
        &mut self,
        level: usize,
        display: &str,
        is_dir: bool,
        collapsed: bool,
        profile: &PromptProfile,
    ) -> bool {
        if self.cut_at.is_some() {
            return false;
        }
        self.level = self.level.max(level);
        let key = if is_dir {
            format!("{display}/")
        } else {
            display.to_owned()
        };
        let line = if collapsed {
            profile.render(PromptKey::ToolIgnoredEntry, &[("path", &key)])
        } else {
            key.clone()
        };
        let cost = line.chars().count() + 1;
        if self.used + cost > self.budget {
            self.cut_at = Some(level);
            return false;
        }
        self.used += cost;
        if collapsed {
            self.collapsed += 1;
        }
        self.entries.push((key, line));
        true
    }

    /// The listing's source stopped at `level` rather than the budget: a
    /// remote machine's line cap.
    pub(crate) fn cut_by_source(&mut self, level: usize) {
        if self.cut_at.is_none() {
            self.cut_at = Some(level);
        }
    }

    /// A directory below the listed one that could not be read.
    pub(crate) fn skipped(&mut self, error: String) {
        self.skipped.push(error);
    }

    pub(crate) fn render(mut self, profile: &PromptProfile) -> String {
        self.entries
            .sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut lines = self
            .entries
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>();
        if lines.is_empty() && self.cut_at.is_none() && self.skipped.is_empty() {
            return profile.text(PromptKey::ToolLsEmpty).to_owned();
        }
        let limit = self.budget.to_string();
        match self.cut_at {
            Some(level) if level > 1 => lines.push(profile.render(
                PromptKey::ToolLsLimit,
                &[("limit", &limit), ("depth", &(level - 2).to_string())],
            )),
            Some(_) => {
                lines.push(profile.render(PromptKey::ToolLsLimitPartial, &[("limit", &limit)]))
            }
            None => {}
        }
        if self.collapsed > 0 {
            lines.push(profile.text(PromptKey::ToolLsIgnoredNote).to_owned());
        }
        lines.extend(skipped_lines(self.skipped, profile));
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

/// A `find` answer under construction. Every match is kept so the total is
/// exact; the page is chosen once they are all in.
#[derive(Default)]
pub(crate) struct FindMatches {
    visible: Vec<String>,
    ignored: Vec<String>,
    scan_cut: bool,
}

impl FindMatches {
    /// One match, spelled as the model sees it (directories already marked).
    pub(crate) fn push(&mut self, display: String, ignored: bool) {
        if ignored {
            self.ignored.push(display);
        } else {
            self.visible.push(display);
        }
    }

    /// The walk stopped at [`FIND_SCAN_LIMIT`] entries.
    pub(crate) fn scan_cut(&mut self) {
        self.scan_cut = true;
    }

    pub(crate) fn render(mut self, profile: &PromptProfile) -> String {
        self.visible.sort_unstable();
        self.ignored.sort_unstable();
        let total = self.visible.len() + self.ignored.len();
        let ignored_total = self.ignored.len();
        let mut lines = self
            .visible
            .into_iter()
            .take(FIND_LIMIT)
            .collect::<Vec<_>>();
        let room = FIND_LIMIT - lines.len();
        lines.extend(
            self.ignored
                .into_iter()
                .take(room)
                .map(|path| profile.render(PromptKey::ToolIgnoredEntry, &[("path", &path)])),
        );
        let shown = lines.len();
        if lines.is_empty() {
            lines.push(profile.text(PromptKey::ToolFindNoMatch).to_owned());
        }
        if total > shown {
            lines.push(profile.render(
                PromptKey::ToolFindLimit,
                &[("shown", &shown.to_string()), ("total", &total.to_string())],
            ));
        }
        if ignored_total > 0 {
            lines.push(profile.render(
                PromptKey::ToolFindIgnoredNote,
                &[("count", &ignored_total.to_string())],
            ));
        }
        if self.scan_cut {
            lines.push(profile.render(
                PromptKey::ToolFindScanLimit,
                &[("limit", &FIND_SCAN_LIMIT.to_string())],
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// Which slice of the matches one `grep` call returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GrepPage {
    pub offset: usize,
    pub limit: usize,
}

impl GrepPage {
    pub(crate) fn from_input(input: &JsonObject) -> Result<Self, String> {
        let limit = match input.get("limit") {
            None | Some(Value::Null) => GREP_DEFAULT_LIMIT,
            Some(value) => {
                let limit = value
                    .as_u64()
                    .ok_or("Parameter limit must be a positive integer")?;
                if limit == 0 || limit > GREP_MAX_LIMIT as u64 {
                    return Err(format!(
                        "Parameter limit must be between 1 and {GREP_MAX_LIMIT}"
                    ));
                }
                limit as usize
            }
        };
        let offset = match input.get("offset") {
            None | Some(Value::Null) => 0,
            Some(value) => {
                let offset = value
                    .as_u64()
                    .ok_or("Parameter offset must be a non-negative integer")?;
                if offset > GREP_MAX_OFFSET as u64 {
                    return Err(format!("Parameter offset cannot exceed {GREP_MAX_OFFSET}"));
                }
                offset as usize
            }
        };
        Ok(Self { offset, limit })
    }

    /// How many matches a search must collect: the page, everything before
    /// it, and one more to know whether another page follows.
    pub(crate) fn wanted(&self) -> usize {
        self.offset + self.limit + 1
    }

    /// The page out of `matches` (in search order, at most [`Self::wanted`]),
    /// then the entries that could not be read.
    pub(crate) fn render(
        &self,
        matches: Vec<String>,
        skipped: Vec<String>,
        profile: &PromptProfile,
    ) -> String {
        let found = matches.len();
        let more = found > self.offset + self.limit;
        let mut lines = matches
            .into_iter()
            .skip(self.offset)
            .take(self.limit)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(if self.offset > 0 && found > 0 {
                profile.render(
                    PromptKey::ToolGrepNoMatchAtOffset,
                    &[
                        ("offset", &self.offset.to_string()),
                        ("count", &found.to_string()),
                    ],
                )
            } else {
                profile.text(PromptKey::ToolGrepNoMatch).to_owned()
            });
        } else if more {
            let to = self.offset + self.limit;
            lines.push(profile.render(
                PromptKey::ToolGrepLimit,
                &[
                    ("from", &(self.offset + 1).to_string()),
                    ("to", &to.to_string()),
                    ("next", &to.to_string()),
                ],
            ));
        }
        lines.extend(skipped_lines(skipped, profile));
        lines.join("\n")
    }
}

/// How many unreadable entries a result names one by one.
const SKIPPED_SHOWN: usize = 20;

/// The `[skipped]` lines of a result: the first few by name, the rest
/// counted.
fn skipped_lines(skipped: Vec<String>, profile: &PromptProfile) -> Vec<String> {
    let more = skipped.len().saturating_sub(SKIPPED_SHOWN);
    let mut lines = skipped
        .into_iter()
        .take(SKIPPED_SHOWN)
        .map(|error| profile.render(PromptKey::ToolGrepSkipped, &[("error", &error)]))
        .collect::<Vec<_>>();
    if more > 0 {
        lines.push(profile.render(
            PromptKey::ToolGrepSkipped,
            &[("error", &format!("… and {more} more"))],
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn git_available() -> bool {
        crate::environment_tools::resolve_on_path("git").is_some()
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn a_listed_directory_covers_everything_below_it() {
        let set = IgnoredSet::from_entries(["target/".to_owned(), "./notes.log".to_owned()]);
        assert!(set.covers("target"));
        assert!(set.covers("target/debug/build"));
        assert!(set.covers("notes.log"));
        assert!(!set.covers("targets"));
        assert!(!set.covers("src/target.rs"));
    }

    #[test]
    fn the_name_list_judges_directories_by_every_component_and_files_by_their_parents() {
        let rules = IgnoreRules::DependencyNames;
        assert!(rules.ignores("node_modules", true));
        assert!(rules.ignores("web/node_modules/react/index.js", false));
        assert!(
            !rules.ignores("scripts/build", false),
            "a file named build is not a build directory"
        );
        assert!(rules.ignores("scripts/build", true));
        assert!(!rules.ignores("packages/app/src", true));
        assert!(!IgnoreRules::NamedRoot.ignores("node_modules", true));
        assert!(
            IgnoreRules::NamedRoot.collapses("sub/.git"),
            "version control is never expanded"
        );
    }

    #[test]
    fn git_quoting_is_undone() {
        assert_eq!(unquote_git_path("plain/path.txt"), "plain/path.txt");
        assert_eq!(unquote_git_path(r#""tab\there""#), "tab\there");
        assert_eq!(
            unquote_git_path(r#""quote\"and\\slash""#),
            "quote\"and\\slash"
        );
        assert_eq!(unquote_git_path(r#""\346\226\207""#), "文");
    }

    #[test]
    fn the_remote_section_carries_the_mode_and_the_entries() {
        let answer = b"git\ntarget/\n\"odd\\tname/\"\n\nrest\n";
        let (rules, rest) = take_remote_rules(answer).unwrap();
        assert_eq!(rest, b"rest\n");
        match rules {
            IgnoreRules::Git(set) => {
                assert!(set.covers("target/debug"));
                assert!(set.covers("odd\tname"));
            }
            other => panic!("{other:?}"),
        }
        let (rules, rest) = take_remote_rules(b"names\n\n").unwrap();
        assert!(matches!(rules, IgnoreRules::DependencyNames));
        assert!(rest.is_empty());
        assert!(
            take_remote_rules(b"git\ntarget/\n").is_err(),
            "no terminator"
        );
    }

    #[test]
    fn git_decides_inside_a_work_tree_and_a_named_ignored_root_hides_nothing() {
        if !git_available() {
            return;
        }
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(repo.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(repo.path().join("src/run.log"), "noise\n").unwrap();
        std::fs::write(repo.path().join("target/debug/out"), "noise\n").unwrap();

        match local_rules(repo.path()) {
            IgnoreRules::Git(set) => {
                assert!(set.covers("target/debug/out"));
                assert!(set.covers("src/run.log"));
                assert!(!set.covers("src/main.rs"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            local_rules(&repo.path().join("target")),
            IgnoreRules::NamedRoot
        ));
        match local_search_files(repo.path()) {
            SearchFiles::Listed(files) => {
                assert!(files.contains(&"src/main.rs".to_owned()), "{files:?}");
                assert!(files.contains(&".gitignore".to_owned()), "{files:?}");
                assert!(
                    !files.iter().any(|file| file.starts_with("target")),
                    "{files:?}"
                );
                assert!(
                    !files.iter().any(|file| file.ends_with(".log")),
                    "{files:?}"
                );
            }
            SearchFiles::Walk(rules) => panic!("{rules:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_linked_directory_is_found_out() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("real/deeper")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("real/link")).unwrap();
        let mut seen = std::collections::HashMap::new();
        assert!(!has_linked_parent(root.path(), "top.txt", &mut seen));
        assert!(!has_linked_parent(
            root.path(),
            "real/deeper/a.txt",
            &mut seen
        ));
        assert!(has_linked_parent(
            root.path(),
            "real/link/secret.txt",
            &mut seen
        ));
        assert!(has_linked_parent(
            root.path(),
            "real/link/more/secret.txt",
            &mut seen
        ));
        // A directory that is gone is not walked through either.
        assert!(has_linked_parent(root.path(), "vanished/a.txt", &mut seen));
    }

    #[test]
    fn a_dotfiles_home_does_not_hide_the_projects_below_it() {
        if !git_available() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        run_git(home.path(), &["init", "-q"]);
        std::fs::write(home.path().join(".gitignore"), "*\n!.bashrc\n").unwrap();
        let project = home.path().join("projects/app");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/lib.rs"), "\n").unwrap();
        assert!(matches!(local_rules(&project), IgnoreRules::NamedRoot));
        assert!(matches!(
            local_search_files(&project),
            SearchFiles::Walk(IgnoreRules::NamedRoot)
        ));
    }

    #[test]
    fn outside_a_work_tree_the_name_list_stands_in() {
        let plain = tempfile::tempdir().unwrap();
        // A temporary directory could sit inside someone's repository; only
        // assert when it does not.
        if git_available()
            && Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(plain.path())
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|output| output.status.success())
        {
            return;
        }
        assert!(matches!(
            local_rules(plain.path()),
            IgnoreRules::DependencyNames
        ));
    }

    #[test]
    fn a_listing_cut_by_its_budget_names_the_depth_that_is_complete() {
        let profile = PromptProfile::builtin_english();
        let mut listing = Listing::new();
        listing.budget = 30;
        assert!(listing.push(1, "a", true, false, &profile));
        assert!(listing.push(1, "b.txt", false, false, &profile));
        assert!(listing.push(2, "a/one.txt", false, false, &profile));
        assert!(!listing.push(2, "a/two-with-a-long-name.txt", false, false, &profile));
        let rendered = listing.render(&profile);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(&lines[..3], &["a/", "a/one.txt", "b.txt"]);
        assert!(lines[3].contains("depth 0"), "{rendered}");
    }

    #[test]
    fn a_collapsed_directory_is_marked_and_explained_once() {
        let profile = PromptProfile::builtin_english();
        let mut listing = Listing::new();
        listing.push(1, "node_modules", true, true, &profile);
        listing.push(1, "target", true, true, &profile);
        listing.push(1, "src", true, false, &profile);
        let rendered = listing.render(&profile);
        assert!(
            rendered.starts_with("node_modules/ (ignored)\nsrc/\ntarget/ (ignored)\n"),
            "{rendered}"
        );
        assert_eq!(rendered.matches("not expanded").count(), 1, "{rendered}");
    }

    #[test]
    fn find_lists_ignored_matches_after_the_rest_and_reports_the_total() {
        let profile = PromptProfile::builtin_english();
        let mut matches = FindMatches::default();
        for index in 0..(FIND_LIMIT + 20) {
            matches.push(format!("node_modules/pkg{index:03}/index.js"), true);
        }
        matches.push("src/index.js".to_owned(), false);
        let rendered = matches.render(&profile);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "src/index.js");
        assert_eq!(lines[1], "node_modules/pkg000/index.js (ignored)");
        assert!(
            rendered.contains(&format!("{FIND_LIMIT} of {}", FIND_LIMIT + 21)),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("{} of the matches", FIND_LIMIT + 20)),
            "{rendered}"
        );
    }

    #[test]
    fn a_grep_page_says_where_the_next_one_starts() {
        let profile = PromptProfile::builtin_english();
        let page = GrepPage::from_input(
            &json!({"limit": 2, "offset": 1})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(page.wanted(), 4);
        let matches = (1..=4).map(|n| format!("a.rs:{n}:x")).collect::<Vec<_>>();
        let rendered = page.render(matches, Vec::new(), &profile);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(&lines[..2], &["a.rs:2:x", "a.rs:3:x"]);
        assert!(lines[2].contains("offset=3"), "{rendered}");

        let past = GrepPage {
            offset: 10,
            limit: 5,
        };
        let rendered = past.render(vec!["a.rs:1:x".into()], Vec::new(), &profile);
        assert!(rendered.contains("offset 10"), "{rendered}");

        assert!(GrepPage::from_input(&json!({"limit": 0}).as_object().unwrap().clone()).is_err());
        assert!(
            GrepPage::from_input(&json!({"limit": 1001}).as_object().unwrap().clone()).is_err()
        );
        assert_eq!(
            GrepPage::from_input(&JsonObject::new()).unwrap(),
            GrepPage {
                offset: 0,
                limit: GREP_DEFAULT_LIMIT
            }
        );
    }
}
