import { describe, expect, it } from "vitest";
import { ASK_USER_PENDING_OUTPUT } from "../types";
import type { ContextItem, JsonObject, ToolContext } from "../types";
import {
  answersFromFormattedContent,
  deriveAgentStatus,
  findPendingQuestion,
  formatQuestionAnswers,
  isClaudeQuestionInput,
  isOrchestrationToolName,
  isTaskUpdateFor,
  parseWaitOutput,
  questionsFromInput,
  taskIdFromTaskCreate
} from "./orchestration";

function tool(
  id: string,
  toolName: string,
  input: JsonObject,
  overrides: { success?: boolean; output?: string; streaming?: boolean; streamStatus?: ToolContext["streamStatus"] } = {}
): ToolContext {
  const { success = true, output = `${id}-output`, streaming, streamStatus } = overrides;
  return {
    id,
    kind: "tool",
    toolName,
    round: 1,
    input,
    result: { success, output, executedAt: "2026-07-12T00:00:00Z", durationMs: 12 },
    ...(streaming === undefined ? {} : { streaming }),
    ...(streamStatus === undefined ? {} : { streamStatus }),
    createdAt: "2026-07-12T00:00:00Z"
  };
}

function text(id: string, kind: "system" | "user" | "assistant", content: string): ContextItem {
  return { id, kind, content, createdAt: "2026-07-12T00:00:00Z" };
}

function reasoning(id: string, content: string): ContextItem {
  return { id, kind: "reasoning", content, createdAt: "2026-07-12T00:00:00Z" };
}

const pendingAskUser = (id: string, input: JsonObject) =>
  tool(id, "ask_user", input, { output: ASK_USER_PENDING_OUTPUT });

function taskCreate(
  contextId: string,
  taskId: string,
  subject: string,
  input: JsonObject = {}
): ToolContext {
  return tool(contextId, "todo", {
    action: "create",
    subject,
    description: `${subject}的详细说明`,
    ...input
  }, {
    output: JSON.stringify({ task: { id: taskId, subject } })
  });
}

function taskUpdate(
  contextId: string,
  taskId: string,
  input: JsonObject,
  overrides: { success?: boolean; output?: string; streaming?: boolean; streamStatus?: ToolContext["streamStatus"] } = {}
): ToolContext {
  return tool(contextId, "todo", { action: "update", taskId, ...input }, {
    output: JSON.stringify({
      success: true,
      taskId,
      updatedFields: Object.keys(input)
    }),
    ...overrides
  });
}

describe("isOrchestrationToolName", () => {
  it("recognizes todo protocol calls without classifying ordinary tools", () => {
    expect(isOrchestrationToolName("todo")).toBe(true);
    expect(isOrchestrationToolName("task_wait")).toBe(true);
    expect(isOrchestrationToolName("read")).toBe(false);
  });
});

