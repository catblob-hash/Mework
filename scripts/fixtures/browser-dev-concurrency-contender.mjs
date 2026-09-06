import { createServer } from "node:http";

import {
  coordinateBrowserDev,
  parseBrowserDevArguments
} from "../browser-dev-coordinator.mjs";

const workspaceRoot = process.env.MEWORK_BROWSER_DEV_FIXTURE_WORKSPACE?.trim();
const origin = process.env.MEWORK_BROWSER_DEV_FIXTURE_ORIGIN?.trim();
const backendOrigin =
  process.env.MEWORK_BROWSER_DEV_FIXTURE_BACKEND_ORIGIN?.trim();
const failRebuilds =
  process.env.MEWORK_BROWSER_DEV_FIXTURE_FAIL_REBUILDS?.trim() === "1";
const startupTimeoutMs = Number(
  process.env.MEWORK_BROWSER_DEV_FIXTURE_STARTUP_TIMEOUT_MS ?? "30000"
);
if (
  !Number.isSafeInteger(startupTimeoutMs)
  || startupTimeoutMs < 500
  || startupTimeoutMs > 60_000
) {
  throw new Error("browser-dev fixture startup timeout 无效");
}

function requireFixtureValue(value, name) {
  if (!value) throw new Error(`缺少 ${name}`);
  return value;
}

function listen(server, url) {
  const parsed = new URL(url);
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(Number(parsed.port), parsed.hostname, () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function close(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

function report(message) {
  return new Promise((resolve, reject) => {
    process.stdout.write(`${JSON.stringify(message)}\n`, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

async function finishProcess(exitCode) {
  process.exitCode = exitCode;
  await new Promise((resolve) => setImmediate(resolve));
}

const { client, stopOnly } = parseBrowserDevArguments(process.argv.slice(2));
if (!client || stopOnly) {
  throw new Error("browser-dev 并发 fixture 只接受 --codex 或 --claude");
}

let coordination;
let frontend;
let backend;
let stopping = false;
let controlTimer;
let controlBusy = false;
let rebuildOrdinal = 0;

async function shutdown(exitCode = 0) {
  if (stopping) return;
  stopping = true;
  if (controlTimer) clearInterval(controlTimer);
  const failures = [];
  for (const server of [frontend, backend]) {
    if (!server) continue;
    try {
      await close(server);
    } catch (error) {
      failures.push(error);
    }
  }
  await coordination?.release();
  if (failures.length > 0) {
    throw new AggregateError(failures, "关闭 browser-dev 并发 fixture 失败");
  }
  await finishProcess(exitCode);
}

try {
  coordination = await coordinateBrowserDev({
    workspaceRoot: requireFixtureValue(
      workspaceRoot,
      "MEWORK_BROWSER_DEV_FIXTURE_WORKSPACE"
    ),
    origin: requireFixtureValue(origin, "MEWORK_BROWSER_DEV_FIXTURE_ORIGIN"),
    backendOrigin: requireFixtureValue(
      backendOrigin,
      "MEWORK_BROWSER_DEV_FIXTURE_BACKEND_ORIGIN"
    ),
    // Keep the fixture deterministic even when the parent test process is
    // running under CI or has an E2E-only environment variable set.
    environment: {},
    startupTimeoutMs,
    onWait() {}
  });

  if (coordination.role === "follower") {
    await report({
      type: "result",
      role: "follower",
      client,
      processId: process.pid,
      ownerPid: coordination.ownerPid,
      sessionId: coordination.sessionId,
      origin: coordination.origin,
      backendOrigin: coordination.backendOrigin
    });
    await finishProcess(0);
  } else {
    backend = createServer((request, response) => {
    if (request.url !== "/health") {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, {
      "content-type": "application/json",
      "cache-control": "no-store"
    });
    response.end(JSON.stringify({ status: "ok", runtime: "rust" }));
  });
  frontend = createServer((_request, response) => {
    response.writeHead(200, {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store"
    });
    response.end('<!doctype html><div id="root"></div>');
  });

  await Promise.all([
    listen(backend, backendOrigin),
    listen(frontend, origin)
  ]);
  coordination.markReady(coordination.sourceFingerprint);
  await report({
    type: "result",
    role: "owner",
    client,
    processId: process.pid,
    ownerPid: process.pid,
    sessionId: coordination.sessionId,
    origin,
    backendOrigin
  });

  const pollControlRequests = async () => {
    if (stopping || controlBusy) return;
    controlBusy = true;
    try {
      if (coordination.stopRequested()) {
        await shutdown(0);
        return;
      }
      const requestedFingerprint = coordination.takeRestartRequest();
      if (!requestedFingerprint) return;

      coordination.markRebuilding(requestedFingerprint);
      rebuildOrdinal += 1;
      await report({
        type: "rebuild",
        ordinal: rebuildOrdinal,
        sourceFingerprint: requestedFingerprint
      });
      await new Promise((resolve) => setTimeout(resolve, 40));
      if (stopping) return;
      if (failRebuilds) coordination.markRebuildFailed();
      else coordination.markReady(requestedFingerprint);
    } finally {
      controlBusy = false;
    }
  };
    controlTimer = setInterval(() => {
      pollControlRequests().catch((error) => {
        report({ type: "error", message: error.stack ?? error.message })
          .finally(() => shutdown(1));
      });
    }, 25);
    process.once("SIGINT", () => {
      shutdown(130).catch(() => finishProcess(1));
    });
    process.once("SIGTERM", () => {
      shutdown(143).catch(() => finishProcess(1));
    });
  }
} catch (error) {
  try {
    await report({ type: "error", message: error.stack ?? error.message });
  } finally {
    await shutdown(1);
  }
}
