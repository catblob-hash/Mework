import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SketchCanvas } from "./SketchCanvas";
import type { SketchCanvasHandle } from "./SketchCanvas";
import { SketchOverlay } from "./SketchOverlay";
import {
  EMPTY_SKETCH_HISTORY,
  SKETCH_HISTORY_LIMIT,
  historyState,
  pushHistory,
  redoHistory,
  undoHistory
} from "./history";
import type { SketchHistory } from "./history";
import { SKETCH_COLORS, SKETCH_STROKE_WIDTH, SKETCH_TEXT_SIZE, SKETCH_TOOLS, constrainPoint, drawStroke } from "./strokes";
import type { SketchStroke } from "./strokes";
import { sketchCommandForEvent, isTypingTarget } from "./useSketch";
import { createRef } from "react";

interface RecordedCall {
  name: string;
  args: number[];
}

interface FakeContext {
  canvas: HTMLCanvasElement;
  calls: RecordedCall[];
  lineCap: string;
  lineJoin: string;
  strokeStyle: string;
  fillStyle: string;
  lineWidth: number;
  font: string;
  textBaseline: string;
}

const CANVAS_CSS_WIDTH = 400;
const CANVAS_CSS_HEIGHT = 300;
const BACKDROP = "data:image/png;base64,QkFDS0Ryb3A=";
const originalSetPointerCapture = Element.prototype.setPointerCapture;
const originalReleasePointerCapture = Element.prototype.releasePointerCapture;

function createFakeContext(canvas: HTMLCanvasElement): FakeContext {
  const calls: RecordedCall[] = [];
  const record = (name: string) => (...args: unknown[]) => {
    calls.push({ name, args: args.filter((value): value is number => typeof value === "number") });
  };
  return {
    canvas,
    calls,
    lineCap: "",
    lineJoin: "",
    strokeStyle: "",
    fillStyle: "",
    lineWidth: 0,
    font: "",
    textBaseline: "",
    setTransform: record("setTransform"),
    clearRect: record("clearRect"),
    fillRect: record("fillRect"),
    strokeRect: record("strokeRect"),
    beginPath: record("beginPath"),
    moveTo: record("moveTo"),
    lineTo: record("lineTo"),
    closePath: record("closePath"),
    arc: record("arc"),
    ellipse: record("ellipse"),
    stroke: record("stroke"),
    fill: record("fill"),
    fillText: record("fillText"),
    drawImage: record("drawImage"),
    scale: record("scale")
  } as unknown as FakeContext;
}

function asContext(fake: FakeContext): CanvasRenderingContext2D {
  return fake as unknown as CanvasRenderingContext2D;
}

function stroke(index: number): SketchStroke {
  return { kind: "line", x1: index, y1: 0, x2: index, y2: 10, color: "#E03131", width: SKETCH_STROKE_WIDTH };
}

describe("sketch history", () => {
  it("keeps the past under the 100-entry cap while the present keeps growing", () => {
    let history: SketchHistory = EMPTY_SKETCH_HISTORY;
    for (let index = 0; index < 150; index += 1) {
      history = pushHistory(history, [...history.present, stroke(index)]);
    }

    expect(SKETCH_HISTORY_LIMIT).toBe(100);
    expect(history.past).toHaveLength(100);
    expect(history.present).toHaveLength(150);
    expect(history.future).toEqual([]);
    expect(historyState(history)).toEqual({ count: 150, canUndo: true, canRedo: false });
  });

  it("undoes one push at a time and redoes back to where it was", () => {
    let history: SketchHistory = pushHistory(EMPTY_SKETCH_HISTORY, [stroke(0)]);
    history = pushHistory(history, [stroke(0), stroke(1)]);

    history = undoHistory(history);
    expect(history.present).toHaveLength(1);
    expect(historyState(history)).toEqual({ count: 1, canUndo: true, canRedo: true });

    history = redoHistory(history);
    expect(history.present).toHaveLength(2);
    expect(historyState(history)).toEqual({ count: 2, canUndo: true, canRedo: false });
  });

  /**
   * Past the cap there is no recorded predecessor left, so undo peels the newest stroke off the
   * present instead of restoring a snapshot. Without that the 101st undo would be a no-op.
   */
  it("peels strokes off the present once the capped past runs out", () => {
    let history: SketchHistory = EMPTY_SKETCH_HISTORY;
    for (let index = 0; index < 150; index += 1) {
      history = pushHistory(history, [...history.present, stroke(index)]);
    }
    for (let index = 0; index < 100; index += 1) history = undoHistory(history);

    expect(history.past).toEqual([]);
    expect(history.present).toHaveLength(50);

    history = undoHistory(history);
    expect(history.present).toHaveLength(49);
  });

  it("stops undoing at an empty present and stops redoing at an empty future", () => {
    const empty = undoHistory(EMPTY_SKETCH_HISTORY);
    expect(empty).toBe(EMPTY_SKETCH_HISTORY);
    expect(redoHistory(EMPTY_SKETCH_HISTORY)).toBe(EMPTY_SKETCH_HISTORY);
    expect(historyState(EMPTY_SKETCH_HISTORY)).toEqual({ count: 0, canUndo: false, canRedo: false });
  });

  it("drops the redo branch as soon as a new stroke lands", () => {
    let history = pushHistory(EMPTY_SKETCH_HISTORY, [stroke(0)]);
    history = undoHistory(history);
    expect(history.future).toHaveLength(1);

    history = pushHistory(history, [stroke(1)]);
    expect(history.future).toEqual([]);
  });
});

