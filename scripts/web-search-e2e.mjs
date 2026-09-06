import { randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { rmSync } from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DATA_IDENTIFIER_PREFIX = "com.mework.app.e2e.";
const MODEL_API_KEY = "MEWORK_E2E_FAKE_MODEL_KEY_91d8e2c4_not_real";
const SENSITIVE_VALUES = [
  MODEL_API_KEY,
  "fixture-only"
];
const MAX_REPORT_BYTES = 256 * 1024;
const MAX_CAPTURED_LOG_CHARS = 2 * 1024 * 1024;

function parseEnvironmentPort(name, raw, fallback) {
  if (raw === undefined) return fallback;
  if (!/^[0-9]{1,5}$/.test(raw)) throw new Error(`${name} 必须是有效端口`);
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    throw new Error(`${name} 必须在 1024–65535 范围内`);
  }
  return port;
}

function environmentPort(name, fallback) {
  return parseEnvironmentPort(name, process.env[name], fallback);
}

function assertDistinctPorts(ports) {
  if (new Set(ports).size !== ports.length) {
    throw new Error("联网搜索 E2E 的四个端口必须互不相同");
  }
}

function markPromiseHandled(promise) {
  void promise.catch(() => {});
  return promise;
}

function createInfrastructureFailureLatch(isShuttingDown) {
  let failure;
  let resolveFailure;
  const promise = new Promise((resolve) => {
    resolveFailure = resolve;
  });
  return {
    get failure() {
      return failure;
    },
    promise,
    latch(error) {
      if (isShuttingDown() || failure) return false;
      failure = error instanceof Error ? error : new Error(String(error));
      resolveFailure(failure);
      return true;
    }
  };
}

function raceAgainstFailure(work, failureLatch) {
  const handledWork = markPromiseHandled(Promise.resolve(work));
  if (failureLatch.failure) return Promise.reject(failureLatch.failure);
  return Promise.race([
    failureLatch.promise.then((error) => {
      throw error;
    }),
    handledWork
  ]);
}

function rejectDeferredAndLatch(rejectDeferred, latchFailure, error, prefix) {
  const failure = error instanceof Error ? error : new Error(String(error));
  rejectDeferred(failure);
  latchFailure(new Error(`${prefix}${failure.message}`));
  return failure;
}

function createSensitiveChunkFilter(sensitiveValues, onSensitiveValue) {
  const retainedCharacters = Math.max(
    0,
    ...sensitiveValues.map((value) => value.length - 1)
  );
  let pending = "";
  let finished = false;

  const drain = (flushAll) => {
    for (const sensitive of sensitiveValues) {
      if (!pending.includes(sensitive)) continue;
      onSensitiveValue(sensitive);
      pending = pending.split(sensitive).join("[REDACTED]");
    }
    const retained = flushAll ? 0 : retainedCharacters;
    const flushLength = Math.max(0, pending.length - retained);
    const output = pending.slice(0, flushLength);
    pending = pending.slice(flushLength);
    return output;
  };

  return {
    push(text) {
      if (finished) return "";
      pending += text;
      return drain(false);
    },
    finish() {
      if (finished) return "";
      const output = drain(true);
      finished = true;
      return output;
    }
  };
}

function selfCheckAssert(condition, message) {
  if (!condition) throw new Error(`runner_self_check_${message}`);
}

