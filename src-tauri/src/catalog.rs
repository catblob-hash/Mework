use std::path::Path;

use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::model::{
    AgentDefinition, AgentDefinitionMemory, AgentDefinitionSource, AgentModelSelection, ApiProvider,
    AppDocument, AppLanguage, CapabilityCatalog, ConversationPreset, ConversationPresetSettings,
    GlobalSettings, PresetLibrary, ProviderFamily, ReasoningEffort, ResolvedLanguage,
    ThemePreference, ToolCategory, ToolDescriptor, ToolParameter, ToolParameterType, Workspace,
    WorkspaceKind,
};

#[cfg(test)]
use crate::model::{
    ContextItem, Conversation, ConversationSettings, SecurityLevel, ToolResult,
};

#[cfg(test)]
const DEFAULT_SYSTEM_PROMPT: &str =
    "你是 Mework 的工程代理。先理解任务，再使用可用工具完成工作，并简洁报告结果。";

fn parameter(
    name: &str,
    label: &str,
    parameter_type: ToolParameterType,
    required: bool,
    default_value: Option<Value>,
    placeholder: Option<&str>,
    help: Option<&str>,
) -> ToolParameter {
    ToolParameter {
        name: name.into(),
        label: label.into(),
        parameter_type,
        required,
        placeholder: placeholder.map(str::to_owned),
        help: help.map(str::to_owned),
        default_value,
    }
}

fn descriptor(
    name: &str,
    label: &str,
    description: &str,
    category: ToolCategory,
    dangerous: bool,
    parameters: Vec<ToolParameter>,
) -> ToolDescriptor {
    ToolDescriptor {
        force_confirmation: false,
        name: name.into(),
        label: label.into(),
        description: description.into(),
        category,
        dangerous,
        parameters,
        input_schema: None,
    }
}

