import { ArrowRight, Bot, FileText, Plus, Settings2, Wrench } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  AGENT_TOOL_WILDCARD,
  MAX_AGENT_TYPE_CHARS,
  agentDefinitionModelIsAvailable,
  buildUserAgentDefinition,
  canonicalAgentToolNames,
  editableUserAgentDefinition,
  setUserAgentDefinitionEnabled,
  userAgentDefinitionDraft,
  validateAgentTypeName
} from "../lib/agentDefinitions";
import type { UserAgentDefinitionDraft } from "../lib/agentDefinitions";
import { supportsVision } from "../lib/modelCapabilities";
import { isPreviewLifecycleToolName } from "../lib/taskTools";
import {
  DEFAULT_SEARCH_COMPRESSION_CUTOFF,
  DEFAULT_SEARCH_MAX_RESULTS
} from "../lib/searchProviders";
import type {
  AgentDefinition,
  AgentModelSelection,
  ApiProvider,
  ContextItem,
  ConversationPreset,
  ConversationTemplateSummary,
  FetchProviderSelection,
  ReasoningEffort,
  SearchDomainFilterMode,
  SearchProviderSelection,
  ToolDescriptor,
  WebSearchAssets
} from "../types";
import { CatalogRow, useCatalogSort } from "./CatalogRow";
import { ConfirmDeleteButton, Dialog, Field, IconButton, Switch } from "./Common";
import { ConversationTemplateEditor } from "./ConversationTemplateEditor";
import { FeaturesPage } from "./FeaturesPage";
import { reorderItems } from "./usePointerDrag";
import "./AgentDefinitionSettings.css";

interface AgentDefinitionSettingsProps {
  definitions: readonly AgentDefinition[];
  /** Unique among the lists on screen: the preset editor mounts a second copy. */
  listId: string;
  providers: readonly ApiProvider[];
  /** The whole trusted tool catalogue. A role selects out of it without being
   * bounded by what the conversation enabled. */
  tools: readonly ToolDescriptor[];
  /** Names the conversation itself enables — the set "inherit" restores, and
   * the set the picker shows while a role has no allowlist of its own. */
  conversationEnabledTools: readonly string[];
  /** Whether the conversation's own model reads images — what a role bound to
   * "inherit" will run on, and so the answer its template page needs. */
  conversationImageInputSupported?: boolean;
  /** The search-provider catalogue, so a role can pick its own backend. */
  webSearchAssets: WebSearchAssets;
  /** Every stored template, for the message counts the template page reports. */
  templates: ConversationTemplateSummary[];
  /** Offered on the template page as bodies to copy over the role's own. */
  presets: readonly ConversationPreset[];
  onReadTemplate: (templateId: string) => Promise<ContextItem[]>;
  /** Writes a body and resolves with the id it landed under, minting when empty. */
  onWriteTemplate: (templateId: string, contexts: ContextItem[]) => Promise<string>;
  /** Widens the conversation's enabled set when a template calls a tool it lacks,
   * for a role that follows that set rather than keeping a list of its own. */
  onEnableTools?: (names: string[]) => void;
  onChange: (definitions: AgentDefinition[]) => void;
}

interface EditorState {
  mode: "create" | "edit";
  originalName: string | null;
  originalRevision: number | null;
  draft: {
    name: string;
    /** Free text; empty means this role contributes no line to the block the
     * host appends to `agent_spawn` / `workflow`'s description. Multi-line and
     * uncapped on purpose — the host writes it through verbatim. */
    description: string;
    modelSelection: AgentModelSelection;
    effort: ReasoningEffort | null;
    /** `null` means "inherit the conversation's own enabled tools". A list is an
     * explicit allowlist selected out of the whole catalogue, and an EMPTY list
     * is a real configuration meaning "no tools" — which is why this is not
     * merely an empty array. */
    tools: string[] | null;
    /** `null` means "follow the conversation's search backend". */
    searchProvider: SearchProviderSelection | null;
    /** The same, on the other leg. */
    fetchProvider: FetchProviderSelection | null;
    /** This role's own result shaping; 0 = unlimited for both. Plain numbers
     * rather than `T | null` because 0 is already the "no cap" answer, leaving
     * nothing over to spell "follow the conversation" with. */
    maxResults: number;
    compressionCutoff: number;
    /** `null` follows the conversation's filtering, its lists included. */
    domainFilter: SearchDomainFilterMode | null;
    includeDomains: string[];
    excludeDomains: string[];
    /** Template seeded as this role's opening history; `null` starts from the
     * task alone. An id only — the body lives in the host's template store. */
    templateId: string | null;
  };
}

/**
 * The role editors left open in mid-edit, by the list they belong to.
 *
 * Deliberately module state, and deliberately NOT persisted. A role dialog is a
 * long form — a name, prose, four backend answers, a tool allowlist — and
 * closing it to go look at what the conversation actually enables used to throw
 * all of that away. So a draft outlives its dialog: reopening the same role, or
 * the new-role dialog, comes back to exactly what was typed.
 *
 * It does not outlive the process, because it is not a saved thing. Nothing is
 * written to the role until Save, and a draft that survived a restart would be
 * an unsaved edit the user could no longer tell apart from a saved one.
 *
 * Each list keys its own entries, and `listId` already carries the conversation
 * — the preset editor mounts a second list under a name of its own — so two
 * conversations never share one in-progress role.
 *
 * Saving clears the entry: the draft has become the role, and keeping a copy
 * would reopen the dialog on a "draft" identical to what is already stored.
 */
const editorDrafts = new Map<string, EditorState>();

/** A ceiling, so a long session cannot accumulate drafts without bound. */
const MAX_EDITOR_DRAFTS = 64;

/** `\u0000` cannot occur in either half: `validateAgentTypeName` refuses a role
 * name carrying a control character, so the two parts can never run together. */
function draftKey(listId: string, roleName: string | null): string {
  return `${listId}\u0000${roleName ?? ""}`;
}

function rememberDraft(key: string, state: EditorState): void {
  // Re-inserting moves the key to the end, so eviction drops the least recently
  // touched draft rather than an arbitrary one.
  editorDrafts.delete(key);
  editorDrafts.set(key, state);
  while (editorDrafts.size > MAX_EDITOR_DRAFTS) {
    const oldest = editorDrafts.keys().next();
    if (oldest.done) break;
    editorDrafts.delete(oldest.value);
  }
}

