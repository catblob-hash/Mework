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
  activeForm: "Active form",
  addBlockedBy: "Add blockers",
  addBlocks: "Add blocked tasks",
  allowed_domains: "Allowed domains",
  blocked_domains: "Blocked domains",
  button: "Mouse button",
  case_sensitive: "Case sensitive",
  character: "Character",
  clear: "Clear",
  colorScheme: "Color scheme",
  command: "Command",
  content: "File content",
  context: "Initial context",
  depth: "Recursion depth",
  description: "",
  double: "Double-click",
  doubleClick: "Double-click",
  end_line: "End line",  effort: "Effort level",
  expression: "Expression",
  schema: "Output schema",
  fields: "Form fields",
  filter: "Filter",
  filePath: "File path",
  find: "Find text",
  full_page: "Full page",
  height: "Height",
  key: "Key",
  label: "Display name",
  languages: "Languages",
  level: "Level",
  line: "Line",
  limit: "Result limit",
  lines: "Line limit",
  load: "Wait for load",
  max_chars: "Maximum characters",
  max_results: "Maximum results",
  message: "Message",
  metadata: "Metadata",
  modifiers: "Modifier keys",
  name: "Name",
  owner: "Owner",
  only_errors: "Errors only",
  operation: "Operation",
  output_format: "Result shape",
  path: "Path",
  paths: "File paths",
  pattern: "Search pattern",
  plan: "Plan",
  preset: "Device preset",
  prompt: "Prompt",
  prompt_text: "Prompt text",
  query: "File name pattern",
  questions: "Questions",
  ref: "Element ref",
  repeat: "Repeat count",
  replace: "Replacement",
  requestId: "Request ID",
  recency: "Recency window",
  research_depth: "Research depth",
  resume_run_id: "Resume run ID",
  script: "JavaScript",
  scale: "Scale",
  search: "Text filter",
  selector: "CSS selector",
  serverId: "Server ID",
  slowly: "Type key by key",
  start_line: "Start line",
  status: "Status",
  styles: "CSS properties",
  subject: "Task subject",
  submit: "Submit after typing",
  tab: "Tab ID",
  taskId: "Task ID",
  task: "Task address",
  tasks: "Tasks",
  source: "Log source",
  target: "Child agent",
  text: "Text",
  text_gone: "Text to disappear",
  threshold: "Score threshold",
  timeout_ms: "Timeout (ms)",
  timeout_seconds: "Timeout (seconds)",
  url: "URL",
  value: "Value",
  values: "Option values",
  width: "Width",
  x: "Horizontal distance",
  y: "Vertical distance"
};

