// Claude Agent family checks for selfcheck.mjs.
//
// These drive the real Claude Code executable Mework ships — the CLI inside the
// Agent SDK's own platform package — through the sidecar, against a scripted
// Anthropic Messages upstream on 127.0.0.1. The CLI never talks to Anthropic:
// `ANTHROPIC_BASE_URL` and a dummy key travel in `agent.env`, which is the only
// channel that may redirect this family and only towards a loopback address.
// `request.apiKey` / `request.baseURL` are set too, with values that must appear
// nowhere upstream. When the platform package is absent (an install that skipped
// optional dependencies) the section is skipped with a warning so the rest of the
// selfcheck stays meaningful.

import { execFileSync } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

const require = createRequire(import.meta.url);

/**
 * Claude Code version the installed SDK pin ships, read from the SDK rather than
 * written down here: this assertion is about the SDK and its CLI agreeing, and
 * the deliberate gate on *which* version that is belongs to
 * `src-tauri/build.rs::CLAUDE_CODE_VERSION`. `null` when the SDK is not installed.
 */
export const BUNDLED_CLI_VERSION = (() => {
  try {
    // Via the main entry, not `.../package.json`: the SDK declares an `exports`
    // map, and Node refuses a subpath the map does not list.
    const root = path.dirname(require.resolve("@anthropic-ai/claude-agent-sdk"));
    return JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).claudeCodeVersion ?? null;
  } catch {
    return null;
  }
})();

/**
 * Locates the Claude Code executable Mework ships, or `null`.
 *
 * This mirrors the SDK's own resolution (`@anthropic-ai/claude-agent-sdk-<os>-<arch>`,
 * `-musl` first on a musl Linux host) so the checks drive the same file the
 * release does. `MEWORK_CLAUDE_EXECUTABLE` overrides it, for measuring another
 * build against these fixtures.
 */
export function resolveClaudeExecutable() {
  const override = process.env.MEWORK_CLAUDE_EXECUTABLE;
  if (override) return existsSync(override) ? override : null;
  const binary = process.platform === "win32" ? "claude.exe" : "claude";
  const musl = process.platform === "linux"
    && process.report?.getReport?.()?.header?.glibcVersionRuntime === undefined;
  const bases = process.platform === "linux"
    ? (musl ? [`linux-${process.arch}-musl`, `linux-${process.arch}`] : [`linux-${process.arch}`, `linux-${process.arch}-musl`])
    : [`${process.platform}-${process.arch}`];
  for (const base of bases) {
    try {
      const resolved = require.resolve(`@anthropic-ai/claude-agent-sdk-${base}/${binary}`);
      if (existsSync(resolved)) return resolved;
    } catch {
      // Not installed for this platform; try the next candidate.
    }
  }
  return null;
}

// ---------------------------------------------------------------- fake Anthropic upstream
//
// Answers `POST /v1/messages` with a scripted SSE stream. Each scripted response
// is a list of blocks: text | thinking (with signature) | tool_use. A response may
// instead carry `status` for an error body, or `delayMs` to stall before the first
// byte (used by the cancel check).

export function startFakeAnthropic() {
  const requests = [];
  const queue = [];
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      let parsed;
      try {
        parsed = JSON.parse(body);
      } catch {
        parsed = body;
      }
      const entry = { method: req.method, url: req.url, headers: req.headers, body: parsed };
      requests.push(entry);
      if (req.method === "POST" && req.url?.startsWith("/v1/messages")) {
        const next = queue.shift() ?? { blocks: [{ type: "text", text: "(no scripted response)" }] };
        const answer = () => {
          if (next.status) {
            res.writeHead(next.status, { "content-type": "application/json" });
            res.end(
              JSON.stringify(next.body ?? { type: "error", error: { type: "api_error", message: "scripted failure" } }),
            );
            return;
          }
          res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
          writeSse(res, next, typeof parsed === "object" && parsed ? parsed.model : "claude-fake");
        };
        if (next.delayMs) setTimeout(answer, next.delayMs).unref();
        else answer();
        return;
      }
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ type: "error", error: { type: "not_found_error", message: `no route ${req.url}` } }));
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({
        baseURL: `http://127.0.0.1:${port}`,
        requests,
        /** POST /v1/messages bodies in arrival order. */
        get calls() {
          return requests.filter((entry) => entry.method === "POST" && entry.url?.startsWith("/v1/messages"));
        },
        push: (...responses) => queue.push(...responses),
        /** Drops the next scripted response without serving it. */
        shift: () => queue.shift(),
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

function writeSse(res, response, model) {
  const send = (event, data) => res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  send("message_start", {
    type: "message_start",
    message: {
      id: `msg_${randomUUID().slice(0, 8)}`,
      type: "message",
      role: "assistant",
      model,
      content: [],
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 123, output_tokens: 1, cache_creation_input_tokens: 7, cache_read_input_tokens: 20 },
    },
  });
  let stop = "end_turn";
  response.blocks.forEach((block, index) => {
    if (block.type === "text") {
      send("content_block_start", { type: "content_block_start", index, content_block: { type: "text", text: "" } });
      for (const piece of chunk(block.text, 7)) {
        send("content_block_delta", { type: "content_block_delta", index, delta: { type: "text_delta", text: piece } });
      }
      send("content_block_stop", { type: "content_block_stop", index });
    } else if (block.type === "thinking") {
      send("content_block_start", {
        type: "content_block_start",
        index,
        content_block: { type: "thinking", thinking: "", signature: "" },
      });
      for (const piece of chunk(block.thinking, 9)) {
        send("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "thinking_delta", thinking: piece },
        });
      }
      send("content_block_delta", {
        type: "content_block_delta",
        index,
        delta: { type: "signature_delta", signature: block.signature ?? "sig_fake" },
      });
      send("content_block_stop", { type: "content_block_stop", index });
    } else if (block.type === "tool_use") {
      stop = "tool_use";
      send("content_block_start", {
        type: "content_block_start",
        index,
        content_block: { type: "tool_use", id: block.id, name: block.name, input: {} },
      });
      for (const piece of chunk(JSON.stringify(block.input ?? {}), 11)) {
        send("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "input_json_delta", partial_json: piece },
        });
      }
      send("content_block_stop", { type: "content_block_stop", index });
    }
  });
  send("message_delta", {
    type: "message_delta",
    delta: { stop_reason: response.stopReason ?? stop, stop_sequence: null },
    usage: { output_tokens: 42 },
  });
  send("message_stop", { type: "message_stop" });
  res.end();
}

function chunk(text, size) {
  const out = [];
  for (let index = 0; index < text.length; index += size) out.push(text.slice(index, index + size));
  if (out.length === 0) out.push("");
  return out;
}

// ---------------------------------------------------------------- checks

const HOST_PROMPT = "You are Mework's assistant. HOST PROMPT MARKER 7f3a.";
/** Key the CLI is meant to use: it rides in `agent.env` next to the loopback stub. */
const UPSTREAM_KEY = "sk-ant-fake-selfcheck";
/** Key placed on the request only. Nothing it touches may ever leave the sidecar. */
const REQUEST_ONLY_KEY = "sk-ant-request-only-a41c";
/** A 1×1 PNG for the `@`-mention probe. */
const PNG_1X1_PROBE = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

const TOOLS = [
  {
    name: "read_file",
    description: "Read a file from the workspace",
    inputSchema: { type: "object", properties: { path: { type: "string", description: "Path" } }, required: ["path"] },
  },
  {
    name: "bash",
    description: "Run a shell command",
    inputSchema: { type: "object", properties: { command: { type: "string" } }, required: ["command"] },
  },
  {
    name: "box",
    description: "Inert delivery box for host notifications",
    inputSchema: {
      type: "object",
      properties: { none: { type: "array", items: { type: "string" }, maxItems: 0 } },
      required: ["none"],
      additionalProperties: false,
    },
  },
];

function profileEnv() {
  const env = {};
  for (const name of ["USERPROFILE", "HOMEDRIVE", "HOMEPATH", "APPDATA", "LOCALAPPDATA", "HOME", "USER", "CLAUDE_CONFIG_DIR"]) {
    if (process.env[name]) env[name] = process.env[name];
  }
  return env;
}