/**
 * The pages the role editor lists down its left edge.
 *
 * Laid out the way a preset's window is — a rail of pages on the left, the page
 * on the right, the save under the rail — because it is the same kind of thing:
 * one reusable body, opened to be edited. `role` is what the role is called and
 * what it runs on; `tools` is the conversation's own features page with the
 * sections a role cannot answer left out; `template` is the opening history.
 */
type RoleEditorPage = "role" | "tools" | "template";

const ROLE_EDITOR_PAGES: ReadonlyArray<{ id: RoleEditorPage; icon: typeof Bot }> = [
  { id: "role", icon: Bot },
  { id: "tools", icon: Wrench },
  { id: "template", icon: FileText }
];

const AGENT_EFFORTS: readonly ReasoningEffort[] = [
  "disabled",
  "low",
  "medium",
  "high",
  "xhigh"
];

interface ExplicitModelOption {
  key: string;
  providerId: string;
  providerName: string;
  modelId: string;
}

/**
 * The roles a conversation preset offers the model.
 *
 * A role answers four questions — which model it runs on, which tools it may
 * use, which search backend those searches go through, and what it tells the
 * model it is for. It carries no system prompt of its own: a named child
 * renders the conversation's prompt through the same subagent addendum an
 * ordinary child gets, so a role is a routing choice rather than a second
 * persona to keep in sync. The description is part of that routing choice and
 * nothing more — it is prose the model reads when picking a name, never
 * instructions the child receives.
 */
