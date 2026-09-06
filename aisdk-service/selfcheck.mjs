// Node 24 on Windows can randomly exit short-lived TCP clients with 0xC0000409
// (see the repository README), so this uses one long-lived fake server and makes
// only a few sidecar requests.

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { Agent, getGlobalDispatcher, setGlobalDispatcher } from "undici";

import { resolveClaudeExecutable, runClaudeAgentChecks, runClaudeAgentShutdownCheck } from "./selfcheck-claude-agent.mjs";

const here = dirname(fileURLToPath(import.meta.url));
// Use the packaged ESM bundle by default. `--sea` (or `MEWORK_AISDK_BIN=<path>`)
// runs the same checks against the executable shipped to users.
const OVERRIDE = process.argv.includes("--sea")
  ? "dist/mework-aisdk.exe"
  : process.env.MEWORK_AISDK_BIN;
const BUNDLE = resolve(here, "dist/main.mjs");
// Protocol generation. Keep this literal because packaged artifacts do not export
// the constant; a mismatch must fail during the ready handshake.
const V = 9;

let failures = 0;
const results = [];

function check(name, ok, detail = "") {
  results.push({ name, ok, detail });
  if (!ok) failures += 1;
  process.stdout.write(`${ok ? "  ok  " : " FAIL "} ${name}${detail ? ` — ${detail}` : ""}\n`);
}

// ---------------------------------------------------------------- Fake upstream

function sse(res, chunks) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  for (const chunk of chunks) res.write(`data: ${JSON.stringify(chunk)}\n\n`);
  res.write("data: [DONE]\n\n");
  res.end();
}

/** A normal Responses stream from Codex, deliberately without content-type. */
function codexSse(res, { failed = false } = {}) {
  res.writeHead(200, {
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  const message = {
    id: "msg_selfcheck_codex",
    type: "message",
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: "codex-ok", annotations: [] }],
  };
  const response = {
    id: "resp_selfcheck_codex",
    object: "response",
    created_at: 1,
    model: "gpt-5-selfcheck-codex",
    status: "completed",
    output: [message],
    usage: { input_tokens: 3, output_tokens: 2, total_tokens: 5 },
  };
  const events = failed
    ? [{
      type: "response.failed",
      response: {
        ...response,
        status: "failed",
        error: { code: "rate_limit_exceeded", message: "slow down" },
      },
    }]
    : [
      { type: "response.created", response: { ...response, status: "in_progress", output: [], usage: null } },
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { ...message, status: "in_progress", content: [] },
      },
      {
        type: "response.content_part.added",
        item_id: message.id,
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: "", annotations: [] },
      },
      {
        type: "response.output_text.delta",
        item_id: message.id,
        output_index: 0,
        content_index: 0,
        delta: "codex-ok",
      },
      {
        type: "response.output_text.done",
        item_id: message.id,
        output_index: 0,
        content_index: 0,
        text: "codex-ok",
      },
      {
        type: "response.content_part.done",
        item_id: message.id,
        output_index: 0,
        content_index: 0,
        part: message.content[0],
      },
      { type: "response.output_item.done", output_index: 0, item: message },
      { type: "response.completed", response },
    ];
  for (const event of events) res.write(`data: ${JSON.stringify(event)}\n\n`);
  res.write("data: [DONE]\n\n");
  res.end();
}

// Anthropic streams have no OpenAI-style [DONE] sentinel; mixing them would
// misclassify dialect regressions as malformed upstream responses.
function anthropicSse(res, stopReason) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  const events = [
    {
      type: "message_start",
      message: {
        id: "msg-selfcheck",
        type: "message",
        role: "assistant",
        model: "selfcheck-model",
        content: [],
        stop_reason: null,
        stop_sequence: null,
        usage: { input_tokens: 1, output_tokens: 0 },
      },
    },
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "好" } },
    { type: "content_block_stop", index: 0 },
    {
      type: "message_delta",
      delta: { stop_reason: stopReason, stop_sequence: null },
      usage: { output_tokens: 1 },
    },
    { type: "message_stop" },
  ];
  for (const event of events) res.write(`data: ${JSON.stringify(event)}\n\n`);
  res.end();
}

/**
 * An Anthropic stream whose thinking is encrypted: the plaintext frames are empty and
 * the item's only payload is the signature.
 *
 * The three switches separate upstreams that look alike on the wire. A relay in front
 * of Claude forwards a stripped `thinking_delta` and bills thinking tokens; DeepSeek's
 * Anthropic-compatible endpoint opens a bare block with a pseudo-signature, no
 * plaintext frame, and no thinking tokens.
 */
function anthropicThinkingSse(res, { signature, thinkingDelta = true, thinkingTokens = 0 }) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  const events = [
    {
      type: "message_start",
      message: {
        id: "msg-selfcheck-thinking",
        type: "message",
        role: "assistant",
        model: "selfcheck-model",
        content: [],
        stop_reason: null,
        stop_sequence: null,
        usage: { input_tokens: 1, output_tokens: 0 },
      },
    },
    { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "", signature: "" } },
    ...(thinkingDelta
      ? [{ type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "" } }]
      : []),
    { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature } },
    { type: "content_block_stop", index: 0 },
    { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "好" } },
    { type: "content_block_stop", index: 1 },
    {
      type: "message_delta",
      delta: { stop_reason: "end_turn", stop_sequence: null },
      usage: { output_tokens: 3, output_tokens_details: { thinking_tokens: thinkingTokens } },
    },
    { type: "message_stop" },
  ];
  for (const event of events) res.write(`data: ${JSON.stringify(event)}\n\n`);
  res.end();
}

function chunk(delta, finish = null, usage) {
  const value = {
    id: "chatcmpl-selfcheck",
    object: "chat.completion.chunk",
    created: 1,
    model: "selfcheck-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  };
  if (usage) value.usage = usage;
  return value;
}

/** Bytes sent by the sidecar, used for authorization-header and request-body assertions. */
const observed = [];

const startThinkingCases = [
  { name: "body-start-only", blocks: [{ signature: "sig-body", thinking: "start-only body", deltas: [] }] },
  { name: "body-delta-wins", blocks: [{ signature: "", signatureDelta: "sig-body-delta", thinking: "complete body", deltas: ["complete ", "body"] }] },
  { name: "body-empty", blocks: [{ signature: "", thinking: "", deltas: [] }] },
  { name: "body-multi-crlf", crlf: true, blocks: [{ signature: "sig-body-first", thinking: "first start", deltas: [] }, { signature: "sig-body-second", thinking: "ignored", deltas: ["second delta"] }] },
  { name: "start-only", blocks: [{ signature: "sig-start-only", deltas: ["start signature body"] }] },
  { name: "empty-start", blocks: [{ signature: "", deltas: ["unsigned body"] }] },
  { name: "delta-only", blocks: [{ signature: "", signatureDelta: "sig-delta", deltas: ["delta body"] }] },
  { name: "delta-wins", blocks: [{ signature: "sig-ignored", signatureDelta: "sig-winner", deltas: ["winner body"] }] },
  { name: "multi-crlf", crlf: true, blocks: [{ signature: "sig-first", deltas: ["first"] }, { signature: "sig-second", signatureDelta: "", deltas: ["second"] }] },
];

async function startThinkingSse(res, fixture) {
  const events = [{ type: "message_start", message: { id: "msg-start", type: "message", role: "assistant", model: "selfcheck", content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 0 } } }];
  fixture.blocks.forEach((block, index) => {
    events.push({ type: "content_block_start", index, content_block: { type: "thinking", thinking: block.thinking ?? "", signature: block.signature } });
    for (const thinking of block.deltas ?? []) events.push({ type: "content_block_delta", index, delta: { type: "thinking_delta", thinking } });
    if (block.signatureDelta !== undefined) events.push({ type: "content_block_delta", index, delta: { type: "signature_delta", signature: block.signatureDelta } });
    events.push({ type: "content_block_stop", index });
  });
  const index = fixture.blocks.length;
  events.push(
    { type: "content_block_start", index, content_block: { type: "tool_use", id: "call-start", name: "ls", input: {} } },
    { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: '{"path":"src"}' } },
    { type: "content_block_stop", index },
    { type: "message_delta", delta: { stop_reason: "tool_use", stop_sequence: null }, usage: { output_tokens: 3 } },
    { type: "message_stop" },
  );
  res.writeHead(200, { "content-type": "text/event-stream" });
  const newline = fixture.crlf ? "\r\n" : "\n";
  const wire = events.map((event) => `event: ${event.type}${newline}data: ${JSON.stringify(event)}${newline}${newline}`).join("");
  // Yield between transport fragments, including split CRLF and JSON boundaries.
  for (let at = 0; at < wire.length; at += 17) {
    res.write(wire.slice(at, at + 17));
    await new Promise((resolve) => setImmediate(resolve));
  }
  res.end();
}

