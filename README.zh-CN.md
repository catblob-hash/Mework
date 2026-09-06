# Mework

> **Mework — 喵。开工。**

[English](README.md) | **简体中文**

📖 **文档站：** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/zh-CN/index.html) —— Mework 的使用方法、工具描述文件（提示词档案）字段参考，以及技能、MCP 服务器与钩子的配置方法。

Mework 是一个本地优先的 Windows 桌面 Agent 工作台。界面用 React 19 构建，宿主是基于 Tauri 2 的 Rust 后端：持久化、能力发现、工具执行和所有安全边界都由宿主负责，内嵌的 AI SDK 侧车是唯一通向模型提供商的网络路径。API Key 由你自己提供——除了你配置的模型调用，任何数据都不会离开本机。

## 亮点

**形式化规约的 Agent 内核。** 回合 / 轮 / 工具 / 子代理的生命周期先写规约、后写实现：CSP-M 交互协议（[`formal/csp/AgentKernel.csp`](formal/csp/AgentKernel.csp)，最高权威）与 TLA+ 安全底线模型（[`formal/tla/AgentKernel.tla`](formal/tla/AgentKernel.tla)）由 ProB 全面检查——模型检查、93 条 CSP 场景精化断言（21 正例 + 72 反例）、守卫删除变异矩阵（TLA 63 + CSP 92）、以及与轻依赖 Rust 内核 crate（`src-tauri/agent-kernel`）的双向轨迹回放。运行时另有影子内核对照真实事件流，任何与规约的分歧都会被上报。

