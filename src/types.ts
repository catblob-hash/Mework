export type ContextKind = "system" | "user" | "reasoning" | "tool" | "assistant";
export type InsertableContextKind = ContextKind;

export type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = Record<string, JsonValue>;

/** Lightweight reference to image bytes kept outside the main document payload. */
export interface ImageAttachment {
  id: string;
  name: string;
  mime: string;
  width: number;
  height: number;
  bytes: number;
  /** Conversation-scoped number backing `[Image #N]` placeholders and playwright `upload_image` references. */
  shortId?: number;
}

interface TextContextBase {
  id: string;
  content: string;
  /** Marks content that is still streaming; completed or interrupted fragments may be persisted. */
  streaming?: boolean;
  createdAt: string;
}

export interface SystemContext extends TextContextBase {
  kind: "system";
  /** Lifecycle diagnostics kept in the local timeline and excluded from every model request. */
  localOnly?: boolean;
  /** Present on lifecycle diagnostics and model-visible context produced by a hook. */
  hookExecution?: HookExecutionMetadata;
}

export interface UserContext extends TextContextBase {
  kind: "user";
  images?: ImageAttachment[];
}

/** One provider-cited web source on an assistant round (server-side search /
 * grounding). Mirrors the host's `ContextSource`; rendered as citation chips
 * under the prose and never projected back into model input. */
export interface ContextSource {
  id: string;
  url?: string;
  title?: string;
}

export interface AssistantContext extends TextContextBase {
  kind: "assistant";
  /** One-based model request round for model-generated assistant output. */
  round?: number;
  /** Local association shared by every canonical item emitted by one model round. */
  modelTurnId?: string;
  /** Visible local fragment from an interrupted stream; excluded from every model request. */
  interrupted?: boolean;
  /** Provider-cited web sources for this round; absent when the provider cited none. */
  sources?: ContextSource[];
}

export type TextContext = SystemContext | UserContext | AssistantContext;

export interface HookExecutionMetadata {
  executionId: string;
  hookId: string;
  hookName: string;
  event: string;
  status: "running" | "succeeded" | "failed" | "blocked";
  contextInjected: boolean;
}

export interface ReasoningContext {
  id: string;
  kind: "reasoning";
  content?: string;
  /**
   * Whether the reasoning is readable plaintext or an encrypted trace.
   * Mirrors Rust `model.rs::ReasoningForm`.
   *
   * Resolve this when creating the card from
   * `ModelProfile.reasoningContent`, not from whether prose is empty. An
   * absent value is a pre-field record and must use `isEncryptedReasoning`.
   */
  form?: ReasoningForm;
  /** Marks reasoning that is still arriving from the model. */
  streaming?: boolean;
  /** One-based model request round for model-generated reasoning. */
  round?: number;
  /** Local association shared by every canonical item emitted by one model round. */
  modelTurnId?: string;
  /** Visible local fragment from an interrupted stream; excluded from every model request. */
  interrupted?: boolean;
  /**
   * Wall-clock milliseconds the provider spent reasoning in this round,
   * measured by the sidecar.
   *
   * Present even when `content` is absent: a Responses round that only returns
   * `encrypted_content` emits no summary text at all, and this plus `tokens`
   * is the entire visible trace of it.
   */
  durationMs?: number;
  /** Provider-reported reasoning tokens for this round. A subset of the round's output tokens. */
  tokens?: number;
  /**
   * The provider's own signed reasoning parts, replayed verbatim on later turns
   * (an Anthropic signature, a Responses item id plus ciphertext). Owned by the
   * host — `ContextItem::Reasoning.replay` — and opaque to the renderer, which
   * never reads inside it. `content` above is the presentation copy.
   */
  replay?: { model: string; parts: unknown[] };
  /**
   * When the live stream saw this round's reasoning open. UI-only and never
   * persisted — the host's `ContextItem::Reasoning` has no such field, so serde
   * drops it on the way to disk. It exists so the card can tick a live clock
   * before `durationMs` arrives.
   */
  startedAt?: string;
  createdAt: string;
}

export interface ToolResult {
  success: boolean;
  output: string;
  images?: ImageAttachment[];
  /** Optional unified diff for file-mutating tools. Kept separate from model-visible output. */
  diff?: string;
  executedAt: string;
  durationMs: number;
}

export type SubagentRunStatus =
  | "completed"
  | "interrupted"
  | "failed"
  | "stopped"
  | "roundLimit";

/** Live lifecycle values streamed on the `status` subagent channel. */
export type SubagentLiveStatus =
  | "running"
  | "idle"
  | "interrupted"
  | "failed"
  | "stopped"
  | "roundLimit";

export interface SubagentUpdate {
  content: string;
  createdAt: string;
}

/** Host-persisted exact resolution for a trusted named-agent definition. */
export interface AgentDefinitionBinding {
  source: AgentDefinitionSource;
  sourceKey: string;
  name: string;
  revision: number;
  /** Host-owned identity epoch; delete/re-add must advance it. */
  memoryEpoch: number;
  providerId: string;
  /** Exact raw, case-sensitive model ID; never a hash or synthetic owner. */
  modelId: string;
  memory: AgentDefinitionMemory;
  scopeKey: string;
  /** Keyed host receipt over the complete trusted definition/model binding. */
  configurationReceipt: string;
  /** Payload shape the receipt was signed over; absent on pre-versioning records. */
  receiptVersion?: number;
}

/** Host-persisted exact model and parent-memory snapshot selected for a fork. */
export interface ForkModelBinding {
  providerId: string;
  /** Exact raw, case-sensitive model ID; never a hash or synthetic owner. */
  modelId: string;
  memoryLanguage: ResolvedAppLanguage;
  /** Exact sorted parent memory-tool subset retained across reload. */
  memoryToolNames: string[];
  systemPromptSnapshot: string;
  systemPromptReceipt: string;
  memorySnapshotReceipt?: string;
  /** Keyed host receipt over the full exact model, capability, and snapshot binding. */
  bindingReceipt: string;
  /** Payload shape the receipt was signed over; absent on pre-versioning records. */
  receiptVersion?: number;
}

/**
 * A cumulative child run snapshot persisted with the latest
 * agent tool context (`agent_spawn`, `send_message`, or `followup_task`);
 * legacy `subagent` contexts carry one too.
 */
