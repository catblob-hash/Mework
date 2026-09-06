import {
  configureI18n,
  getI18nSnapshot,
  listenForSystemLanguageChanges,
  subscribeI18n
} from "./i18n";
import type {
  AppLanguage,
  AppearancePreferences,
  GlobalSettings,
  ResolvedAppLanguage,
  ThemePreference
} from "./types";
import { applyAppearance, defaultAppearancePreferences } from "./lib/appearance";

export type ApplicationTheme = "day" | "night";
/**
 * `appearance` is optional because startup may run before persisted preferences load.
 * Use factory defaults until they are available.
 */
export type ApplicationAppearancePreferences = Pick<GlobalSettings, "appLanguage" | "theme"> & {
  appearance?: AppearancePreferences;
};

export const defaultApplicationAppearance: ApplicationAppearancePreferences = {
  appLanguage: "auto",
  theme: "system"
};

/** Compatibility export for code that still needs the pre-settings bootstrap theme. */
export const applicationTheme: ApplicationTheme = "day";

const themeColors: Record<ApplicationTheme, string> = {
  day: "#f7f7f5",
  night: "#08080a"
};

let preferences: ApplicationAppearancePreferences = { ...defaultApplicationAppearance };
let systemTheme: ApplicationTheme = "day";
let appearanceRoot: HTMLElement | null = null;
let started = false;
let stopMediaListener: (() => void) | null = null;
let stopLanguageListener: (() => void) | null = null;
let stopI18nSubscription: (() => void) | null = null;
let stopNativeThemeListener: (() => void) | null = null;
let nativeListenerGeneration = 0;

function browserPrefersDark(): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function hasTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function resolveApplicationTheme(
  preference: ThemePreference,
  prefersDark = browserPrefersDark()
): ApplicationTheme {
  if (preference === "day" || preference === "night") return preference;
  return prefersDark ? "night" : "day";
}

function updateMeta(name: string, content: string): void {
  if (typeof document === "undefined") return;
  document.querySelector<HTMLMetaElement>(`meta[name="${name}"]`)
    ?.setAttribute("content", content);
}

export function applyResolvedApplicationAppearance(
  theme: ApplicationTheme,
  language: ResolvedAppLanguage,
  preference: ThemePreference,
  root: HTMLElement = document.documentElement
): void {
  root.lang = language;
  root.dataset.theme = theme;
  root.dataset.themePreference = preference;
  root.style.colorScheme = theme === "night" ? "dark" : "light";
  updateMeta("color-scheme", theme === "night" ? "dark" : "light");
  updateMeta("theme-color", themeColors[theme]);
}

function renderApplicationAppearance(): void {
  if (typeof document === "undefined") return;
  const root = appearanceRoot ?? document.documentElement;
  const { resolvedLanguage } = getI18nSnapshot();
  applyResolvedApplicationAppearance(
    resolveApplicationTheme(preferences.theme, systemTheme === "night"),
    resolvedLanguage,
    preferences.theme,
    root
  );
}

/**
 * Applies the currently configured appearance once.
 *
 * Kept for compatibility with the previous bootstrap API; settings-aware code should call
 * `configureApplicationAppearance`.
 */
export function applyApplicationTheme(root: HTMLElement = document.documentElement): void {
  appearanceRoot = root;
  configureI18n(preferences.appLanguage);
  systemTheme = browserPrefersDark() ? "night" : "day";
  renderApplicationAppearance();
}

async function syncNativeWindowPreference(preference: ThemePreference): Promise<void> {
  if (!hasTauriRuntime()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().setTheme(
    preference === "system" ? null : preference === "night" ? "dark" : "light"
  );
}

async function connectNativeThemeListener(generation: number): Promise<void> {
  if (!hasTauriRuntime()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const currentWindow = getCurrentWindow();
  const initial = await currentWindow.theme();
  if (!started || generation !== nativeListenerGeneration) return;
  if (initial === "light" || initial === "dark") {
    systemTheme = initial === "dark" ? "night" : "day";
    renderApplicationAppearance();
  }
  const unlisten = await currentWindow.onThemeChanged(({ payload }) => {
    if (!started || generation !== nativeListenerGeneration) return;
    systemTheme = payload === "dark" ? "night" : "day";
    renderApplicationAppearance();
  });
  if (!started || generation !== nativeListenerGeneration) {
    unlisten();
    return;
  }
  stopNativeThemeListener?.();
  stopNativeThemeListener = unlisten;
}

/** Updates the singleton store; App can call this after its persisted document has loaded. */
export function configureApplicationAppearance(
  next: ApplicationAppearancePreferences
): void {
  preferences = { ...next };
  configureI18n(next.appLanguage);
  renderApplicationAppearance();
  // Apply all appearance settings here so startup and settings changes take effect immediately.
  applyAppearance(next.appearance ?? defaultAppearancePreferences());
  void syncNativeWindowPreference(next.theme).catch(() => undefined);
}

export function startApplicationAppearance(
  initial: ApplicationAppearancePreferences = defaultApplicationAppearance,
  root: HTMLElement = document.documentElement
): () => void {
  appearanceRoot = root;
  configureApplicationAppearance(initial);
  if (started) return stopApplicationAppearance;
  started = true;

  stopLanguageListener = listenForSystemLanguageChanges();
  stopI18nSubscription = subscribeI18n(renderApplicationAppearance);

  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    systemTheme = media.matches ? "night" : "day";
    const onChange = (event: MediaQueryListEvent) => {
      systemTheme = event.matches ? "night" : "day";
      renderApplicationAppearance();
    };
    media.addEventListener("change", onChange);
    stopMediaListener = () => media.removeEventListener("change", onChange);
  }

  nativeListenerGeneration += 1;
  void connectNativeThemeListener(nativeListenerGeneration).catch(() => undefined);
  renderApplicationAppearance();
  return stopApplicationAppearance;
}

export function stopApplicationAppearance(): void {
  if (!started) return;
  started = false;
  nativeListenerGeneration += 1;
  stopMediaListener?.();
  stopLanguageListener?.();
  stopI18nSubscription?.();
  stopNativeThemeListener?.();
  stopMediaListener = null;
  stopLanguageListener = null;
  stopI18nSubscription = null;
  stopNativeThemeListener = null;
}

export function getApplicationAppearancePreferences(): {
  appLanguage: AppLanguage;
  theme: ThemePreference;
  resolvedTheme: ApplicationTheme;
  resolvedLanguage: ResolvedAppLanguage;
} {
  return {
    ...preferences,
    resolvedTheme: resolveApplicationTheme(preferences.theme, systemTheme === "night"),
    resolvedLanguage: getI18nSnapshot().resolvedLanguage
  };
}
