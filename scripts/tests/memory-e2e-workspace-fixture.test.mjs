import assert from "node:assert/strict";
import { existsSync, writeFileSync } from "node:fs";
import test from "node:test";
import path from "node:path";

import {
  cleanupMemoryE2eWorkspaceFixture,
  createMemoryE2eWorkspaceFixture,
  MEMORY_E2E_ENABLE_ENV,
  MEMORY_E2E_RUN_ID_ENV,
  MEMORY_E2E_WORKSPACE_ENV,
  MEMORY_E2E_WORKSPACE_MARKER_ENV,
  MEMORY_E2E_MARKER_FILE,
  validateMemoryE2eWorkspaceFixtureEnvironment
} from "../memory-e2e-workspace-fixture.mjs";

const BRIDGE_TOKEN = "b".repeat(64);

test("creates and independently validates a canonical system-temp workspace", () => {
  const fixture = createMemoryE2eWorkspaceFixture({
    runId: "0123456789abcdef01234567",
    marker: "a".repeat(64)
  });
  try {
    const validated = validateMemoryE2eWorkspaceFixtureEnvironment({
      environment: fixture.environment,
      dataIdentifier: fixture.dataIdentifier,
      bridgeToken: BRIDGE_TOKEN
    });
    assert.equal(validated.runId, fixture.runId);
    assert.equal(validated.workspacePath, fixture.workspacePath);
    assert.deepEqual(validated.rustEnvironment, fixture.environment);
    assert.equal(path.dirname(fixture.workspacePath), path.resolve(path.dirname(fixture.workspacePath)));
  } finally {
    cleanupMemoryE2eWorkspaceFixture(fixture);
  }
  assert.equal(existsSync(fixture.workspacePath), false);
});

test("rejects partial opt-in, invalid identities, and caller-substituted directories", () => {
  assert.equal(validateMemoryE2eWorkspaceFixtureEnvironment({
    environment: {},
    dataIdentifier: "",
    bridgeToken: ""
  }), null);

  for (const environment of [
    { [MEMORY_E2E_RUN_ID_ENV]: "0".repeat(24) },
    { [MEMORY_E2E_ENABLE_ENV]: "0" },
    {
      [MEMORY_E2E_ENABLE_ENV]: "1",
      [MEMORY_E2E_RUN_ID_ENV]: "not-random",
      [MEMORY_E2E_WORKSPACE_ENV]: process.cwd(),
      [MEMORY_E2E_WORKSPACE_MARKER_ENV]: "a".repeat(64)
    }
  ]) {
    assert.throws(() => validateMemoryE2eWorkspaceFixtureEnvironment({
      environment,
      dataIdentifier: "com.mework.app.e2e.memory-not-random",
      bridgeToken: BRIDGE_TOKEN
    }));
  }

  const fixture = createMemoryE2eWorkspaceFixture();
  try {
    assert.throws(() => validateMemoryE2eWorkspaceFixtureEnvironment({
      environment: fixture.environment,
      dataIdentifier: `${fixture.dataIdentifier}-forged`,
      bridgeToken: BRIDGE_TOKEN
    }), /应用数据标识/);
    assert.throws(() => validateMemoryE2eWorkspaceFixtureEnvironment({
      environment: {
        ...fixture.environment,
        [MEMORY_E2E_WORKSPACE_ENV]: process.cwd()
      },
      dataIdentifier: fixture.dataIdentifier,
      bridgeToken: BRIDGE_TOKEN
    }), /系统临时目录/);
  } finally {
    cleanupMemoryE2eWorkspaceFixture(fixture);
  }
});

test("refuses a tampered marker and will not delete that directory", () => {
  const fixture = createMemoryE2eWorkspaceFixture();
  const markerPath = path.join(fixture.workspacePath, MEMORY_E2E_MARKER_FILE);
  writeFileSync(markerPath, "tampered", "utf8");
  assert.throws(() => cleanupMemoryE2eWorkspaceFixture(fixture), /marker/);
  assert.equal(existsSync(fixture.workspacePath), true);

  writeFileSync(
    markerPath,
    [
      "MEWORK_MEMORY_E2E_WORKSPACE_V1",
      `run=${fixture.runId}`,
      `marker=${fixture.marker}`,
      ""
    ].join("\n"),
    "utf8"
  );
  cleanupMemoryE2eWorkspaceFixture(fixture);
});
