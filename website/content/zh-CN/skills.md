# 技能

**技能**是包含 `SKILL.md` 文件的文件夹：为某一种任务打包的说明——部署清单、审查流程、仓库约定——还可以在其旁放置脚本和参考文件。其格式与 Claude Code、Codex 和公开技能注册表使用的格式相同，因此现有技能可原样安装。

## `SKILL.md` 格式

```markdown
---
name: Commit helper
description: Prepare a conventional commit from the current diff
when_to_use: The user asks to commit, stage, or write a commit message
author: Example Org
version: 1.0.0
tags: [git, commits]
---

# Commit helper

1. Run `git status` and `git diff --staged`.
2. Group unrelated changes into separate commits.
3. Write a conventional commit message; run `scripts/check-message.sh` on it.
```

前置元数据是扁平的 `key: value` 键值对（并非完整 YAML）。Mework 只读取以下键，并忽略其他键：

| 键 | 用途 |
|---|---|
| `name` | 技能在目录中的名称，以及模型传递给 `skill` 工具的值。依次回退到第一个标题和文件夹名称。 |
| `description` | 显示在目录中；它与 `when_to_use` 一起构成模型看到的**触发**文本。 |
| `when_to_use` | 以 `description - when_to_use` 的形式追加到触发文本。 |
| `author`、`version`、`tags` | 仅为目录元数据。标签可以是 `[a, b]` 或 `a, b`。 |

**正文**（前置元数据之后的所有内容）是模型读取的内容。它是用户创作的内容，会逐字进入模型上下文，因此应将其写成给模型的说明。其中的相对路径（`scripts/…`、`references/…`）相对于技能目录解析，`skill` 工具会连同正文一起返回该目录。

限制：一个技能最多为 100 MiB 且包含 2 000 个条目；压缩包中的符号链接、绝对路径和 `..` 段会被拒绝；文件夹名称不区分大小写时必须唯一。

## 安装

设置 → **技能** → **添加技能**提供三种来源：

- **在线搜索** — 宿主会并发搜索 skills.sh、claude-plugins.dev 和 clawhub.ai；GitHub 标签页接受仓库中某个 `SKILL.md` 文件的直接链接。安装使用 `git`（仅浅拉取技能目录）或 ZIP 下载；基于 git 的来源要求 `git` 位于你的 `PATH` 中。
- **系统搜索** — 扫描 `~/.mework/skills`、`~/.claude/skills`、`~/.codex/skills`、`~/.config/skills` 以及你工作区中的 `.mework/skills` / `.claude/skills` 文件夹，并列出找到的技能以供导入。
- **本地导入** — 文件夹或 ZIP 文件。

所有来源最终都会进入同一个安装器：技能会被**复制**到 `%APPDATA%\com.mework.app\skills\<folder>`，并注册到设置文档中。有意复制而非引用：正文会粘贴到模型上下文中，而应用外的文件夹可能在两个回合之间被任何其他程序改写。之后编辑原件不会更改已安装的副本——请重新安装以更新。

技能页面中的每张卡片都有一个**全局启用**开关和卸载操作。

## 为对话选择技能

安装使技能可用；只有对话（或其起始预设）在设置抽屉的**技能**部分选择它时，才会使用该技能。必须同时满足两个条件：全局开关已打开，且已选择该 id。已选择但被停用或缺失的技能显示为*已选择，当前未激活*，且不会产生任何作用。

## 技能如何到达模型

**技能传递**开关（位于同一抽屉中，选择技能后显示）用于在两种机制之间选择：

- **关闭（默认）——粘贴到系统提示词中。**每个选定技能的正文都会追加到系统提示词，以 `---` 行分隔。其中不包含标题和前置元数据。模型会在每个回合读取技能，并在每个回合为其支付 token。
- **开启——通过 `skill` 工具按需加载。**系统提示词不包含正文。模型会获得一个 `skill` 工具，其 `name` 参数是选定技能构成的 `enum`，其描述列出各技能的触发文本（[提示词档案](prompt-profiles.html)中的 `skill.tool_description` 和 `skill.listing_row`）。当模型调用它时，工具会返回 `Base directory for this skill: <path>`，随后是正文（`skill.result`）。此模式下，两个名称相同的选定技能会被拒绝，因为 `enum` 无法区分它们。

`skill` 工具是派生出来的，不在工具选择器中：仅当开关开启且至少一个选定技能成功解析时才会出现。子代理会继承父对话所选的技能。

## 验证技能是否已激活

- 传递**关闭**时，下一条模型回复会遵循技能说明；在模型提供商（或模拟提供商）的开发者控制台中，正文会出现在系统提示词的 `---` 分隔符之后。
- 传递**开启**时，时间线会显示一张 `skill` 工具调用卡；模型加载技能时，其结果以基础目录行开头。

## 故障排除

| 症状 | 原因 |
|---|---|
| 设置抽屉中没有该技能 | 它尚未安装——抽屉仅列出已安装技能。请使用添加技能。 |
| 已选择，当前未激活 | 全局开关已关闭，或已安装文件夹失去了其 `SKILL.md`。 |
| 两个技能名称相同，且按需加载失败 | 在其中一个的 `SKILL.md` 中重命名（`name:`），然后重新安装；或者仅选择其中一个。 |
| 某个注册表的在线搜索失败 | 注册表相互独立；其他注册表仍会返回结果。GitHub 直接链接必须以 `/SKILL.md` 结尾。 |
| 在线安装立即失败 | `git` 不在 `PATH` 中；参见设置 → 环境依赖。 |
