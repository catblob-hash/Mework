import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readdirSync,
  renameSync,
  symlinkSync,
  unlinkSync,
  writeFileSync
} from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";

import {
  readWorkspaceLease,
  recoverEmptyWorkspaceLease,
  removeWorkspaceLease,
  resolveLeaseDirectory,
  tryAcquireWorkspaceLease,
  updateWorkspaceLease,
  workspaceMutexListenCandidates,
  workspaceLeaseDirectoryExists
} from "../workspace-coordinator.mjs";
import {
  acceptedBrowserDevRestartFingerprint,
  BROWSER_DEV_COORDINATOR_PROTOCOL,
  BROWSER_DEV_MAX_REBUILD_FAILURES,
  browserDevClientLabel,
  browserDevOwnerFingerprint,
  browserDevRestartRequestKey,
  browserDevSourceFingerprint,
  classifyBrowserDevLease,
  coordinateBrowserDev,
  parseBrowserDevArguments
} from "../browser-dev-coordinator.mjs";
import {
  classifySharedTestLease,
  SHARED_TEST_CACHE_TTL_MS,
  SHARED_TEST_PROTOCOL
} from "../shared-test-coordinator.mjs";

function temporaryWorkspace() {
  return mkdtempSync(path.join(os.tmpdir(), "mework-coordinator-test-"));
}

function activeMutexServerResources() {
  return process.getActiveResourcesInfo()
    .filter((resource) => resource === "PipeWrap").length;
}

function writeWorkspaceFile(workspaceRoot, relativePath, contents) {
  const absolute = path.join(workspaceRoot, relativePath);
  mkdirSync(path.dirname(absolute), { recursive: true });
  writeFileSync(absolute, contents, "utf8");
}

function leaseState(overrides = {}) {
  return {
    protocol: "test-v1",
    sessionId: "owner-session",
    ownerPid: process.pid,
    status: "running",
    ...overrides
  };
}

function leaseToken(sessionId) {
  return createHash("sha256").update(sessionId, "utf8").digest("hex");
}

function removalMarkerName(sessionId, removerPid, removalId) {
  return `.removing-${
    leaseToken(sessionId)
  }-${removerPid}-${removalId}.json`;
}

function removalDirectory(
  directory,
  sessionId,
  removerPid,
  removalId
) {
  return path.join(
    path.dirname(directory),
    `.${path.basename(directory)}.removing-${
      leaseToken(sessionId)
    }-${removerPid}-${removalId}`
  );
}

function startPauseWorker({
  mode,
  workspaceRoot,
  pauseMarker,
  pauseMilliseconds,
  sessionId
}) {
  const child = spawn(process.execPath, [
    path.join(
      process.cwd(),
      "scripts",
      "fixtures",
      "workspace-coordinator-pause-worker.mjs"
    ),
    mode,
    workspaceRoot,
    pauseMarker,
    String(pauseMilliseconds),
    sessionId
  ], {
    cwd: process.cwd(),
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const completion = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({
      code,
      signal,
      stdout,
      stderr
    }));
  });
  return { child, completion };
}

async function waitForPath(target, timeoutMilliseconds = 5_000) {
  const deadline = Date.now() + timeoutMilliseconds;
  while (!existsSync(target)) {
    if (Date.now() >= deadline) {
      throw new Error(`等待 fixture 路径超时: ${target}`);
    }
    await delay(20);
  }
}

function parseWorkerResult(completion) {
  assert.equal(completion.code, 0, completion.stderr || completion.stdout);
  return JSON.parse(completion.stdout.trim());
}

test("forty contenders create exactly one workspace owner", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const winners = await Promise.all(
    Array.from({ length: 40 }, (_, index) => tryAcquireWorkspaceLease({
      workspaceRoot,
      directory,
      state: leaseState({ sessionId: `session-${index}` })
    }))
  );
  assert.equal(winners.filter(Boolean).length, 1);
  assert.equal(readWorkspaceLease({ workspaceRoot, directory }).sessionId, "session-0");
});

test("the OS mutex endpoint is deterministic across workspace aliases", () => {
  const workspaceRoot = temporaryWorkspace();
  mkdirSync(path.join(workspaceRoot, ".codex-tmp"), { recursive: true });
  const aliasParent = temporaryWorkspace();
  const aliasRoot = path.join(aliasParent, "workspace-alias");
  symlinkSync(workspaceRoot, aliasRoot, "junction");
  const directCandidates = workspaceMutexListenCandidates(
    workspaceRoot,
    resolveLeaseDirectory(workspaceRoot, "browser-dev")
  );
  const aliasCandidates = workspaceMutexListenCandidates(
    aliasRoot,
    resolveLeaseDirectory(aliasRoot, "browser-dev")
  );
  assert.deepEqual(aliasCandidates, directCandidates);
  assert.equal(directCandidates.length, 1);
  assert.equal(
    process.platform === "win32"
      ? directCandidates[0].path.startsWith("\\\\.\\pipe\\")
      : directCandidates[0].path.startsWith("\0"),
    true
  );
});