function textOf(message) {
  if (typeof message?.content === "string") return message.content;
  return (message?.content ?? [])
    .filter((block) => block?.type === "text")
    .map((block) => block.text)
    .join("\n");
}

/**
 * The last message of the conversation proper. Claude Code may append a
 * `<system-reminder>` block of its own as a trailing `system`-role message (the
 * context plugin leaves out the environment, model and date ones it would
 * otherwise carry); that is CLI-authored context, not a turn the host projected,
 * so the checks on "what ends the conversation" look past it. Since 2.1.284 the
 * CLI may also append an effort marker: a `system` message with no content that
 * carries only `output_config` (the same effort as the request's top level). It
 * adds nothing the model reads and is skipped too. Only these two are skipped —
 * anything else trailing the conversation must fail.
 */
function lastTurnMessage(messages) {
  const list = Array.isArray(messages) ? messages : [];
  for (let index = list.length - 1; index >= 0; index -= 1) {
    const message = list[index];
    if (message?.role === "system" && textOf(message).includes("<system-reminder>")) continue;
    if (isEffortMarker(message)) continue;
    return message;
  }
  return undefined;
}

/** `{ role: "system", content: [], output_config }` and nothing more. */
function isEffortMarker(message) {
  if (message?.role !== "system" || !Array.isArray(message.content) || message.content.length !== 0) return false;
  const extra = Object.keys(message).filter((key) => key !== "role" && key !== "content");
  return extra.length === 1 && extra[0] === "output_config";
}

/** The host's tool-append marker (Rust `tool_append::marker_message`). */
function toolAdditionMarker(tools) {
  return { role: "system", content: "", providerOptions: { mework: { toolAddition: tools } } };
}

/**
 * `value` as the API reads it: object keys in one order (a request is parsed,
 * not hashed as text) and no prompt-cache breakpoints, which move from request
 * to request by design.
 */
function canonicalRequest(value) {
  if (Array.isArray(value)) return value.map(canonicalRequest);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value).filter((key) => key !== "cache_control").sort().map((key) => [key, canonicalRequest(value[key])]),
  );
}

/**
 * Whether request `next` only extends request `previous`: the same system
 * prompt and tool list, and `previous`'s messages as the start of its own —
 * what lets the prompt cache `previous` wrote serve `next`. `tools: false`
 * leaves the tool list out, for a step that appends a tool (a deferred tool is
 * outside the cache key; the declared ones are compared separately).
 */
function extendsRequest(previous, next, { tools = true } = {}) {
  const before = canonicalRequest(previous ?? {});
  const after = canonicalRequest(next ?? {});
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  const earlier = Array.isArray(before.messages) ? before.messages : [];
  const later = Array.isArray(after.messages) ? after.messages : [];
  const differsAt = earlier.findIndex((message, index) => !same(message, later[index]));
  const sameSystem = same(before.system, after.system);
  const sameTools = !tools || same(before.tools, after.tools);
  return {
    ok: sameSystem && sameTools && earlier.length > 0 && later.length > earlier.length && differsAt === -1,
    detail: `system=${sameSystem} tools=${sameTools} lengths=${earlier.length}/${later.length} differsAt=${differsAt}`
      + (differsAt === -1 ? "" : ` was=${JSON.stringify(earlier[differsAt]).slice(0, 240)} now=${JSON.stringify(later[differsAt] ?? null).slice(0, 240)}`),
  };
}

function describe(frame) {
  return frame.type === "done" ? JSON.stringify(frame.result).slice(0, 300) : JSON.stringify(frame.error ?? frame).slice(0, 300);
}

/**
 * Runs the claude-agent section. `sc` is the sidecar driver from selfcheck.mjs,
 * `check(name, ok, detail)` its assertion recorder, `V` the protocol version.
 */
