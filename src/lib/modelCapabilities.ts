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

/** Endpoints capable of a conversation. Image and audio models are excluded from the chat selector. */
export const CHAT_ENDPOINT_TYPES: readonly EndpointType[] = [
  "openai_chat_completions",
  "openai_responses",
  "anthropic_messages",
  "google_generative",
  "azure_openai",
  "bedrock_converse",
];

/**
 * Endpoints with an implemented call path. Configurable-only endpoints must be
 * described accurately in settings rather than presented as usable.
 */
export function isCallableEndpoint(endpoint: EndpointType): boolean {
  return CHAT_ENDPOINT_TYPES.includes(endpoint);
}

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
    // The Claude Code path is optional: an empty value lets the host search the
    // standard install locations.
    case "claude_agent":
      return ["claude_executable"];
    default:
      return [];
  }
}

/** Capability catalog in chip-rendering order, matching Rust `ModelCapability::CATALOG`. */
export const MODEL_CAPABILITIES: readonly ModelCapability[] = [
  "image_recognition",
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
