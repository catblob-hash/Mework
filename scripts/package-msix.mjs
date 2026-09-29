// Packages the MSIX flavor of a Windows release from an existing
// `npm run tauri:build` output. Windows only: it needs the Windows SDK's
// makepri.exe, makeappx.exe and, to sign, signtool.exe.
//
// Tauri's bundler has no MSIX target, so, like the portable archive, this runs
// beside it. The package lays the files out the way the NSIS installer does —
// `mework.exe` with `mework-aisdk.exe` and `claude.exe` beside it (hard runtime
// contracts, see package-portable.mjs) and the `remote-agents/` resource
// directory — plus the manifest, the logos and their `resources.pri`.
//
// The identity (Package/Identity/Name, Publisher, PublisherDisplayName) is the one
// the Microsoft Store reserved for Mework, kept in `src-tauri/msix/identity.json`.
//
// Output, in target/release/msix/:
//   Mework_<version>_x64_store.msix  unsigned; what is uploaded to Partner Center,
//                                    which signs it for the Store
//   Mework_<version>_x64.msix        with --sign: the same package signed for
//   Mework_msix_signing.cer          sideloading, and the certificate to trust
//
// Usage:
//   node scripts/package-msix.mjs
//   node scripts/package-msix.mjs --sign <thumbprint of a CurrentUser\My certificate
//                                         whose subject is the identity's Publisher>

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { msixFileNames, msixVersion, renderManifest, validateIdentity } from "./package-msix-plan.mjs";

const label = "[package:msix]";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseDirectory = path.join(root, "src-tauri", "target", "release");
const msixSource = path.join(root, "src-tauri", "msix");
const TIMESTAMP_URL = "http://timestamp.digicert.com";

