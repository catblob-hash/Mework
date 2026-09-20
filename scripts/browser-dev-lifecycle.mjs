import { spawn } from "node:child_process";
import path from "node:path";

const CONTROLLED_BACKEND_EXIT_CODES = new Set([0, 75, 101]);

export function isControlledBackendRestart({
  code,
  signal,
  requestMarkerSeen,
  releaseBarrierMarkerSeen,
  commitMarkerSeen,
  webSearchE2EEnabled,
  imageInputE2EEnabled
}) {
  return signal === null
    && CONTROLLED_BACKEND_EXIT_CODES.has(code)
    && requestMarkerSeen === true
    && releaseBarrierMarkerSeen === true
    && commitMarkerSeen === true
    && (webSearchE2EEnabled === true || imageInputE2EEnabled === true);
}

export function isControlledBackendFinalShutdown({
  code,
  signal,
  requestMarkerSeen,
  releaseBarrierMarkerSeen,
  commitMarkerSeen,
  imageInputE2EEnabled
}) {
  return signal === null
    && code === 0
    && requestMarkerSeen === true
    && releaseBarrierMarkerSeen === true
    && commitMarkerSeen === true
    && imageInputE2EEnabled === true;
}

export function waitForBrowserDevChildClose(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.removeListener("close", onClose);
      callback(value);
    };
    const onClose = () => finish(resolve);
    const timer = setTimeout(
      () => finish(
        reject,
        new Error(`${path.basename(child.spawnfile ?? "child")} 停止超时`)
      ),
      timeoutMs
    );
    child.once("close", onClose);
    if (child.exitCode !== null || child.signalCode !== null) finish(resolve);
  });
}

function taskkillBrowserDevProcessTree(pid, timeoutMs) {
  return new Promise((resolve, reject) => {
    const taskkill = spawn("taskkill.exe", ["/PID", String(pid), "/T", "/F"], {
      windowsHide: true,
      stdio: "ignore"
    });
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      callback(value);
    };
    const timer = setTimeout(() => {
      taskkill.kill();
      finish(reject, new Error(`taskkill ${pid} 超时`));
    }, timeoutMs);
    taskkill.once("error", (error) => finish(reject, error));
    taskkill.once("close", (code) => finish(resolve, code));
  });
}

export async function stopBrowserDevChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === "win32") {
    if (!Number.isInteger(child.pid)) {
      await waitForBrowserDevChildClose(child, 2_000);
      return;
    }
    const taskkillCode = await taskkillBrowserDevProcessTree(child.pid, 10_000);
    try {
      await waitForBrowserDevChildClose(child, 10_000);
    } catch (error) {
      throw new Error(
        `无法停止 ${path.basename(child.spawnfile ?? "child")}（taskkill ${taskkillCode}）：${
          error instanceof Error ? error.message : String(error)
        }`
      );
    }
    return;
  }
  child.kill("SIGINT");
  try {
    await waitForBrowserDevChildClose(child, 10_000);
  } catch {
    child.kill("SIGKILL");
    await waitForBrowserDevChildClose(child, 5_000);
  }
}
