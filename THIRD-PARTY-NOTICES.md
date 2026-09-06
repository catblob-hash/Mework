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
`@anthropic-ai/claude-agent-sdk` 0.3.261, declared in
[`aisdk-service/package.json`](aisdk-service/package.json)) is compiled into the
sidecar executable `mework-aisdk.exe` by esbuild. It is the runtime behind the
"Claude Agent (Claude Code)" provider family (`aisdk-service/src/claude-agent.ts`).

The package is not open source: it is © Anthropic, PBC and licensed under
[Anthropic's Commercial Terms of Service](https://www.anthropic.com/legal/commercial-terms)
(see the `LICENSE.md` and `README.md` inside the npm package). Its platform
packages (`@anthropic-ai/claude-agent-sdk-<os>-<arch>`), which contain the Claude
Code executable itself, are **not** redistributed: the provider uses the copy of
Claude Code the user installed on their own machine, and the sidecar never
reads or forwards credentials — the CLI authenticates with its own login. Of the SDK's peer
packages, `@anthropic-ai/sdk` and `@modelcontextprotocol/sdk` are used only for
type declarations and are not bundled; `zod` was already part of the sidecar
through the AI SDK.

---

## Node.js — MIT (and the notices of the components it bundles)

**Bundled executable:** `mework-aisdk.exe` is a copy of the Node.js executable
(`node.exe`) with the sidecar bundle injected as a single-executable application
(`aisdk-service/build.mjs`). The Node.js license file — which carries Node's MIT
license and the notices for V8, OpenSSL, ICU, zlib and the other components
compiled into Node — is reproduced in full in
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) together with the exact Node.js
version the release was built with.
