# mework-aisdk Sidecar

Mework's model-protocol executor. The host is Rust, this service is Node, and they communicate through NDJSON over stdio.

**It performs exactly one task: one model step.** Tools are declared without `execute`, so the AI SDK stops at a tool call. Rust retains tool execution, approvals, hooks, persistence, cancellation, and the decision to begin the next turn. This boundary is intentional: Mework's tool loop is coupled to the agent-kernel formal specification, subagent pool, and task abstraction.

## Status: wired

Every host protocol entry point targets this service: the turn loop in `api.rs`, the one-shot native-search request, and historical projection.

`npm run selfcheck` runs offline checks against one local fake upstream and one real sidecar child process. No API key is required; the `claude-agent` section additionally needs a locally installed native Claude Code and is skipped without one.

| 判别器 | 结论 |
| --- | --- |
| 1 握手 | `hello` → `ready`，协议世代逐字比对 |
| 2 文本流 | delta 拼回正文、用量回传、续接块非空、鉴权头真的写出去了 |
| 3 取消 | 流中途 `cancel` → `AbortController` → `kind: "cancelled"` |
| 4 进程边界 | 被 kill 立即消失；stdin 关闭即 `exit 0` |
| 5 背压 | 1000 个 delta 一个不丢、`seq` 全局单调 |
| 6 工具往返 | 调用交回宿主而不是被 SDK 执行；schema 逐字上线 |
| 8 方言 | 兼容端点的 `max_uses_exceeded` 不打断整轮（见「兼容端点的方言」） |
| + 失败分类 | 5xx → `transient`，400 → `permanent` |
| + 净化 | Key 与请求体都不出现在 stderr |

### Live validation: DeepSeek official API

Offline discriminators cannot establish whether an upstream behaves this way in production. Real-key validation covers three layers:

| 层 | 结论 |
| --- | --- |
| 直接驱动侧车 | chat / responses / anthropic 三协议均通；195 个 delta 跨 2.7 s 证明是真流式；`reasoning-delta` 到齐；取消真的落在 `cancelled`；Key 不进 stderr |
| 宿主 `deepseek_live::*` | 6/6 绿（三条工具循环 + Responses/Anthropic 原生搜索 + Chat 的可修复拒绝） |
| 真机应用（`dev:browser -- --claude`） | 拉取模型、文本回合、`ls` 工具往返全部走通 |

## Packaging: `externalBin`

`tauri.conf.json` declares `externalBin: ["binaries/mework-aisdk"]`. Tauri resolves this to `src-tauri/binaries/mework-aisdk-<target-triple><exe>`, then removes the target triple during installation and places the binary beside the main executable. That is where the production branch of `resolve_binary()` looks.

Responsibilities are deliberately separated:

- `npm run build:sidecar` **only builds** `aisdk-service/dist/mework-aisdk.exe`.
- `src-tauri/build.rs` **only stages** it. The target triple comes from Cargo's authoritative `TARGET`, without reparsing `rustc -vV`, and is recopied when the source artifact changes.

`beforeBuildCommand` runs both. Development requires no additional action: `cargo run` / `dev:browser` causes Tauri to copy the same executable beside `target/debug/`, where the production branch of `resolve_binary()` finds it. If neither artifact exists, `build.rs` stops with an error explaining how to produce it instead of leaving `externalBin` to fail obscurely at the end of packaging.

### Key measurements (Windows x64, Node 24.19.0)

| 量 | 值 |
| --- | --- |
| ESM bundle（开发） | 1.84 MiB |
| 单文件可执行（发布） | **90.5 MiB** 未压缩 |
| ↑ xz -9（≈ NSIS 的 LZMA 增量） | **22.3 MiB** |
| 冷启动 `hello`→`ready` | **~130 ms**（稳态；刚写出来的新二进制首次执行会因
  Defender 首扫到 1–2 s） |
| 空闲 RSS | ~96 MiB |
| 1000 delta 之后 RSS | ~149 MiB |

