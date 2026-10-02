import { useSyncExternalStore } from "react";
import type { BackgroundImage, BackgroundImageData } from "../types";
import { hasBackendRuntime, invoke } from "./backend";

/**
 * Imported window backgrounds: a picked picture cut into a ladder of sizes, kept by
 * the host in a library the user manages, and painted at the size the window needs.
 *
 * The browser engine decodes the file because it knows every format the platform
 * does — HEIC on macOS, AVIF on Windows — and the host's decoders know four. Resizing
 * is done here rather than by `drawImage`, whose filter differs between WebKit and
 * Chromium: an exact area average in linear light, the same arithmetic on both, and
 * the one that neither aliases fine detail nor darkens it. Nothing is invented; a
 * picture smaller than the window is shown at the size it is.
 */

/** Long-edge sizes of the ladder. The top is where a 6K window stops needing more. */
const BACKGROUND_LADDER = [640, 1280, 1920, 2560, 3840, 5120, 7680] as const;
const MAX_LONG_EDGE = BACKGROUND_LADDER[BACKGROUND_LADDER.length - 1];
/**
 * Past this the full-size decode is not read back pixel by pixel; the engine shrinks it
 * first. A 48-megapixel camera frame still gets the area average all the way down.
 */
const MAX_READBACK_PIXELS = 48 * 1024 * 1024;
const MAX_CANVAS_SIDE = 16_384;
const JPEG_QUALITY = 0.9;

export interface Pixels {
  width: number;
  height: number;
  /** RGBA, row-major, not premultiplied — the layout of `ImageData`. */
  data: Uint8ClampedArray<ArrayBuffer>;
}

export interface EncodedTier {
  width: number;
  height: number;
  bytes: Uint8Array;
}

/** Sizes of every tier for a picture of this size, largest first. */
export function backgroundLadder(width: number, height: number): Array<{ width: number; height: number }> {
  const longEdge = Math.max(width, height);
  const topScale = Math.min(1, MAX_LONG_EDGE / longEdge);
  const top = {
    width: Math.max(1, Math.round(width * topScale)),
    height: Math.max(1, Math.round(height * topScale))
  };
  const topLong = Math.max(top.width, top.height);
  const sizes = [top];
  for (let index = BACKGROUND_LADDER.length - 1; index >= 0; index -= 1) {
    const edge = BACKGROUND_LADDER[index];
    // A rung within a tenth of the one above it would be a second copy of it.
    if (edge * 1.1 > topLong) continue;
    const scale = edge / longEdge;
    sizes.push({
      width: Math.max(1, Math.round(width * scale)),
      height: Math.max(1, Math.round(height * scale))
    });
  }
  return sizes;
}

const SRGB_TO_LINEAR = new Float32Array(256);
for (let value = 0; value < 256; value += 1) {
  const channel = value / 255;
  SRGB_TO_LINEAR[value] = channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}
const LINEAR_STEPS = 16_383;
const LINEAR_TO_SRGB = new Uint8Array(LINEAR_STEPS + 1);
for (let step = 0; step <= LINEAR_STEPS; step += 1) {
  const linear = step / LINEAR_STEPS;
  const channel = linear <= 0.0031308 ? linear * 12.92 : 1.055 * linear ** (1 / 2.4) - 0.055;
  LINEAR_TO_SRGB[step] = Math.round(Math.min(1, Math.max(0, channel)) * 255);
}

/**
 * For each output cell along one axis, the source cells it covers and how much of each.
 * Weights of one output cell sum to 1.
 */
function coverage(source: number, target: number): { first: Int32Array; count: Int32Array; weights: Float32Array } {
  const scale = source / target;
  const first = new Int32Array(target);
  const count = new Int32Array(target);
  const weights: number[] = [];
  for (let cell = 0; cell < target; cell += 1) {
    const start = cell * scale;
    const end = Math.min(source, (cell + 1) * scale);
    const from = Math.floor(start);
    const to = Math.min(source, Math.ceil(end));
    first[cell] = weights.length;
    for (let index = from; index < to; index += 1) {
      weights.push(index, (Math.min(index + 1, end) - Math.max(index, start)) / scale);
    }
    count[cell] = to - from;
  }
  return { first, count, weights: Float32Array.from(weights) };
}

