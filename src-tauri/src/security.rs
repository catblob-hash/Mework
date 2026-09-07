use std::{
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

use crate::{
    model::{JsonObject, SecurityLevel, ToolExecutionRequest},
    path_guard::{canonical_workspace, resolve_existing_with_scope, resolve_for_write_with_scope},
};

pub use crate::path_guard::ExecutionScope;

const MAX_PATH_CHARS: usize = 4096;
const MAX_COMMAND_CHARS: usize = 64 * 1024;
const MAX_SHELL_ANALYSIS_CHARS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationEffect {
    Read,
    Write,
    Unbounded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityDecision {
    pub requires_approval: bool,
    /// Circuit-breaker prompts cannot be suppressed by a permissive mode.
    pub mandatory_prompt: bool,
    pub risk_level: RiskLevel,
    pub rule_id: &'static str,
    pub reason: String,
    pub scope: ExecutionScope,
    pub effect: OperationEffect,
    pub target: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum PathMode {
    Existing { default: Option<&'static str> },
    WriteTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellKind {
    Bash,
    PowerShell,
}

/// Classifies one trusted built-in tool request.
///
/// `workspace` and `app_data` must come from the backend's persisted state and
/// Tauri path resolver, not renderer-provided policy fields. The request is
/// used only for the built-in tool name and arguments. Unknown tools and
/// malformed security-relevant arguments are rejected even under full access.
pub fn classify(
    level: SecurityLevel,
    workspace: &Path,
    app_data: &Path,
    request: &ToolExecutionRequest,
) -> Result<SecurityDecision, String> {
    // Plan mode refuses filesystem writes outright. A screenshot is the one
    // write that judgement gets wrong: the PNG is how the browser hands an
    // observation back, not a change to the user's work, so it drops to an
    // ordinary approval instead of being refused.
    let mut filesystem_level = level;
    let (effect, path_mode) = match request.tool_name.as_str() {
        "ls" | "grep" | "find" => (
            OperationEffect::Read,
            Some(PathMode::Existing { default: Some(".") }),
        ),
        "read" => (
            OperationEffect::Read,
            Some(PathMode::Existing { default: None }),
        ),
        "write" => (OperationEffect::Write, Some(PathMode::WriteTarget)),
        "edit" => (
            OperationEffect::Write,
            Some(PathMode::Existing { default: None }),
        ),
        "powershell" | "bash" => {
            let command =
                required_string(&request.input, "command", MAX_COMMAND_CHARS, "command argument")?;
            let kind = if request.tool_name == "powershell" {
                ShellKind::PowerShell
            } else {
                ShellKind::Bash
            };
            return classify_shell(level, workspace, app_data, kind, command);
        }
        // The whole browser surface is one tool multiplexed by `action`, so the policy is chosen
        // per action. A missing or unknown action is refused rather than defaulted: guessing one
        // would classify an interaction as an observation.
        "playwright" => {
            use crate::browser::PlaywrightAction as Action;
            match Action::from_input(&request.input)? {
                // Reading the tab roster and retargeting later actions read and rearrange local
                // state without touching any page, so they classify with the other prompt-free
                // browser observations.
                Action::Snapshot | Action::Wait | Action::Resize | Action::TabList => {
                    return classify_browser_local(workspace, app_data)
                }
                Action::TabSelect => {
                    validate_required_string(&request.input, "tab", 256, "tab argument")?;
                    return classify_browser_local(workspace, app_data);
                }
                Action::Screenshot => {
                    if filesystem_level == SecurityLevel::Plan {
                        filesystem_level = SecurityLevel::RequestApproval;
                    }
                    (OperationEffect::Write, Some(PathMode::WriteTarget))
                }
                Action::Navigate => {
                    validate_required_string(&request.input, "url", 8_192, "URL argument")?;
                    return Ok(classify_unbounded(level));
                }
                Action::TabClose => {
                    validate_required_string(&request.input, "tab", 256, "tab argument")?;
                    return Ok(classify_unbounded(level));
                }
                // Closing the conversation's pages discards page state the same way a tab close
                // does; the next action reopens a blank page.
                Action::Close => return Ok(classify_unbounded(level)),
                Action::Evaluate => {
                    validate_required_string(
                        &request.input,
                        "script",
                        MAX_COMMAND_CHARS,
                        "JavaScript argument",
                    )?;
                    return Ok(classify_unbounded(level));
                }
                Action::Click
                | Action::Type
                | Action::FillForm
                | Action::Select
                | Action::Hover
                | Action::Key
                | Action::Scroll
                | Action::Dialog
                | Action::TabNew => return Ok(classify_unbounded(level)),
                action @ (Action::Console | Action::Network) => {
                    return Ok(classify_browser_sensitive(level, action));
                }
                Action::FileUpload => {
                    return classify_browser_file_upload(
                        level,
                        workspace,
                        app_data,
                        &request.input,
                    )
                }
                // The image number only means something inside a live run's transcript,
                // so there is nothing a manual execution could resolve it against.
                Action::UploadImage => {
                    return Err(
                        "playwright upload_image requires the model run loop to resolve the image number in the conversation; it cannot be manually executed or approved separately"
                            .into(),
                    )
                }
            }
        }
        "web_search" => {
            return Err(format!(
                "Network tool {} must be scheduled by the model run loop from trusted search configuration; it cannot be manually executed or approved separately",
                request.tool_name
            ))
        }
        // Orchestration and agent tools are executed by the model run loop
        // itself; manual execution and approval requests must not treat them
        // as ordinary tools. The retired `subagent` name stays rejected so
        // legacy timeline entries cannot be re-executed either.
        "subagent" | "subagent_update" | "structured_output" | "ask_user" | "todo"
        | "agent_spawn" | "agent_send" | "send_message" | "followup_task" | "task_wait"
        | "task_list" | "workflow" | "workflow_step" | "skill" | "fork" | "plan"
        | "exit_plan_mode" | "enter_plan_mode" => {
            return Err(format!(
                "Orchestration tool {} may only be scheduled by the model run loop; it cannot be manually executed or approved separately",
                request.tool_name
            ))
        }
        "read_global_memory" | "read_project_memory" | "create_global_memory"
        | "create_project_memory" | "edit_global_memory" | "edit_project_memory" => {
            return Err(format!(
                "Long-term memory tool {} requires the host to inject the current model identity; it cannot be manually executed or approved separately",
                request.tool_name
            ))
        }
        name => return Err(format!("Security classifier does not support unknown tool: {name}")),
    };

    let path_mode = path_mode.expect("filesystem tools always have path semantics");
    let requested = match path_mode {
        PathMode::Existing { default } => path_argument(&request.input, default)?,
        PathMode::WriteTarget => path_argument(&request.input, None)?,
    };

    let workspace = canonical_workspace(workspace)?;
    let app_data =
        canonical_workspace(app_data).map_err(|error| format!("Could not access application data directory: {error}"))?;
    let mut roots = vec![workspace.clone()];
    if app_data != workspace {
        roots.push(app_data.clone());
    }
    let denied = [app_data.join("memory")];
    let restricted = ExecutionScope::restricted(roots).denying(denied.clone());
    let unrestricted = ExecutionScope::Unrestricted.denying(denied);
    // Authorization judges scope, not existence. The write resolver also handles
    // missing read targets by verifying the nearest existing ancestor, and rejects
    // broken symlinks. Execution still requires the actual target to exist.
    let target = resolve_for_write_with_scope(&workspace, &requested, &unrestricted)?;
    let is_trusted = resolve_for_write_with_scope(&workspace, &requested, &restricted).is_ok();
    let protected_app_data =
        effect == OperationEffect::Write && is_protected_app_data_target(&target, &app_data);

    classify_filesystem(
        filesystem_level,
        &request.tool_name,
        effect,
        target,
        is_trusted,
        protected_app_data,
        restricted,
        unrestricted,
    )
}

/// Classifies every built-in model-visible Mework tool. Model-only host tools
/// are included here so the catalog has one auditable safety matrix, while
/// `classify` continues to reject attempts to execute those tools manually.
/// Dynamically discovered MCP tools use the separate high-risk approval path.
pub fn classify_model_call(
    level: SecurityLevel,
    workspace: &Path,
    app_data: &Path,
    request: &ToolExecutionRequest,
) -> Result<SecurityDecision, String> {
    let internal = match request.tool_name.as_str() {
        "web_search" | "web_fetch" => {
            return classify_web_network(level, &request.tool_name, &request.input)
        }
        "workflow" => Some(SecurityDecision {
            // Fan-out is broad enough to deserve one explicit authorization,
            // but it is still an ordinary orchestration of tools this
            // conversation already enabled — every step re-enters the same
            // classifier under the same level. Full access therefore clears it,
            // exactly as it clears the `web_search` executor boundary.
            // `mandatory_prompt` stays
            // false so the level, not the tool, decides. The global memory gate
            // above keeps its mandatory prompt because it crosses a boundary the
            // level does not describe.
            requires_approval: level != SecurityLevel::FullAccess,
            mandatory_prompt: false,
            risk_level: RiskLevel::High,
            effect: OperationEffect::Unbounded,
            scope: ExecutionScope::Restricted {
                roots: vec![workspace.to_path_buf()],
            },
            target: None,
            rule_id: "workflow.orchestrated_fan_out",
            reason: "工作流将并发派生多个子代理执行计划，需要一次明确授权".into(),
        }),
        "workflow_step" => {
            return Err("workflow_step is synthetic workflow-internal context and cannot be called as a model tool".into())
        }
        // Sends a conversation image attachment to the page. The model cites a
        // transcript number and the run loop materializes the bytes itself, so
        // unlike the `file_upload` action there is no model-supplied filesystem
        // path to scope — only the same "local data leaves for the page"
        // approval line. Every other action falls through to `classify`.
        "playwright"
            if crate::browser::PlaywrightAction::from_input(&request.input)?
                == crate::browser::PlaywrightAction::UploadImage =>
        {
            if !request.input.contains_key("image_id") {
                return Err("Missing image_id argument".into());
            }
            Some(SecurityDecision {
                requires_approval: level != SecurityLevel::FullAccess,
                mandatory_prompt: false,
                risk_level: RiskLevel::High,
                rule_id: "browser.image_upload",
                reason: "Will send the conversation image attachment to the current web page".into(),
                scope: ExecutionScope::Unrestricted,
                effect: OperationEffect::Unbounded,
                target: None,
            })
        }
        // `structured_output` belongs here rather than with the state-changing
        // names below: it hands this run's own result back to its parent and
        // mutates nothing that outlives the turn. `skill` belongs here for the
        // same shape of reason: the body it returns was read from disk by the
        // trusted request builder before the turn started, so the call itself
        // touches nothing — it is a lookup in host memory. `fork` classifies as
        // low for a third reason: it creates nothing. At every access level,
        // full access included, it only raises a card, and the user's answer on
        // that card — not this classification — is what may create a child.
        "ask_user" | "task_wait" | "task_list" | "send_message" | "structured_output" | "skill"
        | "fork" | "read_global_memory" | "read_project_memory" | "exit_plan_mode"
        | "enter_plan_mode" => Some(internal_decision(
            RiskLevel::Low,
            "host.read_or_coordinate",
            "只读取本轮宿主状态、等待结果或在同一对话内协调，不触碰文件、Shell 或外部网络",
        )),
        // The plan document lives in the conversation store, not on disk, so
        // writing it is a host state change even while plan mode refuses every
        // filesystem write. An unreadable or unknown action classifies as the
        // write.
        "plan" => {
            let action = request.input.get("action").and_then(Value::as_str);
            Some(if action == Some("read") {
                internal_decision(
                    RiskLevel::Low,
                    "host.read_or_coordinate",
                    "只读取本轮宿主状态，不触碰文件、Shell 或外部网络",
                )
            } else {
                internal_decision(
                    RiskLevel::Medium,
                    "host.local_state_change",
                    "会修改当前对话的宿主状态，但不直接触碰文件、Shell 或外部网络",
                )
            })
        }
        // The tier is part of the tool name, so there is no scope argument to
        // validate and no way for a model to talk its way from project into
        // global memory.
        "create_global_memory" | "edit_global_memory" => Some(SecurityDecision {
            // Global memory applies in every workspace, so neither FullAccess
            // nor a hook may turn it into an implicit background mutation.
            requires_approval: true,
            mandatory_prompt: true,
            risk_level: RiskLevel::High,
            rule_id: "memory.global_persistent_mutation",
            reason: "会修改所有项目都会自动加载的全局持久记忆，必须由用户直接确认".into(),
            scope: ExecutionScope::Unrestricted,
            effect: OperationEffect::Write,
            target: None,
        }),
        "create_project_memory" | "edit_project_memory" => Some(internal_decision(
            RiskLevel::Medium,
            "host.local_state_change",
            "会修改当前项目的持久记忆，但不直接触碰文件、Shell 或外部网络",
        )),
        "followup_task" => Some(internal_decision(
            RiskLevel::Medium,
            "host.local_state_change",
            "会修改当前对话的宿主状态，但不直接触碰文件、Shell 或外部网络",
        )),
        // One tool, four actions: `todo` merged its per-operation catalog
        // entries, so the read/write split can no longer be read off the tool
        // name. `get`/`list` only project host state; `create`/`update` mutate
        // the conversation's own task record.
        // An unreadable or unknown action classifies as a mutation — failing
        // toward the stricter tier is the only safe default here.
        "todo" => {
            let action = request.input.get("action").and_then(Value::as_str);
            Some(if matches!(action, Some("get") | Some("list")) {
                internal_decision(
                    RiskLevel::Low,
                    "host.read_or_coordinate",
                    "只读取本轮宿主状态，不触碰文件、Shell 或外部网络",
                )
            } else {
                internal_decision(
                    RiskLevel::Medium,
                    "host.local_state_change",
                    "会修改当前对话的宿主状态，但不直接触碰文件、Shell 或外部网络",
                )
            })
        }
        // Delegation requires approval only in RequestApproval mode; every child tool
        // call re-enters this classifier under the conversation's security level.
        // Plan mode keeps the prompt: a child inherits plan mode and is read-only,
        // but the user is still deciding what happens, so spawning is not silent.
        "agent_spawn" => Some(SecurityDecision {
            requires_approval: matches!(
                level,
                SecurityLevel::RequestApproval | SecurityLevel::Plan
            ),
            mandatory_prompt: false,
            risk_level: RiskLevel::Medium,
            rule_id: "agent.delegation",
            reason: "会派生子代理；子代理的每个后续工具调用仍继承本对话的安全策略".into(),
            scope: ExecutionScope::Unrestricted,
            effect: OperationEffect::Write,
            target: None,
        }),
        // Child-only progress reporting: never in the public catalog, but the
        // classifier still has to name it rather than fall through to unknown.
        "subagent_update" => Some(internal_decision(
            RiskLevel::Medium,
            "host.local_state_change",
            "会向父代理报告子代理状态",
        )),
        _ => None,
    };
    internal.map_or_else(|| classify(level, workspace, app_data, request), Ok)
}

fn classify_web_network(
    level: SecurityLevel,
    tool_name: &str,
    input: &JsonObject,
) -> Result<SecurityDecision, String> {
    // A single authorization covers an entire `web_search` or `web_fetch` call,
    // including every page the search opens or every URL the fetch reads.
    match tool_name {
        "web_search" => {
            validate_required_string(input, "query", 200, "web-search query")?;
            Ok(SecurityDecision {
                requires_approval: level != SecurityLevel::FullAccess,
                mandatory_prompt: false,
                risk_level: RiskLevel::High,
                rule_id: "web.search",
                reason:
                    "联网搜索会把这句查询发给对话选定的搜索后端，并把它返回的不可信网页内容送入上下文"
                        .into(),
                scope: ExecutionScope::Unrestricted,
                effect: OperationEffect::Unbounded,
                target: None,
            })
        }
        "web_fetch" => {
            let urls = input
                .get("urls")
                .and_then(|value| value.as_array())
                .ok_or_else(|| "Missing web-fetch urls".to_owned())?;
            if urls.is_empty() {
                return Err("web-fetch urls must not be empty".into());
            }
            if urls.len() > crate::model::MAX_SEARCH_INPUTS {
                return Err(format!(
                    "web-fetch accepts at most {} URLs per call",
                    crate::model::MAX_SEARCH_INPUTS
                ));
            }
            for value in urls {
                let url = value
                    .as_str()
                    .ok_or_else(|| "Each web-fetch urls entry must be a string".to_owned())?;
                if url.trim().is_empty() || url.chars().count() > 2_048 {
                    return Err("Invalid web-fetch URL".into());
                }
            }
            Ok(SecurityDecision {
                requires_approval: level != SecurityLevel::FullAccess,
                mandatory_prompt: false,
                risk_level: RiskLevel::High,
                rule_id: "web.fetch",
                reason: "网页抓取会由宿主取回这些地址的正文，并把不可信网页内容送入上下文"
                    .into(),
                scope: ExecutionScope::Unrestricted,
                effect: OperationEffect::Unbounded,
                target: None,
            })
        }
        other => Err(format!("Unknown network tool: {other}")),
    }
}

fn internal_decision(
    risk_level: RiskLevel,
    rule_id: &'static str,
    reason: &str,
) -> SecurityDecision {
    SecurityDecision {
        requires_approval: false,
        mandatory_prompt: false,
        risk_level,
        rule_id,
        reason: reason.into(),
        scope: ExecutionScope::Unrestricted,
        effect: if risk_level == RiskLevel::Low {
            OperationEffect::Read
        } else {
            OperationEffect::Write
        },
        target: None,
    }
}

fn classify_browser_local(workspace: &Path, app_data: &Path) -> Result<SecurityDecision, String> {
    let workspace = canonical_workspace(workspace)?;
    let app_data =
        canonical_workspace(app_data).map_err(|error| format!("Could not access application data directory: {error}"))?;
    let mut roots = vec![workspace.clone()];
    if app_data != workspace {
        roots.push(app_data.clone());
    }
    Ok(SecurityDecision {
        requires_approval: false,
        mandatory_prompt: false,
        risk_level: RiskLevel::Low,
        rule_id: "browser.local_observation",
        reason: "只读取或调整本机内置浏览器，不直接产生外部页面操作".into(),
        scope: ExecutionScope::restricted(roots).denying([app_data.join("memory")]),
        effect: OperationEffect::Read,
        target: None,
    })
}

#[derive(Clone, Debug)]
struct ShellAssessment {
    effect: OperationEffect,
    risk_level: RiskLevel,
    rule_id: &'static str,
    reason: String,
    mandatory_prompt: bool,
}

impl ShellAssessment {
    fn read(rule_id: &'static str, reason: impl Into<String>) -> Self {
        Self {
            effect: OperationEffect::Read,
            risk_level: RiskLevel::Low,
            rule_id,
            reason: reason.into(),
            mandatory_prompt: false,
        }
    }

    fn workspace_write(rule_id: &'static str, reason: impl Into<String>) -> Self {
        Self {
            effect: OperationEffect::Write,
            risk_level: RiskLevel::Medium,
            rule_id,
            reason: reason.into(),
            mandatory_prompt: false,
        }
    }

    fn unbounded(rule_id: &'static str, reason: impl Into<String>) -> Self {
        Self {
            effect: OperationEffect::Unbounded,
            risk_level: RiskLevel::High,
            rule_id,
            reason: reason.into(),
            mandatory_prompt: false,
        }
    }

    fn circuit_breaker(rule_id: &'static str, reason: impl Into<String>) -> Self {
        Self {
            mandatory_prompt: true,
            ..Self::unbounded(rule_id, reason)
        }
    }
}

fn classify_shell(
    level: SecurityLevel,
    workspace: &Path,
    app_data: &Path,
    kind: ShellKind,
    command: &str,
) -> Result<SecurityDecision, String> {
    let workspace = canonical_workspace(workspace)?;
    let app_data =
        canonical_workspace(app_data).map_err(|error| format!("Could not access application data directory: {error}"))?;
    let mut roots = vec![workspace.clone()];
    if app_data != workspace {
        roots.push(app_data);
    }

    let assessment = if command.chars().count() > MAX_SHELL_ANALYSIS_CHARS {
        ShellAssessment::unbounded(
            "shell.analysis_limit",
            format!("命令超过静态分析上限 {MAX_SHELL_ANALYSIS_CHARS} 字符，无法证明为只读操作"),
        )
    } else {
        analyze_shell(kind, command, &workspace, &roots)
    };
    // Shell calls require approval below FullAccess. Static analysis determines risk,
    // effect, and circuit breakers; mandatory prompts apply at every level.
    let requires_approval = assessment.mandatory_prompt || level != SecurityLevel::FullAccess;
    Ok(SecurityDecision {
        requires_approval,
        mandatory_prompt: assessment.mandatory_prompt,
        risk_level: assessment.risk_level,
        rule_id: assessment.rule_id,
        reason: assessment.reason,
        // Mework has no OS-level shell sandbox. A statically safe verdict can
        // suppress a prompt, but it must never be misrepresented as an
        // execution boundary to the subprocess layer.
        scope: ExecutionScope::Unrestricted,
        effect: assessment.effect,
        target: None,
    })
}

fn analyze_shell(
    kind: ShellKind,
    command: &str,
    workspace: &Path,
    roots: &[PathBuf],
) -> ShellAssessment {
    if has_network_path(command) {
        return ShellAssessment::unbounded(
            "shell.network_path",
            "命令包含 UNC 或网络路径，访问时可能向远端主机发送 Windows 凭据",
        );
    }
    if has_background_or_call_operator(kind, command) {
        return ShellAssessment::unbounded(
            "shell.background_or_call_operator",
            "命令包含后台执行或 PowerShell 调用运算符，后续载荷与完成状态无法可靠复核",
        );
    }
    if has_dynamic_shell_expansion(kind, command) {
        if looks_like_recursive_delete(kind, command) {
            return ShellAssessment::circuit_breaker(
                "shell.unverifiable_recursive_delete",
                "递归删除目标包含变量、命令替换或动态表达式，静态分类器无法验证最终路径",
            );
        }
        return ShellAssessment::unbounded(
            "shell.dynamic_expansion",
            "命令包含命令替换、进程替换、动态调用或表达式执行，静态分类器无法验证实际载荷",
        );
    }
    if kind == ShellKind::PowerShell && has_powershell_non_filesystem_provider(command) {
        return ShellAssessment::unbounded(
            "powershell.non_filesystem_provider",
            "PowerShell 命令引用注册表、证书、环境变量或其他非文件系统 Provider",
        );
    }
    if looks_like_critical_delete(kind, command) {
        return ShellAssessment::circuit_breaker(
            "shell.critical_delete",
            "删除操作可能递归作用于文件系统根目录、用户主目录或其他关键系统路径",
        );
    }

    let segments = match split_shell_segments(kind, command) {
        Ok(segments) if !segments.is_empty() => segments,
        Ok(_) => {
            return ShellAssessment::unbounded("shell.empty_analysis", "命令没有可静态分析的子命令")
        }
        Err(reason) => return ShellAssessment::unbounded("shell.parse_failed", reason),
    };

    let mut combined = ShellAssessment::read(
        "shell.read_only",
        "所有子命令都属于内置只读集合，且未发现越界路径或写入型参数",
    );
    for segment in segments {
        let next = analyze_shell_segment(kind, &segment, workspace, roots);
        if next.mandatory_prompt {
            return next;
        }
        if next.risk_level > combined.risk_level {
            combined.rule_id = next.rule_id;
            combined.reason = next.reason.clone();
        }
        combined.risk_level = combined.risk_level.max(next.risk_level);
        combined.effect = combine_effect(combined.effect, next.effect);
    }
    combined
}

fn combine_effect(left: OperationEffect, right: OperationEffect) -> OperationEffect {
    match (left, right) {
        (OperationEffect::Unbounded, _) | (_, OperationEffect::Unbounded) => {
            OperationEffect::Unbounded
        }
        (OperationEffect::Write, _) | (_, OperationEffect::Write) => OperationEffect::Write,
        _ => OperationEffect::Read,
    }
}

fn analyze_shell_segment(
    kind: ShellKind,
    segment: &str,
    workspace: &Path,
    roots: &[PathBuf],
) -> ShellAssessment {
    if contains_unsafe_redirection(kind, segment) {
        return ShellAssessment::unbounded(
            "shell.redirection",
            "子命令包含输出重定向或无法静态解析的输入重定向",
        );
    }
    let mut tokens = match tokenize_shell_segment(kind, segment) {
        Ok(tokens) if !tokens.is_empty() => tokens,
        Ok(_) => return ShellAssessment::read("shell.comment", "子命令只包含注释或空白"),
        Err(reason) => return ShellAssessment::unbounded("shell.tokenize_failed", reason),
    };
    strip_safe_environment_assignments(&mut tokens);
    if tokens.is_empty() {
        return ShellAssessment::read(
            "shell.environment_assignment",
            "只设置了当前子进程的环境变量",
        );
    }
    if let Err(reason) = strip_process_wrappers(kind, &mut tokens) {
        return ShellAssessment::unbounded("shell.wrapper_unverifiable", reason);
    }
    if tokens.is_empty() {
        return ShellAssessment::unbounded(
            "shell.wrapper_missing_command",
            "进程包装器后没有可分析的实际命令",
        );
    }

    match kind {
        ShellKind::Bash => analyze_bash_tokens(&tokens, workspace, roots),
        ShellKind::PowerShell => analyze_powershell_tokens(&tokens, workspace, roots),
    }
}

fn analyze_bash_tokens(tokens: &[String], workspace: &Path, roots: &[PathBuf]) -> ShellAssessment {
    if tokens[0].contains('/') || tokens[0].contains('\\') {
        return ShellAssessment::unbounded(
            "shell.explicit_executable_path",
            "命令通过显式路径选择可执行文件，不能仅凭文件名白名单证明其实现安全",
        );
    }
    let command = command_basename(&tokens[0]).to_ascii_lowercase();
    if command == "xargs" {
        return ShellAssessment::unbounded(
            "shell.xargs_data_dependent_command",
            "xargs 会把运行时输入拼接为新的命令或参数，静态分类器无法看到最终载荷",
        );
    }
    if matches!(command.as_str(), "watch" | "setsid" | "ionice" | "flock") {
        return ShellAssessment::unbounded(
            "shell.exec_wrapper",
            format!("{command} 可以反复、异步或带锁执行任意子命令"),
        );
    }
    if command == "git" {
        return analyze_git(tokens, workspace, roots);
    }
    if command == "cd" {
        return analyze_cd(tokens.get(1).map(String::as_str), workspace, roots);
    }
    if command == "find"
        && tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-delete"
                    | "-fls"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
            )
        })
    {
        return ShellAssessment::unbounded(
            "shell.find_side_effect",
            "find 使用了可执行、删除或写文件的参数",
        );
    }
    if matches!(command.as_str(), "find" | "wc" | "du")
        && tokens.iter().any(|token| {
            matches!(token.as_str(), "-files0-from" | "--files0-from")
                || token.starts_with("-files0-from=")
                || token.starts_with("--files0-from=")
        })
    {
        return ShellAssessment::unbounded(
            "shell.runtime_path_list",
            format!("{command} 会在运行时从文件或标准输入读取额外路径，静态参数中看不到最终目标"),
        );
    }
    if matches!(command.as_str(), "find" | "file")
        && tokens
            .iter()
            .skip(1)
            .any(|token| contains_shell_meta(token))
    {
        return ShellAssessment::unbounded(
            "shell.flag_glob",
            format!("{command} 的未解析 glob 可能在展开后变成写入或执行型参数"),
        );
    }
    if command == "file"
        && tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "-m" | "--magic-file" | "-f" | "--files-from" | "-C" | "--compile"
            ) || token.starts_with("--magic-file=")
                || token.starts_with("--files-from=")
                || (!token.starts_with("--")
                    && token.len() > 2
                    && (token.starts_with("-m") || token.starts_with("-f")))
        })
    {
        return ShellAssessment::unbounded(
            "shell.file_external_input",
            "file 使用了会额外打开参数文件的选项",
        );
    }
    if matches!(command.as_str(), "grep" | "egrep" | "fgrep" | "rg")
        && tokens.iter().any(|token| {
            token.starts_with("--file=")
                || (!token.starts_with("--") && token.len() > 2 && token.starts_with("-f"))
        })
    {
        return ShellAssessment::unbounded(
            "shell.pattern_file_argument",
            format!("{command} 把额外文件路径藏在选项值中，保守策略要求逐次批准"),
        );
    }
    if command == "rg"
        && tokens.iter().any(|token| {
            token == "--pre"
                || token.starts_with("--pre=")
                || token == "--hostname-bin"
                || token.starts_with("--hostname-bin=")
        })
    {
        return ShellAssessment::unbounded(
            "shell.ripgrep_external_command",
            "ripgrep 参数会启动外部预处理或辅助命令",
        );
    }
    if is_bash_read_only_command(&command) {
        if !is_bash_builtin_read_command(&command) && shell_search_path_is_suspicious() {
            return ShellAssessment::unbounded(
                "shell.suspicious_search_path",
                "进程 PATH 含相对或空目录，白名单命令名可能解析到工作区中的替代程序",
            );
        }
        if bash_read_command_follows_paths(&command)
            && tokens
                .iter()
                .skip(1)
                .any(|token| contains_shell_meta(token) || token.starts_with('~'))
        {
            return ShellAssessment::unbounded(
                "shell.dynamic_read_path",
                format!("{command} 的 glob、brace 或 home 展开可能越过可信目录"),
            );
        }
        if contains_explicit_outside_path(tokens, workspace, roots) {
            return ShellAssessment::unbounded(
                "shell.read_outside_trusted_roots",
                "只读命令显式引用了工作区和应用数据目录之外的路径",
            );
        }
        return ShellAssessment::read("shell.read_only", format!("{command} 属于内置只读命令集合"));
    }
    if is_bash_workspace_mutation(&command)
        && bash_mutation_is_bounded(&command, tokens, workspace, roots)
    {
        return ShellAssessment::workspace_write(
            "shell.workspace_file_edit",
            format!("{command} 只修改工作区或应用数据目录内的显式路径"),
        );
    }
    ShellAssessment::unbounded(
        "shell.unclassified_command",
        format!("无法证明 {command} 只读或仅修改可信目录"),
    )
}

