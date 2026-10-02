import { beforeEach, describe, expect, it, vi } from "vitest";
import type { FileAttachment, ImageAttachment } from "../types";
import {
  decodeAttachmentText,
  dragItemFromMediaType,
  dragItemFromProbe,
  intakeAttachments,
  mergeFileAttachments,
  rejectionsForDragItems,
  sniffAttachmentBytes,
  summarizeDrag,
  textForModel
} from "./fileAttachments";
import {
  MAX_FILE_ATTACHMENT_PDF_BYTES,
  MAX_MESSAGE_ATTACHMENT_BYTES,
  MAX_TEXT_FILE_TOKENS,
  MAX_TEXT_FILE_UPLOAD_BYTES
} from "./fileBudget";
import { estimateTokens } from "./contextTokens";
import { MAX_IMAGE_ATTACHMENT_BYTES } from "./imageBudget";

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
      size: MAX_TEXT_FILE_UPLOAD_BYTES + 1,
      sniff: "text"
    }).verdict).toBe("tooLarge");
    // Claude Code's limits: text up to 256 KB, a whole PDF up to 20 MB.
    expect(MAX_TEXT_FILE_UPLOAD_BYTES).toBe(262_144);
    expect(dragItemFromProbe({ path: "/t", name: "t", kind: "file", size: MAX_TEXT_FILE_UPLOAD_BYTES, sniff: "text" }).verdict)
      .toBe("text");
    expect(dragItemFromProbe({ path: "/p", name: "p", kind: "file", size: MAX_FILE_ATTACHMENT_PDF_BYTES, sniff: "pdf" }).verdict)
      .toBe("pdf");
    expect(dragItemFromProbe({ path: "/q", name: "q", kind: "file", size: MAX_FILE_ATTACHMENT_PDF_BYTES + 1, sniff: "pdf" }).verdict)
      .toBe("tooLarge");
    expect(dragItemFromProbe({ path: "/i", name: "i", kind: "file", size: MAX_IMAGE_ATTACHMENT_BYTES, sniff: "image" }).verdict)
      .toBe("image");
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

