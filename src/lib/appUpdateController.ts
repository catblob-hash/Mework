import type {
  AppReleaseAsset,
  AppUpdateCheck,
  AppUpdateDownload,
  AppUpdateDownloadEvent,
  AppUpdateInstallOutcome
} from "../types";
import {
  cancelAppUpdateDownload,
  checkAppUpdate,
  downloadAppUpdate,
  installAppUpdate
} from "./runtime";

/**
 * Renderer-side state of the update pipeline: check → download → install.
 *
 * This lives outside the Updates page because the host allows one download at a time and
 * keeps running it whether or not the page is mounted. A page that owned the state would lose
 * the progress bar the moment the user looked at another settings page, and a remount would
 * try to start a second download the host refuses. The controller is the single place that
 * talks to the host commands; the page only renders `current()` and calls the verbs.
 */

export type AppUpdateState =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "check_failed"; message: string }
  | { phase: "checked"; check: AppUpdateCheck }
  | {
    phase: "downloading";
    check: AppUpdateCheck;
    receivedBytes: number;
    totalBytes: number;
    /** The bytes are on disk and the host is comparing the digest with `SHA256SUMS`. */
    verifying: boolean;
  }
  | { phase: "download_failed"; check: AppUpdateCheck; message: string }
  | { phase: "downloaded"; check: AppUpdateCheck; download: AppUpdateDownload }
  | { phase: "installing"; check: AppUpdateCheck; download: AppUpdateDownload }
  | {
    phase: "install_failed";
    check: AppUpdateCheck;
    download: AppUpdateDownload;
    message: string;
  };

/** The host's cancellation message; a cancelled download is not a failure to show. */
export const DOWNLOAD_CANCELLED_MESSAGE = "下载已取消";

/** A check younger than this is reused when the page mounts; the refresh button always refetches. */
export const CHECK_REUSE_WINDOW_MS = 10 * 60 * 1000;

export interface AppUpdateBackend {
  checkAppUpdate: () => Promise<AppUpdateCheck>;
  downloadAppUpdate: (
    asset: AppReleaseAsset,
    checksumsAsset: AppReleaseAsset | null,
    onProgress: (event: AppUpdateDownloadEvent) => void
  ) => Promise<AppUpdateDownload>;
  cancelAppUpdateDownload: () => Promise<void>;
  installAppUpdate: (path: string) => Promise<AppUpdateInstallOutcome>;
}

export interface AppUpdateController {
  subscribe(listener: () => void): () => void;
  current(): AppUpdateState;
  /**
   * Fetch the latest release. With `force: false` a recent successful check is kept and no
   * request is made. A check never interrupts a download in progress or a finished download of
   * the same release; a check that finds a different release drops that stale file state.
   */
  check(options?: { force?: boolean }): Promise<void>;
  /** Start downloading the asset the current check selected. No-op unless `checked`. */
  download(): Promise<void>;
  /** Ask the host to stop the running download; the state settles back to `checked`. */
  cancelDownload(): Promise<void>;
  /**
   * Hand the downloaded file to the host. For the installer flavor the app exits, so the
   * `installing` phase is the last thing this window shows.
   */
  install(): Promise<void>;
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return String(error);
}

