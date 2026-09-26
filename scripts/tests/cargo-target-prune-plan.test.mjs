import assert from "node:assert/strict";
import test from "node:test";

import {
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
} from "../cargo-target-prune-plan.mjs";

const HOUR = 60 * 60 * 1000;
const DAY = 24 * HOUR;
const NOW = Date.UTC(2026, 8, 26, 12);

test("reads the unit hash from every artifact name cargo derives from it", () => {
  const hash = "93988c8bb412d4a5";
  for (const name of [
    `mework-${hash}`,
    `libmework_lib-${hash}.rlib`,
    `libmework_lib-${hash}.rmeta`,
    `mework_lib-${hash}`,
    `mework_lib-${hash}.d`,
    `mework_lib-${hash}.07dxh4c5znba5js4n49kea9qn.1hymssg.rcgu.o`
  ]) {
    assert.equal(unitHashOf(name), hash, name);
  }
  // rustc names incremental directories after the crate's stable id, not the unit hash.
  assert.equal(unitHashOf("mework_lib-00murnpde43xv"), null);
  assert.equal(unitHashOf(".DS_Store"), null);
  assert.equal(unitHashOf("mework-browser-dev"), null);
});

test("only the hash files of a fingerprint count as a use", () => {
  for (const name of [
    "lib-mework_lib",
    "bin-mework",
    "test-lib-mework_lib",
    "test-integration-test-remote",
    "build-script-build-script-build",
    "run-build-script-build-script-build"
  ]) {
    assert.equal(isFingerprintHashFile(name), true, name);
  }
  for (const name of [
    // The dep-info mtime is what cargo compares source files against; it must never be re-armed.
    "dep-lib-mework_lib",
    "output-lib-mework_lib",
    "lib-mework_lib.json",
    "invoked.timestamp",
    ".DS_Store"
  ]) {
    assert.equal(isFingerprintHashFile(name), false, name);
  }
});

test("an atime past mtime is a use since arming; otherwise mtime is the last known use", () => {
  const used = { atimeMs: NOW - HOUR, mtimeMs: NOW - DAY };
  const armed = { atimeMs: NOW - DAY - 1000, mtimeMs: NOW - DAY };
  const fresh = { atimeMs: NOW - DAY, mtimeMs: NOW - DAY };
  assert.equal(hashFileLastUse(used, true), NOW - HOUR);
  assert.equal(hashFileLastUse(armed, true), NOW - DAY);
  assert.equal(hashFileLastUse(fresh, true), NOW - DAY);
  // Without working access times, a use cannot be told from a build.
  assert.equal(hashFileLastUse(used, false), NOW - DAY);

  assert.equal(needsArming(used, true), true);
  // APFS moves atime only while it is strictly older than mtime.
  assert.equal(needsArming(fresh, true), true);
  assert.equal(needsArming(armed, true), false);
  assert.equal(needsArming(used, false), false);
});

function item(label, idleMs, bytes, id = label) {
  return { label, lastUsedMs: NOW - idleMs, files: [{ id, bytes }] };
}

const OPTIONS = {
  nowMs: NOW,
  keepDays: 3,
  maxBytes: 10 * GIB,
  protectHours: 2
};

function labels(items) {
  return items.map((entry) => entry.label).sort();
}

test("deletes what has been idle longer than keepDays", () => {
  const plan = planPrune([item("recent", DAY, GIB), item("old", 4 * DAY, GIB)], OPTIONS);
  assert.deepEqual(labels(plan.keep), ["recent"]);
  assert.deepEqual(labels(plan.remove), ["old"]);
  assert.equal(plan.removedBytes, GIB);
});

test("over the cap, drops everything older than the first item that does not fit", () => {
  const plan = planPrune([
    item("a", 3 * HOUR, 4 * GIB),
    item("b", 4 * HOUR, 4 * GIB),
    item("c", 5 * HOUR, 4 * GIB),
    // Would still fit on its own, but keeping it would make the kept set a non-prefix.
    item("d", 6 * HOUR, GIB)
  ], OPTIONS);
  assert.deepEqual(labels(plan.keep), ["a", "b"]);
  assert.deepEqual(labels(plan.remove), ["c", "d"]);
  assert.equal(plan.keptBytes, 8 * GIB);
});

test("never deletes what was used within protectHours, whatever the size", () => {
  const plan = planPrune([
    item("building", 10 * 60 * 1000, 20 * GIB),
    item("yesterday", DAY, GIB)
  ], OPTIONS);
  assert.deepEqual(labels(plan.keep), ["building"]);
  assert.deepEqual(labels(plan.remove), ["yesterday"]);
});

test("protects everything used since the previous run, whose recorded use is only its first", () => {
  const plan = planPrune([
    item("first-used-after-last-run", 20 * HOUR, 20 * GIB),
    item("idle-since-last-run", 30 * HOUR, GIB)
  ], { ...OPTIONS, lastRunMs: NOW - DAY });
  assert.deepEqual(labels(plan.keep), ["first-used-after-last-run"]);
  assert.deepEqual(labels(plan.remove), ["idle-since-last-run"]);
});

test("counts a hard-linked file once, against the most recent item holding it", () => {
  const plan = planPrune([
    { label: "old-incremental", lastUsedMs: NOW - 5 * DAY, files: [{ id: "shared", bytes: 3 * GIB }] },
    {
      label: "live-executable",
      lastUsedMs: NOW - HOUR,
      files: [{ id: "shared", bytes: 3 * GIB }, { id: "own", bytes: GIB }]
    }
  ], OPTIONS);
  assert.deepEqual(labels(plan.keep), ["live-executable"]);
  assert.equal(plan.keptBytes, 4 * GIB);
  // Deleting the old link frees nothing while the live one still holds the file.
  assert.equal(plan.removedBytes, 0);
});

test("parses the command line", () => {
  assert.deepEqual(parsePruneArguments([]), {
    dryRun: false,
    keepDays: DEFAULT_KEEP_DAYS,
    maxGib: DEFAULT_MAX_GIB,
    protectHours: DEFAULT_PROTECT_HOURS,
    targetDir: null
  });
  assert.deepEqual(
    parsePruneArguments([
      "--dry-run",
      "--keep-days", "1",
      "--max-gb", "12.5",
      "--protect-hours", "0",
      "--target-dir", "src-tauri/target-memory-browser-e2e"
    ]),
    {
      dryRun: true,
      keepDays: 1,
      maxGib: 12.5,
      protectHours: 0,
      targetDir: "src-tauri/target-memory-browser-e2e"
    }
  );
  assert.throws(() => parsePruneArguments(["--all"]), /不支持的 prune:target 参数/u);
  assert.throws(() => parsePruneArguments(["--dry-run", "--dry-run"]), /不能重复/u);
  assert.throws(() => parsePruneArguments(["--keep-days"]), /缺少取值/u);
  assert.throws(() => parsePruneArguments(["--max-gb", "--dry-run"]), /缺少取值/u);
  assert.throws(() => parsePruneArguments(["--max-gb", "-1"]), /需要非负数/u);
  assert.throws(() => parsePruneArguments(["--keep-days", "soon"]), /需要非负数/u);
});
