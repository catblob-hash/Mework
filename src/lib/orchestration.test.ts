import { describe, expect, it } from "vitest";
import { ASK_USER_PENDING_OUTPUT } from "../test/fixtures";
import type { ContextItem, JsonObject, ToolContext } from "../types";
import {
  answersFromFormattedContent,
  formatQuestionAnswers,
  isClaudeQuestionInput,
  isLegacyPausedQuestion,
  joinMultiSelectAnswer,
  questionAnswersFromInput,
  questionOutcome,
  parseWaitOutput,
  questionsFromInput,
  ASK_USER_CLOSED_OUTPUT,
  ASK_USER_NO_ANSWER_OUTPUT
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

const pendingAskUser = (id: string, input: JsonObject) =>
  tool(id, "ask_user", input, { output: ASK_USER_PENDING_OUTPUT });

describe("ask_user outcomes", () => {
  const input: JsonObject = {
    questions: [
      { question: "选哪个数据库？", header: "数据库", options: [{ label: "Postgres", description: "a" }, { label: "SQLite", description: "b" }], multiSelect: false },
      { question: "部署到哪？", header: "部署", options: [{ label: "Fly", description: "a" }, { label: "AWS", description: "b" }], multiSelect: false }
    ]
  };

  it("reads a blocking call's outcome from its own result", () => {
    const answered = tool("ask", "ask_user", {
      ...input,
      answers: { "选哪个数据库？": "Postgres" },
      annotations: {}
    }, { output: 'Your questions have been answered: "选哪个数据库？"="Postgres". You can now continue with these answers in mind.' });
    expect(questionOutcome(answered)).toBe("answered");
    expect(questionOutcome(tool("ask", "ask_user", input, { output: ASK_USER_CLOSED_OUTPUT }))).toBe("closed");
    expect(questionOutcome(tool("ask", "ask_user", input, { output: ASK_USER_NO_ANSWER_OUTPUT }))).toBe("unanswered");
    expect(questionOutcome(tool("ask", "ask_user", input, {
      success: false,
      output: "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\nThe user wants to clarify these questions."
    }))).toBe("declined");
    expect(questionOutcome(tool("ask", "ask_user", input, { success: false, output: "questions must be an array of objects" }))).toBe("failed");
  });

  it("treats only the old paused receipt as a question answered by the next user message", () => {
    expect(isLegacyPausedQuestion(pendingAskUser("ask", input))).toBe(true);
    expect(isLegacyPausedQuestion(tool("ask", "ask_user", input, { output: ASK_USER_CLOSED_OUTPUT }))).toBe(false);
    expect(isLegacyPausedQuestion(tool("ask", "ask_user", input, { output: "", streaming: true }))).toBe(false);
    // Conversation templates store the card with an empty receipt and its answer after it.
    expect(isLegacyPausedQuestion(tool("ask", "ask_user", input, { output: "" }))).toBe(true);
    expect(isLegacyPausedQuestion(text("user", "user", "Asked the user; this turn is paused."))).toBe(false);
  });

  it("reads answers and notes keyed by question text from the executed input", () => {
    const questions = questionsFromInput(input);
    expect(questionAnswersFromInput({
      ...input,
      answers: { "部署到哪？": "AWS" },
      annotations: { "选哪个数据库？": { notes: "都不合适" } }
    }, questions)).toEqual([{ notes: "都不合适" }, { answer: "AWS" }]);
  });

  it("joins multi-select answers the way Claude Code does", () => {
    expect(joinMultiSelectAnswer(["Red", "Blue"])).toBe("Red, Blue");
    expect(joinMultiSelectAnswer(["Red", "my, custom", 'say "hi"'])).toBe('Red, "my, custom", "say \\"hi\\""');
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
