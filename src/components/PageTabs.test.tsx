import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PageTabs, fitPageTabs } from "./PageTabs";
import type { PageTab, PageTabsProps } from "./PageTabs";

const TABS: PageTab[] = [
  { id: "a", label: "alpha" },
  { id: "b", label: "beta", title: "the beta page" },
  { id: "c", label: "gamma" },
  { id: "d", label: "delta" }
];

/** A strip the way a pane holds one: it owns the selection and the order the callbacks report. */
function Harness(props: Partial<PageTabsProps> & { initialActive?: string }) {
  const { initialActive = "a", tabs: initialTabs = TABS, ...rest } = props;
  const [tabs, setTabs] = useState(initialTabs);
  const [activeId, setActiveId] = useState<string | null>(initialActive);
  return (
    <PageTabs
      tabs={tabs}
      activeId={activeId}
      ariaLabel="Pages"
      moreLabel="More pages"
      onSelect={setActiveId}
      onReorder={(ids) => setTabs(ids.map((id) => tabs.find((tab) => tab.id === id) as PageTab))}
      {...rest}
    />
  );
}

function setup(props: Partial<PageTabsProps> & { initialActive?: string } = {}) {
  const handlers = {
    onSelect: vi.fn(),
    onClose: vi.fn(),
    onReorder: vi.fn()
  };
  const view = render(<PageTabs tabs={TABS} activeId="a" ariaLabel="Pages" moreLabel="More pages" {...handlers} {...props} />);
  return { ...handlers, view };
}

function rect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    x: left, y: top, left, top, width, height, right: left + width, bottom: top + height, toJSON: () => ({})
  } as DOMRect;
}

/**
 * Gives jsdom, which lays nothing out, a bar `bar` px wide: every tab 100px at rest (or as
 * `widths` says), a 24px overflow trigger, strip tabs 102px apart, and menu rows 24px tall
 * under the strip.
 */
function layOut({ bar, widths = {} }: { bar: number; widths?: Record<string, number> }) {
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("page-tabs") ? bar : 0;
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const measured = this.dataset.measureTab;
    if (measured !== undefined) return rect(0, 0, widths[measured] ?? 100, 24);
    if (this.dataset.measureTrigger !== undefined) return rect(0, 0, 24, 24);
    if (this.classList.contains("page-tabs__strip")) return rect(0, 4, bar, 24);
    if (this.classList.contains("page-tabs__menu")) {
      return rect(0, 40, 200, this.querySelectorAll("[data-page-tab-slot]").length * 24 + 8);
    }
    if (this.dataset.pageTabSlot !== undefined) {
      const index = Array.from(this.parentElement?.children ?? []).indexOf(this);
      return this.closest(".page-tabs__menu") ? rect(0, 44 + index * 24, 200, 24) : rect(index * 102, 4, 100, 24);
    }
    return rect(0, 0, 0, 0);
  });
}

const tabNames = () => screen.getAllByRole("tab").map((element) => element.textContent);
const tab = (name: string) => screen.getByRole("tab", { name });
const tabBox = (name: string) => tab(name).closest(".page-tab") as HTMLElement;
const menuRows = () => within(screen.getByRole("menu", { name: "More pages" }))
  .getAllByRole("menuitem").map((element) => element.textContent);
const menuRow = (name: string) => within(screen.getByRole("menu", { name: "More pages" }))
  .getByRole("menuitem", { name });

/** Presses at `from`, moves past the drag threshold to `to`, and lets go there. */
function drag(element: HTMLElement, from: { x: number; y: number }, to: { x: number; y: number }) {
  fireEvent.mouseDown(element, { button: 0, clientX: from.x, clientY: from.y });
  fireEvent.pointerDown(element, { pointerId: 7, button: 0, isPrimary: true, clientX: from.x, clientY: from.y });
  fireEvent.pointerMove(window, { pointerId: 7, clientX: to.x, clientY: to.y });
  fireEvent.pointerUp(window, { pointerId: 7, clientX: to.x, clientY: to.y });
}

