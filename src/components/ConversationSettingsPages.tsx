import {
  Blocks,
  Box,
  CornerDownLeft,
  FolderCog,
  FolderGit2,
  Home,
  Pencil,
  Plug,
  RefreshCw,
  Search,
  Settings2,
  SlidersHorizontal
} from "lucide-react";
import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { type TranslationFunction, useI18n } from "../i18n";
import { hasUsableAgentDefinition } from "../lib/agentDefinitions";
import { useCatalogOrder } from "../lib/catalogOrder";
import { modelChoiceOf } from "../lib/documentUpdates";
import { supportsVision } from "../lib/modelCapabilities";
import type {
  ContextItem,
  ConversationPreset,
  ConversationSettings as ConversationSettingsType,
  ConversationTemplateSummary,
  GlobalSettings,
  McpProbeReport,
  ResourceDescriptor,
  ToolDescriptor
} from "../types";
import { AgentDefinitionSettings } from "./AgentDefinitionSettings";
import { CatalogList, CatalogRow, CatalogToggleRow, useCatalogSort } from "./CatalogRow";
import { ConfirmDeleteButton, IconButton, Switch } from "./Common";
import { DocsLink } from "./DocsLink";

/** Said of every control a run has already handed the model and cannot take back. */
export function lockedInThisConversationHint(t: TranslationFunction): string {
  return t(
    "已经交给过模型，本对话里不能再关掉。",
    "The model has already been handed this; it cannot be taken back in this conversation."
  );
}

/**
 * Said of a backend a run has already used.
 *
 * Not the same claim as the hint above: nothing is being withheld, the choice
 * is simply settled. The transcript holds results only this backend could have
 * produced — a native search seals them in blocks only its own provider's
 * models can read back — so a different one cannot take over half way through.
 */
export function settledBackendHint(t: TranslationFunction): string {
  return t(
    "本对话已经用这个后端跑过了；转录里的结果只有它能被回放，所以不能中途换人。开一段新对话可以重选。",
    "This conversation has already run on this backend. The results in its transcript can only be replayed against it, so it cannot be swapped part-way through — start a new conversation to choose again."
  );
}

const KIND_ICONS = {
  skills: Box,
  mcp: Blocks,
  hooks: FolderCog
} as const;

/**
 * Everything a row no longer prints, joined into the tooltip it now rides in.
 *
 * A catalog row is one line, so a description, a path on disk, and a reason the
 * entry is inert all have to share `title`. They are still the only way to tell
 * two same-named resources apart, which is why they are kept at all rather than
 * dropped with the second line they used to occupy.
 */
function rowDetail(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join("\n");
}

const resourceId = (resource: ResourceDescriptor) => resource.id;

/**
 * What a connection test on an MCP row currently knows.
 *
 * Held as a whole per resource id so a re-test replaces the last result rather
 * than layering onto it: a row shows one thing at a time.
 */
type McpProbeState =
  | { status: "running" }
  | { status: "ok"; toolCount: number; detail: string }
  | { status: "failed"; detail: string };

/**
 * One page of the conversation-settings pane for a catalog of named things —
 * skills, MCP servers, or hooks.
 *
 * The three catalogs differ only in what they are called and which of them a run
 * can lock, so they share one page rather than three near-copies. Each entry is
 * a single line carrying one switch on the left and, where the entry is the
 * user's own to remove, the two-step delete on the right. The rows can be dragged
 * into whatever order the user reads them in — see `useCatalogOrder` for why that
 * order is a view preference rather than part of the catalog.
 *
 * A row states its name and nothing about where it came from: every scope badge a
 * row used to print said the same thing its path already says, and the path is in
 * the tooltip.
 *
 * The catalog is the whole discovery: `workspaceId` narrows it to the entries a
 * conversation may actually select — the global level plus its own workspace.
 * Undefined means no narrowing at all, which is what the preset editor wants: a
 * preset is reusable, so it sees everything there is.
 */
