import { useEffect, useRef } from "react";
import { gitTargetKey } from "./git";
import type { GitTarget } from "./git";

/**
 * One checkout the renderer keeps a Git snapshot of: a project workspace of the open
 * conversation, as that conversation sees it (its worktree of the workspace when it has one).
 */
export interface GitPollSurface {
  /** The snapshot's cache key — a `gitSnapshotKey`. */
  key: string;
  /** The `gitSurfaceKey` the snapshot is stored under. */
  surfaceKey: string;
  target: GitTarget;
  /**
   * The checkout is on screen — the one the Git chip shows, or the review pane's open page —
   * and is polled at the fast cadence. The rest only have to keep their tab labels honest.
   */
  foreground: boolean;
}

/** Cadence for a checkout on screen. */
export const GIT_POLL_FOREGROUND_MS = 4_000;
/** Cadence for the conversation's other checkouts. */
export const GIT_POLL_BACKGROUND_MS = 15_000;
/** The longest a failing checkout waits between attempts. */
export const GIT_POLL_MAX_BACKOFF_MS = 60_000;

/**
 * How long to wait before polling a checkout again.
 *
 * A checkout that keeps failing — a machine that went to sleep, a repository that vanished —
 * backs off exponentially from its cadence, so an unreachable machine is asked about once a
 * minute rather than every few seconds while every other workspace carries on at full speed.
 */
export function nextGitPollDelay(foreground: boolean, failures: number): number {
  const cadence = foreground ? GIT_POLL_FOREGROUND_MS : GIT_POLL_BACKGROUND_MS;
  if (failures <= 0) return cadence;
  return Math.min(GIT_POLL_MAX_BACKOFF_MS, cadence * 2 ** Math.min(failures, 8));
}

/**
 * Keeps every surface's snapshot fresh, each on its own loop.
 *
 * Checkouts are independent: one on a slow or unreachable machine never holds up, cancels or
 * delays the others. Each has at most one read in flight — a tick that comes while one is out
 * asks once more when it lands rather than piling requests on a slow link. Returning to the
 * window, or the page becoming visible, reads every checkout at once and forgives their
 * failures, since that is exactly when a machine that was away may be back.
 *
 * `refresh` resolves `false` when the read failed. It is read through a ref, so a new callback
 * does not restart the loops.
 */
export function useGitSurfacePolling(
  surfaces: readonly GitPollSurface[],
  refresh: (surface: GitPollSurface) => Promise<boolean>,
  enabled: boolean
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const signature = surfaces
    .map((surface) => [surface.key, surface.surfaceKey, gitTargetKey(surface.target), surface.foreground].join("\u0000"))
    .join("\u0001");
  const surfacesRef = useRef(surfaces);
  surfacesRef.current = surfaces;

  // biome-ignore lint/correctness/useExhaustiveDependencies: `signature` stands for `surfaces`, which the caller rebuilds every render.
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    const loops = surfacesRef.current.map((surface) => {
      const state = {
        inFlight: false,
        queued: false,
        failures: 0,
        timer: null as number | null
      };
      const schedule = () => {
        if (cancelled) return;
        if (state.timer !== null) window.clearTimeout(state.timer);
        state.timer = window.setTimeout(() => void tick(), nextGitPollDelay(surface.foreground, state.failures));
      };
      const tick = async () => {
        if (cancelled) return;
        if (window.document.visibilityState === "hidden") {
          schedule();
          return;
        }
        if (state.inFlight) {
          state.queued = true;
          return;
        }
        state.inFlight = true;
        try {
          const ok = await refreshRef.current(surface);
          state.failures = ok ? 0 : state.failures + 1;
        } catch {
          state.failures += 1;
        } finally {
          state.inFlight = false;
        }
        if (cancelled) return;
        if (state.queued) {
          state.queued = false;
          void tick();
          return;
        }
        schedule();
      };
      return {
        start: () => void tick(),
        wake: () => {
          state.failures = 0;
          void tick();
        },
        stop: () => {
          if (state.timer !== null) window.clearTimeout(state.timer);
        }
      };
    });
    for (const loop of loops) loop.start();
    const wake = () => {
      if (window.document.visibilityState === "hidden") return;
      for (const loop of loops) loop.wake();
    };
    window.addEventListener("focus", wake);
    window.document.addEventListener("visibilitychange", wake);
    return () => {
      cancelled = true;
      for (const loop of loops) loop.stop();
      window.removeEventListener("focus", wake);
      window.document.removeEventListener("visibilitychange", wake);
    };
  }, [enabled, signature]);
}
