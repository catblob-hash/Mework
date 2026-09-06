use std::{
    collections::{hash_map::RandomState, HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
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
use serde_json::Value;
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const BULK_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const NETWORK_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const MAX_STATUS_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_PATCH_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_JSON_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_GITHUB_VIEWER_LOGIN_OUTPUT: usize = 4 * 1024;
const MAX_ACTION_OUTPUT: usize = 256 * 1024;
const MAX_GIT_INDEX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PATHS_PER_ACTION: usize = 10_000;
const MAX_PATH_BYTES_PER_ACTION: usize = 1024 * 1024;
const MAX_COMMIT_MESSAGE_CHARS: usize = 100_000;
const MAX_COMMIT_MESSAGE_BYTES: usize = MAX_COMMIT_MESSAGE_CHARS * 4 + 1;
const MAX_HISTORY_LIMIT: u16 = 200;
const MAX_HISTORY_SKIP: u32 = 100_000;
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
const MAX_GITHUB_PR_PAGE: u32 = 100_000;
const MAX_GITHUB_PR_PAGE_SIZE: u16 = 100;
const GITHUB_PR_FILES_PAGE_SIZE: u16 = 100;
const MAX_GITHUB_PR_FILE_PAGES: u32 = 30;
const MAX_GITHUB_PR_FILES: u64 = GITHUB_PR_FILES_PAGE_SIZE as u64 * MAX_GITHUB_PR_FILE_PAGES as u64;
const MAX_GITHUB_PR_FILES_PAGE_OUTPUT: usize = 8 * 1024 * 1024;
const DEFAULT_GITHUB_REVIEW_THREADS_PAGE_SIZE: u16 = 30;
const MAX_GITHUB_REVIEW_THREADS_PAGE_SIZE: u16 = 100;
const DEFAULT_GITHUB_REVIEW_COMMENTS_PAGE_SIZE: u16 = 50;
const MAX_GITHUB_REVIEW_COMMENTS_PAGE_SIZE: u16 = 100;
const MAX_GITHUB_REVIEW_CURSOR_BYTES: usize = 2 * 1024;
const MAX_GITHUB_REVIEW_COMMENTS_PER_THREAD: usize = 50;
const MAX_GITHUB_REVIEW_SUBMISSION_COMMENTS: usize = 100;
const MAX_GITHUB_REVIEW_BODY_CHARS: usize = 100_000;
const GITHUB_READINESS_CHECKS_PAGE_SIZE: u16 = 100;
const MAX_GITHUB_READINESS_CHECK_PAGES: u16 = 100;
const MAX_GITHUB_READINESS_CHECKS: usize =
    GITHUB_READINESS_CHECKS_PAGE_SIZE as usize * MAX_GITHUB_READINESS_CHECK_PAGES as usize;
const MAX_GITHUB_READINESS_TIMESTAMP_BYTES: usize = 256;
const GITHUB_READINESS_CORE_QUERY: &str = r#"query MeworkPullRequestReadinessCore($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    id
    nameWithOwner
    mergeCommitAllowed
    squashMergeAllowed
    rebaseMergeAllowed
    pullRequest(number: $number) {
      id
      number
      state
      isDraft
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      mergeStateStatus
      mergeable
      viewerCanUpdate
      viewerCanMergeAsAdmin
      baseRepository { id nameWithOwner }
      headRepository { id nameWithOwner }
    }
  }
}"#;
const GITHUB_READINESS_CHECKS_QUERY: &str = r#"query MeworkPullRequestReadinessChecks($owner: String!, $name: String!, $number: Int!, $first: Int!, $after: String) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    id
    nameWithOwner
    pullRequest(number: $number) {
      id
      number
      state
      isDraft
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      baseRepository { id nameWithOwner }
      headRepository { id nameWithOwner }
      statusCheckRollup {
        contexts(first: $first, after: $after) {
          totalCount
          pageInfo { hasNextPage endCursor }
          nodes {
            __typename
            ... on CheckRun {
              id
              name
              status
              conclusion
              detailsUrl
              startedAt
              completedAt
              checkSuite {
                workflowRun {
                  workflow { name }
                }
              }
              isRequired(pullRequestNumber: $number)
            }
            ... on StatusContext {
              id
              context
              state
              description
              targetUrl
              isRequired(pullRequestNumber: $number)
            }
          }
        }
      }
    }
  }
}"#;
const GITHUB_READINESS_VIEWER_DEFAULT_QUERY: &str = r#"query MeworkPullRequestReadinessViewerDefault($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    id
    nameWithOwner
    viewerDefaultMergeMethod
    pullRequest(number: $number) {
      id
      number
      state
      isDraft
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      baseRepository { id nameWithOwner }
      headRepository { id nameWithOwner }
    }
  }
}"#;
const GITHUB_READINESS_AUTO_MERGE_QUERY: &str = r#"query MeworkPullRequestReadinessAutoMerge($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    id
    nameWithOwner
    pullRequest(number: $number) {
      id
      number
      state
      isDraft
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      baseRepository { id nameWithOwner }
      headRepository { id nameWithOwner }
      autoMergeRequest {
        enabledAt
        mergeMethod
        commitHeadline
        commitBody
        enabledBy { login }
      }
    }
  }
}"#;
const GITHUB_READINESS_MERGE_QUEUE_QUERY: &str = r#"query MeworkPullRequestReadinessMergeQueue($owner: String!, $name: String!, $number: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    id
    nameWithOwner
    pullRequest(number: $number) {
      id
      number
      state
      isDraft
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      baseRepository { id nameWithOwner }
      headRepository { id nameWithOwner }
      isMergeQueueEnabled
      isInMergeQueue
      mergeQueueEntry {
        id
        position
        state
        enqueuedAt
        estimatedTimeToMerge
      }
    }
  }
}"#;
const GITHUB_REVIEW_THREADS_QUERY: &str = r#"query MeworkPullRequestReviewThreads($owner: String!, $name: String!, $number: Int!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number
      state
      headRefOid
      reviewThreads(first: $first, after: $after) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          path
          line
          startLine
          diffSide
          startDiffSide
          originalLine
          originalStartLine
          isResolved
          isOutdated
          viewerCanReply
          viewerCanResolve
          viewerCanUnresolve
          comments(first: 50) {
            totalCount
            pageInfo { hasNextPage endCursor }
            nodes {
              id
              author { login }
              body
              createdAt
              updatedAt
              url
              replyTo { id }
            }
          }
        }
      }
    }
  }
}"#;
const GITHUB_REVIEW_THREAD_SCOPE_QUERY: &str = r#"query MeworkPullRequestReviewThreadScope($threadId: ID!) {
  node(id: $threadId) {
    __typename
    ... on PullRequestReviewThread {
      id
      viewerCanReply
      viewerCanResolve
      viewerCanUnresolve
      repository {
        name
        owner { login }
      }
      pullRequest {
        number
        state
        headRefOid
      }
    }
  }
}"#;
const GITHUB_REVIEW_THREAD_COMMENTS_QUERY: &str = r#"query MeworkPullRequestReviewThreadComments($threadId: ID!, $first: Int!, $after: String) {
  node(id: $threadId) {
    __typename
    ... on PullRequestReviewThread {
      id
      comments(first: $first, after: $after) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          author { login }
          body
          createdAt
          updatedAt
          url
          replyTo { id }
        }
      }
    }
  }
}"#;
const GITHUB_REPLY_REVIEW_THREAD_MUTATION: &str = r#"mutation MeworkReplyReviewThread($threadId: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $threadId, body: $body}) {
    comment { id url }
  }
}"#;
const GITHUB_RESOLVE_REVIEW_THREAD_MUTATION: &str = r#"mutation MeworkResolveReviewThread($threadId: ID!) {
  resolveReviewThread(input: {threadId: $threadId}) {
    thread { id isResolved }
  }
}"#;
const GITHUB_UNRESOLVE_REVIEW_THREAD_MUTATION: &str = r#"mutation MeworkUnresolveReviewThread($threadId: ID!) {
  unresolveReviewThread(input: {threadId: $threadId}) {
    thread { id isResolved }
  }
}"#;
const DISCARD_TARGET_REVISION_TIMEOUT: Duration = Duration::from_secs(5);
const STAGE_ALL_TARGET_REVISION_TIMEOUT: Duration = BULK_COMMAND_TIMEOUT;
const DISCARD_HASH_OBJECT_ARG_BUDGET: usize = 16 * 1024;
const MAX_OPERATION_REVISION_ARTIFACTS: usize = 1_024;
const MAX_OPERATION_REVISION_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPERATION_REVISION_FILE_BYTES: usize = 256 * 1024;
const GITHUB_PR_DETAIL_JSON_FIELDS: &str = "number,title,body,state,isDraft,headRefName,headRefOid,baseRefName,author,updatedAt,url,additions,deletions,changedFiles,commits,reviewDecision,mergeable,statusCheckRollup";
const GITHUB_PR_LIST_JQ: &str = "map({number,title,state,draft,head:{ref:.head.ref},base:{ref:.base.ref},user:{login:.user.login},updated_at,html_url})";
const GITHUB_PR_FILES_JQ: &str =
    "map({filename,previous_filename,status,additions,deletions,changes})";

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
    #[serde(default)]
    pub expected_stage_all_target_revision: Option<String>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        stage_all_target_revision: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        candidate_tree_oid: Option<String>,
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
pub struct GitStageAllPreparation {
    pub snapshot: GitWorkspaceSnapshot,
    pub target_revision: String,
    pub candidate_tree_oid: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitPreparation {
    pub snapshot: GitWorkspaceSnapshot,
    pub target_revision: String,
    pub candidate_tree_oid: String,
    pub message_digest: String,
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
        expected_stage_all_target_revision: Option<String>,
    },
    Staged {
        #[serde(default)]
        path: Option<String>,
    },
    Unstaged {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        expected_stage_all_target_revision: Option<String>,
    },
    Compare {
        base: String,
        head: String,
        #[serde(default)]
        path: Option<String>,
    },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_all_target_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_tree_oid: Option<String>,
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryRequest {
    #[serde(default = "default_history_limit")]
    pub limit: u16,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryPage {
    pub commits: Vec<GitCommit>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    pub oid: String,
    pub short_oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub committed_at: String,
    pub refs: Vec<String>,
    pub subject: String,
    pub body: String,
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
    StageAll {
        expected_content_revision: String,
        expected_target_revision: String,
    },
    Unstage {
        paths: Vec<String>,
    },
    UnstageAll {
        expected_content_revision: String,
    },
    Discard {
        paths: Vec<String>,
        #[serde(default)]
        include_untracked: bool,
        expected_content_revision: String,
        expected_target_revision: String,
    },
    Commit {
        message: String,
        expected_target_revision: String,
        expected_tree_oid: String,
        #[serde(default)]
        amend: bool,
    },
    Fetch {
        expected_repository_id: String,
        expected_worktree_id: String,
        remote: GitRemote,
    },
    Pull {
        expected_repository_id: String,
        expected_worktree_id: String,
        expected_local_branch: String,
        expected_head: String,
        expected_content_revision: String,
        upstream: GitUpstream,
        #[serde(default)]
        rebase: bool,
        #[serde(default)]
        ff_only: bool,
    },
    Push {
        expected_repository_id: String,
        expected_worktree_id: String,
        remote: GitRemote,
        expected_local_branch: String,
        remote_branch: String,
        expected_head: String,
        expected_upstream: Option<GitUpstream>,
        #[serde(default)]
        set_upstream: bool,
        #[serde(default)]
        force_with_lease: bool,
    },
    Checkout {
        branch: String,
    },
    CreateBranch {
        name: String,
        #[serde(default)]
        start_point: Option<String>,
        #[serde(default)]
        checkout: bool,
    },
    DeleteBranch {
        name: String,
        #[serde(default)]
        force: bool,
        expected_head: String,
        expected_oid: String,
    },
    Merge {
        branch: String,
        expected_head: String,
        expected_branch_oid: String,
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
    Stash {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        include_untracked: bool,
    },
    StashPop {
        #[serde(default)]
        index: Option<u32>,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitActionResult {
    pub message: Option<String>,
    pub snapshot: Option<GitWorkspaceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_oid: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepositoryContext {
    pub cli: GithubCliStatus,
    pub repository: Option<GithubRepository>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubCliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub host: Option<String>,
    pub authenticated: bool,
    pub login: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepository {
    pub host: String,
    pub owner: String,
    pub name: String,
    pub name_with_owner: String,
    pub url: String,
    pub default_branch: Option<String>,
    pub viewer_login: Option<String>,
    pub authenticated: bool,
    pub gh_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepositoryIdentity {
    pub host: String,
    pub owner: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestList {
    pub pull_requests: Vec<GithubPullRequestSummary>,
    pub page: u32,
    pub page_size: u16,
    pub has_more: bool,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub author: Option<String>,
    pub updated_at: String,
    pub url: String,
    pub mergeable: Option<bool>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestCheck {
    pub name: String,
    pub state: String,
    pub workflow: Option<String>,
    pub description: Option<String>,
    pub link: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestDetail {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub author: Option<String>,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    pub base_ref_name: String,
    pub draft: bool,
    pub mergeable: Option<bool>,
    pub updated_at: String,
    pub body: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commits: u64,
    pub review_decision: Option<String>,
    pub status_check_rollup: Option<String>,
    pub checks: Vec<GithubPullRequestCheck>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubReadinessAvailability {
    Available,
    Unsupported,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReadinessPhase<T> {
    pub availability: GithubReadinessAvailability,
    pub value: Option<T>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReadinessRepositoryIdentity {
    pub host: String,
    pub node_id: String,
    pub name_with_owner: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestCoreIdentity {
    pub repository: GithubReadinessRepositoryIdentity,
    pub pull_request_node_id: String,
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub base_repository: GithubReadinessRepositoryIdentity,
    pub head_repository: Option<GithubReadinessRepositoryIdentity>,
    pub base_ref_name: String,
    pub base_ref_oid: String,
    pub head_ref_name: String,
    pub head_ref_oid: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestMergePolicy {
    pub merge_state_status: String,
    pub mergeable: String,
    pub merge_commit_allowed: bool,
    pub squash_merge_allowed: bool,
    pub rebase_merge_allowed: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestViewer {
    pub login: String,
    pub can_update: bool,
    pub can_merge_as_admin: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestReadinessCheck {
    pub node_id: String,
    pub kind: String,
    pub name: String,
    pub state: String,
    pub conclusion: Option<String>,
    pub workflow: Option<String>,
    pub description: Option<String>,
    pub link: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub required: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestChecks {
    pub total_count: u64,
    pub checks: Vec<GithubPullRequestReadinessCheck>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestViewerDefault {
    pub merge_method: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestAutoMerge {
    pub enabled_at: Option<String>,
    pub merge_method: String,
    pub commit_headline: Option<String>,
    pub commit_body: Option<String>,
    pub enabled_by: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestMergeQueueEntry {
    pub entry_id: String,
    pub position: u64,
    pub state: String,
    pub enqueued_at: String,
    pub estimated_time_to_merge: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestMergeQueueState {
    pub enabled: bool,
    pub is_in_queue: bool,
    pub entry: Option<GithubPullRequestMergeQueueEntry>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestReadiness {
    pub identity: GithubPullRequestCoreIdentity,
    pub merge_policy: GithubPullRequestMergePolicy,
    pub viewer: GithubPullRequestViewer,
    pub checks: GithubReadinessPhase<GithubPullRequestChecks>,
    pub viewer_default: GithubReadinessPhase<GithubPullRequestViewerDefault>,
    pub auto_merge: GithubReadinessPhase<Option<GithubPullRequestAutoMerge>>,
    pub merge_queue: GithubReadinessPhase<GithubPullRequestMergeQueueState>,
    pub identity_revision: String,
    pub readiness_revision: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestDiff {
    pub path: Option<String>,
    pub head_ref_oid: String,
    pub patch: String,
    pub truncated: bool,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
    pub files: Vec<GitFileChange>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewThreadsRequest {
    pub number: u64,
    pub expected_head_oid: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_github_review_threads_page_size")]
    pub page_size: u16,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewComment {
    pub id: String,
    pub author: Option<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
    pub reply_to_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub start_line: Option<u64>,
    pub diff_side: String,
    pub start_diff_side: Option<String>,
    pub original_line: Option<u64>,
    pub original_start_line: Option<u64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub viewer_can_reply: bool,
    pub viewer_can_resolve: bool,
    pub viewer_can_unresolve: bool,
    pub comments: Vec<GithubReviewComment>,
    pub comments_total_count: u64,
    pub comments_next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewThreadsResult {
    pub number: u64,
    pub head_ref_oid: String,
    pub threads: Vec<GithubReviewThread>,
    pub total_count: u64,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewThreadCommentsRequest {
    pub expected_repository: GithubRepositoryIdentity,
    pub expected_viewer_login: String,
    pub number: u64,
    pub expected_state: String,
    pub expected_head_oid: String,
    pub thread_id: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_github_review_comments_page_size")]
    pub page_size: u16,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewThreadCommentsResult {
    pub number: u64,
    pub head_ref_oid: String,
    pub thread_id: String,
    pub comments: Vec<GithubReviewComment>,
    pub total_count: u64,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubPullRequestReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubReviewSubmissionComment {
    pub path: String,
    pub line: u64,
    pub side: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GithubAction {
    CreatePullRequest {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        expected_local_head_oid: String,
        expected_content_revision: String,
        title: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        head: Option<String>,
        #[serde(default)]
        draft: bool,
    },
    CheckoutPullRequest {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
        expected_local_head_oid: String,
        expected_content_revision: String,
    },
    MergePullRequest {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_base_oid: String,
        expected_state: String,
        expected_identity_revision: String,
        expected_readiness_revision: String,
        method: GithubMergeMethod,
        #[serde(default)]
        delete_branch: bool,
    },
    ClosePullRequest {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
    },
    ReopenPullRequest {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
    },
    SubmitPullRequestReview {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
        event: GithubPullRequestReviewEvent,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        comments: Vec<GithubReviewSubmissionComment>,
    },
    ReplyReviewThread {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
        thread_id: String,
        body: String,
    },
    ResolveReviewThread {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
        thread_id: String,
    },
    UnresolveReviewThread {
        expected_repository: GithubRepositoryIdentity,
        expected_viewer_login: String,
        number: u64,
        expected_head_oid: String,
        expected_state: String,
        thread_id: String,
    },
}

impl GithubAction {
    fn identity_expectation(&self) -> (&GithubRepositoryIdentity, &str) {
        match self {
            Self::CreatePullRequest {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::CheckoutPullRequest {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::MergePullRequest {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::ClosePullRequest {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::ReopenPullRequest {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::SubmitPullRequestReview {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::ReplyReviewThread {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::ResolveReviewThread {
                expected_repository,
                expected_viewer_login,
                ..
            }
            | Self::UnresolveReviewThread {
                expected_repository,
                expected_viewer_login,
                ..
            } => (expected_repository, expected_viewer_login),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GithubMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubActionResult {
    pub repository: Option<GithubRepository>,
    pub pull_request: Option<GithubPullRequestDetail>,
    pub snapshot: Option<GitWorkspaceSnapshot>,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitNetworkPolicy {
    #[default]
    Restricted,
    FullAccess,
}

#[derive(Clone)]
struct Repository {
    network_policy: GitNetworkPolicy,
    root: PathBuf,
    git_dir: PathBuf,
    git_common_dir: PathBuf,
    index_path: PathBuf,
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
struct NetworkGitConfiguration {
    ssh_command: String,
    proxies: Vec<String>,
    helpers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteTransport {
    configuration: Option<NetworkGitConfiguration>,
    proof: GitRemote,
    fetch_urls: Vec<String>,
    push_urls: Vec<String>,
    fetch_refspecs: Vec<String>,
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

#[derive(Clone)]
struct ResolvedGithub {
    repository: Repository,
    gh: Option<PathBuf>,
    selector: Option<String>,
    context: GithubRepositoryContext,
}

struct GithubPullRequestReadState {
    head_ref_oid: String,
    base_ref_oid: String,
    changed_files: u64,
}

struct GithubPullRequestFiles {
    files: Vec<GitFileChange>,
    selected: Option<GithubPullRequestFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GithubPullRequestFile {
    change: GitFileChange,
    changes: u64,
    patch: Option<String>,
}

struct GithubPullRequestFilesPage {
    files: Vec<GithubPullRequestFile>,
    has_more: bool,
}

struct GithubReviewThreadScope {
    repository_owner: String,
    repository_name: String,
    pull_request_number: u64,
    pull_request_state: String,
    head_ref_oid: String,
    viewer_can_reply: bool,
    viewer_can_resolve: bool,
    viewer_can_unresolve: bool,
}

enum GithubActionResponseKind {
    Standard,
    Merge,
    ReviewSubmission,
    ReviewThreadReply,
    ReviewThreadResolution { thread_id: String, resolved: bool },
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

fn default_history_limit() -> u16 {
    50
}

fn default_change_page_limit() -> u16 {
    DEFAULT_CHANGE_PAGE_LIMIT
}

fn default_github_review_threads_page_size() -> u16 {
    DEFAULT_GITHUB_REVIEW_THREADS_PAGE_SIZE
}

fn default_github_review_comments_page_size() -> u16 {
    DEFAULT_GITHUB_REVIEW_COMMENTS_PAGE_SIZE
}

fn validate_github_review_threads_page_size(page_size: u16) -> Result<(), String> {
    if page_size == 0 || page_size > MAX_GITHUB_REVIEW_THREADS_PAGE_SIZE {
        return Err(format!(
            "GitHub 审阅线程每页数量必须在 1 到 {MAX_GITHUB_REVIEW_THREADS_PAGE_SIZE} 之间"
        ));
    }
    Ok(())
}

fn validate_github_review_comments_page_size(page_size: u16) -> Result<(), String> {
    if page_size == 0 || page_size > MAX_GITHUB_REVIEW_COMMENTS_PAGE_SIZE {
        return Err(format!(
            "GitHub 审阅评论每页数量必须在 1 到 {MAX_GITHUB_REVIEW_COMMENTS_PAGE_SIZE} 之间"
        ));
    }
    Ok(())
}

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
    let summary = workspace_summary_from_snapshot(&snapshot);
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
    let expected_stage_all_target_revision = request
        .expected_stage_all_target_revision
        .as_deref()
        .map(|revision| validate_revision_token("Git 全部暂存目标修订", revision))
        .transpose()?;
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
    let mut stage_all_transaction = expected_stage_all_target_revision
        .as_ref()
        .map(|_| StageAllIndexTransaction::acquire(&repository))
        .transpose()?;
    let snapshot = snapshot_for_repository(&repository)?;
    let summary = workspace_summary_from_snapshot(&snapshot);
    if summary.summary_revision != expected_revision {
        return Ok(GitChangePageResult::Stale { summary });
    }

    let mut candidate_tree_oid = None;
    let mut files = if let (Some(expected_target), Some(transaction)) = (
        expected_stage_all_target_revision.as_deref(),
        stage_all_transaction.as_mut(),
    ) {
        validate_stage_all_snapshot(&repository, &snapshot)?;
        let candidate = transaction.build_candidate(&snapshot)?;
        validate_stage_all_candidate(&candidate.proof, expected_target)?;
        invoke_stage_all_test_hook(
            &repository,
            StageAllTestHookPoint::ChangePageBeforeCandidateRead,
        );
        let files = stage_all_candidate_changes(transaction, &snapshot, None)?;
        candidate_tree_oid = Some(candidate.proof.tree_oid.clone());
        invoke_stage_all_test_hook(
            &repository,
            StageAllTestHookPoint::ChangePageBeforeFinalProof,
        );
        let final_snapshot = snapshot_for_repository(&repository)?;
        let final_summary = workspace_summary_from_snapshot(&final_snapshot);
        if final_summary.summary_revision != expected_revision {
            return Ok(GitChangePageResult::Stale {
                summary: final_summary,
            });
        }
        validate_stage_all_snapshot(&repository, &final_snapshot)?;
        let final_candidate = transaction.build_candidate(&final_snapshot)?;
        validate_stage_all_candidate(&final_candidate.proof, expected_target)?;
        if final_candidate.proof != candidate.proof {
            return Err("Git 全部暂存候选在读取变更页期间发生变化；请重新加载".into());
        }
        files
    } else {
        snapshot.files.clone()
    };
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
    let cursor_revision = expected_stage_all_target_revision
        .as_deref()
        .unwrap_or(&expected_revision);
    let offset = request
        .cursor
        .as_deref()
        .map(|cursor| parse_change_cursor(cursor, cursor_revision, &query))
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
        (end < matched_total).then(|| encode_change_cursor(end, cursor_revision, &query));
    Ok(GitChangePageResult::Page {
        revision: expected_revision,
        files: page,
        matched_count,
        next_cursor,
        selection,
        stage_all_target_revision: expected_stage_all_target_revision,
        candidate_tree_oid,
    })
}

pub fn diff(workspace: &Path, request: GitDiffRequest) -> Result<GitDiffResponse, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(expected_target_revision) = match &request {
        GitDiffRequest::Working {
            expected_stage_all_target_revision,
            ..
        }
        | GitDiffRequest::Unstaged {
            expected_stage_all_target_revision,
            ..
        } => expected_stage_all_target_revision.as_deref(),
        GitDiffRequest::Staged { .. } | GitDiffRequest::Compare { .. } => None,
    } {
        return stage_all_proof_bound_diff(&repository, &request, expected_target_revision);
    }
    let (path, mode) = match &request {
        GitDiffRequest::Working { path, .. } => (path.as_deref(), "working"),
        GitDiffRequest::Staged { path } => (path.as_deref(), "staged"),
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
        OsString::from("--unified=3"),
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
        stage_all_target_revision: None,
        candidate_tree_oid: None,
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

pub fn history(workspace: &Path, request: GitHistoryRequest) -> Result<GitHistoryPage, String> {
    let limit = request.limit.clamp(1, MAX_HISTORY_LIMIT);
    let skip = request
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<u32>()
        .map_err(|_| "Git 历史 cursor 无效".to_owned())?;
    if skip > MAX_HISTORY_SKIP {
        return Err(format!("Git 历史 skip 不能超过 {MAX_HISTORY_SKIP}"));
    }
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !repository_has_head(&repository)? {
        return Ok(GitHistoryPage {
            commits: Vec::new(),
            next_cursor: None,
        });
    }
    let requested = u32::from(limit) + 1;
    let format = "format:%H%x00%h%x00%P%x00%an%x00%ae%x00%aI%x00%cI%x00%D%x00%s%x00%b%x00";
    let mut args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("log"),
        OsString::from("-z"),
        OsString::from(format!("--max-count={requested}")),
        OsString::from(format!("--skip={skip}")),
        OsString::from(format!("--format={format}")),
    ];
    if let Some(branch) = request.branch.as_deref() {
        args.push(OsString::from(resolve_commit(&repository, branch)?));
    }
    if let Some(path) = request.path.as_deref() {
        args.push(OsString::from("--"));
        args.push(OsString::from(validate_relative_path(path)?));
    }
    let output = run_git(
        &repository,
        args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
        true,
    )?;
    require_success("读取 Git 历史", &output)?;
    let mut commits = parse_history(&output.stdout)?;
    let has_more = commits.len() > usize::from(limit);
    commits.truncate(usize::from(limit));
    Ok(GitHistoryPage {
        commits,
        next_cursor: has_more.then(|| skip.saturating_add(u32::from(limit)).to_string()),
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

#[cfg(test)]
pub fn execute_action(workspace: &Path, action: GitAction) -> Result<GitActionResult, String> {
    execute_action_with_policy(workspace, action, GitNetworkPolicy::Restricted)
}

pub fn execute_action_with_policy(
    workspace: &Path,
    action: GitAction,
    policy: GitNetworkPolicy,
) -> Result<GitActionResult, String> {
    let mut repository = require_repository(workspace)?;
    repository.network_policy = policy;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if matches!(
        action,
        GitAction::Fetch { .. } | GitAction::Pull { .. } | GitAction::Push { .. }
    ) {
        validate_locked_repository_resolution(workspace, &repository)?;
    }
    let action = expand_rename_pathspecs_for_action(&repository, action)?;
    validate_bulk_action(&repository, &action)?;
    validate_expected_ref_state(&repository, &action)?;
    validate_submodule_action(&repository, &action)?;
    validate_action_during_repository_operation(&repository, &action)?;
    match &action {
        GitAction::Fetch {
            expected_repository_id,
            expected_worktree_id,
            remote,
        } => {
            return execute_fetch_action(
                &repository,
                expected_repository_id,
                expected_worktree_id,
                remote,
            );
        }
        GitAction::Pull {
            expected_repository_id,
            expected_worktree_id,
            expected_local_branch,
            expected_head,
            expected_content_revision,
            upstream,
            rebase,
            ff_only,
        } => {
            return execute_pull_action(
                &repository,
                expected_repository_id,
                expected_worktree_id,
                expected_local_branch,
                expected_head,
                expected_content_revision,
                upstream,
                *rebase,
                *ff_only,
            );
        }
        GitAction::Push {
            expected_repository_id,
            expected_worktree_id,
            remote,
            expected_local_branch,
            remote_branch,
            expected_head,
            expected_upstream,
            set_upstream,
            force_with_lease,
        } => {
            return execute_push_action(
                &repository,
                expected_repository_id,
                expected_worktree_id,
                remote,
                expected_local_branch,
                remote_branch,
                expected_head,
                expected_upstream.as_ref(),
                *set_upstream,
                *force_with_lease,
            );
        }
        _ => {}
    }
    if let GitAction::Commit {
        message,
        expected_target_revision,
        expected_tree_oid,
        amend,
    } = &action
    {
        return execute_commit_action(
            &repository,
            message,
            expected_target_revision,
            expected_tree_oid,
            *amend,
        );
    }
    let output = match action {
        GitAction::StageAll {
            expected_content_revision,
            expected_target_revision,
        } => execute_stage_all_transaction(
            &repository,
            &expected_content_revision,
            &expected_target_revision,
        )?,
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
        committed_oid: None,
    })
}

fn validate_locked_repository_resolution(
    workspace: &Path,
    locked: &Repository,
) -> Result<(), String> {
    let current = require_repository(workspace)?;
    if current.repository_id != locked.repository_id
        || current.worktree_id != locked.worktree_id
        || !same_path(&current.root, &locked.root)
        || !same_path(&current.git_dir, &locked.git_dir)
        || !same_path(&current.git_common_dir, &locked.git_common_dir)
        || !same_path(&current.index_path, &locked.index_path)
        || current.git != locked.git
    {
        return Err("Git 仓库或 worktree 已在取得操作锁后被替换；请刷新后重试".into());
    }
    Ok(())
}

fn validate_workspace_identity(
    repository: &Repository,
    expected_repository_id: &str,
    expected_worktree_id: &str,
) -> Result<(), String> {
    let expected_repository_id = validate_revision_token("Git 仓库身份", expected_repository_id)?;
    let expected_worktree_id = validate_revision_token("Git worktree 身份", expected_worktree_id)?;
    if repository.repository_id != expected_repository_id
        || repository.worktree_id != expected_worktree_id
    {
        return Err("Git 仓库或 worktree 已在确认后发生变化；请刷新后重试".into());
    }
    Ok(())
}

fn validate_remote_proof_shape(remote: &GitRemote) -> Result<(), String> {
    validate_remote_name_syntax(&remote.name, true)?;
    validate_revision_token("Git remote fetch proof", &remote.fetch_revision)?;
    validate_revision_token("Git remote push proof", &remote.push_revision)?;
    if remote.url.is_some() {
        return Err("Git remote proof 不接受 renderer 提供的 URL".into());
    }
    Ok(())
}

fn validate_remote_tracking_namespace(
    repository: &Repository,
    remote_name: &str,
) -> Result<(), String> {
    validate_remote_name_syntax(remote_name, false)?;
    let output = run_git(
        repository,
        [
            OsString::from("check-ref-format"),
            OsString::from(format!("refs/remotes/{remote_name}/mework-proof")),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        16 * 1024,
        true,
    )?;
    if output.success() {
        Ok(())
    } else {
        Err("Git remote 名称不能安全映射到 remote-tracking namespace".into())
    }
}

fn validate_upstream_proof_shape(
    repository: &Repository,
    upstream: &GitUpstream,
) -> Result<(), String> {
    validate_remote_name_syntax(&upstream.remote_name, true)?;
    validate_remote_proof_shape(&upstream.remote)?;
    if upstream.remote.name != upstream.remote_name
        || upstream.is_local != (upstream.remote_name == ".")
        || upstream.remote_branch.is_empty()
        || upstream.remote_branch.contains('\0')
        || upstream.merge_ref != format!("refs/heads/{}", upstream.remote_branch)
        || !upstream.tracking_ref.starts_with("refs/")
        || upstream.tracking_ref.contains('\0')
        || upstream.tracking_ref.starts_with('-')
    {
        return Err("Git upstream proof 结构无效".into());
    }
    validate_branch_name(repository, &upstream.remote_branch)?;
    if upstream.is_local {
        if upstream.tracking_ref != upstream.merge_ref {
            return Err("本地 Git upstream tracking ref 与 merge ref 不一致".into());
        }
    } else {
        validate_remote_tracking_namespace(repository, &upstream.remote_name)?;
        if upstream.tracking_ref
            != format!(
                "refs/remotes/{}/{}",
                upstream.remote_name, upstream.remote_branch
            )
        {
            return Err("Git upstream tracking ref 不在受控 remote-tracking namespace".into());
        }
    }
    if let Some(oid) = upstream.tracking_oid.as_ref() {
        validate_object_id("Git upstream tracking commit", oid.clone())?;
    }
    Ok(())
}

fn validate_expected_upstream(
    repository: &Repository,
    actual: Option<&GitUpstream>,
    expected: Option<&GitUpstream>,
) -> Result<(), String> {
    if let Some(expected) = expected {
        validate_upstream_proof_shape(repository, expected)?;
    }
    if actual != expected {
        return Err("Git upstream 已在确认后发生变化；请刷新后重试".into());
    }
    Ok(())
}

fn verified_remote_transport(
    repository: &Repository,
    expected: &GitRemote,
    for_fetch: bool,
) -> Result<RemoteTransport, String> {
    validate_remote_proof_shape(expected)?;
    if expected.name == "." {
        return Err("本地 remote '.' 不能用于网络 Git 操作".into());
    }
    validate_remote_tracking_namespace(repository, &expected.name)?;
    let mut transport = remote_transport(repository, &expected.name)?;
    if transport.proof != *expected {
        return Err("Git remote transport proof 已在确认后发生变化；请刷新后重试".into());
    }
    let locators = if for_fetch {
        &transport.fetch_urls
    } else {
        &transport.push_urls
    };
    if !for_fetch && locators.len() != 1 {
        return Err("Git push remote 必须恰好包含一个已验证的 push locator".into());
    }
    for locator in locators {
        validate_transport_locator(locator)?;
    }
    let configuration = validate_network_git_configuration(repository)?;
    if locators
        .iter()
        .any(|locator| transport_locator_uses_ssh(locator))
        && configuration.ssh_command.is_empty()
    {
        return Err("当前 Git 安装没有可绑定的可信 SSH 可执行文件；已拒绝 SSH transport".into());
    }
    transport.configuration = Some(configuration);
    Ok(transport)
}

fn validate_network_git_configuration(
    repository: &Repository,
) -> Result<NetworkGitConfiguration, String> {
    let ssh_commands = network_config_values(repository, "core.sshCommand")?;
    let proxies = network_config_values(repository, "core.gitProxy")?;
    let helpers = trusted_credential_helpers(repository)?;
    Ok(NetworkGitConfiguration {
        ssh_command: if repository.network_policy == GitNetworkPolicy::FullAccess {
            ssh_commands.last().cloned()
        } else {
            None
        }.or_else(|| safe_git_ssh_command(repository)).unwrap_or_default(),
        proxies: if repository.network_policy == GitNetworkPolicy::FullAccess {
            proxies
        } else {
            Vec::new()
        },
        helpers,
    })
}

fn trusted_credential_helpers(repository: &Repository) -> Result<Vec<String>, String> {
    network_config_values(repository, "credential.helper")
}

fn network_config_values(repository: &Repository, key: &str) -> Result<Vec<String>, String> {
    let output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--show-scope"),
            OsString::from("--show-origin"),
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
        return Err(format!("无法验证 Git {key} 来源"));
    }
    if output.stdout_truncated {
        return Err(format!("Git {key} 配置超过安全上限"));
    }
    parse_network_config_values(&output.stdout, key, repository.network_policy)
}

#[cfg(test)]
fn parse_trusted_credential_helpers(bytes: &[u8]) -> Result<Vec<String>, String> {
    parse_network_config_values(bytes, "credential.helper", GitNetworkPolicy::Restricted)
}

fn parse_network_config_values(
    bytes: &[u8],
    key: &str,
    policy: GitNetworkPolicy,
) -> Result<Vec<String>, String> {
    let mut fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    if fields.len() % 3 != 0 || fields.len() / 3 > 32 {
        return Err("Git credential.helper 来源响应无效".into());
    }
    let mut helpers = Vec::new();
    for record in fields.chunks_exact(3) {
        let scope = std::str::from_utf8(record[0])
            .map_err(|_| "Git credential.helper scope 无效".to_owned())?;
        let _origin = std::str::from_utf8(record[1])
            .map_err(|_| "Git credential.helper origin 无效".to_owned())?;
        let helper = std::str::from_utf8(record[2])
            .map_err(|_| "Git credential.helper 值无效".to_owned())?;
        if helper.as_bytes().len() > MAX_GIT_REMOTE_VALUE_BYTES
            || helper
                .chars()
                .any(|character| character == '\0' || character == '\r' || character == '\n')
        {
            return Err("Git credential.helper 值无效或超过安全上限".into());
        }
        if !matches!(scope, "system" | "global" | "local" | "worktree") {
            return Err(format!("Git {key} 来自未经允许的配置 scope"));
        }
        // Log only a fixed key, validated source scope and host policy. Values
        // and origin paths may contain credentials and never enter the audit.
        eprintln!("[git-transport-config] key={key} source={scope} policy={policy:?}");
        if key != "credential.helper" {
            if policy == GitNetworkPolicy::Restricted && !helper.trim().is_empty() {
                return Err(format!("当前仓库配置了 {key}；内置网络 Git 操作已拒绝"));
            }
            helpers.push(helper.to_owned());
            continue;
        }
        if policy == GitNetworkPolicy::FullAccess {
            if helper.is_empty() {
                helpers.clear();
            } else {
                helpers.push(helper.to_owned());
            }
            continue;
        }
        match scope {
            "system" | "global" => {
                if helper.is_empty() {
                    helpers.clear();
                } else {
                    if !matches!(
                        helper,
                        "manager"
                            | "manager-core"
                            | "wincred"
                            | "osxkeychain"
                            | "libsecret"
                            | "cache"
                            | "store"
                    ) {
                        return Err(
                            "system/global credential.helper 不在内置网络 Git 的可信允许列表"
                                .into(),
                        );
                    }
                    helpers.push(helper.to_owned());
                }
            }
            "local" | "worktree" if helper.is_empty() => helpers.clear(),
            "local" | "worktree" => {
                return Err(
                    "仓库或 worktree 配置了 credential.helper；内置网络 Git 操作已拒绝".into(),
                )
            }
            _ => {
                return Err(
                    "Git credential.helper 来自未经允许的配置 scope；内置网络操作已拒绝".into(),
                )
            }
        }
    }
    Ok(helpers)
}

fn sanitized_transport_output(output: &CliOutput, transport: &RemoteTransport) -> String {
    // Git may normalize, abbreviate, or otherwise transform a locator before
    // echoing it. Exact string replacement cannot prove that embedded
    // credentials are gone, so no network transport output crosses IPC.
    let _ = (output, transport);
    String::new()
}

fn require_transport_success(
    label: &str,
    output: &CliOutput,
    transport: &RemoteTransport,
) -> Result<(), String> {
    if output.success() {
        return Ok(());
    }
    let _ = transport;
    Err(if output.timed_out {
        format!("{label}超时")
    } else {
        match output.exit_code() {
            Some(code) => format!("{label}失败（退出码 {code}）"),
            None => format!("{label}失败"),
        }
    })
}

fn execute_fetch_action(
    repository: &Repository,
    expected_repository_id: &str,
    expected_worktree_id: &str,
    expected_remote: &GitRemote,
) -> Result<GitActionResult, String> {
    validate_workspace_identity(repository, expected_repository_id, expected_worktree_id)?;
    let transport = verified_remote_transport(repository, expected_remote, true)?;
    let safe_refspec = format!("+refs/heads/*:refs/remotes/{}/*", transport.proof.name);
    let output = run_git_with_verified_transport(
        repository,
        &transport,
        true,
        [
            OsString::from("fetch"),
            OsString::from("--no-tags"),
            OsString::from("--prune"),
            OsString::from("--upload-pack=git-upload-pack"),
            OsString::from(internal_verified_remote_name()),
            OsString::from(safe_refspec),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    require_transport_success("执行 Git fetch", &output, &transport)?;
    let current_transport =
        remote_transport(repository, &transport.proof.name).map_err(|error| {
            format!(
                "Git fetch 已成功，但无法重新验证 remote：{error}；请刷新后核对 partial outcome"
            )
        })?;
    if current_transport.proof != transport.proof {
        return Err(
            "Git fetch 已成功，但 remote 配置在执行期间发生变化；请刷新后核对 partial outcome"
                .into(),
        );
    }
    let message = sanitized_transport_output(&output, &transport);
    Ok(GitActionResult {
        message: (!message.is_empty()).then_some(message),
        snapshot: Some(bounded_workspace_snapshot(snapshot_for_repository(
            repository,
        )?)),
        committed_oid: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_pull_action(
    repository: &Repository,
    expected_repository_id: &str,
    expected_worktree_id: &str,
    expected_local_branch: &str,
    expected_head: &str,
    expected_content_revision: &str,
    expected_upstream: &GitUpstream,
    rebase: bool,
    ff_only: bool,
) -> Result<GitActionResult, String> {
    if rebase {
        return Err("当前后端只允许 fast-forward pull，不执行自动 rebase".into());
    }
    if !ff_only {
        return Err("Git pull 必须明确启用 ffOnly".into());
    }
    validate_workspace_identity(repository, expected_repository_id, expected_worktree_id)?;
    validate_upstream_proof_shape(repository, expected_upstream)?;
    if expected_upstream.is_local {
        return Err("本地 remote '.' 不能用于 Git pull".into());
    }
    let expected_head = validate_object_id("仓库 HEAD", expected_head.to_owned())?;
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let snapshot = snapshot_for_repository(repository)?;
    if !snapshot.is_clean {
        return Err("Git pull 要求工作树干净；请先提交或储藏当前变更".into());
    }
    if snapshot.operation.is_some() {
        return Err("仓库正在执行 Git 操作；已拒绝 pull".into());
    }
    if snapshot.head.as_deref() != Some(expected_head.as_str())
        || snapshot.content_revision != expected_content_revision
    {
        return Err("Git 工作树或 HEAD 已在确认后发生变化；请刷新后重试".into());
    }
    validate_expected_upstream(
        repository,
        snapshot.upstream_target.as_ref(),
        Some(expected_upstream),
    )?;
    let branch = snapshot
        .branch
        .as_deref()
        .ok_or_else(|| "当前 Git HEAD 已分离，不能 pull".to_owned())?;
    if branch != expected_local_branch {
        return Err("当前本地 Git 分支已在确认后发生变化；请刷新后重试".into());
    }
    let transport = verified_remote_transport(repository, &expected_upstream.remote, true)?;
    let fetch_refspec = format!(
        "+{}:{}",
        expected_upstream.merge_ref, expected_upstream.tracking_ref
    );
    let fetch_output = run_git_with_verified_transport(
        repository,
        &transport,
        true,
        [
            OsString::from("fetch"),
            OsString::from("--no-tags"),
            OsString::from("--upload-pack=git-upload-pack"),
            OsString::from(internal_verified_remote_name()),
            OsString::from(fetch_refspec),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    require_transport_success("执行 Git pull 的 fetch 阶段", &fetch_output, &transport)?;

    let current_transport =
        remote_transport(repository, &transport.proof.name).map_err(|error| {
            format!("Git fetch 已成功，但重新验证目标 remote 失败：{error}；请核对 partial outcome")
        })?;
    let (_, current_upstream, local_oid) =
        upstream_target_for_branch(repository, branch, std::slice::from_ref(&current_transport))
            .map_err(|error| {
                format!(
                    "Git fetch 已成功，但重新验证 upstream 失败：{error}；请核对 partial outcome"
                )
            })?;
    let current_upstream = current_upstream.ok_or_else(|| {
        "Git fetch 已成功，但 upstream 已被移除；请核对 partial outcome".to_owned()
    })?;
    if !upstream_route_matches(&current_upstream, expected_upstream)
        || local_oid != expected_head
        || current_transport.proof != transport.proof
    {
        return Err(
            "Git fetch 已成功，但 remote、upstream 或 HEAD 已在执行期间发生变化；请核对 partial outcome"
                .into(),
        );
    }
    let fetched_oid = current_upstream.tracking_oid.clone().ok_or_else(|| {
        "Git fetch 已成功，但 tracking ref 不存在；请核对 partial outcome".to_owned()
    })?;
    let before_merge = snapshot_for_repository(repository).map_err(|error| {
        format!("Git fetch 已成功，但无法重新读取工作树：{error}；请核对 partial outcome")
    })?;
    if !before_merge.is_clean
        || before_merge.operation.is_some()
        || before_merge.branch.as_deref() != Some(expected_local_branch)
        || before_merge.head.as_deref() != Some(expected_head.as_str())
        || before_merge.content_revision != expected_content_revision
    {
        return Err(
            "Git fetch 已成功，但工作树或 HEAD 已在合并前发生变化；请核对 partial outcome".into(),
        );
    }
    let merge_output = run_git_with_disabled_hooks(
        repository,
        [
            OsString::from("merge"),
            OsString::from("--ff-only"),
            OsString::from("--no-edit"),
            OsString::from(&fetched_oid),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    if !merge_output.success() {
        return Err(format!(
            "Git fetch 已成功，但固定 tracking commit 的 fast-forward 合并失败：{}；请核对 partial outcome",
            redact_sensitive_text(&merge_output.display_output())
        ));
    }
    let mut messages = Vec::new();
    let fetch_message = sanitized_transport_output(&fetch_output, &transport);
    if !fetch_message.is_empty() {
        messages.push(fetch_message);
    }
    let merge_message = redact_sensitive_text(&merge_output.display_output());
    if !merge_message.is_empty() {
        messages.push(merge_message);
    }
    Ok(GitActionResult {
        message: (!messages.is_empty()).then(|| messages.join("\n")),
        snapshot: Some(bounded_workspace_snapshot(snapshot_for_repository(
            repository,
        )?)),
        committed_oid: None,
    })
}

fn upstream_route_matches(actual: &GitUpstream, expected: &GitUpstream) -> bool {
    actual.remote_name == expected.remote_name
        && actual.remote_branch == expected.remote_branch
        && actual.merge_ref == expected.merge_ref
        && actual.tracking_ref == expected.tracking_ref
        && actual.is_local == expected.is_local
        && actual.remote == expected.remote
}

fn execute_push_action(
    repository: &Repository,
    expected_repository_id: &str,
    expected_worktree_id: &str,
    expected_remote: &GitRemote,
    expected_local_branch: &str,
    branch: &str,
    expected_head: &str,
    expected_upstream: Option<&GitUpstream>,
    set_upstream: bool,
    force_with_lease: bool,
) -> Result<GitActionResult, String> {
    if force_with_lease {
        return Err("当前后端不允许 force push".into());
    }
    validate_workspace_identity(repository, expected_repository_id, expected_worktree_id)?;
    let expected_head = validate_object_id("仓库 HEAD", expected_head.to_owned())?;
    validate_branch_name(repository, branch)?;
    let local_branch = current_branch_name(repository)?;
    if local_branch != expected_local_branch {
        return Err("当前本地 Git 分支已在确认后发生变化；请刷新后重试".into());
    }
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.head.as_deref() != Some(expected_head.as_str()) {
        return Err("仓库 HEAD 已在确认后发生变化；请刷新后重试".into());
    }
    validate_expected_upstream(
        repository,
        snapshot.upstream_target.as_ref(),
        expected_upstream,
    )?;
    if set_upstream && expected_upstream.is_some() {
        return Err("已有 Git upstream 的分支不能通过普通 push 重新绑定 upstream".into());
    }
    let transport = verified_remote_transport(repository, expected_remote, false)?;

    let output = run_git_with_verified_transport(
        repository,
        &transport,
        false,
        [
            OsString::from("push"),
            OsString::from("--no-verify"),
            OsString::from("--receive-pack=git-receive-pack"),
            OsString::from(internal_verified_remote_name()),
            OsString::from(format!("{expected_head}:refs/heads/{branch}")),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    require_transport_success("执行 Git 推送", &output, &transport)?;
    let mut messages = Vec::new();
    let push_message = sanitized_transport_output(&output, &transport);
    if !push_message.is_empty() {
        messages.push(push_message);
    }

    if set_upstream {
        if let Err(error) = set_branch_upstream_after_push(
            repository,
            &local_branch,
            &transport,
            branch,
            &expected_head,
        ) {
            messages.push(format!(
                "远端推送已成功，但设置 Git upstream 未完成：{error}"
            ));
        }
    }
    let current_transport =
        remote_transport(repository, &transport.proof.name).map_err(|error| {
            format!("远端推送已成功，但无法重新验证 remote：{error}；请刷新后核对 partial outcome")
        })?;
    if current_transport.proof != transport.proof {
        return Err(
            "远端推送已成功，但 Git remote 配置在执行期间发生变化；请刷新后核对 partial outcome"
                .into(),
        );
    }

    let snapshot = bounded_workspace_snapshot(snapshot_for_repository(repository)?);
    let message = messages.join("\n");
    Ok(GitActionResult {
        message: (!message.is_empty()).then_some(message),
        snapshot: Some(snapshot),
        committed_oid: None,
    })
}

fn set_branch_upstream_after_push(
    repository: &Repository,
    local_branch: &str,
    transport: &RemoteTransport,
    remote_branch: &str,
    expected_head: &str,
) -> Result<(), String> {
    if remote_transport(repository, &transport.proof.name)?.proof != transport.proof {
        return Err("remote 配置已在推送后变化".into());
    }
    let remote_key = format!("branch.{local_branch}.remote");
    let merge_key = format!("branch.{local_branch}.merge");
    let remote_output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--replace-all"),
            OsString::from(&remote_key),
            OsString::from(&transport.proof.name),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    require_success("设置 Git upstream remote", &remote_output)?;
    let merge_ref = format!("refs/heads/{remote_branch}");
    let merge_output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--replace-all"),
            OsString::from(&merge_key),
            OsString::from(&merge_ref),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    require_success("设置 Git upstream merge ref", &merge_output)?;
    let atoms = read_upstream_atoms(repository, local_branch)?
        .ok_or_else(|| "Git 未能解析刚设置的 upstream".to_owned())?;
    if atoms.remote_name != transport.proof.name || atoms.merge_ref != merge_ref {
        return Err("Git 刚设置的 upstream 与推送目标不一致".into());
    }
    let transaction = format!(
        "start\nverify HEAD {expected_head}\nupdate {} {expected_head}\nprepare\ncommit\n",
        atoms.tracking_ref
    );
    let update_output = run_git_with_disabled_hooks(
        repository,
        [OsString::from("update-ref"), OsString::from("--stdin")],
        Some(transaction.into_bytes()),
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    require_success("记录 Git upstream tracking commit", &update_output)
}

fn execute_commit_action(
    repository: &Repository,
    message: &str,
    expected_target_revision: &str,
    expected_tree_oid: &str,
    amend: bool,
) -> Result<GitActionResult, String> {
    if amend {
        return Err("当前后端暂不允许 amend；请创建普通提交".into());
    }
    let expected_target_revision =
        validate_revision_token("Git 提交目标修订", expected_target_revision)?;
    let expected_tree_oid = validate_object_id("Git 提交候选树", expected_tree_oid.to_owned())?;
    let normalized_message = normalize_commit_message(repository, message.as_bytes())?;
    let message_digest = commit_message_digest(&normalized_message);
    let mut transaction = StageAllIndexTransaction::acquire(repository)?;
    let snapshot = snapshot_for_repository(repository)?;
    validate_commit_snapshot(&snapshot)?;
    let head = resolve_commit_head_state(repository, &snapshot)?;
    let identity = resolve_commit_identity(repository)?;
    let sign_commit = commit_signing_enabled(repository)?;
    let confirmed =
        transaction.build_commit_candidate(&head, &identity, &message_digest, sign_commit)?;
    validate_commit_candidate(&confirmed, &expected_target_revision, &expected_tree_oid)?;

    run_commit_hook(repository, &transaction.lock_path, "pre-commit", &[])?;
    let after_pre_commit =
        transaction.read_commit_candidate(&head, &identity, &message_digest, sign_commit)?;
    if after_pre_commit != confirmed {
        return Err(
            "pre-commit hook 改变了已确认的索引候选；真实 index 和引用保持不变，请审阅变更后重新提交"
                .into(),
        );
    }

    let message_file = CommitMessageFile::create(repository, &normalized_message)?;
    run_commit_hook(
        repository,
        &transaction.lock_path,
        "prepare-commit-msg",
        &[
            message_file.path.as_os_str().to_os_string(),
            OsString::from("message"),
        ],
    )?;
    run_commit_hook(
        repository,
        &transaction.lock_path,
        "commit-msg",
        &[message_file.path.as_os_str().to_os_string()],
    )?;
    let hook_message = message_file.read()?;
    let final_message = normalize_commit_message(repository, &hook_message)?;
    if final_message != normalized_message {
        return Err(
            "提交信息 hook 改变了已确认的提交说明；引用保持不变，请审阅 hook 输出后重新确认".into(),
        );
    }

    let after_hooks =
        transaction.read_commit_candidate(&head, &identity, &message_digest, sign_commit)?;
    if after_hooks != confirmed {
        return Err(
            "提交 hook 改变了已确认的索引候选；真实 index 和引用保持不变，请审阅变更后重新提交"
                .into(),
        );
    }
    let current_snapshot = snapshot_for_repository(repository)?;
    let current_head = resolve_commit_head_state(repository, &current_snapshot)?;
    if current_head != head {
        return Err("Git HEAD 或目标分支已在确认后变化；引用保持不变，请刷新后重试".into());
    }
    if resolve_commit_identity(repository)? != identity {
        return Err("Git 提交作者或提交者身份已在确认后变化；引用保持不变，请重新确认".into());
    }
    if commit_signing_enabled(repository)? != sign_commit {
        return Err("Git 提交签名设置已在确认后变化；引用保持不变，请重新确认".into());
    }
    transaction.verify_real_index_unchanged()?;

    let mut commit_args = vec![
        OsString::from("commit-tree"),
        OsString::from(&confirmed.tree_oid),
    ];
    if let Some(parent) = confirmed.head.oid.as_deref() {
        commit_args.extend([OsString::from("-p"), OsString::from(parent)]);
    }
    if confirmed.sign_commit {
        commit_args.push(OsString::from("-S"));
    }
    commit_args.extend([OsString::from("-F"), OsString::from("-")]);
    let created = run_git(
        repository,
        commit_args,
        Some(final_message.clone()),
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    require_success("创建 Git 提交对象", &created)?;
    if created.stdout_truncated {
        return Err("Git 提交对象响应超过安全上限；引用未更新".into());
    }
    let committed_oid = validate_object_id(
        "新 Git 提交",
        String::from_utf8_lossy(&created.stdout).trim().to_owned(),
    )?;
    validate_created_commit(
        repository,
        &committed_oid,
        &confirmed.tree_oid,
        confirmed.head.oid.as_deref(),
        &confirmed.identity,
        confirmed.sign_commit,
        &final_message,
    )?;

    invoke_stage_all_test_hook(repository, StageAllTestHookPoint::CommitBeforeRefUpdate);
    let final_snapshot = snapshot_for_repository(repository)?;
    let final_head = resolve_commit_head_state(repository, &final_snapshot)?;
    if final_head != head {
        return Err("Git HEAD 或目标分支在提交对象创建后发生变化；引用未更新，请刷新后重试".into());
    }
    let final_candidate =
        transaction.read_commit_candidate(&head, &identity, &message_digest, sign_commit)?;
    if final_candidate != confirmed {
        return Err(
            "Git 索引候选在引用更新前发生变化；真实 index 和引用保持不变，请重新确认".into(),
        );
    }

    let subject = commit_reflog_subject(&final_message);
    let reflog_message = if head.oid.is_some() {
        format!("commit: {subject}")
    } else {
        format!("commit (initial): {subject}")
    };
    let update_input = commit_ref_transaction_input(&head, &committed_oid)?;
    let updated = run_git(
        repository,
        [
            OsString::from("update-ref"),
            OsString::from("-m"),
            OsString::from(reflog_message),
            OsString::from("--stdin"),
        ],
        Some(update_input),
        LOCAL_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
        false,
    )?;
    if !updated.success() {
        return Err(format!(
            "Git 提交对象已创建，但目标引用未更新；{}",
            command_error("原子更新 Git 提交引用", &updated)
        ));
    }

    drop(transaction);
    drop(message_file);
    let mut notices = vec![format!(
        "已创建提交 {}",
        committed_oid.chars().take(12).collect::<String>()
    )];
    match run_commit_hook_output(repository, None, "post-commit", &[]) {
        Ok(output) if !output.success() => notices.push(format!(
            "提交已完成，但 post-commit hook 失败：{}",
            output.display_output()
        )),
        Err(error) => notices.push(format!("提交已完成，但无法运行 post-commit hook：{error}")),
        _ => {}
    }
    let snapshot = match snapshot_for_repository(repository) {
        Ok(snapshot) => Some(bounded_workspace_snapshot(snapshot)),
        Err(error) => {
            notices.push(format!("提交已完成，但刷新仓库状态失败：{error}"));
            None
        }
    };
    Ok(GitActionResult {
        message: Some(notices.join("\n")),
        snapshot,
        committed_oid: Some(committed_oid),
    })
}

fn validate_commit_candidate(
    candidate: &CommitCandidateProof,
    expected_target_revision: &str,
    expected_tree_oid: &str,
) -> Result<(), String> {
    if candidate.target_revision != expected_target_revision
        || candidate.tree_oid != expected_tree_oid
    {
        return Err(
            "已暂存内容、HEAD、提交说明或签名设置已在确认后变化；引用保持不变，请重新准备提交"
                .into(),
        );
    }
    Ok(())
}

fn validate_created_commit(
    repository: &Repository,
    committed_oid: &str,
    expected_tree_oid: &str,
    expected_parent: Option<&str>,
    expected_identity: &CommitIdentityState,
    expected_signed: bool,
    expected_message: &[u8],
) -> Result<(), String> {
    let output = run_git(
        repository,
        [
            OsString::from("cat-file"),
            OsString::from("commit"),
            OsString::from(committed_oid),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_COMMIT_MESSAGE_BYTES.saturating_add(64 * 1024),
        true,
    )?;
    require_success("复核新 Git 提交对象", &output)?;
    if output.stdout_truncated {
        return Err("新 Git 提交对象超过安全复核上限；引用未更新".into());
    }
    let separator = output
        .stdout
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or_else(|| "新 Git 提交对象缺少消息分隔符；引用未更新".to_owned())?;
    let headers = &output.stdout[..separator];
    let message = &output.stdout[separator + 2..];
    let mut actual_tree = None;
    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;
    let mut signed = false;
    for line in headers.split(|byte| *byte == b'\n') {
        if let Some(value) = line.strip_prefix(b"tree ") {
            actual_tree = Some(String::from_utf8_lossy(value).into_owned());
        } else if let Some(value) = line.strip_prefix(b"parent ") {
            parents.push(String::from_utf8_lossy(value).into_owned());
        } else if let Some(value) = line.strip_prefix(b"author ") {
            author = Some(stable_commit_identity(value)?);
        } else if let Some(value) = line.strip_prefix(b"committer ") {
            committer = Some(stable_commit_identity(value)?);
        } else if line.starts_with(b"gpgsig ") || line.starts_with(b"gpgsig-sha256 ") {
            signed = true;
        }
    }
    if actual_tree.as_deref() != Some(expected_tree_oid) {
        return Err("新 Git 提交对象的树与已确认候选不一致；引用未更新".into());
    }
    let expected_parents = expected_parent.into_iter().collect::<Vec<_>>();
    if parents.iter().map(String::as_str).collect::<Vec<_>>() != expected_parents {
        return Err("新 Git 提交对象的父提交与已确认 HEAD 不一致；引用未更新".into());
    }
    if author.as_deref() != Some(expected_identity.author.as_str())
        || committer.as_deref() != Some(expected_identity.committer.as_str())
    {
        return Err("新 Git 提交对象的作者或提交者身份与已确认设置不一致；引用未更新".into());
    }
    if signed != expected_signed {
        return Err("新 Git 提交对象的签名状态与已确认设置不一致；引用未更新".into());
    }
    if message != expected_message {
        return Err("新 Git 提交对象的提交说明与已确认内容不一致；引用未更新".into());
    }
    Ok(())
}

fn normalize_commit_message(repository: &Repository, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_COMMIT_MESSAGE_BYTES {
        return Err(format!(
            "提交信息不能超过 {MAX_COMMIT_MESSAGE_CHARS} 个字符"
        ));
    }
    if bytes.contains(&0) {
        return Err("提交信息不能包含 NUL 字符".into());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| "提交信息必须是有效的 UTF-8 文本".to_owned())?;
    if text.chars().count() > MAX_COMMIT_MESSAGE_CHARS {
        return Err(format!(
            "提交信息不能超过 {MAX_COMMIT_MESSAGE_CHARS} 个字符"
        ));
    }
    let output = run_git(
        repository,
        [
            OsString::from("stripspace"),
            OsString::from("--strip-comments"),
        ],
        Some(bytes.to_vec()),
        LOCAL_COMMAND_TIMEOUT,
        MAX_COMMIT_MESSAGE_BYTES,
        true,
    )?;
    require_success("规范化 Git 提交信息", &output)?;
    if output.stdout_truncated {
        return Err("规范化后的 Git 提交信息超过安全上限".into());
    }
    let normalized = output.stdout;
    let text = std::str::from_utf8(&normalized)
        .map_err(|_| "规范化后的提交信息不是有效 UTF-8".to_owned())?;
    if text.trim().is_empty() {
        return Err("提交信息不能为空".into());
    }
    if text.chars().count() > MAX_COMMIT_MESSAGE_CHARS {
        return Err(format!(
            "提交信息不能超过 {MAX_COMMIT_MESSAGE_CHARS} 个字符"
        ));
    }
    Ok(normalized)
}

fn commit_message_digest(message: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mework.git.commit-message.v1\0");
    update_revision_component(&mut digest, b"message", message);
    format!("{:x}", digest.finalize())
}

fn commit_signing_enabled(repository: &Repository) -> Result<bool, String> {
    let output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--type=bool"),
            OsString::from("--get"),
            OsString::from("commit.gpgSign"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        true,
    )?;
    if output.success() {
        return match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err("Git commit.gpgSign 配置不是有效布尔值".into()),
        };
    }
    if output.exit_code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        Ok(false)
    } else {
        Err(command_error("读取 Git 提交签名设置", &output))
    }
}

fn resolve_commit_identity(repository: &Repository) -> Result<CommitIdentityState, String> {
    fn resolve(repository: &Repository, variable: &str, label: &str) -> Result<String, String> {
        let output = run_git(
            repository,
            [OsString::from("var"), OsString::from(variable)],
            None,
            LOCAL_COMMAND_TIMEOUT,
            16 * 1024,
            true,
        )?;
        require_success(label, &output)?;
        if output.stdout_truncated {
            return Err(format!("{label}响应超过安全上限"));
        }
        stable_commit_identity(&output.stdout)
    }
    Ok(CommitIdentityState {
        author: resolve(repository, "GIT_AUTHOR_IDENT", "读取 Git 作者身份")?,
        committer: resolve(repository, "GIT_COMMITTER_IDENT", "读取 Git 提交者身份")?,
    })
}

fn stable_commit_identity(bytes: &[u8]) -> Result<String, String> {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let bytes = &bytes[start..end];
    let closing = bytes
        .iter()
        .rposition(|byte| *byte == b'>')
        .ok_or_else(|| "Git 身份缺少邮箱终止符".to_owned())?;
    let identity = &bytes[..=closing];
    if !identity.contains(&b'<') || identity.first() == Some(&b'<') {
        return Err("Git 身份缺少有效名称或邮箱".into());
    }
    String::from_utf8(identity.to_vec()).map_err(|_| "Git 身份必须是有效 UTF-8".to_owned())
}

fn resolve_commit_head_state(
    repository: &Repository,
    snapshot: &GitWorkspaceSnapshot,
) -> Result<CommitHeadState, String> {
    let oid = snapshot
        .head
        .as_ref()
        .map(|oid| validate_object_id("Git HEAD", oid.clone()))
        .transpose()?;
    if snapshot.unborn != oid.is_none() {
        return Err("Git HEAD 的 unborn 状态不一致；请刷新仓库后重试".into());
    }
    let symbolic = run_git(
        repository,
        [
            OsString::from("symbolic-ref"),
            OsString::from("--quiet"),
            OsString::from("HEAD"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        true,
    )?;
    let target_ref = if symbolic.success() {
        if symbolic.stdout_truncated {
            return Err("Git HEAD 引用响应超过安全上限".into());
        }
        let target = String::from_utf8_lossy(&symbolic.stdout).trim().to_owned();
        validate_commit_target_ref(repository, &target)?;
        Some(target)
    } else if symbolic.exit_code() == Some(1)
        && symbolic.stdout.is_empty()
        && symbolic.stderr.is_empty()
    {
        None
    } else {
        return Err(command_error("读取 Git HEAD 引用", &symbolic));
    };
    if oid.is_none() && target_ref.is_none() {
        return Err("未出生的 Git HEAD 必须指向本地分支".into());
    }
    Ok(CommitHeadState { oid, target_ref })
}

fn validate_commit_target_ref(repository: &Repository, target: &str) -> Result<(), String> {
    if !target.starts_with("refs/heads/") {
        return Err("Git HEAD 未指向受支持的本地分支".into());
    }
    let output = run_git(
        repository,
        [OsString::from("check-ref-format"), OsString::from(target)],
        None,
        LOCAL_COMMAND_TIMEOUT,
        4096,
        true,
    )?;
    require_success("验证 Git HEAD 目标分支", &output)
}

fn commit_ref_transaction_input(
    head: &CommitHeadState,
    committed_oid: &str,
) -> Result<Vec<u8>, String> {
    let command = match (head.target_ref.as_deref(), head.oid.as_deref()) {
        (Some(target), Some(old)) => format!("update {target} {committed_oid} {old}\n"),
        (Some(target), None) => format!("create {target} {committed_oid}\n"),
        (None, Some(old)) => format!("option no-deref\nupdate HEAD {committed_oid} {old}\n"),
        (None, None) => return Err("Git 提交缺少可更新的 HEAD 目标".into()),
    };
    Ok(format!("start\n{command}prepare\ncommit\n").into_bytes())
}

fn commit_reflog_subject(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("commit")
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect::<String>()
}

fn run_commit_hook(
    repository: &Repository,
    index_path: &Path,
    hook: &str,
    arguments: &[OsString],
) -> Result<(), String> {
    let output = run_commit_hook_output(repository, Some(index_path), hook, arguments)?;
    if output.success() {
        Ok(())
    } else {
        Err(command_error(&format!("运行 Git {hook} hook"), &output))
    }
}

fn run_commit_hook_output(
    repository: &Repository,
    index_path: Option<&Path>,
    hook: &str,
    arguments: &[OsString],
) -> Result<CliOutput, String> {
    let mut args = vec![
        OsString::from("hook"),
        OsString::from("run"),
        OsString::from("--ignore-missing"),
        OsString::from(hook),
    ];
    if !arguments.is_empty() {
        args.push(OsString::from("--"));
        args.extend(arguments.iter().cloned());
    }
    match index_path {
        Some(index_path) => run_git_with_internal_index(
            repository,
            index_path,
            args,
            None,
            LOCAL_COMMAND_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        ),
        None => run_git(
            repository,
            args,
            None,
            LOCAL_COMMAND_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        ),
    }
}

struct CommitMessageFile {
    path: PathBuf,
}

impl CommitMessageFile {
    fn create(repository: &Repository, bytes: &[u8]) -> Result<Self, String> {
        for attempt in 0..32_u32 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = repository.git_dir.join(format!(
                ".MEWORK_COMMIT_EDITMSG-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            apply_commit_message_nofollow_options(&mut options);
            match options.open(&path) {
                Ok(mut file) => {
                    let metadata = file
                        .metadata()
                        .map_err(|error| format!("无法检查提交信息临时文件: {error}"))?;
                    validate_commit_message_metadata(&metadata)?;
                    file.write_all(bytes)
                        .map_err(|error| format!("无法写入提交信息临时文件: {error}"))?;
                    file.sync_all()
                        .map_err(|error| format!("无法刷新提交信息临时文件: {error}"))?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("无法创建提交信息临时文件: {error}")),
            }
        }
        Err("无法分配提交信息临时文件".into())
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        let metadata = fs::symlink_metadata(&self.path)
            .map_err(|error| format!("无法检查提交信息临时文件: {error}"))?;
        validate_commit_message_metadata(&metadata)?;
        let mut options = OpenOptions::new();
        options.read(true);
        apply_commit_message_nofollow_options(&mut options);
        let mut file = options
            .open(&self.path)
            .map_err(|error| format!("无法以非链接方式读取提交信息临时文件: {error}"))?;
        validate_commit_message_metadata(
            &file
                .metadata()
                .map_err(|error| format!("无法复核提交信息临时文件: {error}"))?,
        )?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        Read::by_ref(&mut file)
            .take(MAX_COMMIT_MESSAGE_BYTES.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("无法读取提交信息临时文件: {error}"))?;
        if bytes.len() > MAX_COMMIT_MESSAGE_BYTES {
            return Err("提交信息 hook 输出超过安全上限".into());
        }
        Ok(bytes)
    }
}

impl Drop for CommitMessageFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn apply_commit_message_nofollow_options(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ);
}

#[cfg(not(windows))]
fn apply_commit_message_nofollow_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

fn validate_commit_message_metadata(metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() || commit_message_metadata_is_link(metadata) {
        return Err("提交信息临时文件必须是普通的非链接文件".into());
    }
    if metadata.len() > MAX_COMMIT_MESSAGE_BYTES as u64 {
        return Err("提交信息 hook 输出超过安全上限".into());
    }
    Ok(())
}

#[cfg(windows)]
fn commit_message_metadata_is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn commit_message_metadata_is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
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

pub fn prepare_stage_all(workspace: &Path) -> Result<GitStageAllPreparation, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut transaction = StageAllIndexTransaction::acquire(&repository)?;
    let first_snapshot = snapshot_for_repository(&repository)?;
    validate_stage_all_snapshot(&repository, &first_snapshot)?;
    let first = transaction.build_candidate(&first_snapshot)?;
    let snapshot = snapshot_for_repository(&repository)?;
    validate_stage_all_snapshot(&repository, &snapshot)?;
    let candidate = transaction.build_candidate(&snapshot)?;
    if first.proof != candidate.proof {
        return Err("Git 工作区在准备全部暂存证明时发生变化；请重试".into());
    }
    Ok(GitStageAllPreparation {
        snapshot: bounded_workspace_snapshot(snapshot),
        target_revision: candidate.proof.target_revision,
        candidate_tree_oid: candidate.proof.tree_oid,
    })
}

pub fn prepare_commit(workspace: &Path, message: &str) -> Result<GitCommitPreparation, String> {
    let repository = require_repository(workspace)?;
    let lock = repository_lock(&repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let normalized_message = normalize_commit_message(&repository, message.as_bytes())?;
    let message_digest = commit_message_digest(&normalized_message);
    let mut transaction = StageAllIndexTransaction::acquire(&repository)?;
    let first_snapshot = snapshot_for_repository(&repository)?;
    validate_commit_snapshot(&first_snapshot)?;
    let first_state = resolve_commit_head_state(&repository, &first_snapshot)?;
    let first_identity = resolve_commit_identity(&repository)?;
    let first_signing = commit_signing_enabled(&repository)?;
    let first = transaction.build_commit_candidate(
        &first_state,
        &first_identity,
        &message_digest,
        first_signing,
    )?;

    let snapshot = snapshot_for_repository(&repository)?;
    validate_commit_snapshot(&snapshot)?;
    let state = resolve_commit_head_state(&repository, &snapshot)?;
    let identity = resolve_commit_identity(&repository)?;
    let signing = commit_signing_enabled(&repository)?;
    let candidate =
        transaction.build_commit_candidate(&state, &identity, &message_digest, signing)?;
    if first != candidate {
        return Err("Git 索引、HEAD 或提交设置在准备提交证明时发生变化；请重试".into());
    }
    Ok(GitCommitPreparation {
        snapshot: bounded_workspace_snapshot(snapshot),
        target_revision: candidate.target_revision,
        candidate_tree_oid: candidate.tree_oid,
        message_digest,
    })
}

fn validate_commit_snapshot(snapshot: &GitWorkspaceSnapshot) -> Result<(), String> {
    if snapshot.conflicted > 0 {
        return Err("Git 索引仍有冲突；请先解决并暂存冲突文件".into());
    }
    if snapshot.staged == 0 {
        return Err("Git 索引没有可提交的已暂存变更".into());
    }
    if snapshot.operation.is_some() {
        return Err("仓库正在执行 Git 操作；请使用对应的继续或中止操作".into());
    }
    Ok(())
}

fn validate_stage_all_snapshot(
    repository: &Repository,
    snapshot: &GitWorkspaceSnapshot,
) -> Result<(), String> {
    if !snapshot.files_complete {
        return Err("Git 全部暂存准备必须基于完整工作区状态".into());
    }
    if let Some(change) = snapshot
        .files
        .iter()
        .find(|change| change.submodule && !change.submodule_commit_changed)
    {
        return Err(format!(
            "子模块 {} 只有内部未提交变更，无法作为父仓库的“全部暂存”目标；请将该子模块作为独立工作区处理",
            change.path
        ));
    }
    for change in snapshot.files.iter().filter(|change| change.untracked) {
        let path = repository.root.join(&change.path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("无法检查全部暂存中的未跟踪目标 {}: {error}", change.path))?;
        if metadata.is_dir() {
            return Err(format!(
                "未跟踪目录 {} 可能是嵌套仓库，无法作为父仓库的“全部暂存”目标；请将它作为独立工作区处理",
                change.path
            ));
        }
    }
    Ok(())
}

fn validate_bulk_action(repository: &Repository, action: &GitAction) -> Result<(), String> {
    let (expected_content_revision, require_clean) = match action {
        GitAction::UnstageAll {
            expected_content_revision,
        } => (expected_content_revision, false),
        GitAction::BisectStep {
            expected_content_revision,
            ..
        } => (expected_content_revision, true),
        _ => return Ok(()),
    };
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.content_revision != expected_content_revision {
        return Err("Git 工作区已在操作前发生变化；请刷新后重试".into());
    }
    if require_clean && !snapshot.is_clean {
        return Err("Git bisect 前进操作要求工作树干净；请先提交或储藏当前变更".into());
    }
    Ok(())
}

fn validate_expected_ref_state(repository: &Repository, action: &GitAction) -> Result<(), String> {
    let compare = |label: &str, expected: &str, actual: String| -> Result<(), String> {
        let expected = validate_object_id(label, expected.to_owned())?;
        if actual != expected {
            return Err(format!("{label} 已在确认后发生变化；请刷新分支并重新确认"));
        }
        Ok(())
    };
    match action {
        GitAction::DeleteBranch {
            name,
            expected_head,
            expected_oid,
            ..
        } => {
            validate_local_branch(repository, name)?;
            compare(
                "仓库 HEAD",
                expected_head,
                resolve_commit(repository, "HEAD")?,
            )?;
            compare(
                "Git 分支",
                expected_oid,
                resolve_commit(repository, &format!("refs/heads/{name}"))?,
            )
        }
        GitAction::Merge {
            branch,
            expected_head,
            expected_branch_oid,
        } => {
            validate_local_branch(repository, branch)?;
            compare(
                "仓库 HEAD",
                expected_head,
                resolve_commit(repository, "HEAD")?,
            )?;
            compare(
                "待合并分支",
                expected_branch_oid,
                resolve_commit(repository, &format!("refs/heads/{branch}"))?,
            )
        }
        GitAction::Push { expected_head, .. } => compare(
            "仓库 HEAD",
            expected_head,
            resolve_commit(repository, "HEAD")?,
        ),
        _ => Ok(()),
    }
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

pub fn github_repository(workspace: &Path) -> Result<Option<GithubRepository>, String> {
    let resolved = resolve_github(workspace)?;
    if let Some(repository) = resolved.context.repository {
        Ok(Some(repository))
    } else if let Some(error) = resolved.context.cli.error {
        Err(redact_sensitive_text(&error))
    } else {
        Ok(None)
    }
}

pub fn github_pull_requests(
    workspace: &Path,
    page: u32,
    page_size: u16,
) -> Result<GithubPullRequestList, String> {
    validate_github_pr_page(page, page_size)?;
    let resolved = resolve_github(workspace)?;
    let mut result = GithubPullRequestList {
        pull_requests: Vec::new(),
        page,
        page_size,
        has_more: false,
        next_page: None,
    };
    let Some(gh) = resolved.gh.as_deref() else {
        return Ok(result);
    };
    let Some(repository) = resolved.context.repository.as_ref() else {
        return Ok(result);
    };
    if !resolved.context.cli.authenticated {
        return Ok(result);
    }
    let output = run_gh(
        gh,
        &resolved.repository.root,
        github_pull_request_list_args(repository, page, page_size),
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request 列表", &output)?;
    let (pull_requests, has_more) = parse_github_pull_request_page(&output.stdout)?;
    result.pull_requests = pull_requests;
    result.has_more = has_more && page < MAX_GITHUB_PR_PAGE;
    result.next_page = result.has_more.then(|| page + 1);
    Ok(result)
}

pub fn github_pull_request_detail(
    workspace: &Path,
    number: u64,
) -> Result<GithubPullRequestDetail, String> {
    validate_pr_number(number)?;
    let resolved = require_github_repository(workspace)?;
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let selector = resolved
        .selector
        .as_deref()
        .expect("validated repository selector");
    let output = run_gh(
        gh,
        &resolved.repository.root,
        [
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(number.to_string()),
            OsString::from("-R"),
            OsString::from(selector),
            OsString::from("--json"),
            OsString::from(GITHUB_PR_DETAIL_JSON_FIELDS),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request", &output)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub Pull Request JSON 无效: {error}"))?;
    parse_pr_detail(&value)
}

pub fn github_pull_request_readiness(
    workspace: &Path,
    number: u64,
) -> Result<GithubPullRequestReadiness, String> {
    validate_pr_number(number)?;
    if number > i32::MAX as u64 {
        return Err("Pull Request 编号超过 GitHub GraphQL Int 上限".into());
    }
    let initially_resolved = require_github_repository(workspace)?;
    let lock = repository_lock(&initially_resolved.repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = require_github_repository(workspace)?;
    if !same_path(
        &initially_resolved.repository.root,
        &resolved.repository.root,
    ) {
        return Err("Git 工作目录已在读取 PR readiness 前发生变化；请刷新后重试".into());
    }
    github_pull_request_readiness_for_resolved(&resolved, number)
}

fn github_pull_request_readiness_for_resolved(
    resolved: &ResolvedGithub,
    number: u64,
) -> Result<GithubPullRequestReadiness, String> {
    validate_pr_number(number)?;
    if number > i32::MAX as u64 {
        return Err("Pull Request 编号超过 GitHub GraphQL Int 上限".into());
    }
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let core_output = run_gh(
        gh,
        &resolved.repository.root,
        github_graphql_args(&repository.host),
        Some(github_readiness_query_input(
            GITHUB_READINESS_CORE_QUERY,
            repository,
            number,
            None,
        )?),
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request readiness 核心信息", &core_output)?;
    if core_output.stdout_truncated {
        return Err("GitHub Pull Request readiness 核心响应超过安全上限".into());
    }
    let (identity, merge_policy, viewer) =
        parse_github_readiness_core_response(&core_output.stdout, repository, number)?;
    let identity_revision =
        github_readiness_revision(b"mework.github-readiness.identity.v1", &identity)?;

    let checks = github_readiness_checks_phase(&resolved, &identity, &viewer.login);
    let viewer_default = github_readiness_viewer_default_phase(&resolved, &identity, &viewer.login);
    let auto_merge = github_readiness_auto_merge_phase(&resolved, &identity, &viewer.login);
    let merge_queue = github_readiness_merge_queue_phase(&resolved, &identity, &viewer.login);
    let readiness_revision = github_readiness_revision(
        b"mework.github-readiness.aggregate.v1",
        &(
            &identity_revision,
            &merge_policy,
            &viewer,
            &checks,
            &viewer_default,
            &auto_merge,
            &merge_queue,
        ),
    )?;
    Ok(GithubPullRequestReadiness {
        identity,
        merge_policy,
        viewer,
        checks,
        viewer_default,
        auto_merge,
        merge_queue,
        identity_revision,
        readiness_revision,
    })
}

pub fn github_pull_request_diff(
    workspace: &Path,
    number: u64,
    path: Option<String>,
) -> Result<GithubPullRequestDiff, String> {
    validate_pr_number(number)?;
    let path = path.map(|path| validate_relative_path(&path)).transpose()?;
    let resolved = require_github_repository(workspace)?;
    let expected = github_pull_request_read_state_for_resolved(&resolved, number)?;
    if expected.changed_files > MAX_GITHUB_PR_FILES {
        return Err(format!(
            "Pull Request #{number} 包含 {} 个文件，超过 GitHub 文件 API 可完整审阅的 {MAX_GITHUB_PR_FILES} 个文件上限",
            expected.changed_files
        ));
    }
    let full_patch = if path.is_none() {
        Some(github_pull_request_full_patch(&resolved, number)?)
    } else {
        None
    };
    let file_read =
        github_pull_request_files(&resolved, number, expected.changed_files, path.as_deref())?;
    let actual = github_pull_request_read_state_for_resolved(&resolved, number)?;
    if actual.head_ref_oid != expected.head_ref_oid
        || actual.base_ref_oid != expected.base_ref_oid
        || actual.changed_files != expected.changed_files
    {
        return Err(format!(
            "Pull Request #{number} 在读取差异时已更新；请刷新后重新审阅"
        ));
    }
    if let Some((patch, truncated, binary)) = full_patch {
        let (additions, deletions) = count_patch_lines(&patch);
        return Ok(GithubPullRequestDiff {
            path: None,
            head_ref_oid: expected.head_ref_oid,
            patch,
            truncated,
            additions,
            deletions,
            binary,
            files: file_read.files,
        });
    }
    let selected = file_read.selected.ok_or_else(|| {
        format!(
            "Pull Request #{number} 不包含文件 {}",
            path.as_deref().unwrap_or_default()
        )
    })?;
    let additions = selected.change.additions.unwrap_or(0);
    let deletions = selected.change.deletions.unwrap_or(0);
    let binary = selected.change.binary;
    let truncated = github_pull_request_file_patch_truncated(&selected);
    let patch = render_github_pull_request_file_patch(&selected);
    Ok(GithubPullRequestDiff {
        path,
        head_ref_oid: expected.head_ref_oid,
        patch,
        truncated,
        additions,
        deletions,
        binary,
        files: file_read.files,
    })
}

pub fn github_pull_request_review_threads(
    workspace: &Path,
    request: GithubReviewThreadsRequest,
) -> Result<GithubReviewThreadsResult, String> {
    validate_pr_number(request.number)?;
    validate_github_review_threads_page_size(request.page_size)?;
    let expected_head_oid =
        validate_object_id("Pull Request head commit", request.expected_head_oid)?;
    let cursor = validate_github_review_cursor(request.cursor.as_deref())?;
    let initially_resolved = require_github_repository(workspace)?;
    let lock = repository_lock(&initially_resolved.repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = require_github_repository(workspace)?;
    if !same_path(
        &initially_resolved.repository.root,
        &resolved.repository.root,
    ) {
        return Err("Git 工作目录已在读取审阅线程前发生变化；请刷新后重试".into());
    }
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let input = github_review_threads_query_input(
        repository,
        request.number,
        request.page_size,
        cursor.as_deref(),
    )?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        github_graphql_args(&repository.host),
        Some(input),
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request 审阅线程", &output)?;
    if output.stdout_truncated {
        return Err("GitHub 审阅线程响应超过安全上限，无法可靠解析".into());
    }
    parse_github_review_threads_response(
        &output.stdout,
        request.number,
        &expected_head_oid,
        request.page_size,
    )
}

pub fn github_pull_request_review_thread_comments(
    workspace: &Path,
    request: GithubReviewThreadCommentsRequest,
) -> Result<GithubReviewThreadCommentsResult, String> {
    let GithubReviewThreadCommentsRequest {
        expected_repository,
        expected_viewer_login,
        number,
        expected_state,
        expected_head_oid,
        thread_id,
        cursor,
        page_size,
    } = request;
    validate_pr_number(number)?;
    validate_github_review_comments_page_size(page_size)?;
    let expected_state =
        normalize_github_pull_request_state("Pull Request 预期状态", &expected_state)?;
    let expected_head_oid = validate_object_id("Pull Request head commit", expected_head_oid)?;
    let thread_id = validate_github_node_id("GitHub 审阅线程 ID", &thread_id)?;
    let cursor = validate_github_review_cursor(cursor.as_deref())?;
    let initially_resolved = require_github_repository(workspace)?;
    let lock = repository_lock(&initially_resolved.repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = require_github_repository(workspace)?;
    if !same_path(
        &initially_resolved.repository.root,
        &resolved.repository.root,
    ) {
        return Err("Git 工作目录已在读取审阅评论前发生变化；请刷新后重试".into());
    }
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    validate_github_write_identity(&expected_repository, &expected_viewer_login, repository)?;
    let scope = github_review_thread_scope_for_resolved(&resolved, &thread_id)?;
    validate_github_review_thread_scope_identity(
        &scope,
        repository,
        number,
        &expected_head_oid,
        &expected_state,
    )?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        github_graphql_args(&repository.host),
        Some(github_review_thread_comments_query_input(
            &thread_id,
            page_size,
            cursor.as_deref(),
        )?),
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub 审阅线程评论", &output)?;
    if output.stdout_truncated {
        return Err("GitHub 审阅线程评论响应超过安全上限，无法可靠解析".into());
    }
    parse_github_review_thread_comments_response(
        &output.stdout,
        number,
        &expected_head_oid,
        &thread_id,
        page_size,
        cursor.as_deref(),
    )
}

fn validate_github_identity_value(label: &str, value: &str) -> Result<(), String> {
    if value.trim() != value
        || value.is_empty()
        || value.contains('\0')
        || value.chars().any(char::is_whitespace)
        || value.len() > 1024
    {
        return Err(format!("{label}无效；请刷新 GitHub 仓库信息后重试"));
    }
    Ok(())
}

fn validate_github_write_identity(
    expected: &GithubRepositoryIdentity,
    expected_viewer_login: &str,
    actual: &GithubRepository,
) -> Result<(), String> {
    validate_github_identity_value("GitHub host", &expected.host)?;
    validate_github_identity_value("GitHub owner", &expected.owner)?;
    validate_github_identity_value("GitHub repository", &expected.name)?;
    validate_github_identity_value("GitHub 登录账号", expected_viewer_login)?;
    if !actual.authenticated
        || !actual.host.eq_ignore_ascii_case(&expected.host)
        || !actual.owner.eq_ignore_ascii_case(&expected.owner)
        || !actual.name.eq_ignore_ascii_case(&expected.name)
    {
        return Err("GitHub 仓库身份已在确认后发生变化；请刷新后重新确认".into());
    }
    let actual_viewer = actual
        .viewer_login
        .as_deref()
        .filter(|login| !login.trim().is_empty())
        .ok_or_else(|| "无法确认 GitHub 当前登录账号；已拒绝写操作".to_owned())?;
    if !actual_viewer.eq_ignore_ascii_case(expected_viewer_login) {
        return Err("GitHub 登录账号已在确认后发生变化；请刷新后重新确认".into());
    }
    Ok(())
}

fn normalize_github_pull_request_state(label: &str, value: &str) -> Result<String, String> {
    if value.trim() != value || value.is_empty() || value.contains('\0') {
        return Err(format!("{label}无效；请刷新 Pull Request 后重试"));
    }
    let state = value.to_ascii_lowercase();
    if !matches!(state.as_str(), "open" | "closed" | "merged") {
        return Err(format!("{label}无效；请刷新 Pull Request 后重试"));
    }
    Ok(state)
}

fn validate_github_pull_request_expectation(
    detail: &GithubPullRequestDetail,
    expected_head_oid: &str,
    expected_state: &str,
    required_state: &str,
) -> Result<String, String> {
    let expected_head_oid =
        validate_object_id("Pull Request head commit", expected_head_oid.to_owned())?;
    let expected_state =
        normalize_github_pull_request_state("Pull Request 预期状态", expected_state)?;
    let actual_state = normalize_github_pull_request_state("Pull Request 当前状态", &detail.state)?;
    if detail.head_ref_oid != expected_head_oid || actual_state != expected_state {
        return Err(format!(
            "Pull Request #{} 已在确认后发生变化；请刷新后重新确认",
            detail.number
        ));
    }
    if actual_state != required_state {
        return Err(format!(
            "Pull Request #{} 当前状态为 {}，不能执行此操作",
            detail.number, actual_state
        ));
    }
    Ok(expected_head_oid)
}

fn validate_github_local_state(
    repository: &Repository,
    expected_local_head_oid: &str,
    expected_content_revision: &str,
) -> Result<GitWorkspaceSnapshot, String> {
    let expected_local_head_oid =
        validate_object_id("本地 Git HEAD", expected_local_head_oid.to_owned())?;
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.operation.is_some() {
        return Err("本地仓库正在执行 Git 操作；请先继续或中止该操作".into());
    }
    if !snapshot.is_clean {
        return Err("本地工作区包含未提交变更；为避免覆盖内容，已拒绝 GitHub 操作".into());
    }
    if snapshot.head.as_deref() != Some(expected_local_head_oid.as_str())
        || snapshot.content_revision != expected_content_revision
    {
        return Err("本地 Git 状态已在确认后发生变化；请刷新后重新确认".into());
    }
    Ok(snapshot)
}

fn validate_github_review_cursor(cursor: Option<&str>) -> Result<Option<String>, String> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.is_empty()
        || cursor.contains('\0')
        || cursor.chars().any(char::is_whitespace)
        || cursor.as_bytes().len() > MAX_GITHUB_REVIEW_CURSOR_BYTES
    {
        return Err("GitHub 审阅线程游标无效；请重新加载".into());
    }
    Ok(Some(cursor.to_owned()))
}

fn validate_github_node_id(label: &str, value: &str) -> Result<String, String> {
    if value.trim() != value
        || value.is_empty()
        || value.contains('\0')
        || value.chars().any(char::is_whitespace)
        || value.len() > 1024
    {
        return Err(format!("{label}无效；请刷新后重试"));
    }
    Ok(value.to_owned())
}

fn github_graphql_args(host: &str) -> Vec<OsString> {
    vec![
        OsString::from("api"),
        OsString::from("graphql"),
        OsString::from("--hostname"),
        OsString::from(host),
        OsString::from("--input"),
        OsString::from("-"),
    ]
}

impl<T> GithubReadinessPhase<T> {
    fn available(value: T) -> Self {
        Self {
            availability: GithubReadinessAvailability::Available,
            value: Some(value),
            error: None,
        }
    }

    fn unavailable(error: String) -> Self {
        let error = redact_sensitive_text(&error)
            .chars()
            .take(1_000)
            .collect::<String>();
        Self {
            availability: if github_readiness_feature_unsupported(&error) {
                GithubReadinessAvailability::Unsupported
            } else {
                GithubReadinessAvailability::Error
            },
            value: None,
            error: Some(error),
        }
    }
}

fn github_readiness_feature_unsupported(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "undefinedfield",
        "doesn't exist on type",
        "does not exist on type",
        "unknown field",
        "cannot query field",
        "unknown argument",
        "is not defined by type",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn github_readiness_revision<T: Serialize>(domain: &[u8], value: &T) -> Result<String, String> {
    let payload = serde_json::to_vec(value)
        .map_err(|error| format!("无法编码 GitHub PR readiness revision: {error}"))?;
    let mut digest = Sha256::new();
    update_revision_component(&mut digest, b"domain", domain);
    update_revision_component(&mut digest, b"payload", &payload);
    Ok(lower_hex_digest(&digest.finalize().into()))
}

fn github_readiness_query_input(
    query: &str,
    repository: &GithubRepository,
    number: u64,
    page: Option<(u16, Option<&str>)>,
) -> Result<Vec<u8>, String> {
    if number == 0 || number > i32::MAX as u64 {
        return Err("Pull Request 编号超过 GitHub GraphQL Int 上限".into());
    }
    let mut variables = serde_json::json!({
        "owner": repository.owner,
        "name": repository.name,
        "number": number,
    });
    if let Some((first, after)) = page {
        if first == 0 || first > GITHUB_READINESS_CHECKS_PAGE_SIZE {
            return Err("GitHub readiness checks 每页数量无效".into());
        }
        let after = validate_github_review_cursor(after)?;
        variables["first"] = Value::from(first);
        variables["after"] = after.map(Value::from).unwrap_or(Value::Null);
    }
    serde_json::to_vec(&serde_json::json!({
        "query": query,
        "variables": variables,
    }))
    .map_err(|error| format!("无法编码 GitHub PR readiness 请求: {error}"))
}

fn validate_github_readiness_enum(label: &str, value: String) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
    {
        return Err(format!("{label}无效"));
    }
    Ok(value)
}

fn parse_github_readiness_repository_identity(
    value: &Value,
    host: &str,
    label: &str,
) -> Result<GithubReadinessRepositoryIdentity, String> {
    let node_id = validate_github_node_id(
        &format!("{label} node ID"),
        &required_string_field(value, "id")?,
    )?;
    let name_with_owner = required_string_field(value, "nameWithOwner")?;
    validate_github_identity_value(&format!("{label} nameWithOwner"), &name_with_owner)?;
    Ok(GithubReadinessRepositoryIdentity {
        host: host.to_ascii_lowercase(),
        node_id,
        name_with_owner,
    })
}

fn parse_github_readiness_core_response(
    bytes: &[u8],
    expected_repository: &GithubRepository,
    expected_number: u64,
) -> Result<
    (
        GithubPullRequestCoreIdentity,
        GithubPullRequestMergePolicy,
        GithubPullRequestViewer,
    ),
    String,
> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub PR readiness 核心 JSON 无效: {error}"))?;
    let data = github_graphql_data(&value, "读取 GitHub PR readiness 核心信息")?;
    let viewer_value = data
        .get("viewer")
        .filter(|viewer| viewer.is_object())
        .ok_or_else(|| "GitHub PR readiness 核心响应缺少 viewer".to_owned())?;
    let viewer_login = required_string_field(viewer_value, "login")?;
    validate_github_identity_value("GitHub viewer login", &viewer_login)?;
    if expected_repository
        .viewer_login
        .as_deref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&viewer_login))
    {
        return Err("GitHub PR readiness viewer 与当前 gh 活动账号不匹配".into());
    }
    let repository_value = data
        .get("repository")
        .filter(|repository| repository.is_object())
        .ok_or_else(|| "GitHub 仓库或 Pull Request 不存在".to_owned())?;
    let repository = parse_github_readiness_repository_identity(
        repository_value,
        &expected_repository.host,
        "GitHub repository",
    )?;
    if !repository
        .name_with_owner
        .eq_ignore_ascii_case(&expected_repository.name_with_owner)
    {
        return Err("GitHub PR readiness repository 身份不匹配".into());
    }
    let pull_request = repository_value
        .get("pullRequest")
        .filter(|pull_request| pull_request.is_object())
        .ok_or_else(|| "GitHub Pull Request 不存在".to_owned())?;
    let number = required_u64_field(pull_request, "number")?;
    if number != expected_number {
        return Err("GitHub PR readiness Pull Request 编号不匹配".into());
    }
    let base_repository_value = pull_request
        .get("baseRepository")
        .filter(|repository| repository.is_object())
        .ok_or_else(|| "GitHub Pull Request 缺少 baseRepository".to_owned())?;
    let base_repository = parse_github_readiness_repository_identity(
        base_repository_value,
        &expected_repository.host,
        "GitHub base repository",
    )?;
    if base_repository.node_id != repository.node_id
        || !base_repository
            .name_with_owner
            .eq_ignore_ascii_case(&repository.name_with_owner)
    {
        return Err("GitHub Pull Request base repository 与请求仓库不匹配".into());
    }
    let head_repository = match pull_request.get("headRepository") {
        None | Some(Value::Null) => None,
        Some(value) if value.is_object() => Some(parse_github_readiness_repository_identity(
            value,
            &expected_repository.host,
            "GitHub head repository",
        )?),
        Some(_) => return Err("GitHub Pull Request headRepository 字段无效".into()),
    };
    let identity = GithubPullRequestCoreIdentity {
        repository,
        pull_request_node_id: validate_github_node_id(
            "GitHub Pull Request node ID",
            &required_string_field(pull_request, "id")?,
        )?,
        number,
        state: normalize_github_pull_request_state(
            "GitHub Pull Request 当前状态",
            &required_string_field(pull_request, "state")?,
        )?,
        draft: required_bool_field(pull_request, "isDraft")?,
        base_repository,
        head_repository,
        base_ref_name: validate_gh_ref(
            "base",
            required_string_field(pull_request, "baseRefName")?,
        )?,
        base_ref_oid: validate_object_id(
            "Pull Request base commit",
            required_string_field(pull_request, "baseRefOid")?,
        )?,
        head_ref_name: validate_gh_ref(
            "head",
            required_string_field(pull_request, "headRefName")?,
        )?,
        head_ref_oid: validate_object_id(
            "Pull Request head commit",
            required_string_field(pull_request, "headRefOid")?,
        )?,
    };
    let merge_policy = GithubPullRequestMergePolicy {
        merge_state_status: validate_github_readiness_enum(
            "GitHub mergeStateStatus",
            required_string_field(pull_request, "mergeStateStatus")?,
        )?,
        mergeable: validate_github_readiness_enum(
            "GitHub mergeable",
            required_string_field(pull_request, "mergeable")?,
        )?,
        merge_commit_allowed: required_bool_field(repository_value, "mergeCommitAllowed")?,
        squash_merge_allowed: required_bool_field(repository_value, "squashMergeAllowed")?,
        rebase_merge_allowed: required_bool_field(repository_value, "rebaseMergeAllowed")?,
    };
    let viewer = GithubPullRequestViewer {
        login: viewer_login,
        can_update: required_bool_field(pull_request, "viewerCanUpdate")?,
        can_merge_as_admin: required_bool_field(pull_request, "viewerCanMergeAsAdmin")?,
    };
    Ok((identity, merge_policy, viewer))
}

fn github_readiness_validate_scope(
    bytes: &[u8],
    label: &str,
    expected: &GithubPullRequestCoreIdentity,
    expected_viewer_login: &str,
) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("{label} JSON 无效: {error}"))?;
    // Keep the parsed owner alive while returning a borrowed PR by moving the
    // Value into the second tuple item is not possible safely. Callers use the
    // owned Value and resolve the PR again after this validator.
    let data = github_graphql_data(&value, label)?;
    let viewer = data
        .get("viewer")
        .filter(|viewer| viewer.is_object())
        .ok_or_else(|| format!("{label}缺少 viewer"))?;
    let viewer_login = required_string_field(viewer, "login")?;
    if !viewer_login.eq_ignore_ascii_case(expected_viewer_login) {
        return Err(format!("{label} viewer 已变化"));
    }
    let repository = data
        .get("repository")
        .filter(|repository| repository.is_object())
        .ok_or_else(|| format!("{label} repository 不存在"))?;
    let repository_id = validate_github_node_id(
        "GitHub repository node ID",
        &required_string_field(repository, "id")?,
    )?;
    if repository_id != expected.repository.node_id
        || !required_string_field(repository, "nameWithOwner")?
            .eq_ignore_ascii_case(&expected.repository.name_with_owner)
    {
        return Err(format!("{label} repository 身份已变化"));
    }
    let pull_request = repository
        .get("pullRequest")
        .filter(|pull_request| pull_request.is_object())
        .ok_or_else(|| format!("{label} Pull Request 不存在"))?;
    validate_github_readiness_pull_request_scope(pull_request, expected, label)?;
    Ok(value)
}

fn validate_github_readiness_pull_request_scope(
    pull_request: &Value,
    expected: &GithubPullRequestCoreIdentity,
    label: &str,
) -> Result<(), String> {
    let id = validate_github_node_id(
        "GitHub Pull Request node ID",
        &required_string_field(pull_request, "id")?,
    )?;
    let number = required_u64_field(pull_request, "number")?;
    let state = normalize_github_pull_request_state(
        "GitHub Pull Request 当前状态",
        &required_string_field(pull_request, "state")?,
    )?;
    let base_ref_name =
        validate_gh_ref("base", required_string_field(pull_request, "baseRefName")?)?;
    let base_ref_oid = validate_object_id(
        "Pull Request base commit",
        required_string_field(pull_request, "baseRefOid")?,
    )?;
    let head_ref_name =
        validate_gh_ref("head", required_string_field(pull_request, "headRefName")?)?;
    let head_ref_oid = validate_object_id(
        "Pull Request head commit",
        required_string_field(pull_request, "headRefOid")?,
    )?;
    let draft = required_bool_field(pull_request, "isDraft")?;
    let base_repository = pull_request
        .get("baseRepository")
        .filter(|value| value.is_object())
        .ok_or_else(|| format!("{label} 缺少 baseRepository"))?;
    let base_repository_id = validate_github_node_id(
        "GitHub base repository node ID",
        &required_string_field(base_repository, "id")?,
    )?;
    let base_repository_name = required_string_field(base_repository, "nameWithOwner")?;
    let head_repository = match pull_request.get("headRepository") {
        None | Some(Value::Null) => None,
        Some(value) if value.is_object() => Some((
            validate_github_node_id(
                "GitHub head repository node ID",
                &required_string_field(value, "id")?,
            )?,
            required_string_field(value, "nameWithOwner")?,
        )),
        Some(_) => return Err(format!("{label} headRepository 字段无效")),
    };
    let expected_head_repository = expected
        .head_repository
        .as_ref()
        .map(|repository| (&repository.node_id, &repository.name_with_owner));
    if id != expected.pull_request_node_id
        || number != expected.number
        || state != expected.state
        || draft != expected.draft
        || base_ref_name != expected.base_ref_name
        || base_ref_oid != expected.base_ref_oid
        || head_ref_name != expected.head_ref_name
        || head_ref_oid != expected.head_ref_oid
        || base_repository_id != expected.base_repository.node_id
        || !base_repository_name.eq_ignore_ascii_case(&expected.base_repository.name_with_owner)
        || head_repository.as_ref().map(|(id, name)| (id, name)) != expected_head_repository
    {
        return Err(format!("{label} PR/base/head 身份已变化；请刷新后重试"));
    }
    Ok(())
}

fn github_readiness_checks_phase(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> GithubReadinessPhase<GithubPullRequestChecks> {
    match github_readiness_checks(resolved, expected, viewer_login) {
        Ok(value) => GithubReadinessPhase::available(value),
        Err(error) => GithubReadinessPhase::unavailable(error),
    }
}

fn github_readiness_checks(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<GithubPullRequestChecks, String> {
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let mut cursor: Option<String> = None;
    let mut expected_total: Option<u64> = None;
    let mut checks = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_cursors = HashSet::new();
    for _ in 0..MAX_GITHUB_READINESS_CHECK_PAGES {
        let output = run_gh(
            gh,
            &resolved.repository.root,
            github_graphql_args(&repository.host),
            Some(github_readiness_query_input(
                GITHUB_READINESS_CHECKS_QUERY,
                repository,
                expected.number,
                Some((GITHUB_READINESS_CHECKS_PAGE_SIZE, cursor.as_deref())),
            )?),
            NETWORK_COMMAND_TIMEOUT,
            MAX_JSON_OUTPUT,
        )?;
        require_success("读取 GitHub Pull Request readiness checks", &output)?;
        if output.stdout_truncated {
            return Err("GitHub Pull Request readiness checks 响应超过安全上限".into());
        }
        let value = github_readiness_validate_scope(
            &output.stdout,
            "读取 GitHub PR readiness checks",
            expected,
            viewer_login,
        )?;
        let pull_request = value
            .get("data")
            .and_then(|data| data.get("repository"))
            .and_then(|repository| repository.get("pullRequest"))
            .ok_or_else(|| "GitHub readiness checks 响应缺少 Pull Request".to_owned())?;
        let Some(rollup) = pull_request
            .get("statusCheckRollup")
            .filter(|value| value.is_object())
        else {
            if cursor.is_some() {
                return Err("GitHub readiness checks 分页期间 statusCheckRollup 消失".into());
            }
            return Ok(GithubPullRequestChecks {
                total_count: 0,
                checks: Vec::new(),
            });
        };
        let contexts = rollup
            .get("contexts")
            .filter(|value| value.is_object())
            .ok_or_else(|| "GitHub readiness checks 响应缺少 contexts".to_owned())?;
        let total_count = required_u64_field(contexts, "totalCount")?;
        if total_count as usize > MAX_GITHUB_READINESS_CHECKS {
            return Err(format!(
                "GitHub readiness checks 超过 {MAX_GITHUB_READINESS_CHECKS} 项安全上限"
            ));
        }
        if expected_total.get_or_insert(total_count) != &total_count {
            return Err("GitHub readiness checks totalCount 在分页期间发生变化".into());
        }
        let nodes = contexts
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| "GitHub readiness checks contexts 缺少 nodes".to_owned())?;
        append_github_readiness_check_nodes(&mut checks, &mut seen_ids, nodes, total_count)?;
        let page_info = contexts
            .get("pageInfo")
            .filter(|value| value.is_object())
            .ok_or_else(|| "GitHub readiness checks contexts 缺少 pageInfo".to_owned())?;
        let next = parse_github_page_next_cursor(page_info, "GitHub readiness checks")?;
        let next = validate_github_readiness_next_cursor(
            cursor.as_deref(),
            next,
            nodes.len(),
            &mut seen_cursors,
        )?;
        let Some(next) = next else {
            if checks.len() as u64 != total_count {
                return Err("GitHub readiness checks 末页数量与 totalCount 不一致".into());
            }
            checks.sort_by(|left, right| {
                (
                    left.kind.as_str(),
                    left.name.as_str(),
                    left.node_id.as_str(),
                )
                    .cmp(&(
                        right.kind.as_str(),
                        right.name.as_str(),
                        right.node_id.as_str(),
                    ))
            });
            return Ok(GithubPullRequestChecks {
                total_count,
                checks,
            });
        };
        cursor = Some(next);
    }
    Err(format!(
        "GitHub readiness checks 超过 {MAX_GITHUB_READINESS_CHECK_PAGES} 页安全上限"
    ))
}

fn validate_github_readiness_next_cursor(
    current: Option<&str>,
    next: Option<String>,
    nodes_len: usize,
    seen: &mut HashSet<String>,
) -> Result<Option<String>, String> {
    if next.as_deref() == current && next.is_some() {
        return Err("GitHub readiness checks 分页游标未前进".into());
    }
    if next.is_some() && nodes_len == 0 {
        return Err("GitHub readiness checks 返回空页面但仍声明存在下一页".into());
    }
    if let Some(next) = next.as_ref() {
        if !seen.insert(next.clone()) {
            return Err("GitHub readiness checks 分页游标形成循环".into());
        }
    }
    Ok(next)
}

fn append_github_readiness_check_nodes(
    checks: &mut Vec<GithubPullRequestReadinessCheck>,
    seen_ids: &mut HashSet<String>,
    nodes: &[Value],
    total_count: u64,
) -> Result<(), String> {
    if nodes.len() > usize::from(GITHUB_READINESS_CHECKS_PAGE_SIZE)
        || checks.len().saturating_add(nodes.len()) > total_count as usize
    {
        return Err("GitHub readiness checks 数量与分页元数据不一致".into());
    }
    for node in nodes {
        let check = parse_github_readiness_check(node)?;
        if !seen_ids.insert(check.node_id.clone()) {
            return Err("GitHub readiness checks 分页包含重复 node ID".into());
        }
        checks.push(check);
    }
    Ok(())
}

fn parse_github_readiness_check(value: &Value) -> Result<GithubPullRequestReadinessCheck, String> {
    let kind = required_string_field(value, "__typename")?;
    let node_id = validate_github_node_id(
        "GitHub readiness check node ID",
        &required_string_field(value, "id")?,
    )?;
    match kind.as_str() {
        "CheckRun" => Ok(GithubPullRequestReadinessCheck {
            node_id,
            kind,
            name: required_string_field(value, "name")?,
            state: validate_github_readiness_enum(
                "GitHub CheckRun status",
                required_string_field(value, "status")?,
            )?,
            conclusion: optional_string_field(value, "conclusion")?
                .map(|value| validate_github_readiness_enum("GitHub CheckRun conclusion", value))
                .transpose()?,
            workflow: parse_github_readiness_check_run_workflow(value)?,
            description: None,
            link: optional_string_field(value, "detailsUrl")?,
            started_at: optional_string_field(value, "startedAt")?,
            completed_at: optional_string_field(value, "completedAt")?,
            required: required_bool_field(value, "isRequired")?,
        }),
        "StatusContext" => Ok(GithubPullRequestReadinessCheck {
            node_id,
            kind,
            name: required_string_field(value, "context")?,
            state: validate_github_readiness_enum(
                "GitHub StatusContext state",
                required_string_field(value, "state")?,
            )?,
            conclusion: None,
            workflow: None,
            description: optional_string_field(value, "description")?,
            link: optional_string_field(value, "targetUrl")?,
            started_at: None,
            completed_at: None,
            required: required_bool_field(value, "isRequired")?,
        }),
        _ => Err("GitHub readiness checks 包含不支持的 context 类型".into()),
    }
}

fn parse_github_readiness_check_run_workflow(value: &Value) -> Result<Option<String>, String> {
    let check_suite = match value.get("checkSuite") {
        None | Some(Value::Null) => return Ok(None),
        Some(check_suite) if check_suite.is_object() => check_suite,
        Some(_) => return Err("GitHub CheckRun checkSuite 字段无效".into()),
    };
    let workflow_run = match check_suite.get("workflowRun") {
        None | Some(Value::Null) => return Ok(None),
        Some(workflow_run) if workflow_run.is_object() => workflow_run,
        Some(_) => return Err("GitHub CheckRun workflowRun 字段无效".into()),
    };
    let workflow = match workflow_run.get("workflow") {
        None | Some(Value::Null) => return Ok(None),
        Some(workflow) if workflow.is_object() => workflow,
        Some(_) => return Err("GitHub CheckRun workflow 字段无效".into()),
    };
    optional_string_field(workflow, "name")
}

#[cfg(test)]
fn github_readiness_unsupported_phase<T>(feature: &str) -> GithubReadinessPhase<T> {
    GithubReadinessPhase {
        availability: GithubReadinessAvailability::Unsupported,
        value: None,
        error: Some(format!("{feature} 将在后续兼容性阶段启用")),
    }
}

fn github_readiness_viewer_default_phase(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> GithubReadinessPhase<GithubPullRequestViewerDefault> {
    match github_readiness_viewer_default(resolved, expected, viewer_login) {
        Ok(value) => GithubReadinessPhase::available(value),
        Err(error) => GithubReadinessPhase::unavailable(error),
    }
}

fn github_readiness_auto_merge_phase(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> GithubReadinessPhase<Option<GithubPullRequestAutoMerge>> {
    match github_readiness_auto_merge(resolved, expected, viewer_login) {
        Ok(value) => GithubReadinessPhase::available(value),
        Err(error) => GithubReadinessPhase::unavailable(error),
    }
}

fn github_readiness_merge_queue_phase(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> GithubReadinessPhase<GithubPullRequestMergeQueueState> {
    match github_readiness_merge_queue(resolved, expected, viewer_login) {
        Ok(value) => GithubReadinessPhase::available(value),
        Err(error) => GithubReadinessPhase::unavailable(error),
    }
}

fn github_readiness_feature_output(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    query: &str,
    label: &str,
) -> Result<Vec<u8>, String> {
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        github_graphql_args(&repository.host),
        Some(github_readiness_query_input(
            query,
            repository,
            expected.number,
            None,
        )?),
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    if output.stdout_truncated || output.stderr_truncated {
        return Err(format!("{label}响应超过安全上限"));
    }
    if !output.success() {
        // `gh api graphql` exits non-zero for schema validation failures while
        // still returning the structured GraphQL error on stdout. Preserve
        // that error so `undefinedField` can degrade only this feature phase.
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if let Err(error) = github_graphql_data(&value, label) {
                return Err(error);
            }
        }
        require_success(label, &output)?;
    }
    Ok(output.stdout)
}

fn github_readiness_viewer_default(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<GithubPullRequestViewerDefault, String> {
    let bytes = github_readiness_feature_output(
        resolved,
        expected,
        GITHUB_READINESS_VIEWER_DEFAULT_QUERY,
        "读取 GitHub viewer default merge method",
    )?;
    parse_github_readiness_viewer_default_response(&bytes, expected, viewer_login)
}

fn parse_github_readiness_viewer_default_response(
    bytes: &[u8],
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<GithubPullRequestViewerDefault, String> {
    let label = "读取 GitHub viewer default merge method";
    let value = github_readiness_validate_scope(bytes, label, expected, viewer_login)?;
    let repository = github_readiness_scoped_repository(&value, label)?;
    Ok(GithubPullRequestViewerDefault {
        merge_method: validate_github_readiness_merge_method(
            "GitHub viewer default merge method",
            required_string_field(repository, "viewerDefaultMergeMethod")?,
        )?,
    })
}

fn github_readiness_auto_merge(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<Option<GithubPullRequestAutoMerge>, String> {
    let bytes = github_readiness_feature_output(
        resolved,
        expected,
        GITHUB_READINESS_AUTO_MERGE_QUERY,
        "读取 GitHub auto-merge",
    )?;
    parse_github_readiness_auto_merge_response(&bytes, expected, viewer_login)
}

fn parse_github_readiness_auto_merge_response(
    bytes: &[u8],
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<Option<GithubPullRequestAutoMerge>, String> {
    let label = "读取 GitHub auto-merge";
    let value = github_readiness_validate_scope(bytes, label, expected, viewer_login)?;
    let pull_request = github_readiness_scoped_pull_request(&value, label)?;
    let auto_merge = match pull_request.get("autoMergeRequest") {
        Some(Value::Null) => return Ok(None),
        Some(auto_merge) if auto_merge.is_object() => auto_merge,
        None => return Err("GitHub auto-merge 响应缺少 autoMergeRequest".into()),
        Some(_) => return Err("GitHub autoMergeRequest 字段无效".into()),
    };
    for field in [
        "enabledAt",
        "mergeMethod",
        "commitHeadline",
        "commitBody",
        "enabledBy",
    ] {
        if auto_merge.get(field).is_none() {
            return Err(format!("GitHub autoMergeRequest 响应缺少字段 {field}"));
        }
    }
    let enabled_by = match auto_merge.get("enabledBy") {
        Some(Value::Null) => None,
        Some(actor) if actor.is_object() => {
            let login = required_string_field(actor, "login")?;
            validate_github_identity_value("GitHub auto-merge enabledBy login", &login)?;
            Some(login)
        }
        None => unreachable!("presence checked above"),
        Some(_) => return Err("GitHub autoMergeRequest.enabledBy 字段无效".into()),
    };
    Ok(Some(GithubPullRequestAutoMerge {
        enabled_at: optional_string_field(auto_merge, "enabledAt")?
            .map(|value| validate_github_readiness_timestamp("GitHub auto-merge enabledAt", value))
            .transpose()?,
        merge_method: validate_github_readiness_merge_method(
            "GitHub auto-merge method",
            required_string_field(auto_merge, "mergeMethod")?,
        )?,
        commit_headline: validate_github_readiness_optional_text(
            "GitHub auto-merge commit headline",
            optional_string_field(auto_merge, "commitHeadline")?,
        )?,
        commit_body: validate_github_readiness_optional_text(
            "GitHub auto-merge commit body",
            optional_string_field(auto_merge, "commitBody")?,
        )?,
        enabled_by,
    }))
}

fn github_readiness_merge_queue(
    resolved: &ResolvedGithub,
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<GithubPullRequestMergeQueueState, String> {
    let bytes = github_readiness_feature_output(
        resolved,
        expected,
        GITHUB_READINESS_MERGE_QUEUE_QUERY,
        "读取 GitHub merge queue",
    )?;
    parse_github_readiness_merge_queue_response(&bytes, expected, viewer_login)
}

fn parse_github_readiness_merge_queue_response(
    bytes: &[u8],
    expected: &GithubPullRequestCoreIdentity,
    viewer_login: &str,
) -> Result<GithubPullRequestMergeQueueState, String> {
    let label = "读取 GitHub merge queue";
    let value = github_readiness_validate_scope(bytes, label, expected, viewer_login)?;
    let pull_request = github_readiness_scoped_pull_request(&value, label)?;
    let enabled = required_bool_field(pull_request, "isMergeQueueEnabled")?;
    let is_in_queue = required_bool_field(pull_request, "isInMergeQueue")?;
    let entry = match pull_request.get("mergeQueueEntry") {
        Some(Value::Null) => None,
        Some(entry) if entry.is_object() => Some(GithubPullRequestMergeQueueEntry {
            entry_id: validate_github_node_id(
                "GitHub merge queue entry ID",
                &required_string_field(entry, "id")?,
            )?,
            position: required_github_graphql_int(entry, "position")?,
            state: validate_github_readiness_enum(
                "GitHub merge queue entry state",
                required_string_field(entry, "state")?,
            )?,
            enqueued_at: validate_github_readiness_timestamp(
                "GitHub merge queue enqueuedAt",
                required_string_field(entry, "enqueuedAt")?,
            )?,
            estimated_time_to_merge: {
                if entry.get("estimatedTimeToMerge").is_none() {
                    return Err("GitHub mergeQueueEntry 响应缺少字段 estimatedTimeToMerge".into());
                }
                optional_github_graphql_int(entry, "estimatedTimeToMerge")?
            },
        }),
        None => return Err("GitHub merge queue 响应缺少 mergeQueueEntry".into()),
        Some(_) => return Err("GitHub mergeQueueEntry 字段无效".into()),
    };
    if is_in_queue != entry.is_some() {
        return Err("GitHub merge queue 的 isInMergeQueue 与 mergeQueueEntry 状态不一致".into());
    }
    if !enabled && (is_in_queue || entry.is_some()) {
        return Err("GitHub merge queue 未启用但响应包含当前队列状态".into());
    }
    Ok(GithubPullRequestMergeQueueState {
        enabled,
        is_in_queue,
        entry,
    })
}

fn github_readiness_scoped_repository<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a Value, String> {
    value
        .get("data")
        .and_then(|data| data.get("repository"))
        .filter(|repository| repository.is_object())
        .ok_or_else(|| format!("{label}响应缺少 repository"))
}

fn github_readiness_scoped_pull_request<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a Value, String> {
    github_readiness_scoped_repository(value, label)?
        .get("pullRequest")
        .filter(|pull_request| pull_request.is_object())
        .ok_or_else(|| format!("{label}响应缺少 Pull Request"))
}

fn validate_github_readiness_merge_method(label: &str, value: String) -> Result<String, String> {
    let value = validate_github_readiness_enum(label, value)?;
    if !matches!(value.as_str(), "MERGE" | "SQUASH" | "REBASE") {
        return Err(format!("{label}不是受支持的 GitHub merge method"));
    }
    Ok(value)
}

fn validate_github_readiness_timestamp(label: &str, value: String) -> Result<String, String> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > MAX_GITHUB_READINESS_TIMESTAMP_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || character == '\0')
    {
        return Err(format!("{label}无效"));
    }
    Ok(value)
}

fn validate_github_readiness_optional_text(
    label: &str,
    value: Option<String>,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > MAX_COMMIT_MESSAGE_BYTES
        || value.chars().count() > MAX_COMMIT_MESSAGE_CHARS
        || value.contains('\0')
    {
        return Err(format!("{label}超过安全上限或包含不允许的 NUL 字符"));
    }
    Ok(Some(value))
}

fn required_github_graphql_int(value: &Value, field: &str) -> Result<u64, String> {
    let value = required_u64_field(value, field)?;
    if value > i32::MAX as u64 {
        return Err(format!("GitHub JSON 字段 {field} 超过 GraphQL Int 上限"));
    }
    Ok(value)
}

fn optional_github_graphql_int(value: &Value, field: &str) -> Result<Option<u64>, String> {
    let value = optional_u64_field(value, field)?;
    if value.is_some_and(|value| value > i32::MAX as u64) {
        return Err(format!("GitHub JSON 字段 {field} 超过 GraphQL Int 上限"));
    }
    Ok(value)
}

fn github_review_threads_query_input(
    repository: &GithubRepository,
    number: u64,
    page_size: u16,
    cursor: Option<&str>,
) -> Result<Vec<u8>, String> {
    if number > i32::MAX as u64 {
        return Err("Pull Request 编号超过 GitHub GraphQL Int 上限".into());
    }
    serde_json::to_vec(&serde_json::json!({
        "query": GITHUB_REVIEW_THREADS_QUERY,
        "variables": {
            "owner": repository.owner,
            "name": repository.name,
            "number": number,
            "first": page_size,
            "after": cursor
        }
    }))
    .map_err(|error| format!("无法编码 GitHub 审阅线程请求: {error}"))
}

fn github_review_thread_comments_query_input(
    thread_id: &str,
    page_size: u16,
    cursor: Option<&str>,
) -> Result<Vec<u8>, String> {
    let thread_id = validate_github_node_id("GitHub 审阅线程 ID", thread_id)?;
    validate_github_review_comments_page_size(page_size)?;
    let cursor = validate_github_review_cursor(cursor)?;
    serde_json::to_vec(&serde_json::json!({
        "query": GITHUB_REVIEW_THREAD_COMMENTS_QUERY,
        "variables": {
            "threadId": thread_id,
            "first": page_size,
            "after": cursor
        }
    }))
    .map_err(|error| format!("无法编码 GitHub 审阅线程评论请求: {error}"))
}

fn github_graphql_data<'a>(value: &'a Value, label: &str) -> Result<&'a Value, String> {
    if let Some(errors) = value.get("errors") {
        let errors = errors
            .as_array()
            .ok_or_else(|| format!("{label} GraphQL errors 字段无效"))?;
        if !errors.is_empty() {
            let message = errors
                .iter()
                .map(|error| {
                    let code = error
                        .get("extensions")
                        .and_then(|extensions| extensions.get("code"))
                        .and_then(Value::as_str)
                        .map(redact_sensitive_text)
                        .map(|code| code.chars().take(128).collect::<String>());
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(redact_sensitive_text)
                        .map(|message| message.chars().take(500).collect::<String>());
                    match (code, message) {
                        (Some(code), Some(message)) => format!("{code}: {message}"),
                        (Some(code), None) => code,
                        (None, Some(message)) => message,
                        (None, None) => String::new(),
                    }
                })
                .filter(|message| !message.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if message.is_empty() {
                format!("{label} GraphQL 返回错误")
            } else {
                format!("{label} GraphQL 返回错误：{message}")
            });
        }
    }
    value
        .get("data")
        .filter(|data| data.is_object())
        .ok_or_else(|| format!("{label} GraphQL 响应缺少 data"))
}

fn required_bool_field(value: &Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("GitHub JSON 缺少布尔字段 {field}"))
}

fn optional_u64_field(value: &Value, field: &str) -> Result<Option<u64>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("GitHub JSON 字段 {field} 不是有效整数")),
    }
}

fn optional_string_field(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .map(Some)
            .ok_or_else(|| format!("GitHub JSON 字段 {field} 不是有效字符串")),
    }
}

fn validate_github_review_line(label: &str, line: Option<u64>) -> Result<Option<u64>, String> {
    if line == Some(0) {
        return Err(format!("{label}必须大于 0"));
    }
    Ok(line)
}

fn validate_github_review_side(label: &str, side: &str) -> Result<String, String> {
    if !matches!(side, "LEFT" | "RIGHT") {
        return Err(format!("{label}必须是 LEFT 或 RIGHT"));
    }
    Ok(side.to_owned())
}

fn parse_github_page_next_cursor(value: &Value, label: &str) -> Result<Option<String>, String> {
    let has_next_page = required_bool_field(value, "hasNextPage")?;
    let end_cursor = optional_string_field(value, "endCursor")?;
    if has_next_page {
        let cursor = end_cursor
            .as_deref()
            .ok_or_else(|| format!("{label}仍有下一页但缺少 endCursor"))?;
        return validate_github_review_cursor(Some(cursor));
    }
    Ok(None)
}

fn parse_github_review_comment(value: &Value) -> Result<GithubReviewComment, String> {
    let id = validate_github_node_id("GitHub 审阅评论 ID", &required_string_field(value, "id")?)?;
    let author = match value.get("author") {
        None | Some(Value::Null) => None,
        Some(author) => optional_string_field(author, "login")?,
    };
    let reply_to_id = match value.get("replyTo") {
        None | Some(Value::Null) => None,
        Some(reply_to) => Some(validate_github_node_id(
            "GitHub 审阅回复目标 ID",
            &required_string_field(reply_to, "id")?,
        )?),
    };
    Ok(GithubReviewComment {
        id,
        author,
        body: required_string_field(value, "body")?,
        created_at: required_string_field(value, "createdAt")?,
        updated_at: required_string_field(value, "updatedAt")?,
        url: required_string_field(value, "url")?,
        reply_to_id,
    })
}

fn parse_github_review_thread(value: &Value) -> Result<GithubReviewThread, String> {
    let comments = value
        .get("comments")
        .filter(|comments| comments.is_object())
        .ok_or_else(|| "GitHub 审阅线程缺少 comments".to_owned())?;
    let comments_total_count = required_u64_field(comments, "totalCount")?;
    let comment_nodes = comments
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "GitHub 审阅线程 comments 缺少 nodes".to_owned())?;
    if comment_nodes.len() > MAX_GITHUB_REVIEW_COMMENTS_PER_THREAD
        || comment_nodes.len() as u64 > comments_total_count
    {
        return Err("GitHub 审阅线程评论数量与分页元数据不一致".into());
    }
    let parsed_comments = comment_nodes
        .iter()
        .map(parse_github_review_comment)
        .collect::<Result<Vec<_>, _>>()?;
    let comments_page_info = comments
        .get("pageInfo")
        .filter(|page_info| page_info.is_object())
        .ok_or_else(|| "GitHub 审阅线程 comments 缺少 pageInfo".to_owned())?;
    let comments_next_cursor =
        parse_github_page_next_cursor(comments_page_info, "GitHub 审阅评论")?;
    let diff_side = validate_github_review_side(
        "GitHub 审阅线程 diffSide",
        &required_string_field(value, "diffSide")?,
    )?;
    let start_diff_side = optional_string_field(value, "startDiffSide")?
        .map(|side| validate_github_review_side("GitHub 审阅线程 startDiffSide", &side))
        .transpose()?;
    let path = validate_relative_path(&required_string_field(value, "path")?)?;
    if path == "." {
        return Err("GitHub 审阅线程必须指定文件路径".into());
    }
    Ok(GithubReviewThread {
        id: validate_github_node_id("GitHub 审阅线程 ID", &required_string_field(value, "id")?)?,
        path,
        line: validate_github_review_line(
            "GitHub 审阅线程 line",
            optional_u64_field(value, "line")?,
        )?,
        start_line: validate_github_review_line(
            "GitHub 审阅线程 startLine",
            optional_u64_field(value, "startLine")?,
        )?,
        diff_side,
        start_diff_side,
        original_line: validate_github_review_line(
            "GitHub 审阅线程 originalLine",
            optional_u64_field(value, "originalLine")?,
        )?,
        original_start_line: validate_github_review_line(
            "GitHub 审阅线程 originalStartLine",
            optional_u64_field(value, "originalStartLine")?,
        )?,
        is_resolved: required_bool_field(value, "isResolved")?,
        is_outdated: required_bool_field(value, "isOutdated")?,
        viewer_can_reply: required_bool_field(value, "viewerCanReply")?,
        viewer_can_resolve: required_bool_field(value, "viewerCanResolve")?,
        viewer_can_unresolve: required_bool_field(value, "viewerCanUnresolve")?,
        comments: parsed_comments,
        comments_total_count,
        comments_next_cursor,
    })
}

fn parse_github_review_threads_response(
    bytes: &[u8],
    expected_number: u64,
    expected_head_oid: &str,
    page_size: u16,
) -> Result<GithubReviewThreadsResult, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅线程 JSON 无效: {error}"))?;
    let data = github_graphql_data(&value, "读取 GitHub 审阅线程")?;
    let repository = data
        .get("repository")
        .filter(|repository| repository.is_object())
        .ok_or_else(|| "GitHub 仓库或 Pull Request 不存在".to_owned())?;
    let pull_request = repository
        .get("pullRequest")
        .filter(|pull_request| pull_request.is_object())
        .ok_or_else(|| "GitHub Pull Request 不存在".to_owned())?;
    let number = required_u64_field(pull_request, "number")?;
    if number != expected_number {
        return Err("GitHub 审阅线程响应的 Pull Request 编号不匹配".into());
    }
    normalize_github_pull_request_state(
        "GitHub Pull Request 当前状态",
        &required_string_field(pull_request, "state")?,
    )?;
    let head_ref_oid = validate_object_id(
        "Pull Request head commit",
        required_string_field(pull_request, "headRefOid")?,
    )?;
    if head_ref_oid != expected_head_oid {
        return Err(format!(
            "Pull Request #{expected_number} head 已在读取审阅线程前发生变化；请刷新后重试"
        ));
    }
    let connection = pull_request
        .get("reviewThreads")
        .filter(|connection| connection.is_object())
        .ok_or_else(|| "GitHub Pull Request 缺少 reviewThreads".to_owned())?;
    let total_count = required_u64_field(connection, "totalCount")?;
    let nodes = connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "GitHub reviewThreads 缺少 nodes".to_owned())?;
    if nodes.len() > usize::from(page_size) || nodes.len() as u64 > total_count {
        return Err("GitHub 审阅线程数量与分页元数据不一致".into());
    }
    let threads = nodes
        .iter()
        .map(parse_github_review_thread)
        .collect::<Result<Vec<_>, _>>()?;
    let page_info = connection
        .get("pageInfo")
        .filter(|page_info| page_info.is_object())
        .ok_or_else(|| "GitHub reviewThreads 缺少 pageInfo".to_owned())?;
    let next_cursor = parse_github_page_next_cursor(page_info, "GitHub 审阅线程")?;
    Ok(GithubReviewThreadsResult {
        number,
        head_ref_oid,
        threads,
        total_count,
        next_cursor,
    })
}

fn parse_github_review_thread_comments_response(
    bytes: &[u8],
    expected_number: u64,
    expected_head_oid: &str,
    expected_thread_id: &str,
    page_size: u16,
    request_cursor: Option<&str>,
) -> Result<GithubReviewThreadCommentsResult, String> {
    validate_pr_number(expected_number)?;
    validate_github_review_comments_page_size(page_size)?;
    let head_ref_oid =
        validate_object_id("Pull Request head commit", expected_head_oid.to_owned())?;
    let thread_id = validate_github_node_id("GitHub 审阅线程 ID", expected_thread_id)?;
    let request_cursor = validate_github_review_cursor(request_cursor)?;
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅线程评论 JSON 无效: {error}"))?;
    let data = github_graphql_data(&value, "读取 GitHub 审阅线程评论")?;
    let node = data
        .get("node")
        .filter(|node| node.is_object())
        .ok_or_else(|| "GitHub 审阅线程不存在".to_owned())?;
    if required_string_field(node, "__typename")? != "PullRequestReviewThread" {
        return Err("GitHub 节点不是 Pull Request 审阅线程".into());
    }
    let response_thread_id =
        validate_github_node_id("GitHub 审阅线程 ID", &required_string_field(node, "id")?)?;
    if response_thread_id != thread_id {
        return Err("GitHub 审阅线程评论响应 ID 不匹配".into());
    }
    let connection = node
        .get("comments")
        .filter(|comments| comments.is_object())
        .ok_or_else(|| "GitHub 审阅线程评论响应缺少 comments".to_owned())?;
    let total_count = required_u64_field(connection, "totalCount")?;
    let nodes = connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "GitHub 审阅线程评论响应缺少 nodes".to_owned())?;
    if nodes.len() > usize::from(page_size) || nodes.len() as u64 > total_count {
        return Err("GitHub 审阅评论数量与分页元数据不一致".into());
    }
    let comments = nodes
        .iter()
        .map(parse_github_review_comment)
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen_comment_ids = HashSet::with_capacity(comments.len());
    if comments
        .iter()
        .any(|comment| !seen_comment_ids.insert(comment.id.as_str()))
    {
        return Err("GitHub 审阅评论页面包含重复评论 ID".into());
    }
    let page_info = connection
        .get("pageInfo")
        .filter(|page_info| page_info.is_object())
        .ok_or_else(|| "GitHub 审阅线程评论响应缺少 pageInfo".to_owned())?;
    let next_cursor = parse_github_page_next_cursor(page_info, "GitHub 审阅评论")?;
    if next_cursor.as_deref() == request_cursor.as_deref() && next_cursor.is_some() {
        return Err("GitHub 审阅评论分页游标未前进；已停止加载".into());
    }
    if next_cursor.is_some() && comments.is_empty() {
        return Err("GitHub 审阅评论分页返回空页面但仍声明存在下一页".into());
    }
    if next_cursor.is_some() && comments.len() as u64 >= total_count {
        return Err("GitHub 审阅评论分页声明与 totalCount 不一致".into());
    }
    if request_cursor.is_none() && next_cursor.is_none() && comments.len() as u64 != total_count {
        return Err("GitHub 审阅评论首末页数量与 totalCount 不一致".into());
    }
    Ok(GithubReviewThreadCommentsResult {
        number: expected_number,
        head_ref_oid,
        thread_id,
        comments,
        total_count,
        next_cursor,
    })
}

#[derive(Clone, Copy)]
enum GithubReviewThreadPermission {
    Reply,
    Resolve,
    Unresolve,
}

fn github_review_thread_scope_input(thread_id: &str) -> Result<Vec<u8>, String> {
    let thread_id = validate_github_node_id("GitHub 审阅线程 ID", thread_id)?;
    serde_json::to_vec(&serde_json::json!({
        "query": GITHUB_REVIEW_THREAD_SCOPE_QUERY,
        "variables": { "threadId": thread_id }
    }))
    .map_err(|error| format!("无法编码 GitHub 审阅线程范围请求: {error}"))
}

fn parse_github_review_thread_scope_response(
    bytes: &[u8],
    expected_thread_id: &str,
) -> Result<GithubReviewThreadScope, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅线程范围 JSON 无效: {error}"))?;
    let data = github_graphql_data(&value, "读取 GitHub 审阅线程范围")?;
    let node = data
        .get("node")
        .filter(|node| node.is_object())
        .ok_or_else(|| "GitHub 审阅线程不存在".to_owned())?;
    if required_string_field(node, "__typename")? != "PullRequestReviewThread" {
        return Err("GitHub 节点不是 Pull Request 审阅线程".into());
    }
    let id = validate_github_node_id("GitHub 审阅线程 ID", &required_string_field(node, "id")?)?;
    if id != expected_thread_id {
        return Err("GitHub 审阅线程范围响应 ID 不匹配".into());
    }
    let repository = node
        .get("repository")
        .filter(|repository| repository.is_object())
        .ok_or_else(|| "GitHub 审阅线程缺少 repository 范围".to_owned())?;
    let pull_request = node
        .get("pullRequest")
        .filter(|pull_request| pull_request.is_object())
        .ok_or_else(|| "GitHub 审阅线程缺少 Pull Request 范围".to_owned())?;
    let owner = repository
        .get("owner")
        .filter(|owner| owner.is_object())
        .ok_or_else(|| "GitHub 审阅线程 repository 缺少 owner".to_owned())?;
    Ok(GithubReviewThreadScope {
        repository_owner: required_string_field(owner, "login")?,
        repository_name: required_string_field(repository, "name")?,
        pull_request_number: required_u64_field(pull_request, "number")?,
        pull_request_state: normalize_github_pull_request_state(
            "GitHub Pull Request 当前状态",
            &required_string_field(pull_request, "state")?,
        )?,
        head_ref_oid: validate_object_id(
            "Pull Request head commit",
            required_string_field(pull_request, "headRefOid")?,
        )?,
        viewer_can_reply: required_bool_field(node, "viewerCanReply")?,
        viewer_can_resolve: required_bool_field(node, "viewerCanResolve")?,
        viewer_can_unresolve: required_bool_field(node, "viewerCanUnresolve")?,
    })
}

fn github_review_thread_scope_for_resolved(
    resolved: &ResolvedGithub,
    thread_id: &str,
) -> Result<GithubReviewThreadScope, String> {
    let gh = resolved
        .gh
        .as_deref()
        .ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        github_graphql_args(&repository.host),
        Some(github_review_thread_scope_input(thread_id)?),
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub 审阅线程范围", &output)?;
    if output.stdout_truncated {
        return Err("GitHub 审阅线程范围响应超过安全上限".into());
    }
    parse_github_review_thread_scope_response(&output.stdout, thread_id)
}

fn validate_github_review_thread_scope(
    scope: &GithubReviewThreadScope,
    repository: &GithubRepository,
    expected_number: u64,
    expected_head_oid: &str,
    expected_state: &str,
    permission: GithubReviewThreadPermission,
) -> Result<(), String> {
    validate_github_review_thread_scope_identity(
        scope,
        repository,
        expected_number,
        expected_head_oid,
        expected_state,
    )?;
    if scope.pull_request_state != "open" {
        return Err(format!(
            "Pull Request #{expected_number} 当前不是 open 状态，已拒绝审阅写操作"
        ));
    }
    let allowed = match permission {
        GithubReviewThreadPermission::Reply => scope.viewer_can_reply,
        GithubReviewThreadPermission::Resolve => scope.viewer_can_resolve,
        GithubReviewThreadPermission::Unresolve => scope.viewer_can_unresolve,
    };
    if !allowed {
        return Err("当前 GitHub 账号没有执行此审阅线程操作的权限".into());
    }
    Ok(())
}

fn validate_github_review_thread_scope_identity(
    scope: &GithubReviewThreadScope,
    repository: &GithubRepository,
    expected_number: u64,
    expected_head_oid: &str,
    expected_state: &str,
) -> Result<(), String> {
    let expected_head_oid =
        validate_object_id("Pull Request head commit", expected_head_oid.to_owned())?;
    let expected_state =
        normalize_github_pull_request_state("Pull Request 预期状态", expected_state)?;
    if !scope
        .repository_owner
        .eq_ignore_ascii_case(&repository.owner)
        || !scope.repository_name.eq_ignore_ascii_case(&repository.name)
        || scope.pull_request_number != expected_number
    {
        return Err("GitHub 审阅线程不属于已确认的仓库或 Pull Request".into());
    }
    if scope.head_ref_oid != expected_head_oid || scope.pull_request_state != expected_state {
        return Err(format!(
            "Pull Request #{expected_number} 已在确认后发生变化；请刷新审阅线程后重试"
        ));
    }
    Ok(())
}

fn validate_github_review_body(
    label: &str,
    body: Option<String>,
) -> Result<Option<String>, String> {
    let Some(body) = body else {
        return Ok(None);
    };
    if body.contains('\0') || body.chars().count() > MAX_GITHUB_REVIEW_BODY_CHARS {
        return Err(format!(
            "{label}不能包含 NUL，且不能超过 {MAX_GITHUB_REVIEW_BODY_CHARS} 个字符"
        ));
    }
    if body.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(body))
}

fn validate_required_github_review_body(label: &str, body: String) -> Result<String, String> {
    validate_github_review_body(label, Some(body))?.ok_or_else(|| format!("{label}不能为空"))
}

fn github_review_submission_args(repository: &GithubRepository, number: u64) -> Vec<OsString> {
    vec![
        OsString::from("api"),
        OsString::from("--hostname"),
        OsString::from(&repository.host),
        OsString::from("-X"),
        OsString::from("POST"),
        OsString::from(format!(
            "repos/{}/{}/pulls/{number}/reviews",
            repository.owner, repository.name
        )),
        OsString::from("--input"),
        OsString::from("-"),
    ]
}

fn github_review_submission_input(
    expected_head_oid: &str,
    event: GithubPullRequestReviewEvent,
    body: Option<String>,
    comments: Vec<GithubReviewSubmissionComment>,
) -> Result<Vec<u8>, String> {
    let expected_head_oid =
        validate_object_id("Pull Request head commit", expected_head_oid.to_owned())?;
    if comments.len() > MAX_GITHUB_REVIEW_SUBMISSION_COMMENTS {
        return Err(format!(
            "一次最多提交 {MAX_GITHUB_REVIEW_SUBMISSION_COMMENTS} 条行内审阅评论"
        ));
    }
    let body = validate_github_review_body("GitHub 审阅正文", body)?;
    if matches!(
        event,
        GithubPullRequestReviewEvent::Comment | GithubPullRequestReviewEvent::RequestChanges
    ) && body.is_none()
    {
        return Err(format!(
            "{} 审阅必须填写正文",
            match event {
                GithubPullRequestReviewEvent::Comment => "COMMENT",
                GithubPullRequestReviewEvent::RequestChanges => "REQUEST_CHANGES",
                GithubPullRequestReviewEvent::Approve => unreachable!(),
            }
        ));
    }
    let comments = comments
        .into_iter()
        .map(|comment| {
            let path = validate_relative_path(&comment.path)?;
            if path == "." {
                return Err("GitHub 行内审阅评论必须指定文件路径".to_owned());
            }
            if comment.line == 0 || comment.line > i32::MAX as u64 {
                return Err("GitHub 行内审阅评论 line 无效".into());
            }
            let side = validate_github_review_side("GitHub 行内审阅评论 side", &comment.side)?;
            let body =
                validate_required_github_review_body("GitHub 行内审阅评论正文", comment.body)?;
            Ok(serde_json::json!({
                "path": path,
                "line": comment.line,
                "side": side,
                "body": body
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let event = match event {
        GithubPullRequestReviewEvent::Comment => "COMMENT",
        GithubPullRequestReviewEvent::Approve => "APPROVE",
        GithubPullRequestReviewEvent::RequestChanges => "REQUEST_CHANGES",
    };
    let mut payload = serde_json::Map::new();
    payload.insert("commit_id".into(), Value::String(expected_head_oid));
    payload.insert("event".into(), Value::String(event.into()));
    if let Some(body) = body {
        payload.insert("body".into(), Value::String(body));
    }
    if !comments.is_empty() {
        payload.insert("comments".into(), Value::Array(comments));
    }
    serde_json::to_vec(&Value::Object(payload))
        .map_err(|error| format!("无法编码 GitHub 审阅提交请求: {error}"))
}

fn parse_github_review_submission_response(bytes: &[u8]) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅提交响应 JSON 无效: {error}"))?;
    let id = required_u64_field(&value, "id")?;
    if id == 0 {
        return Err("GitHub 审阅提交响应的 id 无效".into());
    }
    required_string_field(&value, "state")?;
    Ok(Some(format!("GitHub 审阅 #{id} 已提交")))
}

fn github_review_thread_mutation_input(
    query: &str,
    thread_id: &str,
    body: Option<&str>,
) -> Result<Vec<u8>, String> {
    let thread_id = validate_github_node_id("GitHub 审阅线程 ID", thread_id)?;
    let mut variables = serde_json::Map::new();
    variables.insert("threadId".into(), Value::String(thread_id));
    if let Some(body) = body {
        variables.insert("body".into(), Value::String(body.to_owned()));
    }
    serde_json::to_vec(&serde_json::json!({
        "query": query,
        "variables": variables
    }))
    .map_err(|error| format!("无法编码 GitHub 审阅线程 mutation: {error}"))
}

fn parse_github_review_thread_reply_response(bytes: &[u8]) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅线程回复 JSON 无效: {error}"))?;
    let data = github_graphql_data(&value, "回复 GitHub 审阅线程")?;
    let comment = data
        .get("addPullRequestReviewThreadReply")
        .and_then(|result| result.get("comment"))
        .filter(|comment| comment.is_object())
        .ok_or_else(|| "GitHub 审阅线程回复响应缺少 comment".to_owned())?;
    validate_github_node_id("GitHub 审阅评论 ID", &required_string_field(comment, "id")?)?;
    let url = required_string_field(comment, "url")?;
    Ok(Some(format!("GitHub 审阅回复已提交：{url}")))
}

fn parse_github_review_thread_resolution_response(
    bytes: &[u8],
    expected_thread_id: &str,
    resolved: bool,
) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 审阅线程状态 JSON 无效: {error}"))?;
    let data = github_graphql_data(
        &value,
        if resolved {
            "解决 GitHub 审阅线程"
        } else {
            "取消解决 GitHub 审阅线程"
        },
    )?;
    let field = if resolved {
        "resolveReviewThread"
    } else {
        "unresolveReviewThread"
    };
    let thread = data
        .get(field)
        .and_then(|result| result.get("thread"))
        .filter(|thread| thread.is_object())
        .ok_or_else(|| format!("GitHub {field} 响应缺少 thread"))?;
    let id = validate_github_node_id("GitHub 审阅线程 ID", &required_string_field(thread, "id")?)?;
    if id != expected_thread_id || required_bool_field(thread, "isResolved")? != resolved {
        return Err("GitHub 审阅线程 mutation 响应与请求状态不一致".into());
    }
    Ok(Some(if resolved {
        "GitHub 审阅线程已解决".into()
    } else {
        "GitHub 审阅线程已恢复为未解决".into()
    }))
}

fn github_merge_api_args(repository: &GithubRepository, number: u64) -> Vec<OsString> {
    vec![
        OsString::from("api"),
        OsString::from("--hostname"),
        OsString::from(&repository.host),
        OsString::from("-X"),
        OsString::from("PUT"),
        OsString::from(format!(
            "repos/{}/{}/pulls/{number}/merge",
            repository.owner, repository.name
        )),
        OsString::from("--input"),
        OsString::from("-"),
    ]
}

fn github_merge_api_input(
    expected_head_oid: &str,
    method: GithubMergeMethod,
) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&serde_json::json!({
        "sha": expected_head_oid,
        "merge_method": github_merge_method_name(method)
    }))
    .map_err(|error| format!("无法编码 GitHub 合并请求: {error}"))
}

fn github_merge_method_name(method: GithubMergeMethod) -> &'static str {
    match method {
        GithubMergeMethod::Merge => "merge",
        GithubMergeMethod::Squash => "squash",
        GithubMergeMethod::Rebase => "rebase",
    }
}

fn validate_github_merge_delete_branch(delete_branch: bool) -> Result<(), String> {
    if delete_branch {
        return Err("即时 REST 合并暂不支持同时删除分支；请在确认合并后单独删除分支".into());
    }
    Ok(())
}

fn github_required_check_succeeded(check: &GithubPullRequestReadinessCheck) -> bool {
    if !check.required {
        return true;
    }
    match check.kind.as_str() {
        "CheckRun" => {
            check.state == "COMPLETED"
                && matches!(
                    check.conclusion.as_deref(),
                    Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
                )
        }
        "StatusContext" => check.state == "SUCCESS",
        _ => false,
    }
}

fn validate_github_merge_readiness(
    readiness: &GithubPullRequestReadiness,
    expected_repository: &GithubRepositoryIdentity,
    expected_viewer_login: &str,
    expected_number: u64,
    expected_head_oid: &str,
    expected_base_oid: &str,
    expected_state: &str,
    expected_identity_revision: &str,
    expected_readiness_revision: &str,
    method: GithubMergeMethod,
) -> Result<String, String> {
    validate_github_identity_value("GitHub host", &expected_repository.host)?;
    validate_github_identity_value("GitHub owner", &expected_repository.owner)?;
    validate_github_identity_value("GitHub repository", &expected_repository.name)?;
    validate_github_identity_value("GitHub 登录账号", expected_viewer_login)?;
    validate_pr_number(expected_number)?;
    let expected_head_oid =
        validate_object_id("Pull Request head commit", expected_head_oid.to_owned())?;
    let expected_base_oid =
        validate_object_id("Pull Request base commit", expected_base_oid.to_owned())?;
    let expected_state =
        normalize_github_pull_request_state("Pull Request 预期状态", expected_state)?;
    let expected_identity_revision =
        validate_revision_token("GitHub PR identity revision", expected_identity_revision)?;
    let expected_readiness_revision =
        validate_revision_token("GitHub PR readiness revision", expected_readiness_revision)?;
    let expected_name_with_owner =
        format!("{}/{}", expected_repository.owner, expected_repository.name);
    let identity = &readiness.identity;

    if !identity
        .repository
        .host
        .eq_ignore_ascii_case(&expected_repository.host)
        || !identity
            .repository
            .name_with_owner
            .eq_ignore_ascii_case(&expected_name_with_owner)
        || identity.repository.node_id != identity.base_repository.node_id
        || !identity
            .base_repository
            .host
            .eq_ignore_ascii_case(&identity.repository.host)
        || !identity
            .base_repository
            .name_with_owner
            .eq_ignore_ascii_case(&identity.repository.name_with_owner)
        || identity.number != expected_number
    {
        return Err("GitHub 仓库或 Pull Request 身份已在确认后发生变化；请刷新后重新确认".into());
    }
    if expected_state != "open" || identity.state != "open" || identity.draft {
        return Err(format!(
            "Pull Request #{expected_number} 当前不是可立即合并的非草稿 open 状态"
        ));
    }
    if identity.head_ref_oid != expected_head_oid || identity.base_ref_oid != expected_base_oid {
        return Err(format!(
            "Pull Request #{expected_number} 的 base/head 已在确认后发生变化；请刷新后重新确认"
        ));
    }
    if readiness.identity_revision != expected_identity_revision
        || readiness.readiness_revision != expected_readiness_revision
    {
        return Err(format!(
            "Pull Request #{expected_number} readiness 已在确认后发生变化；请刷新后重新确认"
        ));
    }
    if !readiness
        .viewer
        .login
        .eq_ignore_ascii_case(expected_viewer_login)
    {
        return Err("GitHub 登录账号已在 readiness 确认后发生变化；请刷新后重新确认".into());
    }
    if readiness.merge_queue.availability != GithubReadinessAvailability::Available
        || readiness.merge_queue.error.is_some()
    {
        return Err("无法确认目标分支是否要求 GitHub merge queue；已拒绝即时 REST 合并".into());
    }
    let merge_queue = readiness.merge_queue.value.as_ref().ok_or_else(|| {
        "GitHub merge queue phase 缺少可验证结果；已拒绝即时 REST 合并".to_owned()
    })?;
    if merge_queue.enabled {
        return Err("目标分支要求 GitHub merge queue；不能执行即时 REST 合并".into());
    }
    if merge_queue.is_in_queue || merge_queue.entry.is_some() {
        return Err("Pull Request 已处于 GitHub merge queue；不能执行即时 REST 合并".into());
    }

    let method_allowed = match method {
        GithubMergeMethod::Merge => readiness.merge_policy.merge_commit_allowed,
        GithubMergeMethod::Squash => readiness.merge_policy.squash_merge_allowed,
        GithubMergeMethod::Rebase => readiness.merge_policy.rebase_merge_allowed,
    };
    if !method_allowed {
        return Err(format!(
            "GitHub 仓库策略不允许 {} 合并方式",
            github_merge_method_name(method)
        ));
    }
    if readiness.merge_policy.mergeable != "MERGEABLE" {
        return Err(format!(
            "Pull Request #{expected_number} mergeable 状态为 {}，已拒绝立即合并",
            readiness.merge_policy.mergeable
        ));
    }
    match readiness.merge_policy.merge_state_status.as_str() {
        "CLEAN" | "HAS_HOOKS" => {
            if readiness.checks.availability == GithubReadinessAvailability::Available {
                if readiness.checks.error.is_some() {
                    return Err("GitHub required checks phase 状态矛盾；已拒绝立即合并".into());
                }
                let checks = readiness.checks.value.as_ref().ok_or_else(|| {
                    "GitHub required checks phase 缺少可验证结果；已拒绝立即合并".to_owned()
                })?;
                if checks.total_count != checks.checks.len() as u64 {
                    return Err("Pull Request required checks 结果不完整；已拒绝立即合并".into());
                }
                if checks
                    .checks
                    .iter()
                    .any(|check| !github_required_check_succeeded(check))
                {
                    return Err(
                        "Pull Request readiness 与未成功的 required check 矛盾；已拒绝立即合并"
                            .into(),
                    );
                }
            }
        }
        "UNSTABLE" => {
            if readiness.checks.availability != GithubReadinessAvailability::Available
                || readiness.checks.error.is_some()
            {
                return Err(
                    "Pull Request 为 UNSTABLE，但 required checks 无法完整读取；已拒绝立即合并"
                        .into(),
                );
            }
            let checks = readiness.checks.value.as_ref().ok_or_else(|| {
                "Pull Request 为 UNSTABLE，但 required checks 缺少可验证结果；已拒绝立即合并"
                    .to_owned()
            })?;
            if checks.total_count != checks.checks.len() as u64 {
                return Err("Pull Request required checks 结果不完整；已拒绝立即合并".into());
            }
            if checks
                .checks
                .iter()
                .any(|check| !github_required_check_succeeded(check))
            {
                return Err("Pull Request 仍有未成功终止的 required check；已拒绝立即合并".into());
            }
        }
        status => {
            return Err(format!(
                "Pull Request #{expected_number} mergeStateStatus 为 {status}，已拒绝立即合并"
            ));
        }
    }
    Ok(identity.head_ref_oid.clone())
}

fn parse_github_merge_response(bytes: &[u8]) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GitHub 合并响应 JSON 无效: {error}"))?;
    let merged = value
        .get("merged")
        .and_then(Value::as_bool)
        .ok_or_else(|| "GitHub 合并响应缺少 merged 状态".to_owned())?;
    let message = string_field(&value, "message")
        .map(|message| redact_sensitive_text(&message))
        .filter(|message| !message.trim().is_empty());
    if !merged {
        let suffix = message
            .as_deref()
            .map(|message| format!("：{}", message.chars().take(500).collect::<String>()))
            .unwrap_or_default();
        return Err(format!(
            "GitHub 未立即合并 Pull Request{suffix}；没有启用自动合并或进入合并队列"
        ));
    }
    Ok(message)
}

pub fn execute_github_action(
    workspace: &Path,
    action: GithubAction,
) -> Result<GithubActionResult, String> {
    let initially_resolved = require_github_repository(workspace)?;
    let lock = repository_lock(&initially_resolved.repository);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Re-resolve the remote, active gh account, and repository after acquiring
    // the write lease. A confirmation made for another repo/account must never
    // be replayed after either identity changes while waiting for the lock.
    let resolved = require_github_repository(workspace)?;
    if !same_path(
        &initially_resolved.repository.root,
        &resolved.repository.root,
    ) {
        return Err("Git 工作目录已在确认后发生变化；请刷新后重新确认".into());
    }
    let actual_repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    {
        let (expected_repository, expected_viewer_login) = action.identity_expectation();
        validate_github_write_identity(
            expected_repository,
            expected_viewer_login,
            actual_repository,
        )?;
    }
    let gh = resolved.gh.as_deref().expect("validated GitHub CLI");
    let selector = resolved
        .selector
        .as_deref()
        .expect("validated repository selector");
    let mut created_number = None;
    let mut response_kind = GithubActionResponseKind::Standard;
    let mut expected_checkout_head = None;
    let (args, input) = match action {
        GithubAction::CreatePullRequest {
            expected_repository: _,
            expected_viewer_login: _,
            expected_local_head_oid,
            expected_content_revision,
            title,
            body,
            base,
            head,
            draft,
        } => {
            let title = validate_gh_text("Pull Request 标题", title, 500)?;
            let body = body.unwrap_or_default();
            if body.chars().count() > MAX_COMMIT_MESSAGE_CHARS {
                return Err(format!(
                    "Pull Request 正文不能超过 {MAX_COMMIT_MESSAGE_CHARS} 个字符"
                ));
            }
            let local = validate_github_local_state(
                &resolved.repository,
                &expected_local_head_oid,
                &expected_content_revision,
            )?;
            let base = validate_gh_ref(
                "base",
                base.ok_or_else(|| "创建 Pull Request 必须明确指定 base ref".to_owned())?,
            )?;
            let head = validate_gh_ref(
                "head",
                head.ok_or_else(|| "创建 Pull Request 必须明确指定 head ref".to_owned())?,
            )?;
            if local.branch.as_deref() != Some(head.as_str()) {
                return Err("Pull Request head ref 不再是当前本地分支；请刷新后重新确认".into());
            }
            let mut args = vec![
                OsString::from("pr"),
                OsString::from("create"),
                OsString::from("-R"),
                OsString::from(selector),
                OsString::from("--title"),
                OsString::from(title),
                OsString::from("--body-file"),
                OsString::from("-"),
                OsString::from("--base"),
                OsString::from(base),
                OsString::from("--head"),
                OsString::from(head),
            ];
            if draft {
                args.push(OsString::from("--draft"));
            }
            (args, Some(body.into_bytes()))
        }
        GithubAction::CheckoutPullRequest {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
            expected_local_head_oid,
            expected_content_revision,
        } => {
            validate_pr_number(number)?;
            validate_github_local_state(
                &resolved.repository,
                &expected_local_head_oid,
                &expected_content_revision,
            )?;
            let detail = github_pull_request_detail_for_resolved(&resolved, number)?;
            let expected_head_oid = validate_github_pull_request_expectation(
                &detail,
                &expected_head_oid,
                &expected_state,
                "open",
            )?;
            created_number = Some(number);
            expected_checkout_head = Some(expected_head_oid);
            (
                vec![
                    OsString::from("pr"),
                    OsString::from("checkout"),
                    OsString::from(number.to_string()),
                    OsString::from("-R"),
                    OsString::from(selector),
                ],
                None,
            )
        }
        GithubAction::MergePullRequest {
            expected_repository,
            expected_viewer_login,
            number,
            expected_head_oid,
            expected_base_oid,
            expected_state,
            expected_identity_revision,
            expected_readiness_revision,
            method,
            delete_branch,
        } => {
            validate_pr_number(number)?;
            validate_github_merge_delete_branch(delete_branch)?;
            let readiness = github_pull_request_readiness_for_resolved(&resolved, number)?;
            let expected_head_oid = validate_github_merge_readiness(
                &readiness,
                &expected_repository,
                &expected_viewer_login,
                number,
                &expected_head_oid,
                &expected_base_oid,
                &expected_state,
                &expected_identity_revision,
                &expected_readiness_revision,
                method,
            )?;
            created_number = Some(number);
            response_kind = GithubActionResponseKind::Merge;
            (
                github_merge_api_args(actual_repository, number),
                Some(github_merge_api_input(&expected_head_oid, method)?),
            )
        }
        GithubAction::ClosePullRequest {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
        } => {
            validate_pr_number(number)?;
            let detail = github_pull_request_detail_for_resolved(&resolved, number)?;
            validate_github_pull_request_expectation(
                &detail,
                &expected_head_oid,
                &expected_state,
                "open",
            )?;
            created_number = Some(number);
            (
                vec![
                    OsString::from("pr"),
                    OsString::from("close"),
                    OsString::from(number.to_string()),
                    OsString::from("-R"),
                    OsString::from(selector),
                ],
                None,
            )
        }
        GithubAction::ReopenPullRequest {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
        } => {
            validate_pr_number(number)?;
            let detail = github_pull_request_detail_for_resolved(&resolved, number)?;
            validate_github_pull_request_expectation(
                &detail,
                &expected_head_oid,
                &expected_state,
                "closed",
            )?;
            created_number = Some(number);
            (
                vec![
                    OsString::from("pr"),
                    OsString::from("reopen"),
                    OsString::from(number.to_string()),
                    OsString::from("-R"),
                    OsString::from(selector),
                ],
                None,
            )
        }
        GithubAction::SubmitPullRequestReview {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
            event,
            body,
            comments,
        } => {
            validate_pr_number(number)?;
            let detail = github_pull_request_detail_for_resolved(&resolved, number)?;
            let expected_head_oid = validate_github_pull_request_expectation(
                &detail,
                &expected_head_oid,
                &expected_state,
                "open",
            )?;
            created_number = Some(number);
            response_kind = GithubActionResponseKind::ReviewSubmission;
            (
                github_review_submission_args(actual_repository, number),
                Some(github_review_submission_input(
                    &expected_head_oid,
                    event,
                    body,
                    comments,
                )?),
            )
        }
        GithubAction::ReplyReviewThread {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
            thread_id,
            body,
        } => {
            validate_pr_number(number)?;
            let thread_id = validate_github_node_id("GitHub 审阅线程 ID", &thread_id)?;
            let body = validate_required_github_review_body("GitHub 审阅回复", body)?;
            let scope = github_review_thread_scope_for_resolved(&resolved, &thread_id)?;
            validate_github_review_thread_scope(
                &scope,
                actual_repository,
                number,
                &expected_head_oid,
                &expected_state,
                GithubReviewThreadPermission::Reply,
            )?;
            created_number = Some(number);
            response_kind = GithubActionResponseKind::ReviewThreadReply;
            (
                github_graphql_args(&actual_repository.host),
                Some(github_review_thread_mutation_input(
                    GITHUB_REPLY_REVIEW_THREAD_MUTATION,
                    &thread_id,
                    Some(&body),
                )?),
            )
        }
        GithubAction::ResolveReviewThread {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
            thread_id,
        } => {
            validate_pr_number(number)?;
            let thread_id = validate_github_node_id("GitHub 审阅线程 ID", &thread_id)?;
            let scope = github_review_thread_scope_for_resolved(&resolved, &thread_id)?;
            validate_github_review_thread_scope(
                &scope,
                actual_repository,
                number,
                &expected_head_oid,
                &expected_state,
                GithubReviewThreadPermission::Resolve,
            )?;
            created_number = Some(number);
            response_kind = GithubActionResponseKind::ReviewThreadResolution {
                thread_id: thread_id.clone(),
                resolved: true,
            };
            (
                github_graphql_args(&actual_repository.host),
                Some(github_review_thread_mutation_input(
                    GITHUB_RESOLVE_REVIEW_THREAD_MUTATION,
                    &thread_id,
                    None,
                )?),
            )
        }
        GithubAction::UnresolveReviewThread {
            expected_repository: _,
            expected_viewer_login: _,
            number,
            expected_head_oid,
            expected_state,
            thread_id,
        } => {
            validate_pr_number(number)?;
            let thread_id = validate_github_node_id("GitHub 审阅线程 ID", &thread_id)?;
            let scope = github_review_thread_scope_for_resolved(&resolved, &thread_id)?;
            validate_github_review_thread_scope(
                &scope,
                actual_repository,
                number,
                &expected_head_oid,
                &expected_state,
                GithubReviewThreadPermission::Unresolve,
            )?;
            created_number = Some(number);
            response_kind = GithubActionResponseKind::ReviewThreadResolution {
                thread_id: thread_id.clone(),
                resolved: false,
            };
            (
                github_graphql_args(&actual_repository.host),
                Some(github_review_thread_mutation_input(
                    GITHUB_UNRESOLVE_REVIEW_THREAD_MUTATION,
                    &thread_id,
                    None,
                )?),
            )
        }
    };
    let output = run_gh(
        gh,
        &resolved.repository.root,
        args,
        input,
        NETWORK_COMMAND_TIMEOUT,
        MAX_ACTION_OUTPUT,
    )?;
    let action_label = match &response_kind {
        GithubActionResponseKind::Standard => "执行 GitHub 操作",
        GithubActionResponseKind::Merge => "立即合并 GitHub Pull Request",
        GithubActionResponseKind::ReviewSubmission => "提交 GitHub Pull Request 审阅",
        GithubActionResponseKind::ReviewThreadReply => "回复 GitHub 审阅线程",
        GithubActionResponseKind::ReviewThreadResolution { resolved: true, .. } => {
            "解决 GitHub 审阅线程"
        }
        GithubActionResponseKind::ReviewThreadResolution {
            resolved: false, ..
        } => "取消解决 GitHub 审阅线程",
    };
    require_success(action_label, &output)?;
    if !matches!(&response_kind, GithubActionResponseKind::Standard) && output.stdout_truncated {
        return Err(format!("{action_label}响应超过安全上限，无法确认操作结果"));
    }
    let message = match &response_kind {
        GithubActionResponseKind::Standard => {
            let message = output.display_output();
            (!message.is_empty()).then_some(message)
        }
        GithubActionResponseKind::Merge => parse_github_merge_response(&output.stdout)?,
        GithubActionResponseKind::ReviewSubmission => {
            parse_github_review_submission_response(&output.stdout)?
        }
        GithubActionResponseKind::ReviewThreadReply => {
            parse_github_review_thread_reply_response(&output.stdout)?
        }
        GithubActionResponseKind::ReviewThreadResolution {
            thread_id,
            resolved,
        } => parse_github_review_thread_resolution_response(&output.stdout, thread_id, *resolved)?,
    };
    if created_number.is_none() {
        created_number =
            extract_pull_request_number(String::from_utf8_lossy(&output.stdout).trim());
    }
    let snapshot = if let Some(expected_head_oid) = expected_checkout_head {
        let snapshot = snapshot_for_repository(&resolved.repository)?;
        if snapshot.head.as_deref() != Some(expected_head_oid.as_str()) {
            return Err(
                "GitHub CLI 检出的本地 HEAD 与已确认的 Pull Request head 不一致；请检查工作区"
                    .into(),
            );
        }
        Some(bounded_workspace_snapshot(snapshot))
    } else {
        None
    };
    let pull_request = created_number
        .and_then(|number| github_pull_request_detail_for_resolved(&resolved, number).ok());
    Ok(GithubActionResult {
        repository: resolved.context.repository,
        pull_request,
        snapshot,
        message,
    })
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
    let index_path = index_parent.join("index");
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
        network_policy: GitNetworkPolicy::Restricted,
        root,
        git_dir,
        repository_id,
        worktree_id,
        git_common_dir,
        index_path,
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
    let mut warnings = Vec::new();
    let nested_submodule_changes = parsed
        .changes
        .iter()
        .filter(|change| {
            change.submodule && (change.submodule_modified || change.submodule_untracked)
        })
        .count();
    if nested_submodule_changes > 0 {
        warnings.push(format!(
            "{nested_submodule_changes} 个子模块包含内部未提交变更；请将子模块目录作为独立工作区处理"
        ));
    }
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
    let mut remotes = transports
        .iter()
        .map(|transport| transport.proof.clone())
        .collect::<Vec<_>>();
    if upstream_target
        .as_ref()
        .is_some_and(|target| target.is_local)
    {
        remotes.push(local_remote_proof());
        remotes.sort_by(|left, right| left.name.cmp(&right.name));
        remotes.dedup_by(|left, right| left.name == right.name);
    }
    let remote = preferred_git_remote(&transports, upstream_target.as_ref());
    let operation_state = repository_operation_state(&repository.git_dir)?;
    let operation = operation_state.as_ref().map(|state| state.operation);
    let operation_revision = operation_state.map(|state| state.revision);
    let branch = parsed.branch;
    let mut snapshot = GitWorkspaceSnapshot {
        repository_id: repository.repository_id.clone(),
        worktree_id: repository.worktree_id.clone(),
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
        git_version: repository.git_version.clone(),
        repository_root: repository.root.to_string_lossy().into_owned(),
        worktree_root: repository.root.to_string_lossy().into_owned(),
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
    Ok(snapshot)
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
    update_revision_component(&mut digest, b"staged-diff", &staged);
    update_revision_component(&mut digest, b"unstaged-diff", &unstaged);
    Ok(format!("{:x}", digest.finalize()))
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

fn lower_hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
    let mut per_file = if has_head {
        diff_numstat(repository, false)?
    } else {
        diff_numstat(repository, true)?
    };
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
    Ok(total)
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
                GitAction::Stage { .. }
                    | GitAction::StageAll { .. }
                    | GitAction::Unstage { .. }
                    | GitAction::UnstageAll { .. }
                    | GitAction::Discard { .. }
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

fn parse_history(bytes: &[u8]) -> Result<Vec<GitCommit>, String> {
    let mut commits = Vec::new();
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut index = 0;
    while index < fields.len() {
        while index < fields.len() && fields[index].is_empty() {
            index += 1;
        }
        if index >= fields.len() {
            break;
        }
        if fields.len().saturating_sub(index) < 10 {
            return Err(format!(
                "Git 历史记录字段数量无效（{}）",
                fields.len().saturating_sub(index)
            ));
        }
        let fields = &fields[index..index + 10];
        commits.push(GitCommit {
            oid: lossy(fields[0]),
            short_oid: lossy(fields[1]),
            parents: lossy(fields[2])
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect(),
            author_name: lossy(fields[3]),
            author_email: lossy(fields[4]),
            authored_at: lossy(fields[5]),
            committed_at: lossy(fields[6]),
            refs: lossy(fields[7])
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
            subject: lossy(fields[8]),
            body: fields.get(9).map(|value| lossy(value)).unwrap_or_default(),
        });
        index += 10;
    }
    Ok(commits)
}

fn ensure_branch_merged_into_head(
    repository: &Repository,
    expected_oid: &str,
    expected_head: &str,
) -> Result<(), String> {
    let output = run_git(
        repository,
        [
            OsString::from("merge-base"),
            OsString::from("--is-ancestor"),
            OsString::from(expected_oid),
            OsString::from(expected_head),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        16 * 1024,
        true,
    )?;
    if output.success() {
        Ok(())
    } else if !output.timed_out && output.exit_code() == Some(1) {
        Err("Git 分支尚未合并进当前 HEAD；拒绝删除".into())
    } else {
        Err(command_error("验证 Git 分支是否已合并", &output))
    }
}

fn ensure_branch_not_checked_out(repository: &Repository, full_ref: &str) -> Result<(), String> {
    let output = run_git(
        repository,
        [
            OsString::from("worktree"),
            OsString::from("list"),
            OsString::from("--porcelain"),
            OsString::from("-z"),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
        true,
    )?;
    require_success("读取 Git worktree 列表", &output)?;
    if output.stdout_truncated {
        return Err("Git worktree 列表超过安全上限，拒绝删除分支".into());
    }
    let mut worktree = None;
    for field in output.stdout.split(|byte| *byte == 0) {
        if field.is_empty() {
            worktree = None;
        } else if let Some(path) = strip_ascii_prefix(field, b"worktree ") {
            worktree = Some(lossy(path));
        } else if let Some(branch) = strip_ascii_prefix(field, b"branch ") {
            if lossy(branch) == full_ref {
                let worktree = worktree
                    .as_deref()
                    .ok_or_else(|| "Git worktree 列表缺少工作区路径".to_owned())?;
                return Err(format!("Git 分支正在 worktree {worktree} 中检出；拒绝删除"));
            }
        }
    }
    Ok(())
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
        GitAction::StageAll { .. } => Ok((
            vec![
                OsString::from("add"),
                OsString::from("--all"),
                OsString::from("--"),
                OsString::from("."),
            ],
            None,
            BULK_COMMAND_TIMEOUT,
        )),
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
        GitAction::UnstageAll { .. } => {
            let args = if repository_has_head(repository)? {
                vec![
                    OsString::from("restore"),
                    OsString::from("--staged"),
                    OsString::from("--"),
                    OsString::from("."),
                ]
            } else {
                vec![
                    OsString::from("rm"),
                    OsString::from("--cached"),
                    OsString::from("--ignore-unmatch"),
                    OsString::from("-r"),
                    OsString::from("--"),
                    OsString::from("."),
                ]
            };
            Ok((args, None, BULK_COMMAND_TIMEOUT))
        }
        GitAction::Discard { .. } => {
            unreachable!("discard is handled before preparing a single command")
        }
        GitAction::Commit { .. } => {
            unreachable!("commit is handled by the proof-bound transaction")
        }
        GitAction::CreateBranch {
            name,
            start_point,
            checkout,
        } => {
            validate_branch_name(repository, &name)?;
            let mut args = vec![OsString::from(if checkout { "switch" } else { "branch" })];
            if checkout {
                args.push(OsString::from("-c"));
            }
            args.push(OsString::from(name));
            if let Some(start_point) = start_point {
                args.push(OsString::from(resolve_commit(repository, &start_point)?));
            }
            Ok((args, None, LOCAL_COMMAND_TIMEOUT))
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
        GitAction::DeleteBranch {
            name,
            force,
            expected_head,
            expected_oid,
        } => {
            if force {
                return Err("当前后端不允许强制删除分支".into());
            }
            validate_local_branch(repository, &name)?;
            let expected_head = validate_object_id("仓库 HEAD", expected_head)?;
            let expected_oid = validate_object_id("Git 分支", expected_oid)?;
            ensure_branch_merged_into_head(repository, &expected_oid, &expected_head)?;
            ensure_branch_not_checked_out(repository, &format!("refs/heads/{name}"))?;
            let full_ref = format!("refs/heads/{name}");
            let input = format!(
                "start\nverify HEAD {expected_head}\ndelete {full_ref} {expected_oid}\nprepare\ncommit\n"
            )
            .into_bytes();
            Ok((
                vec![OsString::from("update-ref"), OsString::from("--stdin")],
                Some(input),
                LOCAL_COMMAND_TIMEOUT,
            ))
        }
        GitAction::Merge {
            branch,
            expected_head: _,
            expected_branch_oid,
        } => {
            validate_local_branch(repository, &branch)?;
            let expected_branch_oid = validate_object_id("待合并分支", expected_branch_oid)?;
            Ok((
                vec![
                    OsString::from("merge"),
                    OsString::from("--no-edit"),
                    OsString::from(expected_branch_oid),
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
        GitAction::Stash {
            message,
            include_untracked,
        } => {
            let mut args = vec![OsString::from("stash"), OsString::from("push")];
            if include_untracked {
                args.push(OsString::from("--include-untracked"));
            }
            if let Some(message) = message {
                let message = message.trim();
                if message.is_empty() {
                    return Err("stash 信息不能为空".into());
                }
                if message.chars().count() > MAX_COMMIT_MESSAGE_CHARS {
                    return Err(format!(
                        "stash 信息不能超过 {MAX_COMMIT_MESSAGE_CHARS} 个字符"
                    ));
                }
                args.push(OsString::from("--message"));
                args.push(OsString::from(message));
            }
            Ok((args, None, LOCAL_COMMAND_TIMEOUT))
        }
        GitAction::StashPop { index } => {
            let index = index.unwrap_or(0);
            if index > MAX_HISTORY_SKIP {
                return Err(format!("stash 索引不能超过 {MAX_HISTORY_SKIP}"));
            }
            Ok((
                vec![
                    OsString::from("stash"),
                    OsString::from("pop"),
                    OsString::from(format!("stash@{{{index}}}")),
                ],
                None,
                LOCAL_COMMAND_TIMEOUT,
            ))
        }
        GitAction::Fetch { .. } | GitAction::Pull { .. } | GitAction::Push { .. } => {
            unreachable!("network actions use proof-bound execution paths")
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitIndexBaseline {
    exists: bool,
    length: u64,
    sha256: [u8; 32],
}

impl GitIndexBaseline {
    fn revision(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"mework.git.index-baseline.v1\0");
        update_revision_component(&mut digest, b"exists", &[u8::from(self.exists)]);
        update_revision_component(&mut digest, b"length", &self.length.to_be_bytes());
        update_revision_component(&mut digest, b"sha256", &self.sha256);
        format!("{:x}", digest.finalize())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StageAllCandidateProof {
    target_revision: String,
    tree_oid: String,
    semantic_revision: String,
    index_length: u64,
    index_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitHeadState {
    oid: Option<String>,
    target_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitIdentityState {
    author: String,
    committer: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitCandidateProof {
    target_revision: String,
    tree_oid: String,
    semantic_revision: String,
    index_length: u64,
    index_sha256: [u8; 32],
    head: CommitHeadState,
    identity: CommitIdentityState,
    message_digest: String,
    sign_commit: bool,
}

struct StageAllCandidate {
    proof: StageAllCandidateProof,
    output: CliOutput,
}

struct StageAllIndexTransaction<'a> {
    repository: &'a Repository,
    index_path: PathBuf,
    lock_path: PathBuf,
    nested_lock_path: PathBuf,
    baseline: GitIndexBaseline,
    internal_mutation_started: bool,
    published: bool,
}

impl<'a> StageAllIndexTransaction<'a> {
    fn acquire(repository: &'a Repository) -> Result<Self, String> {
        let index_path = repository.index_path.clone();
        let lock_path = index_path.with_file_name("index.lock");
        let nested_lock_path = index_path.with_file_name("index.lock.lock");
        if nested_lock_path
            .try_exists()
            .map_err(|error| format!("无法检查 Git index 候选锁: {error}"))?
        {
            return Err("Git index 存在未完成的候选锁；未执行任何更改".into());
        }
        let reservation = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "Git index 正被其他进程锁定；未执行任何更改".to_owned()
                } else {
                    format!("无法创建 Git index 事务锁: {error}")
                }
            })?;
        drop(reservation);
        let baseline = match read_git_index_baseline(&index_path) {
            Ok(baseline) => baseline,
            Err(error) => {
                let _ = fs::remove_file(&lock_path);
                return Err(error);
            }
        };
        let mut transaction = Self {
            repository,
            index_path,
            lock_path,
            nested_lock_path,
            baseline,
            internal_mutation_started: false,
            published: false,
        };
        transaction.reset_candidate()?;
        Ok(transaction)
    }

    fn verify_real_index_unchanged(&self) -> Result<(), String> {
        let current = read_git_index_baseline(&self.index_path)?;
        if current != self.baseline {
            return Err("Git index 已被外部进程并发修改；候选暂存未发布，请刷新后重试".into());
        }
        Ok(())
    }

    fn reset_candidate(&mut self) -> Result<(), String> {
        self.verify_real_index_unchanged()?;
        if self
            .nested_lock_path
            .try_exists()
            .map_err(|error| format!("无法检查 Git index 候选锁: {error}"))?
        {
            return Err("Git index 候选事务残留嵌套锁；未执行任何更改".into());
        }
        let lock_metadata = fs::symlink_metadata(&self.lock_path)
            .map_err(|error| format!("无法检查 Git index 事务锁: {error}"))?;
        if !lock_metadata.is_file() || lock_metadata.file_type().is_symlink() {
            return Err("Git index 事务锁不是受支持的普通文件".into());
        }
        let mut candidate = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.lock_path)
            .map_err(|error| format!("无法重置 Git index 候选文件: {error}"))?;
        let source_modified = if self.baseline.exists {
            let source_metadata = fs::symlink_metadata(&self.index_path)
                .map_err(|error| format!("无法检查真实 Git index: {error}"))?;
            if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
                return Err("真实 Git index 不是受支持的普通文件".into());
            }
            let source_modified = source_metadata
                .modified()
                .map_err(|error| format!("无法读取真实 Git index 修改时间: {error}"))?;
            let mut source = fs::File::open(&self.index_path)
                .map_err(|error| format!("无法打开真实 Git index: {error}"))?;
            let copied = std::io::copy(&mut source, &mut candidate)
                .map_err(|error| format!("无法初始化 Git index 候选文件: {error}"))?;
            if copied != self.baseline.length {
                return Err("真实 Git index 在复制时发生变化；候选暂存未发布".into());
            }
            #[cfg(unix)]
            fs::set_permissions(&self.lock_path, source_metadata.permissions())
                .map_err(|error| format!("无法继承 Git index 权限: {error}"))?;
            Some(source_modified)
        } else {
            None
        };
        candidate
            .sync_all()
            .map_err(|error| format!("无法刷新 Git index 候选文件: {error}"))?;
        if let Some(modified) = source_modified {
            candidate
                .set_times(fs::FileTimes::new().set_modified(modified))
                .map_err(|error| format!("无法继承 Git index 修改时间: {error}"))?;
        }
        drop(candidate);
        if !self.baseline.exists {
            self.internal_mutation_started = true;
            let output = run_git_with_internal_index(
                self.repository,
                &self.lock_path,
                [OsString::from("read-tree"), OsString::from("--empty")],
                None,
                LOCAL_COMMAND_TIMEOUT,
                MAX_ACTION_OUTPUT,
                false,
            )?;
            require_success("初始化空 Git index 候选", &output)?;
        }
        self.verify_real_index_unchanged()
    }

    fn build_commit_candidate(
        &mut self,
        head: &CommitHeadState,
        identity: &CommitIdentityState,
        message_digest: &str,
        sign_commit: bool,
    ) -> Result<CommitCandidateProof, String> {
        self.reset_candidate()?;
        self.internal_mutation_started = true;
        let unsplit = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [
                OsString::from("update-index"),
                OsString::from("--no-split-index"),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        )?;
        require_success("展开 Git 提交候选索引", &unsplit)?;
        self.read_commit_candidate(head, identity, message_digest, sign_commit)
    }

    fn read_commit_candidate(
        &self,
        head: &CommitHeadState,
        identity: &CommitIdentityState,
        message_digest: &str,
        sign_commit: bool,
    ) -> Result<CommitCandidateProof, String> {
        self.verify_real_index_unchanged()?;
        if self
            .nested_lock_path
            .try_exists()
            .map_err(|error| format!("无法检查 Git 提交候选嵌套锁: {error}"))?
        {
            return Err("Git 提交候选仍被外部进程锁定；真实 index 保持不变".into());
        }
        let tree = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [OsString::from("write-tree")],
            None,
            LOCAL_COMMAND_TIMEOUT,
            4096,
            false,
        )?;
        require_success("写入 Git 提交候选树", &tree)?;
        if tree.stdout_truncated {
            return Err("Git 提交候选树响应超过安全上限".into());
        }
        let tree_oid = validate_object_id(
            "Git 提交候选树",
            String::from_utf8_lossy(&tree.stdout).trim().to_owned(),
        )?;
        let entries = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [
                OsString::from("ls-files"),
                OsString::from("--stage"),
                OsString::from("-z"),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            0,
            true,
        )?;
        require_success("读取 Git 提交候选语义", &entries)?;
        let semantic_revision = lower_hex_digest(&entries.stdout_sha256);
        let candidate_index = read_stage_all_candidate_identity(&self.lock_path)?;
        let mut digest = Sha256::new();
        digest.update(b"mework.git.commit-target-revision.v1\0");
        update_revision_component(
            &mut digest,
            b"repository-root",
            self.repository.root.as_os_str().as_encoded_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"worktree-git-dir",
            self.repository.git_dir.as_os_str().as_encoded_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"index-baseline-revision",
            self.baseline.revision().as_bytes(),
        );
        update_revision_component(&mut digest, b"candidate-tree-oid", tree_oid.as_bytes());
        update_revision_component(
            &mut digest,
            b"candidate-semantic-revision",
            semantic_revision.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"candidate-index-length",
            &candidate_index.length.to_be_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"candidate-index-sha256",
            &candidate_index.sha256,
        );
        update_revision_component(
            &mut digest,
            b"head-oid",
            head.oid.as_deref().unwrap_or_default().as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"head-target-ref",
            head.target_ref.as_deref().unwrap_or_default().as_bytes(),
        );
        update_revision_component(&mut digest, b"author-identity", identity.author.as_bytes());
        update_revision_component(
            &mut digest,
            b"committer-identity",
            identity.committer.as_bytes(),
        );
        update_revision_component(&mut digest, b"message-digest", message_digest.as_bytes());
        update_revision_component(&mut digest, b"sign-commit", &[u8::from(sign_commit)]);
        self.verify_real_index_unchanged()?;
        Ok(CommitCandidateProof {
            target_revision: format!("{:x}", digest.finalize()),
            tree_oid,
            semantic_revision,
            index_length: candidate_index.length,
            index_sha256: candidate_index.sha256,
            head: head.clone(),
            identity: identity.clone(),
            message_digest: message_digest.to_owned(),
            sign_commit,
        })
    }

    fn build_candidate(
        &mut self,
        snapshot: &GitWorkspaceSnapshot,
    ) -> Result<StageAllCandidate, String> {
        if !snapshot.files_complete {
            return Err("Git 全部暂存候选必须基于完整工作区状态".into());
        }
        self.reset_candidate()?;
        self.internal_mutation_started = true;
        let output = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [
                OsString::from("add"),
                OsString::from("--all"),
                OsString::from("--"),
                OsString::from("."),
            ],
            None,
            STAGE_ALL_TARGET_REVISION_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        )?;
        require_success("构建 Git 全部暂存候选", &output)?;
        let unsplit = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [
                OsString::from("update-index"),
                OsString::from("--no-split-index"),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            MAX_ACTION_OUTPUT,
            false,
        )?;
        require_success("展开 Git split index 候选", &unsplit)?;
        let tree = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [OsString::from("write-tree")],
            None,
            LOCAL_COMMAND_TIMEOUT,
            4096,
            false,
        )?;
        require_success("写入 Git 全部暂存候选树", &tree)?;
        if tree.stdout_truncated {
            return Err("Git 全部暂存候选树响应超过安全上限".into());
        }
        let tree_oid = validate_object_id(
            "Git 全部暂存候选树",
            String::from_utf8_lossy(&tree.stdout).trim().to_owned(),
        )?;
        let entries = run_git_with_internal_index(
            self.repository,
            &self.lock_path,
            [
                OsString::from("ls-files"),
                OsString::from("--stage"),
                OsString::from("-z"),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            0,
            true,
        )?;
        require_success("读取 Git 全部暂存候选语义", &entries)?;
        let semantic_revision = lower_hex_digest(&entries.stdout_sha256);
        let candidate_index = read_stage_all_candidate_identity(&self.lock_path)?;
        let mut digest = Sha256::new();
        digest.update(b"mework.git.stage-all-target-revision.v2-candidate-tree\0");
        update_revision_component(
            &mut digest,
            b"repository-root",
            self.repository.root.as_os_str().as_encoded_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"worktree-git-dir",
            self.repository.git_dir.as_os_str().as_encoded_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"content-revision",
            snapshot.content_revision.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"head",
            snapshot.head.as_deref().unwrap_or_default().as_bytes(),
        );
        update_revision_component(&mut digest, b"unborn", &[u8::from(snapshot.unborn)]);
        update_revision_component(
            &mut digest,
            b"index-baseline-revision",
            self.baseline.revision().as_bytes(),
        );
        update_revision_component(&mut digest, b"candidate-tree-oid", tree_oid.as_bytes());
        update_revision_component(
            &mut digest,
            b"candidate-semantic-revision",
            semantic_revision.as_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"candidate-index-length",
            &candidate_index.length.to_be_bytes(),
        );
        update_revision_component(
            &mut digest,
            b"candidate-index-sha256",
            &candidate_index.sha256,
        );
        self.verify_real_index_unchanged()?;
        Ok(StageAllCandidate {
            proof: StageAllCandidateProof {
                target_revision: format!("{:x}", digest.finalize()),
                tree_oid,
                semantic_revision,
                index_length: candidate_index.length,
                index_sha256: candidate_index.sha256,
            },
            output,
        })
    }

    fn publish(mut self, expected: &StageAllCandidateProof) -> Result<(), String> {
        if self
            .nested_lock_path
            .try_exists()
            .map_err(|error| format!("无法检查 Git index 候选锁: {error}"))?
        {
            return Err("Git index 候选仍被锁定，拒绝发布".into());
        }
        let metadata = fs::symlink_metadata(&self.lock_path)
            .map_err(|error| format!("无法检查 Git index 候选: {error}"))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_GIT_INDEX_BYTES
        {
            return Err("Git index 候选文件类型或大小不受支持".into());
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("无法刷新 Git index 候选: {error}"))?;
        let actual = read_stage_all_candidate_identity(&self.lock_path)?;
        if actual.length != expected.index_length || actual.sha256 != expected.index_sha256 {
            return Err(
                "Git index 候选在验证后被外部修改；真实 index 保持不变，请重新准备全部暂存操作"
                    .into(),
            );
        }
        self.verify_real_index_unchanged()?;
        atomic_publish_git_index(&self.lock_path, &self.index_path)?;
        self.published = true;
        #[cfg(unix)]
        {
            // The rename is already the commit point. Directory fsync improves
            // crash durability, but a failure here must not be reported as an
            // uncommitted transaction because the index has been published.
            let _ =
                fs::File::open(&self.repository.git_dir).and_then(|directory| directory.sync_all());
        }
        Ok(())
    }
}

fn read_stage_all_candidate_identity(path: &Path) -> Result<GitIndexBaseline, String> {
    let identity = read_git_index_baseline(path)
        .map_err(|error| format!("无法验证 Git index 候选原始字节: {error}"))?;
    if !identity.exists {
        return Err("Git index 候选在发布前丢失；真实 index 保持不变".into());
    }
    Ok(identity)
}

#[cfg(windows)]
fn atomic_publish_git_index(candidate: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let candidate = candidate
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    const RETRY_DELAYS_MS: [u64; 3] = [5, 15, 30];
    for attempt in 0..=RETRY_DELAYS_MS.len() {
        let moved = unsafe {
            MoveFileExW(
                candidate.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        let retryable = matches!(error.raw_os_error(), Some(5 | 32 | 33));
        if !retryable || attempt == RETRY_DELAYS_MS.len() {
            return Err(format!("无法原子发布 Git index 候选: {error}"));
        }
        std::thread::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt]));
    }
    unreachable!("bounded Windows index publication loop always returns")
}

#[cfg(not(windows))]
fn atomic_publish_git_index(candidate: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(candidate, destination)
        .map_err(|error| format!("无法原子发布 Git index 候选: {error}"))
}

impl Drop for StageAllIndexTransaction<'_> {
    fn drop(&mut self) {
        if !self.published {
            if self.internal_mutation_started {
                let _ = fs::remove_file(&self.nested_lock_path);
            }
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn read_git_index_baseline(path: &Path) -> Result<GitIndexBaseline, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GitIndexBaseline {
                exists: false,
                length: 0,
                sha256: Sha256::digest([]).into(),
            })
        }
        Err(error) => return Err(format!("无法检查真实 Git index: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("真实 Git index 不是受支持的普通文件".into());
    }
    if metadata.len() > MAX_GIT_INDEX_BYTES {
        return Err(format!(
            "真实 Git index 超过 {} MiB 安全上限",
            MAX_GIT_INDEX_BYTES / 1024 / 1024
        ));
    }
    let mut file =
        fs::File::open(path).map_err(|error| format!("无法打开真实 Git index: {error}"))?;
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("无法读取真实 Git index: {error}"))?;
        if read == 0 {
            break;
        }
        length = length.saturating_add(read as u64);
        if length > MAX_GIT_INDEX_BYTES {
            return Err("真实 Git index 在读取时超过安全上限".into());
        }
        digest.update(&buffer[..read]);
    }
    let after =
        fs::symlink_metadata(path).map_err(|error| format!("无法复核真实 Git index: {error}"))?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || metadata.len() != length
        || after.len() != length
    {
        return Err("真实 Git index 在读取时发生变化".into());
    }
    Ok(GitIndexBaseline {
        exists: true,
        length,
        sha256: digest.finalize().into(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageAllTestHookPoint {
    ExecuteAfterValidation,
    ExecuteBeforePublishRecheck,
    ExecuteBeforeIndexPublish,
    CommitBeforeRefUpdate,
    DiffBeforeCandidateRead,
    DiffBeforeFinalProof,
    ChangePageBeforeCandidateRead,
    ChangePageBeforeFinalProof,
}

#[cfg(not(test))]
fn invoke_stage_all_test_hook(_repository: &Repository, _point: StageAllTestHookPoint) {}

#[cfg(test)]
struct InstalledStageAllTestHook {
    repository_root: PathBuf,
    point: StageAllTestHookPoint,
    callback: Option<Box<dyn FnOnce(&Path) + Send>>,
}

#[cfg(test)]
fn stage_all_test_hook_slot() -> &'static Mutex<Vec<InstalledStageAllTestHook>> {
    static SLOT: OnceLock<Mutex<Vec<InstalledStageAllTestHook>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
fn install_stage_all_test_hook(
    repository_root: &Path,
    point: StageAllTestHookPoint,
    callback: impl FnOnce(&Path) + Send + 'static,
) {
    let mut slot = stage_all_test_hook_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.push(InstalledStageAllTestHook {
        repository_root: fs::canonicalize(repository_root).unwrap(),
        point,
        callback: Some(Box::new(callback)),
    });
}

#[cfg(test)]
fn invoke_stage_all_test_hook(repository: &Repository, point: StageAllTestHookPoint) {
    let callback = {
        let mut slot = stage_all_test_hook_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let position = slot.iter().position(|hook| {
            hook.point == point && same_path(&hook.repository_root, &repository.root)
        });
        position
            .map(|position| slot.swap_remove(position))
            .and_then(|mut hook| hook.callback.take())
    };
    if let Some(callback) = callback {
        callback(&repository.root);
    }
}

fn validate_stage_all_candidate(
    candidate: &StageAllCandidateProof,
    expected_target_revision: &str,
) -> Result<(), String> {
    if candidate.target_revision != expected_target_revision {
        return Err(
            "待暂存内容已在确认后发生变化；候选 index 未发布，请重新准备全部暂存操作".into(),
        );
    }
    Ok(())
}

fn execute_stage_all_transaction(
    repository: &Repository,
    expected_content_revision: &str,
    expected_target_revision: &str,
) -> Result<CliOutput, String> {
    let expected_content_revision =
        validate_revision_token("Git 内容修订", expected_content_revision)?;
    let expected_target_revision =
        validate_revision_token("Git 全部暂存目标修订", expected_target_revision)?;
    let mut transaction = StageAllIndexTransaction::acquire(repository)?;
    let snapshot = snapshot_for_repository(repository)?;
    if snapshot.content_revision != expected_content_revision {
        return Err("Git 工作区已在操作前发生变化；候选 index 未发布，请刷新后重试".into());
    }
    validate_stage_all_snapshot(repository, &snapshot)?;
    invoke_stage_all_test_hook(repository, StageAllTestHookPoint::ExecuteAfterValidation);
    let first = transaction.build_candidate(&snapshot)?;
    validate_stage_all_candidate(&first.proof, &expected_target_revision)?;

    invoke_stage_all_test_hook(
        repository,
        StageAllTestHookPoint::ExecuteBeforePublishRecheck,
    );
    let final_snapshot = snapshot_for_repository(repository)?;
    if final_snapshot.content_revision != expected_content_revision {
        return Err(
            "Git 工作区在候选发布前发生变化；真实 index 保持不变，请重新准备全部暂存操作".into(),
        );
    }
    validate_stage_all_snapshot(repository, &final_snapshot)?;
    let final_candidate = transaction.build_candidate(&final_snapshot)?;
    validate_stage_all_candidate(&final_candidate.proof, &expected_target_revision)?;
    let proof = final_candidate.proof;
    let output = final_candidate.output;
    invoke_stage_all_test_hook(repository, StageAllTestHookPoint::ExecuteBeforeIndexPublish);
    transaction.publish(&proof)?;
    Ok(output)
}

fn stage_all_candidate_changes(
    transaction: &StageAllIndexTransaction<'_>,
    snapshot: &GitWorkspaceSnapshot,
    path: Option<&str>,
) -> Result<Vec<GitFileChange>, String> {
    let mut status_args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--cached"),
        OsString::from("--name-status"),
        OsString::from("--find-renames"),
        OsString::from("-z"),
    ];
    if let Some(head) = snapshot.head.as_deref() {
        status_args.push(OsString::from(head));
    }
    if let Some(path) = path {
        status_args.push(OsString::from("--"));
        status_args.push(OsString::from(path));
    }
    let status = run_git_with_internal_index(
        transaction.repository,
        &transaction.lock_path,
        status_args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("读取 Git 全部暂存候选文件列表", &status)?;
    if status.stdout_truncated {
        return Err("Git 全部暂存候选文件列表超过安全上限".into());
    }
    let mut files = parse_name_status(&status.stdout)?;

    let mut numstat_args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--cached"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--numstat"),
        OsString::from("-z"),
    ];
    if let Some(head) = snapshot.head.as_deref() {
        numstat_args.push(OsString::from(head));
    }
    if let Some(path) = path {
        numstat_args.push(OsString::from("--"));
        numstat_args.push(OsString::from(path));
    }
    let numstat = run_git_with_internal_index(
        transaction.repository,
        &transaction.lock_path,
        numstat_args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_STATUS_OUTPUT,
        true,
    )?;
    require_success("读取 Git 全部暂存候选行数", &numstat)?;
    if numstat.stdout_truncated {
        return Err("Git 全部暂存候选行数超过安全上限".into());
    }
    let stats = parse_numstat(&numstat.stdout)?;
    for file in &mut files {
        if let Some(stat) = stats.get(&file.path) {
            file.additions = (!stat.binary).then_some(stat.additions);
            file.deletions = (!stat.binary).then_some(stat.deletions);
            file.binary = stat.binary;
        }
        file.index_status = match file.status {
            GitFileStatus::Added => "A",
            GitFileStatus::Modified => "M",
            GitFileStatus::Deleted => "D",
            GitFileStatus::Renamed => "R",
            GitFileStatus::Copied => "C",
            GitFileStatus::TypeChanged => "T",
            GitFileStatus::Unmerged => "U",
            GitFileStatus::Untracked | GitFileStatus::Ignored | GitFileStatus::Unknown => "?",
        }
        .into();
        file.worktree_status.clear();
        file.staged = true;
        file.unstaged = false;
        file.untracked = false;
        file.conflicted = false;
        if let Some(current) = snapshot
            .files
            .iter()
            .find(|current| current.path == file.path)
        {
            file.submodule = current.submodule;
            file.submodule_commit_changed = current.submodule_commit_changed;
            file.submodule_modified = current.submodule_modified;
            file.submodule_untracked = current.submodule_untracked;
        }
    }
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
    Ok(files)
}

fn stage_all_proof_bound_diff(
    repository: &Repository,
    request: &GitDiffRequest,
    expected_target_revision: &str,
) -> Result<GitDiffResponse, String> {
    let expected_target_revision =
        validate_revision_token("Git 全部暂存目标修订", expected_target_revision)?;
    let path = match request {
        GitDiffRequest::Working { path, .. } | GitDiffRequest::Unstaged { path, .. } => {
            path.as_deref()
        }
        GitDiffRequest::Staged { .. } | GitDiffRequest::Compare { .. } => {
            return Err("全部暂存证明只能用于 working 或 unstaged diff".into())
        }
    }
    .ok_or_else(|| "全部暂存证明 diff 必须指定 path".to_owned())
    .and_then(validate_relative_path)?;
    let mut transaction = StageAllIndexTransaction::acquire(repository)?;
    let snapshot = snapshot_for_repository(repository)?;
    validate_stage_all_snapshot(repository, &snapshot)?;
    let candidate = transaction.build_candidate(&snapshot)?;
    validate_stage_all_candidate(&candidate.proof, &expected_target_revision)?;

    invoke_stage_all_test_hook(repository, StageAllTestHookPoint::DiffBeforeCandidateRead);
    let mut diff_args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("diff"),
        OsString::from("--cached"),
        OsString::from("--no-color"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--unified=3"),
    ];
    if let Some(head) = snapshot.head.as_deref() {
        diff_args.push(OsString::from(head));
    }
    diff_args.push(OsString::from("--"));
    diff_args.push(OsString::from(&path));
    let output = run_git_with_internal_index(
        repository,
        &transaction.lock_path,
        diff_args,
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_PATCH_OUTPUT,
        true,
    )?;
    require_success("读取 Git 全部暂存证明 diff", &output)?;
    let files = stage_all_candidate_changes(&transaction, &snapshot, Some(&path))?;
    let patch = String::from_utf8_lossy(&output.stdout).into_owned();
    let (additions, deletions) = count_patch_lines(&patch);

    invoke_stage_all_test_hook(repository, StageAllTestHookPoint::DiffBeforeFinalProof);
    let final_snapshot = snapshot_for_repository(repository)?;
    validate_stage_all_snapshot(repository, &final_snapshot)?;
    let final_candidate = transaction.build_candidate(&final_snapshot)?;
    validate_stage_all_candidate(&final_candidate.proof, &expected_target_revision)?;
    if final_candidate.proof != candidate.proof {
        return Err("Git 全部暂存候选在读取 diff 期间发生变化；请重新加载".into());
    }
    Ok(GitDiffResponse {
        path: Some(path),
        patch,
        truncated: output.stdout_truncated,
        additions,
        deletions,
        binary: output_looks_binary(&output.stdout),
        files,
        stage_all_target_revision: Some(expected_target_revision),
        candidate_tree_oid: Some(candidate.proof.tree_oid),
    })
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

fn current_branch_name(repository: &Repository) -> Result<String, String> {
    let snapshot = snapshot_for_repository(repository)?;
    snapshot
        .branch
        .ok_or_else(|| "当前 Git HEAD 已分离，必须明确指定分支".to_owned())
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

fn resolve_github(workspace: &Path) -> Result<ResolvedGithub, String> {
    let repository = require_repository(workspace)?;
    let gh = find_program("gh");
    let Some(preferred) = preferred_remote(&repository, gh.as_deref())? else {
        return Ok(ResolvedGithub {
            repository,
            gh: None,
            selector: None,
            context: GithubRepositoryContext {
                cli: GithubCliStatus {
                    installed: gh.is_some(),
                    version: None,
                    host: None,
                    authenticated: false,
                    login: None,
                    error: Some("Git 仓库没有可识别的 GitHub remote".into()),
                },
                repository: None,
            },
        });
    };
    let PreferredGithubRemote {
        remote,
        verified_auth,
    } = preferred;
    let Some(gh) = gh else {
        return Ok(ResolvedGithub {
            repository,
            gh: None,
            selector: None,
            context: GithubRepositoryContext {
                cli: GithubCliStatus {
                    installed: false,
                    version: None,
                    host: Some(remote.host),
                    authenticated: false,
                    login: None,
                    error: Some("未找到 GitHub CLI（gh）".into()),
                },
                repository: None,
            },
        });
    };
    let version = gh_version(&gh, &repository.root).ok();
    // Always qualify the repository with its host. An ambient GH_HOST must never
    // redirect an owner/name selector to a different GitHub Enterprise account.
    let selector = github_repository_selector(&remote);
    let mut cli = GithubCliStatus {
        installed: true,
        version: version.clone(),
        host: Some(remote.host.clone()),
        authenticated: false,
        login: None,
        error: None,
    };
    let auth_status = verified_auth
        .map(Ok)
        .unwrap_or_else(|| github_auth_status(&gh, &repository.root, &remote.host))
        .and_then(|status| {
            canonicalize_github_auth_status(status, || {
                github_canonical_viewer_login(&gh, &repository.root, &remote.host)
            })
        });
    match auth_status {
        Ok((authenticated, login, error)) => {
            cli.authenticated = authenticated;
            cli.login = login;
            cli.error = error;
        }
        Err(error) => cli.error = Some(error),
    }
    let github_repository = if cli.authenticated {
        match github_repository_details(
            &gh,
            &repository.root,
            &selector,
            &remote,
            cli.login.clone(),
            version.clone().unwrap_or_default(),
        ) {
            Ok(repository) => Some(repository),
            Err(error) => {
                cli.error = Some(error);
                None
            }
        }
    } else {
        None
    };
    Ok(ResolvedGithub {
        repository,
        gh: Some(gh),
        selector: Some(selector),
        context: GithubRepositoryContext {
            cli,
            repository: github_repository,
        },
    })
}

fn require_github_repository(workspace: &Path) -> Result<ResolvedGithub, String> {
    let resolved = resolve_github(workspace)?;
    if !resolved.context.cli.installed {
        return Err("未找到 GitHub CLI（gh）".into());
    }
    if !resolved.context.cli.authenticated {
        return Err(resolved
            .context
            .cli
            .error
            .clone()
            .unwrap_or_else(|| "GitHub CLI 尚未登录".into()));
    }
    if resolved.context.repository.is_none() || resolved.selector.is_none() {
        return Err(resolved
            .context
            .cli
            .error
            .clone()
            .unwrap_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".into()));
    }
    Ok(resolved)
}

struct ParsedRemote {
    host: String,
    owner: String,
    name: String,
}

fn github_repository_selector(remote: &ParsedRemote) -> String {
    format!("{}/{}/{}", remote.host, remote.owner, remote.name)
}

type GithubAuthStatus = (bool, Option<String>, Option<String>);

struct PreferredGithubRemote {
    remote: ParsedRemote,
    // Unknown hosts are only admitted after this exact host has already passed
    // `gh auth status --active`. Reuse that result so discovery and the
    // displayed authentication state cannot disagree because of a second call.
    verified_auth: Option<GithubAuthStatus>,
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

fn validate_transport_locator(locator: &str) -> Result<(), String> {
    if locator.is_empty()
        || locator.as_bytes().len() > MAX_GIT_REMOTE_VALUE_BYTES
        || locator
            .chars()
            .any(|character| character == '\0' || character.is_control())
    {
        return Err("Git remote transport locator 不受支持".into());
    }
    static REMOTE_HELPER: OnceLock<Regex> = OnceLock::new();
    if REMOTE_HELPER
        .get_or_init(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*::").expect("valid remote helper regex"))
        .is_match(locator)
    {
        return Err("Git external remote helper transport 不受支持".into());
    }
    if let Some(separator) = locator.find("://") {
        let scheme = locator[..separator].to_ascii_lowercase();
        if !matches!(scheme.as_str(), "https" | "http" | "ssh" | "git" | "file") {
            return Err("Git remote transport scheme 不受支持".into());
        }
        let parsed =
            url::Url::parse(locator).map_err(|_| "Git remote transport URL 无效".to_owned())?;
        if scheme != "file" {
            let host = parsed
                .host_str()
                .ok_or_else(|| "Git remote transport URL 缺少 host".to_owned())?;
            validate_ssh_host_token(host)?;
        }
        return Ok(());
    }
    if let Some((authority, path)) = scp_transport_parts(locator) {
        let host = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        validate_ssh_host_token(host)?;
        if authority
            .split_once('@')
            .is_some_and(|(user, _)| user.is_empty() || user.starts_with('-'))
        {
            return Err("Git SCP transport user 无效".into());
        }
        validate_transport_path(path, "Git SCP transport path")?;
        return Ok(());
    }
    validate_transport_path(locator, "Git local transport path")
}

fn validate_transport_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('-')
        || path.chars().any(|character| {
            !(character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '/' | '\\' | '.' | '_' | '-' | '~' | ':' | '+' | '@' | '%'
                ))
        })
    {
        return Err(format!("{label} 无效"));
    }
    Ok(())
}

fn validate_ssh_host_token(host: &str) -> Result<(), String> {
    if host.is_empty()
        || host.starts_with('-')
        || host
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err("Git SSH transport host 无效".into());
    }
    Ok(())
}

fn scp_transport_parts(locator: &str) -> Option<(&str, &str)> {
    let windows_drive_path = cfg!(windows)
        && locator
            .as_bytes()
            .get(0..2)
            .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':');
    if windows_drive_path
        || Path::new(locator).is_absolute()
        || locator.starts_with("./")
        || locator.starts_with("../")
        || locator.starts_with(".\\")
        || locator.starts_with("..\\")
        || locator.starts_with("\\\\")
    {
        return None;
    }
    if locator.starts_with('[') {
        let separator = locator.find("]:")?;
        return Some((&locator[..separator + 1], &locator[separator + 2..]));
    }
    locator.split_once(':')
}

fn transport_locator_uses_ssh(locator: &str) -> bool {
    locator
        .find("://")
        .is_some_and(|separator| locator[..separator].eq_ignore_ascii_case("ssh"))
        || (locator.find("://").is_none() && scp_transport_parts(locator).is_some())
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

fn internal_verified_remote_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let first = RandomState::new().build_hasher().finish();
        let second = RandomState::new().build_hasher().finish();
        format!("mework-proof-{first:016x}{second:016x}")
    })
}

fn local_remote_proof() -> GitRemote {
    GitRemote {
        name: ".".into(),
        fetch_revision: remote_transport_revision(b"mework.git.remote-fetch.v1\0", ".", &[], &[]),
        push_revision: remote_transport_revision(b"mework.git.remote-push.v1\0", ".", &[], &[]),
        url: None,
    }
}

fn remote_transport(repository: &Repository, name: &str) -> Result<RemoteTransport, String> {
    validate_remote_name_syntax(name, false)?;
    if !configured_remote_names(repository)?
        .iter()
        .any(|candidate| candidate == name)
    {
        return Err("Git remote 不存在".into());
    }
    remote_transport_for_existing(repository, name)
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
    Ok(RemoteTransport {
        configuration: None,
        proof,
        fetch_urls,
        push_urls,
        fetch_refspecs,
    })
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
    let mut bytes = output.stdout.as_slice();
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
    transports: &[RemoteTransport],
    upstream: Option<&GitUpstream>,
) -> Option<GitRemote> {
    if let Some(upstream) = upstream {
        return Some(upstream.remote.clone());
    }
    for preferred in ["origin", "upstream"] {
        if let Some(transport) = transports
            .iter()
            .find(|transport| transport.proof.name == preferred)
        {
            return Some(transport.proof.clone());
        }
    }
    transports.first().map(|transport| transport.proof.clone())
}

fn preferred_remote(
    repository: &Repository,
    gh: Option<&Path>,
) -> Result<Option<PreferredGithubRemote>, String> {
    let remotes = run_git(
        repository,
        [OsString::from("remote")],
        None,
        LOCAL_COMMAND_TIMEOUT,
        64 * 1024,
        true,
    )?;
    require_success("读取 Git remotes", &remotes)?;
    let names = String::from_utf8_lossy(&remotes.stdout)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut ordered = Vec::new();
    for preferred in ["origin", "upstream"] {
        if names.iter().any(|name| name == preferred) {
            ordered.push(preferred.to_owned());
        }
    }
    ordered.extend(
        names
            .into_iter()
            .filter(|name| name != "origin" && name != "upstream"),
    );
    for name in ordered {
        let output = run_git(
            repository,
            [
                OsString::from("remote"),
                OsString::from("get-url"),
                OsString::from(&name),
            ],
            None,
            LOCAL_COMMAND_TIMEOUT,
            64 * 1024,
            true,
        )?;
        if !output.success() {
            continue;
        }
        let Some(parsed) = parse_remote_url(String::from_utf8_lossy(&output.stdout).trim()) else {
            continue;
        };
        let known_host = is_known_github_host(&parsed.host);
        if let Some(preferred) = qualify_github_remote(parsed, known_host, |host| {
            let gh = gh.ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
            github_auth_status(gh, &repository.root, host)
        }) {
            return Ok(Some(preferred));
        }
    }
    Ok(None)
}

fn is_known_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com")
        || host
            .split('.')
            .any(|component| component.eq_ignore_ascii_case("github"))
        || env::var("GH_HOST")
            .ok()
            .is_some_and(|configured| configured.eq_ignore_ascii_case(host))
}

fn qualify_github_remote(
    remote: ParsedRemote,
    known_host: bool,
    authenticate: impl FnOnce(&str) -> Result<GithubAuthStatus, String>,
) -> Option<PreferredGithubRemote> {
    if known_host {
        return Some(PreferredGithubRemote {
            remote,
            verified_auth: None,
        });
    }
    let auth = authenticate(&remote.host).ok()?;
    if !auth.0 {
        return None;
    }
    Some(PreferredGithubRemote {
        remote,
        verified_auth: Some(auth),
    })
}

fn parse_remote_url(value: &str) -> Option<ParsedRemote> {
    let (host, path) = if let Ok(url) = url::Url::parse(value) {
        (
            url.host_str()?.to_owned(),
            url.path().trim_start_matches('/').to_owned(),
        )
    } else {
        let (_, rest) = value.rsplit_once('@')?;
        let (host, path) = rest.split_once(':')?;
        (host.to_owned(), path.to_owned())
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut components = path.split('/').filter(|value| !value.is_empty());
    let owner = components.next()?.to_owned();
    let name = components.next()?.to_owned();
    if components.next().is_some()
        || host.is_empty()
        || host.starts_with('-')
        || owner.is_empty()
        || name.is_empty()
        || [host.as_str(), owner.as_str(), name.as_str()]
            .iter()
            .any(|value| value.contains('\0') || value.chars().any(char::is_whitespace))
    {
        return None;
    }
    Some(ParsedRemote { host, owner, name })
}

fn gh_version(gh: &Path, cwd: &Path) -> Result<String, String> {
    let output = run_gh(
        gh,
        cwd,
        [OsString::from("--version")],
        None,
        LOCAL_COMMAND_TIMEOUT,
        16 * 1024,
    )?;
    require_success("读取 GitHub CLI 版本", &output)?;
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    Ok(first
        .strip_prefix("gh version ")
        .unwrap_or(&first)
        .split_ascii_whitespace()
        .next()
        .unwrap_or(&first)
        .to_owned())
}

fn github_auth_status(gh: &Path, cwd: &Path, host: &str) -> Result<GithubAuthStatus, String> {
    let output = run_gh(
        gh,
        cwd,
        github_auth_status_args(host),
        None,
        LOCAL_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub CLI 登录状态", &output)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub CLI 登录状态 JSON 无效: {error}"))?;
    parse_auth_status(&value, host)
}

fn canonicalize_github_auth_status(
    status: GithubAuthStatus,
    canonical_login: impl FnOnce() -> Result<String, String>,
) -> Result<GithubAuthStatus, String> {
    let (authenticated, reported_login, error) = status;
    if !authenticated {
        return Ok((false, reported_login, error));
    }
    if error.is_some() {
        return Err("GitHub CLI 活动账号状态不一致；已拒绝继续".into());
    }
    let reported_login =
        reported_login.ok_or_else(|| "GitHub CLI 活动账号缺少 login；已拒绝继续".to_owned())?;
    validate_github_identity_value("GitHub CLI 活动账号 login", &reported_login)?;
    let canonical_login = canonical_login()?;
    validate_github_identity_value("GitHub canonical viewer login", &canonical_login)?;
    Ok((true, Some(canonical_login), None))
}

fn github_canonical_viewer_login(gh: &Path, cwd: &Path, host: &str) -> Result<String, String> {
    let output = run_gh(
        gh,
        cwd,
        github_canonical_viewer_login_args(host),
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_GITHUB_VIEWER_LOGIN_OUTPUT,
    )?;
    require_complete_github_canonical_viewer_output(
        output.stdout_truncated,
        output.stderr_truncated,
    )?;
    require_success("读取 GitHub canonical viewer", &output)?;
    parse_github_canonical_viewer_login(&output.stdout)
}

fn require_complete_github_canonical_viewer_output(
    stdout_truncated: bool,
    stderr_truncated: bool,
) -> Result<(), String> {
    if stdout_truncated || stderr_truncated {
        Err("读取 GitHub canonical viewer 失败：命令输出超过安全上限".into())
    } else {
        Ok(())
    }
}

fn github_canonical_viewer_login_args(host: &str) -> [OsString; 5] {
    [
        OsString::from("api"),
        OsString::from(format!("--hostname={host}")),
        OsString::from("user"),
        OsString::from("--jq"),
        OsString::from(".login"),
    ]
}

fn parse_github_canonical_viewer_login(bytes: &[u8]) -> Result<String, String> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| "GitHub canonical viewer login 不是有效 UTF-8".to_owned())?;
    let login = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value);
    validate_github_identity_value("GitHub canonical viewer login", login)?;
    Ok(login.to_owned())
}

fn github_auth_status_args(host: &str) -> [OsString; 6] {
    [
        OsString::from("auth"),
        OsString::from("status"),
        OsString::from("--active"),
        OsString::from(format!("--hostname={host}")),
        OsString::from("--json"),
        OsString::from("hosts"),
    ]
}

fn parse_auth_status(value: &Value, host: &str) -> Result<GithubAuthStatus, String> {
    let accounts = value
        .get("hosts")
        .and_then(Value::as_object)
        .and_then(|hosts| {
            hosts
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(host))
                .and_then(|(_, accounts)| accounts.as_array())
        })
        .ok_or_else(|| format!("GitHub CLI 未返回 {host} 的登录信息"))?;
    let active = accounts.iter().find(|account| {
        account
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    let Some(active) = active else {
        return Ok((false, None, Some(format!("{host} 没有活动的 gh 账号"))));
    };
    let success = active
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| state.eq_ignore_ascii_case("success"));
    if !success {
        return Ok((
            false,
            string_field(active, "login"),
            Some(format!("{host} 的 gh 活动账号认证无效")),
        ));
    }
    let login = required_string_field(active, "login")
        .map_err(|_| format!("{host} 的 gh 活动账号缺少有效 login"))?;
    validate_github_identity_value("GitHub CLI 活动账号 login", &login)?;
    Ok((true, Some(login), None))
}

fn github_repository_details(
    gh: &Path,
    cwd: &Path,
    selector: &str,
    remote: &ParsedRemote,
    viewer_login: Option<String>,
    gh_version: String,
) -> Result<GithubRepository, String> {
    let output = run_gh(
        gh,
        cwd,
        [
            OsString::from("repo"),
            OsString::from("view"),
            OsString::from(selector),
            OsString::from("--json"),
            OsString::from("nameWithOwner,url,defaultBranchRef"),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub 仓库信息", &output)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub 仓库信息 JSON 无效: {error}"))?;
    Ok(GithubRepository {
        host: remote.host.clone(),
        owner: remote.owner.clone(),
        name: remote.name.clone(),
        name_with_owner: required_string_field(&value, "nameWithOwner")?,
        url: required_string_field(&value, "url")?,
        default_branch: value
            .get("defaultBranchRef")
            .and_then(|branch| branch.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        viewer_login,
        authenticated: true,
        gh_version,
    })
}

fn validate_github_pr_page(page: u32, page_size: u16) -> Result<(), String> {
    if page == 0 || page > MAX_GITHUB_PR_PAGE {
        return Err(format!(
            "GitHub Pull Request 页码必须在 1 到 {MAX_GITHUB_PR_PAGE} 之间"
        ));
    }
    if page_size == 0 || page_size > MAX_GITHUB_PR_PAGE_SIZE {
        return Err(format!(
            "GitHub Pull Request 每页数量必须在 1 到 {MAX_GITHUB_PR_PAGE_SIZE} 之间"
        ));
    }
    Ok(())
}

fn github_pull_request_list_args(
    repository: &GithubRepository,
    page: u32,
    page_size: u16,
) -> Vec<OsString> {
    vec![
        OsString::from("api"),
        OsString::from(format!(
            "repos/{}/{}/pulls",
            repository.owner, repository.name
        )),
        OsString::from(format!("--hostname={}", repository.host)),
        OsString::from("--include"),
        OsString::from("--method=GET"),
        OsString::from("--jq"),
        OsString::from(GITHUB_PR_LIST_JQ),
        OsString::from("-H"),
        OsString::from("Accept: application/vnd.github+json"),
        OsString::from("-f"),
        OsString::from("state=open"),
        OsString::from("-f"),
        OsString::from("sort=updated"),
        OsString::from("-f"),
        OsString::from("direction=desc"),
        OsString::from("-f"),
        OsString::from(format!("page={page}")),
        OsString::from("-f"),
        OsString::from(format!("per_page={page_size}")),
    ]
}

fn parse_github_pull_request_page(
    bytes: &[u8],
) -> Result<(Vec<GithubPullRequestSummary>, bool), String> {
    let (headers, body) = split_gh_api_included_response(bytes)?;
    let has_more = headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("link")
                && value
                    .split(',')
                    .any(|link| link.to_ascii_lowercase().contains("rel=\"next\""))
        })
    });
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("GitHub Pull Request 列表 JSON 无效: {error}"))?;
    let pull_requests = value
        .as_array()
        .ok_or_else(|| "GitHub Pull Request 列表不是数组".to_owned())?
        .iter()
        .map(parse_rest_pr_summary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((pull_requests, has_more))
}

fn split_gh_api_included_response(bytes: &[u8]) -> Result<(String, &[u8]), String> {
    let separator = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            bytes
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
        })
        .ok_or_else(|| "GitHub API 响应缺少 HTTP 标头".to_owned())?;
    let headers = std::str::from_utf8(&bytes[..separator.0])
        .map_err(|error| format!("GitHub API HTTP 标头不是有效 UTF-8: {error}"))?
        .to_owned();
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.trim_start().starts_with("HTTP/"))
    {
        return Err("GitHub API 响应缺少 HTTP 状态行".to_owned());
    }
    Ok((headers, &bytes[separator.0 + separator.1..]))
}

fn parse_rest_pr_summary(value: &Value) -> Result<GithubPullRequestSummary, String> {
    Ok(GithubPullRequestSummary {
        number: required_u64_field(value, "number")?,
        title: required_string_field(value, "title")?,
        state: required_string_field(value, "state")?.to_ascii_lowercase(),
        draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
        head_ref_name: required_nested_string_field(value, "head", "ref")?,
        base_ref_name: required_nested_string_field(value, "base", "ref")?,
        author: value
            .get("user")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        updated_at: required_string_field(value, "updated_at")?,
        url: required_string_field(value, "html_url")?,
        // The list endpoint intentionally omits the expensive mergeability calculation.
        mergeable: None,
    })
}

fn parse_pr_summary(value: &Value) -> Result<GithubPullRequestSummary, String> {
    Ok(GithubPullRequestSummary {
        number: required_u64_field(value, "number")?,
        title: required_string_field(value, "title")?,
        state: required_string_field(value, "state")?.to_ascii_lowercase(),
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        head_ref_name: required_string_field(value, "headRefName")?,
        base_ref_name: required_string_field(value, "baseRefName")?,
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        updated_at: required_string_field(value, "updatedAt")?,
        url: required_string_field(value, "url")?,
        mergeable: mergeable_field(value, "mergeable"),
    })
}

fn parse_pr_detail(value: &Value) -> Result<GithubPullRequestDetail, String> {
    let summary = parse_pr_summary(value)?;
    let checks = parse_status_checks(value.get("statusCheckRollup"));
    let status_check_rollup = summarize_parsed_status_checks(&checks);
    Ok(GithubPullRequestDetail {
        number: summary.number,
        title: summary.title,
        state: summary.state,
        url: summary.url,
        author: summary.author,
        head_ref_name: summary.head_ref_name,
        head_ref_oid: validate_object_id(
            "Pull Request head commit",
            required_string_field(value, "headRefOid")?,
        )?,
        base_ref_name: summary.base_ref_name,
        draft: summary.draft,
        mergeable: summary.mergeable,
        updated_at: summary.updated_at,
        body: string_field(value, "body").unwrap_or_default(),
        additions: u64_field(value, "additions"),
        deletions: u64_field(value, "deletions"),
        changed_files: u64_field(value, "changedFiles"),
        commits: value
            .get("commits")
            .and_then(|commits| {
                commits
                    .as_u64()
                    .or_else(|| commits.as_array().map(|items| items.len() as u64))
            })
            .unwrap_or(0),
        review_decision: string_field(value, "reviewDecision"),
        status_check_rollup,
        checks,
    })
}

fn github_pull_request_detail_for_resolved(
    resolved: &ResolvedGithub,
    number: u64,
) -> Result<GithubPullRequestDetail, String> {
    validate_pr_number(number)?;
    let gh = resolved
        .gh
        .as_deref()
        .ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
    let selector = resolved
        .selector
        .as_deref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        [
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(number.to_string()),
            OsString::from("-R"),
            OsString::from(selector),
            OsString::from("--json"),
            OsString::from(GITHUB_PR_DETAIL_JSON_FIELDS),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request", &output)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub Pull Request JSON 无效: {error}"))?;
    parse_pr_detail(&value)
}

fn github_pull_request_full_patch(
    resolved: &ResolvedGithub,
    number: u64,
) -> Result<(String, bool, bool), String> {
    let gh = resolved
        .gh
        .as_deref()
        .ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
    let selector = resolved
        .selector
        .as_deref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        [
            OsString::from("pr"),
            OsString::from("diff"),
            OsString::from(number.to_string()),
            OsString::from("-R"),
            OsString::from(selector),
            OsString::from("--patch"),
            OsString::from("--color"),
            OsString::from("never"),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_PATCH_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request diff", &output)?;
    let binary = output_looks_binary(&output.stdout);
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.stdout_truncated,
        binary,
    ))
}

fn github_pull_request_read_state_for_resolved(
    resolved: &ResolvedGithub,
    number: u64,
) -> Result<GithubPullRequestReadState, String> {
    let gh = resolved
        .gh
        .as_deref()
        .ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
    let selector = resolved
        .selector
        .as_deref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let output = run_gh(
        gh,
        &resolved.repository.root,
        [
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(number.to_string()),
            OsString::from("-R"),
            OsString::from(selector),
            OsString::from("--json"),
            OsString::from("headRefOid,baseRefOid,changedFiles"),
        ],
        None,
        NETWORK_COMMAND_TIMEOUT,
        MAX_JSON_OUTPUT,
    )?;
    require_success("读取 GitHub Pull Request head commit", &output)?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub Pull Request head commit JSON 无效: {error}"))?;
    parse_github_pull_request_read_state(&value)
}

fn parse_github_pull_request_read_state(
    value: &Value,
) -> Result<GithubPullRequestReadState, String> {
    Ok(GithubPullRequestReadState {
        head_ref_oid: validate_object_id(
            "Pull Request head commit",
            required_string_field(&value, "headRefOid")?,
        )?,
        base_ref_oid: validate_object_id(
            "Pull Request base commit",
            required_string_field(&value, "baseRefOid")?,
        )?,
        changed_files: required_u64_field(&value, "changedFiles")?,
    })
}

fn github_pull_request_files(
    resolved: &ResolvedGithub,
    number: u64,
    expected_file_count: u64,
    selected_path: Option<&str>,
) -> Result<GithubPullRequestFiles, String> {
    if expected_file_count > MAX_GITHUB_PR_FILES {
        return Err(format!(
            "Pull Request #{number} 包含 {expected_file_count} 个文件，超过 GitHub 文件 API 的 {MAX_GITHUB_PR_FILES} 个文件上限"
        ));
    }
    let gh = resolved
        .gh
        .as_deref()
        .ok_or_else(|| "未找到 GitHub CLI（gh）".to_owned())?;
    let repository = resolved
        .context
        .repository
        .as_ref()
        .ok_or_else(|| "当前 Git 仓库无法解析为 GitHub 仓库".to_owned())?;
    let mut files = Vec::with_capacity(expected_file_count as usize);
    let mut selected = None;
    let mut seen_paths = HashSet::with_capacity(expected_file_count as usize);
    for page in 1..=MAX_GITHUB_PR_FILE_PAGES {
        let args = github_pull_request_files_page_args(repository, number, page, selected_path)?;
        let output = run_gh(
            gh,
            &resolved.repository.root,
            args,
            None,
            NETWORK_COMMAND_TIMEOUT,
            MAX_GITHUB_PR_FILES_PAGE_OUTPUT,
        )?;
        require_success("读取 GitHub Pull Request 文件", &output)?;
        if output.stdout_truncated {
            return Err(format!(
                "GitHub Pull Request 文件第 {page} 页超过 {} MiB 安全上限，无法保证完整审阅",
                MAX_GITHUB_PR_FILES_PAGE_OUTPUT / 1024 / 1024
            ));
        }
        let parsed = parse_github_pull_request_files_page(&output.stdout)?;
        if parsed.has_more && parsed.files.is_empty() {
            return Err(format!(
                "GitHub Pull Request 文件第 {page} 页为空但仍声明下一页"
            ));
        }
        for mut file in parsed.files {
            if !seen_paths.insert(file.change.path.clone()) {
                return Err(format!(
                    "GitHub Pull Request 文件列表重复返回路径 {}",
                    file.change.path
                ));
            }
            if selected_path.is_some_and(|path| path == file.change.path) {
                if selected.is_some() {
                    return Err(format!(
                        "GitHub Pull Request 文件列表重复返回目标路径 {}",
                        file.change.path
                    ));
                }
                file.change.binary = github_pull_request_file_is_binary(&file);
                selected = Some(file.clone());
            }
            files.push(file.change);
            if files.len() as u64 > expected_file_count {
                return Err(format!(
                    "GitHub Pull Request 文件数量超过读取前声明的 {expected_file_count} 个；请刷新后重试"
                ));
            }
        }
        if selected_path.is_some() {
            if let Some(selected) = selected.take() {
                return Ok(GithubPullRequestFiles {
                    files: vec![selected.change.clone()],
                    selected: Some(selected),
                });
            }
        }
        if !parsed.has_more {
            if files.len() as u64 != expected_file_count {
                return Err(format!(
                    "GitHub Pull Request 文件列表不完整：预期 {expected_file_count} 个，实际读取 {} 个",
                    files.len()
                ));
            }
            return Ok(GithubPullRequestFiles { files, selected });
        }
        if files.len() as u64 >= expected_file_count {
            return Err(format!(
                "GitHub Pull Request 文件分页与读取前声明的 {expected_file_count} 个文件不一致"
            ));
        }
        if page == MAX_GITHUB_PR_FILE_PAGES {
            return Err(format!(
                "GitHub Pull Request 文件超过 {MAX_GITHUB_PR_FILE_PAGES} 页安全上限，无法保证完整审阅"
            ));
        }
    }
    Err("GitHub Pull Request 文件分页异常终止".to_owned())
}

fn github_pull_request_files_page_args(
    repository: &GithubRepository,
    number: u64,
    page: u32,
    selected_path: Option<&str>,
) -> Result<Vec<OsString>, String> {
    validate_pr_number(number)?;
    if page == 0 || page > MAX_GITHUB_PR_FILE_PAGES {
        return Err(format!(
            "GitHub Pull Request 文件页码必须在 1 到 {MAX_GITHUB_PR_FILE_PAGES} 之间"
        ));
    }
    let jq = if let Some(path) = selected_path {
        let path = serde_json::to_string(path)
            .map_err(|error| format!("无法编码 GitHub Pull Request 文件路径: {error}"))?;
        format!(
            "map({{filename,previous_filename,status,additions,deletions,changes,patch:(if .filename == {path} then .patch else null end)}})"
        )
    } else {
        GITHUB_PR_FILES_JQ.to_owned()
    };
    Ok(vec![
        OsString::from("api"),
        OsString::from(format!(
            "repos/{}/{}/pulls/{number}/files",
            repository.owner, repository.name
        )),
        OsString::from(format!("--hostname={}", repository.host)),
        OsString::from("--include"),
        OsString::from("--method=GET"),
        OsString::from("--jq"),
        OsString::from(jq),
        OsString::from("-H"),
        OsString::from("Accept: application/vnd.github+json"),
        OsString::from("-f"),
        OsString::from(format!("page={page}")),
        OsString::from("-f"),
        OsString::from(format!("per_page={GITHUB_PR_FILES_PAGE_SIZE}")),
    ])
}

fn parse_github_pull_request_files_page(
    bytes: &[u8],
) -> Result<GithubPullRequestFilesPage, String> {
    let (headers, body) = split_gh_api_included_response(bytes)?;
    let has_more = headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("link")
                && value
                    .split(',')
                    .any(|link| link.to_ascii_lowercase().contains("rel=\"next\""))
        })
    });
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("GitHub Pull Request 文件 JSON 无效: {error}"))?;
    let values = value
        .as_array()
        .ok_or_else(|| "GitHub Pull Request 文件列表不是数组".to_owned())?;
    if values.len() > GITHUB_PR_FILES_PAGE_SIZE as usize {
        return Err(format!(
            "GitHub Pull Request 文件单页超过 {GITHUB_PR_FILES_PAGE_SIZE} 个"
        ));
    }
    let files = values
        .iter()
        .map(parse_rest_pull_request_file)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GithubPullRequestFilesPage { files, has_more })
}

fn parse_rest_pull_request_file(value: &Value) -> Result<GithubPullRequestFile, String> {
    let path = validate_relative_path(&required_string_field(value, "filename")?)?;
    if path == "." {
        return Err("GitHub Pull Request 文件路径无效".to_owned());
    }
    let original_path = string_field(value, "previous_filename")
        .map(|path| validate_relative_path(&path))
        .transpose()?
        .filter(|path| path != ".");
    let status = match required_string_field(value, "status")?
        .to_ascii_lowercase()
        .as_str()
    {
        "added" => GitFileStatus::Added,
        "deleted" | "removed" => GitFileStatus::Deleted,
        "renamed" => GitFileStatus::Renamed,
        "copied" => GitFileStatus::Copied,
        "modified" | "changed" | "unchanged" => GitFileStatus::Modified,
        _ => GitFileStatus::Unknown,
    };
    let additions = required_u64_field(value, "additions")?;
    let deletions = required_u64_field(value, "deletions")?;
    let changes = required_u64_field(value, "changes")?;
    let patch = match value.get("patch") {
        None | Some(Value::Null) => None,
        Some(Value::String(patch)) => Some(patch.clone()),
        Some(_) => return Err("GitHub Pull Request 文件 patch 不是字符串".to_owned()),
    };
    Ok(GithubPullRequestFile {
        change: GitFileChange {
            path,
            original_path,
            status,
            index_status: String::new(),
            worktree_status: String::new(),
            staged: false,
            unstaged: false,
            untracked: false,
            conflicted: false,
            additions: Some(additions),
            deletions: Some(deletions),
            binary: false,
            submodule: false,
            submodule_commit_changed: false,
            submodule_modified: false,
            submodule_untracked: false,
        },
        changes,
        patch,
    })
}

fn github_pull_request_file_is_binary(file: &GithubPullRequestFile) -> bool {
    file.patch.is_none()
        && file.change.additions == Some(0)
        && file.change.deletions == Some(0)
        && file.changes == 0
        && !matches!(
            file.change.status,
            GitFileStatus::Renamed | GitFileStatus::Copied
        )
}

fn github_pull_request_file_patch_truncated(file: &GithubPullRequestFile) -> bool {
    let additions = file.change.additions.unwrap_or(0);
    let deletions = file.change.deletions.unwrap_or(0);
    match file.patch.as_deref() {
        Some(patch) => {
            let (patch_additions, patch_deletions) = count_patch_lines(patch);
            patch_additions < additions || patch_deletions < deletions
        }
        None => !file.change.binary && (additions > 0 || deletions > 0 || file.changes > 0),
    }
}

fn render_github_pull_request_file_patch(file: &GithubPullRequestFile) -> String {
    let old_path = file
        .change
        .original_path
        .as_deref()
        .unwrap_or(&file.change.path);
    let old_prefixed = format!("a/{old_path}");
    let new_prefixed = format!("b/{}", file.change.path);
    let old_label = if file.change.status == GitFileStatus::Added {
        "/dev/null".to_owned()
    } else {
        old_prefixed.clone()
    };
    let new_label = if file.change.status == GitFileStatus::Deleted {
        "/dev/null".to_owned()
    } else {
        new_prefixed.clone()
    };
    let mut rendered = format!(
        "diff --git {} {}\n",
        quote_git_patch_path(&old_prefixed),
        quote_git_patch_path(&new_prefixed)
    );
    if file.change.status == GitFileStatus::Renamed {
        rendered.push_str(&format!("rename from {}\n", quote_git_patch_path(old_path)));
        rendered.push_str(&format!(
            "rename to {}\n",
            quote_git_patch_path(&file.change.path)
        ));
    } else if file.change.status == GitFileStatus::Copied {
        rendered.push_str(&format!("copy from {}\n", quote_git_patch_path(old_path)));
        rendered.push_str(&format!(
            "copy to {}\n",
            quote_git_patch_path(&file.change.path)
        ));
    }
    if file.patch.is_some()
        || !matches!(
            file.change.status,
            GitFileStatus::Renamed | GitFileStatus::Copied
        )
    {
        rendered.push_str(&format!("--- {}\n", quote_git_patch_path(&old_label)));
        rendered.push_str(&format!("+++ {}\n", quote_git_patch_path(&new_label)));
    }
    if let Some(patch) = file.patch.as_deref() {
        rendered.push_str(patch);
        if !patch.ends_with('\n') {
            rendered.push('\n');
        }
    } else if file.change.binary {
        rendered.push_str(&format!(
            "Binary files {} and {} differ\n",
            quote_git_patch_path(&old_label),
            quote_git_patch_path(&new_label)
        ));
    }
    rendered
}

fn quote_git_patch_path(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        path.to_owned()
    } else {
        serde_json::to_string(path).unwrap_or_else(|_| "\"invalid path\"".to_owned())
    }
}

fn mergeable_field(value: &Value, field: &str) -> Option<bool> {
    let value = value.get(field)?;
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    match value.as_str()?.to_ascii_uppercase().as_str() {
        "MERGEABLE" => Some(true),
        "CONFLICTING" => Some(false),
        _ => None,
    }
}

fn parse_status_checks(value: Option<&Value>) -> Vec<GithubPullRequestCheck> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|check| GithubPullRequestCheck {
            name: string_field(check, "name")
                .or_else(|| string_field(check, "context"))
                .unwrap_or_else(|| "GitHub check".to_owned()),
            state: normalize_status_check_state(check),
            workflow: string_field(check, "workflowName").or_else(|| {
                check
                    .get("workflow")
                    .and_then(|workflow| string_field(workflow, "name"))
            }),
            description: string_field(check, "description"),
            link: string_field(check, "detailsUrl")
                .or_else(|| string_field(check, "targetUrl"))
                .or_else(|| string_field(check, "link")),
            started_at: string_field(check, "startedAt"),
            completed_at: string_field(check, "completedAt"),
        })
        .collect()
}

fn normalize_status_check_state(check: &Value) -> String {
    let state = string_field(check, "conclusion")
        .or_else(|| string_field(check, "state"))
        .or_else(|| string_field(check, "status"))
        .unwrap_or_default()
        .to_ascii_uppercase();
    match state.as_str() {
        "SUCCESS" | "SUCCESSFUL" => "success",
        "NEUTRAL" => "neutral",
        "SKIPPED" => "skipped",
        "CANCELLED" | "CANCELED" => "cancelled",
        "FAILURE" | "FAILED" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" => "failure",
        _ => "pending",
    }
    .to_owned()
}

fn summarize_parsed_status_checks(checks: &[GithubPullRequestCheck]) -> Option<String> {
    if checks.is_empty() {
        return None;
    }
    let mut pending = false;
    for check in checks {
        if matches!(check.state.as_str(), "failure" | "cancelled") {
            return Some("failure".into());
        }
        if !matches!(check.state.as_str(), "success" | "neutral" | "skipped") {
            pending = true;
        }
    }
    Some(if pending { "pending" } else { "success" }.into())
}

fn validate_gh_text(label: &str, value: String, max_chars: usize) -> Result<String, String> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.contains('\0') {
        return Err(format!("{label}不能为空或包含 NUL"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{label}不能超过 {max_chars} 个字符"));
    }
    Ok(value)
}

fn validate_gh_ref(label: &str, value: String) -> Result<String, String> {
    if value.trim() != value
        || value.is_empty()
        || value.contains('\0')
        || value.starts_with('-')
        || value.len() > 1024
    {
        return Err(format!("Pull Request {label} ref 无效"));
    }
    Ok(value)
}

fn validate_object_id(label: &str, value: String) -> Result<String, String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} 无效"));
    }
    Ok(value.to_ascii_lowercase())
}

fn extract_pull_request_number(value: &str) -> Option<u64> {
    let (_, tail) = value.rsplit_once("/pull/")?;
    tail.split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn validate_pr_number(number: u64) -> Result<(), String> {
    if number == 0 {
        Err("Pull Request 编号必须大于 0".into())
    } else {
        Ok(())
    }
}

fn required_string_field(value: &Value, field: &str) -> Result<String, String> {
    string_field(value, field).ok_or_else(|| format!("GitHub JSON 缺少字符串字段 {field}"))
}

fn required_nested_string_field(
    value: &Value,
    parent: &str,
    field: &str,
) -> Result<String, String> {
    value
        .get(parent)
        .and_then(|nested| nested.get(field))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("GitHub JSON 缺少字符串字段 {parent}.{field}"))
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn required_u64_field(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("GitHub JSON 缺少整数值字段 {field}"))
}

fn u64_field(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
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

fn run_git_with_disabled_hooks(
    repository: &Repository,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
) -> Result<CliOutput, String> {
    let config = vec![(
        "core.hooksPath".to_owned(),
        disabled_hooks_path().to_owned(),
    )];
    let mut prepared = git_command_prefix();
    prepared.extend(args);
    run_program_with_internal_git_settings(
        &repository.git,
        &repository.root,
        prepared,
        input,
        timeout,
        max_output,
        CliKind::GitMutation,
        None,
        Some(&config),
    )
}

fn run_git_with_verified_transport(
    repository: &Repository,
    transport: &RemoteTransport,
    fetch: bool,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
) -> Result<CliOutput, String> {
    let internal_remote = internal_verified_remote_name();
    if internal_remote_config_exists(repository, internal_remote)? {
        return Err("Git remote 配置与内部保留 transport 名称冲突；已拒绝网络操作".into());
    }
    let configuration = transport.configuration.as_ref()
        .ok_or_else(|| "Git transport 缺少已验证的配置快照".to_owned())?;
    let locators = if fetch { &transport.fetch_urls } else { &transport.push_urls };
    let proxy = bound_git_proxy(&configuration.proxies, locators)?;
    let mut config = vec![
        ("protocol.allow".to_owned(), "never".to_owned()),
        ("protocol.ext.allow".to_owned(), "never".to_owned()),
        ("protocol.http.allow".to_owned(), "always".to_owned()),
        ("protocol.https.allow".to_owned(), "always".to_owned()),
        ("protocol.ssh.allow".to_owned(), "always".to_owned()),
        ("protocol.git.allow".to_owned(), "always".to_owned()),
        ("protocol.file.allow".to_owned(), "always".to_owned()),
        ("core.askPass".to_owned(), String::new()),
        ("core.gitProxy".to_owned(), "none".to_owned()),
        ("ssh.variant".to_owned(), "ssh".to_owned()),
        (
            "core.hooksPath".to_owned(),
            disabled_hooks_path().to_owned(),
        ),
        // Scalar and resettable settings are bound to the validated snapshot.
        ("core.sshCommand".to_owned(), configuration.ssh_command.clone()),
        ("credential.helper".to_owned(), String::new()),
    ];
    for helper in &configuration.helpers {
        config.push(("credential.helper".to_owned(), helper.clone()));
    }
    if fetch {
        for locator in &transport.fetch_urls {
            config.push((format!("remote.{internal_remote}.url"), locator.clone()));
        }
    } else {
        for locator in &transport.push_urls {
            config.push((format!("remote.{internal_remote}.url"), locator.clone()));
            config.push((format!("remote.{internal_remote}.pushurl"), locator.clone()));
        }
    }
    let mut prepared = git_command_prefix();
    prepared.extend(args);
    run_program_with_bound_proxy(
        &repository.git,
        &repository.root,
        prepared,
        input,
        timeout,
        max_output,
        CliKind::GitMutation,
        None,
        Some(&config),
        Some(&proxy),
    )
}

// Git's core.gitProxy list uses the first matching entry, not last-value-wins.
// Resolve its domain rules before spawning, then pin the result via the dedicated
// environment override (there is no config-level list reset for this key).
fn bound_git_proxy(proxies: &[String], locators: &[String]) -> Result<String, String> {
    // Fetch uses the first URL; push has already been restricted to one URL.
    let Some(locator) = locators.first().filter(|locator| locator.starts_with("git://")) else {
        return Ok(String::new());
    };
    let url = url::Url::parse(locator).map_err(|_| "Git proxy locator 无效".to_owned())?;
    let host = url.host_str().ok_or_else(|| "Git proxy host 缺失".to_owned())?;
    Ok(proxies.iter().find_map(|value| {
        let (command, domain) = value.split_once(" for ")
            .map_or((value.as_str(), None), |(command, domain)| (command, Some(domain)));
        let matches = domain.is_none_or(|domain| {
            host == domain || host.strip_suffix(domain).is_some_and(|prefix| prefix.ends_with('.'))
        });
        matches.then(|| if command == "none" { String::new() } else { command.to_owned() })
    }).unwrap_or_default())
}

fn internal_remote_config_exists(
    repository: &Repository,
    internal_remote: &str,
) -> Result<bool, String> {
    let pattern = format!("^remote\\.{internal_remote}\\.");
    let output = run_git(
        repository,
        [
            OsString::from("config"),
            OsString::from("--name-only"),
            OsString::from("--get-regexp"),
            OsString::from(pattern),
        ],
        None,
        LOCAL_COMMAND_TIMEOUT,
        64 * 1024,
        true,
    )?;
    if output.success() {
        Ok(!output.stdout.is_empty())
    } else if !output.timed_out && output.exit_code() == Some(1) {
        Ok(false)
    } else {
        Err("无法验证内部 Git transport 配置隔离".into())
    }
}

fn disabled_hooks_path() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn safe_git_ssh_command(repository: &Repository) -> Option<String> {
    let git_parent = repository.git.parent()?;
    let git_root = git_parent.parent().unwrap_or(git_parent);
    #[cfg(windows)]
    let candidates = [
        git_root.join("usr/bin/ssh.exe"),
        git_root.join("mingw64/bin/ssh.exe"),
        git_parent.join("ssh.exe"),
    ];
    #[cfg(not(windows))]
    let candidates = [git_parent.join("ssh"), PathBuf::from("/usr/bin/ssh")];
    let ssh = candidates
        .iter()
        .find_map(|candidate| canonical_file(candidate))?;
    let value = ssh.to_string_lossy();
    if value
        .chars()
        .any(|character| character == '\0' || character == '\r' || character == '\n')
    {
        return None;
    }
    #[cfg(windows)]
    {
        if value.contains('"') {
            return None;
        }
        Some(format!("\"{}\"", value.replace('\\', "/")))
    }
    #[cfg(not(windows))]
    {
        Some(format!("'{}'", value.replace('\'', "'\\''")))
    }
}

fn run_git_with_internal_index(
    repository: &Repository,
    index_path: &Path,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    passive: bool,
) -> Result<CliOutput, String> {
    let expected = repository.index_path.with_file_name("index.lock");
    if !index_path.is_absolute() || !same_path(index_path, &expected) {
        return Err("内部 Git index 路径不属于当前 worktree 的受控事务".into());
    }
    let mut prepared = git_command_prefix();
    prepared.extend(args);
    run_program_with_internal_git_settings(
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
        Some(index_path),
        None,
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

fn run_gh(
    gh: &Path,
    cwd: &Path,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
) -> Result<CliOutput, String> {
    run_program(gh, cwd, args, input, timeout, max_output, CliKind::Github)
}

#[derive(Clone, Copy)]
enum CliKind {
    GitPassive,
    GitMutation,
    Github,
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
    run_program_with_internal_git_settings(
        program, cwd, args, input, timeout, max_output, kind, None, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_program_with_internal_git_settings(
    program: &Path,
    cwd: &Path,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    kind: CliKind,
    internal_git_index: Option<&Path>,
    internal_git_config: Option<&[(String, String)]>,
) -> Result<CliOutput, String> {
    run_program_with_bound_proxy(
        program, cwd, args, input, timeout, max_output, kind,
        internal_git_index, internal_git_config, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_program_with_bound_proxy(
    program: &Path,
    cwd: &Path,
    args: impl IntoIterator<Item = OsString>,
    input: Option<Vec<u8>>,
    timeout: Duration,
    max_output: usize,
    kind: CliKind,
    internal_git_index: Option<&Path>,
    internal_git_config: Option<&[(String, String)]>,
    bound_proxy: Option<&str>,
) -> Result<CliOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_cli_environment(&mut command, kind);
    if let Some(index_path) = internal_git_index {
        // This override is deliberately installed only after all inherited and
        // caller-supplied Git routing variables have been removed. Callers can
        // only reach this path through run_git_with_internal_index, which
        // accepts the worktree-specific index.lock path constructed by Mework.
        command.env("GIT_INDEX_FILE", git_cli_environment_path(index_path));
    }
    if let Some(proxy) = bound_proxy {
        // Unlike scalar -c overrides, this bypasses inherited core.gitProxy's
        // first-match list. Empty explicitly disables proxying, including drift.
        command.env("GIT_PROXY_COMMAND", proxy);
    }
    if let Some(config) = internal_git_config {
        command.env("GIT_CONFIG_COUNT", config.len().to_string());
        for (index, (key, value)) in config.iter().enumerate() {
            command.env(format!("GIT_CONFIG_KEY_{index}"), key);
            command.env(format!("GIT_CONFIG_VALUE_{index}"), value);
        }
    }
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
            .flatten();
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
        } else if name.eq_ignore_ascii_case("gh") {
            if let Some(program_files) = env::var_os("ProgramFiles") {
                candidates.push(PathBuf::from(program_files).join("GitHub CLI/gh.exe"));
            }
            if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
                candidates.push(PathBuf::from(local_app_data).join("Programs/GitHub CLI/gh.exe"));
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
    candidates
        .into_iter()
        .find_map(|path| canonical_file(&path))
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
    let mut options = OpenOptions::new();
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

#[cfg(test)]
fn parse_remote_for_test(value: &str) -> Option<(String, String, String)> {
    parse_remote_url(value).map(|remote| (remote.host, remote.owner, remote.name))
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

    fn assert_stage_all_locks_absent(repository: &Repository) {
        assert!(!repository.index_path.with_file_name("index.lock").exists());
        assert!(!repository
            .index_path
            .with_file_name("index.lock.lock")
            .exists());
    }

    fn prepared_commit_action(root: &Path, message: &str) -> GitAction {
        let preparation = prepare_commit(root, message).unwrap();
        GitAction::Commit {
            message: message.to_owned(),
            expected_target_revision: preparation.target_revision,
            expected_tree_oid: preparation.candidate_tree_oid,
            amend: false,
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

    fn github_repository_fixture(viewer_login: Option<&str>) -> GithubRepository {
        GithubRepository {
            host: "github.example".into(),
            owner: "team".into(),
            name: "project".into(),
            name_with_owner: "team/project".into(),
            url: "https://github.example/team/project".into(),
            default_branch: Some("main".into()),
            viewer_login: viewer_login.map(str::to_owned),
            authenticated: true,
            gh_version: "2.75.0".into(),
        }
    }

    fn github_merge_readiness_fixture(merge_state_status: &str) -> GithubPullRequestReadiness {
        let repository = GithubReadinessRepositoryIdentity {
            host: "github.example".into(),
            node_id: "R_repo".into(),
            name_with_owner: "team/project".into(),
        };
        let identity = GithubPullRequestCoreIdentity {
            repository: repository.clone(),
            pull_request_node_id: "PR_17".into(),
            number: 17,
            state: "open".into(),
            draft: false,
            base_repository: repository,
            head_repository: Some(GithubReadinessRepositoryIdentity {
                host: "github.example".into(),
                node_id: "R_fork".into(),
                name_with_owner: "contributor/project".into(),
            }),
            base_ref_name: "main".into(),
            base_ref_oid: "89abcdef0123456789abcdef0123456789abcdef".into(),
            head_ref_name: "feature/review".into(),
            head_ref_oid: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        let mut readiness = GithubPullRequestReadiness {
            identity,
            merge_policy: GithubPullRequestMergePolicy {
                merge_state_status: merge_state_status.into(),
                mergeable: "MERGEABLE".into(),
                merge_commit_allowed: true,
                squash_merge_allowed: true,
                rebase_merge_allowed: true,
            },
            viewer: GithubPullRequestViewer {
                login: "viewer".into(),
                can_update: true,
                can_merge_as_admin: true,
            },
            checks: GithubReadinessPhase::available(GithubPullRequestChecks {
                total_count: 0,
                checks: Vec::new(),
            }),
            viewer_default: github_readiness_unsupported_phase(
                "GitHub viewer default merge method",
            ),
            auto_merge: github_readiness_unsupported_phase("GitHub auto-merge"),
            merge_queue: GithubReadinessPhase::available(GithubPullRequestMergeQueueState {
                enabled: false,
                is_in_queue: false,
                entry: None,
            }),
            identity_revision: String::new(),
            readiness_revision: String::new(),
        };
        refresh_github_readiness_revisions(&mut readiness);
        readiness
    }

    fn github_readiness_phase_response_fixture(readiness: &GithubPullRequestReadiness) -> Value {
        let identity = &readiness.identity;
        let head_repository = identity.head_repository.as_ref().map(|repository| {
            serde_json::json!({
                "id": repository.node_id,
                "nameWithOwner": repository.name_with_owner
            })
        });
        serde_json::json!({
            "data": {
                "viewer": { "login": readiness.viewer.login },
                "repository": {
                    "id": identity.repository.node_id,
                    "nameWithOwner": identity.repository.name_with_owner,
                    "pullRequest": {
                        "id": identity.pull_request_node_id,
                        "number": identity.number,
                        "state": identity.state.to_ascii_uppercase(),
                        "isDraft": identity.draft,
                        "baseRefName": identity.base_ref_name,
                        "baseRefOid": identity.base_ref_oid,
                        "headRefName": identity.head_ref_name,
                        "headRefOid": identity.head_ref_oid,
                        "baseRepository": {
                            "id": identity.base_repository.node_id,
                            "nameWithOwner": identity.base_repository.name_with_owner
                        },
                        "headRepository": head_repository
                    }
                }
            }
        })
    }

    fn refresh_github_readiness_revisions(readiness: &mut GithubPullRequestReadiness) {
        readiness.identity_revision =
            github_readiness_revision(b"mework.github-readiness.identity.v1", &readiness.identity)
                .unwrap();
        readiness.readiness_revision = github_readiness_revision(
            b"mework.github-readiness.aggregate.v1",
            &(
                &readiness.identity_revision,
                &readiness.merge_policy,
                &readiness.viewer,
                &readiness.checks,
                &readiness.viewer_default,
                &readiness.auto_merge,
                &readiness.merge_queue,
            ),
        )
        .unwrap();
    }

    fn validate_github_merge_readiness_fixture(
        readiness: &GithubPullRequestReadiness,
        method: GithubMergeMethod,
    ) -> Result<String, String> {
        validate_github_merge_readiness(
            readiness,
            &GithubRepositoryIdentity {
                host: "github.example".into(),
                owner: "team".into(),
                name: "project".into(),
            },
            "viewer",
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "89abcdef0123456789abcdef0123456789abcdef",
            "open",
            &readiness.identity_revision,
            &readiness.readiness_revision,
            method,
        )
    }

    fn github_pull_request_detail_fixture() -> GithubPullRequestDetail {
        GithubPullRequestDetail {
            number: 17,
            title: "Safe review".into(),
            state: "open".into(),
            url: "https://github.example/team/project/pull/17".into(),
            author: Some("cat".into()),
            head_ref_name: "feature/review".into(),
            head_ref_oid: "0123456789abcdef0123456789abcdef01234567".into(),
            base_ref_name: "main".into(),
            draft: false,
            mergeable: Some(true),
            updated_at: "2026-07-24T00:00:00Z".into(),
            body: String::new(),
            additions: 3,
            deletions: 1,
            changed_files: 2,
            commits: 1,
            review_decision: Some("APPROVED".into()),
            status_check_rollup: Some("success".into()),
            checks: Vec::new(),
        }
    }

    fn github_review_threads_response_fixture() -> Value {
        serde_json::json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "number": 17,
                        "state": "OPEN",
                        "headRefOid": "0123456789abcdef0123456789abcdef01234567",
                        "reviewThreads": {
                            "totalCount": 2,
                            "pageInfo": {
                                "hasNextPage": true,
                                "endCursor": "thread-cursor-2"
                            },
                            "nodes": [{
                                "id": "PRRT_thread_1",
                                "path": "src/review.rs",
                                "line": 12,
                                "startLine": null,
                                "diffSide": "RIGHT",
                                "startDiffSide": null,
                                "originalLine": 10,
                                "originalStartLine": null,
                                "isResolved": false,
                                "isOutdated": false,
                                "viewerCanReply": true,
                                "viewerCanResolve": true,
                                "viewerCanUnresolve": false,
                                "comments": {
                                    "totalCount": 51,
                                    "pageInfo": {
                                        "hasNextPage": true,
                                        "endCursor": "comment-cursor-50"
                                    },
                                    "nodes": [{
                                        "id": "PRRC_comment_1",
                                        "author": {"login": "cat"},
                                        "body": "Please keep the proof bound.",
                                        "createdAt": "2026-07-24T00:00:00Z",
                                        "updatedAt": "2026-07-24T00:01:00Z",
                                        "url": "https://github.example/team/project/pull/17#discussion_r1",
                                        "replyTo": null
                                    }]
                                }
                            }]
                        }
                    }
                }
            }
        })
    }

    fn github_review_thread_scope_response_fixture() -> Value {
        serde_json::json!({
            "data": {
                "node": {
                    "__typename": "PullRequestReviewThread",
                    "id": "PRRT_thread_1",
                    "viewerCanReply": true,
                    "viewerCanResolve": true,
                    "viewerCanUnresolve": false,
                    "repository": {
                        "name": "project",
                        "owner": {"login": "team"}
                    },
                    "pullRequest": {
                        "number": 17,
                        "state": "OPEN",
                        "headRefOid": "0123456789abcdef0123456789abcdef01234567"
                    }
                }
            }
        })
    }

    fn github_review_thread_comments_response_fixture() -> Value {
        serde_json::json!({
            "data": {
                "node": {
                    "__typename": "PullRequestReviewThread",
                    "id": "PRRT_thread_1",
                    "comments": {
                        "totalCount": 52,
                        "pageInfo": {
                            "hasNextPage": true,
                            "endCursor": "comment-cursor-100"
                        },
                        "nodes": [{
                            "id": "PRRC_comment_51",
                            "author": {"login": "reviewer"},
                            "body": "Second page reply.",
                            "createdAt": "2026-07-24T01:00:00Z",
                            "updatedAt": "2026-07-24T01:01:00Z",
                            "url": "https://github.example/team/project/pull/17#discussion_r51",
                            "replyTo": {"id": "PRRC_comment_1"}
                        }]
                    }
                }
            }
        })
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
        assert!(branches.trim().is_empty(), "空工作树的分支也该删掉：{branches}");

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
        for path in ["z-last.txt", "a-first.txt", "middle.txt"] {
            fs::write(repository.path().join(path), format!("{path}\n")).unwrap();
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
                expected_stage_all_target_revision: None,
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
                expected_stage_all_target_revision: None,
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
                expected_stage_all_target_revision: None,
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
                expected_stage_all_target_revision: None,
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
                    expected_stage_all_target_revision: None,
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
                    expected_stage_all_target_revision: None,
                }
            )
            .unwrap(),
            GitChangePageResult::Stale { .. }
        ));
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
        assert!(validate_relative_path("C:/secret").is_err());
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
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "push",
            "remote": "origin",
            "remoteBranch": "main",
            "expectedLocalBranch": "main",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "setUpstream": true,
            "forceWithLease": false
        }))
        .is_err());
        for legacy in [
            serde_json::json!({"type": "fetch", "remote": "origin"}),
            serde_json::json!({
                "type": "pull",
                "remote": "origin",
                "branch": "main",
                "ffOnly": true
            }),
            serde_json::json!({
                "type": "push",
                "remote": "origin",
                "branch": "main",
                "expectedHead": "0123456789abcdef0123456789abcdef01234567"
            }),
        ] {
            assert!(serde_json::from_value::<GitAction>(legacy).is_err());
        }
        let push: GitAction = serde_json::from_value(serde_json::json!({
            "type": "push",
            "expectedRepositoryId": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedWorktreeId": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567",
            "remote": {
                "name": "origin",
                "fetchRevision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "pushRevision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "url": null
            },
            "expectedLocalBranch": "main",
            "remoteBranch": "main",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedUpstream": null,
            "setUpstream": true,
            "forceWithLease": false
        }))
        .unwrap();
        assert!(matches!(
            push,
            GitAction::Push {
                set_upstream: true,
                force_with_lease: false,
                ..
            }
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "push",
            "remote": "origin",
            "branch": "main",
            "setUpstream": true
        }))
        .is_err());
        let branch: GitAction = serde_json::from_value(serde_json::json!({
            "type": "create_branch",
            "name": "feature/test",
            "startPoint": "main",
            "checkout": true
        }))
        .unwrap();
        assert!(matches!(
            branch,
            GitAction::CreateBranch {
                start_point: Some(_),
                checkout: true,
                ..
            }
        ));
        let stage_all: GitAction = serde_json::from_value(serde_json::json!({
            "type": "stage_all",
            "expectedContentRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedTargetRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert!(matches!(
            stage_all,
            GitAction::StageAll {
                expected_content_revision,
                expected_target_revision
            } if expected_content_revision.starts_with("01234567")
                && expected_target_revision.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "stage_all",
            "expectedContentRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }))
        .is_err());
        let commit: GitAction = serde_json::from_value(serde_json::json!({
            "type": "commit",
            "message": "proof-bound commit",
            "expectedTargetRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedTreeOid": "89abcdef0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert!(matches!(
            commit,
            GitAction::Commit {
                expected_target_revision,
                expected_tree_oid,
                amend: false,
                ..
            } if expected_target_revision.starts_with("01234567")
                && expected_tree_oid.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "commit",
            "message": "legacy unbound commit"
        }))
        .is_err());
        let proof_diff: GitDiffRequest = serde_json::from_value(serde_json::json!({
            "type": "working",
            "path": "src/main.rs",
            "expectedStageAllTargetRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert!(matches!(
            proof_diff,
            GitDiffRequest::Working {
                path: Some(path),
                expected_stage_all_target_revision: Some(revision)
            } if path == "src/main.rs" && revision.starts_with("89abcdef")
        ));
        let proof_page: GitChangePageRequest = serde_json::from_value(serde_json::json!({
            "expectedRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "limit": 25,
            "expectedStageAllTargetRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert_eq!(proof_page.limit, 25);
        assert!(proof_page
            .expected_stage_all_target_revision
            .is_some_and(|revision| revision.starts_with("89abcdef")));
        let merge: GithubAction = serde_json::from_value(serde_json::json!({
            "type": "merge_pull_request",
            "expectedRepository": {
                "host": "github.com",
                "owner": "cat",
                "name": "project"
            },
            "expectedViewerLogin": "octocat",
            "number": 7,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedBaseOid": "89abcdef0123456789abcdef0123456789abcdef",
            "expectedState": "open",
            "expectedIdentityRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedReadinessRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567",
            "method": "squash",
            "deleteBranch": true
        }))
        .unwrap();
        assert!(matches!(
            merge,
            GithubAction::MergePullRequest {
                expected_repository,
                expected_viewer_login,
                number: 7,
                expected_head_oid,
                expected_base_oid,
                expected_state,
                expected_identity_revision,
                expected_readiness_revision,
                method: GithubMergeMethod::Squash,
                delete_branch: true
            } if expected_repository.host == "github.com"
                && expected_repository.owner == "cat"
                && expected_repository.name == "project"
                && expected_viewer_login == "octocat"
                && expected_head_oid == "0123456789abcdef0123456789abcdef01234567"
                && expected_base_oid == "89abcdef0123456789abcdef0123456789abcdef"
                && expected_state == "open"
                && expected_identity_revision.starts_with("01234567")
                && expected_readiness_revision.starts_with("89abcdef")
        ));
        // The former head/state-bound payload is intentionally no longer
        // accepted: merge confirmation must also bind the base and readiness.
        assert!(serde_json::from_value::<GithubAction>(serde_json::json!({
            "type": "merge_pull_request",
            "expectedRepository": {
                "host": "github.com",
                "owner": "cat",
                "name": "project"
            },
            "expectedViewerLogin": "octocat",
            "number": 7,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open",
            "method": "squash"
        }))
        .is_err());
        assert!(serde_json::from_value::<GithubAction>(serde_json::json!({
            "type": "merge_pull_request",
            "expectedRepository": {
                "host": "github.com",
                "owner": "cat",
                "name": "project"
            },
            "expectedViewerLogin": "octocat",
            "number": 7,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedBaseOid": "89abcdef0123456789abcdef0123456789abcdef",
            "expectedState": "open",
            "expectedIdentityRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expectedReadinessRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .is_err());
        assert!(serde_json::from_value::<GithubAction>(serde_json::json!({
            "type": "merge_pull_request",
            "number": 7,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open"
        }))
        .is_err());
        let create: GithubAction = serde_json::from_value(serde_json::json!({
            "type": "create_pull_request",
            "expectedRepository": {
                "host": "github.com",
                "owner": "cat",
                "name": "project"
            },
            "expectedViewerLogin": "octocat",
            "expectedLocalHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedContentRevision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "title": "Bound PR creation",
            "body": "Body",
            "base": "main",
            "head": "feature/review",
            "draft": false
        }))
        .unwrap();
        assert!(matches!(
            create,
            GithubAction::CreatePullRequest {
                expected_repository,
                expected_viewer_login,
                expected_local_head_oid,
                expected_content_revision,
                base: Some(base),
                head: Some(head),
                ..
            } if expected_repository.name == "project"
                && expected_viewer_login == "octocat"
                && expected_local_head_oid.starts_with("01234567")
                && expected_content_revision.len() == 64
                && base == "main"
                && head == "feature/review"
        ));
        let checkout: GithubAction = serde_json::from_value(serde_json::json!({
            "type": "checkout_pull_request",
            "expectedRepository": {
                "host": "github.example",
                "owner": "team",
                "name": "project"
            },
            "expectedViewerLogin": "cat",
            "number": 17,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open",
            "expectedLocalHeadOid": "89abcdef0123456789abcdef0123456789abcdef",
            "expectedContentRevision": "89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        }))
        .unwrap();
        assert!(matches!(
            checkout,
            GithubAction::CheckoutPullRequest {
                number: 17,
                expected_state,
                expected_content_revision,
                ..
            } if expected_state == "open" && expected_content_revision.len() == 64
        ));
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
        let local_merge: GitAction = serde_json::from_value(serde_json::json!({
            "type": "merge",
            "branch": "feature/review",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedBranchOid": "89abcdef0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert!(matches!(
            local_merge,
            GitAction::Merge {
                expected_head,
                expected_branch_oid,
                ..
            } if expected_head.starts_with("01234567")
                && expected_branch_oid.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "merge",
            "branch": "feature/review"
        }))
        .is_err());
        let delete_branch: GitAction = serde_json::from_value(serde_json::json!({
            "type": "delete_branch",
            "name": "feature/review",
            "expectedHead": "0123456789abcdef0123456789abcdef01234567",
            "expectedOid": "89abcdef0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert!(matches!(
            delete_branch,
            GitAction::DeleteBranch {
                expected_head,
                expected_oid,
                ..
            } if expected_head.starts_with("01234567") && expected_oid.starts_with("89abcdef")
        ));
        assert!(serde_json::from_value::<GitAction>(serde_json::json!({
            "type": "delete_branch",
            "name": "feature/review",
            "expectedOid": "89abcdef0123456789abcdef0123456789abcdef"
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
    fn github_selectors_always_include_the_authenticated_host() {
        let github_com = parse_remote_url("https://github.com/openai/codex.git").unwrap();
        assert_eq!(
            github_repository_selector(&github_com),
            "github.com/openai/codex"
        );
        let enterprise =
            parse_remote_url("ssh://git@github.corp.example/team/project.git").unwrap();
        assert_eq!(
            github_repository_selector(&enterprise),
            "github.corp.example/team/project"
        );
    }

    #[test]
    fn github_write_identity_binds_repository_and_active_viewer() {
        let actual = github_repository_fixture(Some("OctoCat"));
        let expected = GithubRepositoryIdentity {
            host: "GITHUB.EXAMPLE".into(),
            owner: "Team".into(),
            name: "Project".into(),
        };
        assert!(validate_github_write_identity(&expected, "octocat", &actual).is_ok());

        let mut wrong_repository = expected.clone();
        wrong_repository.name = "other".into();
        assert!(
            validate_github_write_identity(&wrong_repository, "octocat", &actual)
                .unwrap_err()
                .contains("仓库身份")
        );
        assert!(
            validate_github_write_identity(&expected, "another-user", &actual)
                .unwrap_err()
                .contains("登录账号")
        );
        assert!(validate_github_write_identity(
            &expected,
            "octocat",
            &github_repository_fixture(None)
        )
        .unwrap_err()
        .contains("无法确认"));
    }

    #[test]
    fn github_pull_request_write_expectations_bind_head_and_state() {
        let detail = github_pull_request_detail_fixture();
        assert_eq!(
            validate_github_pull_request_expectation(
                &detail,
                "0123456789ABCDEF0123456789ABCDEF01234567",
                "OPEN",
                "open"
            )
            .unwrap(),
            detail.head_ref_oid
        );
        assert!(validate_github_pull_request_expectation(
            &detail,
            "89abcdef0123456789abcdef0123456789abcdef",
            "open",
            "open"
        )
        .is_err());
        assert!(validate_github_pull_request_expectation(
            &detail,
            &detail.head_ref_oid,
            "closed",
            "open"
        )
        .is_err());
        assert!(validate_github_pull_request_expectation(
            &detail,
            &detail.head_ref_oid,
            "open",
            "closed"
        )
        .is_err());
    }

    #[test]
    fn immediate_merge_uses_host_scoped_rest_args_and_bound_payload() {
        let repository = github_repository_fixture(Some("cat"));
        let args = github_merge_api_args(&repository, 17)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.example",
                "-X",
                "PUT",
                "repos/team/project/pulls/17/merge",
                "--input",
                "-",
            ]
        );
        let input = github_merge_api_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubMergeMethod::Squash,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&input).unwrap(),
            serde_json::json!({
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "merge_method": "squash"
            })
        );
        assert!(validate_github_merge_delete_branch(false).is_ok());
        assert!(validate_github_merge_delete_branch(true)
            .unwrap_err()
            .contains("不支持同时删除分支"));
    }

    #[test]
    fn immediate_merge_response_requires_merged_true() {
        assert_eq!(
            parse_github_merge_response(
                br#"{"sha":"89abcdef0123456789abcdef0123456789abcdef","merged":true,"message":"Pull Request successfully merged"}"#
            )
            .unwrap()
            .as_deref(),
            Some("Pull Request successfully merged")
        );
        let rejected = parse_github_merge_response(
            br#"{"sha":null,"merged":false,"message":"Head branch was modified"}"#,
        )
        .unwrap_err();
        assert!(rejected.contains("未立即合并"));
        assert!(rejected.contains("没有启用自动合并"));
        assert!(parse_github_merge_response(br#"{"message":"missing merged"}"#).is_err());
        assert!(parse_github_merge_response(b"not-json").is_err());
    }

    #[test]
    fn deserializes_review_requests_and_identity_bound_actions() {
        let request: GithubReviewThreadsRequest = serde_json::from_value(serde_json::json!({
            "number": 17,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "cursor": "thread-cursor-2"
        }))
        .unwrap();
        assert_eq!(request.number, 17);
        assert_eq!(request.page_size, DEFAULT_GITHUB_REVIEW_THREADS_PAGE_SIZE);

        let submit: GithubAction = serde_json::from_value(serde_json::json!({
            "type": "submit_pull_request_review",
            "expectedRepository": {
                "host": "github.example",
                "owner": "team",
                "name": "project"
            },
            "expectedViewerLogin": "cat",
            "number": 17,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open",
            "event": "request_changes",
            "body": "Please revise",
            "comments": [{
                "path": "src/review.rs",
                "line": 12,
                "side": "RIGHT",
                "body": "Keep this check atomic."
            }]
        }))
        .unwrap();
        assert!(matches!(
            submit,
            GithubAction::SubmitPullRequestReview {
                event: GithubPullRequestReviewEvent::RequestChanges,
                comments,
                ..
            } if comments.len() == 1 && comments[0].side == "RIGHT"
        ));
        assert!(serde_json::from_value::<GithubAction>(serde_json::json!({
            "type": "reply_review_thread",
            "number": 17,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open",
            "threadId": "PRRT_thread_1",
            "body": "reply"
        }))
        .is_err());
        let resolve: GithubAction = serde_json::from_value(serde_json::json!({
            "type": "resolve_review_thread",
            "expectedRepository": {
                "host": "github.example",
                "owner": "team",
                "name": "project"
            },
            "expectedViewerLogin": "cat",
            "number": 17,
            "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
            "expectedState": "open",
            "threadId": "PRRT_thread_1"
        }))
        .unwrap();
        assert!(matches!(
            resolve,
            GithubAction::ResolveReviewThread { number: 17, .. }
        ));
    }

    #[test]
    fn review_thread_query_is_host_scoped_head_bound_and_strictly_paged() {
        let repository = github_repository_fixture(Some("cat"));
        let args = github_graphql_args(&repository.host)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "api",
                "graphql",
                "--hostname",
                "github.example",
                "--input",
                "-"
            ]
        );
        let input = github_review_threads_query_input(&repository, 17, 30, Some("thread-cursor-1"))
            .unwrap();
        let input: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(
            input.get("query").and_then(Value::as_str),
            Some(GITHUB_REVIEW_THREADS_QUERY)
        );
        assert_eq!(input["variables"]["owner"], "team");
        assert_eq!(input["variables"]["name"], "project");
        assert_eq!(input["variables"]["number"], 17);
        assert_eq!(input["variables"]["first"], 30);
        assert_eq!(input["variables"]["after"], "thread-cursor-1");

        let response = github_review_threads_response_fixture();
        let parsed = parse_github_review_threads_response(
            &serde_json::to_vec(&response).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            30,
        )
        .unwrap();
        assert_eq!(parsed.number, 17);
        assert_eq!(parsed.total_count, 2);
        assert_eq!(parsed.next_cursor.as_deref(), Some("thread-cursor-2"));
        assert_eq!(parsed.threads.len(), 1);
        assert_eq!(parsed.threads[0].diff_side, "RIGHT");
        assert_eq!(parsed.threads[0].line, Some(12));
        assert_eq!(parsed.threads[0].comments_total_count, 51);
        assert_eq!(
            parsed.threads[0].comments_next_cursor.as_deref(),
            Some("comment-cursor-50")
        );
        assert_eq!(parsed.threads[0].comments[0].author.as_deref(), Some("cat"));

        assert!(parse_github_review_threads_response(
            &serde_json::to_vec(&response).unwrap(),
            17,
            "89abcdef0123456789abcdef0123456789abcdef",
            30,
        )
        .unwrap_err()
        .contains("head"));
        let mut graphql_error = response.clone();
        graphql_error["errors"] = serde_json::json!([{"message": "scope denied for ghp_secret"}]);
        assert!(parse_github_review_threads_response(
            &serde_json::to_vec(&graphql_error).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            30,
        )
        .unwrap_err()
        .contains("GraphQL 返回错误"));
        let mut invalid_side = response.clone();
        invalid_side["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]
            ["diffSide"] = Value::String("CENTER".into());
        assert!(parse_github_review_threads_response(
            &serde_json::to_vec(&invalid_side).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            30,
        )
        .is_err());
        let mut invalid_line = response;
        invalid_line["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["line"] =
            Value::from(0);
        assert!(parse_github_review_threads_response(
            &serde_json::to_vec(&invalid_line).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            30,
        )
        .is_err());
        assert!(validate_github_review_cursor(Some("cursor with spaces")).is_err());
        assert!(validate_github_review_threads_page_size(0).is_err());
        assert!(validate_github_review_threads_page_size(1).is_ok());
        assert!(validate_github_review_threads_page_size(100).is_ok());
        assert!(validate_github_review_threads_page_size(101).is_err());
        assert!(
            github_review_threads_query_input(&repository, i32::MAX as u64 + 1, 30, None).is_err()
        );
    }

    #[test]
    fn review_thread_comment_request_is_identity_bound_and_strictly_paged() {
        let request: GithubReviewThreadCommentsRequest =
            serde_json::from_value(serde_json::json!({
                "expectedRepository": {
                    "host": "github.example",
                    "owner": "team",
                    "name": "project"
                },
                "expectedViewerLogin": "cat",
                "number": 17,
                "expectedState": "open",
                "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
                "threadId": "PRRT_thread_1",
                "cursor": "comment-cursor-50"
            }))
            .unwrap();
        assert_eq!(request.page_size, DEFAULT_GITHUB_REVIEW_COMMENTS_PAGE_SIZE);
        assert_eq!(request.expected_repository.host, "github.example");
        assert_eq!(request.expected_viewer_login, "cat");
        assert!(
            serde_json::from_value::<GithubReviewThreadCommentsRequest>(serde_json::json!({
                "number": 17,
                "expectedState": "open",
                "expectedHeadOid": "0123456789abcdef0123456789abcdef01234567",
                "threadId": "PRRT_thread_1"
            }))
            .is_err()
        );
        assert!(validate_github_review_comments_page_size(0).is_err());
        assert!(validate_github_review_comments_page_size(1).is_ok());
        assert!(validate_github_review_comments_page_size(100).is_ok());
        assert!(validate_github_review_comments_page_size(101).is_err());
    }

    #[test]
    fn review_thread_comment_page_query_and_response_are_fail_closed() {
        let input = github_review_thread_comments_query_input(
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap();
        let input: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(
            input.get("query").and_then(Value::as_str),
            Some(GITHUB_REVIEW_THREAD_COMMENTS_QUERY)
        );
        assert_eq!(input["variables"]["threadId"], "PRRT_thread_1");
        assert_eq!(input["variables"]["first"], 50);
        assert_eq!(input["variables"]["after"], "comment-cursor-50");

        let response = github_review_thread_comments_response_fixture();
        let parsed = parse_github_review_thread_comments_response(
            &serde_json::to_vec(&response).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap();
        assert_eq!(parsed.number, 17);
        assert_eq!(
            parsed.head_ref_oid,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(parsed.thread_id, "PRRT_thread_1");
        assert_eq!(parsed.total_count, 52);
        assert_eq!(parsed.next_cursor.as_deref(), Some("comment-cursor-100"));
        assert_eq!(parsed.comments.len(), 1);
        assert_eq!(parsed.comments[0].id, "PRRC_comment_51");
        assert_eq!(
            parsed.comments[0].reply_to_id.as_deref(),
            Some("PRRC_comment_1")
        );

        let mut graphql_error = response.clone();
        graphql_error["errors"] = serde_json::json!([{"message": "forbidden ghp_secret"}]);
        assert!(parse_github_review_thread_comments_response(
            &serde_json::to_vec(&graphql_error).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap_err()
        .contains("GraphQL 返回错误"));

        let mut wrong_thread = response.clone();
        wrong_thread["data"]["node"]["id"] = Value::String("PRRT_thread_2".into());
        assert!(parse_github_review_thread_comments_response(
            &serde_json::to_vec(&wrong_thread).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap_err()
        .contains("ID 不匹配"));

        let mut stalled_cursor = response.clone();
        stalled_cursor["data"]["node"]["comments"]["pageInfo"]["endCursor"] =
            Value::String("comment-cursor-50".into());
        assert!(parse_github_review_thread_comments_response(
            &serde_json::to_vec(&stalled_cursor).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap_err()
        .contains("游标未前进"));

        let mut empty_page = response.clone();
        empty_page["data"]["node"]["comments"]["nodes"] = Value::Array(Vec::new());
        assert!(parse_github_review_thread_comments_response(
            &serde_json::to_vec(&empty_page).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            Some("comment-cursor-50"),
        )
        .unwrap_err()
        .contains("空页面"));

        let mut duplicate_page = response;
        let duplicate = duplicate_page["data"]["node"]["comments"]["nodes"][0].clone();
        duplicate_page["data"]["node"]["comments"]["totalCount"] = Value::from(2);
        duplicate_page["data"]["node"]["comments"]["pageInfo"]["hasNextPage"] = Value::Bool(false);
        duplicate_page["data"]["node"]["comments"]["pageInfo"]["endCursor"] = Value::Null;
        duplicate_page["data"]["node"]["comments"]["nodes"] =
            Value::Array(vec![duplicate.clone(), duplicate]);
        assert!(parse_github_review_thread_comments_response(
            &serde_json::to_vec(&duplicate_page).unwrap(),
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "PRRT_thread_1",
            50,
            None,
        )
        .unwrap_err()
        .contains("重复评论 ID"));
    }

    #[test]
    fn review_thread_scope_binds_repo_pr_head_state_and_viewer_permission() {
        let response = github_review_thread_scope_response_fixture();
        let scope = parse_github_review_thread_scope_response(
            &serde_json::to_vec(&response).unwrap(),
            "PRRT_thread_1",
        )
        .unwrap();
        let repository = github_repository_fixture(Some("cat"));
        validate_github_review_thread_scope(
            &scope,
            &repository,
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "open",
            GithubReviewThreadPermission::Reply,
        )
        .unwrap();
        validate_github_review_thread_scope(
            &scope,
            &repository,
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "open",
            GithubReviewThreadPermission::Resolve,
        )
        .unwrap();
        assert!(validate_github_review_thread_scope(
            &scope,
            &repository,
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "open",
            GithubReviewThreadPermission::Unresolve,
        )
        .unwrap_err()
        .contains("权限"));
        let wrong_repository = GithubRepository {
            name: "other".into(),
            ..repository.clone()
        };
        assert!(validate_github_review_thread_scope(
            &scope,
            &wrong_repository,
            17,
            "0123456789abcdef0123456789abcdef01234567",
            "open",
            GithubReviewThreadPermission::Reply,
        )
        .unwrap_err()
        .contains("不属于"));
        assert!(validate_github_review_thread_scope(
            &scope,
            &repository,
            17,
            "89abcdef0123456789abcdef0123456789abcdef",
            "open",
            GithubReviewThreadPermission::Reply,
        )
        .is_err());

        let input = github_review_thread_scope_input("PRRT_thread_1").unwrap();
        let input: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(
            input.get("query").and_then(Value::as_str),
            Some(GITHUB_REVIEW_THREAD_SCOPE_QUERY)
        );
        assert_eq!(input["variables"]["threadId"], "PRRT_thread_1");
        let mut errors = response;
        errors["errors"] = serde_json::json!([{"message": "forbidden"}]);
        assert!(parse_github_review_thread_scope_response(
            &serde_json::to_vec(&errors).unwrap(),
            "PRRT_thread_1"
        )
        .is_err());
    }

    #[test]
    fn review_submission_rest_payload_binds_sha_event_and_inline_coordinates() {
        let repository = github_repository_fixture(Some("cat"));
        let args = github_review_submission_args(&repository, 17)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "api",
                "--hostname",
                "github.example",
                "-X",
                "POST",
                "repos/team/project/pulls/17/reviews",
                "--input",
                "-"
            ]
        );
        let inline = GithubReviewSubmissionComment {
            path: "src/review.rs".into(),
            line: 12,
            side: "RIGHT".into(),
            body: "Keep this bound.".into(),
        };
        let input = github_review_submission_input(
            "0123456789ABCDEF0123456789ABCDEF01234567",
            GithubPullRequestReviewEvent::RequestChanges,
            Some("Please revise.".into()),
            vec![inline.clone()],
        )
        .unwrap();
        let input: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(
            input["commit_id"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(input["event"], "REQUEST_CHANGES");
        assert_eq!(input["body"], "Please revise.");
        assert_eq!(input["comments"][0]["path"], "src/review.rs");
        assert_eq!(input["comments"][0]["line"], 12);
        assert_eq!(input["comments"][0]["side"], "RIGHT");

        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            None,
            Vec::new()
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            None,
            vec![inline.clone()]
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::RequestChanges,
            None,
            vec![inline.clone()]
        )
        .is_err());
        assert!(github_review_submission_input(
            "bad-sha",
            GithubPullRequestReviewEvent::Approve,
            None,
            Vec::new()
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            Some("body".into()),
            vec![GithubReviewSubmissionComment {
                side: "right".into(),
                ..inline.clone()
            }]
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            Some("body".into()),
            vec![GithubReviewSubmissionComment {
                line: i32::MAX as u64 + 1,
                ..inline.clone()
            }]
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            Some("body".into()),
            vec![GithubReviewSubmissionComment {
                path: "../secret".into(),
                ..inline.clone()
            }]
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Comment,
            Some("body".into()),
            vec![GithubReviewSubmissionComment {
                body: "  ".into(),
                ..inline.clone()
            }]
        )
        .is_err());
        assert!(github_review_submission_input(
            "0123456789abcdef0123456789abcdef01234567",
            GithubPullRequestReviewEvent::Approve,
            None,
            vec![inline; MAX_GITHUB_REVIEW_SUBMISSION_COMMENTS + 1]
        )
        .is_err());
        assert!(parse_github_review_submission_response(
            br#"{"id":123,"state":"CHANGES_REQUESTED"}"#
        )
        .is_ok());
        assert!(parse_github_review_submission_response(br#"{"state":"APPROVED"}"#).is_err());
    }

    #[test]
    fn review_thread_mutations_require_graphql_success_and_exact_state() {
        let input = github_review_thread_mutation_input(
            GITHUB_REPLY_REVIEW_THREAD_MUTATION,
            "PRRT_thread_1",
            Some("Bound reply"),
        )
        .unwrap();
        let input: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(
            input.get("query").and_then(Value::as_str),
            Some(GITHUB_REPLY_REVIEW_THREAD_MUTATION)
        );
        assert_eq!(input["variables"]["threadId"], "PRRT_thread_1");
        assert_eq!(input["variables"]["body"], "Bound reply");
        assert!(github_review_thread_mutation_input(
            GITHUB_REPLY_REVIEW_THREAD_MUTATION,
            "bad thread",
            Some("reply")
        )
        .is_err());

        let reply = br#"{"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"PRRC_reply_1","url":"https://github.example/reply/1"}}}}"#;
        assert!(parse_github_review_thread_reply_response(reply).is_ok());
        let reply_error = br#"{"data":null,"errors":[{"message":"permission denied"}]}"#;
        assert!(parse_github_review_thread_reply_response(reply_error).is_err());

        let resolved = br#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_thread_1","isResolved":true}}}}"#;
        assert!(
            parse_github_review_thread_resolution_response(resolved, "PRRT_thread_1", true).is_ok()
        );
        assert!(
            parse_github_review_thread_resolution_response(resolved, "PRRT_thread_2", true)
                .is_err()
        );
        assert!(
            parse_github_review_thread_resolution_response(resolved, "PRRT_thread_1", false)
                .is_err()
        );
    }

    #[test]
    fn pull_request_details_require_a_full_head_oid() {
        let detail = serde_json::json!({
            "number": 17,
            "title": "Safe review",
            "body": "Body",
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "feature/review",
            "headRefOid": "0123456789ABCDEF0123456789ABCDEF01234567",
            "baseRefName": "main",
            "author": {"login": "cat"},
            "updatedAt": "2026-07-24T00:00:00Z",
            "url": "https://github.com/example/repo/pull/17",
            "additions": 3,
            "deletions": 1,
            "changedFiles": 2,
            "commits": [{"oid": "one"}],
            "reviewDecision": "APPROVED",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{
                "__typename": "CheckRun",
                "name": "frontend",
                "workflowName": "CI",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "detailsUrl": "https://github.com/example/repo/actions/runs/1",
                "startedAt": "2026-07-24T00:01:00Z",
                "completedAt": "2026-07-24T00:02:00Z"
            }, {
                "__typename": "StatusContext",
                "context": "deploy/preview",
                "state": "EXPECTED",
                "description": "Waiting for a deployment",
                "targetUrl": "https://example.invalid/deploy/1"
            }]
        });
        let parsed = parse_pr_detail(&detail).unwrap();
        assert_eq!(
            parsed.head_ref_oid,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(parsed.status_check_rollup.as_deref(), Some("pending"));
        assert_eq!(parsed.checks.len(), 2);
        assert_eq!(parsed.checks[0].name, "frontend");
        assert_eq!(parsed.checks[0].state, "success");
        assert_eq!(parsed.checks[0].workflow.as_deref(), Some("CI"));
        assert_eq!(
            parsed.checks[0].link.as_deref(),
            Some("https://github.com/example/repo/actions/runs/1")
        );
        assert_eq!(parsed.checks[1].name, "deploy/preview");
        assert_eq!(parsed.checks[1].state, "pending");
        assert_eq!(
            parsed.checks[1].description.as_deref(),
            Some("Waiting for a deployment")
        );

        let mut missing = detail.clone();
        missing.as_object_mut().unwrap().remove("headRefOid");
        assert!(parse_pr_detail(&missing).is_err());
        let mut invalid = detail;
        invalid["headRefOid"] = Value::String("--match-head-commit".into());
        assert!(parse_pr_detail(&invalid).is_err());
    }

    #[test]
    fn pull_request_diff_read_state_binds_full_head_and_base_oids() {
        let state = serde_json::json!({
            "headRefOid": "0123456789ABCDEF0123456789ABCDEF01234567",
            "baseRefOid": "89abcdef0123456789abcdef0123456789abcdef",
            "changedFiles": 27
        });
        let parsed = parse_github_pull_request_read_state(&state).unwrap();
        assert_eq!(
            parsed.head_ref_oid,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(
            parsed.base_ref_oid,
            "89abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(parsed.changed_files, 27);

        let mut missing_base = state.clone();
        missing_base.as_object_mut().unwrap().remove("baseRefOid");
        assert!(parse_github_pull_request_read_state(&missing_base).is_err());
        let mut abbreviated_base = state;
        abbreviated_base["baseRefOid"] = Value::String("89abcdef".into());
        assert!(parse_github_pull_request_read_state(&abbreviated_base).is_err());
    }

    #[test]
    fn github_pull_request_pages_use_the_authenticated_host_and_explicit_bounds() {
        let repository = GithubRepository {
            host: "github.example".into(),
            owner: "team".into(),
            name: "project".into(),
            name_with_owner: "team/project".into(),
            url: "https://github.example/team/project".into(),
            default_branch: Some("main".into()),
            viewer_login: Some("cat".into()),
            authenticated: true,
            gh_version: "2.75.0".into(),
        };
        let args = github_pull_request_list_args(&repository, 7, 40)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "api",
                "repos/team/project/pulls",
                "--hostname=github.example",
                "--include",
                "--method=GET",
                "--jq",
                GITHUB_PR_LIST_JQ,
                "-H",
                "Accept: application/vnd.github+json",
                "-f",
                "state=open",
                "-f",
                "sort=updated",
                "-f",
                "direction=desc",
                "-f",
                "page=7",
                "-f",
                "per_page=40",
            ]
        );
        assert!(validate_github_pr_page(1, 1).is_ok());
        assert!(validate_github_pr_page(MAX_GITHUB_PR_PAGE, 100).is_ok());
        assert!(validate_github_pr_page(0, 30).is_err());
        assert!(validate_github_pr_page(1, 0).is_err());
        assert!(validate_github_pr_page(1, 101).is_err());
    }

    #[test]
    fn parses_rest_pull_request_pages_and_authoritative_next_links() {
        let response = concat!(
            "HTTP/2.0 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "Link: <https://github.example/api/v3/repositories/1/pulls?page=3>; rel=\"next\", ",
            "<https://github.example/api/v3/repositories/1/pulls?page=9>; rel=\"last\"\r\n",
            "\r\n",
            "[{",
            "\"number\":17,",
            "\"title\":\"Paged review\",",
            "\"state\":\"open\",",
            "\"draft\":true,",
            "\"head\":{\"ref\":\"feature/review\"},",
            "\"base\":{\"ref\":\"main\"},",
            "\"user\":{\"login\":\"cat\"},",
            "\"updated_at\":\"2026-07-24T00:00:00Z\",",
            "\"html_url\":\"https://github.example/team/project/pull/17\"",
            "}]"
        );
        let (pull_requests, has_more) =
            parse_github_pull_request_page(response.as_bytes()).unwrap();
        assert!(has_more);
        assert_eq!(pull_requests.len(), 1);
        assert_eq!(pull_requests[0].number, 17);
        assert_eq!(pull_requests[0].head_ref_name, "feature/review");
        assert_eq!(pull_requests[0].base_ref_name, "main");
        assert_eq!(pull_requests[0].author.as_deref(), Some("cat"));
        assert!(pull_requests[0].draft);
        assert_eq!(pull_requests[0].mergeable, None);

        let final_page = response.replace(
            concat!(
                "Link: <https://github.example/api/v3/repositories/1/pulls?page=3>; rel=\"next\", ",
                "<https://github.example/api/v3/repositories/1/pulls?page=9>; rel=\"last\"\r\n"
            ),
            "",
        );
        assert!(
            !parse_github_pull_request_page(final_page.as_bytes())
                .unwrap()
                .1
        );
    }

    #[test]
    fn pull_request_file_pages_are_host_scoped_bounded_and_patch_selective() {
        let repository = GithubRepository {
            host: "github.example".into(),
            owner: "team".into(),
            name: "project".into(),
            name_with_owner: "team/project".into(),
            url: "https://github.example/team/project".into(),
            default_branch: Some("main".into()),
            viewer_login: Some("cat".into()),
            authenticated: true,
            gh_version: "2.75.0".into(),
        };
        let args = github_pull_request_files_page_args(&repository, 17, 3, Some("src/review.rs"))
            .unwrap()
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "api",
                "repos/team/project/pulls/17/files",
                "--hostname=github.example",
                "--include",
                "--method=GET",
                "--jq",
                "map({filename,previous_filename,status,additions,deletions,changes,patch:(if .filename == \"src/review.rs\" then .patch else null end)})",
                "-H",
                "Accept: application/vnd.github+json",
                "-f",
                "page=3",
                "-f",
                "per_page=100",
            ]
        );
        let summary_args = github_pull_request_files_page_args(&repository, 17, 1, None).unwrap();
        assert_eq!(summary_args[6].to_string_lossy(), GITHUB_PR_FILES_JQ);
        assert!(github_pull_request_files_page_args(&repository, 17, 0, None).is_err());
        assert!(github_pull_request_files_page_args(
            &repository,
            17,
            MAX_GITHUB_PR_FILE_PAGES + 1,
            None
        )
        .is_err());
        assert_eq!(MAX_GITHUB_PR_FILES, 3_000);
    }

    #[test]
    fn parses_paginated_pull_request_files_and_rename_metadata() {
        let response = concat!(
            "HTTP/2.0 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "Link: <https://github.example/api/v3/repos/team/project/pulls/17/files?page=2>; rel=\"next\"\r\n",
            "\r\n",
            "[",
            "{",
            "\"filename\":\"src/review.rs\",",
            "\"status\":\"modified\",",
            "\"additions\":1,",
            "\"deletions\":1,",
            "\"changes\":2,",
            "\"patch\":\"@@ -1 +1 @@\\n-old\\n+new\"",
            "},",
            "{",
            "\"filename\":\"src/new.rs\",",
            "\"previous_filename\":\"src/old.rs\",",
            "\"status\":\"renamed\",",
            "\"additions\":0,",
            "\"deletions\":0,",
            "\"changes\":0,",
            "\"patch\":null",
            "}",
            "]"
        );
        let page = parse_github_pull_request_files_page(response.as_bytes()).unwrap();
        assert!(page.has_more);
        assert_eq!(page.files.len(), 2);
        assert_eq!(page.files[0].change.path, "src/review.rs");
        assert_eq!(
            page.files[0].patch.as_deref(),
            Some("@@ -1 +1 @@\n-old\n+new")
        );
        assert_eq!(page.files[1].change.status, GitFileStatus::Renamed);
        assert_eq!(
            page.files[1].change.original_path.as_deref(),
            Some("src/old.rs")
        );

        let final_page = response.replace(
            "Link: <https://github.example/api/v3/repos/team/project/pulls/17/files?page=2>; rel=\"next\"\r\n",
            "",
        );
        assert!(
            !parse_github_pull_request_files_page(final_page.as_bytes())
                .unwrap()
                .has_more
        );
    }

    #[test]
    fn single_file_rest_patches_distinguish_binary_missing_and_truncated_text() {
        let complete = parse_rest_pull_request_file(&serde_json::json!({
            "filename": "src/review.rs",
            "status": "modified",
            "additions": 1,
            "deletions": 1,
            "changes": 2,
            "patch": "@@ -1 +1 @@\n-old\n+new"
        }))
        .unwrap();
        assert!(!github_pull_request_file_is_binary(&complete));
        assert!(!github_pull_request_file_patch_truncated(&complete));
        let rendered = render_github_pull_request_file_patch(&complete);
        assert!(rendered.starts_with(
            "diff --git a/src/review.rs b/src/review.rs\n--- a/src/review.rs\n+++ b/src/review.rs\n"
        ));
        assert!(rendered.ends_with("-old\n+new\n"));

        let mut binary = parse_rest_pull_request_file(&serde_json::json!({
            "filename": "assets/image.png",
            "status": "modified",
            "additions": 0,
            "deletions": 0,
            "changes": 0
        }))
        .unwrap();
        binary.change.binary = github_pull_request_file_is_binary(&binary);
        assert!(binary.change.binary);
        assert!(!github_pull_request_file_patch_truncated(&binary));
        assert!(render_github_pull_request_file_patch(&binary)
            .contains("Binary files a/assets/image.png and b/assets/image.png differ"));

        let missing_text = parse_rest_pull_request_file(&serde_json::json!({
            "filename": "generated/large.txt",
            "status": "modified",
            "additions": 500,
            "deletions": 20,
            "changes": 520
        }))
        .unwrap();
        assert!(!github_pull_request_file_is_binary(&missing_text));
        assert!(github_pull_request_file_patch_truncated(&missing_text));

        let partial_text = parse_rest_pull_request_file(&serde_json::json!({
            "filename": "generated/partial.txt",
            "status": "modified",
            "additions": 2,
            "deletions": 1,
            "changes": 3,
            "patch": "@@ -1 +1 @@\n-old\n+new"
        }))
        .unwrap();
        assert!(github_pull_request_file_patch_truncated(&partial_text));

        let renamed = parse_rest_pull_request_file(&serde_json::json!({
            "filename": "src/new name.rs",
            "previous_filename": "src/old name.rs",
            "status": "renamed",
            "additions": 0,
            "deletions": 0,
            "changes": 0
        }))
        .unwrap();
        assert!(!github_pull_request_file_is_binary(&renamed));
        assert!(!github_pull_request_file_patch_truncated(&renamed));
        let rendered = render_github_pull_request_file_patch(&renamed);
        assert!(rendered.contains("rename from \"src/old name.rs\""));
        assert!(rendered.contains("rename to \"src/new name.rs\""));
    }

    #[test]
    fn parses_common_github_remote_forms() {
        assert_eq!(
            parse_remote_for_test("https://github.com/openai/codex.git"),
            Some(("github.com".into(), "openai".into(), "codex".into()))
        );
        assert_eq!(
            parse_remote_for_test("git@github.example.com:team/project.git"),
            Some(("github.example.com".into(), "team".into(), "project".into()))
        );
        assert!(parse_remote_for_test("file:///tmp/repo").is_none());
    }

    #[test]
    fn known_github_hosts_keep_the_fast_path_without_discovery_auth() {
        let remote = parse_remote_url("https://github.com/openai/codex.git").unwrap();
        let qualified = qualify_github_remote(remote, true, |_| {
            panic!("known GitHub hosts must not authenticate during remote discovery")
        })
        .unwrap();
        assert_eq!(qualified.remote.host, "github.com");
        assert!(qualified.verified_auth.is_none());
    }

    #[test]
    fn arbitrary_enterprise_hosts_require_active_success_auth() {
        let remote = parse_remote_url("ssh://git@code.corp.example/platform/mework.git").unwrap();
        let qualified = qualify_github_remote(remote, false, |host| {
            assert_eq!(host, "code.corp.example");
            Ok((true, Some("enterprise-user".into()), None))
        })
        .unwrap();
        assert_eq!(qualified.remote.host, "code.corp.example");
        assert_eq!(
            qualified.verified_auth,
            Some((true, Some("enterprise-user".into()), None))
        );

        let inactive_gitlab = parse_remote_url("https://gitlab.example/team/project.git").unwrap();
        assert!(qualify_github_remote(inactive_gitlab, false, |host| {
            assert_eq!(host, "gitlab.example");
            Ok((
                false,
                None,
                Some("gitlab.example 没有活动的 gh 账号".into()),
            ))
        })
        .is_none());

        let unavailable_host = parse_remote_url("https://source.example/team/project.git").unwrap();
        assert!(qualify_github_remote(unavailable_host, false, |_| {
            Err("gh did not authenticate this host".into())
        })
        .is_none());
    }

    #[test]
    fn enterprise_discovery_auth_command_is_host_scoped_and_never_requests_tokens() {
        let args = github_auth_status_args("code.corp.example")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "auth",
                "status",
                "--active",
                "--hostname=code.corp.example",
                "--json",
                "hosts",
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--show-token"));

        assert!(parse_remote_url("git@--show-token:owner/repo.git").is_none());
        let malicious_args = github_auth_status_args("--show-token")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(malicious_args
            .iter()
            .any(|arg| arg == "--hostname=--show-token"));
        assert!(!malicious_args.iter().any(|arg| arg == "--show-token"));

        let viewer_args = github_canonical_viewer_login_args("code.corp.example")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            viewer_args,
            [
                "api",
                "--hostname=code.corp.example",
                "user",
                "--jq",
                ".login",
            ]
        );
        assert!(!viewer_args.iter().any(|arg| arg == "--show-token"));
    }

    #[test]
    fn auth_json_requires_active_success_state() {
        let success = serde_json::json!({
            "hosts": {
                "code.corp.example": [
                    {"active": true, "login": "octocat", "state": "success"}
                ]
            }
        });
        assert_eq!(
            parse_auth_status(&success, "CODE.CORP.EXAMPLE").unwrap(),
            (true, Some("octocat".into()), None)
        );
        let inactive = serde_json::json!({
            "hosts": {
                "code.corp.example": [
                    {"active": false, "login": "octocat", "state": "success"}
                ]
            }
        });
        assert!(!parse_auth_status(&inactive, "code.corp.example").unwrap().0);
        let invalid = serde_json::json!({
            "hosts": {
                "code.corp.example": [
                    {"active": true, "login": "octocat", "state": "failure"}
                ]
            }
        });
        assert!(!parse_auth_status(&invalid, "code.corp.example").unwrap().0);
        let missing_login = serde_json::json!({
            "hosts": {
                "code.corp.example": [
                    {"active": true, "state": "success"}
                ]
            }
        });
        assert!(parse_auth_status(&missing_login, "code.corp.example").is_err());
    }

    #[test]
    fn canonical_viewer_login_accepts_current_login_and_resolves_renamed_alias() {
        let unchanged =
            canonicalize_github_auth_status((true, Some("example-user".into()), None), || {
                Ok("example-user".into())
            })
            .unwrap();
        assert_eq!(unchanged, (true, Some("example-user".into()), None));

        let renamed =
            canonicalize_github_auth_status((true, Some("example-org".into()), None), || {
                Ok("example-user".into())
            })
            .unwrap();
        assert_eq!(renamed, (true, Some("example-user".into()), None));

        let actual = github_repository_fixture(renamed.1.as_deref());
        let expected = GithubRepositoryIdentity {
            host: actual.host.clone(),
            owner: actual.owner.clone(),
            name: actual.name.clone(),
        };
        validate_github_write_identity(&expected, "example-user", &actual).unwrap();
        assert!(
            validate_github_write_identity(&expected, "example-org", &actual)
                .unwrap_err()
                .contains("登录账号已在确认后发生变化")
        );
    }

    #[test]
    fn canonical_viewer_login_fails_closed_on_api_malformed_or_truncated_output() {
        let api_failure =
            canonicalize_github_auth_status((true, Some("example-org".into()), None), || {
                Err("读取 GitHub canonical viewer 失败".into())
            })
            .unwrap_err();
        assert!(api_failure.contains("canonical viewer"));

        for malformed in [
            b"".as_slice(),
            b"example-user\nunexpected\n".as_slice(),
            b" example-user\n".as_slice(),
            b"example user\n".as_slice(),
            b"example-user\0\n".as_slice(),
            &[0xff, 0xfe],
        ] {
            assert!(parse_github_canonical_viewer_login(malformed).is_err());
        }
        assert_eq!(
            parse_github_canonical_viewer_login(b"example-user\n").unwrap(),
            "example-user"
        );
        assert_eq!(
            parse_github_canonical_viewer_login(b"example-user\r\n").unwrap(),
            "example-user"
        );
        assert!(require_complete_github_canonical_viewer_output(true, false).is_err());
        assert!(require_complete_github_canonical_viewer_output(false, true).is_err());
        require_complete_github_canonical_viewer_output(false, false).unwrap();

        let inactive =
            canonicalize_github_auth_status((false, None, Some("inactive".into())), || {
                panic!("canonical API must not run for an inactive account")
            })
            .unwrap();
        assert_eq!(inactive, (false, None, Some("inactive".into())));
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
    fn github_local_writes_require_confirmed_clean_idle_state() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let discovered = require_repository(repository.path()).unwrap();
        let clean = snapshot_for_repository(&discovered).unwrap();
        validate_github_local_state(
            &discovered,
            clean.head.as_deref().unwrap(),
            &clean.content_revision,
        )
        .unwrap();

        fs::write(repository.path().join("tracked.txt"), "dirty\n").unwrap();
        let dirty = snapshot_for_repository(&discovered).unwrap();
        let dirty_error = validate_github_local_state(
            &discovered,
            dirty.head.as_deref().unwrap(),
            &dirty.content_revision,
        )
        .unwrap_err();
        assert!(dirty_error.contains("未提交变更"));

        let (conflicted_repository, _) = repository_with_merge_conflict();
        execute_action(
            conflicted_repository.path(),
            GitAction::Merge {
                branch: "feature/conflict".into(),
                expected_head: test_commit_oid(conflicted_repository.path(), "HEAD"),
                expected_branch_oid: test_commit_oid(
                    conflicted_repository.path(),
                    "feature/conflict",
                ),
            },
        )
        .unwrap_err();
        let conflicted = require_repository(conflicted_repository.path()).unwrap();
        let conflicted_snapshot = snapshot_for_repository(&conflicted).unwrap();
        let operation_error = validate_github_local_state(
            &conflicted,
            conflicted_snapshot.head.as_deref().unwrap(),
            &conflicted_snapshot.content_revision,
        )
        .unwrap_err();
        assert!(operation_error.contains("正在执行 Git 操作"));
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
        assert!(!same_path(&primary.index_path, &discovered.index_path));
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

        let committed = execute_action(
            repository.path(),
            prepared_commit_action(repository.path(), "add new file"),
        )
        .unwrap();
        assert!(committed.snapshot.is_some());

        let branches = branches(repository.path()).unwrap();
        assert!(branches.branches.iter().any(|branch| branch.current));
        let history = history(
            repository.path(),
            GitHistoryRequest {
                limit: 10,
                ..GitHistoryRequest::default()
            },
        )
        .unwrap();
        assert_eq!(history.commits[0].subject, "add new file");
        assert!(history.commits.len() >= 2);
    }

    #[test]
    fn commit_preparation_binds_message_tree_and_staged_index() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("proof.txt"), "confirmed\n").unwrap();
        run_test_git(repository.path(), &["add", "proof.txt"]);
        let head_before = String::from_utf8(run_test_git_output(
            repository.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        let first = prepare_commit(repository.path(), "confirmed message").unwrap();
        let repeated = prepare_commit(repository.path(), "confirmed message").unwrap();
        assert_eq!(first.target_revision, repeated.target_revision);
        assert_eq!(first.candidate_tree_oid, repeated.candidate_tree_oid);
        assert_eq!(first.message_digest, repeated.message_digest);

        let changed_message = prepare_commit(repository.path(), "different message").unwrap();
        assert_eq!(first.candidate_tree_oid, changed_message.candidate_tree_oid);
        assert_ne!(first.message_digest, changed_message.message_digest);
        assert_ne!(first.target_revision, changed_message.target_revision);

        run_test_git(
            repository.path(),
            &["config", "user.name", "Changed Identity"],
        );
        let changed_identity = prepare_commit(repository.path(), "confirmed message").unwrap();
        assert_ne!(first.target_revision, changed_identity.target_revision);
        run_test_git(repository.path(), &["config", "user.name", "Mework Test"]);
        run_test_git(repository.path(), &["config", "commit.gpgSign", "true"]);
        let changed_signing = prepare_commit(repository.path(), "confirmed message").unwrap();
        assert_ne!(first.target_revision, changed_signing.target_revision);
        run_test_git(repository.path(), &["config", "commit.gpgSign", "false"]);

        fs::write(
            repository.path().join("proof.txt"),
            "changed after confirmation\n",
        )
        .unwrap();
        run_test_git(repository.path(), &["add", "proof.txt"]);
        let error = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "confirmed message".into(),
                expected_target_revision: first.target_revision,
                expected_tree_oid: first.candidate_tree_oid,
                amend: false,
            },
        )
        .unwrap_err();
        assert!(error.contains("已在确认后变化"));
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", "HEAD"]
            ))
            .unwrap()
            .trim(),
            head_before
        );
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());
    }

    #[test]
    fn proof_bound_commit_supports_an_unborn_branch() {
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
        fs::write(repository.path().join("root.txt"), "root\n").unwrap();
        run_test_git(repository.path(), &["add", "root.txt"]);
        let preparation = prepare_commit(repository.path(), "root commit").unwrap();
        assert!(preparation.snapshot.unborn);
        install_test_git_hook(
            repository.path(),
            "post-commit",
            "printf 'post hook failed\\n' >&2\nexit 7",
        );
        let result = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "root commit".into(),
                expected_target_revision: preparation.target_revision,
                expected_tree_oid: preparation.candidate_tree_oid.clone(),
                amend: false,
            },
        )
        .unwrap();
        assert!(result
            .message
            .as_deref()
            .is_some_and(|message| message.contains("post-commit hook 失败")));
        let committed_oid = result.committed_oid.unwrap();
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["show", "-s", "--format=%T", &committed_oid],
            ))
            .unwrap()
            .trim(),
            preparation.candidate_tree_oid
        );
        assert!(String::from_utf8(run_test_git_output(
            repository.path(),
            &["show", "-s", "--format=%P", &committed_oid],
        ))
        .unwrap()
        .trim()
        .is_empty());
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());
    }

    #[test]
    fn proof_bound_commit_updates_detached_head_without_moving_a_branch() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let branch_ref = String::from_utf8(run_test_git_output(
            repository.path(),
            &["symbolic-ref", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        let branch_oid = String::from_utf8(run_test_git_output(
            repository.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(repository.path(), &["switch", "--detach"]);
        fs::write(repository.path().join("detached.txt"), "detached\n").unwrap();
        run_test_git(repository.path(), &["add", "detached.txt"]);
        let preparation = prepare_commit(repository.path(), "detached commit").unwrap();
        assert!(preparation.snapshot.detached);
        let result = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "detached commit".into(),
                expected_target_revision: preparation.target_revision,
                expected_tree_oid: preparation.candidate_tree_oid,
                amend: false,
            },
        )
        .unwrap();
        let committed_oid = result.committed_oid.unwrap();
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", "HEAD"]
            ))
            .unwrap()
            .trim(),
            committed_oid
        );
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", &branch_ref],
            ))
            .unwrap()
            .trim(),
            branch_oid
        );
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());
    }

    #[test]
    fn commit_hooks_cannot_silently_change_the_confirmed_tree_or_message() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("intended.txt"), "intended\n").unwrap();
        run_test_git(repository.path(), &["add", "intended.txt"]);
        let head_before = String::from_utf8(run_test_git_output(
            repository.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        let preparation = prepare_commit(repository.path(), "confirmed hook message").unwrap();
        install_test_git_hook(
            repository.path(),
            "pre-commit",
            "printf 'hook\\n' > hook-added.txt\ngit add -- hook-added.txt",
        );
        let error = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "confirmed hook message".into(),
                expected_target_revision: preparation.target_revision,
                expected_tree_oid: preparation.candidate_tree_oid,
                amend: false,
            },
        )
        .unwrap_err();
        assert!(error.contains("pre-commit hook 改变"));
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", "HEAD"]
            ))
            .unwrap()
            .trim(),
            head_before
        );
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert!(snapshot
            .files
            .iter()
            .any(|change| change.path == "hook-added.txt" && change.untracked));
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());

        install_test_git_hook(repository.path(), "pre-commit", ":");
        install_test_git_hook(
            repository.path(),
            "commit-msg",
            "printf '\\nchanged by hook\\n' >> \"$1\"",
        );
        let preparation = prepare_commit(repository.path(), "confirmed hook message").unwrap();
        let error = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "confirmed hook message".into(),
                expected_target_revision: preparation.target_revision,
                expected_tree_oid: preparation.candidate_tree_oid,
                amend: false,
            },
        )
        .unwrap_err();
        assert!(error.contains("提交信息 hook 改变"));
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", "HEAD"]
            ))
            .unwrap()
            .trim(),
            head_before
        );
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());
    }

    #[test]
    fn commit_ref_cas_rejects_a_head_move_after_object_creation() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let branch_ref = String::from_utf8(run_test_git_output(
            repository.path(),
            &["symbolic-ref", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        let original_head = String::from_utf8(run_test_git_output(
            repository.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(repository.path(), &["switch", "-c", "competing"]);
        fs::write(repository.path().join("competing.txt"), "competing\n").unwrap();
        run_test_git(repository.path(), &["add", "competing.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "competing"]);
        let competing_head = String::from_utf8(run_test_git_output(
            repository.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(repository.path(), &["switch", "-"]);
        fs::write(repository.path().join("intended.txt"), "intended\n").unwrap();
        run_test_git(repository.path(), &["add", "intended.txt"]);
        let preparation = prepare_commit(repository.path(), "intended commit").unwrap();
        let branch_ref_for_hook = branch_ref.clone();
        let competing_for_hook = competing_head.clone();
        let original_for_hook = original_head.clone();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::CommitBeforeRefUpdate,
            move |root| {
                run_test_git(
                    root,
                    &[
                        "update-ref",
                        &branch_ref_for_hook,
                        &competing_for_hook,
                        &original_for_hook,
                    ],
                )
            },
        );
        let error = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "intended commit".into(),
                expected_target_revision: preparation.target_revision,
                expected_tree_oid: preparation.candidate_tree_oid,
                amend: false,
            },
        )
        .unwrap_err();
        assert!(error.contains("提交对象创建后发生变化"));
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["rev-parse", &branch_ref],
            ))
            .unwrap()
            .trim(),
            competing_head
        );
        assert_stage_all_locks_absent(&require_repository(repository.path()).unwrap());
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

    #[test]
    fn failed_git_cli_action_is_returned_as_an_error() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let error = prepare_commit(repository.path(), "nothing to commit").unwrap_err();
        assert!(error.contains("没有可提交"));
    }

    #[test]
    fn bulk_stage_and_unstage_are_revision_bound_without_path_lists() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();
        fs::write(repository.path().join("untracked-a.txt"), "a\n").unwrap();
        fs::write(repository.path().join("untracked-b.txt"), "b\n").unwrap();
        let before = workspace_snapshot(repository.path()).unwrap().unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert_eq!(
            preparation.snapshot.content_revision,
            before.content_revision
        );
        assert!(!preparation.snapshot.files_complete);
        assert!(preparation.snapshot.files.is_empty());

        let staged = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: before.content_revision.clone(),
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap()
        .snapshot
        .unwrap();
        assert_eq!(staged.unstaged, 0);
        assert_eq!(staged.untracked, 0);
        assert_eq!(staged.staged, 3);
        assert_eq!(staged.changed_files, 3);
        assert!(!staged.files_complete);
        assert!(staged.files.is_empty());

        let stale = execute_action(
            repository.path(),
            GitAction::UnstageAll {
                expected_content_revision: before.content_revision,
            },
        )
        .unwrap_err();
        assert!(stale.contains("已在操作前发生变化"));

        let unstaged = execute_action(
            repository.path(),
            GitAction::UnstageAll {
                expected_content_revision: staged.content_revision,
            },
        )
        .unwrap()
        .snapshot
        .unwrap();
        assert_eq!(unstaged.staged, 0);
        assert_eq!(unstaged.unstaged, 3);
        assert_eq!(unstaged.untracked, 2);
        assert!(!unstaged.files_complete);
        assert!(unstaged.files.is_empty());
    }

    #[test]
    fn stage_all_target_revision_rejects_same_size_untracked_rewrite_with_restored_mtime() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        let index_before = run_test_git_output(repository.path(), &["ls-files", "--stage"]);

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
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "omega\n");
        assert_eq!(
            run_test_git_output(repository.path(), &["ls-files", "--stage"]),
            index_before
        );
        let after = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(after.untracked, 1);
        assert_eq!(after.staged, 0);
    }

    #[test]
    fn stage_all_tracked_only_batch_does_not_require_a_path_array() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        for index in 0..256 {
            fs::write(
                repository.path().join(format!("tracked-{index:03}.txt")),
                "first\n",
            )
            .unwrap();
        }
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "add tracked batch"]);
        for index in 0..256 {
            fs::write(
                repository.path().join(format!("tracked-{index:03}.txt")),
                "second\n",
            )
            .unwrap();
        }

        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert_eq!(preparation.snapshot.untracked, 0);
        assert_eq!(preparation.snapshot.stageable, 256);
        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();

        let after = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(after.staged, 256);
        assert_eq!(after.unstaged, 0);
        assert_eq!(after.untracked, 0);
    }

    #[test]
    fn stage_all_candidate_transaction_supports_an_unborn_repository() {
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
        fs::write(repository.path().join("first.txt"), "first\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        assert!(!resolved.index_path.exists());

        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert!(preparation.snapshot.unborn);
        assert!(matches!(preparation.candidate_tree_oid.len(), 40 | 64));
        assert!(!resolved.index_path.exists());
        assert_stage_all_locks_absent(&resolved);

        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert!(resolved.index_path.is_file());
        assert_eq!(
            String::from_utf8(run_test_git_output(repository.path(), &["ls-files"]))
                .unwrap()
                .trim(),
            "first.txt"
        );
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_expands_a_split_index_candidate_without_changing_the_real_baseline_early() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        run_test_git(repository.path(), &["update-index", "--split-index"]);
        let resolved = require_repository(repository.path()).unwrap();
        let split_index_before = fs::read(&resolved.index_path).unwrap();
        assert!(!String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["rev-parse", "--shared-index-path"],
        ))
        .trim()
        .is_empty());
        fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();
        fs::write(repository.path().join("new.txt"), "new\n").unwrap();

        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert_eq!(fs::read(&resolved.index_path).unwrap(), split_index_before);
        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run_test_git_output(
                repository.path(),
                &["diff", "--cached", "--name-only"],
            ))
            .lines()
            .collect::<HashSet<_>>(),
            HashSet::from(["new.txt", "tracked.txt"])
        );
        assert!(String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["rev-parse", "--shared-index-path"],
        ))
        .trim()
        .is_empty());
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_candidate_supports_an_in_cone_sparse_index_without_losing_hidden_entries() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::create_dir_all(repository.path().join("visible")).unwrap();
        fs::create_dir_all(repository.path().join("hidden")).unwrap();
        fs::write(repository.path().join("visible/keep.txt"), "visible\n").unwrap();
        fs::write(repository.path().join("hidden/keep.txt"), "hidden\n").unwrap();
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "sparse folders"]);
        run_test_git(
            repository.path(),
            &["sparse-checkout", "init", "--cone", "--sparse-index"],
        );
        run_test_git(repository.path(), &["sparse-checkout", "set", "visible"]);
        assert!(String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["ls-files", "--sparse"],
        ))
        .lines()
        .any(|line| line == "hidden/"));
        fs::write(
            repository.path().join("visible/keep.txt"),
            "visible changed\n",
        )
        .unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();

        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run_test_git_output(repository.path(), &["write-tree"],))
                .trim(),
            preparation.candidate_tree_oid
        );
        assert!(String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["ls-tree", "-r", "HEAD", "--", "hidden/keep.txt"],
        ))
        .contains("hidden/keep.txt"));
        assert!(
            String::from_utf8_lossy(&run_test_git_output(
                repository.path(),
                &["ls-files", "--sparse"],
            ))
            .lines()
            .any(|line| line == "hidden/"),
            "publishing must preserve the sparse-directory entry"
        );
        let after = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(after.staged, 1);
        assert_eq!(after.unstaged, 0);
        assert_eq!(after.untracked, 0);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_fails_closed_for_an_untracked_path_outside_the_sparse_cone() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::create_dir_all(repository.path().join("visible")).unwrap();
        fs::create_dir_all(repository.path().join("hidden")).unwrap();
        fs::write(repository.path().join("visible/keep.txt"), "visible\n").unwrap();
        fs::write(repository.path().join("hidden/keep.txt"), "hidden\n").unwrap();
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "sparse folders"]);
        run_test_git(
            repository.path(),
            &["sparse-checkout", "init", "--cone", "--sparse-index"],
        );
        run_test_git(repository.path(), &["sparse-checkout", "set", "visible"]);
        fs::create_dir_all(repository.path().join("hidden")).unwrap();
        fs::write(repository.path().join("hidden/new.txt"), "outside cone\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();

        let error = prepare_stage_all(repository.path()).unwrap_err();
        assert!(error.contains("候选"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_candidate_preserves_skip_worktree_and_assume_unchanged_flags() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("skip.txt"), "skip\n").unwrap();
        fs::write(repository.path().join("assume.txt"), "assume\n").unwrap();
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "index flags"]);
        run_test_git(
            repository.path(),
            &["update-index", "--skip-worktree", "skip.txt"],
        );
        run_test_git(
            repository.path(),
            &["update-index", "--assume-unchanged", "assume.txt"],
        );
        fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();

        let preparation = prepare_stage_all(repository.path()).unwrap();
        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["ls-files", "-t", "--", "skip.txt"],
        ))
        .starts_with("S "));
        assert!(String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["ls-files", "-v", "--", "assume.txt"],
        ))
        .starts_with("h "));
    }

    #[test]
    fn stage_all_candidate_binds_the_clean_filter_result() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(
            repository.path().join(".gitattributes"),
            "filtered.txt filter=mework-proof\n",
        )
        .unwrap();
        fs::write(repository.path().join("filtered.txt"), "initial\n").unwrap();
        run_test_git(
            repository.path(),
            &[
                "config",
                "filter.mework-proof.clean",
                "git hash-object --stdin",
            ],
        );
        run_test_git(
            repository.path(),
            &["config", "filter.mework-proof.required", "true"],
        );
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "filtered file"]);
        fs::write(repository.path().join("filtered.txt"), "changed\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteAfterValidation,
            move |root| {
                run_test_git(
                    root,
                    &[
                        "config",
                        "filter.mework-proof.clean",
                        "git hash-object --stdin | git hash-object --stdin",
                    ],
                );
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(
            error.contains("待暂存内容已在确认后发生变化")
                || error.contains("工作区在候选发布前发生变化"),
            "{error}"
        );
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_resolves_an_unmerged_candidate_without_mutating_the_real_index_early() {
        if !git_available() {
            return;
        }
        let (repository, _) = repository_with_merge_conflict();
        execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "feature/conflict".into(),
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_branch_oid: test_commit_oid(repository.path(), "feature/conflict"),
            },
        )
        .unwrap_err();
        fs::write(repository.path().join("tracked.txt"), "resolved\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let unmerged_before = run_test_git_output(repository.path(), &["ls-files", "--unmerged"]);
        assert!(!unmerged_before.is_empty());

        let preparation = prepare_stage_all(repository.path()).unwrap();
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_eq!(
            run_test_git_output(repository.path(), &["ls-files", "--unmerged"]),
            unmerged_before
        );
        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert!(run_test_git_output(repository.path(), &["ls-files", "--unmerged"]).is_empty());
        assert_eq!(
            String::from_utf8_lossy(&run_test_git_output(repository.path(), &["write-tree"],))
                .trim(),
            preparation.candidate_tree_oid
        );
        let after = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(after.operation, Some(GitRepositoryOperation::Merge));
        assert_eq!(after.conflicted, 0);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_candidate_binds_and_publishes_a_submodule_gitlink() {
        if !git_available() {
            return;
        }
        let child = initialized_repository();
        let parent = initialized_repository();
        let child_path = child.path().to_string_lossy().into_owned();
        run_test_git(
            parent.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &child_path,
                "module",
            ],
        );
        run_test_git(parent.path(), &["add", "--all"]);
        run_test_git(parent.path(), &["commit", "-m", "add submodule"]);
        let module = parent.path().join("module");
        run_test_git(&module, &["config", "user.name", "Mework Test"]);
        run_test_git(&module, &["config", "user.email", "mework@example.invalid"]);
        fs::write(module.join("tracked.txt"), "gitlink-b\n").unwrap();
        run_test_git(&module, &["add", "tracked.txt"]);
        run_test_git(&module, &["commit", "-m", "gitlink B"]);
        let gitlink_b = test_commit_oid(&module, "HEAD");
        fs::write(module.join("tracked.txt"), "gitlink-c\n").unwrap();
        run_test_git(&module, &["add", "tracked.txt"]);
        run_test_git(&module, &["commit", "-m", "gitlink C"]);
        let gitlink_c = test_commit_oid(&module, "HEAD");
        run_test_git(&module, &["checkout", &gitlink_b]);
        let resolved = require_repository(parent.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();

        let preparation = prepare_stage_all(parent.path()).unwrap();
        let candidate_entry = String::from_utf8_lossy(&run_test_git_output(
            parent.path(),
            &["ls-tree", &preparation.candidate_tree_oid, "--", "module"],
        ))
        .into_owned();
        assert!(candidate_entry.starts_with("160000 commit "));
        assert!(candidate_entry.contains(&gitlink_b));
        let checkout_c = gitlink_c.clone();
        install_stage_all_test_hook(
            parent.path(),
            StageAllTestHookPoint::ExecuteAfterValidation,
            move |root| run_test_git(&root.join("module"), &["checkout", &checkout_c]),
        );
        let error = execute_action(
            parent.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);

        let preparation = prepare_stage_all(parent.path()).unwrap();
        execute_action(
            parent.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        let staged = String::from_utf8_lossy(&run_test_git_output(
            parent.path(),
            &["ls-files", "--stage", "--", "module"],
        ))
        .into_owned();
        assert!(staged.starts_with("160000 "));
        assert!(staged.contains(&gitlink_c));
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_never_removes_a_preexisting_external_index_lock() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("untracked.txt"), "value\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let lock_path = resolved.index_path.with_file_name("index.lock");
        fs::write(&lock_path, b"external lock").unwrap();

        let error = prepare_stage_all(repository.path()).unwrap_err();
        assert!(error.contains("其他进程锁定"));
        assert_eq!(fs::read(&lock_path).unwrap(), b"external lock");
        fs::remove_file(lock_path).unwrap();
    }

    #[test]
    fn stage_all_rejects_a_same_size_worktree_race_after_validation_without_touching_index() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteAfterValidation,
            move |root| {
                let path = root.join("untracked.txt");
                fs::write(&path, "omega\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("候选 index 未发布"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_rechecks_the_worktree_before_publish_and_leaves_index_unchanged() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteBeforePublishRecheck,
            move |root| {
                let path = root.join("untracked.txt");
                fs::write(&path, "omega\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("候选 index 未发布"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_real_index_cas_preserves_an_external_concurrent_index() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let resolved = require_repository(repository.path()).unwrap();
        let alternate_index = repository.path().join("external.index");
        fs::copy(&resolved.index_path, &alternate_index).unwrap();
        fs::write(repository.path().join("external.txt"), "external\n").unwrap();
        run_test_git_with_index(
            repository.path(),
            &alternate_index,
            &["add", "external.txt"],
        );
        fs::remove_file(repository.path().join("external.txt")).unwrap();
        let external_index_bytes = fs::read(&alternate_index).unwrap();
        fs::write(repository.path().join("wanted.txt"), "wanted\n").unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        let index_path = resolved.index_path.clone();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteBeforeIndexPublish,
            move |_| {
                fs::write(&index_path, &external_index_bytes).unwrap();
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("外部进程并发修改"));
        assert_eq!(
            fs::read(&resolved.index_path).unwrap(),
            fs::read(alternate_index).unwrap()
        );
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_publishes_the_verified_candidate_and_leaves_a_late_worktree_rewrite_unstaged() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        run_test_git(repository.path(), &["config", "core.trustctime", "false"]);
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteBeforeIndexPublish,
            move |root| {
                let path = root.join("untracked.txt");
                fs::write(&path, "omega\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            },
        );

        execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();

        assert_eq!(
            run_test_git_output(repository.path(), &["show", ":untracked.txt"]),
            b"alpha\n"
        );
        assert_eq!(fs::read(&path).unwrap(), b"omega\n");
        assert_ne!(
            run_test_git_output(repository.path(), &["rev-parse", ":untracked.txt"],),
            run_test_git_output(repository.path(), &["hash-object", "--", "untracked.txt"],)
        );
    }

    #[test]
    fn stage_all_candidate_byte_cas_rejects_a_tampered_candidate_before_publish() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("wanted.txt"), "wanted\n").unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let candidate_path = resolved.index_path.with_file_name("index.lock");
        let preparation = prepare_stage_all(repository.path()).unwrap();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteBeforeIndexPublish,
            move |_| {
                let mut bytes = fs::read(&candidate_path).unwrap();
                let last = bytes.last_mut().expect("candidate index is not empty");
                *last ^= 0x01;
                fs::write(&candidate_path, bytes).unwrap();
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("候选在验证后被外部修改"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[cfg(unix)]
    #[test]
    fn stage_all_candidate_tree_rejects_an_untracked_executable_mode_change() {
        use std::os::unix::fs::PermissionsExt;

        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let current = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(
            current.content_revision,
            preparation.snapshot.content_revision
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
    }

    #[cfg(unix)]
    #[test]
    fn stage_all_candidate_tree_binds_an_untracked_symlink_target() {
        use std::os::unix::fs::symlink;

        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let link = repository.path().join("link");
        symlink("target-a", &link).unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        let index_before = fs::read(&resolved.index_path).unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        let candidate_entry = String::from_utf8_lossy(&run_test_git_output(
            repository.path(),
            &["ls-tree", &preparation.candidate_tree_oid, "--", "link"],
        ))
        .into_owned();
        assert!(candidate_entry.starts_with("120000 blob "));
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ExecuteAfterValidation,
            move |root| {
                fs::remove_file(root.join("link")).unwrap();
                symlink("target-b", root.join("link")).unwrap();
            },
        );

        let error = execute_action(
            repository.path(),
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
        assert_eq!(fs::read(&resolved.index_path).unwrap(), index_before);
        assert_stage_all_locks_absent(&resolved);
    }

    #[test]
    fn stage_all_proof_bound_diff_rejects_old_or_mid_read_untracked_content() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        let response = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("untracked.txt".into()),
                expected_stage_all_target_revision: Some(preparation.target_revision.clone()),
            },
        )
        .unwrap();
        assert!(response.patch.contains("+alpha"));
        assert_eq!(
            response.stage_all_target_revision.as_deref(),
            Some(preparation.target_revision.as_str())
        );
        assert_eq!(
            response.candidate_tree_oid.as_deref(),
            Some(preparation.candidate_tree_oid.as_str())
        );

        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::DiffBeforeFinalProof,
            move |root| {
                let path = root.join("untracked.txt");
                fs::write(&path, "omega\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            },
        );
        let error = diff(
            repository.path(),
            GitDiffRequest::Unstaged {
                path: Some("untracked.txt".into()),
                expected_stage_all_target_revision: Some(preparation.target_revision.clone()),
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
        let current = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(
            current.content_revision,
            preparation.snapshot.content_revision
        );
        let error = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("untracked.txt".into()),
                expected_stage_all_target_revision: Some(preparation.target_revision),
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
    }

    #[test]
    fn stage_all_proof_bound_views_pin_the_prepared_head_during_a_ref_aba() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let base_branch = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        let head_a = test_commit_oid(repository.path(), "HEAD");
        run_test_git(repository.path(), &["checkout", "-b", "head-b"]);
        fs::write(repository.path().join("tracked.txt"), "head-b\n").unwrap();
        fs::write(repository.path().join("branch-only.txt"), "branch-only\n").unwrap();
        run_test_git(repository.path(), &["add", "--all"]);
        run_test_git(repository.path(), &["commit", "-m", "alternate head"]);
        let head_b = test_commit_oid(repository.path(), "HEAD");
        run_test_git(repository.path(), &["checkout", &base_branch]);
        fs::write(repository.path().join("tracked.txt"), "candidate\n").unwrap();
        fs::write(repository.path().join("untracked.txt"), "untracked\n").unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();

        let switch_to_b = head_b.clone();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::DiffBeforeCandidateRead,
            move |root| run_test_git(root, &["update-ref", "HEAD", &switch_to_b]),
        );
        let restore_a = head_a.clone();
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::DiffBeforeFinalProof,
            move |root| run_test_git(root, &["update-ref", "HEAD", &restore_a]),
        );
        let response = diff(
            repository.path(),
            GitDiffRequest::Working {
                path: Some("tracked.txt".into()),
                expected_stage_all_target_revision: Some(preparation.target_revision.clone()),
            },
        )
        .unwrap();
        assert!(response.patch.contains("-first"));
        assert!(response.patch.contains("+candidate"));
        assert!(!response.patch.contains("head-b"));

        let switch_to_b = head_b;
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ChangePageBeforeCandidateRead,
            move |root| run_test_git(root, &["update-ref", "HEAD", &switch_to_b]),
        );
        install_stage_all_test_hook(
            repository.path(),
            StageAllTestHookPoint::ChangePageBeforeFinalProof,
            move |root| run_test_git(root, &["update-ref", "HEAD", &head_a]),
        );
        let page = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: preparation.snapshot.summary_revision,
                cursor: None,
                query: None,
                limit: 20,
                selected_path: None,
                expected_stage_all_target_revision: Some(preparation.target_revision),
            },
        )
        .unwrap();
        let GitChangePageResult::Page { files, .. } = page else {
            panic!("proof-bound page unexpectedly became stale");
        };
        assert!(files.iter().any(|file| file.path == "tracked.txt"));
        assert!(files.iter().any(|file| file.path == "untracked.txt"));
        assert!(!files.iter().any(|file| file.path == "branch-only.txt"));
    }

    #[test]
    fn stage_all_proof_bound_change_page_lists_and_echoes_the_candidate_tree() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();
        let path = repository.path().join("untracked.txt");
        fs::write(&path, "alpha\n").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let preparation = prepare_stage_all(repository.path()).unwrap();
        let page = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: preparation.snapshot.summary_revision.clone(),
                cursor: None,
                query: None,
                limit: 20,
                selected_path: Some("untracked.txt".into()),
                expected_stage_all_target_revision: Some(preparation.target_revision.clone()),
            },
        )
        .unwrap();
        assert!(matches!(
            &page,
            GitChangePageResult::Page {
                files,
                stage_all_target_revision: Some(ref target),
                candidate_tree_oid: Some(ref tree),
                selection: Some(GitChangeSelection::Present { .. }),
                ..
            } if target == &preparation.target_revision
                && tree == &preparation.candidate_tree_oid
                && files.iter().any(|file| file.path == "untracked.txt")
        ));
        let GitChangePageResult::Page { files, .. } = &page else {
            unreachable!();
        };
        for path in ["tracked.txt", "untracked.txt"] {
            let file = files.iter().find(|file| file.path == path).unwrap();
            assert!(file.staged, "{path} must be presented as staged");
            assert!(!file.unstaged, "{path} must not expose live unstaged state");
            assert!(
                !file.untracked,
                "{path} must not expose live untracked state"
            );
            assert!(!file.conflicted);
            assert!(file.worktree_status.is_empty());
        }
        let untracked = files
            .iter()
            .find(|file| file.path == "untracked.txt")
            .unwrap();
        assert_eq!(untracked.status, GitFileStatus::Added);
        assert_eq!(untracked.index_status, "A");

        fs::write(&path, "omega\n").unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let error = change_page(
            repository.path(),
            GitChangePageRequest {
                expected_revision: preparation.snapshot.summary_revision,
                cursor: None,
                query: None,
                limit: 20,
                selected_path: None,
                expected_stage_all_target_revision: Some(preparation.target_revision),
            },
        )
        .unwrap_err();
        assert!(error.contains("待暂存内容已在确认后发生变化"));
    }

    #[test]
    fn stage_all_uses_the_linked_worktree_specific_index() {
        if !git_available() {
            return;
        }
        let main = initialized_repository();
        let linked_parent = tempfile::tempdir().unwrap();
        let linked = linked_parent.path().join("linked");
        let linked_text = linked.to_string_lossy().into_owned();
        run_test_git(
            main.path(),
            &["worktree", "add", "-b", "linked-test", &linked_text],
        );
        let main_repository = require_repository(main.path()).unwrap();
        let main_index_before = fs::read(&main_repository.index_path).unwrap();
        let linked_repository = require_repository(&linked).unwrap();
        assert_ne!(main_repository.git_dir, linked_repository.git_dir);
        fs::write(linked.join("tracked.txt"), "linked\n").unwrap();
        fs::write(linked.join("linked-new.txt"), "new\n").unwrap();

        let preparation = prepare_stage_all(&linked).unwrap();
        execute_action(
            &linked,
            GitAction::StageAll {
                expected_content_revision: preparation.snapshot.content_revision,
                expected_target_revision: preparation.target_revision,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read(&main_repository.index_path).unwrap(),
            main_index_before
        );
        assert!(
            run_test_git_output(&linked, &["diff", "--cached", "--name-only"])
                .windows("linked-new.txt".len())
                .any(|window| window == b"linked-new.txt")
        );
        assert_stage_all_locks_absent(&linked_repository);
        run_test_git(
            main.path(),
            &["worktree", "remove", "--force", &linked_text],
        );
    }

    #[test]
    fn branch_merge_stash_and_safe_delete_actions_work_together() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let base = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        execute_action(
            repository.path(),
            GitAction::CreateBranch {
                name: "feature/test".into(),
                start_point: None,
                checkout: true,
            },
        )
        .unwrap();
        fs::write(repository.path().join("feature.txt"), "feature\n").unwrap();
        execute_action(
            repository.path(),
            GitAction::Stage {
                paths: vec!["feature.txt".into()],
            },
        )
        .unwrap();
        execute_action(
            repository.path(),
            prepared_commit_action(repository.path(), "feature commit"),
        )
        .unwrap();
        let comparison = diff(
            repository.path(),
            GitDiffRequest::Compare {
                base: base.clone(),
                head: "feature/test".into(),
                path: None,
            },
        )
        .unwrap();
        assert!(comparison
            .files
            .iter()
            .any(|file| file.path == "feature.txt"));
        execute_action(
            repository.path(),
            GitAction::Checkout {
                branch: base.clone(),
            },
        )
        .unwrap();
        execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "feature/test".into(),
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_branch_oid: test_commit_oid(repository.path(), "feature/test"),
            },
        )
        .unwrap();
        execute_action(
            repository.path(),
            GitAction::CreateBranch {
                name: "delete-me".into(),
                start_point: None,
                checkout: false,
            },
        )
        .unwrap();
        execute_action(
            repository.path(),
            GitAction::DeleteBranch {
                name: "delete-me".into(),
                force: false,
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_oid: test_commit_oid(repository.path(), "delete-me"),
            },
        )
        .unwrap();

        fs::write(repository.path().join("tracked.txt"), "stash me\n").unwrap();
        let stashed = execute_action(
            repository.path(),
            GitAction::Stash {
                message: Some("test stash".into()),
                include_untracked: false,
            },
        )
        .unwrap();
        assert_eq!(stashed.snapshot.unwrap().stash, 1);
        let popped =
            execute_action(repository.path(), GitAction::StashPop { index: Some(0) }).unwrap();
        assert_eq!(popped.snapshot.unwrap().unstaged, 1);
    }

    #[test]
    fn merge_and_delete_reject_a_branch_that_moved_after_confirmation() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let base = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        run_test_git(repository.path(), &["branch", "moving"]);
        let confirmed_branch_oid = test_commit_oid(repository.path(), "moving");
        run_test_git(repository.path(), &["switch", "moving"]);
        fs::write(repository.path().join("moving.txt"), "advanced\n").unwrap();
        run_test_git(repository.path(), &["add", "moving.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "advance moving"]);
        run_test_git(repository.path(), &["switch", &base]);
        let confirmed_head = test_commit_oid(repository.path(), "HEAD");

        let merge_error = execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "moving".into(),
                expected_head: confirmed_head,
                expected_branch_oid: confirmed_branch_oid.clone(),
            },
        )
        .unwrap_err();
        assert!(merge_error.contains("待合并分支 已在确认后发生变化"));

        let delete_error = execute_action(
            repository.path(),
            GitAction::DeleteBranch {
                name: "moving".into(),
                force: false,
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_oid: confirmed_branch_oid,
            },
        )
        .unwrap_err();
        assert!(delete_error.contains("Git 分支 已在确认后发生变化"));
        assert_eq!(
            test_commit_oid(repository.path(), "moving"),
            test_commit_oid(repository.path(), "refs/heads/moving")
        );
    }

    #[test]
    fn push_uses_the_confirmed_oid_sets_upstream_and_rejects_stale_head() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let remote = tempfile::tempdir().unwrap();
        run_test_git(remote.path(), &["init", "--bare"]);
        let remote_path = remote.path().to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["remote", "add", "origin", &remote_path],
        );
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let branch = snapshot.branch.clone().unwrap();
        let confirmed_head = snapshot.head.clone().unwrap();
        let repository_id = snapshot.repository_id.clone();
        let worktree_id = snapshot.worktree_id.clone();
        let remote_proof = snapshot.remote.clone().unwrap();
        install_test_git_hook(
            repository.path(),
            "pre-push",
            "printf invoked > pre-push-marker.txt",
        );
        run_test_git(repository.path(), &["switch", "-c", "same-oid-alias"]);
        let branch_drift_error = execute_action(
            repository.path(),
            GitAction::Push {
                expected_repository_id: repository_id.clone(),
                expected_worktree_id: worktree_id.clone(),
                remote: remote_proof.clone(),
                expected_local_branch: branch.clone(),
                remote_branch: branch.clone(),
                expected_head: confirmed_head.clone(),
                expected_upstream: None,
                set_upstream: false,
                force_with_lease: false,
            },
        )
        .unwrap_err();
        assert!(branch_drift_error.contains("本地 Git 分支"));
        run_test_git(repository.path(), &["switch", &branch]);

        let pushed = execute_action(
            repository.path(),
            GitAction::Push {
                expected_repository_id: repository_id.clone(),
                expected_worktree_id: worktree_id.clone(),
                remote: remote_proof.clone(),
                expected_local_branch: branch.clone(),
                remote_branch: branch.clone(),
                expected_head: confirmed_head.clone(),
                expected_upstream: None,
                set_upstream: true,
                force_with_lease: false,
            },
        )
        .unwrap();
        assert!(!repository.path().join("pre-push-marker.txt").exists());
        assert_eq!(
            pushed.snapshot.unwrap().upstream.as_deref(),
            Some(format!("origin/{branch}").as_str())
        );
        let remote_head = || {
            String::from_utf8(run_test_git_output(
                remote.path(),
                &["rev-parse", &format!("refs/heads/{branch}")],
            ))
            .unwrap()
            .trim()
            .to_owned()
        };
        assert_eq!(remote_head(), confirmed_head);
        assert_eq!(
            String::from_utf8(run_test_git_output(
                repository.path(),
                &["config", "--get", &format!("branch.{branch}.remote")],
            ))
            .unwrap()
            .trim(),
            "origin"
        );
        let already_bound = workspace_snapshot(repository.path()).unwrap().unwrap();
        let rebind_error = execute_action(
            repository.path(),
            GitAction::Push {
                expected_repository_id: already_bound.repository_id.clone(),
                expected_worktree_id: already_bound.worktree_id.clone(),
                remote: already_bound.remote.clone().unwrap(),
                expected_local_branch: branch.clone(),
                remote_branch: "different-target".into(),
                expected_head: already_bound.head.clone().unwrap(),
                expected_upstream: already_bound.upstream_target.clone(),
                set_upstream: true,
                force_with_lease: false,
            },
        )
        .unwrap_err();
        assert!(rebind_error.contains("不能通过普通 push 重新绑定"));

        run_test_git(
            repository.path(),
            &["commit", "--allow-empty", "-m", "advance local head"],
        );
        let advanced_head = test_commit_oid(repository.path(), "HEAD");
        let advanced_snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let stale_error = execute_action(
            repository.path(),
            GitAction::Push {
                expected_repository_id: repository_id.clone(),
                expected_worktree_id: worktree_id.clone(),
                remote: advanced_snapshot.remote.clone().unwrap(),
                expected_local_branch: branch.clone(),
                remote_branch: branch.clone(),
                expected_head: confirmed_head.clone(),
                expected_upstream: advanced_snapshot.upstream_target.clone(),
                set_upstream: false,
                force_with_lease: false,
            },
        )
        .unwrap_err();
        assert!(stale_error.contains("仓库 HEAD 已在确认后发生变化"));
        assert_eq!(remote_head(), confirmed_head);

        execute_action(
            repository.path(),
            GitAction::Push {
                expected_repository_id: repository_id,
                expected_worktree_id: worktree_id,
                remote: advanced_snapshot.remote.unwrap(),
                expected_local_branch: branch.clone(),
                remote_branch: branch.clone(),
                expected_head: advanced_head.clone(),
                expected_upstream: advanced_snapshot.upstream_target,
                set_upstream: false,
                force_with_lease: false,
            },
        )
        .unwrap();
        assert_eq!(remote_head(), advanced_head);
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
        run_test_git(
            repository.path(),
            &["push", "origin", "HEAD:refs/heads/remote-only"],
        );
        run_test_git(
            repository.path(),
            &[
                "config",
                "--replace-all",
                "remote.origin.fetch",
                "+refs/heads/*:refs/heads/*",
            ],
        );
        let dangerous_refspec = workspace_snapshot(repository.path()).unwrap().unwrap();
        execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: dangerous_refspec.repository_id.clone(),
                expected_worktree_id: dangerous_refspec.worktree_id.clone(),
                remote: dangerous_refspec.remote.clone().unwrap(),
            },
        )
        .unwrap();
        let resolved = require_repository(repository.path()).unwrap();
        assert!(resolve_commit(&resolved, "refs/heads/remote-only").is_err());
        assert!(resolve_commit(&resolved, "refs/remotes/origin/remote-only").is_ok());
        let reserved = internal_verified_remote_name().to_owned();
        run_test_git(
            repository.path(),
            &["remote", "add", &reserved, &first_path],
        );
        let collision = workspace_snapshot(repository.path()).unwrap().unwrap();
        let collision_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: collision.repository_id,
                expected_worktree_id: collision.worktree_id,
                remote: collision
                    .remotes
                    .into_iter()
                    .find(|remote| remote.name == "origin")
                    .unwrap(),
            },
        )
        .unwrap_err();
        assert!(collision_error.contains("内部保留 transport"));
        run_test_git(repository.path(), &["remote", "remove", &reserved]);
        run_test_git(
            repository.path(),
            &[
                "config",
                &format!("remote.{reserved}.uploadpack"),
                "unsafe-upload-pack",
            ],
        );
        let hidden_collision = workspace_snapshot(repository.path()).unwrap().unwrap();
        let hidden_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: hidden_collision.repository_id,
                expected_worktree_id: hidden_collision.worktree_id,
                remote: hidden_collision
                    .remotes
                    .into_iter()
                    .find(|remote| remote.name == "origin")
                    .unwrap(),
            },
        )
        .unwrap_err();
        assert!(hidden_error.contains("内部保留 transport"));
        run_test_git(
            repository.path(),
            &["config", "--remove-section", &format!("remote.{reserved}")],
        );
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

        let unsupported = format!("ext::echo {secret}");
        run_test_git(
            repository.path(),
            &["remote", "set-url", "origin", &unsupported],
        );
        run_test_git(
            repository.path(),
            &["remote", "set-url", "--push", "origin", &unsupported],
        );
        let unsupported_snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: unsupported_snapshot.repository_id.clone(),
                expected_worktree_id: unsupported_snapshot.worktree_id.clone(),
                remote: unsupported_snapshot.remote.clone().unwrap(),
            },
        )
        .unwrap_err();
        assert!(error.contains("external remote helper"));
        assert!(!error.contains(secret));
    }

    #[test]
    fn local_dot_upstream_is_visible_but_network_actions_reject_it() {
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

        let fetch_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: snapshot.repository_id.clone(),
                expected_worktree_id: snapshot.worktree_id.clone(),
                remote: target.remote.clone(),
            },
        )
        .unwrap_err();
        assert!(fetch_error.contains("remote '.'"));
        let ff_error = execute_action(
            repository.path(),
            GitAction::Pull {
                expected_repository_id: snapshot.repository_id.clone(),
                expected_worktree_id: snapshot.worktree_id.clone(),
                expected_local_branch: snapshot.branch.clone().unwrap(),
                expected_head: snapshot.head.clone().unwrap(),
                expected_content_revision: snapshot.content_revision.clone(),
                upstream: target.clone(),
                rebase: false,
                ff_only: false,
            },
        )
        .unwrap_err();
        assert!(ff_error.contains("ffOnly"));
        let pull_error = execute_action(
            repository.path(),
            GitAction::Pull {
                expected_repository_id: snapshot.repository_id,
                expected_worktree_id: snapshot.worktree_id,
                expected_local_branch: snapshot.branch.clone().unwrap(),
                expected_head: snapshot.head.unwrap(),
                expected_content_revision: snapshot.content_revision,
                upstream: target,
                rebase: false,
                ff_only: true,
            },
        )
        .unwrap_err();
        assert!(pull_error.contains("remote '.'"));
    }

    #[test]
    fn pull_fetches_then_fast_forwards_the_exact_tracking_oid_and_rejects_dirty_or_stale_proof() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let remote = tempfile::tempdir().unwrap();
        run_test_git(remote.path(), &["init", "--bare"]);
        let remote_path = remote.path().to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["remote", "add", "origin", &remote_path],
        );
        let branch = String::from_utf8(run_test_git_output(
            repository.path(),
            &["branch", "--show-current"],
        ))
        .unwrap()
        .trim()
        .to_owned();
        run_test_git(
            repository.path(),
            &[
                "push",
                "--set-upstream",
                "origin",
                &format!("HEAD:refs/heads/{branch}"),
            ],
        );
        run_test_git(
            remote.path(),
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );

        let collaborator_parent = tempfile::tempdir().unwrap();
        let collaborator = collaborator_parent.path().join("collaborator");
        let collaborator_text = collaborator.to_string_lossy().into_owned();
        run_test_git(
            collaborator_parent.path(),
            &["clone", &remote_path, &collaborator_text],
        );
        run_test_git(&collaborator, &["config", "user.name", "Remote Test"]);
        run_test_git(
            &collaborator,
            &["config", "user.email", "remote@example.invalid"],
        );
        fs::write(collaborator.join("remote.txt"), "remote one\n").unwrap();
        run_test_git(&collaborator, &["add", "remote.txt"]);
        run_test_git(&collaborator, &["commit", "-m", "remote one"]);
        run_test_git(&collaborator, &["push", "origin", &branch]);
        let remote_one = test_commit_oid(&collaborator, "HEAD");
        install_test_git_hook(
            repository.path(),
            "post-merge",
            "printf invoked > post-merge-marker.txt",
        );

        let before = workspace_snapshot(repository.path()).unwrap().unwrap();
        let pulled = execute_action(
            repository.path(),
            GitAction::Pull {
                expected_repository_id: before.repository_id.clone(),
                expected_worktree_id: before.worktree_id.clone(),
                expected_local_branch: before.branch.clone().unwrap(),
                expected_head: before.head.clone().unwrap(),
                expected_content_revision: before.content_revision.clone(),
                upstream: before.upstream_target.clone().unwrap(),
                rebase: false,
                ff_only: true,
            },
        )
        .unwrap();
        assert_eq!(
            pulled.snapshot.unwrap().head.as_deref(),
            Some(remote_one.as_str())
        );
        assert_eq!(test_commit_oid(repository.path(), "HEAD"), remote_one);
        assert!(!repository.path().join("post-merge-marker.txt").exists());

        fs::write(collaborator.join("remote.txt"), "remote two\n").unwrap();
        run_test_git(&collaborator, &["add", "remote.txt"]);
        run_test_git(&collaborator, &["commit", "-m", "remote two"]);
        run_test_git(&collaborator, &["push", "origin", &branch]);
        fs::write(repository.path().join("tracked.txt"), "dirty\n").unwrap();
        let dirty = workspace_snapshot(repository.path()).unwrap().unwrap();
        let dirty_error = execute_action(
            repository.path(),
            GitAction::Pull {
                expected_repository_id: dirty.repository_id.clone(),
                expected_worktree_id: dirty.worktree_id.clone(),
                expected_local_branch: dirty.branch.clone().unwrap(),
                expected_head: dirty.head.clone().unwrap(),
                expected_content_revision: dirty.content_revision.clone(),
                upstream: dirty.upstream_target.clone().unwrap(),
                rebase: false,
                ff_only: true,
            },
        )
        .unwrap_err();
        assert!(dirty_error.contains("工作树干净"));
        run_test_git(repository.path(), &["restore", "tracked.txt"]);

        let confirmed = workspace_snapshot(repository.path()).unwrap().unwrap();
        let other = tempfile::tempdir().unwrap();
        run_test_git(other.path(), &["init", "--bare"]);
        let other_path = other.path().to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["remote", "set-url", "origin", &other_path],
        );
        let stale_error = execute_action(
            repository.path(),
            GitAction::Pull {
                expected_repository_id: confirmed.repository_id,
                expected_worktree_id: confirmed.worktree_id,
                expected_local_branch: confirmed.branch.clone().unwrap(),
                expected_head: confirmed.head.unwrap(),
                expected_content_revision: confirmed.content_revision,
                upstream: confirmed.upstream_target.unwrap(),
                rebase: false,
                ff_only: true,
            },
        )
        .unwrap_err();
        assert!(stale_error.contains("upstream") || stale_error.contains("transport proof"));
    }

    #[test]
    fn fullaccess_transport_accepts_local_executable_configuration() {
        assert!(git_available(), "Git is required for this regression");
        let directory = initialized_repository();
        let mut repository = require_repository(directory.path()).unwrap();
        repository.network_policy = GitNetworkPolicy::FullAccess;
        for (key, value) in [
            ("core.sshCommand", "echo custom-ssh"),
            ("core.gitProxy", "custom-proxy for example.com"),
            ("credential.helper", "!echo custom-helper"),
        ] {
            run_test_git(directory.path(), &["config", "--local", key, value]);
        }
        validate_network_git_configuration(&repository).unwrap();
        let remote = initialized_repository();
        run_test_git(directory.path(), &["remote", "add", "origin", remote.path().to_str().unwrap()]);
        let snapshot = workspace_snapshot(directory.path()).unwrap().unwrap();
        execute_action_with_policy(directory.path(), GitAction::Fetch {
            expected_repository_id: snapshot.repository_id,
            expected_worktree_id: snapshot.worktree_id,
            remote: snapshot.remote.unwrap(),
        }, GitNetworkPolicy::FullAccess).unwrap();
    }

    #[test]
    fn fullaccess_transport_scope_matrix_and_proxy_order() {
        for scope in ["system", "global", "local", "worktree"] {
            for (key, value) in [
                ("core.sshCommand", "custom-ssh --flag"),
                ("core.gitProxy", "custom-proxy for example.com"),
                ("credential.helper", "/enterprise/helper --flag"),
                ("credential.helper", "!echo custom-helper"),
            ] {
                let bytes = format!("{scope}\0file:config\0{value}\0");
                assert!(parse_network_config_values(bytes.as_bytes(), key, GitNetworkPolicy::Restricted).is_err());
                assert_eq!(parse_network_config_values(bytes.as_bytes(), key, GitNetworkPolicy::FullAccess).unwrap(), [value]);
            }
        }
        let reset = b"global\0file:g\0manager\0local\0file:l\0\0worktree\0file:w\0custom --flag\0";
        assert!(parse_trusted_credential_helpers(b"global\0file:g\0manager\0local\0file:l\0\0").unwrap().is_empty());
        assert_eq!(parse_network_config_values(b"local\0file:l\0manager\0", "credential.helper", GitNetworkPolicy::FullAccess).unwrap(), ["manager"]);
        assert_eq!(parse_network_config_values(reset, "credential.helper", GitNetworkPolicy::FullAccess).unwrap(), ["custom --flag"]);
        for bytes in [b"command\0file:c\0helper\0".as_slice(), b"local\0file:c\0bad\nvalue\0", b"local\0file:c\0bad\rvalue\0"] {
            assert!(parse_network_config_values(bytes, "credential.helper", GitNetworkPolicy::FullAccess).is_err());
        }
        let oversized = format!("local\0file:c\0{}\0", "x".repeat(MAX_GIT_REMOTE_VALUE_BYTES + 1));
        assert!(parse_network_config_values(oversized.as_bytes(), "core.sshCommand", GitNetworkPolicy::FullAccess).is_err());
        let proxies = vec!["none for internal.example.com".into(), "enterprise for example.com".into(), "fallback".into()];
        assert_eq!(bound_git_proxy(&proxies, &["git://example.com/repo".into(), "git://elsewhere.invalid/repo".into()]).unwrap(), "enterprise");
        for (host, expected) in [("internal.example.com", ""), ("sub.internal.example.com", ""), ("example.com", "enterprise"), ("notexample.com", "fallback")] {
            assert_eq!(bound_git_proxy(&proxies, &[format!("git://{host}/repo")]).unwrap(), expected);
        }
    }

    #[test]
    fn fullaccess_transport_executes_bound_scripts_despite_configuration_drift() {
        assert!(git_available(), "Git is required for this regression");
        let directory = initialized_repository();
        let ssh = "sh -c 'echo ssh-bound >> recorder; exit 1'";
        let proxy_path = directory.path().join("proxy-recorder.sh");
        fs::write(&proxy_path, "#!/bin/sh\necho proxy-bound >> recorder\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&proxy_path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let proxy = proxy_path.to_string_lossy().replace('\\', "/");
        let helper = "!f() { echo helper-bound >> recorder; echo username=bound-user; echo password=bound-password; }; f";
        run_test_git(directory.path(), &["config", "--local", "credential.helper", ""]);
        for (key, value) in [("core.sshCommand", ssh), ("core.gitProxy", proxy.as_str()), ("credential.helper", helper)] {
            run_test_git(directory.path(), &["config", "--local", "--add", key, value]);
        }
        run_test_git(directory.path(), &["remote", "add", "origin", "ssh://example.invalid/repo"]);
        let mut repository = require_repository(directory.path()).unwrap();
        let ssh_proof = remote_transport(&repository, "origin").unwrap().proof;
        assert!(verified_remote_transport(&repository, &ssh_proof, true).is_err());
        assert!(!directory.path().join("recorder").exists());
        repository.network_policy = GitNetworkPolicy::FullAccess;
        let ssh_transport = verified_remote_transport(&repository, &ssh_proof, true).unwrap();
        run_test_git(directory.path(), &["remote", "set-url", "origin", "git://example.invalid/repo"]);
        let proxy_proof = remote_transport(&repository, "origin").unwrap().proof;
        let proxy_transport = verified_remote_transport(&repository, &proxy_proof, true).unwrap();
        assert!(verified_remote_transport(&repository, &ssh_proof, true).is_err(), "stale proof must remain rejected in FullAccess");
        for key in ["core.sshCommand", "core.gitProxy", "credential.helper"] {
            run_test_git(directory.path(), &["config", "--local", "--replace-all", key, "!echo drifted >> recorder"]);
        }
        for transport in [&ssh_transport, &proxy_transport] {
            let output = run_git_with_verified_transport(
                &repository, transport, true,
                ["fetch", "--", internal_verified_remote_name()].map(OsString::from),
                None, LOCAL_COMMAND_TIMEOUT, 16 * 1024,
            ).unwrap();
            assert!(!output.success());
            assert!(!output.timed_out);
            eprintln!("recorder transport stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
        let credentials = run_git_with_verified_transport(
            &repository, &ssh_transport, true,
            ["credential", "fill"].map(OsString::from),
            Some(b"protocol=https\nhost=example.invalid\n\n".to_vec()), LOCAL_COMMAND_TIMEOUT, 16 * 1024,
        ).unwrap();
        assert!(credentials.success());
        let text = String::from_utf8(credentials.stdout).unwrap();
        assert!(text.contains("username=bound-user"));
        assert!(text.contains("password=bound-password"));
        let recorded = fs::read_to_string(directory.path().join("recorder")).unwrap();
        for marker in ["ssh-bound", "proxy-bound", "helper-bound"] {
            assert!(recorded.contains(marker), "missing {marker}: {recorded}");
        }
        assert!(!recorded.contains("drifted"));
        for locator in ["ext::sh -c evil", "ssh://-host/repo", "-oProxyCommand=evil:repo"] {
            run_test_git(directory.path(), &["config", "remote.origin.url", locator]);
            let current_proof = remote_transport(&repository, "origin").unwrap().proof;
            let error = verified_remote_transport(&repository, &current_proof, true).unwrap_err();
            assert!(!error.contains("transport proof"), "locator must be its own blocker: {error}");
        }
    }

    #[test]
    fn transport_validation_rejects_helpers_ssh_option_injection_and_local_exec_config() {
        assert!(validate_transport_locator("ext::sh -c evil").is_err());
        assert!(validate_transport_locator("hg::https://example.invalid/repo").is_err());
        assert!(validate_transport_locator("-oProxyCommand=evil:repo").is_err());
        assert!(validate_transport_locator("user@-host:repo").is_err());
        assert!(validate_transport_locator("ssh://-host/repo").is_err());
        assert!(validate_transport_locator("ssh://user@example.com/repo").is_ok());
        assert!(validate_transport_locator("user@example.com:repo").is_ok());
        assert!(validate_transport_locator("https://example.com/repo").is_ok());
        assert!(validate_transport_locator("file:///tmp/repo").is_ok());
        assert!(validate_transport_locator("C:\\repos\\target.git").is_ok());
        assert!(validate_transport_locator("-relative.git").is_err());
        assert!(validate_transport_locator("relative path/repo.git").is_err());
        assert_eq!(
            parse_trusted_credential_helpers(
                b"system\0file:/etc/gitconfig\0manager\0global\0file:/home/user/.gitconfig\0cache\0"
            )
            .unwrap(),
            ["manager", "cache"]
        );
        assert!(parse_trusted_credential_helpers(
            b"global\0file:/home/user/.gitconfig\0!sh -c evil\0"
        )
        .is_err());
        assert!(parse_trusted_credential_helpers(
            b"global\0file:/home/user/.gitconfig\0/opt/helper\0"
        )
        .is_err());
        assert!(parse_trusted_credential_helpers(b"local\0file:.git/config\0manager\0").is_err());

        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let remote = tempfile::tempdir().unwrap();
        run_test_git(remote.path(), &["init", "--bare"]);
        let remote_path = remote.path().to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["remote", "add", "origin", &remote_path],
        );
        let snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        trusted_credential_helpers(&require_repository(repository.path()).unwrap()).unwrap();
        run_test_git(
            repository.path(),
            &[
                "config",
                "--local",
                "core.sshCommand",
                "echo unsafe-command",
            ],
        );
        let core_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: snapshot.repository_id.clone(),
                expected_worktree_id: snapshot.worktree_id.clone(),
                remote: snapshot.remote.clone().unwrap(),
            },
        )
        .unwrap_err();
        assert!(core_error.contains("core.sshCommand"));
        run_test_git(
            repository.path(),
            &["config", "--local", "--unset-all", "core.sshCommand"],
        );
        run_test_git(
            repository.path(),
            &["config", "--local", "core.gitProxy", "echo unsafe-proxy"],
        );
        let proxy_snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let proxy_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: proxy_snapshot.repository_id.clone(),
                expected_worktree_id: proxy_snapshot.worktree_id.clone(),
                remote: proxy_snapshot.remote.clone().unwrap(),
            },
        )
        .unwrap_err();
        assert!(proxy_error.contains("core.gitProxy"));
        run_test_git(
            repository.path(),
            &["config", "--local", "--unset-all", "core.gitProxy"],
        );
        run_test_git(
            repository.path(),
            &[
                "config",
                "--local",
                "credential.helper",
                "!echo local-helper",
            ],
        );
        let helper_snapshot = workspace_snapshot(repository.path()).unwrap().unwrap();
        let helper_error = execute_action(
            repository.path(),
            GitAction::Fetch {
                expected_repository_id: helper_snapshot.repository_id,
                expected_worktree_id: helper_snapshot.worktree_id,
                remote: helper_snapshot.remote.unwrap(),
            },
        )
        .unwrap_err();
        assert!(helper_error.contains("credential.helper"));
    }

    #[test]
    fn merge_uses_the_confirmed_commit_even_if_the_branch_ref_moves_after_validation() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let base = workspace_snapshot(repository.path())
            .unwrap()
            .unwrap()
            .branch
            .unwrap();
        run_test_git(repository.path(), &["switch", "-c", "feature/immutable"]);
        fs::write(repository.path().join("confirmed.txt"), "confirmed\n").unwrap();
        run_test_git(repository.path(), &["add", "confirmed.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "confirmed target"]);
        let confirmed_oid = test_commit_oid(repository.path(), "HEAD");
        run_test_git(repository.path(), &["switch", &base]);
        let confirmed_head = test_commit_oid(repository.path(), "HEAD");
        let action = GitAction::Merge {
            branch: "feature/immutable".into(),
            expected_head: confirmed_head,
            expected_branch_oid: confirmed_oid.clone(),
        };
        let resolved = require_repository(repository.path()).unwrap();
        validate_expected_ref_state(&resolved, &action).unwrap();

        run_test_git(repository.path(), &["switch", "feature/immutable"]);
        fs::write(repository.path().join("moved.txt"), "moved\n").unwrap();
        run_test_git(repository.path(), &["add", "moved.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "move branch ref"]);
        run_test_git(repository.path(), &["switch", &base]);

        let (args, input, timeout) = prepare_git_action(&resolved, action).unwrap();
        assert_eq!(
            args.last().unwrap().to_string_lossy(),
            confirmed_oid.as_str()
        );
        let output = run_git(&resolved, args, input, timeout, MAX_ACTION_OUTPUT, false).unwrap();
        require_success("测试固定 merge 目标", &output).unwrap();
        assert!(repository.path().join("confirmed.txt").is_file());
        assert!(!repository.path().join("moved.txt").exists());
    }

    #[test]
    fn delete_branch_atomically_verifies_head_and_old_oid_and_rejects_unmerged_branches() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        let resolved = require_repository(repository.path()).unwrap();
        let old_oid = test_commit_oid(repository.path(), "HEAD");
        run_test_git(repository.path(), &["branch", "delete-cas"]);
        fs::write(repository.path().join("tracked.txt"), "new head\n").unwrap();
        run_test_git(repository.path(), &["add", "tracked.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "advance current head"]);
        let new_oid = test_commit_oid(repository.path(), "HEAD");
        let action = GitAction::DeleteBranch {
            name: "delete-cas".into(),
            force: false,
            expected_head: new_oid.clone(),
            expected_oid: old_oid.clone(),
        };
        validate_expected_ref_state(&resolved, &action).unwrap();
        let (args, input, timeout) = prepare_git_action(&resolved, action).unwrap();
        assert_eq!(
            args.iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["update-ref", "--stdin"]
        );
        let transaction = String::from_utf8(input.clone().unwrap()).unwrap();
        assert!(transaction.contains(&format!("verify HEAD {new_oid}\n")));
        assert!(transaction.contains(&format!("delete refs/heads/delete-cas {old_oid}\n")));
        run_test_git(
            repository.path(),
            &["update-ref", "refs/heads/delete-cas", &new_oid],
        );
        let output = run_git(&resolved, args, input, timeout, MAX_ACTION_OUTPUT, false).unwrap();
        assert!(!output.success());
        assert_eq!(
            test_commit_oid(repository.path(), "refs/heads/delete-cas"),
            new_oid
        );

        run_test_git(
            repository.path(),
            &["update-ref", "refs/heads/delete-cas", &old_oid],
        );
        let action = GitAction::DeleteBranch {
            name: "delete-cas".into(),
            force: false,
            expected_head: test_commit_oid(repository.path(), "HEAD"),
            expected_oid: old_oid.clone(),
        };
        validate_expected_ref_state(&resolved, &action).unwrap();
        let (args, input, timeout) = prepare_git_action(&resolved, action).unwrap();
        run_test_git(
            repository.path(),
            &["commit", "--allow-empty", "-m", "move head"],
        );
        let output = run_git(&resolved, args, input, timeout, MAX_ACTION_OUTPUT, false).unwrap();
        assert!(!output.success());
        assert_eq!(
            test_commit_oid(repository.path(), "refs/heads/delete-cas"),
            old_oid
        );

        run_test_git(repository.path(), &["switch", "-c", "unmerged-delete"]);
        fs::write(repository.path().join("unmerged.txt"), "unmerged\n").unwrap();
        run_test_git(repository.path(), &["add", "unmerged.txt"]);
        run_test_git(repository.path(), &["commit", "-m", "unmerged branch"]);
        let unmerged_oid = test_commit_oid(repository.path(), "HEAD");
        run_test_git(repository.path(), &["switch", "-"]);
        let error = execute_action(
            repository.path(),
            GitAction::DeleteBranch {
                name: "unmerged-delete".into(),
                force: false,
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_oid: unmerged_oid,
            },
        )
        .unwrap_err();
        assert!(error.contains("尚未合并"));
        assert!(resolve_commit(&resolved, "refs/heads/unmerged-delete").is_ok());
    }

    #[test]
    fn delete_branch_rejects_a_ref_checked_out_by_any_worktree() {
        if !git_available() {
            return;
        }
        let repository = initialized_repository();
        run_test_git(repository.path(), &["branch", "worktree-checked"]);
        let expected_oid = test_commit_oid(repository.path(), "worktree-checked");
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree = worktree_parent.path().join("checked-out");
        let worktree_text = worktree.to_string_lossy().into_owned();
        run_test_git(
            repository.path(),
            &["worktree", "add", &worktree_text, "worktree-checked"],
        );

        let error = execute_action(
            repository.path(),
            GitAction::DeleteBranch {
                name: "worktree-checked".into(),
                force: false,
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_oid,
            },
        )
        .unwrap_err();
        assert!(error.contains("worktree"));
        assert!(
            test_commit_oid(repository.path(), "refs/heads/worktree-checked")
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        run_test_git(
            repository.path(),
            &["worktree", "remove", "--force", &worktree_text],
        );
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
        let error = execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "feature/conflict".into(),
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_branch_oid: test_commit_oid(repository.path(), "feature/conflict"),
            },
        )
        .unwrap_err();
        assert!(error.contains("执行 Git 操作失败"));

        let conflicted = workspace_snapshot(repository.path()).unwrap().unwrap();
        assert_eq!(conflicted.operation, Some(GitRepositoryOperation::Merge));
        assert_eq!(conflicted.conflicted, 1);
        let expected_head = conflicted.head.unwrap();
        let blocked = execute_action(
            repository.path(),
            GitAction::Commit {
                message: "must use continue".into(),
                expected_target_revision: "0".repeat(64),
                expected_tree_oid: "0".repeat(40),
                amend: false,
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
        let history = history(
            repository.path(),
            GitHistoryRequest {
                limit: 1,
                ..GitHistoryRequest::default()
            },
        )
        .unwrap();
        assert_eq!(history.commits[0].parents.len(), 2);
    }

    #[test]
    fn conflicted_merge_can_be_aborted_and_stale_controls_fail_closed() {
        if !git_available() {
            return;
        }
        let (repository, _) = repository_with_merge_conflict();
        execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "feature/conflict".into(),
                expected_head: test_commit_oid(repository.path(), "HEAD"),
                expected_branch_oid: test_commit_oid(repository.path(), "feature/conflict"),
            },
        )
        .unwrap_err();
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

        execute_action(
            repository.path(),
            GitAction::Merge {
                branch: "feature/conflict".into(),
                expected_head: expected_head.clone(),
                expected_branch_oid: test_commit_oid(repository.path(), "feature/conflict"),
            },
        )
        .unwrap_err();
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
    fn github_readiness_core_parser_binds_repository_pr_and_viewer() {
        let repository = GithubRepository {
            host: "github.example.com".into(),
            owner: "octo".into(),
            name: "mework".into(),
            name_with_owner: "octo/mework".into(),
            url: "https://github.example.com/octo/mework".into(),
            default_branch: Some("main".into()),
            viewer_login: Some("viewer".into()),
            authenticated: true,
            gh_version: "2.50.0".into(),
        };
        let response = serde_json::json!({
            "data": {
                "viewer": { "login": "viewer" },
                "repository": {
                    "id": "R_repo",
                    "nameWithOwner": "octo/mework",
                    "mergeCommitAllowed": true,
                    "squashMergeAllowed": true,
                    "rebaseMergeAllowed": false,
                    "pullRequest": {
                        "id": "PR_17", "number": 17, "state": "OPEN", "isDraft": false,
                        "baseRefName": "main",
                        "baseRefOid": "0000000000000000000000000000000000000000",
                        "headRefName": "feature",
                        "headRefOid": "1111111111111111111111111111111111111111",
                        "mergeStateStatus": "BLOCKED", "mergeable": "MERGEABLE",
                        "viewerCanUpdate": true, "viewerCanMergeAsAdmin": false,
                        "baseRepository": { "id": "R_repo", "nameWithOwner": "octo/mework" },
                        "headRepository": { "id": "R_fork", "nameWithOwner": "contributor/mework" }
                    }
                }
            }
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let (identity, policy, viewer) =
            parse_github_readiness_core_response(&bytes, &repository, 17).unwrap();
        assert_eq!(identity.repository.node_id, "R_repo");
        assert_eq!(identity.head_repository.as_ref().unwrap().node_id, "R_fork");
        assert_eq!(identity.state, "open");
        assert_eq!(policy.merge_state_status, "BLOCKED");
        assert!(!policy.rebase_merge_allowed);
        assert_eq!(viewer.login, "viewer");

        let mut mismatched = response;
        mismatched["data"]["viewer"]["login"] = Value::String("other".into());
        assert!(parse_github_readiness_core_response(
            &serde_json::to_vec(&mismatched).unwrap(),
            &repository,
            17
        )
        .is_err());
    }

    #[test]
    fn github_readiness_scope_rejects_draft_and_base_or_head_repository_drift() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let expected = &readiness.identity;
        let scope = serde_json::json!({
            "id": expected.pull_request_node_id,
            "number": expected.number,
            "state": "OPEN",
            "isDraft": false,
            "baseRefName": expected.base_ref_name,
            "baseRefOid": expected.base_ref_oid,
            "headRefName": expected.head_ref_name,
            "headRefOid": expected.head_ref_oid,
            "baseRepository": {
                "id": expected.base_repository.node_id,
                "nameWithOwner": expected.base_repository.name_with_owner
            },
            "headRepository": {
                "id": expected.head_repository.as_ref().unwrap().node_id,
                "nameWithOwner": expected.head_repository.as_ref().unwrap().name_with_owner
            }
        });
        validate_github_readiness_pull_request_scope(&scope, expected, "readiness page").unwrap();

        let mut draft = scope.clone();
        draft["isDraft"] = Value::Bool(true);
        assert!(
            validate_github_readiness_pull_request_scope(&draft, expected, "readiness page")
                .unwrap_err()
                .contains("身份已变化")
        );

        let mut base_drift = scope.clone();
        base_drift["baseRepository"]["id"] = Value::String("R_other".into());
        assert!(validate_github_readiness_pull_request_scope(
            &base_drift,
            expected,
            "readiness page"
        )
        .unwrap_err()
        .contains("身份已变化"));

        let mut head_drift = scope;
        head_drift["headRepository"]["nameWithOwner"] = Value::String("other/project".into());
        assert!(validate_github_readiness_pull_request_scope(
            &head_drift,
            expected,
            "readiness page"
        )
        .unwrap_err()
        .contains("身份已变化"));
    }

    #[test]
    fn github_readiness_revisions_are_domain_separated_and_stable() {
        let value = GithubPullRequestViewer {
            login: "viewer".into(),
            can_update: true,
            can_merge_as_admin: false,
        };
        let first = github_readiness_revision(b"mework.test.identity.v1", &value).unwrap();
        let repeated = github_readiness_revision(b"mework.test.identity.v1", &value).unwrap();
        let other_domain = github_readiness_revision(b"mework.test.aggregate.v1", &value).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(first.len(), 64);
        assert_ne!(first, other_domain);
    }

    #[test]
    fn github_merge_readiness_enforces_policy_and_gate_matrix_without_admin_bypass() {
        for status in ["CLEAN", "HAS_HOOKS"] {
            let readiness = github_merge_readiness_fixture(status);
            for method in [
                GithubMergeMethod::Merge,
                GithubMergeMethod::Squash,
                GithubMergeMethod::Rebase,
            ] {
                assert_eq!(
                    validate_github_merge_readiness_fixture(&readiness, method).unwrap(),
                    readiness.identity.head_ref_oid
                );
            }
        }

        let mut unstable = github_merge_readiness_fixture("UNSTABLE");
        unstable.checks = GithubReadinessPhase::available(GithubPullRequestChecks {
            total_count: 4,
            checks: vec![
                GithubPullRequestReadinessCheck {
                    node_id: "CR_success".into(),
                    kind: "CheckRun".into(),
                    name: "build".into(),
                    state: "COMPLETED".into(),
                    conclusion: Some("SUCCESS".into()),
                    workflow: Some("CI".into()),
                    description: None,
                    link: None,
                    started_at: None,
                    completed_at: None,
                    required: true,
                },
                GithubPullRequestReadinessCheck {
                    node_id: "CR_neutral".into(),
                    kind: "CheckRun".into(),
                    name: "lint".into(),
                    state: "COMPLETED".into(),
                    conclusion: Some("NEUTRAL".into()),
                    workflow: Some("CI".into()),
                    description: None,
                    link: None,
                    started_at: None,
                    completed_at: None,
                    required: true,
                },
                GithubPullRequestReadinessCheck {
                    node_id: "SC_success".into(),
                    kind: "StatusContext".into(),
                    name: "policy".into(),
                    state: "SUCCESS".into(),
                    conclusion: None,
                    workflow: None,
                    description: None,
                    link: None,
                    started_at: None,
                    completed_at: None,
                    required: true,
                },
                GithubPullRequestReadinessCheck {
                    node_id: "CR_optional".into(),
                    kind: "CheckRun".into(),
                    name: "optional".into(),
                    state: "COMPLETED".into(),
                    conclusion: Some("FAILURE".into()),
                    workflow: Some("CI".into()),
                    description: None,
                    link: None,
                    started_at: None,
                    completed_at: None,
                    required: false,
                },
            ],
        });
        refresh_github_readiness_revisions(&mut unstable);
        assert!(
            validate_github_merge_readiness_fixture(&unstable, GithubMergeMethod::Squash).is_ok()
        );

        for (kind, state, conclusion) in [
            ("CheckRun", "IN_PROGRESS", None),
            ("CheckRun", "COMPLETED", Some("FAILURE")),
            ("StatusContext", "PENDING", None),
        ] {
            let mut rejected = github_merge_readiness_fixture("UNSTABLE");
            rejected.checks = GithubReadinessPhase::available(GithubPullRequestChecks {
                total_count: 1,
                checks: vec![GithubPullRequestReadinessCheck {
                    node_id: format!("{kind}_{state}"),
                    kind: kind.into(),
                    name: "required".into(),
                    state: state.into(),
                    conclusion: conclusion.map(str::to_owned),
                    workflow: None,
                    description: None,
                    link: None,
                    started_at: None,
                    completed_at: None,
                    required: true,
                }],
            });
            refresh_github_readiness_revisions(&mut rejected);
            assert!(
                validate_github_merge_readiness_fixture(&rejected, GithubMergeMethod::Merge)
                    .unwrap_err()
                    .contains("required check")
            );
        }

        for availability in [
            GithubReadinessAvailability::Unsupported,
            GithubReadinessAvailability::Error,
        ] {
            let mut rejected = github_merge_readiness_fixture("UNSTABLE");
            rejected.checks = GithubReadinessPhase {
                availability,
                value: None,
                error: Some("unavailable".into()),
            };
            refresh_github_readiness_revisions(&mut rejected);
            assert!(
                validate_github_merge_readiness_fixture(&rejected, GithubMergeMethod::Merge)
                    .is_err()
            );
        }

        for status in [
            "BLOCKED",
            "BEHIND",
            "DIRTY",
            "DRAFT",
            "UNKNOWN",
            "FUTURE_STATE",
        ] {
            let readiness = github_merge_readiness_fixture(status);
            assert!(
                validate_github_merge_readiness_fixture(&readiness, GithubMergeMethod::Merge)
                    .is_err(),
                "{status} unexpectedly passed"
            );
        }

        let mut not_mergeable = github_merge_readiness_fixture("CLEAN");
        not_mergeable.merge_policy.mergeable = "UNKNOWN".into();
        refresh_github_readiness_revisions(&mut not_mergeable);
        assert!(
            validate_github_merge_readiness_fixture(&not_mergeable, GithubMergeMethod::Merge)
                .is_err()
        );

        for method in [
            GithubMergeMethod::Merge,
            GithubMergeMethod::Squash,
            GithubMergeMethod::Rebase,
        ] {
            let mut rejected = github_merge_readiness_fixture("CLEAN");
            match method {
                GithubMergeMethod::Merge => rejected.merge_policy.merge_commit_allowed = false,
                GithubMergeMethod::Squash => rejected.merge_policy.squash_merge_allowed = false,
                GithubMergeMethod::Rebase => rejected.merge_policy.rebase_merge_allowed = false,
            }
            refresh_github_readiness_revisions(&mut rejected);
            assert!(validate_github_merge_readiness_fixture(&rejected, method).is_err());
        }
    }

    #[test]
    fn github_merge_readiness_rejects_identity_and_revision_proof_drift() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let expected_repository = GithubRepositoryIdentity {
            host: "github.example".into(),
            owner: "team".into(),
            name: "project".into(),
        };
        let validate = |head: &str,
                        base: &str,
                        identity_revision: &str,
                        readiness_revision: &str,
                        viewer: &str| {
            validate_github_merge_readiness(
                &readiness,
                &expected_repository,
                viewer,
                17,
                head,
                base,
                "open",
                identity_revision,
                readiness_revision,
                GithubMergeMethod::Merge,
            )
        };
        assert!(validate(
            "1123456789abcdef0123456789abcdef01234567",
            &readiness.identity.base_ref_oid,
            &readiness.identity_revision,
            &readiness.readiness_revision,
            "viewer"
        )
        .is_err());
        assert!(validate(
            &readiness.identity.head_ref_oid,
            "99abcdef0123456789abcdef0123456789abcdef",
            &readiness.identity_revision,
            &readiness.readiness_revision,
            "viewer"
        )
        .is_err());
        assert!(validate(
            &readiness.identity.head_ref_oid,
            &readiness.identity.base_ref_oid,
            &"0".repeat(64),
            &readiness.readiness_revision,
            "viewer"
        )
        .is_err());
        assert!(validate(
            &readiness.identity.head_ref_oid,
            &readiness.identity.base_ref_oid,
            &readiness.identity_revision,
            &"f".repeat(64),
            "viewer"
        )
        .is_err());
        assert!(validate(
            &readiness.identity.head_ref_oid,
            &readiness.identity.base_ref_oid,
            &readiness.identity_revision,
            &readiness.readiness_revision,
            "other"
        )
        .is_err());

        let mut draft = readiness.clone();
        draft.identity.draft = true;
        refresh_github_readiness_revisions(&mut draft);
        assert!(validate_github_merge_readiness_fixture(&draft, GithubMergeMethod::Merge).is_err());
        let mut closed = readiness;
        closed.identity.state = "closed".into();
        refresh_github_readiness_revisions(&mut closed);
        assert!(
            validate_github_merge_readiness_fixture(&closed, GithubMergeMethod::Merge).is_err()
        );
    }

    #[test]
    fn github_readiness_checks_query_enforces_page_and_cursor_bounds() {
        let repository = GithubRepository {
            host: "github.example.com".into(),
            owner: "octo".into(),
            name: "mework".into(),
            name_with_owner: "octo/mework".into(),
            url: "https://github.example.com/octo/mework".into(),
            default_branch: Some("main".into()),
            viewer_login: Some("viewer".into()),
            authenticated: true,
            gh_version: "2.50.0".into(),
        };
        let input = github_readiness_query_input(
            GITHUB_READINESS_CHECKS_QUERY,
            &repository,
            17,
            Some((100, Some("cursor-1"))),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&input).unwrap();
        assert_eq!(value["query"].as_str(), Some(GITHUB_READINESS_CHECKS_QUERY));
        let query = value["query"].as_str().unwrap();
        for required_scope in [
            "isDraft",
            "baseRepository { id nameWithOwner }",
            "headRepository { id nameWithOwner }",
            "isRequired(pullRequestNumber: $number)",
        ] {
            assert!(query.contains(required_scope));
        }
        let compact_query = query.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(compact_query.contains("checkSuite { workflowRun { workflow { name } } }"));
        assert!(!compact_query.contains("completedAt workflow { name }"));
        assert_eq!(value["variables"]["first"], 100);
        assert_eq!(value["variables"]["after"], "cursor-1");
        for page_size in [0, 101] {
            assert!(github_readiness_query_input(
                GITHUB_READINESS_CHECKS_QUERY,
                &repository,
                17,
                Some((page_size, None))
            )
            .is_err());
        }
        assert!(github_readiness_query_input(
            GITHUB_READINESS_CHECKS_QUERY,
            &repository,
            i32::MAX as u64 + 1,
            Some((100, None))
        )
        .is_err());
        let oversized_cursor = "x".repeat(MAX_GITHUB_REVIEW_CURSOR_BYTES + 1);
        assert!(github_readiness_query_input(
            GITHUB_READINESS_CHECKS_QUERY,
            &repository,
            17,
            Some((100, Some(&oversized_cursor)))
        )
        .is_err());
    }

    #[test]
    fn github_readiness_check_parser_requires_required_state() {
        let check_run = serde_json::json!({
            "__typename": "CheckRun", "id": "CR_1", "name": "test",
            "status": "COMPLETED", "conclusion": "SUCCESS",
            "detailsUrl": "https://github.example.com/check/1",
            "startedAt": "2026-07-25T00:00:00Z",
            "completedAt": "2026-07-25T00:01:00Z",
            "checkSuite": {
                "workflowRun": {
                    "workflow": { "name": "CI" }
                }
            },
            "isRequired": true
        });
        let parsed = parse_github_readiness_check(&check_run).unwrap();
        assert_eq!(parsed.kind, "CheckRun");
        assert!(parsed.required);
        assert_eq!(parsed.workflow.as_deref(), Some("CI"));
        let mut no_suite = check_run.clone();
        no_suite["checkSuite"] = Value::Null;
        no_suite["workflow"] = serde_json::json!({ "name": "legacy-invalid-path" });
        assert_eq!(
            parse_github_readiness_check(&no_suite).unwrap().workflow,
            None
        );
        let mut invalid = check_run;
        invalid["isRequired"] = Value::Null;
        assert!(parse_github_readiness_check(&invalid).is_err());

        let duplicate = serde_json::json!({
            "__typename": "StatusContext", "id": "SC_1", "context": "lint",
            "state": "SUCCESS", "description": null, "targetUrl": null,
            "isRequired": false
        });
        let mut checks = Vec::new();
        let mut seen = HashSet::new();
        append_github_readiness_check_nodes(&mut checks, &mut seen, &[duplicate.clone()], 2)
            .unwrap();
        assert!(
            append_github_readiness_check_nodes(&mut checks, &mut seen, &[duplicate], 2)
                .unwrap_err()
                .contains("重复")
        );
        assert!(append_github_readiness_check_nodes(
            &mut Vec::new(),
            &mut HashSet::new(),
            &[serde_json::json!({
                "__typename": "StatusContext", "id": "SC_2", "context": "build",
                "state": "SUCCESS", "description": null, "targetUrl": null,
                "isRequired": true
            })],
            0
        )
        .unwrap_err()
        .contains("分页元数据"));

        let stalled_page = serde_json::json!({
            "hasNextPage": true,
            "endCursor": "cursor-1"
        });
        let next = parse_github_page_next_cursor(&stalled_page, "GitHub readiness checks").unwrap();
        assert!(validate_github_readiness_next_cursor(
            Some("cursor-1"),
            next,
            1,
            &mut HashSet::new()
        )
        .unwrap_err()
        .contains("未前进"));
        let mut cursors = HashSet::from(["cursor-2".to_owned()]);
        assert!(validate_github_readiness_next_cursor(
            Some("cursor-1"),
            Some("cursor-2".into()),
            1,
            &mut cursors
        )
        .unwrap_err()
        .contains("循环"));
    }

    #[test]
    fn github_readiness_optional_phases_parse_available_values_and_null_auto_merge() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let mut response = github_readiness_phase_response_fixture(&readiness);
        response["data"]["repository"]["viewerDefaultMergeMethod"] = Value::String("SQUASH".into());
        response["data"]["repository"]["pullRequest"]["autoMergeRequest"] = serde_json::json!({
            "enabledAt": "2026-07-25T01:02:03Z",
            "mergeMethod": "REBASE",
            "commitHeadline": "Merge the reviewed change",
            "commitBody": null,
            "enabledBy": { "login": "viewer" }
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let viewer_default = parse_github_readiness_viewer_default_response(
            &bytes,
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap();
        assert_eq!(viewer_default.merge_method, "SQUASH");
        let auto_merge = parse_github_readiness_auto_merge_response(
            &bytes,
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            auto_merge.enabled_at.as_deref(),
            Some("2026-07-25T01:02:03Z")
        );
        assert_eq!(auto_merge.merge_method, "REBASE");
        assert_eq!(
            auto_merge.commit_headline.as_deref(),
            Some("Merge the reviewed change")
        );
        assert_eq!(auto_merge.enabled_by.as_deref(), Some("viewer"));

        response["data"]["repository"]["pullRequest"]["autoMergeRequest"]["enabledAt"] =
            Value::Null;
        assert_eq!(
            parse_github_readiness_auto_merge_response(
                &serde_json::to_vec(&response).unwrap(),
                &readiness.identity,
                &readiness.viewer.login,
            )
            .unwrap()
            .unwrap()
            .enabled_at,
            None
        );
        response["data"]["repository"]["pullRequest"]["autoMergeRequest"] = Value::Null;
        assert_eq!(
            parse_github_readiness_auto_merge_response(
                &serde_json::to_vec(&response).unwrap(),
                &readiness.identity,
                &readiness.viewer.login,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn github_readiness_optional_phases_use_full_scope_validation() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let expected = &readiness.identity;
        let viewer = &readiness.viewer.login;

        let mut viewer_default = github_readiness_phase_response_fixture(&readiness);
        viewer_default["data"]["repository"]["viewerDefaultMergeMethod"] =
            Value::String("MERGE".into());
        viewer_default["data"]["repository"]["pullRequest"]["baseRefOid"] =
            Value::String("f".repeat(40));
        assert!(parse_github_readiness_viewer_default_response(
            &serde_json::to_vec(&viewer_default).unwrap(),
            expected,
            viewer,
        )
        .unwrap_err()
        .contains("身份已变化"));

        let mut auto_merge = github_readiness_phase_response_fixture(&readiness);
        auto_merge["data"]["repository"]["pullRequest"]["autoMergeRequest"] = Value::Null;
        auto_merge["data"]["viewer"]["login"] = Value::String("other-viewer".into());
        assert!(parse_github_readiness_auto_merge_response(
            &serde_json::to_vec(&auto_merge).unwrap(),
            expected,
            viewer,
        )
        .unwrap_err()
        .contains("viewer 已变化"));

        let mut merge_queue = github_readiness_phase_response_fixture(&readiness);
        merge_queue["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] =
            Value::Bool(false);
        merge_queue["data"]["repository"]["pullRequest"]["isInMergeQueue"] = Value::Bool(false);
        merge_queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"] = Value::Null;
        merge_queue["data"]["repository"]["id"] = Value::String("R_other".into());
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&merge_queue).unwrap(),
            expected,
            viewer,
        )
        .unwrap_err()
        .contains("repository 身份已变化"));
    }

    #[test]
    fn github_readiness_merge_queue_distinguishes_requirement_from_entry_presence() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let mut response = github_readiness_phase_response_fixture(&readiness);
        response["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] = Value::Bool(true);
        response["data"]["repository"]["pullRequest"]["isInMergeQueue"] = Value::Bool(false);
        response["data"]["repository"]["pullRequest"]["mergeQueueEntry"] = Value::Null;
        let enabled_without_entry = parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&response).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap();
        assert!(enabled_without_entry.enabled);
        assert!(!enabled_without_entry.is_in_queue);
        assert_eq!(enabled_without_entry.entry, None);

        response["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] = Value::Bool(false);
        let disabled = parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&response).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.entry, None);

        response["data"]["repository"]["pullRequest"]["isInMergeQueue"] = Value::Bool(true);
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&response).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("状态不一致"));

        response["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] = Value::Bool(true);
        response["data"]["repository"]["pullRequest"]["isInMergeQueue"] = Value::Bool(true);
        response["data"]["repository"]["pullRequest"]["mergeQueueEntry"] = serde_json::json!({
            "id": "MQE_17",
            "position": i32::MAX,
            "state": "AWAITING_CHECKS",
            "enqueuedAt": "2026-07-25T01:02:03Z",
            "estimatedTimeToMerge": i32::MAX
        });
        let queued = parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&response).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap();
        let entry = queued.entry.unwrap();
        assert_eq!(entry.position, i32::MAX as u64);
        assert_eq!(entry.estimated_time_to_merge, Some(i32::MAX as u64));
        assert_eq!(entry.enqueued_at, "2026-07-25T01:02:03Z");

        response["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] = Value::Bool(false);
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&response).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("未启用"));
    }

    #[test]
    fn github_readiness_optional_phase_type_bounds_fail_closed() {
        let readiness = github_merge_readiness_fixture("CLEAN");
        let mut queue = github_readiness_phase_response_fixture(&readiness);
        queue["data"]["repository"]["pullRequest"]["isMergeQueueEnabled"] = Value::Bool(true);
        queue["data"]["repository"]["pullRequest"]["isInMergeQueue"] = Value::Bool(true);
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"] = serde_json::json!({
            "id": "MQE_17",
            "position": i32::MAX as u64 + 1,
            "state": "QUEUED",
            "enqueuedAt": "2026-07-25T01:02:03Z",
            "estimatedTimeToMerge": null
        });
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&queue).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("GraphQL Int 上限"));
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["position"] = Value::from(1);
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["estimatedTimeToMerge"] =
            Value::from(i32::MAX as u64 + 1);
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&queue).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("GraphQL Int 上限"));
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["estimatedTimeToMerge"] =
            Value::Null;
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["enqueuedAt"] =
            Value::String("x".repeat(MAX_GITHUB_READINESS_TIMESTAMP_BYTES + 1));
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&queue).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("enqueuedAt"));
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["enqueuedAt"] =
            Value::String("2026-07-25T01:02:03Z".into());
        queue["data"]["repository"]["pullRequest"]["mergeQueueEntry"]
            .as_object_mut()
            .unwrap()
            .remove("estimatedTimeToMerge");
        assert!(parse_github_readiness_merge_queue_response(
            &serde_json::to_vec(&queue).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("缺少字段 estimatedTimeToMerge"));

        let mut auto_merge = github_readiness_phase_response_fixture(&readiness);
        auto_merge["data"]["repository"]["pullRequest"]["autoMergeRequest"] = serde_json::json!({
            "enabledAt": 17,
            "mergeMethod": "SQUASH",
            "commitHeadline": null,
            "commitBody": null,
            "enabledBy": null
        });
        assert!(parse_github_readiness_auto_merge_response(
            &serde_json::to_vec(&auto_merge).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("enabledAt"));
        auto_merge["data"]["repository"]["pullRequest"]["autoMergeRequest"]["enabledAt"] =
            Value::Null;
        auto_merge["data"]["repository"]["pullRequest"]["autoMergeRequest"]["mergeMethod"] =
            Value::String("FUTURE".into());
        assert!(parse_github_readiness_auto_merge_response(
            &serde_json::to_vec(&auto_merge).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("merge method"));
        auto_merge["data"]["repository"]["pullRequest"]["autoMergeRequest"]
            .as_object_mut()
            .unwrap()
            .remove("enabledAt");
        assert!(parse_github_readiness_auto_merge_response(
            &serde_json::to_vec(&auto_merge).unwrap(),
            &readiness.identity,
            &readiness.viewer.login,
        )
        .unwrap_err()
        .contains("缺少字段 enabledAt"));
    }

    #[test]
    fn github_readiness_undefined_field_is_unsupported_but_operational_errors_are_not() {
        let undefined = serde_json::json!({
            "errors": [{
                "message": "schema validation failed",
                "extensions": { "code": "undefinedField" }
            }]
        });
        let error = github_graphql_data(&undefined, "optional phase").unwrap_err();
        let phase = GithubReadinessPhase::<GithubPullRequestViewerDefault>::unavailable(error);
        assert_eq!(phase.availability, GithubReadinessAvailability::Unsupported);

        for response in [
            serde_json::json!({
                "errors": [{
                    "message": "Resource not accessible by integration",
                    "extensions": { "code": "FORBIDDEN" }
                }]
            }),
            serde_json::json!({
                "errors": [{
                    "message": "API rate limit exceeded",
                    "extensions": { "code": "RATE_LIMITED" }
                }]
            }),
        ] {
            let error = github_graphql_data(&response, "optional phase").unwrap_err();
            let phase = GithubReadinessPhase::<GithubPullRequestViewerDefault>::unavailable(error);
            assert_eq!(phase.availability, GithubReadinessAvailability::Error);
        }
    }

    #[test]
    fn github_readiness_optional_queries_repeat_the_complete_scope() {
        for query in [
            GITHUB_READINESS_VIEWER_DEFAULT_QUERY,
            GITHUB_READINESS_AUTO_MERGE_QUERY,
            GITHUB_READINESS_MERGE_QUEUE_QUERY,
        ] {
            for required_scope in [
                "viewer { login }",
                "id",
                "nameWithOwner",
                "number",
                "state",
                "isDraft",
                "baseRefName",
                "baseRefOid",
                "headRefName",
                "headRefOid",
                "baseRepository { id nameWithOwner }",
                "headRepository { id nameWithOwner }",
            ] {
                assert!(
                    query.contains(required_scope),
                    "query omitted {required_scope}"
                );
            }
        }
        assert!(GITHUB_READINESS_VIEWER_DEFAULT_QUERY.contains("viewerDefaultMergeMethod"));
        assert!(GITHUB_READINESS_AUTO_MERGE_QUERY.contains("autoMergeRequest"));
        assert!(GITHUB_READINESS_MERGE_QUEUE_QUERY.contains("isMergeQueueEnabled"));
        assert!(GITHUB_READINESS_MERGE_QUEUE_QUERY.contains("isInMergeQueue"));
        assert!(GITHUB_READINESS_MERGE_QUEUE_QUERY.contains("estimatedTimeToMerge"));
    }

    #[test]
    fn github_readiness_revision_includes_optional_phase_results() {
        let original = github_merge_readiness_fixture("CLEAN");
        let original_revision = original.readiness_revision.clone();

        let mut changed = original.clone();
        changed.viewer_default = GithubReadinessPhase::available(GithubPullRequestViewerDefault {
            merge_method: "SQUASH".into(),
        });
        refresh_github_readiness_revisions(&mut changed);
        assert_ne!(changed.readiness_revision, original_revision);

        let viewer_revision = changed.readiness_revision.clone();
        changed.auto_merge = GithubReadinessPhase::available(Some(GithubPullRequestAutoMerge {
            enabled_at: None,
            merge_method: "SQUASH".into(),
            commit_headline: None,
            commit_body: None,
            enabled_by: Some("viewer".into()),
        }));
        refresh_github_readiness_revisions(&mut changed);
        assert_ne!(changed.readiness_revision, viewer_revision);

        let auto_revision = changed.readiness_revision.clone();
        changed.merge_queue = GithubReadinessPhase::available(GithubPullRequestMergeQueueState {
            enabled: true,
            is_in_queue: false,
            entry: None,
        });
        refresh_github_readiness_revisions(&mut changed);
        assert_ne!(changed.readiness_revision, auto_revision);
    }

    #[test]
    fn github_merge_readiness_requires_known_non_queue_target_and_consistent_checks() {
        let mut readiness = github_merge_readiness_fixture("CLEAN");
        readiness.merge_queue = GithubReadinessPhase::available(GithubPullRequestMergeQueueState {
            enabled: true,
            is_in_queue: false,
            entry: None,
        });
        refresh_github_readiness_revisions(&mut readiness);
        assert!(
            validate_github_merge_readiness_fixture(&readiness, GithubMergeMethod::Merge)
                .unwrap_err()
                .contains("merge queue")
        );

        readiness.merge_queue = github_readiness_unsupported_phase("GitHub merge queue");
        refresh_github_readiness_revisions(&mut readiness);
        assert!(
            validate_github_merge_readiness_fixture(&readiness, GithubMergeMethod::Merge)
                .unwrap_err()
                .contains("无法确认")
        );

        readiness.merge_queue = GithubReadinessPhase::available(GithubPullRequestMergeQueueState {
            enabled: false,
            is_in_queue: false,
            entry: None,
        });
        readiness.checks = GithubReadinessPhase::available(GithubPullRequestChecks {
            total_count: 1,
            checks: vec![GithubPullRequestReadinessCheck {
                node_id: "CR_required_failure".into(),
                kind: "CheckRun".into(),
                name: "required".into(),
                state: "COMPLETED".into(),
                conclusion: Some("FAILURE".into()),
                workflow: Some("CI".into()),
                description: None,
                link: None,
                started_at: None,
                completed_at: None,
                required: true,
            }],
        });
        refresh_github_readiness_revisions(&mut readiness);
        assert!(
            validate_github_merge_readiness_fixture(&readiness, GithubMergeMethod::Merge)
                .unwrap_err()
                .contains("矛盾")
        );

        readiness.checks = github_readiness_unsupported_phase("GitHub checks");
        refresh_github_readiness_revisions(&mut readiness);
        assert!(
            validate_github_merge_readiness_fixture(&readiness, GithubMergeMethod::Merge).is_ok()
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