fn analyze_powershell_tokens(
    tokens: &[String],
    workspace: &Path,
    roots: &[PathBuf],
) -> ShellAssessment {
    if tokens[0].contains('/') || tokens[0].contains('\\') {
        return ShellAssessment::unbounded(
            "powershell.explicit_executable_path",
            "命令通过路径或模块限定名选择实现，不能仅凭 cmdlet 名称白名单证明其安全",
        );
    }
    let command = canonical_powershell_command(&tokens[0]);
    if is_powershell_read_only_command(&command) {
        if tokens
            .iter()
            .any(|token| token.contains('{') || token.contains('}'))
        {
            return ShellAssessment::unbounded(
                "powershell.script_block",
                "PowerShell 子命令包含脚本块，可能执行任意副作用",
            );
        }
        if tokens
            .iter()
            .skip(1)
            .any(|token| contains_shell_meta(token) || token.starts_with('~'))
        {
            return ShellAssessment::unbounded(
                "powershell.dynamic_read_path",
                "PowerShell 只读命令包含通配符或 home 展开，可能跟随链接或 Provider 越过可信目录",
            );
        }
        if contains_explicit_outside_path(tokens, workspace, roots) {
            return ShellAssessment::unbounded(
                "powershell.read_outside_trusted_roots",
                "只读 cmdlet 显式引用了工作区和应用数据目录之外的路径",
            );
        }
        return ShellAssessment::read(
            "powershell.read_only",
            format!("{command} 属于内置只读 cmdlet 集合"),
        );
    }
    let workspace_write_roots = [workspace.to_path_buf()];
    if is_powershell_workspace_mutation(&command)
        && powershell_mutation_is_bounded(&command, tokens, workspace, &workspace_write_roots)
    {
        return ShellAssessment::workspace_write(
            "powershell.workspace_file_edit",
            format!("{command} 只修改工作区或应用数据目录内的显式路径"),
        );
    }
    ShellAssessment::unbounded(
        "powershell.unclassified_command",
        format!("无法证明 {command} 只读或仅修改可信目录"),
    )
}

