import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import { emptyConversationPresetSettings } from "./lib/conversationPresets";
import type { AppDocument } from "./types";
import {
  chooseComposerOption,
  composerOptionValue,
  documentWithModel,
  resetAppMocks,
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

afterEach(() => configureI18n("zh-CN"));

/** A visible conversation leaves the workspace's unsent slot free for a genuinely new task. */
function documentWithHistory(): AppDocument {
  const document = documentWithModel();
  document.workspaces[0].conversations[0].contexts = [{
    id: "ctx-existing",
    kind: "user",
    content: "已有任务的上下文",
    createdAt: "2026-08-25T00:00:00Z"
  }];
  return document;
}

/**
 * Security level is the only new-conversation setting directly readable from the composer.
 * Each source therefore uses a distinct value to identify the selected precedence path.
 */
function documentWithPresetsAndMemory(): AppDocument {
  const document = documentWithHistory();
  document.globalSettings.conversationPresets = [
    {
      id: "preset-global",
      name: "全局默认",
      description: "",
      templateId: "",
      settings: { ...emptyConversationPresetSettings(), securityLevel: "allow_edits" }
    },
    {
      id: "preset-workspace",
      name: "本工作区",
      description: "",
      templateId: "",
      settings: { ...emptyConversationPresetSettings(), securityLevel: "request_approval" }
    }
  ];
  document.globalSettings.defaultConversationPresetId = "preset-global";
  document.workspaces[0].defaultConversationPresetId = "";
  document.workspaces[0].lastConversationSettings = {
    ...document.workspaces[0].conversations[0].settings,
    securityLevel: "full_access"
  };
  return document;
}

const securityLevel = () => composerOptionValue("安全层级");

describe("new conversation settings source", () => {
  beforeEach(resetAppMocks);

  // These are independent creations: reopening a workspace's existing slot retains its settings.
  it.each([
    ["workspace memory", "在 Mework 新建任务", false, "完全访问", ""],
    ["global preset", "新建任务", false, "允许编辑", "preset-global"],
    ["workspace preset", "在 Mework 新建任务", true, "手动", "preset-workspace"]
  ] as const)("resolves %s when creating a fresh slot", async (
    _source, buttonName, selectWorkspacePreset, expectedSecurity, expectedPresetId
  ) => {
    const user = userEvent.setup();
    const document = documentWithPresetsAndMemory();
    const existingId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    if (selectWorkspacePreset) {
      await user.click(screen.getByRole("button", { name: "Mework 的更多选项" }));
      await user.click(screen.getByRole("menuitem", { name: /默认对话预设/ }));
      await user.click(screen.getByRole("menuitemradio", { name: "本工作区" }));
    }
    await user.click(screen.getByRole("button", { name: buttonName }));
    await waitFor(() => expect(securityLevel()).toBe(expectedSecurity));
    await waitFor(() => {
      const conversations = savedConversations("ws_mework");
      expect(conversations).toHaveLength(2);
      expect(conversations.find((conversation) => conversation.id !== existingId)?.presetId)
        .toBe(expectedPresetId);
    });
  });

  it("keeps following the workspace snapshot after its last conversation is deleted", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithPresetsAndMemory());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const list = screen.getByRole("navigation", { name: "对话列表" });
    await user.click(list.querySelector<HTMLElement>(".conversation-row__main")!);
    await user.click(within(list).getByRole("button", { name: /^删除 / }));
    await user.click(within(list).getByRole("button", { name: /^确认删除 / }));

    // The replacement slot follows the workspace, not the global default ("允许编辑").
    await waitFor(() => expect(securityLevel()).toBe("完全访问"));
  });

  it("remembers a conversation-level change as the workspace's next starting point", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithPresetsAndMemory());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await chooseComposerOption(user, "安全层级", "允许编辑");
    await waitFor(() => expect(securityLevel()).toBe("允许编辑"));

    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await waitFor(() => expect(securityLevel()).toBe("允许编辑"));
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(2));
  });
});