afterEach(() => vi.restoreAllMocks());

describe("fitPageTabs", () => {
  const metrics = (available: number, widths: Record<string, number> = {}) => ({
    widths: new Map(["a", "b", "c", "d"].map((id) => [id, widths[id] ?? 100])),
    available,
    gap: 2,
    trigger: 24
  });

  it("shows every tab when nothing was measured, or when they all fit", () => {
    expect(fitPageTabs(["a", "b"], "a", null)).toEqual({ shown: ["a", "b"], hidden: [] });
    expect(fitPageTabs(["a", "b"], "a", { ...metrics(0) })).toEqual({ shown: ["a", "b"], hidden: [] });
    expect(fitPageTabs(["a", "b", "c", "d"], "a", metrics(406))).toEqual({ shown: ["a", "b", "c", "d"], hidden: [] });
  });

  it("shows the longest run that fits beside the trigger, and hides the rest in order", () => {
    // 290 - 2 - 24 leaves 264: two 100px tabs and their gap, not three.
    expect(fitPageTabs(["a", "b", "c", "d"], "a", metrics(290))).toEqual({ shown: ["a", "b"], hidden: ["c", "d"] });
  });

  it("gives the active tab the run's last slot, and more when it is the wider", () => {
    expect(fitPageTabs(["a", "b", "c", "d"], "d", metrics(290))).toEqual({ shown: ["a", "d"], hidden: ["b", "c"] });
    expect(fitPageTabs(["a", "b", "c", "d"], "d", metrics(290, { d: 200 })))
      .toEqual({ shown: ["d"], hidden: ["a", "b", "c"] });
  });

  it("keeps one tab on screen however narrow the bar", () => {
    expect(fitPageTabs(["a", "b"], "b", metrics(30))).toEqual({ shown: ["b"], hidden: ["a"] });
    expect(fitPageTabs(["a", "b"], null, metrics(30))).toEqual({ shown: ["a"], hidden: ["b"] });
  });
});

