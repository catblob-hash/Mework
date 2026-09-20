import { useSyncExternalStore } from "react";
import type { AppearancePreferences, KeyToken } from "../types";

/**
 * Apply appearance preferences to the DOM.
 *
 * Use CSSOM exclusively and never inject `<style>` elements: production CSP
 * permits neither inline style elements nor attributes, while CSSOM remains
 * available for custom CSS.
 */

/** Theme color presets. They live in TypeScript because the palette guard scans CSS files. */
export const THEME_COLOR_PRESETS: readonly string[] = [
  "#356AE6",
  "#00B96B",
  "#EF4444",
  "#F59E0B",
  "#8B5CF6"
];

/** UI font candidates. The empty first item represents the default. */
export const UI_FONT_PRESETS: readonly string[] = [
  "",
  "Inter",
  "Segoe UI",
  "Microsoft YaHei",
  "Noto Sans SC",
  "Source Han Sans SC",
  "PingFang SC",
  "Arial"
];

/** Monospace font candidates. The empty first item represents the default. */
export const MONO_FONT_PRESETS: readonly string[] = [
  "",
  "Cascadia Code",
  "Cascadia Mono",
  "Consolas",
  "JetBrains Mono",
  "Fira Code",
  "Sarasa Mono SC"
];

export const MIN_MESSAGE_FONT_SIZE = 12;
export const MAX_MESSAGE_FONT_SIZE = 22;
export const MIN_ZOOM = 0.5;
export const MAX_ZOOM = 2;
export const ZOOM_STEP = 0.1;

/**
 * Factory defaults for appearance preferences.
 *
 * This is the sole source of default values for seed documents and global-setting
 * normalization. Rust `AppearancePreferences::default` must match field-for-field
 * so both sides normalize the same document identically.
 */
export function defaultAppearancePreferences(): AppearancePreferences {
  return {
    themeColor: "",
    zoom: 1,
    uiFontFamily: "",
    monoFontFamily: "",
    messageFontSize: 14,
    serifMessages: false,
    wideMessages: false,
    sendShortcut: ["Enter"],
    newlineShortcut: ["Shift", "Enter"],
    spellCheck: false,
    renderUserMarkdown: false,
    confirmMessageDelete: false,
    collapseReasoning: true,
    codeBlockCollapsible: false,
    codeBlockWrappable: false,
    singleDollarMath: true,
    customCss: ""
  };
}

/** Available composer send and newline shortcuts, in display order. */
export const COMPOSER_SHORTCUT_CHOICES: readonly KeyToken[][] = [
  ["Enter"],
  ["Shift", "Enter"],
  ["Control", "Enter"],
  ["Alt", "Enter"]
];

/**
 * Normalize a user-entered hexadecimal color using Cherry Studio's rules:
 * trim whitespace, add `#`, expand three digits, reject non-six-digit values,
 * and return uppercase. `null` preserves the draft instead of persisting it.
 */
export function normalizeHexColor(input: string): string | null {
  const trimmed = input.trim();
  if (!trimmed) return "";
  const body = trimmed.startsWith("#") ? trimmed.slice(1) : trimmed;
  if (!/^[0-9a-fA-F]+$/.test(body)) return null;
  const expanded = body.length === 3
    ? body.split("").map((character) => character.repeat(2)).join("")
    : body;
  if (expanded.length !== 6) return null;
  return `#${expanded.toUpperCase()}`;
}

export function clampZoom(value: number): number {
  if (!Number.isFinite(value)) return 1;
  // Round to one decimal place so floating-point accumulation preserves 100%.
  return Math.round(Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, value)) * 10) / 10;
}

export function clampMessageFontSize(value: number): number {
  if (!Number.isFinite(value)) return 14;
  return Math.min(MAX_MESSAGE_FONT_SIZE, Math.max(MIN_MESSAGE_FONT_SIZE, Math.round(value)));
}

/** Convert a font family to CSS `font-family`; an empty family uses the fallback stack. */
function fontFamilyValue(family: string, fallback: string): string {
  const trimmed = family.trim();
  if (!trimmed) return fallback;
  // Preserve quoted names and font stacks; quote only a single bare family name.
  if (trimmed.includes(",") || trimmed.includes('"')) return trimmed;
  return `"${trimmed}", ${fallback}`;
}

