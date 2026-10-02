/**
 * The liquid glass, baked.
 *
 * Glass is the window's background blurred and saturated, with each surface's tint laid
 * over it. Nothing on the page blurs anything as it is drawn: a `backdrop-filter` is a
 * backdrop layer that macOS's window server blurs afresh whenever anything under or inside
 * it repaints, and that left stray blocks of colour on screen that no screenshot catches
 * (docs/why.md). The background is a still picture, so its glass can be one too. Each
 * filter the glass uses is run once over the picture here, and the stylesheet
 * (`backdrop.css`) lays the result under every glass surface, fixed to the window and fitted
 * as the picture is (`--glass-under`), so a surface shows the part of it that lies under it
 * wherever the surface is and however it moves.
 *
 * Only the background is baked in, so glass is one layer deep: what floats over a glass
 * surface is solid, or a veil over the glass, never glass over glass.
 *
 * The baked picture is small. Blurring takes away everything finer than the blur, so a copy
 * a few pixels per blur radius across holds all that is left, and the browser's own scaling
 * draws it at the window's size without anything a full-size copy would add. The CSP allows
 * pictures only from the app and `data:`, so each is a PNG data URL.
 */

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

/** A `backdrop-filter`, as far as the glass uses one: blur, saturate, brightness. */
export interface GlassFilter {
  /** The blur's standard deviation, in CSS pixels. */
  blur: number;
  saturate: number;
  brightness: number;
}

/** How many pixels of the baked copy the blur's radius spans; enough that scaling it up shows nothing. */
const PLATE_SIGMA = 3;
/** The longest side a baked copy may have, should a small blur over a large picture ask for more. */
const PLATE_MAX = 1024;

/** A filter value as `backdrop.css` writes one: `blur(22px) saturate(130%)`, in either spelling of an amount. */
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

