import { randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  cleanupMemoryE2eWorkspaceFixture,
  createMemoryE2eWorkspaceFixture
} from "./memory-e2e-workspace-fixture.mjs";

const HOST = "127.0.0.1";
const DATA_IDENTIFIER = "com.mework.app.e2e.memory";
const MEMORY_TRIGGER = "MEWORK_MEMORY_PROTOCOL_E2E";
const MEMORY_HOLD_TRIGGER = "MEWORK_MEMORY_E2E_HOLD";
const MODEL_ID = "kimi-k3";
const MEMORY_TOOL_NAMES = [
  "memory_list",
  "memory_read",
  "memory_search",
  "memory_upsert",
  "memory_delete"
];
const PROTOCOLS = {
  openai_chat: {
    path: "/v1/chat/completions",
    finalLabel: "[CHAT_MEMORY_E2E_OK]"
  },
  openai_responses: {
    path: "/v1/responses",
    finalLabel: "[RESPONSES_MEMORY_E2E_OK]"
  },
  anthropic: {
    path: "/v1/messages",
    finalLabel: "[ANTHROPIC_MEMORY_E2E_OK]"
  }
};
const PASSTHROUGH_ENVIRONMENT_NAMES = new Set([
  "appdata",
  "ci",
  "colorterm",
  "comspec",
  "home",
  "homedrive",
  "homepath",
  "lang",
  "lc_all",
  "localappdata",
  "node_no_warnings",
  "node_options",
  "number_of_processors",
  "path",
  "pathext",
  "processor_architecture",
  "processor_identifier",
  "programdata",
  "systemroot",
  "temp",
  "term",
  "tmp",
  "tmpdir",
  "tz",
  "userprofile",
  "windir"
]);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const mockScript = path.join(root, "scripts", "protocol-e2e-mock.mjs");
const browserDevScript = path.join(root, "scripts", "browser-dev.mjs");
const ownedChildren = new Set();
let shuttingDown = false;
let activeBrowserFixture;

function environmentPort(name, fallback) {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  if (!/^[0-9]{1,5}$/.test(raw)) throw new Error(`${name} 必须是有效端口`);
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    throw new Error(`${name} 必须在 1024-65535 范围内`);
  }
  return port;
}

function childEnvironment(overrides = {}) {
  const environment = {};
  for (const name of Object.keys(process.env)) {
    if (!PASSTHROUGH_ENVIRONMENT_NAMES.has(name.toLowerCase())) continue;
    const value = process.env[name];
    if (value !== undefined) environment[name] = value;
  }
  Object.assign(environment, {
    MEWORK_BROWSER_DEV_DATA_IDENTIFIER: DATA_IDENTIFIER,
    MEWORK_MEMORY_PROTOCOL_E2E: "1",
    ...overrides
  });
  delete environment.MEWORK_WEB_SEARCH_E2E;
  delete environment.MEWORK_WEB_SEARCH_E2E_SELF_CHECK;
  return environment;
}

function spawnOwned(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: root,
    windowsHide: true,
    ...options
  });
  ownedChildren.add(child);
  child.once("exit", () => ownedChildren.delete(child));
  return child;
}

async function stopOwnedChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const pid = child.pid;
  if (!Number.isInteger(pid) || pid <= 0) {
    throw new Error("拒绝清理无法确认 PID 的子进程");
  }
  const exited = new Promise((resolve) => child.once("exit", resolve));
  if (process.platform === "win32") {
    await new Promise((resolve) => {
      const killer = spawn("taskkill.exe", ["/PID", String(pid), "/T", "/F"], {
        windowsHide: true,
        stdio: "ignore"
      });
      killer.once("error", resolve);
      killer.once("exit", resolve);
    });
  } else {
    child.kill("SIGINT");
  }
  let timeoutId;
  await Promise.race([
    exited,
    new Promise((resolve) => {
      timeoutId = setTimeout(resolve, 5_000);
    })
  ]);
  clearTimeout(timeoutId);
}

async function stopOwnedChildren() {
  const children = [...ownedChildren];
  await Promise.all(children.map((child) => stopOwnedChild(child)));
}

