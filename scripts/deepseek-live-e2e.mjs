// DeepSeek official-API three-protocol end-to-end test runner, including `--mock`.
//
// Live mode (default):
//   1. Uses the key to GET /models and select an available model (prefer flash; override
//      with --model=).
//   2. Injects its environment and runs the ignored `api::tests::deepseek_live` tests:
//        deepseek_live_chat_completions_tool_loop
//        deepseek_live_responses_tool_loop
//        deepseek_live_anthropic_messages_tool_loop
//        deepseek_live_native_search_responses
//        deepseek_live_native_search_anthropic
//        deepseek_live_native_search_chat_is_a_repairable_refusal
//        deepseek_live_background_workflow_write_chat
//        deepseek_live_background_workflow_write_responses
//        deepseek_live_background_workflow_write_anthropic
//   The key is never written to the repository; provide it through
//   MEWORK_DEEPSEEK_LIVE_API_KEY or --key=.
//
// Mock mode (`--mock`):
//   Starts a local three-protocol SSE server and validates the request-body tool wire
//   contract: all 26 public tools, blank seed descriptions, and selected schema bounds.
//   It drives the same Cargo tests through tool call, host execution, result feedback,
//   and final answer. Native search uses separate fixtures, including a failed
//   `web_search_call` output item that must not interrupt an otherwise valid stream.
//   No network or real key is required.
//
// Usage:
//   MEWORK_DEEPSEEK_LIVE_API_KEY=sk-... npm run test:deepseek-e2e
//   npm run test:deepseek-e2e -- --mock
//   node scripts/deepseek-live-e2e.mjs --key=sk-... [--model=...]
//     [--filter=chat|responses|anthropic|native|bgworkflow]

import { spawn } from "node:child_process";
import http from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const KEY_ENV = "MEWORK_DEEPSEEK_LIVE_API_KEY";
const MODEL_ENV = "MEWORK_DEEPSEEK_LIVE_MODEL";
const OPENAI_BASE = process.env.MEWORK_DEEPSEEK_LIVE_CHAT_BASE_URL?.trim()
  || "https://api.deepseek.com";
const PUBLIC_TOOL_COUNT = 26;
const WIRE_TOOL_COUNT = PUBLIC_TOOL_COUNT;

function argumentValue(name) {
  const prefix = `--${name}=`;
  const raw = process.argv.find((argument) => argument.startsWith(prefix));
  return raw ? raw.slice(prefix.length).trim() : "";
}

function fail(message) {
  console.error(`[deepseek-live-e2e] ${message}`);
  process.exit(1);
}

const mockMode = process.argv.includes("--mock");
const filter = argumentValue("filter");
// Use `deepseek_live::` rather than `deepseek_live`: substring matching would include
// the long-running `deepseek_live_storm::` stress test.
const testFilter = {
  "": "deepseek_live::",
  chat: "deepseek_live_chat_completions_tool_loop",
  responses: "deepseek_live_responses_tool_loop",
  anthropic: "deepseek_live_anthropic_messages_tool_loop",
  native: "deepseek_live_native_search",
  bgworkflow: "deepseek_live_background_workflow_write",
  storm: "deepseek_live_storm::",
}[filter];
if (testFilter === undefined) {
  fail(`--filter 只接受 chat | responses | anthropic | native | bgworkflow | storm，收到：${filter}`);
}
// Mock mode excludes background-workflow tests because they require a real model to
// choose `write`; the mock server replays a fixed `ls` tool loop.
const mockSkips = ["deepseek_live_background_workflow_write"];
// The stress test targets this model; other tests use the discovered flash model.
const STORM_MODEL = "deepseek-v4-flash-vision-exp";

function runCargo(extraEnv) {
  const cargo = spawn(
    "cargo",
    [
      "test",
      "--lib",
      "--",
      testFilter,
      "--ignored",
      "--nocapture",
      "--test-threads=1",
      ...(mockMode ? mockSkips.flatMap((name) => ["--skip", name]) : []),
    ],
    {
      cwd: join(root, "src-tauri"),
      stdio: "inherit",
      shell: process.platform === "win32",
      env: { ...process.env, ...extraEnv },
    },
  );
  return new Promise((resolvePromise) => {
    cargo.on("exit", (code) => resolvePromise(code ?? 1));
  });
}