/** The colour a filter makes of a flat one: the saturate() matrix, then brightness(). A blur leaves it as it is. */
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
 * The radii of three box blurs that, run one after another, make a Gaussian blur of `sigma`
 * (Kovesi's widths: the two nearest odd ones, as many of each as the variance asks for).
 */
export function boxRadii(sigma: number): [number, number, number] {
  const variance = 12 * sigma * sigma;
  let lower = Math.floor(Math.sqrt(variance / 3 + 1));
  if (lower % 2 === 0) lower -= 1;
  lower = Math.max(1, lower);
  const narrow = Math.round((variance - 3 * lower * lower - 12 * lower - 9) / (-4 * lower - 4));
  const radius = (index: number) => ((index < narrow ? lower : lower + 2) - 1) / 2;
  return [radius(0), radius(1), radius(2)];
}

/** One box blur along a line of `length` values `stride` apart from `start`; the ends repeat outwards. */
function blurLine(
  source: Float32Array,
  target: Float32Array,
  start: number,
  stride: number,
  length: number,
  radius: number
): void {
  const last = length - 1;
  const scale = 1 / (2 * radius + 1);
  let sum = source[start] * (radius + 1);
  for (let offset = 1; offset <= radius; offset += 1) sum += source[start + Math.min(offset, last) * stride];
  for (let index = 0; index < length; index += 1) {
    target[start + index * stride] = sum * scale;
    sum += source[start + Math.min(index + radius + 1, last) * stride] - source[start + Math.max(index - radius, 0) * stride];
  }
}

/** Blurs the colour of an RGBA image in place, as `blur(sigma)` does, with `sigma` in its pixels. */
export function blurPixels(pixels: Uint8ClampedArray, width: number, height: number, sigma: number): void {
  if (!(sigma > 0)) return;
  const radii = boxRadii(sigma);
  const count = width * height;
  const plane = new Float32Array(count);
  const scratch = new Float32Array(count);
  for (let channel = 0; channel < 3; channel += 1) {
    for (let index = 0; index < count; index += 1) plane[index] = pixels[index * 4 + channel];
    for (const radius of radii) {
      if (radius <= 0) continue;
      for (let row = 0; row < height; row += 1) blurLine(plane, scratch, row * width, 1, width, radius);
      for (let column = 0; column < width; column += 1) blurLine(scratch, plane, column, width, height, radius);
    }
    for (let index = 0; index < count; index += 1) pixels[index * 4 + channel] = plane[index];
  }
}

/** Runs a filter's colour change over every pixel of an RGBA image, in place. */
export function filterPixels(pixels: Uint8ClampedArray, filter: GlassFilter): void {
  if (filter.saturate === 1 && filter.brightness === 1) return;
  for (let index = 0; index < pixels.length; index += 4) {
    const color = filterColor({ r: pixels[index], g: pixels[index + 1], b: pixels[index + 2] }, filter);
    pixels[index] = color.r;
    pixels[index + 1] = color.g;
    pixels[index + 2] = color.b;
  }
}

/**
 * A picture shrunk to `width` × `height` on a canvas. It is halved step by step first, each step
 * making a pixel the mean of the four under it, so the copy is an average of the picture rather
 * than a sampling of it, which would alias.
 */
function shrink(
  image: CanvasImageSource,
  naturalWidth: number,
  naturalHeight: number,
  width: number,
  height: number
): HTMLCanvasElement | null {
  let source: CanvasImageSource = image;
  let sourceWidth = naturalWidth;
  let sourceHeight = naturalHeight;
  for (;;) {
    const halving = sourceWidth / 2 >= width && sourceHeight / 2 >= height;
    const nextWidth = halving ? Math.round(sourceWidth / 2) : width;
    const nextHeight = halving ? Math.round(sourceHeight / 2) : height;
    const canvas = document.createElement("canvas");
    canvas.width = nextWidth;
    canvas.height = nextHeight;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.drawImage(source, 0, 0, nextWidth, nextHeight);
    if (!halving) return canvas;
    source = canvas;
    sourceWidth = nextWidth;
    sourceHeight = nextHeight;
  }
}

/**
 * A picture with a glass filter run over it, as a PNG data URL: blurred by `sigma` of its own
 * pixels, then saturated and brightened as `filter` says (its own `blur` is not read). Null
 * when the picture cannot be read back, which leaves the surfaces their tint alone.
 */
function bakeGlass(
  image: CanvasImageSource,
  naturalWidth: number,
  naturalHeight: number,
  sigma: number,
  filter: GlassFilter
): string | null {
  if (!naturalWidth || !naturalHeight) return null;
  const scale = Math.min(1, PLATE_SIGMA / Math.max(sigma, 0.001), PLATE_MAX / Math.max(naturalWidth, naturalHeight));
  const width = Math.max(1, Math.round(naturalWidth * scale));
  const height = Math.max(1, Math.round(naturalHeight * scale));
  try {
    const canvas = shrink(image, naturalWidth, naturalHeight, width, height);
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return null;
    const frame = context.getImageData(0, 0, width, height);
    blurPixels(frame.data, width, height, sigma * (width / naturalWidth));
    filterPixels(frame.data, filter);
    context.putImageData(frame, 0, 0);
    return canvas.toDataURL("image/png");
  } catch {
    return null;
  }
}

/** How many of the picture's pixels make one CSS pixel when `object-fit: cover` fits it to a box. */
function coverScale(boxWidth: number, boxHeight: number, width: number, height: number): number {
  return Math.max(boxWidth / width, boxHeight / height);
}

function parseRgb(value: string): Rgb | null {
  const match = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/.exec(value);
  return match ? { r: Number(match[1]), g: Number(match[2]), b: Number(match[3]) } : null;
}

function flat(color: Rgb): string {
  const value = `rgb(${Math.round(color.r)} ${Math.round(color.g)} ${Math.round(color.b)})`;
  return `linear-gradient(${value}, ${value})`;
}

/* The window's glass. */

/** The picture on screen, once it has loaded; null while the ground is a solid one. */
let picture: HTMLImageElement | null = null;
/**
 * What the plates on the root were baked from, so an unchanged window is not baked again: the
 * picture's address (an imported one's is a whole data URL, so it is kept apart) and the rest.
 */
let bakedSource = "";
let baked = "";
let watching = 0;

const PLATE_PROPERTIES = ["--glass-plate", "--glass-plate-draft", "--glass-plate-position"] as const;

function setPlates(values: Partial<Record<(typeof PLATE_PROPERTIES)[number], string>>): void {
  const style = document.documentElement.style;
  for (const name of PLATE_PROPERTIES) {
    const value = values[name];
    if (value) {
      if (style.getPropertyValue(name) !== value) style.setProperty(name, value);
    } else {
      style.removeProperty(name);
    }
  }
}

/**
 * Bakes the plates the glass lies on and sets them on the root: `--glass-plate` for the tiles'
 * filter (`--glass-blur`, `--glass-saturate`), `--glass-plate-draft` for a draft's clearer one
 * (`--glass-draft-filter`), and `--glass-plate-position`, the picture's `object-position`. Over a
 * solid ground a plate is that ground's colour through the filter. Without glass there are none.
 */
function bakeWindow(): void {
  const root = document.documentElement;
  if (root.dataset.glass !== "true") {
    baked = "";
    bakedSource = "";
    setPlates({});
    return;
  }
  const tokens = getComputedStyle(root);
  const tiles = parseGlassFilter(
    `blur(${tokens.getPropertyValue("--glass-blur")}) saturate(${tokens.getPropertyValue("--glass-saturate")})`
  );
  const draft = parseGlassFilter(tokens.getPropertyValue("--glass-draft-filter"));
  const filters = JSON.stringify([tiles, draft]);
  const image = picture?.isConnected && picture.naturalWidth ? picture : null;
  if (image) {
    const { naturalWidth: width, naturalHeight: height } = image;
    const scale = coverScale(window.innerWidth, window.innerHeight, width, height);
    // The blur is set in CSS pixels, so a window resized far enough wants another bake; a few
    // percent either way is not to be seen.
    const step = Math.round(Math.log(scale) / Math.log(1.05));
    const source = image.currentSrc || image.src;
    const key = `picture|${width}x${height}|${step}|${filters}`;
    if (key === baked && source === bakedSource) return;
    const tilesPlate = bakeGlass(image, width, height, tiles.blur / scale, tiles);
    const draftPlate = bakeGlass(image, width, height, draft.blur / scale, draft);
    baked = key;
    bakedSource = source;
    setPlates({
      "--glass-plate": tilesPlate ? `url("${tilesPlate}")` : undefined,
      "--glass-plate-draft": draftPlate ? `url("${draftPlate}")` : undefined,
      "--glass-plate-position": getComputedStyle(image).objectPosition
    });
    return;
  }
  const layer = document.querySelector(".app-backdrop");
  const ground = layer ? parseRgb(getComputedStyle(layer).backgroundColor) : null;
  const key = `ground|${ground ? JSON.stringify(ground) : ""}|${filters}`;
  if (key === baked) return;
  baked = key;
  bakedSource = "";
  setPlates(ground ? {
    "--glass-plate": flat(filterColor(ground, tiles)),
    "--glass-plate-draft": flat(filterColor(ground, draft))
  } : {});
}

/**
 * The picture on screen, handed over once it has loaded (`AppBackdrop`), or null while the
 * ground is a solid one or its colour has changed. The plates follow at once, so the frame that
 * shows the picture shows its glass.
 */
export function setBackdropPicture(image: HTMLImageElement | null): void {
  picture = image;
  if (watching) bakeWindow();
}

/**
 * Keeps the window's plates baked for as long as the backdrop is mounted; returns what stops
 * it. Theme, glass and ground switches change the filters or the ground, and a resize the
 * blur's reach in the picture's pixels, which is baked once the window has settled.
 */
export function watchGlassPlates(): () => void {
  watching += 1;
  bakeWindow();
  const switches = new MutationObserver(bakeWindow);
  switches.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme", "data-glass", "data-backdrop"]
  });
  let timer = 0;
  const onResize = (): void => {
    window.clearTimeout(timer);
    timer = window.setTimeout(bakeWindow, 150);
  };
  window.addEventListener("resize", onResize);
  return () => {
    watching -= 1;
    switches.disconnect();
    window.clearTimeout(timer);
    window.removeEventListener("resize", onResize);
    if (!watching) {
      baked = "";
      bakedSource = "";
      setPlates({});
    }
  };
}

