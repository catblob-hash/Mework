import type {
  ConversationSettings,
  ConversationToolLock,
  ConversationWebSearchSettings,
  FetchProviderSelection,
  ModelCapability,
  ProviderFamily,
  SearchProviderSelection,
  ToolLockRequest
} from "../types";
import { appendsTools } from "./modelCapabilities";

/**
 * The conversation's tool lock: the tool surface its last request went out
 * with, and which model sent it when.
 *
 * The prompt cache belongs to that model and that surface. Two things follow,
 * and both hold only while the model selected now is the one the last request
 * used — picking another model lifts them, and picking this one again puts them
 * back:
 *
 * - **Cache (orange).** While the model's cache is still warm, a change that
 *   would rewrite the cached prefix is drawn orange and asks once before it is
 *   made. It is a warning, not a refusal: the change is the user's to make.
 * - **Hard (gray).** A model that cannot take a tool mid-conversation
 *   (`appendsTools`) has its whole tool surface frozen from its first request
 *   on: nothing joins and nothing leaves, whatever the cache says.
 *
 * Three fields are pins rather than parts of the surface — native search,
 * native fetch, and which skills the system prompt was built with. Each is
 * `null` until the run that sets it and never moves afterwards, whatever model
 * is selected: a second answer would contradict the transcript rather than
 * extend it. A host-run search or fetch backend is not a pin: its results are
 * ordinary tool output any backend can follow, so it is part of the surface
 * and only the cache says anything about moving it.
 *
 * Every model the conversation has sent to keeps its own cache for its own
 * lifetime (`modelRequests`), which the composer's model menu marks; the
 * surface and its two tones belong to the last request alone.
 */
export const EMPTY_TOOL_LOCK: ConversationToolLock = {
  tools: [],
  mcpIds: [],
  globalMemory: false,
  projectMemory: false,
  skillTool: false,
  mcpToolDiscovery: false,
  webSearch: false,
  planMode: false,
  skillIds: [],
  promptSkillIds: null,
  searchBackend: null,
  fetchBackend: null,
  webFetch: false,
  searchProvider: null,
  fetchProvider: null,
  lastRequest: null,
  modelRequests: []
};

/** How long a model's prompt cache is taken to stay warm when its profile says nothing. */
export const DEFAULT_CACHE_TTL_MINUTES = 30;

/**
 * What this run's settings would put in front of the model, beyond what the
 * settings alone can say.
 *
 * `webFetch` is the one piece of exposure the renderer cannot read off the
 * conversation: whether a fetch backend resolves depends on the global provider
 * catalog and on the family of the model this run uses.
 */
export interface ToolExposureContext {
  /** Whether this run grants `web_fetch` at all. */
  webFetch: boolean;
}

/** The request a run is about to send: which model, and when. */
export interface ToolLockRequestContext extends ToolExposureContext {
  providerId: string;
  modelId: string;
  /** RFC 3339. */
  at: string;
}

/** The model selected now, as the lock needs to know it. */
export interface ToolLockModel {
  providerId: string;
  modelId: string;
  family: ProviderFamily;
  /** Whether the model takes a tool mid-conversation (`appendsTools`). */
  appendsTools: boolean;
  /** Minutes the model's prompt cache is taken to stay warm. */
  cacheTtlMinutes?: number;
}

/**
 * A conversation's lock, treating the absent key of a pre-lock conversation as
 * empty.
 *
 * A backend pin written before host-run backends joined the surface may name
 * one of them; it reads as no pin, since nothing in the transcript it stands
 * for is sealed to that backend. A lock from before `modelRequests` existed
 * still knows one model's request: its last.
 */
