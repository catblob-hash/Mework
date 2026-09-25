在预览页面上解析一个 CSS 选择器，以 JSON 记录返回找到的元素：标签名、文本、class 与 id、计算样式、边界框，以及（若存在）其背后的 React 组件。截图没法量，所以模型用它来核对颜色、字体、间距和尺寸。

## 审批

归类为本地浏览器观察（`browser.local_observation`，低风险，读取效果），作用域限于工作区和应用自己的数据目录，因此没有任何安全层级会为它询问；上方的**需审查**标记是目录的审查标志，不是审批线。不过 `selector` 缺失、不是字符串或超过 2,048 字符时，分类器会直接拒掉这次调用，而且是在为它创建页面之前。`PreToolUse` 钩子仍然可以要求一次确认；而你亲自登录过的页面弹出的接管卡，任何层级、钩子或长期许可都答不了。

## 行为与限制

只检查第一个匹配到的元素。没有匹配不算错误：结果是纯文本 `Element not found:` 加上那个选择器。不给 `styles` 时，记录携带十项计算属性 —— `color`、`background-color`、`font-size`、`font-weight`、`padding`、`margin`、`width`、`height`、`display` 和 `visibility`。你也可以改为自己指定，最多 64 项；计算样式里没有的属性会直接不出现在记录中，而不是以空值报告。

`text` 是元素的 `innerText`，截断到 500 字符；`className` 截断到 200。只有元素带着非空 `value` 属性时 `value` 才会出现。`boundingBox` 是内容盒，以 `x`、`y`、`width` 和 `height` 给出。元素位于 React 树内时，会从它的 fiber 补上 `reactComponent` 和 `reactProps`。DOM 与 CSS 域在每条退出路径上都会释放。调用以 30 秒为限。

## 相关

- [preview_snapshot](preview_snapshot.html) —— 用于找出值得检查的选择器
- [preview_screenshot](preview_screenshot.html)
- [使用 Mework](../working.html#the-built-in-browser)
