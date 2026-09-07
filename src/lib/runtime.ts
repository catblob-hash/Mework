import { Channel, hasBackendRuntime, invoke, isBrowserDevRuntime } from "./backend";
import { createId } from "./id";
import {
  createTemporaryWorkspace,
  TEMPORARY_WORKSPACE_ID
} from "./workspaces";
import { createSeedDocument } from "../seed";
import {
  emptyConversationPresetSettings
} from "./conversationPresets";
import { isHostDerivedToolName } from "./taskTools";
import {
  isAgentToolNameList,
  validateAgentTypeSlug
} from "./agentDefinitions";
import type {
  AgentDefinition,
  AppLanguage,
  AppReleaseAsset,
  AppUpdateCheck,
  AppUpdateDownload,
  AppUpdateDownloadEvent,
  AppUpdateInstallOutcome,
  AppVersionInfo,
  ProviderFamily,
  ApiKeyStatus,
  AppearancePreferences,
  CodexOauthStatus,
  ClaudeAgentLoginStatus,
  ApiProvider,
  AppDocument,
  EndpointType,
  CapabilityCatalog,
  ContextItem,
  Conversation,
  ConversationPlan,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings,
  ConversationWorktree,
  ConversationWebSearchSettings,
  EnvironmentToolDefinition,
  EnvironmentToolSnapshot,
  ExecutionEnvironmentAssets,
  GlobalSettings,
  ImageAttachment,
  KeyToken,
  McpProbeReport,
  McpServerConfig,
  McpTransportKind,
  ModelProfile,
  ModelRunRequest,
  ResolvedAppLanguage,
  RunTarget,
  ModelRunResponse,
  ModelStreamEvent,
  PendingForkRequest,
  PendingToolPrompt,
  QueuedMessage,
  ReasoningEffort,
  SecurityLevel,
  ShortcutCommandId,
  ShortcutPreference,
  SkillRecord,
  SkillSearchReport,
  SkillSource,
  SubagentRunRecord,
  SshMachineConfig,
  SystemSkillCandidate,
  ThemePreference,
  ToolDescriptor,
  ToolExecutionRequest,
  WslDistro,
  ToolExecutionResponse,
  ToolApprovalGrant,
  ToolPromptDecision,
  WebSearchAssets,
  FamilySetting,
} from "../types";
import { estimateContextsTokens } from "./contextTokens";
import { SEARCH_PROVIDERS, isKnownSearchProvider, searchProviderSupports } from "./searchProviders";
import {
  knownFamilySettings,
  normalizeCapabilities,
  normalizeEndpointTypes,
  normalizeReasoningContent,
} from "./modelCapabilities";
import {
  clampMessageFontSize,
  clampZoom,
  normalizeHexColor
} from "./appearance";
import { SHORTCUT_COMMANDS, isValidBinding, orderBinding } from "./shortcuts";
import { CLAUDE_AGENT_REGISTRY, ensureClaudeAgentProvider } from "./claudeAgentProvider";
import { ensureCodexProvider } from "./codexProvider";

const STORAGE_KEY = "mework.document.v1";
const API_KEY_LENGTH_PREFIX = "mework.api-key-length.v1.";
const IMAGE_ATTACHMENT_STORAGE_PREFIX = "mework.image-attachment.v1.";
const IMAGE_ATTACHMENT_INDEX_KEY = "mework.image-attachment-index.v1";
const IMAGE_ATTACHMENT_MAX_BYTES = 5 * 1024 * 1024;
const IMAGE_ATTACHMENT_MAX_NAME_BYTES = 256;
const IMAGE_ATTACHMENT_MAX_DIMENSION = 8_000;
const IMAGE_ATTACHMENT_MAX_PIXELS = 16 * 1024 * 1024;
const PREVIEW_IMAGE_MAX_BYTES = 3 * 1024 * 1024;
const PREVIEW_IMAGE_STORAGE_CHARACTERS = Math.floor(4.5 * 1024 * 1024);
const PREVIEW_IMAGE_STORAGE_COUNT = 32;
const PREVIEW_IMAGE_ORPHAN_GRACE_MS = 60 * 60 * 1000;
const API_FORMATS = new Set<ProviderFamily>([
  "openai_responses",
  "openai_codex",
  "openai_chat",
  "anthropic",
  "claude_agent",
  "google",
  "xai",
  "azure",
  "bedrock",
  "vertex",
  "openai_compatible",
]);
const REASONING_EFFORTS = new Set<ReasoningEffort>(["disabled", "low", "medium", "high", "xhigh"]);
type PreviewRunControl = {
  cancelled: boolean;
  steers: QueuedMessage[];
};
const previewRunCancellations = new Map<string, PreviewRunControl>();
const SECURITY_LEVELS = new Set<SecurityLevel>(["request_approval", "allow_edits", "plan", "full_access"]);
const APP_LANGUAGES = new Set<AppLanguage>(["auto", "zh-CN", "en-US"]);
const RESOLVED_APP_LANGUAGES = new Set<ResolvedAppLanguage>(["zh-CN", "en-US"]);
const THEME_PREFERENCES = new Set<ThemePreference>(["day", "night", "system"]);
function uniqueStringIds(value: unknown): string[] {
  return [...new Set(Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string" && Boolean(item.trim()))
    : [])];
}


