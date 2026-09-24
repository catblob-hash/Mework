import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import type { GitWorkspaceSnapshot } from "./lib/git";
import type { AppDocument, MachineShells, ShellBackend } from "./types";
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
/** What the host's last probe of each machine found, as `list_machine_shells` answers it. */
const machineShellProbes = vi.hoisted(() => ({ current: {} as Record<string, MachineShells> }));
vi.mock("./lib/machineShells", async (importOriginal) => ({
  ...await importOriginal<typeof import("./lib/machineShells")>(),
  listMachineShells: vi.fn(async () => machineShellProbes.current)
}));

function probe(os: MachineShells["os"], backends: ShellBackend[]): MachineShells {
  return {
    os,
    shells: backends.map((backend) => ({ backend, path: `/usr/bin/${backend}` })),
    probedAt: "2026-09-23T00:00:00Z"
  };
}

/** A project whose second workspace is a WSL distribution's directory. */
function documentWithWslWorkspace(): AppDocument {
  const document = documentWithModel();
  document.workspaces[0].additionalWorkspaces = [{
    machine: { kind: "wsl", distro: "Ubuntu" },
    path: "/home/dev/services"
  }];
  return document;
}

/** Items of an open menu by their visible text, in order. */
function menuTexts(menu: HTMLElement): (string | null)[] {
  return within(menu).getAllByRole("menuitem").map((item) => item.textContent);
}

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

function devbox() {
  return {
    id: "machine-devbox",
    name: "devbox",
    host: "user@devbox.local",
    port: 0,
    identityFile: "",
    createdAt: "2026-01-01T00:00:00.000Z",
    updatedAt: "2026-01-01T00:00:00.000Z"
  };
}

/** The seed project's name, which is what the composer's terminal button names its workspace by. */
function document_workspaceName(): string {
  return "Mework";
}

/** Wait for and return the branch chip in the input header row. */
async function branchChip(branch: string) {
  return await screen.findByRole("button", { name: `分支：${branch}` });
}

