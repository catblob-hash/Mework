# mework-aisdk Sidecar

Mework's model-protocol executor. The host is Rust, this service is Node, and they communicate through NDJSON over stdio. Every host protocol entry point targets this service: the turn loop in `api.rs`, the one-shot native-search request, and historical projection.

**It performs exactly one task: one model step.** Tools are declared without `execute`, so the AI SDK stops at a tool call. Rust retains tool execution, approvals, hooks, persistence, cancellation, and the decision to begin the next turn; Mework's tool loop is coupled to the agent-kernel formal specification, subagent pool, and task abstraction.

## Packaging: `externalBin`

`tauri.conf.json` declares `externalBin: ["binaries/mework-aisdk"]`. Tauri resolves this to `src-tauri/binaries/mework-aisdk-<target-triple><exe>`, then removes the target triple during installation and places the binary beside the main executable. That is where the production branch of `resolve_binary()` looks.

Responsibilities are separated:

- `npm run build:sidecar` **only builds** `aisdk-service/dist/mework-aisdk.exe` (`dist/mework-aisdk` on macOS and Linux; on macOS the build also re-signs it ad hoc, as Node's SEA instructions require).
- `src-tauri/build.rs` **only stages** it. The target triple comes from Cargo's authoritative `TARGET`, without reparsing `rustc -vV`, and is recopied when the source artifact changes.

`beforeBuildCommand` runs both. Development requires no additional action: `cargo run` / `dev:browser` causes Tauri to copy the same executable beside `target/debug/`, where the production branch of `resolve_binary()` finds it. If neither artifact exists, `build.rs` stops with an error explaining how to produce it instead of leaving `externalBin` to fail obscurely at the end of packaging.

A freshly produced single-file executable takes 1–2 s on its first execution while Defender scans it; in steady state `hello`→`ready` is about 130 ms.

The sidecar is **one multiplexed, lazily started process**: parent turns and all subagents share it, demultiplexed by request ID. It starts on the first `run_step`, `shutdown_sidecar()` explicitly stops it at application exit, and a Windows Job Object is only a fallback.

## Protocol

Each line is one JSON value terminated by `\n`. Three rules apply:

1. **stdout carries only protocol data.** All logs go to stderr. At startup, `main.ts` pins the entire `console.*` family to stderr because any dependency's `console.log` could insert partial text into the protocol stream.
2. **Both sides enforce a line limit** of 16 MiB. Neither the host nor the sidecar trusts the other.
3. **`seq` is process-global and monotonic**, not a per-request counter. Losing all frames for an entire request must still be observable.

### Host → Sidecar

```jsonc
{"v":11,"type":"hello"}
{"v":11,"type":"step","id":"r1","payload":{ /* StepRequest */ }}
{"v":11,"type":"cancel","id":"r1"}
{"v":11,"type":"release","session":"run-…"}          // claude-agent only: the host run ended; no reply
{"v":11,"type":"shutdown"}
```

### Sidecar → Host

```jsonc
{"v":11,"seq":0,"type":"ready","protocol":11,"versions":{"node":"24.19.0"}}
{"v":11,"seq":1,"type":"event","id":"r1","event":{"k":"text-delta","delta":"你好"}}
{"v":11,"seq":2,"type":"done","id":"r1","result":{ /* StepResult */ }}
{"v":11,"seq":3,"type":"error","id":"r1","error":{"kind":"transient","message":"…"}}
```

Event `k` values correspond one-to-one with Rust's `ModelStreamEvent`; the host only forwards them:
`text-delta` / `reasoning-start` / `reasoning-delta` / `reasoning-done` /
`tool-call-announced` / `tool-call` / `usage` / `source` / `heartbeat`.

`reasoning-start` is **not** a prefix of `reasoning-delta`; it is an independent fact. Responses emits it at `response.output_item.added`, even when the summary emits no text because only `reasoning.encrypted_content` was requested or the provider supplies no summary. Without it, reasoning that occurred and incurred cost is absent from both host and UI.

`reasoning-done.durationMs` and `done.result.reasoningMs` are **the same value**: the sum of start-to-end durations for every reasoning item in the step. Timing remains in the sidecar because it alone observes the actual stream ordering. A shared source ensures that the live display and persisted value agree after reload. The **presence** of `reasoningMs`, rather than a value greater than zero, indicates that the step reasoned; sub-millisecond reasoning still counts.

`heartbeat` fires every 500 ms and carries the host cancellation probe. **It is a cancellation-probe carrier, not evidence that the upstream is live.**

Images use the `ImagePart` data-URL form after the host validates count, bytes, pixels, and vision capability. **File** parts such as PDFs are not supported.

### The transport layer does not infer failure from silence

At startup, the sidecar installs a global undici dispatcher with `headersTimeout` and `bodyTimeout` set to `0`, undici's documented disabled value, instead of their 300-second defaults.

This is necessary because protocols that return only encrypted reasoning fields, such as Responses with `store` disabled and only `reasoning.encrypted_content` requested, emit no frames during reasoning except the opening `reasoning-start`. A healthy long reasoning period and a stalled upstream are indistinguishable by elapsed content silence. Incorrectly aborting loses the entire turn, including already-generated, billable reasoning. Real request failures provide explicit signals: a closed connection, an error frame, or an HTTP status code. **Connection timeout remains at its default** because it detects failure to connect, not silence after connection.

The dispatcher is global rather than a per-provider `fetch`: npm undici and Node's bundled copy share the slot at `Symbol.for("undici.globalDispatcher.1")`. One setting therefore covers AI SDK requests, the native-search leg, and token requests from the google-vertex credential chain. This shared symbol is an **undocumented implementation detail**.

`done.result.responseMessages` is an **opaque continuation block** from AI SDK `response.messages`. Anthropic's `encryptedContent` and reasoning signature, and Responses reasoning items, survive across turns through it. The host persists and replays it unchanged.

`error.kind` is the **only** field recognized by the host retry loop. Classification remains in the sidecar because only it observes AI SDK error types. It uses duck typing rather than `instanceof APICallError`: a second `@ai-sdk/provider` copy in `node_modules` silently makes `instanceof` false, and a retryable failure classified as permanent is indistinguishable in the UI from an upstream outage.

## Adapter families

`src/providers.ts` maps provider families to AI SDK providers, exhaustively over Rust's `ProviderFamily`. `src/search.ts` selects the native search and fetch tool for a family.

| family | provider | 原生 web_search | 原生 web_fetch |
| --- | --- | --- | --- |
| `openai-responses` | `createOpenAI().responses()` | `tools.webSearch()` | 无 |
| `openai-codex` | `createOpenAI().responses()` + Codex dialect | `tools.webSearch()` | 无 |
| `openai-chat` | `createOpenAICompatible().chatModel()` | 无（可修复的拒绝） | 无 |
| `anthropic` | `createAnthropic()` | `tools.webSearch_20250305({maxUses})` | `tools.webFetch_20250910({maxUses})` |
| `google` | `createGoogle()` | `tools.googleSearch()` | 无 |
| `xai` | `createXai()` | `tools.webSearch()` | 无 |
| `azure` | `createAzure()` | `tools.webSearchPreview()` | 无 |
| `bedrock` | `createAmazonBedrock()` | `tools.webSearch_20250305()` | `tools.webFetch_20250910()` |
| `vertex` | `createGoogleVertex()` | `tools.googleSearch()` | 无 |
| `openai-compatible` | `createOpenAICompatible().chatModel()` | 无（可修复的拒绝） | 无 |
| `claude-agent` | not AI SDK: `claude-agent.ts` drives the Claude Code executable Mework ships through `@anthropic-ai/claude-agent-sdk` | 无（宿主解析并随包附带） | 无 |

**搜索与抓取不对称，这一点承重。** 没有任何一家的 *搜索* 结果里带宿主可读的正文：Anthropic 把它封在 `encrypted_content` 里，Responses 干脆不返回。所以原生搜索仍然要靠一次嵌套模型请求把结果写成报告。抓取则相反：Anthropic 的 `web_fetch_result` 用 `content.source.{type:"text", data}` 直接给出转成纯文本的页面，宿主自己解析，不需要模型复述。这就是 `StepResult.webDocuments` 只在抓取那条腿上非空的原因，也是对话设置里「抓取提供商」这个选项存在的原因——只有会抓取的后端才不需要另外指定一家。

**Responses-compatible endpoints use `createOpenAI`, not openai-compatible.** This preserves DeepSeek native search: any upstream that implements Responses receives `openai.tools.webSearch()`, whereas the generic openai-compatible provider discards provider tools together with its `unsupported` warning.

- **`openai-codex`** targets ChatGPT-subscription Codex. Its dialect forces the backend's required `stream:true`, `store:false`, and `reasoning.encrypted_content` include, removes rejected `max_output_tokens`, supplies the absent SSE content type, and converts a completed SSE response back to JSON for a non-streaming call. OAuth bearer and ChatGPT account/origin headers are supplied unchanged by the host; the sidecar adds no authentication.

### `claude-agent`: Claude Code as a model backend

`src/claude-agent.ts` is the one family that bypasses `resolveModel()` and `streamText()`. The host's `StepRequest` carries an extra `agent: { session, executable, cwd, env }` block; the sidecar spawns the **Claude Code executable Mework ships** — the CLI inside the pinned Agent SDK's platform package (SDK `0.3.261`, Claude Code `2.1.261`), staged by `src-tauri/build.rs` and installed beside the application, never the user's own install — through the official Agent SDK, and keeps one CLI process per host run, keyed by `agent.session`. Pinning it is what makes the rest of this section true from one release to the next: the CLI's behaviour changes between versions, and `CLAUDE_CODE_CARVED_SLATE=0` — which suppresses the `# Environment` block the CLI would otherwise attach to the first user message — exists in `2.1.261` and is gone in `2.1.278`.

What the CLI is allowed to do is nothing of its own: `tools: []`, `settingSources: []`, `strictMcpConfig: true`, no plugins/agents, and an environment that switches off compaction, attachments, auto-memory, CLAUDE.md, background tasks, telemetry and updates. The only tools the model sees are the host's, published by one hand-written in-process MCP server (`mework`) so the host schemas reach the model verbatim. `CLAUDE_AGENT_SDK_MCP_NO_PREFIX=1` makes the CLI register them under their bare host names, so nothing in the tool loop ever sees an `mcp__mework__` prefix (it is still stripped defensively). The request the CLI sends therefore has `system = [billing header, "You are a Claude agent, built on Anthropic's Claude Agent SDK.", host prompt]` and `tools = the host's tools`; the SDK's identity line and billing header are the SDK's own and are not touched.

When the model calls a tool the MCP handler **parks**, the step ends with `done{calls}`, and the next step for the same session — recognised because its trailing `tool` message answers exactly the parked ids — resolves the handlers and streams the following reply. Carriers riding behind the results — the image bridge, and the host's fabricated `box` exchange carrying a background task's terminal result — are folded into the last tool result rather than sent as user turns, because the CLI would otherwise wrap them as "the user sent a new message while you were working"; the `box` call itself is skipped when scanning back for the last real assistant turn, since the host issued it, not the CLI. The `[SYSTEM NOTIFICATION - NOT USER INPUT]` literal lives in this module in two places: `CONTINUE_NOTICE`, the prompt injected when a parked round has to continue in a fresh CLI session (it lives in that session alone and never enters host history), and a compatibility arm that recognises archived conversations which recorded a delivery as user-role text. A new run starts a new CLI session; host history is rewritten into a Claude Code transcript and injected through `resume` + `sessionStore.load()` (the SDK materialises it in a temporary `CLAUDE_CONFIG_DIR` and copies the CLI's own credentials there itself — this module never reads them). Fresh sessions without history run with `persistSession:false`, so nothing of Mework's lands in `~/.claude/projects`.

Credentials: there are none. This family authenticates with the user's own Claude Code login and nothing else, so `apiKey` and `baseURL` on the request are ignored, and the ambient `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL` / `ANTHROPIC_CUSTOM_HEADERS` / `CLAUDE_CODE_OAUTH_TOKEN` of the sidecar process are deleted before the CLI environment is composed — the Agent SDK only falls back to the stored login when no credential is forced through the environment. `CLAUDE_CODE_USE_BEDROCK` / `_VERTEX` / `_FOUNDRY` are pinned to `0`, and a `claude-`prefixed model id is pinned into `ANTHROPIC_MODEL` and the three `ANTHROPIC_DEFAULT_*_MODEL` variables (an alias such as `sonnet` or `opus[1m]` is left for the CLI to resolve). `CLAUDE_CONFIG_DIR` is never set here, because that is where the login lives and macOS Keychain lookup only happens while it is absent. The single upstream override is a **local test stub**: `agent.env` may carry an `ANTHROPIC_BASE_URL` pointing at a loopback host — a remote one fails the request as `permanent` — and only then is an `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` from `agent.env` passed through. `reasoning` maps to the CLI's `--effort` (`none` disables thinking), `maxOutputTokens` to `CLAUDE_CODE_MAX_OUTPUT_TOKENS`; `temperature`, `toolChoice`, `providerOptions` and `nativeSearch` have no meaning here.

The CLI process is spawned by the sidecar itself (`spawnClaudeCodeProcess`) so that `release`, `cancel` and shutdown can kill it immediately: the SDK's graceful path closes stdin first and lets the CLI finish its turn during a grace window, which on a parked tool round means one more billable API call. Errors are `permanent` or `cancelled` only (the CLI already retries 429/5xx itself). Model discovery does not come through here at all — the host answers it from a built-in registry — and the only request the CLI makes beyond `POST /v1/messages` is a credential-free `HEAD /api/hello` preconnect.

## Commands

```bash
npm run build          # dist/main.mjs (ESM, for development)
npm run selfcheck      # offline checks against the ESM bundle
npm run build:sea      # dist/mework-aisdk[.exe] (CJS -> SEA blob -> postject)
npm run selfcheck:sea  # same discriminators, run against the actual shipped single-file exe
npm run typecheck      # tsc --noEmit
```

`npm run selfcheck` runs offline against one local fake upstream and one real sidecar child process; no API key is required, and the `claude-agent` section additionally needs the Agent SDK's platform package (installed by `npm install` unless optional dependencies were skipped) and is skipped without it.

The development and release forms are separated by CJS bundling, SEA injection, and an invalidated signature, so any stage can fail only in the final artifact. Node SEA **requires CommonJS** entry points because ESM entry points are unsupported, which makes these two distinct builds rather than one artifact in a different wrapper.

## Known pitfalls

- **postject invalidates the Authenticode signature embedded in node.exe** and warns about it. Signing the NSIS installer does not sign the sidecar it contains.
- **`@ai-sdk/google-vertex` brings the CJS `@vercel/oidc` package**, which executes `require("path")` at module initialization. With an ESM bundle, it throws at **load time**, not only when the Vertex path runs, preventing the entire sidecar from starting. `build.mjs` restores `require` with a `createRequire` banner.
- **`streamText` defaults `onError` to `console.error(error)`**, while AI SDK's `APICallError` includes `requestBodyValues`: the **entire request body**, including the system prompt and all conversation text. `main.ts` passes `onError: () => {}` explicitly; the error still arrives through the stream's `error` part.
- **Providers read environment variables when no `apiKey` is given.** Each provider then looks up variables such as `OPENAI_API_KEY`, and an unrelated development-machine key could be silently sent to a user-configured proxy. An empty string is passed explicitly as the fallback so the upstream returns 401.
- **AI SDK documentation may not match installed types.** In `ai@7.0.83`, `fullStream` part names are `text-start`/`text-delta`/`text-end`, `reasoning-start`/`-delta`/`-end`, and `tool-input-start`/`-delta`/`-end`, while the documentation says `'text'`. The installed shape is `TextStreamPart` in `node_modules/ai/dist/index.d.ts`.

## Reasoning levels do not branch here

`reasoning` is **AI SDK 7's shared vocabulary**: `none` / `low` / `medium` / `high` / `xhigh`. The host supplies only the level, and each provider translates it to its own control: for adaptive models, Anthropic sends `thinking:{type:"adaptive",display:"summarized"}` with effort, lowering unsupported `xhigh` to `max`; for manual models, it derives `budgetTokens` as a percentage of `maxOutputTokens` with a minimum of 1024.

## Addresses: absent is not an empty string

An absent `baseURL` means to use that provider's default. Vertex and Bedrock derive endpoints from `project`/`location`/`region` in `settings`, and the AI SDK constructs them. An empty **string** is different: it creates a relative address beginning with `/` and fails with `Invalid URL`, so both sides guard it by treating it as absent.

## Compatible-endpoint dialects

An upstream claiming compatibility with X does not guarantee byte-for-byte compatibility. `src/anthropic-dialect.ts` wraps `fetch` to normalize response bodies to the official shape.

**Error blocks when server-side search exhausts `max_uses`.** Anthropic officially sends a bare object:

```jsonc
"content": {"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}
```

DeepSeek's `/anthropic` endpoint sends this **inside an array**. `@ai-sdk/anthropic` expects `union([array(web_search_result), object(tool_result_error)])`, so neither shape matches and the entire stream ends with a **permanent** `Type validation failed` error. The user sees an interrupted turn although only that search's usage limit was reached. The captured wire bytes are in `fixtures/anthropic-max-uses-exceeded.sse`.

**References in the Responses path are implicit.** DeepSeek's `web_search` reports opened pages only through `web_search_call.action.url`; `message.content[].annotations` is an empty array, and AI SDK's Responses adapter derives `source` parts from `url_citation` annotations, so it produces none. The sidecar therefore harvests `action.url` and `action.sources[].url` out of provider-executed `tool-result` parts itself (`main.ts`, the `web_search` branch), and the host asks for `include: ["web_search_call.action.sources"]` on its one-shot search request so the full consulted set comes back rather than only the URLs the model chose to cite. **This is load-bearing**: `StepResult.sources` becomes the `sources` array of the `web_search` result envelope, which is what the timeline counts to say "已搜索 N 个网站" and what it draws one chip per site from.