function optionalPresetId(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

/** Tool descriptions are discovered JSON files; the document stores only the selected asset ID. */
function normalizeToolDescriptionFileId(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function normalizeConversationPresetSettings(
  value: unknown,
  fallback: ConversationPresetSettings,
  knownToolNames: ReadonlySet<string>
): ConversationPresetSettings {
  const input = record(value) ?? {};
  const enabledTools = Array.isArray(input.enabledTools)
    ? uniqueStringIds(input.enabledTools)
        .filter((name) => knownToolNames.has(name) && !isHostDerivedToolName(name))
    : [...fallback.enabledTools];
  return {
    systemPrompt: typeof input.systemPrompt === "string" ? input.systemPrompt : fallback.systemPrompt,
    enabledTools,
    toolDescriptionFileId: normalizeToolDescriptionFileId(input.toolDescriptionFileId)
      ?? fallback.toolDescriptionFileId,
    agentDefinitions: normalizeAgentDefinitions(
      input.agentDefinitions,
      fallback.agentDefinitions
    ),
    // Absence means false: a role is required, so a silent preset must not route
    // subagents back to the primary conversation model.
    allowRolelessSubagents: input.allowRolelessSubagents === true,
    hookIds: Array.isArray(input.hookIds) ? uniqueStringIds(input.hookIds) : [...fallback.hookIds],
    skillIds: Array.isArray(input.skillIds) ? uniqueStringIds(input.skillIds) : [...fallback.skillIds],
    mcpIds: Array.isArray(input.mcpIds) ? uniqueStringIds(input.mcpIds) : [...fallback.mcpIds],
    webSearch: normalizeConversationWebSearch(input.webSearch, fallback.webSearch),
    securityLevel: normalizeSecurityLevel(input.securityLevel, fallback.securityLevel),
    // Absent means off for both tiers. Memory reads and writes files on disk,
    // so an unstated preset opts in to nothing.
    globalMemoryEnabled: input.globalMemoryEnabled === true,
    projectMemoryEnabled: input.projectMemoryEnabled === true,
    skillToolEnabled: input.skillToolEnabled === true
  };
}

function normalizeConversationPreset(
  value: unknown,
  fallbackSettings: ConversationPresetSettings,
  knownToolNames: ReadonlySet<string>
): ConversationPreset | null {
  const input = record(value);
  const id = optionalPresetId(input?.id);
  if (!input || !id) return null;
  return {
    id,
    name: typeof input.name === "string" ? input.name : "",
    description: typeof input.description === "string" ? input.description : "",
    settings: normalizeConversationPresetSettings(
      input.settings, fallbackSettings, knownToolNames
    )
  };
}

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function optionalFiniteNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function optionalPositiveInteger(value: unknown): number | undefined {
  const number = optionalFiniteNumber(value);
  return number === undefined ? undefined : Math.max(1, Math.floor(number));
}

function optionalNonNegativeInteger(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : undefined;
}


function normalizeImageAttachments(value: unknown): ImageAttachment[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const images = value.flatMap((entry) => {
    const image = record(entry);
    const width = optionalNonNegativeInteger(image?.width);
    const height = optionalNonNegativeInteger(image?.height);
    const bytes = optionalNonNegativeInteger(image?.bytes);
    const shortId = optionalNonNegativeInteger(image?.shortId);
    const name = typeof image?.name === "string" ? image.name.trim() : "";
    const pixels = width === undefined || height === undefined ? 0 : width * height;
    if (
      !image
      || typeof image.id !== "string"
      || !/^[0-9a-f]{64}$/.test(image.id)
      || !isValidImageAttachmentName(name)
      || typeof image.mime !== "string"
      || !["image/png", "image/jpeg", "image/gif", "image/webp"].includes(image.mime)
      || width === undefined
      || width === 0
      || width > IMAGE_ATTACHMENT_MAX_DIMENSION
      || height === undefined
      || height === 0
      || height > IMAGE_ATTACHMENT_MAX_DIMENSION
      || !Number.isSafeInteger(pixels)
      || pixels > IMAGE_ATTACHMENT_MAX_PIXELS
      || bytes === undefined
      || bytes === 0
      || bytes > IMAGE_ATTACHMENT_MAX_BYTES
    ) return [];
    return [{
      id: image.id,
      name,
      mime: image.mime,
      width,
      height,
      bytes,
      // Zero is not a valid conversation number; drop it rather than surface a broken placeholder.
      ...(shortId !== undefined && shortId >= 1 ? { shortId } : {})
    }];
  }).filter((image, index, all) => all.findIndex((candidate) => candidate.id === image.id) === index);
  return images.length ? images : undefined;
}

function normalizeReasoningEffort(value: unknown, fallback: ReasoningEffort = "disabled"): ReasoningEffort {
  return typeof value === "string" && REASONING_EFFORTS.has(value as ReasoningEffort)
    ? value as ReasoningEffort
    : fallback;
}

function normalizeSecurityLevel(value: unknown, fallback: SecurityLevel = "request_approval"): SecurityLevel {
  return typeof value === "string" && SECURITY_LEVELS.has(value as SecurityLevel)
    ? value as SecurityLevel
    : fallback;
}

/**
 * Normalize a conversation worktree only when path, branch, and baseline are valid.
 * Falling back to the workspace root is safe when a worktree cannot be released or
 * checked for additional commits.
 */
function normalizeConversationWorktree(value: unknown): ConversationWorktree | null {
  const worktree = record(value);
  if (!worktree) return null;
  const { path, branch, baseOid } = worktree;
  if (
    typeof path !== "string" || !path.trim() || path.length > 4096
    || typeof branch !== "string" || !branch.trim() || branch.length > 512
    || typeof baseOid !== "string" || !/^[0-9a-f]{7,64}$/.test(baseOid)
  ) return null;
  return { path, branch, baseOid };
}

/**
 * Normalize only the run-target shape. The host rejects missing SSH machines at
 * dispatch time so dangling bindings can remain persisted.
 *
 * Distro validation must match `validate_wsl_distro_name`; accepting a broader form
 * would produce a document that loads but cannot be saved by Rust.
 */
const WSL_DISTRO_PATTERN = /^[\p{L}\p{N}](?:[\p{L}\p{N}._ -]{0,62}[\p{L}\p{N}._-])?$/u;

/** Mirrors host startup-environment names that would run a script before each command. */
const RESERVED_ENV_VAR_NAMES = new Set([
  "BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS", "CDPATH", "GLOBIGNORE", "GIT_EXTERNAL_DIFF"
]);

function normalizeRunTarget(value: unknown): RunTarget | null {
  const target = record(value);
  if (!target) return null;
  if (target.kind === "wsl") {
    const distro = target.distro;
    if (typeof distro !== "string" || !WSL_DISTRO_PATTERN.test(distro)) return null;
    return { kind: "wsl", distro };
  }
  if (target.kind === "ssh") {
    const machineId = target.machineId;
    if (typeof machineId !== "string" || !machineId.trim() || machineId.length > 128) return null;
    return { kind: "ssh", machineId };
  }
  return null;
}

/** Host validation of execution environments is authoritative; discard malformed entries here. */
function normalizeExecutionEnvironments(
  value: unknown,
  fallback: ExecutionEnvironmentAssets
): ExecutionEnvironmentAssets {
  const input = record(value);
  if (!input) {
    return {
      sshMachines: fallback.sshMachines.map((machine) => ({ ...machine })),
      envVars: Object.fromEntries(
        Object.entries(fallback.envVars).map(([key, table]) => [key, { ...table }])
      )
    };
  }
  const now = new Date().toISOString();
  const seen = new Set<string>();
  // Match host validation so every loaded document remains saveable.
  const controlChars = /[\u0000-\u001f\u007f]/;
  const sshMachines: SshMachineConfig[] = (Array.isArray(input.sshMachines) ? input.sshMachines : [])
    .flatMap((entry) => {
      const machine = record(entry);
      if (
        !machine
        || typeof machine.id !== "string" || !machine.id.trim() || machine.id.length > 128
        || typeof machine.name !== "string" || !machine.name.trim()
        || typeof machine.host !== "string" || !machine.host.trim()
      ) return [];
      if (seen.has(machine.id)) return [];
      const host = machine.host.trim();
      // The host is one SSH argv element; whitespace and control characters are
      // invalid, and a leading `-` would be parsed as an option.
      if (host.length > 512 || /\s/.test(host) || controlChars.test(host) || host.startsWith("-")) {
        return [];
      }
      const identityFile = typeof machine.identityFile === "string" ? machine.identityFile : "";
      const remoteCwd = typeof machine.remoteCwd === "string" ? machine.remoteCwd : "";
      if ([identityFile, remoteCwd].some((value) => (
        value.length > 4096 || controlChars.test(value)
      ))) return [];
      seen.add(machine.id);
      const port = typeof machine.port === "number" && Number.isInteger(machine.port)
        && machine.port >= 0 && machine.port <= 65535 ? machine.port : 0;
      return [{
        id: machine.id,
        name: machine.name.trim().slice(0, 64),
        host,
        port,
        identityFile,
        remoteCwd,
        createdAt: typeof machine.createdAt === "string" && machine.createdAt ? machine.createdAt : now,
        updatedAt: typeof machine.updatedAt === "string" && machine.updatedAt ? machine.updatedAt : now
      }];
    })
    .slice(0, 64);
  const envVars: Record<string, Record<string, string>> = {};
  const tables = record(input.envVars) ?? {};
  for (const [key, tableValue] of Object.entries(tables).slice(0, 256)) {
    // Keys must name one of the three environments. A dangling `ssh:<id>` remains
    // valid so a deleted machine does not prevent document persistence.
    const validKey = key === "local"
      || (key.startsWith("wsl:") && WSL_DISTRO_PATTERN.test(key.slice(4)))
      || (key.startsWith("ssh:") && key.slice(4).trim().length > 0 && key.slice(4).length <= 128);
    if (!validKey) continue;
    const table = record(tableValue);
    if (!table) continue;
    const normalized: Record<string, string> = {};
    for (const [name, value] of Object.entries(table)) {
      if (typeof value !== "string") continue;
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || name.length > 128) continue;
      if (RESERVED_ENV_VAR_NAMES.has(name)) continue;
      const upper = name.toUpperCase();
      const harnessPrivate = (upper.startsWith("MEWORK_") || upper.startsWith("VITE_"))
        && (upper.includes("BROWSER_DEV") || upper.includes("E2E"));
      if (harnessPrivate) continue;
      if (value.length > 8192 || controlChars.test(value)) continue;
      if (Object.keys(normalized).length >= 128) break;
      normalized[name] = value;
    }
    envVars[key] = normalized;
  }
  return { sshMachines, envVars };
}

function normalizeAppLanguage(value: unknown, fallback: AppLanguage): AppLanguage {
  return typeof value === "string" && APP_LANGUAGES.has(value as AppLanguage)
    ? value as AppLanguage
    : fallback;
}

function normalizeResolvedAppLanguage(
  value: unknown,
  fallback: ResolvedAppLanguage
): ResolvedAppLanguage {
  return typeof value === "string" && RESOLVED_APP_LANGUAGES.has(value as ResolvedAppLanguage)
    ? value as ResolvedAppLanguage
    : fallback;
}

function normalizeThemePreference(value: unknown, fallback: ThemePreference): ThemePreference {
  return typeof value === "string" && THEME_PREFERENCES.has(value as ThemePreference)
    ? value as ThemePreference
    : fallback;
}

function normalizeModel(value: unknown, family: ProviderFamily): ModelProfile | null {
  const input = record(value);
  if (!input || typeof input.id !== "string" || !input.id.trim()) return null;
  return {
    id: input.id.trim(),
    name: typeof input.name === "string" ? input.name.trim() : "",
    group: typeof input.group === "string" ? input.group.trim() : "",
    contextWindow: optionalPositiveInteger(input.contextWindow),
    maxOutputTokens: optionalPositiveInteger(input.maxOutputTokens),
    capabilities: normalizeCapabilities(Array.isArray(input.capabilities) ? input.capabilities : []),
    reasoningContent: normalizeReasoningContent(input.reasoningContent, family)
  };
}

function normalizeEndpointBaseUrls(value: unknown): Partial<Record<EndpointType, string>> {
  const input = record(value);
  const result: Partial<Record<EndpointType, string>> = {};
  if (!input) return result;
  for (const endpoint of normalizeEndpointTypes(Object.keys(input))) {
    const url = input[endpoint];
    if (typeof url === "string" && url.trim()) result[endpoint] = url.trim();
  }
  return result;
}

/**
 * Family-specific identity fields. Retain only keys known to the selected family;
 * empty values are absent and required settings are checked by the host at runtime.
 */
function normalizeFamilySettings(
  value: unknown,
  family: ProviderFamily
): Partial<Record<FamilySetting, string>> {
  const input = record(value);
  const result: Partial<Record<FamilySetting, string>> = {};
  if (!input) return result;
  for (const setting of knownFamilySettings(family)) {
    const raw = input[setting];
    if (typeof raw === "string" && raw.trim()) result[setting] = raw.trim();
  }
  return result;
}

function normalizeProvider(value: unknown): ApiProvider | null {
  const input = record(value);
  if (!input || typeof input.id !== "string" || !input.id.trim()) return null;
  const family = typeof input.family === "string" && API_FORMATS.has(input.family as ProviderFamily)
    ? input.family as ProviderFamily
    : "openai_responses";
  const seenModelIds = new Set<string>();
  // The family resolves each model's reasoning form, so it must be settled first.
  const models = (Array.isArray(input.models)
    ? input.models
        .map((model) => normalizeModel(model, family))
        .filter((model): model is ModelProfile => Boolean(model))
    : []).filter((model) => {
      if (seenModelIds.has(model.id)) return false;
      seenModelIds.add(model.id);
      return true;
    });
  const requestedModelId = typeof input.activeModelId === "string" ? input.activeModelId.trim() : null;
  const activeModel = models.find((model) => model.id === requestedModelId);
  return {
    id: input.id.trim(),
    name: typeof input.name === "string" && input.name.trim() ? input.name.trim() : "未命名提供商",
    enabled: input.enabled !== false,
    family,
    baseUrl: typeof input.baseUrl === "string" ? input.baseUrl.trim() : "",
    familySettings: normalizeFamilySettings(input.familySettings, family),
    endpointBaseUrls: normalizeEndpointBaseUrls(input.endpointBaseUrls),
    notes: typeof input.notes === "string" ? input.notes : "",
    models,
    activeModelId: activeModel ? activeModel.id : null
  };
}

/** Global provider rows are catalog-only, deduplicated, and canonicalized in catalog order. */
function normalizeWebSearchAssets(value: unknown, fallback: WebSearchAssets): WebSearchAssets {
  // A missing block uses seeded defaults. A present empty block is an explicit
  // choice to disable every provider and must remain empty.
  if (value === undefined || value === null) {
    return structuredClone(fallback);
  }
  const input = record(value);
  const requested = Array.isArray(input?.providers) ? input.providers : [];
  const byKind = new Map<string, Record<string, unknown>>();
  for (const candidate of requested) {
    const provider = record(candidate);
    if (!provider || typeof provider.kind !== "string" || !isKnownSearchProvider(provider.kind)) continue;
    if (!byKind.has(provider.kind)) byKind.set(provider.kind, provider);
  }
  const text = (source: Record<string, unknown> | undefined, key: string) =>
    typeof source?.[key] === "string" ? (source[key] as string).trim() : "";
  const boundedNumber = (candidate: unknown, fallbackValue: number, min: number, max: number) =>
    typeof candidate === "number" && Number.isSafeInteger(candidate) && candidate >= min && candidate <= max
      ? candidate
      : fallbackValue;
  const compression = record(input?.compression);
  // A fetch provider must support fetching; an unsupported binding cannot resolve a backend.
  const fetchProvider = typeof input?.fetchProvider === "string"
    && isKnownSearchProvider(input.fetchProvider)
    && searchProviderSupports(input.fetchProvider, "fetchUrls")
    ? input.fetchProvider
    : null;
  return {
    providers: SEARCH_PROVIDERS.map((catalogProvider) => {
      const provider = byKind.get(catalogProvider.kind);
      return {
        kind: catalogProvider.kind,
        enabled: Boolean(provider?.enabled),
        searchApiHost: text(provider, "searchApiHost"),
        fetchApiHost: text(provider, "fetchApiHost"),
        engines: Array.isArray(provider?.engines)
          ? provider.engines
            .filter((engine): engine is string => typeof engine === "string")
            .map((engine) => engine.trim())
            .filter(Boolean)
          : [],
        basicAuthUsername: text(provider, "basicAuthUsername")
      };
    }),
    fetchProvider,
    maxResults: boundedNumber(input?.maxResults, fallback.maxResults, 1, 50),
    excludeDomains: Array.isArray(input?.excludeDomains)
      ? input.excludeDomains
        .filter((rule): rule is string => typeof rule === "string")
        .map((rule) => rule.trim())
        .filter(Boolean)
      : [],
    compression: {
      method: compression?.method === "none" ? "none" : "cutoff",
      cutoffLimit: boundedNumber(compression?.cutoffLimit, fallback.compression.cutoffLimit, 1, 200_000)
    }
  };
}

const DEFAULT_CONVERSATION_WEB_SEARCH: ConversationWebSearchSettings = {
  maxSearchesPerCall: 0,
  provider: { kind: "native" }
};

/** New-conversation web-search defaults mirror Rust ConversationWebSearchSettings::default. */
export function defaultConversationWebSearchSettings(): ConversationWebSearchSettings {
  return { ...DEFAULT_CONVERSATION_WEB_SEARCH, provider: { kind: "native" } };
}

/** An absent selection is the default (native), matching Rust's
 * `#[serde(default)] SearchProviderSelection::Native` — normalizing a missing
 * field to "unavailable" would silently disable search on any document that
 * predates or partially wrote this block. Only a selection that names something
 * this build cannot resolve becomes unavailable, and it keeps none of the old
 * ID, so enabling that provider later cannot silently restore a binding the
 * user was already told was lost. */
function normalizeSearchProviderSelection(value: unknown): ConversationWebSearchSettings["provider"] {
  if (value === undefined || value === null) return { kind: "native" };
  const input = record(value);
  if (input?.kind === "native") return { kind: "native" };
  if (input?.kind === "explicit" && typeof input.providerKind === "string"
    && isKnownSearchProvider(input.providerKind)) {
    return { kind: "explicit", providerKind: input.providerKind };
  }
  return { kind: "unavailable" };
}


function normalizeConversationWebSearch(
  value: unknown,
  fallback: ConversationWebSearchSettings
): ConversationWebSearchSettings {
  const input = record(value) ?? {};
  const requestedCallLimit = typeof input.maxSearchesPerCall === "number"
    && Number.isInteger(input.maxSearchesPerCall)
    && input.maxSearchesPerCall >= 0
    ? input.maxSearchesPerCall
    : fallback.maxSearchesPerCall;
  return {
    maxSearchesPerCall: Math.min(99_999, requestedCallLimit),
    provider: normalizeSearchProviderSelection(input.provider)
  };
}

function normalizeAgentDefinitions(
  value: unknown,
  fallback: AgentDefinition[]
): AgentDefinition[] {
  const source = Array.isArray(value) ? value : fallback;
  const identities = new Set<string>();
  const definitions: AgentDefinition[] = [];
  for (const candidate of source) {
    if (definitions.length >= 256) break;
    const input = record(candidate);
    if (!input) continue;
    // Tombstones are a host persistence detail. The renderer never exposes or
    // resubmits them; Rust merges deletion epochs into its authoritative copy.
    const deleted = input.deleted === undefined ? false : input.deleted;
    if (typeof deleted !== "boolean") continue;
    if (deleted) continue;
    const definitionSource = input.source;
    if (
      definitionSource !== "user"
      && definitionSource !== "project"
      && definitionSource !== "plugin"
      && definitionSource !== "managed"
    ) continue;
    const name = typeof input.name === "string" ? input.name : "";
    if (validateAgentTypeSlug(name) !== null) continue;
    const sourceKey = typeof input.sourceKey === "string" ? input.sourceKey : "";
    if (
      ((definitionSource === "user" || definitionSource === "managed") && sourceKey !== "")
      || ((definitionSource === "project" || definitionSource === "plugin") && (
        sourceKey.length === 0
        || sourceKey.trim() !== sourceKey
        || Array.from(sourceKey).length > 256
        || /[\u0000-\u001f\u007f]/.test(sourceKey)
      ))
    ) continue;
    const revision = input.revision;
    if (!Number.isSafeInteger(revision) || (revision as number) < 1) continue;
    const memoryEpoch = input.memoryEpoch === undefined ? 1 : input.memoryEpoch;
    if (!Number.isSafeInteger(memoryEpoch) || (memoryEpoch as number) < 1) continue;
    // Host-owned and part of the FROZEN binding projection, so a persisted
    // value is carried through verbatim rather than rewritten to the renderer's
    // own `"none"` — rewriting it would move the execution-mode payload bytes of
    // a trusted project/plugin definition that legitimately asks for memory.
    // Absent reads as `"none"`, matching the Rust serde default.
    const memoryInput = input.memory === undefined ? "none" : input.memory;
    if (
      memoryInput !== "none"
      && memoryInput !== "user"
      && memoryInput !== "project"
      && memoryInput !== "local"
    ) continue;
    const memory = memoryInput;
    // Prose the host renders into a tool description, not a capability. That is
    // the whole reason a malformed value CLAMPS here while `effort`/`tools`
    // below DROP the role: clamping a broken allowlist would silently restore
    // the permissive default, whereas clamping a broken description can only
    // ever make a role say less about itself. Absent is the ordinary state for
    // a role whose owner wrote nothing. Multi-line and unbounded by design —
    // the host writes it through verbatim, so nothing is normalized here.
    const description = typeof input.description === "string" ? input.description : "";
    const modelInput = record(input.modelSelection);
    let modelSelection: AgentDefinition["modelSelection"];
    if (modelInput?.kind === "inherit") {
      modelSelection = { kind: "inherit" };
    } else if (modelInput?.kind === "unavailable") {
      modelSelection = { kind: "unavailable" };
    } else if (modelInput?.kind === "explicit") {
      const providerId = typeof modelInput.providerId === "string" ? modelInput.providerId : "";
      const modelId = typeof modelInput.modelId === "string" ? modelInput.modelId : "";
      // Only the SHAPE is checked here. A pair that does not currently resolve is
      // kept verbatim: at rest there is no way to tell "the provider is signed
      // out and has not fetched its catalog" from "this model is gone forever",
      // and discarding the user's choice over the first case is the worse
      // mistake — a seeded role would lose its model before the user ever logs
      // in. Availability is asked at render and at call time instead, so the
      // role recovers by itself. Mirrors `storage::validate_agent_definition_list`.
      modelSelection = (
        !providerId
        || providerId.trim() !== providerId
        || !modelId
        || modelId.trim() !== modelId
      ) ? { kind: "unavailable" } : { kind: "explicit", providerId, modelId };
    } else {
      continue;
    }
    // The execution overrides. ABSENT is legitimate and must never drop —
    // every definition persisted before these keys existed lacks them. PRESENT
    // BUT MALFORMED does drop: these are capability-bearing, so clamping a
    // broken `tools` to `null` would silently restore the permissive "no
    // allowlist" default. Dropping also matches the host, whose
    // `validate_agent_definitions` refuses the same document outright — so a
    // malformed value can only arrive by hand-editing, never from our writer.
    //
    // `maxRounds` is intentionally ignored: per-role round budgets are not
    // supported. A stale key in an older document is ignored rather than
    // rejected, because it never granted anything — it could only ever tighten
    // a limit that does not exist.
    if (input.effort !== undefined && input.effort !== null
      && !(typeof input.effort === "string"
        && REASONING_EFFORTS.has(input.effort as ReasoningEffort))) continue;
    const effort =
      input.effort === undefined || input.effort === null ? null : (input.effort as ReasoningEffort);
    const toolsInput = input.tools;
    if (toolsInput !== undefined && toolsInput !== null && !isAgentToolNameList(toolsInput)) continue;
    const tools =
      toolsInput === undefined || toolsInput === null ? null : [...(toolsInput as string[])];
    const disallowedInput = input.disallowedTools;
    if (disallowedInput !== undefined && disallowedInput !== null
      && !isAgentToolNameList(disallowedInput)) continue;
    const disallowedTools =
      disallowedInput === undefined || disallowedInput === null
        ? []
        : [...(disallowedInput as string[])];
    // Conversation settings default to `native`; role settings default to inheriting
    // the conversation. Malformed role values become `unavailable` rather than a
    // usable default because this field carries capability selection.
    const searchProvider = input.searchProvider === undefined || input.searchProvider === null
      ? null
      : normalizeSearchProviderSelection(input.searchProvider);
    const identity = JSON.stringify([definitionSource, sourceKey, name]);
    if (identities.has(identity)) continue;
    identities.add(identity);
    definitions.push({
      enabled: typeof input.enabled === "boolean" ? input.enabled : true,
      deleted: false,
      name,
      description,
      source: definitionSource,
      sourceKey,
      revision: revision as number,
      memoryEpoch: memoryEpoch as number,
      modelSelection,
      memory,
      effort,
      tools,
      disallowedTools,
      searchProvider
    });
  }
  return definitions;
}

const MCP_TRANSPORTS = new Set<McpTransportKind>(["stdio", "streamable_http"]);
const SKILL_SOURCES = new Set<SkillSource>(["local_directory", "zip", "system_scan", "remote"]);
/** One hour. A longer timeout means no timeout and should use `longRunning`. */
const MAX_MCP_TIMEOUT_SECONDS = 3600;
const SHORTCUT_COMMAND_IDS = new Set<string>(SHORTCUT_COMMANDS.map((command) => command.id));

function trimmedString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value.trim() : fallback;
}

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => (typeof entry === "string" ? [entry] : []));
}

