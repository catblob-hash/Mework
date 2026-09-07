import type { ProviderFamily, ApiProvider, AppDocument, ConversationPreset } from "../types";
import { implicitConversationPreset } from "./conversationPresets";

/** Fixed ID so repeated seeding replaces the same record rather than adding another. */
const MEMORY_E2E_PRESET_ID = "memory-e2e-full-access";

const PROTOCOLS = [
  "openai_responses",
  "openai_chat",
  "anthropic"
] as const satisfies readonly ProviderFamily[];

/** Provider families exercised by this E2E driver. */
type E2EFamily = (typeof PROTOCOLS)[number];

const DISPLAY_NAMES: Record<E2EFamily, string> = {
  openai_responses: "OpenAI Responses",
  openai_chat: "OpenAI Chat Completions",
  anthropic: "Anthropic Messages"
};

export interface MemoryE2ESeedConfig {
  runId: string;
  protocolBaseUrl: string;
}

export function validateMemoryE2ESeedConfig(
  config: MemoryE2ESeedConfig
): MemoryE2ESeedConfig {
  if (!/^[0-9a-f]{24}$/.test(config.runId)) {
    throw new Error("记忆 E2E run ID 必须是 12 字节随机值的小写十六进制编码");
  }
  let parsed: URL;
  try {
    parsed = new URL(config.protocolBaseUrl);
  } catch {
    throw new Error("记忆 E2E provider base URL 无效");
  }
  const canonical = `http://127.0.0.1:${parsed.port}/v1`;
  if (
    parsed.protocol !== "http:"
    || parsed.hostname !== "127.0.0.1"
    || !parsed.port
    || Number(parsed.port) < 1024
    || Number(parsed.port) > 65_535
    || parsed.pathname !== "/v1"
    || parsed.username
    || parsed.password
    || parsed.search
    || parsed.hash
    || parsed.href !== canonical
  ) {
    throw new Error("记忆 E2E provider 只能使用规范的高位 IPv4 loopback /v1 URL");
  }
  return { runId: config.runId, protocolBaseUrl: canonical };
}

export function memoryE2EProviderId(protocol: ProviderFamily, runId: string): string {
  return `memory-e2e-${protocol}-${runId}`;
}

export function memoryE2EProviders(config: MemoryE2ESeedConfig): ApiProvider[] {
  const validated = validateMemoryE2ESeedConfig(config);
  return PROTOCOLS.map((protocol) => {
    const modelIds = protocol === "openai_responses"
      ? ["kimi-k3", "Kimi-K3"]
      : ["kimi-k3"];
    return {
      id: memoryE2EProviderId(protocol, validated.runId),
      name: `Memory E2E · ${DISPLAY_NAMES[protocol]}`,
      enabled: true,
      endpointBaseUrls: {},
      familySettings: {},
      notes: "",
      family: protocol,
      baseUrl: validated.protocolBaseUrl,
      models: modelIds.map((id) => ({
        id,
        contextWindow: 128_000,
        maxOutputTokens: 8_192,
        name: "",
        group: "",
        capabilities: [],
        reasoningContent: protocol === "openai_responses" ? "encrypted" : "plaintext",
        promptCache: true
      })),
      activeModelId: "kimi-k3"
    };
  });
}

/**
 * Seeds only the random memory E2E application document. The caller owns the
 * isolation proof; this function intentionally does not create a workspace,
 * write a credential, or accept any filesystem path.
 */
export function seedMemoryE2EDocument(
  documentValue: AppDocument,
  config: MemoryE2ESeedConfig
): AppDocument {
  const next = typeof structuredClone === "function"
    ? structuredClone(documentValue)
    : JSON.parse(JSON.stringify(documentValue)) as AppDocument;
  const expectedProviders = memoryE2EProviders(config);
  const existingById = new Map(
    next.globalSettings.apiProviders.map((provider) => [provider.id, provider])
  );
  const providers = expectedProviders.map((provider) => {
    const existing = existingById.get(provider.id);
    const validModelIds = new Set(provider.models.map((model) => model.id));
    return {
      ...provider,
      activeModelId: existing?.activeModelId
        && validModelIds.has(existing.activeModelId)
        ? existing.activeModelId
        : provider.activeModelId
    };
  });
  const validProviderIds = new Set(providers.map((provider) => provider.id));
  next.globalSettings.appLanguage = "zh-CN";
  next.globalSettings.apiProviders = providers;
  next.globalSettings.activeProviderId = next.globalSettings.activeProviderId
    && validProviderIds.has(next.globalSettings.activeProviderId)
    ? next.globalSettings.activeProviderId
    : providers[0].id;
  // Security level is per preset or conversation. The E2E flow requires all
  // UI-created tasks to bypass approval, so seed a full_access preset as the
  // global default and apply it to existing conversation and workspace snapshots.
  // Remaining settings use built-in defaults.
  const preset: ConversationPreset = {
    id: MEMORY_E2E_PRESET_ID,
    name: "Memory E2E · 完全访问",
    description: "记忆 E2E 专用：新任务免审批",
    settings: {
      ...implicitConversationPreset(next.tools, "zh-CN").settings,
      securityLevel: "full_access"
    }
  };
  next.globalSettings.conversationPresets = [
    preset,
    ...next.globalSettings.conversationPresets.filter((item) => item.id !== preset.id)
  ];
  next.globalSettings.defaultConversationPresetId = preset.id;
  next.workspaces = next.workspaces.map((workspace) => ({
    ...workspace,
    lastConversationSettings: workspace.lastConversationSettings
      ? { ...workspace.lastConversationSettings, securityLevel: "full_access" }
      : null,
    conversations: workspace.conversations.map((conversation) => ({
      ...conversation,
      settings: { ...conversation.settings, securityLevel: "full_access" }
    }))
  }));
  return next;
}
