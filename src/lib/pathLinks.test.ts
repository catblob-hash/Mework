import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backendMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  hasBackendRuntime: vi.fn(() => true)
}));

vi.mock("./backend", () => ({
  invoke: backendMocks.invoke,
  hasBackendRuntime: backendMocks.hasBackendRuntime
}));

import { installExternalLinkInterceptor } from "./externalLinks";
import {
  installPathLinkInterceptor,
  machineFromKey,
  revealPath,
  setPathOpenHandler,
  setPathPrefetchHandler
} from "./pathLinks";

/** Where jsdom puts every box: nowhere. */
const NO_BOX = { left: 0, top: 0, right: 0, bottom: 0 };

function pathButton(path: string, baseDir?: string, line?: number): HTMLButtonElement {
  const host = document.createElement("div");
  if (baseDir) host.setAttribute("data-mework-path-base", baseDir);
  const button = document.createElement("button");
  button.setAttribute("data-mework-path", path);
  if (line !== undefined) button.setAttribute("data-mework-path-line", String(line));
  button.textContent = path;
  host.append(button);
  document.body.append(host);
  return button;
}

function clickEvent(overrides: MouseEventInit = {}): MouseEvent {
  return new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ...overrides });
}

describe("revealPath", () => {
  beforeEach(() => {
    backendMocks.invoke.mockReset().mockResolvedValue(undefined);
    backendMocks.hasBackendRuntime.mockReset().mockReturnValue(true);
  });

  it("hands the path and base directory to the host command", async () => {
    await revealPath("src/App.tsx", "C:\\work\\mework");
    expect(backendMocks.invoke).toHaveBeenCalledWith("reveal_path_in_file_manager", {
      path: "src/App.tsx",
      baseDir: "C:\\work\\mework"
    });
  });

  it("has no browser fallback, because a preview has no file manager", async () => {
    backendMocks.hasBackendRuntime.mockReturnValue(false);
    await expect(revealPath("/etc/hosts", null)).rejects.toThrow();
    expect(backendMocks.invoke).not.toHaveBeenCalled();
  });
});

describe("installPathLinkInterceptor", () => {
  let revealed: Array<{ path: string; baseDir: string | null }>;
  let uninstall: () => void;

  beforeEach(() => {
    revealed = [];
    uninstall = installPathLinkInterceptor(document, async (path, baseDir) => {
      revealed.push({ path, baseDir });
    });
  });

  afterEach(() => {
    uninstall();
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("reveals a relative path against the nearest base directory", () => {
    const button = pathButton("src/App.tsx", "C:\\work\\mework");
    const event = clickEvent();
    button.dispatchEvent(event);
    expect(revealed).toEqual([{ path: "src/App.tsx", baseDir: "C:\\work\\mework" }]);
    expect(event.defaultPrevented).toBe(true);
  });

  it("reveals an absolute path even without a base directory", () => {
    pathButton("/etc/hosts").dispatchEvent(clickEvent());
    pathButton("C:\\Windows\\notepad.exe").dispatchEvent(clickEvent());
    expect(revealed).toEqual([
      { path: "/etc/hosts", baseDir: null },
      { path: "C:\\Windows\\notepad.exe", baseDir: null }
    ]);
  });

  it("skips a relative path that has no base directory to resolve against", () => {
    const button = pathButton("src/App.tsx");
    const event = clickEvent();
    button.dispatchEvent(event);
    expect(revealed).toEqual([]);
    expect(event.defaultPrevented).toBe(false);
  });

  it("finds the owner when the click lands on a child element", () => {
    const button = pathButton("/etc/hosts");
    const code = document.createElement("code");
    button.append(code);
    code.dispatchEvent(clickEvent());
    expect(revealed).toEqual([{ path: "/etc/hosts", baseDir: null }]);
  });

  it("ignores clicks that are not a plain activation", () => {
    const button = pathButton("/etc/hosts");
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }, { shiftKey: true }, { altKey: true }, { button: 2 }]) {
      const event = clickEvent(modifier);
      button.dispatchEvent(event);
      expect(event.defaultPrevented, JSON.stringify(modifier)).toBe(false);
    }
    expect(revealed).toEqual([]);
  });

  it("reports a failed reveal instead of throwing into the event loop", async () => {
    uninstall();
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    uninstall = installPathLinkInterceptor(document, async () => {
      throw new Error("nope");
    });
    pathButton("/etc/hosts").dispatchEvent(clickEvent());
    await Promise.resolve();
    await Promise.resolve();
    expect(error).toHaveBeenCalled();
  });

  it("stops intercepting once uninstalled", () => {
    const button = pathButton("/etc/hosts");
    uninstall();
    const event = clickEvent();
    button.dispatchEvent(event);
    expect(revealed).toEqual([]);
    expect(event.defaultPrevented).toBe(false);
    uninstall = () => undefined;
  });

  /**
   * The two document-level interceptors must not see each other's nodes, which
   * is why paths render as buttons rather than anchors.
   */
  it("does not compete with the external-link interceptor", () => {
    const opened: string[] = [];
    const uninstallExternal = installExternalLinkInterceptor(document, async (url) => {
      opened.push(url);
    });
    try {
      pathButton("/etc/hosts").dispatchEvent(clickEvent());
      expect(opened).toEqual([]);

      const link = document.createElement("a");
      link.setAttribute("href", "https://example.com/docs");
      document.body.append(link);
      link.dispatchEvent(clickEvent());
      expect(opened).toEqual(["https://example.com/docs"]);
      expect(revealed).toEqual([{ path: "/etc/hosts", baseDir: null }]);
    } finally {
      uninstallExternal();
    }
  });
});

