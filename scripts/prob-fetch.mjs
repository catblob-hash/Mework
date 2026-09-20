#!/usr/bin/env node
// Assembles the formal-verification toolchain in `tools/prob/`.
//
// This is the local installation entry point for the ProB verifier described in
// docs/formal-methods.md. Specifications are committed under formal/, while each
// developer generates the verifier with `npm run prob:fetch`. This script and
// scripts/formal-verify.mjs share the following layout:
//
//   tools/prob/
//     probcli.exe               # ProB CLI, including lib/cspmf.exe
//     lib/tla2b.jar             # TLA+ to B translator discovered by probcli
//     jre/<release>/            # Portable Temurin JRE injected into PATH by
//                               #   probEnvironment(); no system install
//     PROB-MANIFEST.json        # Pinned sources, hashes, and assembly time
//
// All external sources are pinned and verified:
//   * ProB CLI: versioned URL and SHA-256.
//   * TLA2B.jar: an unversioned upstream URL pinned by SHA-256. A changed
//     download fails verification until a confirmed usable JAR is repinned.
//   * Temurin JRE: versioned GitHub release URL and SHA-256.
//
// Self-check uses counterexamples: each capability proves a passing case and a
// failing case so the verifier cannot silently accept every input.
//
// Usage:
//   node scripts/prob-fetch.mjs               # Assemble and self-check
//   node scripts/prob-fetch.mjs --no-check    # Assemble only
//   node scripts/prob-fetch.mjs --self-check  # Check existing artifacts

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const cacheDir = path.join(repoRoot, ".prob-cache");
export const probDir = path.join(repoRoot, "tools", "prob");

// ---------------------------------------------------------------------------
// Pinned toolchain specifications.
// ---------------------------------------------------------------------------

const PROB_CLI = {
  version: "1.16.0",
  url: "https://stups.hhu-hosting.de/downloads/prob/cli/releases/1.16.0/probcli_windows64.zip",
  sha256: "a58d9e7beb247f334bc9a93b41861706f4432d5fa2d44e4c2148a81b0dcf6e52",
  file: "probcli_windows64_1.16.0.zip",
};

const TLA2B = {
  url: "https://stups.hhu-hosting.de/downloads/prob/tcltk/jars/TLA2B.jar",
  sha256: "5ced2fec2822e385214c565732f7ca61f39fbc6d3a60531268ebe6ed11515454",
  file: "TLA2B.jar",
};

const TEMURIN_JRE = {
  release: "jdk-21.0.12+8-jre",
  url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12%2B8/OpenJDK21U-jre_x64_windows_hotspot_21.0.12_8.zip",
  sha256: "b8aa18fef5edb69bee8618f99677d66d0873d22cb40d974c15ac9ffcdecf73ba",
  file: "temurin-jre-21.0.12+8.zip",
};

function log(message) {
  process.stdout.write(`[prob-fetch] ${message}\n`);
}

function fail(message) {
  process.stderr.write(`[prob-fetch] 失败：${message}\n`);
  process.exit(1);
}

