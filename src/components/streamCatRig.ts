import { CAT_EAR_FAR, CAT_EAR_NEAR, CAT_GLYPH_FLAT, catHeadTransform } from "./catArt";

/**
 * The standing cat the streaming indicator draws, as a scene graph.
 *
 * The brand artwork has no standing cat to borrow one from: it has a cat lying on a desk with
 * its limbs folded under it, one connected mass of black. So the head comes from `catArt.ts`
 * — the face is the whole of the cat's identity and is never redrawn — and everything below
 * the neck is drawn here, rigged into the joints an animation needs.
 *
 * ## Why this is data rather than JSX
 *
 * The drawing is verified in a browser, against the real stylesheet, at a size where a
 * quarter of a unit is visible. A harness that restated the markup would be verifying its own
 * copy, so the parts below are the single source both the component and the harness render —
 * `StreamWaitingIndicator.test.tsx` pins that the component renders exactly this tree.
 *
 * ## The coordinate system
 *
 * Everything is in {@link STREAM_CAT_VIEWBOX}, a 70x49 box with the ground at y=42, and every
 * `transform-origin` in `conversation.css` is a point from {@link STREAM_CAT_JOINTS} written
 * in those units — `transform-box: view-box` is what lets the two be the same numbers. The
 * joints are checked against the stylesheet by test, because a joint that drifts from its
 * origin does not fail loudly: the limb just pivots around the wrong point.
 *
 * Two placements carry the drawing and cannot be nudged on their own:
 *
 * - The head is one even-odd path whose eyes are holes, so anything drawn under one shows
 *   through it. The body stops at x=45.8 and the eyes start at x=47.4; both ears keep clear
 *   of the eye band however far they swing.
 * - The far pair of legs is the near pair, shifted. They sit inside one static
 *   `translate(-3.7 0)` rather than being drawn twice, which is why the far leg's pivot is
 *   written as the near leg's: inside that group, near coordinates are what the far legs are
 *   authored in.
 *
 * ## Why the limbs and the tail are tubes rather than outlines
 *
 * Every bone below is a run of {@link TubeNode}s — a point on the bone and the limb's
 * half-width there — turned into a filled outline by {@link taperedTube}. Two things follow
 * from that, and both are the reason it is written this way rather than as hand-drawn
 * contours:
 *
 * - **A joint is a disc, not a seam.** Consecutive bones share the node they turn on, so the
 *   outline round that joint is one circle centred exactly on the pivot — and a circle centred
 *   on a pivot is the one shape rotation cannot move. The parts stay fused at any angle, with
 *   no seam to catch and no gap to open.
 * - **The width is continuous.** The tail used to be three round-capped strokes of falling
 *   width, and a stroke cannot taper: each segment was a capsule of its own, and the steps
 *   between the three widths read as beads on a string. Sharing the radius at each node makes
 *   the whole run one tube that thins from root to tip.
 */
export const STREAM_CAT_VIEWBOX = "0 0 70 49";

/** Where the head is planted, and how many viewBox units one drawing unit is worth. */
const HEAD_PLACEMENT = catHeadTransform(51.5, 18, 0.047);

/** A point on a bone, and how wide the limb is there: one disc the tube's outline wraps. */
interface TubeNode {
  x: number;
  y: number;
  r: number;
}

/** Path numbers, trimmed. A thousandth of a viewBox unit is a six-hundredth of a pixel. */
function trim(value: number): string {
  return String(Math.round(value * 1000) / 1000);
}

/**
 * The outline of the convex hull of two discs: a capsule that tapers from one radius to the
 * other, as its two tangent lines and its two arcs. Exact, not sampled — the arcs *are* the
 * discs, which is what lets two of these share one and fuse without a seam.
 */
