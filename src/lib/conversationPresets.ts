import type {
  AgentDefinition,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings,
  ConversationWebSearchSettings,
  GlobalSettings
} from "../types";
import { BUILTIN_PRESET_ID } from "../seed";
import { defaultConversationWebSearchSettings } from "./runtime";
import { isHostDerivedToolName } from "./taskTools";
import { toolLockOf } from "./toolLock";

/**
 * Whether `presetId` names the built-in preset. It ships with the build: the
 * host rewrites it on every start and refuses any save that edits or drops it,
 * so the UI offers no rename, delete or save for it — only apply, and saving
 * what the user made of it as a preset of their own.
 */
export function isBuiltinConversationPreset(presetId: string): boolean {
  return presetId === BUILTIN_PRESET_ID;
}

export function emptyConversationPresetSettings(): ConversationPresetSettings {
  return {
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
        : { ...definition.searchProvider },
      fetchProvider: definition.fetchProvider === null || definition.fetchProvider === undefined
        ? null
        : { ...definition.fetchProvider },
      includeDomains: [...(definition.includeDomains ?? [])],
      excludeDomains: [...(definition.excludeDomains ?? [])]
    }];
  });
}

/** Deep-copies the nested web-search selections so presets and conversations do
 * not share mutable values. */
function copyWebSearchSettings(
  settings: ConversationWebSearchSettings
): ConversationWebSearchSettings {
  return {
    ...settings,
    provider: { ...settings.provider },
    fetchProvider: { ...settings.fetchProvider }
  };
}

/** Captures the reusable preset subset of current conversation settings. Resource
 * IDs are copied directly and may be dangling. */
export function captureConversationPresetSettings(
  settings: ConversationSettings
): ConversationPresetSettings {
  return {
    enabledTools: [...settings.enabledTools],
    toolDescriptionFileId: settings.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(settings.agentDefinitions),
    allowRolelessSubagents: settings.allowRolelessSubagents === true,
    hookIds: [...settings.hookIds],
    skillIds: [...settings.skillIds],
    mcpIds: [...settings.mcpIds],
    webSearch: copyWebSearchSettings(settings.webSearch),
    webSearchEnabled: settings.webSearchEnabled === true,
    securityLevel: settings.securityLevel,
    globalMemoryEnabled: settings.globalMemoryEnabled,
    projectMemoryEnabled: settings.projectMemoryEnabled,
    skillToolEnabled: settings.skillToolEnabled === true,
    mcpToolDiscoveryEnabled: settings.mcpToolDiscoveryEnabled === true
  };
}

/** Structural equality that ignores object key order. */
function equalValues(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((item, index) => equalValues(item, b[index]));
  }
  if (!a || !b || typeof a !== "object" || typeof b !== "object") return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  return [...keys].every((key) => equalValues(left[key], right[key]));
}

function sameIdSet(a: readonly string[], b: readonly string[]): boolean {
  const left = new Set(a);
  const right = new Set(b);
  return left.size === right.size && [...left].every((id) => right.has(id));
}

/**
 * Whether two preset-owned bodies mean the same thing, deciding when a
 * conversation stops belonging to the preset it was applied from.
 *
 * Tool and resource ID lists compare as sets: the settings panel rewrites
 * `enabledTools` in view order, which is not an edit the user made.
 */
export function sameConversationPresetSettings(
  a: ConversationPresetSettings,
  b: ConversationPresetSettings
): boolean {
  return a.toolDescriptionFileId === b.toolDescriptionFileId
    && a.allowRolelessSubagents === b.allowRolelessSubagents
    && a.securityLevel === b.securityLevel
    && a.globalMemoryEnabled === b.globalMemoryEnabled
    && a.projectMemoryEnabled === b.projectMemoryEnabled
    && a.skillToolEnabled === b.skillToolEnabled
    && a.mcpToolDiscoveryEnabled === b.mcpToolDiscoveryEnabled
    && a.webSearchEnabled === b.webSearchEnabled
    && sameIdSet(a.enabledTools, b.enabledTools)
    && sameIdSet(a.hookIds, b.hookIds)
    && sameIdSet(a.skillIds, b.skillIds)
    && sameIdSet(a.mcpIds, b.mcpIds)
    && equalValues(a.agentDefinitions, b.agentDefinitions)
    && equalValues(a.webSearch, b.webSearch);
}

/**
 * The preset a new conversation starts from. The host keeps the built-in preset
 * in every document, so `null` — nothing to apply — only answers a document
 * that never went through it.
 */
export function defaultConversationPreset(settings: GlobalSettings): ConversationPreset | null {
  return settings.conversationPresets.find(
    (preset) => preset.id === settings.defaultConversationPresetId
  ) ?? settings.conversationPresets[0] ?? null;
}

/**
 * Resolves an applicable preset by ID. Missing IDs return `null` because deleted
 * presets may still be referenced.
 */
export function conversationPresetById(
  settings: GlobalSettings,
  presetId: string
): ConversationPreset | null {
  if (!presetId) return null;
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
    webSearchEnabled: snapshot.webSearchEnabled === true,
    reasoningEffort: snapshot.reasoningEffort,
    securityLevel: snapshot.securityLevel,
    planModeEnabled: snapshot.planModeEnabled === true,
    globalMemoryEnabled: snapshot.globalMemoryEnabled === true,
    projectMemoryEnabled: snapshot.projectMemoryEnabled === true,
    skillToolEnabled: snapshot.skillToolEnabled === true,
    mcpToolDiscoveryEnabled: snapshot.mcpToolDiscoveryEnabled === true
    // `toolLock` is deliberately absent: the new conversation has run nothing
    // yet, so it has exposed nothing and every setting is still free to move.
  };
}

/** Applies a preset as a complete overlay for preset-owned fields. Conversation-
 * specific reasoning effort and app-data path remain unchanged; host-derived
 * memory tools are excluded because layer switches derive them. The one thing
 * a preset cannot move is a pin: a preset naming another backend where a
 * native one has already run applies everything else and leaves that one field
 * where the transcript put it. A host-run backend is not pinned and moves with
 * the preset. Whether the tool surface may move at all is the model's
 * question, not the preset's — the caller asks it (`restoreLockedSettings`). */
export function applyConversationPresetSettings(
  current: ConversationSettings,
  preset: ConversationPresetSettings,
  knownToolNames?: ReadonlySet<string>
): ConversationSettings {
  const lock = toolLockOf(current);
  const webSearch = copyWebSearchSettings(preset.webSearch);
  const enabledTools = Array.from(new Set(preset.enabledTools)).filter(
    (name) => (!knownToolNames || knownToolNames.has(name)) && !isHostDerivedToolName(name)
  );
  return {
    ...current,
    enabledTools,
    toolDescriptionFileId: preset.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(preset.agentDefinitions),
    allowRolelessSubagents: preset.allowRolelessSubagents === true,
    hookIds: [...new Set(preset.hookIds)],
    skillIds: [...new Set(preset.skillIds)],
    mcpIds: [...new Set(preset.mcpIds)],
    webSearch: {
      ...webSearch,
      provider: lock.searchProvider ?? webSearch.provider,
      fetchProvider: lock.fetchProvider ?? webSearch.fetchProvider
    },
    webSearchEnabled: preset.webSearchEnabled === true,
    securityLevel: preset.securityLevel,
    globalMemoryEnabled: preset.globalMemoryEnabled === true,
    projectMemoryEnabled: preset.projectMemoryEnabled === true,
    skillToolEnabled: preset.skillToolEnabled === true,
    mcpToolDiscoveryEnabled: preset.mcpToolDiscoveryEnabled === true
  };
}
