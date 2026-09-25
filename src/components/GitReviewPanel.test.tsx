import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  GitDiscardPreparation,
  GitDiffResult,
  GitRemote,
  GitWorkspaceSnapshot
} from "../lib/git";
import { gitConversationTarget } from "../lib/git";
import { GitReviewPanel } from "./GitReviewPanel";
import type { SidePaneId } from "../lib/sidePanes";

/**
 * The diff scope lives in the pane title bar's one dropdown, and per-file actions
 * live in each row's `⋮` — so every test that used to click a tab or a hover
 * button goes through one of these.
 */
async function selectDiffScope(name: string): Promise<void> {
  await userEvent.click(screen.getByRole("button", { name: "审阅范围" }));
  await userEvent.click(await screen.findByRole("menuitemradio", { name: new RegExp(`^${name}`) }));
}

function fileRowName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/**
 * A file has two `⋮` — one on its row in the file column and one on its sticky
 * header — so the column's is the one these tests drive: it is there whether or
 * not the file's card is open.
 */
/**
 * Opens one file's card.
 *
 * Every file now draws folded, so a test that reads diff text has to open the
 * file first — the same click a reader makes.
 */
async function expandFileCard(path: string): Promise<void> {
  const card = fileCard(path);
  if (!card) throw new Error(`no card for ${path}`);
  const toggle = card.querySelector(".diff-viewer__file-toggle");
  if (!(toggle instanceof HTMLElement)) throw new Error(`no fold toggle for ${path}`);
  if (toggle.getAttribute("aria-expanded") === "true") return;
  await userEvent.click(toggle);
}

/** One file's card in the continuous diff, by repository-relative path. */
function fileCard(path: string): HTMLElement | null {
  const card = document.querySelector(`[data-diff-file="${path}"]`);
  return card instanceof HTMLElement ? card : null;
}

/**
 * The whole file column, including the filter above the list and the load-more
 * below it — both sit outside the scrolling `<nav>` so they do not scroll away.
 */
function fileColumn(): HTMLElement {
  const column = document.querySelector("[data-diff-tree]");
  if (!(column instanceof HTMLElement)) throw new Error("file column is not rendered");
  return column;
}

function queryFileRow(path: string): HTMLElement | null {
  const list = screen.queryByRole("navigation", { name: "变更文件" });
  return list ? within(list).queryByText(fileRowName(path)) : null;
}

async function openFileMenu(path: string): Promise<void> {
  const list = screen.getByRole("navigation", { name: "变更文件" });
  await userEvent.click(within(list).getByRole("button", { name: `${fileRowName(path)} 的操作` }));
}

/** Opens one file's `⋮` and clicks the row whose label starts with `label`. */
async function clickFileMenuItem(path: string, label: string): Promise<void> {
  await openFileMenu(path);
  await userEvent.click(await screen.findByRole("menuitem", { name: new RegExp(`^${label}`) }));
}

/** The label of one file's `⋮` row, for asserting an armed confirmation. */
async function fileMenuItemLabels(path: string): Promise<string[]> {
  await openFileMenu(path);
  const items = await screen.findAllByRole("menuitem");
  const labels = items.map((item) => item.textContent ?? "");
  await userEvent.keyboard("{Escape}");
  // The menu hands focus back to its trigger on the next frame. Letting that frame pass keeps the
  // refocus from landing after whatever the test clicks next, blurring it — and disarming a
  // confirmation. The trigger may hold the focus already, so waiting for it proves nothing.
  await act(() => new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve())));
  return labels;
}