export interface SubagentRunRecord {
  /** Addressable agent name for agent messaging/waiting; absent on legacy records. */
  name?: string;
  /**
   * Model-visible task address of a managed run (`workflow:<runId>`) or a
   * workflow step's display label. Pool names are host internals; this is what
   * `task_list` prints after the run's turn ended. Absent on ordinary agents.
   */
  label?: string;
  /**
   * Specialized child workspace kind; absent on ordinary agents. Workflow steps
   * use `workflowStep` for their read-only child transcript.
   * Wire values mirror model.rs `SubagentRunKind` under serde camelCase.
   */
  kind?: "general" | "workflowStep";
  /** True only for conversation-context forks that inherit host-bound model auto-memory. */
  inheritsModelMemory?: boolean;
  /** Present only for conversation-context forks; mutually exclusive with agentDefinition. */
  forkModelBinding?: ForkModelBinding;
  /** Present only for a trusted named agent; mutually exclusive with inheritsModelMemory. */
  agentDefinition?: AgentDefinitionBinding;
  /** Host-keyed receipt over the conversation, name, and exact ordinary/fork/named continuation mode. */
  executionModeReceipt?: string;
  task: string;
  status: SubagentRunStatus;
  contexts: ContextItem[];
  updates: SubagentUpdate[];
  queuedMessages?: Array<{ content: string; triggerTurn: boolean }>;
  /**
   * Result of this agent's latest turn when the spawn set an `output_schema`,
   * already validated against it by the host. Absent on every other record.
   */
  structuredOutput?: unknown;
  /**
   * The spawn-time `output_schema` document of a schema-bound run, persisted so
   * a continuation in a later turn stays schema-bound. The host re-compiles it
   * under the spawn-time bounds at rehydration; the renderer never interprets
   * it. Absent on runs spawned without a schema and records persisted before
   * this field existed.
   */
  outputSchema?: unknown;
  /**
   * Tokens this agent alone consumed across all of its turns. Separate from the
   * conversation's own usage, which is a turn total that already absorbed these
   * numbers — the task sidebar shows the per-agent figure. Absent on records
   * persisted before this field existed and on agents that never completed a turn.
   */
  usage?: ModelUsage;
}

export interface SubagentLiveState {
  /** Failed projection survives backoff until this round starts replacing it. */
  retryRound?: number;
  /** Standard contexts projected from the child's nested model stream. */
  contexts: ContextItem[];
  /** Status updates explicitly reported by the child to its parent. */
  updates: SubagentUpdate[];
  /** Latest lifecycle transition streamed on the status channel. */
  status?: SubagentLiveStatus;
  /**
   * Provider-reported cumulative usage per child request round, streamed while
   * the child runs. Keyed by round because each snapshot supersedes that
   * round's previous one; summing the map is what gives the run's total. The
   * persisted record wins once it exists — it is authoritative and spans every
   * turn the agent ran — so this only ever fills the gap before settlement.
   */
  usageByRound?: Record<number, ModelUsage>;
  /** UI-only provider-call correlation; never copied into a persisted child transcript. */
  toolContextIds?: Record<string, string>;
}

export interface ToolContext {
  id: string;
  kind: "tool";
  toolName: string;
  /** One-based model request round. Absent for manual and legacy tool contexts. */
  round?: number;
  /** Local association shared by every canonical item emitted by one model round. */
  modelTurnId?: string;
  /** Model-requested arguments before hooks changed the arguments that were executed. */
  requestedInput?: JsonObject;
  input: JsonObject;
  result: ToolResult;
  /** UI-only tool call that is still moving through the model execution pipeline. */
  streaming?: boolean;
  /** Transient phase used only while a model run is active. */
  streamStatus?: "announced" | "ready" | "running" | "completed";
  /** UI-only live activity of a running subagent call; never persisted. */
  live?: SubagentLiveState;
  /** Persisted child transcript and terminal state for a `subagent` call. */
  subagent?: SubagentRunRecord;
  /**
   * Host proof that this card's result came from the backend's own execution,
   * issued when the card was built. The renderer treats it as opaque and must
   * carry it through untouched: a card that arrives back without it cannot be
   * verified and is quarantined instead of saved.
   */
  attestation?: string;
  createdAt: string;
}

export type ContextItem = TextContext | ReasoningContext | ToolContext;

export interface ConversationSettings {
  systemPrompt: string;
  /** Whether Rust appends the trusted application-data directory to the effective system prompt. */
  includeAppDataPath?: boolean;
  enabledTools: string[];
  /** Directly selected capability-catalog resource IDs. */
  hookIds: string[];
  skillIds: string[];
  mcpIds: string[];
  /** Selected tool-description resource ID; at most one. `null` enables all built-ins. */
  toolDescriptionFileId: string | null;
  /**
   * Named subagent roles selectable by the model in this conversation.
   * Roles are preset components, so switching presets replaces the entire set.
   */
  agentDefinitions: AgentDefinition[];
  /**
   * Allows subagents without a named role. When disabled, `agent_spawn` and
   * workflow steps must name a role. The host treats an empty role set as
   * enabled because otherwise every call would be unsatisfiable.
   */
  allowRolelessSubagents: boolean;
  /** Per-conversation web-search behavior; provider list and keys are global assets. */
  webSearch: ConversationWebSearchSettings;
  reasoningEffort: ReasoningEffort;
  securityLevel: SecurityLevel;
  /**
   * Whether this conversation loads the global memory tier (`~/.mework`). When
   * enabled, that tier's `MEWORK.md` instructions and `MEMORY.md` index are
   * concatenated into the context and its three tools
   * (`read`/`create`/`edit_global_memory`) become available. When disabled,
   * nothing is read, nothing is injected, and those three are withheld.
   */
  globalMemoryEnabled: boolean;
  /**
   * The same for the project tier (`<workspace>/.mework`). The two tiers are
   * independent: either, both or neither may be on.
   */
  projectMemoryEnabled: boolean;
  /**
   * Controls how conversation skills reach the model.
   *
   * When disabled, full skill bodies are added to the system prompt before each
   * round. When enabled, the `skill` tool loads bodies on demand. Both paths
   * use `skillIds` to choose the available skills.
   */
  skillToolEnabled: boolean;
}

/** The reusable subset of conversation settings owned by a conversation preset. */
export interface ConversationPresetSettings {
  systemPrompt: string;
  enabledTools: string[];
  /** Selected tool-description resource ID; at most one. `null` enables all built-ins. */
  toolDescriptionFileId: string | null;
  /** Roles owned by this preset. */
  agentDefinitions: AgentDefinition[];
  /** Template for `allowRolelessSubagents`, copied into a conversation when applying the preset. */
  allowRolelessSubagents: boolean;
  /** Directly selected capability-catalog resource IDs; dangling IDs are retained. */
  hookIds: string[];
  skillIds: string[];
  mcpIds: string[];
  /** Web-search behavior template, copied into new conversations when applying the preset. */
  webSearch: ConversationWebSearchSettings;
  /** Security-policy template copied into conversations; in-conversation changes use the composer selector. */
  securityLevel: SecurityLevel;
  /** Memory-tier templates copied into conversations; enabled tiers inject context and expose their tools. */
  globalMemoryEnabled: boolean;
  projectMemoryEnabled: boolean;
  /** On-demand skill-loading template copied into conversations. */
  skillToolEnabled: boolean;
}

/**
 * One alternative suffix after a user-message fork point.
 *
 * The common prefix stays in `Conversation.contexts`. Exactly one slot for a
 * given `forkContextId` is active and therefore keeps an empty `contexts`
 * array; its live suffix is the portion of `Conversation.contexts` after the
 * fork message. Inactive slots own their suffix snapshots. This keeps context
 * ids unique and avoids copying potentially large tool history.
 */
