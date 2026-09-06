import { spawn } from "node:child_process";
import { join } from "node:path";
import { windowsNativeBuildEnvironment } from "./windows-native-build-tools.mjs";
import { devChildEnvironment } from "./dev-child-environment.mjs";
import {
  DEFAULT_FRONTEND_PORT,
  chooseLoopbackPort,
  parsePort
} from "./dev-server-port.mjs";

/** Environment variable `vite.config.ts` reads to learn which port to bind. */
const DEV_SERVER_PORT_ENVIRONMENT_NAME = "MEWORK_DEV_SERVER_PORT";

const environment = devChildEnvironment({
  buildEnvironment: windowsNativeBuildEnvironment()
});
const tauriArguments = process.argv.slice(2);

// `tauri dev` waits for `devUrl` to answer before it creates the window, so the
// port has to be settled before the CLI starts rather than discovered from
// Vite's output afterwards. Picking it here is what lets both sides agree: Vite
// binds it through the environment variable, and the configuration override
// points the WebView at the same address. The framework default is only a
// preference so a busy port degrades to a working run instead of a failed one.
if (tauriArguments[0] === "dev") {
  const explicit = parsePort(
    process.env[DEV_SERVER_PORT_ENVIRONMENT_NAME],
    DEV_SERVER_PORT_ENVIRONMENT_NAME
  );
  const choice = explicit === null
    ? await chooseLoopbackPort(DEFAULT_FRONTEND_PORT)
    : { port: explicit, preferred: explicit, fellBack: false };
  if (choice.fellBack) {
    process.stdout.write(
      `[tauri] 前端默认端口 ${choice.preferred} 不可用，已改用系统分配的 ${choice.port}\n`
    );
  }
  environment[DEV_SERVER_PORT_ENVIRONMENT_NAME] = String(choice.port);
  tauriArguments.splice(
    1,
    0,
    "--config",
    // The same loopback address the port was probed on and that vite.config.ts
    // binds. Naming `localhost` here instead would let the WebView resolve a
    // different address family than the one that was tested and bound.
    JSON.stringify({ build: { devUrl: `http://127.0.0.1:${choice.port}` } })
  );
}

const tauriEntrypoint = join(
  process.cwd(),
  "node_modules",
  "@tauri-apps",
  "cli",
  "tauri.js",
);
const child = spawn(process.execPath, [tauriEntrypoint, ...tauriArguments], {
  env: environment,
  stdio: "inherit",
  windowsHide: false,
});

child.on("error", (error) => {
  console.error(`无法启动 Tauri CLI：${error.message}`);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
