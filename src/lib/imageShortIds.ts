import type { ContextItem, ImageAttachment } from "../types";

/**
 * Conversation image numbering, mirroring the Rust run loop's helpers in
 * api.rs (`image_placeholder_ids` / `image_short_ids_in_use` /
 * `renumber_user_message_images`). The composer assigns numbers eagerly so
 * the user sees the same `[Image #N]` placeholder the model will; the run
 * loop re-checks on delivery, so a stale draft number is corrected rather
 * than trusted.
 */

const PLACEHOLDER = /\[Image #([0-9]{1,9})\]/g;

export function imagePlaceholder(shortId: number): string {
  return `[Image #${shortId}]`;
}

/** Every valid `[Image #N]` id (N >= 1) literally present in the text. */
export function imagePlaceholderIds(text: string): number[] {
  const ids: number[] = [];
  for (const match of text.matchAll(PLACEHOLDER)) {
    const id = Number.parseInt(match[1], 10);
    if (id >= 1) ids.push(id);
  }
  return ids;
}

/**
 * Collects every image number already spoken for: stored shortIds on user
 * and tool images plus literal placeholders in user text. Placeholders count
 * even when their image is gone, so a number the model may still be reading
 * about is never reissued.
 */
export function imageShortIdsInUse(contexts: Iterable<ContextItem>): Set<number> {
  const taken = new Set<number>();
  for (const context of contexts) {
    if (context.kind === "user") {
      for (const image of context.images ?? []) {
        if (image.shortId !== undefined) taken.add(image.shortId);
      }
      for (const id of imagePlaceholderIds(context.content)) taken.add(id);
    } else if (context.kind === "tool") {
      for (const image of context.result.images ?? []) {
        if (image.shortId !== undefined) taken.add(image.shortId);
      }
    }
  }
  return taken;
}

/** Lowest fresh number strictly above everything taken. */
export function nextImageShortId(taken: ReadonlySet<number>): number {
  let max = 0;
  for (const id of taken) {
    if (id > max) max = id;
  }
  return max + 1;
}

/**
 * Queued messages are not transcript contexts yet, but their numbers are
 * already visible to the user, so allocation must treat them as taken.
 */
export function reserveQueuedMessageIds(
  taken: Set<number>,
  messages: Iterable<{ content: string; images?: ImageAttachment[] }>
): Set<number> {
  for (const message of messages) {
    for (const image of message.images ?? []) {
      if (image.shortId !== undefined) taken.add(image.shortId);
    }
    for (const id of imagePlaceholderIds(message.content)) taken.add(id);
  }
  return taken;
}

/** Appends ` [Image #N]` to the draft, without a leading space on empty text. */
export function appendImagePlaceholder(text: string, shortId: number): string {
  const placeholder = imagePlaceholder(shortId);
  if (!text) return placeholder;
  return text.endsWith(" ") || text.endsWith("\n") ? text + placeholder : `${text} ${placeholder}`;
}

/**
 * Human-facing fragment for title fallbacks: placeholders removed, whitespace
 * collapsed. A message that is nothing but placeholders yields "" so callers
 * can fall through to an image name instead.
 */
export function textWithoutImagePlaceholders(text: string): string {
  return text.replace(PLACEHOLDER, " ").replace(/\s+/g, " ").trim();
}

/**
 * Removes every literal `[Image #N]` occurrence for the given id, collapsing
 * the double space an appended placeholder leaves behind. Text the user wrote
 * around the placeholder is untouched.
 */
export function stripImagePlaceholder(text: string, shortId: number): string {
  const placeholder = imagePlaceholder(shortId);
  let result = "";
  let rest = text;
  for (let at = rest.indexOf(placeholder); at !== -1; at = rest.indexOf(placeholder)) {
    let before = rest.slice(0, at);
    let after = rest.slice(at + placeholder.length);
    if (before.endsWith(" ") && (after.startsWith(" ") || after.startsWith("\n") || !after)) {
      before = before.slice(0, -1);
    } else if (after.startsWith(" ") && !before) {
      after = after.slice(1);
    }
    result += before;
    rest = after;
  }
  return result + rest;
}

/**
 * Send-time collision repair, mirroring Rust `renumber_user_message_images`:
 * an image keeps its draft number when no transcript entry claims it;
 * a real collision gets a fresh number and its placeholder text is rewritten
 * to match. Keepers are reserved before any fresh number is chosen, so a
 * rewrite can never land on a number another draft image still holds.
 */
export function renumberImagesForSend(
  taken: Set<number>,
  content: string,
  images: ImageAttachment[]
): { content: string; images: ImageAttachment[] } {
  const keeps = images.map((image) => {
    const keep = image.shortId !== undefined && !taken.has(image.shortId);
    if (keep && image.shortId !== undefined) taken.add(image.shortId);
    return keep;
  });
  let text = content;
  const renumbered = images.map((image, index) => {
    if (keeps[index]) return image;
    const fresh = nextImageShortId(taken);
    taken.add(fresh);
    if (image.shortId !== undefined) {
      text = text.split(imagePlaceholder(image.shortId)).join(imagePlaceholder(fresh));
    }
    return { ...image, shortId: fresh };
  });
  return { content: text, images: renumbered };
}
