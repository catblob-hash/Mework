import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { requestBrowserDevStop } from "../browser-dev-coordinator.mjs";
import {
  readWorkspaceLease,
  resolveLeaseDirectory
} from "../workspace-coordinator.mjs";

const fixtureWorker = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "fixtures",
  "browser-dev-concurrency-contender.mjs"
);

function reservePort() {
  const server = createServer();
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("无法为 browser-dev fixture 预留端口"));
        return;
      }
      resolve({ server, port: address.port });
    });
  });
}

function closeReservation(reservation) {
  return new Promise((resolve, reject) => {
    reservation.server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

function waitForExit(child) {
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

function staggerContenderStart() {
  return new Promise((resolve) => setTimeout(resolve, 8));
}

function startContender({
  client,
  label = client,
  workspaceRoot,
  origin,
  backendOrigin,
  failRebuilds = false,
  startupTimeoutMs = 30_000
}) {
  const child = spawn(process.execPath, [
    fixtureWorker,
    `--${client}`
  ], {
    env: {
      ...process.env,
      MEWORK_BROWSER_DEV_FIXTURE_WORKSPACE: workspaceRoot,
      MEWORK_BROWSER_DEV_FIXTURE_ORIGIN: origin,
      MEWORK_BROWSER_DEV_FIXTURE_BACKEND_ORIGIN: backendOrigin,
      MEWORK_BROWSER_DEV_FIXTURE_FAIL_REBUILDS: failRebuilds ? "1" : "0",
      MEWORK_BROWSER_DEV_FIXTURE_STARTUP_TIMEOUT_MS: String(startupTimeoutMs)
    },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true
  });
  const output = { stdout: "", stderr: "" };
  let stdoutBuffer = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    output.stderr += chunk;
  });
  const events = [];
  const result = new Promise((resolve, reject) => {
    const handleMessage = (message) => {
      if (message?.type === "rebuild") {
        events.push(message);
        return;
      }
      if (message?.type === "error") {
        reject(new Error(message.message));
        return;
      }
      if (message?.type !== "result") {
        reject(new Error(`未知 fixture IPC 消息：${JSON.stringify(message)}`));
        return;
      }
      resolve(message);
    };
    child.stdout.on("data", (chunk) => {
      output.stdout += chunk;
      stdoutBuffer += chunk;
      for (;;) {
        const newline = stdoutBuffer.indexOf("\n");
        if (newline < 0) break;
        const line = stdoutBuffer.slice(0, newline).trim();
        stdoutBuffer = stdoutBuffer.slice(newline + 1);
        if (!line) continue;
        try {
          handleMessage(JSON.parse(line));
        } catch (error) {
          reject(new Error(
            `fixture contender ${label}/${client} 输出无效 JSONL: ${
              error instanceof Error ? error.message : String(error)
            }\n${line}`
          ));
        }
      }
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 || signal === null && code === null) return;
      reject(new Error(
        `fixture contender ${label}/${client} PID ${child.pid} 提前退出（${
          signal ?? code
        }）\n`
        + `${output.stdout}${output.stderr}`
      ));
    });
  });
  return { child, events, output, result, exit: waitForExit(child) };
}

test("mixed Codex and Claude processes share, coalesce Rust rebuilds, and hand off owner code", {
  timeout: 60_000
}, async () => {
  const workspaceRoot = mkdtempSync(
    path.join(os.tmpdir(), "mework-browser-dev-concurrency-")
  );
  const reservations = await Promise.all([reservePort(), reservePort()]);
  const [frontendReservation, backendReservation] = reservations;
  const origin = `http://127.0.0.1:${frontendReservation.port}`;
  const backendOrigin = `http://127.0.0.1:${backendReservation.port}`;
  const contenders = [];

  try {
    await Promise.all(reservations.map(closeReservation));
    for (let index = 0; index < 8; index += 1) {
      contenders.push(startContender({
        client: index % 2 === 0 ? "codex" : "claude",
        label: `initial-${index}`,
        workspaceRoot,
        origin,
        backendOrigin
      }));
      if (index < 7) await staggerContenderStart();
    }

    const initialResults = await Promise.all(
      contenders.map(({ result }) => result)
    );
    const owners = initialResults.filter(({ role }) => role === "owner");
    const followers = initialResults.filter(({ role }) => role === "follower");

    assert.equal(owners.length, 1);
    assert.equal(followers.length, 7);
    assert.equal(
      initialResults.filter(({ client }) => client === "codex").length,
      4
    );
    assert.equal(
      initialResults.filter(({ client }) => client === "claude").length,
      4
    );
    assert.equal(
      new Set(initialResults.map(({ sessionId }) => sessionId)).size,
      1
    );
    assert.equal(
      new Set(initialResults.map(({ ownerPid }) => ownerPid)).size,
      1
    );
    assert.equal(owners[0].processId, owners[0].ownerPid);
    assert.equal(
      new Set(initialResults.map(({ origin: value }) => value)).size,
      1
    );
    assert.equal(
      new Set(initialResults.map(({ backendOrigin: value }) => value)).size,
      1
    );

    const ownerIndex = initialResults.findIndex(({ role }) => role === "owner");
    const ownerContender = contenders[ownerIndex];
    mkdirSync(path.join(workspaceRoot, "src-tauri"), { recursive: true });
    writeFileSync(
      path.join(workspaceRoot, "src-tauri", "app_commands.rs"),
      "// browser-dev updated Rust fixture\n",
      "utf8"
    );
    const updatedContenders = [];
    for (let index = 0; index < 4; index += 1) {
      const contender = startContender({
        client: index % 2 === 0 ? "claude" : "codex",
        label: `rust-change-${index}`,
        workspaceRoot,
        origin,
        backendOrigin
      });
      updatedContenders.push(contender);
      contenders.push(contender);
      if (index < 3) await staggerContenderStart();
    }
    const updatedResults = await Promise.all(
      updatedContenders.map(({ result }) => result)
    );
    assert.equal(
      updatedResults.every(({ role }) => role === "follower"),
      true
    );
    assert.equal(
      new Set(updatedResults.map(({ sessionId }) => sessionId)).size,
      1
    );
    assert.equal(updatedResults[0].sessionId, owners[0].sessionId);
    assert.equal(
      updatedResults.every(({ ownerPid }) => ownerPid === owners[0].ownerPid),
      true
    );
    assert.equal(ownerContender.events.length, 1);
    assert.equal(ownerContender.events[0].ordinal, 1);

    writeFileSync(
      path.join(workspaceRoot, "package.json"),
      '{"name":"browser-dev-updated-owner-fixture"}\n',
      "utf8"
    );
    const handoffContenders = [];
    for (let index = 0; index < 4; index += 1) {
      const contender = startContender({
        client: index % 2 === 0 ? "codex" : "claude",
        label: `owner-handoff-${index}`,
        workspaceRoot,
        origin,
        backendOrigin
      });
      handoffContenders.push(contender);
      contenders.push(contender);
      if (index < 3) await staggerContenderStart();
    }
    const handoffResults = await Promise.all(
      handoffContenders.map(({ result }) => result)
    );
    const replacementOwners = handoffResults.filter(({ role }) => role === "owner");
    assert.equal(replacementOwners.length, 1);
    assert.equal(
      handoffResults.filter(({ role }) => role === "follower").length,
      3
    );
    assert.notEqual(replacementOwners[0].sessionId, owners[0].sessionId);
    assert.notEqual(replacementOwners[0].ownerPid, owners[0].ownerPid);
    assert.equal(
      new Set(handoffResults.map(({ sessionId }) => sessionId)).size,
      1
    );
    assert.equal(
      new Set(handoffResults.map(({ ownerPid }) => ownerPid)).size,
      1
    );

    const stop = await requestBrowserDevStop({
      workspaceRoot,
      timeoutMs: 10_000
    });
    assert.deepEqual(stop, {
      stopped: true,
      alreadyStopped: false,
      ownerPid: replacementOwners[0].ownerPid
    });

    const exits = await Promise.all(contenders.map(({ exit }) => exit));
    assert.equal(exits.every(({ code, signal }) => code === 0 && signal === null), true);
    assert.equal(existsSync(path.join(workspaceRoot, ".codex-tmp", "browser-dev")), false);
  } finally {
    for (const { child } of contenders) {
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
    rmSync(workspaceRoot, { recursive: true, force: true });
  }
});

test("one bad Rust fingerprint stops after the single bounded retry", {
  timeout: 15_000
}, async () => {
  const workspaceRoot = mkdtempSync(
    path.join(os.tmpdir(), "mework-browser-dev-failed-rebuild-")
  );
  const reservations = await Promise.all([reservePort(), reservePort()]);
  const [frontendReservation, backendReservation] = reservations;
  const origin = `http://127.0.0.1:${frontendReservation.port}`;
  const backendOrigin = `http://127.0.0.1:${backendReservation.port}`;
  const contenders = [];

  try {
    await Promise.all(reservations.map(closeReservation));
    const owner = startContender({
      client: "codex",
      workspaceRoot,
      origin,
      backendOrigin,
      failRebuilds: true
    });
    contenders.push(owner);
    const ownerResult = await owner.result;
    assert.equal(ownerResult.role, "owner");

    mkdirSync(path.join(workspaceRoot, "src-tauri"), { recursive: true });
    writeFileSync(
      path.join(workspaceRoot, "src-tauri", "app_commands.rs"),
      "// persistently bad Rust fixture\n",
      "utf8"
    );
    const follower = startContender({
      client: "claude",
      workspaceRoot,
      origin,
      backendOrigin,
      startupTimeoutMs: 2_000
    });
    contenders.push(follower);
    await assert.rejects(follower.result, /等待共享 browser-dev 启动超时/);
    const followerExit = await follower.exit;
    assert.equal(followerExit.code, 1);

    const state = readWorkspaceLease({
      workspaceRoot,
      directory: resolveLeaseDirectory(workspaceRoot, "browser-dev")
    });
    assert.equal(state.status, "rebuild-failed");
    assert.equal(state.rebuildFailureCount, 2);
    assert.equal(owner.events.length, 2);
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.equal(owner.events.length, 2);

    const stopped = await requestBrowserDevStop({
      workspaceRoot,
      timeoutMs: 10_000
    });
    assert.equal(stopped.ownerPid, ownerResult.ownerPid);
    const ownerExit = await owner.exit;
    assert.deepEqual(ownerExit, { code: 0, signal: null });
  } finally {
    for (const { child } of contenders) {
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
    rmSync(workspaceRoot, { recursive: true, force: true });
  }
});
