# 内置工具

Mework 内置 {{TOOL_COUNT}} 个工具。每个工具都有自己的页面：它替你做什么、什么时候需要审批、模型看到的参数，以及内置提示词档案交给模型的描述。工具名就是模型调用时用的名字，也是[工具描述文件](prompt-profiles.html)里引用的名字。

下面按工具选择器的分组排列。其中有三组工具是同一种能力拆成的许多次调用——文件、shell 和预览——选择器把每一组显示成一行：`files`、`shell` 或 `preview`，点开是它自己的窗口；这三组在本页各有一节，选择器里那一行旁边的书本图标就通向这一节。

## 工具怎样变得可用

其中 {{SWITCHABLE_TOOL_COUNT}} 个是对话或预设的「启用的工具」列表里的开关（composer 旁边的滑块图标，或 设置 → 对话预设）。另外 {{DERIVED_TOOL_COUNT}} 个由宿主从别的设置**派生**，从不出现在那个列表里：

| 派生工具 | 跟随 |
|---|---|
| `read_global_memory`、`create_global_memory`、`edit_global_memory` | **全局记忆**开关 |
| `read_project_memory`、`create_project_memory`、`edit_project_memory` | **项目记忆**开关 |
| `web_search`、`web_fetch` | 对话唯一的**启用联网搜索**开关；一次运行拿到其中哪一个，取决于解析出的后端 |
| `task_wait`、`task_list`、`box` | 任何会产生任务的工具被启用（`agent_spawn`、`workflow`、`bash`、`zsh`、`sh`、`powershell` 或它们的筛选输出形式、`preview_start`） |
| `skill` | **技能按需加载**开关，且至少选中了一个技能 |
| `tool_search` | **工具发现**开关（MCP），且本次运行扣留了 MCP 工具 schema |
| `plan`、`exit_plan_mode` | **安全层级**处于计划模式 |

一个对话里的工具暴露面只增不减。一次运行把某个工具——或某个 MCP 服务器、某层记忆、技能按需加载、联网——展示给模型之后，它在这个对话余下的时间里一直保持开启，设置面板会把它置灰：已经调用过某个工具的转录，没法重放给一个不再拥有该工具的模型。后面的轮次可以扩大这个面，但不能收窄。

## 审批

每次调用在执行前都会在宿主上被归类——读、写或无界操作，工作区内或工作区外——由对话的安全层级决定哪些类别要先问你。选择器里标为**需审查**的工具，就是调用属于写或无界操作的那些。有几种确认任何级别都关不掉：触及工作区根、用户主目录或系统路径的递归删除；写全局记忆；声明需要用户在场的 MCP 工具；对你自己登录过的预览页面执行操作。[安全层级表](working.html#tools-and-approvals)是完整矩阵；每个工具页面写明适用于它的部分。

## 决策模型工具 {#decision-model}

选择器里标着**决策模型**的工具，是为了把大块材料——一个文件、一份目录列表、一条命令的输出、页面上的元素、预览的日志——挡在对话模型的上下文之外。宿主把材料切成小块，交给 TypeSafe Jev 决策模型对照一句自然语言的 `query` 逐块打分，或者从一串元素里选出一个；只有分数达到这次调用的 `threshold` 的部分，或者被选中的那一个元素，才会交回给对话模型。这些小块会发送到 TypeSafe 的 API，所以每一个这类工具都需要先在「全局设置 → 决策模型提供商」里填好 TypeSafe 密钥，预置的预设里也一个都没有打开。

它们有两种形态。八个是独立的工具：`find_files`、`find_content` 和 `find_output` 在选择器里各有一行，四个筛选输出的 shell 和 `preview_find_logs` 则在 [`shell`](#shell) 与 [`preview`](#preview) 窗口里、在各自的基础工具下面选择。另有五个预览工具直接接受决策模型参数，同样在 `preview` 窗口里选择。凡是挂在另一个工具下面选择的决策模型形式，**加上决策模型参数**会把直接形式保留在它旁边，**只用决策模型**则只留下决策模型形式。

## 文件与搜索 {#group-filesystem}

选择器里的**文件与搜索**分组：`files` 这一行，以及三个各占一行的工具。

### `files` — 文件工具 {#files}

列出、搜索、读取和修改工作区文件的六个工具，在选择器里是一行：`files`。它的计数表示六个里开了几个；点开是**文件工具**窗口，每个工具各有一个开关，分组标题上的 **+** 会把这六个连同分组里的其他工具一起打开。`write` 和 `edit` 标为**需审查**。六个工具都限定在工作区和授予本对话的根目录之内，[工具与审批](working.html#tools-and-approvals)里的写入保护对它们每一个都适用。

{{TOOL_TABLE:ls,find,grep,read,write,edit}}

### 独立工具 {#filesystem-standalone}

这几个工具在选择器里各占一行。`lsp` 从语言服务器取得答案（见[代码语义导航](lsp.html)）。`find_files` 和 `find_content` 凭一句描述而不是模式去找文件、找文件里的段落；它们走[决策模型](#decision-model)。

{{TOOL_TABLE:lsp,find_files,find_content}}

## Shell {#group-shell}

### `shell` — Shell 工具 {#shell}

每个 shell 后端一个工具——`bash`、`zsh`、`sh` 和 `powershell`——在选择器里合成一行：`shell`。它的窗口只列出对话所用机器上找到的 shell：Windows 机器可能提供 `powershell` 和 `bash`，macOS、Linux 或 WSL 机器提供 `zsh`、`bash` 和 `sh`。每个 shell 打开之后，还可以另选它的筛选输出形式——`bash_find_output` 及其同类——运行同样的命令，但只返回输出里和描述相符的部分：**加上决策模型参数**把它和普通 shell 一起启用，**只用决策模型**只启用它。无论开的是哪种形式，这一行都只把这个 shell 算一次；分组标题上的 **+** 会打开各个 shell，但从不打开它们的筛选输出形式。所有 shell 工具都标为**需审查**，而且都会产生任务，所以任务工具会随之出现。

{{TOOL_TABLE:bash,bash_find_output,zsh,zsh_find_output,sh,sh_find_output,powershell,powershell_find_output}}

### 独立工具 {#shell-standalone}

`find_output` 在选择器里单独占一行。它按 `task_list` 给出的 `shell:<id>` 地址，回头读取本对话运行过的一条 shell 命令——已结束或仍在运行，前台或后台都可以——只返回保留下来的输出里和描述相符的部分。

{{TOOL_TABLE:find_output}}

## 操控 {#group-web}

选择器里的**操控**分组放着 `preview` 这一行。两个联网工具也归在这里，尽管它们从不出现在选择器里。

### `preview` — 预览工具 {#preview}

`preview_*` 工具运行项目的开发服务器，并在内置浏览器里操作它提供的页面；选择器把它们显示成一行：`preview`。它的窗口有三页，下面各对应一张表。`preview_find_logs` 是 `preview_logs` 的决策模型形式，像 shell 的筛选输出形式一样挂在它下面选择。`preview_console_logs`、`preview_snapshot`、`preview_inspect`、`preview_click` 和 `preview_fill` 直接接受决策模型参数；点击、填写和检查元素还可以在未命中时把每一行元素逐个打分。`preview_screenshot` 和 `preview_upload_image` 只提供给能看图的模型。服务器按 `.mework/launch.json` 里的配置启动——见[内置浏览器](working.html#the-built-in-browser)。

#### 开发服务器管理 {#preview-servers}

{{TOOL_TABLE:preview_start,preview_stop,preview_list,preview_logs,preview_find_logs}}

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