describe("shift constrain", () => {
  it("leaves the point alone when shift is not held", () => {
    const point = { x: 13, y: 77 };
    expect(constrainPoint("line", { x: 0, y: 0 }, point, false)).toBe(point);
  });

  it("snaps line and arrow to 45° increments keeping the drag length", () => {
    const flat = constrainPoint("line", { x: 0, y: 0 }, { x: 100, y: 10 }, true);
    expect(flat.x).toBeCloseTo(Math.hypot(100, 10), 6);
    expect(flat.y).toBeCloseTo(0, 6);

    const diagonal = constrainPoint("arrow", { x: 0, y: 0 }, { x: 100, y: 90 }, true);
    const length = Math.hypot(100, 90);
    expect(diagonal.x).toBeCloseTo(length * Math.SQRT1_2, 6);
    expect(diagonal.y).toBeCloseTo(length * Math.SQRT1_2, 6);
  });

  it("forces rect and ellipse square on the longer axis", () => {
    expect(constrainPoint("rect", { x: 0, y: 0 }, { x: 100, y: 40 }, true)).toEqual({ x: 100, y: 100 });
    expect(constrainPoint("ellipse", { x: 0, y: 0 }, { x: -100, y: 40 }, true)).toEqual({ x: -100, y: 100 });
  });

  /** A drag with no vertical movement still has to produce a square, not a zero-height box. */
  it("treats a zero delta as positive so the square never collapses", () => {
    expect(constrainPoint("rect", { x: 5, y: 5 }, { x: 105, y: 5 }, true)).toEqual({ x: 105, y: 105 });
  });

  it("never constrains freehand or text", () => {
    const point = { x: 3, y: 9 };
    expect(constrainPoint("freehand", { x: 0, y: 0 }, point, true)).toBe(point);
    expect(constrainPoint("text", { x: 0, y: 0 }, point, true)).toBe(point);
  });
});

describe("stroke rendering", () => {
  it("shortens the arrow shaft and builds the head triangle from the stroke width", () => {
    const context = createFakeContext(document.createElement("canvas"));
    drawStroke(asContext(context), {
      kind: "arrow",
      x1: 0,
      y1: 0,
      x2: 100,
      y2: 0,
      color: "#E03131",
      width: SKETCH_STROKE_WIDTH
    });

    const head = Math.min(SKETCH_STROKE_WIDTH * 4, 100);
    const lineTos = context.calls.filter((call) => call.name === "lineTo").map((call) => call.args);
    expect(head).toBe(16);
    expect(lineTos[0]).toEqual([100 - head * 0.7, 0]);
    expect(lineTos[1]).toEqual([100 - head, -head / 2]);
    expect(lineTos[2]).toEqual([100 - head, head / 2]);
    expect(context.calls.some((call) => call.name === "fill")).toBe(true);
  });

  it("stacks text lines at 1.2× the font size", () => {
    const context = createFakeContext(document.createElement("canvas"));
    drawStroke(asContext(context), {
      kind: "text",
      x: 20,
      y: 40,
      text: "one\ntwo",
      size: SKETCH_TEXT_SIZE,
      color: "#1971C2"
    });

    expect(context.font).toBe(`${SKETCH_TEXT_SIZE}px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif`);
    const rows = context.calls.filter((call) => call.name === "fillText").map((call) => call.args);
    expect(rows).toEqual([[20, 40], [20, 40 + SKETCH_TEXT_SIZE * 1.2]]);
  });

  it("draws a single freehand point as a filled dot", () => {
    const context = createFakeContext(document.createElement("canvas"));
    drawStroke(asContext(context), {
      kind: "freehand",
      points: [{ x: 8, y: 9 }],
      color: "#2F9E44",
      width: SKETCH_STROKE_WIDTH
    });

    expect(context.calls.map((call) => call.name)).toEqual(["beginPath", "arc", "fill"]);
    expect(context.calls[1]!.args.slice(0, 3)).toEqual([8, 9, SKETCH_STROKE_WIDTH / 2]);
  });
});