/**
 * Lets the window paint and take input between stretches of a long resize. A message,
 * not a timer: timers in a hidden or occluded window are throttled to about one a
 * second, which turned a four-second import into half a minute when the user looked away.
 */
function yieldToWindow(): Promise<void> {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
}

/**
 * Shrinks `source` to `width`×`height` by averaging exactly the source area under each
 * output pixel. Averaging happens on linear-light, alpha-premultiplied values, so a
 * fine black-and-white pattern becomes the grey it looks like rather than a darker one,
 * and a transparent pixel's hidden colour does not bleed into its neighbours.
 */
export async function areaResize(source: Pixels, width: number, height: number): Promise<Pixels> {
  if (width > source.width || height > source.height) {
    throw new Error("areaResize only shrinks");
  }
  const columns = coverage(source.width, width);
  const rows = coverage(source.height, height);
  const output = new Uint8ClampedArray(width * height * 4);
  const accumulator = new Float32Array(width * 4);
  const linearRow = new Float32Array(source.width * 4);
  const filteredRow = new Float32Array(width * 4);
  let filteredIndex = -1;
  let sliceStart = performance.now();

  const filterSourceRow = (row: number): void => {
    if (filteredIndex === row) return;
    const data = source.data;
    let offset = row * source.width * 4;
    for (let index = 0; index < source.width * 4; index += 4, offset += 4) {
      const alpha = data[offset + 3] / 255;
      linearRow[index] = SRGB_TO_LINEAR[data[offset]] * alpha;
      linearRow[index + 1] = SRGB_TO_LINEAR[data[offset + 1]] * alpha;
      linearRow[index + 2] = SRGB_TO_LINEAR[data[offset + 2]] * alpha;
      linearRow[index + 3] = alpha;
    }
    for (let cell = 0; cell < width; cell += 1) {
      let red = 0;
      let green = 0;
      let blue = 0;
      let alpha = 0;
      const base = columns.first[cell];
      for (let item = 0; item < columns.count[cell]; item += 1) {
        const at = columns.weights[base + item * 2] * 4;
        const weight = columns.weights[base + item * 2 + 1];
        red += linearRow[at] * weight;
        green += linearRow[at + 1] * weight;
        blue += linearRow[at + 2] * weight;
        alpha += linearRow[at + 3] * weight;
      }
      filteredRow[cell * 4] = red;
      filteredRow[cell * 4 + 1] = green;
      filteredRow[cell * 4 + 2] = blue;
      filteredRow[cell * 4 + 3] = alpha;
    }
    filteredIndex = row;
  };

  for (let y = 0; y < height; y += 1) {
    accumulator.fill(0);
    const base = rows.first[y];
    for (let item = 0; item < rows.count[y]; item += 1) {
      filterSourceRow(rows.weights[base + item * 2]);
      const weight = rows.weights[base + item * 2 + 1];
      for (let index = 0; index < accumulator.length; index += 1) {
        accumulator[index] += filteredRow[index] * weight;
      }
    }
    let offset = y * width * 4;
    for (let index = 0; index < accumulator.length; index += 4, offset += 4) {
      const alpha = accumulator[index + 3];
      if (alpha <= 0) continue;
      const unpremultiply = LINEAR_STEPS / alpha;
      output[offset] = LINEAR_TO_SRGB[Math.min(LINEAR_STEPS, Math.round(accumulator[index] * unpremultiply))];
      output[offset + 1] = LINEAR_TO_SRGB[Math.min(LINEAR_STEPS, Math.round(accumulator[index + 1] * unpremultiply))];
      output[offset + 2] = LINEAR_TO_SRGB[Math.min(LINEAR_STEPS, Math.round(accumulator[index + 2] * unpremultiply))];
      output[offset + 3] = Math.round(Math.min(1, alpha) * 255);
    }
    if (performance.now() - sliceStart > 24) {
      await yieldToWindow();
      sliceStart = performance.now();
    }
  }
  return { width, height, data: output };
}

function isOpaque(pixels: Pixels): boolean {
  for (let index = 3; index < pixels.data.length; index += 4) {
    if (pixels.data[index] !== 255) return false;
  }
  return true;
}

