import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PreviewPageTabs } from "./PreviewPageTabs";
import type { PreviewPageTabsProps } from "./PreviewPageTabs";

function setup(overrides: Partial<PreviewPageTabsProps> = {}) {
  const handlers = { onSelect: vi.fn(), onClose: vi.fn() };
  const add = vi.fn();
  render(
    <PreviewPageTabs
      tabs={[
        { id: "conv", label: "web", badge: "1" },
        { id: "conv#tab_a", label: "localhost:5173", badge: "2", title: "http://localhost:5173/" }
      ]}
      activeId="conv"
      add={add}
      {...handlers}
      {...overrides}
    />
  );
  return { ...handlers, add };
}

afterEach(() => vi.restoreAllMocks());

describe("PreviewPageTabs", () => {
  it("gives every page a tab, marks the one on screen, and says which workspace each is", () => {
    setup();
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["1web", "2localhost:5173"]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    expect(tabs[1]).toHaveAttribute("aria-selected", "false");
    expect(tabs[1]).toHaveAttribute("title", "http://localhost:5173/");
  });

  it("switches and closes pages from their tabs", async () => {
    const user = userEvent.setup();
    const { onSelect, onClose } = setup();
    await user.click(screen.getByRole("tab", { name: /localhost:5173/ }));
    expect(onSelect).toHaveBeenCalledWith("conv#tab_a");
    const second = screen.getByRole("tab", { name: /localhost:5173/ }).closest(".preview-page-tab") as HTMLElement;
    await user.click(within(second).getByRole("button", { name: "关闭页面 localhost:5173" }));
    expect(onClose).toHaveBeenCalledWith("conv#tab_a");
  });

  it("opens a page straight away when the conversation has one workspace", async () => {
    const user = userEvent.setup();
    const { add } = setup();
    await user.click(screen.getByRole("button", { name: "新建预览页面" }));
    expect(add).toHaveBeenCalledTimes(1);
  });

  it("asks which workspace when there is more than one, with no pane row in the menu", async () => {
    const user = userEvent.setup();
    const pick = vi.fn();
    setup({
      add: [
        { id: "workspace-1", label: "web", hint: "1", onSelect: () => pick(1) },
        { id: "workspace-2", label: "api", hint: "2", onSelect: () => pick(2) }
      ]
    });
    await user.click(screen.getByRole("button", { name: "新建预览页面" }));
    const menu = await screen.findByRole("menu", { name: "为哪个工作区打开预览" });
    expect(within(menu).queryByText(/预览面板/)).not.toBeInTheDocument();
    await user.click(within(menu).getByRole("menuitem", { name: /api/ }));
    expect(pick).toHaveBeenCalledWith(2);
  });
});
