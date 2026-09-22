# Mework

> **Mework — 喵。开工。**

[English](README.md) | **简体中文**

📖 **文档站：** [catblob-hash.github.io/Mework](https://catblob-hash.github.io/Mework/zh-CN/index.html)

Mework 是一个 Windows 桌面端的编程 Agent。Claude Code 能做的事它基本都能做——读写项目、跑命令、搜网页、开子代理、在浏览器里验收自己的改动——区别在于它是本地应用，模型由你选，而且模型看到的每一句话你都能看见、都能改。

API Key 或订阅由你自己提供，其余一切都在你的机器上运行和保存。

## 你能得到什么

**能把活干完的 Agent。** 带语言服务器代码导航的文件工具、PowerShell 与 Bash（本机、WSL 或 SSH 远程）、网页搜索与抓取、长期记忆、待办清单，以及先出方案再动手的计划模式。日常编程场景下功能与 Claude Code 基本相当，尚有的差距见下文。

**后台子代理与脚本化工作流。** 最多 8 个子代理并行，回合结束后继续跑，应用重启也不丢。可以定义命名角色——用哪个模型、给哪些工具、走哪个搜索后端——让模型按名字调用。更大的任务可以写一段 JavaScript 工作流脚本编排多个代理，带检查点和崩溃恢复。

**会验收的浏览器。** Mework 拉起项目的开发服务器，打开它渲染的页面，让模型读控制台和网络请求、检查元素、点击、输入、截图——然后把结果拿给你看，而不是让你自己去检查。同一个面板也是一个你可以自己操作的普通浏览器。

**看得见的审批。** 每个有风险的调用都会弹卡片，显示完整命令或路径，以及为什么被这样归类。四档安全级别，从逐条批准到完全放行；另有少数确认任何级别都关不掉，比如以你已登录的身份操作网站。

**模型你选，Key 你管。** 支持 OpenAI、Anthropic、Google、Azure OpenAI、Amazon Bedrock、Google Vertex、xAI 以及任何 OpenAI 兼容端点。另有两家登录即用：**OpenAI Codex** 用你的 ChatGPT 订阅，**Claude Agent** 用 Mework 自带的那份 Claude Code，配你本机已有的 Claude Code 登录——不必再单独装一份、也不用操心版本对不上（受 [Claude Code 使用条款](https://code.claude.com/docs/en/legal-and-compliance)约束）。Key 保存在 Windows 凭据管理器，绝不写进配置文件。

**不打扰。** 关窗即缩到托盘，子代理、工作流和 shell 任务照常运行。应用从 GitHub Releases 自动更新。

## 一切由你掌控

多数 Agent 工具替你决定模型看什么，并把对话记录锁死。Mework 两件都不做。

- **技能（Skills）。** `SKILL.md` 目录，与 Claude Code、Codex 和公开技能库同一格式。现有技能放进来就能用；可开局加载，也可按需加载。
- **MCP 服务器。** 任何 stdio 或 Streamable HTTP 服务器，写在与 Claude Code 同形状的 `mcp.json` 里。工具 schema 可以开局声明，也可以按需发现以节省上下文。
- **钩子（Hooks）。** 在七个生命周期节点执行 shell 命令——会话开始、提交提示、每个工具前后、权限请求、停止。可以追加上下文、拦截或改写调用，或让模型继续干活。从 Claude Code 复制过来的 `hooks` 块可直接使用。
- **宿主的每一句话都可以改。** Mework 没有隐藏的系统提示词。它对模型说的所有内容——工具描述、MCP 段落、子代理边界、任务回执——都声明在一个可编辑的文件里，随程序附带中英两套，你自己的覆盖叠在上面。项目级指令写在 `MEWORK.md`；钩子还能每回合额外注入。
- **自由编辑对话。** 在时间轴任意位置右键，即可插入、编辑或删除系统提示词、用户输入、模型回复、思考字段，以及工具调用和它的结果。可从任意消息分支。下一次请求就按你看到的内容构建，所以你可以亲手写下模型的回答或工具的输出，然后从那里继续。
- **不偷偷管理上下文。** Mework 从不在背后压缩或改写历史。用量仪表显示上下文窗口用了多少；删什么、何时删，由你决定。

## 目前还做不到的

- **仅支持 Windows。** 代码保留了跨平台接缝，但目前只发布并支持 Windows 构建。
- **没有自动压缩。** 长会话需要你自己修剪（见上文）。
- **没有 IDE 插件、命令行或斜杠命令。** Mework 是桌面应用；技能承担了斜杠命令的角色。
- **钩子只支持命令类型**，且只有上述七个事件。

## 安装（Windows）

从 [Releases](../../releases) 任选其一：

- **安装器** —— `Mework_1.0.0_x64-setup.exe`（按机器安装）。
- **便携版** —— `Mework_1.0.0_x64_portable.zip`：解压即用，运行 `mework.exe`（`mework-aisdk.exe` 需与其同目录）。

两者都依赖 Microsoft Edge WebView2 Runtime（当前 Windows 10/11 已预装；安装器可在缺失时自动引导安装）。「便携」指的是免安装，不是不落盘：应用仍会写入 `%APPDATA%\com.mework.app`、`%LOCALAPPDATA%\com.mework.app` 与 Windows 凭据管理器。

**首次运行：** 打开「全局设置 → 提供商 → 模型提供商」，添加提供商与 Key（Codex、Claude Agent 两家是登录式），从发现页添加模型，然后在输入框下方选择它。

**更新：** 「设置 → 版本更新」会检查 GitHub Releases。安装版下载新的安装程序并以更新模式运行，保留设置和数据；便携版把新的 zip 下载好，由你解压覆盖旧文件。

## 从源码构建

前置要求：Node.js ≥ 22.12、稳定版 Rust、带 WebView2 的 Windows、Visual Studio Build Tools（MSVC），以及 MSYS2 mingw64 工具链（`gcc`、`make`、`perl`）。

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

安装器产出在 `src-tauri/target/release/bundle/nsis/`；`npm run package:portable` 从同一产物打出便携版。`npm test` 与 `npm run test:rust` 运行检查。

在 Linux 上开发（容器、WSL 或 CI worker）：`bash scripts/setup-linux-dev.sh` 会装好工具链，并补齐 Rust 构建脚本需要、而只有 Windows 天然具备的三件产物。之后上面的检查都可用；只有两个例外：`npm run tauri:build` 面向 WebView2 与 NSIS 打包，`npm run prob:fetch` 钉的是 Windows 版 ProB 制品。

## 许可证

[GNU 通用公共许可证 v3.0 或更高版本](LICENSE)。

随仓库与发布产物一同分发的第三方组件及其条款见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)；编译进发布可执行文件的各依赖库的许可证清单见 [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)。两份文件都随安装包与便携压缩包一起分发。