The 96 MiB idle footprint is V8's baseline. The design uses **one multiplexed, lazily started process**: parent turns and all subagents share the sidecar, demultiplexed by request ID. One process per request would consume nearly 800 MiB for eight concurrent subagents. It starts on the first `run_step`, `shutdown_sidecar()` explicitly stops it at application exit, and a Windows Job Object is only a fallback.

## Protocol

Each line is one JSON value terminated by `\n`. Three rules apply:

1. **stdout carries only protocol data.** All logs go to stderr. At startup, `main.ts` pins the entire `console.*` family to stderr because any dependency's `console.log` could insert partial text into the protocol stream.
2. **Both sides enforce a line limit** of 16 MiB. Neither the host nor the sidecar trusts the other.
3. **`seq` is process-global and monotonic**, not a per-request counter. Losing all frames for an entire request must still be observable.

### Host → Sidecar

```jsonc
{"v":9,"type":"hello"}
{"v":9,"type":"step","id":"r1","payload":{ /* StepRequest */ }}
{"v":9,"type":"cancel","id":"r1"}
{"v":9,"type":"release","session":"run-…"}          // claude-agent only: the host run ended; no reply
{"v":9,"type":"shutdown"}
```

### Sidecar → Host

```jsonc
{"v":9,"seq":0,"type":"ready","protocol":9,"versions":{"node":"24.19.0"}}
{"v":9,"seq":1,"type":"event","id":"r1","event":{"k":"text-delta","delta":"你好"}}
{"v":9,"seq":2,"type":"done","id":"r1","result":{ /* StepResult */ }}
{"v":9,"seq":3,"type":"error","id":"r1","error":{"kind":"transient","message":"…"}}
```

Event `k` values intentionally correspond one-to-one with Rust's `ModelStreamEvent`; the host only forwards them:
`text-delta` / `reasoning-start` / `reasoning-delta` / `reasoning-done` /
`tool-call-announced` / `tool-call` / `usage` / `source` / `heartbeat`.

`reasoning-start` is **not** a prefix of `reasoning-delta`; it is an independent fact. Responses emits it at `response.output_item.added`, even when the summary emits no text because only `reasoning.encrypted_content` was requested or the provider supplies no summary. Without it, reasoning that occurred and incurred cost is absent from both host and UI.

`reasoning-done.durationMs` and `done.result.reasoningMs` are **the same value**: the sum of start-to-end durations for every reasoning item in the step. Timing remains in the sidecar because it alone observes the actual stream ordering. A shared source ensures that the live display and persisted value agree after reload. The **presence** of `reasoningMs`, rather than a value greater than zero, indicates that the step reasoned; sub-millisecond reasoning still counts.

`heartbeat` fires every 500 ms and carries the host cancellation probe. **It is a cancellation-probe carrier, not evidence that the upstream is live.**

### The transport layer does not infer failure from silence

At startup, the sidecar installs a global undici dispatcher with `headersTimeout` and `bodyTimeout` set to `0`, undici's documented disabled value, instead of their 300-second defaults.

This is necessary because protocols that return only encrypted reasoning fields, such as Responses with `store` disabled and only `reasoning.encrypted_content` requested, emit no frames during reasoning except the opening `reasoning-start`. A healthy long reasoning period and a stalled upstream are indistinguishable by elapsed content silence. Incorrectly aborting loses the entire turn, including already-generated, billable reasoning. Real request failures provide explicit signals: a closed connection, an error frame, or an HTTP status code. **Connection timeout remains at its default** because it detects failure to connect, not silence after connection.

The dispatcher is global rather than a per-provider `fetch`: npm undici and Node's bundled copy share the slot at `Symbol.for("undici.globalDispatcher.1")`. One setting therefore covers AI SDK requests, the native-search leg, and token requests from the google-vertex credential chain. This shared symbol is an **undocumented implementation detail**, so selfcheck discriminator 12 verifies both directions: it first confirms that an intentionally small `bodyTimeout` controls built-in `fetch`, then confirms that `0` survives silence. If a Node upgrade breaks this behavior, the likely symptom is an upstream connection ending at five minutes.

