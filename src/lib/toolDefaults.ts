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
  clear: "Clear",
  command: "Command",
  content: "File content",
  context: "Initial context",
  depth: "Recursion depth",
  description: "",
  double: "Double-click",
  end_line: "End line",  effort: "Effort level",
  schema: "Output schema",
  fields: "Form fields",
  filter: "URL filter",
  find: "Find text",
  full_page: "Full page",
  height: "Height",
  inherit_context: "Inherit context",
  key: "Key",
  label: "Display name",
  languages: "Languages",
  limit: "Result limit",
  load: "Wait for load",
  max_chars: "Maximum characters",
  message: "Message",
  metadata: "Metadata",
  modifiers: "Modifier keys",
  name: "Name",
  owner: "Owner",
  only_errors: "Errors only",
  output_format: "Result shape",
  path: "Path",
  paths: "File paths",
  pattern: "Search pattern",
  plan: "Plan",
  prompt: "Prompt",
  prompt_text: "Prompt text",
  query: "File name pattern",
  questions: "Questions",
  ref: "Element ref",
  repeat: "Repeat count",
  replace: "Replacement",
  recency: "Recency window",
  research_depth: "Research depth",
  resume_run_id: "Resume run ID",
  script: "JavaScript",
  selector: "CSS selector",
  slowly: "Type key by key",
  start_line: "Start line",
  status: "Status",
  subject: "Task subject",
  submit: "Submit after typing",
  tab: "Tab ID",
  taskId: "Task ID",
  tasks: "Tasks",
  target: "Child agent",
  text: "Text",
  text_gone: "Text to disappear",
  timeout_ms: "Timeout (ms)",
  timeout_seconds: "Timeout (seconds)",
  url: "URL",
  values: "Option values",
  width: "Width",
  x: "Horizontal distance",
  y: "Vertical distance"
};

const englishParameterHelp: Record<string, string> = {
  "0 仅列出当前目录": "0 lists only the current directory.",
  "本对话已选技能的名字，取自 schema 的 enum":
    "Name of a skill this conversation selected; the schema lists them as an enum.",
  "navigate、snapshot、click、type、fill_form、select、hover、key、scroll、evaluate、wait、screenshot、console、network、dialog、file_upload、upload_image、resize、tab_new、tab_list、tab_select、tab_close、close":
    "navigate, snapshot, click, type, fill_form, select, hover, key, scroll, evaluate, wait, screenshot, console, network, dialog, file_upload, upload_image, resize, tab_new, tab_list, tab_select, tab_close, close.",
  "navigate、tab_new；navigate 还接受 back / forward / reload":
    "navigate, tab_new. navigate also accepts back / forward / reload.",
  "snapshot；范围 1000–60000": "snapshot; range 1,000-60,000.",
  "click、type、select、hover、scroll、wait、screenshot、file_upload、upload_image":
    "click, type, select, hover, scroll, wait, screenshot, file_upload, upload_image.",
  "同 selector 的动作；两者任选其一，scroll、screenshot 以及文件选择器打开时的 file_upload、upload_image 可都不给":
    "Same actions as selector; provide either one. scroll and screenshot may provide neither.",
  "click；left | right | middle": "click; left | right | middle.",
  "click；双击": "click; double-click.",
  "click；组合键数组:Control、Shift、Alt、Meta":
    "click; array of modifier keys: Control, Shift, Alt, Meta.",
  "type 要输入的文本；wait 要等待出现的页面文本":
    "type: the text to enter. wait: page text that must appear.",
  "type 先清空原值（默认 true）；console、network 读取后清空（默认 false）":
    "type: clear the current value first (default true). console, network: clear captured entries after reading (default false).",
  "type；输入后按 Enter": "type; press Enter after typing.",
  "type；逐键输入": "type; dispatch individual key events.",
  "fill_form；数组,每项含 selector 或 ref,配 value、values 或 checked;最多 50 项":
    "fill_form; array of up to 50 fields. Each item has selector or ref plus value, values, or checked.",
  "select；字符串或字符串数组": "select; a string or an array of strings.",
  "key；重复次数 1–50": "key; repeat count: 1-50.",
  "scroll；水平滚动量": "scroll; horizontal scroll amount.",
  "scroll；垂直滚动量": "scroll; vertical scroll amount.",
  "evaluate；支持 await;结尾表达式即返回值。给 ref 时把该元素绑定为 element 变量":
    "evaluate; supports await and the final expression becomes the return value. With ref, that element is bound to the element variable.",
  "wait；等待文本消失": "wait; text that must disappear.",
  "wait；等到 document.readyState 为 complete;可与其他条件同时使用":
    "wait; wait until document.readyState is complete. Can be combined with the other conditions.",
  "wait；最大 30000": "wait; maximum 30,000.",
  "screenshot；工作区内的 .png 路径": "screenshot; a workspace-relative .png path.",
  "screenshot；截取整页而不是视口": "screenshot; capture the entire page instead of the viewport.",
  "console；只看错误": "console; return errors only.",
  "console（默认 100）、network（默认 50）；最多返回条数 1–200":
    "console (default 100), network (default 50); maximum result count 1-200.",
  "network；URL 子串过滤": "network; filter by a URL substring.",
  "dialog；回答当前打开的对话框:true 接受,false 取消（默认 true）":
    "dialog; answer the open dialog: true accepts, false dismisses (default true).",
  "dialog；prompt 对话框的输入,仅在接受时生效": "dialog; the answer for an open prompt dialog, used only when accepting.",
  "file_upload；工作区内路径,字符串或数组,最多 10 个":
    "file_upload; a workspace-relative path or an array of up to 10 paths.",
  "upload_image；对话中的图片编号,如 3、#3 或 [Image #3]":
    "upload_image; the conversation image number, e.g. 3, #3, or [Image #3].",
  "upload_image；页面看到的文件名;默认用附件原名":
    "upload_image; the file name the page sees. Defaults to the attachment's own name.",
  "resize；范围 320–7680": "resize; range 320-7,680.",
  "resize；范围 240–4320": "resize; range 240-4,320.",
  "tab_select、tab_close；tab_list 返回的 tab 值,main 是本对话自己的页面":
    "tab_select, tab_close; a tab value returned by tab_list. main is the conversation's own page.",
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
  "分叉会话的第一条用户消息；说清任务与需要的背景":
    "First user message of the forked conversation; state the task and the background it needs",
  "true 复制到目前为止的时间线与已完成任务；false 只带这条提示词":
    "true copies the timeline so far and the completed tasks; false starts with only this prompt",
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
    "The run ID reported by a previous run of the same plan; journaled steps replay instantly and the rest re-run. The plan body must be byte-identical to the approved one."
};

const englishParameterPlaceholders: Record<string, string> = {
  "[{\"ref\":\"e3\",\"value\":\"张三\"},{\"ref\":\"e5\",\"checked\":true}]": "[{\"ref\":\"e3\",\"value\":\"Alex\"},{\"ref\":\"e5\",\"checked\":true}]",
  "Enter 或 Control+L": "Enter or Control+L",
  "加载中": "Loading",
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
  web_search: "Web search",
  workflow: "Workflow",
  playwright: "Browser",
  agent_spawn: "Subagent",
  send_message: "Send message",
  followup_task: "Follow up",
  task_wait: "Wait for tasks",
  task_list: "List tasks",
  ask_user: "Ask user",
  fork: "Fork conversation",
  todo: "Todo",
  skill: "Skill"
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
