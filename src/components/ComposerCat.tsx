import { CAT_BODY, CAT_FACE, CAT_LAPTOP, CAT_TAIL } from "./catArt";

/**
 * The cat loafing on the top rim of a draft composer, with its laptop.
 *
 * This is not a drawing of the brand cat — it is the brand cat, the same path data the
 * application icon is cut from, with one piece left out. The icon's scene puts the cat on a
 * desk; here the composer's own top border is the desk, so the desk slab is dropped and the
 * cat is lowered onto the border line instead. That is what sells the composer as the thing
 * the cat is lying on rather than something it floats above.
 *
 * ## The two coordinate facts the CSS depends on
 *
 * The viewBox is the artwork's own drawing space, so every number in `orchestration.css` —
 * the transform origins especially — is a drawing coordinate and can be read straight off
 * `catArt.ts`. The ledge line is y=730.9: {@link CAT_TAIL} hangs from exactly there in the
 * original scene, because that is the desk's underside, and {@link CAT_BODY} is lowered onto
 * it by the slab's own thickness so both meet the border at one line. Move one without the
 * other and the cat either floats or the tail grows out of its chest.
 *
 * The box is padded rather than tight because the breath scales the figure about the ledge:
 * at 1.035 the top of the ears reaches y=348.7 and the tip of the tail y=948.7, so a viewBox
 * fitted to the neutral pose would clip both ends once a second.
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
    <svg className="composer-cat" viewBox="292 344 724 610" aria-hidden="true">
      <g className="composer-cat__figure">
        {/* Hangs from the ledge unchanged: in the scene this is where the desk's
            underside was, and here that line is the composer's border. */}
        <path className="composer-cat__tail" d={CAT_TAIL} />
        {/* 62.12 is the desk slab's thickness — the distance from the surface the cat
            lies on (y=668.78) to the underside the tail hangs from (y=730.9). */}
        <g transform="translate(0 62.12)">
          <path className="composer-cat__body" fillRule="evenodd" d={`${CAT_BODY}${CAT_LAPTOP}${CAT_FACE}`} />
        </g>
      </g>
    </svg>
  );
}