function cleanupOwnedDataIdentifier(dataIdentifier) {
  const prefix = "com.mework.app.e2e.memory-";
  if (
    !dataIdentifier.startsWith(prefix)
    || !/^[0-9a-f]{24}$/.test(dataIdentifier.slice(prefix.length))
  ) {
    throw new Error("拒绝清理无法验证的记忆 E2E 应用数据标识");
  }
  for (const [label, directory] of [
    ["APPDATA", process.env.APPDATA],
    ["LOCALAPPDATA", process.env.LOCALAPPDATA]
  ]) {
    if (!directory) continue;
    const parent = path.resolve(directory);
    const target = path.resolve(parent, dataIdentifier);
    if (path.dirname(target) !== parent || path.basename(target) !== dataIdentifier) {
      throw new Error(`拒绝清理越出 ${label} 的记忆 E2E 应用数据目录`);
    }
    rmSync(target, {
      recursive: true,
      force: true,
      maxRetries: 3,
      retryDelay: 120
    });
  }
}

function cleanupActiveBrowserFixture() {
  if (!activeBrowserFixture) return;
  const owned = activeBrowserFixture;
  activeBrowserFixture = undefined;
  let workspaceError;
  try {
    cleanupMemoryE2eWorkspaceFixture(owned.fixture);
  } catch (error) {
    workspaceError = error;
  }
  try {
    cleanupOwnedDataIdentifier(owned.fixture.dataIdentifier);
  } catch (error) {
    if (!workspaceError) throw error;
    throw new Error(
      `${workspaceError instanceof Error ? workspaceError.message : String(workspaceError)}；`
      + `${error instanceof Error ? error.message : String(error)}`
    );
  }
  if (workspaceError) throw workspaceError;
}

function childTermination(child, label) {
  return new Promise((resolve) => {
    child.once("error", (error) => resolve({ label, error }));
    child.once("exit", (code, signal) => resolve({ label, code, signal }));
  });
}

function listen(server, port) {
  return new Promise((resolve, reject) => {
    const onError = (error) => {
      server.off("listening", onListening);
      reject(error);
    };
    const onListening = () => {
      server.off("error", onError);
      resolve();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen({ host: HOST, port, exclusive: true });
  });
}

function closeServer(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => error ? reject(error) : resolve());
  });
}

async function assertPortAvailable(port) {
  const probe = net.createServer();
  try {
    await listen(probe, port);
  } catch (error) {
    const code = error && typeof error === "object" && "code" in error
      ? error.code
      : "unknown";
    throw new Error(`端口 ${port} 不可独占绑定（${code}）；拒绝启动以免终止非本任务进程`);
  } finally {
    if (probe.listening) await closeServer(probe);
  }
}

async function reserveEphemeralPort() {
  const probe = net.createServer();
  await listen(probe, 0);
  const address = probe.address();
  if (!address || typeof address === "string") {
    await closeServer(probe);
    throw new Error("无法分配本地自测端口");
  }
  const port = address.port;
  await closeServer(probe);
  return port;
}

async function assertPortFailClosedSelfCheck() {
  const occupied = net.createServer();
  await listen(occupied, 0);
  const address = occupied.address();
  if (!address || typeof address === "string") {
    await closeServer(occupied);
    throw new Error("无法建立端口占用自测夹具");
  }
  let rejected = false;
  try {
    await assertPortAvailable(address.port);
  } catch (error) {
    rejected = error instanceof Error && error.message.includes("拒绝启动");
  } finally {
    await closeServer(occupied);
  }
  if (!rejected) throw new Error("端口占用没有 fail closed");
}

