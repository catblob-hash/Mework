import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { createTemporaryWorkspace } from "../lib/workspaces";
import type { Conversation, Workspace } from "../types";
import { Sidebar } from "./Sidebar";
import type { ConversationStatus } from "./Sidebar";

function sidebarFixture(): { workspace: Workspace; title: string } {
  const seed = createSeedDocument();
  const title = "待重命名任务";
  const workspace = {
    ...seed.workspaces[0],
    conversations: [{ ...seed.workspaces[0].conversations[0], title }]
  };
  return { workspace, title };
}

function sidebarProps(workspace: Workspace) {
  return {
    workspaces: [workspace],
    activeWorkspaceId: workspace.id,
    activeConversationId: workspace.conversations[0].id,
    onSelectConversation: vi.fn(),
    onNewConversation: vi.fn(),
    onAddWorkspace: vi.fn(),
    onRenameConversation: vi.fn(),
    onDeleteWorkspace: vi.fn(),
    onDeleteConversation: vi.fn(),
    isConversationRunning: vi.fn(() => false),
    conversationStatus: vi.fn((): ConversationStatus => "idle"),
    conversationPresets: [],
    onSetWorkspaceDefaultPreset: vi.fn(),
    isWorkspaceDeleting: vi.fn(() => false),
    onOpenSettings: vi.fn(),
    onReorderWorkspace: vi.fn(),
    onReorderConversation: vi.fn()
  };
}