describe("composer context chips", () => {
  beforeEach(() => {
    resetAppMocks();
    machineShellProbes.current = {};
    // Git controls require the host runtime; otherwise the input header has no branch chip.
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summaryResult(snapshot("main")));
  });

  it("names the project and the branch above the input", async () => {
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // The run-location chip is gone: where things run is a property of each workspace.
    expect(screen.queryByRole("button", { name: /^运行地点：/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^项目：/ })).toBeInTheDocument();
    // A project with one workspace still gets its workspace chip — its menu is where the
    // workspace's variables and its machine's settings open — but no number: the model is
    // never told one for a lone workspace.
    const workspaceChip = screen.getByRole("button", { name: /^工作区：/ });
    expect(workspaceChip.querySelector(".composer-chip__index")).toBeNull();
    expect(await branchChip("main")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
  });

  it("keeps the chip row outside the input box, immediately above it", async () => {
    render(<App />);
    const textarea = await screen.findByLabelText("向 Agent 发送消息");

    const row = screen.getByRole("button", { name: /^项目：/ }).closest(".composer-context");
    const box = textarea.closest(".composer");
    expect(row).not.toBeNull();
    expect(box).not.toBeNull();
    // Conversation state belongs outside the input box but directly above it.
    expect(box).not.toContainElement(row as HTMLElement);
    expect(row!.closest(".composer-wrap")).toBe(box!.closest(".composer-wrap"));
    expect(row!.compareDocumentPosition(box!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("hides the project chip once the conversation has started", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].contexts = [{
      id: "ctx-started",
      kind: "user",
      content: "已经开始了",
      createdAt: "2026-01-01T00:00:00.000Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await branchChip("main");
    expect(screen.queryByRole("button", { name: /^项目：/ })).not.toBeInTheDocument();
  });

  it("points the Git chip at the workspace picked in a multi-workspace project", async () => {
    const document = documentWithModel();
    document.workspaces[0].additionalWorkspaces = [{ path: "D:/shared/design-tokens" }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    gitMocks.getGitWorkspaceSummary.mockImplementation(async (target: { kind: string; member?: number }) => (
      summaryResult(snapshot(target.kind === "workspace" && target.member === 2 ? "tokens-main" : "main"))
    ));
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await branchChip("main");

    const chip = screen.getByRole("button", { name: /^工作区：/ });
    await user.click(chip);
    const menu = await screen.findByRole("menu", { name: "选择工作区" });
    await user.click(within(menu).getByRole("menuitemradio", { name: /^design-tokens/ }));

    // The second workspace is addressed by its number within the project, never by its path.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary.mock.calls.map(([target]) => target))
      .toContainEqual({ kind: "workspace", workspaceId: document.workspaces[0].id, member: 2 }));
    expect(await branchChip("tokens-main")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "工作区：design-tokens" })).toBeInTheDocument();
    // Worktrees only ever stand in for the project's first workspace.
    expect(screen.queryByRole("checkbox", { name: /工作树/ })).not.toBeInTheDocument();
  });

  it("reads the Git status of a workspace on an SSH machine the way it reads a local one", async () => {
    const document = documentWithModel();
    document.globalSettings.executionEnvironments.sshMachines = [devbox()];
    document.workspaces[0].machine = { kind: "ssh", machineId: "machine-devbox" };
    document.workspaces[0].path = "/home/dev/app";
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // The host reads the status on the machine; the renderer asks for it as for any checkout.
    const chip = await branchChip("main");
    expect(gitMocks.getGitWorkspaceSummary.mock.calls[0]?.[0]).toEqual({
      kind: "conversation",
      conversationId: document.workspaces[0].conversations[0].id
    });
    // Everything else the Git surface does acts on a checkout in this filesystem.
    expect(chip).toBeDisabled();
    expect(screen.queryByRole("checkbox", { name: /工作树/ })).not.toBeInTheDocument();
    const card = await screen.findByRole("complementary", { name: "Git 状态" });
    await user.click(within(card).getByRole("button", { name: "展开 Git 状态卡片" }));
    expect(within(card).getByRole("button", { name: /^变更/ })).toBeDisabled();
    expect(gitMocks.getGitBranches).not.toHaveBeenCalled();
  });

  it("reads the status of a project's further workspace on an SSH machine by its number", async () => {
    const document = documentWithModel();
    document.globalSettings.executionEnvironments.sshMachines = [devbox()];
    document.workspaces[0].additionalWorkspaces = [
      { machine: { kind: "ssh", machineId: "machine-devbox" }, path: "/srv/api" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    gitMocks.getGitWorkspaceSummary.mockImplementation(async (target: { kind: string; member?: number }) => (
      summaryResult(snapshot(target.kind === "workspace" && target.member === 2 ? "api-main" : "main"))
    ));
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    expect(await branchChip("main")).toBeEnabled();

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    const menu = await screen.findByRole("menu", { name: "选择工作区" });
    await user.click(within(menu).getByRole("menuitemradio", { name: /^api/ }));

    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary.mock.calls.map(([target]) => target))
      .toContainEqual({ kind: "workspace", workspaceId: document.workspaces[0].id, member: 2 }));
    expect(await branchChip("api-main")).toBeDisabled();
  });

  it("opens a terminal in the selected workspace with a shell its machine was probed to have", async () => {
    machineShellProbes.current = { "wsl:Ubuntu": probe("wsl", ["bash", "zsh", "sh"]) };
    const document = documentWithWslWorkspace();
    document.globalSettings.executionEnvironments.shellPriority = { wsl: ["zsh"] };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // The terminal button under the input is gone; this one, beside the workspace chip, is it.
    expect(screen.queryAllByRole("button", { name: "终端" })
      .filter((button) => button.classList.contains("composer-option"))).toEqual([]);

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(within(await screen.findByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitemradio", { name: /^services/ }));

    // The distribution's probed shells, in WSL's priority order, then the pane — and nothing in
    // the menu opens a menu of its own.
    await user.click(screen.getByRole("button", { name: "在 services 打开终端" }));
    const menu = await screen.findByRole("menu", { name: "用哪个 shell" });
    await waitFor(() => expect(menuTexts(menu)).toEqual(["zsh", "bash", "sh", "打开终端面板"]));
    expect(within(menu).queryAllByRole("menuitem")
      .filter((item) => item.hasAttribute("aria-haspopup"))).toEqual([]);
    await user.click(within(menu).getByRole("menuitem", { name: "sh" }));

    const conversationId = document.workspaces[0].conversations[0].id;
    const panel = window.document.getElementById(`conversation-terminal-${conversationId}-terminal-1`);
    expect(panel).toHaveAttribute("data-launch", JSON.stringify({ workspace: 2, shell: "sh" }));
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["sh 1"]);
  });

  it("opens the pane on the selected workspace's most preferred shell when it has no terminal", async () => {
    machineShellProbes.current = { "wsl:Ubuntu": probe("wsl", ["bash", "zsh"]) };
    const document = documentWithWslWorkspace();
    document.globalSettings.executionEnvironments.shellPriority = { wsl: ["zsh", "bash"] };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(within(await screen.findByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitemradio", { name: /^services/ }));
    await user.click(screen.getByRole("button", { name: "在 services 打开终端" }));
    const menu = await screen.findByRole("menu", { name: "用哪个 shell" });
    await waitFor(() => expect(menuTexts(menu)[0]).toBe("zsh"));
    await user.click(within(menu).getByRole("menuitem", { name: "打开终端面板" }));

    // No default "Terminal" page: the pane opens on a real shell, in the workspace chosen.
    const conversationId = document.workspaces[0].conversations[0].id;
    expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-1`))
      .toHaveAttribute("data-launch", JSON.stringify({ workspace: 2, shell: "zsh" }));
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["zsh 1"]);

    // The same row now puts the pane away rather than adding a terminal.
    await user.click(screen.getByRole("button", { name: "在 services 打开终端" }));
    await user.click(within(await screen.findByRole("menu", { name: "用哪个 shell" }))
      .getByRole("menuitem", { name: "收起终端面板" }));
    expect(screen.queryAllByRole("tab")).toEqual([]);
    expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-2`)).toBeNull();
  });

  it("offers PowerShell and Git Bash for a workspace on a Windows host", async () => {
    const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("Win32");
    try {
      const user = userEvent.setup();
      render(<App />);
      await screen.findByLabelText("向 Agent 发送消息");

      const project = document_workspaceName();
      await user.click(screen.getByRole("button", { name: `在 ${project} 打开终端` }));
      const menu = await screen.findByRole("menu", { name: "用哪个 shell" });
      expect(menuTexts(menu)).toEqual(["PowerShell", "bash", "打开终端面板"]);
    } finally {
      platform.mockRestore();
    }
  });

  it("gives the top-right terminal button the same shell menu for a lone workspace", async () => {
    const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("MacIntel");
    try {
      const document = documentWithModel();
      runtimeMocks.loadDocument.mockResolvedValue(document);
      const user = userEvent.setup();
      render(<App />);
      await screen.findByLabelText("向 Agent 发送消息");

      const toolbar = window.document.querySelector(".pane-toolbar") as HTMLElement;
      await user.click(within(toolbar).getByRole("button", { name: "终端" }));
      const menu = await screen.findByRole("menu", { name: "新建终端" });
      expect(menuTexts(menu)).toEqual(["zsh", "bash", "显示终端面板"]);
      await user.click(within(menu).getByRole("menuitem", { name: "zsh" }));

      const conversationId = document.workspaces[0].conversations[0].id;
      expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-1`))
        .toHaveAttribute("data-launch", JSON.stringify({ workspace: 1, shell: "zsh" }));

      // A second choice opens a second terminal beside the first, numbered within its shell.
      await user.click(within(toolbar).getByRole("button", { name: "终端" }));
      await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
        .getByRole("menuitem", { name: "bash" }));
      await user.click(within(toolbar).getByRole("button", { name: "终端" }));
      await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
        .getByRole("menuitem", { name: "zsh" }));
      expect(screen.queryAllByRole("tab").map((tab) => tab.textContent)).toEqual(["zsh 1", "bash 1", "zsh 2"]);

      // The pane's + offers the same shells, without the row that shows or hides the pane.
      await user.click(screen.getByRole("button", { name: "新建终端" }));
      const paneMenu = await screen.findByRole("menu", { name: "新建终端" });
      expect(menuTexts(paneMenu)).toEqual(["zsh", "bash"]);
      await user.click(within(paneMenu).getByRole("menuitem", { name: "bash" }));
      expect(screen.queryAllByRole("tab").map((tab) => tab.textContent))
        .toEqual(["zsh 1", "bash 1", "zsh 2", "bash 2"]);
    } finally {
      platform.mockRestore();
    }
  });

  it("makes the top-right terminal button pick a workspace, then one of its machine's shells", async () => {
    const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("MacIntel");
    try {
      machineShellProbes.current = {
        local: probe("macos", ["zsh", "bash", "sh"]),
        "wsl:Ubuntu": probe("wsl", ["bash", "sh"])
      };
      const document = documentWithWslWorkspace();
      runtimeMocks.loadDocument.mockResolvedValue(document);
      const user = userEvent.setup();
      render(<App />);
      await screen.findByLabelText("向 Agent 发送消息");

      const toolbar = window.document.querySelector(".pane-toolbar") as HTMLElement;
      await user.click(within(toolbar).getByRole("button", { name: "终端" }));
      const menu = await screen.findByRole("menu", { name: "新建终端" });
      const workspaces = within(menu).getAllByRole("menuitem")
        .filter((item) => item.getAttribute("aria-haspopup") === "menu");
      expect(workspaces.map((item) => item.textContent)).toEqual(["Mework1", "services2"]);

      // The machine's shells open beside the workspace; nothing is opened until one is picked.
      await user.click(within(menu).getByRole("menuitem", { name: /^services/ }));
      const shells = await screen.findByRole("menu", { name: "services" });
      await waitFor(() => expect(menuTexts(shells)).toEqual(["bash", "sh"]));
      expect(screen.queryAllByRole("tab")).toEqual([]);

      await user.click(within(shells).getByRole("menuitem", { name: "sh" }));
      const conversationId = document.workspaces[0].conversations[0].id;
      expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-1`))
        .toHaveAttribute("data-launch", JSON.stringify({ workspace: 2, shell: "sh" }));
      expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["sh 1"]);

      // This machine's workspace offers this machine's shells, without the sh it cannot run.
      await user.click(within(toolbar).getByRole("button", { name: "终端" }));
      await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
        .getByRole("menuitem", { name: /^Mework/ }));
      expect(menuTexts(await screen.findByRole("menu", { name: "Mework" }))).toEqual(["zsh", "bash"]);
      await user.keyboard("{Escape}");

      // The pane's + asks the same two questions, without the row that shows or hides the pane.
      await user.click(screen.getByRole("button", { name: "新建终端" }));
      const paneMenu = await screen.findByRole("menu", { name: "新建终端" });
      expect(menuTexts(paneMenu)).toEqual(["Mework1", "services2"]);
      await user.click(within(paneMenu).getByRole("menuitem", { name: /^services/ }));
      await user.click(within(await screen.findByRole("menu", { name: "services" }))
        .getByRole("menuitem", { name: "bash" }));
      expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-2`))
        .toHaveAttribute("data-launch", JSON.stringify({ workspace: 2, shell: "bash" }));
      expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["sh 1", "bash 1"]);
    } finally {
      platform.mockRestore();
    }
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

  it("attaches a picked directory as its own numbered chip beside the other location chips", async () => {
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/shared/design-tokens");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const add = screen.getByRole("button", { name: "附加工作区" });
    // The picker button trails the existing chips, so a new workspace lands where it was.
    expect(add.closest(".composer-context")).toBe(
      screen.getByRole("button", { name: /^项目：/ }).closest(".composer-context")
    );
    // The terminal button sits immediately before it.
    const terminal = screen.getByRole("button", { name: /打开终端$/ });
    expect(terminal.compareDocumentPosition(add) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    await user.click(add);
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "本机" }));

    const chip = await screen.findByTitle("D:/shared/design-tokens");
    expect(chip).toHaveTextContent("design-tokens");
    // Workspace 1 is the project's own, so the first attached one is 2.
    expect(chip).toHaveTextContent("2");
    expect(chip.compareDocumentPosition(add) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("browses an SSH machine for a directory instead of opening the native dialog", async () => {
    workspacePickerMocks.listRemoteDirectory.mockResolvedValue({
      path: "/home/dev",
      parent: "/home",
      entries: [{ name: "services", path: "/home/dev/services" }]
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const document = documentWithModel();
    document.globalSettings.executionEnvironments.sshMachines = [devbox()];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "附加工作区" }));
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "devbox" }));

    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    expect(workspacePickerMocks.pickWorkspaceDirectory).not.toHaveBeenCalled();
    await user.click(await within(browser).findByText("services"));
    await user.click(within(browser).getByRole("button", { name: "选择" }));

    expect(await screen.findByTitle("/home/dev/services (SSH: devbox)")).toBeInTheDocument();
  });

  it("numbers attached workspaces after every workspace of the project", async () => {
    const document = documentWithModel();
    document.workspaces[0].additionalWorkspaces = [{ path: "D:/shared/lib" }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/shared/design-tokens");
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "附加工作区" }));
    const menu = await screen.findByRole("menu", { name: "在哪台机器上选目录" });
    await user.click(within(menu).getByRole("menuitem", { name: "本机" }));

    expect(await screen.findByTitle("D:/shared/design-tokens")).toHaveTextContent("3");
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
    document.globalSettings.executionEnvironments.sshMachines = [devbox()];
    return document;
  }

  function lastSaved(): AppDocument | undefined {
    return runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
  }

  it("creates a project on an SSH machine from the project chip, through the remote browser", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(draftDocumentWithDevbox());
    workspacePickerMocks.listRemoteDirectory.mockResolvedValue({
      path: "/home/dev",
      parent: "/home",
      entries: [{ name: "services", path: "/home/dev/services" }]
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "项目：选择项目" }));
    await user.click(within(await screen.findByRole("menu", { name: "选择项目" }))
      .getByRole("menuitem", { name: "新建项目…" }));

    const dialog = await screen.findByRole("dialog", { name: "新建项目" });
    await user.click(within(dialog).getByRole("button", { name: "工作区 1 的机器：本机" }));
    const machines = await screen.findByRole("menu", { name: "选择机器" });
    await user.click(within(machines).getByRole("menuitem", { name: "SSH" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "devbox" }));
    await user.click(within(dialog).getByRole("button", { name: "为工作区 1 选择目录" }));

    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    expect(workspacePickerMocks.pickWorkspaceDirectory).not.toHaveBeenCalled();
    await user.click(await within(browser).findByText("services"));
    await user.click(within(browser).getByRole("button", { name: "选择" }));
    await user.click(within(dialog).getByRole("button", { name: "创建项目" }));

    // The draft moves into the new project, named after its directory.
    expect(await screen.findByRole("button", { name: "项目：services" })).toBeInTheDocument();
    await waitFor(() => {
      const project = lastSaved()?.workspaces.find((entry) => entry.path === "/home/dev/services");
      expect(project?.machine).toEqual({ kind: "ssh", machineId: "machine-devbox" });
      expect(project?.name).toBe("services");
    });
    // The draft asks for the new project's Git status, which the host reads on the machine; the
    // branch can only be switched there.
    const project = lastSaved()?.workspaces.find((entry) => entry.path === "/home/dev/services");
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary.mock.calls.map(([target]) => target))
      .toContainEqual({ kind: "workspace", workspaceId: project?.id }));
    expect(await branchChip("main")).toBeDisabled();
  });

  it("creates a project of several workspaces, each picked on its own machine", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(draftDocumentWithDevbox());
    workspacePickerMocks.pickWorkspaceDirectory
      .mockResolvedValueOnce("D:/projects/app")
      .mockResolvedValueOnce("D:/projects/tokens");
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "项目：选择项目" }));
    await user.click(within(await screen.findByRole("menu", { name: "选择项目" }))
      .getByRole("menuitem", { name: "新建项目…" }));

    const dialog = await screen.findByRole("dialog", { name: "新建项目" });
    await user.click(within(dialog).getByRole("button", { name: "为工作区 1 选择目录" }));
    await user.click(within(dialog).getByRole("button", { name: "添加工作区" }));
    await user.click(await within(dialog).findByRole("button", { name: "为工作区 2 选择目录" }));
    await within(dialog).findByRole("button", { name: "工作区 2：D:/projects/tokens" });
    await user.type(within(dialog).getByLabelText("显示名称"), "平台");
    await user.click(within(dialog).getByRole("button", { name: "创建项目" }));

    expect(await screen.findByRole("button", { name: "项目：平台" })).toBeInTheDocument();
    // Two workspaces: the chip that picks which one Git shows appears.
    expect(screen.getByRole("button", { name: "工作区：app" })).toBeInTheDocument();
    await waitFor(() => {
      const project = lastSaved()?.workspaces.find((entry) => entry.name === "平台");
      expect(project?.path).toBe("D:/projects/app");
      expect(project?.additionalWorkspaces).toEqual([{ path: "D:/projects/tokens" }]);
    });
  });

  it("reuses the project already registered with the same workspaces", async () => {
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
      parent: "/home/dev",
      entries: []
    });
    workspacePickerMocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev/services");
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "项目：选择项目" }));
    await user.click(within(await screen.findByRole("menu", { name: "选择项目" }))
      .getByRole("menuitem", { name: "新建项目…" }));

    const dialog = await screen.findByRole("dialog", { name: "新建项目" });
    await user.click(within(dialog).getByRole("button", { name: "工作区 1 的机器：本机" }));
    await user.click(within(await screen.findByRole("menu", { name: "选择机器" }))
      .getByRole("menuitem", { name: "SSH" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "devbox" }));
    await user.click(within(dialog).getByRole("button", { name: "为工作区 1 选择目录" }));
    const browser = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    await user.click(within(browser).getByRole("button", { name: "选择" }));
    await user.click(within(dialog).getByRole("button", { name: "创建项目" }));

    await screen.findByRole("button", { name: "项目：services" });
    await waitFor(() => {
      expect(lastSaved()?.workspaces.filter((entry) => entry.path === "/home/dev/services")).toHaveLength(1);
    });
  });

  it("adds a workspace to an existing project from the sidebar's project menu", async () => {
    workspacePickerMocks.pickWorkspaceDirectory.mockResolvedValue("D:/shared/lib");
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const name = document.workspaces[0].name;
    await user.click(screen.getByRole("button", { name: `${name} 的更多选项` }));
    await user.click(await screen.findByRole("menuitem", { name: "编辑项目…" }));
    const dialog = await screen.findByRole("dialog", { name: "编辑项目" });
    // The project's first workspace is its identity; it cannot be swapped out here.
    expect(within(dialog).getByRole("button", { name: `工作区 1：${document.workspaces[0].path}` })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "添加工作区" }));
    await user.click(await within(dialog).findByRole("button", { name: "为工作区 2 选择目录" }));
    await within(dialog).findByRole("button", { name: "工作区 2：D:/shared/lib" });
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await screen.findByRole("button", { name: `工作区：${document.workspaces[0].path.split(/[\\/]/).filter(Boolean).at(-1)}` }))
      .toBeInTheDocument();
    await waitFor(() => {
      expect(lastSaved()?.workspaces[0].additionalWorkspaces).toEqual([{ path: "D:/shared/lib" }]);
    });
  });
});

describe("workspace and machine settings from the composer", () => {
  beforeEach(() => {
    resetAppMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summaryResult(snapshot("main")));
  });

  function lastSaved(): AppDocument | undefined {
    return runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
  }

  function documentWithTwoMachines() {
    const document = documentWithModel();
    document.globalSettings.executionEnvironments.sshMachines = [devbox()];
    document.workspaces[0].additionalWorkspaces = [
      { machine: { kind: "ssh", machineId: "machine-devbox" }, path: "/srv/api" }
    ];
    return document;
  }

  it("groups the workspace menu by machine, one gear per machine and one per workspace", async () => {
    const document = documentWithTwoMachines();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    const menu = await screen.findByRole("menu", { name: "选择工作区" });
    expect(Array.from(menu.querySelectorAll(".popover-menu__label")).map((label) => label.textContent))
      .toEqual(["本机", "SSH: devbox"]);
    expect(within(menu).getByRole("button", { name: "本机 的设置" })).toBeInTheDocument();
    expect(within(menu).getByRole("button", { name: "SSH: devbox 的设置" })).toBeInTheDocument();
    expect(within(menu).getByRole("button", { name: "api 的环境变量" })).toBeInTheDocument();
  });

  it("edits a workspace's own variables from its gear, keyed by its machine and path", async () => {
    const document = documentWithTwoMachines();
    const localKey = `local|${document.workspaces[0].path}`;
    document.globalSettings.executionEnvironments.envVars = { [localKey]: { KEEP: "1" } };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(await screen.findByRole("button", { name: "api 的环境变量" }));
    const dialog = await screen.findByRole("dialog", { name: "api 的环境变量" });
    const textarea = within(dialog).getByRole("textbox", { name: "环境变量" });
    expect(textarea).toHaveValue("");
    await user.type(textarea, "API_URL=http://localhost:8080");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(lastSaved()?.globalSettings.executionEnvironments.envVars).toEqual({
      [localKey]: { KEEP: "1" },
      "ssh:machine-devbox|/srv/api": { API_URL: "http://localhost:8080" }
    }));
    expect(screen.queryByRole("dialog", { name: "api 的环境变量" })).not.toBeInTheDocument();
  });

  it("opens a machine's settings from its heading's gear, without environment variables", async () => {
    const document = documentWithTwoMachines();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(await screen.findByRole("button", { name: "SSH: devbox 的设置" }));
    const dialog = await screen.findByRole("dialog", { name: "配置 SSH 机器" });
    expect(within(dialog).getByLabelText("主机")).toHaveValue("user@devbox.local");
    expect(within(dialog).queryByText(/环境变量（/)).not.toBeInTheDocument();
    const name = within(dialog).getByLabelText("名称");
    await user.clear(name);
    await user.type(name, "buildbox");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(lastSaved()?.globalSettings.executionEnvironments.sshMachines)
      .toEqual([expect.objectContaining({ id: "machine-devbox", name: "buildbox" })]));
  });

  it("deleting a machine from its settings drops the variables of its workspaces too", async () => {
    const document = documentWithTwoMachines();
    const localKey = `local|${document.workspaces[0].path}`;
    document.globalSettings.executionEnvironments.envVars = {
      [localKey]: { KEEP: "1" },
      "ssh:machine-devbox|/srv/api": { GONE: "1" }
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(await screen.findByRole("button", { name: "SSH: devbox 的设置" }));
    await user.click(within(await screen.findByRole("dialog", { name: "配置 SSH 机器" }))
      .getByRole("button", { name: "删除" }));
    const confirm = await screen.findByRole("dialog", { name: "删除 SSH 机器“devbox”？" });
    expect(confirm).toHaveTextContent("1 个项目、0 个对话仍在使用这台机器");
    await user.click(within(confirm).getByRole("button", { name: "删除" }));

    await waitFor(() => expect(lastSaved()?.globalSettings.executionEnvironments).toEqual({
      sshMachines: [],
      envVars: { [localKey]: { KEEP: "1" } }
    }));
  });

  it("gives an attached workspace's chip a gear for its own variables", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].attachedWorkspaces = [{ path: "D:/shared/design-tokens" }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // With a second workspace the numbers are stated to the model, so the chip shows one.
    expect(screen.getByRole("button", { name: /^工作区：/ }).querySelector(".composer-chip__index"))
      .toHaveTextContent("1");
    await user.click(screen.getByRole("button", { name: "design-tokens 的环境变量" }));
    const dialog = await screen.findByRole("dialog", { name: "design-tokens 的环境变量" });
    await user.type(within(dialog).getByRole("textbox", { name: "环境变量" }), "TOKENS=1");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(lastSaved()?.globalSettings.executionEnvironments.envVars)
      .toEqual({ "local|D:/shared/design-tokens": { TOKENS: "1" } }));
  });
});
