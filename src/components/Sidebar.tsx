import {
  ArrowLeft,
  ChevronRight,
  Folder,
  FolderClock,
  FolderPlus,
  MessageSquarePlus,
  PanelLeftClose,
  Pencil,
  Plus,
  Settings,
  Trash2
} from "lucide-react";
import { Fragment, useEffect, useRef, useState } from "react";
import type {
  KeyboardEvent as ReactKeyboardEvent,
  PointerEvent as ReactPointerEvent,
  ReactNode
} from "react";
import { useI18n } from "../i18n";
import type { AppSurface, Conversation, SettingsView, Workspace } from "../types";
import { IconButton } from "./Common";
import { GlobalSettingsNavigation } from "./GlobalSettingsNavigation";
import { WorkspaceOptionsMenu } from "./WorkspaceOptionsMenu";
import type { WorkspacePresetOption } from "./WorkspaceOptionsMenu";
import { buildConversationTree, conversationAncestorIds } from "../lib/conversationTree";
import { isReservedWorkspace, isTemporaryWorkspace } from "../lib/workspaces";
import type { NewConversationSource } from "../lib/workspaces";
import { isBrowserDevRuntime } from "../lib/backend";
import { usePointerDrag } from "./usePointerDrag";
import type { DragPoint } from "./usePointerDrag";
import { MeworkIcon } from "./MeworkIcon";

export const SIDEBAR_DEFAULT_WIDTH = 264;
export const SIDEBAR_MIN_WIDTH = 220;
export const SIDEBAR_MAX_WIDTH = 420;

export function clampSidebarWidth(width: number): number {
  return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, Math.round(width)));
}

/**
 * Parents whose nested conversations are hidden. This is a per-machine view
 * preference, like the sidebar width, so it lives in `localStorage` instead of the
 * conversation document the host owns. The version suffix lets a future shape change
 * start from an empty set rather than misread the old one.
 */
export const SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY = "mework.sidebar-collapsed-parents.v1";

function loadCollapsedParents(): Set<string> {
  if (typeof window === "undefined") return new Set();
  try {
    const raw = window.localStorage.getItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((id): id is string => typeof id === "string" && id.length > 0));
  } catch {
    // Corrupt or unreadable storage falls back to the default: every fork visible.
    return new Set();
  }
}

function saveCollapsedParents(ids: Set<string>): void {
  if (typeof window === "undefined") return;
  try {
    // Ids of conversations this session has not loaded are kept, so a workspace
    // that is still loading cannot erase a branch collapsed elsewhere.
    window.localStorage.setItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY, JSON.stringify([...ids]));
  } catch {
    // A view preference must never take the sidebar down with it.
  }
}

interface SidebarProps {
  workspaces: Workspace[];
  activeWorkspaceId: string | null;
  activeConversationId: string | null;
  onSelectConversation: (workspaceId: string, conversationId: string) => void;
  onNewConversation: (workspaceId?: string, source?: NewConversationSource) => void;
  onAddWorkspace: () => void;
  onRenameConversation: (workspaceId: string, conversationId: string, title: string) => void;
  onDeleteWorkspace: (workspace: Workspace) => void;
  onDeleteConversation: (conversation: Conversation, workspace: Workspace) => void;
  isConversationRunning: (conversationId: string) => boolean;
  /**
   * Whether this conversation currently has activity: model streaming, an unfinished
   * background command, or a terminal executing a command. Unlike
   * `isConversationRunning`, this only controls the title's slow-pulsing indicator.
   */
  hasLiveActivity?: (conversationId: string) => boolean;
  /** Conversation presets available in the workspace menu, in display order. */
  conversationPresets?: WorkspacePresetOption[];
  /** Sets a workspace's default conversation preset. An empty `presetId` clears it. */
  onSetWorkspaceDefaultPreset?: (workspaceId: string, presetId: string) => void;
  isWorkspaceDeleting: (workspaceId: string) => boolean;
  onOpenSettings: () => void;
  surface?: AppSurface;
  onSelectSettingsView?: (view: SettingsView) => void;
  onReturn?: () => void;
  onClose: () => void;
  open?: boolean;
  width?: number;
  onWidthChange?: (width: number) => void;
  onResizeStateChange?: (resizing: boolean) => void;
  onReorderWorkspace: (workspaceId: string, targetWorkspaceId: string, position: "before" | "after") => void;
  onReorderConversation: (
    workspaceId: string,
    conversationId: string,
    targetConversationId: string,
    position: "before" | "after"
  ) => void;
}

