use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::model::{
    AgentDefinition, AgentDefinitionMemory, AgentDefinitionSource, AgentModelSelection,
    ApiProvider, AppDocument, AppLanguage, CapabilityCatalog, ConversationPreset,
    ConversationPresetSettings, GlobalSettings, PresetLibrary, ProviderFamily, ReasoningEffort,
    ResolvedLanguage, ThemePreference, ToolCategory, ToolDescriptor, ToolParameter,
    ToolParameterType, Workspace, WorkspaceKind,
};

#[cfg(test)]
use crate::model::{ContextItem, Conversation, ConversationSettings, SecurityLevel, ToolResult};

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
                parameter(
                    "limit",
                    "条数上限",
                    Number,
                    false,
                    Some(json!(250)),
                    None,
                    Some("最多返回的匹配行，默认 250，至多 1000"),
                ),
                parameter(
                    "offset",
                    "跳过",
                    Number,
                    false,
                    Some(json!(0)),
                    None,
                    Some("先跳过这么多匹配行，用于翻到下一页"),
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
            "zsh",
            "zsh",
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
                    Some("ls -la"),
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
            "sh",
            "sh",
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
                    Some("ls -la"),
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
            "lsp",
            "代码语义导航",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter(
                    "operation",
                    "操作",
                    StringType,
                    true,
                    None,
                    Some("goToDefinition"),
                    Some("九种之一：goToDefinition、findReferences、hover、documentSymbol、workspaceSymbol、goToImplementation、prepareCallHierarchy、incomingCalls、outgoingCalls"),
                ),
                parameter("filePath", "文件路径", StringType, true, None, None, None),
                parameter(
                    "line",
                    "行号",
                    Number,
                    true,
                    None,
                    None,
                    Some("从 1 开始，与编辑器显示的一致"),
                ),
                parameter(
                    "character",
                    "列号",
                    Number,
                    true,
                    None,
                    None,
                    Some("从 1 开始，与编辑器显示的一致"),
                ),
                parameter(
                    "query",
                    "符号名",
                    StringType,
                    false,
                    None,
                    Some("LspRegistry"),
                    Some("只用于 workspaceSymbol；空查询在多数语言服务器上没有结果"),
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
            "preview_start",
            "启动预览",
            "",
            ToolCategory::Web,
            true,
            vec![parameter(
                "name",
                "名称",
                StringType,
                true,
                None,
                Some("dev"),
                Some(".mework/launch.json 里的服务器名称"),
            )],
        ),
        descriptor(
            "preview_stop",
            "停止预览",
            "",
            ToolCategory::Web,
            true,
            vec![parameter(
                "serverId",
                "服务器 ID",
                StringType,
                true,
                None,
                None,
                Some("要停止的服务器 ID"),
            )],
        ),
        descriptor(
            "preview_list",
            "预览列表",
            "",
            ToolCategory::Web,
            false,
            Vec::new(),
        ),
        descriptor(
            "preview_logs",
            "服务器日志",
            "",
            ToolCategory::Web,
            false,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "level",
                    "级别",
                    StringType,
                    false,
                    Some(json!("all")),
                    None,
                    Some("按级别过滤：all（默认）返回全部输出，error 只返回含 error、exception、failed 或 fatal 的行"),
                ),
                parameter(
                    "lines",
                    "行数上限",
                    Number,
                    false,
                    Some(json!(50)),
                    None,
                    Some("最多返回行数（默认 50）"),
                ),
                parameter(
                    "search",
                    "文本过滤",
                    StringType,
                    false,
                    None,
                    Some("[DEBUG]"),
                    Some("只保留包含该文本的行（例如 [DEBUG]、POST /api）"),
                ),
            ],
        ),
        descriptor(
            "preview_console_logs",
            "控制台日志",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "level",
                    "级别",
                    StringType,
                    false,
                    Some(json!("all")),
                    None,
                    Some("按级别过滤：all（默认）、error（只看错误）、warn（警告加错误）"),
                ),
                parameter(
                    "lines",
                    "行数上限",
                    Number,
                    false,
                    Some(json!(50)),
                    None,
                    Some("最多返回行数（默认 50，上限 200）"),
                ),
            ],
        ),
        descriptor(
            "preview_screenshot",
            "页面截图",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "scale",
                    "缩放",
                    Number,
                    false,
                    None,
                    None,
                    Some("返回图像的缩放系数，取值 0.1 到 1；图像越小消耗的 token 越少。preview_click 用的是 preview_snapshot 给出的元素 UID，而不是像素坐标"),
                ),
            ],
        ),
        descriptor(
            "preview_snapshot",
            "页面快照",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
            ],
        ),
        descriptor(
            "preview_inspect",
            "检查元素",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "selector",
                    "CSS Selector",
                    StringType,
                    true,
                    None,
                    Some(".button"),
                    Some("要检查的元素 CSS 选择器"),
                ),
                parameter(
                    "styles",
                    "CSS 属性",
                    Json,
                    false,
                    None,
                    Some("[\"padding\",\"color\"]"),
                    Some("要返回的 CSS 属性名数组；不给时返回一组常用属性"),
                ),
            ],
        ),
        descriptor(
            "preview_click",
            "点击元素",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "selector",
                    "CSS Selector",
                    StringType,
                    true,
                    None,
                    Some("button.primary"),
                    Some("要点击的元素 CSS 选择器"),
                ),
                parameter(
                    "doubleClick",
                    "双击",
                    Boolean,
                    false,
                    None,
                    None,
                    Some("改为双击"),
                ),
            ],
        ),
        descriptor(
            "preview_fill",
            "填写输入",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "selector",
                    "CSS Selector",
                    StringType,
                    true,
                    None,
                    Some("input[name=email]"),
                    Some("要填写的输入框 CSS 选择器"),
                ),
                parameter(
                    "value",
                    "值",
                    StringType,
                    true,
                    None,
                    None,
                    Some("要填入的值"),
                ),
            ],
        ),
        descriptor(
            "preview_eval",
            "执行脚本",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "expression",
                    "表达式",
                    Multiline,
                    true,
                    None,
                    Some("document.title"),
                    Some("在页面上下文里求值的 JavaScript 表达式；返回值按 JSON 序列化"),
                ),
            ],
        ),
        descriptor(
            "preview_network",
            "网络请求",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "filter",
                    "过滤",
                    StringType,
                    false,
                    Some(json!("all")),
                    None,
                    Some("过滤：all（默认）返回全部请求，failed 只返回 4xx、5xx 与网络错误；给了 requestId 时本项被忽略"),
                ),
                parameter(
                    "requestId",
                    "请求 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("给出时返回该请求的响应正文，而不是列出全部请求；requestId 取自列表输出"),
                ),
            ],
        ),
        descriptor(
            "preview_resize",
            "调整视口",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "preset",
                    "设备预设",
                    StringType,
                    false,
                    None,
                    None,
                    Some("设备预设；给出时覆盖 width 与 height。desktop 清除尺寸模拟，回到面板自身的响应式尺寸"),
                ),
                parameter(
                    "width",
                    "宽度",
                    Number,
                    false,
                    None,
                    Some("1280"),
                    Some("视口宽度，单位 CSS 像素（需同时给 height）"),
                ),
                parameter(
                    "height",
                    "高度",
                    Number,
                    false,
                    None,
                    Some("720"),
                    Some("视口高度，单位 CSS 像素（需同时给 width）"),
                ),
                parameter(
                    "colorScheme",
                    "配色方案",
                    StringType,
                    false,
                    None,
                    None,
                    Some("模拟 prefers-color-scheme 媒体特性，用于测试深色与浅色"),
                ),
            ],
        ),
        descriptor(
            "preview_upload_image",
            "上传图片",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "image_id",
                    "图片编号",
                    StringType,
                    true,
                    None,
                    Some("3"),
                    Some("对话里的图片编号，如 3、#3 或 [Image #3]；也接受 64 位十六进制摘要"),
                ),
                parameter(
                    "selector",
                    "CSS Selector",
                    StringType,
                    false,
                    None,
                    Some("input[type=file]"),
                    Some("目标 file 输入框的 CSS 选择器；不给时用页面上第一个 file 输入框"),
                ),
                parameter(
                    "filename",
                    "文件名",
                    StringType,
                    false,
                    None,
                    Some("photo.png"),
                    Some("页面看到的文件名；不给时用附件原名"),
                ),
            ],
        ),
        descriptor(
            "preview_dialog",
            "回答对话框",
            "",
            ToolCategory::Web,
            true,
            vec![
                parameter(
                    "serverId",
                    "服务器 ID",
                    StringType,
                    false,
                    None,
                    None,
                    Some("服务器 ID"),
                ),
                parameter(
                    "accept",
                    "接受",
                    Boolean,
                    false,
                    Some(json!(true)),
                    None,
                    Some("true 接受对话框，false 取消（默认 true）"),
                ),
                parameter(
                    "prompt_text",
                    "Prompt 输入",
                    StringType,
                    false,
                    None,
                    None,
                    Some("prompt 对话框的输入，仅在接受时生效"),
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
                    Some("可选的可信命名定义名称；可用的名称与用途列在本轮的可用 Agent 清单里。照抄其中一个名称即可——由宿主解析，无歧义时大小写与分隔符不敏感。不能与 context=conversation 同时使用"),
                ),
                parameter(
                    "name",
                    "名称",
                    StringType,
                    true,
                    None,
                    Some("review-api"),
                    Some("必填。用于 task_wait 寻址，也是任务栏里这一行的标题；小写字母开头，可含数字、_ 和 -；整个对话分支树内不可重名"),
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
                    Some("[\"a1\", \"shell:1\"]"),
                    Some("任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，开发服务器写 preview:<serverId>（对话有多个工作区时写 preview:<serverId>@<工作区编号>）；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与开发服务器）"),
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
        descriptor(
            "box",
            "后台结果",
            "",
            ToolCategory::Orchestration,
            false,
            vec![parameter(
                "none",
                "空参数",
                Json,
                true,
                None,
                Some("[]"),
                Some("始终为空数组"),
            )],
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
        // Derived from `mcp_tool_discovery_enabled` the same way, and likewise
        // absent from the picker: it exists only while this run is holding MCP
        // tool schemas back, and it is the only way to get one of them.
        descriptor(
            "tool_search",
            "工具发现",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "query",
                    "查询",
                    StringType,
                    true,
                    None,
                    Some("select:mcp__github__create_issue"),
                    Some("`select:<名字>[,<名字>…]` 按名取，或者用关键词搜索"),
                ),
                parameter(
                    "max_results",
                    "最多返回",
                    Number,
                    false,
                    Some(json!(5)),
                    Some("5"),
                    Some("关键词搜索最多返回几个工具；按名取时不生效"),
                ),
            ],
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
                    Some("上一次运行报出的运行 ID；提示词与选项没变的步骤即时重放，上次停下时还在跑的步骤单独重跑，从第一个改动或失败的步骤起其余全部重跑。可省略脚本与 args 沿用上次的；传入改过的脚本会重新审批"),
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
            vec![parameter(
                "prompt",
                "提示词",
                Multiline,
                true,
                None,
                Some("在分叉出的会话里要完成的任务"),
                Some("分叉会话的第一条用户消息，也是你唯一一次下达指令的机会；说清任务与需要的全部背景"),
            )],
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
        // The handoff tools are derived by the host once the conversation's
        // context crosses the auto-compact threshold (`handoff.rs`); like the
        // plan tools, no picker offers them.
        descriptor(
            "read_handoff_note",
            "读取交接文档",
            "",
            ToolCategory::Orchestration,
            false,
            vec![parameter(
                "name",
                "文档名",
                StringType,
                true,
                None,
                Some("当前进度"),
                Some("交接索引里列出的文档名，.md 后缀可写可不写"),
            )],
        ),
        descriptor(
            "create_handoff_note",
            "写交接文档",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("当前进度"),
                    Some("交接文档名，不能包含路径分隔符；.md 后缀可写可不写"),
                ),
                parameter(
                    "content",
                    "文档内容",
                    Multiline,
                    true,
                    None,
                    None,
                    Some("交接文档的完整 Markdown 正文"),
                ),
                parameter(
                    "description",
                    "索引描述",
                    StringType,
                    true,
                    None,
                    Some("任务目标、已完成的工作与下一步"),
                    Some("一句话说明这份交接文档写了什么，会写进交接索引"),
                ),
            ],
        ),
        descriptor(
            "edit_handoff_note",
            "修改交接文档",
            "",
            ToolCategory::Orchestration,
            false,
            vec![
                parameter(
                    "name",
                    "文档名",
                    StringType,
                    true,
                    None,
                    Some("当前进度"),
                    Some("要修改的交接文档名，.md 后缀可写可不写"),
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
                    Some("任务目标、已完成的工作与下一步"),
                    Some("修改后这份交接文档的一句话说明，会刷新交接索引里的对应条目"),
                ),
            ],
        ),
        descriptor(
            "handoff",
            "交接",
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
        "zsh" => "zsh",
        "sh" => "sh",
        "write" => "Write file",
        "edit" => "Edit file",
        "find" => "Find files",
        "read" => "Read file",
        "lsp" => "Code navigation",
        "web_search" => "Web search",
        "web_fetch" => "Fetch web pages",
        "preview_start" => "Start preview",
        "preview_stop" => "Stop preview",
        "preview_list" => "List previews",
        "preview_logs" => "Server logs",
        "preview_console_logs" => "Console logs",
        "preview_screenshot" => "Page screenshot",
        "preview_snapshot" => "Page snapshot",
        "preview_inspect" => "Inspect element",
        "preview_click" => "Click element",
        "preview_fill" => "Fill input",
        "preview_eval" => "Run script",
        "preview_network" => "Network requests",
        "preview_resize" => "Resize viewport",
        "preview_upload_image" => "Upload image",
        "preview_dialog" => "Answer dialog",
        "agent_spawn" => "Subagent",
        "task_wait" => "Wait for tasks",
        "task_list" => "List tasks",
        "box" => "Background result",
        "read_global_memory" => "Read global memory",
        "read_project_memory" => "Read project memory",
        "create_global_memory" => "Create global memory",
        "create_project_memory" => "Create project memory",
        "edit_global_memory" => "Edit global memory",
        "edit_project_memory" => "Edit project memory",
        "ask_user" => "Ask user",
        "workflow" => "Workflow",
        "skill" => "Skill",
        "tool_search" => "Tool discovery",
        "fork" => "Fork conversation",
        "plan" => "Plan document",
        "exit_plan_mode" => "Exit plan mode",
        "read_handoff_note" => "Read handoff note",
        "create_handoff_note" => "Write handoff note",
        "edit_handoff_note" => "Edit handoff note",
        "handoff" => "Hand off",
        _ => return None,
    })
}