test("a successful mutex close drains its native server handle", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const resourcesBefore = activeMutexServerResources();
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "native-handle-owner" })
  }), true);
  assert.ok(activeMutexServerResources() <= resourcesBefore);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "native-handle-owner"
  }), true);
  assert.ok(activeMutexServerResources() <= resourcesBefore);
});

test("a Windows owner rejects partial browser-dev port occupancy", {
  skip: process.platform !== "win32"
}, async () => {
  const workspaceRoot = temporaryWorkspace();
  const occupiedFrontend = createServer();
  const backendReservation = createServer();
  const listenRandom = (server) => new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.removeListener("error", reject);
      resolve(server.address().port);
    });
  });
  const closeServer = (server) => new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
  const [frontendPort, backendPort] = await Promise.all([
    listenRandom(occupiedFrontend),
    listenRandom(backendReservation)
  ]);
  await closeServer(backendReservation);
  try {
    await assert.rejects(
      coordinateBrowserDev({
        workspaceRoot,
        origin: `http://127.0.0.1:${frontendPort}`,
        backendOrigin: `http://127.0.0.1:${backendPort}`,
        environment: {},
        startupTimeoutMs: 1_000,
        onWait() {}
      }),
      /未纳入协调的旧 browser-dev 已占用/
    );
    assert.equal(
      workspaceLeaseDirectoryExists({
        workspaceRoot,
        directory: resolveLeaseDirectory(workspaceRoot, "browser-dev")
      }),
      false
    );
  } finally {
    await closeServer(occupiedFrontend);
  }
});

test("the OS mutex closes the empty publication ABA window", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const pauseMarker = path.join(workspaceRoot, "acquire-paused.json");
  const worker = startPauseWorker({
    mode: "acquire",
    workspaceRoot,
    pauseMarker,
    pauseMilliseconds: 3_000,
    sessionId: "paused-publisher"
  });
  await waitForPath(pauseMarker);
  assert.deepEqual(readdirSync(directory), []);
  const mutexResourcesBefore = activeMutexServerResources();

  assert.equal(
    await recoverEmptyWorkspaceLease({ workspaceRoot, directory }),
    false
  );
  const blockedContenders = await Promise.all(
    Array.from({ length: 60 }, (_, index) => tryAcquireWorkspaceLease({
      workspaceRoot,
      directory,
      state: leaseState({
        sessionId: `publication-aba-replacement-${index}`
      })
    }))
  );
  assert.equal(blockedContenders.every((result) => result === false), true);
  await delay(20);
  const mutexResourcesAfter = activeMutexServerResources();
  assert.ok(
    mutexResourcesAfter <= mutexResourcesBefore + 2,
    `failed mutex binds leaked server handles: ${mutexResourcesBefore} -> ${
      mutexResourcesAfter
    }`
  );
  let peakSerialMutexResources = mutexResourcesAfter;
  for (let index = 0; index < 1_000; index += 1) {
    assert.equal(await tryAcquireWorkspaceLease({
      workspaceRoot,
      directory,
      state: leaseState({
        sessionId: `serial-publication-aba-replacement-${index}`
      })
    }), false);
    peakSerialMutexResources = Math.max(
      peakSerialMutexResources,
      activeMutexServerResources()
    );
  }
  assert.ok(
    peakSerialMutexResources <= mutexResourcesBefore + 4,
    `serial mutex retries leaked server handles: ${mutexResourcesBefore} -> ${
      peakSerialMutexResources
    }`
  );

  const result = parseWorkerResult(await worker.completion);
  assert.equal(result.paused, true);
  assert.equal(result.result, true);
  assert.equal(
    readWorkspaceLease({ workspaceRoot, directory }).sessionId,
    "paused-publisher"
  );
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "paused-publisher"
  }), true);
});

test("a partial owner write never enters the canonical lease directory", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const originalWriteFileSync = fs.writeFileSync;
  let injected = false;
  fs.writeFileSync = (target, ...args) => {
    if (
      !injected
      && path.basename(String(target)).includes(".owner-candidate-")
    ) {
      injected = true;
      originalWriteFileSync(target, "{\"protocol\":", "utf8");
      const error = new Error("injected partial owner write");
      error.code = "EIO";
      throw error;
    }
    return originalWriteFileSync(target, ...args);
  };
  syncBuiltinESMExports();
  try {
    await assert.rejects(
      tryAcquireWorkspaceLease({
        workspaceRoot,
        directory,
        state: leaseState({ sessionId: "partial-owner-session" })
      }),
      /injected partial owner write/
    );
  } finally {
    fs.writeFileSync = originalWriteFileSync;
    syncBuiltinESMExports();
  }

  assert.equal(injected, true);
  assert.equal(existsSync(directory), false);
  assert.equal(
    readdirSync(path.dirname(directory)).some((name) =>
      name.includes(".owner-candidate-")
    ),
    false
  );
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "owner-after-partial-write" })
  }), true);
  assert.equal(
    readWorkspaceLease({ workspaceRoot, directory }).sessionId,
    "owner-after-partial-write"
  );
});

