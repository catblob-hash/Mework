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
  case_sensitive: "Case sensitive",
  character: "Character",
  colorScheme: "Color scheme",
  command: "Command",
  content: "File content",
  context: "Initial context",
  depth: "Recursion depth",
  description: "",
  doubleClick: "Double-click",
  end_line: "End line",  effort: "Effort level",
  expression: "Expression",
  schema: "Output schema",
  filter: "Filter",
  filePath: "File path",
  find: "Find text",
  height: "Height",
  label: "Display name",
  level: "Level",
  line: "Line",
  lines: "Line limit",
  max_results: "Maximum results",
  message: "Message",
  metadata: "Metadata",
  name: "Name",
  owner: "Owner",
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
  script: "JavaScript",
  scale: "Scale",
  search: "Text filter",
  selector: "CSS selector",
  serverId: "Server ID",
  start_line: "Start line",
  status: "Status",
  styles: "CSS properties",
  subject: "Task subject",
  taskId: "Task ID",
  task: "Task address",
  tasks: "Tasks",
  source: "Log source",
  target: "Child agent",
  threshold: "Score threshold",
  timeout_seconds: "Timeout (seconds)",
  value: "Value",
  width: "Width"
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
  "用自然语言描述要在 Console 里找的内容；按 level 过滤后的日志送给决策模型打分，只返回分数不低于 threshold 的片段。需在对话设置里为本工具开启决策模型参数":
    "Describe what to look for in the console in plain language; the logs that pass level go to the decision model, and only pieces scoring at or above threshold come back. Needs this tool's decision-model parameters turned on in the conversation settings.",
  "用自然语言描述要找的页面元素；可访问性快照送给决策模型打分，只返回分数不低于 threshold 的元素及其选择器，而不是整份快照。需在对话设置里为本工具开启决策模型参数":
    "Describe the page element to look for in plain language; the accessibility snapshot goes to the decision model, and only elements scoring at or above threshold come back with their selectors, instead of the whole snapshot. Needs this tool's decision-model parameters turned on in the conversation settings.",
  "用自然语言描述要点击的元素，代替 selector；决策模型从页面元素里选出所指的一个并点击，都不符合时会选「以上皆非」，什么也不做。需在对话设置里为本工具开启决策模型参数":
    "Describe the element to click in plain language, instead of selector; the decision model chooses the one meant and it is clicked, or answers none of the above and nothing is done. Needs this tool's decision-model parameters turned on in the conversation settings.",
  "用自然语言描述要填写的输入框，代替 selector；决策模型从页面元素里选出所指的一个并填写，都不符合时会选「以上皆非」，什么也不做。需在对话设置里为本工具开启决策模型参数":
    "Describe the input to fill in plain language, instead of selector; the decision model chooses the one meant and it is filled, or answers none of the above and nothing is done. Needs this tool's decision-model parameters turned on in the conversation settings.",
  "用自然语言描述要检查的元素，代替 selector；决策模型从页面元素里选出所指的一个并读取它的样式，都不符合时会选「以上皆非」，按未找到回答。需在对话设置里为本工具开启决策模型参数":
    "Describe the element to inspect in plain language, instead of selector; the decision model chooses the one meant and its styles are read, or answers none of the above and the element is reported as not found. Needs this tool's decision-model parameters turned on in the conversation settings.",
  "0 到 1，最多三位小数；与 query 一起给出，只返回分数不低于它的片段":
    "0 to 1 with at most three decimals, given with query; only pieces scoring at or above it are returned.",
  "0 到 1，最多三位小数；与 query 一起给出，只返回分数不低于它的元素":
    "0 to 1 with at most three decimals, given with query; only pieces scoring at or above it are returned.",
  "用自然语言描述要在日志里找的内容；Console 日志和服务器日志会切块送给决策模型打分":
    "Describe what to look for in the logs in plain language; the console and server logs are cut into chunks the decision model scores.",
  "要搜索的日志：all（默认）同时搜 Console 与服务器日志，console 只搜页面 Console，server 只搜服务器输出":
    "Which logs to search: 'all' (default) searches both the page console and the dev server output, 'console' only the page console, 'server' only the server output.",
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
  "要检查的元素 CSS 选择器；开启决策模型参数后也可以改用 query":
    "CSS selector (e.g., '.button', '#header'); with decision-model parameters on, query can name the element instead",
  "要返回的 CSS 属性名数组；不给时返回一组常用属性":
    "CSS properties to return (e.g., ['padding', 'color']). Defaults to common properties.",
  "要点击的元素 CSS 选择器；开启决策模型参数后也可以改用 query":
    "CSS selector for the element to click; with decision-model parameters on, query can name the element instead",
  "改为双击": "Perform a double-click",
  "要填写的输入框 CSS 选择器；开启决策模型参数后也可以改用 query":
    "CSS selector for the input element; with decision-model parameters on, query can name the element instead",
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
  "5–600 秒，默认 60": "5–600 seconds; default: 60.",
  "Claude Code AskUserQuestion 格式：1–4 题；每题含 header、question、2–4 个 label/description 选项及 multiSelect；无需添加 Other": "Claude Code AskUserQuestion format: 1–4 questions, each with header, question, 2–4 label/description options, and multiSelect. Do not add Other.",
  "分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景":
    "First user message of the forked conversation, and your only chance to instruct it; state the task and all the background it needs",
  "选择操作：create、update、get、list": "create, update, get, list.",
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
  "对话框里的保存按钮": "The Save button in the dialog",
  "邮箱输入框": "The email field",
  "调查 src/ 下的路由结构并总结关键文件": "Inspect routing under src/ and summarize the key files",
  "调查路由": "Inspect routing",
  "在分叉出的会话里要完成的任务": "The task to complete in the forked conversation",
  "补全用户认证": "Implement user authentication",
  "实现登录与注册接口，并补齐测试。": "Add login and signup endpoints and cover them with tests.",
  "正在补全用户认证": "Implementing user authentication",
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
  find_content: "Find content by description",
  find_files: "Find files by description",
  find_output: "Find output by description",
  bash_find_output: "Bash, scored output",
  powershell_find_output: "PowerShell, scored output",
  zsh_find_output: "zsh, scored output",
  sh_find_output: "sh, scored output",
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
  preview_find_logs: "Find logs by description",
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
