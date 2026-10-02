import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { ConversationPlan, ModelRunResponse, ModelStreamEvent } from "./types";
import { documentWithModel, model, openTasksPane, resetAppMocks, runtimeMocks } from "./test/appMocks";
import { emitAppPushEvent } from "./test/appMockInstances";

vi.mock("./lib/appEvents", async () => (await import("./test/appMockInstances")).appEventsModuleMock());

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

afterEach(() => configureI18n("zh-CN"));

function plan(conversationId: string, overrides: Partial<ConversationPlan> = {}): ConversationPlan {
  return {
    conversationId,
    markdown: "# 替换审批闸\n\n1. 先改规范\n2. 再改宿主",
    status: "draft",
    createdAt: "2026-09-06T00:00:00Z",
    updatedAt: "2026-09-06T00:05:00Z",
    ...overrides
  };
}

describe("App plan mode", () => {
  beforeEach(resetAppMocks);

  it("takes a plan-exit card straight to the plan pane without opening the tasks pane", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversation = document.workspaces[0].conversations[0];
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "先做个计划");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    // The plan exists before the card: the model wrote it, then asked to leave.
    await act(async () => {
      emitAppPushEvent({
        type: "conversationPlanUpdated",
        conversationId: conversation.id,
        plan: plan(conversation.id)
      });
    });

    act(() => emit({
      type: "tool_approval_requested",
      promptId: "prompt-plan-exit",
      toolName: "exit_plan_mode",
      label: "退出计划模式",
      summary: "分两步替换审批闸",
      riskLevel: "低",
      reason: "离开计划模式后模型可以修改文件",
      allowAlwaysOffered: true,
      kind: "plan_exit"
    }));

    // The pane opens by itself, because approving a plan you cannot see is the
    // failure the pane exists to prevent. Its name is the plan's own heading.
    const page = await screen.findByRole("region", { name: "替换审批闸" });
    // The pane header carries the status the plan is in; a pending exit card outranks the stored one.
    expect(within(page).getByText("待批准")).toBeInTheDocument();
    expect(page).toHaveTextContent("先改规范");
    // The tasks pane is a roster of running work and stays out of the way.
    expect(screen.queryByRole("region", { name: "任务" })).not.toBeInTheDocument();

    const card = within(page).getByRole("dialog", { name: "计划已就绪，批准或提意见" });
    expect(Array.from(card.querySelectorAll("footer button")).map((button) => button.textContent))
      .toEqual(["发送意见", "批准"]);
    // One card, one place: the composer must not draw a second copy of it.
    expect(screen.getAllByRole("dialog", { name: "计划已就绪，批准或提意见" })).toHaveLength(1);

    await user.click(within(card).getByRole("button", { name: "批准" }));
    expect(runtimeMocks.resolveToolPrompt)
      .toHaveBeenCalledWith("prompt-plan-exit", "allow_once", undefined);

    await act(async () => resolveRun({
      contexts: [{ id: "ctx_started", kind: "assistant", content: "开始实施", createdAt: "2026-09-06T00:10:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 4
    }));
  });

  it("sends a plan back with the feedback the user typed", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversation = document.workspaces[0].conversations[0];

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({
        type: "conversationPlanUpdated",
        conversationId: conversation.id,
        plan: plan(conversation.id)
      });
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-plan-exit-2",
        toolName: "exit_plan_mode",
        label: "退出计划模式",
        summary: "分两步替换审批闸",
        riskLevel: "低",
        reason: "离开计划模式后模型可以修改文件",
        allowAlwaysOffered: true,
        kind: "plan_exit"
      });
    });

    const page = await screen.findByRole("region", { name: "替换审批闸" });
    await user.type(within(page).getByRole("textbox", { name: "修改意见" }), "先补迁移脚本");
    expect(runtimeMocks.resolveToolPrompt).not.toHaveBeenCalled();
    await user.click(within(page).getByRole("button", { name: "发送意见" }));
    expect(runtimeMocks.resolveToolPrompt)
      .toHaveBeenCalledWith("prompt-plan-exit-2", "deny", "先补迁移脚本");
  });

  it("gives a pushed plan a task row that opens the pane, and drops the pane when the plan is cleared", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversation = document.workspaces[0].conversations[0];

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const tasks = await openTasksPane(user);
    expect(within(tasks).getByText("还没有任务")).toBeInTheDocument();

    await act(async () => {
      emitAppPushEvent({
        type: "conversationPlanUpdated",
        conversationId: conversation.id,
        plan: plan(conversation.id, { status: "approved" })
      });
    });

    const row = await within(tasks).findByRole("button", { name: "打开“实施计划”" });
    expect(row).toHaveTextContent("已批准");
    await user.click(row);
    expect(await screen.findByRole("region", { name: "替换审批闸" })).toBeInTheDocument();
    // The plan opens beside the roster, not over it: both panes are on screen.
    expect(tasks).toBeInTheDocument();

    // The host discarding the plan leaves nothing for the pane to show, so it
    // must not stay up as an empty shell.
    await act(async () => {
      emitAppPushEvent({
        type: "conversationPlanUpdated",
        conversationId: conversation.id,
        plan: null
      });
    });
    await waitFor(() => expect(screen.queryByRole("region", { name: "替换审批闸" }))
      .not.toBeInTheDocument());
    expect(within(tasks).queryByRole("button", { name: "打开“实施计划”" })).not.toBeInTheDocument();
  });

  it("lets the user move the security level while a turn is streaming", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => {}));

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "开始");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const trigger = screen.getByRole("button", { name: "安全层级：手动" });
    expect(trigger).toBeEnabled();
    await user.click(trigger);
    await user.click(within(screen.getByRole("menu", { name: "安全层级" }))
      .getByRole("menuitemradio", { name: "完全访问" }));

    // Written through like any other setting; the host moves the running
    // turn's level when the write lands (`conversations::update`).
    expect(await screen.findByRole("button", { name: "安全层级：完全访问" })).toBeInTheDocument();
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
  });

  it("switches plan mode from beside the security level, a streaming turn included", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => {}));

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "开始");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const toggle = screen.getByRole("button", { name: "计划" });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    expect(toggle).toBeEnabled();
    await user.click(toggle);
    // Written through like the level; the host moves the running turn when
    // the write lands (`conversations::update`), and nothing restarts.
    expect(toggle).toHaveAttribute("aria-pressed", "true");
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-pressed", "false");
  });

  it("turns the switch back off when the host says an approved plan ended plan mode", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings = { ...conversation.settings, planModeEnabled: true };
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const toggle = screen.getByRole("button", { name: "计划" });
    expect(toggle).toHaveAttribute("aria-pressed", "true");

    act(() => emitAppPushEvent({ type: "conversationPlanModeChanged", conversationId: conversation.id, enabled: false }));
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    // And it can go on again for the next plan.
    await userEvent.setup().click(toggle);
    expect(toggle).toHaveAttribute("aria-pressed", "true");
  });

  it("offers the three security levels, with plan mode no longer among them", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "安全层级：手动" }));
    const menu = screen.getByRole("menu", { name: "安全层级" });
    expect(within(menu).getAllByRole("menuitemradio").map((item) => item.textContent?.trim()))
      .toEqual(["手动", "允许编辑", "完全访问"]);
  });
});