async function runRunnerSelfCheck() {
  selfCheckAssert(parseEnvironmentPort("PORT", undefined, 1420) === 1420, "fallback_port");
  selfCheckAssert(parseEnvironmentPort("PORT", "1024", 1420) === 1024, "minimum_port");
  selfCheckAssert(parseEnvironmentPort("PORT", "65535", 1420) === 65535, "maximum_port");
  for (const invalid of [
    "",
    " ",
    "\t1420",
    "1420 ",
    "+1420",
    "-1420",
    "1e3",
    "0x590",
    "1023",
    "65536",
    "999999"
  ]) {
    let rejected = false;
    try {
      parseEnvironmentPort("PORT", invalid, 1420);
    } catch {
      rejected = true;
    }
    selfCheckAssert(rejected, `accepted_invalid_port_${JSON.stringify(invalid)}`);
  }
  assertDistinctPorts([1420, 1430, 18080, 18081, 18082]);
  let duplicatePortsRejected = false;
  try {
    assertDistinctPorts([1420, 1430, 18080, 18081, 1430]);
  } catch {
    duplicatePortsRejected = true;
  }
  selfCheckAssert(duplicatePortsRejected, "duplicate_ports_accepted");

  for (const sensitive of SENSITIVE_VALUES) {
    for (let split = 0; split <= sensitive.length; split += 1) {
      let detections = 0;
      const filter = createSensitiveChunkFilter(SENSITIVE_VALUES, () => {
        detections += 1;
      });
      let output = filter.push(`before:${sensitive.slice(0, split)}`);
      output += filter.push(`${sensitive.slice(split)}:after`);
      output += filter.finish();
      selfCheckAssert(output === "before:[REDACTED]:after", "cross_chunk_redaction");
      selfCheckAssert(detections >= 1, "cross_chunk_detection");
      selfCheckAssert(!output.includes(sensitive), "cross_chunk_leak");
    }
    let characterDetections = 0;
    const characterFilter = createSensitiveChunkFilter(SENSITIVE_VALUES, () => {
      characterDetections += 1;
    });
    let characterOutput = characterFilter.push("character:");
    for (const character of sensitive) characterOutput += characterFilter.push(character);
    characterOutput += characterFilter.finish();
    selfCheckAssert(
      characterOutput === "character:[REDACTED]",
      "character_chunk_redaction"
    );
    selfCheckAssert(characterDetections >= 1, "character_chunk_detection");
  }
  const ordinaryText = "ordinary-output-".repeat(16);
  const ordinaryFilter = createSensitiveChunkFilter(SENSITIVE_VALUES, () => {
    throw new Error("runner_self_check_false_sensitive_detection");
  });
  const ordinaryOutput = ordinaryFilter.push(ordinaryText.slice(0, 37))
    + ordinaryFilter.push(ordinaryText.slice(37))
    + ordinaryFilter.finish();
  selfCheckAssert(ordinaryOutput === ordinaryText, "ordinary_output_changed");
  selfCheckAssert(ordinaryFilter.finish() === "", "filter_finish_not_idempotent");

  let shuttingDownForCheck = false;
  const latch = createInfrastructureFailureLatch(() => shuttingDownForCheck);
  const firstFailure = new Error("first_infrastructure_failure");
  selfCheckAssert(latch.latch(firstFailure), "first_failure_not_latched");
  selfCheckAssert(!latch.latch(new Error("second_failure")), "second_failure_replaced_first");
  let racedFailure;
  try {
    await raceAgainstFailure(Promise.resolve("must_not_mask_failure"), latch);
  } catch (error) {
    racedFailure = error;
  }
  selfCheckAssert(racedFailure === firstFailure, "settled_failure_not_observed");
  selfCheckAssert(latch.failure === firstFailure, "latched_failure_identity_changed");

  const shutdownLatch = createInfrastructureFailureLatch(() => shuttingDownForCheck);
  shuttingDownForCheck = true;
  selfCheckAssert(
    !shutdownLatch.latch(new Error("expected_shutdown_close")),
    "shutdown_close_was_latched"
  );
  selfCheckAssert(shutdownLatch.failure === undefined, "shutdown_false_positive");

  let resolveSettledReport;
  let rejectSettledReport;
  const settledReport = markPromiseHandled(new Promise((resolve, reject) => {
    resolveSettledReport = resolve;
    rejectSettledReport = reject;
  }));
  resolveSettledReport("accepted");
  selfCheckAssert(await settledReport === "accepted", "report_did_not_settle");
  shuttingDownForCheck = false;
  const lateReportLatch = createInfrastructureFailureLatch(() => shuttingDownForCheck);
  rejectDeferredAndLatch(
    rejectSettledReport,
    (error) => lateReportLatch.latch(error),
    new Error("duplicate_report"),
    "report:"
  );
  selfCheckAssert(
    lateReportLatch.failure?.message === "report:duplicate_report",
    "settled_report_failure_was_silent"
  );

  const unhandled = [];
  const onUnhandled = (reason) => unhandled.push(reason);
  process.on("unhandledRejection", onUnhandled);
  try {
    const earlyReportRejection = markPromiseHandled(
      Promise.reject(new Error("early_report_rejection"))
    );
    let earlyReportError;
    try {
      await earlyReportRejection;
    } catch (error) {
      earlyReportError = error;
    }
    selfCheckAssert(
      earlyReportError?.message === "early_report_rejection",
      "handled_report_rejection_changed"
    );

    const preLatched = createInfrastructureFailureLatch(() => false);
    const preLatchedFailure = new Error("pre_latched_failure");
    preLatched.latch(preLatchedFailure);
    let rejectLosingWork;
    const losingWork = new Promise((_, reject) => {
      rejectLosingWork = reject;
    });
    let observedPreLatchedFailure;
    try {
      await raceAgainstFailure(losingWork, preLatched);
    } catch (error) {
      observedPreLatchedFailure = error;
    }
    selfCheckAssert(
      observedPreLatchedFailure === preLatchedFailure,
      "pre_latched_failure_was_masked"
    );
    rejectLosingWork(new Error("losing_work_rejection"));

    let postWinnerShutdown = false;
    const postWinnerLatch = createInfrastructureFailureLatch(() => postWinnerShutdown);
    const winner = await raceAgainstFailure(Promise.resolve("winner"), postWinnerLatch);
    selfCheckAssert(winner === "winner", "failure_race_changed_winner");
    const failureAfterWinner = new Error("failure_after_winner");
    postWinnerLatch.latch(failureAfterWinner);
    let lateFailure;
    try {
      await raceAgainstFailure(new Promise(() => {}), postWinnerLatch);
    } catch (error) {
      lateFailure = error;
    }
    selfCheckAssert(lateFailure === failureAfterWinner, "late_failure_was_silent");
    await new Promise((resolve) => setImmediate(resolve));
    selfCheckAssert(unhandled.length === 0, "unhandled_rejection");
  } finally {
    process.off("unhandledRejection", onUnhandled);
  }
}

