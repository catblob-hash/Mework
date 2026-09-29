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
  /** Conversation-scoped number backing `[Image #N]` placeholders and `preview_upload_image` references. */
  shortId?: number;
}

/** How the model reads an attached file: its own text, or the text layer of a PDF. */
export type FileAttachmentFormat = "text" | "pdf";

/**
 * Lightweight reference to a non-image file attached to a user message.
 *
 * Mirrors Rust `model::FileAttachment`. The host keeps the bytes (and, for a
 * PDF, the text read out of it) in its content-addressed attachment store; the
 * model reads the text, inlined into the message when the request is built.
 */
export interface FileAttachment {
  /** Full lowercase SHA-256 of the stored original bytes. */
  id: string;
  name: string;
  format: FileAttachmentFormat;
  /** Size of the stored original. */
  bytes: number;
  /** Estimated tokens of the text the model reads for this file. */
  tokens: number;
  /** Page count of a PDF. */
  pages?: number;
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
  files?: FileAttachment[];
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
  /**
   * The provider's own id for this tool call, replayed verbatim on later turns
   * so the exchange keeps one id for its whole life. Opaque to the renderer,
   * which must carry it through untouched. Absent on manual, host-fabricated
   * and legacy cards, which fall back to a host-minted digest.
   */
  providerCallId?: string;
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
  /**
   * Whether this conversation can reach the web at all.
   *
   * The feature switch for both web tools, in the same shape as the two memory
   * tiers: when enabled, the host derives `web_search` — and `web_fetch` too,
   * if the resolved backend can fetch — into the granted tool set. Neither name
   * is ever taken from `enabledTools`, so they are not rows in the tool picker.
   * When disabled, `webSearch` below describes a backend nothing calls.
   */
  webSearchEnabled: boolean;
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
  /**
   * Controls how this conversation's MCP tools reach the model.
   *
   * When disabled, every discovered MCP tool is declared with its full schema
   * on every request. When enabled, the schemas are withheld, the names are
   * announced in the run's own context, and the `tool_search` tool hands out a
   * schema when the model asks for one. Both paths dial the same `mcpIds`.
   */
  mcpToolDiscoveryEnabled: boolean;
  /**
   * Whether this conversation's commands run in the operating system's
   * sandbox, and what it lets through. Absent is off, with the default
   * network allowlist waiting for when it is switched on.
   */
  sandbox?: SandboxSettings;
  /* The five file write guards used to be switches here. They are now
   * unconditional in the host — every conversation, and every child of one,
   * runs with read-before-write, the stale-write refusal, external-change
   * notices, hook re-sync and the formatter hint — so there is nothing left for
   * a conversation to carry about them. See `FileGuard` on the Rust side. */
  /**
   * What this conversation's runs have already put in front of the model.
   * Absent until the first run. Per-conversation runtime state, not a preset
   * component: it records history and must never be copied into one.
   */
  toolLock?: ConversationToolLock;
}

/**
 * The tool surface a conversation has already exposed. Each run merges its own
 * exposure in, and it never shrinks: a transcript that already calls a tool
 * cannot be replayed to a model that no longer has it, so the settings panel
 * offers additions only.
 *
 * The four nullable fields are pins rather than growing sets. What they hold is
 * not a collection of names but one answer that has already been acted on, and
 * a second answer would contradict the transcript rather than extend it. Each
 * is `null` until the run that sets it.
 */
