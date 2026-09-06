import { createHash, randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { createReadStream, lstatSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { windowsNativeBuildEnvironment } from "./windows-native-build-tools.mjs";
import { devChildEnvironment } from "./dev-child-environment.mjs";
import {
  isControlledBackendFinalShutdown,
  isControlledBackendRestart,
  stopBrowserDevChild
} from "./browser-dev-lifecycle.mjs";
import {
  MEMORY_E2E_HOST_ENVIRONMENT_NAMES,
  validateMemoryE2eWorkspaceFixtureEnvironment
} from "./memory-e2e-workspace-fixture.mjs";
import {
  browserDevClientLabel,
  browserDevLeaseExists,
  browserDevSourceFingerprint,
  coordinateBrowserDev,
  parseBrowserDevArguments,
  requestBrowserDevStop
} from "./browser-dev-coordinator.mjs";
import { chooseDevServerPorts, parsePort } from "./dev-server-port.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { client, stopOnly } = parseBrowserDevArguments(process.argv.slice(2));
const clientLabel = browserDevClientLabel(client);

if (stopOnly) {
  const result = await requestBrowserDevStop({ workspaceRoot: root });
  process.stdout.write(
    result.alreadyStopped
      ? "[browser-dev] 工作区没有运行中的共享实例\n"
      : `[browser-dev] 共享 owner PID ${result.ownerPid} 已受控停止\n`
  );
  process.exit(0);
}
const backendRestartDelayMs = 150;
const backendRestartWindowMs = 30_000;
const maxBackendRestartsPerWindow = 2;
const instanceIdEnvironmentName = "MEWORK_BROWSER_DEV_INSTANCE_ID";
const suppliedTokenEnvironmentName = "MEWORK_BROWSER_DEV_SUPPLIED_TOKEN";
const backendRestartRequestMarker = "[browser-dev] E2E controlled backend restart requested";
const backendRestartCommitMarker = "[browser-dev] E2E controlled backend restart committed";
const webview2ReleaseBarrierMarker = "[browser-dev] E2E WebView2 release barrier passed";
const backendFinalShutdownRequestMarker =
  "[browser-dev] E2E controlled final shutdown requested";
const backendFinalShutdownCommitMarker =
  "[browser-dev] E2E controlled final shutdown committed";
// An explicit port comes from an E2E harness that has already arranged its own
// isolation, so it is honoured verbatim. Otherwise the framework defaults are a
// preference: if something already holds one, take an operating-system assigned
// port rather than refusing to start.
//
// Moving aside is only allowed when this workspace has no browser-dev lease at
// all. With a lease present the shared ports belong to a Mework instance, and
// the coordinator below has to see this process contend for exactly those ports
// to elect a follower, recover a stale lease, or refuse to start beside an
// orphaned one. Quietly starting elsewhere would put a second owner on the same
// stable data directory, which nothing else would stop.
const { frontend: frontendChoice, backend: backendChoice } = await chooseDevServerPorts({
  explicitFrontend: parsePort(
    process.env.MEWORK_BROWSER_DEV_FRONTEND_PORT,
    "MEWORK_BROWSER_DEV_FRONTEND_PORT"
  ),
  explicitBackend: parsePort(
    process.env.MEWORK_BROWSER_DEV_BACKEND_PORT,
    "MEWORK_BROWSER_DEV_BACKEND_PORT"
  ),
  allowFallback: !browserDevLeaseExists(root)
});
const frontendPort = frontendChoice.port;
const backendPort = backendChoice.port;
const origin = `http://127.0.0.1:${frontendPort}`;
const backendUrl = `ws://127.0.0.1:${backendPort}/ws`;
const backendOrigin = `http://127.0.0.1:${backendPort}`;
const browserDevCoordination = await coordinateBrowserDev({
  workspaceRoot: root,
  origin,
  backendOrigin
});
if (browserDevCoordination.role === "follower") {
  process.stdout.write(
    `[browser-dev] 复用工作区共享实例（owner PID ${browserDevCoordination.ownerPid}）\n`
      + (
        clientLabel
          ? `[browser-dev] 请用 ${clientLabel} 内置浏览器打开 ${browserDevCoordination.origin}\n`
          : `[browser-dev] 打开 ${browserDevCoordination.origin}\n`
      )
  );
  process.exit(0);
}
// An owner that had to move off a default port is the one case where the URL to
// open is not the one every instruction names, so say so before anything else
// scrolls past. The editors' launch configurations point at the default port and
// cannot follow, so the address has to be opened by hand.
for (const [role, choice] of [["前端", frontendChoice], ["后端", backendChoice]]) {
  if (choice.fellBack) {
    process.stdout.write(
      `[browser-dev] ${role}默认端口 ${choice.preferred} 不可用，已改用系统分配的 ${choice.port}\n`
    );
  }
}
if (frontendChoice.fellBack) {
  process.stdout.write(
    `[browser-dev] ${clientLabel ?? "调试客户端"}的固定端口配置跟不过来，请手动打开 http://127.0.0.1:${frontendChoice.port}\n`
  );
}
const suppliedDataIdentifier = process.env.MEWORK_BROWSER_DEV_DATA_IDENTIFIER?.trim();
// An interactive debugging session keeps one stable app data directory so
// conversations, providers and API keys survive a restart. Only the E2E
// harnesses, which always supply their own identifier, get a throwaway
// directory that is deleted on shutdown. The backend requires the
// `com.mework.app.e2e.` prefix on every browser-dev identifier, so the stable
// name keeps it and is distinguished by a fixed suffix instead.
const INTERACTIVE_DATA_IDENTIFIER = "com.mework.app.e2e.interactive-dev";
const ephemeralDataIdentifier = process.env.MEWORK_BROWSER_DEV_EPHEMERAL_DATA === "1"
  ? `com.mework.app.e2e.${randomBytes(12).toString("hex")}`
  : null;
const dataIdentifier = suppliedDataIdentifier
  || ephemeralDataIdentifier
  || INTERACTIVE_DATA_IDENTIFIER;
const suppliedToken = process.env[suppliedTokenEnvironmentName]?.trim();
const imageInputRunId = process.env.MEWORK_IMAGE_INPUT_E2E_RUN_ID?.trim();
if (
  suppliedToken
  && (
    !suppliedDataIdentifier?.startsWith("com.mework.app.e2e.image-input-")
    || !/^[0-9a-f]{64}$/.test(suppliedToken)
    || !/^[0-9a-f]{24}$/.test(imageInputRunId ?? "")
  )
) {
  throw new Error(
    `${suppliedTokenEnvironmentName} 只接受隔离图片 E2E 数据标识、run ID 与 32 字节随机小写十六进制令牌`
  );
}
const token = suppliedToken || randomBytes(32).toString("hex");
const memoryWorkspaceFixture = validateMemoryE2eWorkspaceFixtureEnvironment({
  environment: process.env,
  dataIdentifier,
  bridgeToken: token
});
// Cleanup ownership is deliberately never granted to the stable interactive
// directory: deleting it is what silently destroyed saved conversations.
const ownsDataIdentifier = Boolean(ephemeralDataIdentifier)
  || (!suppliedDataIdentifier && dataIdentifier !== INTERACTIVE_DATA_IDENTIFIER)
  || Boolean(memoryWorkspaceFixture);
const imageInputControlledRestart = Boolean(
  suppliedToken
  && suppliedDataIdentifier?.startsWith("com.mework.app.e2e.image-input-")
  && /^[0-9a-f]{24}$/.test(imageInputRunId ?? "")
);
const environment = {
  ...devChildEnvironment({ buildEnvironment: windowsNativeBuildEnvironment() }),
  MEWORK_BROWSER_DEV_ADDRESS: `127.0.0.1:${backendPort}`,
  MEWORK_BROWSER_DEV_ORIGIN: origin,
  MEWORK_BROWSER_DEV_TOKEN: token,
  MEWORK_BROWSER_DEV_DATA_IDENTIFIER: dataIdentifier,
  ...(suppliedToken ? { MEWORK_BROWSER_DEV_ORIGINLESS_E2E_CLEANUP: "1" } : {}),
  VITE_BROWSER_DEV_BACKEND_URL: backendUrl,
  VITE_BROWSER_DEV_TOKEN: token
};
// The fixed memory workspace is host-to-Rust authority. Keep its run identity, marker, and path
// out of the Vite child entirely rather than relying only on Vite's VITE_* exposure convention.
for (const name of MEMORY_E2E_HOST_ENVIRONMENT_NAMES) delete environment[name];
const rustEnvironment = {
  ...environment,
  ...(memoryWorkspaceFixture?.rustEnvironment ?? {})
};
// The instance id is a host-to-Rust restart handshake, not renderer configuration. Remove any
// inherited value here and inject a fresh value only into each Rust child below.
delete environment[instanceIdEnvironmentName];
delete environment[suppliedTokenEnvironmentName];
delete rustEnvironment[instanceIdEnvironmentName];
delete rustEnvironment[suppliedTokenEnvironmentName];

const suppliedPrebuiltBackend = process.env.MEWORK_BROWSER_DEV_PREBUILT_BINARY?.trim();
const suppliedPrebuiltSha256 = process.env.MEWORK_BROWSER_DEV_PREBUILT_SHA256?.trim().toLowerCase();
let prebuiltBackend;
if (suppliedPrebuiltBackend || suppliedPrebuiltSha256) {
  if (
    !imageInputControlledRestart
    || !suppliedPrebuiltBackend
    || !/^[0-9a-f]{64}$/.test(suppliedPrebuiltSha256 ?? "")
  ) {
    throw new Error(
      "预构建 browser-dev 二进制只接受带隔离图片 E2E run ID、token、绝对路径和 SHA-256 的调用"
    );
  }
  const targetDir = path.resolve(
    environment.CARGO_TARGET_DIR
      || path.join(root, "src-tauri", "target")
  );
  const expectedPath = path.join(
    targetDir,
    "debug",
    process.platform === "win32" ? "mework-browser-dev.exe" : "mework-browser-dev"
  );
  const resolvedPath = path.resolve(suppliedPrebuiltBackend);
  if (resolvedPath !== expectedPath) {
    throw new Error("预构建 browser-dev 二进制不在本次 CARGO_TARGET_DIR/debug 的精确路径");
  }
  const metadata = lstatSync(resolvedPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error("预构建 browser-dev 路径不是普通文件");
  }
  const hash = createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = createReadStream(resolvedPath);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.once("error", reject);
    stream.once("end", resolve);
  });
  const actualSha256 = hash.digest("hex");
  if (actualSha256 !== suppliedPrebuiltSha256) {
    throw new Error("预构建 browser-dev 二进制 SHA-256 与调用证明不一致");
  }
  prebuiltBackend = {
    path: resolvedPath,
    size: metadata.size,
    mtimeMs: metadata.mtimeMs
  };
  process.stdout.write(`[browser-dev] verified prebuilt backend ${actualSha256}\n`);
}

