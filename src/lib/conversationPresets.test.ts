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
  sameConversationPresetSettings
} from "./conversationPresets";

/** Reusable preset fields after flattening. */
const PRESET_FIELDS = [
  "agentDefinitions",
  "allowRolelessSubagents",
  "enabledTools",
  "globalMemoryEnabled",
  "hookIds",
  "mcpIds",
  "mcpToolDiscoveryEnabled",
  "projectMemoryEnabled",
  "securityLevel",
  "skillIds",
  "skillToolEnabled",
  "toolDescriptionFileId",
  "webSearch",
  "webSearchEnabled"
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
    searchProvider: { kind: "explicit", providerKind: "tavily" },
    fetchProvider: null,
    maxResults: 5,
    compressionCutoff: 2000,
    domainFilter: null,
    includeDomains: [],
    excludeDomains: [],
    templateId: null
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

  it("enables every backend's shell tool in the implicit preset", () => {
    const document = createSeedDocument();
    // Memory and web tools derive from their conversation switches, not the list.
    const defaultTools = document.tools
      .filter((tool) => tool.category === "filesystem")
      .map((tool) => tool.name);
    expect(defaultTools).not.toContain("preview_start");
    // Turning the switch on is what grants the web tools.
    expect(implicitConversationPreset(document.tools).settings.webSearchEnabled).toBe(true);

    // Every backend's command tool is on — a conversation offers only the ones
    // its machines have — and the default set does not depend on the UI language.
    const shellTools = document.tools
      .filter((tool) => ["bash", "zsh", "sh", "powershell"].includes(tool.name))
      .map((tool) => tool.name);
    expect([...shellTools].sort()).toEqual(["bash", "powershell", "sh", "zsh"]);
    expect(implicitConversationPreset(document.tools, "zh-CN").settings.enabledTools)
      .toEqual([...defaultTools, ...shellTools]);
    expect(implicitConversationPreset(document.tools, "en-US").settings.enabledTools)
      .toEqual(implicitConversationPreset(document.tools, "zh-CN").settings.enabledTools);
  });

  it("keeps the empty and implicit preset shapes flat", () => {
    expect(emptyConversationPresetSettings()).toEqual({
      enabledTools: [],
      toolDescriptionFileId: null,
      agentDefinitions: [],
      allowRolelessSubagents: false,
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      webSearch: defaultConversationWebSearchSettings(),
      webSearchEnabled: false,
      securityLevel: "request_approval",
      globalMemoryEnabled: false,
      projectMemoryEnabled: false,
      skillToolEnabled: false,
      mcpToolDiscoveryEnabled: false
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
      enabledTools: settings.enabledTools,
      toolDescriptionFileId: "tooldesc_user_main_0f0f0f0f",
      agentDefinitions: [testAgentDefinition()],
      allowRolelessSubagents: settings.allowRolelessSubagents,
      hookIds: ["hook_format"],
      skillIds: ["skill_code_review"],
      mcpIds: ["mcp_workspace"],
      webSearch: settings.webSearch,
      webSearchEnabled: true,
      securityLevel: settings.securityLevel,
      globalMemoryEnabled: settings.globalMemoryEnabled,
      projectMemoryEnabled: settings.projectMemoryEnabled,
      skillToolEnabled: settings.skillToolEnabled,
      mcpToolDiscoveryEnabled: settings.mcpToolDiscoveryEnabled
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
      webSearchEnabled: true,
      securityLevel: "full_access",
      globalMemoryEnabled: true,
      projectMemoryEnabled: false,
      skillToolEnabled: true,
      mcpToolDiscoveryEnabled: true
    };

    const applied: ConversationSettings = applyConversationPresetSettings(
      current,
      preset,
      new Set(document.tools.map((tool) => tool.name))
    );

    // Return `ConversationSettings` directly without a wrapper.
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

  it("lets a preset widen a locked conversation but never move what a run has settled", () => {
    const document = createSeedDocument();
    const current: ConversationSettings = {
      ...document.workspaces[0].conversations[0].settings,
      skillIds: ["skill_locked"],
      skillToolEnabled: false,
      webSearchEnabled: true,
      toolLock: {
        tools: [],
        mcpIds: [],
        globalMemory: false,
        projectMemory: false,
        skillTool: false,
        mcpToolDiscovery: false,
        webSearch: true,
        skillIds: ["skill_locked"],
        promptSkillIds: ["skill_locked"],
        searchProvider: { kind: "native" },
        fetchProvider: { kind: "native" }
      }
    };
    const preset = {
      ...captureConversationPresetSettings(current),
      skillIds: ["skill_other"],
      skillToolEnabled: true,
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        provider: { kind: "explicit" as const, providerKind: "tavily" as const },
        fetchProvider: { kind: "explicit" as const, providerKind: "jina" as const }
      }
    };

    const applied = applyConversationPresetSettings(current, preset);

    // Widening is what a preset is for.
    expect(applied.skillIds).toEqual(["skill_locked", "skill_other"]);
    // Everything a run has acted on stays where it put it.
    expect(applied.skillToolEnabled).toBe(false);
    expect(applied.webSearch.provider).toEqual({ kind: "native" });
    expect(applied.webSearch.fetchProvider).toEqual({ kind: "native" });
    // The rest of the preset's web-search body still applies.
    expect(applied.webSearch.maxSearchesPerCall).toBe(preset.webSearch.maxSearchesPerCall);
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
      {
        id: "preset-a",
        name: "审阅",
        description: "",
        templateId: "",
        settings: emptyConversationPresetSettings()
      }
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

  it("treats reordered tool and resource lists as the same preset body", () => {
    const base: ConversationPresetSettings = {
      ...emptyConversationPresetSettings(),
      enabledTools: ["read_file", "write_file", "bash"],
      hookIds: ["hook-a", "hook-b"],
      skillIds: ["skill-a"],
      mcpIds: ["mcp-a"]
    };
    // The settings panel rewrites `enabledTools` in view order; that is not an edit.
    expect(sameConversationPresetSettings(base, {
      ...base,
      enabledTools: ["bash", "write_file", "read_file"],
      hookIds: ["hook-b", "hook-a"]
    })).toBe(true);
    expect(sameConversationPresetSettings(base, {
      ...base,
      enabledTools: ["read_file", "write_file"]
    })).toBe(false);
    expect(sameConversationPresetSettings(base, { ...base, securityLevel: "full_access" })).toBe(false);
    expect(sameConversationPresetSettings(base, {
      ...base,
      webSearch: { ...base.webSearch, maxSearchesPerCall: base.webSearch.maxSearchesPerCall + 1 }
    })).toBe(false);
  });

  it("compares nested roles structurally, so a deep copy still counts as unchanged", () => {
    const withRoles = (definitions: AgentDefinition[]): ConversationPresetSettings => ({
      ...emptyConversationPresetSettings(),
      agentDefinitions: definitions
    });
    const original = withRoles([testAgentDefinition("reviewer")]);
    // Applying a preset deep-copies its roles, so equality can never rely on identity.
    expect(sameConversationPresetSettings(
      original,
      captureConversationPresetSettings({
        ...emptyConversationPresetSettings(),
        includeAppDataPath: false,
        reasoningEffort: "medium",
        agentDefinitions: [testAgentDefinition("reviewer")]
      } as ConversationSettings)
    )).toBe(true);
    expect(sameConversationPresetSettings(original, withRoles([
      { ...testAgentDefinition("reviewer"), effort: "low" }
    ]))).toBe(false);
    expect(sameConversationPresetSettings(original, withRoles([]))).toBe(false);
  });
});
