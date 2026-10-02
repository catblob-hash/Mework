# MCP 服务器

Mework 是一个 MCP **客户端**。在 `mcp.json` 文件中声明服务器，在需要看到它的对话里勾选它，它的工具就会在每一回合开始时被发现，并与内置工具一起提供给模型。支持两种传输方式：

- **stdio** —— Mework 启动一个进程，通过它的 stdin/stdout 讲 JSON-RPC。
- **Streamable HTTP** —— Mework 连接一个 URL。较旧的 HTTP+SSE 传输不受支持。

## 服务器在哪里声明

MCP 服务器**不**在应用内注册。它们和大多数 MCP 客户端一样，声明在一个文件里：

```text
~/.mework/mcp.json                 用户范围——每个工作区
<workspace>/.mework/mcp.json       工作区范围——仅该工作区
```

文件形状就是 Claude Code 的 `.mcp.json`：顶层一个 `mcpServers` 对象，以服务器名为键。

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:/projects"],
      "env": { "LOG_LEVEL": "info" }
    },
    "docs": {
      "type": "http",
      "url": "http://127.0.0.1:3000/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" }
    }
  }
}
```

### 接受的键

| 键 | 含义 |
|---|---|
| `type` | `stdio`，或 `http` / `streamable-http` / `streamable_http` 表示 Streamable HTTP。给了 `command` 而没有 `type` 时，条目按 stdio 处理。 |
| `command`、`args`、`env` | stdio：要启动的进程、它的参数、以及给它的环境变量。`command` 直接执行，绝不经过 shell。 |
| `cwd` | stdio：进程的工作目录。必须是绝对路径，且指向一个实际存在的目录。 |
| `envPassthrough` | stdio：要从 Mework 自己的环境透传给进程的变量名。子进程从一个清空的环境起步，外加一份让它能找到程序与缓存的小白名单（`PATH`、`PATHEXT`、`SystemRoot`、`WINDIR`、`COMSPEC`、`TEMP`、`TMP`、`TMPDIR`、`HOME`、`USERPROFILE`、`APPDATA`、`LOCALAPPDATA`、`PROGRAMDATA`、`ProgramFiles`、`ProgramFiles(x86)`、`XDG_CACHE_HOME`、`XDG_CONFIG_HOME`），因此应用持有的其他任何东西，都只能经这个键或 `env` 到达服务器。`env` 的优先级高于两者。以 `MEWORK_`、`ANTHROPIC_`、`OPENAI_`、`CLAUDE_`、`CODEX_`、`AWS_`、`AZURE_`、`GOOGLE_`、`GEMINI_` 或 `DEEPSEEK_` 开头的名字会被拒绝——应用自己的提供商凭据不许透传——该条目随之显示为不可用。 |
| `url`、`headers` | http：端点与额外的请求头（通常是 `Authorization`）。公网主机必须用 `https`；回环与私网地址可以用 `http`。 |
| `timeout` | 单请求超时，单位为**毫秒**（Claude Code 的键）。低于 1000 的值会被忽略，改用宿主默认值；其他值向上取整到整秒。 |
| `timeoutSeconds` | 同一超时，单位为整秒（Mework 的键）。它优先于 `timeout`，并被夹到 5 分钟以内；`0` 表示用宿主默认值（45 秒）。 |
| `longRunning` | 标记一个调用确实会很慢的服务器：在没有显式超时时，它的调用拿到 5 分钟上限而不是默认的 45 秒。 |
| `description` | 显示在目录中；被选中时列入系统提示词小节 `system.mcp_section`，在 240 个字符处截断。没有描述的条目以 `system.mcp_server_default_description` 列出。 |
| `registryUrl` | 包仓库镜像。对 `npx`、`npm`、`bun`、`bunx`、`pnpm` 与 `yarn` 命令作为 `npm_config_registry` 生效，对 `uv`、`uvx`、`pip`、`pipx`、`python` 与 `python3` 作为 `UV_INDEX_URL` + `PIP_INDEX_URL`；只接受 http(s) URL；你在 `env` 里自己设的变量优先。 |
| `disabledTools` | 不提供给模型的工具名。这是一份排除清单：服务器以后新增的工具在把它的名字加进这里之前都可用。 |
| `disabledAutoApproveTools` | 关闭自动批准的工具名：它的每次调用都在每一级询问，等同于一条 `requiresUserInteraction` 声明。 |
| `${VAR}` / `${VAR:-default}` | 在 `command`、`args`、`env` 的值、`url` 与 `headers` 的值中展开。 |

服务器名只能包含字母、数字、`-` 与 `_`，且不得为 `__proto__`、`constructor` 或 `prototype`。`type` 为 `sse`、`ws` 或 `sdk` 的条目、使用 `headersHelper` 或 `oauth` 的条目、以及引用了一个未设置且没有 `:-default` 的变量的 `${VAR}`，都会让该行**留在目录里**——标为不可用，并把原因作为它的描述——这样从别的客户端拷来的文件会自己解释清楚，而不是悄悄消失。顶层的 `servers` 键或一个 JSON 数组解析不出任何条目（会在宿主 stderr 上留一条说明）；未知的键被忽略。每一条解析出的条目都按运行时的方式校验，因此 Mework 拨不通的条目会以该消息显示为不可用。

只有**工具**会提供给模型。服务器暴露的提示词与资源由连接测试读取，仅此而已。

每个可用的行都有一个**测试连接**按钮：它拨一次服务器，完成握手，并列出它的工具、提示词与资源。该行的徽标随后会报告它找到多少个工具，tooltip 中则带着服务器自报的名称与版本——或者显示**连接失败**，附错误与服务器写进 stderr 的最后几行。不可用的行从不拨号；它的徽标显示**不可用**，原因在 tooltip 中。行的删除按钮会把该键从它被声明的那个 `mcp.json` 中移除，其余条目和其余顶层键都保持你写下的样子；对话上一次请求带着的服务器，在它被画成橘色期间（见[工具锁定](tools.html#lock)）没有删除按钮。

## 为对话选择服务器

与技能一样，被发现使服务器可用；对话抽屉的 **MCP** 页面决定本对话使用哪些（预设模板化的是同一个字段）。一个对话只能从全局 `~/.mework/mcp.json` 加上**它自己工作区**的 `.mework/mcp.json` 中选择；其他工作区的服务器不会出现，预设可以保留任何 id。被选中的服务器会列在系统提示词中，好让模型知道它们存在；它们的工具是否真的可调用，由回合开始时的发现决定。

扫描再也找不到的已选 id 会保持选中，并显示为**悬空**行；它在运行时被**跳过**。扫描能找到、但拨不通的条目会以原因显示为不可用，选中它的运行会**失败**，直到你修好或取消勾选。页面工具条上有一个**重新扫描**按钮和两个**打开文件夹**按钮——全局的 `~/.mework`，以及对话在指向文件夹的工作区中时该工作区的 `.mework`。文件在时它们会显示出那个 `mcp.json`，不在时则创建目录并打开它。

## MCP 工具如何到达模型 {#how-mcp-tools-reach-the-model}

每一回合开始时，宿主连接每一个已选中且可用的服务器（会话按对话划分、池化，空闲 30 分钟后关闭），调用 `tools/list`，并给它找到的每个工具起一个抗冲突的名字：

```text
mcp__<server-slug>_<server-digest>__<tool-slug>__<tool-digest>
```

在这一名字下，模型拿到的是服务器自己的标题、描述与输入 schema；下文的**工具发现**决定它随每个请求发出，还是等模型索取时才取回。`disabledTools` 中的工具名在发现时就被丢弃，模型永远看不到它们。如果某个服务器连接失败，它的工具在该回合缺席，失败被记录；回合本身继续。子代理继承父对话已发现的绑定，并按其角色自己的工具清单收窄。

一次调用会被还原为用工具原名发起的 `tools/call`；回复的 `content`、`structuredContent` 与 `isError` 以 JSON 返回给模型（服务器的 `_meta` 被剥掉）。超过 64 KiB 的回复被拒绝，而不是截断。

### 工具发现

**工具发现**开关（位于列表下方，始终显示）决定发现的工具如何交到模型手上：

- **关闭，开关读作*全部声明*。**每个已发现工具的完整 schema 随每个请求发出，与内置工具完全一样。
- **开启，*按需取回*——经 [`tool_search` 工具](tools/tool_search.html)取回。**不声明任何 MCP schema。上下文改为携带一个 `<deferred-tools>` 块，按声明它们的服务器分组列出被扣留的名字（[提示词档案](prompt-profiles.html)中的 `tool_search.announcement` 与 `tool_search.announcement_row`）。`tool_search` 接受一个 `query`：`select:<name>[,<name>…]` 返回那些确切的定义，其他写法则是对被扣留名字、其服务器及其描述的关键词搜索（`+term` 是每个结果都必须包含的词，其余用于排序），结果数以 `max_results` 为上限，默认 5。定义已经取回的工具从下一步起可以像其他工具一样直接调用：它在取回的位置[追加](tools.html#available)，而不是改写声明的工具列表；调用一个 schema 还没取回的工具会被拒绝，拒绝消息带着那个能修好它的 `select:` 调用（`tool_search.not_loaded`）。

新对话从它起始的预设继承这个开关；内置的 **mework** 预设开局就是开的。改动它会重写提示词里携带这些工具的部分，所以在模型缓存还热的时候，它和[工具锁定](tools.html#lock)里的其他设置一样画成橘色。不支持在对话中途加入工具的模型，取回的 schema 无处交付，所以在这类模型上开关保持关闭、无法打开。公告在整个运行期间固定，所以某个名字在它的 schema 到达之后仍留在名单里。子代理继承该模式，并把继承到的每个工具重新扣留一次，因为它从空历史起步，从没读过父对话的 `tool_search` 结果。

`tool_search` 是派生出来的，不在工具选择器中：它恰好出现在开关开启且本次运行至少扣留了一个工具的时候。

## 批准

MCP 工具被视为外部副作用：

- 在 `request_approval` 与 `allow_edits` 下，每次调用都要确认，除非某个 `PreToolUse` 或 `PermissionRequest` 钩子放行。在 `full_access` 下调用不经询问直接运行。
- 声明了 `_meta["anthropic/requiresUserInteraction"] = true` 的工具**每次调用**都询问，在每一级都如此，钩子无法预先批准它。它给模型的描述会加上 `mcp.mandatory_description_prefix` 前缀，它的标签也带一条写着同样内容的备注。值格式不对时按“询问”失败封闭。
- `disabledAutoApproveTools` 中点名的工具行为相同：同样的前缀，且它在每一级都询问。

## 端到端验证一个服务器

1. 把条目写进 `~/.mework/mcp.json` 或工作区的 `.mework/mcp.json`；该行会出现在抽屉的 **MCP** 页面（若页面当时开着，按**重新扫描**）。点击该行的**测试连接**：徽标应报告它找到多少个工具，或失败原因连同服务器的 stderr。
2. 在对话中勾选它，发一条需要它某个工具的消息，在时间线上寻找一张以该服务器命名的工具卡；卡片出现时批准这次调用。
3. 如果什么都没发生：先确认系统提示词小节存在（服务器必须已被选中），再重读**测试连接**的结果或在终端里跑同一条命令（启动即退出的 stdio 服务器不会产出任何工具），最后看模型——不支持工具调用的提供商永远看不到 MCP 工具。

## 故障排除

| 症状 | 原因 |
|---|---|
| 列表中没有该服务器 | 它的条目不在 `~/.mework/mcp.json` 或本工作区 `.mework/mcp.json` 中，或文件不是 `{"mcpServers": {…}}` 对象（顶层的 `servers` 键或数组不会被读取），或其名称含有字母、数字、`-`、`_` 之外的字符。 |
| 该行显示"Missing environment variables: …" | `command`、`args`、`env`、`url` 或 `headers` 中的某个 `${VAR}` 引用没有值也没有 `:-default`。设置该变量、加一个默认值，或直接写出值。 |
| 该行因传输原因不可用 | `type` 是 `sse`、`ws` 或 `sdk`，给了 `url` 却没给 `type`，或条目使用了 `headersHelper` 或 `oauth`。改用 `"type": "http"`，把请求头放进 `headers`，或在那里带上令牌。 |
| 测试连接以 spawn 错误失败 | `command` 不在应用的 `PATH` 上，或参数不对。在终端里试试同一条命令。Windows 上可执行文件通过 `PATH` 与 `PATHEXT` 解析（因此 `npx` 能找到 `npx.cmd`），但只认 `.COM`、`.EXE`、`.BAT` 与 `.CMD`，且命令行不经过 shell——给可执行文件名或完整路径，而不是管道。 |
| 测试后工具出现了，模型却从不调用 | 该服务器没有在对话中被选中，或该提供商/模型没有工具调用能力。 |
| 模型说某个工具的 schema 没有加载 | **工具发现**是开的，schema 是取回的，不是声明的。拒绝消息指名了加载它的那次 `tool_search` 调用；`<deferred-tools>` 块列出每一个还在等待的名字。 |
| 在完全访问下每次调用仍要询问 | 该工具声明了 `requiresUserInteraction`，或被点名列在 `disabledAutoApproveTools` 中。 |
| 两个服务器暴露同名工具 | 名字自带每服务器摘要，因此两者都可调用；描述会把它们区分开。 |
| 某个 HTTP 服务器拒绝 `http://` | 只有回环/私网地址可以走明文 HTTP；公网端点需要 `https`。 |
| 改了文件却看不到变化 | 屏幕上的列表是一份快照。按页面上的**重新扫描**；启动、删除条目和新增工作区时也会扫描。运行总是现读文件，所以保存的改动下一回合就会生效，无论列表是否已经跟上。 |
