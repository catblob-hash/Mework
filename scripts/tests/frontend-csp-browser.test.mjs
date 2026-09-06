import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  ".."
);
const viteCli = path.join(root, "node_modules", "vite", "bin", "vite.js");
const chromeCandidates = [
  process.env.MEWORK_CHROME_PATH,
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser"
].filter(Boolean);

async function findChrome() {
  for (const candidate of chromeCandidates) {
    try {
      await import("node:fs/promises").then(({ access }) => access(candidate));
      return candidate;
    } catch {
      // Keep looking: a CI worker may install Chrome in only one standard location.
    }
  }
  throw new Error(
    "找不到 Chrome；请设置 MEWORK_CHROME_PATH 或安装 Google Chrome 后运行前端 CSP 浏览器测试"
  );
}

async function rejectedBridge() {
  const server = http.createServer();
  server.on("upgrade", (_request, socket) => {
    socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    await new Promise((resolve) => server.close(resolve));
    throw new Error("无法分配拒绝 bridge 的本地端口");
  }
  return { server, port: address.port };
}

function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolve();
    };
    const timeout = setTimeout(finish, 5_000);
    child.once("close", finish);
    if (process.platform === "win32") {
      const killer = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        windowsHide: true,
        stdio: "ignore"
      });
      killer.once("error", finish);
      return;
    }
    child.kill("SIGTERM");
  });
}

async function waitFor(label, probe, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`${label} 未在 ${timeoutMs}ms 内就绪${lastError ? `：${lastError.message}` : ""}`);
}

class CdpSession {
  constructor(url) {
    this.socket = new WebSocket(url);
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
    this.socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      if (message.id) {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id);
        if (message.error) pending.reject(new Error(message.error.message));
        else pending.resolve(message.result);
        return;
      }
      this.events.push(message);
    });
  }

  async connect() {
    await new Promise((resolve, reject) => {
      this.socket.addEventListener("open", resolve, { once: true });
      this.socket.addEventListener("error", reject, { once: true });
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    const result = new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
    this.socket.send(JSON.stringify({ id, method, params }));
    return result;
  }

  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true
    });
    if (result.exceptionDetails) {
      throw new Error(result.exceptionDetails.text ?? "Chrome evaluation failed");
    }
    return result.result.value;
  }

  close() {
    for (const { reject } of this.pending.values()) reject(new Error("CDP session closed"));
    this.pending.clear();
    this.socket.close();
  }
}

function directive(policy, name) {
  return policy.split(";").map((part) => part.trim()).find((part) => (
    part === name || part.startsWith(`${name} `)
  ));
}

