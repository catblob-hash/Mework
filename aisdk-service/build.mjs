// Sidecar build.
//
//   node build.mjs         -> dist/main.mjs   (ESM for development and checks)
//   node build.mjs --sea   -> dist/mework-aisdk.exe on Windows, dist/mework-aisdk
//                             elsewhere (single-file release artifact)
//
// Use `platform: "node"` rather than neutral: AI SDK provider-utils branches by
// platform, and a browser build would include unused polyfills and select the wrong fetch branch.

import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { stripAuthenticode } from "./strip-authenticode.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const dist = resolve(here, "dist");
const sea = process.argv.includes("--sea");

await mkdir(dist, { recursive: true });

// `@ai-sdk/google-vertex` pulls the CJS `@vercel/oidc` credential chain, which
// calls `require("path")` during module initialization. ESM bundles lack `require`,
// so restore it here before the sidecar loads.
const ESM_BANNER = [
  "// mework-aisdk sidecar bundle — generated, do not edit.",
  'import { createRequire as __mework_createRequire } from "node:module";',
  "const require = __mework_createRequire(import.meta.url);",
].join("\n");

// The Claude Agent SDK evaluates `import.meta.url` at module load (it derives a
// `createRequire` from it, used only to locate its bundled CLI when no
// `pathToClaudeCodeExecutable` is given — which the sidecar always gives).
// CommonJS has no `import.meta`, so the SEA bundle defines it from `__filename`;
// inside the single-file executable that is the sidecar itself.
const CJS_BANNER = [
  "// mework-aisdk sidecar bundle — generated, do not edit.",
  'const __mework_import_meta_url = require("node:url").pathToFileURL(__filename).href;',
].join("\n");

/**
 * The Agent SDK's platform packages (`@anthropic-ai/claude-agent-sdk-<os>-<arch>`)
 * hold a ~200 MiB Claude Code binary. Mework ships that binary, but as a file of
 * its own beside the application — `src-tauri/build.rs` stages it and
 * `tauri.conf.json` declares it an `externalBin` — and the host hands its path to
 * the sidecar in `agent.executable`. Bundling it into this JavaScript would only
 * make the sidecar 200 MiB larger for nothing, so esbuild keeps it out.
 */
const EXTERNAL = ["@anthropic-ai/claude-agent-sdk-*"];

async function bundle(format, outfile) {
  const started = Date.now();
  await build({
    entryPoints: [resolve(here, "src/main.ts")],
    outfile,
    bundle: true,
    platform: "node",
    format,
    target: "node22",
    minify: true,
    sourcemap: false,
    legalComments: "none",
    external: EXTERNAL,
    ...(format === "esm"
      ? { banner: { js: ESM_BANNER } }
      : { banner: { js: CJS_BANNER }, define: { "import.meta.url": "__mework_import_meta_url" } }),
  });
  const { size } = await stat(outfile);
  process.stderr.write(`[build] ${outfile} — ${(size / 1024 / 1024).toFixed(2)} MiB, ${Date.now() - started} ms\n`);
  return size;
}

if (!sea) {
  await bundle("esm", resolve(dist, "main.mjs"));
} else {
  // Node SEA requires a CommonJS entry point, so release and development builds
  // require separate bundles.
  await bundle("cjs", resolve(dist, "main.cjs"));

  const config = resolve(here, "sea-config.json");
  await writeFile(
    config,
    `${JSON.stringify(
      {
        main: "dist/main.cjs",
        output: "dist/sea-prep.blob",
        disableExperimentalSEAWarning: true,
        useSnapshot: false,
        useCodeCache: false,
        // Do not allow NODE_OPTIONS to alter the standalone executable's behavior.
        execArgvExtension: "none",
      },
      null,
      2,
    )}\n`,
  );
  execFileSync(process.execPath, ["--experimental-sea-config", config], { stdio: "inherit", cwd: here });

  // `src-tauri/build.rs` and `scripts/build-aisdk-sidecar.mjs` look for the
  // platform's own executable name; the target triple is added when staging.
  const macos = process.platform === "darwin";
  const exe = resolve(dist, process.platform === "win32" ? "mework-aisdk.exe" : "mework-aisdk");
  await copyFile(process.execPath, exe);
  // Node's official SEA steps for macOS: the copied binary carries Node's own
  // signature, which injection would invalidate, and an arm64 Mac refuses to
  // run a binary whose signature does not verify.
  if (macos) execFileSync("codesign", ["--remove-signature", exe], { stdio: "inherit" });
  // And for Windows (`signtool remove /s`): injected around, Node's Authenticode
  // signature leaves a certificate table nothing can sign over, which fails
  // signing the sidecar and any MSIX that contains it (strip-authenticode.mjs).
  if (process.platform === "win32") await writeFile(exe, stripAuthenticode(await readFile(exe)));
  // Run postject's CLI directly instead of `npx postject`: on Windows, npx needs
  // `shell: true` and may fetch packages during the build. The dev dependency has a stable path.
  execFileSync(
    process.execPath,
    [
      resolve(here, "node_modules/postject/dist/cli.js"),
      exe,
      "NODE_SEA_BLOB",
      resolve(dist, "sea-prep.blob"),
      "--sentinel-fuse",
      "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
      // Mach-O keeps the blob in a segment of its own; without it Node never
      // finds the blob and the executable starts as a plain `node`.
      ...(macos ? ["--macho-segment-name", "NODE_SEA"] : []),
    ],
    { stdio: "inherit", cwd: here },
  );
  // Ad-hoc signature so the binary runs locally; a release is re-signed with a
  // Developer ID when the app bundle is signed.
  if (macos) execFileSync("codesign", ["--sign", "-", exe], { stdio: "inherit" });

  const { size } = await stat(exe);
  process.stderr.write(`[build] ${exe} — ${(size / 1024 / 1024).toFixed(1)} MiB\n`);
  if (process.platform === "win32") {
    // The sidecar is unsigned (Node's signature was removed above); signing the
    // NSIS installer does not sign the executables inside it.
    process.stderr.write("[build] 提醒：侧车 exe 未签名（已去掉 node.exe 原有的签名），需要签名时要对它单独 Authenticode 签名。\n");
  }
}
