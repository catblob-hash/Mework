import { describe, expect, it } from "vitest";
import {
  BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY,
  createBrowserLifecycleIntentEpochIssuer
} from "./browserLifecycleIntent";

function memoryStorage(initial?: string) {
  let value = initial ?? null;
  return {
    getItem: (key: string) => (
      key === BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY ? value : null
    ),
    setItem: (key: string, next: string) => {
      if (key === BROWSER_LIFECYCLE_INTENT_EPOCH_STORAGE_KEY) value = next;
    },
    value: () => value
  };
}

describe("browser lifecycle intent epochs", () => {
  it("orders multiple intents issued within the same millisecond", () => {
    const storage = memoryStorage();
    const issue = createBrowserLifecycleIntentEpochIssuer({
      now: () => 1_000,
      storage: () => storage
    });

    expect(issue()).toBe(1_024_000);
    expect(issue()).toBe(1_024_001);
    expect(storage.value()).toBe("1024001");
  });

  it("continues above the persisted epoch after a renderer reload", () => {
    const storage = memoryStorage();
    const firstRenderer = createBrowserLifecycleIntentEpochIssuer({
      now: () => 2_000,
      storage: () => storage
    });
    const oldEpoch = firstRenderer();
    const reloadedRenderer = createBrowserLifecycleIntentEpochIssuer({
      now: () => 2_000,
      storage: () => storage
    });

    expect(reloadedRenderer()).toBe(oldEpoch + 1);
  });

  it("survives a wall-clock rollback by honoring persisted authority", () => {
    const storage = memoryStorage("3072000");
    const issue = createBrowserLifecycleIntentEpochIssuer({
      now: () => 1_000,
      storage: () => storage
    });

    expect(issue()).toBe(3_072_001);
  });

  it("uses a renderer-local monotonic fallback when storage is unavailable", () => {
    const issue = createBrowserLifecycleIntentEpochIssuer({
      now: () => 4_000,
      storage: () => {
        throw new Error("storage denied");
      }
    });

    expect(issue()).toBe(4_096_000);
    expect(issue()).toBe(4_096_001);
  });

  it("ignores malformed persisted values", () => {
    const storage = memoryStorage("not-an-epoch");
    const issue = createBrowserLifecycleIntentEpochIssuer({
      now: () => 5_000,
      storage: () => storage
    });

    expect(issue()).toBe(5_120_000);
  });

  it("fails closed instead of emitting an unsafe integer", () => {
    const storage = memoryStorage(String(Number.MAX_SAFE_INTEGER));
    const issue = createBrowserLifecycleIntentEpochIssuer({
      now: () => 1,
      storage: () => storage
    });

    expect(issue).toThrow("epoch space is exhausted");
  });
});