describe("findPendingQuestion", () => {
  it("returns the trailing pending ask_user call with question and options parsed from its input", () => {
    const ask = pendingAskUser("ask-pending", {
      question: "采用哪个实现方案？",
      options: ["方案 A", "方案 B", "", "   ", 42]
    });
    const pending = findPendingQuestion([text("user", "user", "开始任务"), ask]);

    expect(pending).not.toBeNull();
    expect(pending?.context).toBe(ask);
    expect(pending?.question).toBe("采用哪个实现方案？");
    expect(pending?.options).toEqual([
      { label: "方案 A", description: "" },
      { label: "方案 B", description: "" }
    ]);
  });

  it("skips same-round rejected leftovers after the ask_user call", () => {
    const ask = pendingAskUser("ask-pending", { question: "继续执行吗？", options: ["继续"] });
    const rejectedRead = tool("read-rejected", "read", { path: "src/App.tsx" }, {
      success: false,
      output: "用户拒绝执行该工具调用"
    });
    const pending = findPendingQuestion([text("user", "user", "开始任务"), ask, rejectedRead]);

    expect(pending?.context).toBe(ask);
    expect(pending?.question).toBe("继续执行吗？");
  });

  it("returns null once a user context follows the ask_user call", () => {
    const ask = pendingAskUser("ask-answered", { question: "采用哪个实现方案？", options: ["方案 A"] });
    const contexts = [text("user", "user", "开始任务"), ask, text("answer", "user", "方案 A")];

    expect(findPendingQuestion(contexts)).toBeNull();
  });

  it("returns null when the ask_user call was rejected", () => {
    // A rejected question (invalid input, hook block) fails; only a successful
    // call is the host's "asked, paused" receipt, whatever wording the prompt
    // profile gave it.
    const ask = tool("ask-rejected", "ask_user", { question: "采用哪个实现方案？" }, {
      success: false,
      output: "questions must be an array of objects"
    });

    expect(findPendingQuestion([text("user", "user", "开始任务"), ask])).toBeNull();
  });

  it("recognizes a pending question by structure, not by the receipt's wording", () => {
    const ask = tool("ask-zh", "ask_user", { question: "继续执行吗？" }, {
      output: "已向用户提问，本轮暂停。"
    });

    expect(findPendingQuestion([text("user", "user", "开始任务"), ask])?.context).toBe(ask);
  });

  it("ignores interleaved system and reasoning contexts", () => {
    const ask = pendingAskUser("ask-pending", { question: "继续执行吗？", options: ["继续", "停止"] });
    const contexts = [
      text("user", "user", "开始任务"),
      reasoning("reason-before", "需要先确认方案"),
      ask,
      text("hook", "system", "钩子注入的诊断信息"),
      reasoning("reason-after", "等待用户回答")
    ];

    expect(findPendingQuestion(contexts)?.context).toBe(ask);
  });

  it("S11：唤醒 fold 的任务结果通知与唤醒回复不关闭悬停的问题", () => {
    const ask = pendingAskUser("ask-pending", { question: "测试继续吗？", options: ["继续", "停止"] });
    const contexts = [
      text("user", "user", "开始任务"),
      ask,
      // Tool contexts do not form an answer boundary.
      {
        id: "ctx_agent-result_wake1",
        kind: "tool" as const,
        toolName: "task_wait",
        input: { tasks: ["shell:lint"] },
        result: {
          success: true,
          output: "[shell:lint · 已完成]\n后台命令 shell:lint（bash）已结束，退出码 0：\nok",
          images: [],
          executedAt: "2026-08-26T00:00:00Z",
          durationMs: 0
        },
        createdAt: "2026-08-26T00:00:00Z"
      },
      text("wake-reply", "assistant", "后台命令结果已送达，问题仍待回答")
    ];

    expect(findPendingQuestion(contexts)?.context).toBe(ask);
    expect(
      findPendingQuestion([...contexts, text("answer", "user", "继续")])
    ).toBeNull();
  });

  /// Legacy persisted folds are user contexts and must remain excluded from answer boundaries by their ID prefix in `HOST_AUTHORED_USER_CONTEXT_PREFIXES`.
  it("S11：历史的 user 载体 fold 同样不关闭悬停的问题", () => {
    const ask = pendingAskUser("ask-legacy", { question: "继续吗？", options: ["继续", "停止"] });
    const contexts = [
      text("user", "user", "开始任务"),
      ask,
      text("ctx_agent-result_legacy", "user", "[shell:lint · 已完成]\nok")
    ];

    expect(findPendingQuestion(contexts)?.context).toBe(ask);
  });
});

