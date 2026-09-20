`send_message` 往子代理的收件箱里投一条消息。除此之外什么也不发生：不开启回合，不收取结果，子代理在下一次组装模型请求时才会读到这条消息。模型用它纠正一个正在运行的子代理——一个刚得知的文件名，一条此刻才开始起作用的约束。

## 审批

这次调用只在本对话内部做协调，不触及任何文件、Shell 或网络，因此被归类为低风险读操作（`host.read_or_coordinate`），任何安全层级都不会询问它。

## 行为与限制

`target` 必须是本对话的子代理；在更早回合中结束的子代理会先从对话自己的记录中恢复。工作流运行和后台 Shell 命令会被拒绝，拒绝信息指向 `task_wait`。`message` 上限为 32768 个字符。

结果会说明发生的是两种情形中的哪一种，因为这个差别决定接下来该做什么。消息会送达正在运行的子代理，它在当前回合内就会读到；不在运行的子代理则只是把消息排队——而排队的消息谁也唤不醒，所以真正另起一个回合的是 `followup_task`，已排队的消息会随它一并带上。

子代理持有的是这个工具的收窄形式，不接受 `target`，因为它们唯一的收件人就是派生它们的主代理。只有当父对话同时启用 `agent_spawn` 和 `send_message` 时，它们才会拿到这个工具。每个子代理最多保留 32 条这样的消息，超限先丢最旧的；它们会在主代理的下一个轮边界，作为状态为 `message` 的一次 `box` 投递送达。

## 相关

- [agent_spawn](agent_spawn.html) — 创建子代理及其名字
- [followup_task](followup_task.html) — 追加一条指令并开启一个回合
- [task_wait](task_wait.html)、[task_list](task_list.html)、[box](box.html)
- [使用 Mework](../working.html#subagents-workflows-and-tasks)
