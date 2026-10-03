# Mework documentation

Mework is a local-first agent workbench for Windows. You bring your own model provider and API key; the app runs the tools, keeps every safety boundary on the host side, and stores everything on your machine. This site explains how to work with it day to day, how to shape what the model is told through **tool-description files (prompt profiles)**, and how to extend it with **skills**, **MCP servers** and **lifecycle hooks**.

> The source, releases and issue tracker live at [github.com/catblob-hash/Mework](https://github.com/catblob-hash/Mework). This site is built from the `website/content` directory of that repository; corrections are welcome as pull requests.

## What Mework is

- A **desktop app** (Tauri 2, WebView2) with a React interface and a Rust host. The host owns persistence, tool execution, approvals and the browser; the interface only displays state and sends intents.
- A **bring-your-own-provider** client. OpenAI Responses and Chat Completions, Anthropic Messages, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI and generic OpenAI-compatible endpoints all go through one embedded AI SDK sidecar. There is no Mework account: every key is yours, and the two built-in families — OpenAI Codex and Claude Agent — use your own ChatGPT and Claude Code logins.
- An **agent runtime** with {{TOOL_COUNT}} built-in tools: filesystem tools including language-server code navigation (`lsp`), shell, web search and fetch, fifteen `preview_*` tools that run the project's dev server and act on the page it serves, subagents, scripted workflows, a way to ask you questions, a fork into a nested sub-conversation, on-demand loading of skills (`skill`) and of MCP tool schemas (`tool_search`), and two tiers of plain-Markdown long-term memory. {{SWITCHABLE_TOOL_COUNT}} of them are conversation settings you turn on and off; the host derives the other {{DERIVED_TOOL_COUNT}} from the two memory switches, the web-access switch, the skill-delivery and MCP tool-discovery switches, the security level, whether the conversation can produce a task, and whether any other preview tool is on. Each one has its own page under [Built-in tools](tools.html).
- A **built-in browser** that is two surfaces at once: a real browser you drive — address bar, back/forward/reload, a dev-server picker, a log drawer — and the page the model starts from the workspace's `.mework/launch.json` and verifies its own work on.
- **Deterministic about safety.** Every tool call is classified on the host against the conversation's security level; approvals are consumed before execution; file access is bounded to the workspace and the directories you grant a conversation; the browser profile is single-use and isolated.

## Install

Download either flavor from the [Releases](https://github.com/catblob-hash/Mework/releases) page, or from its mirror at `https://dl.mework.dev/<tag>/<file>` (for example `https://dl.mework.dev/v1.0.0/Mework_1.0.0_x64-setup.exe`), which serves the same files from Cloudflare where GitHub's downloads are slow:

| Flavor | File | Notes |
|---|---|---|
| Installer | `Mework_<version>_x64-setup.exe` | NSIS, per-machine. Bootstraps the WebView2 runtime if it is missing. |
| Portable | `Mework_<version>_x64_portable.zip` | Unzip anywhere and run `mework.exe`. Keep `mework-aisdk.exe` next to it — it is the only path to model providers. |

"Portable" means no installer, not no state. Both flavors write to the same locations:

| Location | Contents |
|---|---|
| `%APPDATA%\com.mework.app` | The settings document (`document.v1.json`), the conversation database, image attachments, workflow run journals |
| `%LOCALAPPDATA%\com.mework.app` | WebView2 data and the single-use browser profiles |
| Windows Credential Manager | API keys (never written into the document) |
| `~/.mework` | Things you author by hand: long-term memory, `hooks.json`, `skills/`, `mcp.json`, `lsp.json`, `tool-descriptions/*.json` |

## First run

1. Open **Settings → Providers → Model providers**, add a provider and paste its API key (the Codex and Claude Agent providers sign in instead). Click **Fetch models** to open the **Discover models** drawer and add the models you want, or add a model id by hand; a model in the list is ready to use.
2. Back in the workspace view, add a workspace pointing at a folder (**Add workspace** in the sidebar, then **Browse** for the folder) or start in the temporary workspace.
3. Pick the model under the composer, type a task, and send. The first tool call that needs permission shows an approval card naming the tool, what the call will act on, its risk level and the reason it was classified that way; approve once, or choose **Always allow** to remember that tool in that conversation — an offer mandatory confirmations, shell calls and the `preview_*` tools that act on the page never make.

A new conversation opens from the default preset. Out of the box that is **mework**, the one preset Mework ships. It is built in: it updates with each version of Mework and cannot be edited or deleted. It turns on every switchable tool (of the shell tools, only this machine's preferred shell), web access, both memory tiers, on-demand skills and MCP tool discovery; the security level is **Manual** (`request_approval`); the tool descriptions come from the built-in prompt profile; its subagent roles are **Opus** and **Sonnet** (Claude Opus 5.5 and Claude Sonnet 5.5 through Claude Agent), **Sol** and **Luna** (GPT-6.1 Sol and GPT-6 Luna through Codex); and its conversation template is a general engineering system prompt. All of that is a conversation setting you can change afterwards, and you can save the result as a **preset** of your own.

## How a turn works

Understanding the loop makes the rest of the settings obvious:

1. You send a message. The host rebuilds the request from the **persisted** conversation — the renderer cannot inject tools, prompts or history the host did not store.
2. The system prompt is assembled, host sections first: the environment block (working directory, Git and worktree facts, extra directories, platform, OS version, date), the selected skills, the selected MCP servers and hooks, and optionally the app-data directory. Your conversation prompt follows them — the first card of the timeline, when it is a system card; a system card anywhere else is not sent. Mework has no default of its own, so an empty one simply leaves the host sections standing. A conversation opened by a handoff ends the system prompt with the index of the notes it inherited, on a model that reads its tools ahead of its system prompt. Nothing else is added, for any provider: the conversation's own text sits last, so the tools and the host sections ahead of it are the prefix every conversation of the same setup shares.
3. The model answers with text and/or tool calls. Each call is classified (**read / write / unbounded**, inside or outside the workspace, mandatory confirmation or not), run through `PreToolUse` and — only for calls that would otherwise show an approval card — `PermissionRequest`, approved if necessary, executed, and run through `PostToolUse`. Results go back to the model in the next round.
4. Background work — subagents, workflow runs, background shell commands, the dev servers `preview_start` spawned — keeps running after the turn ends. Their results are delivered at the next round boundary as a `<task-notification>`, or when the model calls `task_wait`.
5. The turn ends when the model stops calling tools and no `Stop` hook sends it back to work, when a hook halts the run with `continue: false`, when the model asks you a question with `ask_user`, or when you stop it.

Every fixed sentence the host adds in steps 2–4 is declared in the prompt profile, so you can read exactly what the model was told — see [Prompt profiles](prompt-profiles.html).

## Where to go next

- [Working with Mework](working.html) — workspaces, conversations, presets, tools and approvals, run environments, subagents and workflows, web search, the browser, memory, images and data locations.
- [Built-in tools](tools.html) — every tool with its own page: what it does, when it asks for approval, its parameters, and the description the model is given.
- [Prompt profiles](prompt-profiles.html) — the tool-description file format, every injection point, placeholders, fallback rules and examples.
- [Skills](skills.html) — `SKILL.md` folders: where they live, selecting, on-demand loading.
- [MCP servers](mcp.html) — stdio and Streamable HTTP servers, per-conversation selection, approvals.
- [Code navigation](lsp.html) — the `lsp` tool, its nine operations, `lsp.json` and the built-in server presets.
- [Hooks](hooks.html) — `hooks.json`, the seven lifecycle events, the stdin/stdout contract, examples.

## Building from source

Prerequisites: Node.js ≥ 22.12, stable Rust, Windows with WebView2, Visual Studio Build Tools (MSVC) and an MSYS2 mingw64 toolchain (`gcc`, `make`, `perl`) for the crates that compile C sources.

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

`tauri:build` builds the frontend, compiles the sidecar to a single executable, and produces the installer under `src-tauri/target/release/bundle/nsis/`. `npm run package:portable` zips the portable flavor from that output. The repository README has the complete list of development commands.
