# Code navigation

The `lsp` tool lets the model ask about **symbols** instead of text: where a name is defined, who references it, what its type is, who calls this function. Mework runs language servers for that — ordinary [LSP](https://microsoft.github.io/language-server-protocol/) processes, the same ones your editor uses — and speaks the protocol to them.

Nine operations, one tool:

| Operation | What it answers |
|---|---|
| `goToDefinition` | Where the symbol under the cursor is defined |
| `findReferences` | Every use of it, grouped by file |
| `hover` | Its documentation and type |
| `documentSymbol` | Every symbol in one file, nested |
| `workspaceSymbol` | Symbols matching a query across the project |
| `goToImplementation` | Implementations of an interface or abstract method |
| `prepareCallHierarchy` | The call-hierarchy item at a position |
| `incomingCalls` | Who calls this function |
| `outgoingCalls` | What this function calls |

Every operation takes `filePath`, `line` and `character`. Line and character are **1-based**, the numbers your editor shows. `workspaceSymbol` also takes `query`. The [`lsp` tool page](tools/lsp.html) has the parameters as the model sees them.

`lsp` is one of the switches in a conversation's **Enabled tools** list, under the name **Code navigation**. The built-in **mework** preset has it on.

An ordinary call is classified as a **read** of `filePath`: inside the workspace it passes at every [security level](working.html#tools-and-approvals), and outside it asks at Manual. If the project you are working in ships its own `.mework/lsp.json`, every call is classified as an **unbounded action** instead and asks at every level below Full access — that file gets to name the command Mework launches, and a repository you cloned is not the same thing as a choice you made. Only its existence is checked, never its contents. Nothing else moves the classification: a `~/.mework/lsp.json` of your own and the built-in table leave it where it was, and which server actually answers the call is not consulted either. **Always allow** remembers the answer for `lsp` in that conversation.

## Servers you already have

Mework ships a small table of well-known servers and offers each one **only when its command is already on your `PATH`**:

| Name | Command | Files |
|---|---|---|
| `rust-analyzer` | `rust-analyzer` | `.rs` |
| `typescript-language-server` | `typescript-language-server --stdio` | `.ts` `.tsx` `.mts` `.cts` `.js` `.jsx` `.mjs` `.cjs` |
| `pyright` | `pyright-langserver --stdio` | `.py` `.pyi` |
| `gopls` | `gopls` | `.go` |
| `clangd` | `clangd` | `.c` `.h` `.cpp` `.cc` `.cxx` `.hpp` `.hxx` `.hh` |
| `lua-language-server` | `lua-language-server` | `.lua` |
| `bash-language-server` | `bash-language-server start` | `.sh` `.bash` |

Install the one you want the ordinary way — `rustup component add rust-analyzer`, `npm i -g pyright`, `go install golang.org/x/tools/gopls@latest` — and it is offered from the next scan. A preset whose command is missing is not offered at all, so a file it would have claimed falls through to whatever else claims that extension — and to "No LSP server available" if nothing does.

## Declaring your own

Anything else — a language not in the table, a different server for a language that is, flags your project needs — goes in an `lsp.json`, in the same two places skills, MCP servers and hooks live:

```text
~/.mework/lsp.json                 user scope — every workspace
<workspace>/.mework/lsp.json       workspace scope — that workspace only
```

A top-level `lspServers` object keyed by server name:

```json
{
  "lspServers": {
    "rust-analyzer": {
      "command": "rust-analyzer",
      "extensionToLanguage": { ".rs": "rust" },
      "initializationOptions": { "cargo": { "allFeatures": true } },
      "settings": { "rust-analyzer": { "checkOnSave": false } }
    },
    "zls": {
      "command": "zls",
      "extensionToLanguage": { ".zig": "zig" }
    }
  }
}
```

### Accepted keys

| Key | Meaning |
|---|---|
| `command` | **Required.** The program to launch. It is executed as-is, not through a shell, so give an executable name or a full path. A bare name containing a space is refused — put arguments in `args`. |
| `extensionToLanguage` | **Required.** Maps a file extension to an LSP language id. This one map decides both which files this server handles and the `languageId` sent when a file is opened. Keys are normalized, so `"rs"`, `".RS"` and `".rs"` mean the same thing. |
| `args` | Command-line arguments. Many servers need `--stdio` here. |
| `env` | Extra environment variables for the process. |
| `initializationOptions` | Passed verbatim as `initialize.initializationOptions`. |
| `settings` | Answered to the server's `workspace/configuration` requests and pushed once as `workspace/didChangeConfiguration`. |
| `workspaceFolder` | Root to start the server in. Empty means the conversation's workspace. |
| `startupTimeout` | Milliseconds allowed for `initialize`. Default 30000. |
| `shutdownTimeout` | Milliseconds allowed for a graceful `shutdown`. Default 5000. |
| `restartOnCrash`, `maxRestarts` | Whether to restart a server that dies, and how many times. Defaults `true` and `3`. |
| `diagnostics` | Set `false` to keep navigation but stop this server's problems from being reported to the model. Default `true`. |
| `description` | Free text about the entry, kept to 240 characters. |
| `${VAR}` / `${VAR:-default}` | Expanded in `command`, `args`, `env` values and `workspaceFolder`. |

A server name may contain only letters, numbers, `-` and `_`, and may not be `__proto__`, `constructor` or `prototype`. `transport` is accepted but only `"stdio"` works; `"socket"` makes the entry unusable rather than appearing to work over a transport that is not wired. An entry with no `extensionToLanguage`, a `${VAR}` that is unset with no default, or a spaced bare `command` is refused the same way. A refusal is per entry — the rest of the file still counts. A file that is not valid JSON, or whose top level is not `{ "lspServers": { … } }`, is skipped whole.

## Which server answers a file

The first entry that claims the extension wins, looked up in this order:

1. this workspace's `.mework/lsp.json`
2. `~/.mework/lsp.json`
3. the built-in presets

Your project's file beats your global one; both beat the presets. An entry with the **same name** as a preset replaces it rather than competing with it — that is how you give `rust-analyzer` extra flags without ending up with two of them.

There is no per-conversation selection of servers, the way skills, MCP servers and hooks have one. Which server answers is decided by the file extension, so a checkbox would not express anything.

## Servers start when they are needed

A language server is launched the first time something asks about a file it claims, and then stays up. Every conversation in a workspace shares one — a cold `rust-analyzer` spends real time indexing, and that cost is paid once per project, not once per conversation. They are stopped when the app exits.

The first navigation call in a project can therefore be slow, and can legitimately come back empty while the server is still indexing. Asking again a moment later is the right move; the server is warm from then on.

## Problems after an edit

When the model writes or edits a file that a **running** language server holds, Mework tells the server it changed. Whatever problems the server reports arrive at the start of the model's next step, as a block it reads:

```text
<new-diagnostics>The following new diagnostic issues were detected:

main.rs:
  Error [Line 10:5] cannot find value `x` [E0425] (rustc)</new-diagnostics>
```

A problem is reported once, not every step. One block carries at most 10 problems per file and 30 in total, so a mass rename does not bury the conversation. Nothing is injected into a conversation that cannot edit files, and `"diagnostics": false` on an entry turns this off for that server while leaving navigation working.

This is not a substitute for building or testing: it is whatever the language server already knew, delivered without waiting for it.

## Verifying a server end to end

1. Check the command runs in a terminal (`rust-analyzer --version`, `gopls version`). It has to be on the `PATH` Mework itself inherited at launch, so restart the app if installing it changed your `PATH`.
2. Switch **Code navigation** on and ask something that needs it — "where is `foo` defined?" — in a project of that language. The first call may take a while; ask again if it comes back empty.
3. If the call answers "No LSP server available for file type", nothing claims that extension: check the entry's `extensionToLanguage`, and that the file is `lsp.json` with a top-level `lspServers` key.

## Troubleshooting

| Symptom | Cause |
|---|---|
| "No LSP server available for file type: .x" | Nothing claims that extension. Add an entry to `lsp.json`, or install one of the preset servers. |
| A preset for the language is never used | Its command is not installed, or not on the `PATH` the app inherited. Install it, then restart Mework so it picks up a changed `PATH`. |
| An `lsp.json` entry is never used | It was refused: a `${VAR}` in `command`, `args`, `env` or `workspaceFolder` with no value and no `:-default`; an `extensionToLanguage` that maps nothing; a bare `command` with a space in it; or `"transport": "socket"`. |
| No entry in one file is used | The file is not valid JSON, or its top-level key is not `lspServers` — `languageServers`, from another client, is the usual mistake. |
| Results are empty right after starting | The server is still indexing. Ask again. |
| "The language server did not answer … within 30 seconds" | The server is wedged or the project is very large. Navigation requests have a fixed 30-second deadline; `startupTimeout` does not move it. Check the server's own logs. |
| "The language server '…' failed to initialize" | The handshake did not complete: the process died at startup, or `initialize` did not answer within `startupTimeout`. Raise `startupTimeout` for a server that is merely slow. |
| "its stdout is desynchronized from the base protocol" | The server is writing logs to stdout instead of stderr. Most have a flag for that — check its documentation. |
| Definitions in `target/` or `node_modules/` never show up | Results in git-ignored files are dropped on purpose: they crowd out the answer you wanted. |
| Nothing at all, and the file is huge | Files over 10 MB are not sent to a language server. |