describe("PageTabs", () => {
  it("draws a tab per page, marks the active one, and points it at its panel", () => {
    setup({ activeId: "b", panelId: (id) => `panel-${id}` });

    expect(screen.getByRole("tablist", { name: "Pages" })).toBeInTheDocument();
    expect(tabNames()).toEqual(["alpha", "beta", "gamma", "delta"]);
    expect(tab("beta")).toHaveAttribute("aria-selected", "true");
    expect(tab("beta")).toHaveAttribute("aria-controls", "panel-b");
    expect(tab("alpha")).toHaveAttribute("aria-selected", "false");
    // One tab stop for the whole strip: the active tab.
    expect(tab("beta")).toHaveAttribute("tabindex", "0");
    expect(tab("alpha")).toHaveAttribute("tabindex", "-1");
    expect(tab("beta")).toHaveAttribute("title", "the beta page");
    expect(tab("alpha")).toHaveAttribute("title", "alpha");
  });

  it("selects on click, and closes from the × or with a middle click", async () => {
    const user = userEvent.setup();
    const { onSelect, onClose } = setup({
      tabs: [...TABS.slice(0, 2), { id: "c", label: "gamma", closeDisabled: true }, { id: "d", label: "delta", closable: false }]
    });

    await user.click(tab("beta"));
    expect(onSelect).toHaveBeenCalledExactlyOnceWith("b");
    await user.click(within(tabBox("alpha")).getByRole("button", { name: "关闭 alpha" }));
    expect(onClose).toHaveBeenLastCalledWith("a");
    fireEvent(tab("beta"), new MouseEvent("auxclick", { bubbles: true, button: 1 }));
    expect(onClose).toHaveBeenLastCalledWith("b");
    // A spent × is still shown; a tab that cannot close has none, and ignores the middle button.
    expect(within(tabBox("gamma")).getByRole("button", { name: "关闭 gamma" })).toBeDisabled();
    expect(within(tabBox("delta")).queryByRole("button")).not.toBeInTheDocument();
    fireEvent(tab("delta"), new MouseEvent("auxclick", { bubbles: true, button: 1 }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("closes the focused tab with Delete only where the caller asks for it", async () => {
    const user = userEvent.setup();
    const { onClose, view } = setup();
    tab("alpha").focus();
    await user.keyboard("{Delete}");
    expect(onClose).not.toHaveBeenCalled();

    view.rerender(
      <PageTabs tabs={TABS} activeId="a" ariaLabel="Pages" moreLabel="More pages" onSelect={vi.fn()} onClose={onClose} closeOnDeleteKey />
    );
    tab("alpha").focus();
    await user.keyboard("{Delete}");
    expect(onClose).toHaveBeenCalledExactlyOnceWith("a");
  });

  it("moves the selection with the arrow keys in the caller's order, wrapping, and focus follows", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    tab("alpha").focus();
    await user.keyboard("{ArrowLeft}");
    expect(tab("delta")).toHaveAttribute("aria-selected", "true");
    expect(tab("delta")).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(tab("alpha")).toHaveFocus();
    await user.keyboard("{ArrowRight}{End}");
    expect(tab("delta")).toHaveFocus();
    await user.keyboard("{Home}");
    expect(tab("alpha")).toHaveAttribute("aria-selected", "true");
  });

  it("moves the focused tab a slot with Ctrl+Shift+Arrow, and only when the order is the caller's", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    tab("beta").focus();
    await user.keyboard("{Control>}{Shift>}{ArrowLeft}{/Shift}{/Control}");
    expect(tabNames()).toEqual(["beta", "alpha", "gamma", "delta"]);
    expect(tab("beta")).toHaveFocus();
    // Past the start there is nowhere to go.
    await user.keyboard("{Control>}{Shift>}{ArrowLeft}{/Shift}{/Control}");
    expect(tabNames()).toEqual(["beta", "alpha", "gamma", "delta"]);
  });

  it("does not move tabs from the keyboard without onReorder", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<PageTabs tabs={TABS} activeId="a" ariaLabel="Pages" moreLabel="More pages" onSelect={onSelect} />);
    tab("alpha").focus();
    await user.keyboard("{Control>}{Shift>}{ArrowRight}{/Shift}{/Control}");
    expect(tabNames()).toEqual(["alpha", "beta", "gamma", "delta"]);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("caps every tab at the maximum width, keeping the full label as its hover text", () => {
    const long = "a-very-long-page-name-that-would-otherwise-take-the-whole-bar";
    const { view } = setup({ tabs: [{ id: "a", label: long }], maxTabWidth: 120 });
    const root = view.container.querySelector(".page-tabs") as HTMLElement;
    expect(root.style.getPropertyValue("--page-tab-max-width")).toBe("120px");
    expect(tab(long)).toHaveAttribute("title", long);
  });

  it("draws an in-place editor instead of a tab's label, and reports double clicks", async () => {
    const user = userEvent.setup();
    const onTabDoubleClick = vi.fn();
    setup({
      onTabDoubleClick,
      renderEditor: (candidate) => (candidate.id === "c" ? <input aria-label="Rename" defaultValue={candidate.label} /> : null)
    });

    expect(screen.getByRole("textbox", { name: "Rename" })).toHaveValue("gamma");
    expect(screen.queryByRole("tab", { name: "gamma" })).not.toBeInTheDocument();
    await user.dblClick(tab("beta"));
    expect(onTabDoubleClick).toHaveBeenCalledWith("b");
  });

  it("offers a ⋮ menu of the caller's actions on each tab", async () => {
    const user = userEvent.setup();
    const reveal = vi.fn();
    setup({
      tabMenu: (id) => [{ id: "actions", items: [{ id: "reveal", label: "Reveal", onSelect: () => reveal(id) }] }]
    });

    await user.click(within(tabBox("gamma")).getByRole("button", { name: "gamma 的操作" }));
    await user.click(within(screen.getByRole("menu", { name: "gamma 的操作" })).getByRole("menuitem", { name: "Reveal" }));
    expect(reveal).toHaveBeenCalledExactlyOnceWith("c");
  });

  it("puts trailing controls after the strip", () => {
    setup({ trailing: <button type="button">Add</button> });
    const add = screen.getByRole("button", { name: "Add" });
    expect(screen.getByRole("tablist").compareDocumentPosition(add) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  describe("overflow", () => {
    it("shows every tab and no overflow trigger while nothing is laid out", () => {
      setup();
      expect(tabNames()).toEqual(["alpha", "beta", "gamma", "delta"]);
      expect(screen.queryByRole("button", { name: "More pages" })).not.toBeInTheDocument();
    });

    it("stays away when every tab fits", () => {
      layOut({ bar: 500 });
      setup();
      expect(tabNames()).toEqual(["alpha", "beta", "gamma", "delta"]);
      expect(screen.queryByRole("button", { name: "More pages" })).not.toBeInTheDocument();
    });

    it("lists only the tabs that did not fit, and selects one from the menu", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      const { onSelect, onClose } = setup();

      expect(tabNames()).toEqual(["alpha", "beta"]);
      const trigger = screen.getByRole("button", { name: "More pages" });
      expect(trigger).toHaveAttribute("aria-expanded", "false");
      await user.click(trigger);
      expect(trigger).toHaveAttribute("aria-expanded", "true");
      expect(menuRows()).toEqual(["gamma", "delta"]);

      await user.click(within(screen.getByRole("menu")).getByRole("button", { name: "关闭 delta" }));
      expect(onClose).toHaveBeenCalledExactlyOnceWith("d");
      await user.click(menuRow("gamma"));
      expect(onSelect).toHaveBeenCalledExactlyOnceWith("c");
      expect(screen.queryByRole("menu", { name: "More pages" })).not.toBeInTheDocument();
    });

    it("gives the active tab the last visible slot without changing the caller's order", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      render(<Harness />);

      await user.click(screen.getByRole("button", { name: "More pages" }));
      await user.click(menuRow("delta"));
      expect(tabNames()).toEqual(["alpha", "delta"]);
      expect(tab("delta")).toHaveAttribute("aria-selected", "true");
      expect(tab("delta")).toHaveFocus();
      await user.click(screen.getByRole("button", { name: "More pages" }));
      expect(menuRows()).toEqual(["beta", "gamma"]);
    });

    it("walks the menu with the arrow keys and closes it with Escape", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      setup({ onClose: undefined });

      const trigger = screen.getByRole("button", { name: "More pages" });
      trigger.focus();
      await user.keyboard("{ArrowDown}");
      expect(menuRow("gamma")).toHaveFocus();
      await user.keyboard("{ArrowDown}");
      expect(menuRow("delta")).toHaveFocus();
      await user.keyboard("{ArrowDown}");
      expect(menuRow("gamma")).toHaveFocus();
      await user.keyboard("{ArrowUp}");
      expect(menuRow("delta")).toHaveFocus();
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("menu", { name: "More pages" })).not.toBeInTheDocument();
    });
  });

  describe("dragging", () => {
    it("reorders within the strip, marking the source and the landing spot, and swallows the click", () => {
      layOut({ bar: 500 });
      const { onReorder, onSelect } = setup();
      const alpha = tab("alpha");

      fireEvent.pointerDown(alpha, { pointerId: 7, button: 0, isPrimary: true, clientX: 50, clientY: 16 });
      fireEvent.pointerMove(window, { pointerId: 7, clientX: 280, clientY: 16 });
      expect(tabBox("alpha")).toHaveClass("page-tab--dragging");
      expect(tabBox("gamma")).toHaveClass("page-tab--drop-after");
      fireEvent.pointerUp(window, { pointerId: 7, clientX: 280, clientY: 16 });
      fireEvent.click(alpha);

      expect(onReorder).toHaveBeenCalledExactlyOnceWith(["b", "c", "a", "d"]);
      expect(onSelect).not.toHaveBeenCalled();
      expect(tabBox("alpha")).not.toHaveClass("page-tab--dragging");
      expect(tabBox("gamma")).not.toHaveClass("page-tab--drop-after");
    });

    it("treats a press that stays under the threshold as a click", () => {
      layOut({ bar: 500 });
      const { onReorder } = setup();
      drag(tab("alpha"), { x: 50, y: 16 }, { x: 53, y: 17 });
      expect(onReorder).not.toHaveBeenCalled();
    });

    it("never starts a drag from a tab's own controls", () => {
      layOut({ bar: 500 });
      const { onReorder } = setup();
      drag(within(tabBox("alpha")).getByRole("button", { name: "关闭 alpha" }), { x: 90, y: 16 }, { x: 280, y: 16 });
      expect(onReorder).not.toHaveBeenCalled();
    });

    it("moves a menu row within the menu and onto the strip", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      const { onReorder } = setup();
      await user.click(screen.getByRole("button", { name: "More pages" }));

      // delta's row sits at 68–92; above gamma's middle (56) is before gamma.
      drag(menuRow("delta"), { x: 50, y: 80 }, { x: 50, y: 50 });
      expect(onReorder).toHaveBeenLastCalledWith(["a", "b", "d", "c"]);
      // The strip is a drop target while the menu is open, and pressing a row keeps it open.
      drag(menuRow("delta"), { x: 50, y: 80 }, { x: 20, y: 16 });
      expect(onReorder).toHaveBeenLastCalledWith(["d", "a", "b", "c"]);
      expect(screen.getByRole("menu", { name: "More pages" })).toBeInTheDocument();
    });

    it("moves a strip tab into the open menu", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      const { onReorder } = setup();
      await user.click(screen.getByRole("button", { name: "More pages" }));

      // Pressing a tab must not dismiss the menu it is about to be dropped into.
      drag(tab("beta"), { x: 150, y: 16 }, { x: 50, y: 88 });
      expect(onReorder).toHaveBeenCalledExactlyOnceWith(["a", "c", "d", "b"]);
      expect(screen.getByRole("menu", { name: "More pages" })).toBeInTheDocument();
    });

    it("cancels on Escape, leaving the open menu alone", async () => {
      layOut({ bar: 290 });
      const user = userEvent.setup();
      const { onReorder } = setup();
      await user.click(screen.getByRole("button", { name: "More pages" }));
      const row = menuRow("delta");

      fireEvent.pointerDown(row, { pointerId: 7, button: 0, isPrimary: true, clientX: 50, clientY: 80 });
      fireEvent.pointerMove(window, { pointerId: 7, clientX: 50, clientY: 50 });
      expect(row.closest(".page-tabs__menu-row")).toHaveClass("page-tabs__menu-row--dragging");
      fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
      fireEvent.pointerUp(window, { pointerId: 7, clientX: 50, clientY: 50 });

      expect(onReorder).not.toHaveBeenCalled();
      expect(screen.getByRole("menu", { name: "More pages" })).toBeInTheDocument();
      expect(row.closest(".page-tabs__menu-row")).not.toHaveClass("page-tabs__menu-row--dragging");
    });

    it("does not drag at all without onReorder", () => {
      layOut({ bar: 500 });
      const onSelect = vi.fn();
      render(<PageTabs tabs={TABS} activeId="a" ariaLabel="Pages" moreLabel="More pages" onSelect={onSelect} />);
      fireEvent.pointerDown(tab("alpha"), { pointerId: 7, button: 0, isPrimary: true, clientX: 50, clientY: 16 });
      fireEvent.pointerMove(window, { pointerId: 7, clientX: 280, clientY: 16 });
      expect(tabBox("alpha")).not.toHaveClass("page-tab--dragging");
      fireEvent.pointerUp(window, { pointerId: 7, clientX: 280, clientY: 16 });
    });
  });
});
