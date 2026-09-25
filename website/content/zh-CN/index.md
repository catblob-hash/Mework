# Mework 文档

Mework 是一款面向 Windows 的本地优先智能体工作台。你自带模型提供商和 API 密钥；应用运行工具、在宿主端维护每一道安全边界，并将一切数据存储在你的计算机上。本网站说明如何在日常工作中使用它、如何通过**工具描述文件（提示词档案）**塑造告知模型的内容，以及如何通过**技能**、**MCP 服务器**和**生命周期钩子**扩展它。

> 源码、发行版和问题跟踪器位于 [github.com/catblob-hash/Mework](https://github.com/catblob-hash/Mework)。本网站由该仓库的 `website/content` 目录构建；欢迎通过拉取请求提交修正。

## Mework 是什么

- 一款带有 React 界面和 Rust 宿主的**桌面应用**（Tauri 2、WebView2）。宿主负责持久化、工具执行、批准和浏览器；界面仅显示状态并发送意图。
- 一个**自带提供商**客户端。OpenAI Responses 和 Chat Completions、Anthropic Messages、Google、Azure OpenAI、Amazon Bedrock、Google Vertex、xAI 以及通用 OpenAI 兼容端点，全部通过一个嵌入式 AI SDK 侧车程序接入。没有 Mework 账户：每把密钥都归你所有，两个内置家族——OpenAI Codex 与 Claude Agent——使用你自己的 ChatGPT 和 Claude Code 登录。
- 一个带有 {{TOOL_COUNT}} 个内置工具的**智能体运行时**：文件系统工具（包括语言服务器代码导航 `lsp`）、shell、联网搜索和抓取、15 个把项目自己的开发服务器跑起来并操作它渲染出的页面的 `preview_*` 工具、子代理、脚本化工作流、待办列表、向你提问的方式、一次分叉到嵌套子对话（fork）、按需加载技能（`skill`）与 MCP 工具 schema（`tool_search`），以及两层纯 Markdown 的长期记忆。其中 {{SWITCHABLE_TOOL_COUNT}} 个是你可以逐对话开关的设置；另外 {{DERIVED_TOOL_COUNT}} 个由宿主自行派生——跟随两个记忆开关、联网开关、技能按需加载与 MCP 工具发现这两个开关、安全层级，以及这次对话能否产生任务。每个工具都在[内置工具](tools.html)下有自己的页面。
- 一块**内置浏览器**面板，同时是两个表面：一个你自己驱动的真浏览器——地址栏、前进 / 后退 / 刷新、开发服务器选择器、日志抽屉——也是模型按工作区的 `.mework/launch.json` 启动服务器、并在上面验收自己改动的那个页面。
- **在安全方面具有确定性。**每次工具调用均由宿主依据对话的安全层级进行分类；批准会在执行前被消费；文件访问被限制在工作区内以及你授权给对话的目录中；浏览器档案为单次使用且相互隔离。

## 安装

从 [Releases](https://github.com/catblob-hash/Mework/releases) 页面下载任一版本：

| 版本 | 文件 | 说明 |
|---|---|---|
| 安装版 | `Mework_<version>_x64-setup.exe` | NSIS，按计算机安装。若缺少 WebView2 运行时，会自动引导安装。 |
| 便携版 | `Mework_<version>_x64_portable.zip` | 解压到任意位置并运行 `mework.exe`。请将 `mework-aisdk.exe` 保留在其旁边——它是通往模型提供商的唯一途径。 |

“便携”意为没有安装程序，并不表示没有状态。两种版本均写入相同的位置：

| 位置 | 内容 |
|---|---|
| `%APPDATA%\com.mework.app` | 设置文档（`document.v1.json`）、对话数据库、图像附件、工作流运行日志 |
| `%LOCALAPPDATA%\com.mework.app` | WebView2 数据和单次使用的浏览器档案 |
| Windows 凭据管理器 | API 密钥（绝不会写入文档） |
| `~/.mework` | 你手动创建的内容：长期记忆、`hooks.json`、`skills/`、`mcp.json`、`lsp.json`、`tool-descriptions/*.json` |

## 首次运行

1. 打开**设置 → 提供商 → 模型提供商**，添加一个提供商并粘贴其 API 密钥（Codex 与 Claude Agent 两家是登录式）。点击**拉取模型**打开**发现模型**抽屉并添加所需模型，或手动添加模型 ID；出现在列表里的模型即可使用。
2. 回到工作区视图，添加一个指向文件夹的工作区（侧边栏中的**添加工作区**，再**浏览**选择文件夹），或者从临时工作区开始。
3. 在输入框下方选择模型，输入任务并发送。首个需要权限的工具调用会显示一张批准卡，写明工具名称、调用将作用的对象、其风险级别以及如此归类的理由；可以批准一次，或选择**总是允许**在该对话中记住这个工具——强制确认的调用、shell 调用以及在页面上操作的 `preview_*` 工具绝不会提供这一选项。

新对话从默认预设打开。开箱即为 **Claude Code**——随附的两个预设之一（另一个是 **Codex**）：除 `preview_*` 工具和 `workflow` 外的每个可开关工具全部开启，联网、技能按需加载与 MCP 工具发现开启，两层记忆均关闭，安全层级为**手动**（`request_approval`），工具描述来自内置英文提示词档案。以上全部都是你可以更改的对话设置，也可以保存并复用为一个**预设**。

## 一个回合如何运作

理解这个循环会让其余设置一目了然：

1. 你发送一条消息。宿主会从**已持久化的**对话重建请求——渲染器无法注入宿主未存储的工具、提示词或历史记录。
2. 系统提示词先组装宿主段：环境段（工作目录、Git 与工作树信息、额外目录、平台、操作系统版本、日期）、已选择的技能、已选择的 MCP 服务器和钩子，以及可选的应用数据目录。你的对话提示词跟在其后——Mework 自身没有默认对话提示词，留空就只是让宿主段独立存在。之后是可能随步骤而变的各段：计划模式下的计划模式段、开启联网搜索时的档案 `system.web_safety` 槽位——内置档案把它留空，填入它正是告诉模型网页文本是证据而非指令的方式——以及启用 `preview_start` 且工作区 `launch.json` 的 `autoVerify` 保持开启时的 `<preview_tools>` 验收段。
3. 模型以文本和／或工具调用作答。每次调用都会被分类（**读取／写入／无边界**、在工作区内或外、是否必须确认），依次经过 `PreToolUse` 以及——仅对本来会显示批准卡的调用——`PermissionRequest`，按需获得批准、执行，再经过 `PostToolUse`。结果会在下一轮返回给模型。
4. 后台工作——子代理、工作流运行、后台 shell 命令、`preview_start` 拉起的开发服务器——会在回合结束后继续运行。它们的结果会在下一轮边界以 `<task-notification>` 形式交付，或在模型调用 `task_wait` 时交付。
5. 当模型停止调用工具且没有 `Stop` 钩子把它送回工作、某个钩子以 `continue: false` 叫停运行、模型通过 `ask_user` 向你提问，或你将其停止时，该回合结束。

宿主在第 2–4 步添加的每个固定句子均在提示词档案中声明，因此你可以准确查看模型被告知了什么——参见[提示词档案](prompt-profiles.html)。

## 下一步去哪里

- [使用 Mework](working.html)——工作区、对话、预设、工具和批准、运行环境、子代理和工作流、联网搜索、浏览器、记忆、图像和数据位置。
- [内置工具](tools.html)——每个工具都有自己的页面：它做什么、什么时候请求批准、模型看到的参数，以及交给模型的描述。
- [提示词档案](prompt-profiles.html)——工具描述文件格式、每个注入点、占位符、回退规则和示例。
- [技能](skills.html)——`SKILL.md` 文件夹：位置、选择、按需加载。
- [MCP 服务器](mcp.html)——stdio 和 Streamable HTTP 服务器、按对话选择、批准。
- [代码语义导航](lsp.html)——`lsp` 工具、它的九个操作、`lsp.json` 与内置服务器预设。
- [钩子](hooks.html)——`hooks.json`、七个生命周期事件、stdin/stdout 协议、示例。

## 从源码构建

前提条件：Node.js ≥ 22.12、稳定版 Rust、带 WebView2 的 Windows、Visual Studio Build Tools（MSVC），以及供编译 C 源码的 crate 使用的 MSYS2 mingw64 工具链（`gcc`、`make`、`perl`）。

```bash
npm install
npm --prefix aisdk-service install
npm run tauri:build
```

`tauri:build` 会构建前端，将侧车程序编译为单个可执行文件，并在 `src-tauri/target/release/bundle/nsis/` 下生成安装程序。`npm run package:portable` 会根据该输出打包便携版为 zip。仓库 README 包含完整的开发命令列表。