export interface ConversationBranch {
  id: string;
  forkContextId: string;
  active: boolean;
  contexts: ContextItem[];
  createdAt: string;
  updatedAt: string;
}

/** Retained solely to deserialize persisted `webSearch` abort records. */
export type UserAbortedTaskKind = "subagent" | "workflow" | "terminal" | "shell" | "webSearch" | "browser";

export interface UserAbortedTaskRecord {
  id: string;
  sourceKind: UserAbortedTaskKind;
  sourceIdentity: string;
  label: string;
  detail: string;
  metrics: {
    childCount: number | null;
    tokens: number | null;
    toolCount: number | null;
    elapsedMs: number | null;
  };
  startedAt: string;
  endedAt: string;
  reason: "userAborted";
}

/**
 * An isolated Git worktree used by this conversation.
 *
 * The host runs this conversation's tools against `path`, isolating it from
 * other workspace conversations. It belongs to the conversation instance,
 * not `ConversationSettings`, because settings are copied by presets.
 */
export interface ConversationWorktree {
  /** Absolute worktree root path. */
  path: string;
  /** Branch created for this worktree. */
  branch: string;
  /** Baseline commit used at release to determine whether extra commits exist. */
  baseOid: string;
}

/**
 * Execution target for this conversation's shell commands. `null` is local;
 * local execution is represented by the absence of a remote target.
 *
 * This belongs to the conversation instance because presets and workspace
 * snapshots copy `ConversationSettings`. SSH records store only stable machine
 * IDs; the host resolves endpoint details from {@link ExecutionEnvironmentAssets}.
 */
export type RunTarget =
  | { kind: "wsl"; distro: string }
  | { kind: "ssh"; machineId: string };

export interface Conversation {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  settings: ConversationSettings;
  contexts: ContextItem[];
  queuedMessages: QueuedMessage[];
  branches: ConversationBranch[];
  userAbortedTasks: UserAbortedTaskRecord[];
  /** Isolated worktree, or `null` for the workspace root. See {@link ConversationWorktree}. */
  worktree: ConversationWorktree | null;
  /** Execution target, or `null` for local execution. See {@link RunTarget}. */
  runTarget: RunTarget | null;
  /**
   * The conversation this one was forked from, or `null` for a top-level
   * conversation. Nesting is a renderer concept only: a child has exactly the
   * permissions its own `settings` grant. A parent that no longer exists
   * renders the child at top level.
   */
  parentConversationId: string | null;
}

export interface QueuedMessage {
  id: string;
  content: string;
  images?: ImageAttachment[];
  createdAt: string;
}

export type WorkspaceKind = "directory" | "temporary";

export interface Workspace {
  id: string;
  name: string;
  kind: WorkspaceKind;
  path: string;
  createdAt: string;
  /**
   * Preset ID automatically applied when the workspace's plus button creates a
   * conversation. An empty or dangling ID falls back to `lastConversationSettings`.
   * The sidebar's New Task path always uses the global default preset.
   */
  defaultConversationPresetId: string;
  /** Latest conversation-settings snapshot. It survives deletion of the source conversation. */
  lastConversationSettings: ConversationSettings | null;
  conversations: Conversation[];
}

export interface ConversationPreset {
  id: string;
  name: string;
  description: string;
  settings: ConversationPresetSettings;
}

export interface ResourceDescriptor {
  id: string;
  name: string;
  description: string;
  location: string;
  source: "builtin" | "user" | "workspace";
  available: boolean;
}

/** Provider protocol adaptation family. Mirrors Rust `model.rs::ProviderFamily`. */
export type ProviderFamily =
  | "openai_responses"
  | "openai_codex"
  | "openai_chat"
  | "anthropic"
  | "claude_agent"
  | "google"
  | "xai"
  | "azure"
  | "bedrock"
  | "vertex"
  | "openai_compatible";

/** Family-specific identity field. Mirrors Rust `model.rs::FamilySetting`. */
export type FamilySetting = "region" | "project" | "location" | "api_version" | "claude_executable";

/**
 * Endpoint shape supported by a provider. Mirrors Rust `model.rs::EndpointType`.
 * Identity is per endpoint rather than vendor because most providers expose
 * OpenAI-compatible image and audio endpoint shapes. The first six members are
 * callable conversation endpoints; the remaining four are configuration-only.
 */
export type EndpointType =
  | "openai_chat_completions"
  | "openai_responses"
  | "anthropic_messages"
  | "google_generative"
  | "azure_openai"
  | "bedrock_converse"
  | "openai_image_generation"
  | "openai_image_edit"
  | "openai_text_to_speech"
  | "openai_audio_transcription";

/**
 * Explicit model capabilities. Mirrors Rust `model.rs::ModelCapability`.
 * Unknown models infer capabilities from their ID once, then persist them.
 */
export type ModelCapability = "image_recognition";

export type ReasoningEffort = "disabled" | "low" | "medium" | "high" | "xhigh";

/**
 * Form in which this model returns reasoning. Mirrors Rust
 * `model.rs::ReasoningContent`. This is independent of the `reasoning`
 * capability because providers on the same protocol may return either
 * plaintext or encrypted content. Discovery writes the protocol default
 * explicitly, so there is no "follow the protocol" variant to resolve later.
 */
export type ReasoningContent = "plaintext" | "encrypted";

/**
 * Form of one produced reasoning card after resolving {@link ReasoningContent}.
 * Plaintext cards are editable while encrypted cards can only be deleted.
 */
export type ReasoningForm = "plaintext" | "encrypted";

export type SecurityLevel = "request_approval" | "allow_edits" | "plan" | "full_access";

/**
 * The one plan document a conversation owns while it is in plan mode.
 * Mirrors `ConversationPlan` in the host. There is at most one per
 * conversation: writing a plan replaces the previous body.
 */
export interface ConversationPlan {
  conversationId: string;
  markdown: string;
  status: "draft" | "approved" | "rejected";
  createdAt: string;
  updatedAt: string;
}

export interface ModelProfile {
  id: string;
  /** Display name; empty uses `id`. */
  name: string;
  /** Collapsed group in the model list; empty derives from `id`. */
  group: string;
  contextWindow?: number;
  maxOutputTokens?: number;
  /** Explicit capabilities, deduplicated and sorted by {@link MODEL_CAPABILITIES}. */
  capabilities: ModelCapability[];
  /** Reasoning return form. Always concrete: discovery resolves the protocol default. */
  reasoningContent: ReasoningContent;
  /**
   * Whether requests carry Claude Code's prompt-cache breakpoints. Always
   * concrete and on by default; only families where `promptCacheTakesEffect`
   * holds put it on the wire.
   */
  promptCache: boolean;
}

/** Account facts the host extracted from the Codex OAuth tokens; no token material. */
export interface CodexOauthAccount {
  accountId: string;
  email?: string;
  planType?: string;
}

/** Login state of the built-in OpenAI Codex provider, owned by the host. */
export interface CodexOauthStatus {
  signedIn: boolean;
  signingIn: boolean;
  account: CodexOauthAccount | null;
}

