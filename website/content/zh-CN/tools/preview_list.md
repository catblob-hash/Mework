`preview_list` 报告本对话各工作区里正在运行、且本对话有权寻址的开发服务器。模型用它查找 `serverId` 以便调用 `preview_stop` 或 `preview_logs`，并在再次调用 `preview_start` 之前确认想要的服务器是否已经在跑。

## 审批

读取宿主自己的服务器注册表归类为本地浏览器观察（`browser.local_observation`，低风险，读取效果），作用域限于工作区与应用的数据目录。任何安全层级都不会为它询问，计划模式也不例外，因此永远不会出现批准卡。

## 行为与限制

结果是一个美化打印的 JSON 数组，每台服务器一个对象，携带 `serverId`、`port`、`status` 和 `startedAt`；对话有多个工作区时另有 `workspace`，服务器在别的机器上时另有 `machine`。`serverId` 是条目在 `launch.json` 里的名字，文件重名时带 `-1`、`-2`……编号；两个工作区可以都有一台 `dev`，靠 `workspace` 区分。`status` 在就绪轮询结束前是 `starting`，此后是 `running`；列表只会出现这两个值，因为注册表在进程退出的那一刻就丢弃这台服务器，无论它是由谁结束的。这里没有的 id 要么已经不在了，要么从未存在过。

服务器按工作区依次列出，每个工作区内按启动先后排列，且只有本对话启动的、或没有归属对话的那些。别的对话的服务器在这里不可见，尽管它们同样计入每个工作树五台的限额。接管条目——只有 `url` 没有命令——背后没有进程，永远不会被列出：接管的唯一报告是 `preview_start` 自己的回执，浏览器面板则从配置文件列出它们。

同一个 id 也把服务器当作后台任务来寻址，写作 `preview:<serverId>`——有多个工作区时写 `preview:<serverId>@<工作区编号>`——这正是 `task_list` 打印、`task_wait` 接受的形式。

## 相关

- [preview_start](preview_start.html), [preview_stop](preview_stop.html), [preview_logs](preview_logs.html)
- [task_list](task_list.html), [task_wait](task_wait.html)
- [使用 Mework](../working.html#the-built-in-browser)