fn english_parameter_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "accept" => "Accept",
        "action" => "Action",
        "agent_type" => "Named agent type",
        "args" => "Arguments",
        "case_sensitive" => "Case sensitive",
        "character" => "Character",
        "colorScheme" => "Color scheme",
        "command" => "Command",
        "content" => "File content",
        "context" => "Initial context",
        "depth" => "Recursion depth",
        "description" => "Description",
        "doubleClick" => "Double-click",
        "end_line" => "End line",
        "expression" => "Expression",
        "filename" => "File name",
        "filter" => "Filter",
        "filePath" => "File path",
        "find" => "Find text",
        "height" => "Height",
        "image_id" => "Image number",
        "label" => "Display name",
        "level" => "Level",
        "limit" => "Limit",
        "line" => "Line",
        "lines" => "Line limit",
        "operation" => "Operation",
        "max_results" => "Maximum results",
        "name" => "Name",
        "new_text" => "New text",
        "none" => "Empty argument",
        "offset" => "Offset",
        "old_text" => "Original text",
        "path" => "Path",
        "pattern" => "Search pattern",
        "preset" => "Device preset",
        "prompt" => "Prompt",
        "prompt_text" => "Prompt text",
        "query" => "Query",
        "questions" => "Questions",
        "requestId" => "Request ID",
        "replace" => "Replacement",
        "resume_run_id" => "Resume run ID",
        "run_in_background" => "Run in background",
        "scale" => "Scale",
        "script" => "JavaScript",
        "search" => "Text filter",
        "selector" => "CSS selector",
        "serverId" => "Server ID",
        "start_line" => "Start line",
        "styles" => "CSS properties",
        "tasks" => "Tasks",
        "timeout" => "Timeout (ms)",
        "timeout_seconds" => "Timeout (seconds)",
        "token_budget" => "Token budget",
        "value" => "Value",
        "width" => "Width",
        "urls" => "URLs",
        "schema" => "Output schema",
        _ => return None,
    })
}

