import { describe, expect, it } from "vitest";
import {
  deriveTaskItems,
  finishedTaskItems,
  flattenTaskItems,
  hasAnyTask,
  nextTaskUnreadState,
  runningTaskItems,
  taskActivitySignature,
  taskItemAgent,
  taskStateForStatus,
  taskUnreadVisible
} from "./taskContainer";
import type {
  TaskContainerMessages,
  TaskUnreadInput,
  TaskUnreadState
} from "./taskContainer";
import type { SubagentView } from "./subagents";
import type { TerminalSessionState } from "./terminal";
import type { ShellTaskSnapshot } from "./shellTasks";
import type { BrowserStatus } from "./browser";
import type { UserAbortedTaskRecord } from "../types";

const abortedTask = (overrides: Partial<UserAbortedTaskRecord> = {}): UserAbortedTaskRecord => ({
  id: "abort-1",
  sourceKind: "terminal",
  sourceIdentity: "terminal:terminal-1",
  label: "构建终端",
  detail: "正在执行命令",
  metrics: { childCount: null, tokens: null, toolCount: null, elapsedMs: null },
  startedAt: new Date(1_000).toISOString(),
  endedAt: new Date(6_000).toISOString(),
  reason: "userAborted",
  ...overrides
});

const messages: TaskContainerMessages = {
  workflowLabel: "工作流",
  runningStepCount: (running, total) => `${running}/${total} 个步骤进行中`,
  stepCount: (total) => `${total} 个步骤`,
  terminalIdle: "空闲",
  terminalBusy: "正在执行命令",
  shellRunning: "正在运行",
  shellStopping: "正在中止",
  shellExited: (code) => `已失败（退出码 ${code}）`,
  shellFinished: "已完成",
  shellFailed: "已失败",
  shellStopped: "已中止",
  browserLabel: "浏览器页面",
  browserLoading: "正在加载",
  browserSuspended: "已挂起",
  browserIdle: "已就绪",
  browserAutomation: (tool) => `模型正在操作：${tool}`,
  userAborted: "用户中止操作",
  planLabel: "实施计划",
  planDrafting: "撰写中",
  planAwaitingApproval: "待批准",
  planApproved: "已批准",
  planRejected: "已退回",
  planUpdatedAgo: (minutes) => (minutes === 0 ? "刚刚更新" : `${minutes} 分钟前更新`)
};

/** Pinned clock, one hour after every fixture's start time. */
const NOW = Date.parse("2026-07-20T02:00:00Z");

it.each([false, true])("binds same-name task rows to their source conversation (workflow=%s)", (workflowRun) => {
  const rows = ["A", "B"].map((conversationId) => deriveTaskItems({
    conversationId, agents: [agent("same", { workflowRun })], terminals: []
  }, messages)[0]);
  expect(rows.map((row) => row.conversationId)).toEqual(["A", "B"]);
});

function agent(id: string, overrides: Partial<SubagentView> = {}): SubagentView {
  return {
    id,
    name: null,
    kind: "general",
    workflowRun: false,
    label: id,
    task: `执行 ${id}`,
    status: "completed",
    summary: `${id} 的结论`,
    contexts: [],
    updates: [],
    live: null,
    callIds: [id],
    parentId: null,
    depth: 0,
    childIds: [],
    phase: null,
    phaseIndex: null,
    stepIndex: null,
    role: null,
    modelId: null,
    scriptName: null,
    usage: {},
    toolCount: 0,
    createdAt: "2026-07-20T01:00:00Z",
    completedAt: "2026-07-20T01:05:00Z",
    ...overrides
  };
}

function terminal(overrides: Partial<TerminalSessionState> = {}): TerminalSessionState {
  return {
    terminalId: "terminal-1",
    conversationId: "conversation-1",
    label: "终端 1",
    phase: "running",
    busy: false,
    hasHistory: false,
    cwd: "C:/repo",
    shell: "bash",
    sessionId: "session-1",
    ...overrides
  };
}

