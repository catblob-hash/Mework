import type { TranslationFunction } from "../../i18n";
import { chatEndpointOf } from "../../lib/modelCapabilities";
import { CODEX_DEFAULT_BASE_URL } from "../../lib/codexProvider";
import type { EndpointType, FamilySetting, ProviderFamily } from "../../types";

/**
 * Options for the provider-family picker. Labels are protocol or vendor brands
 * and are not localized. `defaultBaseUrl` replaces an unmodified URL when the
 * family changes; an empty string means the provider has no fixed public URL.
 */
export const API_FORMAT_OPTIONS: ReadonlyArray<{ value: ProviderFamily; label: string; defaultBaseUrl: string }> = [
  { value: "openai_responses", label: "OpenAI Responses", defaultBaseUrl: "https://api.openai.com/v1" },
  { value: "openai_chat", label: "OpenAI Chat Completions", defaultBaseUrl: "https://api.openai.com/v1" },
  { value: "anthropic", label: "Anthropic Messages", defaultBaseUrl: "https://api.anthropic.com/v1" },
  { value: "google", label: "Google Gemini", defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta" },
  { value: "xai", label: "xAI", defaultBaseUrl: "https://api.x.ai/v1" },
  { value: "azure", label: "Azure OpenAI", defaultBaseUrl: "" },
  { value: "bedrock", label: "AWS Bedrock", defaultBaseUrl: "" },
  { value: "vertex", label: "Google Vertex AI", defaultBaseUrl: "" },
  { value: "openai_compatible", label: "OpenAI Compatible", defaultBaseUrl: "" }
];

/** The built-in OAuth provider is fixed rather than selectable in the add dialog. */
export const CODEX_FAMILY_OPTION = {
  value: "openai_codex" as const,
  label: "OpenAI Codex (ChatGPT)",
  defaultBaseUrl: CODEX_DEFAULT_BASE_URL,
};

/** The built-in local-CLI provider is fixed rather than selectable in the add dialog. */
export const CLAUDE_AGENT_FAMILY_OPTION = {
  value: "claude_agent" as const,
  label: "Claude Agent (Claude Code)",
  defaultBaseUrl: "",
};

/** The fixed option for a built-in family, or undefined for user-selectable families. */
export function builtinFamilyOption(family: ProviderFamily) {
  if (family === CODEX_FAMILY_OPTION.value) return CODEX_FAMILY_OPTION;
  if (family === CLAUDE_AGENT_FAMILY_OPTION.value) return CLAUDE_AGENT_FAMILY_OPTION;
  return undefined;
}

export function familyLabel(family: ProviderFamily): string {
  const builtin = builtinFamilyOption(family);
  if (builtin) return builtin.label;
  return API_FORMAT_OPTIONS.find((option) => option.value === family)?.label ?? family;
}

export function endpointLabel(t: TranslationFunction, endpoint: EndpointType): string {
  switch (endpoint) {
    case "openai_chat_completions": return t("对话补全", "Chat completions");
    case "openai_responses": return "Responses";
    case "anthropic_messages": return "Anthropic Messages";
    case "google_generative": return "Google generateContent";
    case "azure_openai": return "Azure OpenAI";
    case "bedrock_converse": return "Bedrock Converse";
    case "openai_image_generation": return t("图片生成", "Image generation");
    case "openai_image_edit": return t("图片编辑", "Image edit");
    case "openai_text_to_speech": return t("语音合成", "Text to speech");
    case "openai_audio_transcription": return t("语音识别", "Transcription");
  }
}

/**
 * Non-chat endpoints that can use dedicated URLs. Chat endpoints use the
 * provider Base URL through their dedicated input to avoid duplicate sources.
 */
export const NON_CHAT_ENDPOINTS: readonly EndpointType[] = [
  "openai_image_generation",
  "openai_image_edit",
  "openai_text_to_speech",
  "openai_audio_transcription"
];

/** Path segment that the host POSTs for this protocol. */
export function chatRequestPath(family: ProviderFamily): string {
  switch (chatEndpointOf(family)) {
    case "openai_responses": return "/responses";
    case "openai_chat_completions": return "/chat/completions";
    case "anthropic_messages": return "/messages";
    // Google, Azure, and Bedrock do not append a fixed suffix: their paths or
    // hosts depend on the model, deployment, API version, or region. Do not
    // display a guessed URL that could make users change a valid configuration.
    default: return "";
  }
}

/**
 * Endpoint suffixes the host removes from a pasted Base URL. Mirrors Rust
 * `strip_known_endpoint`; the paired forms are matched before the single ones.
 */
const PASTED_ENDPOINT_SUFFIXES: readonly (readonly string[])[] = [
  ["chat", "completions"],
  ["responses", "compact"],
  ["models"],
  ["responses"],
  ["messages"],
  ["completions"]
];

/** Base URL with a pasted endpoint suffix removed, matching host normalization. */
function withoutPastedEndpoint(baseUrl: string): string {
  const trimmed = baseUrl.trim().replace(/\/+$/u, "");
  if (!trimmed) return "";
  const segments = trimmed.split("/");
  for (const suffix of PASTED_ENDPOINT_SUFFIXES) {
    if (segments.length <= suffix.length) continue;
    const tail = segments.slice(-suffix.length).map((segment) => segment.toLowerCase());
    if (tail.every((segment, index) => segment === suffix[index])) {
      return segments.slice(0, -suffix.length).join("/");
    }
  }
  return trimmed;
}

/**
 * Full conversation request URL for a Base URL and protocol.
 *
 * The host strips a pasted endpoint suffix before the request goes out, so the
 * preview strips it too. Showing `.../v1/responses/responses` for a Base URL
 * that actually works would push users to "fix" a valid configuration.
 */
export function chatRequestPreview(baseUrl: string, family: ProviderFamily): string {
  const normalized = withoutPastedEndpoint(baseUrl);
  if (!normalized) return "";
  return `${normalized}${chatRequestPath(family)}`;
}

/** Labels and placeholders for family-specific identity fields. */
export function familySettingMeta(
  t: TranslationFunction,
  setting: FamilySetting
): { label: string; placeholder: string; hint: string } {
  switch (setting) {
    case "region":
      return {
        label: t("AWS 区域", "AWS region"),
        placeholder: "us-east-1",
        hint: t("Bedrock 的端点主机名由它决定。", "Bedrock derives its endpoint host from this."),
      };
    case "project":
      return {
        label: t("GCP 项目", "GCP project"),
        placeholder: "my-project-123456",
        hint: t("Vertex 的模型路径里带着它。", "Vertex puts this in the model path."),
      };
    case "location":
      return {
        label: t("GCP 区域", "GCP location"),
        placeholder: "us-central1",
        hint: t("Vertex 的端点主机名与模型路径都带着它。", "Vertex puts this in both the endpoint host and the model path."),
      };
    case "api_version":
      return {
        label: "api-version",
        placeholder: t("留空 = 用默认版本", "blank = provider default"),
        hint: t(
          "Azure 把它放在查询串里。留空时用 AI SDK 自带的默认版本——写一个没验证过的版本号比不写更糟。",
          "Azure passes this as a query parameter. Blank uses the AI SDK default; pinning an unverified version is worse than not pinning one."
        ),
      };
    case "claude_executable":
      return {
        label: t("Claude Code 路径", "Claude Code executable"),
        placeholder: t("留空 = 自动查找 ~/.local/bin 与 PATH", "blank = search ~/.local/bin and PATH"),
        hint: t(
          "只支持原生安装的 Claude Code（claude.exe / claude），npm 安装的 claude.cmd 不行。",
          "Only the native Claude Code install (claude.exe / claude) works; the npm claude.cmd shim does not."
        ),
      };
  }
}
