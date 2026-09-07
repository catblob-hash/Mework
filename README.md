# Mework

> **Mework — Mew. Work.**

**English** | [简体中文](README.zh-CN.md)

📖 **Documentation:** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/en/index.html) — how to work with Mework, the tool-description file (prompt profile) reference, and how to configure skills, MCP servers and hooks.

Mework is a local-first agent workbench for Windows. It pairs a React 19 interface with a Rust host built on Tauri 2: the host owns persistence, capability discovery, tool execution, and every safety boundary, while a bundled AI SDK sidecar is the only path to model providers. You bring your own API keys; nothing leaves your machine except the model calls you configure.

## Highlights

**A formally specified agent kernel.** The turn/round/tool/subagent lifecycle is specified before it is implemented: a CSP-M interaction protocol ([`formal/csp/AgentKernel.csp`](formal/csp/AgentKernel.csp), the highest authority) and a TLA+ safety model ([`formal/tla/AgentKernel.tla`](formal/tla/AgentKernel.tla)) are checked with ProB — model checking, CSP refinement with 93 scenario assertions (21 accepted, 72 rejected), guard-deletion mutation matrices (63 TLA + 92 CSP), and bidirectional trace replay against the dependency-light Rust kernel crate (`src-tauri/agent-kernel`). At runtime a shadow kernel observes the real host event stream and reports any divergence from the spec.