test("Vite 页面以 nonce CSP 启动，拒绝 inline 脚本而不妨碍 CSSOM", { timeout: 75_000 }, async (t) => {
  const bridge = await rejectedBridge();
  const chrome = await findChrome();
  const profile = await mkdtemp(path.join(os.tmpdir(), "mework-csp-chrome-"));
  let vite;
  let viteOutput = "";
  let browser;
  let session;
  t.after(async () => {
    session?.close();
    await Promise.all([stop(browser), stop(vite)]);
    await new Promise((resolve) => bridge.server.close(resolve));
    await rm(profile, { recursive: true, force: true });
  });

  // A failing bridge must surface as normal app state, never as a CSP-induced blank page.
  vite = spawn(process.execPath, [
    viteCli,
    "--config",
    "vite.config.ts",
    "--host",
    "127.0.0.1",
    "--port",
    "0",
    "--strictPort"
  ], {
    cwd: root,
    env: {
      ...process.env,
      // Vite 8 colours its ready banner even when stdout is a pipe, which puts
      // ANSI escapes between `127.0.0.1:` and the port digits — the listener
      // regex below then never matches and the test times out waiting for a
      // server that is already up. Ask for plain output instead of trying to
      // parse through the escapes.
      NO_COLOR: "1",
      VITE_BROWSER_DEV_BACKEND_URL: `ws://127.0.0.1:${bridge.port}/ws`,
      VITE_BROWSER_DEV_TOKEN: "c".repeat(64)
    },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true
  });
  vite.stdout.setEncoding("utf8");
  vite.stderr.setEncoding("utf8");
  vite.stdout.on("data", (chunk) => { viteOutput += chunk; });
  vite.stderr.on("data", (chunk) => { viteOutput += chunk; });
  const origin = await waitFor("Vite listener", () => {
    if (vite.exitCode !== null) throw new Error(`Vite 提前退出 (${vite.exitCode})\n${viteOutput}`);
    const match = /http:\/\/127\.0\.0\.1:(\d+)\//u.exec(viteOutput);
    return match ? `http://127.0.0.1:${match[1]}` : null;
  });
  const url = `${origin}/`;
  await waitFor("Vite", async () => {
    const candidate = await fetch(url);
    return candidate.ok;
  });

  browser = spawn(chrome, [
    "--headless=new",
    "--no-first-run",
    "--no-default-browser-check",
    "--remote-debugging-port=0",
    `--user-data-dir=${profile}`,
    "about:blank"
  ], { stdio: "ignore", windowsHide: true });
  const debugPort = await waitFor("Chrome DevTools port", async () => {
    const value = await readFile(path.join(profile, "DevToolsActivePort"), "utf8");
    const port = Number.parseInt(value.split(/\r?\n/u, 1)[0], 10);
    return Number.isInteger(port) && port > 0 ? port : null;
  });
  const version = await waitFor("Chrome DevTools", async () => {
    const candidate = await fetch(`http://127.0.0.1:${debugPort}/json/version`);
    return candidate.ok ? candidate.json() : null;
  });
  const targetResponse = await fetch(
    `http://127.0.0.1:${debugPort}/json/new?about%3Ablank`,
    { method: "PUT" }
  );
  assert.ok(targetResponse.ok, "Chrome 必须能创建 Vite 页面目标");
  const target = await targetResponse.json();
  session = new CdpSession(target.webSocketDebuggerUrl ?? version.webSocketDebuggerUrl);
  await session.connect();
  await Promise.all([
    session.send("Runtime.enable"),
    session.send("Log.enable"),
    session.send("Network.enable"),
    session.send("Page.enable")
  ]);
  await session.send("Page.navigate", { url });
  try {
    await waitFor("主应用渲染", async () => {
      const state = await session.evaluate(`({
        mounted: document.querySelector('#root')?.childElementCount > 0,
        readyState: document.readyState,
        text: document.body.innerText.slice(0, 500)
      })`);
      if (state.mounted) return true;
      throw new Error(`readyState=${state.readyState}; body=${JSON.stringify(state.text)}`);
    }, 45_000);
  } catch (error) {
    const diagnostics = session.events
      .filter((event) => event.method === "Log.entryAdded" || event.method === "Runtime.exceptionThrown")
      .map((event) => event.params.entry?.text ?? event.params.exceptionDetails?.text)
      .filter(Boolean)
      .join("\n");
    error.message += `\nChrome diagnostics:\n${diagnostics || "(none)"}\nVite output:\n${viteOutput || "(none)"}`;
    throw error;
  }
  const documentResponse = session.events.find((event) => (
    event.method === "Network.responseReceived"
    && event.params.type === "Document"
    && event.params.response.url === url
  ));
  assert.ok(documentResponse, "Chrome 必须观察到主页面的 Document 响应");
  const policyEntry = Object.entries(documentResponse.params.response.headers).find(([name]) => (
    name.toLowerCase() === "content-security-policy"
  ));
  const policy = policyEntry?.[1];
  assert.equal(typeof policy, "string", "Vite 主页面必须以 Content-Security-Policy 响应头交付");
  const scriptSource = directive(policy, "script-src");
  const styleSource = directive(policy, "style-src");
  assert.ok(scriptSource, "CSP 必须显式限制 script-src");
  assert.ok(styleSource, "CSP 必须显式限制 style-src");
  assert.match(policy, /connect-src 'self' ipc: http:\/\/ipc\.localhost/u, "Vite CSP 必须允许 Tauri IPC");
  assert.doesNotMatch(scriptSource, /'unsafe-inline'/, "script-src 不能放行全部 inline script");
  assert.doesNotMatch(styleSource, /'unsafe-inline'/, "style-src 不能放行全部 inline style");
  assert.match(scriptSource, /'nonce-[^']+'/ , "script-src 必须只放行页面 nonce 的 inline 入口");

  const nonceState = await session.evaluate(`(() => {
    const meta = document.querySelector('meta[property="csp-nonce"]');
    const nonce = meta?.nonce || meta?.getAttribute('nonce') || '';
    return {
      nonce,
      scripts: [...document.scripts].map((node) => ({ nonce: node.nonce, inline: !node.src })),
      styles: [...document.querySelectorAll('style')].map((node) => node.nonce)
    };
  })()`);
  assert.ok(nonceState.nonce, "Vite CSP nonce meta 必须存在");
  assert.ok(nonceState.scripts.length > 0, "主页面必须含有已 nonce 化的模块脚本");
  assert.ok(nonceState.scripts.some(({ inline }) => inline), "React refresh inline preamble 必须存在并受 nonce 保护");
  assert.ok(nonceState.scripts.every(({ nonce }) => nonce === nonceState.nonce), "所有脚本 nonce 必须和 CSP meta 一致");
  assert.ok(nonceState.styles.length > 0, "Vite CSS 模块必须实际注入样式节点");
  assert.ok(nonceState.styles.every((nonce) => nonce === nonceState.nonce), "Vite 注入的样式必须携带同一个 nonce");
  assert.ok(policy.includes(`'nonce-${nonceState.nonce}'`), "响应 CSP 必须放行页面实际 nonce");

  const xtermProbe = await session.evaluate(`(async () => {
    const violations = [];
    const onViolation = (event) => violations.push(event.effectiveDirective);
    document.addEventListener('securitypolicyviolation', onViolation);
    const { openCspXtermProbe } = await import('/src/test/cspXtermProbe.ts');
    const before = new Set(document.querySelectorAll('style'));
    const host = document.createElement('div');
    host.style.setProperty('width', '640px');
    host.style.setProperty('height', '200px');
    document.body.append(host);
    const xterm = openCspXtermProbe(host);
    await new Promise((resolve) => xterm.terminal.write('nonce probe', resolve));
    xterm.terminal.resize(41, 6);
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const carrier = document.querySelector('#mework-csp-style-nonce');
    const runtimeStyles = [...document.querySelectorAll('style')].filter((style) => !before.has(style));
    const result = {
      carrierNonce: carrier?.nonce || '',
      styleNonces: runtimeStyles.map((style) => style.nonce),
      violations: [...violations]
    };
    xterm.dispose();
    host.remove();
    document.removeEventListener('securitypolicyviolation', onViolation);
    return result;
  })()`);
  assert.equal(xtermProbe.carrierNonce, nonceState.nonce, "xterm 必须读取页面实际样式 nonce 载体");
  assert.ok(xtermProbe.styleNonces.length > 0, "真实 xterm.open 必须创建运行时样式节点");
  assert.ok(
    xtermProbe.styleNonces.every((nonce) => nonce === nonceState.nonce),
    "xterm 的 viewport、尺寸与主题样式必须全部携带页面 nonce"
  );
  assert.equal(
    xtermProbe.violations.includes("style-src-elem"),
    false,
    "xterm 初始化不得触发 style-src-elem 违规"
  );

  const probe = await session.evaluate(`(async () => {
    const violations = [];
    document.addEventListener('securitypolicyviolation', (event) => {
      violations.push(event.effectiveDirective);
    });
    window.__meworkCspInlineScript = 0;
    window.__meworkCspInlineAttribute = 0;
    const script = document.createElement('script');
    script.textContent = 'window.__meworkCspInlineScript = 1';
    document.body.append(script);
    const button = document.createElement('button');
    button.setAttribute('onclick', 'window.__meworkCspInlineAttribute = 1');
    document.body.append(button);
    button.click();
    const cssom = document.createElement('div');
    document.body.append(cssom);
    cssom.style.setProperty('color', 'rgb(1, 2, 3)');
    await new Promise((resolve) => setTimeout(resolve, 100));
    return {
      inlineScript: window.__meworkCspInlineScript,
      inlineAttribute: window.__meworkCspInlineAttribute,
      cssomColor: getComputedStyle(cssom).color,
      violations
    };
  })()`);
  assert.equal(probe.inlineScript, 0, "动态 inline <script> 必须被 CSP 拒绝");
  assert.equal(probe.inlineAttribute, 0, "inline 事件属性必须被 CSP 拒绝");
  assert.equal(probe.cssomColor, "rgb(1, 2, 3)", "安全的 CSSOM property 写入必须保持可用");
  assert.ok(probe.violations.includes("script-src-elem"), "inline script 必须产生 script-src-elem 违规事件");
  assert.ok(probe.violations.includes("script-src-attr"), "inline 属性必须产生 script-src-attr 违规事件");
  const cspConsoleErrors = session.events.filter((event) => (
    event.method === "Log.entryAdded"
    && /Content Security Policy|violates the following Content Security Policy/i.test(event.params.entry.text)
  ));
  assert.ok(cspConsoleErrors.length >= 2, "Chrome 控制台必须报告两次故意注入的 CSP 拦截");
  const uncaughtExceptions = session.events.filter((event) => event.method === "Runtime.exceptionThrown");
  assert.equal(uncaughtExceptions.length, 0, "不可达 bridge 不得让主应用抛出未捕获异常");
});
