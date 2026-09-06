import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MEWORK_MARK_GLYPH, MEWORK_MARK_HEAD, MeworkIcon } from "./MeworkIcon";
import boxedIconSvg from "../mework-icon.svg?raw";
import markSvg from "../mework-mark.svg?raw";

describe("MeworkIcon", () => {
  it("draws the same geometry as the standalone mark asset", () => {
    // `src/mework-mark.svg` is the shippable copy of the glyph and the component is the
    // in-app copy. Nothing in the build ties them together, so editing one and forgetting
    // the other would silently leave two different brand marks in the product.
    expect(markSvg).toContain(MEWORK_MARK_HEAD);
    expect(markSvg).toContain(MEWORK_MARK_GLYPH);
  });

  it("shares its head contour with the boxed application icon", () => {
    // `src/mework-icon.svg` is what `src-tauri/build.rs` rasterizes into the Windows .ico and
    // what the favicon points at. It is the same creature on a dark plate.
    expect(boxedIconSvg).toContain(MEWORK_MARK_GLYPH);
  });

  it("ships the artwork without the generator's content-credentials blob", () => {
    // The authored files carried ~8.6 KiB of base64 C2PA manifest each. `mework-icon.svg` is
    // `include_bytes!`d into the Windows binary and served as a favicon, so the blob is pure
    // weight; this pins it out of both assets.
    for (const svg of [boxedIconSvg, markSvg]) {
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

  it("punches the eyes with an even-odd hole instead of a mask", () => {
    // A <mask> needs an id, and the four brand slots render on the same page.
    const { container } = render(<MeworkIcon />);
    expect(container.querySelector("mask")).toBeNull();
    expect(container.querySelector('path[fill="currentColor"]')?.getAttribute("fill-rule")).toBe(
      "evenodd"
    );
  });
});