test("invalid acquisition input cannot strand the OS mutex", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  await assert.rejects(
    tryAcquireWorkspaceLease({
      workspaceRoot,
      directory,
      state: leaseState({ sessionId: "" })
    }),
    /缺少有效 sessionId/
  );
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "valid-after-invalid-input" })
  }), true);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "valid-after-invalid-input"
  }), true);
});

test("the OS releases a publication mutex when its process terminates", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const pauseMarker = path.join(workspaceRoot, "crashed-publisher.json");
  const worker = startPauseWorker({
    mode: "acquire",
    workspaceRoot,
    pauseMarker,
    pauseMilliseconds: 30_000,
    sessionId: "crashed-publisher"
  });
  await waitForPath(pauseMarker);
  assert.deepEqual(readdirSync(directory), []);
  try {
    const timeoutStartedAt = Date.now();
    await assert.rejects(
      coordinateBrowserDev({
        workspaceRoot,
        origin: "http://127.0.0.1:1",
        backendOrigin: "http://127.0.0.1:2",
        environment: {},
        startupTimeoutMs: 100,
        emptyLeaseRecoveryMs: 10,
        sourceFingerprintRefreshMs: 10,
        onWait() {}
      }),
      /等待共享 browser-dev 启动超时/
    );
    assert.ok(
      Date.now() - timeoutStartedAt < 500,
      "missing canonical path bypassed the startup deadline"
    );
  } finally {
    if (
      worker.child.exitCode === null
      && worker.child.signalCode === null
    ) {
      worker.child.kill();
    }
    await worker.completion;
  }

  assert.equal(
    await recoverEmptyWorkspaceLease({ workspaceRoot, directory }),
    true
  );
  assert.equal(existsSync(directory), false);
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "owner-after-crash" })
  }), true);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "owner-after-crash"
  }), true);
});

test("the OS mutex closes the two-stage removal ABA window", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const sessionId = "paused-remover-owner";
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId })
  }), true);
  const pauseMarker = path.join(workspaceRoot, "remove-paused.json");
  const worker = startPauseWorker({
    mode: "remove",
    workspaceRoot,
    pauseMarker,
    pauseMilliseconds: 1_200,
    sessionId
  });
  await waitForPath(pauseMarker);

  assert.equal(
    await recoverEmptyWorkspaceLease({ workspaceRoot, directory }),
    false
  );
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "removal-aba-replacement" })
  }), false);

  const result = parseWorkerResult(await worker.completion);
  assert.equal(result.paused, true);
  assert.equal(result.result, true);
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "safe-replacement" })
  }), true);
  assert.equal(
    readWorkspaceLease({ workspaceRoot, directory }).sessionId,
    "safe-replacement"
  );
});

test("owner release waits while another remover holds a null-state claim", async () => {
  const workspaceRoot = temporaryWorkspace();
  const coordination = await coordinateBrowserDev({
    workspaceRoot,
    origin: "http://127.0.0.1:1",
    backendOrigin: "http://127.0.0.1:2",
    environment: {},
    startupTimeoutMs: 2_000,
    onWait() {}
  });
  assert.equal(coordination.role, "owner");
  const pauseMarker = path.join(workspaceRoot, "competing-remove-paused.json");
  const worker = startPauseWorker({
    mode: "remove",
    workspaceRoot,
    pauseMarker,
    pauseMilliseconds: 600,
    sessionId: coordination.sessionId
  });
  await waitForPath(pauseMarker);

  const releaseStartedAt = Date.now();
  await coordination.release();
  const releaseDuration = Date.now() - releaseStartedAt;
  const result = parseWorkerResult(await worker.completion);
  assert.equal(result.result, true);
  assert.ok(
    releaseDuration >= 400,
    `owner release returned during a competing removal claim (${releaseDuration}ms)`
  );
  assert.equal(
    existsSync(resolveLeaseDirectory(workspaceRoot, "browser-dev")),
    false
  );
});

test("only the exact owner can update or remove a lease", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "verification", "npm-test");
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState()
  }), true);
  assert.equal(updateWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "other-session",
    update: { status: "complete" }
  }), false);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "other-session"
  }), false);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "owner-session"
  }), true);
});

