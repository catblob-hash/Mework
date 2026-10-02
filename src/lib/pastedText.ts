/**
 * A long paste folds into a tag in the box, the way Claude Code folds it: the
 * draft holds a short label, and the text the label stands for waits beside the
 * draft until the message leaves the box, where every label becomes that text
 * again. Nothing downstream of the box ever sees a label.
 *
 * A label is plain text in the textarea, so it selects, copies, drags and undoes
 * like the words around it; `PastedTextTags.tsx` draws the tag behind it and
 * keeps the caret from stopping inside it. A label edited by hand is no longer
 * one, and sends as the characters it was left with.
 */

/** A paste longer than this many characters folds into a tag (Claude Code's threshold). */
const PASTED_TEXT_FOLD_CHARACTERS = 800;

export interface PastedText {
  /** Numbered per draft from 1, and never reused while the draft lasts. */
  id: number;
  /** What the draft holds in the text's place. Its number makes it unique in the draft. */
  label: string;
  content: string;
}

/** Names a paste by its number and by how many lines follow its first. */
export type PastedTextLabeler = (id: number, extraLines: number) => string;

export interface PastedTextRange {
  start: number;
  end: number;
  paste: PastedText;
}

/** What copying out of a box puts on the clipboard beside the plain text: the text as the box held it. */
export const DRAFT_CLIPBOARD_TYPE = "application/x-mework-draft+json";

export interface DraftClipboard {
  /** The copied span as the box held it, labels and all. */
  text: string;
  /** The text behind each label in the span. */
  pastes: { label: string; content: string }[];
}

/** A textarea keeps LF only; the text behind a label is what the box would have held. */
export function normalizePastedText(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}

export function foldsPastedText(text: string): boolean {
  return text.length > PASTED_TEXT_FOLD_CHARACTERS;
}

/** Claude Code's "+N lines": the line breaks in the text, so the lines after its first. */
export function pastedTextExtraLines(text: string): number {
  let breaks = 0;
  for (let index = 0; index < text.length; index += 1) {
    if (text.charCodeAt(index) === 10) breaks += 1;
  }
  return breaks;
}

export function nextPastedTextId(pastes: readonly { id: number }[]): number {
  return pastes.reduce((highest, paste) => Math.max(highest, paste.id), 0) + 1;
}

/** Where the draft's labels stand, first to last. */
export function pastedTextRanges(value: string, pastes: readonly PastedText[]): PastedTextRange[] {
  if (!value || !pastes.length) return [];
  const found: PastedTextRange[] = [];
  for (const paste of pastes) {
    if (!paste.label) continue;
    for (let at = value.indexOf(paste.label); at >= 0; at = value.indexOf(paste.label, at + paste.label.length)) {
      found.push({ start: at, end: at + paste.label.length, paste });
    }
  }
  found.sort((left, right) => left.start - right.start || right.end - left.end);
  // Labels differ in their numbers, so none sits inside another; this only
  // guards a label a user built by hand out of pieces of two.
  const ranges: PastedTextRange[] = [];
  for (const range of found) {
    if (range.start >= (ranges.at(-1)?.end ?? 0)) ranges.push(range);
  }
  return ranges;
}

/** The draft as it is sent: every label replaced by the text it stands for. */
export function expandPastedTexts(value: string, pastes: readonly PastedText[]): string {
  const ranges = pastedTextRanges(value, pastes);
  if (!ranges.length) return value;
  let expanded = "";
  let at = 0;
  for (const range of ranges) {
    expanded += value.slice(at, range.start) + range.paste.content;
    at = range.end;
  }
  return expanded + value.slice(at);
}

/** The label a range cut into, if the position falls strictly inside one. */
export function pastedTextRangeAround(
  ranges: readonly PastedTextRange[],
  position: number
): PastedTextRange | undefined {
  return ranges.find((range) => range.start < position && position < range.end);
}

/**
 * What copying `value[start, end)` out of a box puts on the clipboard: the text
 * as it reads anywhere else (every label expanded), and the box's own form, so a
 * paste back into a box brings back exactly what was copied — tags as tags, and
 * text as text however long it is.
 */
export function draftClipboard(
  value: string,
  start: number,
  end: number,
  pastes: readonly PastedText[]
): { plain: string; draft: DraftClipboard } {
  const text = value.slice(start, end);
  const used = new Map<string, string>();
  for (const range of pastedTextRanges(text, pastes)) used.set(range.paste.label, range.paste.content);
  return {
    plain: expandPastedTexts(text, pastes),
    draft: { text, pastes: [...used].map(([label, content]) => ({ label, content })) }
  };
}

export function parseDraftClipboard(raw: string): DraftClipboard | null {
  if (!raw) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    const { text, pastes } = parsed as Partial<DraftClipboard>;
    if (typeof text !== "string" || !Array.isArray(pastes)) return null;
    const valid = pastes.filter((paste): paste is DraftClipboard["pastes"][number] => (
      Boolean(paste) && typeof paste.label === "string" && Boolean(paste.label) && typeof paste.content === "string"
    ));
    return { text, pastes: valid };
  } catch {
    return null;
  }
}

/**
 * Brings copied tags into a draft. A tag whose text the draft already holds
 * keeps that paste's number — cutting a tag and pasting it back leaves it as it
 * was — and any other takes the draft's next free number, so a #1 copied out of
 * another draft never collides with this one's #1.
 */
export function adoptDraftClipboard(
  clip: DraftClipboard,
  existing: readonly PastedText[],
  labelFor: PastedTextLabeler
): { text: string; added: PastedText[] } {
  if (!clip.pastes.length) return { text: clip.text, added: [] };
  const added: PastedText[] = [];
  let nextId = nextPastedTextId(existing);
  const incoming = clip.pastes.map((paste, index) => ({ id: -1 - index, label: paste.label, content: paste.content }));
  const adopted = new Map<string, PastedText>();
  for (const paste of incoming) {
    const held = existing.find((candidate) => candidate.content === paste.content)
      ?? added.find((candidate) => candidate.content === paste.content);
    if (held) {
      adopted.set(paste.label, held);
      continue;
    }
    const id = nextId;
    nextId += 1;
    const fresh = { id, label: labelFor(id, pastedTextExtraLines(paste.content)), content: paste.content };
    added.push(fresh);
    adopted.set(paste.label, fresh);
  }
  let text = "";
  let at = 0;
  for (const range of pastedTextRanges(clip.text, incoming)) {
    text += clip.text.slice(at, range.start) + adopted.get(range.paste.label)!.label;
    at = range.end;
  }
  return { text: text + clip.text.slice(at), added };
}
