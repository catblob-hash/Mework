# 内置工具

Mework 内置 {{TOOL_COUNT}} 个工具。每个工具都有自己的页面：它替你做什么、什么时候需要审批、模型看到的参数，以及内置提示词档案交给模型的描述。工具名就是模型调用时用的名字，也是[工具描述文件](prompt-profiles.html)里引用的名字。

下面按工具选择器的分组排列。每个可开关的工具在选择器里各占一行，行首的书本图标就通向它在这里的页面。

## 工具怎样变得可用 {#available}

其中 {{SWITCHABLE_TOOL_COUNT}} 个是对话或预设的「启用的工具」列表里的开关（composer 旁边的滑块图标，或 设置 → 对话预设）。另外 {{DERIVED_TOOL_COUNT}} 个从别的设置或工具**派生**，从不出现在那个列表里：

| 派生工具 | 跟随 |
|---|---|
| `read_global_memory`、`create_global_memory`、`edit_global_memory` | **全局记忆**开关 |
| `read_project_memory`、`create_project_memory`、`edit_project_memory` | **项目记忆**开关 |
| `web_search`、`web_fetch` | 对话唯一的**启用联网搜索**开关；一次运行拿到其中哪一个，取决于解析出的后端 |
| `task_wait`、`task_list` | 任何会产生任务的工具被启用（`agent_spawn`、`workflow`、`bash`、`zsh`、`sh`、`powershell`、`preview_start`） |
| `box` | 每次运行都有：宿主追加给对话的每一条消息都由它送达 |
| `skill` | **技能按需加载**开关，且至少选中了一个技能 |
| `tool_search` | **工具发现**开关（MCP），且本次运行扣留了 MCP 工具 schema |
| `plan`、`exit_plan_mode` | 对话的**计划模式**开关 |
| `preview_start`、`preview_stop`、`preview_list` | 任何其他 `preview_*` 工具被启用 |

### 工具锁定 {#lock}

对话上一次请求带出去的工具面——工具、MCP 服务器、记忆层、联网、计划模式、技能和两个传递开关——就是它的**工具锁定**，它属于发出那次请求的模型。只要当前选中的还是那个模型，设置面板就把锁定直接画在各行上，行的右侧有一把锁：

- **橘色**——模型的提示缓存还没过期，而改动这一行会重写它缓存着的那部分提示词：关掉上一次请求带着的某个工具、计划模式、联网或技能，或者无论朝哪个方向改动某个 MCP 服务器、某层记忆、**技能按需加载**或**工具发现**。橘色的行仍然可以改。在一个对话里第一次改动时，Mework 会说明缓存将失效，问过你才改，并附一个**不再显示**选项，勾上后在所有对话里都不再提醒。在下一次请求之前改回原样，缓存仍然有效，这一行也重新变回橘色。打开一个工具永远不是橘色的：工具是追加进来的（见下文），缓存不受影响。**设置 → 提供商**里每个模型的档案都有一个**缓存失效时间**（分钟，默认 30）；距上一次请求超过这么久，橘色就消失。
- **灰色**——模型不支持在对话中途加入工具（见下文），所以它的整个工具面在首次请求时就固定了：不加也不减，与缓存无关。技能不是工具，不受限制。

换成别的模型，两种状态都解除，因为那个模型的缓存里没有这些；再换回来，锁定里记着的设置会被套回去。对话用过的搜索与抓取后端是另一回事：无论换什么模型都一直固定，因为转录里的搜索结果只有产生它的那个后端能被回放。

对话中途加入的工具——你在设置里新勾选的、自动压缩武装的交接工具、`tool_search` 取回的 MCP 工具——是**追加**进来的，而不是写回每次请求开头声明的工具列表：那份列表排在提示词最前面，改写它会让整段提示词缓存失效。宿主在时间线上记下工具加入的位置，由协议自己的追加接口在那个位置把工具交给模型，不附带任何文字通知。Anthropic Messages 用会话中途的 `tool_addition`（工具本身以 `defer_loading` 声明）；OpenAI Responses、Azure 与 Codex 用 `additional_tools` 输入项。**Claude Agent** 交给 Claude Code 自己追加：在它支持的模型上是它自己的 `tool_addition`，附带它自己的一行「工具现已可用」文字。其余协议没有追加接口。

模型能不能这样接收工具，是它的一项能力，和视觉输入并列：**设置 → 提供商**里模型属性中的**中途追加工具**。Mework 认得的模型由它自己勾上——获取模型时，以及你手动添加模型、输入 ID 时：Anthropic 官方 API 上的 Fable 5、Mythos 5、Opus 4.8 及之后的版本和 Sonnet 5.5；经 **Claude Agent** 时是同一份名单去掉 Sonnet 5.5；OpenAI 或 Azure 自己的端点上以及经 Codex 时，所有模型。Mework 不认得的模型——尤其是经中转站的，中转站未必把追加接口原样转发——一开始不勾，由你按端点的实际能力来勾：勾上后，追加照样发给那个端点。没有追加接口的协议上，这项能力不起作用。没有这项能力的模型，工具面在首次请求时就固定下来（[灰色](#lock)），**自动压缩**不会启动，也不提供**工具发现**。端点仍然拒绝追加时，工具退回声明列表。

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

{{TOOL_TABLE:agent_spawn,task_wait,task_list,box,workflow,fork,ask_user,skill,tool_search,plan,exit_plan_mode}}

## 长期记忆 {#group-memory}

{{TOOL_TABLE:read_global_memory,create_global_memory,edit_global_memory,read_project_memory,create_project_memory,edit_project_memory}}

## 交接 {#group-handoff}

这四个工具都不是开关。开启**自动压缩**（在上下文用量菜单里）后，上下文越过阈值的对话会被宿主请求交接——模型与端点支持会话中 system 消息时作为追加的系统提示词，否则与后台任务结果一样用 `box` 送达——并获得这四个工具。模型先写交接文档，再调用 `handoff`：它开启一个续接会话（新会话，不是分叉），并结束当前对话。新会话只带着原对话的系统提示词、相同的工具和这些交接文档开始，不带任何历史。继承了交接文档的对话从第一轮请求起就能读取它们。

{{TOOL_TABLE:read_handoff_note,create_handoff_note,edit_handoff_note,handoff}}
