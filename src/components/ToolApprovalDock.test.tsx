import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
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

  it("reports the chosen decision", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ToolApprovalDock pending={prompt()} onDecide={onDecide} />);

    await user.click(screen.getByRole("button", { name: "允许" }));
    expect(onDecide).toHaveBeenCalledWith("allow_once");
  });

  it("reports a denial and a blanket allowance distinctly", async () => {
    const user = userEvent.setup();
    const onDeny = vi.fn();
    const { unmount } = render(<ToolApprovalDock pending={prompt()} onDecide={onDeny} />);
    await user.click(screen.getByRole("button", { name: "拒绝" }));
    expect(onDeny).toHaveBeenCalledWith("deny");
    unmount();

    const onAlways = vi.fn();
    render(<ToolApprovalDock pending={prompt()} onDecide={onAlways} />);
    await user.click(screen.getByRole("button", { name: "总是允许" }));
    expect(onAlways).toHaveBeenCalledWith("allow_always");
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
    expect(onDecide).toHaveBeenCalledWith("allow_once");
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

    expect(onDecide).toHaveBeenNthCalledWith(1, "allow_once");
    expect(onDecide).toHaveBeenNthCalledWith(2, "deny");
  });
});
