import {
  Bot,
  ChevronRight,
  GitFork,
  Globe2,
  LoaderCircle,
  NotebookPen,
  Server,
  Square,
  SquareTerminal,
  Terminal,
  Workflow as WorkflowIcon
} from "lucide-react";
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { useI18n } from "../i18n";
import {
  deriveTaskItems,
  finishedTaskItems,
  runningTaskItems,
  shellTaskDirectory
} from "../lib/taskContainer";
import type { TaskContainerMessages, TaskItem } from "../lib/taskContainer";
import type { ConversationPlan, ForkDecisionRecord, UserAbortedTaskRecord } from "../types";
import type { SubagentView } from "../lib/subagents";
import type { TerminalSessionState } from "../lib/terminal";
import type { ShellTaskSnapshot } from "../lib/shellTasks";
import type { BrowserStatus } from "../lib/browser";
import type { PreviewServerSnapshot } from "../lib/preview";
import type { WorkflowProgressView } from "../lib/workflowProgress";
import { deriveWorkflowRun } from "../lib/workflowRuns";
import { subscribeToolExplanations, toolExplanation, toolExplanationVersion } from "../lib/localModel";
import { IconButton } from "./Common";
import { PathText } from "./PathText";
import { RollingNumber } from "./RollingNumber";
import { WorkflowRunPanel } from "./WorkflowRunPanel";
import "./TaskContainer.css";