function shellTask(overrides: Partial<ShellTaskSnapshot> = {}): ShellTaskSnapshot {
  return {
    shellTaskId: "shell-1",
    conversationId: "conversation-1",
    toolName: "bash",
    command: "npm install",
    stopping: false,
    // 90 seconds before the pinned NOW, so the elapsed column has something to
    // report and a regression to zero would be obvious.
    startedAt: new Date(NOW - 90_000).toISOString(),
    endedAt: null,
    outcome: null,
    exitCode: null,
    ...overrides
  };
}

function browser(overrides: Partial<BrowserStatus> = {}): BrowserStatus {
  return {
    hasPage: true,
    open: true,
    loading: false,
    url: "https://example.test/docs",
    title: "文档",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 1280, height: 800 },
    ...overrides
  };
}

describe("taskStateForStatus", () => {
  it("treats a ceiling as finished and only real terminations as failures", () => {
    expect(taskStateForStatus("running")).toBe("running");
    expect(taskStateForStatus("completed")).toBe("finished");
    // A run that hit its round ceiling still produced usable output; only a
    // provider error, a deliberate stop, or a lost parent turn is a failure.
    expect(taskStateForStatus("roundLimit")).toBe("finished");
    for (const status of ["interrupted", "failed", "stopped"] as const) {
      expect(taskStateForStatus(status)).toBe("failed");
    }
  });
});

