import { ArrowUpRight, ChevronRight, History, MessageSquare } from "lucide-react";
import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { listWireRequests, loadWireRequest } from "../lib/runtime";
import type { WireRequestDetail, WireRequestSummary, WireUsage } from "../lib/runtime";
import {
  anyTruncated,
  ledgerRounds,
  rawPayload,
  readTurn,
  wireUsageIsEmpty
} from "../lib/wireLedger";
import type { LedgerEntry, LedgerRequestGroup, LedgerRound, TurnReading } from "../lib/wireLedger";
import { DiffOutput } from "./DiffOutput";
import "./HistoryPane.css";

export interface HistoryPaneProps {
  conversationId: string;
  /** Change signal: a new array means the trunk moved, so the list is refetched. */
  contexts: unknown[];
  /** Rows appear as they are sent, so a running turn is polled rather than awaited. */
  streaming: boolean;
  /**
   * Which of the conversation's ledgers to read: omitted is the session's own,
   * and a child agent's ledger address is that agent's — its name for a spawned
   * agent, the host's run-scoped address for a workflow step. A child runs
   * under its parent's conversation id, so this is the only thing that
   * separates them.
   */
  owners?: readonly string[];
}

/** How often a running turn's new requests are picked up. */
const STREAMING_POLL_MS = 1000;

function formatBytes(bytes: number): string {
  // A size the host did not report is shown as unknown rather than as a number
  // that is not one: this pane is read for what it can vouch for.
  if (!Number.isFinite(bytes)) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Token counts sit in a 10px column, so they are shortened rather than wrapped. */
function formatTokens(tokens: number): string {
  if (tokens < 1000) return String(tokens);
  if (tokens < 10_000) return `${(tokens / 1000).toFixed(1)}k`;
  if (tokens < 1_000_000) return `${Math.round(tokens / 1000)}k`;
  return `${(tokens / 1_000_000).toFixed(1)}M`;
}

function formatTime(createdAt: string): string {
  const at = new Date(createdAt);
  if (Number.isNaN(at.getTime())) return "";
  return at.toLocaleString(undefined, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  });
}

/**
 * What a turn, or one request inside it, cost — in the three numbers a reader of
 * this pane is after.
 *
 * A counter the provider never disclosed stays out rather than reading as zero,
 * and a request whose response never came back says so — the same rule the byte
 * column follows.
 */
function Usage({ usage }: { usage: WireUsage | undefined }) {
  const { t } = useI18n();
  if (!usage || wireUsageIsEmpty(usage)) {
    return (
      <span className="history-pane__change" title={t("没有记录到用量", "No usage was recorded")}>
        —
      </span>
    );
  }
  return (
    <span
      className="history-pane__change"
      title={t(
        "输入 {in} · 其中缓存命中 {cache} · 输出 {out}",
        "Input {in} · of which cached {cache} · output {out}",
        {
          in: usage.inputTokens ?? "—",
          cache: usage.cachedInputTokens ?? "—",
          out: usage.outputTokens ?? "—"
        }
      )}
    >
      {usage.inputTokens !== undefined && `↑${formatTokens(usage.inputTokens)}`}
      {usage.cachedInputTokens !== undefined && ` ⚡${formatTokens(usage.cachedInputTokens)}`}
      {usage.outputTokens !== undefined && ` ↓${formatTokens(usage.outputTokens)}`}
    </span>
  );
}

/**
 * How a part stands against the payload before it, in the terms the pane paints.
 *
 * A message the model produced is not a change the reader has to inspect — it
 * is what the round was for — so it is left uncoloured even though it is an
 * addition. Only what the person did gets a colour. What the wire cannot answer
 * it does not guess: a message the user typed into the timeline by hand as the
 * assistant is indistinguishable from one the model wrote, and reads as output.
 */
type EntryTone = "kept" | "output" | "user-added" | "removed" | "changed";

function entryTone(entry: LedgerEntry): EntryTone {
  if (entry.status === "removed") return "removed";
  if (entry.status === "changed") return "changed";
  if (entry.status === "kept") return "kept";
  return entry.part.author === "user" ? "user-added" : "output";
}

interface PartRowProps {
  entry: LedgerEntry;
  open: boolean;
  /** True behind the turn's rule: replayed history rather than what it added. */
  cached?: boolean;
  onToggle: () => void;
  children: ReactNode;
}

