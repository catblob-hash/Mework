import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { createTemporaryWorkspace } from "../lib/workspaces";
import type { Conversation, Workspace } from "../types";
import { Sidebar, SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY } from "./Sidebar";

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
    hasLiveActivity: vi.fn(() => false),
    conversationPresets: [],
    onSetWorkspaceDefaultPreset: vi.fn(),
    isWorkspaceDeleting: vi.fn(() => false),
    onOpenSettings: vi.fn(),
    onClose: vi.fn(),
    onReorderWorkspace: vi.fn(),
    onReorderConversation: vi.fn()
  };
}

// The sidebar keeps its collapse preference in localStorage, so no test may
// inherit another's state.
beforeEach(() => {
  window.localStorage.removeItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY);
});

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
    expect(container.querySelector(".collapse-region")).toHaveClass("collapse-region--closed");
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
    expect(screen.getByRole("button", { name: "删除工作区 第二工作区" }))
      .toHaveAttribute("title", "删除工作区 第二工作区");
    await user.click(screen.getByRole("button", { name: "在 第二工作区 新建任务" }));
    expect(props.onNewConversation).toHaveBeenCalledWith(secondWorkspace.id, "workspace");

    const heading = screen.getByText("临时工作区").closest(".workspace-group");
    expect(heading).not.toBeNull();
    expect(heading?.querySelector(".lucide-folder-clock")).toBeInTheDocument();
    expect(heading?.querySelector(".lucide-message-square-plus")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除工作区 临时工作区" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "在 临时工作区 新建任务" }));
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
    expect(screen.getByRole("button", { name: `确认删除 ${title}` })).toHaveClass("sidebar-delete-confirm--armed");
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

    await user.click(screen.getByRole("button", { name: `删除工作区 ${workspace.name}` }));
    expect(props.onDeleteWorkspace).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: `确认删除工作区 ${workspace.name}` })).toHaveClass("sidebar-delete-confirm--armed");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(screen.getByRole("button", { name: `删除工作区 ${workspace.name}` })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: `删除工作区 ${workspace.name}` }));
    await user.click(screen.getByRole("button", { name: `确认删除工作区 ${workspace.name}` }));
    expect(props.onDeleteWorkspace).toHaveBeenCalledWith(expect.objectContaining({ id: workspace.id }));
  });

  it("disables conversation and workspace deletion while a conversation is running", () => {
    const { workspace, title } = sidebarFixture();
    const props = sidebarProps(workspace);

    render(<Sidebar {...props} isConversationRunning={() => true} />);
    expect(screen.getByRole("button", { name: `删除 ${title}` })).toBeDisabled();
    expect(screen.getByRole("button", { name: `删除工作区 ${workspace.name}` })).toBeDisabled();
  });

  it("paints the blinking dot only for conversations that are doing something", () => {
    const { workspace, title } = sidebarFixture();
    const idle = { ...workspace.conversations[0], id: "conversation-idle", title: "空闲任务" };
    const withBoth = { ...workspace, conversations: [workspace.conversations[0], idle] };
    const props = sidebarProps(withBoth);

    render(
      <Sidebar
        {...props}
        hasLiveActivity={(conversationId) => conversationId === withBoth.conversations[0].id}
      />
    );

    // The pulse is independent of the title, and idle rows have none.
    expect(screen.getAllByRole("img", { name: "正在进行" })).toHaveLength(1);
    const liveRow = document.querySelector(`[data-conversation-id="${withBoth.conversations[0].id}"]`)!;
    const idleRow = document.querySelector(`[data-conversation-id="${idle.id}"]`)!;
    expect(liveRow.querySelector(".conversation-row__pulse")).not.toBeNull();
    expect(idleRow.querySelector(".conversation-row__pulse")).toBeNull();
    expect(within(liveRow as HTMLElement).getByText(title)).toBeInTheDocument();
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
    // Its summary shows the currently active preset name.
    await user.click(screen.getByRole("button", { name: `${workspace.name} 的更多选项` }));
    expect(screen.getByText("写代码")).toBeInTheDocument();
    expect(screen.queryByRole("menuitemradio", { name: /审阅/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: /设置默认对话预设/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /审阅/ }));
    expect(props.onSetWorkspaceDefaultPreset).toHaveBeenCalledWith(workspace.id, "preset-a");

    // Reopening starts from the first-level menu; selecting "Follow the most recent setting" clears it.
    await user.click(screen.getByRole("button", { name: `${workspace.name} 的更多选项` }));
    await user.click(screen.getByRole("menuitem", { name: /设置默认对话预设/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /跟随最近一次设置/ }));
    expect(props.onSetWorkspaceDefaultPreset).toHaveBeenLastCalledWith(workspace.id, "");
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

/** Parent/child pair sharing the fixture's shape, with the nesting link set explicitly. */
function nestedFixture(): { workspace: Workspace; parent: Conversation; child: Conversation } {
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

describe("Sidebar nested conversations", () => {
  it("renders a child inside a nested region under its parent and gives the parent a disclosure", () => {
    const { workspace, parent, child } = nestedFixture();
    const props = sidebarProps(workspace);
    const { container } = render(<Sidebar {...props} />);

    const nested = container.querySelector<HTMLElement>(".conversation-list--nested")!;
    expect(nested).not.toBeNull();
    const childRow = container.querySelector<HTMLElement>(`[data-conversation-id="${child.id}"]`)!;
    expect(nested.contains(childRow)).toBe(true);
    expect(childRow).toHaveClass("conversation-row--nested");
    expect(childRow).not.toHaveClass("sortable-surface");
    expect(childRow).toHaveAttribute("data-drag-exclude");
    expect(within(childRow).getByText(child.title)).toBeInTheDocument();

    const parentRow = container.querySelector<HTMLElement>(`[data-conversation-id="${parent.id}"]`)!;
    expect(parentRow).not.toHaveClass("conversation-row--nested");
    const disclosure = parentRow.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!;
    expect(disclosure).not.toBeNull();
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
    // A leaf child never gets a disclosure of its own.
    expect(childRow.querySelector(".conversation-row__disclosure")).toBeNull();
  });

  it("collapses the nested region from the disclosure without selecting the conversation", async () => {
    const { workspace } = nestedFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    const { container } = render(<Sidebar {...props} />);

    const region = container.querySelector<HTMLElement>(".conversation-list--nested")!.closest(".collapse-region")!;
    expect(region).not.toHaveClass("collapse-region--closed");

    await user.click(container.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!);
    expect(region).toHaveClass("collapse-region--closed");
    expect(region).toHaveAttribute("aria-hidden", "true");
    expect(props.onSelectConversation).not.toHaveBeenCalled();
    expect(container.querySelector(".conversation-row__disclosure")).toHaveAttribute("aria-expanded", "false");
  });

  it("re-expands a collapsed parent when one of its children becomes active", async () => {
    const { workspace, child } = nestedFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    const { container, rerender } = render(<Sidebar {...props} />);

    await user.click(container.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!);
    expect(container.querySelector(".conversation-list--nested")!.closest(".collapse-region"))
      .toHaveClass("collapse-region--closed");

    rerender(<Sidebar {...props} activeConversationId={child.id} />);
    const region = container.querySelector(".conversation-list--nested")!.closest(".collapse-region")!;
    expect(region).not.toHaveClass("collapse-region--closed");
    expect(region).not.toHaveAttribute("aria-hidden");
  });

  it("renders a conversation whose parent id is missing at the top level", () => {
    const { workspace } = sidebarFixture();
    const orphan: Conversation = {
      ...workspace.conversations[0],
      id: "conversation-orphan",
      title: "孤儿任务",
      parentConversationId: "conversation-gone"
    };
    const props = sidebarProps({ ...workspace, conversations: [orphan] });
    const { container } = render(<Sidebar {...props} />);

    const row = container.querySelector<HTMLElement>(`[data-conversation-id="${orphan.id}"]`)!;
    expect(row.parentElement).toHaveClass("conversation-list");
    expect(row.parentElement).not.toHaveClass("conversation-list--nested");
    expect(row).not.toHaveClass("conversation-row--nested");
    expect(container.querySelector(".conversation-row__disclosure")).toBeNull();
    expect(container.querySelector(".conversation-list--nested")).toBeNull();
  });

  it("keeps root reordering unaffected by nested rows", () => {
    const { workspace, parent, child } = nestedFixture();
    const second: Conversation = {
      ...parent,
      id: "conversation-second",
      title: "第二任务",
      parentConversationId: null
    };
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
    setRect(group, 0, 180);
    setRect(heading, 0, 32);
    setRect(parentRow, 34, 36);
    setRect(childRow, 72, 36);
    setRect(secondRow, 110, 36);

    // Dropping over the child's band still targets the nearest root row.
    fireEvent.pointerDown(secondRow, { pointerId: 21, button: 0, isPrimary: true, clientX: 100, clientY: 128 });
    fireEvent.pointerMove(window, { pointerId: 21, clientX: 100, clientY: 80 });
    expect(childRow).not.toHaveClass("drop-target--before");
    expect(childRow).not.toHaveClass("drop-target--after");
    fireEvent.pointerUp(window, { pointerId: 21, clientX: 100, clientY: 80 });

    expect(props.onReorderConversation).toHaveBeenCalledWith(workspace.id, second.id, parent.id, "after");
    expect(props.onReorderConversation).toHaveBeenCalledTimes(1);
    // The child never becomes a reorder target and stays under its parent.
    expect(container.querySelector(".conversation-list--nested")!.contains(childRow)).toBe(true);
  });
});


describe("Sidebar nested conversation collapse persistence", () => {
  /** Grandparent → parent → child, plus an unrelated second branch. */
  function deepFixture() {
    const { workspace } = sidebarFixture();
    const grandparent: Conversation = { ...workspace.conversations[0], parentConversationId: null };
    const parent: Conversation = {
      ...workspace.conversations[0],
      id: "conversation-parent",
      title: "中间任务",
      parentConversationId: grandparent.id
    };
    const child: Conversation = {
      ...workspace.conversations[0],
      id: "conversation-child",
      title: "子任务",
      parentConversationId: parent.id
    };
    const other: Conversation = {
      ...workspace.conversations[0],
      id: "conversation-other",
      title: "另一分支",
      parentConversationId: null
    };
    const otherChild: Conversation = {
      ...workspace.conversations[0],
      id: "conversation-other-child",
      title: "另一分支的子任务",
      parentConversationId: other.id
    };
    return {
      workspace: { ...workspace, conversations: [grandparent, parent, child, other, otherChild] },
      grandparent,
      parent,
      child,
      other,
      otherChild
    };
  }

  const regionOf = (id: string) => document.getElementById(`conversation-children-${id}`);
  const collapsedIds = () => JSON.parse(
    window.localStorage.getItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY) ?? "[]"
  ) as string[];

  afterEach(() => {
    vi.restoreAllMocks();
    window.localStorage.removeItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY);
  });

  it("keeps a parent collapsed across a full remount", async () => {
    const { workspace, parent } = nestedFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    const first = render(<Sidebar {...props} />);

    await user.click(first.container.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!);
    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");
    // A remount is the point: `rerender` would keep the same component state.
    first.unmount();

    render(<Sidebar {...props} />);
    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");
    expect(document.querySelector(".conversation-row__disclosure"))
      .toHaveAttribute("aria-expanded", "false");
  });

  it("keeps a re-expanded parent expanded across a full remount", async () => {
    const { workspace, parent } = nestedFixture();
    const props = sidebarProps(workspace);
    const user = userEvent.setup();
    window.localStorage.setItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY, JSON.stringify([parent.id]));
    const first = render(<Sidebar {...props} />);
    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");

    await user.click(first.container.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!);
    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    first.unmount();

    render(<Sidebar {...props} />);
    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    expect(collapsedIds()).not.toContain(parent.id);
  });

  it("restores each parent of a multi-level tree independently", () => {
    const { workspace, grandparent, parent, other } = deepFixture();
    const props = { ...sidebarProps(workspace), activeConversationId: other.id };
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY,
      JSON.stringify([parent.id, other.id])
    );
    render(<Sidebar {...props} />);

    expect(regionOf(grandparent.id)).not.toHaveClass("collapse-region--closed");
    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");
    expect(regionOf(other.id)).toHaveClass("collapse-region--closed");
  });

  it("expands a branch that the stored preference never mentions", () => {
    const { workspace, parent } = nestedFixture();
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY,
      JSON.stringify(["conversation-from-another-session"])
    );
    render(<Sidebar {...sidebarProps(workspace)} />);

    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    // An id belonging to data this session has not loaded stays stored.
    expect(collapsedIds()).toContain("conversation-from-another-session");
  });

  it("persists the ancestor chain opened for the active child without touching other branches", () => {
    const { workspace, grandparent, parent, child, other } = deepFixture();
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY,
      JSON.stringify([grandparent.id, parent.id, other.id])
    );
    const props = { ...sidebarProps(workspace), activeConversationId: child.id };
    const first = render(<Sidebar {...props} />);
    expect(regionOf(grandparent.id)).not.toHaveClass("collapse-region--closed");
    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    expect(regionOf(other.id)).toHaveClass("collapse-region--closed");
    first.unmount();

    // Remounted on a different conversation: the automatic expansion had to be saved.
    render(<Sidebar {...sidebarProps(workspace)} activeConversationId={other.id} />);
    expect(regionOf(grandparent.id)).not.toHaveClass("collapse-region--closed");
    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    expect(regionOf(other.id)).toHaveClass("collapse-region--closed");
  });

  it("falls back to an expanded tree when the stored value is unusable", () => {
    const { workspace, parent } = nestedFixture();
    for (const raw of ["", "not json", "{}", "null", "[1,2]", '[""]']) {
      window.localStorage.setItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY, raw);
      const view = render(<Sidebar {...sidebarProps(workspace)} />);
      expect(regionOf(parent.id), raw).not.toHaveClass("collapse-region--closed");
      view.unmount();
    }
  });

  it("keeps a mixed stored array usable by ignoring its non-string entries", () => {
    const { workspace, parent } = nestedFixture();
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY,
      JSON.stringify([7, null, parent.id])
    );
    render(<Sidebar {...sidebarProps(workspace)} />);

    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");
  });

  it("still renders when storage refuses to be read or written", async () => {
    const { workspace, parent } = nestedFixture();
    const user = userEvent.setup();
    const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage denied");
    });
    const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage denied");
    });

    const { container } = render(<Sidebar {...sidebarProps(workspace)} />);
    expect(regionOf(parent.id)).not.toHaveClass("collapse-region--closed");
    await user.click(container.querySelector<HTMLButtonElement>(".conversation-row__disclosure")!);
    expect(regionOf(parent.id)).toHaveClass("collapse-region--closed");
    expect(read).toHaveBeenCalled();
    expect(write).toHaveBeenCalled();
  });
});
