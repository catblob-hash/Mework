import type { ConversationSettings, ConversationToolLock } from "../types";

/**
 * Tool exposure is one-way. Every run merges what it showed the model into the
 * conversation's lock; later rounds may widen that surface but never narrow it,
 * because a transcript that already calls a tool cannot be replayed to a model
 * that no longer has it.
 *
 * The lock covers every conversation setting that changes which tools reach the
 * wire — the tool list itself, MCP servers, the two memory tiers, on-demand
 * skill loading, and web access — so the settings panel can gray out exactly
 * what is spent.
 *
 * Three of its fields are pins rather than growing sets, because the thing they
 * hold is not a set of names but a single answer that has already been acted
 * on: which backend searched, which backend fetched, and which skills the
 * system prompt was built with. A pin is `null` until the run that sets it, and
 * never moves afterwards — swapping a search backend mid-transcript would leave
 * results in the history that the new backend can neither have produced nor
 * consume.
 */
export const EMPTY_TOOL_LOCK: ConversationToolLock = {
  tools: [],
  mcpIds: [],
  globalMemory: false,
  projectMemory: false,
  skillTool: false,
  mcpToolDiscovery: false,
  webSearch: false,
  skillIds: [],
  promptSkillIds: null,
  searchProvider: null,
  fetchProvider: null
};

/**
 * What this run's settings would put in front of the model, beyond what the
 * settings alone can say.
 *
 * `webFetch` is the one piece of exposure the renderer cannot read off the
 * conversation: whether a fetch backend resolves depends on the global provider
 * catalog and on the family of the model this run uses. `webFetchGrantedBy`
 * computes it from those two.
 */
export interface ToolExposureContext {
  /** Whether this run grants `web_fetch` at all. */
  webFetch: boolean;
}

/** A conversation's lock, treating the absent key of a pre-lock conversation as empty. */
export function toolLockOf(settings: ConversationSettings): ConversationToolLock {
  const lock = settings.toolLock;
  if (!lock) return EMPTY_TOOL_LOCK;
  return {
    tools: lock.tools ?? [],
    mcpIds: lock.mcpIds ?? [],
    globalMemory: lock.globalMemory === true,
    projectMemory: lock.projectMemory === true,
    skillTool: lock.skillTool === true,
    mcpToolDiscovery: lock.mcpToolDiscovery === true,
    webSearch: lock.webSearch === true,
    skillIds: lock.skillIds ?? [],
    promptSkillIds: lock.promptSkillIds ?? null,
    searchProvider: lock.searchProvider ?? null,
    fetchProvider: lock.fetchProvider ?? null
  };
}

/** The exposure these settings ask for, in the same shape as the lock. */
export function toolExposureOf(
  settings: ConversationSettings,
  context: ToolExposureContext
): ConversationToolLock {
  const web = settings.webSearchEnabled === true;
  return {
    tools: settings.enabledTools,
    mcpIds: settings.mcpIds,
    globalMemory: settings.globalMemoryEnabled === true,
    projectMemory: settings.projectMemoryEnabled === true,
    skillTool: settings.skillToolEnabled === true,
    mcpToolDiscovery: settings.mcpToolDiscoveryEnabled === true,
    webSearch: web,
    skillIds: settings.skillIds,
    /* What the system prompt would be built from if this were the first run.
       The merge below keeps whatever the first run actually used. */
    promptSkillIds: settings.skillIds,
    /* A pin records the backend a run actually used, so a selection that sends
       no tool pins nothing: "this conversation does not search" leaves no
       provider-shaped residue in the transcript to be bound by. */
    searchProvider: web && settings.webSearch.provider.kind !== "disabled"
      ? settings.webSearch.provider
      : null,
    fetchProvider: web && context.webFetch ? settings.webSearch.fetchProvider : null
  };
}

export function mergeToolLock(
  lock: ConversationToolLock,
  exposure: ConversationToolLock
): ConversationToolLock {
  return {
    tools: [...new Set([...lock.tools, ...exposure.tools])],
    mcpIds: [...new Set([...lock.mcpIds, ...exposure.mcpIds])],
    globalMemory: lock.globalMemory || exposure.globalMemory,
    projectMemory: lock.projectMemory || exposure.projectMemory,
    /* Not a widening but a pin: once skills have been delivered one way, the
       transcript carries them in that form, and the other form would either
       repeat them or refer to a tool the earlier rounds never had. */
    skillTool: lock.skillIds.length ? lock.skillTool : exposure.skillTool,
    /* The same pin, over MCP servers instead of skills: a conversation whose
       MCP tools went out with their schemas cannot start withholding them, and
       one that announced names cannot retroactively have declared them. */
    mcpToolDiscovery: lock.mcpIds.length
      ? lock.mcpToolDiscovery
      : exposure.mcpToolDiscovery,
    webSearch: lock.webSearch || exposure.webSearch,
    skillIds: [...new Set([...lock.skillIds, ...exposure.skillIds])],
    promptSkillIds: lock.promptSkillIds ?? exposure.promptSkillIds,
    searchProvider: lock.searchProvider ?? exposure.searchProvider,
    fetchProvider: lock.fetchProvider ?? exposure.fetchProvider
  };
}

/**
 * Whether a run has already spent part of this conversation's tool surface.
 *
 * Skills are deliberately absent. A skill selected after the first run reaches
 * the model as its own system message rather than by widening the tool set or
 * rewriting the prompt, so adding one is not a change any dialect can refuse —
 * which is exactly what this predicate is asked about.
 */