function memorySchema(name) {
  const scope = {
    type: "string",
    enum: ["project", "global"],
    default: "project"
  };
  const documentName = { type: "string", minLength: 1, maxLength: 80 };
  const expectedVersion = { type: "integer", minimum: 0 };
  if (name === "memory_list") {
    return { type: "object", properties: { scope }, additionalProperties: false };
  }
  if (name === "memory_read") {
    return {
      type: "object",
      properties: {
        scope,
        name: { ...documentName, default: "MEMORY.md" }
      },
      additionalProperties: false
    };
  }
  if (name === "memory_search") {
    return {
      type: "object",
      properties: {
        scope,
        query: { type: "string", minLength: 1, maxLength: 1000 },
        limit: { type: "integer", minimum: 1, maximum: 50, default: 20 }
      },
      required: ["query"],
      additionalProperties: false
    };
  }
  if (name === "memory_upsert") {
    return {
      type: "object",
      properties: {
        scope,
        name: documentName,
        content: { type: "string", maxLength: 262144 },
        expected_version: expectedVersion
      },
      required: ["name", "content", "expected_version"],
      additionalProperties: false
    };
  }
  return {
    type: "object",
    properties: {
      scope,
      name: documentName,
      expected_version: expectedVersion
    },
    required: ["name", "expected_version"],
    additionalProperties: false
  };
}

function memoryTools(protocol) {
  return MEMORY_TOOL_NAMES.map((name) => {
    const schema = memorySchema(name);
    if (protocol === "openai_chat") {
      return { type: "function", function: { name, parameters: schema } };
    }
    if (protocol === "anthropic") {
      return { name, input_schema: schema };
    }
    return { type: "function", name, parameters: schema };
  });
}

function initialProtocolBody(protocol, sessionId, hold = false) {
  const marker = [
    MEMORY_TRIGGER,
    `MEWORK_PROTOCOL_E2E_SESSION=${sessionId}`,
    hold ? MEMORY_HOLD_TRIGGER : ""
  ].filter(Boolean).join("\n");
  const body = {
    model: MODEL_ID,
    stream: true,
    tools: memoryTools(protocol)
  };
  if (protocol === "openai_responses") {
    body.input = [{
      role: "user",
      content: [{ type: "input_text", text: marker }]
    }];
  } else if (protocol === "anthropic") {
    body.max_tokens = 512;
    body.messages = [{ role: "user", content: marker }];
  } else {
    body.messages = [{ role: "user", content: marker }];
  }
  return body;
}

function appendToolExchange(body, protocol, call, output, isError = false) {
  const outputText = typeof output === "string" ? output : JSON.stringify(output);
  if (protocol === "openai_chat") {
    body.messages.push({
      role: "assistant",
      content: null,
      tool_calls: [{
        id: call.id,
        type: "function",
        function: { name: call.name, arguments: JSON.stringify(call.input) }
      }]
    });
    body.messages.push({
      role: "tool",
      tool_call_id: call.id,
      content: outputText
    });
    return;
  }
  if (protocol === "openai_responses") {
    body.input.push({
      type: "function_call",
      id: `fc_history_${call.id}`,
      call_id: call.id,
      name: call.name,
      arguments: JSON.stringify(call.input),
      status: "completed"
    });
    body.input.push({
      type: "function_call_output",
      call_id: call.id,
      output: outputText
    });
    return;
  }
  body.messages.push({
    role: "assistant",
    content: [{
      type: "tool_use",
      id: call.id,
      name: call.name,
      input: call.input
    }]
  });
  body.messages.push({
    role: "user",
    content: [{
      type: "tool_result",
      tool_use_id: call.id,
      content: [{ type: "text", text: outputText }],
      is_error: isError
    }]
  });
}

function sseValues(text) {
  const values = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.startsWith("data:")) continue;
    const data = line.slice(5).trim();
    if (!data || data === "[DONE]") continue;
    try {
      values.push(JSON.parse(data));
    } catch {
      throw new Error("mock 返回了无法解析的 SSE data 帧");
    }
  }
  return values;
}

