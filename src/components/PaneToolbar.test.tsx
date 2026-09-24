import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { PaneToolbar } from "./PaneToolbar";
import type { PaneToolbarButton, PaneToolbarMenuItem, PaneToolbarProps } from "./PaneToolbar";

type ButtonPatches = Partial<Record<PaneToolbarButton["id"], Partial<PaneToolbarButton>>>;
type MenuPatches = Partial<Record<PaneToolbarMenuItem["id"], Partial<PaneToolbarMenuItem>>>;

const BUTTON_SPECS: { id: PaneToolbarButton["id"]; label: string; activeLabel: string }[] = [
  { id: "terminal", label: "终端", activeLabel: "终端（命令运行中）" },
  { id: "review", label: "审阅", activeLabel: "审阅（有未提交改动）" },
  { id: "preview", label: "预览", activeLabel: "预览（模型正在操作页面）" }
];

const MENU_SPECS: { id: PaneToolbarMenuItem["id"]; label: string }[] = [
  { id: "files", label: "文件" },
  { id: "tasks", label: "任务" }
];

function makeButtons(patches: ButtonPatches = {}): PaneToolbarButton[] {
  return BUTTON_SPECS.map((spec) => ({
    id: spec.id,
    label: spec.label,
    activeLabel: spec.activeLabel,
    icon: <span data-testid={`icon-${spec.id}`} />,
    pressed: false,
    onToggle: vi.fn(),
    ...patches[spec.id]
  }));
}

function makeMenuItems(patches: MenuPatches = {}): PaneToolbarMenuItem[] {
  return MENU_SPECS.map((spec) => ({
    id: spec.id,
    label: spec.label,
    icon: <span data-testid={`icon-${spec.id}`} />,
    checked: false,
    onSelect: vi.fn(),
    ...patches[spec.id]
  }));
}

function renderToolbar(props: Partial<PaneToolbarProps> = {}) {
  return render(
    <PaneToolbar
      buttons={props.buttons ?? makeButtons()}
      menuItems={props.menuItems ?? makeMenuItems()}
    />
  );
}

describe("PaneToolbar", () => {
  beforeEach(() => configureI18n("zh-CN"));

  it("offers one toggle per pane plus the overflow trigger", () => {
    renderToolbar();

    expect(screen.getByRole("button", { name: "终端" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "审阅" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "预览" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "更多选项" })).toBeInTheDocument();
  });

  it("reports an open pane through aria-pressed", () => {
    renderToolbar({ buttons: makeButtons({ review: { pressed: true } }) });

    expect(screen.getByRole("button", { name: "终端" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: "审阅" })).toHaveAttribute("aria-pressed", "true");
  });

  /**
   * The toggles carry no text, so the accessible name is the only place activity behind a closed
   * pane is spelled out; the dot alone says nothing to a screen reader, hence aria-hidden.
   */
  it("renames the button and marks it while the pane has activity", () => {
    const { container } = renderToolbar({ buttons: makeButtons({ terminal: { activity: true } }) });

    const terminal = screen.getByRole("button", { name: "终端（命令运行中）" });
    expect(terminal).toHaveAttribute("title", "终端（命令运行中）");
    expect(screen.queryByRole("button", { name: "终端" })).not.toBeInTheDocument();

    const indicator = terminal.querySelector(".pane-toolbar__indicator");
    expect(indicator).toBeInTheDocument();
    expect(indicator).toHaveAttribute("aria-hidden", "true");
    expect(container.querySelectorAll(".pane-toolbar__indicator")).toHaveLength(1);
  });

  it("toggles the pane on click", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    renderToolbar({ buttons: makeButtons({ preview: { onToggle } }) });

    await user.click(screen.getByRole("button", { name: "预览" }));

    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  /** A disabled toggle must still say why, so the reason replaces the tooltip rather than the name. */
  it("keeps the reason on a disabled button", async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();
    renderToolbar({
      buttons: makeButtons({ review: { disabled: true, title: "先发送一条消息再打开", onToggle } })
    });

    const review = screen.getByRole("button", { name: "审阅" });
    expect(review).toBeDisabled();
    expect(review).toHaveAttribute("title", "先发送一条消息再打开");

    await user.click(review);
    expect(onToggle).not.toHaveBeenCalled();
  });

  it("lists the view items with their checked state", async () => {
    const user = userEvent.setup();
    renderToolbar({ menuItems: makeMenuItems({ tasks: { checked: true } }) });

    await user.click(screen.getByRole("button", { name: "更多选项" }));

    const menu = screen.getByRole("menu", { name: "视图" });
    expect(within(menu).getAllByRole("menuitemradio").map((item) => item.textContent)).toEqual(["文件", "任务"]);
    expect(within(menu).getByRole("menuitemradio", { name: "文件" })).toHaveAttribute("aria-checked", "false");
    expect(within(menu).getByRole("menuitemradio", { name: "任务" })).toHaveAttribute("aria-checked", "true");
  });

  it("selects a view item", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    renderToolbar({ menuItems: makeMenuItems({ files: { onSelect } }) });

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(screen.getByRole("menuitemradio", { name: "文件" }));

    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu", { name: "视图" })).not.toBeInTheDocument();
  });

  it("disables the overflow trigger when no view item can be reached", async () => {
    const user = userEvent.setup();
    renderToolbar({
      menuItems: makeMenuItems({ files: { disabled: true }, tasks: { disabled: true } })
    });

    const trigger = screen.getByRole("button", { name: "更多选项" });
    expect(trigger).toBeDisabled();

    await user.click(trigger);
    expect(screen.queryByRole("menu", { name: "视图" })).not.toBeInTheDocument();
  });
});
