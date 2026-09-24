import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Opt-in stable code signature for macOS development runs.
 *
 * Every `cargo build` links a new ad-hoc signature into the application binary, and macOS
 * binds Keychain "Always Allow" answers and privacy grants (Desktop, Documents, Local Network,
 * ...) to the signature that received them, so after each rebuild Mework is asked about them
 * again. When `MEWORK_DEV_SIGNING_IDENTITY` names a code-signing identity — an "Apple
 * Development" certificate from Xcode, or a self-signed code-signing certificate made in
 * Keychain Access — Cargo runs the binary through scripts/macos-dev-run.sh, which re-signs it
 * with that identity under the fixed identifier `com.mework.app` first.
 *
 * Cargo reads a runner from `CARGO_TARGET_<TRIPLE>_RUNNER`; both Mac triples are set because a
 * host build looks up the host's. A runner the developer configured is never replaced, and
 * nothing changes on other hosts or without the variable.
 */
export const MACOS_DEV_RUNNER = join(dirname(fileURLToPath(import.meta.url)), "macos-dev-run.sh");

const RUNNER_VARIABLES = [
  "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
  "CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER"
];

export function withMacosDevSigning(environment, platform = process.platform) {
  if (platform !== "darwin" || !environment.MEWORK_DEV_SIGNING_IDENTITY?.trim()) return environment;
  const result = { ...environment };
  for (const name of RUNNER_VARIABLES) {
    if (!result[name]) result[name] = MACOS_DEV_RUNNER;
  }
  return result;
}