/** String dictionary for environment variables or request headers; discard blank keys. */
function stringMap(value: unknown): Record<string, string> {
  const input = record(value);
  if (!input) return {};
  const result: Record<string, string> = {};
  for (const [key, entry] of Object.entries(input)) {
    const name = key.trim();
    if (!name || typeof entry !== "string") continue;
    result[name] = entry;
  }
  return result;
}

function normalizeMcpServer(value: unknown, index: number): McpServerConfig | null {
  const input = record(value);
  if (!input) return null;
  const name = trimmedString(input.name);
  if (!name) return null;
  const transport: McpTransportKind = MCP_TRANSPORTS.has(input.transport as McpTransportKind)
    ? input.transport as McpTransportKind
    : "stdio";
  // Discard servers without the required command or URL to avoid a guaranteed failed probe.
  const command = trimmedString(input.command);
  const url = trimmedString(input.url);
  if (transport === "stdio" ? !command : !url) return null;
  const timeout = optionalNonNegativeInteger(input.timeoutSeconds) ?? 0;
  const now = new Date().toISOString();
  return {
    id: trimmedString(input.id) || createId("mcp_server"),
    name,
    description: typeof input.description === "string" ? input.description : "",
    enabled: input.enabled === true,
    transport,
    command,
    args: stringList(input.args),
    env: stringMap(input.env),
    registryUrl: trimmedString(input.registryUrl),
    url,
    headers: stringMap(input.headers),
    timeoutSeconds: Math.min(timeout, MAX_MCP_TIMEOUT_SECONDS),
    longRunning: input.longRunning === true,
    provider: trimmedString(input.provider),
    providerUrl: trimmedString(input.providerUrl),
    tags: uniqueStringIds(input.tags),
    disabledTools: uniqueStringIds(input.disabledTools),
    disabledAutoApproveTools: uniqueStringIds(input.disabledAutoApproveTools),
    sortOrder: optionalNonNegativeInteger(input.sortOrder) ?? index,
    createdAt: trimmedString(input.createdAt) || now,
    updatedAt: trimmedString(input.updatedAt) || now
  };
}

function normalizeMcpServers(value: unknown, fallback: McpServerConfig[]): McpServerConfig[] {
  if (!Array.isArray(value)) return fallback.map((server) => ({ ...server }));
  const seen = new Set<string>();
  return value
    .map((entry, index) => normalizeMcpServer(entry, index))
    .filter((server): server is McpServerConfig => Boolean(server))
    .filter((server) => {
      if (seen.has(server.id)) return false;
      seen.add(server.id);
      return true;
    })
    .sort((left, right) => left.sortOrder - right.sortOrder)
    .map((server, index) => ({ ...server, sortOrder: index }));
}

function normalizeSkill(value: unknown): SkillRecord | null {
  const input = record(value);
  if (!input) return null;
  const folderName = trimmedString(input.folderName);
  const name = trimmedString(input.name);
  // The folder name is the record's only link to skill content on disk.
  if (!folderName || !name) return null;
  const now = new Date().toISOString();
  return {
    id: trimmedString(input.id) || createId("skill"),
    name,
    description: typeof input.description === "string" ? input.description : "",
    folderName,
    source: SKILL_SOURCES.has(input.source as SkillSource)
      ? input.source as SkillSource
      : "local_directory",
    sourceLocation: trimmedString(input.sourceLocation),
    sourceUrl: trimmedString(input.sourceUrl),
    author: trimmedString(input.author),
    version: trimmedString(input.version),
    tags: uniqueStringIds(input.tags),
    contentHash: trimmedString(input.contentHash),
    enabled: input.enabled !== false,
    installedAt: trimmedString(input.installedAt) || now,
    updatedAt: trimmedString(input.updatedAt) || now
  };
}

function normalizeSkills(value: unknown, fallback: SkillRecord[]): SkillRecord[] {
  if (!Array.isArray(value)) return fallback.map((skill) => ({ ...skill }));
  const seenIds = new Set<string>();
  const seenFolders = new Set<string>();
  return value
    .map(normalizeSkill)
    .filter((skill): skill is SkillRecord => Boolean(skill))
    .filter((skill) => {
      const folder = skill.folderName.toLowerCase();
      if (seenIds.has(skill.id) || seenFolders.has(folder)) return false;
      seenIds.add(skill.id);
      seenFolders.add(folder);
      return true;
    });
}

/** Normalize a key chord: tokens must be strings and form a valid ordered binding. */
function normalizeKeyBinding(value: unknown): KeyToken[] {
  const tokens = stringList(value).map((token) => token.trim()).filter(Boolean);
  if (!tokens.length) return [];
  const ordered = orderBinding([...new Set(tokens)]);
  return isValidBinding(ordered) ? ordered : [];
}

function normalizeShortcuts(value: unknown): GlobalSettings["shortcuts"] {
  const input = record(value);
  if (!input) return {};
  const result: GlobalSettings["shortcuts"] = {};
  for (const [id, entry] of Object.entries(input)) {
    // Commands not in the code-owned command table cannot retain orphaned bindings.
    if (!SHORTCUT_COMMAND_IDS.has(id)) continue;
    const preference = record(entry);
    if (!preference) continue;
    const binding = normalizeKeyBinding(preference.binding);
    result[id as ShortcutCommandId] = {
      binding,
      // A command without a binding cannot be enabled.
      enabled: binding.length > 0 && preference.enabled === true
    };
  }
  return result;
}

/** Normalize the composer send/newline chord without checking command-table conflicts. */
function normalizeComposerShortcut(value: unknown, fallback: KeyToken[]): KeyToken[] {
  const tokens = stringList(value).map((token) => token.trim()).filter(Boolean);
  if (!tokens.length) return [...fallback];
  const ordered = orderBinding([...new Set(tokens)]);
  // Both roles choose modifiers for the required terminating `Enter` key.
  return ordered[ordered.length - 1] === "Enter" ? ordered : [...fallback];
}

function normalizeAppearance(
  value: unknown,
  fallback: AppearancePreferences
): AppearancePreferences {
  const input = record(value);
  if (!input) return { ...fallback };
  const sendShortcut = normalizeComposerShortcut(input.sendShortcut, fallback.sendShortcut);
  let newlineShortcut = normalizeComposerShortcut(input.newlineShortcut, fallback.newlineShortcut);
  // Send and newline cannot share a chord. Restore the newline default, or a
  // guaranteed different fallback, on collision.
  if (newlineShortcut.join("+") === sendShortcut.join("+")) {
    newlineShortcut = fallback.newlineShortcut.join("+") === sendShortcut.join("+")
      ? ["Control", "Enter"]
      : [...fallback.newlineShortcut];
  }
  const themeColor = normalizeHexColor(
    typeof input.themeColor === "string" ? input.themeColor : ""
  );
  return {
    themeColor: themeColor ?? "",
    zoom: clampZoom(optionalFiniteNumber(input.zoom) ?? fallback.zoom),
    uiFontFamily: trimmedString(input.uiFontFamily),
    monoFontFamily: trimmedString(input.monoFontFamily),
    messageFontSize: clampMessageFontSize(
      optionalFiniteNumber(input.messageFontSize) ?? fallback.messageFontSize
    ),
    serifMessages: input.serifMessages === true,
    wideMessages: input.wideMessages === true,
    sendShortcut,
    newlineShortcut,
    spellCheck: input.spellCheck === true,
    renderUserMarkdown: input.renderUserMarkdown === true,
    confirmMessageDelete: input.confirmMessageDelete !== false,
    collapseReasoning: input.collapseReasoning !== false,
    codeBlockCollapsible: input.codeBlockCollapsible === true,
    codeBlockWrappable: input.codeBlockWrappable === true,
    singleDollarMath: input.singleDollarMath !== false,
    customCss: typeof input.customCss === "string" ? input.customCss : ""
  };
}

const TOOL_NAME_PATTERN = /^[a-zA-Z][a-zA-Z0-9_-]*$/;

function normalizeEnvironmentTools(
  value: unknown,
  fallback: EnvironmentToolDefinition[]
): EnvironmentToolDefinition[] {
  if (!Array.isArray(value)) return fallback.map((tool) => ({ ...tool }));
  const seen = new Set<string>();
  return value.flatMap((entry) => {
    const input = record(entry);
    if (!input) return [];
    const name = trimmedString(input.name);
    const executable = trimmedString(input.executable) || name;
    if (!TOOL_NAME_PATTERN.test(name) || !executable) return [];
    const key = name.toLowerCase();
    if (seen.has(key)) return [];
    seen.add(key);
    const versionArgs = stringList(input.versionArgs).map((argument) => argument.trim()).filter(Boolean);
    return [{ name, executable, versionArgs: versionArgs.length ? versionArgs : ["--version"] }];
  });
}

function normalizeGlobalSettings(
  value: unknown,
  fallback: GlobalSettings,
  knownToolNames: ReadonlySet<string>
): GlobalSettings {
  const input = record(value) ?? {};
  const seenProviderIds = new Set<string>();
  // Providers are user-created except the built-in Codex and Claude Agent rows,
  // which `ensureCodexProvider` and `ensureClaudeAgentProvider` keep present.
  const providers = ensureClaudeAgentProvider(ensureCodexProvider((Array.isArray(input.apiProviders)
    ? input.apiProviders.map(normalizeProvider).filter((provider): provider is ApiProvider => Boolean(provider))
    : fallback.apiProviders).filter((provider) => {
      if (seenProviderIds.has(provider.id)) return false;
      seenProviderIds.add(provider.id);
      return true;
    })));
  const fallbackPreset = fallback.conversationPresets.find(
    (preset) => preset.id === fallback.defaultConversationPresetId
  ) ?? fallback.conversationPresets[0];
  const fallbackPresetSettings: ConversationPresetSettings = fallbackPreset?.settings
    ?? emptyConversationPresetSettings();
  // Search assets depend only on the fixed provider catalog and can be assembled here.
  const webSearchAssets = normalizeWebSearchAssets(input.webSearch, fallback.webSearch);
  const seenPresetIds = new Set<string>();
  const conversationPresets = (Array.isArray(input.conversationPresets)
    ? input.conversationPresets
        .map((preset) => normalizeConversationPreset(
          preset, fallbackPresetSettings, knownToolNames
        ))
        .filter((preset): preset is ConversationPreset => Boolean(preset))
    : fallback.conversationPresets.map((preset) => ({
        ...preset,
        settings: normalizeConversationPresetSettings(
          preset.settings, fallbackPresetSettings, knownToolNames
        )
      })))
    .filter((preset) => {
      if (seenPresetIds.has(preset.id)) return false;
      seenPresetIds.add(preset.id);
      return true;
    });
  const requestedDefaultPresetId = optionalPresetId(input.defaultConversationPresetId);
  const defaultConversationPresetId = conversationPresets.some(
    (preset) => preset.id === requestedDefaultPresetId
  ) ? requestedDefaultPresetId! : conversationPresets[0]?.id ?? "";
  const requestedProviderId = typeof input.activeProviderId === "string" ? input.activeProviderId.trim() : null;
  const requestedProvider = providers.find((provider) => (
    provider.id === requestedProviderId && provider.enabled
  )) ?? null;
  const activeProvider = requestedProvider
    ?? providers.find((provider) => provider.enabled)
    ?? null;
  return {
    appLanguage: normalizeAppLanguage(input.appLanguage, fallback.appLanguage),
    // The App effect overwrites this with the actually resolved value on load,
    // so it only has to be sane until then.
    resolvedAppLanguage: normalizeResolvedAppLanguage(input.resolvedAppLanguage, fallback.resolvedAppLanguage),
    theme: normalizeThemePreference(input.theme, fallback.theme),
    conversationPresets,
    defaultConversationPresetId,
    lastReasoningEffort: normalizeReasoningEffort(input.lastReasoningEffort, fallback.lastReasoningEffort),
    apiProviders: providers,
    activeProviderId: activeProvider?.id ?? null,
    webSearch: webSearchAssets,
    mcpServers: normalizeMcpServers(input.mcpServers, fallback.mcpServers),
    skills: normalizeSkills(input.skills, fallback.skills),
    appearance: normalizeAppearance(input.appearance, fallback.appearance),
    shortcuts: normalizeShortcuts(input.shortcuts),
    environmentTools: normalizeEnvironmentTools(input.environmentTools, fallback.environmentTools),
    executionEnvironments: normalizeExecutionEnvironments(
      input.executionEnvironments, fallback.executionEnvironments
    )
  };
}

function normalizeContextItems(values: unknown): ContextItem[] {
  if (!Array.isArray(values)) return [];
  return values.flatMap((value) => {
    const normalized = normalizeContextItem(value);
    return normalized ? [normalized] : [];
  });
}

