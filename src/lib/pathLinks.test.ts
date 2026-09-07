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
import { installPathLinkInterceptor, revealPath } from "./pathLinks";

function pathButton(path: string, baseDir?: string): HTMLButtonElement {
  const host = document.createElement("div");
  if (baseDir) host.setAttribute("data-mework-path-base", baseDir);
  const button = document.createElement("button");
  button.setAttribute("data-mework-path", path);
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
