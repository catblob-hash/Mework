`sh` 工具通过机器上的 POSIX `/bin/sh` 运行一条命令行——通常是 dash 或 BusyBox ash，有时是 POSIX 模式下的 bash。只有对话里有机器有 `sh`（macOS、Linux、WSL）时才会列出它；没有装 bash 的机器也因为它而可用。

## 审批

每次调用都会在手动、允许编辑与计划模式弹出卡片；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。命令按与 `bash` 相同的静态分析读取。`sh` 从不携带长期许可，因此不提供**总是允许**。没有操作系统级的沙箱。

## 行为与限制

它以 `sh -c` 运行，不读任何启动文件。在本机上命令成功时目录变更会延续；在 WSL 中和 SSH 上每次调用都从工作区根目录开始。请写可移植的 POSIX shell：不要用 `[[ ]]`、数组、`$'…'` 或花括号展开。输出、`timeout` 与 `run_in_background` 的行为与 `bash` 相同。

## 相关

- [bash](bash.html)、[zsh](zsh.html)
- [使用 Mework](../working.html#tools-and-approvals)