describe("deriveTaskItems", () => {
  // A subagent cannot spawn another subagent — the host withholds `agent_spawn`
  // and `workflow` from every child request — so a plain agent row is a leaf
  // even if a stale persisted record still claims descendants.
  it("renders a subagent as a leaf row", () => {
    const items = deriveTaskItems({
      agents: [
        agent("parent", { childIds: ["child"], status: "running", completedAt: null }),
        agent("child", { parentId: "parent", depth: 1 })
      ],
      terminals: [],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]!.kind).toBe("subagent");
    expect(items[0]!.id).toBe("parent");
    expect(items[0]!.state).toBe("running");
    expect(taskItemAgent(items[0]!)?.id).toBe("parent");
    expect(items[0]!.children).toEqual([]);
    expect(flattenTaskItems(items).map((item) => item.id)).toEqual(["parent"]);
  });

  it("reports each of the four metric columns from the agent's own record", () => {
    const items = deriveTaskItems({
      agents: [
        agent("parent", {
          childIds: ["child"],
          usage: { inputTokens: 900, cachedInputTokens: 100, outputTokens: 340, totalTokens: 1240 },
          toolCount: 7,
          createdAt: "2026-07-20T01:00:00Z",
          completedAt: "2026-07-20T01:02:30Z"
        }),
        agent("child", { parentId: "parent", depth: 1 })
      ],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.metrics).toEqual({
      childCount: 0,
      tokens: 1240,
      toolCount: 7,
      elapsedMs: 150_000
    });
  });

  it("measures a still-running agent against now and falls back to the token breakdown", () => {
    const items = deriveTaskItems({
      agents: [agent("live", {
        status: "running",
        completedAt: null,
        // No totalTokens: cached input is already inside inputTokens, so adding
        // it again would over-report by exactly the cache hit.
        usage: { inputTokens: 500, cachedInputTokens: 400, outputTokens: 60 }
      })],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.metrics.tokens).toBe(560);
    expect(items[0]!.metrics.elapsedMs).toBe(3_600_000);
  });

  it("leaves the token and elapsed columns empty when the record reported neither", () => {
    const items = deriveTaskItems({
      agents: [agent("bare", { createdAt: "", completedAt: null })],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.metrics.tokens).toBeNull();
    expect(items[0]!.metrics.elapsedMs).toBeNull();
  });

  it("shows a failed run's summary as its hover text and says nothing on one that finished", () => {
    const items = deriveTaskItems({
      agents: [
        agent("broken", { status: "failed", summary: "工具调用超时" }),
        agent("ok", { summary: "一切正常" })
      ],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.error).toBe("工具调用超时");
    expect(items[1]!.error).toBeNull();
  });

  it("folds a workflow run's steps into one row grouped by declared phase order", () => {
    const items = deriveTaskItems({
      agents: [
        agent("run", {
          kind: "workflowStep",
          workflowRun: true,
          label: "review-proxy",
          childIds: ["verify-1", "review-1", "review-2"],
          status: "running",
          completedAt: null
        }),
        agent("review-1", {
          kind: "workflowStep", parentId: "run", depth: 1, phase: "Review", phaseIndex: 0
        }),
        agent("review-2", {
          kind: "workflowStep", parentId: "run", depth: 1, phase: "Review", phaseIndex: 0,
          status: "running", completedAt: null
        }),
        agent("verify-1", {
          kind: "workflowStep", parentId: "run", depth: 1, phase: "Verify", phaseIndex: 1
        })
      ],
      terminals: [],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    const [item] = items;
    if (item?.kind !== "workflow") throw new Error("expected a workflow row");
    expect(item.label).toBe("review-proxy");
    expect(item.detail).toBe("1/3 个步骤进行中");
    expect(item.steps.map((step) => step.id)).toEqual(["verify-1", "review-1", "review-2"]);
    // Phases sort by declared index, not by the order the tree emitted them.
    expect(item.phases.map((phase) => phase.phase)).toEqual(["Review", "Verify"]);
    expect(item.phases[0]!.steps.map((step) => step.id)).toEqual(["review-1", "review-2"]);
    // Steps are ordinary child rows in the same tree, not a separate control.
    expect(item.children.map((child) => child.id)).toEqual(["verify-1", "review-1", "review-2"]);
    expect(item.metrics.childCount).toBe(3);
  });

  it("collects unphased steps into a single anonymous group", () => {
    const items = deriveTaskItems({
      agents: [
        agent("run", { kind: "workflowStep", workflowRun: true, childIds: ["a", "b"] }),
        agent("a", { kind: "workflowStep", parentId: "run", depth: 1 }),
        agent("b", { kind: "workflowStep", parentId: "run", depth: 1 })
      ],
      terminals: [],
      now: NOW
    }, messages);

    const [item] = items;
    if (item?.kind !== "workflow") throw new Error("expected a workflow row");
    expect(item.detail).toBe("2 个步骤");
    expect(item.phases).toHaveLength(1);
    expect(item.phases[0]!.phase).toBeNull();
    expect(item.phases[0]!.steps.map((step) => step.id)).toEqual(["a", "b"]);
  });

  it("reports a settled workflow as interrupted when any step did not finish", () => {
    const items = deriveTaskItems({
      agents: [
        agent("run", { kind: "workflowStep", workflowRun: true, childIds: ["ok", "broken"] }),
        agent("ok", { kind: "workflowStep", parentId: "run", depth: 1 }),
        agent("broken", { kind: "workflowStep", parentId: "run", depth: 1, status: "failed" })
      ],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.state).toBe("failed");
  });

  it("treats a lone workflow step as its own subagent row rather than an empty workflow", () => {
    const items = deriveTaskItems({
      agents: [agent("step", { kind: "workflowStep" })],
      terminals: [],
      now: NOW
    }, messages);

    expect(items[0]!.kind).toBe("subagent");
  });

  it("keeps a run that has spawned no step a workflow rather than an openable agent row", () => {
    // Every run passes through this shape in its first seconds, and a run the
    // host rejected before it spawned anything stays there. Classifying by
    // "has a step child" dropped it into a subagent row, which is a row that
    // opens a transcript — and a script driver's transcript is exactly the page
    // that must not exist.
    const items = deriveTaskItems({
      agents: [agent("run", {
        kind: "workflowStep",
        workflowRun: true,
        label: "review-proxy",
        status: "running",
        completedAt: null
      })],
      terminals: [],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]!.kind).toBe("workflow");
    expect(items[0]!.detail).toBe("0 个步骤");
    // Nothing hands the run back as an agent, so nothing can open it.
    expect(taskItemAgent(items[0]!)).toBeNull();
  });

  it("counts a live shell as running whether or not a command is in flight", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [
        terminal(),
        terminal({ terminalId: "terminal-2", label: "终端 2", busy: true }),
        terminal({ terminalId: "terminal-3", label: "终端 3", phase: "exited" })
      ],
      now: NOW
    }, messages);

    expect(items.map((item) => [item.label, item.state, item.detail])).toEqual([
      ["终端 1", "running", "空闲"],
      ["终端 2", "running", "正在执行命令"],
      ["终端 3", "finished", "空闲"]
    ]);
    expect(taskItemAgent(items[0]!)).toBeNull();
    // A shell reports none of the four columns, which renders as "—" not "0".
    expect(items[0]!.metrics)
      .toEqual({ childCount: null, tokens: null, toolCount: null, elapsedMs: null });
  });

  it("keeps a suspended browser page in the list rather than dropping it", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1", browser: browser({ suspended: true, suspendedAtMs: Date.parse("2026-07-20T01:30:00Z") }),
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]!.kind).toBe("browser");
    expect(items[0]!.label).toBe("文档");
    expect(items[0]!.detail).toBe("已挂起");
    // A suspended page still owns its profile and resumes on reopen, so it is a live task with a
    // close button rather than history.
    expect(items[0]!.state).toBe("running");
  });

  it("paints a browser page red only when it actually reported an error", () => {
    const loading = deriveTaskItems({
      agents: [], terminals: [], browserSessionId: "conversation-1", browser: browser({ loading: true, title: null }), now: NOW
    }, messages);
    expect(loading[0]!.state).toBe("running");
    expect(loading[0]!.detail).toBe("正在加载");
    // With no title the URL identifies the page; the generic label is the last resort.
    expect(loading[0]!.label).toBe("https://example.test/docs");

    const failed = deriveTaskItems({
      agents: [], terminals: [], browserSessionId: "conversation-1", browser: browser({ error: "ERR_NAME_NOT_RESOLVED" }), now: NOW
    }, messages);
    expect(failed[0]!.state).toBe("failed");
    expect(failed[0]!.error).toBe("ERR_NAME_NOT_RESOLVED");
  });

  it("omits the browser row entirely when the conversation owns no page", () => {
    const items = deriveTaskItems({
      agents: [], terminals: [], browserSessionId: "conversation-1", browser: browser({ hasPage: false }), now: NOW
    }, messages);

    expect(items).toEqual([]);
  });

  it("omits the browser row when no session id names what stopping it would close", () => {
    const items = deriveTaskItems({
      agents: [], terminals: [], browser: browser(), now: NOW
    }, messages);

    expect(items).toEqual([]);
  });

  /**
   * Without a tab strip, a session with no row of its own is a Chromium process the user can
   * neither see nor close, so every one the Agent opens has to become a row.
   */
  it("gives every preview session its own row, identified by that session", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessions: [
        { sessionId: "conversation-1", status: browser({ title: "主页" }) },
        { sessionId: "conversation-1#agent-1", status: browser({ title: "Documentation" }) }
      ],
      now: NOW
    }, messages);

    expect(items.map((item) => [item.id, item.label])).toEqual([
      ["preview:conversation-1", "主页"],
      ["preview:conversation-1#agent-1", "Documentation"]
    ]);
    // Each row closes its own native session rather than all of them closing the primary.
    expect(items.map((item) => (item.kind === "browser" ? item.sessionId : null)))
      .toEqual(["conversation-1", "conversation-1#agent-1"]);
  });

  /**
   * `playwright tab_select` retargets the Agent's later actions but does not move the trusted
   * surface, so automation is a claim about the primary page only.
   */
  it("attributes automation to the primary session, not to every open tab", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browserSessions: [
        { sessionId: "conversation-1", status: browser({ title: "主页" }) },
        { sessionId: "conversation-1#agent-1", status: browser({ title: "Documentation" }) }
      ],
      browserAutomationTool: "click",
      now: NOW
    }, messages);

    expect(items[0]!.detail).toBe("模型正在操作：click");
    expect(items[1]!.detail).toBe("已就绪");
    expect(items.map((item) => (item.kind === "browser" ? item.automationTool : null)))
      .toEqual(["click", null]);
  });

  it("runs the browser row while the model drives it, however idle the page looks", () => {
    // A preview row is running for as long as the page exists, driven or not: it holds a live
    // Chromium process and a single-use profile until someone closes it. With the tab strip's ×
    // gone, this row's stop control is also the only way to close that page — filing an idle one
    // under "finished" would collapse it behind a disclosure as something unkillable.
    const idle = deriveTaskItems({
      agents: [], terminals: [], browserSessionId: "conversation-1", browser: browser(), now: NOW
    }, messages);
    expect(idle[0]!.state).toBe("running");
    expect(runningTaskItems(idle)).toHaveLength(1);

    const driven = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browser: browser(),
      browserAutomationTool: "click",
      now: NOW
    }, messages);
    expect(driven[0]!.state).toBe("running");
    expect(driven[0]!.detail).toBe("模型正在操作：click");
    expect(runningTaskItems(driven)).toHaveLength(1);
    expect(finishedTaskItems(driven)).toEqual([]);
  });

  it("keeps the row running while a requested automation stop is still in flight", () => {
    // The stop button only renders on a running row, so a row that dropped out
    // of "running" the moment the stop was asked for would take its own
    // spinner off screen before the model actually let go.
    const stopping = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browser: browser(),
      browserAutomationTool: null,
      browserAutomationStopping: true,
      now: NOW
    }, messages);

    expect(stopping[0]!.state).toBe("running");
    expect(stopping[0]!.kind === "browser" && stopping[0]!.automationTool).toBeTruthy();
  });

  it("still paints a driven page red when it reported an error", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browser: browser({ error: "ERR_CONNECTION_RESET" }),
      browserAutomationTool: "navigate",
      now: NOW
    }, messages);

    expect(items[0]!.state).toBe("failed");
  });

  it("tells the browser row's stop control what it is stopping", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browser: browser(),
      browserAutomationTool: "type",
      now: NOW
    }, messages);

    const row = items[0]!;
    expect(row.kind).toBe("browser");
    // Stopping a driven page stops the automation; stopping an idle one closes
    // the page. The row carries which case it is so the shell need not re-derive it.
    expect(row.kind === "browser" && row.automationTool).toBe("type");
  });

  it("does not invent an elapsed duration for recovered shell history with an unknown end", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({ outcome: "failed", endedAt: null, exitCode: null })],
      now: NOW
    }, messages);
    expect(items[0]!.state).toBe("failed");
    expect(items[0]!.metrics.elapsedMs).toBeNull();
    expect(items[0]!.endedAt).toBeNull();
    expect(runningTaskItems(items)).toHaveLength(0);
  });

  it("makes every running shell command its own stoppable row", () => {
    // The whole point: a command's runtime is unpredictable, so while it runs
    // the user has to be able to see what it is and reach a stop control.
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [
        shellTask(),
        shellTask({ shellTaskId: "shell-2", toolName: "powershell", command: "Get-Process" })
      ],
      now: NOW
    }, messages);

    expect(items.map((item) => item.kind)).toEqual(["shell", "shell"]);
    expect(items.map((item) => item.id)).toEqual(["shell-1", "shell-2"]);
    // The row says which tool and which command without the user opening it.
    expect(items[0]!.label).toBe("bash");
    expect(items[0]!.detail).toBe("npm install");
    expect(items[1]!.label).toBe("powershell");
    expect(items.every((item) => item.state === "running")).toBe(true);
    expect(runningTaskItems(items)).toHaveLength(2);
    expect(finishedTaskItems(items)).toHaveLength(0);
  });

  it("collapses a finished shell command into the finish list with how it went", () => {
    // The question after a build is "did it pass". A row that vanished at the
    // exact moment it could answer that — which is what this used to do — never
    // got to, and the user was left with an empty list and no idea.
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [
        shellTask({
          outcome: "succeeded",
          exitCode: 0,
          endedAt: new Date(NOW - 30_000).toISOString()
        }),
        shellTask({
          shellTaskId: "shell-2",
          command: "npm test",
          outcome: "failed",
          exitCode: 1,
          endedAt: new Date(NOW - 10_000).toISOString()
        })
      ],
      now: NOW
    }, messages);

    expect(runningTaskItems(items)).toHaveLength(0);
    expect(finishedTaskItems(items)).toHaveLength(2);
    expect(items[0]!.state).toBe("finished");
    expect(items[0]!.detail).toBe("已完成 · npm install");
    // A non-zero exit is the one shell state worth painting red, and the code is
    // the first thing anyone asks about one.
    expect(items[1]!.state).toBe("failed");
    expect(items[1]!.detail).toBe("已失败（退出码 1） · npm test");
  });

  it("keeps a stopped command neutral without a taskbar abort record", () => {
    const [item] = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({
        outcome: "stopped",
        exitCode: null,
        stopping: true,
        endedAt: new Date(NOW - 5_000).toISOString()
      })],
      now: NOW
    }, messages);

    expect(item!.state).toBe("finished");
    expect(item!.detail).toBe("已中止 · npm install");
    expect(item!.error).toBeNull();
  });

  it("retains a user-aborted task as one failed historical row", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [terminal()],
      userAbortedTasks: [abortedTask()],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "aborted",
      state: "failed",
      detail: "用户中止操作 · 正在执行命令",
      error: "用户中止操作",
      endedAt: new Date(6_000).toISOString(),
      metrics: { elapsedMs: 5_000 }
    });
    expect(runningTaskItems(items)).toHaveLength(0);
    expect(finishedTaskItems(items)).toHaveLength(1);
  });

  it("still renders a persisted abort record whose task kind has been retired", () => {
    // Persisted abort records with retired task kinds must still render as
    // orphaned history.
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      userAbortedTasks: [abortedTask({
        sourceKind: "webSearch",
        sourceIdentity: "web-search:search-1",
        label: "2026 年 Web 搜索趋势",
        detail: "正在搜索"
      })],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "aborted",
      sourceKind: "webSearch",
      state: "failed",
      detail: "用户中止操作 · 正在搜索"
    });
  });

  it("keeps browser automation history separate from the surviving page", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browser: browser(),
      browserSessionId: "conversation-1",
      modelRequestId: "request-1",
      userAbortedTasks: [abortedTask({
        sourceKind: "browser",
        sourceIdentity: "browser-automation:request-1:conversation-1",
        label: "自动化页面"
      })],
      now: NOW
    }, messages);

    expect(items.map((item) => item.kind)).toEqual(["browser", "aborted"]);
  });

  it("does not let closed-page history replace a newly opened page", () => {
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      browser: browser(),
      browserSessionId: "conversation-1",
      userAbortedTasks: [abortedTask({
        sourceKind: "browser",
        sourceIdentity: "browser-page:conversation-1",
        label: "旧页面"
      })],
      now: NOW
    }, messages);

    // A preview is state rather than historical work: a closed page must not
    // displace or reappear over a newly open page.
    expect(items.map((item) => item.kind)).toEqual(["browser"]);
  });

  it("includes aborted history in activity and task presence", () => {
    const sources = { agents: [], terminals: [], userAbortedTasks: [abortedTask()] };
    expect(hasAnyTask(sources)).toBe(true);
    expect(taskActivitySignature(sources)).toContain("x:abort-1:terminal");
  });

  it("freezes a finished command's elapsed column at its real duration", () => {
    // A finished row whose clock kept counting would claim a two-second command
    // had been running for an hour by the time the user scrolled to it.
    const source = () => ({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({
        startedAt: new Date(NOW - 125_000).toISOString(),
        endedAt: new Date(NOW - 25_000).toISOString(),
        outcome: "succeeded" as const,
        exitCode: 0
      })]
    });

    expect(deriveTaskItems({ ...source(), now: NOW }, messages)[0]!.metrics.elapsedMs)
      .toBe(100_000);
    // Same snapshot, later clock: the number must not move.
    expect(deriveTaskItems({ ...source(), now: NOW + 600_000 }, messages)[0]!.metrics.elapsedMs)
      .toBe(100_000);
  });

  it("says a shell command is stopping without dropping its row", () => {
    // Between the button press and the process actually dying the row has to
    // stay running: it is what holds the stop control and its pending spinner.
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({ stopping: true })],
      now: NOW
    }, messages);

    expect(items).toHaveLength(1);
    expect(items[0]!.state).toBe("running");
    expect(items[0]!.detail).toContain("正在中止");
    // The command text survives alongside the status, so the row still says
    // which command is the one being stopped.
    expect(items[0]!.detail).toContain("npm install");
  });

  it("reports how long a shell command has been running, measured from the host's start", () => {
    // The reason the row exists is that a command's runtime is unpredictable,
    // so "how long has this been going" is the one number it can report.
    const items = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({ startedAt: new Date(NOW - 125_000).toISOString() })],
      now: NOW
    }, messages);

    expect(items[0]!.metrics.elapsedMs).toBe(125_000);
    // Still running, so it is measured against now rather than an end time —
    // a later `now` reports a larger number off the same snapshot.
    const later = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({ startedAt: new Date(NOW - 125_000).toISOString() })],
      now: NOW + 30_000
    }, messages);
    expect(later[0]!.metrics.elapsedMs).toBe(155_000);
  });

  it("leaves the shell row's other three columns empty rather than claiming zero", () => {
    // A command spawns no subagents, spends no tokens and calls no tools.
    // Printing 0 would assert it measured them and found none.
    const [item] = deriveTaskItems({
      agents: [],
      terminals: [],
      shellTasks: [shellTask()],
      now: NOW
    }, messages);

    expect(item!.metrics.childCount).toBeNull();
    expect(item!.metrics.tokens).toBeNull();
    expect(item!.metrics.toolCount).toBeNull();
  });
});

