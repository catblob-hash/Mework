import { ChevronRight, MessageSquare, Pencil, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { listHistoryEntries, loadHistoryEntry } from "../lib/runtime";
import type { HistoryEntry, HistoryEntryDetail, HistoryUsage } from "../lib/runtime";
import {
  barItems,
  describeAppended,
  describeChange,
  describeEvent,
  describePrompts,
  describeTools,
  historyBars,
  usageIsEmpty
} from "../lib/historyRecord";
import type { BarKind, EventBadge, EventLabels, HistoryBar, MessageRow } from "../lib/historyRecord";
import { DiffOutput } from "./DiffOutput";
import "./HistoryPane.css";

export interface HistoryPaneProps {
  conversationId: string;
  /** Change signal: a new array means the trunk moved, so the list is refetched. */
  contexts: unknown[];
  /** Rows appear as they are sent, so a running turn is polled rather than awaited. */
  streaming: boolean;
  /**
   * Whose history to read: omitted is the session's own, and a child agent's
   * address is that agent's — its name for a spawned agent, the host's
   * run-scoped address for a workflow step. A child runs under its parent's
   * conversation id, so this is the only thing that separates them.
   */
  owners?: readonly string[];
}

/** How often a running turn's new entries are picked up. */
const STREAMING_POLL_MS = 1000;

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
 * What a turn cost — in the three numbers a reader of this pane is after.
 *
 * A counter the provider never disclosed stays out rather than reading as zero,
 * and a turn whose responses never came back says so.
 */
function Usage({ usage }: { usage: HistoryUsage | undefined }) {
  const { t } = useI18n();
  if (!usage || usageIsEmpty(usage)) {
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

const BAR_GLYPHS: Record<BarKind, LucideIcon> = {
  turn: MessageSquare,
  interrupted: TriangleAlert,
  edits: Pencil,
  pending: MessageSquare
};

interface RowProps {
  /** Distinguishes the two row shapes for their styles. */
  variant: "message" | "event";
  /** The role the row is tagged with; empty for none. */
  label: string;
  detail: string;
  badges: readonly EventBadge[];
  preview: string;
  /** What sits at the row's end: the line counts of a rewrite. */
  stat?: ReactNode;
  title?: string;
  data: Record<`data-${string}`, string | undefined>;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}

/**
 * One single-line row that opens in place. It leads with its role and nothing
 * else: a glyph beside it would be a second name for the same thing.
 */
function Row({
  variant,
  label,
  detail,
  badges,
  preview,
  stat,
  title,
  data,
  open,
  onToggle,
  children
}: RowProps) {
  return (
    <li className="history-pane__part">
      <button
        type="button"
        className={`history-pane__row history-pane__row--${variant}`}
        {...data}
        aria-expanded={open}
        title={title}
        onClick={onToggle}
      >
        <ChevronRight
          size={12}
          className="history-pane__chevron"
          data-open={open || undefined}
          aria-hidden="true"
        />
        {label && <span className="history-pane__role">{label}</span>}
        {detail && <span className="history-pane__detail">{detail}</span>}
        {badges.map((badge) => (
          <span key={badge.label} className="history-pane__badge" data-tone={badge.tone}>
            {badge.label}
          </span>
        ))}
        <span className="history-pane__preview">{preview}</span>
        {stat}
      </button>
      {open && children}
    </li>
  );
}

/** What an opened row shows: its diff when it is a rewrite, its text otherwise. */
function RowBody({ patch, text, truncated }: { patch: string; text: string; truncated?: boolean }) {
  const { t } = useI18n();
  if (patch) {
    return (
      <div className="history-pane__diff">
        <DiffOutput value={patch} />
      </div>
    );
  }
  return (
    <pre className="history-pane__text">
      {text}
      {truncated ? `\n\n${t("（记录时已截断）", "(truncated when recorded)")}` : ""}
    </pre>
  );
}

interface MessageRowsProps {
  rows: readonly MessageRow[];
  rowKey: string;
  openParts: Set<string>;
  onTogglePart: (key: string) => void;
}

/**
 * The lines a rewrite added and took away. A message put there or taken away
 * whole has none: its colour says what happened to it, and a count of its lines
 * would read as an edit it never had.
 */
function RewriteStat({ row }: { row: MessageRow }) {
  const { t } = useI18n();
  if (row.change !== "replace") return null;
  return (
    <span
      className="history-pane__stat"
      title={t("新增 {added} 行，删除 {removed} 行", "{added} lines added, {removed} removed", {
        added: row.additions,
        removed: row.deletions
      })}
    >
      <span data-status="added">+{row.additions}</span>{" "}
      <span data-status="removed">−{row.deletions}</span>
    </span>
  );
}

/** The messages one change or one prompt check produced, each a row of its own. */
function MessageRows({ rows, rowKey, openParts, onTogglePart }: MessageRowsProps) {
  return (
    <>
      {rows.map((row) => {
        const key = `${rowKey}:${row.key}`;
        return (
          <Row
            key={key}
            variant="message"
            label={row.label}
            detail={row.detail}
            badges={row.badges}
            preview={row.preview}
            stat={<RewriteStat row={row} />}
            data={{ "data-kind": row.kind, "data-change": row.change }}
            open={openParts.has(key)}
            onToggle={() => onTogglePart(key)}
          >
            <RowBody patch={row.patch} text={row.text} />
          </Row>
        );
      })}
    </>
  );
}

/** Where a read is still in flight, or failed and can be asked for again. */
function Waiting({ error, onRetry }: { error: string | undefined; onRetry: () => void }) {
  const { t } = useI18n();
  if (error === undefined) {
    return (
      <li className="history-pane__part">
        <p className="history-pane__empty">{t("正在读取…", "Reading…")}</p>
      </li>
    );
  }
  // Held where the entry would be, not over the pane: one unreadable entry must
  // not hide the history it belongs to.
  return (
    <li className="history-pane__part">
      <p className="history-pane__empty" role="alert">
        {error}{" "}
        <button type="button" className="history-pane__retry" onClick={onRetry}>
          {t("重试", "Retry")}
        </button>
      </p>
    </li>
  );
}

interface ChangeRowsProps extends Omit<MessageRowsProps, "rows"> {
  entry: HistoryEntry;
  detail: HistoryEntryDetail;
  labels: EventLabels;
}

/**
 * The messages one change to the timeline made. A component so the reading,
 * with a line diff for every rewrite, is done once per body rather than once a
 * second while a turn is running.
 */
function ChangeRows({ entry, detail, labels, ...rest }: ChangeRowsProps) {
  const rows = useMemo(() => describeChange(entry, detail, labels), [entry, detail, labels]);
  return <MessageRows rows={rows} {...rest} />;
}

interface PromptRowsProps extends Omit<MessageRowsProps, "rows"> {
  before: HistoryEntryDetail | null;
  after: HistoryEntryDetail;
  labels: EventLabels;
}

/**
 * The tool list and the system prompt a turn opened with, when either is new or
 * changed — in that order, the order the model reads them in.
 */
function PromptRows({ before, after, labels, rowKey, ...rest }: PromptRowsProps) {
  const rows = useMemo(() => {
    const key = `p${after.entry.seq}`;
    const previous = before?.parts ?? null;
    const current = after.parts ?? [];
    return [
      ...describeTools(previous, current, key, labels),
      ...describePrompts(previous, current, key, labels)
    ];
  }, [before, after, labels]);
  return <MessageRows rows={rows} rowKey={rowKey} {...rest} />;
}

/** The tools a request of a run that never settled was the first to hand over by append. */
function AppendedRows({ before, after, labels, rowKey, ...rest }: PromptRowsProps) {
  const rows = useMemo(
    () => describeAppended(before?.parts ?? null, after.parts ?? [], `a${after.entry.seq}`, labels),
    [before, after, labels]
  );
  return <MessageRows rows={rows} rowKey={rowKey} {...rest} />;
}

interface EventRowProps {
  entry: HistoryEntry;
  /** The loaded body; `null` while it is being read, absent before that. */
  detail: HistoryEntryDetail | null | undefined;
  /** Why the body could not be read, once a read has failed. */
  error: string | undefined;
  labels: EventLabels;
  open: boolean;
  onToggle: () => void;
  onRetry: () => void;
}

/**
 * One entry of a run that never settled: a response where it arrived, a hook's
 * decision, a call as it ran, what it returned. The row says what it is from the
 * list alone and fills in its preview once its body is read.
 */
function EventRow({ entry, detail, error, labels, open, onToggle, onRetry }: EventRowProps) {
  const { t } = useI18n();
  const described = useMemo(() => describeEvent(entry, detail, labels), [entry, detail, labels]);
  return (
    <Row
      variant="event"
      label={described.label}
      detail={described.detail}
      badges={described.badges}
      preview={described.preview}
      title={t("第 {seq} 条记录", "Entry {seq}", { seq: entry.seq })}
      data={{ "data-tone": described.tone }}
      open={open}
      onToggle={onToggle}
    >
      {error !== undefined ? (
        <p className="history-pane__empty" role="alert">
          {error}{" "}
          <button type="button" className="history-pane__retry" onClick={onRetry}>
            {t("重试", "Retry")}
          </button>
        </p>
      ) : !detail ? (
        <p className="history-pane__empty">{t("正在读取…", "Reading…")}</p>
      ) : (
        <RowBody patch={described.patch} text={described.text} truncated={detail.truncated} />
      )}
    </Row>
  );
}

/**
 * The conversation's history, oldest first, as the bars the things that
 * happened to it make.
 *
 * A turn the user started, a run of edits the user made to the context by hand,
 * and a turn that was cut off before its answer came back each stand as a bar of
 * their own, side by side. Each opens into the messages it added, removed or
 * rewrote, one row each in the order it made them — what the user sent, the
 * system prompt when it changed, what the model thought and said, each call with
 * what it returned, each tool it was handed on the way.
 */
export function HistoryPane({ conversationId, contexts, streaming, owners }: HistoryPaneProps) {
  const { t } = useI18n();
  const [entries, setEntries] = useState<HistoryEntry[] | null>(null);
  const [details, setDetails] = useState<Record<number, HistoryEntryDetail | null>>({});
  const [detailErrors, setDetailErrors] = useState<Record<number, string>>({});
  const [openRows, setOpenRows] = useState<Set<string>>(new Set());
  const [openParts, setOpenParts] = useState<Set<string>>(new Set());
  const [failure, setFailure] = useState<string | null>(null);
  /** Entries already read or in flight, so an expansion never reads twice. */
  const requested = useRef<Set<number>>(new Set());
  /** The conversation the pane is on right now, for discarding late reads. */
  const latestConversation = useRef(conversationId);
  /**
   * Whether this conversation's newest bar has been opened for the reader.
   *
   * Every row here is a fold, so a pane that opened onto nothing but collapsed
   * headers would hide the thing it was opened to look at. Done once per
   * conversation: a reader who closes it has closed it.
   */
  const primed = useRef(false);
  /**
   * Identity of the owners being read, for the refetch dependency.
   *
   * The array is rebuilt on every render of the host, so it is compared by value
   * rather than by reference: by reference the pane would refetch once a render.
   */
  const ownersKey = owners === undefined ? null : JSON.stringify(owners);
  /** The same list, stable for as long as its contents are. */
  const historyOwners = useMemo(
    () => (ownersKey === null ? undefined : (JSON.parse(ownersKey) as string[])),
    [ownersKey]
  );
  const labels = useMemo<EventLabels>(
    () => ({
      toolsAdded: (count) => t("追加 {n} 个工具", "{n} tools added", { n: count }),
      toolsRegistered: (count) => t("注册 {n} 个工具", "{n} tools registered", { n: count }),
      localOnly: t("仅本地", "local only"),
      interrupted: t("中断前的部分", "cut off"),
      unseen: t("（记录里没有这一行被删前的内容）", "(the record never saw this row before it went)"),
      rewroteInput: t("改写了输入", "rewrote input"),
      addedContext: t("注入上下文", "added context"),
      blocked: t("拦下", "blocked"),
      halted: t("中止", "halted"),
      failed: t("失败", "failed"),
      denied: t("被拒绝", "denied"),
      permission: (decision) =>
        decision === "allow"
          ? t("放行", "allowed")
          : decision === "ask"
            ? t("要求确认", "asked")
            : decision === "deny"
              ? t("拒绝", "denied")
              : decision,
      truncated: t("输出被截断", "cut off"),
      paused: t("暂停", "paused"),
      images: (count) => t("{n} 张图", "{n} images", { n: count }),
      files: (count) => t("{n} 个文件", "{n} files", { n: count })
    }),
    [t]
  );

  useEffect(() => {
    latestConversation.current = conversationId;
    requested.current = new Set();
    primed.current = false;
    setEntries(null);
    setDetails({});
    setDetailErrors({});
    setOpenRows(new Set());
    setOpenParts(new Set());
  }, [conversationId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: `contexts` is the change signal — a new array means the trunk moved — though nothing here reads it.
  useEffect(() => {
    let cancelled = false;
    const read = () => {
      listHistoryEntries(conversationId, historyOwners).then(
        (next) => {
          if (cancelled) return;
          setEntries(next);
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
  }, [conversationId, contexts, historyOwners, streaming]);

  /**
   * Reads one entry's body once.
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
      const fail = (message: string) => {
        requested.current.delete(seq);
        setDetails((next) => {
          const rest = { ...next };
          delete rest[seq];
          return rest;
        });
        setDetailErrors((next) => ({ ...next, [seq]: message }));
      };
      loadHistoryEntry(conversation, seq).then(
        (detail) => {
          // A read that lands after the pane moved on belongs to a conversation
          // nobody is looking at; writing it here would show its rows under
          // another one's heading.
          if (conversation !== latestConversation.current) return;
          if (!detail) {
            // An entry the store no longer holds has to be said out loud rather
            // than left as a read that never finishes.
            fail(
              t("第 {seq} 条记录已不在历史记录里。", "Entry {seq} is no longer in the history.", {
                seq
              })
            );
            return;
          }
          setDetails((next) => ({ ...next, [seq]: detail }));
        },
        (error: unknown) => {
          if (conversation !== latestConversation.current) return;
          fail(String(error));
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

  const bars = useMemo(() => historyBars(entries ?? [], { live: streaming }), [entries, streaming]);
  /** The model request before each one: what a turn's system prompt is read against. */
  const predecessors = useMemo(() => {
    const map = new Map<number, number>();
    let previous: number | undefined;
    for (const entry of entries ?? []) {
      if (entry.kind !== "request" || entry.detail.type === "search" || entry.detail.type === "fetch") {
        continue;
      }
      if (previous !== undefined) map.set(entry.seq, previous);
      previous = entry.seq;
    }
    return map;
  }, [entries]);

  /** The payloads a bar's system prompt is read from: its first model request, and the one before. */
  const promptSeqs = useCallback(
    (bar: HistoryBar): { first: number; before: number | undefined } | null => {
      const first = bar.requests.find((request) => request.kind === "model");
      return first ? { first: first.seq, before: predecessors.get(first.seq) } : null;
    },
    [predecessors]
  );

  useEffect(() => {
    if (primed.current || !bars.length) return;
    primed.current = true;
    const newest = bars.at(-1);
    if (newest) setOpenRows(new Set([newest.key]));
  }, [bars]);

  /**
   * Keeps every open bar's entries read.
   *
   * While a turn is still running new entries join it one by one — so this
   * follows the open set rather than the click that opened it. An entry whose
   * read failed is left alone until the reader asks again: retrying it here
   * would turn one unreadable entry into a request a second for as long as the
   * turn runs.
   */
  useEffect(() => {
    for (const bar of bars) {
      if (!openRows.has(bar.key)) continue;
      const prompt = promptSeqs(bar);
      const seqs = [
        ...(prompt ? [prompt.first, ...(prompt.before === undefined ? [] : [prompt.before])] : []),
        ...barItems(bar).flatMap((item) => {
          if (item.type !== "request") return [item.entry.seq];
          const before = predecessors.get(item.request.seq);
          return before === undefined ? [item.request.seq] : [item.request.seq, before];
        })
      ];
      for (const seq of seqs) {
        if (detailErrors[seq] === undefined) load(seq);
      }
    }
  }, [bars, openRows, promptSeqs, predecessors, detailErrors, load]);

  if (failure) {
    return (
      <p className="history-pane__empty" role="alert">
        {failure}
      </p>
    );
  }
  if (entries === null) {
    return <p className="history-pane__empty">{t("正在读取历史记录…", "Reading the history…")}</p>;
  }
  if (!entries.length) {
    return (
      <p className="history-pane__empty">
        {historyOwners
          ? t(
              "这个子代理还没有历史记录。记录从它发出的第一次请求开始。",
              "Nothing has been recorded for this subagent yet. Recording starts with its first request."
            )
          : t(
              "这个对话还没有历史记录。记录从下一次发送或编辑开始。",
              "Nothing has been recorded for this conversation yet. Recording starts with the next send or edit."
            )}
      </p>
    );
  }

  function modelName(bar: HistoryBar): string {
    return bar.mixedModels
      ? t("{model} 等", "{model} and others", { model: bar.modelId })
      : bar.modelId;
  }

  function barTitle(bar: HistoryBar): string {
    if (bar.kind === "edits") return t("上下文编辑", "Context edits");
    if (bar.kind === "interrupted") return t("意外中断", "Interrupted");
    if (bar.kind === "pending") return t("尚未发出", "Not sent yet");
    return modelName(bar);
  }

  function barTooltip(bar: HistoryBar): string {
    if (bar.kind === "edits") {
      return t("手动改动上下文 {n} 次", "{n} changes made to the context by hand", {
        n: bar.entries.length
      });
    }
    if (bar.kind === "pending") {
      return t("还没有请求带出去的内容", "What no request has carried yet");
    }
    if (bar.kind === "interrupted") {
      return t(
        "第 {n} 轮 · 最后一次请求没有等到回复",
        "Turn {n} · its last request never got an answer back",
        { n: bar.index }
      );
    }
    return t("第 {n} 轮 · 发出 {count} 次请求", "Turn {n} · {count} requests", {
      n: bar.index,
      count: bar.requests.length
    });
  }

  function renderPrompt(bar: HistoryBar): ReactNode {
    const prompt = promptSeqs(bar);
    if (!prompt) return null;
    const failed = [prompt.first, prompt.before].find(
      (seq): seq is number => seq !== undefined && detailErrors[seq] !== undefined
    );
    if (failed !== undefined) {
      return <Waiting key="prompt" error={detailErrors[failed]} onRetry={() => retry(failed)} />;
    }
    const after = details[prompt.first];
    const before = prompt.before === undefined ? null : details[prompt.before];
    if (!after || before === undefined || (prompt.before !== undefined && !before)) {
      return <Waiting key="prompt" error={undefined} onRetry={() => undefined} />;
    }
    return (
      <PromptRows
        key="prompt"
        before={before}
        after={after}
        labels={labels}
        rowKey={bar.key}
        openParts={openParts}
        onTogglePart={togglePart}
      />
    );
  }

  function renderBody(bar: HistoryBar): ReactNode {
    const items = barItems(bar);
    const prompt = renderPrompt(bar);
    if (!items.length && !prompt) {
      // A turn from before anything but its payloads was recorded. Saying so is
      // the point: an empty fold would read as a turn in which nothing happened.
      return (
        <p className="history-pane__empty">
          {t(
            "这一轮除了发出的请求，没有记录到别的条目。",
            "Nothing but the requests it sent was recorded for this turn."
          )}
        </p>
      );
    }
    return (
      <ol className="history-pane__parts" data-bar={bar.kind}>
        {prompt}
        {items.map((item) => {
          if (item.type === "request") {
            const seq = item.request.seq;
            const key = `${bar.key}:r${seq}`;
            const before = predecessors.get(seq);
            const after = details[seq];
            const previous = before === undefined ? null : details[before];
            // Nothing until both payloads are read: most requests hand over no
            // tool, and a placeholder for each would flicker through a running turn.
            if (!after || previous === undefined || (before !== undefined && previous === null)) return null;
            return (
              <AppendedRows
                key={key}
                before={previous}
                after={after}
                labels={labels}
                rowKey={key}
                openParts={openParts}
                onTogglePart={togglePart}
              />
            );
          }
          const { type, entry } = item;
          const key = `${bar.key}:e${entry.seq}`;
          if (type === "event") {
            return (
              <EventRow
                key={key}
                entry={entry}
                detail={details[entry.seq]}
                error={detailErrors[entry.seq]}
                labels={labels}
                open={openParts.has(key)}
                onToggle={() => togglePart(key)}
                onRetry={() => retry(entry.seq)}
              />
            );
          }
          const detail = details[entry.seq];
          if (!detail) {
            return (
              <Waiting key={key} error={detailErrors[entry.seq]} onRetry={() => retry(entry.seq)} />
            );
          }
          return (
            <ChangeRows
              key={key}
              entry={entry}
              detail={detail}
              labels={labels}
              rowKey={key}
              openParts={openParts}
              onTogglePart={togglePart}
            />
          );
        })}
      </ol>
    );
  }

  function renderBar(bar: HistoryBar) {
    const open = openRows.has(bar.key);
    const Glyph = BAR_GLYPHS[bar.kind];
    const ran = bar.kind === "turn" || bar.kind === "interrupted";
    return (
      <li className="history-pane__entry" key={bar.key} data-kind={bar.kind}>
        <button
          type="button"
          className="history-pane__row history-pane__row--round"
          data-kind={bar.kind}
          aria-expanded={open}
          title={barTooltip(bar)}
          onClick={() => toggleRow(bar.key)}
        >
          <ChevronRight
            size={13}
            className="history-pane__chevron"
            data-open={open || undefined}
            aria-hidden="true"
          />
          <span className="history-pane__kind" data-kind={bar.kind}>
            <Glyph size={12} aria-hidden="true" />
          </span>
          {/* No number of its own: a bar's position in this list is not an
              entry's number. */}
          <span className="history-pane__title">{barTitle(bar)}</span>
          {bar.kind === "interrupted" && bar.modelId && (
            <span className="history-pane__detail">{modelName(bar)}</span>
          )}
          {ran ? <Usage usage={bar.usage} /> : <span className="history-pane__change" />}
          <span className="history-pane__time">{formatTime(bar.createdAt)}</span>
        </button>
        {open && <div className="history-pane__body">{renderBody(bar)}</div>}
      </li>
    );
  }

  return (
    <div className="history-pane">
      <ol className="history-pane__list">{bars.map(renderBar)}</ol>
    </div>
  );
}
