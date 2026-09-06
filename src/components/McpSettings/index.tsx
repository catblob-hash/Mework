import {
  ArrowLeft,
  Check,
  ChevronDown,
  ExternalLink,
  Filter,
  Plus,
  Search,
  Server,
  ShoppingBag,
  Trash2,
  X
} from "lucide-react";
import type { JSX } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import type { TranslationFunction } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import { cancelMcpProbe, probeMcpServer } from "../../lib/runtime";
import type { McpProbeReport, McpServerConfig } from "../../types";
import { Dialog, IconButton, Switch } from "../Common";
import { findReorderDropTarget, reorderItems, usePointerDrag } from "../usePointerDrag";
import type { ReorderDropTarget } from "../usePointerDrag";
import {
  McpLogsSection,
  McpPromptsSection,
  McpResourcesSection,
  McpToolsSection
} from "./McpDetailSections";
import { McpMarketList } from "./McpMarketList";
import { McpServerForm } from "./McpServerForm";
import type { TextDraft } from "./McpServerForm";
import { makeServer, parseServerImport } from "./mcpImport";
import type { McpImportError } from "./mcpImport";
import "./McpSettings.css";

/**
 * MCP settings page.
 *
 * The page has secondary navigation and a content area. MCP server details persist every input
 * directly; the footer uses connection testing rather than a redundant Save action.
 */

type McpView = "servers" | "market" | "detail";
type ServerFilter = "all" | "enabled" | "disabled" | "stdio" | "streamable_http";
type DetailTab = "general" | "tools" | "prompts" | "resources" | "logs";
type ProbeStatus =
  | { kind: "idle" }
  | { kind: "probing"; probeId: string }
  | { kind: "connected" }
  | { kind: "error"; error: string };

const EMPTY_STATUS: ProbeStatus = { kind: "idle" };
const SORTABLE_LIST_ID = "mcp-servers";

/** Probe ids only have to be unique among live probes; the host bounds their shape. */
let probeSequence = 0;
function nextProbeId(): string {
  probeSequence += 1;
  return `probe-${Date.now().toString(36)}-${probeSequence.toString(36)}`;
}

function statusLabel(status: ProbeStatus, enabled: boolean, t: TranslationFunction): string {
  if (!enabled) return t("已禁用", "Disabled");
  if (status.kind === "probing") return t("连接中", "Connecting");
  if (status.kind === "connected") return t("已连接", "Connected");
  if (status.kind === "error") return t("错误", "Error");
  return t("未探测", "Not probed");
}

function statusClass(status: ProbeStatus, enabled: boolean): string {
  if (!enabled) return "disabled";
  if (status.kind === "probing") return "connecting";
  if (status.kind === "connected") return "connected";
  if (status.kind === "error") return "error";
  return "idle";
}

