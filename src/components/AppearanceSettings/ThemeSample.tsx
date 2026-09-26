import type { JSX } from "react";

/**
 * Which palette a sample is drawn in. `current` and `opposite` are relative to the
 * theme on screen; `day` and `night` are absolute. The stylesheet resolves each to
 * the palette tokens that hold those colours right now — a night sample shown during
 * the day reads the tokens whose day value is the night colour — so every sample is
 * drawn in the palette's real values without the page switching theme.
 */
export type ThemeSampleScheme = "current" | "opposite" | "day" | "night";

/**
 * A miniature of the window — sidebar, top bar, the conversation tile with its
 * composer, and a side pane — drawn in one scheme's colours. With `glass`, it is the
 * custom-background look instead: `picture` (or a stand-in gradient) behind glass tiles.
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
  return (
    <span
      className={`theme-sample${className ? ` ${className}` : ""}`}
      data-scheme={scheme}
      data-glass={glass || undefined}
      aria-hidden="true"
    >
      {glass && (picture
        ? <img className="theme-sample__picture" src={picture} alt="" draggable={false} />
        : <span className="theme-sample__picture theme-sample__picture--placeholder" />)}
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
