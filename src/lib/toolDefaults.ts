import type {
  ResolvedAppLanguage,
  ToolDescriptor,
  ToolParameter
} from "../types";

const englishParameterLabels: Record<string, string> = {
  agent_type: "Named agent type",
  accept: "Accept",
  action: "Action",
  args: "Arguments",
  case_sensitive: "Case sensitive",
  character: "Character",
  colorScheme: "Color scheme",
  command: "Command",
  content: "File content",
  context: "Initial context",
  depth: "Recursion depth",
  description: "Description",
  doubleClick: "Double-click",
  effort: "Effort level",
  end_line: "End line",
  expression: "Expression",
  schema: "Output schema",
  filename: "File name",
  filter: "Filter",
  filePath: "File path",
  find: "Find text",
  height: "Height",
  image_id: "Image number",
  label: "Display name",
  level: "Level",
  limit: "Limit",
  line: "Line",
  lines: "Line limit",
  max_results: "Maximum results",
  name: "Name",
  new_text: "New text",
  none: "Empty argument",
  offset: "Offset",
  old_text: "Original text",
  operation: "Operation",
  path: "Path",
  pattern: "Search pattern",
  plan: "Plan",
  preset: "Device preset",
  prompt: "Prompt",
  prompt_text: "Prompt text",
  query: "File name pattern",
  questions: "Questions",
  replace: "Replacement",
  requestId: "Request ID",
  resume_run_id: "Resume run ID",
  run_in_background: "Run in background",
  script: "JavaScript",
  scale: "Scale",
  search: "Text filter",
  selector: "CSS selector",
  serverId: "Server ID",
  start_line: "Start line",
  styles: "CSS properties",
  tasks: "Tasks",
  timeout: "Timeout (ms)",
  timeout_seconds: "Timeout (seconds)",
  token_budget: "Token budget",
  urls: "URLs",
  value: "Value",
  width: "Width"
};