const DEFAULT_UI_FONT_STACK =
  'Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans SC", sans-serif';
const DEFAULT_MONO_FONT_STACK =
  '"Cascadia Code", "Cascadia Mono", "SFMono-Regular", Consolas, monospace';
const SERIF_FONT_STACK =
  'Georgia, "Times New Roman", "Noto Serif SC", "Source Han Serif SC", serif';

let customCssSheet: CSSStyleSheet | null = null;
let lastCustomCss: string | null = null;

/**
 * Singleton snapshot and subscription for active appearance preferences.
 *
 * Deep rendering components need these preferences without threading booleans
 * through multiple prop chains. `applyAppearance`, called only by
 * `configureApplicationAppearance`, is the sole writer, so the snapshot always
 * matches the applied DOM preferences.
 */
let currentAppearance: AppearancePreferences = defaultAppearancePreferences();
const appearanceListeners = new Set<() => void>();

function publishAppearance(next: AppearancePreferences): void {
  currentAppearance = next;
  for (const listener of appearanceListeners) listener();
}

export function subscribeAppearance(listener: () => void): () => void {
  appearanceListeners.add(listener);
  return () => {
    appearanceListeners.delete(listener);
  };
}

export function getAppearanceSnapshot(): AppearancePreferences {
  return currentAppearance;
}

/** Read active appearance preferences. Snapshot identity changes only after `applyAppearance`. */
export function useAppearance(): AppearancePreferences {
  return useSyncExternalStore(subscribeAppearance, getAppearanceSnapshot, getAppearanceSnapshot);
}

/**
 * Apply custom CSS through a constructable stylesheet. It is a CSSOM object and
 * therefore remains usable under the production style CSP. Unsupported runtimes
 * silently skip this enhancement rather than weakening CSP.
 */
function applyCustomCss(css: string): void {
  if (typeof document === "undefined") return;
  if (css === lastCustomCss) return;
  lastCustomCss = css;
  if (typeof CSSStyleSheet === "undefined" || !("adoptedStyleSheets" in document)) return;
  if (!customCssSheet) {
    try {
      customCssSheet = new CSSStyleSheet();
    } catch {
      customCssSheet = null;
      return;
    }
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, customCssSheet];
  }
  try {
    customCssSheet.replaceSync(css);
  } catch {
    // Keep the last valid CSS when user CSS has a syntax error.
  }
}

export function applyAppearance(appearance: AppearancePreferences): void {
  publishAppearance(appearance);
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const style = root.style;

  const themeColor = normalizeHexColor(appearance.themeColor);
  if (themeColor) {
    style.setProperty("--accent", themeColor);
    // Derive the soft background and focus ring from the accent; eight-digit
    // hexadecimal alpha is CSP-independent.
    style.setProperty("--accent-soft", `${themeColor}1F`);
    style.setProperty("--focus", themeColor);
  } else {
    style.removeProperty("--accent");
    style.removeProperty("--accent-soft");
    style.removeProperty("--focus");
  }

  style.setProperty("--app-font-family", fontFamilyValue(appearance.uiFontFamily, DEFAULT_UI_FONT_STACK));
  style.setProperty("--mono", fontFamilyValue(appearance.monoFontFamily, DEFAULT_MONO_FONT_STACK));
  style.setProperty(
    "--message-font-family",
    appearance.serifMessages ? SERIF_FONT_STACK : "var(--app-font-family)"
  );
  style.setProperty("--message-font-size", `${clampMessageFontSize(appearance.messageFontSize)}px`);
  // Use `zoom`, not `transform: scale()`, so layout, hit testing, and scrollbars scale too.
  style.setProperty("zoom", String(clampZoom(appearance.zoom)));

  root.dataset.wideMessages = appearance.wideMessages ? "true" : "false";
  root.dataset.codeWrap = appearance.codeBlockWrappable ? "true" : "false";
  root.dataset.codeCollapse = appearance.codeBlockCollapsible ? "true" : "false";

  applyCustomCss(appearance.customCss);
}
