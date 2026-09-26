// Selection logic for scripts/prune-cargo-target.mjs, kept free of file-system access so it
// can be tested against synthetic inventories.
//
// Cargo never deletes anything from a target directory. Every change that reaches a unit's
// `-C metadata` hash — features, profile, test versus lib, the resolved dependency graph, the
// compiler — produces a new `<name>-<hash>` set of artifacts beside the old one, and on macOS a
// debug executable additionally keeps each codegen unit's object file in `deps/` for the
// debugger. A few weeks of feature flips and lockfile changes adds up to well over 100 GB.
//
// How a unit's last use is read: on every build, fresh or not, cargo reads the hash file in
// `.fingerprint/<name>-<hash>/` of every unit in the graph, but never the artifacts. APFS,
// however, moves a file's atime on read only while that atime is older than its mtime, so after
// the first read following a build it stops moving. Each run therefore re-arms the hash files it
// keeps: mtime becomes the last use it knows of and atime one second earlier, and cargo's next
// read pushes atime past mtime again. Reading back, atime past mtime is a use since the last run;
// anything else leaves mtime as the last known use. The hash file's mtime is not an input to
// freshness — cargo's own `-Zmtime-on-use` rewrites it on every use for the same purpose — while
// the `dep-*` dep-info beside it is the reference source files are compared against, so that
// one is never touched.

/** A unit's `-C extra-filename` hash in a fingerprint, `build/`, `deps/` or `examples/` name. */
const UNIT_HASH = /-([0-9a-f]{16})(?:\.|$)/u;

export const GIB = 1024 ** 3;
const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;
export const ARM_OFFSET_MS = 1000;

export const DEFAULT_KEEP_DAYS = 3;
export const DEFAULT_MAX_GIB = 30;
export const DEFAULT_PROTECT_HOURS = 2;

export function unitHashOf(name) {
  return UNIT_HASH.exec(name)?.[1] ?? null;
}

/**
 * `lib-*`, `bin-*`, `test-lib-*`, `run-build-script-*` …: the files cargo reads on every build.
 * Dotfiles are Finder's `.DS_Store`, written whenever someone browses the directory.
 */
export function isFingerprintHashFile(name) {
  return !name.startsWith(".")
    && !name.endsWith(".json")
    && !name.startsWith("dep-")
    && !name.startsWith("output-")
    && name !== "invoked.timestamp";
}

/** Last known use of a hash file, as the arming scheme above records it. */
export function hashFileLastUse({ atimeMs, mtimeMs }, atimeTracked) {
  return atimeTracked && atimeMs > mtimeMs ? atimeMs : mtimeMs;
}

/** Whether a kept hash file needs re-arming so the next read by cargo registers. */
export function needsArming({ atimeMs, mtimeMs }, atimeTracked) {
  return atimeTracked && atimeMs >= mtimeMs;
}

/**
 * Decides which items to delete. Each item is `{ lastUsedMs, files: [{ id, bytes }] }`, where
 * `id` identifies the underlying file so hard links — rustc links object files between
 * `incremental/` and `deps/` — are counted once.
 *
 * Protected, whatever the size: anything used within `protectHours`, and anything used since
 * the previous run (`lastRunMs`). The recorded use of the latter is only its first use in that
 * interval, so ranking inside it would evict units still in use.
 *
 * The rest is walked from most to least recently used and kept while it is younger than
 * `keepDays` and everything kept so far fits in `maxBytes`. Once the cap is reached every older
 * item goes, so the kept set is always a prefix and the bytes attributed to removed items are
 * really freed.
 */
export function planPrune(items, { nowMs, keepDays, maxBytes, protectHours, lastRunMs = null }) {
  const protectFromMs = Math.min(nowMs - protectHours * HOUR_MS, lastRunMs ?? Infinity);
  const ordered = [...items].sort((a, b) => b.lastUsedMs - a.lastUsedMs);
  const seen = new Set();
  const keep = [];
  const remove = [];
  let keptBytes = 0;
  let removedBytes = 0;
  let full = false;
  for (const item of ordered) {
    let bytes = 0;
    for (const file of item.files) {
      if (seen.has(file.id)) continue;
      seen.add(file.id);
      bytes += file.bytes;
    }
    const isProtected = item.lastUsedMs >= protectFromMs;
    if (!isProtected && keptBytes + bytes > maxBytes) full = true;
    if (isProtected || (!full && nowMs - item.lastUsedMs < keepDays * DAY_MS)) {
      keep.push({ ...item, bytes });
      keptBytes += bytes;
    } else {
      remove.push({ ...item, bytes });
      removedBytes += bytes;
    }
  }
  return { keep, remove, keptBytes, removedBytes };
}

export function parsePruneArguments(argv) {
  const options = {
    dryRun: false,
    keepDays: DEFAULT_KEEP_DAYS,
    maxGib: DEFAULT_MAX_GIB,
    protectHours: DEFAULT_PROTECT_HOURS,
    targetDir: null
  };
  const valued = {
    "--keep-days": "keepDays",
    "--max-gb": "maxGib",
    "--protect-hours": "protectHours",
    "--target-dir": "targetDir"
  };
  const given = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument !== "--dry-run" && !(argument in valued)) {
      throw new Error(`不支持的 prune:target 参数：${argument}`);
    }
    if (given.has(argument)) throw new Error(`${argument} 不能重复`);
    given.add(argument);
    if (argument === "--dry-run") {
      options.dryRun = true;
      continue;
    }
    const value = argv[index + 1];
    index += 1;
    if (value === undefined || value.startsWith("--")) throw new Error(`${argument} 缺少取值`);
    if (argument === "--target-dir") {
      options.targetDir = value;
      continue;
    }
    const number = Number(value);
    if (!Number.isFinite(number) || number < 0) throw new Error(`${argument} 需要非负数：${value}`);
    options[valued[argument]] = number;
  }
  return options;
}
