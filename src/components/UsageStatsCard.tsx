import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n, type TranslationFunction } from "../i18n";
import {
  formatCompactCount,
  formatExactCount,
  purrDuration,
  summarizeUsage,
  USAGE_RANGES,
  type ModelUsageRow,
  type PurrUnit,
  type UsageBucketSize,
  type UsageRange,
  type UsageSeries,
  type UsageStatistics,
  type UsageSummary
} from "../lib/usageStatistics";
import { MeworkIcon } from "./MeworkIcon";
import "./UsageStatsCard.css";

/**
 * Statistics placeholder for an empty message area.
 *
 * Conversation, message, and activity data come from the conversation store;
 * token usage comes from the built-in gateway ledger.
 */
export function UsageStatsCard({
  statistics,
  loading = false
}: {
  statistics: UsageStatistics;
  loading?: boolean;
}) {
  const { t, resolvedLanguage } = useI18n();
  const [tab, setTab] = useState<"overview" | "models">("overview");
  const [range, setRange] = useState<UsageRange>("all");
  const summary = useMemo(() => summarizeUsage(statistics, range), [statistics, range]);
  const purr = useMemo(() => purrDuration(summary.totalTokens), [summary.totalTokens]);

  const rangeLabel = (value: UsageRange) => (
    value === "all" ? t("全部", "All") : value === "30d" ? t("30 天", "30d") : t("7 天", "7d")
  );

  const peakHourLabel = summary.peakHour === null
    ? "—"
    : resolvedLanguage === "zh-CN"
      ? t("{hour} 时", "{hour}:00", { hour: summary.peakHour })
      : formatHour12(summary.peakHour);

  const tiles: Array<{ key: string; label: string; value: string; title?: string }> = [
    {
      key: "sessions",
      label: t("会话数", "Sessions"),
      value: formatExactCount(summary.sessions)
    },
    {
      key: "messages",
      label: t("消息数", "Messages"),
      value: formatExactCount(summary.messages)
    },
    {
      key: "tokens",
      label: t("总 token", "Total tokens"),
      value: formatCompactCount(summary.totalTokens),
      title: formatExactCount(summary.totalTokens)
    },
    {
      key: "activeDays",
      label: t("活跃天数", "Active days"),
      value: formatExactCount(summary.activeDays)
    },
    {
      key: "currentStreak",
      label: t("当前连续", "Current streak"),
      value: t("{days} 天", "{days}d", { days: summary.currentStreak })
    },
    {
      key: "longestStreak",
      label: t("最长连续", "Longest streak"),
      value: t("{days} 天", "{days}d", { days: summary.longestStreak })
    },
    {
      key: "peakHour",
      label: t("高峰时段", "Peak hour"),
      value: peakHourLabel
    },
    {
      key: "favoriteModel",
      label: t("最常用模型", "Favorite model"),
      value: summary.favoriteModel?.modelId ?? "—",
      title: summary.favoriteModel
        ? `${summary.favoriteModel.providerName || t("未知提供商", "Unknown provider")} · ${summary.favoriteModel.modelId}`
        : undefined
    }
  ];

  return (
    <div className="usage-stats">
      <h2 className="usage-stats__greeting">
        <MeworkIcon size={22} />
        <span>{t("接下来做点什么？", "What's up next?")}</span>
      </h2>
      <section
        className="usage-stats__card"
        aria-label={t("使用统计", "Usage statistics")}
        aria-busy={loading || undefined}
      >
        <div className="usage-stats__toolbar">
          <div className="usage-stats__segmented" role="tablist" aria-label={t("统计视图", "Statistics view")}>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "overview"}
              className={tab === "overview" ? "is-active" : ""}
              onClick={() => setTab("overview")}
            >
              {t("总览", "Overview")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "models"}
              className={tab === "models" ? "is-active" : ""}
              onClick={() => setTab("models")}
            >
              {t("模型", "Models")}
            </button>
          </div>
          <div
            className="usage-stats__segmented usage-stats__segmented--compact"
            role="group"
            aria-label={t("统计区间", "Statistics range")}
          >
            {USAGE_RANGES.map((value) => (
              <button
                key={value}
                type="button"
                aria-pressed={range === value}
                className={range === value ? "is-active" : ""}
                onClick={() => setRange(value)}
              >
                {rangeLabel(value)}
              </button>
            ))}
          </div>
        </div>

        {tab === "overview" ? (
          <>
            <div className="usage-stats__tiles">
              {tiles.map((tile) => (
                <div className="usage-stats__tile" key={tile.key}>
                  <span className="usage-stats__tile-label">{tile.label}</span>
                  <strong className="usage-stats__tile-value" title={tile.title}>{tile.value}</strong>
                </div>
              ))}
            </div>
            <div
              className="usage-stats__heatmap"
              role="img"
              aria-label={t(
                "最近 26 周的活跃度，共 {days} 个活跃日",
                "Activity over the last 26 weeks, {days} active days",
                { days: summary.activeDays }
              )}
            >
              {summary.heatmapWeeks.map((week) => (
                <div className="usage-stats__heatmap-week" key={week[0]?.day ?? ""}>
                  {week.map((cell) => (
                    <span
                      key={cell.day}
                      className={`usage-stats__heatmap-cell usage-stats__heatmap-cell--l${cell.level}${cell.future ? " usage-stats__heatmap-cell--future" : ""}`}
                      title={cell.future
                        ? undefined
                        : t(
                          "{day}：{messages} 条消息 · {tokens} token",
                          "{day}: {messages} messages · {tokens} tokens",
                          {
                            day: cell.day,
                            messages: formatExactCount(cell.messages),
                            tokens: formatCompactCount(cell.tokens)
                          }
                        )}
                    />
                  ))}
                </div>
              ))}
            </div>
            <p className="usage-stats__note">
              {purr
                ? t(
                  "相当于一只猫连续呼噜了 {duration}。",
                  "That's a cat purring nonstop for {duration}.",
                  { duration: `${purr.value.toFixed(2)} ${purrUnitLabel(purr.unit, t)}` }
                )
                : loading
                  ? t("正在读取统计…", "Loading statistics…")
                  : t(
                    "还没有记录到任何 token 用量——发出第一条消息就会开始记账。",
                    "No token usage recorded yet — the first message starts the ledger."
                  )}
            </p>
          </>
        ) : (
          <ModelUsagePanel summary={summary} loading={loading} />
        )}
      </section>
    </div>
  );
}