export interface ConversationToolLock {
  tools: string[];
  mcpIds: string[];
  globalMemory: boolean;
  projectMemory: boolean;
  skillTool: boolean;
  /**
   * Whether a run has already withheld MCP tool schemas behind `tool_search`.
   * A pin rather than a widening, for the same reason `skillTool` is: the
   * transcript carries this conversation's MCP tools one way or the other, and
   * the other way would either redeclare what is already in it or point at a
   * tool the earlier rounds never had.
   */
  mcpToolDiscovery: boolean;
  /**
   * Whether a run has already granted web access. One bit for the feature, not
   * one per tool name: which of `web_search` / `web_fetch` a run grants follows
   * the resolved backend's capabilities, the same way which memory tools a tier
   * grants follows the tier.
   */
  webSearch: boolean;
  /**
   * Skills already handed to the model, by catalog id. They cannot be taken out
   * of the conversation again: their bodies, or the trigger lines standing in
   * for them, are already somewhere in the transcript.
   */
  skillIds: string[];
  /**
   * The skills the system prompt was assembled from, fixed by this
   * conversation's first run and `null` before it. Skills selected afterwards
   * arrive as their own system message instead, so the prompt the earlier
   * rounds were cached against never changes under them.
   */
  promptSkillIds: string[] | null;
  /**
   * The backend that has performed this conversation's searches. Pinned because
   * a native search leaves provider-sealed blocks in the transcript that only
   * that provider's models can read back, and a host-run search leaves ordinary
   * tool results that a native backend would never have produced.
   */
  searchProvider: SearchProviderSelection | null;
  /** The backend that has fetched pages here, pinned for the same reason. `null` while no run has granted `web_fetch` at all. */
  fetchProvider: FetchProviderSelection | null;
}

