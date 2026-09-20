import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  CAT_BODY,
  CAT_DESK,
  CAT_EYES,
  CAT_FACE,
  CAT_GLYPH,
  CAT_GLYPH_PLAIN,
  CAT_HEAD,
  CAT_LAPTOP,
  CAT_MUZZLE,
  CAT_SCENE,
  CAT_TAIL,
  CAT_WHISKERS
} from "./catArt";
import { MEWORK_MARK_SHADOW_OFFSET, MEWORK_MARK_TRANSFORM, MeworkIcon } from "./MeworkIcon";
import logoSvg from "../logo.svg?raw";
import sceneIconSvg from "../mework-icon.svg?raw";
import smallIconSvg from "../mework-icon-small.svg?raw";
import markSvg from "../mework-mark.svg?raw";

type Point = [number, number];

/** A path string, split back into the subpaths it was concatenated from. */
function subpaths(path: string): string[] {
  return path
    .split("M")
    .filter((fragment) => fragment.length > 0)
    .map((fragment) => `M${fragment}`);
}

/**
 * Flatten absolute `M`/`C`/`Z` path data into one closed polygon per subpath.
 *
 * jsdom lays out no SVG and implements no `isPointInFill`, so the even-odd invariants below
 * have to be arithmetic rather than something the browser is asked. That is fine: they are
 * arithmetic — nothing here depends on rendering.
 */
function rings(path: string, steps = 24): Point[][] {
  const tokens = path.match(/[MCZ]|-?\d*\.?\d+/g) ?? [];
  const polygons: Point[][] = [];
  let ring: Point[] = [];
  let cursor: Point = [0, 0];
  let index = 0;
  const num = () => Number(tokens[index++]);
  while (index < tokens.length) {
    const token = tokens[index++];
    if (token === "M") {
      if (ring.length) polygons.push(ring);
      cursor = [num(), num()];
      ring = [cursor];
    } else if (token === "C") {
      const c1: Point = [num(), num()];
      const c2: Point = [num(), num()];
      const to: Point = [num(), num()];
      for (let step = 1; step <= steps; step += 1) {
        const t = step / steps;
        const u = 1 - t;
        ring.push([
          u ** 3 * cursor[0] + 3 * u * u * t * c1[0] + 3 * u * t * t * c2[0] + t ** 3 * to[0],
          u ** 3 * cursor[1] + 3 * u * u * t * c1[1] + 3 * u * t * t * c2[1] + t ** 3 * to[1]
        ]);
      }
      cursor = to;
    }
  }
  if (ring.length) polygons.push(ring);
  return polygons;
}

/** Even-odd containment, the same rule `fill-rule="evenodd"` applies. */
function contains(polygons: Point[][], [x, y]: Point): boolean {
  let crossings = 0;
  for (const ring of polygons) {
    for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
      const [xi, yi] = ring[i] as Point;
      const [xj, yj] = ring[j] as Point;
      if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) crossings += 1;
    }
  }
  return crossings % 2 === 1;
}

/**
 * How the counter-contours sit inside the outer one: how many of their outline points fall
 * outside it, and how close the nearest one comes to it.
 *
 * A subpath that escapes does so through its own outline, so sampling outlines finds every
 * escape without paying for a grid over the whole face.
 */
function containment(outer: string, inner: string): { escaped: number; clearance: number } {
  const contour = rings(outer);
  const edge = contour.flat();
  let escaped = 0;
  let clearance = Number.POSITIVE_INFINITY;
  for (const ring of rings(inner)) {
    for (const point of ring) {
      if (!contains(contour, point)) escaped += 1;
      for (const [ex, ey] of edge) {
        const distance = Math.hypot(ex - point[0], ey - point[1]);
        if (distance < clearance) clearance = distance;
      }
    }
  }
  return { escaped, clearance };
}

/** Rewrite every coordinate pair in a path — used to break the artwork on purpose. */
function movePoints(path: string, move: (point: Point) => Point): string {
  let pendingX: number | null = null;
  return path.replace(/-?\d*\.?\d+/g, (raw) => {
    const value = Number(raw);
    if (pendingX === null) {
      pendingX = value;
      // Hold the x back and emit the moved pair in the y's slot, so the separator the
      // original had between them becomes leading whitespace rather than a stray number.
      return "";
    }
    const [x, y] = move([pendingX, value]);
    pendingX = null;
    return `${x} ${y}`;
  });
}

