/**
 * Small, best-effort preferences that belong to one pane.
 *
 * These are view settings, not data: a lost word-wrap flag costs a keystroke, so
 * storage that refuses to answer is not worth failing over. Private-mode windows
 * and embedded webviews both throw on access rather than returning null, which is
 * why every call is guarded rather than only the read.
 */

export function readStoredFlag(key: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  try {
    const stored = localStorage.getItem(key);
    return stored === null ? fallback : stored === "true";
  } catch {
    return fallback;
  }
}

export function writeStoredFlag(key: string, value: boolean): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // See the note above: a pane preference is not worth an exception.
  }
}

/** Reads one of `choices`, falling back when the stored value is stale or absent. */
export function readStoredChoice<T extends string>(
  key: string,
  choices: readonly T[],
  fallback: T
): T {
  if (typeof localStorage === "undefined") return fallback;
  try {
    const stored = localStorage.getItem(key);
    return choices.includes(stored as T) ? (stored as T) : fallback;
  } catch {
    return fallback;
  }
}

export function writeStoredChoice(key: string, value: string): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(key, value);
  } catch {
    // See the note above.
  }
}
