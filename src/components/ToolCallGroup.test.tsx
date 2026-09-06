import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ASK_USER_PENDING_OUTPUT } from "../types";
import type { ContextItem, ToolContext, ToolDescriptor } from "../types";
import {
  buildContextRenderNodes,
  ToolCallGroup,
  type IndexedToolContext
} from "./ToolCallGroup";

const CREATED_AT = "2026-07-14T00:00:00Z";

interface ToolOptions {
  round?: number;
  modelTurnId?: string;
  input?: ToolContext["input"];
  success?: boolean;
  output?: string;
  diff?: string;
  durationMs?: number;
  streaming?: boolean;
  streamStatus?: ToolContext["streamStatus"];
  live?: ToolContext["live"];
  subagent?: ToolContext["subagent"];
}

function tool(id: string, toolName: string, options: ToolOptions = {}): ToolContext {
  const {
    round,
    modelTurnId,
    input = {},
    success = true,
    output = `${id}-output`,
    diff,
    durationMs = 12,
    streaming,
    streamStatus,
    live,
    subagent
  } = options;
  return {
    id,
    kind: "tool",
    toolName,
    input,
    result: {
      success,
      output,
      ...(diff === undefined ? {} : { diff }),
      executedAt: CREATED_AT,
      durationMs
    },
    ...(round === undefined ? {} : { round }),
    ...(modelTurnId === undefined ? {} : { modelTurnId }),
    ...(streaming === undefined ? {} : { streaming }),
    ...(streamStatus === undefined ? {} : { streamStatus }),
    ...(live === undefined ? {} : { live }),
    ...(subagent === undefined ? {} : { subagent }),
    createdAt: CREATED_AT
  };
}

function entry(item: ToolContext, index: number): IndexedToolContext {
  return { item, index };
}

function nodeShape(nodes: ReturnType<typeof buildContextRenderNodes>) {
  return nodes.map((node) => {
    if (node.kind === "context") return { kind: node.kind, ids: [node.item.id] };
    if (node.kind === "workflow-card") return { kind: node.kind, ids: [node.entry.item.id] };
    if (node.kind === "question") {
      return {
        kind: node.kind,
        ids: [node.entry.item.id, ...(node.answer ? [node.answer.item.id] : [])]
      };
    }
    return { kind: node.kind, ids: node.entries.map(({ item }) => item.id) };
  });
}

function getRow(container: HTMLElement, id: string): HTMLElement {
  const row = container.querySelector<HTMLElement>(`[data-context-id="${id}"]`);
  expect(row).not.toBeNull();
  return row!;
}

function getRowToggle(row: HTMLElement): HTMLButtonElement {
  const button = row.querySelector<HTMLButtonElement>(".tool-context__summary");
  expect(button).not.toBeNull();
  return button!;
}

