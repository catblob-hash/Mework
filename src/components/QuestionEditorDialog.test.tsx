import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { answersFromFormattedContent, questionsFromInput } from "../lib/orchestration";
import type { JsonObject, ToolContext, UserContext } from "../types";
import { QuestionEditorDialog } from "./QuestionEditorDialog";

function questionContext(input: JsonObject): ToolContext {
  return {
    id: "ask-1",
    kind: "tool",
    toolName: "ask_user",
    input,
    result: {
      success: true,
      output: "Waiting for user input.",
      executedAt: "2026-07-23T00:00:00.000Z",
      durationMs: 0
    },
    createdAt: "2026-07-23T00:00:00.000Z"
  };
}

const input: JsonObject = {
  questions: [
    {
      header: "方案",
      question: "选择实现方案？",
      multiSelect: false,
      options: [
        { label: "稳健", description: "优先兼容", preview: "safe()" },
        { label: "快速", description: "优先速度" }
      ]
    },
    {
      header: "能力",
      question: "需要哪些能力？",
      multiSelect: true,
      options: [
        { label: "搜索", description: "查询网络" },
        { label: "导出", description: "生成文件" }
      ]
    }
  ]
};

const answer: UserContext = {
  id: "answer-1",
  kind: "user",
  content: "User has answered your questions: \"选择实现方案？\"=\"稳健\", \"需要哪些能力？\"=\"搜索, 导出\"",
  createdAt: "2026-07-23T00:00:01.000Z"
};

describe("QuestionEditorDialog", () => {
  it("edits fixed questions, options, and answers as one message", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <QuestionEditorDialog
        item={questionContext(input)}
        answer={answer}
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    expect(screen.getAllByRole("heading", { level: 3 })).toHaveLength(2);
    expect(screen.queryByRole("button", { name: /增加问题|删除问题|Add question|Remove question/ })).not.toBeInTheDocument();
    expect(screen.getByLabelText("第 1 题回答")).toHaveValue("稳健");
    expect(screen.getByLabelText("第 2 题回答")).toHaveValue("搜索, 导出");

    await user.clear(screen.getByLabelText("第 1 题问题内容"));
    await user.type(screen.getByLabelText("第 1 题问题内容"), "采用哪个方案？");
    await user.clear(screen.getByLabelText("第 1 题选项 1 名称"));
    await user.type(screen.getByLabelText("第 1 题选项 1 名称"), "兼容");
    await user.clear(screen.getByLabelText("第 1 题选项 1 预览"));
    await user.type(screen.getByLabelText("第 1 题选项 1 预览"), "compatible()");
    await user.clear(screen.getByLabelText("第 1 题回答"));
    await user.type(screen.getByLabelText("第 1 题回答"), "兼容");
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).toHaveBeenCalledTimes(1);
    const [savedInput, savedAnswer] = onSave.mock.calls[0] as [JsonObject, string];
    expect(savedInput).toEqual({
      questions: [
        {
          header: "方案",
          question: "采用哪个方案？",
          multiSelect: false,
          options: [
            { label: "兼容", description: "优先兼容", preview: "compatible()" },
            { label: "快速", description: "优先速度" }
          ]
        },
        {
          header: "能力",
          question: "需要哪些能力？",
          multiSelect: true,
          options: [
            { label: "搜索", description: "查询网络" },
            { label: "导出", description: "生成文件" }
          ]
        }
      ]
    });
    expect(
      answersFromFormattedContent(savedAnswer, questionsFromInput(savedInput))
    ).toEqual(["兼容", "搜索, 导出"]);
  });

  it("validates headers, question content, option fields, and completed answers", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <QuestionEditorDialog
        item={questionContext(input)}
        answer={answer}
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    fireEvent.change(screen.getByLabelText("第 1 题标题"), { target: { value: "一二三四五六七八九十一二三" } });
    await user.clear(screen.getByLabelText("第 1 题问题内容"));
    await user.clear(screen.getByLabelText("第 1 题选项 1 名称"));
    await user.clear(screen.getByLabelText("第 1 题选项 2 说明"));
    await user.clear(screen.getByLabelText("第 1 题回答"));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByText("标题不能超过 12 个字符")).toBeInTheDocument();
    expect(screen.getByText("问题内容不能为空")).toBeInTheDocument();
    expect(screen.getByText("选项名称不能为空")).toBeInTheDocument();
    expect(screen.getByText("选项说明不能为空")).toBeInTheDocument();
    expect(screen.getByText("回答不能为空")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("请修正标记的字段");
  });

  it("allows option counts to change without changing the number of questions", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <QuestionEditorDialog
        item={questionContext(input)}
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    await user.click(screen.getByRole("button", { name: "为第 1 题增加选项" }));
    expect(screen.getAllByRole("heading", { level: 3 })).toHaveLength(2);
    await user.type(screen.getByLabelText("第 1 题选项 3 名称"), "平衡");
    await user.type(screen.getByLabelText("第 1 题选项 3 说明"), "兼顾风险与速度");
    await user.click(screen.getByRole("button", { name: "保存" }));

    const [savedInput] = onSave.mock.calls[0] as [JsonObject];
    const savedQuestions = questionsFromInput(savedInput);
    expect(savedQuestions).toHaveLength(2);
    expect(savedQuestions[0].options.map((option) => option.label)).toEqual(["稳健", "快速", "平衡"]);
  });

  it("keeps an unparseable completed answer editable instead of discarding it", () => {
    render(
      <QuestionEditorDialog
        item={questionContext({
          questions: Array.isArray(input.questions) ? [input.questions[0]] : []
        })}
        answer={{ ...answer, content: "旧版自由文本回答" }}
        onSave={vi.fn()}
        onClose={vi.fn()}
      />
    );

    expect(screen.getByLabelText("第 1 题回答")).toHaveValue("旧版自由文本回答");
  });

  it("locks the dialog during async save and displays save failures", async () => {
    const user = userEvent.setup();
    let rejectSave: ((reason: Error) => void) | undefined;
    const onSave = vi.fn(() => new Promise<void>((_, reject) => {
      rejectSave = reject;
    }));
    render(
      <QuestionEditorDialog
        item={questionContext(input)}
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(screen.getByRole("button", { name: "正在保存…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();

    rejectSave?.(new Error("写入冲突"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("保存失败：写入冲突"));
    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
  });
});
