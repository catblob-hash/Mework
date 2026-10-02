import {
  Blocks,
  Bot,
  Box,
  FileText,
  FolderCog,
  SlidersHorizontal,
  Wrench
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  captureConversationPresetSettings,
  isBuiltinConversationPreset
} from "../lib/conversationPresets";
import { modelChoiceOf } from "../lib/documentUpdates";
import { supportsVision } from "../lib/modelCapabilities";
import { acknowledgeCacheBreak, shouldWarnCacheBreak } from "../lib/cacheBreakWarning";
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
import { Dialog, useInSidebarDialog } from "./Common";
import { isImeKeyEvent } from "../lib/shortcuts";
import { editableUserAgentDefinition } from "../lib/agentDefinitions";
import { isHostDerivedToolName, isPreviewLifecycleToolName } from "../lib/taskTools";
import { familySelectsNativeToolType, familySupportsNativeFetch } from "../lib/webSearch";
import {
  backendPinned,
  backendTone,
  lockTone,
  lockTouch,
  toolLockModelOf,
  toolLockState,
  type LockedSettingKind,
  type WebBackendLeg
} from "../lib/toolLock";
import { ConversationTemplateEditor } from "./ConversationTemplateEditor";
import { AdvancedToolsPage } from "./AdvancedToolsPage";
import { DocsLink } from "./DocsLink";
import { LockableSwitchRow, lockHints as lockHintsOf, type BackendLock } from "./LockTone";
import { SettingsLayout, SettingsNavigation } from "./SettingsLayout";
import { SettingsPageHeading } from "./SettingsPageHeading";
import { ToolSelectionGroups } from "./ToolSelectionGroups";
import { HostedWindow } from "./WindowLayer";
import {
  AgentRolesPage,
  CapabilitySelectionPage,
  ConversationPresetsPage,
  settledBackendHint
} from "./ConversationSettingsPages";
import "./ConversationSettings.css";

/**
 * The pages the conversation-settings pane lists down its left edge.
 *
 * `tools` is the tool picker alone; `advanced` carries what is derived from a
 * switch rather than picked from that list. The others each own one kind of
 * thing the conversation composes with, so a page is never a mixed bag.
 */
