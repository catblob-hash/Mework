import type { SubagentView, SubagentViewStatus } from "./subagents";
import type { TerminalSessionState } from "./terminal";
import type { ShellTaskSnapshot } from "./shellTasks";
import type { BrowserStatus } from "./browser";
import { previewServerAddress, previewUrlIsServedAt, type PreviewServerSnapshot } from "./preview";
import type { ModelUsage, ConversationPlan, ForkDecisionRecord, UserAbortedTaskKind, UserAbortedTaskRecord } from "../types";

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
  /**
   * One dev server this conversation may address. The process is what the task
   * is: it outlives the round, it holds a port, and stopping it is the only way
   * it ends. The page it serves is not part of the row — it is a view, and the
   * stop takes it down with the process.
   */
  | (TaskItemBase & {
      kind: "preview";
      server: PreviewServerSnapshot;
      /** Where the server answers, as the row's detail and the page-match key. */
      address: string;
    })
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
  /**
   * The conversation's plan document. Not a running task at all — it is the one
   * artifact of plan mode, and the task bar is the only navigation the message
   * area has, so it earns a row. Never has children and never stops.
   */
  | (TaskItemBase & { kind: "plan"; plan: ConversationPlan })
  /**
   * One answered `fork` request. Not work either: the decision is already made
   * by the time the row exists, and the row is the user's only trace of it —
   * the model is never told a fork's outcome, so nothing about this reaches
   * `task_list` or any receipt.
   */
  | (TaskItemBase & { kind: "fork"; decision: ForkDecisionRecord })
  | (TaskItemBase & { kind: "aborted"; sourceKind: UserAbortedTaskKind; sourceIdentity: string });

