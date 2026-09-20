把一个值写进对话的预览页面上的一个表单控件。模型用它来在提交前填写字段、清空输入框，或在 `select` 里选一个选项。页面自己的监听器照常运行，因此由框架控制的字段会保住这个值，而不是把它还原。

## 审批

一次调用被归类为无界操作（`tool.unbounded`，高风险）：`request_approval`、`allow_edits` 和 `plan` 会在它运行前询问，`full_access` 不会。**总是允许**不会记住任何页面工具。如果页面是你亲自登录过的，接管卡会先出现，点名已登录的源；这道确认不会被任何安全层级关掉，钩子也答不了它。

## 行为与限制

目标必须可达：本对话工作区的某个 `serverId`、一个运行中的开发服务器，或面板已持有的页面。

全部工作由一段页面侧脚本完成。它先聚焦元素，随后：对 `select`，挑出值或可见文本等于 `value` 的选项；对 `input` 和 `textarea`，经原型上的原生 `value` setter 写入；对 `contenteditable` 元素，替换其文本。其余类型的控件、找不到的元素或不存在的选项，会让调用返回 `Failed to fill element: <selector>`。成功后会接着触发一个冒泡的 `input` 事件和一个冒泡的 `change` 事件。

空字符串是合法值——清空一个字段用的就是它。`selector` 上限 2,048 字符且不得为空，`value` 上限 32,768 字符。调用限时 30 秒；页面持有对话框或文件选择器时会被拒绝；回答是点名选择器的一行字，而不是页面的状态。

## 相关

- [preview_click](preview_click.html) — 填完表单后按下按钮
- [preview_inspect](preview_inspect.html) — 读回字段此刻持有的值
- [preview_upload_image](preview_upload_image.html) — file 输入框的对应工具
- [使用 Mework](../working.html#the-built-in-browser) — 内置浏览器
