import { describe, expect, it } from "vitest";

import { createSeedDocument } from "../seed";
import {
  memoryE2EProviders,
  seedMemoryE2EDocument,
  validateMemoryE2ESeedConfig
} from "./memoryE2ESeed";

const config = {
  runId: "0123456789abcdef01234567",
  protocolBaseUrl: "http://127.0.0.1:18100/v1"
};

describe("memory browser E2E provider seed", () => {
  it("seeds exact cross-provider model identities without paths or credentials", () => {
    const original = createSeedDocument();
    const seeded = seedMemoryE2EDocument(original, config);

    expect(seeded).not.toBe(original);
    // Workspace identity (ID, name, path, and conversations) must remain intact;
    // this function only changes per-conversation security levels.
    expect(seeded.workspaces.map((workspace) => ({
      id: workspace.id,
      name: workspace.name,
      kind: workspace.kind,
      path: workspace.path,
      conversationIds: workspace.conversations.map((conversation) => conversation.id)
    }))).toEqual(original.workspaces.map((workspace) => ({
      id: workspace.id,
      name: workspace.name,
      kind: workspace.kind,
      path: workspace.path,
      conversationIds: workspace.conversations.map((conversation) => conversation.id)
    })));
    expect(seeded.workspaces.flatMap((workspace) => workspace.conversations)
      .every((conversation) => conversation.settings.securityLevel === "full_access")).toBe(true);
    // UI-created tasks must also bypass approval, so the default is full_access.
    const defaultPreset = seeded.globalSettings.conversationPresets.find(
      (preset) => preset.id === seeded.globalSettings.defaultConversationPresetId
    );
    expect(defaultPreset?.settings.securityLevel).toBe("full_access");
    expect(seeded.globalSettings.apiProviders.map((provider) => ({
      format: provider.family,
      baseUrl: provider.baseUrl,
      modelIds: provider.models.map((model) => model.id),
      active: provider.activeModelId
    }))).toEqual([
      {
        format: "openai_responses",
        baseUrl: config.protocolBaseUrl,
        modelIds: ["kimi-k3", "Kimi-K3"],
        active: "kimi-k3"
      },
      {
        format: "openai_chat",
        baseUrl: config.protocolBaseUrl,
        modelIds: ["kimi-k3"],
        active: "kimi-k3"
      },
      {
        format: "anthropic",
        baseUrl: config.protocolBaseUrl,
        modelIds: ["kimi-k3"],
        active: "kimi-k3"
      }
    ]);
    const serialized = JSON.stringify(memoryE2EProviders(config));
    // Seed data must contain no credentials or local paths; the E2E runner stores
    // encrypted values separately in OS credential storage.
    expect(serialized).not.toMatch(/"apiKey"|"key"|workspacePath|workspaceMarker/);
  });

  it("is reload-idempotent and preserves valid provider/model selection", () => {
    const first = seedMemoryE2EDocument(createSeedDocument(), config);
    first.globalSettings.activeProviderId =
      `memory-e2e-openai_responses-${config.runId}`;
    first.globalSettings.apiProviders[0].activeModelId = "Kimi-K3";
    const reloaded = seedMemoryE2EDocument(first, config);

    expect(reloaded.globalSettings.activeProviderId).toBe(
      first.globalSettings.activeProviderId
    );
    expect(reloaded.globalSettings.apiProviders[0].activeModelId).toBe("Kimi-K3");
    expect(reloaded.workspaces).toEqual(first.workspaces);
  });

  it.each([
    { ...config, runId: "forged" },
    { ...config, protocolBaseUrl: "https://127.0.0.1:18100/v1" },
    { ...config, protocolBaseUrl: "http://localhost:18100/v1" },
    { ...config, protocolBaseUrl: "http://127.0.0.1:18100/v1?path=C:%5Csecret" },
    { ...config, protocolBaseUrl: "http://127.0.0.1:80/v1" }
  ])("rejects untrusted seed config %#", (candidate) => {
    expect(() => validateMemoryE2ESeedConfig(candidate)).toThrow();
  });
});