const git = vi.hoisted(() => ({
  executeGitAction: vi.fn(),
  getGitChangePage: vi.fn(),
  getGitDiff: vi.fn(),
  getGitWorkspaceSummary: vi.fn(),
  prepareGitDiscard: vi.fn()
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

/** Which paths the scope-wide `git diff` carries; a test widens it before rendering. */
let mockScopeDiffPaths: string[] = ["src/App.tsx"];

/** A minimal but real unified diff for one path, so the viewer has hunks to draw. */
function mockFilePatch(path: string): string {
  return [
    `diff --git a/${path} b/${path}`,
    "index 1111111..2222222 100644",
    `--- a/${path}`,
    `+++ b/${path}`,
    "@@ -1 +1 @@",
    `-old ${path}`,
    `+new ${path}`,
    ""
  ].join(String.fromCharCode(10));
}

describe("GitReviewPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockScopeDiffPaths = ["src/App.tsx"];
    git.getGitDiff.mockImplementation((_, request) => Promise.resolve({
      patch: request.path
        ? mockFilePatch(request.path)
        : mockScopeDiffPaths.map(mockFilePatch).join(""),
      path: request.path ?? null,
      additions: 1,
      deletions: 1,
      binary: false,
      truncated: false,
      files: []
    }));
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
        : null
    }));
    git.prepareGitDiscard.mockResolvedValue({
      snapshot,
      targetRevision: "target-revision-1"
    });
  });

  it("keeps mounted inactive pages idle and loads the selected diff when activated", async () => {
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active={false}
      />
    );

    expect(screen.getByRole("button", { name: "审阅范围" })).toBeInTheDocument();
    expect(git.getGitDiff).not.toHaveBeenCalled();

    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );

    // The scope is read as one patch, not one file at a time.
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "working" }
    ));
  });

  it("rereads the scope from the title bar dropdown", async () => {
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "working" }
    ));

    await selectDiffScope("已暂存的变更");
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "staged" }
    ));

    await selectDiffScope("未暂存的变更");
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "unstaged" }
    ));
  });

  it("names a worktree by its real name and opens on everything its branch did since it forked", async () => {
    const worktreeTarget = gitConversationTarget("conversation-1", 2);
    const summarySnapshot = {
      ...snapshot,
      summaryRevision: "summary-worktree",
      changedFiles: 1,
      filesComplete: false,
      files: []
    };
    const committed = {
      path: "src/committed.ts",
      status: "added" as const,
      staged: false,
      unstaged: false,
      additions: 12,
      deletions: 0
    };
    git.getGitChangePage.mockImplementation((_, request) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: request.base ? [committed, ...snapshot.files] : snapshot.files,
      matchedCount: request.base ? 2 : 1,
      nextCursor: null,
      selection: null
    }));
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={worktreeTarget}
        snapshot={summarySnapshot}
        checkout={{ worktreeName: "38b8d1d5", baseBranch: "main", baseOid: "abc1234" }}
        active
      />
    );

    // The refs name where the worktree came from and the worktree itself, no placeholder.
    const scope = screen.getByRole("button", { name: "审阅范围" });
    expect(scope).toHaveTextContent("main");
    expect(scope).toHaveTextContent("38b8d1d5");
    expect(scope).not.toHaveTextContent("工作树");
    await waitFor(() => expect(git.getGitChangePage).toHaveBeenCalledWith(
      worktreeTarget,
      expect.objectContaining({ base: "abc1234" })
    ));
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      worktreeTarget,
      { type: "branch", base: "abc1234" }
    ));
    expect(await within(fileColumn()).findByText("committed.ts")).toBeInTheDocument();

    // The uncommitted changes alone are still one choice away, and say so.
    await selectDiffScope("未提交的变更");
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(worktreeTarget, { type: "working" }));
    expect(screen.getByRole("button", { name: "审阅范围" })).toHaveTextContent("未提交");
  });

  it("gives the title bar to the pages and moves the scope into the pane menu", async () => {
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        pageTabs={<div role="tablist" aria-label="审阅的工作区" />}
        active
      />
    );
    expect(screen.getByRole("tablist", { name: "审阅的工作区" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "审阅范围" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "审阅 设置" }));
    await userEvent.click(await screen.findByRole("menuitemradio", { name: /^已暂存的变更/ }));
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "staged" }
    ));
  });

  it("lists every tracked change once, leaves untracked files out, and offers staging actions", async () => {
    const mixedSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 1,
      unstaged: 2,
      untracked: 1,
      files: [
        { path: "src/staged.ts", status: "modified", staged: true, unstaged: false, additions: 1, deletions: 0 },
        { path: "src/unstaged.ts", status: "modified", staged: false, unstaged: true, additions: 2, deletions: 1 },
        { path: "src/new.ts", status: "untracked", staged: false, unstaged: true, untracked: true, additions: 3, deletions: 0 }
      ]
    };
    mockScopeDiffPaths = mixedSnapshot.files.map((file) => file.path);
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={mixedSnapshot}
        active
      />
    );

    const column = fileColumn();
    await waitFor(() => expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(2));
    expect(queryFileRow("src/staged.ts")).toBeInTheDocument();
    expect(queryFileRow("src/unstaged.ts")).toBeInTheDocument();
    expect(queryFileRow("src/new.ts")).not.toBeInTheDocument();
    expect(within(column).getByText("显示 2/2 个变更文件")).toBeInTheDocument();

    // A staged-only file can be unstaged but not staged again, and an unstaged one
    // is the other way round.
    expect(await fileMenuItemLabels("src/staged.ts"))
      .toContainEqual(expect.stringMatching(/^取消暂存/));
    expect(await fileMenuItemLabels("src/staged.ts"))
      .not.toContainEqual(expect.stringMatching(/^暂存/));
    expect(await fileMenuItemLabels("src/unstaged.ts"))
      .toContainEqual(expect.stringMatching(/^暂存/));
  });

  it("opens and jumps to a file picked in the column only once its patch has arrived", async () => {
    const user = userEvent.setup();
    const lateSnapshot: GitWorkspaceSnapshot = {
      ...snapshot,
      staged: 1,
      unstaged: 1,
      files: [
        { path: "src/App.tsx", status: "modified", staged: true, unstaged: false, additions: 1, deletions: 0 },
        { path: "src/late.ts", status: "modified", staged: false, unstaged: true, additions: 1, deletions: 1 }
      ]
    };
    // The scope-wide read leaves one file out, so picking it has to fetch it.
    mockScopeDiffPaths = ["src/App.tsx"];
    let deliverLatePatch: (() => void) | null = null;
    git.getGitDiff.mockImplementation((_, request) => {
      const result: GitDiffResult = {
        patch: request.path
          ? mockFilePatch(request.path)
          : mockScopeDiffPaths.map(mockFilePatch).join(""),
        path: request.path ?? null,
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      };
      if (request.path !== "src/late.ts") return Promise.resolve(result);
      return new Promise<GitDiffResult>((resolve) => {
        deliverLatePatch = () => resolve(result);
      });
    });
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={lateSnapshot}
        active
      />
    );

    const row = await waitFor(() => {
      const found = queryFileRow("src/late.ts");
      if (!found) throw new Error("no row for src/late.ts");
      return found;
    });
    await user.click(row);

    const card = fileCard("src/late.ts");
    if (!card) throw new Error("no card for src/late.ts");
    const toggle = card.querySelector(".diff-viewer__file-toggle");
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      expect.objectContaining({ path: "src/late.ts" })
    ));
    // Still folded while the patch is in flight, and saying nothing about it.
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(within(card).queryByText("正在读取…")).not.toBeInTheDocument();

    await act(async () => {
      deliverLatePatch?.();
      await Promise.resolve();
    });
    await waitFor(() => expect(toggle).toHaveAttribute("aria-expanded", "true"));
    expect(card.textContent).toContain("new src/late.ts");
  });

  it("reloads an open diff when content changes even if all line counts stay equal", async () => {
    const target = gitConversationTarget("conversation-1");
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={target}
        snapshot={changedSnapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(2));

    // Same revision, fresh object identity: nothing changed, so nothing is re-read.
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-large-changes")}
        snapshot={largeSnapshot}
        active
      />
    );

    const column = fileColumn();
    await waitFor(() => expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(200));
    expect(within(column).getByRole("status")).toHaveTextContent("显示 200/450 个变更文件");
    expect(queryFileRow(paths[199])).toBeInTheDocument();
    expect(queryFileRow(paths[200])).not.toBeInTheDocument();
    expect(git.getGitChangePage).toHaveBeenNthCalledWith(1, gitConversationTarget("conversation-large-changes"), {
      expectedRevision: "summary-large",
      limit: 200
    });

    await user.click(within(column).getByRole("button", { name: "再显示 200 个文件" }));

    await waitFor(() => expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(400));
    expect(within(column).getByRole("status")).toHaveTextContent("显示 400/450 个变更文件");
    expect(queryFileRow(paths[399])).toBeInTheDocument();
    expect(queryFileRow(paths[400])).not.toBeInTheDocument();
    expect(git.getGitChangePage).toHaveBeenNthCalledWith(2, gitConversationTarget("conversation-large-changes"), {
      expectedRevision: "summary-large",
      cursor: "page-2",
      limit: 200,
      selectedPath: paths[0]
    });
    expect(git.executeGitAction).not.toHaveBeenCalled();
  }, 10_000);

  it("opens a file the timeline asked for on its diff, with the file column folded for that visit", async () => {
    const paths = Array.from(
      { length: 250 },
      (_, index) => `src/bulk/file-${String(index).padStart(3, "0")}.ts`
    );
    const wanted = paths[240];
    const { snapshot: largeSnapshot, files } = createPagedChangesSnapshot(paths);
    git.getGitChangePage.mockImplementation((_, request) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: files.slice(0, 200),
      matchedCount: 250,
      nextCursor: "page-2",
      selection: request.selectedPath
        ? { state: "present", file: files.find((file) => file.path === request.selectedPath)! }
        : null
    }));
    const handled = vi.fn();
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-reveal")}
        snapshot={largeSnapshot}
        active
        revealRequest={{ path: wanted, nonce: 7, collapseTree: true }}
        onRevealRequestHandled={handled}
      />
    );

    // Asked for by name with the first page, and listed although it sorts past it.
    await waitFor(() => expect(fileCard(wanted)?.querySelector(".diff-viewer__file-toggle"))
      .toHaveAttribute("aria-expanded", "true"));
    expect(git.getGitChangePage).toHaveBeenCalledWith(gitConversationTarget("conversation-reveal"), {
      expectedRevision: "summary-large",
      limit: 200,
      selectedPath: wanted
    });
    expect(fileCard(paths[0])?.querySelector(".diff-viewer__file-toggle")).toHaveAttribute("aria-expanded", "false");
    expect(handled).toHaveBeenCalledWith(7);
    expect(screen.getByRole("button", { name: "显示文件" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.queryByRole("navigation", { name: "变更文件" })).toBeNull();
    // Folded for this visit only: the reader's own choice for the pane is untouched.
    expect(window.localStorage.getItem("mework.review.showFiles")).toBeNull();
  });

  it("opens a file asked for while the pane is open and leaves its column as it was", async () => {
    const props = {
      paneId: "review" as SidePaneId,
      paneExpanded: false,
      onPaneToggleExpand: () => undefined,
      onPaneFocus: () => undefined,
      onPaneClose: () => undefined,
      target: gitConversationTarget("conversation-open-reveal"),
      snapshot,
      active: true
    };
    const view = render(<GitReviewPanel {...props} />);
    await waitFor(() => expect(queryFileRow("src/App.tsx")).toBeInTheDocument());
    expect(fileCard("src/App.tsx")?.querySelector(".diff-viewer__file-toggle")).toHaveAttribute("aria-expanded", "false");

    view.rerender(<GitReviewPanel {...props} revealRequest={{ path: "src/App.tsx", nonce: 1 }} />);

    await waitFor(() => expect(fileCard("src/App.tsx")?.querySelector(".diff-viewer__file-toggle"))
      .toHaveAttribute("aria-expanded", "true"));
    expect(screen.getByRole("button", { name: "隐藏文件" })).toHaveAttribute("aria-pressed", "true");
  });

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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-filter-changes")}
        snapshot={largeSnapshot}
        active
      />
    );

    const column = fileColumn();
    await waitFor(() => expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(2));
    await user.click(within(column).getByRole("button", { name: "再显示 1 个文件" }));
    await waitFor(() => expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(3));
    await user.click(queryFileRow(otherPaths[0])!);

    fireEvent.change(
      within(column).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "MiXeDCase" } }
    );

    await waitFor(() => {
      expect(column.querySelectorAll(".diff-viewer__tree-file")).toHaveLength(2);
      expect(within(column).getByRole("status")).toHaveTextContent("显示 2/2 个变更文件");
    });
    expect(queryFileRow(matchingPaths[1])).toBeInTheDocument();
    expect(queryFileRow(otherPaths[0])).not.toBeInTheDocument();
    // The filtered-out selection is remembered, so clearing the query can restore it.
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-page-race")}
        snapshot={next.snapshot}
        active
      />
    );
    await waitFor(() => expect(queryFileRow("src/new.ts")).toBeInTheDocument());

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

    expect(queryFileRow("src/new.ts")).toBeInTheDocument();
    expect(queryFileRow("src/old.ts")).not.toBeInTheDocument();
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-selection")}
        snapshot={first.snapshot}
        active
      />
    );
    const column = fileColumn();
    await waitFor(() => expect(queryFileRow("src/match/a.ts")?.closest("button"))
      .toHaveAttribute("aria-current", "true"));

    // "present": the backend answers with the file the selection still points at.
    fireEvent.change(
      within(column).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "match" } }
    );
    await waitFor(() => {
      expect(queryFileRow("src/match/b.ts")).toBeInTheDocument();
      expect(queryFileRow("src/match/a.ts")).not.toBeInTheDocument();
    });

    // "filteredOut": the latent selection is kept, so no visible row takes it.
    fireEvent.change(
      within(column).getByRole("searchbox", { name: "筛选变更文件" }),
      { target: { value: "other" } }
    );
    await waitFor(() => expect(queryFileRow("src/other/c.ts")).toBeInTheDocument());
    expect(queryFileRow("src/other/c.ts")?.closest("button")).not.toHaveAttribute("aria-current");

    // "missing": the selection is gone for good, so the first row takes it.
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
    await waitFor(() => expect(queryFileRow("src/other/c.ts")?.closest("button"))
      .toHaveAttribute("aria-current", "true"));
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-stale-page")}
        snapshot={old.snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await waitFor(() => expect(queryFileRow("src/fresh.ts")).toBeInTheDocument());
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

  it("stages a file through a tagged backend action and publishes its snapshot", async () => {
    const onSnapshotChange = vi.fn();
    const onMutationStart = vi.fn(() => true);
    const onMutationEnd = vi.fn();
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
        onMutationStart={onMutationStart}
        onMutationEnd={onMutationEnd}
      />
    );

    await clickFileMenuItem("src/App.tsx", "暂存");

    await waitFor(() => expect(git.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conversation-1"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(onSnapshotChange).toHaveBeenCalledWith(snapshot);
    expect(onMutationStart).toHaveBeenCalledTimes(1);
    expect(onMutationEnd).toHaveBeenCalledTimes(1);
  });

  it("does not let a diff read from before a mutation overwrite the refreshed diff", async () => {
    let resolveStaleDiff!: (value: GitDiffResult) => void;
    git.getGitDiff
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveStaleDiff = resolve;
      }))
      .mockResolvedValueOnce({
        patch: "diff --git a/src/App.tsx b/src/App.tsx\nindex 1111111..2222222 100644\n--- a/src/App.tsx\n+++ b/src/App.tsx\n@@ -1 +1 @@\n-before\n+fresh-after-mutation",
        path: null,
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      });
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(1));

    await clickFileMenuItem("src/App.tsx", "暂存");
    await waitFor(() => expect(git.getGitDiff).toHaveBeenCalledTimes(2));
    await expandFileCard("src/App.tsx");
    await waitFor(() => expect(view.container).toHaveTextContent("fresh-after-mutation"));

    await act(async () => {
      resolveStaleDiff({
        patch: "diff --git a/src/App.tsx b/src/App.tsx\nindex 1111111..2222222 100644\n--- a/src/App.tsx\n+++ b/src/App.tsx\n@@ -1 +1 @@\n-before\n+stale-before-mutation",
        path: null,
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

  it("says an untracked-only working tree has nothing to review and reads no diff", async () => {
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
    mockScopeDiffPaths = ["draft.txt"];
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={untrackedSnapshot}
        active
      />
    );

    expect(await screen.findByText("没有已跟踪的变更；1 个未跟踪文件不在审阅范围内。"))
      .toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "变更文件" })).not.toBeInTheDocument();
    expect(git.getGitDiff).not.toHaveBeenCalled();
  });

  it("requires a new discard confirmation after the repository content revision changes", async () => {
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={{ ...snapshot, staged: 0, files: [changedFile] }}
        active
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/)));

    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
    // The armed confirmation belonged to the old content revision, so it is gone.
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^丢弃未暂存更改/)));
    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    await waitFor(() => expect(git.prepareGitDiscard).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();

    await clickFileMenuItem("src/App.tsx", "确认丢弃未暂存更改");
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={changedSnapshot}
        active
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/)));
    await clickFileMenuItem("src/App.tsx", "确认丢弃未暂存更改");

    // The second preparation came back with a different target revision, so the
    // confirmation re-arms rather than executing against a tree nobody reviewed.
    await waitFor(() => expect(git.prepareGitDiscard).toHaveBeenCalledTimes(2));
    expect(git.executeGitAction).not.toHaveBeenCalled();
    expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));

    await clickFileMenuItem("src/App.tsx", "确认丢弃未暂存更改");
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
  });

  it("ignores out-of-order discard preparations after the repository changes", async () => {
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={repositoryOneSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={repositoryTwoSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^丢弃未暂存更改/)));
    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");

    await act(async () => {
      resolveNewPreparation({
        snapshot: repositoryTwoSnapshot,
        targetRevision: "target-repo-two"
      });
    });
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));

    await act(async () => {
      resolveOldPreparation({
        snapshot: repositoryOneSnapshot,
        targetRevision: "target-repo-one"
      });
    });
    expect(onSnapshotChange).toHaveBeenCalledTimes(1);
    expect(onSnapshotChange).toHaveBeenCalledWith(repositoryTwoSnapshot);
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));
  });

  it("does not publish or arm a discard preparation from an old conversation", async () => {
    const onSnapshotChange = vi.fn();
    let resolveOldPreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard.mockReturnValueOnce(new Promise((resolve) => {
      resolveOldPreparation = resolve;
    }));
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
    expect(await fileMenuItemLabels("src/App.tsx"))
      .not.toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));
    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    expect(git.prepareGitDiscard).toHaveBeenLastCalledWith(
      gitConversationTarget("conversation-2"),
      ["src/App.tsx"],
      false
    );
  });

  it("does not publish or arm a discard preparation after the panel becomes inactive", async () => {
    const onSnapshotChange = vi.fn();
    let resolvePreparation!: (value: GitDiscardPreparation) => void;
    git.prepareGitDiscard.mockReturnValueOnce(new Promise((resolve) => {
      resolvePreparation = resolve;
    }));
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
    expect(await fileMenuItemLabels("src/App.tsx"))
      .not.toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));
  });

  it("invalidates a deferred discard preparation when the same repository snapshot changes", async () => {
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={repositorySnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
    expect(await fileMenuItemLabels("src/App.tsx"))
      .not.toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));
    expect(onSnapshotChange).toHaveBeenCalledTimes(1);
    expect(onSnapshotChange).toHaveBeenCalledWith(externalSnapshot);
  });

  it("preserves an armed discard when the parent echoes its accepted preparation snapshot", async () => {
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));
    expect(onSnapshotChange).toHaveBeenCalledWith(preparedSnapshot);

    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={preparedSnapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/));

    await clickFileMenuItem("src/App.tsx", "确认丢弃未暂存更改");
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

  it("keeps nested submodule changes visible without offering no-op parent actions", async () => {
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={submoduleSnapshot}
        active
      />
    );

    expect(queryFileRow("vendor/module")).not.toBeNull();
    expect(screen.getByText(/请将子模块目录作为独立工作区处理/)).toBeInTheDocument();
    const submoduleMenu = await fileMenuItemLabels("vendor/module");
    expect(submoduleMenu.some((label) => label.startsWith("暂存"))).toBe(false);
    expect(submoduleMenu.some((label) => label.startsWith("丢弃"))).toBe(false);
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={conflictedSnapshot}
        active
      />
    );

    const operation = screen.getByRole("region", { name: "进行中的 Git 操作" });
    expect(operation).toHaveTextContent("Git 合并 正在进行");
    expect(operation).toHaveTextContent("仍有 1 个冲突");
    expect(within(operation).getByRole("button", { name: "继续" })).toBeDisabled();
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^暂存/));

    await user.click(within(operation).getByRole("button", { name: "中止" }));
    expect(within(operation).getByRole("button", { name: "确认中止" })).toBeInTheDocument();
    const restartedSnapshot = {
      ...conflictedSnapshot,
      operationRevision: "operation-revision-2"
    };
    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
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
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        onSnapshotChange={onSnapshotChange}
      />
    );

    await clickFileMenuItem("src/App.tsx", "暂存");

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
    render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        mutationDisabledReason="模型正在使用工作区"
      />
    );

    expect(screen.getByText("模型正在使用工作区")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "审阅范围" })).toBeEnabled();
    expect(await fileMenuItemLabels("src/App.tsx")).toContainEqual(expect.stringMatching(/^暂存/));
    await clickFileMenuItem("src/App.tsx", "暂存");
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

  it("disarms destructive confirmations whenever workspace mutations become locked", async () => {
    const view = render(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );

    await clickFileMenuItem("src/App.tsx", "丢弃未暂存更改");
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/)));

    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
        mutationDisabledReason="模型正在使用工作区"
      />
    );
    await waitFor(async () => expect(await fileMenuItemLabels("src/App.tsx"))
      .not.toContainEqual(expect.stringMatching(/^确认丢弃未暂存更改/)));

    view.rerender(
      <GitReviewPanel
        paneId={"review" as SidePaneId}
        paneExpanded={false}
        onPaneToggleExpand={() => undefined}
        onPaneFocus={() => undefined}
        onPaneClose={() => undefined}
        target={gitConversationTarget("conversation-1")}
        snapshot={snapshot}
        active
      />
    );
    expect(await fileMenuItemLabels("src/App.tsx"))
      .toContainEqual(expect.stringMatching(/^丢弃未暂存更改/));
    expect(git.executeGitAction).not.toHaveBeenCalled();
  });

});
