import type { SubagentView, SubagentViewStatus } from "./subagents";
import type { TerminalSessionState } from "./terminal";
import type { ShellTaskSnapshot } from "./shellTasks";
import type { BrowserStatus } from "./browser";
import type { ModelUsage, UserAbortedTaskKind, UserAbortedTaskRecord } from "../types";

/**
 * Whether a task row is still doing work. Terminals and browser pages have no
 * run status of their own, so a live shell or a loading page counts as running.
 */
export type TaskItemState = "running" | "finished" | "failed";

/** The three colors a workflow square can take, and the row-level status dot. */
export function taskStateForStatus(status: SubagentViewStatus): TaskItemState {
  if (status === "running") return "running";
  // A run that reached a ceiling still carried its task as far as it was
  // allowed to and returned usable output, so it reads as finished. Only a run
  // that ended some other way — a provider error, a deliberate stop, a lost
  // parent turn — is a failure.
  if (status === "completed" || status === "roundLimit") return "finished";
  return "failed";
}

export interface WorkflowPhaseGroup {
  /** Declared phase name, or null for steps the plan left unlabelled. */
  phase: string | null;
  /** Plan order; unnamed phases sort after every named one. */
  phaseIndex: number;
  steps: SubagentView[];
}

/**
 * The four columns every task row shows. A kind that cannot report a column
 * leaves it null rather than zero: a browser page has no token total, and "—"
 * is a different statement than "0".
 */
export interface TaskMetrics {
  /** Direct children, which is what the row's disclosure would expand to. */
  childCount: number | null;
  /** Tokens this task alone spent, never including its children's. */
  tokens: number | null;
  toolCount: number | null;
  /** Wall time in ms, or null when the task reports no start time. */
  elapsedMs: number | null;
}

/** Every task kind the sidebar can show. */
export type TaskItemKind =
  | "subagent"
  | "workflow"
  | "terminal"
  | "shell"
  | "browser";

interface TaskItemBase {
  /** Source identity captured when the row is derived, never the active selection. */
  conversationId?: string;
  id: string;
  label: string;
  detail: string;
  state: TaskItemState;
  metrics: TaskMetrics;
  /** Rows this row's disclosure reveals. Empty for a leaf. */
  children: TaskItem[];
  /**
   * Failure text shown on hover. Only ever set on a failed row — a finished row
   * with a stale error would claim a failure that did not happen.
   */
  error: string | null;
  startedAt: string;
  endedAt: string | null;
}

export type TaskItem =
  | (TaskItemBase & { kind: "subagent"; agent: SubagentView })
  | (TaskItemBase & {
      kind: "workflow";
      agent: SubagentView;
      /** Every step of the run, in plan order, flattened across phases. */
      steps: SubagentView[];
      phases: WorkflowPhaseGroup[];
    })
  | (TaskItemBase & { kind: "terminal"; terminal: TerminalSessionState })
  | (TaskItemBase & { kind: "shell"; shell: ShellTaskSnapshot })
  | (TaskItemBase & {
      kind: "browser";
      browser: BrowserStatus;
      /**
       * The native session the row closes. `BrowserStatus` does not carry it —
       * the host looks it up by key — so the caller that supplied the status
       * supplies its id alongside.
       */
      sessionId: string;
      /**
       * Browser tool the model is driving this page with, or null when nobody
       * is. It decides what the row's stop control means: stopping a page the
       * model is holding stops the automation, not the page.
       */
      automationTool: string | null;
    })
  | (TaskItemBase & { kind: "aborted"; sourceKind: UserAbortedTaskKind; sourceIdentity: string });

export interface TaskContainerMessages {
  workflowLabel: string;
  runningStepCount: (running: number, total: number) => string;
  stepCount: (total: number) => string;
  terminalIdle: string;
  terminalBusy: string;
  /** What the row calls a running command; the command text is the detail. */
  shellRunning: string;
  shellStopping: string;
  /** A failed command's row, when the platform reported an exit code. */
  shellExited: (code: number) => string;
  shellFinished: string;
  /** A failure with no exit code the platform could give us. */
  shellFailed: string;
  /** A command a person killed, which is neither success nor breakage. */
  shellStopped: string;
  browserLabel: string;
  browserLoading: string;
  browserSuspended: string;
  browserIdle: string;
  /** What the row says while the model is driving the page with `tool`. */
  browserAutomation: (tool: string) => string;
  userAborted: string;
}

