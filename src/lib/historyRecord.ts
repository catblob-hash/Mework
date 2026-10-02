/**
 * Reading model for the conversation's history.
 *
 * The host keeps one ordered record of everything that happened to a
 * conversation: every payload it put on the wire, every response, every hook
 * decision, every call as it actually ran and what it returned, and every change
 * to the timeline — what the user sent, what a run settled onto it, and what the
 * user edited by hand. This module turns those entries into what the history pane
 * draws: one bar per thing that happened at the top level — a turn the user
 * started, a run of context edits, a turn that was cut off — and inside each, the
 * messages it added, removed or rewrote.
 *
 * Nothing here re-derives what happened. Every label, preview and diff is
 * computed from the recorded bytes, so a pane built on it cannot drift from the
 * record it is describing. The two counters a turn shows are the exception that
 * proves it: the host counts them against the predecessor's own parts when each
 * request is written, because the renderer would have to read every payload back
 * to reach the same number.
 */

import { lineDiff } from "./lineDiff";
import type {
  HistoryEntry,
  HistoryEntryDetail,
  HistoryOp,
  HistoryPart,
  HistoryRequestKind,
  HistoryUsage
} from "./runtime";

/** Longest single-line preview kept for a row title. */
const PREVIEW_LIMIT = 160;

/**
 * A request entry read as the summary bars are drawn from: its `detail` fields
 * at hand, with the usage the host put on it from its response.
 */
export interface RequestSummary {
  seq: number;
  createdAt: string;
  kind: HistoryRequestKind;
  /** The run this belonged to; empty for host-minted one-shots. */
  requestId: string;
  modelId: string;
  /** What the response reported; absent on a request that never got one. */
  usage?: HistoryUsage;
  /**
   * Whole messages the user added between the request before this one and this
   * one, counted by the host when the entry was written. Absent on entries
   * recorded before the host counted them — which is not the same as zero.
   */
  messagesAdded?: number;
}

function detailString(detail: Record<string, unknown>, key: string): string {
  const value = detail[key];
  return typeof value === "string" ? value : "";
}