pub fn tool_catalog() -> Vec<ToolDescriptor> {
    use ToolParameterType::{Boolean, Json, Multiline, Number, String as StringType};

    vec![
        descriptor(
            "ls",
            "列出文件",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter(
                    "path",
                    "目录",
                    StringType,
                    true,
                    Some(json!(".")),
                    Some("."),
                    None,
                ),
                parameter(
                    "depth",
                    "递归深度",
                    Number,
                    false,
                    Some(json!(1)),
                    None,
                    Some("0 仅列出当前目录"),
                ),
            ],
        ),
        descriptor(
            "grep",
            "搜索内容",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter(
                    "pattern",
                    "搜索内容",
                    StringType,
                    true,
                    None,
                    Some("TODO|FIXME"),
                    None,
                ),
                parameter(
                    "path",
                    "范围",
                    StringType,
                    false,
                    Some(json!(".")),
                    None,
                    None,
                ),
                parameter(
                    "case_sensitive",
                    "区分大小写",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    None,
                ),
            ],
        ),
        descriptor(
            "powershell",
            "PowerShell",
            "",
            ToolCategory::Shell,
            true,
            vec![
                parameter(
                    "command",
                    "命令",
                    Multiline,
                    true,
                    None,
                    Some("Get-ChildItem -Force"),
                    None,
                ),
                parameter(
                    "description",
                    "说明",
                    StringType,
                    false,
                    None,
                    Some("列出当前目录的文件"),
                    None,
                ),
                // No catalog default: the default lives in the host
                // (tool_executor::SHELL_DEFAULT_TIMEOUT) and in the schema text.
                // A default here would prefill the manual tool-call form and send
                // the value explicitly, which is noise in the recorded input.
                parameter(
                    "timeout",
                    "超时（毫秒）",
                    Number,
                    false,
                    None,
                    Some("120000"),
                    None,
                ),
                parameter(
                    "run_in_background",
                    "后台运行",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    None,
                ),
            ],
        ),
        descriptor(
            "bash",
            "Bash",
            "",
            ToolCategory::Shell,
            true,
            vec![
                parameter(
                    "command",
                    "命令",
                    Multiline,
                    true,
                    None,
                    Some("git status --short"),
                    None,
                ),
                parameter(
                    "description",
                    "说明",
                    StringType,
                    false,
                    None,
                    Some("查看工作树状态"),
                    None,
                ),
                // No catalog default: the default lives in the host
                // (tool_executor::SHELL_DEFAULT_TIMEOUT) and in the schema text.
                // A default here would prefill the manual tool-call form and send
                // the value explicitly, which is noise in the recorded input.
                parameter(
                    "timeout",
                    "超时（毫秒）",
                    Number,
                    false,
                    None,
                    Some("120000"),
                    None,
                ),
                parameter(
                    "run_in_background",
                    "后台运行",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    None,
                ),
            ],
        ),
        descriptor(
            "write",
            "写入文件",
            "",
            ToolCategory::Filesystem,
            true,
            vec![
                parameter(
                    "path",
                    "文件路径",
                    StringType,
                    true,
                    None,
                    Some("src/example.ts"),
                    None,
                ),
                parameter("content", "文件内容", Multiline, true, None, None, None),
            ],
        ),
        descriptor(
            "edit",
            "编辑文件",
            "",
            ToolCategory::Filesystem,
            true,
            vec![
                parameter("path", "文件路径", StringType, true, None, None, None),
                parameter("find", "查找内容", Multiline, true, None, None, None),
                parameter("replace", "替换为", Multiline, true, None, None, None),
            ],
        ),
        descriptor(
            "find",
            "查找文件",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter(
                    "query",
                    "文件名模式",
                    StringType,
                    true,
                    None,
                    Some("*.tsx"),
                    None,
                ),
                parameter(
                    "path",
                    "目录",
                    StringType,
                    false,
                    Some(json!(".")),
                    None,
                    None,
                ),
            ],
        ),
        descriptor(
            "read",
            "读取文件",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter("path", "文件路径", StringType, true, None, None, None),
                parameter(
                    "start_line",
                    "起始行",
                    Number,
                    false,
                    Some(json!(1)),
                    None,
                    Some("仅文本文件；从 1 开始"),
                ),
                parameter(
                    "end_line",
                    "结束行",
                    Number,
                    false,
                    None,
                    None,
                    Some("仅文本文件；包含该行"),
                ),
            ],
        ),
        descriptor(
            "web_search",
            "联网搜索",
            "",
            ToolCategory::Web,
            true,
            vec![parameter(
                "query",
                "查询",
                StringType,
                true,
                None,
                Some("Anthropic Claude 4.5 发布日期"),
                Some("一句自足的查询；不要用代词指代上文，长问题拆成多次检索"),
            )],
        ),
        descriptor(
            "web_fetch",
            "抓取网页",
            "",
            ToolCategory::Web,
            true,
            vec![parameter(
                "urls",
                "网址",
                Json,
                true,
                None,
                Some("[\"https://example.com/docs/changelog\"]"),
                Some("一批绝对 http(s) 地址；不知道地址时先用联网搜索"),
            )],
        ),
        descriptor(
            "playwright",
            "浏览器",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "action",
                    "动作",
                    StringType,
                    true,
                    None,
                    Some("click"),
                    Some("navigate、snapshot、click、type、fill_form、select、hover、key、scroll、evaluate、wait、screenshot、console、network、dialog、file_upload、upload_image、resize、tab_new、tab_list、tab_select、tab_close、close"),
                ),
                parameter(
                    "url",
                    "网址",
                    StringType,
                    false,
                    None,
                    Some("https://example.com"),
                    Some("navigate、tab_new；navigate 还接受 back / forward / reload"),
                ),
                parameter(
                    "max_chars",
                    "最大字符数",
                    Number,
                    false,
                    Some(json!(30000)),
                    None,
                    Some("snapshot；范围 1000–60000"),
                ),
                parameter(
                    "selector",
                    "CSS Selector",
                    StringType,
                    false,
                    None,
                    Some("button[type=submit]"),
                    Some("click、type、select、hover、scroll、wait、screenshot、file_upload、upload_image"),
                ),
                parameter(
                    "ref",
                    "元素 Ref",
                    StringType,
                    false,
                    None,
                    Some("e12"),
                    Some("同 selector 的动作；两者任选其一，scroll、screenshot 以及文件选择器打开时的 file_upload、upload_image 可都不给"),
                ),
                parameter(
                    "button",
                    "鼠标按键",
                    StringType,
                    false,
                    Some(json!("left")),
                    None,
                    Some("click；left | right | middle"),
                ),
                parameter(
                    "double",
                    "双击",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("click；双击"),
                ),
                parameter(
                    "modifiers",
                    "修饰键",
                    Json,
                    false,
                    None,
                    Some("[\"Control\"]"),
                    Some("click；组合键数组:Control、Shift、Alt、Meta"),
                ),
                parameter(
                    "text",
                    "文本",
                    Multiline,
                    false,
                    None,
                    None,
                    Some("type 要输入的文本；wait 要等待出现的页面文本"),
                ),
                parameter(
                    "clear",
                    "清空",
                    Boolean,
                    false,
                    None,
                    None,
                    Some("type 先清空原值（默认 true）；console、network 读取后清空（默认 false）"),
                ),
                parameter(
                    "submit",
                    "输入后提交",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("type；输入后按 Enter"),
                ),
                parameter(
                    "slowly",
                    "逐键输入",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("type；逐键输入"),
                ),
                parameter(
                    "fields",
                    "表单字段",
                    Json,
                    false,
                    None,
                    Some("[{\"ref\":\"e3\",\"value\":\"张三\"},{\"ref\":\"e5\",\"checked\":true}]"),
                    Some("fill_form；数组,每项含 selector 或 ref,配 value、values 或 checked;最多 50 项"),
                ),
                parameter(
                    "values",
                    "选项值",
                    Json,
                    false,
                    None,
                    None,
                    Some("select；字符串或字符串数组"),
                ),
                parameter(
                    "key",
                    "按键",
                    StringType,
                    false,
                    None,
                    Some("Enter 或 Control+L"),
                    Some("key"),
                ),
                parameter(
                    "repeat",
                    "重复次数",
                    Number,
                    false,
                    Some(json!(1)),
                    None,
                    Some("key；重复次数 1–50"),
                ),
                parameter(
                    "x",
                    "水平距离",
                    Number,
                    false,
                    Some(json!(0)),
                    None,
                    Some("scroll；水平滚动量"),
                ),
                parameter(
                    "y",
                    "垂直距离",
                    Number,
                    false,
                    Some(json!(600)),
                    None,
                    Some("scroll；垂直滚动量"),
                ),
                parameter(
                    "script",
                    "JavaScript",
                    Multiline,
                    false,
                    None,
                    Some("document.title"),
                    Some("evaluate；支持 await;结尾表达式即返回值。给 ref 时把该元素绑定为 element 变量"),
                ),
                parameter(
                    "text_gone",
                    "消失文本",
                    StringType,
                    false,
                    None,
                    Some("加载中"),
                    Some("wait；等待文本消失"),
                ),
                parameter(
                    "load",
                    "等待加载完成",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("wait；等到 document.readyState 为 complete;可与其他条件同时使用"),
                ),
                parameter(
                    "timeout_ms",
                    "超时毫秒",
                    Number,
                    false,
                    Some(json!(5000)),
                    None,
                    Some("wait；最大 30000"),
                ),
                parameter(
                    "path",
                    "保存路径",
                    StringType,
                    false,
                    Some(json!("browser-screenshot.png")),
                    Some("artifacts/page.png"),
                    Some("screenshot；工作区内的 .png 路径"),
                ),
                parameter(
                    "full_page",
                    "完整页面",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("screenshot；截取整页而不是视口"),
                ),
                parameter(
                    "only_errors",
                    "只看错误",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some("console；只看错误"),
                ),
                parameter(
                    "limit",
                    "条数上限",
                    Number,
                    false,
                    None,
                    None,
                    Some("console（默认 100）、network（默认 50）；最多返回条数 1–200"),
                ),
                parameter(
                    "filter",
                    "URL 过滤",
                    StringType,
                    false,
                    None,
                    Some("/api/"),
                    Some("network；URL 子串过滤"),
                ),
                parameter(
                    "accept",
                    "接受",
                    Boolean,
                    false,
                    None,
                    Some("true"),
                    Some("dialog；回答当前打开的对话框:true 接受,false 取消（默认 true）"),
                ),
                parameter(
                    "prompt_text",
                    "Prompt 输入",
                    StringType,
                    false,
                    None,
                    None,
                    Some("dialog；prompt 对话框的输入,仅在接受时生效"),
                ),
                parameter(
                    "paths",
                    "文件路径",
                    Json,
                    false,
                    None,
                    Some("\"artifacts/report.pdf\""),
                    Some("file_upload；工作区内路径,字符串或数组,最多 10 个"),
                ),
                parameter(
                    "image_id",
                    "图片编号",
                    StringType,
                    false,
                    None,
                    Some("3"),
                    Some("upload_image；对话中的图片编号,如 3、#3 或 [Image #3]"),
                ),
                parameter(
                    "filename",
                    "文件名",
                    StringType,
                    false,
                    None,
                    Some("photo.png"),
                    Some("upload_image；页面看到的文件名;默认用附件原名"),
                ),
                parameter(
                    "width",
                    "宽度",
                    Number,
                    false,
                    None,
                    Some("1280"),
                    Some("resize；范围 320–7680"),
                ),
                parameter(
                    "height",
                    "高度",
                    Number,
                    false,
                    None,
                    Some("720"),
                    Some("resize；范围 240–4320"),
                ),
                parameter(
                    "tab",
                    "标签页 id",
                    StringType,
                    false,
                    None,
                    Some("main"),
                    Some("tab_select、tab_close；tab_list 返回的 tab 值,main 是本对话自己的页面"),
                ),
            ],
        ),
        descriptor(
            "agent_spawn",
            "子代理",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "prompt",
                    "任务",
                    Multiline,
                    true,
                    None,
                    Some("调查 src/ 下的路由结构并总结关键文件"),
                    Some("默认子代理看不到当前对话，任务描述必须自包含全部背景"),
                ),
                parameter(
                    "agent_type",
                    "命名类型",
                    StringType,
                    false,
                    None,
                    Some("code-reviewer"),
                    Some("可选的可信命名定义短名称；可用的名称与用途列在本轮的可用 Agent 清单里。由宿主解析，不能与 context=conversation 同时使用"),
                ),
                parameter(
                    "name",
                    "名称",
                    StringType,
                    true,
                    None,
                    Some("review-api"),
                    Some("必填。用于 send_message / followup_task / task_wait 寻址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名"),
                ),
                parameter(
                    "label",
                    "显示名",
                    StringType,
                    false,
                    None,
                    Some("调查路由"),
                    Some("显示在时间线上的短名称"),
                ),
                parameter(
                    "context",
                    "初始上下文",
                    StringType,
                    false,
                    Some(json!("none")),
                    None,
                    Some("none（默认）：只看到任务；conversation：携带当前对话历史副本"),
                ),
                parameter(
                    "schema",
                    "输出模式",
                    Json,
                    false,
                    None,
                    Some(r#"{"type":"object","properties":{"verdict":{"type":"string"}},"required":["verdict"]}"#),
                    Some("可选的 JSON Schema 子集；给出后子代理必须调用 structured_output 交回符合该模式的结果，返回值会随 task_wait 一起回来。顶层必须是 type 为 object 的对象模式；支持 type、properties、required、items、enum、const、additionalProperties、minItems/maxItems、minLength/maxLength、minimum/maximum，其余关键字会被当场拒绝"),
                ),
            ],
        ),
        descriptor(
            "send_message",
            "发送消息",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "target",
                    "子代理",
                    StringType,
                    true,
                    None,
                    Some("a1"),
                    Some("agent_spawn 返回的名称"),
                ),
                parameter("message", "消息", Multiline, true, None, None, None),
            ],
        ),
        descriptor(
            "followup_task",
            "追加任务",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "target",
                    "子代理",
                    StringType,
                    true,
                    None,
                    Some("a1"),
                    Some("agent_spawn 返回的名称"),
                ),
                parameter("message", "消息", Multiline, true, None, None, None),
            ],
        ),
        descriptor(
            "task_wait",
            "等待任务",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "tasks",
                    "任务列表",
                    Json,
                    false,
                    None,
                    Some("[\"a1\", \"shell:shell-1\"]"),
                    Some("任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，浏览器页面写 browser:<tab>；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与浏览器）"),
                ),
                parameter(
                    "timeout_seconds",
                    "超时秒数",
                    Number,
                    false,
                    Some(json!(60)),
                    None,
                    Some("5–600 秒，默认 60"),
                ),
            ],
        ),
        descriptor(
            "task_list",
            "任务列表",
            "",
            ToolCategory::Orchestration,
            false,
            vec![],
        ),
        // The skill tool is host-only, has no file scope or approval, and does not
        // appear in the picker because its name derives from the conversation's
        // on-demand skill setting.
        descriptor(
            "skill",
            "技能",
            "",
            ToolCategory::Orchestration,
            false,
            vec![parameter(
                "name",
                "技能名",
                StringType,
                true,
                None,
                Some("commit-helper"),
                Some("本对话已选技能的名字，取自 schema 的 enum"),
            )],
        ),
        descriptor(
            "read_global_memory",
            "读取全局记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![parameter(
                "name",
                "文档名",
                StringType,
                true,
                None,
                Some("构建环境"),
                Some("记忆索引里列出的文档名，.md 后缀可写可不写"),
            )],
        ),
        descriptor(
            "read_project_memory",
            "读取项目记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![parameter(
                "name",
                "文档名",
                StringType,
                true,
                None,
                Some("构建环境"),
                Some("记忆索引里列出的文档名，.md 后缀可写可不写"),
            )],
        ),
        descriptor(
            "create_global_memory",
            "创建全局记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("用户偏好"),
                    Some("memory 目录下的单个文档名，不能包含路径分隔符；.md 后缀可写可不写"),
                ),
                parameter(
                    "content",
                    "记忆内容",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("这份记忆的完整 Markdown 正文"),
                ),
                parameter(
                    "description",
                    "索引描述",
                    StringType,
                    true,
                    None,
                    Some("用户长期偏好的语言与代码风格"),
                    Some("一句话说明这份记忆记录了什么，会写进 MEMORY.md 索引供以后判断要不要读取"),
                ),
            ],
        ),
        descriptor(
            "create_project_memory",
            "创建项目记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("构建环境"),
                    Some("memory 目录下的单个文档名，不能包含路径分隔符；.md 后缀可写可不写"),
                ),
                parameter(
                    "content",
                    "记忆内容",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("这份记忆的完整 Markdown 正文"),
                ),
                parameter(
                    "description",
                    "索引描述",
                    StringType,
                    true,
                    None,
                    Some("测试必须用项目自带环境运行"),
                    Some("一句话说明这份记忆记录了什么，会写进 MEMORY.md 索引供以后判断要不要读取"),
                ),
            ],
        ),
        descriptor(
            "edit_global_memory",
            "编辑全局记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("用户偏好"),
                    Some("要修改的记忆文档名，.md 后缀可写可不写"),
                ),
                parameter(
                    "old_text",
                    "原文",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("文档中要被替换的原文，必须唯一匹配；不唯一时请提供更长的片段"),
                ),
                parameter(
                    "new_text",
                    "新文本",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("替换后的文本；留空表示删除这段内容"),
                ),
                parameter(
                    "description",
                    "索引描述",
                    StringType,
                    true,
                    None,
                    Some("用户长期偏好的语言与代码风格"),
                    Some("修改后这份记忆的一句话说明，会刷新 MEMORY.md 索引里的对应条目"),
                ),
            ],
        ),
        descriptor(
            "edit_project_memory",
            "编辑项目记忆",
            "",
            ToolCategory::Memory,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("构建环境"),
                    Some("要修改的记忆文档名，.md 后缀可写可不写"),
                ),
                parameter(
                    "old_text",
                    "原文",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("文档中要被替换的原文，必须唯一匹配；不唯一时请提供更长的片段"),
                ),
                parameter(
                    "new_text",
                    "新文本",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("替换后的文本；留空表示删除这段内容"),
                ),
                parameter(
                    "description",
                    "索引描述",
                    StringType,
                    true,
                    None,
                    Some("测试必须用项目自带环境运行"),
                    Some("修改后这份记忆的一句话说明，会刷新 MEMORY.md 索引里的对应条目"),
                ),
            ],
        ),
        descriptor(
            "workflow",
            "工作流",
            "",
            ToolCategory::Orchestration,
            true,
            vec![
                parameter(
                    "script",
                    "编排脚本",
                    Multiline,
                    true,
                    None,
                    Some("export const meta = { name: \"…\", description: \"…\" }\n…"),
                    Some("以 `export const meta = {…}` 开头的 JS 编排脚本：用 agent()/parallel()/pipeline()/phase()/log() 派生并组织步骤子代理，正文的 return 值就是运行结果。给某一步加 { isolation: \"worktree\" } 会为它单开一棵从 HEAD 检出的 git 工作树（看不到未提交改动；留下改动就保留，没改动就自动拆除）"),
                ),
                parameter(
                    "name",
                    "名称",
                    StringType,
                    true,
                    None,
                    Some("review-sweep"),
                    Some("必填。这次运行在会话代理命名空间里的地址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名，续跑也要换新名字"),
                ),
                parameter(
                    "args",
                    "输入参数",
                    Json,
                    false,
                    None,
                    None,
                    Some("原样暴露给脚本的 JSON 值（全局 args）；数组与对象直接传，不要编码成字符串"),
                ),
                parameter(
                    "token_budget",
                    "token 预算",
                    Number,
                    false,
                    None,
                    None,
                    Some("本次运行允许消耗的 token 硬顶，脚本经 budget 读到；耗尽后新的 agent() 调用抛错"),
                ),
                parameter(
                    "resume_run_id",
                    "续跑运行 ID",
                    StringType,
                    false,
                    None,
                    Some("run0a1b2c3d"),
                    Some("上一次同脚本运行报出的运行 ID；已入日志的步骤即时重放，其余步骤重跑。脚本正文必须与获批时逐字一致"),
                ),
            ],
        ),
        descriptor(
            "todo",
            "待办事项",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "action",
                    "动作",
                    StringType,
                    true,
                    None,
                    Some("create"),
                    Some("选择操作：create、update、get、list"),
                ),
                parameter(
                    "taskId",
                    "任务 ID",
                    StringType,
                    false,
                    None,
                    Some("task-1"),
                    Some("用于 update、get；create 返回的不透明 ID"),
                ),
                parameter(
                    "subject",
                    "任务标题",
                    StringType,
                    false,
                    None,
                    Some("补全用户认证"),
                    Some("create 必填；update 可选"),
                ),
                parameter(
                    "description",
                    "任务说明",
                    Multiline,
                    false,
                    None,
                    Some("实现登录与注册接口，并补齐测试。"),
                    Some("create 必填；update 可选"),
                ),
                parameter(
                    "activeForm",
                    "进行中文案",
                    StringType,
                    false,
                    None,
                    Some("正在补全用户认证"),
                    Some("create、update；任务处于 in_progress 时显示的简短进行时文案"),
                ),
                parameter(
                    "status",
                    "状态",
                    StringType,
                    false,
                    None,
                    Some("in_progress"),
                    Some("用于 update；pending | in_progress | completed | deleted"),
                ),
                parameter(
                    "owner",
                    "负责人",
                    StringType,
                    false,
                    None,
                    None,
                    Some("仅用于 update"),
                ),
                parameter(
                    "addBlocks",
                    "新增被阻塞任务",
                    Json,
                    false,
                    None,
                    None,
                    Some("用于 update；由本任务阻塞的 task ID 数组"),
                ),
                parameter(
                    "addBlockedBy",
                    "新增前置任务",
                    Json,
                    false,
                    None,
                    None,
                    Some("用于 update；阻塞本任务的 task ID 数组"),
                ),
                parameter(
                    "metadata",
                    "元数据",
                    Json,
                    false,
                    None,
                    None,
                    Some("create 整体写入；update 按 key 合并，值为 null 时删除该 key"),
                ),
            ],
        ),
        descriptor(
            "ask_user",
            "提问",
            "",
            ToolCategory::Orchestration,
            false,
            vec![parameter(
                "questions",
                "问题",
                Json,
                true,
                None,
                Some(
                    r#"[{"question":"采用哪个方案？","header":"实现方案","options":[{"label":"方案 A","description":"保持改动最小"},{"label":"方案 B","description":"完整重构"}],"multiSelect":false}]"#,
                ),
                Some(
                    "Claude Code AskUserQuestion 格式：1–4 题；每题含 header、question、2–4 个 label/description 选项及 multiSelect；无需添加 Other",
                ),
            )],
        ),
        descriptor(
            "fork",
            "分叉会话",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "prompt",
                    "提示词",
                    Multiline,
                    true,
                    None,
                    Some("在分叉出的会话里要完成的任务"),
                    Some("分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景"),
                ),
                parameter(
                    "inherit_context",
                    "继承上下文",
                    Boolean,
                    false,
                    Some(json!(false)),
                    None,
                    Some(
                        "可选，默认 false：子对话只带这条 prompt 开始；true 时把目前为止的时间线与已完成任务一并复制进去",
                    ),
                ),
            ],
        ),
        descriptor(
            "plan",
            "计划文档",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "action",
                    "动作",
                    StringType,
                    true,
                    None,
                    Some("write"),
                    Some("选择操作：write 写入或覆盖计划、read 读取当前计划"),
                ),
                parameter(
                    "content",
                    "计划正文",
                    StringType,
                    false,
                    None,
                    None,
                    Some("write 必填；计划的 Markdown 正文，整篇覆盖上一版"),
                ),
            ],
        ),
        descriptor(
            "exit_plan_mode",
            "退出计划模式",
            "",
            ToolCategory::Orchestration,
            false,
            vec![],
        ),
        descriptor(
            "enter_plan_mode",
            "进入计划模式",
            "",
            ToolCategory::Orchestration,
            false,
            vec![],
        ),
    ]
}

