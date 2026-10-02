import type {
  EndpointType,
  FamilySetting,
  ModelCapability,
  ModelProfile,
  ProviderFamily,
  ReasoningContent,
  ReasoningContext,
} from "../types";

/**
 * Endpoint-type catalog in the same order as Rust `EndpointType::CATALOG`.
 * `modelCapabilities.test.ts` compares both sides exactly.
 */
export const ENDPOINT_TYPES: readonly EndpointType[] = [
  "openai_chat_completions",
  "openai_responses",
  "anthropic_messages",
  "google_generative",
  "azure_openai",
  "bedrock_converse",
  "openai_image_generation",
  "openai_image_edit",
  "openai_text_to_speech",
  "openai_audio_transcription",
];

/** Adapter family to chat endpoint type. Mirrors Rust `ProviderFamily::chat_endpoint`. */
export function chatEndpointOf(family: ProviderFamily): EndpointType {
  switch (family) {
    case "openai_responses":
    case "openai_codex":
      return "openai_responses";
    // xAI and the generic compatibility layer use the `/chat/completions` shape.
    case "openai_chat":
    case "xai":
    case "openai_compatible":
      return "openai_chat_completions";
    // The Claude Code CLI speaks Messages upstream, so the endpoint type is a
    // model vocabulary here rather than a request path the host issues itself.
    case "anthropic":
    case "claude_agent":
      return "anthropic_messages";
    case "google":
    case "vertex":
      return "google_generative";
    case "azure":
      return "azure_openai";
    case "bedrock":
      return "bedrock_converse";
  }
}

/** Required identity fields for this family. Mirrors Rust `ProviderFamily::required_settings`. */
export function requiredFamilySettings(family: ProviderFamily): readonly FamilySetting[] {
  switch (family) {
    case "bedrock":
      return ["region"];
    case "vertex":
      return ["project", "location"];
    default:
      return [];
  }
}

/**
 * Whether the chat base URL is derived from identity fields. Mirrors Rust
 * `ProviderFamily::derives_base_url`. Vertex and Bedrock derive endpoints from their
 * identity fields, Codex derives its default endpoint from the ChatGPT backend, and
 * Claude Agent lets the local CLI pick the endpoint, so an empty Base URL remains valid.
 */
export function derivesBaseUrl(family: ProviderFamily): boolean {
  return family === "vertex"
    || family === "bedrock"
    || family === "openai_codex"
    || family === "claude_agent";
}

/** Whether this provider has a usable chat endpoint. */
export function hasUsableBaseUrl(provider: { family: ProviderFamily; baseUrl: string }): boolean {
  return provider.baseUrl.trim().length > 0 || derivesBaseUrl(provider.family);
}

/**
 * Identity fields recognized by this family, including optional fields. Settings use
 * this to determine which inputs to display. Mirrors Rust `ProviderFamily::known_settings`.
 */
export function knownFamilySettings(family: ProviderFamily): readonly FamilySetting[] {
  switch (family) {
    case "bedrock":
      return ["region"];
    case "vertex":
      return ["project", "location"];
    // Azure's `api_version` is optional because the AI SDK provides a default.
    case "azure":
      return ["api_version"];
    // Claude Agent has no identity fields; Mework bundles and version-locks the
    // executable, which the host locates itself.
    case "claude_agent":
    default:
      return [];
  }
}

/** Capability catalog in chip-rendering order, matching Rust `ModelCapability::CATALOG`. */
export const MODEL_CAPABILITIES: readonly ModelCapability[] = [
  "image_recognition",
  "tool_append",
  "system_append",
];

/** Reasoning-form catalog in selector-rendering order, matching Rust `ReasoningContent::CATALOG`. */
export const REASONING_CONTENTS: readonly ReasoningContent[] = [
  "plaintext",
  "encrypted",
];

export function supportsVision(model: ModelProfile): boolean {
  return model.capabilities.includes("image_recognition");
}

