import type {
  AgentDefinition,
  ApiProvider,
  AppDocument,
  ConversationPreset,
  ConversationWebSearchSettings,
  ModelProfile,
  ShellBackend,
  ToolDescriptor
} from "./types";
import { NATIVE_FETCH_TOOLS, NATIVE_SEARCH_TOOLS } from "./types";
import {
  DEFAULT_SEARCH_COMPRESSION_CUTOFF,
  DEFAULT_SEARCH_MAX_RESULTS,
  SEARCH_PROVIDERS
} from "./lib/searchProviders";
import { defaultAppearancePreferences } from "./lib/appearance";
import {
  CLAUDE_AGENT_PROVIDER_FAMILY,
  CLAUDE_AGENT_PROVIDER_NAME,
  CLAUDE_AGENT_REGISTRY
} from "./lib/claudeAgentProvider";
import { CODEX_PROVIDER_FAMILY, CODEX_PROVIDER_NAME } from "./lib/codexProvider";
import { backendOfTool, knownShells, preferredBackend } from "./lib/machineShells";
import { isHostDerivedToolName } from "./lib/taskTools";
import { createId } from "./lib/id";

export const toolCatalog: ToolDescriptor[] = [
  {
    name: "ls",
    label: "列出文件",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [
      { name: "path", label: "目录", type: "string", required: true, defaultValue: ".", placeholder: "." },
      { name: "depth", label: "递归深度", type: "number", required: false, defaultValue: 1, help: "0 仅列出当前目录" }
    ]
  },
  {
    name: "grep",
    label: "搜索内容",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [
      { name: "pattern", label: "搜索内容", type: "string", required: true, placeholder: "TODO|FIXME" },
      { name: "path", label: "范围", type: "string", required: false, defaultValue: "." },
      { name: "case_sensitive", label: "区分大小写", type: "boolean", required: false, defaultValue: false }
    ]
  },
  {
    name: "powershell",
    label: "PowerShell",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: [
      { name: "command", label: "命令", type: "multiline", required: true, placeholder: "Get-ChildItem -Force" },
      { name: "description", label: "说明", type: "string", required: false, placeholder: "列出当前目录的文件" },
      { name: "timeout", label: "超时（毫秒）", type: "number", required: false, placeholder: "120000" },
      { name: "run_in_background", label: "后台运行", type: "boolean", required: false, defaultValue: false }
    ]
  },
  {
    name: "bash",
    label: "Bash",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: [
      { name: "command", label: "命令", type: "multiline", required: true, placeholder: "git status --short" },
      { name: "description", label: "说明", type: "string", required: false, placeholder: "查看工作树状态" },
      { name: "timeout", label: "超时（毫秒）", type: "number", required: false, placeholder: "120000" },
      { name: "run_in_background", label: "后台运行", type: "boolean", required: false, defaultValue: false }
    ]
  },
  {
    name: "zsh",
    label: "zsh",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: [
      { name: "command", label: "命令", type: "multiline", required: true, placeholder: "ls -la" },
      { name: "description", label: "说明", type: "string", required: false, placeholder: "列出当前目录的文件" },
      { name: "timeout", label: "超时（毫秒）", type: "number", required: false, placeholder: "120000" },
      { name: "run_in_background", label: "后台运行", type: "boolean", required: false, defaultValue: false }
    ]
  },
  {
    name: "sh",
    label: "sh",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: [
      { name: "command", label: "命令", type: "multiline", required: true, placeholder: "ls -la" },
      { name: "description", label: "说明", type: "string", required: false, placeholder: "列出当前目录的文件" },
      { name: "timeout", label: "超时（毫秒）", type: "number", required: false, placeholder: "120000" },
      { name: "run_in_background", label: "后台运行", type: "boolean", required: false, defaultValue: false }
    ]
  },
  {
    name: "write",
    label: "写入文件",
    description: "",
    category: "filesystem",
    dangerous: true,
    parameters: [
      { name: "path", label: "文件路径", type: "string", required: true, placeholder: "src/example.ts" },
      { name: "content", label: "文件内容", type: "multiline", required: true }
    ]
  },
  {
    name: "edit",
    label: "编辑文件",
    description: "",
    category: "filesystem",
    dangerous: true,
    parameters: [
      { name: "path", label: "文件路径", type: "string", required: true },
      { name: "find", label: "查找内容", type: "multiline", required: true },
      { name: "replace", label: "替换为", type: "multiline", required: true }
    ]
  },
  {
    name: "find",
    label: "查找文件",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [
      { name: "query", label: "文件名模式", type: "string", required: true, placeholder: "*.tsx" },
      { name: "path", label: "目录", type: "string", required: false, defaultValue: "." }
    ]
  },
  {
    name: "read",
    label: "读取文件",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [
      { name: "path", label: "文件路径", type: "string", required: true },
      { name: "start_line", label: "起始行", type: "number", required: false, defaultValue: 1 },
      { name: "end_line", label: "结束行", type: "number", required: false }
    ]
  },
  {
    name: "lsp",
    label: "代码语义导航",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [
      { name: "operation", label: "操作", type: "string", required: true, placeholder: "goToDefinition", help: "九种之一：goToDefinition、findReferences、hover、documentSymbol、workspaceSymbol、goToImplementation、prepareCallHierarchy、incomingCalls、outgoingCalls" },
      { name: "filePath", label: "文件路径", type: "string", required: true },
      { name: "line", label: "行号", type: "number", required: true, help: "从 1 开始，与编辑器显示的一致" },
      { name: "character", label: "列号", type: "number", required: true, help: "从 1 开始，与编辑器显示的一致" },
      { name: "query", label: "符号名", type: "string", required: false, placeholder: "LspRegistry", help: "只用于 workspaceSymbol；空查询在多数语言服务器上没有结果" }
    ]
  },
  {
    name: "web_search",
    label: "联网搜索",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "query", label: "查询", type: "string", required: true, placeholder: "Anthropic Claude 4.5 发布日期", help: "一句自足的查询；不要用代词指代上文，长问题拆成多次检索" }
    ]
  },
  {
    name: "web_fetch",
    label: "抓取网页",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "urls", label: "网址", type: "json", required: true, placeholder: "[\"https://example.com/docs/changelog\"]", help: "一批绝对 http(s) 地址；不知道地址时先用联网搜索" }
    ]
  },
  {
    name: "preview_start",
    label: "启动预览",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "name", label: "名称", type: "string", required: true, placeholder: "dev", help: ".mework/launch.json 里的服务器名称" }
    ]
  },
  {
    name: "preview_stop",
    label: "停止预览",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: true, help: "要停止的服务器 ID" }
    ]
  },
  {
    name: "preview_list",
    label: "预览列表",
    description: "",
    category: "web",
    dangerous: false,
    parameters: []
  },
  {
    name: "preview_logs",
    label: "服务器日志",
    description: "",
    category: "web",
    dangerous: false,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "level", label: "级别", type: "string", required: false, defaultValue: "all", help: "按级别过滤：all（默认）返回全部输出，error 只返回含 error、exception、failed 或 fatal 的行" },
      { name: "lines", label: "行数上限", type: "number", required: false, defaultValue: 50, help: "最多返回行数（默认 50）" },
      { name: "search", label: "文本过滤", type: "string", required: false, placeholder: "[DEBUG]", help: "只保留包含该文本的行（例如 [DEBUG]、POST /api）" }
    ]
  },
  {
    name: "preview_console_logs",
    label: "控制台日志",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "level", label: "级别", type: "string", required: false, defaultValue: "all", help: "按级别过滤：all（默认）、error（只看错误）、warn（警告加错误）" },
      { name: "lines", label: "行数上限", type: "number", required: false, defaultValue: 50, help: "最多返回行数（默认 50，上限 200）" },
    ]
  },
  {
    name: "preview_screenshot",
    label: "页面截图",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "scale", label: "缩放", type: "number", required: false, help: "返回图像的缩放系数，取值 0.1 到 1；图像越小消耗的 token 越少。preview_click 用的是 preview_snapshot 给出的元素 UID，而不是像素坐标" }
    ]
  },
  {
    name: "preview_snapshot",
    label: "页面快照",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
    ]
  },
  {
    name: "preview_inspect",
    label: "检查元素",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "selector", label: "CSS Selector", type: "string", required: true, placeholder: ".button", help: "要检查的元素 CSS 选择器" },
      { name: "styles", label: "CSS 属性", type: "json", required: false, placeholder: "[\"padding\",\"color\"]", help: "要返回的 CSS 属性名数组；不给时返回一组常用属性" },
    ]
  },
  {
    name: "preview_click",
    label: "点击元素",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "selector", label: "CSS Selector", type: "string", required: true, placeholder: "button.primary", help: "要点击的元素 CSS 选择器" },
      { name: "doubleClick", label: "双击", type: "boolean", required: false, help: "改为双击" },
    ]
  },
  {
    name: "preview_fill",
    label: "填写输入",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "selector", label: "CSS Selector", type: "string", required: true, placeholder: "input[name=email]", help: "要填写的输入框 CSS 选择器" },
      { name: "value", label: "值", type: "string", required: true, help: "要填入的值" },
    ]
  },
  {
    name: "preview_eval",
    label: "执行脚本",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "expression", label: "表达式", type: "multiline", required: true, placeholder: "document.title", help: "在页面上下文里求值的 JavaScript 表达式；返回值按 JSON 序列化" }
    ]
  },
  {
    name: "preview_network",
    label: "网络请求",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "filter", label: "过滤", type: "string", required: false, defaultValue: "all", help: "过滤：all（默认）返回全部请求，failed 只返回 4xx、5xx 与网络错误；给了 requestId 时本项被忽略" },
      { name: "requestId", label: "请求 ID", type: "string", required: false, help: "给出时返回该请求的响应正文，而不是列出全部请求；requestId 取自列表输出" }
    ]
  },
  {
    name: "preview_resize",
    label: "调整视口",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "preset", label: "设备预设", type: "string", required: false, help: "设备预设；给出时覆盖 width 与 height。desktop 清除尺寸模拟，回到面板自身的响应式尺寸" },
      { name: "width", label: "宽度", type: "number", required: false, placeholder: "1280", help: "视口宽度，单位 CSS 像素（需同时给 height）" },
      { name: "height", label: "高度", type: "number", required: false, placeholder: "720", help: "视口高度，单位 CSS 像素（需同时给 width）" },
      { name: "colorScheme", label: "配色方案", type: "string", required: false, help: "模拟 prefers-color-scheme 媒体特性，用于测试深色与浅色" }
    ]
  },
  {
    name: "preview_upload_image",
    label: "上传图片",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "image_id", label: "图片编号", type: "string", required: true, placeholder: "3", help: "对话里的图片编号，如 3、#3 或 [Image #3]；也接受 64 位十六进制摘要" },
      { name: "selector", label: "CSS Selector", type: "string", required: false, placeholder: "input[type=file]", help: "目标 file 输入框的 CSS 选择器；不给时用页面上第一个 file 输入框" },
      { name: "filename", label: "文件名", type: "string", required: false, placeholder: "photo.png", help: "页面看到的文件名；不给时用附件原名" }
    ]
  },
  {
    name: "preview_dialog",
    label: "回答对话框",
    description: "",
    category: "web",
    dangerous: true,
    parameters: [
      { name: "serverId", label: "服务器 ID", type: "string", required: false, help: "服务器 ID" },
      { name: "accept", label: "接受", type: "boolean", required: false, defaultValue: true, help: "true 接受对话框，false 取消（默认 true）" },
      { name: "prompt_text", label: "Prompt 输入", type: "string", required: false, help: "prompt 对话框的输入，仅在接受时生效" }
    ]
  },
  {
    name: "agent_spawn",
    label: "子代理",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "prompt", label: "任务", type: "multiline", required: true, placeholder: "调查 src/ 下的路由结构并总结关键文件", help: "默认子代理看不到当前对话，任务描述必须自包含全部背景" },
      { name: "agent_type", label: "命名类型", type: "string", required: false, placeholder: "code-reviewer", help: "可选的可信命名定义短名称；可用的名称与用途列在本轮的可用 Agent 清单里。由宿主解析，不能与 context=conversation 同时使用" },
      { name: "name", label: "名称", type: "string", required: true, placeholder: "review-api", help: "必填。用于 send_message / followup_task / task_wait 寻址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名" },
      { name: "label", label: "显示名", type: "string", required: false, placeholder: "调查路由", help: "显示在时间线上的短名称" },
      { name: "context", label: "初始上下文", type: "string", required: false, defaultValue: "none", help: "none（默认）：只看到任务；conversation：携带当前对话历史副本" },
      { name: "schema", label: "输出模式", type: "json", required: false, placeholder: "{\"type\":\"object\",\"properties\":{\"verdict\":{\"type\":\"string\"}},\"required\":[\"verdict\"]}", help: "可选的 JSON Schema 子集；给出后子代理必须调用 structured_output 交回符合该模式的结果，返回值会随 task_wait 一起回来。顶层必须是 type 为 object 的对象模式；支持 type、properties、required、items、enum、const、additionalProperties、minItems/maxItems、minLength/maxLength、minimum/maximum，其余关键字会被当场拒绝" }
    ]
  },
  {
    name: "send_message",
    label: "发送消息",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "target", label: "子代理", type: "string", required: true, placeholder: "a1", help: "agent_spawn 返回的名称" },
      { name: "message", label: "消息", type: "multiline", required: true }
    ]
  },
  {
    name: "followup_task",
    label: "追加任务",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "target", label: "子代理", type: "string", required: true, placeholder: "a1", help: "agent_spawn 返回的名称" },
      { name: "message", label: "消息", type: "multiline", required: true }
    ]
  },
  {
    name: "task_wait",
    label: "等待任务",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "tasks", label: "任务列表", type: "json", required: false, placeholder: "[\"a1\", \"terminal:t1\"]", help: "任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，开发服务器写 preview:<serverId>；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与开发服务器）" },
      { name: "timeout_seconds", label: "超时秒数", type: "number", required: false, defaultValue: 60, help: "5–600 秒，默认 60" }
    ]
  },
  {
    name: "task_list",
    label: "任务列表",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "box",
    label: "后台结果",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "skill",
    label: "技能",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      {
        name: "name",
        label: "技能名",
        type: "string",
        required: true,
        placeholder: "commit-helper",
        help: "本对话已选技能的名字，取自 schema 的 enum"
      }
    ]
  },
  {
    name: "tool_search",
    label: "工具发现",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      {
        name: "query",
        label: "查询",
        type: "string",
        required: true,
        placeholder: "select:mcp__github__create_issue",
        help: "`select:<名字>[,<名字>…]` 按名取，或者用关键词搜索"
      },
      {
        name: "max_results",
        label: "最多返回",
        type: "number",
        required: false,
        defaultValue: 5,
        placeholder: "5",
        help: "关键词搜索最多返回几个工具；按名取时不生效"
      }
    ]
  },
  {
    name: "read_global_memory",
    label: "读取全局记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "构建环境", help: "记忆索引里列出的文档名，.md 后缀可写可不写" }
    ]
  },
  {
    name: "read_project_memory",
    label: "读取项目记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "构建环境", help: "记忆索引里列出的文档名，.md 后缀可写可不写" }
    ]
  },
  {
    name: "create_global_memory",
    label: "创建全局记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "用户偏好", help: "memory 目录下的单个文档名，不能包含路径分隔符；.md 后缀可写可不写" },
      { name: "content", label: "记忆内容", type: "multiline", required: true, help: "这份记忆的完整 Markdown 正文" },
      { name: "description", label: "索引描述", type: "string", required: true, placeholder: "用户长期偏好的语言与代码风格", help: "一句话说明这份记忆记录了什么，会写进 MEMORY.md 索引供以后判断要不要读取" }
    ]
  },
  {
    name: "create_project_memory",
    label: "创建项目记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "构建环境", help: "memory 目录下的单个文档名，不能包含路径分隔符；.md 后缀可写可不写" },
      { name: "content", label: "记忆内容", type: "multiline", required: true, help: "这份记忆的完整 Markdown 正文" },
      { name: "description", label: "索引描述", type: "string", required: true, placeholder: "测试必须用项目自带环境运行", help: "一句话说明这份记忆记录了什么，会写进 MEMORY.md 索引供以后判断要不要读取" }
    ]
  },
  {
    name: "edit_global_memory",
    label: "编辑全局记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "用户偏好", help: "要修改的记忆文档名，.md 后缀可写可不写" },
      { name: "old_text", label: "原文", type: "multiline", required: true, help: "文档中要被替换的原文，必须唯一匹配；不唯一时请提供更长的片段" },
      { name: "new_text", label: "新文本", type: "multiline", required: true, help: "替换后的文本；留空表示删除这段内容" },
      { name: "description", label: "索引描述", type: "string", required: true, placeholder: "用户长期偏好的语言与代码风格", help: "修改后这份记忆的一句话说明，会刷新 MEMORY.md 索引里的对应条目" }
    ]
  },
  {
    name: "edit_project_memory",
    label: "编辑项目记忆",
    description: "",
    category: "memory",
    dangerous: false,
    parameters: [
      { name: "name", label: "文档名", type: "string", required: true, placeholder: "构建环境", help: "要修改的记忆文档名，.md 后缀可写可不写" },
      { name: "old_text", label: "原文", type: "multiline", required: true, help: "文档中要被替换的原文，必须唯一匹配；不唯一时请提供更长的片段" },
      { name: "new_text", label: "新文本", type: "multiline", required: true, help: "替换后的文本；留空表示删除这段内容" },
      { name: "description", label: "索引描述", type: "string", required: true, placeholder: "测试必须用项目自带环境运行", help: "修改后这份记忆的一句话说明，会刷新 MEMORY.md 索引里的对应条目" }
    ]
  },
  {
    name: "workflow",
    label: "工作流",
    description: "",
    category: "orchestration",
    dangerous: true,
    parameters: [
      { name: "script", label: "编排脚本", type: "multiline", required: true, placeholder: "export const meta = { name: \"…\", description: \"…\" }\n…", help: "以 `export const meta = {…}` 开头的 JS 编排脚本：用 agent()/parallel()/pipeline()/phase()/log() 派生并组织步骤子代理，正文的 return 值就是运行结果。给某一步加 { isolation: \"worktree\" } 会为它单开一棵从 HEAD 检出的 git 工作树（看不到未提交改动；留下改动就保留，没改动就自动拆除）" },
      { name: "name", label: "名称", type: "string", required: true, placeholder: "review-sweep", help: "必填。这次运行在会话代理命名空间里的地址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名，续跑也要换新名字" },
      { name: "args", label: "输入参数", type: "json", required: false, help: "原样暴露给脚本的 JSON 值（全局 args）；数组与对象直接传，不要编码成字符串" },
      { name: "token_budget", label: "token 预算", type: "number", required: false, help: "本次运行允许消耗的 token 硬顶，脚本经 budget 读到；耗尽后新的 agent() 调用抛错" },
      { name: "resume_run_id", label: "续跑运行 ID", type: "string", required: false, placeholder: "run0a1b2c3d", help: "上一次同脚本运行报出的运行 ID；已入日志的步骤即时重放，其余步骤重跑。脚本正文必须与获批时逐字一致" }
    ]
  },
  {
    name: "todo",
    label: "待办事项",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "action", label: "动作", type: "string", required: true, placeholder: "create", help: "选择操作：create、update、get、list" },
      { name: "taskId", label: "任务 ID", type: "string", required: false, placeholder: "task-1", help: "用于 update、get；create 返回的不透明 ID" },
      { name: "subject", label: "任务标题", type: "string", required: false, placeholder: "补全用户认证", help: "create 必填；update 可选" },
      { name: "description", label: "任务说明", type: "multiline", required: false, placeholder: "实现登录与注册接口，并补齐测试。", help: "create 必填；update 可选" },
      { name: "activeForm", label: "进行中文案", type: "string", required: false, placeholder: "正在补全用户认证", help: "create、update；任务处于 in_progress 时显示的简短进行时文案" },
      { name: "status", label: "状态", type: "string", required: false, placeholder: "in_progress", help: "用于 update；pending | in_progress | completed | deleted" },
      { name: "owner", label: "负责人", type: "string", required: false, help: "仅用于 update" },
      { name: "addBlocks", label: "新增被阻塞任务", type: "json", required: false, help: "用于 update；由本任务阻塞的 task ID 数组" },
      { name: "addBlockedBy", label: "新增前置任务", type: "json", required: false, help: "用于 update；阻塞本任务的 task ID 数组" },
      { name: "metadata", label: "元数据", type: "json", required: false, help: "create 整体写入；update 按 key 合并，值为 null 时删除该 key" }
    ]
  },
  {
    name: "ask_user",
    label: "提问",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      {
        name: "questions",
        label: "问题",
        type: "json",
        required: true,
        placeholder: "[{\"question\":\"采用哪个方案？\",\"header\":\"实现方案\",\"options\":[{\"label\":\"方案 A\",\"description\":\"保持改动最小\"},{\"label\":\"方案 B\",\"description\":\"完整重构\"}],\"multiSelect\":false}]",
        help: "Claude Code AskUserQuestion 格式：1–4 题；每题含 header、question、2–4 个 label/description 选项及 multiSelect；无需添加 Other"
      }
    ]
  },
  {
    name: "fork",
    label: "分叉会话",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      {
        name: "prompt",
        label: "提示词",
        type: "multiline",
        required: true,
        placeholder: "在分叉出的会话里要完成的任务",
        help: "分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景"
      }
    ]
  },
  {
    name: "plan",
    label: "计划文档",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: [
      { name: "action", label: "动作", type: "string", required: true, placeholder: "write", help: "选择操作：write 写入或覆盖计划、read 读取当前计划" },
      { name: "content", label: "计划正文", type: "string", required: false, help: "write 必填；计划的 Markdown 正文，整篇覆盖上一版" }
    ]
  },
  {
    name: "exit_plan_mode",
    label: "退出计划模式",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
];

