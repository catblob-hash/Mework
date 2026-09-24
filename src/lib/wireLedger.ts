/**
 * Reading model for the request ledger.
 *
 * The host records one row per payload it puts on the wire. This module turns
 * those rows into what the history pane draws: the requests grouped into the
 * user-level turns that issued them, and each turn read as one payload whose
 * replayed history folds away behind a rule, leaving what the turn itself put
 * on the wire — request by request — in front of the reader.
 *
 * Nothing here re-derives what was sent. Every label, preview and diff is
 * computed from the recorded bytes, so a pane built on it cannot drift from the
 * request it is describing. The two counters a turn shows are the exception
 * that proves it: the host counts them against the predecessor's own parts when
 * the row is written, because the renderer would have to read every payload
 * back to reach the same number.
 */

import { lineDiff } from "./lineDiff";
import type {
  WirePartKind,
  WireRequestDetail,
  WireRequestPart,
  WireRequestSummary,
  WireUsage
} from "./runtime";

/** Longest single-line preview kept for a row title. */
const PREVIEW_LIMIT = 160;

/** Who put a message into the history, as the wire itself shows it. */
export type WireAuthor = "user" | "model";

/** One part of a request, described well enough to draw a row for it. */
export interface LedgerPart {
  ordinal: number;
  kind: WirePartKind;
  hash: string;
  truncated: boolean;
  /** Wire role for a message; the part's own name otherwise. */
  role: string;
  /** What follows the role in the row title: tool names, an attachment count. */
  detail: string;
  /** Single-line summary of the body. */
  preview: string;
  /** The body as text, which is what the row shows when it opens. */
  text: string;
  bytes: number;
  /** Only on a message, and only when its shape says who wrote it. */
  author?: WireAuthor;
}

/** One request, as a row of the ledger. */
export interface LedgerTurn {
  key: string;
  summary: WireRequestSummary;
}

/**
 * A user-level turn: something the person said, and every request it set off.
 * The same granularity the timeline draws a round at.
 */
export interface LedgerRound {
  key: string;
  /** Position among the rounds the ledger still holds, counted from one. */
  index: number;
  turns: LedgerTurn[];
  /** The model the round ran on. */
  modelId: string;
  /** True when the requests in this round did not all name one model. */
  mixedModels: boolean;
  usage: WireUsage;
  /** Null when no request in the round recorded a count of its own. */
  messagesAdded: number | null;
  messagesRemoved: number | null;
  createdAt: string;
}

/** How one part of a request stands against the request before it. */
export type EntryStatus = "kept" | "added" | "removed" | "changed";

/** One part of a request, with what happened to it since the last one. */
export interface LedgerEntry {
  key: string;
  status: EntryStatus;
  /** The part after the change, or the removed part for a removal. */
  part: LedgerPart;
  additions: number;
  deletions: number;
  /** Unified diff of the two bodies; empty unless the part was rewritten. */
  patch: string;
}

/** One request of a turn, as the pane has it in hand. */
export interface TurnRequest {
  summary: WireRequestSummary;
  parts: readonly WireRequestPart[];
}

/**
 * One request of a turn, and the changes it was the first to put on the wire.
 *
 * A part it carried exactly as the request before it did is not in here: that
 * part was already drawn where something last happened to it, either in the
 * turn's replayed history or under an earlier request of the same turn. So
 * every part of every payload is still accounted for, each at the one place it
 * has something to say.
 */
export interface LedgerRequestGroup {
  key: string;
  summary: WireRequestSummary;
  /** Prompt and message entries this request introduced, in wire order. */
  entries: LedgerEntry[];
}

/**
 * A user-level turn, read against the request that went out before it.
 *
 * Every request replays the whole history, so almost all of what a turn sends
 * is `cached` — messages it carried over untouched. Splitting those off behind
 * one rule is what lets the rest, which is the turn's own output and whatever
 * the user edited, be read without scrolling past everything that came before.
 */