export function CapabilitySelectionPage({
  kind,
  listId,
  resources,
  selectedIds,
  lockedIds = [],
  onChange,
  onDelete,
  error = null,
  searchLabel,
  emptyTitle,
  emptyDescription,
  header = null,
  footer = null,
  workspaceId,
  onRescan,
  onReveal,
  onProbeMcpServer
}: {
  kind: keyof typeof KIND_ICONS;
  /** Unique among the lists on screen: the preset editor mounts a second copy. */
  listId: string;
  resources: ResourceDescriptor[];
  selectedIds: string[];
  /** Selections a run has already exposed, drawn spent rather than removable. */
  lockedIds?: readonly string[];
  onChange: (ids: string[]) => void;
  /**
   * Removes the entry from the catalog itself — the skill folder, the server
   * record, the line in `hooks.json` — not merely from this conversation. Omit
   * it where the caller has nowhere to route that.
   */
  onDelete?: (resource: ResourceDescriptor) => void;
  /** The last failed delete, shown on the page the row is on. */
  error?: string | null;
  searchLabel: string;
  emptyTitle: string;
  emptyDescription: string;
  /** Page-specific controls above the list, such as a delivery-mode switch. */
  header?: ReactNode;
  /**
   * Page-specific controls below the list, for a policy that governs the whole
   * catalog rather than one entry. Drawn whether or not the catalog has
   * anything in it: a policy that only appears once a row exists is a policy
   * the user cannot find out about beforehand.
   */
  footer?: ReactNode;
  /**
   * The conversation's workspace, for narrowing the catalog. A string keeps the
   * global entries and that workspace's; `null` (a draft with no workspace) keeps
   * the global entries alone; undefined shows everything (the preset editor).
   */
  workspaceId?: string | null;
  /** Re-runs discovery on disk. Drawn as a toolbar button when provided. */
  onRescan?: () => void | Promise<void>;
  /**
   * Opens a level's configuration folder. `null` is the global `~/.mework`; the
   * workspace button only exists when the page has a workspace to open.
   */
  onReveal?: (kind: "skills" | "mcp" | "hooks", workspaceId: string | null) => void;
  /** Tests an MCP server's connection. Only meaningful on the MCP page. */
  onProbeMcpServer?: (resource: ResourceDescriptor) => Promise<McpProbeReport>;
}) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  /* One result per resource id, replaced wholesale by the next test. */
  const [probes, setProbes] = useState<Record<string, McpProbeState>>({});
  const Icon = KIND_ICONS[kind];
  const locked = useMemo(() => new Set(lockedIds), [lockedIds]);
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  /* The entries this conversation may select: the global level (no workspace) and
     its own workspace. Anything else belongs to a sibling workspace and is not
     offered here — the host would skip such a selection at run time anyway. */
  const scoped = useMemo(
    () => (workspaceId === undefined
      ? resources
      : resources.filter((resource) => !resource.workspaceId || resource.workspaceId === workspaceId)),
    [resources, workspaceId]
  );
  const { ordered, reorder } = useCatalogOrder(kind, scoped, resourceId);
  /* A selection whose resource has left the catalog is kept, not dropped: the
     scan may simply not have reached it yet, so it stays as a checked row the
     user can clear on purpose. */
  const danglingIds = useMemo(() => {
    const known = new Set(scoped.map((resource) => resource.id));
    return selectedIds.filter((id) => !known.has(id));
  }, [scoped, selectedIds]);

  const needle = query.trim().toLowerCase();
  const matches = (haystack: string) => haystack.toLowerCase().includes(needle);
  const visible = needle
    ? ordered.filter((resource) => (
      matches(resource.name) || matches(resource.description) || matches(resource.location)
    ))
    : ordered;
  const visibleDangling = needle ? danglingIds.filter(matches) : danglingIds;
  /* Reordering a filtered list makes adjacent positions globally ambiguous, and
     a dangling row has no place in the catalog to be dragged to. */
  const sort = useCatalogSort({
    listId,
    ids: visible.map(resourceId),
    enabled: !needle,
    onReorder: reorder
  });

  const toggle = (id: string, checked: boolean) => {
    if (!checked && locked.has(id)) return;
    onChange(checked
      ? [...selectedIds.filter((existing) => existing !== id), id]
      : selectedIds.filter((existing) => existing !== id));
  };

  const probeServer = async (resource: ResourceDescriptor) => {
    if (!onProbeMcpServer) return;
    setProbes((current) => ({ ...current, [resource.id]: { status: "running" } }));
    let next: McpProbeState;
    try {
      const report = await onProbeMcpServer(resource);
      next = report.ok
        ? {
          status: "ok",
          toolCount: report.tools.length,
          /* The row is one line, so who answered has to ride the tooltip. */
          detail: [report.serverName, report.serverVersion].filter(Boolean).join(" ")
        }
        : {
          status: "failed",
          /* A failed probe says why, and then what the server said while it tried:
             the last few stderr lines are usually the whole diagnosis. */
          detail: [report.error, ...report.logs.slice(-5)].filter(Boolean).join("\n")
        };
    } catch (reason) {
      next = { status: "failed", detail: String(reason) };
    }
    setProbes((current) => ({ ...current, [resource.id]: next }));
  };

  const lockedNote = t(
    "已经交给过模型，本对话里不能再移除",
    "Already handed to the model; it cannot be removed in this conversation"
  );
  /* A missing selection is not one thing: skills and MCP are skipped at run time,
     while a hook that is gone fails the run outright. The row says which. */
  const danglingDetail = kind === "hooks"
    ? t(
      "目录中已不存在；运行时会报错，取消勾选可移除",
      "No longer in the catalog. Runs fail until it is unchecked."
    )
    : t(
      "目录中已不存在；运行时跳过，取消勾选可移除",
      "No longer in the catalog. Skipped at run time; uncheck to remove."
    );

  return (
    <>
      {header}
      {error && <p className="field__hint field__hint--error">{error}</p>}

      <div className="capability-page__toolbar">
        <div className="capability-page__search">
          <Search size={13} aria-hidden="true" />
          <input
            aria-label={searchLabel}
            placeholder={searchLabel}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
        <span className="capability-page__count">{t(
          "{selected} / {total} 个已选",
          "{selected} / {total} selected",
          {
            /* Counted over the same population it divides by. A dangling selection is
               still a real row below, but counting it here would read "2 / 1". */
            selected: scoped.filter((resource) => selected.has(resource.id)).length,
            total: scoped.length
          }
        )}</span>
        {/* Where the catalog came from, and how to look at it again: re-read the
            disk, or open the folder it is written in. Kept as one cluster so the
            pane's floor breaks the line between controls, never inside one. */}
        <div className="capability-page__toolbar-actions">
          {onRescan && (
            <IconButton
              label={t("重新扫描", "Rescan")}
              onClick={() => void onRescan()}
            ><RefreshCw size={13} /></IconButton>
          )}
          {onReveal && (
            <IconButton
              label={t("打开全局配置目录", "Open the global config folder")}
              onClick={() => onReveal(kind, null)}
            ><Home size={13} /></IconButton>
          )}
          {onReveal && workspaceId ? (
            <IconButton
              label={t("打开工作区配置目录", "Open this workspace's config folder")}
              onClick={() => onReveal(kind, workspaceId)}
            ><FolderGit2 size={13} /></IconButton>
          ) : null}
        </div>
        <DocsLink page={kind} />
      </div>

      <CatalogList sort={sort}>
        {visible.map((resource) => {
          const isLocked = locked.has(resource.id);
          const isSelected = selected.has(resource.id);
          const probe = probes[resource.id];
          /* The badge is one slot with one meaning: what a test just said, or —
             where there is no test to report — that the entry cannot run. An
             unavailable row is never probed, so the two never compete. */
          const badge = probe?.status === "running"
            ? <em className="catalog-row__badge">{t("测试中…", "Testing…")}</em>
            : probe?.status === "ok"
              ? <em className="catalog-row__badge">{t(
                "{count} 个工具",
                "{count} tools",
                { count: probe.toolCount }
              )}</em>
              : probe?.status === "failed"
                ? <em className="catalog-row__badge catalog-row__badge--warning">{t("连接失败", "Connection failed")}</em>
                : !resource.available
                  ? <em className="catalog-row__badge catalog-row__badge--warning">{t("不可用", "Unavailable")}</em>
                  : undefined;
          const actions: ReactNode[] = [];
          /* A connection test is only offered where there is something to connect
             to: an unavailable entry already carries the reason it cannot. */
          if (kind === "mcp" && onProbeMcpServer && resource.available && !isLocked) {
            actions.push(
              <IconButton
                key="probe"
                /* Named after its row, like the delete button beside it: a list of
                   identical "Test connection" buttons has no accessible name. */
                label={t("测试连接 {name}", "Test connection to {name}", { name: resource.name })}
                disabled={probe?.status === "running"}
                onClick={() => void probeServer(resource)}
              ><Plug size={13} /></IconButton>
            );
          }
          /* A built-in entry has no copy of its own to remove, and a locked one is
             already in the model's hands for this conversation. */
          if (onDelete && resource.source !== "builtin" && !isLocked) {
            actions.push(
              <ConfirmDeleteButton
                key="delete"
                label={t("删除 {name}", "Delete {name}", { name: resource.name })}
                confirmLabel={t("确认删除 {name}", "Confirm deleting {name}", { name: resource.name })}
                onDelete={() => onDelete(resource)}
              />
            );
          }
          return (
            <CatalogToggleRow
              key={resource.id}
              id={resource.id}
              sort={sort}
              name={resource.name}
              detail={rowDetail(
                resource.description,
                resource.location,
                isLocked && lockedNote,
                /* The scope badge is gone, so the one thing it said that the path
                   does not say moves here. */
                !resource.available && t("当前不可用", "Currently unavailable"),
                !isLocked && !resource.available && isSelected && t(
                  "已选择，当前不会生效",
                  "Selected, currently inactive"
                ),
                probe?.status === "ok" && probe.detail,
                probe?.status === "failed" && probe.detail
              )}
              /* A probe result and an unavailable entry both ride this slot; they
                 are mutually exclusive because an unavailable row is not probed. */
              badge={badge}
              actions={actions.length ? actions : undefined}
              checked={isSelected}
              disabled={isLocked}
              onChange={(checked) => toggle(resource.id, checked)}
            />
          );
        })}

        {visibleDangling.map((id) => (
          <CatalogToggleRow
            key={id}
            name={id}
            detail={danglingDetail}
            badge={<em className="catalog-row__badge catalog-row__badge--warning">{t("悬空", "Dangling")}</em>}
            checked
            disabled={locked.has(id)}
            onChange={(checked) => toggle(id, checked)}
          />
        ))}

        {!visible.length && !visibleDangling.length && (
          <div className="capability-page__empty">
            {needle
              ? <>
                <Search size={18} aria-hidden="true" />
                <strong>{t("没有匹配的条目", "No matching entry")}</strong>
                <span>{t(
                  "搜索会匹配名称、说明与所在路径。",
                  "Search matches names, descriptions and paths."
                )}</span>
              </>
              : <>
                <Icon size={18} aria-hidden="true" />
                <strong>{emptyTitle}</strong>
                <span>{emptyDescription}</span>
              </>}
          </div>
        )}
      </CatalogList>
      {footer}
    </>
  );
}