test("state updates use immutable revisions despite Windows sharing violations", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const sessionId = "immutable-state-owner";
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId })
  }), true);
  assert.equal(readWorkspaceLease({ workspaceRoot, directory }).revision, 0);

  const originalRenameSync = fs.renameSync;
  fs.renameSync = function rejectExistingDestination(source, destination) {
    if (existsSync(destination)) {
      const error = new Error("injected Windows replace sharing violation");
      error.code = "EPERM";
      throw error;
    }
    return originalRenameSync.call(this, source, destination);
  };
  syncBuiltinESMExports();
  try {
    assert.equal(updateWorkspaceLease({
      workspaceRoot,
      directory,
      sessionId,
      update: { status: "revision-one" }
    }), true);
  } finally {
    fs.renameSync = originalRenameSync;
    syncBuiltinESMExports();
  }
  assert.equal(readWorkspaceLease({ workspaceRoot, directory }).revision, 1);

  const revisionOneName = readdirSync(directory).find((name) =>
    /^\.state-.+-1-[0-9a-f]{32}\.json$/.test(name)
  );
  assert.ok(revisionOneName);
  const originalUnlinkSync = fs.unlinkSync;
  fs.unlinkSync = function rejectOldGenerationCleanup(target, ...args) {
    if (path.basename(String(target)) === revisionOneName) {
      const error = new Error("injected Windows cleanup sharing violation");
      error.code = "EPERM";
      throw error;
    }
    return originalUnlinkSync.call(this, target, ...args);
  };
  syncBuiltinESMExports();
  try {
    assert.equal(updateWorkspaceLease({
      workspaceRoot,
      directory,
      sessionId,
      update: { status: "revision-two" }
    }), true);
  } finally {
    fs.unlinkSync = originalUnlinkSync;
    syncBuiltinESMExports();
  }
  const latest = readWorkspaceLease({ workspaceRoot, directory });
  assert.equal(latest.revision, 2);
  assert.equal(latest.status, "revision-two");
  assert.equal(
    readdirSync(directory).filter((name) => name.startsWith(".state-")).length,
    2
  );

  assert.equal(updateWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId,
    update: { status: "revision-three" }
  }), true);
  assert.equal(readWorkspaceLease({ workspaceRoot, directory }).revision, 3);
  assert.equal(
    readdirSync(directory).filter((name) => name.startsWith(".state-")).length,
    1
  );
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId
  }), true);
});

test("a stale remover cannot delete a replacement lease generation", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "old-session" })
  }), true);
  const oldMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(oldMarker);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "old-session"
  }), true);
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "replacement-session" })
  }), true);
  const replacementMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(replacementMarker);
  assert.notEqual(replacementMarker, oldMarker);
  assert.equal(await removeWorkspaceLease({
    workspaceRoot,
    directory,
    sessionId: "old-session"
  }), false);
  assert.equal(
    readWorkspaceLease({ workspaceRoot, directory }).sessionId,
    "replacement-session"
  );
});

test("recovers a unique removal claim after its critical section ended", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const sessionId = "dead-finisher-session";
  const previousFinisherPid = process.pid;
  const removalId = "d".repeat(32);
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId })
  }), true);
  const ownerMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(ownerMarker);
  renameSync(
    path.join(directory, ownerMarker),
    path.join(
      directory,
      removalMarkerName(sessionId, previousFinisherPid, removalId)
    )
  );

  assert.equal(
    await recoverEmptyWorkspaceLease({ workspaceRoot, directory }),
    true
  );
  assert.equal(existsSync(directory), false);
});

test("recovers only an ordinary empty or dead unpublished lease directory", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  mkdirSync(directory, { recursive: true });
  assert.equal(workspaceLeaseDirectoryExists({ workspaceRoot, directory }), true);
  assert.equal(await recoverEmptyWorkspaceLease({ workspaceRoot, directory }), true);
  assert.equal(existsSync(directory), false);

  mkdirSync(directory);
  writeWorkspaceFile(workspaceRoot, ".codex-tmp/browser-dev/owner.tmp", "publishing");
  assert.equal(await recoverEmptyWorkspaceLease({ workspaceRoot, directory }), false);
  assert.equal(existsSync(directory), true);

  const abandonedWorkspace = temporaryWorkspace();
  const abandonedDirectory = resolveLeaseDirectory(
    abandonedWorkspace,
    "browser-dev"
  );
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot: abandonedWorkspace,
    directory: abandonedDirectory,
    state: leaseState({
      sessionId: "abandoned-session",
      ownerPid: 2_147_483_647
    })
  }), true);
  const stateFile = readdirSync(abandonedDirectory).find((name) =>
    name.startsWith(".state-") && name.endsWith(".json")
  );
  assert.ok(stateFile);
  unlinkSync(path.join(abandonedDirectory, stateFile));
  assert.equal(readWorkspaceLease({
    workspaceRoot: abandonedWorkspace,
    directory: abandonedDirectory
  }), null);
  assert.equal(await recoverEmptyWorkspaceLease({
    workspaceRoot: abandonedWorkspace,
    directory: abandonedDirectory
  }), true);
  assert.equal(existsSync(abandonedDirectory), false);
});

