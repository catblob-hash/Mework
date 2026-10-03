# Mework

> **Mework — Mew. Work.**

**English** | [简体中文](README.zh-CN.md)

📖 **Documentation:** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/en/index.html)

![Mework: six panes on one task, each maximized in turn; a circle drawn on the page shows the agent where to add a card from a Windows PC's menu; Opus, Sol and Luna review it in parallel; and a new task on DeepSeek streams its thinking](.github/assets/demo.webp)

<sub>One minute, one project across a Mac folder and a Windows PC over SSH. Claude Opus runs the main agent with six panes open. A circle drawn on the page shows it where to add a card from the PC's menu. A workflow sends the review to Opus (Claude), Sol and Luna (GPT) at once. Then a new task on DeepSeek shows its thinking as it streams. Recorded on macOS at 60 fps; the waits are sped up.</sub>

Mework is a desktop coding agent built around one idea: you steer everything the model sees and does, and steering it feels natural.

It does what you would expect from Claude Code — reads and edits your project, runs commands, searches the web, spawns subagents, checks its own work in a browser. What differs is who holds the controls. In Mework you edit the context itself, combine whatever tools you like, and rewrite anything the host injects, all in the same window you work in.

Handing over that much control is the easy part; plenty of tools expose the knobs. The hard part is making them safe to reach for. Mework gets there through a string of deliberate trade-offs, each giving up something you would almost never use so that the obvious move in the interface is also the right one.

You bring your own API key or subscription. Everything else runs and stays on your machine.

## What you control

**The context.** Right-click anywhere in the timeline to insert, edit or delete a system prompt, a user message, a model reply, a reasoning field, or a tool call and its result. Branch from any message. The next request is built from exactly what you see, so you can write the model's answer or a tool's output yourself and carry on from there. The context ring under the composer breaks the window down by where the tokens went.

**The tools.** Each conversation picks its own set: built-in tools, MCP servers, skills, hooks, web access, two tiers of long-term memory, and named subagent roles, each with its own model, tools and search backend. Save a combination as a preset, give it opening messages as a template, and make it a workspace's default.

**The injections.** Mework has no hidden system prompt: the system prompt is a card in your timeline like any other message. Every sentence the host adds — tool descriptions, the environment block, the MCP section, subagent boundaries, task receipts — is a key in one editable prompt profile, with English built in and your own overrides layered on top. Project instructions go in `MEWORK.md`; hooks can inject more on every turn.

## Everything else you'd expect

- **An agent that does the whole job.** Filesystem tools with language-server code navigation; the shells each machine has — PowerShell, Bash, zsh, sh — locally, in WSL or over SSH, with one project free to span several machines; web search and fetch, long-term memory, and a plan mode that proposes before it touches anything. For everyday coding work the feature set is on par with Claude Code; the gaps that remain are listed below.
- **Background subagents and scripted workflows.** Up to eight subagents run at once and keep going after the turn ends. For bigger jobs, a JavaScript workflow script orchestrates many agents with checkpoints, and picks up again after a crash or an app restart.
- **A browser that verifies the work.** Mework starts your project's dev server, opens the page it serves, and lets the model read the console and network, inspect elements, click, type and take screenshots — then show you the result instead of asking you to check. The same pane is a normal browser you can drive yourself.
- **Approval cards you can read.** Every risky call shows the exact command or path and why it was classified that way, under three security levels from approve-everything to full access.
- **The formats you already have.** Skills are `SKILL.md` folders in the format Claude Code, Codex and the public registries use, loaded up front or on demand. MCP servers, stdio or Streamable HTTP, go in an `mcp.json` shaped like Claude Code's, with tool schemas declared up front or discovered on demand. Hooks run shell commands at seven lifecycle points, and a `hooks` block copied from Claude Code works as it is.
- **Your models, your keys.** OpenAI, Anthropic, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI and any OpenAI-compatible endpoint. Two sign in instead: **OpenAI Codex** with your ChatGPT subscription, and **Claude Agent**, which runs the Claude Code build Mework ships against the Claude Code login already on your machine — no separate install to keep in step (subject to the [Claude Code terms](https://code.claude.com/docs/en/legal-and-compliance)). Keys live in Windows Credential Manager, or on a Mac in a vault sealed by one login-keychain item — never in a settings file.
- **Stays out of the way.** Closing the window sends Mework to the tray; subagents, workflows and shell tasks keep running. On Windows the app updates itself from GitHub Releases (on a Mac it points you to the new release), and an optional small model running on your own computer names conversations, captions shell commands and says why a failed call failed.

## Install

Everything is on [Releases](../../releases).

**Windows** (10 or 11, x64; the MSIX needs version 2004 or later) — pick one:

- **Installer** — `Mework_1.0.0_x64-setup.exe` (per-machine).
- **Portable** — `Mework_1.0.0_x64_portable.zip`: unzip anywhere and run `mework.exe` (keep `mework-aisdk.exe` next to it).
- **MSIX** — `Mework_1.0.0_x64.msix`, signed with Mework's own certificate: import `Mework_msix_signing.cer` into *Local Machine → Trusted People* once, then open the package. The same package is coming to the Microsoft Store, which updates it for you.

All three require the Microsoft Edge WebView2 Runtime, which is preinstalled on current Windows 10/11; the installer can bootstrap it if missing. "Portable" means no installer, not no state: the app still writes to `%APPDATA%\com.mework.app`, `%LOCALAPPDATA%\com.mework.app` and Windows Credential Manager.

**macOS** (13 or later, Apple silicon) — `Mework_1.0.0_aarch64.dmg`, signed with a Developer ID and notarized by Apple: open it and drag Mework to Applications. Data lives in `~/Library/Application Support/com.mework.app`, and keys in `~/.mework/credential-vault`, sealed by the *Mework Safe Storage* login-keychain item.

**First run:** open *Settings → Providers → Model providers*, add a provider and its key (or sign in for Codex and Claude Agent), add a model from the discovery page, and pick it under the composer.

**Updates:** *Settings → Updates* checks GitHub Releases. The installer flavor downloads and runs the new setup in update mode, keeping your settings and data; the portable flavor downloads the new zip for you to unzip over the old files. The MSIX package never checks (Windows or the Store updates it), and a Mac is sent to the release page for the new `.dmg`.

## Build from source

Prerequisites: Node.js ≥ 22.12, stable Rust, Windows with WebView2, Visual Studio Build Tools (MSVC), and an MSYS2 mingw64 toolchain (`gcc`, `make`, `perl`).

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

The installer lands under `src-tauri/target/release/bundle/nsis/`; `npm run package:portable` zips the portable flavor from the same output. `npm test` and `npm run test:rust` run the checks.

On macOS (13 or later — the embedded Chromium's floor — Apple silicon or Intel) the prerequisites are the Xcode Command Line Tools, Node.js ≥ 22.12 from nodejs.org, and stable Rust. `bash scripts/setup-macos-dev.sh` checks them and prepares the checkout; afterwards `npm test`, `npm run test:rust` and `npm run tauri:dev` work, and `npm run tauri:build` produces `Mework.app` and a `.dmg` under `src-tauri/target/release/bundle/`. The bundle is not signed as a whole and its executables carry only ad-hoc signatures, which is enough on the Mac that built it; shipping it to others needs a Developer ID signature (`APPLE_SIGNING_IDENTITY`) and notarization. The hardened-runtime entitlements the sidecars and Chromium need are in `src-tauri/Entitlements.plist`, and the privacy-prompt texts macOS shows for Mework and the programs it runs are in `src-tauri/Info.plist` (localized under `src-tauri/InfoPlist/`). macOS ties Keychain and privacy answers to the code signature, and a development build gets a new ad-hoc one on every rebuild; set `MEWORK_DEV_SIGNING_IDENTITY` to a code-signing identity in your login keychain (an *Apple Development* certificate from Xcode, or a self-signed code-signing certificate from Keychain Access) and `npm run tauri:dev` / `npm run dev:browser` re-sign each build with it, so an "Always Allow" keeps working across rebuilds. On a Mac there is no PowerShell: every workspace is POSIX, the shell tool runs the `bash` first on your login shell's `PATH` (the app adopts that `PATH` even when started from Finder), the integrated terminal opens the zsh or bash the machine was found to have, as a login shell, and keys are sealed in `~/.mework/credential-vault` under a single login-keychain item, *Mework Safe Storage*, so macOS asks for the keychain password at most once per build rather than once per key. `npm run reset:data` works on macOS too; with `--keys` it also removes that vault and Mework's keychain items.

Developing on Linux (a container, WSL, or a CI worker): `bash scripts/setup-linux-dev.sh` provisions the toolchain, the frontend bundle and the AI SDK sidecar the Rust build script needs. The checks above work afterwards; `npm run tauri:build`, which bundles for WebView2 and NSIS, and `npm run prob:fetch`, whose pinned ProB artifacts are Windows-only, do not.

## License

[GNU General Public License v3.0 or later](LICENSE).

Redistributed third-party components and their terms are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md); the licenses of the libraries compiled into the release executables are inventoried in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). Both files ship inside the installer and the portable archive.
