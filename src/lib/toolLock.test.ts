import { describe, expect, it } from "vitest";
import type { ConversationSettings, ConversationToolLock } from "../types";
import { defaultConversationWebSearchSettings } from "./runtime";
import {
  backendTone,
  DEFAULT_CACHE_TTL_MINUTES,
  EMPTY_TOOL_LOCK,
  lockTone,
  lockTouch,
  modelCacheWarmUntil,
  planModeLocked,
  refreshedToolLock,
  restoreLockedSettings,
  toolLockModelOf,
  toolLockOf,
  toolLockState,
  withRunToolLock,
  type ToolLockModel
} from "./toolLock";

function settings(patch: Partial<ConversationSettings> = {}): ConversationSettings {
  return {
    enabledTools: [],
    hookIds: [],
    skillIds: [],
    mcpIds: [],
    toolDescriptionFileId: null,
    agentDefinitions: [],
    allowRolelessSubagents: false,
    webSearch: defaultConversationWebSearchSettings(),
    webSearchEnabled: false,
    reasoningEffort: "medium",
    securityLevel: "request_approval",
    globalMemoryEnabled: false,
    projectMemoryEnabled: false,
    skillToolEnabled: false,
    mcpToolDiscoveryEnabled: false,
    ...patch
  };
}

const SENT_AT = "2026-09-30T10:00:00.000Z";
const SENT = Date.parse(SENT_AT);
const MINUTE = 60_000;

/** A model that appends tools, and one whose protocol cannot. */
const OPUS: ToolLockModel = { providerId: "anthropic", modelId: "claude-opus-5-5", family: "anthropic", appendsTools: true };
const HAIKU: ToolLockModel = { providerId: "anthropic", modelId: "claude-haiku-4-5", family: "anthropic", appendsTools: false };
const GPT: ToolLockModel = { providerId: "openai", modelId: "gpt-5.5", family: "openai_responses", appendsTools: true };

/** A lock as a request by `model` would have left it, so a test only states what it is about. */
function lock(model: ToolLockModel, patch: Partial<ConversationToolLock> = {}): ConversationToolLock {
  return {
    ...EMPTY_TOOL_LOCK,
    promptSkillIds: [],
    lastRequest: { providerId: model.providerId, modelId: model.modelId, at: SENT_AT },
    ...patch
  };
}

const OFFLINE = { webFetch: false };
const FETCHING = { webFetch: true };
const request = (model: ToolLockModel, context = OFFLINE) => ({
  ...context,
  providerId: model.providerId,
  modelId: model.modelId,
  at: SENT_AT
});

