import assert from "node:assert/strict";
import test from "node:test";

import { MACOS_DEV_RUNNER, withMacosDevSigning } from "../macos-dev-signing.mjs";

test("routes both Mac targets through the signing runner when an identity is named", () => {
  const environment = withMacosDevSigning({ MEWORK_DEV_SIGNING_IDENTITY: "Apple Development: T" }, "darwin");
  assert.equal(environment.CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER, MACOS_DEV_RUNNER);
  assert.equal(environment.CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER, MACOS_DEV_RUNNER);
});

test("changes nothing without an identity or off macOS", () => {
  const plain = { PATH: "/usr/bin" };
  assert.equal(withMacosDevSigning(plain, "darwin"), plain);
  assert.equal(withMacosDevSigning({ MEWORK_DEV_SIGNING_IDENTITY: "  " }, "darwin").CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER, undefined);
  const windows = { MEWORK_DEV_SIGNING_IDENTITY: "X" };
  assert.equal(withMacosDevSigning(windows, "win32"), windows);
});

test("keeps a runner the developer configured", () => {
  const environment = withMacosDevSigning(
    { MEWORK_DEV_SIGNING_IDENTITY: "X", CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER: "my-runner" },
    "darwin"
  );
  assert.equal(environment.CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER, "my-runner");
  assert.equal(environment.CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER, MACOS_DEV_RUNNER);
});
