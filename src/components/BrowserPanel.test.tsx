import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as browserApi from "../lib/browser";
import {
  claimFloatingSurfaceId,
  publishFloatingSurface,
  resetFloatingSurfaces
} from "../lib/floatingSurfaces";
import * as previewApi from "../lib/preview";
import { configureI18n } from "../i18n";
import {
  BrowserPanel,
  normalizeBrowserAddress,
  splitBrowserAddress,
  type BrowserOverlayRect
} from "./BrowserPanel";
import { previewBodyState, previewServerRows } from "./PreviewPane";

const workspaceTarget = { conversationId: "conversation-1" } as const;
/** The pane the browser draws: its title bar is the browser's own toolbar. */
const pane = { paneId: "preview:test-session" as const, onPaneClose: () => undefined };

/** The page area the native window is positioned from — `.browser-panel__body`, not the pane. */
const PAGE = box(900, 164, 560, 656);
/** A menu inside the page's rectangle: the shape every covering case here is made of. */
const OVER_PAGE = box(1175, 168, 270, 352);
/** A menu opened over the sidebar. Trusted, laid out, and no business of the page's. */
const BESIDE_PAGE = box(24, 300, 240, 320);

function box(left: number, top: number, width: number, height: number): BrowserOverlayRect {
  return { left, top, right: left + width, bottom: top + height, width, height };
}

function asDomRect(rect: BrowserOverlayRect): DOMRect {
  return { ...rect, x: rect.left, y: rect.top, toJSON: () => rect } as DOMRect;
}

/**
 * Lays out the page area, plus whichever of the pane's own surfaces the case is about.
 *
 * jsdom measures everything as zero, and a zero-area rectangle is deliberately read as "React has
 * rendered this but the browser has not placed it yet" rather than as a box at the origin — so
 * without real boxes nothing can ever be found over anything, and no case here is reachable.
 */
function layOutPage(surfaces: Record<string, BrowserOverlayRect> = {}): void {
  const empty = asDomRect(box(0, 0, 0, 0));
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    if (this.classList.contains("browser-panel__body")) return asDomRect(PAGE);
    for (const [className, rect] of Object.entries(surfaces)) {
      if (this.classList.contains(className)) return asDomRect(rect);
    }
    return empty;
  });
}

/** Every cover/uncover the pane asked the host for, in order. */
function occludeCalls(performAction: { mock: { calls: unknown[][] } }): unknown[] {
  return performAction.mock.calls.filter((call) => call[1] === "occlude").map((call) => call[2]);
}

/** Every sink/raise the pane asked the host for, in order. */
function projectCalls(performAction: { mock: { calls: unknown[][] } }): unknown[] {
  return performAction.mock.calls.filter((call) => call[1] === "project").map((call) => call[2]);
}

/**
 * The host's own report of what it currently has.
 *
 * `undefined` is what "not set" looks like on the wire: Rust skips both flags when they are false,
 * so the absent shape is the realistic one and a pane that only understood an explicit `false`
 * would never hear that the page had come back on top of whatever was drawn over it.
 */
function hostStatus(
  base: browserApi.BrowserStatus,
  occluded: boolean | undefined,
  projected: boolean | undefined = undefined
): browserApi.BrowserStatus {
  return {
    ...base,
    ...(occluded === undefined ? {} : { occluded }),
    ...(projected === undefined ? {} : { projected })
  };
}

/**
 * A host that applies what it is told, the way the real one does.
 *
 * The two flags have to be modelled separately or the pane is talking to something that lies: the
 * page is sunk for nearly the whole of a pane's life and only *covered* while a surface is over
 * it, so a fake that reported one and not the other would leave the pane's reconcile forever
 * repairing a sink the host claimed never to have taken.
 */
function hostFake(base: browserApi.BrowserStatus, initial: {
  occluded?: boolean;
  projected?: boolean;
} = {}) {
  const state: { occluded: boolean | undefined; projected: boolean | undefined } = {
    occluded: initial.occluded,
    projected: initial.projected
  };
  const status = () => hostStatus(base, state.occluded, state.projected);
  const apply = (action: string, value: unknown) => {
    if (action === "occlude") state.occluded = value === true ? true : undefined;
    if (action === "project") state.projected = value === true ? true : undefined;
    return status();
  };
  return { state, status, apply };
}

function openStatus(url = "about:blank"): browserApi.BrowserStatus {
  return {
    hasPage: true,
    open: true,
    loading: false,
    url,
    title: "新标签页",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    error: null,
    viewport: { width: 560, height: 640 }
  };
}

function configuration(
  servers: previewApi.PreviewConfiguredServer[],
  autoVerify = true
): previewApi.PreviewConfigurationList {
  return {
    launchJsonPath: "C:\\work\\mework\\.mework\\launch.json",
    servers,
    malformed: [],
    autoVerify
  };
}

function configuredServer(name: string, port: number): previewApi.PreviewConfiguredServer {
  return { name, command: "npm", args: ["run", "dev"], cwd: "C:\\work\\mework", port };
}

function runningServer(name: string, port: number, serverId = `srv-${name}`): previewApi.PreviewServerSnapshot {
  return {
    serverId,
    name,
    port,
    status: "running",
    startedAt: "2026-09-09T00:00:00Z",
    cwd: "C:\\work\\mework",
    sessionId: null
  };
}

