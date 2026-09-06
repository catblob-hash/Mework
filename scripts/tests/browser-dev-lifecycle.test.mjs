import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";

import {
  isControlledBackendFinalShutdown,
  isControlledBackendRestart,
  stopBrowserDevChild
} from "../browser-dev-lifecycle.mjs";

function accepted(overrides = {}) {
  return isControlledBackendRestart({
    code: 75,
    signal: null,
    requestMarkerSeen: true,
    releaseBarrierMarkerSeen: true,
    commitMarkerSeen: true,
    webSearchE2EEnabled: false,
    imageInputE2EEnabled: true,
    ...overrides
  });
}

test("accepts only the three documented wrapper exit codes with the full release handshake", () => {
  assert.equal(accepted({ code: 0 }), true);
  assert.equal(accepted({ code: 75 }), true);
  assert.equal(accepted({ code: 101 }), true);
  assert.equal(accepted({ code: 1 }), false);
  assert.equal(accepted({ code: undefined }), false);
});

test("rejects partial handshakes, signals, and ordinary browser-dev sessions", () => {
  assert.equal(accepted({ requestMarkerSeen: false }), false);
  assert.equal(accepted({ releaseBarrierMarkerSeen: false }), false);
  assert.equal(accepted({ commitMarkerSeen: false }), false);
  assert.equal(accepted({ signal: "SIGTERM" }), false);
  assert.equal(accepted({
    webSearchE2EEnabled: false,
    imageInputE2EEnabled: false
  }), false);
});

test("also admits the authenticated web-search E2E restart scope", () => {
  assert.equal(accepted({
    webSearchE2EEnabled: true,
    imageInputE2EEnabled: false
  }), true);
});

function finalShutdownAccepted(overrides = {}) {
  return isControlledBackendFinalShutdown({
    code: 0,
    signal: null,
    requestMarkerSeen: true,
    releaseBarrierMarkerSeen: true,
    commitMarkerSeen: true,
    imageInputE2EEnabled: true,
    ...overrides
  });
}

test("accepts only a complete image-E2E final shutdown handshake", () => {
  assert.equal(finalShutdownAccepted(), true);
  assert.equal(finalShutdownAccepted({ code: 75 }), false);
  assert.equal(finalShutdownAccepted({ signal: "SIGTERM" }), false);
  assert.equal(finalShutdownAccepted({ requestMarkerSeen: false }), false);
  assert.equal(finalShutdownAccepted({ releaseBarrierMarkerSeen: false }), false);
  assert.equal(finalShutdownAccepted({ commitMarkerSeen: false }), false);
  assert.equal(finalShutdownAccepted({ imageInputE2EEnabled: false }), false);
});

test("waits until a real child is closed before reporting it stopped", {
  timeout: 20_000
}, async () => {
  const child = spawn(
    process.execPath,
    [
      "-e",
      "process.stdout.write('ready\\n'); setInterval(() => {}, 1000)"
    ],
    {
      windowsHide: true,
      stdio: ["ignore", "pipe", "ignore"]
    }
  );
  try {
    await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.stdout.once("data", resolve);
    });
    await stopBrowserDevChild(child);
    assert.equal(
      child.exitCode !== null || child.signalCode !== null,
      true,
      "stop must resolve only after the child close state is observable"
    );
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill();
  }
});
