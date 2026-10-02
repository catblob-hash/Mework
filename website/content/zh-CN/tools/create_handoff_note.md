写一篇新的交接文档：给接手工作的会话看的 Markdown。对话的上下文越过**自动压缩**阈值后宿主才提供这个工具；它从不出现在工具选择器或预设里。下一个会话只能看到这个对话的系统提示词、工具和这些文档，看不到任何历史，所以文档要写清任务、决定、已完成的部分、剩下的部分以及停在了哪里。

## 审批

任何层级都不询问。文档写进应用数据目录下对话自己的笔记本，而不是工作区，因此被归类为修改本对话的宿主状态（`host.local_state_change`）。

## 行为与限制

`description` 成为这篇文档在笔记本索引 `HANDOFF.md` 里的一行，索引由宿主维护和重写；它是一句话，最多 300 个字符。正文不能为空，最多 256 KiB。同名文档已存在时创建会被拒绝——修改文档请用 `edit_handoff_note`。命名规则同 [read_handoff_note](read_handoff_note.html)。

时间线卡片会显示文档名、索引描述和正文，你能看到交接了什么。

## 相关

- [edit_handoff_note](edit_handoff_note.html)、[handoff](handoff.html)
- [使用 Mework](../working.html#auto-compact)
