/**
 * What the glass shows under a box, as one colour, for boxes on the glass that are solid.
 *
 * On macOS a backdrop filter over another one, or a box that repaints over one, leaves
 * stray blocks on screen that no screenshot catches (docs/why.md), so the chips above
 * the composer are solid in glass mode. They still look like the glass by painting its
 * tint over the colour its filter would have made of the background there: the
 * background under the box, averaged as far out as the blur reaches, passed through the
 * filter's saturate() and brightness(). The tint is left to the stylesheet
 * (`backdrop.css`), which knows which glass lies under the box.
 *
 * `AppBackdrop` hands over each picture once it has loaded; a solid ground is read off
 * the background layer itself.
 */

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** A backdrop filter, reduced to what it does to one flat colour. */
export interface GlassFilter {
  /** How far, in CSS pixels, the blur draws from. */
  blur: number;
  saturate: number;
  brightness: number;
}

interface Picture {
  image: HTMLImageElement;
  width: number;
  height: number;
  /** RGBA of the picture shrunk to `width` × `height`. */
  pixels: Uint8ClampedArray;
}

/** Wide enough that a sample is a few CSS pixels of a full-window picture, well under any blur. */
const SAMPLE_WIDTH = 256;

let picture: Picture | null = null;
const listeners = new Set<() => void>();

/** The picture on screen, once it has loaded; null while the ground is a solid one. */
export function setBackdropPicture(image: HTMLImageElement | null): void {
  picture = image ? readPicture(image) : null;
  for (const listener of listeners) listener();
}

function readPicture(image: HTMLImageElement): Picture | null {
  let width = image.naturalWidth;
  let height = image.naturalHeight;
  if (!width || !height) return null;
  try {
    // Each halving makes a pixel the mean of the four under it, so the small copy is an
    // average of the picture rather than a sampling of it. The engines' filters differ
    // in detail, not in the mean of a region, which is all that is read from it.
    let source: CanvasImageSource = image;
    let context: CanvasRenderingContext2D | null;
    do {
      const next = Math.min(width, Math.max(SAMPLE_WIDTH, Math.round(width / 2)));
      height = Math.max(1, Math.round(height * next / width));
      width = next;
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      context = canvas.getContext("2d");
      if (!context) return null;
      context.drawImage(source, 0, 0, width, height);
      source = canvas;
    } while (width > SAMPLE_WIDTH);
    return { image, width, height, pixels: context.getImageData(0, 0, width, height).data };
  } catch {
    // A picture the canvas may not read back leaves the solid ground's colour.
    return null;
  }
}

/** A `backdrop-filter` value, as far as the glass uses one: blur, saturate, brightness. */
export function parseGlassFilter(value: string): GlassFilter {
  const filter: GlassFilter = { blur: 0, saturate: 1, brightness: 1 };
  for (const match of value.matchAll(/(blur|saturate|brightness)\(\s*(-?[\d.]+)(px|%)?\s*\)/g)) {
    const amount = Number.parseFloat(match[2]);
    if (!Number.isFinite(amount)) continue;
    if (match[1] === "blur") filter.blur = amount;
    else filter[match[1] as "saturate" | "brightness"] = match[3] === "%" ? amount / 100 : amount;
  }
  return filter;
}