async function handler(req, res) {
  let body = "";
  for await (const part of req) body += part;
  const parsedBody = safeParse(body);
  observed.push({
    url: req.url,
    auth: req.headers.authorization,
    apiKey: req.headers["x-api-key"],
    betas: req.headers["anthropic-beta"],
    chatgptAccountId: req.headers["chatgpt-account-id"],
    originator: req.headers.originator,
    body: parsedBody,
  });
  const model = parsedBody?.model ?? "";
  if (model.startsWith("anthropic-unsigned-")) {
    if (body.includes("mework-unsigned")) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { type: "invalid_request_error", message: "Invalid signature in thinking block" } }));
    } else anthropicSse(res, "end_turn");
    return;
  }
  const startFixture = startThinkingCases.find((fixture) => model === `anthropic-start-${fixture.name}`);
  if (startFixture) {
    if (parsedBody.messages.some((message) => message.content?.some?.((part) => part.type === "tool_result"))) anthropicSse(res, "end_turn");
    else await startThinkingSse(res, startFixture);
    return;
  }
  if (model.startsWith("gpt-5-selfcheck-codex")) {
    if (model === "gpt-5-selfcheck-codex-400") {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ detail: "Stream must be set to true" }));
      return;
    }
    codexSse(res, { failed: model === "gpt-5-selfcheck-codex-failed" });
    return;
  }
  if (model === "empty-reasoning-sentinel") {
    const tool = parsedBody.messages.find((message) => message.role === "assistant" && message.tool_calls?.length);
    if (!tool || !Object.hasOwn(tool, "reasoning_content") || tool.reasoning_content !== "") {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "missing empty reasoning sentinel" } }));
      return;
    }
    sse(res, [chunk({ content: "sentinel-ok" }), chunk({}, "stop")]);
    return;
  }
  if (model === "text") {
    sse(res, [
      chunk({ role: "assistant", content: "" }),
      chunk({ content: "你好" }),
      chunk({ content: "，世界" }),
      chunk({}, "stop", { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 }),
    ]);
    return;
  }

  if (model === "flood") {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(`data: ${JSON.stringify(chunk({ role: "assistant", content: "" }))}\n\n`);
    for (let i = 0; i < 1000; i += 1) {
      res.write(`data: ${JSON.stringify(chunk({ content: `${i};` }))}\n\n`);
    }
    res.write(`data: ${JSON.stringify(chunk({}, "stop", { prompt_tokens: 1, completion_tokens: 1000, total_tokens: 1001 }))}\n\n`);
    res.write("data: [DONE]\n\n");
    res.end();
    return;
  }

  if (model === "tool") {
    sse(res, [
      chunk({ role: "assistant", content: "" }),
      chunk({
        tool_calls: [
          { index: 0, id: "call_selfcheck_1", type: "function", function: { name: "ls", arguments: "" } },
        ],
      }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '{"path":' } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '"src"}' } }] }),
      chunk({}, "tool_calls", { prompt_tokens: 5, completion_tokens: 9, total_tokens: 14 }),
    ]);
    return;
  }

  if (model.startsWith("cache-")) {
    const [, path, shape] = model.split("-");
    const usage = { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110, completion_tokens_details: { reasoning_tokens: 3 } };
    const variants = {
      hit: { prompt_cache_hit_tokens: 80 },
      pair: { prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 20 },
      standard: { prompt_tokens_details: { cached_tokens: 30 }, prompt_cache_hit_tokens: 80 },
      zero: { prompt_tokens_details: { cached_tokens: 0 }, prompt_cache_hit_tokens: 80 },
      null: { prompt_tokens_details: { cached_tokens: null }, prompt_cache_hit_tokens: 80 },
      negative: { prompt_cache_hit_tokens: -1 }, fraction: { prompt_cache_hit_tokens: 1.5 },
      excess: { prompt_cache_hit_tokens: 101 }, mismatch: { prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 30 },
      badmiss: { prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: -1 },
      string: { prompt_cache_hit_tokens: "80" }, infinite: { prompt_cache_hit_tokens: Infinity },
      standardonly: { prompt_tokens_details: { cached_tokens: 30 } }, none: {},
    };
    Object.assign(usage, variants[shape]);
    const cacheJson = (value) => JSON.stringify(value).replace('"prompt_cache_hit_tokens":null', '"prompt_cache_hit_tokens":1e400');
    if (path === "json") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(cacheJson({ id: "cache", model, object: "chat.completion", created: 1,
        choices: [{ index: 0, message: { role: "assistant", content: "cache-ok" }, finish_reason: "stop" }], usage }));
    } else {
      const frames = path === "tail"
        ? [chunk({ content: "cache-ok" }), chunk({}, "stop"), { usage }]
        : [chunk({ content: "cache-ok" }), chunk({}, "stop", usage)];
      res.writeHead(200, { "content-type": "text/event-stream" });
      for (const frame of frames) res.write(`data: ${cacheJson(frame)}\n\n`);
      res.end("data: [DONE]\n\n");
    }
    return;
  }

  if (model.startsWith("identity-")) {
    const a = { id: "A", function: { name: "alpha", arguments: '{"a":' } };
    const b = { index: 0, id: "B", function: { name: "beta", arguments: '{"b":' } };
    const fixtures = {
      "identity-collision": [a, b, { id: "A", function: { arguments: "1}" } }, { id: "B", function: { arguments: "2}" } }],
      "identity-confirm": [a, { index: 0, id: "A", function: { arguments: "1}" } }],
      "identity-separate": [a, { ...b, index: 1 }, { id: "A", function: { arguments: "1}" } }, { id: "B", function: { arguments: "2}" } }],
      "identity-explicit-first": [{ ...a, index: 0 }, { ...b, index: undefined }, { id: "A", function: { arguments: "1}" } }, { id: "B", function: { arguments: "2}" } }],
      "identity-late-id": [{ index: 0, function: a.function }, { index: 0, id: "A", function: { arguments: "1}" } }],
      "identity-missing-id": [{ index: 0, function: a.function }, { index: 0, function: { arguments: "1}" } }],
      "identity-late-two": [{ index: 0, function: a.function }, { index: 1, function: b.function }, { index: 0, id: "A", function: { arguments: "1}" } }, { index: 1, id: "B", function: { arguments: "2}" } }],
      "identity-repeated-name": [{ index: 0, function: a.function }, { index: 0, id: "A", function: { name: "alpha", arguments: "1}" } }],
      "identity-arguments-first": [{ index: 0, function: { arguments: '{"a":' } }, { index: 0, function: { name: "alpha", arguments: "" } }, { index: 0, id: "A", function: { arguments: "1}" } }],
      "identity-anonymous-single": [{ function: a.function }, { function: { arguments: "1}" } }],
      "identity-anonymous-orphan": [{ function: { arguments: '{}' } }],
      "identity-anonymous-many": [{ function: { name: "alpha", arguments: '{"a":1}' } }, { function: { name: "beta", arguments: '{"b":2}' } }, { function: { arguments: ' ' } }],
      "identity-anonymous-interleaved": [{ function: a.function }, { function: b.function }, { function: { arguments: "1}" } }, { function: { arguments: "2}" } }],
      "identity-rebind": [a, { index: 1, id: "A", function: { arguments: "1}" } }],
    };
    sse(res, [...fixtures[model].map((entry) => chunk({ tool_calls: [entry] })), chunk({}, "tool_calls")]);
    return;
  }

  // Some compatible upstreams omit `tool_calls.index`; subsequent fragments must
  // be associated by id rather than assumed to belong to the last call.
  if (model === "tool-no-index") {
    sse(res, [
      chunk({
        tool_calls: [
          { id: "call_a", type: "function", function: { name: "alpha", arguments: '{"a":' } },
        ],
      }),
      chunk({
        tool_calls: [
          { id: "call_b", type: "function", function: { name: "beta", arguments: '{"b":' } },
        ],
      }),
      chunk({ tool_calls: [{ id: "call_a", function: { arguments: "1}" } }] }),
      chunk({ tool_calls: [{ id: "call_b", function: { arguments: "2}" } }] }),
      chunk({}, "tool_calls", { prompt_tokens: 5, completion_tokens: 9, total_tokens: 14 }),
    ]);
    return;
  }

  // Exercise the real Anthropic request path for raw termination and unsigned replay.
  if (model === "anthropic-pause-turn") {
    anthropicSse(res, "pause_turn");
    return;
  }

  if (model === "anthropic-unsigned-replay") {
    anthropicSse(res, "end_turn");
    return;
  }

  if (model === "claude-selfcheck-redacted") {
    res.writeHead(200, { "content-type": "text/event-stream" });
    const events = [
      { type: "message_start", message: { id: "msg_redacted", type: "message", role: "assistant", model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 0 } } },
      { type: "content_block_start", index: 0, content_block: { type: "redacted_thinking", data: "REDACTED_BYTES" } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "thinking", thinking: "", signature: "" } },
      { type: "content_block_delta", index: 1, delta: { type: "thinking_delta", thinking: "readable" } },
      { type: "content_block_delta", index: 1, delta: { type: "signature_delta", signature: "sig-readable" } },
      { type: "content_block_stop", index: 1 },
      { type: "content_block_start", index: 2, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 2, delta: { type: "text_delta", text: "answer" } },
      { type: "content_block_stop", index: 2 },
      { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 2 } },
      { type: "message_stop" },
    ];
    for (const event of events) res.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
    res.end();
    return;
  }
  // Encrypted-only thinking: the plaintext deltas are empty and the whole item
  // arrives as a signature. Relays in front of Claude answer this way.
  const encryptedThinkingShapes = {
    "claude-selfcheck-encrypted-thinking": { signature: "sig-selfcheck" },
    "claude-selfcheck-empty-signature": { signature: "" },
    // DeepSeek's Anthropic-compatible endpoint on every tool continuation.
    "claude-selfcheck-signature-scaffold": { signature: "pseudo-signature", thinkingDelta: false },
    // A relay that strips the plaintext frames outright but still bills thinking.
    "claude-selfcheck-signature-billed": {
      signature: "sig-billed",
      thinkingDelta: false,
      thinkingTokens: 7,
    },
  };
  if (encryptedThinkingShapes[model]) {
    anthropicThinkingSse(res, encryptedThinkingShapes[model]);
    return;
  }

  if (model === "claude-selfcheck-adaptive" || model === "claude-selfcheck-adaptive-bare") {
    anthropicSse(res, "end_turn");
    return;
  }

  if (model === "slow") {
    // Drip-feed deltas for cancellation testing; never finish naturally.
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(`data: ${JSON.stringify(chunk({ role: "assistant", content: "" }))}\n\n`);
    const timer = setInterval(() => {
      if (res.writableEnded) return clearInterval(timer);
      res.write(`data: ${JSON.stringify(chunk({ content: "." }))}\n\n`);
    }, 120);
    res.on("close", () => clearInterval(timer));
    return;
  }

  if (model === "boom-500") {
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: { message: "上游临时故障", type: "server_error" } }));
    return;
  }

  if (model === "boom-400") {
    res.writeHead(400, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: { message: "模型不存在", type: "invalid_request_error" } }));
    return;
  }



  if (model.startsWith("retry-after-")) {
    const hints = { seconds: "60", date: "Wed, 01 Jan 2031 00:00:00 GMT", past: "Wed, 01 Jan 2020 00:00:00 GMT", invalid: "nope" };
    const [hintName, statusText] = model.slice("retry-after-".length).split("-");
    const hint = hints[hintName];
    res.writeHead(Number(statusText), { "content-type": "application/json", ...(hint ? { "Retry-After": hint } : {}) });
    res.end(JSON.stringify({ error: { message: "retry diagnostic", type: "rate_limit_error" } }));
    return;
  }

  if (model.startsWith("echo-secret-")) {
    const status = Number(model.split("-").at(-1));
    const secret = req.headers.authorization ?? req.headers["x-api-key"] ?? "";
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify({ type: "error", error: { type: "authentication_error", message: `diagnostic ${secret} repeated ${secret}` } }));
    return;
  }

  // Anthropic-compatible endpoints may wrap `web_search_tool_result_error` in an
  // array, unlike the official bare-object shape. Normalize it before it reaches
  // the `@ai-sdk/anthropic` schema validator.
  if (model === "anthropic-max-uses") {
    res.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    res.end(readFileSync(resolve(here, "fixtures/anthropic-max-uses-exceeded.sse"), "utf8"));
    return;
  }

  if (model?.startsWith("budget-")) {
    const round = observed.filter((o) => o.body?.model === model).length - 1;
    const sdk = model.includes("sdk");
    const ids = model === "budget-no-search" ? [] : round === 0 ? ["budget_a", "budget_b"] : model === "budget-host" ? ["budget_a", "budget_c"] : ["budget_c"];
    const frames = [{ type: "message_start", message: { id: `msg_budget_${round}`, type: "message", role: "assistant",
      model, content: [], usage: { input_tokens: 1, output_tokens: 0 } } }];
    let index = 0;
    for (const callId of ids) {
      frames.push({ type: "content_block_start", index, content_block: { type: "server_tool_use", id: callId, name: "web_search", input: {} } },
        { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: '{"query":"facts"}' } },
        { type: "content_block_stop", index });
      index++;
      if (!sdk || round > 0) {
        frames.push({ type: "content_block_start", index, content_block: { type: "web_search_tool_result", tool_use_id: callId,
          content: model.includes("failed") ? { type: "web_search_tool_result_error", error_code: "too_many_requests" } : [] } },
          { type: "content_block_stop", index });
        index++;
      }
    }
    if (sdk && round > 0) {
      for (const callId of ["budget_a", "budget_b"]) {
        frames.push({ type: "content_block_start", index, content_block: { type: "web_search_tool_result", tool_use_id: callId, content: [] } },
          { type: "content_block_stop", index });
        index++;
      }
    }
    frames.push({ type: "content_block_start", index, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index, delta: { type: "text_delta", text: `BUDGET_RESULT_${round}` } },
      { type: "content_block_stop", index },
      { type: "message_delta", delta: { stop_reason: "pause_turn", stop_sequence: null }, usage: { output_tokens: 4 } },
      { type: "message_stop" });
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.end(frames.map((frame) => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join(""));
    return;
  }

  if (model?.startsWith("responses-sources-")) {
    const mode = model.slice("responses-sources-".length);
    const url = "https://source.example/article";
    const action = mode === "find" ? { type: "find_in_page", url, pattern: "facts" }
      : mode === "search" || mode === "bounded" ? { type: "search", query: "facts",
          sources: Array.from({ length: mode === "bounded" ? 80 : 2 }, (_, i) => ({ type: "url", url: i === 0 ? url : `${url}/${i}` })) }
      : mode === "missing" ? undefined : { type: "open_page", url: mode === "unsafe" ? "file:///secret" : url };
    const item = { type: "web_search_call", id: "ws_source", status: "completed", action };
    const frames = [
      { type: "response.created", response: { id: "resp_sources", created_at: 1, model } },
      { type: "response.output_item.added", output_index: 0, item },
      { type: "response.output_item.done", output_index: 0, item },
      { type: "response.output_item.added", output_index: 1, item: { type: "message", id: "msg_source" } },
      { type: "response.output_text.delta", output_index: 1, item_id: "msg_source", delta: "ANSWER_WITHOUT_CITATION" },
      ...(mode === "citation" ? [{ type: "response.output_text.annotation.added", output_index: 1,
        item_id: "msg_source", content_index: 0, annotation_index: 0,
        annotation: { type: "url_citation", url, title: "Explicit citation", start_index: 0, end_index: 6 } }] : []),
      { type: "response.output_item.done", output_index: 1, item: { type: "message", id: "msg_source" } },
      { type: "response.completed", response: { usage: { input_tokens: 3, output_tokens: 3 } } },
    ];
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.end(frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join(""));
    return;
  }

  if (model?.startsWith("responses-phase-")) {
    const mode = model.slice("responses-phase-".length);
    const messages = mode === "commentary-only"
      ? [{ text: "PROCESS_ONLY", phase: "commentary" }]
      : mode === "legacy" ? [{ text: "LEGACY_ONLY", phase: null }]
      : [{ text: "PROCESS_ONLY", phase: "commentary" },
          { text: ["empty", "mixed-empty"].includes(mode) ? "" : mode === "whitespace" ? "   " : "ANSWER_ONLY", phase: "final_answer" },
          ...(mode.startsWith("mixed") ? [{ text: "UNPHASED_ONLY", phase: null }] : [])];
    const frames = [{ type: "response.created", response: { id: "resp_phase", created_at: 1, model } }];
    for (const [index, message] of messages.entries()) {
      const item = { type: "message", id: `msg_phase_${index}`, role: "assistant", phase: message.phase };
      frames.push({ type: "response.output_item.added", output_index: index,
        item: mode === "late" ? { ...item, phase: undefined } : item });
      if (message.text) frames.push({ type: "response.output_text.delta", item_id: item.id,
        output_index: index, content_index: 0, delta: message.text });
      frames.push({ type: "response.output_item.done", output_index: index, item });
    }
    frames.push({ type: "response.completed", response: { usage: { input_tokens: 4, output_tokens: 4 } } });
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.end(frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join(""));
    return;
  }

  // Replay real Responses SSE fixtures rather than hand-written approximations.
  // `responses-encrypted-only` contains no reasoning summary delta; the plaintext
  // model name intentionally matches the AI SDK reasoning-model regex.
  if (
    model === "responses-reasoning"
    || model === "responses-encrypted-only"
    || model === "gpt-5-selfcheck-plaintext"
    || model === "proxy-plaintext-replay"
  ) {
    const fixture = model === "responses-encrypted-only"
      ? "fixtures/openai-responses-encrypted-only.sse"
      : "fixtures/openai-responses-reasoning.sse";
    res.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    let replay = readFileSync(resolve(here, fixture), "utf8");
    if (model === "proxy-plaintext-replay" && !(parsedBody?.include ?? []).includes("reasoning.encrypted_content")) {
      replay = replay.replaceAll(',"encrypted_content":"ENCRYPTED_REASONING_SELFCHECK"', "");
    }
    res.end(replay);
    return;
  }

  // Third-party relay dialects. Each shape used to fail an otherwise successful
  // turn; see the "17 中转站" discriminators.
  if (model === "strict-no-stream-options") {
    // Databricks AI Gateway, Mistral, and Azure AI Foundry serverless all reject
    // the extra request field outright rather than ignoring it.
    if (parsedBody && "stream_options" in parsedBody) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: 'json: unknown field "stream_options"' } }));
      return;
    }
    sse(res, [
      chunk({ role: "assistant", content: "" }),
      chunk({ content: "strict-ok" }),
      chunk({}, "stop", { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 }),
    ]);
    return;
  }

  if (model === "json-body") {
    // A relay fronting a batch backend answers `stream:true` with one ordinary
    // Chat Completions object.
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      id: "chatcmpl-json",
      object: "chat.completion",
      created: 1,
      model: "json-body",
      choices: [{ index: 0, message: { role: "assistant", content: "json-ok" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
    }));
    return;
  }

  if (model === "json-body-truncated") {
    // Same shape as `json-body` but with no terminal reason. Converting it would
    // dress a truncated answer up as a clean `stop`, so it must stay an error.
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      id: "chatcmpl-json-truncated",
      object: "chat.completion",
      created: 1,
      model: "json-body-truncated",
      choices: [{ index: 0, message: { role: "assistant", content: "half an ans" } }],
    }));
    return;
  }

  if (model === "usage-only-tail") {
    // `chunkBaseSchema` requires `choices`, so a usage-only terminator turned a
    // finished answer into a validation error.
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(`data: ${JSON.stringify(chunk({ role: "assistant", content: "" }))}\n\n`);
    res.write(`data: ${JSON.stringify(chunk({ content: "tail-ok" }))}\n\n`);
    res.write(`data: ${JSON.stringify(chunk({}, "stop"))}\n\n`);
    res.write(`data: ${JSON.stringify({
      id: "chatcmpl-selfcheck",
      object: "chat.completion.chunk",
      created: 1,
      model: "usage-only-tail",
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    })}\n\n`);
    res.write("data: [DONE]\n\n");
    res.end();
    return;
  }

  if (model === "wide-done") {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(`data: ${JSON.stringify(chunk({ role: "assistant", content: "" }))}\n\n`);
    res.write(`data: ${JSON.stringify(chunk({ content: "wide-ok" }))}\n\n`);
    res.write(`data: ${JSON.stringify(chunk({}, "stop", { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 }))}\n\n`);
    // Two spaces after the colon; still a valid SSE terminator.
    res.write("data:  [DONE]\n\n");
    res.end();
    return;
  }

  // Claude Code's request shape and its 400 self-heal chain, one relay per class.
  // Each class answers 400 exactly while the rejected feature is present, so a
  // healed retry is the only way to reach the stream.
  const reject400 = (message) => {
    res.writeHead(400, { "content-type": "application/json" });
    res.end(JSON.stringify({ type: "error", error: { type: "invalid_request_error", message } }));
  };
  const hasThinkingBlocks = parsedBody?.messages?.some(
    (message) => Array.isArray(message?.content)
      && message.content.some((block) => block?.type === "thinking" || block?.type === "redacted_thinking"),
  );
  if (model === "claude-selfcheck-replay-keep" || model === "claude-selfcheck-replay-gate" || model === "claude-3-selfcheck-nobeta") {
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-signature") {
    if (hasThinkingBlocks) return reject400("messages.1.content.0: Invalid signature in thinking block");
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-type") {
    if (parsedBody?.thinking?.type === "enabled") return reject400("thinking.type: enabled is not supported for this model");
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-effort") {
    if (parsedBody?.output_config?.effort !== undefined) return reject400("This model does not support the effort parameter");
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-overflow") {
    if (parsedBody?.max_tokens > 9000) {
      return reject400("input length and `max_tokens` exceed context limit: 190000 + 32000 > 200000");
    }
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-3-5-selfcheck-ceiling") {
    if (parsedBody?.max_tokens > 8192) {
      return reject400(`max_tokens: ${parsedBody.max_tokens} > 8192, which is the maximum allowed number of output tokens for ${model}`);
    }
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-beta") {
    if ((req.headers["anthropic-beta"] ?? "").includes("interleaved-thinking-2025-05-14")) {
      return reject400("Unsupported beta: interleaved-thinking-2025-05-14");
    }
    anthropicSse(res, "end_turn");
    return;
  }
  if (model === "claude-selfcheck-heal-cache") {
    if (body.includes("cache_control")) return reject400("system.0.cache_control: Extra inputs are not permitted");
    anthropicSse(res, "end_turn");
    return;
  }

  // A stream with every relay oddity Claude Code tolerates: an unknown block type,
  // an unknown delta type, a thinking block missing its text field, a citations
  // delta, an unknown event, and a `message_delta` without usage.
  if (model === "claude-selfcheck-tolerant-stream") {
    res.writeHead(200, { "content-type": "text/event-stream" });
    const events = [
      { type: "message_start", message: { id: "msg-tolerant", type: "message", role: "assistant", model, content: [], usage: {} } },
      { type: "content_block_start", index: 0, content_block: { type: "weird_block", payload: 1 } },
      { type: "content_block_delta", index: 0, delta: { type: "weird_delta", payload: 2 } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "thinking" } },
      { type: "content_block_delta", index: 1, delta: { type: "thinking_delta", thinking: "想" } },
      { type: "content_block_delta", index: 1, delta: { type: "signature_delta", signature: "sig-tolerant" } },
      { type: "content_block_stop", index: 1 },
      { type: "content_block_start", index: 2, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 2, delta: { type: "text_delta", text: "好" } },
      { type: "content_block_delta", index: 2, delta: { type: "citations_delta", citation: { type: "mystery" } } },
      { type: "content_block_stop", index: 2 },
      { type: "some_future_event", detail: {} },
      { type: "message_delta", delta: { stop_reason: "end_turn" } },
      { type: "message_stop" },
    ];
    for (const event of events) res.write(`data: ${JSON.stringify(event)}\n\n`);
    res.end();
    return;
  }

  // A relay fronting a batch backend answers `stream:true` with one Message body.
  if (model === "claude-selfcheck-json-message") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      id: "msg-json",
      type: "message",
      role: "assistant",
      model,
      content: [
        { type: "thinking", thinking: "想想", signature: "sig-json" },
        { type: "text", text: "json-好" },
      ],
      stop_reason: "end_turn",
      stop_sequence: null,
      usage: { input_tokens: 3, output_tokens: 4 },
    }));
    return;
  }

  res.writeHead(404).end();
}