function freshToolCatalog(): ToolDescriptor[] {
  return toolCatalog.map((tool) => ({
    ...tool,
    parameters: tool.parameters.map((parameter) => ({
      ...parameter,
      defaultValue: parameter.defaultValue === undefined
        ? undefined
        : JSON.parse(JSON.stringify(parameter.defaultValue))
    }))
  }));
}

export const CODEX_PRESET_ID = "preset_codex";
export const CLAUDE_CODE_PRESET_ID = "preset_claude_code";

/** The template id a shipped preset opens with. Keep in sync with
 * `catalog.rs::seeded_template_id`. The body lives in the host's SQLite store,
 * written once by `storage::seed_preset_templates`, so in the browser preview
 * and in vitest these ids dangle — which every reader treats as "no template". */
export const CODEX_TEMPLATE_ID = "template_preset_codex";
export const CLAUDE_CODE_TEMPLATE_ID = "template_preset_claude_code";

/** Mirrors `catalog.rs::builtin_claude_agent_provider`: the whole registry minus
 * the `[1m]` twins, which are the same models under a CLI-only 1M context
 * budget and would double the picker for a distinction most users never make. */
function claudeAgentSeedModels(): ModelProfile[] {
  return CLAUDE_AGENT_REGISTRY
    .filter((model) => !model.id.includes("[1m]"))
    .map(({ id, name, contextWindow, maxOutputTokens }) => ({
      id,
      name,
      group: "claude",
      contextWindow,
      maxOutputTokens,
      capabilities: ["image_recognition"],
      reasoningContent: "plaintext",
      promptCache: true
    }));
}

