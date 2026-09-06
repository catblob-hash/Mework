/**
 * The Mework brand glyph.
 *
 * Rendered inline rather than through `<img src=...>` because the mark is drawn in
 * `currentColor`: an external SVG loaded by `<img>` gets its own document and cannot see the
 * host page's color, so the glyph would have to hard-code a fill and stop tracking the
 * day/night palette. The geometry below mirrors `src/mework-mark.svg`; the companion test pins
 * the two together so the standalone asset and the component cannot drift.
 *
 * The eyes are holes punched by `fill-rule="evenodd"` rather than by a `<mask>`. A mask needs an
 * `id`, and every rendered instance would either collide on that id or need a generated one; the
 * even-odd form is self-contained, so the component stays safe to render many times on a page.
 */

/** Outer contour of the head, used on its own for the offset drop shadow. */
export const MEWORK_MARK_HEAD =
  "M6.6 27C5.6 20.4 5.8 10.8 7 6.6C7.8 4 10.2 3.6 12.2 5.4L24 19.4C27.2 15 34.6 15.4 38.8 17.4L51.4 10.6C53.6 9.2 56.2 10 56.8 12.8C57.8 16.4 58 21.6 56.8 27.2C58 43 47.8 57.2 31.2 57.2C15 57.2 4.8 42.8 6.6 27Z";

/** The head with both eyes appended as counter-contours. */
export const MEWORK_MARK_GLYPH = `${MEWORK_MARK_HEAD}M21.3 30.9a3.5 5 0 1 0 7 0a3.5 5 0 1 0-7 0ZM35.3 31.9a3.5 4 0 1 0 7 0a3.5 4 0 1 0-7 0Z`;

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
      <g transform="rotate(-7 32 34)">
        <path
          fill="#b68235"
          fillOpacity={0.34}
          transform="translate(2.6 2.2)"
          d={MEWORK_MARK_HEAD}
        />
        <path fill="currentColor" fillRule="evenodd" d={MEWORK_MARK_GLYPH} />
      </g>
    </svg>
  );
}