interface RenameDraft {
  workspaceId: string;
  conversationId: string;
  original: string;
  value: string;
}

type PendingDelete =
  | { kind: "workspace"; workspaceId: string }
  | { kind: "conversation"; workspaceId: string; conversationId: string };

type DragItem =
  | { kind: "workspace"; workspaceId: string }
  | { kind: "conversation"; workspaceId: string; conversationId: string };

type DropTarget =
  | { kind: "workspace"; workspaceId: string; position: "before" | "after" }
  | { kind: "conversation"; workspaceId: string; conversationId: string; position: "before" | "after" };

function getVisibleRect(element: HTMLElement): DOMRect | null {
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0 ? rect : null;
}

function canonicalVerticalTarget<T extends { rect: DOMRect }>(
  candidates: T[],
  y: number
): { candidate: T; position: "before" | "after" } | null {
  if (!candidates.length) return null;
  const nextCandidate = candidates.find(({ rect }) => y < rect.top + rect.height / 2);
  return nextCandidate
    ? { candidate: nextCandidate, position: "before" }
    : { candidate: candidates[candidates.length - 1], position: "after" };
}

function getSidebarDropTarget(point: DragPoint, item: DragItem): DropTarget | null {
  const list = document.querySelector<HTMLElement>(".workspace-list");
  const listRect = list ? getVisibleRect(list) : null;
  if (!list || !listRect || point.x < listRect.left - 24 || point.x > listRect.right + 24 || point.y < listRect.top - 24 || point.y > listRect.bottom + 24) {
    return null;
  }

  const groups = Array.from(list.querySelectorAll<HTMLElement>("[data-workspace-group-id]"))
    .map((element) => ({ element, rect: getVisibleRect(element) }))
    .filter((candidate): candidate is { element: HTMLElement; rect: DOMRect } => candidate.rect !== null);
  if (!groups.length) return null;

  if (item.kind === "workspace") {
    const headings = groups
      .filter(({ element }) => element.dataset.workspaceGroupId !== item.workspaceId)
      .map(({ element }) => {
        const heading = element.querySelector<HTMLElement>("[data-workspace-heading]");
        return heading ? { element, rect: getVisibleRect(heading) } : null;
      })
      .filter((candidate): candidate is { element: HTMLElement; rect: DOMRect } => candidate?.rect !== null && candidate !== null);
    const target = canonicalVerticalTarget(headings, point.y);
    if (!target) return null;
    return {
      kind: "workspace",
      workspaceId: target.candidate.element.dataset.workspaceGroupId ?? "",
      position: target.position
    };
  }

  const sourceGroup = groups.find(({ element }) => element.dataset.workspaceGroupId === item.workspaceId);
  if (!sourceGroup || point.y < sourceGroup.rect.top || point.y > sourceGroup.rect.bottom) return null;
  const heading = sourceGroup.element.querySelector<HTMLElement>("[data-workspace-heading]");
  const headingRect = heading ? getVisibleRect(heading) : null;
  if (!headingRect || point.y < headingRect.bottom) return null;

  // Nested child rows are not reorderable, so they are never insertion targets.
  const conversations = Array.from(
    sourceGroup.element.querySelectorAll<HTMLElement>("[data-conversation-id]:not(.conversation-row--nested)")
  )
    .filter((element) => element.dataset.conversationId !== item.conversationId)
    .map((element) => ({ element, rect: getVisibleRect(element) }))
    .filter((candidate): candidate is { element: HTMLElement; rect: DOMRect } => candidate.rect !== null);
  const target = canonicalVerticalTarget(conversations, point.y);
  if (!target) return null;
  return {
    kind: "conversation",
    workspaceId: item.workspaceId,
    conversationId: target.candidate.element.dataset.conversationId ?? "",
    position: target.position
  };
}

