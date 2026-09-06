import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { deriveSubagentViews } from "../lib/subagents";
import type { SubagentView } from "../lib/subagents";
import type { ContextItem, ToolContext } from "../types";
import { SubagentPanel } from "./SubagentPanel";

function toolContext(toolName: string, input: ToolContext["input"], output: string, createdAt: string): ToolContext {
  return {
    id: `tool-${toolName}-${createdAt}`,
    kind: "tool",
    toolName,
    input,
    result: { success: true, output, executedAt: createdAt, durationMs: 8 },
    createdAt
  };
}

function agent(index: number, overrides: Partial<SubagentView> = {}): SubagentView {
  const createdAt = `2026-07-${String(10 + index).padStart(2, "0")}T01:00:00Z`;
  const base: SubagentView = {
    id: `agent-${index}`,
    name: null,
    kind: "general",
    workflowRun: false,
    label: `子代理 ${index}`,
    task: `执行任务 ${index}`,
    status: "completed",
    summary: `完成了任务 ${index}`,
    contexts: [
      { id: `task-${index}`, kind: "user", content: `执行任务 ${index}`, createdAt },
      { id: `answer-${index}`, kind: "assistant", content: `任务 ${index} 的结论`, createdAt }
    ],
    updates: [],
    live: null,
    callIds: [`agent-${index}`],
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
    createdAt,
    completedAt: createdAt,
    ...overrides
  };
  return base;
}

