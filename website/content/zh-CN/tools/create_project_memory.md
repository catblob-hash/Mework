在 `<workspace>/.mework/memory/` 中新建一份 Markdown 文档，并把它的一行描述记入由宿主掌管的 `MEMORY.md` 索引。项目层随工作区而行，因此模型一旦学到只在这里成立的事情——测试怎么跑、某个服务监听在哪——就会用到它。

## 审批

归类为宿主本地状态变更（`host.local_state_change`，中风险，写入效果）。任何安全层级都不会弹出批准卡，因此没有需要记住的**总是允许**。那道强制确认只属于全局层。

## 行为与限制

正文是完整的 Markdown，UTF-8 编码，上限 256 KiB，且不得含 NUL 字符。索引描述会折叠其中的空白，上限 300 个字符；它是唯一进入 `MEMORY.md` 的文本，而这个索引没有任何工具会直接写入。文档命名遵循 read 工具的规则。

名称已存在时直接失败，而不是覆盖。正文先写入同目录下的临时文件，再以原子替换发布，目标位置若是符号链接则拒绝；从存在性检查到索引重写的整段过程，在该层的 `memory/` 目录内持有一把阻塞式跨进程锁（`.memory.lock`）。索引重写失败会把刚建好的文档再删掉，因此不会留下孤儿正文。索引本身上限 64 KiB。

对话没有绑定到磁盘上的工作区目录时该层不可用；它会直接失败，而不会转而写进全局层。成功时工具报告规范化后的文件名和所在的层。和其他工具一样，这次调用连同正文都会显示在时间线卡上、交给钩子，并留在回放给模型的对话里。

## 相关

- [create_global_memory](create_global_memory.html) — 同一种写入，在每个工作区都适用
- [edit_project_memory](edit_project_memory.html), [read_project_memory](read_project_memory.html)
- [使用 Mework](../working.html#long-term-memory) — 两个层与各自的开关