// --------------------------------------------------------------- Mock mode

const wireViolations = [];

function violation(message) {
  wireViolations.push(message);
  console.error(`[mock] WIRE VIOLATION: ${message}`);
}

// Asserts the tool-injection wire contract for each request body.
function assertToolInjection(protocol, body) {
  const tools = body.tools ?? [];
  const functions = tools.map((tool) => {
    if (protocol === "chat") return tool.function ?? {};
    return tool;
  });
  if (functions.length !== WIRE_TOOL_COUNT) {
    violation(`${protocol}: tools 数量 ${functions.length} != ${WIRE_TOOL_COUNT}`);
  }
  // Assert that descriptions contain no prose, not that the key is absent. AI SDK
  // providers may emit an empty `description` string; the host contract requires
  // blank seed descriptions while allowing dependencies to choose field omission.
  const withProse = functions.filter((fn) =>
    typeof fn.description === "string" && fn.description.trim() !== "");
  if (withProse.length) {
    violation(`${protocol}: 种子描述应留空，但出现了非空 description：`
      + withProse.map((fn) => fn.name).join(", "));
  }
  const byName = new Map(functions.map((fn) => [fn.name, fn]));
  const schemaOf = (name) => {
    const fn = byName.get(name);
    if (!fn) return null;
    return protocol === "anthropic" ? fn.input_schema : fn.parameters;
  };
  const ls = schemaOf("ls");
  if (!ls || ls.properties?.depth?.maximum !== 8 || ls.properties?.depth?.minimum !== 0) {
    violation(`${protocol}: ls.depth 应带 minimum 0 / maximum 8，实际 ${JSON.stringify(ls?.properties?.depth)}`);
  }
  if (typeof ls?.description !== "string" || !ls.description.length) {
    violation(`${protocol}: ls schema 根部缺少事实性描述`);
  }
  const askUser = schemaOf("ask_user");
  if (askUser?.properties?.questions?.maxItems !== 4) {
    violation(`${protocol}: ask_user.questions 应 maxItems 4`);
  }
  const taskWait = schemaOf("task_wait");
  const timeout = taskWait?.properties?.timeout_seconds ?? {};
  if (timeout.minimum !== 5 || timeout.maximum !== 600 || timeout.default !== 60) {
    violation(`${protocol}: task_wait.timeout_seconds 应 5–600 默认 60，实际 ${JSON.stringify(timeout)}`);
  }
}

function namedEvent(name, value) {
  return `event: ${name}\r\ndata: ${JSON.stringify(value)}\r\n\r\n`;
}

function dataEvent(value) {
  return `data: ${JSON.stringify(value)}\r\n\r\n`;
}

const usageChat = {
  prompt_tokens: 10,
  prompt_tokens_details: { cached_tokens: 2 },
  completion_tokens: 5,
  total_tokens: 15,
};

function chatFrames(body, kind) {
  const head = { id: "chatcmpl_live_mock", object: "chat.completion.chunk", created: 0, model: body.model };
  if (kind === "tool") {
    return [
      dataEvent({ ...head, choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{
        index: 0, id: "call_ls_1", type: "function",
        function: { name: "ls", arguments: JSON.stringify({ path: "." }) },
      }] }, finish_reason: null }] }),
      dataEvent({ ...head, choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] }),
      dataEvent({ ...head, choices: [], usage: usageChat }),
      "data: [DONE]\r\n\r\n",
    ];
  }
  return [
    dataEvent({ ...head, choices: [{ index: 0, delta: { content: "MEWORK_E2E_OK 2" }, finish_reason: null }] }),
    dataEvent({ ...head, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }),
    dataEvent({ ...head, choices: [], usage: usageChat }),
    "data: [DONE]\r\n\r\n",
  ];
}

function responseEnvelope(body, status, output) {
  return {
    id: "resp_live_mock", object: "response", created_at: 0, model: body.model,
    status, output,
    usage: {
      input_tokens: 10,
      input_tokens_details: { cached_tokens: 2 },
      output_tokens: 5,
      total_tokens: 15,
    },
  };
}