const englishParameterHelp: Record<string, string> = {
  "一句自足的查询；不要用代词指代上文，长问题拆成多次检索":
    "One self-contained query; no pronouns pointing back at the conversation. Break a long question into several searches.",
  "一批绝对 http(s) 地址；不知道地址时先用联网搜索":
    "Absolute http(s) page URLs. Search first when you do not know the URL.",
  "任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，开发服务器写 preview:<serverId>（对话有多个工作区时写 preview:<serverId>@<工作区编号>）；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与开发服务器）":
    "Array of task addresses: a child agent or workflow run by its bare name (a workflow also answers to workflow:<runId>), a background command as shell:<id>, a terminal as terminal:<id>, a dev server as preview:<serverId> (preview:<serverId>@<workspace> when the conversation has several workspaces). The wait ends once every named task has produced a result. Omit to wait for every child agent, workflow run and background command in this conversation (terminals and dev servers excluded).",
  "以 `export const meta = {…}` 开头的 JS 编排脚本：用 agent()/parallel()/pipeline()/phase()/log() 派生并组织步骤子代理，正文的 return 值就是运行结果。给某一步加 { isolation: \"worktree\" } 会为它单开一棵从 HEAD 检出的 git 工作树（看不到未提交改动；留下改动就保留，没改动就自动拆除）":
    "JavaScript orchestration starting with `export const meta = {…}`: spawn steps with agent()/parallel()/pipeline(), narrate with phase()/log(); the body's return value is the run result. Adding { isolation: \"worktree\" } to a step gives it its own git worktree checked out from HEAD (uncommitted changes are not in it; a step that leaves changes keeps its worktree, one that changes nothing has it removed).",
  "原样暴露给脚本的 JSON 值（全局 args）；数组与对象直接传，不要编码成字符串":
    "JSON value exposed to the script as the global `args`, verbatim; pass arrays and objects directly, not as encoded strings.",
  "本次运行允许消耗的 token 硬顶，脚本经 budget 读到；耗尽后新的 agent() 调用抛错":
    "Hard token ceiling for this run, readable as budget in the script; once exhausted, further agent() calls throw.",
  "上一次运行报出的运行 ID；提示词与选项没变的步骤即时重放，上次停下时还在跑的步骤单独重跑，从第一个改动或失败的步骤起其余全部重跑。可省略脚本与 args 沿用上次的；传入改过的脚本会重新审批":
    "Run id from a previous run; steps whose prompt and options are unchanged replay, a step the last attempt left running re-runs alone, and everything from the first changed or failed step re-runs. script and args may be omitted to reuse the last ones; an edited script is approved again.",
  "记忆索引里列出的文档名，.md 后缀可写可不写":
    "A document name listed in the memory index. The .md suffix is optional.",
  "memory 目录下的单个文档名，不能包含路径分隔符；.md 后缀可写可不写":
    "A single document name inside the memory directory. Path separators are forbidden; the .md suffix is optional.",
  "要修改的记忆文档名，.md 后缀可写可不写": "The memory document to modify. The .md suffix is optional.",
  "这份记忆的完整 Markdown 正文": "The document's complete Markdown body.",
  "文档中要被替换的原文，必须唯一匹配；不唯一时请提供更长的片段":
    "The passage to replace. It must match exactly once; supply a longer excerpt when it is not unique.",
  "替换后的文本；留空表示删除这段内容": "The replacement text. Leave it empty to delete the passage.",
  "一句话说明这份记忆记录了什么，会写进 MEMORY.md 索引供以后判断要不要读取":
    "One sentence describing what this memory records. It is written into the MEMORY.md index so a later run can decide whether to read the document.",
  "修改后这份记忆的一句话说明，会刷新 MEMORY.md 索引里的对应条目":
    "One sentence describing this memory after the change. It refreshes the document's entry in the MEMORY.md index.",
  "0 仅列出当前目录": "0 lists only the current directory.",
  "最多返回的匹配行，默认 250，至多 1000": "Most matching lines to return: 250 by default, at most 1,000.",
  "先跳过这么多匹配行，用于翻到下一页": "Matching lines to skip first, to fetch the next page.",
  "本对话已选技能的名字，取自 schema 的 enum":
    "Name of a skill this conversation selected; the schema lists them as an enum.",
  "`select:<名字>[,<名字>…]` 按名取，或者用关键词搜索":
    "`select:<name>[,<name>…]` to fetch exact tools, or keywords to search for them.",
  "关键词搜索最多返回几个工具；按名取时不生效":
    "How many tools a keyword search may return; ignored when fetching by name.",
  ".mework/launch.json 里的服务器名称": "Server name from .mework/launch.json.",
  "要停止的服务器 ID": "Server ID to stop",
  "服务器 ID": "Server ID",
  "按级别过滤：all（默认）返回全部输出，error 只返回含 error、exception、failed 或 fatal 的行":
    "Filter by level: 'all' (default) shows all output, 'error' shows only lines containing error/exception/failed/fatal",
  "最多返回行数（默认 50）": "Max lines to return (default: 50)",
  "只保留包含该文本的行（例如 [DEBUG]、POST /api）":
    "Filter to lines containing this text (e.g., '[DEBUG]', 'POST /api')",
  "九种之一：goToDefinition、findReferences、hover、documentSymbol、workspaceSymbol、goToImplementation、prepareCallHierarchy、incomingCalls、outgoingCalls":
    "One of nine: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls.",
  "从 1 开始，与编辑器显示的一致": "1-based, as shown in editors.",
  "只用于 workspaceSymbol；空查询在多数语言服务器上没有结果":
    "workspaceSymbol only; most language servers return nothing for an empty query.",
  "按级别过滤：all（默认）、error（只看错误）、warn（警告加错误）":
    "Filter by level: 'all' (default), 'error' (errors only), 'warn' (warnings + errors)",
  "最多返回行数（默认 50，上限 200）": "Max lines to return (default: 50, max: 200)",
  "返回图像的缩放系数，取值 0.1 到 1；图像越小消耗的 token 越少。preview_click 用的是 preview_snapshot 给出的元素 UID，而不是像素坐标":
    "Scale factor in [0.1, 1] for the returned image; smaller images use fewer tokens. preview_click uses element UIDs from preview_snapshot, not pixel coordinates.",
  "要检查的元素 CSS 选择器": "CSS selector (e.g., '.button', '#header')",
  "要返回的 CSS 属性名数组；不给时返回一组常用属性":
    "CSS properties to return (e.g., ['padding', 'color']). Defaults to common properties.",
  "要点击的元素 CSS 选择器": "CSS selector for the element to click",
  "改为双击": "Perform a double-click",
  "要填写的输入框 CSS 选择器": "CSS selector for the input element",
  "要填入的值": "Value to fill",
  "在页面上下文里求值的 JavaScript 表达式；返回值按 JSON 序列化":
    "JavaScript expression to evaluate in the page context. Return values are serialized as JSON.",
  "过滤：all（默认）返回全部请求，failed 只返回 4xx、5xx 与网络错误；给了 requestId 时本项被忽略":
    "Filter: 'all' (default) shows all requests, 'failed' shows only 4xx/5xx and network errors. Ignored when requestId is provided.",
  "给出时返回该请求的响应正文，而不是列出全部请求；requestId 取自列表输出":
    "If provided, returns the response body for this specific request instead of listing all requests. Get requestIds from the listing output.",
  "设备预设；给出时覆盖 width 与 height。desktop 清除尺寸模拟，回到面板自身的响应式尺寸":
    "Device preset. Overrides width/height if provided. \"desktop\" clears the size emulation (back to the pane's responsive size).",
  "视口宽度，单位 CSS 像素（需同时给 height）": "Viewport width in CSS pixels (requires height)",
  "视口高度，单位 CSS 像素（需同时给 width）": "Viewport height in CSS pixels (requires width)",
  "模拟 prefers-color-scheme 媒体特性，用于测试深色与浅色":
    "Emulate prefers-color-scheme media feature for dark/light mode testing.",
  "对话里的图片编号，如 3、#3 或 [Image #3]；也接受 64 位十六进制摘要":
    "The conversation image number, e.g. 3, #3, or [Image #3]. A 64-character hex digest also resolves.",
  "目标 file 输入框的 CSS 选择器；不给时用页面上第一个 file 输入框":
    "CSS selector of the target file input. Omitted, the first file input on the page is used.",
  "页面看到的文件名；不给时用附件原名": "The file name the page sees. Defaults to the attachment's own name.",
  "true 接受对话框，false 取消（默认 true）": "true accepts the open dialog, false dismisses it (default true).",
  "prompt 对话框的输入，仅在接受时生效": "The answer for an open prompt dialog, used only when accepting.",
  "默认子代理看不到当前对话，任务描述必须自包含全部背景": "Child agents do not see this conversation by default; include all required context in the task.",
  "可选的 JSON Schema 子集；给出后子代理必须调用 structured_output 交回符合该模式的结果，返回值会随 task_wait 一起回来。顶层必须是 type 为 object 的对象模式；支持 type、properties、required、items、enum、const、additionalProperties、minItems/maxItems、minLength/maxLength、minimum/maximum，其余关键字会被当场拒绝": "Optional JSON Schema subset. When set, the child must call structured_output with a result matching it, and that value comes back with task_wait. The top level must be an object schema; type, properties, required, items, enum, const, additionalProperties, minItems/maxItems, minLength/maxLength and minimum/maximum are supported and every other keyword is rejected on the spot.",
  "可选的可信命名定义短名称；可用的名称与用途列在本轮的可用 Agent 清单里。由宿主解析，不能与 context=conversation 同时使用": "Optional trusted definition slug; the available names and what each is for are listed in this turn's available-agents context. Resolved by the host; cannot be combined with context=conversation.",
  "必填。用于 task_wait 寻址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名": "Required. Name the child yourself: it is the address task_wait takes, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree.",
  "必填。这次运行在会话代理命名空间里的地址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名，续跑也要换新名字": "Required. This run's address in the same namespace agents are named in, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree, so a resume still needs a fresh one.",
  "显示在时间线上的短名称": "Short name shown in the timeline.",
  "none（默认）：只看到任务；conversation：携带当前对话历史副本": "none (default): task only; conversation: include a copy of the current conversation history.",
  "5–600 秒，默认 60": "5–600 seconds; default: 60.",
  "Claude Code AskUserQuestion 格式：1–4 题；每题含 header、question、2–4 个 label/description 选项及 multiSelect；无需添加 Other": "Claude Code AskUserQuestion format: 1–4 questions, each with header, question, 2–4 label/description options, and multiSelect. Do not add Other.",
  "分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景":
    "First user message of the forked conversation, and your only chance to instruct it; state the task and all the background it needs",
  "选择操作：write 写入或覆盖计划、read 读取当前计划":
    "Choose an action: write stores or replaces the plan, read returns the current one.",
  "write 必填；计划的 Markdown 正文，整篇覆盖上一版":
    "Required for write; the plan's Markdown body, which replaces the previous one in full.",
  "交接索引里列出的文档名，.md 后缀可写可不写":
    "A note name listed in the handoff index. The .md suffix is optional.",
  "交接文档名，不能包含路径分隔符；.md 后缀可写可不写":
    "The handoff note's name. Path separators are forbidden; the .md suffix is optional.",
  "要修改的交接文档名，.md 后缀可写可不写": "The handoff note to modify. The .md suffix is optional.",
  "交接文档的完整 Markdown 正文": "The note's complete Markdown body.",
  "一句话说明这份交接文档写了什么，会写进交接索引":
    "One sentence describing what this handoff note holds. It is written into the handoff index.",
  "修改后这份交接文档的一句话说明，会刷新交接索引里的对应条目":
    "One sentence describing this handoff note after the change. It refreshes the note's entry in the handoff index.",
  "始终为空数组": "Always an empty list."
};

