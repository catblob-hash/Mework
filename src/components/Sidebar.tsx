import {
  ChevronRight,
  Folder,
  FolderClock,
  FolderPlus,
  Pencil,
  Plus,
  Server,
  Settings,
  SquareTerminal
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type {
  KeyboardEvent as ReactKeyboardEvent,
  PointerEvent as ReactPointerEvent
} from "react";
import { useI18n } from "../i18n";
import type { Conversation, SshMachineConfig, Workspace } from "../types";
import { ConfirmDeleteButton, IconButton } from "./Common";
import { WorkspaceOptionsMenu } from "./WorkspaceOptionsMenu";
import type { WorkspacePresetOption } from "./WorkspaceOptionsMenu";
import { visibleConversations } from "../lib/draftConversation";
import {
  isReservedWorkspace,
  isTemporaryWorkspace,
  projectWorkspaces,
  workspaceLocationTitle
} from "../lib/workspaces";
import type { NewConversationSource } from "../lib/workspaces";
import { isImeKeyEvent } from "../lib/shortcuts";
import { isBrowserDevRuntime } from "../lib/backend";
import { usePointerDrag } from "./usePointerDrag";
import type { DragPoint } from "./usePointerDrag";
import { MeworkMark } from "./MeworkIcon";

export const SIDEBAR_DEFAULT_WIDTH = 264;
const SIDEBAR_MIN_WIDTH = 220;
const SIDEBAR_MAX_WIDTH = 420;

export function clampSidebarWidth(width: number): number {
  return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, Math.round(width)));
}

/**
 * What a conversation's row mark says, most urgent first when several hold at once:
 * `blocked` — the run waits on the user (an approval, a question, a fork request);
 * `running` — something is under way (a model stream, a background command, a busy terminal);
 * `completed` — a run finished while the user was looking at another conversation;
 * `idle` — none of these.
 */
export type ConversationStatus = "idle" | "running" | "blocked" | "completed";

interface SidebarProps {
  workspaces: Workspace[];
  /** The SSH catalog, so a workspace on one of those machines can be titled by the machine's name. */
  sshMachines?: SshMachineConfig[];
  activeWorkspaceId: string | null;
  activeConversationId: string | null;
  onSelectConversation: (workspaceId: string, conversationId: string) => void;
  onNewConversation: (workspaceId?: string, source?: NewConversationSource) => void;
  onAddWorkspace: () => void;
  /** Opens the project dialog on an existing project, to rename it or change its later workspaces. */
  onEditProject?: (workspaceId: string) => void;
  onRenameConversation: (workspaceId: string, conversationId: string, title: string) => void;
  onDeleteWorkspace: (workspace: Workspace) => void;
  onDeleteConversation: (conversation: Conversation, workspace: Workspace) => void;
  isConversationRunning: (conversationId: string) => boolean;
  /**
   * What the mark before the conversation's title shows. Unlike `isConversationRunning`,
   * which locks deletion, this only paints the mark.
   */
  conversationStatus?: (conversationId: string) => ConversationStatus;
  /** Conversation presets available in the workspace menu, in display order. */
  conversationPresets?: WorkspacePresetOption[];
  /** Sets a workspace's default conversation preset. An empty `presetId` clears it. */
  onSetWorkspaceDefaultPreset?: (workspaceId: string, presetId: string) => void;
  isWorkspaceDeleting: (workspaceId: string) => boolean;
  onOpenSettings: () => void;
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

