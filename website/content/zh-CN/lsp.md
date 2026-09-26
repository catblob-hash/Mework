# 代码语义导航

`lsp` 工具让模型按**符号**而不是按文本提问：这个名字定义在哪、谁引用了它、它的类型是什么、谁调用了这个函数。为此 Mework 会启动语言服务器——就是你编辑器里用的那些普通 [LSP](https://microsoft.github.io/language-server-protocol/) 进程——并与它们对话。

一个工具，九个操作：

| 操作 | 回答什么 |
|---|---|
| `goToDefinition` | 光标处的符号定义在哪 |
| `findReferences` | 它的全部用处，按文件分组 |
| `hover` | 它的文档与类型 |
| `documentSymbol` | 一个文件里的全部符号，按层级 |
| `workspaceSymbol` | 整个项目里匹配查询的符号 |
| `goToImplementation` | 接口或抽象方法的实现 |
| `prepareCallHierarchy` | 该位置的调用层次项 |
| `incomingCalls` | 谁调用了这个函数 |
| `outgoingCalls` | 这个函数调用了什么 |

所有操作都要 `filePath`、`line`、`character`。行号与列号**从 1 开始**，与编辑器显示的一致。`workspaceSymbol` 还接受 `query`。各参数以模型看到的形态列在 [`lsp` 工具页](tools/lsp.html)里。

`lsp` 是对话的**启用工具**列表里的一个开关，名称为**代码语义导航**。内置的 **mework** 预设开着它。

普通调用被归类为对 `filePath` 的一次**读**：在工作区内时任何[安全层级](working.html#tools-and-approvals)都放行，在工作区外时手动与计划模式会询问。如果你在做的项目自带 `.mework/lsp.json`，每次调用就转而归类为**无界操作**，并在完全访问以下的每个层级询问——那份文件能指定 Mework 启动哪个命令，而你 clone 下来的仓库不等于你做过的选择。受检查的只是它存不存在，从不看其内容。除此之外没有任何东西会改变归类：你自己的 `~/.mework/lsp.json` 和内置表都维持原状，实际应答调用的是哪台服务器也不参与判定。**总是允许**会为该对话记住 `lsp` 的答案。

## 你可能已经装了的服务器

Mework 内置一张常见服务器的小表，并且**只在它的命令已经在 `PATH` 上时**才提供：

| 名字 | 命令 | 文件 |
|---|---|---|
| `rust-analyzer` | `rust-analyzer` | `.rs` |
| `typescript-language-server` | `typescript-language-server --stdio` | `.ts` `.tsx` `.mts` `.cts` `.js` `.jsx` `.mjs` `.cjs` |
| `pyright` | `pyright-langserver --stdio` | `.py` `.pyi` |
| `gopls` | `gopls` | `.go` |
| `clangd` | `clangd` | `.c` `.h` `.cpp` `.cc` `.cxx` `.hpp` `.hxx` `.hh` |
| `lua-language-server` | `lua-language-server` | `.lua` |
| `bash-language-server` | `bash-language-server start` | `.sh` `.bash` |

按平常的方式装上你要的那个——`rustup component add rust-analyzer`、`npm i -g pyright`、`go install golang.org/x/tools/gopls@latest`——下次扫描就会提供它。命令不在的预设根本不会被提供，于是原本会归它管的文件落到声明同一扩展名的别的服务器上——谁都不声明时，得到的就是 “No LSP server available”。

## 自己声明

别的情况——表里没有的语言、同一语言想换一台服务器、项目需要的参数——写进 `lsp.json`，位置与技能、MCP、钩子相同：

```text
~/.mework/lsp.json                 用户级——所有工作区都看得到
<工作区>/.mework/lsp.json          工作区级——只属于该工作区
```

顶层是按服务器名索引的 `lspServers` 对象：

```json
{
  "lspServers": {
    "rust-analyzer": {
      "command": "rust-analyzer",
      "extensionToLanguage": { ".rs": "rust" },
      "initializationOptions": { "cargo": { "allFeatures": true } },
      "settings": { "rust-analyzer": { "checkOnSave": false } }
    },
    "zls": {
      "command": "zls",
      "extensionToLanguage": { ".zig": "zig" }
    }
  }
}
```

### 接受的键

| 键 | 含义 |
|---|---|
| `command` | **必填。** 要启动的程序。不经过 shell，原样执行，所以给可执行文件名或完整路径。裸名字里带空格会被拒绝——参数写进 `args`。 |
| `extensionToLanguage` | **必填。** 文件扩展名到 LSP 语言 id 的映射。这一张表同时决定**这台服务器管哪些文件**和打开文件时送出的 `languageId`。键会规范化，`"rs"`、`".RS"`、`".rs"` 是同一个意思。 |
| `args` | 命令行参数。很多服务器需要在这里写 `--stdio`。 |
| `env` | 进程的额外环境变量。 |
| `initializationOptions` | 原样作为 `initialize.initializationOptions` 传出。 |
| `settings` | 用于回答服务器的 `workspace/configuration` 请求，并作为 `workspace/didChangeConfiguration` 推送一次。 |
| `workspaceFolder` | 服务器启动的根目录。留空表示对话所在的工作区。 |
| `startupTimeout` | `initialize` 的毫秒上限，默认 30000。 |
| `shutdownTimeout` | 优雅 `shutdown` 的毫秒上限，默认 5000。 |
| `restartOnCrash`、`maxRestarts` | 崩溃后是否重启、最多几次。默认 `true` 与 `3`。 |
| `diagnostics` | 设为 `false` 保留导航，但不再把这台服务器报的问题送给模型。默认 `true`。 |
| `description` | 关于该条目的自由文本，最多 240 个字符。 |
| `${VAR}` / `${VAR:-默认值}` | 在 `command`、`args`、`env` 的值和 `workspaceFolder` 里展开。 |

服务器名只能含字母、数字、`-` 和 `_`，且不能是 `__proto__`、`constructor`、`prototype`。`transport` 接受但只有 `"stdio"` 能用；`"socket"` 会让该条目不可用，而不是显得能在一条根本没接的传输上工作。没有 `extensionToLanguage`、`${VAR}` 缺值且无默认、裸命令带空格，都以同样的方式被拒绝。拒绝按条目生效——文件其余部分照常算数。整个文件不是合法 JSON，或顶层不是 `{ "lspServers": { … } }` 的，则整个跳过。

## 谁来答一个文件

声明该扩展名的**第一条**条目胜出，按这个顺序查找：

1. 本工作区的 `.mework/lsp.json`
2. `~/.mework/lsp.json`
3. 内置预设

项目自己的文件压过你的全局文件，两者都压过预设。与预设**同名**的条目是替换而不是竞争——想给 `rust-analyzer` 加参数就这么加，不会变成两台。

没有像技能、MCP 服务器和钩子那样的按对话选择。哪台答一个文件由扩展名决定，勾选框表达不了任何东西。

## 用到才启动

第一次有请求落到某台服务器管的文件上时才启动它，之后常驻。同一个工作区的所有对话共用一台——冷启动的 `rust-analyzer` 要花实打实的时间建索引，这份代价按项目付一次，不是按对话。应用退出时一起停掉。

所以一个项目里的第一次调用可能很慢，而且在服务器还在建索引时合理地返回空。过一会儿再问一次就好，之后它就是热的。

## 编辑之后的问题

模型写或改了一个**正在运行**的语言服务器持有的文件时，Mework 会通知那台服务器。它报出来的问题会在模型下一步开始时作为一个块送到：

```text
<new-diagnostics>The following new diagnostic issues were detected:

main.rs:
  Error [Line 10:5] cannot find value `x` [E0425] (rustc)</new-diagnostics>
```

同一个问题只报一次，不会每一步重复。一个块最多每文件 10 条、总共 30 条，所以一次大规模重命名不会把对话淹掉。不能编辑文件的对话不会收到任何注入；条目上写 `"diagnostics": false` 可以只关掉这半边，导航照常。

这不能代替构建和测试：它只是语言服务器本来就知道的东西，不等它、到了就送。

## 端到端验证

1. 先在终端确认命令能跑（`rust-analyzer --version`、`gopls version`）。它必须在 Mework 自己启动时继承到的 `PATH` 上，所以安装改变了你的 `PATH` 时要重启应用。
2. 打开「代码语义导航」，在对应语言的项目里问一句需要它的话——「`foo` 定义在哪」。第一次可能要等；返回空就再问一次。
3. 如果调用回答 “No LSP server available for file type”，说明没有条目声明这个扩展名：检查条目的 `extensionToLanguage`，以及文件确实是带顶层 `lspServers` 键的 `lsp.json`。

## 排查

| 现象 | 原因 |
|---|---|
| “No LSP server available for file type: .x” | 没有条目声明这个扩展名。往 `lsp.json` 里加一条，或者装一台预设里的服务器。 |
| 某语言的预设从不被使用 | 命令没装，或者不在应用继承到的 `PATH` 上。装好之后重启 Mework 才能拿到变化后的 `PATH`。 |
| `lsp.json` 里的某条条目从不被使用 | 它被拒绝了：`command`、`args`、`env` 或 `workspaceFolder` 里的某个 `${VAR}` 既没有值也没有 `:-默认值`；`extensionToLanguage` 一条映射都没有；裸 `command` 里带空格；或者 `"transport": "socket"`。 |
| 一个文件里的条目全都不被使用 | 文件不是合法 JSON，或者顶层键不是 `lspServers`——从别的客户端抄来时写成 `languageServers` 是最常见的错误。 |
| 刚启动就问，结果是空的 | 服务器还在建索引。再问一次。 |
| “The language server did not answer … within 30 seconds” | 服务器卡住了，或者项目非常大。导航请求有固定 30 秒的期限；`startupTimeout` 动不了它。看服务器自己的日志。 |
| “The language server '…' failed to initialize” | 握手没有完成：进程启动即死，或者 `initialize` 没能在 `startupTimeout` 内应答。只是慢的话，调大 `startupTimeout`。 |
| “its stdout is desynchronized from the base protocol” | 服务器把日志写到了 stdout 而不是 stderr。多数服务器有对应的开关，查它的文档。 |
| `target/`、`node_modules/` 里的定义从来不出现 | 被 git 忽略的文件里的命中是**有意**丢掉的：留着只会把你要的那条挤出去。 |
| 什么都没有，而且文件很大 | 超过 10 MB 的文件不会送给语言服务器。 |
