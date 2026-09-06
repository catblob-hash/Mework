import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type {
  ModelRunRequest
} from "./types";
import { resetAppMocks, browserMocks, documentWithModel, model, openPreviewPage, registerAgentPreview, runtimeMocks } from "./test/appMocks";

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

describe("App model run flow — layout", () => {
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

  it("opens global settings as a main page with shared sidebar width and an in-place return button", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const user = userEvent.setup();
    render(<App />);

    const resizeHandle = await screen.findByRole("separator", { name: "调整侧栏宽度" });
    fireEvent.keyDown(resizeHandle, { key: "ArrowRight", shiftKey: true });
    const shell = resizeHandle.closest(".app-shell") as HTMLElement;
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("288px");

    const settingsButton = screen.getByRole("button", { name: "设置" });
    expect(settingsButton.closest(".sidebar__footer")).not.toBeNull();
    await user.click(settingsButton);

    const settingsSidebar = screen.getByRole("complementary", { name: "全局设置导航" });
    const settingsNavigation = within(settingsSidebar).getByRole("navigation", { name: "全局设置分类" });
    expect(within(settingsNavigation).getByRole("button", { name: "对话预设" })).toHaveClass("settings-nav__item--active");
    expect(within(settingsNavigation).queryByRole("button", { name: "通用" })).not.toBeInTheDocument();
    expect(within(settingsSidebar).queryByRole("navigation", { name: "对话列表" })).not.toBeInTheDocument();
    expect(within(settingsSidebar).getByRole("button", { name: "返回" }).closest(".sidebar__footer")).not.toBeNull();
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe("288px");
    expect(screen.getByRole("region", { name: "全局设置" }).closest(".main-pane")).not.toBeNull();
    expect(screen.queryByRole("dialog", { name: "全局设置" })).not.toBeInTheDocument();

    await user.click(within(settingsNavigation).getByRole("button", { name: "对话预设" }));
    expect(within(settingsNavigation).getByRole("button", { name: "对话预设" })).toHaveClass("settings-nav__item--active");
    await user.click(within(settingsSidebar).getByRole("button", { name: "返回" }));

    expect(screen.getByRole("complementary", { name: "工作区和对话" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "全局设置" })).not.toBeInTheDocument();
  });

  it.each([
    ["narrow", "Home", "220px"],
    ["wide", "End", "420px"]
  ] as const)("returns from a secondary surface to the untouched conversation and %s sidebar layout", async (
    _layout,
    resizeKey,
    expectedSidebarWidth
  ) => {
    const document = documentWithModel();
    document.workspaces[0].conversations.push({
      ...document.workspaces[0].conversations[0],
      id: "conversation-second",
      title: "第二任务"
    });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);

    const resizeHandle = await screen.findByRole("separator", { name: "调整侧栏宽度" });
    const shell = resizeHandle.closest(".app-shell") as HTMLElement;
    fireEvent.keyDown(resizeHandle, { key: resizeKey });
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe(expectedSidebarWidth);

    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "保留这段设置页前的草稿");
    const workspaceSidebar = screen.getByRole("complementary", { name: "工作区和对话" });
    expect(within(workspaceSidebar).getByText("新任务").closest(".conversation-row"))
      .toHaveClass("conversation-row--active");
    expect(within(workspaceSidebar).getByText("第二任务").closest(".conversation-row"))
      .not.toHaveClass("conversation-row--active");
    await user.click(screen.getByRole("button", { name: "设置" }));

    const settingsSidebar = screen.getByRole("complementary", { name: "全局设置导航" });
    expect(within(settingsSidebar).getByRole("navigation", { name: "全局设置分类" })).toBeInTheDocument();
    expect(within(settingsSidebar).getByRole("button", { name: "返回" }).closest(".sidebar__footer")).not.toBeNull();
    expect(within(settingsSidebar).queryByRole("button", { name: "设置" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "全局设置" }).closest(".main-pane")).not.toBeNull();
    expect(screen.queryByLabelText("向 Agent 发送消息")).not.toBeInTheDocument();
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe(expectedSidebarWidth);

    await user.click(within(settingsSidebar).getByRole("button", { name: "返回" }));

    const restoredSidebar = screen.getByRole("complementary", { name: "工作区和对话" });
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("保留这段设置页前的草稿");
    expect(within(restoredSidebar).getByText("新任务").closest(".conversation-row"))
      .toHaveClass("conversation-row--active");
    expect(within(restoredSidebar).getByText("第二任务").closest(".conversation-row"))
      .not.toHaveClass("conversation-row--active");
    expect(shell.style.getPropertyValue("--sidebar-width")).toBe(expectedSidebarWidth);
    expect(screen.queryByRole("region", { name: "全局设置" })).not.toBeInTheDocument();
  });

  it("closes the native preview before showing global settings, and comes back to the conversation", async () => {
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

    await user.click(screen.getByRole("button", { name: "设置" }));

    expect(screen.getByRole("region", { name: "全局设置" })).toBeInTheDocument();
    await waitFor(() => expect(browserMocks.performBrowserAction)
      .toHaveBeenCalledWith(conversationId, "hide", true, expect.any(Number)));

    await user.click(screen.getByRole("button", { name: "返回" }));

    // Leaving settings lands on the conversation, not back on the page it was hiding.
    expect(await screen.findByLabelText("向 Agent 发送消息")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "打开侧边栏" })).not.toBeInTheDocument();
  });

  it("retries a transient native hide with the exact same lifecycle epoch", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
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
    await user.click(screen.getByRole("button", { name: "设置" }));

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

  /** No tabs, no resize handle, no toggle: the task bar is the whole navigation surface now. */
  it("has no right sidebar left to open or size", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    expect(window.document.getElementById("right-sidebar-panel")).toBeNull();
    expect(window.document.querySelector(".right-sidebar")).toBeNull();
    expect(screen.queryByRole("button", { name: "打开侧边栏" })).not.toBeInTheDocument();
    expect(screen.queryByRole("separator", { name: "调整右侧页面宽度" })).not.toBeInTheDocument();
  });

  /** The back arrow is the only way out of a page, and it lands on the conversation. */
  it("returns the message area to the conversation from a preview page", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const user = userEvent.setup();
    render(<App />);

    await registerAgentPreview(user);
    await openPreviewPage(user, /打开“/);
    const composer = screen.getByLabelText("向 Agent 发送消息");
    // The conversation is hidden, not unmounted — reading it at all proves it survived.
    expect(composer.closest(".conversation-pane")).toHaveAttribute("hidden");

    await user.click(screen.getByRole("button", { name: "返回对话" }));

    expect(composer.closest(".conversation-pane")).not.toHaveAttribute("hidden");
  });

});
