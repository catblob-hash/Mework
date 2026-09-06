import { createSeedDocument, toolCatalog } from "../seed";
import { defaultConversationWebSearchSettings } from "../lib/runtime";
import type { AppDocument } from "../types";
import type { SubagentView } from "../lib/subagents";
import type { TaskContainerMessages } from "../lib/taskContainer";

const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

/** Rich data used by interaction tests; none of it is part of the product seed. */
export function createTestDocument(): AppDocument {
  const document = createSeedDocument();
  const testWorkspace = document.workspaces[0];
  testWorkspace.id = "ws_mework";
  testWorkspace.name = "Mework";
  testWorkspace.path = "C:\\test\\Mework";
  testWorkspace.createdAt = ago(900);
  testWorkspace.conversations = [
    {
      id: "conv_agent_gui",
      title: "制作 Agent GUI",
      createdAt: ago(160),
      updatedAt: ago(2),
      settings: {
        systemPrompt: "你是 Mework 的主工程代理。保持改动可验证、界面克制，并清楚说明风险。",
        includeAppDataPath: false,
        enabledTools: toolCatalog.map((tool) => tool.name),
        toolDescriptionFileId: null,
        agentDefinitions: [],
        allowRolelessSubagents: false,
        hookIds: [],
        skillIds: [],
        mcpIds: [],
        webSearch: defaultConversationWebSearchSettings(),
        reasoningEffort: "disabled",
        securityLevel: "request_approval",
        globalMemoryEnabled: false,
        projectMemoryEnabled: false,
        skillToolEnabled: false
      },
      branches: [],
      queuedMessages: [],
      userAbortedTasks: [],
      worktree: null,
    runTarget: null,
      parentConversationId: null,
      contexts: [
        {
          id: "ctx_sys",
          kind: "system",
          content: "你是 Mework 的主工程代理。保持改动可验证、界面克制，并清楚说明风险。",
          createdAt: ago(158)
        },
        {
          id: "ctx_user",
          kind: "user",
          content: "制作一个界面仿 Codex 的 agent GUI，工作区下面管理对话，并允许随时编辑上下文。",
          createdAt: ago(156)
        },
        {
          id: "ctx_reason",
          kind: "reasoning",
          content: "先确认信息结构，再将上下文的编辑与执行语义分开。工具调用被编辑后必须重新执行，旧返回值不能保留。",
          createdAt: ago(154)
        },
        {
          id: "ctx_tool",
          kind: "tool",
          toolName: "find",
          input: { path: ".", query: "*.tsx" },
          result: {
            success: true,
            output: "src/App.tsx\nsrc/main.tsx",
            executedAt: ago(150),
            durationMs: 18
          },
          createdAt: ago(150)
        },
        {
          id: "ctx_reasoning_more",
          kind: "reasoning",
          content: "接着核对工具结果与最终回复的关联。",
          createdAt: ago(148)
        },
        {
          id: "ctx_assistant",
          kind: "assistant",
          content: "信息结构已经明确。我会把上下文做成可定位、可编辑、可重放的时间线。",
          createdAt: ago(146)
        }
      ]
    },
    {
      id: "conv_persistence",
      title: "持久化与迁移设计",
      createdAt: ago(320),
      updatedAt: ago(280),
      settings: {
        systemPrompt: "你是 Rust 架构师，优先保证数据可迁移和原子写入。",
        includeAppDataPath: false,
        enabledTools: ["ls", "find", "read", "write", "edit", "powershell"],
        toolDescriptionFileId: null,
        agentDefinitions: [],
        allowRolelessSubagents: false,
        hookIds: [],
        skillIds: [],
        mcpIds: [],
        webSearch: defaultConversationWebSearchSettings(),
        reasoningEffort: "disabled",
        securityLevel: "request_approval",
        globalMemoryEnabled: false,
        projectMemoryEnabled: false,
        skillToolEnabled: false
      },
      branches: [],
      queuedMessages: [],
      userAbortedTasks: [],
      worktree: null,
    runTarget: null,
      parentConversationId: null,
      contexts: [
        {
          id: "ctx_persist_user",
          kind: "user",
          content: "设计本地 JSON 数据的安全持久化方式。",
          createdAt: ago(315)
        },
        {
          id: "ctx_persist_reply",
          kind: "assistant",
          content: "使用版本化文档、临时文件写入与原子替换；加载失败时保留损坏副本。",
          createdAt: ago(290)
        }
      ]
    }
  ];
  // Keep interaction tests deterministic in their historical default language.
  // Fresh-install language initialization is covered by dedicated App tests.
  document.globalSettings.appLanguage = "zh-CN";
  document.globalSettings.resolvedAppLanguage = "zh-CN";
  document.globalSettings.conversationPresets = [{
    id: "conversation_default",
    name: "默认",
    description: "测试对话预设。",
    settings: {
      systemPrompt: "你是一个可靠的工程代理。",
      enabledTools: toolCatalog.map((tool) => tool.name),
      toolDescriptionFileId: null,
      agentDefinitions: [],
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
  }];
  document.globalSettings.defaultConversationPresetId = "conversation_default";
  document.globalSettings.apiProviders = [
    {
      id: "openai_responses",
      name: "OpenAI Responses",
      enabled: true,
      endpointBaseUrls: {},
      familySettings: {},
      notes: "",
      family: "openai_responses",
      baseUrl: "https://api.openai.com/v1",
      activeModelId: null,
      models: []
    },
    {
      id: "openai_chat",
      name: "OpenAI Chat Completions",
      enabled: true,
      endpointBaseUrls: {},
      familySettings: {},
      notes: "",
      family: "openai_chat",
      baseUrl: "https://api.openai.com/v1",
      activeModelId: null,
      models: []
    },
    {
      id: "anthropic_messages",
      name: "Anthropic Messages",
      enabled: true,
      endpointBaseUrls: {},
      familySettings: {},
      notes: "",
      family: "anthropic",
      baseUrl: "https://api.anthropic.com/v1",
      activeModelId: null,
      models: []
    }
  ];
  document.globalSettings.activeProviderId = "openai_responses";
  document.capabilities.skills.push({
    id: "skill_code_review",
    name: "代码审查",
    description: "测试用技能。",
    location: "test://skills/code-review/SKILL.md",
    source: "user",
    available: true
  });
  document.capabilities.mcps.push({
    id: "mcp_workspace",
    name: "Workspace Files",
    description: "测试用 MCP。",
    location: "test://mcp/workspace",
    source: "user",
    available: true
  });
  // The host always lists the two compiled-in profiles first, then files.
  document.capabilities.toolDescriptionFiles.push(
    {
      id: "tooldesc_builtin_en_us",
      name: "Mework built-in (English)",
      description: "Built-in English prompts and tool descriptions",
      location: "builtin:en-US",
      source: "builtin",
      available: true
    },
    {
      id: "tooldesc_builtin_zh_cn",
      name: "Mework 内置（中文）",
      description: "内置中文提示词与工具描述",
      location: "builtin:zh-CN",
      source: "builtin",
      available: true
    },
    {
      id: "tooldesc_user_main_0f0f0f0f",
      name: "main",
      description: "2 个工具描述 · 3 条提示词覆盖",
      location: "test://tool-descriptions/main.json",
      source: "user",
      available: true
    }
  );
  const firstConversation = document.workspaces[0]?.conversations[0];
  if (firstConversation) {
    firstConversation.settings.skillIds = ["skill_code_review"];
    firstConversation.settings.mcpIds = ["mcp_workspace"];
  }
  return document;
}

/**
 * One agent view with every field defaulted.
 *
 * The task panel, the message-stream card and the projection tests all start
 * from the roster, so they share one factory: a per-file copy is how three
 * surfaces end up disagreeing about what a run with no role looks like.
 */
export function subagentViewFixture(
  id: string,
  overrides: Partial<SubagentView> = {}
): SubagentView {
  return {
    id,
    name: null,
    kind: "workflowStep",
    workflowRun: false,
    label: id,
    task: `执行 ${id}`,
    status: "completed",
    summary: `${id} 的结论`,
    contexts: [],
    updates: [],
    live: null,
    callIds: [id],
    parentId: null,
    depth: 0,
    childIds: [],
    phase: null,
    phaseIndex: null,
    stepIndex: null,
    role: null,
    // These required fields use meaningful null values: modelId inherits the conversation model, and scriptName means no named workflow script.
    // Vitest transpiles without type checking, so an omission reaches components as undefined.
    modelId: null,
    scriptName: null,
    usage: {},
    toolCount: 0,
    createdAt: "2026-08-01T00:00:00Z",
    completedAt: "2026-08-01T00:01:00Z",
    ...overrides
  };
}

/** Task-surface copy in the same language the components render under test. */
export const taskMessagesFixture: TaskContainerMessages = {
  workflowLabel: "工作流",
  runningStepCount: (running, total) => `${running}/${total} 个步骤进行中`,
  stepCount: (total) => `${total} 个步骤`,
  terminalIdle: "空闲",
  terminalBusy: "正在执行命令",
  shellRunning: "正在运行",
  shellStopping: "正在中止",
  shellExited: (code) => `已失败（退出码 ${code}）`,
  shellFinished: "已完成",
  shellFailed: "已失败",
  shellStopped: "已中止",
  browserLabel: "浏览器页面",
  browserLoading: "正在加载",
  browserSuspended: "已挂起",
  browserIdle: "已就绪",
  browserAutomation: (tool) => `Model is operating: ${tool}`,
  userAborted: "用户中止操作"
};