export interface TaskContainerMessages {
  workflowLabel: string;
  runningStepCount: (running: number, total: number) => string;
  stepCount: (total: number) => string;
  terminalIdle: string;
  terminalBusy: string;
  /** A command's subtitle when it has neither a summary nor any command text to show. */
  shellRunning: string;
  /** A failed command's hover text, when the platform reported an exit code. */
  shellExited: (code: number) => string;
  /** A failure with no exit code the platform could give us. */
  shellFailed: string;
  /** What a dev-server row is called before its configuration name. */
  previewLabel: string;
  previewStarting: string;
  previewRunning: string;
  browserLabel: string;
  browserLoading: string;
  browserSuspended: string;
  browserIdle: string;
  /** What the row says while the model is driving the page with `tool`. */
  browserAutomation: (tool: string) => string;
  planLabel: string;
  planDrafting: string;
  planAwaitingApproval: string;
  planApproved: string;
  planRejected: string;
  /** How long ago the plan was last written, from whole minutes. */
  planUpdatedAgo: (minutes: number) => string;
  /** An approved fork's row, which is also the only way into the child. */
  forkApproved: string;
  forkDeclined: string;
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
  inheritedModelId: string | null,
  toolExplanation?: ToolExplanationLookup
): TaskItem {
  const state = taskStateForStatus(agent.status);
  return {
    kind: "subagent",
    id: agent.id,
    // Who is doing it, then what it is called: `role:name`. The name is the
    // one the model gave this child, which is also the address it talks to it
    // by; `label` is a fallback for legacy views only. A child that named no
    // role has none to lead with, and its name stands alone.
    label: roleTitle(agent.role?.name ?? null, agent.name || agent.label),
    // What the child was asked, as one line: enough to tell two children of the
    // same role apart without opening either. The local helper model's title
    // for the task replaces the excerpt once it lands. A child whose task is
    // not known yet falls back to the model it answers on.
    detail: generatedTaskTitle(agent, toolExplanation)
      || promptPreview(agent.task)
      || agent.modelId
      || inheritedModelId
      || "",
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
  inheritedModelId: string | null,
  toolExplanation?: ToolExplanationLookup
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
    children: steps.map((step) => taskItemForAgent(step, byId, messages, now, inheritedModelId, toolExplanation)),
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
  inheritedModelId: string | null,
  toolExplanation?: ToolExplanationLookup
): TaskItem {
  return isWorkflowRun(agent)
    ? workflowItem(agent, byId, messages, now, inheritedModelId, toolExplanation)
    : subagentItem(agent, byId, messages, now, inheritedModelId, toolExplanation);
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
 * Whether one of these dev servers is the thing this page is showing.
 *
 * The page carries no binding to a server — the host never wrote one — so the
 * committed URL's origin stands in for it, which is also how the preview pane
 * decides which row is open.
 */
function pageIsServedBy(
  page: BrowserStatus,
  servers: PreviewServerSnapshot[]
): boolean {
  return servers.some((server) => previewUrlIsServedAt(page.url, previewServerAddress(server)));
}

/**
 * One dev server as a task row.
 *
 * Always *running*: the registry drops a server the moment its process exits,
 * whoever ended it, so every snapshot that reaches here is a live process
 * holding a port. Which means the row always carries its stop control, which is
 * the whole point — a dev server nobody can see is exactly the thing that
 * outlives the conversation that started it.
 */
function previewServerItem(
  server: PreviewServerSnapshot,
  messages: TaskContainerMessages,
  now: number
): TaskItem {
  const address = previewServerAddress(server);
  return {
    kind: "preview",
    id: `preview-server:${server.handle}`,
    label: server.name,
    // A server on another machine says which: its address is that machine's `localhost`.
    detail: `${server.status === "starting" ? messages.previewStarting : messages.previewRunning} · ${address}${server.machine ? ` · ${server.machine}` : ""}`,
    state: "running",
    server,
    address,
    // A dev server spends no tokens and calls no tools, and `startedAt` is the
    // one quantity it does report, so the elapsed column says how long it has
    // been up — the number that answers "did I leave this running yesterday".
    metrics: {
      childCount: null,
      tokens: null,
      toolCount: null,
      elapsedMs: elapsedMs(server.startedAt, null, now)
    },
    children: [],
    error: null,
    startedAt: server.startedAt,
    endedAt: null
  };
}

/**
 * Whether the page holds a document of its own.
 *
 * `hasPage` says only that the host owns a native surface: opening the preview
 * pane mints one at `about:blank` and the pane paints its own standby card over
 * it. Blank is the browser at rest rather than a tab — no address, no title,
 * nothing to go back to, and nothing in its single-use profile to sign out of —
 * and the surface behind it is the host's to reclaim under its process budget,
 * not the user's to close. Anything that gives a blank page a history of its own
 * brings the row back: a load in flight, a load that failed, and a blank page
 * arrived at from somewhere else each describe a tab somebody has to be able to
 * find again.
 */
function pageHoldsDocument(page: BrowserStatus): boolean {
  if (page.loading || page.error) return true;
  if (page.canGoBack || page.canGoForward) return true;
  return Boolean(page.url) && !/^about:blank$/i.test(page.url);
}

/**
 * The conversation's one browser page as a task. A suspended page is still a
 * task — Mework released its Chromium surface to stay inside the process
 * budget, but the profile and URL survive and reopening resumes it — so it
 * reports its suspended state rather than disappearing from the list. A page
 * that never loaded anything is not one at all: see {@link pageHoldsDocument}.
 */
function browserItem(
  browser: BrowserStatus,
  sessionId: string,
  automationTool: string | null,
  messages: TaskContainerMessages
): TaskItem | null {
  // The host reports a sleeping page as having no live surface (`hasPage: false`), and every tab
  // not in front sleeps, so `suspended` has to keep its row on its own.
  if (!browser.hasPage && !browser.suspended) return null;
  // A page the model is holding is a task whatever is loaded in it: the automation is the work,
  // and the row is where the task list says so and offers to stop it.
  if (!automationTool && !pageHoldsDocument(browser)) return null;
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
 * `role:title`, the way every agent row and workflow step is titled. A run that
 * named no role is titled by its own name alone.
 */
export function roleTitle(role: string | null | undefined, title: string): string {
  const name = role?.trim();
  return name ? `${name}:${title}` : title;
}

/** The text the local helper model stored beside a tool card, by the card's id. */
export type ToolExplanationLookup = (contextId: string) => string | undefined;

/**
 * The title the local helper model made of a child's task, if it has: the host
 * stores it beside the `agent_spawn` card that started the child, so it is
 * found under one of the child's call ids. Made from the spawn's `prompt`
 * alone, never the child's opening message, which a role's template may wrap
 * around it.
 */
export function generatedTaskTitle(
  agent: SubagentView,
  toolExplanation: ToolExplanationLookup | undefined
): string {
  if (!toolExplanation) return "";
  for (const callId of agent.callIds) {
    const title = toolExplanation(callId)?.trim();
    if (title) return title;
  }
  return "";
}

/** Longest task preview a subtitle carries; the row cuts it to its width anyway. */
const PROMPT_PREVIEW_LENGTH = 240;

/** The prompt an agent was given, as one line for a subtitle. */
export function promptPreview(text: string | null | undefined): string {
  const line = (text ?? "").replace(/\s+/g, " ").trim();
  return line.length > PROMPT_PREVIEW_LENGTH ? `${line.slice(0, PROMPT_PREVIEW_LENGTH)}…` : line;
}

/**
 * Where a shell command ran: the directory it started in, or — on a row recorded
 * before the host kept that — its workspace's root. Null when neither is known.
 */
export function shellTaskDirectory(shell: ShellTaskSnapshot): string | null {
  return shell.cwd?.trim() || shell.workspaceRoot?.trim() || null;
}

/**
 * What a shell command's row and page are called: the shell, then the directory
 * it ran in — `zsh:/Users/me/project`. Which directory is the question a list of
 * near-identical `bash` rows could not answer, and the path says it on every
 * machine and in every workspace alike.
 */
export function shellTaskTitle(shell: ShellTaskSnapshot): string {
  const directory = shellTaskDirectory(shell);
  return directory ? `${shell.toolName}:${directory}` : shell.toolName;
}

/**
 * How long a shell command has run. A finished command the machine that ran it
 * timed reports that figure; everything else is the host's span, which ticks
 * while the command runs.
 */
function shellElapsedMs(shell: ShellTaskSnapshot, now: number): number | null {
  if (shell.outcome && typeof shell.durationMs === "number") return shell.durationMs;
  // Passing the real end freezes a finished row at the command's duration
  // instead of letting it count on forever; with no end on record there is no
  // duration to freeze at.
  if (shell.outcome && !shell.endedAt) return null;
  return elapsedMs(shell.startedAt, shell.endedAt, now);
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
  const failed = shell.outcome === "failed";
  return {
    kind: "shell",
    id: shell.shellTaskId,
    // The shell and the directory are the label, and what the command does is
    // the detail, so the row answers "what is running where" without the user
    // opening anything.
    label: shellTaskTitle(shell),
    // The helper model's summary when it wrote one, the command itself
    // otherwise. No status word goes in front: the section a row sits in says
    // whether it is still running, and a failure is the row's colour.
    detail: shell.explanation?.trim() || shell.command || messages.shellRunning,
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
    // runtime is unpredictable.
    metrics: {
      childCount: null,
      tokens: null,
      toolCount: null,
      elapsedMs: shellElapsedMs(shell, now)
    },
    children: [],
    // The exit code is worth surfacing only on a failure: it is the first thing
    // anyone asks about one, and it is noise on a zero. It is the failed row's
    // hover text rather than its subtitle.
    error: failed
      ? shell.exitCode === null
        ? messages.shellFailed
        : messages.shellExited(shell.exitCode)
      : null,
    startedAt: shell.startedAt,
    endedAt: shell.endedAt
  };
}

function taskItemSourceIdentity(item: TaskItem, modelRequestId: string | null = null): string {
  if (item.kind === "aborted") return item.sourceIdentity;
  if (item.kind === "subagent" || item.kind === "workflow") {
    return `agent:${item.agent.callIds.at(-1) ?? item.agent.id}:${item.agent.createdAt}`;
  }
  if (item.kind === "terminal") return `terminal:${item.terminal.terminalId}`;
  if (item.kind === "shell") return `shell:${item.shell.shellTaskId}`;
  // A dev server is identified by the id the host minted for it, which it never
  // reuses: a restarted `dev` is `dev-2`, so a record can never be read back
  // onto a different process.
  if (item.kind === "preview") return `preview-server:${item.server.handle}`;
  if (item.kind === "browser") {
    return item.automationTool
      ? `browser-automation:${modelRequestId ?? "unknown"}:${item.sessionId}`
      : `browser-page:${item.sessionId}`;
  }
  // One plan per conversation, and the rows are already bound to one.
  if (item.kind === "plan") return "plan";
  // The fork id is minted by the host when the request is raised and outlives
  // the card, so it identifies the decision without the child having to exist.
  if (item.kind === "fork") return `fork:${item.decision.forkId}`;
  throw new Error("Unknown task item kind");
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

/**
 * The plan row. Its state is what the user can still do about it: a plan being
 * written or waiting for an answer is live, an answered one is history.
 */
function planItem(
  plan: ConversationPlan,
  awaitingApproval: boolean,
  drafting: boolean,
  messages: TaskContainerMessages,
  now: number
): TaskItem {
  const status = awaitingApproval
    ? messages.planAwaitingApproval
    : plan.status === "approved"
      ? messages.planApproved
      : plan.status === "rejected"
        ? messages.planRejected
        : messages.planDrafting;
  const updated = Date.parse(plan.updatedAt);
  const minutes = Number.isNaN(updated)
    ? 0
    : Math.max(0, Math.round((now - updated) / 60_000));
  return {
    kind: "plan",
    plan,
    id: "plan",
    label: messages.planLabel,
    detail: `${status} · ${messages.planUpdatedAgo(minutes)}`,
    state: awaitingApproval || drafting ? "running" : "finished",
    metrics: { childCount: null, tokens: null, toolCount: null, elapsedMs: null },
    children: [],
    error: null,
    startedAt: plan.createdAt,
    endedAt: null
  };
}

/**
 * One answered `fork` request. Both outcomes are finished rows: an approval
 * handed the job to a conversation that now runs on its own, and a decline is
 * the end of the request — neither is work this conversation is still doing.
 *
 * The row exists because the decision would otherwise leave no trace anywhere
 * the user can see: the model is never told the outcome, so the task bar is the
 * only place it is recorded.
 */
function forkItem(decision: ForkDecisionRecord, messages: TaskContainerMessages): TaskItem {
  return {
    kind: "fork",
    decision,
    id: `fork:${decision.forkId}`,
    // The host derives the title from the prompt when it raises the request;
    // falling back to the prompt's first line covers a record written before it
    // did, and keeps a row from being titled by nothing.
    label: decision.title || decision.prompt.split("\n", 1)[0] || "",
    detail: decision.approved ? messages.forkApproved : messages.forkDeclined,
    state: "finished",
    // The only interval a decision has is how long the card waited for an
    // answer, which measures the user rather than the task; the other three
    // columns are quantities a fork never had.
    metrics: { childCount: null, tokens: null, toolCount: null, elapsedMs: null },
    children: [],
    error: null,
    startedAt: decision.requestedAt,
    endedAt: decision.decidedAt
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
  /**
   * Every dev server this conversation may address, as the host lists them.
   *
   * These are the preview task rows. A page is not one: it is a view of the
   * process, and {@link deriveTaskItems} only draws a page row for a page no
   * server here accounts for.
   */
  previewServers?: PreviewServerSnapshot[];
  /** Native session the browser row closes; the row is omitted without one. */
  browserSessionId?: string | null;
  /**
   * Every live preview session this conversation owns, one row each for every session with a
   * document to show.
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
  /** The conversation's plan document; absent or null draws no plan row. */
  plan?: ConversationPlan | null;
  /** True while a `plan_exit` card for this conversation is waiting. */
  planAwaitingApproval?: boolean;
  /** True while the conversation's run is live and the level is plan mode. */
  planDrafting?: boolean;
  /**
   * Every `fork` request this conversation raised and the user answered, in any
   * order; the rows are sorted by when each was decided. Model-invisible: the
   * host records them for the task bar alone.
   */
  forkDecisions?: ForkDecisionRecord[];
  /**
   * Model the conversation itself is on, shown by a child that bound no model
   * of its own. A role-less child runs on exactly this, and the record cannot
   * say so: the host writes a binding only for a named agent or a fork.
   */
  inheritedModelId?: string | null;
  /**
   * What the local helper model stored beside tool cards. A child's subtitle
   * reads its spawn card's: the title of its task, once one has been made.
   */
  toolExplanation?: ToolExplanationLookup;
  /** Clock for the elapsed column, so a render stays a pure function. */
  now?: number;
}

/**
 * Projects everything the conversation currently has running into one tree of
 * task rows: subagents, workflow runs, terminals, shell commands, and the
 * browser page. Web search and fetch are ordinary in-round tool calls, not tasks,
 * so they have no row here.
 *
 * The plan document and answered fork requests are rows too, without ever having
 * been work: each is an artifact the task bar is the only navigation to.
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
    forkDecisions = [],
    inheritedModelId = null,
    toolExplanation,
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
    items.push(taskItemForAgent(agent, byId, messages, now, inheritedModelId, toolExplanation));
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

  previewServers.forEach((server) => {
    items.push(previewServerItem(server, messages, now));
  });

  // Only the primary session is the surface Agent tools drive, so automation is reported against
  // it alone; an extra tab the model opened is still just a page until it selects it.
  const previewSessions = browserSessions
    ?? (browser && browserSessionId ? [{ sessionId: browserSessionId, status: browser }] : []);
  previewSessions.forEach(({ sessionId, status }) => {
    // A page one of the rows above already accounts for gets no row of its own: the server is the
    // task, and two stop controls over one resource is how a page outlives the process it was
    // serving. What is left is the orphan — an attach entry, a page the model minted, a URL typed
    // into the address bar — which holds a live Chromium process that nothing else can close.
    if (pageIsServedBy(status, previewServers)) return;
    const page = browserItem(
      status,
      sessionId,
      browserSessionId === null || sessionId === browserSessionId ? automation : null,
      messages
    );
    if (page) items.push(page);
  });

  if (plan) {
    items.push(planItem(plan, planAwaitingApproval, planDrafting, messages, now));
  }

  // Sorted here rather than trusted from the caller, so the derived list is the
  // same whatever order the decisions arrived in. The finish list orders every
  // row by when it ended on its own; see `finishedTaskItems`.
  [...forkDecisions]
    .sort((left, right) => left.decidedAt.localeCompare(right.decidedAt))
    .forEach((decision) => {
      items.push(forkItem(decision, messages));
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

/**
 * When a finished row ended: its end, or its start when it recorded no end.
 * A row that knows neither — an exited terminal, a page that failed to load —
 * sorts after every row that does.
 */
function finishedAt(item: TaskItem): number {
  return parseTime(item.endedAt ?? "") ?? parseTime(item.startedAt) ?? Number.NEGATIVE_INFINITY;
}

/**
 * Rows under the "finish" disclosure: every kind in one list, latest to finish
 * first. Ordered by time rather than by kind, so whatever just ended sits right
 * under the running rows it left, and a shell, a subagent and a workflow that
 * ended together read together.
 */
export function finishedTaskItems(items: TaskItem[]): TaskItem[] {
  // The plan is never history: it is the standing artifact of plan mode and the
  // only way into its page, so it stays in the visible list at every status.
  return items
    .filter((item) => item.state !== "running" && item.kind !== "plan")
    .map((item, index) => ({ item, index, at: finishedAt(item) }))
    // Two rows with no time at all subtract to NaN, which falls through to the
    // order they were derived in, as does a tie.
    .sort((left, right) => (right.at - left.at) || (left.index - right.index))
    .map(({ item }) => item);
}

export function runningTaskItems(items: TaskItem[]): TaskItem[] {
  return items.filter((item) => item.state === "running" || item.kind === "plan");
}

/**
 * How many rows of the running list are work still going on. The plan is left
 * out: it sits in that list at every status, and it is a document, not a
 * process — "drafting" is the model's round, which is already on screen.
 */
export function countRunningTasks(items: TaskItem[]): number {
  return items.filter((item) => item.state === "running" && item.kind !== "plan").length;
}

/** Every row of the tree, parents before their own children. */
export function flattenTaskItems(items: TaskItem[]): TaskItem[] {
  return items.flatMap((item) => [item, ...flattenTaskItems(item.children)]);
}
