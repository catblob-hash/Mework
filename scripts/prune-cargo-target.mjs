#!/usr/bin/env node
// Deletes the build units no cargo invocation has used for a while from a cargo target
// directory. Why cargo needs this and how a unit's last use is read:
// scripts/cargo-target-prune-plan.mjs.
//
// Usage:
//   npm run prune:target                              # src-tauri/target
//   npm run prune:target -- --dry-run
//   npm run prune:target -- --keep-days 1 --max-gb 20
//   npm run prune:target -- --target-dir src-tauri/target-memory-browser-e2e
//
// The cargo wrappers (tauri-with-build-tools, cargo-test, browser-dev) call autoPruneCargoTarget()
// before starting cargo, at most once every AUTO_INTERVAL_HOURS per target directory.
// MEWORK_CARGO_PRUNE=off turns that off.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  ARM_OFFSET_MS,
  DEFAULT_KEEP_DAYS,
  DEFAULT_MAX_GIB,
  DEFAULT_PROTECT_HOURS,
  GIB,
  hashFileLastUse,
  isFingerprintHashFile,
  needsArming,
  parsePruneArguments,
  planPrune,
  unitHashOf
} from "./cargo-target-prune-plan.mjs";

const label = "[prune:target]";
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_TARGET_DIR = path.join(repoRoot, "src-tauri", "target");

const AUTO_INTERVAL_HOURS = 4;
/** Its mtime is when the last run armed the hash files; see planPrune's `lastRunMs`. */
const RUN_STAMP = ".mework-prune-stamp";

function entries(directory) {
  try {
    return fs.readdirSync(directory, { withFileTypes: true });
  } catch {
    return [];
  }
}

/** `{ id, bytes }` for every file under `root`, which may itself be a file. */
function filesUnder(root, into = []) {
  let stat;
  try {
    stat = fs.lstatSync(root);
  } catch {
    return into;
  }
  if (stat.isDirectory()) {
    for (const entry of entries(root)) filesUnder(path.join(root, entry.name), into);
  } else {
    // Allocated blocks rather than length, so sparse and compressed files count what they
    // occupy; Windows reports no blocks.
    into.push({ id: `${stat.dev}:${stat.ino}`, bytes: stat.blocks ? stat.blocks * 512 : stat.size });
  }
  return into;
}

/** `target/<profile>` and `target/<triple>/<profile>`: every directory holding a `.fingerprint`. */
function profileDirectories(targetDir) {
  const result = [];
  for (const first of entries(targetDir)) {
    if (!first.isDirectory()) continue;
    const firstPath = path.join(targetDir, first.name);
    if (fs.existsSync(path.join(firstPath, ".fingerprint"))) {
      result.push(firstPath);
      continue;
    }
    for (const second of entries(firstPath)) {
      const secondPath = path.join(firstPath, second.name);
      if (second.isDirectory() && fs.existsSync(path.join(secondPath, ".fingerprint"))) {
        result.push(secondPath);
      }
    }
  }
  return result;
}

/**
 * Whether reading an armed file moves its access time here. NTFS volumes often have
 * last-access updates disabled and a `noatime` mount has none; without them only mtime is
 * left, which says when a unit was last built rather than last used.
 */
export function atimeIsTracked(targetDir) {
  const probe = path.join(targetDir, `.prune-atime-probe-${process.pid}`);
  try {
    fs.writeFileSync(probe, "probe");
    const mtimeMs = Date.now() - 7 * 24 * 60 * 60 * 1000;
    fs.utimesSync(probe, new Date(mtimeMs - ARM_OFFSET_MS), new Date(mtimeMs));
    fs.readFileSync(probe);
    return fs.statSync(probe).atimeMs > mtimeMs;
  } catch {
    return false;
  } finally {
    fs.rmSync(probe, { force: true });
  }
}

/**
 * Deleting under a running build fails that build, and its units' atimes cannot say they are
 * in use (see the plan module), so any cargo or rustc on the machine postpones the run.
 */
export function compilerIsRunning() {
  try {
    if (process.platform === "win32") {
      const output = execFileSync("tasklist.exe", ["/fo", "csv", "/nh"], {
        encoding: "utf8",
        windowsHide: true
      });
      return output
        .split(/\r?\n/)
        .some((line) => /^"(cargo|rustc)\.exe"/iu.test(line.trim()));
    }
    const output = execFileSync("ps", ["-axo", "comm="], { encoding: "utf8" });
    return output
      .split(/\r?\n/)
      .some((line) => /^(cargo|rustc)$/u.test(path.basename(line.trim())));
  } catch {
    // Unknown is treated as busy: skipping a run costs nothing.
    return true;
  }
}

