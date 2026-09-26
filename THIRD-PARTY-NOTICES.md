# Third-party notices

Mework itself is distributed under GPL-3.0-or-later (see [`LICENSE`](LICENSE)).
The components below are redistributed inside this repository and in the release
artifacts under their own terms, which this notice preserves.

Library dependencies are not vendored in this repository, but the release
executables do contain them: `mework.exe` statically links its Rust crates, and
`mework-aisdk.exe` is a Node.js executable with the sidecar's npm dependency
bundle injected. Their licenses and attributions — including the Node.js runtime's
own license file — are inventoried in
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md), which is generated from the
lockfiles by `npm run licenses:third-party` and ships inside both release
artifacts next to this file.

---

## Chromium Embedded Framework and Chromium — BSD-3-Clause and others (macOS bundles)

**Redistributed in:** `Mework.app/Contents/Frameworks/Chromium Embedded Framework.framework`

On macOS the built-in browser's pages run in the
[Chromium Embedded Framework](https://github.com/chromiumembedded/cef) (CEF), the
binary distribution published at <https://cef-builds.spotifycdn.com>, copied into the
bundle unmodified by [`scripts/stage-macos-cef.mjs`](scripts/stage-macos-cef.mjs).
CEF is BSD-3-Clause (Copyright (c) 2012 Marshall A. Greenblatt, as its headers state);
the notice ships in the bundle as `Contents/Resources/CEF-LICENSE.txt`. The Chromium it
contains carries the licenses of Chromium and its third-party components, all listed in
the distribution's `CREDITS.html`, which ships next to this file in the bundle as
`Contents/Resources/CHROMIUM-CREDITS.html`.

---

## `tauri-apps/tauri` (`tauri-runtime-cef`) — Apache-2.0 OR MIT

**Adapted file:**
[`src-tauri/src/cef_host/pump.rs`](src-tauri/src/cef_host/pump.rs)

The CEF external message pump is adapted from
`crates/tauri-runtime-cef/src/external_message_pump/{mod,macos}.rs` in
[tauri](https://github.com/tauri-apps/tauri), as published in `tauri-runtime-cef`
3.0.0-alpha.2 — itself a port of cefclient's `main_message_loop_external_pump`.
Copyright 2019-2024 Tauri Programme within The Commons Conservancy; used here under
the MIT option. The scheduling and reentrancy logic is unchanged; the module was
folded into one file and given a stop switch for shutdown.

---

## `ggml-org/llama.cpp` — MIT (Windows and Linux builds)

**Linked into:** `mework.exe` / `mework` on Windows and Linux, through the
[`llama-cpp-sys-2`](https://crates.io/crates/llama-cpp-sys-2) crate, which compiles
the llama.cpp and ggml sources it bundles. It runs the optional local helper model
(conversation titles, shell command explanations); macOS builds use Core ML or MLX
instead and do not contain it.

llama.cpp is MIT: Copyright (c) 2023-2026 The ggml authors. The MIT text is in
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md).

The model (Qwen3.5-0.8B, Apache-2.0) is not in the app. When the user turns the
feature on and picks a build, the app downloads that build, converted from the official
release (the GGUF file on Windows and Linux, the Core ML package or the MLX weights on
a Mac), from [`catblob-hash/Mework-Qwen3.5-0.8B`](https://huggingface.co/catblob-hash/Mework-Qwen3.5-0.8B)
on Hugging Face, where it is published with its Apache-2.0 license.

---

## `ml-explore/mlx` — MIT (macOS builds on Apple silicon)

**Bundled:** `Contents/Frameworks/mework-mlx/libmlx.dylib`, Apple's prebuilt MLX
0.32.2 as published on PyPI (`mlx-metal`), which
`Contents/Frameworks/mework-mlx/libmework_mlx.dylib` (Mework's model code,
[`src-tauri/local-model/mlx`](src-tauri/local-model/mlx)) links against. The MLX
kernel library (`mlx.metallib`, from the same release) is not in the app; it comes with
the local helper model's GPU build when the user downloads that build.

```
MIT License

Copyright © 2023 Apple Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## `anthropic-experimental/sandbox-runtime` — Apache-2.0

**Adapted file:**
[`src-tauri/remote-agent/src/agent/sandbox/seatbelt.rs`](src-tauri/remote-agent/src/agent/sandbox/seatbelt.rs)

The macOS Seatbelt profile the agent's sandbox generates follows the one
[sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime) generates
for Claude Code (v0.0.77, commit `ddbeb74`), itself based on Chromium's sandbox policy:
its lists of allowed Mach services, sysctls, IOKit classes and device ioctls are taken
from it, as are the path-to-rule conventions (subpaths for directories, anchored
regular expressions for names protected at any depth, write denials on the ancestors of
protected paths). Copyright Anthropic, PBC, licensed under the Apache License 2.0. The
profile was rewritten rather than copied: the keychain services are left out, `/dev` is
closed but for a short list, pseudo terminals are not allowed, and the protected names,
the bare-repository rule and the hard-link denials are Mework's. The Linux side
(bubblewrap arguments, the seccomp filter) follows the same project's approach without
taking its code.

---

## `obra/superpowers` — MIT

**Vendored files:**
[`src-tauri/resources/builtin-skills/systematic-debugging/SKILL.md`](src-tauri/resources/builtin-skills/systematic-debugging/SKILL.md),
[`src-tauri/resources/builtin-skills/verification-before-completion/SKILL.md`](src-tauri/resources/builtin-skills/verification-before-completion/SKILL.md)

Adapted from `skills/systematic-debugging/SKILL.md` and
`skills/verification-before-completion/SKILL.md` in the
[superpowers](https://github.com/obra/superpowers) repository, at commit
`b36e0829c6d0140e93cfef2ca599b1b07d4a7797` (2026-09-17). The bodies are verbatim
apart from the removals recorded in
[`src-tauri/resources/builtin-skills/README.md`](src-tauri/resources/builtin-skills/README.md);
the frontmatter is rewritten into the flat `key: value` form this host parses.

```
MIT License

Copyright (c) 2025 Jesse Vincent

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## `addyosmani/agent-skills` — MIT

**Vendored files:**
[`src-tauri/resources/builtin-skills/code-review-and-quality/SKILL.md`](src-tauri/resources/builtin-skills/code-review-and-quality/SKILL.md),
[`src-tauri/resources/builtin-skills/git-workflow-and-versioning/SKILL.md`](src-tauri/resources/builtin-skills/git-workflow-and-versioning/SKILL.md)

Adapted from `skills/code-review-and-quality/SKILL.md` and
`skills/git-workflow-and-versioning/SKILL.md` in the
[agent-skills](https://github.com/addyosmani/agent-skills) repository, at commit
`be4e44a9` (2026-09-17), with the same treatment as above.

```
MIT License

Copyright (c) 2025 Addy Osmani

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## `mattpocock/skills` — MIT

**Vendored file:**
[`src-tauri/resources/builtin-skills/resolving-merge-conflicts/SKILL.md`](src-tauri/resources/builtin-skills/resolving-merge-conflicts/SKILL.md)

Adapted from `skills/engineering/resolving-merge-conflicts/SKILL.md` in the
[skills](https://github.com/mattpocock/skills) repository, at commit
`959a8e9f` (2026-09-17), with the same treatment as above.

```
MIT License

Copyright (c) 2026 Matt Pocock

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## `@cherrystudio/provider-registry` — MIT

**Vendored file:** [`src-tauri/resources/model-registry/models.json`](src-tauri/resources/model-registry/models.json)

Copied verbatim from `packages/provider-registry/data/models.json` in the
[Cherry Studio](https://github.com/CherryHQ/cherry-studio) repository, at commit
`ab6dae4b1c88c677e9eb6f2501f9ff39ff3a790f` (2026-08-22), package version
`0.0.1-alpha.1`. See
[`src-tauri/resources/model-registry/README.md`](src-tauri/resources/model-registry/README.md)
for what the file is used for and how to resynchronize it.

The Cherry Studio repository as a whole is AGPL-3.0. Only the
`packages/provider-registry/` subpackage is MIT-licensed, and only data from that
subpackage is vendored here.

The subpackage declares `"license": "MIT"` and `"author": "Cherry Studio"` in its
`package.json` and ships no separate `LICENSE` file, so the copyright line below
names that declared author.

```
MIT License

Copyright (c) Cherry Studio (CherryHQ)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## `@anthropic-ai/claude-agent-sdk` — Anthropic Commercial Terms of Service

**Bundled file:** the SDK's JavaScript (`sdk.mjs` of
`@anthropic-ai/claude-agent-sdk` 0.3.282, declared in
[`aisdk-service/package.json`](aisdk-service/package.json)) is compiled into the
sidecar executable `mework-aisdk.exe` by esbuild. It is the runtime behind the
"Claude Agent (Claude Code)" provider family (`aisdk-service/src/claude-agent.ts`).

**Distributed file:** Mework also distributes the Claude Code executable from the
platform package `@anthropic-ai/claude-agent-sdk-win32-x64` 0.3.282 as
`claude.exe` beside the main application executable — a file of its own, not
bundled into either Mework binary. That executable is Claude Code 2.1.282 and is
staged by `src-tauri/build.rs` from the pinned platform package.

The package is not open source: it is © Anthropic, PBC and licensed under
[Anthropic's Commercial Terms of Service](https://www.anthropic.com/legal/commercial-terms)
(see the `LICENSE.md` and `README.md` inside the npm package). The distributed
platform package's `LICENSE.md` states, verbatim:

```
© Anthropic PBC. All rights reserved. Use is subject to the Legal Agreements outlined here: https://code.claude.com/docs/en/legal-and-compliance.
```

The sidecar never reads or forwards credentials — the CLI uses the user's own
`claude auth login` authentication. Distribution of this executable is subject
to the Anthropic Legal Agreements above; this inventory itself does not
constitute permission to redistribute it, consistent with the warning that this
inventory does not itself establish permission to combine or redistribute
commercial components. Of the SDK's peer packages, `@anthropic-ai/sdk` and
`@modelcontextprotocol/sdk` are used only for type declarations and are not
bundled; `zod` was already part of the sidecar through the AI SDK.

---

## Node.js — MIT (and the notices of the components it bundles)

**Bundled executable:** `mework-aisdk.exe` is a copy of the Node.js executable
(`node.exe`) with the sidecar bundle injected as a single-executable application
(`aisdk-service/build.mjs`). The Node.js license file — which carries Node's MIT
license and the notices for V8, OpenSSL, ICU, zlib and the other components
compiled into Node — is reproduced in full in
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) together with the exact Node.js
version the release was built with.
