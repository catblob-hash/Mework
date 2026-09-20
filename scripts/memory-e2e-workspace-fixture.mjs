import { randomBytes } from "node:crypto";
import {
  lstatSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

export const MEMORY_E2E_ENABLE_ENV = "MEWORK_MEMORY_E2E";
export const MEMORY_E2E_RUN_ID_ENV = "MEWORK_MEMORY_E2E_RUN_ID";
export const MEMORY_E2E_WORKSPACE_ENV = "MEWORK_MEMORY_E2E_WORKSPACE";
export const MEMORY_E2E_WORKSPACE_MARKER_ENV =
  "MEWORK_MEMORY_E2E_WORKSPACE_MARKER";
export const MEMORY_E2E_MARKER_FILE = ".mework-memory-e2e-workspace";
export const MEMORY_E2E_DATA_IDENTIFIER_PREFIX =
  "com.mework.app.e2e.memory-";

export const MEMORY_E2E_HOST_ENVIRONMENT_NAMES = Object.freeze([
  MEMORY_E2E_ENABLE_ENV,
  MEMORY_E2E_RUN_ID_ENV,
  MEMORY_E2E_WORKSPACE_ENV,
  MEMORY_E2E_WORKSPACE_MARKER_ENV
]);

const RUN_ID_PATTERN = /^[0-9a-f]{24}$/;
const MARKER_PATTERN = /^[0-9a-f]{64}$/;

function canonicalPath(target) {
  return realpathSync.native(target);
}

function exactEnvironmentValue(environment, name) {
  const raw = environment[name];
  if (raw === undefined) return undefined;
  if (typeof raw !== "string") throw new Error(`${name} 必须是 Unicode 文本`);
  return raw;
}

function expectedDataIdentifier(runId) {
  return `${MEMORY_E2E_DATA_IDENTIFIER_PREFIX}${runId}`;
}

export function memoryE2eWorkspaceMarkerBody(runId, marker) {
  return [
    "MEWORK_MEMORY_E2E_WORKSPACE_V1",
    `run=${runId}`,
    `marker=${marker}`,
    ""
  ].join("\n");
}

function validateDirectTemporaryWorkspace(workspacePath, runId, temporaryRoot) {
  const canonicalTemporaryRoot = canonicalPath(temporaryRoot);
  const canonicalWorkspace = canonicalPath(workspacePath);
  const relative = path.relative(canonicalTemporaryRoot, canonicalWorkspace);
  const expectedPrefix = `mework-memory-e2e-${runId}-`;
  if (
    !path.isAbsolute(workspacePath)
    || !relative
    || path.isAbsolute(relative)
    || relative === ".."
    || relative.startsWith(`..${path.sep}`)
    || relative.includes(path.sep)
    || !path.basename(canonicalWorkspace).startsWith(expectedPrefix)
    || path.basename(canonicalWorkspace).length <= expectedPrefix.length
  ) {
    throw new Error("记忆 E2E 工作区必须是系统临时目录中的宿主直属随机目录");
  }
  const metadata = lstatSync(workspacePath);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("记忆 E2E 工作区必须是非链接目录");
  }
  return { canonicalTemporaryRoot, canonicalWorkspace };
}

/**
 * Validates the complete host-created fixture envelope before browser-dev
 * forwards it to Rust. The workspace path is never accepted from renderer IPC.
 */
