// Stages the exact assets to upload for a GitHub Release.
//
// The in-app updater matches release assets by their name patterns and verifies
// downloads against SHA256SUMS when that manifest is present. Do not rename the
// staged assets: renamed files break in-app updates.
//
// Usage:
//   node scripts/release-assets.mjs             # writes the default staging directory
//   node scripts/release-assets.mjs --out <dir>

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { formatSha256Sums, releaseAssetNames } from "./release-assets-plan.mjs";

const label = "[release:assets]";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseDirectory = path.join(root, "src-tauri", "target", "release");

function fail(message) {
  console.error(`${label} ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  let out = null;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] !== "--out") fail(`Unsupported argument: ${argv[index]}`);
    out = argv[index + 1];
    if (!out) fail("--out requires a directory path");
    index += 1;
  }
  return { out };
}

function sha256File(file) {
  return new Promise((resolve, reject) => {
    const hash = createHash("sha256");
    const stream = fs.createReadStream(file);
    stream.on("error", reject);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", () => resolve(hash.digest("hex")));
  });
}

const { out } = parseArguments(process.argv.slice(2));
const packageVersion = JSON.parse(
  fs.readFileSync(path.join(root, "package.json"), "utf8")
).version;
const assetNames = releaseAssetNames(packageVersion);
const assets = [
  {
    name: assetNames.installer,
    from: path.join(releaseDirectory, "bundle", "nsis", assetNames.installer)
  },
  {
    name: assetNames.portable,
    from: path.join(releaseDirectory, assetNames.portable)
  }
];

const missing = assets.filter((asset) => !fs.existsSync(asset.from));
if (missing.length > 0) {
  fail(
    `Missing build artifacts:\n${missing.map((asset) => `  ${asset.from}`).join("\n")}\n`
      + "Run npm run tauri:build and npm run package:portable first."
  );
}

const outputDirectory = path.resolve(
  out ?? path.join(releaseDirectory, "release-assets")
);
const checksummedAssets = await Promise.all(assets.map(async (asset) => ({
  ...asset,
  size: fs.statSync(asset.from).size,
  sha256: await sha256File(asset.from)
})));

// Only the three staged files are replaced. The output directory itself is never removed:
// `--out` may point at a directory the user also keeps other things in.
fs.mkdirSync(outputDirectory, { recursive: true });
for (const asset of checksummedAssets) {
  const target = path.join(outputDirectory, asset.name);
  fs.rmSync(target, { force: true });
  fs.copyFileSync(asset.from, target);
}
fs.writeFileSync(
  path.join(outputDirectory, assetNames.checksums),
  formatSha256Sums(checksummedAssets)
);

for (const asset of checksummedAssets) {
  console.log(
    `${label} ${asset.name} (${(asset.size / 1024 / 1024).toFixed(1)} MiB) ${asset.sha256}`
  );
}

const installer = path.join(outputDirectory, assetNames.installer);
const portable = path.join(outputDirectory, assetNames.portable);
const checksums = path.join(outputDirectory, assetNames.checksums);
console.log(`${label} Upload these exact assets to the v${packageVersion} tag:`);
console.log(
  `gh release create v${packageVersion} ${installer} ${portable} ${checksums}`
    + ` --title "Mework ${packageVersion}" --notes-file <release-notes.md>`
);
console.log(`${label} The in-app updater matches these exact asset name patterns; the tag must be v${packageVersion}.`);