function inventory(targetDir, atimeTracked) {
  const items = [];
  for (const profileDir of profileDirectories(targetDir)) {
    const units = new Map();
    const unit = (hash, name) => {
      if (!units.has(hash)) {
        units.set(hash, { name, fingerprinted: false, lastUsedMs: 0, hashFiles: [], paths: [] });
      }
      return units.get(hash);
    };

    for (const entry of entries(path.join(profileDir, ".fingerprint"))) {
      const hash = unitHashOf(entry.name);
      if (!hash || !entry.isDirectory()) continue;
      const directory = path.join(profileDir, ".fingerprint", entry.name);
      const current = unit(hash, entry.name);
      current.fingerprinted = true;
      current.paths.push(directory);
      let newestMtimeMs = 0;
      let hashFileSeen = false;
      for (const file of entries(directory)) {
        if (file.name.startsWith(".")) continue;
        const filePath = path.join(directory, file.name);
        let stat;
        try {
          stat = fs.statSync(filePath);
        } catch {
          continue; // Replaced underneath us by a concurrent cargo.
        }
        newestMtimeMs = Math.max(newestMtimeMs, stat.mtimeMs);
        if (!isFingerprintHashFile(file.name)) continue;
        hashFileSeen = true;
        const times = { atimeMs: stat.atimeMs, mtimeMs: stat.mtimeMs };
        current.hashFiles.push({ path: filePath, ...times });
        current.lastUsedMs = Math.max(current.lastUsedMs, hashFileLastUse(times, atimeTracked));
      }
      // A unit cargo was interrupted in the middle of has no hash file yet.
      if (!hashFileSeen) current.lastUsedMs = Math.max(current.lastUsedMs, newestMtimeMs);
    }

    for (const subdirectory of ["deps", "build", "examples"]) {
      for (const entry of entries(path.join(profileDir, subdirectory))) {
        const hash = unitHashOf(entry.name);
        if (!hash) continue;
        const artifact = path.join(profileDir, subdirectory, entry.name);
        const current = unit(hash, entry.name);
        current.paths.push(artifact);
        // Artifacts whose fingerprint is gone are dated by their own build time.
        if (!current.fingerprinted) {
          try {
            current.lastUsedMs = Math.max(current.lastUsedMs, fs.lstatSync(artifact).mtimeMs);
          } catch {
            // Already gone.
          }
        }
      }
    }

    for (const current of units.values()) {
      items.push({
        label: path.join(path.relative(targetDir, profileDir), current.name),
        lastUsedMs: current.lastUsedMs,
        hashFiles: current.hashFiles,
        paths: current.paths
      });
    }

    // rustc names these after the crate's stable id rather than the unit hash, and writes a new
    // `s-*` session into one on every compile of that unit. The directory's own mtime is no
    // guide: Finder's `.DS_Store` moves it too.
    for (const entry of entries(path.join(profileDir, "incremental"))) {
      if (!entry.isDirectory()) continue;
      const directory = path.join(profileDir, "incremental", entry.name);
      const sessions = entries(directory).filter((e) => e.name.startsWith("s-"));
      let lastUsedMs = 0;
      for (const child of sessions.length > 0 ? sessions.map((e) => path.join(directory, e.name)) : [directory]) {
        try {
          lastUsedMs = Math.max(lastUsedMs, fs.lstatSync(child).mtimeMs);
        } catch {
          // Session directories come and go while rustc runs.
        }
      }
      items.push({
        label: path.join(path.relative(targetDir, profileDir), "incremental", entry.name),
        lastUsedMs,
        hashFiles: [],
        paths: [directory]
      });
    }
  }
  for (const item of items) item.files = item.paths.flatMap((p) => filesUnder(p));
  return items;
}

function formatGib(bytes) {
  return `${(bytes / GIB).toFixed(1)} GiB`;
}

/**
 * Plans and, unless `dryRun`, deletes and re-arms. A unit's fingerprint directory is the first of
 * its paths, so an interrupted run leaves cargo looking at a unit it never built rather than a
 * fresh fingerprint with missing outputs; both rebuild, the first without a confusing message.
 */
export function pruneCargoTarget(targetDir, {
  dryRun = false,
  keepDays = DEFAULT_KEEP_DAYS,
  maxGib = DEFAULT_MAX_GIB,
  protectHours = DEFAULT_PROTECT_HOURS,
  atimeTracked = atimeIsTracked(targetDir),
  nowMs = Date.now()
} = {}) {
  const stamp = path.join(targetDir, RUN_STAMP);
  const lastRunMs = fs.statSync(stamp, { throwIfNoEntry: false })?.mtimeMs ?? null;
  const items = inventory(targetDir, atimeTracked);
  const plan = planPrune(items, {
    nowMs,
    keepDays,
    maxBytes: maxGib * GIB,
    protectHours,
    lastRunMs: atimeTracked ? lastRunMs : null
  });
  if (!dryRun) {
    for (const item of plan.remove) {
      for (const target of item.paths) fs.rmSync(target, { recursive: true, force: true });
    }
    for (const item of plan.keep) {
      for (const file of item.hashFiles) {
        if (!needsArming(file, atimeTracked)) continue;
        const lastUseMs = hashFileLastUse(file, atimeTracked);
        try {
          fs.utimesSync(file.path, new Date(lastUseMs - ARM_OFFSET_MS), new Date(lastUseMs));
        } catch {
          // Rewritten or removed by cargo since the scan; its next read registers either way.
        }
      }
    }
    fs.writeFileSync(stamp, "");
    fs.utimesSync(stamp, new Date(nowMs), new Date(nowMs));
  }
  return { ...plan, atimeTracked };
}