/** Keep in sync with `src-tauri/src/catalog.rs::product_default_api_providers`.
 * IDs stay random per installation because they key credential storage; the
 * renderer's `ensureCodexProvider` / `ensureClaudeAgentProvider` claim these
 * rows by family, so the IDs a seeded preset was written against survive. */
function seedProviders(): ApiProvider[] {
  const blank = {
    baseUrl: "",
    familySettings: {},
    endpointBaseUrls: {},
    notes: ""
  } as const;
  const claudeAgentModels = claudeAgentSeedModels();
  return [
    {
      id: createId("provider"),
      name: CODEX_PROVIDER_NAME,
      // Signed out until the user completes the OAuth flow, and its catalog
      // lives behind that login, so it ships no models.
      enabled: false,
      family: CODEX_PROVIDER_FAMILY,
      ...blank,
      models: [],
      activeModelId: null
    },
    {
      id: createId("provider"),
      name: CLAUDE_AGENT_PROVIDER_NAME,
      enabled: true,
      family: CLAUDE_AGENT_PROVIDER_FAMILY,
      ...blank,
      models: claudeAgentModels,
      activeModelId: claudeAgentModels[0]?.id ?? null
    }
  ];
}

/** One seeded role: everything on, thinking on, nothing said about itself.
 * `tools: null` rather than an allowlist, so the role tracks whatever its preset
 * enables instead of freezing today's catalog into six copies. */
