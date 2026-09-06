# 钩子

钩子是在对话生命周期的固定时点由 Mework 运行的命令。它们从 **stdin** 读取事件的 JSON 描述，并通过 **stdout**（以及退出代码）作答。钩子可以添加上下文、阻止提示词或工具调用、重写工具调用的参数、允许或拒绝权限请求，或在模型尝试停止时要求其继续工作。文件格式和事件词汇遵循 Claude Code 的钩子，因此现有的 `hooks.json` 文件可直接沿用。

## 钩子的定义位置

钩子**不在**应用中编辑。Mework 会读取两个文件，并列出其中发现的内容：

```text
~/.mework/hooks.json                 用户范围——每个工作区
<workspace>/.mework/hooks.json       工作区范围——仅该工作区
```

（为兼容较早的配置，当 `.mework` 文件不存在时，仍会读取 `~/.naiword/hooks.json`。）编辑文件后，打开对话抽屉：**钩子**部分会将每个处理器及其事件显示为说明。每个处理器都是一个可选条目；对话（或预设）会在 `hookIds` 下选择所需的处理器。未被选中时，什么都不会运行。

## 文件格式

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "name": "Branch context",
            "command": "printf 'branch: %s' \"$(git branch --show-current)\"",
            "commandWindows": "Write-Output \"branch: $(git branch --show-current)\"",
            "timeout": 10
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "^(bash|powershell)$",
        "hooks": [
          { "type": "command", "name": "No force push", "command": "python hooks/no_force_push.py" }
        ]
      }
    ]
  }
}
```

| 字段 | 含义 |
|---|---|
| `hooks.<Event>` | 该事件的**组**数组。事件名称严格为 `SessionStart`、`InstructionsLoaded`、`UserPromptSubmit`、`PreToolUse`、`PermissionRequest`、`PostToolUse`、`Stop`。 |
| `matcher` | 可选正则表达式，会针对事件的主体进行测试：对于 `PreToolUse` / `PermissionRequest` / `PostToolUse` 是工具名称；对于 `SessionStart` 是来源（`startup`）。`UserPromptSubmit` 和 `Stop` 会忽略它。`"*"` 或未提供匹配器会匹配所有内容；无效正则表达式会禁用该组。 |
| `hooks[]` | 该组的处理器。仅支持 `"type": "command"`。 |
| `command` | 命令行。在 Windows 上通过 `pwsh`（或 `powershell`）`-NoProfile -Command` 运行；其他平台通过 `bash -lc` 运行。 |
| `commandWindows` | 可选的仅 Windows 命令行，会在 Windows 上替换 `command`，使一个文件可同时服务于两种 Shell。 |
| `name` | 显示名称；默认依次使用 `statusMessage`、`<Event> #<n>`。 |
| `statusMessage` | 钩子运行期间显示在时间线中。 |
| `timeout` | 秒数，1–600（默认 30）。 |

标记为 `async: true` 的处理器仅对 `InstructionsLoaded` 接受；不支持 `asyncRewake`，包含它的处理器会被跳过。

## 钩子收到的内容

命令的工作目录为工作区。stdin 是 UTF-8 JSON 对象；环境变量 `MEWORK_HOOK_EVENT` 也会携带事件名称。常见字段：

```json
{
  "session_id": "<conversation id>",
  "transcript_path": null,
  "cwd": "<workspace path>",
  "hook_event_name": "PreToolUse",
  "model": "<model id>",
  "permission_mode": "default | acceptEdits | bypassPermissions",
  "turn_id": "<uuid>"
}
```

`permission_mode` 映射对话的安全层级：`request_approval` → `default`，`allow_edits` → `acceptEdits`，`full_access` → `bypassPermissions`。事件专属字段：

| 事件 | 额外字段 |
|---|---|
| `SessionStart` | `source: "startup"` — 每个对话仅触发一次，即在此应用会话中的首个回合触发。 |
| `UserPromptSubmit` | `prompt` — 最后一条真实用户消息。 |
| `PreToolUse` | `tool_name`、`tool_use_id`、`tool_input` |
| `PermissionRequest` | `tool_name`、`tool_input`、`permission_suggestions: []` — 仅针对原本会显示批准卡的调用触发。 |
| `PostToolUse` | `tool_name`、`tool_use_id`、`tool_input`、`tool_response`（结果的公开投影） |
| `Stop` | `stop_hook_active`（模型是否正因先前的 Stop 钩子而继续），`last_assistant_message` |
| `InstructionsLoaded` | `file_path`、`memory_type`（`User` / `Project` / `Local` / `Managed`）、`load_reason`（`session_start` / `nested_traversal` / `path_glob_match` / `include`），以及相关时的 `globs`、`trigger_file_path`、`parent_file_path`。不包含指令正文。 |

工具钩子也会为子代理和工作流步骤运行；`SessionStart`、`UserPromptSubmit` 和 `Stop` 仅属于可见对话。

## 钩子如何作答

首先是**退出代码**：

- `0` — 成功；stdout 会按下文解释。
- `2` — **阻止**。stderr 会成为向模型显示的原因（若 stderr 为空，则使用提示词档案中的 `hook.blocked_default`）。对于 `PreToolUse` 和 `PermissionRequest`，这会拒绝调用；对于 `UserPromptSubmit` 和 `SessionStart`，这会在任何模型请求之前结束回合；对于 `PostToolUse`，这会拒绝结果；对于 `Stop`，这会阻止*停止*——模型会收到原因并继续工作，与下方的 `decision: "block"` 完全相同。
- 任何其他值 — 钩子失败；不会产生决定，回合继续进行（失败会记录在时间线中）。

