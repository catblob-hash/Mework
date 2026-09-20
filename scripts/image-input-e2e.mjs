import { randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, lstatSync, mkdtempSync, rmSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DATA_IDENTIFIER_PREFIX = "com.mework.app.e2e.";
const MODEL_API_KEY = "MEWORK_IMAGE_E2E_FAKE_MODEL_KEY_8f5c7d2a_not_real";
const MAX_REPORT_BYTES = 256 * 1024;
const MAX_CAPTURED_LOG_CHARS = 2 * 1024 * 1024;

function environmentPort(name, fallback) {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  if (!/^[0-9]{1,5}$/.test(raw)) throw new Error(`${name} 必须是有效端口`);
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    throw new Error(`${name} 必须在 1024–65535 范围内`);
  }
  return port;
}

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const frontendPort = environmentPort("MEWORK_BROWSER_DEV_FRONTEND_PORT", 1520);
const backendPort = environmentPort("MEWORK_BROWSER_DEV_BACKEND_PORT", 1530);
const protocolPort = environmentPort("MEWORK_PROTOCOL_E2E_PORT", 18100);
const reporterPort = environmentPort("MEWORK_IMAGE_INPUT_E2E_REPORT_PORT", 18101);
const ports = [frontendPort, backendPort, protocolPort, reporterPort];
if (new Set(ports).size !== ports.length) throw new Error("图片输入 E2E 的四个端口必须互不相同");

const frontendOrigin = `http://127.0.0.1:${frontendPort}`;
const protocolBaseUrl = `http://127.0.0.1:${protocolPort}/v1`;
const reportUrl = `http://127.0.0.1:${reporterPort}/result`;
const runId = randomBytes(12).toString("hex");
const reportToken = randomBytes(32).toString("hex");
const bridgeToken = randomBytes(32).toString("hex");
const dataIdentifier = `${DATA_IDENTIFIER_PREFIX}image-input-${randomBytes(12).toString("hex")}`;
const cargoTargetDir = path.resolve(
  process.env.MEWORK_IMAGE_INPUT_E2E_CARGO_TARGET_DIR?.trim()
    || path.join(root, "src-tauri", "target-image-input-e2e")
);
const environment = {
  ...process.env,
  CARGO_TARGET_DIR: cargoTargetDir,
  MEWORK_BROWSER_DEV_FRONTEND_PORT: String(frontendPort),
  MEWORK_BROWSER_DEV_BACKEND_PORT: String(backendPort),
  MEWORK_PROTOCOL_E2E_PORT: String(protocolPort),
  MEWORK_BROWSER_DEV_DATA_IDENTIFIER: dataIdentifier,
  MEWORK_BROWSER_DEV_SUPPLIED_TOKEN: bridgeToken,
  MEWORK_IMAGE_INPUT_E2E_RUN_ID: runId,
  MEWORK_IMAGE_E2E_SELF_CHECK: "0",
  VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL: protocolBaseUrl,
  VITE_IMAGE_INPUT_E2E_RUN_ID: runId,
  VITE_IMAGE_INPUT_E2E_REPORT_URL: reportUrl,
  VITE_IMAGE_INPUT_E2E_REPORT_TOKEN: reportToken
};

let browserDevProcess;
let protocolProcess;
let visibleBrowserProcess;
let browserProfileDirectory;
let shuttingDown = false;
let shutdownPromise;
let reportServerListening = false;
let pageStarted = false;
let reportValue;
let imageInputKeysVerifiedClean = false;
let gracefulBrowserDevShutdownComplete = false;
let capturedLogs = "";
let resolveReport;
let rejectReport;
const reportPromise = new Promise((resolve, reject) => {
  resolveReport = resolve;
  rejectReport = reject;
});
let rejectChildFailure;
const childFailurePromise = new Promise((_, reject) => {
  rejectChildFailure = reject;
});
// Child startup can fail before main reaches its first race. Register handlers immediately so
// Node never treats a prompt, already-observed failure as an unhandled rejection.
void reportPromise.catch(() => undefined);
void childFailurePromise.catch(() => undefined);