function normalizeContextItem(value: unknown): ContextItem | null {
  const input = record(value);
  if (!input || typeof input.id !== "string" || typeof input.kind !== "string") return null;
  if (input.kind === "tool") {
    const result = record(input.result);
    if (!result) return null;
    const subagent = record(input.subagent);
    const context = {
      ...(input as unknown as Extract<ContextItem, { kind: "tool" }>),
      kind: "tool",
      result: {
        ...(result as unknown as Extract<ContextItem, { kind: "tool" }>["result"]),
        images: normalizeImageAttachments(result.images)
      },
      subagent: subagent ? {
        ...(subagent as unknown as NonNullable<Extract<ContextItem, { kind: "tool" }>["subagent"]>),
        contexts: Array.isArray(subagent.contexts)
          ? normalizeContextItems(subagent.contexts)
          : [],
        ...(() => {
          const queuedMessages = Array.isArray(subagent.queuedMessages)
            ? subagent.queuedMessages.flatMap((value) => {
                const message = record(value);
                return message && typeof message.content === "string" && message.content.trim()
                  ? [{ content: message.content, triggerTurn: message.triggerTurn === true }]
                  : [];
              })
            : [];
          return queuedMessages.length ? { queuedMessages } : {};
        })()
      } : undefined
    } satisfies Extract<ContextItem, { kind: "tool" }>;
    return context;
  }
  if (input.kind === "system" || input.kind === "user" || input.kind === "assistant" || input.kind === "reasoning") {
    if (input.kind === "user") {
      return {
        ...(input as unknown as Extract<ContextItem, { kind: "user" }>),
        images: normalizeImageAttachments(input.images)
      };
    }
    return input as unknown as ContextItem;
  }
  return null;
}

/** Defensively normalize the current schema: fill absent optional fields, discard
 * malformed entries, and reject version mismatches. */
export function normalizeDocument(value: unknown): AppDocument {
  const fallback = createSeedDocument();
  const input = record(value);
  if (!input) throw new Error("文档根节点必须是对象");
  const schemaVersion = optionalFiniteNumber(input.schemaVersion) ?? 0;
  if (schemaVersion > fallback.schemaVersion) {
    throw new Error(`数据由更新版本写入（schema ${schemaVersion}），当前版本只支持 schema ${fallback.schemaVersion}`);
  }
  if (schemaVersion < fallback.schemaVersion) {
    throw new Error(`数据由旧版 schema ${schemaVersion} 写入，历史迁移已在未发布阶段删除，当前版本只支持 schema ${fallback.schemaVersion}`);
  }
  // Use the seeded built-in catalog but retain dynamically discovered MCP descriptors
  // that are absent from it, including their enabled-tool selections.
  const dynamicTools = (Array.isArray(input.tools) ? input.tools : [])
    .flatMap((entry) => {
      const descriptor = record(entry);
      const name = typeof descriptor?.name === "string" ? descriptor.name.trim() : "";
      if (!descriptor || !name || descriptor.category !== "mcp") return [];
      return fallback.tools.some((tool) => tool.name === name)
        ? []
        : [descriptor as unknown as ToolDescriptor];
    })
    .filter((tool, index, list) => list.findIndex((entry) => entry.name === tool.name) === index);
  const tools = [...fallback.tools, ...dynamicTools];
  const knownToolNames = new Set(tools.map((tool) => tool.name));
  // Persisted assets and presets live in top-level containers; merge them into the
  // flat `GlobalSettings` input before normalization.
  const globalInput = record(input.globalSettings);
  const mergedGlobalInput: Record<string, unknown> = {
    ...(globalInput ?? {}),
    ...(record(input.assets) ?? {}),
    ...(record(input.presets) ?? {})
  };
  const globalSettings = normalizeGlobalSettings(
    globalInput || record(input.assets) || record(input.presets) ? mergedGlobalInput : undefined,
    fallback.globalSettings,
    knownToolNames
  );
  const sourceWorkspaces = Array.isArray(input.workspaces)
    ? [...input.workspaces as AppDocument["workspaces"]]
    : fallback.workspaces;
  /** Normalize conversation settings and workspace snapshots through the same path
   * because they share a structure and must accept the same values. */
  const normalizeConversationSettingsValue = (value: unknown): ConversationSettings => {
    const settingsInput = record(value) ?? {};
    const enabledTools = uniqueStringIds(settingsInput.enabledTools)
      .filter((name) => knownToolNames.has(name) && !isHostDerivedToolName(name));
    return {
      ...(settingsInput as unknown as ConversationSettings),
      systemPrompt: typeof settingsInput.systemPrompt === "string" ? settingsInput.systemPrompt : "",
      includeAppDataPath: settingsInput.includeAppDataPath === true,
      enabledTools,
      hookIds: uniqueStringIds(settingsInput.hookIds),
      skillIds: uniqueStringIds(settingsInput.skillIds),
      mcpIds: uniqueStringIds(settingsInput.mcpIds),
      toolDescriptionFileId: normalizeToolDescriptionFileId(settingsInput.toolDescriptionFileId),
      agentDefinitions: normalizeAgentDefinitions(
        settingsInput.agentDefinitions,
        []
      ),
      // As for presets, absence means false and requires a role.
      allowRolelessSubagents: settingsInput.allowRolelessSubagents === true,
      webSearch: normalizeConversationWebSearch(
        settingsInput.webSearch,
        DEFAULT_CONVERSATION_WEB_SEARCH
      ),
      reasoningEffort: normalizeReasoningEffort(
        settingsInput.reasoningEffort,
        globalSettings.lastReasoningEffort
      ),
      // No global default is inherited; absent values use the most cautious level.
      securityLevel: normalizeSecurityLevel(settingsInput.securityLevel, "request_approval"),
      // The two tiers are independent switches, and both default OFF: memory
      // reads and writes files the user may not expect a conversation to
      // touch, so turning a tier on is an explicit choice. The built-in
      // engineering defaults preset — not this normalizer — is where a
      // fresh conversation's answer comes from.
      globalMemoryEnabled: settingsInput.globalMemoryEnabled === true,
      projectMemoryEnabled: settingsInput.projectMemoryEnabled === true,
      // Absence means false, preserving inline skill content for older conversations.
      skillToolEnabled: settingsInput.skillToolEnabled === true
    };
  };
  const normalizeConversations = (workspace: AppDocument["workspaces"][number]) =>
    workspace.conversations.map((conversation) => {
        const conversationInput = record(conversation);
        const baseSettings = normalizeConversationSettingsValue(conversationInput?.settings);
      return {
          ...conversation,
          contexts: Array.isArray(conversation.contexts)
            ? normalizeContextItems(conversation.contexts)
            : [],
          queuedMessages: Array.isArray(conversationInput?.queuedMessages)
            ? conversationInput.queuedMessages.flatMap((value) => {
                const message = record(value);
                const images = normalizeImageAttachments(message?.images);
                if (
                  !message
                  || typeof message.id !== "string"
                  || !message.id.trim()
                  || message.id.length > 128
                  || typeof message.content !== "string"
                  || (!message.content.trim() && !images?.length)
                  || Array.from(message.content).length > 100_000
                  || typeof message.createdAt !== "string"
                  || !Number.isFinite(Date.parse(message.createdAt))
                ) return [];
                return [{
                  id: message.id,
                  content: message.content,
                  images,
                  createdAt: message.createdAt
                }];
              }).filter((message, index, messages) => messages.findIndex(
                (candidate) => candidate.id === message.id
              ) === index).slice(0, 100)
            : [],
          userAbortedTasks: Array.isArray(conversationInput?.userAbortedTasks)
            ? conversationInput.userAbortedTasks.flatMap((value) => {
                const task = record(value);
                const metrics = record(task?.metrics);
                const kinds = new Set(["subagent", "workflow", "terminal", "shell", "browser"]);
                const metric = (name: string): number | null | undefined => {
                  const value = metrics?.[name];
                  return value === null || (typeof value === "number" && Number.isFinite(value) && value >= 0)
                    ? value
                    : undefined;
                };
                const childCount = metric("childCount");
                const tokens = metric("tokens");
                const toolCount = metric("toolCount");
                const elapsedMs = metric("elapsedMs");
                if (
                  !task
                  || typeof task.id !== "string" || !task.id.trim() || task.id.length > 128
                  || typeof task.sourceKind !== "string" || !kinds.has(task.sourceKind)
                  || typeof task.sourceIdentity !== "string" || !task.sourceIdentity.trim() || task.sourceIdentity.length > 256
                  || typeof task.label !== "string" || Array.from(task.label).length > 512
                  || typeof task.detail !== "string" || Array.from(task.detail).length > 4096
                  || !metrics
                  || childCount === undefined || tokens === undefined || toolCount === undefined || elapsedMs === undefined
                  || typeof task.startedAt !== "string" || (task.startedAt !== "" && !Number.isFinite(Date.parse(task.startedAt)))
                  || typeof task.endedAt !== "string" || !Number.isFinite(Date.parse(task.endedAt))
                  || task.reason !== "userAborted"
                ) return [];
                return [{
                  id: task.id,
                  sourceKind: task.sourceKind as import("../types").UserAbortedTaskKind,
                  sourceIdentity: task.sourceIdentity,
                  label: task.label,
                  detail: task.detail,
                  metrics: { childCount, tokens, toolCount, elapsedMs },
                  startedAt: task.startedAt,
                  endedAt: task.endedAt,
                  reason: "userAborted" as const
                }];
              }).filter((task, index, tasks) => tasks.findIndex(
                (candidate) => candidate.id === task.id
              ) === index).slice(-256)
            : [],
          branches: Array.isArray(conversationInput?.branches)
            ? conversationInput.branches.flatMap((value) => {
                const branch = record(value);
                if (
                  !branch
                  || typeof branch.id !== "string"
                  || typeof branch.forkContextId !== "string"
                  || typeof branch.active !== "boolean"
                ) return [];
                return [{
                  id: branch.id,
                  forkContextId: branch.forkContextId,
                  active: branch.active,
                  contexts: Array.isArray(branch.contexts)
                    ? normalizeContextItems(branch.contexts)
                    : [],
                  createdAt: typeof branch.createdAt === "string" ? branch.createdAt : conversation.createdAt,
                  updatedAt: typeof branch.updatedAt === "string" ? branch.updatedAt : conversation.updatedAt
                }];
              })
            : [],
          settings: baseSettings,
          worktree: normalizeConversationWorktree(conversationInput?.worktree),
          runTarget: normalizeRunTarget(conversationInput?.runTarget),
          // A parent that turns out not to exist is resolved at render time,
          // not here: the tree builder treats a dangling id as a root.
          parentConversationId: typeof conversationInput?.parentConversationId === "string"
            && conversationInput.parentConversationId.trim()
            && conversationInput.parentConversationId !== conversation.id
            ? conversationInput.parentConversationId
            : null
      };
    });
  const regularWorkspaces: AppDocument["workspaces"] = [];
  let temporaryWorkspace: AppDocument["workspaces"][number] | null = null;
  /** A workspace may retain a dangling default preset ID; unresolved means unset,
   * consistent with capability resource IDs. */
  const workspacePresetId = (workspace: unknown): string => {
    const value = record(workspace)?.defaultConversationPresetId;
    return typeof value === "string" && value.trim() && value.length <= 128 ? value : "";
  };
  const workspaceLastSettings = (workspace: unknown): ConversationSettings | null => {
    const value = record(workspace)?.lastConversationSettings;
    return record(value) ? normalizeConversationSettingsValue(value) : null;
  };
  for (const workspace of sourceWorkspaces) {
    const conversations = normalizeConversations(workspace);
    if (workspace.id === TEMPORARY_WORKSPACE_ID) {
      if (temporaryWorkspace) {
        temporaryWorkspace.conversations.push(...conversations);
      } else {
        temporaryWorkspace = {
          id: TEMPORARY_WORKSPACE_ID,
          name: "临时工作区",
          kind: "temporary",
          path: "",
          createdAt: workspace.createdAt,
          defaultConversationPresetId: workspacePresetId(workspace),
          lastConversationSettings: workspaceLastSettings(workspace),
          conversations
        };
      }
      continue;
    }
    const kind = (workspace as unknown as { kind?: unknown }).kind;
    if (kind !== "directory" || workspace.id.startsWith("__")) {
      if (temporaryWorkspace) {
        temporaryWorkspace.conversations.push(...conversations);
      } else {
        temporaryWorkspace = createTemporaryWorkspace(conversations);
      }
      continue;
    }
    regularWorkspaces.push({
      id: workspace.id,
      name: workspace.name,
      kind: "directory",
      path: workspace.path,
      createdAt: workspace.createdAt,
      defaultConversationPresetId: workspacePresetId(workspace),
      lastConversationSettings: workspaceLastSettings(workspace),
      conversations
    });
  }
  const workspaces = [
    ...regularWorkspaces,
    temporaryWorkspace ?? createTemporaryWorkspace()
  ];
  return {
    schemaVersion: fallback.schemaVersion,
    globalSettings,
    workspaces,
    tools,
    capabilities: record(input.capabilities)
      ? {
          hooks: Array.isArray(record(input.capabilities)?.hooks)
            ? record(input.capabilities)?.hooks as CapabilityCatalog["hooks"]
            : [],
          skills: Array.isArray(record(input.capabilities)?.skills)
            ? record(input.capabilities)?.skills as CapabilityCatalog["skills"]
            : fallback.capabilities.skills,
          mcps: Array.isArray(record(input.capabilities)?.mcps)
            ? record(input.capabilities)?.mcps as CapabilityCatalog["mcps"]
            : fallback.capabilities.mcps,
          toolDescriptionFiles: Array.isArray(record(input.capabilities)?.toolDescriptionFiles)
            ? record(input.capabilities)?.toolDescriptionFiles as CapabilityCatalog["toolDescriptionFiles"]
            : fallback.capabilities.toolDescriptionFiles
        }
      : fallback.capabilities
  };
}