test("recovers a durable removal tombstone left after a deleter crash", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const sessionId = "removal-crash-session";
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId })
  }), true);
  const ownerMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(ownerMarker);
  const removerPid = 2_147_483_647;
  const removalId = "a".repeat(32);
  const removalMarker = removalMarkerName(
    sessionId,
    removerPid,
    removalId
  );
  renameSync(
    path.join(directory, ownerMarker),
    path.join(directory, removalMarker)
  );
  const claimedDirectory = removalDirectory(
    directory,
    sessionId,
    removerPid,
    removalId
  );
  renameSync(directory, claimedDirectory);

  // Simulate a real partial recursive delete: the internal removal marker was
  // already unlinked before Windows rejected another occupied entry. Recovery
  // must rely on the durable sibling directory name, not on directory contents.
  unlinkSync(path.join(claimedDirectory, removalMarker));
  assert.ok(readdirSync(claimedDirectory).some((name) =>
    name.startsWith(".state-")
  ));
  assert.equal(readWorkspaceLease({ workspaceRoot, directory }), null);
  assert.equal(await recoverEmptyWorkspaceLease({ workspaceRoot, directory }), true);
  assert.equal(existsSync(directory), false);
  assert.equal(existsSync(claimedDirectory), false);
});

test("recovers an exact-owner removal claim left before the directory move", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const sessionId = "pre-move-crash-session";
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId })
  }), true);
  const ownerMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(ownerMarker);
  const removerPid = 2_147_483_647;
  const removalId = "b".repeat(32);
  renameSync(
    path.join(directory, ownerMarker),
    path.join(
      directory,
      removalMarkerName(sessionId, removerPid, removalId)
    )
  );

  assert.equal(await recoverEmptyWorkspaceLease({ workspaceRoot, directory }), true);
  assert.equal(existsSync(directory), false);
  assert.equal(
    existsSync(removalDirectory(
      directory,
      sessionId,
      removerPid,
      removalId
    )),
    false
  );
});

test("a partially deleted tombstone cannot damage a replacement lease", async () => {
  const workspaceRoot = temporaryWorkspace();
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  const oldSessionId = "partial-delete-old-session";
  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: oldSessionId })
  }), true);
  const ownerMarker = readdirSync(directory).find((name) =>
    name.startsWith(".owner-")
  );
  assert.ok(ownerMarker);
  const removerPid = 2_147_483_647;
  const removalId = "c".repeat(32);
  const removalMarker = removalMarkerName(
    oldSessionId,
    removerPid,
    removalId
  );
  renameSync(
    path.join(directory, ownerMarker),
    path.join(directory, removalMarker)
  );
  const claimedDirectory = removalDirectory(
    directory,
    oldSessionId,
    removerPid,
    removalId
  );
  renameSync(directory, claimedDirectory);
  unlinkSync(path.join(claimedDirectory, removalMarker));

  assert.equal(await tryAcquireWorkspaceLease({
    workspaceRoot,
    directory,
    state: leaseState({ sessionId: "replacement-after-partial-delete" })
  }), true);
  assert.equal(await recoverEmptyWorkspaceLease({ workspaceRoot, directory }), true);
  assert.equal(existsSync(claimedDirectory), false);
  assert.equal(
    readWorkspaceLease({ workspaceRoot, directory }).sessionId,
    "replacement-after-partial-delete"
  );
});

test("refuses symlinked and out-of-workspace coordination paths", async () => {
  const workspaceRoot = temporaryWorkspace();
  const outside = temporaryWorkspace();
  assert.throws(
    () => resolveLeaseDirectory(workspaceRoot, "..", "outside"),
    /必须位于工作区/
  );
  const directory = resolveLeaseDirectory(workspaceRoot, "browser-dev");
  mkdirSync(path.dirname(directory), { recursive: true });
  symlinkSync(outside, directory, "junction");
  assert.throws(
    () => readWorkspaceLease({ workspaceRoot, directory }),
    /不是普通目录/
  );
  await assert.rejects(
    () => recoverEmptyWorkspaceLease({ workspaceRoot, directory }),
    /不是普通目录/
  );
  assert.throws(
    () => workspaceLeaseDirectoryExists({
      workspaceRoot,
      directory: outside
    }),
    /必须位于工作区/
  );

  const linkedRootWorkspace = temporaryWorkspace();
  symlinkSync(
    outside,
    path.join(linkedRootWorkspace, ".codex-tmp"),
    "junction"
  );
  await assert.rejects(
    () => tryAcquireWorkspaceLease({
      workspaceRoot: linkedRootWorkspace,
      directory: resolveLeaseDirectory(linkedRootWorkspace, "browser-dev"),
      state: leaseState()
    }),
    /祖先不是普通目录/
  );

  const linkedParentWorkspace = temporaryWorkspace();
  mkdirSync(path.join(linkedParentWorkspace, ".codex-tmp"));
  symlinkSync(
    outside,
    path.join(linkedParentWorkspace, ".codex-tmp", "verification"),
    "junction"
  );
  await assert.rejects(
    () => tryAcquireWorkspaceLease({
      workspaceRoot: linkedParentWorkspace,
      directory: resolveLeaseDirectory(
        linkedParentWorkspace,
        "verification",
        "npm-test"
      ),
      state: leaseState()
    }),
    /祖先不是普通目录/
  );
});

