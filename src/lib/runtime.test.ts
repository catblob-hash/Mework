import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { AgentDefinition, ApiProvider, AppDocument, ContextItem, ModelProfile, SubagentRunRecord } from "../types";

const tauriMocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@tauri-apps/api/core")>()),
  invoke: tauriMocks.invoke
}));

import { CLAUDE_AGENT_REGISTRY } from "./claudeAgentProvider";
import { deleteApiKey, fetchModels, flushDocumentSaves, getStoredApiKeyLength, imageAttachmentData, loadDocument, normalizeDocument, prepareImageAttachment, resetDocument, revealApiKey, runModel, saveApiKey, saveDocument } from "./runtime";

const STORAGE_KEY = "mework.document.v1";

function persistedPresetSettings(payload: unknown) {
  const persisted = payload as {
    presets: {
      conversationPresets: Array<{ id: string; settings: { systemPrompt: string } }>;
      defaultConversationPresetId: string;
    };
  };
  return (persisted.presets.conversationPresets.find(
    (preset) => preset.id === persisted.presets.defaultConversationPresetId
  ) ?? persisted.presets.conversationPresets[0]).settings;
}

function defaultPresetSettings(document: AppDocument) {
  return (document.globalSettings.conversationPresets.find(
    (preset) => preset.id === document.globalSettings.defaultConversationPresetId
  ) ?? document.globalSettings.conversationPresets[0]).settings;
}