describe("sketch shortcuts", () => {
  const base = { altKey: false, shiftKey: false, ctrlKey: false, metaKey: false };

  it("maps Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y off Apple platforms", () => {
    expect(sketchCommandForEvent({ ...base, key: "z", ctrlKey: true }, false)).toBe("undo");
    expect(sketchCommandForEvent({ ...base, key: "Z", ctrlKey: true, shiftKey: true }, false)).toBe("redo");
    expect(sketchCommandForEvent({ ...base, key: "y", ctrlKey: true }, false)).toBe("redo");
  });

  it("maps Cmd+Z and Cmd+Shift+Z on Apple platforms and leaves Ctrl+Y alone", () => {
    expect(sketchCommandForEvent({ ...base, key: "z", metaKey: true }, true)).toBe("undo");
    expect(sketchCommandForEvent({ ...base, key: "z", metaKey: true, shiftKey: true }, true)).toBe("redo");
    expect(sketchCommandForEvent({ ...base, key: "y", ctrlKey: true }, true)).toBeNull();
  });

  it("ignores an unmodified key and anything carrying Alt", () => {
    expect(sketchCommandForEvent({ ...base, key: "z" }, false)).toBeNull();
    expect(sketchCommandForEvent({ ...base, key: "z", ctrlKey: true, altKey: true }, false)).toBeNull();
  });

  /** Undo inside a text field belongs to the field, not to the sketch behind it. */
  it("recognises editable targets the shortcuts must yield to", () => {
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const inert = document.createElement("div");
    inert.setAttribute("contenteditable", "false");
    document.body.append(editable, inert);

    expect(isTypingTarget(document.createElement("textarea"))).toBe(true);
    expect(isTypingTarget(document.createElement("input"))).toBe(true);
    expect(isTypingTarget(editable)).toBe(true);
    expect(isTypingTarget(inert)).toBe(false);
    expect(isTypingTarget(document.createElement("button"))).toBe(false);
    expect(isTypingTarget(null)).toBe(false);

    editable.remove();
    inert.remove();
  });
});

describe("sketch registries", () => {
  it("keeps the six tools in palette order", () => {
    expect(SKETCH_TOOLS).toEqual(["pen", "line", "arrow", "rect", "ellipse", "text"]);
  });

  it("offers exactly the four ink colours", () => {
    expect(SKETCH_COLORS).toEqual(["#E03131", "#1971C2", "#2F9E44", "#1F1E1D"]);
  });
});