fn analyze_git(tokens: &[String], workspace: &Path, roots: &[PathBuf]) -> ShellAssessment {
    if tokens.iter().skip(1).any(|token| {
        contains_shell_meta(token)
            || matches!(
                token.as_str(),
                "-C" | "-c"
                    | "--git-dir"
                    | "--work-tree"
                    | "--exec-path"
                    | "--config-env"
                    | "--ext-diff"
                    | "--textconv"
                    | "--output"
                    | "--global"
                    | "--system"
                    | "--file"
                    | "--blob"
                    | "-p"
                    | "--paginate"
            )
            || token.starts_with("--git-dir=")
            || token.starts_with("--work-tree=")
            || token.starts_with("--exec-path=")
            || token.starts_with("--config-env=")
            || token.starts_with("--output=")
            || token == "--open-files-in-pager"
            || token.starts_with("--open-files-in-pager=")
    }) {
        return ShellAssessment::unbounded(
            "git.unverifiable_options",
            "Git 命令包含可改换仓库、执行外部程序、写入输出或在展开后改变参数语义的选项",
        );
    }
    let Some((subcommand_index, subcommand)) = tokens
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, token)| !token.starts_with('-'))
        .map(|(index, token)| (index, token.to_ascii_lowercase()))
    else {
        return ShellAssessment::unbounded("git.missing_subcommand", "git 缺少可分析的子命令");
    };
    let subargs = &tokens[subcommand_index + 1..];
    let read_only = match subcommand.as_str() {
        "status" | "log" | "show" | "diff" | "grep" | "rev-parse" | "ls-files" | "ls-tree"
        | "describe" | "shortlog" | "blame" => true,
        "branch" => {
            subargs.is_empty()
                || (subargs.iter().any(|token| {
                    matches!(
                        token.as_str(),
                        "-l" | "--list"
                            | "--show-current"
                            | "--contains"
                            | "--no-contains"
                            | "--merged"
                            | "--no-merged"
                    )
                }) && !subargs.iter().any(|token| {
                    matches!(
                        token.as_str(),
                        "-d" | "-D"
                            | "-m"
                            | "-M"
                            | "-c"
                            | "-C"
                            | "--delete"
                            | "--move"
                            | "--copy"
                            | "--edit-description"
                            | "--set-upstream-to"
                            | "--unset-upstream"
                    )
                }))
        }
        "tag" => {
            subargs.is_empty()
                || (subargs
                    .iter()
                    .any(|token| matches!(token.as_str(), "-l" | "--list"))
                    && !subargs.iter().any(|token| {
                        matches!(
                            token.as_str(),
                            "-d" | "--delete"
                                | "-a"
                                | "--annotate"
                                | "-s"
                                | "--sign"
                                | "-u"
                                | "--local-user"
                                | "-m"
                                | "--message"
                                | "-F"
                                | "--file"
                                | "-f"
                                | "--force"
                        )
                    }))
        }
        "remote" => {
            subargs.is_empty()
                || matches!(subargs, [only] if matches!(only.as_str(), "-v" | "--verbose"))
                || (subargs.first().is_some_and(|token| token == "get-url")
                    && !subargs.iter().any(|token| {
                        matches!(
                            token.as_str(),
                            "set-url"
                                | "add"
                                | "remove"
                                | "rename"
                                | "update"
                                | "prune"
                                | "--add"
                                | "--delete"
                        )
                    }))
        }
        "config" => {
            subargs.iter().any(|token| {
                token == "--get"
                    || token == "--get-all"
                    || token == "--get-regexp"
                    || token == "--list"
                    || token == "-l"
            }) && !subargs.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "--unset"
                        | "--unset-all"
                        | "--rename-section"
                        | "--remove-section"
                        | "--add"
                        | "--replace-all"
                        | "--edit"
                        | "-e"
                )
            })
        }
        "stash" => subargs
            .first()
            .is_some_and(|token| matches!(token.as_str(), "list" | "show")),
        "worktree" => subargs.first().is_some_and(|token| token == "list"),
        "reflog" => {
            subargs.is_empty()
                || subargs
                    .first()
                    .is_some_and(|token| matches!(token.as_str(), "show" | "exists" | "list"))
        }
        _ => false,
    };
    if !read_only {
        return ShellAssessment::unbounded(
            "git.state_change",
            format!("git {subcommand} 可能修改工作树、本地历史或远端状态"),
        );
    }
    if contains_explicit_outside_path(tokens, workspace, roots) {
        return ShellAssessment::unbounded(
            "git.outside_trusted_roots",
            "Git 只读命令显式引用了可信目录之外的路径或仓库",
        );
    }
    ShellAssessment::read(
        "git.read_only",
        format!("git {subcommand} 属于只读 Git 操作"),
    )
}

fn analyze_cd(target: Option<&str>, workspace: &Path, roots: &[PathBuf]) -> ShellAssessment {
    let Some(target) = target else {
        return ShellAssessment::unbounded(
            "shell.cd_home",
            "未指定目标的 cd 会进入用户主目录，超出可信工作区",
        );
    };
    let scope = ExecutionScope::restricted(roots.to_vec());
    match resolve_existing_with_scope(workspace, target, &scope) {
        Ok(path) if path.is_dir() => {
            ShellAssessment::read("shell.cd_trusted", "cd 目标位于可信目录内")
        }
        _ => ShellAssessment::unbounded(
            "shell.cd_outside_trusted_roots",
            "cd 目标无法证明位于工作区或应用数据目录内",
        ),
    }
}

fn is_bash_read_only_command(command: &str) -> bool {
    matches!(
        command,
        "ls" | "cat"
            | "echo"
            | "printf"
            | "pwd"
            | "head"
            | "tail"
            | "grep"
            | "egrep"
            | "fgrep"
            | "rg"
            | "find"
            | "wc"
            | "which"
            | "whereis"
            | "diff"
            | "cmp"
            | "stat"
            | "du"
            | "df"
            | "file"
            | "basename"
            | "dirname"
            | "realpath"
            | "readlink"
            | "uname"
            | "id"
            | "whoami"
            | "printenv"
            | "true"
            | "false"
    )
}

fn bash_read_command_follows_paths(command: &str) -> bool {
    !matches!(
        command,
        "echo"
            | "printf"
            | "pwd"
            | "which"
            | "whereis"
            | "uname"
            | "id"
            | "whoami"
            | "printenv"
            | "true"
            | "false"
    )
}

fn is_bash_builtin_read_command(command: &str) -> bool {
    matches!(command, "echo" | "printf" | "pwd" | "true" | "false")
}

fn shell_search_path_is_suspicious() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return true;
    };
    std::env::split_paths(&path).any(|entry| entry.as_os_str().is_empty() || !entry.is_absolute())
}

fn is_bash_workspace_mutation(command: &str) -> bool {
    matches!(
        command,
        "mkdir" | "touch" | "rm" | "rmdir" | "mv" | "cp" | "sed"
    )
}

fn bash_mutation_is_bounded(
    command: &str,
    tokens: &[String],
    workspace: &Path,
    roots: &[PathBuf],
) -> bool {
    if tokens.iter().any(|token| {
        contains_shell_meta(token)
            || token == "-"
            || token.starts_with('@')
            || is_dynamic_path_token(token)
    }) {
        return false;
    }
    if command == "sed" {
        let mut has_in_place = false;
        for token in tokens.iter().skip(1) {
            if token == "-i" || token.starts_with("-i") || token == "--in-place" {
                has_in_place = true;
            }
            if token == "e" || token.starts_with("--expression=e") {
                return false;
            }
        }
        if !has_in_place {
            return false;
        }
    }
    let operands = bash_path_operands(command, tokens);
    !operands.is_empty()
        && operands
            .iter()
            .all(|path| path_is_within_trusted_roots(path, workspace, roots, true))
}

fn bash_path_operands<'a>(command: &str, tokens: &'a [String]) -> Vec<&'a str> {
    let mut operands = Vec::new();
    let mut skip_next = false;
    for token in tokens.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(
            token.as_str(),
            "-m" | "--mode" | "-t" | "--target-directory" | "-S" | "--suffix"
        ) {
            skip_next = true;
            continue;
        }
        if token.starts_with('-') {
            continue;
        }
        if command == "sed" && operands.is_empty() {
            // The first non-option is the sed program, not a path.
            operands.push("");
            continue;
        }
        operands.push(token);
    }
    if command == "sed" && operands.first() == Some(&"") {
        operands.remove(0);
    }
    operands
}

fn canonical_powershell_command(raw: &str) -> String {
    match command_basename(raw).to_ascii_lowercase().as_str() {
        "gci" | "ls" | "dir" => "Get-ChildItem".into(),
        "gc" | "cat" | "type" => "Get-Content".into(),
        "gl" | "pwd" => "Get-Location".into(),
        "sl" | "cd" | "chdir" => "Set-Location".into(),
        "gi" => "Get-Item".into(),
        "gp" => "Get-ItemProperty".into(),
        "sls" => "Select-String".into(),
        "measure" => "Measure-Object".into(),
        "compare" | "diff" => "Compare-Object".into(),
        "echo" | "write" => "Write-Output".into(),
        "sc" => "Set-Content".into(),
        "ac" => "Add-Content".into(),
        "clc" => "Clear-Content".into(),
        "ri" | "rm" | "del" | "erase" | "rd" | "rmdir" => "Remove-Item".into(),
        "ni" | "md" | "mkdir" => "New-Item".into(),
        "cpi" | "cp" | "copy" => "Copy-Item".into(),
        "mi" | "mv" | "move" => "Move-Item".into(),
        "rni" | "ren" => "Rename-Item".into(),
        other => other.to_owned(),
    }
}

fn is_powershell_read_only_command(command: &str) -> bool {
    matches!(
        command.to_ascii_lowercase().as_str(),
        "get-childitem"
            | "get-content"
            | "get-location"
            | "get-item"
            | "get-itemproperty"
            | "get-command"
            | "get-process"
            | "get-service"
            | "get-date"
            | "get-filehash"
            | "get-member"
            | "get-variable"
            | "select-string"
            | "select-object"
            | "measure-object"
            | "compare-object"
            | "test-path"
            | "resolve-path"
            | "split-path"
            | "join-path"
            | "convertto-json"
            | "convertfrom-json"
            | "format-list"
            | "format-table"
            | "format-wide"
            | "out-string"
            | "write-output"
    )
}