export async function runClaudeAgentChecks({ sc, check, V, executable }) {
  const upstream = await startFakeAnthropic();
  const cwd = path.join(os.tmpdir(), "mework-selfcheck-claude-agent");
  mkdirSync(cwd, { recursive: true });
  const agentFor = (session, apiKey = UPSTREAM_KEY) => ({
    session,
    executable,
    cwd,
    env: { ...profileEnv(), ANTHROPIC_BASE_URL: upstream.baseURL, ANTHROPIC_API_KEY: apiKey },
  });
  const stepFrame = (id, session, messages, extra = {}) => ({
    v: V,
    type: "step",
    id,
    payload: {
      family: "claude-agent",
      // Both are inert for this family; the host no longer sends them at all.
      baseURL: "https://api.anthropic.com",
      apiKey: REQUEST_ONLY_KEY,
      modelId: "sonnet",
      system: HOST_PROMPT,
      messages,
      tools: TOOLS,
      maxSteps: 1,
      reasoning: "low",
      agent: agentFor(session),
      ...extra,
    },
  });
  const terminal = (id) => (frame) => (frame.type === "done" || frame.type === "error") && frame.id === id;
  const events = (id) => sc.frames.filter((frame) => frame.type === "event" && frame.id === id).map((frame) => frame.event);

  // 30b: a fresh session; the model thinks, speaks, then calls a host tool.
  upstream.push({
    blocks: [
      { type: "thinking", thinking: "Let me look at the file first.", signature: "sig_A" },
      { type: "text", text: "I will read the file." },
      { type: "tool_use", id: "toolu_01", name: "read_file", input: { path: "a.txt" } },
    ],
  });
  const first = [{ role: "user", content: "Please read a.txt" }];
  sc.send(stepFrame("ca-1", "ca-run-1", first));
  const round1 = await sc.wait(terminal("ca-1"), 60000);
  const round1Events = events("ca-1");
  check(
    "30 claude-agent：工具轮以 tool-calls 收尾，calls 用宿主裸名",
    round1.type === "done" && round1.result.finishReason === "tool-calls"
      && round1.result.calls.length === 1 && round1.result.calls[0].toolName === "read_file"
      && round1.result.calls[0].callId === "toolu_01" && round1.result.calls[0].input?.path === "a.txt"
      && round1.result.text === "I will read the file.",
    describe(round1),
  );
  check(
    "30 claude-agent：思考/正文/工具事件按序流出",
    round1Events.some((event) => event.k === "reasoning-start" && event.item === 0)
      && round1Events.some((event) => event.k === "reasoning-delta")
      && round1Events.some((event) => event.k === "reasoning-done" && typeof event.durationMs === "number")
      && round1Events.some((event) => event.k === "text-delta")
      && round1Events.some((event) => event.k === "tool-call-announced" && event.toolName === "read_file")
      && round1Events.some((event) => event.k === "tool-call" && event.callId === "toolu_01")
      && round1Events.some((event) => event.k === "usage" && event.usage.inputTokens === 150 && event.usage.cacheReadTokens === 20),
    JSON.stringify(round1Events.map((event) => event.k)),
  );
  const assistant1 = round1.type === "done" ? round1.result.responseMessages[0] : undefined;
  check(
    "30 claude-agent：responseMessages 是带签名思考的 AI SDK 形状",
    assistant1?.role === "assistant"
      && assistant1.content.some((part) => part.type === "reasoning" && part.providerOptions?.anthropic?.signature === "sig_A"
        && part.text === "Let me look at the file first.")
      && assistant1.content.some((part) => part.type === "tool-call" && part.toolCallId === "toolu_01" && part.toolName === "read_file"),
    JSON.stringify(assistant1).slice(0, 300),
  );
  const call1 = upstream.calls[0]?.body;
  const systemBlocks = Array.isArray(call1?.system) ? call1.system : typeof call1?.system === "string" ? [{ text: call1.system }] : [];
  const systemText = systemBlocks.map((block) => block.text ?? "").join("\n");
  check(
    "30 claude-agent：上游请求的 system 只含 SDK 身份句与宿主提示词，没有 Claude Code 正文",
    systemText.includes(HOST_PROMPT) && systemText.includes("Claude Agent SDK")
      && !/Claude Code, Anthropic's official CLI/.test(systemText) && !/# Tone and style|# Tool usage policy/.test(systemText)
      && systemBlocks.length <= 3,
    `${systemBlocks.length} blocks: ${systemText.slice(0, 200).replace(/\n/g, " ")}`,
  );
  // The version lock, as the upstream sees it: the CLI stamps its own version
  // into the billing header block it prepends to the system prompt, and it must
  // be the version the pinned SDK declares. A resolution that picked up some
  // other Claude Code shows up here.
  const billing = systemBlocks.map((block) => block.text ?? "").find((text) => text.includes("cc_version="));
  check(
    `30 claude-agent：跑的是 SDK 钉住的 Claude Code ${BUNDLED_CLI_VERSION ?? "（读不到版本）"}`,
    BUNDLED_CLI_VERSION !== null
      && new RegExp(`cc_version=${BUNDLED_CLI_VERSION.replace(/\./g, "\\.")}(\\D|$)`).test(billing ?? ""),
    (billing ?? "(no billing header block)").slice(0, 120),
  );
  // Left to itself the CLI attaches context of its own to the first user message:
  // an `# Environment` block (its cwd — Mework's private session folder — its
  // platform, shell and OS version), a model-identity line and today's date. The
  // context plugin's hooks module leaves all three out, so none may appear
  // anywhere in the request, and the prompt must reach upstream as the host sent
  // it. The plugin loads only where the pinned CLI honours function hooks; a pin
  // that stops doing so fails here instead of quietly telling the model about a
  // directory it must not use.
  const CLI_CONTEXT = [
    ["environment block", /You have been invoked in the following environment|^# Environment/m],
    ["model identity", /You are powered by the model/],
    ["date", /Today's date is/],
    ["session folder", new RegExp(cwd.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))],
  ];
  const requestText = [systemText, ...(call1?.messages ?? []).map(textOf)].join("\n");
  const leaked = CLI_CONTEXT.filter(([, pattern]) => pattern.test(requestText)).map(([name]) => name);
  const firstPrompt = lastTurnMessage(call1?.messages);
  check(
    "30 claude-agent：CLI 自带的环境块、模型身份句、日期都被插件略去，上游请求里一处都没有",
    leaked.length === 0,
    leaked.length === 0 ? "absent" : `leaked: ${leaked.join(", ")}`,
  );
  check(
    "30 claude-agent：首条用户消息只有宿主发来的内容",
    firstPrompt?.role === "user" && textOf(firstPrompt).trim() === "Please read a.txt",
    JSON.stringify(firstPrompt?.content ?? null).slice(0, 300),
  );
  const toolNames = (call1?.tools ?? []).map((tool) => tool.name);
  check(
    "30 claude-agent：上游 tools 恰为宿主裸名，无内置工具",
    toolNames.length === TOOLS.length && TOOLS.every((tool) => toolNames.includes(tool.name))
      && call1?.tools.find((tool) => tool.name === "read_file")?.input_schema?.properties?.path?.description === "Path",
    JSON.stringify(toolNames),
  );
  check(
    "30 claude-agent：reasoning:low → output_config.effort=low，没有关掉思考的档位",
    call1?.output_config?.effort === "low" && call1?.thinking?.type !== "disabled",
    JSON.stringify({ thinking: call1?.thinking ?? null, output_config: call1?.output_config ?? null }),
  );

  // 30c: continuation — the parked handler receives the host's result plus a folded notice.
  upstream.push({ blocks: [{ type: "text", text: "The file says hello." }] });
  const NOTIFICATION = "<task-notification>done</task-notification>";
  const noticeBoxId = `toolu_${"a1b2c3d4".repeat(6)}`;
  const continuation = [
    ...first,
    assistant1,
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: "toolu_01", toolName: "read_file", output: { type: "text", value: "hello from a.txt" } }],
    },
    { role: "assistant", content: [{ type: "tool-call", toolCallId: noticeBoxId, toolName: "box", input: { none: [] } }] },
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: noticeBoxId, toolName: "box", output: { type: "text", value: NOTIFICATION } }],
    },
  ];
  sc.send(stepFrame("ca-2", "ca-run-1", continuation));
  const round2 = await sc.wait(terminal("ca-2"), 60000);
  const call2 = upstream.calls[1]?.body;
  const lastUser2 = lastTurnMessage(call2?.messages);
  const toolResult2 = (Array.isArray(lastUser2?.content) ? lastUser2.content : []).find((block) => block.type === "tool_result");
  const resultText2 = (toolResult2?.content ?? []).map((block) => block.text ?? "").join("\n");
  check(
    "30 claude-agent：续步把结果交给挂起的 handler，同一会话继续流出正文",
    round2.type === "done" && round2.result.finishReason === "stop" && round2.result.text === "The file says hello."
      && round2.result.calls.length === 0 && upstream.calls.length === 2,
    describe(round2),
  );
  check(
    "30 claude-agent：tool_result 携带宿主结果与折叠进去的系统通知",
    toolResult2?.tool_use_id === "toolu_01" && resultText2.includes("hello from a.txt") && resultText2.includes("<task-notification>done</task-notification>"),
    JSON.stringify(lastUser2).slice(0, 300),
  );
  const call2Messages = call2?.messages ?? [];
  check(
    "30 claude-agent：续步请求保留了同一会话的思考签名",
    call2Messages.some((message) => message.role === "assistant"
      && (Array.isArray(message.content) ? message.content : []).some((block) => block.type === "thinking" && block.signature === "sig_A")),
    JSON.stringify(call2Messages.map((message) => message.role)),
  );

  // 30d: a new run resumes from host history through a synthesized transcript.
  // The history is laid out exactly as the host projects it: the round's signed
  // reasoning card lands AFTER the tool exchange as an assistant message of its
  // own (wire_history flushes the turn at a tool boundary), followed by the next
  // round's text. The transcript must merge them so the CLI keeps the signature.
  upstream.push({ blocks: [{ type: "text", text: "resumed answer" }] });
  const assistantWithoutReasoning = {
    role: "assistant",
    content: assistant1.content.filter((part) => part.type !== "reasoning"),
  };
  const reasoningOnly = {
    role: "assistant",
    content: [{
      type: "reasoning",
      text: "Let me look at the file first.",
      providerOptions: { anthropic: { signature: "sig_A" }, mework: { model: "sonnet" } },
    }],
  };
  const historyRun = [
    first[0],
    assistantWithoutReasoning,
    continuation[2],
    reasoningOnly,
    { role: "assistant", content: [{ type: "text", text: "The file says hello." }] },
    { role: "user", content: "Thanks. And now?" },
  ];
  sc.send(stepFrame("ca-3", "ca-run-2", historyRun));
  const round3 = await sc.wait(terminal("ca-3"), 60000);
  const call3 = upstream.calls[2]?.body;
  const roles3 = (call3?.messages ?? []).map((message) => message.role);
  const replayedThinking = (call3?.messages ?? []).some((message) => message.role === "assistant"
    && Array.isArray(message.content) && message.content[0]?.type === "thinking" && message.content[0]?.signature === "sig_A"
    && message.content.some((block) => block.type === "text" && block.text === "The file says hello."));
  const replayedToolPair = (call3?.messages ?? []).some((message) => Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_use" && block.id === "toolu_01" && block.name === "read_file"))
    && (call3?.messages ?? []).some((message) => Array.isArray(message.content)
      && message.content.some((block) => block.type === "tool_result" && block.tool_use_id === "toolu_01"));
  const lastTurn3 = lastTurnMessage(call3?.messages);
  check(
    "30 claude-agent：新会话经 resume+sessionStore 重放宿主历史（工具后的签名思考并入下一条 assistant、tool_use/tool_result、新用户消息）",
    round3.type === "done" && round3.result.text === "resumed answer" && replayedThinking && replayedToolPair
      && lastTurn3?.role === "user" && textOf(lastTurn3).includes("Thanks. And now?") && upstream.calls.length === 3,
    `${describe(round3)} roles=${roles3.join(",")}`,
  );

  // 30e: release tears the session down; a later continuation for the same key
  // rebuilds a session from the transcript and nudges the CLI to continue.
  sc.send({ v: V, type: "release", session: "ca-run-1" });
  sc.send({ v: V, type: "release", session: "ca-run-2" });
  sc.send({ v: V, type: "release", session: "never-existed" });
  upstream.push({
    blocks: [{ type: "tool_use", id: "toolu_02", name: "bash", input: { command: "ls" } }],
  });
  sc.send(stepFrame("ca-4", "ca-run-3", [{ role: "user", content: "list files" }]));
  const round4 = await sc.wait(terminal("ca-4"), 60000);
  check(
    "30 claude-agent：release 后新会话照常工作",
    round4.type === "done" && round4.result.calls[0]?.toolName === "bash" && round4.result.calls[0]?.callId === "toolu_02",
    describe(round4),
  );
  sc.send({ v: V, type: "release", session: "ca-run-3" });
  await new Promise((resolve) => setTimeout(resolve, 300));
  upstream.push({ blocks: [{ type: "text", text: "continued after loss" }] });
  const lostContinuation = [
    { role: "user", content: "list files" },
    round4.type === "done" ? round4.result.responseMessages[0] : { role: "assistant", content: [] },
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: "toolu_02", toolName: "bash", output: { type: "text", value: "a.txt\nb.txt" } }],
    },
  ];
  sc.send(stepFrame("ca-5", "ca-run-3", lostContinuation));
  const round5 = await sc.wait(terminal("ca-5"), 60000);
  const call5 = upstream.calls[4]?.body;
  const call5HasResult = (call5?.messages ?? []).some((message) => Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_result" && block.tool_use_id === "toolu_02"));
  const lastTurn5 = lastTurnMessage(call5?.messages);
  check(
    "30 claude-agent：会话丢失后的续步用转录+续跑提示重建，而不是失败",
    round5.type === "done" && round5.result.text === "continued after loss" && call5HasResult
      && lastTurn5?.role === "user" && textOf(lastTurn5).includes("[SYSTEM NOTIFICATION - NOT USER INPUT]"),
    describe(round5),
  );

  // 30e': a background result delivered on its own, with no round parked for it.
  // The tail is only the box pair, so nothing but the folded notice can start the
  // turn; the step must rebuild rather than demand a user message.
  sc.send({ v: V, type: "release", session: "ca-run-3" });
  await new Promise((resolve) => setTimeout(resolve, 300));
  upstream.push({ blocks: [{ type: "text", text: "noted the background result" }] });
  const wakeBoxId = `toolu_${"f0e1d2c3".repeat(6)}`;
  sc.send(
    stepFrame("ca-box", "ca-box", [
      { role: "user", content: "kick off the background job" },
      { role: "assistant", content: [{ type: "text", text: "Started it." }] },
      { role: "assistant", content: [{ type: "tool-call", toolCallId: wakeBoxId, toolName: "box", input: { none: [] } }] },
      {
        role: "tool",
        content: [{
          type: "tool-result",
          toolCallId: wakeBoxId,
          toolName: "box",
          output: { type: "text", value: "<task-notification>bg done</task-notification>" },
        }],
      },
    ]),
  );
  const boxOnly = await sc.wait(terminal("ca-box"), 60000);
  const callBox = upstream.calls.at(-1)?.body;
  const boxDelivered = (callBox?.messages ?? []).some((message) => Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_result" && block.tool_use_id === wakeBoxId))
    && JSON.stringify(callBox?.messages ?? []).includes("<task-notification>bg done</task-notification>");
  check(
    "30 claude-agent：只带 box 通知（无真工具结果）的续步照常重建会话，而不是要求用户消息",
    boxOnly.type === "done" && boxOnly.result.text === "noted the background result" && boxDelivered,
    describe(boxOnly),
  );
  sc.send({ v: V, type: "release", session: "ca-box" });

  // 30f: two tool calls in one reply; the CLI calls handlers sequentially and both
  // results must reach the next request.
  upstream.push({
    blocks: [
      { type: "tool_use", id: "toolu_p1", name: "read_file", input: { path: "one.txt" } },
      { type: "tool_use", id: "toolu_p2", name: "read_file", input: { path: "two.txt" } },
    ],
  });
  sc.send(stepFrame("ca-6", "ca-run-4", [{ role: "user", content: "read both" }]));
  const round6 = await sc.wait(terminal("ca-6"), 60000);
  check(
    "30 claude-agent：同一回复的两个 tool_use 都在 done 前报出",
    round6.type === "done" && round6.result.calls.map((call) => call.callId).join(",") === "toolu_p1,toolu_p2",
    describe(round6),
  );
  upstream.push({ blocks: [{ type: "text", text: "both read" }] });
  sc.send(
    stepFrame("ca-7", "ca-run-4", [
      { role: "user", content: "read both" },
      round6.type === "done" ? round6.result.responseMessages[0] : { role: "assistant", content: [] },
      {
        role: "tool",
        content: [
          { type: "tool-result", toolCallId: "toolu_p1", toolName: "read_file", output: { type: "text", value: "one" } },
          { type: "tool-result", toolCallId: "toolu_p2", toolName: "read_file", output: { type: "error-text", value: "two failed" } },
        ],
      },
    ]),
  );
  const round7 = await sc.wait(terminal("ca-7"), 60000);
  const call7 = upstream.calls.at(-1)?.body;
  const results7 = (call7?.messages ?? []).flatMap((message) => (Array.isArray(message.content) ? message.content : []))
    .filter((block) => block.type === "tool_result");
  check(
    "30 claude-agent：两个结果（含 is_error）都回到下一请求",
    round7.type === "done" && round7.result.text === "both read"
      && results7.some((block) => block.tool_use_id === "toolu_p1")
      && results7.some((block) => block.tool_use_id === "toolu_p2" && block.is_error === true),
    JSON.stringify(results7).slice(0, 300),
  );
  sc.send({ v: V, type: "release", session: "ca-run-4" });

  // 30f': a tool joins while a round is parked. The live session lists it
  // (`tools/list_changed`) instead of being rebuilt: the resumed request carries
  // the parked result as-is — no continue notice — and declares the new tool.
  upstream.push({ blocks: [{ type: "tool_use", id: "toolu_w1", name: "read_file", input: { path: "w.txt" } }] });
  sc.send(stepFrame("ca-w1", "ca-run-widen", [{ role: "user", content: "read w" }]));
  const widen1 = await sc.wait(terminal("ca-w1"), 60000);
  upstream.push({ blocks: [{ type: "text", text: "listed the new tool" }] });
  const LATE = { name: "late_tool", description: "Joined mid-run", inputSchema: { type: "object", properties: {} } };
  sc.send(
    stepFrame("ca-w2", "ca-run-widen", [
      { role: "user", content: "read w" },
      widen1.type === "done" ? widen1.result.responseMessages[0] : { role: "assistant", content: [] },
      {
        role: "tool",
        content: [{ type: "tool-result", toolCallId: "toolu_w1", toolName: "read_file", output: { type: "text", value: "w" } }],
      },
    ], { tools: [...TOOLS, LATE] }),
  );
  const widen2 = await sc.wait(terminal("ca-w2"), 60000);
  const callW2 = upstream.calls.at(-1)?.body;
  const lastW2 = lastTurnMessage(callW2?.messages);
  const resumedInPlace = Array.isArray(lastW2?.content)
    && lastW2.content.some((block) => block.type === "tool_result" && block.tool_use_id === "toolu_w1")
    && !JSON.stringify(callW2?.messages ?? []).includes("[SYSTEM NOTIFICATION - NOT USER INPUT]");
  check(
    "30 claude-agent：回合中途加入的工具由活会话重新列出，不重建会话",
    widen2.type === "done" && widen2.result.text === "listed the new tool" && resumedInPlace
      && (callW2?.tools ?? []).some((tool) => tool.name === "late_tool"),
    `resumedInPlace=${resumedInPlace} tools=${JSON.stringify((callW2?.tools ?? []).map((tool) => tool.name))} ${describe(widen2)}`,
  );
  sc.send({ v: V, type: "release", session: "ca-run-widen" });

  // 30f-addition: the same widening on a model the CLI knows takes tool changes,
  // and on an endpoint it treats as first-party (`ENABLE_TOOL_SEARCH` stands in
  // for the real host the stub is not). The CLI hands the tool over itself: a
  // mid-conversation system message with a `tool_addition` and the one line of
  // text it always puts beside it, the tool declared `defer_loading` — the
  // declared list the earlier requests cached is left as it was. `toolChanges`
  // is what the host sends for this model (Rust `tool_append::appends_tools`).
  const additionAgent = (session, toolChanges = true) => {
    const agent = agentFor(session);
    return { ...agent, env: { ...agent.env, ENABLE_TOOL_SEARCH: "true" }, toolChanges };
  };
  upstream.push({ blocks: [{ type: "tool_use", id: "toolu_a1", name: "read_file", input: { path: "a.txt" } }] });
  sc.send(stepFrame("ca-a1", "ca-run-addition", [{ role: "user", content: "read a" }], {
    modelId: "claude-opus-5-5",
    agent: additionAgent("ca-run-addition"),
  }));
  const addition1 = await sc.wait(terminal("ca-a1"), 60000);
  const callA1 = upstream.calls.at(-1);
  upstream.push({ blocks: [{ type: "text", text: "took the addition" }] });
  sc.send(
    stepFrame("ca-a2", "ca-run-addition", [
      { role: "user", content: "read a" },
      addition1.type === "done" ? addition1.result.responseMessages[0] : { role: "assistant", content: [] },
      {
        role: "tool",
        content: [{ type: "tool-result", toolCallId: "toolu_a1", toolName: "read_file", output: { type: "text", value: "a" } }],
      },
    ], { modelId: "claude-opus-5-5", agent: additionAgent("ca-run-addition"), tools: [...TOOLS, LATE] }),
  );
  const addition2 = await sc.wait(terminal("ca-a2"), 60000);
  const callA2 = upstream.calls.at(-1);
  const additionMessage = (callA2?.body?.messages ?? []).find((message) => message.role === "system"
    && Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_addition" && block.tool?.name === "late_tool"));
  const declaredA1 = (callA1?.body?.tools ?? []).filter((tool) => tool.defer_loading !== true).map((tool) => tool.name).sort();
  const declaredA2 = (callA2?.body?.tools ?? []).filter((tool) => tool.defer_loading !== true).map((tool) => tool.name).sort();
  const lateDeclared = (callA2?.body?.tools ?? []).find((tool) => tool.name === "late_tool");
  check(
    "30 claude-agent：回合中途加入的工具以 CLI 自己的 tool_addition 送达，声明列表不变",
    addition2.type === "done" && addition2.result.text === "took the addition" && additionMessage !== undefined
      && lateDeclared?.defer_loading === true
      && JSON.stringify(declaredA1) === JSON.stringify(declaredA2)
      && String(callA2?.headers?.["anthropic-beta"] ?? "").includes("mid-conversation-tool-changes-2026-07-01"),
    `addition=${JSON.stringify(additionMessage ?? null).slice(0, 300)} late=${JSON.stringify(lateDeclared ?? null).slice(0, 120)} before=${JSON.stringify(declaredA1)} after=${JSON.stringify(declaredA2)} ${describe(addition2)}`,
  );
  sc.send({ v: V, type: "release", session: "ca-run-addition" });

  // 30f-rebuild: the next user turn is a run of its own, and its session is
  // rebuilt from the host's history, where the addition is the host's marker
  // behind the tool result. The CLI gets it back as its own record of the
  // addition, so the rebuilt request only extends the last one: `late_tool`
  // still deferred, its `tool_addition` where it was, the cache behind it
  // intact. Dropping the marker would declare `late_tool` with the rest.
  const additionHistory = [
    { role: "user", content: "read a" },
    addition1.type === "done" ? addition1.result.responseMessages[0] : { role: "assistant", content: [] },
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: "toolu_a1", toolName: "read_file", output: { type: "text", value: "a" } }],
    },
    toolAdditionMarker(["late_tool"]),
    addition2.type === "done" ? addition2.result.responseMessages[0] : { role: "assistant", content: [] },
  ];
  upstream.push({ blocks: [{ type: "text", text: "rebuilt with the addition" }] });
  sc.send(stepFrame("ca-r1", "ca-run-rebuild", [...additionHistory, { role: "user", content: "again" }], {
    modelId: "claude-opus-5-5",
    agent: additionAgent("ca-run-rebuild"),
    tools: [...TOOLS, LATE],
  }));
  const rebuilt = await sc.wait(terminal("ca-r1"), 60000);
  const rebuiltExtends = extendsRequest(callA2?.body, upstream.calls.at(-1)?.body);
  check(
    "30 claude-agent：重建的会话把历史里追加过的工具交还 CLI，请求原样延续上一请求",
    rebuilt.type === "done" && rebuilt.result.text === "rebuilt with the addition" && rebuiltExtends.ok,
    `${rebuiltExtends.detail} ${describe(rebuilt)}`,
  );
  sc.send({ v: V, type: "release", session: "ca-run-rebuild" });

  // 30f-between: a tool the user enabled between two turns. The host marks it
  // right behind the new prompt; the rebuilt session hands it to the CLI there,
  // and the request keeps the last one's declared list and messages, the tool
  // deferred and added behind the prompt. The turn after that, rebuilt again
  // from the same history, extends it in turn.
  const BETWEEN = { name: "between_tool", description: "Enabled between turns", inputSchema: { type: "object", properties: { q: { type: "string" } } } };
  upstream.push({ blocks: [{ type: "text", text: "hi there" }] });
  sc.send(stepFrame("ca-b1", "ca-run-between-1", [{ role: "user", content: "hello" }], {
    modelId: "claude-opus-5-5",
    agent: additionAgent("ca-run-between-1"),
  }));
  const between1 = await sc.wait(terminal("ca-b1"), 60000);
  const callB1 = upstream.calls.at(-1);
  sc.send({ v: V, type: "release", session: "ca-run-between-1" });
  const betweenHistory = [
    { role: "user", content: "hello" },
    between1.type === "done" ? between1.result.responseMessages[0] : { role: "assistant", content: [] },
    { role: "user", content: "use the new tool" },
    toolAdditionMarker(["between_tool"]),
  ];
  upstream.push({ blocks: [{ type: "text", text: "it is here" }] });
  sc.send(stepFrame("ca-b2", "ca-run-between-2", betweenHistory, {
    modelId: "claude-opus-5-5",
    agent: additionAgent("ca-run-between-2"),
    tools: [...TOOLS, BETWEEN],
  }));
  const between2 = await sc.wait(terminal("ca-b2"), 60000);
  const callB2 = upstream.calls.at(-1);
  sc.send({ v: V, type: "release", session: "ca-run-between-2" });
  const b2Messages = callB2?.body?.messages ?? [];
  const promptAt = b2Messages.findIndex((message) => textOf(message) === "use the new tool");
  const behindPrompt = b2Messages[promptAt + 1];
  const betweenDeclared = (callB2?.body?.tools ?? []).find((tool) => tool.name === "between_tool");
  const declaredB1 = (callB1?.body?.tools ?? []).filter((tool) => tool.defer_loading !== true).map((tool) => tool.name).sort();
  const declaredB2 = (callB2?.body?.tools ?? []).filter((tool) => tool.defer_loading !== true).map((tool) => tool.name).sort();
  check(
    "30 claude-agent：两轮之间启用的工具紧跟新提示以 tool_addition 送达，声明列表不变",
    between2.type === "done" && between2.result.text === "it is here"
      && promptAt !== -1
      && behindPrompt?.role === "system"
      && Array.isArray(behindPrompt.content)
      && behindPrompt.content.some((block) => block.type === "tool_addition" && block.tool?.name === "between_tool")
      && betweenDeclared?.defer_loading === true
      && JSON.stringify(declaredB1) === JSON.stringify(declaredB2),
    `behind=${JSON.stringify(behindPrompt ?? null).slice(0, 300)} declared=${JSON.stringify(betweenDeclared ?? null).slice(0, 120)} before=${JSON.stringify(declaredB1)} after=${JSON.stringify(declaredB2)} ${describe(between2)}`,
  );
  upstream.push({ blocks: [{ type: "text", text: "still here" }] });
  sc.send(stepFrame("ca-b3", "ca-run-between-3", [
    ...betweenHistory,
    between2.type === "done" ? between2.result.responseMessages[0] : { role: "assistant", content: [] },
    { role: "user", content: "and again" },
  ], { modelId: "claude-opus-5-5", agent: additionAgent("ca-run-between-3"), tools: [...TOOLS, BETWEEN] }));
  const between3 = await sc.wait(terminal("ca-b3"), 60000);
  sc.send({ v: V, type: "release", session: "ca-run-between-3" });
  const betweenFirstExtends = extendsRequest(callB1?.body, callB2?.body, { tools: false });
  const betweenExtends = extendsRequest(callB2?.body, upstream.calls.at(-1)?.body);
  check(
    "30 claude-agent：两轮之间的追加不动上一请求的前缀，下一轮重建后原样延续",
    between3.type === "done" && betweenFirstExtends.ok && betweenExtends.ok,
    `first: ${betweenFirstExtends.detail} next: ${betweenExtends.detail} ${describe(between3)}`,
  );

  // 30f-no-changes: on a model the CLI does not take tool changes for, the host
  // says so and the markers are dropped. The CLI would otherwise announce the
  // tool in words of its own; instead it is declared with the rest, and nothing
  // the host did not write reaches the model.
  upstream.push({ blocks: [{ type: "text", text: "declared plainly" }] });
  sc.send(stepFrame("ca-n1", "ca-run-no-changes", betweenHistory, {
    modelId: "claude-opus-5-5",
    agent: additionAgent("ca-run-no-changes", false),
    tools: [...TOOLS, BETWEEN],
  }));
  const noChanges = await sc.wait(terminal("ca-n1"), 60000);
  sc.send({ v: V, type: "release", session: "ca-run-no-changes" });
  const callN1 = upstream.calls.at(-1)?.body;
  const plainDeclared = (callN1?.tools ?? []).find((tool) => tool.name === "between_tool");
  const n1Text = JSON.stringify(callN1?.messages ?? []);
  check(
    "30 claude-agent：不接受工具变更的模型丢弃追加标记，工具照常声明且没有 CLI 自写的公告",
    noChanges.type === "done" && plainDeclared !== undefined && plainDeclared.defer_loading !== true
      && !n1Text.includes("tool_addition") && !n1Text.includes("between_tool"),
    `declared=${JSON.stringify(plainDeclared ?? null).slice(0, 120)} messages=${n1Text.slice(0, 300)} ${describe(noChanges)}`,
  );

  // 30f-mention: the attachment pipeline is on for that addition, and nothing
  // else it makes may reach the model — an `@` mention of a local image or file
  // in the user's text included, which the context plugin cannot see when it is
  // an image.
  const mentionDir = mkdtempSync(path.join(os.tmpdir(), "mework-mention-"));
  const mentionImage = path.join(mentionDir, "probe.png");
  const mentionText = path.join(mentionDir, "probe.txt");
  writeFileSync(mentionImage, Buffer.from(PNG_1X1_PROBE, "base64"));
  writeFileSync(mentionText, "PROBE-MENTIONED-FILE-CONTENT\n");
  upstream.push({ blocks: [{ type: "text", text: "mentions seen" }] });
  sc.send(stepFrame("ca-m1", "ca-run-mention", [
    { role: "user", content: `compare @${mentionImage} with @${mentionText}` },
  ]));
  const mention = await sc.wait(terminal("ca-m1"), 60000);
  const callM1 = JSON.stringify(upstream.calls.at(-1)?.body?.messages ?? []);
  check(
    "30 claude-agent：用户消息里 @ 提及的本地图片与文件不进上游请求",
    mention.type === "done" && !callM1.includes('"type":"image"') && !callM1.includes("PROBE-MENTIONED-FILE-CONTENT"),
    callM1.slice(0, 600),
  );
  sc.send({ v: V, type: "release", session: "ca-run-mention" });
  rmSync(mentionDir, { recursive: true, force: true });

  // 30g: cancel while the upstream stalls → cancelled, and the session is gone.
  upstream.push({ delayMs: 20000, blocks: [{ type: "text", text: "never" }] });
  sc.send(stepFrame("ca-8", "ca-run-5", [{ role: "user", content: "slow" }]));
  await new Promise((resolve) => setTimeout(resolve, 2500));
  sc.send({ v: V, type: "cancel", id: "ca-8" });
  const cancelled = await sc.wait(terminal("ca-8"), 30000);
  check(
    "30 claude-agent：cancel 帧让步以 cancelled 收尾",
    cancelled.type === "error" && cancelled.error.kind === "cancelled",
    describe(cancelled),
  );

  // 30h: an upstream error body that echoes the stub key, as some gateways do.
  // The key travels only in `agent.env` (the host sends no top-level key for this
  // family), so redaction has to be fed from there. A 400 is not retried by the
  // CLI, which keeps the check bounded.
  for (const key of ["abc123", "abc1234", "abc12345", "sk-ant-fake-selfcheck", "  abc123  "]) {
    upstream.push({
      status: 400,
      body: { type: "error", error: { type: "invalid_request_error", message: `Invalid API key: ${key.trim()}` } },
    });
    const id = `ca-400-${key.length}`;
    const frame = stepFrame(id, id, [{ role: "user", content: "hi" }], { agent: agentFor(id, key.trim()) });
    delete frame.payload.apiKey;
    sc.send(frame);
    const failed = await sc.wait(terminal(id), 90000);
    check(
      "30 claude-agent：上游错误正文回显 agent.env 的 Key → permanent 且 Key 被遮盖",
      failed.type === "error" && failed.error.kind === "permanent"
        && !JSON.stringify(failed).includes(key.trim()) && failed.error.message.includes("[redacted]")
        && !sc.stderr.includes(key.trim()),
      describe(failed),
    );
  }

  // 30h': a complete tool_use cut off by max_tokens. The CLI still invokes the
  // handler, so the step must end as a tool round instead of waiting for a
  // `tool_use` stop reason that never comes (that wait parked the run for good).
  upstream.push({
    stopReason: "max_tokens",
    blocks: [{ type: "tool_use", id: "toolu_cut", name: "read_file", input: { path: "abc" } }],
  });
  sc.send(stepFrame("ca-cut-1", "ca-cut", [{ role: "user", content: "read abc" }]));
  const cut = await sc.wait(terminal("ca-cut-1"), 20000);
  check(
    "30 claude-agent：max_tokens 截断但 tool_use 完整 → 仍以 tool-calls 收尾并保留 rawFinishReason",
    cut.type === "done" && cut.result.finishReason === "tool-calls" && cut.result.rawFinishReason === "max_tokens"
      && cut.result.calls[0]?.callId === "toolu_cut",
    describe(cut),
  );
  upstream.push({ blocks: [{ type: "text", text: "after cut" }] });
  sc.send(
    stepFrame("ca-cut-2", "ca-cut", [
      { role: "user", content: "read abc" },
      cut.type === "done" ? cut.result.responseMessages[0] : { role: "assistant", content: [] },
      { role: "tool", content: [{ type: "tool-result", toolCallId: "toolu_cut", toolName: "read_file", output: { type: "text", value: "abc!" } }] },
    ]),
  );
  const afterCut = await sc.wait(terminal("ca-cut-2"), 60000);
  check(
    "30 claude-agent：截断工具轮的续步照常送达结果并继续",
    afterCut.type === "done" && afterCut.result.text === "after cut",
    describe(afterCut),
  );
  sc.send({ v: V, type: "release", session: "ca-cut" });

  // 30i: reasoning level and output ceiling reach the request as effort / thinking / max_tokens.
  upstream.push({ blocks: [{ type: "text", text: "effort ok" }] });
  sc.send(stepFrame("ca-effort", "ca-effort", [{ role: "user", content: "hi" }], { reasoning: "xhigh", maxOutputTokens: 4096 }));
  const effort = await sc.wait(terminal("ca-effort"), 60000);
  const effortBody = upstream.calls.at(-1)?.body;
  check(
    "30 claude-agent：reasoning:xhigh → output_config.effort=xhigh + adaptive thinking，maxOutputTokens → max_tokens",
    effort.type === "done" && effortBody?.output_config?.effort === "xhigh" && effortBody?.thinking?.type === "adaptive"
      && effortBody?.max_tokens === 4096,
    JSON.stringify({ thinking: effortBody?.thinking, output_config: effortBody?.output_config, max_tokens: effortBody?.max_tokens }),
  );
  sc.send({ v: V, type: "release", session: "ca-effort" });

  // 30j: `max`, the level the AI SDK has no shared name for, reaches the CLI
  // as itself; there is no level that turns thinking off.
  upstream.push({ blocks: [{ type: "text", text: "max ok" }] });
  sc.send(stepFrame("ca-max", "ca-max", [{ role: "user", content: "hi" }], { reasoning: "max" }));
  const maxEffort = await sc.wait(terminal("ca-max"), 60000);
  const maxBody = upstream.calls.at(-1)?.body;
  check(
    "30 claude-agent：reasoning:max → output_config.effort=max + adaptive thinking",
    maxEffort.type === "done" && maxBody?.output_config?.effort === "max" && maxBody?.thinking?.type === "adaptive",
    JSON.stringify({ thinking: maxBody?.thinking, output_config: maxBody?.output_config }),
  );
  sc.send({ v: V, type: "release", session: "ca-max" });

  // 30p: the host sends a bare model id plus its window; the sidecar asks for
  // the CLI's 1M budget only when the window exceeds 200k, and an id installed
  // with the suffix by an earlier version passes through. The stub key is a
  // Console-style login, whose bare-id budget is 200k, and the CLI's 1M budget
  // shows on the wire as the `context-1m` beta. The suffix itself never does.
  const budgetOf = async (id, modelId, contextWindow) => {
    upstream.push({ blocks: [{ type: "text", text: "budget ok" }] });
    const before = upstream.calls.length;
    sc.send(stepFrame(id, id, [{ role: "user", content: "hi" }], { modelId, contextWindow }));
    const frame = await sc.wait(terminal(id), 60000);
    sc.send({ v: V, type: "release", session: id });
    const call = upstream.calls.slice(before).find((entry) => (entry.body?.max_tokens ?? 0) > 1);
    return {
      done: frame.type === "done",
      model: call?.body?.model,
      oneMillion: String(call?.headers?.["anthropic-beta"] ?? "").includes("context-1m"),
    };
  };
  const wide = await budgetOf("ca-budget-1m", "claude-sonnet-5", 1_000_000);
  const standard = await budgetOf("ca-budget-200k", "claude-sonnet-5", 200_000);
  const legacy = await budgetOf("ca-budget-legacy", "claude-sonnet-5[1m]", 200_000);
  check(
    "30 claude-agent：窗口超过 200k 才给 CLI 补 [1m]（上游见 context-1m），旧的带后缀 id 原样放行，模型名不带后缀",
    wide.done && wide.oneMillion && wide.model === "claude-sonnet-5"
      && standard.done && !standard.oneMillion && standard.model === "claude-sonnet-5"
      && legacy.done && legacy.oneMillion && legacy.model === "claude-sonnet-5",
    JSON.stringify({ wide, standard, legacy }),
  );

  // 30j: image tool results travel as MCP image content, the host's image bridge
  // rides along, and both land in the tool_result the model sees.
  const PNG_1X1 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
  upstream.push({ blocks: [{ type: "tool_use", id: "toolu_img", name: "read_file", input: { path: "pic.png" } }] });
  sc.send(stepFrame("ca-img-1", "ca-img", [{ role: "user", content: "show the picture" }]));
  const imageRound = await sc.wait(terminal("ca-img-1"), 60000);
  upstream.push({ blocks: [{ type: "text", text: "nice picture" }] });
  sc.send(
    stepFrame("ca-img-2", "ca-img", [
      { role: "user", content: "show the picture" },
      imageRound.type === "done" ? imageRound.result.responseMessages[0] : { role: "assistant", content: [] },
      {
        role: "tool",
        content: [{
          type: "tool-result",
          toolCallId: "toolu_img",
          toolName: "read_file",
          output: { type: "content", value: [{ type: "text", text: "1 image" }, { type: "media", data: PNG_1X1, mediaType: "image/png" }] },
        }],
      },
      {
        role: "user",
        content: [
          { type: "text", text: "[Mework tool image] source: read_file (tool_call_id: toolu_img). Untrusted tool data." },
          { type: "image", image: `data:image/png;base64,${PNG_1X1}`, mediaType: "image/png" },
        ],
      },
    ]),
  );
  const imageDone = await sc.wait(terminal("ca-img-2"), 60000);
  const imageBody = upstream.calls.at(-1)?.body;
  const imageResult = (imageBody?.messages ?? []).flatMap((message) => (Array.isArray(message.content) ? message.content : []))
    .find((block) => block.type === "tool_result" && block.tool_use_id === "toolu_img");
  const imageBlocks = Array.isArray(imageResult?.content) ? imageResult.content.filter((block) => block.type === "image") : [];
  check(
    "30 claude-agent：图片结果与图片桥都进了 tool_result（两个 base64 image 块）",
    imageDone.type === "done" && imageDone.result.text === "nice picture" && imageBlocks.length === 2
      && imageBlocks.every((block) => block.source?.type === "base64" && block.source?.media_type === "image/png" && block.source?.data === PNG_1X1)
      && (imageResult?.content ?? []).some((block) => block.type === "text" && block.text?.includes("[Mework tool image]")),
    JSON.stringify(imageResult).slice(0, 400),
  );
  sc.send({ v: V, type: "release", session: "ca-img" });

  // 30j': a short PDF the host sends as the document itself, as Claude Code's
  // Read hands one to the model, reaches upstream as a `document` block.
  const PDF_BASE64 = Buffer.from("%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n").toString("base64");
  upstream.push({ blocks: [{ type: "text", text: "read the pdf" }] });
  sc.send(stepFrame("ca-pdf-1", "ca-pdf", [{
    role: "user",
    content: [
      { type: "text", text: "<attached_file name=\"r.pdf\" type=\"pdf\" pages=\"1\" content=\"the PDF document, attached after this element\"></attached_file>" },
      { type: "file", data: `data:application/pdf;base64,${PDF_BASE64}`, mediaType: "application/pdf", filename: "r.pdf" },
      { type: "text", text: "Summarize." },
    ],
  }]));
  const pdfDone = await sc.wait(terminal("ca-pdf-1"), 60000);
  const pdfDocuments = (upstream.calls.at(-1)?.body?.messages ?? [])
    .flatMap((message) => (Array.isArray(message.content) ? message.content : []))
    .filter((block) => block.type === "document");
  check(
    "30 claude-agent：PDF 文件部件以 document 块到达上游",
    pdfDone.type === "done" && pdfDocuments.length === 1
      && pdfDocuments[0].source?.type === "base64" && pdfDocuments[0].source?.media_type === "application/pdf"
      && pdfDocuments[0].source?.data === PDF_BASE64 && pdfDocuments[0].title === "r.pdf",
    `${describe(pdfDone)} ${JSON.stringify(pdfDocuments).slice(0, 300)}`,
  );
  sc.send({ v: V, type: "release", session: "ca-pdf" });

  // 30k: a missing executable fails fast, and says what it means — the bundled
  // CLI is part of the install, so its absence is a broken install, not
  // something the user can configure their way out of.
  sc.send(stepFrame("ca-missing", "ca-missing", [{ role: "user", content: "hi" }], { agent: { ...agentFor("ca-missing"), executable: path.join(cwd, "no-such-claude.exe") } }));
  const missing = await sc.wait(terminal("ca-missing"), 10000);
  check(
    "30 claude-agent：附带的可执行文件缺失 → permanent 并指向重新安装",
    missing.type === "error" && missing.error.kind === "permanent"
      && missing.error.message.includes("Claude Code") && missing.error.message.includes("重新安装"),
    describe(missing),
  );

  // 30l: `agent.env` is the only endpoint override this family accepts, and only
  // towards this machine. A remote address must fail the request outright rather
  // than point the user's own Claude Code login at a third party.
  const requestsBeforeRemote = upstream.requests.length;
  sc.send(stepFrame("ca-remote", "ca-remote", [{ role: "user", content: "hi" }], {
    agent: {
      session: "ca-remote",
      executable,
      cwd,
      env: { ...profileEnv(), ANTHROPIC_BASE_URL: "https://relay.example.com", ANTHROPIC_API_KEY: UPSTREAM_KEY },
    },
  }));
  const remote = await sc.wait(terminal("ca-remote"), 20000);
  check(
    "30 claude-agent：agent.env 里的远端 ANTHROPIC_BASE_URL 被拒，CLI 根本不起",
    remote.type === "error" && remote.error.kind === "permanent"
      && remote.error.message.includes("agent.env 里的 ANTHROPIC_BASE_URL 只允许本机测试桩")
      && upstream.requests.length === requestsBeforeRemote,
    describe(remote),
  );

  // 30n: the CLI's own login must stay reachable on both session paths. A
  // stand-in CLI records the environment it was started with and exits. On
  // macOS the login is a Keychain entry under the account `$USER`; and a resumed
  // session runs under the SDK's temporary `CLAUDE_CONFIG_DIR`, whose only
  // credential is a copy without its refresh token — so the secure-storage
  // directory must keep naming the user's own, or the session dies with the
  // access token. The stub key keeps the SDK away from the real Keychain.
  const fakeCli = path.join(cwd, "fake-claude-env.mjs");
  writeFileSync(
    fakeCli,
    [
      'import { writeFileSync } from "node:fs";',
      "const dump = {};",
      'for (const name of ["USER", "CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"]) {',
      "  if (name in process.env) dump[name] = process.env[name];",
      "}",
      "writeFileSync(process.env.MEWORK_SELFCHECK_ENV_DUMP, JSON.stringify(dump));",
      "process.exit(1);",
      "",
    ].join("\n"),
  );
  const cliEnvOf = async (id, messages) => {
    const dump = path.join(cwd, `${id}.json`);
    rmSync(dump, { force: true });
    const agent = agentFor(id);
    sc.send(stepFrame(id, id, messages, {
      agent: { ...agent, executable: fakeCli, env: { ...agent.env, MEWORK_SELFCHECK_ENV_DUMP: dump } },
    }));
    // The stand-in exits before speaking, so the step itself fails; only the dump matters.
    await sc.wait(terminal(id), 30000).catch(() => null);
    try {
      return JSON.parse(readFileSync(dump, "utf8"));
    } catch {
      return null;
    }
  };
  const ownStorage = process.env.CLAUDE_CONFIG_DIR ?? "";
  const expectedUser = process.platform === "win32" ? undefined : os.userInfo().username;
  const freshEnv = await cliEnvOf("ca-env-fresh", [{ role: "user", content: "hi" }]);
  check(
    "30 claude-agent：新会话的 CLI 拿到 USER，安全存储目录指向用户自己的配置目录",
    freshEnv !== null && freshEnv.CLAUDE_SECURESTORAGE_CONFIG_DIR === ownStorage
      && freshEnv.CLAUDE_CONFIG_DIR === process.env.CLAUDE_CONFIG_DIR
      && (expectedUser === undefined || freshEnv.USER === expectedUser),
    JSON.stringify(freshEnv),
  );
  const resumedEnv = await cliEnvOf("ca-env-resume", [
    { role: "user", content: "earlier" },
    { role: "assistant", content: [{ type: "text", text: "earlier answer" }] },
    { role: "user", content: "and now" },
  ]);
  check(
    "30 claude-agent：resume 会话在 SDK 的临时 CLAUDE_CONFIG_DIR 下仍读写用户自己的登录",
    resumedEnv !== null && typeof resumedEnv.CLAUDE_CONFIG_DIR === "string"
      && resumedEnv.CLAUDE_CONFIG_DIR !== ownStorage
      && resumedEnv.CLAUDE_SECURESTORAGE_CONFIG_DIR === ownStorage
      && (expectedUser === undefined || resumedEnv.USER === expectedUser),
    JSON.stringify(resumedEnv),
  );

  // 30m: every frame above carried `apiKey`/`baseURL` on the request. The CLI is
  // authenticated by the loopback stub key from `agent.env` — proof that the
  // upstream was reached at all — and the request-only key appears in no request
  // the CLI made. Putting `request.apiKey` back into `cliEnv` fails this. Every
  // request carries a key from `agent.env` and none a bearer: a CLI that reached
  // for the developer's stored login instead would be handing a real OAuth
  // token to the stub.
  const requestOnlyLeaks = upstream.requests.filter((entry) =>
    JSON.stringify({ headers: entry.headers, body: entry.body }).includes(REQUEST_ONLY_KEY));
  const upstreamKeyUsed = upstream.calls.some((entry) => JSON.stringify(entry.headers).includes(UPSTREAM_KEY));
  const storedLoginUsed = upstream.calls.filter(
    (entry) => typeof entry.headers["x-api-key"] !== "string" || entry.headers.authorization !== undefined,
  ).length;
  check(
    "30 claude-agent：request.apiKey 不进 CLI 环境（上游只见 agent.env 里的本机测试桩 Key）",
    upstreamKeyUsed && requestOnlyLeaks.length === 0 && storedLoginUsed === 0,
    `upstream_key_seen=${upstreamKeyUsed} leaks=${requestOnlyLeaks.length} stored_login=${storedLoginUsed}`,
  );

  // HEAD /api/hello is the CLI's fire-and-forget TLS preconnect to the base URL:
  // no headers, no credentials. Anything else would be traffic to explain.
  const strayRequests = upstream.requests.filter(
    (entry) => !(entry.method === "POST" && entry.url?.startsWith("/v1/messages")) && !(entry.method === "HEAD" && entry.url === "/api/hello"),
  );
  check(
    "30 claude-agent：CLI 除 /v1/messages 与预连接 HEAD /api/hello 外没有其它上游流量",
    strayRequests.length === 0,
    JSON.stringify(strayRequests.map((entry) => `${entry.method} ${entry.url}`)),
  );
  await upstream.close();
}