export function toolLockOf(settings: ConversationSettings): ConversationToolLock {
  const lock = settings.toolLock;
  if (!lock) return EMPTY_TOOL_LOCK;
  const lastRequest = lock.lastRequest ?? null;
  return {
    tools: lock.tools ?? [],
    mcpIds: lock.mcpIds ?? [],
    globalMemory: lock.globalMemory === true,
    projectMemory: lock.projectMemory === true,
    skillTool: lock.skillTool === true,
    mcpToolDiscovery: lock.mcpToolDiscovery === true,
    webSearch: lock.webSearch === true,
    planMode: lock.planMode === true,
    skillIds: lock.skillIds ?? [],
    promptSkillIds: lock.promptSkillIds ?? null,
    searchBackend: lock.searchBackend ?? null,
    fetchBackend: lock.fetchBackend ?? null,
    webFetch: lock.webFetch === true,
    searchProvider: nativePin(lock.searchProvider),
    fetchProvider: nativePin(lock.fetchProvider),
    lastRequest,
    modelRequests: lock.modelRequests?.length
      ? lock.modelRequests
      : lastRequest ? [lastRequest] : []
  };
}

/** A backend selection as a pin: only native seals anything into the transcript. */
function nativePin<T extends { kind: string }>(selection: T | null | undefined): T | null {
  return selection?.kind === "native" ? selection : null;
}

/** The exposure these settings ask for, in the same shape as the lock. */
function toolExposureOf(
  settings: ConversationSettings,
  context: ToolExposureContext
): ConversationToolLock {
  const web = settings.webSearchEnabled === true;
  const searching = web && settings.webSearch.provider.kind !== "disabled";
  const fetching = web && context.webFetch;
  return {
    tools: settings.enabledTools,
    mcpIds: settings.mcpIds,
    globalMemory: settings.globalMemoryEnabled === true,
    projectMemory: settings.projectMemoryEnabled === true,
    skillTool: settings.skillToolEnabled === true,
    mcpToolDiscovery: settings.mcpToolDiscoveryEnabled === true,
    webSearch: web,
    planMode: settings.planModeEnabled === true,
    skillIds: settings.skillIds,
    /* What the system prompt would be built from if this were the first run.
       The merge below keeps whatever the first run actually used. */
    promptSkillIds: settings.skillIds,
    /* Both selections are recorded whether or not their tool went out: a
       frozen surface is put back from them, and "off" is part of it. */
    searchBackend: web ? settings.webSearch.provider : null,
    fetchBackend: web ? settings.webSearch.fetchProvider : null,
    webFetch: fetching,
    /* A pin records a native backend a run actually used, so a selection that
       sends no tool pins nothing: "this conversation does not search" leaves no
       provider-sealed residue in the transcript to be bound by. */
    searchProvider: searching ? nativePin(settings.webSearch.provider) : null,
    fetchProvider: fetching ? nativePin(settings.webSearch.fetchProvider) : null,
    lastRequest: null,
    modelRequests: []
  };
}

/**
 * The lock a request leaves behind: its own surface and model replace the
 * last one's, each pin keeps the answer the first run that set it gave, and
 * the request becomes its model's latest.
 */
function nextToolLock(
  lock: ConversationToolLock,
  exposure: ConversationToolLock,
  request: { providerId: string; modelId: string; at: string }
): ConversationToolLock {
  const lastRequest: ToolLockRequest = { providerId: request.providerId, modelId: request.modelId, at: request.at };
  return {
    tools: [...new Set(exposure.tools)],
    mcpIds: [...new Set(exposure.mcpIds)],
    globalMemory: exposure.globalMemory,
    projectMemory: exposure.projectMemory,
    skillTool: exposure.skillTool,
    mcpToolDiscovery: exposure.mcpToolDiscovery,
    webSearch: exposure.webSearch,
    /* Sticky, as the host's plan pair is: once offered it stays offered,
       whatever the switch says now. */
    planMode: lock.planMode || exposure.planMode,
    skillIds: [...new Set(exposure.skillIds)],
    promptSkillIds: lock.promptSkillIds ?? exposure.promptSkillIds,
    searchBackend: exposure.searchBackend,
    fetchBackend: exposure.fetchBackend,
    webFetch: exposure.webFetch,
    searchProvider: lock.searchProvider ?? exposure.searchProvider,
    fetchProvider: lock.fetchProvider ?? exposure.fetchProvider,
    lastRequest,
    modelRequests: withModelRequest(lock.modelRequests, lastRequest)
  };
}

