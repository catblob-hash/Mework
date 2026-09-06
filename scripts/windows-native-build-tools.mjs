import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve, win32 } from "node:path";
import { pathToFileURL } from "node:url";

const WINDOWS_GNU_TARGET_PATTERN = /(mingw|windows)/i;
const defaultMsys2Roots = ["C:\\msys64", "C:\\tools\\msys64"];

function environmentValue(environment, name) {
  const key = Object.keys(environment).find((candidate) => candidate.toLowerCase() === name);
  return key ? environment[key] : undefined;
}

function pathEnvironmentKey(environment) {
  return Object.keys(environment).find((key) => key.toLowerCase() === "path") ?? "Path";
}

function normalizeWindowsPath(value) {
  const trimmed = value?.trim().replace(/^"(.*)"$/, "$1");
  return trimmed ? win32.normalize(trimmed) : undefined;
}

function inferredMsys2Root(pathEntry) {
  const normalized = normalizeWindowsPath(pathEntry);
  if (!normalized || win32.basename(normalized).toLowerCase() !== "bin") return undefined;
  const parent = win32.dirname(normalized);
  const parentName = win32.basename(parent).toLowerCase();
  if (parentName !== "mingw64" && parentName !== "usr") return undefined;
  return win32.dirname(parent);
}

function uniqueWindowsPaths(values) {
  const seen = new Set();
  const result = [];
  for (const value of values) {
    const normalized = normalizeWindowsPath(value);
    if (!normalized) continue;
    const key = normalized.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(normalized);
  }
  return result;
}

export function windowsGnuTargetIsNative(target) {
  return WINDOWS_GNU_TARGET_PATTERN.test(target.trim());
}

export function discoverMsys2Roots(environment) {
  const pathValue = environmentValue(environment, "path") ?? "";
  const explicitRoot = environmentValue(environment, "msys2_root");
  return uniqueWindowsPaths([
    explicitRoot,
    ...pathValue.split(";").map(inferredMsys2Root),
    ...defaultMsys2Roots,
  ]);
}

function environmentWithMsys2Root(environment, root) {
  const result = { ...environment };
  const pathKey = pathEnvironmentKey(result);
  const mingwBin = win32.join(root, "mingw64", "bin");
  const usrBin = win32.join(root, "usr", "bin");
  const selectedBins = new Set([mingwBin, usrBin].map((value) => value.toLowerCase()));
  const existing = (result[pathKey] ?? "")
    .split(";")
    .map(normalizeWindowsPath)
    .filter(Boolean)
    .filter((entry) => !selectedBins.has(entry.toLowerCase()));
  result[pathKey] = [mingwBin, usrBin, ...existing].join(";");

  // A Windows environment is case-insensitive, but a JavaScript object is not. Keep one PATH key
  // so child-process resolution cannot depend on duplicate `PATH`/`Path` insertion order.
  for (const key of Object.keys(result)) {
    if (key !== pathKey && key.toLowerCase() === "path") delete result[key];
  }
  return { environment: result, mingwBin, usrBin };
}