export interface TasksPaneProps {
  conversationId?: string;
  agents: SubagentView[];
  terminals: TerminalSessionState[];
  /** Shell commands this conversation has run, running and finished; each is one row. */
  shellTasks?: ShellTaskSnapshot[];
  browser?: BrowserStatus | null;
  /** Every dev server this conversation may address; one task row each. */
  previewServers?: PreviewServerSnapshot[];
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
  /** The conversation's plan document; absent or null draws no plan row. */
  plan?: ConversationPlan | null;
  /** True while a `plan_exit` card for this conversation is waiting. */
  planAwaitingApproval?: boolean;
  /** True while the conversation's run is live and the level is plan mode. */
  planDrafting?: boolean;
  /**
   * Fork requests this conversation raised and the user answered. Each is one
   * finished row; an approved one is the only way into the child conversation.
   */
  forkDecisions?: ForkDecisionRecord[];
  /**
   * Model the conversation is on, shown by a child that bound no model of its
   * own. A role-less child runs on exactly this; its record cannot say so.
   */
  inheritedModelId?: string | null;
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
  // A dev server is a process, not a page: the row that can stop it must not
  // look like the row that only shows what it serves.
  if (kind === "preview") return <Server size={size} aria-hidden="true" />;
  if (kind === "browser") return <Globe2 size={size} aria-hidden="true" />;
  if (kind === "plan") return <NotebookPen size={size} aria-hidden="true" />;
  if (kind === "fork") return <GitFork size={size} aria-hidden="true" />;
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
  // A terminal, preview server, preview page or shell row opens its own page in the message area
  // instead of a transcript, and only when the shell supplied a handler for it.
  const pageRow = (
    item.kind === "terminal" || item.kind === "preview" || item.kind === "browser"
    || item.kind === "shell"
    || item.kind === "plan"
    // An approved fork opens the child conversation it created. A declined one
    // created nothing, so its row is a record with nowhere to go.
    || (item.kind === "fork" && item.decision.approved)
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
  const labelClass = `task-row__label${item.state === "running" ? " pulse-text" : ""}`;
  const shellDirectory = item.kind === "shell" ? shellTaskDirectory(item.shell) : null;

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
              wherever it appears. A shell's title is a path, so it gives way
              in the middle like every other path; a failed one keeps the
              row's error as its hover text. */}
          {shellDirectory !== null && item.kind === "shell" ? (
            <PathText
              className={labelClass}
              prefix={`${item.shell.toolName}:`}
              path={shellDirectory}
              title={item.error ? null : undefined}
            />
          ) : (
            <span className={labelClass}>{item.label}</span>
          )}
          <span className="task-row__detail">{item.detail}</span>
        </span>
        <TaskMetricColumns item={item} />
        {/* The plan is a document, not work in flight: there is nothing to stop. */}
        {item.state === "running" && item.kind !== "plan" && (
          <IconButton
            label={stopping
              ? t("正在中止“{label}”", "Stopping “{label}”", { label: item.label })
              // A dev server is stopped and a page is closed. Both end the thing,
              // but "中止" is what you do to work in flight, and a page holding a
              // Chromium process is not work — it is a window left open.
              : item.kind === "preview"
                ? t("停止“{label}”", "Stop “{label}”", { label: item.label })
                : item.kind === "browser" && !item.automationTool
                  ? t("关闭“{label}”", "Close “{label}”", { label: item.label })
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
    shellExited: (code) => t("已失败（退出码 {code}）", "Failed (exit {code})", { code }),
    shellFailed: t("已失败", "Failed"),
    previewLabel: t("开发服务器", "Dev server"),
    previewStarting: t("启动中", "Starting"),
    previewRunning: t("运行中", "Running"),
    browserLabel: t("预览页面", "Preview page"),
    browserLoading: t("正在加载", "Loading"),
    browserSuspended: t("已挂起", "Suspended"),
    browserIdle: t("已就绪", "Ready"),
    browserAutomation: (tool) => t("模型正在操作：{tool}", "Model is driving: {tool}", { tool }),
    planLabel: t("实施计划", "Implementation plan"),
    planDrafting: t("撰写中", "Drafting"),
    planAwaitingApproval: t("待批准", "Awaiting approval"),
    planApproved: t("已批准", "Approved"),
    planRejected: t("已退回", "Sent back"),
    planUpdatedAgo: (minutes) => {
      if (minutes < 1) return t("刚刚更新", "Updated just now");
      if (minutes < 60) {
        return t("{count} 分钟前更新", minutes === 1 ? "Updated {count} minute ago" : "Updated {count} minutes ago", { count: minutes });
      }
      if (minutes < 1440) {
        const hours = Math.round(minutes / 60);
        return t("{count} 小时前更新", hours === 1 ? "Updated {count} hour ago" : "Updated {count} hours ago", { count: hours });
      }
      const days = Math.round(minutes / 1440);
      return t("{count} 天前更新", days === 1 ? "Updated {count} day ago" : "Updated {count} days ago", { count: days });
    },
    forkApproved: t("已创建子对话 · 点击打开", "Child conversation created · open"),
    forkDeclined: t("用户拒绝了分叉", "The user declined the fork"),
    userAborted: t("用户中止操作", "Operation aborted by user")
  };
}

/**
 * The conversation's running work as one tree: every subagent, workflow,
 * terminal, shell command, web search and dev server is a row. Children sit at the same left
 * edge as their parents and are collapsed until asked for. Finished rows of
 * every kind share one list under a single "finish" disclosure — open until
 * the user folds it, latest to finish first — so what just ended sits right
 * under what is still running.
 *
 * A preview page is the one thing here that is not work: it is a view of a dev
 * server, so it earns a row only when no server accounts for it — and then only
 * because nothing else can close it.
 */
export function TasksPane({
  conversationId,
  agents,
  terminals,
  shellTasks = [],
  browser = null,
  previewServers = [],
  browserSessionId = null,
  browserSessions,
  browserAutomationTool = null,
  browserAutomationStopping = false,
  modelRequestId = null,
  userAbortedTasks = [],
  plan = null,
  planAwaitingApproval = false,
  planDrafting = false,
  forkDecisions,
  inheritedModelId = null,
  selectedAgentId,
  selectedRowId = null,
  stoppingIds = [],
  workflowProgress = {},
  workflowRunIds = {},
  onWorkflowStepControl,
  onSelectAgent,
  onOpenItem,
  onStopItem
}: TasksPaneProps) {
  const { t } = useI18n();
  const [finishedOpen, setFinishedOpen] = useState(true);
  const [now, setNow] = useState(() => Date.now());
  const stopping = useMemo(() => new Set(stoppingIds), [stoppingIds]);

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

  // A child's subtitle turns into the local helper model's title for its task
  // when that arrives, so the rows are derived again on every arrival.
  const explanationVersion = useSyncExternalStore(subscribeToolExplanations, toolExplanationVersion);

  // biome-ignore lint/correctness/useExhaustiveDependencies: `explanationVersion` is the moment `toolExplanation` reads something new, though nothing here reads the version.
  const items = useMemo(() => deriveTaskItems({
    conversationId,
    agents,
    terminals,
    shellTasks,
    browser,
    previewServers,
    browserSessionId,
    browserSessions,
    browserAutomationTool,
    browserAutomationStopping,
    modelRequestId,
    userAbortedTasks,
    plan,
    planAwaitingApproval,
    planDrafting,
    forkDecisions,
    inheritedModelId,
    toolExplanation,
    now
  }, taskContainerMessages(t)), [
    conversationId,
    agents,
    browser,
    browserAutomationStopping,
    browserAutomationTool,
    browserSessionId,
    browserSessions,
    explanationVersion,
    forkDecisions,
    inheritedModelId,
    modelRequestId,
    now,
    plan,
    planAwaitingApproval,
    planDrafting,
    previewServers,
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
    <div className="tasks-pane">
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
  );
}

export type { TaskItem };
