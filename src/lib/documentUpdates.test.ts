import { describe, expect, it } from "vitest";
import { createTestDocument } from "../test/fixtures";
import {
  applyGlobalSettingsChange,
  applyQuarantinedContextReplacements,
  conversationMatchesPersistenceGeneration,
  replaceConversationFromAuthority
} from "./documentUpdates";
import type { AppDocument, ContextItem, ConversationPreset } from "../types";

function presetFixture(id: string, name: string): ConversationPreset {
  const document = createTestDocument();
  return { ...document.globalSettings.conversationPresets[0], id, name };
}

describe("applyGlobalSettingsChange", () => {
  it("evaluates functional changes against the document it is given, so chained updates compound", () => {
    const base = createTestDocument();
    const first = applyGlobalSettingsChange(base, (current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, presetFixture("preset_a", "A")]
    }));
    const second = applyGlobalSettingsChange(first, (current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, presetFixture("preset_b", "B")]
    }));

    const ids = second.globalSettings.conversationPresets.map((preset) => preset.id);
    expect(ids).toContain("preset_a");
    expect(ids).toContain("preset_b");
    expect(second.globalSettings.conversationPresets.length).toBe(
      base.globalSettings.conversationPresets.length + 2
    );
  });

  it("falls back to the first preset when the default preset disappears", () => {
    const base = createTestDocument();
    const survivor = presetFixture("preset_survivor", "幸存者");
    const next = applyGlobalSettingsChange(base, (current) => ({
      ...current,
      conversationPresets: [survivor],
      defaultConversationPresetId: "preset_deleted"
    }));
    expect(next.globalSettings.defaultConversationPresetId).toBe("preset_survivor");
  });

  it("clears the default preset id when no presets remain", () => {
    const base = createTestDocument();
    const next = applyGlobalSettingsChange(base, (current) => ({
      ...current,
      conversationPresets: []
    }));
    expect(next.globalSettings.defaultConversationPresetId).toBe("");
  });

  it("keeps a still-valid default preset id untouched on wholesale replacement", () => {
    const base = createTestDocument();
    const replacement = {
      ...base.globalSettings,
      lastReasoningEffort: base.globalSettings.lastReasoningEffort
    };
    const next = applyGlobalSettingsChange(base, replacement);
    expect(next.globalSettings.defaultConversationPresetId).toBe(
      base.globalSettings.defaultConversationPresetId
    );
    expect(next.workspaces).toBe(base.workspaces);
  });
});

describe("applyQuarantinedContextReplacements", () => {
  function toolContext(id: string, nested?: ContextItem[]): ContextItem {
    return {
      id,
      kind: "tool",
      toolName: "read",
      input: {},
      result: { success: true, output: "", executedAt: "2026-08-09T00:00:00Z", durationMs: 1 },
      createdAt: "2026-08-09T00:00:00Z",
      ...(nested
        ? { subagent: { task: "child", status: "completed" as const, contexts: nested, updates: [] } }
        : {})
    };
  }

  function quarantine(document: AppDocument, contextId: string) {
    const replacement: ContextItem = {
      id: contextId,
      kind: "system",
      content: "quarantined",
      localOnly: true,
      createdAt: "2026-08-09T00:00:00Z"
    };
    return {
      conversationId: document.workspaces[0].conversations[0].id,
      contextId,
      replacement
    };
  }

  function documentWithContexts(contexts: ContextItem[]): AppDocument {
    const base = createTestDocument();
    return {
      ...base,
      workspaces: base.workspaces.map((workspace, index) =>
        index === 0
          ? {
              ...workspace,
              conversations: workspace.conversations.map((conversation, position) =>
                position === 0 ? { ...conversation, contexts } : conversation
              )
            }
          : workspace
      )
    };
  }

  it("installs the committed marker and keeps everything else", () => {
    const kept: ContextItem = {
      id: "ctx_keep",
      kind: "user",
      content: "unrelated",
      createdAt: "2026-08-09T00:00:00Z"
    };
    const document = documentWithContexts([toolContext("ctx_drop"), kept]);

    const quarantined = quarantine(document, "ctx_drop");
    const next = applyQuarantinedContextReplacements(document, [quarantined]);

    expect(next.workspaces[0].conversations[0].contexts).toEqual([quarantined.replacement, kept]);
  });

  it("reaches contexts nested inside a subagent transcript", () => {
    const document = documentWithContexts([toolContext("ctx_outer", [toolContext("ctx_child")])]);

    const quarantined = quarantine(document, "ctx_child");
    const next = applyQuarantinedContextReplacements(document, [quarantined]);

    const outer = next.workspaces[0].conversations[0].contexts[0];
    expect(outer.kind === "tool" && outer.subagent?.contexts).toEqual([quarantined.replacement]);
  });

  it("keeps the same context ID in a sibling conversation", () => {
    const document = documentWithContexts([toolContext("ctx_shared")]);
    const first = document.workspaces[0].conversations[0];
    document.workspaces[0].conversations.push({
      ...first,
      id: "conv_sibling",
      contexts: [toolContext("ctx_shared")]
    });

    const quarantined = quarantine(document, "ctx_shared");
    const next = applyQuarantinedContextReplacements(document, [quarantined]);

    expect(next.workspaces[0].conversations[0].contexts).toEqual([quarantined.replacement]);
    expect(
      next.workspaces[0].conversations.find((conversation) => conversation.id === "conv_sibling")?.contexts
    ).toHaveLength(1);
  });

  it("matches only the rejected persistence generation", () => {
    const document = documentWithContexts([toolContext("ctx_only")]);
    const conversation = document.workspaces[0].conversations[0];

    expect(conversationMatchesPersistenceGeneration(document, conversation.id, conversation.updatedAt)).toBe(
      true
    );
    expect(
      conversationMatchesPersistenceGeneration(document, conversation.id, "2026-08-24T23:59:59.999Z")
    ).toBe(false);
  });

  it("restores the authoritative workspace membership and order", () => {
    const authority = documentWithContexts([toolContext("ctx_only")]);
    const current = structuredClone(authority);
    const source = current.workspaces[0];
    const sourceId = source.id;
    current.workspaces.push({
      ...source,
      id: "ws_target",
      name: "Target",
      conversations: []
    });
    const target = current.workspaces.at(-1)!;
    const moved = source.conversations.shift()!;
    target.conversations.push(moved);

    const next = replaceConversationFromAuthority(current, authority, moved.id);

    expect(
      next.workspaces
        .find((workspace) => workspace.id === sourceId)
        ?.conversations.some((conversation) => conversation.id === moved.id)
    ).toBe(true);
    expect(
      next.workspaces
        .find((workspace) => workspace.id === "ws_target")
        ?.conversations.some((conversation) => conversation.id === moved.id)
    ).toBe(false);
  });

  it("returns the same document when nothing matches, so no save is scheduled", () => {
    const document = documentWithContexts([toolContext("ctx_only")]);

    expect(applyQuarantinedContextReplacements(document, [quarantine(document, "ctx_absent")])).toBe(
      document
    );
    expect(applyQuarantinedContextReplacements(document, [])).toBe(document);
  });
});
