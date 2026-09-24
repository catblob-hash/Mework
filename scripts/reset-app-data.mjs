// One-click wipe of Mework's on-disk state, so the next launch starts from the
// seed document. The app ships no backward-compatibility migrations by design,
// which makes deleting the persisted document a required step after every
// schema bump rather than an occasional cleanup.
//
// All selection logic lives in scripts/reset-app-data-plan.mjs; this file only
// lists directories, prints the plan and deletes what the plan names.

import { execFileSync } from "node:child_process";
import { createInterface } from "node:readline/promises";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";

import {
  dataRootsFor,
  INTERACTIVE_DEV_IDENTIFIER,
  isMeworkExecutable,
  parseResetArguments,
  planCredentialTargets,
  planDataDirectories,
  planKeychainItems,
  summarizeCredentials
} from "./reset-app-data-plan.mjs";

const label = "[reset:data]";

function fail(message) {
  console.error(`${label} ${message}`);
  process.exit(1);
}

let options;
try {
  options = parseResetArguments(process.argv.slice(2));
} catch (error) {
  fail(error.message);
}

// The app holds document.v1.json under an exclusive instance lock; deleting the
// directory underneath a live process leaves it writing into an unlinked file.
function runningApplications() {
  if (process.platform !== "win32") {
    try {
      const output = execFileSync("ps", ["-axo", "comm="], { encoding: "utf8" });
      return [...new Set(
        output
          .split(/\r?\n/)
          .filter((line) => line.trim() && isMeworkExecutable(line))
          .map((line) => path.basename(line.trim()))
      )];
    } catch {
      return [];
    }
  }
  try {
    const output = execFileSync(
      "tasklist.exe",
      ["/fo", "csv", "/nh"],
      { encoding: "utf8", windowsHide: true }
    );
    return [...new Set(
      output
        .split(/\r?\n/)
        .map((line) => /^"([^"]+)"/u.exec(line.trim())?.[1])
        .filter((name) => name && /^mework(-browser-dev)?\.exe$/iu.test(name))
    )];
  } catch {
    return [];
  }
}