describe("Claude Code AskUserQuestion protocol", () => {
  const input = {
    questions: [
      {
        question: "采用哪个方案？",
        header: "方案",
        options: [
          { label: "方案 A", description: "保持改动最小" },
          { label: "方案 B", description: "完整重构", preview: "B preview" }
        ],
        multiSelect: false
      },
      {
        question: "启用哪些能力？",
        header: "能力",
        options: [
          { label: "搜索", description: "全文搜索" },
          { label: "导出", description: "文件导出" }
        ],
        multiSelect: true
      }
    ]
  } as JsonObject;

  it("parses all Claude fields and formats answers by question text", () => {
    const questions = questionsFromInput(input);
    expect(questions).toHaveLength(2);
    expect(questions[0]).toMatchObject({
      header: "方案",
      multiSelect: false,
      options: [
        { label: "方案 A", description: "保持改动最小" },
        { label: "方案 B", description: "完整重构", preview: "B preview" }
      ]
    });
    const content = formatQuestionAnswers(questions, ["方案 A", "搜索, 导出"]);
    expect(content).toBe(
      'User has answered your questions: "采用哪个方案？"="方案 A", "启用哪些能力？"="搜索, 导出"'
    );
    expect(answersFromFormattedContent(content, questions)).toEqual(["方案 A", "搜索, 导出"]);
  });

  it("validates Claude limits and required option descriptions", () => {
    expect(isClaudeQuestionInput(input)).toBe(true);
    expect(isClaudeQuestionInput({ questions: [] })).toBe(false);
    expect(isClaudeQuestionInput({
      questions: [{
        question: "继续吗？",
        header: "这是一个肯定超过十二字符的标题",
        options: [{ label: "是", description: "继续" }, { label: "否", description: "停止" }],
        multiSelect: false
      }]
    })).toBe(false);
  });
});