fn english_parameter_help(tool: &str, parameter: &str) -> Option<&'static str> {
    Some(match (tool, parameter) {
        ("ls", "depth") => "0 lists only the current directory.",
        ("grep", "limit") => "Most matching lines to return: 250 by default, at most 1,000.",
        ("grep", "offset") => "Matching lines to skip first, to fetch the next page.",
        ("skill", "name") => {
            "Name of a skill this conversation selected; the schema lists them as an enum."
        }
        ("tool_search", "query") => {
            "`select:<name>[,<name>…]` to fetch exact tools, or keywords to search for them."
        }
        ("tool_search", "max_results") => {
            "How many tools a keyword search may return; ignored when fetching by name."
        }
        ("read", "start_line") => "Text files only; 1-based.",
        ("read", "end_line") => "Text files only; inclusive.",
        ("lsp", "operation") => {
            "One of nine: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls."
        }
        ("lsp", "line") => "1-based, as shown in editors.",
        ("lsp", "character") => "1-based, as shown in editors.",
        ("lsp", "query") => {
            "workspaceSymbol only; most language servers return nothing for an empty query."
        }
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
            "Run id from a previous run; steps whose prompt and options are unchanged replay, a step the last attempt left running re-runs alone, and everything from the first changed or failed step re-runs. script and args may be omitted to reuse the last ones; an edited script is approved again."
        }
        ("preview_start", "name") => "Server name from .mework/launch.json.",
        ("preview_stop", "serverId") => "Server ID to stop",
        ("preview_logs", "serverId")
        | ("preview_console_logs", "serverId")
        | ("preview_screenshot", "serverId")
        | ("preview_snapshot", "serverId")
        | ("preview_inspect", "serverId")
        | ("preview_click", "serverId")
        | ("preview_fill", "serverId")
        | ("preview_eval", "serverId")
        | ("preview_network", "serverId")
        | ("preview_resize", "serverId")
        | ("preview_upload_image", "serverId")
        | ("preview_dialog", "serverId") => "Server ID",
        ("preview_logs", "level") => {
            "Filter by level: 'all' (default) shows all output, 'error' shows only lines containing error/exception/failed/fatal"
        }

        ("preview_logs", "lines") => "Max lines to return (default: 50)",
        ("preview_logs", "search") => {
            "Filter to lines containing this text (e.g., '[DEBUG]', 'POST /api')"
        }
        ("preview_console_logs", "level") => {
            "Filter by level: 'all' (default), 'error' (errors only), 'warn' (warnings + errors)"
        }
        ("preview_console_logs", "lines") => "Max lines to return (default: 50, max: 200)",
        ("preview_screenshot", "scale") => {
            "Scale factor in [0.1, 1] for the returned image; smaller images use fewer tokens. preview_click uses element UIDs from preview_snapshot, not pixel coordinates."
        }
        ("preview_inspect", "selector") => {
            "CSS selector (e.g., '.button', '#header')"
        }
        ("preview_inspect", "styles") => {
            "CSS properties to return (e.g., ['padding', 'color']). Defaults to common properties."
        }
        ("preview_click", "selector") => {
            "CSS selector for the element to click"
        }
        ("preview_click", "doubleClick") => "Perform a double-click",
        ("preview_fill", "selector") => {
            "CSS selector for the input element"
        }
        ("preview_fill", "value") => "Value to fill",
        ("preview_eval", "expression") => {
            "JavaScript expression to evaluate in the page context. Return values are serialized as JSON."
        }
        ("preview_network", "filter") => {
            "Filter: 'all' (default) shows all requests, 'failed' shows only 4xx/5xx and network errors. Ignored when requestId is provided."
        }
        ("preview_network", "requestId") => {
            "If provided, returns the response body for this specific request instead of listing all requests. Get requestIds from the listing output."
        }
        ("preview_resize", "preset") => {
            "Device preset. Overrides width/height if provided. \"desktop\" clears the size emulation (back to the pane's responsive size)."
        }
        ("preview_resize", "width") => "Viewport width in CSS pixels (requires height)",
        ("preview_resize", "height") => "Viewport height in CSS pixels (requires width)",
        ("preview_resize", "colorScheme") => {
            "Emulate prefers-color-scheme media feature for dark/light mode testing."
        }
        ("preview_upload_image", "image_id") => {
            "The conversation image number, e.g. 3, #3, or [Image #3]. A 64-character hex digest also resolves."
        }
        ("preview_upload_image", "selector") => {
            "CSS selector of the target file input. Omitted, the first file input on the page is used."
        }
        ("preview_upload_image", "filename") => {
            "The file name the page sees. Defaults to the attachment's own name."
        }
        ("preview_dialog", "accept") => {
            "true accepts the open dialog, false dismisses it (default true)."
        }
        ("preview_dialog", "prompt_text") => {
            "The answer for an open prompt dialog, used only when accepting."
        }
        ("agent_spawn", "prompt") => {
            "Child agents do not see this conversation by default; include all required context in the task."
        }
        ("agent_spawn", "agent_type") => {
            "Optional trusted definition name; the available names and what each is for are listed in this turn's available-agents context. Copy one of those names — the host resolves it, and matches case- and separator-insensitively when that is unambiguous. Cannot be combined with context=conversation."
        }
        ("agent_spawn", "name") => {
            "Required. Name the child yourself: it is the address task_wait takes, and the title the task is listed under. Start with a lowercase letter; digits, _ and - are allowed. It must be unused anywhere in this conversation's branch tree."
        }
        ("agent_spawn", "label") => "Short name shown in the timeline.",
        ("agent_spawn", "context") => {
            "none (default): task only; conversation: include a copy of the current conversation history."
        }
        ("agent_spawn", "schema") => {
            "Optional JSON Schema subset. When set, the child must call structured_output with a result matching it, and that value comes back with task_wait. The top level must be an object schema; type, properties, required, items, enum, const, additionalProperties, minItems/maxItems, minLength/maxLength and minimum/maximum are supported and every other keyword is rejected on the spot."
        }
        ("task_wait", "tasks") => {
            "Array of task addresses: a child agent or workflow run by its bare name (a workflow also answers to workflow:<runId>), a background command as shell:<id>, a terminal as terminal:<id>, a dev server as preview:<serverId> (preview:<serverId>@<workspace> when the conversation has several workspaces). The wait ends once every named task has produced a result. Omit to wait for every child agent, workflow run and background command in this conversation (terminals and dev servers excluded)."
        }
        ("task_wait", "timeout_seconds") => "5-600 seconds; default: 60.",
        ("box", "none") => "Always an empty list.",
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
        ("fork", "prompt") => {
            "First user message of the forked conversation, and your only chance to instruct it; state the task and all the background it needs"
        }
        ("plan", "action") => {
            "Choose an action: write stores or replaces the plan, read returns the current one."
        }
        ("plan", "content") => {
            "Required for write; the plan's Markdown body, which replaces the previous one in full."
        }
        ("read_handoff_note", "name") => {
            "A note name listed in the handoff index. The .md suffix is optional."
        }
        ("create_handoff_note", "name") => {
            "The handoff note's name. Path separators are forbidden; the .md suffix is optional."
        }
        ("edit_handoff_note", "name") => "The handoff note to modify. The .md suffix is optional.",
        ("create_handoff_note", "content") => "The note's complete Markdown body.",
        ("edit_handoff_note", "old_text") => {
            "The passage to replace. It must match exactly once; supply a longer excerpt when it is not unique."
        }
        ("edit_handoff_note", "new_text") => {
            "The replacement text. Leave it empty to delete the passage."
        }
        ("create_handoff_note", "description") => {
            "One sentence describing what this handoff note holds. It is written into the handoff index."
        }
        ("edit_handoff_note", "description") => {
            "One sentence describing this handoff note after the change. It refreshes the note's entry in the handoff index."
        }
        _ => return None,
    })
}

