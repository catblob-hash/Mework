import { createHash, randomBytes } from "node:crypto";
import {
  lstatSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync
} from "node:fs";
import { get } from "node:http";
import { createServer } from "node:net";
import path from "node:path";

import {
  delay,
  fingerprintWorkspaceInputs,
  isProcessAlive,
  readWorkspaceLease,
  recoverEmptyWorkspaceLease,
  removeWorkspaceLease,
  resolveLeaseDirectory,
  tryAcquireWorkspaceLease,
  updateWorkspaceLease,
  workspaceLeaseDirectoryExists
} from "./workspace-coordinator.mjs";

export const BROWSER_DEV_COORDINATOR_PROTOCOL =
  "mework-browser-dev-coordinator-v2";
export const BROWSER_DEV_MAX_REBUILD_FAILURES = 2;

// Each debugging client gets its own explicit flag so the entry point can name the exact built-in
// browser to open, and so an unmarked call still means "automation/E2E".
const browserDevClientFlags = new Map([
  ["--codex", { client: "codex", label: "Codex" }],
  ["--claude", { client: "claude", label: "Claude Code" }]
]);

export function browserDevClientLabel(client) {
  for (const descriptor of browserDevClientFlags.values()) {
    if (descriptor.client === client) return descriptor.label;
  }
  return null;
}

export function parseBrowserDevArguments(args) {
  const supported = new Set([...browserDevClientFlags.keys(), "--stop"]);
  const unknown = args.filter((argument) => !supported.has(argument));
  if (unknown.length > 0) {
    throw new Error(`不支持的 browser-dev 参数：${unknown.join("、")}`);
  }
  if (new Set(args).size !== args.length) {
    throw new Error("browser-dev 参数不能重复");
  }

  const clientFlags = args.filter((argument) => browserDevClientFlags.has(argument));
  if (clientFlags.length > 1) {
    throw new Error(`${clientFlags.join(" 与 ")} 不能同时使用`);
  }
  const stopOnly = args.includes("--stop");
  if (clientFlags.length > 0 && stopOnly) {
    throw new Error(`${clientFlags[0]} 与 --stop 不能同时使用`);
  }
  const client = clientFlags.length > 0
    ? browserDevClientFlags.get(clientFlags[0]).client
    : null;
  return { client, stopOnly };
}

const exclusiveEnvironmentNames = [
  "CARGO_TARGET_DIR",
  "CI",
  "MEWORK_BROWSER_DEV_BACKEND_PORT",
  "MEWORK_BROWSER_DEV_DATA_IDENTIFIER",
  "MEWORK_BROWSER_DEV_FRONTEND_PORT",
  "MEWORK_BROWSER_DEV_PREBUILT_BINARY",
  "MEWORK_BROWSER_DEV_PREBUILT_SHA256",
  "MEWORK_BROWSER_DEV_SUPPLIED_TOKEN",
  "MEWORK_IMAGE_INPUT_E2E_RUN_ID",
  "MEWORK_MEMORY_E2E_RUN_ID",
  "MEWORK_WEB_SEARCH_E2E",
  "MEWORK_WEB_SEARCH_E2E_RUN_ID"
];

const browserDevOwnerFingerprintEntries = [
  "index.html",
  "package-lock.json",
  "package.json",
  "scripts/browser-dev-coordinator.mjs",
  "scripts/browser-dev-lifecycle.mjs",
  "scripts/browser-dev.mjs",
  "scripts/dev-server-port.mjs",
  "scripts/memory-e2e-workspace-fixture.mjs",
  "scripts/vite-csp.mjs",
  "scripts/windows-native-build-tools.mjs",
  "scripts/workspace-coordinator.mjs",
  "tsconfig.json",
  "tsconfig.node.json",
  "vite.config.ts"
];

const browserDevRustFingerprintEntries = [
  ".cargo/config.toml",
  "src/mework-icon.svg",
  "src-tauri/Cargo.lock",
  "src-tauri/Cargo.toml",
  "src-tauri/app_commands.rs",
  "src-tauri/build.rs",
  "src-tauri/capabilities",
  "src-tauri/permissions",
  "src-tauri/resources",
  "src-tauri/src",
  "src-tauri/tauri.conf.json",
  "src-tauri/tauri.windows.conf.json",
  "src-tauri/windows-app-manifest.xml"
];