/// Returns trusted model-facing tool defaults for the configured language.
///
/// Trusted request construction always enters through this function before applying
/// user-authored description overrides. `tool_catalog` creates an independent tree on
/// every call, so localizing this copy cannot mutate the Simplified Chinese defaults.
pub fn tool_catalog_for_language(language: ResolvedLanguage) -> Vec<ToolDescriptor> {
    let mut tools = tool_catalog();
    for tool in &mut tools {
        localize_tool_descriptor(tool, language);
    }
    tools
}

fn localize_tool_descriptor(tool: &mut ToolDescriptor, language: ResolvedLanguage) {
    if language != ResolvedLanguage::EnUs {
        return;
    }
    let label = english_tool_label(&tool.name)
        .unwrap_or_else(|| panic!("missing English defaults for built-in tool {}", tool.name));
    tool.label = label.to_owned();

    for parameter in &mut tool.parameters {
        parameter.label = english_parameter_label(&parameter.name)
            .unwrap_or_else(|| {
                panic!(
                    "missing English label for built-in tool parameter {}.{}",
                    tool.name, parameter.name
                )
            })
            .to_owned();

        if parameter.help.as_deref().is_some_and(contains_han) {
            parameter.help = Some(
                english_parameter_help(&tool.name, &parameter.name)
                    .unwrap_or_else(|| {
                        panic!(
                            "missing English help for built-in tool parameter {}.{}",
                            tool.name, parameter.name
                        )
                    })
                    .to_owned(),
            );
        }
        if parameter.placeholder.as_deref().is_some_and(contains_han) {
            parameter.placeholder = Some(
                english_parameter_placeholder(&tool.name, &parameter.name)
                    .unwrap_or_else(|| {
                        panic!(
                            "missing English placeholder for built-in tool parameter {}.{}",
                            tool.name, parameter.name
                        )
                    })
                    .to_owned(),
            );
        }
    }
}

