import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { ConversationPlan, ModelRunResponse, ModelStreamEvent } from "./types";
import { documentWithModel, model, resetAppMocks, runtimeMocks } from "./test/appMocks";
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

  it("takes a plan-exit card straight to the plan page without opening the task container", async () => {
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

    // The page opens by itself, because approving a plan you cannot see is the
    // failure the page exists to prevent.
    const page = await screen.findByRole("region", { name: "替换审批闸页面" });
    expect(page).toHaveTextContent("计划模式");
    expect(page).toHaveTextContent("先改规范");
    // The task container is a roster of running work and stays out of the way.
    expect(screen.queryByRole("complementary", { name: "任务容器" })).not.toBeInTheDocument();

    const card = within(page).getByRole("dialog", { name: "计划已就绪，是否开始实施？" });
    expect(Array.from(card.querySelectorAll("footer button")).map((button) => button.textContent))
      .toEqual(["否，继续规划", "是，手动批准编辑", "是，自动接受编辑"]);
    // One card, one place: the composer must not draw a second copy of it.
    expect(screen.getAllByRole("dialog", { name: "计划已就绪，是否开始实施？" })).toHaveLength(1);

    await user.click(within(card).getByRole("button", { name: "是，自动接受编辑" }));
    expect(runtimeMocks.resolveToolPrompt)
      .toHaveBeenCalledWith("prompt-plan-exit", "allow_always", undefined);

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

    const page = await screen.findByRole("region", { name: "替换审批闸页面" });
    await user.click(within(page).getByRole("button", { name: "否，继续规划" }));
    expect(runtimeMocks.resolveToolPrompt).not.toHaveBeenCalled();

    await user.type(within(page).getByRole("textbox", { name: "修改意见" }), "先补迁移脚本");
    await user.click(within(page).getByRole("button", { name: "提交反馈" }));
    expect(runtimeMocks.resolveToolPrompt)
      .toHaveBeenCalledWith("prompt-plan-exit-2", "deny", "先补迁移脚本");
  });

  it("gives a pushed plan a task row that opens the page, and drops the page when the plan is cleared", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversation = document.workspaces[0].conversations[0];

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "打开任务容器" }));
    const tasks = screen.getByRole("complementary", { name: "任务容器" });
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
    expect(await screen.findByRole("region", { name: "替换审批闸页面" })).toBeInTheDocument();

    // The host discarding the plan leaves nothing for the page to show, so it
    // must not stay up as an empty shell.
    await act(async () => {
      emitAppPushEvent({
        type: "conversationPlanUpdated",
        conversationId: conversation.id,
        plan: null
      });
    });
    await waitFor(() => expect(screen.queryByRole("region", { name: "替换审批闸页面" }))
      .not.toBeInTheDocument());
    expect(within(tasks).queryByRole("button", { name: "打开“实施计划”" })).not.toBeInTheDocument();
  });

  it("mirrors a host-side security level change into the composer without saving it back", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversation = document.workspaces[0].conversations[0];

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    expect(screen.getByRole("button", { name: "安全层级：手动" })).toBeInTheDocument();

    await act(async () => {
      emitAppPushEvent({
        type: "conversationSecurityLevelChanged",
        conversationId: conversation.id,
        securityLevel: "plan"
      });
    });

    expect(await screen.findByRole("button", { name: "安全层级：计划模式" })).toBeInTheDocument();
    // The host already committed the level; writing it back would race its own save.
    expect(runtimeMocks.updateConversationRemote).not.toHaveBeenCalled();
  });

  it("offers plan mode as its own security level, between accepting edits and full access", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "安全层级：手动" }));
    const menu = screen.getByRole("menu", { name: "安全层级" });
    expect(within(menu).getAllByRole("menuitemradio").map((item) => item.textContent?.trim()))
      .toEqual(["手动", "允许编辑", "计划模式", "完全访问"]);
  });
});
