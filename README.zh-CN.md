# Mework

> **Mework — 喵。开工。**

[English](README.md) | **简体中文**

📖 **文档站：** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/zh-CN/index.html)

![Mework 功能导览：一个项目同时包含 Mac 本地目录和 SSH 连接的 Linux 电脑；子代理和脚本工作流跑在不同厂商的模型上；计划模式与审批；两台机器上的改动审阅、终端和内置浏览器；文件预览；编辑模型看到的内容；预设、设置和中文界面](.github/assets/demo.gif)

<sub>两分钟功能导览：主 Agent 用 DeepSeek，子代理用 Claude 和 GPT 模型，一个项目同时包含 Mac 本地目录和 SSH 连接的 Linux 电脑。macOS 上录制，等待部分已剪掉，较长的步骤已加速。</sub>

Mework 是一个桌面端编程 Agent，它的特点只有一个：模型看到什么、能做什么，都由你掌控，而且掌控起来很自然。

Claude Code 能做的事它基本都能做——读写项目、跑命令、搜网页、开子代理、在浏览器里验收自己的改动。不同在于方向盘在谁手里：在 Mework 里，你可以直接编辑上下文、任意组合工具、改写宿主注入的每一段内容，而且都在你日常干活的同一个窗口里完成。

把控制权交出来并不难，很多工具都摆出了一排旋钮；难的是让这些旋钮随手就能拧、拧了也不出事。Mework 靠的是一连串刻意的设计取舍：每一处都放弃一点你几乎用不到的能力，换来界面上最顺手的操作恰好也是正确的操作。

API Key 或订阅由你自己提供，其余一切都在你的机器上运行和保存。

## 你能掌控什么

**上下文。** 在时间线任意位置右键，即可插入、编辑或删除系统提示词、用户输入、模型回复、思考字段，以及工具调用和它的结果；可从任意消息分支。下一次请求就按你看到的内容构建，所以你可以亲手写下模型的回答或工具的输出，然后从那里继续。输入框下方的上下文圆环会按来源拆开整个窗口，告诉你 token 花在了哪里。

**工具。** 每个对话自己挑工具：内置工具、MCP 服务器、技能、钩子、联网、两层长期记忆，以及命名的子代理角色——每个角色有自己的模型、工具和搜索后端。挑好的组合可以存成预设，配上开局消息作为模板，再设为某个工作区的默认。

**注入。** Mework 没有隐藏的系统提示词：系统提示词就是时间线上的一张卡，和其他消息一样。宿主对模型说的每一句话——工具描述、环境信息、MCP 段落、子代理边界、任务回执——都是同一份提示词档案里的一个键，程序内置英文版，你自己的覆盖叠在上面。项目级指令写在 `MEWORK.md`；钩子还能每回合额外注入。

## 该有的也都有