const englishParameterPlaceholders: Record<string, string> = {
  "查看工作树状态": "Show working tree status",
  "列出当前目录的文件": "List files in the current directory",
  "Anthropic Claude 4.5 发布日期": "Anthropic Claude 4.5 release date",
  "用户偏好": "user-preferences",
  "构建环境": "build-environment",
  "当前进度": "current-state",
  "任务目标、已完成的工作与下一步": "The goal, the work done and the next step",
  "用户长期偏好的语言与代码风格": "The user's long-standing language and code-style preferences",
  "测试必须用项目自带环境运行": "Tests must run in the project's own environment",
  "调查 src/ 下的路由结构并总结关键文件": "Inspect routing under src/ and summarize the key files",
  "调查路由": "Inspect routing",
  "在分叉出的会话里要完成的任务": "The task to complete in the forked conversation",
  "[{\"question\":\"采用哪个方案？\",\"header\":\"实现方案\",\"options\":[{\"label\":\"方案 A\",\"description\":\"保持改动最小\"},{\"label\":\"方案 B\",\"description\":\"完整重构\"}],\"multiSelect\":false}]":
    "[{\"question\":\"Which approach should I use?\",\"header\":\"Approach\",\"options\":[{\"label\":\"Approach A\",\"description\":\"Keep the change small\"},{\"label\":\"Approach B\",\"description\":\"Perform a full rewrite\"}],\"multiSelect\":false}]"
};

