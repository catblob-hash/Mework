import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  CSP_NONCE_PLACEHOLDER,
  createContentSecurityPolicy,
  frontendCspPlugin
} from "../vite-csp.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const viteCli = path.join(root, "node_modules", "vite", "bin", "vite.js");
const cspEnvironmentNames = [
  "VITE_BROWSER_DEV_BACKEND_URL",
  "VITE_BROWSER_DEV_TOKEN",
  "VITE_MEMORY_E2E_ENABLED",
  "VITE_MEMORY_E2E_RUN_ID",
  "VITE_MEMORY_E2E_PROTOCOL_BASE_URL",
  "VITE_IMAGE_INPUT_E2E_RUN_ID",
  "VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL",
  "VITE_IMAGE_INPUT_E2E_REPORT_URL",
  "VITE_IMAGE_INPUT_E2E_REPORT_TOKEN",
  "VITE_WEB_SEARCH_E2E_RUN_ID",
  "VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL",
  "VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN",
  "VITE_WEB_SEARCH_E2E_REPORT_URL",
  "VITE_WEB_SEARCH_E2E_REPORT_TOKEN"
];

function cleanEnvironment() {
  const environment = { ...process.env };
  for (const name of cspEnvironmentNames) delete environment[name];
  return environment;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("无法分配前端 CSP 测试端口"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  const exited = new Promise((resolve) => child.once("exit", resolve));
  const timeout = new Promise((resolve) => setTimeout(resolve, 3_000));
  await Promise.race([exited, timeout]);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

async function waitForPage(url, child, logs) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`Vite 提前退出 (${child.exitCode})\n${logs()}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // The listener is not ready yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 75));
  }
  throw new Error(`Vite 未及时提供测试页面\n${logs()}`);
}

async function fetchHtml(url, init = {}) {
  const deadline = Date.now() + 20_000;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const headers = new Headers(init.headers);
      if (!headers.has("accept")) headers.set("accept", "text/html");
      return await fetch(url, { ...init, headers, signal: AbortSignal.timeout(5_000) });
    } catch (error) {
      lastError = error;
      // Vite may replace its dependency-optimizer session once during a cold start.
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  throw new Error(`无法读取 ${url}: ${lastError?.message ?? "unknown fetch failure"}`);
}

function nonceFromPolicy(policy) {
  const match = /(?:^|;)\s*script-src\s+'nonce-([^']+)'\s+'self'(?:;|$)/u.exec(policy);
  assert.ok(match, "script-src 必须含逐响应 nonce 和 self");
  return match[1];
}

function assertHtmlNonce(response, html) {
  const policy = response.headers.get("content-security-policy") ?? "";
  const nonce = nonceFromPolicy(policy);
  assert.equal(Buffer.from(nonce, "base64").byteLength, 24, "nonce 必须含 192 bit 随机量");
  assert.doesNotMatch(html, new RegExp(CSP_NONCE_PLACEHOLDER, "u"));
  const nonceAttributes = [...html.matchAll(/\snonce="([^"]+)"/gu)].map((match) => match[1]);
  assert.ok(nonceAttributes.length >= 3, "Vite meta、客户端和页面入口都必须 nonce 化");
  assert.ok(nonceAttributes.every((candidate) => candidate === nonce));
  for (const match of html.matchAll(/<(?:script|style)\b[^>]*>|<link\b[^>]*rel="stylesheet"[^>]*>/giu)) {
    assert.match(match[0], new RegExp(`\\snonce="${nonce.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&")}"`, "u"));
  }
  return { nonce, policy };
}

test("CSP policy blocks inline attacks and avoids wildcard sources", () => {
  const policy = createContentSecurityPolicy("test-nonce", [
    "ws://127.0.0.1:1420",
    "ws://127.0.0.1:1430"
  ]);
  assert.match(policy, /default-src 'none'/u);
  assert.match(policy, /script-src-attr 'none'/u);
  assert.match(policy, /style-src-attr 'none'/u);
  assert.match(policy, /connect-src 'self' ipc: http:\/\/ipc\.localhost/u);
  assert.match(policy, /frame-src http: https:/u);
  // With no playback surface, media sources must remain disabled. A static allowlist
  // would add an unused media source to every ordinary `npm run dev` session.
  assert.match(policy, /media-src 'none'/u);
  assert.doesNotMatch(policy, /unsafe-inline|unsafe-eval|:\*/u);
});

test("E2E URL configuration fails closed before the dev server starts", () => {
  const invalid = {
    VITE_IMAGE_INPUT_E2E_RUN_ID: "0123456789abcdef01234567",
    VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL: "http://127.0.0.1:18100/v1",
    VITE_IMAGE_INPUT_E2E_REPORT_URL: "https://attacker.example/result",
    VITE_IMAGE_INPUT_E2E_REPORT_TOKEN: "a".repeat(64)
  };
  assert.throws(
    () => frontendCspPlugin(invalid).config({}, { command: "serve" }),
    /VITE_IMAGE_INPUT_E2E_REPORT_URL.*127\.0\.0\.1/u
  );
  assert.throws(
    () => frontendCspPlugin({ VITE_MEMORY_E2E_ENABLED: "1" }).config({}, { command: "serve" }),
    /VITE_MEMORY_E2E_RUN_ID/u
  );
});