function extractToolCall(protocol, text) {
  const values = sseValues(text);
  if (protocol === "openai_chat") {
    for (const value of values) {
      const call = value.choices?.[0]?.delta?.tool_calls?.[0];
      if (!call) continue;
      return {
        id: call.id,
        name: call.function?.name,
        input: JSON.parse(call.function?.arguments ?? "{}")
      };
    }
  } else if (protocol === "openai_responses") {
    for (const value of values) {
      const item = value.item;
      if (item?.type !== "function_call" || item.status !== "completed") continue;
      return {
        id: item.call_id,
        name: item.name,
        input: JSON.parse(item.arguments ?? "{}")
      };
    }
  } else {
    let call = null;
    let partialJson = "";
    for (const value of values) {
      const block = value.content_block;
      if (value.type === "content_block_start" && block?.type === "tool_use") {
        call = { id: block.id, name: block.name, input: block.input ?? {} };
      }
      if (value.type === "content_block_delta" && value.delta?.type === "input_json_delta") {
        partialJson += value.delta.partial_json ?? "";
      }
    }
    if (call) {
      call.input = partialJson ? JSON.parse(partialJson) : call.input;
      return call;
    }
  }
  throw new Error(`${protocol} 没有返回预期工具调用`);
}

async function postProtocol(baseUrl, protocol, body) {
  const response = await fetch(`${baseUrl}${PROTOCOLS[protocol].path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body)
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${protocol} mock 请求失败（HTTP ${response.status}）：${text.slice(0, 300)}`);
  }
  return text;
}

async function getStatus(serverUrl, sessionId) {
  const response = await fetch(
    `${serverUrl}/memory-e2e/status?session=${encodeURIComponent(sessionId)}`,
    { cache: "no-store" }
  );
  if (!response.ok) throw new Error(`memory status 返回 HTTP ${response.status}`);
  return response.json();
}