export function createAppUpdateController(
  backend: AppUpdateBackend,
  now: () => number = () => Date.now()
): AppUpdateController {
  let state: AppUpdateState = { phase: "idle" };
  let checkedAt = 0;
  let checkSequence = 0;
  let downloadSequence = 0;
  let inFlightCheck: Promise<void> | null = null;
  const listeners = new Set<() => void>();

  const set = (next: AppUpdateState) => {
    state = next;
    for (const listener of [...listeners]) listener();
  };

  const busy = () => state.phase === "downloading" || state.phase === "installing";

  const check = async (options?: { force?: boolean }) => {
    if (busy()) return;
    if (inFlightCheck) return inFlightCheck;
    const reusable = state.phase !== "idle"
      && state.phase !== "checking"
      && state.phase !== "check_failed"
      && now() - checkedAt < CHECK_REUSE_WINDOW_MS;
    if (!options?.force && reusable) return;
    const previous = state;
    const sequence = ++checkSequence;
    // A finished download stays visible while the check runs; only its outcome decides.
    if (previous.phase !== "downloaded" && previous.phase !== "install_failed") {
      set({ phase: "checking" });
    }
    inFlightCheck = (async () => {
      try {
        const check = await backend.checkAppUpdate();
        // The user may have pressed Install while the check was out; a result must never
        // take the page out of `installing` (or a download that started meanwhile).
        if (sequence !== checkSequence || busy()) return;
        checkedAt = now();
        const keepDownload = (previous.phase === "downloaded" || previous.phase === "install_failed")
          && previous.check.release.tag === check.release.tag
          && previous.download.fileName === (check.asset?.name ?? "");
        if (keepDownload) {
          set({ phase: "downloaded", check, download: previous.download });
        } else {
          set({ phase: "checked", check });
        }
      } catch (error) {
        if (sequence !== checkSequence || busy()) return;
        // A file that is already downloaded stays installable when the network is not there
        // to re-check it; only a check that never had one to protect becomes a failure.
        if (previous.phase === "downloaded" || previous.phase === "install_failed") {
          set(previous);
        } else {
          set({ phase: "check_failed", message: errorMessage(error) });
        }
      } finally {
        inFlightCheck = null;
      }
    })();
    return inFlightCheck;
  };

  const download = async () => {
    if (state.phase !== "checked" && state.phase !== "download_failed") return;
    const { check } = state;
    if (!check.asset) return;
    const asset = check.asset;
    const sequence = ++downloadSequence;
    // Only the newest download may write state: a superseded one that settles late (the host
    // finished it after a cancel the renderer had already given up on) is dropped.
    const current = () => sequence === downloadSequence && state.phase === "downloading" && state.check === check;
    set({
      phase: "downloading",
      check,
      receivedBytes: 0,
      totalBytes: asset.size,
      verifying: false
    });
    try {
      const download = await backend.downloadAppUpdate(asset, check.checksumsAsset, (event) => {
        const live = state;
        if (!current() || live.phase !== "downloading") return;
        if (event.type === "progress") {
          set({
            ...live,
            receivedBytes: event.receivedBytes,
            totalBytes: event.totalBytes || asset.size
          });
        } else {
          set({ ...live, verifying: true });
        }
      });
      if (!current()) return;
      set({ phase: "downloaded", check, download });
    } catch (error) {
      if (!current()) return;
      const message = errorMessage(error);
      if (message.includes(DOWNLOAD_CANCELLED_MESSAGE)) {
        set({ phase: "checked", check });
      } else {
        set({ phase: "download_failed", check, message });
      }
    }
  };

  const cancelDownload = async () => {
    if (state.phase !== "downloading") return;
    try {
      await backend.cancelAppUpdateDownload();
    } catch {
      // The host never received the request, so the download is still running and the state
      // must keep saying so; the user can press cancel again.
    }
  };

  const install = async () => {
    if (state.phase !== "downloaded" && state.phase !== "install_failed") return;
    const { check, download } = state;
    set({ phase: "installing", check, download });
    try {
      const outcome = await backend.installAppUpdate(download.path);
      // A revealed archive is still there to reveal again; a launched installer is taking the
      // app down and nothing should change on screen.
      if (outcome.action === "revealed") set({ phase: "downloaded", check, download });
    } catch (error) {
      set({ phase: "install_failed", check, download, message: errorMessage(error) });
    }
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return state;
    },
    check,
    download,
    cancelDownload,
    install
  };
}

/** The one controller the Updates page renders; tests build their own with a fake backend. */
export const appUpdateController = createAppUpdateController({
  checkAppUpdate,
  downloadAppUpdate,
  cancelAppUpdateDownload,
  installAppUpdate
});