describe("taskActivitySignature", () => {
  it("changes when any task's status changes but holds still as the clock advances", () => {
    const sources = (childStatus: SubagentView["status"]) => ({
      agents: [
        agent("parent", { childIds: ["child"], status: "running" as const, completedAt: null }),
        agent("child", { parentId: "parent", depth: 1, status: childStatus, completedAt: null })
      ],
      terminals: []
    });

    // The signature never reads a timestamp: elapsed time ticks every second and
    // would pin the sidebar's unread dot on permanently.
    expect(taskActivitySignature(sources("running")))
      .toBe(taskActivitySignature({ ...sources("running"), now: NOW + 60_000 }));
    expect(taskActivitySignature(sources("running")))
      .not.toBe(taskActivitySignature(sources("completed")));
  });

  it("changes when a task joins or leaves the set", () => {
    const one = taskActivitySignature({ agents: [], terminals: [terminal()] });
    const two = taskActivitySignature({
      agents: [], terminals: [terminal(), terminal({ terminalId: "terminal-2" })]
    });

    expect(one).not.toBe(two);
  });

  it("notices a browser page navigating", () => {
    const page = (url: string) => taskActivitySignature({
      agents: [], terminals: [], browserSessionId: "conversation-1", browser: browser({ url })
    });
    expect(page("https://a.test")).not.toBe(page("https://b.test"));
  });

  it("notices the model picking up and putting down the browser page", () => {
    // A page can sit at one idle URL for a dozen automation steps. Without the
    // driving tool in the fingerprint the dot never lights up for any of them.
    const driven = (tool: string | null, stopping = false) => taskActivitySignature({
      agents: [],
      terminals: [],
      browserSessionId: "conversation-1",
      browser: browser(),
      browserAutomationTool: tool,
      browserAutomationStopping: stopping
    });

    expect(driven(null)).not.toBe(driven("click"));
    expect(driven("click")).not.toBe(driven("type"));
    expect(driven("click")).not.toBe(driven("click", true));
  });

  it("notices a shell command starting, being stopped, and ending", () => {
    const none = taskActivitySignature({ agents: [], terminals: [] });
    const running = taskActivitySignature({
      agents: [], terminals: [], shellTasks: [shellTask()]
    });
    const stopping = taskActivitySignature({
      agents: [], terminals: [], shellTasks: [shellTask({ stopping: true })]
    });
    const finished = taskActivitySignature({
      agents: [],
      terminals: [],
      shellTasks: [shellTask({ outcome: "succeeded", exitCode: 0, endedAt: "2026-08-08T00:00:00Z" })]
    });

    expect(none).not.toBe(running);
    // Asking for a stop is news on its own — the row's wording changes and the
    // user is waiting to see it take effect.
    expect(running).not.toBe(stopping);
    // A command finishing is the change the user most wants the dot for, and it
    // no longer announces itself by the row disappearing from the set.
    expect(running).not.toBe(finished);
  });
});