fn contains_han(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character,
            '\u{3400}'..='\u{4DBF}'
                | '\u{4E00}'..='\u{9FFF}'
                | '\u{F900}'..='\u{FAFF}'
                | '\u{20000}'..='\u{2FA1F}'
        )
    })
}

fn english_tool_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "ls" => "List files",
        "grep" => "Search content",
        "powershell" => "PowerShell",
        "bash" => "Bash",
        "write" => "Write file",
        "edit" => "Edit file",
        "find" => "Find files",
        "read" => "Read file",
        "web_search" => "Web search",
        "web_fetch" => "Fetch web pages",
        "playwright" => "Browser",
        "agent_spawn" => "Subagent",
        "send_message" => "Send message",
        "followup_task" => "Follow up",
        "task_wait" => "Wait for tasks",
        "task_list" => "List tasks",
        "read_global_memory" => "Read global memory",
        "read_project_memory" => "Read project memory",
        "create_global_memory" => "Create global memory",
        "create_project_memory" => "Create project memory",
        "edit_global_memory" => "Edit global memory",
        "edit_project_memory" => "Edit project memory",
        "ask_user" => "Ask user",
        "todo" => "Todo",
        "workflow" => "Workflow",
        "skill" => "Skill",
        "fork" => "Fork conversation",
        "plan" => "Plan document",
        "exit_plan_mode" => "Exit plan mode",
        "enter_plan_mode" => "Enter plan mode",
        _ => return None,
    })
}

fn english_parameter_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "accept" => "Accept",
        "action" => "Action",
        "activeForm" => "Active form",
        "addBlockedBy" => "Add blockers",
        "addBlocks" => "Add blocked tasks",
        "allowed_domains" => "Allowed domains",
        "agent_type" => "Named agent type",
        "args" => "Arguments",
        "background" => "Background color",
        "blocked_domains" => "Blocked domains",
        "button" => "Mouse button",
        "case_sensitive" => "Case sensitive",
        "clear" => "Clear",
        "command" => "Command",
        "content" => "File content",
        "context" => "Initial context",
        "depth" => "Recursion depth",
        "description" => "Description",
        "double" => "Double-click",
        "end_line" => "End line",
        "fields" => "Form fields",
        "filename" => "File name",
        "filter" => "URL filter",
        "find" => "Find text",
        "format" => "Format",
        "full_page" => "Full page",
        "height" => "Height",
        "image_id" => "Image number",
        "input_image_path" => "Input image path",
        "items" => "Task items",
        "key" => "Key",
        "label" => "Display name",
        "layer" => "Layer filter",
        "layers" => "Initial layers",
        "limit" => "Result limit",
        "load" => "Wait for load",
        "max_chars" => "Maximum characters",
        "max_results" => "Maximum results",
        "message" => "Message",
        "metadata" => "Metadata",
        "modifiers" => "Modifier keys",
        "name" => "Name",
        "new_text" => "New text",
        "objective" => "Objective",
        "old_text" => "Original text",
        "owner" => "Owner",
        "only_errors" => "Errors only",
        "ops" => "Operations",
        "options" => "Options",
        "path" => "Path",
        "paths" => "File paths",
        "pattern" => "Search pattern",
        "profile" => "Model profile",
        "prompt" => "Prompt",
        "prompt_text" => "Prompt text",
        "query" => "Query",
        "question" => "Question",
        "questions" => "Questions",
        "ref" => "Element ref",
        "repeat" => "Repeat count",
        "require_screenshot" => "Require screenshot",
        "replace" => "Replacement",
        "resume_run_id" => "Resume run ID",
        "run_in_background" => "Run in background",
        "scale" => "Scale",
        "script" => "JavaScript",
        "selector" => "CSS selector",
        "slowly" => "Type key by key",
        "start_line" => "Start line",
        "status" => "Status",
        "submit" => "Submit after typing",
        "subject" => "Task subject",
        "tab" => "Tab ID",
        "taskId" => "Task ID",
        "tasks" => "Tasks",
        "text" => "Text",
        "text_gone" => "Text to disappear",
        "timeout" => "Timeout (ms)",
        "timeout_ms" => "Timeout (ms)",
        "timeout_seconds" => "Timeout (seconds)",
        "title" => "Title",
        "token_budget" => "Token budget",
        "target" => "Child agent",
        "url" => "URL",
        "values" => "Option values",
        "width" => "Width",
        "x" => "Horizontal distance",
        "y" => "Vertical distance",
        "scope" => "Scope",
        "urls" => "URLs",
        "expected_version" => "Expected version",
        "schema" => "Output schema",
        "inherit_context" => "Inherit context",
        _ => return None,
    })
}