function taperedCapsule(a: TubeNode, b: TubeNode): string {
  const span = Math.hypot(b.x - a.x, b.y - a.y);
  const ux = (b.x - a.x) / span;
  const uy = (b.y - a.y) / span;
  // How far the common tangent leans along the bone to keep touching both discs; zero when
  // the two radii are equal, and the whole of it when one disc is about to swallow the other.
  const lean = (a.r - b.r) / span;
  const rise = Math.sqrt(Math.max(0, 1 - lean * lean));
  const sides = [
    { x: lean * ux - rise * uy, y: lean * uy + rise * ux },
    { x: lean * ux + rise * uy, y: lean * uy - rise * ux }
  ] as const;
  const at = (node: TubeNode, side: (typeof sides)[number]) =>
    `${trim(node.x + node.r * side.x)} ${trim(node.y + node.r * side.y)}`;
  // Both arcs wrap the far side of their own disc, in the same rotational direction, so every
  // capsule is wound the same way and overlapping ones add rather than cancel.
  return (
    `M${at(a, sides[0])}L${at(b, sides[0])}` +
    `A${trim(b.r)} ${trim(b.r)} 0 ${a.r < b.r ? 1 : 0} 0 ${at(b, sides[1])}` +
    `L${at(a, sides[1])}` +
    `A${trim(a.r)} ${trim(a.r)} 0 ${a.r > b.r ? 1 : 0} 0 ${at(a, sides[0])}Z`
  );
}

/**
 * A run of them, sharing a disc at every node: one tube, tapering, with no width step and no
 * seam anywhere along it. Drawn as one path of overlapping subpaths, which a non-zero fill
 * unions because they are all wound the same way.
 */
function taperedTube(nodes: readonly TubeNode[]): string {
  return nodes
    .slice(1)
    .map((node, index) => taperedCapsule(nodes[index]!, node))
    .join("");
}

/**
 * The same, following a cubic instead of a straight line: `steps` chords, each one a tapered
 * capsule sharing its discs with its neighbours.
 *
 * Chords rather than offset curves on purpose. The offset of a cubic is not a cubic, so it
 * would have to be flattened anyway — and a chain of hulls is tangent-continuous at every
 * node by construction, where a flattened offset is only as smooth as its sampling.
 */
function taperedCurve(
  from: TubeNode,
  first: readonly [number, number],
  second: readonly [number, number],
  to: TubeNode,
  steps: number
): string {
  const nodes: TubeNode[] = [];
  for (let step = 0; step <= steps; step += 1) {
    const t = step / steps;
    const m = 1 - t;
    const blend = (a: number, b: number, c: number, d: number) =>
      m * m * m * a + 3 * m * m * t * b + 3 * m * t * t * c + t * t * t * d;
    nodes.push({
      x: blend(from.x, first[0], second[0], to.x),
      y: blend(from.y, first[1], second[1], to.y),
      r: from.r + (to.r - from.r) * t
    });
  }
  return taperedTube(nodes);
}

/**
 * Every pivot the stylesheet turns something about, keyed by the class that turns.
 *
 * Read as: `.stream-cat-tail-mid` rotates about (9.6, 21.4), which is where the tail's base
 * segment ends and its middle one begins. Joints shared by a near and a far part are keyed by
 * the shared class, since both are drawn in the near part's coordinates.
 */
export const STREAM_CAT_JOINTS: Record<string, { x: number; y: number }> = {
  /** Under the middle of the cat, on the ground: what the whole figure rocks and breathes about. */
  "stream-cat": { x: 38, y: 42 },
  "stream-cat-tail": { x: 13.4, y: 26.6 },
  "stream-cat-tail-mid": { x: 9.6, y: 21.4 },
  "stream-cat-tail-tip": { x: 8.6, y: 14.6 },
  "stream-cat-leg--fore": { x: 42.6, y: 28.4 },
  "stream-cat-leg--hind": { x: 21.6, y: 29.2 },
  "stream-cat-shin--fore": { x: 42.9, y: 35.8 },
  "stream-cat-shin--hind": { x: 18, y: 35.6 },
  /** The base of the skull, where it meets the chest: what a head that nods is hinged on. */
  "stream-cat-head": { x: 47.5, y: 25 },
  "stream-cat-ear--far": { x: 47.8, y: 16.3 },
  "stream-cat-ear--near": { x: 57, y: 16.3 },
  /** The laptop's hinge, at the far end of the slab from the cat. */
  "stream-cat-laptop-lid": { x: 62.2, y: 39.1 }
};

