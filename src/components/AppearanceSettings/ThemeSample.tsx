import { useEffect, useRef, useState } from "react";
import type { JSX, RefObject } from "react";
import { frostPicture, parseGlassFilter } from "../../lib/glassPlate";

/**
 * Which palette a sample is drawn in. `current` and `opposite` are relative to the
 * theme on screen; `day` and `night` are absolute. The stylesheet resolves each to
 * the palette tokens that hold those colours right now — a night sample shown during
 * the day reads the tokens whose day value is the night colour — so every sample is
 * drawn in the palette's real values without the page switching theme.
 */
export type ThemeSampleScheme = "current" | "opposite" | "day" | "night";

/** The window's glass in miniature: its blur is the window's scaled down. */
const SAMPLE_GLASS = parseGlassFilter("blur(3px) saturate(130%)");

/**
 * `picture` baked as the sample's glass (`lib/glassPlate.ts`), for the size the sample is drawn
 * at; null until it is ready, or when there is no picture.
 */
function useFrostedPicture(picture: string | null, sample: RefObject<HTMLElement | null>): string | null {
  const [frosted, setFrosted] = useState<{ picture: string; url: string } | null>(null);
  useEffect(() => {
    const box = sample.current;
    if (!picture || !box?.clientWidth || !box.clientHeight) return;
    let disposed = false;
    void frostPicture(picture, box.clientWidth, box.clientHeight, SAMPLE_GLASS).then((url) => {
      if (!disposed && url) setFrosted({ picture, url });
    });
    return () => {
      disposed = true;
    };
  }, [picture, sample]);
  return frosted && frosted.picture === picture ? frosted.url : null;
}

/**
 * A miniature of the window — sidebar, top bar, the conversation tile with its
 * composer, and a side pane — drawn in one scheme's colours. `picture` lies behind the
 * window where its ground shows, in place of the scheme's solid one; with `glass`, the
 * sidebar and tiles are glass over whichever ground it is.
 */
export function ThemeSample({
  scheme,
  glass = false,
  picture = null,
  className
}: {
  scheme: ThemeSampleScheme;
  glass?: boolean;
  picture?: string | null;
  className?: string;
}): JSX.Element {
  const sampleRef = useRef<HTMLSpanElement>(null);
  const frosted = useFrostedPicture(glass ? picture : null, sampleRef);
  return (
    <span
      ref={sampleRef}
      className={`theme-sample${className ? ` ${className}` : ""}`}
      data-scheme={scheme}
      data-glass={glass || undefined}
      data-picture={picture ? true : undefined}
      aria-hidden="true"
    >
      {picture && <img className="theme-sample__picture" src={picture} alt="" draggable={false} />}
      {frosted && (
        <img
          className="theme-sample__picture theme-sample__picture--frosted"
          src={frosted}
          alt=""
          draggable={false}
        />
      )}
      <span className="theme-sample__sidebar">
        <span className="theme-sample__bar theme-sample__new" />
        <span className="theme-sample__selection" />
        <span className="theme-sample__bar theme-sample__nav theme-sample__nav--1" />
        <span className="theme-sample__bar theme-sample__nav theme-sample__nav--2" />
        <span className="theme-sample__bar theme-sample__nav theme-sample__nav--3" />
        <span className="theme-sample__bar theme-sample__nav theme-sample__nav--4" />
      </span>
      <span className="theme-sample__topbar">
        <span className="theme-sample__bar theme-sample__title" />
        <span className="theme-sample__actions" />
      </span>
      <span className="theme-sample__tiles">
        <span className="theme-sample__chat">
          <span className="theme-sample__bubble" />
          <span className="theme-sample__bar theme-sample__line theme-sample__line--1" />
          <span className="theme-sample__bar theme-sample__line theme-sample__line--2" />
          <span className="theme-sample__bar theme-sample__line theme-sample__line--3" />
          <span className="theme-sample__composer">
            <span className="theme-sample__send" />
          </span>
        </span>
        <span className="theme-sample__pane">
          <span className="theme-sample__bar theme-sample__pane-title" />
          <span className="theme-sample__task theme-sample__task--1" />
          <span className="theme-sample__task theme-sample__task--2" />
          <span className="theme-sample__task theme-sample__task--3" />
        </span>
      </span>
    </span>
  );
}
