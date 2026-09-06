import { describe, expect, it } from "vitest";
import type { ContextItem, SubagentLiveState, SubagentRunRecord, ToolContext } from "../types";
import {
  agentTimelineRouteId,
  agentTimelineRunStatus,
  approvalPromptSubagentView,
  deriveSubagentViews,
  findExternalStepBodyRef,
  findOpenableSubagentView,
  findSubagentView,
  graftExternalStepBodies,
  hashSubagentId,
  subagentAvatarTone
} from "./subagents";

function result(output = "完成", success = true, executedAt = "2026-07-14T02:00:00Z") {
  return { success, output, executedAt, durationMs: 12 };
}

function subagent(overrides: Partial<ToolContext> = {}): ToolContext {
  return {
    id: "tool-subagent",
    kind: "tool",
    toolName: "subagent",
    input: { task: "审查路由", label: "路由审查" },
    result: result("最终结论"),
    createdAt: "2026-07-14T01:00:00Z",
    ...overrides
  };
}

const live: SubagentLiveState = {
  contexts: [
    {
      id: "live-read",
      kind: "tool",
      toolName: "read",
      input: { path: "README.md" },
      result: result("README content"),
      streaming: true,
      streamStatus: "completed",
      createdAt: "2026-07-14T01:20:00Z"
    },
    {
      id: "live-answer",
      kind: "assistant",
      content: "正在整理结果",
      streaming: true,
      createdAt: "2026-07-14T01:31:00Z"
    }
  ],
  updates: [{ content: "已经完成目录扫描", createdAt: "2026-07-14T01:30:00Z" }]
};