async function waitForHealth(serverUrl, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let lastStatus = "unreachable";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${serverUrl}/health`, { cache: "no-store" });
      if (response.ok) return;
      lastStatus = `HTTP ${response.status}`;
    } catch (error) {
      lastStatus = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`等待 memory mock 超时：${lastStatus}`);
}

async function waitForUrl(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastStatus = "unreachable";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { cache: "no-store" });
      if (response.ok) return;
      lastStatus = `HTTP ${response.status}`;
    } catch (error) {
      lastStatus = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  throw new Error(`等待 ${url} 超时：${lastStatus}`);
}

async function waitForHeld(serverUrl, sessionId, protocol, timeoutMs = 5_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const status = await getStatus(serverUrl, sessionId);
      if (status.protocols?.[protocol]?.held === true) return status;
    } catch {
      // The session is created only after the held provider request reaches the mock.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`${protocol} held response 没有进入可释放状态`);
}

function assertEqual(actual, expected, code) {
  if (actual !== expected) {
    throw new Error(`${code}：期望 ${JSON.stringify(expected)}，实际 ${JSON.stringify(actual)}`);
  }
}

async function exerciseProtocol(baseUrl, serverUrl, protocol, runId) {
  const sessionId = `memory-${PROTOCOLS[protocol].path.split("/").at(-1)}-${runId}`;
  const body = initialProtocolBody(protocol, sessionId);

  const forgedCall = extractToolCall(protocol, await postProtocol(baseUrl, protocol, body));
  assertEqual(forgedCall.name, "memory_upsert", `${protocol} forged tool`);
  assertEqual(forgedCall.input.modelId, "forged-model", `${protocol} forged modelId`);
  appendToolExchange(body, protocol, forgedCall, "unknown field `modelId`", true);

  const upsertCall = extractToolCall(protocol, await postProtocol(baseUrl, protocol, body));
  assertEqual(upsertCall.name, "memory_upsert", `${protocol} upsert tool`);
  if (Object.hasOwn(upsertCall.input, "modelId")) {
    throw new Error(`${protocol} 合法 upsert 不得包含 modelId`);
  }
  const content = upsertCall.input.content;
  const documentName = upsertCall.input.name;
  const saved = {
    operation: "upsert",
    status: "saved",
    modelId: MODEL_ID,
    scope: "project",
    name: documentName,
    version: 1,
    bytes: Buffer.byteLength(content),
    updatedAt: "2026-07-24T00:00:00Z"
  };
  appendToolExchange(body, protocol, upsertCall, saved);

  const readCall = extractToolCall(protocol, await postProtocol(baseUrl, protocol, body));
  assertEqual(readCall.name, "memory_read", `${protocol} read tool`);
  assertEqual(readCall.input.name, documentName, `${protocol} read name`);
  appendToolExchange(body, protocol, readCall, {
    operation: "read",
    status: "ok",
    modelId: MODEL_ID,
    scope: "project",
    name: documentName,
    version: 1,
    bytes: Buffer.byteLength(content),
    content,
    updatedAt: "2026-07-24T00:00:00Z"
  });

  const finalText = await postProtocol(baseUrl, protocol, body);
  if (!finalText.includes(PROTOCOLS[protocol].finalLabel)) {
    throw new Error(`${protocol} 未返回 memory final 标签`);
  }
  const status = await getStatus(serverUrl, sessionId);
  const state = status.protocols?.[protocol];
  for (const flag of [
    "schemaValidated",
    "forgedRejected",
    "upsertValidated",
    "readValidated",
    "finalized"
  ]) {
    assertEqual(state?.[flag], true, `${protocol} status ${flag}`);
  }
  assertEqual(state?.modelId, MODEL_ID, `${protocol} status modelId`);
  const statusText = JSON.stringify(status);
  for (const privateValue of [content, documentName, forgedCall.id, upsertCall.id, readCall.id]) {
    if (statusText.includes(privateValue)) {
      throw new Error(`${protocol} content-free status 泄漏私有状态`);
    }
  }
}

async function exerciseHeldRelease(baseUrl, serverUrl, outcome, runId) {
  const protocol = outcome === "retry" ? "openai_chat" : "openai_responses";
  const sessionId = `memory-hold-${outcome}-${runId}`;
  const body = initialProtocolBody(protocol, sessionId, true);
  const heldRequest = fetch(`${baseUrl}${PROTOCOLS[protocol].path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body)
  });
  await waitForHeld(serverUrl, sessionId, protocol);
  const release = await fetch(
    `${serverUrl}/memory-e2e/release?session=${encodeURIComponent(sessionId)}`
      + `&protocol=${encodeURIComponent(protocol)}&outcome=${outcome}`,
    { method: "POST" }
  );
  if (!release.ok) throw new Error(`${outcome} release 返回 HTTP ${release.status}`);
  const releaseStatus = await release.json();
  assertEqual(releaseStatus.outcome, outcome, `${outcome} release outcome`);
  const heldResponse = await heldRequest;
  const heldText = await heldResponse.text();
  if (outcome === "retry") {
    assertEqual(heldResponse.status, 500, "retry held response status");
    const retryStatus = await getStatus(serverUrl, sessionId);
    assertEqual(
      retryStatus.protocols?.[protocol]?.holdReleased,
      true,
      "retry holdReleased"
    );
    const retryCall = extractToolCall(protocol, await postProtocol(baseUrl, protocol, body));
    assertEqual(retryCall.input.modelId, "forged-model", "retry resumed forged call");
  } else {
    assertEqual(heldResponse.status, 200, "success held response status");
    if (!heldText.includes(PROTOCOLS[protocol].finalLabel)) {
      throw new Error("success release 未返回 final 标签");
    }
    const successStatus = await getStatus(serverUrl, sessionId);
    assertEqual(successStatus.protocols?.[protocol]?.finalized, true, "success finalized");
  }
}