**Bring your own providers.** OpenAI Responses, OpenAI Chat Completions, Anthropic Messages, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI, and generic OpenAI-compatible endpoints — all through an embedded [Vercel AI SDK](https://sdk.vercel.ai) sidecar compiled to a single executable. Providers are user-defined; there is no built-in vendor list to outgrow. Two families step outside the plain "base URL plus key" shape. **OpenAI Codex**: sign in with your ChatGPT subscription (Plus / Pro / Team) instead of an API key — the OAuth flow runs in the host, tokens stay encrypted on this machine and never reach the UI. **Claude Agent (Claude Code)**: the sidecar drives the Claude Code executable you already have installed through the official [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview), with every Claude Code behaviour switched off (no built-in tools, CLAUDE.md, hooks, MCP servers, compaction or background tasks) and Mework's own tools published to the model under their own names — so Mework's approvals, hooks, MCP servers and skills apply unchanged. There is no key or address to enter: Mework reuses the Claude Code login already on this machine (`claude auth login`), for your own use and subject to the [Claude Code terms](https://code.claude.com/docs/en/legal-and-compliance). For every other provider, API keys live in the OS credential store, never in documents, and requests refuse redirects and embedded credentials.

**27 built-in tools.**
- *Filesystem* (6): `ls`, `grep`, `read`, `write`, `edit`, `find` — workspace-bounded, traversal- and symlink-safe, atomic writes.
- *Shell* (2): `powershell`, `bash` — foreground or background, with per-call approval and full-argument native dialogs.
- *Web* (3): `web_search` and `web_fetch` (provider-native server-side search, or host-run backends such as Tavily, Exa, SearXNG, Jina, Firecrawl, Bocha, Zhipu), plus `playwright`, a 23-action browser-automation tool over the built-in browser.
- *Orchestration* (10): `agent_spawn`, `send_message`, `followup_task`, `task_wait`, `task_list`, `workflow`, `skill`, `todo`, `ask_user`, `fork`.
- *Memory* (6): two-tier plain-Markdown long-term memory (global `~/.mework` and per-project `<workspace>/.mework`) with independent read/create/edit tools per tier.

**Subagents and scripted workflows.** Spawn up to 8 concurrent background subagents that survive across turns and app restarts; message them, wake them, and block on their results with `task_wait`. The `workflow` tool runs a JavaScript orchestration script (`agent()`, `parallel()`, `pipeline()`, `phase()`, `budget`) with incremental persistence, crash recovery, and optional per-step git-worktree isolation.

**A real built-in browser.** Every tab is an isolated, single-use WebView2 profile — no shared cookies, no imported credentials, destroyed on close. Agents drive pages through accessibility-tree snapshots and trusted CDP input; automation shows a visible pointer overlay without injecting anything into the page, and remote content can never reach the Tauri IPC surface.

**Run environments.** Shell tools can execute locally, inside a WSL distribution, or on an SSH machine, with per-environment environment variables and approval fingerprints that invalidate when the environment changes.

**Extensible.** Skills (`SKILL.md` directories, importable from disk, ZIP, or system skill folders), lifecycle hooks (7 events: `SessionStart`, `InstructionsLoaded`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`), and MCP servers over stdio or Streamable HTTP.

**Every host prompt is declared.** All fixed text Mework injects into a model request — the default system prompt, capability sections, safety boundaries, subagent addendum, task receipts, tool framing — lives in one registry with an English default compiled in and a Chinese profile shipped alongside. A hand-written [tool-description file](https://catblob-hash.github.io/Mework/en/prompt-profiles.html) can override any of it per conversation.

**Deterministic safety model.** Layered authorization decides per call: conversation allowlists, risk classification, native approval dialogs that show the full command and workspace, single-use nonces for manual high-risk execution, path guards on every file access, and hook-based deny/rewrite. Approvals are consumed before execution, never after.

**Durable persistence.** A versioned JSON anchor plus a SQLite conversation store with crash-safe incremental streaming rows; corrupted or future-versioned data is quarantined and rebuilt, never silently migrated over.

**Lives in the tray.** Closing the window only hides it; subagents, workflows, shell tasks and the sidecar keep running. The tray icon's menu reopens the window or quits Mework (quitting still flushes everything to disk first), and launching Mework again while it sits in the tray normally just brings the window back instead of starting a second copy.

## Install (Windows)

Grab either flavor from [Releases](../../releases):

- **Installer** — `Mework_1.0.2_x64-setup.exe` (NSIS, per-machine).
- **Portable** — `Mework_1.0.2_x64_portable.zip`: unzip anywhere and run `mework.exe` (keep `mework-aisdk.exe` next to it).

Both require the Microsoft Edge WebView2 Runtime, which is preinstalled on current Windows 10/11; the installer can bootstrap it if missing.

"Portable" means no installation, not no state: the app still writes to `%APPDATA%\com.mework.app`, `%LOCALAPPDATA%\com.mework.app`, and Windows Credential Manager.

First run: open *Settings → Providers → Model providers*, add a provider and its key (or sign in for the Codex and Claude Agent providers), add a model from the discovery page, and pick it under the composer.

*Settings → Updates* shows the running version and checks GitHub Releases for a newer one. The installer flavor downloads the new `-setup.exe`, verifies it against the release's `SHA256SUMS` when present, and runs it in update mode (settings and data are kept; the app restarts). The portable flavor downloads the new zip to your Downloads folder and shows it in Explorer — quit Mework from the tray icon and unzip it over the old files.

## Build from source

Prerequisites: Node.js ≥ 22.12 (22.23.1 pinned in `.node-version`), stable Rust, Windows with WebView2, and Visual Studio Build Tools (MSVC).

The build also needs a native **mingw64** toolchain — `gcc`, `make` and `perl` — because some crates compile C sources. Install [MSYS2](https://www.msys2.org/) and then:

```bash
pacman -S mingw-w64-x86_64-gcc make perl
```

The build wrapper looks for MSYS2 in the usual install locations; if yours is elsewhere, point `MSYS2_ROOT` at the directory that contains both `mingw64\bin` and `usr\bin`. `mingw64\bin` must precede `usr\bin` on `PATH`, which the wrapper arranges for you.

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

`tauri:build` builds the frontend, compiles the sidecar to a single-file executable (Node SEA), and produces the NSIS installer under `src-tauri/target/release/bundle/nsis/`; the bare `mework.exe` and `mework-aisdk.exe` land in `src-tauri/target/release/`.

```bash
npm run package:portable   # zip the portable flavor from an existing tauri:build output
```

```bash
npm run licenses:third-party   # regenerate THIRD-PARTY-LICENSES.md from the lockfiles (run before a release when dependencies changed)
```

```bash
npm run release:assets   # copies installer + portable zip and writes SHA256SUMS for the GitHub release
```

Publish a GitHub release tagged `v<version>` with the three files; the in-app updater relies on the `v` tag prefix, the `-setup.exe` / `_portable.zip` name patterns, and (optionally) `SHA256SUMS`.

## Development

```bash
npm run dev            # static UI preview, no backend
npm run dev:browser    # full-stack browser bridge on 127.0.0.1:1420 (real Rust backend, no native window)
npm test               # lint + frontend checks + vitest
npm run test:rust      # cargo test
npm run verify:formal  # full formal pipeline (fetch ProB first: npm run prob:fetch)
npm run reset:data     # wipe local app data (schema bumps are not migrated)
```

## Repository layout

| Path | What it is |
|---|---|
| `src/` | React 19 + TypeScript UI |
| `src-tauri/src/` | Rust host: tool executor, conversation store, capability & safety layers, kernel shadow |
| `src-tauri/agent-kernel/` | Formal-kernel Rust projection + conformance trace exporter |
| `src-tauri/workflow-core/`, `src-tauri/workflow-script/` | Workflow orchestration engine and its JS runtime |
| `formal/` | CSP-M protocol spec and TLA+ safety model |
| `aisdk-service/` | Node sidecar (AI SDK): the only model-provider network path |
| `scripts/` | Dev, test, and verification pipelines |

## Status

Mework 1.0.2 targets Windows. The codebase carries cross-platform seams (keyring backends, POSIX shell paths), but only the Windows build is released and supported today.

## License

[GNU General Public License v3.0 or later](LICENSE).

Redistributed third-party components and their terms are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md); the licenses of the libraries compiled into the release executables are inventoried in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). Both files ship inside the installer and the portable archive.