describe("Sidebar conversation actions", () => {
  it("drags from the row surface to reorder workspaces and conversations inside one workspace", () => {
    const { workspace } = sidebarFixture();
    const secondConversation = { ...workspace.conversations[0], id: "conversation-second", title: "第二任务" };
    const sourceWorkspace = { ...workspace, conversations: [workspace.conversations[0], secondConversation] };
    const secondWorkspace: Workspace = {
      ...workspace,
      id: "workspace-second",
      name: "第二工作区",
      path: "C:\\second",
      conversations: []
    };
    const props = sidebarProps(sourceWorkspace);
    render(<Sidebar {...props} workspaces={[sourceWorkspace, secondWorkspace]} />);

    const list = document.querySelector<HTMLElement>(".workspace-list")!;
    const firstGroup = document.querySelector<HTMLElement>(`[data-workspace-group-id="${workspace.id}"]`)!;
    const secondGroup = document.querySelector<HTMLElement>(`[data-workspace-group-id="${secondWorkspace.id}"]`)!;
    const firstHeading = firstGroup.querySelector<HTMLElement>(".workspace-heading")!;
    const secondHeading = secondGroup.querySelector<HTMLElement>(".workspace-heading")!;
    const conversationRows = firstGroup.querySelectorAll<HTMLElement>(".conversation-row");
    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 240, bottom: top + height, width: 240, height, toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 300);
    setRect(firstGroup, 0, 110);
    setRect(firstHeading, 0, 32);
    setRect(conversationRows[0], 34, 36);
    setRect(conversationRows[1], 72, 36);
    setRect(secondGroup, 140, 32);
    setRect(secondHeading, 140, 32);

    fireEvent.pointerDown(firstHeading, { pointerId: 1, button: 0, isPrimary: true, clientX: 100, clientY: 16 });
    fireEvent.pointerMove(window, { pointerId: 1, clientX: 100, clientY: 166 });
    fireEvent.pointerUp(window, { pointerId: 1, clientX: 100, clientY: 166 });
    expect(props.onReorderWorkspace).toHaveBeenCalledWith(workspace.id, secondWorkspace.id, "after");

    fireEvent.pointerDown(conversationRows[1], { pointerId: 2, button: 0, isPrimary: true, clientX: 100, clientY: 90 });
    fireEvent.pointerMove(window, { pointerId: 2, clientX: 100, clientY: 40 });
    fireEvent.pointerUp(window, { pointerId: 2, clientX: 100, clientY: 40 });
    expect(props.onReorderConversation).toHaveBeenCalledWith(
      workspace.id,
      secondConversation.id,
      workspace.conversations[0].id,
      "before"
    );
  });

  it("shows one canonical conversation insertion target at the first, middle, and last boundary", () => {
    const { workspace } = sidebarFixture();
    const conversations = [
      workspace.conversations[0],
      { ...workspace.conversations[0], id: "conversation-second", title: "第二任务" },
      { ...workspace.conversations[0], id: "conversation-third", title: "第三任务" }
    ];
    const sourceWorkspace = { ...workspace, conversations };
    const props = sidebarProps(sourceWorkspace);
    const { container } = render(<Sidebar {...props} />);
    const list = container.querySelector<HTMLElement>(".workspace-list")!;
    const group = container.querySelector<HTMLElement>("[data-workspace-group-id]")!;
    const heading = group.querySelector<HTMLElement>(".workspace-heading")!;
    const rows = Array.from(group.querySelectorAll<HTMLElement>(".conversation-row"));
    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 240, bottom: top + height, width: 240, height, toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 220);
    setRect(group, 0, 148);
    setRect(heading, 0, 32);
    rows.forEach((row, index) => setRect(row, 34 + index * 38, 36));
    const targets = () => container.querySelectorAll(".conversation-row.drop-target--before, .conversation-row.drop-target--after");

    fireEvent.pointerDown(rows[2], { pointerId: 11, button: 0, isPrimary: true, clientX: 100, clientY: 128 });
    fireEvent.pointerMove(window, { pointerId: 11, clientX: 100, clientY: 35 });
    expect(targets()).toHaveLength(1);
    expect(rows[0]).toHaveClass("drop-target--before");
    fireEvent.pointerCancel(window, { pointerId: 11, clientX: 100, clientY: 35 });

    fireEvent.pointerDown(rows[2], { pointerId: 12, button: 0, isPrimary: true, clientX: 100, clientY: 128 });
    fireEvent.pointerMove(window, { pointerId: 12, clientX: 100, clientY: 72 });
    expect(targets()).toHaveLength(1);
    expect(rows[1]).toHaveClass("drop-target--before");
    fireEvent.pointerCancel(window, { pointerId: 12, clientX: 100, clientY: 72 });

    fireEvent.pointerDown(rows[0], { pointerId: 13, button: 0, isPrimary: true, clientX: 100, clientY: 50 });
    fireEvent.pointerMove(window, { pointerId: 13, clientX: 100, clientY: 145 });
    expect(targets()).toHaveLength(1);
    expect(rows[2]).toHaveClass("drop-target--after");
    fireEvent.pointerCancel(window, { pointerId: 13, clientX: 100, clientY: 145 });
  });

  it("never targets or moves a conversation outside its workspace", () => {
    const { workspace } = sidebarFixture();
    const secondConversation = { ...workspace.conversations[0], id: "conversation-second", title: "第二任务" };
    const sourceWorkspace = { ...workspace, conversations: [workspace.conversations[0], secondConversation] };
    const destinationConversation = { ...workspace.conversations[0], id: "conversation-destination", title: "其他工作区任务" };
    const secondWorkspace: Workspace = {
      ...workspace,
      id: "workspace-second",
      name: "第二工作区",
      path: "C:\\second",
      conversations: [destinationConversation]
    };
    const props = sidebarProps(sourceWorkspace);
    const { container } = render(<Sidebar {...props} workspaces={[sourceWorkspace, secondWorkspace]} />);
    const list = container.querySelector<HTMLElement>(".workspace-list")!;
    const groups = container.querySelectorAll<HTMLElement>("[data-workspace-group-id]");
    const sourceHeading = groups[0].querySelector<HTMLElement>(".workspace-heading")!;
    const destinationHeading = groups[1].querySelector<HTMLElement>(".workspace-heading")!;
    const sourceRows = groups[0].querySelectorAll<HTMLElement>(".conversation-row");
    const destinationRow = groups[1].querySelector<HTMLElement>(".conversation-row")!;
    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 240, bottom: top + height, width: 240, height, toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 320);
    setRect(groups[0], 0, 110);
    setRect(sourceHeading, 0, 32);
    setRect(sourceRows[0], 34, 36);
    setRect(sourceRows[1], 72, 36);
    setRect(groups[1], 140, 72);
    setRect(destinationHeading, 140, 32);
    setRect(destinationRow, 174, 36);

    fireEvent.pointerDown(sourceRows[0], { pointerId: 14, button: 0, isPrimary: true, clientX: 100, clientY: 50 });
    fireEvent.pointerMove(window, { pointerId: 14, clientX: 100, clientY: 190 });
    expect(container.querySelectorAll(".conversation-row.drop-target--before, .conversation-row.drop-target--after")).toHaveLength(0);
    fireEvent.pointerUp(window, { pointerId: 14, clientX: 100, clientY: 190 });
    expect(props.onReorderConversation).not.toHaveBeenCalled();
  });

  it("does not start row dragging from action buttons", () => {
    const { workspace, title } = sidebarFixture();
    const secondWorkspace: Workspace = { ...workspace, id: "workspace-second", name: "第二工作区", conversations: [] };
    const props = sidebarProps(workspace);
    render(<Sidebar {...props} workspaces={[workspace, secondWorkspace]} />);

    const deleteButton = screen.getByRole("button", { name: `删除 ${title}` });
    fireEvent.pointerDown(deleteButton, { pointerId: 3, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(deleteButton, { pointerId: 3, clientX: 10, clientY: 120 });
    fireEvent.pointerUp(deleteButton, { pointerId: 3, clientX: 10, clientY: 120 });

    expect(props.onReorderConversation).not.toHaveBeenCalled();
    expect(document.body).not.toHaveClass("pointer-sort-active");
  });

  it("keeps the main row click actions when the pointer stays below the drag threshold", async () => {
    const { workspace } = sidebarFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    const { container } = render(<Sidebar {...props} />);

    await user.click(container.querySelector<HTMLButtonElement>(".conversation-row__main")!);
    expect(props.onSelectConversation).toHaveBeenCalledWith(workspace.id, workspace.conversations[0].id);

    await user.click(container.querySelector<HTMLButtonElement>(".workspace-heading > button:first-of-type")!);
    expect(container.querySelector(".conversation-list")).toHaveClass("conversation-list--closed");
  });

  it("supports keyboard reordering from row controls without rendering drag handles", async () => {
    const { workspace } = sidebarFixture();
    const secondConversation = { ...workspace.conversations[0], id: "conversation-second", title: "第二任务" };
    const withTwoConversations = { ...workspace, conversations: [workspace.conversations[0], secondConversation] };
    const secondWorkspace: Workspace = { ...workspace, id: "workspace-second", name: "第二工作区", conversations: [] };
    const props = sidebarProps(withTwoConversations);
    const user = userEvent.setup();
    const { container } = render(<Sidebar {...props} workspaces={[withTwoConversations, secondWorkspace]} />);

    const workspaceControl = container.querySelector<HTMLButtonElement>(`[data-workspace-group-id="${workspace.id}"] .workspace-heading > button:first-of-type`)!;
    workspaceControl.focus();
    await user.keyboard("{Alt>}{ArrowDown}{/Alt}");
    expect(props.onReorderWorkspace).toHaveBeenCalledWith(workspace.id, secondWorkspace.id, "after");

    const conversationControl = screen.getByText(secondConversation.title).closest<HTMLButtonElement>("button")!;
    conversationControl.focus();
    await user.keyboard("{Alt>}{ArrowUp}{/Alt}");
    expect(props.onReorderConversation).toHaveBeenCalledWith(
      workspace.id,
      secondConversation.id,
      workspace.conversations[0].id,
      "before"
    );
    await user.keyboard("{Alt>}{ArrowLeft}{ArrowRight}{/Alt}");
    expect(props.onReorderConversation).toHaveBeenCalledTimes(1);
    expect(document.querySelector(".sidebar-drag-handle")).not.toBeInTheDocument();
    expect(document.querySelector(".lucide-grip-vertical")).not.toBeInTheDocument();
  });

  it("renders independent workspaces and keeps only the temporary workspace reserved", async () => {
    const { workspace } = sidebarFixture();
    const secondWorkspace: Workspace = {
      ...workspace,
      id: "workspace-two",
      name: "第二工作区",
      path: "C:\\second",
      conversations: []
    };
    const temporaryWorkspace = createTemporaryWorkspace();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    const { container } = render(<Sidebar
      {...props}
      workspaces={[workspace, secondWorkspace, temporaryWorkspace]}
    />);

    const secondHeading = screen.getByText("第二工作区").closest(".workspace-group");
    expect(secondHeading?.querySelector(".lucide-folder")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "删除项目 第二工作区" }))
      .toHaveAttribute("title", "删除项目 第二工作区");
    await user.click(screen.getByRole("button", { name: "在 第二工作区 新建任务" }));
    expect(props.onNewConversation).toHaveBeenCalledWith(secondWorkspace.id, "workspace");

    const heading = screen.getByText("临时项目").closest(".workspace-group");
    expect(heading).not.toBeNull();
    expect(heading?.querySelector(".lucide-folder-clock")).toBeInTheDocument();
    expect(heading?.querySelector(".lucide-message-square-plus")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除项目 临时项目" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "在 临时项目 新建任务" }));
    expect(props.onNewConversation).toHaveBeenCalledWith(temporaryWorkspace.id, "workspace");
    expect(container.querySelectorAll(".workspace-group")).toHaveLength(3);
  });

  it("renders the new-task action without the shortcut symbol", () => {
    const { workspace } = sidebarFixture();
    const props = sidebarProps(workspace);
    const { container } = render(<Sidebar {...props} />);
    const button = screen.getByRole("button", { name: "新建任务" });
    expect(button).toHaveClass("new-task-button");
    expect(button.querySelector("kbd")).not.toBeInTheDocument();
    expect(container.querySelector(".new-task-button kbd")).not.toBeInTheDocument();
    fireEvent.click(button);
    expect(props.onNewConversation).toHaveBeenCalledWith();
  });

  it("renames inline on blur and Enter while Escape and blank input keep the old title", async () => {
    const { workspace, title } = sidebarFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    render(<Sidebar {...props} />);

    expect(screen.queryByRole("button", { name: `${title} 的操作` })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: `删除 ${title}` })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: `重命名 ${title}` }));
    let input = screen.getByRole("textbox", { name: `重命名 ${title}` }) as HTMLInputElement;
    expect(input).toHaveFocus();
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(title.length);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.clear(input);
    await user.type(input, "  新任务名称  ");
    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(props.onRenameConversation).toHaveBeenLastCalledWith(workspace.id, workspace.conversations[0].id, "新任务名称");
    expect(screen.queryByRole("textbox", { name: `重命名 ${title}` })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: `重命名 ${title}` }));
    input = screen.getByRole("textbox", { name: `重命名 ${title}` });
    await user.clear(input);
    await user.type(input, "不应保存");
    await user.keyboard("{Escape}");
    expect(props.onRenameConversation).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("textbox", { name: `重命名 ${title}` })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: `重命名 ${title}` }));
    input = screen.getByRole("textbox", { name: `重命名 ${title}` });
    await user.clear(input);
    await user.type(input, "   ");
    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(props.onRenameConversation).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: `重命名 ${title}` }));
    input = screen.getByRole("textbox", { name: `重命名 ${title}` });
    await user.clear(input);
    await user.type(input, "回车保存");
    await user.keyboard("{Enter}");
    expect(props.onRenameConversation).toHaveBeenLastCalledWith(workspace.id, workspace.conversations[0].id, "回车保存");
    expect(props.onRenameConversation).toHaveBeenCalledTimes(2);
  });

  it("requires a second click to delete a conversation and resets confirmation on blur", async () => {
    const { workspace, title } = sidebarFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    render(<Sidebar {...props} />);

    await user.click(screen.getByRole("button", { name: `删除 ${title}` }));
    expect(props.onDeleteConversation).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: `确认删除 ${title}` })).toHaveClass("confirm-delete--armed");
    expect(screen.getByRole("button", { name: `确认删除 ${title}` })).toHaveTextContent("确认");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(screen.getByRole("button", { name: `删除 ${title}` })).toBeInTheDocument();
    expect(props.onDeleteConversation).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: `删除 ${title}` }));
    await user.click(screen.getByRole("button", { name: `确认删除 ${title}` }));
    expect(props.onDeleteConversation).toHaveBeenCalledWith(
      workspace.conversations[0],
      expect.objectContaining({ id: workspace.id })
    );
  });

  it("adds two-click workspace deletion and resets its confirmation on blur", async () => {
    const { workspace } = sidebarFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    render(<Sidebar {...props} />);

    await user.click(screen.getByRole("button", { name: `删除项目 ${workspace.name}` }));
    expect(props.onDeleteWorkspace).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: `确认删除项目 ${workspace.name}` })).toHaveClass("confirm-delete--armed");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(screen.getByRole("button", { name: `删除项目 ${workspace.name}` })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: `删除项目 ${workspace.name}` }));
    await user.click(screen.getByRole("button", { name: `确认删除项目 ${workspace.name}` }));
    expect(props.onDeleteWorkspace).toHaveBeenCalledWith(expect.objectContaining({ id: workspace.id }));
  });

  it("disables conversation and workspace deletion while a conversation is running", () => {
    const { workspace, title } = sidebarFixture();
    const props = sidebarProps(workspace);

    render(<Sidebar {...props} isConversationRunning={() => true} />);
    expect(screen.getByRole("button", { name: `删除 ${title}` })).toBeDisabled();
    expect(screen.getByRole("button", { name: `删除项目 ${workspace.name}` })).toBeDisabled();
  });

  it("marks every row with its conversation's status and names only the ones that say something", () => {
    const { workspace, title } = sidebarFixture();
    const statuses: Record<string, ConversationStatus> = {
      [workspace.conversations[0].id]: "running",
      "conversation-idle": "idle",
      "conversation-blocked": "blocked",
      "conversation-done": "completed"
    };
    const conversations = [
      workspace.conversations[0],
      { ...workspace.conversations[0], id: "conversation-idle", title: "空闲任务" },
      { ...workspace.conversations[0], id: "conversation-blocked", title: "等批准的任务" },
      { ...workspace.conversations[0], id: "conversation-done", title: "跑完的任务" }
    ];
    const props = sidebarProps({ ...workspace, conversations });

    render(<Sidebar {...props} conversationStatus={(conversationId) => statuses[conversationId] ?? "idle"} />);

    const markOf = (id: string) => document.querySelector(`[data-conversation-id="${id}"] .conversation-status`)!;
    // Every row draws the mark, so a title never shifts when its state changes.
    for (const conversation of conversations) {
      expect(markOf(conversation.id)).toHaveClass(`conversation-status--${statuses[conversation.id]}`);
    }
    expect(screen.getByRole("img", { name: "正在进行" })).toBe(markOf(workspace.conversations[0].id));
    expect(screen.getByRole("img", { name: "等待你处理" })).toBe(markOf("conversation-blocked"));
    expect(screen.getByRole("img", { name: "已完成" })).toBe(markOf("conversation-done"));
    // An idle mark is decoration.
    expect(markOf("conversation-idle")).toHaveAttribute("aria-hidden", "true");
    expect(screen.getAllByRole("img")).toHaveLength(3);
    expect(within(document.querySelector<HTMLElement>(`[data-conversation-id="${workspace.conversations[0].id}"]`)!)
      .getByText(title)).toBeInTheDocument();
  });

  it("shows a conversation's title alone, with no time line under it", () => {
    const { workspace } = sidebarFixture();
    const { container } = render(<Sidebar {...sidebarProps(workspace)} />);
    const main = container.querySelector<HTMLElement>(".conversation-row__main")!;
    expect(main.children).toHaveLength(1);
    expect(main.firstElementChild).toHaveClass("conversation-row__title");
  });

  it("opens and closes a project at once, with its chevron after the name", async () => {
    const user = userEvent.setup();
    const { workspace } = sidebarFixture();
    const { container } = render(<Sidebar {...sidebarProps(workspace)} />);

    const toggle = container.querySelector<HTMLButtonElement>(".workspace-heading__toggle")!;
    expect(toggle.lastElementChild).toHaveClass("workspace-heading__chevron");
    expect(toggle.querySelector(".workspace-heading__name")!.nextElementSibling).toBe(toggle.lastElementChild);

    const list = container.querySelector<HTMLElement>(`#workspace-conversations-${workspace.id}`)!;
    expect(list).not.toHaveAttribute("hidden");
    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(list).toHaveAttribute("hidden");
    // No animated region is left to close gradually.
    expect(container.querySelector(".collapse-region")).toBeNull();
  });

  it("sets and clears a workspace's default conversation preset from the … menu", async () => {
    const user = userEvent.setup();
    const { workspace } = sidebarFixture();
    const props = sidebarProps({ ...workspace, defaultConversationPresetId: "preset-b" });

    render(
      <Sidebar
        {...props}
        conversationPresets={[
          { id: "preset-a", name: "审阅" },
          { id: "preset-b", name: "写代码" }
        ]}
      />
    );

    // The secondary list is collapsed by default, leaving only this item in the first-level menu.
    await user.click(screen.getByRole("button", { name: `${workspace.name} 的更多选项` }));
    expect(screen.queryByRole("menuitemradio", { name: /审阅/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: /默认对话预设/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /审阅/ }));
    expect(props.onSetWorkspaceDefaultPreset).toHaveBeenCalledWith(workspace.id, "preset-a");

    // Reopening starts from the first-level menu; picking the checked preset again clears it.
    await user.click(screen.getByRole("button", { name: `${workspace.name} 的更多选项` }));
    await user.click(screen.getByRole("menuitem", { name: /默认对话预设/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /写代码/ }));
    expect(props.onSetWorkspaceDefaultPreset).toHaveBeenLastCalledWith(workspace.id, "");
  });

  it("disables the preset entry when no conversation preset exists", async () => {
    const user = userEvent.setup();
    const { workspace } = sidebarFixture();
    const props = sidebarProps(workspace);

    render(<Sidebar {...props} conversationPresets={[]} />);

    await user.click(screen.getByRole("button", { name: `${workspace.name} 的更多选项` }));
    expect(screen.getByRole("menuitem", { name: /默认对话预设/ })).toBeDisabled();
  });

  it("asks for a workspace-sourced conversation from the + button and a global one from New task", async () => {
    const user = userEvent.setup();
    const { workspace } = sidebarFixture();
    const props = sidebarProps(workspace);

    render(<Sidebar {...props} />);

    await user.click(screen.getByRole("button", { name: `在 ${workspace.name} 新建任务` }));
    expect(props.onNewConversation).toHaveBeenCalledWith(workspace.id, "workspace");

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    expect(props.onNewConversation).toHaveBeenLastCalledWith();
  });

});