`done.result.responseMessages` is an **opaque continuation block** from AI SDK `response.messages`. Anthropic's `encryptedContent` and reasoning signature, and Responses reasoning items, survive across turns through it. The host persists and replays it unchanged.

`error.kind` is the **only** field recognized by the host retry loop. Classification remains in the sidecar because only it observes AI SDK error types. It uses duck typing rather than `instanceof APICallError`: a second `@ai-sdk/provider` copy in `node_modules` silently makes `instanceof` false, and a retryable failure classified as permanent is indistinguishable in the UI from an upstream outage.

## Adapter families

`src/providers.ts` is the **only** location that branches by family. It maps exhaustively to Rust's `ProviderFamily`.

| family | provider | 原生 web_search |
| --- | --- | --- |
| `openai-responses` | `createOpenAI().responses()` | `tools.webSearch()` |
| `openai-codex` | `createOpenAI().responses()` + Codex dialect | `tools.webSearch()` |
| `openai-chat` | `createOpenAICompatible().chatModel()` | 无（可修复的拒绝） |
| `anthropic` | `createAnthropic()` | `tools.webSearch_20250305({maxUses})` |
| `google` | `createGoogle()` | `tools.googleSearch()` |
| `xai` | `createXai()` | `tools.webSearch()` |
| `azure` | `createAzure()` | `tools.webSearchPreview()` |
| `bedrock` | `createAmazonBedrock()` | `tools.webSearch_20250305()` |
| `vertex` | `createGoogleVertex()` | `tools.googleSearch()` |
| `openai-compatible` | `createOpenAICompatible().chatModel()` | 无（可修复的拒绝） |
| `claude-agent` | not AI SDK: `claude-agent.ts` drives the local Claude Code executable through `@anthropic-ai/claude-agent-sdk` | 无（宿主自己检索） |

**Responses-compatible endpoints use `createOpenAI`, not openai-compatible.** This preserves DeepSeek native search: any upstream that implements Responses receives `openai.tools.webSearch()`, whereas the generic openai-compatible provider discards provider tools together with its `unsupported` warning.

- **`openai-codex`** targets ChatGPT-subscription Codex. Its dialect forces the backend's required `stream:true`, `store:false`, and `reasoning.encrypted_content` include, removes rejected `max_output_tokens`, supplies the absent SSE content type, and converts a completed SSE response back to JSON for a non-streaming call. OAuth bearer and ChatGPT account/origin headers are supplied unchanged by the host; the sidecar adds no authentication.

### `claude-agent`: Claude Code as a model backend

`src/claude-agent.ts` is the one family that bypasses `resolveModel()` and `streamText()`. The host's `StepRequest` carries an extra `agent: { session, executable, cwd, env }` block; the sidecar spawns the **user's own native Claude Code executable** (never redistributed) through the official Agent SDK and keeps one CLI process per host run, keyed by `agent.session`.

What the CLI is allowed to do is deliberately nothing of its own: `tools: []`, `settingSources: []`, `strictMcpConfig: true`, no plugins/agents, and an environment that switches off compaction, attachments, auto-memory, CLAUDE.md, background tasks, telemetry and updates. The only tools the model sees are the host's, published by one hand-written in-process MCP server (`mework`) so the host schemas reach the model verbatim. `CLAUDE_AGENT_SDK_MCP_NO_PREFIX=1` makes the CLI register them under their bare host names, so nothing in the tool loop ever sees an `mcp__mework__` prefix (it is still stripped defensively). The request the CLI sends therefore has `system = [billing header, "You are a Claude agent, built on Anthropic's Claude Agent SDK.", host prompt]` and `tools = the host's tools`; the SDK's identity line and billing header are the SDK's own and are not touched.

