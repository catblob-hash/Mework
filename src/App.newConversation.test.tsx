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

const quietReply = {
  contexts: [],
  usage: {},
  model: "test-model",
  providerName: "",
  durationMs: 1
};

const composer = () => screen.getByLabelText("向 Agent 发送消息");

async function send(user: ReturnType<typeof userEvent.setup>, text: string) {
  await user.type(composer(), text);
  await user.click(screen.getByRole("button", { name: "发送" }));
}

describe("new conversation settings source", () => {
  beforeEach(resetAppMocks);

  // A new task is the draft: it resolves its settings from where it was opened, and the
  // conversation it materializes as keeps them.
  it.each([
    ["workspace memory", "在 Mework 新建任务", false, "完全访问", ""],
    ["global preset", "新建任务", false, "允许编辑", "preset-global"],
    ["workspace preset", "在 Mework 新建任务", true, "手动", "preset-workspace"]
  ] as const)("resolves %s for a new task", async (
    _source, buttonName, selectWorkspacePreset, expectedSecurity, expectedPresetId
  ) => {
    const user = userEvent.setup();
    const document = documentWithPresetsAndMemory();
    const existingId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue(quietReply);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    if (selectWorkspacePreset) {
      await user.click(screen.getByRole("button", { name: "Mework 的更多选项" }));
      await user.click(screen.getByRole("menuitem", { name: /默认对话预设/ }));
      await user.click(screen.getByRole("menuitemradio", { name: "本工作区" }));
    }
    await user.click(screen.getByRole("button", { name: buttonName }));
    await waitFor(() => expect(securityLevel()).toBe(expectedSecurity));
    await send(user, "开始");
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
    // The new task is the draft, not a second conversation.
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(1));
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

  it("keeps a new task in the renderer until it is sent, and runs it under the id it became", async () => {
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
    await user.type(composer(), "第一句话");
    // Nothing about the draft reaches the document: its project is still the user's to change.
    await waitFor(() => expect(savedConversations("ws_mework").map((conversation) => conversation.id))
      .toEqual([existingId]));
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const slotId = await waitFor(() => {
      const conversations = savedConversations("ws_mework");
      expect(conversations).toHaveLength(2);
      const created = conversations.find((conversation) => conversation.id !== existingId)!;
      expect(created.id).not.toBe("__draft__");
      expect(created.contexts).toEqual(
        expect.arrayContaining([expect.objectContaining({ kind: "user", content: "第一句话" })])
      );
      expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(2);
      return created.id;
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

  it("keeps one draft across project changes and lands it only in the project it is sent from", async () => {
    const document = documentWithHistory();
    const existingId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue(quietReply);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await user.type(composer(), "换个项目再发");
    await user.click(screen.getByRole("button", { name: "项目：Mework" }));
    await user.click(within(screen.getByRole("menu", { name: "选择项目" }))
      .getByRole("menuitemradio", { name: "临时项目" }));

    // Changing project re-aims the same draft: its text comes along and no project got a slot.
    expect(screen.getByRole("button", { name: "项目：临时项目" })).toBeInTheDocument();
    expect(composer()).toHaveValue("换个项目再发");
    await waitFor(() => {
      expect(savedConversations("ws_mework").map((conversation) => conversation.id)).toEqual([existingId]);
      expect(conversationsIn("__temporary__")).toBe(0);
    });

    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(conversationsIn("__temporary__")).toBe(1));
    expect(conversationsIn("ws_mework")).toBe(1);
  });

  /**
   * The cat marks an unsent task, and there is one of those: a project's new task re-aims the
   * draft that is already open rather than starting a second one beside it.
   */
  it("keeps the cat and the draft's text when a project's new task re-aims the draft", async () => {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByRole("button", { name: "项目：选择项目" });
    expect(container.querySelector(".composer-cat")).not.toBeNull();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "还没挑项目的草稿");
    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await screen.findByRole("button", { name: "项目：Mework" });
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("还没挑项目的草稿");
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(0));
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

  it("keeps the draft and its composer text while another conversation is open", async () => {
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
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(1));
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
    await screen.findByRole("button", { name: "项目：选择项目" });
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

  it("carries the draft's settings, preset, and composer text through a project pick into its first send", async () => {
    const document = documentWithPresetsAndMemory();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    document.workspaces[0].defaultConversationPresetId = "preset-workspace";
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue(quietReply);
    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByRole("button", { name: "项目：选择项目" });
    expect(securityLevel()).toBe("允许编辑");

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "选工作区前的输入");
    await user.click(screen.getByRole("button", { name: "项目：选择项目" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Mework" }));

    // Picking a project only aims the draft: the project's own preset does not replace what the
    // draft already had, and nothing is persisted yet.
    await screen.findByRole("button", { name: "项目：Mework" });
    expect(securityLevel()).toBe("允许编辑");
    expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue("选工作区前的输入");
    await waitFor(() => expect(savedWorkspaces().flatMap((workspace) => workspace.conversations)).toEqual([]));
    expect(screen.getByRole("navigation", { name: "对话列表" }).querySelectorAll(".conversation-row__main"))
      .toHaveLength(0);
    expect(container.querySelector(".composer-cat")).not.toBeNull();

    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => {
      const conversations = savedConversations("ws_mework");
      expect(conversations).toHaveLength(1);
      expect(conversations[0].id).not.toBe("__draft__");
      expect(conversations[0]).toEqual(expect.objectContaining({
        presetId: "preset-global",
        settings: expect.objectContaining({ securityLevel: "allow_edits" }),
        contexts: expect.arrayContaining([
          expect.objectContaining({ kind: "user", content: "选工作区前的输入" })
        ])
      }));
    });
  });

  it("keeps hand-written content in the draft until it is sent, then leads the new conversation with it", async () => {
    const user = userEvent.setup();
    const document = documentWithModel();
    // An empty conversation left from before new tasks were drafts; the draft does not reuse it.
    const leftoverId = document.workspaces[0].conversations[0].id;
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

    // A hand-written message involves no workspace, so the draft stays a draft around it.
    expect(await screen.findByText("手写的开场白")).toBeInTheDocument();
    await waitFor(() => expect(savedConversations("ws_mework")).toEqual([
      expect.objectContaining({ id: leftoverId, contexts: [] })
    ]));
    expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(0);
    expect(screen.getByRole("button", { name: "项目：Mework" })).toBeInTheDocument();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await send(user, "第一句话");

    // The hand-written message leads the new conversation, ahead of the sent message.
    await waitFor(() => {
      expect(conversationsIn("ws_mework")).toBe(2);
      const conversation = savedConversations("ws_mework").find((item) => item.id !== leftoverId);
      expect(conversation?.contexts.slice(0, 2).map(
        (item) => ("content" in item ? item.content : item.kind)
      )).toEqual(["手写的开场白", "第一句话"]);
      expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(1);
    });
  });

  it("fixes the draft in its project once a tool runs in it", async () => {
    const user = userEvent.setup();
    const document = documentWithModel();
    const leftoverId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.type(composer(), "工具之后再说");

    fireEvent.contextMenu(container.querySelector(".empty-state")!, { clientX: 40, clientY: 180 });
    await user.click(screen.getByRole("menuitem", { name: /工具调用/ }));
    await user.click(screen.getByRole("menuitem", { name: "Shell" }));
    await user.click(screen.getByRole("menuitem", { name: /^PowerShell$/ }));
    await user.type(screen.getByLabelText("命令 *"), "Get-ChildItem");
    await user.click(screen.getByRole("button", { name: "执行并添加" }));

    // The host runs the tool against a conversation it holds, never the draft's placeholder.
    await waitFor(() => expect(runtimeMocks.executeTool).toHaveBeenCalledTimes(1));
    const [request] = runtimeMocks.executeTool.mock.calls[0];
    expect(request.conversationId).not.toBe("__draft__");
    expect(request.conversationId).not.toBe(leftoverId);
    expect(runtimeMocks.requestToolApproval)
      .toHaveBeenCalledWith(expect.objectContaining({ conversationId: request.conversationId }));
    // Which is real by the time the host is asked, with the tool card in it.
    expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (saved as AppDocument).workspaces
      .find((workspace) => workspace.id === "ws_mework")?.conversations
      .some((conversation) => conversation.id === request.conversationId))).toBe(true);
    await waitFor(() => expect(savedConversations("ws_mework")
      .find((conversation) => conversation.id === request.conversationId)?.contexts)
      .toEqual([expect.objectContaining({ kind: "tool", toolName: "powershell" })]));
    expect(runtimeMocks.saveDocument.mock.invocationCallOrder[0])
      .toBeLessThan(runtimeMocks.requestToolApproval.mock.invocationCallOrder[0]);
    // It now belongs to its project for good: the project chip is gone, the composer text stays.
    await waitFor(() => expect(screen.queryByRole("button", { name: /^项目：/ })).not.toBeInTheDocument());
    expect(composer()).toHaveValue("工具之后再说");
  });
});

describe("the first task in a new project", () => {
  beforeEach(resetAppMocks);

  it("reaches the host only after the project it lives in", async () => {
    const user = userEvent.setup();
    const document = documentWithHistory();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue(quietReply);
    // The host files a conversation only under a workspace its own document already holds, and it
    // learns of a workspace only from a document save.
    const hostWorkspaces = new Set(document.workspaces.map((workspace) => workspace.id));
    runtimeMocks.hasConversationCommands.mockReturnValue(true);
    runtimeMocks.saveDocument.mockImplementation(async (saved: AppDocument) => {
      hostWorkspaces.clear();
      for (const workspace of saved.workspaces) hostWorkspaces.add(workspace.id);
    });
    runtimeMocks.createConversationRemote.mockImplementation(async (workspaceId, next) => {
      if (!hostWorkspaces.has(workspaceId)) throw new Error(`工作区 ${workspaceId} 不存在`);
      return next;
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建项目" }));
    const dialog = screen.getByRole("dialog", { name: "新建项目" });
    await user.type(within(dialog).getByRole("textbox", { name: "工作区 1 的绝对路径" }), "C:\\Temp\\first-task");
    await user.click(within(dialog).getByRole("button", { name: "创建项目" }));

    // The new project opens on the draft; sending it at once materializes the conversation while
    // the project itself is still waiting on the debounced document save.
    await screen.findByRole("button", { name: "项目：first-task" });
    expect(runtimeMocks.createConversationRemote).not.toHaveBeenCalled();
    await send(user, "第一句话");

    await waitFor(() => expect(runtimeMocks.createConversationRemote).toHaveBeenCalledTimes(1));
    const [workspaceId, created] = runtimeMocks.createConversationRemote.mock.calls[0];
    expect(workspaceId).not.toBe("ws_mework");
    expect(created.id).not.toBe("__draft__");
    // The host accepted it, so it is not a renderer-only ghost the run cannot find.
    await expect(runtimeMocks.createConversationRemote.mock.results[0].value).resolves.toBe(created);
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledWith(
      expect.objectContaining({ conversationId: created.id }),
      expect.any(Function),
      expect.any(String)
    ));
  });
});