/** Gives a canvas's backing store back now rather than whenever it is collected. */
function release(canvas: HTMLCanvasElement): void {
  canvas.width = 0;
  canvas.height = 0;
}

function canvasOf(width: number, height: number): { canvas: HTMLCanvasElement; context: CanvasRenderingContext2D } {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) throw new Error("canvas 2D context is unavailable");
  return { canvas, context };
}

/** The picture at no more than the ladder's top size, as pixels. */
async function readSource(file: Blob): Promise<Pixels> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  } catch {
    try {
      // WebKit before Safari 17 knows only "none" and "flipY" and rejects the option;
      // without it, it already follows the file's EXIF orientation.
      bitmap = await createImageBitmap(file);
    } catch {
      throw new Error("unreadable");
    }
  }
  try {
    const { width, height } = bitmap;
    const [top] = backgroundLadder(width, height);
    // A picture too big to read back whole is shrunk by the engine only as far as the
    // read-back limit, never below the top tier, so the area average still makes the
    // step down to it.
    const readScale = Math.min(
      1,
      Math.max(
        top.width / width,
        Math.min(Math.sqrt(MAX_READBACK_PIXELS / (width * height)), MAX_CANVAS_SIDE / Math.max(width, height))
      )
    );
    const readWidth = Math.max(top.width, Math.round(width * readScale));
    const readHeight = Math.max(top.height, Math.round(height * readScale));
    const { canvas, context } = canvasOf(readWidth, readHeight);
    context.imageSmoothingEnabled = true;
    context.imageSmoothingQuality = "high";
    context.drawImage(bitmap, 0, 0, readWidth, readHeight);
    const image = context.getImageData(0, 0, readWidth, readHeight);
    release(canvas);
    const pixels = { width: readWidth, height: readHeight, data: image.data };
    return readWidth === top.width && readHeight === top.height
      ? pixels
      : areaResize(pixels, top.width, top.height);
  } finally {
    bitmap.close();
  }
}

async function encode(pixels: Pixels, opaque: boolean): Promise<Uint8Array> {
  const { canvas, context } = canvasOf(pixels.width, pixels.height);
  context.putImageData(new ImageData(pixels.data, pixels.width, pixels.height), 0, 0);
  const blob = await new Promise<Blob | null>((resolve) =>
    canvas.toBlob(resolve, opaque ? "image/jpeg" : "image/png", JPEG_QUALITY)
  );
  release(canvas);
  if (!blob) throw new Error("encode");
  return new Uint8Array(await blob.arrayBuffer());
}

/** Every tier of a picked file, smallest first. Each is shrunk from the one above it. */
async function buildBackgroundTiers(file: Blob): Promise<EncodedTier[]> {
  const top = await readSource(file);
  const opaque = isOpaque(top);
  const ladder = backgroundLadder(top.width, top.height);
  const tiers: EncodedTier[] = [];
  let previous = top;
  for (const size of ladder) {
    const pixels = size.width === previous.width && size.height === previous.height
      ? previous
      : await areaResize(previous, size.width, size.height);
    tiers.push({ width: pixels.width, height: pixels.height, bytes: await encode(pixels, opaque) });
    previous = pixels;
    await yieldToWindow();
  }
  return tiers.reverse();
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk));
  }
  return btoa(binary);
}

/* The browser preview has no host; it keeps imported pictures for the life of the page. */
interface PreviewTier {
  width: number;
  height: number;
  dataUrl: string;
}
const previewUploads = new Map<string, PreviewTier[]>();
const previewImages = new Map<string, PreviewTier[]>();

function tierMime(bytes: Uint8Array): string {
  return bytes[0] === 0x89 ? "image/png" : "image/jpeg";
}

async function previewImageId(tiers: PreviewTier[]): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(tiers.map((tier) => `${tier.width}x${tier.height}:${tier.dataUrl}`).join("\n"))
  );
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function putTier(uploadId: string | null, tier: EncodedTier): Promise<string> {
  const data = bytesToBase64(tier.bytes);
  if (hasBackendRuntime()) {
    return invoke<string>("background_image_put", { uploadId, data });
  }
  const id = uploadId ?? crypto.randomUUID().replaceAll("-", "");
  const tiers = previewUploads.get(id) ?? [];
  tiers.push({ width: tier.width, height: tier.height, dataUrl: `data:${tierMime(tier.bytes)};base64,${data}` });
  previewUploads.set(id, tiers);
  return id;
}