export function McpSettings({
  servers,
  onChange
}: {
  servers: McpServerConfig[];
  onChange: (servers: McpServerConfig[]) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [view, setView] = useState<McpView>("servers");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [filter, setFilter] = useState<ServerFilter>("all");
  const [filterOpen, setFilterOpen] = useState(false);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [importText, setImportText] = useState("");
  const [importErrors, setImportErrors] = useState<McpImportError[]>([]);
  const [tab, setTab] = useState<DetailTab>("general");
  const [toolQuery, setToolQuery] = useState("");
  const [toolSearchOpen, setToolSearchOpen] = useState(false);
  const [statuses, setStatuses] = useState<Record<string, ProbeStatus>>({});
  const [reports, setReports] = useState<Record<string, McpProbeReport>>({});
  const [drafts, setDrafts] = useState<Record<string, TextDraft>>({});
  const addMenuRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const mountedRef = useRef(true);
  // The probe each server currently owns. A result whose id is no longer here was
  // cancelled, so it must not land on the page.
  const probesRef = useRef(new Map<string, string>());
  const backendAvailable = hasBackendRuntime();

  const selected = servers.find((server) => server.id === selectedId) ?? null;
  const selectedStatus = selected ? statuses[selected.id] ?? EMPTY_STATUS : EMPTY_STATUS;
  const selectedReport = selected ? reports[selected.id] : undefined;

  useEffect(() => {
    mountedRef.current = true;
    const probes = probesRef.current;
    return () => {
      mountedRef.current = false;
      // Leaving the page ends every probe it started: a stdio probe owns a child
      // process, and nothing here can present its result any more. There is no
      // surface left on which a refusal could be shown.
      for (const probeId of probes.values()) {
        void cancelMcpProbe(probeId).catch(() => undefined);
      }
      probes.clear();
    };
  }, []);

  // Return to the list if the selected server disappears. Depend on stable IDs rather than the
  // parent-created array, which changes identity on every render.
  const serverIds = servers.map((server) => server.id).join("::mcp-id::");
  useEffect(() => {
    if (!selectedId) return;
    if (serverIds.split("::mcp-id::").includes(selectedId)) return;
    setSelectedId(null);
    setView("servers");
  }, [selectedId, serverIds]);

  useEffect(() => {
    if (!searchOpen) return;
    searchInputRef.current?.focus();
  }, [searchOpen]);

  useEffect(() => {
    if (!addMenuOpen && !filterOpen) return;
    const close = (event: MouseEvent) => {
      const target = event.target as Node;
      if (addMenuOpen && !addMenuRef.current?.contains(target)) setAddMenuOpen(false);
      if (filterOpen && !filterRef.current?.contains(target)) setFilterOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setAddMenuOpen(false);
      setFilterOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [addMenuOpen, filterOpen]);

  const counts: Record<ServerFilter, number> = {
    all: servers.length,
    enabled: servers.filter((server) => server.enabled).length,
    disabled: servers.filter((server) => !server.enabled).length,
    stdio: servers.filter((server) => server.transport === "stdio").length,
    streamable_http: servers.filter((server) => server.transport === "streamable_http").length
  };
  const filterLabels: Record<ServerFilter, string> = {
    all: t("全部", "All"),
    enabled: t("已启用", "Enabled"),
    disabled: t("已停用", "Disabled"),
    stdio: t("STDIO", "STDIO"),
    streamable_http: t("流式", "Streamable")
  };
  const filterOrder: ServerFilter[] = ["all", "enabled", "disabled", "stdio", "streamable_http"];

  const needle = query.trim().toLowerCase();
  const visibleServers = useMemo(() => servers.filter((server) => {
    if (filter === "enabled" && !server.enabled) return false;
    if (filter === "disabled" && server.enabled) return false;
    if (filter === "stdio" && server.transport !== "stdio") return false;
    if (filter === "streamable_http" && server.transport !== "streamable_http") return false;
    if (!needle) return true;
    const haystack = `${server.name} ${server.description} ${server.tags.join(" ")} ${server.provider}`.toLowerCase();
    return needle.split(/\s+/).filter(Boolean).every((word) => haystack.includes(word));
  }), [filter, needle, servers]);

  // Reordering while rows are filtered makes adjacent positions globally ambiguous.
  const reorderable = !needle && filter === "all";
  const serverSort = usePointerDrag<string, ReorderDropTarget>({
    getTarget: (point, serverId) => findReorderDropTarget(SORTABLE_LIST_ID, serverId, point),
    onDrop: (serverId, target) => {
      onChange(reorderItems(servers, serverId, target.id, target.position, (server) => server.id)
        .map((server, index) => ({ ...server, sortOrder: index })));
    }
  });
  const moveByKeyboard = (serverId: string, direction: -1 | 1) => {
    const index = servers.findIndex((server) => server.id === serverId);
    const neighbour = servers[index + direction];
    if (!neighbour) return;
    onChange(reorderItems(
      servers,
      serverId,
      neighbour.id,
      direction < 0 ? "before" : "after",
      (server) => server.id
    ).map((server, position) => ({ ...server, sortOrder: position })));
  };

  const replaceServer = (serverId: string, update: (server: McpServerConfig) => McpServerConfig) => {
    const now = new Date().toISOString();
    // Document updates persist automatically. Local text drafts preserve incomplete multiline and
    // KEY=VALUE input until it can become a server field.
    onChange(servers.map((server) => (server.id === serverId
      ? { ...update(server), updatedAt: now }
      : server)));
  };

  const setEnabled = (serverId: string, enabled: boolean) => {
    replaceServer(serverId, (server) => ({ ...server, enabled }));
  };

  const openServer = (serverId: string) => {
    setSelectedId(serverId);
    setTab("general");
    setToolQuery("");
    setToolSearchOpen(false);
    setView("detail");
  };

  const addServer = () => {
    const next = makeServer(
      Math.max(-1, ...servers.map((server) => server.sortOrder)) + 1,
      t("MCP 服务器", "MCP server")
    );
    onChange([...servers, next]);
    setAddMenuOpen(false);
    openServer(next.id);
  };

  const removeServer = (server: McpServerConfig) => {
    if (!window.confirm(t("确定要删除此服务器吗？", "Delete this server?"))) return;
    // A deleted server's probe has nowhere to report to, so end it with the server.
    const probeId = probesRef.current.get(server.id);
    if (probeId) {
      probesRef.current.delete(server.id);
      void cancelMcpProbe(probeId).catch(() => undefined);
    }
    onChange(servers.filter((item) => item.id !== server.id));
    setSelectedId(null);
    setView("servers");
  };

  const probeSelected = async () => {
    if (!selected || !backendAvailable) return;
    const serverId = selected.id;
    const probeId = nextProbeId();
    probesRef.current.set(serverId, probeId);
    setStatuses((current) => ({ ...current, [serverId]: { kind: "probing", probeId } }));
    try {
      const report = await probeMcpServer(selected, probeId);
      if (!mountedRef.current || probesRef.current.get(serverId) !== probeId) return;
      probesRef.current.delete(serverId);
      if (report.cancelled) {
        // The host stopped this probe; it reached no verdict about the server.
        setStatuses((current) => ({ ...current, [serverId]: EMPTY_STATUS }));
        return;
      }
      setReports((current) => ({ ...current, [serverId]: report }));
      setStatuses((current) => ({
        ...current,
        [serverId]: report.ok
          ? { kind: "connected" }
          : { kind: "error", error: report.error || t("服务器拒绝连接", "The server rejected the connection") }
      }));
    } catch (error) {
      if (!mountedRef.current || probesRef.current.get(serverId) !== probeId) return;
      probesRef.current.delete(serverId);
      const message = error instanceof Error ? error.message : String(error);
      setReports((current) => ({
        ...current,
        [serverId]: {
          ok: false,
          cancelled: false,
          protocolVersion: "",
          serverName: "",
          serverVersion: "",
          tools: [],
          prompts: [],
          resources: [],
          logs: [],
          error: message
        }
      }));
      setStatuses((current) => ({ ...current, [serverId]: { kind: "error", error: message } }));
    }
  };

  const cancelSelectedProbe = async () => {
    if (!selected || selectedStatus.kind !== "probing") return;
    const serverId = selected.id;
    const { probeId } = selectedStatus;
    try {
      await cancelMcpProbe(probeId);
    } catch (error) {
      // A refused cancel leaves the probe running: say so instead of leaving a
      // button that appears to have done nothing.
      if (!mountedRef.current) return;
      const message = error instanceof Error ? error.message : String(error);
      setStatuses((current) => ({ ...current, [serverId]: { kind: "error", error: message } }));
      return;
    }
    if (!mountedRef.current) return;
    // The host accepted the cancellation, so this probe no longer owns the card.
    if (probesRef.current.get(serverId) === probeId) probesRef.current.delete(serverId);
    setStatuses((current) => {
      const status = current[serverId];
      if (status?.kind !== "probing" || status.probeId !== probeId) return current;
      return { ...current, [serverId]: EMPTY_STATUS };
    });
  };

  const updateToolExclusion = (
    serverId: string,
    key: "disabledTools" | "disabledAutoApproveTools",
    toolName: string,
    excluded: boolean
  ) => {
    replaceServer(serverId, (server) => {
      const current = server[key];
      // Exclusion lists keep remotely added tools enabled by default.
      return {
        ...server,
        [key]: excluded
          ? Array.from(new Set([...current, toolName]))
          : current.filter((name) => name !== toolName)
      };
    });
  };

  const performImport = () => {
    const { servers: imported, errors } = parseServerImport(importText, servers, t);
    if (errors.length) {
      setImportErrors(errors);
      return;
    }
    onChange([...servers, ...imported]);
    setImportOpen(false);
    if (imported[0]) openServer(imported[0].id);
  };

  const detailTabs: Array<{ key: DetailTab; label: string }> = [
    { key: "general", label: t("通用", "General") }
  ];
  if (selectedReport?.ok) {
    detailTabs.push({
      key: "tools",
      label: selectedReport.tools.length
        ? t("工具 ({count})", "Tools ({count})", { count: selectedReport.tools.length })
        : t("工具", "Tools")
    });
    detailTabs.push({
      key: "prompts",
      label: selectedReport.prompts.length
        ? t("提示 ({count})", "Prompts ({count})", { count: selectedReport.prompts.length })
        : t("提示", "Prompts")
    });
    detailTabs.push({
      key: "resources",
      label: selectedReport.resources.length
        ? t("资源 ({count})", "Resources ({count})", { count: selectedReport.resources.length })
        : t("资源", "Resources")
    });
  }
  detailTabs.push({ key: "logs", label: t("日志", "Logs") });
  const activeTab = detailTabs.some((item) => item.key === tab) ? tab : "general";

  return (
    <div className="settings-page mcp-settings-page">
      <nav className="mcp-subnav" aria-label={t("MCP 导航", "MCP navigation")}>
        <h2 className="mcp-subnav__title">{t("MCP", "MCP")}</h2>
        <div className="mcp-subnav__list">
          <button
            type="button"
            className="mcp-subnav__item"
            aria-current={view !== "market" || undefined}
            data-selected={view !== "market" ? "true" : "false"}
            onClick={() => {
              setView("servers");
              setSelectedId(null);
            }}
          >
            <Server size={16} />
            <span>{t("MCP 服务器", "MCP servers")}</span>
          </button>
          <div className="mcp-subnav__divider" />
          <div className="mcp-subnav__section">{t("发现", "Discover")}</div>
          <button
            type="button"
            className="mcp-subnav__item"
            aria-current={view === "market" || undefined}
            data-selected={view === "market" ? "true" : "false"}
            onClick={() => setView("market")}
          >
            <ShoppingBag size={16} />
            <span>{t("市场", "Marketplaces")}</span>
          </button>
        </div>
      </nav>

      <div className="mcp-pane">
        {view === "market" && <McpMarketList />}

        {view === "servers" && (
          <div className="mcp-pane__column">
            <header className="mcp-pane__header">
              <div className="mcp-pane__heading">
                <h2 className="mcp-pane__title">{t("MCP 服务器", "MCP servers")}</h2>
                <div className="mcp-pane__tools">
                  <div className="mcp-filter" ref={filterRef}>
                    <IconButton
                      label={t("筛选", "Filter")}
                      className={filter === "all" ? "" : "mcp-filter__button--active"}
                      aria-haspopup="menu"
                      aria-expanded={filterOpen}
                      onClick={() => setFilterOpen((current) => !current)}
                    ><Filter size={14} /></IconButton>
                    {filterOpen && (
                      <div className="mcp-menu" role="menu">
                        {filterOrder.map((value) => (
                          <button
                            type="button"
                            role="menuitemradio"
                            aria-checked={filter === value}
                            key={value}
                            onClick={() => {
                              setFilter(value);
                              setFilterOpen(false);
                            }}
                          >
                            <Check size={13} className={filter === value ? "" : "mcp-menu__check--hidden"} />
                            <span>{filterLabels[value]}</span>
                            <em>{counts[value]}</em>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                  <div className={searchOpen ? "mcp-search mcp-search--open" : "mcp-search"}>
                    {searchOpen ? (
                      <>
                        <Search size={13} aria-hidden="true" />
                        <input
                          ref={searchInputRef}
                          value={query}
                          aria-label={t("搜索 MCP 服务器", "Search MCP servers")}
                          placeholder={t("搜索 MCP 服务器…", "Search MCP servers…")}
                          onChange={(event) => setQuery(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key !== "Escape") return;
                            if (query) setQuery("");
                            else setSearchOpen(false);
                          }}
                        />
                        <IconButton
                          label={t("关闭搜索", "Close search")}
                          onClick={() => {
                            setQuery("");
                            setSearchOpen(false);
                          }}
                        ><X size={12} /></IconButton>
                      </>
                    ) : (
                      <IconButton label={t("搜索 MCP 服务器", "Search MCP servers")} onClick={() => setSearchOpen(true)}>
                        <Search size={14} />
                      </IconButton>
                    )}
                  </div>
                </div>
              </div>
              <div className="mcp-split" ref={addMenuRef}>
                <button type="button" className="button button--small mcp-split__main" onClick={addServer}>
                  <Plus size={13} /> {t("添加", "Add")}
                </button>
                <button
                  type="button"
                  className="button button--small mcp-split__toggle"
                  aria-label={t("打开添加菜单", "Open add menu")}
                  aria-haspopup="menu"
                  aria-expanded={addMenuOpen}
                  onClick={() => setAddMenuOpen((current) => !current)}
                ><ChevronDown size={13} /></button>
                {addMenuOpen && (
                  <div className="mcp-menu mcp-menu--end" role="menu">
                    <button type="button" role="menuitem" onClick={addServer}>
                      <Plus size={13} /><span>{t("快速创建", "Quick create")}</span>
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      onClick={() => {
                        setImportText("");
                        setImportErrors([]);
                        setImportOpen(true);
                        setAddMenuOpen(false);
                      }}
                    >
                      <Server size={13} /><span>{t("从 JSON 导入", "Import from JSON")}</span>
                    </button>
                  </div>
                )}
              </div>
            </header>

            <div className="mcp-list" data-sortable-list={SORTABLE_LIST_ID}>
              {visibleServers.map((server) => {
                const status = statuses[server.id] ?? EMPTY_STATUS;
                const report = reports[server.id];
                const dropTarget = serverSort.dropTarget?.id === server.id
                  ? ` drop-target--${serverSort.dropTarget.position}`
                  : "";
                return (
                  // biome-ignore lint/a11y/useSemanticElements: Nested switches and links prevent an outer button.
                  <div
                    role="button"
                    tabIndex={0}
                    key={server.id}
                    data-sortable-id={server.id}
                    aria-label={server.name}
                    title={reorderable
                      ? t("拖动整行排序 · Alt + ↑/↓ 键盘排序", "Drag the row to reorder · Alt + ↑/↓ to reorder with the keyboard")
                      : t("搜索或筛选期间不能排序", "Reordering is disabled while searching or filtering")}
                    aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                    className={`mcp-row sortable-surface${serverSort.activeItem === server.id ? " sortable-surface--dragging" : ""}${dropTarget}`}
                    onClick={(event) => {
                      // Nested switches and links use `data-drag-exclude`; clicking them must not
                      // also open details for this row.
                      if ((event.target as HTMLElement).closest("[data-drag-exclude]")) return;
                      openServer(server.id);
                    }}
                    onKeyDown={(event) => {
                      if (event.currentTarget !== event.target) return;
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        openServer(server.id);
                        return;
                      }
                      if (!reorderable) return;
                      if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
                      event.preventDefault();
                      moveByKeyboard(server.id, event.key === "ArrowUp" ? -1 : 1);
                    }}
                    {...(reorderable ? serverSort.bind(server.id) : {})}
                  >
                    <span
                      className={`mcp-dot mcp-dot--${statusClass(status, server.enabled)}`}
                      aria-hidden="true"
                    />
                    <span className="mcp-row__name">{server.name || t("未命名服务器", "Untitled server")}</span>
                    <span className="mcp-row__version">{report?.serverVersion || "—"}</span>
                    <span className={`mcp-badge mcp-badge--${server.transport}`}>
                      {server.transport === "stdio" ? t("STDIO", "STDIO") : t("流式", "Streamable")}
                    </span>
                    <span className="mcp-row__source">
                      {server.providerUrl && (
                        <a
                          href={server.providerUrl}
                          target="_blank"
                          rel="noreferrer noopener"
                          data-drag-exclude
                          aria-label={t("打开 {name} 的主页", "Open the {name} homepage", { name: server.name })}
                        ><ExternalLink size={13} /></a>
                      )}
                    </span>
                    <span className="mcp-row__toolbar">
                      <Switch
                        checked={server.enabled}
                        onChange={(enabled) => setEnabled(server.id, enabled)}
                        label={t("{name} 启用状态", "{name} enabled state", { name: server.name })}
                      />
                    </span>
                  </div>
                );
              })}
              {!servers.length && (
                <div className="mcp-empty">
                  <Server size={20} />
                  <strong>{t("未配置服务器", "No servers configured")}</strong>
                  <small>{t("添加一台 MCP 服务器，让模型使用外部工具与数据。", "Add an MCP server so models can use external tools and data.")}</small>
                </div>
              )}
              {Boolean(servers.length) && !visibleServers.length && (
                <div className="mcp-empty">
                  <Search size={20} />
                  <strong>{t("没有匹配结果", "No results")}</strong>
                </div>
              )}
            </div>
          </div>
        )}

        {view === "detail" && selected && (
          <div className="mcp-pane__column mcp-detail">
            <header className="mcp-detail__header">
              <div className="mcp-detail__identity">
                <IconButton
                  label={t("返回", "Back")}
                  onClick={() => {
                    setView("servers");
                    setSelectedId(null);
                  }}
                ><ArrowLeft size={16} /></IconButton>
                <strong>{selected.name || t("未命名服务器", "Untitled server")}</strong>
                <span className={`mcp-status mcp-status--${statusClass(selectedStatus, selected.enabled)}`}>
                  {statusLabel(selectedStatus, selected.enabled, t)}
                </span>
                {selectedReport?.serverVersion && (
                  <span className="mcp-detail__version">{selectedReport.serverVersion}</span>
                )}
              </div>
              <Switch
                checked={selected.enabled}
                onChange={(enabled) => setEnabled(selected.id, enabled)}
                label={t("{name} 启用状态", "{name} enabled state", { name: selected.name })}
              />
            </header>
            <div className="mcp-detail__divider" />

            <div className="mcp-detail__tabbar">
              <div className="mcp-segmented" role="tablist" aria-label={t("MCP 服务器", "MCP server")}>
                {detailTabs.map((item) => (
                  <button
                    type="button"
                    role="tab"
                    key={item.key}
                    aria-selected={activeTab === item.key}
                    className={activeTab === item.key ? "mcp-segmented__item mcp-segmented__item--active" : "mcp-segmented__item"}
                    onClick={() => setTab(item.key)}
                  >{item.label}</button>
                ))}
              </div>
              {activeTab === "tools" && Boolean(selectedReport?.tools.length) && (
                <div className={toolSearchOpen ? "mcp-search mcp-search--open" : "mcp-search"}>
                  {toolSearchOpen ? (
                    <>
                      <Search size={13} aria-hidden="true" />
                      <input
                        value={toolQuery}
                        aria-label={t("搜索工具", "Search tools")}
                        placeholder={t("搜索", "Search")}
                        onChange={(event) => setToolQuery(event.target.value)}
                      />
                      <IconButton
                        label={t("关闭搜索", "Close search")}
                        onClick={() => {
                          setToolQuery("");
                          setToolSearchOpen(false);
                        }}
                      ><X size={12} /></IconButton>
                    </>
                  ) : (
                    <IconButton label={t("搜索工具", "Search tools")} onClick={() => setToolSearchOpen(true)}>
                      <Search size={14} />
                    </IconButton>
                  )}
                </div>
              )}
            </div>

            <div className="mcp-detail__body">
              {activeTab === "general" && (
                <McpServerForm
                  server={selected}
                  draft={drafts[selected.id] ?? {}}
                  onDraftChange={(draft) => setDrafts((current) => ({ ...current, [selected.id]: draft }))}
                  onChange={(update) => replaceServer(selected.id, update)}
                />
              )}
              {activeTab === "tools" && selectedReport && (
                <McpToolsSection
                  tools={selectedReport.tools}
                  search={toolQuery}
                  disabledTools={selected.disabledTools}
                  disabledAutoApproveTools={selected.disabledAutoApproveTools}
                  onToggleTool={(name, enabled) => updateToolExclusion(selected.id, "disabledTools", name, !enabled)}
                  onToggleAutoApprove={(name, enabled) => updateToolExclusion(selected.id, "disabledAutoApproveTools", name, !enabled)}
                />
              )}
              {activeTab === "prompts" && selectedReport && (
                <McpPromptsSection prompts={selectedReport.prompts} />
              )}
              {activeTab === "resources" && selectedReport && (
                <McpResourcesSection resources={selectedReport.resources} />
              )}
              {activeTab === "logs" && <McpLogsSection logs={selectedReport?.logs ?? []} />}
            </div>

            <footer className="mcp-detail__footer">
              <button
                type="button"
                className="button button--small mcp-detail__delete"
                onClick={() => removeServer(selected)}
              ><Trash2 size={13} /> {t("删除", "Delete")}</button>
              {selectedStatus.kind === "error" && (
                <span className="mcp-detail__error" role="alert">{selectedStatus.error}</span>
              )}
              {selectedStatus.kind === "probing" && (
                <button
                  type="button"
                  className="button button--small button--secondary"
                  onClick={() => void cancelSelectedProbe()}
                >{t("取消探测", "Cancel probe")}</button>
              )}
              <button
                type="button"
                className="button button--small"
                disabled={!backendAvailable || selectedStatus.kind === "probing"}
                title={backendAvailable ? undefined : t("浏览器预览不能探测服务器", "Server probing is unavailable in browser preview")}
                onClick={() => void probeSelected()}
              >{selectedStatus.kind === "probing"
                ? t("连接中…", "Connecting…")
                : t("测试连接", "Test connection")}</button>
            </footer>
          </div>
        )}
      </div>

      {importOpen && (
        <Dialog
          title={t("从 JSON 导入", "Import from JSON")}
          description={t(
            "请从 MCP 服务器的介绍页面复制配置 JSON，并粘贴到输入框中。",
            "Copy the configuration JSON from an MCP server's page and paste it here."
          )}
          width="620px"
          onClose={() => setImportOpen(false)}
          footer={(
            <>
              <button type="button" className="button button--secondary" onClick={() => setImportOpen(false)}>
                {t("取消", "Cancel")}
              </button>
              <button type="button" className="button" onClick={performImport}>
                {t("确定", "Confirm")}
              </button>
            </>
          )}
        >
          <textarea
            className="input mcp-import__textarea"
            aria-label={t("MCP 服务器 JSON 配置", "MCP server JSON configuration")}
            value={importText}
            onChange={(event) => {
              setImportText(event.target.value);
              setImportErrors([]);
            }}
            placeholder={"{\n  \"mcpServers\": {\n    \"example\": { \"command\": \"npx\", \"args\": [\"-y\", \"mcp-server-example\"] }\n  }\n}"}
          />
          {importErrors.length > 0 && (
            <div className="mcp-import__errors" role="alert">
              <strong>{t("以下条目无法导入", "The following entries cannot be imported")}</strong>
              <ul>
                {importErrors.map((error) => (
                  <li key={`${error.entry}-${error.message}`}><code>{error.entry}</code><span>{error.message}</span></li>
                ))}
              </ul>
            </div>
          )}
        </Dialog>
      )}
    </div>
  );
}