fn english_parameter_help(tool: &str, parameter: &str) -> Option<&'static str> {
    Some(match (tool, parameter) {
        ("ls", "depth") => "0 lists only the current directory.",
        ("skill", "name") => {
            "Name of a skill this conversation selected; the schema lists them as an enum."
        }
        ("read", "start_line") => "Text files only; 1-based.",
        ("read", "end_line") => "Text files only; inclusive.",
        ("web_search", "query") => {
            "One self-contained query; no pronouns pointing back at the conversation. Break a long question into several searches."
        }
        ("web_fetch", "urls") => {
            "Absolute http(s) page URLs. Search first when you do not know the URL."
        }
        ("workflow", "script") => {
            "JavaScript orchestration starting with `export const meta = {…}`: spawn steps with agent()/parallel()/pipeline(), narrate with phase()/log(); the body's return value is the run result. Adding { isolation: \"worktree\" } to a step gives it its own git worktree checked out from HEAD (uncommitted changes are not in it; a step that leaves changes keeps its worktree, one that changes nothing has it removed)."
        }
        ("workflow", "name") => {
            "Required. This run's address in the same namespace agents are named in, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree, so a resume still needs a fresh one."
        }
        ("workflow", "args") => {
            "JSON value exposed to the script as the global `args`, verbatim; pass arrays and objects directly, not as encoded strings."
        }
        ("workflow", "token_budget") => {
            "Hard token ceiling for this run, readable as budget in the script; once exhausted, further agent() calls throw."
        }
        ("workflow", "resume_run_id") => {
            "Run id from a previous run of this same script; journaled steps replay, the first unjournaled one and everything after it re-runs."
        }
        ("playwright", "action") => {
            "navigate, snapshot, click, type, fill_form, select, hover, key, scroll, evaluate, wait, screenshot, console, network, dialog, file_upload, upload_image, resize, tab_new, tab_list, tab_select, tab_close, close."
        }
        ("playwright", "url") => {
            "navigate, tab_new. navigate also accepts back / forward / reload."
        }
        ("playwright", "max_chars") => "snapshot; range 1,000-60,000.",
        ("playwright", "selector") => {
            "click, type, select, hover, scroll, wait, screenshot, file_upload, upload_image."
        }
        ("playwright", "ref") => {
            "Same actions as selector; provide either one. scroll, screenshot, and file_upload/upload_image while a file chooser is open may provide neither."
        }
        ("playwright", "button") => "click; left | right | middle.",
        ("playwright", "double") => "click; double-click.",
        ("playwright", "modifiers") => {
            "click; array of modifier keys: Control, Shift, Alt, Meta."
        }
        ("playwright", "text") => {
            "type: the text to enter. wait: page text that must appear."
        }
        ("playwright", "clear") => {
            "type: clear the current value first (default true). console, network: clear captured entries after reading (default false)."
        }
        ("playwright", "submit") => "type; press Enter after typing.",
        ("playwright", "slowly") => "type; dispatch individual key events.",
        ("playwright", "fields") => {
            "fill_form; array of up to 50 fields. Each item has selector or ref plus value, values, or checked."
        }
        ("playwright", "values") => "select; a string or an array of strings.",
        ("playwright", "key") => "key.",
        ("playwright", "repeat") => "key; repeat count: 1-50.",
        ("playwright", "x") => "scroll; horizontal scroll amount.",
        ("playwright", "y") => "scroll; vertical scroll amount.",
        ("playwright", "script") => {
            "evaluate; supports await and the final expression becomes the return value. With ref, that element is bound to the element variable."
        }
        ("playwright", "text_gone") => "wait; text that must disappear.",
        ("playwright", "load") => {
            "wait; wait until document.readyState is complete. Can be combined with the other conditions."
        }
        ("playwright", "timeout_ms") => "wait; maximum 30,000.",
        ("playwright", "path") => "screenshot; a workspace-relative .png path.",
        ("playwright", "full_page") => {
            "screenshot; capture the entire page instead of the viewport."
        }
        ("playwright", "only_errors") => "console; return errors only.",
        ("playwright", "limit") => {
            "console (default 100), network (default 50); maximum result count 1-200."
        }
        ("playwright", "filter") => "network; filter by a URL substring.",
        ("playwright", "accept") => {
            "dialog; answer the open dialog: true accepts, false dismisses (default true)."
        }
        ("playwright", "prompt_text") => "dialog; the answer for an open prompt dialog, used only when accepting.",
        ("playwright", "paths") => {
            "file_upload; a workspace-relative path or an array of up to 10 paths."
        }
        ("playwright", "image_id") => {
            "upload_image; the conversation image number, e.g. 3, #3, or [Image #3]."
        }
        ("playwright", "filename") => {
            "upload_image; the file name the page sees. Defaults to the attachment's own name."
        }
        ("playwright", "width") => "resize; range 320-7,680.",
        ("playwright", "height") => "resize; range 240-4,320.",
        ("playwright", "tab") => {
            "tab_select, tab_close; a tab value returned by tab_list. main is the conversation's own page."
        }
        ("agent_spawn", "prompt") => {
            "Child agents do not see this conversation by default; include all required context in the task."
        }
        ("agent_spawn", "agent_type") => {
            "Optional trusted definition slug; the available names and what each is for are listed in this turn's available-agents context. Resolved by the host; cannot be combined with context=conversation."
        }
        ("agent_spawn", "name") => {
            "Required. Name the child yourself: it is the address send_message / followup_task / task_wait take, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree."
        }
        ("agent_spawn", "label") => "Short name shown in the timeline.",
        ("agent_spawn", "context") => {
            "none (default): task only; conversation: include a copy of the current conversation history."
        }
        ("agent_spawn", "schema") => {
            "Optional JSON Schema subset. When set, the child must call structured_output with a result matching it, and that value comes back with task_wait. The top level must be an object schema; type, properties, required, items, enum, const, additionalProperties, minItems/maxItems, minLength/maxLength and minimum/maximum are supported and every other keyword is rejected on the spot."
        }
        ("send_message", "target") | ("followup_task", "target") => {
            "Name returned by agent_spawn."
        }
        ("task_wait", "tasks") => {
            "Array of task addresses: a child agent or workflow run by its bare name (a workflow also answers to workflow:<runId>), a background command as shell:<id>, a terminal as terminal:<id>, a browser page as browser:<tab>. The wait ends once every named task has produced a result. Omit to wait for every child agent, workflow run and background command in this conversation (terminals and browser pages excluded)."
        }
        ("task_wait", "timeout_seconds") => "5-600 seconds; default: 60.",
        ("read_global_memory", "name") | ("read_project_memory", "name") => {
            "A document name listed in the memory index. The .md suffix is optional."
        }
        ("create_global_memory", "name") | ("create_project_memory", "name") => {
            "A single document name inside the memory directory. Path separators are forbidden; the .md suffix is optional."
        }
        ("edit_global_memory", "name") | ("edit_project_memory", "name") => {
            "The memory document to modify. The .md suffix is optional."
        }
        ("create_global_memory", "content") | ("create_project_memory", "content") => {
            "The document's complete Markdown body."
        }
        ("edit_global_memory", "old_text") | ("edit_project_memory", "old_text") => {
            "The passage to replace. It must match exactly once; supply a longer excerpt when it is not unique."
        }
        ("edit_global_memory", "new_text") | ("edit_project_memory", "new_text") => {
            "The replacement text. Leave it empty to delete the passage."
        }
        ("create_global_memory", "description")
        | ("create_project_memory", "description") => {
            "One sentence describing what this memory records. It is written into the MEMORY.md index so a later run can decide whether to read the document."
        }
        ("edit_global_memory", "description") | ("edit_project_memory", "description") => {
            "One sentence describing this memory after the change. It refreshes the document's entry in the MEMORY.md index."
        }
        ("ask_user", "questions") => {
            "Claude Code AskUserQuestion format: 1-4 questions, each with header, question, 2-4 label/description options, and multiSelect. Do not add Other."
        }
        ("todo", "action") => "create, update, get, list.",
        ("todo", "taskId") => "update, get; the opaque ID returned by create.",
        ("todo", "subject") | ("todo", "description") => {
            "Required by create; optional patch field for update."
        }
        ("todo", "activeForm") => {
            "create, update; short present-continuous text shown while the task is in_progress."
        }
        ("todo", "status") => "update; pending | in_progress | completed | deleted",
        ("todo", "owner") => "update.",
        ("todo", "addBlocks") => "update; array of task IDs that this task blocks.",
        ("todo", "addBlockedBy") => "update; array of task IDs that block this task.",
        ("todo", "metadata") => {
            "create writes the whole object; update merges keys, and a null value removes that key."
        }
        ("fork", "prompt") => {
            "First user message of the forked conversation, and your only chance to instruct it; state the task and all the background it needs"
        }
        ("fork", "inherit_context") => {
            "Optional, default false: the child starts with only this prompt; true also copies the timeline so far and the completed tasks"
        }
        ("plan", "action") => {
            "Choose an action: write stores or replaces the plan, read returns the current one."
        }
        ("plan", "content") => {
            "Required for write; the plan's Markdown body, which replaces the previous one in full."
        }
        _ => return None,
    })
}

