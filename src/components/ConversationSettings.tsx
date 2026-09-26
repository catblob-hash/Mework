import {
  Blocks,
  Bot,
  Box,
  FileText,
  FolderCog,
  ShieldCheck,
  SlidersHorizontal
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  captureConversationPresetSettings,
  isBuiltinConversationPreset
} from "../lib/conversationPresets";
import { modelChoiceOf } from "../lib/documentUpdates";
import { supportsVision } from "../lib/modelCapabilities";
import type {
  CapabilityCatalog,
  ContextItem,
  Conversation,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings as ConversationSettingsType,
  ConversationTemplateSummary,
  ConversationWebSearchSettings,
  GlobalSettings,
  McpProbeReport,
  ResourceDescriptor,
  ToolDescriptor
} from "../types";
import { Dialog, Switch } from "./Common";
import { editableUserAgentDefinition } from "../lib/agentDefinitions";
import { isHostDerivedToolName, isPreviewLifecycleToolName } from "../lib/taskTools";
import { familySelectsNativeToolType, familySupportsNativeFetch } from "../lib/webSearch";
import { toolLockOf } from "../lib/toolLock";
import { ConversationTemplateEditor } from "./ConversationTemplateEditor";
import { FeaturesPage } from "./FeaturesPage";
import { SandboxSettings } from "./SandboxSettings";
import {
  AgentRolesPage,
  CapabilitySelectionPage,
  ConversationPresetsPage,
  lockedInThisConversationHint,
  settledBackendHint
} from "./ConversationSettingsPages";
import "./ConversationSettings.css";

/**
 * The pages the conversation-settings pane lists down its left edge.
 *
 * `features` carries everything that is not a catalog of named things — the tool
 * picker and the switches derived from it. The others each own one kind of thing
 * the conversation composes with, so a page is never a mixed bag.
 */
type ConversationSettingsView =
  | "features"
  | "sandbox"
  | "skills"
  | "mcp"
  | "hooks"
  | "roles"
  | "template"
  | "presets";

/**
 * What this pane is editing.
 *
 * `preset` is the same pane opened from the presets page on a saved preset, so
 * the two can never drift apart. It drops the page that describes a live
 * conversation rather than a reusable body — a preset has no presets of its own
 * to nest — it gains the one page only a preset has, and its footer saves in
 * place instead of saving a copy.
 */
export type ConversationSettingsMode = "conversation" | "preset";

const NAVIGATION: Array<{ id: ConversationSettingsView; icon: typeof SlidersHorizontal }> = [
  { id: "features", icon: SlidersHorizontal },
  { id: "sandbox", icon: ShieldCheck },
  { id: "skills", icon: Box },
  { id: "mcp", icon: Blocks },
  { id: "hooks", icon: FolderCog },
  { id: "roles", icon: Bot },
  { id: "template", icon: FileText },
  { id: "presets", icon: SlidersHorizontal }
];

/** The page a preset body has no meaning for: a preset holds no presets. */
const PRESET_HIDDEN: ReadonlySet<ConversationSettingsView> = new Set(["presets"]);

/**
 * The page only a preset has.
 *
 * A template belongs to whatever opens with it, and a live conversation opens
 * with itself — its timeline IS its message queue, editable in place. So the
 * page is drawn for a preset, and for a role in its own window, and nowhere
 * else.
 */