/** Child process ids of `pid` (Windows only; `null` elsewhere). */
function childProcessIds(pid) {
  if (process.platform !== "win32") return null;
  const out = execFileSync(
    "powershell",
    ["-NoProfile", "-Command", `(Get-CimInstance Win32_Process -Filter 'ParentProcessId=${pid}').ProcessId`],
    { encoding: "utf8" },
  );
  return out.split(/\r?\n/).map((line) => line.trim()).filter((line) => /^\d+$/.test(line)).map(Number);
}

/**
 * Sidecar shutdown with a parked Claude Code session: closing stdin must exit the
 * sidecar promptly and take the CLI child with it, so a host restart never
 * leaves a `claude` process behind. Uses its own sidecar instance.
 */
export async function runClaudeAgentShutdownCheck({ startSidecar, check, V, executable }) {
  const upstream = await startFakeAnthropic();
  const cwd = path.join(os.tmpdir(), "mework-selfcheck-claude-agent");
  mkdirSync(cwd, { recursive: true });
  const sc = startSidecar();
  sc.send({ v: V, type: "hello" });
  await sc.wait((frame) => frame.type === "ready");
  upstream.push({ blocks: [{ type: "tool_use", id: "toolu_park", name: "bash", input: { command: "sleep" } }] });
  sc.send({
    v: V,
    type: "step",
    id: "ca-park",
    payload: {
      family: "claude-agent",
      baseURL: "https://api.anthropic.com",
      apiKey: REQUEST_ONLY_KEY,
      modelId: "sonnet",
      system: HOST_PROMPT,
      messages: [{ role: "user", content: "park" }],
      tools: TOOLS,
      maxSteps: 1,
      reasoning: "low",
      agent: {
        session: "ca-park",
        executable,
        cwd,
        env: { ...profileEnv(), ANTHROPIC_BASE_URL: upstream.baseURL, ANTHROPIC_API_KEY: UPSTREAM_KEY },
      },
    },
  });
  const parked = await sc.wait((frame) => (frame.type === "done" || frame.type === "error") && frame.id === "ca-park", 60000);
  const before = childProcessIds(sc.child.pid);
  const exited = once(sc.child, "exit");
  sc.stop();
  const [code] = await Promise.race([
    exited,
    new Promise((_, reject) => setTimeout(() => reject(new Error("stdin 关闭后 8 秒未退出")), 8000).unref()),
  ]).catch((error) => [error.message]);
  await new Promise((resolve) => setTimeout(resolve, 1000));
  const survivors = before === null ? [] : before.filter((pid) => {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  });
  check(
    "30 claude-agent：stdin 关闭时挂起的会话被拆掉，侧车退出且 Claude Code 子进程不残留",
    parked.type === "done" && code === 0 && survivors.length === 0,
    `exit=${code} children_before=${JSON.stringify(before)} survivors=${JSON.stringify(survivors)}`,
  );
  for (const pid of survivors) {
    try {
      process.kill(pid);
    } catch {
      // Best effort.
    }
  }
  await upstream.close();
}