describe("the lock a request leaves", () => {
  it("is empty and unengaged for a conversation that has never run", () => {
    const current = settings({ enabledTools: ["read"], globalMemoryEnabled: true });
    expect(toolLockOf(current).tools).toEqual([]);
    expect(toolLockOf(current).lastRequest).toBeNull();
    // Not the same as "opened its prompt with no skills": until a run says so,
    // every selected skill is still part of the prompt about to be built.
    expect(toolLockOf(current).promptSkillIds).toBeNull();
    expect(toolLockState(current, OPUS, SENT).engaged).toBe(false);
  });

  it("records each request's own surface and model, replacing the last one's", () => {
    const first = withRunToolLock(
      settings({ enabledTools: ["read", "write"], globalMemoryEnabled: true }),
      ["read", "write"],
      request(OPUS)
    );
    expect(toolLockOf(first).tools).toEqual(["read", "write"]);
    expect(toolLockOf(first).lastRequest).toEqual({ providerId: "anthropic", modelId: "claude-opus-5-5", at: SENT_AT });

    // The next request went out without `write` and without global memory, on
    // another model: that is what its cache holds now.
    const second = withRunToolLock({ ...first, globalMemoryEnabled: false }, ["read"], request(GPT));
    expect(toolLockOf(second).tools).toEqual(["read"]);
    expect(toolLockOf(second).globalMemory).toBe(false);
    expect(toolLockOf(second).lastRequest?.modelId).toBe("gpt-5.5");

    // Nothing moved: the same object, so the caller skips the write.
    expect(withRunToolLock(second, ["read"], request(GPT))).toBe(second);
  });

  it("keeps the plan pair once a request offered it, as the host does", () => {
    const planning = withRunToolLock(settings({ planModeEnabled: true }), [], request(OPUS));
    expect(toolLockOf(planning).planMode).toBe(true);
    // The plan was approved and the switch went off; the pair did not leave.
    const approved = withRunToolLock({ ...planning, planModeEnabled: false }, [], request(OPUS));
    expect(toolLockOf(approved).planMode).toBe(true);
  });

  it("holds the plan-mode switch only on a frozen surface that never had the pair", () => {
    const never = settings({ toolLock: lock(HAIKU) });
    expect(planModeLocked(toolLockState(never, HAIKU, SENT))).toBe(true);
    // Another model, or one that can take the pair mid-conversation: free.
    expect(planModeLocked(toolLockState(never, OPUS, SENT))).toBe(false);
    const opus = settings({ toolLock: lock(OPUS) });
    expect(planModeLocked(toolLockState(opus, OPUS, SENT))).toBe(false);
    // The pair went out once: switching plan mode moves no tool any more.
    const had = settings({ toolLock: lock(HAIKU, { planMode: true }) });
    expect(planModeLocked(toolLockState(had, HAIKU, SENT))).toBe(false);
  });

  it("records a host-run backend as the surface, and pins only a native one that went out", () => {
    const hostRun = withRunToolLock(settings({
      webSearchEnabled: true,
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        provider: { kind: "explicit", providerKind: "tavily" },
        fetchProvider: { kind: "disabled" }
      }
    }), [], request(OPUS));
    // Its results are ordinary tool output any backend can follow: nothing to pin.
    expect(toolLockOf(hostRun).searchProvider).toBeNull();
    expect(toolLockOf(hostRun).searchBackend).toEqual({ kind: "explicit", providerKind: "tavily" });
    expect(toolLockOf(hostRun).fetchBackend).toEqual({ kind: "disabled" });
    expect(toolLockOf(hostRun).webFetch).toBe(false);
    // No `web_fetch` went out, so nothing about fetching is settled yet.
    expect(toolLockOf(hostRun).fetchProvider).toBeNull();

    const native = withRunToolLock(settings({
      webSearchEnabled: true,
      webSearch: { ...defaultConversationWebSearchSettings(), provider: { kind: "native" }, fetchProvider: { kind: "native" } }
    }), [], request(OPUS, FETCHING));
    expect(toolLockOf(native).searchProvider).toEqual({ kind: "native" });
    expect(toolLockOf(native).fetchProvider).toEqual({ kind: "native" });
    expect(toolLockOf(native).webFetch).toBe(true);

    // Native fetch on a family that folds retrieval into search grants no
    // `web_fetch`, so it seals nothing and pins nothing.
    const folded = withRunToolLock(settings({
      webSearchEnabled: true,
      webSearch: { ...defaultConversationWebSearchSettings(), fetchProvider: { kind: "native" } }
    }), [], request(GPT));
    expect(toolLockOf(folded).fetchProvider).toBeNull();

    // Switching backends afterwards leaves a native pin where it was, while the
    // surface follows the request.
    const moved = withRunToolLock({
      ...native,
      webSearch: {
        ...native.webSearch,
        provider: { kind: "explicit", providerKind: "exa" },
        fetchProvider: { kind: "explicit", providerKind: "jina" }
      }
    }, [], request(OPUS, FETCHING));
    expect(toolLockOf(moved).searchProvider).toEqual({ kind: "native" });
    expect(toolLockOf(moved).fetchProvider).toEqual({ kind: "native" });
    expect(toolLockOf(moved).searchBackend).toEqual({ kind: "explicit", providerKind: "exa" });

    // A host-run backend used first does not stop native from pinning later.
    const thenNative = withRunToolLock({
      ...hostRun,
      webSearch: { ...hostRun.webSearch, provider: { kind: "native" } }
    }, [], request(OPUS));
    expect(toolLockOf(thenNative).searchProvider).toEqual({ kind: "native" });
  });

  it("reads a host-run pin written by an older build as no pin", () => {
    const older = settings({
      toolLock: lock(OPUS, {
        searchProvider: { kind: "explicit", providerKind: "tavily" },
        fetchProvider: { kind: "explicit", providerKind: "jina" }
      })
    });
    expect(toolLockOf(older).searchProvider).toBeNull();
    expect(toolLockOf(older).fetchProvider).toBeNull();
  });

  it("records the opening prompt's skills on the first run, even when there were none", () => {
    const first = withRunToolLock(settings(), ["read"], request(OPUS));
    expect(toolLockOf(first).promptSkillIds).toEqual([]);
    const added = withRunToolLock({ ...first, skillIds: ["skill-a"] }, ["read"], request(OPUS));
    expect(toolLockOf(added).promptSkillIds).toEqual([]);
    expect(toolLockOf(added).skillIds).toEqual(["skill-a"]);
  });
});