type ConversationSettingsView =
  | "tools"
  | "advanced"
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
  { id: "tools", icon: Wrench },
  { id: "advanced", icon: SlidersHorizontal },
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
  /**
   * Saves an edited body of the built-in preset, which cannot change, as a new
   * preset. `templateBody` is the template as edited in the window, or null to
   * copy the built-in's own.
   */
  onSavePresetCopy?: (presetId: string, settings: ConversationPresetSettings, templateBody: ContextItem[] | null) => void;
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
   * In `conversation` mode, saves these components as a new independent preset,
   * opening its timeline as the preset's template when asked to, and resolves
   * with the preset once it is in the document. The pane then opens it in a
   * window at its template page.
   */
  onCreatePreset?: (request: { name: string; captureTemplate: boolean }) => Promise<ConversationPreset>;
  /**
   * In `preset` mode, saves the open preset in place — or, for the built-in one,
   * which cannot change, as a new preset. The copy takes the template body as it
   * was edited in this window, or null when it was not, in which case the copy
   * takes the built-in's own.
   */
  onSaveAsPreset?: (templateBody: ContextItem[] | null) => void;
  mode?: ConversationSettingsMode;
  /** In `preset` mode, the preset this pane is a window onto. */
  presetId?: string;
  /** The page the pane opens on. */
  initialView?: ConversationSettingsView;
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
  onCreatePreset,
  onSaveAsPreset,
  mode = "conversation",
  presetId,
  initialView = "tools"
}: ConversationSettingsProps) {
  const { t } = useI18n();
  const inWindow = useInSidebarDialog();
  const settings = conversation.settings;
  const [view, setView] = useState<ConversationSettingsView>(initialView);
  /* The preset currently open in a window of this pane, held as a whole
   * conversation-settings body rather than a preset body: the pane edits the
   * former, and narrowing back down to the latter is exactly what saving is. */
  const [presetDraft, setPresetDraft] = useState<
    { id: string; settings: ConversationSettingsType; view?: ConversationSettingsView } | null
  >(null);
  /* The open preset's template body, read once the user asks for that page. A
   * template is big enough that reading it on the chance the page is opened
   * would make opening a preset slower for everyone who never looks. */
  const [templateBody, setTemplateBody] = useState<ContextItem[] | null>(null);
  /* The built-in preset's template as edited in its window. The host refuses
   * every write to that template, so the edits are held here, outlive a trip to
   * another page, and go with the copy that saving makes. */
  const [heldTemplate, setHeldTemplate] = useState<ContextItem[] | null>(null);
  /* "Save as preset" asks for a name, and whether the conversation's own
   * timeline becomes the new preset's template. */
  const [saveAs, setSaveAs] = useState<{
    name: string;
    captureTemplate: boolean;
    saving: boolean;
    error: string | null;
  } | null>(null);
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
  /* The conversation's lock, read against the model selected now
   * (`toolLock.ts`): what its last request cached, whether that cache is still
   * warm, and whether this model freezes the tool surface outright. A preset
   * body carries no lock, so a preset window is never toned. */
  const lockModel = useMemo(() => {
    const { provider, model } = modelChoiceOf(globalSettings);
    return toolLockModelOf(provider, model);
  }, [globalSettings]);
  /* Ticks when the cache goes cold, so the orange goes with it. */
  const [lockClock, setLockClock] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `lockClock` is the moment to read the clock again, though nothing here reads it.
  const lockState = useMemo(
    () => toolLockState(settings, lockModel, Date.now()),
    [settings, lockModel, lockClock]
  );
  useEffect(() => {
    if (!lockState.warm || lockState.warmUntil === null) return;
    const timer = window.setTimeout(
      () => setLockClock((tick) => tick + 1),
      Math.max(0, lockState.warmUntil - Date.now()) + 250
    );
    return () => window.clearTimeout(timer);
  }, [lockState]);
  const lock = lockState.lock;
  const toneOf = (kind: LockedSettingKind, lastOn: boolean, nowOn: boolean | undefined) => (
    lockTone(lockState, kind, lastOn, Boolean(nowOn))
  );
  const lockHints = lockHintsOf(t);
  /* A change that would throw the warm cache away waits here for the one
   * warning every page shares; the patch, not the whole body, so a run that
   * lands meanwhile is not written over. The web backends are conversation-only
   * fields and are written the conversation-only way, so the patch remembers
   * which writer it belongs to. */
  const [cacheBreak, setCacheBreak] = useState<{
    patch: Partial<ConversationSettingsType>;
    conversationOnly: boolean;
  } | null>(null);
  const [cacheBreakNever, setCacheBreakNever] = useState(false);
  const commit = (patch: Partial<ConversationSettingsType>, conversationOnly: boolean) => {
    if (conversationOnly) onChangeConversationOnly(patch);
    else onChange({ ...settings, ...patch });
  };
  const guarded = (patch: Partial<ConversationSettingsType>, conversationOnly: boolean) => {
    const touch = lockTouch(lockState, settings, { ...settings, ...patch });
    if (touch.hard) return;
    if (touch.cache && shouldWarnCacheBreak(conversation.id)) {
      setCacheBreakNever(false);
      setCacheBreak({ patch, conversationOnly });
      return;
    }
    commit(patch, conversationOnly);
  };
  const update = (patch: Partial<ConversationSettingsType>) => guarded(patch, false);
  /* Whether this model can take a tool mid-conversation at all. Without it a
     fetched MCP schema would have to join the declared list, so discovery is
     not offered (the host ignores the setting too). */
  const discoveryAvailable = lockModel ? lockModel.appendsTools : true;
  const updateWebSearch = (patch: Partial<ConversationWebSearchSettings>) =>
    guarded({ webSearch: { ...settings.webSearch, ...patch } }, true);
  /* A native backend a run has used is settled for good; a host-run one is a
     cache lock like the switches around it (`backendTone`). */
  const backendLock = (leg: WebBackendLeg): BackendLock | null => {
    const tone = backendTone(
      lockState,
      leg,
      leg === "search" ? settings.webSearch.provider : settings.webSearch.fetchProvider
    );
    if (!tone) return null;
    const note = backendPinned(lock, leg) ? settledBackendHint(t) : lockHints[tone];
    return { tone, note };
  };
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
    tools: t("工具", "Tools"),
    advanced: t("高级工具", "Advanced tools"),
    skills: t("技能", "Skills"),
    mcp: "MCP",
    hooks: t("钩子", "Hooks"),
    roles: t("代理角色", "Agent roles"),
    template: t("对话模板", "Conversation template"),
    presets: t("对话预设", "Conversation presets")
  };
  /** The count each row trails, so the nav says what the conversation carries. */
  const pageCounts: Record<ConversationSettingsView, number | null> = {
    tools: validEnabledToolCount,
    advanced: null,
    skills: settings.skillIds.length,
    mcp: settings.mcpIds.length,
    hooks: settings.hookIds.length,
    roles: editableRoleCount,
    template: heldTemplate?.length ?? templates.find((item) => item.id === openTemplateId)?.messageCount ?? 0,
    presets: presetOptions.length
  };
  const pageBlurbs: Record<ConversationSettingsView, string> = {
    tools: t(
      "本对话交给模型的工具。",
      "The tools this conversation hands the model."
    ),
    advanced: t(
      "不在工具列表里逐个挑、而由开关派生的那几件事：联网、记忆与工具描述。",
      "What is derived from a switch rather than picked from the tool list: web access, memory and tool descriptions."
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

  /* The two tool pages are explained by one page of the site. The link rides on
   * the line that says what the page is for, at its far end, rather than taking
   * a row of its own above the list. */
  const pageDocs = view === "tools" || view === "advanced"
    ? <DocsLink page="features" />
    : undefined;

  /* The page itself, the same in the side pane and in a window: only the frame
   * around it differs. */
  const pageBody = (
    <>
      {view === "tools" && (
        <ToolSelectionGroups
          tools={tools}
          enabledTools={settings.enabledTools}
          toneOf={(name, enabled) => toneOf("tool", lock.tools.includes(name), enabled)}
          lockHints={lockHints}
          onChange={(enabledTools) => update({ enabledTools })}
          expansionKey={conversation.id}
        />
      )}

      {view === "advanced" && (
        <AdvancedToolsPage
          webAccess={{
            enabled: Boolean(settings.webSearchEnabled),
            tone: toneOf("webSearch", lock.webSearch, settings.webSearchEnabled),
            onChange: (webSearchEnabled) => update({ webSearchEnabled })
          }}
          web={{
            value: settings.webSearch,
            onChange: updateWebSearch,
            webSearchAssets: globalSettings.webSearch,
            nativeFetchAvailable,
            nativeToolTypeSelectable,
            searchLock: backendLock("search"),
            fetchLock: backendLock("fetch")
          }}
          memory={{
            global: Boolean(settings.globalMemoryEnabled),
            project: Boolean(settings.projectMemoryEnabled),
            globalTone: toneOf("memory", lock.globalMemory, settings.globalMemoryEnabled),
            projectTone: toneOf("memory", lock.projectMemory, settings.projectMemoryEnabled),
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
          lockHints={lockHints}
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
          toneOf={(id, selected) => toneOf("skill", lock.skillIds.includes(id), selected)}
          lockHints={lockHints}
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
             before deciding to install one. Moving it once skills have
             gone out rewrites the prompt they went out in. */
          footer={(
            <LockableSwitchRow
              title={t("技能按需加载", "Load skills on demand")}
              description={t(
                "关闭时已选技能的正文开局就拼进系统提示词。开启后改为暴露一个 skill 工具：模型先看到每个技能的名字与触发条件，需要哪一个才把正文取出来。",
                "When off, the selected skills' bodies are concatenated into the system prompt up front. When on, a skill tool is exposed instead: the model sees each skill's name and trigger, and pulls the body only for the one it needs."
              )}
              checked={Boolean(settings.skillToolEnabled)}
              tone={toneOf("skillTool", lock.skillTool, settings.skillToolEnabled)}
              hints={lockHints}
              onChange={(skillToolEnabled) => update({ skillToolEnabled })}
              label={settings.skillToolEnabled
                ? t("按需加载", "On demand")
                : t("拼进提示词", "In the prompt")}
            />
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
          toneOf={(id, selected) => toneOf("mcp", lock.mcpIds.includes(id), selected)}
          lockHints={lockHints}
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
             that is worth knowing before adding the first one. A model
             that cannot take a tool mid-conversation has no way to hand
             over a fetched schema, so it is not offered one. */
          footer={(
            <LockableSwitchRow
              title={t("工具发现", "Tool discovery")}
              description={t(
                "关闭时每个 MCP 工具的完整 schema 每一轮都随请求发出。开启后改为只报名字，并暴露一个 tool_search 工具：模型搜到需要的工具，才把它的 schema 取回来，取回之后就能直接调用。",
                "When off, every MCP tool's full schema goes out with every request. When on, only the names are announced and a tool_search tool is exposed: the model searches for the tool it needs, pulls that one's schema, and can then call it directly."
              )}
              checked={discoveryAvailable && Boolean(settings.mcpToolDiscoveryEnabled)}
              tone={toneOf("discovery", lock.mcpToolDiscovery, settings.mcpToolDiscoveryEnabled)}
              hints={lockHints}
              disabled={!discoveryAvailable}
              disabledNote={t(
                "当前模型不支持中途追加工具，取回的 schema 无处交付，所以不提供工具发现。",
                "The selected model cannot take a tool mid-conversation, so a fetched schema would have nowhere to go; tool discovery is not offered."
              )}
              onChange={(mcpToolDiscoveryEnabled) => update({ mcpToolDiscoveryEnabled })}
              label={settings.mcpToolDiscoveryEnabled
                ? t("按需取回", "On demand")
                : t("全部声明", "All declared")}
            />
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
          contexts={heldTemplate ?? templateBody}
          tools={tools}
          enabledTools={settings.enabledTools}
          imageInputSupported={imageInputSupported}
          autosave
          onEnableTools={(names) => update({
            enabledTools: [...new Set([...settings.enabledTools, ...names])]
          })}
          onSave={builtinPreset
            /* Nothing can be written to the built-in's template, so an edit is
               only ever kept for the copy. */
            ? async (contexts) => setHeldTemplate(contexts)
            : async (contexts) => {
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
    </>
  );
  /* The template page is a timeline, which brings its own scroller and its own
   * edges. Padding and a second scrollbar around one would fight it, so that page
   * gets the body flush. */
  const flush = view === "template";
  const pageStack = (
    <div className={flush
      ? "conversation-settings__page-stack conversation-settings__page-stack--flush"
      : "conversation-settings__page-stack"}>
      {pageBody}
    </div>
  );
  const saveLabel = editingPreset && !builtinPreset
    ? t("保存预设", "Save preset")
    : builtinPreset
      ? t("另存为新预设", "Save as new preset")
      : t("另存为预设", "Save as preset");
  const save = editingPreset
    ? () => onSaveAsPreset?.(builtinPreset ? heldTemplate : null)
    : () => setSaveAs({ name: "", captureTemplate: false, saving: false, error: null });
  /* A preset can only open with a timeline the conversation has. */
  const canCaptureTemplate = conversation.contexts.length > 0;
  const createPreset = async () => {
    if (!saveAs || saveAs.saving || !onCreatePreset) return;
    const name = saveAs.name.trim();
    if (!name) return;
    const captureTemplate = saveAs.captureTemplate && canCaptureTemplate;
    setSaveAs({ ...saveAs, saving: true, error: null });
    try {
      const preset = await onCreatePreset({ name, captureTemplate });
      setSaveAs(null);
      // Either way the new preset opens at its template: the timeline it took,
      // or an empty one to write.
      setPresetDraft({ id: preset.id, settings: presetBodyAsSettings(preset, settings), view: "template" });
    } catch (reason) {
      setSaveAs((current) => current && {
        ...current,
        saving: false,
        error: reason instanceof Error ? reason.message : String(reason)
      });
    }
  };

  const dialogs = (
    <>
      {/* The one warning every orange row shares: said once per conversation,
          or never again. The change waits here as a patch and lands on the
          settings as they are when the user answers. */}
      {cacheBreak && (
        <Dialog
          title={t("这样改会让缓存失效", "This change throws the cache away")}
          onClose={() => setCacheBreak(null)}
          width="440px"
          footer={(
            <>
              <button type="button" className="button" onClick={() => setCacheBreak(null)}>
                {t("取消", "Cancel")}
              </button>
              <button
                type="button"
                className="button button--primary"
                onClick={() => {
                  acknowledgeCacheBreak(conversation.id, cacheBreakNever);
                  const { patch, conversationOnly } = cacheBreak;
                  setCacheBreak(null);
                  commit(patch, conversationOnly);
                }}
              >{t("仍然更改", "Change anyway")}</button>
            </>
          )}
        >
          <p className="confirm-copy">{t(
            "橘色的设置是上一次请求带着的，它的提示缓存还没过期。改动其中任何一项，下一次请求就得把整段对话重新计费；在那之前改回原样，缓存仍然有效。",
            "The orange settings are what the last request went out with, and its prompt cache has not expired yet. Changing any of them makes the next request pay for the whole conversation again; change it back before then and the cache still holds."
          )}</p>
          <label className="confirm-check">
            <input
              type="checkbox"
              checked={cacheBreakNever}
              onChange={(event) => setCacheBreakNever(event.target.checked)}
            />
            <span>{t("不再显示", "Don't show this again")}</span>
          </label>
        </Dialog>
      )}

      {saveAs && (
        <Dialog
          title={t("另存为预设", "Save as preset")}
          onClose={() => setSaveAs(null)}
          footer={(
            <>
              <button type="button" className="button button--ghost" onClick={() => setSaveAs(null)}>
                {t("取消", "Cancel")}
              </button>
              <button
                type="button"
                className="button button--primary"
                disabled={!saveAs.name.trim() || saveAs.saving}
                onClick={() => void createPreset()}
              >
                {saveAs.saving ? t("正在保存…", "Saving…") : t("保存为预设", "Save as preset")}
              </button>
            </>
          )}
        >
          <label className="field">
            <span className="field__label">{t("预设名称", "Preset name")}</span>
            <input
              className="input"
              autoFocus
              value={saveAs.name}
              onChange={(event) => setSaveAs((current) => current && { ...current, name: event.target.value })}
              onKeyDown={(event) => {
                if (event.key !== "Enter" || isImeKeyEvent(event.nativeEvent)) return;
                event.preventDefault();
                void createPreset();
              }}
              placeholder={t("例如：代码评审", "e.g. Code review")}
            />
          </label>
          <label
            className="confirm-check"
            title={canCaptureTemplate ? undefined : t("这段对话还没有消息", "This conversation has no messages yet")}
          >
            <input
              type="checkbox"
              checked={saveAs.captureTemplate && canCaptureTemplate}
              disabled={!canCaptureTemplate}
              onChange={(event) => setSaveAs((current) => current && {
                ...current,
                captureTemplate: event.target.checked
              })}
            />
            <span>{t("将当前上下文作为对话模板", "Use the current context as the conversation template")}</span>
          </label>
          {saveAs.error && <p className="field__hint field__hint--error" role="alert">{saveAs.error}</p>}
        </Dialog>
      )}

      {/* A preset opens into this same pane rather than a second editor: they
          would otherwise have to be kept in step by hand forever. Its catalog
          pages list the same skills, servers and hooks the outer pane does, so
          they get the same delete: removing an entry here removes it from disk,
          not merely from the preset. */}
      {presetDraft && openedPreset && (
        <HostedWindow>
          <Dialog
            title={openedPreset.name || t("未命名预设", "Untitled preset")}
            width="1040px"
            sidebar
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
              initialView={presetDraft.view}
              onSaveAsPreset={(templateBody) => {
                const body = captureConversationPresetSettings(presetDraft.settings);
                if (isBuiltinConversationPreset(presetDraft.id)) onSavePresetCopy?.(presetDraft.id, body, templateBody);
                else onSavePreset?.(presetDraft.id, body);
                setPresetDraft(null);
              }}
            />
          </Dialog>
        </HostedWindow>
      )}
    </>
  );

  /* In a window — a preset opened from the presets page — the pane takes the
   * global settings' layout: the same rail and the same page. Saving belongs to
   * the whole window rather than to one page, so it sits at the foot of the rail,
   * as it does in the side pane. */
  if (inWindow) {
    return (
      <>
        <SettingsLayout
          label={editingPreset ? t("预设设置", "Preset settings") : t("对话设置", "Conversation settings")}
          navigation={(
            <SettingsNavigation
              label={t("对话设置分类", "Conversation settings categories")}
              groups={[{
                id: "pages",
                items: pages.map((item) => ({
                  id: item.id,
                  icon: item.icon,
                  label: pageTitles[item.id],
                  count: pageCounts[item.id]
                }))
              }]}
              view={view}
              onSelect={setView}
              footer={(
                <button type="button" className="settings-nav__action" onClick={save}>
                  {saveLabel}
                </button>
              )}
            />
          )}
        >
          <section className={flush ? "settings-page settings-editor-page" : "settings-page"}>
            {/* The rail names the page, so the page says only what it is for, as
                it does in the side pane. */}
            <SettingsPageHeading description={pageBlurbs[view]} action={pageDocs} />
            {builtinPreset && <p className="settings-page__note">{t(
              "内置预设随 Mework 版本更新，不能修改或删除。在这里做的改动，连同对话模板，可以另存为一份新预设。",
              "The built-in preset updates with Mework and cannot be edited or deleted. What you change here, the conversation template included, can be saved as a new preset."
            )}</p>}
            {pageStack}
          </section>
        </SettingsLayout>
        {dialogs}
      </>
    );
  }

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
              onClick={save}
            >{saveLabel}</button>
          </div>
        </div>
      </nav>

      <div className="conversation-settings__page">
        {/* The side pane's own header names it and the page; the blurb says what
            the page is for. */}
        <header className="conversation-settings__page-header">
          <div className="conversation-settings__page-header-line">
            <p>{pageBlurbs[view]}</p>
            {pageDocs}
          </div>
        </header>
        <div className={flush
          ? "conversation-settings__page-body conversation-settings__page-body--flush"
          : "conversation-settings__page-body"}>
          {pageStack}
        </div>
      </div>
      {dialogs}
    </div>
  );
}

/**
 * A preset body widened into the shape this pane edits.
 *
 * The fields a preset does not own are borrowed from the conversation so the
 * pane has something coherent to draw, and are dropped again by
 * `captureConversationPresetSettings` on save. `toolLock` is deliberately NOT
 * borrowed: a preset has run nothing and cached nothing — carrying the
 * conversation's lock in would tone switches orange or gray that are in fact
 * free to move.
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
