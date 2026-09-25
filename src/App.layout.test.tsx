import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type {
  ModelRunRequest
} from "./types";
import { resetAppMocks, browserMocks, documentWithModel, model, openPreviewPage, openTasksPane, registerAgentPreview, runtimeMocks, terminalMocks } from "./test/appMocks";

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

/** Runs the test on a Mac host, whose most preferred shell is zsh, until it finishes. */
function onMacHost() {
  const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("MacIntel");
  onTestFinished(() => platform.mockRestore());
}

/** Shows or hides the terminal pane from the composer's terminal button: the row after its shells. */
async function toggleTerminalPane(
  user: ReturnType<typeof userEvent.setup>,
  name: "打开终端面板" | "收起终端面板"
) {
  await user.click(screen.getByRole("button", { name: /打开终端$/ }));
  await user.click(within(await screen.findByRole("menu", { name: "用哪个 shell" }))
    .getByRole("menuitem", { name }));
}

describe("App model run flow — layout", () => {
  /**
   * Two visible conversation rows. Empty workspace slots are hidden, so the first one needs a
   * context of its own before a second row can be told apart from it.
   */
  function documentWithTwoConversations() {
    const appDocument = documentWithModel();
    appDocument.workspaces[0].conversations[0].contexts = [{
      id: "ctx-layout-first",
      kind: "user",
      content: "第一任务已有内容",
      createdAt: "2026-07-20T00:00:00Z"
    }];
    appDocument.workspaces[0].conversations.push({
      ...appDocument.workspaces[0].conversations[0],
      id: "conversation-second",
      title: "第二任务",
      contexts: [{
        id: "ctx-layout-second",
        kind: "user",
        content: "第二任务已有内容",
        createdAt: "2026-07-20T00:00:01Z"
      }]
    });
    return appDocument;
  }

  beforeEach(resetAppMocks);

  it("does not show a persistent saved indicator", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("suppresses native context menus outside editable fields", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const { container } = render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const shell = container.querySelector<HTMLElement>(".app-shell")!;

    const backgroundEvent = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    expect(shell.dispatchEvent(backgroundEvent)).toBe(false);
    expect(backgroundEvent.defaultPrevented).toBe(true);

    const textareaEvent = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    expect(composer.dispatchEvent(textareaEvent)).toBe(true);
    expect(textareaEvent.defaultPrevented).toBe(false);

    const input = document.createElement("input");
    shell.appendChild(input);
    const inputEvent = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    expect(input.dispatchEvent(inputEvent)).toBe(true);
    expect(inputEvent.defaultPrevented).toBe(false);
    input.remove();

    // A terminal is an input surface too: xterm has put the selection into its helper
    // textarea by the time the menu opens, so the native Copy is the terminal's copy.
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    const screenLayer = document.createElement("div");
    screenLayer.className = "xterm-screen";
    xterm.appendChild(screenLayer);
    shell.appendChild(xterm);
    const terminalEvent = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    expect(screenLayer.dispatchEvent(terminalEvent)).toBe(true);
    expect(terminalEvent.defaultPrevented).toBe(false);
    xterm.remove();
  });

  it("keeps designed context menus active", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    fireEvent.contextMenu(container.querySelector(".context-stream__hint")!, {
      clientX: 20,
      clientY: 20
    });
    expect(screen.getByRole("menu", { name: "添加上下文" })).toBeInTheDocument();
  });

  it("says why a save was rejected instead of showing a bare 保存失败", async () => {
    const reason = "命名子代理 mew 引用了不存在的提供商: deepseek";
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    runtimeMocks.saveDocument.mockReset().mockRejectedValue(new Error(reason));

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // Loading schedules a background save for migration results, so no explicit edit is needed.
    const chip = (await screen.findByText(reason)).parentElement!;
    expect(chip).toHaveTextContent("保存失败");
    expect(chip).toHaveAttribute("title", reason);
  });

  it("shrinks the composer after deleting text that expanded it to the maximum height", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);

    const composer = await screen.findByLabelText<HTMLTextAreaElement>("向 Agent 发送消息");
    let contentHeight = 240;
    Object.defineProperty(composer, "scrollHeight", {
      configurable: true,
      get: () => contentHeight
    });

    fireEvent.change(composer, { target: { value: "很长的内容".repeat(80) } });
    expect(composer.style.height).toBe("180px");

    contentHeight = 34;
    fireEvent.change(composer, { target: { value: "" } });
    expect(composer.style.height).toBe("34px");
  });

  it("keeps send enabled and runs with the current contexts when the composer is empty", async () => {
    const document = documentWithModel();
    const contexts = [
      { id: "ctx-user", kind: "user" as const, content: "先检查现有实现", createdAt: "2026-07-20T00:00:00Z" },
      { id: "ctx-assistant", kind: "assistant" as const, content: "已经完成初步检查", createdAt: "2026-07-20T00:00:01Z" }
    ];
    document.workspaces[0].conversations[0].contexts = contexts;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx-continued", kind: "assistant", content: "继续处理当前上下文", createdAt: "2026-07-20T00:00:02Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 8
    });

    const user = userEvent.setup();
    render(<App />);

    const send = await screen.findByRole("button", { name: "发送" });
    expect(send).toBeEnabled();
    await user.click(send);

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(request.contexts).toEqual(contexts);
    expect(request.contexts.filter((context) => context.kind === "user")).toHaveLength(1);
    expect(await screen.findByText("继续处理当前上下文")).toBeInTheDocument();
  });

  it("resizes the workspace sidebar and remembers the chosen width", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);

    const handle = await screen.findByRole("separator", { name: "调整侧栏宽度" });
    const shell = handle.closest(".app-shell") as HTMLElement;
    expect(handle).toHaveAttribute("aria-valuenow", "264");
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("264px");

    fireEvent.pointerDown(handle, { button: 0, isPrimary: true, pointerId: 7, clientX: 264 });
    fireEvent.pointerMove(window, { pointerId: 7, clientX: 344 });
    fireEvent.pointerUp(window, { pointerId: 7, clientX: 344 });

    expect(handle).toHaveAttribute("aria-valuenow", "344");
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("344px");
    expect(window.localStorage.getItem("mework.sidebar-width")).toBe("344");

    fireEvent.keyDown(handle, { key: "End" });
    expect(handle).toHaveAttribute("aria-valuenow", "420");
  });

  /**
   * Global settings is a dialog now, not a surface that takes over the main pane. The category
   * list moved inside it, so the sidebar never leaves its workspace shape and the conversation
   * underneath is still mounted while the dialog is up.
   */
  it("opens global settings as a dialog over the untouched conversation", async () => {
    const appDocument = documentWithTwoConversations();
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    const user = userEvent.setup();
    render(<App />);

    const resizeHandle = await screen.findByRole("separator", { name: "调整侧栏宽度" });
    fireEvent.keyDown(resizeHandle, { key: "ArrowRight", shiftKey: true });
    const shell = resizeHandle.closest(".app-shell") as HTMLElement;
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("288px");

    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "保留这段开设置前的草稿");
    const settingsButton = screen.getByRole("button", { name: "设置" });
    expect(settingsButton.closest(".sidebar__footer")).not.toBeNull();
    await user.click(settingsButton);

    const dialog = screen.getByRole("dialog", { name: "全局设置" });
    const settingsNavigation = within(dialog).getByRole("navigation", { name: "全局设置分类" });
    expect(within(settingsNavigation).getByRole("button", { name: "模型提供商" }))
      .toHaveClass("settings-nav__item--active");
    expect(within(settingsNavigation).queryByRole("button", { name: "对话预设" })).not.toBeInTheDocument();
    expect(within(settingsNavigation).queryByRole("button", { name: "通用" })).not.toBeInTheDocument();

    // The sidebar kept its own navigation, its 设置 button and its width: there is no 返回 state.
    const workspaceSidebar = screen.getByRole("complementary", { name: "项目和对话" });
    expect(screen.queryByRole("complementary", { name: "全局设置导航" })).not.toBeInTheDocument();
    expect(within(workspaceSidebar).getByRole("button", { name: "设置" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "返回" })).not.toBeInTheDocument();
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("288px");
    // A dialog covers the conversation; it does not unmount it.
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("保留这段开设置前的草稿");
    expect(within(workspaceSidebar).getByText("新任务").closest(".conversation-row"))
      .toHaveClass("conversation-row--active");

    await user.click(within(settingsNavigation).getByRole("button", { name: "外观" }));
    expect(within(settingsNavigation).getByRole("button", { name: "外观" }))
      .toHaveClass("settings-nav__item--active");

    await user.click(within(dialog).getByRole("button", { name: "关闭" }));

    expect(screen.queryByRole("dialog", { name: "全局设置" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("保留这段开设置前的草稿");
    expect(within(workspaceSidebar).getByText("新任务").closest(".conversation-row"))
      .toHaveClass("conversation-row--active");
    expect(within(workspaceSidebar).getByText("第二任务").closest(".conversation-row"))
      .not.toHaveClass("conversation-row--active");
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("288px");
  });

  it("publishes the preview rectangle before the native page is created", async () => {
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    await openPreviewPage(user, /打开“/);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledWith(
      conversationId,
      null,
      expect.any(Number)
    ));
    // The geometry barrier: the rectangle is published before the native page is created, so the
    // host never falls through to its right-edge compatibility layout.
    const openEpoch = browserMocks.openBrowser.mock.calls[0]?.[2];
    const boundsBeforeOpen = browserMocks.setBrowserPanelBounds.mock.calls.filter(
      (call: unknown[]) => call[2] === openEpoch
    );
    expect(boundsBeforeOpen.length).toBeGreaterThan(0);
    expect(boundsBeforeOpen[0]?.[1]).toEqual(expect.objectContaining({ visible: true }));
  });

  /**
   * The terminal owns a PTY the pane does not. Unmounting the panel runs its teardown, which
   * reports the session idle, and a still-running shell would vanish from the task rows.
   */
  it("keeps the terminal mounted while its pane is closed and reopens the same element", async () => {
    onMacHost();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const domId = `conversation-terminal-${conversationId}-terminal-1`;
    // Nothing is opened before anyone asks for a terminal.
    expect(window.document.getElementById(domId)).toBeNull();

    await toggleTerminalPane(user, "打开终端面板");
    const opened = window.document.getElementById(domId);
    expect(opened).not.toBeNull();
    expect(opened?.closest(".pane-tiles__tile")).not.toHaveAttribute("hidden");

    await toggleTerminalPane(user, "收起终端面板");
    const hidden = window.document.getElementById(domId);
    expect(hidden).toBe(opened);
    const tile = hidden?.closest(".pane-tiles__tile");
    expect(tile).toHaveAttribute("hidden");
    expect(tile).toHaveAttribute("inert");

    await toggleTerminalPane(user, "打开终端面板");
    expect(window.document.getElementById(domId)).toBe(opened);
    expect(opened?.closest(".pane-tiles__tile")).not.toHaveAttribute("hidden");
    // Reopening shows the terminals there are rather than adding one.
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  /**
   * The pane is a window onto a set of shells the way a browser window is a window onto a set of
   * pages: the tabs own the sessions, closing the window only takes the window away, and the
   * last tab leaving is what takes the pane with it.
   */
  it("gives the terminal pane a tab per shell and ends them one at a time", async () => {
    onMacHost();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const panel = (terminalId: string) => (
      window.document.getElementById(`conversation-terminal-${conversationId}-${terminalId}`)
    );
    const tabNames = () => screen.queryAllByRole("tab").map((element) => element.textContent);
    const closeControl = (name: string) => within(
      screen.getByRole("tab", { name }).closest(".page-tab") as HTMLElement
    ).getByRole("button", { name: "关闭终端" });

    // The pane opening with nothing in it starts the most preferred shell this machine has.
    await toggleTerminalPane(user, "打开终端面板");
    expect(tabNames()).toEqual(["zsh 1"]);
    expect(panel("terminal-1")).toHaveAttribute("data-launch", JSON.stringify({ workspace: 1, shell: "zsh" }));
    await user.click(screen.getByRole("button", { name: "新建终端" }));
    await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
      .getByRole("menuitem", { name: "zsh" }));
    expect(tabNames()).toEqual(["zsh 1", "zsh 2"]);

    // Both shells stay mounted; only the tab in use is on screen, so switching back to the
    // other is a repaint rather than a new shell.
    expect(panel("terminal-2")).not.toBeNull();
    expect(panel("terminal-1")).toHaveAttribute("inert");
    await user.click(screen.getByRole("tab", { name: "zsh 1" }));
    expect(panel("terminal-1")).not.toHaveAttribute("inert");
    expect(panel("terminal-2")).toHaveAttribute("inert");

    // The pane's own × puts the window away and leaves every shell running behind it.
    await user.click(screen.getByRole("button", { name: "关闭面板" }));
    expect(terminalMocks.closeTerminal).not.toHaveBeenCalled();
    expect(tabNames()).toEqual([]);
    expect(panel("terminal-1")).not.toBeNull();

    await toggleTerminalPane(user, "打开终端面板");
    expect(tabNames()).toEqual(["zsh 1", "zsh 2"]);

    // A tab's × ends that shell, and only that one. The survivor keeps its number: each shell
    // counts the terminals it has had, not the ones still open.
    await user.click(closeControl("zsh 2"));
    await waitFor(() => expect(panel("terminal-2")).toBeNull());
    expect(terminalMocks.closeTerminal).toHaveBeenCalledExactlyOnceWith(conversationId, "terminal-2");
    expect(tabNames()).toEqual(["zsh 1"]);

    // The last tab leaving takes the pane with it.
    await user.click(closeControl("zsh 1"));
    await waitFor(() => expect(panel("terminal-1")).toBeNull());
    expect(terminalMocks.closeTerminal).toHaveBeenLastCalledWith(conversationId, "terminal-1");
    expect(screen.queryByRole("button", { name: "关闭面板" })).not.toBeInTheDocument();

    // Asking for the pane again asks for a terminal to put in it, on an id never used before,
    // and the count goes on from where it was.
    await toggleTerminalPane(user, "打开终端面板");
    expect(tabNames()).toEqual(["zsh 3"]);
    expect(panel("terminal-3")).not.toBeNull();
  });

  /**
   * The reference shell folds a terminal away when its shell ends on its own with nothing to
   * report, and keeps a failing one on screen. Only the clean exit reaches here.
   */
  it("folds away the tab whose shell exited cleanly, wherever it was", async () => {
    onMacHost();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const panel = (terminalId: string) => (
      window.document.getElementById(`conversation-terminal-${conversationId}-${terminalId}`)
    );
    // A hidden tab is `inert`, so its control is out of the accessibility tree: reach it by text.
    const exitCleanly = (terminalId: string) => fireEvent.click(
      within(panel(terminalId) as HTMLElement).getByText("模拟终端干净退出")
    );
    const tabNames = () => screen.queryAllByRole("tab").map((element) => element.textContent);

    await toggleTerminalPane(user, "打开终端面板");
    await user.click(screen.getByRole("button", { name: "新建终端" }));
    await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
      .getByRole("menuitem", { name: "zsh" }));
    expect(tabNames()).toEqual(["zsh 1", "zsh 2"]);

    // A shell that ends behind the tab the user is looking at still takes its own tab with it,
    // and nothing else: the pane and the terminal in front of it stay put, under its own name.
    exitCleanly("terminal-1");
    await waitFor(() => expect(panel("terminal-1")).toBeNull());
    expect(tabNames()).toEqual(["zsh 2"]);
    expect(screen.getByRole("button", { name: "关闭面板" })).toBeInTheDocument();

    // The last one ending takes the pane with it.
    exitCleanly("terminal-2");
    await waitFor(() => expect(tabNames()).toEqual([]));
    expect(screen.queryByRole("button", { name: "关闭面板" })).not.toBeInTheDocument();
  });

  /**
   * The native page follows whichever conversation is on screen: it belongs to one conversation's
   * preview pane, and the host shows exactly one page at a time.
   */
  it("withdraws the native preview when another conversation takes the screen, and re-presents it on return", async () => {
    const appDocument = documentWithTwoConversations();
    const conversationId = appDocument.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    await openPreviewPage(user, /打开“/, conversationId);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(1));

    // The row keeps its place across the round trip; its label does not, because sending the
    // prompt that registered the preview retitled the conversation.
    const conversationRow = (index: number) => screen
      .getByRole("complementary", { name: "项目和对话" })
      .querySelectorAll(".conversation-row")[index]
      .querySelector("button")!;

    await user.click(screen.getByText("第二任务"));
    await waitFor(() => expect(browserMocks.performBrowserAction)
      .toHaveBeenCalledWith(conversationId, "hide", null, expect.any(Number)));

    await user.click(conversationRow(0));

    // The pane never closed, so the surface it owns has to come back with it.
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(2));
    expect(browserMocks.openBrowser.mock.calls[1]?.[0]).toBe(conversationId);
  });

  /**
   * jsdom lays nothing out, so the rectangles the native positioning decides on are supplied here.
   * `DOMRect` on purpose: its coordinates live on the prototype, which is exactly what makes a
   * spread of a measured rectangle lose every field but the ones written over it.
   */
  function stubClientRect(
    element: Element,
    rect: { x: number; y: number; width: number; height: number }
  ) {
    const measured = new DOMRect(rect.x, rect.y, rect.width, rect.height);
    Object.defineProperty(element, "getBoundingClientRect", {
      configurable: true,
      value: () => measured
    });
  }

  function pressConversationSettingsShortcut() {
    fireEvent.keyDown(window, { code: "Comma", ctrlKey: true, shiftKey: true });
  }

  /**
   * The settings pane takes a track in the same column as the preview instead of covering it,
   * so the native child webview keeps the screen it was already given.
   */
  it("leaves the native preview presented when the conversation settings pane opens beside it", async () => {
    const appDocument = documentWithModel();
    const conversationId = appDocument.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    const preview = await openPreviewPage(user, /打开“/, conversationId);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(1));

    stubClientRect(preview.querySelector(".side-pane__body")!, { x: 912, y: 88, width: 280, height: 650 });
    browserMocks.performBrowserAction.mockClear();

    pressConversationSettingsShortcut();
    const settings = await screen.findByRole("region", { name: "对话设置" });

    // Both panes hold their own track, and the page was never withdrawn or re-opened.
    expect(settings).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "预览" })).toBeInTheDocument();
    expect(browserMocks.performBrowserAction.mock.calls.filter(
      (call: unknown[]) => call[1] === "hide"
    )).toHaveLength(0);
    expect(browserMocks.openBrowser).toHaveBeenCalledTimes(1);

    await user.click(within(settings).getByRole("button", { name: "关闭面板" }));
    expect(screen.queryByRole("region", { name: "对话设置" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "预览" })).toBeInTheDocument();
  });

  /**
   * Native deserialization rejects a payload missing a coordinate, and the rejection is silent.
   * Nothing is occluded at the top: the browser's toolbar is the pane's title bar, above this box.
   */
  it("publishes every native bounds field for a preview pane", async () => {
    const appDocument = documentWithModel();
    const conversationId = appDocument.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    const preview = await openPreviewPage(user, /打开“/, conversationId);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(1));

    const body = preview.querySelector(".side-pane__body")!;
    stubClientRect(body, { x: 600, y: 88, width: 592, height: 650 });
    browserMocks.setBrowserPanelBounds.mockClear();

    fireEvent.resize(window);

    await waitFor(() => expect(browserMocks.setBrowserPanelBounds.mock.calls.at(-1)?.[1]).toEqual({
      x: 600,
      y: 88,
      width: 592,
      height: 650,
      visible: true
    }));
  });

  /** A hide that fails transiently is re-issued against the page it was already addressing. */
  it("retries a transient native hide with the exact same lifecycle epoch", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithTwoConversations());
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    let hideAttempts = 0;
    browserMocks.performBrowserAction.mockImplementation(async (
      _conversationId: string,
      action: string
    ) => {
      if (action === "hide" && ++hideAttempts === 1) {
        throw new Error("synthetic transient hide failure");
      }
      return {
        hasPage: action !== "close",
        open: false,
        loading: false,
        url: action === "close" ? "" : "about:blank",
        canGoBack: false,
        canGoForward: false,
        zoom: 1,
        viewport: { width: 560, height: 720 }
      };
    });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    await openPreviewPage(user, /打开“/);
    await user.click(screen.getByText("第二任务"));

    const hideCalls = await waitFor(() => {
      const calls = browserMocks.performBrowserAction.mock.calls.filter(
        (call: unknown[]) => call[1] === "hide"
      );
      expect(calls).toHaveLength(2);
      return calls;
    });
    expect(hideCalls[0]?.[3]).toEqual(expect.any(Number));
    expect(hideCalls[1]?.[3]).toBe(hideCalls[0]?.[3]);
    expect(screen.queryByText(/synthetic transient hide failure/)).not.toBeInTheDocument();
  });

  /** No tabs, no resize handle, no toggle: the pane toolbar is the whole navigation surface now. */
  it("has no right sidebar left to open or size", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    expect(window.document.getElementById("right-sidebar-panel")).toBeNull();
    expect(window.document.querySelector(".right-sidebar")).toBeNull();
    expect(screen.queryByRole("button", { name: "打开侧边栏" })).not.toBeInTheDocument();
    expect(screen.queryByRole("separator", { name: "调整右侧页面宽度" })).not.toBeInTheDocument();
  });

  /** A pane is a tile beside the conversation, so the conversation is never taken off screen. */
  it("keeps the conversation on screen beside a preview pane and folds the pane away on close", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    const preview = await openPreviewPage(user, /打开“/);
    const composer = screen.getByLabelText("向 Agent 发送消息");
    // The conversation is a tile now, not a layer something else covers.
    expect(composer.closest(".conversation-pane")).not.toHaveAttribute("hidden");
    expect(composer.closest(".pane-tiles__chat")).not.toBeNull();
    expect(screen.queryByRole("button", { name: "返回对话" })).not.toBeInTheDocument();

    await user.click(within(preview).getByRole("button", { name: "关闭面板" }));

    expect(preview).not.toBeInTheDocument();
    expect(composer.closest(".conversation-pane")).not.toHaveAttribute("hidden");
  });

  /**
   * Panes stack instead of replacing one another, and conversation settings is one of them now,
   * so it takes a track of its own beside whatever is already open.
   */
  it("stacks side panes, conversation settings among them", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const tasks = await openTasksPane(user);

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "文件" }));
    const files = await screen.findByRole("region", { name: "文件" });
    // Both are on screen at once: opening one no longer retires the other.
    expect(tasks).toBeInTheDocument();
    expect(files).toBeInTheDocument();
    expect(screen.getAllByRole("separator", { name: "调整面板高度" })).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "对话设置" }));
    const settings = await screen.findByRole("region", { name: "对话设置" });
    // A third track, not a cover: the two below it keep their boxes.
    expect(tasks).toBeInTheDocument();
    expect(files).toBeInTheDocument();
    expect(screen.getAllByRole("separator", { name: "调整面板高度" })).toHaveLength(2);
    // Its own page list is the pane's navigation, mirroring the global settings page.
    expect(within(settings).getByRole("navigation", { name: "对话设置分类" })).toBeInTheDocument();

    // The same menu row closes it, like every other pane toggle.
    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "对话设置" }));
    expect(screen.queryByRole("region", { name: "对话设置" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "任务" }));
    expect(screen.queryByRole("region", { name: "任务" })).not.toBeInTheDocument();
    expect(files).toBeInTheDocument();
  });

});
