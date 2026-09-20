import { CAT_GLYPH, CAT_HEAD, catHeadTransform } from "./catArt";

/**
 * The Mework brand glyph: the head of the cat the product is drawn with, as a solid
 * silhouette with its face punched out of it.
 *
 * The head rather than the whole drawing, because every slot that renders this is small — 25px
 * in the sidebar, 28px on the update card, 38px on the startup screen — and the desk, the
 * laptop and the tail stop resolving into anything well above that. The boxed application icon
 * keeps the whole scene for its large frames and falls back to this same head below 64px.
 *
 * Rendered inline rather than through `<img src=...>` because the mark is drawn in
 * `currentColor`: an external SVG loaded by `<img>` gets its own document and cannot see the
 * host page's color, so the glyph would have to hard-code a fill and stop tracking the
 * day/night palette. The geometry comes from `catArt.ts`, which is also what the standalone
 * assets are authored from; the companion test pins the two together so the shipped file and
 * the in-app copy cannot drift.
 */

/** Fits the head into the 64-unit viewBox, with room for the shadow. */
export const MEWORK_MARK_TRANSFORM = catHeadTransform(31.8, 32.2, 0.1351);

/** Offset of the gold under-shadow, in drawing units (≈2.6 × 2.2 at 64). */
export const MEWORK_MARK_SHADOW_OFFSET = "translate(19.25 16.28)";

interface MeworkIconProps {
  className?: string;
  label?: string;
  size?: number;
}

export function MeworkIcon({
  className,
  label,
  size = 24
}: MeworkIconProps) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 64 64"
      fill="currentColor"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      <g transform={MEWORK_MARK_TRANSFORM}>
        <path
          fill="#b68235"
          fillOpacity={0.34}
          transform={MEWORK_MARK_SHADOW_OFFSET}
          d={CAT_HEAD}
        />
        <path fill="currentColor" fillRule="evenodd" d={CAT_GLYPH} />
      </g>
    </svg>
  );
}
