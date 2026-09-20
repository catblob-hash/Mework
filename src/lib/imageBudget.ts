import type { ContextItem } from "../types";
import { MEMORY_TOOL_NAME_SET } from "./memoryTools";

export const MAX_COMPOSER_IMAGES = 20;
export const MAX_IMAGE_ATTACHMENT_BYTES = 5 * 1024 * 1024;
export const MAX_IMAGE_ATTACHMENT_PIXELS = 16 * 1024 * 1024;
export const MAX_COMPOSER_IMAGE_BYTES = 20 * 1024 * 1024;
export const MAX_COMPOSER_IMAGE_PIXELS = 64 * 1024 * 1024;

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

export function projectedImageBudget(contexts: ContextItem[]): { count: number; bytes: number; pixels: number } {
  return contexts.reduce((budget, context) => {
    const images = context.kind === "user"
      ? context.images ?? []
      : context.kind === "tool"
          && !MEMORY_TOOL_NAME_SET.has(context.toolName)
        ? context.result.images ?? []
        : [];
    for (const image of images) {
      budget.count += 1;
      budget.bytes += Number.isFinite(image.bytes) && image.bytes >= 0
        ? image.bytes
        : MAX_COMPOSER_IMAGE_BYTES + 1;
      const pixels = Number.isFinite(image.width)
        && Number.isFinite(image.height)
        && image.width > 0
        && image.height > 0
        ? image.width * image.height
        : MAX_COMPOSER_IMAGE_PIXELS + 1;
      budget.pixels += Number.isSafeInteger(pixels) && pixels <= MAX_IMAGE_ATTACHMENT_PIXELS
        ? pixels
        : MAX_COMPOSER_IMAGE_PIXELS + 1;
    }
    return budget;
  }, { count: 0, bytes: 0, pixels: 0 });
}