function relativeTime(iso: string, t: ReturnType<typeof useI18n>["t"]): string {
  const minutes = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 60_000));
  if (minutes < 1) return t("刚刚", "Just now");
  if (minutes < 60) {
    return t(
      "{count} 分钟",
      minutes === 1 ? "{count} minute" : "{count} minutes",
      { count: minutes }
    );
  }
  if (minutes < 1440) {
    const hours = Math.round(minutes / 60);
    return t(
      "{count} 小时",
      hours === 1 ? "{count} hour" : "{count} hours",
      { count: hours }
    );
  }
  const days = Math.round(minutes / 1440);
  return t(
    "{count} 天",
    days === 1 ? "{count} day" : "{count} days",
    { count: days }
  );
}

export function Sidebar({
  workspaces,
  activeWorkspaceId,
  activeConversationId,
  onSelectConversation,
  onNewConversation,
  onAddWorkspace,
  onRenameConversation,
  onDeleteWorkspace,
  onDeleteConversation,
  isConversationRunning,
  hasLiveActivity = () => false,
  conversationPresets = [],
  onSetWorkspaceDefaultPreset = () => undefined,
  isWorkspaceDeleting,
  onOpenSettings,
  surface = { kind: "workspace" },
  onSelectSettingsView = () => undefined,
  onReturn = () => undefined,
  onClose,
  open = true,
  width = SIDEBAR_DEFAULT_WIDTH,
  onWidthChange = () => undefined,
  onResizeStateChange = () => undefined,
  onReorderWorkspace,
  onReorderConversation
}: SidebarProps) {
  const { t } = useI18n();
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  /** Parents whose children are hidden. Absence means expanded, so forks show up by default. */
  const [collapsedParents, setCollapsedParents] = useState<Set<string>>(loadCollapsedParents);
  const [renameDraft, setRenameDraft] = useState<RenameDraft | null>(null);
  const [pendingDelete, setPendingDelete] = useState<PendingDelete | null>(null);
  const [dragAnnouncement, setDragAnnouncement] = useState("");
  const resizeSessionRef = useRef<{
    pointerId: number;
    startX: number;
    startWidth: number;
    element: HTMLElement;
  } | null>(null);
  const pointerDrag = usePointerDrag<DragItem, DropTarget>({
    getTarget: getSidebarDropTarget,
    onDrop: (item, target) => {
      setPendingDelete(null);
      if (item.kind === "workspace" && target.kind === "workspace" && item.workspaceId !== target.workspaceId) {
        onReorderWorkspace(item.workspaceId, target.workspaceId, target.position);
        setDragAnnouncement(t("工作区顺序已更新", "Workspace order updated"));
        return;
      }
      if (item.kind === "conversation" && target.kind === "conversation"
        && item.workspaceId === target.workspaceId && item.conversationId !== target.conversationId) {
        onReorderConversation(item.workspaceId, item.conversationId, target.conversationId, target.position);
        setDragAnnouncement(t("对话顺序已更新", "Conversation order updated"));
      }
    }
  });
  const dragItem = pointerDrag.activeItem;
  const dropTarget = pointerDrag.dropTarget;

  useEffect(() => {
    const finishResize = (event?: PointerEvent) => {
      const session = resizeSessionRef.current;
      if (!session || (event && event.pointerId !== session.pointerId)) return;
      resizeSessionRef.current = null;
      try {
        if (session.element.hasPointerCapture?.(session.pointerId)) {
          session.element.releasePointerCapture(session.pointerId);
        }
      } catch {
        // Window-level listeners still complete the resize when capture is unavailable.
      }
      document.body.classList.remove("sidebar-resize-active");
      onResizeStateChange(false);
    };
    const moveResize = (event: PointerEvent) => {
      const session = resizeSessionRef.current;
      if (!session || event.pointerId !== session.pointerId) return;
      if (event.cancelable) event.preventDefault();
      onWidthChange(clampSidebarWidth(session.startWidth + event.clientX - session.startX));
    };
    const cancelWithEscape = (event: KeyboardEvent) => {
      const session = resizeSessionRef.current;
      if (!session || event.key !== "Escape") return;
      event.preventDefault();
      onWidthChange(session.startWidth);
      finishResize();
    };
    window.addEventListener("pointermove", moveResize, { passive: false });
    window.addEventListener("pointerup", finishResize);
    window.addEventListener("pointercancel", finishResize);
    window.addEventListener("keydown", cancelWithEscape);
    return () => {
      window.removeEventListener("pointermove", moveResize);
      window.removeEventListener("pointerup", finishResize);
      window.removeEventListener("pointercancel", finishResize);
      window.removeEventListener("keydown", cancelWithEscape);
      document.body.classList.remove("sidebar-resize-active");
    };
  }, [onResizeStateChange, onWidthChange]);

  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || event.isPrimary === false) return;
    event.preventDefault();
    event.stopPropagation();
    resizeSessionRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth: width,
      element: event.currentTarget
    };
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // Some WebViews reject capture; window-level listeners keep resizing active.
    }
    document.body.classList.add("sidebar-resize-active");
    onResizeStateChange(true);
  };

  const resizeWithKeyboard = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? 24 : 12;
    const nextWidth = event.key === "ArrowLeft" ? width - step
      : event.key === "ArrowRight" ? width + step
        : event.key === "Home" ? SIDEBAR_MIN_WIDTH
          : event.key === "End" ? SIDEBAR_MAX_WIDTH
            : null;
    if (nextWidth === null) return;
    event.preventDefault();
    onWidthChange(clampSidebarWidth(nextWidth));
  };

  const moveWorkspaceByKeyboard = (workspaceId: string, direction: -1 | 1) => {
    const index = workspaces.findIndex((workspace) => workspace.id === workspaceId);
    const target = workspaces[index + direction];
    if (!target) return;
    onReorderWorkspace(workspaceId, target.id, direction < 0 ? "before" : "after");
    setDragAnnouncement(direction < 0
      ? t("工作区已向上移动", "Workspace moved up")
      : t("工作区已向下移动", "Workspace moved down"));
  };

  const moveConversationByKeyboard = (workspaceId: string, conversationId: string, direction: -1 | 1) => {
    const workspace = workspaces.find((item) => item.id === workspaceId);
    if (!workspace) return;
    const conversationIndex = workspace.conversations.findIndex((conversation) => conversation.id === conversationId);
    const target = workspace.conversations[conversationIndex + direction];
    if (!target) return;
    onReorderConversation(workspaceId, conversationId, target.id, direction < 0 ? "before" : "after");
    setDragAnnouncement(direction < 0
      ? t("对话已向上移动", "Conversation moved up")
      : t("对话已向下移动", "Conversation moved down"));
  };

  const toggleWorkspace = (id: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleConversationChildren = (id: string) => {
    setCollapsedParents((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  // The active conversation may never sit inside a collapsed parent.
  useEffect(() => {
    if (!activeConversationId) return;
    const workspace = workspaces.find((item) => item.conversations.some(
      (conversation) => conversation.id === activeConversationId
    ));
    if (!workspace) return;
    const ancestors = conversationAncestorIds(workspace.conversations, activeConversationId);
    if (!ancestors.length) return;
    setCollapsedParents((current) => {
      if (!ancestors.some((id) => current.has(id))) return current;
      const next = new Set(current);
      for (const id of ancestors) next.delete(id);
      return next;
    });
  }, [activeConversationId, workspaces]);

  // Saved from the state rather than from either writer, so the disclosure and the
  // automatic ancestor expansion above both survive a remount.
  useEffect(() => {
    saveCollapsedParents(collapsedParents);
  }, [collapsedParents]);

  const finishRename = () => {
    if (!renameDraft) return;
    const title = renameDraft.value.trim();
    if (title && title !== renameDraft.original) {
      onRenameConversation(renameDraft.workspaceId, renameDraft.conversationId, title);
    }
    setRenameDraft(null);
  };

  return (
    <aside
      className={`sidebar ${open ? "" : "sidebar--closed"}`}
      aria-label={surface.kind === "settings"
        ? t("全局设置导航", "Global settings navigation")
        : t("工作区和对话", "Workspaces and conversations")}
      aria-hidden={!open || undefined}
      {...(!open ? { inert: true } : {})}
    >
      <div className="sidebar__brand">
        <MeworkIcon className="brand-mark" size={25} />
        <span>Mework</span>
        <IconButton label={t("收起侧栏", "Collapse sidebar")} onClick={onClose}>
          <PanelLeftClose size={18} />
        </IconButton>
      </div>

      {surface.kind === "settings" ? (
        <>
          <div className="sidebar__section-heading sidebar__section-heading--settings">
            <span>{t("全局设置", "Global settings")}</span>
          </div>
          <GlobalSettingsNavigation
            className="settings-sidebar__nav"
            view={surface.view}
            onSelect={onSelectSettingsView}
          />
        </>
      ) : (
        <>
      <button
        className="new-task-button"
        type="button"
        disabled={Boolean(activeWorkspaceId && isWorkspaceDeleting(activeWorkspaceId))}
        title={activeWorkspaceId && isWorkspaceDeleting(activeWorkspaceId)
          ? t("工作区正在删除", "Workspace is being deleted")
          : undefined}
        onClick={() => onNewConversation()}
      >
        <MessageSquarePlus size={17} />
        <span>{t("新建任务", "New task")}</span>
      </button>

      <div className="sidebar__section-heading">
        <span>{t("工作区", "Workspaces")}</span>
        <IconButton label={t("添加工作区", "Add workspace")} onClick={onAddWorkspace}>
          <FolderPlus size={16} />
        </IconButton>
      </div>

      <nav className="workspace-list" aria-label={t("对话列表", "Conversation list")}>
        {workspaces.map((workspace) => {
          const temporary = isTemporaryWorkspace(workspace);
          const reserved = isReservedWorkspace(workspace);
          const workspaceDisplayName = temporary
            ? t("临时工作区", "Temporary workspace")
            : workspace.name;
          const isCollapsed = collapsed.has(workspace.id);
          const isWorkspaceRunning = workspace.conversations.some((conversation) => isConversationRunning(conversation.id));
          const isDeleting = isWorkspaceDeleting(workspace.id);
          const isLifecycleLocked = isDeleting;
          const isWorkspaceDeleteBlocked = isWorkspaceRunning || isDeleting;
          const isWorkspaceDeletePending = pendingDelete?.kind === "workspace" && pendingDelete.workspaceId === workspace.id;
          const tree = buildConversationTree(workspace.conversations);
          const renderConversationRow = (conversation: Conversation, depth: number): ReactNode => {
            const children = tree.childrenOf.get(conversation.id) ?? [];
            const isNested = depth > 0;
            const isRenaming = renameDraft?.workspaceId === workspace.id && renameDraft.conversationId === conversation.id;
            const isRunning = isLifecycleLocked || isConversationRunning(conversation.id);
            const isLive = hasLiveActivity(conversation.id);
            const isDeletePending = pendingDelete?.kind === "conversation"
              && pendingDelete.workspaceId === workspace.id
              && pendingDelete.conversationId === conversation.id;
            // Only top-level rows take part in reordering; children follow their parent.
            const isSortable = !isNested && !isRunning;
            const childrenExpanded = !collapsedParents.has(conversation.id);
            const row = (
              <div
                className={`conversation-row ${
                  workspace.id === activeWorkspaceId && conversation.id === activeConversationId ? "conversation-row--active" : ""
                } ${isRenaming ? "conversation-row--editing" : ""} ${isSortable ? "sortable-surface" : ""} ${dragItem?.kind === "conversation" && dragItem.conversationId === conversation.id ? "conversation-row--dragging" : ""} ${dropTarget?.kind === "conversation" && dropTarget.workspaceId === workspace.id && dropTarget.conversationId === conversation.id ? `drop-target--${dropTarget.position}` : ""}${isNested ? " conversation-row--nested" : ""}`}
                key={conversation.id}
                data-conversation-id={conversation.id}
                {...(isNested ? { "data-drag-exclude": "" } : {})}
                {...(isSortable ? pointerDrag.bind({ kind: "conversation", workspaceId: workspace.id, conversationId: conversation.id }) : {})}
              >
                {/* A sibling of the main button rather than a child: a button may not contain a
                  * button. It sits in the row's left gutter by absolute positioning either way. */}
                {children.length > 0 && (
                  <button
                    type="button"
                    className="conversation-row__disclosure"
                    data-drag-exclude
                    aria-expanded={childrenExpanded}
                    aria-controls={`conversation-children-${conversation.id}`}
                    aria-label={childrenExpanded
                      ? t("收起子会话", "Collapse child conversations")
                      : t("展开子会话", "Expand child conversations")}
                    onPointerDown={(event) => event.stopPropagation()}
                    onClick={(event) => {
                      // Toggling the disclosure must neither select the row nor start a drag.
                      event.stopPropagation();
                      event.preventDefault();
                      toggleConversationChildren(conversation.id);
                    }}
                  >
                    <ChevronRight className={`disclosure-chevron${childrenExpanded ? " disclosure-chevron--open" : ""}`} size={13} />
                  </button>
                )}
                {isRenaming ? (
                  <input
                    className="conversation-row__rename-input"
                    aria-label={t(
                      "重命名 {title}",
                      "Rename {title}",
                      { title: conversation.title }
                    )}
                    autoFocus
                    value={renameDraft?.value ?? ""}
                    onChange={(event) => setRenameDraft((current) => current ? { ...current, value: event.target.value } : current)}
                    onFocus={(event) => event.currentTarget.select()}
                    onBlur={finishRename}
                    onKeyDown={(event) => {
                      if (event.nativeEvent.isComposing) return;
                      if (event.key === "Enter") {
                        event.preventDefault();
                        event.currentTarget.blur();
                      } else if (event.key === "Escape") {
                        event.preventDefault();
                        setRenameDraft(null);
                      }
                    }}
                  />
                ) : (
                  <button
                    type="button"
                    className="conversation-row__main"
                    onClick={() => onSelectConversation(workspace.id, conversation.id)}
                    aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                    onKeyDown={(event) => {
                      if (isRunning || isNested || !event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
                      event.preventDefault();
                      moveConversationByKeyboard(workspace.id, conversation.id, event.key === "ArrowUp" ? -1 : 1);
                    }}
                  >
                    {/* The row reserves fixed left padding for this indicator, so rendering it
                      * only during activity does not shift the title. */}
                    {isLive && (
                      <span
                        className="conversation-row__pulse"
                        role="img"
                        aria-label={t("正在进行", "In progress")}
                        title={t("正在进行", "In progress")}
                      />
                    )}
                    <span className="conversation-row__title">{conversation.title}</span>
                    <span className="conversation-row__time">{relativeTime(conversation.updatedAt, t)}</span>
                  </button>
                )}
                <div className="conversation-row__actions">
                  <IconButton
                    label={t("重命名 {title}", "Rename {title}", { title: conversation.title })}
                    className="conversation-row__rename"
                    disabled={isRenaming || isLifecycleLocked}
                    onClick={() => setRenameDraft({
                      workspaceId: workspace.id,
                      conversationId: conversation.id,
                      original: conversation.title,
                      value: conversation.title
                    })}
                  >
                    <Pencil size={14} />
                  </IconButton>
                  <IconButton
                    label={isDeletePending
                      ? t("确认删除 {title}", "Confirm deleting {title}", { title: conversation.title })
                      : t("删除 {title}", "Delete {title}", { title: conversation.title })}
                    className={`conversation-row__delete sidebar-delete-confirm icon-button--danger ${isDeletePending ? "sidebar-delete-confirm--armed" : ""}`}
                    disabled={isRunning}
                    title={isRunning
                      ? t(
                        "当前操作结束后才能删除",
                        "Wait for the current operation to finish before deleting"
                      )
                      : isDeletePending
                        ? t("确认删除 {title}", "Confirm deleting {title}", { title: conversation.title })
                        : t("删除 {title}", "Delete {title}", { title: conversation.title })}
                    onBlur={() => setPendingDelete((current) => (
                      current?.kind === "conversation"
                        && current.workspaceId === workspace.id
                        && current.conversationId === conversation.id
                        ? null
                        : current
                    ))}
                    onKeyDown={(event) => {
                      if (event.key !== "Escape" || !isDeletePending) return;
                      event.preventDefault();
                      setPendingDelete(null);
                    }}
                    onClick={() => {
                      if (!isDeletePending) {
                        setPendingDelete({
                          kind: "conversation",
                          workspaceId: workspace.id,
                          conversationId: conversation.id
                        });
                        return;
                      }
                      setPendingDelete(null);
                      onDeleteConversation(conversation, workspace);
                    }}
                  >
                    {isDeletePending
                      ? <span className="sidebar-delete-confirm__label">{t("确认", "Confirm")}</span>
                      : <Trash2 size={14} />}
                  </IconButton>
                </div>
              </div>
            );
            if (!children.length) return row;
            return (
              <Fragment key={conversation.id}>
                {row}
                <div
                  id={`conversation-children-${conversation.id}`}
                  className={`collapse-region ${childrenExpanded ? "" : "collapse-region--closed"}`}
                  aria-hidden={!childrenExpanded || undefined}
                  inert={!childrenExpanded || undefined}
                >
                  <div className="collapse-region__inner conversation-list conversation-list--nested">
                    {children.map((child) => renderConversationRow(child, depth + 1))}
                  </div>
                </div>
              </Fragment>
            );
          };
          return (
            <section
              className={`workspace-group ${dragItem?.kind === "workspace" && dragItem.workspaceId === workspace.id ? "workspace-group--dragging" : ""} ${dropTarget?.kind === "workspace" && dropTarget.workspaceId === workspace.id ? `drop-target--${dropTarget.position}` : ""}`}
              key={workspace.id}
              data-workspace-group-id={workspace.id}
            >
              <div
                className={`workspace-heading${isLifecycleLocked ? "" : " sortable-surface"}`}
                data-workspace-heading
                {...(isLifecycleLocked ? {} : pointerDrag.bind({ kind: "workspace", workspaceId: workspace.id }))}
              >
                <button
                  type="button"
                  onClick={() => toggleWorkspace(workspace.id)}
                  title={workspace.path}
                  aria-expanded={!isCollapsed}
                  aria-controls={`workspace-conversations-${workspace.id}`}
                  aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                  onKeyDown={(event) => {
                    if (isLifecycleLocked || !event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
                    event.preventDefault();
                    moveWorkspaceByKeyboard(workspace.id, event.key === "ArrowUp" ? -1 : 1);
                  }}
                >
                  <ChevronRight className={`disclosure-chevron${isCollapsed ? "" : " disclosure-chevron--open"}`} size={15} />
                  {temporary ? <FolderClock size={15} /> : <Folder size={15} />}
                  <span className="workspace-heading__name">{workspaceDisplayName}</span>
                </button>
                <WorkspaceOptionsMenu
                  workspaceName={workspaceDisplayName}
                  presets={conversationPresets}
                  selectedPresetId={workspace.defaultConversationPresetId}
                  disabled={isLifecycleLocked}
                  onSelectPreset={(presetId) => onSetWorkspaceDefaultPreset(workspace.id, presetId)}
                />
                <IconButton
                  label={t("在 {name} 新建任务", "Create a task in {name}", { name: workspaceDisplayName })}
                  disabled={isLifecycleLocked}
                  title={isDeleting ? t("工作区正在删除", "Workspace is being deleted") : undefined}
                  onClick={() => onNewConversation(workspace.id, "workspace")}
                >
                  <Plus size={15} />
                </IconButton>
                {!reserved && <IconButton
                  label={isWorkspaceDeletePending
                    ? t("确认删除工作区 {name}", "Confirm deleting workspace {name}", { name: workspaceDisplayName })
                    : t("删除工作区 {name}", "Delete workspace {name}", { name: workspaceDisplayName })}
                  className={`workspace-heading__delete sidebar-delete-confirm icon-button--danger ${isWorkspaceDeletePending ? "sidebar-delete-confirm--armed" : ""}`}
                  disabled={isWorkspaceDeleteBlocked}
                  title={isDeleting
                    ? t("工作区正在删除", "Workspace is being deleted")
                    : isWorkspaceRunning
                    ? t(
                      "当前操作结束后才能删除工作区",
                      "Wait for the current operation to finish before deleting the workspace"
                    )
                    : isWorkspaceDeletePending
                      ? t("确认删除工作区 {name}", "Confirm deleting workspace {name}", { name: workspaceDisplayName })
                      : t("删除工作区 {name}", "Delete workspace {name}", { name: workspaceDisplayName })}
                  onBlur={() => setPendingDelete((current) => (
                    current?.kind === "workspace" && current.workspaceId === workspace.id ? null : current
                  ))}
                  onKeyDown={(event) => {
                    if (event.key !== "Escape" || !isWorkspaceDeletePending) return;
                    event.preventDefault();
                    setPendingDelete(null);
                  }}
                  onClick={() => {
                    if (!isWorkspaceDeletePending) {
                      setPendingDelete({ kind: "workspace", workspaceId: workspace.id });
                      return;
                    }
                    setPendingDelete(null);
                    onDeleteWorkspace(workspace);
                  }}
                >
                  {isWorkspaceDeletePending
                    ? <span className="sidebar-delete-confirm__label">{t("确认", "Confirm")}</span>
                    : <Trash2 size={14} />}
                </IconButton>}
              </div>
              <div
                id={`workspace-conversations-${workspace.id}`}
                className={`collapse-region ${isCollapsed ? "collapse-region--closed" : ""}`}
                aria-hidden={isCollapsed || undefined}
                inert={isCollapsed || undefined}
              >
                <div className="collapse-region__inner conversation-list">
                  {tree.roots.map((conversation) => renderConversationRow(conversation, 0))}
                  {workspace.conversations.length === 0 && (
                    <p className="conversation-list__empty">{t("还没有任务", "No tasks yet")}</p>
                  )}
                </div>
              </div>
            </section>
          );
        })}
      </nav>
        </>
      )}

      <div className="sidebar__footer">
        <button type="button" onClick={surface.kind === "workspace" ? onOpenSettings : onReturn}>
          {surface.kind === "workspace" ? <Settings size={17} /> : <ArrowLeft size={17} />}
          <span>{surface.kind === "workspace" ? t("设置", "Settings") : t("返回", "Back")}</span>
        </button>
        {isBrowserDevRuntime() ? (
          <span
            className="version-badge version-badge--dev-domain"
            title={t(
              "browser-dev 使用独立的开发数据域（com.mework.app.e2e.interactive-dev）；这里的对话、设置与正式版互不可见，不是数据丢失。",
              "browser-dev uses an isolated development data domain (com.mework.app.e2e.interactive-dev); conversations and settings here are invisible to the production app by design — nothing is lost."
            )}
          >{t("开发数据域", "Dev data domain")}</span>
        ) : (
          <span className="version-badge">{t("本地", "Local")}</span>
        )}
      </div>
      <div
        className="sidebar-resize-handle"
        role="separator"
        aria-label={t("调整侧栏宽度", "Resize sidebar")}
        aria-orientation="vertical"
        aria-valuemin={SIDEBAR_MIN_WIDTH}
        aria-valuemax={SIDEBAR_MAX_WIDTH}
        aria-valuenow={width}
        aria-valuetext={t("{width} 像素", "{width} pixels", { width })}
        tabIndex={0}
        title={t(
          "拖动调整侧栏宽度；双击恢复默认宽度",
          "Drag to resize the sidebar; double-click to restore the default width"
        )}
        onPointerDown={startResize}
        onKeyDown={resizeWithKeyboard}
        onDoubleClick={() => onWidthChange(SIDEBAR_DEFAULT_WIDTH)}
      />
      {dragAnnouncement && <span className="sr-only" role="status" aria-live="polite">{dragAnnouncement}</span>}
    </aside>
  );
}