/** One single-line row for one part of a payload, opening in place. */
function PartRow({ entry, open, cached, onToggle, children }: PartRowProps) {
  const { t } = useI18n();
  const { part } = entry;
  const tone = entryTone(entry);
  return (
    <li className="history-pane__part">
      <button
        type="button"
        className="history-pane__row history-pane__row--part"
        data-tone={tone}
        data-cached={cached || undefined}
        aria-expanded={open}
        onClick={onToggle}
      >
        <ChevronRight
          size={12}
          className="history-pane__chevron"
          data-open={open || undefined}
          aria-hidden="true"
        />
        <span className="history-pane__role" data-role={part.role}>
          {part.role}
        </span>
        {part.detail && <span className="history-pane__detail">{part.detail}</span>}
        <span className="history-pane__preview">{part.preview}</span>
        {entry.status === "kept" ? (
          <span className="history-pane__meta">{formatBytes(part.bytes)}</span>
        ) : (
          <span className="history-pane__stat" data-status={entry.status}>
            {t("+{added} −{removed} 行", "+{added} −{removed} lines", {
              added: entry.additions,
              removed: entry.deletions
            })}
          </span>
        )}
      </button>
      {open && children}
    </li>
  );
}

interface EntryListProps {
  entries: readonly LedgerEntry[];
  rowKey: string;
  openParts: Set<string>;
  cached?: boolean;
  onTogglePart: (key: string) => void;
}

/** The rows for one group of parts: the prompts, the history, or one request. */
function EntryList({ entries, rowKey, openParts, cached, onTogglePart }: EntryListProps) {
  const { t } = useI18n();
  return (
    <>
      {entries.map((entry) => {
        const key = `${rowKey}:${entry.key}`;
        return (
          <PartRow
            key={key}
            entry={entry}
            open={openParts.has(key)}
            cached={cached}
            onToggle={() => onTogglePart(key)}
          >
            {entry.status === "changed" && entry.patch ? (
              <div className="history-pane__diff">
                <DiffOutput value={entry.patch} />
              </div>
            ) : (
              <pre className="history-pane__text">
                {entry.part.text}
                {entry.part.truncated
                  ? `\n\n${t("（记录时已截断）", "(truncated when recorded)")}`
                  : ""}
              </pre>
            )}
          </PartRow>
        );
      })}
    </>
  );
}

interface RequestRuleProps {
  group: LedgerRequestGroup;
  detail: WireRequestDetail;
  /** True when the turn did not run every one of its requests on one model. */
  showModel: boolean;
  open: boolean;
  onToggle: () => void;
}

/**
 * The rule where one request left.
 *
 * Everything above it, down to the rule before it, is what that payload was the
 * first to carry. It opens into the payload itself, so every request a turn
 * issued still has its bytes one click away: a pane that drew only the turn's
 * last payload would have stopped being a ledger.
 *
 * A component rather than a helper so re-serialising the payload can be
 * memoised on the detail it comes from. The list refetches once a second while
 * a turn runs; without this, every tick would redo it for every open rule.
 */
function RequestRule({ group, detail, showModel, open, onToggle }: RequestRuleProps) {
  const { t } = useI18n();
  const payload = useMemo(() => JSON.stringify(rawPayload(detail), null, 2), [detail]);
  const { summary } = group;
  const kind =
    summary.kind === "search"
      ? t("原生搜索", "Native search")
      : summary.kind === "fetch"
        ? t("原生抓取", "Native fetch")
        : "";
  return (
    <li className="history-pane__part">
      <button
        type="button"
        className="history-pane__row history-pane__row--rule"
        aria-expanded={open}
        title={t(
          "第 {seq} 次请求 · {size} · 展开为发出去的原始 JSON",
          "Request {seq} · {size} · opens the raw payload as sent",
          { seq: summary.seq, size: formatBytes(summary.bytes) }
        )}
        onClick={onToggle}
      >
        <ChevronRight
          size={12}
          className="history-pane__chevron"
          data-open={open || undefined}
          aria-hidden="true"
        />
        <ArrowUpRight size={11} className="history-pane__glyph" aria-hidden="true" />
        {/* The store's own sequence, not a position in the list: retention
            prunes from the front, and a request must not be renumbered by the
            disappearance of one that came before it. */}
        <span className="history-pane__index">{summary.seq}</span>
        {kind && <span className="history-pane__rule-label">{kind}</span>}
        {showModel && <span className="history-pane__rule-label">{summary.modelId}</span>}
        {summary.attempt > 1 && (
          <span className="history-pane__badge">
            {t("第 {n} 次尝试", "attempt {n}", { n: summary.attempt })}
          </span>
        )}
        <span className="history-pane__rule-line" aria-hidden="true" />
        <Usage usage={summary.usage} />
        <span className="history-pane__meta">{formatBytes(summary.bytes)}</span>
        <span className="history-pane__time">{formatTime(summary.createdAt)}</span>
      </button>
      {open && <pre className="history-pane__text history-pane__text--json">{payload}</pre>}
    </li>
  );
}