describe("BrowserPanel", () => {
  afterEach(() => {
    // Unmounting hands the page back to the host, so unmount while the API spies are still live.
    cleanup();
    resetFloatingSurfaces();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    configureI18n("zh-CN");
    document.documentElement.dataset.theme = "day";
  });

  it("renders English browser controls with the reference night welcome background", () => {
    configureI18n("en-US");
    document.documentElement.dataset.theme = "night";

    render(<BrowserPanel {...pane} native={false} />);

    expect(screen.getByLabelText("Page URL")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveClass("browser-panel__welcome");
    expect(screen.getByRole("status")).toHaveAttribute("lang", "en-US");
    expect(screen.getByText("No dev server")).toBeInTheDocument();
    expect(screen.queryByTitle("Browser page")).not.toBeInTheDocument();
  });

  // The standby is one centred line drawn by this window's React. The heading, the globe mark and
  // the instructions the reference carried are gone on purpose: the address field above says what
  // it is for, and the embedded page's own about:blank document no longer draws copy of its own.
  it("renders the localized standby as a single centred line with no instructions", () => {
    render(<BrowserPanel {...pane} native={false} />);

    expect(screen.getByRole("status")).toHaveClass("browser-panel__welcome");
    expect(screen.getByText("没有开发服务器")).toHaveClass("browser-panel__welcome-line");
    expect(screen.queryByRole("heading", { name: "开始浏览" })).not.toBeInTheDocument();
    expect(screen.queryByText("输入 URL 以打开页面")).not.toBeInTheDocument();
    expect(document.querySelector(".browser-panel__welcome-mark")).not.toBeInTheDocument();
    expect(document.querySelector("iframe[srcdoc]")).not.toBeInTheDocument();
  });

  it("normalizes addresses without accepting active-content schemes", () => {
    expect(normalizeBrowserAddress("example.com")).toBe("https://example.com");
    expect(normalizeBrowserAddress("localhost:3000/path")).toBe("http://localhost:3000/path");
    expect(normalizeBrowserAddress("search terms")).toBe("https://www.google.com/search?q=search%20terms");
    expect(() => normalizeBrowserAddress("javascript:alert(1)")).toThrow(/HTTP/);
  });

  it("splits the address chip into the host half and the path half", () => {
    expect(splitBrowserAddress("about:blank")).toBeNull();
    expect(splitBrowserAddress("")).toBeNull();
    expect(splitBrowserAddress("http://localhost:5173/inbox?tab=1#top")).toEqual({
      host: "localhost:5173",
      path: "/inbox?tab=1#top"
    });
  });

  it("navigates inside the preview page while the sidebar owns close behavior", async () => {
    const user = userEvent.setup();
    render(<BrowserPanel {...pane} native={false} />);

    const address = screen.getByLabelText("页面网址");
    await user.clear(address);
    await user.type(address, "example.com{Enter}");
    expect(screen.getByTitle("浏览器页面")).toHaveAttribute("src", "https://example.com");
    expect(screen.getByRole("button", { name: "后退" })).toBeEnabled();

    expect(screen.queryByRole("button", { name: "收起内置浏览器" })).not.toBeInTheDocument();
  });

  // The pane emulates no viewport of its own and hands no page to the system browser: the agent's
  // `preview_resize` is the only thing that resizes the tab, and the toolbar carries neither button.
  it("offers no viewport menu and no external-browser hand-off", async () => {
    const user = userEvent.setup();
    render(<BrowserPanel {...pane} native={false} />);

    const address = screen.getByLabelText("页面网址");
    await user.clear(address);
    await user.type(address, "example.com{Enter}");

    expect(screen.getByTitle("浏览器页面")).not.toHaveAttribute("style");
    expect(screen.queryByRole("button", { name: "视口" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "在系统浏览器打开" })).not.toBeInTheDocument();
  });

  it("clears the loading state when preview navigation returns to the local welcome", async () => {
    const user = userEvent.setup();
    render(<BrowserPanel {...pane} native={false} />);

    const address = screen.getByLabelText("页面网址");
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
    const { container } = render(<BrowserPanel {...pane} native />);
    expect(container.querySelector(".browser-panel__native-surface")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "收起内置浏览器" })).not.toBeInTheDocument();
    expect(screen.queryByText(/正在连接/)).not.toBeInTheDocument();
    expect(screen.queryByTitle("浏览器页面")).not.toBeInTheDocument();
  });

  /**
   * The reference pane has one 32px row, and it is the pane's own title bar. A second chrome strip
   * would both look wrong and reserve height the native page is positioned from.
   */
  it("puts the whole toolbar in the pane title bar, in the reference order", () => {
    const { container } = render(<BrowserPanel {...pane} native />);

    const slot = container.querySelector(".side-pane__header-slot")!;
    expect(slot.querySelector(".browser-panel__toolbar")).toBeInTheDocument();
    expect(container.querySelector(".side-pane__title")).toBeNull();
    // Everything below the title bar is page, cards and the log drawer — no chrome of its own.
    const body = container.querySelector(".side-pane__body")!;
    expect(body.querySelector(".browser-panel__toolbar")).toBeNull();
    expect(body.firstElementChild).toHaveClass("browser-panel-content");

    expect(Array.from(
      slot.querySelectorAll("button"),
      (button) => button.getAttribute("aria-label")
    )).toEqual([
      "服务器与设置",
      "输入网址",
      "后退",
      "前进",
      "标注",
      "选择元素",
      "刷新页面"
    ]);
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

    render(<BrowserPanel {...pane} native={false} browserDev sessionId="conversation-e2e" />);

    expect(await screen.findByTitle("浏览器页面")).toHaveAttribute(
      "src",
      "http://127.0.0.1:1430/image-input-browser-e2e"
    );
    // The chip is a button until it is clicked, and it prints the host and the path separately.
    const chip = screen.getByRole("button", {
      name: "页面网址：http://127.0.0.1:1430/image-input-browser-e2e"
    });
    expect(chip).toHaveTextContent("127.0.0.1:1430");
    expect(chip).toHaveTextContent("/image-input-browser-e2e");
  });

  it("swaps the address chip for an input that navigates on Enter and reverts on Escape", async () => {
    const user = userEvent.setup();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(openStatus("https://example.com/one"));
    const navigate = vi.spyOn(browserApi, "navigateBrowser")
      .mockResolvedValue(openStatus("https://example.com/two"));

    render(<BrowserPanel {...pane} native={false} browserDev sessionId="conversation-address" />);

    const chip = await screen.findByRole("button", { name: "页面网址：https://example.com/one" });
    await user.click(chip);
    const input = screen.getByLabelText("页面网址");
    expect(input).toHaveValue("https://example.com/one");

    await user.clear(input);
    await user.type(input, "example.com/two{Enter}");
    await waitFor(() => expect(navigate).toHaveBeenCalledWith(
      "conversation-address",
      "https://example.com/two"
    ));

    await user.click(await screen.findByRole("button", { name: /页面网址：/ }));
    const reopened = screen.getByLabelText("页面网址");
    await user.clear(reopened);
    await user.type(reopened, "throwaway.example{Escape}");
    await waitFor(() => expect(screen.queryByLabelText("页面网址")).not.toBeInTheDocument());
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  /**
   * A tab whose page never opened still has an address bar. Typing into it opens the page the way
   * selecting the tab does and then goes where the user asked; an open that fails says why,
   * rather than leaving the navigation to fail against a page that is not there.
   */
  it("gives a tab without a page one before navigating it, and says why when it cannot", async () => {
    const user = userEvent.setup();
    const pageless = { ...openStatus(), hasPage: false, open: false };
    const status = vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(pageless);
    const navigate = vi.spyOn(browserApi, "navigateBrowser")
      .mockResolvedValue(openStatus("https://google.com/"));
    const onOpenPage = vi.fn(async () => {
      status.mockResolvedValue(openStatus());
    });

    const { unmount } = render(
      <BrowserPanel {...pane} native active sessionId="conversation-pageless" onOpenPage={onOpenPage} />
    );
    await user.click(await screen.findByRole("button", { name: "输入网址" }));
    await user.type(screen.getByLabelText("页面网址"), "google.com{Enter}");
    await waitFor(() => expect(navigate).toHaveBeenCalledWith(
      "conversation-pageless",
      expect.stringContaining("google.com")
    ));
    expect(onOpenPage).toHaveBeenCalledTimes(1);
    expect(onOpenPage.mock.invocationCallOrder[0]).toBeLessThan(navigate.mock.invocationCallOrder[0]);
    unmount();

    navigate.mockClear();
    status.mockResolvedValue(pageless);
    const failingOpen = vi.fn(async () => {
      status.mockResolvedValue({ ...pageless, error: "CEF refused the page's proxy settings" });
    });
    render(
      <BrowserPanel {...pane} native active sessionId="conversation-pageless-failed" onOpenPage={failingOpen} />
    );
    await user.click(await screen.findByRole("button", { name: "输入网址" }));
    await user.type(screen.getByLabelText("页面网址"), "google.com{Enter}");
    expect(await screen.findByText(/CEF refused the page's proxy settings/)).toBeInTheDocument();
    expect(failingOpen).toHaveBeenCalledTimes(1);
    expect(navigate).not.toHaveBeenCalled();
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
        tool: "preview_click",
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

    render(<BrowserPanel {...pane} native={false} browserDev sessionId="conversation-agent" />);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Page URL: https://example.com/" })).toBeInTheDocument();
    });
    expect(document.querySelector(".browser-panel__control")).not.toBeInTheDocument();
    expect(screen.queryByText("Agent activity")).not.toBeInTheDocument();
    expect(screen.queryByText("You are in control")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Take over browser control|Return control to Agent/ })).not.toBeInTheDocument();
  });

  it("provides Chromium-style menu focus, keyboard navigation, Escape, and outside dismissal", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage({ "browser-panel__menu": OVER_PAGE });

    render(<BrowserPanel {...pane} native active sessionId="conversation-menu-keyboard" />);

    const trigger = screen.getByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    expect(trigger).toHaveAttribute("aria-controls");
    await user.click(trigger);

    const menu = await screen.findByRole("menu", { name: "浏览器菜单" });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(menu).toHaveAttribute("id", trigger.getAttribute("aria-controls"));
    // The menu is HTML over a native window that paints above every HTML layer, so it is not
    // readable at all until the page has gone behind the renderer.
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));
    const openFile = screen.getByRole("menuitem", { name: /打开文件/ });
    const logs = screen.getByRole("menuitem", { name: /显示开发服务器日志/ });
    const clear = screen.getByRole("menuitem", { name: "清除浏览数据" });
    expect(screen.queryByRole("menuitem", { name: /保存屏幕截图/ })).toBeNull();
    await waitFor(() => expect(openFile).toHaveFocus());

    await user.keyboard("{ArrowDown}");
    expect(logs).toHaveFocus();
    await user.keyboard("{End}");
    expect(clear).toHaveFocus();
    await user.keyboard("{Home}");
    expect(openFile).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    expect(clear).toHaveFocus();
    await user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
    // Closing the menu is a state update and nothing else: the host hears the page can come back
    // from the measurement pass that no longer finds a surface over it, not from this keystroke.
    // That is what took the generation fencing out — no menu key has a reply that can arrive late.
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));

    await user.click(trigger);
    await screen.findByRole("menu");
    fireEvent.mouseDown(screen.getByRole("button", { name: "输入网址" }));
    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  });

  /**
   * A card the sidebar draws over this pane is not this pane's to measure, so it arrives as a
   * rectangle. Everything after that is the same: the page is a native window above the renderer,
   * and the card is invisible and unclickable until the page is behind it.
   */
  it("takes the page out from under a trusted overlay the sidebar drew, and gives it back", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    const { rerender } = render(
      <BrowserPanel {...pane} native active sessionId="conversation-outer-overlay" />
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());
    expect(occludeCalls(performAction)).toEqual([]);

    rerender(
      <BrowserPanel {...pane}
        native
        active
        sessionId="conversation-outer-overlay"
        trustedOverlayRect={OVER_PAGE}
      />
    );
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    rerender(
      <BrowserPanel {...pane} native active sessionId="conversation-outer-overlay" trustedOverlayRect={null} />
    );
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));
  });

  /**
   * The floating-surface registry is the path every menu in the app reaches the native page by.
   * Without it only the surfaces this component renders would count, and anything popped from the
   * main view — a timeline context menu, a workspace menu, a dialog — is painted over by the page.
   */
  it("takes the page out from under a menu the rest of the app opened over it", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-registry" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    // Withdrawn, not merely moved: a page that stayed parked would leave a still of itself frozen
    // where the live page belongs, with no surface left whose closing could ever bring it back.
    act(() => publishFloatingSurface(menu, null));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));
  });

  /**
   * The defect this whole mechanism exists to remove, in its last remaining form, end to end.
   *
   * Sleeping, suspending, hiding and re-presenting all restack the page above the renderer without
   * the pane asking, and none of them is an event the pane can hear. Left believing the page is
   * still under its surfaces, the pane would go on painting a still of a page that is live again —
   * and the dialog it was taken out from under would be behind it, unreadable and unclickable,
   * with nothing on screen able to bring it forward.
   *
   * The reconcile itself belongs to the occlusion hook and is tested there against a boolean. What
   * is tested here is the wiring that boolean arrives through: the status poll is the only notice
   * there is, and `status.occluded === true` is how this panel reads it.
   */
  it("covers the page again once the host reports it dropped the cover it had", async () => {
    const base = openStatus();
    // The host's own account of what it currently has parked, which only it may set to true.
    const host = hostFake(base);
    vi.spyOn(browserApi, "getBrowserStatus").mockImplementation(async () => host.status());
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_sessionId, action, value) => host.apply(action, value)
    );
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-dropped-cover" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    // Nothing about the menu changes here: only the host's answer does. A pane that tracked its
    // own last request rather than the host's report would never ask a second time.
    host.state.occluded = false;
    await waitFor(
      () => expect(occludeCalls(performAction)).toEqual([true, true]),
      { timeout: 4000 }
    );

    // And again for the shape the field actually arrives in, since false is skipped on the wire.
    host.state.occluded = undefined;
    await waitFor(
      () => expect(occludeCalls(performAction)).toEqual([true, true, true]),
      { timeout: 4000 }
    );
  });

  /**
   * The other direction of the same disagreement, which only the poll can report.
   *
   * A pane that went away while something was over the page left it parked, because restacking on
   * the way out is the host's. The pane that opens next has nothing over the page, so no uncover is
   * ever coming — and without reading what the host actually has, the page would simply be
   * invisible: open, owned, navigating, and behind a renderer drawing nothing in its place.
   */
  it("drops a cover it mounted onto and nothing on screen still needs", async () => {
    const base = openStatus();
    const host = hostFake(base, { occluded: true, projected: true });
    vi.spyOn(browserApi, "getBrowserStatus").mockImplementation(async () => host.status());
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_sessionId, action, value) => host.apply(action, value)
    );
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-mounted-onto-parked" />);

    // Nothing is drawn over this page, so nothing but the host's own report could ask for this.
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([false]), { timeout: 4000 });
    await waitFor(() => expect(host.state.occluded).toBeUndefined());
    // The sink itself is not a disagreement: sunk is where a page belongs until somebody uses it,
    // so the pane leaves it exactly where it found it.
    expect(projectCalls(performAction)).not.toContain(false);
  });

  /**
   * The projection model, end to end: what is on screen is the app, and the page comes up only for
   * the one thing a picture cannot do.
   *
   * The page is a native child window that paints above every HTML layer, so leaving it on top
   * means the pane's corners, its transitions and every menu the rest of the app opens are painted
   * over by a rectangle React does not control. Raising it on use is what keeps it a browser
   * rather than a picture of one — scrolling, text selection, the IME and native right-click all
   * belong to the real page, and none of them survives being simulated.
   */
  it("keeps the page sunk until the pointer is inside it, and puts it back when it leaves", async () => {
    const base = openStatus("http://localhost:5173/");
    const host = hostFake(base);
    vi.spyOn(browserApi, "getBrowserStatus").mockImplementation(async () => host.status());
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_sessionId, action, value) => host.apply(action, value)
    );
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-projection" />);

    // Sunk on arrival: nothing has to happen for the app to be what the user is looking at.
    await waitFor(() => expect(projectCalls(performAction)).toEqual([true]));

    act(() => {
      window.dispatchEvent(new MouseEvent("pointerover", {
        clientX: PAGE.left + 10,
        clientY: PAGE.top + 10,
        bubbles: true
      }));
    });
    await waitFor(() => expect(projectCalls(performAction)).toEqual([true, false]));

    // Positive evidence of being elsewhere, which is the only kind there is: the raised page eats
    // every pointer event over its own box, so the page element's own `pointerleave` fires on the
    // way *in* and can never mean the user is done.
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerdown", {
        clientX: BESIDE_PAGE.left + 10,
        clientY: BESIDE_PAGE.top + 10,
        bubbles: true
      }));
    });
    await waitFor(() => expect(projectCalls(performAction)).toEqual([true, false, true]));

    // And never once as a cover. Covering hands the page back to the user and takes it out of
    // agent automation; resting is the pane's normal state and must cost the Agent nothing.
    expect(occludeCalls(performAction)).toEqual([]);
  });

  /**
   * Between asking for a cover and the host applying it, every poll answers "not covered" — and so
   * does every poll while the host is refusing outright, which is the state the pane mounts into
   * while the page is still being created. Reading that as a disagreement to act on would re-ask on
   * each one, turning a page the host has not got round to into a request storm against it.
   */
  it("asks once, not once per poll, while the host has yet to confirm the cover", async () => {
    const base = openStatus();
    // The host never acknowledges the cover: it answers every poll and every request the same way.
    const poll = vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(hostStatus(base, undefined));
    const performAction = vi.spyOn(browserApi, "performBrowserAction")
      .mockResolvedValue(hostStatus(base, undefined));
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-unconfirmed-cover" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    const polled = poll.mock.calls.length;
    await waitFor(
      () => expect(poll.mock.calls.length).toBeGreaterThanOrEqual(polled + 3),
      { timeout: 4000 }
    );
    expect(occludeCalls(performAction)).toEqual([true]);
  });

  /**
   * The page coming back is the point once the menu has closed. A pane that re-covered on any
   * report of no cover would fight its own uncover and park a page with nothing drawn over it.
   */
  it("leaves the page up when the host's cover is gone because nothing needs it any more", async () => {
    const base = openStatus();
    const host = hostFake(base);
    const poll = vi.spyOn(browserApi, "getBrowserStatus").mockImplementation(async () => host.status());
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_sessionId, action, value) => host.apply(action, value)
    );
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-cover-not-needed" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));
    // Confirmed, so the loss the pane is watching for is armed.
    await waitFor(() => expect(host.state.occluded).toBe(true));

    act(() => publishFloatingSurface(menu, null));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));

    const polled = poll.mock.calls.length;
    await waitFor(
      () => expect(poll.mock.calls.length).toBeGreaterThanOrEqual(polled + 3),
      { timeout: 4000 }
    );
    expect(occludeCalls(performAction)).toEqual([true, false]);
  });

  /**
   * Covering is not free — it costs a capture and a frozen page — and it is decided against the
   * page's own box rather than the window's, so a menu opened over the sidebar buys nothing.
   */
  it("leaves the page alone for a surface that never reached it", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-elsewhere" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    const elsewhere = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(elsewhere, BESIDE_PAGE));
    await act(async () => undefined);

    expect(occludeCalls(performAction)).toEqual([]);
  });

  /**
   * The pane's own chrome is subject to the same rule as everything else in the app: it is HTML,
   * and the page paints over HTML. Each of these used to publish its own rectangle; now none of
   * them knows anything about the page, and the measurement pass is what notices them.
   */
  it("takes the page out from under the drawing layer it opened", async () => {
    const user = userEvent.setup();
    const status = openStatus("http://localhost:5173/");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    vi.spyOn(browserApi, "captureBrowserPage").mockResolvedValue(null);
    layOutPage({ "browser-panel__sketch": OVER_PAGE });

    render(<BrowserPanel {...pane} native active sessionId="conversation-sketch-occlusion" />);
    const annotate = screen.getByRole("button", { name: "标注" });
    await waitFor(() => expect(annotate).toBeEnabled());

    await user.click(annotate);
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    await user.click(annotate);
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));
  });

  /**
   * The error strip floats over the page now that there is no chrome strip for it to sit in, so a
   * report the page paints over is a report nobody ever reads.
   */
  it("takes the page out from under its own error strip", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    vi.spyOn(browserApi, "openLocalFileInBrowser").mockRejectedValue(new Error("不支持的文件类型"));
    layOutPage({ "browser-panel__error": OVER_PAGE });

    render(<BrowserPanel {...pane} native active sessionId="conversation-error-occlusion" />);
    const trigger = screen.getByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /打开文件/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("不支持的文件类型");
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));
  });

  /**
   * A pane the user has switched away from draws nothing over the page, whatever is still
   * registered: its surfaces belong to a view that is no longer on screen, and a page left parked
   * for them would sit behind a renderer that has stopped painting anything in its place.
   */
  it("leaves no page parked behind the renderer when the pane goes quiet, and covers it again on return", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    const { rerender } = render(
      <BrowserPanel {...pane} native active sessionId="conversation-park-cleanup" />
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());
    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    rerender(<BrowserPanel {...pane} native active={false} sessionId="conversation-park-cleanup" />);
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false]));

    rerender(<BrowserPanel {...pane} native active sessionId="conversation-park-cleanup" />);
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true, false, true]));
  });

  /**
   * The page remnant.
   *
   * A pane goes away because the browser is being hidden or closed, and the host parks and hides
   * the page on that path itself. An unpark sent from the closing pane raises the native window to
   * the top of the z-order at exactly that moment, and whichever of the two lands last wins — so a
   * live page was left painted over the whole app, with no pane remaining that could take it down.
   * Restacking on the way out is the host's, and a pane that merely remounts is put back by the
   * reconcile against what the host reports.
   */
  it("leaves the page's stacking to the host when the pane goes away, rather than racing its hide", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    const { unmount } = render(
      <BrowserPanel {...pane} native active sessionId="conversation-park-remnant" />
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());
    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(occludeCalls(performAction)).toEqual([true]));

    unmount();
    await act(async () => undefined);

    expect(occludeCalls(performAction)).toEqual([true]);
    // And the still goes with it: nothing is left to paint it, so it cannot be left on screen.
    expect(document.querySelector(".browser-panel__still")).toBeNull();
  });

  /**
   * The pane's `⋮` settings menu was removed along with the one item it held, so the browser pane
   * must not grow a second trigger beside the address bar's own menu. The assertion is on the
   * control rather than on the item: a `⋮` with nothing in it is the same defect as one holding a
   * dead row, and `SidePane` only draws the trigger when a pane hands it sections.
   */
  it("draws no pane settings menu of its own beside the browser menu", async () => {
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    layOutPage();

    render(<BrowserPanel {...pane} native active sessionId="conversation-no-pane-menu" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "服务器与设置" })).toBeEnabled());

    expect(screen.queryByRole("button", { name: "预览 设置" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /保存屏幕截图/ })).toBeNull();
  });

  it("arms and disarms the element picker through the host, and reflects the host state", async () => {
    const user = userEvent.setup();
    const idle = openStatus("http://localhost:5173/");
    const armed = { ...idle, elementPicker: { armed: true, pendingPick: false } };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(idle);
    // Only the picker's own action arms it. The pane also sinks and raises the page through this
    // same command, and a mock that answered every one of them with the armed status would have
    // the button pressed before anybody clicked it.
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockImplementation(
      async (_sessionId, action, value) => (
        action === "select_element" && value === true ? armed : idle
      )
    );

    render(<BrowserPanel {...pane} native sessionId="conversation-picker" />);
    const select = screen.getByRole("button", { name: "选择元素" });
    await waitFor(() => expect(select).toBeEnabled());
    expect(select).toHaveAttribute("aria-pressed", "false");

    await user.click(select);
    expect(performAction).toHaveBeenCalledWith("conversation-picker", "select_element", true);
    // The button follows the host rather than a local guess: the page, not the pane, is armed.
    const exit = await screen.findByRole("button", { name: "退出选择模式" });
    expect(exit).toHaveAttribute("aria-pressed", "true");

    await user.click(exit);
    expect(performAction).toHaveBeenCalledWith("conversation-picker", "select_element", false);
  });

  it("drains a waiting pick through its own command rather than off the poll", async () => {
    const idle = openStatus("http://localhost:5173/");
    vi.spyOn(browserApi, "getBrowserStatus")
      .mockResolvedValue({ ...idle, elementPicker: { armed: false, pendingPick: true } });
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(idle);
    const picked = {
      sequence: 7,
      tagName: "button",
      id: null,
      classes: ["btn"],
      attributes: {},
      computedStyles: {},
      boundingBox: { x: 0, y: 0, width: 10, height: 10 },
      screenshotBase64: "",
      outerHtml: null
    } satisfies browserApi.SelectedElement;
    const take = vi.spyOn(browserApi, "takeSelectedElement").mockResolvedValue(picked);
    const onElementPicked = vi.fn();

    render(<BrowserPanel {...pane} native sessionId="conversation-pick" onElementPicked={onElementPicked} />);
    await waitFor(() => expect(take).toHaveBeenCalledWith("conversation-pick"));
    await waitFor(() => expect(onElementPicked).toHaveBeenCalledWith(picked));
  });

  it("leaves no page in inspect mode when the pane goes away while armed", async () => {
    const armed = { ...openStatus("http://localhost:5173/"), elementPicker: { armed: true, pendingPick: false } };
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(armed);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(armed);

    const view = render(<BrowserPanel {...pane} native sessionId="conversation-unmount" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "退出选择模式" })).toBeInTheDocument());
    view.unmount();

    expect(performAction).toHaveBeenCalledWith("conversation-unmount", "select_element", false);
  });

  it("opens a local file from the Files group and follows it in the address bar", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    const openFile = vi.spyOn(browserApi, "openLocalFileInBrowser").mockResolvedValue({
      ...status,
      url: "https://mework-file-preview.invalid/report.html"
    });
    const target = { kind: "workspace", workspaceId: "workspace-open-file" } as const;

    render(<BrowserPanel {...pane} native sessionId="conversation-open-file" fileTarget={target} />);
    const trigger = screen.getByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    const menu = await screen.findByRole("menu", { name: "浏览器菜单" });
    expect(within(menu).getByText("文件")).toBeInTheDocument();

    await user.click(within(menu).getByRole("menuitem", { name: /打开文件/ }));
    // The workspace rides along so the host can open the dialog where the reference does, in the
    // session's own directory. It never carries a path: the file the user picks is the authority.
    await waitFor(() => expect(openFile).toHaveBeenCalledWith("conversation-open-file", target));
    await waitFor(() => expect(screen.getByRole("button", { name: /页面网址/ }))
      .toHaveTextContent("mework-file-preview.invalid"));
  });

  it("reports a refused local file instead of leaving the menu silent", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    vi.spyOn(browserApi, "openLocalFileInBrowser").mockRejectedValue(new Error("不支持的文件类型"));

    render(<BrowserPanel {...pane} native sessionId="conversation-open-file-error" />);
    const trigger = screen.getByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /打开文件/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("不支持的文件类型");
  });

  it("captures the page before the drawing layer covers it, then draws on the capture", async () => {
    const user = userEvent.setup();
    const status = openStatus("http://localhost:5173/");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    const order: string[] = [];
    const capture = vi.spyOn(browserApi, "captureBrowserPage").mockImplementation(async () => {
      order.push("capture");
      return { data: "UE5H", width: 800, height: 600 };
    });

    render(<BrowserPanel {...pane} native sessionId="conversation-annotate" />);
    const annotate = screen.getByRole("button", { name: "标注" });
    await waitFor(() => expect(annotate).toBeEnabled());
    expect(document.querySelector(".browser-panel__sketch")).toBeNull();

    await user.click(annotate);
    // The layer covers the pane body, and the host hands a fully covered rectangle back whole —
    // so a capture asked for after the layer opened would have nothing composited left to read.
    expect(capture).toHaveBeenCalledWith("conversation-annotate");
    expect(order).toEqual(["capture"]);
    expect(document.querySelector(".browser-panel__sketch")).not.toBeNull();
    expect(annotate).toHaveAttribute("aria-pressed", "true");

    const backdrop = await waitFor(() => {
      const image = document.querySelector<HTMLImageElement>(".browser-panel__sketch img");
      expect(image?.getAttribute("src")).toBe("data:image/png;base64,UE5H");
      return image;
    });
    expect(backdrop).not.toBeNull();

    await user.click(annotate);
    expect(document.querySelector(".browser-panel__sketch")).toBeNull();
    expect(annotate).toHaveAttribute("aria-pressed", "false");
  });

  it("opens the drawing layer on a blank backdrop when the capture fails", async () => {
    const user = userEvent.setup();
    const status = openStatus("http://localhost:5173/");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    vi.spyOn(browserApi, "captureBrowserPage").mockRejectedValue(new Error("no page"));

    render(<BrowserPanel {...pane} native sessionId="conversation-annotate-fail" />);
    const annotate = screen.getByRole("button", { name: "标注" });
    await waitFor(() => expect(annotate).toBeEnabled());
    await user.click(annotate);

    expect(document.querySelector(".browser-panel__sketch")).not.toBeNull();
    expect(document.querySelector(".browser-panel__sketch img")).toBeNull();
    // A capture that never arrives must not surface as an error strip over the drawing layer.
    expect(screen.queryByRole("alert")).toBeNull();
  });

  /**
   * The drawing layer is itself a surface over the page, so by the time this capture lands the page
   * is already parked behind the still the occlusion pass stood in — and a parked page composites
   * nothing, which is exactly how the capture comes back empty. The still is a picture of the same
   * page a moment earlier, which is a better thing to draw on than nothing.
   */
  it("draws on the still the page was parked behind when its own capture comes back empty", async () => {
    const user = userEvent.setup();
    const status = openStatus("http://localhost:5173/");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);
    // Whatever the page currently composites: a frame while it is live, nothing once it is parked.
    let composited: browserApi.BrowserPageCapture | null = { data: "V0FSTQ", width: 800, height: 600 };
    const capture = vi.spyOn(browserApi, "captureBrowserPage").mockImplementation(async () => composited);
    // jsdom decodes nothing, so a real `<img>` would measure zero and be rejected as the unpainted
    // single pixel a capture of a surface that was not drawing returns.
    vi.stubGlobal("Image", class {
      src = "";
      naturalWidth = 800;
      naturalHeight = 600;
      decode() { return Promise.resolve(); }
    });
    layOutPage({ "browser-panel__sketch": OVER_PAGE });

    render(<BrowserPanel {...pane} native active sessionId="conversation-annotate-still" />);
    const annotate = screen.getByRole("button", { name: "标注" });
    await waitFor(() => expect(annotate).toBeEnabled());

    // Something else covers the page first; that is what leaves a still in the page's place.
    const menu = claimFloatingSurfaceId();
    act(() => publishFloatingSurface(menu, OVER_PAGE));
    await waitFor(() => expect(document.querySelector(".browser-panel__still"))
      .toHaveAttribute("src", "data:image/png;base64,V0FSTQ"));

    composited = null;
    capture.mockClear();
    await user.click(annotate);

    // The backdrop is the still's own frame, which nothing but the fallback could have produced:
    // the capture this click made answered nothing at all.
    await waitFor(() => expect(document.querySelector(".browser-panel__sketch img"))
      .toHaveAttribute("src", "data:image/png;base64,V0FSTQ"));
    expect(capture).toHaveBeenCalledWith("conversation-annotate-still");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("drops the controls Claude Code's preview pane does not have", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);

    render(<BrowserPanel {...pane} native sessionId="conversation-menu-trimmed" />);
    const trigger = screen.getByRole("button", { name: "服务器与设置" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await screen.findByRole("menu", { name: "浏览器菜单" });

    for (const gone of ["在页面中查找", "打印", "缩放", "显示设备工具栏", "下载", "挂起此任务页面", "浏览器设置"]) {
      expect(screen.queryByText(gone)).not.toBeInTheDocument();
    }
  });

  it("requires an explicit second step before clearing the task Chromium profile", async () => {
    const user = userEvent.setup();
    const status = openStatus();
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(status);
    const performAction = vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(status);

    render(<BrowserPanel {...pane} native sessionId="conversation-clear-data" />);
    const trigger = screen.getByRole("button", { name: "服务器与设置" });
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
    // Nothing on screen changes when a profile is emptied, so the row itself is the report: it
    // holds the menu open long enough to be read, then closes it.
    expect(await screen.findByRole("menuitem", { name: "已清除此标签页的浏览数据" }))
      .toBeInTheDocument();
    await waitFor(
      () => expect(screen.queryByRole("menu", { name: "浏览器菜单" })).not.toBeInTheDocument(),
      { timeout: 4000 }
    );
  });
});