/** The list with `request` as its model's latest, in place of any earlier one. */
function withModelRequest(requests: readonly ToolLockRequest[], request: ToolLockRequest): ToolLockRequest[] {
  return [
    ...requests.filter((item) => !sameModel(item, request)),
    request
  ];
}

function sameModel(
  left: { providerId: string; modelId: string },
  right: { providerId: string; modelId: string }
): boolean {
  return left.providerId === right.providerId && left.modelId === right.modelId;
}

/**
 * The lock with its last request moved to `at` — the moment a long run's final
 * request actually went out, which is what the cache is counted from. Returns
 * the argument when there is no last request to move.
 */
export function refreshedToolLock(lock: ConversationToolLock, at: string): ConversationToolLock {
  const last = lock.lastRequest;
  if (!last) return lock;
  const lastRequest = { ...last, at };
  return {
    ...lock,
    lastRequest,
    modelRequests: withModelRequest(lock.modelRequests ?? [], lastRequest)
  };
}

/** Guards the per-run write so an unchanged lock does not rewrite the conversation. */
function sameToolLock(left: ConversationToolLock, right: ConversationToolLock): boolean {
  return sameIdSet(left.tools, right.tools)
    && sameIdSet(left.mcpIds, right.mcpIds)
    && left.globalMemory === right.globalMemory
    && left.projectMemory === right.projectMemory
    && left.skillTool === right.skillTool
    && left.mcpToolDiscovery === right.mcpToolDiscovery
    && left.webSearch === right.webSearch
    && left.planMode === right.planMode
    && sameIdSet(left.skillIds, right.skillIds)
    && sameIdList(left.promptSkillIds, right.promptSkillIds)
    && sameSelection(left.searchBackend, right.searchBackend)
    && sameSelection(left.fetchBackend, right.fetchBackend)
    && left.webFetch === right.webFetch
    && sameSelection(left.searchProvider, right.searchProvider)
    && sameSelection(left.fetchProvider, right.fetchProvider)
    && sameRequest(left.lastRequest, right.lastRequest)
    && left.modelRequests.length === right.modelRequests.length
    && left.modelRequests.every((item) => right.modelRequests.some((other) => sameRequest(item, other)));
}

function sameRequest(left: ToolLockRequest | null, right: ToolLockRequest | null): boolean {
  if (left === null || right === null) return left === right;
  return sameModel(left, right) && left.at === right.at;
}

function sameIdSet(left: readonly string[], right: readonly string[]): boolean {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((id) => b.has(id));
}

