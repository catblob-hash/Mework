import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "./i18n";
import { defaultAppearancePreferences, getAppearanceSnapshot } from "./lib/appearance";
import {
  applyResolvedApplicationAppearance,
  configureApplicationAppearance,
  resolveApplicationTheme,
  startApplicationAppearance,
  stopApplicationAppearance
} from "./theme";

afterEach(() => {
  stopApplicationAppearance();
  configureI18n("zh-CN");
  document.documentElement.lang = "";
  delete document.documentElement.dataset.theme;
  delete document.documentElement.dataset.themePreference;
  document.documentElement.style.removeProperty("color-scheme");
  for (const property of ["--accent", "--accent-soft", "--focus", "--app-font-family", "--mono", "--message-font-family", "--message-font-size", "zoom"]) {
    document.documentElement.style.removeProperty(property);
  }
  for (const attribute of ["wideMessages", "codeWrap", "codeCollapse"]) {
    delete document.documentElement.dataset[attribute];
  }
  document.querySelectorAll('meta[data-theme-test="true"]').forEach((item) => item.remove());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function testMeta(name: string): HTMLMetaElement {
  const meta = document.createElement("meta");
  meta.name = name;
  meta.dataset.themeTest = "true";
  document.head.append(meta);
  return meta;
}

describe("application theme", () => {
  it("resolves explicit and system theme preferences", () => {
    expect(resolveApplicationTheme("day", true)).toBe("day");
    expect(resolveApplicationTheme("night", false)).toBe("night");
    expect(resolveApplicationTheme("system", false)).toBe("day");
    expect(resolveApplicationTheme("system", true)).toBe("night");
  });

  it("applies language, color scheme, data attributes, and theme metadata", () => {
    const colorScheme = testMeta("color-scheme");
    const themeColor = testMeta("theme-color");

    applyResolvedApplicationAppearance(
      "night",
      "en-US",
      "system",
      document.documentElement
    );

    expect(document.documentElement.lang).toBe("en-US");
    expect(document.documentElement.dataset.theme).toBe("night");
    expect(document.documentElement.dataset.themePreference).toBe("system");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    expect(colorScheme.content).toBe("dark");
    expect(themeColor.content).toBe("#08080a");
  });

  it("routes the schema-91 appearance block through the same single entry point", () => {
    configureApplicationAppearance({
      appLanguage: "en-US",
      theme: "night",
      appearance: {
        ...defaultAppearancePreferences(),
        themeColor: "#00b96b",
        zoom: 1.2,
        messageFontSize: 18,
        wideMessages: true,
        codeBlockWrappable: true
      }
    });

    const root = document.documentElement;
    expect(root.lang).toBe("en-US");
    expect(root.dataset.theme).toBe("night");
    expect(root.style.getPropertyValue("--accent")).toBe("#00B96B");
    expect(root.style.getPropertyValue("--message-font-size")).toBe("18px");
    expect(root.style.getPropertyValue("zoom")).toBe("1.2");
    expect(root.dataset.wideMessages).toBe("true");
    expect(root.dataset.codeWrap).toBe("true");
    expect(root.dataset.codeCollapse).toBe("false");
    // Deep rendering components read the shared snapshot instead of receiving threaded props.
    expect(getAppearanceSnapshot().messageFontSize).toBe(18);
  });

  it("falls back to the factory appearance when the caller has no document yet", () => {
    configureApplicationAppearance({ appLanguage: "zh-CN", theme: "day" });
    expect(getAppearanceSnapshot()).toEqual(defaultAppearancePreferences());
    expect(document.documentElement.style.getPropertyValue("--accent")).toBe("");
  });

  it("tracks live system-theme changes while system mode is selected", () => {
    const changeListener: {
      current: ((event: MediaQueryListEvent) => void) | null;
    } = { current: null };
    const media = {
      matches: false,
      media: "(prefers-color-scheme: dark)",
      onchange: null,
      addEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
        changeListener.current = listener;
      }),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn()
    } as unknown as MediaQueryList;
    vi.stubGlobal("matchMedia", vi.fn(() => media));

    startApplicationAppearance(
      { appLanguage: "zh-CN", theme: "system" },
      document.documentElement
    );
    expect(document.documentElement.dataset.theme).toBe("day");

    changeListener.current?.({ matches: true } as MediaQueryListEvent);

    expect(document.documentElement.dataset.theme).toBe("night");
  });
});
