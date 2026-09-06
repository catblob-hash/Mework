import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  GitCommitPreparation,
  GitDiscardPreparation,
  GitDiffResult,
  GitHubPullRequest,
  GitHubPullRequestDetail,
  GitHubPullRequestDiff,
  GitHubPullRequestReadiness,
  GitHubPullRequestReviewThreadCommentsResult,
  GitHubPullRequestReviewThread,
  GitHubRepository,
  GitRemote,
  GitStageAllPreparation,
  GitWorkspaceSnapshot
} from "../lib/git";
import { installExternalLinkInterceptor } from "../lib/externalLinks";
import { gitConversationTarget } from "../lib/git";
import { GitReviewPanel } from "./GitReviewPanel";

const git = vi.hoisted(() => ({
  executeGitAction: vi.fn(),
  executeGitHubAction: vi.fn(),
  getGitBranches: vi.fn(),
  getGitChangePage: vi.fn(),
  getGitDiff: vi.fn(),
  getGitHistory: vi.fn(),
  getGitHubPullRequestDetail: vi.fn(),
  getGitHubPullRequestDiff: vi.fn(),
  getGitHubPullRequestReadiness: vi.fn(),
  getGitHubPullRequestReviewThreadComments: vi.fn(),
  getGitHubPullRequestReviewThreads: vi.fn(),
  getGitHubPullRequests: vi.fn(),
  getGitHubRepository: vi.fn(),
  getGitWorkspaceSummary: vi.fn(),
  prepareGitCommit: vi.fn(),
  prepareGitDiscard: vi.fn(),
  prepareGitStageAll: vi.fn()
}));

vi.mock("../lib/git", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/git")>(),
  ...git
}));

const originRemote: GitRemote = {
  name: "origin",
  fetchRevision: "origin-fetch-revision-1",
  pushRevision: "origin-push-revision-1",
  url: "https://github.com/example-org/Mework.git"
};

const snapshot: GitWorkspaceSnapshot = {
  repositoryId: "repository-id-1",
  worktreeId: "worktree-id-1",
  branch: "feature/review",
  head: "12ab34cd",
  contentRevision: "revision-1",
  upstream: "origin/feature/review",
  upstreamTarget: {
    remoteName: "origin",
    remoteBranch: "feature/review",
    mergeRef: "refs/heads/feature/review",
    trackingRef: "refs/remotes/origin/feature/review",
    trackingOid: "fedcba9876543210fedcba9876543210fedcba98",
    isLocal: false,
    remote: originRemote
  },
  ahead: 1,
  behind: 0,
  additions: 7,
  deletions: 2,
  staged: 1,
  unstaged: 1,
  untracked: 0,
  conflicted: 0,
  stash: 0,
  files: [{
    path: "src/App.tsx",
    status: "modified",
    staged: true,
    unstaged: true,
    additions: 7,
    deletions: 2
  }],
  remote: originRemote,
  remotes: [originRemote],
  gitVersion: "git version 2.50.0",
  detached: false,
  unborn: false,
  operation: null,
  operationRevision: null,
  isClean: false,
  binaryFiles: 0,
  warnings: []
};

const stageAllSnapshot: GitWorkspaceSnapshot = {
  ...snapshot,
  summaryRevision: "stage-all-summary-revision-1",
  changedFiles: snapshot.files.length,
  stageable: 1,
  unstageable: 1,
  filesComplete: false,
  files: []
};

const stageAllCandidateTreeOid = "a".repeat(40);
const commitCandidateTreeOid = "b".repeat(40);

const repository: GitHubRepository = {
  host: "github.com",
  owner: "example-org",
  name: "Mework",
  nameWithOwner: "example-org/Mework",
  url: "https://github.com/example-org/Mework",
  defaultBranch: "main",
  viewerLogin: "example-user",
  authenticated: true,
  ghVersion: "gh version 2.75.0"
};

const pullRequest: GitHubPullRequest = {
  number: 17,
  title: "Add Git review",
  state: "open",
  url: "https://github.com/example-org/Mework/pull/17",
  author: "example-user",
  headRefName: "feature/review",
  baseRefName: "main",
  draft: false,
  updatedAt: "2026-07-24T00:00:00Z"
};

const pullRequestDetail: GitHubPullRequestDetail = {
  ...pullRequest,
  headRefOid: "0123456789abcdef0123456789abcdef01234567",
  body: "Review the built-in Git UI.",
  additions: 50,
  deletions: 8,
  changedFiles: 4,
  commits: 2
};

const pullRequestReadiness: GitHubPullRequestReadiness = {
  identity: {
    repository: {
      host: repository.host,
      nodeId: "repository-node",
      nameWithOwner: repository.nameWithOwner
    },
    pullRequestNodeId: "pull-request-node",
    number: pullRequest.number,
    state: "open",
    draft: false,
    baseRepository: {
      host: repository.host,
      nodeId: "repository-node",
      nameWithOwner: repository.nameWithOwner
    },
    headRepository: {
      host: repository.host,
      nodeId: "repository-node",
      nameWithOwner: repository.nameWithOwner
    },
    baseRefName: pullRequest.baseRefName,
    baseRefOid: "89abcdef0123456789abcdef0123456789abcdef",
    headRefName: pullRequest.headRefName,
    headRefOid: pullRequestDetail.headRefOid
  },
  mergePolicy: {
    mergeStateStatus: "CLEAN",
    mergeable: "MERGEABLE",
    mergeCommitAllowed: false,
    squashMergeAllowed: true,
    rebaseMergeAllowed: false
  },
  viewer: {
    login: repository.viewerLogin!,
    canUpdate: true,
    canMergeAsAdmin: false
  },
  checks: {
    availability: "available",
    value: { totalCount: 0, checks: [] },
    error: null
  },
  viewerDefault: {
    availability: "available",
    value: { mergeMethod: "SQUASH" },
    error: null
  },
  autoMerge: {
    availability: "available",
    value: null,
    error: null
  },
  mergeQueue: {
    availability: "available",
    value: {
      enabled: false,
      isInQueue: false,
      entry: null
    },
    error: null
  },
  identityRevision: "identity-revision-1",
  readinessRevision: "readiness-revision-1"
};

const reviewThread: GitHubPullRequestReviewThread = {
  id: "thread-1",
  path: "src/App.tsx",
  line: 1,
  startLine: null,
  diffSide: "RIGHT",
  startDiffSide: null,
  originalLine: 1,
  originalStartLine: null,
  isResolved: false,
  isOutdated: false,
  viewerCanReply: true,
  viewerCanResolve: true,
  viewerCanUnresolve: false,
  comments: [{
    id: "comment-1",
    author: "reviewer",
    body: "Please keep this branch explicit.",
    createdAt: "2026-07-24T00:00:00Z",
    updatedAt: "2026-07-24T00:00:00Z",
    url: "https://github.com/example-org/Mework/pull/17#discussion_r1",
    replyToId: null
  }],
  commentsTotalCount: 1,
  commentsNextCursor: null
};

function createPagedChangesSnapshot(paths: string[], summaryRevision = "summary-large") {
  const files = paths.map((path) => ({
    path,
    status: "modified" as const,
    staged: true,
    unstaged: false,
    additions: 1,
    deletions: 0
  }));
  const pagedSnapshot: GitWorkspaceSnapshot = {
    ...snapshot,
    contentRevision: `large-changes-${paths.length}`,
    summaryRevision,
    additions: paths.length,
    deletions: 0,
    staged: paths.length,
    unstaged: 0,
    changedFiles: paths.length,
    stageable: 0,
    unstageable: paths.length,
    filesComplete: false,
    files: []
  };
  return { snapshot: pagedSnapshot, files };
}

