import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import { gitWorkspaceTarget } from "./lib/git";
import * as previewApi from "./lib/preview";
import type { PreviewServerSnapshot } from "./lib/preview";
import type { AppDocument } from "./types";
import {
  browserMocks,
  documentWithModel,
  gitMocks,
  openTasksPane,
  resetAppMocks,
  runtimeMocks,
  terminalMocks
} from "./test/appMocks";

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

const quietReply = {
  contexts: [],
  usage: {},
  model: "test-model",
  providerName: "",
  durationMs: 1
};

/** The fixture's project plus a second one the new task can be moved to. */
function documentWithTwoProjects(): AppDocument {
  const document = documentWithModel();
  document.workspaces.splice(1, 0, {
    ...document.workspaces[0],
    id: "ws_other",
    name: "Other",
    path: "C:/other",
    conversations: []
  });
  return document;
}

const composer = () => screen.getByLabelText("向 Agent 发送消息");
/** Opens the terminal pane from the composer's terminal button: the row after its shells. */
async function openTerminalPane(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: /打开终端$/ }));
  await user.click(within(await screen.findByRole("menu", { name: "用哪个 shell" }))
    .getByRole("menuitem", { name: "打开终端面板" }));
}
/** The first terminal tab's panel, whoever owns it; the owner is part of its DOM id. */
const firstTerminalPanel = () => window.document.querySelector<HTMLElement>(
  'section[id^="conversation-terminal-"][id$="-terminal-1"]'
);
const terminalOwner = (panel: HTMLElement | null) => panel?.id
  .slice("conversation-terminal-".length, -"-terminal-1".length) ?? null;

/**
 * The id the draft's terminals and pages are opened under — the one it will be sent as — read
 * off a terminal opened for it. The pane is put away again; the terminal never starts a shell.
 */
async function draftOwnerId(user: ReturnType<typeof userEvent.setup>) {
  await openTerminalPane(user);
  const ownerId = terminalOwner(firstTerminalPanel());
  await user.click(screen.getByRole("button", { name: "关闭面板" }));
  return ownerId;
}

async function moveDraftTo(user: ReturnType<typeof userEvent.setup>, from: string, to: string) {
  await user.click(screen.getByRole("button", { name: `项目：${from}` }));
  await user.click(within(screen.getByRole("menu", { name: "选择项目" }))
    .getByRole("menuitemradio", { name: to }));
}

/** The pane toolbar's overflow row named `name`. */
async function paneMenuItem(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.click(await screen.findByRole("button", { name: "更多选项" }));
  return await screen.findByRole("menuitemradio", { name });
}

function devServer(serverId: string, sessionId: string | null): PreviewServerSnapshot {
  return {
    serverId,
    name: serverId,
    port: 5173,
    status: "running",
    startedAt: "2026-09-09T00:00:00Z",
    cwd: "C:\\test\\Mework",
    sessionId
  };
}

/**
 * A new task is the renderer's draft until it is sent, yet it has the whole workbench: its
 * terminals and preview page are opened under the id it will be sent as, so nothing moves when it
 * becomes real, and moving it to another project ends them first.
 */
