// Build the remote agent (`mework-remote`) for every machine this toolchain can
// target, and stage the builds where the app bundles them.
//
//   node scripts/build-remote-agents.mjs            # every target this machine can build
//   node scripts/build-remote-agents.mjs --release  # ... and fail unless every target has a build
//   node scripts/build-remote-agents.mjs --only x86_64-unknown-linux-musl
//
// The app uploads its build to an SSH machine whenever the machine is not
// running exactly that build (src-tauri/src/remote_link.rs): updating a
// machine, or rolling it back, is only ever this app handing over its own
// agent, and the machine needs no network of its own for it. Output goes to
// `src-tauri/remote-agents/<target-triple>/mework-remote[.exe]`, which
// `tauri.conf.json` ships as a resource directory.
//
// Every build carries the digest of the agent source it was made from
// (`mework-remote-source:<sha256>`, see src-tauri/remote-agent/build.rs), and
// the app installs only builds made from its own source. A full run therefore
// removes staged builds this run could not remake and that were made from
// other source: bundled, they would only be dead weight. `--release` (what
// `tauri build` runs) then insists that every target has a build, because a
// release is the only place its machines can get one from; set
// MEWORK_REMOTE_AGENTS_ALLOW_MISSING=1 to ship without the missing ones anyway.
// A development build of the app that meets a machine with no build runs this
// script with `--only` for that machine's targets; exit status 3 means this
// computer has no way to build the target asked for.
//
// This machine's own triple always builds. The others build when their Rust
// target is installed (`rustup target add <triple>`):
//
// * Linux musl targets link with the `rust-lld` rustup ships, so a Mac or
//   Windows machine needs no C cross toolchain for them: the agent has no C
//   dependencies and musl brings its own C runtime objects.
// * Apple targets build on a Mac (both architectures, with Xcode's SDK).
// * Windows targets build on Windows with Visual Studio's build tools, and
//   elsewhere with `cargo xwin` (https://github.com/rust-cross/cargo-xwin),
//   which fetches Microsoft's CRT and SDK libraries on first use — under their
//   license, which whoever installs it accepts. A build made on another
//   machine can be staged by copying it to
//   `src-tauri/remote-agents/<target-triple>/mework-remote.exe`.
//
// Windows builds link the C runtime statically: the agent is uploaded to
// machines that need not have the Visual C++ runtime installed.
//
// A Windows build is staged with `srt-win.exe` beside it: the Windows backend
// of the sandbox (vendored, Apache-2.0, under `src-tauri/vendor/srt-win`),
// which the agent finds next to its own executable. It is a crate of its own,
// built here with the agent. Its SQLite is C, which `cargo xwin` archives with
// `llvm-lib`; when that is not on `PATH`, the `llvm-ar` of rustup's
// `llvm-tools` component stands in for it (`rustup component add llvm-tools`).

import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, symlinkSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const crateDir = path.join(root, "src-tauri");
const stageDir = path.join(crateDir, "remote-agents");
const helperDir = path.join(crateDir, "vendor", "srt-win");
/** The Windows sandbox helper, staged beside the agent. */
const HELPER = "srt-win.exe";

const TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
];

function log(message) {
  console.log(`[remote-agents] ${message}`);
}

function hostTriple() {
  const text = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const line = text.split("\n").find((entry) => entry.startsWith("host: "));
  if (!line) throw new Error("rustc -vV did not report a host triple");
  return line.slice("host: ".length).trim();
}

function installedTargets() {
  const result = spawnSync("rustup", ["target", "list", "--installed"], { encoding: "utf8" });
  if (result.status !== 0) return new Set();
  return new Set(result.stdout.split("\n").map((entry) => entry.trim()).filter(Boolean));
}

function hasCargoXwin() {
  const result = spawnSync("cargo", ["xwin", "--version"], { encoding: "utf8" });
  return result.status === 0;
}

function buildable(target, host, installed) {
  if (target === host) return { ok: true };
  if (!installed.has(target)) return { ok: false, why: `rustup target ${target} is not installed` };
  if (target.endsWith("-apple-darwin") && !host.endsWith("-apple-darwin")) {
    return { ok: false, why: "Apple targets build on a Mac" };
  }
  if (target.includes("-windows-") && !host.includes("-windows-")) {
    if (!target.endsWith("-windows-msvc") || !hasCargoXwin()) {
      return {
        ok: false,
        why: "Windows targets build on Windows, or with `cargo xwin` (cargo install cargo-xwin)",
      };
    }
    return { ok: true, xwin: true };
  }
  return { ok: true };
}