function seedAgentDefinition(
  name: string,
  providerId: string,
  modelId: string
): AgentDefinition {
  return {
    enabled: true,
    deleted: false,
    name,
    description: "",
    source: "user",
    sourceKey: "",
    revision: 1,
    memoryEpoch: 1,
    modelSelection: { kind: "explicit", providerId, modelId },
    memory: "none",
    effort: "medium",
    tools: null,
    disallowedTools: [],
    searchProvider: null,
    fetchProvider: null,
    maxResults: DEFAULT_SEARCH_MAX_RESULTS,
    compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
    domainFilter: null,
    includeDomains: [],
    excludeDomains: [],
    templateId: null
  };
}

/**
 * The one shell a fresh install's presets turn on: this machine's most
 * preferred. The host probes for it at first launch
 * (`storage::seed_local_shell`); this seed, which has no probe to ask, reads
 * the OS off the platform string and takes that OS's first shell.
 */
function seedShellBackend(platform: string): ShellBackend {
  const { os, backends } = knownShells(null, {}, platform);
  return (os ? preferredBackend(os, backends) : null) ?? backends[0] ?? "bash";
}

/**
 * Everything in the catalog except the names the host derives for itself and
 * every shell but `shell`. Mirrors `catalog.rs::seed_preset_enabled_tools`
 * narrowed by `storage::seed_local_shell`.
 *
 * The memory tools follow the two memory switches, `skill` follows
 * `skillToolEnabled`, the two web tools follow `webSearchEnabled`, and the task
 * tools appear once something can produce a task, so none of them is named
 * here.
 */
