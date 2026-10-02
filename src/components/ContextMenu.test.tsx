import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ContextMenu } from "./ContextMenu";
import type { ContextMenuSection } from "./ContextMenu";

describe("ContextMenu", () => {
  afterEach(() => vi.restoreAllMocks());

  it("opens a submenu as a panel of its own beside the menu, read from the moment its row is reached", async () => {
    let answer: (sections: ContextMenuSection[]) => void = () => {};
    const loadChildren = vi.fn(() => new Promise<ContextMenuSection[]>((resolve) => { answer = resolve; }));
    const chosen = vi.fn();
    const onClose = vi.fn();
    render(
      <ContextMenu
        anchor={{ x: 10, y: 10 }}
        sections={[{ id: "file", items: [{ id: "open-with", label: "Open With", loadChildren }] }]}
        label="File actions"
        onClose={onClose}
      />
    );
    const menu = screen.getByRole("menu", { name: "File actions" });
    const row = within(menu).getByRole("menuitem", { name: "Open With" });

    fireEvent.mouseEnter(row);
    expect(loadChildren).toHaveBeenCalledTimes(1);
    fireEvent.click(row);
    await act(async () => { answer([{ id: "apps", items: [{ id: "editor", label: "Editor", onSelect: chosen }] }]); });

    const submenu = screen.getByRole("menu", { name: "Open With" });
    const panel = menu.closest(".context-menu");
    expect(panel).not.toContainElement(submenu);
    expect(submenu.closest(".popover-menu__flyout")?.parentElement).toBe(panel?.parentElement);
    expect(row).toHaveAttribute("aria-expanded", "true");

    // A press in the submenu is a press in the menu, not outside it.
    const editor = within(submenu).getByRole("menuitem", { name: "Editor" });
    fireEvent.mouseDown(editor);
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(editor);
    expect(chosen).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(loadChildren).toHaveBeenCalledTimes(1);
  });
});