test("every Vite HTML response receives a fresh matching nonce CSP", { timeout: 75_000 }, async (t) => {
  const port = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const environment = {
    ...cleanEnvironment(),
    VITE_BROWSER_DEV_BACKEND_URL: "ws://127.0.0.1:19100/ws",
    VITE_BROWSER_DEV_TOKEN: "a".repeat(64),
    VITE_MEMORY_E2E_ENABLED: "1",
    VITE_MEMORY_E2E_RUN_ID: "0123456789abcdef01234567",
    VITE_MEMORY_E2E_PROTOCOL_BASE_URL: "http://127.0.0.1:19101/v1",
    VITE_IMAGE_INPUT_E2E_RUN_ID: "123456789abcdef012345678",
    VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL: "http://127.0.0.1:19102/v1",
    VITE_IMAGE_INPUT_E2E_REPORT_URL: "http://127.0.0.1:19103/result",
    VITE_IMAGE_INPUT_E2E_REPORT_TOKEN: "b".repeat(64),
    VITE_WEB_SEARCH_E2E_RUN_ID: "23456789abcdef0123456789",
    VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL: "http://127.0.0.1:19104/v1",
    VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN: "http://127.0.0.1:19105/",
    VITE_WEB_SEARCH_E2E_REPORT_URL: "http://127.0.0.1:19106/result",
    VITE_WEB_SEARCH_E2E_REPORT_TOKEN: "c".repeat(64)
  };
  let output = "";
  const vite = spawn(
    process.execPath,
    [viteCli, "--config", "vite.config.ts", "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: root, env: environment, stdio: ["ignore", "pipe", "pipe"], windowsHide: true }
  );
  vite.stdout.on("data", (chunk) => { output += chunk; });
  vite.stderr.on("data", (chunk) => { output += chunk; });
  t.after(() => stop(vite));
  await waitForPage(origin, vite, () => output);

  const nonces = [];
  for (const pathname of [
    "/",
    "/deep-link",
    "/memory-e2e.html",
    "/image-input-e2e.html",
    "/web-search-e2e.html"
  ]) {
    const response = await fetchHtml(`${origin}${pathname}`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("cache-control"), "no-store");
    const result = assertHtmlNonce(response, await response.text());
    nonces.push(result.nonce);
    assert.match(result.policy, new RegExp(`connect-src 'self' ipc: http://ipc\\.localhost ws://127\\.0\\.0\\.1:${port} ws://127\\.0\\.0\\.1:19100`, "u"));
    for (const allowed of [19103, 19106]) {
      assert.match(result.policy, new RegExp(`http://127\\.0\\.0\\.1:${allowed}(?:\\s|;)`, "u"));
    }
    assert.doesNotMatch(result.policy, /attacker|19101|19102|19104|19105|:\*/u);
  }
  assert.equal(new Set(nonces).size, nonces.length, "同一批页面请求也不能复用 nonce");

  const moduleResponse = await fetch(`${origin}/src/main.tsx`, {
    headers: { accept: "*/*" }
  });
  assert.equal(moduleResponse.status, 200);
  assert.equal(moduleResponse.headers.has("content-security-policy"), false, "CSP 中间件只应处理 HTML 文档响应");

  const parallel = await Promise.all(Array.from({ length: 2 }, async () => {
    const response = await fetchHtml(origin);
    return assertHtmlNonce(response, await response.text()).nonce;
  }));
  assert.equal(new Set(parallel).size, parallel.length, "并发 HTML 请求必须保持 nonce 请求隔离");

  const first = await fetchHtml(origin);
  const firstResult = assertHtmlNonce(first, await first.text());
  const conditional = await fetchHtml(origin, {
    headers: { "if-none-match": "W/\"stale-vite-html\"" }
  });
  assert.equal(conditional.status, 200, "HTML 条件请求不能产生 nonce/body 错配的 304");
  const conditionalResult = assertHtmlNonce(conditional, await conditional.text());
  assert.notEqual(firstResult.nonce, conditionalResult.nonce);
});

test("production Vite output keeps the Tauri style nonce carrier without a dev nonce", { timeout: 30_000 }, () => {
  const outputDirectory = mkdtempSync(path.join(os.tmpdir(), "mework-csp-build-"));
  try {
    const result = spawnSync(
      process.execPath,
      [viteCli, "build", "--config", "vite.config.ts", "--outDir", outputDirectory, "--emptyOutDir"],
      { cwd: root, env: cleanEnvironment(), encoding: "utf8", windowsHide: true }
    );
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    const html = readFileSync(path.join(outputDirectory, "index.html"), "utf8");
    assert.doesNotMatch(html, new RegExp(CSP_NONCE_PLACEHOLDER, "u"));
    assert.doesNotMatch(html, /property=["']csp-nonce["']|\snonce=/iu);
    assert.match(html, /<style id=["']mework-csp-style-nonce["']><\/style>/iu);
  } finally {
    rmSync(outputDirectory, { recursive: true, force: true });
  }
});

test("TypeScript cannot emit a root Vite configuration shadow", () => {
  const nodeConfig = JSON.parse(readFileSync(path.join(root, "tsconfig.node.json"), "utf8"));
  assert.equal(nodeConfig.compilerOptions?.emitDeclarationOnly, true);
  assert.match(nodeConfig.compilerOptions?.outDir ?? "", /^\.\/node_modules\/\.tmp\//u);
  assert.equal(existsSync(path.join(root, "vite.config.js")), false);
  assert.equal(existsSync(path.join(root, "vite.config.d.ts")), false);
  const packageJson = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  for (const name of ["dev", "build", "preview"]) {
    assert.match(packageJson.scripts[name], /--config vite\.config\.ts/u, `${name} must pin the TypeScript config`);
  }
});
