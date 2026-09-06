import {
  Blocks,
  Bot,
  Box,
  FolderCog,
  MessageSquareText,
  Settings2,
  SlidersHorizontal,
  Wrench,
  X
} from "lucide-react";
import { useMemo } from "react";
import { useI18n } from "../i18n";
import { implicitConversationPreset } from "../lib/conversationPresets";
import type {
  CapabilityCatalog,
  Conversation,
  ConversationSettings as ConversationSettingsType,
  ConversationWebSearchSettings,
  GlobalSettings,
  SettingsView,
  ToolDescriptor
} from "../types";
import { CollapsibleSection, IconButton, Switch } from "./Common";
import { AgentDefinitionSettings } from "./AgentDefinitionSettings";
import { editableUserAgentDefinition, hasUsableAgentDefinition } from "../lib/agentDefinitions";
import { isHostDerivedToolName } from "../lib/taskTools";
import { CapabilityResourceList, ToolDescriptionSetList, toolDescriptionProfileTitle } from "./PresetComposition";
import { ToolSelectionGroups } from "./ToolSelectionGroups";
import { WebSearchBehaviorSettings } from "./WebSearchBehaviorSettings";
import "./ConversationSettings.css";

interface ConversationSettingsProps {
  conversation: Conversation;
  globalSettings: GlobalSettings;
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
  /** Changes to preset components apply directly to this conversation's settings. */
  onChange: (settings: ConversationSettingsType) => void;
  /** Changes to conversation-only fields. */
  onChangeConversationOnly: (patch: Partial<ConversationSettingsType>) => void;
  /** Applies a preset's components to this conversation. */
  onApplyPreset: (presetId: string) => void;
  /** Saves this conversation's preset components as a new independent global preset. */
  onSaveAsPreset: () => void;
  onOpenGlobalSettings: (view: SettingsView) => void;
  onClose: () => void;
}