describe("document normalization", () => {
  beforeEach(() => {
    tauriMocks.invoke.mockReset();
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    window.localStorage.clear();
  });

  afterEach(() => {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    window.localStorage.clear();
  });

  it("keeps exactly one disabled Codex and Claude Agent provider during normalization", () => {
    const missing = createSeedDocument();
    missing.globalSettings.apiProviders = [];
    const generated = normalizeDocument(missing).globalSettings.apiProviders;
    expect(generated).toHaveLength(2);
    expect(generated[0]).toMatchObject({ name: "OpenAI Codex", family: "openai_codex", enabled: false });
    expect(generated[1]).toMatchObject({ name: "Claude Agent", family: "claude_agent", enabled: false });
    expect(generated[0].id).toMatch(/^provider_/u);
    expect(generated[1].id).toMatch(/^provider_/u);

    const existing = { ...generated[0], id: "codex-kept", enabled: true };
    const agent = { ...generated[1], id: "agent-kept", enabled: true };
    const duplicated = createSeedDocument();
    duplicated.globalSettings.apiProviders = [
      existing,
      { ...existing, id: "codex-later" },
      agent,
      { ...agent, id: "agent-later" }
    ];
    const normalized = normalizeDocument(duplicated).globalSettings.apiProviders;
    expect(normalized).toEqual([existing, agent]);
    expect(normalized[0].id).toBe("codex-kept");
    expect(normalized[1].id).toBe("agent-kept");
  });

  it("flattens a base URL stored on the Claude Agent row", () => {
    // The local CLI picks the endpoint. An address left over from the version that
    // still offered one is dropped rather than rejected: the host ignores it either
    // way, and keeping it would let the settings pane imply it still has an effect.
    const source = createSeedDocument();
    const agent = normalizeDocument(source).globalSettings.apiProviders
      .find((provider) => provider.family === "claude_agent")!;
    source.globalSettings.apiProviders = [{ ...agent, baseUrl: "https://api.anthropic.com" }];

    const normalized = normalizeDocument(source).globalSettings.apiProviders
      .find((provider) => provider.family === "claude_agent")!;
    expect(normalized.baseUrl).toBe("");
  });

  it("normalizes invalid current appearance preferences to new-document defaults", () => {
    const current = createSeedDocument() as unknown as {
      globalSettings: Record<string, unknown>;
    };
    current.globalSettings.appLanguage = "fr-FR";
    current.globalSettings.resolvedAppLanguage = "auto";
    current.globalSettings.theme = "sepia";

    expect(normalizeDocument(current).globalSettings).toMatchObject({
      appLanguage: "auto",
      resolvedAppLanguage: "zh-CN",
      theme: "system"
    });
  });

  it("restores missing web-search assets with catalog defaults", () => {
    const current = createSeedDocument() as unknown as { globalSettings: Record<string, unknown> };
    delete current.globalSettings.webSearch;
    // Restore a missing asset layer from seed defaults, including the full catalog and
    // its two anonymously available enabled providers.
    expect(normalizeDocument(current).globalSettings.webSearch).toEqual(
      createSeedDocument().globalSettings.webSearch
    );
  });

  it("defaults legacy agent epochs to one and rejects invalid epochs", () => {
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [
      {
        enabled: true,
        deleted: false,
        name: "valid",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 1,
        memoryEpoch: 1,
        modelSelection: { kind: "inherit" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      },
      {
        enabled: true,
        deleted: false,
        name: "invalid",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 1,
        memoryEpoch: 0,
        modelSelection: { kind: "inherit" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      }
    ];
    const legacy = defaultPresetSettings(current)
      .agentDefinitions[0] as unknown as Record<string, unknown>;
    delete legacy.memoryEpoch;

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings.agentDefinitions)
      .toEqual([expect.objectContaining({ name: "valid", deleted: false, memoryEpoch: 1 })]);
  });

  it("keeps a role's binding intact when its bound model is gone", () => {
    // An older behavior rewrote the pair to `unavailable` and discarded both
    // IDs. At rest there is no way to tell "signed out, catalog not fetched"
    // from "gone forever", so the binding waits instead of being destroyed.
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [
      {
        enabled: true,
        deleted: false,
        name: "dangling-provider",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 3,
        memoryEpoch: 2,
        modelSelection: { kind: "explicit", providerId: "provider_gone", modelId: "gpt-4o" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      },
      {
        enabled: true,
        deleted: false,
        name: "survives",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 3,
        memoryEpoch: 2,
        modelSelection: { kind: "inherit" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      }
    ];

    const roles = normalizeDocument(current).globalSettings
      .conversationPresets[0].settings.agentDefinitions;

    expect(roles.map((role) => role.name)).toEqual(["dangling-provider", "survives"]);
    expect(roles[0].modelSelection).toEqual({
      kind: "explicit",
      providerId: "provider_gone",
      modelId: "gpt-4o"
    });
    // The host identity is untouched: this is not a reconfiguration, so a
    // revision bump would invalidate bindings for a role nobody edited.
    expect(roles[0]).toMatchObject({ revision: 3, memoryEpoch: 2 });
  });

  it("still reads a legacy unavailable binding without resurrecting its ids", () => {
    // Written by a build that discarded the pair on demotion. There is nothing
    // to recover, so it stays exactly as stored.
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [
      {
        enabled: true,
        deleted: false,
        name: "legacy",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 3,
        memoryEpoch: 2,
        modelSelection: { kind: "unavailable" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      }
    ];

    const roles = normalizeDocument(current).globalSettings
      .conversationPresets[0].settings.agentDefinitions;

    expect(roles[0].modelSelection).toEqual({ kind: "unavailable" });
    expect(roles[0]).toMatchObject({ revision: 3, memoryEpoch: 2 });
  });

  it("keeps the binding when its provider is merely disabled", () => {
    // Disabled is not absent. Retain catalog bindings verbatim until the user enables
    // the provider again; treating disabled entries as invalid loses their IDs and can
    // make the document unsaveable. Availability for derivation is decided separately
    // by `agentDefinitionModelIsAvailable`.
    const bound = (providerEnabled: boolean) => {
      const current = createSeedDocument();
      const provider = {
        id: "provider_off",
        name: "Off",
        enabled: providerEnabled,
        presetProviderId: null,
        endpointBaseUrls: {},
        notes: "",
        family: "openai_chat" as const,
        baseUrl: "https://example.invalid/v1",
        activeModelId: null,
        models: [{ id: "m1", label: "m1", name: "", group: "", capabilities: [], reasoningContent: "plaintext" }]
      };
      current.globalSettings.apiProviders = [
        provider as unknown as (typeof current.globalSettings.apiProviders)[number]
      ];
      defaultPresetSettings(current).agentDefinitions = [
        {
          enabled: true,
          deleted: false,
          name: "bound-to-disabled",
          description: "",
          source: "user",
          sourceKey: "",
          revision: 3,
          memoryEpoch: 2,
          modelSelection: { kind: "explicit", providerId: "provider_off", modelId: "m1" },
          memory: "none",
          effort: null,
          tools: null,
          disallowedTools: [],
          searchProvider: null
        }
      ];
      return normalizeDocument(current).globalSettings
        .conversationPresets[0].settings.agentDefinitions[0].modelSelection;
    };

    const intact = { kind: "explicit", providerId: "provider_off", modelId: "m1" };
    expect(bound(false)).toEqual(intact);
    expect(bound(true)).toEqual(intact);
  });

  it("round-trips an already-unavailable selection without re-resolving it", () => {
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [
      {
        enabled: true,
        deleted: false,
        name: "already-unavailable",
        description: "",
        source: "user",
        sourceKey: "",
        revision: 3,
        memoryEpoch: 2,
        modelSelection: { kind: "unavailable" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      }
    ];

    const roles = normalizeDocument(current).globalSettings
      .conversationPresets[0].settings.agentDefinitions;

    expect(roles).toHaveLength(1);
    expect(roles[0].modelSelection).toEqual({ kind: "unavailable" });
  });

  it("keeps a legacy agent definition that predates the execution overrides", () => {
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [{
      enabled: true,
      deleted: false,
      name: "legacy",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 3,
      memoryEpoch: 2,
      modelSelection: { kind: "inherit" },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null
    }];
    const legacy = defaultPresetSettings(current)
      .agentDefinitions[0] as unknown as Record<string, unknown>;
    delete legacy.effort;
    delete legacy.tools;
    delete legacy.disallowedTools;
    // An absent field means follow the conversation, not the conversation-level
    // `native` option. The choices have distinct meanings for roles bound to models
    // that do not support native search.
    delete legacy.searchProvider;

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings.agentDefinitions)
      .toEqual([expect.objectContaining({
        name: "legacy",
        revision: 3,
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      })]);
  });

  it.each([
    ["effort", { effort: "extreme" }],
    ["effort", { effort: 3 }],
    ["tools", { tools: "read_file" }],
    ["tools", { tools: [" read_file"] }],
    ["tools", { tools: [""] }],
    ["tools", { tools: [7] }],
    ["disallowedTools", { disallowedTools: "run_command" }],
    ["disallowedTools", { disallowedTools: [{}] }]
  ])("drops a definition whose %s override is malformed", (_field, override) => {
    // Capability-bearing malformed values drop the definition instead of being
    // clamped to a permissive default.
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [{
      enabled: true,
      deleted: false,
      name: "restricted",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      ...override
    } as unknown as AgentDefinition];

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings.agentDefinitions)
      .toEqual([]);
  });

  it.each([
    ["absent", {}, ""],
    ["not a string", { description: 7 }, ""],
    ["null", { description: null }, ""],
    ["multi-line", { description: "第一行。\n\n第三行 - 带横线。" }, "第一行。\n\n第三行 - 带横线。"],
    ["untrimmed", { description: "  留着两头的空白  " }, "  留着两头的空白  "]
  ])("clamps a %s subagent description instead of dropping the role", (
    _label,
    override,
    expected
  ) => {
    // The mirror image of the table above. The description is prose, not a
    // capability, so a malformed value can only ever make a role say less about
    // itself — dropping the whole role over it would delete a working role for a
    // cosmetic reason. A well-formed one is written through verbatim: no trim,
    // no reflow.
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [{
      enabled: true,
      deleted: false,
      name: "restricted",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      ...override
    } as unknown as AgentDefinition];

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings.agentDefinitions)
      .toEqual([expect.objectContaining({ name: "restricted", description: expected })]);
  });

  it("preserves well-formed execution overrides verbatim", () => {
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [{
      enabled: true,
      deleted: false,
      name: "restricted",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" },
      memory: "none",
      effort: "low",
      tools: ["read_file"],
      disallowedTools: ["run_command"],
      searchProvider: { kind: "explicit", providerKind: "tavily" }
    }];

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings.agentDefinitions)
      .toEqual([expect.objectContaining({
        effort: "low",
        tools: ["read_file"],
        disallowedTools: ["run_command"],
        searchProvider: { kind: "explicit", providerKind: "tavily" }
      })]);
  });

  it.each([
    ["absent", undefined, null],
    ["null", null, null],
    ["native", { kind: "native" }, { kind: "native" }],
    [
      "a known catalog entry",
      { kind: "explicit", providerKind: "exa" },
      { kind: "explicit", providerKind: "exa" }
    ],
    // A malformed capability-bearing value must not silently select an available
    // default, because that would turn invalid data into an intentional choice.
    ["an unknown provider", { kind: "explicit", providerKind: "nope" }, { kind: "unavailable" }],
    ["a broken shape", { kind: 7 }, { kind: "unavailable" }]
  ])("normalizes a role search provider that is %s", (_label, stored, expected) => {
    const current = createSeedDocument();
    defaultPresetSettings(current).agentDefinitions = [{
      enabled: true,
      deleted: false,
      name: "searcher",
      description: "",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: stored
    } as unknown as AgentDefinition];

    expect(normalizeDocument(current).globalSettings.conversationPresets[0].settings
      .agentDefinitions[0].searchProvider).toEqual(expected);
  });

  it("keeps catalog providers, deduplicated and in catalog order", () => {
    const current = createSeedDocument() as unknown as { globalSettings: Record<string, unknown> };
    current.globalSettings.webSearch = {
      providers: [
        { kind: "searxng", enabled: 1, searchApiHost: " http://searx.local:8080 ", engines: [" google ", "", 5], basicAuthUsername: " searx " },
        { kind: "tavily", enabled: true, searchApiHost: " https://tavily.example " },
        { kind: "tavily", enabled: false, searchApiHost: "ignored" },
        { kind: "unknown", enabled: true },
        42
      ],
      // A fetch provider must support fetching. Tavily only searches, so clear this
      // binding during load.
      fetchProvider: "tavily",
      maxResults: 999,
      excludeDomains: [" *://ads.example/* ", "", 7],
      compression: { method: "nonsense", cutoffLimit: 0 }
    };
    const normalized = normalizeDocument(current).globalSettings.webSearch;
    // Catalog order is canonical: retain the first duplicate and drop non-catalog or
    // non-object entries.
    expect(normalized.providers.map((provider) => provider.kind)).toEqual(
      createSeedDocument().globalSettings.webSearch.providers.map((provider) => provider.kind)
    );
    const tavily = normalized.providers.find((provider) => provider.kind === "tavily");
    expect(tavily).toEqual({
      kind: "tavily",
      enabled: true,
      searchApiHost: "https://tavily.example",
      fetchApiHost: "",
      engines: [],
      basicAuthUsername: ""
    });
    const searxng = normalized.providers.find((provider) => provider.kind === "searxng");
    expect(searxng?.enabled).toBe(true);
    expect(searxng?.engines).toEqual(["google"]);
    expect(searxng?.basicAuthUsername).toBe("searx");
    expect(normalized.fetchProvider).toBeNull();
    // Out-of-range controls fall back to defaults instead of being persisted verbatim.
    expect(normalized.maxResults).toBe(5);
    expect(normalized.excludeDomains).toEqual(["*://ads.example/*"]);
    expect(normalized.compression).toEqual({ method: "cutoff", cutoffLimit: 2000 });
  });

  it("keeps a fetch provider that really can fetch", () => {
    const current = createSeedDocument() as unknown as { globalSettings: Record<string, unknown> };
    const seeded = createSeedDocument().globalSettings.webSearch;
    current.globalSettings.webSearch = { ...seeded, fetchProvider: "firecrawl" };
    expect(normalizeDocument(current).globalSettings.webSearch.fetchProvider).toBe("firecrawl");
  });

  it("bounds per-conversation search limits and provider selections", () => {
    const current = createSeedDocument() as unknown as { workspaces: { conversations: { settings: Record<string, unknown> }[] }[] };
    const conversations = current.workspaces[0].conversations;
    conversations.push(structuredClone(conversations[0]));
    conversations[0].settings.webSearch = { maxSearchesPerCall: 2_000_000, provider: { kind: "explicit", providerKind: "tavily" } };
    conversations[1].settings.webSearch = { maxSearchesPerCall: -5, provider: { kind: "explicit", providerKind: "ghost" } };
    const normalized = normalizeDocument(current).workspaces[0].conversations;
    expect(normalized[0].settings.webSearch).toEqual({ maxSearchesPerCall: 99_999, provider: { kind: "explicit", providerKind: "tavily" } });
    expect(normalized[1].settings.webSearch).toEqual({ maxSearchesPerCall: 0, provider: { kind: "unavailable" } });
  });

  it("rewrites malformed provider selections to unavailable", () => {
    const current = createSeedDocument() as unknown as { workspaces: { conversations: { settings: Record<string, unknown> }[] }[] };
    current.workspaces[0].conversations[0].settings.webSearch = { maxSearchesPerCall: 0, provider: { kind: "bad" } };
    expect(normalizeDocument(current).workspaces[0].conversations[0].settings.webSearch).toEqual({ maxSearchesPerCall: 0, provider: { kind: "unavailable" } });
  });

  // Absence is not unavailability. Rust gives `provider` a `#[serde(default)]` Auto
  // value; interpreting an omitted field as unavailable would silently remove search
  // capability from valid documents and make the two sides disagree about the JSON.
  it("treats an absent provider selection as auto, not unavailable", () => {
    const current = createSeedDocument() as unknown as { workspaces: { conversations: { settings: Record<string, unknown> }[] }[] };
    current.workspaces[0].conversations[0].settings.webSearch = { maxSearchesPerCall: 3 };
    expect(normalizeDocument(current).workspaces[0].conversations[0].settings.webSearch)
      .toEqual({ maxSearchesPerCall: 3, provider: { kind: "native" } });
  });

  it("restores only the reserved temporary workspace when it is missing", () => {
    const missing = createSeedDocument();
    missing.workspaces = missing.workspaces.filter((workspace) => workspace.id !== "__temporary__");
    const restored = normalizeDocument(missing);
    expect(restored.workspaces.find((workspace) => workspace.id === "__temporary__"))
      .toMatchObject({ name: "临时工作区", kind: "temporary", path: "", conversations: [] });
  });

  it("stays a fixed point on capability selections when normalized twice", () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const capabilityIds = {
      hookIds: ["hook_format"],
      // The catalog has no `skill_missing`; normalization must retain dangling IDs.
      skillIds: ["skill_installer", "skill_missing"],
      mcpIds: ["mcp_workspace"]
    };
    const toolDescriptionFileId = "tooldesc_user_main_0f0f0f0f";
    const webSearch = {
      maxSearchesPerCall: 42,
      provider: { kind: "native" as const }
    };
    Object.assign(conversation.settings, capabilityIds, { toolDescriptionFileId, webSearch });
    Object.assign(defaultPresetSettings(document), capabilityIds, { toolDescriptionFileId });

    const once = normalizeDocument(document);
    const twice = normalizeDocument(once);

    // A second normalization pass must equal the first: normalization is idempotent
    // and must retain dangling IDs.
    for (const normalized of [once, twice]) {
      expect(normalized.workspaces[0].conversations[0].settings).toMatchObject({
        ...capabilityIds,
        toolDescriptionFileId,
        webSearch
      });
      expect(defaultPresetSettings(normalized)).toMatchObject({ ...capabilityIds, toolDescriptionFileId });
    }
    expect(twice).toEqual(once);
  });

  it("preserves a blank conversation preset name during normalization", () => {
    const document = createSeedDocument();
    document.globalSettings.conversationPresets[0].name = "";

    const normalized = normalizeDocument(document);

    expect(normalized.globalSettings.conversationPresets[0].name).toBe("");
  });

  it("preserves dangling capability IDs in presets and in live conversation settings", () => {
    // Resources may be referenced before installation or remain in a preset after
    // removal, so catalog filtering would silently change behavior when they return.
    const document = createSeedDocument();
    const globalPreset = defaultPresetSettings(document);
    globalPreset.skillIds = ["skills_missing"];
    globalPreset.mcpIds = ["mcp_missing"];
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings.hookIds = ["hooks_local_missing"];
    conversation.settings.skillIds = ["skills_local_missing"];
    conversation.settings.mcpIds = ["mcp_local_missing"];

    const normalized = normalizeDocument(document);
    expect(defaultPresetSettings(normalized)).toMatchObject({
      skillIds: ["skills_missing"],
      mcpIds: ["mcp_missing"]
    });
    const normalizedConversation = normalized.workspaces[0].conversations[0];
    expect(normalizedConversation.settings.hookIds).toEqual(["hooks_local_missing"]);
    expect(normalizedConversation.settings.skillIds).toEqual(["skills_local_missing"]);
    expect(normalizedConversation.settings.mcpIds).toEqual(["mcp_local_missing"]);
  });

  it("preserves branch slots and recursively normalizes their suffix contexts", () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "fork-user", kind: "user", content: "fork", createdAt: "2026-07-20T00:00:00Z" },
      { id: "new-answer", kind: "assistant", content: "new", createdAt: "2026-07-20T00:00:01Z" }
    ];
    conversation.branches = [
      {
        id: "old-branch",
        forkContextId: "fork-user",
        active: false,
        contexts: [{ id: "old-answer", kind: "assistant", content: "old", createdAt: "2026-07-20T00:00:02Z" }],
        createdAt: "2026-07-20T00:00:00Z",
        updatedAt: "2026-07-20T00:00:02Z"
      },
      {
        id: "new-branch",
        forkContextId: "fork-user",
        active: true,
        contexts: [],
        createdAt: "2026-07-20T00:00:03Z",
        updatedAt: "2026-07-20T00:00:03Z"
      }
    ];

    expect(normalizeDocument(document).workspaces[0].conversations[0].branches).toEqual(conversation.branches);
  });

  it("keeps preview image bytes outside the document and resolves them after a reload boundary", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));

    const attachment = await prepareImageAttachment("pixel.png", bytes);

    expect(attachment).toMatchObject({
      name: "pixel.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: bytes.byteLength
    });
    expect(JSON.stringify(createSeedDocument())).not.toContain(encoded);
    expect(await imageAttachmentData(attachment.id)).toBe(`data:image/png;base64,${encoded}`);
  });

  it("rejects invalid or oversized browser-preview image dimensions before storing bytes", async () => {
    const pngWithDimensions = (width: number, height: number) => {
      const bytes = new Uint8Array(24);
      bytes.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
      const view = new DataView(bytes.buffer);
      view.setUint32(16, width);
      view.setUint32(20, height);
      return bytes;
    };

    await expect(prepareImageAttachment("unknown.png", pngWithDimensions(0, 100)))
      .rejects.toThrow(/1–8000/);
    await expect(prepareImageAttachment("too-wide.png", pngWithDimensions(8001, 1)))
      .rejects.toThrow(/1–8000/);
    await expect(prepareImageAttachment("too-many-pixels.png", pngWithDimensions(4097, 4097)))
      .rejects.toThrow(/16 MP/);
    expect(window.localStorage.length).toBe(0);
  });

  it("uses the native filename contract in browser preview", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));

    await expect(prepareImageAttachment("\u0000unsafe.png", bytes))
      .rejects.toThrow(/控制字符/);
    await expect(prepareImageAttachment("界".repeat(86), bytes))
      .rejects.toThrow(/256 字节/);
    expect(window.localStorage.length).toBe(0);
  });

  it("accepts a static GIF but rejects an animated GIF before storing it", async () => {
    const staticGif = new Uint8Array([
      71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255,
      44, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59
    ]);
    const animatedGif = new Uint8Array([
      ...staticGif.slice(0, -1),
      ...staticGif.slice(19, -1),
      59
    ]);

    await expect(prepareImageAttachment("static.gif", staticGif)).resolves.toMatchObject({
      mime: "image/gif",
      width: 1,
      height: 1
    });
    const storedKeys = window.localStorage.length;
    await expect(prepareImageAttachment("animated.gif", animatedGif))
      .rejects.toThrow(/动画 GIF/);
    expect(window.localStorage.length).toBe(storedKeys);
  });

  it("revalidates browser-preview sidecar bytes and their SHA-256 before rendering", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));
    const attachment = await prepareImageAttachment("safe.png", bytes);
    const storageKey = `mework.image-attachment.v1.${attachment.id}`;
    const stored = JSON.parse(window.localStorage.getItem(storageKey)!);
    const tampered = Uint8Array.from(bytes);
    tampered[tampered.length - 1] ^= 1;
    stored.dataUrl = `data:image/png;base64,${window.btoa(
      String.fromCharCode(...tampered)
    )}`;
    window.localStorage.setItem(storageKey, JSON.stringify(stored));

    await expect(imageAttachmentData(attachment.id)).rejects.toThrow(/已损坏/);
  });

  it("revalidates the single-frame policy after browser-preview sidecar tampering", async () => {
    const staticGif = new Uint8Array([
      71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255,
      44, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59
    ]);
    const animatedGif = new Uint8Array([
      ...staticGif.slice(0, -1),
      ...staticGif.slice(19, -1),
      59
    ]);
    const attachment = await prepareImageAttachment("safe.gif", staticGif);
    const storageKey = `mework.image-attachment.v1.${attachment.id}`;
    const stored = JSON.parse(window.localStorage.getItem(storageKey)!);
    stored.attachment.bytes = animatedGif.byteLength;
    stored.dataUrl = `data:image/gif;base64,${window.btoa(
      String.fromCharCode(...animatedGif)
    )}`;
    window.localStorage.setItem(storageKey, JSON.stringify(stored));

    await expect(imageAttachmentData(attachment.id)).rejects.toThrow(/已损坏/);
  });

  it("delays browser-preview orphan cleanup and never classifies an unsent draft during an unrelated save", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));
    const attachment = await prepareImageAttachment("draft.png", bytes);
    const indexKey = "mework.image-attachment-index.v1";
    const storageKey = `mework.image-attachment.v1.${attachment.id}`;

    await saveDocument(createSeedDocument());
    expect(JSON.parse(window.localStorage.getItem(indexKey)!)[0]).not.toHaveProperty("orphanedAt");
    expect(window.localStorage.getItem(storageKey)).not.toBeNull();

    const referenced = createSeedDocument();
    referenced.workspaces[0].conversations[0].contexts = [{
      id: "image-user",
      kind: "user",
      content: "",
      images: [attachment],
      createdAt: "2026-07-24T00:00:00Z"
    }];
    await saveDocument(referenced);
    const forgedOnly = createSeedDocument();
    forgedOnly.workspaces[0].conversations[0].contexts = [{
      id: "tool-input-image-shaped-json",
      kind: "tool",
      toolName: "custom_tool",
      input: { images: [{ id: attachment.id }] },
      result: {
        success: true,
        output: "ordinary JSON",
        executedAt: "2026-07-24T00:00:00Z",
        durationMs: 1
      },
      createdAt: "2026-07-24T00:00:00Z"
    }];
    await saveDocument(forgedOnly);
    const orphaned = JSON.parse(window.localStorage.getItem(indexKey)!);
    expect(orphaned[0].orphanedAt).toEqual(expect.any(Number));

    orphaned[0].orphanedAt = Date.now() - (60 * 60 * 1000) - 1;
    window.localStorage.setItem(indexKey, JSON.stringify(orphaned));
    window.localStorage.setItem("unrelated.user.storage", "keep");
    await loadDocument();
    expect(window.localStorage.getItem(storageKey)).toBeNull();
    expect(window.localStorage.getItem("unrelated.user.storage")).toBe("keep");
  });

  it("browser-preview reset removes only Mework image attachment storage", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));
    const attachment = await prepareImageAttachment("reset.png", bytes);
    const storageKey = `mework.image-attachment.v1.${attachment.id}`;
    window.localStorage.setItem("unrelated.user.storage", "keep");

    await resetDocument();

    expect(window.localStorage.getItem(storageKey)).toBeNull();
    expect(window.localStorage.getItem("mework.image-attachment-index.v1")).toBeNull();
    expect(window.localStorage.getItem("unrelated.user.storage")).toBe("keep");
  });

  it("browser-preview reset forgets the key markers of every provider", async () => {
    // Key-status metadata is fingerprinted by provider ID. Reset it so a recreated
    // provider with the same ID does not show stale configured state or length.
    const provider: ApiProvider = {
      id: "provider_reset_probe",
      name: "重置探针",
      enabled: true,
      family: "openai_responses",
      baseUrl: "https://api.openai.com/v1",
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      models: [],
      activeModelId: null
    };
    await saveApiKey(provider, "preview-secret-key");
    expect(await getStoredApiKeyLength(provider.id)).toBe(18);
    window.localStorage.setItem("unrelated.session.storage", "keep");

    await resetDocument();

    expect(await getStoredApiKeyLength(provider.id)).toBeUndefined();
    expect(window.localStorage.getItem("unrelated.session.storage")).toBe("keep");
  });

  it("rejects a forged browser-preview sidecar MIME instead of rendering active image content", async () => {
    const encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(window.atob(encoded), (character) => character.charCodeAt(0));
    const attachment = await prepareImageAttachment("safe.png", bytes);
    const storageKey = `mework.image-attachment.v1.${attachment.id}`;
    const stored = JSON.parse(window.localStorage.getItem(storageKey)!);
    stored.dataUrl = "data:image/svg+xml;base64,PHN2Zy8+";
    window.localStorage.setItem(storageKey, JSON.stringify(stored));

    await expect(imageAttachmentData(attachment.id)).rejects.toThrow(/已损坏/);
  });

  it("uses the final native image attachment command shapes", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const attachment = {
      id: "image-native",
      name: "native.png",
      mime: "image/png",
      width: 20,
      height: 10,
      bytes: 4
    };
    tauriMocks.invoke
      .mockResolvedValueOnce(attachment)
      .mockResolvedValueOnce("data:image/png;base64,AAAA");

    await expect(prepareImageAttachment("native.png", new Uint8Array([1, 2, 3, 4]))).resolves.toEqual(attachment);
    await expect(imageAttachmentData("image-native")).resolves.toBe("data:image/png;base64,AAAA");
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(1, "image_attachment_upload", {
      name: "native.png",
      data: "AQIDBA=="
    });
    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(2, "image_attachment_data", {
      imageId: "image-native"
    });
  });

  it("skips a disabled provider when resolving the active selection", () => {
    const document = createSeedDocument() as unknown as {
      globalSettings: Record<string, unknown>;
    };
    document.globalSettings.apiProviders = [
      {
        id: "provider-disabled",
        name: "Disabled",
        enabled: false,
        presetProviderId: null,
        endpointBaseUrls: {},
        notes: "",
        family: "openai_responses",
        baseUrl: "https://example.invalid/v1",
        models: [],
        activeModelId: null
      },
      {
        id: "provider-enabled",
        name: "Enabled",
        enabled: true,
        presetProviderId: null,
        endpointBaseUrls: {},
        notes: "",
        family: "openai_responses",
        baseUrl: "https://example.invalid/v1",
        models: [],
        activeModelId: null
      }
    ];
    document.globalSettings.activeProviderId = null;

    expect(normalizeDocument(document).globalSettings.activeProviderId).toBe("provider-enabled");

    // An explicit disabled provider is also skipped in favor of the first available
    // provider.
    document.globalSettings.activeProviderId = "provider-disabled";
    expect(normalizeDocument(document).globalSettings.activeProviderId).toBe("provider-enabled");
  });

  it("validates persisted effort values and falls back to the global effort for invalid ones", () => {
    const persisted = createSeedDocument() as unknown as Record<string, unknown>;
    (persisted.globalSettings as Record<string, unknown>).lastReasoningEffort = "high";
    const persistedWorkspaces = persisted.workspaces as Array<{ conversations: Array<{ settings: Record<string, unknown> }> }>;
    persistedWorkspaces[0].conversations[0].settings.reasoningEffort = "low";
    // `minimal` is not a valid level, so reject the setting and fall back to the most
    // recent global level.
    persistedWorkspaces[0].conversations[1].settings.reasoningEffort = "minimal";

    const normalized = normalizeDocument(persisted);
    expect(normalized.globalSettings.lastReasoningEffort).toBe("high");
    expect(normalized.workspaces[0].conversations[0].settings.reasoningEffort).toBe("low");
    expect(normalized.workspaces[0].conversations[1].settings.reasoningEffort).toBe("high");
  });

  it("uses the approval-first policy when security data is missing or invalid", () => {
    const current = createSeedDocument() as unknown as Record<string, unknown>;
    const currentWorkspaces = current.workspaces as Array<{ conversations: Array<{ settings: Record<string, unknown> }> }>;
    currentWorkspaces[0].conversations[0].settings.securityLevel = "unsupported";
    // An absent conversation security level must use the most cautious default, not
    // a mutable global default.
    delete currentWorkspaces[0].conversations[1].settings.securityLevel;

    const normalized = normalizeDocument(current);
    expect(normalized.workspaces[0].conversations[0].settings.securityLevel).toBe("request_approval");
    expect(normalized.workspaces[0].conversations[1].settings.securityLevel).toBe("request_approval");
  });

  it("normalizes a workspace's remembered conversation settings and keeps a dangling default preset id", () => {
    const current = createSeedDocument() as unknown as Record<string, unknown>;
    const currentWorkspaces = current.workspaces as Array<Record<string, unknown>>;
    currentWorkspaces[0].defaultConversationPresetId = "preset-that-was-deleted";
    currentWorkspaces[0].lastConversationSettings = {
      systemPrompt: "记住的设置",
      enabledTools: ["read", "read", "no-such-tool"],
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      toolDescriptionFileId: null,
      agentDefinitions: [],
      reasoningEffort: "high",
      securityLevel: "unsupported",
      globalMemoryEnabled: true,
      projectMemoryEnabled: false
    };

    const workspace = normalizeDocument(current).workspaces[0];
    // Retain dangling IDs so deleting a preset neither rejects the document nor
    // silently rewrites the user's selection.
    expect(workspace.defaultConversationPresetId).toBe("preset-that-was-deleted");
    expect(workspace.lastConversationSettings?.systemPrompt).toBe("记住的设置");
    expect(workspace.lastConversationSettings?.enabledTools).toEqual(["read"]);
    expect(workspace.lastConversationSettings?.securityLevel).toBe("request_approval");
    expect(workspace.lastConversationSettings?.reasoningEffort).toBe("high");
    expect(workspace.lastConversationSettings?.globalMemoryEnabled).toBe(true);
  });

  it("never retains API Key plaintext in browser preview storage", async () => {
    const provider = {
      ...createSeedDocument().globalSettings.apiProviders[0],
      id: "preview-provider"
    };
    await saveApiKey(provider, "super-secret-preview-key");
    const storedValues = Array.from({ length: window.localStorage.length }, (_, index) => {
      const key = window.localStorage.key(index)!;
      return window.localStorage.getItem(key);
    });
    const storedKeys = Array.from({ length: window.localStorage.length }, (_, index) => window.localStorage.key(index));
    // Persist only non-secret length metadata; its fingerprint contains neither ID nor
    // endpoint.
    expect(JSON.stringify(storedValues)).not.toContain("super-secret-preview-key");
    expect(JSON.stringify(storedKeys)).not.toContain(provider.id);
    expect(JSON.stringify(storedKeys)).not.toContain(provider.baseUrl);
    expect(await getStoredApiKeyLength(provider.id)).toBe(24);
    await expect(revealApiKey(provider)).rejects.toThrow(/浏览器预览不会保留/);
  });

  it("keeps key status outside the persisted provider document", async () => {
    const document = createSeedDocument();
    const provider = document.globalSettings.apiProviders[0];
    await saveDocument(document);
    await saveApiKey(provider, "discard-me");

    const loadedProvider = (await loadDocument()).globalSettings.apiProviders[0];
    expect(loadedProvider).not.toHaveProperty("hasApiKey");
    expect(loadedProvider).not.toHaveProperty("apiKeys");
    expect(JSON.parse(window.localStorage.getItem(STORAGE_KEY)!).assets.apiProviders[0]).not.toHaveProperty("hasApiKey");
    expect(await getStoredApiKeyLength(loadedProvider.id)).toBe(10);
  });

  it("persists the schema-60 assets and presets containers with no retired sub-preset layer", async () => {
    const document = createSeedDocument();
    document.workspaces[0].conversations[0].settings.skillIds = ["skill_installer"];
    document.globalSettings.webSearch = {
      ...document.globalSettings.webSearch,
      providers: [{
        kind: "tavily",
        enabled: true,
        searchApiHost: "",
        fetchApiHost: "",
        engines: [],
        basicAuthUsername: ""
      }]
    };

    await saveDocument(document);
    const persisted = JSON.parse(window.localStorage.getItem(STORAGE_KEY)!);

    expect(persisted.schemaVersion).toBe(createSeedDocument().schemaVersion);
    expect(Object.keys(persisted.presets).sort()).toEqual([
      "conversationPresets",
      "defaultConversationPresetId"
    ]);
    // The persisted asset-key set is a contract: MCP servers and skills are required,
    // and any missing or extra key must be visible to this assertion.
    expect(Object.keys(persisted.assets).sort()).toEqual([
      "apiProviders",
      "executionEnvironments",
      "mcpServers",
      "skills",
      "webSearch"
    ]);
    expect(Object.keys(persisted.assets.webSearch).sort()).toEqual([
      "compression",
      "excludeDomains",
      "fetchProvider",
      "maxResults",
      "providers"
    ]);
    // Core settings have an exact key set: appearance, shortcuts, and environment
    // settings belong here, while assets and presets do not.
    expect(Object.keys(persisted.globalSettings).sort()).toEqual([
      "activeProviderId",
      "appLanguage",
      "appearance",
      "environmentTools",
      "lastReasoningEffort",
      "resolvedAppLanguage",
      "shortcuts",
      "theme"
    ]);
    for (const provider of persisted.assets.apiProviders) {
      expect(provider).not.toHaveProperty("chatEnabled");
    }
    // Retired sub-preset layers and their reference keys must never reach disk.
    const serialized = JSON.stringify(persisted);
    for (const retired of [
      "hookPresets",
      "skillPresets",
      "mcpPresets",
      "toolDescriptionSets",
      "securityPolicies",
      "modelPresets",
      "toolDescriptionSetId",
      "toolDescriptions",
      "modelPresetId",
      "securityPolicyId",
      "chatEnabled",
      "localPreset",
      "presetId"
    ]) {
      expect(serialized).not.toContain(retired);
    }
    const settings = persisted.workspaces[0].conversations[0].settings;
    // `modelSelection` is removed only from conversation settings; subagent definitions
    // may still carry it.
    expect(settings).not.toHaveProperty("modelSelection");
    expect(settings.skillIds).toEqual(["skill_installer"]);
    expect(Object.keys(settings)).toEqual(expect.arrayContaining([
      "hookIds",
      "skillIds",
      "mcpIds",
      "toolDescriptionFileId",
      "webSearch"
    ]));
    expect(Object.keys(settings.webSearch).sort()).toEqual([
      "maxSearchesPerCall",
      "provider"
    ]);
  });

  it("persists a validated structured result through a real save/load round-trip", async () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const timestamp = "2026-07-28T00:00:00Z";
    const structuredOutput = { verdict: "ok", findings: [{ file: "a.ts", line: 3 }] };
    const outputSchema = {
      type: "object",
      properties: { verdict: { type: "string" } },
      required: ["verdict"]
    };
    const usage = { inputTokens: 120, cachedInputTokens: 40, outputTokens: 8, totalTokens: 128 };
    conversation.contexts = [{
      id: "schema-bound-agent",
      kind: "tool",
      toolName: "agent_spawn",
      input: {},
      result: { success: true, output: "spawned a1", executedAt: timestamp, durationMs: 1 },
      subagent: {
        kind: "general",
        name: "a1",
        task: "review",
        status: "completed",
        contexts: [],
        updates: [],
        structuredOutput,
        outputSchema,
        usage
      },
      createdAt: timestamp
    }];

    await saveDocument(document);
    const stored = JSON.parse(window.localStorage.getItem(STORAGE_KEY)!);
    expect(stored.workspaces[0].conversations[0].contexts[0].subagent.structuredOutput)
      .toEqual(structuredOutput);
    // Dropping the schema on save silently unbinds every later continuation of
    // this agent (the host re-compiles it at rehydration), so the STORED bytes
    // are the assertion that matters.
    expect(stored.workspaces[0].conversations[0].contexts[0].subagent.outputSchema)
      .toEqual(outputSchema);
    // The host fingerprints the ENTIRE serialized record for the tool receipt,
    // so a field that survives the run but not the save does not merely lose a
    // number — it makes the document unsaveable with "结果或附属记录不是后端执行返回值".
    // That is exactly how `usage` was lost; this builder is an allowlist, so
    // every field of SubagentRunRecord needs an assertion like this one.
    expect(stored.workspaces[0].conversations[0].contexts[0].subagent.usage)
      .toEqual(usage);

    const loaded = (await loadDocument()).workspaces[0].conversations[0];
    expect((loaded.contexts[0] as Extract<ContextItem, { kind: "tool" }>).subagent?.structuredOutput)
      .toEqual(structuredOutput);
    expect((loaded.contexts[0] as Extract<ContextItem, { kind: "tool" }>).subagent?.outputSchema)
      .toEqual(outputSchema);
    expect((loaded.contexts[0] as Extract<ContextItem, { kind: "tool" }>).subagent?.usage)
      .toEqual(usage);
  });

  it("carries the host attestation through a save/load round-trip", async () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const timestamp = "2026-08-09T00:00:00Z";
    const attestation = "a".repeat(64);
    conversation.contexts = [{
      id: "attested-card",
      kind: "tool",
      toolName: "read",
      input: { path: "README.md" },
      result: { success: true, output: "contents", executedAt: timestamp, durationMs: 1 },
      attestation,
      createdAt: timestamp
    }];

    await saveDocument(document);
    const stored = JSON.parse(window.localStorage.getItem(STORAGE_KEY)!);

    // This projection is an allowlist, so an unnamed field is dropped with no
    // type error. Dropping this one strips the card's only durable proof that
    // its result came from the backend, and the host quarantines the card on
    // the very next save — the same class of failure that losing `usage` caused.
    expect(stored.workspaces[0].conversations[0].contexts[0].attestation).toBe(attestation);

    const loaded = (await loadDocument()).workspaces[0].conversations[0];
    expect((loaded.contexts[0] as Extract<ContextItem, { kind: "tool" }>).attestation)
      .toBe(attestation);
  });

  it("leaves a pre-versioning binding unstamped instead of inventing a receipt version", async () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const timestamp = "2026-07-24T01:00:00Z";
    const toolContext = (id: string, subagent: SubagentRunRecord): ContextItem => ({
      id,
      kind: "tool",
      toolName: "agent_spawn",
      input: {},
      result: {
        success: true,
        output: `spawned ${subagent.name}`,
        executedAt: timestamp,
        durationMs: 1
      },
      subagent,
      createdAt: timestamp
    });
    const baseRecord = {
      kind: "general" as const,
      task: "continue safely",
      status: "completed" as const,
      contexts: [],
      updates: []
    };
    // Neither binding carries `receiptVersion`, exactly as every record written
    // before the payload version existed looks on disk.
    conversation.contexts = [
      toolContext("legacy-fork-agent", {
        ...baseRecord,
        name: "legacy-fork-a1",
        inheritsModelMemory: true,
        forkModelBinding: {
          providerId: "provider-a",
          modelId: "kimi-k3",
          memoryLanguage: "zh-CN",
          memoryToolNames: ["memory_read"],
          systemPromptSnapshot: "trusted fork prompt",
          systemPromptReceipt: "2".repeat(64),
          bindingReceipt: "4".repeat(64)
        },
        executionModeReceipt: "5".repeat(64)
      }),
      toolContext("legacy-named-agent", {
        ...baseRecord,
        name: "legacy-named-a1",
        agentDefinition: {
          source: "user",
          sourceKey: "",
          name: "reviewer",
          revision: 7,
          memoryEpoch: 3,
          providerId: "provider-a",
          modelId: "deepseek-v4-flash",
          memory: "project",
          scopeKey: "workspace-a",
          configurationReceipt: "6".repeat(64)
        },
        executionModeReceipt: "7".repeat(64)
      })
    ];

    await saveDocument(document);
    const storedContexts = JSON.parse(window.localStorage.getItem(STORAGE_KEY)!)
      .workspaces[0].conversations[0].contexts;
    // Absence is the version marker on the host side, so the projection must
    // preserve absence: stamping an explicit 1 here would make a genuine
    // pre-versioning record indistinguishable from a deliberately stamped v1.
    expect(Object.keys(storedContexts[0].subagent.forkModelBinding))
      .not.toContain("receiptVersion");
    expect(Object.keys(storedContexts[1].subagent.agentDefinition))
      .not.toContain("receiptVersion");
    // The rest of the binding still round-trips, so the absence above is the
    // projection preserving the record rather than losing the whole binding.
    expect(storedContexts[0].subagent.forkModelBinding.bindingReceipt).toBe("4".repeat(64));
    expect(storedContexts[1].subagent.agentDefinition.configurationReceipt).toBe("6".repeat(64));

    const loadedContexts = (await loadDocument()).workspaces[0].conversations[0].contexts;
    expect(loadedContexts).toEqual(storedContexts);
    const loadedFork = loadedContexts[0] as Extract<ContextItem, { kind: "tool" }>;
    const loadedNamed = loadedContexts[1] as Extract<ContextItem, { kind: "tool" }>;
    expect(loadedFork.subagent?.forkModelBinding?.receiptVersion).toBeUndefined();
    expect(loadedNamed.subagent?.agentDefinition?.receiptVersion).toBeUndefined();
  });

  it("keeps the browser marker when API format or endpoint changes", async () => {
    const provider = createSeedDocument().globalSettings.apiProviders[0];
    await saveApiKey(provider, "discard-me");
    // Key-state identity is provider ID only, so endpoint and protocol changes retain
    // the same marker.
    expect(await getStoredApiKeyLength(provider.id)).toBe(10);
    await saveApiKey({ ...provider, baseUrl: `${provider.baseUrl}/` }, "discard-me");
    expect(await getStoredApiKeyLength(provider.id)).toBe(10);
    await saveApiKey({ ...provider, family: "openai_chat" }, "discard-me");
    expect(await getStoredApiKeyLength(provider.id)).toBe(10);
  });

  it("deletes browser key state and its persisted length", async () => {
    const provider = createSeedDocument().globalSettings.apiProviders[0];
    await saveApiKey(provider, "delete-this-key");
    expect(await getStoredApiKeyLength(provider.id)).toBe(15);

    await expect(deleteApiKey(provider)).resolves.toEqual({ configured: false });
    expect(await getStoredApiKeyLength(provider.id)).toBeUndefined();
  });

  it("preserves malformed browser JSON instead of silently replacing it with defaults", async () => {
    const malformed = "{not-json";
    window.localStorage.setItem(STORAGE_KEY, malformed);

    await expect(loadDocument()).rejects.toThrow(/原始数据已保留/);
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe(malformed);
  });

  it("preserves documents written by a future schema instead of downgrading them", async () => {
    const future = createSeedDocument();
    future.schemaVersion += 1;
    const serialized = JSON.stringify(future);
    window.localStorage.setItem(STORAGE_KEY, serialized);

    await expect(loadDocument()).rejects.toThrow(/更新版本|schema/);
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe(serialized);
  });

  it("rejects documents written by an outdated schema now that migrations are deleted", async () => {
    const outdated = createSeedDocument();
    outdated.schemaVersion -= 1;
    const serialized = JSON.stringify(outdated);
    window.localStorage.setItem(STORAGE_KEY, serialized);

    await expect(loadDocument()).rejects.toThrow(/旧版 schema/);
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe(serialized);
  });

  it("trims and deduplicates provider and model IDs before resolving active selections", () => {
    const source = createSeedDocument() as unknown as Record<string, unknown>;
    const settings = { ...(source.globalSettings as Record<string, unknown>) };
    const providers = settings.apiProviders as Array<Record<string, unknown>>;
    settings.apiProviders = [
      {
        ...providers[0],
        id: "  openai_responses  ",
        activeModelId: "  model-a  ",
        models: [
          // The first two collide once the id is trimmed. Their capability sets
          // differ so the assertion below proves the earlier entry wins rather
          // than the later duplicate.
          { id: "  model-a  ", name: "", group: "", capabilities: ["image_recognition"] },
          { id: "model-a", name: "", group: "", capabilities: [] },
          { id: "model-b", name: "", group: "", capabilities: [] },
          { id: "   ", name: "", group: "", capabilities: [] }
        ]
      },
      { ...providers[0], id: "openai_responses", name: "duplicate provider" },
      { ...providers[1], id: "  openai_chat  " }
    ];
    settings.activeProviderId = "  openai_responses  ";
    source.globalSettings = settings;

    const normalized = normalizeDocument(source);
    expect(normalized.globalSettings.apiProviders.slice(0, 2).map((provider) => provider.id))
      .toEqual(["openai_responses", "openai_chat"]);
    expect(normalized.globalSettings.apiProviders[2]).toMatchObject({ family: "openai_codex", enabled: false });
    expect(normalized.globalSettings.activeProviderId).toBe("openai_responses");
    expect(normalized.globalSettings.apiProviders[0].models.map((model) => model.id)).toEqual(["model-a", "model-b"]);
    expect(normalized.globalSettings.apiProviders[0].models[0].capabilities).toEqual(["image_recognition"]);
    expect(normalized.globalSettings.apiProviders[0].activeModelId).toBe("model-a");
  });

  it("adds only the built-in providers to document-carried user entries", () => {
    // Provider normalization preserves user rows and appends its fixed built-in rows.
    const source = createSeedDocument() as unknown as Record<string, unknown>;
    const settings = { ...(source.globalSettings as Record<string, unknown>) };
    settings.apiProviders = [{
      id: "provider_mine",
      name: "我的中转站",
      enabled: true,
      family: "openai_chat",
      baseUrl: "https://relay.example.com/v1",
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      models: [{ id: "gpt-5", name: "", group: "", capabilities: [] }],
      activeModelId: "gpt-5"
    }];
    settings.activeProviderId = "provider_mine";
    source.globalSettings = settings;

    const normalized = normalizeDocument(source);
    expect(normalized.globalSettings.apiProviders[0].id).toBe("provider_mine");
    expect(normalized.globalSettings.apiProviders).toHaveLength(3);
    expect(normalized.globalSettings.apiProviders[0]).toMatchObject({
      enabled: true,
      name: "我的中转站",
      baseUrl: "https://relay.example.com/v1",
      activeModelId: "gpt-5"
    });
    expect(normalized.globalSettings.apiProviders[1]).toMatchObject({ family: "openai_codex", enabled: false });
    expect(normalized.globalSettings.apiProviders[2]).toMatchObject({ family: "claude_agent", enabled: false });
    expect(normalized.globalSettings.activeProviderId).toBe("provider_mine");
    // Catalog-like IDs receive no special treatment; they are ordinary user entries.
    expect(normalized.globalSettings.apiProviders.some((provider) => provider.id === "openai"))
      .toBe(false);
  });

  it("sends disabled thinking as a mode and omits thinking effort", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: { transformCallback: vi.fn(() => 1) }
    });
    const document = createSeedDocument();
    const provider = document.globalSettings.apiProviders[0];
    const model: ModelProfile = {
      id: "thinking-wire-model",
      name: "",
      group: "",
      capabilities: [],
      reasoningContent: "encrypted"
    };
    const response = {
      contexts: [],
      usage: {},
      model: model.id,
      providerName: provider.name,
      durationMs: 1
    };
    tauriMocks.invoke.mockResolvedValue(response);
    const request = {
      provider,
      model,
      reasoningEffort: "disabled" as const,
      conversationId: document.workspaces[0].conversations[0].id,
      workspacePath: document.workspaces[0].path,
      systemPrompt: "",
      enabledTools: [],
      contexts: [],
      tools: document.tools
    };

    await runModel(request, vi.fn(), "disabled-thinking");
    await runModel({ ...request, reasoningEffort: "high" }, vi.fn(), "high-thinking");

    const disabledRequest = (tauriMocks.invoke.mock.calls[0][1] as {
      request: Record<string, unknown>;
    }).request;
    expect(disabledRequest).toMatchObject({ thinkingMode: "disabled" });
    expect(disabledRequest).not.toHaveProperty("thinkingEffort");
    expect(disabledRequest).not.toHaveProperty("reasoningEffort");

    const highRequest = (tauriMocks.invoke.mock.calls[1][1] as {
      request: Record<string, unknown>;
    }).request;
    expect(highRequest).toMatchObject({ thinkingEffort: "high" });
    expect(highRequest).not.toHaveProperty("thinkingMode");
    expect(highRequest).not.toHaveProperty("reasoningEffort");
  });

  it("fetches the model catalog for a provider that is not enabled yet", async () => {
    // Fetching a catalog is configuration-time work: a provider must be reachable and
    // its models discovered before it can be enabled. An enabled-only gate would make
    // provider setup impossible and fail silently.
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const provider = { ...createSeedDocument().globalSettings.apiProviders[0], enabled: false };
    tauriMocks.invoke.mockResolvedValue([{ id: "catalog-model" }]);

    const models = await fetchModels(provider);

    expect(models.map((model) => model.id)).toEqual(["catalog-model"]);
    // Discovery resolves the protocol default rather than leaving it deferred:
    // this seed provider is the Responses family.
    expect(models[0].reasoningContent).toBe("encrypted");
    expect(tauriMocks.invoke).toHaveBeenCalledWith("fetch_models", { provider });
  });

  it("serves the built-in Claude Agent catalog in browser preview", async () => {
    // There is no `GET /models` behind a local CLI, so the preview mirrors the
    // host's built-in registry row for row instead of inventing placeholder ids.
    const provider = {
      ...createSeedDocument().globalSettings.apiProviders[0],
      family: "claude_agent" as const,
      baseUrl: ""
    };

    const models = await fetchModels(provider);

    expect(models.map((model) => model.id)).toEqual(CLAUDE_AGENT_REGISTRY.map((entry) => entry.id));
    expect(models.map((model) => model.contextWindow))
      .toEqual(CLAUDE_AGENT_REGISTRY.map((entry) => entry.contextWindow));
    expect(models.map((model) => model.maxOutputTokens))
      .toEqual(CLAUDE_AGENT_REGISTRY.map((entry) => entry.maxOutputTokens));
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
  });

  it("replaces stale tool descriptors and removes unsupported tool references", () => {
    const source = createSeedDocument();
    source.tools.push({
      name: "extinct-tool",
      label: "已下线工具",
      description: "旧占位工具",
      category: "orchestration",
      dangerous: false,
      parameters: []
    });
    defaultPresetSettings(source).enabledTools.push("extinct-tool", "missing-tool");
    source.workspaces[0].conversations[0].settings.enabledTools.push("extinct-tool", "missing-tool");

    const normalized = normalizeDocument(source);
    expect(normalized.tools.map((tool) => tool.name)).toEqual(
      createSeedDocument().tools.map((tool) => tool.name)
    );
    expect(defaultPresetSettings(normalized).enabledTools).not.toContain("extinct-tool");
    expect(normalized.workspaces[0].conversations[0].settings.enabledTools).not.toContain("missing-tool");
  });

  it("keeps a deliberate tool disable through normalization", () => {
    const document = createSeedDocument();
    defaultPresetSettings(document).enabledTools = defaultPresetSettings(document)
      .enabledTools.filter((name) => name !== "web_search");
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings.enabledTools = conversation.settings.enabledTools
      .filter((name) => name !== "playwright");

    const normalized = normalizeDocument(document);
    expect(defaultPresetSettings(normalized).enabledTools).not.toContain("web_search");
    expect(normalized.workspaces[0].conversations[0].settings.enabledTools).not.toContain("playwright");
  });

  it("preserves dynamically discovered MCP descriptors and their enablement", () => {
    // Rust persists MCP-discovered tools, including `inputSchema`, in `tools`.
    // Replacing them unconditionally with the seed catalog would silently lose both
    // descriptors and enablement.
    const source = createSeedDocument();
    source.tools.push({
      name: "mcp__probe__search",
      label: "MCP 搜索",
      description: "动态发现的 MCP 工具",
      category: "mcp",
      dangerous: false,
      parameters: [],
      inputSchema: { type: "object", properties: { query: { type: "string" } } }
    });
    source.workspaces[0].conversations[0].settings.enabledTools.push("mcp__probe__search");

    const normalized = normalizeDocument(source);
    const preserved = normalized.tools.find((tool) => tool.name === "mcp__probe__search");
    expect(preserved).toBeDefined();
    expect(preserved!.inputSchema).toEqual({
      type: "object",
      properties: { query: { type: "string" } }
    });
    expect(normalized.workspaces[0].conversations[0].settings.enabledTools)
      .toContain("mcp__probe__search");
  });

  it("keeps valid queued messages and rejects malformed queue records while normalizing", () => {
    const source = createSeedDocument() as unknown as AppDocument & {
      workspaces: Array<AppDocument["workspaces"][number] & {
        conversations: Array<Record<string, unknown>>
      }>
    };
    const image = {
      id: "a".repeat(64),
      name: "screen.png",
      mime: "image/png",
      width: 1280,
      height: 720,
      bytes: 4096
    };
    source.workspaces[0].conversations[0].queuedMessages = [
      {
        id: "queue-valid",
        content: "稍后执行",
        images: [image],
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "queue-image-only",
        content: "",
        images: [image],
        createdAt: "2026-07-24T00:00:01Z"
      },
      {
        id: "",
        content: "无效",
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "queue-invalid-time",
        content: "无效",
        createdAt: "not-a-date"
      }
    ];

    expect(normalizeDocument(source).workspaces[0].conversations[0].queuedMessages).toEqual([
      {
        id: "queue-valid",
        content: "稍后执行",
        images: [image],
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "queue-image-only",
        content: "",
        images: [image],
        createdAt: "2026-07-24T00:00:01Z"
      }
    ]);
  });

  it("serializes Tauri document saves and snapshots each queued value", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    let resolveFirst!: () => void;
    tauriMocks.invoke
      .mockImplementationOnce(() => new Promise<void>((resolve) => {
        resolveFirst = resolve;
      }))
      .mockResolvedValueOnce(undefined);
    const first = createSeedDocument();
    defaultPresetSettings(first).systemPrompt = "first snapshot";
    const second = createSeedDocument();
    defaultPresetSettings(second).systemPrompt = "second snapshot";

    const firstSave = saveDocument(first);
    defaultPresetSettings(first).systemPrompt = "mutated after queueing";
    const secondSave = saveDocument(second);

    await vi.waitFor(() => expect(tauriMocks.invoke).toHaveBeenCalledTimes(1));
    expect(tauriMocks.invoke.mock.calls[0][0]).toBe("save_document");
    expect(persistedPresetSettings(
      (tauriMocks.invoke.mock.calls[0][1] as { document: unknown }).document
    ).systemPrompt).toBe("first snapshot");
    resolveFirst();
    await firstSave;
    await secondSave;

    expect(tauriMocks.invoke).toHaveBeenCalledTimes(2);
    expect(persistedPresetSettings(
      (tauriMocks.invoke.mock.calls[1][1] as { document: unknown }).document
    ).systemPrompt).toBe("second snapshot");
  });

  it("waits for the native durability barrier after queued document saves", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockResolvedValue(undefined);

    await saveDocument(createSeedDocument());
    await flushDocumentSaves();

    expect(tauriMocks.invoke).toHaveBeenNthCalledWith(
      tauriMocks.invoke.mock.calls.length - 1,
      "save_document",
      expect.any(Object)
    );
    expect(tauriMocks.invoke).toHaveBeenLastCalledWith("flush_document_saves");
  });

  it("marks workflow-critical document saves as durable in the native command", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockResolvedValue(undefined);
    const document = createSeedDocument();

    await saveDocument(document, { immutableSnapshot: true, durable: true });

    expect(tauriMocks.invoke).toHaveBeenLastCalledWith("save_document", {
      document: expect.any(Object),
      durable: true
    });
  });

  it("cleans only the removed provider's browser key marker after the deletion is persisted", async () => {
    const document = createSeedDocument();
    const removed = document.globalSettings.apiProviders[0];
    const retained = document.globalSettings.apiProviders[1];
    await saveDocument(document);
    await saveApiKey(removed, "removed-key");
    await saveApiKey(retained, "retained-key");

    const next = {
      ...document,
      globalSettings: {
        ...document.globalSettings,
        apiProviders: document.globalSettings.apiProviders.filter((provider) => provider.id !== removed.id),
        activeProviderId: retained.id
      }
    };
    await saveDocument(next);

    expect(await getStoredApiKeyLength(removed.id)).toBeUndefined();
    expect(await getStoredApiKeyLength(retained.id)).toBe(12);
  });
});