let rustProcess;
let viteProcess;
let readyRustFingerprint = "";
let shuttingDown = false;
let backendRestartTimes = [];
let stopRequestTimer;
// Keep the coordinator alive across transient Cargo failures: another Agent may be halfway through
// a multi-file Rust edit, and dissolving the lease would recreate the same thundering herd.
let sourceReloading = false;
let coordinatedRebuildActive = false;
let shutdownPromise;

function forwardOutput(stream, destination, onText) {
  stream.setEncoding("utf8");
  stream.on("data", (text) => {
    destination.write(text);
    onText?.(text);
  });
}

function cleanupOwnedDataDirectories() {
  if (!ownsDataIdentifier) return;
  const prefix = "com.mework.app.e2e.";
  if (!dataIdentifier.startsWith(prefix) || dataIdentifier.length === prefix.length) {
    console.error("拒绝清理无法验证的浏览器开发数据目录");
    return;
  }

  for (const [label, directory] of [
    ["APPDATA", process.env.APPDATA],
    ["LOCALAPPDATA", process.env.LOCALAPPDATA]
  ]) {
    if (!directory) continue;
    const parent = path.resolve(directory);
    const target = path.resolve(parent, dataIdentifier);
    if (path.dirname(target) !== parent || path.basename(target) !== dataIdentifier) {
      console.error(`拒绝清理越出 ${label} 的浏览器开发数据目录`);
      continue;
    }
    try {
      rmSync(target, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
    } catch (error) {
      console.error(`无法清理 ${label} 中隔离的浏览器开发数据目录: ${error.message}`);
    }
  }
}

function shutdown(exitCode = 0) {
  if (shutdownPromise) return shutdownPromise;
  shuttingDown = true;
  if (stopRequestTimer) clearInterval(stopRequestTimer);
  browserDevCoordination.markStopping();

  shutdownPromise = (async () => {
    for (;;) {
      const activeChildren = [viteProcess, rustProcess].filter(
        (child) =>
          child
          && child.exitCode === null
          && child.signalCode === null
      );
      if (activeChildren.length === 0) break;
      const outcomes = await Promise.allSettled(
        activeChildren.map((child) => stopBrowserDevChild(child))
      );
      const failures = outcomes.filter(({ status }) => status === "rejected");
      if (failures.length > 0) {
        for (const failure of failures) {
          console.error(
            `[browser-dev] 子进程尚未安全停止，保留 owner 租约后重试：${
              failure.reason instanceof Error
                ? failure.reason.message
                : String(failure.reason)
            }`
          );
        }
        await new Promise((resolve) => setTimeout(resolve, 1_000));
      }
    }
    cleanupOwnedDataDirectories();
    await browserDevCoordination.release();
    process.exit(exitCode);
  })();
  return shutdownPromise;
}

function startVite() {
  const viteEntry = path.join(root, "node_modules", "vite", "bin", "vite.js");
  viteProcess = spawn(process.execPath, [viteEntry, "--config", "vite.config.ts", "--host", "127.0.0.1", "--port", String(frontendPort), "--strictPort"], {
    cwd: root,
    env: environment,
    stdio: ["inherit", "pipe", "pipe"]
  });
  let viteReady = false;
  let viteStdoutTail = "";
  forwardOutput(viteProcess.stdout, process.stdout, (text) => {
    if (viteReady) return;
    const combined = `${viteStdoutTail}${text}`;
    if (combined.includes("ready in") || combined.includes("Local")) {
      viteReady = true;
      browserDevCoordination.markReady(readyRustFingerprint);
      process.stdout.write(
        `[browser-dev] 共享实例 ready；并发 Agent 可直接复用 ${origin}\n`
      );
      if (clientLabel) {
        process.stdout.write(`[browser-dev] 请用 ${clientLabel} 内置浏览器打开 ${origin}\n`);
      }
      process.stdout.write(
        ownsDataIdentifier
          ? `[browser-dev] 一次性数据目录 ${dataIdentifier}，退出时会被删除\n`
          : `[browser-dev] 持久数据目录 ${dataIdentifier}，对话与 API 配置会在重启后保留\n`
      );
    }
    viteStdoutTail = combined.slice(-512);
  });
  forwardOutput(viteProcess.stderr, process.stderr);
  viteProcess.on("exit", (code, signal) => {
    if (!shuttingDown) {
      console.error(`Vite 已停止（${signal ?? code ?? "未知原因"}）`);
      shutdown(code || 1);
    }
  });
}

const cargo = process.platform === "win32" ? "cargo.exe" : "cargo";

function admitControlledBackendRestart() {
  const now = Date.now();
  backendRestartTimes = backendRestartTimes.filter(
    (timestamp) => now - timestamp < backendRestartWindowMs
  );
  if (backendRestartTimes.length >= maxBackendRestartsPerWindow) return false;
  backendRestartTimes.push(now);
  return true;
}

function startRustBackend() {
  if (shuttingDown || rustProcess) return;

  // A new opaque id on every spawn lets the authenticated E2E client prove it is talking to the
  // replacement process. It is deliberately scoped to the Rust child and never exposed by Vite.
  const instanceId = randomBytes(32).toString("hex");
  let command = cargo;
  let args = [
    "run",
    "--manifest-path",
    path.join(root, "src-tauri", "Cargo.toml"),
    "--no-default-features",
    "--features",
    "browser-dev",
    "--bin",
    "mework-browser-dev"
  ];
  if (prebuiltBackend) {
    let metadata;
    try {
      metadata = lstatSync(prebuiltBackend.path);
    } catch (error) {
      console.error(`无法复验预构建 browser-dev 二进制：${error.message}`);
      shutdown(1);
      return;
    }
    if (
      !metadata.isFile()
      || metadata.isSymbolicLink()
      || metadata.size !== prebuiltBackend.size
      || metadata.mtimeMs !== prebuiltBackend.mtimeMs
    ) {
      console.error("预构建 browser-dev 二进制在受控运行期间发生变化，已拒绝启动");
      shutdown(1);
      return;
    }
    command = prebuiltBackend.path;
    args = [];
  }
  const buildSourceFingerprint = browserDevSourceFingerprint(root);
  browserDevCoordination.markRebuilding(buildSourceFingerprint);
  const child = spawn(command, args, {
    cwd: root,
    env: {
      ...rustEnvironment,
      [instanceIdEnvironmentName]: instanceId
    },
    stdio: ["inherit", "pipe", "pipe"]
  });
  rustProcess = child;

  let rustReady = false;
  let controlledRestartRequestMarkerSeen = false;
  let controlledRestartCommitMarkerSeen = false;
  let webview2ReleaseBarrierMarkerSeen = false;
  let controlledFinalShutdownRequestMarkerSeen = false;
  let controlledFinalShutdownCommitMarkerSeen = false;
  let rustStdoutTail = "";
  forwardOutput(child.stdout, process.stdout, (text) => {
    if (rustProcess !== child) return;
    const combined = `${rustStdoutTail}${text}`;
    if (!rustReady && combined.includes("[browser-dev] Rust backend ready")) {
      rustReady = true;
      readyRustFingerprint = buildSourceFingerprint;
      if (!viteProcess) {
        startVite();
      } else {
        coordinatedRebuildActive = false;
        browserDevCoordination.markReady(buildSourceFingerprint);
      }
    }
    if (combined.includes(backendRestartRequestMarker)) {
      controlledRestartRequestMarkerSeen = true;
    }
    if (combined.includes(backendRestartCommitMarker)) {
      controlledRestartCommitMarkerSeen = true;
    }
    if (combined.includes(webview2ReleaseBarrierMarker)) {
      webview2ReleaseBarrierMarkerSeen = true;
    }
    if (combined.includes(backendFinalShutdownRequestMarker)) {
      controlledFinalShutdownRequestMarkerSeen = true;
    }
    if (combined.includes(backendFinalShutdownCommitMarker)) {
      controlledFinalShutdownCommitMarkerSeen = true;
    }
    rustStdoutTail = combined.slice(-Math.max(
      backendRestartRequestMarker.length,
      backendRestartCommitMarker.length,
      webview2ReleaseBarrierMarker.length,
      backendFinalShutdownRequestMarker.length,
      backendFinalShutdownCommitMarker.length,
      "[browser-dev] Rust backend ready".length
    ));
  });
  forwardOutput(child.stderr, process.stderr);
  child.on("error", (error) => {
    if (rustProcess !== child || shuttingDown) return;
    console.error(`无法启动 Rust 开发后端: ${error.message}`);
    shutdown(1);
  });
  // Decide only after stdio closes so a marker emitted immediately before graceful exit cannot be
  // lost to Node's earlier `exit` event.
  child.on("close", (code, signal) => {
    if (rustProcess !== child) return;
    rustProcess = undefined;
    if (shuttingDown) return;
    if (sourceReloading) {
      sourceReloading = false;
      process.stderr.write(
        "[browser-dev] 旧 Rust 后端已释放，正在按共享源码指纹协调重建\n"
      );
      setTimeout(() => {
        if (!shuttingDown) startRustBackend();
      }, backendRestartDelayMs).unref();
      return;
    }
    if (coordinatedRebuildActive) {
      coordinatedRebuildActive = false;
      browserDevCoordination.markRebuildFailed();
      process.stderr.write(
        `[browser-dev] 协调重建暂时失败（${signal ?? code ?? "未知原因"}）；保留唯一 owner 与 Vite，等待下一份稳定源码指纹\n`
      );
      return;
    }

    const controlledFinalShutdown = isControlledBackendFinalShutdown({
      code,
      signal,
      requestMarkerSeen: controlledFinalShutdownRequestMarkerSeen,
      releaseBarrierMarkerSeen: webview2ReleaseBarrierMarkerSeen,
      commitMarkerSeen: controlledFinalShutdownCommitMarkerSeen,
      imageInputE2EEnabled: imageInputControlledRestart
    });
    if (controlledFinalShutdown) {
      console.error(
        "Rust 开发后端已完成 WebView2 释放屏障与 E2E final shutdown"
      );
      shutdown(0);
      return;
    }

    // `App::run`-based browser-dev binaries can complete the authenticated Tauri shutdown with
    // code 0, while `run_return` propagates 75 and some Cargo/platform paths wrap it as 101. The
    // accepted-request, WebView2 release-barrier, and post-cleanup commit markers are therefore
    // mandatory and scoped to this exact child; exit code alone can never turn an ordinary crash
    // (or an aborted graceful-exit attempt) into a restart loop.
    const controlledRestart = isControlledBackendRestart({
      code,
      signal,
      requestMarkerSeen: controlledRestartRequestMarkerSeen,
      releaseBarrierMarkerSeen: webview2ReleaseBarrierMarkerSeen,
      commitMarkerSeen: controlledRestartCommitMarkerSeen,
      webSearchE2EEnabled: process.env.MEWORK_WEB_SEARCH_E2E?.trim() === "1",
      imageInputE2EEnabled: imageInputControlledRestart
    });
    if (controlledRestart) {
      if (!admitControlledBackendRestart()) {
        console.error("Rust 开发后端连续请求重启过多，已停止以避免重启风暴");
        shutdown(1);
        return;
      }
      console.error("Rust 开发后端已完成 E2E graceful shutdown，正在保留 Vite 会话并重启");
      setTimeout(() => {
        if (!shuttingDown) startRustBackend();
      }, backendRestartDelayMs).unref();
      return;
    }

    console.error(`Rust 开发后端已停止（${signal ?? code ?? "未知原因"}）`);
    shutdown(code || 1);
  });
}

startRustBackend();

if (browserDevCoordination.shared) {
  stopRequestTimer = setInterval(() => {
    if (browserDevCoordination.stopRequested()) {
      shutdown(0);
      return;
    }
    if (sourceReloading) return;
    const requestedFingerprint = browserDevCoordination.takeRestartRequest();
    if (!requestedFingerprint) return;
    coordinatedRebuildActive = true;
    browserDevCoordination.markRebuilding(requestedFingerprint);
    process.stderr.write(
      `[browser-dev] 接受共享重建请求 ${requestedFingerprint.slice(0, 12)}；Vite 与隔离会话保持运行\n`
    );
    if (rustProcess) {
      const child = rustProcess;
      sourceReloading = true;
      stopBrowserDevChild(child).catch((error) => {
        if (shuttingDown || rustProcess !== child) return;
        sourceReloading = false;
        coordinatedRebuildActive = false;
        browserDevCoordination.markRebuildFailed();
        process.stderr.write(
          `[browser-dev] 无法安全停止旧 Rust 后端，已保留 owner 与现有进程：${
            error instanceof Error ? error.message : String(error)
          }\n`
        );
      });
    } else {
      startRustBackend();
    }
  }, 250);
}

process.on("SIGINT", () => shutdown(130));
process.on("SIGTERM", () => shutdown(143));
