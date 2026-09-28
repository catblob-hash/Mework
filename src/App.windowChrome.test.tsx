import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { AppDocument, Conversation } from "./types";
import { documentWithModel, resetAppMocks, runtimeMocks } from "./test/appMocks";
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

/** Two conversations with something in them, so the sidebar lists both. */
function twoConversations(): { document: AppDocument; first: Conversation; second: Conversation } {
  const document = documentWithModel();
  const first: Conversation = {
    ...document.workspaces[0].conversations[0],
    title: "第一个任务",
    updatedAt: "2026-09-05T00:00:00Z",
    contexts: [{ id: "u1", kind: "user", content: "问一", createdAt: "2026-09-05T00:00:00Z" }]
  };
  const second: Conversation = {
    ...first,
    id: "conv_second",
    title: "第二个任务",
    updatedAt: "2026-09-06T00:00:00Z",
    contexts: [{ id: "u2", kind: "user", content: "问二", createdAt: "2026-09-06T00:00:00Z" }]
  };
  document.workspaces[0].conversations = [first, second];
  return { document, first, second };
}

async function renderApp(document: AppDocument) {
  runtimeMocks.loadDocument.mockResolvedValue(document);
  const user = userEvent.setup();
  render(<App />);
  await screen.findByLabelText("向 Agent 发送消息");
  const sidebar = within(screen.getByRole("navigation", { name: "对话列表" }));
  const activeTitle = () => window.document.querySelector(".conversation-row--active .conversation-row__title")?.textContent;
  return { user, sidebar, activeTitle };
}