test("browser-dev fingerprint covers Cargo, build-script, and served resource inputs", () => {
  const workspaceRoot = temporaryWorkspace();
  const watchedInputs = [
    ".cargo/config.toml",
    "src/mework-icon.svg",
    "src-tauri/app_commands.rs",
    "src-tauri/resources/image-input-browser-e2e.html"
  ];
  for (const relativePath of watchedInputs) {
    writeWorkspaceFile(workspaceRoot, relativePath, `initial:${relativePath}`);
  }
  const baseline = browserDevSourceFingerprint(workspaceRoot);
  for (const relativePath of watchedInputs) {
    writeWorkspaceFile(workspaceRoot, relativePath, `changed:${relativePath}`);
    assert.notEqual(
      browserDevSourceFingerprint(workspaceRoot),
      baseline,
      `${relativePath} must affect the browser-dev fingerprint`
    );
    writeWorkspaceFile(workspaceRoot, relativePath, `initial:${relativePath}`);
    assert.equal(browserDevSourceFingerprint(workspaceRoot), baseline);
  }
});

test("browser-dev owner fingerprint separately covers long-lived Node and Vite inputs", () => {
  const workspaceRoot = temporaryWorkspace();
  const watchedInputs = [
    "package.json",
    "package-lock.json",
    "scripts/browser-dev-coordinator.mjs",
    "scripts/browser-dev.mjs",
    "scripts/vite-csp.mjs",
    "scripts/workspace-coordinator.mjs",
    "vite.config.ts"
  ];
  for (const relativePath of watchedInputs) {
    writeWorkspaceFile(workspaceRoot, relativePath, `initial:${relativePath}`);
  }
  const baseline = browserDevOwnerFingerprint(workspaceRoot);
  for (const relativePath of watchedInputs) {
    writeWorkspaceFile(workspaceRoot, relativePath, `changed:${relativePath}`);
    assert.notEqual(
      browserDevOwnerFingerprint(workspaceRoot),
      baseline,
      `${relativePath} must affect the browser-dev owner fingerprint`
    );
    writeWorkspaceFile(workspaceRoot, relativePath, `initial:${relativePath}`);
    assert.equal(browserDevOwnerFingerprint(workspaceRoot), baseline);
  }
  writeWorkspaceFile(
    workspaceRoot,
    "src-tauri/app_commands.rs",
    "Rust-only change"
  );
  assert.equal(browserDevOwnerFingerprint(workspaceRoot), baseline);
});

test("ready publishes the Rust bytes captured at spawn, not later workspace edits", async () => {
  const workspaceRoot = temporaryWorkspace();
  writeWorkspaceFile(
    workspaceRoot,
    "src-tauri/app_commands.rs",
    "// build fingerprint A\n"
  );
  const coordination = await coordinateBrowserDev({
    workspaceRoot,
    origin: "http://127.0.0.1:1",
    backendOrigin: "http://127.0.0.1:2",
    environment: {},
    startupTimeoutMs: 2_000,
    onWait() {}
  });
  try {
    assert.equal(coordination.role, "owner");
    const builtFingerprint = coordination.sourceFingerprint;
    writeWorkspaceFile(
      workspaceRoot,
      "src-tauri/app_commands.rs",
      "// changed to fingerprint B while Cargo was running\n"
    );
    const changedFingerprint = browserDevSourceFingerprint(workspaceRoot);
    assert.notEqual(changedFingerprint, builtFingerprint);
    coordination.markReady(builtFingerprint);
    const state = readWorkspaceLease({
      workspaceRoot,
      directory: coordination.directory
    });
    assert.equal(state.status, "ready");
    assert.equal(state.sourceFingerprint, builtFingerprint);
    assert.notEqual(state.sourceFingerprint, changedFingerprint);
  } finally {
    await coordination.release();
  }
});

