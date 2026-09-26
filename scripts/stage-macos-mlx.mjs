import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Stages the MLX libraries of the local helper model's GPU build for a macOS bundle:
 * `libmework_mlx.dylib` (the model, see src-tauri/local-model/mlx) and Apple's prebuilt
 * `libmlx.dylib` beside it, which `local-model/build.rs` placed in `.mlx/<version>/mlx/lib`.
 * `tauri.macos.conf.json` copies the staged directory to `Contents/Frameworks/mework-mlx`,
 * where the app `dlopen`s the model library on macOS 14+ (it starts on 13, which MLX does
 * not support, so nothing links against them). The kernels (`mlx.metallib`, 136 MB) are
 * not bundled: they come with the MLX model download.
 *
 * Tauri runs this as part of `beforeBundleCommand`. With APPLE_SIGNING_IDENTITY set, both
 * libraries are signed here with the hardened runtime, as Tauri does not sign files it copies.
 * An Intel build has no MLX: the directory is left empty.
 */

const repository = join(dirname(fileURLToPath(import.meta.url)), "..");
const crate = join(repository, "src-tauri");
const stage = join(crate, "target", "mlx-bundle");

if (process.platform !== "darwin") {
  console.log("[mlx] 非 macOS 构建，无需准备 MLX");
  process.exit(0);
}

rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });

const triple = process.env.TAURI_ENV_TARGET_TRIPLE || "";
const intel = triple ? triple.startsWith("x86_64") : process.arch === "x64";
if (intel) {
  console.log("[mlx] Intel 构建不含 MLX");
  process.exit(0);
}

/** The MLX release `local-model/build.rs` pins. */
function mlxVersion() {
  const script = readFileSync(join(crate, "local-model", "build.rs"), "utf8");
  const match = script.match(/const MLX_VERSION: &str = "([^"]+)";/);
  if (!match) throw new Error("local-model/build.rs 中找不到 MLX_VERSION");
  return match[1];
}

const root = process.env.MEWORK_MLX_DIR || join(repository, ".mlx", mlxVersion());
const lib = join(root, "mlx", "lib");
const libraries = ["libmlx.dylib", "libmework_mlx.dylib"];
for (const name of libraries) {
  if (!existsSync(join(lib, name))) {
    throw new Error(`找不到 ${join(lib, name)}\n先构建一次 src-tauri（local-model 的构建脚本会准备它）。`);
  }
  copyFileSync(join(lib, name), join(stage, name));
}

const signingIdentity = process.env.APPLE_SIGNING_IDENTITY;
if (signingIdentity) {
  for (const name of libraries) {
    execFileSync(
      "codesign",
      ["--force", "--timestamp", "--options", "runtime", "--sign", signingIdentity, join(stage, name)],
      { stdio: "inherit" }
    );
  }
}

console.log(`[mlx] 已准备 MLX 运行库：${stage}`);
