import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ASK_USER_PENDING_OUTPUT } from "../test/fixtures";
import type {
  ContextItem,
  ReasoningContext,
  SystemContext,
  ToolContext,
  ToolDescriptor
} from "../types";
import type { WorkflowRunStep, WorkflowRunView } from "../lib/workflowRuns";
import { recordToolErrorExplanationForTests, resetToolExplanationsForTests } from "../lib/localModel";
import { setPathOpenHandler } from "../lib/pathLinks";
import type { PathOpenRequest } from "../lib/pathLinks";
import {
  buildContextRenderNodes,
  TimelineBlock,
  type BlockEntry
} from "./TimelineBlock";

const CREATED_AT = "2026-07-14T00:00:00Z";

const MCP_TOOL = "mcp__server_0123456789__search__abcdef0123";

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

function reasoning(id: string, options: Partial<Omit<ReasoningContext, "id" | "kind">> = {}): ReasoningContext {
  return { id, kind: "reasoning", content: "", createdAt: CREATED_AT, ...options };
}

function hook(id: string, options: Partial<SystemContext["hookExecution"]> = {}, content = ""): SystemContext {
  return {
    id,
    kind: "system",
    content,
    createdAt: CREATED_AT,
    hookExecution: {
      executionId: `${id}-exec`,
      hookId: `${id}-hook`,
      hookName: "format",
      event: "PostToolUse",
      status: "succeeded",
      contextInjected: false,
      ...options
    }
  };
}

function entry(item: ToolContext, index: number): BlockEntry {
  return { kind: "tool", item, index };
}

function workflowEntry(item: ToolContext, index: number): BlockEntry {
  return { kind: "workflow", item, index };
}

function reasoningEntry(item: ReasoningContext, index: number): BlockEntry {
  return { kind: "reasoning", item, index };
}

function hookEntry(item: SystemContext, index: number): BlockEntry {
  return { kind: "hook", item, index };
}

function workflowStep(key: string, label: string, state: WorkflowRunStep["state"]): WorkflowRunStep {
  return {
    key,
    planIndex: null,
    label,
    state,
    agentId: null,
    role: null,
    task: null,
    modelId: null,
    tokens: null,
    elapsedMs: null,
    cached: false,
    blocked: false,
    skipped: false
  };
}

function workflowView(): WorkflowRunView {
  const steps = [workflowStep("s0", "收集", "finished"), workflowStep("s1", "复核", "running")];
  return {
    id: "run-1",
    name: "重构",
    scriptName: "取数重构",
    description: "重构取数路径",
    state: "running",
    stepCount: 2,
    elapsedMs: 60_000,
    tokens: 1_240,
    phases: [{ key: "P", heading: "P", steps, done: 1, total: 2 }],
    steps,
    logs: [],
    running: true
  };
}

