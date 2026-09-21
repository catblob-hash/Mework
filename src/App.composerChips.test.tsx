import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import type { GitWorkspaceSnapshot } from "./lib/git";
import type { AppDocument } from "./types";
import { configureI18n } from "./i18n";
import { documentWithModel, gitMocks, resetAppMocks, runtimeMocks, workspacePickerMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/workspacePicker", async () => (await import("./test/appMockInstances")).workspacePickerMocks);
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

  it("attaches a picked directory as its own numbered chip beside the other run-location chips", async () => {
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/shared/design-tokens");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const add = screen.getByRole("button", { name: "附加工作区" });
    // The picker button trails the existing chips, so a new workspace lands where it was.
    expect(add.closest(".composer-context")).toBe(
      screen.getByRole("button", { name: "运行地点：本机" }).closest(".composer-context")
    );

    await user.click(add);
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "本机" }));

    const chip = await screen.findByTitle("D:/shared/design-tokens");
    expect(chip).toHaveTextContent("design-tokens");
    // Workspace 1 is the conversation's own, so the first attached one is 2.
    expect(chip).toHaveTextContent("2");
    expect(chip.compareDocumentPosition(add) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("browses an SSH machine for a directory instead of opening the native dialog", async () => {
    workspacePickerMocks.listRemoteDirectory.mockResolvedValue({
      path: "/home/dev",
      directories: ["services"],
      hasParent: true
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "运行地点：本机" }));
    const panel = await screen.findByRole("dialog", { name: "运行地点" });
    await user.click(within(panel).getByRole("button", { name: "添加 SSH 机器…" }));
    const machineDialog = await screen.findByRole("dialog", { name: "添加 SSH 机器" });
    await user.type(within(machineDialog).getByLabelText("名称"), "devbox");
    await user.type(within(machineDialog).getByLabelText("主机"), "user@devbox.local");
    await user.click(within(machineDialog).getByRole("button", { name: "保存" }));

    await user.click(screen.getByRole("button", { name: "附加工作区" }));
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "devbox" }));

    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    expect(workspacePickerMocks.pickWorkspaceDirectory).not.toHaveBeenCalled();
    await user.click(await within(browser).findByText("services"));
    await user.click(within(browser).getByRole("button", { name: "选择" }));

    expect(await screen.findByTitle("/home/dev/services (SSH: devbox)")).toBeInTheDocument();
  });

  it("keeps the directory out of the conversation when the picker is cancelled", async () => {
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue(null);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "附加工作区" }));
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "本机" }));
    await waitFor(() => expect(workspacePickerMocks.pickWorkspaceDirectory).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: /^移除工作区：/ })).not.toBeInTheDocument();
  });

  it("detaches a workspace again from its own chip", async () => {
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/shared/design-tokens");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "附加工作区" }));
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "本机" }));
    const remove = await screen.findByRole("button", {
      name: "移除工作区：D:/shared/design-tokens"
    });
    await user.click(remove);

    await waitFor(() => expect(screen.queryByTitle("D:/shared/design-tokens")).not.toBeInTheDocument());
  });

  it("does not offer the button when no host picker can authorize a directory", async () => {
    workspacePickerMocks.hasNativeWorkspacePicker.mockReturnValue(false);
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    expect(screen.queryByRole("button", { name: "附加工作区" })).not.toBeInTheDocument();
  });

  /** A document with an SSH machine registered and no conversation yet, so the composer is a draft. */
  function draftDocumentWithDevbox() {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    document.globalSettings.executionEnvironments.sshMachines = [{
      id: "machine-devbox",
      name: "devbox",
      host: "user@devbox.local",
      port: 0,
      identityFile: "",
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z"
    }];
    return document;
  }

  async function chooseWorkspaceOn(user: ReturnType<typeof userEvent.setup>, machine: string) {
    await user.click(screen.getByRole("button", { name: "工作区：选择工作区" }));
    const menu = await screen.findByRole("menu", { name: "选择工作区" });
    await user.click(within(menu).getByRole("menuitem", { name: "选择工作区" }));
    await user.click(within(menu).getByRole("menuitem", { name: machine }));
  }

  it("chooses the conversation's workspace on an SSH machine through the remote browser", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(draftDocumentWithDevbox());
    workspacePickerMocks.listRemoteDirectory.mockResolvedValue({
      path: "/home/dev",
      directories: ["services"],
      hasParent: true
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });

    await chooseWorkspaceOn(user, "devbox");
    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    expect(workspacePickerMocks.pickWorkspaceDirectory).not.toHaveBeenCalled();
    await user.click(await within(browser).findByText("services"));
    await user.click(within(browser).getByRole("button", { name: "选择" }));

    // The directory becomes the conversation's workspace, named by its last segment and
    // titled with the machine, and the document records where it lives.
    const chip = await screen.findByRole("button", { name: "工作区：services" });
    expect(chip).toHaveAttribute("title", "/home/dev/services (SSH: devbox)");
    // The browser authorizes on the machine it was opened for; the host returns the resolved path.
    expect(workspacePickerMocks.authorizeRemoteWorkspace).toHaveBeenCalledWith(
      { kind: "ssh", machineId: "machine-devbox" },
      expect.any(String)
    );
    await waitFor(() => {
      const saved = runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
      const workspace = saved?.workspaces.find((entry) => entry.path === "/home/dev/services");
      expect(workspace?.machine).toEqual({ kind: "ssh", machineId: "machine-devbox" });
      expect(workspace?.name).toBe("services");
    });
    // No checkout exists on this machine, so nothing asks the host for Git facts and the
    // branch and worktree controls stay away.
    expect(gitMocks.getGitWorkspaceSummary).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: /^分支：/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: /工作树/ })).not.toBeInTheDocument();
  });

  it("chooses the conversation's workspace on this machine through the native dialog", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(draftDocumentWithDevbox());
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/projects/tokens");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });

    await chooseWorkspaceOn(user, "本机");
    const chip = await screen.findByRole("button", { name: "工作区：tokens" });
    expect(chip).toHaveAttribute("title", "D:/projects/tokens");
    expect(workspacePickerMocks.authorizeRemoteWorkspace).not.toHaveBeenCalled();
    await waitFor(() => {
      const saved = runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
      const workspace = saved?.workspaces.find((entry) => entry.path === "D:/projects/tokens");
      expect(workspace).toBeDefined();
      expect(workspace?.machine).toBeUndefined();
    });
  });

  it("reuses the workspace already registered for the same machine and directory", async () => {
    const document = draftDocumentWithDevbox();
    document.workspaces.unshift({
      ...document.workspaces[0],
      id: "ws_services",
      name: "services",
      path: "/home/dev/services",
      machine: { kind: "ssh", machineId: "machine-devbox" },
      conversations: []
    });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    workspacePickerMocks.listRemoteDirectory.mockResolvedValue({
      path: "/home/dev/services",
      directories: [],
      hasParent: true
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });

    await chooseWorkspaceOn(user, "devbox");
    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    await user.click(within(browser).getByRole("button", { name: "选择" }));

    await screen.findByRole("button", { name: "工作区：services" });
    await waitFor(() => {
      const saved = runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
      expect(saved?.workspaces.filter((entry) => entry.path === "/home/dev/services")).toHaveLength(1);
    });
  });
});