然后是 **stdout**。对于 `SessionStart` 和 `UserPromptSubmit`，接受纯文本并作为附加上下文注入。对于每个事件，都会解析包含以下字段的 JSON 对象：

```json
{
  "continue": false,
  "stopReason": "…",
  "decision": "block",
  "reason": "…",
  "systemMessage": "…",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow | ask | deny",
    "permissionDecisionReason": "…",
    "updatedInput": { "command": "git push --force-with-lease" },
    "additionalContext": "…"
  }
}
```

| 字段 | 效果 |
|---|---|
| `continue: false` | 停止回合（任何事件）。会记录 `stopReason` 或 `reason`。 |
| `decision: "block"` + `reason` | 阻止事件。对于 `Stop`，在没有 `continue: false` 的情况下阻止，表示“继续工作”：原因（或 `hook.continue_fallback`）会作为新用户消息发送给模型，连续最多 3 次（`hook.stop_limit_reached`）。 |
| `systemMessage` | 仅显示在时间线中；模型看不到它。 |
| `hookSpecificOutput.hookEventName` | 必须等于当前事件，否则会忽略专属输出。 |
| `permissionDecision` (`PreToolUse`) | `allow` 会跳过此调用的批准卡；即使在 `full_access` 下，`ask` 也会强制显示批准卡；`deny` 会以 `permissionDecisionReason` 阻止；接受 `defer`，但会忽略它。旧版顶层 `decision: "approve"` / `"block"` 在此处也有效。 |
| `updatedInput` (`PreToolUse`) | 在分类**之前**替换调用参数：重写后的调用会重新分类，并按其自身条件获得批准。非对象值会被忽略，原调用会沿用常规权限流程。 |
| `PermissionRequest` 决定 | 嵌套：`hookSpecificOutput.decision = { "behavior": "allow" \| "deny", "updatedInput": {...}, "interrupt": true }`。接受 `updatedPermissions`，但绝不持久化——Mework 只授予当前调用。 |
| `additionalContext` | 作为模型在下一个请求中可见的非本地系统上下文注入。 |

如果 Stop 钩子输出任何内容，就必须使用 JSON 作答。钩子输出上限为 64 KiB；在一个回合中，所有同步钩子工作除每个处理器超时外，还共享 5 分钟的总预算。

以下两项保证源自“对实际将要运行的内容分类”：一个 `allow` 绝不会覆盖另一个钩子的 `ask` 或 `deny`，且执行前被拒绝的调用绝不会触发 `PostToolUse`。`PostToolUse` 无法撤销已运行的工具（文件写入已落盘）；它的拒绝仅会改变向模型告知的内容，且回执会如此说明（`hook.post_tool_not_rolled_back`）。[批准](working.html#tools-and-approvals)下列出的强制确认无法由钩子预先批准。

`InstructionsLoaded` 仅用于观察：其退出代码、stdout 和 JSON 会被记录，但永远不会到达模型。

## 示例

使用 Python 阻止危险的 git 命令（`PreToolUse`，匹配器 `^(bash|powershell)$`）：

```python
import json, sys, re
event = json.load(sys.stdin)
command = event.get("tool_input", {}).get("command", "")
if re.search(r"git\s+push\s+.*--force\b(?!-with-lease)", command):
    print("force pushes are not allowed; use --force-with-lease", file=sys.stderr)
    sys.exit(2)
```

不阻止而是重写（相同事件），在 stdout 上以 JSON 作答：

```python
import json, sys
event = json.load(sys.stdin)
tool_input = event["tool_input"]
tool_input["command"] = tool_input["command"].replace("--force", "--force-with-lease")
print(json.dumps({
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "ask",
        "updatedInput": tool_input,
        "permissionDecisionReason": "rewrote --force to --force-with-lease"
    }
}))
```

将当前分支添加到每个提示词（`UserPromptSubmit`，纯文本 stdout）：

```json
{ "type": "command", "name": "Branch", "command": "git branch --show-current", "commandWindows": "git branch --show-current" }
```

让模型持续工作直至测试通过（`Stop`）：

```python
import json, subprocess, sys
event = json.load(sys.stdin)
if event.get("stop_hook_active"):
    print(json.dumps({}))          # already continuing once; let it stop
    sys.exit(0)
if subprocess.run(["npm", "test"], capture_output=True).returncode != 0:
    print(json.dumps({"decision": "block", "reason": "npm test fails; fix it before finishing"}))
else:
    print(json.dumps({}))
```

## 在哪里查看发生了什么

每次钩子运行都会在时间线中留下诊断卡：事件、状态、名称、时长、stdout/stderr 和决定。这些卡片仅保存在本地，绝不会进入模型请求。模型会看到的内容是被阻止调用结果中的原因、注入的 `additionalContext`，以及 Stop 钩子的继续消息——它们均通过以 `hook.` 开头的[提示词档案](prompt-profiles.html)键来措辞。

所选钩子也会列在系统提示词中（`system.hooks_section`），其中会有一句话告知模型钩子在宿主上运行，且模型不得声称某个钩子已成功。
