import { afterEach, describe, expect, it } from "vitest";
import { attachTextLayerSelection } from "./pdfTextSelection";

function layerWithLines(): HTMLElement {
  const layer = document.createElement("div");
  layer.className = "textLayer";
  layer.innerHTML = '<span id="one">First line</span><span id="two">Second line</span>';
  document.body.append(layer);
  return layer;
}

describe("PDF text layer selection", () => {
  afterEach(() => {
    document.getSelection()?.removeAllRanges();
    document.body.innerHTML = "";
  });

  it("covers the page while a selection is made there, and lets go when the press ends", () => {
    const layer = layerWithLines();
    const detach = attachTextLayerSelection(layer);
    const end = layer.querySelector(".endOfContent");
    expect(end).not.toBeNull();

    layer.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(layer).toHaveClass("selecting");
    document.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    expect(layer).not.toHaveClass("selecting");
    expect(layer.lastElementChild).toBe(end);

    detach();
    expect(layer.querySelector(".endOfContent")).toBeNull();
  });

  it("moves the box beside the end of the selection that is moving", async () => {
    const layer = layerWithLines();
    const detach = attachTextLayerSelection(layer);
    const end = layer.querySelector(".endOfContent")!;
    const first = document.getElementById("one")!.firstChild!;

    document.getSelection()!.setBaseAndExtent(first, 0, first, 5);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(layer).toHaveClass("selecting");
    expect(document.getElementById("one")!.nextSibling).toBe(end);
    detach();
  });
});