function nodeShape(nodes: ReturnType<typeof buildContextRenderNodes>) {
  return nodes.map((node) => {
    if (node.kind === "context") return { kind: node.kind, ids: [node.item.id] };
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
  const button = row.querySelector<HTMLButtonElement>(".timeline-row__summary");
  expect(button).not.toBeNull();
  return button!;
}

function rowText(row: HTMLElement, part: "name" | "line" | "stat"): string | undefined {
  return row.querySelector<HTMLElement>(`.timeline-row__${part}`)?.textContent ?? undefined;
}

describe("buildContextRenderNodes", () => {
  it("keeps empty canonical model-turn anchors out of the visible timeline", () => {
    const contexts: ContextItem[] = [
      reasoning("empty-reasoning", { round: 1, modelTurnId: "turn-1" }),
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
      { kind: "block", ids: ["visible-tool"] }
    ]);
  });

  /**
   * Empty reasoning with elapsed time or tokens is a real encrypted-only thought,
   * not a protocol anchor. Those metrics are its only visible trace, and the row
   * that carries them belongs in the block beside the work it preceded.
   */
  it("keeps a summary-less reasoning round visible when it carries its own time or tokens", () => {
    const anchorFields = { round: 1, modelTurnId: "turn-1" } as const;
    const cases: ReasoningContext[] = [
      reasoning("timed-reasoning", { durationMs: 18_000, ...anchorFields }),
      reasoning("billed-reasoning", { tokens: 1_240, ...anchorFields })
    ];

    for (const item of cases) {
      expect(nodeShape(buildContextRenderNodes([item, tool("t", "read", { round: 1, modelTurnId: "turn-1" })])))
        .toEqual([
          { kind: "block", ids: [item.id, "t"] }
        ]);
    }
  });

  /** A reasoning record that only announced itself is narrated beside the cat, not as a row. */
  it("drops an empty encrypted reasoning record while it is still streaming", () => {
    const live = reasoning("live-encrypted", { streaming: true, form: "encrypted" });
    expect(nodeShape(buildContextRenderNodes([live, tool("after", "read")]))).toEqual([
      { kind: "block", ids: ["after"] }
    ]);
  });

  it("regroups records after a visible boundary is deleted even when a hidden anchor remains", () => {
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
    const beforeDeletion: ContextItem[] = [
      tool("first-tool", "read", { round: 1, modelTurnId: "turn-1" }),
      deletedBoundary,
      hiddenAnchor,
      reasoning("hidden-reasoning"),
      tool("second-tool", "find")
    ];

    expect(nodeShape(buildContextRenderNodes(beforeDeletion))).toEqual([
      { kind: "block", ids: ["first-tool"] },
      { kind: "context", ids: ["deleted-boundary"] },
      { kind: "block", ids: ["second-tool"] }
    ]);
    expect(nodeShape(buildContextRenderNodes(
      beforeDeletion.filter((context) => context.id !== deletedBoundary.id)
    ))).toEqual([
      { kind: "block", ids: ["first-tool", "second-tool"] }
    ]);
  });

  it("runs one block until a message the user wrote or the model said interrupts it", () => {
    const user: ContextItem = { id: "user", kind: "user", content: "开始", createdAt: CREATED_AT };
    const prose: ContextItem = {
      id: "prose",
      kind: "assistant",
      content: "我先看一眼",
      round: 1,
      createdAt: CREATED_AT
    };
    const contexts: ContextItem[] = [
      user,
      tool("round-1-read", "read", { round: 1, input: { path: "a.ts" } }),
      tool("round-1-edit", "edit", { round: 1, input: { path: "a.ts" } }),
      prose,
      tool("round-1-after-boundary", "find", { round: 1, input: { query: "*.ts" } }),
      tool("round-2-command", "bash", { round: 2, input: { command: "pwd" } }),
      tool("legacy-read", "read", { input: { path: "legacy-a.ts" } }),
      tool("legacy-edit", "edit", { input: { path: "legacy-b.ts" } })
    ];

    // The provider's round number is execution bookkeeping, not a visual
    // boundary: `round-2-command` stays in the block `round-1-after-boundary`
    // opened.
    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "context", ids: ["user"] },
      { kind: "block", ids: ["round-1-read", "round-1-edit"] },
      { kind: "context", ids: ["prose"] },
      {
        kind: "block",
        ids: ["round-1-after-boundary", "round-2-command", "legacy-read", "legacy-edit"]
      }
    ]);
  });

  it("merges tool calls, reasoning and hook records that run contiguously into one block", () => {
    const contexts: ContextItem[] = [
      reasoning("thought", { content: "先读一遍" }),
      tool("read-call", "read", { input: { path: "src/a.ts" } }),
      hook("hook-record", { hookName: "format" }, "已格式化"),
      tool("edit-call", "edit", { input: { path: "src/a.ts" } })
    ];

    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "block", ids: ["thought", "read-call", "hook-record", "edit-call"] }
    ]);
  });

  it("folds agent calls into blocks, leaving only the question outside", () => {
    // Retired names (`agent_send`, `send_message`, `followup_task`, `subagent`,
    // …) are listed too: saved conversations still hold their cards.
    const agentNames = [
      "agent_spawn",
      "agent_send",
      "send_message",
      "followup_task",
      "task_wait",
      "task_list",
      "box",
      "subagent",
      "subagent_update",
      "subagent_activity",
      "update"
    ];
    const agentCalls = agentNames.map((name) => tool(`${name}-call`, name, { round: 7 }));
    const contexts: ContextItem[] = [
      agentCalls[0],
      tool("read-call", "read", { round: 7, input: { path: "src/a.ts" } }),
      agentCalls[1],
      agentCalls[2],
      tool("edit-call", "edit", { round: 7, input: { path: "src/a.ts" } }),
      tool("ask-event", "ask_user", { round: 7, input: { question: "继续吗？" } }),
      ...agentCalls.slice(3)
    ];

    const nodes = buildContextRenderNodes(contexts);
    expect(nodeShape(nodes)).toEqual([
      {
        kind: "block",
        ids: [
          "agent_spawn-call",
          "read-call",
          "agent_send-call",
          "send_message-call",
          "edit-call"
        ]
      },
      { kind: "question", ids: ["ask-event"] },
      { kind: "block", ids: agentNames.slice(3).map((name) => `${name}-call`) }
    ]);

    // Rows keep timeline order, so the insertion menu and the edit/delete
    // affordances still address the raw index they were built from.
    const blocks = nodes.filter((node) => node.kind === "block");
    expect(blocks.map((node) => (node.kind === "block" ? node.entries.map(({ index }) => index) : [])))
      .toEqual([[0, 1, 2, 3, 4], [6, 7, 8, 9, 10, 11, 12, 13]]);
    // The key names the block by the first record in it, so two blocks in one
    // run never collide.
    expect(blocks.map((node) => (node.kind === "block" ? node.key : "")))
      .toEqual(["block:agent_spawn-call", "block:followup_task-call"]);
  });

  it("splits the run at an ask_user instead of hoisting the question out of the middle", () => {
    const contexts: ContextItem[] = [
      tool("before", "read", { input: { path: "a.ts" } }),
      tool("ask", "ask_user", { input: { question: "继续吗？" }, output: ASK_USER_PENDING_OUTPUT }),
      tool("after", "bash", { input: { command: "npm test" } })
    ];

    // Stream order is the whole point: the question was asked between the two
    // calls, and a merged block would claim they ran back to back.
    expect(nodeShape(buildContextRenderNodes(contexts))).toEqual([
      { kind: "block", ids: ["before"] },
      { kind: "question", ids: ["ask"] },
      { kind: "block", ids: ["after"] }
    ]);
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

  it("keeps a workflow call in the block as its own row when it has a run to show", () => {
    const contexts: ContextItem[] = [
      tool("wf-a", "workflow", { round: 3, streaming: true, streamStatus: "running" }),
      tool("read-call", "read", { round: 3, input: { path: "src/a.ts" } }),
      tool("wf-b", "workflow", { round: 3, streaming: true, streamStatus: "running" })
    ];

    const nodes = buildContextRenderNodes(contexts, new Set(), () => true);
    expect(nodeShape(nodes)).toEqual([
      { kind: "block", ids: ["wf-a", "read-call", "wf-b"] }
    ]);
    // Two runs in one block are two ledgers, so each keeps a row of its own to
    // key its view on rather than being merged into one.
    expect(nodes[0].kind === "block" && nodes[0].entries.map(({ kind }) => kind))
      .toEqual(["workflow", "tool", "workflow"]);
  });

  it("drops an unfinished workflow call with no view but keeps a settled one", () => {
    // An unfinished run with no view would claim a zero-step workflow, which is
    // indistinguishable on screen from one whose steps never arrived. A settled
    // call has a receipt to show and stays either way.
    const live = tool("wf-live", "workflow", { round: 3, streaming: true, streamStatus: "running" });
    const completed = tool("wf-completed", "workflow", { round: 3, streaming: true, streamStatus: "completed" });
    const reloaded = tool("wf-reloaded", "workflow", { round: 3, output: "计划结果" });

    expect(nodeShape(buildContextRenderNodes([live, completed, reloaded]))).toEqual([
      { kind: "block", ids: ["wf-completed", "wf-reloaded"] }
    ]);
    expect(nodeShape(buildContextRenderNodes([live, completed, reloaded], new Set(), (id) => id === "wf-live")))
      .toEqual([
        { kind: "block", ids: ["wf-live", "wf-completed", "wf-reloaded"] }
      ]);
  });

  it("hoists an ordinary call that is still in flight onto the stream indicator", () => {
    const live = tool("live-read", "read", { streaming: true, streamStatus: "running", input: { path: "a.ts" } });
    expect(nodeShape(buildContextRenderNodes([live, tool("settled", "find")]))).toEqual([
      { kind: "block", ids: ["settled"] }
    ]);
  });
});