/**
 * The app decides first, and the file manager is what is left when it cannot.
 * Both halves matter: a handler that swallowed the clicks it could not serve
 * would leave a path outside the workspace doing nothing at all.
 */
describe("setPathOpenHandler", () => {
  let revealed: Array<{ path: string; baseDir: string | null }>;
  let uninstall: () => void;
  let unregister: () => void;

  beforeEach(() => {
    revealed = [];
    unregister = () => undefined;
    uninstall = installPathLinkInterceptor(document, async (path, baseDir) => {
      revealed.push({ path, baseDir });
    });
  });

  afterEach(() => {
    unregister();
    uninstall();
    document.body.replaceChildren();
  });

  it("takes the click, with the base directory and the line the display named", () => {
    const requests: unknown[] = [];
    unregister = setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    });

    const event = clickEvent();
    pathButton("src/App.tsx", "C:\\work\\mework", 12).dispatchEvent(event);

    expect(requests).toEqual([{ path: "src/App.tsx", baseDir: "C:\\work\\mework", line: 12, anchor: NO_BOX }]);
    expect(revealed).toEqual([]);
    expect(event.defaultPrevented).toBe(true);
  });

  it("falls through to the file manager when it declines", () => {
    unregister = setPathOpenHandler(() => false);

    pathButton("/etc/hosts").dispatchEvent(clickEvent());

    expect(revealed).toEqual([{ path: "/etc/hosts", baseDir: null }]);
  });

  /** A relative path is the case the reveal fallback cannot serve, and the pane can. */
  it("is offered a relative path the file manager would have skipped", () => {
    const requests: unknown[] = [];
    unregister = setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    });

    const event = clickEvent();
    pathButton("src/App.tsx").dispatchEvent(event);

    expect(requests).toEqual([{ path: "src/App.tsx", baseDir: null, line: null, anchor: NO_BOX }]);
    expect(event.defaultPrevented).toBe(true);
  });

  /** A document in the file pane says which machine its paths are on. */
  it("carries the machine of the surface that showed the path", () => {
    const requests: unknown[] = [];
    unregister = setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    });
    const button = pathButton("docs/x.md", "/srv/app");
    button.parentElement!.setAttribute("data-mework-path-machine", "ssh:devbox");

    button.dispatchEvent(clickEvent());

    expect(requests).toEqual([{
      path: "docs/x.md",
      baseDir: "/srv/app",
      line: null,
      machine: { kind: "ssh", machineId: "devbox" },
      anchor: NO_BOX
    }]);
    expect(machineFromKey("local")).toBeNull();
    expect(machineFromKey("wsl:Ubuntu")).toEqual({ kind: "wsl", distro: "Ubuntu" });
    expect(machineFromKey("elsewhere")).toBeUndefined();
  });

  /** The pointer resting on a path is the head start a probe of where it is needs. */
  it("tells the prefetch handler once per link the pointer rests on", () => {
    vi.useFakeTimers();
    try {
      const hovered: string[] = [];
      const removePrefetch = setPathPrefetchHandler((request) => hovered.push(request.path));
      const first = pathButton("src/a.ts");
      const second = pathButton("src/b.ts");
      const over = (element: HTMLElement) => element.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));

      over(first);
      over(first);
      vi.advanceTimersByTime(100);
      // Passed over on the way to the next one: not looked up.
      over(second);
      vi.advanceTimersByTime(20);
      over(document.body);
      vi.advanceTimersByTime(100);
      over(first);
      vi.advanceTimersByTime(100);
      removePrefetch();
      over(second);
      vi.advanceTimersByTime(100);

      expect(hovered).toEqual(["src/a.ts", "src/a.ts"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops being consulted once unregistered", () => {
    const requests: unknown[] = [];
    setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    })();

    pathButton("/etc/hosts").dispatchEvent(clickEvent());

    expect(requests).toEqual([]);
    expect(revealed).toEqual([{ path: "/etc/hosts", baseDir: null }]);
  });
});