function envFor(target, host) {
  const env = { ...process.env };
  const targetKey = target.toUpperCase().replaceAll("-", "_");
  if (target.includes("-linux-musl") && !host.includes("-linux-")) {
    env[`CARGO_TARGET_${targetKey}_LINKER`] ??= "rust-lld";
  }
  if (target.includes("-windows-")) {
    const key = `CARGO_TARGET_${targetKey}_RUSTFLAGS`;
    env[key] = [env[key], "-C target-feature=+crt-static"].filter(Boolean).join(" ");
  }
  return env;
}

function agentEnv(target, host) {
  return {
    ...envFor(target, host),
    // The agent is uploaded over SSH, so its size is what a first connection
    // waits for: no symbols, whole-program optimization.
    CARGO_PROFILE_RELEASE_STRIP: "symbols",
    CARGO_PROFILE_RELEASE_LTO: "true",
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "1",
  };
}

function onPath(name) {
  const names = process.platform === "win32" ? [name, `${name}.exe`] : [name];
  return (process.env.PATH ?? "")
    .split(path.delimiter)
    .filter(Boolean)
    .some((dir) => names.some((entry) => existsSync(path.join(dir, entry))));
}

/**
 * The environment srt-win builds in for `target`, or why it cannot. srt-win
 * keeps its own release profile; a `cargo xwin` build needs `llvm-lib`, made
 * from rustup's `llvm-ar` when there is none.
 */
function helperEnv(target, host, xwin) {
  const env = envFor(target, host);
  if (!xwin || onPath("llvm-lib")) return { env };
  const sysroot = execFileSync("rustc", ["--print", "sysroot"], { encoding: "utf8" }).trim();
  const exeSuffix = process.platform === "win32" ? ".exe" : "";
  const llvmAr = path.join(sysroot, "lib", "rustlib", host, "bin", `llvm-ar${exeSuffix}`);
  if (!existsSync(llvmAr)) {
    return { why: "srt-win's C code needs llvm-lib, which rustup's llvm-tools provide (rustup component add llvm-tools)" };
  }
  // llvm-ar acts as llvm-lib when it is started by that name.
  const shimDir = path.join(targetDir, "llvm-lib-shim");
  const shim = path.join(shimDir, `llvm-lib${exeSuffix}`);
  mkdirSync(shimDir, { recursive: true });
  rmSync(shim, { force: true });
  try {
    symlinkSync(llvmAr, shim);
  } catch {
    copyFileSync(llvmAr, shim);
  }
  env.PATH = [shimDir, env.PATH].filter(Boolean).join(path.delimiter);
  return { env };
}

/** Builds srt-win for `target` and stages it beside the agent; whether it did. */
function stageHelper(target, check) {
  const { env, why } = helperEnv(target, host, check.xwin);
  if (!env) {
    log(`FAILED ${target} sandbox helper: ${why}`);
    return false;
  }
  log(`build ${target} sandbox helper (srt-win)`);
  const result = spawnSync(
    "cargo",
    [...(check.xwin ? ["xwin", "build"] : ["build"]), "--release", "--locked", "--target", target],
    { cwd: helperDir, env, stdio: ["ignore", "inherit", "inherit"] },
  );
  if (result.status !== 0) {
    log(`FAILED ${target} sandbox helper (exit ${result.status})`);
    return false;
  }
  const helperTargetDir = process.env.CARGO_TARGET_DIR ? targetDir : path.join(helperDir, "target");
  const artifact = path.join(helperTargetDir, target, "release", HELPER);
  if (!existsSync(artifact)) {
    log(`FAILED ${target} sandbox helper: ${artifact} is missing`);
    return false;
  }
  const destination = path.join(stageDir, target, HELPER);
  copyFileSync(artifact, destination);
  log(`staged ${destination} (${(statSync(destination).size / 1024).toFixed(0)} KiB)`);
  return true;
}

/** How `--only` says this computer cannot build the target at all (see remote_link.rs). */
const CANNOT_BUILD_HERE = 3;
const SOURCE_MARKER = Buffer.from("mework-remote-source:");

/** The agent source a build was made from, read from its bytes; null for one from before source identities. */
function sourceOf(file) {
  const bytes = readFileSync(file);
  for (let at = bytes.indexOf(SOURCE_MARKER); at >= 0; at = bytes.indexOf(SOURCE_MARKER, at + 1)) {
    const start = at + SOURCE_MARKER.length;
    const id = bytes.subarray(start, start + 64).toString("latin1");
    if (/^[0-9a-f]{64}$/.test(id)) return id;
  }
  return null;
}

