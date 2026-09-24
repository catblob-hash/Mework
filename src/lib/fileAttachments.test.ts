import { beforeEach, describe, expect, it, vi } from "vitest";
import type { FileAttachment, ImageAttachment } from "../types";
import {
  decodeAttachmentText,
  dragItemFromMediaType,
  dragItemFromProbe,
  intakeAttachments,
  isLongPaste,
  mergeFileAttachments,
  pastedTextFile,
  rejectionsForDragItems,
  sniffAttachmentBytes,
  summarizeDrag
} from "./fileAttachments";
import { MAX_FILE_ATTACHMENT_TEXT_BYTES, MAX_MESSAGE_FILE_TOKENS, MAX_MESSAGE_FILES } from "./fileBudget";

const mocks = vi.hoisted(() => ({
  prepareFileAttachment: vi.fn(),
  extractPdfText: vi.fn()
}));

vi.mock("./runtime", () => ({ prepareFileAttachment: mocks.prepareFileAttachment }));
vi.mock("./pdfDocument", async (importOriginal) => ({
  ...await importOriginal<typeof import("./pdfDocument")>(),
  extractPdfText: mocks.extractPdfText
}));

const { PdfWithoutTextError, PdfPasswordError } = await import("./pdfDocument");

const bytes = (...values: number[]) => new Uint8Array(values);
const utf8 = (text: string) => new TextEncoder().encode(text);

function stored(name: string, id = name): FileAttachment {
  return { id: id.padEnd(64, "0"), name, format: "text", bytes: 10, tokens: 3 };
}

describe("sniffAttachmentBytes", () => {
  it("tells pictures, PDFs and text apart by their bytes", () => {
    expect(sniffAttachmentBytes(bytes(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0))).toBe("image");
    expect(sniffAttachmentBytes(bytes(0xff, 0xd8, 0xff, 0xe0))).toBe("image");
    expect(sniffAttachmentBytes(utf8("GIF89a..."))).toBe("image");
    expect(sniffAttachmentBytes(utf8("RIFF\u0000\u0000\u0000\u0000WEBPVP8 "))).toBe("image");
    expect(sniffAttachmentBytes(utf8("%PDF-1.7\n%âãÏÓ"))).toBe("pdf");
    // The header may sit after some junk, within the first KiB.
    expect(sniffAttachmentBytes(utf8(`${" ".repeat(100)}%PDF-1.4`))).toBe("pdf");
    expect(sniffAttachmentBytes(utf8("export const answer = 42;\n"))).toBe("text");
    expect(sniffAttachmentBytes(utf8("中文说明"))).toBe("text");
  });

  it("calls a NUL or broken UTF-8 binary, and nothing at all empty", () => {
    expect(sniffAttachmentBytes(bytes(0x50, 0x4b, 0x03, 0x04, 0x00))).toBe("binary");
    expect(sniffAttachmentBytes(bytes(0xc3, 0x28))).toBe("binary");
    expect(sniffAttachmentBytes(new Uint8Array())).toBe("empty");
  });
});

describe("decodeAttachmentText", () => {
  it("drops a UTF-8 byte-order mark and reads UTF-16 only with its mark", () => {
    expect(decodeAttachmentText(bytes(0xef, 0xbb, 0xbf, 0x68, 0x69))).toBe("hi");
    expect(decodeAttachmentText(bytes(0xff, 0xfe, 0x68, 0x00, 0x69, 0x00))).toBe("hi");
    expect(decodeAttachmentText(bytes(0xfe, 0xff, 0x00, 0x68, 0x00, 0x69))).toBe("hi");
    // Without a mark UTF-16 is full of NULs, which is what binary looks like.
    expect(decodeAttachmentText(bytes(0x68, 0x00, 0x69, 0x00))).toBeNull();
  });
});

