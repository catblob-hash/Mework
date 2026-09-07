import type {
  AgentDefinition,
  ApiProvider,
  AppDocument,
  ConversationPreset,
  ModelProfile,
  ToolDescriptor
} from "./types";
import { SEARCH_PROVIDERS } from "./lib/searchProviders";
import { defaultAppearancePreferences } from "./lib/appearance";
import { defaultConversationWebSearchSettings } from "./lib/runtime";
import {
  CLAUDE_AGENT_PROVIDER_FAMILY,
  CLAUDE_AGENT_PROVIDER_NAME,
  CLAUDE_AGENT_REGISTRY
} from "./lib/claudeAgentProvider";
import { CODEX_PROVIDER_FAMILY, CODEX_PROVIDER_NAME } from "./lib/codexProvider";
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
    name: "playwright", label: "浏览器", description: "", category: "web", dangerous: true,
    parameters: [
      { name: "action", label: "动作", type: "string", required: true, placeholder: "click", help: "navigate、snapshot、click、type、fill_form、select、hover、key、scroll、evaluate、wait、screenshot、console、network、dialog、file_upload、upload_image、resize、tab_new、tab_list、tab_select、tab_close、close" },
      { name: "url", label: "网址", type: "string", required: false, placeholder: "https://example.com", help: "navigate、tab_new；navigate 还接受 back / forward / reload" },
      { name: "max_chars", label: "最大字符数", type: "number", required: false, defaultValue: 30000, help: "snapshot；范围 1000–60000" },
      { name: "selector", label: "CSS Selector", type: "string", required: false, placeholder: "button[type=submit]", help: "click、type、select、hover、scroll、wait、screenshot、file_upload、upload_image" },
      { name: "ref", label: "元素 Ref", type: "string", required: false, placeholder: "e12", help: "同 selector 的动作；两者任选其一，scroll、screenshot 以及文件选择器打开时的 file_upload、upload_image 可都不给" },
      { name: "button", label: "鼠标按键", type: "string", required: false, defaultValue: "left", help: "click；left | right | middle" },
      { name: "double", label: "双击", type: "boolean", required: false, defaultValue: false, help: "click；双击" },
      { name: "modifiers", label: "修饰键", type: "json", required: false, placeholder: "[\"Control\"]", help: "click；组合键数组:Control、Shift、Alt、Meta" },
      { name: "text", label: "文本", type: "multiline", required: false, help: "type 要输入的文本；wait 要等待出现的页面文本" },
      { name: "clear", label: "清空", type: "boolean", required: false, help: "type 先清空原值（默认 true）；console、network 读取后清空（默认 false）" },
      { name: "submit", label: "输入后提交", type: "boolean", required: false, defaultValue: false, help: "type；输入后按 Enter" },
      { name: "slowly", label: "逐键输入", type: "boolean", required: false, defaultValue: false, help: "type；逐键输入" },
      { name: "fields", label: "表单字段", type: "json", required: false, placeholder: "[{\"ref\":\"e3\",\"value\":\"张三\"},{\"ref\":\"e5\",\"checked\":true}]", help: "fill_form；数组,每项含 selector 或 ref,配 value、values 或 checked;最多 50 项" },
      { name: "values", label: "选项值", type: "json", required: false, help: "select；字符串或字符串数组" },
      { name: "key", label: "按键", type: "string", required: false, placeholder: "Enter 或 Control+L", help: "key" },
      { name: "repeat", label: "重复次数", type: "number", required: false, defaultValue: 1, help: "key；重复次数 1–50" },
      { name: "x", label: "水平距离", type: "number", required: false, defaultValue: 0, help: "scroll；水平滚动量" },
      { name: "y", label: "垂直距离", type: "number", required: false, defaultValue: 600, help: "scroll；垂直滚动量" },
      { name: "script", label: "JavaScript", type: "multiline", required: false, placeholder: "document.title", help: "evaluate；支持 await;结尾表达式即返回值。给 ref 时把该元素绑定为 element 变量" },
      { name: "text_gone", label: "消失文本", type: "string", required: false, placeholder: "加载中", help: "wait；等待文本消失" },
      { name: "load", label: "等待加载完成", type: "boolean", required: false, defaultValue: false, help: "wait；等到 document.readyState 为 complete;可与其他条件同时使用" },
      { name: "timeout_ms", label: "超时毫秒", type: "number", required: false, defaultValue: 5000, help: "wait；最大 30000" },
      { name: "path", label: "保存路径", type: "string", required: false, defaultValue: "browser-screenshot.png", placeholder: "artifacts/page.png", help: "screenshot；工作区内的 .png 路径" },
      { name: "full_page", label: "完整页面", type: "boolean", required: false, defaultValue: false, help: "screenshot；截取整页而不是视口" },
      { name: "only_errors", label: "只看错误", type: "boolean", required: false, defaultValue: false, help: "console；只看错误" },
      { name: "limit", label: "条数上限", type: "number", required: false, help: "console（默认 100）、network（默认 50）；最多返回条数 1–200" },
      { name: "filter", label: "URL 过滤", type: "string", required: false, placeholder: "/api/", help: "network；URL 子串过滤" },
      { name: "accept", label: "接受", type: "boolean", required: false, placeholder: "true", help: "dialog；回答当前打开的对话框:true 接受,false 取消（默认 true）" },
      { name: "prompt_text", label: "Prompt 输入", type: "string", required: false, help: "dialog；prompt 对话框的输入,仅在接受时生效" },
      { name: "paths", label: "文件路径", type: "json", required: false, placeholder: "\"artifacts/report.pdf\"", help: "file_upload；工作区内路径,字符串或数组,最多 10 个" },
      { name: "image_id", label: "图片编号", type: "string", required: false, placeholder: "3", help: "upload_image；对话中的图片编号,如 3、#3 或 [Image #3]" },
      { name: "filename", label: "文件名", type: "string", required: false, placeholder: "photo.png", help: "upload_image；页面看到的文件名;默认用附件原名" },
      { name: "width", label: "宽度", type: "number", required: false, placeholder: "1280", help: "resize；范围 320–7680" },
      { name: "height", label: "高度", type: "number", required: false, placeholder: "720", help: "resize；范围 240–4320" },
      { name: "tab", label: "标签页 id", type: "string", required: false, placeholder: "main", help: "tab_select、tab_close；tab_list 返回的 tab 值,main 是本对话自己的页面" }
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
      { name: "tasks", label: "任务列表", type: "json", required: false, placeholder: "[\"a1\", \"terminal:t1\"]", help: "任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，浏览器页面写 browser:<tab>；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与浏览器）" },
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
      },
      {
        name: "inherit_context",
        label: "继承上下文",
        type: "boolean",
        required: false,
        defaultValue: false,
        help: "可选，默认 false：子对话只带这条 prompt 开始；true 时把目前为止的时间线与已完成任务一并复制进去"
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
  {
    name: "enter_plan_mode",
    label: "进入计划模式",
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
    searchProvider: null
  };
}