async function sha256Hex(value: string): Promise<string> {
  if (!globalThis.crypto?.subtle) throw new Error("当前环境不支持安全的 API Key 端点指纹");
  const digest = await globalThis.crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Browser-preview marker for whether a provider has been configured with a key.
 * It mirrors desktop credential identity: one key per provider ID. */
async function credentialFingerprint(providerId: string): Promise<string> {
  return sha256Hex(`v6\0${providerId}`);
}

export async function getStoredApiKeyLength(providerId: string): Promise<number | undefined> {
  const fingerprint = await credentialFingerprint(providerId);
  const value = Number(window.localStorage.getItem(`${API_KEY_LENGTH_PREFIX}${fingerprint}`));
  return Number.isInteger(value) && value > 0 ? value : undefined;
}

async function rememberApiKeyLength(providerId: string, keyLength?: number): Promise<void> {
  const fingerprint = await credentialFingerprint(providerId);
  const storageKey = `${API_KEY_LENGTH_PREFIX}${fingerprint}`;
  if (Number.isInteger(keyLength) && (keyLength ?? 0) > 0) {
    window.localStorage.setItem(storageKey, String(keyLength));
  } else {
    window.localStorage.removeItem(storageKey);
  }
}

export async function forgetStoredApiKeyLength(providerId: string): Promise<void> {
  await rememberApiKeyLength(providerId);
}

async function browserLoad(): Promise<AppDocument> {
  const stored = window.localStorage.getItem(STORAGE_KEY);
  let document: AppDocument;
  if (!stored) {
    document = createSeedDocument();
  } else {
    try {
      document = normalizeDocument(JSON.parse(stored));
    } catch (error) {
      throw new Error(`本地数据无法读取，原始数据已保留：${error instanceof Error ? error.message : String(error)}`);
    }
  }
  try {
    // A browser reload discards unsent composer state, so every indexed image
    // not referenced by the recovered document can safely enter the delayed
    // orphan lifecycle. Only Mework's attachment namespace is touched.
    reconcilePreviewImageStore(
      new Set(),
      referencedPreviewImageIdsIn(document),
      true
    );
  } catch {
    // Index maintenance is best-effort and must not make a valid document
    // unloadable when localStorage is at quota.
  }
  return document;
}

/**
 * Copies a conversation's history through `throughContextId` into a branch.
 *
 * The host reads the source from its own committed document and re-issues an
 * execution receipt for every copied tool result, so the branch is an
 * independent conversation that can be edited and saved on its own. Ids are
 * regenerated by the host; the renderer must not reuse the source's.
 *
 * The browser preview has no receipt book, so it copies locally with fresh ids.
 */
export async function forkConversationContexts(request: {
  workspaceId: string;
  sourceConversationId: string;
  targetConversationId: string;
  throughContextId: string;
  sourceContexts: ContextItem[];
}): Promise<ContextItem[]> {
  if (hasBackendRuntime()) {
    return invoke<ContextItem[]>("fork_conversation_contexts", {
      workspaceId: request.workspaceId,
      sourceConversationId: request.sourceConversationId,
      targetConversationId: request.targetConversationId,
      throughContextId: request.throughContextId
    });
  }
  const cut = request.sourceContexts.findIndex(
    (context) => context.id === request.throughContextId
  );
  if (cut < 0) throw new Error(`分支起点 ${request.throughContextId} 不在源对话中`);
  const turnIds = new Map<string, string>();
  const remap = (turn?: string): string | undefined => {
    if (turn === undefined) return undefined;
    const existing = turnIds.get(turn);
    if (existing) return existing;
    const next = createId("turn");
    turnIds.set(turn, next);
    return next;
  };
  return request.sourceContexts.slice(0, cut + 1).map((context) => {
    const id = createId("ctx");
    return context.kind === "system" || context.kind === "user"
      ? { ...context, id }
      : { ...context, id, modelTurnId: remap(context.modelTurnId) };
  });
}

export async function loadDocument(): Promise<AppDocument> {
  if (hasBackendRuntime()) return normalizeDocument(await invoke<unknown>("load_document"));
  return browserLoad();
}

/**
 * Whether the host owns conversation data. With a backend, conversation bodies live
 * in host SQLite and renderer changes go through commands. Browser preview and tests
 * instead use the localStorage document as their authoritative model.
 */
export function hasConversationCommands(): boolean {
  return hasBackendRuntime();
}

export async function createConversationRemote(
  workspaceId: string,
  conversation: Conversation
): Promise<Conversation | null> {
  if (!hasBackendRuntime()) return null;
  return invoke<Conversation>("create_conversation", { workspaceId, conversation });
}

export async function deleteConversationRemote(
  workspaceId: string,
  conversationId: string
): Promise<void> {
  if (!hasBackendRuntime()) return;
  await invoke("delete_conversation", { workspaceId, conversationId });
}

export async function updateConversationRemote(
  workspaceId: string,
  conversation: Conversation,
  expectedContextIds: string[]
): Promise<Conversation | null> {
  if (!hasBackendRuntime()) return null;
  return invoke<Conversation>("update_conversation", {
    workspaceId,
    conversation,
    expectedContextIds
  });
}

export async function reorderConversationsRemote(
  workspaceId: string,
  conversationIds: string[]
): Promise<void> {
  if (!hasBackendRuntime()) return;
  await invoke("reorder_conversations", { workspaceId, conversationIds });
}

export async function loadConversationRemote(
  conversationId: string
): Promise<Conversation | null> {
  if (!hasBackendRuntime()) return null;
  return invoke<Conversation | null>("load_conversation", { conversationId });
}

let documentSaveTail: Promise<void> = Promise.resolve();

function copyDocument(document: AppDocument): AppDocument {
  return typeof structuredClone === "function"
    ? structuredClone(document)
    : JSON.parse(JSON.stringify(document)) as AppDocument;
}

function imageAttachmentForPersistence(image: ImageAttachment): ImageAttachment {
  return {
    id: image.id,
    name: image.name,
    mime: image.mime,
    width: image.width,
    height: image.height,
    bytes: image.bytes,
    ...(image.shortId !== undefined ? { shortId: image.shortId } : {})
  };
}

/**
 * Projects a renderer context into the only shape allowed in local persistence.
 * Streaming state and provider protocol envelopes intentionally have no branch
 * here, so browser preview storage enforces the same boundary as Rust serde.
 */
function contextForPersistence(context: ContextItem): ContextItem {
  switch (context.kind) {
    case "system":
      return {
        id: context.id,
        kind: "system",
        content: context.content,
        ...(context.localOnly ? { localOnly: true } : {}),
        ...(context.hookExecution ? {
          hookExecution: {
            executionId: context.hookExecution.executionId,
            hookId: context.hookExecution.hookId,
            hookName: context.hookExecution.hookName,
            event: context.hookExecution.event,
            status: context.hookExecution.status,
            contextInjected: context.hookExecution.contextInjected
          }
        } : {}),
        createdAt: context.createdAt
      };
    case "user":
      return {
        id: context.id,
        kind: "user",
        content: context.content,
        ...(context.images?.length
          ? { images: context.images.map(imageAttachmentForPersistence) }
          : {}),
        createdAt: context.createdAt
      };
    case "assistant":
      return {
        id: context.id,
        kind: "assistant",
        content: context.content,
        ...(context.round === undefined ? {} : { round: context.round }),
        ...(context.modelTurnId === undefined ? {} : { modelTurnId: context.modelTurnId }),
        ...(context.interrupted ? { interrupted: true } : {}),
        createdAt: context.createdAt
      };
    case "reasoning":
      return {
        id: context.id,
        kind: "reasoning",
        ...(context.content === undefined ? {} : { content: context.content }),
        ...(context.form === undefined ? {} : { form: context.form }),
        ...(context.round === undefined ? {} : { round: context.round }),
        ...(context.modelTurnId === undefined ? {} : { modelTurnId: context.modelTurnId }),
        ...(context.interrupted ? { interrupted: true } : {}),
        createdAt: context.createdAt
      };
    case "tool": {
      const input = context.input;
      const requestedInput = context.requestedInput;
      const result = context.result;
      return {
        id: context.id,
        kind: "tool",
        toolName: context.toolName,
        ...(context.round === undefined ? {} : { round: context.round }),
        ...(context.modelTurnId === undefined ? {} : { modelTurnId: context.modelTurnId }),
        ...(requestedInput === undefined ? {} : { requestedInput }),
        input,
        result: {
          success: result.success,
          output: result.output,
          ...(result.images?.length
            ? { images: result.images.map(imageAttachmentForPersistence) }
            : {}),
          ...(result.diff === undefined ? {} : { diff: result.diff }),
          executedAt: result.executedAt,
          durationMs: result.durationMs
        },
        ...(context.subagent ? {
          subagent: {
            ...(context.subagent.kind === undefined ? {} : { kind: context.subagent.kind }),
            ...(context.subagent.name === undefined ? {} : { name: context.subagent.name }),
            ...(context.subagent.label === undefined ? {} : { label: context.subagent.label }),
            ...(context.subagent.inheritsModelMemory ? { inheritsModelMemory: true } : {}),
            ...(context.subagent.forkModelBinding ? {
              forkModelBinding: {
                providerId: context.subagent.forkModelBinding.providerId,
                modelId: context.subagent.forkModelBinding.modelId,
                memoryLanguage: context.subagent.forkModelBinding.memoryLanguage,
                memoryToolNames: [...context.subagent.forkModelBinding.memoryToolNames],
                systemPromptSnapshot: context.subagent.forkModelBinding.systemPromptSnapshot,
                systemPromptReceipt: context.subagent.forkModelBinding.systemPromptReceipt,
                ...(context.subagent.forkModelBinding.memorySnapshotReceipt === undefined
                  ? {}
                  : {
                      memorySnapshotReceipt:
                        context.subagent.forkModelBinding.memorySnapshotReceipt
                    }),
                bindingReceipt: context.subagent.forkModelBinding.bindingReceipt,
                // Absence is the v1 marker on the Rust side, so an absent key
                // must stay absent: writing an explicit 1 would make a genuine
                // pre-versioning record indistinguishable from a stamped one.
                ...(context.subagent.forkModelBinding.receiptVersion === undefined
                  ? {}
                  : { receiptVersion: context.subagent.forkModelBinding.receiptVersion })
              }
            } : {}),
            ...(context.subagent.agentDefinition ? {
              agentDefinition: {
                source: context.subagent.agentDefinition.source,
                sourceKey: context.subagent.agentDefinition.sourceKey,
                name: context.subagent.agentDefinition.name,
                revision: context.subagent.agentDefinition.revision,
                memoryEpoch: context.subagent.agentDefinition.memoryEpoch,
                providerId: context.subagent.agentDefinition.providerId,
                modelId: context.subagent.agentDefinition.modelId,
                memory: context.subagent.agentDefinition.memory,
                scopeKey: context.subagent.agentDefinition.scopeKey,
                configurationReceipt:
                  context.subagent.agentDefinition.configurationReceipt,
                // Same rule as the fork binding: never synthesize a version.
                ...(context.subagent.agentDefinition.receiptVersion === undefined
                  ? {}
                  : { receiptVersion: context.subagent.agentDefinition.receiptVersion })
              }
            } : {}),
            ...(context.subagent.executionModeReceipt
              ? { executionModeReceipt: context.subagent.executionModeReceipt }
              : {}),
            task: context.subagent.task,
            status: context.subagent.status,
            contexts: context.subagent.contexts.map(contextForPersistence),
            updates: context.subagent.updates.map((update) => ({
              content: update.content,
              createdAt: update.createdAt
            })),
            ...(context.subagent.queuedMessages?.length ? {
              queuedMessages: context.subagent.queuedMessages.map((message) => ({
                content: message.content,
                triggerTurn: message.triggerTurn
              }))
            } : {}),
            // This builder is an explicit allowlist, so a new SubagentRunRecord
            // field is dropped on every save until it is named here — with no
            // type error, because the object is built rather than spread.
            ...(context.subagent.structuredOutput === undefined
              ? {}
              : { structuredOutput: context.subagent.structuredOutput }),
            ...(context.subagent.outputSchema === undefined
              ? {}
              : { outputSchema: context.subagent.outputSchema }),
            // The host fingerprints the serialized record for its tool receipt, so
            // retaining run-only fields without persisting them would reject saves.
            ...(context.subagent.usage === undefined
              ? {}
              : { usage: context.subagent.usage })
          }
        } : {}),
        // The host's proof that this card's result came from its own execution.
        // Dropping it here would not lose a cosmetic field — it would strip the
        // card's only durable credential and get the card quarantined on the
        // very next save.
        ...(context.attestation ? { attestation: context.attestation } : {}),
        createdAt: context.createdAt
      };
    }
  }
}

/** Persisted layout stores assets and presets in top-level containers while the
 * in-memory `GlobalSettings` remains flat. */
interface PersistedAppDocument {
  schemaVersion: number;
  globalSettings: {
    appLanguage: AppDocument["globalSettings"]["appLanguage"];
    resolvedAppLanguage: AppDocument["globalSettings"]["resolvedAppLanguage"];
    theme: AppDocument["globalSettings"]["theme"];
    lastReasoningEffort: ReasoningEffort;
    activeProviderId: string | null;
    appearance: AppearancePreferences;
    shortcuts: GlobalSettings["shortcuts"];
    environmentTools: EnvironmentToolDefinition[];
  };
  assets: {
    apiProviders: AppDocument["globalSettings"]["apiProviders"];
    webSearch: AppDocument["globalSettings"]["webSearch"];
    mcpServers: McpServerConfig[];
    skills: SkillRecord[];
    executionEnvironments: ExecutionEnvironmentAssets;
  };
  presets: {
    conversationPresets: AppDocument["globalSettings"]["conversationPresets"];
    defaultConversationPresetId: string;
  };
  workspaces: AppDocument["workspaces"];
  tools: AppDocument["tools"];
  capabilities: AppDocument["capabilities"];
}

function documentForPersistence(document: AppDocument): PersistedAppDocument {
  const {
    conversationPresets,
    defaultConversationPresetId,
    apiProviders,
    webSearch,
    mcpServers,
    skills,
    executionEnvironments,
    ...coreGlobalSettings
  } = document.globalSettings;
  return {
    schemaVersion: document.schemaVersion,
    globalSettings: coreGlobalSettings,
    assets: {
      apiProviders,
      webSearch,
      mcpServers,
      skills,
      executionEnvironments
    },
    presets: {
      conversationPresets,
      defaultConversationPresetId
    },
    tools: document.tools,
    capabilities: document.capabilities,
    workspaces: document.workspaces.map((workspace) => ({
      ...workspace,
      conversations: workspace.conversations.map((conversation) => ({
        ...conversation,
        contexts: conversation.contexts.map(contextForPersistence),
        queuedMessages: conversation.queuedMessages.map((message) => ({
          id: message.id,
          content: message.content,
          ...(message.images?.length
            ? { images: message.images.map(imageAttachmentForPersistence) }
            : {}),
          createdAt: message.createdAt
        })),
        branches: conversation.branches.map((branch) => ({
          ...branch,
          contexts: branch.contexts.map(contextForPersistence)
        }))
      }))
    }))
  };
}

/**
 * Collect provider IDs from the previously persisted document so removed providers
 * can still have their preview credential markers deleted.
 */
function providerIdsFromStoredDocument(value: string | null): Set<string> {
  const result = new Set<string>();
  if (!value) return result;
  try {
    const root = record(JSON.parse(value));
    const assets = record(root?.assets);
    const providers = Array.isArray(assets?.apiProviders) ? assets.apiProviders : [];
    for (const entry of providers) {
      const provider = record(entry);
      const id = typeof provider?.id === "string" ? provider.id.trim() : "";
      if (id) result.add(id);
    }
    return result;
  } catch {
    return result;
  }
}

async function saveDocumentNow(document: AppDocument, durable: boolean): Promise<void> {
  const persisted = documentForPersistence(document);
  if (hasBackendRuntime()) {
    // The host persists conversation bodies; return an empty read-model list to
    // avoid overwriting them with the renderer snapshot.
    await invoke("save_document", {
      document: {
        ...persisted,
        workspaces: persisted.workspaces.map((workspace) => ({
          ...workspace,
          conversations: []
        }))
      },
      durable
    });
    return;
  }
  const previousProviderIds = providerIdsFromStoredDocument(
    window.localStorage.getItem(STORAGE_KEY)
  );
  const previousReferencedImages = referencedPreviewImageIdsFromStorage();
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(persisted));
  try {
    // A removed conversation/edit/branch starts a grace period. Images that
    // were never in the document (for example a live composer draft) are not
    // classified as orphans by an unrelated save.
    reconcilePreviewImageStore(
      previousReferencedImages,
      referencedPreviewImageIdsIn(persisted),
      false
    );
  } catch {
    // The canonical document save already succeeded; delayed cleanup can
    // retry on the next save or reload.
  }
  const nextProviderIds = new Set(persisted.assets.apiProviders.map((provider) => provider.id));
  await Promise.all([...previousProviderIds]
    .filter((providerId) => !nextProviderIds.has(providerId))
    .map((providerId) => queueSecretMutation(() => browserDeleteApiKey(providerId))));
}

