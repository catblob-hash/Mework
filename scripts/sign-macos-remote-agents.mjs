import { execFileSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Signs the macOS builds of the remote agent (`src-tauri/remote-agents/*-apple-darwin/mework-remote`)
 * before Tauri copies them into `Contents/Resources/remote-agents`. Tauri signs only the binaries it
 * builds and its `externalBin`s, not resources, and notarization rejects a bundle holding an unsigned
 * Mach-O. The signature goes into the file the app hashes, so the digest the app expects and the one
 * the uploaded agent reports (`platform::self_digest`) stay the same build.
 *
 * Tauri runs this as part of `beforeBundleCommand`; without APPLE_SIGNING_IDENTITY it does nothing.
 */

const crate = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri");
const agents = join(crate, "remote-agents");
const signingIdentity = process.env.APPLE_SIGNING_IDENTITY;

if (process.platform !== "darwin" || !signingIdentity) process.exit(0);
if (!existsSync(agents)) process.exit(0);

for (const triple of readdirSync(agents).filter((name) => name.endsWith("-apple-darwin"))) {
  const agent = join(agents, triple, "mework-remote");
  if (!existsSync(agent)) continue;
  execFileSync(
    "codesign",
    ["--force", "--timestamp", "--options", "runtime", "--sign", signingIdentity, agent],
    { stdio: "inherit" }
  );
  console.log(`[remote-agents] 已签名 ${triple}`);
}