describe("SubagentPanel", () => {
  it("renders one read-only thread page with structured updates and no list or back navigation", async () => {
    const update = toolContext("subagent_update", { message: "已经完成扫描" }, "状态已返回", "2026-07-14T01:05:00Z");
    const read = toolContext("read", { path: "README.md" }, "README content", "2026-07-14T01:06:00Z");
    const historicalQuestion = toolContext("ask_user", { question: "历史上需要哪种格式？" }, "等待用户回答", "2026-07-14T01:06:30Z");
    const contexts: ContextItem[] = [
      { id: "task", kind: "user", content: "审查 README", createdAt: "2026-07-14T01:00:00Z" },
      update,
      read,
      historicalQuestion,
      { id: "answer", kind: "assistant", content: "README 结构正常", createdAt: "2026-07-14T01:07:00Z" }
    ];
    const selected = agent(1, {
      id: "call-stable",
      label: "README 审查",
      task: "审查 README",
      contexts,
      updates: [{ content: "已经完成扫描", createdAt: "2026-07-14T01:05:00Z" }]
    });
    const user = userEvent.setup();
    render(
      <SubagentPanel
        agent={selected}
      />
    );

    expect(screen.getByRole("heading", { name: "README 审查" })).toBeInTheDocument();
    expect(screen.getByText("只读终端")).toBeInTheDocument();
    expect(screen.getByLabelText("README 审查只读终端")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "进行中" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /完成/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "返回子代理列表" })).not.toBeInTheDocument();
    expect(screen.getByText("审查 README")).toBeInTheDocument();
    expect(screen.getByText("README 结构正常")).toBeInTheDocument();
    expect(screen.getByText("历史上需要哪种格式？").closest(".question-history")).toHaveTextContent("提问");
    expect(screen.getAllByText("已经完成扫描")).toHaveLength(1);
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /编辑|删除|发送/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /读取了文件.*完成/ }));
    expect(screen.getAllByText(/README\.md/).length).toBeGreaterThan(0);
    expect(screen.getByText("README content")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "关闭只读终端页面" })).not.toBeInTheDocument();
  });

  it("reuses main conversation cards without exposing edit, delete, or insertion controls", () => {
    const selected = agent(1, {
      id: "shared-message-renderer",
      label: "消息渲染审查",
      contexts: [
        { id: "shared-task", kind: "user", content: "检查主消息复用", createdAt: "2026-07-14T01:00:00Z" },
        { id: "shared-answer", kind: "assistant", content: "已复用主消息样式", createdAt: "2026-07-14T01:01:00Z" }
      ]
    });
    render(
      <SubagentPanel
        agent={selected}
        onClose={vi.fn()}
      />
    );

    const transcript = screen.getByLabelText("消息渲染审查只读终端");
    const taskCard = within(transcript).getByText("检查主消息复用").closest<HTMLElement>(".context-card--user");
    const answerCard = within(transcript).getByText("已复用主消息样式").closest<HTMLElement>(".context-card--assistant");
    expect(taskCard).not.toBeNull();
    expect(answerCard).not.toBeNull();
    expect(transcript.querySelector(".context-actions")).not.toBeInTheDocument();
    expect(within(transcript).queryByRole("textbox")).not.toBeInTheDocument();
    expect(within(transcript).queryByRole("button", { name: /编辑|删除|发送/ })).not.toBeInTheDocument();

    fireEvent.contextMenu(taskCard!);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("pins the task above updates and replies across the live-to-persisted handoff", () => {
    const task = "始终显示在子会话顶部";
    const update = "已经完成第一阶段";
    const streamingTool: ToolContext = {
      id: "stream-subagent-stable-order",
      kind: "tool",
      toolName: "subagent",
      input: { task, label: "顺序审查" },
      result: { success: true, output: "", executedAt: "2026-07-14T01:00:00Z", durationMs: 0 },
      streaming: true,
      streamStatus: "running",
      live: {
        contexts: [{
          id: "streaming-answer",
          kind: "assistant",
          content: "正在整理流式答复",
          streaming: true,
          createdAt: "2026-07-14T01:03:00Z"
        }],
        updates: [{ content: update, createdAt: "2026-07-14T01:02:00Z" }]
      },
      createdAt: "2026-07-14T01:00:00Z"
    };
    const persistedTool: ToolContext = {
      id: "persisted-subagent-stable-order",
      kind: "tool",
      toolName: "subagent",
      input: { task, label: "顺序审查" },
      result: { success: true, output: "最终答复", executedAt: "2026-07-14T01:06:00Z", durationMs: 360 },
      subagent: {
        task,
        status: "completed",
        contexts: [
          // Persisted clocks are not trusted for semantic placement: this task
          // is intentionally newer than every child event but must remain first.
          { id: "persisted-task", kind: "user", content: task, createdAt: "2026-07-14T01:05:00Z" },
          { id: "persisted-answer", kind: "assistant", content: "最终答复", createdAt: "2026-07-14T01:04:00Z" }
        ],
        updates: [{ content: update, createdAt: "2026-07-14T01:02:00Z" }]
      },
      createdAt: "2026-07-14T01:00:00Z"
    };
    const onClose = vi.fn();
    const assertTaskIsFirst = () => {
      const transcript = screen.getByLabelText("顺序审查只读终端");
      const taskCard = within(transcript).getByText(task).closest<HTMLElement>(".context-card--user");
      expect(taskCard).not.toBeNull();
      const timeline = transcript.querySelectorAll<HTMLElement>(".context-card, .subagent-activity");
      expect(timeline.length).toBeGreaterThan(1);
      expect(timeline[0]).toBe(taskCard);
    };

    const streamingAgent = deriveSubagentViews([streamingTool])[0];
    const persistedAgent = deriveSubagentViews([persistedTool])[0];
    const { rerender } = render(<SubagentPanel agent={streamingAgent} onClose={onClose} />);
    assertTaskIsFirst();
    expect(screen.getByText("正在整理流式答复")).toBeInTheDocument();
    expect(screen.getByLabelText("顺序审查只读终端").querySelector('[data-stream-waiting="true"]')).toHaveAccessibleName("模型正在生成");

    rerender(<SubagentPanel agent={persistedAgent} onClose={onClose} />);
    assertTaskIsFirst();
    expect(screen.getByText(update)).toBeInTheDocument();
    expect(screen.getByText("最终答复")).toBeInTheDocument();
    expect(screen.queryByText("正在整理流式答复")).not.toBeInTheDocument();
    expect(screen.getByLabelText("顺序审查只读终端").querySelector('[data-stream-waiting="true"]')).not.toBeInTheDocument();
  });

  it("keeps a selected detail open when the stable id receives a new snapshot", () => {
    const first = agent(1, { id: "stable", summary: "工作中", status: "running", completedAt: null });
    const onClose = vi.fn();
    const { rerender } = render(<SubagentPanel agent={first} onClose={onClose} />);
    expect(screen.getByRole("heading", { name: "子代理 1" })).toBeInTheDocument();

    rerender(<SubagentPanel agent={agent(1, { id: "stable", summary: "完成", status: "completed" })} onClose={onClose} />);
    expect(screen.getByRole("heading", { name: "子代理 1" })).toBeInTheDocument();
    expect(screen.getByText("已完成")).toBeInTheDocument();
  });

  it("links back to a real parent agent but never to the workflow run above a step", () => {
    const onSelectAgent = vi.fn();
    const parent = agent(1, { id: "parent", label: "调度者", childIds: ["child"] });
    const child = agent(2, { id: "child", label: "子任务", parentId: "parent", depth: 1 });
    const { rerender } = render(
      <SubagentPanel agent={child} agents={[parent, child]} onSelectAgent={onSelectAgent} />
    );
    expect(screen.getByRole("button", { name: "返回上级子代理 调度者" })).toBeInTheDocument();

    // A workflow run is a script: it has no transcript to go back to, so the
    // step above it offers no way into one. The run's own panel in the task
    // container is what the step belongs to.
    const run = agent(3, {
      id: "run",
      label: "review-proxy",
      kind: "workflowStep",
      workflowRun: true,
      childIds: ["step"]
    });
    const step = agent(4, {
      id: "step",
      label: "审查一",
      kind: "workflowStep",
      parentId: "run",
      depth: 1
    });
    rerender(<SubagentPanel agent={step} agents={[run, step]} onSelectAgent={onSelectAgent} />);
    expect(screen.queryByRole("button", { name: /返回上级子代理/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /review-proxy/ })).not.toBeInTheDocument();
  });

});