function appendLog(text) {
  capturedLogs = `${capturedLogs}${text}`.slice(-MAX_CAPTURED_LOG_CHARS);
}

function forwardOutput(stream, destination) {
  stream.setEncoding("utf8");
  stream.on("data", (text) => {
    appendLog(text);
    destination.write(text);
  });
}

function waitForChildClose(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
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
      () => finish(reject, new Error(`${path.basename(child.spawnfile ?? "child")} 停止超时`)),
      timeoutMs
    );
    child.once("close", onClose);
    if (child.exitCode !== null || child.signalCode !== null) finish(resolve);
  });
}

function taskkillProcessTree(pid, timeoutMs) {
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

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === "win32") {
    if (!Number.isInteger(child.pid)) {
      await waitForChildClose(child, 2_000);
      return;
    }
    const taskkillCode = await taskkillProcessTree(child.pid, 10_000);
    try {
      await waitForChildClose(child, 10_000);
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
    await waitForChildClose(child, 10_000);
  } catch {
    child.kill("SIGKILL");
    await waitForChildClose(child, 5_000);
  }
}

async function removeOwnedDirectory(target, label) {
  const transientCodes = new Set([
    "EACCES",
    "EBUSY",
    "EMFILE",
    "ENFILE",
    "ENOTEMPTY",
    "EPERM"
  ]);
  const retryDelays = [0, 250, 500, 1_000, 2_000, 3_000, 4_000];
  let lastError;
  for (const delayMs of retryDelays) {
    if (delayMs > 0) await new Promise((resolve) => setTimeout(resolve, delayMs));
    try {
      let metadata;
      try {
        metadata = lstatSync(target);
      } catch (error) {
        if (error?.code === "ENOENT") return;
        throw error;
      }
      if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
        throw new Error(`${label} 已被替换为非普通目录，拒绝清理`);
      }
      rmSync(target, { recursive: true, force: true, maxRetries: 4, retryDelay: 150 });
      if (existsSync(target)) throw Object.assign(new Error(`${label} 清理后仍存在`), { code: "EBUSY" });
      return;
    } catch (error) {
      lastError = error;
      if (!transientCodes.has(error?.code)) throw error;
    }
  }
  throw new Error(
    `${label} 在受限的原生句柄释放重试期后仍无法清理：${
      lastError instanceof Error ? lastError.message : String(lastError)
    }`
  );
}

async function cleanupOwnedDataDirectories() {
  if (
    !dataIdentifier.startsWith(DATA_IDENTIFIER_PREFIX)
    || dataIdentifier.length === DATA_IDENTIFIER_PREFIX.length
    || dataIdentifier.length > 128
    || /[^A-Za-z0-9._-]/.test(dataIdentifier)
  ) {
    throw new Error("拒绝清理无法验证的图片输入 E2E 数据标识");
  }
  const targets = [];
  for (const [label, directory] of [
    ["APPDATA", process.env.APPDATA],
    ["LOCALAPPDATA", process.env.LOCALAPPDATA]
  ]) {
    if (!directory) continue;
    const parent = path.resolve(directory);
    const target = path.resolve(parent, dataIdentifier);
    if (path.dirname(target) !== parent || path.basename(target) !== dataIdentifier) {
      throw new Error(`拒绝清理越出 ${label} 的图片输入 E2E 数据目录`);
    }
    targets.push({ label, target });
  }
  const results = await Promise.allSettled(targets.map(({ label, target }) => (
    removeOwnedDirectory(target, `${label} 图片输入 E2E 数据目录`)
  )));
  const failures = results.flatMap((result, index) => (
    result.status === "rejected"
      ? [`${targets[index].label}: ${
        result.reason instanceof Error ? result.reason.message : String(result.reason)
      }`]
      : []
  ));
  if (failures.length > 0) {
    throw new Error(failures.join("；"));
  }
}

