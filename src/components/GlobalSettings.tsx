import {
  MessageSquareText,
  Plus,
  Search,
  Trash2
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useI18n } from "../i18n";
import { createId } from "../lib/id";
import { emptyConversationPresetSettings } from "../lib/conversationPresets";
import { MAX_SYSTEM_PROMPT_BYTES, utf8ByteLength } from "../lib/textLimits";
import {
  SECURITY_LEVEL_OPTIONS,
  securityLevelDescription,
  securityLevelLabel
} from "../lib/securityLevels";
import { localizeToolDescriptor } from "../lib/toolDefaults";
import type {
  CapabilityCatalog,
  ConversationPreset,
  GlobalSettings as GlobalSettingsType,
  SettingsView,
  ToolDescriptor,
  Workspace
} from "../types";
import { Switch, CheckRow } from "./Common";
import { ApiProviderSettings } from "./ProviderSettings";
import { AgentDefinitionSettings } from "./AgentDefinitionSettings";
import { hasUsableAgentDefinition } from "../lib/agentDefinitions";
import { GlobalSettingsNavigation } from "./GlobalSettingsNavigation";
import { AppearanceSettings } from "./AppearanceSettings";
import { DependencySettings } from "./DependencySettings";
import { McpSettings } from "./McpSettings";
import { SettingsRail, SettingsRailEmpty, SettingsRailMenuItem, SettingsRailRow } from "./SettingsRail";
import { ShortcutSettings } from "./ShortcutSettings";
import { SkillSettings } from "./SkillSettings";
import { CapabilityResourceList, ToolDescriptionSetList } from "./PresetComposition";
import { ToolSelectionGroups } from "./ToolSelectionGroups";
import { UpdateSettings } from "./UpdateSettings";
import { WebSearchSettings } from "./WebSearchSettings";
import { WebSearchBehaviorSettings } from "./WebSearchBehaviorSettings";

type GlobalSettingsChange = GlobalSettingsType | ((current: GlobalSettingsType) => GlobalSettingsType);
type GlobalSettingsChangeHandler = (change: GlobalSettingsChange) => void;

interface GlobalSettingsProps {
  initialView: SettingsView;
  settings: GlobalSettingsType;
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
  workspaces: Workspace[];
  onChange: GlobalSettingsChangeHandler;
  onFlush?: () => Promise<void>;
  navigationPlacement?: "inline" | "external";
  onViewChange?: (view: SettingsView) => void;
  onClose?: () => void;
}

export function GlobalSettings({
  initialView,
  settings,
  tools,
  capabilities,
  workspaces: _workspaces,
  onChange,
  onFlush,
  navigationPlacement = "inline",
  onViewChange = () => undefined
}: GlobalSettingsProps) {
  const { t } = useI18n();
  // Retired view IDs remain valid navigation entry points and redirect to active pages.
  const redirectedView = (nextView: SettingsView): SettingsView => nextView === "web_search"
    ? "search_providers"
    : nextView === "advanced" || nextView === "memory" || nextView === "general"
      ? "appearance"
      : nextView === "hooks" || nextView === "capability_catalog" || nextView === "agents"
        ? "conversation_presets"
        : nextView;
  const [view, setView] = useState<SettingsView>(() => redirectedView(initialView));
  useEffect(() => {
    setView(redirectedView(initialView));
  }, [initialView]);
  const selectView = (nextView: SettingsView) => {
    const redirected = redirectedView(nextView);
    setView(redirected);
    onViewChange(redirected);
  };

  return (
    <section
      className={`global-settings-layout${navigationPlacement === "external" ? " global-settings-layout--page" : ""}`}
      aria-label={t("全局设置", "Global settings")}
    >
        {navigationPlacement === "inline" && (
          <GlobalSettingsNavigation view={view} onSelect={selectView} />
        )}
        <div className="global-settings-content">
          {view === "appearance" && <AppearanceSettings settings={settings} onChange={onChange} />}
          {view === "conversation_presets" && (
            <ConversationPresetsEditor
              settings={settings}
              tools={tools}
              capabilities={capabilities}
              onChange={onChange}
            />
          )}
          {view === "providers" && <ApiProviderSettings settings={settings} onChange={onChange} onFlush={onFlush} />}
          {view === "search_providers" && (
            <WebSearchSettings
              settings={settings.webSearch}
              onFlush={onFlush}
              onChange={(change) => onChange((current) => ({
                ...current,
                webSearch: typeof change === "function" ? change(current.webSearch) : change
              }))}
            />
          )}
          {view === "mcp" && (
            <McpSettings
              servers={settings.mcpServers}
              onChange={(mcpServers) => onChange((current) => ({ ...current, mcpServers }))}
            />
          )}
          {view === "skills" && (
            <SkillSettings
              skills={settings.skills}
              onChange={(skills) => onChange((current) => ({ ...current, skills }))}
            />
          )}
          {view === "shortcuts" && (
            <ShortcutSettings
              shortcuts={settings.shortcuts}
              onChange={(shortcuts) => onChange((current) => ({ ...current, shortcuts }))}
            />
          )}
          {view === "dependencies" && (
            <DependencySettings
              tools={settings.environmentTools}
              onChange={(environmentTools) => onChange((current) => ({ ...current, environmentTools }))}
            />
          )}
          {view === "updates" && <UpdateSettings />}
        </div>
    </section>
  );
}