**模型提供商自建。** 支持 OpenAI Responses、OpenAI Chat Completions、Anthropic Messages、Google、Azure OpenAI、Amazon Bedrock、Google Vertex、xAI 以及通用 OpenAI 兼容端点——统一经由编译为单文件可执行的 [Vercel AI SDK](https://sdk.vercel.ai) 侧车。没有内置厂商名单，提供商由用户自己定义；有两个家族不是「地址 + Key」的普通形状。**OpenAI Codex**：用 ChatGPT 订阅（Plus / Pro / Team）登录而不是填 API Key——OAuth 流程跑在宿主进程，令牌加密保存在本机、永不进入界面。**Claude Agent（Claude Code）**：侧车通过官方 [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview) 驱动你本机已安装的 Claude Code 可执行文件，Claude Code 自己的行为全部关掉（没有内置工具、CLAUDE.md、钩子、MCP 服务器、自动压缩与后台任务），Mework 的工具以原名发布给模型——审批、钩子、MCP 服务器与技能照常由 Mework 掌管。不需要填 Key 或地址：Mework 复用本机已有的 Claude Code 登录（`claude auth login`），仅供本人使用，并受 [Claude Code 使用条款](https://code.claude.com/docs/en/legal-and-compliance) 约束。其它提供商的 API Key 存进操作系统凭据库、绝不落入文档，请求拒绝重定向与内嵌凭据。

**27 个内置工具。**
- *文件*（6）：`ls`、`grep`、`read`、`write`、`edit`、`find`——以工作区为边界，阻止路径穿越与符号链接逃逸，原子写入。
- *Shell*（2）：`powershell`、`bash`——前台或后台执行，逐次批准，原生对话框完整展示命令。
- *联网*（3）：`web_search` 与 `web_fetch`（可用提供商原生的服务端检索，或宿主直连 Tavily、Exa、SearXNG、Jina、Firecrawl、博查、智谱等后端），以及 `playwright`——驱动内置浏览器的 23 种操作自动化工具。
- *编排*（10）：`agent_spawn`、`send_message`、`followup_task`、`task_wait`、`task_list`、`workflow`、`skill`、`todo`、`ask_user`、`fork`。
- *记忆*（6）：两层纯 Markdown 长期记忆（全局 `~/.mework` 与项目级 `<workspace>/.mework`），每层独立的读取 / 创建 / 编辑工具。

**子代理与脚本化工作流。** 最多 8 个并发后台子代理，可跨回合、跨应用重启存活；可以给它们发消息、唤醒续跑，用 `task_wait` 阻塞等待结果。`workflow` 工具执行 JavaScript 编排脚本（`agent()`、`parallel()`、`pipeline()`、`phase()`、`budget`），支持增量持久化、崩溃恢复与可选的按步骤 git worktree 隔离。

**真正的内置浏览器。** 每个标签页都是独立、一次性的 WebView2 Profile——不共享 Cookie、不导入任何系统凭据、关闭即销毁。Agent 通过无障碍树快照与可信 CDP 输入驱动页面；自动化操作以可见的指针浮层提示，不向页面注入任何 DOM，远程内容也永远触不到 Tauri IPC。

**运行环境。** Shell 工具可以在本机、WSL 发行版或 SSH 机器上执行，支持按环境注入环境变量；批准指纹绑定执行环境，环境一变即失效。

**可扩展。** 技能（`SKILL.md` 目录，可从本地目录、ZIP 或系统技能位置导入）、生命周期钩子（7 个事件：`SessionStart`、`InstructionsLoaded`、`UserPromptSubmit`、`PreToolUse`、`PermissionRequest`、`PostToolUse`、`Stop`）、MCP 服务器（stdio 与 Streamable HTTP 两种传输）。

**每一段宿主提示词都有声明。** Mework 注入模型请求的全部固定文字——默认系统提示词、能力清单段落、安全边界、子代理附言、任务回执、工具输出的框架文字——集中在一个注册表里：英文默认值编译进程序，中文档案随程序附带。手写一份[工具描述文件](https://catblob-hash.github.io/Mework/zh-CN/prompt-profiles.html)即可按对话覆盖其中任何一条。

**确定性的安全模型。** 分层授权逐次判定：对话级工具白名单、风险分类、完整展示命令与工作区的原生批准对话框、手工高风险执行的一次性 nonce、覆盖所有文件访问的路径守卫、钩子级 deny / 改写。批准一律在执行前消费，绝不事后补票。

**可靠的持久化。** 版本化 JSON 锚点 + SQLite 对话库，流式内容增量落盘、崩溃可恢复；损坏或更高版本的数据会被隔离重建，绝不做静默迁移。

## 安装（Windows）

从 [Releases](../../releases) 任选其一：

- **安装器** —— `Mework_1.0.0_x64-setup.exe`（NSIS，按机器安装）。
- **便携版** —— `Mework_1.0.0_x64_portable.zip`：解压即用，运行 `mework.exe`（`mework-aisdk.exe` 需与其同目录）。

两者都依赖 Microsoft Edge WebView2 Runtime（当前 Windows 10/11 已预装；安装器可在缺失时自动引导安装）。

「便携」指的是免安装，不是不落盘：应用仍会写入 `%APPDATA%\com.mework.app`、`%LOCALAPPDATA%\com.mework.app` 与 Windows 凭据管理器。

首次运行：打开「全局设置 → 提供商 → 模型提供商」，添加提供商与 Key（Codex、Claude Agent 两家是登录式，不填 Key），从发现页添加模型，然后在输入框下方选择它。

「设置 → 版本更新」会显示当前运行版本，并检查 GitHub Releases 是否有新版本。安装版会下载新的 `-setup.exe`，在发布附带 `SHA256SUMS` 时进行校验，并以更新模式运行安装程序（保留设置和数据；安装完成后应用会重新打开）。便携版会把新的 zip 下载到「下载」文件夹并在资源管理器中显示——请关闭 Mework，再解压覆盖旧文件。

## 从源码构建

前置要求：Node.js ≥ 22.12（`.node-version` 固定 22.23.1）、稳定版 Rust、带 WebView2 的 Windows、Visual Studio Build Tools（MSVC）。

构建还需要原生 **mingw64** 工具链（`gcc`、`make`、`perl`），因为部分 crate 会编译 C 源码。安装 [MSYS2](https://www.msys2.org/) 后：

```bash
pacman -S mingw-w64-x86_64-gcc make perl
```

构建包装脚本会在常见安装位置查找 MSYS2；若安装在别处，把 `MSYS2_ROOT` 指向同时包含 `mingw64\bin` 与 `usr\bin` 的目录。`mingw64\bin` 必须排在 `usr\bin` 之前，这一步由包装脚本负责。

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

`tauri:build` 会构建前端、把侧车编译为单文件可执行（Node SEA），并在 `src-tauri/target/release/bundle/nsis/` 产出 NSIS 安装器；裸的 `mework.exe` 与 `mework-aisdk.exe` 位于 `src-tauri/target/release/`。

```bash
npm run package:portable   # 从已有的 tauri:build 产物打出便携版压缩包
```

```bash
npm run licenses:third-party   # 从锁文件重新生成 THIRD-PARTY-LICENSES.md（依赖有变动时在发布前运行）
```

```bash
npm run release:assets   # 复制安装器和便携 zip，并为 GitHub release 写入 SHA256SUMS
```

发布 GitHub release 时，将三个文件附到标签为 `v<version>` 的 release；应用内更新器依赖 `v` 标签前缀、`-setup.exe` / `_portable.zip` 命名模式，以及（可选的）`SHA256SUMS`。

## 开发

```bash
npm run dev            # 纯静态界面预览，无后端
npm run dev:browser    # 全栈浏览器桥（127.0.0.1:1420，真实 Rust 后端、无原生窗口）
npm test               # lint + 前端检查 + vitest
npm run test:rust      # cargo test
npm run verify:formal  # 完整形式化验证管线（先 npm run prob:fetch 装配 ProB）
npm run reset:data     # 清空本机应用数据（schema 升版不做迁移）
```

## 仓库结构

| 路径 | 内容 |
|---|---|
| `src/` | React 19 + TypeScript 界面 |
| `src-tauri/src/` | Rust 宿主：工具执行器、对话库、能力与安全层、内核影子 |
| `src-tauri/agent-kernel/` | 形式化内核的 Rust 投影与一致性轨迹导出 |
| `src-tauri/workflow-core/`、`src-tauri/workflow-script/` | 工作流编排引擎及其 JS 运行时 |
| `formal/` | CSP-M 协议规约与 TLA+ 安全模型 |
| `aisdk-service/` | Node 侧车（AI SDK）：唯一的模型网络路径 |
| `scripts/` | 开发、测试与验证管线 |

## 状态

Mework 1.0.0 面向 Windows。代码中保留了跨平台接缝（凭据库后端、POSIX shell 路径），但目前只发布并支持 Windows 构建。

## 许可证

[GNU 通用公共许可证 v3.0 或更高版本](LICENSE)。

随仓库与发布产物一同分发的第三方组件及其条款见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)；编译进发布可执行文件的各依赖库的许可证清单见 [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)。两份文件都随安装包与便携压缩包一起分发。