async function cleanupBrowserProfileDirectory() {
  if (!browserProfileDirectory) return;
  const parent = path.resolve(tmpdir());
  const target = path.resolve(browserProfileDirectory);
  if (
    path.dirname(target) !== parent
    || !path.basename(target).startsWith("mework-image-input-e2e-browser-")
  ) {
    throw new Error("拒绝清理无法验证的图片输入 E2E 浏览器资料目录");
  }
  await removeOwnedDirectory(target, "可见 Chromium 隔离资料目录");
  browserProfileDirectory = undefined;
}

function scanLeaks(value, label) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (text.includes(MODEL_API_KEY)) throw new Error(`${label} 包含图片输入 E2E 假 Key`);
  if (/data:image\/(?:png|jpe?g|gif|webp);base64,/i.test(text)) {
    throw new Error(`${label} 包含图片 data URL`);
  }
  if (/[A-Za-z0-9+/]{512,}={0,2}/.test(text.replace(/\s+/g, ""))) {
    throw new Error(`${label} 包含疑似未脱敏的长图片编码`);
  }
}

function jsonResponse(response, status, value) {
  const body = status === 204 ? "" : JSON.stringify(value);
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "access-control-allow-origin": frontendOrigin,
    "access-control-allow-methods": "POST, OPTIONS",
    "access-control-allow-headers": "content-type, x-mework-e2e-report-token",
    "content-length": Buffer.byteLength(body)
  });
  response.end(body);
}

const reportServer = http.createServer((request, response) => {
  if (request.method === "OPTIONS" && request.url === "/result") {
    jsonResponse(response, 204, {});
    return;
  }
  if (
    request.method !== "POST"
    || request.url !== "/result"
    || request.headers.origin !== frontendOrigin
    || request.headers["content-type"]?.split(";")[0].trim() !== "application/json"
    || request.headers["x-mework-e2e-report-token"] !== reportToken
  ) {
    jsonResponse(response, 404, { error: "not_found" });
    return;
  }
  let bytes = 0;
  const chunks = [];
  request.on("data", (chunk) => {
    bytes += chunk.length;
    if (bytes > MAX_REPORT_BYTES) request.destroy(new Error("report_too_large"));
    else chunks.push(chunk);
  });
  request.on("end", () => {
    try {
      if (reportValue) throw new Error("duplicate_report");
      const parsed = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (
        !parsed
        || !["passed", "failed"].includes(parsed.status)
        || !Array.isArray(parsed.checks)
      ) throw new Error("invalid_report");
      reportValue = parsed;
      resolveReport(parsed);
      jsonResponse(response, 200, { ok: true });
    } catch (error) {
      rejectReport(error);
      jsonResponse(response, 400, { error: "invalid_report" });
    }
  });
});

function spawnChecked(command, args) {
  const child = spawn(command, args, {
    cwd: root,
    env: environment,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true
  });
  forwardOutput(child.stdout, process.stdout);
  forwardOutput(child.stderr, process.stderr);
  child.on("error", (error) => rejectChildFailure(error));
  child.on("exit", (code, signal) => {
    if (!shuttingDown && !reportValue) {
      rejectChildFailure(
        new Error(`${path.basename(command)} 提前退出（${signal ?? code ?? "unknown"}）`)
      );
    }
  });
  return child;
}

async function waitForUrl(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    const controller = new AbortController();
    const attemptTimeout = setTimeout(
      () => controller.abort(),
      Math.max(1, Math.min(2_000, deadline - Date.now()))
    );
    try {
      const response = await fetch(url, { cache: "no-store", signal: controller.signal });
      if (response.ok) return;
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    } finally {
      clearTimeout(attemptTimeout);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`等待 ${url} 超时：${lastError instanceof Error ? lastError.message : String(lastError)}`);
}