describe("where the lock stands for the selected model", () => {
  it("engages only for the model the last request used", () => {
    const current = settings({ toolLock: lock(OPUS, { tools: ["read"] }) });
    expect(toolLockState(current, OPUS, SENT).engaged).toBe(true);
    expect(toolLockState(current, GPT, SENT).engaged).toBe(false);
    expect(toolLockState(current, { ...OPUS, providerId: "relay" }, SENT).engaged).toBe(false);
    expect(toolLockState(current, null, SENT).engaged).toBe(false);
  });

  it("keeps the cache warm for the model's lifetime, thirty minutes by default", () => {
    const current = settings({ toolLock: lock(OPUS) });
    expect(DEFAULT_CACHE_TTL_MINUTES).toBe(30);
    expect(toolLockState(current, OPUS, SENT + 29 * MINUTE).warm).toBe(true);
    expect(toolLockState(current, OPUS, SENT + 30 * MINUTE).warm).toBe(false);
    expect(toolLockState(current, OPUS, SENT).warmUntil).toBe(SENT + 30 * MINUTE);

    const hourly = { ...OPUS, cacheTtlMinutes: 60 };
    expect(toolLockState(current, hourly, SENT + 45 * MINUTE).warm).toBe(true);
    expect(toolLockState(current, { ...OPUS, cacheTtlMinutes: 5 }, SENT + 6 * MINUTE).warm).toBe(false);
  });

  it("freezes the surface of a model that cannot append tools, cache or no cache", () => {
    const current = settings({ toolLock: lock(HAIKU) });
    const cold = toolLockState(current, HAIKU, SENT + 2 * 60 * MINUTE);
    expect(cold.hard).toBe(true);
    expect(cold.warm).toBe(false);
    expect(toolLockState(settings({ toolLock: lock(OPUS) }), OPUS, SENT).hard).toBe(false);
  });

  it("builds the model's view from the provider and profile, or none", () => {
    expect(toolLockModelOf({ id: "p", family: "anthropic" }, { id: "m", cacheTtlMinutes: 5 }))
      .toEqual({ providerId: "p", modelId: "m", family: "anthropic", appendsTools: false, cacheTtlMinutes: 5 });
    // Whether the surface freezes is the model's declared capability, read
    // only where the protocol has an append interface.
    expect(toolLockModelOf({ id: "p", family: "anthropic" }, { id: "m", capabilities: ["tool_append"] })?.appendsTools)
      .toBe(true);
    expect(toolLockModelOf({ id: "p", family: "openai_compatible" }, { id: "m", capabilities: ["tool_append"] })?.appendsTools)
      .toBe(false);
    expect(toolLockModelOf(undefined, { id: "m" })).toBeNull();
    expect(toolLockModelOf({ id: "p", family: "anthropic" }, undefined)).toBeNull();
  });
});

describe("how a setting is drawn", () => {
  const warm = toolLockState(settings({ toolLock: lock(OPUS) }), OPUS, SENT);
  const cold = toolLockState(settings({ toolLock: lock(OPUS) }), OPUS, SENT + 31 * MINUTE);
  const frozen = toolLockState(settings({ toolLock: lock(HAIKU) }), HAIKU, SENT);

  it("draws a tool the last request carried orange while the cache is warm", () => {
    expect(lockTone(warm, "tool", true, true)).toBe("cache");
    // Adding a tool appends it at the end: nothing cached is lost.
    expect(lockTone(warm, "tool", false, false)).toBeNull();
    // Moved away, the cache is already lost for it; moved back, orange again.
    expect(lockTone(warm, "tool", true, false)).toBeNull();
    expect(lockTone(cold, "tool", true, true)).toBeNull();
  });

  it("draws the settings that rewrite the prefix either way orange whichever way they stand", () => {
    for (const kind of ["mcp", "memory", "skillTool", "discovery"] as const) {
      expect(lockTone(warm, kind, false, false)).toBe("cache");
      expect(lockTone(warm, kind, true, true)).toBe("cache");
      expect(lockTone(warm, kind, false, true)).toBeNull();
    }
  });

  it("draws everything but skills gray on a frozen surface", () => {
    for (const kind of ["tool", "webSearch", "mcp", "memory", "skillTool", "discovery"] as const) {
      expect(lockTone(frozen, kind, false, false)).toBe("hard");
      expect(lockTone(frozen, kind, true, false)).toBe("hard");
    }
    // Skills are not tools: a frozen surface still takes them, and only the
    // cache speaks for the ones already sent.
    expect(lockTone(frozen, "skill", false, false)).toBeNull();
    expect(lockTone(frozen, "skill", true, true)).toBe("cache");
  });
});