/** Order matters for neither pin, but "set at all" does. */
function sameIdList(left: string[] | null, right: string[] | null): boolean {
  if (left === null || right === null) return left === right;
  return sameIdSet(left, right);
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

/**
 * The settings a request leaves behind: its exposure recorded as the lock.
 * Returns the argument unchanged when nothing moved, so a caller can skip the
 * write rather than dirty the document once per round.
 */
export function withRunToolLock(
  settings: ConversationSettings,
  tools: string[],
  context: ToolLockRequestContext
): ConversationSettings {
  const current = toolLockOf(settings);
  const next = nextToolLock(
    current,
    { ...toolExposureOf(settings, context), tools },
    context
  );
  if (settings.toolLock && sameToolLock(current, next)) return settings;
  return { ...settings, toolLock: next };
}

// ---------------------------------------------------------------- Lock state

/** Where a conversation's lock stands for the model selected now. */
export interface ToolLockState {
  lock: ConversationToolLock;
  /** The selected model is the one the last request used. */
  engaged: boolean;
  /** …and it cannot take a tool mid-conversation: the whole tool surface is frozen. */
  hard: boolean;
  /** …and its prompt cache is still warm: changes that would rewrite it are orange. */
  warm: boolean;
  /** When `warm` runs out, in epoch milliseconds, so a view can redraw then. */
  warmUntil: number | null;
}

export function toolLockState(
  settings: ConversationSettings,
  model: ToolLockModel | null,
  now: number
): ToolLockState {
  const lock = toolLockOf(settings);
  const last = lock.lastRequest;
  const engaged = Boolean(model && last
    && last.providerId === model.providerId
    && last.modelId === model.modelId);
  if (!engaged || !model || !last) {
    return { lock, engaged: false, hard: false, warm: false, warmUntil: null };
  }
  const warmUntil = cacheExpiry(last, model);
  return {
    lock,
    engaged: true,
    hard: !model.appendsTools,
    warm: warmUntil !== null && now < warmUntil,
    warmUntil
  };
}

/** When a request's cache runs out on `model`, in epoch milliseconds, or `null` for an unreadable time. */
function cacheExpiry(request: ToolLockRequest, model: { cacheTtlMinutes?: number }): number | null {
  const sentAt = Date.parse(request.at);
  const minutes = model.cacheTtlMinutes ?? DEFAULT_CACHE_TTL_MINUTES;
  return Number.isFinite(sentAt) ? sentAt + minutes * 60_000 : null;
}

/**
 * When this conversation's cache on `model` runs out, in epoch milliseconds —
 * counted from that model's own latest request here, whether or not it was the
 * last one — or `null` when that moment has passed or the model never sent.
 * The composer's model menu marks every model this answers for.
 */
export function modelCacheWarmUntil(
  settings: ConversationSettings,
  model: { providerId: string; modelId: string; cacheTtlMinutes?: number },
  now: number
): number | null {
  const request = toolLockOf(settings).modelRequests.find((item) => sameModel(item, model));
  const until = request ? cacheExpiry(request, model) : null;
  return until !== null && now < until ? until : null;
}

/**
 * The kinds of setting the lock covers, by which way of changing them rewrites
 * the cached prefix.
 *
 * - `tool`, `webSearch`: turning one on appends tools, which every protocol
 *   with an append interface takes at the end; only turning one off rewrites
 *   the declared list.
 * - `skill`: a skill added later arrives as a host notice at the end; removing
 *   one changes the system prompt it was built into. Skills are not tools, so
 *   no model's protocol freezes them.
 * - `mcp`, `memory`, `skillTool`, `discovery`: either way rewrites the prefix —
 *   the selected servers are listed in the system prompt, a memory tier's
 *   instructions sit ahead of the history, and the two delivery switches move
 *   skills and schemas between the prompt and the tool list.
 */
export type LockedSettingKind =
  | "tool"
  | "webSearch"
  | "skill"
  | "mcp"
  | "memory"
  | "skillTool"
  | "discovery";

export type LockTone = "cache" | "hard";

const EITHER_WAY: ReadonlySet<LockedSettingKind> = new Set(["mcp", "memory", "skillTool", "discovery"]);

/**
 * How one setting is drawn: gray while the surface is frozen, orange while the
 * cache is warm and changing it would rewrite the prefix, plain otherwise.
 *
 * A setting moved away from what the last request had is plain again — the
 * cache is already lost for it — and turns orange again once it is moved back.
 */
export function lockTone(
  state: ToolLockState,
  kind: LockedSettingKind,
  lastOn: boolean,
  nowOn: boolean
): LockTone | null {
  if (state.hard && kind !== "skill") return "hard";
  if (!state.warm || nowOn !== lastOn) return null;
  return lastOn || EITHER_WAY.has(kind) ? "cache" : null;
}

/**
 * Plan mode is not a locked setting: the composer switch appends its pair once
 * and never takes it away, and its guidance is an appended system prompt, so
 * it touches the cache in neither direction.
 *
 * Whether the composer's plan-mode switch is held off: the surface is frozen
 * (this model cannot take a tool mid-conversation) and the plan pair never went
 * out, so switching plan mode on would have to add two tools. Once the pair
 * has gone out, the switch moves freely on every model — the tools stay, and
 * only the appended guidance comes and goes.
 */
export function planModeLocked(state: ToolLockState): boolean {
  return state.hard && !state.lock.planMode;
}

/** The two web legs, each with a backend selector of its own. */
export type WebBackendLeg = "search" | "fetch";

/**
 * How a web backend selector is drawn.
 *
 * Gray while its native backend is pinned — for every model, since the
 * transcript is sealed to it — and on a frozen surface, like every other row.
 * Otherwise it is a cache lock: orange while the cache is warm and the
 * selector still names the backend the last request's tool went out with. A
 * leg whose tool did not go out is plain, because giving it one is an addition.
 */
export function backendTone(
  state: ToolLockState,
  leg: WebBackendLeg,
  current: SearchProviderSelection | FetchProviderSelection
): LockTone | null {
  if (backendPinned(state.lock, leg) || state.hard) return "hard";
  if (!state.warm) return null;
  const last = leg === "search" ? state.lock.searchBackend : state.lock.fetchBackend;
  const sent = leg === "search"
    ? state.lock.webSearch && last !== null && last.kind !== "disabled"
    : state.lock.webFetch;
  return sent && sameSelection(last, current) ? "cache" : null;
}

/** Whether a leg's native backend is pinned, which no model and no cache lifts. */
export function backendPinned(lock: ConversationToolLock, leg: WebBackendLeg): boolean {
  return (leg === "search" ? lock.searchProvider : lock.fetchProvider) !== null;
}

/** The web settings with each leg named by `restore` put back to what the last request had. */
function restoredBackends(
  web: ConversationWebSearchSettings,
  lock: ConversationToolLock,
  restore: { search: boolean; fetch: boolean }
): ConversationWebSearchSettings {
  const provider = restore.search && lock.searchBackend
    && !sameSelection(lock.searchBackend, web.provider)
    ? lock.searchBackend
    : web.provider;
  const fetchProvider = restore.fetch && lock.fetchBackend
    && !sameSelection(lock.fetchBackend, web.fetchProvider)
    ? lock.fetchBackend
    : web.fetchProvider;
  return provider === web.provider && fetchProvider === web.fetchProvider
    ? web
    : { ...web, provider, fetchProvider };
}

/**
 * Puts the settings the lock holds back to what the last request had, for the
 * moment the model that sent it is selected again.
 *
 * Only what the lock actually holds moves: on a frozen surface everything but
 * skills goes back, and under a warm cache only what `lockTone` and
 * `backendTone` would draw orange — a tool the user added since stays, because
 * adding it cost nothing.
 * Returns the argument when nothing moves.
 */
export function restoreLockedSettings(
  settings: ConversationSettings,
  state: ToolLockState
): ConversationSettings {
  if (!state.engaged || (!state.hard && !state.warm)) return settings;
  const lock = state.lock;
  const union = (now: readonly string[], last: readonly string[]) => [...new Set([...now, ...last])];
  const next: ConversationSettings = state.hard
    ? {
      ...settings,
      enabledTools: [...lock.tools],
      mcpIds: [...lock.mcpIds],
      globalMemoryEnabled: lock.globalMemory,
      projectMemoryEnabled: lock.projectMemory,
      skillToolEnabled: lock.skillTool,
      mcpToolDiscoveryEnabled: lock.mcpToolDiscovery,
      webSearchEnabled: lock.webSearch,
      skillIds: state.warm ? union(settings.skillIds, lock.skillIds) : settings.skillIds,
      webSearch: restoredBackends(settings.webSearch, lock, { search: true, fetch: true })
    }
    : {
      ...settings,
      enabledTools: union(settings.enabledTools, lock.tools),
      mcpIds: [...lock.mcpIds],
      globalMemoryEnabled: lock.globalMemory,
      projectMemoryEnabled: lock.projectMemory,
      skillToolEnabled: lock.skillTool,
      mcpToolDiscoveryEnabled: lock.mcpToolDiscovery,
      webSearchEnabled: settings.webSearchEnabled === true || lock.webSearch,
      skillIds: union(settings.skillIds, lock.skillIds),
      webSearch: restoredBackends(settings.webSearch, lock, {
        search: lock.webSearch && lock.searchBackend?.kind !== "disabled",
        fetch: lock.webFetch
      })
    };
  return sameSurface(settings, next) ? settings : next;
}

function sameSurface(left: ConversationSettings, right: ConversationSettings): boolean {
  return sameIdSet(left.enabledTools, right.enabledTools)
    && sameIdSet(left.mcpIds, right.mcpIds)
    && sameIdSet(left.skillIds, right.skillIds)
    && Boolean(left.globalMemoryEnabled) === Boolean(right.globalMemoryEnabled)
    && Boolean(left.projectMemoryEnabled) === Boolean(right.projectMemoryEnabled)
    && Boolean(left.skillToolEnabled) === Boolean(right.skillToolEnabled)
    && Boolean(left.mcpToolDiscoveryEnabled) === Boolean(right.mcpToolDiscoveryEnabled)
    && Boolean(left.webSearchEnabled) === Boolean(right.webSearchEnabled)
    && left.webSearch === right.webSearch;
}

/** The lock's view of the model selected now, or `null` when none is. */
export function toolLockModelOf(
  provider: { id: string; family: ProviderFamily } | undefined,
  model: { id: string; capabilities?: readonly ModelCapability[]; cacheTtlMinutes?: number } | undefined
): ToolLockModel | null {
  if (!provider || !model) return null;
  return {
    providerId: provider.id,
    modelId: model.id,
    family: provider.family,
    appendsTools: appendsTools(provider, model),
    cacheTtlMinutes: model.cacheTtlMinutes
  };
}

/** What a change of settings would do to the lock: rewrite a warm cache, or move a frozen surface. */
export interface LockTouch {
  cache: boolean;
  hard: boolean;
}

/**
 * Whether going from `before` to `after` moves any setting `lockTone` draws
 * orange or gray in `before` — the one question the settings pane asks before
 * writing a change, so every page it draws shares one warning.
 */
export function lockTouch(
  state: ToolLockState,
  before: ConversationSettings,
  after: ConversationSettings
): LockTouch {
  const touch: LockTouch = { cache: false, hard: false };
  const note = (tone: LockTone | null) => {
    if (tone === "cache") touch.cache = true;
    if (tone === "hard") touch.hard = true;
  };
  const lists: Array<[LockedSettingKind, readonly string[], readonly string[], readonly string[]]> = [
    ["tool", state.lock.tools, before.enabledTools, after.enabledTools],
    ["mcp", state.lock.mcpIds, before.mcpIds, after.mcpIds],
    ["skill", state.lock.skillIds, before.skillIds, after.skillIds]
  ];
  for (const [kind, last, from, to] of lists) {
    const lastSet = new Set(last);
    const fromSet = new Set(from);
    const toSet = new Set(to);
    for (const id of new Set([...from, ...to])) {
      if (fromSet.has(id) === toSet.has(id)) continue;
      note(lockTone(state, kind, lastSet.has(id), fromSet.has(id)));
    }
  }
  const switches: Array<[LockedSettingKind, boolean, boolean | undefined, boolean | undefined]> = [
    ["webSearch", state.lock.webSearch, before.webSearchEnabled, after.webSearchEnabled],
    ["memory", state.lock.globalMemory, before.globalMemoryEnabled, after.globalMemoryEnabled],
    ["memory", state.lock.projectMemory, before.projectMemoryEnabled, after.projectMemoryEnabled],
    ["skillTool", state.lock.skillTool, before.skillToolEnabled, after.skillToolEnabled],
    ["discovery", state.lock.mcpToolDiscovery, before.mcpToolDiscoveryEnabled, after.mcpToolDiscoveryEnabled]
  ];
  for (const [kind, lastOn, from, to] of switches) {
    if (Boolean(from) === Boolean(to)) continue;
    note(lockTone(state, kind, lastOn, Boolean(from)));
  }
  const legs: Array<[WebBackendLeg, SearchProviderSelection | FetchProviderSelection, SearchProviderSelection | FetchProviderSelection]> = [
    ["search", before.webSearch.provider, after.webSearch.provider],
    ["fetch", before.webSearch.fetchProvider, after.webSearch.fetchProvider]
  ];
  for (const [leg, from, to] of legs) {
    if (sameSelection(from, to)) continue;
    note(backendTone(state, leg, from));
  }
  return touch;
}
