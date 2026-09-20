`box` 是一个容器，而不是一个动作。当一项后台任务进入终态却没有人等待它时，宿主会把该任务的结果写进一个模型从未请求过的 `box` 工具结果——子代理的回答、工作流运行的结局、后台命令的输出。它被放进目录，是为了让模型在转录中出现这样一个结果时认得出这个形状。

## 审批

没有可批准的东西：这次调用被归类为低风险读操作，任何安全层级都不会询问它，而实际上的调用者就是宿主。

## 行为与限制

这个工具由宿主派生，从来不是一个复选框。只要对话启用了 `agent_spawn`、`workflow`、`bash`、`powershell` 或 `preview_start`——能往任务面板里添一行的那五个工具——它就会出现，并随它们一起消失。调用它不算错误；宿主会用一句话回答，说明什么都没有发生。

一次投递的正文是一个 `<task-notification>` 块：`<task-id>` 携带任务的地址，`<status>` 携带持久化的状态字（`completed`、`failed`、`stopped`、`interrupted`、`roundLimit`），一个单行的 `<summary>`，`<result>` 里是输出，以及 `<usage>` 携带那个已结束回合的 token 数、工具调用数和时长。仍在运行的子代理发给主代理的消息也搭乘同一载体，状态为 `message`，且没有 `<usage>`。任务自身输出里出现的收尾 `</result>` 或 `</task-notification>` 会被中和，因此一个结果无法在自己周围伪造字段；其他标记则按原样保留。

每次投递都会持久化为一张卡片，此后的每个回合都从那张卡片重建同一次交换，而不是生成新卡。

## 相关

- [task_wait](task_wait.html) — 在结果以这种方式送达之前收取它
- [task_list](task_list.html)、[agent_spawn](agent_spawn.html)
- [使用 Mework](../working.html#subagents-workflows-and-tasks)