describe("the new task's panes", () => {
  beforeEach(() => {
    resetAppMocks();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithTwoProjects());
    runtimeMocks.runModel.mockResolvedValue(quietReply);
  });

  it("opens its terminal under the id it is sent as, and keeps that shell across the first send", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));

    await openTerminalPane(user);
    const panel = firstTerminalPanel();
    const ownerId = terminalOwner(panel);
    expect(ownerId).toMatch(/^conv_/);
    expect(panel).not.toHaveAttribute("inert");
    fireEvent.click(within(panel!).getByText("模拟终端历史"));

    await user.type(composer(), "终端里已经在跑了");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    // The conversation the draft became is the terminal's owner, and its panel is the same element:
    // nothing was detached, reopened or closed on the way.
    expect(runtimeMocks.runModel.mock.calls[0][0].conversationId).toBe(ownerId);
    expect(firstTerminalPanel()).toBe(panel);
    expect(panel).not.toHaveAttribute("inert");
    expect(terminalMocks.closeTerminal).not.toHaveBeenCalled();
  });

  it("asks before ending a running shell to move the task, and ends it only when told to", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await openTerminalPane(user);
    const ownerId = terminalOwner(firstTerminalPanel());
    fireEvent.click(within(firstTerminalPanel()!).getByText("模拟终端历史"));

    await moveDraftTo(user, "Mework", "Other");
    const prompt = await screen.findByRole("dialog", { name: "把新任务换到别的项目？" });
    expect(prompt).toHaveTextContent("它有 1 个终端还在运行 shell。");

    // Keeping it where it is leaves everything running.
    await user.click(within(prompt).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("dialog", { name: "把新任务换到别的项目？" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "项目：Mework" })).toBeInTheDocument();
    expect(terminalMocks.closeTerminal).not.toHaveBeenCalled();
    expect(firstTerminalPanel()).not.toHaveAttribute("inert");

    await moveDraftTo(user, "Mework", "Other");
    await user.click(within(await screen.findByRole("dialog", { name: "把新任务换到别的项目？" }))
      .getByRole("button", { name: "结束并换项目" }));

    // The shell ends, its pane goes with it, and the next one is opened under a fresh id: a shell
    // from one project is never handed to a conversation in another.
    await waitFor(() => expect(terminalMocks.closeTerminal).toHaveBeenCalledWith(ownerId, "terminal-1"));
    expect(await screen.findByRole("button", { name: "项目：Other" })).toBeInTheDocument();
    expect(screen.queryAllByRole("tab")).toEqual([]);
    expect(firstTerminalPanel()).toBeNull();
    await openTerminalPane(user);
    const nextOwner = terminalOwner(firstTerminalPanel());
    expect(nextOwner).toMatch(/^conv_/);
    expect(nextOwner).not.toBe(ownerId);
    expect(composer()).toBeInTheDocument();

    // Sending lands it in the new project under that fresh id.
    await user.type(composer(), "换好了");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel.mock.calls[0][0].conversationId).toBe(nextOwner);
  });

  it("moves without asking when no terminal has a shell, still putting its panes away", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await openTerminalPane(user);
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await moveDraftTo(user, "Mework", "Other");

    expect(await screen.findByRole("button", { name: "项目：Other" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "把新任务换到别的项目？" })).not.toBeInTheDocument();
    expect(screen.queryAllByRole("tab")).toEqual([]);
  });

  it("opens the tasks pane for a new task", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));

    expect(await openTasksPane(user)).toBeInTheDocument();
  });

  it("opens the files and outgoing-requests panes for a new task", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));

    const files = await paneMenuItem(user, "文件");
    expect(files).toBeEnabled();
    await user.click(files);
    expect(await screen.findByRole("region", { name: "文件" })).toBeInTheDocument();

    const history = await paneMenuItem(user, "发出的请求");
    expect(history).toBeEnabled();
    await user.click(history);
    const ledger = await screen.findByRole("region", { name: "发出的请求" });
    // Nothing has been sent yet, so the ledger is simply empty.
    expect(await within(ledger).findByText(/这个对话还没有记录到发出去的请求/)).toBeInTheDocument();
  });

  it("keeps the files pane closed to a new task aimed at no project, which has no directory yet", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await moveDraftTo(user, "Mework", "临时项目");
    await screen.findByRole("button", { name: "项目：临时项目" });

    expect(await paneMenuItem(user, "文件")).toBeDisabled();
    expect(screen.getByRole("menuitemradio", { name: "发出的请求" })).toBeEnabled();
  });
});