describe("buildContextRenderNodes", () => {
  it("keeps empty canonical model-turn anchors out of the visible timeline", () => {
    const contexts: ContextItem[] = [
      {
        id: "empty-reasoning",
        kind: "reasoning",
        content: "",
        round: 1,
        modelTurnId: "turn-1",
        createdAt: CREATED_AT
      },
      {
        id: "model-turn-anchor",
        kind: "assistant",
        content: "",
        round: 1,
        modelTurnId: "turn-1",
        createdAt: CREATED_AT
      },
      tool("visible-tool", "read", { round: 1, modelTurnId: "turn-1" })
    ];

    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "tool-group", ids: ["visible-tool"] }
    ]);
  });

  /**
   * Empty reasoning with elapsed time or tokens is a real encrypted-only thought,
   * not a protocol anchor. Those metrics are its only visible trace.
   */
  it("keeps a summary-less reasoning round visible when it carries its own time or tokens", () => {
    const anchorFields = { round: 1, modelTurnId: "turn-1", createdAt: CREATED_AT } as const;
    const cases: ContextItem[] = [
      { id: "timed-reasoning", kind: "reasoning", content: "", durationMs: 18_000, ...anchorFields },
      { id: "billed-reasoning", kind: "reasoning", content: "", tokens: 1_240, ...anchorFields }
    ];

    for (const reasoning of cases) {
      expect(nodeShape(buildContextRenderNodes([reasoning, tool("t", "read", { round: 1, modelTurnId: "turn-1" })])))
        .toEqual([
          { kind: "context", ids: [reasoning.id] },
          { kind: "tool-group", ids: ["t"] }
        ]);
    }
  });

  it("regroups tools after a visible boundary is deleted even when a hidden anchor remains", () => {
    const deletedBoundary: ContextItem = {
      id: "deleted-boundary",
      kind: "assistant",
      content: "旧的中间回复",
      round: 2,
      modelTurnId: "turn-2",
      createdAt: CREATED_AT
    };
    const hiddenAnchor: ContextItem = {
      id: "hidden-anchor",
      kind: "assistant",
      content: "",
      createdAt: CREATED_AT
    };
    const hiddenReasoning: ContextItem = {
      id: "hidden-reasoning",
      kind: "reasoning",
      content: "",
      createdAt: CREATED_AT
    };
    const beforeDeletion: ContextItem[] = [
      tool("first-tool", "read", { round: 1, modelTurnId: "turn-1" }),
      deletedBoundary,
      hiddenAnchor,
      hiddenReasoning,
      tool("second-tool", "find")
    ];

    expect(nodeShape(buildContextRenderNodes(beforeDeletion))).toEqual([
      { kind: "tool-group", ids: ["first-tool"] },
      { kind: "context", ids: ["deleted-boundary"] },
      { kind: "tool-group", ids: ["second-tool"] }
    ]);
    expect(nodeShape(buildContextRenderNodes(
      beforeDeletion.filter((context) => context.id !== deletedBoundary.id)
    ))).toEqual([
      { kind: "tool-group", ids: ["first-tool", "second-tool"] }
    ]);
  });

  it("groups every adjacent tool call until a non-tool message creates a boundary", () => {
    const user: ContextItem = {
      id: "user",
      kind: "user",
      content: "开始",
      createdAt: CREATED_AT
    };
    const reasoning: ContextItem = {
      id: "reasoning",
      kind: "reasoning",
      content: "继续检查",
      createdAt: CREATED_AT
    };
    const contexts: ContextItem[] = [
      user,
      tool("round-1-read", "read", { round: 1, input: { path: "a.ts" } }),
      tool("round-1-edit", "edit", { round: 1, input: { path: "a.ts" } }),
      reasoning,
      tool("round-1-after-boundary", "find", { round: 1, input: { query: "*.ts" } }),
      tool("round-2-command", "bash", { round: 2, input: { command: "pwd" } }),
      tool("legacy-read", "read", { input: { path: "legacy-a.ts" } }),
      tool("legacy-edit", "edit", { input: { path: "legacy-b.ts" } })
    ];

    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "context", ids: ["user"] },
      { kind: "tool-group", ids: ["round-1-read", "round-1-edit"] },
      { kind: "context", ids: ["reasoning"] },
      {
        kind: "tool-group",
        ids: ["round-1-after-boundary", "round-2-command", "legacy-read", "legacy-edit"]
      }
    ]);
  });

  it("folds agent and task-state calls into the one tool group, leaving only the question outside", () => {
    const agentNames = [
      "agent_spawn",
      "agent_send",
      "send_message",
      "followup_task",
      "task_wait",
      "task_list",
      "subagent",
      "subagent_update",
      "subagent_activity",
      "update"
    ];
    const agentCalls = agentNames.map((name) => tool(`${name}-call`, name, { round: 7 }));
    const contexts: ContextItem[] = [
      agentCalls[0],
      tool("read-call", "read", { round: 7, input: { path: "src/a.ts" } }),
      tool("todo-event", "todo", { round: 7, input: { action: "update", taskId: "task-1", status: "in_progress" } }),
      agentCalls[1],
      agentCalls[2],
      tool("edit-call", "edit", { round: 7, input: { path: "src/a.ts" } }),
      tool("ask-event", "ask_user", { round: 7, input: { question: "继续吗？" } }),
      ...agentCalls.slice(3)
    ];

    const nodes = buildContextRenderNodes(contexts);
    expect(nodeShape(nodes)).toEqual([
      {
        kind: "tool-group",
        ids: [
          "agent_spawn-call",
          "read-call",
          "todo-event",
          "agent_send-call",
          "send_message-call",
          "edit-call",
          ...agentNames.slice(3).map((name) => `${name}-call`)
        ]
      },
      { kind: "question", ids: ["ask-event"] }
    ]);

    // Rows keep timeline order, so the insertion menu and the edit/delete
    // affordances still address the raw index they were built from.
    const group = nodes.find((node) => node.kind === "tool-group");
    if (group?.kind === "tool-group") {
      expect(group.entries.map(({ index }) => index)).toEqual([
        0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13
      ]);
    }
  });

  it("pairs the next user boundary with ask_user and removes the standalone user node", () => {
    const ask = tool("ask", "ask_user", {
      input: { question: "继续吗？", options: ["继续", "停止"] },
      output: ASK_USER_PENDING_OUTPUT
    });
    const answer: ContextItem = {
      id: "answer",
      kind: "user",
      content: "继续",
      createdAt: CREATED_AT
    };
    expect(nodeShape(buildContextRenderNodes([ask, answer]))).toEqual([
      { kind: "question", ids: ["ask", "answer"] }
    ]);
  });

  it("gives every live workflow run its own card instead of merging them", () => {
    // Two runs in one block are two ledgers. A merged node could only key its
    // live entries on one call id, which would silently drop the other card.
    const contexts: ContextItem[] = [
      tool("wf-a", "workflow", { round: 3, streaming: true, streamStatus: "running" }),
      tool("read-call", "read", { round: 3, input: { path: "src/a.ts" } }),
      tool("wf-b", "workflow", { round: 3, streaming: true, streamStatus: "running" })
    ];

    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "workflow-card", ids: ["wf-a"] },
      { kind: "tool-group", ids: ["read-call"] },
      { kind: "workflow-card", ids: ["wf-b"] }
    ]);
  });

  it("keeps a settled workflow run on its own card, one node per call", () => {
    // The card reads the agent roster, which outlives the model run, so a
    // finished plan keeps the card it had while it ran. Two runs in one block
    // stay two nodes: a merged one would have to pick a single call id to key
    // on and would silently drop the other run.
    const settled = tool("wf-done", "workflow", { round: 3, output: "计划结果" });
    const completed = tool("wf-completed", "workflow", {
      round: 3,
      streaming: true,
      streamStatus: "completed"
    });

    expect(nodeShape(buildContextRenderNodes([settled, completed]))).toEqual([
      { kind: "workflow-card", ids: ["wf-done"] },
      { kind: "workflow-card", ids: ["wf-completed"] }
    ]);
  });

  it("keeps every workflow call out of the ordinary group summary", () => {
    // The summary and the row list are two filters over the same block. If they
    // disagree the call is counted and then never drawn, or the reverse.
    const settled = tool("wf-done", "workflow", { round: 3 });
    const live = tool("wf-live", "workflow", { round: 3, streaming: true, streamStatus: "running" });
    const read = tool("read-1", "read", { round: 3, input: { path: "a.ts" } });
    const { container } = render(
      <ToolCallGroup
        entries={[entry(settled, 0), entry(live, 1), entry(read, 2)]}
        tools={[]}
        insertionIndex={null}
      />
    );

    expect(container.querySelectorAll(".tool-call-group__item")).toHaveLength(1);
    expect(container.querySelector('[data-context-id="read-1"]')).toBeInTheDocument();
    expect(container.querySelector('[data-context-id="wf-done"]')).toBeNull();
    expect(container.querySelector('[data-context-id="wf-live"]')).toBeNull();
    expect(container.querySelector(".tool-call-group__heading")).toHaveTextContent("读取了文件 a.ts");
  });
});