/** Legend rows shown before the "show the rest" toggle. */
const LEGEND_VISIBLE = 6;

/** The palette is one accent ramp; models past the last step share its palest shade. */
const SERIES_SHADES = 8;

/** Dated x-axis labels at most, before the plot's own width narrows it further. */
const AXIS_LABELS = 7;

/**
 * Room one dated label needs at the 10px axis font, widest locale first
 * ("2026年8月" is far wider than "Aug 2026"), plus a gap so neighbours breathe.
 */
const LABEL_WIDTH: Record<UsageBucketSize, number> = { day: 56, week: 56, month: 68 };

/** Models listed in one hover card. The rest collapse into a counted line, never silently. */
const TOOLTIP_ROWS = 8;

function shadeIndex(index: number): number {
  return Math.min(index, SERIES_SHADES - 1);
}

/**
 * The models tab: usage bucketed over time, stacked per model, plus a legend
 * carrying each model's share.
 *
 * Bars and legend both index into `models`, so "which model" and "which shade"
 * are the same fact and no separate colour map has to be kept in sync.
 */
function ModelUsagePanel({ summary, loading }: { summary: UsageSummary; loading: boolean }) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);
  const { models } = summary;

  if (!models.length) {
    return (
      <div className="usage-stats__models">
        <p className="usage-stats__note">
          {loading
            ? t("正在读取统计…", "Loading statistics…")
            : t("这个区间还没有任何模型用量。", "No model usage in this range yet.")}
        </p>
      </div>
    );
  }

  const visible = expanded ? models : models.slice(0, LEGEND_VISIBLE);
  const hidden = models.length - visible.length;

  return (
    <div className="usage-stats__models">
      {/* Remount when the bucketing changes so the hovered index can never point
          at a column that is no longer the one under the cursor. */}
      <UsageSeriesChart
        key={`${summary.series.bucket}:${summary.series.points.length}`}
        series={summary.series}
        models={models}
      />
      <ul className="usage-stats__legend">
        {visible.map((row, index) => (
          <li className="usage-stats__legend-row" key={row.key} title={legendTitle(row, t)}>
            <span
              className={`usage-stats__swatch usage-stats__swatch--s${shadeIndex(index)}`}
              aria-hidden="true"
            />
            <span className="usage-stats__legend-name">{row.modelId}</span>
            <span className="usage-stats__legend-tokens">
              {t(
                "{input} 输入 · {output} 输出 · {cache} 缓存",
                "{input} in · {output} out · {cache} cache",
                {
                  input: formatCompactCount(row.inputTokens),
                  output: formatCompactCount(row.outputTokens),
                  cache: formatCompactCount(row.cachedInputTokens)
                }
              )}
            </span>
            <span className="usage-stats__legend-share">{formatShare(row.share)}</span>
            {/* The visible row compacts the numbers and leaves the percentage
                unlabelled; the table this replaced announced provider, requests
                and exact totals through column headers, so restate them here
                rather than only in `title`, which assistive tech may skip. */}
            <span className="sr-only">{legendDetail(row, t)}</span>
          </li>
        ))}
      </ul>
      {models.length > LEGEND_VISIBLE ? (
        <button
          type="button"
          className="usage-stats__legend-more"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded
            ? t("收起", "Show less")
            : t("显示其余 {count} 个", "Show {count} more", { count: hidden })}
        </button>
      ) : null}
    </div>
  );
}