describe("TimelineBlock", () => {
  it("collapses and restores the block's list region", async () => {
    const user = userEvent.setup();
    const entries = [
      entry(tool("read", "read", { round: 1, input: { path: "a.ts" }, output: "line" }), 2),
      entry(tool("find", "find", { round: 1, input: { query: "*.ts" }, output: "a.ts" }), 3)
    ];
    render(
      <TimelineBlock entries={entries} tools={[]} insertionIndex={null} />
    );

    // One clause per bucket the block contains — there is no single-call
    // special case and no "N running, N failed" suffix.
    const toggle = screen.getByRole("button", { name: "读取了 1 个文件，检查了 1 次文件与目录" });
    const list = screen.getByRole("list");
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveAttribute("aria-controls", list.id);
    expect(list).not.toHaveAttribute("hidden");
    // The chevron stands against the end of the summary, inside the toggle, and
    // the figure it shares the heading with stays out of the accessible name.
    expect(toggle.querySelector(".timeline-block__chevron"))
      .toHaveClass("disclosure-chevron", "disclosure-chevron--open");
    expect(toggle.querySelector(".timeline-block__tokens")).toHaveTextContent(/token/);

    // The list closes in the same frame, with no region left mid-transition.
    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(list).toHaveAttribute("hidden");
    expect(screen.queryByRole("list")).not.toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(list).not.toHaveAttribute("hidden");
    expect(screen.getByRole("list")).toBe(list);
  });

  it("names a row by what the call did and keeps the wire name on the disclosure", async () => {
    const user = userEvent.setup();
    const command = tool("bash-call", "bash", {
      round: 1,
      input: { command: "npm test" },
      output: "ok"
    });
    const { container } = render(
      <TimelineBlock entries={[entry(command, 0)]} tools={[]} insertionIndex={null} />
    );

    const row = getRow(container, command.id);
    expect(row).toHaveAttribute("data-row-kind", "tool");
    expect(rowText(row, "name")).toBe("运行了 Bash 命令");
    expect(rowText(row, "line")).toBeUndefined();
    expect(rowText(row, "stat")).toBe("8 token");
    const toggle = getRowToggle(row);
    expect(toggle).toHaveAttribute("aria-label", "运行了 Bash 命令 · bash · npm test · 完成");
    expect(toggle).toHaveAttribute("title", "bash · npm test · 完成");
    // The wire name and the argument are the disclosure's alone; the visible
    // row stays one line.
    expect(row).not.toHaveTextContent("npm test");
    // A settled call says so by not being coloured; no marker restates it.
    expect(row.querySelector(".execution-status")).toBeNull();

    await user.click(toggle);
    expect(within(row).getByText("ok").tagName).toBe("PRE");
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
      <TimelineBlock
        entries={[entry(write, 0), entry(editCall, 1)]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const writeRow = getRow(container, write.id);
    const editRow = getRow(container, editCall.id);
    const writeToggle = getRowToggle(writeRow);
    const editToggle = getRowToggle(editRow);

    expect(rowText(writeRow, "name")).toBe("已创建：new.ts");
    expect(rowText(editRow, "name")).toBe("已编辑：existing.ts");
    // The lines each call changed, as the host diffed them, beside the name.
    expect(writeRow.querySelector(".timeline-row__figures")).toHaveTextContent("+1−0");
    expect(editRow.querySelector(".timeline-row__figures")).toHaveTextContent("+1−1");
    expect(writeToggle).toHaveAttribute("aria-label", "已创建：new.ts · write · new.ts · 完成");
    expect(editToggle).toHaveAttribute("aria-label", "已编辑：existing.ts · edit · existing.ts · 完成");
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

    await user.click(container.querySelector<HTMLButtonElement>(".timeline-block__toggle")!);
    await waitFor(() => {
      expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).not.toBeInTheDocument();
    });
    await user.click(container.querySelector<HTMLButtonElement>(".timeline-block__toggle")!);
    expect(editToggle).toHaveAttribute("aria-expanded", "true");
    expect(editRow.querySelector('[data-tool-name="edit"][data-tool-family="diff"]')).toBeInTheDocument();
  });

  it("keeps a row closed when an existing call transitions to failure", async () => {
    const user = userEvent.setup();
    const succeeded = tool("read-transition", "read", {
      round: 3,
      input: { path: "broken.ts" },
      output: "old content"
    });
    const { container, rerender } = render(
      <TimelineBlock entries={[entry(succeeded, 4)]} tools={[]} insertionIndex={null} />
    );
    const row = getRow(container, succeeded.id);
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "false");
    expect(row).toHaveClass("timeline-row--success");

    const failed: ToolContext = {
      ...succeeded,
      result: {
        ...succeeded.result,
        success: false,
        output: "无法读取 broken.ts"
      }
    };
    rerender(
      <TimelineBlock entries={[entry(failed, 4)]} tools={[]} insertionIndex={null} />
    );

    // The red line says it failed; the body waits for the reader to open it.
    expect(row).toHaveClass("timeline-row--error");
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "false");
    expect(within(row).queryByRole("alert")).not.toBeInTheDocument();

    await user.click(getRowToggle(row));
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "true");
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
      <TimelineBlock
        entries={[entry(announced, 5)]}
        tools={[]}
        insertionIndex={null}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />
    );
    const row = getRow(container, announced.id);
    const toggle = getRowToggle(row);

    expect(row).toHaveClass("timeline-row--announced");
    expect(row).toHaveAttribute("aria-busy", "true");
    // No marker of its own: the tone class and the accessible name carry it.
    expect(row.querySelector(".execution-status")).toBeNull();
    expect(toggle).toHaveAttribute("aria-label", "正在读取文件 · read · pending.ts · 准备参数");
    expect(toggle).toHaveAttribute("aria-disabled", "true");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).not.toHaveAttribute("aria-controls");
    expect(row.querySelector(".timeline-row__details")).not.toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: /编辑工具调用/ })).not.toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: /删除工具调用/ })).not.toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(row.querySelector(".timeline-row__details")).not.toBeInTheDocument();
  });

  it("opens the file a row names in a page of its own, without opening the row", async () => {
    const user = userEvent.setup();
    const requests: PathOpenRequest[] = [];
    const remove = setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    });
    const read = tool("read-row", "read", { input: { path: "src/lib/notes.md", start_line: 12 }, output: "x" });
    const other = tool("other-row", "edit", { input: { path: "b.ts", workspace: 2 }, output: "ok" });
    const { container } = render(
      <TimelineBlock entries={[entry(read, 0), entry(other, 1)]} tools={[]} insertionIndex={null} pathBaseDir="/repo" />
    );
    const row = getRow(container, read.id);
    const link = row.querySelector<HTMLElement>(".timeline-row__file")!;
    expect(link).toHaveTextContent("notes.md");
    expect(link).toHaveAttribute("title", "src/lib/notes.md");
    await user.click(link);
    expect(requests).toEqual([expect.objectContaining({
      path: "src/lib/notes.md",
      baseDir: "/repo",
      line: 12,
      workspace: null,
      newPage: true
    })]);
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "false");
    // Written against another workspace, the path is not this surface's to resolve.
    await user.click(getRow(container, other.id).querySelector<HTMLElement>(".timeline-row__file")!);
    expect(requests[1]).toMatchObject({ path: "b.ts", baseDir: null, workspace: 2, newPage: true });
    // The rest of the line still opens the row.
    await user.click(getRowToggle(row));
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "true");
    remove();
  });

  it("titles a failed call with its error until the local model says why", () => {
    resetToolExplanationsForTests();
    const failed = tool("failed-row", "bash", {
      input: { command: "pnpm i" },
      success: false,
      output: "Exit code 127\nbash: pnpm: command not found"
    });
    const { container } = render(<TimelineBlock entries={[entry(failed, 0)]} tools={[]} insertionIndex={null} />);
    const row = getRow(container, failed.id);
    expect(rowText(row, "name")).toBe("Bash 命令失败：bash: pnpm: command not found");
    act(() => recordToolErrorExplanationForTests(failed.id, "没有安装 pnpm"));
    expect(rowText(row, "name")).toBe("Bash 命令失败：没有安装 pnpm");
    expect(row).toHaveClass("timeline-row--error");
    resetToolExplanationsForTests();
  });

  it("edits, deletes, marks insertion, and reports original context indices", async () => {
    const user = userEvent.setup();
    const first = tool("first-row", "read", { round: 5, input: { path: "first.ts" } });
    const second = tool("second-row", "find", { round: 5, input: { query: "*.tsx" } });
    const onEdit = vi.fn();
    const onDelete = vi.fn();
    const onOpenInsert = vi.fn();
    const { container } = render(
      <TimelineBlock
        entries={[entry(first, 4), entry(second, 7)]}
        tools={[]}
        insertionIndex={7}
        onEdit={onEdit}
        onDelete={onDelete}
        onOpenInsert={onOpenInsert}
        // A tool row shows its pencil only where the edit can be committed;
        // a template's timeline passes neither and gets delete alone.
        onSaveTool={vi.fn()}
        onSaveToolEdit={vi.fn()}
      />
    );
    const firstRow = getRow(container, first.id);
    const secondRow = getRow(container, second.id);
    const insertionLine = container.querySelector<HTMLElement>(".timeline-block__insertion");
    expect(container.querySelectorAll(".timeline-block__insertion")).toHaveLength(1);
    expect(secondRow.previousElementSibling).toBe(insertionLine);

    await user.click(within(firstRow).getByRole("button", { name: "编辑工具调用 已读取：first.ts" }));
    await user.click(within(secondRow).getByRole("button", { name: "删除工具调用 已查找：*.tsx" }));
    // Both callbacks now take the whole context item, because a row may hold
    // reasoning or a hook record rather than a tool call.
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

    const block = container.querySelector<HTMLElement>(".timeline-block")!;
    vi.spyOn(block, "getBoundingClientRect").mockReturnValue({
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
    fireEvent.contextMenu(block, { clientY: 25 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(4);
    fireEvent.contextMenu(block, { clientY: 175 });
    expect(onOpenInsert.mock.calls.at(-1)?.[1]).toBe(8);
  });

  it("opens the in-place tool editor in the row's own body and keeps the list reachable", async () => {
    const user = userEvent.setup();
    const read = tool("edited-row", "read", { round: 5, input: { path: "a.ts" }, output: "旧的返回值" });
    const onCancelEdit = vi.fn();
    const onSaveToolEdit = vi.fn().mockResolvedValue(undefined);
    const { container } = render(
      <TimelineBlock
        entries={[entry(read, 0)]}
        tools={[]}
        insertionIndex={null}
        editingContextId={read.id}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onCancelEdit={onCancelEdit}
        onSaveTool={vi.fn().mockResolvedValue(undefined)}
        onSaveToolEdit={onSaveToolEdit}
      />
    );
    const row = getRow(container, read.id);
    const editor = row.querySelector<HTMLElement>('.inline-tool-editor[data-tool-name="read"]');

    expect(editor).toBeInTheDocument();
    expect(getRowToggle(row)).toHaveAttribute("aria-expanded", "true");
    // The editor owns the body, so the row's own edit/delete controls step aside.
    expect(within(row).queryByRole("button", { name: /编辑工具调用/ })).not.toBeInTheDocument();

    // A collapsed list is `hidden`, which would make the open editor unreachable,
    // so the block refuses to close over it.
    const blockToggle = container.querySelector<HTMLButtonElement>(".timeline-block__toggle")!;
    await user.click(blockToggle);
    expect(blockToggle).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".timeline-block__list")).not.toHaveAttribute("hidden");

    await user.clear(within(editor!).getByLabelText("path"));
    await user.type(within(editor!).getByLabelText("path"), "b.ts");
    await user.click(within(editor!).getByRole("button", { name: "保存" }));
    expect(onSaveToolEdit).toHaveBeenCalledWith({ path: "b.ts" }, "旧的返回值", []);
  });

  it("offers no edit on a preview row, which is one merged tool with no single call to rewrite", () => {
    const snapshot = tool("preview-row", "preview_snapshot", { round: 5, input: {}, output: "- document" });
    const { container } = render(
      <TimelineBlock
        entries={[entry(snapshot, 0)]}
        tools={[]}
        insertionIndex={null}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />
    );
    const row = getRow(container, snapshot.id);

    expect(within(row).queryByRole("button", { name: /编辑工具调用/ })).not.toBeInTheDocument();
    expect(within(row).getByRole("button", { name: /删除工具调用/ })).toBeInTheDocument();
  });

  it("uses the raw detail fallback and descriptor label for an unknown tool", async () => {    const user = userEvent.setup();
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
      <TimelineBlock
        entries={[entry(unknown, 9)]}
        tools={[descriptor]}
        insertionIndex={null}
      />
    );
    const row = getRow(container, unknown.id);
    // The row says what happened in the descriptor's own words; the wire name
    // is what the disclosure keeps.
    expect(rowText(row, "name")).toBe("使用了 神秘工具");
    expect(getRowToggle(row)).toHaveAttribute("aria-label", "使用了 神秘工具 · mystery_tool · 完成");

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
      <TimelineBlock entries={[entry(spawn, 0), entry(stopped, 1)]} tools={[]} insertionIndex={null} />
    );

    expect(getRow(container, spawn.id)).toHaveClass("timeline-row--running");
    expect(getRow(container, spawn.id)).toHaveAttribute("aria-busy", "true");
    const stoppedRow = getRow(container, stopped.id);
    expect(stoppedRow).toHaveClass("timeline-row--halted");
    expect(stoppedRow).not.toHaveClass("timeline-row--error");
    expect(getRowToggle(stoppedRow).getAttribute("aria-label")).toContain("已停止");
  });

  it("sends an agent-run row to the child's panel instead of opening a drawer", async () => {
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
      <TimelineBlock entries={entries} tools={[]} insertionIndex={null} onOpenSubagent={onOpenSubagent} />
    );
    // The child is read in its panel, so the row carries no disclosure at all.
    const spawnRow = getRow(routed.container, spawn.id);
    expect(spawnRow.querySelector(".timeline-row__chevron")).toBeNull();
    expect(spawnRow.querySelector(".timeline-row__details")).toBeNull();
    await user.click(getRowToggle(spawnRow));
    expect(onOpenSubagent).toHaveBeenCalledWith("reviewer");

    // Every other row still opens its own body under the line.
    for (const id of [note.id, read.id]) {
      expect(getRow(routed.container, id).querySelector(".timeline-row__chevron")).not.toBeNull();
    }
    routed.unmount();

    // The read-only subagent terminal renders the same rows and owns no panel,
    // so the agent-run row falls back to its receipt rather than dangling.
    const ambient = render(<TimelineBlock entries={entries} tools={[]} insertionIndex={null} readOnly />);
    const ambientRow = getRow(ambient.container, spawn.id);
    expect(ambientRow.querySelector(".timeline-row__chevron")).not.toBeNull();
    await user.click(getRowToggle(ambientRow));
    expect(ambientRow.querySelector(".timeline-row__details")).not.toBeNull();
  });

  it("summarizes a mixed block by what it actually contains", () => {
    const { container } = render(
      <TimelineBlock
        entries={[
          entry(tool("s-read", "read", { round: 2, input: { path: "src/a.ts" } }), 0),
          entry(tool("s-spawn", "agent_spawn", { round: 2, input: { name: "reviewer" } }), 1),
          entry(tool("s-skill", "skill", { round: 2, input: { name: "pdf" } }), 2)
        ]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const toggle = container.querySelector<HTMLElement>(".timeline-block__toggle");
    expect(toggle).toHaveTextContent("读取了 1 个文件");
    expect(toggle).toHaveTextContent("调用了 1 个子代理");
    expect(toggle).toHaveTextContent("读取了 1 个技能");
  });

  it("draws a tool call, a reasoning record and a hook record as three rows of one block", () => {
    const command = tool("mix-bash", "bash", { round: 2, input: { command: "npm test" } });
    const thought = reasoning("mix-think", { content: "先跑一遍测试", tokens: 1_240 });
    const record = hook("mix-hook", { hookName: "format", event: "PostToolUse", contextInjected: true }, "已格式化 2 个文件");
    const { container } = render(
      <TimelineBlock
        entries={[entry(command, 0), reasoningEntry(thought, 1), hookEntry(record, 2)]}
        tools={[]}
        insertionIndex={null}
      />
    );

    const rows = container.querySelectorAll<HTMLElement>(".timeline-row");
    expect(rows).toHaveLength(3);
    expect([...rows].map((row) => row.dataset.rowKind)).toEqual(["tool", "reasoning", "hook"]);
    // A hook is named by the hook, the way a run is named by the run.
    expect([...rows].map((row) => rowText(row, "name"))).toEqual(["运行了 Bash 命令", "think", "format"]);
    expect(rowText(rows[2], "line")).toBeUndefined();
    expect(within(rows[2]).getByText("已加入模型上下文")).toBeInTheDocument();

    // One clause per bucket, in the heading's own order of consequence.
    const toggle = container.querySelector<HTMLElement>(".timeline-block__toggle");
    expect(toggle).toHaveTextContent("运行了 1 个命令，触发了 1 个 hook，思考了 1 次");
  });

  it("shows the newest reasoning line while it streams and the first line once it settles", () => {
    const content = "先定位失败用例\n再看调用栈\n最后改断言";
    const live = render(
      <TimelineBlock
        entries={[reasoningEntry(reasoning("live-think", { content, streaming: true, tokens: 1_240 }), 0)]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const liveRow = getRow(live.container, "live-think");
    // While the round is thinking the row reads as the thought moving forward.
    expect(rowText(liveRow, "line")).toBe("最后改断言");
    expect(rowText(liveRow, "stat")).toBe("1.2k tokens");
    expect(getRowToggle(liveRow)).toHaveAttribute("aria-label", "think · 正在思考");
    expect(liveRow).toHaveClass("timeline-row--running");
    live.unmount();

    const settled = render(
      <TimelineBlock
        entries={[reasoningEntry(reasoning("settled-think", { content }), 0)]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const settledRow = getRow(settled.container, "settled-think");
    // Settled, the first line is what names what the round set out to do.
    expect(rowText(settledRow, "line")).toBe("先定位失败用例");
    expect(getRowToggle(settledRow)).toHaveAttribute("aria-label", "think · 思考过程");
  });

  it("sweeps in what each commit adds to a streaming reasoning line, and nothing once it settles", () => {
    const block = (content: string, streaming = true) => (
      <TimelineBlock
        entries={[reasoningEntry(reasoning("sweep-think", { content, streaming }), 0)]}
        tools={[]}
        insertionIndex={null}
      />
    );
    const fresh = (row: HTMLElement) => Array.from(row.querySelectorAll(".timeline-row__line .stream-fresh"))
      .map((span) => span.textContent);
    const { container, rerender } = render(block("先定位"));
    const row = getRow(container, "sweep-think");
    expect(fresh(row)).toEqual(["先定位"]);

    // A continuation: only the continuation moves.
    rerender(block("先定位失败用例"));
    expect(rowText(row, "line")).toBe("先定位失败用例");
    expect(fresh(row)).toEqual(["失败用例"]);

    // A new line is new from its first character.
    rerender(block("先定位失败用例\n再看调用栈"));
    expect(fresh(row)).toEqual(["再看调用栈"]);

    rerender(block("先定位失败用例\n再看调用栈", false));
    expect(rowText(row, "line")).toBe("先定位失败用例");
    expect(fresh(row)).toEqual([]);
  });

  it("gives an encrypted reasoning row its token count and refuses to edit it", () => {
    // The body never reached the client, so anything typed here would be
    // fabricated history the next round is asked to believe. The token figure
    // is the entire visible trace of the thought.
    const encrypted = reasoning("encrypted-think", { form: "encrypted", tokens: 1_240, durationMs: 18_000 });
    const { container } = render(
      <TimelineBlock
        entries={[reasoningEntry(encrypted, 0)]}
        tools={[]}
        insertionIndex={null}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />
    );

    const row = getRow(container, encrypted.id);
    expect(rowText(row, "name")).toBe("think");
    expect(rowText(row, "line")).toBe("1.2k tokens");
    expect(rowText(row, "stat")).toBeUndefined();
    expect(getRowToggle(row)).toHaveAttribute("aria-label", "think · 加密思考");
    // Nothing to open: an empty body would be a name with a blank under it.
    expect(getRowToggle(row)).toHaveAttribute("aria-disabled", "true");
    expect(row.querySelector(".timeline-row__details")).not.toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: "编辑上下文" })).not.toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "删除上下文" })).toBeInTheDocument();
  });

  it("sends a workflow row to the run's panel rather than opening its receipt", async () => {
    const user = userEvent.setup();
    const onOpenWorkflowRun = vi.fn();
    const call = tool("wf-call", "workflow", {
      round: 3,
      input: { scriptName: "重构" },
      output: "",
      streaming: true,
      streamStatus: "running"
    });
    const view = workflowView();
    const { container } = render(
      <TimelineBlock
        entries={[workflowEntry(call, 0)]}
        tools={[]}
        insertionIndex={null}
        workflowRunByCall={{ "wf-call": view }}
        onOpenWorkflowRun={onOpenWorkflowRun}
      />
    );

    const row = getRow(container, call.id);
    expect(row).toHaveAttribute("data-row-kind", "workflow");
    // The run's own name, because a block can hold several runs that would
    // otherwise all read "正在运行工作流".
    expect(rowText(row, "name")).toBe("重构");
    expect(rowText(row, "line")).toBeUndefined();
    expect(rowText(row, "stat")).toBe("2 个代理 · 8 token");
    // A run is read in the task panel, so the row neither draws a disclosure
    // nor keeps a body to draw into.
    expect(row.querySelector(".timeline-row__chevron")).toBeNull();
    expect(row.querySelector(".timeline-row__details")).toBeNull();

    await user.click(getRowToggle(row));
    expect(container.querySelector(".workflow-run")).toBeNull();
    expect(onOpenWorkflowRun).toHaveBeenCalledWith("run-1");

    // The heading counts the run in the subagent bucket rather than inventing
    // one of its own.
    expect(container.querySelector(".timeline-block__toggle")).toHaveTextContent("调用了 1 个子代理");
  });

  it("falls back to the workflow receipt when the run has no view left to show", async () => {
    const user = userEvent.setup();
    const reloaded = tool("wf-reloaded", "workflow", { round: 3, output: "workflow:runabc" });
    const { container } = render(
      <TimelineBlock entries={[workflowEntry(reloaded, 0)]} tools={[]} insertionIndex={null} />
    );

    const row = getRow(container, reloaded.id);
    expect(rowText(row, "name")).toBe("完成了工作流");
    expect(getRowToggle(row)).toHaveAttribute("aria-label", "完成了工作流 · workflow · 完成");
    await user.click(getRowToggle(row));
    expect(container.querySelector(".workflow-run")).toBeNull();
    expect(container.textContent).toContain("workflow:runabc");
  });

  it("names an MCP row by its own tool and counts it in the MCP bucket", () => {
    const call = tool("mcp-call", MCP_TOOL, { round: 4, input: { query: "订单退款" }, durationMs: 41 });

    // No descriptor: an archived conversation whose server has since been
    // removed still yields readable slugs from the wire name.
    const bare = render(
      <TimelineBlock entries={[entry(call, 0)]} tools={[]} insertionIndex={null} />
    );
    const bareRow = getRow(bare.container, call.id);
    expect(rowText(bareRow, "name")).toBe("调用了 MCP 工具 search");
    expect(rowText(bareRow, "line")).toBeUndefined();
    expect(getRowToggle(bareRow)).toHaveAttribute("aria-label", "调用了 MCP 工具 search · search · 订单退款 · 完成");
    expect(bare.container.querySelector(".timeline-block__toggle")).toHaveTextContent("调用了 1 个 MCP 工具");
    bare.unmount();

    // With one, the label carries the server and tool in the words the user chose.
    const descriptor: ToolDescriptor = {
      name: MCP_TOOL,
      label: "MCP Acme / Search",
      description: "",
      category: "orchestration",
      dangerous: false,
      parameters: []
    };
    const labelled = render(
      <TimelineBlock entries={[entry(call, 0)]} tools={[descriptor]} insertionIndex={null} />
    );
    const labelledRow = getRow(labelled.container, call.id);
    expect(rowText(labelledRow, "name")).toBe("调用了 MCP 工具 Search");
    expect(getRowToggle(labelledRow)).toHaveAttribute("aria-label", "调用了 MCP 工具 Search · Search · 订单退款 · 完成");
    expect(labelled.container.querySelector(".timeline-block__toggle")).toHaveTextContent("调用了 1 个 MCP 工具");
  });
});