export interface TurnReading {
  /** The prompts and the tool surface. Never folded: there are at most three. */
  prompts: LedgerEntry[];
  /** Messages this turn replayed untouched from before it, in wire order. */
  cached: LedgerEntry[];
  /** One group per request, in the order they went out. */
  groups: LedgerRequestGroup[];
  /**
   * False when there was no earlier request to read this turn's first payload
   * against, so nothing in it is attributed to anyone. Without it an
   * unattributed turn and one that genuinely repeated the payload before it
   * would draw the same.
   */
  comparable: boolean;
}

function collapse(text: string): string {
  const line = text
    .split("\n")
    .map((entry) => entry.trim())
    .find((entry) => entry.length > 0);
  if (!line) return "";
  return line.length > PREVIEW_LIMIT ? `${line.slice(0, PREVIEW_LIMIT)}…` : line;
}

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? "";
  } catch {
    return String(value);
  }
}

function parse(body: string): unknown {
  try {
    return JSON.parse(body);
  } catch {
    return null;
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/**
 * Renders one content part the way its own shape asks to be read: prose as
 * prose, a call as its arguments, a result as its output. A part whose shape is
 * unknown falls back to its JSON rather than disappearing — an unreadable row is
 * still evidence, an absent one is not.
 */
/**
 * Keys under which a provider carries the part of its reasoning that only it can
 * read back: Anthropic's `signature` / `redactedData`, and the Responses family's
 * `reasoningEncryptedContent` / `itemId`. A card with any of them replays as
 * itself; one without is text the next request will drop.
 */
const REASONING_PAYLOAD_KEYS = ["signature", "redactedData", "reasoningEncryptedContent", "itemId"];

function renderContentPart(part: Record<string, unknown>, names: string[]): string {
  const type = text(part.type);
  if (type === "text") return text(part.text);
  if (type === "reasoning") {
    const options = asRecord(part.providerOptions);
    const signed = options
      ? Object.values(options).some((value) => {
          const inner = asRecord(value);
          return Boolean(inner && REASONING_PAYLOAD_KEYS.some((key) => inner[key]));
        })
      : false;
    const head = signed ? "[reasoning · signed]" : "[reasoning]";
    // The payload is shown rather than summarised away: whether a signature or a
    // ciphertext actually went out is the question this pane exists to answer.
    const tail = options ? `\n\n[providerOptions]\n${pretty(options)}` : "";
    return `${head}\n${text(part.text)}${tail}`;
  }
  if (type === "tool-call") {
    const name = text(part.toolName);
    if (name) names.push(name);
    return `→ ${name} (${text(part.toolCallId)})\n${pretty(part.input)}`;
  }
  if (type === "tool-result") {
    const name = text(part.toolName);
    if (name) names.push(name);
    const output = asRecord(part.output);
    const body =
      output && (output.type === "text" || output.type === "error-text")
        ? text(output.value)
        : pretty(part.output);
    return `← ${name} (${text(part.toolCallId)})\n${body}`;
  }
  if (type === "image") {
    names.push("image");
    // The recorded form is the attachment placeholder, never the base64 the
    // provider received; say so rather than letting a reader assume a leak.
    return `[image ${text(part.mediaType)} · attachment reference, bytes not recorded]\n${pretty(part.image)}`;
  }
  if (type === "mework-file") {
    names.push("file");
    // Like an image, a file is recorded as its reference; the text the model
    // read is inlined only on the way out, so the ledger never holds it.
    const file = asRecord(part.file);
    return `[file ${text(file?.name)} · attachment reference, text not recorded]\n${pretty(part.file)}`;
  }
  return pretty(part);
}

/**
 * Who a message came from, which the wire role alone does not answer.
 *
 * Anthropic carries tool results inside a `user` message, so reading the role
 * literally would make every round of a tool-using turn look like something the
 * person typed — and the whole point of the counts on a turn is to separate
 * what the user did from what the model did.
 */
function messageAuthor(message: Record<string, unknown>): WireAuthor {
  if (text(message.role) !== "user") return "model";
  const content = message.content;
  if (
    Array.isArray(content)
    && content.length > 0
    && content.every((entry) => asRecord(entry)?.type === "tool-result")
  ) {
    return "model";
  }
  return "user";
}

function describeMessage(part: WireRequestPart): LedgerPart {
  const message = asRecord(parse(part.body));
  if (!message) {
    return {
      ordinal: part.ordinal,
      kind: part.kind,
      hash: part.hash,
      truncated: part.truncated,
      role: "message",
      detail: "",
      preview: collapse(part.body),
      text: part.body,
      bytes: part.bytes
    };
  }
  const role = text(message.role) || "message";
  const names: string[] = [];
  let body: string;
  if (typeof message.content === "string") {
    body = message.content;
  } else if (Array.isArray(message.content)) {
    body = message.content
      .map((entry) => {
        const record = asRecord(entry);
        return record ? renderContentPart(record, names) : pretty(entry);
      })
      .join("\n\n");
  } else {
    body = pretty(message.content);
  }
  const options = asRecord(message.providerOptions);
  if (options) body = `${body}\n\n[providerOptions]\n${pretty(options)}`;
  return {
    ordinal: part.ordinal,
    kind: part.kind,
    hash: part.hash,
    truncated: part.truncated,
    role,
    detail: [...new Set(names)].join(", "),
    preview: collapse(body),
    text: body,
    bytes: part.bytes,
    author: messageAuthor(message)
  };
}

/** Describes one recorded part: a prompt, the tool surface, or a message. */
export function describePart(part: WireRequestPart): LedgerPart {
  if (part.kind === "message") return describeMessage(part);
  if (part.kind === "tools") {
    const tools = parse(part.body);
    const names = Array.isArray(tools)
      ? tools.map((entry) => text(asRecord(entry)?.name)).filter(Boolean)
      : [];
    return {
      ordinal: part.ordinal,
      kind: part.kind,
      hash: part.hash,
      truncated: part.truncated,
      role: "tools",
      detail: String(names.length),
      preview: names.join(", "),
      text: pretty(tools ?? part.body),
      bytes: part.bytes
    };
  }
  return {
    ordinal: part.ordinal,
    kind: part.kind,
    hash: part.hash,
    truncated: part.truncated,
    role: part.kind === "system" ? "system" : "system+",
    detail: "",
    preview: collapse(part.body),
    text: part.body,
    bytes: part.bytes
  };
}

/** The usage counters the ledger records, for field-wise arithmetic. */
const USAGE_FIELDS = ["inputTokens", "cachedInputTokens", "outputTokens"] as const;

/**
 * Adds up what a group of requests cost.
 *
 * A counter nobody reported stays absent rather than becoming zero: the pane
 * says "not recorded" for what it cannot vouch for, and a zero would be a claim.
 * Input is summed across rounds even though each round replays the history —
 * that repetition is exactly what was paid for, and it is how the timeline's
 * own round total is computed.
 */
export function sumWireUsage(usages: readonly (WireUsage | undefined)[]): WireUsage {
  const total: WireUsage = {};
  for (const field of USAGE_FIELDS) {
    const values = usages.flatMap((usage) => {
      const value = usage?.[field];
      return typeof value === "number" ? [value] : [];
    });
    if (values.length) total[field] = values.reduce((sum, value) => sum + value, 0);
  }
  return total;
}

/** True when nothing about this usage was reported. */
export function wireUsageIsEmpty(usage: WireUsage): boolean {
  return USAGE_FIELDS.every((field) => usage[field] === undefined);
}

/**
 * Whether a request opens a user-level turn.
 *
 * A turn is one thing the person said and everything it set off, which is the
 * granularity the timeline draws a round at. The ledger can tell that from its
 * own rows without consulting the timeline: the request has to belong to a
 * different run than the one before it — the later rounds of a turn are the
 * same run — and it has to carry a message the person actually wrote. A bare
 * Send, a task wake and an `ask_user` answer all open a new run while
 * continuing the same round, and none of them reaches the model as typed text:
 * they arrive as tool results, which `messageAuthor` reads as the model's.
 *
 * A host-minted native search or fetch opens nothing. It spends tokens inside
 * whatever the user was already doing and belongs to that turn.
 */
function opensRound(summary: WireRequestSummary, previous: WireRequestSummary): boolean {
  if (summary.kind !== "model") return false;
  if (summary.requestId !== "" && summary.requestId === previous.requestId) return false;
  // A row recorded before the ledger counted its own delta cannot answer this,
  // so it falls back to the run boundary — which is what the pane drew before
  // the counts existed.
  return summary.messagesAdded === undefined || summary.messagesAdded > 0;
}

/** Sums a round's counts, keeping `null` for a round that recorded none. */
function countTotal(
  turns: readonly LedgerTurn[],
  read: (summary: WireRequestSummary) => number | undefined
): number | null {
  const values = turns.flatMap((turn) => {
    const value = read(turn.summary);
    return typeof value === "number" ? [value] : [];
  });
  return values.length ? values.reduce((sum, value) => sum + value, 0) : null;
}

/**
 * Groups the recorded requests into the turns that issued them.
 *
 * Nothing is dropped and nothing is re-ordered: every request appears under
 * exactly one turn, in the order it went out, so the list still accounts for
 * everything that was sent.
 */
export function ledgerRounds(requests: readonly WireRequestSummary[]): LedgerRound[] {
  const groups: LedgerTurn[][] = [];
  requests.forEach((summary, index) => {
    const previous = requests[index - 1];
    if (!previous || opensRound(summary, previous)) groups.push([]);
    groups[groups.length - 1].push({ key: `r${summary.seq}`, summary });
  });
  return groups.map((turns, index) => {
    const modelId = turns[0].summary.modelId;
    return {
      key: `t${turns[0].summary.seq}`,
      index: index + 1,
      turns,
      modelId,
      mixedModels: turns.some((turn) => turn.summary.modelId !== modelId),
      usage: sumWireUsage(turns.map((turn) => turn.summary.usage)),
      messagesAdded: countTotal(turns, (summary) => summary.messagesAdded),
      messagesRemoved: countTotal(turns, (summary) => summary.messagesRemoved),
      createdAt: turns[0].summary.createdAt
    };
  });
}

/**
 * Ceiling on the alignment table.
 *
 * The table is `before × after` cells. Past this the pair is reported as wholly
 * replaced, which is the honest answer for a comparison that is not worth
 * blocking a frame on.
 */
const MAX_ALIGN_CELLS = 1_000_000;

/**
 * Aligns two parts arrays by body hash.
 *
 * Hash equality is exact, which is the point: a part that survived a change
 * untouched must not be reported as rewritten just because something before it
 * moved. Returns, per side, whether each entry is part of the common
 * subsequence.
 *
 * The common head and tail are stripped before the table is allocated, and that
 * is not an optimisation for a rare case — it is the normal case. Every request
 * replays the whole history, so two consecutive requests differ only by what the
 * last round appended; without the strip, a conversation a few thousand messages
 * long would allocate hundreds of megabytes to report two added rows.
 */
function alignByHash(
  before: readonly WireRequestPart[],
  after: readonly WireRequestPart[]
): { beforeKept: boolean[]; afterKept: boolean[] } {
  const beforeKept = new Array<boolean>(before.length).fill(false);
  const afterKept = new Array<boolean>(after.length).fill(false);
  const shortest = Math.min(before.length, after.length);
  let head = 0;
  while (head < shortest && before[head].hash === after[head].hash) {
    beforeKept[head] = true;
    afterKept[head] = true;
    head += 1;
  }
  let tail = 0;
  while (
    tail < shortest - head
    && before[before.length - 1 - tail].hash === after[after.length - 1 - tail].hash
  ) {
    beforeKept[before.length - 1 - tail] = true;
    afterKept[after.length - 1 - tail] = true;
    tail += 1;
  }

  const rows = before.length - head - tail;
  const columns = after.length - head - tail;
  if (rows <= 0 || columns <= 0) return { beforeKept, afterKept };
  if ((rows + 1) * (columns + 1) > MAX_ALIGN_CELLS) return { beforeKept, afterKept };

  // One flat table, row-major, `(rows + 1) × (columns + 1)`, over the middles.
  const table = new Uint32Array((rows + 1) * (columns + 1));
  for (let row = rows - 1; row >= 0; row -= 1) {
    for (let column = columns - 1; column >= 0; column -= 1) {
      const index = row * (columns + 1) + column;
      table[index] =
        before[head + row].hash === after[head + column].hash
          ? table[index + columns + 2] + 1
          : Math.max(table[index + columns + 1], table[index + 1]);
    }
  }
  let row = 0;
  let column = 0;
  while (row < rows && column < columns) {
    const index = row * (columns + 1) + column;
    if (before[head + row].hash === after[head + column].hash) {
      beforeKept[head + row] = true;
      afterKept[head + column] = true;
      row += 1;
      column += 1;
      continue;
    }
    if (table[index + columns + 1] >= table[index + 1]) row += 1;
    else column += 1;
  }
  return { beforeKept, afterKept };
}

/** Whether a removal and an addition are two versions of the same part. */
function samePart(before: LedgerPart, after: LedgerPart): boolean {
  if (before.kind !== after.kind) return false;
  if (before.kind !== "message") return true;
  return before.role === after.role;
}

/** A part this request carried exactly as the one before it did. */
function keptEntry(part: WireRequestPart): LedgerEntry {
  return {
    key: `=${part.ordinal}-${part.hash}`,
    status: "kept",
    part: describePart(part),
    additions: 0,
    deletions: 0,
    patch: ""
  };
}

/**
 * Turns one run of removals and additions into entries.
 *
 * A removal and an addition of the same kind and role in the same place are one
 * part that was rewritten, not two unrelated events — reporting them separately
 * is what makes a one-word edit read as a message deleted and another invented.
 */
function pairRun(removed: WireRequestPart[], added: WireRequestPart[]): LedgerEntry[] {
  const entries: LedgerEntry[] = [];
  const pending = removed.map((part) => describePart(part));
  for (const entry of added) {
    const part = describePart(entry);
    const matchAt = pending.findIndex((candidate) => samePart(candidate, part));
    const source = matchAt < 0 ? null : pending.splice(matchAt, 1)[0];
    const diff = lineDiff(source ? source.text : "", part.text, {
      path: `${part.role}#${part.ordinal}`
    });
    entries.push({
      key: `${source ? "~" : "+"}${entry.ordinal}-${entry.hash}`,
      status: source ? "changed" : "added",
      part,
      additions: diff.additions,
      deletions: diff.deletions,
      patch: diff.patch
    });
  }
  for (const part of pending) {
    const diff = lineDiff(part.text, "", { path: `${part.role}#${part.ordinal}` });
    entries.push({
      key: `-${part.ordinal}-${part.hash}`,
      status: "removed",
      part,
      additions: diff.additions,
      deletions: diff.deletions,
      patch: diff.patch
    });
  }
  return entries;
}

/**
 * Every part of `after`, in wire order, saying what happened to it since
 * `before` — with the parts that disappeared standing where they stood.
 *
 * Unlike a diff, nothing is omitted: a request is the whole payload, and a
 * reading of it that skipped what did not change would not be a reading of the
 * payload at all.
 */
function alignEntries(
  before: readonly WireRequestPart[],
  after: readonly WireRequestPart[]
): LedgerEntry[] {
  const { beforeKept, afterKept } = alignByHash(before, after);
  const entries: LedgerEntry[] = [];
  let row = 0;
  let column = 0;
  while (row < before.length || column < after.length) {
    const removed: WireRequestPart[] = [];
    while (row < before.length && !beforeKept[row]) {
      removed.push(before[row]);
      row += 1;
    }
    const added: WireRequestPart[] = [];
    while (column < after.length && !afterKept[column]) {
      added.push(after[column]);
      column += 1;
    }
    if (removed.length || added.length) {
      entries.push(...pairRun(removed, added));
      continue;
    }
    // Both sides sit on a kept entry, and the alignment pairs those one to one.
    if (row >= before.length || column >= after.length) break;
    entries.push(keptEntry(after[column]));
    row += 1;
    column += 1;
  }
  return entries;
}

function isMessage(part: WireRequestPart): boolean {
  return part.kind === "message";
}

/** The entries of an alignment that say something happened. */
function changedOnly(entries: readonly LedgerEntry[]): LedgerEntry[] {
  return entries.filter((entry) => entry.status !== "kept");
}

/**
 * Reads one user-level turn: its requests in the order they went out, against
 * the request that went out before the turn began.
 *
 * `before` is `null` when there is no such request to compare against — the
 * ledger's retention pruned it, or the row was dropped. The turn's opening
 * payload then reads as replayed rather than as invented here: the record
 * cannot say who put those messages in the history, and green rows would be a
 * claim it cannot support. Pass an empty array instead for a turn that opens
 * the conversation, where "nothing came before" is a fact rather than a gap.
 *
 * Each later request is read against the one before it, so a group holds
 * exactly what that payload was the first to carry — which is what lets the
 * pane put a rule under each request without drawing the same message twice.
 */
export function readTurn(
  before: readonly WireRequestPart[] | null,
  requests: readonly TurnRequest[]
): TurnReading {
  const [first, ...rest] = requests;
  if (!first) return { prompts: [], cached: [], groups: [], comparable: false };
  const messages = first.parts.filter(isMessage);
  const prompts = first.parts.filter((part) => !isMessage(part));
  const opening = before
    ? alignEntries(before.filter(isMessage), messages)
    : messages.map((part) => keptEntry(part));
  const groups: LedgerRequestGroup[] = [
    { key: `q${first.summary.seq}`, summary: first.summary, entries: changedOnly(opening) }
  ];
  let previous = first.parts;
  for (const request of rest) {
    // A prompt that changed mid-turn belongs to the request that changed it;
    // the tool surface moving between two rounds is exactly the kind of thing
    // this pane is opened to find, and it has to be visible where it happened.
    groups.push({
      key: `q${request.summary.seq}`,
      summary: request.summary,
      entries: [
        ...changedOnly(
          alignEntries(
            previous.filter((part) => !isMessage(part)),
            request.parts.filter((part) => !isMessage(part))
          )
        ),
        ...changedOnly(alignEntries(previous.filter(isMessage), request.parts.filter(isMessage)))
      ]
    });
    previous = request.parts;
  }
  return {
    prompts: before
      ? alignEntries(before.filter((part) => !isMessage(part)), prompts)
      : prompts.map((part) => keptEntry(part)),
    cached: opening.filter((entry) => entry.status === "kept"),
    groups,
    comparable: before !== null
  };
}

/**
 * Whether any compared body was cut at record time.
 *
 * A part is hashed as it is stored, so two requests whose bodies differ only
 * past the cap compare as identical. "Nothing changed" would be a claim the
 * record cannot support; this is what lets the pane say so.
 */
export function anyTruncated(...groups: readonly (readonly WireRequestPart[])[]): boolean {
  return groups.some((parts) => parts.some((part) => part.truncated));
}

/**
 * Field order of the request as the host serialises it (`StepRequest`). The
 * parts are stored apart from the envelope only so the ledger does not grow with
 * the square of the conversation; putting them back in declaration order is what
 * makes "the payload as sent" true of the object rather than nearly true.
 */
const WIRE_FIELD_ORDER = [
  "family",
  "baseURL",
  "settings",
  "modelId",
  "system",
  "systemDynamic",
  "messages",
  "tools",
  "maxSteps",
  "maxOutputTokens",
  "reasoning",
  "reasoningContent",
  "promptCache",
  "providerOptions",
  "nativeSearch",
  "nativeFetch"
];

/**
 * Reassembles the recorded request as one object, in the order the host wrote
 * its fields. `$notRecorded` leads, because what is absent from a forensic
 * record has to be the first thing its reader sees.
 */
export function rawPayload(detail: WireRequestDetail): unknown {
  const envelope = asRecord(detail.envelope) ?? {};
  const fields: Record<string, unknown> = { ...envelope };
  const messages: unknown[] = [];
  for (const part of detail.parts) {
    if (part.kind === "message") {
      messages.push(parse(part.body) ?? part.body);
      continue;
    }
    if (part.kind === "tools") {
      fields.tools = parse(part.body) ?? part.body;
      continue;
    }
    fields[part.kind] = part.body;
  }
  fields.messages = messages;

  const payload: Record<string, unknown> = {};
  if ("$notRecorded" in fields) payload.$notRecorded = fields.$notRecorded;
  for (const key of WIRE_FIELD_ORDER) {
    if (key in fields) payload[key] = fields[key];
  }
  // Anything the envelope carried that this list does not name is still part of
  // the record; dropping it to keep the order tidy would be the one thing this
  // pane must never do.
  for (const [key, value] of Object.entries(fields)) {
    if (!(key in payload)) payload[key] = value;
  }
  return payload;
}
