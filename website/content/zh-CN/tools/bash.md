`bash` 工具通过原生 Bash 运行一条命令行——Git for Windows 或 MSYS2，绝不是 System32 的 WSL 启动器。当没有专用工具能覆盖这项工作时，模型就会用它：构建、测试、`git`、包管理器。

## 审批

每次调用都会在手动、允许编辑与计划模式弹出卡片；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。静态分析只决定卡片的风险级别与规则：重定向、命令替换或显式的可执行文件路径都算无界操作，而超过 10,000 字符的命令则完全不做分析。可能波及文件系统根、你的主目录或系统路径的递归删除，以及目标无法静态解析的递归删除，是一道任何安全层级和任何钩子都关不掉的确认。`bash` 从不携带长期许可，因此不提供**总是允许**。没有操作系统级的沙箱：批准只是让命令得以运行，并不会约束它。

## 行为与限制

对话的第一次调用会跑一遍你的 rc 文件，把结果存成一份快照，之后每个 shell 都 source 它，所以你的别名和函数都在；快照建不起来时退回登录 shell。目录变更只有在命令成功且没有离开工作区时才会延续。输出逐行解码，优先按 UTF-8，解不出的按系统 ANSI 代码页，并把 CRLF 折叠为 LF；失败的调用以 `Exit code N` 开头。模型最多看到 64 KiB，并标注已截断；任务面板保留最后 256 KiB。`timeout` 被钳制到 600000 毫秒，默认 120000 毫秒；到期时命令会被移交任务面板，只有没有空位接手时才会被杀掉。`run_in_background` 立即返回一个 `shell:<id>` 地址。在 WSL 中和 SSH 上它以 `--noprofile --norc` 运行，没有快照，也不延续目录。

## 相关

- [powershell](powershell.html)、[task_wait](task_wait.html)、[task_list](task_list.html)
- [使用 Mework](../working.html#tools-and-approvals)
- [钩子](../hooks.html)
- [bash_find_output](bash_find_output.html)、[find_output](find_output.html) — 只取回命令输出里和描述相符的部分
