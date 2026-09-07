import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { ASK_USER_PENDING_OUTPUT } from "../types";
import type { ContextItem, ToolContext } from "../types";
import type { ConversationTurn } from "../lib/conversationTurns";
import { applyAppearance, defaultAppearancePreferences } from "../lib/appearance";
import { deriveWorkflowProgress } from "../lib/workflowProgress";
import { deriveWorkflowItems } from "../lib/taskContainer";
import { deriveWorkflowRun } from "../lib/workflowRuns";
import { subagentViewFixture, taskMessagesFixture } from "../test/fixtures";
import { ContextStream } from "./ContextStream";

describe("ContextStream", () => {
  afterEach(() => vi.unstubAllGlobals());

  const box = (top: number, height: number) => ({
    x: 0,
    y: top,
    top,
    left: 0,
    right: 600,
    bottom: top + height,
    width: 600,
    height,
    toJSON: () => ({})
  } as DOMRect);

  const setVerticalMetrics = (element: HTMLElement, clientHeight: number, scrollHeight: number) => {
    Object.defineProperties(element, {
      clientHeight: { configurable: true, value: clientHeight },
      scrollHeight: { configurable: true, value: scrollHeight }
    });
  };

  it("offers exactly five insertable context kinds and never encrypted reasoning", () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const onInsert = vi.fn();
    const { container } = render(
      <ContextStream
        contexts={conversation.contexts}
        tools={document.tools}
        enabledTools={conversation.settings.enabledTools}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={onInsert}
      />
    );
    fireEvent.contextMenu(container.querySelector(".context-stream__hint")!, { clientX: 20, clientY: 20 });
    const menu = screen.getByRole("menu");
    expect(menu).toBeInTheDocument();
    expect(within(menu).getAllByRole("menuitem")).toHaveLength(5);
    expect(within(menu).getByText("系统提示词")).toBeInTheDocument();
    expect(within(menu).getByText("用户输入")).toBeInTheDocument();
    expect(within(menu).getByText("思考字段")).toBeInTheDocument();
    expect(within(menu).getByText("工具调用")).toBeInTheDocument();
    expect(within(menu).getByText("模型回复")).toBeInTheDocument();
    expect(within(menu).queryByText("添加加密思考")).not.toBeInTheDocument();
  });

  /// Legacy fold records are user contexts. New folds use host-synthesized
  /// `task_wait` tool contexts, but these archived records must continue to render
  /// as ordinary user cards without special-case filtering.
  it("renders a legacy folded agent-result notification as a plain user card", () => {
    const document = createSeedDocument();
    const notification: ContextItem = {
      id: "ctx_agent-result_0af31cde9b",
      kind: "user",
      content: "[a1 · 已完成]\n审查完成",
      createdAt: "2026-07-29T00:00:00Z"
    };
    const { container } = render(
      <ContextStream
        contexts={[notification]}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />
    );
    const card = container.querySelector(".context-card--user");
    expect(card).not.toBeNull();
    expect(card).toHaveTextContent("审查完成");
    expect(card).toHaveTextContent("[a1 · 已完成]");
  });

  it("renders an answered ask_user as one editable and deletable message group", () => {
    const document = createSeedDocument();
    const onEdit = vi.fn();
    const onDelete = vi.fn();
    const onEditQuestion = vi.fn();
    const onDeleteQuestion = vi.fn();
    const ask: ToolContext = {
      id: "ask-history",
      kind: "tool",
      toolName: "ask_user",
      input: {
        questions: [{
          question: "采用哪个方案？",
          header: "方案",
          options: [
            { label: "方案 A", description: "保持改动最小" },
            { label: "方案 B", description: "完整重构" }
          ],
          multiSelect: false
        }]
      },
      result: {
        success: true,
        output: ASK_USER_PENDING_OUTPUT,
        executedAt: "2026-07-23T00:00:00Z",
        durationMs: 0
      },
      createdAt: "2026-07-23T00:00:00Z"
    };
    const answer: ContextItem = {
      id: "ask-history-answer",
      kind: "user",
      content: 'User has answered your questions: "采用哪个方案？"="方案 A"',
      createdAt: "2026-07-23T00:00:01Z"
    };
    const { container } = render(
      <ContextStream
        contexts={[ask, answer]}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
        onEdit={onEdit}
        onDelete={onDelete}
        onEditQuestion={onEditQuestion}
        onDeleteQuestion={onDeleteQuestion}
      />
    );

    expect(screen.getByText("你的回答").closest(".question-history__output")).toHaveTextContent("方案 A");
    expect(container.querySelector(".context-card--user")).toBeNull();
    const card = screen.getByText("你的回答").closest(".question-history") as HTMLElement;
    expect(within(card).getAllByRole("button")).toHaveLength(2);
    expect(within(card).queryByRole("button", { name: "编辑提问" })).not.toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: "编辑回答" })).not.toBeInTheDocument();
    fireEvent.click(within(card).getByRole("button", { name: "编辑提问与回答" }));
    fireEvent.click(within(card).getByRole("button", { name: "删除整条提问消息" }));
    expect(onEditQuestion).toHaveBeenCalledTimes(1);
    expect(onEditQuestion).toHaveBeenCalledWith(ask, answer);
    expect(onDeleteQuestion).toHaveBeenCalledTimes(1);
    expect(onDeleteQuestion).toHaveBeenCalledWith(ask, answer);
    expect(onEdit).not.toHaveBeenCalled();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it("keeps a real composer turn anchor outside an unanswered ask_user group", () => {
    const document = createSeedDocument();
    const firstUser: ContextItem = {
      id: "turn-anchor-before-question",
      kind: "user",
      content: "先问我一个问题",
      createdAt: "2026-07-24T00:00:00Z"
    };
    const ask: ToolContext = {
      id: "ask-before-new-turn",
      kind: "tool",
      toolName: "ask_user",
      input: {
        questions: [{
          question: "要继续旧任务吗？",
          header: "旧任务",
          options: [{ label: "继续", description: "继续旧任务" }],
          multiSelect: false
        }]
      },
      result: {
        success: true,
        output: ASK_USER_PENDING_OUTPUT,
        executedAt: "2026-07-24T00:00:01Z",
        durationMs: 0
      },
      createdAt: "2026-07-24T00:00:01Z"
    };
    const nextUser: ContextItem = {
      id: "real-composer-next-turn",
      kind: "user",
      content: "这是从主输入框发起的新任务",
      createdAt: "2026-07-24T00:01:00Z"
    };
    const turns: ConversationTurn[] = [{
      id: "old-turn",
      requestId: "old-run",
      anchorContextId: firstUser.id,
      modelId: "test-model",
      startedAt: firstUser.createdAt,
      endedAt: nextUser.createdAt,
      durationMs: 1_000,
      status: "interrupted",
      contextIds: [ask.id],
      usage: {},
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: true,
      userToggled: false
    }, {
      id: "new-turn",
      requestId: "new-run",
      anchorContextId: nextUser.id,
      modelId: "test-model",
      startedAt: nextUser.createdAt,
      durationMs: 0,
      status: "running",
      contextIds: [],
      usage: {},
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: true,
      userToggled: false
    }];

    const { container } = render(
      <ContextStream
        contexts={[firstUser, ask, nextUser]}
        turns={turns}
        onToggleTurn={vi.fn()}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
      />
    );

    const nextUserCard = screen.getByText(nextUser.content).closest(".context-card");
    expect(nextUserCard).toHaveClass("context-card--user");
    expect(nextUserCard?.closest("[data-turn-id]")).toBeNull();
    expect(screen.getByText("要继续旧任务吗？").closest("[data-turn-id]"))
      .toHaveAttribute("data-turn-id", "old-turn");
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(2);
  });

  it("draws a turn header only while it has a body, a live run, or a failure to show", () => {
    const anchor = {
      id: "user-anchor",
      kind: "user" as const,
      content: "提问",
      createdAt: "2026-07-20T00:00:00Z"
    };
    const baseTurn = {
      requestId: "run-1",
      anchorContextId: anchor.id,
      modelId: "test-model",
      startedAt: anchor.createdAt,
      durationMs: 0,
      // The reply this turn used to own has already been deleted from contexts.
      contextIds: ["assistant-deleted"],
      usage: {},
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: true,
      userToggled: false
    };

    const { container, rerender } = render(
      <ContextStream
        contexts={[anchor]}
        turns={[{ ...baseTurn, id: "finished-turn", status: "completed" } as ConversationTurn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
      />
    );
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(0);
    expect(screen.getByText(anchor.content)).toBeInTheDocument();

    // A turn that is still streaming keeps its header: it hosts the waiting
    // indicator even before its first message arrives.
    rerender(
      <ContextStream
        contexts={[anchor]}
        turns={[{ ...baseTurn, id: "running-turn", status: "running", contextIds: [] } as ConversationTurn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
        streaming
      />
    );
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);

    // A turn with nothing to disclose and no notice to host draws no header:
    // "stopped after 3s" over an empty body reads as a bug rather than as a
    // record of the round. A stop before the first message is dropped from
    // storage outright, so what actually reaches this branch is a round whose
    // own messages the user deleted.
    rerender(
      <ContextStream
        contexts={[anchor]}
        turns={[{ ...baseTurn, id: "stopped-turn", status: "interrupted", contextIds: [] } as ConversationTurn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
      />
    );
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(0);
    expect(screen.getByText(anchor.content)).toBeInTheDocument();

    // The same turn keeps its header once it carries a failure notice, which is
    // the one thing an otherwise empty round still has to say.
    rerender(
      <ContextStream
        contexts={[anchor]}
        turns={[{
          ...baseTurn,
          id: "failed-turn",
          status: "interrupted",
          contextIds: [],
          error: {
            message: "无法连接 API",
            providerName: "Test Provider",
            modelName: "test-model",
            at: "2026-07-20T00:00:12Z"
          }
        } as ConversationTurn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
      />
    );
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    expect(screen.getByText("无法连接 API")).toBeInTheDocument();
  });

  it("keeps a turn header over the messages it still owns after its anchor is deleted", () => {
    // The user deleted the message that started this round; its reply survived.
    const reply = {
      id: "assistant-orphaned",
      kind: "assistant" as const,
      content: "被留下来的回复",
      createdAt: "2026-07-20T00:00:01Z"
    };
    const orphaned: ConversationTurn = {
      id: "orphaned-turn",
      requestId: "run-1",
      anchorContextId: "user-anchor-deleted",
      modelId: "test-model",
      startedAt: "2026-07-20T00:00:00Z",
      endedAt: "2026-07-20T00:00:12Z",
      durationMs: 12_000,
      status: "interrupted",
      contextIds: [reply.id],
      usage: { inputTokens: 100, cachedInputTokens: 20, outputTokens: 50 },
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: true,
      userToggled: false
    };

    const { container } = render(
      <ContextStream
        contexts={[reply]}
        turns={[orphaned]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
      />
    );

    // Losing the anchor must not cost the round its duration and token counts,
    // nor make its reply look like part of whatever round precedes it.
    expect(screen.getByText(reply.content).closest("[data-turn-id]"))
      .toHaveAttribute("data-turn-id", "orphaned-turn");
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    expect(screen.getByRole("button", { name: /test-model 在 12s 后中止/ }))
      .toHaveAccessibleName(/100/);
  });

  it("opens the insert menu from an actually empty conversation surface", () => {
    const document = createSeedDocument();
    const onInsert = vi.fn();
    const { container } = render(
      <ContextStream
        contexts={[]}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={onInsert}
      />
    );
    fireEvent.contextMenu(container.querySelector(".empty-state")!, { clientX: 32, clientY: 180 });
    const menu = screen.getByRole("menu", { name: "添加上下文" });
    expect(menu).toBeInTheDocument();
    fireEvent.click(within(menu).getByRole("menuitem", { name: /用户输入.*插入一条用户消息/ }));
    expect(onInsert).toHaveBeenCalledWith(0, "user");
  });

  it("regroups tools after deleting a visible boundary and maps insertion UI across hidden anchors", () => {
    const firstTool: ToolContext = {
      id: "edited-group-first",
      kind: "tool",
      toolName: "read",
      round: 1,
      modelTurnId: "stale-turn-a",
      input: { path: "a.txt" },
      result: { success: true, output: "a", executedAt: "2026-07-22T00:00:00Z", durationMs: 1 },
      createdAt: "2026-07-22T00:00:00Z"
    };
    const secondTool: ToolContext = {
      id: "edited-group-second",
      kind: "tool",
      toolName: "find",
      input: { pattern: "b" },
      result: { success: true, output: "b", executedAt: "2026-07-22T00:00:04Z", durationMs: 1 },
      createdAt: "2026-07-22T00:00:04Z"
    };
    const boundary = {
      id: "edited-group-boundary",
      kind: "assistant" as const,
      content: "删除我",
      createdAt: "2026-07-22T00:00:01Z"
    };
    const hiddenAssistant = {
      id: "edited-group-empty-assistant",
      kind: "assistant" as const,
      content: "",
      createdAt: "2026-07-22T00:00:02Z"
    };
    const hiddenReasoning = {
      id: "edited-group-empty-reasoning",
      kind: "reasoning" as const,
      content: "",
      createdAt: "2026-07-22T00:00:03Z"
    };
    const onInsert = vi.fn();
    const renderStream = (includeBoundary: boolean) => (
      <ContextStream
        contexts={[
          firstTool,
          ...(includeBoundary ? [boundary] : []),
          hiddenAssistant,
          hiddenReasoning,
          secondTool
        ]}
        tools={[]}
        enabledTools={[]}
        onInsert={onInsert}
      />
    );
    const { container, rerender } = render(renderStream(true));
    expect(container.querySelectorAll(".tool-call-group")).toHaveLength(2);

    rerender(renderStream(false));
    expect(container.querySelectorAll(".tool-call-group")).toHaveLength(1);
    const firstRow = container.querySelector<HTMLElement>('[data-context-id="edited-group-first"]')!;
    vi.spyOn(firstRow, "getBoundingClientRect").mockReturnValue(box(100, 100));
    fireEvent.contextMenu(firstRow, { clientX: 40, clientY: 175 });

    const insertion = container.querySelector<HTMLElement>(".tool-call-group__insertion")!;
    expect(insertion).toBeInTheDocument();
    expect(insertion.nextElementSibling).toHaveAttribute("data-context-id", "edited-group-second");
    fireEvent.click(screen.getByRole("menuitem", { name: /用户输入.*插入一条用户消息/ }));
    expect(onInsert).toHaveBeenCalledWith(1, "user");
  });

  it("treats provenance-free empty anchors like an empty timeline without hiding stream activity", () => {
    const anchors = [
      {
        id: "empty-assistant-anchor",
        kind: "assistant" as const,
        content: "",
        createdAt: "2026-07-22T00:00:00Z"
      },
      {
        id: "empty-reasoning-anchor",
        kind: "reasoning" as const,
        content: "",
        createdAt: "2026-07-22T00:00:01Z"
      }
    ];
    const { container, rerender } = render(
      <ContextStream contexts={anchors} tools={[]} enabledTools={[]} />
    );
    expect(screen.getByText("这段对话还没有消息")).toBeInTheDocument();
    expect(container.querySelector(".context-card")).not.toBeInTheDocument();

    rerender(<ContextStream contexts={anchors} tools={[]} enabledTools={[]} streaming />);
    expect(screen.getByText("这段对话还没有消息")).toBeInTheDocument();
    expect(container.querySelectorAll('[data-stream-waiting="true"]')).toHaveLength(1);
  });

  it("uses the clicked half of a context card to insert before or after it", () => {
    const document = createSeedDocument();
    const context = document.workspaces[0].conversations[0].contexts[0];
    const onInsert = vi.fn();
    const { container } = render(
      <ContextStream
        contexts={[context]}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={onInsert}
      />
    );
    const card = container.querySelector<HTMLElement>(".context-card")!;
    vi.spyOn(card, "getBoundingClientRect").mockReturnValue(box(100, 100));

    fireEvent.contextMenu(card, { clientX: 40, clientY: 125 });
    fireEvent.click(screen.getByRole("menuitem", { name: /模型回复.*插入一条模型回复/ }));
    expect(onInsert).toHaveBeenLastCalledWith(0, "assistant");

    fireEvent.contextMenu(card, { clientX: 40, clientY: 175 });
    fireEvent.click(screen.getByRole("menuitem", { name: /模型回复.*插入一条模型回复/ }));
    expect(onInsert).toHaveBeenLastCalledWith(1, "assistant");
  });

  it("maps a right-click in timeline whitespace to the nearest insertion gap", () => {
    const document = createSeedDocument();
    const contexts = document.workspaces[0].conversations[0].contexts.slice(0, 2);
    const onInsert = vi.fn();
    const { container } = render(
      <ContextStream
        contexts={contexts}
        tools={document.tools}
        enabledTools={document.tools.map((tool) => tool.name)}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={onInsert}
      />
    );
    const cards = container.querySelectorAll<HTMLElement>(".context-card");
    vi.spyOn(cards[0], "getBoundingClientRect").mockReturnValue(box(0, 80));
    vi.spyOn(cards[1], "getBoundingClientRect").mockReturnValue(box(120, 80));

    fireEvent.contextMenu(container.querySelector(".context-stream")!, { clientX: 30, clientY: 100 });
    fireEvent.click(screen.getByRole("menuitem", { name: /用户输入.*插入一条用户消息/ }));
    expect(onInsert).toHaveBeenCalledWith(1, "user");
  });

  it("does not restart follow-output after deleting the tail and keeps the next context menu open", () => {
    const first = { id: "delete-tail-user", kind: "user" as const, content: "保留", createdAt: "2026-07-21T00:00:00Z" };
    const last = { id: "delete-tail-system", kind: "system" as const, content: "删除", createdAt: "2026-07-21T00:00:01Z" };
    const onInsert = vi.fn();
    const frames: FrameRequestCallback[] = [];
    const requestFrame = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const { container, rerender } = render(
      <ContextStream contexts={[first, last]} tools={[]} enabledTools={[]} onInsert={onInsert} />
    );
    frames.splice(0).forEach((callback) => callback(0));
    requestFrame.mockClear();

    rerender(<ContextStream contexts={[first]} tools={[]} enabledTools={[]} onInsert={onInsert} />);
    expect(requestFrame).not.toHaveBeenCalled();

    fireEvent.contextMenu(container.querySelector(".context-stream__hint")!, { clientX: 40, clientY: 220 });
    const menu = screen.getByRole("menu", { name: "添加上下文" });
    expect(menu).toBeInTheDocument();
    fireEvent.click(within(menu).getByRole("menuitem", { name: /模型回复.*插入一条模型回复/ }));
    expect(onInsert).toHaveBeenCalledWith(1, "assistant");
    requestFrame.mockRestore();
  });

  it("still follows an appended tail with one instant animation-frame update", () => {
    const first = { id: "append-user", kind: "user" as const, content: "问题", createdAt: "2026-07-21T00:00:00Z" };
    const last = { id: "append-system", kind: "system" as const, content: "新增", createdAt: "2026-07-21T00:00:01Z" };
    const frames: FrameRequestCallback[] = [];
    const requestFrame = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const { container, rerender } = render(
      <ContextStream contexts={[first]} tools={[]} enabledTools={[]} onInsert={vi.fn()} />
    );
    frames.splice(0).forEach((callback) => callback(0));
    requestFrame.mockClear();
    const scroller = container.querySelector<HTMLElement>(".context-scroll")!;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 640 });

    rerender(<ContextStream contexts={[first, last]} tools={[]} enabledTools={[]} onInsert={vi.fn()} />);
    expect(requestFrame).toHaveBeenCalledTimes(1);
    frames.splice(0).forEach((callback) => callback(1));
    expect(scroller.scrollTop).toBe(640);
    requestFrame.mockRestore();
  });

  it("cancels queued follow-output when a context menu opens before the frame", () => {
    const first = { id: "queued-menu-user", kind: "user" as const, content: "问题", createdAt: "2026-07-21T00:00:00Z" };
    const last = { id: "queued-menu-system", kind: "system" as const, content: "补充", createdAt: "2026-07-21T00:00:01Z" };
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrameId = 0;
    const requestFrame = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      const id = ++nextFrameId;
      frames.set(id, callback);
      return id;
    });
    const cancelFrame = vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
      frames.delete(id);
    });
    const { container, rerender } = render(
      <ContextStream timelineId="conversation-a" contexts={[first]} tools={[]} enabledTools={[]} onInsert={vi.fn()} />
    );
    for (const [id, callback] of [...frames]) {
      frames.delete(id);
      callback(0);
    }
    requestFrame.mockClear();

    rerender(
      <ContextStream timelineId="conversation-a" contexts={[first, last]} tools={[]} enabledTools={[]} onInsert={vi.fn()} />
    );
    expect(frames.size).toBe(1);
    fireEvent.contextMenu(container.querySelector(".context-stream__hint")!, { clientX: 40, clientY: 220 });

    expect(cancelFrame).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(0);
    expect(screen.getByRole("menu", { name: "添加上下文" })).toBeInTheDocument();
    requestFrame.mockRestore();
    cancelFrame.mockRestore();
  });

  it("does not treat a different conversation timeline as appended output", () => {
    const first = { id: "switch-a", kind: "user" as const, content: "对话 A", createdAt: "2026-07-21T00:00:00Z" };
    const other = { id: "switch-b-1", kind: "user" as const, content: "对话 B", createdAt: "2026-07-21T00:00:01Z" };
    const otherAnswer = { id: "switch-b-2", kind: "system" as const, content: "补充 B", createdAt: "2026-07-21T00:00:02Z" };
    const frames: FrameRequestCallback[] = [];
    const requestFrame = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const { rerender } = render(
      <ContextStream timelineId="conversation-a" contexts={[first]} tools={[]} enabledTools={[]} />
    );
    frames.splice(0).forEach((callback) => callback(0));
    requestFrame.mockClear();

    rerender(
      <ContextStream timelineId="conversation-b" contexts={[other, otherAnswer]} tools={[]} enabledTools={[]} />
    );

    expect(requestFrame).not.toHaveBeenCalled();
    requestFrame.mockRestore();
  });

  it("skips disabled tool insertion during keyboard navigation and restores its trigger", async () => {
    const document = createSeedDocument();
    const context = document.workspaces[0].conversations[0].contexts[0];
    const { container } = render(
      <ContextStream
        contexts={[context]}
        tools={document.tools}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    const card = container.querySelector<HTMLElement>(".context-card")!;
    card.focus();
    fireEvent.keyDown(card, { key: "F10", shiftKey: true });
    const menu = screen.getByRole("menu", { name: "添加上下文" });
    const assistant = within(menu).getByRole("menuitem", { name: /模型回复.*插入一条模型回复/ });
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(assistant).toHaveFocus();

    fireEvent.keyDown(menu, { key: "Escape" });
    await waitFor(() => expect(card).toHaveFocus());
  });

  it("keeps canonical reasoning editable and deletable", () => {
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const { container } = render(
      <ContextStream
        contexts={conversation.contexts}
        tools={document.tools}
        enabledTools={conversation.settings.enabledTools}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    const reasoning = container.querySelector<HTMLElement>('[data-context-id="ctx_reasoning_more"]')!;
    expect(reasoning.querySelector<HTMLButtonElement>('button[aria-label="编辑上下文"]')).toBeEnabled();
    expect(reasoning.querySelector<HTMLButtonElement>('button[aria-label="删除上下文"]')).toBeEnabled();
  });

  it("places user edit and delete actions between copy and run without message timestamps", () => {
    const contexts = [
      { id: "user-layout", kind: "user" as const, content: "用户正文", createdAt: "2026-07-11T08:20:00Z" },
      { id: "reasoning-layout", kind: "reasoning" as const, content: "思考正文", createdAt: "2026-07-11T08:21:00Z" },
      { id: "assistant-layout", kind: "assistant" as const, content: "回复正文", createdAt: "2026-07-11T08:22:00Z" }
    ];
    const { container } = render(
      <ContextStream
        contexts={contexts}
        tools={[]}
        enabledTools={[]}
        onBranchFrom={vi.fn()}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const user = container.querySelector<HTMLElement>('[data-context-id="user-layout"]')!;
    expect(user.querySelector(".context-card__header")).not.toBeInTheDocument();
    expect(user.querySelector(":scope > .context-actions")).not.toBeInTheDocument();
    expect(within(user).queryByText("用户输入")).not.toBeInTheDocument();
    expect(user.querySelector(".context-card__time")).not.toBeInTheDocument();
    const toolbar = user.querySelector<HTMLElement>(".user-message-toolbar")!;
    expect(within(toolbar).getAllByRole("button").map((button) => button.getAttribute("aria-label"))).toEqual([
      "复制用户消息",
      "编辑上下文",
      "删除上下文",
      "从此消息分支"
    ]);

    const reasoning = container.querySelector<HTMLElement>('[data-context-id="reasoning-layout"]')!;
    const reasoningHeading = reasoning.querySelector(".reasoning-content__heading")!;
    expect(reasoningHeading.firstElementChild).toHaveClass("reasoning-content__summary");
    expect(reasoningHeading.firstElementChild?.nextElementSibling).toHaveClass("context-actions");

    const assistant = container.querySelector<HTMLElement>('[data-context-id="assistant-layout"]')!;
    const assistantKind = assistant.querySelector(".context-card__kind")!;
    expect(assistantKind).toHaveTextContent("模型回复");
    expect(assistantKind.nextElementSibling).toHaveClass("context-actions");
    expect(assistant.querySelector(".context-card__time")).not.toBeInTheDocument();
  });

  it("uses the normal assistant card while streaming and disables its mutation actions", () => {
    const assistant = {
      id: "assistant-streaming-layout",
      kind: "assistant" as const,
      content: "正在逐段返回",
      streaming: true,
      createdAt: "2026-07-20T00:00:00Z"
    };
    const { container } = render(
      <ContextStream
        contexts={[assistant]}
        tools={[]}
        enabledTools={[]}
        streaming
        timelineMutationLocked
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const card = container.querySelector<HTMLElement>('[data-context-id="assistant-streaming-layout"]')!;
    expect(within(card).getByText("模型回复")).toBeInTheDocument();
    expect(within(card).queryByText("模型回复 · 正在生成")).not.toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "编辑上下文" })).toBeDisabled();
    expect(within(card).getByRole("button", { name: "删除上下文" })).toBeDisabled();
    expect(card).not.toHaveClass("context-card--streaming");
    expect(card.querySelector(".streaming-cursor")).not.toBeInTheDocument();
  });

  it("branches only from ordinary editable user messages and exposes the disabled reason", () => {
    const user = { id: "user-run", kind: "user" as const, content: "从这里开始", createdAt: "2026-07-20T00:00:00Z" };
    const assistant = { id: "assistant-run", kind: "assistant" as const, content: "旧回复", createdAt: "2026-07-20T00:00:01Z" };
    const onBranchFrom = vi.fn();
    const { rerender } = render(
      <ContextStream
        contexts={[user, assistant]}
        tools={[]}
        enabledTools={[]}
        onBranchFrom={onBranchFrom}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const branch = screen.getByRole("button", { name: "从此消息分支" });
    fireEvent.click(branch);
    expect(onBranchFrom).toHaveBeenCalledWith(user);
    expect(branch).toHaveAttribute("title", "在新对话中继续这条消息");

    rerender(
      <ContextStream
        contexts={[user, assistant]}
        tools={[]}
        enabledTools={[]}
        onBranchFrom={onBranchFrom}
        branchFromDisabledReason="工作区正在删除，无法创建分支"
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    expect(screen.getByRole("button", { name: "从此消息分支" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "从此消息分支" })).toHaveAttribute("title", "工作区正在删除，无法创建分支");

    rerender(
      <ContextStream
        contexts={[user]}
        tools={[]}
        enabledTools={[]}
        readOnly
        onBranchFrom={onBranchFrom}
      />
    );
    expect(screen.queryByRole("button", { name: "从此消息分支" })).not.toBeInTheDocument();
  });

  it("copies an ordinary user message and confirms success", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const user = { id: "user-copy", kind: "user" as const, content: "第一行\n第二行", createdAt: "2026-07-20T00:00:00Z" };

    render(
      <ContextStream
        contexts={[user]}
        tools={[]}
        enabledTools={[]}
        readOnly
      />
    );

    fireEvent.click(screen.getByRole("button", { name: "复制用户消息" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("第一行\n第二行"));
    expect(screen.getByRole("button", { name: "已复制" })).not.toHaveTextContent("已复制");
  });

  it("does not offer an empty text copy action for an image-only user message", () => {
    render(
      <ContextStream
        contexts={[{
          id: "user-image-only",
          kind: "user",
          content: "",
          images: [{
            id: "a".repeat(64),
            name: "image.png",
            mime: "image/png",
            width: 1,
            height: 1,
            bytes: 1
          }],
          createdAt: "2026-07-20T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        readOnly
      />
    );

    expect(screen.getByRole("list", { name: "1 张图片" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "复制用户消息" })).not.toBeInTheDocument();
  });

  it("navigates stable sibling positions below their fork message", () => {
    const user = { id: "user-fork", kind: "user" as const, content: "选择分支", createdAt: "2026-07-20T00:00:00Z" };
    const onSelectBranch = vi.fn();
    render(
      <ContextStream
        contexts={[user]}
        tools={[]}
        enabledTools={[]}
        onBranchFrom={vi.fn()}
        branchNavigations={{
          "user-fork": { activeIndex: 1, branchIds: ["first", "second", "third"] }
        }}
        onSelectBranch={onSelectBranch}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByText("2 / 3")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "上一个分支" }));
    expect(onSelectBranch).toHaveBeenLastCalledWith("user-fork", "first");
    fireEvent.click(screen.getByRole("button", { name: "下一个分支" }));
    expect(onSelectBranch).toHaveBeenLastCalledWith("user-fork", "third");
  });

  /**
   * Encrypted reasoning may have no summary text but still consumes time and
   * tokens. Its card is the only visible evidence of that reasoning. Records
   * without `form` predate the field and use the empty-content fallback.
   */
  it("renders a summary-less reasoning round as a static line carrying only its time and tokens", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-encrypted-only",
          kind: "reasoning" as const,
          durationMs: 18_000,
          tokens: 1_240,
          createdAt: "2026-08-29T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByText("思考了")).toBeInTheDocument();
    expect(container.querySelector(".reasoning-content__meta")?.textContent).toBe("18s · 1.2k");
    // Empty content leaves nothing to disclose.
    expect(screen.queryByRole("button", { name: "思考了" })).not.toBeInTheDocument();
    expect(container.querySelector(".reasoning-content__region")).toBeNull();
  });

  /**
   * Encrypted reasoning is deletable but not editable: its text never reached
   * the client, and invented text would become part of the next-round history.
   */
  it("gives an encrypted reasoning card a delete action and no edit action", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-encrypted",
          kind: "reasoning" as const,
          form: "encrypted" as const,
          durationMs: 18_000,
          tokens: 1_240,
          createdAt: "2026-08-29T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const card = container.querySelector<HTMLElement>('[data-context-id="reasoning-encrypted"]')!;
    expect(card.querySelector('button[aria-label="编辑上下文"]')).toBeNull();
    expect(card.querySelector<HTMLButtonElement>('button[aria-label="删除上下文"]')).toBeEnabled();
  });

  /** Summary text returned with encrypted reasoning remains readable; encryption
   * affects edit permission, not visibility. */
  /**
   * A subagent transcript's reasoning cards carry no resolved form, so an
   * encrypted-only round reaches the renderer as an empty streaming card. It has
   * neither a body to disclose nor a duration to state; the stream indicator
   * beside the cat is its surface until the round closes.
   */
  it("draws no card for encrypted reasoning that is still arriving", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-live-encrypted",
          kind: "reasoning" as const,
          content: "",
          streaming: true,
          createdAt: "2026-09-04T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        streaming
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(container.querySelector('[data-context-id="reasoning-live-encrypted"]')).toBeNull();
    expect(container.querySelector(".reasoning-content")).toBeNull();
    // The indicator still stands in for the round.
    expect(container.querySelector("[data-stream-waiting]")).toBeInTheDocument();
  });

  /** Reasoning that is arriving as readable text keeps its own live card,
   * whatever form produced it. */
  it("keeps the live card for reasoning that is arriving as text", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-live-summary",
          kind: "reasoning" as const,
          form: "encrypted" as const,
          content: "先读文件",
          streaming: true,
          createdAt: "2026-09-04T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        streaming
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(container.querySelector('[data-context-id="reasoning-live-summary"]')).toBeInTheDocument();
    expect(container.querySelector(".reasoning-content__body")).toHaveTextContent("先读文件");
  });

  it("still expands an encrypted reasoning card that came back with summary text", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-encrypted-summary",
          kind: "reasoning" as const,
          form: "encrypted" as const,
          content: "先读文件，再决定改哪里。",
          createdAt: "2026-08-29T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByRole("button", { name: "思考过程" })).toBeInTheDocument();
    const card = container.querySelector<HTMLElement>('[data-context-id="reasoning-encrypted-summary"]')!;
    expect(card.querySelector('button[aria-label="编辑上下文"]')).toBeNull();
  });

  /**
   * A plaintext record with no text has the opposite form from an encrypted one.
   * Its producing model attribute, not empty content, permits expansion and editing.
   */
  it("keeps an empty plaintext reasoning card expandable and editable", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-empty-plaintext",
          kind: "reasoning" as const,
          form: "plaintext" as const,
          durationMs: 18_000,
          tokens: 1_240,
          createdAt: "2026-08-29T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByRole("button", { name: "思考了" })).toBeInTheDocument();
    const card = container.querySelector<HTMLElement>('[data-context-id="reasoning-empty-plaintext"]')!;
    expect(card.querySelector<HTMLButtonElement>('button[aria-label="编辑上下文"]')).toBeEnabled();
    expect(card.querySelector<HTMLButtonElement>('button[aria-label="删除上下文"]')).toBeEnabled();
  });

  /** Metadata stays outside the button because changing values would alter its accessible name. */
  it("keeps reasoning metadata out of the disclosure button's accessible name", () => {
    const { container } = render(
      <ContextStream
        contexts={[{
          id: "reasoning-with-meta",
          kind: "reasoning" as const,
          content: "先读文件，再决定改哪里。",
          durationMs: 84_000,
          tokens: 512,
          createdAt: "2026-08-29T00:00:00Z"
        }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByRole("button", { name: "思考了" })).toBeInTheDocument();
    expect(container.querySelector(".reasoning-content__meta")?.textContent).toBe("1m24s · 512");
  });

  it("moves completed reasoning between collapsed, preview, and full views and releases collapsed Markdown", async () => {
    const reasoning = {
      id: "reasoning-three-state",
      kind: "reasoning" as const,
      content: "第一行\n第二行\n第三行\n第四行\n第五行",
      createdAt: "2026-07-11T00:00:00Z"
    };
    const { container } = render(
      <ContextStream
        contexts={[reasoning]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const summary = screen.getByRole("button", { name: "思考过程" });
    const region = container.querySelector<HTMLElement>(".reasoning-content__region")!;
    expect(summary).toHaveAttribute("aria-expanded", "false");
    expect(region).toHaveClass("collapse-region--closed");
    expect(region).toHaveAttribute("aria-hidden", "true");
    expect(container.querySelector(".reasoning-content__body")).not.toBeInTheDocument();

    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(region).not.toHaveClass("collapse-region--closed");
    expect(region).not.toHaveAttribute("aria-hidden");
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");
    const body = container.querySelector<HTMLElement>(".reasoning-content__body")!;
    expect(body).toHaveTextContent("第一行 第二行 第三行 第四行 第五行");
    setVerticalMetrics(body, 80, 120);
    fireEvent(window, new Event("resize"));

    fireEvent.click(screen.getByRole("button", { name: "展开" }));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "full");
    expect(screen.getByRole("button", { name: "收起" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "收起" }));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");

    fireEvent.click(screen.getByRole("button", { name: "展开" }));
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "false");
    expect(region).toHaveClass("collapse-region--closed");
    expect(container.querySelector(".reasoning-content__body")).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector(".reasoning-content__body")).not.toBeInTheDocument());

    fireEvent.click(summary);
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");
    expect(container.querySelector(".reasoning-content__body")).toBeInTheDocument();
  });

  it("keeps large sets of collapsed reasoning free of Markdown DOM and resize observers", async () => {
    let activeObservers = 0;
    class ResizeObserverMock {
      private observing = false;
      observe() {
        if (this.observing) return;
        this.observing = true;
        activeObservers += 1;
      }
      disconnect() {
        if (!this.observing) return;
        this.observing = false;
        activeObservers -= 1;
      }
      unobserve() { this.disconnect(); }
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    const contexts = Array.from({ length: 120 }, (_, index) => ({
      id: `collapsed-reasoning-${index}`,
      kind: "reasoning" as const,
      content: `第 ${index + 1} 段思考\n`.repeat(80),
      createdAt: "2026-07-21T00:00:00Z"
    }));
    const { container } = render(
      <ContextStream contexts={contexts} tools={[]} enabledTools={[]} onInsert={vi.fn()} />
    );

    expect(container.querySelectorAll(".reasoning-content__body")).toHaveLength(0);
    expect(activeObservers).toBe(0);

    const first = screen.getAllByRole("button", { name: "思考过程" })[0];
    fireEvent.click(first);
    expect(container.querySelectorAll(".reasoning-content__body")).toHaveLength(1);
    await waitFor(() => expect(activeObservers).toBeGreaterThan(0));

    fireEvent.click(first);
    await waitFor(() => {
      expect(container.querySelectorAll(".reasoning-content__body")).toHaveLength(0);
      expect(activeObservers).toBe(0);
    });
  });

  it("uses the shared Markdown and math renderer for replies and visible reasoning", () => {
    const contexts = [
      {
        id: "reasoning-markdown",
        kind: "reasoning" as const,
        content: "**推导**：\\(x^2\\)",
        createdAt: "2026-07-11T00:00:00Z"
      },
      {
        id: "assistant-markdown",
        kind: "assistant" as const,
        content: "## 结论\n\n$$x=\\frac{-b}{2a}$$",
        createdAt: "2026-07-11T00:00:01Z"
      }
    ];
    const { container } = render(
      <ContextStream
        contexts={contexts}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByRole("heading", { name: "结论" })).toBeInTheDocument();
    expect(container.querySelector("[data-context-id='assistant-markdown'] .katex-display")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "思考过程" }));
    expect(container.querySelector("[data-context-id='reasoning-markdown'] strong")).toHaveTextContent("推导");
    expect(container.querySelector("[data-context-id='reasoning-markdown'] .katex")).toBeInTheDocument();
  });

  it("renders provider citations as a chip row under assistant prose", () => {
    // Source labels degrade from title to host to ID. Sources without URLs must
    // not appear as external links, and source-free legacy cards have no landmark.
    const cited = {
      id: "assistant-citations",
      kind: "assistant" as const,
      content: "带引用的回复",
      sources: [
        { id: "s1", url: "https://example.com/a", title: "示例来源" },
        { id: "s2", url: "https://docs.example.org/b" },
        { id: "s3" },
        { id: "unsafe", url: "javascript:alert(1)" }
      ],
      createdAt: "2026-09-01T00:00:00Z"
    };
    const plain = {
      id: "assistant-no-citations",
      kind: "assistant" as const,
      content: "没有引用的回复",
      createdAt: "2026-09-01T00:00:01Z"
    };
    const { container } = render(
      <ContextStream contexts={[cited, plain]} tools={[]} enabledTools={[]} />
    );

    const citedCard = container.querySelector<HTMLElement>('[data-context-id="assistant-citations"]')!;
    const sources = citedCard.querySelector<HTMLElement>(".context-card__sources")!;
    const chips = sources.querySelectorAll<HTMLElement>(".context-card__source");
    expect(sources).toBeInTheDocument();
    expect(chips).toHaveLength(4);
    expect(chips[3].tagName).toBe("SPAN");
    expect(chips[3]).not.toHaveAttribute("href");
    expect(chips[0].tagName).toBe("A");
    expect(chips[0]).toHaveAttribute("href", "https://example.com/a");
    expect(chips[0]).toHaveTextContent("示例来源");
    expect(chips[1]).toHaveTextContent("docs.example.org");
    expect(chips[2].tagName).toBe("SPAN");
    expect(chips[2]).toHaveTextContent("s3");
    expect(container.querySelector('[data-context-id="assistant-no-citations"] .context-card__sources')).toBeNull();
  });

  it("keeps Markdown and math rendered while replies and reasoning are streaming", () => {
    const contexts = [
      {
        id: "reasoning-markdown-stream",
        kind: "reasoning" as const,
        content: "**推导中**：\\(x^2\\)",
        streaming: true,
        createdAt: "2026-07-11T00:00:00Z"
      },
      {
        id: "assistant-markdown-stream",
        kind: "assistant" as const,
        content: "## 当前结论\n\n$$x=\\frac{-b}{2a}$$",
        streaming: true,
        createdAt: "2026-07-11T00:00:01Z"
      }
    ];
    const { container } = render(
      <ContextStream
        contexts={contexts}
        tools={[]}
        enabledTools={[]}
        streaming
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByRole("heading", { name: "当前结论" })).toBeInTheDocument();
    expect(container.querySelector("[data-context-id='assistant-markdown-stream'] .katex-display")).toBeInTheDocument();
    expect(container.querySelector("[data-context-id='reasoning-markdown-stream'] strong")).toHaveTextContent("推导中");
    expect(container.querySelector("[data-context-id='reasoning-markdown-stream'] .katex")).toBeInTheDocument();
  });

  it("only renders preview expansion when the four-line clamp actually overflows and remeasures changes", async () => {
    let resizeCallback: ResizeObserverCallback | null = null;
    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);

    const reasoning = {
      id: "reasoning-overflow",
      kind: "reasoning" as const,
      createdAt: "2026-07-11T00:00:00Z"
    };
    const renderReasoning = (content: string) => (
      <ContextStream
        contexts={[{ ...reasoning, content }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    const { container, rerender } = render(renderReasoning("短内容"));
    fireEvent.click(screen.getByRole("button", { name: "思考过程" }));
    const body = container.querySelector<HTMLElement>(".reasoning-content__body")!;
    const panel = container.querySelector<HTMLElement>(".reasoning-content__panel")!;

    setVerticalMetrics(body, 80, 80);
    fireEvent(window, new Event("resize"));
    fireEvent.pointerMove(panel);
    expect(screen.queryByRole("button", { name: "展开" })).not.toBeInTheDocument();
    expect(panel).not.toHaveClass("reasoning-content__panel--overflowing");

    setVerticalMetrics(body, 80, 121);
    expect(resizeCallback).not.toBeNull();
    act(() => resizeCallback?.([], {} as ResizeObserver));
    expect(await screen.findByRole("button", { name: "展开" })).toBeInTheDocument();
    expect(panel).toHaveClass("reasoning-content__panel--overflowing");

    setVerticalMetrics(body, 80, 80);
    rerender(renderReasoning("更新后的短内容"));
    await waitFor(() => expect(screen.queryByRole("button", { name: "展开" })).not.toBeInTheDocument());

    setVerticalMetrics(body, 80, 140);
    fireEvent(window, new Event("resize"));
    expect(await screen.findByRole("button", { name: "展开" })).toBeInTheDocument();
  });

  it("does not carry pointer focus or visibility from collapse back to the preview control", () => {
    const reasoning = {
      id: "reasoning-pointer-focus",
      kind: "reasoning" as const,
      content: "第一行\n第二行\n第三行\n第四行\n第五行",
      createdAt: "2026-07-11T00:00:00Z"
    };
    const { container } = render(
      <ContextStream
        contexts={[reasoning]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: "思考过程" }));
    const body = container.querySelector<HTMLElement>(".reasoning-content__body")!;
    const panel = container.querySelector<HTMLElement>(".reasoning-content__panel")!;
    setVerticalMetrics(body, 80, 120);
    fireEvent(window, new Event("resize"));

    const expand = screen.getByRole("button", { name: "展开" });
    expand.focus();
    fireEvent.click(expand, { detail: 1 });
    const collapse = screen.getByRole("button", { name: "收起" });
    expect(collapse).not.toHaveFocus();

    collapse.focus();
    fireEvent.click(collapse, { detail: 1 });
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");
    expect(screen.queryByRole("button", { name: "展开" })).not.toBeInTheDocument();

    fireEvent.pointerMove(panel);
    const returnedExpand = screen.getByRole("button", { name: "展开" });
    expect(returnedExpand).not.toHaveFocus();

    returnedExpand.focus();
    fireEvent.click(returnedExpand, { detail: 0 });
    expect(screen.getByRole("button", { name: "收起" })).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "收起" }), { detail: 0 });
    expect(screen.getByRole("button", { name: "展开" })).toHaveFocus();
  });

  it("automatically keeps overflowing streaming reasoning in preview", async () => {
    const reasoning = {
      id: "reasoning-stream",
      kind: "reasoning" as const,
      content: "正在逐字出现的长思考内容",
      streaming: true,
      createdAt: "2026-07-11T00:00:00Z"
    };
    const renderStream = (content: string, streaming = true) => (
      <ContextStream
        contexts={[{ ...reasoning, content, streaming }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    const { container, rerender } = render(renderStream(reasoning.content));

    const article = container.querySelector("article")!;
    const summary = screen.getByRole("button", { name: "正在思考" });
    expect(article).toHaveAttribute("aria-busy", "true");
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "full");
    expect(screen.getByText(reasoning.content)).toBeInTheDocument();
    expect(container.querySelector(".streaming-cursor")).not.toBeInTheDocument();
    const body = container.querySelector<HTMLElement>(".reasoning-content__body")!;
    setVerticalMetrics(body, 80, 120);
    fireEvent(window, new Event("resize"));

    await waitFor(() => expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview"));
    expect(screen.getByRole("button", { name: "展开" })).toBeInTheDocument();

    rerender(renderStream(`${reasoning.content}，后续仍在继续增加。`, false));
    await waitFor(() => expect(screen.getByRole("button", { name: "思考过程" })).toHaveAttribute("aria-expanded", "true"));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");
    expect(article).not.toHaveAttribute("aria-busy");
  });

  it("never overrides a user's streaming reasoning view choice", async () => {
    const reasoning = {
      id: "reasoning-stream-user-view",
      kind: "reasoning" as const,
      streaming: true,
      createdAt: "2026-07-11T00:00:00Z"
    };
    const renderStream = (content: string, streaming = true) => (
      <ContextStream
        contexts={[{ ...reasoning, content, streaming }]}
        tools={[]}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );
    const { container, rerender } = render(renderStream("尚未超过预览长度"));
    const summary = screen.getByRole("button", { name: "正在思考" });

    fireEvent.click(summary);
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "collapsed");
    rerender(renderStream("已经超过预览长度的思考内容，继续增加也不应自动打开。"));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "collapsed");

    fireEvent.click(summary);
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "preview");
    const body = container.querySelector<HTMLElement>(".reasoning-content__body")!;
    setVerticalMetrics(body, 80, 130);
    fireEvent(window, new Event("resize"));
    const expand = await screen.findByRole("button", { name: "展开" });
    fireEvent.click(expand);
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "full");

    rerender(renderStream("已经超过预览长度的思考内容，继续增加也不应自动收回预览。更多内容。"));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "full");
    rerender(renderStream("思考完成后也保留用户选择的完整视图。", false));
    await waitFor(() => expect(screen.getByRole("button", { name: "思考过程" })).toHaveAttribute("aria-expanded", "true"));
    expect(container.querySelector(".reasoning-content")).toHaveAttribute("data-state", "full");
  });

  it("keeps a completed turn collapsed while rendering its terminal assistant outside the disclosure", () => {
    const contexts: ContextItem[] = [
      {
        id: "turn-completed-user",
        kind: "user",
        content: "检查完成状态",
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "turn-completed-reasoning",
        kind: "reasoning",
        content: "先检查中间结果",
        createdAt: "2026-07-24T00:00:01Z"
      },
      {
        id: "turn-completed-final",
        kind: "assistant",
        content: "这是最终回复",
        createdAt: "2026-07-24T00:01:01Z"
      }
    ];
    const turn: ConversationTurn = {
      id: "turn-completed",
      requestId: "request-completed",
      anchorContextId: "turn-completed-user",
      modelId: "model-completed",
      startedAt: "2026-07-24T00:00:00Z",
      endedAt: "2026-07-24T00:01:01Z",
      durationMs: 61_000,
      status: "completed",
      contextIds: ["turn-completed-reasoning", "turn-completed-final"],
      usage: {
        inputTokens: 120,
        cachedInputTokens: 40,
        outputTokens: 12
      },
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: false,
      userToggled: false
    };
    const onToggleTurn = vi.fn();
    const { container } = render(
      <ContextStream
        contexts={contexts}
        turns={[turn]}
        onToggleTurn={onToggleTurn}
        tools={[]}
        enabledTools={[]}
        onInsert={vi.fn()}
      />
    );

    const disclosure = container.querySelector<HTMLElement>('[data-turn-id="turn-completed"]')!;
    const toggle = disclosure.querySelector<HTMLButtonElement>(".conversation-turn-disclosure__toggle")!;
    const body = disclosure.querySelector<HTMLElement>(".conversation-turn-disclosure__body")!;
    const anchor = container.querySelector<HTMLElement>('[data-context-id="turn-completed-user"]')!;
    const reasoning = container.querySelector<HTMLElement>('[data-context-id="turn-completed-reasoning"]')!;
    const terminal = container.querySelector<HTMLElement>('[data-context-id="turn-completed-final"]')!;

    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).toHaveTextContent("model-completed 运行了 1m1s");
    expect(toggle).toHaveTextContent("输入120");
    expect(toggle).toHaveTextContent("缓存输入40");
    expect(toggle).toHaveTextContent("输出12");
    expect(body).toHaveClass("collapse-region--closed");
    expect(body).toHaveAttribute("aria-hidden", "true");
    expect(anchor.closest(".conversation-turn-disclosure")).toBeNull();
    expect(reasoning.closest(".conversation-turn-disclosure")).toBe(disclosure);
    expect(terminal.closest(".conversation-turn-disclosure")).toBeNull();
    expect(screen.getByText("这是最终回复")).toBeInTheDocument();

    fireEvent.click(toggle);
    expect(onToggleTurn).toHaveBeenCalledWith("turn-completed");
  });

  it("keeps trailing hook diagnostics inside a completed turn while exposing its final assistant", () => {
    const contexts: ContextItem[] = [
      {
        id: "turn-hook-tail-user",
        kind: "user",
        content: "完成后运行钩子",
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "turn-hook-tail-reasoning",
        kind: "reasoning",
        content: "先生成正常回复",
        createdAt: "2026-07-24T00:00:01Z"
      },
      {
        id: "turn-hook-tail-final",
        kind: "assistant",
        content: "这是应当留在折叠块外的最终回复",
        createdAt: "2026-07-24T00:00:02Z"
      },
      {
        id: "turn-hook-tail-diagnostic",
        kind: "system",
        content: "Stop 生命周期钩子已完成",
        localOnly: true,
        hookExecution: {
          executionId: "hook-tail-execution",
          hookId: "hook-tail",
          hookName: "完成后检查",
          event: "Stop",
          status: "succeeded",
          contextInjected: false
        },
        createdAt: "2026-07-24T00:00:03Z"
      }
    ];
    const turn: ConversationTurn = {
      id: "turn-hook-tail",
      requestId: "request-hook-tail",
      anchorContextId: "turn-hook-tail-user",
      modelId: "model-hook-tail",
      startedAt: "2026-07-24T00:00:00Z",
      endedAt: "2026-07-24T00:00:03Z",
      durationMs: 3_000,
      status: "completed",
      contextIds: [
        "turn-hook-tail-reasoning",
        "turn-hook-tail-final",
        "turn-hook-tail-diagnostic"
      ],
      usage: {
        inputTokens: 90,
        cachedInputTokens: 30,
        outputTokens: 9
      },
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: false,
      userToggled: false
    };
    const { container } = render(
      <ContextStream
        contexts={contexts}
        turns={[turn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
        onInsert={vi.fn()}
      />
    );

    const disclosure = container.querySelector<HTMLElement>('[data-turn-id="turn-hook-tail"]')!;
    const toggle = disclosure.querySelector<HTMLButtonElement>(".conversation-turn-disclosure__toggle")!;
    const finalAssistant = container.querySelector<HTMLElement>('[data-context-id="turn-hook-tail-final"]')!;
    const trailingHook = container.querySelector<HTMLElement>('[data-context-id="turn-hook-tail-diagnostic"]')!;

    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(finalAssistant.closest(".conversation-turn-disclosure")).toBeNull();
    expect(trailingHook.closest(".conversation-turn-disclosure")).toBe(disclosure);
    expect(screen.getByText("这是应当留在折叠块外的最终回复")).toBeInTheDocument();
    expect(within(disclosure).getByText("Stop 生命周期钩子已完成")).toBeInTheDocument();
  });

  it("keeps an interrupted partial inside an expanded disclosure", () => {
    const contexts: ContextItem[] = [
      {
        id: "turn-interrupted-user",
        kind: "user",
        content: "检查中断状态",
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "turn-interrupted-reasoning",
        kind: "reasoning",
        content: "中断前的思考",
        interrupted: true,
        createdAt: "2026-07-24T00:00:01Z"
      },
      {
        id: "turn-interrupted-partial",
        kind: "assistant",
        content: "中断前的部分回复",
        interrupted: true,
        createdAt: "2026-07-24T00:00:12Z"
      }
    ];
    const turn: ConversationTurn = {
      id: "turn-interrupted",
      requestId: "request-interrupted",
      anchorContextId: "turn-interrupted-user",
      modelId: "model-interrupted",
      startedAt: "2026-07-24T00:00:00Z",
      endedAt: "2026-07-24T00:00:12Z",
      durationMs: 12_000,
      status: "interrupted",
      contextIds: ["turn-interrupted-reasoning", "turn-interrupted-partial"],
      usage: {
        inputTokens: 30,
        cachedInputTokens: 10,
        outputTokens: 4
      },
      usageOffset: {},
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 1,
      expanded: true,
      userToggled: false
    };
    const { container } = render(
      <ContextStream
        contexts={contexts}
        turns={[turn]}
        onToggleTurn={vi.fn()}
        tools={[]}
        enabledTools={[]}
        onInsert={vi.fn()}
      />
    );

    const disclosure = container.querySelector<HTMLElement>('[data-turn-id="turn-interrupted"]')!;
    const toggle = disclosure.querySelector<HTMLButtonElement>(".conversation-turn-disclosure__toggle")!;
    const body = disclosure.querySelector<HTMLElement>(".conversation-turn-disclosure__body")!;
    const anchor = container.querySelector<HTMLElement>('[data-context-id="turn-interrupted-user"]')!;
    const reasoning = container.querySelector<HTMLElement>('[data-context-id="turn-interrupted-reasoning"]')!;
    const partial = container.querySelector<HTMLElement>('[data-context-id="turn-interrupted-partial"]')!;

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveTextContent("model-interrupted 在 12s 后中止");
    expect(toggle).toHaveTextContent("输入30");
    expect(toggle).toHaveTextContent("缓存输入10");
    expect(toggle).toHaveTextContent("输出4");
    expect(body).not.toHaveClass("collapse-region--closed");
    expect(body).not.toHaveAttribute("aria-hidden");
    expect(anchor.closest(".conversation-turn-disclosure")).toBeNull();
    expect(reasoning.closest(".conversation-turn-disclosure")).toBe(disclosure);
    expect(partial.closest(".conversation-turn-disclosure")).toBe(disclosure);
    expect(within(disclosure).getByText("中断前的部分回复")).toBeInTheDocument();
  });

  it("marks lifecycle diagnostics as local-only", () => {
    const document = createSeedDocument();
    render(
      <ContextStream
        contexts={[{ id: "hook-local", kind: "system", content: "hook output", localOnly: true, createdAt: new Date().toISOString() }]}
        tools={document.tools}
        enabledTools={[]}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    expect(screen.getByText("仅本地")).toHaveAttribute("title", "只保存在本地时间线，不会发送给模型");
  });

  it("keeps one cat for the full stream and narrates every call in flight beside it", () => {
    const user = {
      id: "wait-user",
      kind: "user" as const,
      content: "读取 README",
      createdAt: "2026-07-20T00:00:00Z"
    };
    const announced: ToolContext = {
      id: "wait-read",
      kind: "tool",
      toolName: "read",
      input: {},
      result: { success: true, output: "", executedAt: "2026-07-20T00:00:01Z", durationMs: 0 },
      streaming: true,
      streamStatus: "announced",
      createdAt: "2026-07-20T00:00:01Z"
    };
    const renderStream = (contexts: Array<typeof user | ToolContext>, streaming = true) => (
      <ContextStream
        contexts={contexts}
        tools={[]}
        enabledTools={[]}
        streaming={streaming}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const { container, rerender } = render(renderStream([user]));
    const waiting = container.querySelector<HTMLElement>('[data-stream-waiting="true"]')!;
    const cat = waiting.querySelector<HTMLElement>(".stream-waiting__cat")!;
    expect(waiting).toHaveAccessibleName("模型正在生成");
    expect(cat).toBeInTheDocument();
    expect(waiting.querySelector(".stream-waiting__activity")).not.toBeInTheDocument();

    rerender(renderStream([user, announced]));
    const pending = container.querySelector<HTMLElement>('[data-stream-waiting="true"]')!;
    expect(pending).toHaveAttribute("data-pending-tool", "read");
    expect(pending).toHaveAccessibleName("正在读取文件");
    expect(within(pending).getByText("正在读取文件")).toBeInTheDocument();
    expect(pending.querySelector(".stream-waiting__cat")).toBe(cat);
    expect(container.querySelector('[data-tool-name="read"]')).not.toBeInTheDocument();

    // Arguments complete the line: the path the block would have shown travels
    // with the call onto the indicator rather than being dropped.
    rerender(renderStream([user, { ...announced, input: { path: "README.md" }, streamStatus: "ready" }]));
    expect(container.querySelector('[data-stream-waiting="true"]')).toHaveAccessibleName("正在读取文件 README.md");
    expect(container.querySelector('[data-stream-waiting="true"] .stream-waiting__cat')).toBe(cat);
    expect(container.querySelector('[data-tool-name="read"]')).not.toBeInTheDocument();

    // Execution is still "in flight", so the call stays on the indicator instead
    // of expanding into a block that could only show a heading and a spinner.
    rerender(renderStream([user, { ...announced, input: { path: "README.md" }, streamStatus: "running" }]));
    expect(container.querySelector('[data-stream-waiting="true"]')).toHaveAccessibleName("正在读取文件 README.md");
    expect(container.querySelector('[data-stream-waiting="true"] .stream-waiting__cat')).toBe(cat);
    expect(container.querySelector('[data-tool-name="read"]')).not.toBeInTheDocument();

    // The receipt is what puts the call in the timeline, and takes it off the
    // indicator in the same flush.
    rerender(renderStream([user, {
      ...announced,
      input: { path: "README.md" },
      streamStatus: "completed",
      result: { success: true, output: "# Mework", executedAt: "2026-07-20T00:00:02Z", durationMs: 12 }
    }]));
    expect(container.querySelector('[data-stream-waiting="true"]')).toHaveAccessibleName("模型正在生成");
    expect(container.querySelector('[data-tool-name="read"]')).toBeInTheDocument();

    rerender(renderStream([user], false));
    expect(container.querySelector('[data-stream-waiting="true"]')).not.toBeInTheDocument();
  });

  it("gives every concurrently running call its own line on the indicator", () => {
    // Async tools are dispatched and only collected at the round's settlement
    // point, so a later call really does run beside them. Narrating only the
    // newest would leave the earlier one with no surface at all.
    const live = (id: string, toolName: string, input: ToolContext["input"]): ToolContext => ({
      id,
      kind: "tool",
      toolName,
      input,
      result: { success: true, output: "", executedAt: "2026-07-20T00:00:01Z", durationMs: 0 },
      streaming: true,
      streamStatus: "running",
      createdAt: "2026-07-20T00:00:01Z"
    });

    const { container } = render(
      <ContextStream
        contexts={[
          live("call-search", "web_search", { query: "rust async" }),
          live("call-bash", "bash", { command: "npm test" })
        ]}
        tools={[]}
        enabledTools={[]}
        streaming
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onInsert={vi.fn()}
      />
    );

    const waiting = container.querySelector<HTMLElement>('[data-stream-waiting="true"]')!;
    const lines = waiting.querySelectorAll(".stream-waiting__activity");
    expect(lines).toHaveLength(2);
    expect(lines[0]).toHaveTextContent("正在联网搜索rust async");
    expect(lines[1]).toHaveTextContent("正在运行 Bash 命令npm test");
    // The attribute names the newest call, which is what a caller watching for
    // "what started last" reads.
    expect(waiting).toHaveAttribute("data-pending-tool", "bash");
    expect(container.querySelector(".tool-call-group")).not.toBeInTheDocument();
  });

  describe("workflow run card", () => {
    const liveWorkflow: ToolContext = {
      id: "wf-call",
      kind: "tool",
      toolName: "workflow",
      round: 1,
      input: { scriptName: "重构" },
      result: { success: true, output: "", executedAt: "2026-08-01T00:00:00Z", durationMs: 0 },
      streaming: true,
      streamStatus: "running",
      createdAt: "2026-08-01T00:00:00Z"
    };
    // Built through the real projections rather than hand-written: a fixture
    // shaped by hand is exactly where a divergence from the live view hides.
    const view = deriveWorkflowRun(
      deriveWorkflowItems([
        subagentViewFixture("run", {
          workflowRun: true, label: "重构", task: "重构取数路径", childIds: ["s0", "s1"]
        }),
        subagentViewFixture("s0", {
          label: "收集", parentId: "run", depth: 1, callIds: ["wf-call-ws1"],
          phase: "P", phaseIndex: 0, status: "completed"
        }),
        subagentViewFixture("s1", {
          label: "复核", parentId: "run", depth: 1, callIds: ["wf-call-ws2"],
          phase: "P", phaseIndex: 0, status: "running", completedAt: null
        })
      ], taskMessagesFixture, Date.parse("2026-08-01T00:02:00Z"))[0],
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "done", label: "收集", phase: "P", phaseIndex: 0 },
        { kind: "agent", index: 1, state: "progress", label: "复核", phase: "P", phaseIndex: 0 }
      ])
    );

    it("titles the run by kind, states its agent count and one square per step", () => {
      const { container } = render(
        <ContextStream
          contexts={[liveWorkflow]}
          tools={[]}
          enabledTools={[]}
          workflowRunByCall={{ "wf-call": view }}
        />
      );

      // No handler, so the card states the run rather than offering to open it.
      const card = container.querySelector<HTMLElement>(".workflow-run-card")!;
      expect(card.tagName).toBe("SECTION");
      expect(within(card).getByText("工作流")).toBeInTheDocument();
      expect(card.querySelector(".workflow-run-card__metrics")).toHaveTextContent("1m 00s");
      expect(within(card).getByText("2 个代理")).toBeInTheDocument();
      // The driver's own name is a pool handle the roster hands out, so it is
      // not the card's title. It survives in the accessible name and nowhere
      // a reader has to look at it.
      expect(within(card).queryByText("重构")).not.toBeInTheDocument();
      expect(container.querySelectorAll(".workflow-run-card__pip")).toHaveLength(2);
      expect(container.querySelectorAll(".workflow-run-card__pip--finished")).toHaveLength(1);
      expect(container.querySelectorAll(".workflow-run-card__pip--running")).toHaveLength(1);
    });

    it("sends the click to the task panel instead of a transcript", async () => {
      const onOpenWorkflowRun = vi.fn();
      render(
        <ContextStream
          contexts={[liveWorkflow]}
          tools={[]}
          enabledTools={[]}
          workflowRunByCall={{ "wf-call": view }}
          onOpenWorkflowRun={onOpenWorkflowRun}
        />
      );

      await userEvent.click(screen.getByRole("button", { name: /重构/ }));
      expect(onOpenWorkflowRun).toHaveBeenCalledWith("run");
    });

    it("draws nothing until the run has a step to report", () => {
      // An empty card would read as a zero-step run, which is not what a run
      // that has simply not reported yet means.
      const { container } = render(
        <ContextStream contexts={[liveWorkflow]} tools={[]} enabledTools={[]} />
      );

      expect(container.querySelector(".workflow-run-card")).toBeNull();
    });

    it("degrades a live completed call with no run view to the plain tool card", () => {
      // Workflow can fail its preflight (bad params, missing script) before a
      // run record ever exists, and `streaming` stays raised for the rest of
      // the model run — a `!streaming` judgement kept that failure invisible
      // until the turn ended. `streamStatus === "completed"` is the terminal
      // shape regardless of the flag.
      const failed: ToolContext = {
        ...liveWorkflow,
        streamStatus: "completed",
        result: { success: false, output: "脚本不存在", executedAt: "2026-08-01T00:00:01Z", durationMs: 5 }
      };
      const { container } = render(
        <ContextStream contexts={[failed]} tools={[]} enabledTools={[]} />
      );

      expect(container.querySelector(".workflow-run-card")).toBeNull();
      expect(container.querySelector('.tool-call-group [data-context-id="wf-call"]')).toBeInTheDocument();
    });

    it("routes insertion to the card's own context index", () => {
      const onInsert = vi.fn();
      const before: ContextItem = {
        id: "before",
        kind: "user",
        content: "跑一个工作流",
        createdAt: "2026-08-01T00:00:00Z"
      };
      const { container } = render(
        <ContextStream
          contexts={[before, liveWorkflow]}
          tools={[]}
          enabledTools={[]}
          onInsert={onInsert}
          workflowRunByCall={{ "wf-call": view }}
        />
      );

      // Without the node in `renderedContextIndexes` the menu cannot resolve a
      // target index for the card's position, and the insert silently lands on
      // the end of the timeline instead.
      const card = container.querySelector<HTMLElement>(".workflow-run-card")!;
      const slot = card.closest<HTMLElement>(".context-slot")!;
      vi.spyOn(slot, "getBoundingClientRect").mockReturnValue(box(100, 100));
      fireEvent.contextMenu(slot, { clientX: 40, clientY: 120 });
      fireEvent.click(screen.getByRole("menuitem", { name: /用户输入.*插入一条用户消息/ }));
      expect(onInsert).toHaveBeenCalledWith(1, "user");
    });

    it("keeps the card out of the ordinary tool disclosure, settled as well as live", () => {
      const read: ToolContext = {
        id: "read-call",
        kind: "tool",
        toolName: "read",
        round: 1,
        input: { path: "a.ts" },
        result: { success: true, output: "a", executedAt: "2026-08-01T00:00:00Z", durationMs: 1 },
        createdAt: "2026-08-01T00:00:00Z"
      };
      // A settled run keeps its card: it is built from the agent roster, which
      // outlives the model run that produced the progress events.
      const settled: ToolContext = {
        ...liveWorkflow,
        streaming: undefined,
        streamStatus: undefined,
        result: { ...liveWorkflow.result, output: "{\"status\":\"complete\"}" }
      };
      const { container } = render(
        <ContextStream
          contexts={[settled, read]}
          tools={[]}
          enabledTools={[]}
          workflowRunByCall={{ "wf-call": view }}
        />
      );

      expect(container.querySelector(".workflow-run-card")).toBeInTheDocument();
      expect(container.querySelectorAll(".tool-call-group")).toHaveLength(1);
      expect(container.querySelector('.tool-call-group [data-context-id="wf-call"]')).toBeNull();
      expect(container.querySelector('.tool-call-group [data-context-id="read-call"]')).toBeInTheDocument();
    });
  });

  describe("path links", () => {
    const body = "见 `src/App.tsx` 与 C:\\Windows\\notepad.exe";
    const baseDir = "C:\\work\\mework";

    const assistant = {
      id: "assistant-paths",
      kind: "assistant" as const,
      content: body,
      createdAt: "2026-09-01T00:00:00Z"
    };
    const user = {
      id: "user-paths",
      kind: "user" as const,
      content: body,
      createdAt: "2026-09-01T00:00:01Z"
    };

    function pathTargets(container: HTMLElement, contextId: string): string[] {
      const card = container.querySelector<HTMLElement>(`[data-context-id="${contextId}"]`)!;
      return [...card.querySelectorAll<HTMLElement>("[data-mework-path]")].map(
        (node) => node.getAttribute("data-mework-path") ?? ""
      );
    }

    afterEach(() => applyAppearance(defaultAppearancePreferences()));

    it("links paths in a model reply and publishes the working directory", () => {
      const { container } = render(
        <ContextStream contexts={[assistant]} tools={[]} enabledTools={[]} pathBaseDir={baseDir} />
      );
      expect(pathTargets(container, "assistant-paths")).toEqual(["src/App.tsx", "C:\\Windows\\notepad.exe"]);
      expect(container.querySelector('[data-context-id="assistant-paths"] .markdown-content'))
        .toHaveAttribute("data-mework-path-base", baseDir);
    });

    /**
     * The appearance preference routes user messages through the same Markdown
     * renderer, so the opt-in has to be decided by the card kind rather than by
     * the renderer itself.
     */
    it("never links paths a user typed, even when user Markdown is enabled", () => {
      applyAppearance({ ...defaultAppearancePreferences(), renderUserMarkdown: true });
      const { container } = render(
        <ContextStream contexts={[assistant, user]} tools={[]} enabledTools={[]} pathBaseDir={baseDir} />
      );
      expect(pathTargets(container, "assistant-paths")).toHaveLength(2);
      expect(pathTargets(container, "user-paths")).toEqual([]);
      expect(container.querySelector('[data-context-id="user-paths"] .markdown-content'))
        .not.toHaveAttribute("data-mework-path-base");
    });
  });
});
