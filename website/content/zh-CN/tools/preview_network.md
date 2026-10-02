一个工具，两种形态。不带 `requestId` 时，它列出预览页面发出过的请求 —— id、方法、url、状态和失败原因。带上它时，则把那条响应的正文从浏览器缓存里读回来 —— 页面看着没问题、背后的数据却不对时，模型就靠它检查渲染出的页面上看不见的 API 载荷。

## 审批

归类为敏感浏览器观察（`browser.sensitive_observation`，高风险，无界效果）：每行都带着页面所登录站点的完整 URL、查询参数和响应正文。手动和允许编辑会询问；完全访问不会。**总是允许**永远记不住它 —— 没有任何 `preview_*` 页面工具能携带长期许可。你亲自登录过的页面会另外触发一道接管确认，任何层级或钩子都关不掉。

## 行为与限制

账本由浏览器自己的网络事件填入，保存页面最近的 500 行，满了先丢最旧的；只有从零创建的页面才从空账本开始。每行形如 `[requestId] METHOD url`，响应到达后补上 `→ status statusText`，没到达则补上 `[FAILED: reason]`。URL 截断到 8,000 字符；`data:` URL 保留媒体类型，载荷则变成被省略的字符数。`filter: "failed"` 保留失败以及 400 及以上的所有状态；无法识别的值会被拒绝，而不是被悄悄当作 `all`。空列表回答 `No network requests recorded.` 或 `No failed requests.`

能解析为 JSON 的正文会美化打印，然后截断到 10,000 字符并附上原始总数。二进制正文以 base64 报告并注明长度，不展示内容。缓存已经淘汰的正文会返回一个说明此事的错误：`requestId` 只在响应还在缓存里时有效。调用以 30 秒为限。

## 相关

- [preview_console_logs](preview_console_logs.html)
- [preview_logs](preview_logs.html) —— 服务器那一侧
- [使用 Mework](../working.html#the-built-in-browser)