describe("drag verdicts", () => {
  it("takes the host's word on folders, sizes and what a file starts with", () => {
    expect(dragItemFromProbe({ path: "/a", name: "a", kind: "directory", size: 0, sniff: "none" }).verdict).toBe("directory");
    expect(dragItemFromProbe({ path: "/b.md", name: "b.md", kind: "file", size: 10, sniff: "text" }).verdict).toBe("text");
    expect(dragItemFromProbe({
      path: "/big.log",
      name: "big.log",
      kind: "file",
      size: MAX_FILE_ATTACHMENT_TEXT_BYTES + 1,
      sniff: "text"
    }).verdict).toBe("tooLarge");
    expect(dragItemFromProbe({ path: "/c.zip", name: "c.zip", kind: "file", size: 10, sniff: "binary" }).verdict).toBe("unsupported");
    expect(dragItemFromProbe({ path: "/d", name: "d", kind: "file", size: 0, sniff: "empty" }).verdict).toBe("empty");
  });

  it("reads what it can from a browser's media types and waits on the rest", () => {
    expect(dragItemFromMediaType("image/png").verdict).toBe("image");
    expect(dragItemFromMediaType("application/pdf").verdict).toBe("pdf");
    expect(dragItemFromMediaType("text/markdown").verdict).toBe("text");
    expect(dragItemFromMediaType("application/json").verdict).toBe("text");
    expect(dragItemFromMediaType("").verdict).toBe("unknown");
    expect(dragItemFromMediaType("video/mp2t").verdict).toBe("unknown");
    expect(dragItemFromMediaType("application/zip").verdict).toBe("unsupported");
  });

  it("counts a picture as refused where the model has no image input", () => {
    const items = [
      { name: "a.png", verdict: "image" as const },
      { name: "b.md", verdict: "text" as const },
      { name: "dir", verdict: "directory" as const },
      { name: "", verdict: "unknown" as const }
    ];
    expect(summarizeDrag(items, true)).toEqual({ accepted: 2, undecided: 1, rejected: [{ verdict: "directory", count: 1 }] });
    expect(summarizeDrag(items, false)).toEqual({
      accepted: 1,
      undecided: 1,
      rejected: [
        { verdict: "imageInputUnavailable", count: 1 },
        { verdict: "directory", count: 1 }
      ]
    });
    expect(rejectionsForDragItems(items, false)).toEqual([
      { name: "a.png", reason: "imageInputUnavailable" },
      { name: "dir", reason: "directory" }
    ]);
  });
});

describe("long pastes", () => {
  it("becomes a file past the character or line threshold", () => {
    expect(isLongPaste("a".repeat(4_999))).toBe(false);
    expect(isLongPaste("a".repeat(5_000))).toBe(true);
    expect(isLongPaste(Array.from({ length: 99 }, () => "x").join("\n"))).toBe(false);
    expect(isLongPaste(Array.from({ length: 100 }, () => "x").join("\n"))).toBe(true);
  });

  it("is named so it cannot collide with a file the message already carries", async () => {
    const first = pastedTextFile("one", []);
    expect(first.name).toBe("pasted-text.md");
    expect(first.type).toBe("text/markdown");
    expect(await first.text()).toBe("one");
    expect(pastedTextFile("two", [stored("pasted-text.md")]).name).toBe("pasted-text-2.md");
    expect(pastedTextFile("three", [stored("pasted-text.md"), stored("pasted-text-2.md")]).name)
      .toBe("pasted-text-3.md");
  });
});