describe("putting the lock back when its model is picked again", () => {
  it("returns what the cache holds and keeps what was added for free", () => {
    const lastSent = settings({
      enabledTools: ["write", "bash"],
      mcpIds: ["mcp-b"],
      globalMemoryEnabled: false,
      toolLock: lock(OPUS, { tools: ["read", "write"], mcpIds: ["mcp-a"], globalMemory: true, skillIds: ["s"] })
    });
    const restored = restoreLockedSettings(lastSent, toolLockState(lastSent, OPUS, SENT));
    expect(restored.enabledTools).toEqual(["write", "bash", "read"]);
    expect(restored.mcpIds).toEqual(["mcp-a"]);
    expect(restored.globalMemoryEnabled).toBe(true);
    expect(restored.skillIds).toEqual(["s"]);
  });

  it("puts a frozen surface back exactly, and leaves skills alone once the cache is cold", () => {
    const moved = settings({
      enabledTools: ["read", "bash"],
      skillIds: ["t"],
      planModeEnabled: true,
      toolLock: lock(HAIKU, { tools: ["read", "write"], skillIds: ["s"] })
    });
    const restored = restoreLockedSettings(moved, toolLockState(moved, HAIKU, SENT + 60 * MINUTE));
    expect(restored.enabledTools).toEqual(["read", "write"]);
    // Plan mode is the composer's switch, not part of the surface: putting the
    // tools back leaves it where the user put it.
    expect(restored.planModeEnabled).toBe(true);
    expect(restored.skillIds).toEqual(["t"]);
  });

  it("moves nothing for another model, a cold cache, or settings already in place", () => {
    const moved = settings({ enabledTools: ["bash"], toolLock: lock(OPUS, { tools: ["read"] }) });
    expect(restoreLockedSettings(moved, toolLockState(moved, GPT, SENT))).toBe(moved);
    expect(restoreLockedSettings(moved, toolLockState(moved, OPUS, SENT + 31 * MINUTE))).toBe(moved);
    const inPlace = settings({ enabledTools: ["read"], toolLock: lock(OPUS, { tools: ["read"] }) });
    expect(restoreLockedSettings(inPlace, toolLockState(inPlace, OPUS, SENT))).toBe(inPlace);
  });
});

describe("what a change does to the lock", () => {
  const before = settings({
    enabledTools: ["read"],
    skillIds: ["s"],
    toolLock: lock(OPUS, { tools: ["read"], skillIds: ["s"] })
  });
  const warm = toolLockState(before, OPUS, SENT);

  it("warns when an orange setting moves, and only then", () => {
    expect(lockTouch(warm, before, { ...before, enabledTools: [] })).toEqual({ cache: true, hard: false });
    expect(lockTouch(warm, before, { ...before, enabledTools: ["read", "bash"] })).toEqual({ cache: false, hard: false });
    expect(lockTouch(warm, before, { ...before, skillIds: [] }).cache).toBe(true);
    expect(lockTouch(warm, before, { ...before, skillIds: ["s", "t"] }).cache).toBe(false);
    expect(lockTouch(warm, before, { ...before, mcpIds: ["mcp-a"] }).cache).toBe(true);
    expect(lockTouch(warm, before, { ...before, projectMemoryEnabled: true }).cache).toBe(true);
    // Settings the lock does not cover never warn.
    expect(lockTouch(warm, before, { ...before, securityLevel: "full_access" })).toEqual({ cache: false, hard: false });
  });

  it("refuses a move on a frozen surface, skills aside", () => {
    const frozenBefore = { ...before, toolLock: lock(HAIKU, { tools: ["read"] }) };
    const frozen = toolLockState(frozenBefore, HAIKU, SENT + 60 * MINUTE);
    expect(lockTouch(frozen, frozenBefore, { ...frozenBefore, enabledTools: ["read", "bash"] }).hard).toBe(true);
    // Plan mode is not a locked setting; `planModeLocked` answers for it.
    expect(lockTouch(frozen, frozenBefore, { ...frozenBefore, planModeEnabled: true })).toEqual({ cache: false, hard: false });
    expect(lockTouch(frozen, frozenBefore, { ...frozenBefore, skillIds: ["s", "t"] })).toEqual({ cache: false, hard: false });
  });

  it("says nothing once another model is selected", () => {
    const elsewhere = toolLockState(before, GPT, SENT);
    expect(lockTouch(elsewhere, before, { ...before, enabledTools: [], mcpIds: ["x"] })).toEqual({ cache: false, hard: false });
  });
});