/** One node of the scene graph: a `<path>` when it carries `d`, a `<g>` otherwise. */
export interface StreamCatPart {
  className?: string;
  /** A fixed placement that is never animated — the far-leg offset and the head's scale. */
  transform?: string;
  opacity?: number;
  d?: string;
  fillRule?: "evenodd";
  children?: StreamCatPart[];
}

/**
 * The torso: a haunch at the back, a dip at the waist and a chest that comes forward far
 * enough to carry the chin. The old body was a rounded rectangle, which is the shape of
 * everything and the silhouette of nothing.
 *
 * Its front edge stops at x=45.6. The near eye's leading edge is at x=47.5, and the body is
 * drawn under the head, so everything between those two numbers is the margin that keeps the
 * cat from looking out through a hole with a shoulder in it.
 */
const BODY =
  "M12.8 28.6C12.8 24.9 15 22.6 18.6 22.4C22.6 22.2 26 23 29.6 22.8C33.8 22.6 38.6 22.6 42 24.2C44.8 25.5 46 27.4 45.8 29.8C45.6 32.4 43.8 34.4 41 35C37 35.8 32.4 35 28.4 35.4C24.4 35.8 20.2 36 17 34.6C14 33.3 12.8 31.6 12.8 28.6Z";

/**
 * The same cat sitting down, drawn rather than derived.
 *
 * A cat sitting is not a cat rotated: its spine shortens as it comes up, its back end goes
 * from being carried to being sat on, and no transform of the standing body — rotation,
 * squash, or both — produces that silhouette. Every attempt lands somewhere between a cat
 * stretching and a cat falling over.
 *
 * So there are two bodies and exactly one is ever visible. They swap on a single frame, in
 * the middle of the paw landing on the lid, which is the loudest moment the animation has and
 * the only place a cut passes for force rather than for a glitch. The legs and the tail cut
 * on that same frame, so nothing is halfway between the two poses at any time.
 *
 * The front stops short of the floor so the forelegs read as their own column below it. The
 * forelegs themselves do not move to sit down — a cat's shoulder is the one thing that stays
 * put between standing and sitting — and both paws stay on the floor, because a paw left
 * reaching for the laptop takes away the only front leg the silhouette has and the cat reads
 * as lunging rather than sitting.
 */
const BODY_SEATED =
  "M40.2 23.8C44 23.7 46.8 25.2 47.2 27.8C47.6 30.4 47.2 33 45.8 34.8C44 37 41 37.8 38 38.8C34.4 40 30.6 42.3 28.2 41.5C25.8 40.6 25 38.4 25 35.6C25 31.6 26.2 27.6 29.2 25.4C31.8 23.5 36.4 23.9 40.2 23.8Z";

/**
 * Shoulder to wrist.
 *
 * The top of it is a disc centred on the shoulder joint itself, which is what keeps it out of
 * the silhouette: the chest curve falls away towards the throat, so a leg squared off across
 * the top of the shoulder rides up over it and puts an unexplained lump on the cat's
 * upper-right corner — and rotating the leg to take a stride pushes the lump further out. A
 * disc on the pivot can be tucked under the chest once and stays tucked at every angle,
 * because rotation about its own centre leaves it exactly where it was.
 */
const FORE_SHOULDER: TubeNode = { x: 42.6, y: 28.4, r: 2.5 };
const FORE_WRIST: TubeNode = { x: 42.9, y: 35.8, r: 1.8 };

/**
 * Wrist to the floor: the shin, and then the paw lying along the ground in front of it.
 *
 * The paw is a stadium rather than a block with a toe on it — the ground is a line the cat
 * stands on, not a surface it is drawn against, and a flat sole with a corner at each end
 * reads as a hoof at this size. Constant width from ankle to toe, so the whole sole rests on
 * y=42 rather than touching it at one point.
 */
const FORE_ANKLE: TubeNode = { x: 43, y: 40.45, r: 1.55 };
const FORE_TOE: TubeNode = { x: 44.5, y: 40.45, r: 1.55 };

