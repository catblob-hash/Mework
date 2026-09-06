import { describe, expect, it } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { defaultConversationWebSearchSettings } from "./runtime";
import type { AgentDefinition, ConversationPresetSettings, ConversationSettings } from "../types";
import {
  applyConversationPresetSettings,
  captureConversationPresetSettings,
  cloneConversationSettings,
  conversationPresetById,
  defaultConversationPreset,
  emptyConversationPresetSettings,
  implicitConversationPreset,
  IMPLICIT_CONVERSATION_PRESET_ID,
  preferredShellToolName
} from "./conversationPresets";

/** Reusable preset fields after flattening. */
const PRESET_FIELDS = [
  "agentDefinitions",
  "allowRolelessSubagents",
  "enabledTools",
  "globalMemoryEnabled",
  "hookIds",
  "mcpIds",
  "projectMemoryEnabled",
  "securityLevel",
  "skillIds",
  "skillToolEnabled",
  "systemPrompt",
  "toolDescriptionFileId",
  "webSearch"
];

function testAgentDefinition(name = "reviewer"): AgentDefinition {
  return {
    enabled: true,
    deleted: false,
    name,
    description: "",
    source: "user",
    sourceKey: name,
    revision: 1,
    memoryEpoch: 1,
    modelSelection: { kind: "explicit", providerId: "provider_a", modelId: "model_a" },
    memory: "none",
    effort: "high",
    tools: ["read"],
    disallowedTools: ["write"],
    searchProvider: { kind: "explicit", providerKind: "tavily" }
  };
}

