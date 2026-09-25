import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { gitConversationTarget } from "./git";
import {
  GIT_POLL_BACKGROUND_MS,
  GIT_POLL_FOREGROUND_MS,
  GIT_POLL_MAX_BACKOFF_MS,
  nextGitPollDelay,
  useGitSurfacePolling
} from "./gitPolling";
import type { GitPollSurface } from "./gitPolling";

const surface = (member: number, foreground: boolean): GitPollSurface => ({
  key: member > 1 ? `c1#${member}` : "c1",
  surfaceKey: member > 1 ? `p1#${member}` : "p1",
  target: gitConversationTarget("c1", member),
  foreground
});

describe("git polling cadence", () => {
  it("polls what is on screen fast, the rest slower, and backs a failing checkout off", () => {
    expect(nextGitPollDelay(true, 0)).toBe(GIT_POLL_FOREGROUND_MS);
    expect(nextGitPollDelay(false, 0)).toBe(GIT_POLL_BACKGROUND_MS);
    expect(nextGitPollDelay(true, 1)).toBe(GIT_POLL_FOREGROUND_MS * 2);
    expect(nextGitPollDelay(true, 2)).toBe(GIT_POLL_FOREGROUND_MS * 4);
    expect(nextGitPollDelay(true, 30)).toBe(GIT_POLL_MAX_BACKOFF_MS);
  });
});

describe("useGitSurfacePolling", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("keeps each checkout on its own loop, so a slow one holds nobody up", async () => {
    const calls: string[] = [];
    let releaseSlow: (() => void) | null = null;
    const refresh = vi.fn((polled: GitPollSurface) => {
      calls.push(polled.key);
      if (polled.key === "c1#2") {
        return new Promise<boolean>((resolve) => {
          releaseSlow = () => resolve(true);
        });
      }
      return Promise.resolve(true);
    });
    renderHook(() => useGitSurfacePolling([surface(1, true), surface(2, true)], refresh, true));
    await act(async () => {
      await Promise.resolve();
    });
    expect(calls).toEqual(["c1", "c1#2"]);
    // The slow checkout is still out; the fast one keeps its cadence.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(GIT_POLL_FOREGROUND_MS * 2);
    });
    expect(calls.filter((key) => key === "c1").length).toBe(3);
    expect(calls.filter((key) => key === "c1#2").length).toBe(1);
    await act(async () => {
      releaseSlow?.();
      await Promise.resolve();
    });
  });

  it("backs a failing checkout off and forgives it when the window regains focus", async () => {
    const refresh = vi.fn(() => Promise.resolve(false));
    renderHook(() => useGitSurfacePolling([surface(1, true)], refresh, true));
    await act(async () => {
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(1);
    // One failure doubles the wait: nothing at the plain cadence.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(GIT_POLL_FOREGROUND_MS);
    });
    expect(refresh).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(GIT_POLL_FOREGROUND_MS);
    });
    expect(refresh).toHaveBeenCalledTimes(2);
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      await Promise.resolve();
    });
    expect(refresh).toHaveBeenCalledTimes(3);
  });

  it("stops when disabled", async () => {
    const refresh = vi.fn(() => Promise.resolve(true));
    renderHook(() => useGitSurfacePolling([surface(1, true)], refresh, false));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(GIT_POLL_FOREGROUND_MS * 3);
    });
    expect(refresh).not.toHaveBeenCalled();
  });
});