/**
 * Conversation preset editor.
 *
 * The rail identifies the default preset, and the detail control is the sole
 * action for changing it.
 */
function ConversationPresetsEditor({
  settings,
  tools,
  capabilities,
  onChange
}: {
  settings: GlobalSettingsType;
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
  onChange: GlobalSettingsChangeHandler;
}) {
  const { resolvedLanguage, t } = useI18n();
  const [selectedId, setSelectedId] = useState<string | null>(settings.defaultConversationPresetId || settings.conversationPresets[0]?.id || null);
  const [query, setQuery] = useState("");
  // Categories are intentionally transient until the classification scheme is defined.
  const [presetCategories, setPresetCategories] = useState<Record<string, string>>({});
  const selected = settings.conversationPresets.find((item) => item.id === selectedId) ?? settings.conversationPresets[0] ?? null;
  const localizedTools = useMemo(() => tools.map((tool) => localizeToolDescriptor(tool, resolvedLanguage)), [resolvedLanguage, tools]);
  const untitled = t("未命名预设", "Untitled preset");
  const update = (preset: ConversationPreset) => onChange((current) => ({
    ...current,
    conversationPresets: current.conversationPresets.map((item) => item.id === preset.id ? preset : item)
  }));
  const updateSettings = (next: ConversationPreset["settings"]) => selected && update({ ...selected, settings: next });
  const add = () => {
    const source = selected?.settings ?? emptyConversationPresetSettings();
    const next: ConversationPreset = {
      id: createId("conversation_preset"),
      name: selected ? t("{name} 副本", "{name} copy", { name: selected.name || untitled }) : "",
      description: selected?.description ?? "",
      settings: {
        systemPrompt: source.systemPrompt,
        enabledTools: [...source.enabledTools],
        toolDescriptionFileId: source.toolDescriptionFileId,
        // Copy nested role objects to prevent edits in one preset affecting another.
        agentDefinitions: source.agentDefinitions.map((definition) => ({
          ...definition,
          modelSelection: { ...definition.modelSelection },
          tools: definition.tools === null ? null : [...definition.tools],
          disallowedTools: [...definition.disallowedTools],
          searchProvider: definition.searchProvider === null
            ? null
            : { ...definition.searchProvider }
        })),
        allowRolelessSubagents: source.allowRolelessSubagents === true,
        hookIds: [...source.hookIds],
        skillIds: [...source.skillIds],
        mcpIds: [...source.mcpIds],
        // Deep-copy the nested search provider selection.
        webSearch: {
          ...source.webSearch,
          provider: { ...source.webSearch.provider }
        },
        securityLevel: source.securityLevel,
        globalMemoryEnabled: source.globalMemoryEnabled,
        projectMemoryEnabled: source.projectMemoryEnabled,
        skillToolEnabled: source.skillToolEnabled === true
      }
    };
    onChange((current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, next],
      defaultConversationPresetId: current.defaultConversationPresetId || next.id
    }));
    setSelectedId(next.id);
  };
  const remove = (presetId: string) => {
    if (!window.confirm(t("确定删除此对话预设？", "Delete this conversation preset?"))) return;
    const remaining = settings.conversationPresets.filter((item) => item.id !== presetId);
    onChange((current) => ({
      ...current,
      conversationPresets: remaining,
      defaultConversationPresetId: current.defaultConversationPresetId === presetId
        ? remaining[0]?.id ?? ""
        : current.defaultConversationPresetId
    }));
    if (selectedId === presetId) setSelectedId(remaining[0]?.id ?? null);
  };
  const makeDefault = (presetId: string) => onChange((current) => ({
    ...current,
    defaultConversationPresetId: presetId
  }));

  /** Search preset names and descriptions in the rail. */
  const visiblePresets = (() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return settings.conversationPresets;
    return settings.conversationPresets.filter((preset) => (
      preset.name.toLowerCase().includes(needle) || preset.description.toLowerCase().includes(needle)
    ));
  })();

  const selectedName = selected ? selected.name.trim() || untitled : "";
  const isDefault = Boolean(selected && selected.id === settings.defaultConversationPresetId);

  return (
    <div className="settings-editor-page settings-rail-page conversation-preset-page">
      <SettingsRail
        search={{
          value: query,
          onChange: setQuery,
          label: t("搜索预设", "Search presets"),
          placeholder: t("搜索预设名称或描述", "Search preset names or descriptions")
        }}
        footer={(
          <button type="button" className="provider-rail__add" onClick={add}>
            <Plus size={13} /> {t("新建预设", "New preset")}
          </button>
        )}
      >
        {visiblePresets.map((preset) => (
          <SettingsRailRow
            key={preset.id}
            label={preset.name.trim() || untitled}
            title={preset.description || undefined}
            selected={preset.id === selected?.id}
            active={preset.id === settings.defaultConversationPresetId}
            onSelect={() => setSelectedId(preset.id)}
            menu={(close) => (
              <SettingsRailMenuItem
                danger
                onClick={() => {
                  close();
                  remove(preset.id);
                }}
              ><Trash2 size={13} /> {t("删除预设", "Delete preset")}</SettingsRailMenuItem>
            )}
          />
        ))}
        {!settings.conversationPresets.length && (
          <SettingsRailEmpty
            icon={<MessageSquareText size={18} />}
            title={t("还没有对话预设", "No conversation presets yet")}
            description={t("新建一个预设。", "Create a preset.")}
          />
        )}
        {Boolean(settings.conversationPresets.length) && !visiblePresets.length && (
          <SettingsRailEmpty
            icon={<Search size={18} />}
            title={t("没有匹配的预设", "No matching preset")}
            description={t("搜索会匹配预设名与描述。", "Search matches preset names and descriptions.")}
          />
        )}
      </SettingsRail>

      {selected ? (
        <div className="provider-pane">
          <header className="provider-pane__header">
            <div className="provider-pane__identity">
              <h1>{selectedName}</h1>
            </div>
            {/* A default preset cannot be unset while presets exist. */}
            <button
              type="button"
              className="button button--secondary button--small"
              disabled={isDefault}
              title={isDefault
                ? t(
                  "{name} 已经是新对话的默认预设。",
                  "{name} is already the default preset for new conversations.",
                  { name: selectedName }
                )
                : t(
                  "把 {name} 设为新对话的默认预设。",
                  "Make {name} the default preset for new conversations.",
                  { name: selectedName }
                )}
              onClick={() => makeDefault(selected.id)}
            >{isDefault ? t("已是默认", "Default") : t("设为默认", "Set as default")}</button>
          </header>

          <div className="provider-pane__body">
            <div className="provider-pane__stack">
              <section className="provider-field">
                <div className="provider-field__title"><span>{t("预设名称", "Preset name")}</span></div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input"
                      aria-label={t("预设名称", "Preset name")}
                      value={selected.name}
                      onChange={(event) => update({ ...selected, name: event.target.value })}
                    />
                  </div>
                </div>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("预设描述", "Preset description")}</span></div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input"
                      aria-label={t("预设描述", "Preset description")}
                      value={selected.description}
                      onChange={(event) => update({ ...selected, description: event.target.value })}
                    />
                  </div>
                </div>
                <p className="provider-field__help">{t(
                  "一句话说明；指针停在左栏那一行上时显示。",
                  "A one-line summary, shown when the pointer rests on its row in the list."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("预设类别", "Preset category")}</span></div>
                <select
                  className="input"
                  aria-label={t("预设类别", "Preset category")}
                  value={presetCategories[selected.id] ?? ""}
                  onChange={(event) => setPresetCategories((current) => ({ ...current, [selected.id]: event.target.value }))}
                >
                  <option value="">{t("未分类", "Uncategorized")}</option>
                  <option value="code">{t("代码", "Code")}</option>
                  <option value="writing">{t("写作", "Writing")}</option>
                  <option value="research">{t("研究", "Research")}</option>
                </select>
                <p className="provider-field__help">{t(
                  "分类体系还没定下来，这一项暂时不写进预设文档。",
                  "The category scheme is not settled yet, so this choice is not stored in the preset document."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("系统提示词", "System prompt")}</span></div>
                <div className="provider-input-group provider-input-group--multiline">
                  <textarea
                    className="provider-input provider-textarea"
                    aria-label={t("系统提示词", "System prompt")}
                    placeholder={t("留空则不发送基础系统提示词", "Leave empty to send no base system prompt")}
                    value={selected.settings.systemPrompt}
                    onChange={(event) => utf8ByteLength(event.target.value) <= MAX_SYSTEM_PROMPT_BYTES && updateSettings({ ...selected.settings, systemPrompt: event.target.value })}
                  />
                </div>
              </section>

              {/* Search and memory settings determine capabilities without being
                  individual tools. Template semantics: applying the preset copies
                  them into a newly created conversation; later edits to the preset
                  never mutate existing conversations. */}
              <section className="provider-field">
                <div className="provider-field__title"><span>{t("联网搜索", "Web search")}</span></div>
                <WebSearchBehaviorSettings
                  value={selected.settings.webSearch}
                  onChange={(patch) => updateSettings({
                    ...selected.settings,
                    webSearch: { ...selected.settings.webSearch, ...patch }
                  })}
                  webSearchAssets={settings.webSearch}
                />
              </section>

              {/* Memory tools are derived from these layer switches. */}
              <section className="provider-field">
                <div className="provider-field__title"><span>{t("记忆", "Memory")}</span></div>
                <div className="tool-toggle-row">
                  <span><strong>{t("启用全局记忆", "Enable global memory")}</strong><small>{t(
                    "~/.mework 的 MEWORK.md 与 MEMORY.md 拼进上下文，并暴露全局记忆的读取/创建/编辑三个工具。",
                    "~/.mework's MEWORK.md and MEMORY.md join the context, and the read/create/edit global memory tools are exposed."
                  )}</small></span>
                  <Switch
                    checked={Boolean(selected.settings.globalMemoryEnabled)}
                    onChange={(globalMemoryEnabled) => updateSettings({ ...selected.settings, globalMemoryEnabled })}
                    label={selected.settings.globalMemoryEnabled
                      ? t("全局记忆已开启", "Global memory enabled")
                      : t("全局记忆已关闭", "Global memory disabled")}
                  />
                </div>
                <div className="tool-toggle-row">
                  <span><strong>{t("启用项目记忆", "Enable project memory")}</strong><small>{t(
                    "工作区 .mework 的 MEWORK.md 与 MEMORY.md 拼进上下文，并暴露项目记忆的读取/创建/编辑三个工具。",
                    "The workspace's .mework MEWORK.md and MEMORY.md join the context, and the read/create/edit project memory tools are exposed."
                  )}</small></span>
                  <Switch
                    checked={Boolean(selected.settings.projectMemoryEnabled)}
                    onChange={(projectMemoryEnabled) => updateSettings({ ...selected.settings, projectMemoryEnabled })}
                    label={selected.settings.projectMemoryEnabled
                      ? t("项目记忆已开启", "Project memory enabled")
                      : t("项目记忆已关闭", "Project memory disabled")}
                  />
                </div>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("启用工具", "Enabled tools")}</span></div>
                <ToolSelectionGroups
                  tools={localizedTools}
                  enabledTools={selected.settings.enabledTools}
                  onChange={(enabledTools) => updateSettings({ ...selected.settings, enabledTools })}
                  expansionKey={selected.id}
                  density="column"
                />
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("代理角色", "Agent roles")}</span></div>
                <AgentDefinitionSettings
                  definitions={selected.settings.agentDefinitions}
                  providers={settings.apiProviders}
                  tools={localizedTools}
                  conversationEnabledTools={selected.settings.enabledTools}
                  webSearchAssets={settings.webSearch}
                  onChange={(agentDefinitions) => updateSettings({ ...selected.settings, agentDefinitions })}
                />
                {/* Hide the roleless-subagent switch when no usable role exists. */}
                {hasUsableAgentDefinition(selected.settings.agentDefinitions, settings.apiProviders) ? (
                  <div className="tool-toggle-row">
                    <span><strong>{t("允许无角色子代理", "Allow role-less subagents")}</strong><small>{t(
                      "关闭时 agent_spawn 与 workflow 步骤都必须点名一个角色。开启则恢复旧语义：省略角色的子代理沿用对话的模型。",
                      "When off, agent_spawn and workflow steps must both name a role. When on, the old behaviour returns: a subagent that names none inherits the conversation's model."
                    )}</small></span>
                    <Switch
                      checked={Boolean(selected.settings.allowRolelessSubagents)}
                      onChange={(allowRolelessSubagents) =>
                        updateSettings({ ...selected.settings, allowRolelessSubagents })}
                      label={selected.settings.allowRolelessSubagents
                        ? t("角色可选", "Role optional")
                        : t("角色必填", "Role required")}
                    />
                  </div>
                ) : null}
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("技能", "Skills")}</span></div>
                <CapabilityResourceList
                  resources={capabilities.skills}
                  selectedIds={selected.settings.skillIds}
                  onChange={(skillIds) => updateSettings({ ...selected.settings, skillIds })}
                  emptyText={t("尚未发现技能", "No skills discovered")}
                />
                {/* Hide the skill-loading switch when no skills are selected. */}
                {selected.settings.skillIds.length ? (
                  <div className="tool-toggle-row">
                    <span><strong>{t("技能按需加载", "Load skills on demand")}</strong><small>{t(
                      "关闭时已选技能的正文开局就拼进系统提示词。开启后改为暴露一个 skill 工具：模型先看到每个技能的名字与触发条件，需要哪一个才把正文取出来。",
                      "When off, the selected skills' bodies are concatenated into the system prompt up front. When on, a skill tool is exposed instead: the model sees each skill's name and trigger, and pulls the body only for the one it needs."
                    )}</small></span>
                    <Switch
                      checked={Boolean(selected.settings.skillToolEnabled)}
                      onChange={(skillToolEnabled) =>
                        updateSettings({ ...selected.settings, skillToolEnabled })}
                      label={selected.settings.skillToolEnabled
                        ? t("按需加载", "On demand")
                        : t("拼进提示词", "In the prompt")}
                    />
                  </div>
                ) : null}
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>MCP</span></div>
                <CapabilityResourceList
                  resources={capabilities.mcps}
                  selectedIds={selected.settings.mcpIds}
                  onChange={(mcpIds) => updateSettings({ ...selected.settings, mcpIds })}
                  emptyText={t("尚未发现 MCP Server", "No MCP servers discovered")}
                />
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("钩子", "Hooks")}</span></div>
                <CapabilityResourceList
                  resources={capabilities.hooks}
                  selectedIds={selected.settings.hookIds}
                  onChange={(hookIds) => updateSettings({ ...selected.settings, hookIds })}
                  emptyText={t("尚未发现钩子", "No hooks discovered")}
                />
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("工具描述", "Tool descriptions")}</span></div>
                <ToolDescriptionSetList
                  resources={capabilities.toolDescriptionFiles}
                  selectedId={selected.settings.toolDescriptionFileId}
                  onChange={(toolDescriptionFileId) => updateSettings({ ...selected.settings, toolDescriptionFileId })}
                />
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("安全策略", "Security policy")}</span></div>
                <div className="preset-choice-list">
                  {/* Applying a preset copies its security policy into the conversation. */}
                  {SECURITY_LEVEL_OPTIONS.map((level) => (
                    <CheckRow
                      key={level}
                      checked={selected.settings.securityLevel === level}
                      onChange={(checked) => {
                        if (checked) updateSettings({ ...selected.settings, securityLevel: level });
                      }}
                      title={securityLevelLabel(level, t)}
                      description={securityLevelDescription(level, t)}
                    />
                  ))}
                </div>
              </section>
            </div>
          </div>
        </div>
      ) : (
        <div className="provider-pane provider-pane--empty">
          <MessageSquareText size={24} />
          <strong>{t("还没有对话预设", "No conversation presets yet")}</strong>
          <span>{t(
            "把系统提示词、启用工具与技能/MCP/钩子的选择存成一份可套用的模板。",
            "Save a system prompt, tool selection, and skill/MCP/hook picks as a reusable template."
          )}</span>
          <button type="button" className="button button--secondary button--small" onClick={add}>
            <Plus size={13} /> {t("新建预设", "New preset")}
          </button>
        </div>
      )}
    </div>
  );
}
