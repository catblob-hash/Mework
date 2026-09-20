# 内置工具

Mework 内置 {{TOOL_COUNT}} 个工具。每个工具都有自己的页面：它替你做什么、什么时候需要审批、模型看到的参数，以及内置提示词档案交给模型的描述。工具名就是模型调用时用的名字，也是[工具描述文件](prompt-profiles.html)里引用的名字。

## 工具怎样变得可用

其中 {{SWITCHABLE_TOOL_COUNT}} 个是对话或预设的「启用的工具」列表里的开关（composer 旁边的滑块图标，或 设置 → 对话预设）。另外 {{DERIVED_TOOL_COUNT}} 个由宿主从别的设置**派生**，从不出现在那个列表里：

| 派生工具 | 跟随 |
|---|---|
| `read_global_memory`、`create_global_memory`、`edit_global_memory` | **全局记忆**开关 |
| `read_project_memory`、`create_project_memory`、`edit_project_memory` | **项目记忆**开关 |
| `web_search`、`web_fetch` | 对话唯一的**启用联网搜索**开关；一次运行拿到其中哪一个，取决于解析出的后端 |
| `task_wait`、`task_list`、`box` | 任何会产生任务的工具被启用（`agent_spawn`、`workflow`、`bash`、`powershell`、`preview_start`） |
| `skill` | **技能按需加载**开关，且至少选中了一个技能 |
| `tool_search` | **工具发现**开关（MCP），且本次运行扣留了 MCP 工具 schema |
| `plan`、`exit_plan_mode` | **安全层级**处于计划模式 |

一个对话里的工具暴露面只增不减。一次运行把某个工具——或某个 MCP 服务器、某层记忆、技能按需加载、联网——展示给模型之后，它在这个对话余下的时间里一直保持开启，设置面板会把它置灰：已经调用过某个工具的转录，没法重放给一个不再拥有该工具的模型。后面的轮次可以扩大这个面，但不能收窄。

## 审批

每次调用在执行前都会在宿主上被归类——读、写或无界操作，工作区内或工作区外——由对话的安全层级决定哪些类别要先问你。选择器里标为**需审查**的工具，就是调用属于写或无界操作的那些。有几种确认任何级别都关不掉：触及工作区根、用户主目录或系统路径的递归删除；写全局记忆；声明需要用户在场的 MCP 工具；对你自己登录过的预览页面执行操作。[安全层级表](working.html#tools-and-approvals)是完整矩阵；每个工具页面写明适用于它的部分。

## 全部工具

{{TOOL_INDEX}}
