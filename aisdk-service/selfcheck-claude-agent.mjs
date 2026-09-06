// Claude Agent family checks for selfcheck.mjs.
//
// These drive the real, locally installed Claude Code executable through the
// sidecar, against a scripted Anthropic Messages upstream on 127.0.0.1. The CLI
// never talks to Anthropic: `ANTHROPIC_BASE_URL` and a dummy key travel in
// `agent.env`, which is the only channel that may redirect this family and only
// towards a loopback address. `request.apiKey` / `request.baseURL` are set too,
// with values that must appear nowhere upstream. When no native executable is
// installed the section is skipped with a warning so the rest of the selfcheck
// stays meaningful on any machine.

import { execFileSync } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { existsSync, mkdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

/** Locates the native Claude Code executable the checks may use, or `null`. */
export function resolveClaudeExecutable() {
  const override = process.env.MEWORK_CLAUDE_EXECUTABLE;
  if (override) return existsSync(override) ? override : null;
  const name = process.platform === "win32" ? "claude.exe" : "claude";
  const candidate = path.join(os.homedir(), ".local", "bin", name);
  return existsSync(candidate) ? candidate : null;
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
];

function profileEnv() {
  const env = {};
  for (const name of ["USERPROFILE", "HOMEDRIVE", "HOMEPATH", "APPDATA", "LOCALAPPDATA", "HOME", "CLAUDE_CONFIG_DIR"]) {
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
      reasoning: "none",
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
  const toolNames = (call1?.tools ?? []).map((tool) => tool.name);
  check(
    "30 claude-agent：上游 tools 恰为宿主裸名，无内置工具",
    toolNames.length === TOOLS.length && TOOLS.every((tool) => toolNames.includes(tool.name))
      && call1?.tools.find((tool) => tool.name === "read_file")?.input_schema?.properties?.path?.description === "Path",
    JSON.stringify(toolNames),
  );
  check(
    "30 claude-agent：thinking 被 reasoning:none 关掉",
    call1?.thinking === undefined || call1?.thinking?.type === "disabled",
    JSON.stringify(call1?.thinking ?? null),
  );

  // 30c: continuation — the parked handler receives the host's result plus a folded notice.
  upstream.push({ blocks: [{ type: "text", text: "The file says hello." }] });
  const notice = "[SYSTEM NOTIFICATION - NOT USER INPUT]\n<task-notification>done</task-notification>";
  const continuation = [
    ...first,
    assistant1,
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: "toolu_01", toolName: "read_file", output: { type: "text", value: "hello from a.txt" } }],
    },
    { role: "user", content: notice },
  ];
  sc.send(stepFrame("ca-2", "ca-run-1", continuation));
  const round2 = await sc.wait(terminal("ca-2"), 60000);
  const call2 = upstream.calls[1]?.body;
  const lastUser2 = call2?.messages?.at(-1);
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
  check(
    "30 claude-agent：新会话经 resume+sessionStore 重放宿主历史（工具后的签名思考并入下一条 assistant、tool_use/tool_result、新用户消息）",
    round3.type === "done" && round3.result.text === "resumed answer" && replayedThinking && replayedToolPair
      && textOf(call3?.messages?.at(-1)).includes("Thanks. And now?") && upstream.calls.length === 3,
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
  check(
    "30 claude-agent：会话丢失后的续步用转录+续跑提示重建，而不是失败",
    round5.type === "done" && round5.result.text === "continued after loss" && call5HasResult
      && textOf(call5?.messages?.at(-1)).includes("[SYSTEM NOTIFICATION - NOT USER INPUT]"),
    describe(round5),
  );

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

  // 30k: a missing executable fails fast with installation guidance.
  sc.send(stepFrame("ca-missing", "ca-missing", [{ role: "user", content: "hi" }], { agent: { ...agentFor("ca-missing"), executable: path.join(cwd, "no-such-claude.exe") } }));
  const missing = await sc.wait(terminal("ca-missing"), 10000);
  check(
    "30 claude-agent：可执行文件缺失 → permanent 并给安装指引",
    missing.type === "error" && missing.error.kind === "permanent" && missing.error.message.includes("Claude Code"),
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

  // 30m: every frame above carried `apiKey`/`baseURL` on the request. The CLI is
  // authenticated by the loopback stub key from `agent.env` — proof that the
  // upstream was reached at all — and the request-only key appears in no request
  // the CLI made. Putting `request.apiKey` back into `cliEnv` fails this.
  const requestOnlyLeaks = upstream.requests.filter((entry) =>
    JSON.stringify({ headers: entry.headers, body: entry.body }).includes(REQUEST_ONLY_KEY));
  const upstreamKeyUsed = upstream.calls.some((entry) => JSON.stringify(entry.headers).includes(UPSTREAM_KEY));
  check(
    "30 claude-agent：request.apiKey 不进 CLI 环境（上游只见 agent.env 里的本机测试桩 Key）",
    upstreamKeyUsed && requestOnlyLeaks.length === 0,
    `upstream_key_seen=${upstreamKeyUsed} leaks=${requestOnlyLeaks.length}`,
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
      reasoning: "none",
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
