在对话的预览页面上模拟一个视口尺寸，还可以附带一种配色方案。模型用它来在手机或平板宽度下检查响应式布局，以及看一眼它刚写完媒体查询的那个页面在深色模式下的渲染。

## 审批

调整尺寸被归类为本地浏览器观察（`browser.local_observation`，低风险，读取效果）——它调整的是内置浏览器而不是作用于页面，和 `preview_snapshot`、`preview_inspect` 走的是同一条线。没有任何安全层级会询问，也不出现批准卡；上方的**需审查**标记是工具选择器的审查标志，不是审批线。它唯一还能引发的提示，是你亲自登录过的页面在任何页面工具触及标签页之前触发的接管确认；这张卡任何安全层级和任何钩子都答不了。

## 行为与限制

和每个页面工具一样，它需要一个运行中的开发服务器或面板已持有的页面，并以 30 秒为限。

宽度低于 768 CSS 像素时，模拟还会让页面相信自己是一部手机：设备度量携带 `mobile: true` 和 2 的设备像素比，用户代理变成一个由页面引擎（Windows 上的 WebView2、macOS 上内嵌的 CEF）自己的 Chromium 主版本号拼出的 Android Chrome 字符串，并启用触摸模拟，带五个触点和鼠标到触摸的转换。清除尺寸时这一切一并收回。

既不给尺寸也不给 `colorScheme` 的调用是错误，未知的 `preset` 或 `colorScheme`、少给一个维度的自定义尺寸同样是错误。尺寸留在标签页上，大于面板时缩小以适应，直到一次 `desktop` 调用清除它；`desktop` 不动 `colorScheme`。回答里每应用一项覆写就有一句话。

## 相关

- [preview_screenshot](preview_screenshot.html) — 看模拟后的布局
- [preview_inspect](preview_inspect.html) — 在新尺寸下量取一个元素
- [使用 Mework](../working.html#the-built-in-browser) — 内置浏览器