/**
 * The agent-roles page.
 *
 * The role list is the page; the one policy that governs all of them — whether a
 * subagent may decline to name a role at all — sits below the list's add button,
 * read after the roles it qualifies, and is always drawn so the policy is legible
 * even when no role exists yet.
 */
export function AgentRolesPage({
  listId,
  settings,
  globalSettings,
  tools,
  templates,
  presets,
  onReadTemplate,
  onWriteTemplate,
  onChange
}: {
  /** Unique among the lists on screen: the preset editor mounts a second copy. */
  listId: string;
  settings: ConversationSettingsType;
  globalSettings: GlobalSettings;
  tools: ToolDescriptor[];
  templates: ConversationTemplateSummary[];
  /** Drawn read-only beside a role's own template, for comparison. */
  presets: readonly ConversationPreset[];
  onReadTemplate: (templateId: string) => Promise<ContextItem[]>;
  onWriteTemplate: (templateId: string, contexts: ContextItem[]) => Promise<string>;
  onChange: (patch: Partial<ConversationSettingsType>) => void;
}) {
  const { t } = useI18n();
  /* Unlike the visible-role count, this asks whether the model can select a
   * role at all, including read-only project and plugin roles. */
  const hasUsableRole = useMemo(
    () => hasUsableAgentDefinition(settings.agentDefinitions, globalSettings.apiProviders),
    [settings.agentDefinitions, globalSettings.apiProviders]
  );
  /* What a role bound to "inherit" will run on, so its template window knows
     whether a message written there may carry an image. */
  const conversationImageInputSupported = useMemo(() => {
    const model = modelChoiceOf(globalSettings).model;
    return Boolean(model && supportsVision(model));
  }, [globalSettings]);

  return (
    <>
      {/* Preset components are copied into this conversation and never flow back
          to the preset. Roles are selected by name and bound to models here. */}
      <AgentDefinitionSettings
        definitions={settings.agentDefinitions}
        listId={listId}
        providers={globalSettings.apiProviders}
        tools={tools}
        conversationEnabledTools={settings.enabledTools}
        conversationImageInputSupported={conversationImageInputSupported}
        webSearchAssets={globalSettings.webSearch}
        templates={templates}
        presets={presets}
        onReadTemplate={onReadTemplate}
        onWriteTemplate={onWriteTemplate}
        onEnableTools={(names) => onChange({
          enabledTools: [...new Set([...settings.enabledTools, ...names])]
        })}
        onChange={(agentDefinitions) => onChange({ agentDefinitions })}
      />

      {/* Below the list's add button, where the reader lands after the roles
          themselves: with no usable role the host allows role-less execution
          regardless, so the switch reads as a statement of intent rather than a
          live control — but it stays on screen so the policy is never invisible. */}
      <div className="tool-toggle-row">
        <span><strong>{t("允许无角色子代理", "Allow role-less subagents")}</strong><small>{hasUsableRole
          ? t(
            "关闭时 agent_spawn 与 workflow 步骤都必须点名一个角色。开启则恢复旧语义：省略角色的子代理沿用本对话的模型。",
            "When off, agent_spawn and workflow steps must both name a role. When on, the old behaviour returns: a subagent that names none inherits this conversation's model."
          )
          : t(
            "当前没有可用角色，宿主一律允许无角色执行，这个开关要等到有可用角色后才生效。",
            "No usable role exists, so the host allows role-less execution either way; this switch takes effect once one does."
          )}</small></span>
        <Switch
          checked={Boolean(settings.allowRolelessSubagents)}
          onChange={(allowRolelessSubagents) => onChange({ allowRolelessSubagents })}
          label={settings.allowRolelessSubagents
            ? t("角色可选", "Role optional")
            : t("角色必填", "Role required")}
        />
      </div>
    </>
  );
}

