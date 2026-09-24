import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalTabBar } from "./TerminalTabBar";
import type { TerminalTabBarProps } from "./TerminalTabBar";

function setup(overrides: Partial<TerminalTabBarProps> = {}) {
  const handlers = {
    onSelect: vi.fn(),
    onClose: vi.fn(),
    onRename: vi.fn()
  };
  const onAddShell = vi.fn();
  const view = render(
    <TerminalTabBar
      tabs={[{ id: "terminal-1", label: "终端 1" }, { id: "terminal-2", label: "终端 2" }]}
      activeId="terminal-1"
      panelId={(terminalId) => `panel-${terminalId}`}
      add={{
        sections: [{
          id: "shells",
          items: ["zsh", "bash"].map((shell) => ({
            id: shell,
            label: shell,
            onSelect: () => onAddShell(shell)
          }))
        }]
      }}
      {...handlers}
      {...overrides}
    />
  );
  return { ...handlers, onAddShell, view };
}

function tab(name: string) {
  return screen.getByRole("tab", { name });
}
function closeControl(name: string) {
  return within(screen.getByRole("tab", { name }).closest(".terminal-tab") as HTMLElement)
    .getByRole("button", { name: "关闭终端" });
}

afterEach(() => vi.restoreAllMocks());

describe("TerminalTabBar", () => {
  it("gives every terminal a tab and points it at that terminal's panel", () => {
    setup();

    expect(screen.getAllByRole("tab").map((element) => element.textContent))
      .toEqual(["终端 1", "终端 2"]);
    expect(tab("终端 1")).toHaveAttribute("aria-selected", "true");
    expect(tab("终端 1")).toHaveAttribute("aria-controls", "panel-terminal-1");
    expect(tab("终端 2")).toHaveAttribute("aria-selected", "false");
  });

  it("selects on click and opens another terminal from the +'s menu", async () => {
    const user = userEvent.setup();
    const { onSelect, onAddShell } = setup();

    await user.click(tab("终端 2"));
    expect(onSelect).toHaveBeenCalledWith("terminal-2");
    await user.click(screen.getByRole("button", { name: "新建终端" }));
    // The + only asks; nothing opens until a shell is picked.
    expect(onAddShell).not.toHaveBeenCalled();
    const menu = screen.getByRole("menu", { name: "新建终端" });
    await user.click(within(menu).getByRole("menuitem", { name: "bash" }));
    expect(onAddShell).toHaveBeenCalledExactlyOnceWith("bash");
  });

  /** The tab is the shell's only place on screen, so this control is what ends the shell. */
  it("closes one terminal from its own tab, and spends the control while it is going", async () => {
    const user = userEvent.setup();
    const { onClose } = setup({ closingIds: new Set(["terminal-2"]) });

    await user.click(closeControl("终端 1"));
    expect(onClose).toHaveBeenCalledWith("terminal-1");
    expect(closeControl("终端 2")).toBeDisabled();
  });

  it("moves between tabs with the arrow keys, wrapping at the ends", async () => {
    const user = userEvent.setup();
    const { onSelect } = setup();

    tab("终端 1").focus();
    await user.keyboard("{ArrowRight}");
    expect(onSelect).toHaveBeenLastCalledWith("terminal-2");
    await user.keyboard("{ArrowLeft}");
    expect(onSelect).toHaveBeenLastCalledWith("terminal-2");
  });

  describe("renaming", () => {
    it("commits a new name on Enter", async () => {
      const user = userEvent.setup();
      const { onRename } = setup();

      await user.dblClick(tab("终端 1"));
      const field = screen.getByRole("textbox", { name: "重命名终端" });
      expect(field).toHaveValue("终端 1");
      await user.clear(field);
      await user.type(field, "构建{Enter}");
      expect(onRename).toHaveBeenCalledWith("terminal-1", "构建");
      expect(screen.queryByRole("textbox", { name: "重命名终端" })).not.toBeInTheDocument();
    });

    it("keeps the old name on Escape", async () => {
      const user = userEvent.setup();
      const { onRename } = setup();

      await user.dblClick(tab("终端 1"));
      await user.type(screen.getByRole("textbox", { name: "重命名终端" }), "构建{Escape}");
      expect(onRename).not.toHaveBeenCalled();
      expect(tab("终端 1")).toBeInTheDocument();
    });
  });

  describe("the overflow selector", () => {
    it("stays away while every tab fits", () => {
      setup();
      expect(screen.queryByRole("button", { name: "更多终端" })).not.toBeInTheDocument();
    });

    it("appears once a tab is out of reach and reaches it", async () => {
      vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockReturnValue(400);
      vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(120);
      const user = userEvent.setup();
      const { onSelect } = setup();

      await user.click(screen.getByRole("button", { name: "更多终端" }));
      await user.click(screen.getByRole("menuitemradio", { name: "终端 2" }));
      expect(onSelect).toHaveBeenCalledWith("terminal-2");
    });
  });
});