describe("BrowserPanel dev servers", () => {
  beforeEach(() => {
    configureI18n("en-US");
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(openStatus());
    vi.spyOn(browserApi, "performBrowserAction").mockResolvedValue(openStatus());
    vi.spyOn(browserApi, "navigateBrowser").mockResolvedValue(openStatus("http://localhost:5173/"));
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    configureI18n("zh-CN");
  });

  it("joins launch.json configurations to the processes answering them", () => {
    const rows = previewServerRows(
      [configuredServer("web", 5173), configuredServer("api", 8080)],
      [runningServer("web", 5199), { ...runningServer("stray", 4000), status: "starting" }]
    );

    expect(rows.map((row) => [row.name, row.port, row.running, row.starting])).toEqual([
      // The running process wins on port: a launch.json entry only says which port was asked for.
      ["web", 5199, true, false],
      ["api", 8080, false, false],
      ["stray", 4000, false, true]
    ]);
  });

  it("orders the body states so an action's own report is never hidden by a loaded page", () => {
    const rows = previewServerRows([configuredServer("web", 5173)], []);
    const base = {
      url: "http://localhost:5173/",
      configurationCount: 1,
      rows,
      pendingName: null,
      startError: null,
      stopped: null
    };

    expect(previewBodyState(base)).toEqual({ kind: "page" });
    expect(previewBodyState({ ...base, pendingName: "web" })).toEqual({ kind: "starting" });
    expect(previewBodyState({ ...base, startError: { name: "web", message: "boom" } }))
      .toEqual({ kind: "start-failed", name: "web", message: "boom" });
    expect(previewBodyState({ ...base, stopped: { label: "web:5173" } }))
      .toEqual({ kind: "stopped", label: "web:5173" });
    expect(previewBodyState({ ...base, url: "about:blank" })).toEqual({ kind: "start-page" });
    expect(previewBodyState({ ...base, url: "about:blank", configurationCount: 0, rows: [] }))
      .toEqual({ kind: "no-config" });
    // Stopping a server takes its page down, so the usual ending is no page at all — and what
    // belongs there is the page the server can be run from again, not a card about the thing
    // that was just closed.
    expect(previewBodyState({ ...base, url: "about:blank", stopped: { label: "web:5173" } }))
      .toEqual({ kind: "start-page" });
  });

  it("opens a server on another machine only once the host says it answers", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    const listed = vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);
    const starting = { ...runningServer("web", 5173), status: "starting" as const, machine: "devbox" };
    vi.spyOn(previewApi, "startPreviewServer").mockImplementation(async () => {
      listed.mockResolvedValue([starting]);
      return { server: starting, reused: false };
    });
    render(<BrowserPanel {...pane} native sessionId="conversation-remote" target={{ conversationId: "c", workspace: 2 }} />);
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(within(await screen.findByRole("menu", { name: "Browser menu" }))
      .getByRole("menuitemradio", { name: "Run web" }));

    // Started there, still being waited out: the starting card, and no page yet.
    expect(await screen.findByText("Starting server")).toBeInTheDocument();
    expect(browserApi.navigateBrowser).not.toHaveBeenCalledWith("conversation-remote", expect.anything());

    listed.mockResolvedValue([{ ...starting, status: "running" }]);
    await waitFor(() => expect(browserApi.navigateBrowser).toHaveBeenCalledWith(
      "conversation-remote",
      "http://localhost:5173/"
    ), { timeout: 4000 });
  });

  it("lists every configuration in the overflow menu with the row itself as the run control", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173), configuredServer("api", 8080)]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([runningServer("api", 8080)]);
    const start = vi.spyOn(previewApi, "startPreviewServer").mockResolvedValue({
      server: runningServer("web", 5173),
      reused: false
    });
    render(<BrowserPanel {...pane} native sessionId="conversation-servers" target={workspaceTarget} />);
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    const menu = await screen.findByRole("menu", { name: "Browser menu" });

    expect(within(menu).getByText("Servers")).toBeInTheDocument();
    expect(within(menu).getByText("Settings")).toBeInTheDocument();
    // The idle row carries `Run` as a label, not a second control; the running one carries Stop.
    const idle = within(menu).getByRole("menuitemradio", { name: "Run web" });
    expect(idle).toHaveTextContent("Run");
    expect(within(idle).queryByRole("button")).not.toBeInTheDocument();
    expect(within(menu).getByRole("button", { name: "Stop api" })).toBeInTheDocument();
    expect(within(menu).queryByRole("button", { name: "Run web" })).not.toBeInTheDocument();
    expect(within(menu).getByRole("menuitem", { name: /Stop all servers/ })).toBeInTheDocument();
    // Auto verify is a launch.json field, not a menu toggle: the row is gone from the menu.
    expect(within(menu).queryByRole("menuitem", { name: /auto verify/i })).not.toBeInTheDocument();
    // The reference leading menu has no way to add a server; the file is the only source.
    expect(within(menu).queryByRole("menuitem", { name: /Add server/ })).not.toBeInTheDocument();

    await user.click(idle);
    await waitFor(() => expect(start).toHaveBeenCalledWith(workspaceTarget, "web"));
    // A started server is what the pane then shows, so the address follows it without being typed.
    await waitFor(() => expect(browserApi.navigateBrowser).toHaveBeenCalledWith(
      "conversation-servers",
      "http://localhost:5173/"
    ));
  });

  it("starts a server for the owner it was given, which is how a new task claims its own", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);
    const start = vi.spyOn(previewApi, "startPreviewServer").mockResolvedValue({
      server: { ...runningServer("web", 5173), sessionId: "conv_draft" },
      reused: false
    });
    render(
      <BrowserPanel
        {...pane}
        native
        sessionId="conv_draft"
        target={{ conversationId: "conv_draft", draftWorkspaceId: "workspace-1" }}
      />
    );
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(within(await screen.findByRole("menu", { name: "Browser menu" }))
      .getByRole("menuitemradio", { name: "Run web" }));

    // The draft's servers are its own by the id it will materialize as, which is the target's.
    await waitFor(() => expect(start).toHaveBeenCalledWith(
      { conversationId: "conv_draft", draftWorkspaceId: "workspace-1" },
      "web"
    ));
  });

  it("stops one server from its row and every server from the group's own action", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173), configuredServer("api", 8080)]));
    vi.spyOn(previewApi, "listPreviewServers")
      .mockResolvedValue([runningServer("web", 5173), runningServer("api", 8080)]);
    const stop = vi.spyOn(previewApi, "stopPreviewServer").mockResolvedValue(true);
    render(<BrowserPanel {...pane} native sessionId="conversation-stop" target={workspaceTarget} />);
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());

    await user.click(trigger);
    const menu = await screen.findByRole("menu", { name: "Browser menu" });
    await user.click(within(menu).getByRole("button", { name: "Stop web" }));
    await waitFor(() => expect(stop).toHaveBeenCalledWith("srv-web"));

    stop.mockClear();
    await user.click(trigger);
    const reopened = await screen.findByRole("menu", { name: "Browser menu" });
    await user.click(within(reopened).getByRole("menuitem", { name: /Stop all servers/ }));
    await waitFor(() => expect(stop.mock.calls.map(([serverId]) => serverId).sort())
      .toEqual(["srv-api", "srv-web"]));
  });

  it("drops the servers group entirely when the project configures none", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations").mockResolvedValue(configuration([]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);

    render(<BrowserPanel {...pane} native sessionId="conversation-empty" target={workspaceTarget} />);
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);

    const menu = await screen.findByRole("menu", { name: "Browser menu" });
    // The reference drops the whole Servers group rather than explaining its absence in a row;
    // the pane body already says it, in its one standby line.
    expect(within(menu).queryByText("Servers")).not.toBeInTheDocument();
    expect(within(menu).getByText("Settings")).toBeInTheDocument();
    expect(within(menu).queryByRole("menuitem", { name: /Stop all servers/ })).not.toBeInTheDocument();
    expect(screen.getByText("No dev server")).toHaveClass("browser-panel__welcome-line");
  });

  it("renders the start page with the first five servers and reveals the rest on See all", async () => {
    const user = userEvent.setup();
    const servers = Array.from({ length: 7 }, (_, index) => configuredServer(`web-${index}`, 5170 + index));
    vi.spyOn(previewApi, "listPreviewConfigurations").mockResolvedValue(configuration(servers));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);

    render(<BrowserPanel {...pane} native sessionId="conversation-start-page" target={workspaceTarget} />);

    const list = await screen.findByRole("list");
    await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(5));

    await user.click(screen.getByRole("button", { name: "See all" }));
    expect(within(screen.getByRole("list")).getAllByRole("listitem")).toHaveLength(7);
  });

  it("reports a failed start with the host's diagnosis, a copy affordance, and a retry", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);
    const start = vi.spyOn(previewApi, "startPreviewServer")
      .mockRejectedValue(new Error("npm ERR! missing script: dev"));
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

    render(<BrowserPanel {...pane} native sessionId="conversation-failed" target={workspaceTarget} />);
    await user.click(await screen.findByRole("button", { name: "Run web" }));

    expect(await screen.findByText("Dev server failed to start")).toBeInTheDocument();
    expect(screen.getByText(/Error details were sent to Claude/)).toBeInTheDocument();
    expect(screen.getByText(/npm ERR! missing script: dev/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Copy error log" }));
    expect(writeText).toHaveBeenCalledWith("npm ERR! missing script: dev");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(start).toHaveBeenCalledTimes(2));
  });

  /** A server that exits by itself keeps its page: what the page shows is the diagnosis. */
  it("offers a restart when the server behind the page disappears", async () => {
    vi.spyOn(browserApi, "getBrowserStatus").mockResolvedValue(openStatus("http://localhost:5173/"));
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    const list = vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([runningServer("web", 5173)]);
    vi.spyOn(previewApi, "startPreviewServer").mockResolvedValue({
      server: runningServer("web", 5173),
      reused: false
    });
    const user = userEvent.setup();

    render(<BrowserPanel {...pane} native sessionId="conversation-stopped" target={workspaceTarget} />);
    // The page is already showing the server, so the start page is not on screen: the menu is
    // where a server is adopted from once there is something in the body.
    await user.click(screen.getByRole("button", { name: "Servers & settings" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Open web" }));

    list.mockResolvedValue([]);
    expect(await screen.findByText("web:5173", {}, { timeout: 4000 })).toBeInTheDocument();
    expect(screen.getByText("Stopped")).toBeInTheDocument();
    expect(screen.getByText("The dev server stopped. Restart it, or close the preview."))
      .toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart web:5173" })).toBeInTheDocument();
  });

  it("docks the dev-server log drawer below the page and colours its lines", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([runningServer("web", 5173)]);
    const logs = vi.spyOn(previewApi, "readPreviewServerLogs").mockResolvedValue("No logs yet.");

    render(<BrowserPanel {...pane} native sessionId="conversation-logs" target={workspaceTarget} />);
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /Show dev server logs/ }));

    // The empty state names the server the drawer is following, so "nothing printed yet" cannot be
    // read as "this drawer is a placeholder".
    expect(await screen.findByText("Waiting for output from web…")).toBeInTheDocument();
    await waitFor(() => expect(logs).toHaveBeenCalledWith("srv-web", { lines: 200 }));
    expect(document.querySelector('.browser-panel__log-grip[role="separator"]'))
      .toHaveAttribute("aria-orientation", "horizontal");

    logs.mockResolvedValue("ready in 300ms\nnpm ERR! oh no\nWARN slow build\n");
    // The drawer polls on the source's 1000 ms cadence, which is the default waitFor budget itself.
    await waitFor(
      () => expect(screen.getByText("npm ERR! oh no")).toHaveClass("browser-panel__log-line", "is-error"),
      { timeout: 4000 }
    );
    expect(screen.getByText("WARN slow build")).toHaveClass("browser-panel__log-line", "is-warn");
    expect(screen.getByText("ready in 300ms").className).toBe("browser-panel__log-line");

    await user.click(trigger);
    expect(await screen.findByRole("menuitem", { name: /Hide dev server logs/ })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Close dev server logs" }));
    await waitFor(() => expect(document.querySelector(".browser-panel__log-drawer")).toBeNull());
  });

  it("reports the pane height the log drawer takes away from the native page", async () => {
    const user = userEvent.setup();
    vi.spyOn(previewApi, "listPreviewConfigurations")
      .mockResolvedValue(configuration([configuredServer("web", 5173)]));
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([runningServer("web", 5173)]);
    vi.spyOn(previewApi, "readPreviewServerLogs").mockResolvedValue("No logs yet.");
    const reserved: number[] = [];
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const height = this.classList.contains("browser-panel__log-drawer") ? 200 : 0;
      return { x: 0, y: 0, left: 0, top: 0, right: 0, bottom: height, width: 0, height, toJSON: () => ({}) } as DOMRect;
    });

    render(
      <BrowserPanel {...pane}
        native
        sessionId="conversation-reserved"
        target={workspaceTarget}
        onReservedBottomChange={(value) => reserved.push(value)}
      />
    );
    const trigger = screen.getByRole("button", { name: "Servers & settings" });
    await waitFor(() => expect(trigger).toBeEnabled());
    expect(reserved.at(-1)).toBe(0);

    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /Show dev server logs/ }));
    await waitFor(() => expect(reserved.at(-1)).toBe(200));
  });

  it("keeps the native toolbar free of viewport and external-browser controls", () => {
    render(<BrowserPanel {...pane} native sessionId="conversation-viewport" />);

    expect(screen.queryByRole("button", { name: "Viewport" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open in external browser" })).not.toBeInTheDocument();
  });
});
