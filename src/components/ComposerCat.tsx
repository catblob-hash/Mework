import { CAT_BODY, CAT_FACE, CAT_LAPTOP, CAT_ON_LEDGE, CAT_SCENE_VIEWBOX, CAT_TAIL } from "./catArt";

/**
 * The cat loafing on the top rim of a draft composer, with its laptop.
 *
 * This is the mascot's own drawing (`catArt.ts`), not the brand mark. The original drawing
 * puts the cat on a desk; here the composer's own top border is the desk, so the cat is
 * lowered onto the border line instead of a slab. That is what sells the composer as the
 * thing the cat is lying on rather than something it floats above.
 *
 * ## The coordinate facts the CSS depends on
 *
 * The viewBox is the artwork's own drawing space ({@link CAT_SCENE_VIEWBOX}, shared with the
 * streaming cat), so every number in `orchestration.css` — the transform origins especially —
 * is a drawing coordinate and can be read straight off `catArt.ts`. The ledge line is
 * y=730.9, and the box is padded round it for the breath; both are explained where they are
 * defined.
 *
 * ## Why the cat and the laptop are one path
 *
 * They overlap, and under `fill-rule="evenodd"` the overlap cancels — that white notch is the
 * gap that reads as the far paw resting on the keyboard. Drawing them as two filled shapes
 * fills the notch in and the paw disappears into the laptop. The face is punched out of the
 * same path for the same reason, which is also why the head cannot be animated separately
 * here: anything drawn over the head to nod it would plug the eyes. The figure breathes and
 * the tail sways instead.
 */
export function ComposerCat() {
  return (
    <svg className="composer-cat" viewBox={CAT_SCENE_VIEWBOX} aria-hidden="true">
      <g className="composer-cat__figure">
        {/* Hangs from the ledge unchanged: in the scene this is where the desk's
            underside was, and here that line is the composer's border. */}
        <path className="composer-cat__tail" d={CAT_TAIL} />
        <g transform={CAT_ON_LEDGE}>
          <path className="composer-cat__body" fillRule="evenodd" d={`${CAT_BODY}${CAT_LAPTOP}${CAT_FACE}`} />
        </g>
      </g>
    </svg>
  );
}