async function assertImageBrowserFixtureCsp() {
  const url = `http://127.0.0.1:${backendPort}/image-input-browser-e2e`;
  const response = await fetch(url, { cache: "no-store" });
  if (!response.ok) throw new Error(`图片浏览器夹具返回 HTTP ${response.status}`);
  const policy = response.headers.get("content-security-policy") ?? "";
  for (const directive of [
    "default-src 'none'",
    "script-src 'none'",
    "script-src-attr 'none'",
    "style-src 'none'",
    "style-src-attr 'none'",
    "frame-ancestors 'none'"
  ]) {
    if (!policy.split(";").some((part) => part.trim() === directive)) {
      throw new Error(`图片浏览器夹具 CSP 缺少 ${directive}`);
    }
  }
  if (/unsafe-inline|unsafe-eval|\*/u.test(policy)) {
    throw new Error("图片浏览器夹具 CSP 含宽松来源");
  }
  const body = await response.text();
  if (!body.includes("MEWORK_IMAGE_INPUT_BROWSER_E2E_READY")) {
    throw new Error("图片浏览器夹具缺少就绪标记");
  }
}

function openVisibleBrowser(url) {
  if (process.env.MEWORK_IMAGE_INPUT_E2E_NO_OPEN === "1") {
    process.stdout.write(`OPEN ${url}\n`);
    return;
  }
  let browserCommand;
  let browserArguments;
  if (process.platform === "win32") {
    const configured = process.env.MEWORK_IMAGE_INPUT_E2E_BROWSER?.trim();
    if (
      configured
      && (!path.isAbsolute(configured) || !existsSync(configured))
    ) {
      throw new Error(`MEWORK_IMAGE_INPUT_E2E_BROWSER 不存在或不是绝对路径：${configured}`);
    }
    const candidates = [
      process.env["ProgramFiles(x86)"]
        ? path.join(process.env["ProgramFiles(x86)"], "Microsoft", "Edge", "Application", "msedge.exe")
        : undefined,
      process.env.ProgramFiles
        ? path.join(process.env.ProgramFiles, "Microsoft", "Edge", "Application", "msedge.exe")
        : undefined,
      process.env.LOCALAPPDATA
        ? path.join(process.env.LOCALAPPDATA, "Microsoft", "Edge", "Application", "msedge.exe")
        : undefined,
      process.env.ProgramFiles
        ? path.join(process.env.ProgramFiles, "Google", "Chrome", "Application", "chrome.exe")
        : undefined,
      process.env["ProgramFiles(x86)"]
        ? path.join(process.env["ProgramFiles(x86)"], "Google", "Chrome", "Application", "chrome.exe")
        : undefined,
      process.env.LOCALAPPDATA
        ? path.join(process.env.LOCALAPPDATA, "Google", "Chrome", "Application", "chrome.exe")
        : undefined
    ].filter((candidate, index, all) => candidate && all.indexOf(candidate) === index);
    browserCommand = configured
      || candidates.find((candidate) => path.isAbsolute(candidate) && existsSync(candidate));
    if (!browserCommand) {
      throw new Error("找不到 Microsoft Edge 或 Google Chrome，无法启动 Chromium 图片真实界面验收");
    }
    browserProfileDirectory = mkdtempSync(
      path.join(path.resolve(tmpdir()), "mework-image-input-e2e-browser-")
    );
    browserArguments = [
      `--user-data-dir=${browserProfileDirectory}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-sync",
      "--disable-background-mode",
      // When fully occluded, Chromium marks the page hidden and suspends rendering.
      // IntersectionObserver then stops dispatching, making deferred-content assertions time out.
      // Keep the acceptance window rendered as a visible page.
      "--disable-features=CalculateNativeWinOcclusion",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--new-window",
      url
    ];
  } else {
    browserCommand = process.platform === "darwin" ? "open" : "xdg-open";
    browserArguments = [url];
  }
  const child = process.platform === "win32"
    ? spawn(browserCommand, browserArguments, {
        cwd: root,
        env: environment,
        stdio: "ignore",
        windowsHide: false
      })
    : spawn(browserCommand, browserArguments, {
        cwd: root,
        env: environment,
        detached: true,
        stdio: "ignore"
      });
  child.once("error", (error) => {
    if (!shuttingDown && !reportValue) {
      rejectChildFailure(new Error(`无法启动可见浏览器：${error.message}`));
    }
  });
  child.once("close", (code, signal) => {
    if (!shuttingDown && !reportValue) {
      rejectChildFailure(
        new Error(`可见浏览器在验收完成前退出（${signal ?? code ?? "unknown"}）`)
      );
    }
  });
  if (process.platform === "win32") visibleBrowserProcess = child;
  else child.unref();
}

function expectedProviderIds() {
  return [
    `image-e2e-openai_chat-${runId}`,
    `image-e2e-openai_responses-${runId}`,
    `image-e2e-anthropic-${runId}`
  ];
}

function invokeBrowserDev(command, args = {}, timeoutMs = 8_000) {
  if (typeof WebSocket !== "function") {
    return Promise.reject(new Error("当前 Node 运行时缺少 WebSocket，无法执行 E2E 宿主清理"));
  }
  return new Promise((resolve, reject) => {
    const url = new URL(`ws://127.0.0.1:${backendPort}/ws`);
    url.searchParams.set("token", bridgeToken);
    const socket = new WebSocket(url);
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        socket.close();
      } catch {
        // The result is already authoritative.
      }
      callback(value);
    };
    const timer = setTimeout(
      () => finish(reject, new Error(`调用 ${command} 超时`)),
      timeoutMs
    );
    socket.addEventListener("open", () => {
      try {
        socket.send(JSON.stringify({ id: 1, command, args }));
      } catch (error) {
        finish(reject, error);
      }
    }, { once: true });
    socket.addEventListener("message", (event) => {
      try {
        const message = JSON.parse(String(event.data));
        if (message?.type !== "result" || message.id !== 1) return;
        if (message.ok) finish(resolve, message.value);
        else finish(reject, new Error(message.error || `${command} 失败`));
      } catch (error) {
        finish(reject, error);
      }
    });
    socket.addEventListener(
      "error",
      () => finish(reject, new Error(`无法连接 browser-dev 执行 ${command}`)),
      { once: true }
    );
    socket.addEventListener(
      "close",
      () => finish(reject, new Error(`browser-dev 在返回 ${command} 结果前关闭连接`)),
      { once: true }
    );
  });
}