if (process.env.MEWORK_WEB_SEARCH_E2E_RUNNER_SELF_CHECK === "1") {
  await runRunnerSelfCheck();
  process.stdout.write("SELF_CHECK web-search-e2e runner PASS\n");
  process.exit(0);
}

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const frontendPort = environmentPort("MEWORK_BROWSER_DEV_FRONTEND_PORT", 1420);
const backendPort = environmentPort("MEWORK_BROWSER_DEV_BACKEND_PORT", 1430);
const protocolPort = environmentPort("MEWORK_PROTOCOL_E2E_PORT", 18080);
const reporterPort = environmentPort("MEWORK_WEB_SEARCH_E2E_REPORT_PORT", 18081);
const ports = [frontendPort, backendPort, protocolPort, reporterPort];
assertDistinctPorts(ports);

const frontendOrigin = `http://127.0.0.1:${frontendPort}`;
const browserOrigin = `http://127.0.0.1:${backendPort}`;
const protocolBaseUrl = `http://127.0.0.1:${protocolPort}/v1`;
const reportUrl = `http://127.0.0.1:${reporterPort}/result`;
const runId = randomBytes(12).toString("hex");
const reportToken = randomBytes(32).toString("hex");
const dataIdentifier = `${DATA_IDENTIFIER_PREFIX}web-search-${randomBytes(12).toString("hex")}`;
const environment = {
  ...process.env,
  MEWORK_BROWSER_DEV_FRONTEND_PORT: String(frontendPort),
  MEWORK_BROWSER_DEV_BACKEND_PORT: String(backendPort),
  MEWORK_PROTOCOL_E2E_PORT: String(protocolPort),
  MEWORK_BROWSER_DEV_DATA_IDENTIFIER: dataIdentifier,
  MEWORK_WEB_SEARCH_E2E: "1",
  MEWORK_WEB_SEARCH_E2E_RUN_ID: runId,
  VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL: protocolBaseUrl,
  VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN: browserOrigin,
  VITE_WEB_SEARCH_E2E_RUN_ID: runId,
  VITE_WEB_SEARCH_E2E_REPORT_URL: reportUrl,
  VITE_WEB_SEARCH_E2E_REPORT_TOKEN: reportToken
};

