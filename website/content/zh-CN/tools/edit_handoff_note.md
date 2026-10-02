替换一篇已有交接文档中的一段，并刷新它的索引描述。继承了交接文档、又被要求再次交接的对话用它更新这些文档，而不是另写一份，因此一连串交接始终维护同一套文档。

## 审批

任何层级都不询问：与 `create_handoff_note` 一样，它只修改本对话自己的笔记本（`host.local_state_change`）。

## 行为与限制

`old_text` 必须在文档中恰好出现一次；找不到或出现多次都会报错并说明是哪种情况。`new_text` 留空表示删除这一段。`description` 替换这篇文档的索引描述。修改后的文档仍以 256 KiB 为上限。

## 相关

- [create_handoff_note](create_handoff_note.html)、[read_handoff_note](read_handoff_note.html)、[handoff](handoff.html)