/** English built-in tool labels. Must match Rust's `catalog.rs::english_tool_label`.
 * This contains labels only: tool descriptions (`ToolDescriptor.description`) are always
 * empty in the seed and are neither read nor localized by the frontend. */
const englishToolLabels: Record<string, string> = {
  ls: "List files",
  grep: "Search content",
  powershell: "PowerShell",
  bash: "Bash",
  zsh: "zsh",
  sh: "sh",
  write: "Write file",
  edit: "Edit file",
  find: "Find files",
  read: "Read file",
  lsp: "Code navigation",
  web_search: "Web search",
  web_fetch: "Fetch web pages",
  workflow: "Workflow",
  preview_start: "Start preview",
  preview_stop: "Stop preview",
  preview_list: "List previews",
  preview_logs: "Server logs",
  preview_console_logs: "Console logs",
  preview_screenshot: "Page screenshot",
  preview_snapshot: "Page snapshot",
  preview_inspect: "Inspect element",
  preview_click: "Click element",
  preview_fill: "Fill input",
  preview_eval: "Run script",
  preview_network: "Network requests",
  preview_resize: "Resize viewport",
  preview_upload_image: "Upload image",
  preview_dialog: "Answer dialog",
  agent_spawn: "Subagent",
  task_wait: "Wait for tasks",
  task_list: "List tasks",
  box: "Background result",
  read_global_memory: "Read global memory",
  read_project_memory: "Read project memory",
  create_global_memory: "Create global memory",
  create_project_memory: "Create project memory",
  edit_global_memory: "Edit global memory",
  edit_project_memory: "Edit project memory",
  ask_user: "Ask user",
  fork: "Fork conversation",
  skill: "Skill",
  tool_search: "Tool discovery",
  plan: "Plan document",
  exit_plan_mode: "Exit plan mode",
  read_handoff_note: "Read handoff note",
  create_handoff_note: "Write handoff note",
  edit_handoff_note: "Edit handoff note",
  handoff: "Hand off"
};

function cloneDefaultValue(value: ToolParameter["defaultValue"]): ToolParameter["defaultValue"] {
  if (value === undefined || value === null || typeof value !== "object") return value;
  return JSON.parse(JSON.stringify(value)) as ToolParameter["defaultValue"];
}

function localizeToolParameter(parameter: ToolParameter): ToolParameter {
  const localized: ToolParameter = {
    ...parameter,
    label: englishParameterLabels[parameter.name] ?? parameter.label,
    defaultValue: cloneDefaultValue(parameter.defaultValue)
  };
  if (parameter.help) {
    localized.help = englishParameterHelp[parameter.help] ?? parameter.help;
  }
  if (parameter.placeholder) {
    localized.placeholder = englishParameterPlaceholders[parameter.placeholder] ?? parameter.placeholder;
  }
  return localized;
}

export function localizeToolDescriptor(
  tool: ToolDescriptor,
  language: ResolvedAppLanguage
): ToolDescriptor {
  const label = language === "en-US" ? englishToolLabels[tool.name] : undefined;
  return label
    ? {
        ...tool,
        label,
        parameters: tool.parameters.map((parameter) => localizeToolParameter(parameter))
      }
    : tool;
}