const server = createServer(handler);
/** Second origin: the `stream_options` accommodation is remembered per origin. */
const strictServer = createServer(handler);

function safeParse(text) {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

// ---------------------------------------------------------------- Sidecar driver

function startSidecar() {
  const child = OVERRIDE
    ? spawn(resolve(here, OVERRIDE), [], { stdio: ["pipe", "pipe", "pipe"] })
    : spawn(process.execPath, [BUNDLE], { stdio: ["pipe", "pipe", "pipe"] });
  const frames = [];
  const waiters = [];
  let buffer = "";
  let stderr = "";

  child.stdout.on("data", (data) => {
    buffer += data.toString("utf8");
    for (;;) {
      const index = buffer.indexOf("\n");
      if (index === -1) break;
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      if (!line) continue;
      const frame = JSON.parse(line);
      frames.push(frame);
      for (let i = waiters.length - 1; i >= 0; i -= 1) {
        if (waiters[i].match(frame)) waiters.splice(i, 1)[0].resolve(frame);
      }
    }
  });
  child.stderr.on("data", (data) => {
    stderr += data.toString("utf8");
  });

  return {
    child,
    frames,
    get stderr() {
      return stderr;
    },
    send(frame) {
      child.stdin.write(`${JSON.stringify(frame)}\n`);
    },
    wait(match, timeoutMs = 15000) {
      const existing = frames.find(match);
      if (existing) return Promise.resolve(existing);
      return new Promise((resolve, reject) => {
        const waiter = { match, resolve };
        waiters.push(waiter);
        setTimeout(() => {
          const at = waiters.indexOf(waiter);
          if (at >= 0) waiters.splice(at, 1);
          reject(new Error(`等待帧超时：${match.toString().slice(0, 80)}`));
        }, timeoutMs).unref();
      });
    },
    stop() {
      child.stdin.end();
    },
  };
}

function step(id, baseURL, modelId, extra = {}) {
  return {
    v: V,
    type: "step",
    id,
    payload: {
      family: "openai-compatible",
      baseURL,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId,
      messages: [{ role: "user", content: "hi" }],
      maxSteps: 1,
      ...extra,
    },
  };
}



// ---------------------------------------------------------------- Run

async function main() {
  // Attach both listeners before awaiting either: `once` would otherwise miss a
  // "listening" event that fired while the first await was pending.
  const listening = [once(server, "listening"), once(strictServer, "listening")];
  server.listen(0, "127.0.0.1");
  strictServer.listen(0, "127.0.0.1");
  await Promise.all(listening);
  const baseURL = `http://127.0.0.1:${server.address().port}/v1`;
  // Relays that reject `stream_options` are remembered per request URL, so this
  // case gets its own listener and cannot disable usage reporting for the other
  // discriminators that post to the same path.
  const strictBaseURL = `http://127.0.0.1:${strictServer.address().port}/v1`;
  const jsonBaseURL = baseURL;
  const tailBaseURL = baseURL;
  const doneBaseURL = baseURL;
  const codexBaseURL = `http://127.0.0.1:${server.address().port}/backend-api/codex`;

  // Check 1: handshake and cold-start timing.
  const coldStart = Date.now();
  const sc = startSidecar();
  sc.send({ v: V, type: "hello" });
  const ready = await sc.wait((f) => f.type === "ready");
  const coldMs = Date.now() - coldStart;
  check("1 握手：hello → ready", ready.protocol === V, `协议 ${ready.protocol}，冷启动 ${coldMs} ms`);

  // Start-block fallbacks must survive the real SDK and a subsequent tool round.
  await Promise.all(startThinkingCases.map(async (fixture) => {
    const id = `s-start-${fixture.name}`;
    const model = `anthropic-start-${fixture.name}`;
    const extra = { family: "anthropic", tools: [{ name: "ls", description: "List", inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } }] };
    sc.send(step(id, baseURL, model, extra));
    const done = await sc.wait((f) => f.type === "done" && f.id === id);
    const reasoning = done.result.responseMessages.flatMap((message) => message.content).filter((part) => part.type === "reasoning");
    const expectedText = fixture.blocks.map((block) => block.deltas.length ? block.deltas.join("") : block.thinking ?? "");
    const events = sc.frames.filter((frame) => frame.id === id && frame.type === "event").map((frame) => frame.event);
    check(`reasoning ${fixture.name}: streamed and final body`, events.filter((event) => event.k === "reasoning-delta").map((event) => event.delta).join("") === expectedText.join("") && JSON.stringify(done.result.reasoning) === JSON.stringify(expectedText.filter(Boolean)));
    check(`reasoning ${fixture.name}: card counts`, events.filter((event) => event.k === "reasoning-start").length === expectedText.filter(Boolean).length && events.filter((event) => event.k === "reasoning-done").length === expectedText.filter(Boolean).length);
    check(`reasoning ${fixture.name}: SDK body`, JSON.stringify(reasoning.map((part) => part.text).filter(Boolean)) === JSON.stringify(expectedText.filter(Boolean)));
    const expectedSignatures = fixture.blocks.map((block) => block.signatureDelta || block.signature || undefined);
    check(`reasoning ${fixture.name}: SDK signatures`, JSON.stringify(reasoning.map((part) => part.providerOptions?.anthropic?.signature)) === JSON.stringify(expectedSignatures));
    sc.send(step(`${id}-replay`, baseURL, model, { ...extra, messages: [
      { role: "user", content: "hi" }, ...done.result.responseMessages,
      { role: "tool", content: [{ type: "tool-result", toolCallId: "call-start", toolName: "ls", output: { type: "text", value: "src" } }] },
    ] }));
    await sc.wait((f) => f.type === "done" && f.id === `${id}-replay`);
    const calls = observed.filter((call) => call.body?.model === model);
    const replay = calls[1]?.body.messages.flatMap((message) => message.content).filter((part) => part.type === "thinking") ?? [];
    check(`reasoning ${fixture.name}: one replay request`, calls.length === 2);
    if (expectedSignatures.every(Boolean)) check(`reasoning ${fixture.name}: exact replay`, JSON.stringify(replay.map((part) => part.signature)) === JSON.stringify(expectedSignatures) && JSON.stringify(replay.map((part) => part.thinking)) === JSON.stringify(expectedText));
  }));

  // Check 2: a complete `streamText` call.
  sc.send(step("s-text", baseURL, "text"));
  const doneText = await sc.wait((f) => f.type === "done" && f.id === "s-text");
  const deltas = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-text" && f.event.k === "text-delta")
    .map((f) => f.event.delta);
  check("2 文本流：正文完整", doneText.result.text === "你好，世界", JSON.stringify(doneText.result.text));
  check("2 文本流：delta 拼回正文", deltas.join("") === "你好，世界", deltas.join(""));
  check(
    "2 文本流：用量回传",
    doneText.result.usage.inputTokens === 11 && doneText.result.usage.outputTokens === 7,
    JSON.stringify(doneText.result.usage),
  );
  check(
    "2 文本流：续接块非空",
    Array.isArray(doneText.result.responseMessages) && doneText.result.responseMessages.length > 0,
    `${doneText.result.responseMessages.length} 条`,
  );
  const textCall = observed.find((o) => o.body?.model === "text");
  check("2 文本流：鉴权头真的写出去了", textCall?.auth === "Bearer sk-selfcheck-not-a-real-key", textCall?.auth ?? "(无)");

  for (const family of ["openai-chat", "openai-compatible"]) {
    for (const present of [false, true]) {
      const id = `sentinel-${family}-${present}`;
      sc.send(step(id, baseURL, "empty-reasoning-sentinel", {
        family,
        messages: [
          { role: "user", content: "read" },
          {
            role: "assistant",
            content: [{ type: "tool-call", toolCallId: "call_sentinel", toolName: "read", input: {} }],
            ...(present ? { providerOptions: { openaiCompatible: { reasoning_content: "" } } } : {}),
          },
          { role: "tool", content: [{ type: "tool-result", toolCallId: "call_sentinel", toolName: "read", output: { type: "text", value: "ok" } }] },
        ],
      }));
      const terminal = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
      check(`empty sentinel ${family}: ${present ? "preserved" : "missing rejected"}`,
        present ? terminal.type === "done" && terminal.result.text === "sentinel-ok" : terminal.type === "error");
    }
  }

  // Check 6: tool-call round trip; tools are declared but never executed.
  sc.send(
    step("s-tool", baseURL, "tool", {
      tools: [
        {
          name: "ls",
          description: "列目录",
          inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
        },
      ],
    }),
  );
  const doneTool = await sc.wait((f) => f.type === "done" && f.id === "s-tool");
  check(
    "6 工具往返：调用交回宿主而不是被 SDK 执行",
    doneTool.result.calls.length === 1 &&
      doneTool.result.calls[0].toolName === "ls" &&
      doneTool.result.calls[0].input?.path === "src",
    JSON.stringify(doneTool.result.calls),
  );
  const announced = sc.frames.filter(
    (f) => f.type === "event" && f.id === "s-tool" && f.event.k === "tool-call-announced",
  );
  check("6 工具往返：先报名后给参数", announced.length === 1, `${announced.length} 条 announced`);
  const toolCall = observed.find((o) => o.body?.model === "tool");
  check(
    "6 工具往返：schema 逐字上线",
    toolCall?.body?.tools?.[0]?.function?.parameters?.properties?.path?.type === "string",
    JSON.stringify(toolCall?.body?.tools?.[0]?.function?.parameters ?? null),
  );

  // Check 5: backpressure and ordering across 1,000 deltas.
  sc.send(step("s-flood", baseURL, "flood"));
  const doneFlood = await sc.wait((f) => f.type === "done" && f.id === "s-flood", 30000);
  const floodDeltas = sc.frames.filter(
    (f) => f.type === "event" && f.id === "s-flood" && f.event.k === "text-delta",
  );
  const expected = Array.from({ length: 1000 }, (_, i) => `${i};`).join("");
  check("5 背压：1000 个 delta 一个不丢", doneFlood.result.text === expected, `收到 ${floodDeltas.length} 帧`);
  const seqs = sc.frames.map((f) => f.seq);
  check(
    "5 背压：seq 全局单调",
    seqs.every((s, i) => i === 0 || s > seqs[i - 1]),
    `${seqs.length} 帧`,
  );

  // Check 3: cancellation.
  sc.send(step("s-slow", baseURL, "slow"));
  await sc.wait((f) => f.type === "event" && f.id === "s-slow" && f.event.k === "text-delta");
  sc.send({ v: V, type: "cancel", id: "s-slow" });
  const cancelled = await sc.wait((f) => (f.type === "error" || f.type === "done") && f.id === "s-slow");
  check(
    "3 取消：中途中止并报 cancelled",
    cancelled.type === "error" && cancelled.error.kind === "cancelled",
    JSON.stringify(cancelled.type === "error" ? cancelled.error : cancelled.type),
  );

  // Check error classification; the host retry loop uses this field.
  sc.send(step("s-500", baseURL, "boom-500"));
  const err500 = await sc.wait((f) => f.type === "error" && f.id === "s-500");
  check("+ 分类：5xx 判 transient", err500.error.kind === "transient", JSON.stringify(err500.error));
  sc.send(step("s-400", baseURL, "boom-400"));
  const err400 = await sc.wait((f) => f.type === "error" && f.id === "s-400");
  check("+ 分类：400 判 permanent", err400.error.kind === "permanent", JSON.stringify(err400.error));
  // A midstream-socket test in this harness would be a false discriminator:
  // both correct and broken `cause` traversal get wrapped as a retryable
  // APICallError here. The only test that distinguishes the implementations is
  // api::tests::exhausted_retries_keep_streamed_partial_as_interrupted_contexts
  // (six connections versus one).
  for (const status of [429, 503]) {
  for (const hint of ["seconds", "date", "past", "invalid", "missing"]) {
    const id = `retry-${hint}-step-${status}`;
    const frame = step(id, baseURL, `retry-after-${hint}-${status}`);
    const before = Date.now();
    const callsBefore = observed.length;
    sc.send(frame);
    const result = await sc.wait((f) => f.id === id && f.type === "error");
    const delay = result.error.retryAfterMs;
    const date = Date.parse("Wed, 01 Jan 2031 00:00:00 GMT");
    check(`01 ${id}: Retry-After survives error frame`,
      result.error.kind === "transient" && result.error.status === status && observed.length === callsBefore + 1
        && (hint === "seconds" ? delay === 60000
          : hint === "past" ? delay === 0
          // Windows processes can sample wall clocks on different timer ticks.
          : hint === "date" ? delay >= date - Date.now() - 50 && delay <= date - before + 50
          : delay === undefined), JSON.stringify({ error: result.error, before, after: Date.now() }));
  }
  }

  // Real error exits must redact request-local credentials.
  for (const family of ["openai-chat", "openai-compatible", "anthropic"]) {
    for (const status of [400, 401, 429]) {
      const id = `redact-${family}-step-${status}`;
      const key = `secret-${id}`;
      sc.send(step(id, baseURL, `echo-secret-${status}`, { family, apiKey: key }));
      const result = await sc.wait((f) => f.id === id && (f.type === "error" || f.type === "done"));
      check(`26 ${id}: error credential redaction`,
        (observed.at(-1)?.auth === `Bearer ${key}` || observed.at(-1)?.apiKey === key)
          && result.type === "error" && result.error.status === status
          && result.error.kind === (status === 429 ? "transient" : "permanent")
          && result.error.message.includes("diagnostic") && result.error.message.includes("[redacted]")
          && !JSON.stringify(result).includes(key) && !sc.stderr.includes(key));
    }
  }

  const concurrentRedactions = ["a.b[1]", "other-secret"].map((key, index) => {
    const id = `redact-header-${index}`;
    sc.send(step(id, baseURL, "echo-secret-401", {
      apiKey: "", headers: { Authorization: `Bearer ${key}` },
    }));
    return sc.wait((f) => f.id === id && (f.type === "error" || f.type === "done")).then((result) => {
      check(`26 ${id}: concurrent header credential`, result.type === "error"
        && result.error.message.includes("[redacted]") && !JSON.stringify(result).includes(key)
        && observed.some((o) => o.auth === `Bearer ${key}`) && !sc.stderr.includes(key));
    });
  });
  await Promise.all(concurrentRedactions);

  // Keys and request bodies must never appear in stderr; `APICallError` can carry
  // the complete request in `requestBodyValues`.
  check("+ 净化：Key 不出现在 stderr", !sc.stderr.includes("sk-selfcheck"), sc.stderr.slice(0, 120));
  check(
    "+ 净化：请求体不出现在 stderr",
    !sc.stderr.includes("requestBodyValues") && !sc.stderr.includes("boom-500"),
    sc.stderr.slice(0, 160),
  );

  let rss = 0;
  try {
    rss = process.platform === "win32" ? 0 : 0;
  } catch {}

  // Check 8: exhausting Anthropic native-search `max_uses` must not interrupt the turn.
  sc.send({
    v: V,
    type: "step",
    id: "s-anthropic-dialect",
    payload: {
      family: "anthropic",
      // `createAnthropic` appends `/messages` to baseURL; dispatch uses the body
      // model, so the request path is irrelevant here.
      baseURL: `http://127.0.0.1:${server.address().port}/anthropic`,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId: "anthropic-max-uses",
      messages: [{ role: "user", content: "查五件事" }],
      maxSteps: 1,
      maxOutputTokens: 1024,
      nativeSearch: { maxUses: 1 },
    },
  });
  const dialect = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-dialect",
  );
  check(
    "8 方言：max_uses 用尽不判整轮失败",
    dialect.type === "done",
    dialect.type === "done" ? "done" : JSON.stringify(dialect.error).slice(0, 160),
  );
  check(
    "8 方言：错误块之后的正文照样送达",
    dialect.type === "done" && typeof dialect.result?.text === "string" && dialect.result.text.length > 0,
    dialect.type === "done" ? JSON.stringify(dialect.result.text.slice(0, 60)) : "(无结果)",
  );

  // Check 9: Responses reasoning. Verify request options, reasoning events, and
  // matching event/result duration after a real `streamText` request.
  const responsesBase = `http://127.0.0.1:${server.address().port}/v1`;
  const responsesStep = (id, modelId) => ({
    v: V,
    type: "step",
    id,
    payload: {
      family: "openai-responses",
      baseURL: responsesBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId,
      messages: [{ role: "user", content: "读一下 step.rs" }],
      maxSteps: 1,
      reasoning: "high",
      // Responses and Azure always receive `reasoningContent`; encrypted reasoning
      // is forwarded eagerly so a standalone `reasoning-start` creates a card.
      reasoningContent: "encrypted",
      providerOptions: { openai: { store: false, include: ["reasoning.encrypted_content"] } },
    },
  });

  sc.send(responsesStep("s-reasoning", "responses-reasoning"));
  const reasoningDone = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-reasoning",
  );
  check(
    "9 Responses：思考流跑完",
    reasoningDone.type === "done",
    reasoningDone.type === "done" ? "done" : JSON.stringify(reasoningDone.error).slice(0, 200),
  );

  const reasoningBody = observed.find((o) => o.body?.model === "responses-reasoning")?.body;
  check(
    "9 Responses：请求体带 store:false（上下文由 Mework 自己携带）",
    reasoningBody?.store === false,
    `store=${JSON.stringify(reasoningBody?.store)}`,
  );
  check(
    "9 Responses：请求体主动要 reasoning.encrypted_content",
    Array.isArray(reasoningBody?.include)
      && reasoningBody.include.includes("reasoning.encrypted_content"),
    JSON.stringify(reasoningBody?.include),
  );

  const reasoningEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-reasoning" && f.event.k.startsWith("reasoning-"))
    .map((f) => f.event);
  check(
    "9 Responses：start / delta / done 三条都到了",
    reasoningEvents[0]?.k === "reasoning-start"
      && reasoningEvents.filter((e) => e.k === "reasoning-delta").length === 2
      && reasoningEvents.some((e) => e.k === "reasoning-done"),
    reasoningEvents.map((e) => e.k).join(","),
  );
  const doneEvent = reasoningEvents.find((e) => e.k === "reasoning-done");
  check(
    "9 Responses：耗时进事件、也进 done，两处同一个数",
    typeof doneEvent?.durationMs === "number"
      && reasoningDone.type === "done"
      && reasoningDone.result.reasoningMs === doneEvent.durationMs,
    JSON.stringify({ event: doneEvent?.durationMs, result: reasoningDone.result?.reasoningMs }),
  );
  check(
    "9 Responses：思考 token 回传",
    reasoningDone.type === "done" && reasoningDone.result.usage.reasoningTokens === 900,
    JSON.stringify(reasoningDone.type === "done" ? reasoningDone.result.usage : {}),
  );
  // Preserve encrypted reasoning opaquely in response messages for cross-round replay.
  check(
    "9 Responses：密文进了不透明续接块",
    reasoningDone.type === "done"
      && JSON.stringify(reasoningDone.result.responseMessages).includes("ENCRYPTED_REASONING_SELFCHECK"),
    JSON.stringify(reasoningDone.type === "done" ? reasoningDone.result.responseMessages : []).slice(0, 200),
  );

  // Check 10: encrypted-only reasoning. With no summary delta, visibility depends
  // on the presence of `reasoningMs` and `usage.reasoningTokens`.
  sc.send(responsesStep("s-encrypted", "responses-encrypted-only"));
  const encrypted = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-encrypted",
  );
  check(
    "10 纯密文：一段摘要都没有",
    encrypted.type === "done" && Array.isArray(encrypted.result.reasoning) && encrypted.result.reasoning.length === 1 && encrypted.result.reasoning[0] === "",
    JSON.stringify(encrypted.type === "done" ? encrypted.result.reasoning : encrypted.error).slice(0, 160),
  );
  const encryptedEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-encrypted" && f.event.k.startsWith("reasoning-"))
    .map((f) => f.event.k);
  check(
    "10 纯密文：仍然发出 reasoning-start（否则思考彻底隐形）",
    encryptedEvents.includes("reasoning-start") && !encryptedEvents.includes("reasoning-delta"),
    encryptedEvents.join(",") || "(一条都没有)",
  );
  check(
    "10 纯密文：耗时与思考 token 仍然报出来",
    encrypted.type === "done"
      && typeof encrypted.result.reasoningMs === "number"
      && encrypted.result.usage.reasoningTokens === 512,
    JSON.stringify({
      reasoningMs: encrypted.type === "done" ? encrypted.result.reasoningMs : undefined,
      usage: encrypted.type === "done" ? encrypted.result.usage : undefined,
    }),
  );

  // Check 11: plaintext presentation still requests encrypted replay data.
  // The model name matches the SDK regex; proxy names are covered below.
  sc.send({
    v: V,
    type: "step",
    id: "s-plaintext",
    payload: {
      family: "openai-responses",
      baseURL: responsesBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId: "gpt-5-selfcheck-plaintext",
      messages: [{ role: "user", content: "读一下 step.rs" }],
      maxSteps: 1,
      reasoning: "high",
      reasoningContent: "plaintext",
      providerOptions: { openai: { store: false } },
    },
  });
  const plaintext = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-plaintext",
  );
  check(
    "11 明文思考：请求跑完",
    plaintext.type === "done",
    plaintext.type === "done" ? "done" : JSON.stringify(plaintext.error).slice(0, 200),
  );
  const plaintextBody = observed.find((o) => o.body?.model === "gpt-5-selfcheck-plaintext")?.body;
  check(
    "11 明文思考：请求体包含 reasoning.encrypted_content",
    plaintextBody !== undefined
      && (plaintextBody.include ?? []).includes("reasoning.encrypted_content"),
    JSON.stringify(plaintextBody?.include ?? "(没有 include 字段)"),
  );
  // Removing encrypted reasoning content must not transfer context ownership.
  check(
    "11 明文思考：store 仍然是 false（上下文照旧由 Mework 自己带）",
    plaintextBody?.store === false,
    `store=${JSON.stringify(plaintextBody?.store)}`,
  );
  // The stripping layer must preserve the reasoning effort; `forceReasoning:false`
  // would also remove the automatic include, but changes this field.
  check(
    "11 明文思考：思考档位原样送达",
    plaintextBody?.reasoning?.effort === "high",
    JSON.stringify(plaintextBody?.reasoning ?? null),
  );

  for (const scenario of ["sdk", "sdk-exhausted", "host", "failed", "unlimited", "no-search"]) {
    const modelId = `budget-${scenario}`;
    const makeBudgetStep = (id, maxUses, messages, maxSteps) => ({ v: V, type: "step", id, payload: {
      family: "anthropic", baseURL: `http://127.0.0.1:${server.address().port}/anthropic`, apiKey: "selfcheck",
      modelId, maxSteps, maxOutputTokens: 1024, messages, nativeSearch: { maxUses },
    } });
    const messages = [{ role: "user", content: "Search" }];
    const id = `budget-check-${scenario}`;
    sc.send(makeBudgetStep(id, scenario === "unlimited" ? 0 : scenario === "sdk-exhausted" ? 2 : 3, messages, scenario.startsWith("sdk") ? 3 : 1));
    const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
    const bodies = observed.filter((o) => o.body?.model === modelId).map((o) => o.body);
    const maxUses = (body) => body?.tools?.find((tool) => tool.name === "web_search")?.max_uses;
    check(`native search budget ${scenario}`, done.type === "done"
      && done.result.nativeSearchUses === (scenario === "sdk" ? 3 : scenario === "no-search" ? 0 : 2)
      && (scenario === "unlimited" ? maxUses(bodies[0]) === undefined : maxUses(bodies[0]) === (scenario === "sdk-exhausted" ? 2 : 3))
      && (scenario !== "sdk-exhausted" || bodies.length === 1)
      && (scenario !== "sdk" || (bodies.length === 2 && maxUses(bodies[1]) === 1)),
      JSON.stringify({ done, budgets: bodies.map(maxUses) }));
    if (scenario === "host" && done.type === "done") {
      const nextId = `${id}-next`;
      sc.send(makeBudgetStep(nextId, 1, [...messages, ...done.result.responseMessages], 1));
      const next = await sc.wait((f) => f.id === nextId && ["done", "error"].includes(f.type));
      check("native search budget host continuation", next.type === "done" && next.result.nativeSearchUses === 1
        && maxUses(observed.filter((o) => o.body?.model === modelId)[1]?.body) === 1, JSON.stringify(next));
    }
  }

  {
    const id = "budget-accounted-without-replay";
    sc.send({ v: V, type: "step", id, payload: {
      family: "anthropic", baseURL: `http://127.0.0.1:${server.address().port}/anthropic`, apiKey: "selfcheck",
      modelId: "budget-accounted", maxSteps: 1, maxOutputTokens: 1024,
      messages: [{ role: "user", content: "Search" }],
      nativeSearch: { maxUses: 1, previousCallIds: ["budget_a"] },
    } });
    const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
    check("native search accounted IDs survive missing replay steps", done.type === "done"
      && done.result.nativeSearchUses === 1
      && JSON.stringify(done.result.nativeSearchCallIds) === '["budget_b"]', JSON.stringify(done));
  }

  for (const mode of ["open", "find", "search", "citation", "missing", "unsafe", "bounded"]) {
    const id = `search-sources-${mode}`;
    sc.send({ v: V, type: "step", id, payload: {
      family: "openai-responses", baseURL: responsesBase, apiKey: "selfcheck",
      modelId: `responses-sources-${mode}`, maxSteps: 1,
      messages: [{ role: "user", content: "Search" }], nativeSearch: { maxUses: 3 },
    } });
    const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
    const expected = ["missing", "unsafe"].includes(mode) ? 0 : mode === "bounded" ? 64 : mode === "search" ? 2 : 1;
    const events = sc.frames.filter((f) => f.id === id && f.type === "event" && f.event.k === "source");
    check(`provider search sources ${mode}`, done.type === "done" && done.result.sources.length === expected
      && done.result.calls.length === 0 && events.length === expected
      && events.every((f, i) => f.event.url === done.result.sources[i].url)
      && (expected === 0 || done.result.sources[0].url === "https://source.example/article"), JSON.stringify(done));
  }

  for (const mode of ["normal", "late", "legacy", "commentary-only"]) {
    for (const native of [true, false]) {
      const id = `phase-${mode}-${native}`;
      sc.send({ v: V, type: "step", id, payload: {
        family: "openai-responses", baseURL: responsesBase, apiKey: "selfcheck",
        modelId: `responses-phase-${mode}`, maxSteps: 1,
        messages: [{ role: "user", content: "Search" }],
        ...(native ? { nativeSearch: { maxUses: 3 } } : {}),
      } });
      const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
      const expected = mode === "legacy" ? "LEGACY_ONLY"
        : mode === "commentary-only" ? (native ? "" : "PROCESS_ONLY")
        : (native ? "ANSWER_ONLY" : "PROCESS_ONLYANSWER_ONLY");
      check(`search phase ${mode} native=${native}`, done.type === "done" && done.result.text === expected
        && (mode === "legacy" || JSON.stringify(done.result.responseMessages).includes("PROCESS_ONLY")),
        JSON.stringify(done));
    }
  }

  for (const mode of ["empty", "whitespace", "mixed", "mixed-empty"]) {
    const id = `final-presence-${mode}`;
    sc.send({ v: V, type: "step", id, payload: {
      family: "openai-responses", baseURL: responsesBase, apiKey: "selfcheck",
      modelId: `responses-phase-${mode}`, maxSteps: 1,
      messages: [{ role: "user", content: "Search" }], nativeSearch: { maxUses: 3 },
    } });
    const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
    const expected = mode === "mixed" ? "ANSWER_ONLY" : mode === "whitespace" ? "   " : "";
    check(`search final presence ${mode}`, done.type === "done" && done.result.text === expected,
      JSON.stringify(done));
  }

  let replayMessages = [{ role: "user", content: "Read and continue" }];
  for (let round = 0; round < 2; round++) {
    const id = `plaintext-replay-${round}`;
    const start = observed.length;
    sc.send({ v: V, type: "step", id, payload: {
      family: "openai-responses", baseURL: responsesBase, apiKey: "selfcheck",
      modelId: "proxy-plaintext-replay", maxSteps: 1, reasoning: "high",
      reasoningContent: "plaintext", messages: replayMessages,
      providerOptions: { openai: { store: false, reasoningSummary: null,
        include: ["reasoning.encrypted_content", "web_search_call.results"], promptCacheKey: "replay" } },
    } });
    const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
    const body = observed.slice(start).find((o) => o.body?.model === "proxy-plaintext-replay")?.body;
    check(`plaintext proxy encrypted replay ${round}`, done.type === "done"
      && body?.include.includes("reasoning.encrypted_content")
      && body.include.includes("web_search_call.results") && body.store === false
      && body.prompt_cache_key === "replay"
      && JSON.stringify(done.result.responseMessages).includes("ENCRYPTED_REASONING_SELFCHECK")
      && (round === 0 || JSON.stringify(body.input).includes("ENCRYPTED_REASONING_SELFCHECK")),
      JSON.stringify({ body, done }));
    if (done.type === "done") replayMessages = [...replayMessages, ...done.result.responseMessages,
      { role: "user", content: "Continue" }];
  }

  // Responses defaults mirror the host's options; null suppresses the SDK default.
  for (const family of ["openai-responses", "openai-codex", "azure"]) {
    for (const mode of ["plaintext", "encrypted"]) {
      for (const summary of [null, "detailed"]) {
        const id = `summary-${family}-${mode}-${summary}`;
        const key = family === "azure" ? "azure" : "openai";
        const start = observed.length;
        sc.send({ v: V, type: "step", id, payload: {
          family, baseURL: responsesBase, apiKey: "selfcheck",
          modelId: "gpt-5-selfcheck-plaintext", maxSteps: 1,
          messages: [{ role: "user", content: "Summary default" }],
          reasoning: "high", reasoningContent: mode,
          providerOptions: { [key]: { store: false, reasoningSummary: summary,
            promptCacheKey: id, ...(mode === "encrypted" ? { include: ["reasoning.encrypted_content"] } : {}) } },
        } });
        const done = await sc.wait((f) => f.id === id && ["done", "error"].includes(f.type));
        const body = observed.slice(start).find((o) => o.body?.model === "gpt-5-selfcheck-plaintext")?.body;
        check(`summary default ${id}`, done.type === "done" && body?.reasoning?.effort === "high"
          && (summary === null ? !("summary" in body.reasoning) : body.reasoning.summary === summary)
          && body.store === false && body.prompt_cache_key === id,
          JSON.stringify({ type: done.type, body }));
      }
    }
  }

  // Check 12: the global dispatcher must control Node's built-in fetch. npm undici
  // and Node currently share `Symbol.for("undici.globalDispatcher.1")`; verify both
  // a forced body timeout and a disabled timeout.
  const stallMs = 1200;
  const staller = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "text/plain" });
    response.write("head");
    setTimeout(() => response.end("tail"), stallMs).unref();
  });
  await new Promise((done) => staller.listen(0, "127.0.0.1", done));
  const stallUrl = `http://127.0.0.1:${staller.address().port}/`;
  const previousDispatcher = getGlobalDispatcher();
  const fetchStaller = async (bodyTimeout) => {
    setGlobalDispatcher(new Agent({ bodyTimeout, headersTimeout: 0 }));
    try {
      return { ok: true, text: await (await globalThis.fetch(stallUrl)).text() };
    } catch (error) {
      return { ok: false, code: (error?.cause ?? error)?.code };
    }
  };
  const tight = await fetchStaller(200);
  const disabled = await fetchStaller(0);
  setGlobalDispatcher(previousDispatcher);
  staller.close();
  check(
    "12 传输层：全局 dispatcher 管得住内置 fetch",
    tight.ok === false && tight.code === "UND_ERR_BODY_TIMEOUT",
    `小超时结局 ${JSON.stringify(tight)}`,
  );
  check(
    "12 传输层：关掉 bodyTimeout 后扛得过沉默",
    disabled.ok === true && disabled.text === "headtail",
    `${stallMs} ms 沉默 → ${JSON.stringify(disabled)}`,
  );

  // Check 13: every reasoning stream event must include its item ordinal. Do not
  // reuse prior frames because request identity prevents cross-round masking.
  sc.send(responsesStep("s-reasoning-item", "responses-reasoning"));
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-reasoning-item");
  const reasoningOrdinalEvents = sc.frames
    .filter(
      (f) => f.type === "event"
        && f.id === "s-reasoning-item"
        && ["reasoning-start", "reasoning-delta", "reasoning-done"].includes(f.event.k),
    )
    .map((f) => f.event);
  check(
    "13 思考序号：start / delta / done 全部归属 item 0",
    reasoningOrdinalEvents.length > 0 && reasoningOrdinalEvents.every((event) => event.item === 0),
    JSON.stringify(reasoningOrdinalEvents.map(({ k, item }) => ({ k, item }))),
  );

  const anthropicBase = `http://127.0.0.1:${server.address().port}/anthropic`;

  // Check 14: retain `pause_turn` as the raw termination signal used to continue a turn.
  sc.send({
    v: V,
    type: "step",
    id: "s-anthropic-raw-finish",
    payload: {
      family: "anthropic",
      baseURL: anthropicBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId: "anthropic-pause-turn",
      messages: [{ role: "user", content: "继续" }],
      maxSteps: 1,
      maxOutputTokens: 1024,
    },
  });
  const rawFinish = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-raw-finish",
  );
  check(
    "14 原始终因：pause_turn 归一为 stop 且保留 rawFinishReason",
    rawFinish.type === "done"
      && rawFinish.result.finishReason === "stop"
      && rawFinish.result.rawFinishReason === "pause_turn",
    rawFinish.type === "done"
      ? JSON.stringify({ finishReason: rawFinish.result.finishReason, rawFinishReason: rawFinish.result.rawFinishReason })
      : JSON.stringify(rawFinish.error),
  );

  // Check 15: unsigned foreign history must never be dressed up as signed thinking.
  sc.send({
    v: V,
    type: "step",
    id: "s-anthropic-unsigned-replay",
    payload: {
      family: "anthropic",
      baseURL: anthropicBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId: "anthropic-unsigned-replay",
      messages: [
        { role: "user", content: [{ type: "text", text: "继续" }] },
        {
          role: "assistant",
          content: [
            { type: "reasoning", text: "先想一步" },
            { type: "text", text: "好的" },
          ],
        },
        { role: "user", content: [{ type: "text", text: "然后呢" }] },
      ],
      maxSteps: 1,
      maxOutputTokens: 1024,
    },
  });
  const unsignedReplay = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-unsigned-replay",
  );
  const replayBody = observed.find((o) => o.body?.model === "anthropic-unsigned-replay")?.body;
  const replayAssistantBlocks = replayBody?.messages?.find((message) => message.role === "assistant")?.content;
  check(
    "untagged: first request omits unsigned thinking",
    Array.isArray(replayAssistantBlocks)
      && !replayAssistantBlocks.some((block) => block?.type === "thinking")
      && observed.filter((call) => call.body?.model === "anthropic-unsigned-replay").length === 1,
    JSON.stringify(replayAssistantBlocks),
  );
  check(
    "15 无签名思考：同轮可见正文照样回放",
    Array.isArray(replayAssistantBlocks)
      && replayAssistantBlocks.some((block) => block?.type === "text" && block.text === "好的"),
    JSON.stringify(replayAssistantBlocks),
  );
  check(
    "15 无签名思考：回放请求本身跑完",
    unsignedReplay.type === "done",
    unsignedReplay.type === "done" ? "done" : JSON.stringify(unsignedReplay.error),
  );

  for (const name of ["tagged", "empty", "old-sentinel"]) {
    const model = `anthropic-unsigned-${name}`;
    const content = [{ type: "reasoning", text: "foreign", providerOptions: name === "old-sentinel" ? { anthropic: { signature: "mework-unsigned" } } : { mework: { model }, openai: { reasoningEncryptedContent: "foreign" } } }];
    if (name === "tagged") content.push({ type: "text", text: "visible" });
    sc.send(step(model, anthropicBase, model, { family: "anthropic", messages: [{ role: "user", content: "hi" }, { role: "assistant", content }, { role: "user", content: "continue" }] }));
    const result = await sc.wait((frame) => frame.id === model && (frame.type === "done" || frame.type === "error"));
    const calls = observed.filter((call) => call.body?.model === model);
    const blocks = calls[0]?.body.messages.find((message) => message.role === "assistant")?.content ?? [];
    check(`${name}: clean first request`, result.type === "done" && calls.length === 1 && !blocks.some((block) => block.type === "thinking") && blocks.some((block) => block.type === "text" && block.text === (name === "tagged" ? "visible" : "(no content)")), JSON.stringify(blocks));
  }

  // Check 18: encrypted-only thinking. Relayed Claude endpoints return the whole
  // thinking item as a signature with empty plaintext deltas. The signature is then
  // the only evidence reasoning happened, and dropping it hides thinking entirely.
  const anthropicStep = (id, modelId, extra = {}) => ({
    v: V,
    type: "step",
    id,
    payload: {
      family: "anthropic",
      baseURL: anthropicBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId,
      messages: [{ role: "user", content: "想一想" }],
      maxSteps: 1,
      maxOutputTokens: 2048,
      ...extra,
    },
  });

  sc.send(anthropicStep("s-redacted", "claude-selfcheck-redacted"));
  const redacted = await sc.wait((f) => f.id === "s-redacted" && ["done", "error"].includes(f.type));
  const redactedStarts = sc.frames.filter((f) => f.id === "s-redacted" && f.event?.k === "reasoning-start").map((f) => f.event);
  check("redacted item keeps encrypted form and ordinal before readable signed item",
    redactedStarts[0]?.item === 0 && redactedStarts[0]?.form === "encrypted"
      && redactedStarts[1]?.item === 1 && redactedStarts[1]?.form === "plaintext");
  check("redacted empty item is not compressed out at settlement",
    redacted.type === "done" && JSON.stringify(redacted.result.reasoning) === JSON.stringify(["", "readable"]));
  if (redacted.type === "done") {
    sc.send(anthropicStep("s-redacted-replay", "claude-selfcheck-redacted", {
      messages: [{ role: "user", content: "think" }, ...redacted.result.responseMessages, { role: "user", content: "continue" }],
    }));
    const replayed = await sc.wait((f) => f.id === "s-redacted-replay" && ["done", "error"].includes(f.type));
    const request = observed.filter((o) => o.body?.model === "claude-selfcheck-redacted").at(-1);
    const blocks = request?.body?.messages?.find((message) => message.role === "assistant")?.content;
    check("redacted replay preserves opaque bytes and readable signature",
      replayed.type === "done" && blocks?.[0]?.type === "redacted_thinking" && blocks[0].data === "REDACTED_BYTES"
        && blocks?.[1]?.type === "thinking" && blocks[1].thinking === "readable" && blocks[1].signature === "sig-readable");
  }

  sc.send(anthropicStep("s-anthropic-encThink", "claude-selfcheck-encrypted-thinking", { reasoning: "high" }));
  const encThink = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-encThink",
  );
  const encThinkEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-anthropic-encThink")
    .map((f) => f.event);
  check(
    "18 加密思考：只有签名也算思考过，卡片与耗时都在",
    encThink.type === "done"
      && encThink.result.reasoningMs !== undefined
      && encThinkEvents.some((event) => event.k === "reasoning-start")
      && encThinkEvents.some((event) => event.k === "reasoning-done"),
    encThink.type === "done"
      ? `reasoningMs=${encThink.result.reasoningMs} 事件=${encThinkEvents.map((e) => e.k).join(",")}`
      : JSON.stringify(encThink.error),
  );
  check(
    "18 加密思考：没有明文可显示，摘要保持为空",
    encThink.type === "done"
      && Array.isArray(encThink.result.reasoning)
      && encThink.result.reasoning.length === 1 && encThink.result.reasoning[0] === "",
    encThink.type === "done" ? JSON.stringify(encThink.result.reasoning) : "(错误)",
  );
  check(
    "18 加密思考：密文进了不透明续接块",
    encThink.type === "done"
      && JSON.stringify(encThink.result.responseMessages).includes("sig-selfcheck"),
    encThink.type === "done" ? JSON.stringify(encThink.result.responseMessages).slice(0, 140) : "(错误)",
  );

  // The negative control: an empty signature is upstream scaffolding, not a payload.
  // Without it, "any empty delta counts" would pass check 18 for the wrong reason.
  sc.send(anthropicStep("s-anthropic-empty-sig", "claude-selfcheck-empty-signature", { reasoning: "high" }));
  const emptySignature = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-empty-sig",
  );
  const emptySignatureEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-anthropic-empty-sig")
    .map((f) => f.event.k);
  check(
    "18 加密思考：空签名不算证据，不铸空思考卡",
    emptySignature.type === "done"
      && emptySignature.result.reasoningMs === undefined
      && !emptySignatureEvents.includes("reasoning-start"),
    emptySignature.type === "done"
      ? `reasoningMs=${emptySignature.result.reasoningMs} 事件=${emptySignatureEvents.join(",")}`
      : JSON.stringify(emptySignature.error),
  );

  // The two upstreams that look identical apart from one frame and one counter.
  // DeepSeek opens a bare thinking block on every tool continuation; keeping it card-
  // less is the whole reason the plaintext-frame signal exists.
  sc.send(anthropicStep("s-anthropic-scaffold", "claude-selfcheck-signature-scaffold", { reasoning: "high" }));
  const scaffold = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-scaffold",
  );
  const scaffoldEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-anthropic-scaffold")
    .map((f) => f.event.k);
  check(
    "18 加密思考：只有伪签名、没有明文帧也没有思考 token 的脚手架不出卡",
    scaffold.type === "done"
      && scaffold.result.reasoningMs === undefined
      && !scaffoldEvents.includes("reasoning-start"),
    scaffold.type === "done"
      ? `reasoningMs=${scaffold.result.reasoningMs} 事件=${scaffoldEvents.join(",")}`
      : JSON.stringify(scaffold.error),
  );

  sc.send(anthropicStep("s-anthropic-billed", "claude-selfcheck-signature-billed", { reasoning: "high" }));
  const billed = await sc.wait(
    (f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-billed",
  );
  const billedEvents = sc.frames
    .filter((f) => f.type === "event" && f.id === "s-anthropic-billed")
    .map((f) => f.event.k);
  check(
    "18 加密思考：同样的形态但上游报了思考 token，结算时补出卡",
    billed.type === "done"
      && billed.result.reasoningMs !== undefined
      && billedEvents.includes("reasoning-start")
      && billedEvents.includes("reasoning-done"),
    billed.type === "done"
      ? `reasoningMs=${billed.result.reasoningMs} tokens=${billed.result.usage?.reasoningTokens} 事件=${billedEvents.join(",")}`
      : JSON.stringify(billed.error),
  );

  // Check 19: adaptive thinking is dropped by relays, which answer 200 with no Rewrite it to the explicit-budget form every compatible
  // endpoint implements, without moving the caller's output ceiling.
  sc.send(anthropicStep("s-anthropic-adaptive", "claude-selfcheck-adaptive", { reasoning: "high" }));
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-adaptive");
  const adaptiveBody = observed.find((o) => o.body?.model === "claude-selfcheck-adaptive")?.body;
  check(
    "19 adaptive 回退：中转站收到的是 enabled 加显式预算",
    adaptiveBody?.thinking?.type === "enabled"
      && adaptiveBody.thinking.budget_tokens === Math.round(2048 * 0.6),
    JSON.stringify(adaptiveBody?.thinking),
  );
  check(
    "19 adaptive 回退：预算从 max_tokens 里出，不抬高上限",
    adaptiveBody?.max_tokens === 2048,
    String(adaptiveBody?.max_tokens),
  );

  // No effort means no share to apply. Leaving the body untouched proves the rewrite
  // is driven by the effort the SDK sends, not by the word "adaptive".
  sc.send(
    anthropicStep("s-anthropic-adaptive-bare", "claude-selfcheck-adaptive-bare", {
      providerOptions: { anthropic: { thinking: { type: "adaptive" } } },
    }),
  );
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-anthropic-adaptive-bare");
  const bareAdaptiveBody = observed.find((o) => o.body?.model === "claude-selfcheck-adaptive-bare")?.body;
  check(
    "19 adaptive 回退：没有 effort 的请求原样放行",
    bareAdaptiveBody?.thinking?.type === "adaptive" && bareAdaptiveBody.output_config === undefined,
    JSON.stringify(bareAdaptiveBody?.thinking),
  );

  // Check 20: history replays the provider's own signed reasoning parts, tagged by the
  // host with the producing model. A matching model replays the signature verbatim;
  // a different model drops the block entirely, as Claude Code does, and the tag
  // itself never reaches the wire.
  const signedHistory = (taggedModel) => [
    { role: "user", content: [{ type: "text", text: "继续" }] },
    {
      role: "assistant",
      content: [
        {
          type: "reasoning",
          text: "先想一步",
          providerOptions: { anthropic: { signature: "sig-history" }, mework: { model: taggedModel } },
        },
        { type: "text", text: "好的" },
      ],
    },
    { role: "user", content: [{ type: "text", text: "然后呢" }] },
  ];
  sc.send(
    anthropicStep("s-replay-keep", "claude-selfcheck-replay-keep", {
      system: "你是助手",
      messages: signedHistory("claude-selfcheck-replay-keep"),
      maxOutputTokens: undefined,
    }),
  );
  const replayKeep = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-replay-keep");
  const keepCall = observed.find((o) => o.body?.model === "claude-selfcheck-replay-keep");
  const keepBlocks = keepCall?.body?.messages?.find((message) => message.role === "assistant")?.content;
  check(
    "20 签名回放：同一模型的历史思考原样带签名上线",
    replayKeep.type === "done"
      && Array.isArray(keepBlocks)
      && keepBlocks.some((block) => block?.type === "thinking" && block.thinking === "先想一步" && block.signature === "sig-history"),
    replayKeep.type === "done" ? JSON.stringify(keepBlocks) : JSON.stringify(replayKeep.error),
  );
  check(
    "20 签名回放：宿主的模型标记不进请求体",
    keepCall !== undefined && !JSON.stringify(keepCall.body).includes('"mework"'),
    keepCall === undefined ? "(上游没收到)" : "ok",
  );
  sc.send(
    anthropicStep("s-replay-gate", "claude-selfcheck-replay-gate", {
      messages: signedHistory("claude-other-model"),
    }),
  );
  const replayGate = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-replay-gate");
  const gateBlocks = observed.find((o) => o.body?.model === "claude-selfcheck-replay-gate")?.body?.messages
    ?.find((message) => message.role === "assistant")?.content;
  check(
    "20 签名回放：换了模型就整块丢弃，既不带旧签名也不注哨兵",
    replayGate.type === "done"
      && Array.isArray(gateBlocks)
      && !gateBlocks.some((block) => block?.type === "thinking")
      && gateBlocks.some((block) => block?.type === "text" && block.text === "好的"),
    replayGate.type === "done" ? JSON.stringify(gateBlocks) : JSON.stringify(replayGate.error),
  );

  // Check 21: Claude Code's request shape — its betas, its cache breakpoints, and
  // its default output ceiling.
  check(
    "21 请求形状：interleaved-thinking beta 随请求上线",
    (keepCall?.betas ?? "").includes("interleaved-thinking-2025-05-14"),
    JSON.stringify(keepCall?.betas ?? null),
  );
  sc.send(anthropicStep("s-nobeta", "claude-3-selfcheck-nobeta"));
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-nobeta");
  const noBetaCall = observed.find((o) => o.body?.model === "claude-3-selfcheck-nobeta");
  check(
    "21 请求形状：Claude 3 不发 interleaved-thinking beta",
    noBetaCall !== undefined && !(noBetaCall.betas ?? "").includes("interleaved-thinking"),
    JSON.stringify(noBetaCall?.betas ?? null),
  );
  const keepSystem = keepCall?.body?.system;
  const keepLastMessage = keepCall?.body?.messages?.at(-1);
  const keepLastBlock = Array.isArray(keepLastMessage?.content) ? keepLastMessage.content.at(-1) : undefined;
  check(
    "21 请求形状：system 末块与最后一条消息末块打上缓存断点",
    Array.isArray(keepSystem)
      && keepSystem.at(-1)?.cache_control?.type === "ephemeral"
      && keepLastBlock?.cache_control?.type === "ephemeral",
    JSON.stringify({ system: keepSystem?.at?.(-1), last: keepLastBlock }),
  );
  check(
    "21 请求形状：未设上限时 max_tokens 是 Claude Code 的 32000",
    keepCall?.body?.max_tokens === 32000,
    String(keepCall?.body?.max_tokens),
  );

  // Check 22: the 400 self-heal chain. Every class is exercised against a relay that
  // rejects exactly that feature, and the repair must be remembered so the next
  // request to the same model skips the wasted round trip.
  const calls = (modelId) => observed.filter((o) => o.body?.model === modelId);
  sc.send(
    anthropicStep("s-heal-signature", "claude-selfcheck-heal-signature", {
      messages: signedHistory("claude-selfcheck-heal-signature"),
    }),
  );
  const healSignature = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-signature");
  const signatureCalls = calls("claude-selfcheck-heal-signature");
  const carriesThinking = (call) => call.body.messages.some(
    (m) => Array.isArray(m.content) && m.content.some((b) => b.type === "thinking"),
  );
  check(
    "22 自愈：签名被拒 → 剥掉全部思考块重试一次",
    healSignature.type === "done"
      && signatureCalls.length === 2
      && carriesThinking(signatureCalls[0])
      && !carriesThinking(signatureCalls[1]),
    healSignature.type === "done" ? `${signatureCalls.length} 次请求` : JSON.stringify(healSignature.error),
  );
  sc.send(
    anthropicStep("s-heal-signature-2", "claude-selfcheck-heal-signature", {
      messages: signedHistory("claude-selfcheck-heal-signature"),
    }),
  );
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-signature-2");
  check(
    "22 自愈：剥思考的决定被记住，下一次直接不带",
    calls("claude-selfcheck-heal-signature").length === 3,
    `${calls("claude-selfcheck-heal-signature").length} 次请求`,
  );

  sc.send(anthropicStep("s-heal-type", "claude-selfcheck-heal-type", { reasoning: "high" }));
  const healType = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-type");
  const typeCalls = calls("claude-selfcheck-heal-type");
  check(
    "22 自愈：thinking.type 被拒 → enabled/adaptive 对调重试",
    healType.type === "done"
      && typeCalls.length === 2
      && typeCalls[0].body.thinking?.type === "enabled"
      && typeCalls[1].body.thinking?.type === "adaptive",
    healType.type === "done" ? JSON.stringify(typeCalls.map((c) => c.body.thinking)) : JSON.stringify(healType.error),
  );

  sc.send(anthropicStep("s-heal-effort", "claude-selfcheck-heal-effort", { reasoning: "high" }));
  const healEffort = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-effort");
  const effortCalls = calls("claude-selfcheck-heal-effort");
  check(
    "22 自愈：effort 不被支持 → 去掉 output_config.effort 重试",
    healEffort.type === "done"
      && effortCalls.length === 2
      && effortCalls[0].body.output_config?.effort === "high"
      && effortCalls[1].body.output_config?.effort === undefined,
    healEffort.type === "done" ? JSON.stringify(effortCalls.map((c) => c.body.output_config ?? null)) : JSON.stringify(healEffort.error),
  );

  sc.send(anthropicStep("s-heal-overflow", "claude-selfcheck-heal-overflow", { maxOutputTokens: 32000 }));
  const healOverflow = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-overflow");
  const overflowCalls = calls("claude-selfcheck-heal-overflow");
  check(
    "22 自愈：input + max_tokens 超上下文 → 按服务端给的数字压低 max_tokens",
    healOverflow.type === "done"
      && overflowCalls.length === 2
      && overflowCalls[0].body.max_tokens === 32000
      && overflowCalls[1].body.max_tokens === 9000,
    healOverflow.type === "done" ? JSON.stringify(overflowCalls.map((c) => c.body.max_tokens)) : JSON.stringify(healOverflow.error),
  );

  sc.send(anthropicStep("s-heal-ceiling", "claude-3-5-selfcheck-ceiling", { maxOutputTokens: undefined, reasoning: "high" }));
  const healCeiling = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-ceiling");
  sc.send(anthropicStep("s-heal-ceiling-2", "claude-3-5-selfcheck-ceiling", { maxOutputTokens: undefined, reasoning: "high" }));
  await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-ceiling-2");
  const ceilingCalls = calls("claude-3-5-selfcheck-ceiling");
  check(
    "22 自愈：模型上限被拒 → 按服务端数字压低 max_tokens，预算随之压进上限，且被记住",
    healCeiling.type === "done"
      && ceilingCalls.length === 3
      && ceilingCalls[0].body.max_tokens > 8192
      && ceilingCalls[1].body.max_tokens === 8192
      && ceilingCalls[2].body.max_tokens === 8192
      && ceilingCalls.slice(1).every((c) => (c.body.thinking?.budget_tokens ?? 0) < 8192),
    healCeiling.type === "done"
      ? JSON.stringify(ceilingCalls.map((c) => [c.body.max_tokens, c.body.thinking?.budget_tokens ?? null]))
      : JSON.stringify(healCeiling.error),
  );

  sc.send(anthropicStep("s-heal-beta", "claude-selfcheck-heal-beta"));
  const healBeta = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-beta");
  const betaCalls = calls("claude-selfcheck-heal-beta");
  check(
    "22 自愈：beta 被拒 → 去掉那一个 beta 重试",
    healBeta.type === "done"
      && betaCalls.length === 2
      && (betaCalls[0].betas ?? "").includes("interleaved-thinking")
      && !(betaCalls[1].betas ?? "").includes("interleaved-thinking"),
    healBeta.type === "done" ? JSON.stringify(betaCalls.map((c) => c.betas ?? null)) : JSON.stringify(healBeta.error),
  );

  sc.send(anthropicStep("s-heal-cache", "claude-selfcheck-heal-cache", { system: "你是助手" }));
  const healCache = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-heal-cache");
  const cacheCalls = calls("claude-selfcheck-heal-cache");
  check(
    "22 自愈：cache_control 被拒 → 全部剥掉重试",
    healCache.type === "done"
      && cacheCalls.length === 2
      && JSON.stringify(cacheCalls[0].body).includes("cache_control")
      && !JSON.stringify(cacheCalls[1].body).includes("cache_control"),
    healCache.type === "done" ? `${cacheCalls.length} 次请求` : JSON.stringify(healCache.error),
  );

  // Check 23: the stream is as tolerant as Claude Code's assembler.
  sc.send(anthropicStep("s-tolerant", "claude-selfcheck-tolerant-stream", { reasoning: "high" }));
  const tolerant = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-tolerant");
  check(
    "23 流容错：未知块、未知增量、缺字段的思考块、citations 与未知事件都不拖垮整轮",
    tolerant.type === "done"
      && tolerant.result.text === "好"
      && tolerant.result.reasoning.join("") === "想"
      && JSON.stringify(tolerant.result.responseMessages).includes("sig-tolerant"),
    tolerant.type === "done"
      ? JSON.stringify({ text: tolerant.result.text, reasoning: tolerant.result.reasoning })
      : JSON.stringify(tolerant.error).slice(0, 200),
  );

  sc.send(anthropicStep("s-json-message", "claude-selfcheck-json-message", { reasoning: "high" }));
  const jsonMessage = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-json-message");
  check(
    "23 流容错：stream:true 却回单个 Message JSON 体也能收下，思考与签名不丢",
    jsonMessage.type === "done"
      && jsonMessage.result.text === "json-好"
      && jsonMessage.result.reasoning.join("") === "想想"
      && JSON.stringify(jsonMessage.result.responseMessages).includes("sig-json")
      && jsonMessage.result.usage?.outputTokens === 4,
    jsonMessage.type === "done"
      ? JSON.stringify({ text: jsonMessage.result.text, reasoning: jsonMessage.result.reasoning, usage: jsonMessage.result.usage })
      : JSON.stringify(jsonMessage.error).slice(0, 200),
  );

  // Check 24: Codex replays every stored reasoning item — id, ciphertext, and
  // summary — on later turns. History carries the SDK's own reasoning parts, so
  // the request must contain the rebuilt item and none of the host's tag.
  sc.send({
    v: V,
    type: "step",
    id: "s-responses-replay",
    payload: {
      family: "openai-responses",
      baseURL: responsesBase,
      apiKey: "sk-selfcheck-not-a-real-key",
      modelId: "responses-reasoning",
      messages: [
        { role: "user", content: [{ type: "text", text: "读一下 step.rs" }] },
        {
          role: "assistant",
          content: [
            {
              type: "reasoning",
              text: "先读 step.rs",
              providerOptions: {
                openai: { itemId: "rs_history_1", reasoningEncryptedContent: "ENCRYPTED_HISTORY" },
                mework: { model: "responses-reasoning" },
              },
            },
            { type: "text", text: "读完了" },
          ],
        },
        { role: "user", content: [{ type: "text", text: "然后呢" }] },
      ],
      maxSteps: 1,
      reasoning: "high",
      reasoningContent: "encrypted",
      providerOptions: { openai: { store: false, include: ["reasoning.encrypted_content"], promptCacheKey: "conv-selfcheck" } },
    },
  });
  const responsesReplay = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-responses-replay");
  const responsesReplayCall = observed.filter((o) => o.body?.model === "responses-reasoning").at(-1);
  const replayedItem = responsesReplayCall?.body?.input?.find((item) => item?.type === "reasoning");
  check(
    "24 Responses 回放：历史 reasoning item 带 id、密文与摘要重建上线",
    responsesReplay.type === "done"
      && replayedItem?.id === "rs_history_1"
      && replayedItem?.encrypted_content === "ENCRYPTED_HISTORY"
      && replayedItem?.summary?.[0]?.text === "先读 step.rs",
    responsesReplay.type === "done" ? JSON.stringify(replayedItem ?? responsesReplayCall?.body?.input) : JSON.stringify(responsesReplay.error),
  );
  check(
    "24 Responses 回放：prompt_cache_key 按会话上线（Codex 按线程键缓存）",
    responsesReplayCall?.body?.prompt_cache_key === "conv-selfcheck",
    JSON.stringify(responsesReplayCall?.body?.prompt_cache_key ?? null),
  );
  check(
    "24 Responses 回放：宿主的模型标记不进请求体",
    responsesReplayCall !== undefined && !JSON.stringify(responsesReplayCall.body).includes('"mework"'),
    responsesReplayCall === undefined ? "(上游没收到)" : "ok",
  );

  // Check 25: ChatGPT-subscription Codex only accepts a narrowly constrained
  // Responses dialect, including its content-type-less SSE transport.
  sc.send(
    step("s-codex", codexBaseURL, "gpt-5-selfcheck-codex", {
      family: "openai-codex",
      maxOutputTokens: 64,
      providerOptions: { openai: { store: false } },
      headers: { "chatgpt-account-id": "acct_selfcheck", originator: "mework" },
    }),
  );
  const codexDone = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-codex");
  const codexCall = observed.find((o) => o.body?.model === "gpt-5-selfcheck-codex");
  check(
    "25 codex：无 content-type 的 SSE 仍完成并送达正文",
    codexDone.type === "done" && codexDone.result.text === "codex-ok",
    codexDone.type === "done" ? codexDone.result.text : JSON.stringify(codexDone.error),
  );
  check(
    "25 codex：请求体被矫正为 stream:true/store:false 且无 max_output_tokens",
    codexCall?.url === "/backend-api/codex/responses"
      && codexCall.body?.stream === true
      && codexCall.body?.store === false
      && !("max_output_tokens" in (codexCall.body ?? {}))
      && Array.isArray(codexCall.body?.include)
      && codexCall.body.include.includes("reasoning.encrypted_content"),
    JSON.stringify({ url: codexCall?.url, body: codexCall?.body }),
  );
  check(
    "25 codex：OAuth 与 ChatGPT 宿主头逐字送达",
    codexCall?.auth === "Bearer sk-selfcheck-not-a-real-key"
      && codexCall.chatgptAccountId === "acct_selfcheck"
      && codexCall.originator === "mework",
    JSON.stringify({ auth: codexCall?.auth, account: codexCall?.chatgptAccountId, originator: codexCall?.originator }),
  );

  sc.send(step("s-codex-400", codexBaseURL, "gpt-5-selfcheck-codex-400", { family: "openai-codex" }));
  const codex400 = await sc.wait((f) => (f.type === "done" || f.type === "error") && f.id === "s-codex-400");
  check(
    "25 codex：上游 400 原样通过并判 permanent",
    codex400.type === "error" && codex400.error.kind === "permanent",
    JSON.stringify(codex400.type === "error" ? codex400.error : codex400),
  );

  // Check 16: interleaved tool fragments without indexes must continue by id.
  sc.send(
    step("s-tool-no-index", baseURL, "tool-no-index", {
      tools: [
        {
          name: "alpha",
          description: "alpha",
          inputSchema: { type: "object", properties: { a: { type: "number" } }, required: ["a"] },
        },
        {
          name: "beta",
          description: "beta",
          inputSchema: { type: "object", properties: { b: { type: "number" } }, required: ["b"] },
        },
      ],
    }),
  );
  const noIndexTools = await sc.wait((f) => f.type === "done" && f.id === "s-tool-no-index");
  const alpha = noIndexTools.result.calls.find((call) => call.toolName === "alpha");
  const beta = noIndexTools.result.calls.find((call) => call.toolName === "beta");
  check(
    "16 工具序号：漏 index 的交错参数按 id 各自拼回",
    noIndexTools.result.calls.length === 2 && alpha?.input?.a === 1 && beta?.input?.b === 2,
    JSON.stringify(noIndexTools.result.calls),
  );

  for (const family of ["openai-chat", "openai-compatible"]) {
    for (const path of ["sse", "tail", "json"]) {
      for (const [shape, cached] of Object.entries({ hit: 80, pair: 80, standard: 30, zero: 0, null: 80,
        negative: 0, fraction: 0, excess: 0, mismatch: 0, badmiss: 0, string: 0, infinite: 0, standardonly: 30, none: 0 })) {
        const id = `s-cache-${family}-${path}-${shape}`;
        sc.send(step(id, baseURL, `cache-${path}-${shape}`, { family }));
        const result = await sc.wait((f) => f.id === id && (f.type === "done" || f.type === "error"));
        const usage = result.result?.usage;
        check(`cache ${family} ${path} ${shape}`, result.type === "done" && usage?.cacheReadTokens === cached
          && usage.inputTokens === 100 && usage.outputTokens === 10 && usage.totalTokens === 110
          && usage.reasoningTokens === 3, JSON.stringify(result));
      }
    }
    for (const [shape, error, count] of [
      ["collision", "index conflict", 0], ["rebind", "index conflict", 0],
      ["late-id", null, 1], ["missing-id", null, 1], ["late-two", null, 2],
      ["repeated-name", null, 1], ["arguments-first", null, 1],
      ["anonymous-single", null, 1], ["anonymous-orphan", "identity ambiguity", 0],
      ["anonymous-many", "identity ambiguity", 0], ["anonymous-interleaved", "identity ambiguity", 0],
      ["confirm", null, 1], ["separate", null, 2], ["explicit-first", null, 2],
    ]) {
      const id = `s-identity-${family}-${shape}`;
      sc.send(step(id, baseURL, `identity-${shape}`, {
        family,
        tools: ["alpha", "beta"].map((name) => ({ name, description: name, inputSchema: { type: "object", additionalProperties: true } })),
      }));
      const result = await sc.wait((f) => f.id === id && (f.type === "done" || f.type === "error"));
      if (!error && result.type === "done") {
        const announcements = sc.frames.filter((f) => f.id === id && f.type === "event" && f.event.k === "tool-call-announced");
        check(`identity announcements ${family} ${shape}`, announcements.length === count
          && announcements.every((frame, index) => frame.event.callId === result.result.calls[index].callId), JSON.stringify(announcements));
      }
      check(`identity ${family} ${shape}`, error
        ? result.type === "error" && result.error.message.includes(error)
        : result.type === "done" && result.result.calls.length === count
          && result.result.calls[0]?.input?.a === 1
          && (count === 1 || result.result.calls[1]?.input?.b === 2), JSON.stringify(result));
    }
  }

  // Check 17: four third-party relay behaviours that each used to fail the turn.
  // Every one is served from its own origin, because the `stream_options`
  // accommodation is remembered per origin.
  sc.send(step("s-relay-strict", strictBaseURL, "strict-no-stream-options"));
  const strict = await sc.wait((f) => f.id === "s-relay-strict" && (f.type === "done" || f.type === "error"));
  const strictCalls = observed.filter((call) => call.body?.model === "strict-no-stream-options");
  check(
    "17 中转站：拒收 stream_options 时去掉它重试一次",
    strict.type === "done" && strict.result.text === "strict-ok",
    strict.type === "done" ? strict.result.text : JSON.stringify(strict.error),
  );
  check(
    "17 中转站：重试正好一次，且第二次真的不带 stream_options",
    strictCalls.length === 2
      && strictCalls[0].body?.stream_options !== undefined
      && strictCalls[1].body?.stream_options === undefined,
    JSON.stringify(strictCalls.map((call) => call.body?.stream_options ?? null)),
  );

  sc.send(step("s-relay-json", jsonBaseURL, "json-body"));
  const jsonBody = await sc.wait((f) => f.id === "s-relay-json" && (f.type === "done" || f.type === "error"));
  check(
    "17 中转站：stream:true 却回单个 JSON 体也能收下",
    jsonBody.type === "done" && jsonBody.result.text === "json-ok"
      && jsonBody.result.usage?.totalTokens === 5,
    jsonBody.type === "done"
      ? `${jsonBody.result.text} ${JSON.stringify(jsonBody.result.usage)}`
      : JSON.stringify(jsonBody.error),
  );

  sc.send(step("s-relay-truncated", jsonBaseURL, "json-body-truncated"));
  const truncated = await sc.wait((f) => f.id === "s-relay-truncated" && (f.type === "done" || f.type === "error"));
  check(
    "17 中转站：没有终止原因的 JSON 体不冒充成功收尾",
    truncated.type === "error",
    truncated.type === "done"
      ? `被当成了成功：${JSON.stringify(truncated.result.text)}`
      : JSON.stringify(truncated.error.message).slice(0, 90),
  );

  sc.send(step("s-relay-tail", tailBaseURL, "usage-only-tail"));  const usageTail = await sc.wait((f) => f.id === "s-relay-tail" && (f.type === "done" || f.type === "error"));
  check(
    "17 中转站：只带 usage 没有 choices 的收尾块不判失败",
    usageTail.type === "done" && usageTail.result.text === "tail-ok"
      && usageTail.result.usage?.totalTokens === 2,
    usageTail.type === "done"
      ? `${usageTail.result.text} ${JSON.stringify(usageTail.result.usage)}`
      : JSON.stringify(usageTail.error),
  );

  sc.send(step("s-relay-done", doneBaseURL, "wide-done"));
  const wideDone = await sc.wait((f) => f.id === "s-relay-done" && (f.type === "done" || f.type === "error"));
  check(
    "17 中转站：[DONE] 多一个空格不把整轮拖垮",
    wideDone.type === "done" && wideDone.result.text === "wide-ok",
    wideDone.type === "done" ? wideDone.result.text : JSON.stringify(wideDone.error),
  );

  // Check 30: the claude-agent family against the locally installed Claude Code
  // executable and a scripted Anthropic upstream. Skipped, loudly, without one.
  const claudeExecutable = resolveClaudeExecutable();
  if (claudeExecutable) {
    await runClaudeAgentChecks({ sc, check, V, executable: claudeExecutable });
    await runClaudeAgentShutdownCheck({ startSidecar, check, V, executable: claudeExecutable });
  } else {
    process.stdout.write(
      "警告：未找到原生 Claude Code 可执行文件（~/.local/bin/claude 或 MEWORK_CLAUDE_EXECUTABLE），跳过 claude-agent 判别器\n",
    );
  }

  // Check 4: a killed process must actually exit so the host can settle it as interrupted.
  sc.send(step("s-kill", baseURL, "slow"));
  await sc.wait((f) => f.type === "event" && f.id === "s-kill" && f.event.k === "text-delta");
  const exited = once(sc.child, "exit");
  sc.child.kill();
  const [code, signal] = await Promise.race([
    exited,
    new Promise((_, reject) => setTimeout(() => reject(new Error("kill 之后 5 秒未退出")), 5000).unref()),
  ]);
  check("4 被 kill：进程立即消失", sc.child.killed === true, `code=${code} signal=${signal}`);

  // Clean exit: a fresh sidecar exits when stdin closes.
  const sc2 = startSidecar();
  sc2.send({ v: V, type: "hello" });
  await sc2.wait((f) => f.type === "ready");
  const exited2 = once(sc2.child, "exit");
  sc2.stop();
  const [code2] = await Promise.race([
    exited2,
    new Promise((_, reject) => setTimeout(() => reject(new Error("stdin 关闭后 5 秒未退出")), 5000).unref()),
  ]);
  check("4 干净退出：stdin 关闭即退出", code2 === 0, `code=${code2}`);

  server.close();
  strictServer.close();
  process.stdout.write(`\n冷启动 ${coldMs} ms · ${results.length} 条判别器 · ${failures} 条失败\n`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((error) => {
  process.stderr.write(`selfcheck 崩了：${error?.stack ?? error}\n`);
  process.exit(1);
});