describe("GitReviewPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    git.getGitDiff.mockImplementation((_, request) => Promise.resolve({
      patch: "@@ -1 +1 @@\n-old\n+new",
      path: request.path ?? null,
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: [],
      ...(request.expectedStageAllTargetRevision ? {
        stageAllTargetRevision: request.expectedStageAllTargetRevision,
        candidateTreeOid: stageAllCandidateTreeOid
      } : {})
    }));
    git.getGitBranches.mockResolvedValue({
      defaultBranch: "main",
      branches: [{
        name: "feature/review",
        kind: "local",
        current: true,
        head: "12ab34cd",
        upstream: "origin/feature/review",
        ahead: 1,
        behind: 0
      }, {
        name: "main",
        kind: "local",
        current: false,
        head: "98fe76dc",
        upstream: "origin/main",
        ahead: 0,
        behind: 0,
        merged: true
      }]
    });
    git.getGitHistory.mockResolvedValue({
      nextCursor: null,
      commits: [{
        oid: "12ab34cd",
        shortOid: "12ab34cd",
        subject: "Add Git review",
        authorName: "Cat",
        authoredAt: "2026-07-24T00:00:00Z",
        parents: []
      }]
    });
    git.executeGitAction.mockResolvedValue({ snapshot });
    git.getGitWorkspaceSummary.mockResolvedValue({
      kind: "unchanged",
      revision: snapshot.contentRevision
    });
    git.getGitChangePage.mockImplementation((_, request) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: snapshot.files,
      matchedCount: snapshot.files.length,
      nextCursor: null,
      selection: request.selectedPath
        ? { state: "present", file: snapshot.files[0] }
        : null,
      ...(request.expectedStageAllTargetRevision ? {
        stageAllTargetRevision: request.expectedStageAllTargetRevision,
        candidateTreeOid: stageAllCandidateTreeOid
      } : {})
    }));
    git.prepareGitDiscard.mockResolvedValue({
      snapshot,
      targetRevision: "target-revision-1"
    });
    git.prepareGitCommit.mockResolvedValue({
      snapshot,
      targetRevision: "commit-target-revision-1",
      candidateTreeOid: commitCandidateTreeOid,
      messageDigest: "commit-message-digest-1"
    } satisfies GitCommitPreparation);
    git.prepareGitStageAll.mockResolvedValue({
      snapshot: stageAllSnapshot,
      targetRevision: "stage-all-target-revision-1",
      candidateTreeOid: stageAllCandidateTreeOid
    });
    git.getGitHubRepository.mockResolvedValue(repository);
    git.getGitHubPullRequests.mockResolvedValue({
      pullRequests: [pullRequest],
      page: 1,
      pageSize: 30,
      hasMore: false,
      nextPage: null
    });
    git.getGitHubPullRequestDetail.mockResolvedValue(pullRequestDetail);
    git.getGitHubPullRequestReadiness.mockResolvedValue(pullRequestReadiness);
    git.getGitHubPullRequestDiff.mockResolvedValue({
      headRefOid: pullRequestDetail.headRefOid,
      patch: "@@ -1 +1 @@\n-old\n+new",
      path: "src/App.tsx",
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: []
    });
    git.getGitHubPullRequestReviewThreads.mockResolvedValue({
      number: pullRequest.number,
      headRefOid: pullRequestDetail.headRefOid,
      threads: [],
      totalCount: 0,
      nextCursor: null
    });
    git.getGitHubPullRequestReviewThreadComments.mockResolvedValue({
      number: pullRequest.number,
      headRefOid: pullRequestDetail.headRefOid,
      threadId: reviewThread.id,
      comments: [],
      totalCount: reviewThread.commentsTotalCount,
      nextCursor: null
    });
    git.executeGitHubAction.mockResolvedValue({
      repository,
      pullRequest: pullRequestDetail,
      snapshot
    });
  });

  it("keeps mounted inactive pages idle and loads the selected diff when activated", async () => {
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active={false}
      />
    );

    expect(screen.getByRole("tab", { name: "变更" })).toHaveAttribute("aria-label", "变更");
    expect(git.getGitDiff).not.toHaveBeenCalled();

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );

    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "working", path: "src/App.tsx" }
    ));
  });

  it("groups the change composition without duplicating partially staged files", () => {
    const compositionSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      additions: 11,
      deletions: 4,
      staged: 3,
      unstaged: 4,
      untracked: 1,
      conflicted: 1,
      files: [{
        path: "src/conflicted.ts",
        status: "unmerged",
        staged: true,
        unstaged: true,
        conflicted: true
      }, {
        path: "src/partial.ts",
        status: "modified",
        staged: true,
        unstaged: true,
        additions: 3,
        deletions: 1
      }, {
        path: "src/staged.ts",
        status: "modified",
        staged: true,
        unstaged: false
      }, {
        path: "src/unstaged.ts",
        status: "modified",
        staged: false,
        unstaged: true
      }, {
        path: "src/new.ts",
        status: "untracked",
        staged: false,
        unstaged: true,
        untracked: true
      }]
    };

    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-composition")}
        snapshot={compositionSnapshot}
        active={false}
      />
    );

    expect(within(screen.getByRole("group", { name: "冲突" }))
      .getByText("src/conflicted.ts")).toBeInTheDocument();
    const partialGroup = screen.getByRole("group", { name: "部分暂存" });
    expect(within(partialGroup).getByText("索引 + 工作区")).toBeInTheDocument();
    expect(within(partialGroup).getByText("src/partial.ts")).toBeInTheDocument();
    expect(within(partialGroup).getByText("已暂存 + 未暂存")).toBeInTheDocument();
    expect(screen.getAllByText("src/partial.ts")).toHaveLength(1);
    expect(within(screen.getByRole("group", { name: "已暂存" }))
      .getByText("src/staged.ts")).toBeInTheDocument();
    expect(within(screen.getByRole("group", { name: "未暂存" }))
      .getByText("src/unstaged.ts")).toBeInTheDocument();
    expect(within(screen.getByRole("group", { name: "未跟踪" }))
      .getByText("src/new.ts")).toBeInTheDocument();

    expect(within(partialGroup).getByRole("button", { name: "暂存 src/partial.ts" }))
      .toBeEnabled();
    expect(within(partialGroup).getByRole("button", { name: "取消暂存 src/partial.ts" }))
      .toBeEnabled();
    expect(within(partialGroup).getByRole("button", {
      name: "丢弃 src/partial.ts 的未暂存更改"
    })).toBeEnabled();
  });

  it("reloads an open diff when content changes even if all line counts stay equal", async () => {
    const target = gitConversationTarget("conversation-1");
    const view = render(
      <GitReviewPanel
        target={target}
        snapshot={snapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(1));

    const changedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      contentRevision: "revision-2",
      files: snapshot.files.map((file) => ({ ...file }))
    };
    view.rerender(
      <GitReviewPanel
        target={target}
        snapshot={changedSnapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(2));

    view.rerender(
      <GitReviewPanel
        target={target}
        snapshot={{
          ...changedSnapshot,
          files: changedSnapshot.files.map((file) => ({ ...file }))
        }}
        active
      />
    );
    await act(async () => Promise.resolve());
    expect(git.getGitDiff).toHaveBeenCalledTimes(2);
  });

  it("loads changed files from the backend in 200-row pages", async () => {
    const user = userEvent.setup();
    const paths = Array.from(
      { length: 450 },
      (_, index) => `src/bulk/file-${String(index).padStart(3, "0")}.ts`
    );
    const { snapshot: largeSnapshot, files } = createPagedChangesSnapshot(paths);
    git.getGitChangePage
      .mockResolvedValueOnce({
        kind: "page",
        revision: "summary-large",
        files: files.slice(0, 200),
        matchedCount: 450,
        nextCursor: "page-2",
        selection: null
      })
      .mockResolvedValueOnce({
        kind: "page",
        revision: "summary-large",
        files: files.slice(200, 400),
        matchedCount: 450,
        nextCursor: "page-3",
        selection: { state: "present", file: files[0] }
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-large-changes")}
        snapshot={largeSnapshot}
        active
      />
    );

    const fileList = screen.getByRole("navigation", { name: "变更文件" });
    await waitFor(() => expect(fileList.querySelectorAll(".git-review__file-row")).toHaveLength(200));
    expect(within(fileList).getByRole("status")).toHaveTextContent("显示 200/450 个变更文件");
    expect(within(fileList).getByText(paths[199])).toBeInTheDocument();
    expect(within(fileList).queryByText(paths[200])).not.toBeInTheDocument();
    expect(git.getGitChangePage).toHaveBeenNthCalledWith(1, gitConversationTarget("conversation-large-changes"), {
      expectedRevision: "summary-large",
      limit: 200
    });

    await user.click(within(fileList).getByRole("button", { name: "再显示 200 个文件" }));

    await waitFor(() => expect(fileList.querySelectorAll(".git-review__file-row")).toHaveLength(400));
    expect(within(fileList).getByRole("status")).toHaveTextContent("显示 400/450 个变更文件");
    expect(within(fileList).getByText(paths[399])).toBeInTheDocument();
    expect(within(fileList).queryByText(paths[400])).not.toBeInTheDocument();
    expect(git.getGitChangePage).toHaveBeenNthCalledWith(2, gitConversationTarget("conversation-large-changes"), {
      expectedRevision: "summary-large",
      cursor: "page-2",
      limit: 200,
      selectedPath: paths[0]
    });
    expect(git.executeGitAction).not.toHaveBeenCalled();
  }, 10_000);

  it("resets backend pagination when the path filter changes", async () => {
    const user = userEvent.setup();
    const matchingPaths = [
      "src/MiXeDCase/file-000.ts",
      "src/MiXeDCase/file-001.ts"
    ];
    const otherPaths = ["src/other/file-002.ts"];
    const { snapshot: largeSnapshot, files } = createPagedChangesSnapshot(
      [...matchingPaths, ...otherPaths],
      "summary-filter"
    );
    git.getGitChangePage.mockImplementation(async (
      _conversationId: string,
      request: { cursor?: string; query?: string; selectedPath?: string }
    ) => {
      if (request.query === "mixedcase") {
        return {
          kind: "page",
          revision: "summary-filter",
          files: files.slice(0, 2),
          matchedCount: 2,
          nextCursor: null,
          selection: request.selectedPath
            ? { state: "filteredOut" as const }
            : null
        };
      }
      const offset = request.cursor ? 2 : 0;
      return {
        kind: "page",
        revision: "summary-filter",
        files: files.slice(offset, offset + 2),
        matchedCount: 3,
        nextCursor: offset === 0 ? "all-page-2" : null,
        selection: request.selectedPath
          ? { state: "present" as const, file: files.find((file) => file.path === request.selectedPath)! }
          : null
      };
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-filter-changes")}
        snapshot={largeSnapshot}
        active
      />
    );

    const fileList = screen.getByRole("navigation", { name: "变更文件" });
    await waitFor(() => expect(fileList.querySelectorAll(".git-review__file-row")).toHaveLength(2));
    await user.click(within(fileList).getByRole("button", { name: "再显示 1 个文件" }));
    await waitFor(() => expect(fileList.querySelectorAll(".git-review__file-row")).toHaveLength(3));
    const previouslySelectedFile = within(fileList).getByText(otherPaths[0]).closest("button");
    expect(previouslySelectedFile).not.toBeNull();
    await user.click(previouslySelectedFile!);
    expect(previouslySelectedFile).toHaveAttribute("aria-pressed", "true");

    fireEvent.change(
      within(fileList).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "MiXeDCase" } }
    );

    await waitFor(() => {
      expect(fileList.querySelectorAll(".git-review__file-row")).toHaveLength(2);
      expect(within(fileList).getByRole("status")).toHaveTextContent("显示 2/2 个变更文件");
    });
    expect(within(fileList).getByText(matchingPaths[1])).toBeInTheDocument();
    expect(within(fileList).queryByText(otherPaths[0])).not.toBeInTheDocument();
    expect(git.getGitChangePage).toHaveBeenLastCalledWith(
      gitConversationTarget("conversation-filter-changes"),
      {
        expectedRevision: "summary-filter",
        query: "mixedcase",
        limit: 200,
        selectedPath: otherPaths[0]
      }
    );
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("ignores a late change page from an older summary revision", async () => {
    const old = createPagedChangesSnapshot(["src/old.ts"], "summary-old");
    const next = createPagedChangesSnapshot(["src/new.ts"], "summary-new");
    let resolveOldPage!: (value: {
      kind: "page";
      revision: string;
      files: typeof old.files;
      matchedCount: number;
      nextCursor: null;
      selection: null;
    }) => void;
    git.getGitChangePage.mockImplementation((
      _conversationId: string,
      request: { expectedRevision: string }
    ) => {
      if (request.expectedRevision === "summary-old") {
        return new Promise((resolve) => {
          resolveOldPage = resolve;
        });
      }
      return Promise.resolve({
        kind: "page",
        revision: "summary-new",
        files: next.files,
        matchedCount: 1,
        nextCursor: null,
        selection: null
      });
    });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-page-race")}
        snapshot={old.snapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitChangePage).toHaveBeenCalledWith(
      gitConversationTarget("conversation-page-race"),
      { expectedRevision: "summary-old", limit: 200 }
    ));

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-page-race")}
        snapshot={next.snapshot}
        active
      />
    );
    await waitFor(() => expect(screen.getAllByText("src/new.ts").length).toBeGreaterThan(0));

    await act(async () => {
      resolveOldPage({
        kind: "page",
        revision: "summary-old",
        files: old.files,
        matchedCount: 1,
        nextCursor: null,
        selection: null
      });
      await Promise.resolve();
    });

    expect(screen.getAllByText("src/new.ts").length).toBeGreaterThan(0);
    expect(screen.queryByText("src/old.ts")).not.toBeInTheDocument();
  });

  it("uses present, filtered-out, and missing selection states distinctly", async () => {
    const first = createPagedChangesSnapshot(
      ["src/match/a.ts", "src/match/b.ts", "src/other/c.ts"],
      "selection-v1"
    );
    git.getGitChangePage.mockImplementation(async (
      _conversationId: string,
      request: { expectedRevision: string; query?: string; selectedPath?: string }
    ) => {
      if (request.expectedRevision === "selection-v2") {
        return {
          kind: "page",
          revision: "selection-v2",
          files: [first.files[2]],
          matchedCount: 1,
          nextCursor: null,
          selection: { state: "missing" as const }
        };
      }
      if (request.query === "match") {
        return {
          kind: "page",
          revision: "selection-v1",
          files: [first.files[1]],
          matchedCount: 2,
          nextCursor: null,
          selection: {
            state: "present" as const,
            file: first.files[0]
          }
        };
      }
      if (request.query === "other") {
        return {
          kind: "page",
          revision: "selection-v1",
          files: [first.files[2]],
          matchedCount: 1,
          nextCursor: null,
          selection: { state: "filteredOut" as const }
        };
      }
      return {
        kind: "page",
        revision: "selection-v1",
        files: first.files.slice(0, 2),
        matchedCount: 3,
        nextCursor: null,
        selection: null
      };
    });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-selection")}
        snapshot={first.snapshot}
        active
      />
    );
    const fileList = screen.getByRole("navigation", { name: "变更文件" });
    await waitFor(() => expect(within(fileList).getByText("src/match/a.ts").closest("button"))
      .toHaveAttribute("aria-pressed", "true"));

    fireEvent.change(
      within(fileList).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "match" } }
    );
    await waitFor(() => {
      expect(within(fileList).getByText("src/match/b.ts")).toBeInTheDocument();
      expect(screen.getByRole("article", { name: "src/match/a.ts 差异" })).toBeInTheDocument();
    });

    fireEvent.change(
      within(fileList).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "other" } }
    );
    await waitFor(() => expect(within(fileList).getByText("src/other/c.ts").closest("button"))
      .toHaveAttribute("aria-pressed", "false"));
    expect(screen.queryByRole("article")).not.toBeInTheDocument();

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-selection")}
        snapshot={{
          ...first.snapshot,
          summaryRevision: "selection-v2",
          contentRevision: "selection-content-v2",
          changedFiles: 1
        }}
        active
      />
    );
    await waitFor(() => expect(within(fileList).getByText("src/other/c.ts").closest("button"))
      .toHaveAttribute("aria-pressed", "true"));
    expect(screen.getByRole("article", { name: "src/other/c.ts 差异" })).toBeInTheDocument();
  });

  it("publishes a stale page summary and reloads against its new revision", async () => {
    const old = createPagedChangesSnapshot(["src/old.ts"], "stale-v1");
    const next = createPagedChangesSnapshot(["src/fresh.ts"], "stale-v2");
    const { files: _files, filesComplete: _filesComplete, ...nextSummary } = next.snapshot;
    const onSnapshotChange = vi.fn();
    git.getGitChangePage
      .mockResolvedValueOnce({
        kind: "stale",
        summary: nextSummary
      })
      .mockResolvedValueOnce({
        kind: "page",
        revision: "stale-v2",
        files: next.files,
        matchedCount: 1,
        nextCursor: null,
        selection: null
      });

    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stale-page")}
        snapshot={old.snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await waitFor(() => expect(screen.getAllByText("src/fresh.ts").length).toBeGreaterThan(0));
    expect(onSnapshotChange).toHaveBeenCalledWith(expect.objectContaining({
      summaryRevision: "stale-v2",
      files: [],
      filesComplete: false
    }));
    expect(git.getGitChangePage).toHaveBeenNthCalledWith(2, gitConversationTarget("conversation-stale-page"), {
      expectedRevision: "stale-v2",
      limit: 200
    });
  });

  it("uses revision-bound bulk actions instead of sending an unbounded path list", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-bulk")}
        snapshot={{
          ...snapshot,
          staged: 12_001,
          unstaged: 12_001,
          changedFiles: 24_002,
          stageable: 12_001,
          unstageable: 12_001,
          filesComplete: true
        }}
        active
      />
    );

    expect(screen.getByText("24002 个文件")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    await waitFor(() => expect(screen.getByRole("button", {
      name: "确认全部暂存"
    })).toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "确认全部暂存" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-bulk"),
      {
        type: "stage_all",
        expectedContentRevision: "revision-1",
        expectedTargetRevision: "stage-all-target-revision-1"
      }
    ));
    expect(git.prepareGitStageAll).toHaveBeenCalledTimes(2);

    await user.click(screen.getByRole("button", { name: "全部取消" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-bulk"),
      { type: "unstage_all", expectedContentRevision: "revision-1" }
    ));
  });

  it("replaces a live diff with the exact immutable stage-all candidate before confirmation", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    let resolveCandidatePage!: (value: {
      kind: "page";
      revision: string;
      files: GitWorkspaceSnapshot["files"];
      matchedCount: number;
      nextCursor: null;
      selection: { state: "present"; file: GitWorkspaceSnapshot["files"][number] };
      stageAllTargetRevision: string;
      candidateTreeOid: string;
    }) => void;
    let resolveCandidateDiff!: (value: GitDiffResult) => void;
    git.getGitDiff
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-old\n+A-visible-before-prepare",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      })
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveCandidateDiff = resolve;
      }));
    git.getGitChangePage.mockReturnValueOnce(new Promise((resolve) => {
      resolveCandidatePage = resolve;
    }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-review")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await waitFor(() => expect(view.container).toHaveTextContent("A-visible-before-prepare"));

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    await waitFor(() => expect(git.getGitChangePage).toHaveBeenCalledWith(
      gitConversationTarget("conversation-stage-review"),
      expect.objectContaining({
        expectedRevision: "stage-all-summary-revision-1",
        expectedStageAllTargetRevision: "stage-all-target-revision-1",
        limit: 200
      })
    ));
    expect(onSnapshotChange).toHaveBeenCalledWith(expect.objectContaining({
      files: [],
      filesComplete: false,
      summaryRevision: "stage-all-summary-revision-1"
    }));
    expect(view.container).not.toHaveTextContent("A-visible-before-prepare");
    expect(screen.getByRole("button", {
      name: "校验候选差异"
    })).toBeDisabled();

    await act(async () => {
      resolveCandidatePage({
        kind: "page",
        revision: "stage-all-summary-revision-1",
        files: snapshot.files,
        matchedCount: snapshot.files.length,
        nextCursor: null,
        selection: { state: "present", file: snapshot.files[0] },
        stageAllTargetRevision: "stage-all-target-revision-1",
        candidateTreeOid: stageAllCandidateTreeOid
      });
    });
    await waitFor(() => expect(git.getGitDiff).toHaveBeenLastCalledWith(
      gitConversationTarget("conversation-stage-review"),
      {
        type: "working",
        path: "src/App.tsx",
        expectedStageAllTargetRevision: "stage-all-target-revision-1"
      }
    ));
    expect(screen.getByRole("button", {
      name: "校验候选差异"
    })).toBeDisabled();

    await act(async () => {
      resolveCandidateDiff({
        patch: "@@ -1 +1 @@\n-old\n+B-from-immutable-candidate",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: [],
        stageAllTargetRevision: "stage-all-target-revision-1",
        candidateTreeOid: stageAllCandidateTreeOid
      });
    });
    expect(await screen.findByRole("button", {
      name: "确认全部暂存"
    })).toBeEnabled();
    expect(view.container).toHaveTextContent("B-from-immutable-candidate");

    await user.click(screen.getByRole("button", { name: "确认全部暂存" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-stage-review"),
      {
        type: "stage_all",
        expectedContentRevision: "revision-1",
        expectedTargetRevision: "stage-all-target-revision-1"
      }
    ));
  });

  it("keeps a paginated stage-all candidate proof-bound while more files load", async () => {
    const user = userEvent.setup();
    const secondFile = {
      ...snapshot.files[0],
      path: "src/Other.tsx"
    };
    git.prepareGitStageAll.mockResolvedValue({
      snapshot: {
        ...stageAllSnapshot,
        changedFiles: 2,
        stageable: 2
      },
      targetRevision: "stage-all-target-revision-1",
      candidateTreeOid: stageAllCandidateTreeOid
    });
    git.getGitChangePage.mockImplementation((_, request) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: request.cursor ? [secondFile] : snapshot.files,
      matchedCount: 2,
      nextCursor: request.cursor ? null : "candidate-page-2",
      selection: {
        state: "present",
        file: snapshot.files[0]
      },
      stageAllTargetRevision: request.expectedStageAllTargetRevision,
      candidateTreeOid: stageAllCandidateTreeOid
    }));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-pages")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    expect(await screen.findByRole("button", {
      name: "确认全部暂存"
    })).toBeEnabled();
    expect(await screen.findByRole("button", {
      name: "再显示 1 个文件"
    })).toBeEnabled();

    await user.click(screen.getByRole("button", {
      name: "再显示 1 个文件"
    }));
    expect(await screen.findByText("src/Other.tsx")).toBeInTheDocument();
    expect(await screen.findByRole("button", {
      name: "确认全部暂存"
    })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "全部取消" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "取消候选" })).toBeEnabled();
    expect(screen.queryByRole("button", {
      name: "暂存 src/App.tsx"
    })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", {
      name: "取消暂存 src/App.tsx"
    })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "获取" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-stage-pages"),
      {
        type: "fetch",
        expectedRepositoryId: snapshot.repositoryId,
        expectedWorktreeId: snapshot.worktreeId,
        remote: originRemote
      }
    ));
    expect(screen.queryByRole("button", {
      name: "确认全部暂存"
    })).not.toBeInTheDocument();
  });

  it("cancels a reviewed stage-all candidate without mutating the repository", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-cancel")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    expect(await screen.findByRole("button", {
      name: "确认全部暂存"
    })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "取消候选" }));

    expect(screen.queryByRole("button", {
      name: "确认全部暂存"
    })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全部暂存" })).toBeEnabled();
    expect(screen.getByText("已取消全部暂存候选")).toBeInTheDocument();
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("disarms stage all when a candidate diff does not echo the exact proof", async () => {
    const user = userEvent.setup();
    git.getGitDiff
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-old\n+live",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      })
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-old\n+wrong-candidate",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: [],
        stageAllTargetRevision: "different-target",
        candidateTreeOid: stageAllCandidateTreeOid
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-wrong-diff")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    expect(await screen.findByText(/全部暂存证明已失效/)).toBeInTheDocument();
    expect(screen.queryByRole("button", {
      name: "确认全部暂存"
    })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全部暂存" })).toBeEnabled();
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("re-arms stage all when the explicit target proof changes", async () => {
    const user = userEvent.setup();
    const preparation = (targetRevision: string): GitStageAllPreparation => ({
      snapshot: stageAllSnapshot,
      targetRevision,
      candidateTreeOid: stageAllCandidateTreeOid
    });
    git.prepareGitStageAll
      .mockResolvedValueOnce(preparation("stage-target-1"))
      .mockResolvedValueOnce(preparation("stage-target-2"))
      .mockResolvedValueOnce(preparation("stage-target-2"));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-drift")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    await user.click(await screen.findByRole("button", { name: "确认全部暂存" }));
    await waitFor(() => expect(git.prepareGitStageAll).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(screen.getByText(/待暂存内容已变化/)).toBeInTheDocument();
    expect(await screen.findByRole("button", {
      name: "确认全部暂存"
    })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "确认全部暂存" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-stage-drift"),
      {
        type: "stage_all",
        expectedContentRevision: "revision-1",
        expectedTargetRevision: "stage-target-2"
      }
    ));
    expect(git.prepareGitStageAll).toHaveBeenCalledTimes(3);
  });

  it("does not publish or arm a stage-all proof from an old conversation", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    let resolveOldPreparation!: (value: GitStageAllPreparation) => void;
    git.prepareGitStageAll.mockReturnValueOnce(new Promise((resolve) => {
      resolveOldPreparation = resolve;
    }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-old")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", { name: "全部暂存" }));
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stage-current")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await act(async () => {
      resolveOldPreparation({
        snapshot: stageAllSnapshot,
        targetRevision: "stage-target-old",
        candidateTreeOid: stageAllCandidateTreeOid
      });
    });

    expect(onSnapshotChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "确认全部暂存" })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", {
      name: "全部暂存"
    })).toBeEnabled());
  });

  it("stages a file through a tagged backend action and publishes its snapshot", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const onMutationStart = vi.fn(() => true);
    const onMutationEnd = vi.fn();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
        onMutationStart={onMutationStart}
        onMutationEnd={onMutationEnd}
      />
    );

    await user.click(screen.getByRole("button", { name: "暂存 src/App.tsx" }));

    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(onSnapshotChange).toHaveBeenCalledWith(snapshot);
    expect(onMutationStart).toHaveBeenCalledTimes(1);
    expect(onMutationEnd).toHaveBeenCalledTimes(1);
  });

  it("does not let a diff read from before a mutation overwrite the refreshed diff", async () => {
    const user = userEvent.setup();
    let resolveStaleDiff!: (value: GitDiffResult) => void;
    git.getGitDiff
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveStaleDiff = resolve;
      }))
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-before\n+fresh-after-mutation",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole("button", { name: "暂存 src/App.tsx" }));
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(view.container).toHaveTextContent("fresh-after-mutation"));

    await act(async () => {
      resolveStaleDiff({
        patch: "@@ -1 +1 @@\n-before\n+stale-before-mutation",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      });
      await Promise.resolve();
    });

    expect(view.container).toHaveTextContent("fresh-after-mutation");
    expect(view.container).not.toHaveTextContent("stale-before-mutation");
  });

  it("surfaces concise backend feedback for repository-level operations", async () => {
    const user = userEvent.setup();
    git.executeGitAction.mockResolvedValueOnce({
      snapshot,
      message: "Everything up-to-date"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "获取" }));
    expect(await screen.findByText("Everything up-to-date")).toBeInTheDocument();
  });

  it("requires a proof-bound second confirmation before pushing", async () => {
    const user = userEvent.setup();
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-push")}
        snapshot={snapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "推送" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(screen.getByRole("button", {
      name: "确认推送 origin/feature/review · 12ab34cd"
    })).toHaveClass("git-review__confirm--armed");

    const movedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      head: "98fe76dc"
    };
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-push")}
        snapshot={movedSnapshot}
        active
      />
    );
    expect(screen.getByRole("button", { name: "推送" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "推送" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", {
      name: "确认推送 origin/feature/review · 98fe76dc"
    }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-push"),
      {
        type: "push",
        expectedRepositoryId: snapshot.repositoryId,
        expectedWorktreeId: snapshot.worktreeId,
        remote: originRemote,
        expectedLocalBranch: "feature/review",
        remoteBranch: "feature/review",
        expectedHead: "98fe76dc",
        expectedUpstream: snapshot.upstreamTarget,
        setUpstream: false
      }
    ));
  });

  it("defaults to the real upstream remote and pulls its structured branch proof", async () => {
    const user = userEvent.setup();
    const upstreamRemote: GitRemote = {
      name: "upstream",
      fetchRevision: "upstream-fetch-revision-1",
      pushRevision: "upstream-push-revision-1",
      url: "https://github.com/example/Mework.git"
    };
    const upstreamTarget = {
      remoteName: "upstream",
      remoteBranch: "stable",
      mergeRef: "refs/heads/stable",
      trackingRef: "refs/remotes/upstream/stable",
      trackingOid: "1111111111111111111111111111111111111111",
      isLocal: false,
      remote: upstreamRemote
    };
    const upstreamSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      upstream: "legacy/string/must-not-drive-pull",
      upstreamTarget,
      remote: originRemote,
      remotes: [originRemote, upstreamRemote]
    };
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-upstream")}
        snapshot={upstreamSnapshot}
        active
      />
    );

    expect(screen.getByRole("combobox", { name: "Git 远端" })).toHaveValue("upstream");
    expect(view.container).not.toHaveTextContent(originRemote.url!);
    expect(view.container).not.toHaveTextContent(upstreamRemote.url!);

    await user.click(screen.getByRole("button", { name: "拉取" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", {
      name: "确认拉取 upstream/stable"
    }));

    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-upstream"),
      {
        type: "pull",
        expectedRepositoryId: upstreamSnapshot.repositoryId,
        expectedWorktreeId: upstreamSnapshot.worktreeId,
        expectedLocalBranch: "feature/review",
        expectedHead: upstreamSnapshot.head,
        expectedContentRevision: upstreamSnapshot.contentRevision,
        upstream: upstreamTarget,
        ffOnly: true
      }
    ));
  });

  it("uses the selected remote proof for fetch and disarms push when selection changes", async () => {
    const user = userEvent.setup();
    const upstreamRemote: GitRemote = {
      name: "upstream",
      fetchRevision: "upstream-fetch-revision-1",
      pushRevision: "upstream-push-revision-1",
      url: "ssh://git@github.com/example/Mework.git"
    };
    const upstreamTarget = {
      remoteName: "upstream",
      remoteBranch: "review-target",
      mergeRef: "refs/heads/review-target",
      trackingRef: "refs/remotes/upstream/review-target",
      trackingOid: "2222222222222222222222222222222222222222",
      isLocal: false,
      remote: upstreamRemote
    };
    const multiRemoteSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      upstreamTarget,
      remote: originRemote,
      remotes: [originRemote, upstreamRemote]
    };
    git.executeGitAction.mockResolvedValue({ snapshot: multiRemoteSnapshot });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-remote-select")}
        snapshot={multiRemoteSnapshot}
        active
      />
    );

    const remoteSelect = screen.getByRole("combobox", { name: "Git 远端" });
    await user.selectOptions(remoteSelect, "origin");
    await user.click(screen.getByRole("button", { name: "获取" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-remote-select"),
      {
        type: "fetch",
        expectedRepositoryId: multiRemoteSnapshot.repositoryId,
        expectedWorktreeId: multiRemoteSnapshot.worktreeId,
        remote: originRemote
      }
    ));

    git.executeGitAction.mockClear();
    await user.selectOptions(remoteSelect, "upstream");
    await user.click(screen.getByRole("button", { name: "推送" }));
    expect(screen.getByRole("button", {
      name: "确认推送 upstream/review-target · 12ab34cd"
    })).toHaveClass("git-review__confirm--armed");

    await user.selectOptions(remoteSelect, "origin");
    expect(screen.getByRole("button", { name: "推送" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "推送" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", {
      name: "确认推送 origin/feature/review · 12ab34cd"
    }));

    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-remote-select"),
      {
        type: "push",
        expectedRepositoryId: multiRemoteSnapshot.repositoryId,
        expectedWorktreeId: multiRemoteSnapshot.worktreeId,
        remote: originRemote,
        expectedLocalBranch: "feature/review",
        remoteBranch: "feature/review",
        expectedHead: multiRemoteSnapshot.head,
        expectedUpstream: upstreamTarget,
        setUpstream: false
      }
    ));
  });

  it("disarms a pull confirmation when its remote or tracking proof drifts", async () => {
    const user = userEvent.setup();
    const upstreamRemote: GitRemote = {
      name: "upstream",
      fetchRevision: "upstream-fetch-revision-1",
      pushRevision: "upstream-push-revision-1",
      url: "https://github.com/example/Mework.git"
    };
    const upstreamSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      remote: originRemote,
      remotes: [originRemote, upstreamRemote],
      upstreamTarget: {
        remoteName: "upstream",
        remoteBranch: "stable",
        mergeRef: "refs/heads/stable",
        trackingRef: "refs/remotes/upstream/stable",
        trackingOid: "3333333333333333333333333333333333333333",
        isLocal: false,
        remote: upstreamRemote
      }
    };
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-pull-drift")}
        snapshot={upstreamSnapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "拉取" }));
    expect(screen.getByRole("button", {
      name: "确认拉取 upstream/stable"
    })).toBeInTheDocument();

    const movedRemote: GitRemote = {
      ...upstreamRemote,
      fetchRevision: "upstream-fetch-revision-2"
    };
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-pull-drift")}
        snapshot={{
          ...upstreamSnapshot,
          remotes: [originRemote, movedRemote],
          upstreamTarget: {
            ...upstreamSnapshot.upstreamTarget!,
            trackingOid: "4444444444444444444444444444444444444444",
            remote: movedRemote
          }
        }}
        active
      />
    );

    expect(await screen.findByRole("button", { name: "拉取" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "拉取" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(screen.getByRole("button", {
      name: "确认拉取 upstream/stable"
    })).toBeInTheDocument();
  });

  it("labels the dot remote as local and disables every network action", () => {
    const localRemote: GitRemote = {
      name: ".",
      fetchRevision: "local-fetch-revision-1",
      pushRevision: "local-push-revision-1",
      url: null
    };
    const localSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      upstream: "main",
      upstreamTarget: {
        remoteName: ".",
        remoteBranch: "main",
        mergeRef: "refs/heads/main",
        trackingRef: "refs/heads/main",
        trackingOid: snapshot.head,
        isLocal: true,
        remote: localRemote
      },
      remote: localRemote,
      remotes: [localRemote]
    };
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-local-remote")}
        snapshot={localSnapshot}
        active
      />
    );

    expect(screen.getByRole("combobox", { name: "Git 远端" })).toHaveValue(".");
    expect(screen.getByText(
      "“.” 是本地仓库；获取、拉取和推送不可用。"
    )).toHaveAttribute("role", "status");
    expect(screen.getByRole("button", { name: "获取" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "拉取" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "推送" })).toBeDisabled();
  });

  it("does not let a late network response overwrite a newer same-worktree remote scope", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    let resolveFetch!: (value: { snapshot: GitWorkspaceSnapshot }) => void;
    git.executeGitAction.mockReturnValueOnce(new Promise((resolve) => {
      resolveFetch = resolve;
    }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-remote-race")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", { name: "获取" }));
    const movedRemote: GitRemote = {
      ...originRemote,
      fetchRevision: "origin-fetch-revision-2"
    };
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-remote-race")}
        snapshot={{
          ...snapshot,
          remote: movedRemote,
          remotes: [movedRemote],
          upstreamTarget: {
            ...snapshot.upstreamTarget!,
            remote: movedRemote
          }
        }}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await act(async () => {
      resolveFetch({
        snapshot: {
          ...snapshot,
          branch: "stale/network-response"
        }
      });
      await Promise.resolve();
    });

    expect(onSnapshotChange).not.toHaveBeenCalled();
    expect(view.container).not.toHaveTextContent("stale/network-response");
  });

  it("refreshes the summary after a partial network failure without reporting success", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const refreshedRemote: GitRemote = {
      ...originRemote,
      fetchRevision: "origin-fetch-after-partial-failure"
    };
    const refreshedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      remote: refreshedRemote,
      remotes: [refreshedRemote],
      upstreamTarget: {
        ...snapshot.upstreamTarget!,
        remote: refreshedRemote
      },
      summaryRevision: "summary-after-partial-failure",
      changedFiles: snapshot.files.length,
      stageable: 1,
      unstageable: 1,
      filesComplete: false
    };
    const { files: _files, ...refreshedSummary } = refreshedSnapshot;
    git.executeGitAction.mockRejectedValueOnce(new Error("fetch updated refs before failing"));
    git.getGitWorkspaceSummary.mockResolvedValueOnce({
      kind: "snapshot",
      summary: refreshedSummary
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-partial-network-error")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", { name: "获取" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "fetch updated refs before failing"
    );
    await waitFor(() => expect(onSnapshotChange).toHaveBeenCalledWith(
      expect.objectContaining({
        remote: refreshedRemote,
        files: []
      })
    ));
    expect(screen.queryByText("Git 操作已完成")).not.toBeInTheDocument();
  });

  it("states clearly that discarding an untracked file permanently deletes it", async () => {
    const user = userEvent.setup();
    const untrackedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 0,
      unstaged: 1,
      untracked: 1,
      files: [{
        path: "draft.txt",
        status: "untracked",
        staged: false,
        unstaged: true,
        untracked: true,
        conflicted: false,
        additions: 1,
        deletions: 0,
        binary: false
      }]
    };
    git.executeGitAction.mockResolvedValueOnce({ snapshot: untrackedSnapshot });
    git.prepareGitDiscard.mockResolvedValue({
      snapshot: untrackedSnapshot,
      targetRevision: "target-revision-1"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={untrackedSnapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", {
      name: "永久删除未跟踪文件 draft.txt"
    }));
    await user.click(await screen.findByRole("button", {
      name: "确认永久删除未跟踪文件 draft.txt"
    }));
    expect(git.prepareGitDiscard).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      ["draft.txt"],
      true
    );
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "discard",
        paths: ["draft.txt"],
        includeUntracked: true,
        expectedContentRevision: "revision-1",
        expectedTargetRevision: "target-revision-1"
      }
    ));
  });

  it("requires a new discard confirmation after the repository content revision changes", async () => {
    const user = userEvent.setup();
    const changedFile = {
      path: "src/App.tsx",
      status: "modified" as const,
      staged: false,
      unstaged: true,
      additions: 7,
      deletions: 2
    };
    git.prepareGitDiscard
      .mockResolvedValueOnce({
        snapshot: { ...snapshot, staged: 0, files: [changedFile] },
        targetRevision: "target-revision-1"
      })
      .mockResolvedValueOnce({
        snapshot: {
          ...snapshot,
          contentRevision: "revision-2",
          staged: 0,
          files: [changedFile]
        },
        targetRevision: "target-revision-2"
      })
      .mockResolvedValueOnce({
        snapshot: {
          ...snapshot,
          contentRevision: "revision-2",
          staged: 0,
          files: [changedFile]
        },
        targetRevision: "target-revision-2"
      });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={{ ...snapshot, staged: 0, files: [changedFile] }}
        active
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    expect(await screen.findByRole("button", { name: "确认丢弃 src/App.tsx" })).toBeInTheDocument();

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={{
          ...snapshot,
          contentRevision: "revision-2",
          staged: 0,
          files: [changedFile]
        }}
        active
      />
    );
    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    await waitFor(() => expect(git.prepareGitDiscard).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();

    await user.click(await screen.findByRole("button", { name: "确认丢弃 src/App.tsx" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "discard",
        paths: ["src/App.tsx"],
        includeUntracked: false,
        expectedContentRevision: "revision-2",
        expectedTargetRevision: "target-revision-2"
      }
    ));
  });

  it("requires another confirmation when only the discard target revision changes", async () => {
    const user = userEvent.setup();
    const changedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 0,
      files: [{
        ...snapshot.files[0],
        staged: false,
        unstaged: true
      }]
    };
    git.prepareGitDiscard
      .mockResolvedValueOnce({
        snapshot: changedSnapshot,
        targetRevision: "target-revision-1"
      })
      .mockResolvedValueOnce({
        snapshot: changedSnapshot,
        targetRevision: "target-revision-2"
      })
      .mockResolvedValueOnce({
        snapshot: changedSnapshot,
        targetRevision: "target-revision-2"
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={changedSnapshot}
        active
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    await user.click(await screen.findByRole("button", {
      name: "确认丢弃 src/App.tsx"
    }));

    await waitFor(() => expect(git.prepareGitDiscard).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(screen.getByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();

    await user.click(screen.getByRole("button", {
      name: "确认丢弃 src/App.tsx"
    }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "discard",
        paths: ["src/App.tsx"],
        includeUntracked: false,
        expectedContentRevision: "revision-1",
        expectedTargetRevision: "target-revision-2"
      }
    ));
    expect(git.prepareGitDiscard).toHaveBeenCalledTimes(3);
  });

  it("ignores out-of-order discard preparations after the repository changes", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const repositoryOneSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      repositoryRoot: "C:/repo-one",
      worktreeRoot: "C:/repo-one"
    };
    const repositoryTwoSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      contentRevision: "revision-repo-two",
      repositoryRoot: "C:/repo-two",
      worktreeRoot: "C:/repo-two"
    };
    let resolveOldPreparation!: (value: GitDiscardPreparation) => void;
    let resolveNewPreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveOldPreparation = resolve;
      }))
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveNewPreparation = resolve;
      }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={repositoryOneSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={repositoryTwoSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await waitFor(() => expect(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    })).toBeEnabled());
    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));

    await act(async () => {
      resolveNewPreparation({
        snapshot: repositoryTwoSnapshot,
        targetRevision: "target-repo-two"
      });
    });
    expect(await screen.findByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();

    await act(async () => {
      resolveOldPreparation({
        snapshot: repositoryOneSnapshot,
        targetRevision: "target-repo-one"
      });
    });
    expect(onSnapshotChange).toHaveBeenCalledTimes(1);
    expect(onSnapshotChange).toHaveBeenCalledWith(repositoryTwoSnapshot);
    expect(screen.getByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();
  });

  it("does not publish or arm a discard preparation from an old conversation", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    let resolveOldPreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard.mockReturnValueOnce(new Promise((resolve) => {
      resolveOldPreparation = resolve;
    }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-2")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await act(async () => {
      resolveOldPreparation({
        snapshot,
        targetRevision: "target-conversation-one"
      });
    });

    expect(onSnapshotChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    expect(git.prepareGitDiscard).toHaveBeenLastCalledWith(
      gitConversationTarget("conversation-2"),
      ["src/App.tsx"],
      false
    );
  });

  it("does not publish or arm a discard preparation after the panel becomes inactive", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    let resolvePreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard.mockReturnValueOnce(new Promise((resolve) => {
      resolvePreparation = resolve;
    }));
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active={false}
        onSnapshotChange={onSnapshotChange}
      />
    );
    await act(async () => {
      resolvePreparation({
        snapshot,
        targetRevision: "target-inactive"
      });
    });

    expect(onSnapshotChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).not.toBeInTheDocument();
  });

  it("invalidates a deferred discard preparation when the same repository snapshot changes", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const repositorySnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      repositoryRoot: "C:/repo",
      worktreeRoot: "C:/repo"
    };
    const externalSnapshot: GitWorkspaceSnapshot = {
      ...repositorySnapshot,
      contentRevision: "revision-external"
    };
    let resolveOldPreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveOldPreparation = resolve;
      }))
      .mockResolvedValueOnce({
        snapshot: externalSnapshot,
        targetRevision: "target-external"
      });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={repositorySnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={externalSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await act(async () => {
      resolveOldPreparation({
        snapshot: repositorySnapshot,
        targetRevision: "target-before-external-change"
      });
    });

    expect(onSnapshotChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    expect(await screen.findByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();
    expect(onSnapshotChange).toHaveBeenCalledTimes(1);
    expect(onSnapshotChange).toHaveBeenCalledWith(externalSnapshot);
  });

  it("preserves an armed discard when the parent echoes its accepted preparation snapshot", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const preparedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      contentRevision: "revision-prepared"
    };
    git.prepareGitDiscard.mockResolvedValue({
      snapshot: preparedSnapshot,
      targetRevision: "target-prepared"
    });
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(screen.getByRole("button", {
      name: "丢弃 src/App.tsx 的未暂存更改"
    }));
    expect(await screen.findByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();
    expect(onSnapshotChange).toHaveBeenCalledWith(preparedSnapshot);

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={preparedSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    expect(screen.getByRole("button", {
      name: "确认丢弃 src/App.tsx"
    })).toBeInTheDocument();

    await user.click(screen.getByRole("button", {
      name: "确认丢弃 src/App.tsx"
    }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "discard",
        paths: ["src/App.tsx"],
        includeUntracked: false,
        expectedContentRevision: "revision-prepared",
        expectedTargetRevision: "target-prepared"
      }
    ));
  });

  it("keeps nested submodule changes visible without offering no-op parent actions", () => {
    const submoduleSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 0,
      unstaged: 1,
      files: [{
        path: "vendor/module",
        status: "modified",
        staged: false,
        unstaged: true,
        untracked: false,
        conflicted: false,
        additions: 0,
        deletions: 0,
        binary: false,
        submodule: true,
        submoduleCommitChanged: false,
        submoduleModified: true,
        submoduleUntracked: false
      }],
      warnings: ["1 个子模块包含内部未提交变更；请将子模块目录作为独立工作区处理"]
    };
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={submoduleSnapshot}
        active
      />
    );

    expect(screen.getByText("子模块")).toBeInTheDocument();
    expect(screen.getByText(/请将子模块目录作为独立工作区处理/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "暂存 vendor/module" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", {
      name: "丢弃 vendor/module 的未暂存更改"
    })).not.toBeInTheDocument();
  });

  it("exposes repository recovery controls while keeping conflict resolution available", async () => {
    const user = userEvent.setup();
    const conflictedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      operation: "merge",
      operationRevision: "operation-revision-1",
      conflicted: 1,
      files: [{
        ...snapshot.files[0],
        status: "unmerged",
        staged: true,
        unstaged: true,
        conflicted: true
      }]
    };
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={conflictedSnapshot}
        active
      />
    );

    const operation = screen.getByRole("region", { name: "进行中的 Git 操作" });
    expect(operation).toHaveTextContent("Git 合并 正在进行");
    expect(operation).toHaveTextContent("仍有 1 个冲突");
    expect(within(operation).getByRole("button", { name: "继续" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "暂存 src/App.tsx" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "获取" })).toBeDisabled();

    await user.click(within(operation).getByRole("button", { name: "中止" }));
    expect(within(operation).getByRole("button", { name: "确认中止" })).toBeInTheDocument();
    const restartedSnapshot = {
      ...conflictedSnapshot,
      operationRevision: "operation-revision-2"
    };
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={restartedSnapshot}
        active
      />
    );
    expect(within(operation).getByRole("button", { name: "中止" })).toBeInTheDocument();
    expect(git.executeGitAction).not.toHaveBeenCalled();

    await user.click(within(operation).getByRole("button", { name: "中止" }));
    await user.click(within(operation).getByRole("button", { name: "确认中止" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "abort_operation",
        operation: "merge",
        expectedHead: "12ab34cd",
        expectedOperationRevision: "operation-revision-2"
      }
    ));
  });

  it("advances an active bisect with proof-bound old, new, and skip controls", async () => {
    const user = userEvent.setup();
    const bisectSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      contentRevision: "clean-bisect-revision",
      staged: 0,
      unstaged: 0,
      untracked: 0,
      files: [],
      operation: "bisect",
      operationRevision: "bisect-operation-revision-1",
      isClean: true
    };
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-bisect")}
        snapshot={bisectSnapshot}
        active
      />
    );

    const operation = screen.getByRole("region", { name: "进行中的 Git 操作" });
    expect(operation).toHaveTextContent("测试当前提交");
    expect(within(operation).queryByRole("button", { name: "继续" })).not.toBeInTheDocument();
    expect(within(operation).getByRole("button", { name: "标为新状态" })).toBeEnabled();
    expect(within(operation).getByRole("button", { name: "跳过" })).toBeEnabled();
    expect(within(operation).getByRole("button", { name: "结束" })).toBeEnabled();

    await user.click(within(operation).getByRole("button", { name: "标为旧状态" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(within(operation).getByRole("button", { name: "确认旧状态" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-bisect"),
      {
        type: "bisect_step",
        outcome: "old",
        expectedHead: "12ab34cd",
        expectedOperationRevision: "bisect-operation-revision-1",
        expectedContentRevision: "clean-bisect-revision"
      }
    ));
  });

  it("keeps bisect advancement disabled until the worktree is clean", () => {
    const dirtyBisectSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      operation: "bisect",
      operationRevision: "bisect-operation-revision-1"
    };
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-bisect-dirty")}
        snapshot={dirtyBisectSnapshot}
        active
      />
    );

    const operation = screen.getByRole("region", { name: "进行中的 Git 操作" });
    expect(operation).toHaveTextContent("先提交或储藏当前变更");
    expect(within(operation).getByRole("button", { name: "标为旧状态" })).toBeDisabled();
    expect(within(operation).getByRole("button", { name: "标为新状态" })).toBeDisabled();
    expect(within(operation).getByRole("button", { name: "跳过" })).toBeDisabled();
    expect(within(operation).getByRole("button", { name: "结束" })).toBeEnabled();
  });

  it("reconciles the repository snapshot after a failed action mutates Git state", async () => {
    const user = userEvent.setup();
    const onSnapshotChange = vi.fn();
    const conflictedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      summaryRevision: "conflicted-summary",
      operation: "merge",
      operationRevision: "operation-revision-1",
      conflicted: 1,
      changedFiles: 1,
      stageable: 1,
      unstageable: 1,
      filesComplete: false,
      files: [{
        ...snapshot.files[0],
        status: "unmerged",
        conflicted: true
      }]
    };
    const {
      files: _conflictedFiles,
      filesComplete: _conflictedFilesComplete,
      ...conflictedSummary
    } = conflictedSnapshot;
    git.executeGitAction.mockRejectedValueOnce(new Error("merge conflict"));
    git.getGitWorkspaceSummary.mockResolvedValueOnce({
      kind: "snapshot",
      summary: conflictedSummary
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await user.click(await screen.findByRole("button", { name: "合并 main" }));
    await user.click(screen.getByRole("button", { name: "确认合并 main" }));

    await waitFor(() => expect(git.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      undefined
    ));
    expect(onSnapshotChange).toHaveBeenCalledWith({
      ...conflictedSummary,
      files: [],
      filesComplete: false
    });
    expect(screen.getByRole("region", { name: "进行中的 Git 操作" })).toHaveTextContent("Git 合并 正在进行");
    expect(screen.getByRole("alert")).toHaveTextContent("merge conflict");
  });

  it("keeps review readable but disables repository mutations while the workspace is busy", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        mutationDisabledReason="模型正在使用工作区"
      />
    );

    expect(screen.getByText("模型正在使用工作区")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "历史" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "暂存 src/App.tsx" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "暂存 src/App.tsx" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("disarms destructive confirmations whenever workspace mutations become locked", async () => {
    const user = userEvent.setup();
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: "合并 main" }));
    expect(screen.getByRole("button", { name: "确认合并 main" })).toBeInTheDocument();

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
        mutationDisabledReason="模型正在使用工作区"
      />
    );
    await waitFor(() => expect(screen.queryByRole("button", { name: "确认合并 main" })).not.toBeInTheDocument());

    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
      />
    );
    expect(await screen.findByRole("button", { name: "合并 main" })).toBeInTheDocument();
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("loads history and branches only when their review views are selected", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="history"
        active
      />
    );

    await waitFor(() => expect(git.getGitHistory).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { limit: 100 }
    ));
    expect(git.getGitBranches).not.toHaveBeenCalled();
    expect(git.getGitHubRepository).not.toHaveBeenCalled();

    await user.click(screen.getByRole("tab", { name: "分支" }));
    await waitFor(() => expect(git.getGitBranches).toHaveBeenCalledWith(gitConversationTarget("conversation-1")));
    expect(git.getGitHubRepository).not.toHaveBeenCalled();
  });

  it("loads additional history pages with the backend cursor", async () => {
    const user = userEvent.setup();
    git.getGitHistory
      .mockResolvedValueOnce({
        nextCursor: "100",
        commits: [{
          oid: "12ab34cd",
          shortOid: "12ab34cd",
          subject: "Newest commit",
          authorName: "Cat",
          authoredAt: "2026-07-24T00:00:00Z",
          parents: []
        }]
      })
      .mockResolvedValueOnce({
        nextCursor: null,
        commits: [{
          oid: "98fe76dc",
          shortOid: "98fe76dc",
          subject: "Older commit",
          authorName: "Cat",
          authoredAt: "2026-07-23T00:00:00Z",
          parents: []
        }]
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="history"
        active
      />
    );

    await screen.findByText("Newest commit");
    await user.click(screen.getByRole("button", { name: "加载更多提交" }));
    await waitFor(() => expect(git.getGitHistory).toHaveBeenNthCalledWith(
      2,
      gitConversationTarget("conversation-1"),
      { limit: 100, cursor: "100" }
    ));
    expect(await screen.findByText("Older commit")).toBeInTheDocument();
    expect(screen.getByText("Newest commit")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "加载更多提交" })).not.toBeInTheDocument();
  });

  it("keeps concurrent view requests independent when tabs change quickly", async () => {
    const user = userEvent.setup();
    let resolveHistory!: (value: Awaited<ReturnType<typeof git.getGitHistory>>) => void;
    git.getGitHistory.mockReturnValueOnce(new Promise((resolve) => {
      resolveHistory = resolve;
    }));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="history"
        active
      />
    );

    await waitFor(() => expect(git.getGitHistory).toHaveBeenCalledTimes(1));
    await user.click(screen.getByRole("tab", { name: "分支" }));
    await screen.findByText("origin/main");

    await act(async () => {
      resolveHistory({
        nextCursor: null,
        commits: [{
          oid: "12ab34cd",
          shortOid: "12ab34cd",
          subject: "Add Git review",
          authorName: "Cat",
          authoredAt: "2026-07-24T00:00:00Z",
          parents: []
        }]
      });
    });
    await user.click(screen.getByRole("tab", { name: "历史" }));

    expect(await screen.findByText("Add Git review")).toBeInTheDocument();
    expect(git.getGitHistory).toHaveBeenCalledTimes(1);
  });

  it("requires confirmation to merge or safely delete a local branch", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
      />
    );

    await screen.findByRole("button", { name: "合并 main" });
    await user.click(screen.getByRole("button", { name: "合并 main" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "确认合并 main" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "merge",
        branch: "main",
        expectedHead: "12ab34cd",
        expectedBranchOid: "98fe76dc"
      }
    ));

    await waitFor(() => expect(screen.getByRole("button", { name: "删除分支 main" })).toBeEnabled());
    git.executeGitAction.mockClear();
    await user.click(screen.getByRole("button", { name: "删除分支 main" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "确认删除分支 main" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "delete_branch",
        name: "main",
        expectedHead: "12ab34cd",
        expectedOid: "98fe76dc"
      }
    ));
  });

  it("uses a newly checked-out branch as the compare head and rejects identical refs", async () => {
    const user = userEvent.setup();
    git.executeGitAction.mockResolvedValueOnce({
      snapshot: { ...snapshot, branch: "feature/new-review" }
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
      />
    );

    await user.type(screen.getByRole("textbox", { name: "新分支名称" }), "feature/new-review");
    await user.click(screen.getByRole("button", { name: "新建并切换" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "create_branch", name: "feature/new-review", checkout: true }
    ));

    await user.click(screen.getByRole("tab", { name: "比较" }));
    const base = screen.getByRole("combobox", { name: "基准" });
    const head = screen.getByRole("combobox", { name: "目标" });
    expect(base).toHaveValue("origin/feature/review");
    expect(head).toHaveValue("feature/new-review");
    expect(screen.getByRole("button", { name: "比较" })).toBeEnabled();

    await user.selectOptions(base, "main");
    await user.selectOptions(head, "main");
    expect(screen.getByRole("button", { name: "比较" })).toBeDisabled();
    expect(screen.getByText("请选择两个不同的分支")).toBeInTheDocument();
  });

  it("allows remote refs to participate in branch comparisons", async () => {
    const user = userEvent.setup();
    git.getGitBranches.mockResolvedValueOnce({
      defaultBranch: "main",
      branches: [{
        name: "main",
        fullName: "refs/heads/main",
        kind: "local",
        current: true,
        head: "12ab34cd",
        upstream: "origin/main",
        ahead: 0,
        behind: 0
      }, {
        name: "origin/release",
        fullName: "refs/remotes/origin/release",
        kind: "remote",
        current: false,
        head: "98fe76dc",
        upstream: null,
        ahead: 0,
        behind: 0
      }]
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="compare"
        active
      />
    );

    const base = screen.getByRole("combobox", { name: "基准" });
    expect(await within(base).findByRole("option", { name: "origin/release" })).toBeInTheDocument();
    await user.selectOptions(base, "origin/release");
    await user.click(screen.getByRole("button", { name: "比较" }));
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "compare",
        base: "origin/release",
        head: "feature/review"
      }
    ));
  });

  it("distinguishes an empty comparison result from a comparison that has not run", async () => {
    const user = userEvent.setup();
    git.getGitDiff.mockResolvedValueOnce({
      patch: "",
      path: null,
      additions: 0,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="compare"
        active
      />
    );

    expect(screen.getByText("选择两个分支开始比较")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "比较" }));
    await waitFor(() => expect(screen.getByText("两个分支没有差异")).toBeInTheDocument());
    expect(git.getGitDiff).toHaveBeenCalledWith(gitConversationTarget("conversation-1"), {
      type: "compare",
      base: "origin/feature/review",
      head: "feature/review"
    });

    await user.selectOptions(screen.getByRole("combobox", { name: "基准" }), "main");
    expect(screen.queryByText("两个分支没有差异")).not.toBeInTheDocument();
    expect(screen.getByText("选择两个分支开始比较")).toBeInTheDocument();
  });

  it("invalidates an in-flight comparison when either selected ref changes", async () => {
    const user = userEvent.setup();
    let resolveComparison!: (value: {
      patch: string;
      path: null;
      additions: number;
      deletions: number;
      binary: boolean;
      truncated: boolean;
      files: [];
    }) => void;
    git.getGitDiff.mockImplementationOnce(() => new Promise((resolve) => {
      resolveComparison = resolve;
    }));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="compare"
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "比较" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "基准" }), "main");
    await act(async () => {
      resolveComparison({
        patch: "stale comparison patch",
        path: null,
        additions: 1,
        deletions: 0,
        binary: false,
        truncated: false,
        files: []
      });
      await Promise.resolve();
    });

    expect(screen.queryByText("stale comparison patch")).not.toBeInTheDocument();
    expect(screen.getByText("选择两个分支开始比较")).toBeInTheDocument();
  });

  it("keeps the full comparison file list while loading a selected file patch", async () => {
    const user = userEvent.setup();
    git.getGitDiff
      .mockResolvedValueOnce({
        patch: "summary patch",
        path: null,
        additions: 2,
        deletions: 0,
        binary: false,
        truncated: false,
        files: [{
          path: "src/a.ts",
          status: "modified",
          staged: false,
          unstaged: true,
          additions: 1,
          deletions: 0
        }, {
          path: "src/b.ts",
          status: "modified",
          staged: false,
          unstaged: true,
          additions: 1,
          deletions: 0
        }]
      })
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-old\n+selected",
        path: "src/a.ts",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: [{
          path: "src/a.ts",
          status: "modified",
          staged: false,
          unstaged: true,
          additions: 1,
          deletions: 1
        }]
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="compare"
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "比较" }));
    const firstFile = await screen.findByRole("button", { name: /src\/a\.ts/ });
    expect(screen.getByRole("button", { name: /src\/b\.ts/ })).toBeInTheDocument();
    await user.click(firstFile);
    await waitFor(() => expect(git.getGitDiff).toHaveBeenLastCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "compare",
        base: "origin/feature/review",
        head: "feature/review",
        path: "src/a.ts"
      }
    ));
    expect(screen.getByRole("button", { name: /src\/b\.ts/ })).toBeInTheDocument();
  });

  it("does not retry failed lazy loads until the user explicitly asks", async () => {
    const user = userEvent.setup();
    git.getGitHubRepository.mockRejectedValue(new Error("gh unavailable"));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("gh unavailable");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(git.getGitHubRepository).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(git.getGitHubRepository).toHaveBeenCalledTimes(2));
  });

  it("requires confirmation before stashing or popping the latest stash", async () => {
    const user = userEvent.setup();
    const snapshotWithStash: GitWorkspaceSnapshot = { ...snapshot, stash: 1 };
    git.executeGitAction.mockResolvedValue({ snapshot: snapshotWithStash });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshotWithStash}
        initialView="branches"
        active
      />
    );

    await user.click(screen.getByRole("button", { name: "储藏更改" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认储藏更改" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "stash" }
    ));

    await waitFor(() => expect(screen.getByRole("button", { name: "弹出最近储藏" })).toBeEnabled());
    git.executeGitAction.mockClear();
    await user.click(screen.getByRole("button", { name: "弹出最近储藏" }));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认弹出最近储藏" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "stash_pop", index: 0 }
    ));
  });

  it("can stash an untracked-only workspace without silently leaving files behind", async () => {
    const user = userEvent.setup();
    const untrackedOnlySnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 0,
      unstaged: 0,
      untracked: 1,
      files: [{
        path: "notes.txt",
        status: "untracked",
        staged: false,
        unstaged: false,
        untracked: true,
        additions: 1,
        deletions: 0
      }]
    };
    git.executeGitAction.mockResolvedValue({ snapshot: untrackedOnlySnapshot });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-untracked-stash")}
        snapshot={untrackedOnlySnapshot}
        initialView="branches"
        active
      />
    );

    const stash = screen.getByRole("button", {
      name: "储藏更改与未跟踪文件"
    });
    expect(stash).toBeEnabled();
    await user.click(stash);
    expect(git.executeGitAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", {
      name: "确认储藏更改与未跟踪文件"
    }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-untracked-stash"),
      { type: "stash", includeUntracked: true }
    ));
  });

  it("reloads branch and history caches after operations that can change them", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="branches"
        active
      />
    );

    await waitFor(() => expect(git.getGitBranches).toHaveBeenCalledTimes(1));
    await user.click(screen.getByRole("button", { name: "获取" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "fetch",
        expectedRepositoryId: snapshot.repositoryId,
        expectedWorktreeId: snapshot.worktreeId,
        remote: originRemote
      }
    ));
    await waitFor(() => expect(git.getGitBranches).toHaveBeenCalledTimes(2));

    await user.click(screen.getByRole("tab", { name: "历史" }));
    await waitFor(() => expect(git.getGitHistory).toHaveBeenCalledTimes(1));
    await user.click(screen.getByRole("tab", { name: "变更" }));
    await user.type(screen.getByRole("textbox", { name: "提交说明" }), "Refresh history");
    await user.click(screen.getByRole("button", { name: "提交 1 个已暂存文件" }));
    await waitFor(() => expect(git.prepareGitCommit).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      "Refresh history"
    ));
    expect(screen.getByText(`1 个文件 · 树 ${commitCandidateTreeOid.slice(0, 8)}`))
      .toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "确认提交 1 个文件" }));
    await waitFor(() => expect(git.prepareGitCommit).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        type: "commit",
        message: "Refresh history",
        expectedTargetRevision: "commit-target-revision-1",
        expectedTreeOid: commitCandidateTreeOid
      }
    ));
    await user.click(screen.getByRole("tab", { name: "历史" }));
    await waitFor(() => expect(git.getGitHistory).toHaveBeenCalledTimes(2));
  });

  it("refreshes a changed commit proof instead of executing it", async () => {
    const user = userEvent.setup();
    const refreshedTreeOid = "c".repeat(40);
    git.prepareGitCommit
      .mockResolvedValueOnce({
        snapshot,
        targetRevision: "commit-target-revision-1",
        candidateTreeOid: commitCandidateTreeOid,
        messageDigest: "digest-1"
      } satisfies GitCommitPreparation)
      .mockResolvedValueOnce({
        snapshot,
        targetRevision: "commit-target-revision-1",
        candidateTreeOid: refreshedTreeOid,
        messageDigest: "digest-2"
      } satisfies GitCommitPreparation);
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-commit-drift")}
        snapshot={snapshot}
        active
      />
    );

    await user.type(screen.getByRole("textbox", { name: "提交说明" }), "Proof-bound commit");
    await user.click(screen.getByRole("button", { name: "提交 1 个已暂存文件" }));
    expect(await screen.findByText(`1 个文件 · 树 ${commitCandidateTreeOid.slice(0, 8)}`))
      .toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "确认提交 1 个文件" }));

    await waitFor(() => expect(git.prepareGitCommit).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(await screen.findByText(`1 个文件 · 树 ${refreshedTreeOid.slice(0, 8)}`))
      .toBeInTheDocument();
    expect(screen.getByText(/提交候选已变化/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "确认提交 1 个文件" })).toBeEnabled();
  });

  it("ignores a stale commit preparation after the message changes", async () => {
    const user = userEvent.setup();
    let resolvePreparation!: (value: GitCommitPreparation) => void;
    git.prepareGitCommit.mockReturnValueOnce(new Promise((resolve) => {
      resolvePreparation = resolve;
    }));
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stale-commit")}
        snapshot={snapshot}
        active
      />
    );

    const message = screen.getByRole("textbox", { name: "提交说明" });
    await user.type(message, "Old message");
    await user.click(screen.getByRole("button", { name: "提交 1 个已暂存文件" }));
    fireEvent.change(message, { target: { value: "New message" } });
    await act(async () => {
      resolvePreparation({
        snapshot,
        targetRevision: "stale-target",
        candidateTreeOid: commitCandidateTreeOid,
        messageDigest: "stale-digest"
      });
    });

    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(screen.queryByText(`1 个文件 · 树 ${commitCandidateTreeOid.slice(0, 8)}`))
      .not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交 1 个已暂存文件" })).toBeEnabled();
  });

  it("clears a commit proof when repository scope, snapshot, or another mutation changes", async () => {
    const user = userEvent.setup();
    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-proof-scope")}
        snapshot={snapshot}
        active
      />
    );
    const prepare = async () => {
      const commit = screen.getByRole("button", { name: "提交 1 个已暂存文件" });
      await user.click(commit);
      await screen.findByRole("button", { name: "确认提交 1 个文件" });
    };

    await user.type(screen.getByRole("textbox", { name: "提交说明" }), "Scoped proof");
    await prepare();
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-proof-scope-2")}
        snapshot={snapshot}
        active
      />
    );
    await waitFor(() => expect(screen.queryByRole("button", { name: "确认提交 1 个文件" }))
      .not.toBeInTheDocument());

    await prepare();
    const changedSnapshot = {
      ...snapshot,
      contentRevision: "revision-2",
      head: "56ef78ab"
    };
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-proof-scope-2")}
        snapshot={changedSnapshot}
        active
      />
    );
    await waitFor(() => expect(screen.queryByRole("button", { name: "确认提交 1 个文件" }))
      .not.toBeInTheDocument());

    await prepare();
    await user.click(screen.getByRole("button", { name: "暂存 src/App.tsx" }));
    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-proof-scope-2"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(screen.queryByRole("button", { name: "确认提交 1 个文件" }))
      .not.toBeInTheDocument();
  });

  it("requires a second confirmation and sends the exact proof-bound merge payload", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await screen.findByText("Review the built-in Git UI.");
    expect(screen.getByRole("group", { name: "GitHub 写入身份" })).toHaveTextContent(
      "github.com/example-org/Mework"
    );
    expect(screen.getByRole("group", { name: "GitHub 写入身份" })).toHaveTextContent(
      "@example-user · 提交 01234567"
    );
    expect(screen.getByRole("button", { name: "检出" })).toBeDisabled();

    await waitFor(() => expect(git.getGitHubPullRequestReadiness).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      pullRequest.number
    ));
    const mergeMethod = await screen.findByRole("combobox", { name: "合并方式" });
    expect(mergeMethod).toHaveValue("squash");
    expect(within(mergeMethod).getAllByRole("option")).toHaveLength(1);
    expect(within(mergeMethod).getByRole("option", { name: "压缩合并" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeEnabled();
    expect(screen.getByText(/首次点击只锁定/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "压缩合并" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "确认压缩合并" })).toBeEnabled();
    expect(screen.getByText(/base 89abcdef、head 01234567/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "确认压缩合并" }));
    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "merge_pull_request",
        number: pullRequest.number,
        expectedHeadOid: pullRequestReadiness.identity.headRefOid,
        expectedBaseOid: pullRequestReadiness.identity.baseRefOid,
        expectedState: "open",
        expectedIdentityRevision: pullRequestReadiness.identityRevision,
        expectedReadinessRevision: pullRequestReadiness.readinessRevision,
        method: "squash"
      }
    ));
    await waitFor(() => expect(git.getGitHubPullRequestDetail.mock.calls.length).toBeGreaterThan(1));
    await waitFor(() => expect(git.getGitHubPullRequestReadiness.mock.calls.length).toBeGreaterThan(1));
    expect(screen.queryByRole("button", { name: "确认压缩合并" })).not.toBeInTheDocument();
  });

  it("keeps the selected diff side in a local draft and submits an exact review action", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", {
      name: "选择 src/App.tsx 新文件第 1 行"
    }));
    await user.type(screen.getByRole("textbox", { name: "第 1 行评论" }), "Keep the new branch explicit.");
    await user.click(screen.getByRole("button", { name: "保存草稿" }));
    expect(screen.getByRole("button", {
      name: "移除 src/App.tsx 第 1 行草稿"
    })).toBeInTheDocument();

    await user.type(screen.getByRole("textbox", { name: "审阅总结" }), "Please revise this path.");
    await user.click(within(screen.getByRole("group", { name: "审阅结论" }))
      .getByRole("button", { name: "要求修改" }));
    await user.click(screen.getByRole("button", { name: "提交审阅" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认提交审阅" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "submit_pull_request_review",
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        event: "request_changes",
        body: "Please revise this path.",
        comments: [{
          path: "src/App.tsx",
          line: 1,
          side: "RIGHT",
          body: "Keep the new branch explicit."
        }]
      }
    ));
  });

  it("requires a summary for comment reviews even when inline drafts exist", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", {
      name: "选择 src/App.tsx 新文件第 1 行"
    }));
    fireEvent.change(screen.getByRole("textbox", { name: "第 1 行评论" }), {
      target: { value: "Keep this explicit." }
    });
    await user.click(screen.getByRole("button", { name: "保存草稿" }));

    expect(screen.getByRole("textbox", { name: "审阅总结" })).toBeRequired();
    expect(screen.getByRole("textbox", { name: "审阅总结" })).toHaveAttribute(
      "placeholder",
      "填写本次评论的总结（必填）"
    );
    expect(screen.getByText("评论审阅必须填写总结，行评论草稿不能替代总结。"))
      .toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeDisabled();

    fireEvent.change(screen.getByRole("textbox", { name: "审阅总结" }), {
      target: { value: "One inline note." }
    });
    expect(screen.queryByText("评论审阅必须填写总结，行评论草稿不能替代总结。"))
      .not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "提交审阅" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认提交审阅" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "submit_pull_request_review",
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        event: "comment",
        body: "One inline note.",
        comments: [{
          path: "src/App.tsx",
          line: 1,
          side: "RIGHT",
          body: "Keep this explicit."
        }]
      }
    ));
  });

  it("allows approval without a summary while retaining second confirmation", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(within(screen.getByRole("group", { name: "审阅结论" }))
      .getByRole("button", { name: "通过" }));

    expect(screen.getByRole("textbox", { name: "审阅总结" })).not.toBeRequired();
    expect(screen.getByRole("textbox", { name: "审阅总结" })).toHaveAttribute(
      "placeholder",
      "审阅总结（可选）"
    );
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "提交审阅" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认提交审阅" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "submit_pull_request_review",
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        event: "approve",
        comments: []
      }
    ));
  });

  it("replies to and resolves a review thread with exact identity and head guards", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReviewThreads.mockResolvedValue({
      number: pullRequest.number,
      headRefOid: pullRequestDetail.headRefOid,
      threads: [reviewThread],
      totalCount: 1,
      nextCursor: null
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("Please keep this branch explicit.")).toBeInTheDocument();
    await user.type(screen.getByRole("textbox", { name: "回复线程 thread-1" }), "Agreed.");
    await user.click(screen.getByRole("button", { name: "回复" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认回复" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenNthCalledWith(
      1,
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "reply_review_thread",
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        threadId: reviewThread.id,
        body: "Agreed."
      }
    ));

    const resolveButton = await screen.findByRole("button", { name: "解决" });
    await waitFor(() => expect(resolveButton).toBeEnabled());
    await user.click(resolveButton);
    await user.click(screen.getByRole("button", { name: "确认解决" }));
    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenNthCalledWith(
      2,
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "resolve_review_thread",
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        threadId: reviewThread.id
      }
    ));
  });

  it("paginates review threads, deduplicates overlaps, and clears drafts on a stale head", async () => {
    const user = userEvent.setup();
    const secondThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      id: "thread-2",
      line: 2,
      originalLine: 2,
      comments: [{
        ...reviewThread.comments[0],
        id: "comment-2",
        body: "A second thread."
      }]
    };
    git.getGitHubPullRequestReviewThreads
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threads: [reviewThread],
        totalCount: 3,
        nextCursor: "cursor-2"
      })
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threads: [reviewThread, secondThread],
        totalCount: 3,
        nextCursor: "cursor-3"
      })
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: "fedcba9876543210fedcba9876543210fedcba98",
        threads: [],
        totalCount: 0,
        nextCursor: null
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", {
      name: "选择 src/App.tsx 新文件第 1 行"
    }));
    fireEvent.change(screen.getByRole("textbox", { name: "第 1 行评论" }), {
      target: { value: "Do not submit this stale draft." }
    });
    await user.click(screen.getByRole("button", { name: "保存草稿" }));
    fireEvent.change(screen.getByRole("textbox", { name: "审阅总结" }), {
      target: { value: "Local review body" }
    });

    await user.click(screen.getByRole("button", { name: "加载更多线程" }));
    expect(await screen.findByText("A second thread.")).toBeInTheDocument();
    expect(screen.getAllByText("Please keep this branch explicit.")).toHaveLength(1);
    expect(git.getGitHubPullRequestReviewThreads).toHaveBeenNthCalledWith(
      2,
      gitConversationTarget("conversation-1"),
      {
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        cursor: "cursor-2",
        pageSize: 30
      }
    );

    await user.click(screen.getByRole("button", { name: "加载更多线程" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("审阅草稿已清除");
    expect(screen.queryByRole("button", {
      name: "移除 src/App.tsx 第 1 行草稿"
    })).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "审阅总结" })).toHaveValue("");
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeDisabled();
    expect(git.getGitHubPullRequestReviewThreads).toHaveBeenNthCalledWith(
      3,
      gitConversationTarget("conversation-1"),
      {
        number: pullRequest.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        cursor: "cursor-3",
        pageSize: 30
      }
    );
  });

  it("paginates thread replies with exact guards, deduplicates comments, and stops cursor loops", async () => {
    const user = userEvent.setup();
    const firstPageThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      commentsTotalCount: 3,
      commentsNextCursor: "reply-cursor-a"
    };
    const secondComment = {
      ...reviewThread.comments[0],
      id: "comment-2",
      body: "Second reply."
    };
    const thirdComment = {
      ...reviewThread.comments[0],
      id: "comment-3",
      body: "Third reply."
    };
    git.getGitHubPullRequestReviewThreads.mockResolvedValueOnce({
      number: pullRequest.number,
      headRefOid: pullRequestDetail.headRefOid,
      threads: [firstPageThread],
      totalCount: 1,
      nextCursor: null
    });
    git.getGitHubPullRequestReviewThreadComments
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threadId: reviewThread.id,
        comments: [reviewThread.comments[0], secondComment],
        totalCount: 3,
        nextCursor: "reply-cursor-b"
      })
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threadId: reviewThread.id,
        comments: [thirdComment],
        totalCount: 3,
        nextCursor: "reply-cursor-a"
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", { name: /加载更多回复/ }));

    expect(await screen.findByText("Second reply.")).toBeInTheDocument();
    expect(screen.getAllByText("Please keep this branch explicit.")).toHaveLength(1);
    expect(git.getGitHubPullRequestReviewThreadComments).toHaveBeenNthCalledWith(
      1,
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        number: pullRequest.number,
        expectedState: "open",
        expectedHeadOid: pullRequestDetail.headRefOid,
        threadId: reviewThread.id,
        cursor: "reply-cursor-a",
        pageSize: 50
      }
    );

    await user.click(screen.getByRole("button", { name: /加载更多回复/ }));
    expect(await screen.findByText("Third reply.")).toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("重复的回复游标");
    expect(screen.queryByRole("button", { name: /加载更多回复/ })).not.toBeInTheDocument();
    expect(git.getGitHubPullRequestReviewThreadComments).toHaveBeenNthCalledWith(
      2,
      gitConversationTarget("conversation-1"),
      expect.objectContaining({
        threadId: reviewThread.id,
        cursor: "reply-cursor-b",
        pageSize: 50
      })
    );
  });

  it("keeps reply pagination loading and errors isolated per review thread", async () => {
    const user = userEvent.setup();
    const firstThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      commentsTotalCount: 2,
      commentsNextCursor: "thread-1-cursor"
    };
    const secondThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      id: "thread-2",
      line: 2,
      originalLine: 2,
      comments: [{
        ...reviewThread.comments[0],
        id: "thread-2-comment-1",
        body: "Second thread initial reply."
      }],
      commentsTotalCount: 2,
      commentsNextCursor: "thread-2-cursor"
    };
    let resolveFirstThread!: (value: GitHubPullRequestReviewThreadCommentsResult) => void;
    git.getGitHubPullRequestReviewThreads.mockResolvedValueOnce({
      number: pullRequest.number,
      headRefOid: pullRequestDetail.headRefOid,
      threads: [firstThread, secondThread],
      totalCount: 2,
      nextCursor: null
    });
    git.getGitHubPullRequestReviewThreadComments.mockImplementation((_, request) => {
      if (request.threadId === firstThread.id) {
        return new Promise((resolve) => {
          resolveFirstThread = resolve;
        });
      }
      return Promise.reject(new Error("Thread two replies failed"));
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    const firstThreadArticle = (await screen.findByText(
      "Please keep this branch explicit."
    )).closest("article");
    const secondThreadArticle = (await screen.findByText(
      "Second thread initial reply."
    )).closest("article");
    expect(firstThreadArticle).not.toBeNull();
    expect(secondThreadArticle).not.toBeNull();

    await user.click(within(firstThreadArticle!).getByRole("button", {
      name: /加载更多回复/
    }));
    await user.click(within(secondThreadArticle!).getByRole("button", {
      name: /加载更多回复/
    }));

    expect(within(firstThreadArticle!).getByRole("button", {
      name: "正在加载回复"
    })).toBeDisabled();
    expect(await within(secondThreadArticle!).findByRole("alert")).toHaveTextContent(
      "Thread two replies failed"
    );
    expect(within(secondThreadArticle!).getByRole("button", {
      name: /加载更多回复/
    })).toBeEnabled();

    await act(async () => {
      resolveFirstThread({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threadId: firstThread.id,
        comments: [{
          ...reviewThread.comments[0],
          id: "comment-2",
          body: "First thread loaded reply."
        }],
        totalCount: 2,
        nextCursor: null
      });
    });
    expect(await screen.findByText("First thread loaded reply.")).toBeInTheDocument();
    expect(screen.getByText("Thread two replies failed")).toBeInTheDocument();
  });

  it("ignores a pending reply page after the review switches to another head", async () => {
    const user = userEvent.setup();
    const oldThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      commentsTotalCount: 2,
      commentsNextCursor: "old-head-cursor"
    };
    const newHeadOid = "fedcba9876543210fedcba9876543210fedcba98";
    const newDetail: GitHubPullRequestDetail = {
      ...pullRequestDetail,
      headRefOid: newHeadOid
    };
    const newThread: GitHubPullRequestReviewThread = {
      ...reviewThread,
      comments: [{
        ...reviewThread.comments[0],
        id: "new-head-comment",
        body: "Reply from the new head."
      }],
      commentsTotalCount: 1,
      commentsNextCursor: null
    };
    let resolveOldPage!: (value: GitHubPullRequestReviewThreadCommentsResult) => void;
    git.getGitHubPullRequestDetail
      .mockResolvedValueOnce(pullRequestDetail)
      .mockResolvedValueOnce(newDetail);
    git.getGitHubPullRequestDiff
      .mockResolvedValueOnce({
        headRefOid: pullRequestDetail.headRefOid,
        patch: "@@ -1 +1 @@\n-old\n+new",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      })
      .mockResolvedValueOnce({
        headRefOid: newHeadOid,
        patch: "@@ -1 +1 @@\n-old\n+newer",
        path: "src/App.tsx",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      });
    git.getGitHubPullRequestReviewThreads
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threads: [oldThread],
        totalCount: 1,
        nextCursor: null
      })
      .mockResolvedValueOnce({
        number: pullRequest.number,
        headRefOid: newHeadOid,
        threads: [newThread],
        totalCount: 1,
        nextCursor: null
      });
    git.getGitHubPullRequestReviewThreadComments.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveOldPage = resolve;
      })
    );
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", { name: /加载更多回复/ }));
    await user.click(screen.getByRole("button", { name: "返回" }));
    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("Reply from the new head.")).toBeInTheDocument();

    await act(async () => {
      resolveOldPage({
        number: pullRequest.number,
        headRefOid: pullRequestDetail.headRefOid,
        threadId: oldThread.id,
        comments: [{
          ...reviewThread.comments[0],
          id: "stale-page-comment",
          body: "Stale reply that must stay hidden."
        }],
        totalCount: 2,
        nextCursor: null
      });
    });
    expect(screen.queryByText("Stale reply that must stay hidden.")).not.toBeInTheDocument();
    expect(screen.getByText("Reply from the new head.")).toBeInTheDocument();
  });

  it("keeps PR detail and readiness guardrails available when review threads fail to load", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReviewThreads.mockRejectedValueOnce(
      new Error("GraphQL reviewThreads is unavailable")
    );
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("Review the built-in Git UI.")).toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("GraphQL reviewThreads is unavailable");
    expect(await screen.findByText("合并状态正常")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeEnabled();
    expect(screen.queryByRole("button", {
      name: "选择 src/App.tsx 新文件第 1 行"
    })).not.toBeInTheDocument();
  });

  it("shows fork identity, groups required and optional checks, and honors the allowed default method", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestDetail.mockResolvedValueOnce({
      ...pullRequestDetail,
      statusCheckRollup: "failure",
      checks: [{
        name: "legacy-rollup",
        state: "failure",
        workflow: "Legacy"
      }]
    });
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      identity: {
        ...pullRequestReadiness.identity,
        headRepository: {
          host: repository.host,
          nodeId: "fork-repository-node",
          nameWithOwner: "contributor/Mework-fork"
        }
      },
      mergePolicy: {
        ...pullRequestReadiness.mergePolicy,
        mergeStateStatus: "UNSTABLE",
        squashMergeAllowed: true,
        rebaseMergeAllowed: true
      },
      checks: {
        availability: "available",
        value: {
          totalCount: 2,
          checks: [{
            nodeId: "check-required",
            kind: "CheckRun",
            name: "typecheck",
            state: "COMPLETED",
            conclusion: "SUCCESS",
            workflow: "CI",
            description: null,
            link: "https://github.com/example-org/Mework/actions/runs/1",
            startedAt: "2026-07-24T00:00:00Z",
            completedAt: "2026-07-24T00:01:00Z",
            required: true
          }, {
            nodeId: "check-optional",
            kind: "CheckRun",
            name: "browser-e2e",
            state: "COMPLETED",
            conclusion: "FAILURE",
            workflow: "CI",
            description: "Chromium failed",
            link: null,
            startedAt: "2026-07-24T00:00:00Z",
            completedAt: "2026-07-24T00:02:00Z",
            required: false
          }]
        },
        error: null
      },
      viewerDefault: {
        availability: "available",
        value: { mergeMethod: "REBASE" },
        error: null
      },
      readinessRevision: "readiness-revision-fork"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByRole("group", { name: "拉取请求仓库路径" })).toHaveTextContent(
      "contributor/Mework-fork:feature/review → example-org/Mework:main"
    );
    expect(screen.getByText("非必需检查未通过")).toBeInTheDocument();
    expect(screen.getByText("必需检查通过")).toBeInTheDocument();
    await user.click(await screen.findByText("检查详情"));

    const required = screen.getByRole("region", { name: "必需检查" });
    const optional = screen.getByRole("region", { name: "其他检查" });
    expect(within(required).getByText("typecheck")).toBeInTheDocument();
    expect(within(required).getByText("不阻断")).toBeInTheDocument();
    expect(within(optional).getByText("browser-e2e")).toBeInTheDocument();
    expect(within(optional).getByText("CI · Chromium failed")).toBeInTheDocument();
    expect(within(optional).getByText("失败")).toBeInTheDocument();
    expect(within(optional).getByText("不作为阻断依据")).toBeInTheDocument();
    expect(screen.queryByText("legacy-rollup")).not.toBeInTheDocument();
    const mergeMethod = screen.getByRole("combobox", { name: "合并方式" });
    expect(mergeMethod).toHaveValue("rebase");
    expect(within(mergeMethod).getAllByRole("option").map((option) => option.getAttribute("value")))
      .toEqual(["squash", "rebase"]);
    expect(screen.getByRole("button", { name: "变基合并" })).toBeEnabled();
    expect(git.getGitHubPullRequestDetail).toHaveBeenCalledTimes(1);
    expect(git.getGitHubPullRequestReadiness).toHaveBeenCalledTimes(1);
  });

  it("disables merge when a required check has not completed successfully", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      mergePolicy: {
        ...pullRequestReadiness.mergePolicy,
        mergeStateStatus: "UNSTABLE"
      },
      checks: {
        availability: "available",
        value: {
          totalCount: 1,
          checks: [{
            nodeId: "required-failure",
            kind: "CheckRun",
            name: "required-ci",
            state: "COMPLETED",
            conclusion: "FAILURE",
            workflow: "CI",
            description: null,
            link: null,
            startedAt: null,
            completedAt: null,
            required: true
          }]
        },
        error: null
      },
      readinessRevision: "readiness-required-failure"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-required-failure")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("1 项必需检查未通过")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeDisabled();
    expect(screen.getByText("1 项必需检查尚未成功完成。")).toBeInTheDocument();
  });

  it("disables merge for an unknown merge state", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      mergePolicy: {
        ...pullRequestReadiness.mergePolicy,
        mergeStateStatus: "UNKNOWN"
      },
      readinessRevision: "readiness-unknown-state"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-unknown-merge-state")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findAllByText("合并状态未知")).toHaveLength(2);
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeDisabled();
  });

  it("fails closed when the target branch requires a merge queue", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      mergeQueue: {
        availability: "available",
        value: {
          enabled: true,
          isInQueue: false,
          entry: null
        },
        error: null
      },
      readinessRevision: "readiness-queue-required"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-queue-required")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("目标分支要求合并队列")).toBeInTheDocument();
    expect(screen.getByText("目标分支要求合并队列；即时合并不可用。"))
      .toBeInTheDocument();
    expect(screen.queryByText(/自动合并/)).not.toBeInTheDocument();
    const merge = screen.getByRole("button", { name: "压缩合并" });
    expect(merge).toBeDisabled();
    await user.click(merge);
    expect(screen.queryByRole("button", { name: "确认压缩合并" })).not.toBeInTheDocument();
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
  });

  ([
    {
      label: "unsupported",
      phase: {
        availability: "unsupported",
        value: null,
        error: "merge queue is unsupported"
      }
    },
    {
      label: "error",
      phase: {
        availability: "error",
        value: null,
        error: "merge queue lookup failed"
      }
    },
    {
      label: "missing",
      phase: {
        availability: "available",
        value: null,
        error: null
      }
    }
  ] satisfies Array<{
    label: string;
    phase: GitHubPullRequestReadiness["mergeQueue"];
  }>).forEach(({ label, phase }) => {
    it(`fails closed when merge queue readiness is ${label}`, async () => {
      const user = userEvent.setup();
      git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
        ...pullRequestReadiness,
        mergeQueue: phase,
        readinessRevision: `readiness-queue-${label}`
      });
      render(
        <GitReviewPanel
          target={gitConversationTarget(`conversation-queue-${label}`)}
          snapshot={snapshot}
          initialView="pullRequests"
          active
        />
      );

      await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
      expect(await screen.findByText("无法确认队列要求")).toBeInTheDocument();
      const merge = screen.getByRole("button", { name: "压缩合并" });
      expect(merge).toBeDisabled();
      await user.click(merge);
      expect(screen.queryByRole("button", { name: "确认压缩合并" })).not.toBeInTheDocument();
      expect(git.executeGitHubAction).not.toHaveBeenCalled();
    });
  });

  it("shows merge queue position and state without exposing a queue write action", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      mergeQueue: {
        availability: "available",
        value: {
          enabled: true,
          isInQueue: true,
          entry: {
            entryId: "queue-entry-1",
            position: 3,
            state: "QUEUED",
            enqueuedAt: "2026-07-25T00:00:00Z",
            estimatedTimeToMerge: 300
          }
        },
        error: null
      },
      readinessRevision: "readiness-in-queue"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-in-queue")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("目标分支要求合并队列")).toBeInTheDocument();
    expect(screen.getByText("合并队列 · #3 · QUEUED")).toBeInTheDocument();
    expect(screen.getByText("已在合并队列中（位置 3，状态 QUEUED）。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: /加入.*队列|退出.*队列/ })).not.toBeInTheDocument();
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
  });

  it("shows the active auto-merge method and actor", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      autoMerge: {
        availability: "available",
        value: {
          enabledAt: null,
          mergeMethod: "SQUASH",
          commitHeadline: null,
          commitBody: null,
          enabledBy: "merge-bot"
        },
        error: null
      },
      readinessRevision: "readiness-auto-merge"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-auto-merge")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("自动合并 · 压缩合并 · @merge-bot")).toBeInTheDocument();
    expect(screen.getByText("无需合并队列")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeEnabled();
  });

  it("allows a clean proof when check classification is unsupported", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestDetail.mockResolvedValueOnce({
      ...pullRequestDetail,
      statusCheckRollup: "failure",
      checks: [{
        name: "legacy-unclassified",
        state: "failure",
        workflow: "Legacy"
      }]
    });
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      checks: {
        availability: "unsupported",
        value: null,
        error: "required classification unsupported"
      },
      readinessRevision: "readiness-clean-unclassified"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-clean-unclassified")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("合并状态正常")).toBeInTheDocument();
    expect(screen.getByText("检查未分类")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeEnabled();
  });

  it("clears merge confirmation when the selected method changes", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockResolvedValueOnce({
      ...pullRequestReadiness,
      mergePolicy: {
        ...pullRequestReadiness.mergePolicy,
        rebaseMergeAllowed: true
      },
      readinessRevision: "readiness-two-methods"
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-method-change")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await user.click(await screen.findByRole("button", { name: "压缩合并" }));
    expect(screen.getByRole("button", { name: "确认压缩合并" })).toBeInTheDocument();

    await user.selectOptions(screen.getByRole("combobox", { name: "合并方式" }), "rebase");
    expect(screen.queryByRole("button", { name: "确认压缩合并" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "变基合并" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "变基合并" }));
    expect(screen.getByRole("button", { name: "确认变基合并" })).toBeInTheDocument();
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
  });

  it("keeps basic review available when readiness fails and never enables merge", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestReadiness.mockRejectedValueOnce(
      new Error("required-check classification is unavailable")
    );
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-readiness-error")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByText("Review the built-in Git UI.")).toBeInTheDocument();
    expect(await screen.findByText("required-check classification is unavailable"))
      .toBeInTheDocument();
    expect(screen.getByText("检查未分类")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "合并不可用" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "关闭" })).toBeEnabled();
  });

  it("ignores a late readiness response after selecting another pull request", async () => {
    const user = userEvent.setup();
    const nextPullRequest: GitHubPullRequest = {
      ...pullRequest,
      number: 18,
      title: "Second review",
      url: `${repository.url}/pull/18`,
      headRefName: "feature/second"
    };
    const nextDetail: GitHubPullRequestDetail = {
      ...pullRequestDetail,
      ...nextPullRequest,
      headRefOid: "fedcba9876543210fedcba9876543210fedcba98",
      body: "Review the second pull request."
    };
    const nextReadiness: GitHubPullRequestReadiness = {
      ...pullRequestReadiness,
      identity: {
        ...pullRequestReadiness.identity,
        pullRequestNodeId: "pull-request-node-18",
        number: 18,
        headRefName: nextPullRequest.headRefName,
        headRefOid: nextDetail.headRefOid
      },
      identityRevision: "identity-revision-18",
      readinessRevision: "readiness-revision-18"
    };
    let resolveOldReadiness!: (value: GitHubPullRequestReadiness) => void;
    git.getGitHubPullRequests.mockResolvedValueOnce({
      pullRequests: [pullRequest, nextPullRequest],
      page: 1,
      pageSize: 30,
      hasMore: false,
      nextPage: null
    });
    git.getGitHubPullRequestDetail.mockImplementation((
      _conversationId: string,
      number: number
    ) => Promise.resolve(number === 18 ? nextDetail : pullRequestDetail));
    git.getGitHubPullRequestDiff.mockImplementation((
      _conversationId: string,
      number: number
    ) => Promise.resolve({
      headRefOid: number === 18 ? nextDetail.headRefOid : pullRequestDetail.headRefOid,
      patch: "@@ -1 +1 @@\n-old\n+new",
      path: "src/App.tsx",
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: []
    }));
    git.getGitHubPullRequestReviewThreads.mockImplementation((
      _conversationId: string,
      request: { number: number; expectedHeadOid: string }
    ) => Promise.resolve({
      number: request.number,
      headRefOid: request.expectedHeadOid,
      threads: [],
      totalCount: 0,
      nextCursor: null
    }));
    git.getGitHubPullRequestReadiness.mockImplementation((
      _conversationId: string,
      number: number
    ) => number === 18
      ? Promise.resolve(nextReadiness)
      : new Promise((resolve) => {
          resolveOldReadiness = resolve;
        }));

    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-stale-readiness")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await screen.findByText("Review the built-in Git UI.");
    await waitFor(() => expect(resolveOldReadiness).toBeTypeOf("function"));
    await user.click(screen.getByRole("button", { name: "返回" }));
    await user.click(await screen.findByRole("button", { name: /#18 Second review/ }));
    expect(await screen.findByRole("group", { name: "拉取请求仓库路径" }))
      .toHaveTextContent("example-org/Mework:feature/second → example-org/Mework:main");

    await act(async () => {
      resolveOldReadiness({
        ...pullRequestReadiness,
        identity: {
          ...pullRequestReadiness.identity,
          headRepository: {
            host: repository.host,
            nodeId: "stale-fork-node",
            nameWithOwner: "stale/fork"
          }
        },
        readinessRevision: "stale-readiness"
      });
      await Promise.resolve();
    });
    expect(screen.getByRole("group", { name: "拉取请求仓库路径" }))
      .toHaveTextContent("example-org/Mework:feature/second → example-org/Mework:main");
    expect(screen.queryByText(/stale\/fork/)).not.toBeInTheDocument();
  });

  it("keeps pull request file navigation visible while lazily loading a selected patch", async () => {
    const user = userEvent.setup();
    let resolveFirstFile!: (value: GitHubPullRequestDiff) => void;
    const summaryDiff: GitHubPullRequestDiff = {
      headRefOid: pullRequestDetail.headRefOid,
      patch: "full summary",
      path: null,
      additions: 2,
      deletions: 2,
      binary: false,
      truncated: false,
      files: [{
        path: "src/a.ts",
        status: "modified",
        additions: 1,
        deletions: 1
      }, {
        path: "src/b.ts",
        status: "modified",
        additions: 1,
        deletions: 1
      }]
    };
    git.getGitHubPullRequestDiff.mockImplementation((
      _conversationId: string,
      _number: number,
      path?: string
    ) => {
      if (!path) return Promise.resolve(summaryDiff);
      if (path === "src/a.ts") {
        return new Promise((resolve) => {
          resolveFirstFile = resolve;
        });
      }
      return Promise.resolve({
        ...summaryDiff,
        path,
        patch: "@@ -1 +1 @@\n-old-b\n+new-b",
        files: summaryDiff.files.filter((file) => file.path === path)
      });
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    const files = await screen.findByRole("navigation", { name: "拉取请求文件" });
    expect(within(files).getByRole("button", { name: /src\/a\.ts/ })).toBeInTheDocument();
    const secondFile = within(files).getByRole("button", { name: /src\/b\.ts/ });
    expect(secondFile).toBeInTheDocument();
    await waitFor(() => expect(resolveFirstFile).toBeTypeOf("function"));

    await user.click(secondFile);
    expect(await screen.findByRole("region", { name: "src/b.ts 文件差异" })).toBeInTheDocument();
    expect(within(files).getByRole("button", { name: /src\/a\.ts/ })).toBeInTheDocument();

    await act(async () => {
      resolveFirstFile({
        ...summaryDiff,
        path: "src/a.ts",
        patch: "@@ -1 +1 @@\n-old-a\n+stale-a",
        files: summaryDiff.files.slice(0, 1)
      });
      await Promise.resolve();
    });
    expect(screen.getByRole("region", { name: "src/b.ts 文件差异" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "src/a.ts 文件差异" })).not.toBeInTheDocument();
  });

  it("rejects a file diff from a newer pull request head and disables merge", async () => {
    const user = userEvent.setup();
    const summaryDiff: GitHubPullRequestDiff = {
      headRefOid: pullRequestDetail.headRefOid,
      patch: "full summary",
      path: null,
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: [{
        path: "src/App.tsx",
        status: "modified",
        additions: 1,
        deletions: 1
      }]
    };
    git.getGitHubPullRequestDiff
      .mockResolvedValueOnce(summaryDiff)
      .mockResolvedValueOnce({
        ...summaryDiff,
        headRefOid: "fedcba9876543210fedcba9876543210fedcba98",
        path: "src/App.tsx",
        patch: "newer head"
      });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("读取文件差异时已更新");
    expect(screen.getByRole("navigation", { name: "拉取请求文件" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeDisabled();
  });

  it("loads pull request pages explicitly, deduplicates overlaps, and gates list controls", async () => {
    const user = userEvent.setup();
    const nextPullRequest: GitHubPullRequest = {
      ...pullRequest,
      number: 18,
      title: "Continue Git review",
      url: "https://github.com/example-org/Mework/pull/18",
      headRefName: "feature/continue"
    };
    let resolveNextPage!: (value: {
      pullRequests: GitHubPullRequest[];
      page: number;
      pageSize: number;
      hasMore: boolean;
      nextPage: number | null;
    }) => void;
    git.getGitHubPullRequests
      .mockResolvedValueOnce({
        pullRequests: [pullRequest],
        page: 1,
        pageSize: 30,
        hasMore: true,
        nextPage: 2
      })
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveNextPage = resolve;
      }));

    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await screen.findByRole("button", { name: /#17 Add Git review/ });
    expect(git.getGitHubPullRequests).toHaveBeenCalledWith(gitConversationTarget("conversation-1"), {
      page: 1,
      pageSize: 30
    });

    await user.click(screen.getByRole("button", { name: "加载更多拉取请求" }));
    expect(screen.getByRole("button", { name: "刷新拉取请求" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "新建拉取请求" })).toBeDisabled();
    expect(git.getGitHubPullRequests).toHaveBeenLastCalledWith(gitConversationTarget("conversation-1"), {
      page: 2,
      pageSize: 30
    });

    await act(async () => {
      resolveNextPage({
        pullRequests: [{ ...pullRequest, title: "Add Git review (updated)" }, nextPullRequest],
        page: 2,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      });
    });

    expect(await screen.findByRole("button", { name: /#18 Continue Git review/ })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /#17 Add Git review/ })).toHaveLength(1);
    expect(screen.getByRole("button", { name: /#17 Add Git review \(updated\)/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "加载更多拉取请求" })).not.toBeInTheDocument();
  });

  it("keeps the first pull request page visible during a manual refresh", async () => {
    const user = userEvent.setup();
    const refreshedPullRequest: GitHubPullRequest = {
      ...pullRequest,
      number: 18,
      title: "Refreshed request"
    };
    let resolveRefresh!: (value: {
      pullRequests: GitHubPullRequest[];
      page: number;
      pageSize: number;
      hasMore: boolean;
      nextPage: number | null;
    }) => void;
    git.getGitHubPullRequests
      .mockResolvedValueOnce({
        pullRequests: [pullRequest],
        page: 1,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      })
      .mockReturnValueOnce(new Promise((resolve) => {
        resolveRefresh = resolve;
      }));

    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );
    const existing = await screen.findByRole("button", { name: /#17 Add Git review/ });
    await user.click(screen.getByRole("button", { name: "刷新拉取请求" }));

    expect(existing).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /#17 Add Git review/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "刷新拉取请求" })).toBeDisabled();

    await act(async () => {
      resolveRefresh({
        pullRequests: [refreshedPullRequest],
        page: 1,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      });
    });

    expect(await screen.findByRole("button", { name: /#18 Refreshed request/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /#17 Add Git review/ })).not.toBeInTheDocument();
  });

  it("does not let an old pull request page cross a same-path repository/worktree identity switch", async () => {
    const user = userEvent.setup();
    const repositoryTwo: GitHubRepository = {
      ...repository,
      owner: "other",
      name: "repo",
      nameWithOwner: "other/repo",
      url: "https://github.com/other/repo"
    };
    const oldSecondPage: GitHubPullRequest = {
      ...pullRequest,
      number: 18,
      title: "Old repository request"
    };
    const repositoryTwoPullRequest: GitHubPullRequest = {
      ...pullRequest,
      number: 29,
      title: "New repository request",
      url: "https://github.com/other/repo/pull/29"
    };
    let root: "one" | "two" = "one";
    let resolveOldPage!: (value: {
      pullRequests: GitHubPullRequest[];
      page: number;
      pageSize: number;
      hasMore: boolean;
      nextPage: number | null;
    }) => void;
    git.getGitHubRepository.mockImplementation(async () => (
      root === "one" ? repository : repositoryTwo
    ));
    git.getGitHubPullRequests.mockImplementation((
      _conversationId: string,
      request: { page: number; pageSize: number }
    ) => {
      if (root === "one" && request.page === 1) {
        return Promise.resolve({
          pullRequests: [pullRequest],
          page: 1,
          pageSize: 30,
          hasMore: true,
          nextPage: 2
        });
      }
      if (root === "one") {
        return new Promise((resolve) => {
          resolveOldPage = resolve;
        });
      }
      return Promise.resolve({
        pullRequests: [repositoryTwoPullRequest],
        page: 1,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      });
    });

    const view = render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={{
          ...snapshot,
          repositoryId: "repository-id-one",
          worktreeId: "worktree-id-one",
          repositoryRoot: "C:/same-path",
          worktreeRoot: "C:/same-path"
        }}
        initialView="pullRequests"
        active
      />
    );
    await screen.findByRole("button", { name: /#17 Add Git review/ });
    await user.click(screen.getByRole("button", { name: "加载更多拉取请求" }));
    await waitFor(() => expect(resolveOldPage).toBeTypeOf("function"));

    root = "two";
    view.rerender(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={{
          ...snapshot,
          repositoryId: "repository-id-two",
          worktreeId: "worktree-id-two",
          repositoryRoot: "C:/same-path",
          worktreeRoot: "C:/same-path"
        }}
        initialView="pullRequests"
        active
      />
    );
    expect(await screen.findByText("other/repo")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /#29 New repository request/ })).toBeInTheDocument();

    await act(async () => {
      resolveOldPage({
        pullRequests: [oldSecondPage],
        page: 2,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      });
    });

    expect(screen.queryByRole("button", { name: /#18 Old repository request/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /#29 New repository request/ })).toBeInTheDocument();
  });

  it("rejects a pull request review assembled from different head commits", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestDiff.mockResolvedValueOnce({
      headRefOid: "fedcba9876543210fedcba9876543210fedcba98",
      patch: "@@ -1 +1 @@\n-old\n+new",
      path: null,
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: []
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("拉取请求在读取期间已更新");
    expect(screen.queryByText("Review the built-in Git UI.")).not.toBeInTheDocument();
  });

  it("warns about truncated pull request diffs and blocks merging", async () => {
    const user = userEvent.setup();
    git.getGitHubPullRequestDiff.mockResolvedValueOnce({
      headRefOid: pullRequestDetail.headRefOid,
      patch: "@@ -1 +1 @@\n-old\n+new",
      path: null,
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: true,
      files: []
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("当前审阅不完整");
    expect(screen.getByRole("button", { name: "压缩合并" })).toBeDisabled();
    const reviewEvent = screen.getByRole("group", { name: "审阅结论" });
    await user.click(within(reviewEvent).getByRole("button", { name: "通过" }));
    expect(screen.getByText("完整差异已截断，无法确认全部变更，因此不能通过审阅。"))
      .toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeDisabled();

    await user.click(within(reviewEvent).getByRole("button", { name: "要求修改" }));
    expect(screen.getByRole("textbox", { name: "审阅总结" })).toBeRequired();
    expect(screen.getByRole("textbox", { name: "审阅总结" })).toHaveAttribute(
      "placeholder",
      "说明需要修改的内容（必填）"
    );
    expect(screen.getByText("要求修改必须填写审阅总结。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: "审阅总结" }), "Please revisit the visible changes.");
    expect(screen.getByRole("button", { name: "提交审阅" })).toBeEnabled();
  });

  it("clears an existing review when an action returns a different head commit", async () => {
    const user = userEvent.setup();
    git.executeGitHubAction.mockResolvedValueOnce({
      repository,
      pullRequest: {
        ...pullRequestDetail,
        headRefOid: "fedcba9876543210fedcba9876543210fedcba98"
      },
      snapshot
    });
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={{
          ...snapshot,
          staged: 0,
          unstaged: 0,
          additions: 0,
          deletions: 0,
          files: [],
          isClean: true
        }}
        initialView="pullRequests"
        active
      />
    );

    await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
    await screen.findByText("Review the built-in Git UI.");
    await user.click(screen.getByRole("button", { name: "检出" }));
    await user.click(screen.getByRole("button", { name: "确认检出" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      {
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        type: "checkout_pull_request",
        number: pullRequestDetail.number,
        expectedHeadOid: pullRequestDetail.headRefOid,
        expectedState: "open",
        expectedLocalHeadOid: snapshot.head,
        expectedContentRevision: snapshot.contentRevision
      }
    ));
    await waitFor(() => expect(screen.queryByText("Review the built-in Git UI.")).not.toBeInTheDocument());
    expect(await screen.findByRole("button", { name: /#17 Add Git review/ })).toBeInTheDocument();
  });

  it("confirms the explicit head and base before creating a pull request", async () => {
    const user = userEvent.setup();
    render(
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    await screen.findByRole("button", { name: /#17 Add Git review/ });
    await user.click(screen.getByRole("button", { name: "新建拉取请求" }));
    await user.type(screen.getByRole("textbox", { name: "拉取请求标题" }), "A new PR");
    expect(screen.getAllByText("feature/review").length).toBeGreaterThan(0);
    expect(screen.getAllByText("main").length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "创建" }));
    expect(git.executeGitHubAction).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认创建" }));

    await waitFor(() => expect(git.executeGitHubAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      expect.objectContaining({
        type: "create_pull_request",
        title: "A new PR",
        expectedRepository: {
          host: repository.host,
          owner: repository.owner,
          name: repository.name
        },
        expectedViewerLogin: repository.viewerLogin,
        expectedLocalHeadOid: snapshot.head,
        expectedContentRevision: snapshot.contentRevision
      })
    ));
    expect(screen.queryByText("Review the built-in Git UI.")).not.toBeInTheDocument();
  });

  describe("pull request browser entry", () => {
    const linkName = "在浏览器中打开 PR";
    let uninstallInterceptor: (() => void) | null = null;

    afterEach(() => {
      uninstallInterceptor?.();
      uninstallInterceptor = null;
    });

    /** Installs the app-wide interceptor so the anchor is exercised the way it ships. */
    const interceptExternalLinks = () => {
      const opened = vi.fn<(url: string) => Promise<void>>().mockResolvedValue(undefined);
      uninstallInterceptor = installExternalLinkInterceptor(document, opened);
      return opened;
    };

    const panel = () => (
      <GitReviewPanel
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        initialView="pullRequests"
        active
      />
    );

    it("hands the loaded pull request address to the system browser", async () => {
      const user = userEvent.setup();
      const opened = interceptExternalLinks();
      render(panel());

      await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
      await screen.findByText("Review the built-in Git UI.");
      const link = screen.getByRole("link", { name: linkName });
      expect(link).toHaveAttribute("href", pullRequest.url);

      await user.click(link);
      await waitFor(() => expect(opened).toHaveBeenCalledWith(pullRequest.url));
    });

    it("keeps the address returned by the API so an Enterprise host survives", async () => {
      const user = userEvent.setup();
      const enterpriseUrl = "https://git.example-corp.internal/example-org/Mework/pull/17";
      git.getGitHubPullRequests.mockResolvedValue({
        pullRequests: [{ ...pullRequest, url: enterpriseUrl }],
        page: 1,
        pageSize: 30,
        hasMore: false,
        nextPage: null
      });
      git.getGitHubPullRequestDetail.mockResolvedValue({ ...pullRequestDetail, url: enterpriseUrl });
      render(panel());

      await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
      await screen.findByText("Review the built-in Git UI.");
      expect(screen.getByRole("link", { name: linkName })).toHaveAttribute("href", enterpriseUrl);
    });

    it("offers the same entry while the detail is still loading", async () => {
      const user = userEvent.setup();
      git.getGitHubPullRequestDetail.mockReturnValue(new Promise(() => {}));
      render(panel());

      await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
      await screen.findByText("正在读取拉取请求");
      expect(screen.getByRole("link", { name: linkName })).toHaveAttribute("href", pullRequest.url);
    });

    it("offers the same entry when the detail request failed", async () => {
      const user = userEvent.setup();
      git.getGitHubPullRequestDetail.mockRejectedValue(new Error("gh: 拉取请求读取超时"));
      render(panel());

      await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
      await screen.findByText("gh: 拉取请求读取超时");
      expect(screen.getByRole("link", { name: linkName })).toHaveAttribute("href", pullRequest.url);
    });

    it("renders no clickable entry when the address is unusable", async () => {
      const user = userEvent.setup();
      // `javascript:` is the discriminating case: an anchor built without the
      // http(s) check would still be a link here, only a dead and unsafe one.
      for (const url of ["", "javascript:alert(1)"]) {
        git.getGitHubPullRequests.mockResolvedValue({
          pullRequests: [{ ...pullRequest, url }],
          page: 1,
          pageSize: 30,
          hasMore: false,
          nextPage: null
        });
        git.getGitHubPullRequestDetail.mockResolvedValue({ ...pullRequestDetail, url });
        const view = render(panel());

        await user.click(await screen.findByRole("button", { name: /#17 Add Git review/ }));
        await screen.findByText("Review the built-in Git UI.");
        expect(screen.queryByRole("link", { name: linkName }), url).not.toBeInTheDocument();
        expect(screen.queryByLabelText(linkName), url).not.toBeInTheDocument();
        view.unmount();
      }
    });
  });
});
