import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import type { GitWorkspaceSnapshot } from "./lib/git";
import { configureI18n } from "./i18n";
import { documentWithModel, gitMocks, resetAppMocks, runtimeMocks } from "./test/appMocks";

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

afterEach(() => configureI18n("zh-CN"));

function snapshot(branch: string): GitWorkspaceSnapshot {
  return {
    repositoryId: "repository-id-workspace",
    worktreeId: "worktree-id-workspace",
    repositoryRoot: "C:/workspace",
    worktreeRoot: "C:/workspace",
    branch,
    head: "head-oid",
    contentRevision: `revision-${branch}`,
    upstream: null,
    ahead: 0,
    behind: 0,
    additions: 0,
    deletions: 0,
    staged: 0,
    unstaged: 0,
    untracked: 0,
    conflicted: 0,
    stash: 0,
    files: [],
    remote: null,
    remotes: [],
    gitVersion: "git version 2.50.0",
    detached: false,
    unborn: false,
    operation: null,
    operationRevision: null,
    isClean: true,
    binaryFiles: 0,
    warnings: []
  } as unknown as GitWorkspaceSnapshot;
}

function summaryResult(value: GitWorkspaceSnapshot) {
  const { files, ...summary } = value;
  return {
    kind: "snapshot" as const,
    summary: {
      ...summary,
      summaryRevision: `summary-${value.contentRevision}`,
      changedFiles: 0,
      stageable: 0,
      unstageable: 0
    }
  };
}

/** Wait for and return the branch chip in the input header row. */
async function branchChip(branch: string) {
  return await screen.findByRole("button", { name: `分支：${branch}` });
}