function UsageSeriesChart({
  series,
  models
}: {
  series: UsageSeries;
  models: ModelUsageRow[];
}) {
  const { t, resolvedLanguage } = useI18n();
  const [hovered, setHovered] = useState<number | null>(null);
  const [plotWidth, setPlotWidth] = useState(0);
  const plotRef = useRef<HTMLDivElement>(null);
  // The card is a `1fr` track inside a shell that can be squeezed to a few
  // hundred pixels, so how many dates fit is a measurement, not a constant.
  useEffect(() => {
    const node = plotRef.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      setPlotWidth(entries[0]?.contentRect.width ?? 0);
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  // A range can hold requests but no tokens when upstream reported no usage.
  // Dividing by zero makes every bar NaN%, which the browser reads as `auto` —
  // a full-height column.
  const scale = series.max > 0 ? series.max : 1;
  const lastIndex = series.points.length - 1;
  const point = hovered === null ? null : series.points[hovered] ?? null;
  // Labels are counted back from the last bar, so however many bars there are,
  // the most recent one always carries its date.
  const labelBudget = plotWidth > 0
    ? Math.max(2, Math.min(AXIS_LABELS, Math.floor(plotWidth / LABEL_WIDTH[series.bucket])))
    : AXIS_LABELS;
  const stride = Math.max(1, Math.ceil(series.points.length / labelBudget));

  const tipRows = point
    ? point.byModel
      .map((tokens, index) => ({ tokens, model: models[index], index }))
      .filter((row) => row.tokens > 0 && row.model)
      .sort((left, right) => right.tokens - left.tokens)
    : [];
  const tipVisible = tipRows.slice(0, TOOLTIP_ROWS);

  return (
    <div className="usage-stats__chart">
      <div className="usage-stats__chart-axis" aria-hidden="true">
        {series.ticks.map((tick) => (
          <span
            className="usage-stats__chart-tick"
            key={tick}
            style={{ bottom: `${(tick / scale) * 100}%` }}
          >
            {formatCompactCount(tick)}
          </span>
        ))}
      </div>
      <div
        className="usage-stats__chart-plot"
        ref={plotRef}
        role="group"
        tabIndex={0}
        aria-label={t(
          "按{bucket}分列的 token 用量，峰值 {peak}；用左右方向键逐列查看",
          "Token usage per {bucket}, peaking at {peak}. Use the arrow keys to step through it.",
          {
            bucket: bucketNoun(series.bucket, t),
            peak: formatCompactCount(series.max)
          }
        )}
        onMouseLeave={() => setHovered(null)}
        onMouseOver={(event) => {
          // Delegated to the whole plot rather than one listener per bar: there
          // can be 240 of them, and events still bubble back here while the
          // pointer crosses the hover card.
          const column = (event.target as HTMLElement).closest<HTMLElement>("[data-index]");
          setHovered(column ? Number(column.dataset.index) : null);
        }}
        onFocus={() => setHovered((current) => current ?? lastIndex)}
        onBlur={() => setHovered(null)}
        onKeyDown={(event) => {
          // One tab stop plus arrow keys, rather than 240 focusable bars.
          const step = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
          if (step) {
            event.preventDefault();
            setHovered((current) => (
              current === null
                ? (step < 0 ? lastIndex : 0)
                : Math.min(lastIndex, Math.max(0, current + step))
            ));
          } else if (event.key === "Home") {
            event.preventDefault();
            setHovered(0);
          } else if (event.key === "End") {
            event.preventDefault();
            setHovered(lastIndex);
          } else if (event.key === "Escape") {
            setHovered(null);
          }
        }}
      >
        {series.ticks.map((tick) => (
          <span
            className="usage-stats__chart-grid"
            key={tick}
            style={{ bottom: `${(tick / scale) * 100}%` }}
          />
        ))}
        <div className="usage-stats__chart-columns">
          {series.points.map((entry, index) => (
            <div
              className={`usage-stats__chart-column${index === hovered ? " is-hovered" : ""}`}
              key={entry.key}
              data-index={index}
              data-bucket={entry.key}
            >
              <span className="usage-stats__chart-stack">
                {entry.byModel.map((tokens, modelIndex) => (
                  tokens > 0 && models[modelIndex] ? (
                    <span
                      className={`usage-stats__chart-segment usage-stats__chart-segment--s${shadeIndex(modelIndex)}`}
                      key={models[modelIndex].key}
                      style={{ height: `${(tokens / scale) * 100}%` }}
                    />
                  ) : null
                ))}
              </span>
            </div>
          ))}
        </div>
        {point && tipVisible.length ? (
          <div
            className={`usage-stats__chart-tip${hovered !== null && hovered * 2 >= lastIndex ? " usage-stats__chart-tip--end" : ""}`}
            aria-hidden="true"
          >
            <strong className="usage-stats__chart-tip-title">
              {bucketTitle(point.startMs, series.bucket, resolvedLanguage)}
            </strong>
            {tipVisible.map((row) => (
              <span className="usage-stats__chart-tip-row" key={row.model.key}>
                <span
                  className={`usage-stats__swatch usage-stats__swatch--s${shadeIndex(row.index)}`}
                  aria-hidden="true"
                />
                <span className="usage-stats__chart-tip-name">{row.model.modelId}</span>
                <span className="usage-stats__chart-tip-value">{formatCompactCount(row.tokens)}</span>
              </span>
            ))}
            {tipRows.length > tipVisible.length ? (
              <span className="usage-stats__chart-tip-rest">
                {t("还有 {count} 个模型", "{count} more models", {
                  count: tipRows.length - tipVisible.length
                })}
              </span>
            ) : null}
          </div>
        ) : null}
      </div>
      {/* The live region is mounted for the life of the chart, empty or not: a
          region that appears together with its text is routinely missed, so the
          spoken copy lives here and the visual card stays decorative. */}
      <span className="sr-only" aria-live="polite">
        {point
          ? [
            bucketTitle(point.startMs, series.bucket, resolvedLanguage),
            ...tipRows.map((row) => `${row.model.modelId} ${formatExactCount(row.tokens)}`)
          ].join(" · ")
          : ""}
      </span>
      <div className="usage-stats__chart-days" aria-hidden="true">
        {series.points.map((entry, index) => (
          (lastIndex - index) % stride === 0 ? (
            <span
              className={`usage-stats__chart-day${index !== 0 && index !== lastIndex ? " usage-stats__chart-day--mid" : ""}`}
              key={entry.key}
              style={index === lastIndex
                ? { right: 0 }
                : index === 0
                  ? { left: 0 }
                  : { left: `${((index + 0.5) / series.points.length) * 100}%` }}
            >
              {bucketLabel(entry.startMs, series.bucket, resolvedLanguage)}
            </span>
          ) : null
        ))}
      </div>
    </div>
  );
}

function bucketLabel(startMs: number, bucket: UsageBucketSize, locale: string): string {
  return new Intl.DateTimeFormat(
    locale,
    bucket === "month"
      ? { year: "numeric", month: "short" }
      : { month: "short", day: "numeric" }
  ).format(new Date(startMs));
}

/** A weekly bar's hover title spans both ends: the Monday alone never reads as seven days. */
function bucketTitle(startMs: number, bucket: UsageBucketSize, locale: string): string {
  if (bucket !== "week") return bucketLabel(startMs, bucket, locale);
  const end = new Date(startMs);
  end.setDate(end.getDate() + 6);
  return `${bucketLabel(startMs, "day", locale)} – ${bucketLabel(end.getTime(), "day", locale)}`;
}

function bucketNoun(bucket: UsageBucketSize, t: TranslationFunction): string {
  if (bucket === "month") return t("月", "month");
  if (bucket === "week") return t("周", "week");
  return t("天", "day");
}

function purrUnitLabel(unit: PurrUnit, t: TranslationFunction): string {
  if (unit === "month") return t("个月", "months");
  if (unit === "day") return t("天", "days");
  if (unit === "hour") return t("小时", "hours");
  return t("分钟", "minutes");
}

function legendTitle(row: ModelUsageRow, t: TranslationFunction): string {
  return [
    row.providerName || t("未知提供商", "Unknown provider"),
    t("{count} 次请求", "{count} requests", { count: formatExactCount(row.requests) }),
    t("输入 {value}", "Input {value}", { value: formatExactCount(row.inputTokens) }),
    t("输出 {value}", "Output {value}", { value: formatExactCount(row.outputTokens) }),
    t("缓存 {value}", "Cached {value}", { value: formatExactCount(row.cachedInputTokens) }),
    t("总计 {value}", "Total {value}", { value: formatExactCount(row.totalTokens) })
  ].join(" · ");
}

/** The spoken row. It names the share too — a bare `%` beside a number is not a sentence. */
function legendDetail(row: ModelUsageRow, t: TranslationFunction): string {
  return [
    row.modelId,
    legendTitle(row, t),
    t("占 {value}", "{value} of all tokens", { value: formatShare(row.share) })
  ].join(" · ");
}

/** One decimal. Rounding to whole percents lines up a column of "0%" that hides the order. */
function formatShare(share: number): string {
  const percent = Number.isFinite(share) ? Math.max(0, share) * 100 : 0;
  return `${percent.toFixed(1)}%`;
}

function formatHour12(hour: number): string {
  const suffix = hour < 12 ? "AM" : "PM";
  const twelve = hour % 12 === 0 ? 12 : hour % 12;
  return `${twelve} ${suffix}`;
}