/** Inspect `saveDocument`: conversation commands are disabled in these tests. */
function savedWorkspaces(): AppDocument["workspaces"] {
  const calls = runtimeMocks.saveDocument.mock.calls;
  const latest = calls.at(-1);
  if (!latest) throw new Error("文档还没有落过盘");
  return (latest[0] as AppDocument).workspaces;
}

function savedConversations(workspaceId: string) {
  return savedWorkspaces().find((workspace) => workspace.id === workspaceId)?.conversations ?? [];
}

function conversationsIn(workspaceId: string): number {
  return savedConversations(workspaceId).length;
}

describe("draft conversation", () => {
  beforeEach(resetAppMocks);

  it("persists a hidden workspace slot immediately and sends with that same real id", async () => {
    const user = userEvent.setup();
    const document = documentWithHistory();
    const existingId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{
        id: "ctx_reply",
        kind: "assistant",
        content: "好的",
        createdAt: "2026-08-26T00:00:00Z"
      }],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const list = screen.getByRole("navigation", { name: "对话列表" });
    expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(1);
    const slotId = await waitFor(() => {
      const conversations = savedConversations("ws_mework");
      expect(conversations).toHaveLength(2);
      const slot = conversations.find((conversation) => conversation.id !== existingId)!;
      expect(slot.id).not.toBe("__draft__");
      expect(slot.contexts).toEqual([]);
      return slot.id;
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "第一句话");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    await waitFor(() => {
      expect(conversationsIn("ws_mework")).toBe(2);
      expect(savedConversations("ws_mework").find((conversation) => conversation.id === slotId)?.contexts)
        .toEqual(expect.arrayContaining([expect.objectContaining({ kind: "user", content: "第一句话" })]));
      expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(2);
    });
    expect(runtimeMocks.runModel).toHaveBeenCalledWith(
      expect.objectContaining({
        conversationId: slotId,
        contexts: expect.arrayContaining([
          expect.objectContaining({ kind: "user", content: "第一句话" })
        ])
      }),
      expect.any(Function),
      expect.any(String)
    );
  });

  it("reuses the existing empty slot and retains its settings across both new-task entry points", async () => {
    const document = documentWithPresetsAndMemory();
    const slot = document.workspaces[0].conversations[0];
    slot.contexts = [];
    slot.settings.securityLevel = "request_approval";
    const slotId = slot.id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    for (const name of ["在 Mework 新建任务", "新建任务"]) {
      await user.click(screen.getByRole("button", { name }));
      await waitFor(() => expect(securityLevel()).toBe("手动"));
      await waitFor(() => expect(savedConversations("ws_mework").map((conversation) => conversation.id))
        .toEqual([slotId]));
      expect(screen.getByRole("navigation", { name: "对话列表" }).querySelectorAll(".conversation-row__main"))
        .toHaveLength(0);
    }
  });

  /**
   * The cat marks an unsent task. The renderer draft is only the half of that state a fresh
   * install ever sees: once a workspace is picked, a new task is real from the first keystroke
   * because the host needs an id, yet it is still unsent and still withheld from the sidebar.
   */
  it("keeps the cat perched when a workspace new task replaces the renderer draft", async () => {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });
    expect(container.querySelector(".composer-cat")).not.toBeNull();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "将被丢弃的无工作区草稿");
    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue(""));
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(1));
    // The renderer draft is gone and the slot is a persisted conversation; the cat stays on its rim.
    expect(container.querySelector(".composer-cat")).not.toBeNull();
  });

  /** A conversation that holds messages is not the empty desk the cat lies on. */
  it("sends the cat away once the conversation holds content, and back on a new task", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithHistory());
    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const list = screen.getByRole("navigation", { name: "对话列表" });
    await user.click(list.querySelector<HTMLElement>(".conversation-row__main")!);
    await waitFor(() => expect(container.querySelector(".composer-cat")).toBeNull());

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(container.querySelector(".composer-cat")).not.toBeNull());
  });

  it("retains a persisted slot's composer text when another conversation is selected", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithHistory());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "没发出去的半句话");

    const list = screen.getByRole("navigation", { name: "对话列表" });
    await user.click(list.querySelector<HTMLElement>(".conversation-row__main")!);
    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue(""));

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("没发出去的半句话"));
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(2));
  });

  it("lands on a draft with no workspace and sends into the temporary workspace", async () => {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });
    const user = userEvent.setup();

    render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });
    await waitFor(() => expect(savedWorkspaces().flatMap((workspace) => workspace.conversations)).toEqual([]));

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "先说再挑目录");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // Sending without a selected workspace creates the conversation in the temporary workspace.
    await waitFor(() => expect(conversationsIn("__temporary__")).toBe(1));
    expect(conversationsIn("ws_mework")).toBe(0);
    expect(savedConversations("__temporary__")[0].id).not.toBe("__draft__");
  });

  /** Inserts a message into the timeline through the right-click menu on an empty conversation. */
  async function insertUserMessage(
    user: ReturnType<typeof userEvent.setup>,
    container: HTMLElement,
    content: string
  ) {
    fireEvent.contextMenu(container.querySelector(".empty-state") ?? container.querySelector(".context-stream")!, {
      clientX: 40,
      clientY: 180
    });
    await user.click(screen.getByRole("menuitem", { name: "用户输入" }));
    // The editor is a portalled dialog, so it is outside the render container.
    await user.type(document.querySelector<HTMLTextAreaElement>(".context-text-editor textarea")!, content);
    await user.click(screen.getByRole("button", { name: "保存" }));
  }

  it("persists renderer draft settings, preset, and composer text when a workspace is selected", async () => {
    const document = documentWithPresetsAndMemory();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    document.workspaces[0].defaultConversationPresetId = "preset-workspace";
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByRole("button", { name: "工作区：选择工作区" });
    expect(securityLevel()).toBe("允许编辑");

    // Content is written after the workspace exists, never before: a conversation that already has
    // contexts is barred from changing workspace, so this is the only order the product allows.
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "选工作区前的输入");
    await waitFor(() => expect(savedWorkspaces().flatMap((workspace) => workspace.conversations)).toEqual([]));

    await user.click(screen.getByRole("button", { name: "工作区：选择工作区" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Mework" }));

    await screen.findByRole("button", { name: "工作区：Mework" });
    expect(securityLevel()).toBe("允许编辑");
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("选工作区前的输入");
    await waitFor(() => {
      const conversations = savedConversations("ws_mework");
      expect(conversations).toHaveLength(1);
      expect(conversations[0].id).not.toBe("__draft__");
      expect(conversations[0]).toEqual(expect.objectContaining({
        presetId: "preset-global",
        settings: expect.objectContaining({ securityLevel: "allow_edits" }),
        contexts: []
      }));
    });
    // It is real but still empty, so it stays out of the sidebar until it holds something — and
    // the cat stays on its composer for the same reason.
    expect(screen.getByRole("navigation", { name: "对话列表" }).querySelectorAll(".conversation-row__main"))
      .toHaveLength(0);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(container.querySelector(".composer-cat")).not.toBeNull();
  });

  it("persists hand-written content before sending and reveals the same slot in the sidebar", async () => {
    const user = userEvent.setup();
    const document = documentWithModel();
    const slotId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_reply", kind: "assistant", content: "好的", createdAt: "2026-08-26T00:00:00Z" }],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });

    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    const list = screen.getByRole("navigation", { name: "对话列表" });
    expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(0);

    await insertUserMessage(user, container, "手写的开场白");

    expect(await screen.findByText("手写的开场白")).toBeInTheDocument();
    await waitFor(() => {
      expect(savedConversations("ws_mework")).toHaveLength(1);
      expect(savedConversations("ws_mework")[0]).toEqual(expect.objectContaining({
        id: slotId,
        contexts: [expect.objectContaining({ kind: "user", content: "手写的开场白" })]
      }));
      expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(1);
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "第一句话");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // The hand-written message leads the same persisted conversation, ahead of the sent message.
    await waitFor(() => {
      expect(conversationsIn("ws_mework")).toBe(1);
      const conversation = savedConversations("ws_mework").find((item) => item.id === slotId);
      expect(conversation?.contexts.slice(0, 2).map(
        (item) => ("content" in item ? item.content : item.kind)
      )).toEqual(["手写的开场白", "第一句话"]);
    });
  });
});
