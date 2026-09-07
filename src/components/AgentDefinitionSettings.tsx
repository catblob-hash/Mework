import { Bot, Plus, Settings2, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useI18n } from "../i18n";
import {
  AGENT_TOOL_WILDCARD,
  MAX_AGENT_TYPE_CHARS,
  agentDefinitionModelIsAvailable,
  buildUserAgentDefinition,
  canonicalAgentToolNames,
  editableUserAgentDefinition,
  userAgentDefinitionDraft,
  validateAgentTypeSlug
} from "../lib/agentDefinitions";
import type { UserAgentDefinitionDraft } from "../lib/agentDefinitions";
import type {
  AgentDefinition,
  AgentModelSelection,
  ApiProvider,
  ReasoningEffort,
  SearchProviderSelection,
  ToolDescriptor,
  WebSearchAssets
} from "../types";
import { Dialog, Field, IconButton } from "./Common";
import { SearchProviderField } from "./SearchProviderField";
import { ToolSelectionGroups } from "./ToolSelectionGroups";
import "./AgentDefinitionSettings.css";

interface AgentDefinitionSettingsProps {
  definitions: readonly AgentDefinition[];
  providers: readonly ApiProvider[];
  /** The whole trusted tool catalogue. A role selects out of it without being
   * bounded by what the conversation enabled. */
  tools: readonly ToolDescriptor[];
  /** Names the conversation itself enables — the set "inherit" restores, and
   * the set the picker shows while a role has no allowlist of its own. */
  conversationEnabledTools: readonly string[];
  /** The search-provider catalogue, so a role can pick its own backend. */
  webSearchAssets: WebSearchAssets;
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
  };
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
  providers,
  tools,
  conversationEnabledTools,
  webSearchAssets,
  onChange
}: AgentDefinitionSettingsProps) {
  const { t } = useI18n();
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [deleteCandidate, setDeleteCandidate] = useState<AgentDefinition | null>(null);

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

  const beginCreate = () => {
    setEditor({
      mode: "create",
      originalName: null,
      originalRevision: null,
      draft: {
        name: "",
        description: "",
        modelSelection: { kind: "inherit" },
        effort: null,
        tools: null,
        searchProvider: null
      }
    });
    setEditorError(null);
  };

  const beginEdit = (definition: AgentDefinition) => {
    if (!editableUserAgentDefinition(definition)) return;
    const draft = userAgentDefinitionDraft(definition);
    setEditor({
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
        searchProvider: draft.searchProvider
      }
    });
    setEditorError(null);
  };

  const replaceEditorDraft = (change: Partial<EditorState["draft"]>) => {
    setEditor((current) => current ? {
      ...current,
      draft: { ...current.draft, ...change }
    } : current);
    setEditorError(null);
  };

  const slugError = editor ? validateAgentTypeSlug(editor.draft.name) : null;
  const duplicateName = editor ? userDefinitions.some((definition) => (
    definition.name === editor.draft.name
    && definition.name !== editor.originalName
  )) : false;

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

  const slugErrorText = () => {
    if (duplicateName) {
      return t("已有同名的角色。", "A role with this name already exists.");
    }
    switch (slugError) {
      case "required":
        return t("角色名称不能为空。", "A role name is required.");
      case "too_long":
        return t(
          "角色名称最多 {count} 个字符。",
          "A role name can contain at most {count} characters.",
          { count: MAX_AGENT_TYPE_CHARS }
        );
      case "first_character":
        return t(
          "角色名称必须以小写英文字母开头。",
          "A role name must begin with a lowercase ASCII letter."
        );
      case "characters":
        return t(
          "角色名称只能包含小写英文字母、数字、下划线和连字符。",
          "A role name can contain only lowercase ASCII letters, digits, underscores, and hyphens."
        );
      default:
        return null;
    }
  };

  const saveEditor = () => {
    if (!editor) return;
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
    if (slugError || duplicateName) {
      setEditorError(t("请先修正标记的字段。", "Fix the marked fields before saving."));
      return;
    }

    const draft: UserAgentDefinitionDraft = {
      name: editor.draft.name,
      description: editor.draft.description,
      modelSelection: editor.draft.modelSelection,
      effort: editor.draft.effort,
      tools: editor.draft.tools === null
        ? null
        : canonicalAgentToolNames(editor.draft.tools),
      disallowedTools: [],
      searchProvider: editor.draft.searchProvider
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
    setEditor(null);
    setEditorError(null);
  };

  const deleteDefinition = () => {
    if (!deleteCandidate || !editableUserAgentDefinition(deleteCandidate)) return;
    onChange(activeDefinitions.filter((definition) => definition !== deleteCandidate));
    setDeleteCandidate(null);
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

  return (
    <>
      <div className="agent-definitions">
        {userDefinitions.length === 0 && (
          <p className="agent-definitions__empty">{t(
            "还没有角色。主代理只能按名称选择这里定义的角色，看不到也改不了它背后的模型。",
            "No roles yet. The main agent can only pick a role defined here by name, and never sees or changes the model behind it."
          )}</p>
        )}
        <div className="agent-definitions__list">
          {userDefinitions.map((definition) => (
            <div className="agent-definition-row" key={definition.name}>
              <Bot size={15} aria-hidden="true" />
              <button
                type="button"
                className="agent-definition-row__copy"
                onClick={() => beginEdit(definition)}
                aria-label={t("打开角色 {name}", "Open role {name}", { name: definition.name })}
              >
                <span><strong>{definition.name}</strong></span>
                <small className={roleIsUncallable(definition)
                  ? "agent-definition-row__model agent-definition-row__model--unavailable"
                  : "agent-definition-row__model"}
                >{modelLabel(definition)}</small>
              </button>
              <IconButton
                className="agent-definition-row__action"
                label={t("设置角色 {name}", "Configure role {name}", { name: definition.name })}
                onClick={() => beginEdit(definition)}
              >
                <Settings2 size={13} />
              </IconButton>
              <IconButton
                className="agent-definition-row__action agent-definition-row__action--danger"
                label={t("删除角色 {name}", "Delete role {name}", { name: definition.name })}
                onClick={() => setDeleteCandidate(definition)}
              >
                <Trash2 size={13} />
              </IconButton>
            </div>
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
          description={t(
            "角色回答三件事：跑在哪个模型上，能用哪些工具，以及对主代理怎么介绍自己。它没有自己的系统提示词——命名子代理和普通子代理一样使用本对话的提示词。",
            "A role answers three questions: which model it runs on, which tools it may use, and how it introduces itself to the main agent. It has no system prompt of its own — a named child uses this conversation's prompt, exactly like an ordinary child."
          )}
          width="620px"
          // Deliberately not dismissible: a stray click on the backdrop would
          // discard an in-progress role without asking.
          dismissible={false}
          onClose={() => setEditor(null)}
          footer={(
            <>
              <button type="button" className="button button--secondary" onClick={() => setEditor(null)}>
                {t("取消", "Cancel")}
              </button>
              <button
                type="button"
                className="button button--primary"
                // An unnamed role cannot be saved: the name IS the handle the
                // model selects it by, so a blank one would be unaddressable.
                disabled={Boolean(slugError) || duplicateName}
                onClick={saveEditor}
              >
                {t("保存角色", "Save role")}
              </button>
            </>
          )}
        >
          <div className="agent-definition-editor">
            <Field
              label={t("角色名称", "Role name")}
              hint={slugErrorText() ?? t(
                "主代理按这个名称选择角色。1–64 个字符，以小写英文字母开头，仅可使用 a-z、0-9、_ 和 -。",
                "The main agent selects a role by this name. 1–64 characters, beginning with a lowercase ASCII letter, using only a-z, 0-9, _ and -."
              )}
            >
              <input
                className={`input input--code${slugError || duplicateName ? " input--error" : ""}`}
                value={editor.draft.name}
                aria-label={t("角色名称", "Role name")}
                aria-invalid={Boolean(slugError || duplicateName)}
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

            <div className="agent-definition-editor__grid">
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
            </div>

            {/* Outside the two-column grid on purpose: its hint is a paragraph,
              * and a paragraph in a half-width cell wraps into a column the eye
              * has to hunt through. */}
            <SearchProviderField
              value={editor.draft.searchProvider}
              onChange={(searchProvider) => replaceEditorDraft({ searchProvider })}
              webSearchAssets={webSearchAssets}
              inheritOption
              hint={t(
                "这个角色的「联网搜索」用哪个后端。留在「跟随对话设置」就用调用方对话的选择。选「原生」时用的是这个角色自己的模型——它所在的协议家族如果不支持模型自带搜索，检索会以可修复的错误失败，而不会悄悄换一家。抓取网页那条腿不在这里：它没有原生可选，一律用全局设置里的抓取提供商。",
                "Which backend this role's web search goes through. Leave it on \"follow the conversation\" to use the caller's choice. \"Native\" means this role's OWN model — if its protocol family has no built-in search, the search fails with a fixable error rather than quietly switching backends. Fetching pages is not chosen here: that leg has no native option and always uses the global fetch provider."
              )}
            />

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
                <div className="tool-bulk-actions">
                  <button
                    type="button"
                    className="text-button"
                    disabled={editor.draft.tools !== null
                      && visibleSelectedCount(editor.draft.tools) === selectableToolNames.length}
                    onClick={() => replaceEditorDraft({ tools: [...selectableToolNames] })}
                  >{t("全部启用", "Enable all")}</button>
                  <button
                    type="button"
                    className="text-button"
                    disabled={editor.draft.tools !== null
                      && visibleSelectedCount(editor.draft.tools) === 0}
                    onClick={() => replaceEditorDraft({ tools: [] })}
                  >{t("全部关闭", "Disable all")}</button>
                  <button
                    type="button"
                    className="text-button"
                    disabled={editor.draft.tools === null}
                    onClick={() => replaceEditorDraft({ tools: null })}
                  >{t("继承对话设置", "Inherit the conversation")}</button>
                </div>
              </div>
              <ToolSelectionGroups
                tools={selectableTools as ToolDescriptor[]}
                // While inheriting, the picker shows the conversation's own set
                // rather than an empty list: that IS what this role will get,
                // and drawing it as "nothing selected" would misread as broken.
                enabledTools={editor.draft.tools ?? inheritedToolNames}
                onChange={(tools) => replaceEditorDraft({ tools })}
                expansionKey={editor.originalName ?? "new-role"}
                density="column"
              />
            </section>

            {editor.mode === "edit" && editor.originalName !== editor.draft.name && (
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

      {deleteCandidate && (
        <Dialog
          title={t("删除角色？", "Delete role?")}
          description={t(
            "主代理将不能再按这个名称派生子代理。",
            "The main agent will no longer be able to spawn a subagent under this name."
          )}
          width="440px"
          onClose={() => setDeleteCandidate(null)}
          footer={(
            <>
              <button type="button" className="button button--secondary" onClick={() => setDeleteCandidate(null)}>
                {t("取消", "Cancel")}
              </button>
              <button type="button" className="button button--danger" onClick={deleteDefinition}>
                {t("删除角色", "Delete role")}
              </button>
            </>
          )}
        >
          <p className="agent-definition-delete">
            <Bot size={15} aria-hidden="true" />
            <strong>{deleteCandidate.name}</strong>
          </p>
        </Dialog>
      )}
    </>
  );
}