/** The reusable subset of conversation settings owned by a conversation preset. */
export interface ConversationPresetSettings {
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
  /**
   * Web-access template copied into conversations. An enabled preset derives
   * `web_search`, and `web_fetch` too when the backend can fetch.
   */
  webSearchEnabled: boolean;
  /** Security-policy template copied into conversations; in-conversation changes use the composer selector. */
  securityLevel: SecurityLevel;
  /** Memory-tier templates copied into conversations; enabled tiers inject context and expose their tools. */
  globalMemoryEnabled: boolean;
  projectMemoryEnabled: boolean;
  /** On-demand skill-loading template copied into conversations. */
  skillToolEnabled: boolean;
  /** MCP tool-discovery template copied into conversations. */
  mcpToolDiscoveryEnabled: boolean;
  /** Sandbox template copied into conversations; absent means off. */
  sandbox?: SandboxSettings;
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

/**
 * The conversation a timeline fork was taken from, and which of its forks this is.
 *
 * A fork is titled `<origin title>-fork-<number>` and keeps following the
 * origin's title (see `lib/conversationForks.ts`). A fork of a fork names the
 * same origin, so all forks of one conversation share one numbering — keyed by
 * the origin's id, never its title, so two conversations with the same title
 * count separately.
 */
export interface ConversationForkOrigin {
  conversationId: string;
  number: number;
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
 * An isolated Git worktree used by this conversation in place of one of its
 * project's workspaces.
 *
 * The host runs this conversation's tools against `path` wherever they name
 * that workspace, isolating it from the project's other conversations. It
 * belongs to the conversation instance, not `ConversationSettings`, because
 * settings are copied by presets.
 */
export interface ConversationWorktree {
  /** Absolute worktree root path, on the machine of the workspace it came from. */
  path: string;
  /** Branch created for this worktree. */
  branch: string;
  /** Baseline commit used at release to determine whether extra commits exist. */
  baseOid: string;
  /** The branch the worktree was forked from; absent for a detached HEAD or an older record. */
  baseBranch?: string | null;
  /**
   * The project workspace this worktree was checked out from — its machine and registered
   * directory — which it stands in for. Absent on records from before every workspace could
   * have one: those are workspace 1's.
   */
  workspace?: AttachedWorkspace | null;
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

/**
 * One directory a conversation may work in, together with the machine it is on.
 *
 * `machine` absent (or `null`) is the host machine, matching {@link RunTarget}'s
 * "local is the absent variant" convention. The model never addresses these by
 * path: it names a workspace by its 1-based position in the conversation's list,
 * which is what the `workspace` parameter on every path-taking tool carries.
 */
export interface AttachedWorkspace {
  machine?: RunTarget | null;
  /** Absolute path on that machine. A remote path is POSIX and may begin `~`. */
  path: string;
}

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
  /**
   * Isolated worktrees, at most one per project workspace; a workspace without one runs at its
   * registered directory. See {@link ConversationWorktree} and `worktreeFor`.
   */
  worktrees: ConversationWorktree[];
  /** Execution target, or `null` for local execution. See {@link RunTarget}. */
  runTarget: RunTarget | null;
  /**
   * Directories outside the primary workspace that this conversation may also
   * work in, each on the machine it names — the composer's workspace chips.
   * Together with the primary workspace they form the numbered list the model
   * addresses: the primary is workspace 1 and these follow in order.
   *
   * On the conversation rather than in {@link ConversationSettings} for the same
   * reason as `worktrees`: presets and workspace snapshots copy settings
   * wholesale, and one conversation's granted path is not another's. Each entry
   * came back from a host directory picker — native for the host machine, the
   * remote browser for a WSL or SSH machine — which is the only thing that
   * authorizes it; writing a path here that no picker returned makes the
   * document unsavable.
   */
  attachedWorkspaces: AttachedWorkspace[];
  /**
   * Superseded by {@link Conversation.attachedWorkspaces}, which carries a
   * machine alongside each path. Present only on documents written before
   * workspaces could be remote; the host folds it into host-machine entries and
   * never writes it back.
   */
  additionalDirectories?: string[];
  /**
   * The conversation this one was forked from, or `null` for a top-level
   * conversation. Nesting is a renderer concept only: a child has exactly the
   * permissions its own `settings` grant. A parent that no longer exists
   * renders the child at top level.
   */
  parentConversationId: string | null;
  /**
   * Set on a conversation forked from the timeline's context menu while its
   * title is still the one named after its origin. Renaming the fork clears
   * it: a name the user chose no longer follows anything.
   */
  forkOf?: ConversationForkOrigin | null;
  /**
   * Conversation preset most recently applied. Empty means an unnamed draft:
   * either nothing was ever applied, or a preset-owned field has since changed.
   * A trace, not a link — it never validates, never disables a field, and may
   * dangle once its preset is deleted.
   */
  presetId: string;
  /**
   * Conversation template most recently applied, or empty for none. A trace on
   * the same terms as `presetId`: never validated, never disabling, free to
   * dangle. It is what lets the owner tell "this timeline is that template's
   * message queue" from "this timeline is the user's own work" — which is the
   * difference between a preset quietly replacing the timeline and asking first.
   */
  templateId: string;
}

export interface QueuedMessage {
  id: string;
  content: string;
  images?: ImageAttachment[];
  files?: FileAttachment[];
  createdAt: string;
}

export type WorkspaceKind = "directory" | "temporary";

export interface Workspace {
  id: string;
  name: string;
  kind: WorkspaceKind;
  path: string;
  /**
   * Machine this workspace's directory lives on. Absent (or `null`) is the host
   * machine, which is what every workspace registered before machines existed is.
   */
  machine?: RunTarget | null;
  /**
   * The project's workspaces after the first. The sidebar entry is a project: one
   * or more directories, each on its own machine. `path` and `machine` above are
   * the first — workspace 1, the one the Git chip shows by default — and these
   * follow it as workspaces 2, 3, … in every conversation
   * of the project, ahead of the conversation's own attached workspaces.
   *
   * Absent on projects registered with a single directory. Each entry came back
   * from a host directory picker, on the same terms as an attached workspace.
   */
  additionalWorkspaces?: AttachedWorkspace[];
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
  /**
   * The one conversation template this preset opens with, or empty for none.
   *
   * Bound by id on the same terms as a role's: the body lives in the host's
   * template store, so nothing the renderer writes here can become a tool result
   * the model believes really ran. May dangle — a template that is gone opens
   * nothing.
   */
  templateId: string;
  settings: ConversationPresetSettings;
}

/**
 * A stored message queue, as the renderer sees it without its body. Bodies are
 * never sent here: the host owns them, because a template carries tool cards and
 * applying one re-issues host receipts. The renderer only ever names a template.
 *
 * Nothing draws `name` any more — a template is titled by whatever owns it, a
 * preset or a role — but the host still keeps the column, so it is still read.
 */
export interface ConversationTemplateSummary {
  id: string;
  name: string;
  messageCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface ResourceDescriptor {
  id: string;
  name: string;
  description: string;
  location: string;
  source: "builtin" | "user" | "workspace";
  available: boolean;
  /**
   * The directory workspace this entry was read from (`<workspace>/.mework`),
   * when it was read from one. Absent for global (`~/.mework`) and built-in
   * entries, which every conversation may select. A conversation may select
   * only entries with no `workspaceId` or with its own workspace's id; the
   * host resolves its runs against exactly that set.
   */
  workspaceId?: string;
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
export type FamilySetting = "region" | "project" | "location" | "api_version";

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
  /**
   * No backend searches for this conversation. Distinct from turning web access
   * off: the fetch leg keeps whatever backend it names, so a conversation may
   * retrieve a page it was given the address of without being able to go
   * looking for one.
   */
  | { kind: "disabled" }
  | { kind: "unavailable" };

/**
 * Which backend retrieves a named page for `web_fetch`.
 *
 * Separate from `SearchProviderSelection` because fetching and searching are
 * different upstream capabilities and a backend may have one without the other:
 * DeepSeek and OpenAI expose only a server-side search tool and keep page
 * retrieval internal to it, while Anthropic exposes search and fetch as two
 * distinct server tools. `native` names the conversation's own upstream,
 * independently of who searches: on Anthropic that grants a second web tool, and
 * on a family that folds retrieval into its search tool it grants none, which is
 * exactly that family's own shape.
 *
 * Every variant names a backend outright. There is deliberately no "automatic":
 * a selector that resolved to something else — the search backend, or a global
 * default — reads as a choice while being an alias for one, and the thing it
 * aliased could be changed from another screen without this one saying so.
 */
export type FetchProviderSelection =
  | { kind: "native" }
  | { kind: "explicit"; providerKind: SearchProviderKind }
  | { kind: "disabled" };

/**
 * Which version of the Anthropic Messages server-side `web_search` tool the
 * native leg attaches, as the `type` that goes on the wire.
 *
 * This is the one native-backend choice that is a protocol detail rather than a
 * backend: every version names the same tool and returns the same block shapes,
 * and only Messages spells the version into the request. Families on another
 * protocol have no `type` to pick, so they ignore the selection and send their
 * own native tool instead — the selection is kept rather than rewritten, so
 * returning to a Messages model returns to the version that was chosen.
 *
 * The list is exactly what this build can emit, bounded by the AI SDK's
 * Anthropic provider rather than by the API: a version the SDK cannot map is
 * dropped from the request with a warning, so offering it would take web search
 * away without saying so. Mirrors Rust `model::NativeSearchTool`.
 */
export type NativeSearchTool = "web_search_20250305" | "web_search_20260209";

/** The same, on the other web tool. Mirrors Rust `model::NativeFetchTool`. */
export type NativeFetchTool = "web_fetch_20250910" | "web_fetch_20260209";

/** Offered search versions, in the order the menu lists them; the first is the default. */
export const NATIVE_SEARCH_TOOLS: readonly NativeSearchTool[] = [
  "web_search_20250305",
  "web_search_20260209"
];

/** Offered fetch versions; the first is the default. */
export const NATIVE_FETCH_TOOLS: readonly NativeFetchTool[] = [
  "web_fetch_20250910",
  "web_fetch_20260209"
];

/**
 * Which domain list, if either, filters a conversation's search results.
 * Mirrors Rust `model::SearchDomainFilterMode`.
 */
export type SearchDomainFilterMode = "off" | "exclude" | "include";

/** Per-conversation web-search behavior and backend selection. */
export interface ConversationWebSearchSettings {
  /**
   * Native searches allowed inside one `web_search` call. 0 = unlimited.
   * Only the native backend reads it; a catalog provider returns `maxResults`
   * results for the one search it was asked to run.
   */
  maxSearchesPerCall: number;
  provider: SearchProviderSelection;
  /** Fetch backend for this conversation. */
  fetchProvider: FetchProviderSelection;
  /**
   * Which Messages `web_search` version the native search leg sends.
   *
   * It sits beside the backend selection rather than inside the `native`
   * variant so that leaving native — for a catalog provider, or for a model
   * whose family has no version to pick — only stops it from being used, never
   * erases it. Coming back to a Messages model finds the same version still
   * selected.
   */
  nativeSearchTool: NativeSearchTool;
  /** The same, for the native fetch leg. */
  nativeFetchTool: NativeFetchTool;
  /**
   * How many results one search asks a catalog provider for. 0 = unlimited,
   * meaning the host asks for no cap and takes whatever the upstream returns.
   * A native backend decides its own search depth and ignores this.
   */
  maxResults: number;
  /**
   * Whole-call token budget for result bodies, split evenly across the results.
   * 0 = unlimited: whatever the upstream returns goes in verbatim, so one fetch
   * can take a whole context window.
   */
  compressionCutoff: number;
  /**
   * Which of the two domain lists filters this conversation's results, if
   * either. They are a choice rather than a pair of switches because a result
   * admitted by one and refused by the other has no obvious answer, and a
   * selector that can only be in one state never asks that question.
   *
   * `off` keeps both lists — turning filtering off is not the same as throwing
   * away what was written, and coming back finds it still there.
   */
  domainFilter: SearchDomainFilterMode;
  /**
   * Result allowlist, in effect while `domainFilter` is `include`: a result is
   * kept only if it matches one of these rules, so an empty list while the mode
   * is on admits nothing.
   *
   * Syntax for both lists: `<all_urls>`, a `scheme://host/path` match pattern
   * (`*` wildcards, `*.` matches subdomains), or `/regex/`.
   */
  includeDomains: string[];
  /** Result blocklist, in effect while `domainFilter` is `exclude`: a result is
   * dropped if it matches one of these rules. */
  excludeDomains: string[];
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

/**
 * The global search assets: the provider catalog and nothing else.
 *
 * Everything about how a search BEHAVES — how many results, how hard they are
 * compressed, which domains are admitted — belongs to the conversation that
 * runs it, and to the subagent role when a role runs its own. What is left here
 * is what genuinely has one value per installation: which providers exist,
 * where they point, and whether they are switched on.
 */
export interface WebSearchAssets {
  providers: SearchProviderConfig[];
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
   * Whether the model may select this role at all. The conversation-settings
   * roles page draws it as the row's switch; a trusted project/plugin/managed
   * source may also ship a disabled definition, and that one keeps shadowing a
   * user definition of the same name.
   *
   * Capability-bearing, unlike `description` and `templateId`: the host's
   * `same_user_agent_configuration` compares it, so turning a role off advances
   * `revision` and refuses every child already bound to it — which is the point
   * of turning it off.
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
   * `searchProvider` picks this role's `web_search` backend and `fetchProvider`
   * its `web_fetch` backend; `null` follows the calling conversation on either
   * leg independently. The two are separate answers for the same reason they
   * are on a conversation — upstreams disagree about how many web tools there
   * are — and neither can grant a web tool to a conversation that is offline.
   *
   * `maxResults` and `compressionCutoff` are the role's own result shaping, in
   * the same units and with the same `0 = unlimited` reading as the
   * conversation's. They are plain numbers rather than `T | null` because 0 is
   * already an answer — "no cap" — so there is no spare value left to spell
   * "follow the conversation" with.
   *
   * `domainFilter` is `null` when this role filters results the way its caller
   * does, lists included; naming a mode takes over BOTH the mode and the two
   * lists below, so a role that filters filters by its own rules alone. The
   * lists are plain arrays: they are only read when a mode is named, and an
   * empty list under a named mode is a real configuration ("allow nothing",
   * "block nothing") rather than an absent one.
   */
  effort: ReasoningEffort | null;
  tools: string[] | null;
  disallowedTools: string[];
  searchProvider: SearchProviderSelection | null;
  fetchProvider: FetchProviderSelection | null;
  maxResults: number;
  compressionCutoff: number;
  domainFilter: SearchDomainFilterMode | null;
  includeDomains: string[];
  excludeDomains: string[];
  /**
   * Conversation template seeded as this role's opening history, or `null` for a
   * child that starts from the task alone. Only ever an id: the body lives in
   * the host's template store, so nothing the renderer writes here can become a
   * tool result the child believes really ran.
   *
   * The role owns this one rather than picking it out of a shared catalog, so
   * the id is minted the first time a body is written for it and nothing else
   * ever cites it.
   *
   * NOT capability-bearing — prose the child reads, granting no tool and binding
   * no model — so it is absent from `sameUserAgentConfiguration` for the same
   * reason `description` is. May dangle; a deleted template seeds nothing.
   */
  templateId: string | null;
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
  /** Panes of glass over {@link background}; light or dark glass follows {@link GlobalSettings.theme}. */
  liquidGlass: boolean;
  /**
   * The window's background (`lib/background.ts`): `solid`, the theme's own ground, which
   * follows the theme; `solid:day` / `solid:night`, one theme's ground picked while the other
   * was on screen, until the theme changes; `builtin:<name>`; or an imported picture's host id.
   */
  background: string;
  /** The local helper model's uses and prompts (Appearance → Local model). */
  localModel: LocalModelPreferences;
}

/** Mirror of Rust `model::LocalModelPreferences`. Empty prompts mean the built-in ones. */
export interface LocalModelPreferences {
  /** Name conversations from their first message. */
  titles: boolean;
  /** Describe each shell command in one line on its card. */
  shellExplanations: boolean;
  titlePrompt: string;
  shellPrompt: string;
}

/** Mirror of Rust `helper_model::Phase` (serde tag `phase`). */
/** Mirror of Rust `helper_model::VariantId`: one build of the model per inference backend. */
export type LocalModelVariantId = "ane" | "mlx" | "llama";

/** Mirror of Rust `helper_model::Unavailable`: why a build cannot run on this machine. */
export type LocalModelUnavailable =
  | "needsAppleSilicon"
  | "noNeuralEngine"
  | "needsMacos14"
  | "needsMacos15"
  | "notInThisBuild";

export type LocalModelPhase =
  | { phase: "missing" }
  | { phase: "unsupported"; reason: LocalModelUnavailable }
  | { phase: "downloading"; received: number; total: number; source: "huggingFace" | "hfMirror" | "mirror" }
  | { phase: "preparing"; step: "compile" | "verify"; done: number; total: number }
  | { phase: "ready" }
  | { phase: "failed"; message: string };

/** Mirror of Rust `helper_model::VariantStatus`. */
export type LocalModelVariantStatus = LocalModelPhase & {
  id: LocalModelVariantId;
  downloadBytes: number;
  diskBytes: number;
};

/** Mirror of Rust `helper_model::Machine`. */
export interface LocalModelMachine {
  chip: string | null;
  model: string | null;
  osVersion: string | null;
  appleSilicon: boolean;
  neuralEngineCores: number | null;
}

/** Mirror of Rust `helper_model::Status`. */
export interface LocalModelStatus {
  machine: LocalModelMachine;
  /** Every build this app knows, best first. */
  variants: LocalModelVariantStatus[];
  active: LocalModelVariantId | null;
  recommended: LocalModelVariantId | null;
  /** The active build is loading and caching its prompts. */
  warming: boolean;
  device: string | null;
  loaded: boolean;
  running: number;
  queued: number;
  slots: number;
  context: number;
  diskBytes: number;
  lastError: string | null;
}

/** Mirror of Rust `helper_model::PromptReport`. */
export interface LocalModelPromptReport {
  tokens: number;
  cacheBytes: number;
  maxTokens: number;
}

/** A background picture the host has stored, as its largest tier. */
export interface BackgroundImage {
  id: string;
  width: number;
  height: number;
}

/** One tier of a background picture, ready to paint. */
export interface BackgroundImageData {
  dataUrl: string;
  width: number;
  height: number;
  /** No larger tier exists. */
  largest: boolean;
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
export type AppInstallFlavor = "installer" | "portable" | "msix";

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
  /** The asset for this machine's flavor and architecture, when the release has one. Always null when `inAppInstall` is false. */
  asset: AppReleaseAsset | null;
  /** `SHA256SUMS` when the release publishes one. */
  checksumsAsset: AppReleaseAsset | null;
  checkedAt: string;
  /**
   * Whether this host can download and install the update in-app. Only Windows can: every
   * release asset is a Windows build, so other hosts are sent to the release page.
   */
  inAppInstall: boolean;
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

/*
 * Skills and MCP servers have no renderer-side record types. Both are files the
 * user owns — `~/.mework/skills/<dir>/SKILL.md` and `<level>/mcp.json` — so the
 * only renderer view of them is the `ResourceDescriptor` the host's
 * `discover_capabilities` scan returns, and the only writes are the host's
 * `delete_skill` / `delete_mcp_server` commands. The in-app registries
 * (`assets.skills`, `assets.mcpServers`) and the online skill-registry search are
 * retired; nothing here mirrors them.
 */

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
  /** New conversations link to this preset. The host keeps the built-in preset in every document and points a default that no longer resolves at it. */
  defaultConversationPresetId: string;
  /** Default inherited by the next conversation; each conversation keeps its own value. */
  lastReasoningEffort: ReasoningEffort;
  apiProviders: ApiProvider[];
  activeProviderId: string | null;
  /** Web-search assets: external providers and the global selected provider. */
  webSearch: WebSearchAssets;
  /** Appearance preferences. */
  appearance: AppearancePreferences;
  /** Per-command bindings. Absent entries use code defaults; resetting a command removes its entry. */
  shortcuts: Partial<Record<ShortcutCommandId, ShortcutPreference>>;
  /** User-added environment dependencies; built-ins are code constants. */
  environmentTools: EnvironmentToolDefinition[];
  /** Execution-environment assets: SSH machine catalog and environment-keyed variables. */
  executionEnvironments: ExecutionEnvironmentAssets;
  /**
   * The unsent new task's own settings, absent when there is none. The draft
   * copies a preset once, when it is opened, and from then on owns its
   * settings like any conversation; keeping them here is what lets it survive a
   * restart instead of being rebuilt from the preset.
   */
  draftConversation?: DraftConversationSnapshot | null;
}

/** What of the new-task draft outlives the process: its settings and their preset trace. */
export interface DraftConversationSnapshot {
  settings: ConversationSettings;
  /** Same trace semantics as `Conversation.presetId`. */
  presetId: string;
}

/**
 * User-registered SSH execution machine. `host` accepts `user@hostname`, a
 * hostname, or an `~/.ssh/config` host alias. Authentication material is not
 * persisted; OpenSSH resolves it from identity files and the agent.
 *
 * The machine carries no working directory. A directory on it is a workspace
 * like any other — picked through the remote directory browser and recorded on
 * the conversation, where it gets the number the model addresses it by.
 */
export interface SshMachineConfig {
  id: string;
  name: string;
  host: string;
  /** `0` uses the default SSH port, 22. */
  port: number;
  /** Private key path; empty uses OpenSSH's default resolution. */
  identityFile: string;
  /**
   * The shell the machine's agent runs Mework's own scripts in (the remote file
   * tools and language servers). Absent until the machine is first probed, when
   * the first backend of its OS's priority list that it has is recorded here.
   */
  agentShell?: ShellBackend;
  createdAt: string;
  updatedAt: string;
}

/**
 * A shell Mework runs commands and its own scripts through. Mirrors the host's
 * `shell_backend::ShellBackend`; which ones a machine has is found by probing it.
 */
export type ShellBackend = "bash" | "zsh" | "sh" | "powershell";

/** A machine's operating system. WSL is one of them, not a kind of shell. */
export type MachineOs = "windows" | "macos" | "linux" | "wsl";

/** One backend a probe found, and where. */
export interface DetectedShell {
  backend: ShellBackend;
  path: string;
}

/** What a probe learned about one machine. Mirrors `machine_shells::MachineShells`. */
export interface MachineShells {
  os: MachineOs;
  shells: DetectedShell[];
  probedAt: string;
}

/**
 * Execution-environment assets. WSL distributions are machine state and are
 * enumerated live by `list_wsl_distros`. Environment variables are keyed by
 * `local`, `wsl:<distro>`, or `ssh:<machine id>`.
 */
export interface ExecutionEnvironmentAssets {
  sshMachines: SshMachineConfig[];
  envVars: Record<string, Record<string, string>>;
  /** Each WSL distribution's agent shell, by distribution name. */
  wslAgentShells?: Record<string, ShellBackend>;
}

/** Which hosts a sandbox's processes may connect to. */
export type SandboxNetworkMode = "off" | "allowlist" | "open";

/**
 * The sandbox a conversation's commands run in when it is on. Mirrors
 * `model::SandboxSettings`: one sandboxed agent process per conversation and
 * machine, which can write the conversation's workspaces (and nothing in them
 * that runs outside the sandbox later), cannot read credentials, and reaches
 * the network only through a proxy that applies `network`. A setting of each
 * conversation, and a component of a preset — the conversation is the
 * smallest thing a sandbox is drawn around.
 */
export interface SandboxSettings {
  enabled: boolean;
  network: {
    mode: SandboxNetworkMode;
    /** `example.com`, `*.example.com` (subdomains only), optionally `:port`. */
    allow: string[];
    deny: string[];
  };
  /** Further directories every sandbox may write: absolute or `~/…`. */
  writable: string[];
  /** Further paths no sandbox may read, besides the built-in credential locations. */
  denyRead: string[];
}

/** What a machine's agent reports about sandboxing there. Mirrors `protocol::SandboxSupport`. */
export interface SandboxSupport {
  /** `seatbelt`, `bubblewrap`, `srt-win`; empty when none. */
  backend: string;
  available: boolean;
  detail: string;
  /** Not available until the machine is set up for it once, with administrator rights (Windows). */
  setup: boolean;
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

export interface AttestEditedToolContextRequest {
  conversationId: string;
  contextId: string;
  toolName: string;
  input: JsonObject;
  output: string;
  images: ImageAttachment[];
}

export interface AttestEditedToolContextResponse {
  input: JsonObject;
  result: ToolResult;
  attestation: string;
}

/** A call written out by hand rather than executed. The host owns every result
 * field but the text, so none of them are sent. */
export interface AttestInsertedToolContextRequest {
  conversationId: string;
  contextId: string;
  toolName: string;
  input: JsonObject;
  output: string;
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
   * to leave plan mode and start implementing; it is answered with the same
   * decisions but draws different copy and collects feedback on a denial.
   */
  kind?: "tool" | "plan_exit";
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
  | { type: "user_input_received"; round: number; id: string; content: string; images?: ImageAttachment[]; files?: FileAttachment[]; createdAt: string }
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
  | { type: "tool_approval_requested"; promptId: string; toolName: string; label: string; summary: string; riskLevel: string; reason: string; requester?: string; sourceAgent?: string; sourceCallId?: string; allowAlwaysOffered: boolean; mandatory?: boolean; kind?: "tool" | "plan_exit" }
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
  | "providers"
  | "search_providers"
  | "mcp"
  | "skills"
  | "shortcuts"
  | "usage"
  | "execution_environments"
  | "dependencies"
  | "updates"
  | "web_search"
  | "memory"
  | "agents"
  | "hooks"
  | "advanced"
  | "conversation_presets"
  | "capability_catalog";
