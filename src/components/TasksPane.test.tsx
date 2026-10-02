import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SubagentView } from "../lib/subagents";
import type { TerminalSessionState } from "../lib/terminal";
import type { ShellTaskSnapshot } from "../lib/shellTasks";
import type { ForkDecisionRecord } from "../types";
import { TasksPane } from "./TaskContainer";

function agent(id: string, overrides: Partial<SubagentView> = {}): SubagentView {
  return {
    id,
    name: null,
    ledgerOwner: null,
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
    startedAt: new Date(Date.now() - 90_000).toISOString(),
    endedAt: null,
    outcome: null,
    exitCode: null,
    ...overrides
  };
}

function forkDecision(overrides: Partial<ForkDecisionRecord> = {}): ForkDecisionRecord {
  return {
    forkId: "fork-1",
    workspaceId: "workspace-1",
    sourceConversationId: "conversation-1",
    title: "迁移升级脚本",
    prompt: "迁移升级脚本\n把 v4 的迁移拆成两步。",
    requestedAt: "2026-07-20T01:40:00Z",
    decidedAt: "2026-07-20T01:41:00Z",
    approved: true,
    childConversationId: "conversation-2",
    ...overrides
  };
}

function renderPane(props: Partial<Parameters<typeof TasksPane>[0]> = {}) {
  const onSelectAgent = vi.fn();
  const onStopItem = vi.fn();
  const onOpenItem = vi.fn();
  const { container, rerender } = render(
    <TasksPane
      agents={[]}
      terminals={[]}
      selectedAgentId={null}
      onSelectAgent={onSelectAgent}
      onOpenItem={onOpenItem}
      onStopItem={onStopItem}
      {...props}
    />
  );
  return {
    onSelectAgent,
    onOpenItem,
    onStopItem,
    rerender,
    pane: container.querySelector(".tasks-pane") as HTMLElement
  };
}

