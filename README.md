# Mework

> **Mework — Mew. Work.**

**English** | [简体中文](README.zh-CN.md)

📖 **Documentation:** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/en/index.html)

Mework is a desktop coding agent for Windows. It does what you would expect from Claude Code — reads and edits your project, runs commands, searches the web, spawns subagents, checks its own work in a browser — but as a local app where you choose the model, and where nothing the model is told is hidden from you or off limits to edit.

You bring your own API key or subscription. Everything else runs and stays on your machine.

## What you get

**An agent that does the whole job.** Filesystem tools with language-server code navigation, PowerShell and Bash (locally, in WSL, or over SSH), web search and fetch, long-term memory, a to-do list, and a plan mode that makes the model propose before it touches anything. For everyday coding work the feature set is on par with Claude Code; the differences that remain are listed below.

**Background subagents and scripted workflows.** Up to eight subagents run at once, keep going after the turn ends, and survive an app restart. You can define named roles — which model, which tools, which search backend — and let the model pick one. For bigger jobs, a JavaScript workflow script orchestrates many agents with checkpoints and crash recovery.

**A browser that verifies the work.** Mework starts your project's dev server, opens the page it serves, and lets the model read the console and network, inspect elements, click, type and take screenshots — then show you the result instead of asking you to check. The same pane is a normal browser you can drive yourself.

**Approvals you can see.** Every risky call shows a card with the exact command or path and why it was classified that way. Four security levels, from approve-everything to full access, and a few confirmations that no level can turn off — like acting on a site you logged into yourself.

**Your models, your keys.** OpenAI, Anthropic, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI and any OpenAI-compatible endpoint. Two sign in instead: **OpenAI Codex** with your ChatGPT subscription, and **Claude Agent**, which reuses the Claude Code login already on your machine (subject to the [Claude Code terms](https://code.claude.com/docs/en/legal-and-compliance)). Keys live in Windows Credential Manager, never in a settings file.

**Stays out of the way.** Closing the window sends Mework to the tray; subagents, workflows and shell tasks keep running. The app updates itself from GitHub Releases.

## You are in control

Most agent tools decide what the model is told and lock the transcript. Mework does neither.

- **Skills.** `SKILL.md` folders in the same format Claude Code, Codex and the public registries use. Drop an existing skill in and it works; load skills up front or on demand.
- **MCP servers.** Any stdio or Streamable HTTP server, declared in an `mcp.json` with the same shape as Claude Code's. Tool schemas can be declared up front or discovered on demand to save context.
- **Hooks.** Shell commands at seven lifecycle points — session start, prompt submit, before and after each tool, permission requests, stop. They can add context, block or rewrite a call, or send the model back to work. A `hooks` block copied from Claude Code works as it is.
- **Every host sentence is yours to change.** Mework has no hidden system prompt. Everything it says to the model — tool descriptions, the MCP section, subagent boundaries, task receipts — is declared in one editable file, with English and Chinese profiles shipped and your own overrides layered on top. Project instructions go in `MEWORK.md`; hooks can inject more per turn.
- **Edit the conversation freely.** Right-click anywhere in the timeline to insert, edit or delete a system prompt, a user message, a model reply, a reasoning field, or a tool call and its result. Branch from any message. The next request is built from what you see, so you can write the model's answer or a tool's output yourself and continue from there.
- **No silent context management.** Mework never compacts or rewrites the history behind your back. A usage meter shows how much of the context window is used; what to trim and when is your decision.

## What it does not do yet

- **Windows only.** The code has cross-platform seams, but only the Windows build is released and supported.
- **No automatic compaction.** Long sessions are your responsibility to trim (see above).
- **No IDE plugin, CLI or slash commands.** Mework is a desktop app; skills take the place of slash commands.
- **Hooks are command hooks only**, and only the seven events above.

## Install (Windows)

Grab either flavor from [Releases](../../releases):

- **Installer** — `Mework_1.0.0_x64-setup.exe` (per-machine).
- **Portable** — `Mework_1.0.0_x64_portable.zip`: unzip anywhere and run `mework.exe` (keep `mework-aisdk.exe` next to it).

Both require the Microsoft Edge WebView2 Runtime, which is preinstalled on current Windows 10/11; the installer can bootstrap it if missing. "Portable" means no installer, not no state: the app still writes to `%APPDATA%\com.mework.app`, `%LOCALAPPDATA%\com.mework.app` and Windows Credential Manager.

**First run:** open *Settings → Providers → Model providers*, add a provider and its key (or sign in for Codex and Claude Agent), add a model from the discovery page, and pick it under the composer.

**Updates:** *Settings → Updates* checks GitHub Releases. The installer flavor downloads and runs the new setup in update mode, keeping your settings and data; the portable flavor downloads the new zip for you to unzip over the old files.

## Build from source

Prerequisites: Node.js ≥ 22.12, stable Rust, Windows with WebView2, Visual Studio Build Tools (MSVC), and an MSYS2 mingw64 toolchain (`gcc`, `make`, `perl`).

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

The installer lands under `src-tauri/target/release/bundle/nsis/`; `npm run package:portable` zips the portable flavor from the same output. `npm test` and `npm run test:rust` run the checks.

## License

[GNU General Public License v3.0 or later](LICENSE).

Redistributed third-party components and their terms are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md); the licenses of the libraries compiled into the release executables are inventoried in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). Both files ship inside the installer and the portable archive.