interface TurnBodyProps {
  reading: TurnReading;
  round: LedgerRound;
  details: Record<number, WireRequestDetail | null>;
  /** True when a body this turn is read from was cut at record time. */
  truncated: boolean;
  rowKey: string;
  openParts: Set<string>;
  onTogglePart: (key: string) => void;
}

/**
 * One turn as a single flat list: its prompts, the history it replayed folded
 * behind one rule, and then everything it put on the wire, ruled off under each
 * request that left.
 *
 * The history folds because every request replays all of it; without the fold
 * the two or three parts worth reading would sit under hundreds the reader has
 * already seen. It opens *above* its own rule rather than below, so the rule
 * stays what it says it is — the line between what was carried over and what
 * this turn added.
 */
function TurnBody({
  reading,
  round,
  details,
  truncated,
  rowKey,
  openParts,
  onTogglePart
}: TurnBodyProps) {
  const { t } = useI18n();
  const cachedKey = `${rowKey}:cached`;
  const cachedOpen = openParts.has(cachedKey);
  const rule = useRef<HTMLLIElement | null>(null);
  /** Where the rule sat when the reader clicked it, so it can be put back. */
  const anchored = useRef<number | null>(null);

  function toggleCached() {
    anchored.current = rule.current?.getBoundingClientRect().top ?? null;
    onTogglePart(cachedKey);
  }

  useLayoutEffect(() => {
    const top = anchored.current;
    anchored.current = null;
    const row = rule.current;
    if (top === null || !row) return;
    const scroller = row.closest(".history-pane");
    // The history opens above its own rule, and the browser's own scroll
    // anchoring does not catch an insertion made by a re-render: without this,
    // opening a few hundred replayed messages would shove the rows the reader
    // was actually looking at off the bottom of the pane.
    if (scroller) scroller.scrollTop += row.getBoundingClientRect().top - top;
  }, [cachedOpen]);

  const unchanged =
    reading.comparable
    && reading.groups.every((group) => !group.entries.length)
    && reading.prompts.every((entry) => entry.status === "kept");
  return (
    <ol className="history-pane__parts">
      <EntryList
        entries={reading.prompts}
        rowKey={rowKey}
        openParts={openParts}
        onTogglePart={onTogglePart}
      />
      {reading.cached.length > 0 && (
        <>
          {cachedOpen && (
            <EntryList
              entries={reading.cached}
              rowKey={cachedKey}
              openParts={openParts}
              cached
              onTogglePart={onTogglePart}
            />
          )}
          <li className="history-pane__part" ref={rule}>
            <button
              type="button"
              className="history-pane__row history-pane__row--rule"
              aria-expanded={cachedOpen}
              title={t(
                "这一轮原样重放的历史，展开在这条线上方",
                "The history this turn replayed untouched, opening above this rule"
              )}
              onClick={toggleCached}
            >
              <ChevronRight
                size={12}
                className="history-pane__chevron"
                data-open={cachedOpen || undefined}
                aria-hidden="true"
              />
              <History size={11} className="history-pane__glyph" aria-hidden="true" />
              <span className="history-pane__rule-label">
                {t("此前 {n} 条消息", "{n} earlier messages", { n: reading.cached.length })}
              </span>
              <span className="history-pane__rule-line" aria-hidden="true" />
            </button>
          </li>
        </>
      )}
      {unchanged && (
        // A retry re-sends the same bytes, and so does a round the host repeated
        // for a continuation. Saying so is the point: "nothing new" is a finding.
        <li className="history-pane__part">
          <p className="history-pane__empty">
            {truncated
              ? t(
                  "与上一次请求相比，记录下来的部分没有差异；其中有正文在记录时被截断，更长处的改动看不到。",
                  "No difference from the request before it in what was recorded; a body was cut at record time, so a change past the cap cannot be seen."
                )
              : t(
                  "这一轮发出去的内容和上一次请求完全相同。",
                  "This turn carried exactly what the request before it carried."
                )}
          </p>
        </li>
      )}
      {reading.groups.map((group) => {
        const detail = details[group.summary.seq];
        const groupKey = `${rowKey}:${group.key}`;
        return (
          <Fragment key={group.key}>
            <EntryList
              entries={group.entries}
              rowKey={groupKey}
              openParts={openParts}
              onTogglePart={onTogglePart}
            />
            {detail && (
              <RequestRule
                group={group}
                detail={detail}
                showModel={round.mixedModels}
                open={openParts.has(groupKey)}
                onToggle={() => onTogglePart(groupKey)}
              />
            )}
          </Fragment>
        );
      })}
    </ol>
  );
}

