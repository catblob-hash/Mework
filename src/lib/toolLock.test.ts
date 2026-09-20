import { describe, expect, it } from "vitest";
import type { ConversationSettings, ConversationToolLock } from "../types";
import { defaultConversationWebSearchSettings } from "./runtime";
import {
  EMPTY_TOOL_LOCK,
  mergeToolLock,
  sameToolLock,
  settingsAtToolLockFloor,
  toolExposureOf,
  toolLockAdditions,
  toolLockEngaged,
  toolLockOf,
  withRunToolLock
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

/** A lock as a run would have left it, so a test only states what it is about. */
function lock(patch: Partial<ConversationToolLock> = {}): ConversationToolLock {
  return { ...EMPTY_TOOL_LOCK, promptSkillIds: [], ...patch };
}

const OFFLINE = { webFetch: false };
const FETCHING = { webFetch: true };

describe("toolLock", () => {
  it("treats a conversation that has never run as unlocked", () => {
    const current = toolLockOf(settings({ enabledTools: ["read"], globalMemoryEnabled: true }));
    expect(toolLockEngaged(current)).toBe(false);
    expect(current.tools).toEqual([]);
    // Not the same as "opened its prompt with no skills": until a run says so,
    // every selected skill is still part of the prompt about to be built.
    expect(current.promptSkillIds).toBeNull();
  });

  it("merges each run's exposure and never gives any of it back", () => {
    const first = mergeToolLock(toolLockOf(settings()), toolExposureOf(settings({
      enabledTools: ["read", "write"],
      globalMemoryEnabled: true
    }), OFFLINE));
    expect(first.tools).toEqual(["read", "write"]);
    expect(first.globalMemory).toBe(true);

    // A later round that drops `write` and global memory still holds both.
    const second = mergeToolLock(first, toolExposureOf(settings({ enabledTools: ["read"] }), OFFLINE));
    expect(second.tools).toEqual(["read", "write"]);
    expect(second.globalMemory).toBe(true);
    expect(sameToolLock(first, second)).toBe(true);
  });

  it("reports only what the settings add on top of the lock", () => {
    const current = settings({
      enabledTools: ["read", "bash"],
      mcpIds: ["mcp-a"],
      projectMemoryEnabled: true,
      globalMemoryEnabled: true,
      toolLock: lock({ tools: ["read"], globalMemory: true })
    });
    const additions = toolLockAdditions(current, OFFLINE);
    expect(additions.tools).toEqual(["bash"]);
    expect(additions.mcpIds).toEqual(["mcp-a"]);
    expect(additions.projectMemory).toBe(true);
    // Already spent, so not an addition.
    expect(additions.globalMemory).toBe(false);
    expect(toolLockEngaged(additions)).toBe(true);
  });

  it("pulls settings back to exactly the locked floor", () => {
    const floored = settingsAtToolLockFloor(settings({
      enabledTools: ["read", "bash"],
      mcpIds: ["mcp-a", "mcp-b"],
      skillIds: ["skill-a", "skill-b"],
      skillToolEnabled: true,
      globalMemoryEnabled: true,
      toolLock: lock({
        tools: ["read"],
        mcpIds: ["mcp-b"],
        skillIds: ["skill-b"],
        globalMemory: true
      })
    }));
    expect(floored.enabledTools).toEqual(["read"]);
    expect(floored.mcpIds).toEqual(["mcp-b"]);
    expect(floored.skillIds).toEqual(["skill-b"]);
    expect(floored.skillToolEnabled).toBe(false);
    // The floor is not a reset: what the model already has stays on.
    expect(floored.globalMemoryEnabled).toBe(true);
    expect(toolLockEngaged(toolLockAdditions(floored, OFFLINE))).toBe(false);
  });

  it("pins the search backend on the run that grants web access, and only fetch's when it is granted", () => {
    const offline = withRunToolLock(settings({
      webSearchEnabled: true,
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        provider: { kind: "explicit", providerKind: "tavily" },
        fetchProvider: { kind: "disabled" }
      }
    }), [], OFFLINE);
    expect(toolLockOf(offline).searchProvider).toEqual({ kind: "explicit", providerKind: "tavily" });
    // No `web_fetch` went out, so nothing about fetching is spent and picking a
    // backend later is an ordinary widening.
    expect(toolLockOf(offline).fetchProvider).toBeNull();

    const withFetch = withRunToolLock(settings({
      webSearchEnabled: true,
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        fetchProvider: { kind: "native" }
      }
    }), [], FETCHING);
    expect(toolLockOf(withFetch).fetchProvider).toEqual({ kind: "native" });

    // Switching a pinned backend afterwards leaves the pin where it was.
    const moved = withRunToolLock({
      ...withFetch,
      webSearch: {
        ...withFetch.webSearch,
        provider: { kind: "explicit", providerKind: "exa" },
        fetchProvider: { kind: "explicit", providerKind: "jina" }
      }
    }, [], FETCHING);
    expect(toolLockOf(moved).searchProvider).toEqual({ kind: "native" });
    expect(toolLockOf(moved).fetchProvider).toEqual({ kind: "native" });
    expect(settingsAtToolLockFloor(moved).webSearch.fetchProvider).toEqual({ kind: "native" });
  });

  it("records the opening prompt's skills on the first run, even when there were none", () => {
    const first = withRunToolLock(settings(), ["read"], OFFLINE);
    expect(toolLockOf(first).promptSkillIds).toEqual([]);

    // Selecting one now is an addition, not part of the prompt, and delivery
    // mode is not yet settled because nothing has gone out.
    const added = withRunToolLock({ ...first, skillIds: ["skill-a"], skillToolEnabled: true }, ["read"], OFFLINE);
    expect(toolLockOf(added).promptSkillIds).toEqual([]);
    expect(toolLockOf(added).skillIds).toEqual(["skill-a"]);
    expect(toolLockOf(added).skillTool).toBe(true);

    // Now it is: a later round cannot move the same skills behind the other route.
    const flipped = withRunToolLock({ ...added, skillToolEnabled: false }, ["read"], OFFLINE);
    expect(toolLockOf(flipped).skillTool).toBe(true);
    // Skills are not a tool-surface widening — they arrive as system messages —
    // so adding one must not raise the mid-conversation exposure prompt.
    expect(toolLockEngaged(toolLockAdditions(
      { ...flipped, skillIds: ["skill-a", "skill-b"] },
      OFFLINE
    ))).toBe(false);
  });

  it("settles MCP delivery on the first run that dialed a server", () => {
    const first = withRunToolLock(
      settings({ mcpIds: ["mcp-a"], mcpToolDiscoveryEnabled: true }),
      ["read"],
      OFFLINE
    );
    expect(toolLockOf(first).mcpToolDiscovery).toBe(true);
    expect(toolLockOf(first).mcpIds).toEqual(["mcp-a"]);

    // A later round cannot move the same servers' tools onto the other route:
    // the transcript already carries them announced rather than declared.
    const flipped = withRunToolLock({ ...first, mcpToolDiscoveryEnabled: false }, ["read"], OFFLINE);
    expect(toolLockOf(flipped).mcpToolDiscovery).toBe(true);
    expect(settingsAtToolLockFloor(flipped).mcpToolDiscoveryEnabled).toBe(true);

    // Under discovery a newly selected server declares nothing — its tools are
    // announced by name — so adding one must not raise the mid-conversation
    // exposure prompt, exactly as adding a skill does not.
    const widened = { ...flipped, mcpIds: ["mcp-a", "mcp-b"] };
    expect(toolLockAdditions(widened, OFFLINE).mcpIds).toEqual([]);
    expect(toolLockEngaged(toolLockAdditions(widened, OFFLINE))).toBe(false);
    // The id still merges into the lock: it was exposed, just not as a tool.
    expect(toolLockOf(withRunToolLock(widened, ["read"], OFFLINE)).mcpIds)
      .toEqual(["mcp-a", "mcp-b"]);
  });

  it("keeps a newly selected server a tool-set widening while schemas are declared", () => {
    const declared = withRunToolLock(settings({ mcpIds: ["mcp-a"] }), ["read"], OFFLINE);
    expect(toolLockOf(declared).mcpToolDiscovery).toBe(false);
    const additions = toolLockAdditions({ ...declared, mcpIds: ["mcp-a", "mcp-b"] }, OFFLINE);
    expect(additions.mcpIds).toEqual(["mcp-b"]);
    expect(toolLockEngaged(additions)).toBe(true);
  });
});
