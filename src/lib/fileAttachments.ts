import type { FileAttachment, ImageAttachment } from "../types";
import { estimateTokens } from "./contextTokens";
import {
  MAX_FILE_ATTACHMENT_PDF_BYTES,
  MAX_FILE_ATTACHMENT_TEXT_BYTES,
  MAX_MESSAGE_ATTACHMENT_BYTES,
  MAX_TEXT_FILE_TOKENS,
  MAX_TEXT_FILE_UPLOAD_BYTES,
  TRUNCATED_TEXT_FILE_LINES
} from "./fileBudget";
import { MAX_IMAGE_ATTACHMENT_BYTES } from "./imageBudget";
import {
  extractPdfText,
  PdfPasswordError,
  PdfWithoutTextError,
  type ExtractedPdfText
} from "./pdfDocument";
import { prepareFileAttachment, type DroppedPathProbe } from "./runtime";

/**
 * Everything a user can attach to a message, as one gesture.
 *
 * A picked, pasted or dropped file is sorted by what its bytes are — never by
 * its name, which says nothing reliable about a `Makefile` or a `.ts` Windows
 * files under video — and goes one of three ways: an image joins the message's
 * images (the path images have always taken), a PDF is read for its text, and
 * a UTF-8 text file is kept as it is. Anything else is turned away with a
 * reason, because a model that reads text has no use for it.
 */

export type AttachmentKind = "image" | "pdf" | "text";

export type AttachmentRejectionReason =
  | "directory"
  | "empty"
  | "unsupported"
  | "tooLarge"
  /** A text file still over the token cap in its first lines. */
  | "tooLong"
  | "imageInputUnavailable"
  | "imageRejected"
  | "pdfWithoutText"
  | "pdfPassword"
  | "pdfUnreadable"
  /** Taking it would put the message's attachments past {@link MAX_MESSAGE_ATTACHMENT_BYTES}. */
  | "messageTooLarge"
  | "failed";

export interface AttachmentRejection {
  /** The file turned away; absent when only a count is known. */
  name?: string;
  reason: AttachmentRejectionReason;
}

export interface AttachmentIntakeResult {
  images: ImageAttachment[];
  files: FileAttachment[];
  rejected: AttachmentRejection[];
}

/** What the first bytes of a file say it is. */
export type ByteSniff = AttachmentKind | "binary" | "empty";

