import {
  BRAND_AMBER,
  BRAND_PLATE,
  BRAND_PROMPT,
  ICON_BARE_VIEWBOX,
  ICON_MARK_TRANSFORM,
  ICON_PROMPT_PATH,
  MARK_PATH,
  MARK_VIEWBOX
} from "./brandMark";

/**
 * The Mework brand in the app: the application icon and the bare mark. The geometry is
 * `brandMark.ts`, which the shipped SVG assets are cut from too.
 *
 * Rendered inline rather than through `<img src=...>` so the mark can take its color from
 * the page: an external SVG loaded by `<img>` gets its own document and
 * cannot see the host's palette, so it could not follow day and night.
 */

interface BrandProps {
  className?: string;
}

/**
 * The application icon as the Dock and the taskbar show it: the prompt and the amber mark on
 * the dark plate. Drawn edge to edge — the OS margins the shipped files carry are the OS's —
 * and in fixed colors, because it is a picture of the app rather than a piece of the page.
 *
 * Without the plate it is a piece of the page instead: the prompt takes `currentColor` and the
 * mark the amber token, so both follow day and night, and the box closes in on the two of them.
 */
export function MeworkIcon({ className, size = 24, plate = true }: BrandProps & { size?: number; plate?: boolean }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox={plate ? "0 0 100 100" : ICON_BARE_VIEWBOX}
      aria-hidden={true}
      focusable="false"
    >
      {plate && (
        <>
          <rect width="100" height="100" rx="23" fill={BRAND_PLATE} />
          <rect
            x="0.5"
            y="0.5"
            width="99"
            height="99"
            rx="22.5"
            fill="none"
            stroke="#ffffff"
            strokeOpacity={0.07}
          />
        </>
      )}
      <path
        d={ICON_PROMPT_PATH}
        fill="none"
        stroke={plate ? BRAND_PROMPT : "currentColor"}
        strokeWidth={6}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        className={plate ? undefined : "mework-icon__mark"}
        d={MARK_PATH}
        transform={ICON_MARK_TRANSFORM}
        fill={BRAND_AMBER}
      />
    </svg>
  );
}

/** The mark alone, filled with `currentColor`; size it by height in CSS. */
export function MeworkMark({ className }: BrandProps) {
  return (
    <svg
      className={className}
      viewBox={MARK_VIEWBOX}
      fill="currentColor"
      aria-hidden={true}
      focusable="false"
    >
      <path d={MARK_PATH} />
    </svg>
  );
}
