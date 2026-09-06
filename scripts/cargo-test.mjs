#!/usr/bin/env node
// Runs `cargo test` for the Tauri crate inside the project's own native build
// environment, and makes the produced test binary actually loadable.
//
// Why this exists: the lib test binary is linked into `target/<profile>/deps/`, but
// `WebView2Loader.dll` is staged one directory up in `target/<profile>/`. Cargo adds
// that directory to the child's DLL search path when *it* runs the harness; anything
// that execs the binary another way gets STATUS_DLL_NOT_FOUND (exit 0xC0000135), which
// surfaces as an empty test run rather than a readable error.
//
// Usage:
//   node scripts/cargo-test.mjs                      # cargo test --lib
//   node scripts/cargo-test.mjs --lib storage::       # filter
//   node scripts/cargo-test.mjs -- --test-threads=4   # harness args
//
// CARGO_TARGET_DIR may be set by the caller to keep a run out of the shared target
// directory; cargo takes an exclusive lock on it, and concurrent runs starve rather
// than fail, which looks exactly like a hang.
//
// Interrupting a run ends the process tree this wrapper started — cargo and the
// `mework_lib-<hash>.exe` harness it linked — and reports a non-zero exit code. See
// scripts/cargo-test-lifecycle.mjs for why, and for the ownership rule that keeps it from
// touching another session's processes.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { superviseCargoTest } from "./cargo-test-lifecycle.mjs";
import { windowsNativeBuildEnvironment } from "./windows-native-build-tools.mjs";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const crateDir = path.join(repoRoot, "src-tauri");

function pathKeyOf(environment) {
  return Object.keys(environment).find((key) => key.toLowerCase() === "path") ?? "Path";
}

function withRuntimeDllDirectory(environment, targetDir, profile) {
  const result = { ...environment };
  const key = pathKeyOf(result);
  const runtimeDir = path.win32.normalize(path.join(targetDir, profile));
  if (!existsSync(runtimeDir)) return result;
  const existing = (result[key] ?? "")
    .split(";")
    .filter((entry) => entry.trim() && entry.trim().toLowerCase() !== runtimeDir.toLowerCase());
  result[key] = [runtimeDir, ...existing].join(";");
  return result;
}

const args = process.argv.slice(2);
const cargoArgs = args.length > 0 ? args : ["--lib"];
const profile = cargoArgs.includes("--release") ? "release" : "debug";
const targetDir = process.env.CARGO_TARGET_DIR
  ? path.resolve(process.env.CARGO_TARGET_DIR)
  : path.join(crateDir, "target");

const base = await windowsNativeBuildEnvironment();
const environment = withRuntimeDllDirectory(
  { ...base, CARGO_TARGET_DIR: targetDir },
  targetDir,
  profile,
);

// Spawned without a shell on purpose: the pid recorded here has to be cargo's own, because a
// Ctrl+C is answered by killing the tree rooted at it. A `cmd.exe` in between usually exits
// first, and once it is gone there is no parent chain left that reaches an orphaned cargo or its
// test harness. libuv finds `cargo.exe` on the child's own PATH, which is the build environment
// computed above.
const child = spawn("cargo", ["test", ...cargoArgs], {
  cwd: crateDir,
  env: environment,
  stdio: "inherit",
});

// `process.exitCode` rather than `process.exit`: stderr is a pipe when this runs under
// `npm test`, and exiting immediately after writing truncates the diagnostic.
const outcome = await superviseCargoTest({
  child,
  onDiagnostic: (line) => process.stderr.write(`${line}\n`),
});
if (outcome.message) process.stderr.write(`${outcome.message}\n`);
process.exitCode = outcome.exitCode;