describe("ToolCallGroup", () => {
  it("workflowFallback renders a settled workflow call the ordinary block refuses", async () => {
    const user = userEvent.setup();
    const wf = tool("wf-fallback", "workflow", {
      round: 1,
      input: { name: "audit" },
      output: "workflow:runabc"
    });

    // The ordinary tool block must exclude workflow calls; the workflow card owns them.
    const { container: ordinary } = render(
      <ToolCallGroup entries={[entry(wf, 0)]} tools={[]} insertionIndex={null} />
    );
    expect(ordinary.firstChild).toBeNull();

    // When no WorkflowRunView can be dispatched after reload or restart, this fallback
    // must render the receipt instead of passing the workflow item to the filtered block.
    const { container } = render(
      <ToolCallGroup
        entries={[entry(wf, 0)]}
        tools={[]}
        insertionIndex={null}
        workflowFallback
      />
    );
    const row = getRow(container, wf.id);
    expect(within(row).getByText("完成了工作流")).toBeInTheDocument();
    await user.click(getRowToggle(row));
    expect(container.textContent).toContain("workflow:runabc");
  });

  it("collapses and restores the first-level tool-block summary", async () => {
    const user = userEvent.setup();
    const entries = [
      entry(tool("read", "read", { round: 1, input: { path: "a.ts" }, output: "line" }), 2),
      entry(tool("find", "find", { round: 1, input: { query: "*.ts" }, output: "a.ts" }), 3)
    ];
    const { container } = render(
      <ToolCallGroup entries={entries} tools={[]} insertionIndex={null} />
    );

    const toggle = screen.getByRole("button", { name: "检查了 2 次文件与目录" });
    const list = screen.getByRole("list");
    const listRegion = container.querySelector<HTMLElement>(".tool-call-group__list-region")!;
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveAttribute("aria-controls", list.id);
    expect(listRegion).not.toHaveClass("collapse-region--closed");

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(listRegion).toHaveClass("collapse-region--closed");
    expect(listRegion).toHaveAttribute("aria-hidden", "true");
    expect(screen.queryByRole("list")).not.toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(listRegion).not.toHaveClass("collapse-region--closed");
    expect(screen.getByRole("list")).toBe(list);
  });

  it("expands each second-level row independently and renders write/edit diffs", async () => {
    const user = userEvent.setup();
    const write = tool("write-new", "write", {
      round: 2,
      input: { path: "new.ts", content: "export const created = true;\n" },
      output: "已创建 new.ts",
      diff: "--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1 @@\n+export const created = true;\n"
    });
    const editCall = tool("edit-existing", "edit", {
      round: 2,
      input: { path: "existing.ts", find: "1", replace: "2" },
      output: "已编辑 existing.ts",
      diff: "--- a/existing.ts\n+++ b/existing.ts\n@@ -1 +1 @@\n-export const value = 1;\n+export const value = 2;\n"
    });
    const { container } = render(
      <ToolCallGroup
        entries={[entry(write, 0), entry(editCall, 1)]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const writeRow = getRow(container, write.id);
    const editRow = getRow(container, editCall.id);
    const writeToggle = getRowToggle(writeRow);
    const editToggle = getRowToggle(editRow);

    expect(within(writeRow).getByText("创建了文件")).toBeInTheDocument();
    expect(within(editRow).getByText("编辑了文件")).toBeInTheDocument();
    expect(writeToggle).toHaveAttribute("aria-expanded", "false");
    expect(editToggle).toHaveAttribute("aria-expanded", "false");
    expect(writeRow.querySelector('[data-tool-name="write"][data-tool-family="diff"]')).not.toBeInTheDocument();
    expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).not.toBeInTheDocument();

    await user.click(writeToggle);
    expect(writeToggle).toHaveAttribute("aria-expanded", "true");
    expect(editToggle).toHaveAttribute("aria-expanded", "false");
    expect(writeRow.querySelector('[data-tool-name="write"][data-tool-family="diff"]')).toBeInTheDocument();
    expect(within(writeRow).getByRole("region", { name: "new.ts 文件差异" })).toHaveTextContent("export const created = true;");
    expect(within(editRow).queryByRole("region", { name: "existing.ts 文件差异" })).not.toBeInTheDocument();

    await user.click(editToggle);
    expect(writeToggle).toHaveAttribute("aria-expanded", "true");
    expect(editToggle).toHaveAttribute("aria-expanded", "true");
    expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).toBeInTheDocument();
    expect(within(editRow).getByRole("region", { name: "existing.ts 文件差异" })).toHaveTextContent("export const value = 2;");

    await user.click(writeToggle);
    expect(writeToggle).toHaveAttribute("aria-expanded", "false");
    expect(editToggle).toHaveAttribute("aria-expanded", "true");
    expect(within(writeRow).queryByRole("region", { name: "new.ts 文件差异" })).not.toBeInTheDocument();
    expect(within(editRow).getByRole("region", { name: "existing.ts 文件差异" })).toBeInTheDocument();
    await waitFor(() => {
      expect(writeRow.querySelector('[data-tool-name="write"][data-tool-family="diff"]')).not.toBeInTheDocument();
    });

    await user.click(container.querySelector<HTMLButtonElement>(".tool-call-group__toggle")!);
    await waitFor(() => {
      expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).not.toBeInTheDocument();
    });
    await user.click(container.querySelector<HTMLButtonElement>(".tool-call-group__toggle")!);
    expect(editToggle).toHaveAttribute("aria-expanded", "true");
    expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).toBeInTheDocument();
  });

  it("auto-expands a row when an existing call transitions to failure", async () => {
    const succeeded = tool("read-transition", "read", {
      round: 3,
      input: { path: "broken.ts" },
      output: "old content"
    });
    const { container, rerender } = render(
      <ToolCallGroup entries={[entry(succeeded, 4)]} tools={[]} insertionIndex={null} />
    );
    const row = getRow(container, succeeded.id);
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "false");

    const failed: ToolContext = {
      ...succeeded,
      result: {
        ...succeeded.result,
        success: false,
        output: "无法读取 broken.ts"
      }
    };
    rerender(
      <ToolCallGroup entries={[entry(failed, 4)]} tools={[]} insertionIndex={null} />
    );

    await waitFor(() => expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "true"));
    expect(within(row).getByRole("alert")).toHaveTextContent("读取文件失败");
    expect(within(row).getByRole("alert")).toHaveTextContent("无法读取 broken.ts");
  });

  it("keeps an announced streaming call non-expandable and hides persisted-call actions", async () => {
    const user = userEvent.setup();
    const announced = tool("announced-read", "read", {
      round: 4,
      input: { path: "pending.ts" },
      output: "",
      durationMs: 0,
      streaming: true,
      streamStatus: "announced"
    });
    const { container } = render(
      <ToolCallGroup
        entries={[entry(announced, 5)]}
        tools={[]}
        insertionIndex={null}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />
    );
    const row = getRow(container, announced.id);
    const toggle = getRowToggle(row);

    expect(row).toHaveAttribute("aria-busy", "true");
    expect(row.querySelector(".stream-waiting__square--tool")).toBeInTheDocument();
    expect(toggle).toHaveTextContent("正在读取文件");
    expect(toggle).toHaveAttribute("aria-disabled", "true");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).not.toHaveAttribute("aria-controls");
    expect(row.querySelector(".tool-context__details-region")).not.toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: /编辑工具调用/ })).not.toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: /删除工具调用/ })).not.toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(row.querySelector(".tool-context__details-region")).not.toBeInTheDocument();
  });

  it("edits, deletes, marks insertion, and reports original context indices", async () => {
    const user = userEvent.setup();
    const first = tool("first-row", "read", { round: 5, input: { path: "first.ts" } });
    const second = tool("second-row", "find", { round: 5, input: { query: "*.tsx" } });
    const onEdit = vi.fn();
    const onDelete = vi.fn();
    const onOpenInsert = vi.fn();
    const { container } = render(
      <ToolCallGroup
        entries={[entry(first, 4), entry(second, 7)]}
        tools={[]}
        insertionIndex={7}
        onEdit={onEdit}
        onDelete={onDelete}
        onOpenInsert={onOpenInsert}
      />
    );
    const firstRow = getRow(container, first.id);
    const secondRow = getRow(container, second.id);
    const insertionLine = container.querySelector<HTMLElement>(".tool-call-group__insertion");
    expect(container.querySelectorAll(".tool-call-group__insertion")).toHaveLength(1);
    expect(secondRow.previousElementSibling).toBe(insertionLine);

    await user.click(within(firstRow).getByRole("button", { name: "编辑工具调用 读取了文件" }));
    await user.click(within(secondRow).getByRole("button", { name: "删除工具调用 查找了文件" }));
    expect(onEdit).toHaveBeenCalledWith(first);
    expect(onDelete).toHaveBeenCalledWith(second);

    vi.spyOn(secondRow, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 100,
      top: 100,
      left: 0,
      right: 600,
      bottom: 200,
      width: 600,
      height: 100,
      toJSON: () => ({})
    } as DOMRect);
    fireEvent.contextMenu(secondRow, { clientY: 125 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(7);
    fireEvent.contextMenu(secondRow, { clientY: 175 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(8);
    fireEvent.keyDown(secondRow, { key: "F10", shiftKey: true });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(8);

    const group = container.querySelector<HTMLElement>(".tool-call-group")!;
    vi.spyOn(group, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      right: 600,
      bottom: 200,
      width: 600,
      height: 200,
      toJSON: () => ({})
    } as DOMRect);
    fireEvent.contextMenu(group, { clientY: 25 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(4);
    fireEvent.contextMenu(group, { clientY: 175 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(8);
  });

  it("uses the raw detail fallback and descriptor label for an unknown tool", async () => {
    const user = userEvent.setup();
    const unknown = tool("unknown-call", "mystery_tool", {
      round: 6,
      input: { payload: "opaque" },
      output: "opaque-result",
      durationMs: 37
    });
    const descriptor: ToolDescriptor = {
      name: "mystery_tool",
      label: "神秘工具",
      description: "",
      category: "orchestration",
      dangerous: false,
      parameters: []
    };
    const { container } = render(
      <ToolCallGroup
        entries={[entry(unknown, 9)]}
        tools={[descriptor]}
        insertionIndex={null}
      />
    );
    const row = getRow(container, unknown.id);
    expect(within(row).getByText("使用了 神秘工具")).toBeInTheDocument();
    expect(within(row).getByText("37 ms")).toBeInTheDocument();

    await user.click(getRowToggle(row));
    const detail = row.querySelector<HTMLElement>('[data-tool-name="mystery_tool"][data-tool-family="raw"]');
    expect(detail).toBeInTheDocument();
    expect(within(detail!).getByText("opaque-result").tagName).toBe("PRE");
    expect(within(detail!).getByText("原始数据")).toBeInTheDocument();
  });


  it("gives an agent-run row the child's real status instead of the call receipt", () => {
    // A completed spawn call may still own a live background child, and a child
    // that was stopped or hit its round limit ended without failing. Reading
    // result.success alone reported all three as plain success or failure.
    const spawn = tool("spawn-live", "agent_spawn", {
      round: 9,
      input: { name: "reviewer", label: "接口审查", prompt: "审查接口" },
      streaming: true,
      streamStatus: "completed"
    });
    const stopped = tool("spawn-stopped", "agent_spawn", {
      round: 9,
      input: { name: "tester" },
      live: { contexts: [], updates: [], status: "stopped" }
    });
    const { container } = render(
      <ToolCallGroup entries={[entry(spawn, 0), entry(stopped, 1)]} tools={[]} insertionIndex={null} />
    );

    expect(getRow(container, spawn.id)).toHaveClass("tool-call-group__item--running");
    expect(getRow(container, spawn.id)).toHaveAttribute("aria-busy", "true");
    const stoppedRow = getRow(container, stopped.id);
    expect(stoppedRow).toHaveClass("tool-call-group__item--halted");
    expect(stoppedRow).not.toHaveClass("tool-call-group__item--error");
    expect(within(stoppedRow).getByText("已停止")).toBeInTheDocument();
  });

  it("offers the child transcript only on agent-run rows and only where a drawer exists", async () => {
    const user = userEvent.setup();
    const onOpenSubagent = vi.fn();
    const spawn = tool("spawn-open", "agent_spawn", {
      round: 9,
      input: { name: "reviewer", label: "接口审查" }
    });
    const note = tool("note-open", "subagent_update", { round: 9, input: { message: "正在跑 e2e" } });
    const read = tool("read-open", "read", { round: 9, input: { path: "src/a.ts" } });
    const entries = [entry(spawn, 0), entry(note, 1), entry(read, 2)];

    const routed = render(
      <ToolCallGroup entries={entries} tools={[]} insertionIndex={null} onOpenSubagent={onOpenSubagent} />
    );
    expect(routed.container.querySelectorAll(".tool-call-group__open-agent")).toHaveLength(1);
    await user.click(within(getRow(routed.container, spawn.id)).getByRole("button", { name: /打开子代理/ }));
    expect(onOpenSubagent).toHaveBeenCalledWith("reviewer");
    expect(getRow(routed.container, note.id).querySelector(".tool-call-group__open-agent")).toBeNull();
    expect(getRow(routed.container, read.id).querySelector(".tool-call-group__open-agent")).toBeNull();
    routed.unmount();

    // The read-only subagent terminal renders the same rows and owns no drawer,
    // so the affordance must disappear rather than dangle.
    const ambient = render(<ToolCallGroup entries={entries} tools={[]} insertionIndex={null} readOnly />);
    expect(ambient.container.querySelector(".tool-call-group__open-agent")).toBeNull();
  });

  it("summarizes a mixed block by what it actually contains", () => {
    const { container } = render(
      <ToolCallGroup
        entries={[
          entry(tool("s-read", "read", { round: 2, input: { path: "src/a.ts" } }), 0),
          entry(tool("s-spawn", "agent_spawn", { round: 2, input: { name: "reviewer" } }), 1),
          entry(tool("s-todo", "todo", { round: 2, input: { action: "create", subject: "修复登录" } }), 2)
        ]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const toggle = container.querySelector<HTMLElement>(".tool-call-group__toggle");
    expect(toggle).toHaveTextContent("检查了 1 次文件与目录");
    expect(toggle).toHaveTextContent("1 次子代理操作");
    expect(toggle).toHaveTextContent("1 次状态变更");
  });

});