const onlyIndex = process.argv.indexOf("--only");
const only = onlyIndex >= 0 ? process.argv[onlyIndex + 1] : null;
const release = process.argv.includes("--release");
const host = hostTriple();
const installed = installedTargets();
const exe = (target) => (target.includes("-windows-") ? "mework-remote.exe" : "mework-remote");
const staged = (target) => path.join(stageDir, target, exe(target));
const targetDir = process.env.CARGO_TARGET_DIR
  ? path.resolve(process.env.CARGO_TARGET_DIR)
  : path.join(crateDir, "target");
const builtNow = new Set();
let failed = 0;

if (only && !TARGETS.includes(only)) {
  log(`skip ${only}: not one of the targets this script builds`);
  process.exit(CANNOT_BUILD_HERE);
}

for (const target of TARGETS) {
  if (only && target !== only) continue;
  const check = buildable(target, host, installed);
  if (!check.ok) {
    log(`skip ${target}: ${check.why}`);
    if (only) process.exit(CANNOT_BUILD_HERE);
    continue;
  }
  log(`build ${target}${check.xwin ? " (cargo xwin)" : ""}`);
  const result = spawnSync(
    "cargo",
    [
      ...(check.xwin ? ["xwin", "build"] : ["build"]),
      "-p",
      "mework-remote-agent",
      "--release",
      "--target",
      target,
    ],
    { cwd: crateDir, env: agentEnv(target, host), stdio: ["ignore", "inherit", "inherit"] },
  );
  if (result.status !== 0) {
    log(`FAILED ${target} (exit ${result.status})`);
    failed += 1;
    continue;
  }
  const artifact = path.join(targetDir, target, "release", exe(target));
  if (!existsSync(artifact)) {
    log(`FAILED ${target}: ${artifact} is missing`);
    failed += 1;
    continue;
  }
  const destination = staged(target);
  mkdirSync(path.dirname(destination), { recursive: true });
  copyFileSync(artifact, destination);
  log(`staged ${destination} (${(statSync(destination).size / 1024).toFixed(0)} KiB)`);
  builtNow.add(target);
  // Without its helper the agent still serves SSH machines; only the
  // sandbox on this computer needs it.
  if (target.includes("-windows-") && !stageHelper(target, check)) failed += 1;
}

if (only) {
  process.exit(builtNow.has(only) ? 0 : 1);
}

// This machine's own build is always made, so it names the source every other staged build has
// to have been made from.
const expected = builtNow.has(host) ? sourceOf(staged(host)) : null;
if (!expected) {
  log(`FAILED: no ${host} build to tell the current agent source by`);
  process.exit(1);
}
const covered = [];
const missing = [];
const helperless = [];
for (const target of TARGETS) {
  const file = staged(target);
  const helper = path.join(stageDir, target, HELPER);
  if (!existsSync(file)) {
    rmSync(helper, { force: true });
    missing.push(target);
    continue;
  }
  const source = sourceOf(file);
  if (source !== expected) {
    rmSync(file, { force: true });
    rmSync(helper, { force: true });
    log(`removed the staged ${target} build: it was made from ${source ? `other agent source (${source.slice(0, 12)})` : "agent source older than source identities"}, not ${expected.slice(0, 12)}`);
    missing.push(target);
    continue;
  }
  covered.push(target);
  if (target.includes("-windows-") && !existsSync(helper)) helperless.push(target);
}

log(`${builtNow.size} built, ${failed} failed; builds for ${covered.join(", ") || "nothing"} live in ${stageDir}`);
if (missing.length > 0) {
  log(`no build for ${missing.join(", ")}: machines of those platforms cannot be given the agent by this build of the app`);
  if (release && process.env.MEWORK_REMOTE_AGENTS_ALLOW_MISSING !== "1") {
    log("a release has to carry every platform's agent (install the Rust targets and tools the skips above name, or build on another machine and copy the build in); set MEWORK_REMOTE_AGENTS_ALLOW_MISSING=1 to release without them");
    process.exit(1);
  }
}
if (helperless.length > 0) {
  log(`no ${HELPER} beside the ${helperless.join(", ")} build: Mework on those platforms has no sandbox`);
  if (release && process.env.MEWORK_REMOTE_AGENTS_ALLOW_MISSING !== "1") {
    log(`a release has to carry the sandbox helper with every Windows agent (see the failures above); set MEWORK_REMOTE_AGENTS_ALLOW_MISSING=1 to release without it`);
    process.exit(1);
  }
}