describe("the new task's host-backed panes", () => {
  beforeEach(() => {
    resetAppMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(documentWithTwoProjects());
    runtimeMocks.runModel.mockResolvedValue(quietReply);
  });
  afterEach(() => vi.restoreAllMocks());

  it("asks about the shells the host still runs for it, including ones not on screen", async () => {
    terminalMocks.liveTerminalCount.mockResolvedValue(2);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const ownerId = await draftOwnerId(user);

    await moveDraftTo(user, "Mework", "Other");

    const prompt = await screen.findByRole("dialog", { name: "把新任务换到别的项目？" });
    expect(prompt).toHaveTextContent("它有 2 个终端还在运行 shell。");
    expect(terminalMocks.liveTerminalCount).toHaveBeenCalledWith(ownerId);
  });

  it("reads Git again for the project it moves to", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary)
      .toHaveBeenCalledWith(gitWorkspaceTarget("ws_mework"), undefined));

    await moveDraftTo(user, "Mework", "Other");

    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary)
      .toHaveBeenCalledWith(gitWorkspaceTarget("ws_other"), undefined));
  });

  it("opens its preview page under the id it is sent as, and closes it when the task moves", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const ownerId = await draftOwnerId(user);

    const toolbar = window.document.querySelector(".pane-toolbar") as HTMLElement;
    await user.click(within(toolbar).getByRole("button", { name: "预览" }));
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalled());
    expect(browserMocks.openBrowser.mock.calls[0]?.[0]).toBe(ownerId);

    await moveDraftTo(user, "Mework", "Other");

    await waitFor(() => expect(browserMocks.closeBrowserSession)
      .toHaveBeenCalledWith(ownerId, expect.any(Number)));
  });

  it("starts its dev servers under the id it is sent as", async () => {
    vi.spyOn(previewApi, "listPreviewConfigurations").mockResolvedValue({
      launchJsonPath: "C:\\test\\Mework\\.mework\\launch.json",
      servers: [{ name: "web", command: "npm", args: ["run", "dev"], cwd: "C:\\test\\Mework", port: 5173 }],
      malformed: []
    });
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);
    const start = vi.spyOn(previewApi, "startPreviewServer")
      .mockResolvedValue({ server: devServer("srv-web", null), reused: false });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const ownerId = await draftOwnerId(user);

    const toolbar = window.document.querySelector(".pane-toolbar") as HTMLElement;
    await user.click(within(toolbar).getByRole("button", { name: "预览" }));
    const trigger = await screen.findByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(within(await screen.findByRole("menu", { name: "浏览器菜单" }))
      .getByRole("menuitemradio", { name: "运行 web" }));

    // Addressed like a terminal: the id it will be sent as, the project it is aimed at, and the
    // server is that id's own.
    await waitFor(() => expect(start)
      .toHaveBeenCalledWith({ conversationId: ownerId, draftWorkspaceId: "ws_mework" }, "web"));
  });

  it("asks about the dev servers it started, and stops only those when it moves", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const ownerId = (await draftOwnerId(user))!;
    const list = vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([
      devServer("srv-mine", ownerId),
      devServer("srv-shared", null),
      devServer("srv-theirs", "conv_other")
    ]);
    const stop = vi.spyOn(previewApi, "stopPreviewServer").mockResolvedValue(true);

    await moveDraftTo(user, "Mework", "Other");

    const prompt = await screen.findByRole("dialog", { name: "把新任务换到别的项目？" });
    expect(prompt).toHaveTextContent("它启动的 1 个开发服务器还在运行。");
    expect(prompt).not.toHaveTextContent("终端还在运行 shell");
    expect(list).toHaveBeenCalledWith({ conversationId: ownerId, draftWorkspaceId: "ws_mework" });
    expect(stop).not.toHaveBeenCalled();

    await user.click(within(prompt).getByRole("button", { name: "结束并换项目" }));

    // A server nobody owns, or another conversation's, is the project's to keep.
    await waitFor(() => expect(stop).toHaveBeenCalledWith("srv-mine"));
    expect(stop).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole("button", { name: "项目：Other" })).toBeInTheDocument();
  });
});