export function browserDevCanShare(environment = process.env) {
  return !exclusiveEnvironmentNames.some((name) => {
    const value = environment[name];
    return typeof value === "string" && value.trim() !== "";
  });
}

export function browserDevSourceFingerprint(workspaceRoot) {
  return fingerprintWorkspaceInputs(workspaceRoot, browserDevRustFingerprintEntries, {
    salt: `${BROWSER_DEV_COORDINATOR_PROTOCOL}\0rust\0${process.platform}\0${process.arch}`
  });
}

export function browserDevOwnerFingerprint(workspaceRoot) {
  // This fingerprint is sealed only when the Node/Vite owner is acquired. It must
  // never be refreshed by a Rust-only rebuild, otherwise old coordinator code could
  // falsely advertise that newly edited owner code is already running.
  return fingerprintWorkspaceInputs(workspaceRoot, browserDevOwnerFingerprintEntries, {
    salt: `${BROWSER_DEV_COORDINATOR_PROTOCOL}\0owner\0${process.platform}\0${process.arch}`
  });
}

/**
 * Whether this workspace already has a browser-dev lease of any status.
 *
 * A lease — healthy, starting, or stale with a dead owner — means the shared
 * ports belong to a Mework instance, and the election, stale recovery and
 * orphan-detection paths below all reason about exactly those ports. A caller
 * that quietly started somewhere else instead of contending for them would slip
 * past every one of those checks and elect a second owner onto the same, stable
 * data directory, which has no exclusive lock of its own to stop it.
 */
export function browserDevLeaseExists(workspaceRoot) {
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  return (
    workspaceLeaseDirectoryExists({ workspaceRoot, directory })
    || Boolean(readWorkspaceLease({ workspaceRoot, directory }))
  );
}

export function classifyBrowserDevLease({
  state,
  ownerFingerprint,
  sourceFingerprint,
  ownerAlive
}) {
  if (!state) return "wait-for-state";
  if (
    state.protocol !== BROWSER_DEV_COORDINATOR_PROTOCOL
    || typeof state.sessionId !== "string"
    || !Number.isSafeInteger(state.ownerPid)
  ) {
    return "incompatible";
  }
  if (!ownerAlive) return "stale";
  if (state.status === "stopping") return "wait-for-stop";
  if (state.ownerFingerprint !== ownerFingerprint) return "owner-mismatch";
  if (state.sourceFingerprint !== sourceFingerprint) return "source-mismatch";
  if (state.status === "rebuild-failed") return "rebuild-failed";
  if (state.status !== "ready") return "wait-for-ready";
  return "probe";
}

export function browserDevRestartRequestKey({
  classification,
  state,
  sourceFingerprint
}) {
  if (classification === "source-mismatch") {
    return `mismatch\0${state.sessionId}\0${sourceFingerprint}`;
  }
  if (classification === "rebuild-failed") {
    const failureCount = Number.isSafeInteger(state.rebuildFailureCount)
      ? state.rebuildFailureCount
      : BROWSER_DEV_MAX_REBUILD_FAILURES;
    if (failureCount >= BROWSER_DEV_MAX_REBUILD_FAILURES) return null;
    return ["rebuild-failed", state.sessionId, sourceFingerprint, failureCount].join("\0");
  }
  return null;
}

export function acceptedBrowserDevRestartFingerprint({
  state,
  currentSourceFingerprint,
  request
}) {
  if (!state) return null;
  if (state.sourceFingerprint !== currentSourceFingerprint) {
    return currentSourceFingerprint;
  }
  if (state.status !== "rebuild-failed") {
    return null;
  }
  const failureCount = Number.isSafeInteger(state.rebuildFailureCount)
    ? state.rebuildFailureCount
    : BROWSER_DEV_MAX_REBUILD_FAILURES;
  if (
    failureCount >= BROWSER_DEV_MAX_REBUILD_FAILURES
    || request?.observedStatus !== "rebuild-failed"
    || request.observedSourceFingerprint !== currentSourceFingerprint
    || request.observedRebuildFailureCount !== failureCount
  ) {
    return null;
  }
  return currentSourceFingerprint;
}