fn is_powershell_workspace_mutation(command: &str) -> bool {
    matches!(
        command.to_ascii_lowercase().as_str(),
        "set-content" | "add-content" | "clear-content" | "remove-item"
    )
}

fn powershell_mutation_is_bounded(
    command: &str,
    tokens: &[String],
    workspace: &Path,
    roots: &[PathBuf],
) -> bool {
    if tokens.iter().any(|token| {
        contains_shell_meta(token)
            || is_dynamic_path_token(token)
            || token.eq_ignore_ascii_case("-Credential")
            || token.eq_ignore_ascii_case("-ComputerName")
    }) {
        return false;
    }
    if command.eq_ignore_ascii_case("Remove-Item")
        && tokens
            .iter()
            .any(|token| token.eq_ignore_ascii_case("-Recurse") || token == "-r")
    {
        return false;
    }
    let paths = powershell_path_operands(command, tokens);
    if paths.is_empty()
        || !paths
            .iter()
            .all(|path| path_is_within_trusted_roots(path, workspace, roots, true))
    {
        return false;
    }
    if command.eq_ignore_ascii_case("Remove-Item") {
        let scope = ExecutionScope::restricted(roots.to_vec());
        return paths.iter().all(|path| {
            resolve_for_write_with_scope(workspace, path, &scope)
                .ok()
                .is_some_and(|resolved| roots.iter().all(|root| !same_path(&resolved, root)))
        });
    }
    true
}

fn powershell_path_operands<'a>(command: &str, tokens: &'a [String]) -> Vec<&'a str> {
    let command = command.to_ascii_lowercase();
    let mut paths = Vec::new();
    let mut index = 1;
    let mut positional_taken = false;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.eq_ignore_ascii_case("-Path")
            || token.eq_ignore_ascii_case("-LiteralPath")
            || token.eq_ignore_ascii_case("-Destination")
        {
            if let Some(value) = tokens.get(index + 1) {
                paths.push(value.as_str());
                index += 2;
                continue;
            }
            return Vec::new();
        }
        if token.eq_ignore_ascii_case("-Value")
            || token.eq_ignore_ascii_case("-Filter")
            || token.eq_ignore_ascii_case("-Include")
            || token.eq_ignore_ascii_case("-Exclude")
            || token.eq_ignore_ascii_case("-Name")
            || token.eq_ignore_ascii_case("-ItemType")
        {
            index += 2;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        if !positional_taken {
            paths.push(token);
            positional_taken = true;
        } else if matches!(command.as_str(), "copy-item" | "move-item" | "rename-item") {
            paths.push(token);
        }
        index += 1;
    }
    paths
}

fn path_is_within_trusted_roots(
    raw: &str,
    workspace: &Path,
    roots: &[PathBuf],
    for_write: bool,
) -> bool {
    if raw.trim().is_empty()
        || is_dynamic_path_token(raw)
        || contains_shell_meta(raw)
        || has_network_path(raw)
    {
        return false;
    }
    let scope = ExecutionScope::restricted(roots.to_vec());
    if for_write {
        resolve_for_write_with_scope(workspace, raw, &scope).is_ok()
    } else {
        resolve_existing_with_scope(workspace, raw, &scope).is_ok()
    }
}

fn contains_explicit_outside_path(tokens: &[String], workspace: &Path, roots: &[PathBuf]) -> bool {
    tokens.iter().skip(1).any(|token| {
        if is_dynamic_path_token(token) || has_network_path(token) {
            return true;
        }
        let embedded_path = token.split_once('=').map(|(_, value)| value).or_else(|| {
            token
                .strip_prefix('-')
                .and_then(|value| value.split_once(':').map(|(_, path)| path))
        });
        let candidate_token = embedded_path.unwrap_or(token);
        let path = Path::new(candidate_token);
        let explicitly_path_like = path.is_absolute()
            || candidate_token == ".."
            || candidate_token.starts_with("../")
            || candidate_token.starts_with("..\\")
            || candidate_token.starts_with("~/")
            || candidate_token.starts_with("~\\");
        if explicitly_path_like {
            return !path_is_within_trusted_roots(candidate_token, workspace, roots, false);
        }
        // Existing relative operands are cheap to verify. Non-path words such
        // as grep patterns are ignored here.
        let candidate = workspace.join(token);
        candidate.exists() && !path_is_within_trusted_roots(token, workspace, roots, false)
    })
}

fn has_dynamic_shell_expansion(kind: ShellKind, command: &str) -> bool {
    match kind {
        ShellKind::Bash => {
            command.contains('$')
                || command.contains("$(")
                || command.contains("`")
                || command.contains("<(")
                || command.contains(">(")
                || command.contains("${!")
        }
        ShellKind::PowerShell => {
            command.contains('$')
                || command.contains('`')
                || command.contains('@')
                || command.contains("--%")
                || command.contains('(')
                || command.contains(')')
                || command.contains(',')
                || command.contains("Invoke-Expression")
                || command.contains("invoke-expression")
                || command.contains("iex ")
                || command.trim_start().starts_with("& ")
        }
    }
}

fn has_network_path(value: &str) -> bool {
    value.contains(r"\\")
}

fn has_powershell_non_filesystem_provider(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    if [
        "registry::",
        "certificate::",
        "hklm:",
        "hkcu:",
        "hkcr:",
        "hku:",
        "hkcc:",
        "cert:",
        "env:",
        "alias:",
        "function:",
        "variable:",
        "wsman:",
    ]
    .iter()
    .any(|provider| lowered.contains(provider))
    {
        return true;
    }
    lowered.split_whitespace().any(|token| {
        let trimmed =
            token.trim_matches(|character| matches!(character, '"' | '\'' | '(' | ')' | ',' | ';'));
        if url::Url::parse(trimmed).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
        }) {
            return false;
        }
        trimmed
            .find(":\\")
            .or_else(|| trimmed.find(":/"))
            .is_some_and(|index| index != 1)
    })
}

fn has_background_or_call_operator(kind: ShellKind, command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
            continue;
        }
        if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if (kind == ShellKind::Bash && character == '\\')
                || (kind == ShellKind::PowerShell && character == '`')
            {
                escaped = true;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '\\' if kind == ShellKind::Bash => escaped = true,
            '`' if kind == ShellKind::PowerShell => escaped = true,
            '&' => {
                let previous = index.checked_sub(1).and_then(|item| chars.get(item));
                let next = chars.get(index + 1);
                if previous != Some(&'&') && next != Some(&'&') {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn is_dynamic_path_token(token: &str) -> bool {
    token.starts_with('$')
        || token.starts_with('%')
        || token.contains("$env:")
        || token.contains("${")
        || token.contains("$(")
        || token.contains('`')
}

fn contains_shell_meta(token: &str) -> bool {
    token.contains('*')
        || token.contains('?')
        || token.contains('[')
        || token.contains(']')
        || token.contains('{')
        || token.contains('}')
}

fn command_basename(command: &str) -> &str {
    command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_matches('"')
        .trim_matches('\'')
}

fn strip_safe_environment_assignments(tokens: &mut Vec<String>) {
    while tokens.first().is_some_and(|token| {
        let Some((name, value)) = token.split_once('=') else {
            return false;
        };
        let upper = name.to_ascii_uppercase();
        matches!(
            upper.as_str(),
            "LANG"
                | "LANGUAGE"
                | "LC_ALL"
                | "LC_CTYPE"
                | "LC_MESSAGES"
                | "TZ"
                | "NO_COLOR"
                | "FORCE_COLOR"
                | "TERM"
                | "COLORTERM"
        ) && !is_dynamic_path_token(value)
    }) {
        tokens.remove(0);
    }
}

fn strip_process_wrappers(kind: ShellKind, tokens: &mut Vec<String>) -> Result<(), String> {
    if kind == ShellKind::PowerShell {
        return Ok(());
    }
    loop {
        let Some(first) = tokens
            .first()
            .map(|token| command_basename(token).to_ascii_lowercase())
        else {
            return Ok(());
        };
        match first.as_str() {
            "command" if tokens.get(1).is_some_and(|token| token == "-v") => return Ok(()),
            "command" | "builtin" | "noglob" | "nohup" => {
                tokens.remove(0);
            }
            "time" | "nice" => {
                tokens.remove(0);
                while tokens.first().is_some_and(|token| token.starts_with('-')) {
                    tokens.remove(0);
                }
            }
            "timeout" => {
                tokens.remove(0);
                while tokens.first().is_some_and(|token| token.starts_with('-')) {
                    let option = tokens.remove(0);
                    if matches!(option.as_str(), "-k" | "--kill-after" | "-s" | "--signal")
                        && !tokens.is_empty()
                    {
                        tokens.remove(0);
                    }
                }
                if tokens.is_empty() {
                    return Err("timeout is missing a duration and command".into());
                }
                tokens.remove(0);
            }
            "stdbuf" => {
                tokens.remove(0);
                while tokens.first().is_some_and(|token| token.starts_with('-')) {
                    tokens.remove(0);
                }
            }
            _ => return Ok(()),
        }
    }
}

fn split_shell_segments(kind: ShellKind, command: &str) -> Result<Vec<String>, String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let chars = command.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if escaped {
            current.push(character);
            escaped = false;
            index += 1;
            continue;
        }
        if quote == Some('\'') {
            current.push(character);
            if character == '\'' {
                quote = None;
            }
            index += 1;
            continue;
        }
        if quote == Some('"') {
            current.push(character);
            if character == '"' {
                quote = None;
            } else if (kind == ShellKind::Bash && character == '\\')
                || (kind == ShellKind::PowerShell && character == '`')
            {
                escaped = true;
            }
            index += 1;
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                current.push(character);
            }
            '\\' if kind == ShellKind::Bash => {
                escaped = true;
                current.push(character);
            }
            '`' if kind == ShellKind::PowerShell => {
                escaped = true;
                current.push(character);
            }
            '(' | '[' | '{' => {
                depth = depth.saturating_add(1);
                current.push(character);
            }
            ')' | ']' | '}' => {
                if depth == 0 {
                    return Err("Command contains unmatched closing delimiters and cannot be statically parsed".into());
                }
                depth -= 1;
                current.push(character);
            }
            '\r' => {}
            '\n' | ';' if depth == 0 => push_shell_segment(&mut segments, &mut current),
            '&' | '|' if depth == 0 => {
                push_shell_segment(&mut segments, &mut current);
                if chars.get(index + 1) == Some(&character)
                    || (character == '|' && chars.get(index + 1) == Some(&'&'))
                {
                    index += 1;
                }
            }
            _ => current.push(character),
        }
        index += 1;
    }
    if quote.is_some() || escaped || depth != 0 {
        return Err("Command contains an unclosed quote, escape, or parenthesis and cannot be statically parsed".into());
    }
    push_shell_segment(&mut segments, &mut current);
    Ok(segments)
}

fn push_shell_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_owned());
    }
    current.clear();
}

