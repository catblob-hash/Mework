import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  BRAND_AMBER,
  ICON_MARK_TRANSFORM,
  ICON_PROMPT_PATH,
  ICON_SMALL_MARK_TRANSFORM,
  LOCKUP_MARK_TRANSFORM,
  LOCKUP_VIEWBOX,
  MARK_PATH,
  WORDMARK_PATH
} from "./brandMark";
import { MeworkIcon, MeworkLockup, MeworkMark } from "./MeworkIcon";
import logoSvg from "../logo.svg?raw";
import iconSvg from "../mework-icon.svg?raw";
import smallIconSvg from "../mework-icon-small.svg?raw";
import markSvg from "../mework-mark.svg?raw";
import terminalCss from "../styles/terminal.css?raw";

/** The box a `translate(x y) scale(s)` puts the 120 × 200 mark in. */
function markBox(transform: string) {
  const match = /^translate\(([\d.]+) ([\d.]+)\) scale\(([\d.]+)\)$/.exec(transform);
  if (!match) throw new Error(`unexpected transform ${transform}`);
  const [x, y, scale] = match.slice(1).map(Number) as [number, number, number];
  return { left: x, top: y, right: x + 120 * scale, bottom: y + 200 * scale };
}

describe("brand assets", () => {
  it("cuts every shipped asset from the one mark", () => {
    // Nothing in the build ties the files to `brandMark.ts`, so an edit to one side alone
    // would leave two different marks in the product: the app's and the installer's.
    for (const svg of [markSvg, logoSvg, iconSvg, smallIconSvg]) {
      expect(svg).toContain(`d="${MARK_PATH}"`);
    }
  });

  it("draws the terminal cursor from the one mark", () => {
    // The stylesheet's masks are data URIs: the filled mark and its hollow twin, which draws
    // the path twice (clip and stroke).
    expect(terminalCss.split(`d='${MARK_PATH}'`)).toHaveLength(4);
  });

  it("lays out the lockup and the icons as the in-app copies do", () => {
    expect(logoSvg).toContain(`d="${WORDMARK_PATH}"`);
    expect(logoSvg).toContain(`viewBox="${LOCKUP_VIEWBOX}"`);
    expect(logoSvg).toContain(`transform="${LOCKUP_MARK_TRANSFORM}"`);
    expect(iconSvg).toContain(`d="${ICON_PROMPT_PATH}"`);
    expect(iconSvg).toContain(`transform="${ICON_MARK_TRANSFORM}"`);
    // The small frames drop the prompt: below 32px its stroke is thinner than a pixel.
    expect(smallIconSvg).toContain(`transform="${ICON_SMALL_MARK_TRANSFORM}"`);
    expect(smallIconSvg).not.toContain(ICON_PROMPT_PATH);
  });

  it("centres the mark on the icon plate", () => {
    const full = markBox(ICON_MARK_TRANSFORM);
    expect((full.top + full.bottom) / 2).toBeCloseTo(50, 1);
    const small = markBox(ICON_SMALL_MARK_TRANSFORM);
    expect((small.top + small.bottom) / 2).toBeCloseTo(50, 1);
    expect((small.left + small.right) / 2).toBeCloseTo(50, 1);
  });

  it("ships the artwork without generator metadata", () => {
    // The boxed icons are `include_bytes!`d into the Windows binary and served as favicons,
    // so an editor's embedded manifest would be pure weight.
    for (const svg of [logoSvg, iconSvg, smallIconSvg, markSvg]) {
      expect(svg).not.toContain("c2pa");
      expect(svg).not.toContain("<metadata");
    }
  });
});

describe("MeworkIcon", () => {
  it("draws the shipped icon's prompt and mark, decoratively", () => {
    const { container } = render(<MeworkIcon size={44} />);
    const icon = container.querySelector("svg");
    expect(icon?.getAttribute("aria-hidden")).toBe("true");
    expect(icon?.getAttribute("width")).toBe("44");
    expect(container.querySelector(`path[d="${ICON_PROMPT_PATH}"]`)).not.toBeNull();
    const mark = container.querySelector(`path[d="${MARK_PATH}"]`);
    expect(mark?.getAttribute("transform")).toBe(ICON_MARK_TRANSFORM);
    expect(mark?.getAttribute("fill")).toBe(BRAND_AMBER);
  });
});

describe("MeworkMark", () => {
  it("is the bare mark in the surface's own color", () => {
    const { container } = render(<MeworkMark className="app-loading__mark" />);
    const mark = container.querySelector("svg");
    expect(mark?.getAttribute("aria-hidden")).toBe("true");
    expect(mark?.getAttribute("class")).toBe("app-loading__mark");
    expect(mark?.getAttribute("fill")).toBe("currentColor");
    expect(container.querySelector("path")?.getAttribute("d")).toBe(MARK_PATH);
  });
});

describe("MeworkLockup", () => {
  it("names itself and leaves the mark's color to the stylesheet", () => {
    const { container } = render(<MeworkLockup className="brand-lockup" />);
    expect(screen.getByRole("img", { name: "Mework" })).toBe(container.querySelector("svg"));
    const mark = container.querySelector(".mework-lockup__mark");
    expect(mark?.getAttribute("d")).toBe(MARK_PATH);
    expect(mark?.getAttribute("transform")).toBe(LOCKUP_MARK_TRANSFORM);
    // A fill attribute would pin one palette; the class takes the amber token, day or night.
    expect(mark?.hasAttribute("fill")).toBe(false);
  });
});
