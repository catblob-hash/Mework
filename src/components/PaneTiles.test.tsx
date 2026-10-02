import { fireEvent, render, screen } from "@testing-library/react";
import { useContext, useRef } from "react";
import { describe, expect, it, vi } from "vitest";
import paneTilesCss from "./PaneTiles.css?raw";
import { PaneTiles } from "./PaneTiles";
import type { PaneTilesProps } from "./PaneTiles";
import { PaneTileGeometryContext } from "./paneTileGeometry";
import type { SidePaneId } from "../lib/sidePanes";

function setup(overrides: Partial<PaneTilesProps> = {}) {
  const props: PaneTilesProps = {
    chat: <div>Chat</div>,
    panes: [{ id: "terminal", node: <div>Terminal</div> }, { id: "tasks", node: <div>Tasks</div> }],
    sideFlex: 1, paneFlex: {},
    onSideFlexChange: vi.fn(), onPaneFlexChange: vi.fn(), onResizeStateChange: vi.fn(),
    ...overrides
  };
  const result = render(<PaneTiles {...props} />);
  const rect = (selector: string, width: number, height: number) => {
    const element = result.container.querySelector(selector)!;
    vi.spyOn(element, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, left: 0, top: 0, right: width, bottom: height, width, height, toJSON: () => ({}) });
    return element;
  };
  rect(".pane-tiles__chat", 1000, 800);
  if (props.panes.length) rect(".pane-tiles__side", 500, 800);
  result.container.querySelectorAll(".pane-tiles__tile").forEach((element) => {
    vi.spyOn(element, "getBoundingClientRect").mockReturnValue({ width: 500, height: 400 } as DOMRect);
  });
  return { ...result, props, rect };
}

function pointer(target: Element | Window, type: string, clientX = 0, clientY = 0, pointerId = 1) {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, clientX, clientY });
  Object.defineProperties(event, { pointerId: { value: pointerId }, isPrimary: { value: true } });
  fireEvent(target, event);
}

const row = () => screen.getByRole("separator", { name: "调整面板宽度" });
const column = () => screen.getByRole("separator", { name: "调整面板高度" });
const between = () => screen.getByRole("separator", { name: "调整列宽度" });

/** The column and row shares a tile or divider is placed by. */
function placement(element: HTMLElement) {
  const read = (name: string) => element.style.getPropertyValue(`--tile-${name}`);
  const entries = ["col", "cols", "x", "w", "row", "rows", "y", "h"].map((name) => [name, read(name)] as const);
  return Object.fromEntries(entries.filter(([, value]) => value !== ""));
}

/** The declaration block of every rule whose selector mentions `needle`; jsdom loads no stylesheet. */
function rulesMentioning(needle: string): string[] {
  const pattern = new RegExp(`${needle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[^{}]*\\{([^}]*)\\}`, "g");
  return [...paneTilesCss.matchAll(pattern)].map((match) => match[1]!);
}

/** Counts its own mounts so a remount is distinguishable from a re-render. */
let mountCount = 0;
function MountCounted() {
  const instance = useRef(0);
  if (instance.current === 0) instance.current = ++mountCount;
  return <div data-testid="counted">{instance.current}</div>;
}