function requestWithTimeout(url, timeoutMs) {
  return new Promise((resolve) => {
    const request = get(url, {
      headers: { "cache-control": "no-store" }
    }, (response) => {
      response.setEncoding("utf8");
      let body = "";
      response.on("data", (chunk) => {
        if (body.length < 64 * 1024) body += chunk;
      });
      response.once("end", () => {
        resolve({
          ok: response.statusCode >= 200 && response.statusCode < 300,
          body
        });
      });
    });
    request.setTimeout(timeoutMs, () => request.destroy());
    request.once("error", () => resolve(null));
  });
}

function drainClosedServerHandle() {
  return new Promise((resolve) => {
    setImmediate(() => setImmediate(resolve));
  });
}

function loopbackPortAvailable(url) {
  const parsed = new URL(url);
  const port = Number(parsed.port);
  if (
    parsed.protocol !== "http:"
    || parsed.hostname !== "127.0.0.1"
    || !Number.isSafeInteger(port)
    || port < 1
    || port > 65_535
  ) {
    throw new Error(`browser-dev 协调端点不是有效 IPv4 loopback URL：${url}`);
  }
  const server = createServer((socket) => socket.destroy());
  return new Promise((resolve, reject) => {
    const onError = (error) => {
      server.close();
      drainClosedServerHandle().then(() => {
        if (error?.code === "EADDRINUSE" || error?.code === "EACCES") {
          resolve(false);
        } else {
          reject(error);
        }
      }, reject);
    };
    server.once("error", onError);
    server.listen({
      host: parsed.hostname,
      port,
      exclusive: true
    }, () => {
      server.removeListener("error", onError);
      server.close((error) => {
        drainClosedServerHandle().then(() => {
          if (error) reject(error);
          else resolve(true);
        }, reject);
      });
    });
  });
}

async function inspectEndpoints({ origin, backendOrigin }) {
  // On this Windows toolchain, Node 24.15 intermittently fast-fails in the
  // short-lived TCP client path. Separately, nodejs/node#56645 documents the
  // same exit code during platform shutdown. Ready is published only after
  // Rust and Vite listen, so verify both listeners through server-only bind
  // exclusion and keep Node out of the unstable client path.
  if (process.platform === "win32") {
    const availability = await Promise.all([
      loopbackPortAvailable(backendOrigin),
      loopbackPortAvailable(origin)
    ]);
    return {
      occupied: availability.some((available) => !available),
      reusable: availability.every((available) => !available)
    };
  }
  const [backend, frontend] = await Promise.all([
    requestWithTimeout(`${backendOrigin}/health`, 900),
    requestWithTimeout(`${origin}/`, 900)
  ]);
  let reusable = false;
  if (!backend?.ok || !frontend?.ok) {
    return { occupied: false, reusable: false };
  }
  try {
    const health = JSON.parse(backend.body);
    reusable = (
      health?.status === "ok"
      && health?.runtime === "rust"
      && frontend.body.includes('id="root"')
    );
  } catch {
    reusable = false;
  }
  return { occupied: reusable, reusable };
}

function stateForOwner({
  sessionId,
  ownerFingerprint,
  sourceFingerprint,
  origin,
  backendOrigin
}) {
  const now = new Date().toISOString();
  return {
    protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
    sessionId,
    ownerPid: process.pid,
    ownerFingerprint,
    sourceFingerprint,
    rebuildFailureCount: 0,
    origin,
    backendOrigin,
    status: "starting",
    startedAt: now,
    updatedAt: now
  };
}

function controlRequestPath(directory, name, sessionId) {
  const token = createHash("sha256").update(sessionId, "utf8").digest("hex");
  return path.join(directory, `${name}-request-${token}.json`);
}

function stopRequestPath(directory, sessionId) {
  return controlRequestPath(directory, "stop", sessionId);
}

function restartRequestPath(directory, sessionId) {
  return controlRequestPath(directory, "restart", sessionId);
}