test("browser-dev followers reject stale and mismatched owners before probing", () => {
  const state = {
    protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
    sessionId: "session",
    ownerPid: process.pid,
    ownerFingerprint: "owner-a",
    sourceFingerprint: "a",
    rebuildFailureCount: 0,
    status: "ready"
  };
  assert.equal(classifyBrowserDevLease({
    state,
    ownerFingerprint: "owner-a",
    sourceFingerprint: "a",
    ownerAlive: true
  }), "probe");
  assert.equal(classifyBrowserDevLease({
    state,
    ownerFingerprint: "owner-a",
    sourceFingerprint: "b",
    ownerAlive: true
  }), "source-mismatch");
  assert.equal(classifyBrowserDevLease({
    state,
    ownerFingerprint: "owner-b",
    sourceFingerprint: "a",
    ownerAlive: true
  }), "owner-mismatch");
  assert.equal(classifyBrowserDevLease({
    state,
    ownerFingerprint: "owner-a",
    sourceFingerprint: "a",
    ownerAlive: false
  }), "stale");
  assert.equal(classifyBrowserDevLease({
    state: { ...state, status: "stopping" },
    ownerFingerprint: "owner-b",
    sourceFingerprint: "a",
    ownerAlive: true
  }), "wait-for-stop");
  assert.equal(classifyBrowserDevLease({
    state: { ...state, status: "rebuilding" },
    ownerFingerprint: "owner-a",
    sourceFingerprint: "a",
    ownerAlive: true
  }), "wait-for-ready");
  assert.equal(classifyBrowserDevLease({
    state: { ...state, status: "rebuild-failed" },
    ownerFingerprint: "owner-a",
    sourceFingerprint: "a",
    ownerAlive: true
  }), "rebuild-failed");
  assert.equal(classifyBrowserDevLease({
    state: { ...state, status: "rebuild-failed" },
    ownerFingerprint: "owner-a",
    sourceFingerprint: "new-source",
    ownerAlive: true
  }), "source-mismatch");
});

test("browser-dev restart requests converge and stop after one bounded retry", () => {
  const ready = {
    protocol: BROWSER_DEV_COORDINATOR_PROTOCOL,
    sessionId: "session",
    ownerPid: process.pid,
    ownerFingerprint: "owner",
    sourceFingerprint: "current",
    rebuildFailureCount: 0,
    status: "ready",
    updatedAt: "2026-07-28T00:00:00.000Z"
  };
  const mismatchKey = browserDevRestartRequestKey({
    classification: "source-mismatch",
    state: ready,
    sourceFingerprint: "next"
  });
  assert.equal(mismatchKey, "mismatch\0session\0next");

  const failedKey = browserDevRestartRequestKey({
    classification: "rebuild-failed",
    state: {
      ...ready,
      status: "rebuild-failed",
      rebuildFailureCount: 1
    },
    sourceFingerprint: "current"
  });
  assert.equal(
    failedKey,
    ["rebuild-failed", "session", "current", "1"].join("\0")
  );
  assert.equal(browserDevRestartRequestKey({
    classification: "rebuild-failed",
    state: {
      ...ready,
      status: "rebuild-failed",
      rebuildFailureCount: BROWSER_DEV_MAX_REBUILD_FAILURES
    },
    sourceFingerprint: "current"
  }), null);
  assert.equal(browserDevRestartRequestKey({
    classification: "rebuild-failed",
    state: {
      ...ready,
      status: "rebuild-failed",
      rebuildFailureCount: 1,
      updatedAt: "2026-07-28T00:00:01.000Z"
    },
    sourceFingerprint: "current"
  }), failedKey);
  assert.equal(browserDevRestartRequestKey({
    classification: "probe",
    state: ready,
    sourceFingerprint: "current"
  }), null);

  assert.equal(acceptedBrowserDevRestartFingerprint({
    state: ready,
    currentSourceFingerprint: "current"
  }), null);
  assert.equal(acceptedBrowserDevRestartFingerprint({
    state: { ...ready, status: "rebuilding" },
    currentSourceFingerprint: "current"
  }), null);
  assert.equal(acceptedBrowserDevRestartFingerprint({
    state: {
      ...ready,
      status: "rebuild-failed",
      rebuildFailureCount: 1
    },
    currentSourceFingerprint: "current",
    request: {
      observedStatus: "rebuild-failed",
      observedSourceFingerprint: "current",
      observedRebuildFailureCount: 1
    }
  }), "current");
  assert.equal(acceptedBrowserDevRestartFingerprint({
    state: {
      ...ready,
      status: "rebuild-failed",
      rebuildFailureCount: BROWSER_DEV_MAX_REBUILD_FAILURES
    },
    currentSourceFingerprint: "current",
    request: {
      observedStatus: "rebuild-failed",
      observedSourceFingerprint: "current",
      observedRebuildFailureCount: 1
    }
  }), null);
  assert.equal(acceptedBrowserDevRestartFingerprint({
    state: ready,
    currentSourceFingerprint: "new-current",
    request: {
      observedStatus: "ready",
      observedSourceFingerprint: "current",
      observedRebuildFailureCount: 0
    }
  }), "new-current");
});