/** The colour a filter makes of a flat one: the saturate() matrix, then brightness(). */
export function filterColor(color: Rgb, filter: GlassFilter): Rgb {
  const s = filter.saturate;
  const { r, g, b } = color;
  const saturated = {
    r: (0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b,
    g: (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b,
    b: (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b
  };
  // Each step of a filter clamps what it makes, as the engines' do.
  const clamp = (value: number) => Math.min(255, Math.max(0, value));
  const channel = (value: number) => clamp(clamp(value) * filter.brightness);
  return { r: channel(saturated.r), g: channel(saturated.g), b: channel(saturated.b) };
}

/**
 * Where a picture of this size lies when `object-fit: cover` fits it to `box` at
 * `position` (a computed `object-position`): the part outside the box is cropped away.
 */
export function coverRect(box: Box, width: number, height: number, position: string): Box {
  const scale = Math.max(box.width / width, box.height / height);
  const shown = { width: width * scale, height: height * scale };
  const [x = "50%", y = "50%"] = position.trim().split(/\s+/);
  return {
    left: box.left + positionOffset(x, box.width - shown.width),
    top: box.top + positionOffset(y, box.height - shown.height),
    ...shown
  };
}

function positionOffset(token: string, free: number): number {
  if (token.endsWith("%")) return free * Number.parseFloat(token) / 100;
  if (token.endsWith("px")) return Number.parseFloat(token);
  if (token === "left" || token === "top") return 0;
  if (token === "right" || token === "bottom") return free;
  return free / 2;
}

/** The mean colour of an RGBA image over a rectangle of its pixels; null if none lie in it. */
export function regionMean(pixels: Uint8ClampedArray, width: number, height: number, region: Box): Rgb | null {
  const x0 = Math.max(0, Math.floor(region.left));
  const y0 = Math.max(0, Math.floor(region.top));
  const x1 = Math.min(width, Math.ceil(region.left + region.width));
  const y1 = Math.min(height, Math.ceil(region.top + region.height));
  if (x1 <= x0 || y1 <= y0) return null;
  let r = 0;
  let g = 0;
  let b = 0;
  for (let y = y0; y < y1; y += 1) {
    for (let index = (y * width + x0) * 4, end = (y * width + x1) * 4; index < end; index += 4) {
      r += pixels[index];
      g += pixels[index + 1];
      b += pixels[index + 2];
    }
  }
  const count = (x1 - x0) * (y1 - y0);
  return { r: r / count, g: g / count, b: b / count };
}

/** The background's mean colour over a box of the window, in the window's coordinates. */
function backgroundMean(area: Box): Rgb | null {
  if (picture?.image.isConnected) {
    const placed = coverRect(
      picture.image.getBoundingClientRect(),
      picture.width,
      picture.height,
      getComputedStyle(picture.image).objectPosition
    );
    const scale = picture.width / placed.width;
    const mean = regionMean(picture.pixels, picture.width, picture.height, {
      left: (area.left - placed.left) * scale,
      top: (area.top - placed.top) * scale,
      width: area.width * scale,
      height: area.height * scale
    });
    if (mean) return mean;
  }
  const layer = document.querySelector(".app-backdrop");
  return layer ? parseRgb(getComputedStyle(layer).backgroundColor) : null;
}

function parseRgb(value: string): Rgb | null {
  const match = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/.exec(value);
  return match ? { r: Number(match[1]), g: Number(match[2]), b: Number(match[3]) } : null;
}

/** What `filter` shows of the background under `box`: its blur's reach averaged, then its colour change. */
function groundUnder(box: DOMRect, filter: GlassFilter): Rgb | null {
  const mean = backgroundMean({
    left: box.left - filter.blur,
    top: box.top - filter.blur,
    width: box.width + 2 * filter.blur,
    height: box.height + 2 * filter.blur
  });
  return mean && filterColor(mean, filter);
}

function paintGrounds(container: HTMLElement, selector: string): void {
  const root = document.documentElement;
  const tokens = root.dataset.glass === "true" ? getComputedStyle(root) : null;
  // The tiles' glass, as `backdrop.css` spells it, and the draft's clearer one.
  const tiles = tokens && parseGlassFilter(
    `blur(${tokens.getPropertyValue("--glass-blur")}) saturate(${tokens.getPropertyValue("--glass-saturate")})`
  );
  const draft = tokens && parseGlassFilter(tokens.getPropertyValue("--glass-draft-filter"));
  for (const element of container.querySelectorAll<HTMLElement>(selector)) {
    const box = element.getBoundingClientRect();
    setColorProperty(element, "--glass-ground", tiles && groundUnder(box, tiles));
    setColorProperty(element, "--glass-ground-draft", draft && groundUnder(box, draft));
  }
}

function setColorProperty(element: HTMLElement, name: string, color: Rgb | null): void {
  const value = color ? `rgb(${Math.round(color.r)} ${Math.round(color.g)} ${Math.round(color.b)})` : "";
  if (element.style.getPropertyValue(name) === value) return;
  if (value) element.style.setProperty(name, value);
  else element.style.removeProperty(name);
}

/**
 * Keeps `--glass-ground` (under the tiles' glass) and `--glass-ground-draft` (under a
 * draft's) on each box in `container` that matches `selector`, each for the background
 * under it, for as long as `container` is mounted; returns what stops it.
 */
export function watchGlassGround(container: HTMLElement, selector: string): () => void {
  let frame = 0;
  const schedule = (): void => {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      paintGrounds(container, selector);
    });
  };
  // Right away, so the first frame is painted with it.
  paintGrounds(container, selector);
  // The boxes move whenever one they sit in changes size: the composer growing a line, a
  // dock opening over it, the sidebar or a side pane, the window.
  const resizes = typeof ResizeObserver === "function" ? new ResizeObserver(schedule) : null;
  for (let box: HTMLElement | null = container; box; box = box.parentElement) resizes?.observe(box);
  // And whenever one of them comes, goes or changes its text, which moves those after it.
  const contents = new MutationObserver(schedule);
  contents.observe(container, { childList: true, subtree: true, characterData: true });
  // Theme, glass and ground switches change the tokens and the solid ground.
  const switches = new MutationObserver(schedule);
  switches.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme", "data-glass", "data-backdrop"]
  });
  listeners.add(schedule);
  return () => {
    cancelAnimationFrame(frame);
    resizes?.disconnect();
    contents.disconnect();
    switches.disconnect();
    listeners.delete(schedule);
  };
}

/** A ref callback for `watchGlassGround`, to create once per selector and keep. */
export function glassGroundRef(selector: string): (container: HTMLElement | null) => (() => void) | undefined {
  return (container) => (container ? watchGlassGround(container, selector) : undefined);
}