The round boundary is unchanged. When the model calls a tool the MCP handler **parks**, the step ends with `done{calls}`, and the next step for the same session — recognised because its trailing `tool` message answers exactly the parked ids — resolves the handlers and streams the following reply. Carrier user messages behind the results (the image bridge, `[SYSTEM NOTIFICATION - NOT USER INPUT]` notices) are folded into the last tool result rather than sent as user turns, because the CLI would otherwise wrap them as "the user sent a new message while you were working". A new run starts a new CLI session; host history is rewritten into a Claude Code transcript and injected through `resume` + `sessionStore.load()` (the SDK materialises it in a temporary `CLAUDE_CONFIG_DIR` and copies the CLI's own credentials there itself — this module never reads them). Fresh sessions without history run with `persistSession:false`, so nothing of Mework's lands in `~/.claude/projects`.

Credentials: there are none. This family authenticates with the user's own Claude Code login and nothing else, so `apiKey` and `baseURL` on the request are ignored, and the ambient `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL` / `ANTHROPIC_CUSTOM_HEADERS` / `CLAUDE_CODE_OAUTH_TOKEN` of the sidecar process are deleted before the CLI environment is composed — the Agent SDK only falls back to the stored login when no credential is forced through the environment. `CLAUDE_CODE_USE_BEDROCK` / `_VERTEX` / `_FOUNDRY` are pinned to `0`, and a `claude-`prefixed model id is pinned into `ANTHROPIC_MODEL` and the three `ANTHROPIC_DEFAULT_*_MODEL` variables (an alias such as `sonnet` or `opus[1m]` is left for the CLI to resolve). `CLAUDE_CONFIG_DIR` is never set here, because that is where the login lives and macOS Keychain lookup only happens while it is absent. The single upstream override is a **local test stub**: `agent.env` may carry an `ANTHROPIC_BASE_URL` pointing at a loopback host — a remote one fails the request as `permanent` — and only then is an `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` from `agent.env` passed through. `reasoning` maps to the CLI's `--effort` (`none` disables thinking), `maxOutputTokens` to `CLAUDE_CODE_MAX_OUTPUT_TOKENS`; `temperature`, `toolChoice`, `providerOptions` and `nativeSearch` have no meaning here.

The CLI process is spawned by the sidecar itself (`spawnClaudeCodeProcess`) so that `release`, `cancel` and shutdown can kill it immediately: the SDK's graceful path closes stdin first and lets the CLI finish its turn during a grace window, which on a parked tool round means one more billable API call. Errors are `permanent` or `cancelled` only (the CLI already retries 429/5xx itself). Model discovery does not come through here at all — the host answers it from a built-in registry — and the only request the CLI makes beyond `POST /v1/messages` is a credential-free `HEAD /api/hello` preconnect.

Selfcheck discriminator 30 drives the real `~/.local/bin/claude(.exe)` (or `MEWORK_CLAUDE_EXECUTABLE`) against a scripted Anthropic upstream on `127.0.0.1`; without a native executable the section is skipped with a warning.

Anthropic is pinned to the base `webSearch_20250305`: `_20260209` defaults to `allowed_callers = ["code_execution_20260120"]`, while not every compatible endpoint provides code execution.

## Commands

```bash
npm run build          # dist/main.mjs (ESM, for development)
npm run selfcheck      # offline checks against the ESM bundle
npm run build:sea      # dist/mework-aisdk.exe (CJS -> SEA blob -> postject)
npm run selfcheck:sea  # same discriminators, run against the actual shipped single-file exe
npm run typecheck      # tsc --noEmit
```

`selfcheck:sea` is necessary because the development and release forms are separated by CJS bundling, SEA injection, and an invalidated signature. Any stage can fail only in the final artifact. Node SEA **requires CommonJS** entry points because ESM entry points are unsupported, so these are two distinct builds rather than one artifact in a different wrapper.

## Known pitfalls

- **postject invalidates the Authenticode signature embedded in node.exe** and warns about it. Before release, re-sign this executable **separately**: signing the NSIS installer does not sign the sidecar it contains.
- **`@ai-sdk/google-vertex` brings the CJS `@vercel/oidc` package**, which executes `require("path")` at module initialization. With an ESM bundle, it throws at **load time**, not only when the Vertex path runs, preventing the entire sidecar from starting. `build.mjs` restores `require` with a `createRequire` banner.
- **`streamText` defaults `onError` to `console.error(error)`**, while AI SDK's `APICallError` includes `requestBodyValues`: the **entire request body**, including the system prompt and all conversation text. Always pass `onError: () => {}` explicitly; the error still arrives through the stream's `error` part.
- **Do not read environment variables.** Without an explicit `apiKey`, each provider looks up variables such as `OPENAI_API_KEY`; an unrelated development-machine key could then be silently sent to a user-configured proxy. Pass an empty string explicitly as the fallback and let the upstream return 401.
- **AI SDK documentation may not match installed types.** In `ai@7.0.83`, `fullStream` part names are `text-start`/`text-delta`/`text-end`, `reasoning-start`/`-delta`/`-end`, and `tool-input-start`/`-delta`/`-end`, while the documentation says `'text'`. Before changing this layer, inspect `TextStreamPart` in `node_modules/ai/dist/index.d.ts` rather than copying the documentation.

## Reasoning levels do not branch here

`reasoning` is **AI SDK 7's shared vocabulary**: `none` / `low` / `medium` / `high` / `xhigh`. The host supplies only the level, and each provider translates it to its own control. The upstream mapping is more accurate than a host-maintained table: for adaptive models, Anthropic sends `thinking:{type:"adaptive",display:"summarized"}` with effort, lowering unsupported `xhigh` to `max`; for manual models, it derives `budgetTokens` as a percentage of `maxOutputTokens` with a minimum of 1024.

## Addresses: absent is not an empty string

An absent `baseURL` means to use that provider's default. Vertex and Bedrock derive endpoints from `project`/`location`/`region` in `settings`, and the AI SDK constructs them. Reimplementing this construction in the host adds a drifting duplicate. An empty **string** is different: it creates a relative address beginning with `/` and fails with `Invalid URL`, so both sides guard it by treating it as absent.

## Compatible-endpoint dialects

An upstream claiming compatibility with X does not guarantee byte-for-byte compatibility. `src/anthropic-dialect.ts` is the sole current adaptation point; it wraps `fetch` to normalize response bodies to the official shape.

**Error blocks when server-side search exhausts `max_uses`.** Anthropic officially sends a bare object:

```jsonc
"content": {"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}
```

DeepSeek's `/anthropic` endpoint sends this **inside an array**. `@ai-sdk/anthropic` expects `union([array(web_search_result), object(tool_result_error)])`, so neither shape matches and the entire stream ends with a **permanent** `Type validation failed` error. The user sees an interrupted turn although only that search's usage limit was reached.

Discriminator 8 replays the captured **wire bytes** in `fixtures/anthropic-max-uses-exceeded.sse`. It is a **payload discriminator**: deleting the `fetch:` line in `providers.ts` immediately reproduces the production `Type validation failed` error.

**References in the Responses path are implicit.** DeepSeek's `web_search` reports opened pages only through `web_search_call.action.url`; `message.content[].annotations` is an empty array. AI SDK's Responses adapter derives `source` parts from `url_citation` annotations, leaving `StepResult.sources` empty. **This does not currently affect the product**: citations for native search already appear in the model response, and the host has no `sources` consumer because `From<StepResult>` does not read it. Supporting citations would require extracting `action.url` from provider-executed tool calls.

## Not yet implemented

- Images use the `ImagePart` data-URL form after the host validates count, bytes, pixels, and vision capability. **File** parts such as PDFs are not supported.