function responsesFrames(body, kind) {
  if (kind === "tool") {
    const argumentsText = JSON.stringify({ path: "." });
    const started = { type: "function_call", id: "fc_live_mock", status: "in_progress", call_id: "call_ls_1", name: "ls", arguments: "" };
    const completed = { ...started, status: "completed", arguments: argumentsText };
    return [
      namedEvent("response.created", { type: "response.created", response: responseEnvelope(body, "in_progress", []) }),
      namedEvent("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: started }),
      namedEvent("response.function_call_arguments.delta", { type: "response.function_call_arguments.delta", output_index: 0, item_id: started.id, delta: argumentsText }),
      namedEvent("response.function_call_arguments.done", { type: "response.function_call_arguments.done", output_index: 0, item_id: started.id, arguments: argumentsText }),
      namedEvent("response.output_item.done", { type: "response.output_item.done", output_index: 0, item: completed }),
      namedEvent("response.completed", { type: "response.completed", response: responseEnvelope(body, "completed", [completed]) }),
    ];
  }
  const text = "MEWORK_E2E_OK 2";
  const started = { type: "message", id: "msg_live_mock", status: "in_progress", role: "assistant", content: [] };
  const part = { type: "output_text", text, annotations: [] };
  const completed = { ...started, status: "completed", content: [part] };
  return [
    namedEvent("response.created", { type: "response.created", response: responseEnvelope(body, "in_progress", []) }),
    namedEvent("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: started }),
    namedEvent("response.output_text.delta", { type: "response.output_text.delta", output_index: 0, item_id: started.id, content_index: 0, delta: text }),
    namedEvent("response.output_item.done", { type: "response.output_item.done", output_index: 0, item: completed }),
    namedEvent("response.completed", { type: "response.completed", response: responseEnvelope(body, "completed", [completed]) }),
  ];
}

function anthropicFrames(body, kind) {
  const block = kind === "tool"
    ? {
      start: { type: "tool_use", id: "toolu_ls_1", name: "ls", input: {} },
      delta: { type: "input_json_delta", partial_json: JSON.stringify({ path: "." }) },
    }
    : {
      start: { type: "text", text: "" },
      delta: { type: "text_delta", text: "MEWORK_E2E_OK 2" },
    };
  return [
    namedEvent("message_start", {
      type: "message_start",
      message: {
        id: "msg_live_mock", type: "message", role: "assistant", content: [], model: body.model,
        stop_reason: null, stop_sequence: null,
        usage: { input_tokens: 10, cache_creation_input_tokens: 2, cache_read_input_tokens: 3 },
      },
    }),
    namedEvent("content_block_start", { type: "content_block_start", index: 0, content_block: block.start }),
    namedEvent("content_block_delta", { type: "content_block_delta", index: 0, delta: block.delta }),
    namedEvent("content_block_stop", { type: "content_block_stop", index: 0 }),
    namedEvent("message_delta", {
      type: "message_delta",
      delta: { stop_reason: kind === "tool" ? "tool_use" : "end_turn", stop_sequence: null },
      usage: { output_tokens: 5 },
    }),
    namedEvent("message_stop", { type: "message_stop" }),
  ];
}

function secondRound(protocol, body) {
  if (protocol === "chat") {
    return (body.messages ?? []).some((message) => message.role === "tool");
  }
  if (protocol === "responses") {
    return (body.input ?? []).some((item) => item.type === "function_call_output");
  }
  return (body.messages ?? []).some((message) =>
    Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_result"));
}

// A native-search request has no client tools and exactly one provider-executed search
// tool: unversioned `web_search` for Responses or versioned `web_search_YYYYMMDD` for
// Anthropic.
function nativeSearchRequest(protocol, body) {
  const tools = body.tools ?? [];
  if (tools.length !== 1 || typeof tools[0]?.type !== "string") return false;
  if (protocol === "responses") return tools[0].type === "web_search";
  if (protocol === "anthropic") return tools[0].type.startsWith("web_search_");
  return false;
}

const NATIVE_SEARCH_TEXT = "MEWORK_NATIVE_SEARCH_OK 来源：https://example.com/source";

// Responses native-search fixture: a `web_search_call` output item terminates with
// `status:"failed"`, then the model emits final text. It ensures a failed server-tool
// item does not interrupt the stream.
function responsesNativeSearchFrames(body) {
  const failedCall = {
    type: "web_search_call", id: "ws_mock_1", status: "failed",
    action: { type: "open_page", url: "https://example.com/blocked" },
  };
  const message = {
    type: "message", id: "msg_ws_mock", status: "completed", role: "assistant",
    content: [{ type: "output_text", text: NATIVE_SEARCH_TEXT, annotations: [] }],
  };
  return [
    namedEvent("response.created", { type: "response.created", response: responseEnvelope(body, "in_progress", []) }),
    namedEvent("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { type: "web_search_call", id: failedCall.id, status: "in_progress" } }),
    namedEvent("response.output_item.done", { type: "response.output_item.done", output_index: 0, item: failedCall }),
    namedEvent("response.output_item.added", { type: "response.output_item.added", output_index: 1, item: { ...message, status: "in_progress", content: [] } }),
    namedEvent("response.output_text.delta", { type: "response.output_text.delta", output_index: 1, item_id: message.id, content_index: 0, delta: NATIVE_SEARCH_TEXT }),
    namedEvent("response.output_item.done", { type: "response.output_item.done", output_index: 1, item: message }),
    namedEvent("response.completed", { type: "response.completed", response: responseEnvelope(body, "completed", [failedCall, message]) }),
  ];
}

// Anthropic native-search fixture: `server_tool_use`, then
// `web_search_tool_result`, then final text in one stream ending with `end_turn`.
function anthropicNativeSearchFrames(body) {
  return [
    namedEvent("message_start", {
      type: "message_start",
      message: {
        id: "msg_ws_mock", type: "message", role: "assistant", content: [], model: body.model,
        stop_reason: null, stop_sequence: null,
        usage: { input_tokens: 10, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 },
      },
    }),
    namedEvent("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "server_tool_use", id: "ws_call_mock", name: "web_search", input: {}, caller: { type: "direct" } } }),
    namedEvent("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify({ query: "mock" }) } }),
    namedEvent("content_block_stop", { type: "content_block_stop", index: 0 }),
    namedEvent("content_block_start", { type: "content_block_start", index: 1, content_block: { type: "web_search_tool_result", tool_use_id: "ws_call_mock", content: [{ type: "web_search_result", title: "Mock source", url: "https://example.com/source", encrypted_content: "opaque" }] } }),
    namedEvent("content_block_stop", { type: "content_block_stop", index: 1 }),
    namedEvent("content_block_start", { type: "content_block_start", index: 2, content_block: { type: "text", text: "" } }),
    namedEvent("content_block_delta", { type: "content_block_delta", index: 2, delta: { type: "text_delta", text: NATIVE_SEARCH_TEXT } }),
    namedEvent("content_block_stop", { type: "content_block_stop", index: 2 }),
    namedEvent("message_delta", {
      type: "message_delta",
      delta: { stop_reason: "end_turn", stop_sequence: null },
      usage: { output_tokens: 9, server_tool_use: { web_search_requests: 1 } },
    }),
    namedEvent("message_stop", { type: "message_stop" }),
  ];
}

function startMockServer() {
  return new Promise((resolvePromise) => {
    const server = http.createServer((request, response) => {
      let raw = "";
      request.on("data", (chunk) => { raw += chunk; });
      request.on("end", () => {
        let protocol = null;
        if (request.url.endsWith("/chat/completions")) protocol = "chat";
        else if (request.url.endsWith("/responses")) protocol = "responses";
        else if (request.url.endsWith("/messages")) protocol = "anthropic";
        if (!protocol || request.method !== "POST") {
          response.writeHead(404).end();
          return;
        }
        let body;
        try {
          body = JSON.parse(raw);
        } catch {
          response.writeHead(400).end("invalid json");
          return;
        }
        // Native-search requests omit the client tool catalog, so the regular
        // injection contract does not apply; replay the search fixture directly.
        if (nativeSearchRequest(protocol, body)) {
          const frames = protocol === "responses"
            ? responsesNativeSearchFrames(body)
            : anthropicNativeSearchFrames(body);
          response.writeHead(200, {
            "content-type": "text/event-stream; charset=utf-8",
            "cache-control": "no-cache, no-transform",
            connection: "close",
          });
          response.end(frames.join(""));
          return;
        }
        assertToolInjection(protocol, body);
        if (wireViolations.length) {
          response.writeHead(400, { "content-type": "application/json" })
            .end(JSON.stringify({ error: { message: `wire contract violated: ${wireViolations.join(" | ")}` } }));
          return;
        }
        const kind = secondRound(protocol, body) ? "final" : "tool";
        const frames = protocol === "chat"
          ? chatFrames(body, kind)
          : protocol === "responses"
            ? responsesFrames(body, kind)
            : anthropicFrames(body, kind);
        response.writeHead(200, {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-cache, no-transform",
          connection: "close",
        });
        response.end(frames.join(""));
      });
    });
    server.listen(0, "127.0.0.1", () => {
      resolvePromise(server);
    });
  });
}

// ------------------------------------------------------------- Main flow

if (mockMode) {
  const server = await startMockServer();
  const { port } = server.address();
  const base = `http://127.0.0.1:${port}`;
  console.log(`[deepseek-live-e2e] 机制模式：本地三协议 mock 于 ${base}`);
  const code = await runCargo({
    [KEY_ENV]: "sk-mework-mechanics-only",
    [MODEL_ENV]: "mock-flash-v4",
    MEWORK_DEEPSEEK_LIVE_CHAT_BASE_URL: base,
    MEWORK_DEEPSEEK_LIVE_RESPONSES_BASE_URL: base,
    MEWORK_DEEPSEEK_LIVE_ANTHROPIC_BASE_URL: `${base}/anthropic/v1`,
  });
  server.close();
  if (wireViolations.length) {
    fail(`wire 契约违例 ${wireViolations.length} 条（见上方 [mock] 日志）`);
  }
  process.exit(code);
}

const apiKey = argumentValue("key") || process.env[KEY_ENV]?.trim() || "";
if (!apiKey) {
  fail(`缺少 API Key：设置 ${KEY_ENV} 或传 --key=sk-...（机制验证可用 --mock）`);
}

async function listModels() {
  const response = await fetch(`${OPENAI_BASE}/models`, {
    headers: { Authorization: `Bearer ${apiKey}` },
    signal: AbortSignal.timeout(30_000),
  });
  const body = await response.text();
  if (!response.ok) {
    fail(`GET /models 失败（HTTP ${response.status}）：${body.slice(0, 300)}\n` +
      "通常意味着 Key 无效或被截断；请核对后重试。");
  }
  const parsed = JSON.parse(body);
  const ids = (parsed.data ?? []).map((model) => model.id).filter(Boolean);
  if (!ids.length) fail(`/models 返回了空模型清单：${body.slice(0, 300)}`);
  return ids;
}

function resolveModel(ids) {
  const explicit = argumentValue("model") || process.env[MODEL_ENV]?.trim();
  if (explicit) {
    if (!ids.includes(explicit)) {
      console.warn(`[deepseek-live-e2e] 警告：/models 未列出 ${explicit}，仍按指定使用。可用：${ids.join(", ")}`);
    }
    return explicit;
  }
  // The stress test uses its fixed target instead of flash-model discovery.
  if (filter === "storm") {
    if (!ids.includes(STORM_MODEL)) {
      console.warn(`[deepseek-live-e2e] 警告：/models 未列出 ${STORM_MODEL}，仍按压力腿约定使用。可用：${ids.join(", ")}`);
    }
    return STORM_MODEL;
  }
  // Prefer an ID containing `flash`, then prefer v4 among multiple matches.
  const flash = ids.filter((id) => /flash/iu.test(id));
  if (flash.length === 1) return flash[0];
  if (flash.length > 1) {
    const v4 = flash.find((id) => /v?4/u.test(id));
    return v4 ?? flash.sort().at(-1);
  }
  fail(`没有找到 flash 系模型；用 --model= 指定。可用：${ids.join(", ")}`);
  return "";
}

const ids = await listModels();
const model = resolveModel(ids);
console.log(`[deepseek-live-e2e] 模型：${model}（可用 ${ids.length} 个：${ids.join(", ")}）`);
process.exit(await runCargo({ [KEY_ENV]: apiKey, [MODEL_ENV]: model }));