describe("hasAnyTask", () => {
  it("is false only when nothing at all is running or finished", () => {
    expect(hasAnyTask({ agents: [], terminals: [] })).toBe(false);
    expect(hasAnyTask({ agents: [agent("done")], terminals: [] })).toBe(true);
    expect(hasAnyTask({ agents: [], terminals: [terminal({ phase: "exited" })] })).toBe(true);
    // A running command is a task even when it is the only thing happening —
    // otherwise the sidebar button stays dark while a shell holds the run.
    expect(hasAnyTask({ agents: [], terminals: [], shellTasks: [shellTask()] })).toBe(true);
    // A page needs both a surface and the session id that would close it.
    expect(hasAnyTask({ agents: [], terminals: [], browser: browser() })).toBe(false);
    expect(hasAnyTask({
      agents: [], terminals: [], browser: browser(), browserSessionId: "conversation-1"
    })).toBe(true);
  });
});

describe("task unread latch", () => {
  const input = (overrides: Partial<TaskUnreadInput> = {}): TaskUnreadInput => ({
    conversationId: "conversation-1",
    signature: "a:live:running:0:0",
    hasTasks: true,
    panelOpen: false,
    ...overrides
  });

  it("lights up on a change, clears when the panel is opened, and returns on the next one", () => {
    let state: TaskUnreadState = { seen: "", conversationId: "conversation-1" };

    // A first task arriving is news.
    expect(taskUnreadVisible(state, input())).toBe(true);

    // Opening the panel is what marks it seen.
    state = nextTaskUnreadState(state, input({ panelOpen: true }));
    expect(taskUnreadVisible(state, input({ panelOpen: true }))).toBe(false);
    expect(taskUnreadVisible(state, input())).toBe(false);

    // The next change to any task lights it again without further bookkeeping.
    const later = input({ signature: "a:live:completed:0:0" });
    expect(taskUnreadVisible(state, later)).toBe(true);
  });

  it("stays dark once the last task is gone even though the signature changed", () => {
    const state: TaskUnreadState = { seen: "a:live:running:0:0", conversationId: "conversation-1" };
    expect(taskUnreadVisible(state, input({ signature: "", hasTasks: false }))).toBe(false);
  });

  it("adopts a conversation's signature on arrival so switching is not itself news", () => {
    const previous: TaskUnreadState = { seen: "a:old:running:0:0", conversationId: "conversation-1" };
    const arriving = input({ conversationId: "conversation-2", signature: "a:new:running:0:0" });

    // Before adoption the latch belongs to another conversation and says nothing.
    expect(taskUnreadVisible(previous, arriving)).toBe(false);

    const state = nextTaskUnreadState(previous, arriving);
    expect(state).toEqual({ seen: "a:new:running:0:0", conversationId: "conversation-2" });
    expect(taskUnreadVisible(state, arriving)).toBe(false);

    // Work that starts after the switch is news again.
    expect(taskUnreadVisible(state, { ...arriving, signature: "a:new:completed:0:0" })).toBe(true);
  });

  it("does not mark a tree seen while the panel is closed", () => {
    const previous: TaskUnreadState = { seen: "", conversationId: "conversation-1" };
    expect(nextTaskUnreadState(previous, input())).toBe(previous);
  });
});

describe("task item partitions", () => {
  it("splits running rows from everything the finish disclosure hides", () => {
    const items = deriveTaskItems({
      agents: [
        agent("live", { status: "running", completedAt: null }),
        agent("done"),
        agent("broken", { status: "interrupted" })
      ],
      terminals: [terminal({ phase: "exited" })],
      now: NOW
    }, messages);

    expect(runningTaskItems(items).map((item) => item.id)).toEqual(["live"]);
    expect(finishedTaskItems(items).map((item) => item.id))
      .toEqual(["done", "broken", "terminal-1"]);
  });
});