/**
 * Login state of the local Claude Code CLI, read by the host from
 * `claude auth status --json`. Mework never holds this credential: the CLI owns
 * its own login and this is only a report of it.
 */
export interface ClaudeAgentLoginStatus {
  signedIn: boolean;
  /** `claude.ai` | `console` | `none`, or whatever else the CLI reports. */
  authMethod: string;
  email: string | null;
  orgName: string | null;
  subscriptionType: string | null;
  /** Path the host resolved the executable to. */
  executable: string;
  /** Configuration directory actually in effect (`CLAUDE_CONFIG_DIR` or `~/.claude`). */
  configDir: string;
}

export interface ApiProvider {
  id: string;
  name: string;
  enabled: boolean;
  family: ProviderFamily;
  baseUrl: string;
  /** Family-specific identity fields. Mirrors Rust `ApiProvider::family_settings`. */
  familySettings: Partial<Record<FamilySetting, string>>;
  /** Base URL overrides for non-conversation endpoints; omitted entries use `baseUrl`. */
  endpointBaseUrls: Partial<Record<EndpointType, string>>;
  notes: string;
  models: ModelProfile[];
  /** The model selected for this provider. Each provider remembers its own selection. */
  activeModelId: string | null;
}

/**
 * Search-provider catalog kinds. Mirrors `SearchProviderKind` in `model.rs`;
 * `searchProviders.test.ts` compares the two tables row for row.
 */
export type SearchProviderKind =
  | "zhipu"
  | "tavily"
  | "searxng"
  | "exa"
  | "exa-mcp"
  | "bocha"
  | "querit"
  | "fetch"
  | "jina"
  | "firecrawl";

/** What the host can ask a search provider to do. */
export type SearchCapability = "searchKeywords" | "fetchUrls";

/**
 * `native` = the conversation's own model performs the search with its own
 * server-side tool. Its encrypted payload can be consumed only by that
 * provider's models. The catalog entries are a different backend entirely: the
 * host runs those searches itself.
 */
export type SearchProviderSelection =
  | { kind: "native" }
  | { kind: "explicit"; providerKind: SearchProviderKind }
  | { kind: "unavailable" };

/** Per-conversation web-search behavior and backend selection. */
export interface ConversationWebSearchSettings {
  /**
   * Native searches allowed inside one `web_search` call. 0 = unlimited.
   * Only the native backend reads it; a catalog provider's result count is the
   * global `maxResults`.
   */
  maxSearchesPerCall: number;
  provider: SearchProviderSelection;
}

/** Global provider catalog configuration; secrets remain in OS credentials. */
export interface SearchProviderConfig {
  kind: SearchProviderKind;
  enabled: boolean;
  /** Empty = the catalog default endpoint for `searchKeywords`. */
  searchApiHost: string;
  /** Empty = the catalog default endpoint for `fetchUrls`. */
  fetchApiHost: string;
  /** Searxng only; empty = pick the instance's general web engines from /config. */
  engines: string[];
  /** Searxng only. The password lives in the OS credential store, never here. */
  basicAuthUsername: string;
}

/** How a result's body text is shortened before it reaches the model. */
export type SearchCompressionMethod = "none" | "cutoff";

export interface SearchCompression {
  method: SearchCompressionMethod;
  /** Whole-call token budget, split evenly across the results. */
  cutoffLimit: number;
}

export interface WebSearchAssets {
  providers: SearchProviderConfig[];
  /**
   * Which provider `web_fetch` uses. Fetching has no native leg — no wire
   * protocol here exposes "retrieve this page" as a server-side tool — so it is
   * a global default rather than a per-conversation choice.
   */
  fetchProvider: SearchProviderKind | null;
  maxResults: number;
  /**
   * Result blacklist. Syntax: `<all_urls>`, a `scheme://host/path` match
   * pattern (`*` wildcards, `*.` matches subdomains), or `/regex/`.
   */
  excludeDomains: string[];
  compression: SearchCompression;
}

export type AppLanguage = "auto" | "zh-CN" | "en-US";
export type ResolvedAppLanguage = Exclude<AppLanguage, "auto">;
export type ThemePreference = "day" | "night" | "system";

export type AgentDefinitionSource = "user" | "project" | "plugin" | "managed";
export type AgentDefinitionMemory = "none" | "user" | "project" | "local";
export type AgentModelSelection =
  | { kind: "inherit" }
  | { kind: "explicit"; providerId: string; modelId: string }
  /**
   * The exact provider/model pair this role was bound to no longer resolves.
   * Written on load in place of the dead binding, keeping none of the old
   * identifiers — so re-enabling a provider never silently restores a binding
   * the user was already told they had lost. A role in this state is hidden
   * from the model and fails with its own wording if named anyway.
   */
  | { kind: "unavailable" };

/**
 * Host-validated named-agent configuration. Model-facing calls select only
 * `name`; source, provider/model and memory identity are never accepted from
 * agent_spawn input.
 *
 * A role belongs to a conversation preset rather than to global settings, and
 * carries no system prompt of its own: a named child renders the conversation's
 * own prompt through the subagent addendum, exactly like an ordinary child. A
 * role answers "which model, which tools, which search backend, and what it
 * tells the model it is for".
 */
export interface AgentDefinition {
  /**
   * Host-owned. The renderer always writes `true`: a user-authored role lives
   * inside a preset, so the preset is already the on/off. A trusted
   * project/plugin/managed source may still ship a disabled definition, and
   * that one keeps shadowing a user definition of the same name.
   */
  enabled: boolean;
  /** Host-retained tombstone; hidden from the renderer's editable surface. */
  deleted: boolean;
  name: string;
  /**
   * What this role is for, in the user's own words. The host renders it into
   * the model-facing description of whichever of `agent_spawn` / `workflow`
   * carries the role block; empty means the role contributes no line at all.
   *
   * NOT capability-bearing, which is why it is absent from
   * `sameUserAgentConfiguration`: rewording a role must not advance `revision`
   * and revoke the children already bound to it. Free text — multi-line, no
   * length cap, written through verbatim.
   */
  description: string;
  source: AgentDefinitionSource;
  sourceKey: string;
  revision: number;
  /**
   * Host-owned positive identity epoch. Renderer-created definitions use a
   * provisional value of 1; only host persistence makes an epoch authoritative.
   */
  memoryEpoch: number;
  modelSelection: AgentModelSelection;
  /**
   * Host-owned, and part of the FROZEN `AgentDefinitionBindingV1` projection
   * together with the scope key derived from it — which is why it survives the
   * removal of its editor. The renderer always writes `"none"`, so a
   * user-authored role receives no memory tools. Do not drop the field to tidy
   * up: moving those bytes burns every execution-mode reservation row already
   * held under the old ones.
   */
  memory: AgentDefinitionMemory;
  /**
   * Execution overrides. Unlike `description` these are capability-bearing and
   * are covered by BOTH host payloads, so they are `T | null` rather than `?`:
   * the Rust side writes every key on every save (no `skip_serializing_if`)
   * precisely so that "absent" and "at the default" stay distinguishable, and an
   * optional key here would let a save silently drop one back to the default.
   *
   * `tools` is an independent SELECTION out of the trusted tool catalogue, not
   * an intersection with the calling conversation's enabled set: a role may
   * grant a catalogue tool the conversation switched off. `null` still means
   * "inherit the conversation's enabled tools".
   * `disallowedTools` subtracts, and the host's own
   * `SUBAGENT_DISABLED_TOOL_NAMES` floor is below both.
   *
   * `searchProvider` picks this role's `web_search` backend; `null` follows the
   * calling conversation. Only the search leg — `web_fetch`'s provider is a
   * global asset with no per-conversation choice either.
   */
  effort: ReasoningEffort | null;
  tools: string[] | null;
  disallowedTools: string[];
  searchProvider: SearchProviderSelection | null;
}

