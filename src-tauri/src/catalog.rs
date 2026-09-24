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
            "bash_find_output",
            "Bash（筛选输出）",
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
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("测试失败的原因"),
                    Some("用自然语言描述要在命令输出里找的内容；输出会切块送给决策模型打分，只返回过阈值的片段"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
                ),
            ],
        ),
        descriptor(
            "powershell_find_output",
            "PowerShell（筛选输出）",
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
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("测试失败的原因"),
                    Some("用自然语言描述要在命令输出里找的内容；输出会切块送给决策模型打分，只返回过阈值的片段"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
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
            "zsh_find_output",
            "zsh（筛选输出）",
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
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("测试失败的原因"),
                    Some("用自然语言描述要在命令输出里找的内容；输出会切块送给决策模型打分，只返回过阈值的片段"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
                ),
            ],
        ),
        descriptor(
            "sh_find_output",
            "sh（筛选输出）",
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
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("测试失败的原因"),
                    Some("用自然语言描述要在命令输出里找的内容；输出会切块送给决策模型打分，只返回过阈值的片段"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
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
            "find_content",
            "按描述查找内容",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter("path", "文件路径", StringType, true, None, None, None),
                parameter(
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("处理登录失败的代码"),
                    Some("用自然语言描述要找的内容；文件会切块送给决策模型打分"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
                ),
                parameter(
                    "start_line",
                    "起始行",
                    Number,
                    false,
                    Some(json!(1)),
                    None,
                    Some("从 1 开始"),
                ),
                parameter(
                    "end_line",
                    "结束行",
                    Number,
                    false,
                    None,
                    None,
                    Some("包含该行；留空到文件末尾"),
                ),
            ],
        ),
        descriptor(
            "find_files",
            "按描述查找文件",
            "",
            ToolCategory::Filesystem,
            false,
            vec![
                parameter(
                    "path",
                    "目录",
                    StringType,
                    false,
                    Some(json!(".")),
                    None,
                    None,
                ),
                parameter(
                    "query",
                    "查找文件",
                    StringType,
                    true,
                    None,
                    Some("存放提供商凭据的代码"),
                    Some("用自然语言描述要找的文件；目录列表会切块送给决策模型打分"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
                ),
                parameter(
                    "depth",
                    "递归深度",
                    Number,
                    false,
                    Some(json!(6)),
                    None,
                    Some("0 仅列出当前目录"),
                ),
            ],
        ),
        descriptor(
            "find_output",
            "按描述查找输出",
            "",
            ToolCategory::Shell,
            false,
            vec![
                parameter(
                    "task",
                    "任务地址",
                    StringType,
                    true,
                    None,
                    Some("shell:3"),
                    Some("task_list 里的 shell 任务地址，如 shell:3"),
                ),
                parameter(
                    "query",
                    "查找内容",
                    StringType,
                    true,
                    None,
                    Some("有没有编译错误"),
                    Some("用自然语言描述要在输出里找的内容；输出会切块送给决策模型打分"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
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
                parameter(
                    "query",
                    "查询",
                    StringType,
                    false,
                    None,
                    Some("有没有关于 hydration 的报错"),
                    Some("用自然语言描述要在 Console 里找的内容；按 level 过滤后的日志送给决策模型打分，只返回分数不低于 threshold 的片段。需在对话设置里为本工具开启决策模型参数"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    false,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；与 query 一起给出，只返回分数不低于它的片段"),
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
                parameter(
                    "query",
                    "查询",
                    StringType,
                    false,
                    None,
                    Some("顶部导航里的登录按钮"),
                    Some("用自然语言描述要找的页面元素；可访问性快照送给决策模型打分，只返回分数不低于 threshold 的元素及其选择器，而不是整份快照。需在对话设置里为本工具开启决策模型参数"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    false,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；与 query 一起给出，只返回分数不低于它的元素"),
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
                    false,
                    None,
                    Some(".button"),
                    Some("要检查的元素 CSS 选择器；开启决策模型参数后也可以改用 query"),
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
                parameter(
                    "query",
                    "元素描述",
                    StringType,
                    false,
                    None,
                    Some("对话框里的保存按钮"),
                    Some("用自然语言描述要检查的元素，代替 selector；决策模型从页面元素里选出所指的一个并读取它的样式，都不符合时会选「以上皆非」，按未找到回答。需在对话设置里为本工具开启决策模型参数"),
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
                    false,
                    None,
                    Some("button.primary"),
                    Some("要点击的元素 CSS 选择器；开启决策模型参数后也可以改用 query"),
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
                parameter(
                    "query",
                    "元素描述",
                    StringType,
                    false,
                    None,
                    Some("对话框里的保存按钮"),
                    Some("用自然语言描述要点击的元素，代替 selector；决策模型从页面元素里选出所指的一个并点击，都不符合时会选「以上皆非」，什么也不做。需在对话设置里为本工具开启决策模型参数"),
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
                    false,
                    None,
                    Some("input[name=email]"),
                    Some("要填写的输入框 CSS 选择器；开启决策模型参数后也可以改用 query"),
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
                parameter(
                    "query",
                    "元素描述",
                    StringType,
                    false,
                    None,
                    Some("邮箱输入框"),
                    Some("用自然语言描述要填写的输入框，代替 selector；决策模型从页面元素里选出所指的一个并填写，都不符合时会选「以上皆非」，什么也不做。需在对话设置里为本工具开启决策模型参数"),
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
            "preview_find_logs",
            "按描述查找日志",
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
                    "query",
                    "查找日志",
                    StringType,
                    true,
                    None,
                    Some("有没有关于 hydration 的报错"),
                    Some("用自然语言描述要在日志里找的内容；Console 日志和服务器日志会切块送给决策模型打分"),
                ),
                parameter(
                    "threshold",
                    "分数阈值",
                    Number,
                    true,
                    None,
                    Some("0.6"),
                    Some("0 到 1，最多三位小数；只返回分数不低于它的片段"),
                ),
                parameter(
                    "source",
                    "来源",
                    StringType,
                    false,
                    Some(json!("all")),
                    None,
                    Some("要搜索的日志：all（默认）同时搜 Console 与服务器日志，console 只搜页面 Console，server 只搜服务器输出"),
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
                    Some("[\"a1\", \"shell:1\"]"),
                    Some("任务地址数组：子代理与工作流直接写名称（工作流也可写 workflow:<runId>），后台命令写 shell:<id>，终端写 terminal:<id>，开发服务器写 preview:<serverId>；等待到点名的任务全部给出结果为止，省略时等待本对话全部子代理、工作流与后台命令（不含终端与开发服务器）"),
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
        "find_content" => "Find content by description",
        "find_files" => "Find files by description",
        "find_output" => "Find output by description",
        "bash_find_output" => "Bash, scored output",
        "powershell_find_output" => "PowerShell, scored output",
        "zsh_find_output" => "zsh, scored output",
        "sh_find_output" => "sh, scored output",
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
        "preview_find_logs" => "Find logs by description",
        "agent_spawn" => "Subagent",
        "send_message" => "Send message",
        "followup_task" => "Follow up",
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
        "todo" => "Todo",
        "workflow" => "Workflow",
        "skill" => "Skill",
        "tool_search" => "Tool discovery",
        "fork" => "Fork conversation",
        "plan" => "Plan document",
        "exit_plan_mode" => "Exit plan mode",
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
        "line" => "Line",
        "lines" => "Line limit",
        "operation" => "Operation",
        "max_results" => "Maximum results",
        "message" => "Message",
        "metadata" => "Metadata",
        "name" => "Name",
        "new_text" => "New text",
        "old_text" => "Original text",
        "owner" => "Owner",
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
        "status" => "Status",
        "styles" => "CSS properties",
        "subject" => "Task subject",
        "taskId" => "Task ID",
        "task" => "Task address",
        "tasks" => "Tasks",
        "source" => "Log source",
        "threshold" => "Score threshold",
        "timeout" => "Timeout (ms)",
        "timeout_seconds" => "Timeout (seconds)",
        "token_budget" => "Token budget",
        "target" => "Child agent",
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
        ("find_content", "query") => {
            "Describe what to look for in plain language; the file is cut into chunks the decision model scores."
        }
        ("find_files", "query") => {
            "Describe the files to look for in plain language; the directory listing is cut into groups the decision model scores."
        }
        ("find_files", "depth") => "0 lists only the current directory.",
        ("find_output", "task") => "The shell task address from task_list, e.g. shell:3.",
        ("find_output", "query") => {
            "Describe what to look for in the output in plain language; the output is cut into chunks the decision model scores."
        }
        ("bash_find_output", "query")
        | ("powershell_find_output", "query")
        | ("zsh_find_output", "query")
        | ("sh_find_output", "query") => {
            "Describe what to look for in the command's output in plain language; the output is cut into chunks the decision model scores and only pieces above the threshold are returned."
        }
        ("find_content", "threshold")
        | ("find_files", "threshold")
        | ("find_output", "threshold")
        | ("bash_find_output", "threshold")
        | ("powershell_find_output", "threshold")
        | ("zsh_find_output", "threshold")
        | ("sh_find_output", "threshold")
        | ("preview_find_logs", "threshold") => {
            "0 to 1 with at most three decimals; only pieces scoring at or above it are returned."
        }
        ("find_content", "start_line") => "1-based.",
        ("find_content", "end_line") => "Inclusive; leave empty for the end of the file.",
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
            "Run id from a previous run of this same script; journaled steps replay, the first unjournaled one and everything after it re-runs."
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
        | ("preview_dialog", "serverId")
        | ("preview_find_logs", "serverId") => "Server ID",
        ("preview_console_logs", "query") => {
            "Describe what to look for in the console in plain language; the logs that pass level go to the decision model, and only pieces scoring at or above threshold come back. Needs this tool's decision-model parameters turned on in the conversation settings."
        }
        ("preview_console_logs", "threshold") | ("preview_snapshot", "threshold") => {
            "0 to 1 with at most three decimals, given with query; only pieces scoring at or above it are returned."
        }
        ("preview_snapshot", "query") => {
            "Describe the page element to look for in plain language; the accessibility snapshot goes to the decision model, and only elements scoring at or above threshold come back with their selectors, instead of the whole snapshot. Needs this tool's decision-model parameters turned on in the conversation settings."
        }
        ("preview_click", "query") => {
            "Describe the element to click in plain language, instead of selector; the decision model chooses the one meant and it is clicked, or answers none of the above and nothing is done. Needs this tool's decision-model parameters turned on in the conversation settings."
        }
        ("preview_fill", "query") => {
            "Describe the input to fill in plain language, instead of selector; the decision model chooses the one meant and it is filled, or answers none of the above and nothing is done. Needs this tool's decision-model parameters turned on in the conversation settings."
        }
        ("preview_inspect", "query") => {
            "Describe the element to inspect in plain language, instead of selector; the decision model chooses the one meant and its styles are read, or answers none of the above and the element is reported as not found. Needs this tool's decision-model parameters turned on in the conversation settings."
        }
        ("preview_find_logs", "query") => {
            "Describe what to look for in the logs in plain language; the console and server logs are cut into chunks the decision model scores."
        }
        ("preview_find_logs", "source") => {
            "Which logs to search: 'all' (default) searches both the page console and the dev server output, 'console' only the page console, 'server' only the server output."
        }
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
            "CSS selector (e.g., '.button', '#header'); with decision-model parameters on, query can name the element instead"
        }
        ("preview_inspect", "styles") => {
            "CSS properties to return (e.g., ['padding', 'color']). Defaults to common properties."
        }
        ("preview_click", "selector") => {
            "CSS selector for the element to click; with decision-model parameters on, query can name the element instead"
        }
        ("preview_click", "doubleClick") => "Perform a double-click",
        ("preview_fill", "selector") => {
            "CSS selector for the input element; with decision-model parameters on, query can name the element instead"
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
            "Array of task addresses: a child agent or workflow run by its bare name (a workflow also answers to workflow:<runId>), a background command as shell:<id>, a terminal as terminal:<id>, a dev server as preview:<serverId>. The wait ends once every named task has produced a result. Omit to wait for every child agent, workflow run and background command in this conversation (terminals and dev servers excluded)."
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
        ("zsh", "description") | ("sh", "description") => "List files in the current directory",
        ("find_content", "query") => "The code that handles a failed login",
        ("find_files", "query") => "The code that stores provider credentials",
        ("find_output", "query") => "Are there compile errors",
        ("bash_find_output", "description") => "Show working tree status",
        ("powershell_find_output", "description") => "List files in the current directory",
        ("zsh_find_output", "description") | ("sh_find_output", "description") => {
            "List files in the current directory"
        }
        ("bash_find_output", "query")
        | ("powershell_find_output", "query")
        | ("zsh_find_output", "query")
        | ("sh_find_output", "query") => "Why the tests failed",
        ("preview_find_logs", "query") | ("preview_console_logs", "query") => {
            "Any errors about hydration"
        }
        ("preview_snapshot", "query") => "The login button in the top navigation",
        ("preview_click", "query") | ("preview_inspect", "query") => {
            "The Save button in the dialog"
        }
        ("preview_fill", "query") => "The email field",
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
        ("todo", "subject") => "Implement user authentication",
        ("todo", "description") => "Add login and signup endpoints and cover them with tests.",
        ("todo", "activeForm") => "Implementing user authentication",
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
        fetch_provider: None,
        max_results: crate::model::DEFAULT_SEARCH_MAX_RESULTS,
        compression_cutoff: crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT,
        domain_filter: None,
        include_domains: Vec::new(),
        exclude_domains: Vec::new(),
        template_id: None,
    }
}

/// Everything in the catalog except the names the host derives for itself and
/// the decision-model tools.
///
/// The memory tools follow the two memory switches, `skill` follows
/// `skill_tool_enabled`, `tool_search` follows `mcp_tool_discovery_enabled`,
/// the task-runtime tools appear only once something can produce a task, and
/// the plan tools follow the security level. Listing any of them here would be
/// inert at best: the renderer strips them again when the preset is applied.
/// Mirrors the renderer's `isHostDerivedToolName`.
///
/// The decision-model tools are withheld because none of them works until the
/// TypeSafe key is configured. Every shell's command tool is listed here; the
/// first launch narrows them to the one this machine prefers
/// (`storage::seed_local_shell`), which only a probe of the machine can say.
/// The renderer mirror is `src/seed.ts::seedPresetEnabledTools`.
fn seed_preset_enabled_tools(tools: &[ToolDescriptor]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| {
            !crate::mework_memory::is_memory_tool(&tool.name)
                && !crate::agents::is_task_runtime_tool_name(&tool.name)
                && !crate::plan_mode::is_plan_mode_tool_name(&tool.name)
                && tool.name != crate::capabilities::SKILL_TOOL
                && tool.name != crate::capabilities::TOOL_SEARCH_TOOL
                && !crate::decision_tools::is_decision_tool_name(&tool.name)
        })
        .map(|tool| tool.name.clone())
        .collect()
}

fn seed_preset(
    id: &str,
    name: &str,
    tools: &[ToolDescriptor],
    web_search: crate::model::ConversationWebSearchSettings,
    agent_definitions: Vec<AgentDefinition>,
) -> ConversationPreset {
    ConversationPreset {
        id: id.into(),
        name: name.into(),
        description: String::new(),
        // Both shipped presets open with the system prompt seeded alongside
        // them; `storage::seed_preset_templates` writes the body the first time
        // this document is initialized, and a dangling id reads as "no template".
        template_id: seeded_template_id(id).into(),
        settings: ConversationPresetSettings {
            enabled_tools: seed_preset_enabled_tools(tools),
            tool_description_file_id: None,
            agent_definitions,
            // Every child is one of the three named roles, so the model cannot
            // route around them by spawning an anonymous one.
            allow_roleless_subagents: false,
            // The built-in capability files are written and their ids minted by
            // `capability_seed` at first launch, which then fills these in — the
            // ids hash an absolute path this function cannot know. Hooks stay
            // unselected on purpose: a dangling hook id fails every run closed,
            // and the built-ins are meant to be deletable.
            hook_ids: Vec::new(),
            skill_ids: Vec::new(),
            mcp_ids: Vec::new(),
            web_search,
            // The shipped presets mirror CLIs that search the web, so the switch
            // is on; which of the two web tools that grants follows the backend.
            web_search_enabled: true,
            security_level: Default::default(),
            // Every tool but the decision-model ones is on, and the memory
            // tools are switched by these two rather than named in the list.
            global_memory_enabled: true,
            project_memory_enabled: true,
            // Both capability surfaces load on demand rather than inlining every
            // selected body and every MCP schema into the system prompt.
            skill_tool_enabled: true,
            mcp_tool_discovery_enabled: true,
            decision_parameter_modes: Default::default(),
            decision_miss_scoring: Default::default(),
            sandbox: Default::default(),
        },
    }
}

/// The template id a shipped preset opens with, or `""` for any other preset.
///
/// Fixed rather than minted, because `storage::seed_preset_templates` has to
/// recognize its own rows across restarts to stay idempotent, and a random id
/// would leave the renderer seed unable to name the same template.
pub(crate) fn seeded_template_id(preset_id: &str) -> &'static str {
    match preset_id {
        CODEX_PRESET_ID => "template_preset_codex",
        CLAUDE_CODE_PRESET_ID => "template_preset_claude_code",
        _ => "",
    }
}

/// The two shipped presets, each mirroring the agent fleet of the CLI it is
/// named after.
///
/// The Codex roles are bound to models that do not exist until the user signs
/// in and fetches the catalog. That is deliberate and is why a dangling
/// `Explicit` pair is now kept verbatim: the binding waits, and the role starts
/// working the moment its model shows up.
///
/// The two web legs differ because the families differ, not by oversight.
/// Search is `Native` for both. Fetch is `Native` only for Codex, whose family
/// folds retrieval into its one `web_search` tool — that is the family's own
/// shape, so it grants one web tool rather than two. `ClaudeAgent` supports
/// neither native leg (`web_search::family_supports_native_fetch`), so the
/// Claude Code preset ships no fetch backend at all: its model answers from
/// what it knows, and the user picks a fetch backend when one is wanted.
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
    let web_search = |fetch_provider| crate::model::ConversationWebSearchSettings {
        max_searches_per_call: 0,
        provider: crate::model::SearchProviderSelection::Native,
        fetch_provider,
        // Factory presets ship the basic versions. A newer one is a choice with
        // its own costs, not a default to hand every new conversation.
        native_search_tool: crate::model::NativeSearchTool::default(),
        native_fetch_tool: crate::model::NativeFetchTool::default(),
        max_results: crate::model::DEFAULT_SEARCH_MAX_RESULTS,
        compression_cutoff: crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT,
        // A shipped domain list would be this application deciding what the web
        // is allowed to say, so the filter is off and both lists are the user's
        // to write.
        domain_filter: crate::model::SearchDomainFilterMode::Off,
        include_domains: Vec::new(),
        exclude_domains: Vec::new(),
    };
    PresetLibrary {
        conversation_presets: vec![
            seed_preset(
                CODEX_PRESET_ID,
                "Codex",
                tools,
                web_search(crate::model::FetchProviderSelection::Native),
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
                web_search(crate::model::FetchProviderSelection::Native),
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
            decision_parameter_modes: Default::default(),
            decision_miss_scoring: Default::default(),
            sandbox: Default::default(),
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
                    global_memory_enabled: false,
                    project_memory_enabled: false,
                    skill_tool_enabled: false,
                    mcp_tool_discovery_enabled: false,
                    decision_parameter_modes: Default::default(),
                    decision_miss_scoring: Default::default(),
                    remembered_decision_forms: Default::default(),
                    remembered_tool_families: Default::default(),
                    sandbox: Default::default(),
                    tool_lock: None,
                },
                contexts: Vec::new(),
                queued_messages: Vec::new(),
                branches: Vec::new(),
                user_aborted_tasks: Vec::new(),
                worktree: None,
                run_target: None,
                additional_directories: Vec::new(),
                parent_conversation_id: None,
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

        assert_eq!(chinese.len(), 56);
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
