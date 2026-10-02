import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { AppDocument, Conversation } from "./types";
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

/** The continuation as the host commits it: the conversation's system card,
 * the handoff index, then the host's opening message its first run is armed on.
 * Not a fork: it takes the source's place in the tree and carries no tool lock. */
function continuation(document: AppDocument): Conversation {
  const source = document.workspaces[0].conversations[0];
  const { toolLock: _cacheState, ...settings } = source.settings;
  return {
    ...source,
    id: "conv_handoff_child",
    title: `${source.title}-handover-1`,
    settings,
    contexts: [
      {
        id: "ctx_handoff-system_1",
        kind: "system",
        content: "You are the project's reviewer.",
        createdAt: "2026-09-29T00:00:00Z"
      },
      {
        id: "ctx_handoff_index",
        kind: "system",
        content: "# Handoff notes\n\n- state.md — where the work stands",
        createdAt: "2026-09-29T00:00:00Z"
      },
      {
        id: "ctx_handoff-start_1",
        kind: "user",
        content: "Read the handoff notes, then continue the work from where it stopped.",
        createdAt: "2026-09-29T00:00:01Z"
      }
    ],
    parentConversationId: source.parentConversationId,
    forkOf: null,
    handoffOf: { conversationId: source.id, number: 1 }
  };
}

function withHistory(document: AppDocument): AppDocument {
  const source = document.workspaces[0].conversations[0];
  source.title = "长任务";
  source.contexts = [
    { id: "u1", kind: "user", content: "第一问", createdAt: "2026-09-29T00:00:00Z" },
    { id: "a1", kind: "assistant", content: "第一答", createdAt: "2026-09-29T00:00:01Z" }
  ];
  return document;
}

describe("App auto-compact handoff", () => {
  beforeEach(resetAppMocks);

  it("follows a handed-off conversation on screen into its continuation and starts its run", async () => {
    const document = withHistory(documentWithModel());
    const source = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const child = continuation(document);
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_child_reply", kind: "assistant", content: "接着做完了", createdAt: "2026-09-29T00:00:02Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const sidebar = within(window.document.querySelector(".workspace-list") as HTMLElement);
    expect(sidebar.getByText("长任务").closest(".conversation-row")).toHaveClass("conversation-row--active");

    await act(async () => {
      emitAppPushEvent({
        type: "conversationHandedOff",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: source.id,
        childConversationId: child.id
      });
    });

    await waitFor(() => expect(runtimeMocks.loadConversationRemote).toHaveBeenCalledWith(child.id));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0];
    expect(request.conversationId).toBe(child.id);
    // Armed on the host's opening message, the continuation's last user message.
    expect(request.forkPromptContextId).toBe("ctx_handoff-start_1");
    expect(runtimeMocks.createConversationRemote).not.toHaveBeenCalled();
    // The page moved with the conversation.
    await waitFor(() => expect(
      sidebar.getByText("长任务-handover-1").closest(".conversation-row")
    ).toHaveClass("conversation-row--active"));
    // Listed at the top, where a conversation the user starts goes.
    const continuationRow = sidebar.getByText("长任务-handover-1").closest(".conversation-row") as HTMLElement;
    expect(continuationRow.parentElement!.querySelector(".conversation-row")).toBe(continuationRow);
    expect(await screen.findByText("接着做完了")).toBeInTheDocument();
  });

  it("starts the continuation in the background when another conversation is on screen", async () => {
    const document = withHistory(documentWithModel());
    const source = document.workspaces[0].conversations[0];
    const other: Conversation = {
      ...source,
      id: "conv_other",
      title: "另一个任务",
      contexts: [{ id: "o1", kind: "user", content: "别的事", createdAt: "2026-09-29T00:00:00Z" }]
    };
    document.workspaces[0].conversations = [source, other];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const child = continuation(document);
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const sidebar = within(window.document.querySelector(".workspace-list") as HTMLElement);
    await user.click(sidebar.getByText("另一个任务"));
    expect(sidebar.getByText("另一个任务").closest(".conversation-row")).toHaveClass("conversation-row--active");

    await act(async () => {
      emitAppPushEvent({
        type: "conversationHandedOff",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: source.id,
        childConversationId: child.id
      });
    });

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel.mock.calls[0][0].conversationId).toBe(child.id);
    await waitFor(() => expect(sidebar.getByText("长任务-handover-1")).toBeInTheDocument());
    expect(sidebar.getByText("另一个任务").closest(".conversation-row")).toHaveClass("conversation-row--active");
  });

  it("saves the switch and the threshold chosen in the usage menu", async () => {
    const document = withHistory(documentWithModel());
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: /^上下文用量：/ }));
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    await user.click(within(panel).getByRole("button", { name: /自动压缩/ }));
    // The submenu is a panel of its own beside the usage panel.
    const field = screen.getByRole("spinbutton", { name: "压缩阈值（百分比）" });
    await user.clear(field);
    await user.type(field, "55{Enter}");
    await user.click(screen.getByRole("switch", { name: "启用自动压缩" }));

    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      saved.globalSettings.autoCompact?.enabled === false
      && saved.globalSettings.autoCompact?.thresholdPercent === 55
    ))).toBe(true), { timeout: 3000 });
  });
});