/** De-duplicated capabilities in catalog order, for persistence and rendering. */
export function normalizeCapabilities(values: readonly unknown[]): ModelCapability[] {
  const requested = new Set(values.filter((value): value is ModelCapability => (
    typeof value === "string" && (MODEL_CAPABILITIES as readonly string[]).includes(value)
  )));
  return MODEL_CAPABILITIES.filter((capability) => requested.has(capability));
}

/** De-duplicated endpoint types in catalog order, for `ApiProvider.endpointBaseUrls`. */
export function normalizeEndpointTypes(values: readonly unknown[]): EndpointType[] {
  const requested = new Set(values.filter((value): value is EndpointType => (
    typeof value === "string" && (ENDPOINT_TYPES as readonly string[]).includes(value)
  )));
  return ENDPOINT_TYPES.filter((endpoint) => requested.has(endpoint));
}

/**
 * Persistence form of reasoning content. The field is concrete on every model, so
 * an unrecognized or absent value — including the retired `"auto"` in an older
 * document — resolves against the family here rather than staying deferred.
 */
export function normalizeReasoningContent(
  value: unknown,
  family: ProviderFamily
): ReasoningContent {
  if (typeof value === "string" && (REASONING_CONTENTS as readonly string[]).includes(value)) {
    return value as ReasoningContent;
  }
  return reasoningContentTakesEffect(family) ? "encrypted" : "plaintext";
}

/**
 * Whether this family has a real consumer for {@link ReasoningContent}. Mirrors Rust
 * `ProviderFamily::reasoning_content_takes_effect`. Responses-family providers,
 * including Codex, and Azure use this setting to determine `include`; for other
 * families the upstream decides and the stored value is descriptive.
 */
export function reasoningContentTakesEffect(family: ProviderFamily): boolean {
  return family === "openai_responses" || family === "openai_codex" || family === "azure";
}

/**
 * Persistence form of the prompt-cache attribute. A boolean has no family-derived
 * default: anything that is not a boolean, including the absent key of an older
 * document, resolves to Claude Code's default of enabled.
 */
export function normalizePromptCache(value: unknown): boolean {
  return typeof value === "boolean" ? value : true;
}

/**
 * Whether this family's dialect places prompt-cache breakpoints, so the model's
 * `promptCache` attribute reaches the wire. Mirrors Rust
 * `ProviderFamily::prompt_cache_takes_effect`: only the Messages protocol takes
 * explicit `cache_control` markers.
 */
export function promptCacheTakesEffect(family: ProviderFamily): boolean {
  return family === "anthropic";
}

/** Models the Messages API takes `tool_addition` from. Mirrors Rust `ANTHROPIC_TOOL_CHANGE_MODELS`. */
const ANTHROPIC_TOOL_CHANGE_MODELS = ["fable-5", "mythos-5", "opus-5", "opus-4-8", "sonnet-5-5"] as const;
/** Earlier Claude models the same documentation leaves out. Mirrors Rust `ANTHROPIC_EARLIER_MODELS`. */
const ANTHROPIC_EARLIER_MODELS = [
  "claude-instant", "claude-2", "claude-3", "opus-4", "sonnet-4", "haiku-4", "haiku-3",
] as const;
/** Current models documented without it, as released. Mirrors Rust `ANTHROPIC_RELEASES_WITHOUT`. */
const ANTHROPIC_RELEASES_WITHOUT = ["sonnet-5"] as const;
/** Models Claude Code appends tools for itself. Mirrors Rust `CLAUDE_CODE_TOOL_CHANGE_MODELS`. */
const CLAUDE_CODE_TOOL_CHANGE_MODELS = ["fable-5", "mythos-5", "opus-5", "opus-4-8"] as const;

/**
 * Whether `modelId` names one of `families`, followed by whatever `restOk`
 * accepts, the family appearing whole (at the start or after a `-`). Case,
 * `.` for `-` and Claude Code's `[1m]` suffix do not matter. Mirrors Rust
 * `tool_append::names_model_with`.
 */
