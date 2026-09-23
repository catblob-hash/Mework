// Build the remote agent (`mework-remote`) for every machine this toolchain can
// target, and stage the builds where the app bundles them.
//
//   node scripts/build-remote-agents.mjs            # every target this machine can build
//   node scripts/build-remote-agents.mjs --only x86_64-unknown-linux-musl
//
// The app uploads the matching build to an SSH machine the first time it
// reaches it (src-tauri/src/remote_link.rs), so a build missing here is a
// platform whose machines keep the per-command SSH transport. Output goes to
// `src-tauri/remote-agents/<target-triple>/mework-remote[.exe]`, which
// `tauri.conf.json` ships as a resource directory.
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

import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const crateDir = path.join(root, "src-tauri");
const stageDir = path.join(crateDir, "remote-agents");

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
  const env = {
    ...process.env,
    // The agent is uploaded over SSH, so its size is what a first connection
    // waits for: no symbols, whole-program optimization.
    CARGO_PROFILE_RELEASE_STRIP: "symbols",
    CARGO_PROFILE_RELEASE_LTO: "true",
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "1",
  };
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

const onlyIndex = process.argv.indexOf("--only");
const only = onlyIndex >= 0 ? process.argv[onlyIndex + 1] : null;
const host = hostTriple();
const installed = installedTargets();
const exe = (target) => (target.includes("-windows-") ? "mework-remote.exe" : "mework-remote");
let built = 0;
let failed = 0;

for (const target of TARGETS) {
  if (only && target !== only) continue;
  const check = buildable(target, host, installed);
  if (!check.ok) {
    log(`skip ${target}: ${check.why}`);
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
    { cwd: crateDir, env: envFor(target, host), stdio: "inherit" },
  );
  if (result.status !== 0) {
    log(`FAILED ${target} (exit ${result.status})`);
    failed += 1;
    continue;
  }
  const targetDir = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(crateDir, "target");
  const artifact = path.join(targetDir, target, "release", exe(target));
  if (!existsSync(artifact)) {
    log(`FAILED ${target}: ${artifact} is missing`);
    failed += 1;
    continue;
  }
  const destination = path.join(stageDir, target, exe(target));
  mkdirSync(path.dirname(destination), { recursive: true });
  copyFileSync(artifact, destination);
  log(`staged ${destination} (${(statSync(destination).size / 1024).toFixed(0)} KiB)`);
  built += 1;
}

log(`${built} built, ${failed} failed; builds live in ${stageDir}`);
// The host's own build is the one the app cannot do without for a same-platform
// machine; anything else missing only narrows which machines get the agent.
if (failed > 0 && !existsSync(path.join(stageDir, host, exe(host)))) {
  process.exit(1);
}
