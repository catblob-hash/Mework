读取预览页面的无障碍树，以缩进大纲的形式返回 —— 每个节点一行，写明它的角色、可访问名称，以及（若有）它的值。这是模型查看页面确切文本与结构最廉价又可靠的办法。模型会在一次导航或一次交互之后用它，确认页面说出的正是该说的话。

## 审批

归类为本地浏览器观察（`browser.local_observation`，低风险，读取效果），作用域限于工作区和应用自己的数据目录，因此没有任何安全层级会为它询问；上方的**需审查**标记是目录的审查标志，不是审批线。`PreToolUse` 钩子仍然可以要求一次确认；而宿主为你亲自登录过的页面弹出的接管卡，任何层级、钩子或长期许可都答不了。

## 行为与限制

无障碍域只为这次读取启用，读完即释放。每行形如 `[uid] role: "name" (value: "…")`，按深度缩进，名称和值各自截断到 200 字符。下面再无有趣内容的 presentational 或 generic 包装节点自身不打印，由子节点顶上它的位置；只含一个子节点的 generic 包装节点会折叠进该子节点；SVG 根的内部结构和装饰性图像的子节点会被丢弃；深度超过 8 之后，节点的子树改以 `... (N descendants)` 代替。整个快照上限 12,000 字符，并以一行写明真实总数收尾。无话可报的页面回答 `No accessible content found.`

每行的 `uid` 来自一个贯穿页面整个生命周期的计数器，因此后来的快照绝不会重用先前的编号。它不是地址：`preview_click` 和 `preview_fill` 接受的是 CSS 选择器。调用需要一个为本工作区运行着的开发服务器，或者面板已经持有的页面，并以 30 秒为限。

## 决策模型参数

在对话设置的预览工具窗口里为它打开决策模型选项后，它多出 `query` 与 `threshold`：页面的元素行切成段交给 TypeSafe Jev 决策模型打分，只返回分数不低于 `threshold` 的元素，每个都附上能直接交给 [preview_click](preview_click.html)、[preview_fill](preview_fill.html)、[preview_inspect](preview_inspect.html) 的 CSS 选择器，而不是整份快照。**加上决策模型参数**保留整份快照；**只用决策模型**要求每次都带这两个参数。元素行会发送到 TypeSafe 的 API，需要先在「全局设置 → 决策模型提供商」里填好密钥。

## 相关

- [preview_inspect](preview_inspect.html)
- [preview_screenshot](preview_screenshot.html)
- [使用 Mework](../working.html#the-built-in-browser)
