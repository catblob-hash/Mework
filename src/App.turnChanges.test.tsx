import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import { CONVERSATION_TURNS_STORAGE_KEY } from "./lib/conversationTurns";
import type { GitFileChange, GitTarget } from "./lib/git";
import { resetAppMocks, documentWithModel, gitMocks, runtimeMocks } from "./test/appMocks";
import type { ContextItem } from "./types";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

afterEach(() => {
  configureI18n("zh-CN");
  window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY);
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
});

const at = "2026-09-25T00:00:00.000Z";

function fileChange(id: string, toolName: string, path: string, created: boolean): ContextItem {
  return {
    id,
    kind: "tool",
    toolName,
    input: toolName === "write" ? { path, content: "x" } : { path, find: "a", replace: "b" },
    result: {
      success: true,
      output: "ok",
      diff: `--- ${created ? "/dev/null" : path}\n+++ ${path}\n@@ -1 +1,2 @@\n-a\n+b\n+c\n`,
      executedAt: at,
      durationMs: 1
    },
    createdAt: at
  };
}

const trackedFile: GitFileChange = {
  path: "src/App.tsx",
  status: "modified",
  staged: false,
  unstaged: true,
  additions: 2,
  deletions: 1
};

/** A conversation whose one turn edited a tracked file and wrote a new one; Git answers for it. */
function stageTurnChanges() {
    const appDocument = documentWithModel();
    const conversation = appDocument.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "ask", kind: "user", content: "改一下", createdAt: at },
      fileChange("edit-app", "edit", "src/App.tsx", false),
      fileChange("write-notes", "write", "notes/new.md", true),
      { id: "done", kind: "assistant", content: "改好了", createdAt: at }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      [conversation.id]: [{
        id: "turn-1",
        requestId: "request-1",
        anchorContextId: "ask",
        modelId: "model",
        startedAt: at,
        endedAt: at,
        durationMs: 1000,
        status: "completed",
        contextIds: ["edit-app", "write-notes", "done"],
        usage: {},
        usageOffset: {},
        usageBaseline: {},
        usageRevisionAtStart: 0,
        segmentCount: 1
      }]
    }));
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const root = appDocument.workspaces[0].path;
    const summary = {
      kind: "snapshot",
      summary: {
        repositoryId: "repository-id",
        worktreeId: "worktree-id",
        repositoryRoot: root,
        worktreeRoot: root,
        branch: "main",
        head: "abc123",
        contentRevision: "content-1",
        summaryRevision: "summary-1",
        upstream: null,
        upstreamTarget: null,
        ahead: 0,
        behind: 0,
        additions: 2,
        deletions: 1,
        staged: 0,
        unstaged: 1,
        untracked: 1,
        conflicted: 0,
        stash: 0,
        changedFiles: 2,
        stageable: 2,
        unstageable: 0,
        remote: null,
        remotes: [],
        gitVersion: "git version 2.50.0",
        detached: false,
        unborn: false,
        operation: null,
        operationRevision: null,
        isClean: false,
        binaryFiles: 0,
        warnings: []
      }
    };
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summary);
    gitMocks.getGitChangePage.mockImplementation((
      _target: GitTarget,
      request: { expectedRevision: string; selectedPath?: string }
    ) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: [trackedFile],
      matchedCount: 1,
      nextCursor: null,
      selection: request.selectedPath === trackedFile.path
        ? { state: "present", file: trackedFile }
        : request.selectedPath ? { state: "missing" } : null
    }));
    gitMocks.getGitDiff.mockResolvedValue({
      patch: "diff --git a/src/App.tsx b/src/App.tsx\n--- a/src/App.tsx\n+++ b/src/App.tsx\n@@ -1 +1,2 @@\n-a\n+b\n+c\n",
      path: null,
      additions: 2,
      deletions: 1,
      binary: false,
      truncated: false,
      files: []
    });
    return { summary };
}

async function expectDiffOpenInReview() {
    const review = await screen.findByRole("region", { name: "审阅" });
    await waitFor(() => expect(
      review.querySelector('[data-diff-file="src/App.tsx"] .diff-viewer__file-toggle')
    ).toHaveAttribute("aria-expanded", "true"));
    return review;
}

describe("App — a turn's changed files", () => {
  beforeEach(resetAppMocks);

  it("opens a tracked change on its diff in the review pane and anything else in the file pane", async () => {
    stageTurnChanges();
    const user = userEvent.setup();
    render(<App />);
    const card = await screen.findByRole("region", { name: "编辑了 2 个文件" });
    expect(within(card).getByText("+4")).toBeInTheDocument();

    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalled());
    await user.click(within(card).getByRole("button", { name: /App\.tsx/ }));
    const review = await expectDiffOpenInReview();
    // Opened just for this file, so its file column starts folded.
    expect(within(review).getByRole("button", { name: "显示文件" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.queryByRole("region", { name: "文件" })).toBeNull();

    // Git does not track the new file, so there is no diff of it to review.
    await user.click(within(card).getByRole("button", { name: /new\.md/ }));
    expect(await screen.findByRole("region", { name: "文件" })).toBeInTheDocument();
    expect(gitMocks.getGitChangePage).toHaveBeenCalledWith(expect.anything(), {
      expectedRevision: "summary-1",
      selectedPath: "notes/new.md",
      limit: 1
    });
  });

  it("asks Git itself when the conversation has no review page yet", async () => {
    const { summary } = stageTurnChanges();
    // The poll's read failed, so no page exists when the row is clicked.
    gitMocks.getGitWorkspaceSummary.mockReset();
    gitMocks.getGitWorkspaceSummary
      .mockRejectedValueOnce(new Error("Git timed out"))
      .mockResolvedValue(summary);
    const user = userEvent.setup();
    render(<App />);
    const card = await screen.findByRole("region", { name: "编辑了 2 个文件" });
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "审阅" })).toBeDisabled();

    await user.click(within(card).getByRole("button", { name: /App\.tsx/ }));
    await expectDiffOpenInReview();
    expect(screen.queryByRole("region", { name: "文件" })).toBeNull();
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledTimes(2);
  });
});