/**
 * The wrappers' hook. Never throws and never blocks a build on a failure: at worst the target
 * directory keeps growing as it did before.
 */
export function autoPruneCargoTarget(targetDir = DEFAULT_TARGET_DIR, environment = process.env) {
  if (environment.MEWORK_CARGO_PRUNE === "off") return;
  try {
    if (!fs.existsSync(targetDir)) return;
    const last = fs.statSync(path.join(targetDir, RUN_STAMP), { throwIfNoEntry: false });
    if (last && Date.now() - last.mtimeMs < AUTO_INTERVAL_HOURS * 60 * 60 * 1000) return;
    // Pruning by build time alone would delete dependencies that are still in use but have not
    // needed a rebuild; an explicit `npm run prune:target` may accept that, a background hook not.
    if (!atimeIsTracked(targetDir) || compilerIsRunning()) return;
    // Before the first run nothing is armed, so a unit's recorded use is only its first read
    // after its build, and dependencies built days ago but read by every build since would go.
    // The first run therefore only arms; deleting starts once uses have been recorded.
    const result = pruneCargoTarget(
      targetDir,
      last ? { atimeTracked: true } : { atimeTracked: true, keepDays: Infinity, maxGib: Infinity }
    );
    if (result.remove.length > 0) {
      const shown = targetDir.startsWith(repoRoot + path.sep) ? path.relative(repoRoot, targetDir) : targetDir;
      console.log(
        `${label} ${shown}：清掉 ${result.remove.length} 项过期构建产物，`
          + `释放 ${formatGib(result.removedBytes)}，保留 ${formatGib(result.keptBytes)}`
      );
    }
  } catch (error) {
    console.warn(`${label} 自动清理跳过：${error.message}`);
  }
}

async function main() {
  let options;
  try {
    options = parsePruneArguments(process.argv.slice(2));
  } catch (error) {
    console.error(`${label} ${error.message}`);
    process.exit(1);
  }
  const targetDir = path.resolve(options.targetDir ?? DEFAULT_TARGET_DIR);
  if (!fs.existsSync(targetDir)) {
    console.log(`${label} ${targetDir} 不存在，无事可做`);
    return;
  }
  if (!options.dryRun && compilerIsRunning()) {
    console.error(`${label} 有 cargo 或 rustc 正在运行，删它正在用的产物会让那次构建失败；等它结束再清`);
    process.exit(1);
  }
  const atimeTracked = atimeIsTracked(targetDir);
  if (!atimeTracked) {
    console.warn(
      `${label} 这个文件系统读文件不更新访问时间，只能按构建时间判断；仍在用但 ${options.keepDays} 天没重编的依赖也会被删，下次构建重编`
    );
  } else if (!fs.existsSync(path.join(targetDir, RUN_STAMP))) {
    console.warn(
      `${label} 这个目录第一次清理，还没有使用记录，只知道每个单元构建后第一次被引用的时间；很早编译、至今仍在用的依赖也可能被删，下次构建重编一次`
    );
  }
  const result = pruneCargoTarget(targetDir, { ...options, atimeTracked });
  if (options.dryRun) {
    const largest = [...result.remove].sort((a, b) => b.bytes - a.bytes);
    for (const item of largest.slice(0, 20)) {
      const idleDays = ((Date.now() - item.lastUsedMs) / (24 * 60 * 60 * 1000)).toFixed(1);
      console.log(`  ${formatGib(item.bytes).padStart(9)}  闲置 ${idleDays.padStart(4)} 天  ${item.label}`);
    }
    if (largest.length > 20) console.log(`  …另 ${largest.length - 20} 项`);
  }
  console.log(
    `${label} ${options.dryRun ? "将删除" : "已删除"} ${result.remove.length} 项，${formatGib(result.removedBytes)}；`
      + `保留 ${result.keep.length} 项，${formatGib(result.keptBytes)}`
      + `（上次清理以来和 ${options.protectHours} 小时内用过的不动，超过 ${options.keepDays} 天没用的删，其余按最近使用保留到 ${options.maxGib} GiB）`
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await main();
}
