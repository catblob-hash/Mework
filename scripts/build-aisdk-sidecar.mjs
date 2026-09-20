// Package the AI SDK sidecar as a single executable.
//
//   node scripts/build-aisdk-sidecar.mjs
//
// The artifact is `aisdk-service/dist/mework-aisdk.exe`. Naming and placement belong to `src-tauri/build.rs`: `tauri.conf.json` requires `src-tauri/binaries/mework-aisdk-<target-triple><exe>`, and Cargo's `TARGET` is the authoritative target triple.
//
// Keep this separate from the frontend build because rebuilding the 90 MiB sidecar is comparatively expensive. `tauri.conf.json` runs both from `beforeBuildCommand`.

import { execFileSync } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const service = join(root, "aisdk-service");

function fail(message) {
  console.error(`[aisdk-sidecar] ${message}`);
  process.exit(1);
}

if (!existsSync(join(service, "node_modules"))) {
  fail("aisdk-service/node_modules 不存在；先在 aisdk-service/ 里 `npm install`");
}

try {
  execFileSync(process.execPath, [join(service, "build.mjs"), "--sea"], {
    cwd: service,
    stdio: "inherit",
  });
} catch (error) {
  fail(`侧车打包失败：${error.message}`);
}

const built = join(service, "dist", process.platform === "win32" ? "mework-aisdk.exe" : "mework-aisdk");
if (!existsSync(built)) {
  fail(`侧车产物不存在：${built}`);
}

console.log(`[aisdk-sidecar] ${built} — ${(statSync(built).size / 1024 / 1024).toFixed(1)} MiB`);
console.log("[aisdk-sidecar] src-tauri/build.rs 会在下一次 cargo 构建时按目标三元组摆到 externalBin 的位置。");