function seedPresetEnabledTools(tools: readonly ToolDescriptor[], shell: ShellBackend): string[] {
  return tools
    .map((tool) => tool.name)
    .filter((name) => {
      if (isHostDerivedToolName(name)) return false;
      const backend = backendOfTool(name);
      return backend === null || backend === shell;
    });
}

function seedPreset(
  id: string,
  name: string,
  enabledTools: string[],
  templateId: string,
  webSearch: ConversationWebSearchSettings,
  agentDefinitions: AgentDefinition[]
): ConversationPreset {
  return {
    id,
    name,
    description: "",
    // The body lives in the host store, written once at first launch; this id
    // dangles in the browser preview, which reads as "no template".
    templateId,
    settings: {
      // The host has no default prompt of its own; a fresh conversation sends
      // only its capability sections until the user writes one.
      enabledTools,
      toolDescriptionFileId: null,
      agentDefinitions,
      // Every child is one of the three named roles, so the model cannot route
      // around them by spawning an anonymous one.
      allowRolelessSubagents: false,
      // The host mints these from the absolute paths of the capability files it
      // writes at first launch, so the renderer seed cannot know them and ships
      // none. Hooks stay unselected even there: a dangling hook id fails every
      // run closed, and the built-ins are meant to be deletable.
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      webSearch,
      // These presets mirror CLIs that search the web, so web access is on;
      // which of the two web tools that grants follows the resolved backend.
      webSearchEnabled: true,
      securityLevel: "request_approval",
      // The memory tools are switched by these two rather than named in the list.
      globalMemoryEnabled: true,
      projectMemoryEnabled: true,
      // Both capability surfaces load on demand rather than inlining every
      // selected body and every MCP schema into the system prompt.
      skillToolEnabled: true,
      mcpToolDiscoveryEnabled: true
    }
  };
}

