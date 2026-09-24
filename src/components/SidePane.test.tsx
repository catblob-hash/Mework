import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sidePaneDomId } from "../lib/sidePanes";
import { SidePane } from "./SidePane";

let observed: Element[] = [];

beforeEach(() => {
  observed = [];
  vi.stubGlobal("ResizeObserver", class ResizeObserverMock {
    observe(target: Element) {
      observed.push(target);
    }
    unobserve() {}
    disconnect() {}
  });
});

afterEach(() => vi.unstubAllGlobals());

describe("SidePane", () => {
  it("names the region after its title and closes on the ×", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(
      <SidePane id="terminal" title="终端" onClose={onClose}>
        <p>面板内容</p>
      </SidePane>
    );

    const region = screen.getByRole("region", { name: "终端" });
    expect(region).toHaveTextContent("面板内容");
    await user.click(screen.getByRole("button", { name: "关闭面板" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  /** The address the host measures for the native browser child window. */
  it("carries the pane id in its DOM id, data attribute and kind class", () => {
    const { container } = render(
      <SidePane id="preview:conv-1#tab-2" title="预览" onClose={() => undefined}>
        <p>页面</p>
      </SidePane>
    );

    const pane = container.querySelector(".side-pane");
    expect(pane).toHaveAttribute("id", sidePaneDomId("preview:conv-1#tab-2"));
    expect(pane).toHaveAttribute("data-pane-id", "preview:conv-1#tab-2");
    expect(pane).toHaveClass("side-pane--preview");
  });

  /** A pane whose own chrome is a title bar — the browser — fills the row instead of a title. */
  it("gives the header slot the title's place while still naming the region", () => {
    const { container } = render(
      <SidePane
        id="preview:session"
        title="预览"
        header={<button type="button">地址栏</button>}
        onClose={() => undefined}
      >
        <p>页面</p>
      </SidePane>
    );

    expect(screen.getByRole("region", { name: "预览" })).toBeInTheDocument();
    expect(container.querySelector(".side-pane__title")).toBeNull();
    const slot = container.querySelector(".side-pane__header-slot")!;
    expect(slot).toContainElement(screen.getByRole("button", { name: "地址栏" }));
    // The close × keeps its own cluster, after the slot.
    expect(slot.nextElementSibling).toHaveClass("side-pane__controls");
  });

  it("puts trailing controls before the close button", () => {
    render(
      <SidePane
        id="plan"
        title="实施计划"
        trailing={<button type="button">停止</button>}
        onClose={() => undefined}
      >
        <p>计划</p>
      </SidePane>
    );

    const controls = screen.getByRole("button", { name: "停止" }).parentElement;
    expect(controls).toHaveClass("side-pane__controls");
    const labels = Array.from(controls?.children ?? [], (child) => child.textContent);
    expect(labels[0]).toBe("停止");
    expect(controls?.children).toHaveLength(2);
  });

  /**
   * Focus follows the pointer as well as the keyboard: clicking into a terminal or a diff never
   * moves DOM focus to the pane itself, so a focus-only rule would leave the parent pointing at
   * whichever pane was opened last.
   */
  it("reports focus on a pointer press anywhere inside", () => {
    const onFocus = vi.fn();
    render(
      <SidePane id="tasks" title="任务" onFocus={onFocus} onClose={() => undefined}>
        <p>列表</p>
      </SidePane>
    );

    fireEvent.pointerDown(screen.getByText("列表"), { pointerId: 1, button: 0, isPrimary: true });
    expect(onFocus).toHaveBeenCalledTimes(1);
  });

  it("reports focus when something inside takes keyboard focus", () => {
    const onFocus = vi.fn();
    render(
      <SidePane id="tasks" title="任务" onFocus={onFocus} onClose={() => undefined}>
        <button type="button">重试</button>
      </SidePane>
    );

    screen.getByRole("button", { name: "重试" }).focus();
    expect(onFocus).toHaveBeenCalled();
  });

  /**
   * The native browser page is a child window above the renderer, positioned from this rectangle.
   * It has to be the body alone: the browser's own chrome is the pane's title bar, so a rectangle
   * that included the header would slide the page under it.
   */
  it("publishes the body rectangle — not the whole pane", () => {
    const onContentBoundsChange = vi.fn();
    const { container } = render(
      <SidePane
        id="preview:session"
        title="预览"
        onClose={() => undefined}
        onContentBoundsChange={onContentBoundsChange}
      >
        <p>页面</p>
      </SidePane>
    );

    expect(onContentBoundsChange).toHaveBeenCalledTimes(1);
    expect(onContentBoundsChange.mock.calls[0]![0]).toMatchObject({
      x: expect.any(Number),
      y: expect.any(Number),
      width: expect.any(Number),
      height: expect.any(Number)
    });
    expect(observed).toEqual([container.querySelector(".side-pane__body")]);
  });

  /** A transition moves the card without resizing it, so the observer alone would miss it. */
  it("republishes after the pane settles from a transition", () => {
    const onContentBoundsChange = vi.fn();
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
    vi.stubGlobal("cancelAnimationFrame", () => {});
    const { container } = render(
      <SidePane
        id="preview:session"
        title="预览"
        onClose={() => undefined}
        onContentBoundsChange={onContentBoundsChange}
      >
        <p>页面</p>
      </SidePane>
    );
    onContentBoundsChange.mockClear();

    fireEvent.transitionEnd(container.querySelector(".side-pane")!);
    expect(frames).toHaveLength(1);
    frames[0]!(0);
    expect(onContentBoundsChange).toHaveBeenCalledTimes(1);
  });

  /** Panes that hold nothing the host has to position must not pay for a ResizeObserver. */
  it("measures nothing when no one asked for the rectangle", () => {
    render(
      <SidePane id="tasks" title="任务" onClose={() => undefined}>
        <p>列表</p>
      </SidePane>
    );

    expect(observed).toEqual([]);
  });

  it("offers no expand control until one is wired, then flips its label and pressed state", async () => {
    const user = userEvent.setup();
    const onToggleExpand = vi.fn();
    const { rerender } = render(
      <SidePane id="tasks" title="任务" onClose={vi.fn()}><p>内容</p></SidePane>
    );
    expect(screen.queryByRole("button", { name: "展开" })).toBeNull();

    rerender(
      <SidePane id="tasks" title="任务" onClose={vi.fn()} onToggleExpand={onToggleExpand}><p>内容</p></SidePane>
    );
    const expand = screen.getByRole("button", { name: "展开" });
    expect(expand).toHaveAttribute("aria-pressed", "false");
    await user.click(expand);
    expect(onToggleExpand).toHaveBeenCalledTimes(1);

    rerender(
      <SidePane id="tasks" title="任务" onClose={vi.fn()} expanded onToggleExpand={onToggleExpand}><p>内容</p></SidePane>
    );
    expect(screen.getByRole("button", { name: "折叠" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.queryByRole("button", { name: "展开" })).toBeNull();
  });

  it("orders the title bar controls: pane menu, expand, then close", () => {
    const { container } = render(
      <SidePane
        id="tasks" title="任务" onClose={vi.fn()} onToggleExpand={vi.fn()}
        trailing={<button type="button">自定义</button>}
        menuSections={[{ id: "s", items: [{ id: "a", label: "保存屏幕截图" }] }]}
      ><p>内容</p></SidePane>
    );
    const labels = [...container.querySelectorAll(".side-pane__controls button")]
      .map((button) => button.textContent || button.getAttribute("aria-label"));
    expect(labels).toEqual(["自定义", "任务 设置", "展开", "关闭面板"]);
  });

  it("draws no pane menu for a pane that contributes no rows", () => {
    const { container } = render(
      <SidePane id="tasks" title="任务" onClose={vi.fn()} menuSections={[]}><p>内容</p></SidePane>
    );
    expect(container.querySelector(".side-pane__menu")).toBeNull();
  });

});
