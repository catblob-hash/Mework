import { Bot, Plus, Settings2 } from "lucide-react";
import { useMemo, useState } from "react";
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
import { ConversationTemplateWindow } from "./ConversationTemplateWindow";
import { DocsLink } from "./DocsLink";
import { FetchProviderField } from "./FetchProviderField";
import { SearchDomainFilterRow } from "./SearchDomainFilterRow";
import { SearchProviderField } from "./SearchProviderField";
import { SearchResultShapingFields } from "./SearchResultShapingFields";
import { ToolSelectionGroups } from "./ToolSelectionGroups";
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
   * "inherit" will run on, and so the answer its template window needs. */
  conversationImageInputSupported?: boolean;
  /** The search-provider catalogue, so a role can pick its own backend. */
  webSearchAssets: WebSearchAssets;
  /** Every stored template, for the message count a role's row reports. */
  templates: ConversationTemplateSummary[];
  /** Drawn read-only in the template window, beside the role's own body. */
  presets: readonly ConversationPreset[];
  onReadTemplate: (templateId: string) => Promise<ContextItem[]>;
  /** Writes a body and resolves with the id it landed under, minting when empty. */
  onWriteTemplate: (templateId: string, contexts: ContextItem[]) => Promise<string>;
  /** Widens the conversation's enabled set when a template calls a tool it lacks. */
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
  /* The role's template, open in a window over the role editor. Its own state
   * because the body is written straight to the host: the role editor's draft
   * holds only the id, and only once there is one. */
  const [templateWindowOpen, setTemplateWindowOpen] = useState(false);
  /* The role's own domain lists, open in a window over the role editor. Its own
   * state for the same reason the template window's is: the row that opens it
   * is one field of the role, not a second editor. */
  const [domainWindowOpen, setDomainWindowOpen] = useState(false);
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
   * disabled while visible switches are still off. */
  const visibleSelectedCount = (names: readonly string[]) =>
    names.filter((name) => selectableToolNameSet.has(name)).length;
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

  const beginCreate = () => {
    const key = draftKey(listId, null);
    const remembered = editorDrafts.get(key);
    setEditor(remembered ?? {
      mode: "create",
      originalName: null,
      originalRevision: null,
      draft: blankDraft()
    });
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
    setTemplateWindowOpen(false);
    setDomainWindowOpen(false);
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
    setTemplateWindowOpen(false);
    setDomainWindowOpen(false);
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
          title={editor.mode === "create"
            ? t("新建角色", "New role")
            : t("角色设置", "Role settings")}
          width="620px"
          // Deliberately not dismissible: a stray click on the backdrop would
          // discard an in-progress role without asking. The header's own close
          // button is unaffected — that one is a decision, not a slip.
          dismissible={false}
          onClose={closeEditor}
          footer={(
            <>
              <button
                type="button"
                className="button button--secondary"
                onClick={closeEditor}
              >
                {/* Not "Cancel": closing keeps the draft, and a button that
                    says it cancels while the edit survives is a lie about what
                    just happened. The parenthesis is the only place the user is
                    told the draft outlives the dialog, and it also keeps this
                    button's name distinct from the header's own close. Save is
                    still the only thing that writes to the role. */}
                {t("关闭（保留草稿）", "Close (keep draft)")}
              </button>
              <button
                type="button"
                className="button button--primary"
                // Not disabled on a blank name. A dialog that opens with its
                // save greyed out and no explanation is a puzzle; pressing it
                // and being told what is missing is an answer.
                onClick={saveEditor}
              >
                {t("保存", "Save")}
              </button>
            </>
          )}
        >
          <div className="agent-definition-editor">
            {/* No standing hint under it any more. A role name is free text, so
              * there is no shape to teach — the only thing left to say about it
              * is that a particular one was refused, and that is what the line
              * carries when there is one. */}
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

            {/* Deliberately no character counter and no maximum. This is the one
              * field on this screen the model actually reads as prose, and a
              * budget shown next to it would push users to write a label where a
              * sentence belongs. The host writes it through verbatim. */}
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

            {/* Everything below is a row rather than a stacked form field: what
              * it is called and what it means on the left, the one control that
              * sets it on the right. It is the shape the conversation's own
              * settings page uses for the same kind of question, and it is the
              * reason these four share a wrapper — `.field-row` is the recipe,
              * and the column keeps one right edge whether the control is a
              * picker or a way in. */}
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
                  {/* `hidden` keeps this out of the dropdown list: it exists
                    * only so the closed select does not fall through to the
                    * first option and display a working model the role does
                    * not have.
                    *
                    * A binding whose pair is still recorded but no longer offered — its model
                    * has not been fetched, or the provider is disabled — names the model the
                    * user actually chose. It must never print the raw `providerId`, which is a
                    * random per-installation UUID and reads as a hash; the provider's own name
                    * is the part a person can act on, and when its row is gone entirely there
                    * is no name to give, so the model ID stands alone. */}
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

              {/* The same four web questions the conversation's own settings
                  page asks, in the same order and through the same components,
                  with one answer added to each: follow the caller. A role that
                  answered fewer of them than a conversation does would be a
                  second, quietly narrower surface for the same subject. */}
              <SearchProviderField
                value={editor.draft.searchProvider}
                onChange={(searchProvider) => replaceEditorDraft({ searchProvider })}
                webSearchAssets={webSearchAssets}
                inheritOption
                hint={t(
                  "这个角色的「联网搜索」用哪个后端。留在「跟随对话设置」就用调用方对话的选择。选「原生」时用的是这个角色自己的模型——它所在的协议家族如果不支持模型自带搜索，检索会以可修复的错误失败，而不会悄悄换一家。",
                  "Which backend this role's web search goes through. Leave it on \"follow the conversation\" to use the caller's choice. \"Native\" means this role's OWN model — if its protocol family has no built-in search, the search fails with a fixable error rather than quietly switching backends."
                )}
              />

              <FetchProviderField
                value={editor.draft.fetchProvider}
                onChange={(fetchProvider) => replaceEditorDraft({ fetchProvider })}
                webSearchAssets={webSearchAssets}
                inheritOption
                hint={t(
                  "这个角色抓取网页用哪个后端，和上面的搜索后端各答各的。留在「跟随对话设置」就用调用方对话的选择。这里选什么都不能让一个关掉联网的对话联网——联网与否由对话决定，这里只决定由谁去抓。",
                  "Which backend this role fetches pages with, answered separately from the search backend above. Leave it on \"follow the conversation\" to use the caller's choice. Nothing chosen here can put a conversation that is offline back on the network: whether to reach the web at all is the conversation's decision, and this one is only who does the fetching."
                )}
              />

              {/* The role's own result shaping. Unlike the backends above these
                  do not offer "follow the conversation": 0 already means "no
                  cap", so there is no value left over to spell inheritance
                  with, and a role therefore always answers for itself. */}
              <SearchResultShapingFields
                maxResults={editor.draft.maxResults}
                compressionCutoff={editor.draft.compressionCutoff}
                onChangeMaxResults={(maxResults) => replaceEditorDraft({ maxResults })}
                onChangeCompressionCutoff={(compressionCutoff) => replaceEditorDraft({
                  compressionCutoff
                })}
              />

              <SearchDomainFilterRow
                mode={editor.draft.domainFilter}
                includeDomains={editor.draft.includeDomains}
                excludeDomains={editor.draft.excludeDomains}
                inheritOption
                windowOpen={domainWindowOpen}
                onOpenWindow={() => setDomainWindowOpen(true)}
                onCloseWindow={() => setDomainWindowOpen(false)}
                onChangeMode={(domainFilter) => replaceEditorDraft({ domainFilter })}
                onChangeRules={(list, rules) => replaceEditorDraft(
                  list === "include" ? { includeDomains: rules } : { excludeDomains: rules }
                )}
                hint={t(
                  "这个角色怎么按域名筛检索结果。留在「跟随对话设置」就连名单一起用调用方对话的；一旦自己选了模式，用的就只有下面这两份名单，不再叠加对话的。",
                  "How this role filters results by domain. Left on \"follow the conversation\" it uses the caller's mode AND the caller's lists; naming a mode of its own switches to these two lists alone, which are not layered onto the conversation's."
                )}
              />

              {/* Deliberately NOT a `Field`: that renders a `<label>`, which would
                  make this whole row — the line under the name included — a click
                  target for the one labelable thing inside it. A select could
                  absorb that; a button that opens a window cannot. */}
              <div className="field">
                <span className="field__label">{t("对话模板", "Conversation template")}</span>
                {/* A button rather than a picker: the role owns its template, so
                    there is nothing to choose between — only a body to write. The
                    window it opens draws the presets' templates alongside, so what
                    this role opens with can be written next to what it will run
                    beside. */}
                <button
                  type="button"
                  className="button button--secondary"
                  onClick={() => setTemplateWindowOpen(true)}
                >{t("编辑对话模板", "Edit conversation template")}</button>
                <span className="field__hint">{t(
                  "这个角色的开局历史。模板里的用户消息若恰好含一个 {input}，主代理给的输入会替换到那里；没有或有两个以上时，输入作为最后一条用户消息追加。",
                  "This role's opening history. If the template's user messages contain exactly one {input}, the caller's input replaces it; with none or with two or more, the input is appended as a final user message."
                )}</span>
              </div>
            </div>

            <section className="agent-definition-editor__tools">
              <div className="settings-section__heading settings-section__heading--split">
                <div><span><strong>{t("启用工具", "Enabled tools")}</strong><small>{editor.draft.tools === null
                  ? t(
                      "跟随对话设置：本对话启用哪些，这个角色就有哪些",
                      "Following the conversation: this role gets whatever the conversation enables"
                    )
                  : t(
                      "{enabled} / {total} 个已选",
                      "{enabled} / {total} selected",
                      {
                        enabled: visibleSelectedCount(editor.draft.tools),
                        total: selectableToolNames.length
                      }
                    )}</small></span></div>
                {/* The same way out the conversation's own tool picker ends
                    its heading with. The three bulk buttons that used to sit
                    here are gone: every group below now carries its own pair,
                    which says which tools are being moved instead of acting on
                    the whole catalogue at once. */}
                <DocsLink page="features" />
              </div>
              <ToolSelectionGroups
                tools={selectableTools as ToolDescriptor[]}
                // While inheriting, the picker shows the conversation's own set
                // rather than an empty list: that IS what this role will get,
                // and drawing it as "nothing selected" would misread as broken.
                enabledTools={editor.draft.tools ?? inheritedToolNames}
                onChange={(tools) => replaceEditorDraft({ tools })}
                expansionKey={editor.originalName ?? "new-role"}
              />
            </section>

            {editor.mode === "edit" && editor.originalName !== draftName && (
              <p className="agent-definition-editor__warning">{t(
                "重命名会创建新的可信身份，正在运行的旧名称调用不会自动跟随。",
                "Renaming creates a new trusted identity; a run already using the old name does not follow it."
              )}</p>
            )}
            {editorError && (
              <p className="agent-definition-editor__error" role="alert">{editorError}</p>
            )}
          </div>
        </Dialog>
      )}

      {/* Opened over the role editor rather than replacing it: the template is
          one field of the role being edited, and coming back to the rest of that
          role is the whole point of it being a window. */}
      {editor && templateWindowOpen && (
        <ConversationTemplateWindow
          ownTemplateId={editor.draft.templateId ?? ""}
          presets={presets.map((preset) => ({
            id: preset.id,
            name: preset.name,
            templateId: preset.templateId
          }))}
          templates={templates}
          tools={[...tools]}
          /* The conversation's set, not the role's allowlist: enabling a tool
             from here flips a switch on the page behind, and that page is the
             conversation's tool picker. */
          enabledTools={conversationEnabledTools}
          imageInputSupported={selectedModelReadsImages}
          onReadTemplate={onReadTemplate}
          onSaveOwnTemplate={async (contexts) => {
            const savedId = await onWriteTemplate(editor.draft.templateId ?? "", contexts);
            // The role only ever learns its template's id here, so the draft has
            // to take it before the window can be closed on a first save.
            if (savedId !== editor.draft.templateId) replaceEditorDraft({ templateId: savedId });
            return savedId;
          }}
          onEnableTools={onEnableTools}
          onClose={() => setTemplateWindowOpen(false)}
        />
      )}
    </>
  );
}
