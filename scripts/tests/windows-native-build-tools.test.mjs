import assert from "node:assert/strict";
import test from "node:test";
import { win32 } from "node:path";
import {
  discoverMsys2Roots,
  prepareWindowsNativeBuildEnvironment,
  windowsGnuTargetIsNative,
} from "../windows-native-build-tools.mjs";

const root = "C:\\msys64";
const mingwBin = win32.join(root, "mingw64", "bin");
const usrBin = win32.join(root, "usr", "bin");
const existingPath = `${usrBin};C:\\Windows\\System32;${mingwBin}`;
const existingDirectories = new Set([mingwBin, usrBin].map((value) => value.toLowerCase()));

function mockRun({
  target = "x86_64-w64-mingw32",
  failedTools = [],
  resolvedGcc = win32.join(mingwBin, "gcc.exe"),
} = {}) {
  const failed = new Set(failedTools);
  return (command, args) => {
    if (failed.has(command)) {
      return { status: 1, stdout: "", stderr: `${command} unavailable` };
    }
    if (command === "gcc.exe") {
      assert.deepEqual(args, ["-dumpmachine"]);
      return { status: 0, stdout: `${target}\r\n`, stderr: "" };
    }
    if (command === "make.exe" || command === "perl.exe") {
      return { status: 0, stdout: `${command} test version\r\n`, stderr: "" };
    }
    if (command === "where.exe") {
      const resolved = {
        "gcc.exe": resolvedGcc,
        "make.exe": win32.join(usrBin, "make.exe"),
        "perl.exe": win32.join(usrBin, "perl.exe"),
      }[args[0]];
      return resolved
        ? { status: 0, stdout: `${resolved}\r\n`, stderr: "" }
        : { status: 1, stdout: "", stderr: "not found" };
    }
    throw new Error(`unexpected command: ${command}`);
  };
}

test("puts native mingw gcc before usr tools and validates the target", () => {
  const { environment, toolchain } = prepareWindowsNativeBuildEnvironment({
    environment: {
      Path: existingPath,
      MSYS2_ROOT: root,
      CARGO_TARGET_DIR: "C:\\workspace\\target-image-input-e2e",
      MEWORK_IMAGE_INPUT_E2E_RUN_ID: "0123456789abcdef01234567",
    },
    platform: "win32",
    roots: [root],
    exists: (candidate) => existingDirectories.has(candidate.toLowerCase()),
    run: mockRun(),
  });

  assert.deepEqual(environment.Path.split(";").slice(0, 3), [
    mingwBin,
    usrBin,
    "C:\\Windows\\System32",
  ]);
  assert.equal(toolchain.gccTarget, "x86_64-w64-mingw32");
  assert.equal(toolchain.gccPath, win32.join(mingwBin, "gcc.exe"));
  assert.equal(toolchain.makePath, win32.join(usrBin, "make.exe"));
  assert.equal(toolchain.perlPath, win32.join(usrBin, "perl.exe"));
  assert.equal(environment.CARGO_TARGET_DIR, "C:\\workspace\\target-image-input-e2e");
  assert.equal(environment.MEWORK_IMAGE_INPUT_E2E_RUN_ID, "0123456789abcdef01234567");
});

test("discovers one MSYS2 root even when usr bin precedes mingw bin", () => {
  assert.deepEqual(discoverMsys2Roots({ PATH: existingPath }).slice(0, 1), [root]);
});

test("fails clearly when a required build tool is unavailable", () => {
  assert.throws(
    () =>
      prepareWindowsNativeBuildEnvironment({
        environment: { Path: existingPath },
        platform: "win32",
        roots: [root],
        exists: (candidate) => existingDirectories.has(candidate.toLowerCase()),
        run: mockRun({ failedTools: ["make.exe"] }),
      }),
    (error) => {
      assert.match(error.message, /Windows GNU 原生构建工具检查失败/);
      assert.match(error.message, /make/);
      assert.match(error.message, /MSYS2_ROOT/);
      return true;
    },
  );
});

test("rejects a Cygwin gcc target even when gcc itself runs", () => {
  assert.throws(
    () =>
      prepareWindowsNativeBuildEnvironment({
        environment: { Path: existingPath },
        platform: "win32",
        roots: [root],
        exists: (candidate) => existingDirectories.has(candidate.toLowerCase()),
        run: mockRun({ target: "x86_64-pc-cygwin" }),
      }),
    (error) => {
      assert.match(error.message, /x86_64-pc-cygwin/);
      assert.match(error.message, /mingw 或 windows/);
      return true;
    },
  );
});

test("rejects a gcc path that still resolves outside the selected mingw bin", () => {
  assert.throws(
    () =>
      prepareWindowsNativeBuildEnvironment({
        environment: { Path: existingPath },
        platform: "win32",
        roots: [root],
        exists: (candidate) => existingDirectories.has(candidate.toLowerCase()),
        run: mockRun({ resolvedGcc: win32.join(usrBin, "gcc.exe") }),
      }),
    /而不是优先使用/,
  );
});

test("does not probe or alter build tools on non-Windows platforms", () => {
  let called = false;
  const original = { PATH: "/usr/local/bin:/usr/bin", CUSTOM: "value" };
  const result = prepareWindowsNativeBuildEnvironment({
    environment: original,
    platform: "linux",
    run: () => {
      called = true;
      throw new Error("must not run");
    },
  });
  assert.deepEqual(result, { environment: original, toolchain: undefined });
  assert.equal(called, false);
  assert.notEqual(result.environment, original);
});

test("accepts MinGW and Windows GNU target spellings only", () => {
  assert.equal(windowsGnuTargetIsNative("x86_64-w64-mingw32"), true);
  assert.equal(windowsGnuTargetIsNative("x86_64-pc-windows-gnu"), true);
  assert.equal(windowsGnuTargetIsNative("x86_64-pc-cygwin"), false);
});