let browserDevProcess;
let protocolProcess;
let shuttingDown = false;
let reportValue;
let resolveReport;
let rejectReport;
let capturedLogs = "";
let reportServerListening = false;
const infrastructureFailureLatch = createInfrastructureFailureLatch(() => shuttingDown);
const reportPromise = markPromiseHandled(new Promise((resolve, reject) => {
  resolveReport = resolve;
  rejectReport = reject;
}));

function appendLog(text) {
  capturedLogs = `${capturedLogs}${text}`.slice(-MAX_CAPTURED_LOG_CHARS);
}

function latchInfrastructureFailure(error) {
  infrastructureFailureLatch.latch(error);
}

function raceInfrastructureFailure(work) {
  return raceAgainstFailure(work, infrastructureFailureLatch);
}

function failReport(error) {
  // A rejected report Promise cannot change state after a valid report already resolved it.
  // Latch the failure separately so duplicate/late malformed reports still revoke a pending PASS.
  rejectDeferredAndLatch(
    rejectReport,
    latchInfrastructureFailure,
    error,
    "联网搜索报告失败："
  );
}

function redactSensitiveValues(text) {
  let redacted = String(text ?? "");
  for (const sensitive of SENSITIVE_VALUES) {
    if (!redacted.includes(sensitive)) continue;
    redacted = redacted.split(sensitive).join("[REDACTED]");
  }
  return redacted;
}

function forwardOutput(stream, destination, label) {
  let ended = false;
  const filter = createSensitiveChunkFilter(SENSITIVE_VALUES, () => {
    latchInfrastructureFailure(new Error(`${label} 包含已知假 Key/Cookie 值`));
  });
  const write = (output) => {
    if (!output) return;
    appendLog(output);
    destination.write(output);
  };
  const finish = () => {
    if (ended) return;
    ended = true;
    write(filter.finish());
  };
  stream.setEncoding("utf8");
  stream.on("data", (text) => {
    write(filter.push(text));
  });
  stream.on("error", (error) => {
    latchInfrastructureFailure(new Error(`${label} 输出流失败：${error.message}`));
  });
  stream.on("end", finish);
  stream.on("close", finish);
}

function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === "win32") {
    const terminator = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
      windowsHide: true,
      stdio: "ignore"
    });
    terminator.on("error", () => {
      // Cleanup continues to listener closure/data removal; process.exit is the final owner fence.
    });
  } else {
    child.kill("SIGINT");
  }
}

function cleanupOwnedDataDirectories() {
  if (
    !dataIdentifier.startsWith(DATA_IDENTIFIER_PREFIX)
    || dataIdentifier.length === DATA_IDENTIFIER_PREFIX.length
    || dataIdentifier.length > 128
    || /[^A-Za-z0-9._-]/.test(dataIdentifier)
  ) {
    throw new Error("拒绝清理无法验证的联网搜索 E2E 数据标识");
  }
  for (const [label, directory] of [
    ["APPDATA", process.env.APPDATA],
    ["LOCALAPPDATA", process.env.LOCALAPPDATA]
  ]) {
    if (!directory) continue;
    const parent = path.resolve(directory);
    const target = path.resolve(parent, dataIdentifier);
    if (path.dirname(target) !== parent || path.basename(target) !== dataIdentifier) {
      throw new Error(`拒绝清理越出 ${label} 的联网搜索 E2E 数据目录`);
    }
    rmSync(target, { recursive: true, force: true, maxRetries: 4, retryDelay: 150 });
  }
}

function scanLeaks(value, label) {
  const text = typeof value === "string"
    ? value
    : JSON.stringify(value) ?? String(value);
  for (const sensitive of SENSITIVE_VALUES) {
    if (text.includes(sensitive)) throw new Error(`${label} 包含已知假 Key/Cookie 值`);
  }
}