function runProbe(run, command, args, environment) {
  let result;
  try {
    result = run(command, args, {
      env: environment,
      encoding: "utf8",
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    return {
      ok: false,
      detail: error instanceof Error ? error.message : String(error),
      stdout: "",
    };
  }
  const stdout = typeof result.stdout === "string" ? result.stdout.trim() : "";
  const stderr = typeof result.stderr === "string" ? result.stderr.trim() : "";
  return {
    ok: result.status === 0 && !result.error,
    detail: result.error?.message || stderr.split(/\r?\n/, 1)[0] || `退出码 ${result.status ?? "未知"}`,
    stdout,
  };
}

function firstResolvedCommandPath(run, command, environment) {
  const result = runProbe(run, "where.exe", [command], environment);
  if (!result.ok) return undefined;
  return normalizeWindowsPath(result.stdout.split(/\r?\n/, 1)[0]);
}

function inspectWindowsBuildTools(environment, run, expectedMingwBin) {
  const gcc = runProbe(run, "gcc.exe", ["-dumpmachine"], environment);
  const make = runProbe(run, "make.exe", ["--version"], environment);
  const perl = runProbe(run, "perl.exe", ["--version"], environment);
  const missing = [
    ["gcc", gcc],
    ["make", make],
    ["perl", perl],
  ].filter(([, probe]) => !probe.ok);
  if (missing.length > 0) {
    throw new Error(
      `缺少或无法运行 ${missing.map(([name, probe]) => `${name}（${probe.detail}）`).join("、")}`,
    );
  }
  if (!gcc.stdout || !windowsGnuTargetIsNative(gcc.stdout)) {
    throw new Error(
      `gcc -dumpmachine 返回 ${JSON.stringify(gcc.stdout || "<empty>")}；目标必须包含 mingw 或 windows`,
    );
  }

  const gccPath = firstResolvedCommandPath(run, "gcc.exe", environment);
  if (expectedMingwBin && gccPath) {
    const relative = win32.relative(expectedMingwBin, gccPath);
    if (relative.startsWith("..") || win32.isAbsolute(relative)) {
      throw new Error(
        `gcc 解析为 ${gccPath}，而不是优先使用 ${expectedMingwBin} 中的原生编译器`,
      );
    }
  }
  return {
    gccTarget: gcc.stdout,
    gccPath,
    makePath: firstResolvedCommandPath(run, "make.exe", environment),
    perlPath: firstResolvedCommandPath(run, "perl.exe", environment),
  };
}

export function prepareWindowsNativeBuildEnvironment({
  environment = process.env,
  platform = process.platform,
  exists = existsSync,
  run = spawnSync,
  roots,
} = {}) {
  const original = { ...environment };
  if (platform !== "win32") return { environment: original, toolchain: undefined };

  const attempts = [];
  let foundMsys2Layout = false;
  for (const root of uniqueWindowsPaths(roots ?? discoverMsys2Roots(original))) {
    const mingwBin = win32.join(root, "mingw64", "bin");
    const usrBin = win32.join(root, "usr", "bin");
    const absentDirectories = [
      !exists(mingwBin) && "mingw64\\bin",
      !exists(usrBin) && "usr\\bin",
    ].filter(Boolean);
    if (absentDirectories.length > 0) {
      attempts.push(`${root}: 缺少 ${absentDirectories.join("、")}`);
      continue;
    }
    foundMsys2Layout = true;

    const candidate = environmentWithMsys2Root(original, root);
    try {
      const inspection = inspectWindowsBuildTools(candidate.environment, run, candidate.mingwBin);
      return {
        environment: candidate.environment,
        toolchain: {
          root,
          mingwBin: candidate.mingwBin,
          usrBin: candidate.usrBin,
          ...inspection,
        },
      };
    } catch (error) {
      attempts.push(`${root}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  // Preserve compatibility with an already-correct non-MSYS2 Windows GNU environment, but never
  // silently fall back after finding an MSYS2 layout whose gcc still resolves incorrectly.
  if (!foundMsys2Layout) {
    try {
      const inspection = inspectWindowsBuildTools(original, run);
      return { environment: original, toolchain: { root: undefined, ...inspection } };
    } catch (error) {
      attempts.push(`现有 PATH: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  throw new Error(
    [
      "Windows GNU 原生构建工具检查失败。",
      ...attempts.map((attempt) => `- ${attempt}`),
      "需要原生 mingw64 gcc，以及可执行的 make/perl；mingw64\\bin 必须排在 usr\\bin 前。",
      "请安装 MSYS2，或把 MSYS2_ROOT 指向同时包含 mingw64\\bin 与 usr\\bin 的安装目录。",
    ].join("\n"),
  );
}

export function windowsNativeBuildEnvironment(options) {
  return prepareWindowsNativeBuildEnvironment(options).environment;
}

const invokedDirectly =
  process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (invokedDirectly) {
  if (!process.argv.includes("--self-check")) {
    console.error("用法：node scripts/windows-native-build-tools.mjs --self-check");
    process.exitCode = 2;
  } else {
    try {
      const { toolchain } = prepareWindowsNativeBuildEnvironment();
      if (!toolchain) {
        console.log("[build-tools] 非 Windows 平台，无需 MSYS2 工具链");
      } else {
        console.log(
          `[build-tools] OK gcc=${toolchain.gccPath ?? "PATH"} target=${toolchain.gccTarget}`
          + ` make=${toolchain.makePath ?? "PATH"} perl=${toolchain.perlPath ?? "PATH"}`,
        );
      }
    } catch (error) {
      console.error(error instanceof Error ? error.message : String(error));
      process.exitCode = 1;
    }
  }
}