const IMAGE_SIGNATURES: readonly (readonly number[])[] = [
  [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
  [0xff, 0xd8, 0xff],
  [0x47, 0x49, 0x46, 0x38, 0x37, 0x61],
  [0x47, 0x49, 0x46, 0x38, 0x39, 0x61]
];

function startsWith(bytes: Uint8Array, signature: readonly number[], offset = 0): boolean {
  if (bytes.length < offset + signature.length) return false;
  return signature.every((byte, index) => bytes[offset + index] === byte);
}

function isImageBytes(bytes: Uint8Array): boolean {
  if (IMAGE_SIGNATURES.some((signature) => startsWith(bytes, signature))) return true;
  // RIFF....WEBP
  return startsWith(bytes, [0x52, 0x49, 0x46, 0x46]) && startsWith(bytes, [0x57, 0x45, 0x42, 0x50], 8);
}

/** `%PDF-` anywhere in the first KiB, which is where the format allows the header to be. */
function isPdfBytes(bytes: Uint8Array): boolean {
  const limit = Math.min(bytes.length, 1024) - 5;
  for (let index = 0; index <= limit; index += 1) {
    if (
      bytes[index] === 0x25 && bytes[index + 1] === 0x50 && bytes[index + 2] === 0x44
      && bytes[index + 3] === 0x46 && bytes[index + 4] === 0x2d
    ) return true;
  }
  return false;
}

/**
 * The file's text, or `null` when it is not text.
 *
 * UTF-8 (with or without a byte-order mark) is taken as it is; UTF-16 is taken
 * only with its byte-order mark, since without one it cannot be told from
 * binary. A NUL anywhere means binary, as it does to git.
 */
export function decodeAttachmentText(bytes: Uint8Array): string | null {
  let text: string;
  try {
    if (startsWith(bytes, [0xff, 0xfe])) text = new TextDecoder("utf-16le", { fatal: true }).decode(bytes.subarray(2));
    else if (startsWith(bytes, [0xfe, 0xff])) text = new TextDecoder("utf-16be", { fatal: true }).decode(bytes.subarray(2));
    else text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
  if (text.includes("\u0000")) return null;
  return text.startsWith("\uFEFF") ? text.slice(1) : text;
}

export function sniffAttachmentBytes(bytes: Uint8Array): ByteSniff {
  if (!bytes.length) return "empty";
  if (isImageBytes(bytes)) return "image";
  if (isPdfBytes(bytes)) return "pdf";
  return decodeAttachmentText(bytes) === null ? "binary" : "text";
}

/**
 * The verdict on one item of a drag still in the air, before anything is read.
 *
 * `unknown` is the browser's answer for a file whose type it cannot name —
 * the drop will read it and decide. Everything else is final.
 */
export type DragItemVerdict =
  | AttachmentKind
  | "unknown"
  | "directory"
  | "unsupported"
  | "empty"
  | "tooLarge";

export interface DragItem {
  name: string;
  verdict: DragItemVerdict;
  /** The native path, when the drag came from the operating system through the host. */
  path?: string;
}

function sizeFits(kind: AttachmentKind, size: number): boolean {
  return size <= (kind === "image"
    ? MAX_IMAGE_ATTACHMENT_BYTES
    : kind === "pdf" ? MAX_FILE_ATTACHMENT_PDF_BYTES : MAX_TEXT_FILE_UPLOAD_BYTES);
}

/** The largest file of any kind this intake reads before it knows the kind. */
const MAX_UNSNIFFED_BYTES = Math.max(MAX_IMAGE_ATTACHMENT_BYTES, MAX_FILE_ATTACHMENT_PDF_BYTES);

/**
 * What of a text file the model reads, by Claude Code's Read rule (mirrors Rust
 * `file_attachments::ModelText`): the whole text within
 * {@link MAX_TEXT_FILE_TOKENS}, else its first {@link TRUNCATED_TEXT_FILE_LINES}
 * lines when those are within it, else `null` — the file is not attached.
 */
export function textForModel(text: string): { text: string; truncated: boolean } | null {
  if (estimateTokens(text) <= MAX_TEXT_FILE_TOKENS) return { text, truncated: false };
  let end = -1;
  for (let line = 0; line < TRUNCATED_TEXT_FILE_LINES; line += 1) {
    end = text.indexOf("\n", end + 1);
    if (end < 0) return null;
  }
  const head = text.slice(0, end);
  return estimateTokens(head) <= MAX_TEXT_FILE_TOKENS ? { text: head, truncated: true } : null;
}

/** The host looked at the path itself: it knows a folder from a file, and what the file starts with. */
export function dragItemFromProbe(probe: DroppedPathProbe): DragItem {
  const base = { name: probe.name, path: probe.path };
  if (probe.kind === "directory") return { ...base, verdict: "directory" };
  if (probe.kind !== "file") return { ...base, verdict: "unsupported" };
  switch (probe.sniff) {
    case "image":
    case "pdf":
    case "text":
      return { ...base, verdict: sizeFits(probe.sniff, probe.size) ? probe.sniff : "tooLarge" };
    case "empty":
      return { ...base, verdict: "empty" };
    default:
      return { ...base, verdict: "unsupported" };
  }
}

const IMAGE_MEDIA_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

const TEXT_MEDIA_TYPES = new Set([
  "application/json",
  "application/ld+json",
  "application/xml",
  "application/javascript",
  "application/typescript",
  "application/x-sh",
  "application/x-httpd-php",
  "application/x-yaml",
  "application/yaml",
  "application/toml",
  "application/sql",
  "application/x-ipynb+json",
  "image/svg+xml"
]);

/**
 * A browser drag names each file's media type and nothing else — not its name,
 * not whether it is a folder. What the type rules out is ruled out; a missing
 * or unfamiliar type waits for the drop.
 */
export function dragItemFromMediaType(type: string): DragItem {
  const normalized = type.trim().toLowerCase();
  const name = "";
  if (IMAGE_MEDIA_TYPES.has(normalized)) return { name, verdict: "image" };
  if (normalized === "application/pdf") return { name, verdict: "pdf" };
  if (normalized.startsWith("text/") || TEXT_MEDIA_TYPES.has(normalized)) return { name, verdict: "text" };
  if (
    !normalized
    || normalized === "application/octet-stream"
    // Windows names TypeScript files after MPEG transport streams.
    || normalized === "video/mp2t"
  ) return { name, verdict: "unknown" };
  return { name, verdict: "unsupported" };
}

/** What a drop zone would make of a drag, given whether it can take images. */
export interface DragSummary {
  accepted: number;
  /** Items that might be accepted once read. */
  undecided: number;
  rejected: { verdict: Exclude<DragItemVerdict, AttachmentKind | "unknown"> | "imageInputUnavailable"; count: number }[];
}

export function summarizeDrag(items: readonly DragItem[], imageInput: boolean): DragSummary {
  let accepted = 0;
  let undecided = 0;
  const rejected = new Map<DragSummary["rejected"][number]["verdict"], number>();
  for (const item of items) {
    if (item.verdict === "image" && !imageInput) {
      rejected.set("imageInputUnavailable", (rejected.get("imageInputUnavailable") ?? 0) + 1);
    } else if (item.verdict === "image" || item.verdict === "pdf" || item.verdict === "text") {
      accepted += 1;
    } else if (item.verdict === "unknown") {
      undecided += 1;
    } else {
      rejected.set(item.verdict, (rejected.get(item.verdict) ?? 0) + 1);
    }
  }
  return {
    accepted,
    undecided,
    rejected: [...rejected].map(([verdict, count]) => ({ verdict, count }))
  };
}

/** Items a drop turns away before reading anything, as the rejections the notice lists. */
export function rejectionsForDragItems(items: readonly DragItem[], imageInput: boolean): AttachmentRejection[] {
  return items.flatMap((item): AttachmentRejection[] => {
    const name = item.name || undefined;
    switch (item.verdict) {
      case "image":
        return imageInput ? [] : [{ name, reason: "imageInputUnavailable" }];
      case "directory":
        return [{ name, reason: "directory" }];
      case "unsupported":
        return [{ name, reason: "unsupported" }];
      case "empty":
        return [{ name, reason: "empty" }];
      case "tooLarge":
        return [{ name, reason: "tooLarge" }];
      default:
        return [];
    }
  });
}

export interface AttachmentIntake {
  /**
   * Takes the image files and returns those it attached. Absent when the model
   * this message goes to cannot see images.
   */
  addImages?: (files: File[]) => Promise<ImageAttachment[]>;
  /** The files this message already carries, read when the batch is ready to land. */
  existingFiles: () => readonly FileAttachment[];
  /** The images this message already carries; with its files, they count against its size. */
  existingImages: () => readonly ImageAttachment[];
  /** Rejections decided before the files were read (a folder in a native drop). */
  preRejected?: readonly AttachmentRejection[];
}

/**
 * Admits files to one message while its attachments stay within
 * {@link MAX_MESSAGE_ATTACHMENT_BYTES}, in the order they are offered: one that
 * would go past it is turned away, and a smaller one after it may still fit.
 */
export function messageAttachmentRoom(
  images: readonly ImageAttachment[],
  files: readonly FileAttachment[]
): (file: File) => boolean {
  let room = MAX_MESSAGE_ATTACHMENT_BYTES
    - images.reduce((total, image) => total + image.bytes, 0)
    - files.reduce((total, file) => total + file.bytes, 0);
  return (file) => {
    if (file.size > room) return false;
    room -= file.size;
    return true;
  };
}

async function readFileBytes(file: File): Promise<Uint8Array> {
  return new Uint8Array(await file.arrayBuffer());
}

/** A document read and checked, ready to upload, with the tokens its text will cost. */
type ReadDocument =
  | {
      ok: true;
      name: string;
      bytes: Uint8Array;
      format: "pdf" | "text";
      extracted?: ExtractedPdfText;
      tokens: number;
    }
  | { ok: false; rejection: AttachmentRejection };

async function readDocument(file: File, bytes: Uint8Array, kind: "pdf" | "text"): Promise<ReadDocument> {
  const name = file.name.trim() || (kind === "pdf" ? "document.pdf" : "text.txt");
  try {
    if (kind === "text") {
      const text = decodeAttachmentText(bytes);
      if (text === null) return { ok: false, rejection: { name, reason: "unsupported" } };
      // UTF-16 is stored as the UTF-8 the model will read, so the host sees one encoding.
      const utf8 = bytes[0] === 0xff || bytes[0] === 0xfe ? new TextEncoder().encode(text) : bytes;
      if (utf8.byteLength > MAX_TEXT_FILE_UPLOAD_BYTES) return { ok: false, rejection: { name, reason: "tooLarge" } };
      if (!text.trim()) return { ok: false, rejection: { name, reason: "empty" } };
      // A long file is sent as its first lines; the budget counts what is sent.
      const read = textForModel(text);
      if (!read) return { ok: false, rejection: { name, reason: "tooLong" } };
      return { ok: true, name, bytes: utf8, format: "text", tokens: estimateTokens(read.text) };
    }
    let extracted: ExtractedPdfText;
    try {
      extracted = await extractPdfText(bytes);
    } catch (error) {
      if (error instanceof PdfWithoutTextError) return { ok: false, rejection: { name, reason: "pdfWithoutText" } };
      if (error instanceof PdfPasswordError) return { ok: false, rejection: { name, reason: "pdfPassword" } };
      return { ok: false, rejection: { name, reason: "pdfUnreadable" } };
    }
    if (new TextEncoder().encode(extracted.text).byteLength > MAX_FILE_ATTACHMENT_TEXT_BYTES) {
      return { ok: false, rejection: { name, reason: "tooLarge" } };
    }
    return { ok: true, name, bytes, format: "pdf", extracted, tokens: estimateTokens(extracted.text) };
  } catch {
    return { ok: false, rejection: { name, reason: "failed" } };
  }
}

/**
 * Attaches a batch of files to one message.
 *
 * Images keep their own numbering (`addImages`); files get the checks here.
 * Both count, in the order they come, against what one message's attachments
 * may come to. Files already on the message, and files that turn out to be the
 * same bytes as one of them, are not attached twice — a duplicate is not an
 * error worth a notice, so it is dropped without one.
 */
export async function intakeAttachments(
  files: readonly File[],
  intake: AttachmentIntake
): Promise<AttachmentIntakeResult> {
  const rejected: AttachmentRejection[] = [...(intake.preRejected ?? [])];
  const fitsMessage = messageAttachmentRoom(intake.existingImages(), intake.existingFiles());
  const imageFiles: File[] = [];
  const documents: { file: File; bytes: Uint8Array; kind: "pdf" | "text" }[] = [];
  for (const file of files) {
    const name = file.name || undefined;
    if (file.size <= 0) {
      rejected.push({ name, reason: "empty" });
      continue;
    }
    // A file that says it is a picture goes the way pictures always have; the
    // host decodes it and is the one to refuse it if it is not.
    if (IMAGE_MEDIA_TYPES.has(file.type.toLowerCase())) {
      if (!intake.addImages) rejected.push({ name, reason: "imageInputUnavailable" });
      else if (!sizeFits("image", file.size)) rejected.push({ name, reason: "tooLarge" });
      else if (!fitsMessage(file)) rejected.push({ name, reason: "messageTooLarge" });
      else imageFiles.push(file);
      continue;
    }
    if (file.size > MAX_UNSNIFFED_BYTES) {
      rejected.push({ name, reason: "tooLarge" });
      continue;
    }
    let bytes: Uint8Array;
    try {
      bytes = await readFileBytes(file);
    } catch {
      rejected.push({ name, reason: "failed" });
      continue;
    }
    const sniff = sniffAttachmentBytes(bytes);
    if (sniff === "image") {
      if (!intake.addImages) rejected.push({ name, reason: "imageInputUnavailable" });
      else if (!sizeFits("image", file.size)) rejected.push({ name, reason: "tooLarge" });
      else if (!fitsMessage(file)) rejected.push({ name, reason: "messageTooLarge" });
      else imageFiles.push(file);
    } else if (sniff === "pdf" && !sizeFits("pdf", file.size)) {
      rejected.push({ name, reason: "tooLarge" });
    } else if (sniff === "pdf" || sniff === "text") {
      if (fitsMessage(file)) documents.push({ file, bytes, kind: sniff });
      else rejected.push({ name, reason: "messageTooLarge" });
    } else {
      rejected.push({ name, reason: sniff === "empty" ? "empty" : "unsupported" });
    }
  }

  let images: ImageAttachment[] = [];
  if (imageFiles.length && intake.addImages) {
    images = await intake.addImages(imageFiles);
    // The image path drops what it cannot take without saying which; the count
    // is what is left to report.
    for (let missing = imageFiles.length - images.length; missing > 0; missing -= 1) {
      rejected.push({ reason: "imageRejected" });
    }
  }

  const read = await Promise.all(documents.map((entry) => readDocument(entry.file, entry.bytes, entry.kind)));
  const uploads: Extract<ReadDocument, { ok: true }>[] = [];
  for (const result of read) {
    if (!result.ok) rejected.push(result.rejection);
    else uploads.push(result);
  }
  const uploaded = await Promise.all(uploads.map(async (entry) => {
    try {
      return entry.extracted
        ? await prepareFileAttachment(entry.name, entry.bytes, entry.format, entry.extracted)
        : await prepareFileAttachment(entry.name, entry.bytes, entry.format);
    } catch {
      rejected.push({ name: entry.name, reason: "failed" });
      return null;
    }
  }));
  const known = new Set(intake.existingFiles().map((file) => file.id));
  const accepted: FileAttachment[] = [];
  for (const file of uploaded) {
    if (!file || known.has(file.id)) continue;
    known.add(file.id);
    accepted.push(file);
  }
  return { images, files: accepted, rejected };
}

/** Merges newly attached files into a message's list, by id, keeping order. */
export function mergeFileAttachments(
  current: readonly FileAttachment[],
  added: readonly FileAttachment[]
): FileAttachment[] {
  const known = new Set(current.map((file) => file.id));
  const merged = [...current];
  for (const file of added) {
    if (known.has(file.id)) continue;
    known.add(file.id);
    merged.push(file);
  }
  return merged;
}

/**
 * Attaches files to a user message written or edited in place — on the
 * timeline, or in a template's body — given what the message already carries.
 * The caller numbers images against its own transcript and uploads everything;
 * the editor merges what comes back.
 */
export type AddMessageAttachments = (
  files: File[],
  existing: { images: readonly ImageAttachment[]; files: readonly FileAttachment[] },
  preRejected?: readonly AttachmentRejection[]
) => Promise<AttachmentIntakeResult>;