function jsonResponse(response, status, value) {
  const body = JSON.stringify(value);
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

// A `passed` report must have actually run every leg. Without this the page
// could report success while silently skipping phases — the failure mode this
// list exists to prevent. Keep it in step with the phases in
// `src/webSearchE2E.ts`; a renamed check must fail loudly here, not vanish.
function assertPassedWebSearchEvidence(report) {
  if (report.status !== "passed") return;
  for (const name of [
    "search-provider-credential-safety",
    "web-search-anthropic-native-tool",
    "web-search-native-backend",
    "web-search-result-error-is-an-outcome"
  ]) {
    if (
      !report.checks.some((check) => check?.name === name && check.state === "PASS")
    ) {
      throw new Error(`passed_report_missing_check_${name}`);
    }
  }
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
  response.on("error", () => {
    failReport(new Error("report_response_failed"));
  });
  let bytes = 0;
  let reportTooLarge = false;
  const chunks = [];
  request.on("data", (chunk) => {
    if (reportTooLarge) return;
    bytes += chunk.length;
    if (bytes > MAX_REPORT_BYTES) {
      reportTooLarge = true;
      chunks.length = 0;
      failReport(new Error("report_too_large"));
      jsonResponse(response, 413, { error: "report_too_large" });
      return;
    }
    chunks.push(chunk);
  });
  request.on("end", () => {
    if (reportTooLarge) return;
    try {
      if (reportValue) throw new Error("duplicate_report");
      const parsed = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (
        !parsed
        || !["passed", "failed", "blocked"].includes(parsed.status)
        || !Array.isArray(parsed.checks)
      ) throw new Error("invalid_report");
      assertPassedWebSearchEvidence(parsed);
      reportValue = parsed;
      resolveReport(parsed);
      jsonResponse(response, 200, { ok: true });
    } catch (error) {
      failReport(error);
      jsonResponse(response, 400, { error: "invalid_report" });
    }
  });
  request.on("aborted", () => {
    if (!reportTooLarge && !reportValue) {
      failReport(new Error("report_request_aborted"));
    }
  });
  request.on("error", () => {
    if (!reportTooLarge && !reportValue) {
      failReport(new Error("report_request_failed"));
    }
  });
});

reportServer.on("error", (error) => {
  latchInfrastructureFailure(new Error(`联网搜索报告服务失败：${error.message}`));
});

reportServer.on("close", () => {
  reportServerListening = false;
  if (!shuttingDown) {
    latchInfrastructureFailure(new Error("联网搜索报告服务在判定前关闭"));
  }
});

function assertReportServerReady() {
  if (infrastructureFailureLatch.failure) throw infrastructureFailureLatch.failure;
  if (!reportServerListening || !reportServer.listening) {
    throw new Error("联网搜索报告服务在判定前已停止监听");
  }
}