describe("PaneTiles", () => {
  it("renders only the chat when no panes are open", () => {
    const { container } = setup({ panes: [] });
    expect(screen.getByText("Chat")).toBeVisible();
    expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    expect(container.querySelector(".pane-tiles__side")).toBeNull();
  });

  it("stacks tiles with handles between them and applies the flex values", () => {
    const { container } = setup({ sideFlex: 3, paneFlex: { terminal: 2 } });
    expect(screen.getAllByRole("separator")).toHaveLength(2);
    expect(container.querySelector(".pane-tiles__chat")).toHaveStyle({ flexGrow: "2", flexShrink: "1", flexBasis: "0px" });
    expect(container.querySelector(".pane-tiles__side")).toHaveStyle({ flexGrow: "3", flexShrink: "1", flexBasis: "0px" });
    const tiles = [...container.querySelectorAll<HTMLElement>(".pane-tiles__tile")];
    expect(placement(tiles[0])).toEqual({ col: "0", cols: "1", x: "0", w: "1", row: "0", rows: "2", y: "0", h: "0.666667" });
    expect(placement(tiles[1])).toEqual({ col: "0", cols: "1", x: "0", w: "1", row: "1", rows: "2", y: "0.666667", h: "0.333333" });
    // The divider sits in the gap under the first tile: where the second one's share starts.
    expect(placement(column())).toMatchObject({ row: "0", rows: "2", y: "0.666667" });
    expect(tiles[0].nextElementSibling).toBe(column());
    expect(column().nextElementSibling).toBe(tiles[1]);
    expect(tiles[1].nextElementSibling).toBeNull();
  });

  it("exposes focusable, oriented separators and rounded percentages", () => {
    setup({ sideFlex: 3, paneFlex: { terminal: 2, tasks: 1 } });
    expect(row()).toHaveAttribute("aria-orientation", "vertical");
    expect(column()).toHaveAttribute("aria-orientation", "horizontal");
    expect(row()).toHaveAttribute("aria-valuenow", "60");
    expect(column()).toHaveAttribute("aria-valuenow", "67");
    for (const handle of screen.getAllByRole("separator")) {
      expect(handle).toHaveAttribute("tabindex", "0");
      expect(handle).toHaveAttribute("aria-valuemin", "0");
      expect(handle).toHaveAttribute("aria-valuemax", "100");
    }
  });

  it("converts row pixels into the ratio of the two post-drag widths", () => {
    const { props, container, rect } = setup();
    const handle = row();
    const capture = vi.fn();
    const release = vi.fn();
    Object.assign(handle, { setPointerCapture: capture, hasPointerCapture: () => true, releasePointerCapture: release });
    pointer(handle, "pointerdown", 1000);
    expect(capture).toHaveBeenCalledWith(1);
    expect(props.onResizeStateChange).toHaveBeenCalledWith(true);
    expect(handle).toHaveClass("is-active");
    expect(container.firstChild).toHaveClass("pane-tiles--resizing");
    expect(document.body).toHaveClass("pane-tiles-resize-active");
    rect(".pane-tiles__chat", 1100, 800);
    pointer(window, "pointermove", 900);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(600 / 900 * 2);
    pointer(window, "pointermove", 1100);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(400 / 1100 * 2);
    pointer(window, "pointerup", 1100);
    expect(release).toHaveBeenCalledWith(1);
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    expect(handle).not.toHaveClass("is-active");
    expect(container.firstChild).not.toHaveClass("pane-tiles--resizing");
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
  });

  it("enforces both pixel minimums before conversion", () => {
    const { props } = setup();
    pointer(row(), "pointerdown", 1000);
    pointer(window, "pointermove", 10000);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(280 / 1220 * 2);
    pointer(window, "pointermove", -10000);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(1180 / 320 * 2);
  });

  it("clamps the resulting side flex to the module's limits", () => {
    const { props, rect } = setup();
    rect(".pane-tiles__chat", 10000, 800);
    pointer(row(), "pointerdown", 0);
    pointer(window, "pointermove", 10000);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(0.25);
    pointer(window, "pointerup");
    rect(".pane-tiles__chat", 320, 800);
    rect(".pane-tiles__side", 5000, 800);
    pointer(row(), "pointerdown", 0);
    pointer(window, "pointermove", 0);
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(8);
  });

  it("Escape restores the original controlled side flex and ends the drag", () => {
    const { props, rerender } = setup();
    pointer(row(), "pointerdown", 1000);
    pointer(window, "pointermove", 900);
    rerender(<PaneTiles {...props} sideFlex={1.2} />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(props.onSideFlexChange).toHaveBeenLastCalledWith(1);
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
    pointer(window, "pointermove", 800);
    expect(props.onSideFlexChange).toHaveBeenCalledTimes(2);
  });

  it.each(["terminal", "review", "files", "preview:session"] as const)("resets %s to its kind default", (id) => {
    const { props } = setup({ panes: [{ id, node: <div>Pane</div> }], sideFlex: 5 });
    fireEvent.doubleClick(row());
    expect(props.onSideFlexChange).toHaveBeenCalledWith(id === "terminal" ? 1 : 3);
  });

  it.each([
    ["ArrowLeft", false, 524 / 976 * 2], ["ArrowRight", false, 476 / 1024 * 2],
    ["ArrowLeft", true, 548 / 952 * 2], ["ArrowRight", true, 452 / 1048 * 2],
    ["Home", false, 280 / 1220 * 2], ["End", false, 1180 / 320 * 2]
  ])("resizes the row with %s (shift %s)", (key, shiftKey, expected) => {
    const { props } = setup();
    fireEvent.keyDown(row(), { key, shiftKey });
    expect(props.onSideFlexChange).toHaveBeenCalledWith(expected);
    expect(props.onResizeStateChange).not.toHaveBeenCalled();
  });

  it("redistributes adjacent column flex, preserving the sum and unrelated panes", () => {
    const { props } = setup({ paneFlex: { terminal: 2, tasks: 2, plan: 3 } });
    pointer(column(), "pointerdown", 0, 400);
    expect(document.body).toHaveClass("pane-tiles-resize-active--column");
    pointer(window, "pointermove", 0, 500);
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ terminal: 2.5, tasks: 1.5, plan: 3 });
    pointer(window, "pointermove", 0, 10000);
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ terminal: 3.5, tasks: 0.5, plan: 3 });
    pointer(window, "pointermove", 0, -10000);
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ terminal: 0.5, tasks: 3.5, plan: 3 });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith(props.paneFlex);
    expect(document.body).not.toHaveClass("pane-tiles-resize-active--column");
  });

  it.each([
    { heights: [100, 250, 250], weights: [0.25, 1, 1], delta: 24 },
    { heights: [250, 100, 250], weights: [1, 0.25, 1], delta: -24 },
    { heights: [250, 250, 100], weights: [1, 1, 0.25], delta: 24 }
  ])("keeps unrelated pane geometry fixed with minimum-constrained heights $heights", ({ heights, weights, delta }) => {
    const ids = ["terminal", "tasks", "plan"] as const;
    const paneFlex = Object.fromEntries(ids.map((id, index) => [id, weights[index]]));
    const { props, container, rect, rerender } = setup({
      panes: ids.map((id) => ({ id, node: <div>{id}</div> })), paneFlex
    });
    rect(".pane-tiles__side", 500, 624);
    container.querySelectorAll(".pane-tiles__tile").forEach((tile, index) => {
      vi.mocked(tile.getBoundingClientRect).mockReturnValue({ width: 500, height: heights[index] } as DOMRect);
    });
    const handle = screen.getAllByRole("separator", { name: "调整面板高度" })[0];
    pointer(handle, "pointerdown", 0, 100);
    pointer(window, "pointermove", 0, 100);
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith(paneFlex);
    pointer(window, "pointermove", 0, 100 + delta);
    const next = vi.mocked(props.onPaneFlexChange).mock.lastCall![0];
    expect(next.terminal + next.tasks).toBeCloseTo(weights[0] + weights[1]);
    // These weights allocate all 600 available pixels proportionally, with none below 100.
    const total = ids.reduce((sum, id) => sum + next[id], 0);
    const expected = [heights[0] + delta, heights[1] - delta, heights[2]];
    ids.forEach((id, index) => {
      expect(600 * next[id] / total).toBeCloseTo(expected[index]);
      expect(next[id]).toBeGreaterThanOrEqual(0.25);
      expect(next[id]).toBeLessThanOrEqual(8);
    });
    rerender(<PaneTiles {...props} paneFlex={next} />);
    expect(handle).toHaveClass("is-active");
    pointer(window, "pointermove", 0, 100);
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith(paneFlex);
    pointer(window, "pointermove", 0, 100 + delta);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith(paneFlex);
    fireEvent.keyDown(handle, { key: delta > 0 ? "ArrowDown" : "ArrowUp" });
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith(next);
  });

  it("resets only the two column neighbours to one", () => {
    const { props } = setup({ paneFlex: { terminal: 2, tasks: 3, plan: 4 } });
    fireEvent.doubleClick(column());
    expect(props.onPaneFlexChange).toHaveBeenCalledWith({ terminal: 1, tasks: 1, plan: 4 });
  });

  it.each([
    ["ArrowUp", false, 0.94], ["ArrowDown", false, 1.06],
    ["ArrowUp", true, 0.88], ["ArrowDown", true, 1.12],
    ["Home", false, 0.25], ["End", false, 1.75]
  ])("resizes the column with %s (shift %s)", (key, shiftKey, expected) => {
    const { props } = setup();
    fireEvent.keyDown(column(), { key, shiftKey });
    expect(props.onPaneFlexChange).toHaveBeenCalledWith({ terminal: expected, tasks: 2 - Number(expected) });
  });

  it("ignores other pointer ids and handles capture failure and cancellation", () => {
    const { props } = setup();
    Object.assign(row(), { setPointerCapture: () => { throw new Error("capture unavailable"); } });
    pointer(row(), "pointerdown", 1000);
    pointer(window, "pointermove", 900, 0, 2);
    pointer(window, "pointerup", 900, 0, 2);
    expect(props.onSideFlexChange).not.toHaveBeenCalled();
    expect(props.onResizeStateChange).not.toHaveBeenCalledWith(false);
    pointer(window, "pointermove", 900);
    expect(props.onSideFlexChange).toHaveBeenCalledWith(600 / 900 * 2);
    pointer(window, "pointercancel");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
  });

  it.each(["row", "column"])("ends a %s resize when its handle disappears", (axis) => {
    const { props, container, rerender, rect } = setup();
    const handle = axis === "row" ? row() : column();
    pointer(handle, "pointerdown");
    rerender(<PaneTiles {...props} panes={axis === "row" ? [] : props.panes.slice(0, 1)} />);
    expect(handle).not.toHaveClass("is-active");
    expect(container.firstChild).not.toHaveClass("pane-tiles--resizing");
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
    expect(document.body).not.toHaveClass("pane-tiles-resize-active--column");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    pointer(window, "pointermove", 50, 50);
    expect(props.onSideFlexChange).not.toHaveBeenCalled();
    expect(props.onPaneFlexChange).not.toHaveBeenCalled();
    rerender(<PaneTiles {...props} />);
    rect(".pane-tiles__side", 500, 800);
    pointer(row(), "pointerdown");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(true);
  });

  it("ends a column resize when its neighbour changes without removing the handle", () => {
    const { props, rerender } = setup();
    const handle = column();
    pointer(handle, "pointerdown");
    rerender(<PaneTiles {...props} panes={[props.panes[0], { id: "plan", node: <div>Plan</div> }]} />);
    expect(column()).toBe(handle);
    expect(handle).not.toHaveClass("is-active");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    pointer(window, "pointermove", 0, 50);
    expect(props.onPaneFlexChange).not.toHaveBeenCalled();
  });

  it.each(["row", "column"])("ends a %s resize on lost pointer capture", (axis) => {
    const { props, container } = setup();
    const handle = axis === "row" ? row() : column();
    pointer(handle, "pointerdown");
    pointer(handle, "lostpointercapture", 0, 0, 2);
    pointer(axis === "row" ? column() : row(), "lostpointercapture");
    expect(props.onResizeStateChange).not.toHaveBeenCalledWith(false);
    pointer(handle, "lostpointercapture");
    expect(handle).not.toHaveClass("is-active");
    expect(container.firstChild).not.toHaveClass("pane-tiles--resizing");
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
    expect(document.body).not.toHaveClass("pane-tiles-resize-active--column");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    pointer(window, "pointermove", 50, 50);
    pointer(window, "pointerup");
    expect(props.onResizeStateChange).toHaveBeenCalledTimes(2);
    expect(props.onSideFlexChange).not.toHaveBeenCalled();
    expect(props.onPaneFlexChange).not.toHaveBeenCalled();
    pointer(handle, "pointerdown");
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(true);
  });

  it("cleans up a live drag on unmount", () => {
    const { props, unmount } = setup();
    pointer(row(), "pointerdown", 1000);
    unmount();
    expect(props.onResizeStateChange).toHaveBeenLastCalledWith(false);
    expect(document.body).not.toHaveClass("pane-tiles-resize-active");
    pointer(window, "pointermove", 900);
    expect(props.onSideFlexChange).not.toHaveBeenCalled();
  });

  it("does not resize unmeasured elements or consume unrelated keys", () => {
    const { props, rect } = setup();
    fireEvent.keyDown(row(), { key: "ArrowUp" });
    rect(".pane-tiles__chat", 0, 0);
    pointer(row(), "pointerdown", 1000);
    pointer(window, "pointermove", 900);
    fireEvent.keyDown(row(), { key: "ArrowLeft" });
    expect(props.onSideFlexChange).not.toHaveBeenCalled();
    expect(props.onResizeStateChange).not.toHaveBeenCalled();
  });

  /** A tile may be parked out of sight to keep its surface mounted; it must cost no layout. */
  it("takes a hidden pane out of the layout, the handles and the drag arithmetic", () => {
    const { container } = setup({
      panes: [
        { id: "terminal", node: <div>Terminal</div>, hidden: true },
        { id: "tasks", node: <div>Tasks</div> },
        { id: "plan", node: <div>Plan</div> }
      ],
      paneFlex: { terminal: 5, tasks: 3, plan: 1 }
    });
    const tiles = container.querySelectorAll(".pane-tiles__tile");
    expect(tiles).toHaveLength(3);
    expect(tiles[0]).toHaveAttribute("hidden");
    expect(tiles[0]).toHaveAttribute("inert");
    expect(tiles[1]).not.toHaveAttribute("hidden");
    // One separator between the two visible tiles, and none against the hidden one.
    expect(screen.getAllByRole("separator", { name: "调整面板高度" })).toHaveLength(1);
    expect(tiles[1].nextElementSibling).toBe(column());
    expect(column().nextElementSibling).toBe(tiles[2]);
    // The hidden pane's flex is not part of the ratio the visible separator reports.
    expect(column()).toHaveAttribute("aria-valuenow", "75");
    expect(container.querySelector(".pane-tiles__side")).not.toHaveAttribute("hidden");
  });

  it("keeps the side column present but weightless when every pane is hidden", () => {
    const { container } = setup({ panes: [{ id: "terminal", node: <div>Terminal</div>, hidden: true }] });
    const side = container.querySelector(".pane-tiles__side")!;
    expect(side).toBeInTheDocument();
    expect(side).toHaveAttribute("hidden");
    expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    expect(rulesMentioning(".pane-tiles__side[hidden]").join(" ")).toContain("display: none");
    expect(rulesMentioning(".pane-tiles__tile[hidden]").join(" ")).toContain("display: none");
  });

  it("moves a hidden pane's separator arithmetic onto the visible neighbours only", () => {
    const { props } = setup({
      panes: [
        { id: "tasks", node: <div>Tasks</div> },
        { id: "terminal", node: <div>Terminal</div>, hidden: true },
        { id: "plan", node: <div>Plan</div> }
      ],
      paneFlex: { tasks: 1, terminal: 4, plan: 1 }
    });
    fireEvent.doubleClick(column());
    expect(props.onPaneFlexChange).toHaveBeenCalledWith({ tasks: 1, terminal: 4, plan: 1 });
    pointer(column(), "pointerdown", 0, 400);
    pointer(window, "pointermove", 0, 500);
    // Only the two visible neighbours move; the parked pane keeps its remembered weight.
    expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ tasks: 1.25, terminal: 4, plan: 0.75 });
  });

  /** Hiding is why the entry exists: unmounting is what it must never do. */
  it("keeps a pane's React instance across a hidden → visible flip", () => {
    mountCount = 0;
    const props: PaneTilesProps = {
      chat: <div>Chat</div>,
      panes: [{ id: "terminal", node: <MountCounted />, hidden: true }],
      sideFlex: 1, paneFlex: {},
      onSideFlexChange: vi.fn(), onPaneFlexChange: vi.fn(), onResizeStateChange: vi.fn()
    };
    const { rerender } = render(<PaneTiles {...props} />);
    const before = screen.getByTestId("counted");
    expect(before).toHaveTextContent("1");

    rerender(<PaneTiles {...props} panes={[{ id: "terminal", node: <MountCounted /> }]} />);
    expect(screen.getByTestId("counted")).toBe(before);
    expect(before).toHaveTextContent("1");
    expect(mountCount).toBe(1);

    rerender(<PaneTiles {...props} panes={[
      { id: "tasks", node: <div>Tasks</div> },
      { id: "terminal", node: <MountCounted />, hidden: true }
    ]} />);
    expect(screen.getByTestId("counted")).toBe(before);
    expect(mountCount).toBe(1);
  });

  /**
   * A short window must stack every open pane rather than overflow: the authored floor is one
   * pane header, and 100px survives only as the drag/keyboard floor inside the component.
   */
  it("authors a one-header tile floor and places tiles by their shares alone", () => {
    const { container } = setup();
    const tile = rulesMentioning(".pane-tiles .pane-tiles__tile").join(" ");
    expect(tile).toContain("min-height: 32px");
    expect(tile).not.toContain("min-height: 100px");
    expect(tile).toContain("position: absolute");
    for (const variable of ["--tile-x", "--tile-w", "--tile-y", "--tile-h", "--tile-col", "--tile-row"]) {
      expect(tile).toContain(`var(${variable})`);
    }
    const side = rulesMentioning(".pane-tiles > .pane-tiles__side").join(" ");
    expect(side).toContain("min-height: 0");
    expect(side).toContain("overflow: hidden");
    expect(side).toContain("position: relative");
    expect(placement(container.querySelector<HTMLElement>(".pane-tiles__tile")!)).toMatchObject({ h: "0.5" });
  });

  /**
   * Expanding a pane must not tear down the others: a pane can own a live shell or a native page
   * whose teardown is reported as a stop. They keep their box and only leave the flow.
   */
  it("shows one pane alone, keeping the chat and the other tiles mounted but inert", () => {
    const { container } = setup({ expanded: "tasks" });
    expect(screen.getByText("Chat")).toBeInTheDocument();
    expect(screen.getByText("Terminal")).toBeInTheDocument();
    const chat = container.querySelector(".pane-tiles__chat")!;
    expect(chat).toHaveClass("is-solo-hidden");
    expect(chat).toHaveAttribute("inert");
    const tiles = [...container.querySelectorAll(".pane-tiles__tile")];
    const [terminal, tasks] = tiles;
    expect(terminal).toHaveClass("is-solo-hidden");
    expect(terminal).toHaveAttribute("inert");
    expect(tasks).not.toHaveClass("is-solo-hidden");
    expect(tasks).not.toHaveAttribute("inert");
    expect(tasks).toHaveClass("is-solo");
    expect(terminal).not.toHaveClass("is-solo");
    expect(container.querySelector(".pane-tiles")).toHaveClass("pane-tiles--solo");
    // The shown pane covers the whole area; the others keep their own box, hidden.
    const shown = rulesMentioning(".pane-tiles__tile.is-solo").join(" ");
    for (const edge of ["left: 0", "top: 0", "width: 100%", "height: 100%"]) expect(shown).toContain(edge);
    expect(container.querySelector(".pane-tiles__side")).not.toHaveStyle({ minWidth: "280px" });
  });

  it("withdraws every resize handle while a pane is shown alone", () => {
    setup({ expanded: "tasks" });
    expect(screen.queryByRole("separator", { name: "调整面板宽度" })).toBeNull();
    expect(screen.queryByRole("separator", { name: "调整面板高度" })).toBeNull();
  });

  it("ignores an expansion naming a pane it was not given", () => {
    const { container } = setup({ expanded: "review" });
    expect(container.querySelector(".pane-tiles")).not.toHaveClass("pane-tiles--solo");
    expect(container.querySelector(".is-solo-hidden")).toBeNull();
    expect(row()).toBeInTheDocument();
  });

  describe("columns", () => {
    const three = [
      { id: "terminal" as const, node: <div>Terminal</div> },
      { id: "tasks" as const, node: <div>Tasks</div> },
      { id: "plan" as const, node: <div>Plan</div> }
    ];
    const columns = () => [["terminal", "tasks"], ["plan"]] as SidePaneId[][];

    function setupColumns(overrides: Partial<PaneTilesProps> = {}) {
      const result = setup({
        panes: three, columns: columns(), columnFlex: [2, 1], onColumnFlexChange: vi.fn(), ...overrides
      });
      const tile = (id: string) => [...result.container.querySelectorAll<HTMLElement>(".pane-tiles__tile")]
        .find((element) => element.textContent?.toLowerCase().includes(id))!;
      return { ...result, tile };
    }

    it("places each pane by its column's share of the width and its share of the column", () => {
      const { tile } = setupColumns({ paneFlex: { terminal: 3 } });
      expect(placement(tile("terminal"))).toEqual({ col: "0", cols: "2", x: "0", w: "0.666667", row: "0", rows: "2", y: "0", h: "0.75" });
      expect(placement(tile("tasks"))).toEqual({ col: "0", cols: "2", x: "0", w: "0.666667", row: "1", rows: "2", y: "0.75", h: "0.25" });
      expect(placement(tile("plan"))).toEqual({ col: "1", cols: "2", x: "0.666667", w: "0.333333", row: "0", rows: "1", y: "0", h: "1" });
    });

    it("draws a divider between the columns and one between the tiles of each column", () => {
      setupColumns();
      expect(screen.getAllByRole("separator", { name: "调整列宽度" })).toHaveLength(1);
      expect(screen.getAllByRole("separator", { name: "调整面板高度" })).toHaveLength(1);
      expect(between()).toHaveAttribute("aria-orientation", "vertical");
      expect(between()).toHaveAttribute("aria-valuenow", "67");
      // Its left edge is where the second column's share starts, less one gap.
      expect(placement(between())).toMatchObject({ col: "0", cols: "2", x: "0.666667" });
      expect(placement(column())).toMatchObject({ col: "0", w: "0.666667", row: "0", y: "0.5" });
    });

    it("needs more room for more columns, but not while one pane is shown alone", () => {
      const { container, rerender, props } = setupColumns();
      expect(container.querySelector(".pane-tiles__side")).toHaveStyle({ minWidth: "412px" });
      rerender(<PaneTiles {...props} columns={[["terminal"], ["tasks"], ["plan"]]} columnFlex={[1, 1, 1]} />);
      expect(container.querySelector(".pane-tiles__side")).toHaveStyle({ minWidth: "624px" });
      rerender(<PaneTiles {...props} columns={[["terminal", "tasks", "plan"]]} columnFlex={[1]} />);
      expect(container.querySelector(".pane-tiles__side")).toHaveStyle({ minWidth: "280px" });
    });

    it("moves the divider between two columns by the pixels dragged, within each column's floor", () => {
      const { props, tile } = setupColumns();
      const lastWeights = (expected: number[]) => {
        const weights = vi.mocked(props.onColumnFlexChange!).mock.lastCall![0];
        expect(weights).toHaveLength(expected.length);
        expected.forEach((weight, index) => { expect(weights[index]).toBeCloseTo(weight); });
      };
      vi.mocked(tile("terminal").getBoundingClientRect).mockReturnValue({ width: 600, height: 400 } as DOMRect);
      vi.mocked(tile("plan").getBoundingClientRect).mockReturnValue({ width: 300, height: 800 } as DOMRect);
      pointer(between(), "pointerdown", 600);
      expect(document.body).not.toHaveClass("pane-tiles-resize-active--column");
      pointer(window, "pointermove", 650);
      lastWeights([650 / 900 * 3, 250 / 900 * 3]);
      pointer(window, "pointermove", 5000);
      lastWeights([700 / 900 * 3, 200 / 900 * 3]);
      pointer(window, "pointermove", -5000);
      lastWeights([200 / 900 * 3, 700 / 900 * 3]);
      fireEvent.keyDown(window, { key: "Escape" });
      expect(props.onColumnFlexChange).toHaveBeenLastCalledWith([2, 1]);
      fireEvent.keyDown(between(), { key: "ArrowRight" });
      lastWeights([624 / 900 * 3, 276 / 900 * 3]);
      fireEvent.keyDown(between(), { key: "Home" });
      lastWeights([200 / 900 * 3, 700 / 900 * 3]);
      expect(props.onSideFlexChange).not.toHaveBeenCalled();
      expect(props.onPaneFlexChange).not.toHaveBeenCalled();
    });

    it("lets a column under its floor be dragged wider, but not narrower", () => {
      const { props, tile } = setupColumns({ columnFlex: [3, 1] });
      vi.mocked(tile("terminal").getBoundingClientRect).mockReturnValue({ width: 300, height: 400 } as DOMRect);
      vi.mocked(tile("plan").getBoundingClientRect).mockReturnValue({ width: 100, height: 800 } as DOMRect);
      pointer(between(), "pointerdown", 300);
      expect(document.body).toHaveClass("pane-tiles-resize-active");
      pointer(window, "pointermove", 350);
      expect(props.onColumnFlexChange).toHaveBeenLastCalledWith([3, 1]);
      pointer(window, "pointermove", 250);
      const [first, second] = vi.mocked(props.onColumnFlexChange!).mock.lastCall![0];
      expect(first).toBeCloseTo(250 / 400 * 4);
      expect(second).toBeCloseTo(150 / 400 * 4);
      pointer(window, "pointermove", -5000);
      const [floored] = vi.mocked(props.onColumnFlexChange!).mock.lastCall![0];
      expect(floored).toBeCloseTo(200 / 400 * 4);
      pointer(window, "pointerup", -5000);
      expect(document.body).not.toHaveClass("pane-tiles-resize-active");
    });

    it("lets a tile under its floor be dragged taller, but not shorter", () => {
      const { props, tile } = setupColumns({ paneFlex: { terminal: 3, tasks: 1 } });
      vi.mocked(tile("terminal").getBoundingClientRect).mockReturnValue({ width: 500, height: 300 } as DOMRect);
      vi.mocked(tile("tasks").getBoundingClientRect).mockReturnValue({ width: 500, height: 60 } as DOMRect);
      pointer(column(), "pointerdown", 0, 300);
      pointer(window, "pointermove", 0, 350);
      expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ terminal: 3, tasks: 1 });
      pointer(window, "pointermove", 0, 200);
      const flex = vi.mocked(props.onPaneFlexChange).mock.lastCall![0];
      expect(flex.terminal).toBeCloseTo(200 / 360 * 4);
      expect(flex.tasks).toBeCloseTo(160 / 360 * 4);
      pointer(window, "pointerup", 0, 200);
    });

    it("evens out the two neighbours on a double click, leaving the other columns' weights", () => {
      const { props } = setupColumns({
        columns: [["terminal"], ["tasks"], ["plan"]], columnFlex: [1, 3, 2]
      });
      fireEvent.doubleClick(screen.getAllByRole("separator", { name: "调整列宽度" })[1]);
      expect(props.onColumnFlexChange).toHaveBeenCalledWith([1, 2.5, 2.5]);
    });

    it("resizes a column's tiles among themselves, leaving the other column alone", () => {
      const { props } = setupColumns({ paneFlex: { terminal: 1, tasks: 1, plan: 5 } });
      pointer(column(), "pointerdown", 0, 400);
      pointer(window, "pointermove", 0, 500);
      expect(props.onPaneFlexChange).toHaveBeenLastCalledWith({ terminal: 1.25, tasks: 0.75, plan: 5 });
    });

    it("keeps a pane's React instance when it moves to another column", () => {
      mountCount = 0;
      const panes = [
        { id: "terminal" as const, node: <MountCounted /> },
        { id: "tasks" as const, node: <div>Tasks</div> },
        { id: "plan" as const, node: <div>Plan</div> }
      ];
      const { rerender, props } = setupColumns({ panes });
      const before = screen.getByTestId("counted");
      rerender(<PaneTiles {...props} columns={[["tasks"], ["plan", "terminal"]]} />);
      rerender(<PaneTiles {...props} columns={[["plan"], ["tasks"], ["terminal"]]} columnFlex={[1, 1, 1]} />);
      expect(screen.getByTestId("counted")).toBe(before);
      expect(mountCount).toBe(1);
    });

    it("tells a pane where its tile is, and changes that only when the tile moves", () => {
      const seen: string[] = [];
      function Probe() {
        seen.push(useContext(PaneTileGeometryContext));
        return null;
      }
      const panes = [three[0], three[1], { id: "plan" as const, node: <Probe /> }];
      const { rerender, props } = setupColumns({ panes });
      const first = seen.at(-1);
      expect(first).toContain("0.666667");
      // Resizing within the other column leaves this tile where it was.
      rerender(<PaneTiles {...props} paneFlex={{ terminal: 2 }} />);
      expect(seen.at(-1)).toBe(first);
      rerender(<PaneTiles {...props} columnFlex={[1, 1]} />);
      expect(seen.at(-1)).not.toBe(first);
      rerender(<PaneTiles {...props} panes={[{ ...panes[0], hidden: true }, panes[1], panes[2]]} />);
      expect(seen.at(-1)).toBe(first);
    });

    it("skips panes it was not given and gives a pane no column names a column of its own", () => {
      const { tile } = setupColumns({
        columns: [["review", "terminal"], ["files"]],
        columnFlex: [1, 3]
      });
      expect(placement(tile("terminal"))).toMatchObject({ col: "0", cols: "2", rows: "1", w: "0.5" });
      // `tasks` and `plan` sit in no column, so they share one opened after the given ones.
      expect(placement(tile("tasks"))).toMatchObject({ col: "1", rows: "2", row: "0" });
      expect(placement(tile("plan"))).toMatchObject({ col: "1", rows: "2", row: "1" });
    });

    it("drops a column whose panes are all hidden, but keeps its index in the weights", () => {
      const { props, tile } = setupColumns({
        panes: [{ ...three[0], hidden: true }, { ...three[1], hidden: true }, three[2]],
        columns: [["terminal", "tasks"], ["plan"], []] as SidePaneId[][],
        columnFlex: [2, 1]
      });
      expect(placement(tile("plan"))).toMatchObject({ col: "0", cols: "1", w: "1" });
      expect(tile("terminal").getAttribute("style")).toBeNull();
      expect(screen.queryByRole("separator", { name: "调整列宽度" })).toBeNull();
      fireEvent.doubleClick(row());
      expect(props.onSideFlexChange).toHaveBeenCalledWith(1);
    });

    it("resets the side area to its first pane's width plus half the chat for each further column", () => {
      const { props } = setupColumns({
        panes: [{ id: "review", node: <div>Review</div> }, ...three],
        columns: [["review", "terminal"], ["tasks"], ["plan"]], columnFlex: [3, 1, 1]
      });
      fireEvent.doubleClick(row());
      expect(props.onSideFlexChange).toHaveBeenCalledWith(3 + 2);
    });

    it("draws no divider between columns nobody can resize", () => {
      setupColumns({ onColumnFlexChange: undefined });
      expect(screen.queryByRole("separator", { name: "调整列宽度" })).toBeNull();
    });

    it("withdraws the column dividers while a pane is shown alone", () => {
      setupColumns({ expanded: "plan" });
      expect(screen.queryByRole("separator")).toBeNull();
    });
  });

  it("takes a hidden tile out of the flow instead of out of the layout", () => {
    const solo = rulesMentioning("\.pane-tiles--solo").join(" ");
    expect(solo).toContain("visibility: hidden");
    expect(solo).toContain("position: absolute");
    expect(solo).toContain("pointer-events: none");
    expect(solo).not.toContain("display: none");
  });

});