const presetId = (preset: ConversationPreset) => preset.id;

/**
 * The conversation-presets page.
 *
 * A preset is a saved copy of everything else in this pane, so its settings
 * button opens this same pane on that copy rather than a second, smaller editor
 * that would have to be kept in step with it. The arrow in the left slot stamps
 * the copy onto this conversation and is a one-time action, which is why the
 * applied row is marked rather than tethered — see the preset trace on the
 * conversation itself.
 */
export function ConversationPresetsPage({
  listId,
  presets,
  appliedId,
  editable,
  error = null,
  onApply,
  onOpen,
  onRename,
  onDelete
}: {
  /** Unique among the lists on screen: the preset editor mounts a second copy. */
  listId: string;
  presets: ConversationPreset[];
  appliedId: string;
  /** False for the implicit built-in shown when nothing is saved: it has no row on disk. */
  editable: boolean;
  /** The last failed apply. Applying is the only action here that can fail. */
  error?: string | null;
  onApply: (id: string) => void;
  onOpen: (id: string) => void;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
}) {
  const { t } = useI18n();
  const [renaming, setRenaming] = useState<{ id: string; original: string; name: string } | null>(null);
  const { ordered, reorder } = useCatalogOrder("presets", presets, presetId);
  const sort = useCatalogSort({ listId, ids: ordered.map(presetId), onReorder: reorder });

  /* Renaming commits by leaving the field, the way the sidebar renames a
   * conversation and the way every other edit in this application is kept: there
   * is no Save button to forget to press. A name emptied or left alone is not a
   * rename, so it closes without writing rather than storing a blank. */
  const finishRename = () => {
    if (!renaming) return;
    const name = renaming.name.trim();
    if (name && name !== renaming.original) onRename(renaming.id, name);
    setRenaming(null);
  };

  return (
    <>
      {error && <p className="field__hint field__hint--error">{error}</p>}
      <CatalogList sort={sort}>
        {ordered.map((preset) => {
          const isApplied = preset.id === appliedId;
          const isRenaming = renaming?.id === preset.id;
          return (
            <CatalogRow
              key={preset.id}
              id={preset.id}
              sort={sort}
              name={preset.name || t("未命名预设", "Untitled preset")}
              on={isApplied}
              detail={rowDetail(
                preset.description,
                !editable && t(
                  "内置默认值，没有保存成一份预设，所以不能改名或删除。",
                  "The built-in default. It is not a saved preset, so it cannot be renamed or deleted."
                )
              )}
              icon={<SlidersHorizontal size={13} aria-hidden="true" />}
              nameEditor={isRenaming ? (
                <input
                  className="input"
                  autoFocus
                  aria-label={t("重命名预设 {name}", "Rename preset {name}", {
                    name: renaming.original || t("未命名预设", "Untitled preset")
                  })}
                  value={renaming.name}
                  onChange={(event) => setRenaming((current) => (
                    current && { ...current, name: event.target.value }
                  ))}
                  onFocus={(event) => event.currentTarget.select()}
                  onBlur={finishRename}
                  onKeyDown={(event) => {
                    if (event.nativeEvent.isComposing) return;
                    if (event.key === "Enter") {
                      event.preventDefault();
                      event.currentTarget.blur();
                    } else if (event.key === "Escape") {
                      event.preventDefault();
                      setRenaming(null);
                    }
                  }}
                />
              ) : undefined}
              lead={(
                <IconButton
                  label={t("套用", "Apply")}
                  onClick={() => onApply(preset.id)}
                ><CornerDownLeft size={13} /></IconButton>
              )}
              actions={<>
                <IconButton
                  label={t("重命名", "Rename")}
                  /* Held down while its own field is open, the way the sidebar
                     holds a conversation's rename button down: the button that
                     opened the field has nothing left to do until it closes. */
                  disabled={!editable || isRenaming}
                  onClick={() => setRenaming({
                    id: preset.id,
                    original: preset.name,
                    name: preset.name
                  })}
                ><Pencil size={13} /></IconButton>
                <IconButton
                  label={t("打开预设 {name}", "Open preset {name}", { name: preset.name })}
                  onClick={() => onOpen(preset.id)}
                ><Settings2 size={13} /></IconButton>
                <ConfirmDeleteButton
                  label={t("删除预设 {name}", "Delete preset {name}", { name: preset.name })}
                  confirmLabel={t("确认删除预设 {name}", "Confirm deleting preset {name}", { name: preset.name })}
                  disabled={!editable}
                  onDelete={() => onDelete(preset.id)}
                />
              </>}
            />
          );
        })}
      </CatalogList>
      <p className="capability-page__hint">{t(
        "行首的箭头把预设的内容复制到本对话，之后改哪一边都不影响另一边；右边的设置按钮在窗口里打开那份预设。拖动整行可以排序。",
        "The arrow at the head of a row copies that preset's contents into this conversation; editing either one afterwards does not affect the other. The settings button on the right opens that preset in a window. Drag a row to reorder."
      )}</p>
    </>
  );
}