describe("TasksPane", () => {
  it("renders the list without any container chrome of its own", () => {
    const { pane } = renderPane();

    expect(pane).toBeInTheDocument();
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "收起任务容器" })).not.toBeInTheDocument();
    expect(within(pane).getByText("还没有任务")).toBeInTheDocument();
  });

  it("gives a running agent, a terminal and a shell command one row each", () => {
    const { pane } = renderPane({
      agents: [agent("live", { label: "正在审查", status: "running", completedAt: null })],
      terminals: [terminal({ busy: true })],
      shellTasks: [shellTask()]
    });

    expect(within(pane).getByText("正在审查")).toBeInTheDocument();
    expect(within(pane).getByText("终端 1")).toBeInTheDocument();
    expect(within(pane).getByText("正在执行命令")).toBeInTheDocument();
    expect(within(pane).getByText("bash")).toBeInTheDocument();
    expect(within(pane).getByText("npm install")).toBeInTheDocument();
  });

  it("routes an agent row to its transcript and the other rows to their pages", async () => {
    const user = userEvent.setup();
    const { pane, onSelectAgent, onOpenItem } = renderPane({
      agents: [agent("live", { label: "正在审查", status: "running", completedAt: null })],
      terminals: [terminal()],
      shellTasks: [shellTask()],
      browserSessionId: "conversation-1",
      browser: {
        hasPage: true, open: true, loading: false,
        url: "https://example.test/docs", title: "文档",
        canGoBack: false, canGoForward: false, zoom: 1,
        viewport: { width: 1280, height: 800 }
      }
    });

    await user.click(within(pane).getByRole("button", { name: "打开子代理 正在审查" }));
    expect(onSelectAgent).toHaveBeenCalledWith("live");
    expect(onOpenItem).not.toHaveBeenCalled();

    await user.click(within(pane).getByRole("button", { name: "打开“终端 1”" }));
    expect(onOpenItem.mock.calls[0]![0]).toMatchObject({ kind: "terminal", id: "terminal-1" });

    // A ready page is a live task, not history, so it is in the list without a disclosure.
    await user.click(within(pane).getByRole("button", { name: "打开“文档”" }));
    expect(onOpenItem.mock.calls[1]![0]).toMatchObject({ kind: "browser", sessionId: "conversation-1" });

    // A shell command's output is a page too — the read-only terminal. The row is labelled
    // by its tool, with the command text as the detail.
    await user.click(within(pane).getByRole("button", { name: "打开“bash”" }));
    expect(onOpenItem.mock.calls[2]![0]).toMatchObject({ kind: "shell", id: "shell-1" });

    // Only the agent row ever reaches the transcript selector.
    expect(onSelectAgent).toHaveBeenCalledTimes(1);
  });

  it("gives a dev server its own row, stopped rather than aborted", async () => {
    const user = userEvent.setup();
    const { pane, onStopItem, onOpenItem } = renderPane({
      previewServers: [{
        handle: "7",
        serverId: "dev",
        name: "dev",
        port: 5173,
        status: "running",
        startedAt: "2026-07-20T01:00:00Z",
        cwd: "C:/repo",
        sessionId: "conversation-1"
      }]
    });

    expect(within(pane).getByText("dev")).toBeInTheDocument();
    expect(within(pane).getByText("运行中 · http://localhost:5173")).toBeInTheDocument();
    // The row opens the preview it serves; it is not a transcript.
    await user.click(within(pane).getByRole("button", { name: "打开“dev”" }));
    expect(onOpenItem.mock.calls[0]![0]).toMatchObject({ kind: "preview", address: "http://localhost:5173" });

    // "停止", not "中止": a dev server is a process somebody started, not work in flight.
    await user.click(within(pane).getByRole("button", { name: "停止“dev”" }));
    expect(onStopItem.mock.calls[0]![0]).toMatchObject({ kind: "preview", server: { serverId: "dev" } });
  });

  it("stops a running row without opening it", async () => {
    const user = userEvent.setup();
    const { pane, onStopItem, onOpenItem, onSelectAgent } = renderPane({
      shellTasks: [shellTask()]
    });

    expect(within(pane).getByText("npm install")).toBeInTheDocument();
    await user.click(within(pane).getByRole("button", { name: "中止“bash”" }));
    expect(onStopItem).toHaveBeenCalledTimes(1);
    expect(onStopItem.mock.calls[0]![0]).toMatchObject({
      kind: "shell",
      id: "shell-1",
      shell: { conversationId: "conversation-1", shellTaskId: "shell-1" }
    });
    expect(onOpenItem).not.toHaveBeenCalled();
    expect(onSelectAgent).not.toHaveBeenCalled();
  });

  it("offers a stop control only for rows that are still running", async () => {
    const user = userEvent.setup();
    const { pane, onStopItem } = renderPane({
      agents: [agent("live", { label: "正在审查", status: "running", completedAt: null })]
    });

    await user.click(within(pane).getByRole("button", { name: "中止“正在审查”" }));
    expect(onStopItem).toHaveBeenCalledTimes(1);
    expect(onStopItem.mock.calls[0]![0]).toMatchObject({ kind: "subagent", id: "live" });

    // Stopping a row does not also open it: the click stops at the button.
    await user.click(within(pane).getByRole("button", { name: "打开子代理 正在审查" }));
  });

  it("disables the stop control once an abort is already in flight", () => {
    const { pane } = renderPane({
      agents: [agent("live", { label: "正在审查", status: "running", completedAt: null })],
      stoppingIds: ["live"]
    });

    expect(within(pane).getByRole("button", { name: "正在中止“正在审查”" })).toBeDisabled();
  });

  it("shows how long a shell command has been running and keeps counting", async () => {
    // A blocked `bash` call emits nothing between start and finish, so unlike an
    // agent row there is no stream to re-render this pane as a side effect. If
    // the column does not advance on its own it freezes at whatever it read when
    // the row appeared, which reads as a hung command.
    vi.useFakeTimers();
    try {
      const startedAt = new Date(Date.now() - 65_000).toISOString();
      const { pane } = renderPane({ shellTasks: [shellTask({ startedAt })] });

      const row = within(pane).getByText("npm install").closest(".task-row") as HTMLElement;
      expect(within(row).getByTitle("运行时长")).toHaveTextContent("1m05s");

      await act(async () => {
        vi.advanceTimersByTime(10_000);
      });
      expect(within(row).getByTitle("运行时长")).toHaveTextContent("1m15s");
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops ticking once no shell command is left running", async () => {
    // The interval exists for running shell rows only; leaving it armed would
    // re-render the whole pane every second to recompute a frozen number. The
    // rerender keeps the row and only ends it — which is exactly what a finished
    // command now looks like, and the case an emptied list would not have caught.
    vi.useFakeTimers();
    try {
      const { rerender } = renderPane({ shellTasks: [shellTask()] });
      expect(vi.getTimerCount()).toBeGreaterThan(0);

      rerender(
        <TasksPane
          agents={[]}
          terminals={[]}
          shellTasks={[shellTask({
            outcome: "succeeded",
            exitCode: 0,
            endedAt: new Date().toISOString()
          })]}
          selectedAgentId={null}
          onSelectAgent={vi.fn()}
          onStopItem={vi.fn()}
        />
      );
      await act(async () => {
        await Promise.resolve();
      });
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("moves a finished shell command into the finish list, red and with no stop control", () => {
    const { pane } = renderPane({
      shellTasks: [shellTask({
        command: "npm test",
        outcome: "failed",
        exitCode: 1,
        endedAt: new Date().toISOString(),
        cwd: "/Users/me/web"
      })]
    });

    // The subtitle is the command alone; the failure is the row's colour, and
    // the exit code its hover text.
    const detail = within(pane).getByText("npm test");
    const row = detail.closest(".task-row") as HTMLElement;
    expect(row).toHaveClass("task-row--failed");
    expect(detail.closest(".task-row__main")).toHaveAttribute("title", "已失败（退出码 1）");
    // Titled by the shell and the directory it ran in, drawn as a path.
    const label = row.querySelector(".task-row__label") as HTMLElement;
    expect(label).toHaveClass("path-text");
    expect(label).toHaveTextContent("bash:/Users/me/web");
    // No process is left to kill, so offering a button that cannot work would be a lie.
    expect(within(row).queryByRole("button", { name: /中止/ })).not.toBeInTheDocument();
  });

  it("shows finished rows under an open finish disclosure until it is folded", async () => {
    const user = userEvent.setup();
    const { pane } = renderPane({
      agents: [
        agent("live", { label: "正在审查", status: "running", completedAt: null }),
        agent("done", { label: "已经完成" })
      ]
    });

    expect(within(pane).getByRole("button", { name: "打开子代理 正在审查" })).toBeInTheDocument();
    const disclosure = within(pane).getByRole("button", { name: /已完成/ });
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
    expect(within(pane).getByRole("button", { name: "打开子代理 已经完成" })).toBeInTheDocument();

    await user.click(disclosure);
    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    expect(within(pane).queryByRole("button", { name: "打开子代理 已经完成" })).not.toBeInTheDocument();

    await user.click(disclosure);
    expect(within(pane).getByRole("button", { name: "打开子代理 已经完成" })).toBeInTheDocument();
  });

  it("draws a workflow as its own panel whose steps open but whose run does not", async () => {
    const user = userEvent.setup();
    const { pane, onSelectAgent } = renderPane({
      agents: [
        agent("run", {
          kind: "workflowStep",
          workflowRun: true,
          label: "review-proxy",
          task: "审查代理路径",
          status: "running",
          completedAt: null,
          childIds: ["review-1", "verify-1"]
        }),
        agent("review-1", {
          kind: "workflowStep", parentId: "run", depth: 1, label: "审查一",
          callIds: ["wf-ws1"], role: { name: "reviewer", modelId: "sonnet-5" },
          phase: "Review", phaseIndex: 0, status: "running", completedAt: null
        }),
        agent("verify-1", {
          kind: "workflowStep", parentId: "run", depth: 1, label: "验证一",
          callIds: ["wf-ws2"],
          phase: "Verify", phaseIndex: 1, status: "running", completedAt: null
        })
      ]
    });

    const slot = pane.querySelector("[data-workflow-run]") as HTMLElement;
    expect(slot).toBeTruthy();
    expect(slot.getAttribute("data-workflow-run")).toBeTruthy();

    // The run is a panel, not a row: it states the plan and never offers a
    // transcript, because a script does not have one.
    const panel = within(slot).getByRole("region", { name: "工作流 review-proxy" });
    expect(within(panel).getByText("审查代理路径")).toBeInTheDocument();
    expect(within(pane).queryByRole("button", { name: /打开子代理 review-proxy/ })).not.toBeInTheDocument();

    // Phase headers remain visible with completion counts; only real steps are controls.
    const heads = Array.from(panel.querySelectorAll(".workflow-phase__head"));
    expect(heads.map((head) => head.textContent)).toEqual(["Review0/1", "Verify0/1"]);
    expect(heads.every((head) => head.closest("button") === null)).toBe(true);
    // A step is titled role:name and subtitled with what it was asked; the
    // model stays out of sight, in the tooltip.
    expect(within(panel).getByText("reviewer:审查一")).toBeInTheDocument();
    expect(within(panel).getByText("执行 review-1")).toBeInTheDocument();
    expect(within(panel).queryByText("sonnet-5")).not.toBeInTheDocument();

    await user.click(within(panel).getByRole("button", { name: "打开步骤 验证一" }));
    expect(onSelectAgent).toHaveBeenCalledWith("verify-1");
    // Opening a step opens that step, not the workflow it belongs to.
    expect(onSelectAgent).not.toHaveBeenCalledWith("run");
  });

  it("renders persisted user aborts as inert failed rows", () => {
    const { pane, onStopItem, onSelectAgent } = renderPane({
      userAbortedTasks: [{
        id: "abort-1",
        sourceKind: "subagent",
        sourceIdentity: "agent:call-1:2026-01-01T00:00:00Z",
        label: "中止审查",
        detail: "检查任务栏",
        metrics: { childCount: 0, tokens: 10, toolCount: 1, elapsedMs: 1000 },
        startedAt: "2026-01-01T00:00:00Z",
        endedAt: "2026-01-01T00:00:01Z",
        reason: "userAborted"
      }]
    });

    const detail = within(pane).getByText("用户中止操作 · 检查任务栏");
    const row = detail.closest("li");
    expect(row).toHaveClass("task-row--failed");
    expect(detail.closest(".task-row__main")).toHaveAttribute("title", "用户中止操作");
    expect(within(row as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    expect(onStopItem).not.toHaveBeenCalled();
    expect(onSelectAgent).not.toHaveBeenCalled();
  });

  it("shows a failed row's error as its hover text and says nothing on a clean one", () => {
    const { pane } = renderPane({
      agents: [
        agent("broken", { label: "抓取失败", status: "failed", summary: "工具调用超时" }),
        agent("ok", { label: "抓取成功", summary: "一切正常" })
      ]
    });

    expect(within(pane).getByRole("button", { name: "打开子代理 抓取失败" }))
      .toHaveAttribute("title", "工具调用超时");
    // A run that finished cleanly has no failure to explain, so no tooltip.
    expect(within(pane).getByRole("button", { name: "打开子代理 抓取成功" }))
      .not.toHaveAttribute("title");
  });

  it("reports the four metric columns and drops a terminal's outright", () => {
    const { pane } = renderPane({
      agents: [agent("live", {
        label: "正在审查",
        status: "running",
        completedAt: null,
        childIds: ["child"],
        usage: { totalTokens: 8_400 },
        toolCount: 7,
        createdAt: new Date(Date.now() - 90_000).toISOString()
      }), agent("child", { parentId: "live", depth: 1 })],
      terminals: [terminal()]
    });

    const agentRow = within(pane).getByText("正在审查").closest(".task-row") as HTMLElement;
    // A subagent cannot spawn another subagent, so this column is always zero
    // for a plain agent row; only a workflow reports children, in its own panel.
    expect(within(agentRow).getByTitle("子代数")).toHaveTextContent("0子");
    expect(within(agentRow).getByTitle("token 数")).toHaveTextContent("8.4k");
    expect(within(agentRow).getByTitle("工具调用数")).toHaveTextContent("7工具");
    expect(within(agentRow).getByTitle("运行时长")).toHaveTextContent(/^1m3\ds$/);

    // Child count, tokens and tool calls are quantities only a subagent has, so
    // a terminal shows no columns at all rather than a row of em dashes.
    const terminalRow = within(pane).getByText("终端 1").closest(".task-row") as HTMLElement;
    expect(within(terminalRow).queryAllByTitle(/子代数|token 数|工具调用数|运行时长/)).toEqual([]);
    expect(terminalRow.querySelector(".task-metrics")).toBeNull();
  });

  it("leaves a shell command only the one column it can report", () => {
    const { pane } = renderPane({ shellTasks: [shellTask()] });

    const shellRow = within(pane).getByText("bash").closest(".task-row") as HTMLElement;
    // Elapsed is the whole reason the row exists, and it is the only quantity a
    // command has. The other three would be dashes, so they are not drawn.
    expect(within(shellRow).getByTitle("运行时长")).toHaveTextContent(/^1m3\ds$/);
    expect(within(shellRow).queryByTitle("子代数")).toBeNull();
    expect(within(shellRow).queryByTitle("token 数")).toBeNull();
    expect(within(shellRow).queryByTitle("工具调用数")).toBeNull();
    // The list has no nesting left to draw, so no row reserves a disclosure
    // slot: every row starts at the pane's left edge.
    expect(shellRow.querySelector(".task-row__disclosure")).toBeNull();
  });

  it("leaves a blank preview page out of the list entirely", () => {
    const { pane } = renderPane({
      browserSessionId: "conversation-1",
      browser: {
        hasPage: true, open: true, loading: false,
        url: "about:blank", title: null,
        canGoBack: false, canGoForward: false, zoom: 1,
        viewport: { width: 1280, height: 800 }
      }
    });

    // Opening the preview pane mints a native surface at `about:blank` and paints the pane's own
    // standby card over it. That is the browser at rest, not a tab: the list stays empty until
    // something is actually loaded in it.
    expect(within(pane).getByText("还没有任务")).toBeInTheDocument();
    expect(within(pane).queryByText("预览页面")).toBeNull();
    expect(within(pane).queryByText("已就绪")).toBeNull();
  });

  it("lists a suspended browser page as its own task row", () => {
    const { pane } = renderPane({
      browserSessionId: "conversation-1",
      browser: {
        hasPage: false, suspended: true, open: false, loading: false,
        url: "https://example.test/docs", title: "文档",
        canGoBack: false, canGoForward: false, zoom: 1,
        viewport: { width: 1280, height: 800 }
      }
    });

    // A suspended page keeps its profile and resumes on reopen, so it stays in the live list
    // rather than behind the finish disclosure: it is still something the user has to be able to
    // reach and close.
    expect(within(pane).getByText("文档")).toBeInTheDocument();
    expect(within(pane).getByText("已挂起")).toBeInTheDocument();
  });

  it("lists a terminal process without making it openable as an agent", async () => {
    const user = userEvent.setup();
    const { pane, onStopItem, onSelectAgent } = renderPane({
      onOpenItem: undefined,
      terminals: [terminal({ busy: true })]
    });

    expect(within(pane).getByText("终端 1")).toBeInTheDocument();
    expect(within(pane).getByText("正在执行命令")).toBeInTheDocument();
    expect(within(pane).queryByRole("button", { name: "打开子代理 终端 1" })).not.toBeInTheDocument();
    // Without an onOpenItem handler the row is inert rather than falsely clickable.
    expect(within(pane).queryByRole("button", { name: "打开“终端 1”" })).not.toBeInTheDocument();

    await user.click(within(pane).getByRole("button", { name: "中止“终端 1”" }));
    expect(onStopItem.mock.calls[0]![0]).toMatchObject({ kind: "terminal", id: "terminal-1" });
    expect(onSelectAgent).not.toHaveBeenCalled();
  });

  it("gives the plan a row that opens its page and offers nothing to stop", async () => {
    const user = userEvent.setup();
    const { pane, onOpenItem, onSelectAgent } = renderPane({
      plan: {
        conversationId: "conversation-1",
        markdown: "# 替换审批闸\n\n分三步。",
        status: "draft",
        createdAt: "2026-07-20T01:00:00Z",
        updatedAt: new Date().toISOString()
      },
      planAwaitingApproval: true
    });

    const row = within(pane).getByRole("button", { name: "打开“实施计划”" });
    expect(row).toHaveTextContent("待批准");
    expect(row).toHaveTextContent("刚刚更新");
    // A document is not work in flight, so the row carries no abort control.
    expect(within(pane).queryByRole("button", { name: /中止/ })).not.toBeInTheDocument();

    await user.click(row);
    expect(onOpenItem).toHaveBeenCalledTimes(1);
    expect(onOpenItem.mock.calls[0]![0]).toMatchObject({ kind: "plan", id: "plan" });
    expect(onSelectAgent).not.toHaveBeenCalled();
  });

  it("opens the child conversation from an approved fork's row", async () => {
    const user = userEvent.setup();
    const { pane, onOpenItem, onSelectAgent, onStopItem } = renderPane({
      forkDecisions: [forkDecision()]
    });

    // A decision is settled by the time it has a row, so it is history.
    expect(within(pane).getByRole("button", { name: /已完成/ })).toHaveAttribute("aria-expanded", "true");

    const row = within(pane).getByRole("button", { name: "打开“迁移升级脚本”" });
    expect(row).toHaveTextContent("已创建子对话 · 点击打开");
    await user.click(row);
    expect(onOpenItem).toHaveBeenCalledTimes(1);
    expect(onOpenItem.mock.calls[0]![0]).toMatchObject({
      kind: "fork",
      id: "fork:fork-1",
      decision: { forkId: "fork-1", childConversationId: "conversation-2" }
    });
    // A fork is not an agent and not work in flight: nothing to open a
    // transcript for and nothing left to stop.
    expect(onSelectAgent).not.toHaveBeenCalled();
    expect(onStopItem).not.toHaveBeenCalled();
    expect(within(pane).queryByRole("button", { name: /中止/ })).not.toBeInTheDocument();
  });

  it("leaves a declined fork's row inert, since it created nothing to open", async () => {
    const user = userEvent.setup();
    const { pane, onOpenItem } = renderPane({
      forkDecisions: [forkDecision({
        forkId: "fork-2",
        title: "重写导出脚本",
        approved: false,
        childConversationId: null
      })]
    });

    // The refusal is still recorded — the model is never told the outcome, so
    // this row is the only trace the request ever existed.
    const detail = within(pane).getByText("用户拒绝了分叉");
    const row = detail.closest("li") as HTMLElement;
    expect(row).toHaveTextContent("重写导出脚本");
    expect(within(pane).queryByRole("button", { name: "打开“重写导出脚本”" }))
      .not.toBeInTheDocument();

    await user.click(detail);
    expect(onOpenItem).not.toHaveBeenCalled();
  });

  it("marks the row whose transcript is showing", () => {
    const { pane } = renderPane({
      agents: [agent("live", { label: "正在审查", status: "running", completedAt: null })],
      selectedAgentId: "live"
    });

    expect(within(pane).getByRole("button", { name: "打开子代理 正在审查" }))
      .toHaveAttribute("aria-current", "true");
  });
});