const englishParameterHelp: Record<string, string> = {
  "0 仅列出当前目录": "0 lists only the current directory.",
  "用自然语言描述要找的文件；目录列表会切块送给决策模型打分":
    "Describe the files to look for in plain language; the directory listing is cut into groups the decision model scores.",
  "用自然语言描述要找的内容；文件会切块送给决策模型打分":
    "Describe what to look for in plain language; the file is cut into chunks the decision model scores.",
  "task_list 里的 shell 任务地址，如 shell:3": "The shell task address from task_list, e.g. shell:3.",
  "用自然语言描述要在输出里找的内容；输出会切块送给决策模型打分":
    "Describe what to look for in the output in plain language; the output is cut into chunks the decision model scores.",
  "用自然语言描述要在命令输出里找的内容；输出会切块送给决策模型打分，只返回过阈值的片段":
    "Describe what to look for in the command's output in plain language; the output is cut into chunks the decision model scores and only pieces above the threshold are returned.",
  "用自然语言描述要找的页面元素；页面的可访问性快照会切块送给决策模型打分":
    "Describe the page element to look for in plain language; the page's accessibility snapshot is cut into chunks the decision model scores.",
  "用自然语言描述要在日志里找的内容；Console 日志和服务器日志会切块送给决策模型打分":
    "Describe what to look for in the logs in plain language; the console and server logs are cut into chunks the decision model scores.",
  "要搜索的日志：all（默认）同时搜 Console 与服务器日志，console 只搜页面 Console，server 只搜服务器输出":
    "Which logs to search: 'all' (default) searches both the page console and the dev server output, 'console' only the page console, 'server' only the server output.",
  "用自然语言描述要操作的元素；宿主把页面元素交给决策模型选出一个并直接操作":
    "Describe the element to act on in plain language; the host hands the page's elements to the decision model, which picks one, and acts on it directly.",
  "0 到 1，最多三位小数；只返回分数不低于它的片段":
    "0 to 1 with at most three decimals; only pieces scoring at or above it are returned.",
  "从 1 开始": "1-based.",
  "包含该行；留空到文件末尾": "Inclusive; leave empty for the end of the file.",
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
  "必填。用于 send_message / followup_task / task_wait 寻址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名": "Required. Name the child yourself: it is the address send_message / followup_task / task_wait take, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree.",
  "必填。这次运行在会话代理命名空间里的地址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名，续跑也要换新名字": "Required. This run's address in the same namespace agents are named in, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree, so a resume still needs a fresh one.",
  "显示在时间线上的短名称": "Short name shown in the timeline.",
  "none（默认）：只看到任务；conversation：携带当前对话历史副本": "none (default): task only; conversation: include a copy of the current conversation history.",
  "agent_spawn 返回的名称": "Name returned by agent_spawn.",
  "任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，浏览器页面写 browser:<tab>；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与浏览器）": "Array of task addresses: a child agent or workflow run by its bare name (a workflow also answers to workflow:<runId>), a background command as shell:<id>, a terminal as terminal:<id>, a browser page as browser:<tab>. The wait ends once every named task has produced a result. Omit to wait for every child agent, workflow run and background command in this conversation (terminals and browser pages excluded).",
  "5–600 秒，默认 60": "5–600 seconds; default: 60.",
  "Claude Code AskUserQuestion 格式：1–4 题；每题含 header、question、2–4 个 label/description 选项及 multiSelect；无需添加 Other": "Claude Code AskUserQuestion format: 1–4 questions, each with header, question, 2–4 label/description options, and multiSelect. Do not add Other.",
  "分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景":
    "First user message of the forked conversation, and your only chance to instruct it; state the task and all the background it needs",
  "可选，默认 false：子对话只带这条 prompt 开始；true 时把目前为止的时间线与已完成任务一并复制进去":
    "Optional, default false: the child starts with only this prompt; true also copies the timeline so far and the completed tasks",
  "选择操作：create、update、get、list": "create, update, get, list.",
  "选择操作：create、update、get": "create, update, get.",
  "用于 update、get；create 返回的不透明 ID": "update, get; the opaque ID returned by create.",
  "create 必填；update 可选": "Required by create; optional patch field for update.",
  "create、update；任务处于 in_progress 时显示的简短进行时文案":
    "create, update; short present-continuous text shown while the task is in_progress.",
  "用于 update；pending | in_progress | completed | deleted":
    "update; pending | in_progress | completed | deleted",
  "仅用于 update": "update.",
  "用于 update；由本任务阻塞的 task ID 数组": "update; array of task IDs that this task blocks.",
  "用于 update；阻塞本任务的 task ID 数组": "update; array of task IDs that block this task.",
  "create 整体写入；update 按 key 合并，值为 null 时删除该 key":
    "create writes the whole object; update merges keys, and a null value removes that key.",
  "必须是自足的目标而不是关键词串：写清要回答的问题、已知背景和完成标准": "A self-contained goal, not a keyword string: the questions to answer, what is already known, and what counts as done.",
  "可选；描述希望拿到的结果结构": "Optional; describe the result structure you want.",
  "只有这些域名会被搜索或打开；与 blocked_domains 互斥": "Only these domains may be searched or opened; mutually exclusive with blocked_domains.",
  "这些域名永不被搜索或打开；与 allowed_domains 互斥": "These domains are never searched or opened; mutually exclusive with allowed_domains.",
  "传给搜索后端的时效窗口": "Recency window passed to the search backend.",
  "把结果限定在这些语言代码上": "Restrict results to these language codes.",
  "quick | standard | deep；决定执行子代理数量与工具调用预算，是检索预算而不是推理强度": "quick | standard | deep; sets the executor count and the tool-call budget. This is a search budget, not a reasoning-effort level.",
  "声明式计划：phases 依序推进，parallel 全并发，pipeline 逐项流水；步骤靠 inputFrom 引用更早 phase 的 label 才能拿到它的产出": "Declarative plan: phases run in order; parallel fans out; pipeline streams per item. A step sees an earlier phase's output only by naming its label in inputFrom.",
  "暴露给计划的 JSON 值；首个 pipeline phase 的 items 数组来自这里": "JSON value exposed to the plan; a leading pipeline phase takes its items array from here.",
  "上一次同计划运行报出的运行 ID；已入日志的步骤即时重放，其余步骤重跑。计划正文必须与获批时逐字一致":
    "The run ID reported by a previous run of the same plan; journaled steps replay instantly and the rest re-run. The plan body must be byte-identical to the approved one.",
  "选择操作：write 写入或覆盖计划、read 读取当前计划":
    "Choose an action: write stores or replaces the plan, read returns the current one.",
  "write 必填；计划的 Markdown 正文，整篇覆盖上一版":
    "Required for write; the plan's Markdown body, which replaces the previous one in full."
};