fn english_parameter_placeholder(tool: &str, parameter: &str) -> Option<&'static str> {
    Some(match (tool, parameter) {
        ("bash", "description") => "Show working tree status",
        ("powershell", "description") => "List files in the current directory",
        ("zsh", "description") | ("sh", "description") => "List files in the current directory",
        ("web_search", "query") => "Anthropic Claude 4.5 release date",
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
        ("fork", "prompt") => "The task to complete in the forked conversation",
        ("read_handoff_note", "name")
        | ("create_handoff_note", "name")
        | ("edit_handoff_note", "name") => "current-state",
        ("create_handoff_note", "description") | ("edit_handoff_note", "description") => {
            "The goal, the work done and the next step"
        }
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

/// Ships a starting catalog already installed, so a fresh install can talk to
/// Claude without first fetching. The fetch itself asks the CLI and cannot run
/// while the default document is built, so the rows come from the seed table,
/// projected the way a fetch projects rows — group, capabilities and reasoning
/// shape included.
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
    provider.models = crate::model_discovery::claude_agent_seed_models(&provider);
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

/// Id of the one conversation preset Mework ships.
///
/// The preset belongs to the build, not to the user's data. Storage rewrites
/// it from [`builtin_preset`] on every start (`storage::install_builtin_preset`)
/// and holds every save to what it wrote (`storage::keep_builtin_preset`), so
/// it can be neither edited nor deleted and always matches the running version.
/// A user who wants something different applies it and saves the result as a
/// preset of their own.
pub(crate) const BUILTIN_PRESET_ID: &str = "preset_mework";

/// The template the built-in preset opens with.
///
/// Fixed rather than minted: storage has to find its own row to rewrite it on
/// every start, and the renderer seed has to name the same one.
pub(crate) const BUILTIN_PRESET_TEMPLATE_ID: &str = "template_preset_mework";

/// Presets earlier builds seeded into the document as ordinary user data, each
/// with the template it opened with. The built-in preset replaces them, so
/// storage removes both on load.
pub(crate) const RETIRED_SEEDED_PRESETS: &[(&str, &str)] = &[
    ("preset_codex", "template_preset_codex"),
    ("preset_claude_code", "template_preset_claude_code"),
];

/// The system prompt the built-in preset opens with: the one `System` row of
/// its template, which `aisdk::step::system_prompt_parts` lifts into the
/// request's system half.
///
/// Engineering practice only. Where the model runs, with which tools and on
/// which machines, is what the host's own sections say, so this says none of it.
pub(crate) const BUILTIN_PRESET_PROMPT: &str = "\
    You are a software engineer working in the user's codebase. You help with engineering tasks: fixing bugs, building features, refactoring, explaining code and reviewing changes.\n\
    \n\
    # How to work\n\
    - Read before you write. Look at the relevant code, its callers and the conventions around it before changing anything, and make new code read like its surroundings: the same naming, idioms and comment density.\n\
    - Do what was asked, no less and no more. Don't quietly narrow the task, and don't add features, refactors, files or abstractions nobody requested. If you notice a real problem outside the task, mention it briefly instead of fixing it.\n\
    - Prefer the smallest change that fully solves the problem, and fix causes rather than symptoms. Leave no dead code, debug output or commented-out blocks behind.\n\
    - Check what you can check. Don't guess at an API, a path, a flag or a behaviour when the code, its documentation or a quick run can tell you.\n\
    - Don't introduce security problems: validate untrusted input at the boundary, never assemble commands or queries from it by string concatenation, and keep credentials out of code and logs.\n\
    \n\
    # Verify before you claim\n\
    - Saying something is done, fixed or passing needs evidence: build it, run the tests or the program, and read the output. If you could not verify something, say so and say why.\n\
    - Report failures as they are, with their output. Never weaken, skip or delete a test to make it pass unless that is the task.\n\
    \n\
    # Actions with consequences\n\
    - Confirm with the user before anything destructive or hard to reverse (deleting or overwriting files, discarding changes, rewriting history, force-pushing) and before anything others will see (pushing, publishing, sending messages). An approval covers the action it was given for, not later ones.\n\
    - Look at what you are about to delete or overwrite before you do it. Don't commit or push unless asked.\n\
    \n\
    # Delegating\n\
    - Hand self-contained, independently checkable pieces of work to subagents, and run independent ones in parallel. Keep work that needs judgement across the whole task in the main thread.\n\
    - A subagent sees only the prompt you give it: include the goal, the relevant paths and constraints, and what to return. Treat what comes back as a claim to check, not a fact.\n\
    \n\
    # Communicating\n\
    - Lead with the answer or the outcome, then the evidence, then anything still open. Be concise and skip preamble and recaps.\n\
    - Reply in the language the user writes in. Cite code as `path:line`.\n\
    - When a request is ambiguous in a way that changes the result, ask. Otherwise make a sensible choice, say what it was, and proceed.";

/// The built-in preset's roles: name, the provider family it runs on, its
/// model, and what it is for.
///
/// Each role is bound to one model, and the binding is kept while that model
/// does not exist yet — Codex lists nothing until the user signs in — so the
/// role starts working the moment its model shows up. The descriptions reach
/// the model as the role listing on `agent_spawn`/`workflow`, so they say what
/// each model is good and bad at and when to pick it.
const BUILTIN_ROLES: &[(&str, ProviderFamily, &str, &str)] = &[
    (
        "Opus",
        ProviderFamily::ClaudeAgent,
        "claude-opus-5-5",
        "Claude Opus 5.5 (Anthropic). Excellent judgement and taste. Takes on ambiguous, open-ended and hard problems, including ones where it is not yet clear what the problem is, and its conclusions can be relied on. Pick it for the hardest work and for anything that needs sound judgement across a wide area: design, diagnosis, review and deciding what to do.",
    ),
    (
        "Sonnet",
        ProviderFamily::ClaudeAgent,
        "claude-sonnet-5-5",
        "Claude Sonnet 5.5 (Anthropic). Has the strengths of Opus in a lighter form: good judgement and able to work through ambiguous problems, though less reliably on the hardest ones, and at a lower cost. Pick it for work that needs judgement but not Opus's full depth.",
    ),
    (
        "Sol",
        ProviderFamily::OpenaiCodex,
        "gpt-6.1-sol",
        "GPT-6.1 Sol (OpenAI). Rigorous, precise reasoning that very rarely makes a mistake, but weak judgement: it does poorly on vague tasks and on ones that call for weighing many things across a wide area, and is at its best on a focused, local problem. Pick it for clearly specified, verifiable work where correctness matters most; give it the exact goal and how to check the result, and keep open-ended decisions away from it.",
    ),
    (
        "Luna",
        ProviderFamily::OpenaiCodex,
        "gpt-6-luna",
        "GPT-6 Luna (OpenAI). Very cheap, with limited reasoning: don't ask it to make judgement calls or to verify anything that is hard to check. Pick it for simple, mechanical, easily checked chores in bulk, such as repetitive edits, searches and lookups, and collecting or summarizing output, and for many parallel workers.",
    ),
];

/// One role of the built-in preset: everything on and thinking on.
///
/// `tools: None` rather than an explicit allowlist, so the role tracks whatever
/// the preset enables instead of freezing today's catalog into a copy per role.
fn builtin_role(name: &str, provider_id: &str, model_id: &str, description: &str) -> AgentDefinition {
    AgentDefinition {
        enabled: true,
        deleted: false,
        name: name.into(),
        description: description.into(),
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
        fetch_provider: None,
        max_results: crate::model::DEFAULT_SEARCH_MAX_RESULTS,
        compression_cutoff: crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT,
        domain_filter: None,
        include_domains: Vec::new(),
        exclude_domains: Vec::new(),
        template_id: None,
    }
}

/// The tools the built-in preset enables out of a document's catalog `tools`:
/// every built-in tool except the names the host derives for itself, and of
/// the shell command tools only `shell`'s — all of them when `shell` is `None`,
/// which only a probe of the machine can narrow (`storage::install_builtin_preset`).
///
/// Drawn from the document's catalog rather than this build's alone, because a
/// preset naming a tool the document does not list fails validation, and that
/// list moves with the renderer's saves. MCP tools the document also lists are
/// not built-in and are left out.
///
/// The memory tools follow the two memory switches, `skill` follows
/// `skill_tool_enabled`, `tool_search` follows `mcp_tool_discovery_enabled`,
/// the task-runtime tools appear only once something can produce a task, the
/// plan tools follow the security level, and the handoff tools follow the
/// conversation's context. Listing any of them here would be
/// inert at best: the renderer strips them again when the preset is applied.
/// Mirrors the renderer's `isHostDerivedToolName` and `seedPresetEnabledTools`.
pub(crate) fn builtin_preset_enabled_tools(
    tools: &[ToolDescriptor],
    shell: Option<crate::shell_backend::ShellBackend>,
) -> Vec<String> {
    let built_in = tool_catalog()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<std::collections::HashSet<_>>();
    tools
        .iter()
        .map(|tool| tool.name.as_str())
        .filter(|name| {
            built_in.contains(*name)
                && !crate::mework_memory::is_memory_tool(name)
                && !crate::agents::is_task_runtime_tool_name(name)
                && !crate::plan_mode::is_plan_mode_tool_name(name)
                && !crate::handoff::is_handoff_tool_name(name)
                && *name != crate::capabilities::SKILL_TOOL
                && *name != crate::capabilities::TOOL_SEARCH_TOOL
                && match crate::shell_backend::ShellBackend::of_tool(name) {
                    Some(backend) => shell.map_or(true, |shell| shell == backend),
                    None => true,
                }
        })
        .map(str::to_owned)
        .collect()
}

/// The built-in preset as this build defines it, against the providers and the
/// tool catalog of the document it goes into.
///
/// The one shell is this machine's and is chosen by storage, which takes a
/// probe. A role whose provider row the document lacks is left out rather than
/// bound to nothing — validation refuses an empty provider id — and returns on
/// the first start after the renderer puts the row back.
pub(crate) fn builtin_preset(
    providers: &[ApiProvider],
    tools: &[ToolDescriptor],
    shell: Option<crate::shell_backend::ShellBackend>,
) -> ConversationPreset {
    let agent_definitions = BUILTIN_ROLES
        .iter()
        .filter_map(|(name, family, model_id, description)| {
            let provider = providers
                .iter()
                .find(|provider| provider.family == *family)?;
            Some(builtin_role(name, &provider.id, model_id, description))
        })
        .collect();
    ConversationPreset {
        id: BUILTIN_PRESET_ID.into(),
        name: "mework".into(),
        description: String::new(),
        template_id: BUILTIN_PRESET_TEMPLATE_ID.into(),
        settings: ConversationPresetSettings {
            enabled_tools: builtin_preset_enabled_tools(tools, shell),
            tool_description_file_id: None,
            agent_definitions,
            // Every child is one of the named roles, so the model cannot route
            // around them by spawning an anonymous one.
            allow_roleless_subagents: false,
            // Nothing is selected: skills, MCP servers and hooks are opt-in per
            // conversation, and that includes the built-in Mework SDK skill.
            hook_ids: Vec::new(),
            skill_ids: Vec::new(),
            mcp_ids: Vec::new(),
            // Native search and fetch, resolved against the conversation
            // model's family at run time.
            web_search: crate::model::ConversationWebSearchSettings {
                max_searches_per_call: 0,
                provider: crate::model::SearchProviderSelection::Native,
                fetch_provider: crate::model::FetchProviderSelection::Native,
                // The basic versions. A newer one is a choice with its own
                // costs, not a default to hand every new conversation.
                native_search_tool: crate::model::NativeSearchTool::default(),
                native_fetch_tool: crate::model::NativeFetchTool::default(),
                max_results: crate::model::DEFAULT_SEARCH_MAX_RESULTS,
                compression_cutoff: crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT,
                // A shipped domain list would be this application deciding what
                // the web is allowed to say, so the filter is off and both lists
                // are the user's to write.
                domain_filter: crate::model::SearchDomainFilterMode::Off,
                include_domains: Vec::new(),
                exclude_domains: Vec::new(),
            },
            web_search_enabled: true,
            security_level: Default::default(),
            // The memory tools are switched by these two rather than named in
            // the list.
            global_memory_enabled: true,
            project_memory_enabled: true,
            // Both capability surfaces load on demand rather than inlining every
            // selected body and every MCP schema into the system prompt.
            skill_tool_enabled: true,
            mcp_tool_discovery_enabled: true,
        },
    }
}

fn product_default_presets(providers: &[ApiProvider], tools: &[ToolDescriptor]) -> PresetLibrary {
    PresetLibrary {
        conversation_presets: vec![builtin_preset(providers, tools, None)],
        // Storage refuses an empty default once presets exist.
        default_conversation_preset_id: BUILTIN_PRESET_ID.into(),
    }
}

pub(crate) fn product_default_document() -> AppDocument {
    let tools = tool_catalog();
    let now = Utc::now();
    let timestamp = |minutes: i64| (now - Duration::minutes(minutes)).to_rfc3339();
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
            draft_conversation: None,
            auto_compact: Default::default(),
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
            },
        },
        presets,
        // A fresh installation has no directory workspace; the renderer opens a
        // draft with none selected and its first message lands in the temporary one.
        workspaces: vec![Workspace {
            id: "__temporary__".into(),
            name: "临时工作区".into(),
            kind: WorkspaceKind::Temporary,
            path: String::new(),
            machine: None,
            additional_workspaces: Vec::new(),
            created_at: timestamp(0),
            default_conversation_preset_id: String::new(),
            last_conversation_settings: None,
            conversations: Vec::new(),
        }],
        tools,
        capabilities: CapabilityCatalog {
            hooks: Vec::new(),
            skills: Vec::new(),
            mcps: Vec::new(),
            lsps: Vec::new(),
            tool_description_files: Vec::new(),
        },
    };
    document
}

