在 `~/.mework/memory/` 中新建一份 Markdown 文档，并把它的一行描述记入由宿主掌管的 `MEMORY.md` 索引。全局层在每个工作区都会加载，因此模型一旦学到某件与当前打开的项目无关、又值得长期留存的事情——一项偏好、一条本机事实、一条约定——就会用到它。

## 审批

每一次调用都会弹出批准卡，所有安全层级都如此，完全访问和计划模式也不例外（`memory.global_persistent_mutation`，高风险，写入效果）。这道确认是强制的：回答 `allow` 的 `PreToolUse` 钩子满足不了它，卡片不提供**总是允许**，早先为这个工具名记录的长期许可既不会被读取，也不会被写入。拒绝会让调用失败，回合继续运行。

## 行为与限制

正文是完整的 Markdown，UTF-8 编码，上限 256 KiB，且不得含 NUL 字符。索引描述会折叠其中的空白，上限 300 个字符；它是唯一进入 `MEMORY.md` 的文本，而这个索引没有任何工具会直接写入。文档命名遵循 read 工具的规则。

名称已存在时直接失败，而不是覆盖。正文先写入同目录下的临时文件，再以原子替换发布，目标位置若是符号链接则拒绝；从存在性检查到索引重写的整段过程持有一把阻塞式跨进程锁（`.memory.lock`），因此共享 `~/.mework` 的第二个 Mework 实例只会等待，而不会丢掉一次索引更新。索引重写失败会把刚建好的文档再删掉，因此不会留下孤儿正文。索引本身上限 64 KiB。成功时工具报告规范化后的文件名和所在的层。正文不进入时间线、钩子与回执，它们只保留名称、描述和一个字节数。

## 相关

- [create_project_memory](create_project_memory.html) — 同一种写入，作用于工作区
- [edit_global_memory](edit_global_memory.html), [read_global_memory](read_global_memory.html)
- [使用 Mework](../working.html#long-term-memory) · [钩子](../hooks.html)