export function ConversationSettings({
  conversation,
  globalSettings,
  tools,
  capabilities,
  onChange,
  onChangeConversationOnly,
  onApplyPreset,
  onSaveAsPreset,
  onOpenGlobalSettings,
  onClose
}: ConversationSettingsProps) {
  const { t, resolvedLanguage } = useI18n();
  const settings = conversation.settings;

  // Memory, task-runtime, and skill tools are derived from their respective
  // settings and are excluded from the enabled-tools list and its bulk actions.
  const toolNames = useMemo(() => Array.from(new Set(
    tools
      .filter((tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name))
      .map((tool) => tool.name)
  )), [tools]);
  const validEnabledToolCount = useMemo(() => {
    const enabled = new Set(settings.enabledTools);
    return toolNames.filter((name) => enabled.has(name)).length;
  }, [settings.enabledTools, toolNames]);
  const allToolsEnabled = toolNames.length > 0
    && validEnabledToolCount === toolNames.length;
  /* Counted with the SAME predicate the role list filters on: a conversation
   * legitimately carries host-injected project/plugin roles that are read-only
   * here and never drawn, so a raw `.length` would announce roles the list then
   * declines to show. */
  const editableRoleCount = useMemo(
    () => settings.agentDefinitions.filter(editableUserAgentDefinition).length,
    [settings.agentDefinitions]
  );
  /* Unlike the visible-role count, this asks whether the model can select a
   * role, including read-only project and plugin roles. */
  const hasUsableRole = useMemo(
    () => hasUsableAgentDefinition(settings.agentDefinitions, globalSettings.apiProviders),
    [settings.agentDefinitions, globalSettings.apiProviders]
  );
  const update = (patch: Partial<ConversationSettingsType>) => onChange({ ...settings, ...patch });
  const updateWebSearch = (patch: Partial<ConversationWebSearchSettings>) =>
    onChangeConversationOnly({ webSearch: { ...settings.webSearch, ...patch } });
  const replaceVisibleTools = (enabledTools: string[]) => {
    const visible = new Set(toolNames);
    update({
      enabledTools: [
        ...settings.enabledTools.filter((name) => !visible.has(name)),
        ...enabledTools
      ]
    });
  };
  /** A preset supplies initial values only; applying one never creates a
   * follow relationship, so the selector always displays its placeholder. */
  const presetOptions = globalSettings.conversationPresets.length
    ? globalSettings.conversationPresets
    : [implicitConversationPreset(tools, resolvedLanguage)];
  const selectedSetName = settings.toolDescriptionFileId === null
    ? t("Mework 内置（英文）", "Mework built-in (English)")
    : toolDescriptionProfileTitle(
      capabilities.toolDescriptionFiles.find(
        (resource) => resource.id === settings.toolDescriptionFileId
      ),
      t
    );

  return (
    <aside className="conversation-settings" aria-label={t("本对话设置", "Conversation settings")}>
      <header className="conversation-settings__header">
        <div>
          <span><SlidersHorizontal size={15} /> {t("仅影响当前对话", "Only affects this conversation")}</span>
          <h2>{t("本对话设置", "Conversation settings")}</h2>
        </div>
        <IconButton label={t("关闭本对话设置", "Close conversation settings")} onClick={onClose}><X size={18} /></IconButton>
      </header>

      <div className="conversation-settings__scroll">
        <div className="conversation-settings__apply">
          <select
            className="input"
            aria-label={t("套用预设", "Apply a preset")}
            value=""
            onChange={(event) => {
              if (event.target.value) onApplyPreset(event.target.value);
            }}
          >
            <option value="">{t("套用预设…", "Apply a preset…")}</option>
            {presetOptions.map((preset) => (
              <option
                key={preset.id}
                value={preset.id}
              >
                {preset.name || t("未命名预设", "Untitled preset")}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="text-button"
            onClick={onSaveAsPreset}
          >{t("另存为预设", "Save as preset")}</button>
          <button
            type="button"
            className="text-button"
            onClick={() => onOpenGlobalSettings("conversation_presets")}
          >{t("管理预设", "Manage presets")}</button>
        </div>

        <section className="settings-section">
          <div className="settings-section__heading">
            <MessageSquareText size={16} />
            <span><strong>{t("系统提示词", "System prompt")}</strong><small>{t(
              "{count} 字符",
              "{count} characters",
              { count: settings.systemPrompt.length }
            )}</small></span>
          </div>
          <textarea
            className="input input--code conversation-settings__system-prompt"
            rows={8}
            aria-label={t("系统提示词", "System prompt")}
            value={settings.systemPrompt}
            onChange={(event) => update({ systemPrompt: event.target.value })}
            placeholder={t(
              "留空则不发送基础系统提示词，只发送能力增补段",
              "Leave empty to send no base system prompt — only the capability sections"
            )}
          />
        </section>

        <section className="settings-section">
          <div className="settings-section__heading settings-section__heading--split">
            <div><Wrench size={16} /><span><strong>{t("启用工具", "Enabled tools")}</strong><small>{t(
              "{enabled} / {total} 个已选",
              "{enabled} / {total} selected",
              { enabled: validEnabledToolCount, total: toolNames.length }
            )}</small></span></div>
            <div className="tool-bulk-actions">
              <button
                type="button"
                className="text-button"
                disabled={!toolNames.length || allToolsEnabled}
                onClick={() => replaceVisibleTools(toolNames)}
              >{t("全部启用", "Enable all")}</button>
              <button
                type="button"
                className="text-button"
                disabled={!validEnabledToolCount}
                onClick={() => replaceVisibleTools([])}
              >{t("全部关闭", "Disable all")}</button>
            </div>
          </div>
          <ToolSelectionGroups
            tools={tools}
            enabledTools={settings.enabledTools}
            onChange={(enabledTools) => update({ enabledTools })}
            expansionKey={conversation.id}
          />
        </section>

        <CollapsibleSection
          title={t("代理角色", "Agent roles")}
          icon={<Bot size={15} />}
          summary={editableRoleCount
            ? t("{count} 个角色", "{count} roles", { count: editableRoleCount })
            : t("未定义", "None defined")}
        >
          {/* Preset components are copied into this conversation and never flow
              back to the preset. Roles are selected by name and bound to models here. */}
          <AgentDefinitionSettings
            definitions={settings.agentDefinitions}
            providers={globalSettings.apiProviders}
            tools={tools}
            conversationEnabledTools={settings.enabledTools}
            webSearchAssets={globalSettings.webSearch}
            onChange={(agentDefinitions) => update({ agentDefinitions })}
          />
          {/* Hide the switch when no usable role exists: the host always allows
              role-less execution in that case, so the switch would have no effect. */}
          {hasUsableRole ? (
            <div className="tool-toggle-row">
              <span><strong>{t("允许无角色子代理", "Allow role-less subagents")}</strong><small>{t(
                "关闭时 agent_spawn 与 workflow 步骤都必须点名一个角色。开启则恢复旧语义：省略角色的子代理沿用本对话的模型。",
                "When off, agent_spawn and workflow steps must both name a role. When on, the old behaviour returns: a subagent that names none inherits this conversation's model."
              )}</small></span>
              <Switch
                checked={Boolean(settings.allowRolelessSubagents)}
                onChange={(allowRolelessSubagents) => update({ allowRolelessSubagents })}
                label={settings.allowRolelessSubagents
                  ? t("角色可选", "Role optional")
                  : t("角色必填", "Role required")}
              />
            </div>
          ) : null}
        </CollapsibleSection>

        <CollapsibleSection
          title={t("技能", "Skills")}
          icon={<Box size={15} />}
          summary={settings.skillIds.length
            ? t("{count} 个已选", "{count} selected", { count: settings.skillIds.length })
            : t("未选择", "None selected")}
        >
          <CapabilityResourceList
            resources={capabilities.skills}
            selectedIds={settings.skillIds}
            onChange={(skillIds) => update({ skillIds })}
            emptyText={t("尚未发现技能", "No skills discovered")}
          />
          {/* This changes delivery, not the selected skills. Hide it when no
              skills are selected because neither delivery path has an effect. */}
          {settings.skillIds.length ? (
            <div className="tool-toggle-row">
              <span><strong>{t("技能按需加载", "Load skills on demand")}</strong><small>{t(
                "关闭时已选技能的正文开局就拼进系统提示词。开启后改为暴露一个 skill 工具：模型先看到每个技能的名字与触发条件，需要哪一个才把正文取出来。",
                "When off, the selected skills' bodies are concatenated into the system prompt up front. When on, a skill tool is exposed instead: the model sees each skill's name and trigger, and pulls the body only for the one it needs."
              )}</small></span>
              <Switch
                checked={Boolean(settings.skillToolEnabled)}
                onChange={(skillToolEnabled) => update({ skillToolEnabled })}
                label={settings.skillToolEnabled
                  ? t("按需加载", "On demand")
                  : t("拼进提示词", "In the prompt")}
              />
            </div>
          ) : null}
        </CollapsibleSection>

        <CollapsibleSection
          title="MCP"
          icon={<Blocks size={15} />}
          summary={settings.mcpIds.length
            ? t("{count} 个已选", "{count} selected", { count: settings.mcpIds.length })
            : t("未选择", "None selected")}
        >
          <CapabilityResourceList
            resources={capabilities.mcps}
            selectedIds={settings.mcpIds}
            onChange={(mcpIds) => update({ mcpIds })}
            emptyText={t("尚未发现 MCP Server", "No MCP servers discovered")}
          />
        </CollapsibleSection>

        <CollapsibleSection
          title={t("钩子", "Hooks")}
          icon={<FolderCog size={15} />}
          summary={settings.hookIds.length
            ? t("{count} 个已选", "{count} selected", { count: settings.hookIds.length })
            : t("未选择", "None selected")}
        >
          <CapabilityResourceList
            resources={capabilities.hooks}
            selectedIds={settings.hookIds}
            onChange={(hookIds) => update({ hookIds })}
            emptyText={t("尚未发现钩子", "No hooks discovered")}
          />
        </CollapsibleSection>

        <CollapsibleSection
          title={t("工具描述", "Tool descriptions")}
          icon={<Settings2 size={15} />}
          summary={selectedSetName ?? t("悬空选择", "Dangling selection")}
        >
          <ToolDescriptionSetList
            resources={capabilities.toolDescriptionFiles}
            selectedId={settings.toolDescriptionFileId}
            onChange={(toolDescriptionFileId) => update({ toolDescriptionFileId })}
          />
        </CollapsibleSection>

        <section className="settings-section">
          <div className="web-search-provider-field">
            <WebSearchBehaviorSettings
              value={settings.webSearch}
              onChange={updateWebSearch}
              webSearchAssets={globalSettings.webSearch}
            />
          </div>
          <div className="tool-toggle-row">
            <span><strong>{t("启用全局记忆", "Enable global memory")}</strong><small>{t(
              "开启后，~/.mework 的 MEWORK.md 常驻指令与 MEMORY.md 记忆索引拼进上下文，读取/创建/编辑全局记忆三个工具随之可用。",
              "When enabled, ~/.mework's MEWORK.md instructions and MEMORY.md index join the context, and the read/create/edit global memory tools become available."
            )}</small></span>
            <Switch
              checked={Boolean(settings.globalMemoryEnabled)}
              onChange={(globalMemoryEnabled) => update({ globalMemoryEnabled })}
              label={settings.globalMemoryEnabled
                ? t("全局记忆已开启", "Global memory enabled")
                : t("全局记忆已关闭", "Global memory disabled")}
            />
          </div>
          <div className="tool-toggle-row">
            <span><strong>{t("启用项目记忆", "Enable project memory")}</strong><small>{t(
              "开启后，当前工作区 .mework 的 MEWORK.md 与 MEMORY.md 拼进上下文，读取/创建/编辑项目记忆三个工具随之可用。",
              "When enabled, this workspace's .mework MEWORK.md and MEMORY.md join the context, and the read/create/edit project memory tools become available."
            )}</small></span>
            <Switch
              checked={Boolean(settings.projectMemoryEnabled)}
              onChange={(projectMemoryEnabled) => update({ projectMemoryEnabled })}
              label={settings.projectMemoryEnabled
                ? t("项目记忆已开启", "Project memory enabled")
                : t("项目记忆已关闭", "Project memory disabled")}
            />
          </div>
          <div className="tool-toggle-row">
            <span><strong>{t("拼接应用数据目录", "Include app data directory")}</strong><small>{t(
              "开启后，模型会在系统提示词中看到受信任的应用数据目录绝对路径。",
              "When enabled, the model sees the trusted app data directory's absolute path in the system prompt."
            )}</small></span>
            <Switch
              checked={Boolean(settings.includeAppDataPath)}
              onChange={(includeAppDataPath) => onChangeConversationOnly({ includeAppDataPath })}
              label={settings.includeAppDataPath
                ? t("拼接应用数据目录已开启", "App data directory enabled")
                : t("拼接应用数据目录已关闭", "App data directory disabled")}
            />
          </div>
        </section>
      </div>
    </aside>
  );
}
