import { fireEvent, render, screen } from "@testing-library/react";
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
});
