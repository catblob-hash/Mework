import { execFileSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { REPOSITORY_CEF_PATH, withCefBuildEnvironment } from "./cef-environment.mjs";

/**
 * Stages what a macOS bundle needs for the built-in browser before Tauri bundles it: the
 * Chromium Embedded Framework, and the helper apps Chromium launches its renderer, GPU and
 * utility processes from. `tauri.macos.conf.json` copies both into `Contents/Frameworks` from
 * the fixed directory written here — the layout Chromium itself requires, and the one
 * Claude.app ships (`Claude Helper*.app` beside `Electron Framework.framework`).
 *
 * Tauri runs this as `beforeBundleCommand`, with TAURI_ENV_DEBUG and TAURI_ENV_TARGET_TRIPLE
 * describing the build being bundled. Both go in through `bundle.macOS.files` rather than
 * `frameworks`: tauri-build copies every framework into `target/` on each compile, development
 * builds included, which for a 300 MB framework that only a bundle needs is pure cost. Tauri
 * signs neither, so when APPLE_SIGNING_IDENTITY is set they are signed here, inside-out, with
 * the hardened runtime and the entitlements each helper's process type needs; Tauri then signs
 * the application around them.
 */

const repository = join(dirname(fileURLToPath(import.meta.url)), "..");
const crate = join(repository, "src-tauri");
const stage = join(crate, "target", "cef-bundle");
const helperName = "Mework Helper";

if (process.platform !== "darwin") {
  console.log("[cef] 非 macOS 构建，无需准备 Chromium 组件");
  process.exit(0);
}

const debug = process.env.TAURI_ENV_DEBUG === "true";
const triple = process.env.TAURI_ENV_TARGET_TRIPLE || undefined;
const arch = (triple ?? process.arch).startsWith("x86_64") || process.arch === "x64" ? "x86_64" : "aarch64";

/** The CEF build the crate was compiled against: cef-dll-sys `154.0.0+154.0.23` is CEF 154.0.23. */
function cefVersion() {
  const lock = readFileSync(join(crate, "Cargo.lock"), "utf8");
  const match = lock.match(/name = "cef-dll-sys"\r?\nversion = "[^"+]+\+([^"]+)"/);
  if (!match) throw new Error("Cargo.lock 中找不到 cef-dll-sys 的版本");
  return match[1];
}

const cefPath = process.env.CEF_PATH || REPOSITORY_CEF_PATH;
const distribution = join(cefPath, cefVersion(), `cef_macos_${arch}`);
const framework = join(distribution, "Chromium Embedded Framework.framework");
if (!existsSync(framework)) {
  throw new Error(`找不到 Chromium Embedded Framework：${framework}\n先构建一次 src-tauri（cef-dll-sys 会下载它）。`);
}

// The helper is a separate, small executable: bundling the application binary five times over
// would multiply the bundle, and a helper must not be able to start the application.
const cargoArguments = ["build", "--bin", "mework-cef-helper"];
if (!debug) cargoArguments.push("--release");
if (triple) cargoArguments.push("--target", triple);
execFileSync("cargo", cargoArguments, {
  cwd: crate,
  env: withCefBuildEnvironment(process.env),
  stdio: "inherit"
});
const targetDirectory = process.env.CARGO_TARGET_DIR || join(crate, "target");
const helperBinary = join(targetDirectory, ...(triple ? [triple] : []), debug ? "debug" : "release", "mework-cef-helper");

const version = JSON.parse(readFileSync(join(crate, "tauri.conf.json"), "utf8")).version;
// The helpers load the same framework as the application, so they share its floor (CEF 154
// is built for macOS 13), which tauri.macos.conf.json states once for both.
const minimumSystemVersion = JSON.parse(readFileSync(join(crate, "tauri.macos.conf.json"), "utf8"))
  .bundle.macOS.minimumSystemVersion;
const identifier = JSON.parse(readFileSync(join(crate, "tauri.conf.json"), "utf8")).identifier;

/** Chromium picks a helper by suffix; each process type runs under its own entitlements. */
const variants = [
  { suffix: "", id: "helper", entitlements: [] },
  { suffix: " (GPU)", id: "helper.gpu", entitlements: ["com.apple.security.cs.allow-jit"] },
  { suffix: " (Renderer)", id: "helper.renderer", entitlements: ["com.apple.security.cs.allow-jit"] },
  {
    suffix: " (Plugin)",
    id: "helper.plugin",
    entitlements: [
      "com.apple.security.cs.allow-unsigned-executable-memory",
      "com.apple.security.cs.disable-library-validation"
    ]
  },
  { suffix: " (Alerts)", id: "helper.alerts", entitlements: [] }
];

function infoPlist(name, bundleIdentifier) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>${name}</string>
  <key>CFBundleExecutable</key><string>${name}</string>
  <key>CFBundleIdentifier</key><string>${bundleIdentifier}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>${name}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSEnvironment</key>
  <dict><key>MallocNanoZone</key><string>0</string></dict>
  <key>LSFileQuarantineEnabled</key><true/>
  <key>LSMinimumSystemVersion</key><string>${minimumSystemVersion}</string>
  <key>LSUIElement</key><string>1</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
`;
}

function entitlementsPlist(keys) {
  const entries = keys.map((key) => `  <key>${key}</key><true/>`).join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
${entries}
</dict>
</plist>
`;
}

rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });
// ditto keeps the framework's symlinks and extended attributes exactly as distributed.
execFileSync("ditto", [framework, join(stage, "Chromium Embedded Framework.framework")]);
// The licenses that travel with the framework (THIRD-PARTY-NOTICES.md): Chromium's, collected
// in CREDITS.html, and CEF's own BSD notice, which the distribution carries in its headers.
cpSync(join(distribution, "CREDITS.html"), join(stage, "CHROMIUM-CREDITS.html"));
const headerLines = readFileSync(join(distribution, "include", "cef_app.h"), "utf8").split(/\r?\n/);
const noticeEnd = headerLines.findIndex((line) => line.includes("POSSIBILITY OF SUCH DAMAGE"));
if (noticeEnd < 0) throw new Error("CEF 头文件中找不到许可声明");
const cefNotice = headerLines
  .slice(0, noticeEnd + 1)
  .map((line) => line.replace(/^\/\/ ?/, ""))
  .join("\n");
