import { describe, expect, it } from "vitest";
import {
  appendImagePlaceholder,
  imagePlaceholderIds,
  imageShortIdsInUse,
  nextImageShortId,
  renumberImagesForSend,
  stripImagePlaceholder,
  textWithoutImagePlaceholders
} from "./imageShortIds";
import type { ContextItem, ImageAttachment } from "../types";

function image(shortId?: number): ImageAttachment {
  return {
    id: "a".repeat(64),
    name: "pixel.png",
    mime: "image/png",
    width: 1,
    height: 1,
    bytes: 68,
    ...(shortId !== undefined ? { shortId } : {})
  };
}

describe("imagePlaceholderIds", () => {
  it("finds every well-formed placeholder", () => {
    expect(imagePlaceholderIds("see [Image #1] and [Image #23]")).toEqual([1, 23]);
  });

  it("rejects zero, overlong digits, and malformed brackets", () => {
    expect(imagePlaceholderIds("[Image #0]")).toEqual([]);
    expect(imagePlaceholderIds("[Image #1234567890]")).toEqual([]);
    expect(imagePlaceholderIds("[Image #2")).toEqual([]);
    expect(imagePlaceholderIds("[Image # 3]")).toEqual([]);
    expect(imagePlaceholderIds("[image #3]")).toEqual([]);
  });
});

describe("imageShortIdsInUse", () => {
  it("collects user image ids, user text placeholders, and tool image ids", () => {
    const contexts: ContextItem[] = [
      {
        id: "u1",
        kind: "user",
        content: "kept text [Image #7] after its image was removed",
        createdAt: "2026-08-06T00:00:00Z",
        images: [image(2)]
      },
      {
        id: "t1",
        kind: "tool",
        toolName: "playwright",
        input: { action: "screenshot" },
        createdAt: "2026-08-06T00:00:00Z",
        result: {
          success: true,
          output: "{}",
          images: [image(4)],
          executedAt: "2026-08-06T00:00:00Z",
          durationMs: 1
        }
      },
      {
        id: "a1",
        kind: "assistant",
        content: "[Image #9] mentioned by the model does not reserve a number",
        createdAt: "2026-08-06T00:00:00Z"
      }
    ];
    expect([...imageShortIdsInUse(contexts)].sort((a, b) => a - b)).toEqual([2, 4, 7]);
  });
});

describe("nextImageShortId", () => {
  it("starts at 1 and always goes above the maximum", () => {
    expect(nextImageShortId(new Set())).toBe(1);
    expect(nextImageShortId(new Set([3, 7]))).toBe(8);
  });
});

describe("appendImagePlaceholder", () => {
  it("appends with a separating space only when needed", () => {
    expect(appendImagePlaceholder("", 1)).toBe("[Image #1]");
    expect(appendImagePlaceholder("hello", 2)).toBe("hello [Image #2]");
    expect(appendImagePlaceholder("hello ", 3)).toBe("hello [Image #3]");
    expect(appendImagePlaceholder("line\n", 4)).toBe("line\n[Image #4]");
  });
});

describe("textWithoutImagePlaceholders", () => {
  it("drops placeholders and collapses the leftover whitespace", () => {
    expect(textWithoutImagePlaceholders("[Image #1]保存失败也不能丢")).toBe("保存失败也不能丢");
    expect(textWithoutImagePlaceholders("a [Image #2] b")).toBe("a b");
    expect(textWithoutImagePlaceholders("[Image #1] [Image #2]")).toBe("");
    expect(textWithoutImagePlaceholders("plain text")).toBe("plain text");
  });
});

describe("stripImagePlaceholder", () => {
  it("removes the placeholder and the space the append added", () => {
    expect(stripImagePlaceholder("hello [Image #2]", 2)).toBe("hello");
    expect(stripImagePlaceholder("[Image #1] hello", 1)).toBe("hello");
    expect(stripImagePlaceholder("a [Image #3] b", 3)).toBe("a b");
    expect(stripImagePlaceholder("[Image #4]", 4)).toBe("");
  });

  it("leaves other placeholders and surrounding text alone", () => {
    expect(stripImagePlaceholder("a [Image #1] b [Image #2]", 2)).toBe("a [Image #1] b");
    expect(stripImagePlaceholder("untouched", 5)).toBe("untouched");
  });

  it("removes every occurrence of the id", () => {
    expect(stripImagePlaceholder("[Image #6] mid [Image #6]", 6)).toBe("mid");
  });
});

describe("renumberImagesForSend", () => {
  it("keeps non-colliding numbers untouched", () => {
    const result = renumberImagesForSend(new Set([1, 2]), "a [Image #3]", [image(3)]);
    expect(result.content).toBe("a [Image #3]");
    expect(result.images[0].shortId).toBe(3);
  });

  it("renumbers collisions and rewrites their placeholders", () => {
    const result = renumberImagesForSend(new Set([1, 2, 3]), "see [Image #2]", [image(2)]);
    expect(result.content).toBe("see [Image #4]");
    expect(result.images[0].shortId).toBe(4);
  });

  it("reserves keepers before choosing fresh numbers", () => {
    // A(#3) collides with the transcript, B(#5) does not. The fresh number
    // for A must skip B's 5; a single-pass counter would hand A the 5 and
    // then corrupt A's rewritten placeholder while renumbering B.
    const result = renumberImagesForSend(
      new Set([1, 2, 3, 4]),
      "first [Image #3] second [Image #5]",
      [image(3), image(5)]
    );
    expect(result.content).toBe("first [Image #6] second [Image #5]");
    expect(result.images.map((entry) => entry.shortId)).toEqual([6, 5]);
  });

  it("assigns fresh numbers to unnumbered images without touching text", () => {
    const result = renumberImagesForSend(new Set([2]), "plain", [image()]);
    expect(result.content).toBe("plain");
    expect(result.images[0].shortId).toBe(3);
  });
});
