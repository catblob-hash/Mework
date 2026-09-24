import { describe, expect, it } from "vitest";
import {
  CAT_BODY,
  CAT_BODY_WITHOUT_PAWS,
  CAT_EYES,
  CAT_EYE_WHITES,
  CAT_FACE,
  CAT_FAR_PAW,
  CAT_LID_LIFT,
  CAT_LIDS,
  CAT_MUZZLE,
  CAT_NEAR_PAW,
  CAT_NEAR_PAW_OVERLAP,
  CAT_PUPILS,
  CAT_WHISKERS,
  CAT_WRIST
} from "./catArt";

type Point = [number, number];

/** A path string, split back into the subpaths it was concatenated from. */
function subpaths(path: string): string[] {
  return path
    .split("M")
    .filter((fragment) => fragment.length > 0)
    .map((fragment) => `M${fragment}`);
}

/**
 * The centre of an SVG arc with equal radii and no rotation, from its endpoints and flags —
 * the endpoint-to-centre conversion of SVG 1.1 §F.6.5, reduced to that case.
 */
function arcCentre(from: Point, radius: number, large: number, sweep: number, to: Point): Point {
  const hx = (from[0] - to[0]) / 2;
  const hy = (from[1] - to[1]) / 2;
  const half = hx * hx + hy * hy;
  const reach = Math.sqrt(Math.max(0, (radius * radius - half) / half)) * (large === sweep ? -1 : 1);
  return [reach * hy + (from[0] + to[0]) / 2, -reach * hx + (from[1] + to[1]) / 2];
}

interface Arc {
  centre: Point;
  radius: number;
}

/**
 * Flatten absolute `M`/`C`/`L`/`A`/`Z` path data into one closed polygon per subpath, and
 * report every arc it met.
 *
 * jsdom lays out no SVG and implements no `isPointInFill`, so the invariants below have to be
 * arithmetic rather than something the browser is asked. That is fine: they are arithmetic —
 * nothing here depends on rendering.
 */
function flatten(path: string, steps = 24): { polygons: Point[][]; arcs: Arc[] } {
  const tokens = path.match(/[MCLAZ]|-?\d*\.?\d+/g) ?? [];
  const polygons: Point[][] = [];
  const arcs: Arc[] = [];
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
    } else if (token === "L") {
      cursor = [num(), num()];
      ring.push(cursor);
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
    } else if (token === "A") {
      const radius = num();
      num();
      num();
      const large = num();
      const sweep = num();
      const to: Point = [num(), num()];
      const centre = arcCentre(cursor, radius, large, sweep, to);
      arcs.push({ centre, radius });
      const start = Math.atan2(cursor[1] - centre[1], cursor[0] - centre[0]);
      let turn = Math.atan2(to[1] - centre[1], to[0] - centre[0]) - start;
      if (sweep === 1 && turn < 0) turn += 2 * Math.PI;
      if (sweep === 0 && turn > 0) turn -= 2 * Math.PI;
      const reach = Math.hypot(cursor[0] - centre[0], cursor[1] - centre[1]);
      for (let step = 1; step <= steps; step += 1) {
        const angle = start + (turn * step) / steps;
        ring.push([centre[0] + reach * Math.cos(angle), centre[1] + reach * Math.sin(angle)]);
      }
      cursor = to;
    }
  }
  if (ring.length) polygons.push(ring);
  return { polygons, arcs };
}