/**
 * MCP transport supported by the Rust client. Legacy HTTP+SSE is rejected, so
 * it has no corresponding `sse` variant.
 */
export type McpTransportKind = "stdio" | "streamable_http";

/**
 * User-configured MCP server. Server existence is an asset-layer concern;
 * presets and conversations choose usage through `mcpIds`. Credentials persist
 * with the document and are redacted by host-side `Debug`.
 */
export interface McpServerConfig {
  id: string;
  name: string;
  description: string;
  /** Globally enabled; disabled servers are absent from the capability catalog. */
  enabled: boolean;
  transport: McpTransportKind;
  /** stdio executable. */
  command: string;
  /** stdio command-line arguments, one per line. */
  args: string[];
  /** Additional stdio environment variables. */
  env: Record<string, string>;
  /**
   * Package-registry mirror for stdio launchers; empty uses the toolchain default.
   * The host maps it to environment variables only for npx/bunx and uv/uvx,
   * while explicitly supplied `env` values take precedence.
   */
  registryUrl: string;
  /** Streamable HTTP endpoint URL. */
  url: string;
  /** Additional streamable HTTP request headers. */
  headers: Record<string, string>;
  /** `0` uses the host default timeout. */
  timeoutSeconds: number;
  /** Declares that calls may run long, causing the host to relax idle detection. */
  longRunning: boolean;
  provider: string;
  providerUrl: string;
  tags: string[];
  /**
   * Exclusion-based tool switch. An empty list enables every server tool, so
   * newly added server tools are enabled by default.
   */
  disabledTools: string[];
  /** Exclusion-based list: listed tools require approval on every call. */
  disabledAutoApproveTools: string[];
  /** Ascending display order written by drag sorting. */
  sortOrder: number;
  createdAt: string;
  updatedAt: string;
}

/** Skill installation source. */
export type SkillSource = "local_directory" | "zip" | "system_scan" | "remote";

/**
 * Skill installed in the application-data directory. Skill directories are
 * copied to `<app_data>/skills/<folderName>`; external directories can be
 * imported through a one-time system-skill scan.
 */
export interface SkillRecord {
  id: string;
  name: string;
  description: string;
  /** Application-data directory name; unique case-insensitively. */
  folderName: string;
  source: SkillSource;
  /** Original import location, used only for display. */
  sourceLocation: string;
  /** Registry page URL for a remotely installed skill; empty for local installs. */
  sourceUrl: string;
  author: string;
  version: string;
  tags: string[];
  /** `SKILL.md` content hash used to detect external post-install changes. */
  contentHash: string;
  /** Globally enabled; `skillIds` is the second selection layer. */
  enabled: boolean;
  installedAt: string;
  updatedAt: string;
}

/**
 * Token in a key combination. Modifiers use `Control`, `Alt`, `Shift`, or
 * `Meta`; all other tokens are `KeyboardEvent.code` values so bindings are
 * keyboard-layout independent.
 */
export type KeyToken = string;

/** Persisted shortcut preference; command labels, groups, and defaults are code constants. */
export interface ShortcutPreference {
  /** Empty means unbound. Unbound commands never dispatch and cannot be enabled. */
  binding: KeyToken[];
  enabled: boolean;
}

/**
 * Bindable shortcut command. The command table is owned by
 * `src/lib/shortcuts.ts`; documents store only bindings and enabled states.
 */
export type ShortcutCommandId =
  | "app.settings.open"
  | "app.conversation_settings.open"
  | "app.zoom.in"
  | "app.zoom.out"
  | "app.zoom.reset"
  | "conversation.create"
  | "conversation.next"
  | "conversation.previous"
  | "conversation.stop"
  | "message.copy_last"
  | "message.edit_last_user"
  | "panel.browser.toggle"
  | "panel.close";

/** Only preferences that the Mework renderer can apply. */
export interface AppearancePreferences {
  /** Uppercase `#RRGGBB` accent color; empty uses the palette accent. */
  themeColor: string;
  /** Page zoom multiplier, 0.5-2.0 in 0.1 increments. */
  zoom: number;
  /** UI font family; empty uses the system default. */
  uiFontFamily: string;
  /** Monospace font family; empty uses the system default. */
  monoFontFamily: string;
  /** Message text size, 12-22. */
  messageFontSize: number;
  /** Use a serif font for message prose. */
  serifMessages: boolean;
  /** Use the wide message layout; inverse of `chat.narrow_mode`. */
  wideMessages: boolean;
  /** Key combination for sending messages; mutually exclusive with {@link newlineShortcut}. */
  sendShortcut: KeyToken[];
  /** Key combination for inserting a newline; mutually exclusive with {@link sendShortcut}. */
  newlineShortcut: KeyToken[];
  /** Enable composer spell checking. */
  spellCheck: boolean;
  /** Render user-authored messages as Markdown. */
  renderUserMarkdown: boolean;
  /** Confirm before deleting a timeline item. Defaults to false because deletion is undoable. */
  confirmMessageDelete: boolean;
  /** Collapse reasoning chains by default. */
  collapseReasoning: boolean;
  /** Allow long code blocks to collapse. */
  codeBlockCollapsible: boolean;
  /** Wrap code blocks instead of horizontally scrolling. */
  codeBlockWrappable: boolean;
  /** Enable inline `$...$` math; disabled mode recognizes only `$$...$$`. */
  singleDollarMath: boolean;
  /** User-defined CSS applied through a constructable stylesheet, not a `<style>` element. */
  customCss: string;
}

/** User-added executable to monitor on PATH; built-in definitions are code constants. */
export interface EnvironmentToolDefinition {
  name: string;
  /** Executable name to locate on PATH, without an extension. */
  executable: string;
  /** Arguments for obtaining the version; empty uses `["--version"]`. */
  versionArgs: string[];
}

/** Environment dependency check result. The host probes live and never persists it. */
export interface EnvironmentToolSnapshot {
  name: string;
  executable: string;
  /** Resolved absolute path; empty when not found. */
  path: string;
  /** Detected version; empty when parsing fails. */
  version: string;
  /** Probe failure reason; empty on success. */
  error: string;
  description: string;
  repoUrl: string;
  homepage: string;
  /** Built-in presets cannot be deleted; user-added definitions can. */
  builtin: boolean;
}

/** How this copy of Mework was installed. Mirrors Rust `app_update::InstallFlavor`. */
export type AppInstallFlavor = "installer" | "portable";

