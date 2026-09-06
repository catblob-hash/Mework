import { describe, expect, it } from "vitest";
import type { AgentDefinition } from "../types";
import {
  buildUserAgentDefinition,
  userAgentDefinitionDraft,
  validateAgentTypeSlug
} from "./agentDefinitions";

function definition(overrides: Partial<AgentDefinition> = {}): AgentDefinition {
  return {
    enabled: true,
    deleted: false,
    name: "code-reviewer",
    description: "",
    source: "user",
    sourceKey: "",
    revision: 7,
    memoryEpoch: 12,
    modelSelection: { kind: "inherit" },
    memory: "none",
    effort: null,
    tools: null,
    disallowedTools: [],
    searchProvider: null,
    ...overrides
  };
}

describe("agentDefinitions", () => {
  it("mirrors the trusted host slug contract", () => {
    expect(validateAgentTypeSlug("reviewer")).toBeNull();
    expect(validateAgentTypeSlug("code-reviewer_2")).toBeNull();
    expect(validateAgentTypeSlug("")).toBe("required");
    expect(validateAgentTypeSlug("2reviewer")).toBe("first_character");
    expect(validateAgentTypeSlug("Reviewer")).toBe("first_character");
    expect(validateAgentTypeSlug("code/reviewer")).toBe("characters");
    expect(validateAgentTypeSlug(`a${"x".repeat(64)}`)).toBe("too_long");
  });

  it("starts at revision one and always emits enabled user roles without memory", () => {
    expect(buildUserAgentDefinition({
      name: "reviewer",
      description: "",
      modelSelection: {
        kind: "explicit",
        providerId: "provider:/精确",
        modelId: "kimi/vision:v4-模型"
      },
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null
    })).toEqual({
      enabled: true,
      deleted: false,
      name: "reviewer",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: {
        kind: "explicit",
        providerId: "provider:/精确",
        modelId: "kimi/vision:v4-模型"
      },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null
    });
  });

  it.each([
    ["model selection", {
      modelSelection: {
        kind: "explicit" as const,
        providerId: "p:/原样",
        modelId: "kimi:v4/视觉-模型"
      }
    }],
    // The execution overrides. Each is capability-bearing, so an edit that left
    // the revision alone would be a configuration change the host identity
    // ledger — which reconciles on revision — never records.
    ["effort", { effort: "low" as const }],
    ["tool allowlist", { tools: ["read_file"] }],
    ["tool deny list", { disallowedTools: ["run_command"] }],
    ["search backend", {
      searchProvider: { kind: "explicit" as const, providerKind: "tavily" as const }
    }]
  ])("increments revision for a material %s change", (_label, change) => {
    const previous = definition();
    const draft = { ...userAgentDefinitionDraft(previous), ...change };
    expect(buildUserAgentDefinition(draft, previous).revision).toBe(8);
  });

  it("round-trips the execution overrides through the draft without a revision bump", () => {
    const previous = definition({
      effort: "low",
      tools: ["read_file", "search_files"],
      disallowedTools: ["run_command"],
      searchProvider: { kind: "explicit", providerKind: "exa" }
    });
    const draft = userAgentDefinitionDraft(previous);
    expect(draft).toMatchObject({
      effort: "low",
      tools: ["read_file", "search_files"],
      disallowedTools: ["run_command"],
      searchProvider: { kind: "explicit", providerKind: "exa" }
    });
    // Copies, not aliases: mutating the draft must not reach the persisted
    // definition, or an abandoned edit would still change the saved value.
    draft.tools?.push("run_command");
    expect(previous.tools).toEqual(["read_file", "search_files"]);
    expect(draft.searchProvider).not.toBe(previous.searchProvider);

    const rebuilt = buildUserAgentDefinition(userAgentDefinitionDraft(previous), previous);
    expect(rebuilt.revision).toBe(previous.revision);
    expect(rebuilt).toMatchObject({
      enabled: true,
      memory: "none",
      effort: "low",
      tools: ["read_file", "search_files"],
      disallowedTools: ["run_command"],
      searchProvider: { kind: "explicit", providerKind: "exa" }
    });
    expect(rebuilt.searchProvider).not.toBe(previous.searchProvider);
  });

  it("canonicalizes tool lists so a reorder is not mistaken for an edit", () => {
    const previous = definition({ tools: ["a_tool", "b_tool"], disallowedTools: ["z_tool"] });
    const reordered = buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      tools: ["b_tool", "a_tool", "a_tool"]
    }, previous);
    expect(reordered.tools).toEqual(["a_tool", "b_tool"]);
    expect(reordered.revision).toBe(previous.revision);
  });

  it("keeps an empty allowlist distinct from an absent one", () => {
    const previous = definition({ tools: null });
    const emptied = buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      tools: []
    }, previous);
    expect(emptied.tools).toEqual([]);
    expect(emptied.revision).toBe(previous.revision + 1);
  });

  it("preserves revision when configuration is unchanged", () => {
    const previous = definition();
    expect(buildUserAgentDefinition(
      userAgentDefinitionDraft(previous),
      previous
    ).revision).toBe(7);
  });

  it("preserves the host epoch across edits without treating it as renderer authority", () => {
    const previous = definition({ memoryEpoch: 12 });
    expect(buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      effort: "high"
    }, previous)).toMatchObject({
      deleted: false,
      revision: 8,
      memoryEpoch: 12
    });
  });

  it("treats a rename as a new identity instead of moving memory", () => {
    const previous = definition();
    expect(buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      name: "security-reviewer"
    }, previous)).toMatchObject({
      revision: 1,
      memoryEpoch: 1
    });
  });

  it("rejects an invalid host epoch instead of silently authorizing it", () => {
    const previous = definition({ memoryEpoch: 0 });
    expect(() => buildUserAgentDefinition(
      userAgentDefinitionDraft(previous),
      previous
    )).toThrow("agent_definition_memory_epoch_invalid");
  });

  it("bumps the revision for an effort-only edit but not for a no-op save", () => {
    // `effort` is capability-bearing, so an unchanged revision would let a live
    // run keep riding a configuration the user has already replaced.
    const previous = definition({ revision: 7 });
    expect(buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      effort: "xhigh"
    }, previous)).toMatchObject({
      revision: 8,
      effort: "xhigh"
    });
    expect(buildUserAgentDefinition(
      userAgentDefinitionDraft(previous),
      previous
    ).revision).toBe(7);
  });

  it("round-trips the subagent description without bumping the revision", () => {
    // The opposite of every case in the table above, and deliberately so: the
    // description is prose the model reads when picking a name, never a
    // capability the child holds. A bump here would make the host refuse every
    // child already bound to this role — a live run killed by a reword.
    const previous = definition({ revision: 7, description: "旧说明" });
    const reworded = buildUserAgentDefinition({
      ...userAgentDefinitionDraft(previous),
      description: "新说明：\n第二行照原样保留。"
    }, previous);
    expect(reworded).toMatchObject({
      revision: 7,
      description: "新说明：\n第二行照原样保留。"
    });
    expect(userAgentDefinitionDraft(previous).description).toBe("旧说明");
  });

  it("opens the draft on an empty description for a role persisted without a description", () => {
    const legacy = definition();
    delete (legacy as Partial<AgentDefinition>).description;
    expect(userAgentDefinitionDraft(legacy).description).toBe("");
  });
});
