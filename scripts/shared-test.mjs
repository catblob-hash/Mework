import { randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  delay,
  fingerprintWorkspaceInputs,
  readWorkspaceLease,
  recoverEmptyWorkspaceLease,
  removeWorkspaceLease,
  resolveLeaseDirectory,
  tryAcquireWorkspaceLease,
  updateWorkspaceLease,
  workspaceLeaseDirectoryExists
} from "./workspace-coordinator.mjs";
import {
  classifySharedTestLease,
  SHARED_TEST_PROTOCOL
} from "./shared-test-coordinator.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const directory = resolveLeaseDirectory(root, "verification", "npm-test");
const force = process.env.MEWORK_FORCE_TEST?.trim() === "1";

const fingerprint = fingerprintWorkspaceInputs(root, [
  "index.html",
  "package-lock.json",
  "package.json",
  "scripts",
  "src",
  "src-tauri/Cargo.lock",
  "src-tauri/Cargo.toml",
  "src-tauri/build.rs",
  "src-tauri/src",
  "src-tauri/tests",
  "tsconfig.json",
  "tsconfig.node.json",
  "vite.config.ts",
  "vitest.config.ts"
], {
  salt: `${SHARED_TEST_PROTOCOL}\0${process.version}\0${process.platform}\0${process.arch}`
});

function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === "win32") {
    spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
      windowsHide: true,
      stdio: "ignore"
    });
  } else {
    child.kill("SIGINT");
  }
}

function runDirectTest() {
  const npmCli = process.env.npm_execpath?.trim();
  if (!npmCli || !path.isAbsolute(npmCli)) {
    throw new Error("共享测试必须由 npm test 启动，且 npm_execpath 必须是绝对路径");
  }
  const child = spawn(process.execPath, [npmCli, "run", "test:direct"], {
    cwd: root,
    env: process.env,
    stdio: "inherit"
  });
  process.once("SIGINT", () => stopChild(child));
  process.once("SIGTERM", () => stopChild(child));
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      resolve({
        exitCode: Number.isInteger(code) ? code : 1,
        signal: signal ?? null
      });
    });
  });
}

let lastProgressAt = 0;
for (;;) {
  const sessionId = randomBytes(16).toString("hex");
  const now = new Date().toISOString();
  if (await tryAcquireWorkspaceLease({
    workspaceRoot: root,
    directory,
    state: {
      protocol: SHARED_TEST_PROTOCOL,
      sessionId,
      ownerPid: process.pid,
      fingerprint,
      status: "running",
      startedAt: now,
      updatedAt: now
    }
  })) {
    const startedAt = Date.now();
    process.stdout.write(
      `[test:shared] 本 Agent 获得全量测试单飞 lease，指纹 ${fingerprint.slice(0, 12)}\n`
    );
    let result;
    try {
      result = await runDirectTest();
    } catch (error) {
      result = { exitCode: 1, signal: null, error: error.message };
    }
    const completedAt = new Date().toISOString();
    updateWorkspaceLease({
      workspaceRoot: root,
      directory,
      sessionId,
      update: {
        status: "complete",
        completedAt,
        durationMs: Date.now() - startedAt,
        result
      }
    });
    process.exitCode = result.exitCode;
    break;
  }

  const state = readWorkspaceLease({ workspaceRoot: root, directory });
  if (!state) {
    if (await recoverEmptyWorkspaceLease({ workspaceRoot: root, directory })) {
      continue;
    }
    if (!workspaceLeaseDirectoryExists({
      workspaceRoot: root,
      directory
    })) {
      // Another process may hold the OS mutex before publishing the canonical
      // directory. Fall through to the ordinary backoff instead of rebinding
      // the same named pipe in a native-handle busy loop.
    }
  }
  const classification = classifySharedTestLease({
    state,
    fingerprint,
    forceRun: force
  });
  if (classification === "reuse") {
    const result = state.result ?? { exitCode: 1 };
    process.stdout.write(
      `[test:shared] 复用同一工作树指纹的全量测试结果（${Math.round((state.durationMs ?? 0) / 1000)} 秒，exit ${result.exitCode}）\n`
    );
    process.exitCode = Number.isInteger(result.exitCode) ? result.exitCode : 1;
    break;
  }
  if (classification === "incompatible") {
    throw new Error("全量测试协调状态版本不兼容；请检查 .codex-tmp/verification/npm-test");
  }
  if (classification === "stale" || classification === "replace") {
    if (await removeWorkspaceLease({
      workspaceRoot: root,
      directory,
      sessionId: state.sessionId
    })) {
      continue;
    }
  }
  if (Date.now() - lastProgressAt >= 15_000) {
    lastProgressAt = Date.now();
    const sameFingerprint = state?.fingerprint === fingerprint;
    process.stdout.write(
      sameFingerprint
        ? `[test:shared] 相同源码的全量测试已由 PID ${state?.ownerPid ?? "unknown"} 执行，等待共享结果\n`
        : `[test:shared] 工作区已有另一份全量测试，等待其释放单飞 lease\n`
    );
  }
  await delay(400);
}
