// Sidecar build.
//
//   node build.mjs         -> dist/main.mjs   (ESM for development and checks)
//   node build.mjs --sea   -> dist/mework-aisdk.exe (single-file release artifact)
//
// Use `platform: "node"` rather than neutral: AI SDK provider-utils branches by
// platform, and a browser build would include unused polyfills and select the wrong fetch branch.

import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, stat, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
// `createRequire` from it, used only to locate its optional bundled CLI, which
// Mework never uses). CommonJS has no `import.meta`, so the SEA bundle defines it
// from `__filename`; inside the single-file executable that is the sidecar itself.
const CJS_BANNER = [
  "// mework-aisdk sidecar bundle — generated, do not edit.",
  'const __mework_import_meta_url = require("node:url").pathToFileURL(__filename).href;',
].join("\n");

/**
 * The Agent SDK's platform packages (`@anthropic-ai/claude-agent-sdk-<os>-<arch>`)
 * hold a 200 MiB Claude Code binary. They are resolved only when no
 * `pathToClaudeCodeExecutable` is given; the sidecar always gives one, so they
 * stay out of the bundle and out of the release.
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

  const exe = resolve(dist, "mework-aisdk.exe");
  await copyFile(process.execPath, exe);
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
    ],
    { stdio: "inherit", cwd: here },
  );

  const { size } = await stat(exe);
  process.stderr.write(`[build] ${exe} — ${(size / 1024 / 1024).toFixed(1)} MiB\n`);
  // postject invalidates Node's Authenticode signature. Sign the sidecar
  // executable separately; signing NSIS does not sign the embedded sidecar.
  process.stderr.write("[build] 提醒：postject 已使 node.exe 的签名失效，发布前必须对该 exe 单独 Authenticode 签名。\n");
}