/**
 * A workflow run is the parent view of the `workflowStep` children its script
 * starts. It is identified by the call that started it, not by whether it has
 * a step yet: a run whose first step has not spawned — or one that failed
 * before it spawned any — is still a run, and falling through to a subagent row
 * gave it the one thing it must never have, a transcript to open.
 */
function isWorkflowRun(agent: SubagentView): boolean {
  return agent.workflowRun;
}

/** Steps in plan order: phase index first, then the order the tree emitted them. */
function groupStepsByPhase(steps: SubagentView[]): WorkflowPhaseGroup[] {
  const groups: WorkflowPhaseGroup[] = [];
  steps.forEach((step) => {
    // A step with no declared phase still belongs to a group, keyed by its own
    // absence: an unphased plan then renders as exactly one anonymous group
    // rather than one group per step.
    const existing = groups.find((group) => group.phase === (step.phase ?? null));
    if (existing) {
      existing.steps.push(step);
      if (step.phaseIndex !== null) {
        existing.phaseIndex = Math.min(existing.phaseIndex, step.phaseIndex);
      }
      return;
    }
    groups.push({
      phase: step.phase ?? null,
      phaseIndex: step.phaseIndex ?? Number.MAX_SAFE_INTEGER,
      steps: [step]
    });
  });
  return groups.sort((left, right) => left.phaseIndex - right.phaseIndex);
}

/** The worst state among a run's steps decides the run's own square color. */
function workflowState(agent: SubagentView, steps: SubagentView[]): TaskItemState {
  const own = taskStateForStatus(agent.status);
  if (own === "running") return "running";
  if (steps.some((step) => taskStateForStatus(step.status) === "failed")) return "failed";
  return own;
}

