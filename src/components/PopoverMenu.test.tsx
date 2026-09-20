import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { configureI18n } from "../i18n";
import { PopoverMenu } from "./PopoverMenu";

function renderMenu() {
  render(
    <PopoverMenu
      trigger={<span>模型</span>}
      triggerLabel="模型"
      menuLabel="模型"
      sections={[{
        id: "models",
        items: [
          { id: "a", label: "模型 A" },
          { id: "b", label: "模型 B" }
        ]
      }]}
    />
  );
}

describe("PopoverMenu", () => {
  beforeEach(() => configureI18n("zh-CN"));

  it("stays open while its own list scrolls", async () => {
    const user = userEvent.setup();
    renderMenu();
    await user.click(screen.getByRole("button", { name: "模型" }));

    const list = screen.getByRole("menu", { name: "模型" }).querySelector(".popover-menu__list");
    expect(list).not.toBeNull();
    // A real scroll event does not bubble; it reaches the window listener through capture, which is
    // exactly how `fireEvent.scroll` propagates it here.
    fireEvent.scroll(list as Element);

    expect(screen.getByRole("menu", { name: "模型" })).toBeInTheDocument();
  });

  it("closes when the page behind it scrolls, because the measured position goes stale", async () => {
    const user = userEvent.setup();
    renderMenu();
    await user.click(screen.getByRole("button", { name: "模型" }));
    expect(screen.getByRole("menu", { name: "模型" })).toBeInTheDocument();

    fireEvent.scroll(document);

    expect(screen.queryByRole("menu", { name: "模型" })).toBeNull();
  });
  /* Two nesting shapes, because they answer different questions. Inline is a
     list that happens to have groups in it; a flyout is a second step that
     refines the row it came from, so the row stays visible beside its choices.
     Only the second one needs the panel to stop clipping what it contains. */
  it("opens a nested list beside its row in flyout mode, not inside the scrolling list", async () => {
    const user = userEvent.setup();
    render(
      <PopoverMenu
        trigger={<span>后端</span>}
        triggerLabel="后端"
        menuLabel="后端"
        submenu="flyout"
        sections={[{
          id: "backends",
          items: [
            {
              id: "native",
              label: "原生",
              checked: true,
              children: [{ id: "v1", label: "web_search_20250305", checked: true }]
            },
            { id: "provider", label: "提供商", checked: false }
          ]
        }]}
      />
    );
    await user.click(screen.getByRole("button", { name: "后端" }));

    const panel = screen.getByRole("menu", { name: "后端" });
    expect(panel).toHaveClass("popover-menu__panel--flyout");
    const native = screen.getByRole("menuitemradio", { name: "原生" });
    expect(native).toHaveAttribute("aria-expanded", "false");
    await user.click(native);

    expect(native).toHaveAttribute("aria-expanded", "true");
    const nested = screen.getByRole("menu", { name: "原生" });
    expect(nested).toHaveClass("popover-menu__submenu--flyout");
    // The row that owns the flyout is its positioning context; without it the
    // panel would be placed against the window instead of beside the row.
    expect(nested.parentElement).toHaveClass("popover-menu__row--branch");
    expect(within(nested).getByRole("menuitemradio", { name: "web_search_20250305" }))
      .toBeInTheDocument();
  });

  it("nests inline by default, so an ordinary menu keeps one scrolling list", async () => {
    const user = userEvent.setup();
    render(
      <PopoverMenu
        trigger={<span>分组</span>}
        triggerLabel="分组"
        menuLabel="分组"
        sections={[{
          id: "groups",
          items: [{ id: "g", label: "一组", children: [{ id: "x", label: "成员" }] }]
        }]}
      />
    );
    await user.click(screen.getByRole("button", { name: "分组" }));
    await user.click(screen.getByRole("menuitem", { name: "一组" }));

    const nested = screen.getByRole("menu", { name: "一组" });
    expect(nested).toHaveClass("popover-menu__submenu");
    expect(nested).not.toHaveClass("popover-menu__submenu--flyout");
    expect(screen.getByRole("menu", { name: "分组" }))
      .not.toHaveClass("popover-menu__panel--flyout");
  });
});
