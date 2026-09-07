import type { ComponentProps } from "react";
import type { TaskContainer } from "./components/TaskContainer";
import type { TaskItem } from "./lib/taskContainer";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, { subagentViewsEqualForChrome } from "./App";
import { configureI18n } from "./i18n";
import { ASK_USER_PENDING_OUTPUT } from "./types";
import { roundModelTurnId, roundProseContextId } from "./lib/runContexts";
import type { SubagentView } from "./lib/subagents";
import type {
  AppDocument,
  JsonObject,
  ModelRunResponse,
  ModelStreamEvent,
  ToolContext
} from "./types";
import {
  resetAppMocks,
  browserMocks,
  documentWithModel,
  registerAgentPreview,
  openTaskContainer,
  model,
  runtimeMocks
} from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

const taskCapture = vi.hoisted(() => ({ props: null as ComponentProps<typeof TaskContainer> | null }));
const taskStop = vi.hoisted(() => vi.fn(async () => true));
vi.mock("./lib/shellTasks", async (importOriginal) => ({
  ...await importOriginal<typeof import("./lib/shellTasks")>(), stopConversationTask: taskStop
}));
vi.mock("./components/TaskContainer", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./components/TaskContainer")>();
  return { ...actual, TaskContainer: (props: ComponentProps<typeof TaskContainer>) => {
    taskCapture.props = props;
    return <actual.TaskContainer {...props} />;
  } };
});

afterEach(() => configureI18n("zh-CN"));

