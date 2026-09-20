import { createHash, randomBytes } from "node:crypto";
import {
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  unlinkSync,
  writeFileSync
} from "node:fs";
import { createServer } from "node:net";
import path from "node:path";

export const WORKSPACE_COORDINATOR_PROTOCOL = "mework-workspace-coordinator-v2";

function coordinationRoot(workspaceRoot) {
  return path.resolve(workspaceRoot, ".codex-tmp");
}

export function resolveLeaseDirectory(workspaceRoot, ...segments) {
  const base = coordinationRoot(workspaceRoot);
  const directory = path.resolve(base, ...segments);
  const relative = path.relative(base, directory);
  if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error("协调目录必须位于工作区 .codex-tmp 的直接后代中");
  }
  return directory;
}

function assertManagedLeaseDirectory(workspaceRoot, directory) {
  const base = coordinationRoot(workspaceRoot);
  const expected = resolveLeaseDirectory(
    workspaceRoot,
    path.relative(base, path.resolve(directory))
  );
  if (expected !== path.resolve(directory)) {
    throw new Error("拒绝操作工作区之外的协调目录");
  }
  const parentRelative = path.relative(base, path.dirname(expected));
  let ancestor = base;
  for (const segment of parentRelative ? parentRelative.split(path.sep) : []) {
    assertOrdinaryDirectoryIfPresent(ancestor);
    ancestor = path.join(ancestor, segment);
  }
  assertOrdinaryDirectoryIfPresent(ancestor);
  return expected;
}

function assertOrdinaryDirectoryIfPresent(directory) {
  let metadata;
  try {
    metadata = lstatSync(directory);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("协调路径祖先不是普通目录");
  }
}

export function workspaceMutexListenCandidates(workspaceRoot, directory) {
  const normalizedRoot = path.resolve(workspaceRoot);
  const normalizedDirectory = path.resolve(directory);
  const physicalRoot = realpathSync.native(normalizedRoot);
  const physicalDirectory = path.resolve(
    physicalRoot,
    path.relative(normalizedRoot, normalizedDirectory)
  );
  const identity = process.platform === "win32"
    ? `${physicalRoot}\0${physicalDirectory}`.toLowerCase()
    : `${physicalRoot}\0${physicalDirectory}`;
  const token = createHash("sha256").update(identity, "utf8").digest("hex");
  if (process.platform === "win32") {
    return [{
      path: `\\\\.\\pipe\\mework-workspace-coordinator-${token}`,
      exclusive: true
    }];
  }
  if (process.platform === "linux") {
    return [{
      path: `\0mework-workspace-coordinator-${token}`,
      exclusive: true
    }];
  }
  throw new Error("当前平台不支持 Mework 工作区协调 mutex");
}

async function tryListenWorkspaceMutex(listenOptions) {
  const server = createServer((socket) => {
    socket.destroy();
  });
  return await new Promise((resolve, reject) => {
    const onError = (error) => {
      const errorCode = error?.code ?? "unknown";
      if (errorCode === "EADDRINUSE" || errorCode === "EACCES") {
        // A failed Windows pipe bind still owns a libuv server handle until it
        // is explicitly closed. Release it before returning to the caller.
        server.close();
        setImmediate(() => {
          setImmediate(() => {
            resolve({ errorCode, mutex: null });
          });
        });
        return;
      }
      server.close();
      setImmediate(() => setImmediate(() => reject(error)));
    };
    server.once("error", onError);
    server.listen(listenOptions, () => {
      server.removeListener("error", onError);
      resolve({ errorCode: null, mutex: { server } });
    });
  });
}

