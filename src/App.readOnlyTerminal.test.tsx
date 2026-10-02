import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { ShellTaskSnapshot } from "./lib/shellTasks";
import { documentWithModel, openTasksPane, resetAppMocks, runtimeMocks, terminalMocks } from "./test/appMocks";
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
// xterm cannot draw in jsdom; what matters here is which command the page is subscribed to.
vi.mock("./components/ShellTaskPanel", () => ({
  ShellTaskPanel: ({ task, open }: { task: ShellTaskSnapshot; open: boolean }) => (
    <div data-testid="read-only-output" data-open={String(open)}>{task.command}</div>
  )
}));

afterEach(() => configureI18n("zh-CN"));

/** Runs the test on a Mac host, whose most preferred shell is zsh, until it finishes. */
function onMacHost() {
  const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("MacIntel");
  onTestFinished(() => platform.mockRestore());
}

function shellTask(conversationId: string, shellTaskId: string, command: string): ShellTaskSnapshot {
  return {
    shellTaskId,
    conversationId,
    toolName: "bash",
    command,
    stopping: false,
    startedAt: "2026-07-20T01:00:00Z",
    endedAt: null,
    outcome: null,
    exitCode: null
  };
}

const tabNames = () => within(screen.getByRole("tablist", { name: "终端标签" }))
  .getAllByRole("tab").map((element) => element.textContent);

describe("the terminal pane's read-only page", () => {
  beforeEach(resetAppMocks);

  it("opens a command's output as the first tab, one page for every command, and closes with the pane's last tab", async () => {
    onMacHost();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    act(() => {
      emitAppPushEvent({ type: "shellTaskStarted", conversationId, task: shellTask(conversationId, "shell-1", "npm test") });
      emitAppPushEvent({ type: "shellTaskStarted", conversationId, task: shellTask(conversationId, "shell-2", "cargo build") });
    });

    const tasks = await openTasksPane(user);
    const [firstRow] = within(tasks).getAllByRole("button", { name: "打开“bash”" });
    await user.click(firstRow);
    // The row opens the terminal pane onto the command's output and starts no shell for it.
    expect(tabNames()).toEqual(["bash"]);
    expect(screen.getByRole("tab", { name: "bash" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "bash" })).toHaveAttribute("title", "npm test");
    expect(screen.getByTestId("read-only-output")).toHaveTextContent("npm test");
    expect(screen.getByTestId("read-only-output")).toHaveAttribute("data-open", "true");
    expect(window.document.getElementById(`conversation-terminal-${conversationId}-terminal-1`)).toBeNull();

    // A shell opened beside it goes after it, and the page keeps the start.
    await user.click(screen.getByRole("button", { name: "新建终端" }));
    await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
      .getByRole("menuitem", { name: "zsh" }));
    expect(tabNames()).toEqual(["bash", "zsh 1"]);
    expect(screen.getByTestId("read-only-output")).toHaveAttribute("data-open", "false");

    // Another command's row points the one page at that command rather than adding a second.
    const [, secondRow] = within(tasks).getAllByRole("button", { name: "打开“bash”" });
    await user.click(secondRow);
    expect(tabNames()).toEqual(["bash", "zsh 1"]);
    expect(screen.getAllByTestId("read-only-output")).toHaveLength(1);
    expect(screen.getByTestId("read-only-output")).toHaveTextContent("cargo build");
    expect(screen.getByRole("tab", { name: "bash" })).toHaveAttribute("aria-selected", "true");

    // Closing the page ends nothing: the command runs on and the shell stays.
    await user.click(within(screen.getByRole("tab", { name: "bash" }).closest(".page-tab") as HTMLElement)
      .getByRole("button", { name: "关闭只读终端" }));
    await waitFor(() => expect(tabNames()).toEqual(["zsh 1"]));
    expect(terminalMocks.closeTerminal).not.toHaveBeenCalled();
    expect(screen.getByRole("tab", { name: "zsh 1" })).toHaveAttribute("aria-selected", "true");
  });

  it("goes with the host's output, taking the pane along when it was the last tab", async () => {
    onMacHost();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    act(() => {
      emitAppPushEvent({ type: "shellTaskStarted", conversationId, task: shellTask(conversationId, "shell-1", "npm test") });
    });

    await user.click(within(await openTasksPane(user)).getByRole("button", { name: "打开“bash”" }));
    expect(tabNames()).toEqual(["bash"]);
    act(() => {
      emitAppPushEvent({ type: "shellTaskEvicted", conversationId, shellTaskId: "shell-1" });
    });
    await waitFor(() => expect(screen.queryByRole("tablist", { name: "终端标签" })).toBeNull());
    expect(screen.queryByTestId("read-only-output")).toBeNull();
    // Nothing was left to hold a page open, so asking for the pane again asks for a shell.
    const toolbar = window.document.querySelector(".pane-toolbar") as HTMLElement;
    await user.click(within(toolbar).getByRole("button", { name: "终端" }));
    await user.click(within(await screen.findByRole("menu", { name: "新建终端" }))
      .getByRole("menuitem", { name: "显示终端面板" }));
    expect(tabNames()).toEqual(["zsh 1"]);
  });
});