#[cfg(not(test))]
pub fn default_document() -> AppDocument {
    product_default_document()
}

#[cfg(test)]
pub fn default_document() -> AppDocument {
    let mut document = product_default_document();
    let enabled_tools = document
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    hydrate_test_settings(&mut document, &enabled_tools);
    document
}

#[cfg(test)]
fn hydrate_test_settings(document: &mut AppDocument, enabled_tools: &[String]) {
    document.presets.conversation_presets = vec![ConversationPreset {
        id: "conversation_default".into(),
        name: "默认".into(),
        description: "测试对话预设。".into(),
        template_id: String::new(),
        settings: ConversationPresetSettings {
            enabled_tools: enabled_tools.to_vec(),
            web_search_enabled: true,
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
            mcp_tool_discovery_enabled: false,
        },
    }];
    document.presets.default_conversation_preset_id = "conversation_default".into();
    document.assets.api_providers = default_api_providers();
    document.global_settings.active_provider_id = Some("openai_responses".into());
    // The product seed ships only the temporary workspace. Host tests address a
    // directory workspace at index 0 and the conversation it carries, so add both here.
    document.workspaces.insert(
        0,
        Workspace {
            id: "ws_default".into(),
            name: "Workspace".into(),
            kind: WorkspaceKind::Directory,
            // `validate_shape` rejects a directory workspace with a blank path.
            path: ".".into(),
            created_at: Utc::now().to_rfc3339(),
            default_conversation_preset_id: String::new(),
            last_conversation_settings: None,
            machine: None,
            additional_workspaces: Vec::new(),
            conversations: vec![Conversation {
                id: "conv_welcome".into(),
                title: String::new(),
                created_at: Utc::now().to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
                settings: ConversationSettings {
                    include_app_data_path: false,
                    enabled_tools: enabled_tools.to_vec(),
                    web_search_enabled: true,
                    hook_ids: Vec::new(),
                    skill_ids: Vec::new(),
                    mcp_ids: Vec::new(),
                    tool_description_file_id: None,
                    agent_definitions: Vec::new(),
                    allow_roleless_subagents: false,
                    web_search: Default::default(),
                    reasoning_effort: Default::default(),
                    security_level: Default::default(),
                    plan_mode_enabled: false,
                    global_memory_enabled: false,
                    project_memory_enabled: false,
                    skill_tool_enabled: false,
                    mcp_tool_discovery_enabled: false,
                    legacy_sandbox: Default::default(),
                    tool_lock: None,
                },
                contexts: Vec::new(),
                queued_messages: Vec::new(),
                branches: Vec::new(),
                user_aborted_tasks: Vec::new(),
                worktrees: Vec::new(),
                run_target: None,
                additional_directories: Vec::new(),
                parent_conversation_id: None,
                fork_of: None,
                handoff_of: None,
                preset_id: String::new(),
                template_id: String::new(),
                attached_workspaces: Vec::new(),
            }],
        },
    );
    if let Some(conversation) = document
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.conversations.first_mut())
    {
        let now = Utc::now();
        let timestamp = |minutes: i64| (now - Duration::minutes(minutes)).to_rfc3339();
        conversation.settings.enabled_tools = enabled_tools.to_vec();
        conversation.contexts = vec![
            ContextItem::System {
                id: "ctx_welcome_system".into(),
                content: "测试对话已创建。".into(),
                local_only: false,
                hook_execution: None,
                tools_added: Vec::new(),
                created_at: timestamp(4),
            },
            ContextItem::User {
                id: "ctx_welcome_user".into(),
                content: "检查工作区，并协助我完成第一个任务。".into(),
                images: Vec::new(),
                files: Vec::new(),
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
                provider_call_id: None,
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
                notice: None,
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

        assert_eq!(chinese.len(), 49);
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
        let document = default_document();

        assert_eq!(document.schema_version, crate::storage::SCHEMA_VERSION);
        assert_eq!(
            document.workspaces[0].conversations[0]
                .settings
                .security_level,
            SecurityLevel::default()
        );
        // Empty workspace-level settings make new conversations use the global default preset.
        assert!(document.workspaces.iter().all(|workspace| workspace
            .default_conversation_preset_id
            .is_empty()
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