function parseTime(value: string): number | null {
  if (!value) return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/**
 * Wall time of one task. A running task is measured against `now` rather than
 * left null, so its row ticks; `now` is passed in so a render is a pure
 * function of its inputs and a test can pin the clock.
 */
function elapsedMs(startedAt: string, endedAt: string | null, now: number): number | null {
  const start = parseTime(startedAt);
  if (start === null) return null;
  const end = endedAt ? parseTime(endedAt) : now;
  if (end === null) return null;
  // A record whose end timestamp precedes its start is a clock artifact, not a
  // negative duration; clamping keeps the column monotonic.
  return Math.max(0, end - start);
}

/**
 * Tokens to show for one agent. `totalTokens` is what the provider reported;
 * falling back to the sum of the parts covers a provider that reports only the
 * breakdown. Cached input is excluded from the fallback because it is already
 * inside `inputTokens`.
 */
function tokenTotal(usage: ModelUsage): number | null {
  if (usage.totalTokens !== undefined) return usage.totalTokens;
  const parts = [usage.inputTokens, usage.outputTokens].filter(
    (value): value is number => value !== undefined
  );
  return parts.length ? parts.reduce((total, value) => total + value, 0) : null;
}

/**
 * The failure text a red row shows on hover. A run that ended badly says so in
 * its summary — that is the last thing it managed to report — so there is no
 * separate error channel to read.
 */
function agentError(agent: SubagentView, state: TaskItemState): string | null {
  return state === "failed" ? agent.summary || null : null;
}

function agentMetrics(agent: SubagentView, childCount: number, now: number): TaskMetrics {
  return {
    childCount,
    tokens: tokenTotal(agent.usage),
    toolCount: agent.toolCount,
    elapsedMs: elapsedMs(agent.createdAt, agent.completedAt, now)
  };
}

/**
 * One row per ordinary subagent. It has no children: a subagent cannot spawn
 * another one (`SUBAGENT_DISABLED_TOOL_NAMES` withholds `agent_spawn` and
 * `workflow` from every child request, and both runners reject a forged call at
 * depth ≥ 1), so the only real tree in the sidebar is a workflow and its steps.
 */
function subagentItem(
  agent: SubagentView,
  _byId: Map<string, SubagentView>,
  _messages: TaskContainerMessages,
  now: number,
  inheritedModelId: string | null
): TaskItem {
  const state = taskStateForStatus(agent.status);
  return {
    kind: "subagent",
    id: agent.id,
    // The name the model gave this child, which is also the address it talks to
    // it by. `label` is a fallback for legacy views only: it is free text the
    // model writes, and what it usually writes there is a précis of the task —
    // so a row titled by it says what was asked rather than who is doing it,
    // and two children of the same role become two rows with the same title.
    label: agent.name || agent.label,
    // Which role is answering — never what the child was told. The task the
    // model sent is the child's own transcript; repeating it here made every
    // row a wall of prompt text and told the reader nothing they could act on.
    //
    // The role is the name the user configured and the model selects by, so it
    // is the fact worth a row: two children on the same model but different
    // roles are doing different jobs, and the model ID said they were the same.
    // A child that named no role has none, and then the model it inherits is
    // the only thing left to say.
    detail: agent.role?.name ?? agent.modelId ?? inheritedModelId ?? "",
    state,
    agent,
    metrics: agentMetrics(agent, 0, now),
    children: [],
    error: agentError(agent, state),
    startedAt: agent.createdAt,
    endedAt: agent.completedAt
  };
}

function workflowItem(
  agent: SubagentView,
  byId: Map<string, SubagentView>,
  messages: TaskContainerMessages,
  now: number,
  inheritedModelId: string | null
): Extract<TaskItem, { kind: "workflow" }> {
  const steps = agent.childIds.flatMap((childId) => {
    const child = byId.get(childId);
    return child && child.kind === "workflowStep" ? [child] : [];
  });
  const running = steps.filter((step) => taskStateForStatus(step.status) === "running").length;
  const state = workflowState(agent, steps);
  return {
    kind: "workflow",
    id: agent.id,
    // Named by the model, exactly like a child agent. The plan's own name is
    // the detail below it: what the run is called and what it is running are
    // two different facts, and only the first is an address.
    label: agent.name || agent.label || messages.workflowLabel,
    detail: agent.scriptName ?? (running > 0
      ? messages.runningStepCount(running, steps.length)
      : messages.stepCount(steps.length)),
    state,
    agent,
    steps,
    phases: groupStepsByPhase(steps),
    // A workflow's steps are ordinary rows in the same tree, not a second kind
    // of control: the user's layout puts them at the same left edge as any
    // other child.
    metrics: agentMetrics(agent, steps.length, now),
    children: steps.map((step) => taskItemForAgent(step, byId, messages, now, inheritedModelId)),
    error: agentError(agent, state),
    startedAt: agent.createdAt,
    endedAt: agent.completedAt
  };
}

function taskItemForAgent(
  agent: SubagentView,
  byId: Map<string, SubagentView>,
  messages: TaskContainerMessages,
  now: number,
  inheritedModelId: string | null
): TaskItem {
  return isWorkflowRun(agent)
    ? workflowItem(agent, byId, messages, now, inheritedModelId)
    : subagentItem(agent, byId, messages, now, inheritedModelId);
}

/**
 * Just the workflow runs, for a caller that renders them outside the task tree.
 *
 * The message stream draws the same runs the task panel does, so it has to
 * agree with it on what a run's steps, phases and metrics are. Sharing this
 * entry point is what makes that agreement structural instead of a convention
 * two call sites are trusted to keep.
 *
 * A workflow only ever runs at the conversation's own level — a child's tool
 * set has the descriptor removed outright, so no agent can start one — which is
 * why this walks the roots rather than the whole tree.
 */
export function deriveWorkflowItems(
  agents: SubagentView[],
  messages: TaskContainerMessages,
  now: number = Date.now(),
  inheritedModelId: string | null = null
): Extract<TaskItem, { kind: "workflow" }>[] {
  const byId = new Map(agents.map((agent) => [agent.id, agent]));
  return agents.flatMap((agent) => (
    agent.depth === 0 && isWorkflowRun(agent)
      ? [workflowItem(agent, byId, messages, now, inheritedModelId)]
      : []
  ));
}

/**
 * Stand-in tool name for a page whose automation stop is in flight but whose
 * last driving tool has already left the run's stream. It only ever reaches the
 * `browserAutomation` message, never a comparison.
 */
const stoppingAutomationTool = "browser";

/**
 * The conversation's one browser page as a task. A suspended page is still a
 * task — Mework released its Chromium surface to stay inside the process
 * budget, but the profile and URL survive and reopening resumes it — so it
 * reports its suspended state rather than disappearing from the list.
 */
function browserItem(
  browser: BrowserStatus,
  sessionId: string,
  automationTool: string | null,
  messages: TaskContainerMessages
): TaskItem | null {
  if (!browser.hasPage) return null;
  const suspended = Boolean(browser.suspended);
  const detail = automationTool
    ? messages.browserAutomation(automationTool)
    : suspended
      ? messages.browserSuspended
      : browser.loading
        ? messages.browserLoading
        : messages.browserIdle;
  // A page that failed to load is the one preview state worth painting red. Everything else is
  // *running*, including a page just sitting there: it holds a live Chromium process and a
  // single-use profile until someone closes it, and a task list that called that "finished" was
  // describing the page's loading spinner rather than the resource.
  //
  // This is also the row's only close affordance. `TaskRow` renders the stop control on running
  // rows, and with the tab strip's × gone an idle preview marked finished would collapse into the
  // finished section as something the user could see and never get rid of.
  const state: TaskItemState = browser.error ? "failed" : "running";
  return {
    kind: "browser",
    // One row per native session. A fixed id was enough while every other tab was reachable from
    // the tab strip; now it would collapse three Agent tabs into one row pointing at one of them.
    id: `preview:${sessionId}`,
    label: browser.title?.trim() || browser.url || messages.browserLabel,
    detail,
    state,
    browser,
    sessionId,
    automationTool,
    // The status carries no open time — `suspendedAtMs` is when the surface was
    // released, which is a different quantity from how long the page has been a
    // task — so the elapsed column stays honest and empty.
    metrics: { childCount: null, tokens: null, toolCount: null, elapsedMs: null },
    children: [],
    error: browser.error ?? null,
    startedAt: "",
    endedAt: null
  };
}

/**
 * One shell command as a task row, running or finished. A finished command keeps
 * its row on purpose: the question a user has after a build is "did it pass",
 * and a row that vanished at the exact moment it could answer that never got to.
 */
function shellItem(
  shell: ShellTaskSnapshot,
  messages: TaskContainerMessages,
  now: number
): TaskItem {
  // The command text is the detail whatever the state, so the row still answers
  // "which command was that" once it is in the collapsed finish list. What
  // changes is the status word in front of it.
  const command = shell.command || messages.shellRunning;
  const status = shell.outcome
    ? shell.outcome === "succeeded"
      ? messages.shellFinished
      : shell.outcome === "stopped"
        ? messages.shellStopped
        // The exit code is worth surfacing only on a failure: it is the first
        // thing anyone asks about one, and it is noise on a zero.
        : shell.exitCode === null
          ? messages.shellFailed
          : messages.shellExited(shell.exitCode)
    : shell.stopping
      ? messages.shellStopping
      : null;
  return {
    kind: "shell",
    id: shell.shellTaskId,
    // The tool name is the label and the command is the detail, so the row
    // answers "what is running" without the user opening anything.
    label: shell.toolName,
    detail: status ? `${status} · ${command}` : command,
    // A stopped process is only classified as a user-aborted failure when the
    // taskbar has persisted the corresponding abort record. Other cancellation
    // paths keep the registry's neutral stopped outcome.
    state: !shell.outcome
      ? "running"
      : shell.outcome === "failed"
        ? "failed"
        : "finished",
    shell,
    // Only the elapsed column can say anything: a shell command spawns no
    // children, spends no tokens and calls no tools. It is also the column that
    // matters most here — the whole reason the row exists is that a command's
    // runtime is unpredictable. Passing the real end freezes it at the
    // command's duration instead of letting a finished row count on forever.
    metrics: {
      childCount: null,
      tokens: null,
      toolCount: null,
      elapsedMs: shell.outcome && !shell.endedAt
        ? null
        : elapsedMs(shell.startedAt, shell.endedAt, now)
    },
    children: [],
    error: null,
    startedAt: shell.startedAt,
    endedAt: shell.endedAt
  };
}

export function taskItemSourceIdentity(item: TaskItem, modelRequestId: string | null = null): string {
  if (item.kind === "aborted") return item.sourceIdentity;
  if (item.kind === "subagent" || item.kind === "workflow") {
    return `agent:${item.agent.callIds.at(-1) ?? item.agent.id}:${item.agent.createdAt}`;
  }
  if (item.kind === "terminal") return `terminal:${item.terminal.terminalId}`;
  if (item.kind === "shell") return `shell:${item.shell.shellTaskId}`;
  if (item.kind === "browser") {
    return item.automationTool
      ? `browser-automation:${modelRequestId ?? "unknown"}:${item.sessionId}`
      : `browser-page:${item.sessionId}`;
  }
  throw new Error("Unknown task item kind");
}

export function userAbortedTaskRecord(
  item: TaskItem,
  modelRequestId: string | null,
  id: string,
  endedAt: string
): UserAbortedTaskRecord {
  const sourceKind = item.kind === "aborted" ? item.sourceKind : item.kind;
  return {
    id,
    sourceKind,
    sourceIdentity: taskItemSourceIdentity(item, modelRequestId),
    label: item.label,
    detail: item.detail,
    metrics: { ...item.metrics },
    startedAt: item.startedAt,
    endedAt,
    reason: "userAborted"
  };
}

function abortedTaskItem(record: UserAbortedTaskRecord, messages: TaskContainerMessages): TaskItem {
  return {
    kind: "aborted",
    sourceKind: record.sourceKind,
    sourceIdentity: record.sourceIdentity,
    id: `aborted:${record.id}`,
    label: record.label,
    detail: record.detail ? `${messages.userAborted} · ${record.detail}` : messages.userAborted,
    state: "failed",
    metrics: {
      ...record.metrics,
      elapsedMs: elapsedMs(record.startedAt, record.endedAt, Date.parse(record.endedAt))
        ?? record.metrics.elapsedMs
    },
    children: [],
    error: messages.userAborted,
    startedAt: record.startedAt,
    endedAt: record.endedAt
  };
}

export interface TaskSources {
  conversationId?: string;
  agents: SubagentView[];
  terminals: TerminalSessionState[];
  /**
   * Shell commands this conversation has run, running and finished. The registry
   * retains finished ones — bounded per conversation — so the sidebar can say how
   * a command went instead of only that one is in flight.
   */
  shellTasks?: ShellTaskSnapshot[];
  browser?: BrowserStatus | null;
  /** Native session the browser row closes; the row is omitted without one. */
  browserSessionId?: string | null;
  /**
   * Every live preview session this conversation owns, one row each.
   *
   * The single `browser`/`browserSessionId` pair above predates the tab strip's removal: it
   * described whichever tab the sidebar happened to be showing, which was enough while every
   * other tab was still reachable from that strip. Without a strip, a session with no row of its
   * own is a Chromium process the user can neither see nor close, so an Agent that opens three
   * tabs must produce three rows. When this is supplied it replaces the pair.
   */
  browserSessions?: { sessionId: string; status: BrowserStatus }[];
  /**
   * Browser tool the model is driving the page with right now, or null. It is
   * read from the live run rather than from `BrowserStatus`, because the status
   * describes the page and this describes who is holding it.
   */
  browserAutomationTool?: string | null;
  /** Whether a requested automation stop has not yet been observed. */
  browserAutomationStopping?: boolean;
  modelRequestId?: string | null;
  userAbortedTasks?: UserAbortedTaskRecord[];
  /**
   * Model the conversation itself is on, shown by a child that bound no model
   * of its own. A role-less child runs on exactly this, and the record cannot
   * say so: the host writes a binding only for a named agent or a fork.
   */
  inheritedModelId?: string | null;
  /** Clock for the elapsed column, so a render stays a pure function. */
  now?: number;
}

/**
 * Projects everything the conversation currently has running into one tree of
 * task rows: subagents, workflow runs, terminals, shell commands, and the
 * browser page. Web search and fetch are ordinary in-round tool calls, not tasks,
 * so they have no row here.
 *
 * Only depth-0 agents become top-level rows. Their descendants are `children`
 * of those rows rather than siblings, which is what lets the sidebar render one
 * tree instead of two controls.
 */
export function deriveTaskItems(
  sources: TaskSources,
  messages: TaskContainerMessages
): TaskItem[] {
  const {
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
    inheritedModelId = null,
    now = Date.now()
  } = sources;
  const byId = new Map(agents.map((agent) => [agent.id, agent]));
  const items: TaskItem[] = [];
  // A stop that has been asked for but not yet observed is still automation:
  // the row has to stay visible and running until the model actually lets go,
  // and the stop button on it has to stay rendered to show its pending spinner.
  const automation = browserAutomationStopping
    ? browserAutomationTool ?? stoppingAutomationTool
    : browserAutomationTool;

  agents.forEach((agent) => {
    if (agent.depth !== 0) return;
    items.push(taskItemForAgent(agent, byId, messages, now, inheritedModelId));
  });

  terminals.forEach((terminal) => {
    items.push({
      kind: "terminal",
      id: terminal.terminalId,
      label: terminal.label,
      detail: terminal.busy ? messages.terminalBusy : messages.terminalIdle,
      // A terminal that has exited is done; one that is still up is a live
      // process the user can still stop, whether or not a command is running.
      state: terminal.phase === "running" ? "running" : "finished",
      terminal,
      metrics: { childCount: null, tokens: null, toolCount: null, elapsedMs: null },
      children: [],
      error: null,
      startedAt: "",
      endedAt: null
    });
  });

  shellTasks.forEach((shell) => {
    items.push(shellItem(shell, messages, now));
  });

  // Only the primary session is the surface Agent tools drive, so automation is reported against
  // it alone; an extra tab the model opened is still just a page until it selects it.
  const previewSessions = browserSessions
    ?? (browser && browserSessionId ? [{ sessionId: browserSessionId, status: browser }] : []);
  previewSessions.forEach(({ sessionId, status }) => {
    const page = browserItem(
      status,
      sessionId,
      browserSessionId === null || sessionId === browserSessionId ? automation : null,
      messages
    );
    if (page) items.push(page);
  });

  const abortedBySource = new Map(userAbortedTasks.map((record) => [record.sourceIdentity, record]));
  const liveSources = new Set(items.map((item) => taskItemSourceIdentity(item, modelRequestId)));
  const merged = items.map((item) => {
    const sourceIdentity = taskItemSourceIdentity(item, modelRequestId);
    // A preview is a state, not a task with a history. Closing one is the page
    // ceasing to exist, so there is nothing for an abort record to describe and
    // nothing to fold into "finished" — the row simply goes. The host no longer
    // writes these records; the filter stays because an app-data file written
    // before that change still carries some, and resurrecting a dead page as a
    // permanent failed row is exactly the outcome this rules out.
    const record = sourceIdentity.startsWith("browser-page:")
      ? undefined
      : abortedBySource.get(sourceIdentity);
    return record ? abortedTaskItem(record, messages) : item;
  });
  userAbortedTasks.forEach((record) => {
    if (record.sourceIdentity.startsWith("browser-page:")) return;
    if (!liveSources.has(record.sourceIdentity)) {
      merged.push(abortedTaskItem(record, messages));
    }
  });

  const bindSource = (item: TaskItem): TaskItem => ({
    ...item,
    conversationId: sources.conversationId,
    children: item.children.map(bindSource)
  });
  return merged.map(bindSource);
}

/** Rows the "finish" disclosure hides, in the order they finished. */
export function finishedTaskItems(items: TaskItem[]): TaskItem[] {
  return items.filter((item) => item.state !== "running");
}

export function runningTaskItems(items: TaskItem[]): TaskItem[] {
  return items.filter((item) => item.state === "running");
}

/**
 * The agent a task row opens in the main area; other kinds have none.
 *
 * A workflow run is one of the other kinds. It is a script, not an agent, and
 * the transcript behind its view is the driver's own synthetic step list —
 * handing it back here is how it used to end up in the read-only panel.
 */
export function taskItemAgent(item: TaskItem): SubagentView | null {
  return item.kind === "subagent" ? item.agent : null;
}

/**
 * A stable fingerprint of the whole task tree, used to decide whether the
 * sidebar button should show its unread dot. It folds in every row's identity
 * AND its state, because the user asked for the dot on *any* change — a task
 * that merely finished is news too. Elapsed time is deliberately excluded: it
 * changes every second and would pin the dot on forever.
 *
 * It reads the sources rather than derived rows so the caller does not have to
 * build the localized message table just to know whether anything moved; the
 * fingerprint is then the same in every language.
 */
export function taskActivitySignature(sources: TaskSources): string {
  const parts: string[] = [];
  sources.agents.forEach((agent) => {
    parts.push(`a:${agent.id}:${agent.status}:${agent.childIds.length}:${agent.toolCount}`);
  });
  sources.terminals.forEach((terminal) => {
    parts.push(`t:${terminal.terminalId}:${terminal.phase}:${terminal.busy ? 1 : 0}`);
  });
  sources.shellTasks?.forEach((shell) => {
    // The outcome is folded in, not just the id: a command finishing is the one
    // change the user most wants the dot for, and it no longer announces itself
    // by the row disappearing.
    parts.push(`s:${shell.shellTaskId}:${shell.outcome ?? "running"}:${shell.stopping ? 1 : 0}`);
  });
  const browser = sources.browser;
  if (browser?.hasPage && sources.browserSessionId) {
    parts.push([
      "b",
      sources.browserSessionId,
      browser.suspended ? "suspended" : browser.loading ? "loading" : "idle",
      browser.url,
      browser.error ?? "",
      // Which tool is driving the page is news in its own right: the page can
      // sit at one idle URL for a dozen automation steps, and without this the
      // dot would never light up for any of them.
      sources.browserAutomationTool ?? "",
      sources.browserAutomationStopping ? "stopping" : ""
    ].join(":"));
  }
  sources.userAbortedTasks?.forEach((task) => {
    parts.push(`x:${task.id}:${task.sourceKind}:${task.sourceIdentity}:${task.endedAt}`);
  });
  return parts.join("|");
}

/** Whether the sources describe any task at all, however it ended. */
export function hasAnyTask(sources: TaskSources): boolean {
  if (sources.agents.length > 0 || sources.terminals.length > 0) return true;
  if ((sources.shellTasks?.length ?? 0) > 0) return true;
  if (sources.browser?.hasPage && sources.browserSessionId) return true;
  return (sources.userAbortedTasks?.length ?? 0) > 0;
}

export interface TaskUnreadState {
  /** Signature the user has already looked at. */
  seen: string;
  /** Conversation the signature belongs to; a switch is not itself news. */
  conversationId: string | null;
}

export interface TaskUnreadInput {
  conversationId: string | null;
  signature: string;
  hasTasks: boolean;
  /** Whether the task panel is on screen right now. */
  panelOpen: boolean;
}

/**
 * Advances the unread latch. Opening the panel marks the current tree as seen,
 * which is what makes the dot disappear; the next change to the set or to any
 * task's status makes the signature differ again and the dot returns. Arriving
 * at a conversation adopts its signature so merely switching never lights up.
 */
export function nextTaskUnreadState(
  previous: TaskUnreadState,
  input: TaskUnreadInput
): TaskUnreadState {
  if (previous.conversationId !== input.conversationId) {
    return { seen: input.signature, conversationId: input.conversationId };
  }
  if (input.panelOpen && previous.seen !== input.signature) {
    return { seen: input.signature, conversationId: input.conversationId };
  }
  return previous;
}

/** Whether the button should paint its dot for this latch and these sources. */
export function taskUnreadVisible(state: TaskUnreadState, input: TaskUnreadInput): boolean {
  if (!input.hasTasks || input.panelOpen) return false;
  if (state.conversationId !== input.conversationId) return false;
  return state.seen !== input.signature;
}

/** Every row of the tree, parents before their own children. */
export function flattenTaskItems(items: TaskItem[]): TaskItem[] {
  return items.flatMap((item) => [item, ...flattenTaskItems(item.children)]);
}