/** The thigh: the mass that makes a cat's back end taller than its front, ending at the hock. */
const HIND_UPPER =
  "M26 26.4C27.4 30 26.6 33.8 22.8 36.6C21.2 37.7 17.2 37 16.6 35.2C15.8 32.6 17.2 29 19 26Z";

/**
 * Hock to the floor, angled forward — the second half of the bend that reads as a cat's leg.
 *
 * Thick at the hock, where it has to carry the width the thigh hands it and reach forward far
 * enough that the thigh's leading corner is not left hanging over air.
 */
const HIND_HOCK: TubeNode = { x: 18, y: 35.6, r: 2.3 };
const HIND_ANKLE: TubeNode = { x: 19.8, y: 40.4, r: 1.6 };
const HIND_TOE: TubeNode = { x: 22.6, y: 40.4, r: 1.6 };

/**
 * The tail, in three joints rather than one rigid hook.
 *
 * One tube from root to tip: the radius at each end of a segment is the radius the next
 * segment starts on, so the width falls off continuously and the joints are invisible. Its
 * liveliness is the lag between them — the tip is still going out while the base is already
 * coming back — which needs the joints to exist without being seen.
 */
const TAIL_ROOT: TubeNode = { x: 13.4, y: 26.6, r: 2.15 };
const TAIL_ELBOW: TubeNode = { x: 9.6, y: 21.4, r: 1.72 };
const TAIL_WRIST: TubeNode = { x: 8.6, y: 14.6, r: 1.26 };
const TAIL_POINT: TubeNode = { x: 12.2, y: 9.6, r: 0.9 };

/**
 * Every bone drawn as a tube, with the node it starts on and the node it ends on.
 *
 * Exported because two things about these numbers fail silently and only in motion. A bone
 * whose first node has drifted off the joint its group turns about no longer presents a disc
 * centred on the pivot, so the part it hangs off tears away from it at the ends of a swing;
 * and a bone whose first radius no longer matches the last radius of the bone above it puts a
 * step in the outline, which is the bead-on-a-string tail this replaced. Neither shows up in
 * a still frame of the resting pose, which is the only thing a snapshot would catch.
 *
 * `run` is every node in order, including the ones inside a bone that do not articulate — the
 * paw is the second half of its own shin, not a part of its own, because nothing turns there.
 */
export interface StreamCatBone {
  /** The group that turns this bone; a key of {@link STREAM_CAT_JOINTS} when it turns at all. */
  readonly className: string;
  readonly run: readonly TubeNode[];
}

export const STREAM_CAT_BONES: readonly StreamCatBone[] = [
  { className: "stream-cat-tail", run: [TAIL_ROOT, TAIL_ELBOW] },
  { className: "stream-cat-tail-mid", run: [TAIL_ELBOW, TAIL_WRIST] },
  { className: "stream-cat-tail-tip", run: [TAIL_WRIST, TAIL_POINT] },
  { className: "stream-cat-leg--fore", run: [FORE_SHOULDER, FORE_WRIST] },
  { className: "stream-cat-shin--fore", run: [FORE_WRIST, FORE_ANKLE, FORE_TOE] },
  { className: "stream-cat-shin--hind", run: [HIND_HOCK, HIND_ANKLE, HIND_TOE] }
];

const FORE_UPPER = taperedTube([FORE_SHOULDER, FORE_WRIST]);
const FORE_LOWER = taperedTube([FORE_WRIST, FORE_ANKLE, FORE_TOE]);
const HIND_LOWER = taperedTube([HIND_HOCK, HIND_ANKLE, HIND_TOE]);
const TAIL_BASE = taperedCurve(TAIL_ROOT, [12, 24.8], [10.4, 23.4], TAIL_ELBOW, 4);
const TAIL_MID = taperedCurve(TAIL_ELBOW, [8.8, 19.2], [8.3, 16.8], TAIL_WRIST, 4);
const TAIL_TIP = taperedCurve(TAIL_WRIST, [8.9, 12], [10.2, 10.4], TAIL_POINT, 6);