function namesModelWith(modelId: string, families: readonly string[], restOk: (rest: string) => boolean): boolean {
  const id = modelId.toLowerCase().replaceAll(".", "-").replace(/\[1m\]$/, "");
  return families.some((family) => {
    let at = id.indexOf(family);
    while (at !== -1) {
      if ((at === 0 || id[at - 1] === "-") && restOk(id.slice(at + family.length))) return true;
      at = id.indexOf(family, at + 1);
    }
    return false;
  });
}

/** The family followed by the end of the id or another `-` segment. Mirrors Rust `names_model`. */
function namesModel(modelId: string, families: readonly string[]): boolean {
  return namesModelWith(modelId, families, (rest) => rest === "" || rest.startsWith("-"));
}

/** The family as released: the end of the id or a dated snapshot. Mirrors Rust `names_release`. */
function namesRelease(modelId: string, families: readonly string[]): boolean {
  return namesModelWith(modelId, families, (rest) => rest === "" || /^-\d{8}$/u.test(rest));
}

/**
 * Whether `baseUrl` is the vendor's own endpoint for `family`; empty is the
 * provider's default, which is. Mirrors Rust `host_append::at_vendor_endpoint`.
 */
export function atVendorEndpoint(family: ProviderFamily, baseUrl: string): boolean {
  const trimmed = baseUrl.trim();
  if (!trimmed) return true;
  let host: string;
  try {
    host = new URL(trimmed).hostname.toLowerCase();
  } catch {
    return false;
  }
  switch (family) {
    case "anthropic":
      return host === "api.anthropic.com";
    case "openai_responses":
    case "openai_codex":
    case "azure":
    case "openai_chat":
      return host === "api.openai.com"
        || host === "chatgpt.com"
        || [".openai.azure.com", ".cognitiveservices.azure.com", ".services.ai.azure.com"]
          .some((suffix) => host.endsWith(suffix));
    default:
      return false;
  }
}

/** What Anthropic documents for this model at its own endpoint. Mirrors Rust `tool_append::anthropic_documents`. */
function anthropicDocuments(modelId: string): boolean | null {
  if (namesModel(modelId, ANTHROPIC_TOOL_CHANGE_MODELS)) return true;
  if (namesModel(modelId, ANTHROPIC_EARLIER_MODELS) || namesRelease(modelId, ANTHROPIC_RELEASES_WITHOUT)) return false;
  return null;
}

/**
 * What Mework knows about this model taking a tool mid-conversation at this
 * endpoint; `null` is the user's to say (a relay, a model the vendor's
 * documentation does not cover). Mirrors Rust `tool_append::known`.
 */
export function knownToolAppend(family: ProviderFamily, baseUrl: string, modelId: string): boolean | null {
  const vendor = atVendorEndpoint(family, baseUrl);
  switch (family) {
    case "openai_codex":
      return true;
    case "openai_responses":
    case "azure":
      return vendor ? true : null;
    case "anthropic":
      return vendor ? anthropicDocuments(modelId) : null;
    case "claude_agent":
      return namesModel(modelId, CLAUDE_CODE_TOOL_CHANGE_MODELS);
    default:
      return false;
  }
}

/**
 * What Mework knows about this model taking a system message mid-conversation
 * at this endpoint; `null` is the user's to say. Mirrors Rust
 * `system_append::known`.
 */
export function knownSystemAppend(family: ProviderFamily, baseUrl: string, modelId: string): boolean | null {
  const vendor = atVendorEndpoint(family, baseUrl);
  switch (family) {
    case "openai_responses":
    case "openai_codex":
    case "azure":
      return true;
    case "openai_chat":
      return vendor ? true : null;
    case "anthropic":
      return vendor ? anthropicDocuments(modelId) : null;
    case "openai_compatible":
    case "xai":
      return null;
    default:
      return false;
  }
}

