import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { PendingQuestion, QuestionItemView } from "../lib/orchestration";
import type { ToolContext } from "../types";
import { QuestionDock } from "./QuestionDock";

function question(id: string, questions: QuestionItemView[]): PendingQuestion {
  const input = { questions };
  const context: ToolContext = {
    id,
    kind: "tool",
    toolName: "ask_user",
    round: 1,
    input: input as unknown as ToolContext["input"],
    result: {
      success: true,
      output: "等待用户回答",
      executedAt: "2026-07-14T01:00:00Z",
      durationMs: 0
    },
    createdAt: "2026-07-14T01:00:00Z"
  };
  const first = questions[0];
  return { context, questions, question: first.question, options: first.options };
}

const single = (prompt: string): QuestionItemView => ({
  question: prompt,
  header: "方案",
  options: [
    { label: "方案 A", description: "保持改动最小" },
    { label: "方案 B", description: "完整重构" }
  ],
  multiSelect: false
});

describe("QuestionDock", () => {
  it("renders nothing without a pending question", () => {
    const { container } = render(<QuestionDock pending={null} onAnswer={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders a single question page, focuses the first choice, and completes once", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    const { container } = render(
      <QuestionDock pending={question("ask-plan", [single("采用哪个实现方案？")])} onAnswer={onAnswer} />
    );

    const dialog = screen.getByRole("dialog", { name: "需要你的回答" });
    expect(dialog).toHaveAttribute("aria-modal", "false");
    expect(dialog).toHaveAttribute("data-pending-question", "true");
    expect(container.querySelectorAll("fieldset")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "上一项" })).toBeDisabled();
    const done = screen.getByRole("button", { name: "完成" });
    expect(done).toBeDisabled();
    const first = screen.getByRole("button", { name: /方案 A.*保持改动最小/ });
    await waitFor(() => expect(first).toHaveFocus());

    await user.click(first);
    expect(first).toHaveAttribute("aria-pressed", "true");
    expect(done).toBeEnabled();
    await user.dblClick(done);

    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith(
      'User has answered your questions: "采用哪个实现方案？"="方案 A"'
    );
    expect(screen.getByRole("status")).toHaveTextContent("回答已提交");
    expect(first).toBeDisabled();
  });

  it("pages through questions and preserves option and custom-answer drafts when going back", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    const featureQuestion: QuestionItemView = {
      question: "需要启用哪些能力？",
      header: "能力",
      options: [
        { label: "搜索", description: "启用全文搜索" },
        { label: "导出", description: "启用文件导出" }
      ],
      multiSelect: true
    };
    const detailQuestion = single("还有其他要求吗？");
    const { container } = render(
      <QuestionDock pending={question("ask-multi", [featureQuestion, detailQuestion])} onAnswer={onAnswer} />
    );

    expect(container.querySelectorAll("fieldset")).toHaveLength(1);
    expect(screen.getByText("需要启用哪些能力？")).toBeInTheDocument();
    expect(screen.queryByText("还有其他要求吗？")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "上一项" })).toBeDisabled();
    const next = screen.getByRole("button", { name: "下一项" });
    expect(next).toBeDisabled();
    await user.click(screen.getByRole("button", { name: /搜索.*启用全文搜索/ }));
    await user.click(screen.getByRole("button", { name: /导出.*启用文件导出/ }));
    expect(next).toBeEnabled();
    expect(onAnswer).not.toHaveBeenCalled();
    await user.click(next);

    expect(container.querySelectorAll("fieldset")).toHaveLength(1);
    expect(screen.queryByText("需要启用哪些能力？")).not.toBeInTheDocument();
    const secondQuestion = screen.getByText("还有其他要求吗？").closest("fieldset")!;
    expect(screen.getByRole("button", { name: "上一项" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "完成" })).toBeDisabled();
    await user.click(within(secondQuestion).getByRole("button", { name: /其他.*输入自定义回答/ }));
    const custom = within(secondQuestion).getByPlaceholderText("输入你的回答…");
    await user.type(custom, "  需要离线模式  ");
    expect(screen.getByRole("button", { name: "完成" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "上一项" }));
    expect(screen.getByText("需要启用哪些能力？")).toBeInTheDocument();
    expect(screen.queryByText("还有其他要求吗？")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /搜索.*启用全文搜索/ })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /导出.*启用文件导出/ })).toHaveAttribute("aria-pressed", "true");

    await user.click(screen.getByRole("button", { name: "下一项" }));
    expect(screen.getByText("还有其他要求吗？")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("输入你的回答…")).toHaveValue("  需要离线模式  ");
    await user.click(screen.getByRole("button", { name: "完成" }));

    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith(
      'User has answered your questions: "需要启用哪些能力？"="搜索, 导出", "还有其他要求吗？"="需要离线模式"'
    );
  });

  it("closes an unfinished question through the supplied callback", async () => {
    const pending = question("ask-dismiss", [single("还要继续吗？")]);
    const onAnswer = vi.fn();
    const onDismissQuestion = vi.fn();
    const user = userEvent.setup();
    render(
      <QuestionDock
        pending={pending}
        onAnswer={onAnswer}
        onDismissQuestion={onDismissQuestion}
      />
    );

    await user.click(screen.getByRole("button", { name: "关闭并删除提问" }));

    expect(onDismissQuestion).toHaveBeenCalledTimes(1);
    expect(onDismissQuestion).toHaveBeenCalledWith(pending.context);
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("honors the disabled branch without reporting a submission", () => {
    const onAnswer = vi.fn();
    const { container } = render(
      <QuestionDock pending={question("ask-disabled", [single("现在继续吗？")])} disabled onAnswer={onAnswer} />
    );

    expect(screen.getByRole("dialog", { name: "需要你的回答" })).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByRole("button", { name: /方案 A/ })).toBeDisabled();
    expect(screen.getByText("暂时无法提交回答")).toBeInTheDocument();
    fireEvent.submit(container.querySelector("form")!);
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("resets selection when the question payload changes", async () => {
    const onAnswer = vi.fn();
    const user = userEvent.setup();
    const first = question("ask-first", [single("先选一个？")]);
    const { rerender } = render(<QuestionDock pending={first} onAnswer={onAnswer} />);

    await user.click(screen.getByRole("button", { name: /方案 A/ }));
    expect(screen.getByRole("button", { name: /方案 A/ })).toHaveAttribute("aria-pressed", "true");

    const changed = question("ask-first", [single("改成另一个问题？")]);
    rerender(<QuestionDock pending={changed} onAnswer={onAnswer} />);

    expect(screen.getByRole("button", { name: /方案 A/ })).toHaveAttribute("aria-pressed", "false");
    await waitFor(() => expect(screen.getByRole("button", { name: /方案 A/ })).toHaveFocus());
  });
});