export async function saveDocument(
  document: AppDocument,
  options: { immutableSnapshot?: boolean; durable?: boolean } = {}
): Promise<void> {
  // React state snapshots are immutable already. Callers that can uphold that
  // contract avoid a second full-document clone before IPC serialization.
  const snapshot = options.immutableSnapshot ? document : copyDocument(document);
  const operation = documentSaveTail
    .catch(() => undefined)
    .then(() => saveDocumentNow(snapshot, options.durable === true));
  documentSaveTail = operation;
  return operation;
}

export async function flushDocumentSaves(): Promise<void> {
  await documentSaveTail;
  if (hasBackendRuntime()) {
    await invoke("flush_document_saves");
  }
}

export async function requestToolApproval(request: ToolExecutionRequest): Promise<ToolApprovalGrant> {
  if (hasBackendRuntime()) return invoke<ToolApprovalGrant>("request_tool_approval", { request });
  return { nonce: createId("preview-approval"), expiresInMs: 90_000 };
}

/**
 * Answers one approval card. For a call the renderer started itself, the
 * returned grant carries the nonce — the backend mints it here, from the
 * arguments it classified when the card was raised, so the call that runs is
 * the one the user saw. For a card raised inside a model run, the blocked
 * worker is what resumes and the grant is empty.
 */
export async function resolveToolPrompt(
  promptId: string,
  decision: ToolPromptDecision,
  /** Only a denied plan-exit card carries one: what the model should change. */
  feedback?: string
): Promise<ToolApprovalGrant> {
  if (hasBackendRuntime()) {
    return invoke<ToolApprovalGrant>("resolve_tool_prompt", { promptId, decision, feedback });
  }
  if (decision === "deny") throw new Error("用户拒绝了这次工具执行");
  return { nonce: createId("preview-approval"), expiresInMs: 90_000 };
}

export async function executeTool(request: ToolExecutionRequest, approvalNonce?: string): Promise<ToolExecutionResponse> {
  if (hasBackendRuntime()) return invoke<ToolExecutionResponse>("execute_tool", { request, approvalNonce });
  const started = performance.now();
  await new Promise((resolve) => window.setTimeout(resolve, 280));
  return {
    success: true,
    output: `[浏览器预览] ${request.toolName} 已接收参数\n${JSON.stringify(request.input, null, 2)}`,
    executedAt: new Date().toISOString(),
    durationMs: Math.round(performance.now() - started)
  };
}

function detectImageMime(bytes: Uint8Array): ImageAttachment["mime"] {
  if (
    bytes.length >= 8
    && bytes[0] === 0x89
    && bytes[1] === 0x50
    && bytes[2] === 0x4e
    && bytes[3] === 0x47
    && bytes[4] === 0x0d
    && bytes[5] === 0x0a
    && bytes[6] === 0x1a
    && bytes[7] === 0x0a
  ) return "image/png";
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    bytes.length >= 6
    && bytes[0] === 0x47
    && bytes[1] === 0x49
    && bytes[2] === 0x46
    && bytes[3] === 0x38
    && (bytes[4] === 0x37 || bytes[4] === 0x39)
    && bytes[5] === 0x61
  ) return "image/gif";
  if (
    bytes.length >= 12
    && bytes[0] === 0x52
    && bytes[1] === 0x49
    && bytes[2] === 0x46
    && bytes[3] === 0x46
    && bytes[8] === 0x57
    && bytes[9] === 0x45
    && bytes[10] === 0x42
    && bytes[11] === 0x50
  ) return "image/webp";
  throw new Error("仅支持 PNG、JPEG、WebP 或静态 GIF 图片");
}

function isValidImageAttachmentName(name: string): boolean {
  return name.length > 0
    && new TextEncoder().encode(name).byteLength <= IMAGE_ATTACHMENT_MAX_NAME_BYTES
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(name);
}

function validatePreviewImageName(name: string): string {
  const normalized = name.trim();
  if (!isValidImageAttachmentName(normalized)) {
    throw new Error(
      `图片名称不能为空、不能含控制字符且不能超过 ${IMAGE_ATTACHMENT_MAX_NAME_BYTES} 字节`
    );
  }
  return normalized;
}

function skipGifSubBlocks(bytes: Uint8Array, start: number): number {
  let cursor = start;
  while (true) {
    const length = bytes[cursor];
    if (length === undefined) throw new Error("GIF 子块长度被截断");
    cursor += 1;
    if (length === 0) return cursor;
    cursor += length;
    if (cursor > bytes.length) throw new Error("GIF 子块数据被截断");
  }
}

function validatePreviewAnimationPolicy(bytes: Uint8Array, mime: ImageAttachment["mime"]): void {
  if (mime !== "image/gif") return;
  if (bytes.length < 13) throw new Error("GIF 文件头无效");
  let cursor = 13;
  const packed = bytes[10];
  if ((packed & 0x80) !== 0) {
    cursor += 3 * (1 << ((packed & 0x07) + 1));
    if (cursor > bytes.length) throw new Error("GIF 全局调色板被截断");
  }
  let frames = 0;
  while (true) {
    const marker = bytes[cursor];
    if (marker === undefined) throw new Error("GIF 数据缺少结束标记");
    if (marker === 0x3b) {
      if (frames !== 1) throw new Error("GIF 必须包含且仅包含一帧图片");
      return;
    }
    if (marker === 0x21) {
      cursor += 2;
      if (cursor > bytes.length) throw new Error("GIF 扩展块位置被截断");
      cursor = skipGifSubBlocks(bytes, cursor);
      continue;
    }
    if (marker === 0x2c) {
      frames += 1;
      if (frames > 1) {
        throw new Error("仅支持单帧 GIF；动画 GIF 无法作为跨 API 图片输入");
      }
      cursor += 10;
      if (cursor > bytes.length) throw new Error("GIF 图像描述块被截断");
      const localPacked = bytes[cursor - 1];
      if ((localPacked & 0x80) !== 0) {
        cursor += 3 * (1 << ((localPacked & 0x07) + 1));
        if (cursor > bytes.length) throw new Error("GIF 局部调色板被截断");
      }
      cursor += 1;
      if (cursor > bytes.length) throw new Error("GIF LZW 参数被截断");
      cursor = skipGifSubBlocks(bytes, cursor);
      continue;
    }
    throw new Error(`GIF 包含未知数据块 0x${marker.toString(16).padStart(2, "0")}`);
  }
}

function detectImageDimensions(bytes: Uint8Array, mime: string): { width: number; height: number } {
  if (mime === "image/png" && bytes.length >= 24) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return { width: view.getUint32(16), height: view.getUint32(20) };
  }
  if (mime === "image/gif" && bytes.length >= 10) {
    return {
      width: bytes[6] | (bytes[7] << 8),
      height: bytes[8] | (bytes[9] << 8)
    };
  }
  if (mime === "image/jpeg") {
    let offset = 2;
    const dimensionMarkers = new Set([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf]);
    while (offset + 8 < bytes.length) {
      if (bytes[offset] !== 0xff) {
        offset += 1;
        continue;
      }
      const marker = bytes[offset + 1];
      if (dimensionMarkers.has(marker)) {
        return {
          height: (bytes[offset + 5] << 8) | bytes[offset + 6],
          width: (bytes[offset + 7] << 8) | bytes[offset + 8]
        };
      }
      const length = (bytes[offset + 2] << 8) | bytes[offset + 3];
      if (length < 2) break;
      offset += 2 + length;
    }
  }
  if (mime === "image/webp" && bytes.length >= 30) {
    const chunk = String.fromCharCode(bytes[12], bytes[13], bytes[14], bytes[15]);
    if (chunk === "VP8X") {
      return {
        width: 1 + bytes[24] + (bytes[25] << 8) + (bytes[26] << 16),
        height: 1 + bytes[27] + (bytes[28] << 8) + (bytes[29] << 16)
      };
    }
    if (chunk === "VP8 " && bytes.length >= 30) {
      return {
        width: (bytes[26] | (bytes[27] << 8)) & 0x3fff,
        height: (bytes[28] | (bytes[29] << 8)) & 0x3fff
      };
    }
    if (chunk === "VP8L" && bytes.length >= 25) {
      return {
        width: 1 + (((bytes[22] & 0x3f) << 8) | bytes[21]),
        height: 1 + (((bytes[24] & 0x0f) << 10) | (bytes[23] << 2) | ((bytes[22] & 0xc0) >> 6))
      };
    }
  }
  return { width: 0, height: 0 };
}

function validatePreviewImageDimensions(dimensions: { width: number; height: number }): void {
  const pixels = dimensions.width * dimensions.height;
  if (
    dimensions.width <= 0
    || dimensions.height <= 0
    || dimensions.width > IMAGE_ATTACHMENT_MAX_DIMENSION
    || dimensions.height > IMAGE_ATTACHMENT_MAX_DIMENSION
    || !Number.isSafeInteger(pixels)
    || pixels > IMAGE_ATTACHMENT_MAX_PIXELS
  ) {
    throw new Error(
      `浏览器预览中的图片宽高必须为 1–${IMAGE_ATTACHMENT_MAX_DIMENSION} 像素，且总像素不能超过 16 MP`
    );
  }
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, Math.min(bytes.length, offset + 0x8000)));
  }
  return window.btoa(binary);
}

function strictBase64ToBytes(encoded: string): Uint8Array {
  const maxEncodedLength = 4 * Math.ceil(PREVIEW_IMAGE_MAX_BYTES / 3);
  if (
    encoded.length === 0
    || encoded.length > maxEncodedLength
    || encoded.length % 4 !== 0
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(encoded)
  ) {
    throw new Error("图片 base64 数据无效");
  }
  const binary = window.atob(encoded);
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  if (bytes.byteLength === 0 || bytes.byteLength > PREVIEW_IMAGE_MAX_BYTES) {
    throw new Error("浏览器预览图片字节数无效");
  }
  return bytes;
}

async function browserImageAttachmentId(bytes: Uint8Array): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error("浏览器预览缺少安全哈希能力，无法保存图片");
  }
  const digestInput = new Uint8Array(bytes.byteLength);
  digestInput.set(bytes);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", digestInput.buffer);
  return Array.from(new Uint8Array(digest)).map((value) => value.toString(16).padStart(2, "0")).join("");
}

interface PreviewImageIndexEntry {
  id: string;
  characters: number;
  touchedAt: number;
  orphanedAt?: number;
}

function previewImageIndex(): PreviewImageIndexEntry[] {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(IMAGE_ATTACHMENT_INDEX_KEY) ?? "[]") as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.flatMap((entry) => {
      const value = record(entry);
      const orphanedAt = optionalNonNegativeInteger(value?.orphanedAt);
      return value
        && typeof value.id === "string"
        && optionalNonNegativeInteger(value.characters) !== undefined
        && optionalNonNegativeInteger(value.touchedAt) !== undefined
        ? [{
            id: value.id,
            characters: value.characters as number,
            touchedAt: value.touchedAt as number,
            ...(orphanedAt === undefined ? {} : { orphanedAt })
          }]
        : [];
    });
  } catch {
    return [];
  }
}

function referencedPreviewImageIdsIn(value: unknown): Set<string> {
  const ids = new Set<string>();
  const addImages = (candidate: unknown) => {
    if (!Array.isArray(candidate)) return;
    candidate.forEach((image) => {
      const attachment = record(image);
      if (typeof attachment?.id === "string" && /^[0-9a-f]{64}$/u.test(attachment.id)) {
        ids.add(attachment.id);
      }
    });
  };
  const visitContexts = (candidate: unknown) => {
    if (!Array.isArray(candidate)) return;
    candidate.forEach((entry) => {
      const context = record(entry);
      if (!context) return;
      if (context.kind === "user") addImages(context.images);
      if (context.kind === "tool") {
        addImages(record(context.result)?.images);
        visitContexts(record(context.subagent)?.contexts);
      }
    });
  };
  const document = record(value);
  if (!document || !Array.isArray(document.workspaces)) return ids;
  document.workspaces.forEach((workspaceValue) => {
    const workspace = record(workspaceValue);
    if (!Array.isArray(workspace?.conversations)) return;
    workspace.conversations.forEach((conversationValue) => {
      const conversation = record(conversationValue);
      if (!conversation) return;
      visitContexts(conversation.contexts);
      if (Array.isArray(conversation.queuedMessages)) {
        conversation.queuedMessages.forEach((message) => addImages(record(message)?.images));
      }
      if (Array.isArray(conversation.branches)) {
        conversation.branches.forEach((branch) => visitContexts(record(branch)?.contexts));
      }
    });
  });
  return ids;
}

function referencedPreviewImageIdsFromStorage(): Set<string> {
  try {
    return referencedPreviewImageIdsIn(JSON.parse(
      window.localStorage.getItem(STORAGE_KEY) ?? "null"
    ));
  } catch {
    // A corrupt preview document is handled by browserLoad; do not delete attachments optimistically.
    return new Set();
  }
}

function writePreviewImageIndex(index: PreviewImageIndexEntry[]): void {
  window.localStorage.setItem(IMAGE_ATTACHMENT_INDEX_KEY, JSON.stringify(index));
}

function reconcilePreviewImageStore(
  previousReferenced: Set<string>,
  nextReferenced: Set<string>,
  markAllUnreferenced: boolean
): void {
  const now = Date.now();
  const next: PreviewImageIndexEntry[] = [];
  for (const entry of previewImageIndex()) {
    const storageKey = `${IMAGE_ATTACHMENT_STORAGE_PREFIX}${entry.id}`;
    if (window.localStorage.getItem(storageKey) === null) continue;
    if (nextReferenced.has(entry.id)) {
      next.push({
        id: entry.id,
        characters: entry.characters,
        touchedAt: entry.touchedAt
      });
      continue;
    }
    const orphanedAt = entry.orphanedAt
      ?? ((markAllUnreferenced || previousReferenced.has(entry.id)) ? now : undefined);
    if (orphanedAt !== undefined && now - orphanedAt >= PREVIEW_IMAGE_ORPHAN_GRACE_MS) {
      window.localStorage.removeItem(storageKey);
      continue;
    }
    next.push({
      ...entry,
      ...(orphanedAt === undefined ? {} : { orphanedAt })
    });
  }
  writePreviewImageIndex(next);
}

