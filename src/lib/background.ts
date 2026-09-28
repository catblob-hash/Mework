import chairPicture from "../assets/backgrounds/chair.jpg";
import chairThumbnail from "../assets/backgrounds/chair-thumb.jpg";
import curtainPicture from "../assets/backgrounds/curtain.jpg";
import curtainThumbnail from "../assets/backgrounds/curtain-thumb.jpg";
import deskPicture from "../assets/backgrounds/desk.jpg";
import deskThumbnail from "../assets/backgrounds/desk-thumb.jpg";
import shelfPicture from "../assets/backgrounds/shelf.jpg";
import shelfThumbnail from "../assets/backgrounds/shelf-thumb.jpg";

/**
 * The window's background (Appearance → Custom background), as saved in
 * `AppearancePreferences.background`.
 *
 * Each theme has a solid ground of its own, and `solid` — the default — is whichever
 * one the theme on screen owns, so it follows every change of theme with no write.
 * Picking the other theme's ground saves `solid:<scheme>`; that one is shown as it
 * is until the theme next changes, when it goes back to `solid`. Pictures, bundled
 * or imported, stay whatever the theme does.
 */

export type BackgroundScheme = "day" | "night";

export interface BuiltinPicture {
  /** The saved value, `builtin:<name>`. */
  id: string;
  /** Where the picture's cat is: under the desk, on the chair, on the bookshelf, behind the curtain. */
  name: "desk" | "chair" | "shelf" | "curtain";
  /** 3840×2400, for the window. */
  picture: string;
  /** 480×300, for the picker and the previews. */
  thumbnail: string;
  /**
   * The cat, as an `object-position`: cropping the picture to the window's shape
   * trims around it, so a tall or wide window still has it in view.
   */
  focus: string;
}

export const BUILTIN_PICTURES: readonly BuiltinPicture[] = [
  { id: "builtin:desk", name: "desk", picture: deskPicture, thumbnail: deskThumbnail, focus: "25% 81%" },
  { id: "builtin:chair", name: "chair", picture: chairPicture, thumbnail: chairThumbnail, focus: "90% 83%" },
  { id: "builtin:shelf", name: "shelf", picture: shelfPicture, thumbnail: shelfThumbnail, focus: "70% 49%" },
  { id: "builtin:curtain", name: "curtain", picture: curtainPicture, thumbnail: curtainThumbnail, focus: "95% 70%" }
];

export const THEME_SOLID = "solid";

export type Background =
  /** A theme's ground; `scheme` is null for the one the theme on screen owns. */
  | { kind: "solid"; scheme: BackgroundScheme | null }
  | { kind: "builtin"; picture: BuiltinPicture }
  | { kind: "imported"; id: string };

const IMPORTED_ID = /^[0-9a-f]{64}$/;

export function parseBackground(value: string): Background {
  if (value === "solid:day" || value === "solid:night") {
    return { kind: "solid", scheme: value === "solid:day" ? "day" : "night" };
  }
  const builtin = BUILTIN_PICTURES.find((picture) => picture.id === value);
  if (builtin) return { kind: "builtin", picture: builtin };
  if (IMPORTED_ID.test(value)) return { kind: "imported", id: value };
  return { kind: "solid", scheme: null };
}

/** The saved value for a background; anything unknown is the theme's own ground. */
export function normalizeBackground(value: unknown): string {
  if (typeof value !== "string") return THEME_SOLID;
  const background = parseBackground(value);
  return background.kind === "solid" && background.scheme === null ? THEME_SOLID : value;
}

/** What picking one theme's ground saves while `theme` is on screen. */
export function solidBackground(scheme: BackgroundScheme, theme: BackgroundScheme): string {
  return scheme === theme ? THEME_SOLID : `solid:${scheme}`;
}

/** The background after the theme on screen changes: a solid ground goes to the new theme's. */
export function backgroundAfterThemeChange(value: string): string {
  return parseBackground(value).kind === "solid" ? THEME_SOLID : value;
}

/**
 * Whether the window's ground is something other than the theme's own: a picture, or
 * the other theme's solid. The surfaces that paint the ground then let it through.
 */
export function replacesThemeGround(value: string): boolean {
  const background = parseBackground(value);
  return background.kind !== "solid" || background.scheme !== null;
}
