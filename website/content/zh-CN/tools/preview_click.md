点击对话的预览页面上的一个元素。模型会在快照告诉它页面上有什么之后用它：提交表单、打开菜单、跟进它刚改动的流程。鼠标输入由宿主经调试协议派发，不会作为页面脚本注入。

## 审批

一次调用被归类为无界操作（`tool.unbounded`，高风险），因此 `request_approval`、`allow_edits` 和 `plan` 会在它运行前询问，`full_access` 不会。页面工具从不提供**总是允许**，所以下一次点击还会再问一遍。在这一层之下，你亲自登录过的页面会先弹出一张接管卡，点名已登录的源；这道确认任何安全层级和任何钩子都替你答不了，而这份授权在页面离开该源的那一刻就失效。

## 行为与限制

它需要有可作用的对象：本工作区的某个 `serverId`、一台 `preview_start` 已经跑起来的开发服务器，或面板已持有的页面。

元素用 `document.querySelector` 找到，滚到视口中央，再在它边界矩形的中心以一对按下 / 释放完成点击——设了 `doubleClick` 就做两次。匹配不到任何东西的选择器，或没有矩形的元素，会让调用返回 `Failed to click element: <selector>`。`selector` 上限 2,048 字符，且不得为空。

调用限时 30 秒。页面已经持有对话框或文件选择器时，点击会被拒绝，并点名能清掉它的那个工具；而由这次点击自己打开的那一个，会在动作中途把页面挡住，结果变成 `{"interrupted": "modal-state"}`。成功的回答是点名选择器的一行字，而不是点击之后页面的描述。

## 相关

- [preview_fill](preview_fill.html) — 向输入框输入，而不是点击它
- [preview_snapshot](preview_snapshot.html) — 页面上有什么，前后各看一次
- [preview_dialog](preview_dialog.html) — 回答一次点击弹出的对话框
- [使用 Mework](../working.html#the-built-in-browser) — 浏览器与 `.mework/launch.json`