/** The slab, drawn closed; the lid rotates off it about the hinge at its far end. */
const LAPTOP_BASE = "M49.8 39.4C49.8 38.8 63 38.8 63 39.4L63 41.3C63 42 49.8 42 49.8 41.3Z";
const LAPTOP_LID = "M50.2 37.1C50.2 36.5 62.4 36.5 62.4 37.1L62.4 38.9C62.4 39.5 50.2 39.5 50.2 38.9Z";

function foreLeg(side: "near" | "far"): StreamCatPart {
  return {
    className: `stream-cat-leg stream-cat-leg--fore stream-cat-leg--fore-${side}`,
    children: [
      { d: FORE_UPPER },
      { className: `stream-cat-shin stream-cat-shin--fore stream-cat-shin--fore-${side}`, children: [{ d: FORE_LOWER }] }
    ]
  };
}

function hindLeg(side: "near" | "far"): StreamCatPart {
  return {
    className: `stream-cat-leg stream-cat-leg--hind stream-cat-leg--hind-${side}`,
    children: [
      { d: HIND_UPPER },
      { className: `stream-cat-shin stream-cat-shin--hind stream-cat-shin--hind-${side}`, children: [{ d: HIND_LOWER }] }
    ]
  };
}

/**
 * The cat itself, from the tail forward.
 *
 * Draw order is the depth order, and one step of it is load-bearing: the near foreleg comes
 * before the head, so a paw folded up to the muzzle can never be painted over an eye. Sharing
 * one flat colour means nothing is ever in front of anything — overlapping parts simply fuse —
 * so the only thing order decides is what shows through the holes.
 *
 * There is deliberately no group between the figure and its limbs. A cat sitting down is not
 * a cat rotated: the spine shortens as it comes up, and any single transform that puts the
 * back end on the floor either drags the head off the top of the box or leaves the cat
 * stretched out like it is having a long lie down. So the seated pose is posed part by part
 * in the stylesheet, the way an animator would, and the body is not transformed at all — it
 * is swapped for {@link BODY_SEATED}.
 */
export const STREAM_CAT_FIGURE: StreamCatPart[] = [
  {
    className: "stream-cat-tail",
    children: [
      { d: TAIL_BASE },
      {
        className: "stream-cat-tail-mid",
        children: [{ d: TAIL_MID }, { className: "stream-cat-tail-tip", children: [{ d: TAIL_TIP }] }]
      }
    ]
  },
  {
    // The depth pair: the same two legs, half a step further away and half a tone lighter.
    className: "stream-cat-far",
    transform: "translate(-3.7 0)",
    opacity: 0.45,
    children: [hindLeg("far"), foreLeg("far")]
  },
  { className: "stream-cat-body", d: BODY },
  { className: "stream-cat-seat", d: BODY_SEATED },
  hindLeg("near"),
  foreLeg("near"),
  {
    className: "stream-cat-head",
    children: [
      { className: "stream-cat-ear stream-cat-ear--far", children: [{ transform: HEAD_PLACEMENT, d: CAT_EAR_FAR }] },
      { className: "stream-cat-ear stream-cat-ear--near", children: [{ transform: HEAD_PLACEMENT, d: CAT_EAR_NEAR }] },
      { className: "stream-cat-face", transform: HEAD_PLACEMENT, fillRule: "evenodd", d: CAT_GLYPH_FLAT }
    ]
  }
];

/**
 * The laptop, which only exists while the cat is having anything to do with it.
 *
 * It is a sibling of the figure rather than a part of it: the figure rocks and breathes, and
 * a laptop that rocked with the cat would read as being held rather than stood over. Drawn
 * before the cat, so the paw that comes down on the lid is never behind it.
 */
export const STREAM_CAT_LAPTOP: StreamCatPart = {
  className: "stream-cat-laptop",
  children: [
    { className: "stream-cat-laptop-base", d: LAPTOP_BASE },
    { className: "stream-cat-laptop-lid", children: [{ d: LAPTOP_LID }] }
  ]
};
