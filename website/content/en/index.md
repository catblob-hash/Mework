# Mework documentation

Mework is a local-first agent workbench for Windows. You bring your own model provider and API key; the app runs the tools, keeps every safety boundary on the host side, and stores everything on your machine. This site explains how to work with it day to day, how to shape what the model is told through **tool-description files (prompt profiles)**, and how to extend it with **skills**, **MCP servers** and **lifecycle hooks**.

> The source, releases and issue tracker live at [github.com/catblob-hash/Mework](https://github.com/catblob-hash/Mework). This site is built from the `website/content` directory of that repository; corrections are welcome as pull requests.

## What Mework is

- A **desktop app** (Tauri 2, WebView2) with a React interface and a Rust host. The host owns persistence, tool execution, approvals and the browser; the interface only displays state and sends intents.
- A **bring-your-own-provider** client. OpenAI Responses and Chat Completions, Anthropic Messages, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI and generic OpenAI-compatible endpoints all go through one embedded AI SDK sidecar. There is no built-in vendor account.
- An **agent runtime** with 26 built-in tools: filesystem, shell, web search and fetch, a real built-in browser, subagents, scripted workflows, a todo list, a way to ask you questions, and two tiers of plain-Markdown long-term memory.
- **Deterministic about safety.** Every tool call is classified on the host against the conversation's security level; approvals are consumed before execution; file access is bounded to the workspace; the browser profile is single-use and isolated.

## Install

Download either flavor from the [Releases](https://github.com/catblob-hash/Mework/releases) page:

| Flavor | File | Notes |
|---|---|---|
| Installer | `Mework_<version>_x64-setup.exe` | NSIS, per-machine. Bootstraps the WebView2 runtime if it is missing. |
| Portable | `Mework_<version>_x64_portable.zip` | Unzip anywhere and run `mework.exe`. Keep `mework-aisdk.exe` next to it — it is the only path to model providers. |

"Portable" means no installer, not no state. Both flavors write to the same locations:

| Location | Contents |
|---|---|
| `%APPDATA%\com.mework.app` | The settings document (`document.v1.json`), the conversation database, installed skills, image attachments, workflow run journals |
| `%LOCALAPPDATA%\com.mework.app` | WebView2 data and the single-use browser profiles |
| Windows Credential Manager | API keys (never written into the document) |
| `~/.mework` | Things you author by hand: long-term memory, `hooks.json`, `tool-descriptions/*.json`, skill folders to import |

## First run

1. Open **Settings → Providers → Model providers**, add a provider and paste its API key (the Codex and Claude Agent providers sign in instead). Click **Fetch models** to open the discovery page and add the models you want, or add a model id by hand; a model in the list is ready to use. Pick one as the provider's current model.
2. Back in the workspace view, create a workspace pointing at a folder (the **+** on a workspace row) or start with the scratch workspace.
3. Pick the model under the composer, type a task, and send. The first tool call that needs permission shows an approval card with the full command or path; approve once, or choose "always allow" for that tool in that conversation.

The default conversation runs with the file tools, the shell tool for your platform, and `web_search` enabled, at the **request approval** security level, and with the built-in English prompt profile. All of that is a conversation setting you can change, and a **preset** you can save and reuse.

## How a turn works

Understanding the loop makes the rest of the settings obvious:

1. You send a message. The host rebuilds the request from the **persisted** conversation — the renderer cannot inject tools, prompts or history the host did not store.
2. The system prompt is assembled: your conversation prompt (or the profile's default when it is empty), the selected skills, the selected MCP servers and hooks, optionally the app-data directory, and — when `web_search` is enabled — the web evidence safety boundary.
3. The model answers with text and/or tool calls. Each call is classified (**read / write / unbounded**, inside or outside the workspace, mandatory confirmation or not), run through `PreToolUse` / `PermissionRequest` hooks, approved if necessary, executed, and run through `PostToolUse`. Results go back to the model in the next round.
4. Background work — subagents, workflow runs, background shell commands — keeps running after the turn ends. Their results are delivered at the next round boundary as a `<task-notification>`, or when the model calls `task_wait`.
5. The turn ends when the model stops calling tools, a `Stop` hook halts it, the model asks you a question with `ask_user`, or you stop it.

Every fixed sentence the host adds in steps 2–4 is declared in the prompt profile, so you can read exactly what the model was told — see [Prompt profiles](prompt-profiles.html).

## Where to go next

- [Working with Mework](working.html) — workspaces, conversations, presets, tools and approvals, run environments, subagents and workflows, web search, the browser, memory, images and data locations.
- [Prompt profiles](prompt-profiles.html) — the tool-description file format, every injection point, placeholders, fallback rules and examples.
- [Skills](skills.html) — `SKILL.md` folders: installing, selecting, on-demand loading.
- [MCP servers](mcp.html) — stdio and Streamable HTTP servers, per-conversation selection, approvals.
- [Hooks](hooks.html) — `hooks.json`, the seven lifecycle events, the stdin/stdout contract, examples.

## Building from source

Prerequisites: Node.js ≥ 22.12, stable Rust, Windows with WebView2, Visual Studio Build Tools (MSVC) and an MSYS2 mingw64 toolchain (`gcc`, `make`, `perl`) for the crates that compile C sources.

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

`tauri:build` builds the frontend, compiles the sidecar to a single executable, and produces the installer under `src-tauri/target/release/bundle/nsis/`. `npm run package:portable` zips the portable flavor from that output. The repository README has the complete list of development commands.
