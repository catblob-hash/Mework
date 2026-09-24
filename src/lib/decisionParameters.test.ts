import { describe, expect, it } from "vitest";
import type { ConversationSettings, ConversationToolLock } from "../types";
import { applyConversationPresetSettings, captureConversationPresetSettings } from "./conversationPresets";
import {
  decisionFormAllowed,
  type DecisionToolSettings,
  joinDecisionForms,
  normalizeDecisionParameterModes,
  normalizeRememberedDecisionForms,
  switchToolsOff,
  switchToolsOn
} from "./decisionParameters";
import { defaultConversationWebSearchSettings } from "./runtime";
import {
  EMPTY_TOOL_LOCK,
  settingsAtToolLockFloor,
  toolLockAdditions,
  toolLockEngaged,
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

function lock(patch: Partial<ConversationToolLock> = {}): ConversationToolLock {
  return { ...EMPTY_TOOL_LOCK, promptSkillIds: [], ...patch };
}

const OFFLINE = { webFetch: false };

describe("decision parameter forms", () => {
  it("joins to the narrowest form that accepts both, like the host", () => {
    expect(joinDecisionForms(null, null)).toBeNull();
    expect(joinDecisionForms("replace", "replace")).toBe("replace");
    expect(joinDecisionForms(null, "replace")).toBe("augment");
    expect(joinDecisionForms("replace", null)).toBe("augment");
    expect(joinDecisionForms(null, "augment")).toBe("augment");
  });

  it("allows only forms that still accept what the floor covered", () => {
    expect(decisionFormAllowed(undefined, "replace")).toBe(true);
    expect(decisionFormAllowed(null, "augment")).toBe(true);
    expect(decisionFormAllowed(null, "replace")).toBe(false);
    expect(decisionFormAllowed("replace", null)).toBe(false);
    expect(decisionFormAllowed("augment", "replace")).toBe(false);
    expect(decisionFormAllowed("augment", "augment")).toBe(true);
  });

  it("keeps only known modes on decision-parameter tools", () => {
    expect(normalizeDecisionParameterModes({
      preview_click: "replace",
      preview_eval: "augment",
      preview_fill: "bogus"
    })).toEqual({ preview_click: "replace" });
    expect(normalizeDecisionParameterModes(null)).toEqual({});
  });

  it("records each exposed tool's form in the lock and widens it across runs", () => {
    const first = withRunToolLock(settings({
      enabledTools: ["preview_click", "preview_snapshot"],
      decisionParameterModes: { preview_snapshot: "replace", preview_fill: "augment" }
    }), ["preview_click", "preview_snapshot"], OFFLINE);
    expect(first.toolLock?.decisionParameterModes).toEqual({ preview_snapshot: "replace" });

    // Asking for the direct form after query-only calls went out widens instead.
    const second = withRunToolLock({
      ...first,
      decisionParameterModes: { preview_click: "replace" }
    }, ["preview_click", "preview_snapshot"], OFFLINE);
    expect(second.toolLock?.decisionParameterModes).toEqual({
      preview_click: "augment",
      preview_snapshot: "augment"
    });
  });

  it("counts a widened form on an exposed tool as an addition", () => {
    const exposed = settings({
      enabledTools: ["preview_click"],
      toolLock: lock({ tools: ["preview_click"] })
    });
    expect(toolLockEngaged(toolLockAdditions(exposed, OFFLINE))).toBe(false);
    const widened = { ...exposed, decisionParameterModes: { preview_click: "augment" as const } };
    expect(toolLockAdditions(widened, OFFLINE).decisionParameterModes).toEqual({ preview_click: "augment" });
    expect(toolLockEngaged(toolLockAdditions(widened, OFFLINE))).toBe(true);
  });

  it("pulls settings back to the forms the lock covers", () => {
    const floored = settingsAtToolLockFloor(settings({
      enabledTools: ["preview_click", "preview_fill"],
      decisionParameterModes: { preview_click: "augment", preview_fill: "replace" },
      toolLock: lock({ tools: ["preview_click"], decisionParameterModes: { preview_click: "replace" } })
    }));
    expect(floored.decisionParameterModes).toEqual({ preview_click: "replace" });
  });

  it("carries forms through presets without narrowing an exposed tool", () => {
    const body = captureConversationPresetSettings(settings({
      enabledTools: ["preview_click"],
      decisionParameterModes: { preview_click: "replace", preview_fill: "augment" }
    }));
    // A form on a tool the body does not enable says nothing, so it is not saved.
    expect(body.decisionParameterModes).toEqual({ preview_click: "replace" });

    const applied = applyConversationPresetSettings(settings({
      enabledTools: ["preview_click"],
      toolLock: lock({ tools: ["preview_click"] })
    }), body);
    // Selector calls are already in the transcript, so `replace` arrives as `augment`.
    expect(applied.decisionParameterModes).toEqual({ preview_click: "augment" });
  });
});

function toolSettings(patch: Partial<DecisionToolSettings> = {}): DecisionToolSettings {
  return {
    enabledTools: [],
    decisionParameterModes: {},
    decisionMissScoring: [],
    rememberedDecisionForms: {},
    ...patch
  };
}

describe("remembered decision forms", () => {
  it("keeps a switched-off tool's form and miss scoring, and hands them back when it comes on", () => {
    const off = switchToolsOff(toolSettings({
      enabledTools: ["preview_click", "read"],
      decisionParameterModes: { preview_click: "replace" },
      decisionMissScoring: ["preview_click"]
    }), new Set(["preview_click"]));
    expect(off).toEqual(toolSettings({
      enabledTools: ["read"],
      rememberedDecisionForms: { preview_click: { form: "replace", missScoring: true } }
    }));

    const on = switchToolsOn(off, ["preview_click"], new Set(["preview_click"]));
    expect(on).toEqual(toolSettings({
      enabledTools: ["read", "preview_click"],
      decisionParameterModes: { preview_click: "replace" },
      decisionMissScoring: ["preview_click"]
    }));
  });

  it("reads a tool-backed row's form off its two tools", () => {
    const available = new Set(["bash", "bash_find_output"]);
    for (const [enabledTools, form] of [
      [["bash", "bash_find_output"], "augment"],
      [["bash_find_output"], "replace"]
    ] as const) {
      const off = switchToolsOff(toolSettings({ enabledTools: [...enabledTools] }), new Set(enabledTools));
      expect(off.rememberedDecisionForms).toEqual({ bash: { form } });
      expect(switchToolsOn(off, ["bash"], available).enabledTools).toEqual([...enabledTools]);
    }
    // The direct form leaves nothing behind.
    expect(switchToolsOff(toolSettings({ enabledTools: ["bash"] }), new Set(["bash"])).rememberedDecisionForms)
      .toEqual({});
  });

  it("remembers nothing for a row that stays on", () => {
    // Taking the scored tool away alone is a change of form, not a switch-off.
    const next = switchToolsOff(
      toolSettings({ enabledTools: ["bash", "bash_find_output"] }),
      new Set(["bash_find_output"])
    );
    expect(next).toEqual(toolSettings({ enabledTools: ["bash"] }));
  });

  it("brings a remembered variant back as its base tool when the owner cannot offer the variant", () => {
    const next = switchToolsOn(
      toolSettings({ rememberedDecisionForms: { zsh: { form: "replace" } } }),
      ["zsh"],
      new Set(["zsh"])
    );
    expect(next).toEqual(toolSettings({ enabledTools: ["zsh"] }));
  });

  it("leaves a row that is already on in the form it has", () => {
    const next = switchToolsOn(
      toolSettings({
        enabledTools: ["preview_click"],
        rememberedDecisionForms: { preview_click: { form: "augment" } }
      }),
      ["preview_click"],
      new Set(["preview_click"])
    );
    expect(next.decisionParameterModes).toEqual({});
    expect(next.rememberedDecisionForms).toEqual({ preview_click: { form: "augment" } });
  });

  it("keeps only the entries this build understands", () => {
    expect(normalizeRememberedDecisionForms({
      preview_click: { form: "augment", missScoring: true },
      preview_snapshot: { form: "replace", missScoring: true },
      bash: { form: "replace" },
      read: { form: "augment" },
      preview_fill: { form: "sideways" },
      preview_inspect: "augment"
    })).toEqual({
      preview_click: { form: "augment", missScoring: true },
      // The snapshot scores rather than chooses, so it has no miss to score.
      preview_snapshot: { form: "replace" },
      bash: { form: "replace" }
    });
    expect(normalizeRememberedDecisionForms(["augment"])).toEqual({});
  });
});