describe("deriveAgentStatus", () => {
  it("keeps settled todo updates and skips in-flight streaming calls", () => {
    const contexts = [
      taskCreate("todo-create", "task-1", "旧任务"),
      taskUpdate("todo-complete", "task-1", { status: "completed" }),
      taskUpdate("todo-stream", "task-1", { subject: "还在流式传输的任务" }, {
        streaming: true,
        streamStatus: "running"
      })
    ];

    expect(deriveAgentStatus(contexts).todo).toEqual([{
      id: "task-1",
      content: "旧任务",
      description: "旧任务的详细说明",
      status: "completed",
      blocks: [],
      blockedBy: []
    }]);
  });

  it("requires merged state calls to carry an action before starting fresh task state", () => {
    const contexts = [
      tool("missing-todo-action", "todo", { subject: "缺少动作的任务" }),
      tool("malformed-task-create", "todo", {
        action: "create",
        subject: "无效现代任务",
        description: "没有结构化 id"
      }, {
        output: JSON.stringify({ task: { subject: "无效现代任务" } })
      }),
      taskCreate("modern-create-1", "task-1", "现代任务一"),
      taskCreate("modern-create-2", "task-2", "现代任务二"),
      tool("late-missing-todo-action", "todo", { subject: "不应插入的任务" })
    ];

    expect(deriveAgentStatus(contexts)).toEqual({
      todo: [
        {
          id: "task-1",
          content: "现代任务一",
          description: "现代任务一的详细说明",
          status: "pending",
          blocks: [],
          blockedBy: []
        },
        {
          id: "task-2",
          content: "现代任务二",
          description: "现代任务二的详细说明",
          status: "pending",
          blocks: [],
          blockedBy: []
        }
      ]
    });
  });

  it("returns null todo when no orchestration calls exist", () => {
    const contexts = [
      text("user", "user", "开始任务"),
      reasoning("reason", "先看看目录结构"),
      tool("read", "read", { path: "README.md" })
    ];

    expect(deriveAgentStatus(contexts)).toEqual({ todo: null });
  });

  it("replays todo create and partial update actions in order", () => {
    const contexts = [
      taskCreate("create-1", "task-1", "实现认证", {
        activeForm: "正在实现认证",
        metadata: { source: "model", removed: true }
      }),
      taskCreate("create-2", "task-2", "补齐测试"),
      taskUpdate("update-1", "task-1", {
        status: "in_progress",
        subject: "实现完整认证",
        owner: "agent-auth",
        addBlocks: ["task-2", "missing-task"],
        metadata: { removed: null, priority: "high" }
      }),
      tool("list", "todo", { action: "list" }, {
        output: JSON.stringify({ tasks: [{ id: "fake", subject: "只读结果", status: "completed", blockedBy: [] }] })
      }),
      taskUpdate("update-2", "task-2", {
        addBlockedBy: ["task-1"]
      })
    ];

    expect(deriveAgentStatus(contexts)).toEqual({
      todo: [
        {
          id: "task-1",
          content: "实现完整认证",
          description: "实现认证的详细说明",
          activeForm: "正在实现认证",
          status: "in_progress",
          blocks: ["task-2"],
          blockedBy: [],
          owner: "agent-auth",
          metadata: { source: "model", priority: "high" }
        },
        {
          id: "task-2",
          content: "补齐测试",
          description: "补齐测试的详细说明",
          status: "pending",
          blocks: [],
          blockedBy: ["task-1"]
        }
      ]
    });
  });

  it("replays a todo update from its input patch when an earlier update is removed", () => {
    const create = taskCreate("create", "task-1", "实现认证");
    const rename = taskUpdate("rename", "task-1", { subject: "实现完整认证" });
    const start = taskUpdate("start", "task-1", { status: "in_progress" });

    expect(deriveAgentStatus([create, rename, start]).todo).toMatchObject([{
      id: "task-1",
      content: "实现完整认证",
      status: "in_progress"
    }]);
    expect(deriveAgentStatus([create, start]).todo).toMatchObject([{
      id: "task-1",
      content: "实现认证",
      status: "in_progress"
    }]);
  });

  it("ignores failed, in-flight, malformed, duplicate, and dangling task events", () => {
    const valid = taskCreate("create", "task-1", "有效任务");
    const duplicate = taskCreate("duplicate", "task-1", "重复任务");
    const malformed = tool("malformed", "todo", {
      action: "create",
      subject: "坏结果",
      description: "结果没有 task id"
    }, { output: JSON.stringify({ task: { subject: "坏结果" } }) });
    const running = taskUpdate("running", "task-1", { status: "completed" }, {
      streaming: true,
      streamStatus: "running"
    });
    const failed = taskUpdate("failed", "task-1", { status: "completed" }, {
      success: false
    });
    const dangling = taskUpdate("dangling", "task-404", { status: "in_progress" });
    const malformedOutput = taskUpdate("bad-output", "task-1", { status: "completed" }, {
      output: JSON.stringify({ success: true, taskId: "some-other-task", updatedFields: ["status"] })
    });
    const missingUpdatedField = taskUpdate(
      "missing-updated-field",
      "task-1",
      { status: "completed", subject: "不完整确认" },
      {
        output: JSON.stringify({
          success: true,
          taskId: "task-1",
          updatedFields: ["status"]
        })
      }
    );
    const unexpectedUpdatedField = taskUpdate(
      "unexpected-updated-field",
      "task-1",
      { status: "completed" },
      {
        output: JSON.stringify({
          success: true,
          taskId: "task-1",
          updatedFields: ["status", "subject"]
        })
      }
    );

    expect(deriveAgentStatus([
      malformed,
      valid,
      duplicate,
      running,
      failed,
      dangling,
      malformedOutput,
      missingUpdatedField,
      unexpectedUpdatedField
    ]).todo).toEqual([{
      id: "task-1",
      content: "有效任务",
      description: "有效任务的详细说明",
      status: "pending",
      blocks: [],
      blockedBy: []
    }]);
  });

  it("removes a task with deleted and rolls back when that update is removed", () => {
    const create = taskCreate("create", "task-1", "交付功能");
    const start = taskUpdate("start", "task-1", { status: "in_progress" });
    const remove = taskUpdate("remove", "task-1", { status: "deleted" });

    expect(deriveAgentStatus([create, start, remove]).todo).toEqual([]);
    expect(deriveAgentStatus([create, start]).todo).toMatchObject([{
      id: "task-1",
      content: "交付功能",
      status: "in_progress"
    }]);
    expect(deriveAgentStatus([start, remove]).todo).toBeNull();
  });

});

describe("state event relationship helpers", () => {
  it("parses strict task create results and matches updates independently of execution success", () => {
    const createTask = taskCreate("task-create", "task-7", "实现导出");
    const failedTaskUpdate = taskUpdate("task-update", "task-7", { status: "completed" }, {
      success: false
    });

    expect(taskIdFromTaskCreate(createTask)).toBe("task-7");
    expect(isTaskUpdateFor(failedTaskUpdate, "task-7")).toBe(true);
    expect(taskIdFromTaskCreate(tool("bad", "todo", {
      action: "create",
      subject: "坏结果",
      description: "x"
    }, { output: "Task task-8 created" }))).toBeNull();
  });
});