function clearPreviewImageStore(): void {
  const keys: string[] = [];
  for (let index = 0; index < window.localStorage.length; index += 1) {
    const key = window.localStorage.key(index);
    if (key?.startsWith(IMAGE_ATTACHMENT_STORAGE_PREFIX)) keys.push(key);
  }
  keys.forEach((key) => window.localStorage.removeItem(key));
  window.localStorage.removeItem(IMAGE_ATTACHMENT_INDEX_KEY);
}

function makePreviewImageStorageRoom(incomingId: string, incomingCharacters: number): PreviewImageIndexEntry[] {
  const referenced = referencedPreviewImageIdsFromStorage();
  const now = Date.now();
  const index = previewImageIndex()
    .filter((entry) => entry.id !== incomingId)
    .filter((entry) => window.localStorage.getItem(`${IMAGE_ATTACHMENT_STORAGE_PREFIX}${entry.id}`) !== null)
    .sort((left, right) => left.touchedAt - right.touchedAt);
  let characters = index.reduce((total, entry) => total + entry.characters, 0) + incomingCharacters;
  while (
    index.length + 1 > PREVIEW_IMAGE_STORAGE_COUNT
    || characters > PREVIEW_IMAGE_STORAGE_CHARACTERS
  ) {
    const removableIndex = index.findIndex((entry) => (
      !referenced.has(entry.id)
      && entry.orphanedAt !== undefined
      && now - entry.orphanedAt >= PREVIEW_IMAGE_ORPHAN_GRACE_MS
    ));
    if (removableIndex < 0) break;
    const [removed] = index.splice(removableIndex, 1);
    characters -= removed.characters;
    window.localStorage.removeItem(`${IMAGE_ATTACHMENT_STORAGE_PREFIX}${removed.id}`);
  }
  if (
    index.length + 1 > PREVIEW_IMAGE_STORAGE_COUNT
    || characters > PREVIEW_IMAGE_STORAGE_CHARACTERS
  ) {
    throw new Error("浏览器预览图片存储已满；请删除不再需要的旧对话图片后重试");
  }
  return index;
}

/** Stores image bytes outside the main document and returns its lightweight reference. */
export async function prepareImageAttachment(name: string, bytes: Uint8Array): Promise<ImageAttachment> {
  if (hasBackendRuntime()) {
    return invoke<ImageAttachment>("image_attachment_upload", { name, data: bytesToBase64(bytes) });
  }
  const normalizedName = validatePreviewImageName(name);
  if (bytes.byteLength > PREVIEW_IMAGE_MAX_BYTES) {
    throw new Error("浏览器预览中的单张图片不能超过 3 MiB");
  }
  const mime = detectImageMime(bytes);
  validatePreviewAnimationPolicy(bytes, mime);
  const dimensions = detectImageDimensions(bytes, mime);
  validatePreviewImageDimensions(dimensions);
  const attachment: ImageAttachment = {
    id: await browserImageAttachmentId(bytes),
    name: normalizedName,
    mime,
    ...dimensions,
    bytes: bytes.byteLength
  };
  const dataUrl = `data:${mime};base64,${bytesToBase64(bytes)}`;
  const storageKey = `${IMAGE_ATTACHMENT_STORAGE_PREFIX}${attachment.id}`;
  const previous = window.localStorage.getItem(storageKey);
  try {
    const index = makePreviewImageStorageRoom(attachment.id, dataUrl.length);
    window.localStorage.setItem(
      storageKey,
      JSON.stringify({ attachment, dataUrl })
    );
    writePreviewImageIndex([
      ...index,
      { id: attachment.id, characters: dataUrl.length, touchedAt: Date.now() }
    ]);
  } catch (error) {
    try {
      if (previous === null) window.localStorage.removeItem(storageKey);
      else window.localStorage.setItem(storageKey, previous);
    } catch {
      // Preserve the original quota/storage error below.
    }
    throw error instanceof Error && error.message.startsWith("浏览器预览")
      ? error
      : new Error("浏览器预览无法保存图片；本地存储空间可能不足");
  }
  return attachment;
}

/** Resolves one attachment to a displayable data URL without putting bytes in the document. */
export async function imageAttachmentData(imageId: string): Promise<string> {
  if (hasBackendRuntime()) return invoke<string>("image_attachment_data", { imageId });
  const stored = window.localStorage.getItem(`${IMAGE_ATTACHMENT_STORAGE_PREFIX}${imageId}`);
  if (!stored) throw new Error(`图片 ${imageId} 不存在`);
  try {
    const input = record(JSON.parse(stored));
    const attachment = record(input?.attachment);
    const mime = attachment?.mime;
    const normalizedAttachment = normalizeImageAttachments([attachment])?.[0];
    if (
      normalizedAttachment?.id !== imageId
      || typeof input?.dataUrl !== "string"
      || !input.dataUrl.startsWith(`data:${mime};base64,`)
    ) throw new Error();
    const bytes = strictBase64ToBytes(input.dataUrl.slice(`data:${mime};base64,`.length));
    if (bytes.byteLength !== normalizedAttachment.bytes) throw new Error();
    const detectedMime = detectImageMime(bytes);
    if (detectedMime !== normalizedAttachment.mime) throw new Error();
    validatePreviewAnimationPolicy(bytes, detectedMime);
    const dimensions = detectImageDimensions(bytes, detectedMime);
    validatePreviewImageDimensions(dimensions);
    if (
      dimensions.width !== normalizedAttachment.width
      || dimensions.height !== normalizedAttachment.height
      || await browserImageAttachmentId(bytes) !== imageId
    ) throw new Error();
    try {
      const current = previewImageIndex().find((entry) => entry.id === imageId);
      const index = previewImageIndex().filter((entry) => entry.id !== imageId);
      writePreviewImageIndex([
        ...index,
        {
          id: imageId,
          characters: input.dataUrl.length,
          touchedAt: Date.now(),
          ...(current?.orphanedAt === undefined ? {} : { orphanedAt: current.orphanedAt })
        }
      ]);
    } catch {
      // Reading an existing attachment should still work if index maintenance hits quota.
    }
    return input.dataUrl;
  } catch {
    throw new Error(`图片 ${imageId} 已损坏`);
  }
}

export async function refreshCapabilities(): Promise<CapabilityCatalog> {
  if (hasBackendRuntime()) return invoke<CapabilityCatalog>("discover_capabilities");
  return (await browserLoad()).capabilities;
}

/**
 * Host commands for settings pages have no browser-preview fallback: probing MCP
 * servers, copying skill directories, and launching PATH processes cannot be
 * represented faithfully in a browser. Callers use `hasBackendRuntime()` to gate UI.
 */
export async function probeMcpServer(
  server: McpServerConfig,
  probeId: string
): Promise<McpProbeReport> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法探测 MCP 服务器");
  return invoke<McpProbeReport>("mcp_probe_server", { server, probeId });
}

/** Cancels the probe `probeId` names. The host answers whether it accepted it. */
export async function cancelMcpProbe(probeId: string): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法探测 MCP 服务器");
  return invoke<void>("mcp_cancel_probe", { probeId });
}

/** When `path` is absent, the host opens a system picker; `null` means cancellation. */
export async function installSkillFromDirectory(path?: string): Promise<SkillRecord | null> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法安装技能");
  return invoke<SkillRecord | null>("skill_install_directory", { path: path ?? null });
}

export async function installSkillFromArchive(path?: string): Promise<SkillRecord | null> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法安装技能");
  return invoke<SkillRecord | null>("skill_install_archive", { path: path ?? null });
}

export async function uninstallSkill(folderName: string): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法卸载技能");
  await invoke<void>("skill_uninstall", { folderName });
}

export async function scanSystemSkills(): Promise<SystemSkillCandidate[]> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法扫描系统技能");
  return invoke<SystemSkillCandidate[]>("skill_scan_system");
}

/**
 * Search three online skill registries through the host. Production CSP allows only
 * `connect-src ipc:`, and an individual source failure is reported without failing
 * the aggregate search.
 */
export async function searchSkillRegistries(query: string): Promise<SkillSearchReport> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法搜索在线技能");
  return invoke<SkillSearchReport>("skill_search_registries", { query });
}

/** Install a remote skill using its opaque search-result handle. */
export async function installSkillFromRegistry(installSource: string): Promise<SkillRecord> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法安装技能");
  return invoke<SkillRecord>("skill_install_remote", { installSource });
}

export async function environmentToolSnapshots(): Promise<EnvironmentToolSnapshot[]> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法探测环境依赖");
  return invoke<EnvironmentToolSnapshot[]>("environment_tool_snapshots");
}

/** Enumerate installed WSL distros. Browser preview returns an empty list, which is a
 * normal absence of available distros rather than an error. */
export async function listWslDistros(): Promise<WslDistro[]> {
  if (!hasBackendRuntime()) return [];
  return invoke<WslDistro[]>("list_wsl_distros");
}

export async function revealEnvironmentTool(executable: string): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法打开目录");
  await invoke<void>("reveal_environment_tool", { executable });
}

/** Running version and install flavor; no network. */
export async function appVersionInfo(): Promise<AppVersionInfo> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法读取版本信息");
  return invoke<AppVersionInfo>("app_version_info");
}

/** Ask GitHub Releases for the latest version. The host owns the request; the WebView's CSP
 * has no route to github.com. */
export async function checkAppUpdate(): Promise<AppUpdateCheck> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法检查更新");
  return invoke<AppUpdateCheck>("check_app_update");
}

/**
 * Download the asset a check selected. `onProgress` receives byte counts while the transfer
 * runs and a `verifying` marker while the host compares the digest with `SHA256SUMS`. Rejects
 * with the host's `下载已取消` message after `cancelAppUpdateDownload`.
 */
export async function downloadAppUpdate(
  asset: AppReleaseAsset,
  checksumsAsset: AppReleaseAsset | null,
  onProgress: (event: AppUpdateDownloadEvent) => void
): Promise<AppUpdateDownload> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法下载更新");
  const channel = new Channel<AppUpdateDownloadEvent>();
  channel.onmessage = onProgress;
  return invoke<AppUpdateDownload>("download_app_update", {
    asset,
    checksumsAsset,
    onProgress: channel
  });
}

export async function cancelAppUpdateDownload(): Promise<void> {
  if (!hasBackendRuntime()) return;
  await invoke<void>("cancel_app_update_download");
}

/**
 * Hand the downloaded file over. For the installer flavor the host starts the installer and
 * exits the app, so the returned promise may never settle; callers must not wait on it to
 * update their UI. For the portable flavor the archive is shown in the file manager.
 */
export async function installAppUpdate(path: string): Promise<AppUpdateInstallOutcome> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法安装更新");
  return invoke<AppUpdateInstallOutcome>("install_app_update", { path });
}

let secretMutationTail: Promise<void> = Promise.resolve();

function queueSecretMutation<T>(operation: () => Promise<T>): Promise<T> {
  const result = secretMutationTail.catch(() => undefined).then(operation);
  secretMutationTail = result.then(() => undefined, () => undefined);
  return result;
}

async function saveApiKeyNow(provider: ApiProvider, apiKey: string): Promise<ApiKeyStatus> {
  const secret = apiKey.trim();
  if (!secret) throw new Error("API Key 不能为空");
  const keyLength = Array.from(secret).length;
  if (hasBackendRuntime()) {
    const result = await invoke<ApiKeyStatus | boolean | null>("save_api_key", { provider, apiKey: secret });
    const status = typeof result === "object" && result && typeof result.configured === "boolean"
      ? result
      : { configured: result !== false };
    await rememberApiKeyLength(provider.id, keyLength);
    return { ...status, keyLength };
  }
  // Browser preview never performs a real network call, so retaining the supplied secret has no
  // benefit. Keep only the non-secret length metadata that draws the mask.
  await rememberApiKeyLength(provider.id, keyLength);
  return { configured: true, keyLength };
}

export async function saveApiKey(provider: ApiProvider, apiKey: string): Promise<ApiKeyStatus> {
  return queueSecretMutation(() => saveApiKeyNow(provider, apiKey));
}

export async function revealApiKey(provider: ApiProvider): Promise<string> {
  await secretMutationTail;
  if (!hasBackendRuntime()) throw new Error("浏览器预览不会保留 API Key 明文");
  const secret = await invoke<string>("reveal_api_key", { provider });
  await rememberApiKeyLength(provider.id, Array.from(secret).length);
  return secret;
}

export async function deleteApiKey(provider: ApiProvider): Promise<ApiKeyStatus> {
  return queueSecretMutation(async () => {
    const status = hasBackendRuntime()
      ? await invoke<ApiKeyStatus>("delete_api_key", { provider })
      : await browserDeleteApiKey(provider.id);
    await forgetStoredApiKeyLength(provider.id);
    return status;
  });
}

export async function codexOauthSignIn(provider: ApiProvider): Promise<CodexOauthStatus> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法登录 ChatGPT");
  return invoke<CodexOauthStatus>("codex_oauth_sign_in", { provider });
}

export async function codexOauthCancelSignIn(provider: ApiProvider): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法登录 ChatGPT");
  await invoke("codex_oauth_cancel_sign_in", { provider });
}

export async function codexOauthStatus(provider: ApiProvider): Promise<CodexOauthStatus> {
  if (!hasBackendRuntime()) return { signedIn: false, signingIn: false, account: null };
  return invoke<CodexOauthStatus>("codex_oauth_status", { provider });
}

export async function codexOauthSignOut(provider: ApiProvider): Promise<CodexOauthStatus> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法登录 ChatGPT");
  return invoke<CodexOauthStatus>("codex_oauth_sign_out", { provider });
}

/**
 * Read the local Claude Code login. There is no preview fallback: a fabricated
 * "signed out" would invite the user to run a login the preview cannot start.
 */
export async function claudeAgentLoginStatus(provider: ApiProvider): Promise<ClaudeAgentLoginStatus> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法读取 Claude Code 登录状态");
  return invoke<ClaudeAgentLoginStatus>("claude_agent_login_status", { provider });
}

export async function claudeAgentOpenLogin(provider: ApiProvider): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法读取 Claude Code 登录状态");
  await invoke("claude_agent_open_login", { provider });
}

async function browserDeleteApiKey(providerId: string): Promise<ApiKeyStatus> {
  await rememberApiKeyLength(providerId);
  return { configured: false };
}

/**
 * Fetch a provider's model catalog as a configuration-time action. It requires only
 * a base URL and usually a key; enabled status is required only for conversation
 * requests, which are checked by `runModel` and `validate_run_request`.
 */
