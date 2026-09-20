import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  DEV_APPLICATION_PATH_ENVIRONMENT_NAME,
  devApplicationPath,
  devChildEnvironment,
  sanitizeDevChildEnvironment
} from "../dev-child-environment.mjs";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  ".."
);

function runNode(code, environment) {
  const child = spawn(process.execPath, ["-e", code], {
    cwd: root,
    env: environment,
    stdio: ["ignore", "pipe", "pipe"]
  });
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => resolve({ code, stdout, stderr }));
  });
}

test("the inherited FORCE_COLOR never reaches a development child", () => {
  const base = {
    Path: "C:\\WINDOWS\\system32",
    NO_COLOR: undefined,
    CARGO_TARGET_DIR: "C:\\build\\target",
    MEWORK_BROWSER_DEV_TOKEN: "token",
    npm_execpath: "C:\\npm\\npm-cli.js"
  };
  delete base.NO_COLOR;
  for (const value of ["1", "2", "3", "0", "", "true"]) {
    const input = { ...base, FORCE_COLOR: value };
    const sanitized = sanitizeDevChildEnvironment(input);
    assert.equal(Object.hasOwn(sanitized, "FORCE_COLOR"), false, `FORCE_COLOR=${value}`);
    // The input object is a caller's environment — often `process.env` — so it
    // must come back untouched.
    assert.equal(input.FORCE_COLOR, value);
  }
  // Windows treats environment names case-insensitively; an exact-name delete
  // would let these spellings through.
  for (const name of ["force_color", "Force_Color", "FoRcE_cOlOr"]) {
    const sanitized = sanitizeDevChildEnvironment({ ...base, [name]: "1" });
    assert.deepEqual(Object.keys(sanitized).filter(
      (key) => key.toLowerCase() === "force_color"
    ), []);
  }
  // Everything else — including the build PATH, the cargo target directory and
  // the browser-dev bridge wiring — is passed through unchanged.
  assert.deepEqual(sanitizeDevChildEnvironment(base), base);
  assert.deepEqual(
    sanitizeDevChildEnvironment({ ...base, NO_COLOR: "1", FORCE_COLOR: "1" }),
    { ...base, NO_COLOR: "1" }
  );
  assert.deepEqual(sanitizeDevChildEnvironment({}), {});
});

test("a sanitized environment stops Node from colouring its own output", async () => {
  // The regression this pins is not about the helper's return value but about
  // what a child actually prints: `console.log` of a non-string uses
  // `util.inspect`, which emits ANSI whenever the stream reports colour support,
  // and `FORCE_COLOR` makes it report support even through a pipe.
  const code = "console.log({ n: 1 });";
  const inherited = { ...process.env, FORCE_COLOR: "1" };
  delete inherited.NO_COLOR;
  const polluted = await runNode(code, inherited);
  assert.equal(polluted.code, 0, polluted.stderr);
  assert.equal(
    polluted.stdout.includes("\u001b["),
    true,
    `对照组必须真的带 ANSI，否则本回归钉是假绿：${JSON.stringify(polluted.stdout)}`
  );

  const sanitized = sanitizeDevChildEnvironment(inherited);
  const clean = await runNode(code, sanitized);
  assert.equal(clean.code, 0, clean.stderr);
  assert.equal(clean.stdout.includes("\u001b["), false, JSON.stringify(clean.stdout));
  assert.equal(clean.stdout.trim(), "{ n: 1 }");

  const reported = await runNode(
    "process.stdout.write(String(process.env.FORCE_COLOR));",
    sanitized
  );
  assert.equal(reported.stdout, "undefined");
});

test("both development launchers sanitize before they spawn anything", () => {
  for (const relativePath of [
    "scripts/browser-dev.mjs",
    "scripts/tauri-with-build-tools.mjs"
  ]) {
    const source = readFileSync(path.join(root, relativePath), "utf8");
    assert.match(
      source,
      /import \{ devChildEnvironment \} from "\.\/dev-child-environment\.mjs";/,
      relativePath
    );
    // The build environment is only ever consumed through the helper, so a
    // future edit cannot reintroduce a raw `windowsNativeBuildEnvironment()`
    // spread into a child environment.
    assert.match(
      source,
      /devChildEnvironment\(\{\s*buildEnvironment: windowsNativeBuildEnvironment\(\)\s*\}\)/,
      relativePath
    );
    assert.equal(
      /(?<!buildEnvironment: )windowsNativeBuildEnvironment\(\)/.test(
        source.replace(/^import .*$/gm, "")
      ),
      false,
      `${relativePath} 仍有未经开发子进程环境处理的构建环境用法`
    );
  }
});

const SYSTEM32 = "C:\\WINDOWS\\system32";
const MINGW_BIN = "C:\\msys64\\mingw64\\bin";
const USR_BIN = "C:\\msys64\\usr\\bin";
const NPM_BIN = "C:\\repo\\node_modules\\.bin";

