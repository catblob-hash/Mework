回答预览页面打开的 `alert`、`confirm` 或 `prompt`。对话框由原生持有——页面的 JavaScript 就阻塞在那次调用里，和真实浏览器一样——而且一个对话框被持有期间，其他所有页面工具都会被拒绝。模型在自己的一次点击弹出确认框的那一刻就会用它。

## 审批

一次调用被归类为无界操作（`tool.unbounded`，高风险），因此 `request_approval` 和 `allow_edits` 会先询问，`full_access` 不会。页面工具从不提供**总是允许**。如果被持有的页面是你亲自登录过的，接管卡会先出现，并点名已登录的源；这道确认任何安全层级和任何钩子都替你答不了。

## 行为与限制

`accept` 默认为 true，因此不带参数的调用接受对话框；false 则将其取消。`prompt_text` 是在 `prompt` 里输入的答案，仅在接受时生效，上限 4,096 字符。

必须有一个对话框开着。没有时调用失败，并返回页面最近弹出的五个对话框，让模型看看自己是不是漏掉了一个。

与多数页面工具不同，这一个会等页面平复，然后描述它：它回答的那个对话框、是否被接受、prompt 文本，以及一个页面块，带有 URL、标题、加载状态、控制台计数、动作期间打印的错误行，和一个有界的无障碍快照。如果作答启动了一次导航，或导航策略拒绝了一次导航，结果都会说明。页面若再打开一个对话框，报告的就是那个模态状态，而不是快照。

## 相关

- [preview_click](preview_click.html) — 通常引发对话框的那次交互
- [preview_upload_image](preview_upload_image.html) — 清掉另一种模态状态：文件选择器
- [preview_snapshot](preview_snapshot.html) — 没有东西持有它时的页面
- [使用 Mework](../working.html#the-built-in-browser) — 内置浏览器