function listEntries(directory) {
  if (!directory) return [];
  try {
    return fs
      .readdirSync(directory, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name);
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

function directorySize(target) {
  let total = 0;
  const pending = [target];
  while (pending.length > 0) {
    const current = pending.pop();
    let entries;
    try {
      entries = fs.readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) {
        pending.push(full);
      } else if (entry.isFile()) {
        try {
          total += fs.statSync(full).size;
        } catch {
          // A file vanishing mid-scan only makes the reported size smaller.
        }
      }
    }
  }
  return total;
}

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`;
}

const home = process.env.USERPROFILE || process.env.HOME || os.homedir();
const roots = dataRootsFor(process.platform, process.env, home);
for (const root of roots) root.entries = listEntries(root.directory);

let directories;
try {
  directories = planDataDirectories(roots, options.scope);
} catch (error) {
  fail(error.message);
}

// On macOS every credential lives in ~/.mework/credential-vault, sealed under
// one login-keychain item (src-tauri/src/credential_vault.rs); older builds
// wrote one keychain item per credential. `--keys` removes the vault and every
// item of either kind.
let credentials = [];
let keychainItems = [];
const credentialVaultDirectory = home && fs.existsSync(path.join(home, ".mework", "credential-vault"))
  ? path.join(home, ".mework", "credential-vault")
  : null;
if (options.keys && process.platform === "darwin") {
  try {
    // Attributes only: without `-d` the dump neither prints nor unlocks secrets.
    const output = execFileSync("security", ["dump-keychain"], {
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024
    });
    keychainItems = planKeychainItems(output);
    credentials = keychainItems;
  } catch (error) {
    fail(`无法读取 macOS 钥匙串: ${error.message}`);
  }
} else if (options.keys) {
  if (process.platform !== "win32") {
    fail("--keys 目前只支持 Windows 凭据管理器与 macOS 钥匙串");
  }
  try {
    const output = execFileSync("cmdkey.exe", ["/list"], {
      encoding: "utf8",
      windowsHide: true
    });
    credentials = planCredentialTargets(output);
  } catch (error) {
    fail(`无法读取 Windows 凭据管理器: ${error.message}`);
  }
}

const scopeNames = { prod: "正式数据目录", dev: "调试数据目录", all: "全部数据目录" };
console.log(`${label} 清理范围：${scopeNames[options.scope]}`);

// src-tauri/src/mework_memory.rs keeps the global memory tier under the home
// directory, deliberately outside both AppData roots.
const memoryDirectory = home && fs.existsSync(path.join(home, ".mework", "memory"))
  ? path.join(home, ".mework", "memory")
  : null;
const codexOauthDirectory = home && fs.existsSync(path.join(home, ".mework", "codex-oauth"))
  ? path.join(home, ".mework", "codex-oauth")
  : null;
const keyDirectories = options.keys
  ? [codexOauthDirectory, credentialVaultDirectory].filter(Boolean)
  : [];

if (directories.length === 0 && credentials.length === 0 && keyDirectories.length === 0) {
  console.log(`${label} 没有需要清理的内容。`);
  process.exit(0);
}

for (const directory of directories) {
  const note = directory.identifier === INTERACTIVE_DEV_IDENTIFIER
    ? "（dev:browser 常驻调试目录）"
    : "";
  console.log(
    `${label}   ${directory.label}/${directory.identifier}`
      + `  ${formatSize(directorySize(directory.path))} ${note}`
  );
}
for (const { service, count } of summarizeCredentials(credentials)) {
  console.log(`${label}   凭据 ${service} × ${count}`);
}
if (options.keys && codexOauthDirectory) {
  console.log(`${label}   ${codexOauthDirectory}（ChatGPT 登录令牌，随凭据一起清理）`);
}
if (options.keys && credentialVaultDirectory) {
  console.log(`${label}   ${credentialVaultDirectory}（macOS 凭据库，随钥匙串主密钥一起清理）`);
}

// Global memory is plain Markdown the user wrote by hand and carries no schema
// version, so a schema bump never invalidates it. Say so rather than skipping
// it silently — "wipe app data" could reasonably be read as including it.
if (memoryDirectory) {
  console.log(`${label} 保留 ${memoryDirectory}（无 schema 的手写全局记忆，需要时请手动删除）`);
}

if (options.dryRun) {
  console.log(`${label} --dry-run：未删除任何内容。`);
  process.exit(0);
}
const running = runningApplications();
if (running.length > 0) {
  fail(`请先退出 ${running.join("、")} 再清理，否则正在运行的实例会继续写入已删除的目录。`);
}

if (!options.assumeYes) {
  if (!process.stdin.isTTY) {
    fail("非交互式终端请显式传入 --yes 确认删除。");
  }
  const readline = createInterface({ input: process.stdin, output: process.stdout });
  const answer = await readline.question(`${label} 以上内容将被永久删除，确认？(y/N) `);
  readline.close();
  if (answer.trim().toLowerCase() !== "y") {
    console.log(`${label} 已取消。`);
    process.exit(0);
  }
}

let failed = 0;
for (const directory of directories) {
  try {
    fs.rmSync(directory.path, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
    console.log(`${label} 已删除 ${directory.label}/${directory.identifier}`);
  } catch (error) {
    failed += 1;
    console.error(`${label} 无法删除 ${directory.path}: ${error.message}`);
  }
}

for (const { service, account } of keychainItems) {
  try {
    execFileSync("security", ["delete-generic-password", "-s", service, "-a", account], { stdio: "ignore" });
    console.log(`${label} 已删除钥匙串项 ${service} / ${account}`);
  } catch (error) {
    failed += 1;
    console.error(`${label} 无法删除钥匙串项 ${service} / ${account}: ${error.message}`);
  }
}

for (const { target } of process.platform === "win32" ? credentials : []) {
  try {
    execFileSync("cmdkey.exe", [`/delete:${target}`], { stdio: "ignore", windowsHide: true });
    console.log(`${label} 已删除凭据 ${target}`);
  } catch (error) {
    failed += 1;
    console.error(`${label} 无法删除凭据 ${target}: ${error.message}`);
  }
}

// src-tauri/src/codex_oauth.rs keeps the ChatGPT OAuth tokens as files encrypted
// under master keys that live in the credentials just deleted, and on macOS the
// credential vault is sealed under the keychain item just deleted. Without those
// keys the files are unreadable, so `--keys` removes them as well.
for (const directory of keyDirectories) {
  try {
    fs.rmSync(directory, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
    console.log(`${label} 已删除 ${directory}`);
  } catch (error) {
    failed += 1;
    console.error(`${label} 无法删除 ${directory}: ${error.message}`);
  }
}

if (failed > 0) {
  fail(`${failed} 项清理失败。`);
}
console.log(`${label} 清理完成，下次启动将从种子文档重建。`);