function fail(message) {
  console.error(`${label} ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  let sign = null;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] !== "--sign") fail(`Unsupported argument: ${argv[index]}`);
    sign = argv[index + 1];
    if (!sign || !/^[0-9A-Fa-f]{40}$/u.test(sign)) fail("--sign needs a 40-hex-digit certificate thumbprint");
    index += 1;
  }
  return { sign };
}

/** The newest Windows SDK's x64 tools, or $MEWORK_WINDOWS_SDK_BIN. */
function sdkBin() {
  const override = process.env.MEWORK_WINDOWS_SDK_BIN;
  if (override) return override;
  const kits = path.join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Windows Kits", "10", "bin");
  const versions = fs.existsSync(kits)
    ? fs.readdirSync(kits).filter((name) => /^10\.\d+\.\d+\.\d+$/u.test(name))
    : [];
  versions.sort((left, right) => {
    const a = left.split(".").map(Number);
    const b = right.split(".").map(Number);
    return a.map((part, index) => part - b[index]).find((delta) => delta !== 0) ?? 0;
  });
  const found = versions.reverse()
    .map((version) => path.join(kits, version, "x64"))
    .find((directory) => fs.existsSync(path.join(directory, "makeappx.exe")));
  if (!found) fail(`No Windows SDK with makeappx.exe under ${kits}; install the Windows SDK or set MEWORK_WINDOWS_SDK_BIN`);
  return found;
}

function run(tool, args) {
  execFileSync(tool, args, { stdio: "inherit", windowsHide: true });
}

if (process.platform !== "win32") fail("MSIX packages are built on Windows (makeappx.exe).");
const { sign } = parseArguments(process.argv.slice(2));

const packageVersion = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version;
let version;
let identity;
try {
  version = msixVersion(packageVersion);
  identity = validateIdentity(JSON.parse(fs.readFileSync(path.join(msixSource, "identity.json"), "utf8")));
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
const manifest = renderManifest(fs.readFileSync(path.join(msixSource, "AppxManifest.xml"), "utf8"), {
  identity,
  version
});

const payload = [
  { from: path.join(releaseDirectory, "mework.exe"), as: "mework.exe" },
  { from: path.join(releaseDirectory, "mework-aisdk.exe"), as: "mework-aisdk.exe" },
  { from: path.join(releaseDirectory, "claude.exe"), as: "claude.exe" },
  { from: path.join(root, "LICENSE"), as: "LICENSE" },
  { from: path.join(root, "THIRD-PARTY-NOTICES.md"), as: "THIRD-PARTY-NOTICES.md" },
  { from: path.join(root, "THIRD-PARTY-LICENSES.md"), as: "THIRD-PARTY-LICENSES.md" }
];
const remoteAgents = path.join(root, "src-tauri", "remote-agents");
const missing = payload.filter((entry) => !fs.existsSync(entry.from)).map((entry) => entry.from);
if (!fs.existsSync(remoteAgents)) missing.push(remoteAgents);
if (missing.length > 0) {
  fail(`Missing build artifacts:\n${missing.map((file) => `  ${file}`).join("\n")}\nRun npm run tauri:build first.`);
}

const tools = sdkBin();
const staging = path.join(releaseDirectory, ".msix-staging");
const priWork = path.join(releaseDirectory, ".msix-pri");
const outputDirectory = path.join(releaseDirectory, "msix");
for (const directory of [staging, priWork]) {
  fs.rmSync(directory, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
  fs.mkdirSync(directory, { recursive: true });
}
fs.mkdirSync(outputDirectory, { recursive: true });

for (const entry of payload) fs.copyFileSync(entry.from, path.join(staging, entry.as));
fs.cpSync(remoteAgents, path.join(staging, "remote-agents"), { recursive: true });
fs.cpSync(path.join(msixSource, "Assets"), path.join(staging, "Assets"), { recursive: true });
fs.writeFileSync(path.join(staging, "AppxManifest.xml"), manifest);

// The logos carry scale/targetsize qualifiers; resources.pri is what maps
// `Assets\Square44x44Logo.png` in the manifest to them. The config lives outside
// the staging directory so it is not indexed into the package.
const priConfig = path.join(priWork, "priconfig.xml");
run(path.join(tools, "makepri.exe"), ["createconfig", "/cf", priConfig, "/dq", "en-US", "/pv", "10.0.0", "/o"]);
run(path.join(tools, "makepri.exe"), [
  "new",
  "/pr", staging,
  "/cf", priConfig,
  "/mn", path.join(staging, "AppxManifest.xml"),
  "/of", path.join(staging, "resources.pri"),
  "/o"
]);

const names = msixFileNames(packageVersion);
const storePackage = path.join(outputDirectory, names.store);
run(path.join(tools, "makeappx.exe"), ["pack", "/d", staging, "/p", storePackage, "/o"]);
console.log(`${label} ${storePackage} (${(fs.statSync(storePackage).size / 1024 / 1024).toFixed(1)} MiB, unsigned, for Partner Center)`);

if (sign) {
  const signedPackage = path.join(outputDirectory, names.signed);
  fs.copyFileSync(storePackage, signedPackage);
  // signtool refuses a certificate whose subject is not the manifest's Publisher.
  run(path.join(tools, "signtool.exe"), [
    "sign", "/fd", "SHA256", "/sha1", sign, "/s", "My", "/tr", TIMESTAMP_URL, "/td", "SHA256", signedPackage
  ]);
  const certificate = path.join(outputDirectory, names.certificate);
  fs.rmSync(certificate, { force: true });
  execFileSync("powershell.exe", [
    "-NoProfile",
    "-NonInteractive",
    "-Command",
    `Export-Certificate -Cert 'Cert:\\CurrentUser\\My\\${sign}' -FilePath '${certificate}' -Type CERT | Out-Null`
  ], { stdio: "inherit", windowsHide: true });
  console.log(`${label} ${signedPackage} (signed with ${sign})`);
  console.log(`${label} ${certificate}`);
}

fs.rmSync(staging, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
fs.rmSync(priWork, { recursive: true, force: true, maxRetries: 3, retryDelay: 120 });
