import { useMemo, useSyncExternalStore } from "react";
import type { AppLanguage, ResolvedAppLanguage } from "./types";

export type TranslationParameter = string | number;
export type TranslationParameters = Record<string, TranslationParameter>;

export interface I18nSnapshot {
  preference: AppLanguage;
  resolvedLanguage: ResolvedAppLanguage;
}

export type TranslationFunction = (
  zhCn: string,
  enUs: string,
  parameters?: TranslationParameters
) => string;

const listeners = new Set<() => void>();

// Component tests render components without main.tsx. Keep that surface stable in Chinese;
// the real application bootstrap explicitly configures `auto`.
let snapshot: I18nSnapshot = {
  preference: "zh-CN",
  resolvedLanguage: "zh-CN"
};

function browserLanguage(): string {
  return typeof navigator === "undefined" ? "" : navigator.language;
}

const chineseLanguageSubtags = new Set([
  "zh",
  "cmn",
  "yue",
  "wuu",
  "hak",
  "nan",
  "gan",
  "hsn",
  "cdo",
  "cjy",
  "cpx",
  "czh",
  "czo",
  "lzh",
  "ltc",
  "mnp",
  "och"
]);

/** Maps Chinese-family language tags to the shipped Simplified Chinese catalog and all others to English. */
export function resolveApplicationLanguage(
  preference: AppLanguage,
  systemLocale: string = browserLanguage()
): ResolvedAppLanguage {
  if (preference !== "auto") return preference;
  const primarySubtag = systemLocale.trim().replaceAll("_", "-").split("-", 1)[0]?.toLowerCase();
  return primarySubtag && chineseLanguageSubtags.has(primarySubtag)
    ? "zh-CN"
    : "en-US";
}

function emitIfChanged(next: I18nSnapshot): void {
  if (
    next.preference === snapshot.preference
    && next.resolvedLanguage === snapshot.resolvedLanguage
  ) {
    return;
  }
  snapshot = next;
  listeners.forEach((listener) => listener());
}

export function configureI18n(
  preference: AppLanguage,
  systemLocale: string = browserLanguage()
): ResolvedAppLanguage {
  const resolvedLanguage = resolveApplicationLanguage(preference, systemLocale);
  emitIfChanged({ preference, resolvedLanguage });
  return resolvedLanguage;
}

export function refreshAutomaticLanguage(systemLocale: string = browserLanguage()): void {
  if (snapshot.preference !== "auto") return;
  configureI18n("auto", systemLocale);
}

export function getI18nSnapshot(): I18nSnapshot {
  return snapshot;
}

export function subscribeI18n(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Keeps an `auto` preference synchronized with Windows/browser language changes. */
export function listenForSystemLanguageChanges(): () => void {
  if (typeof window === "undefined") return () => undefined;
  const onLanguageChange = () => refreshAutomaticLanguage();
  window.addEventListener("languagechange", onLanguageChange);
  return () => window.removeEventListener("languagechange", onLanguageChange);
}

export function interpolateTranslation(
  template: string,
  parameters?: TranslationParameters
): string {
  if (!parameters) return template;
  return template.replace(/\{([A-Za-z0-9_]+)\}/g, (placeholder, name: string) => (
    Object.hasOwn(parameters, name)
      ? String(parameters[name])
      : placeholder
  ));
}

export function translate(
  language: ResolvedAppLanguage,
  zhCn: string,
  enUs: string,
  parameters?: TranslationParameters
): string {
  return interpolateTranslation(language === "zh-CN" ? zhCn : enUs, parameters);
}

/** Non-React translation helper that follows the current global preference store. */
export const t: TranslationFunction = (zhCn, enUs, parameters) => (
  translate(snapshot.resolvedLanguage, zhCn, enUs, parameters)
);

export function useI18n(): {
  resolvedLanguage: ResolvedAppLanguage;
  t: TranslationFunction;
} {
  const current = useSyncExternalStore(
    subscribeI18n,
    getI18nSnapshot,
    getI18nSnapshot
  );
  return useMemo(() => ({
    resolvedLanguage: current.resolvedLanguage,
    t: (zhCn: string, enUs: string, parameters?: TranslationParameters) => (
      translate(current.resolvedLanguage, zhCn, enUs, parameters)
    )
  }), [current.resolvedLanguage]);
}