async function cleanupImageInputKeysViaBridge() {
  const result = await invokeBrowserDev("browser_e2e_cleanup_image_input_keys");
  if (
    !result
    || JSON.stringify(result.providerIds) !== JSON.stringify(expectedProviderIds())
    || !Array.isArray(result.configured)
    || result.configured.length !== 3
    || !result.configured.every((configured) => configured === false)
  ) {
    throw new Error("browser-dev 返回了无效的图片 E2E 凭据清理结果");
  }
  imageInputKeysVerifiedClean = true;
}

async function requestGracefulBrowserDevShutdown() {
  if (!browserDevProcess || browserDevProcess.exitCode !== null || browserDevProcess.signalCode !== null) {
    throw new Error("browser-dev 在最终 WebView2 释放验收前已停止");
  }

  // The page already deletes these credentials before and after its persistence restart. Verify
  // once more from the final backend before deliberately taking its authenticated bridge down.
  await cleanupImageInputKeysViaBridge();
  const instanceId = await invokeBrowserDev("browser_e2e_instance_id");
  if (typeof instanceId !== "string" || !/^[0-9a-f]{64}$/.test(instanceId)) {
    throw new Error("browser-dev 最终实例 ID 无效");
  }

  let acknowledgementError;
  try {
    const acceptance = await invokeBrowserDev(
      "browser_e2e_shutdown_backend",
      { instanceId },
      15_000
    );
    if (
      !acceptance
      || acceptance.accepted !== true
      || acceptance.instanceId !== instanceId
    ) {
      throw new Error("browser-dev 没有确认精确最终实例的 shutdown 请求");
    }
  } catch (error) {
    // As with the mid-run restart, a successful host can close the WebSocket after accepting the
    // request but before its queued result reaches Node. The wrapper's complete marker handshake
    // and clean exit below are the authoritative evidence.
    acknowledgementError = error;
  }

  try {
    await waitForChildClose(browserDevProcess, 120_000);
  } catch (error) {
    throw new Error(
      `browser-dev 未在 WebView2 释放屏障后正常退出：${
        error instanceof Error ? error.message : String(error)
      }${
        acknowledgementError
          ? `；bridge ACK: ${
              acknowledgementError instanceof Error
                ? acknowledgementError.message
                : String(acknowledgementError)
            }`
          : ""
      }`
    );
  }
  if (browserDevProcess.signalCode !== null || browserDevProcess.exitCode !== 0) {
    throw new Error(
      `browser-dev 最终退出不是受控成功（${
        browserDevProcess.signalCode ?? browserDevProcess.exitCode ?? "unknown"
      }）${
        acknowledgementError
          ? `；bridge ACK: ${
              acknowledgementError instanceof Error
                ? acknowledgementError.message
                : String(acknowledgementError)
            }`
          : ""
      }`
    );
  }
  gracefulBrowserDevShutdownComplete = true;
  process.stdout.write(
    `PASS webview2-release-and-final-shutdown ${
      acknowledgementError ? "wrapper marker handshake" : "bridge ACK + wrapper marker handshake"
    }\n`
  );
}