fn english_parameter_placeholder(tool: &str, parameter: &str) -> Option<&'static str> {
    Some(match (tool, parameter) {
        ("bash", "description") => "Show working tree status",
        ("powershell", "description") => "List files in the current directory",
        ("playwright", "fields") => {
            "[{\"ref\":\"e3\",\"value\":\"Alex\"},{\"ref\":\"e5\",\"checked\":true}]"
        }
        ("web_search", "query") => "Anthropic Claude 4.5 release date",
        ("web_fetch", "urls") => "[\"https://example.com/docs/changelog\"]",
        ("playwright", "key") => "Enter or Control+L",
        ("playwright", "text_gone") => "Loading",
        ("agent_spawn", "prompt") => "Inspect routing under src/ and summarize the key files",
        ("agent_spawn", "label") => "Inspect routing",
        ("read_global_memory", "name")
        | ("create_global_memory", "name")
        | ("edit_global_memory", "name") => "user-preferences",
        ("read_project_memory", "name")
        | ("create_project_memory", "name")
        | ("edit_project_memory", "name") => "build-environment",
        ("create_global_memory", "description") | ("edit_global_memory", "description") => {
            "The user's long-standing language and code-style preferences"
        }
        ("create_project_memory", "description") | ("edit_project_memory", "description") => {
            "Tests must run in the project's own environment"
        }
        ("ask_user", "questions") => {
            r#"[{"question":"Which approach should I use?","header":"Approach","options":[{"label":"Approach A","description":"Keep the change small"},{"label":"Approach B","description":"Perform a full rewrite"}],"multiSelect":false}]"#
        }
        ("todo", "action") => "create",
        ("todo", "subject") => "Implement user authentication",
        ("todo", "description") => {
            "Add login and signup endpoints and cover them with tests."
        }
        ("todo", "activeForm") => "Implementing user authentication",
        ("todo", "taskId") => "task-1",
        ("todo", "status") => "in_progress",
        ("fork", "prompt") => "The task to complete in the forked conversation",
        _ => return None,
    })
}

/// The two built-in provider rows, freshly identified.
///
/// IDs stay random per installation: they key credential storage, so a fixed one
/// would let two data domains collide on the same secret. Residency — "exactly
/// one row per built-in family" — is NOT implemented here; it stays the
/// renderer's `ensureCodexProvider` / `ensureClaudeAgentProvider`, which claim
/// these rows by FAMILY and therefore keep the IDs a seeded preset was written
/// against. This function only supplies the first values.
fn product_default_api_providers() -> Vec<ApiProvider> {
    vec![builtin_codex_provider(), builtin_claude_agent_provider()]
}

fn builtin_provider_id() -> String {
    format!("provider_{}", uuid::Uuid::new_v4())
}

/// Signed out until the user completes the OAuth flow, so it ships no models:
/// the Codex catalog lives behind that login and cannot be fetched here.
fn builtin_codex_provider() -> ApiProvider {
    ApiProvider {
        id: builtin_provider_id(),
        name: "OpenAI Codex".into(),
        enabled: false,
        family: ProviderFamily::OpenaiCodex,
        base_url: String::new(),
        family_settings: Default::default(),
        endpoint_base_urls: Default::default(),
        notes: String::new(),
        models: Vec::new(),
        active_model_id: None,
    }
}

/// Ships its catalog already installed, because this family's "fetch models" is
/// a built-in table rather than a request: `model_discovery::fetch_models`
/// performs no I/O for it, so running the real fetch here is both possible and
/// the only way to guarantee the seeded rows are identical to what the button
/// would produce — group, capabilities and reasoning shape included.
///
/// The `[1m]` twins are dropped. They are the same model under a CLI-only 1M
/// context budget, and listing both halves doubles the picker for a distinction
/// most users never make.
fn builtin_claude_agent_provider() -> ApiProvider {
    let mut provider = ApiProvider {
        id: builtin_provider_id(),
        name: "Claude Agent".into(),
        enabled: true,
        family: ProviderFamily::ClaudeAgent,
        base_url: String::new(),
        family_settings: Default::default(),
        endpoint_base_urls: Default::default(),
        notes: String::new(),
        models: Vec::new(),
        active_model_id: None,
    };
    provider.models = crate::model_discovery::fetch_models(&provider)
        .unwrap_or_default()
        .into_iter()
        .filter(|model| !model.id.contains("[1m]"))
        .collect();
    provider.active_model_id = provider.models.first().map(|model| model.id.clone());
    provider
}

#[cfg(test)]
pub fn default_api_providers() -> Vec<ApiProvider> {
    vec![
        ApiProvider {
            id: "openai_responses".into(),
            name: "OpenAI Responses".into(),
            enabled: true,
            family: ProviderFamily::OpenaiResponses,
            base_url: "https://api.openai.com/v1".into(),
            family_settings: Default::default(),
            endpoint_base_urls: Default::default(),
            notes: String::new(),
            models: Vec::new(),
            active_model_id: None,
        },
        ApiProvider {
            id: "openai_chat".into(),
            name: "OpenAI Chat Completions".into(),
            enabled: true,
            family: ProviderFamily::OpenaiChat,
            base_url: "https://api.openai.com/v1".into(),
            family_settings: Default::default(),
            endpoint_base_urls: Default::default(),
            notes: String::new(),
            models: Vec::new(),
            active_model_id: None,
        },
        ApiProvider {
            id: "anthropic_messages".into(),
            name: "Anthropic Messages".into(),
            enabled: true,
            family: ProviderFamily::Anthropic,
            base_url: "https://api.anthropic.com/v1".into(),
            family_settings: Default::default(),
            endpoint_base_urls: Default::default(),
            notes: String::new(),
            models: Vec::new(),
            active_model_id: None,
        },
    ]
}

