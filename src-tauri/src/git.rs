use std::{
    collections::{hash_map::RandomState, HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    fs,
    hash::{BuildHasher, Hasher},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, OnceLock, Weak},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use crate::host_platform::is_uninstalled_developer_tool_shim;

const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const BULK_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const MAX_STATUS_OUTPUT: usize = 16 * 1024 * 1024;
// Diff 上下文的钳制只约束 `--unified=` 这个参数本身，不约束输出规模；
// 真正的输出上限仍由 MAX_PATCH_OUTPUT 兜底（超出时以 truncated 报告）。
const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MAX_DIFF_CONTEXT: u32 = 100_000;
const MAX_PATCH_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_JSON_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_ACTION_OUTPUT: usize = 256 * 1024;
const MAX_PATHS_PER_ACTION: usize = 10_000;
const MAX_PATH_BYTES_PER_ACTION: usize = 1024 * 1024;
const DEFAULT_CHANGE_PAGE_LIMIT: u16 = 200;
const MAX_CHANGE_PAGE_LIMIT: u16 = 500;
const MAX_CHANGE_QUERY_BYTES: usize = 2 * 1024;
const MAX_CHANGE_CURSOR_BYTES: usize = 256;
const MAX_GIT_BISECT_TERM_BYTES: usize = 1024;
const MAX_GIT_REMOTES: usize = 64;
const MAX_GIT_REMOTE_URLS: usize = 32;
const MAX_GIT_REMOTE_REFSPECS: usize = 64;
const MAX_GIT_REMOTE_VALUE_BYTES: usize = 64 * 1024;
const MAX_GIT_REMOTE_CONFIG_OUTPUT: usize = 512 * 1024;
const DISCARD_TARGET_REVISION_TIMEOUT: Duration = Duration::from_secs(5);
const DISCARD_HASH_OBJECT_ARG_BUDGET: usize = 16 * 1024;
const MAX_OPERATION_REVISION_ARTIFACTS: usize = 1_024;
const MAX_OPERATION_REVISION_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPERATION_REVISION_FILE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorkspaceSnapshot {
    pub repository_id: String,
    pub worktree_id: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub content_revision: String,
    pub upstream: Option<String>,
    pub upstream_target: Option<GitUpstream>,
    pub ahead: u32,
    pub behind: u32,
    pub additions: u64,
    pub deletions: u64,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub stash: u32,
    pub files: Vec<GitFileChange>,
    pub remote: Option<GitRemote>,
    pub remotes: Vec<GitRemote>,
    pub git_version: String,
    pub repository_root: String,
    pub worktree_root: String,
    pub detached: bool,
    pub unborn: bool,
    pub operation: Option<GitRepositoryOperation>,
    pub operation_revision: Option<String>,
    pub is_clean: bool,
    pub binary_files: u32,
    pub warnings: Vec<String>,
    pub summary_revision: String,
    pub changed_files: u32,
    pub stageable: u32,
    pub unstageable: u32,
    pub files_complete: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorkspaceSummary {
    pub repository_id: String,
    pub worktree_id: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub content_revision: String,
    pub summary_revision: String,
    pub upstream: Option<String>,
    pub upstream_target: Option<GitUpstream>,
    pub ahead: u32,
    pub behind: u32,
    pub additions: u64,
    pub deletions: u64,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub stash: u32,
    pub changed_files: u32,
    pub stageable: u32,
    pub unstageable: u32,
    pub remote: Option<GitRemote>,
    pub remotes: Vec<GitRemote>,
    pub git_version: String,
    pub repository_root: String,
    pub worktree_root: String,
    pub detached: bool,
    pub unborn: bool,
    pub operation: Option<GitRepositoryOperation>,
    pub operation_revision: Option<String>,
    pub is_clean: bool,
    pub binary_files: u32,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GitWorkspaceSummaryResult {
    NotRepository,
    Unchanged { revision: String },
    Snapshot { summary: GitWorkspaceSummary },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitChangePageRequest {
    pub expected_revision: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_change_page_limit")]
    pub limit: u16,
    #[serde(default)]
    pub selected_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum GitChangeSelection {
    Present { file: GitFileChange },
    FilteredOut,
    Missing,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GitChangePageResult {
    Stale {
        summary: GitWorkspaceSummary,
    },
    Page {
        revision: String,
        files: Vec<GitFileChange>,
        matched_count: u32,
        next_cursor: Option<String>,
        selection: Option<GitChangeSelection>,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiscardPreparation {
    pub snapshot: GitWorkspaceSnapshot,
    pub target_revision: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchState {
    pub head: Option<String>,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub detached: bool,
    pub unborn: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
    pub original_path: Option<String>,
    pub status: GitFileStatus,
    pub index_status: String,
    pub worktree_status: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflicted: bool,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
    pub submodule: bool,
    pub submodule_commit_changed: bool,
    pub submodule_modified: bool,
    pub submodule_untracked: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Unmerged,
    Ignored,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitRepositoryOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitBisectOutcome {
    Old,
    New,
    Skip,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitLineStats {
    pub additions: u64,
    pub deletions: u64,
    pub binary_files: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitRemote {
    pub name: String,
    pub fetch_revision: String,
    pub push_revision: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitUpstream {
    pub remote_name: String,
    pub remote_branch: String,
    pub merge_ref: String,
    pub tracking_ref: String,
    pub tracking_oid: Option<String>,
    pub is_local: bool,
    pub remote: GitRemote,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GitDiffRequest {
    Working {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        context: Option<u32>,
    },
    Staged {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        context: Option<u32>,
    },
    Unstaged {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        context: Option<u32>,
    },
    Compare {
        base: String,
        head: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        context: Option<u32>,
    },
}

fn resolved_diff_context(request: &GitDiffRequest) -> u32 {
    let requested = match request {
        GitDiffRequest::Working { context, .. }
        | GitDiffRequest::Staged { context, .. }
        | GitDiffRequest::Unstaged { context, .. }
        | GitDiffRequest::Compare { context, .. } => *context,
    };
    requested
        .unwrap_or(DEFAULT_DIFF_CONTEXT)
        .clamp(0, MAX_DIFF_CONTEXT)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffResponse {
    pub path: Option<String>,
    pub patch: String,
    pub truncated: bool,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
    pub files: Vec<GitFileChange>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranch {
    pub name: String,
    pub full_name: String,
    pub kind: GitBranchKind,
    pub current: bool,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub merged: Option<bool>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitBranchKind {
    Local,
    Remote,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchesResult {
    pub branches: Vec<GitBranch>,
    pub default_branch: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GitAction {
    Stage {
        paths: Vec<String>,
    },
    Unstage {
        paths: Vec<String>,
    },
    Discard {
        paths: Vec<String>,
        #[serde(default)]
        include_untracked: bool,
        expected_content_revision: String,
        expected_target_revision: String,
    },
    Checkout {
        branch: String,
    },
    ContinueOperation {
        operation: GitRepositoryOperation,
        expected_head: String,
        expected_operation_revision: String,
    },
    SkipOperation {
        operation: GitRepositoryOperation,
        expected_head: String,
        expected_operation_revision: String,
    },
    AbortOperation {
        operation: GitRepositoryOperation,
        expected_head: String,
        expected_operation_revision: String,
    },
    BisectStep {
        outcome: GitBisectOutcome,
        expected_head: String,
        expected_operation_revision: String,
        expected_content_revision: String,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitActionResult {
    pub message: Option<String>,
    pub snapshot: Option<GitWorkspaceSnapshot>,
}

#[derive(Clone)]
struct Repository {
    root: PathBuf,
    git_dir: PathBuf,
    git_common_dir: PathBuf,
    repository_id: String,
    worktree_id: String,
    git: PathBuf,
    git_version: String,
}

struct RepositoryOperationState {
    operation: GitRepositoryOperation,
    revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteTransport {
    proof: GitRemote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamAtoms {
    local_ref: String,
    local_oid: String,
    tracking_ref: String,
    tracking_short: String,
    remote_name: String,
    merge_ref: String,
}

struct CliOutput {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_sha256: [u8; 32],
    timed_out: bool,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl CliOutput {
    fn success(&self) -> bool {
        !self.timed_out && self.status.is_some_and(|status| status.success())
    }

    fn exit_code(&self) -> Option<i32> {
        self.status.and_then(|status| status.code())
    }

    fn display_output(&self) -> String {
        let mut output = String::new();
        if !self.stdout.is_empty() {
            output.push_str(&String::from_utf8_lossy(&self.stdout));
        }
        if !self.stderr.is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&String::from_utf8_lossy(&self.stderr));
        }
        if self.stdout_truncated || self.stderr_truncated {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("… 命令输出已截断");
        }
        if self.timed_out {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("命令执行超时，已终止");
        }
        redact_sensitive_text(output.trim())
    }
}

fn default_change_page_limit() -> u16 {
    DEFAULT_CHANGE_PAGE_LIMIT
}

#[cfg(test)]
pub fn workspace_snapshot(workspace: &Path) -> Result<Option<GitWorkspaceSnapshot>, String> {
    let Some(repository) = discover_repository(workspace)? else {
        return Ok(None);
    };
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot_for_repository(&repository).map(Some)
}

pub fn workspace_summary(
    workspace: &Path,
    known_revision: Option<String>,
) -> Result<GitWorkspaceSummaryResult, String> {
    let Some(repository) = discover_repository(workspace)? else {
        return Ok(GitWorkspaceSummaryResult::NotRepository);
    };
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = snapshot_for_repository(&repository)?;
    summary_result(&snapshot, known_revision)
}

/// The summary answer for `snapshot`: `Unchanged` when the caller already
/// holds its revision.
pub(crate) fn summary_result(
    snapshot: &GitWorkspaceSnapshot,
    known_revision: Option<String>,
) -> Result<GitWorkspaceSummaryResult, String> {
    let summary = workspace_summary_from_snapshot(snapshot);
    let known_revision = known_revision
        .as_deref()
        .map(|revision| validate_revision_token("Git 汇总修订", revision))
        .transpose()?;
    if known_revision.as_deref() == Some(summary.summary_revision.as_str()) {
        Ok(GitWorkspaceSummaryResult::Unchanged {
            revision: summary.summary_revision,
        })
    } else {
        Ok(GitWorkspaceSummaryResult::Snapshot { summary })
    }
}

pub fn change_page(
    workspace: &Path,
    request: GitChangePageRequest,
) -> Result<GitChangePageResult, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let expected_revision = validate_revision_token("Git 变更页修订", &request.expected_revision)?;
    let query = normalize_change_query(request.query.as_deref())?;
    if request.limit == 0 || request.limit > MAX_CHANGE_PAGE_LIMIT {
        return Err(format!(
            "Git 变更页大小必须在 1 到 {MAX_CHANGE_PAGE_LIMIT} 之间"
        ));
    }
    let selected_path = request
        .selected_path
        .as_deref()
        .map(validate_relative_path)
        .transpose()?;
    let snapshot = snapshot_for_repository(&repository)?;
    let summary = workspace_summary_from_snapshot(&snapshot);
    if summary.summary_revision != expected_revision {
        return Ok(GitChangePageResult::Stale { summary });
    }

    let mut files = snapshot.files.clone();
    // The review panel lists tracked changes only. Dropping untracked entries here
    // rather than in the renderer keeps the page, `matched_count`, the cursor and
    // the selection resolved against the same list.
    files.retain(|file| !file.untracked);
    files.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| {
                left.original_path
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes()
                    .cmp(
                        right
                            .original_path
                            .as_deref()
                            .unwrap_or_default()
                            .as_bytes(),
                    )
            })
    });
    let matches_query = |file: &GitFileChange| change_matches_query(file, &query);
    let selection = selected_path.as_deref().map(|selected_path| {
        match files.iter().find(|file| file.path == selected_path) {
            Some(file) if matches_query(file) => GitChangeSelection::Present { file: file.clone() },
            Some(_) => GitChangeSelection::FilteredOut,
            None => GitChangeSelection::Missing,
        }
    });
    let offset = request
        .cursor
        .as_deref()
        .map(|cursor| parse_change_cursor(cursor, &expected_revision, &query))
        .transpose()?
        .unwrap_or(0);
    let mut matched_total = 0_usize;
    let mut page = Vec::with_capacity(usize::from(request.limit));
    for file in files.into_iter().filter(matches_query) {
        if matched_total >= offset && page.len() < usize::from(request.limit) {
            page.push(file);
        }
        matched_total = matched_total.saturating_add(1);
    }
    if offset > matched_total {
        return Err("Git 变更页游标超出当前匹配结果；请重新加载".into());
    }
    let matched_count = u32::try_from(matched_total).unwrap_or(u32::MAX);
    let end = offset.saturating_add(page.len());
    let next_cursor =
        (end < matched_total).then(|| encode_change_cursor(end, &expected_revision, &query));
    Ok(GitChangePageResult::Page {
        revision: expected_revision,
        files: page,
        matched_count,
        next_cursor,
        selection,
    })
}

pub fn diff(workspace: &Path, request: GitDiffRequest) -> Result<GitDiffResponse, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let unified = resolved_diff_context(&request);
    let (path, mode) = match &request {
        GitDiffRequest::Working { path, .. } => (path.as_deref(), "working"),
        GitDiffRequest::Staged { path, .. } => (path.as_deref(), "staged"),
        GitDiffRequest::Unstaged { path, .. } => (path.as_deref(), "unstaged"),
        GitDiffRequest::Compare { path, .. } => (path.as_deref(), "compare"),
    };
    let path = path.map(validate_relative_path).transpose()?;
    let snapshot = snapshot_for_repository(&repository)?;
    let requested_untracked = matches!(
        &request,
        GitDiffRequest::Working { .. } | GitDiffRequest::Unstaged { .. }
    ) && path.as_ref().is_some_and(|path| {
        snapshot
            .files
            .iter()
            .any(|file| file.path == *path && file.untracked)
    });
    let mut args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--no-color"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from(format!("--unified={unified}")),
    ];
    let accepts_difference_exit = requested_untracked;
    if requested_untracked && mode != "staged" {
        let path = path
            .as_ref()
            .ok_or_else(|| "读取未跟踪文件 diff 时必须指定 path".to_owned())?;
        let absolute = canonical_existing_repo_file(&repository.root, path)?;
        args.push(OsString::from("--no-index"));
        args.push(OsString::from("--"));
        args.push(OsString::from("/dev/null"));
        args.push(absolute.into_os_string());
    } else {
        match &request {
            GitDiffRequest::Working { .. } => {
                if repository_has_head(&repository)? {
                    args.push(OsString::from("HEAD"));
                } else {
                    args.push(OsString::from("--cached"));
                }
            }
            GitDiffRequest::Staged { .. } => args.push(OsString::from("--cached")),
            GitDiffRequest::Unstaged { .. } => {}
            GitDiffRequest::Compare { base, head, .. } => {
                let base = resolve_commit(&repository, base)?;
                let head = resolve_commit(&repository, head)?;
                args.push(OsString::from(format!("{base}...{head}")));
            }
        }
        if let Some(path) = &path {
            args.push(OsString::from("--"));
            args.push(OsString::from(path));
        }
    }
    let output = run_git(
        &repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_PATCH_OUTPUT,
        true,
    )?;
    let acceptable = output.success()
        || (accepts_difference_exit && !output.timed_out && output.exit_code() == Some(1));
    if !acceptable {
        return Err(command_error("读取 Git diff", &output));
    }
    let patch = String::from_utf8_lossy(&output.stdout).into_owned();
    let (additions, deletions) = count_patch_lines(&patch);
    let files = diff_files_for_request(&repository, &request, path.as_deref(), &snapshot)?;
    Ok(GitDiffResponse {
        path,
        patch,
        truncated: output.stdout_truncated,
        additions,
        deletions,
        binary: output_looks_binary(&output.stdout),
        files,
    })
}

fn diff_files_for_request(
    repository: &Repository,
    request: &GitDiffRequest,
    path: Option<&str>,
    snapshot: &GitWorkspaceSnapshot,
) -> Result<Vec<GitFileChange>, String> {
    let mut args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--name-status"),
        OsString::from("--find-renames"),
        OsString::from("-z"),
    ];
    append_diff_selector(repository, request, &mut args)?;
    if let Some(path) = path {
        args.push(OsString::from("--"));
        args.push(OsString::from(path));
    }
    let output = run_git(
        repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("读取 Git diff 文件列表", &output)?;
    if output.stdout_truncated {
        return Err("Git diff 文件列表超过安全上限，无法保证结果完整".into());
    }
    let mut files = parse_name_status(&output.stdout)?;
    let line_stats = diff_numstat_for_request(repository, request, path)?;
    for file in &mut files {
        if let Some(stats) = line_stats.get(&file.path) {
            file.additions = (!stats.binary).then_some(stats.additions);
            file.deletions = (!stats.binary).then_some(stats.deletions);
            file.binary = stats.binary;
        }
        if !matches!(request, GitDiffRequest::Compare { .. }) {
            if let Some(status) = snapshot
                .files
                .iter()
                .find(|status| status.path == file.path)
            {
                file.index_status = status.index_status.clone();
                file.worktree_status = status.worktree_status.clone();
                file.staged = status.staged;
                file.unstaged = status.unstaged;
                file.untracked = status.untracked;
                file.conflicted = status.conflicted;
            }
        }
    }
    if matches!(
        request,
        GitDiffRequest::Working { .. } | GitDiffRequest::Unstaged { .. }
    ) {
        files.extend(
            snapshot
                .files
                .iter()
                .filter(|file| {
                    file.untracked && path.map(|requested| requested == file.path).unwrap_or(true)
                })
                .cloned(),
        );
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    Ok(files)
}

fn append_diff_selector(
    repository: &Repository,
    request: &GitDiffRequest,
    args: &mut Vec<OsString>,
) -> Result<(), String> {
    match request {
        GitDiffRequest::Working { .. } => {
            if repository_has_head(repository)? {
                args.push(OsString::from("HEAD"));
            } else {
                args.push(OsString::from("--cached"));
            }
        }
        GitDiffRequest::Staged { .. } => args.push(OsString::from("--cached")),
        GitDiffRequest::Unstaged { .. } => {}
        GitDiffRequest::Compare { base, head, .. } => {
            let base = resolve_commit(repository, base)?;
            let head = resolve_commit(repository, head)?;
            args.push(OsString::from(format!("{base}...{head}")));
        }
    }
    Ok(())
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<GitFileChange>, String> {
    let records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let code = lossy(records[index]);
        let status = code.chars().next().unwrap_or('?');
        let (path, original_path, consumed) = if matches!(status, 'R' | 'C') {
            let original = records
                .get(index + 1)
                .ok_or_else(|| "Git diff rename 缺少原路径".to_owned())?;
            let path = records
                .get(index + 2)
                .ok_or_else(|| "Git diff rename 缺少目标路径".to_owned())?;
            (lossy(path), Some(lossy(original)), 3)
        } else {
            let path = records
                .get(index + 1)
                .ok_or_else(|| "Git diff 文件状态缺少路径".to_owned())?;
            (lossy(path), None, 2)
        };
        changes.push(GitFileChange {
            path,
            original_path,
            status: match status {
                'A' => GitFileStatus::Added,
                'M' => GitFileStatus::Modified,
                'D' => GitFileStatus::Deleted,
                'R' => GitFileStatus::Renamed,
                'C' => GitFileStatus::Copied,
                'T' => GitFileStatus::TypeChanged,
                'U' => GitFileStatus::Unmerged,
                _ => GitFileStatus::Unknown,
            },
            index_status: String::new(),
            worktree_status: String::new(),
            staged: false,
            unstaged: false,
            untracked: false,
            conflicted: status == 'U',
            additions: None,
            deletions: None,
            binary: false,
            submodule: false,
            submodule_commit_changed: false,
            submodule_modified: false,
            submodule_untracked: false,
        });
        index += consumed;
    }
    Ok(changes)
}

fn diff_numstat_for_request(
    repository: &Repository,
    request: &GitDiffRequest,
    path: Option<&str>,
) -> Result<HashMap<String, FileLineStats>, String> {
    let mut args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--numstat"),
        OsString::from("-z"),
    ];
    append_diff_selector(repository, request, &mut args)?;
    if let Some(path) = path {
        args.push(OsString::from("--"));
        args.push(OsString::from(path));
    }
    let output = run_git(
        repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("统计 Git diff 文件行数", &output)?;
    if output.stdout_truncated {
        return Err("Git diff 行数统计超过安全上限，无法保证结果完整".into());
    }
    parse_numstat(&output.stdout)
}

pub fn branches(workspace: &Path) -> Result<GitBranchesResult, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(GitBranchesResult {
        branches: branches_for_repository(&repository)?,
        default_branch: default_branch_for_repository(&repository)?,
    })
}

/// Relative path for workflow-step isolated worktrees.
///
/// Keeping it inside the repository lets upward-looking tooling, especially Node
/// `node_modules` resolution, continue to find repository-root dependencies.
const ISOLATED_WORKTREE_DIRECTORY: &str = "worktrees";
const MEWORK_PROJECT_DIRECTORY: &str = ".mework";

/// Contents of the self-ignoring `.gitignore` in isolated worktree directories.
///
/// A `*` entry ignores the directory and its own `.gitignore`, keeping it absent
/// from the parent repository's `git status`.
const ISOLATED_WORKTREE_GITIGNORE: &str = "*\n";

/// Prefix shared by isolated-worktree branch and directory names.
const ISOLATED_WORKTREE_BRANCH_PREFIX: &str = "mework/wf";

/// Branch prefix for conversation-isolated worktrees. It is distinct from
/// workflow-step branches so residual branches and cleanup policies stay separate.
const CONVERSATION_WORKTREE_BRANCH_PREFIX: &str = "mework/conv";

/// Container path for conversation-isolated worktrees. Workflow steps use
/// `<runId>/<slot>` while conversations use `conversations/<conversationId>`,
/// preventing name collisions.
const CONVERSATION_WORKTREE_DIRECTORY: &str = "conversations";

/// An isolated worktree created for a workflow step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IsolatedWorktree {
    /// Absolute worktree root used as the step subagent's trusted workspace.
    pub path: PathBuf,
    /// Branch created for this worktree.
    pub branch: String,
    /// Baseline commit used to detect extra commits during release.
    pub base_oid: String,
}

/// Creates an isolated worktree checked out from HEAD for a workflow step.
///
/// # Boundary
///
/// - The workspace must itself be a repository root. Subdirectories must fail
///   rather than silently escalating to an ancestor repository.
/// - The checkout is the HEAD commit, excluding uncommitted and untracked parent
///   files; this is the definition of Git worktree isolation.
/// - Hold `repository_lock` throughout to exclude UI-initiated Git writes.
///
/// This host-internal operation does not use the user-facing `GitAction` protocol.
pub(crate) fn create_isolated_worktree(
    workspace: &Path,
    run_id: &str,
    slot: &str,
) -> Result<IsolatedWorktree, String> {
    let run_id = validate_worktree_component("运行 id", run_id)?;
    let slot = validate_worktree_component("步骤槽位名", slot)?;
    let branch = format!("{ISOLATED_WORKTREE_BRANCH_PREFIX}/{run_id}/{slot}");
    create_worktree(workspace, &[&run_id, &slot], &branch, None)
}

/// Creates an isolated worktree for a conversation.
///
/// It shares the workflow-step mechanism and container, but uses
/// `conversations/`, the `mework/conv` prefix, and an optional baseline branch.
/// `from_branch` must name an existing local branch; `None` uses current HEAD.
pub(crate) fn create_conversation_worktree(
    workspace: &Path,
    conversation_id: &str,
    from_branch: Option<&str>,
) -> Result<IsolatedWorktree, String> {
    let conversation_id = validate_worktree_component("对话 id", conversation_id)?;
    let branch = format!("{CONVERSATION_WORKTREE_BRANCH_PREFIX}/{conversation_id}");
    create_worktree(
        workspace,
        &[CONVERSATION_WORKTREE_DIRECTORY, &conversation_id],
        &branch,
        from_branch,
    )
}

/// Shared isolated-worktree creation procedure.
///
/// `segments` are relative path components inside the container, `branch` is the
/// new branch, and `start_point` is an optional baseline local branch.
fn create_worktree(
    workspace: &Path,
    segments: &[&str],
    branch: &str,
    start_point: Option<&str>,
) -> Result<IsolatedWorktree, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // The baseline branch must exist. Checking out a nonexistent ref yields a
    // detached HEAD while the returned record would falsely name a branch.
    if let Some(name) = start_point {
        validate_local_branch(&repository, name)?;
    }

    let container = repository
        .root
        .join(MEWORK_PROJECT_DIRECTORY)
        .join(ISOLATED_WORKTREE_DIRECTORY);
    // `fs::canonicalize` gives `repository.root` a Windows `\\?\` long-path
    // prefix. Git accepts it as a cwd but not as a `worktree add` target, so
    // normalize it here before it is also used as the step workspace path.
    let container = PathBuf::from(git_cli_environment_path(&container));
    fs::create_dir_all(&container)
        .map_err(|error| format!("无法创建隔离工作树目录 {}: {error}", container.display()))?;
    let ignore = container.join(".gitignore");
    // Recreate the self-ignore file every time; deleting it would expose future
    // worktrees as untracked parent-repository content.
    if fs::read(&ignore).ok().as_deref() != Some(ISOLATED_WORKTREE_GITIGNORE.as_bytes()) {
        fs::write(&ignore, ISOLATED_WORKTREE_GITIGNORE)
            .map_err(|error| format!("无法写入隔离工作树的 .gitignore: {error}"))?;
    }

    let revision = start_point.unwrap_or("HEAD");
    let output = run_git(
        &repository,
        [OsString::from("rev-parse"), OsString::from(revision)],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        true,
    )?;
    if !output.success() {
        // An unborn HEAD has no commit available as a checkout baseline.
        return Err(
            "无法读取当前仓库的 HEAD（仓库可能还没有任何提交），隔离工作树需要一个基线提交"
                .to_owned(),
        );
    }
    let base_oid = String::from_utf8(output.stdout.clone())
        .map_err(|_| "Git 返回的 HEAD 不是有效 UTF-8".to_owned())?
        .trim()
        .to_owned();
    if base_oid.is_empty() || !base_oid.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Git 返回的 HEAD 不是一个提交 ID".to_owned());
    }

    let mut path = container;
    for segment in segments {
        path = path.join(segment);
    }
    let output = run_git(
        &repository,
        [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from(branch),
            git_cli_environment_path(&path),
            OsString::from(&base_oid),
        ],
        None,
        BULK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    require_success("创建隔离工作树", &output)?;
    Ok(IsolatedWorktree {
        path,
        branch: branch.to_owned(),
        base_oid,
    })
}

/// Releases an isolated worktree after a step: remove it only when unchanged;
/// otherwise preserve it intact.
///
/// `Ok(true)` means removal succeeded. `Ok(false)` retains the worktree and branch
/// unless both `git status --porcelain` is empty and no commits follow baseline.
/// Git's own safe `worktree remove` and `branch -d` checks provide a second guard.
/// Cleanup failure preserves the worktree rather than failing the whole run.
pub(crate) fn release_isolated_worktree(
    workspace: &Path,
    worktree: &IsolatedWorktree,
) -> Result<bool, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !worktree.path.is_dir() {
        // A missing directory means removal is already complete; clear its
        // registration without error.
        prune_worktrees(&repository);
        return Ok(true);
    }
    if worktree_has_changes(&repository, worktree)? {
        return Ok(false);
    }
    let output = run_git(
        &repository,
        [
            OsString::from("worktree"),
            OsString::from("remove"),
            git_cli_environment_path(&worktree.path),
        ],
        None,
        BULK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    if !output.success() {
        return Ok(false);
    }
    let output = run_git(
        &repository,
        [
            OsString::from("branch"),
            OsString::from("-d"),
            OsString::from(&worktree.branch),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    // Failure to delete the branch does not change the fact that its worktree
    // has been removed.
    let _ = output.success();
    // `git worktree remove` deletes only the leaf. Remove an empty `<runId>/`
    // parent as well, but never touch one containing another worktree.
    if let Some(parent) = worktree.path.parent() {
        let _ = fs::remove_dir(parent);
    }
    prune_worktrees(&repository);
    Ok(true)
}

/// Releases an isolated worktree from its conversation registration.
///
/// It uses the same retain-on-change semantics as `release_isolated_worktree`.
pub(crate) fn release_conversation_worktree(
    workspace: &Path,
    worktree: &crate::model::ConversationWorktree,
) -> Result<bool, String> {
    release_isolated_worktree(
        workspace,
        &IsolatedWorktree {
            path: PathBuf::from(&worktree.path),
            branch: worktree.branch.clone(),
            base_oid: worktree.base_oid.clone(),
        },
    )
}

/// Whether a worktree has uncommitted changes or commits after its baseline.
fn worktree_has_changes(
    repository: &Repository,
    worktree: &IsolatedWorktree,
) -> Result<bool, String> {
    let git = repository.git.clone();
    let mut arguments = git_command_prefix();
    arguments.extend([
        OsString::from("status"),
        OsString::from("--porcelain"),
        OsString::from("--untracked-files=all"),
    ]);
    let status = run_program(
        &git,
        &worktree.path,
        arguments,
        None,
        BULK_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        CliKind::GitPassive,
    )?;
    // Treat unreadable status as modified: wasting disk is preferable to deleting
    // work that may contain content.
    if !status.success() || !status.stdout.is_empty() {
        return Ok(true);
    }
    let mut arguments = git_command_prefix();
    arguments.extend([
        OsString::from("rev-list"),
        OsString::from("--count"),
        OsString::from(format!("{}..HEAD", worktree.base_oid)),
    ]);
    let ahead = run_program(
        &git,
        &worktree.path,
        arguments,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        CliKind::GitPassive,
    )?;
    if !ahead.success() {
        return Ok(true);
    }
    let count = String::from_utf8_lossy(&ahead.stdout).trim().to_owned();
    Ok(count != "0")
}

/// Removes registrations whose worktree directories vanished. It changes only
/// `$GIT_COMMON_DIR/worktrees` management files, never worktree content.
fn prune_worktrees(repository: &Repository) {
    let _ = run_git(
        repository,
        [OsString::from("worktree"), OsString::from("prune")],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    );
}

/// Validate host-generated run IDs and slots before placing them in paths and ref
/// names. Reject `..`, `/`, and leading `-` to prevent path escape or option injection.
fn validate_worktree_component(label: &str, value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 128 {
        return Err(format!("{label}长度不合法"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(format!("{label}只能包含字母、数字、下划线与连字符"));
    }
    if value.starts_with('-') || value.starts_with('.') {
        return Err(format!("{label}不能以连字符或点开头"));
    }
    Ok(value.to_owned())
}

pub fn execute_action(workspace: &Path, action: GitAction) -> Result<GitActionResult, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let action = expand_rename_pathspecs_for_action(&repository, action)?;
    validate_bulk_action(&repository, &action)?;
    validate_submodule_action(&repository, &action)?;
    validate_action_during_repository_operation(&repository, &action)?;
    let output = match action {
        GitAction::Discard {
            paths,
            include_untracked,
            expected_content_revision,
            expected_target_revision,
        } => execute_discard(
            &repository,
            &paths,
            include_untracked,
            &expected_content_revision,
            &expected_target_revision,
        )?,
        action => {
            let (args, input, timeout) = prepare_git_action(&repository, action)?;
            run_git(&repository, args, input, timeout, MAX_ACTION_OUTPUT, false)?
        }
    };
    require_success("执行 Git 操作", &output)?;
    let message = output.display_output();
    let snapshot = bounded_workspace_snapshot(snapshot_for_repository(&repository)?);
    Ok(GitActionResult {
        message: (!message.is_empty()).then_some(message),
        snapshot: Some(snapshot),
    })
}

pub fn prepare_discard(
    workspace: &Path,
    paths: &[String],
    include_untracked: bool,
) -> Result<GitDiscardPreparation, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = snapshot_for_repository(&repository)?;
    let selection = discard_selection(&snapshot, paths)?;
    let target_revision = discard_target_revision(&repository, &selection, include_untracked)?;
    Ok(GitDiscardPreparation {
        snapshot,
        target_revision,
    })
}

fn validate_bulk_action(repository: &Repository, action: &GitAction) -> Result<(), String> {
    let GitAction::BisectStep {
        expected_content_revision,
        ..
    } = action
    else {
        return Ok(());
    };
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.content_revision != expected_content_revision {
        return Err("Git 工作区已在操作前发生变化；请刷新后重试".into());
    }
    if !snapshot.is_clean {
        return Err("Git bisect 前进操作要求工作树干净；请先提交或储藏当前变更".into());
    }
    Ok(())
}

fn expand_rename_pathspecs_for_action(
    repository: &Repository,
    action: GitAction,
) -> Result<GitAction, String> {
    let paths = match action {
        GitAction::Unstage { paths } => paths,
        action => return Ok(action),
    };
    let normalized = paths
        .into_iter()
        .map(|path| validate_relative_path(&path))
        .collect::<Result<Vec<_>, _>>()?;
    let snapshot = snapshot_for_repository(repository)?;
    let mut seen = HashSet::new();
    let mut expanded = Vec::with_capacity(normalized.len());
    for path in normalized {
        if seen.insert(path.clone()) {
            expanded.push(path.clone());
        }
        let Some(change) = snapshot.files.iter().find(|change| change.path == path) else {
            continue;
        };
        if change.status != GitFileStatus::Renamed {
            continue;
        }
        if let Some(original_path) = change.original_path.as_ref() {
            if seen.insert(original_path.clone()) {
                expanded.push(original_path.clone());
            }
        }
    }
    Ok(GitAction::Unstage { paths: expanded })
}

fn validate_submodule_action(repository: &Repository, action: &GitAction) -> Result<(), String> {
    let paths = match action {
        GitAction::Stage { paths } => paths,
        _ => return Ok(()),
    };
    let normalized = paths
        .iter()
        .map(|path| validate_relative_path(path))
        .collect::<Result<HashSet<_>, _>>()?;
    let snapshot = snapshot_for_repository(repository)?;
    for change in snapshot
        .files
        .iter()
        .filter(|change| normalized.contains(&change.path) && change.submodule)
    {
        if !change.submodule_commit_changed {
            return Err(format!(
                "子模块 {} 只有内部未提交变更，父仓库没有可暂存的 gitlink；请将该子模块作为独立工作区处理",
                change.path
            ));
        }
    }
    Ok(())
}

fn require_repository(workspace: &Path) -> Result<Repository, String> {
    discover_repository(workspace)?
        .ok_or_else(|| "当前工作目录不是独立的 Git 仓库根目录".to_owned())
}

fn discover_repository(workspace: &Path) -> Result<Option<Repository>, String> {
    let workspace = fs::canonicalize(workspace)
        .map_err(|error| format!("无法访问 Git 工作目录 {}: {error}", workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!("Git 工作目录不是文件夹: {}", workspace.display()));
    }
    let git = find_program("git").ok_or_else(|| "未找到 Git CLI，请先安装 Git".to_owned())?;
    let output = run_program(
        &git,
        &workspace,
        [
            OsString::from("--no-optional-locks"),
            OsString::from("-c"),
            OsString::from("core.fsmonitor=false"),
            OsString::from("-c"),
            OsString::from("gc.auto=0"),
            OsString::from("-c"),
            OsString::from("maintenance.auto=false"),
            OsString::from("-c"),
            OsString::from("submodule.recurse=false"),
            OsString::from("-c"),
            OsString::from("fetch.recurseSubmodules=false"),
            OsString::from("-c"),
            OsString::from("push.recurseSubmodules=no"),
            OsString::from("rev-parse"),
            OsString::from("--path-format=absolute"),
            OsString::from("--show-toplevel"),
            OsString::from("--git-dir"),
            OsString::from("--git-common-dir"),
            OsString::from("--git-path"),
            OsString::from("index"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        64 * 1024,
        CliKind::GitPassive,
    )?;
    if !output.success() {
        let text = output.display_output();
        if text.to_ascii_lowercase().contains("not a git repository") {
            return Ok(None);
        }
        return Err(command_error("检测 Git 仓库", &output));
    }
    let paths = parse_rev_parse_paths(&output.stdout, 4)?;
    let root = canonical_git_directory("仓库根目录", Path::new(&paths[0]), &workspace)?;
    if !same_path(&root, &workspace) {
        // A selected subdirectory must not silently elevate Git access to its parent repository.
        return Ok(None);
    }
    let git_dir = PathBuf::from(&paths[1]);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        root.join(git_dir)
    };
    let git_dir = canonical_git_directory("元数据目录", &git_dir, &root)?;
    let git_common_dir = PathBuf::from(&paths[2]);
    let git_common_dir = if git_common_dir.is_absolute() {
        git_common_dir
    } else {
        root.join(git_common_dir)
    };
    let git_common_dir = canonical_git_directory("共享元数据目录", &git_common_dir, &root)?;
    if !path_is_within(&git_dir, &git_common_dir) {
        return Err("Git 返回的 worktree 元数据目录不属于共享元数据目录".into());
    }
    let index_path = PathBuf::from(&paths[3]);
    let index_path = if index_path.is_absolute() {
        index_path
    } else {
        root.join(index_path)
    };
    let index_parent = index_path
        .parent()
        .ok_or_else(|| "Git worktree index 路径缺少父目录".to_owned())?;
    let index_parent = fs::canonicalize(index_parent).map_err(|error| {
        format!(
            "无法验证 Git worktree index 父目录 {}: {error}",
            index_parent.display()
        )
    })?;
    if !same_path(&index_parent, &git_dir)
        || index_path
            .file_name()
            .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("index"))
    {
        return Err("Git 返回的 index 不属于当前 worktree 元数据目录".into());
    }
    let version_output = run_program(
        &git,
        &root,
        [OsString::from("--version")],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        CliKind::GitPassive,
    )?;
    require_success("读取 Git 版本", &version_output)?;
    let version_text = String::from_utf8_lossy(&version_output.stdout);
    let version_text = version_text.trim();
    let git_version = version_text
        .strip_prefix("git version ")
        .unwrap_or(version_text)
        .to_owned();
    let repository_id = git_path_id(b"mework.git.repository-id.v1", &git_common_dir)?;
    let worktree_id = git_worktree_id(&repository_id, &root, &git_dir)?;
    Ok(Some(Repository {
        root,
        git_dir,
        repository_id,
        worktree_id,
        git_common_dir,
        git,
        git_version,
    }))
}

fn snapshot_for_repository(repository: &Repository) -> Result<GitWorkspaceSnapshot, String> {
    let status = run_git(
        repository,
        [
            OsString::from("--no-optional-locks"),
            OsString::from("status"),
            OsString::from("--porcelain=v2"),
            OsString::from("-z"),
            OsString::from("--branch"),
            OsString::from("--show-stash"),
            OsString::from("--untracked-files=all"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("读取 Git 状态", &status)?;
    if status.stdout_truncated {
        return Err("Git 状态超过安全上限，无法可靠显示完整变更".into());
    }
    let mut parsed = parse_porcelain_v2(&status.stdout)?;
    let mut warnings = nested_submodule_warnings(&parsed.changes);
    let line_stats = match combined_line_stats(repository, &mut parsed.changes) {
        Ok(stats) => stats,
        Err(error) => {
            warnings.push(error);
            GitLineStats {
                additions: 0,
                deletions: 0,
                binary_files: 0,
            }
        }
    };
    let content_revision = repository_content_revision(repository, &parsed.changes)?;
    let (transports, remote_warnings) = snapshot_remote_transports(repository);
    warnings.extend(remote_warnings);
    let (upstream, upstream_target) = if let Some(branch_name) = parsed.branch.head.as_deref() {
        match upstream_target_for_branch(repository, branch_name, &transports) {
            Ok((upstream, target, local_oid)) => {
                if parsed.branch.oid.as_deref() != Some(local_oid.as_str())
                    || parsed.branch.upstream != upstream
                {
                    return Err("Git 分支或 upstream 在状态读取期间发生变化；请重试".into());
                }
                (upstream, target)
            }
            Err(error) => {
                warnings.push(error);
                (parsed.branch.upstream.clone(), None)
            }
        }
    } else {
        (None, None)
    };
    let operation_state = repository_operation_state(&repository.git_dir)?;
    Ok(assemble_snapshot(SnapshotParts {
        repository_id: repository.repository_id.clone(),
        worktree_id: repository.worktree_id.clone(),
        root: repository.root.to_string_lossy().into_owned(),
        git_version: repository.git_version.clone(),
        parsed,
        line_stats,
        content_revision,
        remote_proofs: transports
            .iter()
            .map(|transport| transport.proof.clone())
            .collect(),
        upstream,
        upstream_target,
        operation_state,
        warnings,
    }))
}

/// A warning for submodules with uncommitted work of their own, which the
/// status of the superproject reports but cannot act on.
fn nested_submodule_warnings(changes: &[GitFileChange]) -> Vec<String> {
    let nested_submodule_changes = changes
        .iter()
        .filter(|change| {
            change.submodule && (change.submodule_modified || change.submodule_untracked)
        })
        .count();
    if nested_submodule_changes > 0 {
        vec![format!(
            "{nested_submodule_changes} 个子模块包含内部未提交变更；请将子模块目录作为独立工作区处理"
        )]
    } else {
        Vec::new()
    }
}

/// Everything a snapshot is built from, however it was read: by this host's
/// Git on its own checkout, or by a probe on another machine
/// ([`remote_workspace_snapshot`]).
struct SnapshotParts {
    repository_id: String,
    worktree_id: String,
    root: String,
    git_version: String,
    parsed: ParsedStatus,
    line_stats: GitLineStats,
    content_revision: String,
    /// Configured remotes, in the order Git lists them.
    remote_proofs: Vec<GitRemote>,
    upstream: Option<String>,
    upstream_target: Option<GitUpstream>,
    operation_state: Option<RepositoryOperationState>,
    warnings: Vec<String>,
}

fn assemble_snapshot(parts: SnapshotParts) -> GitWorkspaceSnapshot {
    let SnapshotParts {
        repository_id,
        worktree_id,
        root,
        git_version,
        parsed,
        line_stats,
        content_revision,
        remote_proofs,
        upstream,
        upstream_target,
        operation_state,
        warnings,
    } = parts;
    let staged_count = parsed.changes.iter().filter(|change| change.staged).count() as u32;
    let unstaged_count = parsed
        .changes
        .iter()
        .filter(|change| change.unstaged)
        .count() as u32;
    let untracked_count = parsed
        .changes
        .iter()
        .filter(|change| change.untracked)
        .count() as u32;
    let conflicted_count = parsed
        .changes
        .iter()
        .filter(|change| change.conflicted)
        .count() as u32;
    let remote = preferred_git_remote(&remote_proofs, upstream_target.as_ref());
    let mut remotes = remote_proofs;
    if upstream_target
        .as_ref()
        .is_some_and(|target| target.is_local)
    {
        remotes.push(local_remote_proof());
        remotes.sort_by(|left, right| left.name.cmp(&right.name));
        remotes.dedup_by(|left, right| left.name == right.name);
    }
    let operation = operation_state.as_ref().map(|state| state.operation);
    let operation_revision = operation_state.map(|state| state.revision);
    let branch = parsed.branch;
    let mut snapshot = GitWorkspaceSnapshot {
        repository_id,
        worktree_id,
        branch: branch.head,
        head: branch.oid,
        content_revision,
        upstream,
        upstream_target,
        ahead: branch.ahead,
        behind: branch.behind,
        additions: line_stats.additions,
        deletions: line_stats.deletions,
        staged: staged_count,
        unstaged: unstaged_count,
        untracked: untracked_count,
        conflicted: conflicted_count,
        stash: parsed.stash_count,
        files: parsed.changes,
        remote,
        remotes,
        git_version,
        repository_root: root.clone(),
        worktree_root: root,
        detached: branch.detached,
        unborn: branch.unborn,
        operation,
        operation_revision,
        is_clean: staged_count == 0 && unstaged_count == 0 && untracked_count == 0,
        binary_files: line_stats.binary_files,
        warnings,
        summary_revision: String::new(),
        changed_files: 0,
        stageable: 0,
        unstageable: 0,
        files_complete: true,
    };
    let summary = workspace_summary_from_snapshot(&snapshot);
    snapshot.summary_revision = summary.summary_revision;
    snapshot.changed_files = summary.changed_files;
    snapshot.stageable = summary.stageable;
    snapshot.unstageable = summary.unstageable;
    snapshot
}

/// One Git invocation the status probe on another machine ran, as it
/// reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteGitOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RemoteGitOutput {
    fn success(&self, label: &str) -> Result<&[u8], String> {
        if self.status == 0 {
            return Ok(&self.stdout);
        }
        let detail = String::from_utf8_lossy(&self.stderr);
        let detail = detail.trim();
        Err(if detail.is_empty() {
            format!("{label}失败（退出码 {}）", self.status)
        } else {
            format!("{label}失败：{detail}")
        })
    }
}

/// What the status probe of [`crate::remote_git`] read about a workspace on
/// another machine: the reads [`snapshot_for_repository`] makes here, made
/// there, each under its own name.
#[derive(Clone, Debug, Default)]
pub(crate) struct RemoteGitProbe {
    /// The machine's identity (`run_environment::env_key`), folded into the
    /// repository's: one machine's `/srv/app` is not another's.
    pub machine_key: String,
    pub sections: HashMap<String, RemoteGitOutput>,
}

impl RemoteGitProbe {
    fn section(&self, name: &str) -> Result<&RemoteGitOutput, String> {
        self.sections
            .get(name)
            .ok_or_else(|| format!("远端 Git 状态探测缺少 {name} 段"))
    }
}

/// The snapshot [`snapshot_for_repository`] would build for a checkout this
/// host cannot reach, from what the probe read on its machine; `None` when the
/// workspace is not a repository root.
///
/// The rules are the local ones: a workspace below a repository's root is not
/// a repository, line counts, remotes and the upstream fold the same way, and
/// a failed line count, remote or upstream read is a warning rather than a
/// failure. What differs is what the host cannot do across the link — hash
/// the tracked diffs itself (the machine reports their digests) or read the
/// upstream twice to prove it held still — and the identities, which are keyed
/// by machine rather than by this filesystem's metadata.
pub(crate) fn remote_workspace_snapshot(
    probe: &RemoteGitProbe,
) -> Result<Option<GitWorkspaceSnapshot>, String> {
    let rev_parse = probe.section("rev-parse")?;
    if rev_parse.status != 0
        && String::from_utf8_lossy(&rev_parse.stderr)
            .to_ascii_lowercase()
            .contains("not a git repository")
    {
        return Ok(None);
    }
    let output = rev_parse.success("检测 Git 仓库")?;
    // `--show-prefix` comes first and is empty exactly when the workspace is
    // the repository root: the answer `same_path(root, workspace)` gives here,
    // without comparing two spellings of a path on a machine whose rules the
    // host does not share.
    let newline = output
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "Git 返回的仓库路径数量不正确".to_owned())?;
    if !output[..newline].iter().all(|byte| *byte == b'\r') {
        // A selected subdirectory must not silently elevate Git access to its parent repository.
        return Ok(None);
    }
    let paths = parse_rev_parse_paths(&output[newline + 1..], 3)?;
    let root = remote_path_text(&paths[0]);
    let git_dir = remote_absolute_path(&root, &paths[1]);
    let git_common_dir = remote_absolute_path(&root, &paths[2]);
    let mut digest = Sha256::new();
    digest.update(b"mework.git.remote-repository-id.v1\0");
    update_revision_component(&mut digest, b"machine", probe.machine_key.as_bytes());
    update_revision_component(&mut digest, b"git-common-dir", git_common_dir.as_bytes());
    let repository_id = format!("{:x}", digest.finalize());
    let mut digest = Sha256::new();
    digest.update(b"mework.git.remote-worktree-id.v1\0");
    update_revision_component(&mut digest, b"repository-id", repository_id.as_bytes());
    update_revision_component(&mut digest, b"root", root.as_bytes());
    update_revision_component(&mut digest, b"git-dir", git_dir.as_bytes());
    let worktree_id = format!("{:x}", digest.finalize());

    let version = lossy(probe.section("version")?.success("读取 Git 版本")?);
    let version = version.trim();
    let git_version = version
        .strip_prefix("git version ")
        .unwrap_or(version)
        .to_owned();
    let mut parsed = parse_porcelain_v2(probe.section("status")?.success("读取 Git 状态")?)?;
    let mut warnings = nested_submodule_warnings(&parsed.changes);
    let per_file = probe
        .section("numstat")
        .and_then(|output| output.success("统计 Git 变更行数"))
        .and_then(parse_numstat);
    let line_stats = match per_file {
        Ok(per_file) => apply_line_stats(&mut parsed.changes, per_file),
        Err(error) => {
            warnings.push(error);
            GitLineStats {
                additions: 0,
                deletions: 0,
                binary_files: 0,
            }
        }
    };
    let staged = probe
        .section("staged-digest")?
        .success("计算 Git 暂存内容修订")?;
    let unstaged = probe
        .section("unstaged-digest")?
        .success("计算 Git 工作树内容修订")?;
    let content_revision =
        content_revision(&parsed.changes, staged.trim_ascii(), unstaged.trim_ascii());
    let remote_proofs = match probe
        .section("remotes")
        .and_then(|output| output.success("读取 Git remotes"))
    {
        Ok(listing) => remote_proofs_from_listing(listing, &mut warnings),
        Err(error) => {
            warnings.push(error);
            Vec::new()
        }
    };
    let (upstream, upstream_target) = match parsed.branch.head.as_deref() {
        Some(branch) => match remote_upstream_target(probe, branch, &remote_proofs) {
            Ok((upstream, target, local_oid)) => {
                if parsed.branch.oid.as_deref() != Some(local_oid.as_str())
                    || parsed.branch.upstream != upstream
                {
                    return Err("Git 分支或 upstream 在状态读取期间发生变化；请重试".into());
                }
                (upstream, target)
            }
            Err(error) => {
                warnings.push(error);
                (parsed.branch.upstream.clone(), None)
            }
        },
        None => (None, None),
    };
    let operation_state = remote_operation_state(probe.section("operation")?)?;
    Ok(Some(assemble_snapshot(SnapshotParts {
        repository_id,
        worktree_id,
        root,
        git_version,
        parsed,
        line_stats,
        content_revision,
        remote_proofs,
        upstream,
        upstream_target,
        operation_state,
        warnings,
    })))
}

/// A path the machine's Git printed, with `/` separators and no trailing one
/// except at a filesystem root (`/`, `C:/`).
fn remote_path_text(path: &str) -> String {
    let path = path.replace('\\', "/");
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return format!("{trimmed}/");
    }
    trimmed.to_owned()
}

/// `path` resolved against `root` the way Git reports relative metadata
/// directories: relative to the directory it ran in, which is the root here.
fn remote_absolute_path(root: &str, path: &str) -> String {
    let path = remote_path_text(path);
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":/");
    if absolute {
        path
    } else {
        format!("{}/{path}", root.trim_end_matches('/'))
    }
}

/// Remote proofs from `git remote -v`: one per remote, in the order Git lists
/// them, over its fetch and push URLs. As on this host, the URLs themselves
/// never leave the backend.
fn remote_proofs_from_listing(listing: &[u8], warnings: &mut Vec<String>) -> Vec<GitRemote> {
    let mut remotes: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    for line in lossy(listing).lines() {
        let line = line.trim_end_matches('\r');
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let (url, push) = if let Some(url) = rest.strip_suffix(" (fetch)") {
            (url, false)
        } else if let Some(url) = rest.strip_suffix(" (push)") {
            (url, true)
        } else {
            continue;
        };
        if let Err(error) = validate_remote_name_syntax(name, false) {
            warnings.push(format!(
                "Git remote {name} 的 transport proof 不可用：{error}"
            ));
            continue;
        }
        let index = match remotes.iter().position(|(existing, _, _)| existing == name) {
            Some(index) => index,
            None => {
                remotes.push((name.to_owned(), Vec::new(), Vec::new()));
                remotes.len() - 1
            }
        };
        let urls = if push {
            &mut remotes[index].2
        } else {
            &mut remotes[index].1
        };
        urls.push(url.to_owned());
    }
    remotes
        .into_iter()
        .map(|(name, fetch_urls, push_urls)| GitRemote {
            fetch_revision: remote_transport_revision(
                b"mework.git.remote-fetch.v1\0",
                &name,
                &fetch_urls,
                &[],
            ),
            push_revision: remote_transport_revision(
                b"mework.git.remote-push.v1\0",
                &name,
                &push_urls,
                &[],
            ),
            name,
            url: None,
        })
        .collect()
}

/// [`upstream_target_for_branch`] over the probe's `for-each-ref` of the local
/// branches, whose current one is marked by `%(HEAD)`.
fn remote_upstream_target(
    probe: &RemoteGitProbe,
    branch: &str,
    remotes: &[GitRemote],
) -> Result<(Option<String>, Option<GitUpstream>, String), String> {
    let listing = probe
        .section("branches")?
        .success("读取 Git upstream atoms")?;
    let full_ref = format!("refs/heads/{branch}");
    let record = listing
        .split(|byte| *byte == b'\n')
        .find_map(|record| record.strip_prefix(b"*\0"))
        .ok_or_else(|| "当前本地 Git 分支已在读取 upstream 时消失".to_owned())?;
    let local_oid = record
        .split(|byte| *byte == 0)
        .nth(1)
        .map(lossy)
        .unwrap_or_default();
    let Some(atoms) = parse_upstream_atoms(record, &full_ref)? else {
        let local_oid = validate_object_id("本地分支提交", local_oid)?;
        return Ok((None, None, local_oid));
    };
    let tracking = probe.section("upstream-oid")?;
    let tracking_oid = (tracking.status == 0)
        .then(|| validate_object_id("upstream 提交", lossy(tracking.stdout.trim_ascii())).ok())
        .flatten();
    let is_local = atoms.remote_name == ".";
    let remote = if is_local {
        local_remote_proof()
    } else {
        remotes
            .iter()
            .find(|remote| remote.name == atoms.remote_name)
            .cloned()
            .ok_or_else(|| "Git upstream 指向不存在的 remote".to_owned())?
    };
    let remote_branch = atoms
        .merge_ref
        .strip_prefix("refs/heads/")
        .ok_or_else(|| "Git upstream merge ref 无效".to_owned())?
        .to_owned();
    let target = GitUpstream {
        remote_name: atoms.remote_name,
        remote_branch,
        merge_ref: atoms.merge_ref,
        tracking_ref: atoms.tracking_ref,
        tracking_oid,
        is_local,
        remote,
    };
    Ok((Some(atoms.tracking_short), Some(target), atoms.local_oid))
}

/// The operation in progress, from the probe's `operation` section: its label
/// on the first line, then a checksum line per state file, which the revision
/// is taken over.
fn remote_operation_state(
    output: &RemoteGitOutput,
) -> Result<Option<RepositoryOperationState>, String> {
    let bytes = output.success("检查 Git 操作标志")?;
    let label_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let label = lossy(&bytes[..label_end]);
    let label = label.trim();
    if label.is_empty() {
        return Ok(None);
    }
    let operation = [
        GitRepositoryOperation::Merge,
        GitRepositoryOperation::Rebase,
        GitRepositoryOperation::CherryPick,
        GitRepositoryOperation::Revert,
        GitRepositoryOperation::Bisect,
    ]
    .into_iter()
    .find(|operation| repository_operation_label(*operation) == label)
    .ok_or_else(|| format!("远端 Git 报告了未知的进行中操作 {label}"))?;
    let mut digest = Sha256::new();
    digest.update(b"mework.git.remote-operation-revision.v1\0");
    update_revision_component(&mut digest, b"state", bytes);
    Ok(Some(RepositoryOperationState {
        operation,
        revision: format!("{:x}", digest.finalize()),
    }))
}

fn workspace_summary_from_snapshot(snapshot: &GitWorkspaceSnapshot) -> GitWorkspaceSummary {
    let changed_files = if snapshot.files_complete {
        u32::try_from(snapshot.files.len()).unwrap_or(u32::MAX)
    } else {
        snapshot.changed_files
    };
    let stageable = if snapshot.files_complete {
        u32::try_from(
            snapshot
                .files
                .iter()
                .filter(|file| file.unstaged && (!file.submodule || file.submodule_commit_changed))
                .count(),
        )
        .unwrap_or(u32::MAX)
    } else {
        snapshot.stageable
    };
    let unstageable = if snapshot.files_complete {
        u32::try_from(snapshot.files.iter().filter(|file| file.staged).count()).unwrap_or(u32::MAX)
    } else {
        snapshot.unstageable
    };
    let mut digest = Sha256::new();
    digest.update(b"mework.git.workspace-summary.v2\0");
    for (label, value) in [
        ("repository-id", snapshot.repository_id.as_str()),
        ("worktree-id", snapshot.worktree_id.as_str()),
        ("repository-root", snapshot.repository_root.as_str()),
        ("worktree-root", snapshot.worktree_root.as_str()),
        ("content-revision", snapshot.content_revision.as_str()),
        ("branch", snapshot.branch.as_deref().unwrap_or_default()),
        ("head", snapshot.head.as_deref().unwrap_or_default()),
        ("upstream", snapshot.upstream.as_deref().unwrap_or_default()),
        (
            "operation",
            snapshot
                .operation
                .map(repository_operation_label)
                .unwrap_or_default(),
        ),
        (
            "operation-revision",
            snapshot.operation_revision.as_deref().unwrap_or_default(),
        ),
        (
            "remote",
            snapshot
                .remote
                .as_ref()
                .map(|remote| remote.name.as_str())
                .unwrap_or_default(),
        ),
        ("git-version", snapshot.git_version.as_str()),
    ] {
        update_revision_component(&mut digest, label.as_bytes(), value.as_bytes());
    }
    if let Some(remote) = snapshot.remote.as_ref() {
        update_revision_component(
            &mut digest,
            b"preferred-remote-name",
            remote.name.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"preferred-remote-fetch-revision",
            remote.fetch_revision.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"preferred-remote-push-revision",
            remote.push_revision.as_bytes(),
        );
    }
    for remote in &snapshot.remotes {
        update_revision_component(&mut digest, b"remote-name", remote.name.as_bytes());
        update_revision_component(
            &mut digest,
            b"remote-fetch-revision",
            remote.fetch_revision.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"remote-push-revision",
            remote.push_revision.as_bytes(),
        );
    }
    if let Some(upstream) = snapshot.upstream_target.as_ref() {
        for (label, value) in [
            ("upstream-remote-name", upstream.remote_name.as_str()),
            ("upstream-remote-branch", upstream.remote_branch.as_str()),
            ("upstream-merge-ref", upstream.merge_ref.as_str()),
            ("upstream-tracking-ref", upstream.tracking_ref.as_str()),
            (
                "upstream-tracking-oid",
                upstream.tracking_oid.as_deref().unwrap_or_default(),
            ),
            (
                "upstream-remote-fetch-revision",
                upstream.remote.fetch_revision.as_str(),
            ),
            (
                "upstream-remote-push-revision",
                upstream.remote.push_revision.as_str(),
            ),
        ] {
            update_revision_component(&mut digest, label.as_bytes(), value.as_bytes());
        }
        update_revision_component(
            &mut digest,
            b"upstream-is-local",
            &[u8::from(upstream.is_local)],
        );
    }
    for (label, value) in [
        ("ahead", u64::from(snapshot.ahead)),
        ("behind", u64::from(snapshot.behind)),
        ("additions", snapshot.additions),
        ("deletions", snapshot.deletions),
        ("staged", u64::from(snapshot.staged)),
        ("unstaged", u64::from(snapshot.unstaged)),
        ("untracked", u64::from(snapshot.untracked)),
        ("conflicted", u64::from(snapshot.conflicted)),
        ("stash", u64::from(snapshot.stash)),
        ("changed-files", u64::from(changed_files)),
        ("stageable", u64::from(stageable)),
        ("unstageable", u64::from(unstageable)),
        ("binary-files", u64::from(snapshot.binary_files)),
    ] {
        update_revision_component(&mut digest, label.as_bytes(), &value.to_be_bytes());
    }
    update_revision_component(
        &mut digest,
        b"flags",
        &[
            u8::from(snapshot.detached),
            u8::from(snapshot.unborn),
            u8::from(snapshot.is_clean),
        ],
    );
    for warning in &snapshot.warnings {
        update_revision_component(&mut digest, b"warning", warning.as_bytes());
    }
    let summary_revision = format!("{:x}", digest.finalize());

    GitWorkspaceSummary {
        repository_id: snapshot.repository_id.clone(),
        worktree_id: snapshot.worktree_id.clone(),
        branch: snapshot.branch.clone(),
        head: snapshot.head.clone(),
        content_revision: snapshot.content_revision.clone(),
        summary_revision,
        upstream: snapshot.upstream.clone(),
        upstream_target: snapshot.upstream_target.clone(),
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        staged: snapshot.staged,
        unstaged: snapshot.unstaged,
        untracked: snapshot.untracked,
        conflicted: snapshot.conflicted,
        stash: snapshot.stash,
        changed_files,
        stageable,
        unstageable,
        remote: snapshot.remote.clone(),
        remotes: snapshot.remotes.clone(),
        git_version: snapshot.git_version.clone(),
        repository_root: snapshot.repository_root.clone(),
        worktree_root: snapshot.worktree_root.clone(),
        detached: snapshot.detached,
        unborn: snapshot.unborn,
        operation: snapshot.operation,
        operation_revision: snapshot.operation_revision.clone(),
        is_clean: snapshot.is_clean,
        binary_files: snapshot.binary_files,
        warnings: snapshot.warnings.clone(),
    }
}

fn bounded_workspace_snapshot(mut snapshot: GitWorkspaceSnapshot) -> GitWorkspaceSnapshot {
    if snapshot.summary_revision.is_empty() {
        let summary = workspace_summary_from_snapshot(&snapshot);
        snapshot.summary_revision = summary.summary_revision;
        snapshot.changed_files = summary.changed_files;
        snapshot.stageable = summary.stageable;
        snapshot.unstageable = summary.unstageable;
    }
    snapshot.files.clear();
    snapshot.files_complete = false;
    snapshot
}

fn normalize_change_query(query: Option<&str>) -> Result<String, String> {
    let query = query.unwrap_or_default().trim();
    if query.contains('\0') || query.as_bytes().len() > MAX_CHANGE_QUERY_BYTES {
        return Err(format!(
            "Git 变更筛选不能包含 NUL，且不能超过 {MAX_CHANGE_QUERY_BYTES} 字节"
        ));
    }
    Ok(query.to_lowercase())
}

fn change_matches_query(file: &GitFileChange, query: &str) -> bool {
    query.is_empty()
        || file.path.to_lowercase().contains(query)
        || file
            .original_path
            .as_deref()
            .is_some_and(|path| path.to_lowercase().contains(query))
}

fn encode_change_cursor(offset: usize, revision: &str, query: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mework.git.change-cursor.v1\0");
    update_revision_component(&mut digest, b"revision", revision.as_bytes());
    update_revision_component(&mut digest, b"query", query.as_bytes());
    update_revision_component(
        &mut digest,
        b"offset",
        &u64::try_from(offset).unwrap_or(u64::MAX).to_be_bytes(),
    );
    format!("{offset:x}.{:x}", digest.finalize())
}

fn parse_change_cursor(cursor: &str, revision: &str, query: &str) -> Result<usize, String> {
    if cursor.len() > MAX_CHANGE_CURSOR_BYTES || cursor.contains('\0') {
        return Err("Git 变更页游标无效；请重新加载".into());
    }
    let (offset, _) = cursor
        .split_once('.')
        .ok_or_else(|| "Git 变更页游标无效；请重新加载".to_owned())?;
    if offset.is_empty() || !offset.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Git 变更页游标无效；请重新加载".into());
    }
    let offset = usize::from_str_radix(offset, 16)
        .map_err(|_| "Git 变更页游标无效；请重新加载".to_owned())?;
    if encode_change_cursor(offset, revision, query) != cursor {
        return Err("Git 变更页游标与当前仓库或筛选条件不匹配；请重新加载".into());
    }
    Ok(offset)
}

fn repository_content_revision(
    repository: &Repository,
    changes: &[GitFileChange],
) -> Result<String, String> {
    let empty_diff: [u8; 32] = Sha256::digest([]).into();
    let staged = if changes
        .iter()
        .any(|change| change.staged && !change.untracked)
    {
        tracked_diff_digest(repository, true)?
    } else {
        empty_diff
    };
    let unstaged = if changes
        .iter()
        .any(|change| change.unstaged && !change.untracked)
    {
        tracked_diff_digest(repository, false)?
    } else {
        empty_diff
    };
    Ok(content_revision(changes, &staged, &unstaged))
}

/// The content revision of a change list whose staged and unstaged tracked
/// diffs have the digests given: any change to a path, its status or the bytes
/// of either diff changes it.
fn content_revision(changes: &[GitFileChange], staged: &[u8], unstaged: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mework.git.content-revision.v3-canonical\0");
    let mut ordered = changes.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| {
                left.original_path
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes()
                    .cmp(
                        right
                            .original_path
                            .as_deref()
                            .unwrap_or_default()
                            .as_bytes(),
                    )
            })
    });
    for change in ordered {
        update_revision_component(&mut digest, b"path", change.path.as_bytes());
        update_revision_component(
            &mut digest,
            b"original-path",
            change
                .original_path
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        update_revision_component(&mut digest, b"status", git_file_status_label(change.status));
        update_revision_component(&mut digest, b"index-status", change.index_status.as_bytes());
        update_revision_component(
            &mut digest,
            b"worktree-status",
            change.worktree_status.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"flags",
            &[
                u8::from(change.staged),
                u8::from(change.unstaged),
                u8::from(change.untracked),
                u8::from(change.conflicted),
                u8::from(change.submodule),
                u8::from(change.submodule_commit_changed),
                u8::from(change.submodule_modified),
                u8::from(change.submodule_untracked),
            ],
        );
    }
    update_revision_component(&mut digest, b"staged-diff", staged);
    update_revision_component(&mut digest, b"unstaged-diff", unstaged);
    format!("{:x}", digest.finalize())
}

fn tracked_diff_digest(repository: &Repository, staged: bool) -> Result<[u8; 32], String> {
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--binary"),
        OsString::from("--full-index"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--no-color"),
        OsString::from("--no-renames"),
    ];
    if staged {
        args.push(OsString::from("--cached"));
    }
    args.push(OsString::from("--"));
    let output = run_git(repository, args, None, LOCAL_COMMAND_TIMEOUT, 0, true)?;
    require_success(
        if staged {
            "计算 Git 暂存内容修订"
        } else {
            "计算 Git 工作树内容修订"
        },
        &output,
    )?;
    Ok(output.stdout_sha256)
}

fn update_revision_component(digest: &mut Sha256, label: &[u8], value: &[u8]) {
    update_revision_component_header(digest, label, value.len() as u64);
    digest.update(value);
}

fn update_revision_component_header(digest: &mut Sha256, label: &[u8], value_len: u64) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update(value_len.to_be_bytes());
}

struct ParsedStatus {
    branch: GitBranchState,
    changes: Vec<GitFileChange>,
    stash_count: u32,
}

fn parse_porcelain_v2(bytes: &[u8]) -> Result<ParsedStatus, String> {
    let mut oid = None;
    let mut head = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut stash_count = 0;
    let mut changes = Vec::new();
    let records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if let Some(value) = strip_ascii_prefix(record, b"# branch.oid ") {
            let value = lossy(value);
            if value != "(initial)" {
                oid = Some(value);
            }
        } else if let Some(value) = strip_ascii_prefix(record, b"# branch.head ") {
            head = Some(lossy(value));
        } else if let Some(value) = strip_ascii_prefix(record, b"# branch.upstream ") {
            upstream = Some(lossy(value));
        } else if let Some(value) = strip_ascii_prefix(record, b"# branch.ab ") {
            let text = lossy(value);
            for part in text.split_ascii_whitespace() {
                if let Some(value) = part.strip_prefix('+') {
                    ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = part.strip_prefix('-') {
                    behind = value.parse().unwrap_or(0);
                }
            }
        } else if let Some(value) = strip_ascii_prefix(record, b"# stash ") {
            stash_count = lossy(value).parse().unwrap_or(0);
        } else if record.starts_with(b"# ") {
            // Porcelain v2 is extensible: consumers must ignore header fields
            // they do not recognize instead of rejecting a newer Git version.
        } else if record.starts_with(b"1 ") {
            changes.push(parse_ordinary_change(record)?);
        } else if record.starts_with(b"2 ") {
            let original = records
                .get(index + 1)
                .ok_or_else(|| "Git rename 状态缺少原路径".to_owned())?;
            changes.push(parse_renamed_change(record, original)?);
            index += 1;
        } else if record.starts_with(b"u ") {
            changes.push(parse_unmerged_change(record)?);
        } else if let Some(path) = strip_ascii_prefix(record, b"? ") {
            changes.push(GitFileChange {
                path: lossy(path),
                original_path: None,
                status: GitFileStatus::Untracked,
                index_status: "?".into(),
                worktree_status: "?".into(),
                staged: false,
                unstaged: true,
                untracked: true,
                conflicted: false,
                additions: None,
                deletions: None,
                binary: false,
                submodule: false,
                submodule_commit_changed: false,
                submodule_modified: false,
                submodule_untracked: false,
            });
        } else if let Some(path) = strip_ascii_prefix(record, b"! ") {
            // --ignored is not requested today, but keep the parser forward compatible.
            changes.push(GitFileChange {
                path: lossy(path),
                original_path: None,
                status: GitFileStatus::Ignored,
                index_status: "!".into(),
                worktree_status: "!".into(),
                staged: false,
                unstaged: false,
                untracked: false,
                conflicted: false,
                additions: None,
                deletions: None,
                binary: false,
                submodule: false,
                submodule_commit_changed: false,
                submodule_modified: false,
                submodule_untracked: false,
            });
        } else {
            return Err(format!("无法解析 Git status 记录: {}", lossy(record)));
        }
        index += 1;
    }
    let raw_head = head.unwrap_or_else(|| "(detached)".into());
    let detached = raw_head == "(detached)";
    let unborn = oid.is_none() && raw_head != "(detached)";
    Ok(ParsedStatus {
        branch: GitBranchState {
            head: (!detached).then_some(raw_head),
            oid,
            upstream,
            ahead,
            behind,
            detached,
            unborn,
        },
        changes,
        stash_count,
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct SubmoduleState {
    submodule: bool,
    commit_changed: bool,
    modified: bool,
    untracked: bool,
}

fn parse_submodule_state(value: &[u8]) -> Result<SubmoduleState, String> {
    if value == b"N..." {
        return Ok(SubmoduleState::default());
    }
    if value.len() != 4 || value[0] != b'S' {
        return Err(format!("Git submodule 状态无效: {}", lossy(value)));
    }
    let valid = |actual: u8, marker: u8| actual == b'.' || actual == marker;
    if !valid(value[1], b'C') || !valid(value[2], b'M') || !valid(value[3], b'U') {
        return Err(format!("Git submodule 状态无效: {}", lossy(value)));
    }
    Ok(SubmoduleState {
        submodule: true,
        commit_changed: value[1] == b'C',
        modified: value[2] == b'M',
        untracked: value[3] == b'U',
    })
}

fn parse_ordinary_change(record: &[u8]) -> Result<GitFileChange, String> {
    let fields = splitn_ascii(record, b' ', 9);
    if fields.len() != 9 {
        return Err("Git ordinary status 字段数量无效".into());
    }
    let (index_status, worktree_status) = parse_xy(fields[1])?;
    let submodule = parse_submodule_state(fields[2])?;
    let status = kind_from_status(&index_status, &worktree_status);
    Ok(GitFileChange {
        path: lossy(fields[8]),
        original_path: None,
        status,
        staged: index_status != ".",
        unstaged: worktree_status != ".",
        untracked: false,
        conflicted: false,
        additions: None,
        deletions: None,
        binary: false,
        submodule: submodule.submodule,
        submodule_commit_changed: submodule.commit_changed,
        submodule_modified: submodule.modified,
        submodule_untracked: submodule.untracked,
        index_status,
        worktree_status,
    })
}

fn parse_renamed_change(record: &[u8], original: &[u8]) -> Result<GitFileChange, String> {
    let fields = splitn_ascii(record, b' ', 10);
    if fields.len() != 10 {
        return Err("Git rename status 字段数量无效".into());
    }
    let (index_status, worktree_status) = parse_xy(fields[1])?;
    let submodule = parse_submodule_state(fields[2])?;
    let score = fields[8].first().copied().unwrap_or(b'R');
    Ok(GitFileChange {
        path: lossy(fields[9]),
        original_path: Some(lossy(original)),
        status: if score == b'C' {
            GitFileStatus::Copied
        } else {
            GitFileStatus::Renamed
        },
        staged: index_status != ".",
        unstaged: worktree_status != ".",
        untracked: false,
        conflicted: false,
        additions: None,
        deletions: None,
        binary: false,
        submodule: submodule.submodule,
        submodule_commit_changed: submodule.commit_changed,
        submodule_modified: submodule.modified,
        submodule_untracked: submodule.untracked,
        index_status,
        worktree_status,
    })
}

fn parse_unmerged_change(record: &[u8]) -> Result<GitFileChange, String> {
    let fields = splitn_ascii(record, b' ', 11);
    if fields.len() != 11 {
        return Err("Git conflict status 字段数量无效".into());
    }
    let (index_status, worktree_status) = parse_xy(fields[1])?;
    let submodule = parse_submodule_state(fields[2])?;
    Ok(GitFileChange {
        path: lossy(fields[10]),
        original_path: None,
        status: GitFileStatus::Unmerged,
        index_status,
        worktree_status,
        staged: true,
        unstaged: true,
        untracked: false,
        conflicted: true,
        additions: None,
        deletions: None,
        binary: false,
        submodule: submodule.submodule,
        submodule_commit_changed: submodule.commit_changed,
        submodule_modified: submodule.modified,
        submodule_untracked: submodule.untracked,
    })
}

fn parse_xy(value: &[u8]) -> Result<(String, String), String> {
    if value.len() != 2 || !value.is_ascii() {
        return Err("Git status XY 字段无效".into());
    }
    Ok((
        char::from(value[0]).to_string(),
        char::from(value[1]).to_string(),
    ))
}

fn kind_from_status(index: &str, worktree: &str) -> GitFileStatus {
    let combined = [index, worktree];
    if combined.contains(&"U") {
        GitFileStatus::Unmerged
    } else if combined.contains(&"D") {
        GitFileStatus::Deleted
    } else if combined.contains(&"A") {
        GitFileStatus::Added
    } else if combined.contains(&"R") {
        GitFileStatus::Renamed
    } else if combined.contains(&"C") {
        GitFileStatus::Copied
    } else if combined.contains(&"T") {
        GitFileStatus::TypeChanged
    } else if combined.contains(&"M") {
        GitFileStatus::Modified
    } else {
        GitFileStatus::Unknown
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FileLineStats {
    additions: u64,
    deletions: u64,
    binary: bool,
}

fn combined_line_stats(
    repository: &Repository,
    changes: &mut [GitFileChange],
) -> Result<GitLineStats, String> {
    let has_head = repository_has_head(repository)?;
    let per_file = if has_head {
        diff_numstat(repository, false)?
    } else {
        diff_numstat(repository, true)?
    };
    Ok(apply_line_stats(changes, per_file))
}

/// Folds `git diff --numstat` against HEAD (the index, before the first commit)
/// into the changes and their total. Untracked files carry no counts.
fn apply_line_stats(
    changes: &mut [GitFileChange],
    mut per_file: HashMap<String, FileLineStats>,
) -> GitLineStats {
    let mut total = GitLineStats {
        additions: 0,
        deletions: 0,
        binary_files: 0,
    };

    for change in changes {
        if change.untracked {
            change.additions = None;
            change.deletions = None;
            change.binary = false;
            continue;
        }
        let stats = per_file.remove(&change.path).unwrap_or_default();
        change.additions = (!stats.binary).then_some(stats.additions);
        change.deletions = (!stats.binary).then_some(stats.deletions);
        change.binary = stats.binary;
        total.additions = total.additions.saturating_add(stats.additions);
        total.deletions = total.deletions.saturating_add(stats.deletions);
        total.binary_files = total.binary_files.saturating_add(u32::from(stats.binary));
    }
    total
}

fn diff_numstat(
    repository: &Repository,
    staged: bool,
) -> Result<HashMap<String, FileLineStats>, String> {
    let mut args = vec![
        OsString::from("diff"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--numstat"),
        OsString::from("-z"),
    ];
    if staged {
        args.push(OsString::from("--cached"));
    } else {
        args.push(OsString::from("HEAD"));
    }
    let output = run_git(
        repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("统计 Git 变更行数", &output)?;
    if output.stdout_truncated {
        return Err("Git 变更行数统计超过安全上限，无法保证结果完整".into());
    }
    parse_numstat(&output.stdout)
}

fn parse_numstat(bytes: &[u8]) -> Result<HashMap<String, FileLineStats>, String> {
    let records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut result = HashMap::new();
    let mut index = 0;
    while index < records.len() {
        let mut fields = records[index].splitn(3, |byte| *byte == b'\t');
        let additions = fields
            .next()
            .ok_or_else(|| "Git numstat 缺少新增行数字段".to_owned())?;
        let deletions = fields
            .next()
            .ok_or_else(|| "Git numstat 缺少删除行数字段".to_owned())?;
        let inline_path = fields
            .next()
            .ok_or_else(|| "Git numstat 缺少路径字段".to_owned())?;
        let path = if inline_path.is_empty() {
            // With -z, rename/copy entries encode an empty inline path followed
            // by the source and destination as two additional NUL records.
            let destination = records
                .get(index + 2)
                .ok_or_else(|| "Git numstat rename 缺少目标路径".to_owned())?;
            index += 2;
            lossy(destination)
        } else {
            lossy(inline_path)
        };
        let binary = additions == b"-" || deletions == b"-";
        let stats = FileLineStats {
            additions: if binary {
                0
            } else {
                parse_numstat_count(additions)?
            },
            deletions: if binary {
                0
            } else {
                parse_numstat_count(deletions)?
            },
            binary,
        };
        result.insert(path, stats);
        index += 1;
    }
    Ok(result)
}

fn parse_numstat_count(value: &[u8]) -> Result<u64, String> {
    lossy(value)
        .parse::<u64>()
        .map_err(|_| "Git numstat 行数无效".to_owned())
}

fn repository_operation_state(git_dir: &Path) -> Result<Option<RepositoryOperationState>, String> {
    let path_exists = |relative: &str| {
        git_dir
            .join(relative)
            .try_exists()
            .map_err(|error| format!("无法检查 Git 操作标志 {relative}: {error}"))
    };
    let operation = if path_exists("MERGE_HEAD")? {
        GitRepositoryOperation::Merge
    } else if path_exists("rebase-merge")? || path_exists("rebase-apply")? {
        GitRepositoryOperation::Rebase
    } else if path_exists("CHERRY_PICK_HEAD")? {
        GitRepositoryOperation::CherryPick
    } else if path_exists("REVERT_HEAD")? {
        GitRepositoryOperation::Revert
    } else if path_exists("BISECT_LOG")? {
        GitRepositoryOperation::Bisect
    } else {
        return Ok(None);
    };
    let revision = repository_operation_revision(git_dir, operation)?;
    Ok(Some(RepositoryOperationState {
        operation,
        revision,
    }))
}

struct OperationRevisionBudget {
    artifacts: usize,
    content_bytes: usize,
}

fn repository_operation_revision(
    git_dir: &Path,
    operation: GitRepositoryOperation,
) -> Result<String, String> {
    let mut roots = match operation {
        GitRepositoryOperation::Merge => vec![
            PathBuf::from("MERGE_HEAD"),
            PathBuf::from("MERGE_MODE"),
            PathBuf::from("MERGE_MSG"),
            PathBuf::from("AUTO_MERGE"),
            PathBuf::from("MERGE_RR"),
        ],
        GitRepositoryOperation::Rebase => vec![
            PathBuf::from("rebase-merge"),
            PathBuf::from("rebase-apply"),
            PathBuf::from("REBASE_HEAD"),
            PathBuf::from("sequencer"),
        ],
        GitRepositoryOperation::CherryPick => vec![
            PathBuf::from("CHERRY_PICK_HEAD"),
            PathBuf::from("MERGE_MSG"),
            PathBuf::from("sequencer"),
        ],
        GitRepositoryOperation::Revert => vec![
            PathBuf::from("REVERT_HEAD"),
            PathBuf::from("MERGE_MSG"),
            PathBuf::from("sequencer"),
        ],
        GitRepositoryOperation::Bisect => {
            let mut roots = vec![PathBuf::from("refs/bisect")];
            let entries = fs::read_dir(git_dir)
                .map_err(|error| format!("无法读取 Git 操作目录 {}: {error}", git_dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    format!(
                        "无法枚举 Git bisect 操作标志 {}: {error}",
                        git_dir.display()
                    )
                })?;
                if entry.file_name().to_string_lossy().starts_with("BISECT_") {
                    roots.push(PathBuf::from(entry.file_name()));
                }
            }
            roots
        }
    };
    roots.sort();
    roots.dedup();
    let mut digest = Sha256::new();
    digest.update(b"mework.git.operation-revision.v1\0");
    update_revision_component(
        &mut digest,
        b"operation",
        repository_operation_label(operation).as_bytes(),
    );
    let mut budget = OperationRevisionBudget {
        artifacts: 0,
        content_bytes: 0,
    };
    for relative in roots {
        hash_operation_artifact(git_dir, &relative, &mut digest, &mut budget)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_operation_artifact(
    git_dir: &Path,
    relative: &Path,
    digest: &mut Sha256,
    budget: &mut OperationRevisionBudget,
) -> Result<(), String> {
    budget.artifacts = budget.artifacts.saturating_add(1);
    if budget.artifacts > MAX_OPERATION_REVISION_ARTIFACTS {
        return Err(format!(
            "Git 操作元数据超过 {MAX_OPERATION_REVISION_ARTIFACTS} 个条目，无法生成安全修订"
        ));
    }
    update_revision_component(
        digest,
        b"artifact-path",
        relative.as_os_str().as_encoded_bytes(),
    );
    let path = git_dir.join(relative);
    let before = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            update_revision_component(digest, b"artifact-type", b"absent");
            return Ok(());
        }
        Err(error) => {
            return Err(format!(
                "无法读取 Git 操作元数据 {}: {error}",
                relative.display()
            ))
        }
    };
    let created = before.created().ok();
    let modified = before.modified().map_err(|error| {
        format!(
            "无法读取 Git 操作元数据 {} 的修改时间: {error}",
            relative.display()
        )
    })?;
    hash_operation_optional_timestamp(digest, b"artifact-created", created);
    hash_operation_timestamp(digest, b"artifact-modified", modified);
    hash_operation_metadata_identity(digest, &before);
    update_revision_component(digest, b"artifact-len", &before.len().to_be_bytes());

    if before.file_type().is_symlink() {
        update_revision_component(digest, b"artifact-type", b"symlink");
        let target = fs::read_link(&path).map_err(|error| {
            format!("无法读取 Git 操作符号链接 {}: {error}", relative.display())
        })?;
        update_revision_component(
            digest,
            b"artifact-symlink-target",
            target.as_os_str().as_encoded_bytes(),
        );
    } else if before.is_dir() {
        update_revision_component(digest, b"artifact-type", b"directory");
        let entries = fs::read_dir(&path).map_err(|error| {
            format!(
                "无法读取 Git 操作元数据目录 {}: {error}",
                relative.display()
            )
        })?;
        let mut children = entries
            .map(|entry| {
                entry
                    .map(|entry| relative.join(entry.file_name()))
                    .map_err(|error| {
                        format!(
                            "无法枚举 Git 操作元数据目录 {}: {error}",
                            relative.display()
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        for child in children {
            hash_operation_artifact(git_dir, &child, digest, budget)?;
        }
    } else if before.is_file() {
        update_revision_component(digest, b"artifact-type", b"file");
        let remaining = MAX_OPERATION_REVISION_CONTENT_BYTES.saturating_sub(budget.content_bytes);
        let content_limit = usize::try_from(before.len())
            .unwrap_or(usize::MAX)
            .min(MAX_OPERATION_REVISION_FILE_BYTES)
            .min(remaining);
        let mut file = fs::File::open(&path)
            .map_err(|error| format!("无法打开 Git 操作元数据 {}: {error}", relative.display()))?;
        let mut content = Vec::with_capacity(content_limit);
        Read::by_ref(&mut file)
            .take(content_limit as u64)
            .read_to_end(&mut content)
            .map_err(|error| format!("无法读取 Git 操作元数据 {}: {error}", relative.display()))?;
        budget.content_bytes = budget.content_bytes.saturating_add(content.len());
        update_revision_component(digest, b"artifact-content-prefix", &content);
        update_revision_component(
            digest,
            b"artifact-content-complete",
            if before.len() == content.len() as u64 {
                b"true"
            } else {
                b"false"
            },
        );
        if before.len() <= content_limit as u64 && content.len() as u64 != before.len() {
            return Err(format!(
                "Git 操作元数据在读取时缩短，请刷新后重试: {}",
                relative.display()
            ));
        }
    } else {
        return Err(format!(
            "Git 操作元数据类型不受支持: {}",
            relative.display()
        ));
    }

    let after = fs::symlink_metadata(&path)
        .map_err(|error| format!("无法复核 Git 操作元数据 {}: {error}", relative.display()))?;
    let after_created = after.created().ok();
    let after_modified = after.modified().map_err(|error| {
        format!(
            "无法复核 Git 操作元数据 {} 的修改时间: {error}",
            relative.display()
        )
    })?;
    if before.file_type() != after.file_type()
        || before.len() != after.len()
        || created != after_created
        || modified != after_modified
    {
        return Err(format!(
            "Git 操作元数据在生成修订时发生变化，请刷新后重试: {}",
            relative.display()
        ));
    }
    Ok(())
}

fn hash_operation_timestamp(digest: &mut Sha256, label: &[u8], timestamp: SystemTime) {
    let (sign, duration) = match timestamp.duration_since(UNIX_EPOCH) {
        Ok(duration) => (b'+', duration),
        Err(error) => (b'-', error.duration()),
    };
    let mut encoded = Vec::with_capacity(13);
    encoded.push(sign);
    encoded.extend_from_slice(&duration.as_secs().to_be_bytes());
    encoded.extend_from_slice(&duration.subsec_nanos().to_be_bytes());
    update_revision_component(digest, label, &encoded);
}

fn hash_operation_optional_timestamp(
    digest: &mut Sha256,
    label: &[u8],
    timestamp: Option<SystemTime>,
) {
    match timestamp {
        Some(timestamp) => hash_operation_timestamp(digest, label, timestamp),
        None => update_revision_component(digest, label, b"unavailable"),
    }
}

#[cfg(unix)]
fn hash_operation_metadata_identity(digest: &mut Sha256, metadata: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;

    let mut identity = Vec::with_capacity(32);
    identity.extend_from_slice(&metadata.dev().to_be_bytes());
    identity.extend_from_slice(&metadata.ino().to_be_bytes());
    identity.extend_from_slice(&metadata.ctime().to_be_bytes());
    identity.extend_from_slice(&metadata.ctime_nsec().to_be_bytes());
    update_revision_component(digest, b"artifact-identity", &identity);
}

#[cfg(windows)]
fn hash_operation_metadata_identity(digest: &mut Sha256, metadata: &fs::Metadata) {
    use std::os::windows::fs::MetadataExt;

    let mut identity = Vec::with_capacity(8);
    identity.extend_from_slice(&metadata.creation_time().to_be_bytes());
    update_revision_component(digest, b"artifact-identity", &identity);
}

#[cfg(not(any(unix, windows)))]
fn hash_operation_metadata_identity(digest: &mut Sha256, _metadata: &fs::Metadata) {
    update_revision_component(digest, b"artifact-identity", b"unavailable");
}

fn repository_operation_label(operation: GitRepositoryOperation) -> &'static str {
    match operation {
        GitRepositoryOperation::Merge => "merge",
        GitRepositoryOperation::Rebase => "rebase",
        GitRepositoryOperation::CherryPick => "cherry-pick",
        GitRepositoryOperation::Revert => "revert",
        GitRepositoryOperation::Bisect => "bisect",
    }
}

fn expected_operation_for_action(
    action: &GitAction,
) -> Option<(GitRepositoryOperation, &str, &str)> {
    match action {
        GitAction::ContinueOperation {
            operation,
            expected_head,
            expected_operation_revision,
        }
        | GitAction::SkipOperation {
            operation,
            expected_head,
            expected_operation_revision,
        }
        | GitAction::AbortOperation {
            operation,
            expected_head,
            expected_operation_revision,
        } => Some((*operation, expected_head, expected_operation_revision)),
        GitAction::BisectStep {
            expected_head,
            expected_operation_revision,
            ..
        } => Some((
            GitRepositoryOperation::Bisect,
            expected_head,
            expected_operation_revision,
        )),
        _ => None,
    }
}

fn validate_action_during_repository_operation(
    repository: &Repository,
    action: &GitAction,
) -> Result<(), String> {
    let current = repository_operation_state(&repository.git_dir)?;
    let expected = expected_operation_for_action(action);
    match (current.as_ref(), expected) {
        (None, Some((expected, _, _))) => Err(format!(
            "Git {} 操作已经结束；请刷新仓库状态后重试",
            repository_operation_label(expected)
        )),
        (Some(current), Some((expected, _, _))) if current.operation != expected => Err(format!(
            "仓库当前正在执行 Git {}，不是请求中的 {}；请刷新后重试",
            repository_operation_label(current.operation),
            repository_operation_label(expected)
        )),
        (Some(current), Some((_, expected_head, expected_revision))) => {
            let expected_head = validate_object_id("Git 操作起始提交", expected_head.to_owned())?;
            let actual_head = resolve_commit(repository, "HEAD")?;
            if actual_head != expected_head {
                return Err("仓库 HEAD 已在确认后发生变化；请刷新并重新确认当前 Git 操作".into());
            }
            let expected_revision = validate_revision_token("Git 操作修订", expected_revision)?;
            if current.revision != expected_revision {
                return Err("当前 Git 操作已在确认后变化或重新开始；请刷新并重新确认操作".into());
            }
            Ok(())
        }
        (Some(_), None)
            if matches!(
                action,
                GitAction::Stage { .. } | GitAction::Unstage { .. } | GitAction::Discard { .. }
            ) =>
        {
            Ok(())
        }
        (Some(current), None) => Err(format!(
            "仓库正在执行 Git {}；请先解决冲突并继续，或中止当前操作",
            repository_operation_label(current.operation)
        )),
        (None, None) => Ok(()),
    }
}

fn validate_revision_token(label: &str, value: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label}无效；请刷新仓库状态后重试"));
    }
    Ok(value.to_ascii_lowercase())
}

#[derive(Clone, Copy)]
enum RepositoryOperationControl {
    Continue,
    Skip,
    Abort,
}

fn prepare_repository_operation_action(
    operation: GitRepositoryOperation,
    control: RepositoryOperationControl,
) -> Result<Vec<OsString>, String> {
    let args = match (operation, control) {
        (GitRepositoryOperation::Merge, RepositoryOperationControl::Continue) => {
            vec![OsString::from("commit"), OsString::from("--no-edit")]
        }
        (GitRepositoryOperation::Rebase, RepositoryOperationControl::Continue) => vec![
            OsString::from("-c"),
            OsString::from("core.editor=true"),
            OsString::from("rebase"),
            OsString::from("--continue"),
        ],
        (GitRepositoryOperation::CherryPick, RepositoryOperationControl::Continue) => {
            vec![OsString::from("cherry-pick"), OsString::from("--continue")]
        }
        (GitRepositoryOperation::Revert, RepositoryOperationControl::Continue) => {
            vec![OsString::from("revert"), OsString::from("--continue")]
        }
        (GitRepositoryOperation::Bisect, RepositoryOperationControl::Continue) => {
            return Err(
                "Git bisect 需要先标记当前提交为 good 或 bad；当前界面只支持结束二分查找".into(),
            );
        }
        (GitRepositoryOperation::Merge, RepositoryOperationControl::Skip)
        | (GitRepositoryOperation::Bisect, RepositoryOperationControl::Skip) => {
            return Err(format!(
                "Git {} 不支持跳过当前提交",
                repository_operation_label(operation)
            ));
        }
        (GitRepositoryOperation::Rebase, RepositoryOperationControl::Skip) => {
            vec![OsString::from("rebase"), OsString::from("--skip")]
        }
        (GitRepositoryOperation::CherryPick, RepositoryOperationControl::Skip) => {
            vec![OsString::from("cherry-pick"), OsString::from("--skip")]
        }
        (GitRepositoryOperation::Revert, RepositoryOperationControl::Skip) => {
            vec![OsString::from("revert"), OsString::from("--skip")]
        }
        (GitRepositoryOperation::Merge, RepositoryOperationControl::Abort) => {
            vec![OsString::from("merge"), OsString::from("--abort")]
        }
        (GitRepositoryOperation::Rebase, RepositoryOperationControl::Abort) => {
            vec![OsString::from("rebase"), OsString::from("--abort")]
        }
        (GitRepositoryOperation::CherryPick, RepositoryOperationControl::Abort) => {
            vec![OsString::from("cherry-pick"), OsString::from("--abort")]
        }
        (GitRepositoryOperation::Revert, RepositoryOperationControl::Abort) => {
            vec![OsString::from("revert"), OsString::from("--abort")]
        }
        (GitRepositoryOperation::Bisect, RepositoryOperationControl::Abort) => {
            vec![OsString::from("bisect"), OsString::from("reset")]
        }
    };
    Ok(args)
}

fn parse_bisect_term_output(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_GIT_BISECT_TERM_BYTES {
        return Err("Git bisect 自定义术语超过安全上限".into());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Git bisect 自定义术语不是有效 UTF-8".to_owned())?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let term = text.strip_suffix('\r').unwrap_or(text);
    if term.is_empty() || term.as_bytes().contains(&0) || term.contains('\r') || term.contains('\n')
    {
        return Err("Git bisect 自定义术语格式无效".into());
    }
    Ok(term.to_owned())
}

fn resolve_bisect_step_command(
    repository: &Repository,
    outcome: GitBisectOutcome,
) -> Result<Vec<OsString>, String> {
    if outcome == GitBisectOutcome::Skip {
        return Ok(vec![OsString::from("bisect"), OsString::from("skip")]);
    }
    let term_flag = match outcome {
        GitBisectOutcome::Old => "--term-old",
        GitBisectOutcome::New => "--term-new",
        GitBisectOutcome::Skip => unreachable!("skip returns before resolving a bisect term"),
    };
    let output = run_git(
        repository,
        [
            OsString::from("bisect"),
            OsString::from("terms"),
            OsString::from(term_flag),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_GIT_BISECT_TERM_BYTES,
        true,
    )?;
    require_success("读取 Git bisect 自定义术语", &output)?;
    if output.stdout_truncated {
        return Err("Git bisect 自定义术语超过安全上限".into());
    }
    Ok(vec![
        OsString::from("bisect"),
        OsString::from(parse_bisect_term_output(&output.stdout)?),
    ])
}

fn branches_for_repository(repository: &Repository) -> Result<Vec<GitBranch>, String> {
    let format = "%(refname)%00%(refname:short)%00%(objectname)%00%(upstream:short)%00%(upstream:track)%00%(HEAD)";
    let output = run_git(
        repository,
        [
            OsString::from("for-each-ref"),
            OsString::from(format!("--format={format}")),
            OsString::from("refs/heads"),
            OsString::from("refs/remotes"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
        true,
    )?;
    require_success("读取 Git 分支", &output)?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut branches = Vec::new();
    for line in text.lines() {
        let fields = line.split('\0').collect::<Vec<_>>();
        if fields.len() != 6 {
            return Err("Git 分支输出字段数量无效".into());
        }
        if fields[0].ends_with("/HEAD") {
            continue;
        }
        let kind = if fields[0].starts_with("refs/heads/") {
            GitBranchKind::Local
        } else {
            GitBranchKind::Remote
        };
        let (ahead, behind) = parse_upstream_track(fields[4]);
        branches.push(GitBranch {
            full_name: fields[0].to_owned(),
            name: fields[1].to_owned(),
            kind,
            head: nonempty(fields[2]),
            upstream: nonempty(fields[3]),
            ahead,
            behind,
            current: fields[5] == "*",
            merged: None,
        });
    }
    Ok(branches)
}

fn parse_upstream_track(value: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    let value = value.trim().trim_start_matches('[').trim_end_matches(']');
    for part in value.split(',').map(str::trim) {
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

fn default_branch_for_repository(repository: &Repository) -> Result<Option<String>, String> {
    let output = run_git(
        repository,
        [
            OsString::from("symbolic-ref"),
            OsString::from("--quiet"),
            OsString::from("--short"),
            OsString::from("refs/remotes/origin/HEAD"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        16 * 1024,
        true,
    )?;
    if !output.success() {
        return Ok(None);
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(branch
        .strip_prefix("origin/")
        .map(str::to_owned)
        .or_else(|| nonempty(&branch)))
}

fn prepare_git_action(
    repository: &Repository,
    action: GitAction,
) -> Result<(Vec<OsString>, Option<Vec<u8>>, Duration), String> {
    match action {
        GitAction::Stage { paths } => {
            let input = encode_pathspecs(&paths)?;
            Ok((
                vec![
                    OsString::from("--literal-pathspecs"),
                    OsString::from("add"),
                    OsString::from("--all"),
                    OsString::from("--pathspec-from-file=-"),
                    OsString::from("--pathspec-file-nul"),
                ],
                Some(input),
                LOCAL_COMMAND_TIMEOUT,
            ))
        }
        GitAction::Unstage { paths } => {
            let input = encode_pathspecs(&paths)?;
            let command = if repository_has_head(repository)? {
                "restore"
            } else {
                "rm"
            };
            let mut args = vec![
                OsString::from("--literal-pathspecs"),
                OsString::from(command),
            ];
            if command == "restore" {
                args.push(OsString::from("--staged"));
            } else {
                args.extend([
                    OsString::from("--cached"),
                    OsString::from("--ignore-unmatch"),
                ]);
            }
            args.extend([
                OsString::from("--pathspec-from-file=-"),
                OsString::from("--pathspec-file-nul"),
            ]);
            Ok((args, Some(input), LOCAL_COMMAND_TIMEOUT))
        }
        GitAction::Discard { .. } => {
            unreachable!("discard is handled before preparing a single command")
        }
        GitAction::Checkout { branch: name } => {
            validate_local_branch(repository, &name)?;
            Ok((
                vec![
                    OsString::from("switch"),
                    OsString::from("--no-guess"),
                    OsString::from(name),
                ],
                None,
                LOCAL_COMMAND_TIMEOUT,
            ))
        }
        GitAction::ContinueOperation { operation, .. } => Ok((
            prepare_repository_operation_action(operation, RepositoryOperationControl::Continue)?,
            None,
            LOCAL_COMMAND_TIMEOUT,
        )),
        GitAction::SkipOperation { operation, .. } => Ok((
            prepare_repository_operation_action(operation, RepositoryOperationControl::Skip)?,
            None,
            LOCAL_COMMAND_TIMEOUT,
        )),
        GitAction::AbortOperation { operation, .. } => Ok((
            prepare_repository_operation_action(operation, RepositoryOperationControl::Abort)?,
            None,
            LOCAL_COMMAND_TIMEOUT,
        )),
        GitAction::BisectStep { outcome, .. } => Ok((
            resolve_bisect_step_command(repository, outcome)?,
            None,
            LOCAL_COMMAND_TIMEOUT,
        )),
    }
}

fn discard_selection(
    snapshot: &GitWorkspaceSnapshot,
    paths: &[String],
) -> Result<Vec<GitFileChange>, String> {
    let encoded = encode_pathspecs(paths)?;
    let mut normalized = encoded
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(lossy)
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    let mut selection = Vec::with_capacity(normalized.len());
    for path in normalized {
        let change = snapshot
            .files
            .iter()
            .find(|change| change.path == path)
            .ok_or_else(|| format!("只能丢弃当前 Git 变更列表中的路径: {path}"))?;
        if change.submodule {
            return Err(format!(
                "Mework 不会从父仓库递归丢弃子模块 {} 的内部变更；请将该子模块作为独立工作区处理",
                change.path
            ));
        }
        selection.push(change.clone());
    }
    Ok(selection)
}

fn git_file_status_label(status: GitFileStatus) -> &'static [u8] {
    match status {
        GitFileStatus::Modified => b"modified",
        GitFileStatus::Added => b"added",
        GitFileStatus::Deleted => b"deleted",
        GitFileStatus::Renamed => b"renamed",
        GitFileStatus::Copied => b"copied",
        GitFileStatus::TypeChanged => b"type-changed",
        GitFileStatus::Unmerged => b"unmerged",
        GitFileStatus::Untracked => b"untracked",
        GitFileStatus::Ignored => b"ignored",
        GitFileStatus::Unknown => b"unknown",
    }
}

fn target_proof_remaining(deadline: Instant, action_label: &str) -> Result<Duration, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(format!(
            "计算 Git {action_label}目标修订超时；未执行任何更改"
        ))
    } else {
        Ok(remaining)
    }
}

fn os_argument_cost(value: &OsStr) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value
            .encode_wide()
            .count()
            .saturating_mul(2)
            .saturating_add(2)
    }
    #[cfg(not(windows))]
    {
        value.as_encoded_bytes().len().saturating_add(1)
    }
}

fn discard_argument_batches(values: &[OsString]) -> Vec<&[OsString]> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < values.len() {
        let mut end = start;
        let mut cost = 0_usize;
        while end < values.len() {
            let next = os_argument_cost(&values[end]);
            if end > start && cost.saturating_add(next) > DISCARD_HASH_OBJECT_ARG_BUDGET {
                break;
            }
            cost = cost.saturating_add(next);
            end += 1;
        }
        batches.push(&values[start..end]);
        start = end;
    }
    batches
}

fn discard_target_revision(
    repository: &Repository,
    selection: &[GitFileChange],
    include_untracked: bool,
) -> Result<String, String> {
    selected_target_revision(
        repository,
        selection,
        include_untracked,
        DISCARD_TARGET_REVISION_TIMEOUT,
        b"mework.git.discard-target-revision.v1\0",
        "丢弃",
    )
}

fn selected_target_revision(
    repository: &Repository,
    selection: &[GitFileChange],
    include_untracked: bool,
    timeout: Duration,
    domain: &[u8],
    action_label: &str,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut digest = Sha256::new();
    digest.update(domain);
    update_revision_component(
        &mut digest,
        b"repository-root",
        repository.root.as_os_str().as_encoded_bytes(),
    );
    update_revision_component(
        &mut digest,
        b"include-untracked",
        &[u8::from(include_untracked)],
    );

    let mut index_paths = Vec::new();
    let mut regular_files = Vec::<(String, PathBuf)>::new();
    for change in selection {
        target_proof_remaining(deadline, action_label)?;
        if change.untracked && !include_untracked {
            return Err(format!(
                "未跟踪文件 {} 只有在明确允许删除未跟踪文件时才能丢弃",
                change.path
            ));
        }
        update_revision_component(&mut digest, b"path", change.path.as_bytes());
        update_revision_component(
            &mut digest,
            b"original-path",
            change.original_path.as_deref().unwrap_or("").as_bytes(),
        );
        update_revision_component(&mut digest, b"status", git_file_status_label(change.status));
        update_revision_component(&mut digest, b"index-status", change.index_status.as_bytes());
        update_revision_component(
            &mut digest,
            b"worktree-status",
            change.worktree_status.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"flags",
            &[
                u8::from(change.staged),
                u8::from(change.unstaged),
                u8::from(change.untracked),
                u8::from(change.conflicted),
            ],
        );

        if !change.untracked {
            index_paths.push(change.path.clone());
            if let Some(original) = change.original_path.as_ref() {
                index_paths.push(original.clone());
            }
        }
        let path = repository.root.join(&change.path);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&path).map_err(|error| {
                    format!(
                        "无法读取 Git {action_label}目标符号链接 {}: {error}",
                        change.path
                    )
                })?;
                update_revision_component(&mut digest, b"worktree-kind", b"symlink");
                update_revision_component(
                    &mut digest,
                    b"symlink-target",
                    target.as_os_str().as_encoded_bytes(),
                );
            }
            Ok(metadata) if metadata.is_file() => {
                let canonical = fs::canonicalize(&path).map_err(|error| {
                    format!("无法访问 Git {action_label}目标 {}: {error}", change.path)
                })?;
                if !canonical.starts_with(&repository.root) {
                    return Err(format!("Git {action_label}目标越出仓库: {}", change.path));
                }
                update_revision_component(&mut digest, b"worktree-kind", b"regular");
                hash_target_regular_file_mode(&mut digest, &metadata);
                regular_files.push((change.path.clone(), PathBuf::from(&change.path)));
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(format!(
                    "Git {action_label}目标不是普通文件: {}",
                    change.path
                ));
            }
            Ok(_) => {
                return Err(format!(
                    "Git {action_label}目标类型不受支持: {}",
                    change.path
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                update_revision_component(&mut digest, b"worktree-kind", b"missing");
            }
            Err(error) => {
                return Err(format!(
                    "无法检查 Git {action_label}目标 {}: {error}",
                    change.path
                ));
            }
        }
    }

    index_paths.sort_unstable();
    index_paths.dedup();
    let index_args = index_paths.iter().map(OsString::from).collect::<Vec<_>>();
    for batch in discard_argument_batches(&index_args) {
        for path in batch {
            update_revision_component(
                &mut digest,
                b"index-path",
                path.as_os_str().as_encoded_bytes(),
            );
        }
        let mut args = vec![
            OsString::from("--literal-pathspecs"),
            OsString::from("ls-files"),
            OsString::from("--stage"),
            OsString::from("-z"),
            OsString::from("--"),
        ];
        args.extend(batch.iter().cloned());
        let output = run_git(
            repository,
            args,
            None,
            target_proof_remaining(deadline, action_label)?,
            0,
            true,
        )?;
        if output.timed_out {
            return Err(format!(
                "计算 Git {action_label}目标索引修订超时；未执行任何更改"
            ));
        }
        require_success(&format!("读取 Git {action_label}目标索引来源"), &output)?;
        update_revision_component(&mut digest, b"index-source-digest", &output.stdout_sha256);
    }

    let regular_args = regular_files
        .iter()
        .map(|(_, path)| path.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    let mut regular_offset = 0_usize;
    for batch in discard_argument_batches(&regular_args) {
        let mut args = vec![
            OsString::from("hash-object"),
            OsString::from("--no-filters"),
            OsString::from("--"),
        ];
        args.extend(batch.iter().cloned());
        let output_limit = batch
            .len()
            .saturating_mul(66)
            .saturating_add(1024)
            .min(MAX_STATUS_OUTPUT);
        let output = run_git(
            repository,
            args,
            None,
            target_proof_remaining(deadline, action_label)?,
            output_limit,
            true,
        )?;
        if output.timed_out {
            return Err(format!(
                "计算 Git {action_label}目标内容修订超时；未执行任何更改"
            ));
        }
        require_success(&format!("计算 Git {action_label}目标内容修订"), &output)?;
        if output.stdout_truncated {
            return Err(format!("Git {action_label}目标内容修订输出超过安全上限"));
        }
        let object_id_output = String::from_utf8_lossy(&output.stdout);
        let object_ids = object_id_output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if object_ids.len() != batch.len() {
            return Err(format!(
                "Git {action_label}目标内容修订数量与请求路径不一致"
            ));
        }
        for (index, object_id) in object_ids.into_iter().enumerate() {
            if !matches!(object_id.len(), 40 | 64)
                || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!("Git {action_label}目标内容修订无效"));
            }
            let (relative, _) = &regular_files[regular_offset + index];
            update_revision_component(&mut digest, b"content-path", relative.as_bytes());
            update_revision_component(
                &mut digest,
                b"content-object-id",
                object_id.to_ascii_lowercase().as_bytes(),
            );
        }
        regular_offset += batch.len();
    }
    target_proof_remaining(deadline, action_label)?;
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn hash_target_regular_file_mode(digest: &mut Sha256, metadata: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    update_revision_component(
        digest,
        b"worktree-executable",
        &[u8::from(metadata.mode() & 0o111 != 0)],
    );
}

#[cfg(not(unix))]
fn hash_target_regular_file_mode(digest: &mut Sha256, _metadata: &fs::Metadata) {
    update_revision_component(digest, b"worktree-executable", b"platform-ignored");
}

fn execute_discard(
    repository: &Repository,
    paths: &[String],
    include_untracked: bool,
    expected_content_revision: &str,
    expected_target_revision: &str,
) -> Result<CliOutput, String> {
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let expected_target_revision =
        validate_revision_token("Git 丢弃目标修订", expected_target_revision)?;
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.content_revision != expected_content_revision {
        return Err("文件内容已在确认后发生变化；请刷新差异并重新确认丢弃".into());
    }
    let selection = discard_selection(&snapshot, paths)?;
    let actual_target_revision =
        discard_target_revision(repository, &selection, include_untracked)?;
    if actual_target_revision != expected_target_revision {
        return Err("待丢弃文件已在确认后发生变化；请刷新差异并重新确认丢弃".into());
    }
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    for change in selection {
        if change.untracked {
            untracked.push(change.path);
        } else {
            tracked.push(change.path);
        }
    }
    let mut output = if tracked.is_empty() {
        run_git(
            repository,
            [
                OsString::from("--no-optional-locks"),
                OsString::from("rev-parse"),
                OsString::from("--is-inside-work-tree"),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            16 * 1024,
            true,
        )?
    } else {
        let input = encode_pathspecs(&tracked)?;
        let output = run_git(
            repository,
            [
                OsString::from("--literal-pathspecs"),
                OsString::from("restore"),
                OsString::from("--worktree"),
                OsString::from("--pathspec-from-file=-"),
                OsString::from("--pathspec-file-nul"),
            ],
            Some(input),
            LOCAL_COMMAND_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        )?;
        if !output.success() {
            return Ok(output);
        }
        output
    };
    let mut removed = 0_u32;
    if include_untracked {
        for path in &untracked {
            remove_exact_untracked_file(&repository.root, path)?;
            removed = removed.saturating_add(1);
        }
    }
    let mut message = format!("已丢弃 {} 个未暂存变更", tracked.len());
    if include_untracked {
        message.push_str(&format!("，删除 {removed} 个未跟踪文件"));
    } else if !untracked.is_empty() {
        message.push_str(&format!("；保留 {} 个未跟踪文件", untracked.len()));
    }
    output.stdout = message.into_bytes();
    output.stdout_sha256 = Sha256::digest(&output.stdout).into();
    output.stderr.clear();
    output.stdout_truncated = false;
    output.stderr_truncated = false;
    Ok(output)
}

fn remove_exact_untracked_file(root: &Path, relative: &str) -> Result<(), String> {
    let relative = validate_relative_path(relative)?;
    remove_file_beneath_root(root, Path::new(&relative))
}

#[cfg(windows)]
fn remove_file_beneath_root(root: &Path, relative: &Path) -> Result<(), String> {
    use std::{
        mem::{size_of, zeroed},
        os::windows::{
            ffi::OsStringExt,
            io::{AsRawHandle, OwnedHandle},
        },
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FileDispositionInfo, FileDispositionInfoEx, GetFileInformationByHandle,
            GetFinalPathNameByHandleW, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
        },
    };

    fn open_handle(
        path: &Path,
        access: u32,
        open_reparse_point: bool,
    ) -> Result<OwnedHandle, String> {
        use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        wide.push(0);
        let flags = FILE_FLAG_BACKUP_SEMANTICS
            | if open_reparse_point {
                FILE_FLAG_OPEN_REPARSE_POINT
            } else {
                0
            };
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
    }

    fn final_handle_path(handle: HANDLE) -> Result<PathBuf, String> {
        let mut buffer = vec![0_u16; 512];
        loop {
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    handle,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    VOLUME_NAME_DOS,
                )
            };
            if length == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if (length as usize) < buffer.len() {
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer[..length as usize],
                )));
            }
            buffer.resize(length as usize + 1, 0);
        }
    }

    fn normalized_handle_path(path: &Path) -> String {
        path.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    }

    let root_handle = open_handle(root, FILE_READ_ATTRIBUTES, false)
        .map_err(|error| format!("无法锁定 Git 仓库根目录 {}: {error}", root.display()))?;
    let target_path = root.join(relative);
    let target_handle = open_handle(&target_path, DELETE | FILE_READ_ATTRIBUTES, true)
        .map_err(|error| format!("无法打开未跟踪文件 {}: {error}", relative.to_string_lossy()))?;

    let root_final = final_handle_path(root_handle.as_raw_handle().cast())
        .map_err(|error| format!("无法验证 Git 仓库根目录句柄: {error}"))?;
    let target_final = final_handle_path(target_handle.as_raw_handle().cast())
        .map_err(|error| format!("无法验证未跟踪文件 {}: {error}", relative.to_string_lossy()))?;
    let root_final = normalized_handle_path(&root_final);
    let target_final = normalized_handle_path(&target_final);
    let descendant_prefix = format!("{root_final}\\");
    if !target_final.starts_with(&descendant_prefix) {
        return Err(format!(
            "拒绝删除越出 Git 仓库的未跟踪文件: {}",
            relative.to_string_lossy()
        ));
    }

    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe {
        GetFileInformationByHandle(
            target_handle.as_raw_handle().cast(),
            &mut information as *mut _,
        )
    } == 0
    {
        return Err(format!(
            "无法检查未跟踪文件 {}: {}",
            relative.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(format!(
            "拒绝递归删除未跟踪目录: {}",
            relative.to_string_lossy()
        ));
    }

    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    let deleted = unsafe {
        SetFileInformationByHandle(
            target_handle.as_raw_handle().cast(),
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if deleted == 0 {
        let fallback = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                target_handle.as_raw_handle().cast(),
                FileDispositionInfo,
                (&fallback as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(format!(
                "无法删除未跟踪文件 {}: {}",
                relative.to_string_lossy(),
                std::io::Error::last_os_error()
            ));
        }
    }
    drop(target_handle);
    Ok(())
}

#[cfg(unix)]
fn remove_file_beneath_root(root: &Path, relative: &Path) -> Result<(), String> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };

    fn c_path(value: &std::ffi::OsStr) -> Result<CString, String> {
        CString::new(value.as_bytes()).map_err(|_| "Git 路径包含 NUL 字节".to_owned())
    }

    let root_path = c_path(root.as_os_str())?;
    let root_fd = unsafe {
        libc::open(
            root_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "无法锁定 Git 仓库根目录 {}: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let mut components = relative.components().peekable();
    let Some(Component::Normal(first)) = components.next() else {
        return Err("未跟踪文件路径无效".into());
    };
    let mut current = first;
    while components.peek().is_some() {
        let name = c_path(current)?;
        let next_fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next_fd < 0 {
            return Err(format!(
                "拒绝沿符号链接访问未跟踪文件 {}: {}",
                relative.display(),
                std::io::Error::last_os_error()
            ));
        }
        directory = unsafe { OwnedFd::from_raw_fd(next_fd) };
        let Some(Component::Normal(next)) = components.next() else {
            return Err("未跟踪文件路径无效".into());
        };
        current = next;
    }

    let name = c_path(current)?;
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(format!(
            "无法检查未跟踪文件 {}: {}",
            relative.display(),
            std::io::Error::last_os_error()
        ));
    }
    if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
        return Err(format!("拒绝递归删除未跟踪目录: {}", relative.display()));
    }
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(format!(
            "无法删除未跟踪文件 {}: {}",
            relative.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn encode_pathspecs(paths: &[String]) -> Result<Vec<u8>, String> {
    if paths.is_empty() {
        return Err("Git 文件操作至少需要一个路径".into());
    }
    if paths.len() > MAX_PATHS_PER_ACTION {
        return Err(format!(
            "Git 文件操作一次最多接受 {MAX_PATHS_PER_ACTION} 个路径"
        ));
    }
    let mut input = Vec::new();
    for path in paths {
        let path = validate_relative_path(path)?;
        if path == "." {
            return Err("批量路径操作不接受工作区根目录；请使用 stageAll".into());
        }
        input.extend_from_slice(path.as_bytes());
        input.push(0);
        if input.len() > MAX_PATH_BYTES_PER_ACTION {
            return Err("Git 文件操作路径总长度超过 1 MiB".into());
        }
    }
    Ok(input)
}

fn validate_relative_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.contains('\0') {
        return Err("Git 路径不能为空或包含 NUL".into());
    }
    if path.contains('\\') {
        return Err("Git 路径必须使用 / 分隔".into());
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err("Git 路径必须相对于仓库根目录".into());
    }
    let mut components = Vec::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => components.push(part.to_string_lossy().into_owned()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("Git 路径不得越出仓库根目录".into())
            }
        }
    }
    if components.is_empty() {
        return Ok(".".into());
    }
    Ok(components.join("/"))
}

fn canonical_existing_repo_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let candidate = root.join(relative);
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("无法访问 Git 文件 {relative}: {error}"))?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(format!("Git 文件越出仓库或不是普通文件: {relative}"));
    }
    Ok(canonical)
}

fn validate_branch_name(repository: &Repository, name: &str) -> Result<(), String> {
    if name.trim() != name || name.is_empty() || name.contains('\0') {
        return Err("Git 分支名称无效".into());
    }
    let output = run_git(
        repository,
        [
            OsString::from("check-ref-format"),
            OsString::from("--branch"),
            OsString::from(name),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        16 * 1024,
        true,
    )?;
    require_success("验证 Git 分支名称", &output)
}

fn validate_local_branch(repository: &Repository, name: &str) -> Result<(), String> {
    validate_branch_name(repository, name)?;
    if branches_for_repository(repository)?
        .iter()
        .any(|branch| branch.kind == GitBranchKind::Local && branch.name == name)
    {
        Ok(())
    } else {
        Err(format!("本地 Git 分支不存在: {name}"))
    }
}

fn repository_has_head(repository: &Repository) -> Result<bool, String> {
    let output = run_git(
        repository,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from("HEAD^{commit}"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        true,
    )?;
    Ok(output.success())
}

fn resolve_commit(repository: &Repository, revision: &str) -> Result<String, String> {
    if revision.trim() != revision
        || revision.is_empty()
        || revision.contains('\0')
        || revision.starts_with('-')
        || revision.len() > 1024
    {
        return Err("Git revision 无效".into());
    }
    let output = run_git(
        repository,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--end-of-options"),
            OsString::from(format!("{revision}^{{commit}}")),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        true,
    )?;
    require_success("解析 Git revision", &output)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn count_patch_lines(patch: &str) -> (u64, u64) {
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    for line in patch.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            additions = additions.saturating_add(1);
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions = deletions.saturating_add(1);
        }
    }
    (additions, deletions)
}

fn output_looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
        || bytes
            .windows(b"GIT binary patch".len())
            .any(|window| window == b"GIT binary patch")
        || bytes
            .windows(b"Binary files ".len())
            .any(|window| window == b"Binary files ")
}

fn configured_remote_names(repository: &Repository) -> Result<Vec<String>, String> {
    let remotes = run_git(
        repository,
        [OsString::from("remote")],
        None,
        LOCAL_COMMAND_TIMEOUT,
        64 * 1024,
        true,
    )?;
    require_success("读取 Git remotes", &remotes)?;
    if remotes.stdout_truncated {
        return Err("Git remote 列表超过安全上限".into());
    }
    let text = std::str::from_utf8(&remotes.stdout)
        .map_err(|_| "Git remote 列表不是有效 UTF-8".to_owned())?;
    let mut names = text
        .lines()
        .map(|name| name.strip_suffix('\r').unwrap_or(name))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if names.len() > MAX_GIT_REMOTES {
        return Err(format!("Git remote 数量超过 {MAX_GIT_REMOTES} 个安全上限"));
    }
    for name in &names {
        validate_remote_name_syntax(name, false)?;
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn validate_remote_name_syntax(name: &str, allow_local: bool) -> Result<(), String> {
    if allow_local && name == "." {
        return Ok(());
    }
    if name.trim() != name
        || name.is_empty()
        || name == "."
        || name.starts_with('-')
        || name.len() > 1024
        || name
            .chars()
            .any(|character| character == '\0' || character.is_control())
    {
        return Err("Git remote 名称无效".into());
    }
    Ok(())
}

fn remote_config_values(
    repository: &Repository,
    key: &str,
    max_values: usize,
) -> Result<Vec<String>, String> {
    let output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--null"),
            OsString::from("--get-all"),
            OsString::from(key),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_GIT_REMOTE_CONFIG_OUTPUT,
        true,
    )?;
    if !output.success() {
        if !output.timed_out && output.exit_code() == Some(1) {
            return Ok(Vec::new());
        }
        return Err("无法读取 Git transport 配置".into());
    }
    if output.stdout_truncated {
        return Err("Git transport 配置超过安全上限".into());
    }
    let mut values = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let value =
            std::str::from_utf8(raw).map_err(|_| "Git transport 配置不是有效 UTF-8".to_owned())?;
        if value.as_bytes().len() > MAX_GIT_REMOTE_VALUE_BYTES
            || value
                .chars()
                .any(|character| character == '\0' || character == '\r' || character == '\n')
        {
            return Err("Git transport 配置值无效或超过安全上限".into());
        }
        values.push(value.to_owned());
        if values.len() > max_values {
            return Err("Git transport 配置项数量超过安全上限".into());
        }
    }
    Ok(values)
}

fn remote_effective_urls(
    repository: &Repository,
    name: &str,
    push: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec![
        OsString::from("remote"),
        OsString::from("get-url"),
        OsString::from("--all"),
    ];
    if push {
        args.push(OsString::from("--push"));
    }
    args.push(OsString::from(name));
    let output = run_git(
        repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_GIT_REMOTE_CONFIG_OUTPUT,
        true,
    )?;
    if !output.success() {
        return Err("无法读取 Git remote transport locator".into());
    }
    if output.stdout_truncated {
        return Err("Git remote transport locator 超过安全上限".into());
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Git remote transport locator 不是有效 UTF-8".to_owned())?;
    let mut values = Vec::new();
    for value in text.lines() {
        let value = value.strip_suffix('\r').unwrap_or(value);
        if value.is_empty()
            || value.as_bytes().len() > MAX_GIT_REMOTE_VALUE_BYTES
            || value
                .chars()
                .any(|character| character == '\0' || character.is_control())
        {
            return Err("Git remote transport locator 无效或超过安全上限".into());
        }
        values.push(value.to_owned());
        if values.len() > MAX_GIT_REMOTE_URLS {
            return Err(format!(
                "Git remote transport locator 超过 {MAX_GIT_REMOTE_URLS} 个安全上限"
            ));
        }
    }
    if values.is_empty() {
        return Err("Git remote 没有可用的 transport locator".into());
    }
    Ok(values)
}

fn remote_transport_revision(
    domain: &[u8],
    name: &str,
    urls: &[String],
    extra: &[String],
) -> String {
    // Remote locators can contain credentials. A plain digest would let the
    // renderer mount offline guesses against the serialized proof, so use
    // process-random keyed hashers. Proofs are intentionally opaque and only
    // stable for this process lifetime; four independently keyed SipHash
    // outputs retain the existing 256-bit token shape.
    static KEYS: OnceLock<[RandomState; 4]> = OnceLock::new();
    let keys = KEYS.get_or_init(|| std::array::from_fn(|_| RandomState::new()));
    let mut proof = String::with_capacity(64);
    for (index, key) in keys.iter().enumerate() {
        let mut hasher = key.build_hasher();
        hasher.write_usize(index);
        hash_opaque_component(&mut hasher, b"domain", domain);
        hash_opaque_component(&mut hasher, b"name", name.as_bytes());
        for url in urls {
            hash_opaque_component(&mut hasher, b"url", url.as_bytes());
        }
        for value in extra {
            hash_opaque_component(&mut hasher, b"config", value.as_bytes());
        }
        proof.push_str(&format!("{:016x}", hasher.finish()));
    }
    proof
}

fn hash_opaque_component(hasher: &mut impl Hasher, label: &[u8], value: &[u8]) {
    hasher.write_usize(label.len());
    hasher.write(label);
    hasher.write_usize(value.len());
    hasher.write(value);
}

fn local_remote_proof() -> GitRemote {
    GitRemote {
        name: ".".into(),
        fetch_revision: remote_transport_revision(b"mework.git.remote-fetch.v1\0", ".", &[], &[]),
        push_revision: remote_transport_revision(b"mework.git.remote-push.v1\0", ".", &[], &[]),
        url: None,
    }
}

fn remote_transport_for_existing(
    repository: &Repository,
    name: &str,
) -> Result<RemoteTransport, String> {
    let configured_urls = remote_config_values(
        repository,
        &format!("remote.{name}.url"),
        MAX_GIT_REMOTE_URLS,
    )?;
    if configured_urls.is_empty() {
        return Err("Git remote 没有有效的 url 配置".into());
    }
    let _configured_push_urls = remote_config_values(
        repository,
        &format!("remote.{name}.pushurl"),
        MAX_GIT_REMOTE_URLS,
    )?;
    let fetch_urls = remote_effective_urls(repository, name, false)?;
    let push_urls = remote_effective_urls(repository, name, true)?;
    let fetch_refspecs = remote_config_values(
        repository,
        &format!("remote.{name}.fetch"),
        MAX_GIT_REMOTE_REFSPECS,
    )?;
    let proof = GitRemote {
        name: name.to_owned(),
        fetch_revision: remote_transport_revision(
            b"mework.git.remote-fetch.v1\0",
            name,
            &fetch_urls,
            &fetch_refspecs,
        ),
        push_revision: remote_transport_revision(
            b"mework.git.remote-push.v1\0",
            name,
            &push_urls,
            &[],
        ),
        // Raw locators can contain credentials. They remain backend-only.
        url: None,
    };
    Ok(RemoteTransport { proof })
}

fn snapshot_remote_transports(repository: &Repository) -> (Vec<RemoteTransport>, Vec<String>) {
    let names = match configured_remote_names(repository) {
        Ok(names) => names,
        Err(error) => return (Vec::new(), vec![error]),
    };
    let mut transports = Vec::new();
    let mut warnings = Vec::new();
    for name in names {
        match remote_transport_for_existing(repository, &name) {
            Ok(transport) => transports.push(transport),
            Err(error) => warnings.push(format!(
                "Git remote {name} 的 transport proof 不可用：{error}"
            )),
        }
    }
    (transports, warnings)
}

fn read_upstream_atoms(
    repository: &Repository,
    branch: &str,
) -> Result<Option<UpstreamAtoms>, String> {
    validate_branch_name(repository, branch)?;
    let full_ref = format!("refs/heads/{branch}");
    let format = "%(refname)%00%(objectname)%00%(upstream)%00%(upstream:short)%00%(upstream:remotename)%00%(upstream:remoteref)";
    let output = run_git(
        repository,
        [
            OsString::from("for-each-ref"),
            OsString::from("--count=1"),
            OsString::from(format!("--format={format}")),
            OsString::from(&full_ref),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        64 * 1024,
        true,
    )?;
    require_success("读取 Git upstream atoms", &output)?;
    if output.stdout_truncated {
        return Err("Git upstream atoms 超过安全上限".into());
    }
    parse_upstream_atoms(&output.stdout, &full_ref)
}

/// One `for-each-ref` record of [`read_upstream_atoms`]' format for
/// `full_ref`, or `None` when that branch has no upstream.
fn parse_upstream_atoms(bytes: &[u8], full_ref: &str) -> Result<Option<UpstreamAtoms>, String> {
    let mut bytes = bytes;
    while bytes
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.is_empty() {
        return Err("当前本地 Git 分支已在读取 upstream 时消失".into());
    }
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.len() != 6 {
        return Err("Git upstream atoms 字段数量无效".into());
    }
    let fields = fields
        .iter()
        .map(|field| {
            std::str::from_utf8(field)
                .map(str::to_owned)
                .map_err(|_| "Git upstream atoms 不是有效 UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fields[0] != full_ref {
        return Err("Git upstream atoms 返回了错误的本地分支".into());
    }
    let local_oid = validate_object_id("本地分支提交", fields[1].clone())?;
    if fields[2].is_empty() {
        if fields[3..].iter().any(|field| !field.is_empty()) {
            return Err("Git upstream atoms 不完整".into());
        }
        return Ok(None);
    }
    if fields[3].is_empty() || fields[4].is_empty() || fields[5].is_empty() {
        return Err("Git upstream atoms 不完整".into());
    }
    validate_remote_name_syntax(&fields[4], true)?;
    if !fields[5].starts_with("refs/heads/") {
        return Err("Git upstream merge ref 不是远端分支".into());
    }
    Ok(Some(UpstreamAtoms {
        local_ref: fields[0].clone(),
        local_oid,
        tracking_ref: fields[2].clone(),
        tracking_short: fields[3].clone(),
        remote_name: fields[4].clone(),
        merge_ref: fields[5].clone(),
    }))
}

fn upstream_target_for_branch(
    repository: &Repository,
    branch: &str,
    transports: &[RemoteTransport],
) -> Result<(Option<String>, Option<GitUpstream>, String), String> {
    let first = read_upstream_atoms(repository, branch)?;
    let local_oid = first
        .as_ref()
        .map(|atoms| atoms.local_oid.clone())
        .unwrap_or_else(|| {
            resolve_commit(repository, &format!("refs/heads/{branch}"))
                .unwrap_or_else(|_| String::new())
        });
    if first.is_none() {
        if local_oid.is_empty() {
            return Err("无法解析当前本地 Git 分支".into());
        }
        return Ok((None, None, local_oid));
    }
    let first = first.expect("checked above");
    let first_tracking_oid = resolve_commit(repository, &first.tracking_ref).ok();
    let second = read_upstream_atoms(repository, branch)?
        .ok_or_else(|| "Git upstream 在读取时被移除；请重试".to_owned())?;
    let second_tracking_oid = resolve_commit(repository, &second.tracking_ref).ok();
    if first != second || first_tracking_oid != second_tracking_oid {
        return Err("Git upstream 在读取时发生变化；请重试".into());
    }
    let is_local = second.remote_name == ".";
    let remote = if is_local {
        local_remote_proof()
    } else {
        transports
            .iter()
            .find(|transport| transport.proof.name == second.remote_name)
            .map(|transport| transport.proof.clone())
            .ok_or_else(|| "Git upstream 指向不存在的 remote".to_owned())?
    };
    let remote_branch = second
        .merge_ref
        .strip_prefix("refs/heads/")
        .ok_or_else(|| "Git upstream merge ref 无效".to_owned())?
        .to_owned();
    let target = GitUpstream {
        remote_name: second.remote_name.clone(),
        remote_branch,
        merge_ref: second.merge_ref,
        tracking_ref: second.tracking_ref,
        tracking_oid: second_tracking_oid,
        is_local,
        remote,
    };
    Ok((Some(second.tracking_short), Some(target), second.local_oid))
}

fn preferred_git_remote(
    remotes: &[GitRemote],
    upstream: Option<&GitUpstream>,
) -> Option<GitRemote> {
    if let Some(upstream) = upstream {
        return Some(upstream.remote.clone());
    }
    for preferred in ["origin", "upstream"] {
        if let Some(remote) = remotes.iter().find(|remote| remote.name == preferred) {
            return Some(remote.clone());
        }
    }
    remotes.first().cloned()
}

fn validate_object_id(label: &str, value: String) -> Result<String, String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} 无效"));
    }
    Ok(value.to_ascii_lowercase())
}

fn run_git(
    repository: &Repository,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    passive: bool,
) -> Result<CliOutput, String> {
    let mut prepared = git_command_prefix();
    prepared.extend(args);
    run_program(
        &repository.git,
        &repository.root,
        prepared,
        input,
        timeout,
        max_output,
        if passive {
            CliKind::GitPassive
        } else {
            CliKind::GitMutation
        },
    )
}

fn git_command_prefix() -> Vec<OsString> {
    vec![
        OsString::from("-c"),
        OsString::from("core.fsmonitor=false"),
        OsString::from("-c"),
        OsString::from("gc.auto=0"),
        OsString::from("-c"),
        OsString::from("maintenance.auto=false"),
        OsString::from("-c"),
        OsString::from("submodule.recurse=false"),
        OsString::from("-c"),
        OsString::from("fetch.recurseSubmodules=false"),
        OsString::from("-c"),
        OsString::from("push.recurseSubmodules=no"),
        OsString::from("-c"),
        OsString::from("color.ui=false"),
        OsString::from("-c"),
        OsString::from("core.quotepath=false"),
    ]
}

#[derive(Clone, Copy)]
enum CliKind {
    GitPassive,
    GitMutation,
}

const GIT_ENVIRONMENT_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_SHALLOW_FILE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_ATTR_NOSYSTEM",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_OPTIONAL_LOCKS",
];

fn is_git_environment_override(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    GIT_ENVIRONMENT_OVERRIDES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || name
            .get(.."GIT_CONFIG_KEY_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_CONFIG_KEY_"))
        || name
            .get(.."GIT_CONFIG_VALUE_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_CONFIG_VALUE_"))
}

fn configure_cli_environment(command: &mut Command, kind: CliKind) {
    for name in GIT_ENVIRONMENT_OVERRIDES {
        command.env_remove(name);
    }
    let explicit_overrides = command
        .get_envs()
        .filter_map(|(name, _)| is_git_environment_override(name).then(|| name.to_os_string()))
        .collect::<Vec<_>>();
    for name in explicit_overrides {
        command.env_remove(name);
    }
    for (name, _) in env::vars_os() {
        if is_git_environment_override(&name) {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE_PACKET")
        .env_remove("GIT_TRACE_CURL")
        .env_remove("GIT_TRACE2")
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_CURL_VERBOSE")
        .env_remove("GH_DEBUG")
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH_VARIANT")
        .env_remove("GIT_PROXY_COMMAND")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env_remove("SSH_ASKPASS_REQUIRE")
        .env_remove("GCM_ASKPASS");
    if matches!(kind, CliKind::GitPassive) {
        command.env("GIT_OPTIONAL_LOCKS", "0");
    }
    if matches!(kind, CliKind::GitMutation) {
        command
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_EDITOR", "true");
    }
}

fn run_program(
    program: &Path,
    cwd: &Path,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    kind: CliKind,
) -> Result<CliOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_cli_environment(&mut command, kind);
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let containment = ProcessContainment::create()?;
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 {}: {error}", program.display()))?;
    if let Err(error) = containment.assign(&child) {
        terminate_uncontained(&mut child);
        return Err(error);
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法捕获命令标准输出".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法捕获命令错误输出".to_owned())?;
    let stdout_reader = capture_pipe(stdout, max_output);
    let stderr_reader = capture_pipe(stderr, max_output);
    let input_writer = input.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || -> Result<(), String> {
            stdin
                .write_all(&input)
                .map_err(|error| format!("无法写入命令标准输入: {error}"))?;
            stdin
                .flush()
                .map_err(|error| format!("无法刷新命令标准输入: {error}"))
        })
    });

    let waited = child
        .wait_timeout(timeout)
        .map_err(|error| format!("等待命令退出失败: {error}"))?;
    let timed_out = waited.is_none();
    let status = if let Some(status) = waited {
        Some(status)
    } else {
        containment.terminate_tree(&mut child);
        child.wait_timeout(PROCESS_TERMINATION_GRACE).ok().flatten()
    };
    if let Some(writer) = input_writer {
        writer
            .join()
            .map_err(|_| "命令标准输入线程异常终止".to_owned())??;
    }
    let stdout_capture = stdout_reader
        .join()
        .map_err(|_| "命令标准输出线程异常终止".to_owned())?
        .map_err(|error| format!("读取命令标准输出失败: {error}"))?;
    let stderr_capture = stderr_reader
        .join()
        .map_err(|_| "命令错误输出线程异常终止".to_owned())?
        .map_err(|error| format!("读取命令错误输出失败: {error}"))?;
    Ok(CliOutput {
        status,
        stdout: stdout_capture.output,
        stderr: stderr_capture.output,
        stdout_sha256: stdout_capture.sha256,
        timed_out,
        stdout_truncated: stdout_capture.truncated,
        stderr_truncated: stderr_capture.truncated,
    })
}

#[cfg(windows)]
fn git_cli_environment_path(path: &Path) -> OsString {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        OsString::from(format!(r"\\{unc}"))
    } else if let Some(local) = value.strip_prefix(r"\\?\") {
        OsString::from(local)
    } else {
        path.as_os_str().to_os_string()
    }
}

#[cfg(not(windows))]
fn git_cli_environment_path(path: &Path) -> OsString {
    path.as_os_str().to_os_string()
}

struct CapturedPipe {
    output: Vec<u8>,
    truncated: bool,
    sha256: [u8; 32],
}

fn capture_pipe(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<std::io::Result<CapturedPipe>> {
    thread::spawn(move || -> std::io::Result<CapturedPipe> {
        let mut output = Vec::new();
        let mut truncated = false;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = pipe.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            let remaining = limit.saturating_sub(output.len());
            output.extend_from_slice(&buffer[..read.min(remaining)]);
            truncated |= read > remaining;
        }
        Ok(CapturedPipe {
            output,
            truncated,
            sha256: digest.finalize().into(),
        })
    })
}

fn require_success(label: &str, output: &CliOutput) -> Result<(), String> {
    if output.success() {
        Ok(())
    } else {
        Err(command_error(label, output))
    }
}

fn command_error(label: &str, output: &CliOutput) -> String {
    let detail = output.display_output();
    if detail.is_empty() {
        match output.exit_code() {
            Some(code) => format!("{label}失败（退出码 {code}）"),
            None => format!("{label}失败"),
        }
    } else {
        format!("{label}失败：{detail}")
    }
}

fn find_program(name: &str) -> Option<PathBuf> {
    let requested = Path::new(name);
    if requested.components().count() > 1 {
        return requested
            .is_absolute()
            .then(|| canonical_file(requested))
            .flatten()
            .filter(|path| !is_uninstalled_developer_tool_shim(path));
    }
    let mut candidates = Vec::new();
    #[cfg(windows)]
    {
        let executable = format!("{name}.exe");
        if name.eq_ignore_ascii_case("git") {
            if let Some(program_files) = env::var_os("ProgramFiles") {
                candidates.push(PathBuf::from(program_files).join("Git/cmd/git.exe"));
            }
            if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
                candidates.push(PathBuf::from(local_app_data).join("Programs/Git/cmd/git.exe"));
            }
        }
        if let Some(path) = env::var_os("PATH") {
            candidates.extend(
                env::split_paths(&path)
                    .filter(|directory| directory.is_absolute())
                    .map(|directory| directory.join(&executable)),
            );
        }
    }
    #[cfg(not(windows))]
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(
            env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .map(|directory| directory.join(name)),
        );
    }
    // On a Mac without the command line tools, `/usr/bin/git` is a stand-in
    // that opens the install dialog on every run. It is passed over rather than
    // ending the search, so a real git later on PATH (Homebrew's, say) is still
    // found, and with none the caller reports Git as not installed.
    candidates
        .into_iter()
        .filter_map(|path| canonical_file(&path))
        .find(|path| !is_uninstalled_developer_tool_shim(path))
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    path.is_file()
        .then(|| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn repository_lock(repository: &Repository) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks
        .get(&repository.git_common_dir)
        .and_then(Weak::upgrade)
    {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(repository.git_common_dir.clone(), Arc::downgrade(&lock));
    lock
}

fn parse_rev_parse_paths(output: &[u8], expected_count: usize) -> Result<Vec<String>, String> {
    let output = std::str::from_utf8(output)
        .map_err(|_| "Git 返回的仓库路径不是有效 UTF-8，无法安全使用".to_owned())?;
    let output = output.strip_suffix('\n').unwrap_or(output);
    if output.is_empty() || output.ends_with('\n') {
        return Err("Git 返回的仓库路径数量不正确".into());
    }
    let paths = output
        .split('\n')
        .map(|path| path.strip_suffix('\r').unwrap_or(path))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if paths.len() != expected_count
        || paths
            .iter()
            .any(|path| path.is_empty() || path.contains('\0'))
    {
        return Err("Git 返回的仓库路径数量或格式不正确".into());
    }
    Ok(paths)
}

fn canonical_git_directory(
    label: &str,
    path: &Path,
    relative_to: &Path,
) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        relative_to.join(path)
    };
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("无法验证 Git {label} {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("Git {label} 不是目录: {}", canonical.display()));
    }
    Ok(canonical)
}

fn path_is_within(path: &Path, ancestor: &Path) -> bool {
    if same_path(path, ancestor) {
        return true;
    }
    let path = path_identity(path);
    let ancestor = path_identity(ancestor);
    let separator = std::path::MAIN_SEPARATOR;
    let ancestor = ancestor.trim_end_matches(separator);
    path.strip_prefix(ancestor)
        .is_some_and(|suffix| suffix.starts_with(separator))
}

fn git_path_id(domain: &[u8], path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("无法读取 Git 身份目录元数据 {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(b"\0");
    update_revision_component(
        &mut digest,
        b"canonical-path",
        path_identity(path).as_bytes(),
    );
    hash_git_path_metadata_identity(&mut digest, path, &metadata);
    Ok(format!("{:x}", digest.finalize()))
}

fn git_worktree_id(repository_id: &str, root: &Path, git_dir: &Path) -> Result<String, String> {
    let root_metadata = fs::metadata(root).map_err(|error| {
        format!(
            "无法读取 Git worktree 根目录元数据 {}: {error}",
            root.display()
        )
    })?;
    let git_dir_metadata = fs::metadata(git_dir).map_err(|error| {
        format!(
            "无法读取 Git worktree 元数据目录元数据 {}: {error}",
            git_dir.display()
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(b"mework.git.worktree-id.v1\0");
    update_revision_component(&mut digest, b"repository-id", repository_id.as_bytes());
    update_revision_component(
        &mut digest,
        b"canonical-root",
        path_identity(root).as_bytes(),
    );
    update_revision_component(
        &mut digest,
        b"canonical-git-dir",
        path_identity(git_dir).as_bytes(),
    );
    hash_git_path_metadata_identity(&mut digest, root, &root_metadata);
    hash_git_path_metadata_identity(&mut digest, git_dir, &git_dir_metadata);
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn hash_git_path_metadata_identity(digest: &mut Sha256, _path: &Path, metadata: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;

    let mut identity = Vec::with_capacity(16);
    identity.extend_from_slice(&metadata.dev().to_be_bytes());
    identity.extend_from_slice(&metadata.ino().to_be_bytes());
    update_revision_component(digest, b"filesystem-identity", &identity);
}

#[cfg(windows)]
fn hash_git_path_metadata_identity(digest: &mut Sha256, path: &Path, metadata: &fs::Metadata) {
    use std::{
        mem::MaybeUninit,
        os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
    };

    let mut identity = Vec::with_capacity(32);
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    if let Ok(file) = options.open(path) {
        let mut info = MaybeUninit::<FILE_ID_INFO>::zeroed();
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                info.as_mut_ptr().cast(),
                u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).unwrap_or(u32::MAX),
            )
        };
        if success != 0 {
            let info = unsafe { info.assume_init() };
            identity.extend_from_slice(&info.VolumeSerialNumber.to_be_bytes());
            identity.extend_from_slice(&info.FileId.Identifier);
        }
    }
    identity.extend_from_slice(&metadata.creation_time().to_be_bytes());
    update_revision_component(digest, b"filesystem-identity", &identity);
}

#[cfg(not(any(unix, windows)))]
fn hash_git_path_metadata_identity(digest: &mut Sha256, _path: &Path, metadata: &fs::Metadata) {
    let creation = metadata.created().ok();
    hash_operation_optional_timestamp(digest, b"filesystem-created", creation);
}

fn path_identity(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        path_identity(left) == path_identity(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn splitn_ascii(value: &[u8], delimiter: u8, count: usize) -> Vec<&[u8]> {
    value.splitn(count, |byte| *byte == delimiter).collect()
}

fn strip_ascii_prefix<'a>(value: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    value.strip_prefix(prefix)
}

fn lossy(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn redact_sensitive_text(value: &str) -> String {
    static URL_USERINFO: OnceLock<Regex> = OnceLock::new();
    static SCP_USERINFO: OnceLock<Regex> = OnceLock::new();
    static GITHUB_TOKEN: OnceLock<Regex> = OnceLock::new();
    let value = URL_USERINFO
        .get_or_init(|| {
            Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)[^\s/@]+@")
                .expect("valid URL userinfo redaction regex")
        })
        .replace_all(value, "${1}[REDACTED]@");
    let value = SCP_USERINFO
        .get_or_init(|| {
            Regex::new(r#"(?m)(^|[\s'"(])([^\s/@:]+)@([A-Za-z0-9.-]+):"#)
                .expect("valid scp userinfo redaction regex")
        })
        .replace_all(&value, "${1}[REDACTED]@${3}:");
    GITHUB_TOKEN
        .get_or_init(|| {
            Regex::new(r"(?i)(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})")
                .expect("valid GitHub token redaction regex")
        })
        .replace_all(&value, "[REDACTED]")
        .into_owned()
}

#[cfg(windows)]
struct ProcessContainment {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessContainment {
    fn create() -> Result<Self, String> {
        use std::{ffi::c_void, mem::size_of, ptr};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "无法创建命令进程作业对象: {}",
                std::io::Error::last_os_error()
            ));
        }
        let containment = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                containment.handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(format!(
                "无法配置命令进程作业对象: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(containment)
    }

    fn assign(&self, child: &Child) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        let assigned = unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as _) };
        if assigned == 0 {
            return Err(format!(
                "无法将命令进程加入受控作业对象: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn terminate_tree(&self, child: &mut Child) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessContainment {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(unix)]
struct ProcessContainment;

#[cfg(unix)]
impl ProcessContainment {
    fn create() -> Result<Self, String> {
        Ok(Self)
    }

    fn assign(&self, _child: &Child) -> Result<(), String> {
        Ok(())
    }

    fn terminate_tree(&self, child: &mut Child) {
        let process_group = -(child.id() as i32);
        if unsafe { libc::kill(process_group, libc::SIGKILL) } != 0 {
            let _ = child.kill();
        }
    }
}

fn terminate_uncontained(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait_timeout(PROCESS_TERMINATION_GRACE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        find_program("git").is_some()
    }

    fn run_test_git(root: &Path, args: &[&str]) {
        let git = find_program("git").expect("Git is available");
        let output = Command::new(git)
            .current_dir(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_test_git_output(root: &Path, args: &[&str]) -> Vec<u8> {
        let git = find_program("git").expect("Git is available");
        let output = Command::new(git)
            .current_dir(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn run_test_git_with_index(root: &Path, index: &Path, args: &[&str]) {
        let git = find_program("git").expect("Git is available");
        let output = Command::new(git)
            .current_dir(root)
            .args(args)
            .env("GIT_INDEX_FILE", git_cli_environment_path(index))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git with alternate index {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn install_test_git_hook(root: &Path, name: &str, body: &str) {
        let git_dir = String::from_utf8(run_test_git_output(
            root,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        ))
        .unwrap();
        let hook = PathBuf::from(git_dir.trim()).join("hooks").join(name);
        fs::write(&hook, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn initialized_repository() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        run_test_git(root.path(), &["init"]);
        run_test_git(root.path(), &["config", "user.name", "Mework Test"]);
        run_test_git(
            root.path(),
            &["config", "user.email", "mework@example.invalid"],
        );
        fs::write(root.path().join("tracked.txt"), "first\n").unwrap();
        run_test_git(root.path(), &["add", "tracked.txt"]);
        run_test_git(root.path(), &["commit", "-m", "initial"]);
        root
    }

    fn test_commit_oid(root: &Path, revision: &str) -> String {
        let repository = require_repository(root).unwrap();
        resolve_commit(&repository, revision).unwrap()
    }

    fn repository_with_merge_conflict() -> (tempfile::TempDir, String) {
        let repository = initialized_repository();
        let base = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        run_test_git(repository.path(), &["switch", "-c", "feature/conflict"]);
        fs::write(repository.path().join("tracked.txt"), "feature\n").unwrap();
        run_test_git(repository.path(), &["add", "tracked.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "feature"]);
        run_test_git(repository.path(), &["switch", &base]);
        fs::write(repository.path().join("tracked.txt"), "main\n").unwrap();
        run_test_git(repository.path(), &["add", "tracked.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "main"]);
        (repository, base)
    }

    /// Merges `feature/conflict` into the checked-out branch of a
    /// `repository_with_merge_conflict`, which stops on the conflict.
    fn start_conflicted_merge(root: &Path) {
        let git = find_program("git").expect("Git is available");
        let output = Command::new(git)
            .current_dir(root)
            .args(["merge", "--no-edit", "feature/conflict"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "the merge must stop on its conflict"
        );
    }

    #[test]
    fn parses_porcelain_branch_changes_and_conflicts() {
        let bytes = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0# future.header value\0# stash 4\0\
1 .M N... 100644 100644 100644 a a changed.txt\0\
2 R. N... 100644 100644 100644 a b R100 new.txt\0old.txt\0\
u UU N... 100644 100644 100644 100644 a b c d conflict.txt\0\
? untracked.txt\0";
        let parsed = parse_porcelain_v2(bytes).unwrap();
        assert_eq!(parsed.branch.head.as_deref(), Some("main"));
        assert_eq!(parsed.branch.ahead, 2);
        assert_eq!(parsed.branch.behind, 3);
        assert_eq!(parsed.stash_count, 4);
        assert_eq!(parsed.changes.len(), 4);
        assert_eq!(parsed.changes[1].status, GitFileStatus::Renamed);
        assert_eq!(parsed.changes[1].original_path.as_deref(), Some("old.txt"));
        assert!(parsed.changes[2].conflicted);
        assert_eq!(parsed.changes[3].status, GitFileStatus::Untracked);
    }

    /// Isolated worktrees must be invisible to the parent repository.
    ///
    /// The container's self-ignoring `*` entry must hide the entire directory
    /// from `git status`.
    #[test]
    fn an_isolated_worktree_stays_invisible_to_the_parent_repository() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let worktree =
            create_isolated_worktree(repository.path(), "run1", "ws1").expect("worktree created");

        assert!(worktree.path.is_dir());
        assert!(worktree.path.join("tracked.txt").is_file());
        assert_eq!(worktree.branch, "mework/wf/run1/ws1");
        assert_eq!(
            fs::read_to_string(
                repository
                    .path()
                    .join(".mework")
                    .join("worktrees")
                    .join(".gitignore")
            )
            .unwrap(),
            "*\n"
        );
        let status = run_test_git_output(repository.path(), &["status", "--porcelain"]);
        assert!(
            status.is_empty(),
            "父仓库状态被工作树污染：{}",
            String::from_utf8_lossy(&status)
        );
    }

    /// Cleanup is conservative: remove only unchanged worktrees and preserve
    /// their branches whenever uncommitted or post-baseline committed work exists.
    ///
    /// A clean status alone cannot distinguish unused from post-baseline
    /// committed work, which must also be retained.
    #[test]
    fn worktree_cleanup_removes_only_the_ones_that_did_nothing() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();

        let untouched =
            create_isolated_worktree(repository.path(), "run1", "ws1").expect("worktree created");
        assert!(release_isolated_worktree(repository.path(), &untouched).unwrap());
        assert!(!untouched.path.exists());
        // `git worktree remove` deletes only the leaf; remove the empty
        // `<runId>/` parent to prevent invisible empty-directory accumulation.
        assert!(!untouched.path.parent().unwrap().exists());
        let branches = String::from_utf8(run_test_git_output(
            repository.path(),
            &["branch", "--list", "mework/wf/run1/ws1"],
        ))
        .unwrap();
        assert!(
            branches.trim().is_empty(),
            "空工作树的分支也该删掉：{branches}"
        );

        let dirty =
            create_isolated_worktree(repository.path(), "run1", "ws2").expect("worktree created");
        fs::write(dirty.path.join("tracked.txt"), "changed\n").unwrap();
        assert!(!release_isolated_worktree(repository.path(), &dirty).unwrap());
        assert!(dirty.path.is_dir(), "有改动的工作树必须保留");

        let committed =
            create_isolated_worktree(repository.path(), "run1", "ws3").expect("worktree created");
        fs::write(committed.path.join("tracked.txt"), "committed\n").unwrap();
        run_test_git(&committed.path, &["add", "tracked.txt"]);
        run_test_git(&committed.path, &["commit", "-m", "step work"]);
        // Only commits relative to baseline distinguish this clean worktree from
        // one with no work.
        assert!(!release_isolated_worktree(repository.path(), &committed).unwrap());
        assert!(committed.path.is_dir(), "已提交的工作树必须保留");
        let branches = String::from_utf8(run_test_git_output(
            repository.path(),
            &["branch", "--list", "mework/wf/run1/ws3"],
        ))
        .unwrap();
        assert!(!branches.trim().is_empty(), "保留的工作树要连分支一起留住");
    }

    /// Isolation must fail for a workspace that is not a repository root rather
    /// than escalating to an ancestor repository.
    #[test]
    fn isolation_refuses_a_workspace_that_is_not_its_own_repository_root() {
        if !git_available() {
            return;
        }
        let plain = tempfile::tempdir().unwrap();
        assert!(create_isolated_worktree(plain.path(), "run1", "ws1").is_err());

        let repository = initialized_repository();
        let nested = repository.path().join("sub");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            create_isolated_worktree(&nested, "run1", "ws1").is_err(),
            "子目录不得借上级仓库开出工作树"
        );
    }

    /// Run IDs and slots enter paths and ref names, so host-generated values
    /// still require allowlist validation.
    #[test]
    fn worktree_path_components_reject_traversal_and_option_lookalikes() {
        for bad in ["..", "a/b", "a\\b", "-force", ".hidden", "", "a b"] {
            assert!(
                validate_worktree_component("测试", bad).is_err(),
                "{bad} 应当被拒绝"
            );
        }
        assert!(validate_worktree_component("测试", "run0a1b2c3d").is_ok());
        assert!(validate_worktree_component("测试", "ws12-as-reviewer").is_ok());
    }

    #[test]
    fn bounded_summary_and_change_pages_are_revision_and_query_bound() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        // Committed first and then edited: `change_page` serves the review panel,
        // which lists tracked changes only, so untracked fixtures would page empty.
        const PAGED: [&str; 3] = ["z-last.txt", "a-first.txt", "middle.txt"];
        for path in PAGED {
            fs::write(repository.path().join(path), format!("{path}\n")).unwrap();
        }
        run_test_git(repository.path(), &["add", "z-last.txt", "a-first.txt", "middle.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "seed change pages"]);
        for path in PAGED {
            fs::write(repository.path().join(path), format!("{path} edited\n")).unwrap();
        }

        let summary = match workspace_summary(repository.path(), None).unwrap() {
            GitWorkspaceSummaryResult::Snapshot { summary } => summary,
            other => panic!("expected bounded summary, got {other:?}"),
        };
        assert_eq!(summary.changed_files, 3);
        assert_eq!(summary.stageable, 3);
        assert_eq!(summary.summary_revision.len(), 64);
        assert!(matches!(
            workspace_summary(
                repository.path(),
                Some(summary.summary_revision.clone())
            )
            .unwrap(),
            GitWorkspaceSummaryResult::Unchanged { revision }
                if revision == summary.summary_revision
        ));

        let first = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: summary.summary_revision.clone(),
                cursor: None,
                query: None,
                limit: 2,
                selected_path: Some("z-last.txt".into()),
            },
        )
        .unwrap();
        let (cursor, first_paths) = match first {
            GitChangePageResult::Page {
                files,
                matched_count,
                next_cursor,
                selection: Some(GitChangeSelection::Present { file }),
                ..
            } => {
                assert_eq!(matched_count, 3);
                assert_eq!(file.path, "z-last.txt");
                (
                    next_cursor.expect("second page"),
                    files.into_iter().map(|file| file.path).collect::<Vec<_>>(),
                )
            }
            other => panic!("expected first page, got {other:?}"),
        };
        assert_eq!(first_paths, ["a-first.txt", "middle.txt"]);

        let second = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: summary.summary_revision.clone(),
                cursor: Some(cursor.clone()),
                query: None,
                limit: 2,
                selected_path: None,
            },
        )
        .unwrap();
        assert!(matches!(
            second,
            GitChangePageResult::Page {
                files,
                next_cursor: None,
                ..
            } if files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>()
                == ["z-last.txt"]
        ));

        let mismatched_query = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: summary.summary_revision.clone(),
                cursor: Some(cursor),
                query: Some("first".into()),
                limit: 2,
                selected_path: None,
            },
        )
        .unwrap_err();
        assert!(mismatched_query.contains("筛选条件不匹配"));

        let filtered_selection = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: summary.summary_revision.clone(),
                cursor: None,
                query: Some("first".into()),
                limit: 2,
                selected_path: Some("z-last.txt".into()),
            },
        )
        .unwrap();
        assert!(matches!(
            filtered_selection,
            GitChangePageResult::Page {
                matched_count: 1,
                selection: Some(GitChangeSelection::FilteredOut),
                ..
            }
        ));
        assert!(matches!(
            change_page(
                repository.path(),
                GitChangePageRequest {
                    expected_revision: summary.summary_revision.clone(),
                    cursor: None,
                    query: None,
                    limit: 2,
                    selected_path: Some("missing.txt".into()),
                }
            )
            .unwrap(),
            GitChangePageResult::Page {
                selection: Some(GitChangeSelection::Missing),
                ..
            }
        ));

        fs::write(repository.path().join("later.txt"), "later\n").unwrap();
        assert!(matches!(
            change_page(
                repository.path(),
                GitChangePageRequest {
                    expected_revision: summary.summary_revision,
                    cursor: None,
                    query: None,
                    limit: 2,
                    selected_path: Some("missing.txt".into()),
                }
            )
            .unwrap(),
            GitChangePageResult::Stale { .. }
        ));
    }

    #[test]
    fn change_pages_leave_untracked_files_out_of_the_review_list() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "second\n").unwrap();
        fs::write(repository.path().join("scratch.txt"), "scratch\n").unwrap();

        let summary = match workspace_summary(repository.path(), None).unwrap() {
            GitWorkspaceSummaryResult::Snapshot { summary } => summary,
            other => panic!("expected bounded summary, got {other:?}"),
        };
        // The snapshot still counts the untracked file — `git_status`, discard and
        // stash all need it. Only the review page drops it, so the renderer can
        // recover its own total as `changed_files - untracked`.
        assert_eq!(summary.changed_files, 2);
        assert_eq!(summary.untracked, 1);

        let page = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: summary.summary_revision,
                cursor: None,
                query: None,
                limit: 10,
                selected_path: Some("scratch.txt".into()),
            },
        )
        .unwrap();
        assert!(
            matches!(
                &page,
                GitChangePageResult::Page {
                    files,
                    matched_count: 1,
                    next_cursor: None,
                    selection: Some(GitChangeSelection::Missing),
                    ..
                } if files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>()
                    == ["tracked.txt"]
            ),
            "expected only the tracked change, got {page:?}"
        );
    }

    #[test]
    fn canonical_content_revision_does_not_depend_on_porcelain_record_order() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("a.txt"), "a\n").unwrap();
        fs::write(repository.path().join("z.txt"), "z\n").unwrap();
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let mut reversed = snapshot.files.clone();
        reversed.reverse();
        let repository = require_repository(repository.path()).unwrap();
        assert_eq!(
            repository_content_revision(&repository, &snapshot.files).unwrap(),
            repository_content_revision(&repository, &reversed).unwrap()
        );
    }

    #[test]
    fn validates_relative_paths_and_literal_pathspec_payloads() {
        assert_eq!(
            validate_relative_path("./src/main.rs").unwrap(),
            "src/main.rs"
        );
        assert!(validate_relative_path("../secret").is_err());
        // A drive prefix only means "absolute" on Windows; elsewhere `C:` is an
        // ordinary directory name a repository may contain.
        assert_eq!(
            validate_relative_path("C:/secret").is_err(),
            crate::host_platform::host_platform().is_windows()
        );
        assert!(validate_relative_path("bad\\path").is_err());
        let encoded = encode_pathspecs(&["-leading.txt".into(), "line\nbreak.txt".into()]).unwrap();
        assert_eq!(encoded, b"-leading.txt\0line\nbreak.txt\0");
    }

    #[test]
    fn parses_bounded_git_bisect_terms_without_localized_prose() {
        assert_eq!(parse_bisect_term_output(b"works\n").unwrap(), "works");
        assert_eq!(
            parse_bisect_term_output(b"old-state\r\n").unwrap(),
            "old-state"
        );
        assert!(parse_bisect_term_output(b"").is_err());
        assert!(parse_bisect_term_output(b"old\nnew\n").is_err());
        assert!(parse_bisect_term_output(&vec![b'a'; MAX_GIT_BISECT_TERM_BYTES + 1]).is_err());
    }

    #[test]
    fn deserializes_frontend_action_tags_and_camel_case_fields() {
        assert!(
            validate_object_id("head", "0123456789ABCDEF0123456789ABCDEF01234567".into()).is_ok()
        );
        assert!(validate_object_id("head", "--match-head-commit".into()).is_err());
        let discard: GitAction = serde_json::from_value(serde_json::json!({
            "type": "discard",
            "paths": ["tracked.txt"],
            "includeUntracked": false,
            "expectedContentRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedTargetRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert!(matches!(
            discard,
            GitAction::Discard {
                expected_content_revision,
                expected_target_revision,
                ..
            } if expected_content_revision.starts_with("01234567")
                && expected_target_revision.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "discard",
            "paths": ["tracked.txt"],
            "expectedContentRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }))
        .is_err());
        let continue_operation: GitAction = serde_json::from_value(serde_json::json!({
            "type": "continue_operation",
            "operation": "merge",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedOperationRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert!(matches!(
            continue_operation,
            GitAction::ContinueOperation {
                operation: GitRepositoryOperation::Merge,
                expected_operation_revision,
                ..
            } if expected_operation_revision.starts_with("01234567")
        ));
        let bisect_step: GitAction = serde_json::from_value(serde_json::json!({
            "type": "bisect_step",
            "outcome": "old",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedOperationRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedContentRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert!(matches!(
            bisect_step,
            GitAction::BisectStep {
                outcome: GitBisectOutcome::Old,
                expected_operation_revision,
                expected_content_revision,
                ..
            } if expected_operation_revision.starts_with("01234567")
                && expected_content_revision.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "bisect_step",
            "outcome": "skip",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedOperationRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }))
        .is_err());
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "abort_operation",
            "operation": "merge",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567"
        }))
        .is_err());
    }

    #[test]
    fn only_exact_repository_roots_are_discovered() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let nested = repository.path().join("nested");
        fs::create_dir(&nested).unwrap();
        assert!(discover_repository(repository.path()).unwrap().is_some());
        assert!(discover_repository(&nested).unwrap().is_none());
        let plain = tempfile::tempdir().unwrap();
        assert!(discover_repository(plain.path()).unwrap().is_none());
        fs::create_dir(plain.path().join(".git")).unwrap();
        assert!(discover_repository(plain.path()).unwrap().is_none());
    }

    #[test]
    fn linked_worktree_root_is_discovered() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let parent = tempfile::tempdir().unwrap();
        let linked = parent.path().join("linked");
        run_test_git(
            repository.path(),
            &[
                "worktree",
                "add",
                "-b",
                "linked-test",
                linked.to_str().unwrap(),
            ],
        );
        assert!(linked.join(".git").is_file());
        let primary = discover_repository(repository.path()).unwrap().unwrap();
        let discovered = discover_repository(&linked).unwrap().unwrap();
        assert!(same_path(
            &discovered.root,
            &fs::canonicalize(&linked).unwrap()
        ));
        assert!(same_path(
            &primary.git_common_dir,
            &discovered.git_common_dir
        ));
        assert_eq!(primary.repository_id, discovered.repository_id);
        assert_ne!(primary.worktree_id, discovered.worktree_id);
        assert_eq!(
            primary.worktree_id,
            git_worktree_id(&primary.repository_id, &primary.root, &primary.git_dir).unwrap()
        );
        assert_eq!(
            discovered.worktree_id,
            git_worktree_id(
                &discovered.repository_id,
                &discovered.root,
                &discovered.git_dir
            )
            .unwrap()
        );
        assert!(Arc::ptr_eq(
            &repository_lock(&primary),
            &repository_lock(&discovered)
        ));
        let primary_snapshot = snapshot_for_repository(&primary).unwrap();
        let linked_snapshot = snapshot_for_repository(&discovered).unwrap();
        assert_eq!(
            primary_snapshot.repository_id,
            linked_snapshot.repository_id
        );
        assert_ne!(primary_snapshot.worktree_id, linked_snapshot.worktree_id);
        assert!(workspace_snapshot(&linked).unwrap().is_some());
    }

    #[test]
    #[cfg(windows)]
    fn canonical_path_ids_follow_windows_case_insensitive_path_semantics() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(temporary.path()).unwrap();
        let differently_cased = PathBuf::from(canonical.to_string_lossy().to_ascii_uppercase());
        assert!(same_path(&canonical, &differently_cased));
        assert_eq!(
            git_path_id(b"mework.git.repository-id.v1", &canonical).unwrap(),
            git_path_id(b"mework.git.repository-id.v1", &differently_cased).unwrap()
        );
        let repository_id = git_path_id(b"mework.git.repository-id.v1", &canonical).unwrap();
        assert_eq!(
            git_worktree_id(&repository_id, &canonical, &canonical).unwrap(),
            git_worktree_id(&repository_id, &differently_cased, &differently_cased).unwrap()
        );
    }

    #[test]
    fn repository_path_id_changes_when_a_directory_is_recreated_at_the_same_path() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("recreated-repository");
        fs::create_dir(&path).unwrap();
        let first = git_path_id(b"mework.git.repository-id.v1", &path).unwrap();
        fs::remove_dir(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let second = git_path_id(b"mework.git.repository-id.v1", &path).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn git_command_prefix_disables_parent_repository_submodule_recursion() {
        let prefix = git_command_prefix()
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            prefix,
            [
                "-c",
                "core.fsmonitor=false",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "submodule.recurse=false",
                "-c",
                "fetch.recurseSubmodules=false",
                "-c",
                "push.recurseSubmodules=no",
                "-c",
                "color.ui=false",
                "-c",
                "core.quotepath=false",
            ]
        );
    }

    #[test]
    fn nested_submodule_changes_are_reported_and_never_treated_as_parent_changes() {
        if !git_available() {
            return;
        }
        let child = initialized_repository();
        let parent = initialized_repository();
        run_test_git(
            parent.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                child.path().to_str().unwrap(),
                "module",
            ],
        );
        run_test_git(parent.path(), &["commit", "-am", "add submodule"]);
        fs::write(parent.path().join("module").join("tracked.txt"), "nested\n").unwrap();

        let snapshot = workspace_snapshot(parent.path()).unwrap().unwrap();
        let submodule = snapshot
            .files
            .iter()
            .find(|change| change.path == "module")
            .unwrap();
        assert!(submodule.submodule);
        assert!(submodule.submodule_modified);
        assert!(!submodule.submodule_commit_changed);
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("子模块")));
        let expected_content_revision = snapshot.content_revision.clone();

        let stage_error = execute_action(
            parent.path(),
            GitAction::Stage {
                paths: vec!["module".into()],
            },
        )
        .unwrap_err();
        assert!(stage_error.contains("没有可暂存的 gitlink"));
        let discard_error = execute_action(
            parent.path(),
            GitAction::Discard {
                paths: vec!["module".into()],
                include_untracked: false,
                expected_content_revision,
                expected_target_revision: "0".repeat(64),
            },
        )
        .unwrap_err();
        assert!(discard_error.contains("不会从父仓库递归丢弃"));
        assert_eq!(
            fs::read_to_string(parent.path().join("module").join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "nested\n"
        );
    }

    #[test]
    fn snapshot_actions_branches_and_history_form_a_vertical_slice() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "first\nsecond\n").unwrap();
        fs::write(repository.path().join("new.txt"), "new\n").unwrap();

        let before = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(matches!(before.branch.as_deref(), Some("master" | "main")));
        assert_eq!(before.unstaged, 2);
        assert!(!before.is_clean);

        let staged = execute_action(
            repository.path(),
            GitAction::Stage {
                paths: vec!["new.txt".into()],
            },
        )
        .unwrap();
        assert_eq!(staged.snapshot.unwrap().staged, 1);

        let branches = branches(repository.path()).unwrap();
        assert!(branches.branches.iter().any(|branch| branch.current));
    }

    #[test]
    fn snapshot_counts_combined_diff_once_and_leaves_untracked_lines_unknown() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        for name in ["both.txt", "staged.txt", "unstaged.txt"] {
            fs::write(repository.path().join(name), "base\n").unwrap();
        }
        run_test_git(
            repository.path(),
            &["add", "both.txt", "staged.txt", "unstaged.txt"],
        );
        run_test_git(repository.path(), &["commit", "-m", "add fixtures"]);

        fs::write(repository.path().join("both.txt"), "index version\n").unwrap();
        fs::write(repository.path().join("staged.txt"), "base\nstaged\n").unwrap();
        run_test_git(repository.path(), &["add", "both.txt", "staged.txt"]);
        fs::write(repository.path().join("both.txt"), "worktree version\n").unwrap();
        fs::write(repository.path().join("unstaged.txt"), "base\nunstaged\n").unwrap();
        fs::write(repository.path().join("untracked.txt"), "one\ntwo\n").unwrap();

        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(snapshot.staged, 2);
        assert_eq!(snapshot.unstaged, 3);
        assert_eq!(snapshot.untracked, 1);
        assert_eq!(snapshot.additions, 3);
        assert_eq!(snapshot.deletions, 1);
        let both = snapshot
            .files
            .iter()
            .find(|file| file.path == "both.txt")
            .unwrap();
        assert_eq!(both.additions, Some(1));
        assert_eq!(both.deletions, Some(1));
        let untracked = snapshot
            .files
            .iter()
            .find(|file| file.path == "untracked.txt")
            .unwrap();
        assert_eq!(untracked.additions, None);
        assert_eq!(untracked.deletions, None);
    }

    #[test]
    fn snapshot_content_revision_detects_equal_line_count_edits_and_is_stable() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "second\n").unwrap();

        let second = workspace_snapshot(repository.path()).unwrap().unwrap();
        let second_again = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(second.additions, 1);
        assert_eq!(second.deletions, 1);
        assert_eq!(second.content_revision.len(), 64);
        assert_eq!(second.content_revision, second_again.content_revision);

        fs::write(repository.path().join("tracked.txt"), "planet\n").unwrap();
        let planet = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(planet.additions, 1);
        assert_eq!(planet.deletions, 1);
        assert_ne!(second.content_revision, planet.content_revision);
    }

    #[test]
    fn snapshot_fast_content_revision_does_not_read_untracked_content() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();

        let alpha = workspace_snapshot(repository.path()).unwrap().unwrap();
        let alpha_again = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(alpha.untracked, 1);
        assert_eq!(alpha.additions, 0);
        assert_eq!(alpha.files[0].additions, None);
        assert_eq!(alpha.content_revision, alpha_again.content_revision);

        fs::write(&path, "omega\n").unwrap();
        let omega = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(omega.untracked, 1);
        assert_eq!(omega.additions, 0);
        assert_eq!(omega.files[0].additions, None);
        assert_eq!(alpha.content_revision, omega.content_revision);
    }

    #[test]
    fn snapshot_content_revision_handles_unborn_binary_rename_and_delete() {
        if !git_available() {
            return;
        }
        let repository = tempfile::tempdir().unwrap();
        run_test_git(repository.path(), &["init"]);
        run_test_git(repository.path(), &["config", "user.name", "Mework Test"]);
        run_test_git(
            repository.path(),
            &["config", "user.email", "mework@example.invalid"],
        );
        let binary = repository.path().join("binary.dat");
        fs::write(&binary, [0, 1, 2, 3]).unwrap();
        run_test_git(repository.path(), &["add", "binary.dat"]);

        let unborn = workspace_snapshot(repository.path()).unwrap().unwrap();
        let unborn_again = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(unborn.unborn);
        assert_eq!(unborn.binary_files, 1);
        assert_eq!(unborn.content_revision, unborn_again.content_revision);

        fs::write(&binary, [0, 1, 2, 4]).unwrap();
        run_test_git(repository.path(), &["add", "binary.dat"]);
        let changed_binary = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_ne!(unborn.content_revision, changed_binary.content_revision);

        run_test_git(repository.path(), &["commit", "-m", "add binary"]);
        let clean = workspace_snapshot(repository.path()).unwrap().unwrap();
        run_test_git(repository.path(), &["mv", "binary.dat", "renamed.dat"]);
        let renamed = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(renamed.files.iter().any(|change| {
            change.path == "renamed.dat"
                && change.original_path.as_deref() == Some("binary.dat")
                && change.status == GitFileStatus::Renamed
        }));
        assert_ne!(clean.content_revision, renamed.content_revision);

        run_test_git(repository.path(), &["commit", "-m", "rename binary"]);
        let renamed_clean = workspace_snapshot(repository.path()).unwrap().unwrap();
        fs::remove_file(repository.path().join("renamed.dat")).unwrap();
        let deleted = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(deleted.files.iter().any(|change| {
            change.path == "renamed.dat" && change.status == GitFileStatus::Deleted
        }));
        assert_ne!(renamed_clean.content_revision, deleted.content_revision);
    }

    #[test]
    fn capture_pipe_hashes_the_full_stream_while_retaining_only_the_limit() {
        let payload = (0..65_537)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let captured = capture_pipe(std::io::Cursor::new(payload.clone()), 17)
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(captured.output, payload[..17]);
        assert!(captured.truncated);
        let expected: [u8; 32] = Sha256::digest(&payload).into();
        assert_eq!(captured.sha256, expected);
    }

    #[test]
    fn discard_restores_worktree_from_index_without_losing_staged_content() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "staged\n").unwrap();
        run_test_git(repository.path(), &["add", "tracked.txt"]);
        fs::write(repository.path().join("tracked.txt"), "unstaged\n").unwrap();
        let paths = vec!["tracked.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, false).unwrap();

        execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: false,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(repository.path().join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "staged\n",
        );
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let tracked = snapshot
            .files
            .iter()
            .find(|file| file.path == "tracked.txt")
            .unwrap();
        assert!(tracked.staged);
        assert!(!tracked.unstaged);
    }

    #[test]
    fn discard_removes_only_selected_untracked_files() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("remove-me.txt"), "remove\n").unwrap();
        fs::write(repository.path().join("keep-me.txt"), "keep\n").unwrap();
        let paths = vec!["remove-me.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, true).unwrap();

        execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: true,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();

        assert!(!repository.path().join("remove-me.txt").exists());
        assert!(repository.path().join("keep-me.txt").is_file());
        assert!(workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .files
            .iter()
            .any(|file| file.path == "keep-me.txt"));
    }

    #[test]
    fn discard_rejects_content_that_changed_after_confirmation() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "confirmed\n").unwrap();
        let paths = vec!["tracked.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, false).unwrap();
        fs::write(repository.path().join("tracked.txt"), "other\n").unwrap();

        let error = execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: false,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();

        assert!(error.contains("确认后发生变化"));
        assert_eq!(
            fs::read_to_string(repository.path().join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "other\n"
        );
    }

    #[test]
    fn discard_target_revision_rejects_same_size_untracked_rewrite_with_restored_mtime() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let paths = vec!["untracked.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, true).unwrap();

        fs::write(&path, "omega\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let rewritten = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(
            preparation.snapshot.content_revision,
            rewritten.content_revision
        );

        let error = execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: true,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待丢弃文件已在确认后发生变化"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "omega\n");
    }

    #[test]
    fn discard_preparation_rejects_selected_untracked_without_delete_permission() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("keep.txt");
        fs::write(&path, "keep\n").unwrap();
        let error =
            prepare_discard(repository.path(), &["keep.txt".to_owned()], false).unwrap_err();
        assert!(error.contains("明确允许删除未跟踪文件"));
        assert_eq!(fs::read_to_string(path).unwrap(), "keep\n");
    }

    #[test]
    fn hash_object_batch_output_follows_argument_order_used_by_target_proofs() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        run_test_git(root.path(), &["init"]);
        fs::write(root.path().join("first.bin"), b"first").unwrap();
        fs::write(root.path().join("second.bin"), b"second").unwrap();
        let repository = require_repository(root.path()).unwrap();
        let hash = |paths: &[&str]| {
            let mut args = vec![
                OsString::from("hash-object"),
                OsString::from("--no-filters"),
                OsString::from("--"),
            ];
            args.extend(paths.iter().map(OsString::from));
            let output = run_git(
                &repository,
                args,
                None,
                DISCARD_TARGET_REVISION_TIMEOUT,
                4096,
                true,
            )
            .unwrap();
            require_success("测试 Git hash-object 顺序", &output).unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        };
        let first = hash(&["first.bin"]);
        let second = hash(&["second.bin"]);
        assert_eq!(
            hash(&["second.bin", "first.bin"]),
            vec![second[0].clone(), first[0].clone()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn discard_target_revision_hashes_symlink_target_without_following_it() {
        use std::os::unix::fs::symlink;

        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("target-a.txt"), "a\n").unwrap();
        fs::write(repository.path().join("target-b.txt"), "b\n").unwrap();
        let link = repository.path().join("link.txt");
        symlink("target-a.txt", &link).unwrap();
        let paths = vec!["link.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, true).unwrap();

        fs::remove_file(&link).unwrap();
        symlink("target-b.txt", &link).unwrap();
        let current = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(
            preparation.snapshot.content_revision,
            current.content_revision
        );
        let error = execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: true,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待丢弃文件已在确认后发生变化"));
        assert_eq!(fs::read_link(link).unwrap(), PathBuf::from("target-b.txt"));
    }

    #[test]
    fn selected_tracked_discard_proof_ignores_unrelated_sparse_untracked_content() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();
        let huge = repository.path().join("unrelated-huge.bin");
        fs::File::create(&huge)
            .unwrap()
            .set_len(2 * 1024 * 1024 * 1024)
            .unwrap();
        let paths = vec!["tracked.txt".to_owned()];
        let preparation = prepare_discard(repository.path(), &paths, false).unwrap();
        let untracked = preparation
            .snapshot
            .files
            .iter()
            .find(|change| change.path == "unrelated-huge.bin")
            .unwrap();
        assert_eq!(untracked.additions, None);
        assert_eq!(untracked.deletions, None);

        fs::File::options()
            .write(true)
            .open(&huge)
            .unwrap()
            .set_len(3 * 1024 * 1024 * 1024)
            .unwrap();
        execute_action(
            repository.path(),
            GitAction::Discard {
                paths,
                include_untracked: false,
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(repository.path().join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "first\n"
        );
        assert_eq!(fs::metadata(&huge).unwrap().len(), 3 * 1024 * 1024 * 1024);
    }

    #[test]
    fn discard_target_revision_is_bound_to_the_canonical_worktree() {
        if !git_available() {
            return;
        }
        let repository_a = initialized_repository();
        let repository_b = initialized_repository();
        fs::write(repository_a.path().join("tracked.txt"), "changed\n").unwrap();
        fs::write(repository_b.path().join("tracked.txt"), "changed\n").unwrap();
        let paths = vec!["tracked.txt".to_owned()];
        let preparation_a = prepare_discard(repository_a.path(), &paths, false).unwrap();
        let preparation_b = prepare_discard(repository_b.path(), &paths, false).unwrap();
        assert_ne!(preparation_a.target_revision, preparation_b.target_revision);

        let error = execute_action(
            repository_b.path(),
            GitAction::Discard {
                paths,
                include_untracked: false,
                expected_content_revision: preparation_b.snapshot.content_revision,
                expected_target_revision: preparation_a.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待丢弃文件已在确认后发生变化"));
        assert_eq!(
            fs::read_to_string(repository_b.path().join("tracked.txt")).unwrap(),
            "changed\n"
        );
    }

    #[test]
    fn discard_preparation_binds_rename_source_and_missing_worktree_state() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        run_test_git(repository.path(), &["mv", "tracked.txt", "renamed.txt"]);
        fs::write(repository.path().join("renamed.txt"), "worktree\n").unwrap();
        let rename_paths = vec!["renamed.txt".to_owned()];
        let renamed = prepare_discard(repository.path(), &rename_paths, false).unwrap();
        assert_eq!(renamed.target_revision.len(), 64);

        fs::remove_file(repository.path().join("renamed.txt")).unwrap();
        let missing = prepare_discard(repository.path(), &rename_paths, false).unwrap();
        assert_ne!(renamed.target_revision, missing.target_revision);
        execute_action(
            repository.path(),
            GitAction::Discard {
                paths: rename_paths,
                include_untracked: false,
                expected_content_revision: missing.snapshot.content_revision,
                expected_target_revision: missing.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(repository.path().join("renamed.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "first\n"
        );
    }

    #[test]
    fn untracked_deletion_is_handle_relative_and_rejects_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested").join("file.txt"), "safe\n").unwrap();
        remove_exact_untracked_file(root.path(), "nested/file.txt").unwrap();
        assert!(!root.path().join("nested").join("file.txt").exists());

        fs::create_dir(root.path().join("directory")).unwrap();
        let error = remove_exact_untracked_file(root.path(), "directory").unwrap_err();
        assert!(error.contains("目录"));
        assert!(root.path().join("directory").is_dir());
    }

    #[test]
    fn untracked_deletion_rejects_intermediate_link_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, "must remain\n").unwrap();
        let link = root.path().join("jump");

        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(outside.path(), &link) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create test directory symlink: {error}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        let error = remove_exact_untracked_file(root.path(), "jump/victim.txt").unwrap_err();
        assert!(error.contains("拒绝") || error.contains("无法打开"));
        assert_eq!(fs::read_to_string(victim).unwrap(), "must remain\n");
    }

    #[test]
    fn git_cli_environment_cannot_redirect_repository_discovery() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let plain = tempfile::tempdir().unwrap();
        let git = find_program("git").unwrap();
        let mut command = Command::new(git);
        command
            .current_dir(plain.path())
            .args(["rev-parse", "--show-toplevel"])
            .env("GIT_DIR", repository.path().join(".git"))
            .env("GIT_WORK_TREE", plain.path())
            .env("GIT_INDEX_FILE", repository.path().join("redirected-index"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.bare")
            .env("GIT_CONFIG_VALUE_0", "false");

        configure_cli_environment(&mut command, CliKind::GitPassive);
        let scrubbed = command
            .get_envs()
            .filter(|(name, _)| is_git_environment_override(name))
            .collect::<Vec<_>>();
        assert!(
            scrubbed.iter().all(|(name, value)| {
                if name
                    .to_string_lossy()
                    .eq_ignore_ascii_case("GIT_OPTIONAL_LOCKS")
                {
                    value.is_some_and(|value| value == "0")
                } else {
                    value.is_none()
                }
            }),
            "Git routing/configuration overrides must be removed: {scrubbed:?}"
        );

        let output = command.output().unwrap();
        assert!(
            !output.status.success(),
            "environment overrides escaped the non-repository workspace: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn diff_context_fixture() -> tempfile::TempDir {
        let repository = initialized_repository();
        let lines = (1..=40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        fs::write(
            repository.path().join("context.txt"),
            lines.join("\n") + "\n",
        )
        .unwrap();
        run_test_git(repository.path(), &["add", "context.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "context file"]);
        let mut changed = (1..=40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        changed[19] = "changed".into();
        fs::write(
            repository.path().join("context.txt"),
            changed.join("\n") + "\n",
        )
        .unwrap();
        repository
    }

    fn diff_context_lines(patch: &str) -> usize {
        patch.lines().filter(|line| line.starts_with(' ')).count()
    }

    #[test]
    fn diff_context_defaults_to_three_lines_when_absent() {
        if !git_available() {
            return;
        }
        let repository = diff_context_fixture();
        let default_patch = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("context.txt".into()),
                context: None,
            },
        )
        .unwrap()
        .patch;
        assert_eq!(diff_context_lines(&default_patch), 6);
        assert!(default_patch.contains("-line 20"));
        assert!(default_patch.contains("+changed"));
    }

    #[test]
    fn diff_context_zero_emits_no_context_lines() {
        if !git_available() {
            return;
        }
        let repository = diff_context_fixture();
        let patch = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("context.txt".into()),
                context: Some(0),
            },
        )
        .unwrap()
        .patch;
        assert_eq!(diff_context_lines(&patch), 0);
        assert!(patch.contains("-line 20"));
        assert!(patch.contains("+changed"));
    }

    #[test]
    fn diff_context_twenty_five_returns_strictly_more_lines_than_default() {
        if !git_available() {
            return;
        }
        let repository = diff_context_fixture();
        let default_patch = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("context.txt".into()),
                context: None,
            },
        )
        .unwrap()
        .patch;
        let wide_patch = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("context.txt".into()),
                context: Some(25),
            },
        )
        .unwrap()
        .patch;
        assert_eq!(diff_context_lines(&default_patch), 6);
        assert_eq!(diff_context_lines(&wide_patch), 39);
        assert!(wide_patch.lines().count() > default_patch.lines().count());
    }

    #[test]
    fn diff_context_u32_max_is_clamped_and_still_succeeds() {
        if !git_available() {
            return;
        }
        let repository = diff_context_fixture();
        let patch = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("context.txt".into()),
                context: Some(u32::MAX),
            },
        )
        .unwrap()
        .patch;
        assert!(patch.contains("-line 20"));
        assert!(patch.contains("+changed"));
        assert_eq!(diff_context_lines(&patch), 39);
    }

    #[test]
    fn branch_compare_and_checkout_actions_work_together() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let base = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        run_test_git(repository.path(), &["switch", "-c", "feature/test"]);
        fs::write(repository.path().join("feature.txt"), "feature\n").unwrap();
        execute_action(
            repository.path(),
            GitAction::Stage {
                paths: vec!["feature.txt".into()],
            },
        )
        .unwrap();
        run_test_git(repository.path(), &["commit", "-m", "feature commit"]);
        let comparison = diff(
            repository.path(),
            GitDiffRequest::Compare {
                base: base.clone(),
                head: "feature/test".into(),
                path: None,
                context: None,
            },
        )
        .unwrap();
        assert!(comparison
            .files
            .iter()
            .any(|file| file.path == "feature.txt"));
        let checked_out = execute_action(
            repository.path(),
            GitAction::Checkout {
                branch: base.clone(),
            },
        )
        .unwrap();
        assert_eq!(checked_out.snapshot.unwrap().branch, Some(base));
    }

    #[test]
    fn snapshot_uses_the_atom_qualified_upstream_and_binds_all_remote_transports() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let origin = tempfile::tempdir().unwrap();
        let upstream = tempfile::tempdir().unwrap();
        run_test_git(origin.path(), &["init", "--bare"]);
        run_test_git(upstream.path(), &["init", "--bare"]);
        let origin_path = origin.path().to_string_lossy().into_owned();
        let upstream_path = upstream.path().to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["remote", "add", "origin", &origin_path],
        );
        run_test_git(
            repository.path(),
            &["remote", "add", "upstream", &upstream_path],
        );
        run_test_git(
            repository.path(),
            &["push", "origin", "HEAD:refs/heads/main"],
        );
        run_test_git(
            repository.path(),
            &["push", "upstream", "HEAD:refs/heads/review"],
        );
        run_test_git(repository.path(), &["fetch", "upstream"]);
        let branch = String::from_utf8(run_test_git_output(
            repository.path(),
            &["branch", "--show-current"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(
            repository.path(),
            &["branch", "--set-upstream-to=upstream/review", "--", &branch],
        );

        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(snapshot.remote.as_ref().unwrap().name, "upstream");
        assert_eq!(
            snapshot
                .remotes
                .iter()
                .map(|remote| remote.name.as_str())
                .collect::<Vec<_>>(),
            ["origin", "upstream"]
        );
        let target = snapshot.upstream_target.as_ref().unwrap();
        assert_eq!(target.remote_name, "upstream");
        assert_eq!(target.remote_branch, "review");
        assert_eq!(target.merge_ref, "refs/heads/review");
        assert_eq!(target.tracking_ref, "refs/remotes/upstream/review");
        assert_eq!(target.tracking_oid.as_deref(), snapshot.head.as_deref());
        assert!(!target.is_local);
        assert_eq!(target.remote, snapshot.remote.clone().unwrap());
        assert!(snapshot.remotes.iter().all(|remote| remote.url.is_none()
            && remote.fetch_revision.len() == 64
            && remote.push_revision.len() == 64));
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains(&origin_path));
        assert!(!serialized.contains(&upstream_path));

        run_test_git(repository.path(), &["config", "remote.broken.url", ""]);
        run_test_git(
            repository.path(),
            &[
                "config",
                "remote.broken.fetch",
                "+refs/heads/*:refs/remotes/broken/*",
            ],
        );
        let with_broken = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(with_broken
            .remotes
            .iter()
            .any(|remote| remote.name == "origin"));
        assert!(with_broken
            .remotes
            .iter()
            .any(|remote| remote.name == "upstream"));
        assert!(with_broken
            .warnings
            .iter()
            .any(|warning| warning.contains("remote broken")));
    }

    #[test]
    fn remote_proofs_detect_fetch_and_pushurl_drift_without_serializing_credentials() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        run_test_git(first.path(), &["init", "--bare"]);
        run_test_git(second.path(), &["init", "--bare"]);
        let first_path = first.path().to_string_lossy().into_owned();
        let second_path = second.path().to_string_lossy().into_owned();
        run_test_git(repository.path(), &["remote", "add", "origin", &first_path]);
        let initial = workspace_snapshot(repository.path()).unwrap().unwrap();
        let initial_remote = initial.remote.clone().unwrap();

        run_test_git(
            repository.path(),
            &["remote", "set-url", "--push", "origin", &second_path],
        );
        let push_changed = workspace_snapshot(repository.path()).unwrap().unwrap();
        let push_remote = push_changed.remote.clone().unwrap();
        assert_eq!(initial_remote.fetch_revision, push_remote.fetch_revision);
        assert_ne!(initial_remote.push_revision, push_remote.push_revision);

        run_test_git(
            repository.path(),
            &["remote", "set-url", "origin", &second_path],
        );
        let fetch_changed = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_ne!(
            push_remote.fetch_revision,
            fetch_changed.remote.as_ref().unwrap().fetch_revision
        );

        let secret = "credential-secret-never-render";
        let credential_url = format!("https://user:{secret}@example.invalid/team/repo.git");
        run_test_git(
            repository.path(),
            &["remote", "set-url", "origin", &credential_url],
        );
        run_test_git(
            repository.path(),
            &["remote", "set-url", "--push", "origin", &credential_url],
        );
        let credential_snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let serialized = serde_json::to_string(&credential_snapshot).unwrap();
        assert!(!serialized.contains(secret));
        assert!(!serialized.contains("example.invalid"));
    }

    #[test]
    fn local_dot_upstream_is_visible_in_the_snapshot() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let branch = String::from_utf8(run_test_git_output(
            repository.path(),
            &["branch", "--show-current"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(
            repository.path(),
            &["config", &format!("branch.{branch}.remote"), "."],
        );
        run_test_git(
            repository.path(),
            &[
                "config",
                &format!("branch.{branch}.merge"),
                "refs/heads/missing-peer",
            ],
        );
        let missing = workspace_snapshot(repository.path()).unwrap().unwrap();
        let missing_target = missing.upstream_target.as_ref().unwrap();
        assert!(missing_target.is_local);
        assert_eq!(missing_target.remote_branch, "missing-peer");
        assert_eq!(missing_target.tracking_oid, None);
        assert!(serde_json::to_value(&missing).unwrap()["upstreamTarget"].is_object());

        run_test_git(repository.path(), &["branch", "local-peer"]);
        run_test_git(
            repository.path(),
            &[
                "config",
                &format!("branch.{branch}.merge"),
                "refs/heads/local-peer",
            ],
        );
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let target = snapshot.upstream_target.clone().unwrap();
        assert!(target.is_local);
        assert_eq!(target.remote_name, ".");
        assert_eq!(target.tracking_ref, "refs/heads/local-peer");
        assert_eq!(target.tracking_oid.as_deref(), snapshot.head.as_deref());
        assert!(snapshot.remotes.iter().any(|remote| remote.name == "."));
    }

    #[test]
    fn unstaging_a_rename_restores_both_sides_of_the_index_change() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        run_test_git(repository.path(), &["mv", "tracked.txt", "renamed.txt"]);
        let renamed = workspace_snapshot(repository.path()).unwrap().unwrap();
        let change = renamed
            .files
            .iter()
            .find(|change| change.path == "renamed.txt")
            .unwrap();
        assert_eq!(change.status, GitFileStatus::Renamed);
        assert_eq!(change.original_path.as_deref(), Some("tracked.txt"));

        execute_action(
            repository.path(),
            GitAction::Unstage {
                paths: vec!["renamed.txt".into()],
            },
        )
        .unwrap();
        let unstaged = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(unstaged.staged, 0);
        assert!(unstaged.files.iter().any(|change| {
            change.path == "tracked.txt"
                && change.status == GitFileStatus::Deleted
                && change.unstaged
        }));
        assert!(unstaged.files.iter().any(|change| {
            change.path == "renamed.txt"
                && change.status == GitFileStatus::Untracked
                && change.untracked
        }));
    }

    #[test]
    fn conflicted_merge_can_be_resolved_and_continued_without_an_editor() {
        if !git_available() {
            return;
        }
        let (repository, _) = repository_with_merge_conflict();
        start_conflicted_merge(repository.path());

        let conflicted = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(conflicted.operation, Some(GitRepositoryOperation::Merge));
        assert_eq!(conflicted.conflicted, 1);
        let expected_head = conflicted.head.unwrap();
        let blocked = execute_action(
            repository.path(),
            GitAction::Checkout {
                branch: "feature/conflict".into(),
            },
        )
        .unwrap_err();
        assert!(blocked.contains("请先解决冲突并继续"));

        fs::write(repository.path().join("tracked.txt"), "resolved\n").unwrap();
        execute_action(
            repository.path(),
            GitAction::Stage {
                paths: vec!["tracked.txt".into()],
            },
        )
        .unwrap();
        let operation_revision = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .operation_revision
            .unwrap();
        let continued = execute_action(
            repository.path(),
            GitAction::ContinueOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head,
                expected_operation_revision: operation_revision,
            },
        )
        .unwrap();
        let snapshot = continued.snapshot.unwrap();
        assert_eq!(snapshot.operation, None);
        assert_eq!(snapshot.conflicted, 0);
        assert!(snapshot.is_clean);
        let parents =
            run_test_git_output(repository.path(), &["show", "-s", "--format=%P", "HEAD"]);
        assert_eq!(
            String::from_utf8_lossy(&parents).split_whitespace().count(),
            2
        );
    }

    #[test]
    fn conflicted_merge_can_be_aborted_and_stale_controls_fail_closed() {
        if !git_available() {
            return;
        }
        let (repository, _) = repository_with_merge_conflict();
        start_conflicted_merge(repository.path());
        let conflicted = workspace_snapshot(repository.path()).unwrap().unwrap();
        let expected_head = conflicted.head.unwrap();
        let expected_operation_revision = conflicted.operation_revision.unwrap();
        let changed_confirmation = execute_action(
            repository.path(),
            GitAction::AbortOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head: "0000000000000000000000000000000000000000".into(),
                expected_operation_revision: expected_operation_revision.clone(),
            },
        )
        .unwrap_err();
        assert!(changed_confirmation.contains("HEAD 已在确认后发生变化"));

        let aborted = execute_action(
            repository.path(),
            GitAction::AbortOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head: expected_head.clone(),
                expected_operation_revision: expected_operation_revision.clone(),
            },
        )
        .unwrap();
        let snapshot = aborted.snapshot.unwrap();
        assert_eq!(snapshot.operation, None);
        assert_eq!(
            fs::read_to_string(repository.path().join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "main\n"
        );

        let stale = execute_action(
            repository.path(),
            GitAction::AbortOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head: expected_head.clone(),
                expected_operation_revision: expected_operation_revision.clone(),
            },
        )
        .unwrap_err();
        assert!(stale.contains("已经结束"));

        start_conflicted_merge(repository.path());
        let restarted = workspace_snapshot(repository.path()).unwrap().unwrap();
        let restarted_revision = restarted.operation_revision.unwrap();
        assert_ne!(restarted_revision, expected_operation_revision);
        let stale_restart = execute_action(
            repository.path(),
            GitAction::AbortOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head: expected_head.clone(),
                expected_operation_revision,
            },
        )
        .unwrap_err();
        assert!(stale_restart.contains("变化或重新开始"));
        execute_action(
            repository.path(),
            GitAction::AbortOperation {
                operation: GitRepositoryOperation::Merge,
                expected_head,
                expected_operation_revision: restarted_revision,
            },
        )
        .unwrap();
    }

    #[test]
    fn bisect_steps_map_custom_terms_and_bind_head_operation_and_clean_worktree() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        for revision in 1..=12 {
            fs::write(
                repository.path().join("tracked.txt"),
                format!("revision {revision}\n"),
            )
            .unwrap();
            run_test_git(repository.path(), &["add", "tracked.txt"]);
            run_test_git(
                repository.path(),
                &["commit", "-m", &format!("revision {revision}")],
            );
        }
        run_test_git(
            repository.path(),
            &[
                "bisect",
                "start",
                "--term-old=works",
                "--term-new=breaks",
                "HEAD",
                "HEAD~12",
            ],
        );

        let first = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(first.operation, Some(GitRepositoryOperation::Bisect));
        assert!(first.is_clean);
        let first_head = first.head.clone().unwrap();
        let first_operation_revision = first.operation_revision.clone().unwrap();
        let advanced = execute_action(
            repository.path(),
            GitAction::BisectStep {
                outcome: GitBisectOutcome::Old,
                expected_head: first_head.clone(),
                expected_operation_revision: first_operation_revision.clone(),
                expected_content_revision: first.content_revision.clone(),
            },
        )
        .unwrap()
        .snapshot
        .unwrap();
        assert_eq!(advanced.operation, Some(GitRepositoryOperation::Bisect));
        assert_ne!(advanced.head.as_deref(), Some(first_head.as_str()));
        let git_dir = require_repository(repository.path()).unwrap().git_dir;
        let log = fs::read_to_string(git_dir.join("BISECT_LOG")).unwrap();
        assert!(log.contains("git bisect works"));

        let stale = execute_action(
            repository.path(),
            GitAction::BisectStep {
                outcome: GitBisectOutcome::Old,
                expected_head: first_head,
                expected_operation_revision: first_operation_revision,
                expected_content_revision: first.content_revision,
            },
        )
        .unwrap_err();
        assert!(stale.contains("HEAD 已在确认后发生变化"));

        let skipped = execute_action(
            repository.path(),
            GitAction::BisectStep {
                outcome: GitBisectOutcome::Skip,
                expected_head: advanced.head.clone().unwrap(),
                expected_operation_revision: advanced.operation_revision.clone().unwrap(),
                expected_content_revision: advanced.content_revision.clone(),
            },
        )
        .unwrap()
        .snapshot
        .unwrap();
        assert_eq!(skipped.operation, Some(GitRepositoryOperation::Bisect));
        let log = fs::read_to_string(git_dir.join("BISECT_LOG")).unwrap();
        assert!(log.contains("git bisect skip"));

        let marked_new = execute_action(
            repository.path(),
            GitAction::BisectStep {
                outcome: GitBisectOutcome::New,
                expected_head: skipped.head.clone().unwrap(),
                expected_operation_revision: skipped.operation_revision.clone().unwrap(),
                expected_content_revision: skipped.content_revision.clone(),
            },
        )
        .unwrap()
        .snapshot
        .unwrap();
        let log = fs::read_to_string(git_dir.join("BISECT_LOG")).unwrap();
        assert!(log.contains("git bisect breaks"));

        fs::write(repository.path().join("dirty.txt"), "dirty\n").unwrap();
        let dirty = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(!dirty.is_clean);
        let error = execute_action(
            repository.path(),
            GitAction::BisectStep {
                outcome: GitBisectOutcome::Skip,
                expected_head: dirty.head.unwrap(),
                expected_operation_revision: dirty.operation_revision.unwrap(),
                expected_content_revision: dirty.content_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("工作树干净"));
        assert_eq!(
            test_commit_oid(repository.path(), "HEAD"),
            marked_new.head.unwrap()
        );
    }

    #[test]
    fn redacts_tokens_and_url_userinfo_from_diagnostics() {
        let redacted = redact_sensitive_text(
            "https://secret@example.com/repo ssh://oauth:secret@example.com/repo \
             secret@github.com:owner/repo ghp_abcdefghijklmnopqrstuvwxyz",
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("ghp_"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