  const conversations = Array.from(sourceGroup.element.querySelectorAll<HTMLElement>("[data-conversation-id]"))
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

/**
 * The brand mark as a status light: an outline while idle, a blinking cursor while working,
 * amber while it waits on the user and blue once a run finished unseen.
 */
function ConversationStatusMark({ status }: { status: ConversationStatus }) {
  const { t } = useI18n();
  const label = status === "running" ? t("正在进行", "In progress")
    : status === "blocked" ? t("等待你处理", "Waiting for you")
      : status === "completed" ? t("已完成", "Finished")
        : null;
  return (
    <span
      className={`conversation-status conversation-status--${status}`}
      {...(label ? { role: "img", "aria-label": label, title: label } : { "aria-hidden": true })}
    >
      <MeworkMark className="conversation-status__mark" />
    </span>
  );
}

export function Sidebar({
  workspaces,
  sshMachines = [],
  activeWorkspaceId,
  activeConversationId,
  onSelectConversation,
  onNewConversation,
  onAddWorkspace,
  onEditProject,
  onRenameConversation,
  onDeleteWorkspace,
  onDeleteConversation,
  isConversationRunning,
  conversationStatus = () => "idle",
  conversationPresets = [],
  onSetWorkspaceDefaultPreset = () => undefined,
  isWorkspaceDeleting,
  onOpenSettings,
  open = true,
  width = SIDEBAR_DEFAULT_WIDTH,
  onWidthChange = () => undefined,
  onResizeStateChange = () => undefined,
  onReorderWorkspace,
  onReorderConversation
}: SidebarProps) {
  const { t } = useI18n();
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [renameDraft, setRenameDraft] = useState<RenameDraft | null>(null);
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
      if (item.kind === "workspace" && target.kind === "workspace" && item.workspaceId !== target.workspaceId) {
        onReorderWorkspace(item.workspaceId, target.workspaceId, target.position);
        setDragAnnouncement(t("项目顺序已更新", "Project order updated"));
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
      ? t("项目已向上移动", "Project moved up")
      : t("项目已向下移动", "Project moved down"));
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
      aria-label={t("项目和对话", "Projects and conversations")}
      aria-hidden={!open || undefined}
      {...(!open ? { inert: true } : {})}
    >
      {/* The window's top row. The traffic lights and the shell's navigation float over it
        * (`ShellNav` in App), so it only holds their room and moves the window. */}
      <div className="sidebar__chrome-row" data-tauri-drag-region="deep" />

      <button
        className="new-task-button"
        type="button"
        disabled={Boolean(activeWorkspaceId && isWorkspaceDeleting(activeWorkspaceId))}
        title={activeWorkspaceId && isWorkspaceDeleting(activeWorkspaceId)
          ? t("项目正在删除", "Project is being deleted")
          : undefined}
        onClick={() => onNewConversation()}
      >
        <Plus size={15} />
        <span>{t("新建任务", "New task")}</span>
      </button>

      <div className="sidebar__section-heading">
        <span>{t("项目", "Projects")}</span>
        <IconButton label={t("新建项目", "New project")} onClick={onAddWorkspace}>
          <FolderPlus size={15} />
        </IconButton>
      </div>

      <nav className="workspace-list" aria-label={t("对话列表", "Conversation list")}>
        {workspaces.map((workspace) => {
          const temporary = isTemporaryWorkspace(workspace);
          const reserved = isReservedWorkspace(workspace);
          const workspaceDisplayName = temporary
            ? t("临时项目", "Temporary project")
            : workspace.name;
          const isCollapsed = collapsed.has(workspace.id);
          // An unsent draft slot has nothing to show yet, so the list withholds it until it does.
          // A fork is listed like any other conversation of its project, in the project's order.
          const listed = visibleConversations(workspace.conversations);
          const isWorkspaceRunning = workspace.conversations.some((conversation) => isConversationRunning(conversation.id));
          const isDeleting = isWorkspaceDeleting(workspace.id);
          const isLifecycleLocked = isDeleting;
          const isWorkspaceDeleteBlocked = isWorkspaceRunning || isDeleting;
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
                  className="workspace-heading__toggle"
                  onClick={() => toggleWorkspace(workspace.id)}
                  title={temporary
                    ? workspaceDisplayName
                    : projectWorkspaces(workspace)
                      .map((entry) => workspaceLocationTitle(entry.path, entry.machine, sshMachines))
                      .join("\n")}
                  aria-expanded={!isCollapsed}
                  aria-controls={`workspace-conversations-${workspace.id}`}
                  aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                  onKeyDown={(event) => {
                    if (isLifecycleLocked || !event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
                    event.preventDefault();
                    moveWorkspaceByKeyboard(workspace.id, event.key === "ArrowUp" ? -1 : 1);
                  }}
                >
                  {temporary
                    ? <FolderClock size={14} />
                    : !workspace.machine
                      ? <Folder size={14} />
                      : workspace.machine.kind === "wsl"
                        ? <SquareTerminal size={14} />
                        : <Server size={14} />}
                  <span className="workspace-heading__name">{workspaceDisplayName}</span>
                  <ChevronRight
                    className={`workspace-heading__chevron${isCollapsed ? "" : " workspace-heading__chevron--open"}`}
                    size={13}
                    aria-hidden="true"
                  />
                </button>
                <WorkspaceOptionsMenu
                  workspaceName={workspaceDisplayName}
                  presets={conversationPresets}
                  selectedPresetId={workspace.defaultConversationPresetId}
                  disabled={isLifecycleLocked}
                  onSelectPreset={(presetId) => onSetWorkspaceDefaultPreset(workspace.id, presetId)}
                  onEditProject={reserved || !onEditProject ? undefined : () => onEditProject(workspace.id)}
                />
                <IconButton
                  label={t("在 {name} 新建任务", "Create a task in {name}", { name: workspaceDisplayName })}
                  disabled={isLifecycleLocked}
                  title={isDeleting ? t("项目正在删除", "Project is being deleted") : undefined}
                  onClick={() => onNewConversation(workspace.id, "workspace")}
                >
                  <Plus size={14} />
                </IconButton>
                {!reserved && <ConfirmDeleteButton
                  label={t("删除项目 {name}", "Delete project {name}", { name: workspaceDisplayName })}
                  confirmLabel={t("确认删除项目 {name}", "Confirm deleting project {name}", { name: workspaceDisplayName })}
                  className="workspace-heading__delete"
                  size={13}
                  disabled={isWorkspaceDeleteBlocked}
                  title={isDeleting
                    ? t("项目正在删除", "Project is being deleted")
                    : isWorkspaceRunning
                    ? t(
                      "当前操作结束后才能删除项目",
                      "Wait for the current operation to finish before deleting the project"
                    )
                    : undefined}
                  onDelete={() => onDeleteWorkspace(workspace)}
                />}
              </div>
              {/* Opens and closes at once: a project is a list to scan, not a panel to reveal. */}
              <div
                id={`workspace-conversations-${workspace.id}`}
                className={`conversation-list${isCollapsed ? " conversation-list--closed" : ""}`}
                hidden={isCollapsed}
              >
                {listed.map((conversation) => {
                  const isRenaming = renameDraft?.workspaceId === workspace.id && renameDraft.conversationId === conversation.id;
                  const isRunning = isLifecycleLocked || isConversationRunning(conversation.id);
                  const isSortable = !isRunning;
                  return (
                    <div
                      className={`conversation-row ${
                        workspace.id === activeWorkspaceId && conversation.id === activeConversationId ? "conversation-row--active" : ""
                      } ${isRenaming ? "conversation-row--editing" : ""} ${isSortable ? "sortable-surface" : ""} ${dragItem?.kind === "conversation" && dragItem.conversationId === conversation.id ? "conversation-row--dragging" : ""} ${dropTarget?.kind === "conversation" && dropTarget.workspaceId === workspace.id && dropTarget.conversationId === conversation.id ? `drop-target--${dropTarget.position}` : ""}`}
                      key={conversation.id}
                      data-conversation-id={conversation.id}
                      {...(isSortable ? pointerDrag.bind({ kind: "conversation", workspaceId: workspace.id, conversationId: conversation.id }) : {})}
                    >
                      <ConversationStatusMark status={conversationStatus(conversation.id)} />
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
                            if (isImeKeyEvent(event.nativeEvent)) return;
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
                            if (isRunning || !event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
                            event.preventDefault();
                            moveConversationByKeyboard(workspace.id, conversation.id, event.key === "ArrowUp" ? -1 : 1);
                          }}
                        >
                          <span className="conversation-row__title">{conversation.title}</span>
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
                          <Pencil size={13} />
                        </IconButton>
                        <ConfirmDeleteButton
                          label={t("删除 {title}", "Delete {title}", { title: conversation.title })}
                          confirmLabel={t("确认删除 {title}", "Confirm deleting {title}", { title: conversation.title })}
                          className="conversation-row__delete"
                          size={13}
                          disabled={isRunning}
                          title={isRunning
                            ? t(
                              "当前操作结束后才能删除",
                              "Wait for the current operation to finish before deleting"
                            )
                            : undefined}
                          onDelete={() => onDeleteConversation(conversation, workspace)}
                        />
                      </div>
                    </div>
                  );
                })}
                {listed.length === 0 && (
                  <p className="conversation-list__empty">{t("还没有任务", "No tasks yet")}</p>
                )}
              </div>
            </section>
          );
        })}
      </nav>

      <div className="sidebar__footer">
        <button type="button" onClick={onOpenSettings}>
          <Settings size={17} />
          <span>{t("设置", "Settings")}</span>
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