export async function fetchModels(provider: ApiProvider): Promise<ModelProfile[]> {
  if (hasBackendRuntime()) {
    // Normalize discovered models because the host omits empty `name` and `group`
    // fields while `ModelProfile` requires strings and the renderer calls `.trim()`.
    const discovered = await invoke<unknown[]>("fetch_models", { provider });
    return discovered
      .map((model) => normalizeModel(model, provider.family))
      .filter((model): model is ModelProfile => Boolean(model));
  }
  // Browser preview lacks host discovery policy and catalog. Its fixture must match
  // the live shape, including `group`, so preview grouping matches desktop behavior.
  const common = {
    reasoningContent: normalizeReasoningContent(undefined, provider.family)
  };
  if (provider.family === "anthropic") {
    return [
      {
        id: "claude-sonnet-preview",
        name: "Claude Sonnet (preview)",
        group: "claude",
        contextWindow: 200000,
        maxOutputTokens: 64000,
        capabilities: ["image_recognition"],
        ...common
      },
      {
        id: "claude-opus-preview",
        name: "Claude Opus (preview)",
        group: "claude",
        contextWindow: 200000,
        maxOutputTokens: 64000,
        capabilities: ["image_recognition"],
        ...common
      }
    ];
  }
  if (provider.family === "claude_agent") {
    // The Claude Agent family has no `GET /models`: the host serves a built-in
    // registry, so the preview mirrors it row for row.
    return CLAUDE_AGENT_REGISTRY.map(({ id, name, contextWindow, maxOutputTokens }) => ({
      id,
      name,
      group: "claude",
      contextWindow,
      maxOutputTokens,
      capabilities: ["image_recognition"],
      ...common
    }));
  }
  return [
    {
      id: "gpt-preview",
      name: "GPT (preview)",
      group: "gpt",
      contextWindow: 128000,
      maxOutputTokens: 32768,
      capabilities: ["image_recognition"],
      ...common
    },
    {
      id: "gpt-mini-preview",
      name: "GPT Mini (preview)",
      group: "gpt",
      contextWindow: 128000,
      maxOutputTokens: 16384,
      capabilities: ["image_recognition"],
      ...common
    },
    {
      id: "text-embedding-preview",
      name: "Text Embedding (preview)",
      group: "text",
      contextWindow: 8192,
      capabilities: [],
      ...common
    }
  ];
}

function modelRequestWithThinkingSelection<T extends { reasoningEffort: ReasoningEffort }>(
  request: T
): Omit<T, "reasoningEffort"> & (
  | { thinkingMode: "disabled" }
  | { thinkingEffort: Exclude<ReasoningEffort, "disabled"> }
) {
  const { reasoningEffort, ...rest } = request;
  return reasoningEffort === "disabled"
    ? { ...rest, thinkingMode: "disabled" }
    : { ...rest, thinkingEffort: reasoningEffort };
}

export async function runModel(
  request: ModelRunRequest,
  onEvent: (event: ModelStreamEvent) => void,
  requestId: string
): Promise<ModelRunResponse> {
  if (!request.provider.enabled) {
    throw new Error(`API 提供商 ${request.provider.name} 未启用，不能执行聊天请求`);
  }
  if (hasBackendRuntime()) {
    type ModelChannelEvent = ModelStreamEvent
      | { type: "debug_request_body"; round: number; body: unknown };
    const channel = new Channel<ModelChannelEvent>();
    channel.onmessage = (event) => {
      if (event.type === "debug_request_body") {
        console.log(`[dev] 模型 API 请求体（第 ${event.round} 轮）：`, event.body);
        return;
      }
      onEvent(event);
    };
    return invoke<ModelRunResponse>("run_model", {
      request: modelRequestWithThinkingSelection(request),
      forkPromptContextId: request.forkPromptContextId,
      requestId,
      onEvent: channel
    });
  }
  const cancellation: PreviewRunControl = { cancelled: false, steers: [] };
  previewRunCancellations.set(requestId, cancellation);
  try {
    const started = performance.now();
    const latestUser = [...request.contexts].reverse().find((context) => context.kind === "user");
    const text = latestUser && "content" in latestUser ? latestUser.content ?? "" : "";
    const reasoning = request.reasoningEffort === "disabled"
      ? ""
      : `【浏览器预览】正在以 ${request.reasoningEffort} 程度检查当前对话与工作区上下文。`;
    const output = `【浏览器预览】${request.provider.name} / ${request.model.id} 已接收请求。${text ? `\n\n用户输入：${text}` : ""}`;
    const ensureRunning = () => {
      if (cancellation.cancelled) throw new Error("模型运行已停止");
    };
    for (const delta of reasoning.match(/[\s\S]{1,8}/g) ?? []) {
      await new Promise((resolve) => window.setTimeout(resolve, 4));
      ensureRunning();
      onEvent({ type: "reasoning_delta", round: 1, delta });
    }
    if (reasoning) onEvent({ type: "reasoning_done", round: 1 });
    for (const delta of output.match(/[\s\S]{1,8}/g) ?? []) {
      await new Promise((resolve) => window.setTimeout(resolve, 4));
      ensureRunning();
      onEvent({ type: "text_delta", round: 1, delta });
    }
    const inputTokens = Math.max(1, Math.ceil(text.length / 3));
    const outputTokens = Math.ceil((reasoning.length + output.length) / 3);
    const modelTurnId = `model-turn-${requestId}-1`;
    const contexts: ContextItem[] = [
        ...(reasoning ? [{
          id: createId("ctx"),
          kind: "reasoning" as const,
          content: reasoning,
          // The preview fixture generates its own plaintext reasoning content locally.
          form: "plaintext" as const,
          round: 1,
          modelTurnId,
          createdAt: new Date().toISOString()
        }] : []),
        { id: createId("ctx"), kind: "assistant", content: output, round: 1, modelTurnId, createdAt: new Date().toISOString() }
    ];
    let round = 2;
    while (cancellation.steers.length) {
      ensureRunning();
      const steers = cancellation.steers.splice(0);
      for (const steer of steers) {
        onEvent({
          type: "user_input_received",
          round,
          id: steer.id,
          content: steer.content,
          images: steer.images,
          createdAt: steer.createdAt
        });
        contexts.push({
          id: steer.id,
          kind: "user",
          content: steer.content,
          images: steer.images,
          createdAt: steer.createdAt
        });
      }
      const steeredOutput = `【浏览器预览】已在当前回合接收引导：${steers.map((steer) =>
        steer.content || `（${steer.images?.length ?? 0} 张图片）`
      ).join("\n")}`;
      for (const delta of steeredOutput.match(/[\s\S]{1,8}/g) ?? []) {
        await new Promise((resolve) => window.setTimeout(resolve, 4));
        ensureRunning();
        onEvent({ type: "text_delta", round, delta });
      }
      contexts.push({
        id: createId("ctx"),
        kind: "assistant",
        content: steeredOutput,
        round,
        modelTurnId: `model-turn-${requestId}-${round}`,
        createdAt: new Date().toISOString()
      });
      round += 1;
    }
    return {
      contexts,
      usage: { inputTokens, outputTokens, totalTokens: inputTokens + outputTokens },
      model: request.model.id,
      providerName: request.provider.name,
      durationMs: Math.round(performance.now() - started),
      stopReason: "browser_preview",
      contextTokens: inputTokens + outputTokens
    };
  } finally {
    if (previewRunCancellations.get(requestId) === cancellation) {
      previewRunCancellations.delete(requestId);
    }
  }
}

export async function cancelModelRun(requestId: string): Promise<boolean> {
  if (hasBackendRuntime()) return invoke<boolean>("cancel_model_run", { requestId });
  const cancellation = previewRunCancellations.get(requestId);
  if (!cancellation) return false;
  cancellation.cancelled = true;
  return true;
}

/** Cancel the current run by conversation when a reload has lost the renderer's
 * request ID. Browser preview tracks runs only by request ID, so it returns false. */
export async function cancelConversationRun(conversationId: string): Promise<boolean> {
  if (hasBackendRuntime()) {
    return invoke<boolean>("cancel_conversation_run", { conversationId });
  }
  return false;
}

export interface ResumableRun {
  conversationId: string;
  requestId: string;
  running: boolean;
}

export interface RunSettlementPayload {
  response?: ModelRunResponse;
  error?: string;
}

export type AttachRunResult =
  | { status: "running"; requestId: string; request: ModelRunRequest; droppedEvents: number }
  | { status: "finished"; requestId: string; request: ModelRunRequest; settlement: RunSettlementPayload }
  | { status: "none" };

/** List host runs that are live or have unclaimed settlement. Browser preview has none. */
export async function listResumableRuns(): Promise<ResumableRun[]> {
  if (hasBackendRuntime()) return invoke<ResumableRun[]>("list_resumable_runs");
  return [];
}

/** Rescan conversations with deliverable results and no active run after adoption.
 * `taskSettled` is an edge notification that can be evicted from bounded host backlog
 * or lost when renderer state reloads. Browser preview has no pending wakeups. */
export async function listWakePendingConversations(): Promise<string[]> {
  if (hasBackendRuntime()) return invoke<string[]>("list_wake_pending_conversations");
  return [];
}

/** List approval cards awaiting answers. Attach replay restores run-owned cards, but
 * background-task cards can outlive every open stream and need restoration after
 * startup or reconnection. Browser preview has none. */
export async function listPendingToolPrompts(): Promise<
  (PendingToolPrompt & { conversationId: string })[]
> {
  if (hasBackendRuntime()) {
    return invoke<(PendingToolPrompt & { conversationId: string })[]>(
      "list_pending_tool_prompts"
    );
  }
  return [];
}

/** Load the conversation's plan document, or null when it has none. The plan is
 * host state, not part of the document snapshot, so browser preview has none. */
export async function loadConversationPlan(conversationId: string): Promise<ConversationPlan | null> {
  if (hasBackendRuntime()) {
    return invoke<ConversationPlan | null>("load_conversation_plan", { conversationId });
  }
  return null;
}

/** List the model's fork requests still waiting for the user, oldest first. The card
 * belongs to no run, so nothing but this re-lists it after a reload. */
export async function listPendingForkRequests(): Promise<PendingForkRequest[]> {
  if (hasBackendRuntime()) {
    return invoke<PendingForkRequest[]>("list_pending_fork_requests");
  }
  return [];
}

export async function workflowRunHistory(conversationId: string): Promise<import("./workflowRuns").WorkflowHistoryRun[]> {
  return hasBackendRuntime() ? invoke("workflow_run_history", { conversationId }) : [];
}

export interface PendingForkStart {
  workspaceId: string;
  conversationId: string;
  promptContextId: string;
}

export async function listPendingForkStarts(): Promise<PendingForkStart[]> {
  return hasBackendRuntime() ? invoke<PendingForkStart[]>("list_pending_fork_starts") : [];
}

/** Answer one fork card. Approval creates the child and returns it; the `forkResolved`
 * push event — not this result — is what starts the child's run, so the auto-approved
 * path and this one share one starter. */
export async function resolveForkRequest(
  forkId: string,
  approved: boolean
): Promise<Conversation | null> {
  if (!hasBackendRuntime()) return null;
  return invoke<Conversation | null>("resolve_fork_request", { forkId, approved });
}

/** Attach a conversation's host event stream, replaying buffered events into `onEvent`. */
export async function attachModelRun(
  conversationId: string,
  onEvent: (event: ModelStreamEvent) => void
): Promise<AttachRunResult> {
  if (!hasBackendRuntime()) return { status: "none" };
  type ModelChannelEvent = ModelStreamEvent
    | { type: "debug_request_body"; round: number; body: unknown };
  const channel = new Channel<ModelChannelEvent>();
  channel.onmessage = (event) => {
    if (event.type === "debug_request_body") return;
    onEvent(event);
  };
  return invoke<AttachRunResult>("attach_model_run", { conversationId, onEvent: channel });
}

/** Claim a `run_concluded` settlement, returning `null` when none is available. */
export async function takeRunSettlement(
  conversationId: string
): Promise<RunSettlementPayload | null> {
  if (!hasBackendRuntime()) return null;
  return invoke<RunSettlementPayload | null>("take_run_settlement", { conversationId });
}

export async function steerModelRun(
  requestId: string,
  message: QueuedMessage
): Promise<void> {
  if (hasBackendRuntime()) {
    await invoke("steer_model_run", {
      requestId,
      messageId: message.id,
      content: message.content,
      images: message.images,
      createdAt: message.createdAt
    });
    return;
  }
  const control = previewRunCancellations.get(requestId);
  if (!control) throw new Error("模型回合已经结束；消息仍保留在队列中");
  control.steers.push(message);
}

/**
 * Asks a live workflow run to skip one step.
 *
 * Skip only. A step the scheduler has already settled cannot be re-opened — the
 * plan is a one-way state machine and the cache chain latches on its first miss
 * — so there is no retry to offer, and the host refuses the word outright
 * rather than accepting it and doing nothing.
 */
export async function skipWorkflowStep(
  requestId: string,
  runId: string,
  stepIndex: number
): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("当前页面没有连接 Rust 后端");
  await invoke("workflow_step_control", {
    requestId,
    runId,
    stepIndex,
    action: "skip"
  });
}

/**
 * Loads one workflow step's externalized full record from the run directory.
 *
 * The timeline context only carries a preview and the retrieval coordinates
 * (`runId` + `stepIndex`); the drawer calls this when it needs the transcript.
 * `null` is a legal answer, not an error: run directories are deleted with
 * their conversation, so a dangling coordinate renders as "body no longer
 * available" rather than throwing.
 */
export async function workflowStepRecord(
  conversationId: string,
  runId: string,
  stepIndex: number
): Promise<SubagentRunRecord | null> {
  if (!hasBackendRuntime()) return null;
  return (
    (await invoke<SubagentRunRecord | null>("workflow_step_record", {
      conversationId,
      runId,
      stepIndex
    })) ?? null
  );
}

/** Clear browser-preview markers that record a configured provider key. Provider IDs
 * may survive a reset, so their ID-keyed markers must be removed with the document. */
function clearPreviewApiKeyMarkers(): void {
  const localKeys: string[] = [];
  for (let index = 0; index < window.localStorage.length; index += 1) {
    const key = window.localStorage.key(index);
    if (key?.startsWith(API_KEY_LENGTH_PREFIX)) localKeys.push(key);
  }
  localKeys.forEach((key) => window.localStorage.removeItem(key));
}

export async function resetDocument(): Promise<AppDocument> {  await documentSaveTail.catch(() => undefined);
  if (hasBackendRuntime()) return normalizeDocument(await invoke<unknown>("reset_document"));
  const next = createSeedDocument();
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  clearPreviewImageStore();
  clearPreviewApiKeyMarkers();
  return next;
}