async function runCapturedMockSelfCheck(runId) {
  const child = spawnOwned(process.execPath, [mockScript], {
    env: childEnvironment({
      MEWORK_MEMORY_E2E_RUN_ID: runId,
      MEWORK_MEMORY_E2E_SELF_CHECK: "1"
    }),
    stdio: ["ignore", "pipe", "pipe"]
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (text) => {
    stdout = `${stdout}${text}`.slice(-64 * 1024);
  });
  child.stderr.on("data", (text) => {
    stderr = `${stderr}${text}`.slice(-64 * 1024);
  });
  const result = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  if (
    result.code !== 0
    || !stdout.includes("SELF_CHECK protocol-e2e-mock memory PASS")
  ) {
    throw new Error(
      `protocol mock 纯自测失败（${result.signal ?? result.code ?? "unknown"}）：`
      + `${stderr || stdout}`
    );
  }
}

async function runSelfCheck() {
  const runId = `self-${Date.now().toString(36)}`;
  await assertPortFailClosedSelfCheck();
  await runCapturedMockSelfCheck(runId);
  const port = await reserveEphemeralPort();
  await assertPortAvailable(port);
  const environment = childEnvironment({
    MEWORK_PROTOCOL_E2E_PORT: String(port),
    MEWORK_MEMORY_E2E_RUN_ID: runId
  });
  delete environment.MEWORK_MEMORY_E2E_SELF_CHECK;
  const child = spawnOwned(process.execPath, [mockScript], {
    env: environment,
    stdio: ["ignore", "pipe", "pipe"]
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (text) => {
    stderr = `${stderr}${text}`.slice(-64 * 1024);
  });
  const serverUrl = `http://${HOST}:${port}`;
  try {
    await waitForHealth(serverUrl);
    for (const protocol of Object.keys(PROTOCOLS)) {
      await exerciseProtocol(serverUrl, serverUrl, protocol, runId);
    }
    await exerciseHeldRelease(serverUrl, serverUrl, "retry", runId);
    await exerciseHeldRelease(serverUrl, serverUrl, "success", runId);
  } catch (error) {
    if (stderr) {
      throw new Error(`${error instanceof Error ? error.message : String(error)}\nmock stderr: ${stderr}`);
    }
    throw error;
  } finally {
    await stopOwnedChild(child);
  }
  process.stdout.write("SELF_CHECK memory-e2e runner PASS\n");
}

async function runMockServer() {
  const port = environmentPort("MEWORK_PROTOCOL_E2E_PORT", 18080);
  await assertPortAvailable(port);
  const runId = `mock-${Date.now().toString(36)}`;
  const environment = childEnvironment({
    MEWORK_PROTOCOL_E2E_PORT: String(port),
    MEWORK_MEMORY_E2E_RUN_ID: runId
  });
  delete environment.MEWORK_MEMORY_E2E_SELF_CHECK;
  const child = spawnOwned(process.execPath, [mockScript], {
    env: environment,
    stdio: "inherit"
  });
  const serverUrl = `http://${HOST}:${port}`;
  await waitForHealth(serverUrl);
  process.stdout.write(`READY memory-e2e-mock ${serverUrl}/v1\n`);
  process.stdout.write(
    `STATUS ${serverUrl}/memory-e2e/status?session=<session-id>\n`
  );
  const exit = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  if (!shuttingDown && exit.code !== 0) {
    throw new Error(`memory mock 提前退出（${exit.signal ?? exit.code ?? "unknown"}）`);
  }
}

async function runBrowserDevFixture() {
  const frontendPort = environmentPort("MEWORK_BROWSER_DEV_FRONTEND_PORT", 1520);
  const backendPort = environmentPort("MEWORK_BROWSER_DEV_BACKEND_PORT", 1530);
  const protocolPort = environmentPort("MEWORK_PROTOCOL_E2E_PORT", 18100);
  if (new Set([frontendPort, backendPort, protocolPort]).size !== 3) {
    throw new Error("记忆浏览器 E2E 的三个端口必须互不相同");
  }
  for (const port of [frontendPort, backendPort, protocolPort]) {
    await assertPortAvailable(port);
  }

  const fixture = createMemoryE2eWorkspaceFixture({
    runId: randomBytes(12).toString("hex"),
    marker: randomBytes(32).toString("hex")
  });
  activeBrowserFixture = { fixture };
  const cargoTargetDir = path.resolve(
    process.env.MEWORK_MEMORY_E2E_CARGO_TARGET_DIR?.trim()
      || path.join(root, "src-tauri", "target-memory-browser-e2e")
  );
  const commonEnvironment = {
    ...fixture.environment,
    MEWORK_BROWSER_DEV_DATA_IDENTIFIER: fixture.dataIdentifier,
    MEWORK_MEMORY_E2E_RUN_ID: fixture.runId,
    MEWORK_BROWSER_DEV_FRONTEND_PORT: String(frontendPort),
    MEWORK_BROWSER_DEV_BACKEND_PORT: String(backendPort),
    MEWORK_PROTOCOL_E2E_PORT: String(protocolPort),
    CARGO_TARGET_DIR: cargoTargetDir,
    VITE_MEMORY_E2E_ENABLED: "1",
    VITE_MEMORY_E2E_RUN_ID: fixture.runId,
    VITE_MEMORY_E2E_PROTOCOL_BASE_URL: `http://${HOST}:${protocolPort}/v1`
  };
  const mockEnvironment = childEnvironment(commonEnvironment);
  delete mockEnvironment.MEWORK_MEMORY_E2E_SELF_CHECK;
  const mock = spawnOwned(process.execPath, [mockScript], {
    env: mockEnvironment,
    stdio: "inherit"
  });
  const mockTermination = childTermination(mock, "memory protocol mock");
  const protocolOrigin = `http://${HOST}:${protocolPort}`;
  await Promise.race([
    waitForHealth(protocolOrigin, 15_000),
    mockTermination.then((exit) => {
      throw new Error(
        `${exit.label} 启动前退出（${
          exit.error?.message ?? exit.signal ?? exit.code ?? "unknown"
        }）`
      );
    })
  ]);

  const browserEnvironment = childEnvironment(commonEnvironment);
  delete browserEnvironment.MEWORK_MEMORY_E2E_SELF_CHECK;
  const browserDev = spawnOwned(process.execPath, [browserDevScript], {
    env: browserEnvironment,
    stdio: "inherit"
  });
  const browserTermination = childTermination(browserDev, "browser-dev");
  const backendOrigin = `http://${HOST}:${backendPort}`;
  const frontendOrigin = `http://${HOST}:${frontendPort}`;
  const pageUrl = `${frontendOrigin}/memory-e2e.html`;
  await Promise.race([
    Promise.all([
      waitForHealth(backendOrigin, 5 * 60_000),
      waitForUrl(pageUrl, 5 * 60_000)
    ]),
    browserTermination.then((exit) => {
      throw new Error(
        `${exit.label} 就绪前退出（${
          exit.error?.message ?? exit.signal ?? exit.code ?? "unknown"
        }）`
      );
    })
  ]);

  process.stdout.write(`READY memory-browser-e2e ${pageUrl}\n`);
  process.stdout.write(`PROVIDER_BASE_URL ${protocolOrigin}/v1\n`);
  process.stdout.write(`RUN_ID ${fixture.runId}\n`);
  process.stdout.write(`DATA_IDENTIFIER ${fixture.dataIdentifier}\n`);
  process.stdout.write(
    "WORKSPACE picker returns one host-generated canonical temporary directory; renderer sends no path\n"
  );

  const exit = await Promise.race([mockTermination, browserTermination]);
  if (!shuttingDown) {
    throw new Error(
      `${exit.label} 提前退出（${
        exit.error?.message ?? exit.signal ?? exit.code ?? "unknown"
      }）`
    );
  }
}

async function main() {
  const args = process.argv.slice(2);
  const allowed = new Set(["--self-check", "--browser-dev"]);
  const unknown = args.filter((arg) => !allowed.has(arg));
  if (
    unknown.length > 0
    || new Set(args).size !== args.length
    || args.length > 1
  ) {
    throw new Error(`未知或重复参数：${unknown.join(", ") || args.join(", ")}`);
  }
  if (args.includes("--self-check")) {
    await runSelfCheck();
    return;
  }
  if (args.includes("--browser-dev")) {
    await runBrowserDevFixture();
    return;
  }
  await runMockServer();
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    shuttingDown = true;
    void stopOwnedChildren()
      .then(() => cleanupActiveBrowserFixture())
      .then(() => process.exit(0))
      .catch((error) => {
        process.stderr.write(
          `FAIL memory-e2e cleanup ${error instanceof Error ? error.message : String(error)}\n`
        );
        process.exit(1);
      });
  });
}

let exitCode = 0;
try {
  await main();
} catch (error) {
  exitCode = 1;
  process.stderr.write(
    `FAIL memory-e2e ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`
  );
} finally {
  shuttingDown = true;
  try {
    await stopOwnedChildren();
    cleanupActiveBrowserFixture();
  } catch (error) {
    exitCode = 1;
    process.stderr.write(
      `FAIL memory-e2e cleanup ${error instanceof Error ? error.message : String(error)}\n`
    );
  }
}
process.exit(exitCode);