describe("deriveSubagentViews", () => {
  it("preserves a failed workflow step's published role after serialization without a worker record", () => {
    const streamed = subagent({
      id: "workflow-step-failed",
      toolName: "workflow_step",
      input: { label: "review", task: "review the host", role: "reviewer", roleModelId: "sonnet-5" },
      streaming: true
    });
    const settled = {
      ...streamed,
      streaming: false,
      result: result("registration capacity exhausted", false)
    };
    for (const card of [streamed, settled, JSON.parse(JSON.stringify(settled))]) {
      const view = deriveSubagentViews([card]).find((view) => view.id === streamed.id);
      expect(view).toMatchObject({ label: "review", role: { name: "reviewer", modelId: "sonnet-5" }, modelId: "sonnet-5" });
    }
    const roleless = deriveSubagentViews([{ ...settled, input: { label: "review", task: "review the host" } }]);
    expect(roleless).toEqual([]);
  });

  it("prefers the persisted transcript, pins its task first, and projects persisted updates as tools", () => {
    const assistant: ContextItem = {
      id: "child-answer",
      kind: "assistant",
      content: "路由由 router.ts 统一注册",
      createdAt: "2026-07-14T01:50:00Z"
    };
    const views = deriveSubagentViews([subagent({
      subagent: {
        task: "审查路由",
        status: "completed",
        contexts: [assistant],
        updates: [{ content: "定位到入口", createdAt: "2026-07-14T01:20:00Z" }]
      }
    })]);

    expect(views).toHaveLength(1);
    expect(views[0]).toMatchObject({
      id: "tool-subagent",
      label: "路由审查",
      task: "审查路由",
      status: "completed",
      summary: "路由由 router.ts 统一注册",
      completedAt: "2026-07-14T02:00:00Z"
    });
    expect(views[0].contexts.map((context) => context.kind)).toEqual(["user", "tool", "assistant"]);
    expect(views[0].contexts[0]).toMatchObject({ kind: "user", content: "审查路由" });
    expect(views[0].contexts[1]).toMatchObject({
      kind: "tool",
      toolName: "subagent_update",
      input: { message: "定位到入口" },
      result: {
        success: true,
        output: "状态已返回给主智能体",
        executedAt: "2026-07-14T01:20:00Z"
      }
    });
    expect(views[0].contexts[2]).toBe(assistant);
    expect(views[0].updates).toEqual([{ content: "定位到入口", createdAt: "2026-07-14T01:20:00Z" }]);
  });

  it("does not duplicate a task already present at the start of a persisted transcript", () => {
    const task: ContextItem = {
      id: "child-task",
      kind: "user",
      content: "审查路由",
      createdAt: "2026-07-14T01:00:00Z"
    };
    const [view] = deriveSubagentViews([subagent({
      subagent: { task: "审查路由", status: "completed", contexts: [task], updates: [] }
    })]);
    expect(view.contexts).toEqual([task]);
  });

  it("moves a matching legacy task to the front instead of synthesizing a duplicate", () => {
    const answer: ContextItem = {
      id: "legacy-answer",
      kind: "assistant",
      content: "先于任务落盘的旧记录",
      createdAt: "2026-07-14T01:01:00Z"
    };
    const task: ContextItem = {
      id: "legacy-task",
      kind: "user",
      content: "审查路由",
      createdAt: "2026-07-14T01:02:00Z"
    };
    const [view] = deriveSubagentViews([subagent({
      subagent: { task: "审查路由", status: "completed", contexts: [answer, task], updates: [] }
    })]);

    expect(view.contexts).toEqual([task, answer]);
    expect(view.contexts.filter((context) => context.kind === "user" && context.content === "审查路由")).toHaveLength(1);
  });

  it("builds a task-first live transcript with structured nested tools", () => {
    const [view] = deriveSubagentViews([subagent({
      id: "stream-tool-run-call-live",
      input: { task: "核对测试" },
      result: result("", true),
      streaming: true,
      streamStatus: "running",
      live
    })]);

    expect(view).toMatchObject({
      id: "stream-tool-run-call-live",
      label: "子代理 1",
      status: "running",
      summary: "已经完成目录扫描",
      completedAt: null,
      live
    });
    expect(view.contexts.map((context) => context.kind)).toEqual(["user", "tool", "tool", "assistant"]);
    expect(view.contexts[0]).toMatchObject({ kind: "user", content: "核对测试" });
    expect(view.contexts[1]).toMatchObject({
      kind: "tool",
      toolName: "read",
      input: { path: "README.md" },
      result: { success: true, output: "README content" }
    });
    expect(view.contexts[2]).toMatchObject({
      kind: "tool",
      toolName: "subagent_update",
      input: { message: "已经完成目录扫描" },
      result: { success: true, output: "状态已返回给主智能体" }
    });
    expect(view.contexts[3]).toMatchObject({
      kind: "assistant",
      content: "正在整理结果",
      streaming: true
    });
  });

  it("keeps running agents first and then sorts terminal records newest first", () => {
    const contexts: ContextItem[] = [
      subagent({ id: "old", input: { task: "old" }, result: result("old", true, "2026-07-12T00:00:00Z") }),
      subagent({
        id: "running",
        input: { task: "running" },
        streaming: true,
        streamStatus: "running",
        live: { ...live, updates: [] }
      }),
      subagent({
        id: "interrupted",
        input: { task: "interrupted" },
        result: result("stopped", false, "2026-07-14T00:00:00Z"),
        subagent: { task: "interrupted", status: "interrupted", contexts: [], updates: [] }
      })
    ];

    expect(deriveSubagentViews(contexts).map((view) => [view.id, view.status])).toEqual([
      ["running", "running"],
      ["interrupted", "interrupted"],
      ["old", "completed"]
    ]);
  });

  it("uses the stable local context id across the streaming-to-persisted replacement", () => {
    const streaming = deriveSubagentViews([subagent({
      id: "stream-tool-run-call-stable",
      streaming: true,
      streamStatus: "running",
      live
    })])[0];
    const persisted = deriveSubagentViews([subagent({
      id: "stream-tool-run-call-stable",
      subagent: { task: "审查路由", status: "completed", contexts: [], updates: [] }
    })])[0];

    expect(streaming.id).toBe("stream-tool-run-call-stable");
    expect(persisted.id).toBe(streaming.id);
  });

  it("assigns deterministic avatar tones", () => {
    expect(hashSubagentId("agent-a")).toBe(hashSubagentId("agent-a"));
    expect(subagentAvatarTone("agent-a")).toBe(subagentAvatarTone("agent-a"));
  });

  it("localizes generated labels, transcript placeholders, summaries, and supplemental updates", () => {
    const messages = {
      fallbackLabel: (index: number) => `Subagent ${index}`,
      workflowStepLabel: "Workflow step",
      missingTask: "No task description provided",
      updateReturned: "Status returned to the primary agent",
      running: "Working",
      interrupted: "Subagent interrupted",
      failed: "Subagent failed",
      stopped: "Subagent stopped",
      roundLimit: "Subagent hit its round limit",
      completed: "Subagent completed"
    };
    const [running] = deriveSubagentViews([subagent({
      input: {},
      result: result("", true),
      streaming: true,
      streamStatus: "running",
      live: { contexts: [], updates: [], status: "running" }
    })], messages);
    expect(running).toMatchObject({
      label: "Subagent 1",
      task: "",
      summary: "Working"
    });
    expect(running.contexts[0]).toMatchObject({
      kind: "user",
      content: "No task description provided"
    });

    const [completed] = deriveSubagentViews([subagent({
      input: {},
      result: result("", true),
      subagent: {
        task: "",
        status: "completed",
        contexts: [],
        updates: [{ content: "Progress", createdAt: "2026-07-14T01:30:00Z" }]
      }
    })], messages);
    expect(completed.contexts.find((context) => context.kind === "tool")).toMatchObject({
      result: { output: "Status returned to the primary agent" }
    });

    const [interrupted] = deriveSubagentViews([subagent({
      input: {},
      result: result("", false),
      subagent: { task: "", status: "interrupted", contexts: [], updates: [] }
    })], messages);
    expect(interrupted.summary).toBe("Subagent interrupted");

    const [done] = deriveSubagentViews([subagent({
      input: {},
      result: result("", true),
      subagent: { task: "", status: "completed", contexts: [], updates: [] }
    })], messages);
    expect(done.summary).toBe("Subagent completed");
  });

  it("reports the record's own token total and counts only this agent's tool calls", () => {
    const childTool: ContextItem = {
      id: "child-read",
      kind: "tool",
      toolName: "read",
      input: { path: "src/router.ts" },
      result: result("router source"),
      createdAt: "2026-07-14T01:10:00Z"
    };
    const nestedChild: ContextItem = {
      id: "child-spawn",
      kind: "tool",
      toolName: "agent_spawn",
      input: { task: "深挖", name: "deep" },
      result: result("已派生"),
      createdAt: "2026-07-14T01:20:00Z",
      subagent: {
        name: "deep",
        task: "深挖",
        status: "completed",
        contexts: [
          {
            id: "deep-read",
            kind: "tool",
            toolName: "read",
            input: { path: "src/api.ts" },
            result: result("api source"),
            createdAt: "2026-07-14T01:21:00Z"
          }
        ],
        updates: [],
        usage: { inputTokens: 10, outputTokens: 5, totalTokens: 15 }
      }
    };
    const [view] = deriveSubagentViews([subagent({
      subagent: {
        task: "审查路由",
        status: "completed",
        contexts: [childTool, nestedChild],
        updates: [],
        usage: { inputTokens: 900, cachedInputTokens: 100, outputTokens: 340, totalTokens: 1240 }
      }
    })]);

    expect(view.usage).toEqual({
      inputTokens: 900,
      cachedInputTokens: 100,
      outputTokens: 340,
      totalTokens: 1240
    });
    // The synthesized task context is a user turn, `read` and `agent_spawn` are
    // this agent's own two calls, and the nested child's `read` must not be one
    // of them — it hangs off the spawn record, not beside it.
    expect(view.toolCount).toBe(2);
    expect(view.childIds).toEqual(["deep"]);
  });

  it("leaves the metrics empty for a record that never reported usage", () => {
    const [view] = deriveSubagentViews([subagent({
      subagent: { task: "审查路由", status: "completed", contexts: [], updates: [] }
    })]);
    expect(view.usage).toEqual({});
    expect(view.toolCount).toBe(0);
  });
});