pub(crate) const CODEX_PRESET_ID: &str = "preset_codex";
pub(crate) const CLAUDE_CODE_PRESET_ID: &str = "preset_claude_code";

/// One seeded role: everything on, thinking on, and nothing said about itself.
///
/// `tools: None` rather than an explicit allowlist, so the role tracks whatever
/// its preset enables instead of freezing today's catalog into six copies.
fn seed_agent_definition(name: &str, provider_id: &str, model_id: &str) -> AgentDefinition {
    AgentDefinition {
        enabled: true,
        deleted: false,
        name: name.into(),
        description: String::new(),
        source: AgentDefinitionSource::User,
        source_key: String::new(),
        revision: 1,
        memory_epoch: 1,
        model_selection: AgentModelSelection::Explicit {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        },
        memory: AgentDefinitionMemory::None,
        effort: Some(ReasoningEffort::Medium),
        tools: None,
        disallowed_tools: Vec::new(),
        search_provider: None,
    }
}

/// Everything in the catalog except the names the host derives for itself.
///
/// The memory tools follow the two memory switches, `skill` follows
/// `skill_tool_enabled`, the task-runtime tools appear only once something
/// can produce a task, and the plan tools follow the security level. Listing
/// any of them here would be inert at best: the renderer strips them again when
/// the preset is applied. Mirrors the renderer's `isHostDerivedToolName`.
fn seed_preset_enabled_tools(tools: &[ToolDescriptor]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| {
            !crate::mework_memory::is_memory_tool(&tool.name)
                && !crate::agents::is_task_runtime_tool_name(&tool.name)
                && !crate::plan_mode::is_plan_mode_tool_name(&tool.name)
                && tool.name != crate::capabilities::SKILL_TOOL
        })
        .map(|tool| tool.name.clone())
        .collect()
}

fn seed_preset(
    id: &str,
    name: &str,
    tools: &[ToolDescriptor],
    agent_definitions: Vec<AgentDefinition>,
) -> ConversationPreset {
    ConversationPreset {
        id: id.into(),
        name: name.into(),
        description: String::new(),
        settings: ConversationPresetSettings {
            // The host has no default prompt of its own; a fresh conversation
            // sends only its capability sections until the user writes one.
            system_prompt: String::new(),
            enabled_tools: seed_preset_enabled_tools(tools),
            tool_description_file_id: None,
            agent_definitions,
            // Every child is one of the three named roles, so the model cannot
            // route around them by spawning an anonymous one.
            allow_roleless_subagents: false,
            hook_ids: Vec::new(),
            skill_ids: Vec::new(),
            mcp_ids: Vec::new(),
            web_search: Default::default(),
            security_level: Default::default(),
            global_memory_enabled: false,
            project_memory_enabled: false,
            skill_tool_enabled: false,
        },
    }
}