const CONVERSATION_HIDDEN: ReadonlySet<ConversationSettingsView> = new Set(["template"]);

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
  /** Renames a saved preset in place. */
  onRenamePreset?: (presetId: string, name: string) => void;
  /** Deletes a saved preset. Conversations keep the dangling trace. */
  onDeletePreset?: (presetId: string) => void;
  /** Writes an edited body back onto a saved preset. */
  onSavePreset?: (presetId: string, settings: ConversationPresetSettings) => void;
  /** Saves an edited body of the built-in preset, which cannot change, as a new preset. */
  onSavePresetCopy?: (presetId: string, settings: ConversationPresetSettings) => void;
  /**
   * Records the template id a preset opens with, at once rather than on Save.
   * A body is written to the host the moment the user asks for it, so the id it
   * landed under has to reach the document in the same breath — a preset dialog
   * abandoned afterwards would otherwise leave a body nothing cites.
   */
  onBindPresetTemplate?: (presetId: string, templateId: string) => void;
  /** Every stored template, for the message count a rail entry trails. */
  templates: ConversationTemplateSummary[];
  /** Reads a template body from the host store; an unwritten id reads as empty. */
  onReadTemplate: (templateId: string) => Promise<ContextItem[]>;
  /**
   * Writes a body and resolves with the id it landed under, minting one when the
   * owner has none yet. Bodies only ever leave the renderer through here.
   */
  onWriteTemplate: (templateId: string, contexts: ContextItem[]) => Promise<string>;
  /**
   * Removes a skill, an MCP server or a hook from the catalog itself — the
   * folder, the record, the line in `hooks.json` — not just from this
   * conversation. Omitted where the owner has nowhere to route that, which is
   * every surface that is not the live application.
   */
  onDeleteCapability?: (kind: "skills" | "mcp" | "hooks", resource: ResourceDescriptor) => void;
  /** The last failed capability delete, shown on the page it happened on. */
  capabilityError?: string | null;
  /**
   * The conversation's workspace, for narrowing the three capability pages to
   * the entries a run may actually use: the global level plus this workspace's.
   * `null` is a draft that has no workspace yet, which sees the global level
   * alone. Undefined means no narrowing — the preset editor, which is reusable
   * and therefore points at nothing in particular.
   */
  workspaceId?: string | null;
  /**
   * Re-runs capability discovery on disk. Called once when the pane mounts, and
   * from a toolbar button on each capability page.
   */
  onRescanCapabilities?: () => void | Promise<void>;
  /**
   * Opens a capability level in the OS file manager. `null` is the global
   * `~/.mework`; a workspace id opens that workspace's `.mework`.
   */
  onRevealCapabilityLocation?: (
    kind: "skills" | "mcp" | "hooks",
    workspaceId: string | null
  ) => void;
  /** Tests an MCP server's connection, from the MCP page's row action. */
  onProbeMcpServer?: (resource: ResourceDescriptor) => Promise<McpProbeReport>;
  /**
   * The last failed preset apply, shown on the presets page. A preset that opens
   * with a message queue reaches the host to instantiate it, and that is the one
   * thing here that can fail after the click.
   */
  presetError?: string | null;
  /**
   * In `conversation` mode, saves these components as a new independent preset.
   * In `preset` mode, saves the open preset in place.
   */
  onSaveAsPreset: () => void;
  mode?: ConversationSettingsMode;
  /** In `preset` mode, the preset this pane is a window onto. */
  presetId?: string;
}