function rings(path: string): Point[][] {
  return flatten(path).polygons;
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

/** Rewrite every coordinate pair in a path — used to move an overlay, or break the artwork on purpose. */
function movePoints(path: string, move: (point: Point) => Point): string {
  let pendingX: number | null = null;
  // Arc flags and radii are not coordinates, so an arc's own numbers are passed through
  // untouched and only its endpoint is moved.
  return path.replace(/A([\d.]+) ([\d.]+) (\d) (\d) (\d) (-?[\d.]+) (-?[\d.]+)|-?\d*\.?\d+/g, (raw, r1, r2, rot, large, sweep, ax, ay) => {
    if (r1 !== undefined) {
      const [x, y] = move([Number(ax), Number(ay)]);
      return `A${r1} ${r2} ${rot} ${large} ${sweep} ${x} ${y}`;
    }
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

const shift = (path: string, dx: number, dy: number) => movePoints(path, ([x, y]) => [x + dx, y + dy]);

/** Turns a path about a point, the way `rotate(<degrees>deg)` with that origin does. */
function turn(path: string, [cx, cy]: Point, degrees: number): string {
  const angle = (degrees * Math.PI) / 180;
  return movePoints(path, ([x, y]) => [
    cx + (x - cx) * Math.cos(angle) - (y - cy) * Math.sin(angle),
    cy + (x - cx) * Math.sin(angle) + (y - cy) * Math.cos(angle)
  ]);
}

/** Every outline point of `path` that lands in one of the face's holes. */
function pointsInHoles(path: string, holes: string): Point[] {
  const cut = rings(holes);
  return rings(path).flat().filter((point) => contains(cut, point));
}

/** A grid over a box, dropping points too close to any of `edges` for the outline sampling to settle. */
function interiorGrid(box: [number, number, number, number], step: number, edges: string[], margin: number): Point[] {
  // Outlines are sampled densely enough that a point near one is near a sample of it; the
  // samples are bucketed by `margin`-sized cells so each grid point checks only its neighbours.
  const cells = new Map<string, Point[]>();
  const cell = (x: number, y: number) => `${Math.floor(x / margin)},${Math.floor(y / margin)}`;
  for (const point of edges.flatMap((path) => flatten(path, 96).polygons.flat())) {
    const key = cell(point[0], point[1]);
    cells.set(key, [...(cells.get(key) ?? []), point]);
  }
  const nearEdge = (x: number, y: number) => {
    for (let i = -1; i <= 1; i += 1) {
      for (let j = -1; j <= 1; j += 1) {
        const bucket = cells.get(cell(x + i * margin, y + j * margin)) ?? [];
        if (bucket.some(([ex, ey]) => Math.hypot(ex - x, ey - y) <= margin)) return true;
      }
    }
    return false;
  };
  const points: Point[] = [];
  for (let x = box[0]; x <= box[2]; x += step) {
    for (let y = box[1]; y <= box[3]; y += step) {
      if (!nearEdge(x, y)) points.push([x, y]);
    }
  }
  return points;
}

describe("catArt", () => {
  it("composes the face from parts the small surfaces can drop", () => {
    // One string that is the concatenation of the others, not several independently edited
    // copies.
    expect(CAT_FACE).toBe(`${CAT_EYES}${CAT_WHISKERS}${CAT_MUZZLE}`);
    expect(subpaths(CAT_EYES)).toHaveLength(2);
    expect(subpaths(CAT_WHISKERS)).toHaveLength(2);
    expect(subpaths(CAT_MUZZLE)).toHaveLength(1);
  });

  it("keeps every face mark inside the body the composer cat punches it from", () => {
    // Even-odd punches a hole only where a subpath lies *inside* the outer contour. One that
    // reached past the cheek would be filled instead of cleared and would render as a spike
    // growing out of the silhouette.
    const { escaped, clearance } = containment(CAT_BODY, CAT_FACE);
    expect(escaped).toBe(0);
    expect(clearance).toBeGreaterThan(10);

    // The scan has to be able to fail: push the near whiskers out through the body's
    // underside (y≈658 below them) and it must report them. Without this, a scanner that
    // silently matched nothing would pass.
    const breached = containment(CAT_BODY, movePoints(CAT_WHISKERS, ([x, y]) => [x, y + 120]));
    expect(breached.escaped).toBeGreaterThan(100);
  });

  it("puts the body back together exactly from the pieces the streaming cat cuts it into", () => {
    // Anything missing would be a hole in the cat at rest; anything extra, a lump. Points
    // right on an outline are left out, because two samplings of one curve disagree there.
    const body = rings(CAT_BODY);
    const rest = rings(CAT_BODY_WITHOUT_PAWS);
    const far = rings(CAT_FAR_PAW);
    const near = rings(CAT_NEAR_PAW);
    const grid = interiorGrid([300, 295, 800, 680], 3, [CAT_BODY, CAT_BODY_WITHOUT_PAWS, CAT_FAR_PAW, CAT_NEAR_PAW], 1.5);
    expect(grid.length).toBeGreaterThan(10_000);
    const wrong = grid.filter((point) => (
      contains(body, point) !== (contains(rest, point) || contains(far, point) || contains(near, point))
    ));
    expect(wrong).toEqual([]);
    // The paws really were taken out, rather than left in the body with copies on top: all
    // they share with it is the thin overlap that hides each joint.
    for (const paw of [far, near]) {
      const area = grid.filter((point) => contains(paw, point)).length;
      expect(area).toBeGreaterThan(150);
      expect(grid.filter((point) => contains(paw, point) && contains(rest, point)).length).toBeLessThan(area / 4);
    }
  });

  it("cuts the far paw along arcs centred on the wrist it turns about", () => {
    // A seam is only invisible at every angle if it is a circle about the pivot; an arc about
    // anywhere else opens as soon as the paw lifts.
    const arcs = [...flatten(CAT_BODY_WITHOUT_PAWS).arcs, ...flatten(CAT_FAR_PAW).arcs];
    expect(arcs).toHaveLength(2);
    for (const { centre } of arcs) {
      expect(centre[0]).toBeCloseTo(CAT_WRIST.x, 0);
      expect(centre[1]).toBeCloseTo(CAT_WRIST.y, 0);
    }
    // The paw's arc sits inside the body's, which is the overlap that hides the joint.
    const [body, paw] = arcs as [Arc, Arc];
    expect(paw.radius).toBeLessThan(body.radius);
  });

  it("keeps both paws clear of the face however far they move", () => {
    // A paw is the head's colour; one pushed over a hole would plug it. Past the range the
    // stylesheet uses, to leave room for tuning.
    for (let degrees = -16; degrees <= 0; degrees += 2) {
      expect(pointsInHoles(turn(CAT_FAR_PAW, [CAT_WRIST.x, CAT_WRIST.y], degrees), CAT_FACE)).toEqual([]);
    }
    for (let dx = -CAT_NEAR_PAW_OVERLAP; dx <= CAT_NEAR_PAW_OVERLAP; dx += 4) {
      for (const dy of [-4, 0, 4]) expect(pointsInHoles(shift(CAT_NEAR_PAW, dx, dy), CAT_FACE)).toEqual([]);
    }
  });

  it("recomposes the drawing's eyes from the whites and the pupils", () => {
    const eyes = rings(CAT_EYES);
    const whites = rings(CAT_EYE_WHITES);
    const pupils = rings(CAT_PUPILS);
    const grid = interiorGrid([500, 515, 765, 590], 1, [CAT_EYES, CAT_EYE_WHITES, CAT_PUPILS], 0.8);
    const wrong = grid.filter((point) => contains(eyes, point) !== (contains(whites, point) && !contains(pupils, point)));
    expect(wrong).toEqual([]);
  });

  it("keeps the pupils on solid head wherever their eyes take them", () => {
    // Drawn over the face, they are invisible on the head and visible over an eye — and
    // visible as a blot anywhere else: past the silhouette, or over the muzzle or whiskers.
    for (let dx = -30; dx <= 30; dx += 3) {
      const moved = shift(CAT_PUPILS, dx, 0);
      expect(containment(CAT_BODY, moved).escaped).toBe(0);
      expect(pointsInHoles(moved, `${CAT_WHISKERS}${CAT_MUZZLE}`)).toEqual([]);
    }
  });

  it("parks the lids out of sight and closes the eyes completely with them", () => {
    // Parked, a lid must touch no hole at all, with room to spare so its edge is not
    // antialiased against the eye's.
    const parked = containment(CAT_BODY, CAT_LIDS);
    expect(parked.escaped).toBe(0);
    expect(pointsInHoles(CAT_LIDS, CAT_FACE)).toEqual([]);
    const gap = containment(CAT_LIDS, CAT_EYE_WHITES);
    expect(gap.escaped).toBe(rings(CAT_EYE_WHITES).flat().length);
    expect(gap.clearance).toBeGreaterThan(3);

    // Lowered past the eye by the 8 the stylesheet adds, every point of each eye is inside
    // its lid with clearance, which is what leaves no ghost of the outline.
    const closed = shift(CAT_LIDS, 0, CAT_LID_LIFT + 8);
    const cover = containment(closed, CAT_EYE_WHITES);
    expect(cover.escaped).toBe(0);
    expect(cover.clearance).toBeGreaterThan(3);

    // All the way down and back, a lid never reaches anything but the eyes.
    for (let dy = 0; dy <= CAT_LID_LIFT + 8; dy += 4) {
      const lowered = shift(CAT_LIDS, 0, dy);
      expect(containment(CAT_BODY, lowered).escaped).toBe(0);
      expect(pointsInHoles(lowered, `${CAT_WHISKERS}${CAT_MUZZLE}`)).toEqual([]);
    }
  });
});