describe("textForModel", () => {
  it("sends a text within the token cap whole", () => {
    const text = Array.from({ length: 5_000 }, () => "z").join("\n");
    expect(textForModel(text)).toEqual({ text, truncated: false });
  });

  it("cuts a longer one to its first 2000 lines, as Claude Code's Read does", () => {
    // 3000 lines of 40 characters: 30 750 tokens, 20 500 in the first 2000.
    const lines = Array.from({ length: 3_000 }, (_, index) => `${String(index + 1).padStart(5, "0")}${"x".repeat(35)}`);
    const text = lines.join("\n");
    expect(estimateTokens(text)).toBeGreaterThan(MAX_TEXT_FILE_TOKENS);
    const read = textForModel(text);
    expect(read).toEqual({ text: lines.slice(0, 2_000).join("\n"), truncated: true });
  });

  it("gives up on one still over the cap in those lines", () => {
    expect(textForModel(Array.from({ length: 1_500 }, () => "y".repeat(80)).join("\n"))).toBeNull();
    expect(textForModel("w".repeat(120_000))).toBeNull();
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
    ], { existingFiles: () => [], existingImages: () => [] });

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
      new File([utf8("a\n".repeat(MAX_TEXT_FILE_UPLOAD_BYTES / 2 + 1))], "huge.log"),
      new File([bytes(1)], "photo.png", { type: "image/png" })
    ], { existingFiles: () => [], existingImages: () => [], preRejected: [{ name: "folder", reason: "directory" }] });

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

  it("sends a long text file as its first lines, and refuses one that is still too long", async () => {
    const lines = Array.from({ length: 3_000 }, () => "x".repeat(40));
    const result = await intakeAttachments([
      new File([utf8(lines.join("\n"))], "big.log"),
      new File([utf8(Array.from({ length: 1_500 }, () => "y".repeat(80)).join("\n"))], "wide.csv")
    ], { existingFiles: () => [], existingImages: () => [] });

    expect(result.files.map((file) => file.name)).toEqual(["big.log"]);
    expect(result.rejected).toEqual([{ name: "wide.csv", reason: "tooLong" }]);
  });

  it("turns away a picture or PDF too large to hand over, by name", async () => {
    const addImages = vi.fn().mockResolvedValue([]);
    const photo = new File([bytes(1)], "photo.jpg", { type: "image/jpeg" });
    Object.defineProperty(photo, "size", { value: MAX_IMAGE_ATTACHMENT_BYTES + 1 });
    const pdf = new File([utf8("%PDF-1.7 "), new Uint8Array(MAX_FILE_ATTACHMENT_PDF_BYTES)], "big.pdf");
    const result = await intakeAttachments([photo, pdf], { addImages, existingFiles: () => [], existingImages: () => [] });

    expect(addImages).not.toHaveBeenCalled();
    expect(result.rejected).toEqual([
      { name: "photo.jpg", reason: "tooLarge" },
      { name: "big.pdf", reason: "tooLarge" }
    ]);
    expect(mocks.extractPdfText).not.toHaveBeenCalled();
  });

  it("hands pictures to the image path and reports the ones it did not take", async () => {
    const accepted: ImageAttachment = { id: "i".repeat(64), name: "a.png", mime: "image/png", width: 1, height: 1, bytes: 1, shortId: 1 };
    const addImages = vi.fn().mockResolvedValue([accepted]);
    const result = await intakeAttachments([
      new File([bytes(1)], "a.png", { type: "image/png" }),
      new File([bytes(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)], "unlabelled")
    ], { addImages, existingFiles: () => [], existingImages: () => [] });

    expect(addImages).toHaveBeenCalledWith([
      expect.objectContaining({ name: "a.png" }),
      expect.objectContaining({ name: "unlabelled" })
    ]);
    expect(result.images).toEqual([accepted]);
    expect(result.rejected).toEqual([{ reason: "imageRejected" }]);
  });

  it("takes any number of files and drops a file it already has", async () => {
    const existing = Array.from({ length: 20 }, (_, index) => stored(`f${index}.txt`));
    const added = Array.from({ length: 25 }, (_, index) => new File([utf8(`n${index}`)], `n${index}.txt`));
    const result = await intakeAttachments([new File([utf8("again")], "f0.txt"), ...added], {
      existingFiles: () => existing, existingImages: () => []
    });

    // `f0.txt` is the same file the message already carries; how many is not budgeted.
    expect(result.files.map((file) => file.name)).toEqual(added.map((file) => file.name));
    expect(result.rejected).toEqual([]);
    expect(mergeFileAttachments(existing, result.files)).toHaveLength(45);
  });

  it("keeps one message's attachments within 32 MiB, images and files together, in the order they come", async () => {
    const MIB = 1024 * 1024;
    expect(MAX_MESSAGE_ATTACHMENT_BYTES).toBe(32 * MIB);
    const sized = (file: File, size: number) => {
      Object.defineProperty(file, "size", { value: size });
      return file;
    };
    const photoImage: ImageAttachment = { id: "p".repeat(64), name: "photo.png", mime: "image/png", width: 1, height: 1, bytes: 1, shortId: 1 };
    const addImages = vi.fn().mockResolvedValue([photoImage]);
    const photo = sized(new File([bytes(1)], "photo.png", { type: "image/png" }), 15 * MIB);
    const report = sized(new File([utf8("%PDF-1.7 ")], "report.pdf"), 10 * MIB);
    const notes = new File([utf8("notes")], "notes.txt");
    // 2 MiB of images and 10 MiB of files already on the message leave 20 MiB.
    const result = await intakeAttachments([photo, report, notes], {
      addImages,
      existingFiles: () => [{ ...stored("old.pdf"), format: "pdf", bytes: 10 * MIB, pages: 1 }],
      existingImages: () => [{ ...photoImage, id: "o".repeat(64), bytes: 2 * MIB }]
    });

    // The photo fits (5 MiB left), the report does not, and the small note still does.
    expect(addImages).toHaveBeenCalledWith([photo]);
    expect(result.files.map((file) => file.name)).toEqual(["notes.txt"]);
    expect(result.rejected).toEqual([{ name: "report.pdf", reason: "messageTooLarge" }]);
    expect(mocks.extractPdfText).not.toHaveBeenCalled();
  });

  it("takes a message's files however much text they come to", async () => {
    const heavy = { ...stored("heavy.txt"), tokens: 400_000 };
    const result = await intakeAttachments([
      new File([utf8("tiny")], "tiny.txt"),
      new File([utf8("x".repeat(400))], "more.txt")
    ], { existingFiles: () => [heavy], existingImages: () => [] });

    expect(result.files.map((file) => file.name)).toEqual(["tiny.txt", "more.txt"]);
    expect(result.rejected).toEqual([]);
  });
});