/// The two shipped presets, each mirroring the agent fleet of the CLI it is
/// named after.
///
/// The Codex roles are bound to models that do not exist until the user signs
/// in and fetches the catalog. That is deliberate and is why a dangling
/// `Explicit` pair is now kept verbatim: the binding waits, and the role starts
/// working the moment its model shows up.
fn product_default_presets(providers: &[ApiProvider], tools: &[ToolDescriptor]) -> PresetLibrary {
    let provider_id = |family: ProviderFamily| {
        providers
            .iter()
            .find(|provider| provider.family == family)
            .map(|provider| provider.id.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let codex = provider_id(ProviderFamily::OpenaiCodex);
    let claude_agent = provider_id(ProviderFamily::ClaudeAgent);
    PresetLibrary {
        conversation_presets: vec![
            seed_preset(
                CODEX_PRESET_ID,
                "Codex",
                tools,
                vec![
                    seed_agent_definition("sol", &codex, "gpt-5.6-sol"),
                    seed_agent_definition("terra", &codex, "gpt-5.6-terra"),
                    seed_agent_definition("luna", &codex, "gpt-5.6-luna"),
                ],
            ),
            seed_preset(
                CLAUDE_CODE_PRESET_ID,
                "Claude Code",
                tools,
                vec![
                    seed_agent_definition("opus", &claude_agent, "claude-opus-5"),
                    seed_agent_definition("sonnet", &claude_agent, "claude-sonnet-5"),
                    seed_agent_definition("haiku", &claude_agent, "claude-haiku-4-5"),
                ],
            ),
        ],
        // Claude Agent is the one built-in that is usable without a sign-in, so
        // it is the default. Storage refuses an empty default once presets
        // exist, so this cannot be left blank.
        default_conversation_preset_id: CLAUDE_CODE_PRESET_ID.into(),
    }
}

pub(crate) fn product_default_document(workspace_path: &Path) -> AppDocument {
    let tools = tool_catalog();
    let now = Utc::now();
    let timestamp = |minutes: i64| (now - Duration::minutes(minutes)).to_rfc3339();
    let workspace_name = workspace_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace")
        .to_owned();
    let api_providers = product_default_api_providers();
    let presets = product_default_presets(&api_providers, &tools);
    // The one built-in that works without a sign-in, so it is what a fresh
    // install talks to. Leaving this null would make the renderer pick the first
    // enabled row anyway and then persist the choice as a change.
    let active_provider_id = api_providers
        .iter()
        .find(|provider| provider.enabled)
        .map(|provider| provider.id.clone());
    let document = AppDocument {
        schema_version: crate::storage::SCHEMA_VERSION,
        global_settings: GlobalSettings {
            app_language: AppLanguage::Auto,
            resolved_app_language: ResolvedLanguage::ZhCn,
            theme: ThemePreference::System,
            last_reasoning_effort: Default::default(),
            active_provider_id,
            // Keep this field-by-field in sync with `src/seed.ts` DEFAULT_APPEARANCE.
            appearance: Default::default(),
            // Empty values use the renderer command catalog's default bindings.
            shortcuts: Default::default(),
            environment_tools: Vec::new(),
        },
        assets: crate::model::AssetLibrary {
            api_providers,
            // New installations have no SSH machines or run-environment variables.
            execution_environments: Default::default(),
            // Keep this in sync with `src/seed.ts`: enable the anonymous Exa MCP
            // search provider and Jina fetch provider by default; other providers
            // require user credentials.
            web_search: crate::model::WebSearchAssets {
                providers: crate::model::SearchProviderKind::CATALOG
                    .iter()
                    .copied()
                    .map(|kind| {
                        if matches!(
                            kind,
                            crate::model::SearchProviderKind::ExaMcp
                                | crate::model::SearchProviderKind::Jina
                        ) {
                            crate::model::SearchProviderConfig::enabled(kind)
                        } else {
                            crate::model::SearchProviderConfig::new(kind)
                        }
                    })
                    .collect(),
                fetch_provider: Some(crate::model::SearchProviderKind::Jina),
                ..Default::default()
            },
            // New installations have no MCP servers or skills.
            mcp_servers: Vec::new(),
            skills: Vec::new(),
        },
        presets,
        workspaces: vec![
            Workspace {
                id: "ws_default".into(),
                name: workspace_name,
                kind: WorkspaceKind::Directory,
                path: workspace_path.to_string_lossy().into_owned(),
                created_at: timestamp(5),
                default_conversation_preset_id: String::new(),
                last_conversation_settings: None,
                // Product seeds have no conversations; the renderer owns the initial draft.
                conversations: Vec::new(),
            },
            Workspace {
                id: "__temporary__".into(),
                name: "临时工作区".into(),
                kind: WorkspaceKind::Temporary,
                path: String::new(),
                created_at: timestamp(0),
                default_conversation_preset_id: String::new(),
                last_conversation_settings: None,
                conversations: Vec::new(),
            },
        ],
        tools,
        capabilities: CapabilityCatalog {
            hooks: Vec::new(),
            skills: Vec::new(),
            mcps: Vec::new(),
            tool_description_files: Vec::new(),
        },
    };
    document
}

#[cfg(not(test))]
pub fn default_document(workspace_path: &Path) -> AppDocument {
    product_default_document(workspace_path)
}

#[cfg(test)]
pub fn default_document(workspace_path: &Path) -> AppDocument {
    let mut document = product_default_document(workspace_path);
    let system_prompt = DEFAULT_SYSTEM_PROMPT.to_owned();
    let enabled_tools = document
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    hydrate_test_settings(&mut document, &system_prompt, &enabled_tools);
    document
}

#[cfg(test)]
fn hydrate_test_settings(
    document: &mut AppDocument,
    system_prompt: &str,
    enabled_tools: &[String],
) {
    document.presets.conversation_presets = vec![ConversationPreset {
        id: "conversation_default".into(),
        name: "默认".into(),
        description: "测试对话预设。".into(),
        settings: ConversationPresetSettings {
            system_prompt: system_prompt.into(),
            enabled_tools: enabled_tools.to_vec(),
            tool_description_file_id: None,
            agent_definitions: Vec::new(),
            allow_roleless_subagents: false,
            hook_ids: Vec::new(),
            skill_ids: Vec::new(),
            mcp_ids: Vec::new(),
            web_search: Default::default(),
            security_level: Default::default(),
            global_memory_enabled: false,
            project_memory_enabled: false,
            skill_tool_enabled: false,
        },
    }];
    document.presets.default_conversation_preset_id = "conversation_default".into();
    document.assets.api_providers = default_api_providers();
    document.global_settings.active_provider_id = Some("openai_responses".into());
    // Test fixtures provide the initial conversation expected by host tests.
    if let Some(workspace) = document.workspaces.first_mut() {
        if workspace.conversations.is_empty() {
            workspace.conversations.push(Conversation {
                id: "conv_welcome".into(),
                title: String::new(),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
                settings: ConversationSettings {
                    system_prompt: system_prompt.into(),
                    include_app_data_path: false,
                    enabled_tools: enabled_tools.to_vec(),
                    hook_ids: Vec::new(),
                    skill_ids: Vec::new(),
                    mcp_ids: Vec::new(),
                    tool_description_file_id: None,
                    agent_definitions: Vec::new(),
                    allow_roleless_subagents: false,
                    web_search: Default::default(),
                    reasoning_effort: Default::default(),
                    security_level: Default::default(),
                    global_memory_enabled: false,
                    project_memory_enabled: false,
                    skill_tool_enabled: false,
                },
                contexts: Vec::new(),
                queued_messages: Vec::new(),
                branches: Vec::new(),
                user_aborted_tasks: Vec::new(),
                worktree: None,
                run_target: None,
                parent_conversation_id: None,
            });
        }
    }
    if let Some(conversation) = document
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.conversations.first_mut())
    {
        let now = Utc::now();
        let timestamp = |minutes: i64| (now - Duration::minutes(minutes)).to_rfc3339();
        conversation.settings.system_prompt = system_prompt.into();
        conversation.settings.enabled_tools = enabled_tools.to_vec();
        conversation.contexts = vec![
            ContextItem::System {
                id: "ctx_welcome_system".into(),
                content: system_prompt.into(),
                local_only: false,
                hook_execution: None,
                created_at: timestamp(4),
            },
            ContextItem::User {
                id: "ctx_welcome_user".into(),
                content: "检查工作区，并协助我完成第一个任务。".into(),
                images: Vec::new(),
                created_at: timestamp(3),
            },
            ContextItem::Reasoning {
                id: "ctx_welcome_reasoning".into(),
                content: Some("先读取工作区结构，再决定需要修改的最小范围。".into()),
                form: Some(crate::model::ReasoningForm::Plaintext),
                round: None,
                model_turn_id: None,
                interrupted: false,
                duration_ms: None,
                tokens: None,
                replay: None,
                created_at: timestamp(2),
            },
            ContextItem::Tool {
                id: "ctx_welcome_tool".into(),
                tool_name: "ls".into(),
                round: None,
                model_turn_id: None,
                requested_input: None,
                input: serde_json::from_value(json!({ "path": ".", "depth": 1 }))
                    .expect("object literal"),
                result: ToolResult {
                    success: true,
                    output: "工作区尚未扫描；编辑参数后重新执行。".into(),
                    images: Vec::new(),
                    diff: None,
                    executed_at: timestamp(1),
                    duration_ms: 0,
                },
                subagent: None,
                attestation: String::new(),
                created_at: timestamp(1),
            },
            ContextItem::Assistant {
                id: "ctx_welcome_assistant".into(),
                content: "我已准备好。可以直接描述你希望在这个工作区完成的目标。".into(),
                round: None,
                model_turn_id: None,
                interrupted: false,
                sources: Vec::new(),
                created_at: timestamp(0),
            },
        ];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_tool_catalog_localizes_every_visible_default_without_mutating_chinese() {
        let chinese = tool_catalog();
        let english = tool_catalog_for_language(ResolvedLanguage::EnUs);
        let chinese_after = tool_catalog_for_language(ResolvedLanguage::ZhCn);

        assert_eq!(chinese.len(), 30);
        assert_eq!(english.len(), chinese.len());
        assert_eq!(chinese_after, chinese);
        for (localized, canonical) in english.iter().zip(&chinese) {
            let expected_label = english_tool_label(&canonical.name).unwrap_or_else(|| {
                panic!("{} must have explicit English defaults", canonical.name)
            });
            assert_eq!(localized.name, canonical.name);
            assert_eq!(localized.label, expected_label);
            // Built-in tool descriptions stay empty; model-visible descriptions
            // can only come from `.mework/tool-descriptions` overrides.
            assert_eq!(localized.description, "");
            assert_eq!(canonical.description, "");
            assert_eq!(localized.category, canonical.category);
            assert_eq!(localized.dangerous, canonical.dangerous);
            assert!(!contains_han(&localized.label), "{} label", localized.name);
            assert!(
                !contains_han(&localized.description),
                "{} description",
                localized.name
            );
            assert_eq!(localized.parameters.len(), canonical.parameters.len());

            for (parameter, canonical_parameter) in
                localized.parameters.iter().zip(&canonical.parameters)
            {
                assert_eq!(parameter.name, canonical_parameter.name);
                assert_eq!(parameter.parameter_type, canonical_parameter.parameter_type);
                assert_eq!(parameter.required, canonical_parameter.required);
                assert_eq!(parameter.default_value, canonical_parameter.default_value);
                assert_eq!(
                    parameter.label,
                    english_parameter_label(&parameter.name).unwrap_or_else(|| {
                        panic!(
                            "{}.{} must have an explicit English label",
                            localized.name, parameter.name
                        )
                    })
                );
                assert!(
                    !contains_han(&parameter.label),
                    "{}.{} label",
                    localized.name,
                    parameter.name
                );
                if let Some(help) = &parameter.help {
                    assert!(
                        !contains_han(help),
                        "{}.{} help: {help}",
                        localized.name,
                        parameter.name
                    );
                }
                if let Some(placeholder) = &parameter.placeholder {
                    assert!(
                        !contains_han(placeholder),
                        "{}.{} placeholder: {placeholder}",
                        localized.name,
                        parameter.name
                    );
                }
            }
        }
    }

    #[test]
    fn seeded_conversations_start_at_the_most_cautious_security_level() {
        let document = default_document(Path::new("."));

        assert_eq!(document.schema_version, crate::storage::SCHEMA_VERSION);
        assert_eq!(
            document.workspaces[0].conversations[0]
                .settings
                .security_level,
            SecurityLevel::default()
        );
        // Empty workspace-level settings make new conversations use the global default preset.
        assert!(document
            .workspaces
            .iter()
            .all(|workspace| workspace.default_conversation_preset_id.is_empty()
                && workspace.last_conversation_settings.is_none()));
        assert!(!document
            .workspaces
            .iter()
            .any(|workspace| workspace.kind == WorkspaceKind::Unsupported));
        assert!(document.workspaces.iter().any(|workspace| {
            workspace.id == "__temporary__" && workspace.kind == WorkspaceKind::Temporary
        }));
    }
}
