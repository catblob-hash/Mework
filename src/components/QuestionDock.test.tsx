import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { QuestionItemView } from "../lib/orchestration";
import type { JsonValue, PendingToolPrompt } from "../types";
import { QuestionDock } from "./QuestionDock";

function card(questions: QuestionItemView[], promptId = "prompt-1"): PendingToolPrompt {
  return {
    promptId,
    toolName: "ask_user",
    kind: "question",
    label: "回答问题？",
    summary: questions[0]?.question ?? "",
    riskLevel: "低",
    reason: "模型在等你回答问题",
    allowAlwaysOffered: false,
    mandatory: true,
    questions: questions as unknown as JsonValue
  };
}

const choice = (prompt: string, header = "方案", multiSelect = false): QuestionItemView => ({
  question: prompt,
  header,
  options: [
    { label: "方案 A", description: "保持改动最小" },
    { label: "方案 B", description: "完整重构" }
  ],
  multiSelect
});

describe("QuestionDock", () => {
  it("draws nothing without a question card", () => {
    const { container } = render(<QuestionDock prompt={null} onRespond={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
    const approval = { ...card([choice("Q?")]), kind: "tool" as const };
    const { container: other } = render(<QuestionDock prompt={approval} onRespond={vi.fn()} />);
    expect(other).toBeEmptyDOMElement();
  });

  it("submits a lone single-select question as soon as an option is picked", async () => {
    const onRespond = vi.fn();
    render(<QuestionDock prompt={card([choice("采用哪个方案？")])} onRespond={onRespond} />);

    // One single-select question: no Submit tab and no arrows.
    expect(screen.queryByRole("tab", { name: /提交/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "切换到下一个问题" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("option", { name: /方案 A/ }));

    expect(onRespond).toHaveBeenCalledTimes(1);
    expect(onRespond).toHaveBeenCalledWith({
      action: "submit",
      answers: ["方案 A"],
      previews: [null],
      notes: [null]
    });
  });

  it("advances on each pick and submits what was answered from the review screen", async () => {
    const onRespond = vi.fn();
    render(
      <QuestionDock
        prompt={card([choice("先做哪个？", "顺序"), choice("用什么库？", "库")])}
        onRespond={onRespond}
      />
    );

    await userEvent.click(screen.getByRole("option", { name: /方案 B/ }));
    // Picked, so the first tab is ticked and the second question is up.
    expect(screen.getByRole("tab", { name: "顺序（已回答）" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "用什么库？" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("tab", { name: /提交/ }));
    expect(screen.getByRole("heading", { name: "检查你的回答" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("你还有问题没有回答");
    expect(screen.getByText("→ 方案 B")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "提交回答" }));
    expect(onRespond).toHaveBeenCalledWith(expect.objectContaining({
      action: "submit",
      answers: ["方案 B", null]
    }));
  });

  it("joins multi-select answers, typed Other included, and moves on with Next", async () => {
    const onRespond = vi.fn();
    render(
      <QuestionDock
        prompt={card([choice("要哪些功能？", "功能", true), choice("用什么库？", "库")])}
        onRespond={onRespond}
      />
    );

    await userEvent.click(screen.getByRole("checkbox", { name: /方案 A/ }));
    await userEvent.type(screen.getByRole("textbox", { name: "其他" }), "my, custom");
    await userEvent.click(screen.getByRole("button", { name: "下一题" }));
    await userEvent.click(screen.getByRole("button", { name: /先聊聊这个/ }));

    expect(onRespond).toHaveBeenCalledWith(expect.objectContaining({
      action: "chat",
      answers: ['方案 A, "my, custom"', null]
    }));
  });

  it("answers a single-select question with the typed Other text, and closes on a blank one", async () => {
    const onRespond = vi.fn();
    const { unmount } = render(
      <QuestionDock prompt={card([choice("采用哪个方案？")])} onRespond={onRespond} />
    );
    const other = screen.getByRole("textbox", { name: "其他" });
    await userEvent.type(other, "  都不要  {Enter}");
    expect(onRespond).toHaveBeenLastCalledWith(expect.objectContaining({
      action: "submit",
      answers: ["  都不要  "]
    }));
    unmount();

    const onClose = vi.fn();
    render(<QuestionDock prompt={card([choice("采用哪个方案？")], "prompt-2")} onRespond={onClose} />);
    await userEvent.type(screen.getByRole("textbox", { name: "其他" }), "{Enter}");
    expect(onClose).toHaveBeenCalledWith({ action: "close", answers: [] });
  });

  it("checks the single-select Other circle when its box is clicked, and clears it when an option is picked", async () => {
    const onDraftChange = vi.fn();
    render(
      <QuestionDock
        prompt={card([choice("先做哪个？", "顺序"), choice("用什么库？", "库")])}
        onRespond={vi.fn()}
        onDraftChange={onDraftChange}
      />
    );
    const otherCircle = screen.getByRole("radio", { name: "选择其他" });
    expect(otherCircle).toHaveAttribute("aria-checked", "false");

    await userEvent.click(screen.getByRole("textbox", { name: "其他" }));
    expect(otherCircle).toHaveAttribute("aria-checked", "true");
    // Checked but empty is not an answer yet.
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", { action: "close", answers: [] });

    await userEvent.type(screen.getByRole("textbox", { name: "其他" }), "自己写");
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", expect.objectContaining({
      action: "submit",
      answers: ["自己写", null]
    }));

    // Picking an option moves the check off Other; the typed text stays but no longer answers.
    fireEvent.click(screen.getByRole("option", { name: /方案 A/ }));
    await userEvent.click(screen.getByRole("tab", { name: "顺序（已回答）" }));
    expect(screen.getByRole("radio", { name: "选择其他" })).toHaveAttribute("aria-checked", "false");
    expect(screen.getByRole("textbox", { name: "其他" })).toHaveValue("自己写");
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", expect.objectContaining({
      answers: ["方案 A", null]
    }));

    await userEvent.click(screen.getByRole("radio", { name: "选择其他" }));
    expect(screen.getByRole("radio", { name: "选择其他" })).toHaveAttribute("aria-checked", "true");
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", expect.objectContaining({
      answers: ["自己写", null]
    }));
  });

  it("closes with Esc and with the close button", async () => {
    const onEsc = vi.fn();
    const { unmount } = render(<QuestionDock prompt={card([choice("Q?")])} onRespond={onEsc} />);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onEsc).toHaveBeenCalledWith({ action: "close", answers: [] });
    unmount();

    const onButton = vi.fn();
    render(<QuestionDock prompt={card([choice("Q?")], "prompt-2")} onRespond={onButton} />);
    await userEvent.click(screen.getByRole("button", { name: "关闭提问" }));
    expect(onButton).toHaveBeenCalledWith({ action: "close", answers: [] });
  });

  it("keeps the page told what a composer message would hand back", async () => {
    const onDraftChange = vi.fn();
    render(
      <QuestionDock
        prompt={card([choice("先做哪个？", "顺序"), choice("用什么库？", "库")])}
        onRespond={vi.fn()}
        onDraftChange={onDraftChange}
      />
    );
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", { action: "close", answers: [] });

    await userEvent.click(screen.getByRole("option", { name: /方案 A/ }));
    expect(onDraftChange).toHaveBeenLastCalledWith("prompt-1", expect.objectContaining({
      action: "submit",
      answers: ["方案 A", null]
    }));
  });

  it("shows the focused option's preview beside the list and takes notes with n", async () => {
    const onRespond = vi.fn();
    const withPreview: QuestionItemView = {
      question: "哪种布局？",
      header: "布局",
      multiSelect: false,
      options: [
        { label: "网格", description: "两列", preview: "GRID-PREVIEW" },
        { label: "列表", description: "一列", preview: "LIST-PREVIEW" }
      ]
    };
    render(<QuestionDock prompt={card([withPreview, choice("用什么库？", "库")])} onRespond={onRespond} />);

    expect(screen.getByText("GRID-PREVIEW")).toBeInTheDocument();
    // The preview layout has no Other row.
    expect(screen.queryByRole("textbox", { name: "其他" })).not.toBeInTheDocument();
    fireEvent.mouseEnter(screen.getByRole("option", { name: /列表/ }));
    expect(screen.getByText("LIST-PREVIEW")).toBeInTheDocument();

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "n" });
    const notes = await screen.findByPlaceholderText("为这个方案添加备注…");
    await userEvent.type(notes, "再宽一点");
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    await userEvent.click(screen.getByRole("option", { name: /网格/ }));
    await userEvent.click(screen.getByRole("tab", { name: /提交/ }));
    await userEvent.click(screen.getByRole("button", { name: "提交回答" }));

    expect(onRespond).toHaveBeenCalledWith({
      action: "submit",
      answers: ["网格", null],
      previews: ["GRID-PREVIEW", null],
      notes: ["再宽一点", null]
    });
  });

  it("unlocks for a retry when the host did not take the answer", async () => {
    const onRespond = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    render(<QuestionDock prompt={card([choice("先做哪个？"), choice("用什么库？", "库")])} onRespond={onRespond} />);

    await userEvent.click(screen.getByRole("button", { name: "关闭提问" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭提问" })).toBeEnabled());
    await userEvent.click(screen.getByRole("button", { name: "关闭提问" }));
    expect(onRespond).toHaveBeenCalledTimes(2);
  });
});