function spawnChecked(command, args, options) {
  const child = spawn(command, args, {
    cwd: root,
    env: environment,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    ...options
  });
  const processLabel = path.basename(command);
  forwardOutput(child.stdout, process.stdout, `${processLabel} stdout`);
  forwardOutput(child.stderr, process.stderr, `${processLabel} stderr`);
  child.on("error", (error) => latchInfrastructureFailure(error));
  child.on("exit", (code, signal) => {
    if (!shuttingDown) {
      latchInfrastructureFailure(
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
    try {
      const response = await fetch(url, { cache: "no-store" });
      if (response.ok) return;
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`等待 ${url} 超时：${lastError instanceof Error ? lastError.message : String(lastError)}`);
}

function openVisibleBrowser(url) {
  if (process.env.MEWORK_WEB_SEARCH_E2E_NO_OPEN === "1") {
    process.stdout.write(`OPEN ${url}\n`);
    return;
  }
  const child = process.platform === "win32"
    ? spawn("rundll32.exe", ["url.dll,FileProtocolHandler", url], {
        cwd: root,
        env: environment,
        detached: true,
        stdio: "ignore",
        windowsHide: false
      })
    : process.platform === "darwin"
      ? spawn("open", [url], { cwd: root, env: environment, detached: true, stdio: "ignore" })
      : spawn("xdg-open", [url], { cwd: root, env: environment, detached: true, stdio: "ignore" });
  child.on("error", (error) => {
    latchInfrastructureFailure(new Error(`无法打开联网搜索 E2E 页面：${error.message}`));
  });
  child.unref();
}

async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  stopChild(protocolProcess);
  stopChild(browserDevProcess);
  await new Promise((resolve) => setTimeout(resolve, 900));
  const closeResults = await Promise.allSettled([
    reportServerListening
      ? new Promise((resolve, reject) => reportServer.close((error) => error ? reject(error) : resolve()))
      : Promise.resolve()
  ]);
  reportServerListening = false;
  let cleanupError;
  try {
    cleanupOwnedDataDirectories();
  } catch (error) {
    cleanupError = error;
  }
  const closeErrors = closeResults
    .filter((result) => result.status === "rejected")
    .map((result) => result.reason instanceof Error ? result.reason.message : String(result.reason));
  if (cleanupError || closeErrors.length > 0) {
    const details = [
      ...closeErrors,
      ...(cleanupError
        ? [cleanupError instanceof Error ? cleanupError.message : String(cleanupError)]
        : [])
    ];
    throw new Error(`联网搜索 E2E 清理未完全成功：${details.join("；")}`);
  }
}

async function main() {
  await runRunnerSelfCheck();
  process.stdout.write("SELF_CHECK web-search-e2e runner PASS\n");
  const selfCheck = spawnSync(process.execPath, [path.join(root, "scripts", "protocol-e2e-mock.mjs")], {
    cwd: root,
    env: {
      ...environment,
      MEWORK_WEB_SEARCH_E2E_SELF_CHECK: "1"
    },
    encoding: "utf8",
    windowsHide: true
  });
  const selfCheckStdout = selfCheck.stdout ?? "";
  const selfCheckStderr = selfCheck.stderr ?? "";
  if (
    selfCheck.status !== 0
    || !selfCheckStdout.includes("SELF_CHECK protocol-e2e-mock web-search PASS")
  ) {
    const diagnostic = redactSensitiveValues(
      selfCheckStderr || selfCheckStdout || selfCheck.error?.message
    );
    throw new Error(`protocol mock self-check 失败：${diagnostic}`);
  }
  scanLeaks(selfCheckStdout, "protocol mock self-check stdout");
  scanLeaks(selfCheckStderr, "protocol mock self-check stderr");
  process.stdout.write(selfCheckStdout);

  await raceInfrastructureFailure(new Promise((resolve, reject) => {
    reportServer.once("error", reject);
    reportServer.listen(reporterPort, "127.0.0.1", () => {
      reportServerListening = true;
      resolve();
    });
  }));
  protocolProcess = spawnChecked(process.execPath, [path.join(root, "scripts", "protocol-e2e-mock.mjs")]);
  browserDevProcess = spawnChecked(process.execPath, [path.join(root, "scripts", "browser-dev.mjs")]);

  await raceInfrastructureFailure(Promise.all([
    waitForUrl(`http://127.0.0.1:${protocolPort}/health`, 180_000),
    // A cold Windows/GNU build may compile vendored OpenSSL before browser-dev can bind.
    waitForUrl(`${frontendOrigin}/web-search-e2e.html`, 900_000)
  ]));
  const pageUrl = `${frontendOrigin}/web-search-e2e.html`;
  process.stdout.write(`READY web-search-e2e ${pageUrl}\n`);
  openVisibleBrowser(pageUrl);

  const timeout = new Promise((_, reject) => {
    setTimeout(() => reject(new Error("联网搜索 E2E 页面在 12 分钟内没有报告结果")), 12 * 60_000).unref();
  });
  const report = await raceInfrastructureFailure(Promise.race([reportPromise, timeout]));
  assertReportServerReady();
  scanLeaks(capturedLogs, "browser-dev/protocol mock 日志");
  scanLeaks(report, "E2E 报告");

  for (const check of report.checks) {
    process.stdout.write(`${check.state} ${check.name} ${check.detail}\n`);
  }
  if (report.failure) process.stderr.write(`${report.failure}\n`);
  process.stdout.write(`${report.status.toUpperCase()} web-search-e2e\n`);
  return report.status === "passed" ? 0 : report.status === "blocked" ? 2 : 1;
}

let exitCode = 1;
try {
  exitCode = await main();
} catch (error) {
  process.stderr.write(`FAIL web-search-e2e ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
} finally {
  try {
    await shutdown();
  } catch (error) {
    process.stderr.write(`FAIL web-search-e2e cleanup ${error instanceof Error ? error.message : String(error)}\n`);
    exitCode = 1;
  }
}
process.exit(exitCode);
