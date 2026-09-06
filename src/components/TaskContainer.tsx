import {
  Bot,
  ChevronRight,
  Globe2,
  LoaderCircle,
  PanelRightClose,
  Square,
  SquareTerminal,
  Terminal,
  Workflow as WorkflowIcon
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  KeyboardEvent as ReactKeyboardEvent,
  PointerEvent as ReactPointerEvent
} from "react";
import { useI18n } from "../i18n";
import {
  deriveTaskItems,
  finishedTaskItems,
  flattenTaskItems,
  runningTaskItems
} from "../lib/taskContainer";
import type { TaskContainerMessages, TaskItem, TaskItemState } from "../lib/taskContainer";
import type { AgentStatus, TodoItemView } from "../lib/orchestration";
import type { UserAbortedTaskRecord } from "../types";
import type { SubagentView } from "../lib/subagents";
import type { TerminalSessionState } from "../lib/terminal";
import type { ShellTaskSnapshot } from "../lib/shellTasks";
import type { BrowserStatus } from "../lib/browser";
import type { WorkflowProgressView } from "../lib/workflowProgress";
import { deriveWorkflowRun } from "../lib/workflowRuns";
import { IconButton } from "./Common";
import { RollingNumber } from "./RollingNumber";
import { WorkflowRunPanel } from "./WorkflowRunPanel";
import "./TaskContainer.css";

export const taskContainerId = "task-container-panel";
export const TASK_CONTAINER_DEFAULT_WIDTH = 340;
export const TASK_CONTAINER_MIN_WIDTH = 260;
export const TASK_CONTAINER_MAX_WIDTH = 520;

export function clampTaskContainerWidth(width: number, maxWidth = TASK_CONTAINER_MAX_WIDTH): number {
  return Math.min(maxWidth, Math.max(TASK_CONTAINER_MIN_WIDTH, Math.round(width)));
}

export interface TaskContainerProps {
  conversationId?: string;
  open: boolean;
  agents: SubagentView[];
  terminals: TerminalSessionState[];
  /** Shell commands this conversation has run, running and finished; each is one row. */
  shellTasks?: ShellTaskSnapshot[];
  browser?: BrowserStatus | null;
  /** Native session the browser row closes; the row is omitted without one. */
  browserSessionId?: string | null;
  /** Every live preview session, one row each. Replaces the pair above when supplied. */
  browserSessions?: { sessionId: string; status: BrowserStatus }[];
  /** Browser tool the model is driving the page with, or null when nobody is. */
  browserAutomationTool?: string | null;
  /** Whether a requested automation stop has not yet been observed. */
  browserAutomationStopping?: boolean;
  modelRequestId?: string | null;
  userAbortedTasks?: UserAbortedTaskRecord[];
  /**
   * Model the conversation is on, shown by a child that bound no model of its
   * own. A role-less child runs on exactly this; its record cannot say so.
   */
  inheritedModelId?: string | null;
  /** Todo plan, shown above the list as the run's own summary. */
  status?: AgentStatus;
  /** Agent view currently shown in the main area, so its row reads as selected. */
  selectedAgentId: string | null;
  /**
   * Row whose page the message area is showing, when it is not an agent transcript — a preview,
   * a shell command's output, the review page. Falls back to `selectedAgentId`.
   */
  selectedRowId?: string | null;
  /** Ids whose abort has been requested and not yet observed as terminal. */
  stoppingIds?: string[];
  /**
   * Live progress ledgers, keyed by the workflow run's view id. They carry the
   * two things the agent roster cannot: the plan slots that never got an agent,
   * and the run's narration lines. A settled run simply has no entry.
   */
  workflowProgress?: Record<string, WorkflowProgressView>;
  /** Run identifiers the Skip command addresses, keyed the same way. */
  workflowRunIds?: Record<string, string>;
  onWorkflowStepControl?: (runId: string, stepIndex: number) => void;
  onSelectAgent: (agentId: string) => void;
  /**
   * Opens a row that is not an agent — a terminal or the browser page — in its
   * sidebar page. Rows of those kinds stay inert without it.
   */
  onOpenItem?: (item: TaskItem) => void;
  onStopItem: (item: TaskItem) => void;
  onClose: () => void;
  width?: number;
  onWidthChange?: (width: number) => void;
  /** Lets the shell drop its grid transition while a drag is in flight. */
  onResizeStateChange?: (resizing: boolean) => void;
}

