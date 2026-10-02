import { MAX_IMAGE_ATTACHMENT_BYTES, MAX_IMAGE_ATTACHMENT_PIXELS } from "./imageBudget";
import { intakeAttachments, type AddMessageAttachments } from "./fileAttachments";
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
 * Only each image on its own is judged here; what the message's attachments
 * come to in all is the shared intake's to check, before these are read.
 *
 * Whatever is rejected is dropped silently, exactly as the composer drops it:
 * the paste is a gesture, not a request, and the box it happened in has no room
 * to explain itself.
 */
async function acceptPastedImages(
  files: File[],
  existing: readonly ImageAttachment[],
  taken: Set<number>
): Promise<ImageAttachment[]> {
  const acceptedFiles = files.filter((file) => file.size > 0 && file.size <= MAX_IMAGE_ATTACHMENT_BYTES);
  if (!acceptedFiles.length) return [];
  const results = await Promise.allSettled(acceptedFiles.map(async (file) => (
    prepareImageAttachment(file.name, new Uint8Array(await file.arrayBuffer()))
  )));
  const known = new Set(existing.map((image) => image.id));
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
    ) {
      continue;
    }
    known.add(image.id);
    const shortId = nextImageShortId(taken);
    taken.add(shortId);
    accepted.push({ ...image, shortId });
  }
  return accepted;
}

/**
 * The attach handler for a user message edited in place: images numbered
 * against `taken()` through {@link acceptPastedImages}, files through the
 * shared intake. `imageInput` false turns images away with a reason rather
 * than letting them land where the model could never read them.
 */
export function messageAttachmentAdder(imageInput: boolean, taken: () => Set<number>): AddMessageAttachments {
  return (files, existing, preRejected) => intakeAttachments(files, {
    addImages: imageInput
      ? (images) => acceptPastedImages(images, existing.images, taken())
      : undefined,
    existingFiles: () => existing.files,
    existingImages: () => existing.images,
    preRejected
  });
}
