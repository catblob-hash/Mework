import { afterEach, describe, expect, it, vi } from "vitest";
import {
  claimFloatingSurfaceId,
  floatingSurfaceRects,
  floatingSurfacesOver,
  publishFloatingSurface,
  resetFloatingSurfaces,
  subscribeFloatingSurfaces,
  type FloatingSurfaceRect
} from "./floatingSurfaces";

function rect(left: number, top: number, width: number, height: number): FloatingSurfaceRect {
  return { left, top, right: left + width, bottom: top + height, width, height };
}

afterEach(() => {
  resetFloatingSurfaces();
});

describe("floating surface registry", () => {
  it("keeps one box per surface and withdraws it on null", () => {
    const menu = claimFloatingSurfaceId();
    const dialog = claimFloatingSurfaceId();
    expect(menu).not.toBe(dialog);

    publishFloatingSurface(menu, rect(1175, 160, 270, 352));
    publishFloatingSurface(dialog, rect(0, 0, 1440, 900));
    expect(floatingSurfaceRects()).toEqual([rect(1175, 160, 270, 352), rect(0, 0, 1440, 900)]);

    publishFloatingSurface(menu, rect(1175, 200, 270, 352));
    expect(floatingSurfaceRects()).toEqual([rect(1175, 200, 270, 352), rect(0, 0, 1440, 900)]);

    publishFloatingSurface(dialog, null);
    expect(floatingSurfaceRects()).toEqual([rect(1175, 200, 270, 352)]);
  });

  it("notifies only on a real change", () => {
    const listener = vi.fn();
    const unsubscribe = subscribeFloatingSurfaces(listener);
    const menu = claimFloatingSurfaceId();

    publishFloatingSurface(menu, rect(1175, 160, 270, 352));
    expect(listener).toHaveBeenCalledTimes(1);

    // Publishers measure after every render. Republishing the same box must not make the host
    // reinstall a native region it already holds.
    publishFloatingSurface(menu, rect(1175, 160, 270, 352));
    expect(listener).toHaveBeenCalledTimes(1);

    // Nor may withdrawing a surface that was never registered wake anybody.
    publishFloatingSurface(claimFloatingSurfaceId(), null);
    expect(listener).toHaveBeenCalledTimes(1);

    publishFloatingSurface(menu, null);
    expect(listener).toHaveBeenCalledTimes(2);

    unsubscribe();
    publishFloatingSurface(menu, rect(0, 0, 10, 10));
    expect(listener).toHaveBeenCalledTimes(2);
  });
});

describe("floatingSurfacesOver", () => {
  const page = rect(900, 164, 560, 656);

  it("keeps the surfaces that overlap the page", () => {
    const overlapping = rect(880, 600, 120, 200);
    const inside = rect(1000, 300, 200, 120);
    expect(floatingSurfacesOver([overlapping, inside], page)).toEqual([overlapping, inside]);
  });

  it("drops surfaces that miss the page", () => {
    // The consumer publishes the union of whatever it is handed, so a menu opened over the sidebar
    // would otherwise stretch that union across the window and uncover the page for nothing.
    const sidebarMenu = rect(24, 300, 240, 320);
    const aboveThePane = rect(1000, 20, 200, 100);
    expect(floatingSurfacesOver([sidebarMenu, aboveThePane], page)).toEqual([]);
  });

  it("drops unlaid-out surfaces and reports nothing for an unlaid-out page", () => {
    expect(floatingSurfacesOver([rect(1000, 300, 0, 0)], page)).toEqual([]);
    expect(floatingSurfacesOver([rect(1000, 300, 200, 120)], rect(0, 0, 0, 0))).toEqual([]);
  });

  it("treats a shared edge as no overlap", () => {
    expect(floatingSurfacesOver([rect(700, 300, 200, 120)], page)).toEqual([]);
    expect(floatingSurfacesOver([rect(701, 300, 200, 120)], page)).toEqual([rect(701, 300, 200, 120)]);
  });
});