describe("composer context chips", () => {
  beforeEach(() => {
    resetAppMocks();
    // Git controls require the host runtime; otherwise the input header has no branch chip.
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summaryResult(snapshot("main")));
  });

  it("names the run location, the workspace, and the branch above the input", async () => {
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // The run location remains a menu even with only one available value.
    expect(screen.getByRole("button", { name: "运行地点：本机" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^工作区：/ })).toBeInTheDocument();
    expect(await branchChip("main")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
  });

  it("keeps the chip row outside the input box, immediately above it", async () => {
    render(<App />);
    const textarea = await screen.findByLabelText("向 Agent 发送消息");

    const row = screen.getByRole("button", { name: "运行地点：本机" }).closest(".composer-context");
    const box = textarea.closest(".composer");
    expect(row).not.toBeNull();
    expect(box).not.toBeNull();
    // Conversation state belongs outside the input box but directly above it.
    expect(box).not.toContainElement(row as HTMLElement);
    expect(row!.closest(".composer-wrap")).toBe(box!.closest(".composer-wrap"));
    expect(row!.compareDocumentPosition(box!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("lists only local branches and checks the current one", async () => {
    gitMocks.getGitBranches.mockResolvedValue({
      branches: [
        { name: "main", kind: "local", current: true, head: "a", upstream: null, ahead: 0, behind: 0 },
        { name: "feature/x", kind: "local", current: false, head: "b", upstream: null, ahead: 0, behind: 0 },
        { name: "origin/main", kind: "remote", current: false, head: "c", upstream: null, ahead: 0, behind: 0 }
      ],
      defaultBranch: "main"
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(await branchChip("main"));
    const menu = await screen.findByRole("menu", { name: "切换分支" });
    await waitFor(() => expect(within(menu).getByRole("menuitemradio", { name: "main" })).toBeInTheDocument());
    expect(within(menu).getByRole("menuitemradio", { name: "main" })).toHaveAttribute("aria-checked", "true");
    expect(within(menu).getByRole("menuitemradio", { name: "feature/x" })).toBeInTheDocument();
    // Checking out a remote reference here would create a detached HEAD.
    expect(within(menu).queryByRole("menuitemradio", { name: "origin/main" })).not.toBeInTheDocument();
  });

  it("checks out the branch the user picked", async () => {
    gitMocks.getGitBranches.mockResolvedValue({
      branches: [
        { name: "main", kind: "local", current: true, head: "a", upstream: null, ahead: 0, behind: 0 },
        { name: "feature/x", kind: "local", current: false, head: "b", upstream: null, ahead: 0, behind: 0 }
      ],
      defaultBranch: "main"
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(await branchChip("main"));
    const menu = await screen.findByRole("menu", { name: "切换分支" });
    await waitFor(() => expect(within(menu).getByRole("menuitemradio", { name: "feature/x" })).toBeInTheDocument());
    await user.click(within(menu).getByRole("menuitemradio", { name: "feature/x" }));

    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "conversation" }),
      { type: "checkout", branch: "feature/x" }
    ));
  });

  it("creates an isolated worktree, shows its branch, and releases it again", async () => {
    gitMocks.createConversationWorktree.mockResolvedValue({
      path: "C:/workspace/.mework/worktrees/conversations/conv-1",
      branch: "mework/conv/conv-1",
      baseOid: "abcdef1"
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await branchChip("main");

    await user.click(screen.getByRole("checkbox", { name: /工作树/ }));
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalled());
    // Show the worktree branch because it is where the agent writes.
    expect(await branchChip("mework/conv/conv-1")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /工作树/ })).toBeChecked());

    await user.click(screen.getByRole("checkbox", { name: /工作树/ }));
    await waitFor(() => expect(gitMocks.releaseConversationWorktree).toHaveBeenCalled());
    expect(await branchChip("main")).toBeInTheDocument();
  });

  it("keeps a worktree that still holds uncommitted work and says so", async () => {
    gitMocks.createConversationWorktree.mockResolvedValue({
      path: "C:/workspace/.mework/worktrees/conversations/conv-1",
      branch: "mework/conv/conv-1",
      baseOid: "abcdef1"
    });
    gitMocks.releaseConversationWorktree.mockResolvedValue(false);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await branchChip("main");

    await user.click(screen.getByRole("checkbox", { name: /工作树/ }));
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /工作树/ })).toBeChecked());
    await user.click(screen.getByRole("checkbox", { name: /工作树/ }));

    // Preserve uncommitted work but remove the conversation's worktree association.
    expect(await screen.findByRole("alert")).toHaveTextContent("工作树里还有未提交的改动");
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked());
  });

  it("selects a WSL distro from the run-location picker", async () => {
    runtimeMocks.listWslDistros.mockResolvedValue([
      { name: "Ubuntu", version: 2, isDefault: true },
      { name: "Debian", version: 1, isDefault: false }
    ]);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    // Distributions are enumerated asynchronously after opening the panel.
    await user.click(await within(panel).findByText("Ubuntu"));

    // Selection updates the chip immediately and persists as the conversation run location.
    expect(await screen.findByRole("button", { name: "运行地点：Ubuntu" })).toBeInTheDocument();
  });

  it("edits environment variables behind the per-row gear", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "为 本机 配置环境变量" }));

    const dialog = await screen.findByRole("dialog", { name: /本机 的环境变量/ });
    await user.type(within(dialog).getByPlaceholderText(/API_KEY=value/), "FOO=bar");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    // Environment variables are global assets and must reappear with a count badge.
    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const reopened = await screen.findByRole("dialog", { name: "运行地点" });
    expect(within(reopened).getByTitle("1 个环境变量")).toBeInTheDocument();
    await user.click(within(reopened).getByRole("button", { name: "为 本机 配置环境变量" }));
    expect(
      (await screen.findByPlaceholderText(/API_KEY=value/) as HTMLTextAreaElement).value
    ).toBe("FOO=bar");
  });

  it("rejects invalid variable names instead of silently dropping them", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "为 本机 配置环境变量" }));

    const dialog = await screen.findByRole("dialog", { name: /本机 的环境变量/ });
    await user.type(within(dialog).getByPlaceholderText(/API_KEY=value/), "1BAD=x");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("1BAD");
    // Keep the dialog open for correction instead of silently dropping invalid rows.
    expect(screen.getByRole("dialog", { name: /本机 的环境变量/ })).toBeInTheDocument();
  });

  it("keeps environment values verbatim, including surrounding whitespace", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    let panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "为 本机 配置环境变量" }));

    // `=` and surrounding whitespace are part of the value; trimming breaks format-sensitive tokens.
    const dialog = await screen.findByRole("dialog", { name: /本机 的环境变量/ });
    await user.type(within(dialog).getByPlaceholderText(/API_KEY=value/), "TOKEN= a=b ");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "为 本机 配置环境变量" }));
    expect(
      (await screen.findByPlaceholderText(/API_KEY=value/) as HTMLTextAreaElement).value
    ).toBe("TOKEN= a=b ");
  });

  it("refuses host-reserved variable names instead of letting the document become unsavable", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "为 本机 配置环境变量" }));

    // BASH_ENV runs a script before each visible command. Reject it here because
    // host validation rejects the entire document.
    const dialog = await screen.findByRole("dialog", { name: /本机 的环境变量/ });
    await user.type(within(dialog).getByPlaceholderText(/API_KEY=value/), "BASH_ENV=/tmp/pwn");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("BASH_ENV");
    expect(screen.getByRole("dialog", { name: /本机 的环境变量/ })).toBeInTheDocument();
  });

  it("adds an SSH machine and selects it as the run location", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "添加 SSH 机器…" }));

    const dialog = await screen.findByRole("dialog", { name: "添加 SSH 机器" });
    await user.type(within(dialog).getByLabelText("名称"), "devbox");
    await user.type(within(dialog).getByLabelText("主机"), "user@devbox.local");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const reopened = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(reopened).getByText("devbox"));
    expect(await screen.findByRole("button", { name: "运行地点：devbox" })).toBeInTheDocument();
  });

  it("explains the failure instead of silently leaving the checkbox where it was", async () => {
    gitMocks.createConversationWorktree.mockRejectedValue(new Error("仓库还没有任何提交"));
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await branchChip("main");

    await user.click(screen.getByRole("checkbox", { name: /工作树/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("仓库还没有任何提交");
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
  });
});