function detailNumber(detail: Record<string, unknown>, key: string): number | undefined {
  const value = detail[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function detailFlag(detail: Record<string, unknown>, key: string): boolean {
  return detail[key] === true;
}

/** Reads a request entry as a summary. */
export function requestSummary(entry: HistoryEntry): RequestSummary {
  const detail = entry.detail;
  const kind = detailString(detail, "type");
  return {
    seq: entry.seq,
    createdAt: entry.createdAt,
    kind: kind === "search" || kind === "fetch" ? kind : "model",
    requestId: entry.requestId ?? "",
    modelId: detailString(detail, "modelId"),
    usage: entry.usage,
    messagesAdded: detailNumber(detail, "messagesAdded")
  };
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
 * Keys under which a provider carries the part of its reasoning that only it can
 * read back: Anthropic's `signature` / `redactedData`, and the Responses family's
 * `reasoningEncryptedContent` / `itemId`. A card with any of them replays as
 * itself; one without is text the next request will drop.
 */
const REASONING_PAYLOAD_KEYS = ["signature", "redactedData", "reasoningEncryptedContent", "itemId"];

/**
 * Renders one content part the way its own shape asks to be read: prose as
 * prose, a call as its arguments, a result as its output. A part whose shape is
 * unknown falls back to its JSON rather than disappearing — an unreadable row is
 * still evidence, an absent one is not.
 */
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
  return pretty(part);
}

/** A message as the row it draws: its role, what it called, and its text. */
interface DescribedMessage {
  role: string;
  /** Tool names: what follows the role in the row title. */
  detail: string;
  preview: string;
  text: string;
}

/** Reads one recorded message body — a response, as the model sent it back. */
function describeMessage(body: string): DescribedMessage {
  const message = asRecord(parse(body));
  if (!message) return { role: "message", detail: "", preview: collapse(body), text: body };
  const role = text(message.role) || "message";
  const names: string[] = [];
  let rendered: string;
  if (typeof message.content === "string") {
    rendered = message.content;
  } else if (Array.isArray(message.content)) {
    rendered = message.content
      .map((entry) => {
        const record = asRecord(entry);
        return record ? renderContentPart(record, names) : pretty(entry);
      })
      .join("\n\n");
  } else {
    rendered = pretty(message.content);
  }
  const options = asRecord(message.providerOptions);
  if (options) rendered = `${rendered}\n\n[providerOptions]\n${pretty(options)}`;
  return {
    role,
    detail: [...new Set(names)].join(", "),
    preview: collapse(rendered),
    text: rendered
  };
}

/** The usage counters the record keeps, for field-wise arithmetic. */
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
export function sumUsage(usages: readonly (HistoryUsage | undefined)[]): HistoryUsage {
  const total: HistoryUsage = {};
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
export function usageIsEmpty(usage: HistoryUsage): boolean {
  return USAGE_FIELDS.every((field) => usage[field] === undefined);
}

/**
 * Whether a request with nothing else to place it opens a user-level turn.
 *
 * What the user sent is on record as a change of its own and opens the turn it
 * starts; this is for requests recorded without one. The request has to belong to
 * a different run than the one before it — the later rounds of a turn are the
 * same run — and it has to carry a message the person actually wrote. A bare
 * Send, a task wake and an `ask_user` answer all open a new run while continuing
 * the same round, and none of them reaches the model as typed text: they arrive
 * as tool results, which the host does not count as the user's.
 *
 * A host-minted native search or fetch opens nothing. It spends tokens inside
 * whatever the user was already doing and belongs to that turn.
 */
function opensRound(summary: RequestSummary, previous: RequestSummary): boolean {
  if (summary.kind !== "model") return false;
  if (summary.requestId !== "" && summary.requestId === previous.requestId) return false;
  // A row recorded before the host counted its own delta cannot answer this,
  // so it falls back to the run boundary.
  return summary.messagesAdded === undefined || summary.messagesAdded > 0;
}

/**
 * What a bar at the top of the history stands for.
 *
 * - `turn`: something the user sent, and everything it set off.
 * - `interrupted`: a turn whose last request never got its answer back — the
 *   stream was cut, the app went away, or the run was stopped mid-answer.
 * - `edits`: changes the user made to the context by hand, back to back.
 * - `pending`: something the user sent that no request has carried yet.
 */
export type BarKind = "turn" | "interrupted" | "edits" | "pending";

/** One bar of the history, and the entries it is drawn from. */
export interface HistoryBar {
  key: string;
  kind: BarKind;
  /** Position among the turns, counted from one; zero on an edit bar. */
  index: number;
  /** The requests its runs sent, oldest first. */
  requests: RequestSummary[];
  /** Every other entry recorded in it, oldest first. */
  entries: HistoryEntry[];
  /** The model the turn ran on; empty on a bar that sent nothing. */
  modelId: string;
  /** True when the requests in this bar did not all name one model. */
  mixedModels: boolean;
  usage: HistoryUsage;
  createdAt: string;
}

/** Whether a timeline change is what the user sent, or the record's starting point. */
function opensTurn(entry: HistoryEntry): boolean {
  const source = entry.detail.source;
  return source === "message" || source === "baseline";
}

interface DraftBar {
  kind: "turn" | "edits";
  opener: HistoryEntry;
  requests: RequestSummary[];
  entries: HistoryEntry[];
}

/**
 * Splits a history into the bars the pane draws.
 *
 * What the user sent opens a turn, and every entry of a run goes with the turn
 * that run's first request went out in — wherever its number falls, so a
 * `UserPromptSubmit` hook before the request and the settlement after the last
 * one land in the same place. The user's own edits to the context stand between
 * turns as a bar of their own, and a run that starts after them — a resend of an
 * edited message — is a turn of its own, not the tail of the one before the edits.
 *
 * `live` says the newest turn is still running, so its unanswered request is one
 * still being answered rather than one that was cut off.
 *
 * Nothing is dropped and nothing is re-ordered inside a bar.
 */
export function historyBars(
  entries: readonly HistoryEntry[],
  options: { live?: boolean } = {}
): HistoryBar[] {
  const drafts: DraftBar[] = [];
  const byRun = new Map<string, DraftBar>();
  /** Entries of a run whose first request has not gone out yet, and the turn they followed. */
  const early = new Map<string, { turn: DraftBar | null; entries: HistoryEntry[] }>();
  /** The turn a request with nothing else to place it joins. */
  let turn: DraftBar | null = null;
  let previous: RequestSummary | null = null;
  const open = (kind: DraftBar["kind"], opener: HistoryEntry): DraftBar => {
    const draft: DraftBar = { kind, opener, requests: [], entries: [] };
    drafts.push(draft);
    return draft;
  };

  for (const entry of entries) {
    if (entry.kind === "edit") {
      if (opensTurn(entry)) {
        // A turn nothing has sent yet takes the next message as part of itself.
        if (!turn || turn.requests.length) turn = open("turn", entry);
        turn.entries.push(entry);
      } else {
        const last = drafts.at(-1);
        (last?.kind === "edits" ? last : open("edits", entry)).entries.push(entry);
        turn = null;
      }
      continue;
    }
    if (entry.kind === "request") {
      const summary = requestSummary(entry);
      let draft = summary.requestId ? byRun.get(summary.requestId) : undefined;
      if (!draft) {
        const joins =
          turn !== null
          && (!turn.requests.length || (previous !== null && !opensRound(summary, previous)));
        if (!joins || !turn) turn = open("turn", entry);
        draft = turn;
        if (summary.requestId) {
          byRun.set(summary.requestId, draft);
          const waiting = early.get(summary.requestId);
          if (waiting) {
            draft.entries.push(...waiting.entries);
            early.delete(summary.requestId);
          }
        }
      }
      draft.requests.push(summary);
      previous = summary;
      continue;
    }
    const draft = entry.requestId ? byRun.get(entry.requestId) : undefined;
    if (draft) {
      draft.entries.push(entry);
    } else if (entry.requestId) {
      const waiting = early.get(entry.requestId) ?? { turn, entries: [] };
      waiting.entries.push(entry);
      early.set(entry.requestId, waiting);
    } else {
      turn ??= open("turn", entry);
      turn.entries.push(entry);
    }
  }
  // A run a hook stopped before it sent anything stays with the turn it followed.
  for (const waiting of early.values()) {
    (waiting.turn ?? open("turn", waiting.entries[0])).entries.push(...waiting.entries);
  }

  const answered = new Set<number>();
  let firstResponse = Number.POSITIVE_INFINITY;
  for (const entry of entries) {
    if (entry.kind !== "response") continue;
    if (entry.answers !== undefined) answered.add(entry.answers);
    firstResponse = Math.min(firstResponse, entry.seq);
  }
  const running = options.live
    ? [...drafts].reverse().find((draft) => draft.requests.length > 0)
    : undefined;

  let turns = 0;
  return drafts.map((draft) => {
    const requests = draft.requests;
    const models = requests.filter((request) => request.kind === "model");
    const modelId = (models[0] ?? requests[0])?.modelId ?? "";
    const last = models.at(-1);
    let kind: BarKind = draft.kind;
    if (draft.kind === "turn") {
      if (!requests.length) kind = "pending";
      else if (
        last
        && draft !== running
        // An answer can only be missing once answers are being recorded at all:
        // a turn from before that has none, and was not cut off for it.
        && firstResponse < last.seq
        && !answered.has(last.seq)
      ) {
        kind = "interrupted";
      }
    }
    return {
      key: `b${draft.opener.seq}`,
      kind,
      index: draft.kind === "edits" ? 0 : ++turns,
      requests,
      entries: [...draft.entries].sort((left, right) => left.seq - right.seq),
      modelId,
      mixedModels: models.some((request) => request.modelId !== modelId),
      usage: sumUsage(requests.map((request) => request.usage)),
      createdAt: draft.opener.createdAt
    };
  });
}

/** One thing a bar lays out, in the order it happened. */
export type BarItem =
  /** A change to the timeline — what the user sent, edited, or a run settled — drawn as its messages. */
  | { type: "change"; entry: HistoryEntry }
  /** An entry of a run that never settled, drawn as itself: nothing else shows what it did. */
  | { type: "event"; entry: HistoryEntry }
  /**
   * A request of a run that never settled, drawn as the tools it was the first
   * to hand over by append, if any: its marker reached no timeline change.
   */
  | { type: "request"; request: RequestSummary };

/**
 * What a bar draws, in order.
 *
 * A run that settled is drawn as the messages its settlement put on the
 * timeline — its reasoning, what it said, each call with what it returned, each
 * tool it was handed along the way. Its responses, calls and results are what
 * those messages were made from, so they are not drawn again. A run that never
 * settled — still running, or gone before it could — has only those to show,
 * and its requests, which are where a tool it was handed shows.
 */
export function barItems(bar: HistoryBar): BarItem[] {
  const settled = new Set(
    bar.entries.flatMap((entry) => (entry.kind === "run" && entry.requestId ? [entry.requestId] : []))
  );
  const items = bar.entries.flatMap((entry): { seq: number; item: BarItem }[] => {
    if (entry.kind === "edit" || entry.kind === "run") return [{ seq: entry.seq, item: { type: "change", entry } }];
    if (entry.requestId && settled.has(entry.requestId)) return [];
    return [{ seq: entry.seq, item: { type: "event", entry } }];
  });
  for (const request of bar.requests) {
    if (request.kind !== "model" || (request.requestId && settled.has(request.requestId))) continue;
    items.push({ seq: request.seq, item: { type: "request", request } });
  }
  return items.sort((left, right) => left.seq - right.seq).map(({ item }) => item);
}

/** Where a request's marker messages name the tools they hand over (`tool_append.rs`). */
const MARKER_OPTIONS_KEY = "mework";
const MARKER_TOOLS_KEY = "toolAddition";

/** Read once per loaded payload: a running turn re-renders every second. */
const appendedByPayload = new WeakMap<readonly HistoryPart[], string[]>();

/**
 * The tools a request handed over by append rather than declared, in the order
 * its markers name them.
 *
 * A tool that joins mid-conversation stays in the request's tool list — the
 * sidecar needs its definition to hand it over — but reaches the model at the
 * marker the host left in the transcript where it joined. Every later request
 * replays that marker, so this is every tool appended up to this request.
 */
export function appendedTools(parts: readonly HistoryPart[]): string[] {
  const cached = appendedByPayload.get(parts);
  if (cached) return cached;
  const names: string[] = [];
  for (const part of parts) {
    if (part.kind !== "message" || !part.body.includes(`"${MARKER_TOOLS_KEY}"`)) continue;
    const message = asRecord(parse(part.body));
    if (message?.role !== "system") continue;
    const tools = asRecord(asRecord(message.providerOptions)?.[MARKER_OPTIONS_KEY])?.[MARKER_TOOLS_KEY];
    if (!Array.isArray(tools)) continue;
    for (const tool of tools) {
      if (typeof tool === "string" && !names.includes(tool)) names.push(tool);
    }
  }
  appendedByPayload.set(parts, names);
  return names;
}

/**
 * The tools a request was the first to hand over by append, against the request
 * before it — drawn as the system message that hands them over, which is what it
 * is on the wire and what its settled record says.
 */
export function describeAppended(
  before: readonly HistoryPart[] | null,
  after: readonly HistoryPart[],
  key: string,
  labels: EventLabels
): MessageRow[] {
  const earlier = new Set(before ? appendedTools(before) : []);
  const tools = appendedTools(after).filter((name) => !earlier.has(name));
  if (!tools.length) return [];
  return [
    {
      key: `${key}-appended`,
      change: "insert",
      kind: "toolsAdded",
      label: "system",
      detail: labels.toolsAdded(tools.length),
      badges: [],
      preview: tools.join(", "),
      text: tools.join("\n"),
      patch: "",
      additions: 0,
      deletions: 0
    }
  ];
}

/** One declared tool: its name, and its definition as the row shows it. */
interface DeclaredTool {
  name: string;
  text: string;
}

/**
 * The tools a request declared up front, in its order — its tool list minus
 * the ones it handed over by append — or `null` when it declared none.
 *
 * A list the record had to cut is no longer JSON; it is still compared and
 * shown, as the text it is, rather than read as no list at all.
 */
function declaredTools(parts: readonly HistoryPart[]): DeclaredTool[] | null {
  const part = parts.find((candidate) => candidate.kind === "tools");
  if (!part) return null;
  const list = parse(part.body);
  if (!Array.isArray(list)) return [{ name: "", text: part.body }];
  const appended = new Set(appendedTools(parts));
  return list.flatMap((tool): DeclaredTool[] => {
    const record = asRecord(tool);
    const name = text(record?.name);
    if (appended.has(name)) return [];
    if (!record) return [{ name: "", text: pretty(tool) }];
    const definition = Object.fromEntries(Object.entries(record).filter(([field]) => field !== "name"));
    return [{ name, text: `${name}\n${pretty(definition)}` }];
  });
}

/**
 * The tools a turn's request declared, when the list is new or differs from
 * the one the request before it declared.
 *
 * Like the system prompt it is not a row of the timeline — the host composes it
 * for every request — so it is read from the payloads. It is no message either:
 * it is the request's `tools` field, ahead of the system prompt in what the model
 * reads, and it is tagged as that field rather than with a role it does not
 * have. A tool handed over by append is not part of it: the list stays as it
 * was, which is the point of appending.
 */
export function describeTools(
  before: readonly HistoryPart[] | null,
  after: readonly HistoryPart[],
  key: string,
  labels: EventLabels
): MessageRow[] {
  const now = declaredTools(after);
  const then = before ? declaredTools(before) : null;
  if (!now && !then) return [];
  const body = (tools: DeclaredTool[] | null) => (tools ?? []).map((tool) => tool.text).join("\n\n");
  const nowText = body(now);
  const thenText = body(then);
  if (now && then && nowText === thenText) return [];
  const shown = now ?? then ?? [];
  const change: HistoryOp["op"] = now && then ? "replace" : now ? "insert" : "remove";
  const truncated = [after, before ?? []].some((parts) =>
    parts.some((part) => part.kind === "tools" && part.truncated)
  );
  let preview = shown.map((tool) => tool.name).filter(Boolean).join(", ");
  if (now && then) {
    // What a reader of a changed list is after is which tools it is about.
    const previous = new Map(then.map((tool) => [tool.name, tool.text]));
    const current = new Set(now.map((tool) => tool.name));
    const added = now.filter((tool) => !previous.has(tool.name)).map((tool) => tool.name);
    const removed = then.filter((tool) => !current.has(tool.name)).map((tool) => tool.name);
    const rewritten = now
      .filter((tool) => previous.has(tool.name) && previous.get(tool.name) !== tool.text)
      .map((tool) => tool.name);
    preview = [
      added.length ? `+${added.join(", ")}` : "",
      removed.length ? `−${removed.join(", ")}` : "",
      rewritten.length ? `~${rewritten.join(", ")}` : ""
    ]
      .filter(Boolean)
      .join(" ");
  }
  const diff = now && then ? lineDiff(thenText, nowText, { path: "tools" }) : null;
  return [
    {
      key: `${key}-tools`,
      change,
      kind: "tools",
      label: "tools",
      detail: labels.toolsRegistered(shown.length),
      badges: truncated ? [{ label: labels.truncated, tone: "warning" }] : [],
      preview: collapse(preview),
      text: change === "remove" ? thenText : nowText,
      patch: diff?.patch ?? "",
      additions: diff?.additions ?? 0,
      deletions: diff?.deletions ?? 0
    }
  ];
}

/** What kind of thing an event row stands for, for its colour. */
export type EventTone = "response" | "hook" | "tool" | "result";

/** A short tag on a row: what a hook decided, whether a call was rewritten. */
export interface EventBadge {
  label: string;
  /** `hook` for a hook's doing, `warning` for a refusal, a failure or a cut. */
  tone: "hook" | "warning" | "neutral";
}

/** One entry of a run that never settled, described well enough to draw a row for it. */
export interface DescribedEvent {
  tone: EventTone;
  /** The role it plays: `assistant` for a response, `tool` for a call and its result, `hook`. */
  label: RowRole | "";
  /** What follows the label: which tool, which hook on which event, the tools a response called. */
  detail: string;
  preview: string;
  badges: EventBadge[];
  /** What the row shows when it opens. */
  text: string;
  /** A unified diff to show instead of `text`, when the entry is a rewrite. */
  patch: string;
}

/** What a message row stands for, for its colour. */
export type MessageKind =
  | "user"
  | "assistant"
  | "reasoning"
  | "system"
  | "toolsAdded"
  | "tools"
  | "tool"
  | "prompt";

/**
 * The tag a row leads with: the role its message plays, as the timeline and the
 * wire name it. It is the same word in a turn, an edit and a cut-off turn, and in
 * either language; what else the row is (which tool, which hook, whose prompt) is
 * its detail.
 *
 * `tools` is the one tag that is not a role: the tool list a request declares is
 * no message, and nobody speaks it. It is tagged with the request field it is.
 */
export type RowRole = "system" | "user" | "assistant" | "reasoning" | "tool" | "hook" | "tools";

/** One message a bar added, removed or rewrote. */
export interface MessageRow {
  key: string;
  /** What happened to it: a message put there, one taken away, or one rewritten. */
  change: HistoryOp["op"];
  kind: MessageKind;
  /** Its role; empty for a row the record never saw, whose role it cannot know. */
  label: RowRole | "";
  detail: string;
  badges: EventBadge[];
  preview: string;
  /** The message as it reads after the change, or as it read before a removal. */
  text: string;
  /** Unified diff of a rewrite; empty otherwise. */
  patch: string;
  /**
   * Lines a rewrite added and took away. Only a rewrite has them: a message put
   * there or taken away whole is said by its colour, and a count of its lines
   * would read as an edit it never had.
   */
  additions: number;
  deletions: number;
}

/**
 * Labels the renderer localises; the reading model stays language-neutral. The
 * roles rows are tagged with are not among them: they are the timeline's words.
 */
export interface EventLabels {
  /** What a system message that handed the model new tools says of them. */
  toolsAdded: (count: number) => string;
  /** What the tool list a request declared says of itself. */
  toolsRegistered: (count: number) => string;
  localOnly: string;
  interrupted: string;
  /** Said of a removed message the record never saw the body of. */
  unseen: string;
  rewroteInput: string;
  addedContext: string;
  blocked: string;
  halted: string;
  failed: string;
  denied: string;
  permission: (decision: string) => string;
  truncated: string;
  paused: string;
  images: (count: number) => string;
  files: (count: number) => string;
}

function prettyBody(body: string | undefined): string {
  if (body === undefined) return "";
  const value = parse(body);
  return value === null ? body : pretty(value);
}

type RowBody = Omit<MessageRow, "key" | "change" | "patch" | "additions" | "deletions">;

/** A timeline row — a `ContextItem` as JSON — as the message row it draws. */
function describeContextRow(body: string, labels: EventLabels): RowBody {
  const row = asRecord(parse(body));
  if (!row) {
    return { kind: "system", label: "", detail: "", badges: [], preview: collapse(body), text: body };
  }
  const kind = text(row.kind);
  const content = typeof row.content === "string" ? row.content : row.content === undefined ? "" : pretty(row.content);
  const badges: EventBadge[] = [];
  if (row.interrupted === true) badges.push({ label: labels.interrupted, tone: "warning" });
  if (kind === "tool") {
    const result = asRecord(row.result);
    const output = text(result?.output);
    if ("requestedInput" in row) badges.push({ label: labels.rewroteInput, tone: "hook" });
    if (result?.success === false) badges.push({ label: labels.failed, tone: "warning" });
    // The call's arguments, then what came back, each as the model reads it:
    // the result follows untitled, so a `box` delivery reads as the message it
    // is. What a hook rewrote comes last, apart from the exchange.
    const sections = [pretty(row.input)];
    if (output) sections.push(output);
    if ("requestedInput" in row) sections.push(`[requestedInput]\n${pretty(row.requestedInput)}`);
    return {
      kind: "tool",
      label: "tool",
      detail: text(row.toolName),
      badges,
      preview: collapse(JSON.stringify(row.input) ?? ""),
      text: sections.join("\n\n")
    };
  }
  if (kind === "user") {
    const images = Array.isArray(row.images) ? row.images.length : 0;
    const files = Array.isArray(row.files) ? row.files.length : 0;
    if (images) badges.push({ label: labels.images(images), tone: "neutral" });
    if (files) badges.push({ label: labels.files(files), tone: "neutral" });
    return { kind: "user", label: "user", detail: "", badges, preview: collapse(content), text: content };
  }
  if (kind === "assistant") {
    return { kind: "assistant", label: "assistant", detail: "", badges, preview: collapse(content), text: content };
  }
  if (kind === "reasoning") {
    return { kind: "reasoning", label: "reasoning", detail: "", badges, preview: collapse(content), text: content };
  }
  if (kind === "system" && Array.isArray(row.toolsAdded)) {
    const tools = row.toolsAdded.map(text).filter(Boolean);
    return {
      kind: "toolsAdded",
      label: "system",
      detail: labels.toolsAdded(tools.length),
      badges,
      preview: tools.join(", "),
      text: content || tools.join("\n")
    };
  }
  if (row.localOnly === true) badges.push({ label: labels.localOnly, tone: "neutral" });
  const hook = asRecord(row.hookExecution);
  // A system message: one the user put there, or one a hook added. A row of no
  // kind the timeline knows gets no role rather than a wrong one.
  return {
    kind: "system",
    label: kind === "system" ? "system" : "",
    detail: text(hook?.hookName),
    badges,
    preview: collapse(content),
    text: content
  };
}

/**
 * The messages one change to the timeline added, removed and rewrote, in the
 * order it made them.
 *
 * A removal is drawn as the message that went, read from the body the record
 * last had for that row; a rewrite as the message it became, opening into the
 * diff. A message a run left empty — the shell of a round that only called
 * tools — says nothing and is left out.
 */
export function describeChange(
  entry: HistoryEntry,
  detail: HistoryEntryDetail,
  labels: EventLabels
): MessageRow[] {
  return (detail.ops ?? []).flatMap((op): MessageRow[] => {
    const after = op.body === undefined ? null : describeContextRow(op.body, labels);
    const before = op.before === undefined ? null : describeContextRow(op.before, labels);
    const shown: RowBody =
      (op.op === "remove" ? before : after)
      ?? { kind: "system", label: "", detail: "", badges: [], preview: labels.unseen, text: labels.unseen };
    if (op.op === "insert" && shown.kind === "assistant" && !shown.text && !shown.badges.length) return [];
    // The diff card's header is this path, so it names the row as its tag does.
    const diff =
      op.op === "replace" && before && after
        ? lineDiff(before.text, after.text, { path: [after.label, op.contextId].filter(Boolean).join("#") })
        : null;
    return [
      {
        key: `${entry.seq}-${op.ordinal}`,
        change: op.op,
        ...shown,
        patch: diff?.patch ?? "",
        additions: diff?.additions ?? 0,
        deletions: diff?.deletions ?? 0
      }
    ];
  });
}

/**
 * The system prompt a turn opened with, when it is new or differs from the one
 * the request before it carried.
 *
 * It is not a row of the timeline — the host composes it for every request — so
 * it is read from the payloads themselves: the turn's first request against the
 * one that went out before it. `before` is `null` for a turn with no request
 * before it, where the prompt is new as a matter of fact.
 *
 * The static part and the dynamic part are both the model's system message, so
 * both are tagged `system`, as the timeline's own system rows are.
 */
export function describePrompts(
  before: readonly HistoryPart[] | null,
  after: readonly HistoryPart[],
  key: string,
  labels: EventLabels
): MessageRow[] {
  return (["system", "systemDynamic"] as const).flatMap((kind): MessageRow[] => {
    const now = after.find((part) => part.kind === kind);
    const then = before?.find((part) => part.kind === kind);
    if (!now && !then) return [];
    if (now && then && now.hash === then.hash) return [];
    const shown = now ?? then;
    if (!shown) return [];
    const change: HistoryOp["op"] = now && then ? "replace" : now ? "insert" : "remove";
    const badges: EventBadge[] = shown.truncated ? [{ label: labels.truncated, tone: "warning" }] : [];
    const diff = now && then ? lineDiff(then.body, now.body, { path: "system" }) : null;
    return [
      {
        key: `${key}-${kind}`,
        change,
        kind: "prompt",
        label: "system",
        detail: "",
        badges,
        preview: collapse(shown.body),
        text: shown.body,
        patch: diff?.patch ?? "",
        additions: diff?.additions ?? 0,
        deletions: diff?.deletions ?? 0
      }
    ];
  });
}

/**
 * Describes one entry of a run that never settled: a response, a hook's
 * decision, a call as it ran, what it returned. `detail` is its loaded body, when
 * it has been read; before that the row still says what it is, from the metadata
 * the list carries.
 */
export function describeEvent(
  entry: HistoryEntry,
  detail: HistoryEntryDetail | null | undefined,
  labels: EventLabels
): DescribedEvent {
  const meta = entry.detail;
  const body = detail?.body;
  const empty: DescribedEvent = {
    tone: "result",
    label: "",
    detail: "",
    preview: "",
    badges: [],
    text: prettyBody(body),
    patch: ""
  };
  if (entry.kind === "response") {
    const described =
      body === undefined || !asRecord(parse(body)) ? null : describeMessage(body);
    const finish = detailString(meta, "rawFinishReason") || detailString(meta, "finishReason");
    const badges: EventBadge[] = [];
    if (finish === "pause_turn") badges.push({ label: labels.paused, tone: "neutral" });
    if (finish === "length" || finish === "max_tokens") {
      badges.push({ label: labels.truncated, tone: "warning" });
    }
    return {
      ...empty,
      tone: "response",
      label: "assistant",
      detail: described?.detail ?? "",
      preview: described?.preview ?? "",
      badges,
      text: described?.text ?? prettyBody(body)
    };
  }
  if (entry.kind === "hook") {
    const badges: EventBadge[] = [];
    if (detailFlag(meta, "halted")) badges.push({ label: labels.halted, tone: "warning" });
    else if (detailFlag(meta, "blocked")) badges.push({ label: labels.blocked, tone: "hook" });
    const permission = detailString(meta, "permission");
    if (permission && !detailFlag(meta, "blocked")) {
      badges.push({ label: labels.permission(permission), tone: "hook" });
    }
    if (detailFlag(meta, "rewroteInput")) badges.push({ label: labels.rewroteInput, tone: "hook" });
    if (detailFlag(meta, "addedContext")) badges.push({ label: labels.addedContext, tone: "hook" });
    if (meta.success === false) badges.push({ label: labels.failed, tone: "warning" });
    const record = body === undefined ? null : asRecord(parse(body));
    const sections: string[] = [];
    for (const key of ["reason", "systemMessage", "additionalContext"]) {
      if (typeof record?.[key] === "string") sections.push(`[${key}]\n${record[key] as string}`);
    }
    if (record && "updatedInput" in record) sections.push(`[updatedInput]\n${pretty(record.updatedInput)}`);
    if (typeof record?.output === "string" && record.output) sections.push(`[output]\n${record.output}`);
    const matcher = detailString(meta, "matcher");
    return {
      ...empty,
      tone: "hook",
      label: "hook",
      detail: [detailString(meta, "event"), detailString(meta, "hookName"), matcher].filter(Boolean).join(" · "),
      preview: collapse(sections[0]?.replace(/^\[[^\]]+\]\n/, "") ?? ""),
      badges,
      text: sections.join("\n\n") || prettyBody(body)
    };
  }
  if (entry.kind === "tool") {
    const record = body === undefined ? null : asRecord(parse(body));
    const input = record ? pretty(record.input) : "";
    const badges: EventBadge[] = [];
    if (detailFlag(meta, "rewritten")) badges.push({ label: labels.rewroteInput, tone: "hook" });
    const denied = detailString(meta, "denied");
    if (denied) badges.push({ label: labels.denied, tone: "warning" });
    const patch =
      record && "requestedInput" in record
        ? lineDiff(pretty(record.requestedInput), input, { path: detailString(meta, "name") }).patch
        : "";
    return {
      ...empty,
      tone: "tool",
      label: "tool",
      detail: detailString(meta, "name"),
      preview: collapse(record ? JSON.stringify(record.input) ?? "" : ""),
      badges,
      text: [input, denied ? `[denied]\n${denied}` : ""].filter(Boolean).join("\n\n"),
      patch
    };
  }
  if (entry.kind === "result") {
    const record = body === undefined ? null : asRecord(parse(body));
    const output = text(record?.output);
    const badges: EventBadge[] = [];
    if (meta.success === false) badges.push({ label: labels.failed, tone: "warning" });
    const images = detailNumber(meta, "images");
    if (images) badges.push({ label: labels.images(images), tone: "neutral" });
    return {
      ...empty,
      tone: "result",
      label: "tool",
      detail: detailString(meta, "name"),
      preview: collapse(output),
      badges,
      text: output || prettyBody(body)
    };
  }
  return empty;
}
