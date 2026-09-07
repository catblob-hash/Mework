import { describe, expect, it } from "vitest";
// Vite's ?raw import loads Rust source as plain text for cross-language constant alignment.
// It needs neither Node types nor compilation.
import storageSource from "../src-tauri/src/storage.rs?raw";
import { CLAUDE_CODE_PRESET_ID, CODEX_PRESET_ID, createSeedDocument } from "./seed";
import { implicitConversationPreset, preferredShellToolName } from "./lib/conversationPresets";
import { toolCatalog } from "./seed";
import { defaultConversationWebSearchSettings, normalizeDocument } from "./lib/runtime";

describe("seed document", () => {
  it("keeps the Rust and TS schema version constants identical", () => {
    // Keep the Rust and TypeScript schema-version constants aligned.
    const match = storageSource.match(/pub const SCHEMA_VERSION: u32 = (\d+);/);
    expect(match).not.toBeNull();
    expect(Number(match![1])).toBe(createSeedDocument().schemaVersion);
  });

  it("starts with the approval-first security policy", () => {
    const document = createSeedDocument();
    expect(document.schemaVersion).toBe(3);
    expect(document.globalSettings).toMatchObject({
      appLanguage: "auto",
      resolvedAppLanguage: "zh-CN",
      theme: "system"
    });
    expect(document.globalSettings.defaultConversationPresetId).toBe(CLAUDE_CODE_PRESET_ID);
    expect(document.globalSettings.conversationPresets.map((preset) => preset.id))
      .toEqual([CODEX_PRESET_ID, CLAUDE_CODE_PRESET_ID]);
    // Seed conversations always use the most cautious security level.
    expect(document.workspaces.flatMap((workspace) => workspace.conversations)
      .every((conversation) => conversation.settings.securityLevel === "request_approval")).toBe(true);
  });

  it("gives every seeded workspace an empty default preset and no remembered settings", () => {
    const workspaces = createSeedDocument().workspaces;
    expect(workspaces.every((workspace) => (
      workspace.defaultConversationPresetId === "" && workspace.lastConversationSettings === null
    ))).toBe(true);
  });

  it("includes the empty reserved temporary-workspace group", () => {
    const workspaces = createSeedDocument().workspaces;
    expect(workspaces.find((workspace) => workspace.id === "__temporary__"))
      .toMatchObject({ name: "临时工作区", kind: "temporary", path: "", conversations: [] });
  });

  it("normalizes the seed with the built-in Codex and Claude Agent providers", () => {
    const settings = normalizeDocument(createSeedDocument()).globalSettings;
    expect(settings.apiProviders).toHaveLength(2);
    expect(settings.apiProviders[0]).toMatchObject({
      family: "openai_codex",
      enabled: false,
      name: "OpenAI Codex"
    });
    expect(settings.apiProviders[1]).toMatchObject({
      family: "claude_agent",
      enabled: true,
      name: "Claude Agent"
    });
    expect(settings.apiProviders[0].id).toMatch(/^provider_/u);
    expect(settings.apiProviders[1].id).toMatch(/^provider_/u);
    // Codex has no catalog until the user completes its OAuth flow.
    expect(settings.apiProviders[0].models).toEqual([]);
    // Claude Agent's catalog is a local built-in table, so it ships installed.
    const claudeModelIds = settings.apiProviders[1].models.map((model) => model.id);
    expect(claudeModelIds).toContain("claude-opus-5");
    expect(claudeModelIds).toContain("claude-sonnet-5");
    expect(claudeModelIds).toContain("claude-haiku-4-5");
    expect(claudeModelIds.filter((id) => id.includes("[1m]"))).toEqual([]);
    expect(settings.apiProviders[1].activeModelId).toBe(claudeModelIds[0]);
    expect(settings.activeProviderId).toBe(settings.apiProviders[1].id);
    // Keep the TypeScript seed catalog aligned with Rust's product default.
    expect(settings.webSearch.providers.map((provider) => provider.kind)).toEqual([
      "zhipu", "tavily", "searxng", "exa", "exa-mcp", "bocha", "querit", "fetch", "jina", "firecrawl"
    ]);
    expect(
      settings.webSearch.providers.filter((provider) => provider.enabled).map((provider) => provider.kind)
    ).toEqual(["exa-mcp", "jina"]);
    expect(settings.webSearch.fetchProvider).toBe("jina");
    expect(settings.webSearch.maxResults).toBe(5);
    expect(settings.webSearch.excludeDomains).toEqual([]);
    expect(settings.webSearch.compression).toEqual({ method: "cutoff", cutoffLimit: 2000 });
  });

  it("ships workspace, web, and host-run orchestration tools", () => {
    const tools = createSeedDocument().tools;
    expect(tools).toHaveLength(30);
    expect(new Set(tools.map((tool) => tool.name)).size).toBe(tools.length);
    const webTools = tools.filter((tool) => tool.category === "web");
    expect(webTools.map((tool) => tool.name)).toEqual(["web_search", "web_fetch", "playwright"]);
    expect(webTools.filter((tool) => tool.dangerous).map((tool) => tool.name)).toEqual([
      "web_search", "web_fetch", "playwright"
    ]);
    const orchestrationTools = tools.filter((tool) => tool.category === "orchestration");
    expect(orchestrationTools.map((tool) => tool.name)).toEqual([
      "agent_spawn", "send_message", "followup_task", "task_wait", "task_list",
      "skill", "workflow", "todo", "ask_user", "fork",
      "plan", "exit_plan_mode", "enter_plan_mode"
    ]);
    expect(orchestrationTools.filter((tool) => tool.dangerous).map((tool) => tool.name)).toEqual(["workflow"]);
    expect(tools.find((tool) => tool.name === "agent_spawn")).toMatchObject({ label: "子代理" });
    expect(tools.find((tool) => tool.name === "workflow")).toMatchObject({ label: "工作流" });
    expect(tools.find((tool) => tool.name === "todo")).toMatchObject({ label: "待办事项" });
    expect(tools.filter((tool) => tool.category === "memory").map((tool) => tool.name)).toEqual([
      "read_global_memory", "read_project_memory",
      "create_global_memory", "create_project_memory",
      "edit_global_memory", "edit_project_memory"
    ]);
    for (const tool of tools.filter((item) => item.category === "memory")) {
      expect(tool.dangerous).toBe(false);
      // Memory belongs to a location, not a model. No memory tool may carry
      // an identity, a scope selector, or a filesystem path.
      for (const forbidden of ["modelId", "scope", "path", "expected_version"]) {
        expect(tool.parameters.some((parameter) => parameter.name === forbidden)).toBe(false);
      }
    }
    expect(tools.some((tool) => tool.name === "subagent")).toBe(false);
  });

  it("does not ship demonstration timeline data or any built-in capability", () => {
    const document = createSeedDocument();
    const contexts = document.workspaces.flatMap((workspace) => workspace.conversations.flatMap((conversation) => conversation.contexts));
    expect(contexts).toEqual([]);
    expect(JSON.stringify(document)).not.toContain("制作 Agent GUI");
    expect(JSON.stringify(document)).not.toContain("持久化与迁移设计");
    expect(JSON.stringify(document)).not.toMatch(/[A-Za-z]:\\\\Users\\\\/);
    expect(document.capabilities.skills).toEqual([]);
  });

  it("ships the two shipped presets and no other user-managed preset domain", () => {
    const document = createSeedDocument();
    const settings = document.globalSettings;
    const [codex, claudeCode] = settings.conversationPresets;
    expect([codex.name, claudeCode.name]).toEqual(["Codex", "Claude Code"]);
    expect(settings.defaultConversationPresetId).toBe(CLAUDE_CODE_PRESET_ID);

    // Each fleet is exactly three named roles bound to explicit models, thinking
    // on, tools inherited from the preset, and nothing said about itself.
    expect(codex.settings.agentDefinitions.map((role) => role.name))
      .toEqual(["sol", "terra", "luna"]);
    expect(claudeCode.settings.agentDefinitions.map((role) => role.name))
      .toEqual(["opus", "sonnet", "haiku"]);
    for (const preset of settings.conversationPresets) {
      // No anonymous children: every subagent goes through one of the roles.
      expect(preset.settings.allowRolelessSubagents).toBe(false);
      // Everything on except the names the host derives for itself. The plan
      // tools among them: they follow the security level, not a tool toggle.
      expect(preset.settings.enabledTools).toEqual(
        document.tools.map((tool) => tool.name).filter((name) => !(
          document.tools.find((tool) => tool.name === name)!.category === "memory"
          || ["skill", "task_wait", "task_list"].includes(name)
          || ["plan", "exit_plan_mode", "enter_plan_mode"].includes(name)
        ))
      );
      for (const role of preset.settings.agentDefinitions) {
        expect(role).toMatchObject({
          description: "",
          effort: "medium",
          tools: null,
          source: "user",
          sourceKey: "",
          revision: 1,
          memoryEpoch: 1,
          memory: "none"
        });
        expect(role.modelSelection.kind).toBe("explicit");
      }
    }

    // Retired preset collections must not reappear, even as empty arrays.
    for (const retired of [
      "hookPresets",
      "skillPresets",
      "mcpPresets",
      "toolDescriptionSets",
      "securityPolicies",
      "modelPresets"
    ]) {
      expect(settings).not.toHaveProperty(retired);
    }
    expect(document.workspaces.flatMap((workspace) => workspace.conversations)).toEqual([]);
    const implicit = implicitConversationPreset(toolCatalog, "zh-CN").settings;
    expect(implicit.hookIds).toEqual([]);
    expect(implicit.skillIds).toEqual([]);
    expect(implicit.mcpIds).toEqual([]);
    expect(implicit.toolDescriptionFileId).toBeNull();
    for (const retired of [
      "hookPresetIds",
      "skillPresetIds",
      "mcpPresetIds",
      "toolDescriptionSetId",
      "modelPresetId",
      "modelSelection",
      "securityPolicyId"
    ]) {
      expect(implicit).not.toHaveProperty(retired);
    }
    expect(createSeedDocument().capabilities.mcps).toEqual([]);
  });

  it("starts MCP servers, skills, shortcuts, and environment tools empty with factory appearance values", () => {
    const settings = createSeedDocument().globalSettings;
    // New installations register MCP servers and skills explicitly.
    expect(settings.mcpServers).toEqual([]);
    expect(settings.skills).toEqual([]);
    expect(settings.shortcuts).toEqual({});
    expect(settings.environmentTools).toEqual([]);
    expect(settings.appearance).toEqual({
      themeColor: "",
      zoom: 1,
      uiFontFamily: "",
      monoFontFamily: "",
      messageFontSize: 14,
      serifMessages: false,
      wideMessages: false,
      sendShortcut: ["Enter"],
      newlineShortcut: ["Shift", "Enter"],
      spellCheck: false,
      renderUserMarkdown: false,
      confirmMessageDelete: false,
      collapseReasoning: true,
      codeBlockCollapsible: false,
      codeBlockWrappable: false,
      singleDollarMath: true,
      customCss: ""
    });
  });

  it("gives a fresh conversation the default per-conversation search behaviour", () => {
    expect(implicitConversationPreset(toolCatalog, "zh-CN").settings.webSearch).toEqual({
      maxSearchesPerCall: 0,
      provider: { kind: "native" }
    });
    expect(defaultConversationWebSearchSettings()).toEqual({
      maxSearchesPerCall: 0,
      provider: { kind: "native" }
    });
  });

  it("keeps memory tools out of the implicit enabled-tool list", () => {
    // Memory tools are derived from the two memory-layer switches rather than the enabled list.
    const document = createSeedDocument();
    const memoryToolNames = new Set(
      document.tools.filter((tool) => tool.category === "memory").map((tool) => tool.name)
    );
    expect(memoryToolNames.size).toBe(6);
    const implicit = implicitConversationPreset(document.tools, "zh-CN").settings;
    expect(implicit.globalMemoryEnabled).toBe(false);
    expect(implicit.projectMemoryEnabled).toBe(false);
    expect(implicit.enabledTools.some((name) => memoryToolNames.has(name))).toBe(false);
  });

  it("gives a fresh conversation the implicit template's platform shell", () => {
    const implicit = implicitConversationPreset(toolCatalog, "zh-CN").settings;
    expect(implicit.enabledTools.filter(
      (name) => name === "powershell" || name === "bash"
    )).toEqual([preferredShellToolName(window.navigator.platform)]);
  });
});