/**
 * The conversation's outgoing requests, oldest first, grouped into the turns
 * that issued them.
 *
 * A turn opens into the payload it built: the history it replayed, folded, and
 * then every part it put on the wire, ruled off at each request that left.
 * Reading a turn against the payload before it is what says which of its
 * messages the rounds produced and which the user put there — so the pane
 * accounts for everything that was sent, not only for what the conversation
 * kept afterwards.
 */
export function HistoryPane({ conversationId, contexts, streaming, owners }: HistoryPaneProps) {
  const { t } = useI18n();
  const [requests, setRequests] = useState<WireRequestSummary[] | null>(null);
  const [details, setDetails] = useState<Record<number, WireRequestDetail | null>>({});
  const [detailErrors, setDetailErrors] = useState<Record<number, string>>({});
  const [openRows, setOpenRows] = useState<Set<string>>(new Set());
  const [openParts, setOpenParts] = useState<Set<string>>(new Set());
  const [failure, setFailure] = useState<string | null>(null);
  /** Requests already read or in flight, so an expansion never reads twice. */
  const requested = useRef<Set<number>>(new Set());
  /** The conversation the pane is on right now, for discarding late reads. */
  const latestConversation = useRef(conversationId);
  /**
   * Whether this conversation's newest turn has been opened for the reader.
   *
   * Every row here is a fold, so a pane that opened onto nothing but collapsed
   * headers would hide the thing it was opened to look at. Done once per
   * conversation: a reader who closes it has closed it.
   */
  const primed = useRef(false);
  /**
   * One reading per turn, computed at most once per shape.
   *
   * A loaded detail never changes, so the sequence numbers that went into a
   * reading identify it completely — and a running turn gains requests, so the
   * entry is replaced rather than added to. Without this the alignment and
   * every line diff under it would recompute on every render, and the list
   * re-renders once a second while a turn is running.
   */
  const readingCache = useRef<Map<string, { signature: string; reading: TurnReading }>>(new Map());
  /**
   * Identity of the ledger being read, for the refetch dependency.
   *
   * The array is rebuilt on every render of the host, so it is compared by value
   * rather than by reference: by reference the pane would refetch once a render.
   */
  const ownersKey = owners === undefined ? null : JSON.stringify(owners);
  /** The same list, stable for as long as its contents are. */
  const ledgerOwners = useMemo(
    () => (ownersKey === null ? undefined : (JSON.parse(ownersKey) as string[])),
    [ownersKey]
  );

  useEffect(() => {
    latestConversation.current = conversationId;
    requested.current = new Set();
    readingCache.current = new Map();
    primed.current = false;
    setRequests(null);
    setDetails({});
    setDetailErrors({});
    setOpenRows(new Set());
    setOpenParts(new Set());
  }, [conversationId]);

  useEffect(() => {
    let cancelled = false;
    const read = () => {
      listWireRequests(conversationId, ledgerOwners).then(
        (next) => {
          if (cancelled) return;
          setRequests(next);
          setFailure(null);
        },
        (error: unknown) => {
          if (!cancelled) setFailure(String(error));
        }
      );
    };
    // Debounced so a burst of trunk changes costs one read, not one per row.
    const timer = setTimeout(read, 150);
    const poll = streaming ? setInterval(read, STREAMING_POLL_MS) : null;
    return () => {
      cancelled = true;
      clearTimeout(timer);
      if (poll !== null) clearInterval(poll);
    };
  }, [conversationId, contexts, ledgerOwners, streaming]);

  /**
   * Reads one request's parts once.
   *
   * The guard is a ref rather than a look at `details`, because a state updater
   * must stay a pure function of its input: React is allowed to run it twice,
   * and a fetch started from inside one would be issued twice with it.
   */
  const load = useCallback(
    (seq: number) => {
      if (requested.current.has(seq)) return;
      requested.current.add(seq);
      const conversation = conversationId;
      setDetails((current) => ({ ...current, [seq]: null }));
      loadWireRequest(conversation, seq).then(
        (detail) => {
          // A read that lands after the pane moved on belongs to a conversation
          // nobody is looking at; writing it here would show its rows under
          // another one's heading.
          if (conversation !== latestConversation.current) return;
          if (!detail) {
            // A turn is drawn from every one of its payloads at once, so a row
            // the store no longer holds has to be said out loud rather than
            // left as a read that never finishes.
            requested.current.delete(seq);
            setDetailErrors((next) => ({
              ...next,
              [seq]: t(
                "第 {seq} 次请求已不在账本里。",
                "Request {seq} is no longer in the ledger.",
                { seq }
              )
            }));
            return;
          }
          setDetails((next) => ({ ...next, [seq]: detail }));
        },
        (error: unknown) => {
          if (conversation !== latestConversation.current) return;
          // Held against the row, not the pane: one unreadable request must not
          // hide the ledger it belongs to.
          requested.current.delete(seq);
          setDetailErrors((next) => ({ ...next, [seq]: String(error) }));
        }
      );
    },
    [conversationId, t]
  );

  const retry = useCallback(
    (seq: number) => {
      setDetailErrors((next) => {
        const rest = { ...next };
        delete rest[seq];
        return rest;
      });
      load(seq);
    },
    [load]
  );

  const toggleRow = useCallback((key: string) => {
    setOpenRows((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const togglePart = useCallback((key: string) => {
    setOpenParts((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const rounds = useMemo(() => ledgerRounds(requests ?? []), [requests]);
  /** Sequence number of the request before each one, by position in the list. */
  const predecessors = useMemo(() => {
    const map = new Map<number, number>();
    (requests ?? []).forEach((summary, index) => {
      const previous = (requests ?? [])[index - 1];
      if (previous) map.set(summary.seq, previous.seq);
    });
    return map;
  }, [requests]);

  /** Every payload an open turn is drawn from, including the one before it. */
  const turnSeqs = useCallback(
    (round: LedgerRound): number[] => {
      const seqs = round.turns.map((turn) => turn.summary.seq);
      const before = predecessors.get(seqs[0]);
      return before === undefined ? seqs : [before, ...seqs];
    },
    [predecessors]
  );

  useEffect(() => {
    if (primed.current || !rounds.length) return;
    primed.current = true;
    const newest = rounds.at(-1);
    if (newest) setOpenRows(new Set([newest.key]));
  }, [rounds]);

  /**
   * Keeps every open turn's payloads read.
   *
   * A turn is drawn from all of them at once, and while it is still running new
   * ones join it row by row — so this follows the open set rather than the click
   * that opened it. A payload whose read failed is left alone until the reader
   * asks again: retrying it here would turn one unreadable row into a request a
   * second for as long as the turn runs.
   */
  useEffect(() => {
    for (const round of rounds) {
      if (!openRows.has(round.key)) continue;
      for (const seq of turnSeqs(round)) {
        if (detailErrors[seq] === undefined) load(seq);
      }
    }
  }, [rounds, openRows, turnSeqs, detailErrors, load]);

  if (failure) {
    return (
      <p className="history-pane__empty" role="alert">
        {failure}
      </p>
    );
  }
  if (requests === null) {
    return <p className="history-pane__empty">{t("正在读取请求账本…", "Reading the request log…")}</p>;
  }
  if (!requests.length) {
    return (
      <p className="history-pane__empty">
        {ledgerOwners
          ? t(
              "这个子代理还没有记录到发出去的请求。记录从下一次请求开始。",
              "No outgoing request has been recorded for this subagent yet. Recording starts with the next one."
            )
          : t(
              "这个对话还没有记录到发出去的请求。记录从下一次请求开始。",
              "No outgoing request has been recorded for this conversation yet. Recording starts with the next one."
            )}
      </p>
    );
  }

  /**
   * Whole messages the user added and removed before this turn's payloads went
   * out.
   *
   * Counted by the host against each predecessor's own parts, because reaching
   * the same number here would mean reading every recorded payload back. A turn
   * from before the ledger counted them draws nothing, which is not the same as
   * drawing a zero.
   */
  function renderCounts(added: number | null, removed: number | null): ReactNode {
    if (!added && !removed) return null;
    return (
      <span
        className="history-pane__counts"
        title={t(
          "用户手动新增 {added} 条、删除 {removed} 条消息",
          "{added} messages the user added, {removed} the user removed",
          { added: added ?? 0, removed: removed ?? 0 }
        )}
      >
        {added ? (
          <span className="history-pane__count" data-status="added">
            +{added}
          </span>
        ) : null}
        {removed ? (
          <span className="history-pane__count" data-status="removed">
            −{removed}
          </span>
        ) : null}
      </span>
    );
  }

  /** A turn's body while its payloads are still unread, or unreadable. */
  function pending(seqs: number[]): ReactNode {
    const failed = seqs.filter((seq) => detailErrors[seq] !== undefined);
    if (!failed.length) {
      return <p className="history-pane__empty">{t("正在读取…", "Reading…")}</p>;
    }
    return (
      <p className="history-pane__empty" role="alert">
        {detailErrors[failed[0]]}{" "}
        <button
          type="button"
          className="history-pane__retry"
          onClick={() => {
            for (const seq of failed) retry(seq);
          }}
        >
          {t("重试", "Retry")}
        </button>
      </p>
    );
  }

  /** The reading of one turn against the request before it, computed once. */
  function readingFor(round: LedgerRound): TurnReading {
    const seqs = round.turns.map((turn) => turn.summary.seq);
    const beforeSeq = predecessors.get(seqs[0]);
    const beforeDetail = beforeSeq === undefined ? null : details[beforeSeq] ?? null;
    // A turn that opens the conversation had an empty payload before it as a
    // matter of fact. Anything else with nothing to compare against is a
    // retention gap or a dropped row left behind, and the reading attributes
    // nothing rather than calling the whole history new.
    const atStart = seqs[0] === 1;
    const before = beforeDetail ? beforeDetail.parts : atStart ? [] : null;
    const signature = `${beforeDetail ? beforeSeq : atStart ? "start" : "none"}:${seqs.join(",")}`;
    const cached = readingCache.current.get(round.key);
    if (cached && cached.signature === signature) return cached.reading;
    const reading = readTurn(
      before,
      round.turns.map((turn) => ({
        summary: turn.summary,
        parts: (details[turn.summary.seq] as WireRequestDetail).parts
      }))
    );
    readingCache.current.set(round.key, { signature, reading });
    return reading;
  }

  function roundTitle(round: LedgerRound): string {
    return round.mixedModels
      ? t("{model} 等", "{model} and others", { model: round.modelId })
      : round.modelId;
  }

  function renderRound(round: LedgerRound) {
    const open = openRows.has(round.key);
    const seqs = round.turns.map((turn) => turn.summary.seq);
    const beforeSeq = predecessors.get(seqs[0]);
    const loaded = seqs.every((seq) => details[seq]);
    // The request before the turn is only needed to attribute its opening
    // payload; when it cannot be read the turn is still shown, with nothing
    // attributed.
    const waitingOnBefore =
      beforeSeq !== undefined && !details[beforeSeq] && detailErrors[beforeSeq] === undefined;
    return (
      <li className="history-pane__entry" key={round.key} data-kind="round">
        <button
          type="button"
          className="history-pane__row history-pane__row--round"
          aria-expanded={open}
          title={t("第 {n} 轮 · 发出 {count} 次请求", "Turn {n} · {count} requests", {
            n: round.index,
            count: round.turns.length
          })}
          onClick={() => toggleRow(round.key)}
        >
          <ChevronRight
            size={13}
            className="history-pane__chevron"
            data-open={open || undefined}
            aria-hidden="true"
          />
          <span className="history-pane__kind" data-kind="round">
            <MessageSquare size={12} aria-hidden="true" />
          </span>
          {/* No number of its own. The rules inside it carry the store's
              sequence, and a second counter out here would read as one of
              those — a turn's position in this list is not a request id. */}
          <span className="history-pane__title">{roundTitle(round)}</span>
          <Usage usage={round.usage} />
          {renderCounts(round.messagesAdded, round.messagesRemoved)}
          <span className="history-pane__time">{formatTime(round.createdAt)}</span>
        </button>
        {open && (
          <div className="history-pane__body">
            {!loaded || waitingOnBefore ? (
              pending(turnSeqs(round))
            ) : (
              <TurnBody
                reading={readingFor(round)}
                round={round}
                details={details}
                truncated={seqs.some((seq) => anyTruncated(details[seq]?.parts ?? []))}
                rowKey={round.key}
                openParts={openParts}
                onTogglePart={togglePart}
              />
            )}
          </div>
        )}
      </li>
    );
  }

  return (
    <div className="history-pane">
      <ol className="history-pane__list">{rounds.map(renderRound)}</ol>
    </div>
  );
}