/** Running version and install shape. Mirrors Rust `app_update::AppVersionInfo`. */
export interface AppVersionInfo {
  version: string;
  flavor: AppInstallFlavor;
  /** A `cargo build` without `--release`; never shipped as a release asset. */
  developmentBuild: boolean;
  arch: string;
  os: string;
  repositoryUrl: string;
  releasesUrl: string;
  executableDir: string;
}

/** One downloadable file on a GitHub release. Mirrors Rust `app_update::ReleaseAsset`. */
export interface AppReleaseAsset {
  name: string;
  downloadUrl: string;
  size: number;
}

/** Result of asking GitHub for the latest release. Mirrors Rust `app_update::UpdateCheck`. */
export interface AppUpdateCheck {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  release: {
    tag: string;
    name: string;
    htmlUrl: string;
    /** Release body as GitHub stores it: Markdown. */
    notes: string;
    /** ISO 8601; empty when GitHub omits it. */
    publishedAt: string;
  };
  /** The asset for this machine's flavor and architecture, when the release has one. */
  asset: AppReleaseAsset | null;
  /** `SHA256SUMS` when the release publishes one. */
  checksumsAsset: AppReleaseAsset | null;
  checkedAt: string;
}

/** Streamed while an update downloads. Mirrors Rust `app_update::DownloadEvent`. */
export type AppUpdateDownloadEvent =
  | { type: "progress"; receivedBytes: number; totalBytes: number }
  | { type: "verifying" };

/** A finished download. Mirrors Rust `app_update::DownloadedUpdate`. */
export interface AppUpdateDownload {
  path: string;
  fileName: string;
  sizeBytes: number;
  sha256: string;
  /** `verified`: matched the release's SHA256SUMS; `unavailable`: the release publishes no SHA256SUMS at all. */
  verification: "verified" | "unavailable";
  flavor: AppInstallFlavor;
}

/** What `install_app_update` did. Mirrors Rust `app_update::InstallOutcome`. */
export interface AppUpdateInstallOutcome {
  action: "installer_launched" | "revealed";
}

/** MCP connectivity-probe result. Mirrors Rust `lib.rs::McpProbeReport`. */
export interface McpProbeReport {
  ok: boolean;
  /** Whether the user cancelled this probe, which is not a connection failure. */
  cancelled: boolean;
  /** Negotiated MCP protocol version; empty on probe failure. */
  protocolVersion: string;
  /** Remote `serverInfo.name`; empty on probe failure. */
  serverName: string;
  /** Remote `serverInfo.version`; empty on probe failure. */
  serverVersion: string;
  tools: McpProbeTool[];
  /** Nonempty only if the remote declares the prompts capability. */
  prompts: McpProbePrompt[];
  /** Nonempty only if the remote declares the resources capability. */
  resources: McpProbeResource[];
  /** stderr lines written during a stdio probe; always empty for HTTP servers. */
  logs: string[];
  error: string;
}

export interface McpProbeTool {
  /** Remote tool name, not the model-facing prefixed name. */
  name: string;
  title: string;
  description: string;
  /** The remote declares that every call requires manual confirmation. */
  requiresUserInteraction: boolean;
  inputSchema: JsonValue;
}

/** One `prompts/list` entry. */
export interface McpProbePrompt {
  name: string;
  title: string;
  description: string;
  arguments: McpProbePromptArgument[];
}

export interface McpProbePromptArgument {
  name: string;
  description: string;
  required: boolean;
}

/** One `resources/list` entry. */
export interface McpProbeResource {
  uri: string;
  name: string;
  title: string;
  description: string;
  mimeType: string;
  /** Byte count declared by the remote; `0` when undeclared. */
  size: number;
}

/** Online skill registry, sharing values with Rust `skill_registry.rs::SkillSearchSource`. */
export type SkillSearchSource = "skills.sh" | "claude-plugins.dev" | "clawhub.ai" | "github";

/** Online skill search result. Mirrors Rust `skill_registry.rs::SkillSearchResult`. */
export interface SkillSearchResult {
  slug: string;
  name: string;
  description: string;
  author: string;
  stars: number;
  downloads: number;
  sourceRegistry: SkillSearchSource;
  /** Registry page URL for this skill. */
  sourceUrl: string;
  /** Opaque installation handle passed unchanged to `skill_install_remote`. */
  installSource: string;
}

/** Cross-registry search result; successes are returned even if some sources fail. */
export interface SkillSearchReport {
  results: SkillSearchResult[];
  /** Unreachable sources; contains all sources when `results` is empty. */
  failedSources: SkillSearchSource[];
}

/** Candidate found by scanning system skill directories. Mirrors Rust `skills.rs::SystemSkillCandidate`. */
export interface SystemSkillCandidate {
  sourceName: string;
  folderName: string;
  name: string;
  description: string;
  directoryPath: string;
  /** Same-name folder is already installed; shown but cannot be imported again. */
  conflict: boolean;
}

export interface GlobalSettings {
  /** Application chrome language. `auto` resolves from the current OS/browser locale. */
  appLanguage: AppLanguage;
  /**
   * What `appLanguage` currently resolves to, mirrored for the backend.
   *
   * This value carries both host-rendered UI text and the language a
   * user-written tool-description file is treated as being in: such a file
   * declares none of its own, and the built-in profile that fills the keys it
   * omits follows from this. Selecting a built-in profile — or selecting
   * nothing, which is the English built-in — pins the language to that profile
   * instead. Only the renderer can resolve `auto`, so it writes the resolved
   * value here for the backend to read.
   */
  resolvedAppLanguage: ResolvedAppLanguage;
  /** Persisted appearance preference. `system` follows the current Windows theme. */
  theme: ThemePreference;
  conversationPresets: ConversationPreset[];
  /** New conversations link to this preset; empty when the implicit blank default is active. */
  defaultConversationPresetId: string;
  /** Default inherited by the next conversation; each conversation keeps its own value. */
  lastReasoningEffort: ReasoningEffort;
  apiProviders: ApiProvider[];
  activeProviderId: string | null;
  /** Web-search assets: external providers and the global selected provider. */
  webSearch: WebSearchAssets;
  /** MCP assets: catalog of user-configured servers. */
  mcpServers: McpServerConfig[];
  /** Index of skills installed in the application-data directory. */
  skills: SkillRecord[];
  /** Appearance preferences. */
  appearance: AppearancePreferences;
  /** Per-command bindings. Absent entries use code defaults; resetting a command removes its entry. */
  shortcuts: Partial<Record<ShortcutCommandId, ShortcutPreference>>;
  /** User-added environment dependencies; built-ins are code constants. */
  environmentTools: EnvironmentToolDefinition[];
  /** Execution-environment assets: SSH machine catalog and environment-keyed variables. */
  executionEnvironments: ExecutionEnvironmentAssets;
}

/**
 * User-registered SSH execution machine. `host` accepts `user@hostname`, a
 * hostname, or an `~/.ssh/config` host alias. Authentication material is not
 * persisted; OpenSSH resolves it from identity files and the agent.
 */