describe("conversation presets", () => {
  it("uses the configured default preset ID", () => {
    const document = createSeedDocument();
    const second = {
      ...document.globalSettings.conversationPresets[0],
      id: "conversation_second",
      name: "Second"
    };
    document.globalSettings.conversationPresets.push(second);
    document.globalSettings.defaultConversationPresetId = second.id;
    expect(defaultConversationPreset(document.globalSettings)?.id).toBe(second.id);
  });

  it("selects the platform shell and localizes the implicit system prompt", () => {
    const document = createSeedDocument();
    // Memory tools derive from the two conversation memory-tier switches.
    const defaultTools = document.tools
      .filter((tool) => tool.category === "filesystem" || tool.name === "web_search")
      .map((tool) => tool.name);
    expect(defaultTools).not.toContain("playwright");

    expect(preferredShellToolName("Win32")).toBe("powershell");
    expect(preferredShellToolName("Linux x86_64")).toBe("bash");
    expect(preferredShellToolName("MacIntel")).toBe("bash");

    // The prompt stays empty in both languages: the host has no default system
    // prompt of its own, so nothing here depends on the UI language at creation
    // time.
    expect(implicitConversationPreset(document.tools, "zh-CN", "Win32").settings)
      .toMatchObject({
        systemPrompt: "",
        enabledTools: [...defaultTools, "powershell"]
      });
    expect(implicitConversationPreset(document.tools, "en-US", "Linux x86_64").settings)
      .toMatchObject({
        systemPrompt: "",
        enabledTools: [...defaultTools, "bash"]
      });
    expect(implicitConversationPreset(document.tools, "en-US", "MacIntel").settings.enabledTools)
      .toEqual([...defaultTools, "bash"]);
  });

  it("keeps the empty and implicit preset shapes flat", () => {
    expect(emptyConversationPresetSettings()).toEqual({
      systemPrompt: "",
      enabledTools: [],
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
    });
    expect(Object.keys(implicitConversationPreset(createSeedDocument().tools).settings).sort())
      .toEqual(PRESET_FIELDS);
  });

  it("captures the reusable subset from one settings argument", () => {
    const document = createSeedDocument();
    const settings = document.workspaces[0].conversations[0].settings;
    settings.hookIds = ["hook_format"];
    settings.toolDescriptionFileId = "tooldesc_user_main_0f0f0f0f";
    settings.agentDefinitions = [testAgentDefinition()];

    const captured = captureConversationPresetSettings(settings);

    // Presets contain no resolvable references, so capture needs only conversation settings.
    expect(captured).toEqual({
      systemPrompt: settings.systemPrompt,
      enabledTools: settings.enabledTools,
      toolDescriptionFileId: "tooldesc_user_main_0f0f0f0f",
      agentDefinitions: [testAgentDefinition()],
      allowRolelessSubagents: settings.allowRolelessSubagents,
      hookIds: ["hook_format"],
      skillIds: ["skill_code_review"],
      mcpIds: ["mcp_workspace"],
      webSearch: settings.webSearch,
      securityLevel: settings.securityLevel,
      globalMemoryEnabled: settings.globalMemoryEnabled,
      projectMemoryEnabled: settings.projectMemoryEnabled,
      skillToolEnabled: settings.skillToolEnabled
    });
    // Conversation-only fields do not enter presets; web search, security, and memory tiers do.
    expect(Object.keys(captured).sort()).toEqual(PRESET_FIELDS);
    captured.enabledTools.push("mutated");
    captured.agentDefinitions[0].tools?.push("write");
    captured.agentDefinitions[0].modelSelection = { kind: "inherit" };
    captured.agentDefinitions[0].searchProvider = { kind: "unavailable" };
    captured.webSearch.provider = { kind: "unavailable" };
    expect(settings.enabledTools).not.toContain("mutated");
    expect(settings.agentDefinitions[0].tools).toEqual(["read"]);
    expect(settings.agentDefinitions[0].modelSelection).toEqual({
      kind: "explicit", providerId: "provider_a", modelId: "model_a"
    });
    // Nested role search settings require a deep copy.
    expect(settings.agentDefinitions[0].searchProvider).toEqual({
      kind: "explicit", providerKind: "tavily"
    });
    // Mutating the captured settings must not affect the conversation.
    expect(settings.webSearch.provider).toEqual({ kind: "native" });
  });

  it("applies a preset straight onto conversation settings and keeps conversation-only fields", () => {
    const document = createSeedDocument();
    const current = document.workspaces[0].conversations[0].settings;
    current.webSearch = { ...current.webSearch, maxSearchesPerCall: 7 };
    const preset: ConversationPresetSettings = {
      systemPrompt: "预设提示词",
      enabledTools: ["read", "read", "extinct-tool"],
      toolDescriptionFileId: "tooldesc_user_preset_11112222",
      agentDefinitions: [testAgentDefinition()],
      allowRolelessSubagents: true,
      hookIds: ["hook_a", "hook_a"],
      skillIds: ["skill_installer"],
      mcpIds: [],
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        maxSearchesPerCall: 42
      },
      securityLevel: "full_access",
      globalMemoryEnabled: true,
      projectMemoryEnabled: false,
      skillToolEnabled: true
    };

    const applied: ConversationSettings = applyConversationPresetSettings(
      current,
      preset,
      new Set(document.tools.map((tool) => tool.name))
    );

    // Return `ConversationSettings` directly without a wrapper.
    expect(applied.systemPrompt).toBe("预设提示词");
    expect(applied).not.toHaveProperty("settings");
    expect(applied).not.toHaveProperty("missingReferences");
    expect(applied.enabledTools).toEqual(["read"]);
    expect(applied.hookIds).toEqual(["hook_a"]);
    expect(applied.toolDescriptionFileId).toBe("tooldesc_user_preset_11112222");
    expect(applied.agentDefinitions).toEqual([testAgentDefinition()]);
    applied.agentDefinitions[0].tools?.push("write");
    applied.agentDefinitions[0].modelSelection = { kind: "inherit" };
    expect(preset.agentDefinitions[0].tools).toEqual(["read"]);
    expect(preset.agentDefinitions[0].modelSelection).toEqual({
      kind: "explicit", providerId: "provider_a", modelId: "model_a"
    });
    // Applying a preset replaces web-search behavior with a deep copy.
    expect(applied.webSearch).toEqual(preset.webSearch);
    applied.webSearch.provider = { kind: "unavailable" };
    expect(preset.webSearch.provider).toEqual({ kind: "native" });
    // Security policy is a preset component.
    expect(applied.securityLevel).toBe("full_access");
    expect(applied.reasoningEffort).toBe(current.reasoningEffort);
    // Memory-tier switches are preset components and replace their respective values.
    expect(current.globalMemoryEnabled).toBe(false);
    expect(applied.globalMemoryEnabled).toBe(true);
    expect(applied.projectMemoryEnabled).toBe(false);
  });

  it("keeps dangling capability IDs on both sides of a capture/apply round trip", () => {
    // Capability IDs may outlive installation, so they must not be filtered by the catalog.
    const document = createSeedDocument();
    const current = document.workspaces[0].conversations[0].settings;
    current.hookIds = ["hook_not_installed"];
    current.skillIds = ["skill_not_installed"];
    current.mcpIds = ["mcp_not_installed"];

    const captured = captureConversationPresetSettings(current);
    const applied = applyConversationPresetSettings(current, captured);

    expect(captured).toMatchObject({
      hookIds: ["hook_not_installed"],
      skillIds: ["skill_not_installed"],
      mcpIds: ["mcp_not_installed"]
    });
    expect(applied).toMatchObject({
      hookIds: ["hook_not_installed"],
      skillIds: ["skill_not_installed"],
      mcpIds: ["mcp_not_installed"]
    });
  });

  it("deep-copies a remembered snapshot so the new conversation cannot edit the workspace's copy", () => {
    const document = createSeedDocument();
    const snapshot: ConversationSettings = {
      ...document.workspaces[0].conversations[0].settings,
      reasoningEffort: "high",
      securityLevel: "full_access",
      includeAppDataPath: true,
      enabledTools: ["read", "read", "no-such-tool"],
      globalMemoryEnabled: true
    };

    const cloned = cloneConversationSettings(
      snapshot,
      new Set(document.tools.map((tool) => tool.name))
    );

    // Snapshot-only fields distinguish remembered settings from a preset application.
    expect(cloned.reasoningEffort).toBe("high");
    expect(cloned.includeAppDataPath).toBe(true);
    expect(cloned.securityLevel).toBe("full_access");
    // Remove duplicate or retired tool names.
    expect(cloned.enabledTools).toEqual(["read"]);
    expect(cloned.globalMemoryEnabled).toBe(true);
    // Nested settings must not be shared with the workspace snapshot.
    cloned.webSearch.provider = { kind: "explicit", providerKind: "tavily" };
    expect(snapshot.webSearch.provider).toEqual({ kind: "native" });
    expect(cloned.hookIds).not.toBe(snapshot.hookIds);
  });

  it("resolves a preset id leniently, including the reserved built-in one", () => {
    const document = createSeedDocument();
    document.globalSettings.conversationPresets = [
      { id: "preset-a", name: "审阅", description: "", settings: emptyConversationPresetSettings() }
    ];

    expect(conversationPresetById(document.globalSettings, "preset-a")?.name).toBe("审阅");
    // Missing and empty IDs resolve to no preset rather than throwing or falling back.
    expect(conversationPresetById(document.globalSettings, "preset-gone")).toBeNull();
    expect(conversationPresetById(document.globalSettings, "")).toBeNull();
    expect(conversationPresetById(
      document.globalSettings,
      IMPLICIT_CONVERSATION_PRESET_ID,
      document.tools
    )?.id).toBe(IMPLICIT_CONVERSATION_PRESET_ID);
  });
});