export function AgentDefinitionSettings({
  definitions,
  listId,
  providers,
  tools,
  conversationEnabledTools,
  conversationImageInputSupported = false,
  webSearchAssets,
  templates,
  presets,
  onReadTemplate,
  onWriteTemplate,
  onEnableTools,
  onChange
}: AgentDefinitionSettingsProps) {
  const { t } = useI18n();
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  /* Every role opens on its own settings page: the name is the one thing a
   * new role cannot be saved without. */
  const [page, setPage] = useState<RoleEditorPage>("role");
  /* The role's template body, read the first time its page is opened. It lives
   * here rather than in the template editor because the rail needs it too: a
   * preset's body is only copied over this one after asking, and only when
   * there is something here to lose. The body is written straight to the host
   * as it is edited; the role's draft holds only the id, and only once there is
   * one. */
  const [templateBody, setTemplateBody] = useState<ContextItem[] | null>(null);
  /* The id whose body `templateBody` already holds. A first save mints the id
   * the draft then takes, and re-reading a body this editor just wrote would be
   * a second chance to lose what was typed since, not a refresh. */
  const heldTemplateId = useRef<string | null>(null);
  /* Bumped when a body the template editor did not write lands under it — a
   * preset's, copied over — so the editor starts again on that body instead of
   * keeping the draft it was seeded with. */
  const [templateGeneration, setTemplateGeneration] = useState(0);
  /* A preset whose template is waiting on the user's word before it replaces
   * the role's non-empty one. */
  const [pendingOverwrite, setPendingOverwrite] = useState<ConversationPreset | null>(null);
  const [overwriting, setOverwriting] = useState(false);
  const [overwriteError, setOverwriteError] = useState<string | null>(null);
  /* Whether the name has been asked to be valid yet. A create dialog opens on an
   * empty name, and announcing "a role name is required" before the user has had
   * a chance to type one reads as a complaint about their not having typed it.
   * Save is what asks. */
  const [nameChecked, setNameChecked] = useState(false);

  const activeDefinitions = useMemo(
    () => definitions.filter((definition) => !definition.deleted),
    [definitions]
  );
  const userDefinitions = useMemo(
    () => activeDefinitions.filter(editableUserAgentDefinition),
    [activeDefinitions]
  );
  /* Two whole categories are withheld here, for two unrelated reasons.
   *
   * `orchestration` — a child may hold NONE of it. The host strips
   * `agent_spawn` / `send_message` / `followup_task` / `task_wait` /
   * `task_list` / `workflow` / `todo` / `ask_user` from every child
   * template (`SUBAGENT_DISABLED_TOOL_NAMES` in `src-tauri/src/api.rs`), and
   * that list is re-applied to the catalogue a role's allowlist selects out
   * of — so ticking one of them here could never grant it. It could only make
   * the role worse: a role whose allowlist named nothing else resolves to the
   * empty set and its first spawn fails outright ("no usable tools resolved" —
   * the one degenerate case the host lets through is a conversation that
   * enabled no tools at all). Drawing a switch for a capability this screen
   * cannot grant is a promise it cannot keep.
   *
   * Excluded by CATEGORY rather than by name deliberately. The name list lives
   * in Rust and is the authority; a second copy here would drift, and it would
   * drift in the dangerous direction — a newly denied orchestration tool would
   * silently start appearing as a grantable switch. By category the default is
   * closed.
   *
   * Two members of that category are NOT actually withdrawn by this rule.
   * `skill` is available to a child on purpose, and `task_wait` / `task_list`
   * are re-derived for a child that still holds a task-producing tool. All
   * three are host-derived (`isHostDerivedToolName`), so `ToolSelectionGroups`
   * has never offered them in any picker and nothing on this screen decides
   * their fate — the host exempts them from the allowlist in both directions.
   *
   * `memory` — excluded for a different reason entirely: memory availability
   * derives from the conversation's memory switches plus the role's memory
   * binding, never from a tool list.
   *
   * NOTE what is NOT a rule here: the conversation's own enabled set. This
   * picker draws from the whole catalogue, and the host honours that — a tool
   * ticked here is granted even if the calling conversation switched it off.
   * `web_search` / `web_fetch` are ordinary `web` entries. */
  const selectableTools = useMemo(
    () => tools.filter((tool) =>
      tool.category !== "orchestration" && tool.category !== "memory"),
    [tools]
  );
  const selectableToolNames = useMemo(
    () => selectableTools.map((tool) => tool.name),
    [selectableTools]
  );
  const selectableToolNameSet = useMemo(
    () => new Set(selectableToolNames),
    [selectableToolNames]
  );
  const inheritedToolNames = useMemo(
    () => conversationEnabledTools.filter((name) => selectableToolNameSet.has(name)),
    [conversationEnabledTools, selectableToolNameSet]
  );
  /* How many of a saved allowlist's entries this screen can actually show.
   *
   * An allowlist may legitimately name tools the picker does not offer — an
   * orchestration name ticked before this screen stopped listing them, or a
   * name from a catalogue this build no longer carries. Those entries are left
   * alone rather than stripped on load: the host intersects them away, so they
   * are inert, while rewriting them here would advance `revision` and cut every
   * child already bound to this role over a capability it never actually held.
   * They must not be COUNTED, though. Counting the raw array would make the
   * header report tools the picker is not drawing, and would leave "Enable all"
   * disabled while visible switches are still off. The preview lifecycle tools
   * have no switch of their own either: they follow the other preview tools. */
  const visibleSelectedCount = (names: readonly string[]) =>
    names.filter((name) => selectableToolNameSet.has(name) && !isPreviewLifecycleToolName(name)).length;
  const explicitModelOptions = useMemo<ExplicitModelOption[]>(() => {
    const result: ExplicitModelOption[] = [];
    for (const provider of providers) {
      if (!provider.enabled) continue;
      for (const model of provider.models) {
        result.push({
          key: `configured-model-${result.length}`,
          providerId: provider.id,
          providerName: provider.name,
          modelId: model.id
        });
      }
    }
    return result;
  }, [providers]);

  /* A brand-new role follows its caller in every answer it has one for: the
   * model, the effort, both web backends, the domain filter, and — through
   * `tools: null` — the conversation's own enabled set. It is the only opening
   * state that cannot be wrong, because it is not yet a decision: a role that
   * started from a snapshot of the conversation would silently stop tracking it
   * the moment the conversation changed. Result shaping is the one exception,
   * and only because 0 already means "no cap" there, leaving no value over to
   * spell inheritance with. */
  const blankDraft = (): EditorState["draft"] => ({
    name: "",
    description: "",
    modelSelection: { kind: "inherit" },
    effort: null,
    tools: null,
    searchProvider: null,
    fetchProvider: null,
    maxResults: DEFAULT_SEARCH_MAX_RESULTS,
    compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
    domainFilter: null,
    includeDomains: [],
    excludeDomains: [],
    templateId: null
  });

  /* Whatever the last role left on screen — the page it was on, the template
   * body it read, a question it was asking — belongs to that role. */
  const resetEditorChrome = () => {
    setPage("role");
    setTemplateBody(null);
    heldTemplateId.current = null;
    setTemplateGeneration((generation) => generation + 1);
    setPendingOverwrite(null);
    setOverwriteError(null);
  };

  const beginCreate = () => {
    const key = draftKey(listId, null);
    const remembered = editorDrafts.get(key);
    setEditor(remembered ?? {
      mode: "create",
      originalName: null,
      originalRevision: null,
      draft: blankDraft()
    });
    resetEditorChrome();
    setEditorError(null);
    // A remembered draft has already been typed into, so its name may be
    // answered for; a fresh one has not been asked yet.
    setNameChecked(Boolean(remembered?.draft.name.trim()));
  };

  const beginEdit = (definition: AgentDefinition) => {
    if (!editableUserAgentDefinition(definition)) return;
    const draft = userAgentDefinitionDraft(definition);
    /* A draft left open on this role, unless the role has moved underneath it.
     * A revision that advanced elsewhere means the saved role is no longer the
     * one this draft was started from, and saving it would be refused anyway —
     * so the stored role wins and the stale draft is dropped rather than shown
     * as if it were still live. */
    const key = draftKey(listId, definition.name);
    const remembered = editorDrafts.get(key);
    if (remembered && remembered.originalRevision !== definition.revision) {
      editorDrafts.delete(key);
    }
    setEditor(remembered?.originalRevision === definition.revision ? remembered : {
      mode: "edit",
      originalName: definition.name,
      originalRevision: definition.revision,
      draft: {
        name: draft.name,
        description: draft.description,
        modelSelection: draft.modelSelection,
        effort: draft.effort,
        // A saved wildcard allowlist means the same thing as no allowlist, and
        // the picker has no way to draw "everything including names I cannot
        // see", so it opens on inherit.
        tools: draft.tools === null || draft.tools.includes(AGENT_TOOL_WILDCARD)
          ? null
          : draft.tools,
        searchProvider: draft.searchProvider,
        fetchProvider: draft.fetchProvider,
        maxResults: draft.maxResults,
        compressionCutoff: draft.compressionCutoff,
        domainFilter: draft.domainFilter,
        includeDomains: draft.includeDomains,
        excludeDomains: draft.excludeDomains,
        templateId: draft.templateId
      }
    });
    resetEditorChrome();
    setEditorError(null);
    // An existing role arrives with a valid name, so validation has nothing to
    // withhold: any error from here on is one the user has just introduced.
    setNameChecked(true);
  };

  const replaceEditorDraft = (change: Partial<EditorState["draft"]>) => {
    setEditor((current) => {
      if (!current) return current;
      const next = { ...current, draft: { ...current.draft, ...change } };
      // Every keystroke lands in the cache as well as in state, so closing the
      // dialog needs no save of its own — there is nothing left to lose by then.
      rememberDraft(draftKey(listId, current.originalName), next);
      return next;
    });
    setEditorError(null);
    // Typing a name is the user taking up the question, so from the first
    // keystroke the field answers live. Clearing it again does not put the
    // complaint back — that is the state the dialog opened in.
    if (typeof change.name === "string" && change.name.trim().length > 0) setNameChecked(true);
  };

  /* Closing keeps the draft. It is the whole point of the cache: leaving to
   * check what the conversation enables, or to read a tool's page, is not a
   * decision to throw the half-written role away. Nothing has been written to
   * the role either way — Save is still the only thing that writes. */
  const closeEditor = () => {
    setEditor(null);
    setPendingOverwrite(null);
    setEditorError(null);
  };

  /* The name as it will be SAVED. A name is free text now, so edge whitespace is
   * the one thing still normalised — it is invisible, it would become part of the
   * role's identity, and a name differing from another only by a trailing space is
   * one nobody can tell apart in the list or point the model at. Trimmed here
   * rather than on each keystroke: a name may contain spaces, and trimming as the
   * user types would eat the one they just pressed before the next word. */
  const draftName = editor ? editor.draft.name.trim() : "";
  const nameError = editor ? validateAgentTypeName(draftName) : null;
  const duplicateName = editor ? userDefinitions.some((definition) => (
    definition.name === draftName
    && definition.name !== editor.originalName
  )) : false;
  /** Whether the name's verdict is the user's business yet. See `nameChecked`. */
  const showNameError = nameChecked && Boolean(nameError || duplicateName);

  const selectedModelSelection = editor?.draft.modelSelection;
  const selectedExplicitOption = selectedModelSelection?.kind === "explicit"
    ? explicitModelOptions.find((option) => (
        option.providerId === selectedModelSelection.providerId
        && option.modelId === selectedModelSelection.modelId
      )) ?? null
    : null;
  // Same rule the list rows and the host use, rather than a third spelling of
  // it: `explicitModelOptions` is already filtered to enabled providers and
  // models, so a saved selection that finds no option is exactly one the host
  // would refuse to resolve.
  const modelSelectionUnavailable = selectedModelSelection !== undefined
    && !agentDefinitionModelIsAvailable(selectedModelSelection, providers);
  // No option carries the dead binding, so the select shows nothing selected
  // rather than a placeholder row the user could mistake for a choice. The
  // error styling and hint carry the state instead.
  const modelSelectionValue = selectedModelSelection?.kind === "explicit"
    ? selectedExplicitOption?.key ?? ""
    : selectedModelSelection?.kind === "unavailable"
      ? ""
      : "inherit";
  /* Whether a message written into this role's template can carry an image.
   * A bound pair is asked directly; "inherit" is whatever the conversation runs
   * on; a binding that no longer resolves answers no, because there is no model
   * left to say yes for it. */
  const selectedModelReadsImages = selectedModelSelection?.kind === "explicit"
    ? Boolean((() => {
      const provider = providers.find((candidate) => (
        candidate.enabled && candidate.id === selectedModelSelection.providerId
      ));
      const model = provider?.models.find((candidate) => candidate.id === selectedModelSelection.modelId);
      return model && supportsVision(model);
    })())
    : selectedModelSelection?.kind === "inherit" && conversationImageInputSupported;

  /* Names a bound pair that does not currently resolve, WITHOUT printing the
   * `providerId` — it is a random per-installation UUID and reads as a hash.
   * The provider's display name is the actionable half, and it is available
   * whenever the row still exists, which covers the ordinary cases: signed out,
   * disabled, or catalog not fetched yet. Only a provider deleted outright has
   * no name left, and then the model ID stands alone rather than being padded
   * with an identifier the user cannot use. */
  const unavailableBindingLabel = (
    selection: Extract<AgentModelSelection, { kind: "explicit" }>
  ) => {
    const provider = providers.find((candidate) => candidate.id === selection.providerId);
    return provider ? `${provider.name} · ${selection.modelId}` : selection.modelId;
  };

  /* The template page. The body is read the first time the page is opened and
   * only then — a template is big enough that reading it on the chance the page
   * is opened would slow down opening every role — and kept across leaving the
   * page and coming back, since every edit on it is already written through. */
  const editorTemplateId = editor?.draft.templateId ?? "";
  const templatePageOpen = editor !== null && page === "template";
  useEffect(() => {
    if (!templatePageOpen || heldTemplateId.current === editorTemplateId) return;
    if (!editorTemplateId) {
      heldTemplateId.current = "";
      setTemplateBody([]);
      return;
    }
    let abandoned = false;
    setTemplateBody(null);
    void (async () => {
      let contexts: ContextItem[] = [];
      try {
        contexts = await onReadTemplate(editorTemplateId);
      } catch {
        // An unreadable body is an empty one to work from, as it is on a
        // preset's page: the page still has to open, and the next save
        // overwrites whatever is there regardless.
      }
      if (abandoned) return;
      heldTemplateId.current = editorTemplateId;
      setTemplateBody(contexts);
    })();
    return () => { abandoned = true; };
  }, [editorTemplateId, onReadTemplate, templatePageOpen]);

  /* How many messages the role's template holds right now: the body in hand
   * when there is one, otherwise what the host's summary last said. */
  const templateMessageCount = templateBody?.length
    ?? templates.find((summary) => summary.id === editorTemplateId)?.messageCount
    ?? 0;
  const presetTemplateCount = (preset: ConversationPreset) => (preset.templateId
    ? templates.find((summary) => summary.id === preset.templateId)?.messageCount ?? 0
    : 0);

  /* The tools the role can actually call, which is what its template may place.
   * Drawn out of the same catalogue the tools page offers, so a template never
   * holds a card for a tool the role could not be given. */
  const roleToolNames = (editor?.draft.tools ?? inheritedToolNames)
    .filter((name) => selectableToolNameSet.has(name));

  /* Writes a body under the role's template and makes sure the draft cites it:
   * the host mints the id on a first write, and the draft is the only place the
   * role will ever learn it from. */
  const writeRoleTemplate = async (templateId: string, contexts: ContextItem[]) => {
    const savedId = await onWriteTemplate(templateId, contexts);
    heldTemplateId.current = savedId;
    setTemplateBody(contexts);
    if (savedId !== templateId) replaceEditorDraft({ templateId: savedId });
  };

  /* Copies a preset's opening history over the role's own. The body is written
   * at once, as every other edit on this page is; the editor then starts again
   * on it, because the draft it was seeded with is no longer what is stored. */
  const overwriteTemplate = async (preset: ConversationPreset) => {
    setPendingOverwrite(null);
    setOverwriteError(null);
    setOverwriting(true);
    try {
      const contexts = await onReadTemplate(preset.templateId);
      // The host refuses an empty body, and copying nothing over something is
      // not what the arrow promised either.
      if (!contexts.length) {
        throw new Error(t("这份预设的对话模板是空的。", "This preset's conversation template is empty."));
      }
      await writeRoleTemplate(editorTemplateId, contexts);
      setTemplateGeneration((generation) => generation + 1);
    } catch (reason) {
      setOverwriteError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setOverwriting(false);
    }
  };

  /* Replacing an empty template loses nothing, so it just happens; replacing
   * one that already says something asks first. */
  const requestOverwrite = (preset: ConversationPreset) => {
    if (templateMessageCount > 0) setPendingOverwrite(preset);
    else void overwriteTemplate(preset);
  };

  const pageTitles: Record<RoleEditorPage, string> = {
    role: t("角色设置", "Role settings"),
    tools: t("工具", "Tools"),
    template: t("对话模板", "Conversation template")
  };
  const pageBlurbs: Record<RoleEditorPage, string> = {
    role: t(
      "这个角色叫什么、主代理从描述里读到它是干什么的，以及它跑在哪个模型上。关掉窗口会保留草稿，点「保存角色」才会写入。",
      "What this role is called, what the main agent reads about what it is for, and which model it runs on. Closing the window keeps the draft; nothing is written until Save role."
    ),
    tools: t(
      "这个角色拿到的工具面与联网后端，和对话自己的功能页是同一页；联网开关、记忆与工具描述由调用它的对话决定，这里不出现。",
      "The tool surface and web backends this role gets — the same page as the conversation's own features page. Web access, memory and tool descriptions are decided by the conversation that calls it, so they do not appear here."
    ),
    template: t(
      "这个角色的开局历史。模板里的用户消息若恰好含一个 {input}，主代理给的输入会替换到那里；没有或有两个以上时，输入作为最后一条用户消息追加。左下方点一份预设，可以用它的对话模板覆盖这里。",
      "This role's opening history. If the template's user messages contain exactly one {input}, the caller's input replaces it; with none or with two or more, the input is appended as a final user message. Pick a preset at the bottom left to copy its template over this one."
    )
  };

  const nameErrorText = () => {
    if (duplicateName) {
      return t("已有同名的角色。", "A role with this name already exists.");
    }
    switch (nameError) {
      case "required":
        return t("角色名称不能为空。", "A role name is required.");
      case "too_long":
        return t(
          "角色名称最多 {count} 个字符。",
          "A role name can contain at most {count} characters.",
          { count: MAX_AGENT_TYPE_CHARS }
        );
      case "characters":
        return t(
          "角色名称不能包含换行或其它控制字符。",
          "A role name cannot contain newlines or other control characters."
        );
      default:
        return null;
    }
  };

  const saveEditor = () => {
    if (!editor) return;
    // Saving is the moment the name has to be right, so it is also the moment
    // the field is allowed to say so.
    setNameChecked(true);
    const currentDefinition = editor.mode === "edit"
      ? activeDefinitions.find((definition) => (
          editableUserAgentDefinition(definition)
          && definition.name === editor.originalName
        )) ?? null
      : null;
    if (
      editor.mode === "edit"
      && (!currentDefinition || currentDefinition.revision !== editor.originalRevision)
    ) {
      setEditorError(t(
        "此角色已在别处变化。请关闭后重新打开再编辑。",
        "This role changed elsewhere. Close and reopen it before editing."
      ));
      return;
    }
    // `modelSelectionUnavailable` is deliberately NOT a save blocker. It is a
    // state the role arrives in on its own — the provider went away, nobody
    // typed anything wrong — so refusing the save would trap every other edit
    // (a rename, a tool change) behind fixing a model the user may not be able
    // to restore right now. The role stays uncallable until the model is set;
    // that is the enforcement, and it does not need a second one here.
    if (nameError || duplicateName) {
      setEditorError(t("请先修正标记的字段。", "Fix the marked fields before saving."));
      // The marked field is on the role page; a refusal read from any other
      // page would point at something the user cannot see.
      setPage("role");
      return;
    }

    const draft: UserAgentDefinitionDraft = {
      name: draftName,
      description: editor.draft.description,
      modelSelection: editor.draft.modelSelection,
      effort: editor.draft.effort,
      tools: editor.draft.tools === null
        ? null
        : canonicalAgentToolNames(editor.draft.tools),
      disallowedTools: [],
      searchProvider: editor.draft.searchProvider,
      fetchProvider: editor.draft.fetchProvider,
      maxResults: editor.draft.maxResults,
      compressionCutoff: editor.draft.compressionCutoff,
      domainFilter: editor.draft.domainFilter,
      includeDomains: [...editor.draft.includeDomains],
      excludeDomains: [...editor.draft.excludeDomains],
      templateId: editor.draft.templateId
    };

    let saved: AgentDefinition;
    try {
      saved = buildUserAgentDefinition(draft, currentDefinition);
    } catch {
      setEditorError(t(
        "此角色的配置版本已无法安全递增，请新建一个不同名称的角色。",
        "This role's configuration revision can no longer be incremented safely. Create a role with a different name."
      ));
      return;
    }

    if (currentDefinition) {
      onChange(activeDefinitions.map((definition) => (
        definition === currentDefinition ? saved : definition
      )));
    } else {
      onChange([...activeDefinitions, saved]);
    }
    /* The draft has become the role, so it stops being a draft. Both keys go:
     * the one it was opened under, and the one it would be reopened under after
     * a rename — otherwise the new name would reopen on some older draft that
     * happened to share it. */
    editorDrafts.delete(draftKey(listId, editor.originalName));
    editorDrafts.delete(draftKey(listId, draftName));
    setEditor(null);
    setPendingOverwrite(null);
    setEditorError(null);
  };

  /* Deleting a role is undone by retyping it, and the row says whose name is
   * about to go, so the two-step button on the row is the whole confirmation. */
  const deleteDefinition = (definition: AgentDefinition) => {
    if (!editableUserAgentDefinition(definition)) return;
    // A draft of a role that no longer exists would reappear under the name if
    // it were retyped, which reads as the deletion not having taken.
    editorDrafts.delete(draftKey(listId, definition.name));
    onChange(activeDefinitions.filter((candidate) => candidate !== definition));
  };

  /* The row's second line is the model the role actually runs on. `inherit`
   * has no model ID of its own to print — it rides whatever model the calling
   * conversation is on — so it says so rather than inventing one. An
   * `unavailable` role has no ID left to print either: an older build discarded
   * it when the binding was found broken, and nothing writes that state now. */
  const modelLabel = (definition: AgentDefinition) => {
    const selection = definition.modelSelection;
    if (selection.kind === "inherit") {
      return t("跟随对话模型", "Follows the conversation's model");
    }
    if (selection.kind === "unavailable") {
      return t(
        "原模型已不可用，模型看不到这个角色",
        "The bound model is gone; hidden from the model"
      );
    }
    if (!agentDefinitionModelIsAvailable(selection, providers)) {
      return t(
        "{model} · 模型暂时取不到，模型看不到这个角色",
        "{model} · model unavailable for now, hidden from the model",
        { model: selection.modelId }
      );
    }
    return selection.modelId;
  };

  const roleIsUncallable = (definition: AgentDefinition) => (
    !agentDefinitionModelIsAvailable(definition.modelSelection, providers)
  );

  /* Unlike the catalogs this pane cannot write, a role's position IS data: the
   * model reads the roles in order. So dragging one rewrites the real array
   * rather than a stored view order.
   *
   * Only the visible rows are permuted, and each non-visible definition — a
   * tombstone, a read-only project or plugin role — keeps the exact slot it had.
   * Reordering `definitions` directly by name would let a deleted namesake be
   * moved instead of the row that was dragged. */
  const reorderRoles = (
    sourceName: string,
    targetName: string,
    position: "before" | "after"
  ) => {
    const rearranged = reorderItems(
      [...userDefinitions],
      sourceName,
      targetName,
      position,
      (definition) => definition.name
    );
    let cursor = 0;
    onChange(definitions.map((definition) => (
      !definition.deleted && editableUserAgentDefinition(definition)
        ? rearranged[cursor++] ?? definition
        : definition
    )));
  };
  const sort = useCatalogSort({
    listId,
    ids: userDefinitions.map((definition) => definition.name),
    onReorder: reorderRoles
  });

  return (
    <>
      <div className="agent-definitions">
        {userDefinitions.length === 0 && (
          <p className="agent-definitions__empty">{t(
            "还没有角色。主代理只能按名称选择这里定义的角色，看不到也改不了它背后的模型。",
            "No roles yet. The main agent can only pick a role defined here by name, and never sees or changes the model behind it."
          )}</p>
        )}
        <div className="agent-definitions__list" data-sortable-list={listId}>
          {userDefinitions.map((definition) => (
            <CatalogRow
              key={definition.name}
              id={definition.name}
              sort={sort}
              name={definition.name}
              icon={<Bot size={13} aria-hidden="true" />}
              detail={modelLabel(definition)}
              lead={(
                <Switch
                  checked={definition.enabled !== false}
                  label={definition.enabled !== false
                    ? t("角色 {name} 已启用", "Role {name} enabled", { name: definition.name })
                    : t("角色 {name} 已停用", "Role {name} disabled", { name: definition.name })}
                  onChange={(enabled) => onChange(activeDefinitions.map((candidate) => (
                    candidate === definition
                      ? setUserAgentDefinitionEnabled(candidate, enabled)
                      : candidate
                  )))}
                />
              )}
              badge={roleIsUncallable(definition)
                ? <em className="catalog-row__badge catalog-row__badge--warning">{t("模型不可用", "No model")}</em>
                : undefined}
              actions={(
                <>
                  <IconButton
                    label={t("设置角色 {name}", "Configure role {name}", { name: definition.name })}
                    onClick={() => beginEdit(definition)}
                  >
                    <Settings2 size={13} />
                  </IconButton>
                  <ConfirmDeleteButton
                    label={t("删除角色 {name}", "Delete role {name}", { name: definition.name })}
                    confirmLabel={t("确认删除角色 {name}", "Confirm deleting role {name}", { name: definition.name })}
                    onDelete={() => deleteDefinition(definition)}
                  />
                </>
              )}
            />
          ))}
          {/* Always one row below the last role, so "add another" is where the
            * list ends rather than in a header the eye has already left. */}
          <button
            type="button"
            className="agent-definition-add"
            onClick={beginCreate}
            disabled={activeDefinitions.length >= 256}
            aria-label={t("新建角色", "New role")}
          >
            <Plus size={15} />
          </button>
        </div>
      </div>

      {editor && (
        <Dialog
          /* Named after the role, the way a preset's window is named after the
             preset. The name as it was OPENED, not as it is being typed: the
             title is how the user knows which role this window is, and it
             should not change under them mid-rename. */
          title={editor.mode === "create"
            ? t("新建角色", "New role")
            : editor.originalName ?? t("角色设置", "Role settings")}
          width="1040px"
          bodyClassName="dialog__body--flush"
          // Deliberately not dismissible: a stray click on the backdrop would
          // discard an in-progress role without asking. The header's own close
          // button is unaffected — that one is a decision, not a slip, and it
          // keeps the draft rather than throwing it away.
          dismissible={false}
          onClose={closeEditor}
        >
          <div className="conversation-settings agent-definition-editor">
            <nav
              className="conversation-settings__nav settings-nav agent-definition-editor__nav"
              aria-label={t("角色设置分类", "Role settings categories")}
            >
              <div className="conversation-settings__nav-items">
                {ROLE_EDITOR_PAGES.map((item) => {
                  const Icon = item.icon;
                  return (
                    <button
                      type="button"
                      key={item.id}
                      aria-current={page === item.id || undefined}
                      className={page === item.id
                        ? "settings-nav__item settings-nav__item--active"
                        : "settings-nav__item"}
                      onClick={() => setPage(item.id)}
                    >
                      <Icon size={14} aria-hidden="true" />
                      <span>{pageTitles[item.id]}</span>
                      {item.id === "template" && (
                        <small className="conversation-settings__nav-count">{templateMessageCount}</small>
                      )}
                    </button>
                  );
                })}
              </div>

              {/* Only while the template is on screen: copying a preset's body
                  over the role's is an act on that page, and it is read against
                  the body it would replace. One line per preset, the whole line
                  the target, and the arrow pointing at where the body goes. */}
              {page === "template" && (
                <div className="agent-definition-editor__presets">
                  <span className="agent-definition-editor__presets-label">
                    {t("从预设覆盖", "Copy from a preset")}
                  </span>
                  <div className="agent-definition-editor__presets-list">
                    {presets.map((preset) => {
                      const name = preset.name || t("未命名预设", "Untitled preset");
                      const empty = presetTemplateCount(preset) === 0;
                      return (
                        <button
                          type="button"
                          key={preset.id}
                          className="agent-definition-editor__preset"
                          aria-label={t(
                            "用预设 {name} 的对话模板覆盖",
                            "Copy preset {name}'s template over this one",
                            { name }
                          )}
                          title={empty
                            ? t("这份预设没有对话模板。", "This preset has no conversation template.")
                            : name}
                          disabled={empty || overwriting}
                          onClick={() => requestOverwrite(preset)}
                        >
                          <span>{name}</span>
                          <ArrowRight size={12} aria-hidden="true" />
                        </button>
                      );
                    })}
                    {presets.length === 0 && (
                      <p className="agent-definition-editor__presets-empty">
                        {t("还没有对话预设。", "No conversation presets yet.")}
                      </p>
                    )}
                  </div>
                  {overwriteError && (
                    <p className="agent-definition-editor__error" role="alert">{overwriteError}</p>
                  )}
                </div>
              )}

              {/* Saving belongs to the whole window rather than to any one page,
                  so it is the one thing under the list — the corner a preset's
                  window keeps its own save in. */}
              <div className="conversation-settings__nav-footer">
                <div className="conversation-settings__preset-actions">
                  <button
                    type="button"
                    className="text-button"
                    // Not disabled on a blank name. A save greyed out with no
                    // explanation is a puzzle; pressing it and being told what
                    // is missing is an answer.
                    onClick={saveEditor}
                  >{t("保存角色", "Save role")}</button>
                </div>
                {editorError && (
                  <p className="agent-definition-editor__error" role="alert">{editorError}</p>
                )}
              </div>
            </nav>

            <div className="conversation-settings__page">
              <header className="conversation-settings__page-header">
                <p>{pageBlurbs[page]}</p>
              </header>
              {/* The template page is a timeline, which brings its own scroller
                  and its own edges, so it gets the body flush — as it does on a
                  preset's page. */}
              <div className={page === "template"
                ? "conversation-settings__page-body conversation-settings__page-body--flush"
                : "conversation-settings__page-body"}>
                <div className={page === "template"
                  ? "conversation-settings__page-stack conversation-settings__page-stack--flush"
                  : "conversation-settings__page-stack"}>
                  {page === "role" && (
                    <>
                      <section className="conversation-settings__field agent-definition-editor__identity">
                        {/* No standing hint under it. A role name is free text, so
                          * there is no shape to teach — the only thing left to say
                          * about it is that a particular one was refused, and that
                          * is what the line carries when there is one. */}
                        <Field
                          label={t("角色名称", "Role name")}
                          hint={showNameError ? nameErrorText() ?? undefined : undefined}
                          hintIsError
                        >
                          <input
                            className={`input${showNameError ? " input--error" : ""}`}
                            value={editor.draft.name}
                            aria-label={t("角色名称", "Role name")}
                            aria-invalid={showNameError}
                            onChange={(event) => replaceEditorDraft({ name: event.target.value })}
                            autoComplete="off"
                            autoFocus
                          />
                        </Field>

                        {/* Deliberately no character counter and no maximum. This
                          * is the one field on this screen the model actually reads
                          * as prose, and a budget shown next to it would push users
                          * to write a label where a sentence belongs. The host
                          * writes it through verbatim. */}
                        <Field
                          label={t("子代理描述", "Subagent description")}
                          hint={t(
                            "告诉主代理这个角色是干什么的。会拼进本对话「子代理」/「工作流」工具的描述里，每个角色一行；留空则这个角色不出现在那份说明里。",
                            "Tells the main agent what this role is for. It is appended to this conversation's Subagent / Workflow tool description, one line per role; leave it empty and this role contributes no line."
                          )}
                        >
                          <textarea
                            className="input"
                            rows={4}
                            value={editor.draft.description}
                            aria-label={t("子代理描述", "Subagent description")}
                            onChange={(event) => replaceEditorDraft({ description: event.target.value })}
                            placeholder={t(
                              "对抗式审查：负责证伪既有结论、挑错，不产出主线方案。",
                              "Adversarial review: refutes existing conclusions and finds faults; does not produce the main proposal."
                            )}
                          />
                        </Field>

                        {editor.mode === "edit" && editor.originalName !== draftName && (
                          <p className="agent-definition-editor__warning">{t(
                            "重命名会创建新的可信身份，正在运行的旧名称调用不会自动跟随。",
                            "Renaming creates a new trusted identity; a run already using the old name does not follow it."
                          )}</p>
                        )}
                      </section>

                      {/* A row rather than a stacked form field: what it is called
                        * and what it means on the left, the one control that sets
                        * it on the right — the shape the features page uses for
                        * the same kind of question. */}
                      <section className="conversation-settings__field">
                        <div className="agent-definition-editor__rows field-row">
                          <Field
                            label={t("执行模型", "Execution model")}
                            hint={modelSelectionUnavailable
                              ? selectedModelSelection?.kind === "explicit"
                                ? t(
                                    "这个模型现在取不到——提供商还没拉取模型、被停用，或者这一行已经不在了。角色暂时不能被调用，但绑定会一直留着，模型回来就自动恢复。",
                                    "This model cannot be resolved right now — the provider has not fetched its models, is disabled, or the row is gone. The role cannot be called for now, but the binding is kept and recovers by itself once the model is back."
                                  )
                                : t(
                                    "这条绑定是旧版本记下的「已失效」，模型 ID 当时就被丢掉了，找不回来；请选择跟随对话或另一个可用模型。",
                                    "An older build recorded this binding as broken and discarded the model ID at the time, so it cannot be recovered. Choose the conversation's model or another available one."
                                  )
                              : t(
                                  "只列出已启用提供商中的模型；保存精确 provider/model ID。",
                                  "Only models from enabled providers are listed; exact provider/model IDs are saved."
                                )}
                          >
                            <select
                              className={`input${modelSelectionUnavailable ? " input--error" : ""}`}
                              value={modelSelectionValue}
                              aria-label={t("执行模型", "Execution model")}
                              aria-invalid={modelSelectionUnavailable}
                              onChange={(event) => {
                                if (event.target.value === "inherit") {
                                  replaceEditorDraft({ modelSelection: { kind: "inherit" } });
                                  return;
                                }
                                const option = explicitModelOptions.find((candidate) => (
                                  candidate.key === event.target.value
                                ));
                                if (!option) return;
                                replaceEditorDraft({
                                  modelSelection: {
                                    kind: "explicit",
                                    providerId: option.providerId,
                                    modelId: option.modelId
                                  }
                                });
                              }}
                            >
                              {/* `hidden` keeps this out of the dropdown list: it
                                * exists only so the closed select does not fall
                                * through to the first option and display a working
                                * model the role does not have.
                                *
                                * A binding whose pair is still recorded but no longer
                                * offered — its model has not been fetched, or the
                                * provider is disabled — names the model the user
                                * actually chose. It must never print the raw
                                * `providerId`, which is a random per-installation
                                * UUID and reads as a hash; the provider's own name
                                * is the part a person can act on, and when its row
                                * is gone entirely there is no name to give, so the
                                * model ID stands alone. */}
                              {modelSelectionUnavailable && (
                                <option value="" disabled hidden>
                                  {selectedModelSelection?.kind === "explicit"
                                    ? t(
                                        "{model}（不可用）",
                                        "{model} (unavailable)",
                                        { model: unavailableBindingLabel(selectedModelSelection) }
                                      )
                                    : t("请选择执行模型", "Choose an execution model")}
                                </option>
                              )}
                              <option value="inherit">{t("跟随对话模型", "Follow the conversation's model")}</option>
                              {explicitModelOptions.map((option) => (
                                <option key={option.key} value={option.key}>
                                  {option.providerName} · {option.modelId}
                                </option>
                              ))}
                            </select>
                          </Field>

                          <Field
                            label={t("推理强度", "Reasoning effort")}
                            hint={t(
                              "留空表示跟随对话的推理强度。",
                              "Leave unset to follow the conversation's reasoning effort."
                            )}
                          >
                            <select
                              className="input"
                              value={editor.draft.effort ?? "inherit"}
                              aria-label={t("推理强度", "Reasoning effort")}
                              onChange={(event) => replaceEditorDraft({
                                effort: event.target.value === "inherit"
                                  ? null
                                  : (event.target.value as ReasoningEffort)
                              })}
                            >
                              <option value="inherit">{t("跟随对话", "Follow the conversation")}</option>
                              {AGENT_EFFORTS.map((effort) => (
                                <option key={effort} value={effort}>{effort}</option>
                              ))}
                            </select>
                          </Field>
                        </div>
                      </section>
                    </>
                  )}

                  {/* The conversation's own features page, not a copy of it: a
                      row added or reworded there lands here too. What a role
                      leaves out is exactly what it has no field for — the web
                      access switch (whether to reach the web at all is the
                      caller's decision), the memory tiers (derived from the
                      caller's switches and the role's memory binding, never from
                      a tool list), the app-data path, and the tool-description
                      profile. What it adds is a "follow the conversation" answer
                      on every row that has a caller to follow. */}
                  {page === "tools" && (
                    <FeaturesPage
                      picker={{
                        tools: selectableTools as ToolDescriptor[],
                        // While inheriting, the picker shows the conversation's
                        // own set rather than an empty list: that IS what this
                        // role will get, and drawing it as "nothing selected"
                        // would misread as broken.
                        enabledTools: editor.draft.tools ?? inheritedToolNames,
                        onChange: (tools) => replaceEditorDraft({ tools }),
                        expansionKey: editor.originalName ?? "new-role"
                      }}
                      pickerSummary={editor.draft.tools === null
                        ? t(
                            "跟随对话设置：本对话启用哪些，这个角色就有哪些",
                            "Following the conversation: this role gets whatever the conversation enables"
                          )
                        : t(
                            "{enabled} / {total} 个已选",
                            "{enabled} / {total} selected",
                            {
                              enabled: visibleSelectedCount(editor.draft.tools),
                              total: selectableToolNames.filter((name) => !isPreviewLifecycleToolName(name)).length
                            }
                          )}
                      web={{
                        inheritOption: true,
                        value: {
                          provider: editor.draft.searchProvider,
                          fetchProvider: editor.draft.fetchProvider,
                          maxResults: editor.draft.maxResults,
                          compressionCutoff: editor.draft.compressionCutoff,
                          domainFilter: editor.draft.domainFilter,
                          includeDomains: editor.draft.includeDomains,
                          excludeDomains: editor.draft.excludeDomains
                        },
                        onChange: ({ provider, ...rest }) => replaceEditorDraft(
                          provider === undefined ? rest : { ...rest, searchProvider: provider }
                        ),
                        webSearchAssets
                      }}
                    />
                  )}

                  {/* The role's own message queue, edited on the same surface a
                      preset's is. Each edit is written back as it lands, and the
                      first write mints the id the draft then carries. */}
                  {page === "template" && (
                    <ConversationTemplateEditor
                      key={templateGeneration}
                      templateId={editorTemplateId}
                      contexts={templateBody}
                      tools={selectableTools as ToolDescriptor[]}
                      enabledTools={roleToolNames}
                      imageInputSupported={selectedModelReadsImages}
                      autosave
                      /* A role that follows the conversation's set is widened
                         by widening that set; a role with a list of its own is
                         widened on its own list, and only on Save. */
                      onEnableTools={editor.draft.tools === null
                        ? onEnableTools
                        : (names) => replaceEditorDraft({
                            tools: [...new Set([...(editor.draft.tools ?? []), ...names])]
                          })}
                      onSave={(contexts) => writeRoleTemplate(editorTemplateId, contexts)}
                    />
                  )}
                </div>
              </div>
            </div>
          </div>
        </Dialog>
      )}

      {pendingOverwrite && (
        <Dialog
          title={t("覆盖对话模板？", "Overwrite the conversation template?")}
          description={t(
            "这个角色的对话模板里已有 {count} 条消息，会被预设「{name}」的对话模板整个替换，替换后无法撤销。",
            "This role's template already holds {count} messages. They will all be replaced by the template of preset \u201c{name}\u201d, and this cannot be undone.",
            {
              count: templateMessageCount,
              name: pendingOverwrite.name || t("未命名预设", "Untitled preset")
            }
          )}
          onClose={() => setPendingOverwrite(null)}
          footer={(
            <>
              <button
                type="button"
                className="button button--secondary"
                onClick={() => setPendingOverwrite(null)}
              >{t("取消", "Cancel")}</button>
              <button
                type="button"
                className="button button--danger"
                onClick={() => void overwriteTemplate(pendingOverwrite)}
              >{t("覆盖", "Overwrite")}</button>
            </>
          )}
        />
      )}
    </>
  );
}
