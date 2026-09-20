import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backendMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  hasBackendRuntime: vi.fn(() => true)
}));

vi.mock("./backend", () => ({
  invoke: backendMocks.invoke,
  hasBackendRuntime: backendMocks.hasBackendRuntime
}));

import { externalHttpUrl, installExternalLinkInterceptor, openExternalUrl } from "./externalLinks";

/** jsdom's default address, used to identify the application's own origin. */
const APP_ORIGIN = "http://localhost:3000";

function anchor(href: string): HTMLAnchorElement {
  const element = document.createElement("a");
  element.setAttribute("href", href);
  element.textContent = "link";
  document.body.append(element);
  return element;
}

function clickEvent(overrides: MouseEventInit = {}): MouseEvent {
  return new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ...overrides });
}

describe("externalHttpUrl", () => {
  it("accepts absolute remote http(s) addresses", () => {
    expect(externalHttpUrl("https://example.com")).toBe("https://example.com/");
    expect(externalHttpUrl("  https://example.com/docs?a=1#b  ")).toBe("https://example.com/docs?a=1#b");
    expect(externalHttpUrl("http://example.com:8443/p")).toBe("http://example.com:8443/p");
  });

  it("rejects every scheme other than http(s)", () => {
    for (const candidate of [
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "file:///C:/Windows/System32/calc.exe",
      "vbscript:msgbox(1)",
      "ms-settings:privacy",
      "mailto:a@b.c",
      "about:blank",
      "blob:https://example.com/x"
    ]) {
      expect(externalHttpUrl(candidate), candidate).toBeNull();
    }
  });

  it("rejects relative links and the application's own origin", () => {
    // Relative URLs resolve to the application's origin, so both cases use the same criterion.
    expect(externalHttpUrl("/settings")).toBeNull();
    expect(externalHttpUrl("#section")).toBeNull();
    expect(externalHttpUrl("docs/readme")).toBeNull();
    expect(externalHttpUrl(`${APP_ORIGIN}/index.html`)).toBeNull();
  });

  it("rejects empty and non-string input", () => {
    expect(externalHttpUrl("")).toBeNull();
    expect(externalHttpUrl("   ")).toBeNull();
    expect(externalHttpUrl(null)).toBeNull();
    expect(externalHttpUrl(undefined)).toBeNull();
    expect(externalHttpUrl("https://")).toBeNull();
  });
});

describe("openExternalUrl", () => {
  beforeEach(() => {
    backendMocks.invoke.mockReset().mockResolvedValue(undefined);
    backendMocks.hasBackendRuntime.mockReset().mockReturnValue(true);
  });

  it("hands the normalized address to the host command", async () => {
    await openExternalUrl("  https://example.com/a b  ");
    expect(backendMocks.invoke).toHaveBeenCalledWith("open_external_url", {
      url: "https://example.com/a%20b"
    });
  });

  it("refuses anything that is not an external http(s) address", async () => {
    await expect(openExternalUrl("javascript:alert(1)")).rejects.toThrow();
    await expect(openExternalUrl("/settings")).rejects.toThrow();
    expect(backendMocks.invoke).not.toHaveBeenCalled();
  });

  it("falls back to a browser tab when no Rust backend is attached", async () => {
    backendMocks.hasBackendRuntime.mockReturnValue(false);
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    await openExternalUrl("https://example.com/");
    expect(open).toHaveBeenCalledWith("https://example.com/", "_blank", "noopener,noreferrer");
    expect(backendMocks.invoke).not.toHaveBeenCalled();
    open.mockRestore();
  });
});

describe("installExternalLinkInterceptor", () => {
  let opened: string[];
  let uninstall: () => void;

  beforeEach(() => {
    opened = [];
    uninstall = installExternalLinkInterceptor(document, async (url) => {
      opened.push(url);
    });
  });

  afterEach(() => {
    uninstall();
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("takes over a plain left click on an external anchor", () => {
    const link = anchor("https://example.com/docs");
    const event = clickEvent();
    link.dispatchEvent(event);
    expect(opened).toEqual(["https://example.com/docs"]);
    // preventDefault prevents duplicate opening; the host's on_new_window fallback
    // runs only when the default behavior proceeds.
    expect(event.defaultPrevented).toBe(true);
  });

  it("takes over a middle click, which WebView2 would otherwise drop", () => {
    const link = anchor("https://example.com/");
    const event = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });
    link.dispatchEvent(event);
    expect(opened).toEqual(["https://example.com/"]);
    expect(event.defaultPrevented).toBe(true);
  });

  it("finds the anchor when the click lands on a child element", () => {
    const link = anchor("https://example.com/");
    const icon = document.createElement("span");
    link.append(icon);
    icon.dispatchEvent(clickEvent());
    expect(opened).toEqual(["https://example.com/"]);
  });

  it("leaves in-app and non-http links completely alone", () => {
    for (const href of ["/settings", "#anchor", "javascript:alert(1)", "mailto:a@b.c", `${APP_ORIGIN}/x`]) {
      document.body.replaceChildren();
      const link = anchor(href);
      const event = clickEvent();
      link.dispatchEvent(event);
      expect(opened, href).toEqual([]);
      expect(event.defaultPrevented, href).toBe(false);
    }
  });

  it("ignores clicks that are not a plain activation", () => {
    const link = anchor("https://example.com/");
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }, { shiftKey: true }, { altKey: true }, { button: 2 }]) {
      const event = clickEvent(modifier);
      link.dispatchEvent(event);
      expect(event.defaultPrevented, JSON.stringify(modifier)).toBe(false);
    }
    expect(opened).toEqual([]);
  });

  it("ignores a click something else already handled", () => {
    const link = anchor("https://example.com/");
    link.addEventListener("click", (event) => event.preventDefault());
    // The target listener runs during bubbling, so defaultPrevented must be set before capture.
    const event = clickEvent();
    event.preventDefault();
    link.dispatchEvent(event);
    expect(opened).toEqual([]);
  });

  it("reports a failed open instead of throwing into the event loop", async () => {
    uninstall();
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    uninstall = installExternalLinkInterceptor(document, async () => {
      throw new Error("nope");
    });
    anchor("https://example.com/").dispatchEvent(clickEvent());
    await Promise.resolve();
    await Promise.resolve();
    expect(error).toHaveBeenCalled();
  });

  it("stops intercepting once uninstalled", () => {
    const link = anchor("https://example.com/");
    uninstall();
    const event = clickEvent();
    link.dispatchEvent(event);
    expect(opened).toEqual([]);
    expect(event.defaultPrevented).toBe(false);
    uninstall = () => undefined;
  });
});
