import { afterEach, describe, expect, it } from "vitest";
import { coverRect, filterColor, parseGlassFilter, regionMean, watchGlassGround } from "./glassGround";

describe("glass ground", () => {
  afterEach(() => {
    document.body.innerHTML = "";
    document.documentElement.removeAttribute("data-glass");
    document.documentElement.removeAttribute("style");
  });

  it("reads the glass's filters, in either spelling of an amount", () => {
    expect(parseGlassFilter("blur(22px) saturate(130%)")).toEqual({ blur: 22, saturate: 1.3, brightness: 1 });
    expect(parseGlassFilter("blur(7px) saturate(1.8) brightness(.92)")).toEqual({ blur: 7, saturate: 1.8, brightness: 0.92 });
    expect(parseGlassFilter("")).toEqual({ blur: 0, saturate: 1, brightness: 1 });
  });

  it("saturates a colour as the filter does, leaves a grey grey, and clamps", () => {
    const warm = { r: 150, g: 120, b: 90 };
    expect(filterColor(warm, { blur: 0, saturate: 1, brightness: 1 })).toEqual(warm);
    const saturated = filterColor(warm, { blur: 0, saturate: 2, brightness: 1 });
    expect(saturated.r).toBeGreaterThan(warm.r);
    expect(saturated.b).toBeLessThan(warm.b);
    const grey = filterColor({ r: 100, g: 100, b: 100 }, { blur: 0, saturate: 2, brightness: 1 });
    for (const channel of [grey.r, grey.g, grey.b]) expect(channel).toBeCloseTo(100, 6);
    expect(filterColor({ r: 250, g: 250, b: 250 }, { blur: 0, saturate: 1, brightness: 1.06 })).toEqual({ r: 255, g: 255, b: 255 });
  });

  it("places a picture as object-fit: cover does, cropping at its position", () => {
    const box = { left: 10, top: 20, width: 100, height: 100 };
    // 200 × 100 scaled to the box's height: 200 wide, 100 of it cropped away.
    expect(coverRect(box, 200, 100, "25% 81%")).toEqual({ left: 10 - 25, top: 20, width: 200, height: 100 });
    expect(coverRect(box, 200, 100, "50% 50%").left).toBe(10 - 50);
    expect(coverRect(box, 100, 200, "0% 100%")).toEqual({ left: 10, top: 20 - 100, width: 100, height: 200 });
  });

  it("averages the pixels in a region, clipped to the picture", () => {
    // 2 × 2: black, white / red, blue.
    const pixels = new Uint8ClampedArray([
      0, 0, 0, 255, 255, 255, 255, 255,
      255, 0, 0, 255, 0, 0, 255, 255
    ]);
    expect(regionMean(pixels, 2, 2, { left: 0, top: 0, width: 2, height: 2 })).toEqual({ r: 127.5, g: 63.75, b: 127.5 });
    expect(regionMean(pixels, 2, 2, { left: -5, top: 0.5, width: 20, height: 0.5 })).toEqual({ r: 127.5, g: 127.5, b: 127.5 });
    expect(regionMean(pixels, 2, 2, { left: 3, top: 0, width: 1, height: 1 })).toBeNull();
  });

  it("sets each glass's ground on the matching boxes from a solid background, and nothing without glass", () => {
    const root = document.documentElement;
    root.style.setProperty("--glass-blur", "22px");
    root.style.setProperty("--glass-saturate", "130%");
    root.style.setProperty("--glass-draft-filter", "blur(7px) saturate(200%) brightness(1.06)");
    const layer = document.createElement("div");
    layer.className = "app-backdrop";
    layer.style.backgroundColor = "rgb(100, 120, 140)";
    const row = document.createElement("div");
    row.innerHTML = '<button class="chip"></button><span class="label"></span>';
    document.body.append(layer, row);
    const chip = row.querySelector<HTMLElement>(".chip")!;
    const label = row.querySelector<HTMLElement>(".label")!;

    const stopPlain = watchGlassGround(row, ".chip");
    expect(chip.style.getPropertyValue("--glass-ground")).toBe("");
    stopPlain();

    root.dataset.glass = "true";
    const stop = watchGlassGround(row, ".chip");
    const expected = (filter: string) => {
      const color = filterColor({ r: 100, g: 120, b: 140 }, parseGlassFilter(filter));
      return `rgb(${Math.round(color.r)} ${Math.round(color.g)} ${Math.round(color.b)})`;
    };
    expect(chip.style.getPropertyValue("--glass-ground")).toBe(expected("saturate(130%)"));
    expect(chip.style.getPropertyValue("--glass-ground-draft")).toBe(expected("saturate(200%) brightness(1.06)"));
    expect(label.style.getPropertyValue("--glass-ground")).toBe("");
    expect(row.style.getPropertyValue("--glass-ground")).toBe("");
    stop();
  });
});
