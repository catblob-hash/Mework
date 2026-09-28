import { describe, expect, it } from "vitest";
import {
  backgroundAfterThemeChange,
  BUILTIN_PICTURES,
  parseBackground,
  replacesThemeGround,
  solidBackground
} from "./background";

describe("window background", () => {
  const imported = "0123456789abcdef".repeat(4);

  it("reads every kind of saved value, and anything unknown as the theme's own ground", () => {
    expect(parseBackground("solid")).toEqual({ kind: "solid", scheme: null });
    expect(parseBackground("solid:night")).toEqual({ kind: "solid", scheme: "night" });
    expect(parseBackground("builtin:chair")).toMatchObject({ kind: "builtin", picture: { name: "chair" } });
    expect(parseBackground(imported)).toEqual({ kind: "imported", id: imported });
    expect(parseBackground("builtin:gone")).toEqual({ kind: "solid", scheme: null });
    expect(parseBackground(imported.toUpperCase())).toEqual({ kind: "solid", scheme: null });
    expect(new Set(BUILTIN_PICTURES.map((picture) => picture.id)).size).toBe(BUILTIN_PICTURES.length);
  });

  it("saves the theme's own solid as following it, and the other theme's as that one", () => {
    expect(solidBackground("day", "day")).toBe("solid");
    expect(solidBackground("night", "day")).toBe("solid:night");
    expect(solidBackground("day", "night")).toBe("solid:day");
  });

  it("sends a solid ground to the new theme's when the theme changes, and leaves pictures", () => {
    expect(backgroundAfterThemeChange("solid:night")).toBe("solid");
    expect(backgroundAfterThemeChange("solid")).toBe("solid");
    expect(backgroundAfterThemeChange("builtin:curtain")).toBe("builtin:curtain");
    expect(backgroundAfterThemeChange(imported)).toBe(imported);
  });

  it("lets the ground through only for something other than the theme's own", () => {
    expect(replacesThemeGround("solid")).toBe(false);
    expect(replacesThemeGround("solid:day")).toBe(true);
    expect(replacesThemeGround("builtin:desk")).toBe(true);
    expect(replacesThemeGround(imported)).toBe(true);
  });
});