test("the application PATH keeps System32 ahead of the MSYS shadows", () => {
  const applicationPath = [NPM_BIN, SYSTEM32].join(";");
  const buildPath = [MINGW_BIN, USR_BIN, NPM_BIN, SYSTEM32].join(";");
  const restored = devApplicationPath(applicationPath, buildPath);
  // `<msys2>\usr\bin` ships `cmd`, `find`, `sort`, `more`, `link` and `tar`, so
  // whichever directory comes first decides what a shell the app opens runs.
  assert.equal(restored, [NPM_BIN, SYSTEM32, MINGW_BIN, USR_BIN].join(";"));
  const entries = restored.split(";");
  assert.equal(entries.indexOf(SYSTEM32) < entries.indexOf(USR_BIN), true);
  // Appended, never dropped: a GNU-target binary still finds its runtime DLLs.
  assert.equal(entries.includes(MINGW_BIN), true);

  // Case and quoting differences must not duplicate an entry.
  assert.equal(
    devApplicationPath(
      [NPM_BIN, SYSTEM32].join(";"),
      [MINGW_BIN, "c:\\windows\\SYSTEM32", NPM_BIN].join(";")
    ),
    [NPM_BIN, SYSTEM32, MINGW_BIN].join(";")
  );
  // Nothing was injected, so there is nothing to hand over.
  assert.equal(devApplicationPath(applicationPath, applicationPath), undefined);
  assert.equal(devApplicationPath(applicationPath, NPM_BIN), undefined);
  assert.equal(devApplicationPath(undefined, buildPath), undefined);
  assert.equal(devApplicationPath(applicationPath, undefined), undefined);
});

test("the launcher hands the application PATH over without touching the build PATH", () => {
  const applicationEnvironment = { Path: [NPM_BIN, SYSTEM32].join(";") };
  const buildEnvironment = {
    Path: [MINGW_BIN, USR_BIN, NPM_BIN, SYSTEM32].join(";"),
    FORCE_COLOR: "1",
    CARGO_TARGET_DIR: "C:\\build\\target"
  };
  const child = devChildEnvironment({
    buildEnvironment,
    applicationEnvironment,
    platform: "win32"
  });
  // cargo still compiles with the toolchain first; only the handoff differs.
  assert.equal(child.Path, buildEnvironment.Path);
  assert.equal(child.CARGO_TARGET_DIR, "C:\\build\\target");
  assert.equal(Object.hasOwn(child, "FORCE_COLOR"), false);
  assert.equal(
    child[DEV_APPLICATION_PATH_ENVIRONMENT_NAME],
    [NPM_BIN, SYSTEM32, MINGW_BIN, USR_BIN].join(";")
  );
  assert.deepEqual(buildEnvironment, {
    Path: [MINGW_BIN, USR_BIN, NPM_BIN, SYSTEM32].join(";"),
    FORCE_COLOR: "1",
    CARGO_TARGET_DIR: "C:\\build\\target"
  });

  // No toolchain injection, no marker.
  assert.equal(
    Object.hasOwn(
      devChildEnvironment({
        buildEnvironment: { Path: applicationEnvironment.Path },
        applicationEnvironment,
        platform: "win32"
      }),
      DEV_APPLICATION_PATH_ENVIRONMENT_NAME
    ),
    false
  );
  // Non-Windows never injects a toolchain, and an inherited marker from an outer
  // launcher must not survive into this run.
  for (const platform of ["linux", "darwin", "win32"]) {
    const nested = devChildEnvironment({
      buildEnvironment: {
        PATH: applicationEnvironment.Path,
        [DEV_APPLICATION_PATH_ENVIRONMENT_NAME]: "C:\\stale",
        mework_dev_application_path: "C:\\stale-too"
      },
      applicationEnvironment,
      platform
    });
    assert.deepEqual(
      Object.keys(nested).filter(
        (key) => key.toLowerCase() === DEV_APPLICATION_PATH_ENVIRONMENT_NAME.toLowerCase()
      ),
      [],
      platform
    );
  }
});

test("the Rust entry points consume the handoff before anything is spawned", () => {
  const rust = readFileSync(
    path.join(root, "src-tauri", "src", "child_environment.rs"),
    "utf8"
  );
  // One source of truth for the variable name across the launcher and the host.
  assert.match(
    rust,
    new RegExp(
      `DEV_APPLICATION_PATH_ENVIRONMENT_NAME: &str = "${DEV_APPLICATION_PATH_ENVIRONMENT_NAME}"`
    )
  );
  const lib = readFileSync(path.join(root, "src-tauri", "src", "lib.rs"), "utf8");
  for (const entry of ["pub fn run\\(\\)", "pub fn run_browser_dev\\(\\) -> i32"]) {
    const body = new RegExp(`${entry} \\{[\\s\\S]{0,400}?restore_dev_application_path\\(\\);`);
    assert.match(lib, body, entry);
  }
});
