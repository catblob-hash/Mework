import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import path from "node:path";

import {
  removeWorkspaceLease,
  resolveLeaseDirectory,
  tryAcquireWorkspaceLease
} from "../workspace-coordinator.mjs";

const [
  mode,
  workspaceRootArgument,
  pauseMarkerArgument,
  pauseMillisecondsArgument,
  sessionId
] = process.argv.slice(2);

if (mode !== "acquire" && mode !== "remove") {
  throw new Error("mode 必须是 acquire 或 remove");
}
if (!workspaceRootArgument || !pauseMarkerArgument || !sessionId) {
  throw new Error(
    "用法: workspace-coordinator-pause-worker.mjs "
    + "<acquire|remove> <workspaceRoot> <pauseMarker> <pauseMs> <sessionId>"
  );
}

const pauseMilliseconds = Number(pauseMillisecondsArgument);
if (
  !Number.isSafeInteger(pauseMilliseconds)
  || pauseMilliseconds < 0
  || pauseMilliseconds > 2_147_483_647
) {
  throw new Error("pauseMs 必须是 0 到 2147483647 之间的整数");
}

const workspaceRoot = path.resolve(workspaceRootArgument);
const pauseMarker = path.resolve(pauseMarkerArgument);
const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
const normalizedDirectory = normalizePath(directory);
const originalMkdirSync = fs.mkdirSync;
const originalRenameSync = fs.renameSync;
const pauseArray = new Int32Array(new SharedArrayBuffer(4));
let paused = false;

function normalizePath(value) {
  const resolved = path.resolve(String(value));
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

function publishPauseMarker() {
  if (paused) return;
  paused = true;
  originalMkdirSync(path.dirname(pauseMarker), { recursive: true });
  fs.writeFileSync(
    pauseMarker,
    `${JSON.stringify({
      mode,
      pid: process.pid,
      sessionId
    })}\n`,
    "utf8"
  );
  Atomics.wait(pauseArray, 0, 0, pauseMilliseconds);
}

if (mode === "acquire") {
  fs.mkdirSync = function patchedMkdirSync(target, ...args) {
    const result = originalMkdirSync.call(this, target, ...args);
    if (!paused && normalizePath(target) === normalizedDirectory) {
      publishPauseMarker();
    }
    return result;
  };
} else {
  fs.renameSync = function patchedRenameSync(source, destination) {
    const result = originalRenameSync.call(this, source, destination);
    if (
      !paused
      && path.dirname(normalizePath(source)) === normalizedDirectory
      && path.dirname(normalizePath(destination)) === normalizedDirectory
      && path.basename(String(source)).startsWith(".owner-")
      && path.basename(String(destination)).startsWith(".removing-")
    ) {
      publishPauseMarker();
    }
    return result;
  };
}
syncBuiltinESMExports();

let result;
try {
  result = mode === "acquire"
    ? await tryAcquireWorkspaceLease({
      workspaceRoot,
      directory,
      state: {
        protocol: "workspace-coordinator-pause-fixture-v1",
        sessionId,
        ownerPid: process.pid,
        status: "running"
      }
    })
    : await removeWorkspaceLease({
      workspaceRoot,
      directory,
      sessionId
    });
} finally {
  fs.mkdirSync = originalMkdirSync;
  fs.renameSync = originalRenameSync;
  syncBuiltinESMExports();
}

process.stdout.write(`${JSON.stringify({
  mode,
  paused,
  pid: process.pid,
  result,
  sessionId
})}\n`);
