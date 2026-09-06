import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({ invoke: vi.fn() }));
const rendererMountMocks = vi.hoisted(() => ({
  authority: vi.fn()
}));

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@tauri-apps/api/core")>()),
  invoke: tauriMocks.invoke
}));

vi.mock("./browserRendererMount", () => ({
  browserRendererMutationAuthority: rendererMountMocks.authority
}));

import {
  closeBrowserSession,
  getBrowserStatus,
  navigateBrowser,
  openBrowser,
  performBrowserAction,
  setBrowserPanelBounds,
  type BrowserStatus
} from "./browser";

const rendererAuthority = {
  rendererMountId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  rendererMountGeneration: 27
};

const status: BrowserStatus = {
  hasPage: true,
  open: true,
  loading: false,
  url: "https://example.com/",
  title: "Example",
  canGoBack: false,
  canGoForward: false,
  zoom: 1,
  viewport: { width: 1200, height: 742 }
};

const closeDisposition = {
  status: "cleanupPending" as const,
  intentAccepted: true,
  cleanupComplete: false,
  surfaceHidden: true,
  errorCode: "importDrainTimeout" as const,
  message: "浏览器已标记关闭并隐藏，但资料导入尚未安全结束；请稍后重试清理"
};

describe("browser runtime bridge", () => {
  beforeEach(() => {
    tauriMocks.invoke.mockReset();
    rendererMountMocks.authority.mockReset();
    rendererMountMocks.authority.mockResolvedValue(rendererAuthority);
  });

  afterEach(() => {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    vi.restoreAllMocks();
  });

  it("opens a regular tab only as the browser-preview fallback", async () => {
    const open = vi.spyOn(window, "open").mockReturnValue({} as Window);

    await expect(openBrowser("preview-session", " https://example.com/ ")).resolves.toMatchObject({
      hasPage: true,
      open: true,
      url: "https://example.com/",
      viewport: { width: 1200, height: 742 }
    });
    expect(open).toHaveBeenCalledWith("https://example.com/", "_blank", "noopener,noreferrer");
    expect(rendererMountMocks.authority).not.toHaveBeenCalled();
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
  });

  it("uses the native commands for desktop navigation and controls", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockResolvedValue(status);

    await expect(openBrowser("conversation-one", null, 10_240)).resolves.toEqual(status);
    await expect(getBrowserStatus("conversation-one")).resolves.toEqual(status);
    await expect(setBrowserPanelBounds("conversation-one", {
      x: 700,
      y: 0,
      width: 500,
      height: 800,
      visible: true,
      occludedTop: 80
    }, 10_240)).resolves.toEqual(status);
    await expect(navigateBrowser("conversation-one", "https://example.com/")).resolves.toEqual(status);
    await expect(performBrowserAction("conversation-one", "zoom_in")).resolves.toEqual(status);
    await expect(performBrowserAction("conversation-one", "zoom", 1.25)).resolves.toEqual(status);
    await expect(performBrowserAction(
      "conversation-one",
      "close",
      null,
      10_241
    )).resolves.toEqual(status);

    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(1, "browser_open", {
      sessionId: "conversation-one",
      url: null,
      lifecycleEpoch: 10_240,
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(2, "browser_status", {
      sessionId: "conversation-one",
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(3, "browser_set_panel_bounds", {
      sessionId: "conversation-one",
      bounds: { x: 700, y: 0, width: 500, height: 800, visible: true, occludedTop: 80 },
      lifecycleEpoch: 10_240,
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(4, "browser_navigate", {
      sessionId: "conversation-one",
      url: "https://example.com/",
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(5, "browser_action", {
      sessionId: "conversation-one",
      action: "zoom_in",
      value: null,
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(6, "browser_action", {
      sessionId: "conversation-one",
      action: "zoom",
      value: 1.25,
      ...rendererAuthority
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(7, "browser_action", {
      sessionId: "conversation-one",
      action: "close",
      value: null,
      lifecycleEpoch: 10_241,
      ...rendererAuthority
    });
  });

  it("attaches the exact renderer mount authority to the full native browser IPC matrix", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockResolvedValue(status);

    await Promise.all([
      openBrowser("conversation-one", null, 20_000),
      closeBrowserSession("conversation-one", 20_001),
      getBrowserStatus("conversation-one"),
      setBrowserPanelBounds("conversation-one", {
        x: 0,
        y: 0,
        width: 500,
        height: 800,
        visible: true
      }, 20_000),
      navigateBrowser("conversation-one", "https://example.com/"),
      performBrowserAction("conversation-one", "hide", null, 20_002)
    ]);

    const expectedCommands = [
      "browser_open",
      "browser_close",
      "browser_status",
      "browser_set_panel_bounds",
      "browser_navigate",
      "browser_action"
    ];
    expect(tauriMocks.invoke.mock.calls.map(([command]) => command).sort()).toEqual(
      [...expectedCommands].sort()
    );
    for (const [command, args] of tauriMocks.invoke.mock.calls) {
      expect(expectedCommands).toContain(command);
      expect(args).toMatchObject(rendererAuthority);
      expect(args).toHaveProperty("rendererMountId", rendererAuthority.rendererMountId);
      expect(args).toHaveProperty(
        "rendererMountGeneration",
        rendererAuthority.rendererMountGeneration
      );
    }
  });

  it("does not invoke a browser target after stale renderer authority fails closed", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    rendererMountMocks.authority.mockRejectedValue(
      new Error("浏览器渲染器写入权限已失效")
    );
    tauriMocks.invoke.mockRejectedValue(new Error("native-secret-target-error"));

    const results = await Promise.allSettled([
      openBrowser("conversation-one", null, 30_000),
      navigateBrowser("conversation-one", "https://example.com/")
    ]);

    expect(results).toHaveLength(2);
    for (const result of results) {
      expect(result.status).toBe("rejected");
      if (result.status === "rejected") {
        expect(String(result.reason)).toContain("写入权限已失效");
        expect(String(result.reason)).not.toContain("native-secret");
      }
    }
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
  });

  it("rejects invalid or misplaced lifecycle epochs before IPC", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });

    await expect(openBrowser("conversation-one"))
      .rejects.toThrow("需要可信 UI");
    await expect(performBrowserAction("conversation-one", "close"))
      .rejects.toThrow("需要可信 UI");
    await expect(openBrowser("conversation-one", null, 0))
      .rejects.toThrow("正的安全整数");
    await expect(closeBrowserSession("conversation-one", 0))
      .rejects.toThrow("正的安全整数");
    await expect(setBrowserPanelBounds("conversation-one", {
      x: 0,
      y: 0,
      width: 1,
      height: 1,
      visible: true
    }, 0)).rejects.toThrow("正的安全整数");
    await expect(performBrowserAction("conversation-one", "zoom", 1.25, 10))
      .rejects.toThrow("只能用于收起或关闭操作");
    expect(rendererMountMocks.authority).not.toHaveBeenCalled();
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
  });

  it("returns the structured close disposition without projecting page status", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockResolvedValue(closeDisposition);

    await expect(closeBrowserSession("conversation-one", 10_241))
      .resolves.toEqual(closeDisposition);
    expect(tauriMocks.invoke).toHaveBeenCalledWith("browser_close", {
      sessionId: "conversation-one",
      lifecycleEpoch: 10_241,
      ...rendererAuthority
    });
    expect(closeDisposition).not.toHaveProperty("url");
    expect(closeDisposition).not.toHaveProperty("title");
  });

  it("rejects native-only controls in browser preview", async () => {
    await expect(getBrowserStatus("conversation-one")).rejects.toThrow(/仅可在 Mework 桌面应用/);
    await expect(setBrowserPanelBounds("conversation-one", {
      x: 0,
      y: 0,
      width: 0,
      height: 0,
      visible: false
    }, 10)).rejects.toThrow(/仅可在 Mework 桌面应用/);
    await expect(performBrowserAction("conversation-one", "devtools")).rejects.toThrow(/仅可在 Mework 桌面应用/);
    await expect(navigateBrowser("conversation-one", "https://example.com/")).rejects.toThrow(/仅可在 Mework 桌面应用/);
    expect(rendererMountMocks.authority).not.toHaveBeenCalled();
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
  });
});
