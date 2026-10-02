import type { ContextItem } from "../types";
import { MEMORY_TOOL_NAME_SET } from "./memoryTools";

/**
 * Image limits, mirroring Rust `image_attachments.rs`.
 *
 * A picture the user attaches is shrunk by the host the way Claude Code shrinks
 * a prompt image — at most 2000 px a side, then at most 500 KB where a JPEG can
 * get it there (a picture with transparency stays a PNG) — so what is stored is
 * far smaller than the file that was picked.
 *
 * These bound one image. How many a message or a request carries, and what
 * they come to together, is not budgeted.
 */

/**
 * Largest image *file* handed to the host (Rust `MAX_IMAGE_UPLOAD_BYTES`).
 * Claude Code sets no bound; this one keeps a decode within memory.
 */
export const MAX_IMAGE_ATTACHMENT_BYTES = 32 * 1024 * 1024;
/** Largest stored image, in pixels (Rust `MAX_IMAGE_ATTACHMENT_PIXELS`); a tool's screenshot may be this large. */
export const MAX_IMAGE_ATTACHMENT_PIXELS = 16 * 1024 * 1024;

export function contextsContainProjectedImages(contexts: ContextItem[]): boolean {
  return contexts.some((item) => (
    (item.kind === "user" && Boolean(item.images?.length))
    || (
      item.kind === "tool"
      && !MEMORY_TOOL_NAME_SET.has(item.toolName)
      && Boolean(item.result.images?.length)
    )
  ));
}