test("browser-dev recognizes each client mode without changing E2E defaults", () => {
  assert.deepEqual(parseBrowserDevArguments([]), {
    client: null,
    stopOnly: false
  });
  assert.deepEqual(parseBrowserDevArguments(["--codex"]), {
    client: "codex",
    stopOnly: false
  });
  assert.deepEqual(parseBrowserDevArguments(["--claude"]), {
    client: "claude",
    stopOnly: false
  });
  assert.deepEqual(parseBrowserDevArguments(["--stop"]), {
    client: null,
    stopOnly: true
  });
  assert.equal(browserDevClientLabel("codex"), "Codex");
  assert.equal(browserDevClientLabel("claude"), "Claude Code");
  assert.equal(browserDevClientLabel(null), null);
  assert.throws(
    () => parseBrowserDevArguments(["--codex", "--stop"]),
    /不能同时使用/
  );
  assert.throws(
    () => parseBrowserDevArguments(["--claude", "--stop"]),
    /不能同时使用/
  );
  assert.throws(
    () => parseBrowserDevArguments(["--codex", "--claude"]),
    /不能同时使用/
  );
  assert.throws(
    () => parseBrowserDevArguments(["--codex", "--codex"]),
    /不能重复/
  );
  assert.throws(
    () => parseBrowserDevArguments(["--claude", "--claude"]),
    /不能重复/
  );
  assert.throws(
    () => parseBrowserDevArguments(["--browser"]),
    /不支持的 browser-dev 参数/
  );
});

test("shared tests wait for one owner and reuse a fresh matching result", () => {
  const running = {
    protocol: SHARED_TEST_PROTOCOL,
    sessionId: "session",
    ownerPid: process.pid,
    fingerprint: "same",
    status: "running"
  };
  assert.equal(classifySharedTestLease({
    state: running,
    fingerprint: "same",
    ownerAlive: true
  }), "wait");
  assert.equal(classifySharedTestLease({
    state: running,
    fingerprint: "same",
    ownerAlive: false
  }), "stale");
  const completedAt = new Date().toISOString();
  assert.equal(classifySharedTestLease({
    state: {
      ...running,
      status: "complete",
      completedAt,
      result: { exitCode: 0 }
    },
    fingerprint: "same",
    ownerAlive: false
  }), "reuse");
  assert.equal(classifySharedTestLease({
    state: {
      ...running,
      status: "complete",
      completedAt,
      result: { exitCode: 0 }
    },
    fingerprint: "changed",
    ownerAlive: false
  }), "replace");
});

test("shared tests never reuse a cached failure", () => {
  const completed = {
    protocol: SHARED_TEST_PROTOCOL,
    sessionId: "session",
    ownerPid: process.pid,
    fingerprint: "same",
    status: "complete",
    completedAt: new Date().toISOString()
  };
  const classify = (result, overrides = {}) => classifySharedTestLease({
    state: { ...completed, result },
    fingerprint: "same",
    ownerAlive: false,
    ...overrides
  });
  // 成功结果仍然复用，10 分钟 TTL 与强制重跑的既有语义不变。
  assert.equal(classify({ exitCode: 0 }), "reuse");
  assert.equal(classify({ exitCode: 0, signal: null }), "reuse");
  assert.equal(classify({ exitCode: 0 }, { forceRun: true }), "replace");
  assert.equal(
    classify(
      { exitCode: 0 },
      { now: Date.parse(completed.completedAt) + SHARED_TEST_CACHE_TTL_MS + 1 }
    ),
    "replace"
  );
  // 失败、被信号中断、启动异常、结果缺失或格式异常一律重跑，好让诊断重新打印。
  for (const [label, result] of [
    ["非零退出码", { exitCode: 1 }],
    ["其他非零退出码", { exitCode: 2 }],
    ["被信号中断", { exitCode: 0, signal: "SIGINT" }],
    ["启动异常", { exitCode: 1, signal: null, error: "spawn ENOENT" }],
    ["成功码但带 error", { exitCode: 0, signal: null, error: "spawn ENOENT" }],
    ["结果缺失", undefined],
    ["结果为 null", null],
    ["退出码不是整数", { exitCode: "0" }],
    ["退出码缺失", {}],
    ["结果不是对象", "ok"],
    ["结果是数组", [{ exitCode: 0 }]]
  ]) {
    assert.equal(classify(result), "replace", `${label} 不应复用`);
  }
  // 单飞互斥不受影响：活着的 owner 仍然等待，死掉的 owner 仍然判 stale。
  const running = { ...completed, status: "running" };
  assert.equal(
    classifySharedTestLease({ state: running, fingerprint: "same", ownerAlive: true }),
    "wait"
  );
  assert.equal(
    classifySharedTestLease({ state: running, fingerprint: "same", ownerAlive: false }),
    "stale"
  );
});
