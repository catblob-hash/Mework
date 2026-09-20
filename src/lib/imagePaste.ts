import {
  MAX_COMPOSER_IMAGE_BYTES,
  MAX_COMPOSER_IMAGE_PIXELS,
  MAX_COMPOSER_IMAGES,
  MAX_IMAGE_ATTACHMENT_BYTES,
  MAX_IMAGE_ATTACHMENT_PIXELS
} from "./imageBudget";
import { nextImageShortId } from "./imageShortIds";
import { prepareImageAttachment } from "./runtime";
import type { ImageAttachment } from "../types";

/**
 * Images pasted into a user message that is being written or edited in place.
 *
 * The composer has its own copy of these gates because it also serializes
 * batches and reports a loading state; what is shared is the judgement itself,
 * so a message written on a timeline can never carry an attachment the composer
 * would have refused. `taken` is the caller's view of which numbers are already
 * spoken for — a conversation's transcript and queue, or a template's own body —
 * and is extended in place so two pastes in a row cannot reissue a number.
 *
 * Whatever is rejected is dropped silently, exactly as the composer drops it:
 * the paste is a gesture, not a request, and the box it happened in has no room
 * to explain itself.
 */
export async function acceptPastedImages(
  files: File[],
  existing: readonly ImageAttachment[],
  taken: Set<number>
): Promise<ImageAttachment[]> {
  let selectedBytes = existing.reduce((total, image) => total + image.bytes, 0);
  const acceptedFiles: File[] = [];
  for (const file of files) {
    if (
      file.size <= 0
      || file.size > MAX_IMAGE_ATTACHMENT_BYTES
      || existing.length + acceptedFiles.length >= MAX_COMPOSER_IMAGES
      || selectedBytes + file.size > MAX_COMPOSER_IMAGE_BYTES
    ) {
      continue;
    }
    acceptedFiles.push(file);
    selectedBytes += file.size;
  }
  if (!acceptedFiles.length) return [];
  const results = await Promise.allSettled(acceptedFiles.map(async (file) => (
    prepareImageAttachment(file.name, new Uint8Array(await file.arrayBuffer()))
  )));
  const known = new Set(existing.map((image) => image.id));
  let selectedPixels = existing.reduce(
    (total, image) => total + image.width * image.height,
    0
  );
  for (const image of existing) {
    if (image.shortId !== undefined) taken.add(image.shortId);
  }
  const accepted: ImageAttachment[] = [];
  for (const result of results) {
    if (result.status !== "fulfilled") continue;
    const image = result.value;
    const pixels = image.width * image.height;
    if (
      known.has(image.id)
      || !Number.isSafeInteger(pixels)
      || pixels <= 0
      || pixels > MAX_IMAGE_ATTACHMENT_PIXELS
      || selectedPixels + pixels > MAX_COMPOSER_IMAGE_PIXELS
    ) {
      continue;
    }
    known.add(image.id);
    selectedPixels += pixels;
    const shortId = nextImageShortId(taken);
    taken.add(shortId);
    accepted.push({ ...image, shortId });
  }
  return accepted;
}
