`zsh` 工具通过 zsh 运行一条命令行。只有对话里有机器装了 zsh（macOS、Linux 或 WSL 发行版；Mework 不在 Windows 上探测 zsh）时才会列出它，并且每次调用只能指定机器上有 zsh 的工作区。

## 审批

每次调用都会在手动、允许编辑与计划模式弹出卡片；只有完全访问——或一个回答 `allow` 的 `PreToolUse` 钩子——能放行。命令按与 `bash` 相同的静态分析读取，它只决定卡片的风险级别与规则。`zsh` 从不携带长期许可，因此不提供**总是允许**。没有操作系统级的沙箱：批准只是让命令得以运行，并不会约束它。

## 行为与限制

在本机上 zsh 以登录 shell 运行（`zsh -l -c`），由 `.zprofile` 设置 `PATH`；没有 rc 快照，所以 `.zshrc` 里只在交互时定义的别名不存在。目录变更只有在命令成功时才会延续。在 WSL 中和 SSH 上它以 `zsh -f -c` 运行，不读任何启动文件，也不延续目录。输出、`timeout` 与 `run_in_background` 的行为与 `bash` 相同。

## 相关

- [bash](bash.html)、[sh](sh.html)、[powershell](powershell.html)
- [使用 Mework](../working.html#tools-and-approvals)
- [zsh_find_output](zsh_find_output.html) — 只取回输出里和描述相符的部分
