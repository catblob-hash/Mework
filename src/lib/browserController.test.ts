import { describe, expect, it, vi } from "vitest";
import { createBrowserController } from "./browserController";
import type { BrowserStatus } from "./browser";

function status(overrides: Partial<BrowserStatus> = {}): BrowserStatus {
  return {
    hasPage: true,
    open: true,
    loading: false,
    url: "about:blank",
    title: "page",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 560, height: 720 },
    ...overrides
  } as BrowserStatus;
}

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("createBrowserController", () => {
  it("notifies on status and runtime-ready changes but not on no-ops", () => {
    const controller = createBrowserController();
    const listener = vi.fn();
    controller.subscribe(listener);

    controller.setRuntimeReady(false);
    expect(listener).not.toHaveBeenCalled();
    controller.setRuntimeReady(true);
    expect(listener).toHaveBeenCalledTimes(1);

    controller.updateStatuses((current) => current);
    expect(listener).toHaveBeenCalledTimes(1);
    controller.updateStatuses((current) => ({ ...current, "session-1": status() }));
    expect(listener).toHaveBeenCalledTimes(2);
    expect(controller.current().statuses["session-1"]).toBeDefined();
  });

  it("tracks the visible session with conditional clear and take", () => {
    const controller = createBrowserController();
    expect(controller.visibleSession()).toBeNull();
    controller.setVisibleSession("session-1");
    controller.clearVisibleSessionIf("session-2");
    expect(controller.visibleSession()).toBe("session-1");
    controller.clearVisibleSessionIf("session-1");
    expect(controller.visibleSession()).toBeNull();
    controller.setVisibleSession("session-3");
    expect(controller.takeVisibleSession()).toBe("session-3");
    expect(controller.visibleSession()).toBeNull();
  });

  it("treats a newer intent as invalidating older epochs and supports restore", () => {
    const controller = createBrowserController();
    const openEpoch = controller.issueIntent("session-1", "open");
    expect(controller.intentIsCurrent("session-1", openEpoch, "open")).toBe(true);
    expect(controller.intentIsCurrent("session-1", openEpoch, "closed")).toBe(false);

    const previous = controller.currentIntent("session-1");
    const closedEpoch = controller.issueIntent("session-1", "closed");
    expect(closedEpoch).toBeGreaterThan(openEpoch);
    expect(controller.intentIsCurrent("session-1", openEpoch, "open")).toBe(false);
    expect(controller.currentIntent("session-1")?.desired).toBe("closed");

    controller.restoreIntent("session-1", previous);
    expect(controller.intentIsCurrent("session-1", openEpoch, "open")).toBe(true);
    controller.restoreIntent("session-1", undefined);
    expect(controller.currentIntent("session-1")).toBeUndefined();
  });

  it("dedupClose shares one in-flight task, runs create synchronously, and allows retry", async () => {
    const controller = createBrowserController();
    const gate = deferred();
    let createRuns = 0;
    const create = () => {
      createRuns += 1;
      // The synchronous prologue must run before dedupClose returns.
      controller.issueIntent("session-1", "closed");
      return gate.promise;
    };
    const first = controller.dedupClose("session-1", create);
    expect(createRuns).toBe(1);
    expect(controller.closeInFlight("session-1")).toBe(true);
    const second = controller.dedupClose("session-1", create);
    expect(second).toBe(first);
    expect(createRuns).toBe(1);
    gate.resolve();
    await first;
    expect(controller.closeInFlight("session-1")).toBe(false);

    const retry = controller.dedupClose("session-1", create);
    expect(createRuns).toBe(2);
    await retry;
  });

  it("dedupClose clears the slot and propagates rejection", async () => {
    const controller = createBrowserController();
    const failing = controller.dedupClose("session-1", () => Promise.reject(new Error("close failed")));
    await expect(failing).rejects.toThrow("close failed");
    expect(controller.closeInFlight("session-1")).toBe(false);
  });

  it("pendingOpens snapshots in-flight opens and settles regardless of outcome", async () => {
    const controller = createBrowserController();
    const opening = deferred<BrowserStatus>();
    const failing = deferred<BrowserStatus>();
    controller.trackOpen("session-1", opening.promise);
    controller.trackOpen("session-1", failing.promise);
    const snapshot = controller.pendingOpens("session-1");
    expect(snapshot).toHaveLength(2);

    opening.resolve(status());
    failing.reject(new Error("open failed"));
    await Promise.allSettled(snapshot);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(controller.pendingOpens("session-1")).toHaveLength(0);
  });
});