- **能把活干完的 Agent。** 带语言服务器代码导航的文件工具；每台机器上可用的 shell——PowerShell、Bash、zsh、sh（本机、WSL 或 SSH 远程），一个项目可以横跨几台机器；网页搜索与抓取、长期记忆，以及先出方案再动手的计划模式。日常编程场景下功能与 Claude Code 基本相当，尚有的差距见下文。
- **后台子代理与脚本化工作流。** 最多 8 个子代理并行，回合结束后继续跑。更大的任务可以写一段 JavaScript 工作流脚本编排多个代理，带检查点，崩溃或应用重启后能接着跑。
- **会验收的浏览器。** Mework 拉起项目的开发服务器，打开它渲染的页面，让模型读控制台和网络请求、检查元素、点击、输入、截图——然后把结果拿给你看，而不是让你自己去检查。同一个面板也是一个你可以自己操作的普通浏览器。
- **看得懂的审批卡。** 每个有风险的调用都会显示完整命令或路径，以及为什么被这样归类；三档安全级别，从逐条批准到完全放行。
- **沿用你已有的格式。** 技能是 `SKILL.md` 目录，与 Claude Code、Codex 和公开技能库同一格式，可开局加载，也可按需加载。MCP 服务器支持 stdio 与 Streamable HTTP，写在与 Claude Code 同形状的 `mcp.json` 里，工具 schema 可以开局声明，也可以按需发现。钩子在七个生命周期节点执行 shell 命令，从 Claude Code 复制过来的 `hooks` 块可直接使用。
- **模型你选，Key 你管。** 支持 OpenAI、Anthropic、Google、Azure OpenAI、Amazon Bedrock、Google Vertex、xAI 以及任何 OpenAI 兼容端点。另有两家登录即用：**OpenAI Codex** 用你的 ChatGPT 订阅，**Claude Agent** 用 Mework 自带的那份 Claude Code，配你本机已有的 Claude Code 登录——不必再单独装一份、也不用操心版本对不上（受 [Claude Code 使用条款](https://code.claude.com/docs/en/legal-and-compliance)约束）。Key 保存在 Windows 凭据管理器；在 Mac 上存进一个由登录钥匙串条目加密的保险库——绝不写进配置文件。
- **不打扰。** 关窗即缩到托盘，子代理、工作流和 shell 任务照常运行。Windows 上应用从 GitHub Releases 自动更新（Mac 上会提示你去下载新版本）；可选的本地小模型在你自己的电脑上给对话起标题、给 shell 命令写一行说明、说清失败的调用为什么失败。

## 安装

全部在 [Releases](../../releases)。

**Windows**（Windows 10 或 11，x64；MSIX 需 2004 及以上版本）任选其一：

- **安装器** —— `Mework_1.0.0_x64-setup.exe`（按机器安装）。
- **便携版** —— `Mework_1.0.0_x64_portable.zip`：解压即用，运行 `mework.exe`（`mework-aisdk.exe` 需与其同目录）。
- **MSIX** —— `Mework_1.0.0_x64.msix`，用 Mework 自己的证书签名：先把 `Mework_msix_signing.cer` 导入「本地计算机 → 受信任人」（只需一次），再打开安装包。同一个包即将上架 Microsoft Store，由商店负责更新。

三者都依赖 Microsoft Edge WebView2 Runtime（当前 Windows 10/11 已预装；安装器可在缺失时自动引导安装）。「便携」指的是免安装，不是不落盘：应用仍会写入 `%APPDATA%\com.mework.app`、`%LOCALAPPDATA%\com.mework.app` 与 Windows 凭据管理器。

**macOS**（13 及以上，Apple 芯片）—— `Mework_1.0.0_aarch64.dmg`，带 Developer ID 签名并经 Apple 公证：打开后把 Mework 拖进「应用程序」。数据在 `~/Library/Application Support/com.mework.app`，Key 在 `~/.mework/credential-vault`，由登录钥匙串里的「Mework Safe Storage」条目加密。

**首次运行：** 打开「全局设置 → 提供商 → 模型提供商」，添加提供商与 Key（Codex、Claude Agent 两家是登录式），从发现页添加模型，然后在输入框下方选择它。

**更新：** 「设置 → 版本更新」会检查 GitHub Releases。安装版下载新的安装程序并以更新模式运行，保留设置和数据；便携版把新的 zip 下载好，由你解压覆盖旧文件。MSIX 版不检查（由 Windows 或商店更新）；Mac 版会带你去发布页下载新的 `.dmg`。

## 从源码构建

前置要求：Node.js ≥ 22.12、稳定版 Rust、带 WebView2 的 Windows、Visual Studio Build Tools（MSVC），以及 MSYS2 mingw64 工具链（`gcc`、`make`、`perl`）。

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

安装器产出在 `src-tauri/target/release/bundle/nsis/`；`npm run package:portable` 从同一产物打出便携版。`npm test` 与 `npm run test:rust` 运行检查。

在 macOS（13 及以上——内嵌 Chromium 的最低要求，Apple 芯片或 Intel）上，前置要求是 Xcode Command Line Tools、nodejs.org 的 Node.js ≥ 22.12 和稳定版 Rust。`bash scripts/setup-macos-dev.sh` 会检查它们并准备好检出目录；之后 `npm test`、`npm run test:rust`、`npm run tauri:dev` 都可用，`npm run tauri:build` 会在 `src-tauri/target/release/bundle/` 下产出 `Mework.app` 和 `.dmg`。产物没有整体签名，其中的可执行文件只带 ad-hoc 签名，在构建它的那台 Mac 上足够用；要分发给别人，需要 Developer ID 签名（`APPLE_SIGNING_IDENTITY`）和公证。侧车与 Chromium 在 hardened runtime 下需要的 entitlements 写在 `src-tauri/Entitlements.plist`，macOS 为 Mework 及其运行的程序弹出隐私权限时显示的说明写在 `src-tauri/Info.plist`（本地化文本在 `src-tauri/InfoPlist/`）。macOS 把钥匙串和隐私权限的授权绑定到代码签名上，而开发构建每次重新编译都会得到新的 ad-hoc 签名；把 `MEWORK_DEV_SIGNING_IDENTITY` 设为登录钥匙串里的一个代码签名身份（Xcode 里的 *Apple Development* 证书，或用“钥匙串访问”创建的自签名代码签名证书），`npm run tauri:dev` / `npm run dev:browser` 就会用它重新签名每次构建，“始终允许”在重新编译后依然有效。Mac 上没有 PowerShell：所有工作区都是 POSIX，shell 工具运行登录 shell `PATH` 里排在最前的 `bash`（即使从 Finder 启动，应用也会采用这份 `PATH`），内置终端可以用登录方式打开本机探测到的 zsh 或 bash，Key 加密保存在 `~/.mework/credential-vault`，钥匙串里只有一项 *Mework Safe Storage* 主密钥，所以 macOS 每个构建最多询问一次钥匙串密码，而不是每个 Key 各问一次。`npm run reset:data` 在 macOS 上同样可用；加 `--keys` 会一并删除这个凭据库和 Mework 的钥匙串项。

在 Linux 上开发（容器、WSL 或 CI worker）：`bash scripts/setup-linux-dev.sh` 会装好工具链，并准备好 Rust 构建脚本需要的前端产物与 AI SDK 侧车。之后上面的检查都可用；只有两个例外：`npm run tauri:build` 面向 WebView2 与 NSIS 打包，`npm run prob:fetch` 钉的是 Windows 版 ProB 制品。

## 许可证

[GNU 通用公共许可证 v3.0 或更高版本](LICENSE)。

随仓库与发布产物一同分发的第三方组件及其条款见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)；编译进发布可执行文件的各依赖库的许可证清单见 [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)。两份文件都随安装包与便携压缩包一起分发。