function shutdown() {
  if (shutdownPromise) return shutdownPromise;
  shutdownPromise = (async () => {
    shuttingDown = true;
    let keyCleanupError;
    if (
      pageStarted
      && !imageInputKeysVerifiedClean
      && browserDevProcess?.exitCode === null
      && browserDevProcess?.signalCode === null
    ) {
      try {
        await cleanupImageInputKeysViaBridge();
      } catch (error) {
        keyCleanupError = error;
      }
    }
    const [protocolStopResult, browserStopResult, visibleBrowserStopResult] = await Promise.allSettled([
      stopChild(protocolProcess),
      stopChild(browserDevProcess),
      stopChild(visibleBrowserProcess)
    ]);
    if (reportServerListening) {
      await new Promise((resolve) => reportServer.close(resolve));
      reportServerListening = false;
    }
    const cleanupErrors = [];
    if (protocolStopResult.status === "rejected") {
      cleanupErrors.push(protocolStopResult.reason);
    }
    if (browserStopResult.status === "rejected") {
      cleanupErrors.push(browserStopResult.reason);
      cleanupErrors.push(new Error("browser-dev 未确认退出，已保留隔离应用数据以避免删除活跃进程文件"));
    } else {
      try {
        await cleanupOwnedDataDirectories();
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (visibleBrowserStopResult.status === "rejected") {
      cleanupErrors.push(visibleBrowserStopResult.reason);
      cleanupErrors.push(new Error("可见 Chromium 未确认退出，已保留其隔离浏览器资料"));
    } else {
      try {
        await cleanupBrowserProfileDirectory();
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (keyCleanupError) cleanupErrors.push(keyCleanupError);
    if (cleanupErrors.length > 0) {
      throw new Error(
        `图片输入 E2E 清理失败：${cleanupErrors.map((error) =>
          error instanceof Error ? error.message : String(error)
        ).join("；")}`
      );
    }
  })();
  return shutdownPromise;
}

async function main() {
  const syntaxCheck = spawnSync(process.execPath, ["--check", path.join(root, "scripts", "protocol-e2e-mock.mjs")], {
    cwd: root,
    env: environment,
    encoding: "utf8",
    windowsHide: true
  });
  if (syntaxCheck.status !== 0) {
    throw new Error(`protocol mock 语法检查失败：${syntaxCheck.stderr || syntaxCheck.stdout}`);
  }
  const imageMockSelfCheck = spawnSync(
    process.execPath,
    [path.join(root, "scripts", "protocol-e2e-mock.mjs")],
    {
      cwd: root,
      env: { ...environment, MEWORK_IMAGE_E2E_SELF_CHECK: "1" },
      encoding: "utf8",
      windowsHide: true
    }
  );
  if (imageMockSelfCheck.status !== 0) {
    throw new Error(
      `protocol mock 图片语义自检失败：${
        imageMockSelfCheck.stderr || imageMockSelfCheck.stdout
      }`
    );
  }
  process.stdout.write(imageMockSelfCheck.stdout);

  await new Promise((resolve, reject) => {
    reportServer.once("error", reject);
    reportServer.listen(reporterPort, "127.0.0.1", () => {
      reportServerListening = true;
      resolve();
    });
  });
  protocolProcess = spawnChecked(process.execPath, [path.join(root, "scripts", "protocol-e2e-mock.mjs")]);
  browserDevProcess = spawnChecked(process.execPath, [path.join(root, "scripts", "browser-dev.mjs")]);

  await Promise.race([
    Promise.all([
      waitForUrl(`http://127.0.0.1:${protocolPort}/health`, 240_000),
      waitForUrl(`http://127.0.0.1:${backendPort}/image-input-browser-e2e`, 3_600_000),
      // A completely cold Windows GNU target can spend well over 30 minutes
      // building vendored OpenSSL and SQLCipher before
      // browser-dev can listen. Keep this startup budget separate from the
      // 15-minute page interaction/report budget below.
      waitForUrl(`${frontendOrigin}/image-input-e2e.html`, 3_600_000)
    ]),
    childFailurePromise
  ]);
  await assertImageBrowserFixtureCsp();
  const pageUrl = `${frontendOrigin}/image-input-e2e.html`;
  process.stdout.write(`READY image-input-e2e ${pageUrl}\n`);
  pageStarted = true;
  openVisibleBrowser(pageUrl);

  const timeout = new Promise((_, reject) => {
    setTimeout(() => reject(new Error("图片输入 E2E 页面在 15 分钟内没有报告结果")), 15 * 60_000).unref();
  });
  const report = await Promise.race([reportPromise, childFailurePromise, timeout]);
  scanLeaks(capturedLogs, "browser-dev/protocol mock 日志");
  scanLeaks(report, "E2E 报告");
  for (const check of report.checks) {
    process.stdout.write(`${check.state} ${check.name} ${check.detail}\n`);
  }
  if (report.failure) process.stderr.write(`${report.failure}\n`);
  if (report.status === "passed") {
    await requestGracefulBrowserDevShutdown();
    if (!gracefulBrowserDevShutdownComplete) {
      throw new Error("browser-dev 最终释放屏障没有产生完成证据");
    }
  }
  process.stdout.write(`${report.status.toUpperCase()} image-input-e2e\n`);
  return report.status === "passed" ? 0 : 1;
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    const signalExitCode = signal === "SIGINT" ? 130 : 143;
    void shutdown()
      .then(() => process.exit(signalExitCode))
      .catch((error) => {
        process.stderr.write(`FAIL image-input-e2e signal cleanup ${
          error instanceof Error ? error.message : String(error)
        }\n`);
        process.exit(1);
      });
  });
}

let exitCode = 1;
try {
  exitCode = await main();
} catch (error) {
  process.stderr.write(`FAIL image-input-e2e ${
    error instanceof Error ? error.stack ?? error.message : String(error)
  }\n`);
} finally {
  try {
    await shutdown();
  } catch (error) {
    process.stderr.write(`FAIL image-input-e2e cleanup ${
      error instanceof Error ? error.message : String(error)
    }\n`);
    exitCode = 1;
  }
}
process.exit(exitCode);