/* Pictures in miniature (the theme cards). */

const frosted = new Map<string, Promise<string | null>>();

/**
 * A picture frosted as the window's glass is, for drawing at `boxWidth` × `boxHeight` with
 * `object-fit: cover`: `filter`'s blur is in CSS pixels of that box. Kept per picture and size,
 * since the cards ask for the same one again each time the settings open.
 */
export function frostPicture(src: string, boxWidth: number, boxHeight: number, filter: GlassFilter): Promise<string | null> {
  const key = `${src}|${Math.round(boxWidth)}x${Math.round(boxHeight)}|${JSON.stringify(filter)}`;
  let result = frosted.get(key);
  if (!result) {
    result = new Promise<HTMLImageElement>((resolve, reject) => {
      const image = new Image();
      image.onload = () => resolve(image);
      image.onerror = () => reject(new Error("picture failed to load"));
      image.src = src;
    })
      .then((image) => {
        const { naturalWidth: width, naturalHeight: height } = image;
        const scale = coverScale(boxWidth, boxHeight, width, height);
        return bakeGlass(image, width, height, filter.blur / scale, filter);
      })
      .catch(() => null);
    if (frosted.size >= 8) frosted.delete(frosted.keys().next().value as string);
    frosted.set(key, result);
  }
  return result;
}