function writeControlRequest(directory, name, request) {
  const target = controlRequestPath(directory, name, request.sessionId);
  const temporary = path.join(
    directory,
    `.${name}-${process.pid}-${randomBytes(8).toString("hex")}.tmp`
  );
  let temporaryWritten = false;
  try {
    writeFileSync(temporary, `${JSON.stringify(request, null, 2)}\n`, {
      encoding: "utf8",
      mode: 0o600
    });
    temporaryWritten = true;
    for (let attempt = 0; attempt < 4; attempt += 1) {
      try {
        renameSync(temporary, target);
        return true;
      } catch (error) {
        if (!["EACCES", "EEXIST", "EPERM"].includes(error?.code)) throw error;
        try {
          const targetMetadata = lstatSync(target);
          if (!targetMetadata.isFile() || targetMetadata.isSymbolicLink()) {
            throw new Error("browser-dev 控制请求目标不是普通文件");
          }
          // Requests are wake-up edges. Once one complete request has won publication, another
          // contender for the same session can safely coalesce into it; the owner re-fingerprints
          // the workspace when consuming a restart edge.
          return false;
        } catch (targetError) {
          if (targetError?.code === "ENOENT") continue;
          throw targetError;
        }
      }
    }
    throw new Error(`无法发布 browser-dev ${name} 控制请求`);
  } catch (error) {
    // The observed generation may have completed between classification and
    // publication. Its control edge is obsolete; a new loop iteration will read
    // the replacement lease. Session-specific target names also make a late write
    // into a replacement directory harmless to that new owner.
    if (error?.code === "ENOENT") return false;
    throw error;
  } finally {
    if (temporaryWritten) {
      try {
        unlinkSync(temporary);
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
    }
  }
}

function readStopRequest(directory, sessionId) {
  try {
    return JSON.parse(
      readFileSync(stopRequestPath(directory, sessionId), "utf8")
    );
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

function readRestartRequest(directory, sessionId) {
  try {
    return JSON.parse(
      readFileSync(restartRequestPath(directory, sessionId), "utf8")
    );
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

export async function requestBrowserDevStop({
  workspaceRoot,
  timeoutMs = 15_000
}) {
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const state = readWorkspaceLease({ workspaceRoot, directory });
  if (!state) return { stopped: true, alreadyStopped: true };
  if (
    state.protocol !== BROWSER_DEV_COORDINATOR_PROTOCOL
    || typeof state.sessionId !== "string"
    || !isProcessAlive(state.ownerPid)
  ) {
    throw new Error("browser-dev 没有可安全通知的活跃共享 owner");
  }
  const request = {
    protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
    sessionId: state.sessionId,
    requesterPid: process.pid,
    requestedAt: new Date().toISOString()
  };
  writeControlRequest(directory, "stop", request);

  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const current = readWorkspaceLease({ workspaceRoot, directory });
    if (!current || !isProcessAlive(state.ownerPid)) {
      return {
        stopped: true,
        alreadyStopped: false,
        ownerPid: state.ownerPid
      };
    }
    await delay(150);
  }
  throw new Error(`等待共享 browser-dev owner PID ${state.ownerPid} 停止超时`);
}

async function coordinateBrowserDevInternal({
  workspaceRoot,
  origin,
  backendOrigin,
  environment = process.env,
  startupTimeoutMs = 20 * 60 * 1000,
  emptyLeaseRecoveryMs = 5_000,
  sourceFingerprintRefreshMs = 2_000,
  onWait = (message) => process.stdout.write(`${message}\n`)
}) {
  if (!browserDevCanShare(environment)) {
    return {
      role: "owner",
      shared: false,
      markReady() {},
      markStopping() {},
      markRebuilding() {},
      markRebuildFailed() {},
      takeRestartRequest() { return null; },
      stopRequested() { return false; },
      release() {}
    };
  }

  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  let ownerFingerprint = "";
  let ownerFingerprintReadAt = 0;
  let sourceFingerprint = "";
  let sourceFingerprintReadAt = 0;
  const refreshOwnerFingerprint = (force = false) => {
    const now = Date.now();
    if (
      force
      || !ownerFingerprint
      || now - ownerFingerprintReadAt >= sourceFingerprintRefreshMs
    ) {
      ownerFingerprint = browserDevOwnerFingerprint(workspaceRoot);
      ownerFingerprintReadAt = now;
    }
    return ownerFingerprint;
  };
  const refreshSourceFingerprint = (force = false) => {
    const now = Date.now();
    if (
      force
      || !sourceFingerprint
      || now - sourceFingerprintReadAt >= sourceFingerprintRefreshMs
    ) {
      sourceFingerprint = browserDevSourceFingerprint(workspaceRoot);
      sourceFingerprintReadAt = now;
    }
    return sourceFingerprint;
  };
  const sessionId = randomBytes(16).toString("hex");
  const ownerState = stateForOwner({
    sessionId,
    ownerFingerprint,
    sourceFingerprint,
    origin,
    backendOrigin
  });
  const deadline = Date.now() + startupTimeoutMs;
  let lastProgressAt = 0;
  let missingStateSince = 0;
  let requestedRestartFor = "";
  let requestedOwnerHandoffFor = "";
  let shouldTryAcquire = true;
  let lastObservedStateGeneration = "";

  for (;;) {
    if (shouldTryAcquire) {
      // Hash exactly before a possible mkdir win. Followers do not repeat this expensive scan on
      // every 350 ms poll; after observing a lease they use the bounded refresh below.
      refreshOwnerFingerprint(true);
      refreshSourceFingerprint(true);
      ownerState.ownerFingerprint = ownerFingerprint;
      ownerState.sourceFingerprint = sourceFingerprint;
      ownerState.rebuildFailureCount = 0;
      ownerState.updatedAt = new Date().toISOString();
      if (await tryAcquireWorkspaceLease({
        workspaceRoot,
        directory,
        state: ownerState
      })) {
        const releaseOwnedLease = async () => {
          const releaseDeadline = Date.now() + 15_000;
          for (;;) {
            if (await removeWorkspaceLease({
              workspaceRoot,
              directory,
              sessionId
            })) {
              return;
            }
            const current = readWorkspaceLease({ workspaceRoot, directory });
            if (current && current.sessionId !== sessionId) return;
            if (
              !current
              && !workspaceLeaseDirectoryExists({ workspaceRoot, directory })
            ) {
              return;
            }
            if (Date.now() >= releaseDeadline) {
              throw new Error(
                `等待 browser-dev owner session ${sessionId} 释放租约超时`
              );
            }
            await delay(50);
          }
        };
        const existingEndpoints = await inspectEndpoints({
          origin,
          backendOrigin
        });
        if (existingEndpoints.occupied) {
          await releaseOwnedLease();
          throw new Error(
            `检测到未纳入协调的旧 browser-dev 已占用 ${origin} / ${backendOrigin}；请先结束旧实例，再重试 npm run dev:browser`
          );
        }
        const acquiredOwnerFingerprint = ownerFingerprint;
        let activeRebuildFingerprint = sourceFingerprint;
        let rebuildFailureCount = 0;
        return {
          role: "owner",
          shared: true,
          sessionId,
          ownerFingerprint: acquiredOwnerFingerprint,
          sourceFingerprint,
          directory,
          markReady(builtSourceFingerprint) {
            if (
              typeof builtSourceFingerprint !== "string"
              || builtSourceFingerprint.length === 0
            ) {
              throw new Error("browser-dev ready 缺少本次 Rust spawn 的源码指纹");
            }
            // Publish exactly the bytes captured when this Rust child was spawned.
            // If files changed during Cargo, followers observe a mismatch and request
            // the next build instead of treating unbuilt bytes as converged.
            activeRebuildFingerprint = builtSourceFingerprint;
            rebuildFailureCount = 0;
            updateWorkspaceLease({
              workspaceRoot,
              directory,
              sessionId,
              update: {
                status: "ready",
                sourceFingerprint: activeRebuildFingerprint,
                rebuildFailureCount
              }
            });
          },
          markStopping() {
            updateWorkspaceLease({
              workspaceRoot,
              directory,
              sessionId,
              update: { status: "stopping" }
            });
          },
          markRebuilding(requestedFingerprint) {
            if (requestedFingerprint !== activeRebuildFingerprint) {
              activeRebuildFingerprint = requestedFingerprint;
              rebuildFailureCount = 0;
            }
            updateWorkspaceLease({
              workspaceRoot,
              directory,
              sessionId,
              update: {
                status: "rebuilding",
                sourceFingerprint: activeRebuildFingerprint,
                rebuildFailureCount
              }
            });
          },
          markRebuildFailed() {
            rebuildFailureCount += 1;
            updateWorkspaceLease({
              workspaceRoot,
              directory,
              sessionId,
              update: {
                status: "rebuild-failed",
                sourceFingerprint: activeRebuildFingerprint,
                rebuildFailureCount
              }
            });
          },
          takeRestartRequest() {
            const request = readRestartRequest(directory, sessionId);
            if (
              request?.protocol !== BROWSER_DEV_COORDINATOR_PROTOCOL
              || request.sessionId !== sessionId
              || typeof request.sourceFingerprint !== "string"
            ) {
              return null;
            }
            try {
              unlinkSync(restartRequestPath(directory, sessionId));
            } catch (error) {
              if (error?.code !== "ENOENT") throw error;
            }
            if (
              browserDevOwnerFingerprint(workspaceRoot)
              !== acquiredOwnerFingerprint
            ) {
              // This process cannot load edited coordinator/Vite configuration. A follower will
              // publish a stop edge and elect a fresh owner instead of asking this one to rebuild.
              return null;
            }
            const currentSourceFingerprint = browserDevSourceFingerprint(workspaceRoot);
            const currentState = readWorkspaceLease({ workspaceRoot, directory });
            const acceptedFingerprint = acceptedBrowserDevRestartFingerprint({
              state: currentState?.sessionId === sessionId ? currentState : null,
              currentSourceFingerprint,
              request
            });
            if (!acceptedFingerprint) {
              // A late duplicate from another follower must not trigger a second rebuild after this
              // owner has already converged on the same workspace bytes.
              return null;
            }
            // The request is only a wake-up edge. The owner rebuilds the source that exists when it
            // accepts that edge, not a possibly stale fingerprint captured by one follower.
            return acceptedFingerprint;
          },
          stopRequested() {
            const request = readStopRequest(directory, sessionId);
            if (
              request?.protocol !== BROWSER_DEV_COORDINATOR_PROTOCOL
              || request.sessionId !== sessionId
            ) {
              return false;
            }
            try {
              unlinkSync(stopRequestPath(directory, sessionId));
            } catch (error) {
              if (error?.code !== "ENOENT") throw error;
            }
            return true;
          },
          async release() {
            await releaseOwnedLease();
          }
        };
      }
      shouldTryAcquire = false;
    }

    const state = readWorkspaceLease({ workspaceRoot, directory });
    if (
      !state
      && !workspaceLeaseDirectoryExists({ workspaceRoot, directory })
    ) {
      shouldTryAcquire = true;
      missingStateSince = 0;
    }
    const stateGeneration = state
      ? [
          state.sessionId,
          state.ownerFingerprint,
          state.sourceFingerprint,
          state.status,
          state.rebuildFailureCount,
          state.updatedAt
        ].join("\0")
      : "";
    if (
      state
      && stateGeneration !== lastObservedStateGeneration
      && (
        state.status === "rebuild-failed"
        || state.ownerFingerprint !== ownerFingerprint
        || state.sourceFingerprint !== sourceFingerprint
      )
    ) {
      // A newly published owner fingerprint/failure is a convergence edge, so do not wait for the
      // periodic refresh before comparing it with the workspace that exists now.
      refreshOwnerFingerprint(true);
      refreshSourceFingerprint(true);
    } else {
      refreshOwnerFingerprint(false);
      refreshSourceFingerprint(false);
    }
    lastObservedStateGeneration = stateGeneration;
    const classification = classifyBrowserDevLease({
      state,
      ownerFingerprint,
      sourceFingerprint,
      ownerAlive: isProcessAlive(state?.ownerPid)
    });

    const ownerHandoffKey = classification === "owner-mismatch"
      ? `${state.sessionId}\0${ownerFingerprint}`
      : null;
    if (
      ownerHandoffKey
      && requestedOwnerHandoffFor !== ownerHandoffKey
    ) {
      writeControlRequest(directory, "stop", {
        protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
        sessionId: state.sessionId,
        requesterPid: process.pid,
        reason: "owner-fingerprint-mismatch",
        requestedAt: new Date().toISOString()
      });
      requestedOwnerHandoffFor = ownerHandoffKey;
      onWait(
        `[browser-dev] owner 脚本或 Vite 配置已变化，已请求 PID ${state.ownerPid} 受控交接`
      );
    }

    const restartRequestKey = state
      ? browserDevRestartRequestKey({
          classification,
          state,
          sourceFingerprint
        })
      : null;
    if (restartRequestKey && requestedRestartFor !== restartRequestKey) {
      writeControlRequest(directory, "restart", {
        protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
        sessionId: state.sessionId,
        requesterPid: process.pid,
        sourceFingerprint,
        observedStatus: state.status,
        observedSourceFingerprint: state.sourceFingerprint,
        observedRebuildFailureCount: state.rebuildFailureCount,
        requestedAt: new Date().toISOString()
      });
      requestedRestartFor = restartRequestKey;
      onWait(
        `[browser-dev] 源码已变化，已请求共享 owner PID ${state.ownerPid} 协调重建`
      );
    }
    if (classification === "incompatible") {
      throw new Error(
        "已有 browser-dev 协调状态版本不兼容；请结束现有实例后重新运行 npm run dev:browser"
      );
    }
    if (classification === "stale") {
      const staleEndpoints = await inspectEndpoints({
        origin,
        backendOrigin
      });
      if (staleEndpoints.occupied) {
        throw new Error(
          "检测到 owner 已结束但端口仍由孤儿 browser-dev 占用；请结束该工作区的旧进程树后重试"
        );
      }
      if (await removeWorkspaceLease({
        workspaceRoot,
        directory,
        sessionId: state.sessionId
      })) {
        shouldTryAcquire = true;
        continue;
      }
    }
    const readyEndpoints = classification === "probe"
      ? await inspectEndpoints({
          origin: state.origin,
          backendOrigin: state.backendOrigin
        })
      : null;
    if (readyEndpoints?.reusable) {
      return {
        role: "follower",
        shared: true,
        sessionId: state.sessionId,
        ownerFingerprint,
        sourceFingerprint,
        origin: state.origin,
        backendOrigin: state.backendOrigin,
        ownerPid: state.ownerPid,
        markReady() {},
        markStopping() {},
        markRebuilding() {},
        markRebuildFailed() {},
        takeRestartRequest() { return null; },
        stopRequested() { return false; },
        release() {}
      };
    }

    if (classification === "wait-for-state") {
      missingStateSince ||= Date.now();
      if (Date.now() - missingStateSince >= emptyLeaseRecoveryMs) {
        if (await recoverEmptyWorkspaceLease({ workspaceRoot, directory })) {
          missingStateSince = 0;
          shouldTryAcquire = true;
          continue;
        }
        // A live owner may have published between our null read and recovery attempt.
        if (readWorkspaceLease({ workspaceRoot, directory })) {
          missingStateSince = 0;
          continue;
        }
        if (!workspaceLeaseDirectoryExists({ workspaceRoot, directory })) {
          missingStateSince = 0;
          shouldTryAcquire = true;
        }
      }
    } else {
      missingStateSince = 0;
    }

    if (Date.now() >= deadline) {
      throw new Error("等待共享 browser-dev 启动超时");
    }
    if (Date.now() - lastProgressAt >= 15_000) {
      lastProgressAt = Date.now();
      onWait(
        `[browser-dev] 另一个 Agent 正在启动共享实例（owner PID ${state?.ownerPid ?? "unknown"}），等待其 ready`
      );
    }
    await delay(Math.min(350, Math.max(1, deadline - Date.now())));
  }
}

export async function coordinateBrowserDev(options) {
  // A pending top-level await is not itself a ref'ed libuv handle. Keep the
  // coordinator alive across the brief interval between releasing its OS
  // mutex and scheduling the next probe/timer. This also avoids the Windows
  // Node 23+ shutdown race tracked by nodejs/node#56645 while released builds
  // still lack the upstream fix.
  const keepAlive = setInterval(() => {}, 1_000);
  try {
    return await coordinateBrowserDevInternal(options);
  } finally {
    clearInterval(keepAlive);
  }
}