async function tryAcquireWorkspaceMutex(workspaceRoot, directory) {
  const keepAlive = setInterval(() => {}, 1_000);
  let keepAliveTransferred = false;
  try {
    const candidates = workspaceMutexListenCandidates(workspaceRoot, directory);
    for (let index = 0; index < candidates.length; index += 1) {
      const attempt = await tryListenWorkspaceMutex(candidates[index]);
      if (attempt.mutex) {
        keepAliveTransferred = true;
        return { ...attempt.mutex, keepAlive };
      }
      if (attempt.errorCode === "EADDRINUSE") return null;
      throw new Error(
        `无法绑定工作区协调 mutex：${attempt.errorCode ?? "unknown"}`
      );
    }
    throw new Error("没有可用的工作区协调 mutex 端点");
  } finally {
    if (!keepAliveTransferred) clearInterval(keepAlive);
  }
}

async function releaseWorkspaceMutex(mutex) {
  try {
    await new Promise((resolve, reject) => {
      mutex.server.close((error) => {
        if (error) reject(error);
        else resolve();
      });
    });
    // On Windows, libuv can still report a PipeWrap/TCPServerWrap during the
    // close callback and the following check phase. Two turns are required
    // before the native handle is actually gone.
    await new Promise((resolve) => {
      setImmediate(() => {
        setImmediate(resolve);
      });
    });
  } finally {
    clearInterval(mutex.keepAlive);
  }
}

function sessionFileToken(sessionId) {
  if (typeof sessionId !== "string" || sessionId.length === 0) {
    throw new Error("协调租约缺少有效 sessionId");
  }
  return createHash("sha256").update(sessionId, "utf8").digest("hex");
}

function ownerMarkerPath(directory, sessionId) {
  return path.join(directory, `.owner-${sessionFileToken(sessionId)}.json`);
}

function ownerCandidatePath(directory, sessionId) {
  return path.join(
    path.dirname(directory),
    `.${path.basename(directory)}.owner-candidate-${
      sessionFileToken(sessionId)
    }-${process.pid}-${randomBytes(16).toString("hex")}.json`
  );
}

function removalMarkerPath(
  directory,
  sessionId,
  removerPid,
  removalId
) {
  if (!Number.isSafeInteger(removerPid) || removerPid <= 0) {
    throw new Error("协调删除认领缺少有效 removerPid");
  }
  if (
    typeof removalId !== "string"
    || !/^[0-9a-f]{32}$/.test(removalId)
  ) {
    throw new Error("协调删除认领缺少有效 removalId");
  }
  return path.join(
    directory,
    `.removing-${
      sessionFileToken(sessionId)
    }-${removerPid}-${removalId}.json`
  );
}

function removalDirectoryPath(directory, claim) {
  return path.join(
    path.dirname(directory),
    `.${path.basename(directory)}.removing-${
      sessionFileToken(claim.sessionId)
    }-${claim.removerPid}-${claim.removalId}`
  );
}

function legacyStatePath(directory, sessionId) {
  return path.join(directory, `.state-${sessionFileToken(sessionId)}.json`);
}

function stateFileExpression(sessionId) {
  return new RegExp(
    `^\\.state-${sessionFileToken(sessionId)}-([0-9]+)-[0-9a-f]{32}\\.json$`
  );
}

function stateGenerationPath(directory, sessionId, revision, generation) {
  if (!Number.isSafeInteger(revision) || revision < 0) {
    throw new Error("协调状态缺少有效 revision");
  }
  if (typeof generation !== "string" || !/^[0-9a-f]{32}$/.test(generation)) {
    throw new Error("协调状态缺少有效 generation");
  }
  return path.join(
    directory,
    `.state-${
      sessionFileToken(sessionId)
    }-${revision}-${generation}.json`
  );
}