const englishParameterPlaceholders: Record<string, string> = {
  "处理登录失败的代码": "The code that handles a failed login",
  "存放提供商凭据的代码": "The code that stores provider credentials",
  "shell:3": "shell:3",
  "有没有编译错误": "Are there compile errors",
  "测试失败的原因": "Why the tests failed",
  "顶部导航里的登录按钮": "The login button in the top navigation",
  "有没有关于 hydration 的报错": "Any errors about hydration",
  "查清 X 的当前状态：需要回答哪些问题、已知什么、什么算答完了": "Establish the current state of X: which questions to answer, what is already known, and what counts as done",
  "对比表 / 时间线 / 清单 / 直接答案": "Comparison table / timeline / list / direct answer",
  "调查 src/ 下的路由结构并总结关键文件": "Inspect routing under src/ and summarize the key files",
  "调查路由": "Inspect routing",
  "在分叉出的会话里要完成的任务": "The task to complete in the forked conversation",
  "补全用户认证": "Implement user authentication",
  "实现登录与注册接口，并补齐测试。": "Add login and signup endpoints and cover them with tests.",
  "正在补全用户认证": "Implementing user authentication",
  "所有认证测试通过，且 lint 无错误": "All authentication tests pass and lint is clean",
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
  write: "Write file",
  edit: "Edit file",
  find: "Find files",
  read: "Read file",
  find_content: "Find content",
  find_files: "Find files by description",
  find_output: "Find command output",
  bash_find_output: "Bash, scored output",
  powershell_find_output: "PowerShell, scored output",
  lsp: "Code navigation",
  web_search: "Web search",
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
  preview_find_element: "Find page elements by description",
  preview_find_logs: "Find logs by description",
  preview_click_by_description: "Click element by description",
  preview_fill_by_description: "Fill element by description",
  preview_inspect_by_description: "Inspect element by description",
  agent_spawn: "Subagent",
  send_message: "Send message",
  followup_task: "Follow up",
  task_wait: "Wait for tasks",
  task_list: "List tasks",
  box: "Background result",
  ask_user: "Ask user",
  fork: "Fork conversation",
  todo: "Todo",
  skill: "Skill",
  tool_search: "Tool discovery",
  plan: "Plan document",
  exit_plan_mode: "Exit plan mode"
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

export function hasEnglishToolDefault(toolName: string): boolean {
  return  Object.hasOwn(englishToolLabels, toolName);
}