export interface SshMachineConfig {
  id: string;
  name: string;
  host: string;
  /** `0` uses the default SSH port, 22. */
  port: number;
  /** Private key path; empty uses OpenSSH's default resolution. */
  identityFile: string;
  /** Remote working directory; empty uses the remote home and accepts a `~` prefix. */
  remoteCwd: string;
  createdAt: string;
  updatedAt: string;
}

/**
 * Execution-environment assets. WSL distributions are machine state and are
 * enumerated live by `list_wsl_distros`. Environment variables are keyed by
 * `local`, `wsl:<distro>`, or `ssh:<machine id>`.
 */
export interface ExecutionEnvironmentAssets {
  sshMachines: SshMachineConfig[];
  envVars: Record<string, Record<string, string>>;
}

/** Installed WSL distribution enumerated live by `list_wsl_distros`. */
export interface WslDistro {
  name: string;
  version: number;
  isDefault: boolean;
}

export type ToolParameterType = "string" | "number" | "boolean" | "multiline" | "json";

export interface ToolParameter {
  name: string;
  label: string;
  type: ToolParameterType;
  required: boolean;
  placeholder?: string;
  help?: string;
  defaultValue?: JsonValue;
}

export interface ToolDescriptor {
  name: string;
  label: string;
  /**
   * Transport-only field not read or displayed by the frontend. Built-in seed
   * descriptions are empty; nonempty values come only from dynamically
   * discovered MCP tools, and the backend assembles effective descriptions.
   */
  description: string;
  category: "filesystem" | "shell" | "web" | "orchestration" | "memory" | "mcp";
  dangerous: boolean;
  parameters: ToolParameter[];
  inputSchema?: JsonValue;
}

/*
 * Tool-description file contents are not modeled here. The app discovers and
 * selects files but never reads, edits, or writes their bodies. Their format
 * lives in the host: `capabilities.rs` parses the file, `prompt_profile.rs`
 * owns the `prompts` registry and the document shape.
 */

export interface CapabilityCatalog {
  hooks: ResourceDescriptor[];
  skills: ResourceDescriptor[];
  mcps: ResourceDescriptor[];
  /** Tool-description JSON files discovered on disk, one descriptor per file. */
  toolDescriptionFiles: ResourceDescriptor[];
}

export interface AppDocument {
  schemaVersion: number;
  globalSettings: GlobalSettings;
  workspaces: Workspace[];
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
}

export interface ToolExecutionRequest {
  conversationId: string;
  workspacePath: string;
  toolName: string;
  input: JsonObject;
}

export interface ToolExecutionResponse extends ToolResult {}

/** What the renderer needs to draw one approval card. Mirrors
 * `PendingToolPrompt` in `src-tauri/src/tool_prompt.rs`, and matches the
 * payload of a `tool_approval_requested` stream event field for field. */
export interface PendingToolPrompt {
  promptId: string;
  toolName: string;
  label: string;
  /** A short description of the call, derived from its redacted arguments. */
  summary: string;
  riskLevel: string;
  reason: string;
  /** The requesting subagent's name, or absent for the main session. */
  requester?: string;
  /** Machine-readable requester address, unlike `requester` which is
   * display-escaped text: the child's addressable name plus the
   * parent-timeline call id of its turn. Used to open the requesting
   * child's page when the card arrives. */
  sourceAgent?: string;
  sourceCallId?: string;
  /** False for shell and MCP tools, and for anything the backend marked
   * mandatory: those must be answered one call at a time. */
  allowAlwaysOffered: boolean;
  /** Whether this card appears whatever the conversation's security level is.
   * `allowAlwaysOffered` cannot stand in for it: an ordinary shell or MCP call
   * also declines blanket permission while still being an approval the level
   * decides. Full access clears every prompt except these. */
  mandatory?: boolean;
  /**
   * Which card this is. Plain tool approvals omit it. `plan_exit` asks whether
   * to leave plan mode and start implementing, `plan_enter` asks whether to
   * enter plan mode; both are answered with the same decisions but draw
   * different copy and, for `plan_exit`, collect feedback on a denial.
   */
  kind?: "tool" | "plan_exit" | "plan_enter";
}

export type ToolPromptDecision = "deny" | "allow_once" | "allow_always";

/**
 * One pending `fork` request raised by the model. Mirrors
 * `PendingForkRequest` in `src-tauri/src/fork_requests.rs` and the flattened
 * payload of the `forkRequested` push event.
 */
export interface PendingForkRequest {
  forkId: string;
  workspaceId: string;
  sourceConversationId: string;
  /** Display-escaped title of the conversation that raised the request. */
  sourceTitle: string;
  /** The child's first user message, verbatim. */
  prompt: string;
  inheritContext: boolean;
  requestedAt: string;
}

/**
 * How one `fork` request ended. Mirrors `ForkDecisionRecord` in
 * `src-tauri/src/fork_requests.rs`.
 *
 * The record exists for the user alone: it is the task bar's only trace that a
 * fork was ever asked for. The model is never told the outcome, so this never
 * reaches `task_list` or any tool receipt.
 */
export interface ForkDecisionRecord {
  forkId: string;
  workspaceId: string;
  sourceConversationId: string;
  /** Display-escaped title derived from the prompt when the request was raised. */
  title: string;
  /** The child's first user message, verbatim. */
  prompt: string;
  inheritContext: boolean;
  requestedAt: string;
  decidedAt: string;
  approved: boolean;
  /** Set exactly when a child conversation exists; null on a decline. */
  childConversationId: string | null;
}

export interface ToolApprovalGrant {
  /** Present only when the runtime required and received an explicit approval. */
  nonce?: string;
  expiresInMs: number;
  /** Present when the call still needs the user's answer. Draw this card and
   * call `resolveToolPrompt`; that is what mints the nonce. Neither field set
   * means no approval was needed at all. */
  prompt?: PendingToolPrompt;
}

export interface ApiKeyStatus {
  configured: boolean;
  /** Persisted by the frontend so the masked value can match the secret without retaining it. */
  keyLength?: number;
}

export interface ModelRunRequest {
  /** Host-minted first-run intent, consumed only when the run becomes adoptable. */
  forkPromptContextId?: string;
  provider: ApiProvider;
  model: ModelProfile;
  reasoningEffort: ReasoningEffort;
  /** Identifies the persisted conversation whose dynamic presets Rust must resolve. */
  conversationId: string;
  workspacePath: string;
  systemPrompt: string;
  enabledTools: string[];
  contexts: ContextItem[];
  tools: ToolDescriptor[];
}

export interface ModelUsage {
  inputTokens?: number;
  /** Cached reads included in `inputTokens`; cache writes are deliberately excluded. */
  cachedInputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  /**
   * Reasoning tokens included in `outputTokens` — never an addend, same
   * discipline as `cachedInputTokens`. OpenAI Responses reports `0` rather
   * than omitting it when a round did no reasoning.
   */
  reasoningTokens?: number;
}

