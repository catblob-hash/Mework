# 内置工具

Mework 内置 {{TOOL_COUNT}} 个工具。每个工具都有自己的页面：它替你做什么、什么时候需要审批、模型看到的参数，以及内置提示词档案交给模型的描述。工具名就是模型调用时用的名字，也是[工具描述文件](prompt-profiles.html)里引用的名字。

下面按工具选择器的分组排列。每个可开关的工具在选择器里各占一行，行首的书本图标就通向它在这里的页面。

## 工具怎样变得可用

其中 {{SWITCHABLE_TOOL_COUNT}} 个是对话或预设的「启用的工具」列表里的开关（composer 旁边的滑块图标，或 设置 → 对话预设）。另外 {{DERIVED_TOOL_COUNT}} 个从别的设置或工具**派生**，从不出现在那个列表里：

| 派生工具 | 跟随 |
|---|---|
| `read_global_memory`、`create_global_memory`、`edit_global_memory` | **全局记忆**开关 |
| `read_project_memory`、`create_project_memory`、`edit_project_memory` | **项目记忆**开关 |
| `web_search`、`web_fetch` | 对话唯一的**启用联网搜索**开关；一次运行拿到其中哪一个，取决于解析出的后端 |
| `task_wait`、`task_list`、`box` | 任何会产生任务的工具被启用（`agent_spawn`、`workflow`、`bash`、`zsh`、`sh`、`powershell`、`preview_start`） |
| `skill` | **技能按需加载**开关，且至少选中了一个技能 |
| `tool_search` | **工具发现**开关（MCP），且本次运行扣留了 MCP 工具 schema |
| `plan`、`exit_plan_mode` | **安全层级**处于计划模式 |
| `preview_start`、`preview_stop`、`preview_list` | 任何其他 `preview_*` 工具被启用 |

一个对话里的工具暴露面只增不减。一次运行把某个工具——或某个 MCP 服务器、某层记忆、技能按需加载、联网——展示给模型之后，它在这个对话余下的时间里一直保持开启，设置面板会把它置灰：已经调用过某个工具的转录，没法重放给一个不再拥有该工具的模型。后面的轮次可以扩大这个面，但不能收窄。

## 审批

每次调用在执行前都会在宿主上被归类——读、写或无界操作，工作区内或工作区外——由对话的安全层级决定哪些类别要先问你。选择器里标为**需审查**的工具，就是调用属于写或无界操作的那些。有几种确认任何级别都关不掉：触及工作区根、用户主目录或系统路径的递归删除；写全局记忆；声明需要用户在场的 MCP 工具；对你自己登录过的预览页面执行操作。[安全层级表](working.html#tools-and-approvals)是完整矩阵；每个工具页面写明适用于它的部分。

## 文件与搜索 {#group-filesystem}

选择器里的**文件与搜索**分组：六个文件工具，以及 `lsp`。

### 文件工具 {#files}

列出、搜索、读取和修改工作区文件的六个工具。`write` 和 `edit` 标为**需审查**。六个工具都限定在工作区和授予本对话的根目录之内，[工具与审批](working.html#tools-and-approvals)里的写入保护对它们每一个都适用。

{{TOOL_TABLE:ls,find,grep,read,write,edit}}

### `lsp` — 代码语义导航 {#filesystem-standalone}

`lsp` 向语言服务器询问符号而不是文本——某个名字在哪里定义、谁引用了它、谁调用了某个函数。它运行哪些服务器、怎样配置，见[代码语义导航](lsp.html)。

{{TOOL_TABLE:lsp}}

## Shell {#group-shell}

### Shell 工具 {#shell}

每个 shell 后端一个工具——`bash`、`zsh`、`sh` 和 `powershell`。选择器只列出对话所用机器上找到的 shell：Windows 机器可能提供 `powershell` 和 `bash`，macOS、Linux 或 WSL 机器提供 `zsh`、`bash` 和 `sh`。所有 shell 工具都标为**需审查**，而且都会产生任务，所以任务工具会随之出现。

{{TOOL_TABLE:bash,zsh,sh,powershell}}

## 预览 {#group-web}

选择器里的**预览**分组放着各个预览工具。两个联网工具也归在这里，尽管它们从不出现在选择器里。

### 预览工具 {#preview}

`preview_*` 工具运行项目的开发服务器，并在内置浏览器里操作它提供的页面。`preview_start`、`preview_stop` 和 `preview_list` 在选择器里没有自己的行：只要启用了任何其他预览工具，它们就随之启用。`preview_screenshot` 和 `preview_upload_image` 只提供给能看图的模型。服务器按 `.mework/launch.json` 里的配置启动——见[内置浏览器](working.html#the-built-in-browser)。

#### 开发服务器管理 {#preview-servers}

{{TOOL_TABLE:preview_start,preview_stop,preview_list,preview_logs}}

#### 查看页面 {#preview-observe}

{{TOOL_TABLE:preview_console_logs,preview_screenshot,preview_snapshot,preview_inspect,preview_network}}

#### 操作页面 {#preview-act}

{{TOOL_TABLE:preview_click,preview_fill,preview_eval,preview_resize,preview_upload_image,preview_dialog}}

### 联网 {#web-access}

两者都跟随对话唯一的**启用联网搜索**开关；一次运行拿到其中哪一个，取决于解析出的后端。

{{TOOL_TABLE:web_search,web_fetch}}

## 代理编排 {#group-orchestration}

{{TOOL_TABLE:agent_spawn,send_message,followup_task,task_wait,task_list,box,workflow,fork,todo,ask_user,skill,tool_search,plan,exit_plan_mode}}

## 长期记忆 {#group-memory}

{{TOOL_TABLE:read_global_memory,create_global_memory,edit_global_memory,read_project_memory,create_project_memory,edit_project_memory}}