/** A fork beside its source, sharing the fixture's shape, with the fork link set explicitly. */
function forkFixture(): { workspace: Workspace; parent: Conversation; child: Conversation } {
  const { workspace } = sidebarFixture();
  const parent: Conversation = { ...workspace.conversations[0], parentConversationId: null };
  const child: Conversation = {
    ...workspace.conversations[0],
    id: "conversation-child",
    title: "子任务",
    parentConversationId: parent.id
  };
  return { workspace: { ...workspace, conversations: [parent, child] }, parent, child };
}

describe("Sidebar forks", () => {
  it("lists a fork as an ordinary conversation of its project, in the project's order", () => {
    const { workspace, parent, child } = forkFixture();
    const props = sidebarProps(workspace);
    const { container } = render(<Sidebar {...props} />);

    const rows = Array.from(container.querySelectorAll<HTMLElement>(".conversation-row"));
    expect(rows.map((row) => row.dataset.conversationId)).toEqual([parent.id, child.id]);
    const childRow = rows[1];
    expect(childRow.parentElement).toBe(rows[0].parentElement);
    expect(childRow).toHaveClass("sortable-surface");
    expect(childRow).not.toHaveAttribute("data-drag-exclude");
    expect(container.querySelector(".conversation-row__disclosure")).toBeNull();
    expect(container.querySelector(".conversation-list--nested")).toBeNull();
  });

  it("reorders a fork like any other conversation", () => {
    const { workspace, parent, child } = forkFixture();
    const second: Conversation = { ...parent, id: "conversation-second", title: "第二任务", parentConversationId: null };
    const sourceWorkspace = { ...workspace, conversations: [parent, child, second] };
    const props = sidebarProps(sourceWorkspace);
    const { container } = render(<Sidebar {...props} />);

    const list = container.querySelector<HTMLElement>(".workspace-list")!;
    const group = container.querySelector<HTMLElement>("[data-workspace-group-id]")!;
    const heading = group.querySelector<HTMLElement>(".workspace-heading")!;
    const parentRow = container.querySelector<HTMLElement>(`[data-conversation-id="${parent.id}"]`)!;
    const childRow = container.querySelector<HTMLElement>(`[data-conversation-id="${child.id}"]`)!;
    const secondRow = container.querySelector<HTMLElement>(`[data-conversation-id="${second.id}"]`)!;
    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 240, bottom: top + height, width: 240, height, toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 300);
    setRect(group, 0, 130);
    setRect(heading, 0, 28);
    setRect(parentRow, 30, 28);
    setRect(childRow, 60, 28);
    setRect(secondRow, 90, 28);

    // Moving the fork above its source is allowed: nothing ties it to the row it came from.
    fireEvent.pointerDown(childRow, { pointerId: 22, button: 0, isPrimary: true, clientX: 100, clientY: 74 });
    fireEvent.pointerMove(window, { pointerId: 22, clientX: 100, clientY: 34 });
    expect(parentRow).toHaveClass("drop-target--before");
    fireEvent.pointerUp(window, { pointerId: 22, clientX: 100, clientY: 34 });

    expect(props.onReorderConversation).toHaveBeenCalledWith(workspace.id, child.id, parent.id, "before");
    expect(props.onReorderConversation).toHaveBeenCalledTimes(1);
  });
});
