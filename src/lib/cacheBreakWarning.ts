/**
 * Whether to warn before a settings change that would throw a warm prompt cache
 * away (`lockTouch` in `toolLock.ts`).
 *
 * The warning is said once per conversation: after the first answer, the user
 * knows what an orange row means there. Which conversations have heard it is
 * kept for this session of the app only — the cache it is about does not
 * outlive the session by much either. "Don't show again" is a view preference
 * of the same class as a catalog's row order, and lives in `localStorage`
 * beside those.
 */
const STORAGE_KEY = "mework.cache-break-warning.v1";

const acknowledged = new Set<string>();

function suppressed(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(STORAGE_KEY) === "off";
  } catch {
    return false;
  }
}

export function shouldWarnCacheBreak(conversationId: string): boolean {
  return !acknowledged.has(conversationId) && !suppressed();
}

/** The user went ahead: no second warning in this conversation, and none anywhere if `never`. */
export function acknowledgeCacheBreak(conversationId: string, never: boolean): void {
  acknowledged.add(conversationId);
  if (!never || typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, "off");
  } catch {
    // An unavailable store costs the preference, never the change.
  }
}

/** For tests: forget every answer. */
export function resetCacheBreakWarnings(): void {
  acknowledged.clear();
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing to forget.
  }
}