describe("intakeAttachments", () => {
  beforeEach(() => {
    mocks.prepareFileAttachment.mockReset().mockImplementation(async (
      name: string,
      data: Uint8Array,
      format: "text" | "pdf",
      extracted?: { text: string; pages: number }
    ) => ({
      id: name.padEnd(64, "0"),
      name,
      format,
      bytes: data.byteLength,
      tokens: 1,
      ...(extracted ? { pages: extracted.pages } : {})
    }));
    mocks.extractPdfText.mockReset();
  });

  it("stores a text file as UTF-8 and a PDF with its text", async () => {
    mocks.extractPdfText.mockResolvedValue({ text: "[Page 1]\nhello", pages: 1 });
    const result = await intakeAttachments([
      new File([bytes(0xff, 0xfe, 0x68, 0x00, 0x69, 0x00)], "wide.txt"),
      new File([utf8("%PDF-1.7 body")], "paper.pdf", { type: "application/pdf" })
    ], { existingFiles: () => [] });

    expect(result.rejected).toEqual([]);
    expect(result.files.map((file) => file.name)).toEqual(["wide.txt", "paper.pdf"]);
    const [textCall, pdfCall] = mocks.prepareFileAttachment.mock.calls;
    expect(new TextDecoder().decode(textCall[1])).toBe("hi");
    expect(textCall[2]).toBe("text");
    expect(pdfCall[2]).toBe("pdf");
    expect(pdfCall[3]).toEqual({ text: "[Page 1]\nhello", pages: 1 });
  });

  it("gives each refused file its reason", async () => {
    mocks.extractPdfText
      .mockRejectedValueOnce(new PdfWithoutTextError("scan"))
      .mockRejectedValueOnce(new PdfPasswordError("locked"));
    const result = await intakeAttachments([
      new File([bytes(0x50, 0x4b, 0x03, 0x04, 0x00)], "bundle.zip"),
      new File([], "empty.txt"),
      new File([utf8("%PDF-1.4")], "scan.pdf"),
      new File([utf8("%PDF-1.4")], "locked.pdf"),
      new File([utf8("a".repeat(MAX_FILE_ATTACHMENT_TEXT_BYTES + 1))], "huge.log"),
      new File([bytes(1)], "photo.png", { type: "image/png" })
    ], { existingFiles: () => [], preRejected: [{ name: "folder", reason: "directory" }] });

    expect(result.files).toEqual([]);
    expect(result.rejected).toEqual([
      { name: "folder", reason: "directory" },
      { name: "bundle.zip", reason: "unsupported" },
      { name: "empty.txt", reason: "empty" },
      { name: "photo.png", reason: "imageInputUnavailable" },
      { name: "scan.pdf", reason: "pdfWithoutText" },
      { name: "locked.pdf", reason: "pdfPassword" },
      { name: "huge.log", reason: "tooLarge" }
    ]);
    expect(mocks.prepareFileAttachment).not.toHaveBeenCalled();
  });

  it("hands pictures to the image path and reports the ones it did not take", async () => {
    const accepted: ImageAttachment = { id: "i".repeat(64), name: "a.png", mime: "image/png", width: 1, height: 1, bytes: 1, shortId: 1 };
    const addImages = vi.fn().mockResolvedValue([accepted]);
    const result = await intakeAttachments([
      new File([bytes(1)], "a.png", { type: "image/png" }),
      new File([bytes(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)], "unlabelled")
    ], { addImages, existingFiles: () => [] });

    expect(addImages).toHaveBeenCalledWith([
      expect.objectContaining({ name: "a.png" }),
      expect.objectContaining({ name: "unlabelled" })
    ]);
    expect(result.images).toEqual([accepted]);
    expect(result.rejected).toEqual([{ reason: "imageRejected" }]);
  });

  it("stops at the per-message file limit and drops a file it already has", async () => {
    const existing = Array.from({ length: MAX_MESSAGE_FILES - 1 }, (_, index) => stored(`f${index}.txt`));
    const result = await intakeAttachments([
      new File([utf8("again")], "f0.txt"),
      new File([utf8("last")], "last.txt")
    ], { existingFiles: () => existing });

    // `f0.txt` fits by count but is the same file the message already carries.
    expect(result.files).toEqual([]);
    expect(result.rejected).toEqual([{ name: "last.txt", reason: "tooMany" }]);
    expect(mergeFileAttachments(existing, [stored("new.txt")])).toHaveLength(MAX_MESSAGE_FILES);
    expect(mergeFileAttachments([...existing, stored("new.txt")], [stored("more.txt")])).toHaveLength(MAX_MESSAGE_FILES);
  });

  it("keeps one message's file text within its budget, before uploading what would not fit", async () => {
    const heavy = { ...stored("heavy.txt"), tokens: MAX_MESSAGE_FILE_TOKENS - 10 };
    const result = await intakeAttachments([
      new File([utf8("tiny")], "tiny.txt"),
      new File([utf8("x".repeat(400))], "more.txt")
    ], { existingFiles: () => [heavy] });

    expect(result.files.map((file) => file.name)).toEqual(["tiny.txt"]);
    expect(result.rejected).toEqual([{ name: "more.txt", reason: "overBudget" }]);
    expect(mocks.prepareFileAttachment).toHaveBeenCalledTimes(1);
  });
});
