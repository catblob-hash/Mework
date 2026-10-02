import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { PendingToolPrompt } from "../types";
import { ToolApprovalDock } from "./ToolApprovalDock";

function prompt(overrides: Partial<PendingToolPrompt> = {}): PendingToolPrompt {
  return {
    promptId: "prompt-1",
    toolName: "write_file",
    label: "写入文件",
    summary: "src/main.rs",
    riskLevel: "中",
    reason: "写入工作区文件会覆盖现有内容",
    allowAlwaysOffered: true,
    ...overrides
  };
}

describe("ToolApprovalDock", () => {
  afterEach(() => configureI18n("zh-CN"));

  it("renders nothing when no call is waiting", () => {
    const { container } = render(<ToolApprovalDock pending={null} onDecide={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("describes the call and offers deny, always allow, and allow in that order", () => {
    render(<ToolApprovalDock pending={prompt()} onDecide={vi.fn()} />);

    const dialog = screen.getByRole("dialog", { name: "需要你的确认" });
    expect(dialog).toHaveTextContent("写入文件");
    expect(dialog).toHaveTextContent("src/main.rs");
    expect(dialog).toHaveTextContent("写入工作区文件会覆盖现有内容");
    expect(dialog).toHaveTextContent("风险 中");

    const actions = Array.from(dialog.querySelectorAll("footer button"))
      .map((button) => button.textContent);
    expect(actions).toEqual(["拒绝", "总是允许", "允许"]);
  });

  it("frames the host's risk level as English copy in the English UI", () => {
    configureI18n("en-US");
    render(<ToolApprovalDock pending={prompt({ riskLevel: "Medium" })} onDecide={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Your approval is needed" })).toHaveTextContent("Medium risk");
  });

  it("reports the chosen decision", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ToolApprovalDock pending={prompt()} onDecide={onDecide} />);

    await user.click(screen.getByRole("button", { name: "允许" }));
    expect(onDecide).toHaveBeenCalledWith("allow_once", undefined);
  });

  it("reports a denial and a blanket allowance distinctly", async () => {
    const user = userEvent.setup();
    const onDeny = vi.fn();
    const { unmount } = render(<ToolApprovalDock pending={prompt()} onDecide={onDeny} />);
    await user.click(screen.getByRole("button", { name: "拒绝" }));
    expect(onDeny).toHaveBeenCalledWith("deny", undefined);
    unmount();

    const onAlways = vi.fn();
    render(<ToolApprovalDock pending={prompt()} onDecide={onAlways} />);
    await user.click(screen.getByRole("button", { name: "总是允许" }));
    expect(onAlways).toHaveBeenCalledWith("allow_always", undefined);
  });

  it("answers once even when the buttons are clicked repeatedly", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ToolApprovalDock pending={prompt()} onDecide={onDecide} />);

    const allow = screen.getByRole("button", { name: "允许" });
    await user.click(allow);
    await user.click(allow);
    await user.click(screen.getByRole("button", { name: "拒绝" }));

    expect(onDecide).toHaveBeenCalledTimes(1);
    expect(onDecide).toHaveBeenCalledWith("allow_once", undefined);
    expect(screen.getByRole("status")).toHaveTextContent("已允许");
  });

  it("withholds the blanket allowance the backend did not offer", () => {
    render(
      <ToolApprovalDock
        pending={prompt({ toolName: "bash", label: "运行命令", summary: "rm -rf build", allowAlwaysOffered: false })}
        onDecide={vi.fn()}
      />
    );

    expect(screen.queryByRole("button", { name: "总是允许" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "拒绝" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "允许" })).toBeInTheDocument();
  });

  it("names the subagent that asked", () => {
    render(
      <ToolApprovalDock pending={prompt({ requester: "researcher" })} onDecide={vi.fn()} />
    );
    expect(screen.getByRole("dialog")).toHaveTextContent("researcher · 写入文件");
  });

  it("draws the stack pager only for two or more pending cards", () => {
    const { rerender } = render(
      <ToolApprovalDock pending={prompt()} onDecide={vi.fn()} />
    );
    expect(screen.queryByRole("button", { name: "下一条待确认" })).not.toBeInTheDocument();

    rerender(
      <ToolApprovalDock
        pending={prompt()}
        stack={{ index: 0, total: 3, onNavigate: vi.fn() }}
        onDecide={vi.fn()}
      />
    );
    expect(screen.getByRole("dialog")).toHaveTextContent("1/3");
    expect(screen.getByRole("button", { name: "上一条待确认" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "下一条待确认" })).toBeEnabled();
  });

  it("flips through the stack without answering, and stops at the last card", async () => {
    const user = userEvent.setup();
    const onNavigate = vi.fn();
    const onDecide = vi.fn();
    const { rerender } = render(
      <ToolApprovalDock
        pending={prompt()}
        stack={{ index: 1, total: 3, onNavigate }}
        onDecide={onDecide}
      />
    );
    expect(screen.getByRole("dialog")).toHaveTextContent("2/3");

    await user.click(screen.getByRole("button", { name: "上一条待确认" }));
    expect(onNavigate).toHaveBeenCalledWith(-1);
    await user.click(screen.getByRole("button", { name: "下一条待确认" }));
    expect(onNavigate).toHaveBeenCalledWith(1);
    expect(onDecide).not.toHaveBeenCalled();

    rerender(
      <ToolApprovalDock
        pending={prompt({ promptId: "prompt-3" })}
        stack={{ index: 2, total: 3, onNavigate }}
        onDecide={onDecide}
      />
    );
    expect(screen.getByRole("button", { name: "下一条待确认" })).toBeDisabled();
  });

  it("starts a new card fresh after the previous one was answered", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    const { rerender } = render(<ToolApprovalDock pending={prompt()} onDecide={onDecide} />);
    await user.click(screen.getByRole("button", { name: "允许" }));

    rerender(
      <ToolApprovalDock
        pending={prompt({ promptId: "prompt-2", summary: "src/lib.rs" })}
        onDecide={onDecide}
      />
    );
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "拒绝" }));

    expect(onDecide).toHaveBeenNthCalledWith(1, "allow_once", undefined);
    expect(onDecide).toHaveBeenNthCalledWith(2, "deny", undefined);
  });

  it("asks for approval or feedback instead of a tool question", () => {
    render(
      <ToolApprovalDock
        pending={prompt({ toolName: "exit_plan_mode", label: "退出计划模式", kind: "plan_exit", summary: "分三步替换旧的审批闸" })}
        onDecide={vi.fn()}
      />
    );

    const dialog = screen.getByRole("dialog", { name: "计划已就绪，批准或提意见" });
    expect(dialog).toHaveAttribute("data-approval-kind", "plan_exit");
    // The card is about the conversation, not about one call: no risk, no label.
    expect(dialog).not.toHaveTextContent("风险");
    expect(dialog).not.toHaveTextContent("退出计划模式");

    // No refusal and no security level among the answers: approve, or reply.
    const actions = Array.from(dialog.querySelectorAll("footer button"))
      .map((button) => button.textContent);
    expect(actions).toEqual(["发送意见", "批准"]);
    expect(screen.getByRole("textbox", { name: "修改意见" })).toBeInTheDocument();
  });

  it("approves a plan without touching the security level", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ToolApprovalDock pending={prompt({ kind: "plan_exit" })} onDecide={onDecide} />);
    await user.click(screen.getByRole("button", { name: "批准" }));
    expect(onDecide).toHaveBeenCalledWith("allow_once", undefined);
    expect(screen.getByRole("status")).toHaveTextContent("已批准，模型开始实施");
  });

  it("sends what to change from the card's reply box", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ToolApprovalDock pending={prompt({ kind: "plan_exit" })} onDecide={onDecide} />);

    const send = screen.getByRole("button", { name: "发送意见" });
    expect(send).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: "修改意见" }), "  先补迁移脚本  ");
    // Typing is not yet an answer; the model is still waiting.
    expect(onDecide).not.toHaveBeenCalled();
    await user.click(send);

    expect(onDecide).toHaveBeenCalledTimes(1);
    expect(onDecide).toHaveBeenCalledWith("deny", "先补迁移脚本");
    expect(screen.getByRole("status")).toHaveTextContent("已发送意见");
  });
});