writeFileSync(join(stage, "CEF-LICENSE.txt"), `Chromium Embedded Framework\n\n${cefNotice.trim()}\n`);

const signingIdentity = process.env.APPLE_SIGNING_IDENTITY;
const sign = (path, entitlements) => execFileSync("codesign", [
  "--force",
  "--timestamp",
  "--options",
  "runtime",
  ...(entitlements ? ["--entitlements", entitlements] : []),
  "--sign",
  signingIdentity,
  path
], { stdio: "inherit" });
if (signingIdentity) {
  const stagedFramework = join(stage, "Chromium Embedded Framework.framework");
  for (const library of readdirSync(join(stagedFramework, "Libraries"))) {
    if (library.endsWith(".dylib")) sign(join(stagedFramework, "Libraries", library));
  }
  sign(stagedFramework);
}
for (const variant of variants) {
  const name = `${helperName}${variant.suffix}`;
  const bundle = join(stage, `${name}.app`);
  const executable = join(bundle, "Contents", "MacOS", name);
  mkdirSync(dirname(executable), { recursive: true });
  cpSync(helperBinary, executable);
  chmodSync(executable, 0o755);
  writeFileSync(join(bundle, "Contents", "Info.plist"), infoPlist(name, `${identifier}.${variant.id}`));
  if (signingIdentity) {
    const entitlements = join(tmpdir(), `mework-${variant.id}.entitlements`);
    writeFileSync(entitlements, entitlementsPlist(variant.entitlements));
    sign(bundle, entitlements);
    rmSync(entitlements, { force: true });
  }
}

console.log(`[cef] 已准备 Chromium 组件：${stage}`);
