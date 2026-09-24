import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * On macOS the built-in browser's page engine is the Chromium Embedded Framework, which
 * `cef-dll-sys` downloads (about 130 MB) and compiles its wrapper against at build time. Left
 * alone it downloads into Cargo's OUT_DIR, so every `cargo clean` or build-script change would
 * fetch it again; pointing `CEF_PATH` at one ignored directory in the repository keeps a single
 * copy that every build — `tauri dev`, `cargo test`, bundling — shares.
 *
 * An explicit `CEF_PATH` in the environment always wins. Other platforms are left untouched:
 * nothing there depends on CEF.
 */
export const REPOSITORY_CEF_PATH = join(dirname(fileURLToPath(import.meta.url)), "..", ".cef");

export function withCefBuildEnvironment(environment, platform = process.platform) {
  if (platform !== "darwin" || environment.CEF_PATH) return environment;
  return { ...environment, CEF_PATH: REPOSITORY_CEF_PATH };
}