function taskItemIcon(item: TaskItem, size = 13) {
  const kind = item.kind === "aborted" ? item.sourceKind : item.kind;
  if (kind === "terminal") return <SquareTerminal size={size} aria-hidden="true" />;
  // Distinct from the terminal's framed glyph: a command the model started is
  // not a shell the user opened, and the two sit in the same list.
  if (kind === "shell") return <Terminal size={size} aria-hidden="true" />;
  // Persisted aborted records can retain sourceKind: "webSearch".
  if (kind === "webSearch") return <Globe2 size={size} aria-hidden="true" />;
  if (kind === "workflow") return <WorkflowIcon size={size} aria-hidden="true" />;
  if (kind === "browser") return <Globe2 size={size} aria-hidden="true" />;
  return <Bot size={size} aria-hidden="true" />;
}

/** 1_240 → "1.2k", 12_400 → "12k". Below 1000 the exact count fits, so it stays exact. */
function formatTokens(tokens: number): string {
  if (tokens < 1000) return String(tokens);
  const thousands = tokens / 1000;
  return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
}

/** Compact wall time: "9s", "1m24s", "3m02s", "1h04m". */
function formatElapsed(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m${String(seconds).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, "0")}m`;
}

/** Whether a row's metrics describe an agent, whose columns are the four below. */
function reportsAgentMetrics(item: TaskItem): boolean {
  const kind = item.kind === "aborted" ? item.sourceKind : item.kind;
  return kind === "subagent" || kind === "workflow";
}

/**
 * The four metric columns. On an agent row a column it cannot report renders an
 * em dash rather than a zero — a run whose provider reported no token total is
 * not the same claim as one that spent none.
 *
 * A row that is not an agent has no such claim to make: child count, tokens and
 * tool calls are quantities only a subagent has, so those columns are dropped
 * outright instead of painting a row of dashes down every shell command, web
 * search, terminal and browser page.
 */
function TaskMetricColumns({ item }: { item: TaskItem }) {
  const { t } = useI18n();
  const metrics = item.metrics;
  const columns: { key: string; value: number | null; text: string; title: string }[] = [
    {
      key: "children",
      value: metrics.childCount,
      text: metrics.childCount === null ? "—" : t("{count}子", "{count}c", { count: metrics.childCount }),
      title: t("子代数", "Child count")
    },
    {
      key: "tokens",
      value: metrics.tokens,
      text: metrics.tokens === null ? "—" : formatTokens(metrics.tokens),
      title: t("token 数", "Tokens")
    },
    {
      key: "tools",
      value: metrics.toolCount,
      text: metrics.toolCount === null ? "—" : t("{count}工具", "{count}t", { count: metrics.toolCount }),
      title: t("工具调用数", "Tool calls")
    },
    {
      key: "elapsed",
      value: metrics.elapsedMs,
      text: metrics.elapsedMs === null ? "—" : formatElapsed(metrics.elapsedMs),
      title: t("运行时长", "Elapsed")
    }
  ];
  const shown = reportsAgentMetrics(item)
    ? columns
    : columns.filter((column) => column.value !== null);
  if (!shown.length) return null;
  return (
    <span className="task-metrics" aria-hidden="true">
      {shown.map((column) => (
        <span className="task-metrics__cell" key={column.key} title={column.title}>
          <RollingNumber value={column.text} />
        </span>
      ))}
    </span>
  );
}

/**
 * One row of the list. Every row starts hard against the panel's left edge: the
 * sidebar has no nesting left to draw. A workflow renders its own panel with its
 * steps inside it, and an ordinary subagent cannot have children — the host
 * refuses to let a subagent spawn another one.
 */
function TaskRow({
  item,
  selectedAgentId,
  selectedRowId,
  stoppingIds,
  onSelectAgent,
  onOpenItem,
  onStopItem
}: {
  item: TaskItem;
  selectedAgentId: string | null;
  /**
   * Row whose page the message area is showing. Every openable row can be the current one now,
   * not just an agent, because a preview or a shell command takes over the same surface a
   * transcript does.
   */
  selectedRowId: string | null;
  stoppingIds: Set<string>;
  onSelectAgent: (agentId: string) => void;
  onOpenItem?: (item: TaskItem) => void;
  onStopItem: (item: TaskItem) => void;
}) {
  const { t } = useI18n();
  // A workflow no longer reaches this row: it draws as its own panel. Only a
  // plain subagent opens a transcript from the tree — and a workflow driver
  // never had one worth opening, which is why the row stopped offering it.
  const agentRow = item.kind === "subagent";
  // A terminal, preview or shell row opens its own page in the message area instead of a
  // transcript, and only when the shell supplied a handler for it.
  const pageRow = (
    item.kind === "terminal" || item.kind === "browser" || item.kind === "shell"
  ) && Boolean(onOpenItem);
  const openable = agentRow || pageRow;
  // An agent row's id *is* its agent id, so one comparison covers both the transcript case and
  // the pages that replaced the sidebar tabs.
  const selected = openable && (
    selectedRowId !== null
      ? item.id === selectedRowId
      : agentRow && item.agent.id === selectedAgentId
  );
  const stopping = stoppingIds.has(item.id);

  const activate = () => {
    if (agentRow) onSelectAgent(item.agent.id);
    else if (pageRow) onOpenItem!(item);
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    if (event.target !== event.currentTarget) return;
    event.preventDefault();
    activate();
  };

  return (
    <li className={`task-row task-row--${item.state}${selected ? " task-row--selected" : ""}`}>
      <div
        className="task-row__main"
        role={openable ? "button" : undefined}
        tabIndex={openable ? 0 : undefined}
        aria-label={agentRow
          ? t("打开子代理 {label}", "Open subagent {label}", { label: item.label })
          : pageRow
            ? t("打开“{label}”", "Open “{label}”", { label: item.label })
            : undefined}
        aria-current={selected || undefined}
        // The error text is the row's tooltip, which is what "hover a failed
        // row to see why" means. A row that did not fail has none.
        title={item.error ?? undefined}
        onClick={activate}
        onKeyDown={onKeyDown}
      >
        <span className={`task-row__icon task-row__icon--${item.state}`}>{taskItemIcon(item)}</span>
        <span className="task-row__copy">
          {/* A running row's title carries the same left-to-right sweep the
              stream indicator uses, so "still working" reads the same way
              wherever it appears. */}
          <span className={`task-row__label${item.state === "running" ? " pulse-text" : ""}`}>{item.label}</span>
          <span className="task-row__detail">{item.detail}</span>
        </span>
        <TaskMetricColumns item={item} />
        {item.state === "running" && (
          <IconButton
            label={stopping
              ? t("正在中止“{label}”", "Stopping “{label}”", { label: item.label })
              : t("中止“{label}”", "Stop “{label}”", { label: item.label })}
            className="task-row__stop"
            disabled={stopping}
            onClick={(event) => {
              event.stopPropagation();
              onStopItem(item);
            }}
          >
            {stopping
              ? <LoaderCircle size={11} className="task-row__stop-spinner" />
              : <Square size={9} fill="currentColor" />}
          </IconButton>
        )}
      </div>
    </li>
  );
}

/**
 * The run's todo plan, at the top of the task sidebar.
 * The plan collapses because it can run to a dozen entries and the tasks below
 * it are what the panel is for; the progress bar keeps it legible while closed.
 */
function TaskStatusHeader({ status }: { status: AgentStatus }) {
  const { t } = useI18n();
  const [planOpen, setPlanOpen] = useState(false);
  const todo: TodoItemView[] = status.todo ?? [];
  const done = todo.filter((item) => item.status === "completed").length;
  const active = todo.find((item) => item.status === "in_progress");
  // A todo may name a dependency that is itself still open, which is the one
  // reason an entry can be pending while nothing blocks the rest of the plan.
  const completedIds = new Set(todo.flatMap((item) => (
    item.id && item.status === "completed" ? [item.id] : []
  )));
  if (!todo.length) return null;
  return (
    <section className="task-status" aria-label={t("计划", "Plan")}>
      {todo.length > 0 && (
        <div role="region" aria-label={t("任务清单", "Task list")}>
          <button
            type="button"
            className="task-status__toggle"
            aria-expanded={planOpen}
            onClick={() => setPlanOpen((current) => !current)}
          >
            <ChevronRight
              size={12}
              aria-hidden="true"
              className={`task-status__chevron${planOpen ? " task-status__chevron--open" : ""}`}
            />
            <span className="task-status__summary">
              {active?.activeForm || active?.content || t("计划", "Plan")}
            </span>
            <span className="task-status__count">{done}/{todo.length}</span>
          </button>
          <div
            className="task-status__progress"
            aria-label={t(
              "任务进度 {completed}/{total}",
              "Task progress {completed}/{total}",
              { completed: done, total: todo.length }
            )}
          >
            <span style={{ width: `${Math.round((done / todo.length) * 100)}%` }} />
          </div>
          {planOpen && (
            <ul className="task-status__todo">
              {todo.map((item, index) => {
                const blocked = item.blockedBy?.filter((id) => !completedIds.has(id)).length ?? 0;
                return (
                  <li
                    key={item.id ?? `${index}-${item.content}`}
                    className={`task-status__todo-item task-status__todo-item--${item.status}`}
                    title={item.description}
                  >
                    <span className="task-status__todo-marker" aria-hidden="true" />
                    <span className="task-status__todo-copy">
                      <span>{item.content}</span>
                      {blocked > 0 && (
                        <small>{t(
                          "等待 {count} 个前置任务",
                          "Waiting for {count} dependencies",
                          { count: blocked }
                        )}</small>
                      )}
                      {blocked === 0 && item.owner && (
                        <small>{t("负责人：{owner}", "Owner: {owner}", { owner: item.owner })}</small>
                      )}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}
    </section>
  );
}

/**
 * Localized task-row copy.
 *
 * Shared with the message stream, which derives the same workflow runs this
 * panel does: two bags would let the same run be labelled two ways a few
 * hundred pixels apart.
 */
export function taskContainerMessages(t: ReturnType<typeof useI18n>["t"]): TaskContainerMessages {
  return {
    workflowLabel: t("工作流", "Workflow"),
    runningStepCount: (running, total) => t(
      "{running}/{total} 个步骤进行中",
      "{running}/{total} steps running",
      { running, total }
    ),
    stepCount: (total) => t("{total} 个步骤", "{total} steps", { total }),
    terminalIdle: t("空闲", "Idle"),
    terminalBusy: t("正在执行命令", "Running a command"),
    shellRunning: t("正在运行", "Running"),
    shellStopping: t("正在中止", "Stopping"),
    shellExited: (code) => t("已失败（退出码 {code}）", "Failed (exit {code})", { code }),
    shellFinished: t("已完成", "Finished"),
    shellFailed: t("已失败", "Failed"),
    shellStopped: t("已中止", "Stopped"),
    browserLabel: t("预览页面", "Preview page"),
    browserLoading: t("正在加载", "Loading"),
    browserSuspended: t("已挂起", "Suspended"),
    browserIdle: t("已就绪", "Ready"),
    browserAutomation: (tool) => t("模型正在操作：{tool}", "Model is driving: {tool}", { tool }),
    userAborted: t("用户中止操作", "Operation aborted by user")
  };
}

/**
 * The conversation's running work as one tree: every subagent, workflow,
 * terminal, shell command, web search and browser page is a row. Children sit at the same left
 * edge as their parents and are collapsed until asked for. Finished rows
 * collapse behind a single "finish" disclosure so the list stays about now.
 */
export function TaskContainer({
  conversationId,
  open,
  agents,
  terminals,
  shellTasks = [],
  browser = null,
  browserSessionId = null,
  browserSessions,
  browserAutomationTool = null,
  browserAutomationStopping = false,
  modelRequestId = null,
  userAbortedTasks = [],
  status = { todo: null },
  selectedAgentId,
  selectedRowId = null,
  stoppingIds = [],
  workflowProgress = {},
  workflowRunIds = {},
  onWorkflowStepControl,
  onSelectAgent,
  onOpenItem,
  onStopItem,
  onClose,
  width = TASK_CONTAINER_DEFAULT_WIDTH,
  onWidthChange,
  onResizeStateChange = () => undefined
}: TaskContainerProps) {
  const { t } = useI18n();
  const [finishedOpen, setFinishedOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const stopping = useMemo(() => new Set(stoppingIds), [stoppingIds]);
  const resizeSessionRef = useRef<{
    pointerId: number;
    startX: number;
    startWidth: number;
    element: HTMLElement;
  } | null>(null);

  useEffect(() => {
    if (!onWidthChange) return undefined;
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
      document.body.classList.remove("task-container-resize-active");
      onResizeStateChange(false);
    };
    const moveResize = (event: PointerEvent) => {
      const session = resizeSessionRef.current;
      if (!session || event.pointerId !== session.pointerId) return;
      if (event.cancelable) event.preventDefault();
      // The container is docked to the right edge, so dragging left widens it.
      onWidthChange(clampTaskContainerWidth(session.startWidth + session.startX - event.clientX));
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
    window.addEventListener("keydown", cancelWithEscape, true);
    return () => {
      window.removeEventListener("pointermove", moveResize);
      window.removeEventListener("pointerup", finishResize);
      window.removeEventListener("pointercancel", finishResize);
      window.removeEventListener("keydown", cancelWithEscape, true);
      document.body.classList.remove("task-container-resize-active");
    };
  }, [onResizeStateChange, onWidthChange]);

  // A background shell command is the one task type that needs a clock of its
  // own. Every other running row belongs to something that streams and
  // re-renders this panel as a side effect, so its elapsed column advances for
  // free. A registry-backed command can emit nothing between start and end.
  //
  // Armed on a running task, not on history: finished rows stay in the list, and
  // their columns are frozen at a real duration, so they must not make this whole
  // panel re-render once a second forever.
  const backgroundTaskRunning = shellTasks.some((task) => task.outcome === null);
  useEffect(() => {
    if (!backgroundTaskRunning) return undefined;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [backgroundTaskRunning]);

  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!onWidthChange || event.button !== 0 || event.isPrimary === false) return;
    event.preventDefault();
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
    document.body.classList.add("task-container-resize-active");
    onResizeStateChange(true);
  };

  const resizeWithKeyboard = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!onWidthChange) return;
    const step = event.shiftKey ? 24 : 12;
    const nextWidth = event.key === "ArrowLeft" ? width + step
      : event.key === "ArrowRight" ? width - step
        : event.key === "Home" ? TASK_CONTAINER_MIN_WIDTH
          : event.key === "End" ? TASK_CONTAINER_MAX_WIDTH
            : null;
    if (nextWidth === null) return;
    event.preventDefault();
    onWidthChange(clampTaskContainerWidth(nextWidth));
  };

  const items = useMemo(() => deriveTaskItems({
    conversationId,
    agents,
    terminals,
    shellTasks,
    browser,
    browserSessionId,
    browserSessions,
    browserAutomationTool,
    browserAutomationStopping,
    modelRequestId,
    userAbortedTasks,
    now
  }, taskContainerMessages(t)), [
    conversationId,
    agents,
    browser,
    browserAutomationStopping,
    browserAutomationTool,
    browserSessionId,
    browserSessions,
    modelRequestId,
    now,
    shellTasks,
    terminals,
    t,
    userAbortedTasks
  ]);

  const running = runningTaskItems(items);
  const finished = finishedTaskItems(items);

  const renderRow = (item: TaskItem) => {
    // A workflow is not a row. It draws its own panel — plan description,
    // phases, and one line per agent with the role's model, its tokens and its
    // wall time — because that is what the run actually is, and because the row
    // it used to be could only offer a transcript that does not exist.
    if (item.kind === "workflow") {
      return (
        <li
          className="task-container__workflow"
          data-workflow-run={item.id}
          key={`${item.kind}:${item.id}`}
        >
          <WorkflowRunPanel
            view={deriveWorkflowRun(item, workflowProgress[item.id] ?? null)}
            selectedAgentId={selectedAgentId}
            stopping={stopping.has(item.id)}
            runId={workflowRunIds[item.id] ?? null}
            onOpenAgent={onSelectAgent}
            onStop={() => onStopItem(item)}
            onStepControl={onWorkflowStepControl}
          />
        </li>
      );
    }
    return (
      <TaskRow
        key={`${item.kind}:${item.id}`}
        item={item}
        selectedAgentId={selectedAgentId}
        selectedRowId={selectedRowId}
        stoppingIds={stopping}
        onSelectAgent={onSelectAgent}
        onOpenItem={onOpenItem}
        onStopItem={onStopItem}
      />
    );
  };

  return (
    <aside
      id={taskContainerId}
      className={`task-container task-container--${open ? "open" : "closed"}`}
      aria-label={t("任务容器", "Tasks")}
      aria-hidden={!open || undefined}
      inert={!open || undefined}
      style={{ width: `${width}px` }}
    >
      {onWidthChange && (
        <div
          className="task-container-resize-handle"
          role="separator"
          aria-label={t("调整任务容器宽度", "Resize the task container")}
          aria-orientation="vertical"
          aria-valuemin={TASK_CONTAINER_MIN_WIDTH}
          aria-valuemax={TASK_CONTAINER_MAX_WIDTH}
          aria-valuenow={width}
          aria-valuetext={t("{width} 像素", "{width} pixels", { width })}
          tabIndex={0}
          title={t(
            "拖动调整任务容器宽度；双击恢复默认宽度",
            "Drag to resize the task container; double-click to restore the default width"
          )}
          onPointerDown={startResize}
          onKeyDown={resizeWithKeyboard}
          onDoubleClick={() => onWidthChange(TASK_CONTAINER_DEFAULT_WIDTH)}
        />
      )}
      <header className="task-container__header">
        <h2 className="task-container__title">{t("任务", "Tasks")}</h2>
        <IconButton label={t("收起任务容器", "Collapse tasks")} onClick={onClose}>
          <PanelRightClose size={16} />
        </IconButton>
      </header>
      <div className="task-container__body">
        <TaskStatusHeader status={status} />
        {!items.length && (
          <p className="task-container__empty">{t("还没有任务", "No tasks yet")}</p>
        )}
        {running.length > 0 && (
          <ul className="task-container__list">{running.map(renderRow)}</ul>
        )}
        {finished.length > 0 && (
          <div className="task-container__finished">
            <button
              type="button"
              className="task-container__finished-toggle"
              aria-expanded={finishedOpen}
              onClick={() => setFinishedOpen((current) => !current)}
            >
              <ChevronRight
                size={12}
                aria-hidden="true"
                className={`task-container__finished-chevron${finishedOpen ? " task-container__finished-chevron--open" : ""}`}
              />
              <span>{t("已完成", "finish")}</span>
              <span className="task-container__finished-count">{finished.length}</span>
            </button>
            {finishedOpen && (
              <ul className="task-container__list">{finished.map(renderRow)}</ul>
            )}
          </div>
        )}
      </div>
    </aside>
  );
}

export { flattenTaskItems };
export type { TaskItem, TaskItemState };