function seedPreset(
  id: string,
  name: string,
  tools: readonly ToolDescriptor[],
  agentDefinitions: AgentDefinition[]
): ConversationPreset {
  return {
    id,
    name,
    description: "",
    settings: {
      // The host has no default prompt of its own; a fresh conversation sends
      // only its capability sections until the user writes one.
      systemPrompt: "",
      // Everything except the names the host derives for itself: the memory
      // tools follow the two memory switches, `skill` follows `skillToolEnabled`,
      // and the task tools appear once something can produce a task.
      enabledTools: tools
        .map((tool) => tool.name)
        .filter((name) => !isHostDerivedToolName(name)),
      toolDescriptionFileId: null,
      agentDefinitions,
      // Every child is one of the three named roles, so the model cannot route
      // around them by spawning an anonymous one.
      allowRolelessSubagents: false,
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      webSearch: defaultConversationWebSearchSettings(),
      securityLevel: "request_approval",
      globalMemoryEnabled: false,
      projectMemoryEnabled: false,
      skillToolEnabled: false
    }
  };
}

/** Keep in sync with `src-tauri/src/catalog.rs::product_default_presets`. The
 * Codex roles are bound to models that do not exist until the user signs in and
 * fetches the catalog; the binding waits rather than being discarded, and the
 * role starts working the moment its model shows up. */
function seedPresets(
  providers: readonly ApiProvider[],
  tools: readonly ToolDescriptor[]
): ConversationPreset[] {
  const providerId = (family: string) =>
    providers.find((provider) => provider.family === family)?.id ?? "";
  const codex = providerId(CODEX_PROVIDER_FAMILY);
  const claudeAgent = providerId(CLAUDE_AGENT_PROVIDER_FAMILY);
  return [
    seedPreset(CODEX_PRESET_ID, "Codex", tools, [
      seedAgentDefinition("sol", codex, "gpt-5.6-sol"),
      seedAgentDefinition("terra", codex, "gpt-5.6-terra"),
      seedAgentDefinition("luna", codex, "gpt-5.6-luna")
    ]),
    seedPreset(CLAUDE_CODE_PRESET_ID, "Claude Code", tools, [
      seedAgentDefinition("opus", claudeAgent, "claude-opus-5"),
      seedAgentDefinition("sonnet", claudeAgent, "claude-sonnet-5"),
      seedAgentDefinition("haiku", claudeAgent, "claude-haiku-4-5")
    ])
  ];
}

export const createSeedDocument = (): AppDocument => {
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
      conversationPresets: seedPresets(apiProviders, tools),
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
        })),
        fetchProvider: "jina",
        maxResults: 5,
        excludeDomains: [],
        compression: { method: "cutoff", cutoffLimit: 2000 }
      },
      // MCP servers and skills require explicit user creation or import.
      mcpServers: [],
      skills: [],
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
      {
        id: "ws_default",
        name: "Workspace",
        kind: "directory",
        path: ".",
        createdAt: now,
        defaultConversationPresetId: "",
        lastConversationSettings: null,
        conversations: []
      },
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