describe("background agent views", () => {
  const spawnContext = (overrides: Partial<ToolContext> = {}): ToolContext => ({
    id: "tool-spawn",
    kind: "tool",
    toolName: "agent_spawn",
    input: { task: "审查 API", name: "helper", label: "审查" },
    result: result("ok"),
    createdAt: "2026-07-14T01:00:00Z",
    ...overrides
  });

  it("merges agent_spawn and agent_send continuations into one view keyed by name", () => {
    const spawn = spawnContext({
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "t1-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "t1-answer", kind: "assistant", content: "初版结论", createdAt: "2026-07-14T01:01:00Z" }
        ],
        updates: []
      }
    });
    const send: ToolContext = {
      id: "tool-send",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "补充风险" },
      result: result("子代理 helper 已被唤醒并继续运行（保留其全部历史上下文）。"),
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "t1-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "t1-answer", kind: "assistant", content: "初版结论", createdAt: "2026-07-14T01:01:00Z" },
          { id: "t2-message", kind: "user", content: "补充风险", createdAt: "2026-07-14T02:00:00Z" },
          { id: "t2-answer", kind: "assistant", content: "修订后的结论", createdAt: "2026-07-14T02:01:00Z" }
        ],
        updates: [{ content: "正在修订", createdAt: "2026-07-14T02:00:30Z" }]
      },
      createdAt: "2026-07-14T02:00:00Z"
    };

    const views = deriveSubagentViews([spawn, send]);
    expect(views).toHaveLength(1);
    expect(views[0]).toMatchObject({
      id: "helper",
      name: "helper",
      label: "审查",
      task: "审查 API",
      status: "completed"
    });
    expect(views[0].callIds).toEqual(["tool-spawn", "tool-send"]);
    // The latest record wins: the drawer shows the full cumulative transcript.
    expect(views[0].contexts.some((context) => context.kind === "assistant" && context.content === "修订后的结论")).toBe(true);
    expect(views[0].updates).toEqual([{ content: "正在修订", createdAt: "2026-07-14T02:00:30Z" }]);
  });

  /**
   * A role name is available before the host creates a record.
   *
   * The persisted record is authoritative but arrives after `agent_spawn`
   * completes. The requested role therefore covers the streaming interval, and
   * the resolved record binding supersedes it when available.
   */
  it("names the role a spawn asked for until its record supersedes it", () => {
    const streaming = spawnContext({
      input: { task: "审查 API", name: "helper", agent_type: "reviewer" },
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "running" }
    });
    const [streamingView] = deriveSubagentViews([streaming]);

    expect(streamingView.role).toEqual({ name: "reviewer", modelId: null });
    // Do not invent a model when the call provides only a role name.
    expect(streamingView.modelId).toBeNull();

    // A settled record's normalized binding, including its exact model, is authoritative.
    const settled = spawnContext({
      input: { task: "审查 API", name: "helper", agent_type: "reviewer" },
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [],
        updates: [],
        agentDefinition: {
          name: "code-reviewer",
          modelId: "sonnet-5"
        } as SubagentRunRecord["agentDefinition"]
      }
    });
    const [settledView] = deriveSubagentViews([settled]);

    expect(settledView.role).toEqual({ name: "code-reviewer", modelId: "sonnet-5" });
    expect(settledView.modelId).toBe("sonnet-5");
  });

  it("keeps queue-only send_message out of the child transcript until it is drained", () => {
    const spawn = spawnContext({
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "queue-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "queue-answer", kind: "assistant", content: "初版结论", createdAt: "2026-07-14T01:01:00Z" }
        ],
        updates: []
      }
    });
    const queued: ToolContext = {
      id: "tool-queue-only",
      kind: "tool",
      toolName: "send_message",
      input: { target: "helper", message: "暂存这条要求" },
      result: result(""),
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "queue-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "queue-answer", kind: "assistant", content: "初版结论", createdAt: "2026-07-14T01:01:00Z" }
        ],
        updates: [],
        queuedMessages: [{ content: "暂存这条要求", triggerTurn: false }]
      },
      createdAt: "2026-07-14T02:00:00Z"
    };

    const [view] = deriveSubagentViews([spawn, queued]);
    expect(view.name).toBe("helper");
    expect(view.callIds).toEqual(["tool-spawn", "tool-queue-only"]);
    expect(view.contexts.some(
      (context) => context.kind === "user" && context.content === "暂存这条要求"
    )).toBe(false);
  });

  it("keeps the persisted conversation visible while an agent_send continuation streams", () => {
    const spawn = spawnContext({
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "old-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "old-answer", kind: "assistant", content: "上一轮结论", createdAt: "2026-07-14T01:01:00Z" }
        ],
        updates: [{ content: "上一轮已完成", createdAt: "2026-07-14T01:00:30Z" }]
      }
    });
    const send: ToolContext = {
      id: "tool-send-live",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "补充检查并发边界" },
      result: result("消息已发送给子代理 helper。", true, "2026-07-14T02:00:01Z"),
      streaming: true,
      streamStatus: "completed",
      live: {
        contexts: [
          {
            id: "new-reasoning",
            kind: "reasoning",
            content: "检查并发交接",
            createdAt: "2026-07-14T02:00:10Z"
          },
          {
            id: "new-answer",
            kind: "assistant",
            content: "正在补充并发结论",
            streaming: true,
            createdAt: "2026-07-14T02:00:20Z"
          }
        ],
        updates: [{ content: "正在检查并发", createdAt: "2026-07-14T02:00:15Z" }],
        status: "running"
      },
      createdAt: "2026-07-14T02:00:00Z"
    };

    const [view] = deriveSubagentViews([spawn, send]);
    expect(view.status).toBe("running");
    expect(view.contexts.flatMap((context) => context.kind === "user" ? [context.content] : [])).toEqual([
      "审查 API",
      "补充检查并发边界"
    ]);
    expect(view.contexts.some((context) => context.id === "old-answer" && context.kind === "assistant")).toBe(true);
    expect(view.contexts.some((context) => context.id === "new-answer" && context.kind === "assistant")).toBe(true);
    expect(view.updates.map((update) => update.content)).toEqual(["上一轮已完成", "正在检查并发"]);
  });

  it("orders a queued message inside the live child conversation even when events stay on the spawn call", () => {
    const spawn = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: {
        contexts: [
          {
            id: "before-send",
            kind: "assistant",
            content: "第一轮结论",
            createdAt: "2026-07-14T01:04:00Z"
          },
          {
            id: "after-send",
            kind: "assistant",
            content: "已处理追加指令",
            streaming: true,
            createdAt: "2026-07-14T01:06:00Z"
          }
        ],
        updates: [],
        status: "running"
      }
    });
    const send: ToolContext = {
      id: "tool-send-queued",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "继续检查取消竞态" },
      result: result("消息已排队。", true, "2026-07-14T01:05:01Z"),
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [] },
      createdAt: "2026-07-14T01:05:00Z"
    };

    const [view] = deriveSubagentViews([spawn, send]);
    expect(view.status).toBe("running");
    expect(view.contexts.flatMap((context) => {
      if (context.kind === "user" || context.kind === "assistant") return [context.content];
      return [];
    })).toEqual([
      "审查 API",
      "第一轮结论",
      "继续检查取消竞态",
      "已处理追加指令"
    ]);
    expect(view.live?.contexts.map((context) => context.id)).toEqual(["before-send", "after-send"]);
  });

  it("does not duplicate an agent_send message when a cumulative record is backfilled onto an older call", () => {
    const spawn = spawnContext({
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "backfill-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:01Z" },
          { id: "backfill-first", kind: "assistant", content: "第一轮完成", createdAt: "2026-07-14T01:04:00Z" },
          { id: "backfill-send", kind: "user", content: "追加验证", createdAt: "2026-07-14T01:05:01Z" },
          { id: "backfill-second", kind: "assistant", content: "追加验证完成", createdAt: "2026-07-14T01:06:00Z" }
        ],
        updates: []
      }
    });
    const send: ToolContext = {
      id: "tool-send-backfilled",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "追加验证" },
      result: result("消息已排队。", true, "2026-07-14T01:05:00Z"),
      createdAt: "2026-07-14T01:05:00Z"
    };

    const [view] = deriveSubagentViews([spawn, send]);
    expect(view.contexts.filter(
      (context) => context.kind === "user" && context.content === "追加验证"
    )).toHaveLength(1);
    expect(view.status).toBe("completed");
  });

  it("keeps a running child running when a call addressed to it fails", () => {
    // A run tool's result reports whether that *call* was accepted, not how the
    // child ended: a message the host refused says nothing about the child that
    // was never handed it. The child's own live status is authoritative, so the
    // rejected call must not repaint a live agent as failed.
    const spawn = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "running" }
    });
    const rejectedSend: ToolContext = {
      id: "tool-send-rejected",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "补充要求" },
      result: result("子代理正忙，消息未送达。", false, "2026-07-14T01:05:00Z"),
      streaming: true,
      streamStatus: "completed",
      createdAt: "2026-07-14T01:05:00Z"
    };
    expect(deriveSubagentViews([spawn, rejectedSend])[0].status).toBe("running");

    // The same rejected call once the child has finished on its own terms.
    const finished = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "idle" }
    });
    expect(deriveSubagentViews([finished, rejectedSend])[0].status).toBe("completed");
  });

  it("does not let a failed tool call inside the child end the child's run", () => {
    // The inner call is an ordinary event the child is expected to handle.
    const failedInnerCall: ContextItem = {
      id: "live-failing-read",
      kind: "tool",
      toolName: "read",
      input: { path: "missing.md" },
      result: result("文件不存在", false, "2026-07-14T01:21:00Z"),
      streaming: true,
      streamStatus: "completed",
      createdAt: "2026-07-14T01:21:00Z"
    };
    const running = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [failedInnerCall], updates: [], status: "running" }
    });
    expect(deriveSubagentViews([running])[0].status).toBe("running");
  });

  it("reports the terminal lifecycle status a child actually reached", () => {
    for (const status of ["failed", "stopped", "roundLimit"] as const) {
      const view = deriveSubagentViews([spawnContext({
        streaming: true,
        streamStatus: "completed",
        live: { contexts: [], updates: [], status }
      })])[0];
      expect(view.status).toBe(status);
    }
  });

  it("keeps the agent running while its call completed but the child still streams", () => {
    const streamed = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "running" }
    });
    expect(deriveSubagentViews([streamed])[0].status).toBe("running");

    const idle = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: {
        contexts: [{ id: "idle-answer", kind: "assistant", content: "结论", createdAt: "2026-07-14T01:01:00Z" }],
        updates: [],
        status: "idle"
      }
    });
    expect(deriveSubagentViews([idle])[0].status).toBe("completed");

    const interrupted = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "interrupted" }
    });
    expect(deriveSubagentViews([interrupted])[0].status).toBe("interrupted");
  });

  it("does not mark an already-running agent complete during an agent_send hand-off", () => {
    const spawn = spawnContext({
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [], status: "running" }
    });
    const send: ToolContext = {
      id: "tool-send-running",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "helper", message: "再检查一遍边界条件" },
      result: result("消息已发送给仍在运行的子代理 helper。"),
      streaming: true,
      streamStatus: "completed",
      live: { contexts: [], updates: [] },
      createdAt: "2026-07-14T01:05:00Z"
    };

    const [view] = deriveSubagentViews([spawn, send]);
    expect(view.status).toBe("running");
    expect(view.callIds).toEqual(["tool-spawn", "tool-send-running"]);
  });

  it("derives a view from a lone continuation and finds it by any call id", () => {
    const send: ToolContext = {
      id: "tool-send-only",
      kind: "tool",
      toolName: "agent_send",
      input: { agent: "a9", message: "继续上次的分析" },
      result: result("子代理 a9 已被唤醒并继续运行（保留其全部历史上下文）。"),
      subagent: {
        name: "a9",
        task: "历史任务",
        status: "interrupted",
        contexts: [
          { id: "old-task", kind: "user", content: "历史任务", createdAt: "2026-07-13T01:00:00Z" },
          { id: "new-message", kind: "user", content: "继续上次的分析", createdAt: "2026-07-14T01:00:00Z" }
        ],
        updates: []
      },
      createdAt: "2026-07-14T01:00:00Z"
    };
    const views = deriveSubagentViews([send]);
    expect(views).toHaveLength(1);
    expect(views[0].id).toBe("a9");
    expect(views[0].task).toBe("历史任务");
    expect(views[0].status).toBe("interrupted");

    expect(findSubagentView(views, "tool-send-only")).toBe(views[0]);
    expect(findSubagentView(views, "a9")).toBe(views[0]);
    expect(findSubagentView(views, "ghost")).toBeNull();
  });

  it("projects workflow steps as specialized read-only child views", () => {
    const step: ToolContext = {
      id: "tool-workflow-step",
      kind: "tool",
      toolName: "workflow_step",
      input: { task: "Inspect the scheduler." },
      result: result("Done"),
      subagent: {
        kind: "workflowStep",
        task: "Inspect the scheduler.",
        status: "completed",
        contexts: [],
        updates: []
      },
      createdAt: "2026-07-29T01:00:00Z"
    };

    const [view] = deriveSubagentViews([step]);
    expect(view).toMatchObject({
      id: "tool-workflow-step",
      name: null,
      kind: "workflowStep",
      label: "工作流步骤",
      task: "Inspect the scheduler.",
      status: "completed"
    });
  });

  it("keeps an externalized workflow step visible and trusts its input status", () => {
    // The host externalized this step's body to the run directory: no nested
    // record, only a preview plus retrieval coordinates and the terminal
    // status in the input. The call itself succeeded, so a view that fell
    // back to the result would misreport the interrupted step as completed.
    const step: ToolContext = {
      id: "tool-external-step",
      kind: "tool",
      toolName: "workflow_step",
      input: {
        task: "Inspect the scheduler.",
        runId: "wf-run-1",
        stepIndex: 0,
        status: "interrupted",
        outputBytes: 620000,
        outputSha256: "ab12"
      },
      result: result("部分结论…（预览截断；完整正文见运行目录逐步记录，抽屉里按需加载）"),
      createdAt: "2026-08-04T01:00:00Z"
    };

    const views = deriveSubagentViews([step]);
    expect(views).toHaveLength(1);
    expect(views[0]).toMatchObject({
      id: "tool-external-step",
      kind: "workflowStep",
      task: "Inspect the scheduler.",
      status: "interrupted"
    });

    // Without coordinates the same context is a childless helper call again
    // and stays hidden — the coordinates alone are what promise a body.
    const bare: ToolContext = {
      ...step,
      input: { task: "Inspect the scheduler.", status: "interrupted" }
    };
    expect(deriveSubagentViews([bare])).toHaveLength(0);
  });

  it("reads an externalized step's tokens off its shell, without loading the body", () => {
    // Usage is a handful of integers, so the host leaves it on the shell next
    // to the terminal status. Requiring the nested record for it meant the
    // tokens column stayed "—" until somebody opened the step and the drawer
    // fetched its transcript over IPC.
    const step: ToolContext = {
      id: "tool-external-step",
      kind: "tool",
      toolName: "workflow_step",
      input: {
        task: "Inspect the scheduler.",
        runId: "wf-run-1",
        stepIndex: 0,
        status: "completed",
        usage: { inputTokens: 8_100, cachedInputTokens: 6_000, outputTokens: 420, totalTokens: 8_520 },
        toolUseCount: 7,
        outputBytes: 620000,
        outputSha256: "ab12"
      },
      result: result("预览"),
      createdAt: "2026-08-04T01:00:00Z"
    };

    const [view] = deriveSubagentViews([step]);
    expect(view.usage).toEqual({
      inputTokens: 8_100,
      cachedInputTokens: 6_000,
      outputTokens: 420,
      totalTokens: 8_520
    });
    // The shell's own contexts are a preview and a fingerprint, so counting them
    // would report zero tools for a step that called seven.
    expect(view.toolCount).toBe(7);

    // A shell the host wrote before these rode along reports nothing rather
    // than zero: "—" and "0" are different claims.
    const { usage: _usage, toolUseCount: _toolUseCount, ...older } = step.input;
    const [legacy] = deriveSubagentViews([{ ...step, input: older }]);
    expect(legacy.usage).toEqual({});
    expect(legacy.toolCount).toBeNull();
  });

  it("reports a running agent's streamed usage before any record exists", () => {
    // The host streams the child's provider snapshots the whole time it runs.
    // Sourcing the figure only from a settled record left every running agent
    // showing "—" for its entire life.
    const [view] = deriveSubagentViews([subagent({
      id: "stream-tool-run-call-live",
      input: { task: "核对测试" },
      result: result("", true),
      streaming: true,
      streamStatus: "running",
      live: {
        ...live,
        // Two rounds of one turn: each snapshot is cumulative for its own
        // round, so the run's total is their sum.
        usageByRound: {
          0: { inputTokens: 1_200, outputTokens: 90 },
          1: { inputTokens: 1_500, outputTokens: 40 }
        }
      }
    })]);

    expect(view.status).toBe("running");
    expect(view.usage).toEqual({ inputTokens: 2_700, outputTokens: 130 });
  });

  it("lets a settled record supersede the live snapshots it replaces", () => {
    // The record is cumulative across every turn the agent ran and is the
    // authoritative figure; adding the live snapshots to it would bill the
    // final turn twice.
    const [view] = deriveSubagentViews([subagent({
      streaming: true,
      streamStatus: "completed",
      live: { ...live, usageByRound: { 0: { inputTokens: 1_200, outputTokens: 90 } } },
      subagent: {
        task: "审查路由",
        status: "completed",
        contexts: [],
        updates: [],
        usage: { inputTokens: 1_200, outputTokens: 90, totalTokens: 1_290 }
      } as SubagentRunRecord
    })]);
    expect(view.usage).toEqual({ inputTokens: 1_200, outputTokens: 90, totalTokens: 1_290 });
  });

  it("grafts a loaded body onto its externalized step and stops advertising coordinates", () => {
    const step: ToolContext = {
      id: "tool-external-step",
      kind: "tool",
      toolName: "workflow_step",
      input: { task: "Inspect the scheduler.", runId: "wf-run-1", stepIndex: 1, status: "completed" },
      result: result("预览"),
      createdAt: "2026-08-04T01:00:00Z"
    };
    const run: ToolContext = {
      id: "tool-workflow",
      kind: "tool",
      toolName: "workflow",
      input: { scriptName: "review-proxy", scriptBytes: 4096, scriptSha256: "0f1e" },
      result: result("{\"status\":\"complete\"}"),
      subagent: {
        kind: "workflowStep",
        task: "Review the proxy.",
        status: "completed",
        contexts: [step],
        updates: []
      },
      createdAt: "2026-08-04T01:00:00Z"
    };
    const contexts: ContextItem[] = [run];

    // The finder digs into nested records — that is where step contexts live.
    expect(findExternalStepBodyRef(contexts, ["tool-external-step"]))
      .toEqual({ runId: "wf-run-1", stepIndex: 1 });
    expect(findExternalStepBodyRef(contexts, ["tool-workflow"])).toBeNull();

    const record: SubagentRunRecord = {
      kind: "workflowStep",
      task: "Inspect the scheduler.",
      status: "completed",
      contexts: [{
        id: "step-answer",
        kind: "assistant",
        content: "完整正文",
        createdAt: "2026-08-04T01:00:01Z"
      }],
      updates: []
    };
    const grafted = graftExternalStepBodies(
      contexts,
      (ref) => ref.runId === "wf-run-1" && ref.stepIndex === 1 ? record : undefined
    );
    const graftedRun = grafted[0] as ToolContext;
    const graftedStep = graftedRun.subagent?.contexts[0] as ToolContext;
    expect(graftedStep.subagent).toBe(record);
    // A grafted step carries a record again, so it yields no coordinates and
    // can never trigger a second fetch.
    expect(findExternalStepBodyRef(grafted, ["tool-external-step"])).toBeNull();
    // The original tree is untouched, and a lookup that answers nothing
    // returns the input array by reference for memoized consumers.
    expect((contexts[0] as ToolContext).subagent?.contexts[0]).toBe(step);
    expect(graftExternalStepBodies(contexts, () => undefined)).toBe(contexts);
  });

  it("names a workflow run after its plan, whose body the public input strips away", () => {
    const run: ToolContext = {
      id: "tool-workflow",
      kind: "tool",
      toolName: "workflow",
      // The backend keeps only a name and a fingerprint: the plan body itself is
      // carried by the step contexts, never copied into the public timeline.
      input: { scriptName: "review-proxy", scriptBytes: 4096, scriptSha256: "0f1e" },
      result: result("{\"status\":\"complete\"}"),
      subagent: {
        kind: "workflowStep",
        task: "Review the proxy.",
        status: "completed",
        contexts: [],
        updates: []
      },
      createdAt: "2026-07-29T01:00:00Z"
    };

    const [view] = deriveSubagentViews([run]);
    expect(view).toMatchObject({ kind: "workflowStep", label: "review-proxy" });
  });

  it("does not restate the task on a card the host stamped after the child's first turn", () => {
    // Production shape, which no fixture had: the host mints an agent card in
    // `tool_context_for_turn` once the *call* has finished, so the card's clock
    // is later than the child's own opening turn. Requiring the persisted turn
    // to be the newer of the two therefore rejected the real match and grew a
    // second copy of the task, stamped with the card's late clock.
    const spawn: ToolContext = {
      id: "tool-spawn-late-card",
      kind: "tool",
      toolName: "agent_spawn",
      input: { name: "helper", prompt: "审查 API" },
      result: result("子代理 helper 已启动", true, "2026-08-26T01:00:05Z"),
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "child-task", kind: "user", content: "审查 API", createdAt: "2026-08-26T01:00:01Z" },
          { id: "child-answer", kind: "assistant", content: "结论", createdAt: "2026-08-26T01:03:00Z" }
        ],
        updates: []
      },
      createdAt: "2026-08-26T01:00:05Z"
    };

    const [view] = deriveSubagentViews([spawn]);
    expect(view.contexts.map((context) => context.id)).toEqual(["child-task", "child-answer"]);
  });

  it("does not restate an externalized step's task once its body is grafted back", () => {
    // `synthesize_step_context` stamps a step's card when the whole *run*
    // settles, and writes the prompt into the input precisely because the body
    // was externalized. Grafting the body back brings the same turn in from the
    // record, so the input's copy must not be synthesized on top of it — it
    // carried the run's end time and landed after the step's final answer.
    const step: ToolContext = {
      id: "tool-external-step",
      kind: "tool",
      toolName: "workflow_step",
      input: {
        label: "审查一",
        task: "审查代理内核",
        runId: "wf-run-1",
        stepIndex: 0,
        status: "completed"
      },
      result: result("预览", true, "2026-08-26T02:00:00Z"),
      subagent: {
        kind: "workflowStep",
        task: "审查代理内核",
        status: "completed",
        contexts: [
          { id: "step-task", kind: "user", content: "审查代理内核", createdAt: "2026-08-26T01:00:01Z" },
          { id: "step-answer", kind: "assistant", content: "完整正文", createdAt: "2026-08-26T01:03:00Z" }
        ],
        updates: []
      },
      createdAt: "2026-08-26T02:00:00Z"
    };

    const [view] = deriveSubagentViews([step]);
    expect(view.contexts.map((context) => context.id)).toEqual(["step-task", "step-answer"]);
  });

  it("still shows a follow-up the child has not drained yet", () => {
    // The other half of the same rule: consuming a persisted turn is what
    // suppresses the synthesized one, so a message the record has not captured
    // must still appear — otherwise dropping the clock comparison would hide
    // every queued follow-up instead of just the duplicates.
    const spawn: ToolContext = {
      id: "tool-spawn",
      kind: "tool",
      toolName: "agent_spawn",
      input: { name: "helper", prompt: "审查 API" },
      result: result("已启动", true, "2026-08-26T01:00:05Z"),
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "child-task", kind: "user", content: "审查 API", createdAt: "2026-08-26T01:00:01Z" }
        ],
        updates: []
      },
      createdAt: "2026-08-26T01:00:05Z"
    };
    const followUp: ToolContext = {
      id: "tool-followup",
      kind: "tool",
      toolName: "followup_task",
      input: { target: "helper", message: "补充并发风险" },
      result: result("已送达", true, "2026-08-26T01:10:00Z"),
      createdAt: "2026-08-26T01:10:00Z"
    };

    const [view] = deriveSubagentViews([spawn, followUp]);
    expect(view.contexts.filter((context) => context.kind === "user").map((context) => context.content))
      .toEqual(["审查 API", "补充并发风险"]);
  });

  it("still shows a follow-up whose text a forked parent turn happens to repeat", () => {
    // `context: "conversation"` copies the parent's own user turns into the head
    // of the child's record, so the record holds user turns no call produced.
    // Matching on content alone let this undrained follow-up claim the
    // inherited turn and disappear; the pairing has to preserve record order.
    const spawn: ToolContext = {
      id: "tool-spawn",
      kind: "tool",
      toolName: "agent_spawn",
      input: { name: "alpha", prompt: "审查 API", context: "conversation" },
      result: result("已启动", true, "2026-08-26T10:00:05Z"),
      createdAt: "2026-08-26T10:00:05Z"
    };
    const followUp: ToolContext = {
      id: "tool-followup",
      kind: "tool",
      toolName: "followup_task",
      input: { target: "alpha", message: "补充并发风险" },
      result: result("已送达", true, "2026-08-26T10:01:00Z"),
      subagent: {
        name: "alpha",
        task: "审查 API",
        status: "completed",
        contexts: [
          // Inherited from the parent conversation, which had already said this.
          { id: "parent-u1", kind: "user", content: "补充并发风险", createdAt: "2026-08-26T09:00:00Z" },
          { id: "child-task", kind: "user", content: "审查 API", createdAt: "2026-08-26T10:00:00Z" },
          { id: "child-answer", kind: "assistant", content: "结论", createdAt: "2026-08-26T10:00:03Z" }
        ],
        updates: []
      },
      createdAt: "2026-08-26T10:01:00Z"
    };

    const [view] = deriveSubagentViews([spawn, followUp]);
    expect(view.contexts.filter((context) => context.kind === "user").map((context) => context.content))
      .toEqual(["审查 API", "补充并发风险", "补充并发风险"]);
    // The one at the tail is the follow-up the child has not answered yet.
    expect(view.contexts.at(-1)).toMatchObject({
      kind: "user",
      content: "补充并发风险",
      createdAt: "2026-08-26T10:01:00Z"
    });
  });

  it("marks the workflow run as a run and refuses to open it as an agent", () => {
    // Both the run and its steps carry `kind: "workflowStep"`, so the flag is
    // what tells them apart — and it is read off the `workflow` call, not off
    // the children, so a run that has not spawned a step yet is still a run.
    const step: ToolContext = {
      id: "tool-workflow-step",
      kind: "tool",
      toolName: "workflow_step",
      input: { label: "审查一", task: "审查代理内核" },
      result: result("Done"),
      subagent: {
        kind: "workflowStep",
        task: "审查代理内核",
        status: "completed",
        contexts: [],
        updates: []
      },
      createdAt: "2026-08-26T01:00:00Z"
    };
    const run: ToolContext = {
      id: "tool-workflow",
      kind: "tool",
      toolName: "workflow",
      input: { scriptName: "review-proxy", scriptBytes: 4096, scriptSha256: "0f1e" },
      result: result("{\"status\":\"complete\"}"),
      subagent: {
        kind: "workflowStep",
        task: "Review the proxy.",
        status: "completed",
        contexts: [step],
        updates: []
      },
      createdAt: "2026-08-26T02:00:00Z"
    };

    const views = deriveSubagentViews([run]);
    const runView = views.find((view) => view.id === "tool-workflow");
    const stepView = views.find((view) => view.id === "tool-workflow-step");
    expect(runView?.workflowRun).toBe(true);
    expect(stepView?.workflowRun).toBe(false);
    // The step keeps its read-only transcript; the run has none to offer.
    expect(findOpenableSubagentView(views, "tool-workflow-step")).toBe(stepView);
    expect(findOpenableSubagentView(views, "tool-workflow")).toBeNull();
    // Routing still resolves it — the task panel derives the run from this
    // very view — so only the read-only surface is closed off.
    expect(findSubagentView(views, "tool-workflow")).toBe(runView);
  });

  it("marks a run that has not spawned a step yet as a run", () => {
    const starting: ToolContext = {
      id: "tool-workflow",
      kind: "tool",
      toolName: "workflow",
      input: { scriptName: "review-proxy", scriptBytes: 4096, scriptSha256: "0f1e" },
      result: result(""),
      streaming: true,
      streamStatus: "running",
      live: { contexts: [], updates: [], status: "running" },
      createdAt: "2026-08-26T02:00:00Z"
    };

    const [view] = deriveSubagentViews([starting]);
    expect(view.childIds).toEqual([]);
    expect(view.workflowRun).toBe(true);
    expect(findOpenableSubagentView([view], "tool-workflow")).toBeNull();
  });

  it("resolves an approval card's source to the requester's openable page", () => {
    // A regular subagent card can be addressed by its view id.
    const spawned: ToolContext = {
      id: "tool-spawn",
      kind: "tool",
      toolName: "agent_spawn",
      input: { name: "researcher", prompt: "调研路由" },
      result: result("已派生"),
      subagent: {
        name: "researcher",
        task: "调研路由",
        // An in-flight record deliberately carries the live status; the settled
        // record type does not include it, hence the assertion.
        status: "running" as unknown as SubagentRunRecord["status"],
        contexts: [],
        updates: []
      },
      createdAt: "2026-08-26T01:00:00Z"
    };
    // A streaming workflow step has no record and is keyed by its parent
    // timeline context id, so the card must carry `sourceCallId`.
    const streamingStep: ToolContext = {
      id: "call-step-1",
      kind: "tool",
      toolName: "workflow_step",
      input: { label: "审查一", task: "审查代理内核" },
      result: result(""),
      streaming: true,
      streamStatus: "running",
      live: { contexts: [], updates: [], status: "running" },
      createdAt: "2026-08-26T02:00:00Z"
    };
    const views = deriveSubagentViews([spawned, streamingStep]);

    expect(
      approvalPromptSubagentView(views, { sourceAgent: "researcher", sourceCallId: "call-x" })?.id
    ).toBe("researcher");
    expect(
      approvalPromptSubagentView(views, { sourceAgent: "ws1", sourceCallId: "call-step-1" })?.id
    ).toBe("call-step-1");
    // Primary-conversation cards have no source and do not navigate.
    expect(approvalPromptSubagentView(views, {})).toBeNull();
    // Workflow runs are not openable and do not navigate.
    const run: ToolContext = {
      id: "tool-workflow",
      kind: "tool",
      toolName: "workflow",
      input: { scriptName: "review-proxy", scriptBytes: 4096, scriptSha256: "0f1e" },
      result: result(""),
      streaming: true,
      streamStatus: "running",
      live: { contexts: [], updates: [], status: "running" },
      createdAt: "2026-08-26T02:00:00Z"
    };
    const withRun = deriveSubagentViews([run]);
    expect(
      approvalPromptSubagentView(withRun, { sourceCallId: "tool-workflow" })
    ).toBeNull();
  });


  it("opens no agent thread for a web_search call", () => {
    // A web search has a registry-backed task row, not a SubagentRunRecord, so it
    // must never grow an empty read-only subagent thread in the timeline.
    const search: ToolContext = {
      id: "tool-web-search",
      kind: "tool",
      toolName: "web_search",
      input: { objective: "查清发布日期" },
      result: result("{\"findings\":\"…\"}"),
      createdAt: "2026-07-24T02:00:00Z"
    };

    expect(deriveSubagentViews([search])).toEqual([]);
  });
});