fn tokenize_shell_segment(kind: ShellKind, segment: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut chars = segment.chars().peekable();
    while let Some(character) = chars.next() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if (kind == ShellKind::Bash && character == '\\')
                || (kind == ShellKind::PowerShell && character == '`')
            {
                escaped = true;
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '\\' if kind == ShellKind::Bash => escaped = true,
            '`' if kind == ShellKind::PowerShell => escaped = true,
            '#' if current.is_empty() => break,
            character if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if quote.is_some() || escaped {
        return Err("Subcommand contains an unclosed quote or escape".into());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn contains_unsafe_redirection(kind: ShellKind, segment: &str) -> bool {
    let chars = segment.chars().collect::<Vec<_>>();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
            index += 1;
            continue;
        }
        if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if (kind == ShellKind::Bash && character == '\\')
                || (kind == ShellKind::PowerShell && character == '`')
            {
                escaped = true;
            }
            index += 1;
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '\\' if kind == ShellKind::Bash => escaped = true,
            '`' if kind == ShellKind::PowerShell => escaped = true,
            '<' => return true,
            '>' => {
                index += 1;
                if chars.get(index) == Some(&'>') {
                    index += 1;
                }
                while chars.get(index).is_some_and(|item| item.is_whitespace()) {
                    index += 1;
                }
                let target = if chars
                    .get(index)
                    .is_some_and(|item| matches!(*item, '\'' | '"'))
                {
                    let target_quote = chars[index];
                    index += 1;
                    let start = index;
                    while chars.get(index).is_some_and(|item| *item != target_quote) {
                        index += 1;
                    }
                    if chars.get(index) != Some(&target_quote) {
                        return true;
                    }
                    chars[start..index].iter().collect::<String>()
                } else {
                    let start = index;
                    while chars.get(index).is_some_and(|item| {
                        !item.is_whitespace() && !matches!(item, ';' | '|' | '&' | '<' | '>')
                    }) {
                        index += 1;
                    }
                    chars[start..index].iter().collect::<String>()
                };
                let safe = match kind {
                    ShellKind::Bash => target == "/dev/null",
                    ShellKind::PowerShell => target.eq_ignore_ascii_case("$null"),
                };
                if !safe {
                    return true;
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn looks_like_recursive_delete(kind: ShellKind, command: &str) -> bool {
    let normalized = command
        .to_ascii_lowercase()
        .replace('"', "")
        .replace('\'', "")
        .replace('`', "");
    let words = normalized
        .split(|character: char| {
            character.is_whitespace()
                || matches!(character, ';' | '|' | '&' | '(' | ')' | '{' | '}')
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let delete_command = match kind {
        ShellKind::Bash => words.iter().any(|word| matches!(*word, "rm" | "rmdir")),
        ShellKind::PowerShell => words.iter().any(|word| {
            matches!(
                *word,
                "remove-item" | "rm" | "ri" | "del" | "erase" | "rd" | "rmdir"
            )
        }),
    };
    let recursive_flag = words.iter().any(|word| match kind {
        ShellKind::Bash => {
            *word == "--recursive"
                || (word.starts_with('-') && !word.starts_with("--") && word.contains('r'))
        }
        ShellKind::PowerShell => {
            word.starts_with("-recurse") || *word == "-r" || *word == "-rec" || *word == "-re"
        }
    });
    delete_command && recursive_flag
}

fn looks_like_critical_delete(kind: ShellKind, command: &str) -> bool {
    if !looks_like_recursive_delete(kind, command) {
        return false;
    }
    let lowered = command
        .to_ascii_lowercase()
        .replace('"', "")
        .replace('\'', "");
    match kind {
        ShellKind::Bash => [
            " /", " ~", "$home", "${home}", "/home", "/users", "/etc", "/bin", "/usr", "/var",
        ]
        .iter()
        .any(|target| lowered.contains(target)),
        ShellKind::PowerShell => {
            lowered.split_whitespace().any(|word| {
                let target = word.trim_matches(|character| {
                    matches!(character, '"' | '\'' | ',' | ';' | '(' | ')')
                });
                let bytes = target.as_bytes();
                bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/')
                    && (bytes.len() == 3 || (bytes.len() == 4 && matches!(bytes[3], b'*' | b'?')))
            }) || lowered.contains(r" c:\")
                || lowered.contains(r" c:/")
                || lowered.contains("$home")
                || lowered.contains("$env:userprofile")
                || lowered.contains(r"\windows")
                || lowered.contains(r"\program files")
        }
    }
}

fn classify_unbounded(level: SecurityLevel) -> SecurityDecision {
    match level {
        SecurityLevel::FullAccess => SecurityDecision {
            requires_approval: false,
            mandatory_prompt: false,
            risk_level: RiskLevel::High,
            rule_id: "tool.unbounded",
            reason: "完全访问允许执行已验证的命令工具".into(),
            scope: ExecutionScope::Unrestricted,
            effect: OperationEffect::Unbounded,
            target: None,
        },
        // Plan mode refuses filesystem writes outright, but an unbounded action
        // is not a file it could name a substitute for, so it asks like manual
        // approval and the user decides.
        SecurityLevel::Plan | SecurityLevel::RequestApproval | SecurityLevel::AllowEdits => {
            SecurityDecision {
                requires_approval: true,
                mandatory_prompt: false,
                risk_level: RiskLevel::High,
                rule_id: "tool.unbounded",
                reason: "命令执行无法可靠限制为可信目录内的只读或写入操作".into(),
                scope: ExecutionScope::Unrestricted,
                effect: OperationEffect::Unbounded,
                target: None,
            }
        }
    }
}

fn classify_browser_sensitive(
    level: SecurityLevel,
    action: crate::browser::PlaywrightAction,
) -> SecurityDecision {
    use crate::browser::PlaywrightAction as Action;
    let subject = match action {
        Action::Console => "console logs, which may contain tokens, error context, and an optional clear operation",
        Action::Network => "network logs, which may contain complete URLs, query parameters, and an optional clear operation",
        _ => "browser-sensitive data",
    };
    SecurityDecision {
        requires_approval: level != SecurityLevel::FullAccess,
        mandatory_prompt: false,
        risk_level: RiskLevel::High,
        rule_id: "browser.sensitive_observation",
        reason: format!("This tool will read {subject}"),
        scope: ExecutionScope::Unrestricted,
        effect: OperationEffect::Unbounded,
        target: None,
    }
}

const MAX_UPLOAD_PATHS: usize = 10;

/// The `file_upload` action hands local files to the current web page, an
/// irreversible outbound transfer. Only the `paths` shape is validated here;
/// selector/ref presence, path existence, file kind, and scope containment are
/// re-checked by the execution layer against the returned scope.
fn classify_browser_file_upload(
    level: SecurityLevel,
    workspace: &Path,
    app_data: &Path,
    input: &JsonObject,
) -> Result<SecurityDecision, String> {
    validate_upload_paths(input)?;
    let reason = "将把本机文件发送给当前网页".to_owned();
    let app_data =
        canonical_workspace(app_data).map_err(|error| format!("Could not access application data directory: {error}"))?;
    let denied = [app_data.join("memory")];
    if level == SecurityLevel::FullAccess {
        return Ok(SecurityDecision {
            requires_approval: false,
            mandatory_prompt: false,
            risk_level: RiskLevel::High,
            rule_id: "browser.file_upload",
            reason,
            scope: ExecutionScope::Unrestricted.denying(denied),
            effect: OperationEffect::Unbounded,
            target: None,
        });
    }
    let workspace = canonical_workspace(workspace)?;
    let mut roots = vec![workspace.clone()];
    if app_data != workspace {
        roots.push(app_data.clone());
    }
    Ok(SecurityDecision {
        requires_approval: true,
        mandatory_prompt: false,
        risk_level: RiskLevel::High,
        rule_id: "browser.file_upload",
        reason,
        scope: ExecutionScope::restricted(roots).denying([app_data.join("memory")]),
        effect: OperationEffect::Unbounded,
        target: None,
    })
}

/// Accepts `paths` as one string or an array of strings: 1 to
/// `MAX_UPLOAD_PATHS` entries, each non-empty and at most `MAX_PATH_CHARS`
/// characters. Malformed shapes are rejected even under full access.
fn validate_upload_paths(input: &JsonObject) -> Result<(), String> {
    let paths = match input.get("paths") {
        None | Some(Value::Null) => return Err("Missing upload-file paths argument".into()),
        Some(Value::String(path)) => vec![path.as_str()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| "Each paths argument entry must be a string".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("paths argument must be a string or an array of strings".into()),
    };
    if paths.is_empty() {
        return Err("paths argument requires at least one file path".into());
    }
    if paths.len() > MAX_UPLOAD_PATHS {
        return Err(format!("paths argument permits at most {MAX_UPLOAD_PATHS} file paths"));
    }
    for path in paths {
        if path.trim().is_empty() {
            return Err("paths argument must not contain an empty path".into());
        }
        if path.chars().count() > MAX_PATH_CHARS {
            return Err(format!("A paths argument path exceeds the {MAX_PATH_CHARS}-character limit"));
        }
    }
    Ok(())
}

fn classify_filesystem(
    level: SecurityLevel,
    tool_name: &str,
    effect: OperationEffect,
    target: PathBuf,
    is_trusted: bool,
    protected_app_data: bool,
    restricted: ExecutionScope,
    unrestricted: ExecutionScope,
) -> Result<SecurityDecision, String> {
    if level == SecurityLevel::FullAccess {
        return Ok(SecurityDecision {
            requires_approval: false,
            mandatory_prompt: false,
            risk_level: match effect {
                OperationEffect::Read => RiskLevel::Low,
                OperationEffect::Write => RiskLevel::Medium,
                OperationEffect::Unbounded => RiskLevel::High,
            },
            rule_id: "filesystem.full_access",
            reason: "完全访问允许执行已验证的文件操作".into(),
            scope: unrestricted,
            effect,
            target: Some(target),
        });
    }

    // Plan mode refuses rather than prompts, so the user is never asked to
    // approve a change while the plan they are reading is still a draft. This
    // is the only decision point, so scope and trust cannot route around it.
    if level == SecurityLevel::Plan
        && matches!(effect, OperationEffect::Write | OperationEffect::Unbounded)
    {
        return Err(format!(
            "plan mode is active, so `{tool_name}` may not modify {}; write the plan with the `plan` tool, or call `exit_plan_mode` to ask the user to leave plan mode",
            target.display()
        ));
    }

    let scope = if is_trusted { restricted } else { unrestricted };
    if !is_trusted {
        // AllowEdits permits reads outside the workspace; other outside access requires
        // approval below FullAccess.
        let outside_read_allowed =
            level == SecurityLevel::AllowEdits && effect == OperationEffect::Read;
        return Ok(SecurityDecision {
            requires_approval: !outside_read_allowed,
            mandatory_prompt: false,
            risk_level: if outside_read_allowed {
                RiskLevel::Medium
            } else {
                RiskLevel::High
            },
            rule_id: if outside_read_allowed {
                "filesystem.outside_read_allow_edits"
            } else {
                "filesystem.outside_trusted_roots"
            },
            reason: if outside_read_allowed {
                "目标在工作区外；允许编辑模式放行工作区外读取".into()
            } else {
                "目标位于工作区和应用数据目录之外".into()
            },
            scope,
            effect,
            target: Some(target),
        });
    }
    if protected_app_data && level == SecurityLevel::AllowEdits {
        return Ok(SecurityDecision {
            requires_approval: true,
            mandatory_prompt: false,
            risk_level: RiskLevel::High,
            rule_id: "filesystem.protected_app_data",
            reason: "目标是应用控制数据，自动写入可能修改安全策略".into(),
            scope,
            effect,
            target: Some(target),
        });
    }

    Ok(match (level, effect) {
        // Plan reads are manual-approval reads: a trusted read passes, and the
        // outside-workspace branch above already asked.
        (
            SecurityLevel::RequestApproval | SecurityLevel::Plan,
            OperationEffect::Read,
        ) => SecurityDecision {
            requires_approval: false,
            mandatory_prompt: false,
            risk_level: RiskLevel::Low,
            rule_id: "filesystem.trusted_read",
            reason: "只读目标位于工作区或应用数据目录内".into(),
            scope,
            effect,
            target: Some(target),
        },
        (SecurityLevel::RequestApproval, OperationEffect::Write) => SecurityDecision {
            requires_approval: true,
            mandatory_prompt: false,
            risk_level: RiskLevel::Medium,
            rule_id: "filesystem.trusted_write_manual",
            reason: "请求批准模式要求确认所有写入操作".into(),
            scope,
            effect,
            target: Some(target),
        },
        (SecurityLevel::AllowEdits, OperationEffect::Read | OperationEffect::Write) => {
            SecurityDecision {
                requires_approval: false,
                mandatory_prompt: false,
                risk_level: match effect {
                    OperationEffect::Read => RiskLevel::Low,
                    OperationEffect::Write => RiskLevel::Medium,
                    OperationEffect::Unbounded => RiskLevel::High,
                },
                rule_id: "filesystem.trusted_allow_edits",
                reason: "允许编辑模式允许可信目录内的读写操作".into(),
                scope,
                effect,
                target: Some(target),
            }
        }
        _ => unreachable!(
            "full access, unbounded effects and plan-mode writes are handled earlier"
        ),
    })
}

fn path_argument(input: &JsonObject, default: Option<&str>) -> Result<String, String> {
    match input.get("path") {
        None | Some(Value::Null) => default
            .map(str::to_owned)
            .ok_or_else(|| "Missing path argument".to_owned()),
        Some(Value::String(path)) => {
            if path.trim().is_empty() {
                return Err("path argument must not be empty".into());
            }
            if path.chars().count() > MAX_PATH_CHARS {
                return Err(format!("path argument exceeds the {MAX_PATH_CHARS}-character limit"));
            }
            if path.contains('\0') {
                return Err("path argument contains an invalid character".into());
            }
            Ok(path.clone())
        }
        Some(_) => Err("path argument must be a string".into()),
    }
}

fn validate_required_string(
    input: &JsonObject,
    key: &str,
    max_chars: usize,
    label: &str,
) -> Result<(), String> {
    required_string(input, key, max_chars, label).map(|_| ())
}

fn required_string<'a>(
    input: &'a JsonObject,
    key: &str,
    max_chars: usize,
    label: &str,
) -> Result<&'a str, String> {
    let value = input
        .get(key)
        .ok_or_else(|| format!("Missing {label} {key}"))?
        .as_str()
        .ok_or_else(|| format!("{label} {key} must be a string"))?;
    if value.trim().is_empty() {
        return Err(format!("{label} {key} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{label} {key} exceeds the {max_chars}-character limit"));
    }
    Ok(value)
}

fn is_protected_app_data_target(target: &Path, app_data: &Path) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    // New write targets are returned lexically while their existing ancestor
    // is canonicalized for containment. Canonicalize the direct parent too so
    // Windows verbatim-path prefixes and symlinked app-data roots compare
    // consistently.
    let canonical_parent = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    if !same_path(&canonical_parent, app_data) {
        return false;
    }
    let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let normalized = normalize_name(name);
    normalized == "document.v1.json"
        || normalized.starts_with(".document.v1.json.tmp-")
        || (normalized.starts_with("document.v1.corrupt-") && normalized.ends_with(".json"))
}

#[cfg(windows)]
fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn same_path(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[cfg(not(windows))]
fn normalize_name(name: &str) -> String {
    name.to_owned()
}

// ---------------------------------------------------------------------------
// Credential scanning
//
// These functions scan arbitrary text for credentials during project instruction loading
// and instruction-file write validation, so they belong to the security boundary.
// ---------------------------------------------------------------------------

/// High-confidence credential detector shared by model memory and project
/// instruction loading. It intentionally ignores ordinary hashes/UUIDs while
/// rejecting known token formats, credential-labelled assignments, cookies,
/// and long mixed-alphabet high-entropy tokens.
pub(crate) fn contains_sensitive_secret(content: &str) -> bool {
    static DIRECT_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    static ASSIGNMENT_PATTERN: OnceLock<Regex> = OnceLock::new();
    let direct_patterns = DIRECT_PATTERNS.get_or_init(|| {
        [
            r"(?i)-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
            r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}\b",
            r"\bxox[baprs]-[A-Za-z0-9-]{20,}\b",
            r"\bsk-(?:proj-|ant-api03-|live-)?[A-Za-z0-9_-]{16,}\b",
            r"\b(?:pk|rk)_live_[A-Za-z0-9]{16,}\b",
            r"\bnpm_[A-Za-z0-9]{20,}\b",
            r"\bsq0atp-[A-Za-z0-9_-]{20,}\b",
            r"\bAIza[0-9A-Za-z_-]{30,}\b",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{16,}",
            r"(?i)(?:https?|mongodb(?:\+srv)?|postgres(?:ql)?|mysql)://[^\s/:@]+:[^\s/@]{4,}@[^\s/]+",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("static secret pattern must compile"))
        .collect()
    });
    if direct_patterns
        .iter()
        .any(|pattern| pattern.is_match(content))
    {
        return true;
    }
    let assignment_pattern = ASSIGNMENT_PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:api[_ -]?key|secret(?:[_ -]?key)?|client[_ -]?secret|password|passwd|access[_ -]?token|refresh[_ -]?token|auth(?:orization)?[_ -]?token|id[_ -]?token|session(?:[_ -]?id|[_ -]?token)?|cookie|set-cookie|private[_ -]?key|credential)\b\s*[:=]\s*["']?([^\s"'`]{8,})"#,
        )
        .expect("static assignment secret pattern must compile")
    });
    assignment_pattern.captures_iter(content).any(|captures| {
        captures
            .get(1)
            .is_some_and(|capture| !looks_like_secret_placeholder(capture.as_str()))
    }) || contains_high_entropy_secret(content)
}

fn contains_high_entropy_secret(content: &str) -> bool {
    content
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+' | '/' | '='))
        })
        .map(|token| token.trim_matches('='))
        .filter(|token| (40..=512).contains(&token.len()))
        .any(|token| {
            if looks_like_secret_placeholder(token)
                || token.chars().all(|character| character.is_ascii_hexdigit())
            {
                return false;
            }
            let has_lower = token.bytes().any(|byte| byte.is_ascii_lowercase());
            let has_upper = token.bytes().any(|byte| byte.is_ascii_uppercase());
            let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
            let has_symbol = token
                .bytes()
                .any(|byte| matches!(byte, b'_' | b'-' | b'+' | b'/'));
            if [has_lower, has_upper, has_digit, has_symbol]
                .into_iter()
                .filter(|present| *present)
                .count()
                < 3
            {
                return false;
            }
            let mut frequencies = [0usize; 256];
            for byte in token.bytes() {
                frequencies[usize::from(byte)] += 1;
            }
            let length = token.len() as f64;
            let entropy = frequencies
                .into_iter()
                .filter(|count| *count > 0)
                .map(|count| {
                    let probability = count as f64 / length;
                    -probability * probability.log2()
                })
                .sum::<f64>();
            entropy >= 4.3
        })
}

fn looks_like_secret_placeholder(value: &str) -> bool {
    let value = value
        .trim_matches(|character: char| {
            matches!(character, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
        })
        .to_ascii_lowercase();
    value.is_empty()
        || value
            .chars()
            .all(|character| matches!(character, '*' | 'x' | '•'))
        || [
            "example",
            "placeholder",
            "redacted",
            "changeme",
            "your_",
            "your-",
            "<your",
            "${",
            "{{",
            "[redacted",
        ]
        .iter()
        .any(|placeholder| value.contains(placeholder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};
    use std::fs;

    fn request(workspace: &Path, tool_name: &str, input: Value) -> ToolExecutionRequest {
        ToolExecutionRequest {
            conversation_id: "conversation-test".into(),
            workspace_path: workspace.to_string_lossy().into_owned(),
            tool_name: tool_name.into(),
            input: input.as_object().cloned().unwrap_or_else(Map::new),
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        app_data: PathBuf,
        outside: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            let app_data = root.path().join("app-data");
            let outside = root.path().join("outside");
            fs::create_dir_all(&workspace).unwrap();
            fs::create_dir_all(&app_data).unwrap();
            fs::create_dir_all(&outside).unwrap();
            fs::write(workspace.join("inside.txt"), "inside").unwrap();
            fs::write(app_data.join("app.txt"), "app").unwrap();
            fs::write(outside.join("outside.txt"), "outside").unwrap();
            Self {
                _root: root,
                workspace,
                app_data,
                outside,
            }
        }

        fn classify(
            &self,
            level: SecurityLevel,
            tool_name: &str,
            input: Value,
        ) -> Result<SecurityDecision, String> {
            classify(
                level,
                &self.workspace,
                &self.app_data,
                &request(&self.workspace, tool_name, input),
            )
        }

        fn classify_model_call(
            &self,
            level: SecurityLevel,
            tool_name: &str,
            input: Value,
        ) -> Result<SecurityDecision, String> {
            super::classify_model_call(
                level,
                &self.workspace,
                &self.app_data,
                &request(&self.workspace, tool_name, input),
            )
        }
    }

    #[test]
    fn missing_ls_target_is_classified_without_bypassing_boundaries() {
        let fixture = Fixture::new();
        let decision = fixture.classify_model_call(SecurityLevel::FullAccess, "ls", json!({"path":"missing/nested"})).unwrap();
        assert!(!decision.requires_approval);
        assert!(decision.target.unwrap().ends_with("missing/nested"));
        let denied = fixture.app_data.join("memory/missing");
        assert!(fixture.classify_model_call(SecurityLevel::FullAccess, "ls", json!({"path":denied})).is_err());
        let outside = fixture.outside.join("missing");
        let decision = fixture.classify_model_call(SecurityLevel::RequestApproval, "ls", json!({"path":outside})).unwrap();
        assert!(decision.requires_approval);
    }

    fn is_restricted(decision: &SecurityDecision) -> bool {
        matches!(
            decision.scope,
            ExecutionScope::Restricted { .. } | ExecutionScope::RestrictedExcept { .. }
        )
    }

    fn is_unrestricted(decision: &SecurityDecision) -> bool {
        matches!(
            decision.scope,
            ExecutionScope::Unrestricted | ExecutionScope::UnrestrictedExcept { .. }
        )
    }

    #[test]
    fn request_approval_matrix_distinguishes_reads_writes_and_scope() {
        let fixture = Fixture::new();
        let workspace_read = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "read",
                json!({"path":"inside.txt"}),
            )
            .unwrap();
        let app_read = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "read",
                json!({"path":fixture.app_data.join("app.txt")}),
            )
            .unwrap();
        let outside_read = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "read",
                json!({"path":fixture.outside.join("outside.txt")}),
            )
            .unwrap();
        let inside_write = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "write",
                json!({"path":"new.txt","content":"new"}),
            )
            .unwrap();

        assert!(!workspace_read.requires_approval);
        assert!(!app_read.requires_approval);
        assert!(is_restricted(&workspace_read));
        assert!(is_restricted(&app_read));
        assert!(outside_read.requires_approval);
        assert!(is_unrestricted(&outside_read));
        assert!(inside_write.requires_approval);
        assert!(is_restricted(&inside_write));
    }

    /// Plan mode reads like manual approval and refuses every change. The refusal
    /// is an `Err`, not a prompt: while the user is still reading a draft plan
    /// there is nothing for them to approve, so the model must be told to write
    /// the plan or leave the mode instead of asking to edit a file.
    #[test]
    fn plan_matrix_reads_like_request_approval_and_refuses_every_change() {
        let fixture = Fixture::new();
        let workspace_read = fixture
            .classify(SecurityLevel::Plan, "read", json!({"path":"inside.txt"}))
            .unwrap();
        let app_read = fixture
            .classify(
                SecurityLevel::Plan,
                "read",
                json!({"path":fixture.app_data.join("app.txt")}),
            )
            .unwrap();
        let outside_read = fixture
            .classify(
                SecurityLevel::Plan,
                "read",
                json!({"path":fixture.outside.join("outside.txt")}),
            )
            .unwrap();
        assert!(!workspace_read.requires_approval);
        assert!(is_restricted(&workspace_read));
        assert!(!app_read.requires_approval);
        assert!(is_restricted(&app_read));
        assert!(outside_read.requires_approval);
        assert!(is_unrestricted(&outside_read));

        // Scope does not route around the refusal: inside, in application data
        // and outside all fail, and the message names the tool and the target so
        // the model can tell which call it was.
        for (tool, input) in [
            ("write", json!({"path":"new.txt","content":"new"})),
            (
                "write",
                json!({"path":fixture.outside.join("new.txt"),"content":"new"}),
            ),
            (
                "write",
                json!({"path":fixture.app_data.join("new.txt"),"content":"new"}),
            ),
            (
                "edit",
                json!({"path":"inside.txt","find":"inside","replace":"changed"}),
            ),
        ] {
            let error = fixture.classify(SecurityLevel::Plan, tool, input).unwrap_err();
            assert!(error.contains("plan mode is active"), "{tool}: {error}");
            assert!(error.contains(tool), "{tool}: {error}");
            assert!(error.contains("exit_plan_mode"), "{tool}: {error}");
        }

        // An unbounded action is not a file the model could name a substitute
        // for, so it asks rather than failing.
        assert!(classify_unbounded(SecurityLevel::Plan).requires_approval);
        assert_eq!(
            classify_unbounded(SecurityLevel::Plan),
            classify_unbounded(SecurityLevel::RequestApproval)
        );
        let shell = fixture
            .classify(SecurityLevel::Plan, "bash", json!({"command":"git status"}))
            .unwrap();
        assert!(shell.requires_approval);

        // A child inherits plan mode and is read-only, but spawning it is still
        // the run acting while the user decides, so it prompts.
        let spawn = fixture
            .classify_model_call(SecurityLevel::Plan, "agent_spawn", json!({}))
            .unwrap();
        assert!(spawn.requires_approval);
        assert_eq!(spawn.rule_id, "agent.delegation");

        // The plan tools themselves are host state, not disk, so they run under
        // plan mode without a prompt; writing the plan is the point of the mode.
        for (tool, input, risk) in [
            ("enter_plan_mode", json!({}), RiskLevel::Low),
            ("exit_plan_mode", json!({}), RiskLevel::Low),
            ("plan", json!({"action":"read"}), RiskLevel::Low),
            (
                "plan",
                json!({"action":"write","content":"# Plan"}),
                RiskLevel::Medium,
            ),
            // An action the host cannot read is classified as the write.
            ("plan", json!({}), RiskLevel::Medium),
        ] {
            let decision = fixture
                .classify_model_call(SecurityLevel::Plan, tool, input)
                .unwrap();
            assert!(!decision.requires_approval, "{tool}");
            assert!(!decision.mandatory_prompt, "{tool}");
            assert_eq!(decision.risk_level, risk, "{tool}");
            assert!(decision.rule_id.starts_with("host."), "{tool}");
        }

        // None of the three is reachable from the manual path, where a timeline
        // entry could otherwise be replayed to move the mode behind the run.
        for tool in ["plan", "exit_plan_mode", "enter_plan_mode"] {
            assert!(
                fixture.classify(SecurityLevel::Plan, tool, json!({})).is_err(),
                "{tool} must not be manually executable"
            );
        }
    }

    #[test]
    fn allow_edits_matrix_allows_trusted_writes_but_asks_outside() {
        let fixture = Fixture::new();
        for path in [
            fixture.workspace.join("new.txt"),
            fixture.app_data.join("new.txt"),
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::AllowEdits,
                    "write",
                    json!({"path":path,"content":"new"}),
                )
                .unwrap();
            assert!(!decision.requires_approval);
            assert!(is_restricted(&decision));
        }

        let outside = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "write",
                json!({"path":fixture.outside.join("new.txt"),"content":"new"}),
            )
            .unwrap();
        assert!(outside.requires_approval);
        assert!(is_unrestricted(&outside));
    }

    #[test]
    fn allow_edits_protects_application_control_documents() {
        let fixture = Fixture::new();
        for name in [
            "document.v1.json",
            ".document.v1.json.tmp-42-1",
            "document.v1.corrupt-20260712T120000Z.json",
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::AllowEdits,
                    "write",
                    json!({"path":fixture.app_data.join(name),"content":"tamper"}),
                )
                .unwrap();
            assert!(decision.requires_approval, "{name} must require approval");
            assert!(decision.reason.contains("应用控制数据"));
            assert!(is_restricted(&decision));
        }

        let ordinary = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "write",
                json!({"path":fixture.app_data.join("cache.txt"),"content":"ok"}),
            )
            .unwrap();
        assert!(!ordinary.requires_approval);
    }

    #[test]
    fn memory_storage_is_denied_before_any_full_access_decision() {
        let fixture = Fixture::new();
        let memory = fixture.app_data.join("memory");
        fs::create_dir_all(&memory).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            fs::write(memory.join(format!("memory.v1.sqlite3{suffix}")), "private").unwrap();
        }

        for level in [
            SecurityLevel::Plan,
            SecurityLevel::RequestApproval,
            SecurityLevel::AllowEdits,
            SecurityLevel::FullAccess,
        ] {
            for tool in ["read", "ls", "grep", "find", "edit"] {
                let input = if tool == "grep" {
                    json!({"path":memory,"pattern":"private"})
                } else if tool == "find" {
                    json!({"path":memory,"query":"*"})
                } else if tool == "edit" {
                    json!({
                        "path":memory.join("memory.v1.sqlite3"),
                        "find":"private",
                        "replace":"stolen"
                    })
                } else {
                    json!({"path":memory.join("memory.v1.sqlite3")})
                };
                assert!(
                    fixture.classify(level, tool, input).is_err(),
                    "{level:?} {tool} must not receive an executable decision"
                );
            }
            assert!(fixture
                .classify(
                    level,
                    "write",
                    json!({
                        "path":memory.join("memory.v1.sqlite3-wal"),
                        "content":"tamper"
                    })
                )
                .is_err());
        }
    }

    #[test]
    fn powershell_http_urls_are_not_provider_paths() {
        for value in ["http://example.com", "https://example.com", "HTTPS://example.com", "'https://example.com'", "\"http://example.com\"", "C:/work", r"C:\work"] {
            assert!(!has_powershell_non_filesystem_provider(value), "{value}");
        }
        for value in [r"HKLM:\Software", r"Registry::HKEY_LOCAL_MACHINE\Software", "Cert:/item", "Store:/item", r"Store:\item", "https://example.com Store:/item"] {
            assert!(has_powershell_non_filesystem_provider(value), "{value}");
        }
        let fixture = Fixture::new();
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits, SecurityLevel::FullAccess] {
            let decision = fixture.classify(level, "powershell", json!({"command":"Write-Output 'https://example.com'"})).unwrap();
            assert_eq!(decision.rule_id, "shell.read_only");
            assert_eq!(decision.effect, OperationEffect::Read);
            assert_eq!(decision.risk_level, RiskLevel::Low);
            assert_eq!(decision.requires_approval, level != SecurityLevel::FullAccess);
            let network = fixture.classify(level, "powershell", json!({"command":"Invoke-WebRequest https://example.com"})).unwrap();
            assert_eq!(network.rule_id, "powershell.unclassified_command");
        }
    }

    #[test]
    fn shell_static_analysis_distinguishes_read_only_and_unbounded_commands() {
        let fixture = Fixture::new();
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            // Shell calls require approval below FullAccess; analysis only assigns risk
            // and effect.
            let read_only = fixture
                .classify(level, "powershell", json!({"command":"Get-ChildItem"}))
                .unwrap();
            assert!(read_only.requires_approval);
            assert_eq!(read_only.effect, OperationEffect::Read);
            assert_eq!(read_only.risk_level, RiskLevel::Low);

            let unbounded = fixture
                .classify(
                    level,
                    "powershell",
                    json!({"command":"Invoke-WebRequest https://example.com"}),
                )
                .unwrap();
            assert!(unbounded.requires_approval);
            assert_eq!(unbounded.effect, OperationEffect::Unbounded);
            assert_eq!(unbounded.risk_level, RiskLevel::High);
            assert_eq!(unbounded.scope, ExecutionScope::Unrestricted);
        }
        let full = fixture
            .classify(
                SecurityLevel::FullAccess,
                "bash",
                json!({"command":"curl https://example.com"}),
            )
            .unwrap();
        assert!(!full.requires_approval);
        assert_eq!(full.risk_level, RiskLevel::High);
        assert_eq!(full.scope, ExecutionScope::Unrestricted);
    }

    #[test]
    fn shell_compound_commands_are_classified_by_the_riskiest_segment() {
        let fixture = Fixture::new();
        for (tool, command) in [
            ("bash", "git status --short && ls"),
            ("powershell", "Get-ChildItem | Select-Object -First 1"),
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    tool,
                    json!({"command":command}),
                )
                .unwrap();
            // Approval still applies to shell commands; risk is the maximum segment risk.
            assert!(decision.requires_approval, "{tool}: {command}");
            assert_eq!(decision.risk_level, RiskLevel::Low);
        }

        for (tool, command) in [
            ("bash", "git status && curl https://example.com"),
            (
                "powershell",
                "Get-ChildItem; Invoke-WebRequest https://example.com",
            ),
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    tool,
                    json!({"command":command}),
                )
                .unwrap();
            assert!(decision.requires_approval, "{tool}: {command}");
            assert_eq!(decision.risk_level, RiskLevel::High);
        }
    }

    #[test]
    fn shell_asks_below_full_access_even_for_workspace_writes() {
        // AllowEdits does not exempt shell calls. Static workspace-write classification
        // still determines the displayed risk, but approval remains required.
        let fixture = Fixture::new();
        let powershell = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "powershell",
                json!({"command":"Set-Content -Path generated.txt -Value ok"}),
            )
            .unwrap();
        assert!(powershell.requires_approval);
        assert_eq!(powershell.risk_level, RiskLevel::Medium);
        assert_eq!(powershell.rule_id, "powershell.workspace_file_edit");

        let bash = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "bash",
                json!({"command":"touch generated.txt"}),
            )
            .unwrap();
        assert!(bash.requires_approval);
        assert_eq!(bash.risk_level, RiskLevel::Medium);

        let outside = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "powershell",
                json!({
                    "command": format!(
                        "Set-Content -Path \"{}\" -Value no",
                        fixture.outside.join("outside.txt").display()
                    )
                }),
            )
            .unwrap();
        assert!(outside.requires_approval);
        assert_eq!(outside.risk_level, RiskLevel::High);
    }

    #[test]
    fn allow_edits_reads_outside_workspace_without_prompt() {
        // AllowEdits permits reads outside the workspace but still asks for writes.
        let fixture = Fixture::new();
        let outside_read = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "read",
                json!({"path":fixture.outside.join("outside.txt")}),
            )
            .unwrap();
        assert!(!outside_read.requires_approval);
        assert_eq!(outside_read.rule_id, "filesystem.outside_read_allow_edits");
        assert!(is_unrestricted(&outside_read));
    }

    #[test]
    fn agent_spawn_asks_in_request_approval_and_plan() {
        // Delegation prompts only where the user is still deciding.
        let fixture = Fixture::new();
        for level in [SecurityLevel::RequestApproval, SecurityLevel::Plan] {
            let ask = fixture
                .classify_model_call(level, "agent_spawn", json!({}))
                .unwrap();
            assert!(ask.requires_approval, "{level:?}");
            assert!(!ask.mandatory_prompt, "{level:?}");
            assert_eq!(ask.rule_id, "agent.delegation");
        }
        for level in [SecurityLevel::AllowEdits, SecurityLevel::FullAccess] {
            let auto = fixture
                .classify_model_call(level, "agent_spawn", json!({}))
                .unwrap();
            assert!(!auto.requires_approval, "{level:?}");
        }
    }

    #[test]
    fn shell_fail_closed_guards_cover_dynamic_background_redirect_and_length() {
        let fixture = Fixture::new();
        for (tool, command, rule) in [
            ("bash", "echo $(whoami)", "shell.dynamic_expansion"),
            ("bash", "git status &", "shell.background_or_call_operator"),
            (
                "bash",
                "echo ok > secret.txt 2>/dev/null",
                "shell.redirection",
            ),
            (
                "powershell",
                "Get-ChildItem; & Get-ChildItem",
                "shell.background_or_call_operator",
            ),
            (
                "powershell",
                "Get-Content -Path \\\\server\\share\\file.txt",
                "shell.network_path",
            ),
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    tool,
                    json!({"command":command}),
                )
                .unwrap();
            assert_eq!(decision.rule_id, rule, "{tool}: {command}");
            assert!(decision.requires_approval, "{tool}: {command}");
            assert_eq!(decision.risk_level, RiskLevel::High);
        }

        let too_long = "x".repeat(MAX_SHELL_ANALYSIS_CHARS + 1);
        let decision = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "bash",
                json!({"command":too_long}),
            )
            .unwrap();
        assert!(decision.requires_approval);
        assert_eq!(decision.rule_id, "shell.analysis_limit");
    }

    #[test]
    fn recursive_delete_circuit_breakers_survive_full_access() {
        let fixture = Fixture::new();
        for (tool, command) in [
            ("bash", "rm -rf /"),
            ("bash", "rm -rf \"/\""),
            ("bash", "rm -rf $HOME"),
            ("bash", "printf -v X '\\057'; rm -rf \"$X\""),
            ("powershell", "Remove-Item -Recurse C:\\"),
            ("powershell", "Remove-Item -Recurse -Force \"C:\\\""),
            ("powershell", "ri -Recurse -Force C:\\"),
            ("powershell", "Remove-Item -Re`curse C:\\"),
            ("powershell", "Remove-Item -Recurse \"$env:SystemDrive\\\""),
            ("powershell", "Remove-Item -Recurse $env:USERPROFILE"),
        ] {
            let decision = fixture
                .classify(SecurityLevel::FullAccess, tool, json!({"command":command}))
                .unwrap();
            assert!(decision.requires_approval, "{tool}: {command}");
            assert!(decision.mandatory_prompt);
            assert_eq!(decision.risk_level, RiskLevel::High);
        }
    }

    #[test]
    fn unsafe_environment_and_executable_path_cannot_borrow_read_only_trust() {
        let fixture = Fixture::new();
        for command in [
            "LD_PRELOAD=./payload.so ls",
            "./ls",
            "git diff --output=leak.txt",
            "git branch feature-created-by-mistake",
            "git stash push list",
            "git worktree add list",
            "git remote -v set-url origin https://example.com/repo.git",
            "git grep --open-files-in-pager=sh needle",
            "git reflog expire --expire=now --all",
            "find *.rs",
            "find . -fprint0 victim.txt",
            "rg --pre 'sh -c evil' needle .",
            "printf '\\057etc\\057passwd\\n' | xargs cat",
            "printf '\\057etc\\057passwd\\0' | wc --files0-from=-",
            "grep -f/etc/passwd needle inside.txt",
            "file -m/etc/passwd inside.txt",
            "file -C -mmagic inside.txt",
            "cat ~root/.ssh/id_rsa",
            "cat */secret",
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    "bash",
                    json!({"command":command}),
                )
                .unwrap();
            assert!(decision.requires_approval, "{command}");
            assert_ne!(decision.risk_level, RiskLevel::Low, "{command}");
        }

        let safe_env = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "bash",
                json!({"command":"NO_COLOR=1 git status --short"}),
            )
            .unwrap();
        // Safe environment variables do not raise risk; shell approval remains required.
        assert!(safe_env.requires_approval);
        assert_eq!(safe_env.risk_level, RiskLevel::Low);
    }

    #[test]
    fn powershell_allow_edits_cannot_delete_trees_or_application_control_data() {
        let fixture = Fixture::new();
        let commands = vec![
            "Remove-Item -Recurse .".to_owned(),
            "ri -Recurse -Force C:\\".to_owned(),
            "Remove-Item -Re`curse C:\\".to_owned(),
            "Remove-Item -Recurse \"$env:SystemDrive\\\"".to_owned(),
            "Remove-Item .".to_owned(),
            "Remove-Item HKCU:\\Software\\Mework".to_owned(),
            "Get-ItemProperty HKLM:\\Software".to_owned(),
            "Get-Content */secret".to_owned(),
            "Write-Output (Start-Process notepad)".to_owned(),
            "Write-Output (Remove-Item inside.txt)".to_owned(),
            "Set-Content \"inside.txt\", \"C:\\outside.txt\" -Value pwn".to_owned(),
            format!(
                "Set-Content -Path \"{}\" -Value tamper",
                fixture.app_data.join("document.v1.json").display()
            ),
        ];
        for command in commands {
            let decision = fixture
                .classify(
                    SecurityLevel::AllowEdits,
                    "powershell",
                    json!({"command":&command}),
                )
                .unwrap();
            assert!(decision.requires_approval, "{command}");
            assert_ne!(decision.risk_level, RiskLevel::Low, "{command}");
        }
    }

    #[test]
    fn full_access_allows_valid_outside_paths_but_not_invalid_requests() {
        let fixture = Fixture::new();
        let outside = fixture
            .classify(
                SecurityLevel::FullAccess,
                "read",
                json!({"path":fixture.outside.join("outside.txt")}),
            )
            .unwrap();
        assert!(!outside.requires_approval);
        assert!(is_unrestricted(&outside));

        assert!(fixture
            .classify(SecurityLevel::FullAccess, "read", json!({}))
            .is_err());
        assert!(fixture
            .classify(
                SecurityLevel::FullAccess,
                "write",
                json!({"path":42,"content":"invalid"})
            )
            .is_err());
        assert!(fixture
            .classify(SecurityLevel::FullAccess, "bash", json!({"command":""}))
            .is_err());
        assert!(fixture
            .classify(SecurityLevel::FullAccess, "unknown", json!({}))
            .is_err());
    }

    #[test]
    fn optional_read_roots_default_to_workspace() {
        let fixture = Fixture::new();
        for tool in ["ls", "grep", "find"] {
            let decision = fixture
                .classify(SecurityLevel::RequestApproval, tool, json!({}))
                .unwrap();
            assert!(!decision.requires_approval);
            assert!(is_restricted(&decision));
        }
    }

    #[test]
    fn browser_observation_and_sensitive_state_have_distinct_policies() {
        let fixture = Fixture::new();
        for action in ["snapshot", "wait", "resize"] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    "playwright",
                    json!({"action": action}),
                )
                .unwrap();
            assert!(
                !decision.requires_approval,
                "{action} should be local/read-only"
            );
            assert!(is_restricted(&decision));
        }
        for action in ["console", "network"] {
            let decision = fixture
                .classify(
                    SecurityLevel::RequestApproval,
                    "playwright",
                    json!({"action": action}),
                )
                .unwrap();
            assert!(
                decision.requires_approval,
                "{action} can expose sensitive logs"
            );
            assert_eq!(decision.effect, OperationEffect::Unbounded);
        }
        let upload = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "playwright",
                json!({"action":"file_upload","selector":"input[type=file]","paths":["a.txt"]}),
            )
            .unwrap();
        assert!(
            upload.requires_approval,
            "file upload sends local files to the page"
        );
        assert_eq!(upload.effect, OperationEffect::Unbounded);
        assert!(is_restricted(&upload));
    }

    /// One tool, 23 actions: the `dangerous` catalog flag can no longer express the per-action
    /// approval line, so the expectation lives here explicitly instead of being derived from it.
    #[test]
    fn every_playwright_action_has_a_matching_security_policy() {
        use crate::browser::PlaywrightAction as Action;

        let fixture = Fixture::new();
        let browser_tools = crate::catalog::tool_catalog()
            .into_iter()
            .filter(|tool| tool.name == "playwright")
            .collect::<Vec<_>>();
        assert_eq!(browser_tools.len(), 1);
        assert_eq!(browser_tools[0].name, "playwright");
        assert!(browser_tools[0].dangerous);

        for action in Action::ALL {
            let name = action.as_str();
            let mut input = match action {
                Action::Navigate => json!({"url":"https://example.com"}),
                Action::Evaluate => json!({"script":"document.title"}),
                Action::Screenshot => json!({"path":"page.png"}),
                Action::FillForm => json!({"fields":[{"ref":"e1","value":"x"}]}),
                Action::FileUpload => json!({"selector":"input","paths":["a.txt"]}),
                Action::UploadImage => json!({"selector":"input","image_id":"1"}),
                Action::TabSelect | Action::TabClose => json!({"tab":"agent-1"}),
                _ => json!({}),
            };
            input["action"] = json!(name);

            // `classify_model_call` is the run loop's entry point; every action except
            // `upload_image` falls through to the manual classifier unchanged, and that one
            // exists only inside a run, so the manual path must refuse it instead.
            if action == Action::UploadImage {
                let manual = fixture
                    .classify(SecurityLevel::FullAccess, "playwright", input.clone())
                    .unwrap_err();
                assert!(
                    manual.contains("requires the model run loop"),
                    "manual execution must be refused: {manual}"
                );
            }

            // Observations and local retargeting never prompt; everything that drives a page,
            // reads sensitive logs, or moves bytes does.
            let expects_approval = !matches!(
                action,
                Action::Snapshot | Action::Wait | Action::Resize | Action::TabList | Action::TabSelect
            );
            let guarded = fixture
                .classify_model_call(SecurityLevel::RequestApproval, "playwright", input.clone())
                .unwrap_or_else(|error| panic!("{name} has no security policy: {error}"));
            assert_eq!(
                guarded.requires_approval, expects_approval,
                "playwright {name} approval line changed"
            );

            let full = fixture
                .classify_model_call(SecurityLevel::FullAccess, "playwright", input)
                .unwrap_or_else(|error| panic!("{name} rejects full-access execution: {error}"));
            assert!(
                !full.requires_approval,
                "playwright {name} should execute without approval in full-access mode"
            );
        }
    }

    /// A `playwright` call with no action, or an action this build does not have, must be
    /// refused rather than falling into the most permissive branch.
    #[test]
    fn playwright_requires_a_known_action() {
        let fixture = Fixture::new();
        for input in [json!({}), json!({"action":"exfiltrate"}), json!({"action": 3})] {
            let error = fixture
                .classify_model_call(SecurityLevel::FullAccess, "playwright", input.clone())
                .unwrap_err();
            assert!(
                error.starts_with("playwright "),
                "{input} must be refused by the classifier: {error}"
            );
        }
    }

    #[test]
    fn global_memory_mutations_require_a_native_confirmation_in_every_mode() {
        let fixture = Fixture::new();
        for level in [
            SecurityLevel::Plan,
            SecurityLevel::RequestApproval,
            SecurityLevel::AllowEdits,
            SecurityLevel::FullAccess,
        ] {
            for tool in ["create_global_memory", "edit_global_memory"] {
                let decision = fixture
                    .classify_model_call(level, tool, json!({"name": "preferences"}))
                    .unwrap();
                assert!(decision.requires_approval, "{level:?} {tool}");
                assert!(decision.mandatory_prompt, "{level:?} {tool}");
                assert_eq!(
                    decision.rule_id, "memory.global_persistent_mutation",
                    "{level:?} {tool}"
                );
                assert_eq!(decision.risk_level, RiskLevel::High);
                assert!(decision.reason.contains("所有项目"));
            }

            // The project tier is a local state change, not a mandatory prompt.
            for tool in ["create_project_memory", "edit_project_memory"] {
                let decision = fixture
                    .classify_model_call(level, tool, json!({"name": "build"}))
                    .unwrap();
                assert!(!decision.mandatory_prompt, "{level:?} {tool}");
                assert_eq!(decision.risk_level, RiskLevel::Medium, "{level:?} {tool}");
            }

            // Reads never prompt in any mode.
            for tool in ["read_global_memory", "read_project_memory"] {
                let decision = fixture
                    .classify_model_call(level, tool, json!({"name": "build"}))
                    .unwrap();
                assert_eq!(decision.risk_level, RiskLevel::Low, "{level:?} {tool}");
                assert_eq!(decision.effect, OperationEffect::Read, "{level:?} {tool}");
            }
        }

        // The tier now comes from the tool name, so a `scope` argument is inert
        // rather than a way to talk a project write into the global tier.
        for scope in [json!("global"), json!("other"), json!(42)] {
            let decision = fixture
                .classify_model_call(
                    SecurityLevel::FullAccess,
                    "create_project_memory",
                    json!({"name": "build", "scope": scope}),
                )
                .unwrap();
            assert!(!decision.mandatory_prompt);
            assert_eq!(decision.risk_level, RiskLevel::Medium);
        }
    }

    #[test]
    fn browser_interactions_require_approval_except_under_full_access() {
        let fixture = Fixture::new();
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            let decision = fixture
                .classify(
                    level,
                    "playwright",
                    json!({"action":"navigate","url":"https://example.com"}),
                )
                .unwrap();
            assert!(decision.requires_approval);
            assert_eq!(decision.effect, OperationEffect::Unbounded);
        }
        let full = fixture
            .classify(
                SecurityLevel::FullAccess,
                "playwright",
                json!({"action":"click","selector":"button"}),
            )
            .unwrap();
        assert!(!full.requires_approval);
        assert!(fixture
            .classify(
                SecurityLevel::FullAccess,
                "playwright",
                json!({"action":"navigate"})
            )
            .is_err());
    }

    #[test]
    fn web_search_has_one_outer_permission_boundary() {
        let fixture = Fixture::new();
        let query = json!({ "query": "Anthropic Claude 4.5 release date" });
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            let decision = fixture
                .classify_model_call(level, "web_search", query.clone())
                .unwrap();
            assert!(decision.requires_approval, "{level:?}");
            assert_eq!(decision.rule_id, "web.search");
        }
        let full = fixture
            .classify_model_call(SecurityLevel::FullAccess, "web_search", query)
            .unwrap();
        assert!(!full.requires_approval);
        assert_eq!(full.rule_id, "web.search");

        // Tool name is the only routing criterion. Legacy delegation fields are unknown
        // parameters, not an alternate security tier.
        let error = fixture
            .classify_model_call(
                SecurityLevel::FullAccess,
                "web_search",
                json!({ "objective": "compare the primary sources" }),
            )
            .expect_err("objective 形状不再被 web_search 入口接受");
        assert!(error.contains("query"), "报错必须指向缺失的 query：{error}");

        // Retired tool names must remain unknown rather than bypass approval.
        fixture
            .classify_model_call(
                SecurityLevel::FullAccess,
                "web_query",
                json!({ "query": "primary source" }),
            )
            .expect_err("被删除的原生腿必须是未知工具，而不是一条免提示通道");
    }

    /// `web_fetch` and `web_search` share one boundary: one authorization covers every
    /// URL in the call, so both must receive the same high-risk classification.
    #[test]
    fn web_fetch_shares_the_same_outer_boundary_and_bounds_its_url_list() {
        let fixture = Fixture::new();
        let urls = json!({ "urls": ["https://example.com/a", "https://example.com/b"] });
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            let decision = fixture
                .classify_model_call(level, "web_fetch", urls.clone())
                .unwrap();
            assert!(decision.requires_approval, "{level:?}");
            assert_eq!(decision.rule_id, "web.fetch");
            assert_eq!(decision.risk_level, RiskLevel::High);
        }
        let full = fixture
            .classify_model_call(SecurityLevel::FullAccess, "web_fetch", urls)
            .unwrap();
        assert!(!full.requires_approval);

        // Validate shape boundaries before authorization; empty or oversized URL lists
        // must fail without showing an approval prompt.
        for refused in [
            json!({}),
            json!({ "urls": [] }),
            json!({ "urls": [5] }),
            json!({ "urls": ["  "] }),
        ] {
            fixture
                .classify_model_call(SecurityLevel::FullAccess, "web_fetch", refused.clone())
                .expect_err("{refused} 必须在分类阶段就被拒绝");
        }
        let too_many = json!({
            "urls": (0..=crate::model::MAX_SEARCH_INPUTS)
                .map(|index| format!("https://example.com/{index}"))
                .collect::<Vec<_>>()
        });
        fixture
            .classify_model_call(SecurityLevel::FullAccess, "web_fetch", too_many)
            .expect_err("超过一次调用的地址上限必须被拒绝");
    }

    #[test]
    fn workflow_has_one_outer_permission_boundary_that_full_access_clears() {
        let fixture = Fixture::new();
        let input = json!({"script": "export const meta = { name: \"audit\", description: \"d\" }\nreturn 1"});
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            let decision = fixture
                .classify_model_call(level, "workflow", input.clone())
                .unwrap();
            assert!(decision.requires_approval, "{level:?}");
            assert_eq!(decision.rule_id, "workflow.orchestrated_fan_out");
        }

        // Full access means every model operation runs unattended. Fan-out is
        // broad, but each step re-enters this classifier at the same level, so
        // the level and not the tool decides — hence no mandatory prompt.
        let full = fixture
            .classify_model_call(SecurityLevel::FullAccess, "workflow", input)
            .unwrap();
        assert!(!full.requires_approval);
        assert!(!full.mandatory_prompt);
        assert_eq!(full.rule_id, "workflow.orchestrated_fan_out");
    }

    #[test]
    fn browser_observability_logs_are_sensitive() {
        let fixture = Fixture::new();
        for action in ["console", "network"] {
            let guarded = fixture
                .classify(
                    SecurityLevel::AllowEdits,
                    "playwright",
                    json!({"action": action}),
                )
                .unwrap();
            assert!(guarded.requires_approval, "{action} must require approval");
            assert_eq!(guarded.effect, OperationEffect::Unbounded);
            assert!(!guarded.reason.is_empty());

            let full = fixture
                .classify(
                    SecurityLevel::FullAccess,
                    "playwright",
                    json!({"action": action}),
                )
                .unwrap();
            assert!(!full.requires_approval);
        }
    }

    #[test]
    fn browser_file_upload_validates_paths_and_requires_approval() {
        let fixture = Fixture::new();
        let valid = json!({"action":"file_upload","selector":"input[type=file]","paths":["artifacts/report.pdf"]});
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            let decision = fixture
                .classify(level, "playwright", valid.clone())
                .unwrap();
            assert!(decision.requires_approval);
            assert_eq!(decision.effect, OperationEffect::Unbounded);
            assert!(is_restricted(&decision));
            assert!(decision.reason.contains("本机文件"));
        }

        // A bare string is accepted in place of a one-element array.
        let single = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "playwright",
                json!({"action":"file_upload","ref":"e7","paths":"artifacts/report.pdf"}),
            )
            .unwrap();
        assert!(single.requires_approval);

        let full = fixture
            .classify(SecurityLevel::FullAccess, "playwright", valid)
            .unwrap();
        assert!(!full.requires_approval);
        assert!(is_unrestricted(&full));

        for invalid in [
            json!({"action":"file_upload","selector":"input"}),
            json!({"action":"file_upload","paths":null}),
            json!({"action":"file_upload","paths":42}),
            json!({"action":"file_upload","paths":[]}),
            json!({"action":"file_upload","paths":[42]}),
            json!({"action":"file_upload","paths":["  "]}),
            json!({"action":"file_upload","paths":["x".repeat(MAX_PATH_CHARS + 1)]}),
            json!({"action":"file_upload","paths":(0..=MAX_UPLOAD_PATHS).map(|index| format!("file-{index}.txt")).collect::<Vec<_>>()}),
        ] {
            assert!(
                fixture
                    .classify(SecurityLevel::FullAccess, "playwright", invalid.clone())
                    .is_err(),
                "malformed paths must be rejected even under full access: {invalid}"
            );
        }
    }

    #[test]
    fn browser_screenshots_follow_workspace_write_policy() {
        let fixture = Fixture::new();
        let approval = fixture
            .classify(
                SecurityLevel::RequestApproval,
                "playwright",
                json!({"action":"screenshot","path":"artifacts/page.png"}),
            )
            .unwrap();
        assert!(approval.requires_approval);
        assert!(is_restricted(&approval));

        let allowed = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "playwright",
                json!({"action":"screenshot","path":"artifacts/page.png"}),
            )
            .unwrap();
        assert!(!allowed.requires_approval);
        assert!(is_restricted(&allowed));

        let outside = fixture
            .classify(
                SecurityLevel::AllowEdits,
                "playwright",
                json!({"action":"screenshot","path":fixture.outside.join("page.png")}),
            )
            .unwrap();
        assert!(outside.requires_approval);
        assert!(is_unrestricted(&outside));
    }

    /// A screenshot is how the browser hands an observation back, so plan mode
    /// asks for it instead of refusing it the way it refuses every other write.
    /// The rest of the browser's write surface keeps plan mode's answer.
    #[test]
    fn plan_mode_asks_for_a_screenshot_rather_than_refusing_it() {
        let fixture = Fixture::new();
        for path in [
            json!("artifacts/page.png"),
            json!(fixture.outside.join("page.png")),
        ] {
            let decision = fixture
                .classify(
                    SecurityLevel::Plan,
                    "playwright",
                    json!({"action":"screenshot","path":path}),
                )
                .unwrap();
            assert_eq!(
                decision,
                fixture
                    .classify(
                        SecurityLevel::RequestApproval,
                        "playwright",
                        json!({"action":"screenshot","path":path}),
                    )
                    .unwrap(),
                "{path}"
            );
            assert!(decision.requires_approval, "{path}");
        }

        // The carve-out is the screenshot's own arm, so a file write through the
        // same tool name is still refused.
        let error = fixture
            .classify(
                SecurityLevel::Plan,
                "write",
                json!({"path":"artifacts/page.png","content":"x"}),
            )
            .unwrap_err();
        assert!(error.contains("plan mode is active"), "{error}");
    }

    #[test]
    fn task_tools_are_rejected_before_manual_execution_or_approval() {
        let fixture = Fixture::new();
        for level in [
            SecurityLevel::Plan,
            SecurityLevel::RequestApproval,
            SecurityLevel::AllowEdits,
            SecurityLevel::FullAccess,
        ] {
            for (tool, action) in [
                ("todo", "create"),
                ("todo", "update"),
                ("todo", "get"),
                ("todo", "list"),
            ] {
                let error = fixture
                    .classify(level, tool, json!({"action":action}))
                    .unwrap_err();
                assert!(error.contains("may only be scheduled by the model run loop"), "{tool} {action}: {error}");
                assert!(error.contains("cannot be manually executed or approved separately"), "{tool} {action}: {error}");
            }
        }
    }

    /// One tool, several actions: since the merge the read/write split can no
    /// longer be read off the tool name, so the model-run classifier reads
    /// `action`. Reads must stay Low and writes Medium — and an action the
    /// classifier cannot read must fail toward the stricter tier, because the
    /// alternative is a mutation slipping through as a read.
    #[test]
    fn merged_state_tools_classify_reads_and_writes_by_action() {
        let fixture = Fixture::new();
        for level in [
            SecurityLevel::Plan,
            SecurityLevel::RequestApproval,
            SecurityLevel::AllowEdits,
            SecurityLevel::FullAccess,
        ] {
            for (tool, action) in [("todo", "get"), ("todo", "list")] {
                let decision = fixture
                    .classify_model_call(level, tool, json!({"action": action}))
                    .unwrap();
                assert_eq!(decision.rule_id, "host.read_or_coordinate", "{tool} {action}");
                assert_eq!(decision.risk_level, RiskLevel::Low, "{tool} {action}");
                assert!(!decision.requires_approval, "{tool} {action}");
            }

            for (tool, input) in [
                ("todo", json!({"action":"create","subject":"a","description":"b"})),
                ("todo", json!({"action":"update","taskId":"task-1","status":"completed"})),
                // Unreadable or unknown discriminators fail toward the write tier.
                ("todo", json!({})),
                ("todo", json!({"action":"delete"})),
                ("todo", json!({"action":123})),
            ] {
                let decision = fixture
                    .classify_model_call(level, tool, input.clone())
                    .unwrap();
                assert_eq!(decision.rule_id, "host.local_state_change", "{tool} {input}");
                assert_eq!(decision.risk_level, RiskLevel::Medium, "{tool} {input}");
                assert!(!decision.requires_approval, "{tool} {input}");
            }
        }
    }
}