/** Keep in sync with `src-tauri/src/catalog.rs::product_default_presets`. The
 * Codex roles are bound to models that do not exist until the user signs in and
 * fetches the catalog; the binding waits rather than being discarded, and the
 * role starts working the moment its model shows up.
 *
 * The two web legs differ because the families differ. Search is native for
 * both. Fetch is native only for Codex, whose family folds retrieval into its
 * one `web_search` tool. The Claude Agent family supports neither native leg,
 * so the Claude Code preset ships no fetch backend at all. */
function seedPresets(
  providers: readonly ApiProvider[],
  tools: readonly ToolDescriptor[],
  platform: string
): ConversationPreset[] {
  const enabledTools = () => seedPresetEnabledTools(tools, seedShellBackend(platform));
  const providerId = (family: string) =>
    providers.find((provider) => provider.family === family)?.id ?? "";
  const codex = providerId(CODEX_PROVIDER_FAMILY);
  const claudeAgent = providerId(CLAUDE_AGENT_PROVIDER_FAMILY);
  const webSearch = (
    fetchProvider: ConversationWebSearchSettings["fetchProvider"]
  ): ConversationWebSearchSettings => ({
    maxSearchesPerCall: 0,
    provider: { kind: "native" },
    fetchProvider,
    // Factory presets ship the basic versions. A newer one is a choice with its
    // own costs, not a default to hand every new conversation.
    nativeSearchTool: NATIVE_SEARCH_TOOLS[0],
    nativeFetchTool: NATIVE_FETCH_TOOLS[0],
    maxResults: DEFAULT_SEARCH_MAX_RESULTS,
    compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
    // Factory presets filter nothing and carry no rules to filter with. The
    // lists are the user's to write, and a shipped one would be this
    // application deciding what the web is allowed to say.
    domainFilter: "off",
    includeDomains: [],
    excludeDomains: []
  });
  return [
    seedPreset(CODEX_PRESET_ID, "Codex", enabledTools(), CODEX_TEMPLATE_ID, webSearch({ kind: "native" }), [
      seedAgentDefinition("sol", codex, "gpt-5.6-sol"),
      seedAgentDefinition("terra", codex, "gpt-5.6-terra"),
      seedAgentDefinition("luna", codex, "gpt-5.6-luna")
    ]),
    seedPreset(
      CLAUDE_CODE_PRESET_ID,
      "Claude Code",
      enabledTools(),
      CLAUDE_CODE_TEMPLATE_ID,
      webSearch({ kind: "native" }),
      [
        seedAgentDefinition("opus", claudeAgent, "claude-opus-5"),
        seedAgentDefinition("sonnet", claudeAgent, "claude-sonnet-5"),
        seedAgentDefinition("haiku", claudeAgent, "claude-haiku-4-5")
      ]
    )
  ];
}