async function commitUpload(uploadId: string): Promise<BackgroundImage> {
  if (hasBackendRuntime()) {
    return invoke<BackgroundImage>("background_image_commit", { uploadId });
  }
  const tiers = (previewUploads.get(uploadId) ?? []).sort((left, right) => left.width - right.width);
  previewUploads.delete(uploadId);
  const largest = tiers.at(-1);
  if (!largest) throw new Error("empty upload");
  const id = await previewImageId(tiers);
  previewImages.set(id, tiers);
  return { id, width: largest.width, height: largest.height };
}

/*
 * Ids are content hashes, so importing the same picture again — after its files went
 * missing, say — gives back the id the settings already hold and changes nothing a
 * reader would notice. Every import and removal therefore also bumps this count, and
 * whatever lists or shows the pictures reads again when it moves.
 */
let libraryGeneration = 0;
const libraryListeners = new Set<() => void>();

function subscribeLibrary(listener: () => void): () => void {
  libraryListeners.add(listener);
  return () => {
    libraryListeners.delete(listener);
  };
}

function libraryChanged(): void {
  libraryGeneration += 1;
  for (const listener of libraryListeners) listener();
}

/** Changes after every import or removal; a dependency for anything that lists or shows pictures. */
export function useBackgroundLibraryGeneration(): number {
  return useSyncExternalStore(subscribeLibrary, () => libraryGeneration, () => libraryGeneration);
}

/** Decodes, resizes and stores a picked picture; the result's id goes into the appearance settings. */
export async function importBackgroundImage(file: Blob): Promise<BackgroundImage> {
  const tiers = await buildBackgroundTiers(file);
  let uploadId: string | null = null;
  for (const tier of tiers) {
    uploadId = await putTier(uploadId, tier);
  }
  if (!uploadId) throw new Error("empty upload");
  const image = await commitUpload(uploadId);
  libraryChanged();
  return image;
}

/** The imported pictures, the most recent first. */
export async function listBackgroundImages(): Promise<BackgroundImage[]> {
  if (hasBackendRuntime()) return invoke<BackgroundImage[]>("background_image_list");
  return [...previewImages].reverse().map(([id, tiers]) => {
    const largest = tiers[tiers.length - 1];
    return { id, width: largest.width, height: largest.height };
  });
}

/** Removes an imported picture from the library; the settings must stop pointing at it. */
export async function deleteBackgroundImage(imageId: string): Promise<void> {
  if (hasBackendRuntime()) {
    await invoke("background_image_delete", { imageId });
  } else {
    previewImages.delete(imageId);
  }
  libraryChanged();
}

/** The tier of a stored picture that covers `width`×`height` device pixels, or its largest. */
export async function backgroundImageData(
  imageId: string,
  width: number,
  height: number
): Promise<BackgroundImageData> {
  const viewportWidth = Math.max(1, Math.min(32_768, Math.round(width)));
  const viewportHeight = Math.max(1, Math.min(32_768, Math.round(height)));
  if (hasBackendRuntime()) {
    return invoke<BackgroundImageData>("background_image_data", {
      imageId,
      width: viewportWidth,
      height: viewportHeight
    });
  }
  const tiers = previewImages.get(imageId);
  if (!tiers?.length) throw new Error("背景图片不存在");
  const index = tiers.findIndex((tier) => tier.width >= viewportWidth && tier.height >= viewportHeight);
  const chosen = index < 0 ? tiers.length - 1 : index;
  const tier = tiers[chosen];
  return { dataUrl: tier.dataUrl, width: tier.width, height: tier.height, largest: chosen === tiers.length - 1 };
}

/** Whether a tier already covers a window of this many device pixels. */
export function tierCovers(tier: { width: number; height: number }, width: number, height: number): boolean {
  return tier.width >= Math.round(width) && tier.height >= Math.round(height);
}
