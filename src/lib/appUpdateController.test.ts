import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  CHECK_REUSE_WINDOW_MS,
  DOWNLOAD_CANCELLED_MESSAGE,
  createAppUpdateController,
  errorMessage
} from "./appUpdateController";
import type {
  AppReleaseAsset,
  AppUpdateCheck,
  AppUpdateDownload,
  AppUpdateDownloadEvent,
  AppUpdateInstallOutcome
} from "../types";
import type { AppUpdateBackend, AppUpdateController } from "./appUpdateController";

const asset: AppReleaseAsset = {
  name: "Mework_1.1.0_x64-setup.exe",
  downloadUrl: "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
  size: 1000
};

const checksumsAsset: AppReleaseAsset = {
  name: "SHA256SUMS",
  downloadUrl: "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/SHA256SUMS",
  size: 200
};

const updateCheck: AppUpdateCheck = {
  currentVersion: "1.0.0",
  latestVersion: "1.1.0",
  updateAvailable: true,
  release: {
    tag: "v1.1.0",
    name: "Mework 1.1.0",
    htmlUrl: "https://github.com/catblob-hash/Mework/releases/tag/v1.1.0",
    notes: "New features",
    publishedAt: "2026-09-05T00:00:00Z"
  },
  asset,
  checksumsAsset,
  checkedAt: "2026-09-05T00:00:00Z"
};

const noUpdateCheck: AppUpdateCheck = {
  ...updateCheck,
  latestVersion: "1.0.0",
  updateAvailable: false,
  asset: null,
  checksumsAsset: null
};

const downloadResult: AppUpdateDownload = {
  path: "C:\\Users\\me\\Downloads\\Mework_1.1.0_x64-setup.exe",
  fileName: asset.name,
  sizeBytes: asset.size,
  sha256: "a".repeat(64),
  verification: "verified",
  flavor: "installer"
};

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function createBackend() {
  return {
    checkAppUpdate: vi.fn<AppUpdateBackend["checkAppUpdate"]>(),
    downloadAppUpdate: vi.fn<AppUpdateBackend["downloadAppUpdate"]>(),
    cancelAppUpdateDownload: vi.fn<AppUpdateBackend["cancelAppUpdateDownload"]>(),
    installAppUpdate: vi.fn<AppUpdateBackend["installAppUpdate"]>()
  };
}

async function reachDownloaded(
  controller: AppUpdateController,
  backend: ReturnType<typeof createBackend>
) {
  backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
  await controller.check();
  backend.downloadAppUpdate.mockResolvedValueOnce(downloadResult);
  await controller.download();
}

function currentDownloaded(controller: AppUpdateController) {
  const state = controller.current();
  expect(state.phase).toBe("downloaded");
  if (state.phase !== "downloaded") throw new Error("Expected downloaded state");
  return state;
}

