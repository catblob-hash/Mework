import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as browserApi from "../lib/browser";
import { configureI18n } from "../i18n";
import {
  BrowserPanel,
  browserMenuRegion,
  browserOverlayRegion,
  normalizeBrowserAddress
} from "./BrowserPanel";

describe("BrowserPanel", () => {
  afterEach(() => {
    // Native browser cleanup dispatches a final menu action, so unmount while API spies are active.
    cleanup();
    vi.restoreAllMocks();
    configureI18n("zh-CN");
    document.documentElement.dataset.theme = "day";
  });

  it("renders English browser controls with the reference night welcome background", () => {
    configureI18n("en-US");
    document.documentElement.dataset.theme = "night";

    render(<BrowserPanel native={false} />);

    expect(screen.getByLabelText("URL or search query")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveClass("browser-panel__welcome");
    expect(screen.getByRole("status")).toHaveAttribute("lang", "en-US");
    expect(screen.getByRole("heading", { name: "Start browsing", level: 2 })).toBeInTheDocument();
    expect(screen.queryByTitle("Browser page")).not.toBeInTheDocument();
  });

  it("renders the localized blank-page welcome with a globe mark", () => {
    render(<BrowserPanel native={false} />);

    expect(screen.getByRole("status")).toHaveClass("browser-panel__welcome");
    expect(screen.getByRole("heading", { name: "开始浏览", level: 2 })).toBeInTheDocument();
    expect(screen.getByText("输入 URL 以打开页面")).toBeInTheDocument();
    expect(document.querySelector(".browser-panel__welcome-mark")).toBeInTheDocument();
    expect(document.querySelector("iframe[srcdoc]")).not.toBeInTheDocument();
  });

  it("normalizes addresses without accepting active-content schemes", () => {
    expect(normalizeBrowserAddress("example.com")).toBe("https://example.com");
    expect(normalizeBrowserAddress("localhost:3000/path")).toBe("http://localhost:3000/path");
    expect(normalizeBrowserAddress("search terms")).toBe("https://www.google.com/search?q=search%20terms");
    expect(() => normalizeBrowserAddress("javascript:alert(1)")).toThrow(/HTTP/);
  });

  it("measures the trusted menu relative to the unchanged Chromium viewport", () => {
    const rect = (left: number, top: number, width: number, height: number) => ({
      left,
      top,
      right: left + width,
      bottom: top + height,
      width,
      height
    });
    expect(browserMenuRegion(
      rect(900, 120, 560, 700),
      rect(900, 120, 560, 44),
      rect(1175, 160, 270, 352),
      17
    )).toEqual({
      generation: 17,
      expanded: true,
      rect: { x: 275, y: -4, width: 270, height: 352 },
      shadow: 18
    });
    expect(browserMenuRegion(
      rect(0, 0, 560, 700),
      rect(0, 0, 560, 44),
      rect(0, 0, 0, 352),
      18
    )).toBeNull();
  });

  it("removes the whole native page region while a trusted browser dialog is open", () => {
    const rect = (left: number, top: number, width: number, height: number) => ({
      left,
      top,
      right: left + width,
      bottom: top + height,
      width,
      height
    });
    expect(browserOverlayRegion(
      rect(900, 120, 560, 700),
      rect(900, 120, 560, 44),
      19
    )).toEqual({
      generation: 19,
      expanded: true,
      rect: { x: 0, y: 0, width: 560, height: 656 },
      shadow: 0
    });
    expect(browserOverlayRegion(
      rect(0, 0, 560, 44),
      rect(0, 0, 560, 44),
      20
    )).toBeNull();
  });

  it("navigates inside the preview page while the sidebar owns close behavior", async () => {
    const user = userEvent.setup();
    render(<BrowserPanel native={false} />);

    const address = screen.getByLabelText("网址或搜索内容");
    await user.clear(address);
    await user.type(address, "example.com{Enter}");
    expect(screen.getByTitle("浏览器页面")).toHaveAttribute("src", "https://example.com");
    expect(screen.getByRole("button", { name: "后退" })).toBeEnabled();

    expect(screen.queryByRole("button", { name: "收起内置浏览器" })).not.toBeInTheDocument();
  });

  it("clears the loading state when preview navigation returns to the local welcome", async () => {
    const user = userEvent.setup();
    render(<BrowserPanel native={false} />);

    const address = screen.getByLabelText("网址或搜索内容");
    await user.clear(address);
    await user.type(address, "example.com{Enter}");
    fireEvent.load(screen.getByTitle("浏览器页面"));
    expect(screen.queryByLabelText("网页加载中")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "后退" }));
    expect(await screen.findByRole("status")).toHaveClass("browser-panel__welcome");
    await waitFor(() => expect(screen.queryByLabelText("网页加载中")).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "刷新页面" }));
    await waitFor(() => expect(screen.queryByLabelText("网页加载中")).not.toBeInTheDocument());
  });

  it("renders React browser chrome without a second empty DOM placeholder", () => {
    const { container } = render(<BrowserPanel native />);
    expect(container.querySelector(".browser-panel__native-surface")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "收起内置浏览器" })).not.toBeInTheDocument();
    expect(screen.queryByText(/正在连接/)).not.toBeInTheDocument();
    expect(screen.queryByTitle("浏览器页面")).not.toBeInTheDocument();
  });

  it("mirrors the real Rust browser session into the dev-browser preview", async () => {
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue({
      hasPage: true,
      open: true,
      loading: false,
      url: "http://127.0.0.1:1430/image-input-browser-e2e",
      title: "Mework Image Input Browser E2E",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    });

    render(<BrowserPanel native={false} browserDev sessionId="conversation-e2e" />);

    expect(await screen.findByTitle("浏览器页面")).toHaveAttribute(
      "src",
      "http://127.0.0.1:1430/image-input-browser-e2e"
    );
    expect(screen.getByLabelText("网址或搜索内容")).toHaveValue(
      "http://127.0.0.1:1430/image-input-browser-e2e"
    );
  });

  it("keeps browser ownership controls out of the chrome while agent activity is active", async () => {
    configureI18n("en-US");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue({
      hasPage: true,
      open: true,
      loading: false,
      url: "https://example.com/",
      title: "Example",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 },
      agentActivity: {
        tool: "playwright",
        source: "CDP",
        active: true,
        updatedAtMs: 123
      },
      control: {
        owner: "agent",
        handoffRequested: false,
        requestedTool: null,
        updatedAtMs: 123
      }
    });

    render(<BrowserPanel native={false} browserDev sessionId="conversation-agent" />);

    await waitFor(() => {
      expect(screen.getByLabelText("URL or search query")).toHaveValue("https://example.com/");
    });
    expect(document.querySelector(".browser-panel__control")).not.toBeInTheDocument();
    expect(screen.queryByText("Agent activity")).not.toBeInTheDocument();
    expect(screen.queryByText("You are in control")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Take over browser control|Return control to Agent/ })).not.toBeInTheDocument();
  });

  it("provides Chromium-style menu focus, keyboard navigation, Escape, and outside dismissal", async () => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);

    render(<BrowserPanel native sessionId="conversation-menu-keyboard" />);

    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    expect(trigger).toHaveAttribute("aria-controls");
    await user.click(trigger);

    const menu = await screen.findByRole("menu", { name: "浏览器菜单" });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(menu).toHaveAttribute("id", trigger.getAttribute("aria-controls"));
    const find = screen.getByRole("menuitem", { name: /在页面中查找/ });
    const print = screen.getByRole("menuitem", { name: /打印/ });
    const clear = screen.getByRole("menuitem", { name: "清除浏览数据" });
    await waitFor(() => expect(find).toHaveFocus());

    await user.keyboard("{ArrowDown}");
    expect(print).toHaveFocus();
    await user.keyboard("{End}");
    expect(clear).toHaveFocus();
    await user.keyboard("{Home}");
    expect(find).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    expect(clear).toHaveFocus();
    await user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
    expect(performAction).toHaveBeenCalledWith(
      "conversation-menu-keyboard",
      "menu",
      expect.objectContaining({ expanded: false, generation: expect.any(Number) })
    );

    await user.click(trigger);
    await screen.findByRole("menu");
    fireEvent.mouseDown(screen.getByLabelText("网址或搜索内容"));
    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  });

  it("cuts the native page region for a trusted overlay owned by the sidebar", async () => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    const overlayRect = {
      left: 1175,
      top: 160,
      right: 1445,
      bottom: 512,
      width: 270,
      height: 352
    };

    const { rerender } = render(
      <BrowserPanel native active sessionId="conversation-outer-overlay" />
    );
    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());

    rerender(
      <BrowserPanel
        native
        active
        sessionId="conversation-outer-overlay"
        trustedOverlayRect={overlayRect}
      />
    );
    // The add menu covers a page the host draws above the renderer, so its exact rectangle has to
    // be removed from the native window region for the card to be visible at all.
    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-outer-overlay",
      "menu",
      expect.objectContaining({
        expanded: true,
        rect: expect.objectContaining({ width: 270, height: 352 }),
        shadow: 18
      })
    ));
    performAction.mockClear();

    rerender(
      <BrowserPanel native active sessionId="conversation-outer-overlay" trustedOverlayRect={null} />
    );
    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-outer-overlay",
      "menu",
      expect.objectContaining({ expanded: false })
    ));

    // Only one native region can be installed, so an outer overlay takes over from the panel's
    // own menu — and closing it leaves the page whole again rather than keeping a stale hole.
    await user.click(trigger);
    await screen.findByRole("menu", { name: "浏览器菜单" });
    performAction.mockClear();
    rerender(
      <BrowserPanel
        native
        active
        sessionId="conversation-outer-overlay"
        trustedOverlayRect={overlayRect}
      />
    );
    await waitFor(() => expect(screen.queryByRole("menu", { name: "浏览器菜单" })).not.toBeInTheDocument());
    rerender(
      <BrowserPanel native active sessionId="conversation-outer-overlay" trustedOverlayRect={null} />
    );
    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-outer-overlay",
      "menu",
      expect.objectContaining({ expanded: false })
    ));
  });

  it("does not let a delayed native open completion undo a newer close", async () => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    let resolveOpen!: (value: browserApi.BrowserStatus) => void;
    const delayedOpen = new Promise<browserApi.BrowserStatus>((resolve) => {
      resolveOpen = resolve;
    });
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_conversationId, action, value) => {
        if (
          action === "menu"
          && typeof value === "object"
          && value !== null
          && value.expanded
          && !value.rect
        ) {
          return delayedOpen;
        }
        return status;
      }
    );

    const { rerender } = render(
      <BrowserPanel native active sessionId="conversation-menu-generation" />
    );
    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-menu-generation",
      "menu",
      expect.objectContaining({ expanded: true, generation: expect.any(Number) })
    ));

    rerender(<BrowserPanel native active={false} sessionId="conversation-menu-generation" />);
    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-menu-generation",
      "menu",
      expect.objectContaining({ expanded: false, generation: expect.any(Number) })
    ));
    const requests = performAction.mock.calls
      .filter(([, action]) => action === "menu")
      .map(([, , value]) => value as browserApi.BrowserMenuRegion);
    expect(requests.at(-1)?.generation).toBeGreaterThan(requests[0].generation);

    await act(async () => {
      resolveOpen(status);
      await delayedOpen;
    });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it.each([
    ["打印", "print"],
    ["显示设备工具栏", "devtools"],
    ["截取屏幕截图", "screenshot"],
    ["挂起此任务页面", "suspend"]
  ])("closes the native menu before running the one-shot %s action", async (label, actionName) => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);

    render(<BrowserPanel native sessionId={`conversation-menu-${actionName}`} />);
    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: new RegExp(label) }));

    await waitFor(() => {
      expect(performAction).toHaveBeenCalledWith(
        `conversation-menu-${actionName}`,
        actionName,
        null
      );
    });
    const relevantCalls = performAction.mock.calls.filter(([, name]) => (
      name === "menu" || name === actionName
    ));
    expect(relevantCalls.slice(0, 3)).toEqual([
      [
        `conversation-menu-${actionName}`,
        "menu",
        expect.objectContaining({ expanded: true, generation: expect.any(Number) })
      ],
      [
        `conversation-menu-${actionName}`,
        "menu",
        expect.objectContaining({ expanded: false, generation: expect.any(Number) })
      ],
      [`conversation-menu-${actionName}`, actionName, null]
    ]);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("requires an explicit second step before clearing the task Chromium profile", async () => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);

    render(<BrowserPanel native sessionId="conversation-clear-data" />);
    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: "清除浏览数据" }));

    expect(performAction).not.toHaveBeenCalledWith(
      "conversation-clear-data",
      "clear_data",
      null
    );
    expect(screen.getByText(/不关页面就退出登录/)).toBeInTheDocument();
    await user.click(screen.getByRole("menuitem", { name: "清除此标签页的全部浏览数据" }));

    await waitFor(() => expect(performAction).toHaveBeenCalledWith(
      "conversation-clear-data",
      "clear_data",
      null
    ));
    const relevantCalls = performAction.mock.calls.filter(([, name]) => (
      name === "menu" || name === "clear_data"
    ));
    expect(relevantCalls.slice(0, 3)).toEqual([
      [
        "conversation-clear-data",
        "menu",
        expect.objectContaining({ expanded: true, generation: expect.any(Number) })
      ],
      [
        "conversation-clear-data",
        "menu",
        expect.objectContaining({ expanded: false, generation: expect.any(Number) })
      ],
      ["conversation-clear-data", "clear_data", null]
    ]);
  });

  it("closes before prompting for find while zoom stays inside the open menu", async () => {
    const user = userEvent.setup();
    const status: browserApi.BrowserStatus = {
      hasPage: true,
      open: true,
      loading: false,
      url: "about:blank",
      title: "新标签页",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      error: null,
      viewport: { width: 560, height: 640 }
    };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    const prompt = vi.spyOn(window, "prompt").mockReturnValue("needle");

    render(<BrowserPanel native sessionId="conversation-menu-find-zoom" />);
    const trigger = screen.getByRole("button", { name: "自定义及控制浏览器" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("button", { name: "放大页面" }));
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(performAction).toHaveBeenCalledWith(
      "conversation-menu-find-zoom",
      "zoom_in",
      null
    );

    await user.click(screen.getByRole("menuitem", { name: /在页面中查找/ }));
    await waitFor(() => expect(prompt).toHaveBeenCalledOnce());
    expect(performAction.mock.calls.slice(-2)).toEqual([
      [
        "conversation-menu-find-zoom",
        "menu",
        expect.objectContaining({ expanded: false, generation: expect.any(Number) })
      ],
      ["conversation-menu-find-zoom", "find", "needle"]
    ]);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });


});