describe("sketch surface", () => {
  let contexts: FakeContext[] = [];
  const originalGetContext = HTMLCanvasElement.prototype.getContext;
  const originalToDataURL = HTMLCanvasElement.prototype.toDataURL;

  beforeEach(() => {
    contexts = [];
    HTMLCanvasElement.prototype.getContext = function fakeGetContext(this: HTMLCanvasElement) {
      const context = createFakeContext(this);
      contexts.push(context);
      return asContext(context);
    } as unknown as typeof HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.toDataURL = () => "data:image/png;base64,Q09NUE9TSVRF";
    // jsdom lays nothing out, so the surface would measure 0×0 and skip its backing store.
    Object.defineProperty(HTMLCanvasElement.prototype, "offsetWidth", { configurable: true, get: () => CANVAS_CSS_WIDTH });
    Object.defineProperty(HTMLCanvasElement.prototype, "offsetHeight", { configurable: true, get: () => CANVAS_CSS_HEIGHT });
    Element.prototype.setPointerCapture = () => {};
    Element.prototype.releasePointerCapture = () => {};
    vi.stubGlobal("devicePixelRatio", 2);
    vi.stubGlobal("Image", class FakeImage {
      src = "";
      naturalWidth = 1600;
      naturalHeight = 1200;
      decode() {
        return Promise.resolve();
      }
    });
  });

  afterEach(() => {
    HTMLCanvasElement.prototype.getContext = originalGetContext;
    HTMLCanvasElement.prototype.toDataURL = originalToDataURL;
    Element.prototype.setPointerCapture = originalSetPointerCapture;
    Element.prototype.releasePointerCapture = originalReleasePointerCapture;
    Reflect.deleteProperty(HTMLCanvasElement.prototype, "offsetWidth");
    Reflect.deleteProperty(HTMLCanvasElement.prototype, "offsetHeight");
    vi.unstubAllGlobals();
  });

  const drawFreehand = (canvas: HTMLElement) => {
    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 50, clientY: 60 });
    fireEvent.pointerUp(canvas, { pointerId: 1 });
  };

  it("sizes the backing store in device pixels and draws in CSS pixels", () => {
    render(<SketchCanvas color={SKETCH_COLORS[0]!} strokeWidth={SKETCH_STROKE_WIDTH} />);

    const canvas = screen.getByRole("application", { name: "绘图画布" }) as HTMLCanvasElement;
    expect(canvas.width).toBe(CANVAS_CSS_WIDTH * 2);
    expect(canvas.height).toBe(CANVAS_CSS_HEIGHT * 2);
    expect(contexts[0]!.calls[0]).toEqual({ name: "setTransform", args: [2, 0, 0, 2, 0, 0] });
  });

  it("composites onto the backdrop at the backdrop's own resolution", async () => {
    const ref = createRef<SketchCanvasHandle>();
    render(<SketchCanvas ref={ref} color={SKETCH_COLORS[0]!} strokeWidth={SKETCH_STROKE_WIDTH} />);
    drawFreehand(screen.getByRole("application", { name: "绘图画布" }));

    const dataUrl = await ref.current!.toComposite(BACKDROP);

    expect(dataUrl).toBe("data:image/png;base64,Q09NUE9TSVRF");
    const composite = contexts.at(-1)!;
    expect(composite.canvas.width).toBe(1600);
    expect(composite.canvas.height).toBe(1200);
    expect(composite.calls.some((call) => call.name === "drawImage")).toBe(true);
    const scale = composite.calls.find((call) => call.name === "scale");
    expect(scale?.args).toEqual([1600 / CANVAS_CSS_WIDTH, 1200 / CANVAS_CSS_HEIGHT]);
  });

  it("returns the backdrop untouched when nothing has been measured yet", async () => {
    Reflect.deleteProperty(HTMLCanvasElement.prototype, "offsetWidth");
    Object.defineProperty(HTMLCanvasElement.prototype, "offsetWidth", { configurable: true, get: () => 0 });
    const ref = createRef<SketchCanvasHandle>();
    render(<SketchCanvas ref={ref} color={SKETCH_COLORS[0]!} strokeWidth={SKETCH_STROKE_WIDTH} />);

    await expect(ref.current!.toComposite(BACKDROP)).resolves.toBe(BACKDROP);
  });

  it("undoes and redoes strokes through the imperative handle", () => {
    const ref = createRef<SketchCanvasHandle>();
    render(<SketchCanvas ref={ref} color={SKETCH_COLORS[0]!} strokeWidth={SKETCH_STROKE_WIDTH} />);
    const canvas = screen.getByRole("application", { name: "绘图画布" });
    drawFreehand(canvas);
    drawFreehand(canvas);

    expect(ref.current!.getStrokes()).toHaveLength(2);
    expect(ref.current!.undo()).toBe(1);
    expect(ref.current!.redo()).toBe(2);
    ref.current!.clear();
    expect(ref.current!.getStrokes()).toEqual([]);
  });
});