function readOwnerMarker(directory) {
  const expression = /^\.owner-[0-9a-f]{64}\.json$/;
  let names;
  try {
    names = readdirSync(directory).filter((name) => expression.test(name));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  if (names.length !== 1) return null;
  try {
    const marker = JSON.parse(
      readFileSync(path.join(directory, names[0]), "utf8")
    );
    if (
      typeof marker?.sessionId !== "string"
      || names[0] !== path.basename(ownerMarkerPath(directory, marker.sessionId))
    ) {
      return null;
    }
    return marker;
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

function readRemovalMarker(directory) {
  const expression = /^\.removing-([0-9a-f]{64})-([1-9][0-9]*)-([0-9a-f]{32})\.json$/;
  let names;
  try {
    names = readdirSync(directory).filter((name) => expression.test(name));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  if (names.length !== 1) return null;
  const match = expression.exec(names[0]);
  if (!match) return null;
  const removerPid = Number(match[2]);
  if (!Number.isSafeInteger(removerPid) || removerPid <= 0) return null;
  try {
    const marker = JSON.parse(
      readFileSync(path.join(directory, names[0]), "utf8")
    );
    const removalId = match[3];
    if (
      typeof marker?.sessionId !== "string"
      || match[1] !== sessionFileToken(marker.sessionId)
      || names[0] !== path.basename(removalMarkerPath(
        directory,
        marker.sessionId,
        removerPid,
        removalId
      ))
    ) {
      return null;
    }
    return {
      ...marker,
      removerPid,
      removalId,
      markerPath: path.join(directory, names[0])
    };
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

function newRemovalClaim(directory, sessionId) {
  const removerPid = process.pid;
  const removalId = randomBytes(16).toString("hex");
  return {
    sessionId,
    removerPid,
    removalId,
    markerPath: removalMarkerPath(
      directory,
      sessionId,
      removerPid,
      removalId
    )
  };
}

function takeOverRemovalClaim(directory, previousClaim) {
  const claim = newRemovalClaim(directory, previousClaim.sessionId);
  try {
    // The old path includes the previous finisher's unique random generation.
    // It is never recreated, so exactly one recovery process can rename it to
    // its own live identity without a fixed-path compare/delete ABA.
    renameSync(previousClaim.markerPath, claim.markerPath);
    return claim;
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function removalDirectoryExpression(directory) {
  const escapedName = path.basename(directory).replace(
    /[.*+?^${}()|[\]\\]/g,
    "\\$&"
  );
  return new RegExp(
    `^\\.${escapedName}\\.removing-[0-9a-f]{64}-[1-9][0-9]*-[0-9a-f]{32}$`
  );
}

function claimedWorkspaceLeaseDirectories(directory) {
  const parent = path.dirname(directory);
  const expression = removalDirectoryExpression(directory);
  let names;
  try {
    names = readdirSync(parent);
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
  return names
    .filter((name) => expression.test(name))
    .map((name) => path.join(parent, name));
}

function removeClaimedWorkspaceLease(claimedDirectory) {
  let metadata;
  try {
    metadata = lstatSync(claimedDirectory);
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("协调删除认领路径不是普通目录");
  }
  rmSync(claimedDirectory, {
    recursive: true,
    force: true,
    maxRetries: 3,
    retryDelay: 100
  });
  return true;
}

function tryRemoveClaimedWorkspaceLease(claimedDirectory) {
  try {
    return removeClaimedWorkspaceLease(claimedDirectory);
  } catch (error) {
    if (
      error?.code === "EACCES"
      || error?.code === "EBUSY"
      || error?.code === "ENOTEMPTY"
      || error?.code === "EPERM"
    ) {
      // The sibling directory name remains the durable claim. It no longer
      // blocks acquisition of the canonical lease path, and a later recovery
      // pass can finish cleanup after the Windows sharing violation clears.
      return false;
    }
    throw error;
  }
}

function finishClaimedWorkspaceLease(directory, claim) {
  const currentClaim = readRemovalMarker(directory);
  if (
    !currentClaim
    || currentClaim.markerPath !== claim.markerPath
    || currentClaim.removerPid !== process.pid
  ) {
    return false;
  }
  const claimedDirectory = removalDirectoryPath(directory, claim);
  let moved = false;
  try {
    // From this point onward the finisher touches only its unique sibling path,
    // never the reusable canonical path. A replacement can acquire immediately
    // after this atomic move without being visible to a delayed cleanup.
    renameSync(directory, claimedDirectory);
    moved = true;
  } catch (error) {
    if (error?.code === "ENOENT") {
      // A previous invocation may already have completed the unique move.
    } else if (
      error?.code === "EACCES"
      || error?.code === "EBUSY"
      || error?.code === "ENOTEMPTY"
      || error?.code === "EPERM"
    ) {
      // Do not leave a live-PID removal marker after this critical section
      // returns. Roll back the exact claim so a dead owner is recoverable after
      // this process exits, even if Windows temporarily refused the directory move.
      try {
        renameSync(
          claim.markerPath,
          ownerMarkerPath(directory, claim.sessionId)
        );
      } catch (rollbackError) {
        if (rollbackError?.code !== "ENOENT") throw rollbackError;
      }
      return false;
    } else {
      throw error;
    }
  }
  return tryRemoveClaimedWorkspaceLease(claimedDirectory) || moved;
}

function claimAndRemoveWorkspaceLease(directory, sessionId) {
  const claim = newRemovalClaim(directory, sessionId);
  try {
    // One rename both consumes the exact owner generation and publishes the
    // live finisher PID/random generation in the destination filename.
    renameSync(
      ownerMarkerPath(directory, sessionId),
      claim.markerPath
    );
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  return finishClaimedWorkspaceLease(directory, claim);
}

function writeStateFile(directory, state) {
  const token = sessionFileToken(state.sessionId);
  const revision = Number.isSafeInteger(state.revision) && state.revision >= 0
    ? state.revision
    : 0;
  const generation = randomBytes(16).toString("hex");
  const target = stateGenerationPath(
    directory,
    state.sessionId,
    revision,
    generation
  );
  const temporary = path.join(
    directory,
    `.state-${token}-${process.pid}-${randomBytes(16).toString("hex")}.tmp`
  );
  let published = false;
  try {
    writeFileSync(temporary, `${JSON.stringify({
      ...state,
      revision
    }, null, 2)}\n`, {
      encoding: "utf8",
      flag: "wx",
      mode: 0o600
    });
    renameSync(temporary, target);
    published = true;
  } finally {
    if (!published) {
      try {
        unlinkSync(temporary);
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
    }
  }

  // State generations are immutable. Readers select the highest complete
  // revision, so Windows sharing violations during old-generation cleanup
  // cannot make publication fail or expose partial JSON.
  const currentName = path.basename(target);
  const expression = stateFileExpression(state.sessionId);
  const legacyName = path.basename(legacyStatePath(directory, state.sessionId));
  for (const name of readdirSync(directory)) {
    if (
      name === currentName
      || (name !== legacyName && !expression.test(name))
    ) {
      continue;
    }
    try {
      unlinkSync(path.join(directory, name));
    } catch (error) {
      if (
        error?.code !== "ENOENT"
        && error?.code !== "EACCES"
        && error?.code !== "EBUSY"
        && error?.code !== "EPERM"
      ) {
        throw error;
      }
    }
  }
}

function readStateForOwner(directory, marker) {
  const expression = stateFileExpression(marker.sessionId);
  const legacyName = path.basename(legacyStatePath(
    directory,
    marker.sessionId
  ));
  const candidates = [];
  for (const name of readdirSync(directory)) {
    const match = expression.exec(name);
    if (match) {
      const revision = Number(match[1]);
      if (Number.isSafeInteger(revision)) {
        candidates.push({ name, revision });
      }
    } else if (name === legacyName) {
      candidates.push({ name, revision: -1 });
    }
  }
  candidates.sort(
    (left, right) =>
      right.revision - left.revision
      || right.name.localeCompare(left.name)
  );
  for (const candidate of candidates) {
    try {
      const state = JSON.parse(
        readFileSync(path.join(directory, candidate.name), "utf8")
      );
      if (
        state?.sessionId === marker.sessionId
        && state.ownerPid === marker.ownerPid
        && (
          candidate.revision < 0
          || state.revision === candidate.revision
        )
      ) {
        return state;
      }
    } catch (error) {
      if (error?.code !== "ENOENT" && !(error instanceof SyntaxError)) {
        throw error;
      }
    }
  }
  return null;
}

export async function tryAcquireWorkspaceLease({
  workspaceRoot,
  directory,
  state
}) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  const candidate = ownerCandidatePath(managedDirectory, state.sessionId);
  mkdirSync(path.dirname(managedDirectory), { recursive: true });
  const mutex = await tryAcquireWorkspaceMutex(workspaceRoot, managedDirectory);
  if (!mutex) return false;
  try {
    // Publish the complete owner record outside the canonical directory first.
    // A hard kill during this write leaves only an inert unique sibling, never
    // an unparseable file that blocks the lease pathname.
    writeFileSync(
      candidate,
      `${JSON.stringify({
        protocol: WORKSPACE_COORDINATOR_PROTOCOL,
        sessionId: state.sessionId,
        ownerPid: state.ownerPid
      }, null, 2)}\n`,
      {
        encoding: "utf8",
        flag: "wx",
        mode: 0o600
      }
    );
    try {
      mkdirSync(managedDirectory);
    } catch (error) {
      if (error?.code === "EEXIST") return false;
      throw error;
    }
    try {
      renameSync(
        candidate,
        ownerMarkerPath(managedDirectory, state.sessionId)
      );
      writeStateFile(managedDirectory, state);
      return true;
    } catch (error) {
      const marker = readOwnerMarker(managedDirectory);
      if (
        !marker
        || marker.sessionId !== state.sessionId
        || !claimAndRemoveWorkspaceLease(managedDirectory, state.sessionId)
      ) {
        try {
          rmdirSync(managedDirectory);
        } catch (cleanupError) {
          if (
            cleanupError?.code !== "ENOENT"
            && cleanupError?.code !== "ENOTEMPTY"
          ) {
            throw cleanupError;
          }
        }
      }
      throw error;
    }
  } finally {
    try {
      unlinkSync(candidate);
    } catch {
      // The unique sibling is inert. Candidate cleanup must never hide the
      // operation result or skip release of the kernel mutex.
    } finally {
      await releaseWorkspaceMutex(mutex);
    }
  }
}

export function readWorkspaceLease({ workspaceRoot, directory }) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  let metadata;
  try {
    metadata = lstatSync(managedDirectory);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("协调路径不是普通目录");
  }
  const marker = readOwnerMarker(managedDirectory);
  if (!marker) return null;
  try {
    const state = readStateForOwner(managedDirectory, marker);
    return (
      marker.protocol === WORKSPACE_COORDINATOR_PROTOCOL
      && state
    )
      ? state
      : null;
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

export function workspaceLeaseDirectoryExists({
  workspaceRoot,
  directory
}) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  let metadata;
  try {
    metadata = lstatSync(managedDirectory);
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("协调路径不是普通目录");
  }
  return true;
}

export async function recoverEmptyWorkspaceLease({
  workspaceRoot,
  directory
}) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  const mutex = await tryAcquireWorkspaceMutex(workspaceRoot, managedDirectory);
  if (!mutex) return false;
  try {
    let recovered = false;
    for (const claimedDirectory of claimedWorkspaceLeaseDirectories(
      managedDirectory
    )) {
      recovered = tryRemoveClaimedWorkspaceLease(claimedDirectory) || recovered;
    }
    let metadata;
    try {
      metadata = lstatSync(managedDirectory);
    } catch (error) {
      if (error?.code === "ENOENT") return recovered;
      throw error;
    }
    if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
      throw new Error("协调路径不是普通目录");
    }
    const entries = readdirSync(managedDirectory);
    if (entries.length !== 0) {
      const removalClaim = readRemovalMarker(managedDirectory);
      if (removalClaim) {
        // Holding the OS mutex proves no previous finisher is still inside its
        // critical section. Takeover does not depend on PID liveness/reuse.
        const takeover = takeOverRemovalClaim(
          managedDirectory,
          removalClaim
        );
        return takeover
          ? finishClaimedWorkspaceLease(managedDirectory, takeover) || recovered
          : recovered;
      }
      const marker = readOwnerMarker(managedDirectory);
      if (!marker || isProcessAlive(marker.ownerPid)) return recovered;
      return claimAndRemoveWorkspaceLease(
        managedDirectory,
        marker.sessionId
      ) || recovered;
    }

    // The OS mutex stays held across the emptiness check and rmdir. Acquisition
    // cannot publish a replacement pathname until this exact operation ends.
    try {
      rmdirSync(managedDirectory);
      return true;
    } catch (error) {
      if (error?.code === "ENOENT") return true;
      if (error?.code === "ENOTEMPTY") return recovered;
      throw error;
    }
  } finally {
    await releaseWorkspaceMutex(mutex);
  }
}

export function updateWorkspaceLease({
  workspaceRoot,
  directory,
  sessionId,
  update
}) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  const current = readWorkspaceLease({
    workspaceRoot,
    directory: managedDirectory
  });
  if (!current || current.sessionId !== sessionId) return false;
  try {
    const currentRevision = Number.isSafeInteger(current.revision)
      && current.revision >= 0
      ? current.revision
      : -1;
    if (currentRevision >= Number.MAX_SAFE_INTEGER) {
      throw new Error("协调状态 revision 已耗尽");
    }
    writeStateFile(managedDirectory, {
      ...current,
      ...update,
      revision: currentRevision + 1,
      updatedAt: new Date().toISOString()
    });
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  return readWorkspaceLease({
    workspaceRoot,
    directory: managedDirectory
  })?.sessionId === sessionId;
}

export async function removeWorkspaceLease({
  workspaceRoot,
  directory,
  sessionId
}) {
  const managedDirectory = assertManagedLeaseDirectory(workspaceRoot, directory);
  const mutex = await tryAcquireWorkspaceMutex(workspaceRoot, managedDirectory);
  if (!mutex) return false;
  try {
    const current = readWorkspaceLease({
      workspaceRoot,
      directory: managedDirectory
    });
    if (!current || current.sessionId !== sessionId) return false;
    return claimAndRemoveWorkspaceLease(managedDirectory, sessionId);
  } finally {
    await releaseWorkspaceMutex(mutex);
  }
}

export function isProcessAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

export function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function hashEntry(hash, absolute, relative, ignoredDirectoryNames) {
  const metadata = lstatSync(absolute);
  const normalized = relative.split(path.sep).join("/");
  if (metadata.isSymbolicLink()) {
    hash.update(`link\0${normalized}\0${readlinkSync(absolute)}\0`);
    return;
  }
  if (metadata.isDirectory()) {
    const directoryName = path.basename(absolute);
    if (
      ignoredDirectoryNames.has(directoryName)
      || directoryName.startsWith("target-")
    ) {
      return;
    }
    hash.update(`dir\0${normalized}\0`);
    for (const name of readdirSync(absolute).sort()) {
      hashEntry(
        hash,
        path.join(absolute, name),
        path.join(relative, name),
        ignoredDirectoryNames
      );
    }
    return;
  }
  if (!metadata.isFile()) return;
  hash.update(`file\0${normalized}\0${metadata.size}\0`);
  hash.update(readFileSync(absolute));
  hash.update("\0");
}

export function fingerprintWorkspaceInputs(
  workspaceRoot,
  entries,
  {
    ignoredDirectoryNames = new Set([
      ".codex-tmp",
      ".git",
      "dist",
      "node_modules",
      "target"
    ]),
    salt = ""
  } = {}
) {
  const root = path.resolve(workspaceRoot);
  const hash = createHash("sha256");
  hash.update(`${WORKSPACE_COORDINATOR_PROTOCOL}\0${salt}\0`);
  for (const entry of [...entries].sort()) {
    const absolute = path.resolve(root, entry);
    const relative = path.relative(root, absolute);
    if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) {
      throw new Error(`指纹输入越出工作区: ${entry}`);
    }
    try {
      hashEntry(hash, absolute, relative, ignoredDirectoryNames);
    } catch (error) {
      if (error?.code === "ENOENT") {
        hash.update(`missing\0${relative.split(path.sep).join("/")}\0`);
        continue;
      }
      throw error;
    }
  }
  return hash.digest("hex");
}