describe("agent timeline row status", () => {
  function agentTool(
    id: string,
    toolName: string,
    input: ToolContext["input"] = {},
    options: {
      success?: boolean;
      live?: SubagentLiveState;
      streaming?: boolean;
      streamStatus?: ToolContext["streamStatus"];
      subagent?: ToolContext["subagent"];
    } = {}
  ): ToolContext {
    const success = options.success ?? true;
    return {
      id,
      kind: "tool",
      toolName,
      input,
      result: { success, output: "", executedAt: "2026-07-14T00:00:00Z", durationMs: 5 },
      ...(options.streaming || options.live ? {
        streaming: true,
        streamStatus: options.streamStatus ?? "completed",
        ...(options.live ? { live: options.live } : {})
      } : {}),
      ...(options.subagent ? { subagent: options.subagent } : {}),
      createdAt: "2026-07-14T00:00:00Z"
    };
  }

  const runningLive = (status: SubagentLiveState["status"] = "running"): SubagentLiveState => ({
    contexts: [],
    updates: [],
    status
  });

  it("keeps accepted spawn and send calls running through the streamed handoff gap", () => {
    // The call only acknowledges that the protocol accepted it; the background
    // child is still running until live/subagent state arrives.
    expect(agentTimelineRunStatus(agentTool("spawn", "agent_spawn", { name: "spawned" }, {
      streaming: true,
      streamStatus: "completed"
    }))).toBe("running");
    expect(agentTimelineRunStatus(agentTool("send", "agent_send", { agent: "continued" }, {
      streaming: true,
      streamStatus: "completed"
    }))).toBe("running");
    // A queued message is not a run: it never owns a child of its own.
    expect(agentTimelineRunStatus(agentTool("queue", "send_message", { target: "beta" }, {
      streaming: true,
      streamStatus: "completed"
    }))).toBe("completed");
    expect(agentTimelineRunStatus(agentTool("persisted", "agent_spawn", { name: "finished" }, {
      subagent: { name: "finished", task: "完成", status: "completed", contexts: [], updates: [] }
    }))).toBe("completed");
  });

  it("gives a live child the terminal status it actually reached, not a blanket interrupt", () => {
    // Folding every non-idle terminal value into "interrupted" reported a
    // deliberate stop or a round ceiling as a failure neither of them was.
    for (const status of ["failed", "stopped", "roundLimit", "interrupted"] as const) {
      expect(agentTimelineRunStatus(
        agentTool(`live-${status}`, "agent_spawn", {}, { live: runningLive(status) })
      )).toBe(status);
    }
    expect(agentTimelineRunStatus(
      agentTool("idle", "agent_spawn", {}, { live: runningLive("idle") })
    )).toBe("completed");
    // A settled context whose live state still claims "running" never reported
    // an ending, so it keeps the interruption its result already records.
    expect(agentTimelineRunStatus({
      ...agentTool("settled-running", "workflow_step", {}, { live: runningLive() }),
      streaming: undefined,
      result: { success: false, output: "", executedAt: "2026-07-14T00:00:00Z", durationMs: 1 }
    })).toBe("interrupted");
  });

  it("routes a row to its agent by name before falling back to the call id", () => {
    expect(agentTimelineRouteId(agentTool("call", "agent_spawn", { name: "spawned" }, {
      subagent: { name: "recorded", task: "t", status: "completed", contexts: [], updates: [] }
    }))).toBe("recorded");
    expect(agentTimelineRouteId(agentTool("call", "agent_send", { agent: "addressed" }))).toBe("addressed");
    expect(agentTimelineRouteId(agentTool("call", "send_message", { target: "queued" }))).toBe("queued");
    expect(agentTimelineRouteId(agentTool("call", "agent_spawn", { name: "declared" }))).toBe("declared");
    expect(agentTimelineRouteId(agentTool("anonymous", "workflow_step", {}))).toBe("anonymous");
  });
});