describe("SketchOverlay", () => {
  const originalGetContext = HTMLCanvasElement.prototype.getContext;
  const originalToDataURL = HTMLCanvasElement.prototype.toDataURL;

  beforeEach(() => {
    HTMLCanvasElement.prototype.getContext = function fakeGetContext(this: HTMLCanvasElement) {
      return asContext(createFakeContext(this));
    } as unknown as typeof HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.toDataURL = () => "data:image/png;base64,Q09NUE9TSVRF";
    Object.defineProperty(HTMLCanvasElement.prototype, "offsetWidth", { configurable: true, get: () => CANVAS_CSS_WIDTH });
    Object.defineProperty(HTMLCanvasElement.prototype, "offsetHeight", { configurable: true, get: () => CANVAS_CSS_HEIGHT });
    Element.prototype.setPointerCapture = () => {};
    vi.stubGlobal("Image", class FakeImage {
      src = "";
      naturalWidth = 1600;
      naturalHeight = 1200;
      decode() {
        return Promise.resolve();
      }
    });
  });

  afterEach(() => {
    HTMLCanvasElement.prototype.getContext = originalGetContext;
    HTMLCanvasElement.prototype.toDataURL = originalToDataURL;
    Element.prototype.setPointerCapture = originalSetPointerCapture;
    Reflect.deleteProperty(HTMLCanvasElement.prototype, "offsetWidth");
    Reflect.deleteProperty(HTMLCanvasElement.prototype, "offsetHeight");
    vi.unstubAllGlobals();
  });

  const renderOverlay = (onAttach = vi.fn(), onCancel = vi.fn()) => {
    render(<SketchOverlay backdropDataUrl={BACKDROP} onCancel={onCancel} onAttach={onAttach} />);
    return { onAttach, onCancel };
  };

  it("lays the tool row out in palette order and offers the four ink colours", () => {
    renderOverlay();

    const tools = screen.getByRole("group", { name: "绘图工具" });
    expect(Array.from(tools.children, (child) => child.getAttribute("aria-label")))
      .toEqual(["画笔", "直线", "箭头", "矩形", "椭圆", "文字"]);
    expect(tools.children[0]).toHaveAttribute("aria-pressed", "true");

    const colors = screen.getByRole("group", { name: "墨水颜色" });
    expect(Array.from(colors.children, (child) => child.getAttribute("aria-label")))
      .toEqual(["红色", "蓝色", "绿色", "黑色"]);
    expect(colors.children[0]).toHaveAttribute("aria-pressed", "true");
  });

  it("keeps the primary action disabled until a stroke exists", async () => {
    const { onAttach } = renderOverlay();

    const attach = screen.getByRole("button", { name: "添加到对话" });
    expect(attach).toBeDisabled();
    expect(screen.getByRole("button", { name: "暂无可撤销的内容" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "暂无可清除的内容" })).toBeDisabled();

    const canvas = screen.getByRole("application", { name: "绘图画布" });
    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 40, clientY: 40 });
    fireEvent.pointerUp(canvas, { pointerId: 1 });

    expect(attach).toBeEnabled();
    expect(screen.getByRole("button", { name: "撤销" })).toBeEnabled();
    fireEvent.click(attach);
    await waitFor(() => expect(onAttach).toHaveBeenCalledWith("data:image/png;base64,Q09NUE9TSVRF"));
  });

  it("closes without confirming while the page is unmarked", () => {
    const { onCancel } = renderOverlay();

    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("asks before discarding marks and only cancels once discard is confirmed", () => {
    const { onCancel } = renderOverlay();
    const canvas = screen.getByRole("application", { name: "绘图画布" });
    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 40, clientY: 40 });
    fireEvent.pointerUp(canvas, { pointerId: 1 });

    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(onCancel).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "放弃这些标注？" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "放弃" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("selects a tool and an ink colour from the rows", () => {
    renderOverlay();

    fireEvent.click(screen.getByRole("button", { name: "箭头" }));
    expect(screen.getByRole("button", { name: "箭头" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "画笔" })).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(screen.getByRole("button", { name: "绿色" }));
    expect(screen.getByRole("button", { name: "绿色" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "红色" })).toHaveAttribute("aria-pressed", "false");
  });

  it("undoes on Ctrl+Z and redoes on Ctrl+Shift+Z from inside the overlay", () => {
    const { container } = render(
      <SketchOverlay backdropDataUrl={null} onCancel={() => undefined} onAttach={() => undefined} />
    );
    const overlay = container.querySelector(".sketch-overlay")!;
    const canvas = screen.getByRole("application", { name: "绘图画布" });
    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 40, clientY: 40 });
    fireEvent.pointerUp(canvas, { pointerId: 1 });
    expect(screen.getByRole("button", { name: "添加到对话" })).toBeEnabled();

    fireEvent.keyDown(overlay, { key: "z", ctrlKey: true });
    expect(screen.getByRole("button", { name: "添加到对话" })).toBeDisabled();

    fireEvent.keyDown(overlay, { key: "z", ctrlKey: true, shiftKey: true });
    expect(screen.getByRole("button", { name: "添加到对话" })).toBeEnabled();
  });

  it("leaves the shortcut to the text tool while a label is being typed", () => {
    renderOverlay();
    const canvas = screen.getByRole("application", { name: "绘图画布" });
    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 40, clientY: 40 });
    fireEvent.pointerUp(canvas, { pointerId: 1 });

    fireEvent.click(screen.getByRole("button", { name: "文字" }));
    fireEvent.pointerDown(canvas, { pointerId: 2, button: 0, isPrimary: true, clientX: 80, clientY: 80 });
    const textarea = screen.getByRole("textbox", { name: "文字标注" });
    fireEvent.keyDown(textarea, { key: "z", ctrlKey: true });

    expect(screen.getByRole("button", { name: "添加到对话" })).toBeEnabled();
  });

  it("commits a text label on Ctrl+Enter and drops it on Escape", () => {
    renderOverlay();
    const canvas = screen.getByRole("application", { name: "绘图画布" });
    fireEvent.click(screen.getByRole("button", { name: "文字" }));

    fireEvent.pointerDown(canvas, { pointerId: 1, button: 0, isPrimary: true, clientX: 30, clientY: 30 });
    fireEvent.change(screen.getByRole("textbox", { name: "文字标注" }), { target: { value: "看这里" } });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "文字标注" }), { key: "Enter", ctrlKey: true });
    expect(screen.getByRole("button", { name: "添加到对话" })).toBeEnabled();
    expect(screen.queryByRole("textbox", { name: "文字标注" })).not.toBeInTheDocument();

    fireEvent.pointerDown(canvas, { pointerId: 2, button: 0, isPrimary: true, clientX: 60, clientY: 60 });
    fireEvent.change(screen.getByRole("textbox", { name: "文字标注" }), { target: { value: "算了" } });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "文字标注" }), { key: "Escape" });
    expect(screen.queryByRole("textbox", { name: "文字标注" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "撤销" }));
    expect(screen.getByRole("button", { name: "添加到对话" })).toBeDisabled();
  });

  it("shows a flat panel instead of an image when the capture produced nothing", () => {
    const { container } = render(
      <SketchOverlay backdropDataUrl={null} onCancel={() => undefined} onAttach={() => undefined} />
    );

    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector(".sketch-overlay__backdrop--blank")).toBeInTheDocument();
  });

  /** Once the pill no longer fits its pane, the two rows have to become dropdowns. */
  it("collapses the tool and colour rows into menus when the pill overflows", () => {
    Object.defineProperty(HTMLDivElement.prototype, "offsetWidth", { configurable: true, get: () => 620 });
    try {
      const { container } = render(
        <SketchOverlay backdropDataUrl={null} onCancel={() => undefined} onAttach={() => undefined} />
      );

      expect(container.querySelector('[data-layout="compact"]')).toBeInTheDocument();
      expect(screen.queryByRole("group", { name: "绘图工具" })).not.toBeInTheDocument();
      expect(screen.queryByRole("group", { name: "墨水颜色" })).not.toBeInTheDocument();

      const toolTrigger = screen.getByRole("button", { name: "绘图工具" });
      expect(toolTrigger).toHaveAttribute("aria-haspopup", "menu");
      expect(screen.getByRole("button", { name: "墨水颜色" })).toHaveAttribute("aria-haspopup", "menu");
      expect(screen.getByRole("button", { name: "关闭" })).toHaveClass("sketch-toolbar__action");

      fireEvent.click(toolTrigger);
      const menu = screen.getByRole("menu", { name: "绘图工具" });
      expect(Array.from(menu.querySelectorAll("strong"), (item) => item.textContent))
        .toEqual(["画笔", "直线", "箭头", "矩形", "椭圆", "文字"]);
    } finally {
      Reflect.deleteProperty(HTMLDivElement.prototype, "offsetWidth");
    }
  });
});
