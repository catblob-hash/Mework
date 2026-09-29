# 隐私政策

生效日期：2026 年 9 月 29 日。本政策适用于 Mework 桌面应用的所有版本：安装版、便携版，以及通过 Microsoft Store 分发的 MSIX 版。

## 概要

Mework 不收集任何数据。没有 Mework 账号，没有遥测、统计分析或崩溃上报，开发者也不运营任何供应用连接的服务器。你在 Mework 里做的一切都留在你的电脑上，只有你让它发往你所选服务的请求除外。

## 留在你电脑上的内容

设置、工作区、对话、附件、记忆与日志存放在 `%APPDATA%\com.mework.app` 和 `%LOCALAPPDATA%\com.mework.app`。API Key 与登录令牌保存在操作系统的凭据库（Windows 凭据管理器）里，从不写进上述文件。卸载应用不会删除这些数据；删除这两个文件夹即可清除。

## 会离开你电脑的内容，以及去向

只发往你配置的服务，且只在你使用时发送：

- **模型提供商**（例如 OpenAI、Anthropic、Google、Azure OpenAI、Amazon Bedrock、Google Vertex、xAI、DeepSeek 或任何 OpenAI 兼容端点）会收到每次请求的对话内容、其中包含的文件与工具结果，以及你的 API Key 或登录凭证。这些数据受各提供商自己的隐私政策约束。
- **OpenAI Codex** 用你的 ChatGPT 账号在 OpenAI 登录。**Claude Agent** 提供商运行随 Mework 附带的 Claude Code，用你的 Anthropic 账号登录；Mework 关闭了它的遥测与错误上报。
- **联网搜索与抓取**：你配置的搜索服务会收到查询词；智能体抓取的网站、你在内置浏览器里打开的页面会收到普通的网页请求。
- 你添加的 **MCP 服务器、钩子和 SSH 机器** 会收到你配置给它们的内容。
- **更新检查**：安装版与便携版在你打开「设置 → 版本更新」时向 GitHub（`api.github.com`）查询最新发布，GitHub 会看到你的 IP 地址与应用版本号。Microsoft Store 版从不检查，由 Store 负责更新。
- **本地小模型**：你选择安装时，其文件从 Hugging Face 下载。

## 儿童

Mework 是面向开发者的工具，不以 13 岁以下儿童为对象。

## 变更

本政策的变更会发布在本页，并更新生效日期。

## 联系方式

对本政策有疑问，请在 <https://github.com/catblob-hash/Mework/issues> 提交 issue。