describe("App model run flow — agents", () => {
  beforeEach(resetAppMocks);

  it.each(["subagent", "workflow"] as const)("stops stale %s rows in their source conversation", async (kind) => {
    const document = documentWithModel();
    const source = document.workspaces[0].conversations[0];
    document.workspaces[0].conversations.push({ ...source, id: "conversation-B", title: "Conversation B" });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    taskStop.mockClear();
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await waitFor(() => expect(taskCapture.props?.conversationId).toBe(source.id));
    const oldStop = taskCapture.props!.onStopItem;
    const oldRow = { id: "same", conversationId: source.id, kind,
      agent: { name: "same", label: "same" } } as TaskItem;
    await user.click(screen.getByRole("button", { name: /^Conversation B/ }));
    await waitFor(() => expect(taskCapture.props?.conversationId).toBe("conversation-B"));
    act(() => oldStop(oldRow));
    await waitFor(() => expect(taskStop).toHaveBeenCalledWith(source.id, "same"));
    expect(taskStop).not.toHaveBeenCalledWith("conversation-B", "same");
  });

  it("renders hook lifecycle and context-injection state while the run is active", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "检查钩子流");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => emit({
      type: "hook_execution_started",
      round: 0,
      executionId: "hook-run-1",
      hookId: "hook-1",
      hookName: "加载分支上下文",
      event: "UserPromptSubmit",
      statusMessage: "正在读取分支"
    }));
    expect(await screen.findByText("UserPromptSubmit · 执行中")).toBeInTheDocument();
    expect(screen.getByText(/正在读取分支/).closest("article")).toHaveAttribute("aria-busy", "true");

    act(() => emit({
      type: "hook_execution_completed",
      round: 0,
      executionId: "hook-run-1",
      hookId: "hook-1",
      hookName: "加载分支上下文",
      event: "UserPromptSubmit",
      result: { success: true, output: "branch: feature/hooks", executedAt: "2026-07-12T00:00:00Z", durationMs: 8 },
      blocked: false,
      contextInjected: true
    }));
    expect(await screen.findByText("UserPromptSubmit · 完成")).toBeInTheDocument();
    expect(screen.getByText("已加入模型上下文")).toBeInTheDocument();
    expect(screen.getByText(/branch: feature\/hooks/)).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 12
    }));
  });

  /**
   * With the tab strip gone, a preview row's stop control is the only way to close a page — so it
   * has to mean two different things depending on who is holding the page. While Playwright is
   * driving, it stops the automation; once nobody is, it closes the session.
   */
  it("stops Playwright rather than closing the preview while the model is driving it", async () => {
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });

    const user = userEvent.setup();
    render(<App />);
    const { emit, resolveRun } = await registerAgentPreview(user);
    const container = await openTaskContainer(user);
    const previewRow = await within(container).findByRole("button", { name: /打开“/ });
    expect(previewRow).toBeInTheDocument();

    act(() => {
      emit({ type: "tool_call_announced", round: 2, callId: "call-browser-click", toolName: "playwright", contextId: "ctx-call-browser-click" });
      emit({
        type: "tool_call_arguments_ready",
        round: 2,
        callId: "call-browser-click",
        input: { action: "click", selector: "text=继续" }
      });
    });
    act(() => emit({ type: "tool_execution_started", round: 2, callId: "call-browser-click" }));

    // Stream state publishes on a fixed cadence, so the row re-labels on the next commit.
    const stopAutomation = await within(container).findByRole("button", { name: /中止“/ });
    await user.click(stopAutomation);
    await waitFor(() => expect(runtimeMocks.cancelModelRun).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(browserMocks.performBrowserAction)
      .toHaveBeenCalledWith(conversationId, "stop"));
    // The page itself survives: stopping the driver is not closing the tab.
    expect(browserMocks.closeBrowserSession).not.toHaveBeenCalled();

    await act(async () => resolveRun({
      contexts: [{
        id: "ctx-browser-click-final",
        kind: "tool",
        toolName: "playwright",
        round: 2,
        input: { action: "click", selector: "text=继续" },
        result: {
          success: true,
          output: "clicked",
          executedAt: "2026-07-24T00:00:00Z",
          durationMs: 5
        },
        createdAt: "2026-07-24T00:00:00Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 10,
      stopReason: "cancelled"
    }));

    // Nobody is holding the page now, so the same control closes it.
    await user.click(await within(container).findByRole("button", { name: /中止“/ }));
    await waitFor(() => expect(browserMocks.closeBrowserSession).toHaveBeenCalledWith(
      conversationId,
      expect.any(Number)
    ));
  }, 15_000);

  it("renders same-round streamed tools in place and replaces them with final contexts", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "检查并读取文件");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call_read", toolName: "read", contextId: "ctx_tool_read_derived" });
      emit({ type: "tool_call_announced", round: 1, callId: "call_find", toolName: "find", contextId: "ctx_tool_find_derived" });
    });

    // Both calls are in flight, so neither draws a block: the indicator narrates
    // one line each, in announcement order.
    const toolWaiting = await screen.findByRole("status", { name: "正在读取文件；正在查找文件" });
    expect(toolWaiting).toHaveAttribute("data-pending-tool", "find");
    expect(toolWaiting.querySelector(".stream-waiting__cat")).toBeInTheDocument();
    expect(toolWaiting.querySelectorAll(".stream-waiting__activity")).toHaveLength(2);
    expect(container.querySelector(".tool-call-group")).not.toBeInTheDocument();

    act(() => emit({
      type: "tool_call_arguments_ready",
      round: 1,
      callId: "call_read",
      input: { path: "README.md", line_start: 1 }
    }));
    act(() => emit({
      type: "tool_call_arguments_ready",
      round: 1,
      callId: "call_read",
      input: { path: "README.md" }
    }));
    // Arguments only complete the read's line — the path the block would have
    // shown travels onto the indicator with it.
    expect(await screen.findByRole("status", { name: "正在读取文件 README.md；正在查找文件" }))
      .toBeInTheDocument();
    expect(container.querySelector(".tool-call-group")).not.toBeInTheDocument();

    // Starting execution is not a reason to draw a block either. The receipt is:
    // until it lands there is nothing in the call a block could show.
    act(() => emit({ type: "tool_execution_started", round: 1, callId: "call_read" }));
    expect(container.querySelector(".tool-call-group")).not.toBeInTheDocument();

    const readResult = {
      success: true,
      output: "read-output",
      executedAt: "2026-07-12T00:00:00Z",
      durationMs: 12
    };
    act(() => emit({
      type: "tool_execution_completed",
      round: 1,
      callId: "call_read",
      result: readResult
    }));
    await waitFor(() => expect(container.querySelector(".tool-call-group")).toBeInTheDocument());
    const group = container.querySelector<HTMLElement>(".tool-call-group")!;
    const groupToggle = group.querySelector<HTMLButtonElement>(".tool-call-group__toggle")!;
    const readRow = group.querySelector<HTMLElement>('[data-context-id]')!;
    const readRowId = readRow.dataset.contextId;
    expect(within(readRow).getByRole("status")).toHaveTextContent("完成");
    expect(group.querySelectorAll('[data-context-id]')).toHaveLength(1);
    expect(within(group).queryByRole("button", { name: /编辑工具调用/ })).not.toBeInTheDocument();
    expect(within(group).queryByRole("button", { name: /删除工具调用/ })).not.toBeInTheDocument();
    expect(groupToggle).toHaveAttribute("aria-expanded", "true");
    // The find call has not finished, so it keeps its line and the block has
    // exactly one row.
    expect(screen.getByRole("status", { name: "正在查找文件" }))
      .toHaveAttribute("data-pending-tool", "find");

    await user.click(within(readRow).getByRole("button", { name: /读取了文件.*完成/ }));
    expect(within(readRow).getByText("read-output")).toBeInTheDocument();

    const findResult = {
      success: false,
      output: "find-failed",
      executedAt: "2026-07-12T00:00:01Z",
      durationMs: 7
    };
    act(() => {
      emit({ type: "tool_call_arguments_ready", round: 1, callId: "call_find", input: { query: "*.md" } });
      emit({ type: "tool_execution_started", round: 1, callId: "call_find" });
      emit({ type: "tool_execution_completed", round: 1, callId: "call_find", result: findResult });
    });
    expect(await within(group).findByText("find-failed")).toBeInTheDocument();
    // The read row is the same element it was before its sibling landed.
    expect(group.querySelector(`[data-context-id="${readRowId}"]`)).toBe(readRow);
    expect(groupToggle).toHaveAttribute("aria-expanded", "true");

    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalled());
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => !JSON.stringify(saved).includes("stream-tool-"))).toBe(true);

    await act(async () => resolveRun({
      contexts: [
        {
          // A real host announces and then persists the same id — that
          // agreement is what keeps one timeline row instead of two, and what
          // keeps the card's attestation reachable at save time.
          id: "ctx_tool_read_derived",
          kind: "tool",
          toolName: "read",
          round: 1,
          input: { path: "README.md" },
          result: readResult,
          createdAt: "2026-07-12T00:00:00Z"
        },
        {
          id: "ctx_tool_find_derived",
          kind: "tool",
          toolName: "find",
          round: 1,
          input: { query: "*.md" },
          result: findResult,
          createdAt: "2026-07-12T00:00:01Z"
        }
      ],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 25
    }));

    // The streaming row and the persisted card are one entry, because both
    // sides use the id the host announced rather than each minting its own.
    await waitFor(() => expect(container.querySelector(`[data-context-id="${readRowId}"]`)).toBeInTheDocument());
    expect(readRowId).toBe("ctx_tool_read_derived");
    expect(container.querySelectorAll('[data-context-id="ctx_tool_read_derived"]')).toHaveLength(1);
    expect(container.querySelectorAll('[data-context-id="ctx_tool_find_derived"]')).toHaveLength(1);
    const completedTurn = screen.getByRole("button", { name: /test-model 运行了/ });
    expect(completedTurn).toHaveAttribute("aria-expanded", "false");
    await user.click(completedTurn);
    expect(screen.getByRole("button", { name: "编辑工具调用 读取了文件" })).toBeInTheDocument();
    await waitFor(() => {
      const saved = runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined;
      const savedRead = saved?.workspaces
        .flatMap((workspace) => workspace.conversations)
        .flatMap((conversation) => conversation.contexts)
        .find((context) => context.kind === "tool" && context.toolName === "read");
      expect(savedRead).toMatchObject({
        input: { path: "README.md" },
        requestedInput: { path: "README.md", line_start: 1 }
      });
      expect(JSON.stringify(savedRead)).not.toContain("call_read");
    });
  });

  it("keeps a read-only subagent detail selected across the streamed-to-persisted handoff", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const task = "审查浏览器集成并总结风险";
    const label = "Browser review";
    const update = "已扫描目录，正在核对浏览器工具。";
    const streamedPrelude = "先读取子代理工作区说明。";
    const streamedText = "流式子代理结论";
    const finalText = "最终子代理结论：浏览器集成正常。";
    const result = {
      success: true,
      output: finalText,
      executedAt: "2026-07-14T01:04:00Z",
      durationMs: 240
    };
    const user = userEvent.setup();
    const { container } = render(<App />);
    await openTaskContainer(user);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "启动子代理审查");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call-subagent-review", toolName: "subagent", contextId: "ctx_subagent_review_final" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-subagent-review",
        input: { task, label }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-subagent-review" });
    });

    const tasks = await openTaskContainer(user);
    const card = await within(tasks).findByRole("button", { name: `打开子代理 ${label}` });
    expect(card).not.toHaveAttribute("aria-current");
    await user.click(card);
    expect(card).toHaveAttribute("aria-current", "true");

    const selectedPanel = screen.getByRole("region", { name: label });
    const mainView = container.querySelector<HTMLElement>('[data-conversation-view="editable"]')!;
    const childView = selectedPanel.querySelector<HTMLElement>('[data-conversation-view="readonly"]')!;
    expect(mainView).toHaveClass("conversation-view");
    expect(childView).toHaveClass("conversation-view");
    expect(mainView.querySelector<HTMLElement>(".context-stream")?.className)
      .toBe(childView.querySelector<HTMLElement>(".context-stream")?.className);
    expect(mainView.querySelector(".composer-wrap")).toBeInTheDocument();
    expect(childView.querySelector(".composer-wrap")).not.toBeInTheDocument();
    expect(within(selectedPanel).getByLabelText(`${label}只读终端`)).toBeInTheDocument();
    expect(within(selectedPanel).queryByRole("textbox")).not.toBeInTheDocument();
    expect(within(selectedPanel).queryByRole("button", { name: /编辑|删除|发送/ })).not.toBeInTheDocument();

    act(() => {
      emit({
        type: "usage_updated",
        round: 1,
        usage: { inputTokens: 10, cachedInputTokens: 2, outputTokens: 3, totalTokens: 13 }
      });
      const childUsageEvent: ModelStreamEvent = {
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: {
          type: "usage_updated",
          round: 1,
          usage: { inputTokens: 7, cachedInputTokens: 4, outputTokens: 5, totalTokens: 12 }
        }
      };
      emit(childUsageEvent);
      emit(childUsageEvent);
      emit({ type: "subagent_delta", round: 1, callId: "call-subagent-review", channel: "update", delta: update });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: { type: "text_delta", round: 1, delta: streamedPrelude }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: { type: "tool_call_announced", round: 1, callId: "child-read", toolName: "read", contextId: "ctx-child-read" }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: { type: "tool_call_arguments_ready", round: 1, callId: "child-read", input: { path: "README.md" } }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: { type: "tool_execution_started", round: 1, callId: "child-read" }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: {
          type: "tool_execution_completed",
          round: 1,
          callId: "child-read",
          result: { success: true, output: "README child content", executedAt: "2026-07-14T01:03:00Z", durationMs: 12 }
        }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-subagent-review",
        event: { type: "text_delta", round: 2, delta: streamedText }
      });
      emit({ type: "tool_execution_completed", round: 1, callId: "call-subagent-review", result });
    });

    // The conversation stays mounted behind the subagent view rather than being
    // torn down, so its own assertions have to opt into the hidden subtree.
    expect(await screen.findByRole("button", {
      name: /test-model 运行了 .*输入17.*缓存输入6.*输出8/,
      hidden: true
    })).toBeInTheDocument();
    expect(within(selectedPanel).getByText(update)).toBeInTheDocument();
    expect(within(selectedPanel).getByText(streamedPrelude)).toBeInTheDocument();
    expect(within(selectedPanel).getByText(streamedText)).toBeInTheDocument();
    expect(within(selectedPanel).queryByText(`${streamedPrelude}${streamedText}`)).not.toBeInTheDocument();
    const childRead = within(selectedPanel).getByRole("button", { name: /读取了文件.*完成/ });
    await user.click(childRead);
    expect(within(selectedPanel).getByText("README child content")).toBeInTheDocument();
    expect(within(selectedPanel).getByText("已完成")).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [{
        id: "ctx_subagent_review_final",
        kind: "tool",
        toolName: "subagent",
        round: 1,
        input: { task, label },
        result,
        subagent: {
          task,
          status: "completed",
          contexts: [
            { id: "ctx_subagent_task", kind: "user", content: task, createdAt: "2026-07-14T01:00:00Z" },
            { id: "ctx_subagent_answer", kind: "assistant", content: finalText, createdAt: "2026-07-14T01:04:00Z" }
          ],
          updates: [{ content: update, createdAt: "2026-07-14T01:02:00Z" }]
        },
        createdAt: "2026-07-14T01:00:00Z"
      }],
      usage: { inputTokens: 17, cachedInputTokens: 6, outputTokens: 8, totalTokens: 25 },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 260
    }));

    expect(await screen.findByRole("button", {
      name: /test-model 运行了 .*输入17.*缓存输入6.*输出8/,
      hidden: true
    })).toHaveAttribute("aria-expanded", "false");
    const persistedPanel = await screen.findByRole("region", { name: label });
    expect(persistedPanel).toBe(selectedPanel);
    expect(within(persistedPanel).queryByRole("button", { name: "返回子代理列表" })).not.toBeInTheDocument();
    expect(within(persistedPanel).getByText(finalText)).toBeInTheDocument();
    expect(within(persistedPanel).getByText(update)).toBeInTheDocument();
    expect(within(persistedPanel).queryByText(streamedPrelude)).not.toBeInTheDocument();
    expect(within(persistedPanel).queryByText(streamedText)).not.toBeInTheDocument();
    // The subagent stops being a task once its run persists, so its row leaves
    // the container even though the timeline keeps a chip for the record.
    expect(within(tasks).queryByRole("button", {
      name: `打开子代理 ${label}`
    })).not.toBeInTheDocument();
  });

  it("renders agent_send as a new user turn without hiding the existing child transcript", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].contexts = [{
      id: "ctx-old-spawn",
      kind: "tool",
      toolName: "agent_spawn",
      input: { task: "审查 API", name: "helper", label: "API 审查" },
      result: {
        success: true,
        output: "ok",
        executedAt: "2026-07-14T01:01:00Z",
        durationMs: 0
      },
      subagent: {
        name: "helper",
        task: "审查 API",
        status: "completed",
        contexts: [
          { id: "child-old-task", kind: "user", content: "审查 API", createdAt: "2026-07-14T01:00:00Z" },
          { id: "child-old-answer", kind: "assistant", content: "旧的 API 审查结论", createdAt: "2026-07-14T01:01:00Z" }
        ],
        updates: []
      },
      createdAt: "2026-07-14T01:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await openTaskContainer(user);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "让 helper 继续检查");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call-send-helper", toolName: "agent_send", contextId: "ctx-call-send-helper" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-send-helper",
        input: { agent: "helper", message: "追加检查取消竞态" }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-send-helper" });
      emit({
        type: "tool_execution_completed",
        round: 1,
        callId: "call-send-helper",
        result: {
          success: true,
          output: "子代理 helper 已被唤醒并继续运行。",
          executedAt: "2026-07-14T02:00:00Z",
          durationMs: 0
        }
      });
      emit({ type: "subagent_delta", round: 1, callId: "call-send-helper", channel: "status", delta: "running" });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-send-helper",
        event: { type: "text_delta", round: 1, delta: "正在检查取消竞态" }
      });
    });

    const tasks = await openTaskContainer(user);
    // Task rows use the model-required, addressable `name`; the transcript panel
    // continues to use the display `label`.
    await user.click(await within(tasks).findByRole("button", { name: "打开子代理 helper" }));
    const panel = screen.getByRole("region", { name: "API 审查" });
    expect(within(panel).getByText("旧的 API 审查结论")).toBeInTheDocument();
    expect(within(panel).getByText("追加检查取消竞态")).toBeInTheDocument();
    expect(within(panel).getByText("正在检查取消竞态")).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 20
    }));
  });

  it("keeps parallel child streams isolated while rendering both with the shared timeline", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });
    const user = userEvent.setup();
    render(<App />);
    await openTaskContainer(user);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "并行审查两个模块");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      for (const child of [
        { callId: "call-alpha", name: "alpha", label: "Alpha", task: "审查 Alpha" },
        { callId: "call-beta", name: "beta", label: "Beta", task: "审查 Beta" }
      ]) {
        emit({ type: "tool_call_announced", round: 1, callId: child.callId, toolName: "agent_spawn", contextId: `ctx-${child.callId}` });
        emit({ type: "tool_call_arguments_ready", round: 1, callId: child.callId, input: { prompt: child.task, name: child.name, label: child.label } });
        emit({ type: "tool_execution_started", round: 1, callId: child.callId });
        emit({ type: "subagent_delta", round: 1, callId: child.callId, channel: "status", delta: "running" });
        emit({
          type: "tool_execution_completed",
          round: 1,
          callId: child.callId,
          result: { success: true, output: "ok", executedAt: "2026-07-14T02:00:00Z", durationMs: 0 }
        });
      }
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-alpha",
        event: { type: "text_delta", round: 1, delta: "Alpha 独立流" }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-beta",
        event: { type: "text_delta", round: 1, delta: "Beta 独立流" }
      });
    });

    const tasks = await openTaskContainer(user);
    await user.click(await within(tasks).findByRole("button", { name: "打开子代理 alpha" }));
    let panel = screen.getByRole("region", { name: "Alpha" });
    expect(within(panel).getByText("Alpha 独立流")).toBeInTheDocument();
    expect(within(panel).queryByText("Beta 独立流")).not.toBeInTheDocument();

    // With Alpha's transcript up the conversation is hidden, so Beta is reached
    // from the task container rather than from the timeline.
    await user.click(within(tasks).getByRole("button", { name: "打开子代理 beta" }));
    panel = screen.getByRole("region", { name: "Beta" });
    expect(within(panel).getByText("Beta 独立流")).toBeInTheDocument();
    expect(within(panel).queryByText("Alpha 独立流")).not.toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 20
    }));
  });

  it("renders a web_search call as an ordinary tool record with no agent panel", async () => {
    // The tool card still owns no SubagentRunRecord. Its registry-backed task row
    // arrives separately through app push events, so neither surface is an agent
    // panel.
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(
      await screen.findByLabelText("向 Agent 发送消息"),
      "调查当前联网搜索实现"
    );
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({
        type: "tool_call_announced",
        round: 1,
        callId: "call-web-search",
        toolName: "web_search",
        contextId: "ctx-web-search-final"
      });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-web-search",
        input: { query: "搜索组运行时的实时树是否可见" }
      });
      emit({
        type: "tool_execution_started",
        round: 1,
        callId: "call-web-search"
      });
    });

    await act(async () => resolveRun({
      contexts: [{
        id: "ctx-web-search-final",
        kind: "tool",
        toolName: "web_search",
        round: 1,
        input: { query: "搜索组运行时的实时树是否可见" },
        result: {
          success: true,
          output: "{\"findings\":\"搜索最终报告\",\"untrustedWebContent\":true}",
          executedAt: "2026-07-27T08:00:10Z",
          durationMs: 10
        },
        createdAt: "2026-07-27T08:00:00Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 20
    }));

    expect(await screen.findByText("完成了联网搜索")).toBeInTheDocument();
    // Retired vocabulary and agent panels must not appear.
    expect(screen.queryByRole("region", { name: "联网搜索" })).not.toBeInTheDocument();
    expect(screen.queryByText("联网研究")).not.toBeInTheDocument();
    expect(screen.queryByText("研究执行")).not.toBeInTheDocument();
    expect(screen.queryByText("页面读取")).not.toBeInTheDocument();
  });

  it("reveals a streamed ask_user card and queues an early option click for the next turn", async () => {
    const document = documentWithModel();
    const firstConversation = { ...document.workspaces[0].conversations[0], title: "提问任务" };
    const secondConversation = {
      ...firstConversation,
      id: "conv_other_task",
      title: "其他任务",
      contexts: []
    };
    document.workspaces[0].conversations = [firstConversation, secondConversation];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveFirstRun!: (value: unknown) => void;
    const pendingResult = {
      success: true,
      output: ASK_USER_PENDING_OUTPUT,
      executedAt: "2026-07-12T00:00:00Z",
      durationMs: 4
    };
    const finalQuestion = {
      id: "ctx_ask_user_final",
      kind: "tool" as const,
      toolName: "ask_user",
      round: 1,
      input: {
        questions: [{
          question: "采用哪个实现方案？",
          header: "方案",
          options: [
            { label: "方案 A", description: "保持改动最小" },
            { label: "方案 B", description: "完整重构" }
          ],
          multiSelect: false
        }]
      },
      result: pendingResult,
      createdAt: "2026-07-12T00:00:00Z"
    };
    runtimeMocks.runModel
      .mockImplementationOnce((_request, onEvent) => {
        emit = onEvent;
        return new Promise((resolve) => { resolveFirstRun = resolve; });
      })
      .mockResolvedValueOnce({
        contexts: [{ id: "ctx_after_answer", kind: "assistant", content: "已按方案 A 继续", createdAt: "2026-07-12T00:00:01Z" }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 5,
        stopReason: "completed"
      });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "请先确认方案");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call_ask", toolName: "ask_user", contextId: "ctx-call_ask" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call_ask",
        input: finalQuestion.input
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call_ask" });
      emit({ type: "tool_execution_completed", round: 1, callId: "call_ask", result: pendingResult });
    });

    const option = await screen.findByRole("button", { name: /方案 A/ });
    expect(option.closest("[data-pending-question=true]")).toBeInTheDocument();
    expect(screen.getByLabelText("回答 Agent 的提问")).toBeInTheDocument();
    await user.click(option);
    await user.click(screen.getByRole("button", { name: "完成" }));
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    await user.click(screen.getByText("其他任务").closest("button")!);

    await act(async () => resolveFirstRun({
      contexts: [finalQuestion],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 9,
      stopReason: "cancelled"
    }));

    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toBeInTheDocument());
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    await user.click(screen.getByText("提问任务").closest("button")!);
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.runModel).toHaveBeenNthCalledWith(2, expect.objectContaining({
      contexts: expect.arrayContaining([
        expect.objectContaining({ kind: "tool", toolName: "ask_user" }),
        expect.objectContaining({
          kind: "user",
          content: 'User has answered your questions: "采用哪个实现方案？"="方案 A"'
        })
      ])
    }), expect.any(Function), expect.any(String));
  });

  /**
   * When renderer and conversation-store bodies differ item-by-item, the host
   * retains its own body. The renderer must recommit question answers against
   * that authoritative body before starting a model run.
   */
  function conversationWithRefusingHost(alwaysRefuse: boolean) {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    const askCard: ToolContext = {
      id: "ctx_ask_refused",
      kind: "tool",
      toolName: "ask_user",
      round: 1,
      input: {
        questions: [{
          question: "采用哪个实现方案？",
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
        executedAt: "2026-08-25T00:00:00Z",
        durationMs: 0
      },
      createdAt: "2026-08-25T00:00:00Z"
    };
    const userTurn = {
      id: "ctx_user_first",
      kind: "user" as const,
      content: "请先确认方案",
      createdAt: "2026-08-25T00:00:00Z"
    };
    const assistant = (id: string) => ({
      id,
      kind: "assistant" as const,
      content: "我需要你先定方案",
      createdAt: "2026-08-25T00:00:00Z"
    });
    const streamedAssistantId = "stream-assistant-run_earlier-1";
    const hostAssistantId = "ctx_assistant_host";
    conversation.contexts = [userTurn, assistant(streamedAssistantId), askCard];
    const host = { contexts: [userTurn, assistant(hostAssistantId), askCard] as typeof conversation.contexts };

    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.hasConversationCommands.mockReturnValue(true);
    runtimeMocks.createConversationRemote.mockImplementation(async (_workspaceId, next) => next);
    runtimeMocks.loadConversationRemote.mockImplementation(async () => ({ ...conversation, contexts: host.contexts }));
    runtimeMocks.updateConversationRemote.mockImplementation(async (_workspaceId, next, expected) => {
      const inSync = !alwaysRefuse
        && host.contexts.length === expected.length
        && host.contexts.every((context, index) => context.id === expected[index]);
      if (!inSync) return { ...next, contexts: host.contexts };
      host.contexts = next.contexts;
      return next;
    });
    return { document, host, hostAssistantId, streamedAssistantId };
  }

  it("re-commits a refused question answer against the host body instead of running without it", async () => {
    const { host, hostAssistantId, streamedAssistantId } = conversationWithRefusingHost(false);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 3,
      stopReason: "completed"
    });

    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /方案 A/ }));
    await user.click(screen.getByRole("button", { name: "完成" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    // The answer must enter the authoritative conversation body before the run.
    await waitFor(() => expect(host.contexts.some((context) => (
      context.kind === "user" && context.content.startsWith("User has answered your questions:")
    ))).toBe(true));
    expect(host.contexts.some((context) => context.id === hostAssistantId)).toBe(true);
    expect(host.contexts.some((context) => context.id === streamedAssistantId)).toBe(false);
    expect(screen.queryByText("回答已提交")).not.toBeInTheDocument();
  });

  it("refuses to start a run when the answer cannot be committed, and unlocks the dock", async () => {
    conversationWithRefusingHost(true);

    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /方案 A/ }));
    await user.click(screen.getByRole("button", { name: "完成" }));

    expect(await screen.findByText(
      "这条消息没能写进对话库，已取消发送——请重新发送，不要让模型收到一份缺了它的历史。"
    )).toBeInTheDocument();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    // An uncommitted answer must not appear submitted; controls unlock and the question remains pending.
    expect(screen.queryByText("回答已提交")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "完成" })).toBeInTheDocument();
  });

  /**
   * A rejected host write bypasses `applyAuthority`, leaving an optimistic
   * renderer copy. Reload the authoritative body so a run cannot proceed with
   * missing history, and show the rejection to the user.
   */
  it.each(["success", "error", "null"])("treats a host-rejected write as uncommitted with reload %s", async (reload) => {
    const { host } = conversationWithRefusingHost(false);
    runtimeMocks.updateConversationRemote.mockRejectedValue(new Error(
      "对话 conv-1 的工具卡 ctx_tool_mew（agent_spawn）无法证明来自本应用自己的执行"
    ));

    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /方案 A/ }));
    if (reload === "error") runtimeMocks.loadConversationRemote.mockRejectedValue(new Error("offline"));
    if (reload === "null") runtimeMocks.loadConversationRemote.mockResolvedValue(null);
    await user.click(screen.getByRole("button", { name: "完成" }));

    const banner = await screen.findByText(/这条消息没能写进对话库/);
    // Display the host's rejection reason instead of leaving it only in DevTools.
    expect(banner).toHaveTextContent("无法证明来自本应用自己的执行");
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(host.contexts.some((context) => (
      context.kind === "user" && context.content.startsWith("User has answered your questions:")
    ))).toBe(false);
    expect(screen.queryByText("回答已提交")).not.toBeInTheDocument();
    if (reload === "success") expect(screen.getByRole("button", { name: "完成" })).toBeInTheDocument();
    else expect(screen.queryByRole("button", { name: "停止生成" })).not.toBeInTheDocument();
  });

  it("holds the queue while a question is pending and lets the answer jump ahead of it", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    const askCard: ToolContext = {
      id: "ctx_ask_queued",
      kind: "tool",
      toolName: "ask_user",
      round: 1,
      input: {
        questions: [{
          question: "采用哪个实现方案？",
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
        executedAt: "2026-08-25T00:00:00Z",
        durationMs: 0
      },
      createdAt: "2026-08-25T00:00:00Z"
    };
    conversation.contexts = [
      { id: "ctx_user_first", kind: "user", content: "请先确认方案", createdAt: "2026-08-25T00:00:00Z" },
      askCard
    ];
    conversation.queuedMessages = [
      { id: "queued_unrelated", content: "顺手把 README 也改了", createdAt: "2026-08-25T00:00:01Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 3,
      stopReason: "completed"
    });

    const user = userEvent.setup();
    render(<App />);
    const option = await screen.findByRole("button", { name: /方案 A/ });
    // A queued message is not an answer. Starting it would close the pending
    // question before the actual answer arrives.
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.click(option);
    await user.click(screen.getByRole("button", { name: "完成" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalled());
    const firstRequest = runtimeMocks.runModel.mock.calls[0][0];
    expect(firstRequest.contexts.at(-1)).toEqual(expect.objectContaining({
      kind: "user",
      content: 'User has answered your questions: "采用哪个实现方案？"="方案 A"'
    }));
    // Continue the queue only after the answer is committed.
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.runModel.mock.calls[1][0].contexts.at(-1)).toEqual(expect.objectContaining({
      kind: "user",
      content: "顺手把 README 也改了"
    }));
  });

  it("keeps an answered ask_user continuation in one running UI turn until the final completion", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emitFirst!: (event: ModelStreamEvent) => void;
    let emitSecond!: (event: ModelStreamEvent) => void;
    let resolveFirst!: (value: ModelRunResponse) => void;
    let resolveSecond!: (value: ModelRunResponse) => void;
    const pendingResult = {
      success: true,
      output: ASK_USER_PENDING_OUTPUT,
      executedAt: "2026-07-24T00:00:01Z",
      durationMs: 4
    };
    const question: ToolContext = {
      id: "ctx_same_turn_question",
      kind: "tool",
      toolName: "ask_user",
      round: 1,
      input: {
        questions: [{
          question: "继续采用方案 A 吗？",
          header: "方案",
          options: [
            { label: "方案 A", description: "沿用当前实现" },
            { label: "方案 B", description: "切换实现路径" }
          ],
          multiSelect: false
        }]
      },
      result: pendingResult,
      createdAt: "2026-07-24T00:00:01Z"
    };
    runtimeMocks.runModel
      .mockImplementationOnce((_request, onEvent) => {
        emitFirst = onEvent;
        return new Promise<ModelRunResponse>((resolve) => { resolveFirst = resolve; });
      })
      .mockImplementationOnce((_request, onEvent) => {
        emitSecond = onEvent;
        return new Promise<ModelRunResponse>((resolve) => { resolveSecond = resolve; });
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "先确认方案再继续");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emitFirst({
        type: "usage_updated",
        round: 1,
        usage: {
          inputTokens: 100,
          cachedInputTokens: 20,
          outputTokens: 7,
          totalTokens: 107
        }
      });
      emitFirst({ type: "tool_call_announced", round: 1, callId: "call_same_turn_ask", toolName: "ask_user", contextId: "ctx-call_same_turn_ask" });
      emitFirst({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call_same_turn_ask",
        input: question.input
      });
      emitFirst({ type: "tool_execution_started", round: 1, callId: "call_same_turn_ask" });
      emitFirst({
        type: "tool_execution_completed",
        round: 1,
        callId: "call_same_turn_ask",
        result: pendingResult
      });
    });

    await user.click(await screen.findByRole("button", { name: /方案 A.*沿用当前实现/ }));
    await user.click(screen.getByRole("button", { name: "完成" }));
    await act(async () => resolveFirst({
      contexts: [question],
      usage: {
        inputTokens: 100,
        cachedInputTokens: 20,
        outputTokens: 7,
        totalTokens: 107
      },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 2_000,
      stopReason: "awaiting_user"
    }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    const continuingTurn = screen.getByRole("button", {
      name: /test-model 运行了 2s.*输入100.*缓存输入20.*输出7/
    });
    expect(continuingTurn).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    await waitFor(() => {
      const questionCard = container.querySelector<HTMLElement>(".question-history");
      expect(questionCard?.closest("[data-turn-id]")).toBe(
        container.querySelector("[data-turn-id]")
      );
      expect(within(questionCard!).getByText("你的回答")).toBeInTheDocument();
      expect(within(questionCard!).getByText("方案 A")).toBeInTheDocument();
    });

    act(() => emitSecond({
      type: "usage_updated",
      round: 1,
      usage: {
        inputTokens: 140,
        cachedInputTokens: 30,
        outputTokens: 5,
        totalTokens: 145
      }
    }));
    expect(await screen.findByRole("button", {
      name: /test-model 运行了 .*输入240.*缓存输入50.*输出12/
    })).toHaveAttribute("aria-expanded", "true");

    await act(async () => resolveSecond({
      contexts: [
        {
          id: "ctx_same_turn_reasoning",
          kind: "reasoning",
          content: "结合用户回答继续处理",
          createdAt: "2026-07-24T00:00:03Z"
        },
        {
          id: "ctx_same_turn_final",
          kind: "assistant",
          content: "已按方案 A 完成",
          createdAt: "2026-07-24T00:00:04Z"
        }
      ],
      usage: {
        inputTokens: 140,
        cachedInputTokens: 30,
        outputTokens: 9,
        totalTokens: 149
      },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 3_000,
      stopReason: "completed"
    }));

    const completedTurn = await screen.findByRole("button", {
      name: /test-model 运行了 5s.*输入240.*缓存输入50.*输出16/
    });
    const disclosure = container.querySelector<HTMLElement>("[data-turn-id]")!;
    const questionCard = container.querySelector<HTMLElement>(".question-history")!;
    const continuedReasoning = container.querySelector<HTMLElement>(
      '[data-context-id="ctx_same_turn_reasoning"]'
    )!;
    const finalAssistant = container.querySelector<HTMLElement>(
      '[data-context-id="ctx_same_turn_final"]'
    )!;

    expect(completedTurn).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    expect(questionCard.closest("[data-turn-id]")).toBe(disclosure);
    expect(continuedReasoning.closest("[data-turn-id]")).toBe(disclosure);
    expect(finalAssistant.closest("[data-turn-id]")).toBeNull();
    expect(screen.getByText("已按方案 A 完成")).toBeInTheDocument();
  });

  it("discards a queued ask_user answer when the run is stopped", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    const pendingResult = {
      success: true,
      output: ASK_USER_PENDING_OUTPUT,
      executedAt: "2026-07-12T00:00:00Z",
      durationMs: 4
    };
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "等待提问");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call_stop_ask", toolName: "ask_user", contextId: "ctx-call_stop_ask" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call_stop_ask",
        input: {
          questions: [{
            question: "继续吗？",
            header: "继续",
            options: [
              { label: "继续", description: "继续当前任务" },
              { label: "停止", description: "停止当前任务" }
            ],
            multiSelect: false
          }]
        }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call_stop_ask" });
      emit({ type: "tool_execution_completed", round: 1, callId: "call_stop_ask", result: pendingResult });
    });

    await user.click(await screen.findByRole("button", { name: /继续.*继续当前任务/ }));
    await user.click(screen.getByRole("button", { name: "完成" }));
    await user.click(screen.getByRole("button", { name: "停止生成" }));
    expect(runtimeMocks.cancelModelRun).toHaveBeenCalledTimes(1);

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 9,
      stopReason: "awaiting_user"
    }));
    // Wait for run settlement, then confirm no continuation starts.
    await waitFor(() => expect(screen.queryByRole("button", { name: /停止生成|正在停止生成/ }))
      .not.toBeInTheDocument());
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
  });

  it("shows reasoning while it streams and collapses it when answer text begins", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    let runRequestId = "";
    runtimeMocks.runModel.mockImplementation((_request, onEvent, requestId) => {
      emit = onEvent;
      runRequestId = requestId;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "展示思考流");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => emit({ type: "reasoning_delta", round: 1, delta: "先检查结构，再验证行为。" }));
    expect(await screen.findByText("先检查结构，再验证行为。")).toBeVisible();
    // The heading states the round's cost rather than its phase, both while the
    // clock runs and after it stops; the figures sit beside it, outside the
    // button, so the accessible name does not change every second.
    expect(screen.getByRole("button", { name: "思考了" })).toHaveAttribute("aria-expanded", "true");

    act(() => emit({ type: "reasoning_done", round: 1 }));
    await waitFor(() => expect(screen.getByRole("button", { name: "思考了" })).toHaveAttribute("aria-expanded", "true"));
    expect(screen.getByText("先检查结构，再验证行为。")).toBeInTheDocument();
    act(() => emit({ type: "text_delta", round: 1, delta: "已完成" }));
    expect(await screen.findByText("已完成")).toBeInTheDocument();

    // Host and renderer derive prose IDs from the same round identity, allowing
    // in-place settlement replacement instead of appending another context.
    const modelTurnId = roundModelTurnId(runRequestId, 1);
    await act(async () => resolveRun({
      contexts: [
        {
          id: roundProseContextId(runRequestId, 1, "reasoning"),
          kind: "reasoning",
          content: "先检查结构，再验证行为。",
          round: 1,
          modelTurnId,
          createdAt: "2026-07-11T00:00:00Z"
        },
        {
          id: roundProseContextId(runRequestId, 1, "assistant"),
          kind: "assistant",
          content: "最终答案",
          round: 1,
          modelTurnId,
          createdAt: "2026-07-11T00:00:01Z"
        }
      ],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 15
    }));
    expect(await screen.findByText("最终答案")).toBeInTheDocument();
    const completedTurn = screen.getByRole("button", { name: /test-model 运行了/ });
    expect(completedTurn).toHaveAttribute("aria-expanded", "false");
    await user.click(completedTurn);
    // The host's settled card carries no duration of its own here, so the
    // heading falls back to naming the phase rather than stating a time it
    // cannot measure.
    expect(screen.getByRole("button", { name: "思考过程" })).toHaveAttribute("aria-expanded", "true");
  });
  it("draws a workflow as one compact card in the stream and a panel in the tasks", async () => {
    // The whole surface in one pass: the stream gets a summary, the task panel
    // gets the detail, and neither offers the driver's own transcript — a
    // script has none, and the row that used to offer it went to a blank page.
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise(() => undefined);
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "跑一个工作流");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const step = (callId: string, input: JsonObject) => {
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-workflow",
        event: { type: "tool_call_announced", round: 1, callId, toolName: "workflow_step", contextId: `ctx-${callId}` }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-workflow",
        event: { type: "tool_call_arguments_ready", round: 1, callId, input }
      });
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-workflow",
        event: { type: "tool_execution_started", round: 1, callId }
      });
      // The step worker's own running status, wrapped once by the driver. The
      // roster refuses to invent a workflow child without it: a workflow call
      // with no live child is a call the host rejected before it spawned one.
      emit({
        type: "subagent_event",
        round: 1,
        callId: "call-workflow",
        event: { type: "subagent_delta", round: 1, callId, channel: "status", delta: "running" }
      });
    };

    act(() => {
      // The host strips the script body from the public input down to the run's
      // own name, the plan name and a fingerprint. `name` is what the model
      // called this run — required, and the run's task address; `scriptName` is
      // the plan's own `meta.name`. Same value here so the visible title reads
      // the same either way.
      emit({ type: "tool_call_announced", round: 1, callId: "call-workflow", toolName: "workflow", contextId: "ctx-workflow" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-workflow",
        input: {
          name: "fetch-models-gate-audit",
          scriptName: "fetch-models-gate-audit",
          scriptBytes: 4096,
          scriptSha256: "0f1e"
        }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-workflow" });
      step("call-workflow-ws1", { label: "audit:rust-path", phase: "Audit", phaseIndex: 0, task: "审查 Rust 路径" });
      step("call-workflow-ws2", { label: "verify:one", phase: "Verify", phaseIndex: 1, task: "复核第一条结论" });
      emit({
        type: "workflow_progress",
        round: 1,
        callId: "call-workflow",
        runId: "run-abc",
        entry: { kind: "agent", index: 0, state: "progress", label: "audit:rust-path", phase: "Audit", phaseIndex: 0 }
      });
    });

    // The stream has one compact card and no ordinary workflow-running row.
    const card = await screen.findByRole("button", { name: /fetch-models-gate-audit/ });
    expect(card).toHaveClass("workflow-run-card");
    expect(within(card).getByText("2 个代理")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /打开子代理 fetch-models-gate-audit/ })).not.toBeInTheDocument();

    // Clicking it brings the run's panel forward instead of navigating.
    await user.click(card);
    const tasks = await screen.findByRole("complementary", { name: "任务容器" });
    const panel = await within(tasks).findByRole("region", { name: "工作流 fetch-models-gate-audit" });
    // Phases are always-visible headings; only actual steps are controls.
    expect(within(panel).getByText("Audit")).toBeInTheDocument();
    expect(within(panel).getByText("Verify")).toBeInTheDocument();
    // The main area still shows the conversation, not a subagent transcript.
    expect(screen.getByLabelText("向 Agent 发送消息")).toBeVisible();

    // A step is a real agent, so its tile does open its transcript.
    await user.click(within(panel).getByRole("button", { name: "打开步骤 audit:rust-path" }));
    expect(await screen.findByText("只读终端")).toBeInTheDocument();
  });
});