describe("App window chrome", () => {
  beforeEach(resetAppMocks);

  it("walks back and forward through the conversations opened before", async () => {
    const { document, first, second } = twoConversations();
    const { user, sidebar, activeTitle } = await renderApp(document);

    await user.click(sidebar.getByText(first.title));
    await user.click(sidebar.getByText(second.title));
    const back = screen.getByRole("button", { name: "后退到上一个对话" });
    const forward = screen.getByRole("button", { name: "前进到下一个对话" });
    expect(forward).toBeDisabled();

    await user.click(back);
    expect(activeTitle()).toBe(first.title);
    expect(forward).toBeEnabled();

    await user.click(forward);
    expect(activeTitle()).toBe(second.title);
    expect(forward).toBeDisabled();

    // A new task is not a stop in the history: back from it returns to where it was opened.
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    expect(activeTitle()).toBeUndefined();
    await user.click(back);
    expect(activeTitle()).toBe(second.title);
  });

  it("sends a solid ground picked from the other theme back to the theme's own when the theme changes", async () => {
    const { document } = twoConversations();
    document.globalSettings.theme = "day";
    document.globalSettings.appearance = { ...document.globalSettings.appearance, background: "solid:night" };
    const { user } = await renderApp(document);
    const root = window.document.documentElement;
    // Applying the saved theme at startup is not a change of theme.
    expect(root.dataset.theme).toBe("day");
    expect(root.dataset.backdrop).toBe("custom");

    await user.click(screen.getByRole("button", { name: "设置" }));
    const dialog = screen.getByRole("dialog", { name: "全局设置" });
    await user.click(within(within(dialog).getByRole("navigation", { name: "全局设置分类" })).getByRole("button", { name: "外观" }));
    await user.click(within(dialog).getByRole("button", { name: "深色" }));
    expect(root.dataset.theme).toBe("night");
    await waitFor(() => expect(root.dataset.backdrop).toBe("theme"));

    // A picture stays whatever the theme does.
    await user.click(within(dialog).getByRole("button", { name: "选择背景…" }));
    const library = await screen.findByRole("dialog", { name: "背景" });
    await user.click(within(library).getByRole("button", { name: "书架中" }));
    await user.click(within(library).getByRole("button", { name: "完成" }));
    await user.click(within(dialog).getByRole("button", { name: "浅色" }));
    expect(root.dataset.theme).toBe("day");
    expect(root.dataset.backdrop).toBe("custom");
    expect(window.document.querySelector(".app-backdrop__image")).toHaveAttribute("src", expect.stringContaining("shelf"));
  });

  it("toggles the sidebar from the drawer button beside the arrows", async () => {
    const { document } = twoConversations();
    const { user } = await renderApp(document);
    const shell = window.document.querySelector(".app-shell")!;

    await user.click(screen.getByRole("button", { name: "收起侧栏" }));
    expect(shell).toHaveClass("app-shell--sidebar-closed");
    await user.click(screen.getByRole("button", { name: "打开侧栏" }));
    expect(shell).not.toHaveClass("app-shell--sidebar-closed");
  });

  it("renames the conversation in place from its title in the top bar", async () => {
    const { document, first } = twoConversations();
    const { user, sidebar } = await renderApp(document);
    await user.click(sidebar.getByText(first.title));

    const header = window.document.querySelector<HTMLElement>(".topbar")!;
    // The project follows the title on the same line; no project dropdown is left.
    expect(within(header).getByText("Mework")).toHaveClass("conversation-title__project");

    await user.click(within(header).getByRole("button", { name: `对话标题：${first.title}，点击重命名` }));
    const field = within(header).getByRole("textbox", { name: "对话标题" });
    expect(field).toHaveValue(first.title);
    await user.keyboard("{Escape}");
    expect(within(header).queryByRole("textbox")).toBeNull();
    expect(sidebar.getByText(first.title)).toBeInTheDocument();

    await user.click(within(header).getByRole("button", { name: `对话标题：${first.title}，点击重命名` }));
    await user.clear(within(header).getByRole("textbox", { name: "对话标题" }));
    await user.keyboard("改过的标题{Enter}");
    expect(within(header).getByRole("button", { name: "对话标题：改过的标题，点击重命名" })).toBeInTheDocument();
    expect(sidebar.getByText("改过的标题")).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([snapshot]) => (
      (snapshot as AppDocument).workspaces[0].conversations.some((conversation) => conversation.title === "改过的标题")
    ))).toBe(true));
  });

  it("finds a conversation by title from the search button and opens it", async () => {
    const { document, first, second } = twoConversations();
    const { user, sidebar, activeTitle } = await renderApp(document);
    await user.click(sidebar.getByText(first.title));

    await user.click(screen.getByRole("button", { name: "搜索对话" }));
    const dialog = screen.getByRole("dialog", { name: "搜索对话" });
    const field = within(dialog).getByRole("combobox", { name: "搜索对话" });
    expect(field).toHaveFocus();
    // With no query, the most recently updated come first.
    expect(within(dialog).getAllByRole("option").map((option) => option.textContent))
      .toEqual([`${second.title}Mework`, `${first.title}Mework`]);

    await user.keyboard("第二");
    expect(within(dialog).getAllByRole("option")).toHaveLength(1);
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("dialog", { name: "搜索对话" })).toBeNull();
    expect(activeTitle()).toBe(second.title);

    await user.click(screen.getByRole("button", { name: "搜索对话" }));
    await user.keyboard("没有这个");
    expect(within(screen.getByRole("dialog", { name: "搜索对话" })).getByText("没有匹配的对话")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "搜索对话" })).toBeNull();
  });

  it("turns a conversation's mark amber while a request waits on the user", async () => {
    const { document, first } = twoConversations();
    await renderApp(document);
    const mark = () => window.document.querySelector(`[data-conversation-id="${first.id}"] .conversation-status`);
    expect(mark()).toHaveClass("conversation-status--idle");

    await act(async () => {
      emitAppPushEvent({
        type: "forkRequested",
        forkId: "fork-1",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: first.id,
        sourceTitle: first.title,
        prompt: "再分一个",
        requestedAt: "2026-09-07T00:00:00Z"
      });
    });
    expect(mark()).toHaveClass("conversation-status--blocked");
    expect(mark()).toHaveAccessibleName("等待你处理");
  });
});
