在预览页面里求值一个 JavaScript 表达式，并把结果交回给模型。它是最后的检查手段：读快照暴露不了的状态、查询 DOM，或用 `window.location` 把页面挪走——这组工具里没有导航工具。

## 审批

一次调用被归类为无界操作（`tool.unbounded`，高风险），因此 `request_approval`、`allow_edits` 和 `plan` 会在它运行前弹一张批准卡，`full_access` 不会。页面工具从不提供**总是允许**。在你亲自登录过的页面上，运行循环会先征询模型可否接管这个标签页，并点名已登录的源；这张卡不会被任何安全层级禁用，钩子也答不了它。

## 行为与限制

表达式在页面自己的上下文里求值，结果按值返回；是 Promise 就先等它。完成值以美化打印的 JSON 返回，JavaScript 的 `undefined` 以文本 `undefined` 返回，抛出的异常则连同异常自身的描述成为这次调用的错误消息。

`expression` 不得为空，上限 65,536 字符。求值限时 15 秒，整次调用限时 30 秒。

脚本发起的导航，仍由该对话安全层级下的页面导航策略裁决；被拒绝的导航会回滚，页面停在原地。对话框或文件选择器被持有期间，求值一律被拒绝，直到清掉它的那个工具运行为止；求值期间打开的对话框会以 `{"interrupted": "modal-state"}` 结束这次求值。你亲自控制面板期间它同样被拒绝。

## 相关

- [preview_snapshot](preview_snapshot.html) — 无需脚本的结构化页面内容
- [preview_inspect](preview_inspect.html) — 单个选择器的计算样式与几何信息
- [preview_console_logs](preview_console_logs.html) — 页面打印了什么
- [使用 Mework](../working.html#the-built-in-browser) — 一个对话一个页面