describe("subagentViewsEqualForChrome", () => {
  function view(overrides: Partial<SubagentView> = {}): SubagentView {
    return {
      id: "ws1",
      name: null,
      kind: "workflowStep",
      workflowRun: false,
      label: "audit",
      task: "审计",
      status: "completed",
      summary: "完成",
      contexts: [],
      updates: [],
      live: null,
      callIds: ["call-1"],
      parentId: "wf",
      depth: 1,
      childIds: [],
      phase: null,
      phaseIndex: null,
      stepIndex: 0,
      role: null,
      usage: {},
      toolCount: 0,
      createdAt: "2026-08-26T00:00:00.000Z",
      completedAt: "2026-08-26T00:01:00.000Z",
      ...overrides
    } as SubagentView;
  }

  it("treats a usage-only change as a change", () => {
    // `useStoreSelector` keeps the *previous* array whenever this says equal, so
    // a comparator that ignores usage discards the update outright. That is why
    // an externalized workflow step only showed its tokens once some unrelated
    // field happened to move as well.
    expect(subagentViewsEqualForChrome([view()], [view()])).toBe(true);
    expect(subagentViewsEqualForChrome(
      [view()],
      [view({ usage: { inputTokens: 8_100, outputTokens: 420 } })]
    )).toBe(false);
  });

  it("also notices the other metrics the task rows read", () => {
    expect(subagentViewsEqualForChrome([view()], [view({ toolCount: 3 })])).toBe(false);
    expect(subagentViewsEqualForChrome([view()], [view({ stepIndex: 1 })])).toBe(false);
    expect(subagentViewsEqualForChrome(
      [view()],
      [view({ role: { name: "reviewer", modelId: "m", providerId: "p" } as SubagentView["role"] })]
    )).toBe(false);
  });
});