describe("the web backends", () => {
  const TAVILY = { kind: "explicit", providerKind: "tavily" } as const;
  const EXA = { kind: "explicit", providerKind: "exa" } as const;
  const JINA = { kind: "explicit", providerKind: "jina" } as const;
  /* The last request searched with Tavily and fetched with Jina. */
  const hostRun = (model: ToolLockModel) => settings({
    webSearchEnabled: true,
    webSearch: { ...defaultConversationWebSearchSettings(), provider: TAVILY, fetchProvider: JINA },
    toolLock: lock(model, { webSearch: true, searchBackend: TAVILY, fetchBackend: JINA, webFetch: true })
  });

  it("draws a host-run backend orange while the cache is warm, and plain once it has moved", () => {
    const current = hostRun(OPUS);
    const warm = toolLockState(current, OPUS, SENT);
    expect(backendTone(warm, "search", TAVILY)).toBe("cache");
    expect(backendTone(warm, "fetch", JINA)).toBe("cache");
    expect(backendTone(warm, "search", EXA)).toBeNull();
    expect(backendTone(toolLockState(current, OPUS, SENT + 31 * MINUTE), "search", TAVILY)).toBeNull();
    // Another model's cache is not this one's.
    expect(backendTone(toolLockState(current, GPT, SENT), "search", TAVILY)).toBeNull();
  });

  it("leaves a leg plain when its tool did not go out, since giving it one is an addition", () => {
    const current = settings({
      toolLock: lock(OPUS, {
        webSearch: true,
        searchBackend: { kind: "disabled" },
        fetchBackend: { kind: "native" },
        webFetch: false
      })
    });
    const warm = toolLockState(current, OPUS, SENT);
    expect(backendTone(warm, "search", { kind: "disabled" })).toBeNull();
    expect(backendTone(warm, "fetch", { kind: "native" })).toBeNull();
  });

  it("draws a native pin gray for every model, and every backend gray on a frozen surface", () => {
    const pinned = settings({
      toolLock: lock(OPUS, { webSearch: true, searchBackend: { kind: "native" }, searchProvider: { kind: "native" } })
    });
    expect(backendTone(toolLockState(pinned, GPT, SENT + 90 * MINUTE), "search", { kind: "native" })).toBe("hard");
    expect(backendTone(toolLockState(pinned, GPT, SENT + 90 * MINUTE), "fetch", { kind: "native" })).toBeNull();
    const frozen = toolLockState(hostRun(HAIKU), HAIKU, SENT + 90 * MINUTE);
    expect(backendTone(frozen, "search", EXA)).toBe("hard");
    expect(backendTone(frozen, "fetch", JINA)).toBe("hard");
  });

  it("warns before a warm host-run backend moves, and refuses a pinned native one", () => {
    const before = hostRun(OPUS);
    const warm = toolLockState(before, OPUS, SENT);
    const moved = (patch: Partial<ConversationSettings["webSearch"]>) => ({
      ...before,
      webSearch: { ...before.webSearch, ...patch }
    });
    expect(lockTouch(warm, before, moved({ provider: EXA }))).toEqual({ cache: true, hard: false });
    expect(lockTouch(warm, before, moved({ fetchProvider: { kind: "disabled" } }))).toEqual({ cache: true, hard: false });
    // Settings beside the backends are not the lock's.
    expect(lockTouch(warm, before, moved({ maxResults: 3 }))).toEqual({ cache: false, hard: false });
    expect(lockTouch(toolLockState(before, OPUS, SENT + 31 * MINUTE), before, moved({ provider: EXA })))
      .toEqual({ cache: false, hard: false });

    const pinned = {
      ...before,
      webSearch: { ...before.webSearch, provider: { kind: "native" } as const },
      toolLock: lock(OPUS, { searchProvider: { kind: "native" } })
    };
    const elsewhere = toolLockState(pinned, GPT, SENT + 90 * MINUTE);
    expect(lockTouch(elsewhere, pinned, { ...pinned, webSearch: { ...pinned.webSearch, provider: EXA } }).hard).toBe(true);
  });

  it("puts a warm backend back when its model is picked again, and a frozen one exactly", () => {
    const moved = {
      ...hostRun(OPUS),
      webSearch: { ...hostRun(OPUS).webSearch, provider: EXA, fetchProvider: { kind: "disabled" } as const }
    };
    const restored = restoreLockedSettings(moved, toolLockState(moved, OPUS, SENT));
    expect(restored.webSearch.provider).toEqual(TAVILY);
    expect(restored.webSearch.fetchProvider).toEqual(JINA);

    // A leg that sent nothing keeps what was picked since: that was free.
    const offLeg = settings({
      webSearchEnabled: true,
      webSearch: { ...defaultConversationWebSearchSettings(), provider: EXA },
      toolLock: lock(OPUS, { webSearch: true, searchBackend: { kind: "disabled" } })
    });
    expect(restoreLockedSettings(offLeg, toolLockState(offLeg, OPUS, SENT)).webSearch.provider).toEqual(EXA);
    // Frozen, even "off" goes back: turning a leg on would add a tool.
    const frozenOff = { ...offLeg, toolLock: lock(HAIKU, { webSearch: true, searchBackend: { kind: "disabled" } }) };
    expect(restoreLockedSettings(frozenOff, toolLockState(frozenOff, HAIKU, SENT + 90 * MINUTE)).webSearch.provider)
      .toEqual({ kind: "disabled" });

    // Backends already in place move nothing.
    const inPlace = hostRun(OPUS);
    expect(restoreLockedSettings(inPlace, toolLockState(inPlace, OPUS, SENT))).toBe(inPlace);
  });
});

