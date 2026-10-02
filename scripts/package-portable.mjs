// Packages the portable flavor of a Windows release from an existing
// `npm run tauri:build` output.
//
// Tauri has no portable bundle target — its target enum is deb/rpm/AppImage/MSI/
// NSIS/app/DMG — so this runs beside the bundler rather than inside it.
//
// The archive is an allowlist, never a copy of `target/release`: that directory
// also holds PDBs, dependency intermediates, and staging left over from earlier
// resource configurations. Three executables, the remote agents and the license files are
// the whole payload.
//
// The adjacent binary names are hard runtime contracts. A release build resolves
// the AI SDK sidecar as `<dir of mework.exe>/mework-aisdk.exe` and the bundled
// Claude Code as `<dir of mework.exe>/claude.exe`, neither with a source-tree
// fallback (src-tauri/src/aisdk/process.rs, src-tauri/src/aisdk/agent.rs), so
// renaming either, keeping the `-<target-triple>` suffix, or nesting one in a
// subdirectory produces an archive that launches and then fails every model
// request — or, for `claude.exe`, every request of the Claude Agent family.
//
// Usage:
//   node scripts/package-portable.mjs             # writes the default archive
//   node scripts/package-portable.mjs --out <zip>

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const label = "[package:portable]";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseDirectory = path.join(root, "src-tauri", "target", "release");

function fail(message) {
  console.error(`${label} ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  let out = null;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] !== "--out") fail(`不支持的参数：${argv[index]}`);
    out = argv[index + 1];
    if (!out) fail("--out 需要一个路径");
    index += 1;
  }
  return { out };
}

const { out } = parseArguments(process.argv.slice(2));

const packageVersion = JSON.parse(
  fs.readFileSync(path.join(root, "package.json"), "utf8")
).version;

// Every entry lands at the archive root: the documented instruction is "unzip
// anywhere and run mework.exe", and both adjacent binaries must sit next to it.
const payload = [
  { from: path.join(releaseDirectory, "mework.exe"), as: "mework.exe" },
  { from: path.join(releaseDirectory, "mework-aisdk.exe"), as: "mework-aisdk.exe" },
  { from: path.join(releaseDirectory, "claude.exe"), as: "claude.exe" },
  { from: path.join(root, "LICENSE"), as: "LICENSE" },
  { from: path.join(root, "THIRD-PARTY-NOTICES.md"), as: "THIRD-PARTY-NOTICES.md" },
  { from: path.join(root, "THIRD-PARTY-LICENSES.md"), as: "THIRD-PARTY-LICENSES.md" }
];
// The agent builds go in as the `remote-agents/` directory beside mework.exe, where the
// app looks for them when it has no resource directory (install_machine_links in
// src-tauri/src/lib.rs), the same layout the installer and the MSIX get. Without it a
// portable install can give neither an SSH machine nor a WSL distribution its agent,
// and has no Windows sandbox helper (srt-win.exe) for local commands.
const remoteAgents = path.join(root, "src-tauri", "remote-agents");

const missing = payload.filter((entry) => !fs.existsSync(entry.from)).map((entry) => entry.from);
if (!fs.existsSync(remoteAgents)) missing.push(remoteAgents);
if (missing.length > 0) {
  fail(
    `缺少构建产物：\n${missing.map((file) => `  ${file}`).join("\n")}\n`
      + "Please run npm run tauri:build first."
  );
}

const archive = path.resolve(
  out ?? path.join(releaseDirectory, `Mework_${packageVersion}_x64_portable.zip`)
);
// Staged inside the release directory so the copy never crosses a volume and so
// a failed run leaves nothing outside the build tree.
const staging = path.join(releaseDirectory, ".portable-staging");

fs.rmSync(staging, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
fs.mkdirSync(staging, { recursive: true });
for (const entry of payload) {
  fs.copyFileSync(entry.from, path.join(staging, entry.as));
}
fs.cpSync(remoteAgents, path.join(staging, "remote-agents"), { recursive: true });
const archived = [...payload.map((entry) => entry.as), "remote-agents"];

fs.mkdirSync(path.dirname(archive), { recursive: true });
fs.rmSync(archive, { force: true });

// Compress-Archive is present on every supported Windows and needs no extra
// dependency. `-LiteralPath` on the staged children puts the files at the archive
// root instead of nesting them under the staging directory's own name.
try {
  execFileSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Compress-Archive -LiteralPath "
        + archived.map((name) => `'${path.join(staging, name)}'`).join(",")
        + ` -DestinationPath '${archive}' -CompressionLevel Optimal -Force`
    ],
    { stdio: "inherit", windowsHide: true }
  );
} catch (error) {
  fs.rmSync(staging, { recursive: true, force: true });
  fail(`打包失败：${error instanceof Error ? error.message : String(error)}`);
}

fs.rmSync(staging, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });

const size = fs.statSync(archive).size;
console.log(`${label} 已生成 ${archive}（${(size / 1024 / 1024).toFixed(1)} MiB）`);
for (const name of archived) console.log(`${label}   ${name}`);
