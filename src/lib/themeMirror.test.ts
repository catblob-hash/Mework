import { describe, expect, it } from "vitest";

import paletteCss from "../palette.css?raw";
import { mirrorLightnessHex } from "./themeMirror";

/**
 * `src/palette.css` is generated from its own day block by
 * `scripts/write-theme-night-palette.mjs`, and `mirrorLightnessHex` is the
 * frontend's copy of that rule for colours xterm needs as literal values. The
 * rule lives in two languages, so it is checked against the generated palette
 * rather than against hand-written expectations: every opaque token in the
 * palette is a case.
 */
function paletteBlocks(): { day: Map<string, string>; night: Map<string, string> } {
  const blocks = paletteCss.match(
    /:root\s*\{([\s\S]*?)\}\s*:root\[data-theme="night"\]\s*\{([\s\S]*?)\}/
  );
  if (!blocks) throw new Error("src/palette.css lost its :root / night blocks");
  const read = (source: string) => {
    const values = new Map<string, string>();
    for (const [, token, value] of source.matchAll(/(--color-[a-z0-9_-]+)\s*:\s*([^;]+);/g)) {
      values.set(token, value.trim());
    }
    return values;
  };
  return { day: read(blocks[1]), night: read(blocks[2]) };
}

describe("mirrorLightnessHex", () => {
  it("reproduces the night value of every opaque palette token", () => {
    const { day, night } = paletteBlocks();
    const hexTokens = [...day].filter(([, value]) => value.startsWith("#"));
    expect(hexTokens.length).toBeGreaterThan(100);
    for (const [token, dayValue] of hexTokens) {
      expect(mirrorLightnessHex(dayValue), token).toBe(night.get(token));
    }
  });

  it("carries an eight-digit alpha through untouched", () => {
    expect(mirrorLightnessHex("#af957388")).toBe(`${mirrorLightnessHex("#af9573")}88`);
  });

  it("leaves a value it cannot parse alone", () => {
    expect(mirrorLightnessHex("currentColor")).toBe("currentColor");
    expect(mirrorLightnessHex("rgb(1 2 3)")).toBe("rgb(1 2 3)");
  });

  it("keeps a mid-lightness hue where it is and only moves the light/dark ends", () => {
    // `--danger` sits at the middle of the lightness axis, so night barely moves it.
    expect(mirrorLightnessHex("#c43d3d")).toBe("#c23b3b");
    expect(mirrorLightnessHex("#ffffff")).toBe("#000000");
    expect(mirrorLightnessHex("#000000")).toBe("#ffffff");
  });
});