describe("createAppUpdateController", () => {
  let now: number;

  beforeEach(() => {
    now = 0;
  });

  it("checks, reuses a recent result, force-refreshes, and expires the reuse window", async () => {
    const backend = createBackend();
    backend.checkAppUpdate.mockResolvedValue(updateCheck);
    const controller = createAppUpdateController(backend, () => now);
    const notifications: string[] = [];
    controller.subscribe(() => notifications.push(controller.current().phase));

    const first = controller.check();
    expect(controller.current().phase).toBe("checking");
    expect(notifications).toEqual(["checking"]);
    await first;
    expect(controller.current()).toEqual({ phase: "checked", check: updateCheck });
    expect(notifications).toEqual(["checking", "checked"]);

    now = CHECK_REUSE_WINDOW_MS - 1;
    await controller.check();
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(1);

    await controller.check({ force: true });
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(2);

    now += CHECK_REUSE_WINDOW_MS + 1;
    await controller.check();
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(3);
  });

  it("records a check failure and can recover with a forced check", async () => {
    const backend = createBackend();
    backend.checkAppUpdate
      .mockRejectedValueOnce(new Error("GitHub unavailable"))
      .mockResolvedValueOnce(updateCheck);
    const controller = createAppUpdateController(backend, () => now);

    await controller.check();
    expect(controller.current()).toEqual({ phase: "check_failed", message: "GitHub unavailable" });

    await controller.check({ force: true });
    expect(controller.current()).toEqual({ phase: "checked", check: updateCheck });
  });

  it("shares concurrent checks while the backend request is pending", async () => {
    const backend = createBackend();
    const pendingCheck = deferred<AppUpdateCheck>();
    backend.checkAppUpdate.mockReturnValue(pendingCheck.promise);
    const controller = createAppUpdateController(backend, () => now);

    const first = controller.check();
    const second = controller.check();
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(1);
    expect(controller.current().phase).toBe("checking");

    pendingCheck.resolve(updateCheck);
    await Promise.all([first, second]);
    expect(controller.current()).toEqual({ phase: "checked", check: updateCheck });
  });

  it("downloads an available asset and exposes progress and verification state", async () => {
    const backend = createBackend();
    backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
    const pendingDownload = deferred<AppUpdateDownload>();
    let onProgress: ((event: AppUpdateDownloadEvent) => void) | undefined;
    backend.downloadAppUpdate.mockImplementation((_asset, _checksums, progress) => {
      onProgress = progress;
      return pendingDownload.promise;
    });
    const controller = createAppUpdateController(backend, () => now);
    await controller.check();

    const download = controller.download();
    expect(controller.current()).toEqual({
      phase: "downloading",
      check: updateCheck,
      receivedBytes: 0,
      totalBytes: asset.size,
      verifying: false
    });
    expect(backend.downloadAppUpdate).toHaveBeenCalledWith(asset, checksumsAsset, expect.any(Function));

    onProgress?.({ type: "progress", receivedBytes: 400, totalBytes: 0 });
    expect(controller.current()).toMatchObject({
      phase: "downloading",
      receivedBytes: 400,
      totalBytes: asset.size,
      verifying: false
    });
    onProgress?.({ type: "verifying" });
    expect(controller.current()).toMatchObject({ phase: "downloading", verifying: true });

    pendingDownload.resolve(downloadResult);
    await download;
    expect(controller.current()).toEqual({ phase: "downloaded", check: updateCheck, download: downloadResult });
  });

  it("does not download outside a selected update with an asset", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);

    await controller.download();
    expect(backend.downloadAppUpdate).not.toHaveBeenCalled();

    backend.checkAppUpdate.mockResolvedValueOnce(noUpdateCheck);
    await controller.check();
    await controller.download();
    expect(controller.current()).toEqual({ phase: "checked", check: noUpdateCheck });
    expect(backend.downloadAppUpdate).not.toHaveBeenCalled();
  });

  it("returns cancelled downloads to checked, records other failures, and retries", async () => {
    const backend = createBackend();
    backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
    backend.downloadAppUpdate
      .mockRejectedValueOnce(new Error(`host: ${DOWNLOAD_CANCELLED_MESSAGE}`))
      .mockRejectedValueOnce(new Error("connection lost"))
      .mockResolvedValueOnce(downloadResult);
    const controller = createAppUpdateController(backend, () => now);
    await controller.check();

    await controller.download();
    expect(controller.current()).toEqual({ phase: "checked", check: updateCheck });

    await controller.download();
    expect(controller.current()).toEqual({
      phase: "download_failed",
      check: updateCheck,
      message: "connection lost"
    });

    await controller.download();
    expect(controller.current()).toEqual({ phase: "downloaded", check: updateCheck, download: downloadResult });
    expect(backend.downloadAppUpdate).toHaveBeenCalledTimes(3);
  });

  it("only cancels a running download and keeps downloading when the cancel command fails", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);

    await controller.cancelDownload();
    expect(backend.cancelAppUpdateDownload).not.toHaveBeenCalled();

    backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
    await controller.check();
    const pendingDownload = deferred<AppUpdateDownload>();
    backend.downloadAppUpdate.mockReturnValueOnce(pendingDownload.promise);
    void controller.download();
    backend.cancelAppUpdateDownload.mockRejectedValueOnce(new Error("host would not stop"));

    await controller.cancelDownload();
    expect(backend.cancelAppUpdateDownload).toHaveBeenCalledTimes(1);
    // The host never got the request, so the transfer is still running and the state says so.
    expect(controller.current()).toMatchObject({ phase: "downloading", check: updateCheck });

    // A second press reaches the host; the host answers by rejecting the download.
    backend.cancelAppUpdateDownload.mockResolvedValueOnce(undefined);
    await controller.cancelDownload();
    expect(backend.cancelAppUpdateDownload).toHaveBeenCalledTimes(2);
    pendingDownload.reject(new Error(DOWNLOAD_CANCELLED_MESSAGE));
    await vi.waitFor(() => expect(controller.current()).toEqual({ phase: "checked", check: updateCheck }));
  });

  it("drops the late outcome of a superseded download and never leaves installing for a check result", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);
    backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
    await controller.check();

    // First download: the renderer gives up on it (host rejects as cancelled) but the host
    // actually completes it later. Meanwhile a second download runs.
    const first = deferred<AppUpdateDownload>();
    backend.downloadAppUpdate.mockReturnValueOnce(first.promise);
    void controller.download();
    const staleState = controller.current();
    expect(staleState.phase).toBe("downloading");
    // Simulate the renderer treating the first as cancelled by moving on: reject it as cancelled.
    first.reject(new Error(DOWNLOAD_CANCELLED_MESSAGE));
    await vi.waitFor(() => expect(controller.current().phase).toBe("checked"));
    const second = deferred<AppUpdateDownload>();
    backend.downloadAppUpdate.mockReturnValueOnce(second.promise);
    void controller.download();
    expect(controller.current().phase).toBe("downloading");
    // A progress callback captured by the first download must not touch the second.
    const firstProgress = backend.downloadAppUpdate.mock.calls[0][2];
    firstProgress({ type: "progress", receivedBytes: 999, totalBytes: 1000 });
    expect(controller.current()).toMatchObject({ phase: "downloading", receivedBytes: 0 });
    second.resolve(downloadResult);
    await vi.waitFor(() => expect(controller.current().phase).toBe("downloaded"));

    // A forced check that is still out when Install is pressed must not clobber `installing`.
    const pendingCheck = deferred<AppUpdateCheck>();
    backend.checkAppUpdate.mockReturnValueOnce(pendingCheck.promise);
    const refresh = controller.check({ force: true });
    const pendingInstall = deferred<AppUpdateInstallOutcome>();
    backend.installAppUpdate.mockReturnValueOnce(pendingInstall.promise);
    void controller.install();
    expect(controller.current().phase).toBe("installing");
    pendingCheck.resolve({ ...updateCheck, checkedAt: "2026-09-06T00:00:00Z" });
    await refresh;
    expect(controller.current().phase).toBe("installing");
    pendingInstall.resolve({ action: "installer_launched" });
    await Promise.resolve();
    expect(controller.current().phase).toBe("installing");
  });

  it("keeps a portable download after reveal and keeps an installer launch in progress", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);
    await reachDownloaded(controller, backend);

    const reveal = deferred<AppUpdateInstallOutcome>();
    backend.installAppUpdate.mockReturnValueOnce(reveal.promise);
    const install = controller.install();
    expect(controller.current().phase).toBe("installing");
    expect(backend.installAppUpdate).toHaveBeenCalledWith(downloadResult.path);

    reveal.resolve({ action: "revealed" });
    await install;
    expect(currentDownloaded(controller)).toEqual({
      phase: "downloaded",
      check: updateCheck,
      download: downloadResult
    });

    backend.installAppUpdate.mockResolvedValueOnce({ action: "installer_launched" });
    await controller.install();
    expect(controller.current()).toEqual({ phase: "installing", check: updateCheck, download: downloadResult });
  });

  it("records install failures and retries from install_failed", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);
    await reachDownloaded(controller, backend);
    backend.installAppUpdate
      .mockRejectedValueOnce(new Error("cannot launch installer"))
      .mockResolvedValueOnce({ action: "revealed" });

    await controller.install();
    expect(controller.current()).toEqual({
      phase: "install_failed",
      check: updateCheck,
      download: downloadResult,
      message: "cannot launch installer"
    });

    await controller.install();
    expect(currentDownloaded(controller)).toEqual({
      phase: "downloaded",
      check: updateCheck,
      download: downloadResult
    });
  });

  it("does not check while downloading or installing", async () => {
    const backend = createBackend();
    backend.checkAppUpdate.mockResolvedValueOnce(updateCheck);
    const controller = createAppUpdateController(backend, () => now);
    await controller.check();

    const pendingDownload = deferred<AppUpdateDownload>();
    backend.downloadAppUpdate.mockReturnValueOnce(pendingDownload.promise);
    const download = controller.download();
    await controller.check();
    await controller.check({ force: true });
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(1);

    pendingDownload.resolve(downloadResult);
    await download;
    const pendingInstall = deferred<AppUpdateInstallOutcome>();
    backend.installAppUpdate.mockReturnValueOnce(pendingInstall.promise);
    const install = controller.install();
    await controller.check();
    await controller.check({ force: true });
    expect(backend.checkAppUpdate).toHaveBeenCalledTimes(1);

    pendingInstall.resolve({ action: "installer_launched" });
    await install;
  });

  it("keeps a matching downloaded file visible through a forced refresh and drops it for a new release", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);
    await reachDownloaded(controller, backend);
    const matchingCheck: AppUpdateCheck = { ...updateCheck, checkedAt: "2026-09-06T00:00:00Z" };
    backend.checkAppUpdate.mockResolvedValueOnce(matchingCheck);

    const matchingRefresh = controller.check({ force: true });
    expect(controller.current()).toEqual({ phase: "downloaded", check: updateCheck, download: downloadResult });
    await matchingRefresh;
    expect(currentDownloaded(controller)).toEqual({
      phase: "downloaded",
      check: matchingCheck,
      download: downloadResult
    });

    const newerCheck: AppUpdateCheck = {
      ...matchingCheck,
      latestVersion: "1.2.0",
      release: { ...matchingCheck.release, tag: "v1.2.0", name: "Mework 1.2.0" }
    };
    backend.checkAppUpdate.mockResolvedValueOnce(newerCheck);
    await controller.check({ force: true });
    expect(controller.current()).toEqual({ phase: "checked", check: newerCheck });
  });

  it("keeps a downloaded file installable when a forced re-check fails", async () => {
    const backend = createBackend();
    const controller = createAppUpdateController(backend, () => now);
    await reachDownloaded(controller, backend);
    backend.checkAppUpdate.mockRejectedValueOnce(new Error("offline"));

    await controller.check({ force: true });
    // The file on disk is still the latest known release; losing the install button to a
    // network blip would be worse than showing a slightly stale check.
    expect(controller.current()).toEqual({ phase: "downloaded", check: updateCheck, download: downloadResult });

    // Without a download to protect, the same failure is reported.
    const fresh = createAppUpdateController(backend, () => now);
    backend.checkAppUpdate.mockRejectedValueOnce(new Error("offline"));
    await fresh.check();
    expect(fresh.current()).toEqual({ phase: "check_failed", message: "offline" });
  });

  it("formats all supported error values", () => {
    expect(errorMessage(new Error("boom"))).toBe("boom");
    expect(errorMessage("plain failure")).toBe("plain failure");
    expect(errorMessage(42)).toBe("42");
  });
});
