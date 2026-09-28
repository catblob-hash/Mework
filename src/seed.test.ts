import { describe, expect, it } from "vitest";
// Vite's ?raw import loads Rust source as plain text for cross-language constant alignment.
// It needs neither Node types nor compilation.
import storageSource from "../src-tauri/src/storage.rs?raw";
import catalogSource from "../src-tauri/src/catalog.rs?raw";
import {
  BUILTIN_PRESET_ID,
  BUILTIN_PRESET_TEMPLATE_ID,
  createSeedDocument
} from "./seed";
import { defaultConversationWebSearchSettings, normalizeDocument } from "./lib/runtime";
import type { ConversationPreset } from "./types";

function builtinPresetOf(platform?: string): ConversationPreset {
  const presets = createSeedDocument(platform).globalSettings.conversationPresets;
  const preset = presets.find((candidate) => candidate.id === BUILTIN_PRESET_ID);
  expect(preset).toBeDefined();
  return preset!;
}

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
    expect(document.globalSettings.defaultConversationPresetId).toBe(BUILTIN_PRESET_ID);
    expect(document.globalSettings.conversationPresets.map((preset) => preset.id))
      .toEqual([BUILTIN_PRESET_ID]);
    // Seed conversations always use the most cautious security level.
    expect(document.workspaces.flatMap((workspace) => workspace.conversations)
      .every((conversation) => conversation.settings.securityLevel === "request_approval")).toBe(true);
  });

  it("gives every seeded workspace an empty default preset and no remembered settings", () => {
    const workspaces = createSeedDocument().workspaces;
    expect(workspaces).toHaveLength(1);
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
    // Claude Agent ships its seed rows installed; fetching asks the CLI later.
    const claudeModelIds = settings.apiProviders[1].models.map((model) => model.id);
    expect(claudeModelIds).toContain("claude-opus-5-5");
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
  });

  it("ships workspace, web, and host-run orchestration tools", () => {
    const tools = createSeedDocument().tools;
    expect(tools).toHaveLength(48);
    expect(new Set(tools.map((tool) => tool.name)).size).toBe(tools.length);
    const webTools = tools.filter((tool) => tool.category === "web");
    expect(webTools.map((tool) => tool.name)).toEqual([
      "web_search", "web_fetch",
      "preview_start", "preview_stop", "preview_list", "preview_logs", "preview_console_logs",
      "preview_screenshot", "preview_snapshot", "preview_inspect", "preview_click",
      "preview_fill", "preview_eval", "preview_network", "preview_resize",
      "preview_upload_image", "preview_dialog"
    ]);
    // preview_list and preview_logs read host-side state the user's own launch.json
    // produced: no page content, no process started or killed.
    expect(webTools.filter((tool) => !tool.dangerous).map((tool) => tool.name)).toEqual([
      "preview_list", "preview_logs"
    ]);
    const orchestrationTools = tools.filter((tool) => tool.category === "orchestration");
    expect(orchestrationTools.map((tool) => tool.name)).toEqual([
      "agent_spawn", "send_message", "followup_task", "task_wait", "task_list", "box",
      "skill", "tool_search", "workflow", "todo", "ask_user", "fork",
      "plan", "exit_plan_mode"
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

  it("ships the one built-in preset and no other user-managed preset domain", () => {
    const document = createSeedDocument("MacIntel");
    const settings = document.globalSettings;
    const [preset] = settings.conversationPresets;
    expect(preset.name).toBe("mework");
    expect(settings.defaultConversationPresetId).toBe(BUILTIN_PRESET_ID);

    // Three named roles bound to explicit models, thinking on, tools inherited
    // from the preset, and a description saying when to pick each.
    const provider = (family: string) => settings.apiProviders.find((item) => item.family === family)!.id;
    expect(preset.settings.agentDefinitions.map((role) => [role.name, role.modelSelection])).toEqual([
      ["Opus", { kind: "explicit", providerId: provider("claude_agent"), modelId: "claude-opus-5-5" }],
      ["Sol", { kind: "explicit", providerId: provider("openai_codex"), modelId: "gpt-6-sol" }],
      ["Luna", { kind: "explicit", providerId: provider("openai_codex"), modelId: "gpt-6-luna" }]
    ]);
    for (const role of preset.settings.agentDefinitions) {
      expect(role).toMatchObject({
        effort: "medium",
        tools: null,
        source: "user",
        sourceKey: "",
        revision: 1,
        memoryEpoch: 1,
        memory: "none",
        maxResults: 5,
        compressionCutoff: 2000
      });
      // The host's table is the one that ships; this mirror has to say the same.
      expect(role.description).not.toBe("");
      expect(catalogSource).toContain(`"${role.name}"`);
      expect(catalogSource).toContain(`"${role.description}"`);
    }
    expect(catalogSource).toContain(`const BUILTIN_PRESET_ID: &str = "${BUILTIN_PRESET_ID}";`);
    expect(catalogSource).toContain(
      `const BUILTIN_PRESET_TEMPLATE_ID: &str = "${BUILTIN_PRESET_TEMPLATE_ID}";`
    );

    // No anonymous children: every subagent goes through one of the roles.
    expect(preset.settings.allowRolelessSubagents).toBe(false);
    // Everything on except the names the host derives for itself. The two web
    // tools among them: they follow `webSearchEnabled`, and the plan tools
    // follow the security level, not a tool toggle. Of the shells, only this
    // machine's most preferred one.
    expect(preset.settings.enabledTools).toEqual(
      document.tools.map((tool) => tool.name).filter((name) => !(
        document.tools.find((tool) => tool.name === name)!.category === "memory"
        || ["skill", "tool_search", "task_wait", "task_list", "box", "web_search", "web_fetch"].includes(name)
        || ["plan", "exit_plan_mode"].includes(name)
        || ["bash", "sh", "powershell"].includes(name)
      ))
    );
    expect(preset.settings.enabledTools).toEqual(expect.arrayContaining(["zsh", "preview_start", "workflow"]));
    // The memory tools are switched rather than listed, and both switches are on.
    expect(preset.settings.globalMemoryEnabled).toBe(true);
    expect(preset.settings.projectMemoryEnabled).toBe(true);
    // Web access is on even though the two web tool names are host-derived
    // from this switch.
    expect(preset.settings.webSearchEnabled).toBe(true);
    // Both capability surfaces load on demand rather than inlining every
    // selected body and every MCP schema into the system prompt.
    expect(preset.settings.skillToolEnabled).toBe(true);
    expect(preset.settings.mcpToolDiscoveryEnabled).toBe(true);
    // Capability ids hash an absolute path, so only the host can mint them;
    // the renderer seed ships none. Hooks are never selected even there — a
    // dangling hook id fails every run of the conversation closed.
    expect(preset.settings.skillIds).toEqual([]);
    expect(preset.settings.mcpIds).toEqual([]);
    expect(preset.settings.hookIds).toEqual([]);
    expect(preset.settings.toolDescriptionFileId).toBeNull();
    // It opens with the system prompt the host writes behind this id.
    expect(preset.templateId).toBe(BUILTIN_PRESET_TEMPLATE_ID);

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
    for (const retired of [
      "hookPresetIds",
      "skillPresetIds",
      "mcpPresetIds",
      "toolDescriptionSetId",
      "modelPresetId",
      "modelSelection",
      "securityPolicyId"
    ]) {
      expect(preset.settings).not.toHaveProperty(retired);
    }
    expect(document.workspaces.flatMap((workspace) => workspace.conversations)).toEqual([]);
    expect(createSeedDocument().capabilities.mcps).toEqual([]);
  });

  it("starts shortcuts and environment tools empty with factory appearance values", () => {
    const settings = createSeedDocument().globalSettings;
    /* Skills and MCP servers are not seeded here at all: they are files the user
     * owns under `~/.mework` and each workspace's `.mework`, discovered by the
     * host rather than recorded in the document. */
    expect(settings).not.toHaveProperty("mcpServers");
    expect(settings).not.toHaveProperty("skills");
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
      customCss: "",
      liquidGlass: false,
      background: "solid",
      localModel: { titles: false, shellExplanations: false, titlePrompt: "", shellPrompt: "" }
    });
  });

  it("carries a custom background from before the library forward as glass over its picture", () => {
    const picture = "a".repeat(64);
    const read = (appearance: Record<string, unknown>) => {
      const document = createSeedDocument();
      return normalizeDocument({
        ...document,
        globalSettings: { ...document.globalSettings, appearance: { ...document.globalSettings.appearance, ...appearance } }
      }).globalSettings.appearance;
    };
    // The seed's own values stand in for a current document; drop them to read an old one.
    const legacy = (fields: Record<string, unknown>) => ({ background: undefined, liquidGlass: undefined, ...fields });

    expect(read(legacy({ customBackground: true, backgroundImage: picture })))
      .toMatchObject({ liquidGlass: true, background: picture });
    // Off, the remembered picture was not on screen; the plain theme was.
    expect(read(legacy({ customBackground: false, backgroundImage: picture })))
      .toMatchObject({ liquidGlass: false, background: "solid" });
    expect(read({ background: "builtin:nowhere" }).background).toBe("solid");
    expect(read({ background: "solid:night" }).background).toBe("solid:night");
    expect(read({ background: "builtin:shelf", liquidGlass: true }))
      .toMatchObject({ liquidGlass: true, background: "builtin:shelf" });
  });

  it("gives a fresh conversation the default per-conversation search behaviour", () => {
    expect(builtinPresetOf().settings.webSearch).toEqual({
      maxSearchesPerCall: 0,
      provider: { kind: "native" },
      fetchProvider: { kind: "native" },
      nativeSearchTool: "web_search_20250305",
      nativeFetchTool: "web_fetch_20250910",
      maxResults: 5,
      compressionCutoff: 2000,
      domainFilter: "off",
      includeDomains: [],
      excludeDomains: []
    });
    expect(defaultConversationWebSearchSettings()).toEqual({
      maxSearchesPerCall: 0,
      provider: { kind: "native" },
      fetchProvider: { kind: "native" },
      nativeSearchTool: "web_search_20250305",
      nativeFetchTool: "web_fetch_20250910",
      maxResults: 5,
      compressionCutoff: 2000,
      domainFilter: "off",
      includeDomains: [],
      excludeDomains: []
    });
  });

  it("keeps memory tools out of the built-in preset's enabled-tool list", () => {
    // Memory tools are derived from the two memory-layer switches rather than the enabled list.
    const document = createSeedDocument();
    const memoryToolNames = new Set(
      document.tools.filter((tool) => tool.category === "memory").map((tool) => tool.name)
    );
    expect(memoryToolNames.size).toBe(6);
    const settings = builtinPresetOf().settings;
    expect(settings.globalMemoryEnabled).toBe(true);
    expect(settings.projectMemoryEnabled).toBe(true);
    expect(settings.enabledTools.some((name) => memoryToolNames.has(name))).toBe(false);
  });

  it("turns on the platform's most preferred shell in the built-in preset", () => {
    const shellsOn = (platform: string) => builtinPresetOf(platform).settings.enabledTools
      .filter((name) => ["bash", "zsh", "sh", "powershell"].includes(name));
    expect(shellsOn("Win32")).toEqual(["powershell"]);
    expect(shellsOn("MacIntel")).toEqual(["zsh"]);
    expect(shellsOn("Linux x86_64")).toEqual(["bash"]);
    // A platform the table does not know gets the first shell in tool order.
    expect(shellsOn("")).toEqual(["bash"]);
  });
});
