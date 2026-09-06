import { render, screen, waitFor, within } from "@testing-library/react";
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

/**
 * Security level is the only new-conversation setting directly readable from the composer.
 * Each source therefore uses a distinct value to identify the selected precedence path.
 */
function documentWithPresetsAndMemory(): AppDocument {
  const document = documentWithModel();
  document.globalSettings.conversationPresets = [
    {
      id: "preset-global",
      name: "全局默认",
      description: "",
      settings: { ...emptyConversationPresetSettings(), securityLevel: "allow_edits" }
    },
    {
      id: "preset-workspace",
      name: "本工作区",
      description: "",
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

  it("resolves the workspace +, the top New task, and a workspace default preset in that order", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithPresetsAndMemory());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await waitFor(() => expect(securityLevel()).toBe("完全访问"));

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(securityLevel()).toBe("允许编辑"));

    await user.click(screen.getByRole("button", { name: "Mework 的更多选项" }));
    await user.click(screen.getByRole("menuitem", { name: /设置默认对话预设/ }));
    await user.click(screen.getByRole("menuitemradio", { name: "本工作区" }));
    await user.click(screen.getByRole("button", { name: "在 Mework 新建任务" }));
    await waitFor(() => expect(securityLevel()).toBe("请求批准"));
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
  });
});

/**
 * A draft conversation is persisted only when its first message is sent.
 * Inspect the latest `saveDocument` argument because conversation commands are disabled in these tests.
 */
function savedWorkspaces(): AppDocument["workspaces"] {
  const calls = runtimeMocks.saveDocument.mock.calls;
  const latest = calls.at(-1);
  if (!latest) throw new Error("文档还没有落过盘");
  return (latest[0] as AppDocument).workspaces;
}

function conversationsIn(workspaceId: string): number {
  return savedWorkspaces().find((workspace) => workspace.id === workspaceId)
    ?.conversations.length ?? 0;
}

describe("draft conversation", () => {
  beforeEach(resetAppMocks);

  it("enters a draft without creating anything, then materializes it on the first send", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
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
    await screen.findByRole("region", { name: "使用统计" });
    const list = screen.getByRole("navigation", { name: "对话列表" });
    expect(list.querySelectorAll(".conversation-row__main")).toHaveLength(1);

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "第一句话");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    // The materialized conversation belongs to the workspace selected by the draft.
    await waitFor(() => expect(conversationsIn("ws_mework")).toBe(2));
    expect(runtimeMocks.runModel).toHaveBeenCalledWith(
      expect.objectContaining({
        contexts: expect.arrayContaining([
          expect.objectContaining({ kind: "user", content: "第一句话" })
        ])
      }),
      expect.any(Function),
      expect.any(String)
    );
  });

  /**
   * The cat is a draft-only ornament. It overhangs the composer's top edge, and
   * a conversation with history puts the timeline right there.
   */
  it("perches the cat on the composer only while the conversation is a draft", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());

    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    expect(container.querySelector(".composer-cat")).toBeNull();

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(container.querySelector(".composer-cat")).not.toBeNull());

    const rows = screen.getByRole("navigation", { name: "对话列表" });
    await user.click(rows.querySelector<HTMLElement>(".conversation-row__main")!);
    await waitFor(() => expect(container.querySelector(".composer-cat")).toBeNull());
  });

  it("throws the draft away when another conversation is selected", async () => {
    const user = userEvent.setup();
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "没发出去的半句话");

    const list = screen.getByRole("navigation", { name: "对话列表" });
    await user.click(list.querySelector<HTMLElement>(".conversation-row__main")!);
    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue(""));

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(screen.getByLabelText("向 Agent 发送消息")).toHaveValue(""));
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

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "先说再挑目录");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // Sending without a selected workspace creates the conversation in the temporary workspace.
    await waitFor(() => expect(conversationsIn("__temporary__")).toBe(1));
    expect(conversationsIn("ws_mework")).toBe(0);
  });
});
