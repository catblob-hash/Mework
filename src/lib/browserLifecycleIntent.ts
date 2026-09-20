export const BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY =
  "mework.browser-lifecycle-intent-epoch.v1";

const EPOCHS_PER_MILLISECOND = 1024;
const MAX_EPOCH = Number.MAX_SAFE_INTEGER;

type EpochStorage = Pick<Storage, "getItem" | "setItem">;

function defaultStorage(): EpochStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

function parseStoredEpoch(value: string | null): number {
  if (!value || !/^[1-9]\d*$/.test(value)) return 0;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : 0;
}

function clockEpoch(nowMs: number): number {
  if (!Number.isFinite(nowMs) || nowMs <= 0) return 1;
  const milliseconds = Math.floor(nowMs);
  if (milliseconds > Math.floor(MAX_EPOCH / EPOCHS_PER_MILLISECOND)) {
    throw new Error("Browser lifecycle intent clock is outside the supported range.");
  }
  return Math.max(1, milliseconds * EPOCHS_PER_MILLISECOND);
}

export function createBrowserLifecycleIntentEpochIssuer(options?: {
  now?: () => number;
  storage?: () => EpochStorage | null;
}): () => number {
  const now = options?.now ?? Date.now;
  const storage = options?.storage ?? defaultStorage;
  let lastIssued = 0;

  return () => {
    const candidateFromClock = clockEpoch(now());
    let persisted = 0;
    let targetStorage: EpochStorage | null = null;
    try {
      targetStorage = storage();
      persisted = parseStoredEpoch(
        targetStorage?.getItem(BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY) ?? null
      );
    } catch {
      targetStorage = null;
    }

    const previous = Math.max(lastIssued, persisted);
    if (previous >= MAX_EPOCH) {
      throw new Error("Browser lifecycle intent epoch space is exhausted.");
    }
    const next = Math.max(candidateFromClock, previous + 1);
    if (!Number.isSafeInteger(next) || next <= 0) {
      throw new Error("Browser lifecycle intent epoch is invalid.");
    }
    lastIssued = next;

    try {
      targetStorage?.setItem(BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY, String(next));
    } catch {
      // A monotonic in-memory epoch still protects this renderer when storage is unavailable.
    }
    return next;
  };
}

export const issueBrowserLifecycleIntentEpoch =
  createBrowserLifecycleIntentEpochIssuer();
