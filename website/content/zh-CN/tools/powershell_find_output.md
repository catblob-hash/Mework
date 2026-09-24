`powershell_find_output` 和 [powershell](powershell.html) 完全一样地运行一条命令——以 `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command` 启动，优先 PowerShell 7，其次 Windows PowerShell 5.1——只返回输出里和这次调用的 `query` 相符、分数不低于 `threshold` 的部分，由 TypeSafe Jev 决策模型打分。凡是提供 `powershell` 的地方都提供它，也就是装有 PowerShell 的 Windows 机器，WSL 上从不提供。它是 `powershell` 的决策模型形式，在 Shell 工具窗口里选择。

## 审批

与 `powershell` 完全相同：每次调用都会在手动、允许编辑与计划模式弹出卡片，卡片上显示命令；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。静态分析只决定卡片的风险级别与规则：它看得见的文件访问按文件工具的路径策略评判；凡是触及非文件系统提供程序、UNC 路径或调用运算符的命令，一律算无界操作；可能波及文件系统根、你的主目录或系统路径的递归删除，是一道任何安全层级和任何钩子都关不掉的确认。不提供**总是允许**，也没有操作系统级的沙箱。

## 行为与限制

命令走的就是 `powershell` 自己的路径——同样的 UTF-8 前置脚本、逐行解码并把 CRLF 折叠为 LF、目录规则和 `timeout` 钳制——并以一条 `powershell` 命令登记在任务面板上。在经 SSH 连接的 Windows 机器上，它经由那台机器的代理运行，从工作区根目录开始。打分的对象、保留在报告上方的状态行、到达 `timeout` 时停止而不是转入后台、没有 `run_in_background`，以及 `query`、`threshold` 和密钥要等命令运行完才检查，都与 [bash_find_output](bash_find_output.html) 相同；命中的标签形如 `output lines N-M`。

## 决策模型

在 Shell 工具窗口的 **PowerShell** 下面，**加上决策模型参数**会同时启用 `powershell` 和 `powershell_find_output`，**只用决策模型**只留下 `powershell_find_output`。预置的预设不会打开它。查询和输出切出的块会发送到 TypeSafe 的 API，需要先在「全局设置 → 决策模型提供商」里填好密钥。

## 相关

- [powershell](powershell.html)、[bash_find_output](bash_find_output.html)、[find_output](find_output.html)
- [决策模型工具](../tools.html#decision-model)
- [使用 Mework](../working.html#tools-and-approvals)
- [钩子](../hooks.html)
