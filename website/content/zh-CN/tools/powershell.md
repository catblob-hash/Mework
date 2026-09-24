`powershell` 工具通过 PowerShell 运行一条命令，以 `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command` 启动。优先使用 PowerShell 7，实在没有才退到 Windows PowerShell 5.1。在 Windows 上，模型用它处理 Git Bash 覆盖不到的工作：cmdlet、.NET 类型、注册表。

## 审批

每次 shell 调用都会在手动、允许编辑与计划模式弹出卡片；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。静态分析只决定卡片的风险级别与规则：它看得见的文件访问（`Get-Content`、`Remove-Item` 及其别名）按文件工具的路径策略评判；凡是触及非文件系统提供程序、UNC 路径或调用运算符的命令，一律算无界操作，而超过 10,000 字符的命令则完全不做分析。可能波及文件系统根、你的主目录或系统路径的递归删除，是一道任何安全层级和任何钩子都关不掉的确认。`powershell` 从不携带长期许可，因此不提供**总是允许**。没有操作系统级的沙箱：批准只表示命令可以运行，不表示它受到了约束。

## 行为与限制

一段前置脚本会把 `Out-File` 的默认编码设为 UTF-8，并在 `FullLanguage` 语言模式下设置 `$OutputEncoding` 与纯文本渲染；当命令在语法上必须排在最前面时（`param`、`using …`、以 `[` 开头），这段前置脚本会被跳过。文件 cmdlet 保留各自的默认值，所以 5.1 下对无 BOM 的 UTF-8 文件执行 `Get-Content` 会返回乱码。输出逐行解码——优先 UTF-8，解不出的按系统 ANSI 代码页——并把 CRLF 折叠为 LF；失败的调用以 `Exit code N` 开头，模型最多看到 64 KiB，并标注已截断。最终目录只有落在工作区内时才会延续，后台运行的命令则从不延续目录。`timeout` 被钳制到 600000 毫秒，默认 120000 毫秒。在经 SSH 连接的 Windows 机器上，它经由那台机器的代理运行，控制台输出切换为 UTF-8，每次调用都从工作区根目录开始；WSL 和没有 PowerShell 的机器上不会提供它。

## 相关

- [bash](bash.html)、[read](read.html)、[task_wait](task_wait.html)
- [使用 Mework](../working.html#tools-and-approvals)
- [钩子](../hooks.html)
- [powershell_find_output](powershell_find_output.html) — 只取回输出里和描述相符的部分
