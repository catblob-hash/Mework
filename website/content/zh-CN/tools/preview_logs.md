`preview_logs` 返回一台开发服务器打印过的东西：它自己的 stdout 和 stderr，从启动起就被捕获。模型在改动需要服务器重新构建之后、页面加载失败时，或启动的服务器没有按预期提供服务时用到它。这是服务器的输出，不是页面的——浏览器控制台消息来自 `preview_console_logs`。

## 审批

读取宿主自己填充的缓冲区归类为本地浏览器观察（`browser.local_observation`，低风险，读取效果），作用域限于工作区与应用的数据目录。任何安全层级都不会为它询问，计划模式也不例外，因此永远不会出现批准卡。

## 行为与限制

每台服务器保留一个 1000 段的环形缓冲。一段是管道的一次读取，不是一行，所以单段可以容纳很多行；两条流按到达顺序共享这个缓冲。`lines` 默认 50，会被限定在 1–200 之间而不是被拒绝；它按段切取末尾，因此返回的文本可能多得多。

`level: "error"` 只保留含有 `error`、`exception`、`failed` 或 `fatal` 的 stderr 段，不区分大小写；stdout 里写着 "error" 的行不算。`search` 是区分大小写的子串过滤，在级别过滤之后施加。`all` 和 `error` 之外的任何 `level` 都会让调用失败。

不给 `serverId` 时，调用回退到本对话在此可寻址的第一台启动中或运行中的服务器。没有可读的内容会以文字说明，而不是作为失败返回：`No logs yet.`、`No server errors found.` 或 `No logs matching "…"`。来自接管的 id，以及一台服务器都没有的调用，都会以一句话应答并指回 `preview_list`。

服务器的缓冲会在进程退出或被 `preview_stop` 杀掉的那一刻丢弃，所以必须在服务器还活着时读取输出。服务器以 `FORCE_COLOR=1` 运行，因此输出可能带有 ANSI 转义序列。

## 相关

- [preview_start](preview_start.html), [preview_list](preview_list.html), [preview_stop](preview_stop.html)
- [preview_console_logs](preview_console_logs.html), [preview_network](preview_network.html)
- [使用 Mework](../working.html#the-built-in-browser)
- [preview_find_logs](preview_find_logs.html) —— 按描述在这份输出和页面控制台里查找日志行
