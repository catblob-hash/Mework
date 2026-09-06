import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// The helper is plain ESM so the Node-native CSP regression test can exercise it directly.
// @ts-expect-error This config-only module intentionally has no browser-facing declaration file.
import { frontendCspPlugin } from "./scripts/vite-csp.mjs";

// The port is chosen by whoever starts this server (scripts/tauri-with-build-tools.mjs
// or scripts/browser-dev.mjs), which prefers 1420 but takes an operating-system
// assigned port when it is busy. Binding is strict on purpose: the caller already
// established the port is free and told the WebView where to look, so silently
// drifting to another one would leave the window pointed at nothing. The host is
// pinned to the same loopback address the caller probed and advertises — leaving
// it to resolve `localhost` could bind `::1` while the probe tested `127.0.0.1`,
// which turns the intended fallback into a hard startup failure.
const developmentServerHost = "127.0.0.1";
const developmentServerPort = Number(process.env.MEWORK_DEV_SERVER_PORT ?? 1420);

export default defineConfig({
  plugins: [react(), frontendCspPlugin()],
  clearScreen: false,
  server: {
    host: developmentServerHost,
    port: developmentServerPort,
    strictPort: true,
    // Per-request HTML nonces must stay responsive during a cold start. Vite's
    // eager graph crawl otherwise monopolizes transforms after the first page.
    preTransformRequests: false,
    // Native builds and isolated E2E targets create thousands of artifacts under these trees.
    // Watching them can starve Vite's HTTP loop while Cargo links the browser-dev binary.
    watch: {
      ignored: [
        "**/src-tauri/**",
        "**/.claude/**",
        "**/.codex/**",
        "**/.codex-tmp/**",
        "**/.mework/**",
        "**/.prob-cache/**",
        "**/dist/**",
        "**/docs/**",
        "**/formal/**",
        "**/scratchpad/**",
        "**/target*/**",
        "**/tools/**"
      ]
    }
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    sourcemap: true,
    rollupOptions: {
      input: {
        main: "index.html"
      }
    }
  }
});