export const createSeedDocument = (
  platform: string = globalThis.navigator?.platform ?? ""
): AppDocument => {
  const tools = freshToolCatalog();
  const now = new Date().toISOString();
  const apiProviders = seedProviders();
  // The one built-in that works without a sign-in, so it is what a fresh
  // install talks to.
  const activeProvider = apiProviders.find((provider) => provider.enabled) ?? null;

  return {
    schemaVersion: 3,
    globalSettings: {
      appLanguage: "auto",
      resolvedAppLanguage: "zh-CN",
      theme: "system",
      conversationPresets: seedPresets(apiProviders, tools, platform),
      // Storage refuses an empty default once presets exist, so this is written
      // explicitly rather than left to the normalizer's first-preset fallback.
      defaultConversationPresetId: CLAUDE_CODE_PRESET_ID,
      lastReasoningEffort: "disabled",
      apiProviders,
      activeProviderId: activeProvider?.id ?? null,
      webSearch: {
        // Keep this list synchronized item-by-item with the Rust seed in
        // `src-tauri/src/catalog.rs::product_default_document`. Enable only the
        // anonymous `exa-mcp` search and `jina` fetch providers by default; the others require user API keys.
        providers: SEARCH_PROVIDERS.map((provider) => ({
          kind: provider.kind,
          enabled: provider.kind === "exa-mcp" || provider.kind === "jina",
          searchApiHost: "",
          fetchApiHost: "",
          engines: [],
          basicAuthUsername: ""
        }))
      },
      appearance: defaultAppearancePreferences(),
      // SSH machines and run-environment variables require explicit user creation.
      executionEnvironments: { sshMachines: [], envVars: {} },
      // An empty object uses the default bindings in `src/lib/shortcuts.ts`.
      shortcuts: {},
      environmentTools: []
    },
    tools,
    capabilities: {
      hooks: [],
      skills: [],
      mcps: [],
      toolDescriptionFiles: []
    },
    workspaces: [
      // A fresh installation has no directory workspace; the renderer opens a
      // draft with none selected and its first message lands in the temporary one.
      {
        id: "__temporary__",
        name: "临时工作区",
        kind: "temporary",
        path: "",
        createdAt: now,
        defaultConversationPresetId: "",
        lastConversationSettings: null,
        conversations: []
      }
    ]
  };
};
