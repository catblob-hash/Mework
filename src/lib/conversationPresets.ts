import type {
  AgentDefinition,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings,
  ConversationWebSearchSettings,
  GlobalSettings,
  ResolvedAppLanguage,
  SandboxSettings,
  ToolDescriptor
} from "../types";
import {
  decisionMissScoringFor,
  decisionModesFor,
  flooredDecisionParameterModes,
  normalizeDecisionMissScoring,
  normalizeDecisionParameterModes,
  sameDecisionMissScoring,
  sameDecisionParameterModes
} from "./decisionParameters";
import { defaultConversationWebSearchSettings, defaultSandboxSettings } from "./runtime";
import { isHostDerivedToolName } from "./taskTools";
import { toolLockOf } from "./toolLock";

export const IMPLICIT_CONVERSATION_PRESET_ID = "__implicit_conversation_preset__";

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
    mcpToolDiscoveryEnabled: false,
    decisionParameterModes: {},
    decisionMissScoring: []
  };
}

/**
 * The shell command tools the implicit preset turns on: every backend's. Which
 * of them a conversation actually offers follows its machines — the host
 * withdraws a shell no machine has — so enabling them all is what lets each
 * machine's shells work without a trip to the settings. The scoring variants
 * stay off, as the seeded presets keep every decision tool off.
 */
const IMPLICIT_SHELL_TOOLS: ReadonlySet<string> = new Set(["bash", "zsh", "sh", "powershell"]);

