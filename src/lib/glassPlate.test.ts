import { afterEach, describe, expect, it } from "vitest";
import { blurPixels, boxRadii, filterColor, filterPixels, parseGlassFilter, watchGlassPlates } from "./glassPlate";

/** A one-row RGBA image of grey levels. */
function row(levels: number[]): Uint8ClampedArray {
  return new Uint8ClampedArray(levels.flatMap((level) => [level, level, level, 255]));
}

function levels(pixels: Uint8ClampedArray): number[] {
  return Array.from({ length: pixels.length / 4 }, (_, index) => pixels[index * 4]);
}

describe("glass plate", () => {
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

  it("splits a Gaussian into three box blurs of the same spread", () => {
    for (const sigma of [1, 3, 7.5, 22]) {
      const variance = boxRadii(sigma).reduce((sum, radius) => sum + ((2 * radius + 1) ** 2 - 1) / 12, 0);
      expect(Math.abs(variance - sigma * sigma)).toBeLessThanOrEqual(Math.max(1, sigma * sigma * 0.05));
    }
    expect(boxRadii(0.2)).toEqual([0, 0, 0]);
  });

  it("blurs without moving or losing light, and leaves a flat field flat", () => {
    const flat = row(Array(12).fill(90));
    blurPixels(flat, 12, 1, 2);
    expect(levels(flat)).toEqual(Array(12).fill(90));

    const spot = row(Array.from({ length: 41 }, (_, index) => (index === 20 ? 255 : 0)));
    blurPixels(spot, 41, 1, 3);
    const blurred = levels(spot);
    expect(blurred[20]).toBeLessThan(255);
    expect(blurred[20]).toBeGreaterThan(blurred[17]);
    for (let offset = 1; offset <= 10; offset += 1) {
      expect(Math.abs(blurred[20 - offset] - blurred[20 + offset])).toBeLessThanOrEqual(1);
    }
    expect(Math.abs(blurred.reduce((sum, level) => sum + level, 0) - 255)).toBeLessThanOrEqual(20);
    // Alpha is left as it was.
    expect(spot[20 * 4 + 3]).toBe(255);
  });

  it("runs the filter's colour change over every pixel", () => {
    const pixels = new Uint8ClampedArray([150, 120, 90, 255, 100, 100, 100, 255]);
    const filter = { blur: 0, saturate: 2, brightness: 1 };
    filterPixels(pixels, filter);
    const warm = filterColor({ r: 150, g: 120, b: 90 }, filter);
    expect(Array.from(pixels.slice(0, 3))).toEqual([warm.r, warm.g, warm.b].map(Math.round));
    expect(Array.from(pixels.slice(4, 7))).toEqual([100, 100, 100]);
  });

  it("lays a solid ground's colour through each filter on the root while glass is on", async () => {
    const root = document.documentElement;
    root.style.setProperty("--glass-blur", "22px");
    root.style.setProperty("--glass-saturate", "130%");
    root.style.setProperty("--glass-draft-filter", "blur(7px) saturate(200%) brightness(1.06)");
    const layer = document.createElement("div");
    layer.className = "app-backdrop";
    layer.style.backgroundColor = "rgb(100, 120, 140)";
    document.body.append(layer);
    const expected = (filter: string) => {
      const color = filterColor({ r: 100, g: 120, b: 140 }, parseGlassFilter(filter));
      const value = `rgb(${Math.round(color.r)} ${Math.round(color.g)} ${Math.round(color.b)})`;
      return `linear-gradient(${value}, ${value})`;
    };

    const stopPlain = watchGlassPlates();
    expect(root.style.getPropertyValue("--glass-plate")).toBe("");
    stopPlain();

    root.dataset.glass = "true";
    const stop = watchGlassPlates();
    expect(root.style.getPropertyValue("--glass-plate")).toBe(expected("saturate(130%)"));
    expect(root.style.getPropertyValue("--glass-plate-draft")).toBe(expected("saturate(200%) brightness(1.06)"));
    expect(root.style.getPropertyValue("--glass-plate-position")).toBe("");

    // Turning the glass off takes the plates away.
    root.removeAttribute("data-glass");
    await Promise.resolve();
    expect(root.style.getPropertyValue("--glass-plate")).toBe("");

    root.dataset.glass = "true";
    await Promise.resolve();
    expect(root.style.getPropertyValue("--glass-plate")).toBe(expected("saturate(130%)"));
    stop();
    expect(root.style.getPropertyValue("--glass-plate")).toBe("");
    expect(root.style.getPropertyValue("--glass-plate-draft")).toBe("");
  });
});