export interface ModelRunResponse {
  contexts: ContextItem[];
  /** Validated `structured_output` value of a schema-bound run; absent otherwise. */
  structuredOutput?: unknown;
  usage: ModelUsage;
  model: string;
  providerName: string;
  durationMs: number;
  stopReason?: string;
  /** Size of the final request round, unlike cumulative `usage`. */
  contextTokens?: number;
  /**
   * Terminal request failure of the turn (`stopReason === "error"`), after
   * automatic retries. Rendered as a dismissable transient notice; never
   * persisted into the timeline.
   */
  error?: ModelRunFailure;
}

export interface ModelRunFailure {
  message: string;
  /** Tool round the failing request belonged to (1-based). */
  round: number;
  /** Total requests attempted for that round, including the first one. */
  attempts: number;
}

export type SubagentChannel = "text" | "reasoning" | "activity" | "update" | "status";

/** Lifecycle state of one workflow step in the progress card. `error` also
 * carries user skips; check `skipped` to tell them apart. */
export type WorkflowStepState = "start" | "progress" | "done" | "error";

/**
 * One row of a workflow run's progress ledger, mirroring `ProgressRow` in
 * `workflow-core/src/progress.rs`.
 *
 * `agent` rows merge by `index` for the step's whole lifetime; `log` rows carry
 * a host-assigned monotonic index and only ever accumulate. Optional fields are
 * omitted rather than sent as null, so absence and emptiness stay distinct.
 */
export interface WorkflowProgressEntry {
  kind: "agent" | "log";
  index: number;
  state: WorkflowStepState;
  label?: string;
  phase?: string;
  phaseIndex?: number;
  preview?: string;
  message?: string;
  cached?: boolean;
  blocked?: boolean;
  skipped?: boolean;
}

export type ModelStreamEvent =
  | { type: "text_delta"; round: number; delta: string }
  /**
   * The provider opened a reasoning item. Deliberately not implied by the
   * first `reasoning_delta`: a Responses round that only returns
   * `encrypted_content` never emits a delta, and this is then the one signal
   * that the model is thinking. The renderer starts its live clock here.
   *
   * `item` is the zero-based ordinal of the reasoning item inside the round
   * (dense — the sidecar only numbers items that survive its evidence gate).
   * One live reasoning row is keyed per ordinal. Optional only for fixture
   * ergonomics; the wire always carries it since protocol generation 5.
   */
  | { type: "reasoning_start"; round: number; item?: number; form?: ReasoningForm }
  | { type: "reasoning_delta"; round: number; item?: number; delta: string }
  /** `durationMs` is the sidecar's cumulative reasoning wall clock for the step. */
  | { type: "reasoning_done"; round: number; item?: number; durationMs?: number }
  /** Provider-reported cumulative usage for this backend request round. */
  | { type: "usage_updated"; round: number; usage: ModelUsage }
  | { type: "user_input_received"; round: number; id: string; content: string; images?: ImageAttachment[]; createdAt: string }
  | {
      type: "tool_call_announced";
      round: number;
      callId: string;
      toolName: string;
      /** The timeline id the persisted card will carry. The streaming row must
       * adopt it: when both sides minted their own id, the renderer's replaced
       * the host's on the way to disk and any attestation bound to the host's
       * id could never match again. */
      contextId: string;
    }
  | { type: "tool_call_arguments_ready"; round: number; callId: string; input: JsonObject }
  | { type: "tool_execution_started"; round: number; callId: string }
  | { type: "tool_execution_completed"; round: number; callId: string; result: ToolResult }
  /**
   * Settled form of an announced tool card. At the round boundary it contains
   * the terminal subagent record and a new signature. The renderer replaces
   * the persisted card in place and saves it immediately.
   */
  | { type: "tool_context_settled"; round: number; context: ToolContext }
  /** A tool call is waiting on the user. The renderer draws an approval card
   * above the composer and answers with `resolveToolPrompt`; the backend worker
   * that raised it stays blocked until then. Carries no round: approval is
   * raised from the tool executor, which does not know the surrounding turn. */
  | { type: "tool_approval_requested"; promptId: string; toolName: string; label: string; summary: string; riskLevel: string; reason: string; requester?: string; sourceAgent?: string; sourceCallId?: string; allowAlwaysOffered: boolean; mandatory?: boolean; kind?: "tool" | "plan_exit" | "plan_enter" }
  /** The card for `promptId` is over. `approved` is what the host concluded,
   * which is not always what the user clicked — a cancelled run resolves its
   * outstanding cards as denied. */
  | { type: "tool_approval_resolved"; promptId: string; approved: boolean }
  | { type: "subagent_event"; round: number; callId: string; event: ModelStreamEvent }
  | { type: "subagent_delta"; round: number; callId: string; channel: SubagentChannel; delta: string }
  /** One transition of a running workflow's progress ledger. Per-step
   * transcripts still ride `subagent_event`; this carries only the card's row
   * state, so a renderer that ignores it loses the card, not the transcripts.
   * `runId` is what the card's Skip/Retry commands address — the host mints it
   * per run (reusing it on resume), so the stream is the only place to learn it. */
  | { type: "workflow_progress"; round: number; callId: string; runId: string; entry: WorkflowProgressEntry }
  | { type: "hook_execution_started"; round: number; executionId: string; hookId: string; hookName: string; event: string; statusMessage?: string }
  | { type: "hook_execution_completed"; round: number; executionId: string; hookId: string; hookName: string; event: string; result: ToolResult; blocked: boolean; reason?: string; contextInjected: boolean }
  /** A transient request failure is about to be retried: the renderer drops
   * the failed attempt's partial content for `round` and shows a temporary
   * notice. The message is never persisted as timeline context. */
  | { type: "stream_retry_scheduled"; round: number; attempt: number; maxAttempts: number; delayMs: number; message: string }
  /** Backend cancellation probe emitted while no data is flowing; ignored. */
  | { type: "ping" }
  /**
   * The host has settled this run and placed the result in its settlement slot.
   * The initiating invocation ignores this event; an `attachModelRun` adopter
   * uses it to call `takeRunSettlement`.
   */
  | { type: "run_concluded"; requestId: string };

/**
 * Historical settings view IDs remain for persisted routes and are redirected
 * by `GlobalSettings` to current views. `skills` and `mcp` are current pages.
 */
export type SettingsView =
  | "general"
  | "appearance"
  | "conversation_presets"
  | "providers"
  | "search_providers"
  | "mcp"
  | "skills"
  | "shortcuts"
  | "dependencies"
  | "updates"
  | "web_search"
  | "memory"
  | "agents"
  | "hooks"
  | "advanced"
  | "capability_catalog";

export type AppSurface =
  | { kind: "workspace" }
  | { kind: "settings"; view: SettingsView };

/**
 * The `ask_user` tool output the built-in English prompt profile persists while
 * the answer is outstanding (`task.ask_user_pending`). Fixtures use it; the UI
 * does not match it — a profile may word it differently, so a pending question
 * is recognized structurally: a successful `ask_user` result with no real user
 * reply after it.
 */
export const ASK_USER_PENDING_OUTPUT = "Asked the user; this turn is paused.";