export function implicitConversationPreset(
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN"
): ConversationPreset {
  // Memory tools derive from their two layer switches and remain disabled by
  // default because they read and write user-disk files. The web tools derive
  // from `webSearchEnabled` for the same reason, so this list names neither of
  // them — turning the switch on is what grants them.
  const enabledTools = tools
    .filter((tool) => tool.category === "filesystem")
    .map((tool) => tool.name);
  enabledTools.push(...tools.filter((tool) => IMPLICIT_SHELL_TOOLS.has(tool.name)).map((tool) => tool.name));
  return {
    id: IMPLICIT_CONVERSATION_PRESET_ID,
    name: resolvedLanguage === "zh-CN"
      ? "内置工程默认值"
      : "Built-in engineering defaults",
    description: "",
    // It has no row on disk, so there is nowhere to hang a template body either.
    templateId: "",
    settings: {
      ...emptyConversationPresetSettings(),
      enabledTools,
      webSearchEnabled: true
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

/** Deep-copies a sandbox so presets and conversations do not share its lists. */
function copySandboxSettings(settings: SandboxSettings): SandboxSettings {
  return {
    ...settings,
    network: {
      ...settings.network,
      allow: [...settings.network.allow],
      deny: [...settings.network.deny]
    },
    writable: [...settings.writable],
    denyRead: [...settings.denyRead]
  };
}

/** A body's sandbox as a field, left out when the body states none — which reads as off. */
function sandboxField(settings: SandboxSettings | undefined): { sandbox?: SandboxSettings } {
  return settings ? { sandbox: copySandboxSettings(settings) } : {};
}

/** Captures the reusable preset subset of current conversation settings. Resource
 * IDs are copied directly and may be dangling. */
export function captureConversationPresetSettings(
  settings: ConversationSettings
): ConversationPresetSettings {
  const decisionParameterModes = decisionModesFor(
    normalizeDecisionParameterModes(settings.decisionParameterModes),
    settings.enabledTools
  );
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
    mcpToolDiscoveryEnabled: settings.mcpToolDiscoveryEnabled === true,
    /* Only the tools the body enables: a form on a tool that is off says nothing. */
    decisionParameterModes,
    /* Likewise only the tools with a form: one on its direct form never misses. */
    decisionMissScoring: decisionMissScoringFor(
      normalizeDecisionMissScoring(settings.decisionMissScoring),
      decisionParameterModes,
      settings.enabledTools
    ),
    ...sandboxField(settings.sandbox)
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
    && equalValues(a.webSearch, b.webSearch)
    && sameDecisionParameterModes(
      decisionModesFor(a.decisionParameterModes, a.enabledTools),
      decisionModesFor(b.decisionParameterModes, b.enabledTools)
    )
    && sameDecisionMissScoring(
      decisionMissScoringFor(a.decisionMissScoring, a.decisionParameterModes, a.enabledTools),
      decisionMissScoringFor(b.decisionMissScoring, b.decisionParameterModes, b.enabledTools)
    )
    /* An unstated sandbox is the default one, so leaving it out and writing it
       out in full say the same thing. */
    && equalValues(a.sandbox ?? defaultSandboxSettings(), b.sandbox ?? defaultSandboxSettings());
}

export function defaultConversationPreset(
  settings: GlobalSettings,
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN"
): ConversationPreset {
  return settings.conversationPresets.find(
    (preset) => preset.id === settings.defaultConversationPresetId
  ) ?? settings.conversationPresets[0] ?? implicitConversationPreset(tools, resolvedLanguage);
}

/**
 * Resolves an applicable preset by ID. The built-in default uses a reserved ID;
 * missing custom IDs return `null` because deleted presets may still be referenced.
 */
export function conversationPresetById(
  settings: GlobalSettings,
  presetId: string,
  tools: readonly ToolDescriptor[] = [],
  resolvedLanguage: ResolvedAppLanguage = "zh-CN"
): ConversationPreset | null {
  if (!presetId) return null;
  if (presetId === IMPLICIT_CONVERSATION_PRESET_ID) {
    return implicitConversationPreset(tools, resolvedLanguage);
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
    globalMemoryEnabled: snapshot.globalMemoryEnabled === true,
    projectMemoryEnabled: snapshot.projectMemoryEnabled === true,
    skillToolEnabled: snapshot.skillToolEnabled === true,
    mcpToolDiscoveryEnabled: snapshot.mcpToolDiscoveryEnabled === true,
    decisionParameterModes: normalizeDecisionParameterModes(snapshot.decisionParameterModes),
    decisionMissScoring: normalizeDecisionMissScoring(snapshot.decisionMissScoring),
    ...sandboxField(snapshot.sandbox)
    // `toolLock` is deliberately absent: the new conversation has run nothing
    // yet, so it has exposed nothing and every setting is still free to move.
    // `rememberedDecisionForms` and `rememberedToolFamilies` are absent too:
    // the choices a conversation kept for the tools it switched off belong to
    // that conversation.
  };
}

/** Applies a preset as a complete overlay for preset-owned fields. Conversation-
 * specific reasoning effort and app-data path remain unchanged; host-derived
 * memory tools are excluded because layer switches derive them. Tool-bearing
 * fields keep the locked floor underneath: an overlay may widen what the model
 * can call but never takes back what a run already showed it, and it cannot
 * move a pin — a preset naming another search backend applies everything else
 * and leaves that one field where the transcript put it. */
export function applyConversationPresetSettings(
  current: ConversationSettings,
  preset: ConversationPresetSettings,
  knownToolNames?: ReadonlySet<string>
): ConversationSettings {
  const lock = toolLockOf(current);
  const webSearch = copyWebSearchSettings(preset.webSearch);
  const enabledTools = Array.from(new Set([...lock.tools, ...preset.enabledTools])).filter(
    (name) => (!knownToolNames || knownToolNames.has(name)) && !isHostDerivedToolName(name)
  );
  /* A form widens over what the transcript holds rather than replacing it:
     a preset asking for `replace` on a tool whose selector calls already went
     out gets `augment`, which accepts both. */
  const decisionParameterModes = flooredDecisionParameterModes(
    lock,
    normalizeDecisionParameterModes(preset.decisionParameterModes),
    enabledTools
  );
  return {
    ...current,
    enabledTools,
    decisionParameterModes,
    /* Miss scoring changes no schema, so the preset's answer simply applies,
       kept to the tools that come out with a form. */
    decisionMissScoring: decisionMissScoringFor(
      normalizeDecisionMissScoring(preset.decisionMissScoring),
      decisionParameterModes,
      enabledTools
    ),
    toolDescriptionFileId: preset.toolDescriptionFileId,
    agentDefinitions: copyAgentDefinitions(preset.agentDefinitions),
    allowRolelessSubagents: preset.allowRolelessSubagents === true,
    hookIds: [...new Set(preset.hookIds)],
    skillIds: [...new Set([...lock.skillIds, ...preset.skillIds])],
    mcpIds: [...new Set([...lock.mcpIds, ...preset.mcpIds])],
    webSearch: {
      ...webSearch,
      provider: lock.searchProvider ?? webSearch.provider,
      fetchProvider: lock.fetchProvider ?? webSearch.fetchProvider
    },
    webSearchEnabled: preset.webSearchEnabled === true || lock.webSearch,
    securityLevel: preset.securityLevel,
    globalMemoryEnabled: preset.globalMemoryEnabled === true || lock.globalMemory,
    projectMemoryEnabled: preset.projectMemoryEnabled === true || lock.projectMemory,
    /* Once skills have gone out, how they go out is settled: the preset's own
       answer would either repeat what the transcript holds or point at a tool
       the earlier rounds never had. */
    skillToolEnabled: lock.skillIds.length
      ? lock.skillTool
      : preset.skillToolEnabled === true || lock.skillTool,
    /* Same settlement over MCP: once this conversation's servers have been
       dialed, whether their schemas went on the wire is a fact about the
       transcript, not a preference a preset may restate. */
    mcpToolDiscoveryEnabled: lock.mcpIds.length
      ? lock.mcpToolDiscovery
      : preset.mcpToolDiscoveryEnabled === true || lock.mcpToolDiscovery,
    /* No lock reaches the sandbox: it confines what later commands can do and
       puts nothing in front of the model, so the preset's answer applies as it
       is — including none, which is off. */
    sandbox: preset.sandbox ? copySandboxSettings(preset.sandbox) : undefined
  };
}
