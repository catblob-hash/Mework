import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { ContextItem, ToolContext } from "../types";
import { ConversationView } from "./ConversationView";

function structuralClasses(element: Element): string[] {
  return [...element.classList]
    .filter((className) => className !== "context-card--has-actions")
    .sort();
}

function structuralShape(element: Element): unknown {
  const mutationOnly = ".context-actions, .user-message-toolbar__edit, .user-message-toolbar__delete, .context-stream__hint, .insertion-line";
  return {
    tag: element.tagName.toLowerCase(),
    classes: structuralClasses(element),
    children: Array.from(element.children)
      .filter((child) => !child.matches(mutationOnly))
      .map(structuralShape)
  };
}

function readTool(): ToolContext {
  return {
    id: "shared-read",
    kind: "tool",
    toolName: "read",
    round: 1,
    input: { path: "README.md" },
    result: {
      success: true,
      output: "README content",
      executedAt: "2026-07-14T01:02:00Z",
      durationMs: 8
    },
    createdAt: "2026-07-14T01:02:00Z"
  };
}

describe("ConversationView shared rendering contract", () => {
  it("keeps editable and read-only timelines structurally identical outside the allowed mutation surface", async () => {
    Object.defineProperty(HTMLElement.prototype, "scrollTo", {
      configurable: true,
      value: vi.fn()
    });
    const document = createSeedDocument();
    const contexts: ContextItem[] = [
      { id: "shared-user", kind: "user", content: "检查共享视图", createdAt: "2026-07-14T01:00:00Z" },
      { id: "shared-reasoning", kind: "reasoning", content: "先读取说明", createdAt: "2026-07-14T01:01:00Z" },
      readTool(),
      { id: "shared-assistant", kind: "assistant", content: "共享视图正常", createdAt: "2026-07-14T01:03:00Z" }
    ];
    const handlers = { onEdit: vi.fn(), onDelete: vi.fn(), onInsert: vi.fn() };
    const user = userEvent.setup();
    const { container } = render(
      <div>
        <ConversationView
          contexts={contexts}
          tools={document.tools}
          enabledTools={document.tools.map((tool) => tool.name)}
          ariaLabel="可编辑主会话"
          composer={<div className="composer"><textarea aria-label="主会话输入" /></div>}
          {...handlers}
        />
        <ConversationView
          contexts={contexts}
          tools={document.tools}
          enabledTools={[]}
          editable={false}
          ariaLabel="只读子会话"
        />
      </div>
    );

    const editableView = container.querySelector<HTMLElement>('[data-conversation-view="editable"]')!;
    const readonlyView = container.querySelector<HTMLElement>('[data-conversation-view="readonly"]')!;
    expect(editableView).toHaveClass("conversation-view");
    expect(readonlyView).toHaveClass("conversation-view");

    const editableTimeline = editableView.querySelector<HTMLElement>(".context-stream")!;
    const readonlyTimeline = readonlyView.querySelector<HTMLElement>(".context-stream")!;
    expect(readonlyTimeline.className).toBe(editableTimeline.className);
    expect(structuralShape(readonlyTimeline)).toEqual(structuralShape(editableTimeline));

    for (const contextId of ["shared-user", "shared-reasoning", "shared-read", "shared-assistant"]) {
      const editableContext = editableView.querySelector<HTMLElement>(`[data-context-id="${contextId}"]`)!;
      const readonlyContext = readonlyView.querySelector<HTMLElement>(`[data-context-id="${contextId}"]`)!;
      expect(readonlyContext.tagName).toBe(editableContext.tagName);
      expect(structuralClasses(readonlyContext)).toEqual(structuralClasses(editableContext));
    }

    expect(editableView.querySelector(".context-actions")).toBeInTheDocument();
    expect(readonlyView.querySelector(".context-actions")).not.toBeInTheDocument();
    expect(within(editableView).getByLabelText("主会话输入")).toBeInTheDocument();
    expect(within(readonlyView).queryByRole("textbox")).not.toBeInTheDocument();
    expect(within(readonlyView).queryByRole("button", { name: /编辑|删除|发送/ })).not.toBeInTheDocument();

    fireEvent.contextMenu(editableView.querySelector<HTMLElement>('[data-context-id="shared-user"]')!, {
      clientX: 24,
      clientY: 24
    });
    expect(screen.getByRole("menu", { name: "添加上下文" })).toBeInTheDocument();
    fireEvent.pointerDown(window);
    expect(screen.queryByRole("menu", { name: "添加上下文" })).not.toBeInTheDocument();

    fireEvent.contextMenu(readonlyView.querySelector<HTMLElement>('[data-context-id="shared-user"]')!, {
      clientX: 24,
      clientY: 24
    });
    expect(screen.queryByRole("menu", { name: "添加上下文" })).not.toBeInTheDocument();

    const readonlyTool = within(readonlyView).getByRole("button", { name: /读取了文件.*完成/ });
    expect(readonlyTool).toHaveAttribute("aria-expanded", "false");
    await user.click(readonlyTool);
    expect(readonlyTool).toHaveAttribute("aria-expanded", "true");
    expect(within(readonlyView).getByText("README content")).toBeInTheDocument();
  });

  it("uses the same near-bottom stream following for editable and read-only conversations", async () => {
    const contexts: ContextItem[] = [
      { id: "follow-user", kind: "user", content: "开始", createdAt: "2026-07-14T01:00:00Z" }
    ];
    const renderViews = (items: ContextItem[]) => (
      <div>
        <ConversationView contexts={items} tools={[]} enabledTools={[]} />
        <ConversationView contexts={items} tools={[]} enabledTools={[]} editable={false} />
      </div>
    );
    const { container, rerender } = render(renderViews(contexts));
    const scrollers = Array.from(container.querySelectorAll<HTMLElement>(".context-scroll"));
    scrollers.forEach((scroller) => {
      Object.defineProperties(scroller, {
        scrollHeight: { configurable: true, value: 1_000 },
        clientHeight: { configurable: true, value: 500 },
        scrollTop: { configurable: true, value: 350, writable: true }
      });
    });

    rerender(renderViews([
      ...contexts,
      { id: "follow-assistant", kind: "assistant", content: "流式新增", streaming: true, createdAt: "2026-07-14T01:01:00Z" }
    ]));

    await waitFor(() => {
      for (const scroller of scrollers) expect(scroller.scrollTop).toBe(1_000);
    });
  });
});
