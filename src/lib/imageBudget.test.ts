import { describe, expect, it } from "vitest";
import type { ContextItem, ImageAttachment } from "../types";
import { contextsContainProjectedImages } from "./imageBudget";

function image(index: number): ImageAttachment {
  return {
    id: String(index).padStart(64, "0"),
    name: `${index}.png`,
    mime: "image/png",
    width: 1,
    height: 1,
    bytes: 1
  };
}

function tool(toolName: string, count: number): ContextItem {
  return {
    id: `tool-${toolName}`,
    kind: "tool",
    toolName,
    input: {},
    result: {
      success: true,
      output: "",
      images: Array.from({ length: count }, (_, index) => image(index)),
      executedAt: "2026-09-30T00:00:00Z",
      durationMs: 1
    },
    createdAt: "2026-09-30T00:00:00Z"
  } as ContextItem;
}

describe("contextsContainProjectedImages", () => {
  it("sees user and tool images, but not memory tools' own", () => {
    const user: ContextItem = { id: "user", kind: "user", content: "", images: [image(1)], createdAt: "2026-09-30T00:00:00Z" };
    expect(contextsContainProjectedImages([user])).toBe(true);
    expect(contextsContainProjectedImages([tool("preview_screenshot", 1)])).toBe(true);
    expect(contextsContainProjectedImages([tool("read_global_memory", 3)])).toBe(false);
    expect(contextsContainProjectedImages([{ ...user, images: [] }])).toBe(false);
  });
});