export function toolLockEngaged(lock: ConversationToolLock): boolean {
  return lock.tools.length > 0
    || lock.mcpIds.length > 0
    || lock.globalMemory
    || lock.projectMemory
    || lock.skillTool
    || lock.mcpToolDiscovery
    || lock.webSearch
    || lock.searchProvider !== null
    || lock.fetchProvider !== null;
}

/** Guards the per-run merge so an unchanged lock does not rewrite the conversation. */
export function sameToolLock(left: ConversationToolLock, right: ConversationToolLock): boolean {
  return left.tools.length === right.tools.length
    && left.tools.every((name) => right.tools.includes(name))
    && left.mcpIds.length === right.mcpIds.length
    && left.mcpIds.every((id) => right.mcpIds.includes(id))
    && left.globalMemory === right.globalMemory
    && left.projectMemory === right.projectMemory
    && left.skillTool === right.skillTool
    && left.mcpToolDiscovery === right.mcpToolDiscovery
    && left.webSearch === right.webSearch
    && left.skillIds.length === right.skillIds.length
    && left.skillIds.every((id) => right.skillIds.includes(id))
    && sameIdList(left.promptSkillIds, right.promptSkillIds)
    && sameSelection(left.searchProvider, right.searchProvider)
    && sameSelection(left.fetchProvider, right.fetchProvider);
}

/** Order matters for neither pin, but "set at all" does. */
function sameIdList(left: string[] | null, right: string[] | null): boolean {
  if (left === null || right === null) return left === right;
  return left.length === right.length && left.every((id) => right.includes(id));
}

/**
 * Both selections are discriminated unions whose only other field is a provider
 * kind, so the two fields are the whole comparison. Serializing instead would
 * make key order part of the answer.
 */
function sameSelection(
  left: { kind: string; providerKind?: string } | null,
  right: { kind: string; providerKind?: string } | null
): boolean {
  if (left === null || right === null) return left === right;
  return left.kind === right.kind && left.providerKind === right.providerKind;
}

/** What the settings would add on top of what the model has already seen. */
export function toolLockAdditions(
  settings: ConversationSettings,
  context: ToolExposureContext
): ConversationToolLock {
  const lock = toolLockOf(settings);
  const exposure = toolExposureOf(settings, context);
  const lockedTools = new Set(lock.tools);
  const lockedMcpIds = new Set(lock.mcpIds);
  const lockedSkillIds = new Set(lock.skillIds);
  return {
    tools: exposure.tools.filter((name) => !lockedTools.has(name)),
    /* Under tool discovery a newly selected server declares nothing: its tools
       are announced by name and stay behind `tool_search`, so selecting one is
       no more a change to the tool set than selecting another skill is. The
       ids still merge into the lock below — they are exposure, just not
       exposure any dialect can refuse. */
    mcpIds: lock.mcpToolDiscovery
      ? []
      : exposure.mcpIds.filter((id) => !lockedMcpIds.has(id)),
    globalMemory: exposure.globalMemory && !lock.globalMemory,
    projectMemory: exposure.projectMemory && !lock.projectMemory,
    skillTool: exposure.skillTool && !lock.skillTool,
    mcpToolDiscovery: exposure.mcpToolDiscovery && !lock.mcpToolDiscovery,
    webSearch: exposure.webSearch && !lock.webSearch,
    skillIds: exposure.skillIds.filter((id) => !lockedSkillIds.has(id)),
    /* A pin is an addition only the first time it is set: changing a pinned
       backend afterwards is refused outright, not offered as a widening. */
    promptSkillIds: lock.promptSkillIds === null ? exposure.promptSkillIds : null,
    searchProvider: lock.searchProvider === null ? exposure.searchProvider : null,
    fetchProvider: lock.fetchProvider === null ? exposure.fetchProvider : null
  };
}

/** Settings pulled back to exactly the surface the lock already covers. */
export function settingsAtToolLockFloor(settings: ConversationSettings): ConversationSettings {
  const lock = toolLockOf(settings);
  const lockedTools = new Set(lock.tools);
  const lockedMcpIds = new Set(lock.mcpIds);
  const lockedSkillIds = new Set(lock.skillIds);
  return {
    ...settings,
    enabledTools: settings.enabledTools.filter((name) => lockedTools.has(name)),
    mcpIds: settings.mcpIds.filter((id) => lockedMcpIds.has(id)),
    globalMemoryEnabled: lock.globalMemory,
    projectMemoryEnabled: lock.projectMemory,
    skillToolEnabled: lock.skillTool,
    mcpToolDiscoveryEnabled: lock.mcpToolDiscovery,
    webSearchEnabled: lock.webSearch,
    skillIds: settings.skillIds.filter((id) => lockedSkillIds.has(id)),
    webSearch: {
      ...settings.webSearch,
      provider: lock.searchProvider ?? settings.webSearch.provider,
      fetchProvider: lock.fetchProvider ?? settings.webSearch.fetchProvider
    }
  };
}

/**
 * The settings a run leaves behind: its own exposure merged into the lock.
 * Returns the argument unchanged when the run added nothing, so a caller can
 * skip the write rather than dirty the document once per round.
 */
export function withRunToolLock(
  settings: ConversationSettings,
  tools: string[],
  context: ToolExposureContext
): ConversationSettings {
  const current = toolLockOf(settings);
  const merged = mergeToolLock(current, { ...toolExposureOf(settings, context), tools });
  // A conversation whose first run exposed nothing at all still has to record
  // that it ran: `promptSkillIds` is how a later skill knows it is an addition
  // rather than part of the opening prompt.
  if (settings.toolLock && sameToolLock(current, merged)) return settings;
  return { ...settings, toolLock: merged };
}