describe("each model's own cache", () => {
  it("keeps every model's latest request, one entry per model", () => {
    const first = withRunToolLock(settings(), [], request(OPUS));
    const later = "2026-09-30T10:10:00.000Z";
    const second = withRunToolLock(first, [], { ...request(GPT), at: later });
    expect(toolLockOf(second).lastRequest?.modelId).toBe("gpt-5.5");
    expect(toolLockOf(second).modelRequests.map((item) => item.modelId)).toEqual(["claude-opus-5-5", "gpt-5.5"]);
    const third = withRunToolLock(second, [], { ...request(OPUS), at: later });
    expect(toolLockOf(third).modelRequests).toEqual([
      { providerId: "openai", modelId: "gpt-5.5", at: later },
      { providerId: "anthropic", modelId: "claude-opus-5-5", at: later }
    ]);
  });

  it("marks each model warm for its own lifetime, whichever sent last", () => {
    const current = withRunToolLock(
      withRunToolLock(settings(), [], request(OPUS)),
      [],
      { ...request(GPT), at: new Date(SENT + 20 * MINUTE).toISOString() }
    );
    // Opus sent first and is not selected, yet its cache still holds.
    expect(modelCacheWarmUntil(current, OPUS, SENT + 25 * MINUTE)).toBe(SENT + 30 * MINUTE);
    expect(modelCacheWarmUntil(current, OPUS, SENT + 30 * MINUTE)).toBeNull();
    expect(modelCacheWarmUntil(current, GPT, SENT + 45 * MINUTE)).toBe(SENT + 50 * MINUTE);
    expect(modelCacheWarmUntil(current, { ...GPT, cacheTtlMinutes: 5 }, SENT + 26 * MINUTE)).toBeNull();
    expect(modelCacheWarmUntil(current, HAIKU, SENT)).toBeNull();
  });

  it("reads a lock from before the list existed as knowing its last request", () => {
    const older = settings({ toolLock: { ...lock(OPUS), modelRequests: undefined as never } });
    expect(modelCacheWarmUntil(older, OPUS, SENT)).toBe(SENT + 30 * MINUTE);
  });

  it("counts from the moment a long run's last request went out, for that model too", () => {
    const current = withRunToolLock(withRunToolLock(settings(), [], request(GPT)), [], request(OPUS));
    const later = "2026-09-30T10:40:00.000Z";
    const refreshed = refreshedToolLock(toolLockOf(current), later);
    expect(refreshed.lastRequest?.at).toBe(later);
    expect(refreshed.modelRequests.find((item) => item.modelId === "claude-opus-5-5")?.at).toBe(later);
    expect(refreshed.modelRequests.find((item) => item.modelId === "gpt-5.5")?.at).toBe(SENT_AT);
    expect(refreshedToolLock(EMPTY_TOOL_LOCK, later)).toBe(EMPTY_TOOL_LOCK);
  });
});
