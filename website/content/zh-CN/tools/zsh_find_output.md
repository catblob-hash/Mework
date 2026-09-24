`zsh_find_output` 和 [zsh](zsh.html) 完全一样地运行一条命令行，只返回输出里和这次调用的 `query` 相符、分数不低于 `threshold` 的部分，由 TypeSafe Jev 决策模型打分。和 `zsh` 一样，只有对话里有机器装了 zsh 时才会列出它，并且每次调用只能指定机器上有 zsh 的工作区。它是 `zsh` 的决策模型形式，在 Shell 工具窗口里选择。

## 审批

与 `zsh` 相同：每次调用都会在手动、允许编辑与计划模式弹出卡片；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。命令按与 `bash` 相同的静态分析读取，它只决定卡片的风险级别与规则。不提供**总是允许**，也没有操作系统级的沙箱。

## 行为与限制

命令按 `zsh` 的方式运行——在本机上是登录 shell（`zsh -l -c`），在 WSL 中和 SSH 上是 `zsh -f -c`——并以一条 `zsh` 命令登记在任务面板上。打分的对象、保留在报告上方的状态行、到达 `timeout` 时停止而不是转入后台、没有 `run_in_background`，以及 `query`、`threshold` 和密钥要等命令运行完才检查，都与 [bash_find_output](bash_find_output.html) 相同；命中的标签形如 `output lines N-M`。

## 决策模型

在 Shell 工具窗口的 **zsh** 下面，**加上决策模型参数**会同时启用 `zsh` 和 `zsh_find_output`，**只用决策模型**只留下 `zsh_find_output`。预置的预设不会打开它。查询和输出切出的块会发送到 TypeSafe 的 API，需要先在「全局设置 → 决策模型提供商」里填好密钥。

## 相关

- [zsh](zsh.html)、[bash_find_output](bash_find_output.html)、[find_output](find_output.html)
- [决策模型工具](../tools.html#decision-model)
- [使用 Mework](../working.html#tools-and-approvals)