export function validateMemoryE2eWorkspaceFixtureEnvironment({
  environment,
  dataIdentifier,
  bridgeToken,
  temporaryRoot = tmpdir()
}) {
  const enabled = exactEnvironmentValue(environment, MEMORY_E2E_ENABLE_ENV);
  const runId = exactEnvironmentValue(environment, MEMORY_E2E_RUN_ID_ENV);
  const workspacePath = exactEnvironmentValue(
    environment,
    MEMORY_E2E_WORKSPACE_ENV
  );
  const marker = exactEnvironmentValue(
    environment,
    MEMORY_E2E_WORKSPACE_MARKER_ENV
  );
  const anyConfigured = [enabled, runId, workspacePath, marker].some(
    (value) => value !== undefined
  );
  if (!anyConfigured) return null;
  if (enabled !== "1") {
    throw new Error(`${MEMORY_E2E_ENABLE_ENV} 只接受精确值 1`);
  }
  if (!RUN_ID_PATTERN.test(runId ?? "")) {
    throw new Error(
      `${MEMORY_E2E_RUN_ID_ENV} 必须是 12 字节随机值的小写十六进制编码`
    );
  }
  if (!MARKER_PATTERN.test(marker ?? "")) {
    throw new Error(
      `${MEMORY_E2E_WORKSPACE_MARKER_ENV} 必须是 32 字节随机值的小写十六进制编码`
    );
  }
  if (!workspacePath) {
    throw new Error(`缺少 ${MEMORY_E2E_WORKSPACE_ENV}`);
  }
  if (dataIdentifier !== expectedDataIdentifier(runId)) {
    throw new Error("记忆 E2E 必须使用与 run ID 精确绑定的独占应用数据标识");
  }
  if (!MARKER_PATTERN.test(bridgeToken ?? "")) {
    throw new Error("记忆 E2E 只接受 browser-dev 生成的 32 字节随机桥令牌");
  }

  const { canonicalWorkspace } = validateDirectTemporaryWorkspace(
    workspacePath,
    runId,
    temporaryRoot
  );
  const markerPath = path.join(canonicalWorkspace, MEMORY_E2E_MARKER_FILE);
  const markerMetadata = lstatSync(markerPath);
  if (!markerMetadata.isFile() || markerMetadata.isSymbolicLink()) {
    throw new Error("记忆 E2E 工作区 marker 必须是非链接普通文件");
  }
  const expectedMarkerBody = memoryE2eWorkspaceMarkerBody(runId, marker);
  if (readFileSync(markerPath, "utf8") !== expectedMarkerBody) {
    throw new Error("记忆 E2E 工作区 marker 与宿主身份不匹配");
  }

  return Object.freeze({
    runId,
    marker,
    workspacePath: canonicalWorkspace,
    rustEnvironment: Object.freeze({
      [MEMORY_E2E_ENABLE_ENV]: "1",
      [MEMORY_E2E_RUN_ID_ENV]: runId,
      [MEMORY_E2E_WORKSPACE_ENV]: canonicalWorkspace,
      [MEMORY_E2E_WORKSPACE_MARKER_ENV]: marker
    })
  });
}

/**
 * Creates the only supported memory browser E2E workspace. Callers receive the
 * exact environment envelope, never a way to substitute a renderer path.
 */
export function createMemoryE2eWorkspaceFixture({
  runId = randomBytes(12).toString("hex"),
  marker = randomBytes(32).toString("hex"),
  temporaryRoot = tmpdir()
} = {}) {
  if (!RUN_ID_PATTERN.test(runId)) {
    throw new Error("记忆 E2E fixture run ID 无效");
  }
  if (!MARKER_PATTERN.test(marker)) {
    throw new Error("记忆 E2E fixture marker 无效");
  }
  const canonicalTemporaryRoot = canonicalPath(temporaryRoot);
  const workspacePath = mkdtempSync(
    path.join(canonicalTemporaryRoot, `mework-memory-e2e-${runId}-`)
  );
  try {
    const canonicalWorkspace = canonicalPath(workspacePath);
    validateDirectTemporaryWorkspace(
      canonicalWorkspace,
      runId,
      canonicalTemporaryRoot
    );
    writeFileSync(
      path.join(canonicalWorkspace, MEMORY_E2E_MARKER_FILE),
      memoryE2eWorkspaceMarkerBody(runId, marker),
      { encoding: "utf8", flag: "wx", mode: 0o600 }
    );
    return Object.freeze({
      runId,
      marker,
      dataIdentifier: expectedDataIdentifier(runId),
      workspacePath: canonicalWorkspace,
      environment: Object.freeze({
        [MEMORY_E2E_ENABLE_ENV]: "1",
        [MEMORY_E2E_RUN_ID_ENV]: runId,
        [MEMORY_E2E_WORKSPACE_ENV]: canonicalWorkspace,
        [MEMORY_E2E_WORKSPACE_MARKER_ENV]: marker
      })
    });
  } catch (error) {
    rmSync(workspacePath, { recursive: true, force: true });
    throw error;
  }
}

export function cleanupMemoryE2eWorkspaceFixture(
  fixture,
  { temporaryRoot = tmpdir() } = {}
) {
  const validated = validateMemoryE2eWorkspaceFixtureEnvironment({
    environment: fixture.environment,
    dataIdentifier: fixture.dataIdentifier,
    bridgeToken: "0".repeat(64),
    temporaryRoot
  });
  if (!validated || validated.workspacePath !== fixture.workspacePath) {
    throw new Error("拒绝清理无法重新验证的记忆 E2E 工作区");
  }
  rmSync(validated.workspacePath, {
    recursive: true,
    force: true,
    maxRetries: 3,
    retryDelay: 120
  });
}