function sha256Of(filePath) {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function download(spec) {
  const target = path.join(cacheDir, spec.file);
  if (existsSync(target) && sha256Of(target) === spec.sha256) {
    log(`缓存命中：${spec.file}`);
    return target;
  }
  log(`下载 ${spec.url}`);
  const response = await fetch(spec.url, { redirect: "follow" });
  if (!response.ok) throw new Error(`下载失败 HTTP ${response.status}：${spec.url}`);
  const partial = `${target}.partial`;
  await pipeline(Readable.fromWeb(response.body), createWriteStream(partial));
  const actual = sha256Of(partial);
  if (actual !== spec.sha256) {
    rmSync(partial, { force: true });
    throw new Error(
      `SHA-256 不符：${spec.file}\n  期望 ${spec.sha256}\n  实得 ${actual}\n` +
        "（若上游滚动更新了制品，必须人工确认新制品可用后再更新本文件的钉版）",
    );
  }
  rmSync(target, { force: true });
  copyFileSync(partial, target);
  rmSync(partial, { force: true });
  return target;
}

function extractZip(zipPath, destDir) {
  // Use Windows System32 bsdtar (libarchive), which extracts ZIPs and accepts
  // `C:\` drive paths. MSYS2 GNU tar parses `C:` as a remote host.
  const systemTar = path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  const tar = existsSync(systemTar) ? systemTar : "tar";
  const result = spawnSync(tar, ["-xf", zipPath, "-C", destDir], {
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  if (result.status !== 0) {
    throw new Error(`解压 ${zipPath} 失败：${result.stderr?.toString() ?? "unknown"}`);
  }
}

export function probcliPath() {
  return path.join(probDir, "probcli.exe");
}

function jreBinDir() {
  return path.join(probDir, "jre", TEMURIN_JRE.release, "bin");
}

// probcli discovers Java for tla2b.jar through PATH; inject the portable JRE.
export function probEnvironment(base = process.env) {
  const result = { ...base };
  const key = Object.keys(result).find((k) => k.toLowerCase() === "path") ?? "Path";
  result[key] = [jreBinDir(), result[key] ?? ""].join(path.delimiter);
  return result;
}

export function runProbcli(args, options = {}) {
  // This probcli build resolves its `lib/` directory from cwd, not argv[0] or
  // PROBDIR. Always run with `probDir` as cwd and pass absolute file paths.
  // TLA2B writes generated files beside the `.tla` source regardless of cwd.
  return spawnSync(probcliPath(), args, {
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    env: probEnvironment(),
    encoding: "utf8",
    timeout: options.timeout ?? 120_000,
    cwd: probDir,
  });
}

function assembled() {
  return existsSync(probcliPath()) && existsSync(path.join(probDir, "lib", "tla2b.jar"));
}

// ---------------------------------------------------------------------------
// Self-check: every capability requires a passing case and a failing case.
// ---------------------------------------------------------------------------

const SMOKE_CSP = `channel req, ok, err

SPEC = req -> (ok -> SPEC [] err -> SPEC)

GOOD = req -> ok -> req -> err -> STOP
BAD = ok -> STOP
DEADLOCKY = req -> STOP
`;

const SMOKE_TLA = `---- MODULE ProbSmoke ----
EXTENDS Naturals
VARIABLES x
Init == x = 0
Incr == x' = (x + 1) % 3
Reset == x = 2 /\\ x' = 0
Next == Incr \\/ Reset
Inv == x < 3
BadInv == x < 2
====
`;

function expectExit(label, result, expected) {
  if (result.error) fail(`${label}：无法执行 probcli（${result.error.message}）`);
  if (result.status !== expected) {
    fail(
      `${label}：期望退出码 ${expected}，实得 ${result.status}\n` +
        `--- stdout 尾部 ---\n${(result.stdout ?? "").slice(-2000)}\n` +
        `--- stderr 尾部 ---\n${(result.stderr ?? "").slice(-2000)}`,
    );
  }
  log(`自检通过：${label}`);
}

export function selfCheck() {
  if (!assembled()) fail("tools/prob 尚未装配，先跑 node scripts/prob-fetch.mjs");
  const version = runProbcli(["-svers"]);
  if (version.status !== 0 || !`${version.stdout}`.includes(PROB_CLI.version)) {
    fail(`probcli -svers 未报告 ${PROB_CLI.version}：${version.stdout} ${version.stderr}`);
  }
  log(`自检通过：probcli ${PROB_CLI.version}`);

  const work = mkdtempSync(path.join(os.tmpdir(), "prob-smoke-"));
  try {
    const csp = path.join(work, "smoke.csp");
    writeFileSync(csp, SMOKE_CSP);
    expectExit("CSP 迹精化（正例）", runProbcli(["-strict", "-csp_assertion", "SPEC [T= GOOD", csp]), 0);
    expectExit("CSP 迹精化（错例必须失败）", runProbcli(["-strict", "-csp_assertion", "SPEC [T= BAD", csp]), 1);
    expectExit(
      "CSP 死锁断言（正例）",
      runProbcli(["-strict", "-csp_assertion", "SPEC :[deadlock free [F]]", csp]),
      0,
    );
    expectExit(
      "CSP 死锁断言（错例必须失败）",
      runProbcli(["-strict", "-csp_assertion", "DEADLOCKY :[deadlock free [F]]", csp]),
      1,
    );

    // The TLA smoke test covers both tla2b.jar and the portable JRE.
    const tla = path.join(work, "ProbSmoke.tla");
    writeFileSync(tla, SMOKE_TLA);
    writeFileSync(path.join(work, "ProbSmoke.cfg"), "INIT Init\nNEXT Next\nINVARIANT Inv\n");
    expectExit("TLA 不变量（正例）", runProbcli([tla, "-model_check", "-strict"]), 0);
    writeFileSync(path.join(work, "ProbSmoke.cfg"), "INIT Init\nNEXT Next\nINVARIANT BadInv\n");
    expectExit(
      "TLA 不变量（错例必须失败）",
      runProbcli([tla, "-model_check", "-strict"]),
      1,
    );

    // CSP||TLA composition guides the TLA-translated machine with a CSP process.
    // The guide executes two `Incr` steps to x=2, which violates `BadInv` but
    // not `Inv`.
    const guide = path.join(work, "guide.csp");
    writeFileSync(guide, "channel Incr\n\nMAIN = Incr -> Incr -> STOP\n");
    writeFileSync(path.join(work, "ProbSmoke.cfg"), "INIT Init\nNEXT Next\nINVARIANT Inv\n");
    expectExit(
      "CSP||TLA 组合（正例）",
      runProbcli([tla, "-csp_guide", guide, "-model_check", "-nodead", "-strict"]),
      0,
    );
    writeFileSync(path.join(work, "ProbSmoke.cfg"), "INIT Init\nNEXT Next\nINVARIANT BadInv\n");
    expectExit(
      "CSP||TLA 组合（错例必须失败）",
      runProbcli([tla, "-csp_guide", guide, "-model_check", "-nodead", "-strict"]),
      1,
    );

    // Trace replay distinguishes an exact replay, a rejected final event whose
    // guard is false, and malformed JSON.
    writeFileSync(path.join(work, "ProbSmoke.cfg"), "INIT Init\nNEXT Next\nINVARIANT Inv\n");
    const goodTrace = path.join(work, "good.prob2trace");
    writeFileSync(
      goodTrace,
      JSON.stringify({
        transitionList: [{ name: "$initialise_machine" }, { name: "Incr" }, { name: "Incr" }],
      }),
    );
    expectExit("trace 回放（正例）", runProbcli([tla, "-trace_replay", "json", goodTrace, "-strict"]), 0);
    const refusedTrace = path.join(work, "refused.prob2trace");
    writeFileSync(
      refusedTrace,
      JSON.stringify({ transitionList: [{ name: "$initialise_machine" }, { name: "Reset" }] }),
    );
    expectExit(
      "trace 回放（拒收错例必须失败）",
      runProbcli([tla, "-trace_replay", "json", refusedTrace, "-strict"]),
      1,
    );
    const malformedTrace = path.join(work, "malformed.prob2trace");
    writeFileSync(malformedTrace, "这不是 JSON");
    expectExit(
      "trace 回放（畸形文件必须失败）",
      runProbcli([tla, "-trace_replay", "json", malformedTrace, "-strict"]),
      1,
    );
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

// Assembly alone is insufficient: the manifest must match these pinned hashes
// and probcli must report the pinned version. Reassemble stale artifacts so an
// old toolchain cannot impersonate the verified one.
function assembledAndPinned() {
  if (!assembled()) return false;
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path.join(probDir, "PROB-MANIFEST.json"), "utf8"));
  } catch {
    return false;
  }
  if (manifest?.probCli?.sha256 !== PROB_CLI.sha256) return false;
  if (manifest?.tla2b?.sha256 !== TLA2B.sha256) return false;
  if (manifest?.jre?.sha256 !== TEMURIN_JRE.sha256) return false;
  const version = runProbcli(["-svers"], { timeout: 30_000 });
  return version.status === 0 && `${version.stdout}`.includes(PROB_CLI.version);
}

export async function ensureProb() {
  if (assembledAndPinned()) return;
  await assemble({ check: false });
}

async function assemble({ check }) {
  mkdirSync(cacheDir, { recursive: true });
  const cliZip = await download(PROB_CLI);
  const tlaJar = await download(TLA2B);
  const jreZip = await download(TEMURIN_JRE);

  rmSync(probDir, { recursive: true, force: true });
  const staging = `${probDir}.staging`;
  rmSync(staging, { recursive: true, force: true });
  mkdirSync(staging, { recursive: true });
  log("解压 ProB CLI");
  extractZip(cliZip, staging);
  copyFileSync(tlaJar, path.join(staging, "lib", "tla2b.jar"));
  const jreDir = path.join(staging, "jre");
  mkdirSync(jreDir, { recursive: true });
  log("解压便携 JRE");
  extractZip(jreZip, jreDir);
  const jreEntries = readdirSync(jreDir);
  if (!jreEntries.includes(TEMURIN_JRE.release)) {
    throw new Error(`JRE 解压布局意外：${jreEntries.join(", ")}（期望 ${TEMURIN_JRE.release}）`);
  }
  writeFileSync(
    path.join(staging, "PROB-MANIFEST.json"),
    `${JSON.stringify(
      {
        assembledAt: new Date().toISOString(),
        probCli: { version: PROB_CLI.version, url: PROB_CLI.url, sha256: PROB_CLI.sha256 },
        tla2b: { url: TLA2B.url, sha256: TLA2B.sha256 },
        jre: { release: TEMURIN_JRE.release, url: TEMURIN_JRE.url, sha256: TEMURIN_JRE.sha256 },
      },
      null,
      2,
    )}\n`,
  );
  mkdirSync(path.dirname(probDir), { recursive: true });
  // Windows directory renames can transiently fail with EPERM while antivirus
  // scans an open handle. The complete staging directory can be retried.
  const { renameSync } = await import("node:fs");
  for (let attempt = 0; ; attempt += 1) {
    try {
      renameSync(staging, probDir);
      break;
    } catch (error) {
      if (attempt >= 4) throw error;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  }
  log(`装配完成：${probDir}`);
  if (check) selfCheck();
}

async function main() {
  if (process.platform !== "win32") {
    fail("prob-fetch 目前只钉了 Windows 制品（probcli_windows64 + windows JRE）");
  }
  const args = process.argv.slice(2);
  const checkOnly = args.includes("--self-check");
  const skipCheck = args.includes("--no-check");
  const unknown = args.filter((a) => a !== "--self-check" && a !== "--no-check");
  if (unknown.length > 0) fail(`未知参数：${unknown.join(" ")}`);
  if (checkOnly) {
    selfCheck();
    return;
  }
  await assemble({ check: !skipCheck });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => fail(error?.stack ?? String(error)));
}