/**
 * The append capabilities Mework declares for a model it fills in itself:
 * those it knows the model has at this endpoint. Mirrors Rust
 * `model_discovery::known_append_capabilities`.
 */
export function knownAppendCapabilities(
  provider: { family: ProviderFamily; baseUrl: string },
  modelId: string
): ModelCapability[] {
  return [
    ...(knownToolAppend(provider.family, provider.baseUrl, modelId) === true ? ["tool_append" as const] : []),
    ...(knownSystemAppend(provider.family, provider.baseUrl, modelId) === true ? ["system_append" as const] : []),
  ];
}

/**
 * Whether this family has a tool-append interface, so the `tool_append`
 * capability is read. Mirrors Rust `ProviderFamily::tool_append_takes_effect`.
 */
export function toolAppendTakesEffect(family: ProviderFamily): boolean {
  return family === "anthropic"
    || family === "openai_responses"
    || family === "openai_codex"
    || family === "azure"
    || family === "claude_agent";
}

/**
 * Whether this family can carry a system message mid-conversation, so the
 * `system_append` capability is read. Mirrors Rust
 * `ProviderFamily::system_append_takes_effect`.
 */
export function systemAppendTakesEffect(family: ProviderFamily): boolean {
  return family === "anthropic"
    || family === "openai_responses"
    || family === "openai_codex"
    || family === "azure"
    || family === "openai_chat"
    || family === "openai_compatible"
    || family === "xai";
}

/**
 * Whether a conversation on this model can take a tool mid-conversation at all:
 * the model declares `tool_append` — filled in by Mework where it knows, by the
 * user everywhere else — and its protocol has an append interface. Where it
 * cannot, the conversation's tool surface is fixed from its first request on —
 * the settings lock it gray — and auto-compact and MCP tool discovery, which
 * both add tools mid-run, are unavailable. Mirrors Rust `tool_append::appends_tools`.
 */
export function appendsTools(
  provider: { family: ProviderFamily },
  model: { capabilities?: readonly ModelCapability[] }
): boolean {
  return toolAppendTakesEffect(provider.family) && (model.capabilities ?? []).includes("tool_append");
}

/**
 * Whether a reasoning card is encrypted and therefore removable but not editable.
 * `form` is authoritative. Its absence falls back to an empty body for legacy cards;
 * all reads must use this function to keep the fallback consistent. Streaming cards
 * are already non-editable, and settlement writes the definitive `form`.
 */
export function isEncryptedReasoning(item: Pick<ReasoningContext, "form" | "content">): boolean {
  return item.form === undefined ? (item.content ?? "").length === 0 : item.form === "encrypted";
}

/** Display name, falling back to the ID. Mirrors Rust `ModelProfile::display_name`. */
export function modelDisplayName(model: ModelProfile): string {
  return model.name.trim() || model.id;
}

/**
 * Repairs `activeModelId` to a model that still exists. Presence in the provider's
 * list is the whole of usability now, but a removed model can still leave the id
 * dangling, which storage validation rejects on save.
 */
export function repairActiveModelId(provider: {
  models: ModelProfile[];
  activeModelId: string | null;
}): string | null {
  const current = provider.models.find((model) => model.id === provider.activeModelId);
  if (current) return current.id;
  return provider.models[0]?.id ?? null;
}

/**
 * Derives a model group when `group` is empty. This must match Rust
 * `derive_model_group_name`: slash-separated IDs use their first segment, while flat
 * IDs use their family prefix. Matching rules keep discovered and legacy models grouped
 * consistently.
 */
export function modelGroup(model: ModelProfile): string {
  const explicit = model.group.trim();
  if (explicit) return explicit;
  const id = model.id.trim();
  if (!id) return "";
  if (id.includes("/")) return id.slice(0, id.indexOf("/")).trim();
  const family = id.slice(0, id.indexOf("-") === -1 ? id.length : id.indexOf("-")).trim();
  return family === id ? "" : family;
}
