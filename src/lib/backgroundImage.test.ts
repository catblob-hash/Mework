import { describe, expect, it } from "vitest";
import { areaResize, backgroundLadder, tierCovers, type Pixels } from "./backgroundImage";

function pixels(width: number, height: number, rgba: number[]): Pixels {
  return { width, height, data: new Uint8ClampedArray(rgba) };
}

describe("backgroundLadder", () => {
  it("keeps a picture under the top at its own size and adds every smaller rung", () => {
    expect(backgroundLadder(6000, 4000)).toEqual([
      { width: 6000, height: 4000 },
      { width: 5120, height: 3413 },
      { width: 3840, height: 2560 },
      { width: 2560, height: 1707 },
      { width: 1920, height: 1280 },
      { width: 1280, height: 853 },
      { width: 640, height: 427 }
    ]);
  });

  it("brings an oversized picture down to the top and skips a rung that would repeat it", () => {
    const ladder = backgroundLadder(8000, 4500);
    expect(ladder[0]).toEqual({ width: 7680, height: 4320 });
    expect(ladder[1]).toEqual({ width: 5120, height: 2880 });
    expect(backgroundLadder(2700, 1500).map((size) => size.width)).toEqual([2700, 1920, 1280, 640]);
  });

  it("measures portrait pictures by their long edge", () => {
    expect(backgroundLadder(1500, 3000).slice(0, 2)).toEqual([
      { width: 1500, height: 3000 },
      { width: 1280, height: 2560 }
    ]);
  });

  it("gives a picture below the smallest rung only itself", () => {
    expect(backgroundLadder(600, 400)).toEqual([{ width: 600, height: 400 }]);
  });
});

describe("areaResize", () => {
  it("averages in linear light, so black and white make the grey they look like", async () => {
    const checker = pixels(2, 2, [
      0, 0, 0, 255, 255, 255, 255, 255,
      255, 255, 255, 255, 0, 0, 0, 255
    ]);
    const result = await areaResize(checker, 1, 1);
    // Half the light is sRGB 188, not the 128 a gamma-naive average darkens it to.
    expect(Array.from(result.data)).toEqual([188, 188, 188, 255]);
  });

  it("does not let a transparent pixel's hidden colour bleed", async () => {
    const result = await areaResize(pixels(2, 1, [255, 0, 0, 255, 0, 255, 0, 0]), 1, 1);
    expect(Array.from(result.data)).toEqual([255, 0, 0, 128]);
  });

  it("splits a source pixel between the outputs it straddles", async () => {
    // 3 → 2: the middle (white) pixel is half in each output, so the left one holds a
    // third of the light — sRGB 156 — and the right one is all white.
    const row = pixels(3, 1, [0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255]);
    const result = await areaResize(row, 2, 1);
    const [left, , , , right] = Array.from(result.data);
    expect(left).toBe(156);
    expect(right).toBe(255);
  });

  it("keeps a flat colour exactly", async () => {
    const flat = pixels(4, 3, Array.from({ length: 12 }, () => [31, 97, 200, 255]).flat());
    const result = await areaResize(flat, 3, 2);
    for (let index = 0; index < result.data.length; index += 4) {
      expect(Array.from(result.data.slice(index, index + 4))).toEqual([31, 97, 200, 255]);
    }
  });

  it("refuses to enlarge", async () => {
    await expect(areaResize(pixels(1, 1, [0, 0, 0, 255]), 2, 2)).rejects.toThrow();
  });
});

describe("tierCovers", () => {
  it("needs both sides of the window", () => {
    expect(tierCovers({ width: 1920, height: 1080 }, 1920, 1080)).toBe(true);
    expect(tierCovers({ width: 1920, height: 1080 }, 1440, 1200)).toBe(false);
  });
});