export function ConversationSettings({
  conversation,
  globalSettings,
  tools,
  capabilities,
  onChange,
  onChangeConversationOnly,
  onApplyPreset,
  onRenamePreset,
  onDeletePreset,
  onSavePreset,
  onSavePresetCopy,
  onBindPresetTemplate,
  templates,
  onReadTemplate,
  onWriteTemplate,
  onDeleteCapability,
  capabilityError = null,
  presetError = null,
  workspaceId,
  onRescanCapabilities,
  onRevealCapabilityLocation,
  onProbeMcpServer,
  onSaveAsPreset,
  mode = "conversation",
  presetId
}: ConversationSettingsProps) {
  const { t } = useI18n();
  const settings = conversation.settings;
  const [view, setView] = useState<ConversationSettingsView>("features");
  /* The preset currently open in a window of this pane, held as a whole
   * conversation-settings body rather than a preset body: the pane edits the
   * former, and narrowing back down to the latter is exactly what saving is. */
  const [presetDraft, setPresetDraft] = useState<
    { id: string; settings: ConversationSettingsType } | null
  >(null);
  /* The open preset's template body, read once the user asks for that page. A
   * template is big enough that reading it on the chance the page is opened
   * would make opening a preset slower for everyone who never looks. */
  const [templateBody, setTemplateBody] = useState<ContextItem[] | null>(null);
  const editingPreset = mode === "preset";
  const pages = useMemo(
    () => NAVIGATION.filter((item) => (editingPreset
      ? !PRESET_HIDDEN.has(item.id)
      : !CONVERSATION_HIDDEN.has(item.id))),
    [editingPreset]
  );

  /* Opening the pane is one of the moments discovery runs (a run re-discovers on
   * its own anyway), so the catalog it draws is what is on disk now rather than
   * what startup saw. Guarded so it fires once per mounted pane even if the
   * owner hands down a fresh closure on every render. */
  const rescannedOnMount = useRef(false);
  useEffect(() => {
    if (rescannedOnMount.current || !onRescanCapabilities) return;
    rescannedOnMount.current = true;
    void onRescanCapabilities();
  }, [onRescanCapabilities]);

  // Memory, task-runtime, and skill tools are derived from their respective
  // settings and are excluded from the enabled-tools list and its count. The
  // preview lifecycle tools follow the other preview tools, so they are not
  // counted either.
  const toolNames = useMemo(() => Array.from(new Set(
    tools
      .filter((tool) => tool.category !== "memory"
        && !isHostDerivedToolName(tool.name)
        && !isPreviewLifecycleToolName(tool.name))
      .map((tool) => tool.name)
  )), [tools]);
  const validEnabledToolCount = useMemo(() => {
    const enabled = new Set(settings.enabledTools);
    return toolNames.filter((name) => enabled.has(name)).length;
  }, [settings.enabledTools, toolNames]);
  /* Counted with the SAME predicate the role list filters on: a conversation
   * legitimately carries host-injected project/plugin roles that are read-only
   * here and never drawn, so a raw `.length` would announce roles the list then
   * declines to show. */
  const editableRoleCount = useMemo(
    () => settings.agentDefinitions.filter(editableUserAgentDefinition).length,
    [settings.agentDefinitions]
  );
  const update = (patch: Partial<ConversationSettingsType>) => onChange({ ...settings, ...patch });
  const updateWebSearch = (patch: Partial<ConversationWebSearchSettings>) =>
    onChangeConversationOnly({ webSearch: { ...settings.webSearch, ...patch } });
  /* Whether the model this conversation runs on can retrieve a page with its
   * own provider's server-side fetch tool. It decides whether the fetch-provider
   * selector is worth drawing at all, so it is read from the active provider
   * rather than from the conversation, which carries no model binding. */
  const nativeFetchAvailable = useMemo(
    () => familySupportsNativeFetch(
      globalSettings.apiProviders.find(
        (provider) => provider.id === globalSettings.activeProviderId
      )?.family
    ),
    [globalSettings.apiProviders, globalSettings.activeProviderId]
  );
  /* Whether this conversation's model writes the version of its native web
   * tools into the request, which is the only case where there is a version to
   * choose. Read from the active provider for the same reason as above. The
   * conversation keeps its chosen versions either way — a model on another
   * protocol quietly sends its own native tools instead of rewriting them. */
  const nativeToolTypeSelectable = useMemo(
    () => familySelectsNativeToolType(
      globalSettings.apiProviders.find(
        (provider) => provider.id === globalSettings.activeProviderId
      )?.family
    ),
    [globalSettings.apiProviders, globalSettings.activeProviderId]
  );
  /* Whether a message written into the preset's own template may carry an
   * image: a preset opens conversations that run on the conversation model, so
   * that model is the one that answers for it. */
  const imageInputSupported = useMemo(() => {
    const model = modelChoiceOf(globalSettings).model;
    return Boolean(model && supportsVision(model));
  }, [globalSettings]);
  /* Everything a run has already put in front of the model. Tool exposure is
   * one-way, so each control the lock covers is drawn spent rather than removed:
   * the user can still see what this conversation is carrying. */
  const lock = useMemo(() => toolLockOf(settings), [settings]);
  const lockedHint = lockedInThisConversationHint(t);
  /* Delivery freezes as soon as any skill has gone out, in either direction:
     bodies already in the prompt cannot be moved behind a tool the earlier
     rounds did not have, and trigger lines already sent cannot be replaced by
     bodies without saying the same thing twice. A conversation that has run
     without ever selecting a skill is still free to choose. */
  const skillDeliveryLocked = lock.skillIds.length > 0;
  /* Same rule one catalog over: once a server has been dialed, whether its
     tools' schemas went on the wire is settled. Schemas already declared
     cannot be withdrawn behind a tool the earlier rounds did not have, and
     names already announced cannot retroactively have carried their schemas. */
  const mcpDeliveryLocked = lock.mcpIds.length > 0;
  const presetOptions = globalSettings.conversationPresets;
  /* The built-in preset ships with the build and cannot change, so its window
   * edits a draft the user can only keep as a preset of their own. */
  const builtinPreset = editingPreset && Boolean(presetId) && isBuiltinConversationPreset(presetId!);
  /** A deleted preset leaves a dangling ID, which reads as an unnamed draft. */
  const appliedPresetId = presetOptions.some((preset) => preset.id === conversation.presetId)
    ? conversation.presetId
    : "";
  /* In preset mode the pane is a window onto one preset, and the preset's
   * template rides in on the synthesized conversation's own trace field. An id
   * nothing has been written under yet is the ordinary state of a preset that
   * has never been given an opening queue. */
  const openTemplateId = editingPreset ? conversation.templateId : "";

  /* Read the body the first time the page is opened, and only then. Switching
   * away and back keeps what is already in hand: the editor holds the draft, so
   * re-reading would be a second chance to lose unsaved edits, not a refresh. */
  useEffect(() => {
    if (view !== "template") return;
    if (!openTemplateId) { setTemplateBody([]); return; }
    let abandoned = false;
    setTemplateBody(null);
    void (async () => {
      try {
        const contexts = await onReadTemplate(openTemplateId);
        if (!abandoned) setTemplateBody(contexts);
      } catch {
        // An unreadable body is an empty one to work from: the page still has to
        // open, and saving overwrites whatever is there regardless.
        if (!abandoned) setTemplateBody([]);
      }
    })();
    return () => { abandoned = true; };
  }, [onReadTemplate, openTemplateId, view]);

  const openPreset = (presetId: string) => {
    const preset = presetOptions.find((candidate) => candidate.id === presetId);
    if (!preset) return;
    setPresetDraft({ id: preset.id, settings: presetBodyAsSettings(preset, settings) });
  };
  const openedPreset = presetDraft
    ? presetOptions.find((preset) => preset.id === presetDraft.id) ?? null
    : null;

  /* A list id has to be unique among the lists mounted at once, and a preset
   * opens this same pane in a dialog over the conversation's copy of it — so the
   * two skills lists on screen must not answer to the same name. */
  const listId = (view: ConversationSettingsView) => `catalog:${conversation.id}:${view}`;

  const pageTitles: Record<ConversationSettingsView, string> = {
    features: t("功能", "Features"),
    sandbox: t("沙箱", "Sandbox"),
    skills: t("技能", "Skills"),
    mcp: "MCP",
    hooks: t("钩子", "Hooks"),
    roles: t("代理角色", "Agent roles"),
    template: t("对话模板", "Conversation template"),
    presets: t("对话预设", "Conversation presets")
  };
  /** The count each row trails, so the nav says what the conversation carries. */
  const pageCounts: Record<ConversationSettingsView, number | null> = {
    features: null,
    sandbox: null,
    skills: settings.skillIds.length,
    mcp: settings.mcpIds.length,
    hooks: settings.hookIds.length,
    roles: editableRoleCount,
    template: templates.find((item) => item.id === openTemplateId)?.messageCount ?? 0,
    presets: presetOptions.length
  };
  const pageBlurbs: Record<ConversationSettingsView, string> = {
    features: t(
      "本对话给模型的工具面，以及由开关派生的那几件事。",
      "The tool surface this conversation hands the model, and the things derived from switches."
    ),
    sandbox: editingPreset
      ? t(
        "套用这份预设的对话，命令是否在操作系统的沙箱里运行。沙箱以对话为单位：每个对话在它用到的每台机器上各有一个沙箱进程，按“命令里的代码可能是恶意的”来设防。",
        "Whether the commands of a conversation this preset is applied to run in the operating system's sandbox. A sandbox is drawn around one conversation: each has one sandboxed process on every machine it uses, set up on the assumption that the code its commands run may be hostile."
      )
      : t(
        "本对话的命令是否在操作系统的沙箱里运行。沙箱以对话为单位：本对话在它用到的每台机器上各有一个沙箱进程，与其他对话互不影响，按“命令里的代码可能是恶意的”来设防。",
        "Whether this conversation's commands run in the operating system's sandbox. A sandbox is drawn around one conversation: this one has one sandboxed process on every machine it uses, apart from every other conversation's, set up on the assumption that the code its commands run may be hostile."
      ),
    skills: t(
      "技能从 ~/.mework/skills/ 与工作区的 .mework/skills/ 扫描而来；这里挑给本对话用的，并决定正文怎么送到模型面前。",
      "Skills are scanned from ~/.mework/skills/ and the workspace's .mework/skills/. Pick which ones this conversation uses, and how their bodies reach the model."
    ),
    mcp: t(
      "MCP Server 写在 ~/.mework/mcp.json 或工作区的 .mework/mcp.json 里；选中的在本对话运行期间接入，它们的工具随之出现。",
      "MCP servers are declared in ~/.mework/mcp.json or the workspace's .mework/mcp.json. Selected servers attach for this conversation's runs, and their tools appear with them."
    ),
    hooks: t(
      "钩子写在 ~/.mework/hooks.json 或工作区的 .mework/hooks.json 里，在固定的事件点运行外部命令；这里只挑本对话要跑哪些。",
      "Hooks live in ~/.mework/hooks.json or the workspace's .mework/hooks.json and run external commands at fixed events. Pick which ones apply to this conversation."
    ),
    roles: t(
      "子代理可以点名的角色。每个角色自己绑模型、工具与开局历史，开关决定它是否出现在模型面前。",
      "The roles a subagent can be spawned as. Each binds its own model, tools and opening history; the switch decides whether the model sees it at all."
    ),
    template: t(
      "套用这份预设时铺进对话的开局消息。右键可以插入消息与工具调用；工具只列本预设启用的那些。",
      "The opening messages laid into a conversation when this preset is applied. Right-click to insert messages and tool calls; only the tools this preset enables are offered."
    ),
    presets: t(
      "存好的这整页设置。套用一份会把它的内容复制到本对话。",
      "Saved copies of this whole page. Applying one copies its contents into this conversation."
    )
  };

  return (
    <div className="conversation-settings">
      <nav
        className="conversation-settings__nav settings-nav"
        aria-label={t("对话设置分类", "Conversation settings categories")}
      >
        <div className="conversation-settings__nav-items">
          {pages.map((item) => {
            const Icon = item.icon;
            const count = pageCounts[item.id];
            return (
              <button
                type="button"
                key={item.id}
                aria-current={view === item.id || undefined}
                className={view === item.id
                  ? "settings-nav__item settings-nav__item--active"
                  : "settings-nav__item"}
                onClick={() => setView(item.id)}
              >
                <Icon size={14} aria-hidden="true" />
                <span>{pageTitles[item.id]}</span>
                {/* A zero says "nothing selected" as clearly as a number says how
                    many, so the count is drawn for every page that has one. */}
                {count === null
                  ? null
                  : <small className="conversation-settings__nav-count">{count}</small>}
              </button>
            );
          })}
        </div>

        {/* Saving is the only preset action that belongs to the whole page rather
            than to one preset's row, so it is the one thing under the list. */}
        <div className="conversation-settings__nav-footer">
          <div className="conversation-settings__preset-actions">
            <button
              type="button"
              className="text-button"
              onClick={onSaveAsPreset}
            >{editingPreset && !builtinPreset
              ? t("保存预设", "Save preset")
              : builtinPreset
                ? t("另存为新预设", "Save as new preset")
                : t("另存为预设", "Save as preset")}</button>
          </div>
        </div>
      </nav>

      <div className="conversation-settings__page">
        <header className="conversation-settings__page-header">
          <p>{pageBlurbs[view]}</p>
          {builtinPreset && <p className="field__hint">{t(
            "内置预设随 Mework 版本更新，不能修改或删除。在这里改动的设置可以另存为一份新预设；对话模板只读，不随副本带走。",
            "The built-in preset updates with Mework and cannot be edited or deleted. Settings changed here can be saved as a new preset; the conversation template is read-only and does not go with the copy."
          )}</p>}
        </header>
        {/* The template page is a timeline, which brings its own scroller and
            its own edges. Padding and a second scrollbar around one would fight
            it, so that page gets the body flush. */}
        <div className={view === "template"
          ? "conversation-settings__page-body conversation-settings__page-body--flush"
          : "conversation-settings__page-body"}>
          <div className={view === "template"
            ? "conversation-settings__page-stack conversation-settings__page-stack--flush"
            : "conversation-settings__page-stack"}>
            {view === "features" && (
              <FeaturesPage
                picker={{
                  tools,
                  enabledTools: settings.enabledTools,
                  lockedTools: lock.tools,
                  onChange: (enabledTools) => update({ enabledTools }),
                  expansionKey: conversation.id
                }}
                pickerSummary={t(
                  "{enabled} / {total} 个已选",
                  "{enabled} / {total} selected",
                  { enabled: validEnabledToolCount, total: toolNames.length }
                )}
                webAccess={{
                  enabled: Boolean(settings.webSearchEnabled),
                  locked: lock.webSearch,
                  onChange: (webSearchEnabled) => update({ webSearchEnabled })
                }}
                web={{
                  value: settings.webSearch,
                  onChange: updateWebSearch,
                  webSearchAssets: globalSettings.webSearch,
                  nativeFetchAvailable,
                  nativeToolTypeSelectable,
                  lockedHint: settledBackendHint(t),
                  searchLocked: lock.searchProvider !== null,
                  fetchLocked: lock.fetchProvider !== null
                }}
                memory={{
                  global: Boolean(settings.globalMemoryEnabled),
                  project: Boolean(settings.projectMemoryEnabled),
                  globalLocked: lock.globalMemory,
                  projectLocked: lock.projectMemory,
                  onChangeGlobal: (globalMemoryEnabled) => update({ globalMemoryEnabled }),
                  onChangeProject: (projectMemoryEnabled) => update({ projectMemoryEnabled })
                }}
                /* Conversation-only, so a preset body has no room to carry it:
                   drawing the switch there would promise a save that discards it. */
                appDataPath={editingPreset ? undefined : {
                  enabled: Boolean(settings.includeAppDataPath),
                  onChange: (includeAppDataPath) => onChangeConversationOnly({ includeAppDataPath })
                }}
                toolDescription={{
                  resources: capabilities.toolDescriptionFiles,
                  selectedId: settings.toolDescriptionFileId,
                  onChange: (toolDescriptionFileId) => update({ toolDescriptionFileId })
                }}
                lockedHint={lockedHint}
              />
            )}

            {view === "sandbox" && (
              <SandboxSettings
                settings={settings.sandbox}
                onChange={(sandbox) => update({ sandbox })}
              />
            )}

            {view === "skills" && (
              <CapabilitySelectionPage
                kind="skills"
                onDelete={onDeleteCapability && ((resource) => onDeleteCapability("skills", resource))}
                error={capabilityError}
                listId={listId("skills")}
                resources={capabilities.skills}
                selectedIds={settings.skillIds}
                lockedIds={lock.skillIds}
                workspaceId={workspaceId}
                onRescan={onRescanCapabilities}
                onReveal={onRevealCapabilityLocation}
                onChange={(skillIds) => update({ skillIds })}
                searchLabel={t("搜索技能", "Search skills")}
                emptyTitle={t("尚未发现技能", "No skills discovered")}
                emptyDescription={t(
                  "把技能目录放在 ~/.mework/skills/ 或工作区的 .mework/skills/ 下，Mework 会自动扫描。",
                  "Put skill folders under ~/.mework/skills/ or the workspace's .mework/skills/ and Mework scans them automatically."
                )}
                /* Delivery is a property of the page, not of any one row, so it
                   is read after the list it qualifies and drawn even with an
                   empty catalog: how skills would arrive is worth knowing
                   before deciding to install one. Frozen once skills have gone
                   out — the transcript already carries them one way. */
                footer={(
                  <div className={`tool-toggle-row${skillDeliveryLocked ? " tool-toggle-row--locked" : ""}`}>
                    <span><strong>{t("技能按需加载", "Load skills on demand")}</strong><small>{t(
                      "关闭时已选技能的正文开局就拼进系统提示词。开启后改为暴露一个 skill 工具：模型先看到每个技能的名字与触发条件，需要哪一个才把正文取出来。",
                      "When off, the selected skills' bodies are concatenated into the system prompt up front. When on, a skill tool is exposed instead: the model sees each skill's name and trigger, and pulls the body only for the one it needs."
                    )}</small>{skillDeliveryLocked && <small>{lockedHint}</small>}</span>
                    <Switch
                      checked={Boolean(settings.skillToolEnabled)}
                      disabled={skillDeliveryLocked}
                      onChange={(skillToolEnabled) => update({ skillToolEnabled })}
                      label={settings.skillToolEnabled
                        ? t("按需加载", "On demand")
                        : t("拼进提示词", "In the prompt")}
                    />
                  </div>
                )}
              />
            )}

            {view === "mcp" && (
              <CapabilitySelectionPage
                kind="mcp"
                onDelete={onDeleteCapability && ((resource) => onDeleteCapability("mcp", resource))}
                error={capabilityError}
                listId={listId("mcp")}
                resources={capabilities.mcps}
                selectedIds={settings.mcpIds}
                lockedIds={lock.mcpIds}
                workspaceId={workspaceId}
                onRescan={onRescanCapabilities}
                onReveal={onRevealCapabilityLocation}
                onProbeMcpServer={onProbeMcpServer}
                onChange={(mcpIds) => update({ mcpIds })}
                searchLabel={t("搜索 MCP Server", "Search MCP servers")}
                emptyTitle={t("尚未发现 MCP Server", "No MCP servers discovered")}
                emptyDescription={t(
                  "把服务器写进 ~/.mework/mcp.json 或工作区的 .mework/mcp.json（mcpServers 格式，与 Claude Code 的 .mcp.json 相同），保存后重新扫描。",
                  "Declare servers in ~/.mework/mcp.json or the workspace's .mework/mcp.json (the mcpServers shape, same as Claude Code's .mcp.json), then rescan."
                )}
                /* How the selected servers' tools arrive, read after the list
                   it qualifies and drawn even with an empty catalog: a server
                   with thirty tools costs a great deal of every request, and
                   that is worth knowing before adding the first one. Frozen
                   once any server has been dialed — the transcript already
                   carries its tools one way. */
                footer={(
                  <div className={`tool-toggle-row${mcpDeliveryLocked ? " tool-toggle-row--locked" : ""}`}>
                    <span><strong>{t("工具发现", "Tool discovery")}</strong><small>{t(
                      "关闭时每个 MCP 工具的完整 schema 每一轮都随请求发出。开启后改为只报名字，并暴露一个 tool_search 工具：模型搜到需要的工具，才把它的 schema 取回来，取回之后就能直接调用。",
                      "When off, every MCP tool's full schema goes out with every request. When on, only the names are announced and a tool_search tool is exposed: the model searches for the tool it needs, pulls that one's schema, and can then call it directly."
                    )}</small>{mcpDeliveryLocked && <small>{lockedHint}</small>}</span>
                    <Switch
                      checked={Boolean(settings.mcpToolDiscoveryEnabled)}
                      disabled={mcpDeliveryLocked}
                      onChange={(mcpToolDiscoveryEnabled) => update({ mcpToolDiscoveryEnabled })}
                      label={settings.mcpToolDiscoveryEnabled
                        ? t("按需取回", "On demand")
                        : t("全部声明", "All declared")}
                    />
                  </div>
                )}
              />
            )}

            {view === "hooks" && (
              <CapabilitySelectionPage
                kind="hooks"
                onDelete={onDeleteCapability && ((resource) => onDeleteCapability("hooks", resource))}
                error={capabilityError}
                listId={listId("hooks")}
                resources={capabilities.hooks}
                selectedIds={settings.hookIds}
                workspaceId={workspaceId}
                onRescan={onRescanCapabilities}
                onReveal={onRevealCapabilityLocation}
                onChange={(hookIds) => update({ hookIds })}
                searchLabel={t("搜索钩子", "Search hooks")}
                emptyTitle={t("尚未发现钩子", "No hooks discovered")}
                emptyDescription={t(
                  "钩子写在 ~/.mework/hooks.json 或工作区的 .mework/hooks.json 里，保存后自动扫描。",
                  "Hooks live in ~/.mework/hooks.json or the workspace's .mework/hooks.json, and are re-scanned on save."
                )}
              />
            )}

            {view === "roles" && (
              <AgentRolesPage
                listId={listId("roles")}
                settings={settings}
                globalSettings={globalSettings}
                tools={tools}
                templates={templates}
                presets={presetOptions}
                onReadTemplate={onReadTemplate}
                onWriteTemplate={onWriteTemplate}
                onChange={update}
              />
            )}

            {/* The preset's own message queue, edited on the same surface a
                timeline is. Each edit is written back as it lands — the page
                carries no save of its own — and saving mints the id when the
                preset has none and binds it immediately: the body is already on
                disk by then, so deferring the binding to the dialog's Save would
                be a window in which abandoning the dialog stranded it. */}
            {view === "template" && (
              <ConversationTemplateEditor
                templateId={openTemplateId}
                contexts={templateBody}
                tools={tools}
                enabledTools={settings.enabledTools}
                editable={!builtinPreset}
                imageInputSupported={imageInputSupported}
                autosave
                onEnableTools={(names) => update({
                  enabledTools: [...new Set([...settings.enabledTools, ...names])]
                })}
                onSave={async (contexts) => {
                  const savedId = await onWriteTemplate(openTemplateId, contexts);
                  setTemplateBody(contexts);
                  if (presetId && savedId !== openTemplateId) {
                    onBindPresetTemplate?.(presetId, savedId);
                  }
                }}
              />
            )}

            {view === "presets" && (
              <ConversationPresetsPage
                listId={listId("presets")}
                presets={presetOptions}
                appliedId={appliedPresetId}
                error={presetError}
                onApply={onApplyPreset}
                onOpen={openPreset}
                onRename={(presetId, name) => onRenamePreset?.(presetId, name)}
                onDelete={(presetId) => onDeletePreset?.(presetId)}
              />
            )}
          </div>
        </div>
      </div>

      {/* A preset opens into this same pane rather than a second editor: they
          would otherwise have to be kept in step by hand forever. Its catalog
          pages list the same skills, servers and hooks the outer pane does, so
          they get the same delete: removing an entry here removes it from disk,
          not merely from the preset. */}
      {presetDraft && openedPreset && (
        <Dialog
          title={openedPreset.name || t("未命名预设", "Untitled preset")}
          width="1040px"
          bodyClassName="dialog__body--flush"
          onClose={() => setPresetDraft(null)}
        >
          <ConversationSettings
            mode="preset"
            presetId={presetDraft.id}
            conversation={{
              ...conversation,
              id: `preset:${presetDraft.id}`,
              presetId: "",
              /* The preset's own binding, read live rather than from the draft:
                 saving a body binds the id at once, so the draft is not where
                 that fact lives. */
              templateId: openedPreset.templateId,
              settings: presetDraft.settings
            }}
            globalSettings={globalSettings}
            tools={tools}
            capabilities={capabilities}
            onChange={(next) => setPresetDraft((current) => (
              current && { ...current, settings: next }
            ))}
            onChangeConversationOnly={(patch) => setPresetDraft((current) => (
              current && { ...current, settings: { ...current.settings, ...patch } }
            ))}
            onApplyPreset={onApplyPreset}
            onBindPresetTemplate={onBindPresetTemplate}
            templates={templates}
            onReadTemplate={onReadTemplate}
            onWriteTemplate={onWriteTemplate}
            onDeleteCapability={onDeleteCapability}
            capabilityError={capabilityError}
            /* A preset is reusable and points at no workspace in particular, so it
               is deliberately handed no `workspaceId`: the pages draw the whole
               catalog rather than narrowing to one conversation's level. */
            onRescanCapabilities={onRescanCapabilities}
            onRevealCapabilityLocation={onRevealCapabilityLocation}
            onProbeMcpServer={onProbeMcpServer}
            onSaveAsPreset={() => {
              const body = captureConversationPresetSettings(presetDraft.settings);
              if (isBuiltinConversationPreset(presetDraft.id)) onSavePresetCopy?.(presetDraft.id, body);
              else onSavePreset?.(presetDraft.id, body);
              setPresetDraft(null);
            }}
          />
        </Dialog>
      )}
    </div>
  );
}

/**
 * A preset body widened into the shape this pane edits.
 *
 * The fields a preset does not own are borrowed from the conversation so the
 * pane has something coherent to draw, and are dropped again by
 * `captureConversationPresetSettings` on save. `toolLock` is deliberately NOT
 * borrowed: a preset has run nothing and exposed nothing, so nothing in it is
 * spent — carrying the conversation's lock in would grey out switches that are
 * in fact still free to move.
 */
function presetBodyAsSettings(
  preset: ConversationPreset,
  conversationSettings: ConversationSettingsType
): ConversationSettingsType {
  return {
    ...preset.settings,
    includeAppDataPath: false,
    reasoningEffort: conversationSettings.reasoningEffort
  };
}
