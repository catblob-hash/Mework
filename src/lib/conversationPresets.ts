import type {
  AgentDefinition,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings,
  ConversationWebSearchSettings,
  GlobalSettings,
  ResolvedAppLanguage,
  ToolDescriptor
} from "../types";
import { defaultConversationWebSearchSettings } from "./runtime";
import { isHostDerivedToolName } from "./taskTools";

export const IMPLICIT_CONVERSATION_PRESET_ID = "__implicit_conversation_preset__";

export function emptyConversationPresetSettings(): ConversationPresetSettings {
  return {
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
  };
}

export function preferredShellToolName(platform: string): "powershell" | "bash" {
  return /^win/i.test(platform.trim()) ? "powershell" : "bash";
}

function browserPlatform(): string {
  return typeof navigator === "undefined" ? "" : navigator.platform;
}

export function implicitConversationPreset(
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN",
  platform: string = browserPlatform()
): ConversationPreset {
  const shellToolName = preferredShellToolName(platform);
  // Memory tools derive from their two layer switches and remain disabled by
  // default because they read and write user-disk files. Name `web_search`
  // explicitly rather than allowing the whole web category, which also includes
  // the browser.
  const enabledTools = tools
    .filter((tool) => tool.category === "filesystem" || tool.name === "web_search")
    .map((tool) => tool.name);
  if (tools.some((tool) => tool.name === shellToolName)) enabledTools.push(shellToolName);
  return {
    id: IMPLICIT_CONVERSATION_PRESET_ID,
    name: resolvedLanguage === "zh-CN"
      ? "内置工程默认值"
      : "Built-in engineering defaults",
    description: "",
    settings: {
      ...emptyConversationPresetSettings(),
      // Empty on purpose, and it stays empty: the host has no default system
      // prompt of its own, so a fresh conversation sends only the capability
      // sections until the user writes a prompt in the UI.
      systemPrompt: "",
      enabledTools
    }
  };
}

/** Deep-copies nested agent definitions. Host-owned fields are copied unchanged;
 * duplicate names retain only their first definition, matching host uniqueness. */
function copyAgentDefinitions(
  definitions: readonly AgentDefinition[]
): AgentDefinition[] {
  const seen = new Set<string>();
  return definitions.flatMap((definition) => {
    if (seen.has(definition.name)) return [];
    seen.add(definition.name);
    return [{
      ...definition,
      modelSelection: { ...definition.modelSelection },
      tools: definition.tools === null ? null : [...definition.tools],
      disallowedTools: [...definition.disallowedTools],
      searchProvider: definition.searchProvider === null
        ? null
        : { ...definition.searchProvider }
    }];
  });
}

/** Deep-copies the nested web-search provider so presets and conversations do
 * not share mutable values. */
function copyWebSearchSettings(
  settings: ConversationWebSearchSettings
): ConversationWebSearchSettings {
  return {
    ...settings,
    provider: { ...settings.provider }
  };
}

/** Captures the reusable preset subset of current conversation settings. Resource
 * IDs are copied directly and may be dangling. */
export function captureConversationPresetSettings(
  settings: ConversationSettings
): ConversationPresetSettings {
  return {
    systemPrompt: settings.systemPrompt,
    enabledTools: [...settings.enabledTools],
    toolDescriptionFileId: settings.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(settings.agentDefinitions),
    allowRolelessSubagents: settings.allowRolelessSubagents === true,
    hookIds: [...settings.hookIds],
    skillIds: [...settings.skillIds],
    mcpIds: [...settings.mcpIds],
    webSearch: copyWebSearchSettings(settings.webSearch),
    securityLevel: settings.securityLevel,
    globalMemoryEnabled: settings.globalMemoryEnabled,
    projectMemoryEnabled: settings.projectMemoryEnabled,
    skillToolEnabled: settings.skillToolEnabled === true
  };
}

export function defaultConversationPreset(
  settings: GlobalSettings,
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN",
  platform: string = browserPlatform()
): ConversationPreset {
  return settings.conversationPresets.find(
    (preset) => preset.id === settings.defaultConversationPresetId
  ) ?? settings.conversationPresets[0] ?? implicitConversationPreset(
    tools,
    resolvedLanguage,
    platform
  );
}

/**
 * Resolves an applicable preset by ID. The built-in default uses a reserved ID;
 * missing custom IDs return `null` because deleted presets may still be referenced.
 */
export function conversationPresetById(
  settings: GlobalSettings,
  presetId: string,
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN",
  platform: string = browserPlatform()
): ConversationPreset | null {
  if (!presetId) return null;
  if (presetId === IMPLICIT_CONVERSATION_PRESET_ID) {
    return implicitConversationPreset(tools, resolvedLanguage, platform);
  }
  return settings.conversationPresets.find((preset) => preset.id === presetId) ?? null;
}

/**
 * Clones a workspace's saved settings snapshot into a new conversation.
 *
 * The snapshot is complete rather than an overlay, so all fields and nested
 * mutable structures are copied. Remove vanished and host-derived tool names.
 */
export function cloneConversationSettings(
  snapshot: ConversationSettings,
  knownToolNames?: ReadonlySet<string>
): ConversationSettings {
  return {
    systemPrompt: snapshot.systemPrompt,
    includeAppDataPath: snapshot.includeAppDataPath === true,
    enabledTools: Array.from(new Set(snapshot.enabledTools)).filter(
      (name) => (!knownToolNames || knownToolNames.has(name)) && !isHostDerivedToolName(name)
    ),
    hookIds: [...new Set(snapshot.hookIds)],
    skillIds: [...new Set(snapshot.skillIds)],
    mcpIds: [...new Set(snapshot.mcpIds)],
    toolDescriptionFileId: snapshot.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(snapshot.agentDefinitions),
    allowRolelessSubagents: snapshot.allowRolelessSubagents === true,
    webSearch: copyWebSearchSettings(snapshot.webSearch),
    reasoningEffort: snapshot.reasoningEffort,
    securityLevel: snapshot.securityLevel,
    globalMemoryEnabled: snapshot.globalMemoryEnabled === true,
    projectMemoryEnabled: snapshot.projectMemoryEnabled === true,
    skillToolEnabled: snapshot.skillToolEnabled === true
  };
}

/** Applies a preset as a complete overlay for preset-owned fields. Conversation-
 * specific reasoning effort and app-data path remain unchanged; host-derived
 * memory tools are excluded because layer switches derive them. */
export function applyConversationPresetSettings(
  current: ConversationSettings,
  preset: ConversationPresetSettings,
  knownToolNames?: ReadonlySet<string>
): ConversationSettings {
  return {
    ...current,
    systemPrompt: preset.systemPrompt,
    enabledTools: Array.from(new Set(preset.enabledTools)).filter(
      (name) => (!knownToolNames || knownToolNames.has(name)) && !isHostDerivedToolName(name)
    ),
    toolDescriptionFileId: preset.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(preset.agentDefinitions),
    allowRolelessSubagents: preset.allowRolelessSubagents === true,
    hookIds: [...new Set(preset.hookIds)],
    skillIds: [...new Set(preset.skillIds)],
    mcpIds: [...new Set(preset.mcpIds)],
    webSearch: copyWebSearchSettings(preset.webSearch),
    securityLevel: preset.securityLevel,
    globalMemoryEnabled: preset.globalMemoryEnabled === true,
    projectMemoryEnabled: preset.projectMemoryEnabled === true,
    skillToolEnabled: preset.skillToolEnabled === true
  };
}