describe("MeworkIcon", () => {
  it("draws the same geometry as the standalone mark asset", () => {
    // `src/mework-mark.svg` is the shippable copy of the glyph and the component is the
    // in-app copy. Nothing in the build ties them together, so editing one and forgetting
    // the other would silently leave two different brand marks in the product.
    expect(markSvg).toContain(CAT_HEAD);
    expect(markSvg).toContain(CAT_GLYPH);
    // The fitting transform is part of the geometry: the path data is in the artwork's own
    // drawing space, so a drifting transform moves the mark without touching a `d`. It is
    // also what `catHeadTransform` has to reproduce byte for byte, which is why the centre
    // it rounds to and the one the asset was fitted with have to be the same number.
    expect(markSvg).toContain(MEWORK_MARK_TRANSFORM);
    expect(markSvg).toContain(MEWORK_MARK_SHADOW_OFFSET);
  });

  it("shares its whole glyph with the small application icon", () => {
    // `src/mework-icon-small.svg` is this head on the dark plate. `src-tauri/build.rs`
    // rasterizes it into the .ico frames below 64px, where the desk scene is a smudge.
    expect(smallIconSvg).toContain(CAT_GLYPH);
  });

  it("carries the whole scene, undivided, in the boxed icon and the logo", () => {
    // The scene is one even-odd path and has to stay one: the gap that puts the near paw in
    // front of the laptop, and the seam that seats the chin on the desk, are subpaths
    // cancelling against each other. Split them into separately filled shapes and those
    // gaps fill in — so this pins the whole concatenation rather than its pieces.
    expect(sceneIconSvg).toContain(CAT_SCENE);
    expect(logoSvg).toContain(CAT_SCENE);
    expect(CAT_SCENE).toBe(`${CAT_BODY}${CAT_LAPTOP}${CAT_FACE}${CAT_DESK}${CAT_TAIL}`);
    // The scene draws the body, not the cut-out head: the head is a separate outline made
    // for the surfaces that show it alone, and drawing it here would double the cheek line.
    expect(sceneIconSvg).not.toContain(CAT_HEAD);
  });

  it("composes the face from parts the small surfaces can drop", () => {
    // The streaming cat's head is 20px across, where a whisker is a quarter of a pixel. It
    // draws CAT_GLYPH_PLAIN instead, so the split has to stay a split: one string that is
    // the concatenation of the others, not several independently edited copies.
    expect(CAT_FACE).toBe(`${CAT_EYES}${CAT_WHISKERS}${CAT_MUZZLE}`);
    expect(CAT_GLYPH).toBe(`${CAT_HEAD}${CAT_FACE}`);
    expect(CAT_GLYPH_PLAIN).toBe(`${CAT_HEAD}${CAT_EYES}${CAT_MUZZLE}`);
    expect(subpaths(CAT_EYES)).toHaveLength(2);
    expect(subpaths(CAT_WHISKERS)).toHaveLength(2);
    expect(subpaths(CAT_MUZZLE)).toHaveLength(1);
    expect(subpaths(CAT_HEAD)).toHaveLength(1);
  });

  it("keeps every face mark inside the head's own contour", () => {
    // Even-odd punches a hole only where a subpath lies *inside* the outer contour. One that
    // reached past the cheek would be filled instead of cleared and would render as a spike
    // growing out of the silhouette. The head is a cut, not a drawn shape — its jaw was
    // chosen to hold the near whiskers — so this is the assertion that cut is still right.
    const { escaped, clearance } = containment(CAT_HEAD, CAT_FACE);
    expect(escaped).toBe(0);
    // Real clearance is 27.4 units on a 411-unit head, and pushing the whiskers 20 units out
    // still leaves 14.3. Ten leaves room to redraw and still fails long before a mark
    // actually breaches the cheek.
    expect(clearance).toBeGreaterThan(10);

    // The scan has to be able to fail: push the near whiskers out through the cheek and it
    // must report them. Without this, a scanner that silently matched nothing would pass.
    // 80 units puts 321 of the whiskers' 482 outline samples outside, so a bound well above
    // one also rules out a scanner that only ever finds a stray point.
    const breached = containment(CAT_HEAD, movePoints(CAT_WHISKERS, ([x, y]) => [x - 80, y]));
    expect(breached.escaped).toBeGreaterThan(100);
  });

  it("ships the artwork without the generator's content-credentials blob", () => {
    // The authored files carried ~8.6 KiB of base64 C2PA manifest each. The boxed icons are
    // `include_bytes!`d into the Windows binary and served as favicons, so the blob is pure
    // weight; this pins it out of every shipped asset.
    for (const svg of [logoSvg, sceneIconSvg, smallIconSvg, markSvg]) {
      expect(svg).not.toContain("c2pa");
      expect(svg).not.toContain("<metadata");
    }
  });

  it("is decorative by default and labelled on request", () => {
    const { container, rerender } = render(<MeworkIcon size={25} className="brand-mark" />);
    const glyph = container.querySelector("svg");
    expect(glyph).not.toBeNull();
    expect(glyph?.getAttribute("aria-hidden")).toBe("true");
    expect(glyph?.getAttribute("width")).toBe("25");
    expect(glyph?.getAttribute("height")).toBe("25");
    expect(glyph?.getAttribute("class")).toBe("brand-mark");
    // The mark tracks the surface it sits on rather than hard-coding a palette color.
    expect(glyph?.getAttribute("fill")).toBe("currentColor");

    rerender(<MeworkIcon label="Mework" />);
    expect(screen.getByRole("img", { name: "Mework" })).toBeTruthy();
  });

  it("punches the face with an even-odd hole instead of a mask", () => {
    // A <mask> needs an id, and the four brand slots render on the same page.
    const { container } = render(<MeworkIcon />);
    expect(container.querySelector("mask")).toBeNull();
    expect(container.querySelector('path[fill="currentColor"]')?.getAttribute("fill-rule")).toBe(
      "evenodd"
    );
  });
});