describe("state action discriminator", () => {
  it("ignores calls without required actions and malformed todo actions", () => {
    const contexts = [
      tool("missing-todo-action", "todo", { subject: "缺少动作" }),
      tool("bad-create", "todo", { action: "create", subject: "无结构化 id" }, {
        output: JSON.stringify({ task: { subject: "无结构化 id" } })
      }),
      tool("todo-get", "todo", { action: "get", taskId: "task-1" }, {
        output: JSON.stringify({ task: { id: "task-1", subject: "只读结果" } })
      })
    ];

    expect(deriveAgentStatus(contexts)).toEqual({ todo: null });
  });

  it("uses required actions for todo create, update, and read-only calls", () => {
    const create = taskCreate("create", "task-1", "待办");
    const update = taskUpdate("update", "task-1", { status: "completed" });
    const get = tool("get", "todo", { action: "get", taskId: "task-1" }, {
      output: JSON.stringify({ task: { id: "task-1", subject: "待办", status: "completed" } })
    });
    const list = tool("list", "todo", { action: "list" }, {
      output: JSON.stringify({ tasks: [{ id: "task-1", subject: "待办", status: "completed" }] })
    });

    expect(deriveAgentStatus([create, update, get, list])).toMatchObject({
      todo: [{ id: "task-1", content: "待办", status: "completed" }]
    });
  });
});

describe("parseWaitOutput", () => {
  it("splits every drained envelope and keeps the trailing status roll-up apart", () => {
    const output = [
      "[review · 进度更新]",
      "已检查接口",
      "",
      "[review · 已完成]",
      "12 项检查全部通过",
      "第二行正文",
      "",
      "[tester · 已停止]",
      "（没有返回文本结果）",
      "",
      "当前状态：review 已完成、tester 已停止"
    ].join("\n");

    expect(parseWaitOutput(output)).toEqual({
      notice: "",
      statusLine: "当前状态：review 已完成、tester 已停止",
      envelopes: [
        { agent: "review", status: "进度更新", body: "已检查接口" },
        { agent: "review", status: "已完成", body: "12 项检查全部通过\n第二行正文" },
        { agent: "tester", status: "已停止", body: "（没有返回文本结果）" }
      ]
    });
  });

  it("returns the timeout and nothing-to-drain notices as notice, not as an envelope", () => {
    const timeout =
      "等待 60 秒后期限到了，被等待的任务还没有给出结果——它们仍在后台运行，什么都没有丢。可以再等一次（需要更久就把 timeout_seconds 调大，上限 600 秒），也可以先做别的。";
    expect(parseWaitOutput(timeout)).toEqual({
      envelopes: [],
      statusLine: "",
      notice: timeout
    });
    expect(parseWaitOutput("没有正在运行的任务，也没有待收取的更新。")).toMatchObject({
      envelopes: [],
      notice: "没有正在运行的任务，也没有待收取的更新。"
    });
  });

  it("keeps a notice that precedes real envelopes and never swallows unparsed text", () => {
    // A timeout delivers accumulated progress updates with its notification; only a terminal result or expiry ends the wait.
    const timeout =
      "等待 30 秒后期限到了，被等待的任务还没有给出结果——它们仍在后台运行，什么都没有丢。可以再等一次（需要更久就把 timeout_seconds 调大，上限 600 秒），也可以先做别的。";
    const parsed = parseWaitOutput([timeout, "", "[alpha · 进度更新]", "仍在跑 e2e"].join("\n"));

    expect(parsed.notice).toBe(timeout);
    expect(parsed.envelopes).toEqual([{ agent: "alpha", status: "进度更新", body: "仍在跑 e2e" }]);
  });

  it("refuses text that only looks like a header so opaque provider errors survive intact", () => {
    // A single bracketed line with no separator is not an envelope, and a
    // header split across lines is not one either. Both must reach the caller
    // as notice so the detail view can fall back to the raw output.
    const parsed = parseWaitOutput("provider returned an opaque wait error [oops]");
    expect(parsed.envelopes).toEqual([]);
    expect(parsed.notice).toBe("provider returned an opaque wait error [oops]");
  });
});
