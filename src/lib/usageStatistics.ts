import { hasBackendRuntime, invoke } from "./backend";
import { loadConversationTurns } from "./conversationTurns";
import type { AppDocument } from "../types";

/** 一个 UTC 整点里的对话活动量。与 Rust `conversation_store::ActivityBucket` 同形。 */
export interface ActivityBucket {
  hourStartMs: number;
  userMessages: number;
  assistantMessages: number;
  sessions: number;
}

export type UsageOrigin = "conversation" | "subagent" | "web_search";

/** 一个 UTC 整点 × 模型的 token 用量。与 Rust `token_ledger::UsageBucket` 同形。 */
export interface UsageBucket {
  hourStartMs: number;
  providerId: string;
  providerName: string;
  modelId: string;
  origin: UsageOrigin;
  requests: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export interface UsageStatistics {
  generatedAtMs: number;
  ledgerStartedAtMs: number;
  activity: ActivityBucket[];
  usage: UsageBucket[];
}

export interface UsageBackfillEvent {
  id: string;
  occurredAtMs: number;
  origin: UsageOrigin;
  conversationId: string;
  workspaceId: string;
  providerId: string;
  providerName: string;
  modelId: string;
  requests: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export const EMPTY_USAGE_STATISTICS: UsageStatistics = {
  generatedAtMs: 0,
  ledgerStartedAtMs: 0,
  activity: [],
  usage: []
};

export type UsageRange = "all" | "30d" | "7d";

export const USAGE_RANGES: UsageRange[] = ["all", "30d", "7d"];

/** 热力图恒定覆盖最近 26 周，与上方指标的区间开关无关。 */
export const HEATMAP_WEEKS = 26;

export interface ModelUsageRow {
  key: string;
  providerName: string;
  modelId: string;
  requests: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
  /** 占区间内总 token 的比例，0–1。总数为 0 时是 0。 */
  share: number;
}

/** 一根柱子代表多长时间。跨度长了就并桶，否则柱子会细到看不见。 */
export type UsageBucketSize = "day" | "week" | "month";

export interface UsageSeriesPoint {
  /** 桶起点的本地时间戳。 */
  startMs: number;
  /** 桶起点的本地日 `YYYY-MM-DD`，做稳定 key。 */
  key: string;
  /** 各模型在这个桶里的 token；下标与 `UsageSummary.models` 一一对齐。 */
  byModel: number[];
  totalTokens: number;
}

export interface UsageSeries {
  bucket: UsageBucketSize;
  /** 自左向右按时间升序，最后一根永远是「现在」所在的桶。 */
  points: UsageSeriesPoint[];
  /** y 轴上界（已经取整到刻度上）；区间内没有 token 时是 0。 */
  max: number;
  /** 从 0 起的网格线刻度，最后一个等于 `max`；`max` 为 0 时是空的。 */
  ticks: number[];
}

export interface HeatmapCell {
  /** 本地日，`YYYY-MM-DD`。 */
  day: string;
  /** 该日起点的本地时间戳，用于渲染标题。 */
  startMs: number;
  messages: number;
  tokens: number;
  /** 0–4，0 表示当天没有任何活动。 */
  level: number;
  /** 该格晚于今天（补齐当前这一周用的空位）。 */
  future: boolean;
}

export interface UsageSummary {
  sessions: number;
  messages: number;
  requests: number;
  totalTokens: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  activeDays: number;
  currentStreak: number;
  longestStreak: number;
  /** 本地小时 0–23；区间内没有任何消息时为 null。 */
  peakHour: number | null;
  favoriteModel: ModelUsageRow | null;
  models: ModelUsageRow[];
  /** 区间内按时间分桶、按模型分层的堆叠柱状图数据。 */
  series: UsageSeries;
  /** 按周分列、每列 7 天（周一在上）的最近 26 周。 */
  heatmapWeeks: HeatmapCell[][];
  hasActivity: boolean;
  hasTokens: boolean;
}

export async function fetchUsageStatistics(): Promise<UsageStatistics> {
  if (!hasBackendRuntime()) return EMPTY_USAGE_STATISTICS;
  return invoke<UsageStatistics>("token_usage_statistics");
}

/** 一次推送的事件上限。回合表是本地存储，条数由用户的历史决定，不设界会让
 * 单次 IPC 载荷跟着历史一起长。 */
const BACKFILL_CHUNK = 500;

export async function pushUsageBackfill(events: UsageBackfillEvent[]): Promise<number> {
  if (!hasBackendRuntime() || !events.length) return 0;
  let written = 0;
  for (let index = 0; index < events.length; index += BACKFILL_CHUNK) {
    written += await invoke<number>("backfill_token_usage", {
      events: events.slice(index, index + BACKFILL_CHUNK)
    });
  }
  return written;
}

/** 记下「已经对着哪一份流水账补过历史」。值是那份流水账的记账起点。 */
export const USAGE_BACKFILL_STORAGE_KEY = "mework.usage-backfilled.v1";

/**
 * 补一次历史——只在需要时。
 *
 * 补过之后就不必再补：记账起点之前的回合已经全部推过去了，之后的每一次请求都
 * 由网关自己记。判据是记账起点本身而不是一个布尔标记，所以流水账被重建过
 * （`reset:data`、版本不匹配封存）之后会自动再补一次——那正是标记该失效的时候。
 *
 * 返回是否真的推送过。推送过就该重读一次统计，否则卡片要等到下一次进草稿才
 * 看得见历史。
 */
export async function ensureUsageBackfill(
  document: AppDocument | null,
  statistics: UsageStatistics
): Promise<boolean> {
  if (!hasBackendRuntime() || !statistics.ledgerStartedAtMs) return false;
  const marker = String(statistics.ledgerStartedAtMs);
  if (readBackfillMarker() === marker) return false;
  const events = usageBackfillEventsFromTurns(document)
    .filter((event) => event.occurredAtMs < statistics.ledgerStartedAtMs);
  if (events.length) await pushUsageBackfill(events);
  writeBackfillMarker(marker);
  return events.length > 0;
}

function readBackfillMarker(): string | null {
  try {
    return window.localStorage.getItem(USAGE_BACKFILL_STORAGE_KEY);
  } catch {
    return null;
  }
}

function writeBackfillMarker(value: string): void {
  try {
    window.localStorage.setItem(USAGE_BACKFILL_STORAGE_KEY, value);
  } catch {
    // 标记写不进去只意味着下次启动会再补一遍：幂等键让那次是 no-op。
  }
}

/**
 * 网关上线之前的每回合用量只存在于渲染层的本地回合表里，所以补历史只能由这里
 * 推过去。取的是 `turn.usage`——UI 回合自己的用量，它**已经吸收了**子代理的
 * 用量，所以不能再去补一遍子代理记录，那会双计。
 *
 * `id` 可推导（`turn:<对话>:<回合>`），宿主侧是 `INSERT OR IGNORE`，因此重复推
 * 送是 no-op；宿主还会丢掉落在记账起点之后的事件，那段时间网关自己记过了。
 */
export function usageBackfillEventsFromTurns(
  document: AppDocument | null,
  now = Date.now()
): UsageBackfillEvent[] {
  const turnsByConversation = loadConversationTurns(now);
  const workspaceByConversation = new Map<string, string>();
  for (const workspace of document?.workspaces ?? []) {
    for (const conversation of workspace.conversations) {
      workspaceByConversation.set(conversation.id, workspace.id);
    }
  }
  // 历史回合只记了模型 id，没记提供商。按当前目录反查一次是最好的还原：模型 id
  // 在一份文档里几乎总是唯一的，认不出来就留空，卡片会显示成「未知提供商」，
  // 而不是编一个。
  const providerByModel = new Map<string, { id: string; name: string }>();
  for (const provider of document?.globalSettings.apiProviders ?? []) {
    for (const model of provider.models) {
      if (!providerByModel.has(model.id)) {
        providerByModel.set(model.id, { id: provider.id, name: provider.name });
      }
    }
  }
  const events: UsageBackfillEvent[] = [];
  for (const [conversationId, turns] of Object.entries(turnsByConversation)) {
    for (const turn of turns) {
      const input = turn.usage.inputTokens ?? 0;
      const output = turn.usage.outputTokens ?? 0;
      const cached = turn.usage.cachedInputTokens ?? 0;
      const total = turn.usage.totalTokens ?? input + output;
      if (!input && !output && !cached && !total) continue;
      const occurredAtMs = new Date(turn.endedAt ?? turn.startedAt).getTime();
      if (!Number.isFinite(occurredAtMs)) continue;
      const provider = providerByModel.get(turn.modelId);
      events.push({
        id: `turn:${conversationId}:${turn.id}`,
        occurredAtMs,
        origin: "conversation",
        conversationId,
        workspaceId: workspaceByConversation.get(conversationId) ?? "",
        providerId: provider?.id ?? "",
        providerName: provider?.name ?? "",
        modelId: turn.modelId,
        requests: 1,
        inputTokens: input,
        cachedInputTokens: cached,
        outputTokens: output,
        totalTokens: total
      });
    }
  }
  return events;
}

function localDayKey(date: Date): string {
  const year = date.getFullYear();
  const month = `${date.getMonth() + 1}`.padStart(2, "0");
  const day = `${date.getDate()}`.padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function startOfLocalDay(timestamp: number): Date {
  const date = new Date(timestamp);
  date.setHours(0, 0, 0, 0);
  return date;
}

function addDays(date: Date, days: number): Date {
  const next = new Date(date);
  next.setDate(next.getDate() + days);
  return next;
}

/** 周一 = 0。热力图按周分列，与 GitHub 式贡献图同一个排布。 */
function mondayIndex(date: Date): number {
  return (date.getDay() + 6) % 7;
}

/** 超过这么多天就并成周桶，周桶再超同样多根就并成月桶。 */
const MAX_DAY_BUCKETS = 62;

/**
 * 柱子根数的硬上限。桶宽已经会随跨度放大，但补历史时见过 1970 这种离谱时间戳，
 * 月桶照样能长到几百根——上限从「现在」往回截，留下的永远是最近那一段。
 */
export const MAX_SERIES_POINTS = 240;

const DAY_MS = 86_400_000;

function startOfBucket(timestamp: number, bucket: UsageBucketSize): Date {
  const day = startOfLocalDay(timestamp);
  if (bucket === "day") return day;
  if (bucket === "week") return addDays(day, -mondayIndex(day));
  day.setDate(1);
  return day;
}

function stepBucket(date: Date, bucket: UsageBucketSize, steps: number): Date {
  if (bucket === "day") return addDays(date, steps);
  if (bucket === "week") return addDays(date, steps * 7);
  const next = new Date(date);
  next.setMonth(next.getMonth() + steps);
  return next;
}

/** 跨天数按本地零点相减再取整——夏令时那两天会差一小时，取整把它吸收掉。 */
function localDaysBetween(fromMs: number, toMs: number): number {
  return Math.round(
    (startOfLocalDay(toMs).getTime() - startOfLocalDay(fromMs).getTime()) / DAY_MS
  ) + 1;
}

/** 从 `fromMs` 到 `now`（含两端）一共要画多少根这种宽度的柱子。 */
function bucketSpan(fromMs: number, now: number, bucket: UsageBucketSize): number {
  const start = startOfBucket(fromMs, bucket);
  const end = startOfBucket(now, bucket);
  if (bucket === "month") {
    return (end.getFullYear() - start.getFullYear()) * 12
      + (end.getMonth() - start.getMonth()) + 1;
  }
  const days = localDaysBetween(start.getTime(), end.getTime());
  return bucket === "week" ? Math.round((days - 1) / 7) + 1 : days;
}

/**
 * 桶宽按**真实柱子数**选，而不是按天数折算：一段 434 天的区间若从周日起、到周六
 * 止，对齐到周一之后是 63 根，「天数 ÷ 7」这个判据看不出多出来的那一根。
 */
function pickBucketSize(fromMs: number, now: number): UsageBucketSize {
  if (bucketSpan(fromMs, now, "day") <= MAX_DAY_BUCKETS) return "day";
  if (bucketSpan(fromMs, now, "week") <= MAX_DAY_BUCKETS) return "week";
  return "month";
}

/**
 * 轴上界取「好看的整数」而不是峰值本身：刻度落在 1/2/2.5/5 的整数倍上，读数才是
 * `5M / 10M / 15M / 20M` 而不是 `4.7M / 9.4M / …`。
 *
 * 候选里选**留白最少**的那一档，而不是照「目标 4 格」直接算：峰值刚过一档时
 * （20.4M 之于 5M 一格）照算会跳到 10M 一格、上界 30M，图的上半截整片空着。
 */
export function niceTicks(max: number, maxIntervals = 5): { max: number; ticks: number[] } {
  if (!Number.isFinite(max) || max <= 0) return { max: 0, ticks: [] };
  const exponent = Math.floor(Math.log10(max));
  const candidates: Array<{ step: number; top: number; count: number }> = [];
  for (let power = exponent - 2; power <= exponent + 1; power += 1) {
    for (const multiplier of [1, 2, 2.5, 5]) {
      const step = multiplier * 10 ** power;
      // 非整数步长会把刻度写成 `7.5`，而 token 只有整数。
      if (step < 1 || !Number.isInteger(step)) continue;
      const count = Math.ceil(max / step);
      if (count >= 1 && count <= maxIntervals) candidates.push({ step, top: count * step, count });
    }
  }
  // 三格以下画不成一张有刻度的图；实在凑不出来（峰值小到只有一两格）才退让。
  const dense = candidates.filter((entry) => entry.count >= 3);
  const pool = dense.length ? dense : candidates;
  const best = pool.sort((left, right) => (
    left.top - right.top
    || Math.abs(left.count - 4) - Math.abs(right.count - 4)
    || left.count - right.count
  ))[0];
  if (!best) return { max: Math.round(max), ticks: [0, Math.round(max)] };
  const ticks: number[] = [];
  for (let index = 0; index <= best.count; index += 1) ticks.push(Math.round(index * best.step));
  return { max: Math.round(best.top), ticks };
}

/**
 * 把「桶起点 → 模型 → token」摊平成图表要的定长数组。
 *
 * `byModel` 的下标就是 `models` 的下标，图例和柱子因此共用同一套颜色，不必再
 * 传一份配色映射。
 */
function buildSeries(
  totals: Map<number, Map<string, number>>,
  models: ModelUsageRow[],
  fromMs: number,
  now: number,
  bucket: UsageBucketSize
): UsageSeries {
  const modelIndex = new Map(models.map((row, index) => [row.key, index]));
  const starts: Date[] = [];
  const floor = startOfBucket(fromMs, bucket).getTime();
  // 从「现在」往回铺：截断发生在最旧的一端，最近的那几根柱子永远在。每走一步都
  // 重新归一到桶起点——有的时区午夜根本不存在（南半球春令时就在午夜跳表），那天
  // 的 `setHours(0)` 会落在 01:00；不归一的话游标会带着这一小时一路往回走，键就
  // 再也对不上 `seriesTotals` 里那一份，柱子会凭空少掉一整天。
  for (
    let cursor = startOfBucket(now, bucket);
    cursor.getTime() >= floor && starts.length < MAX_SERIES_POINTS;
    cursor = startOfBucket(stepBucket(cursor, bucket, -1).getTime(), bucket)
  ) {
    starts.push(cursor);
  }
  starts.reverse();

  const columns = starts.map(() => new Map<string, number>());
  const indexByStart = new Map(starts.map((start, index) => [start.getTime(), index]));
  const oldest = starts[0]?.getTime() ?? 0;
  for (const [startMs, tokensByModel] of totals) {
    // 落在窗口外的桶折进最近的那一根，而不是丢掉。柱子加起来必须等于图例——
    // 「最旧那根鼓起来一块」看得见，「悄悄少算」看不见。窗口外只有两种来路：
    // 被 MAX_SERIES_POINTS 截掉的史前时间戳，和时钟回拨后落在未来的记录。
    const index = indexByStart.get(startMs) ?? (startMs < oldest ? 0 : starts.length - 1);
    const column = columns[index];
    if (!column) continue;
    for (const [key, tokens] of tokensByModel) {
      column.set(key, (column.get(key) ?? 0) + tokens);
    }
  }

  let peak = 0;
  const points = starts.map((start, index) => {
    const byModel = new Array<number>(models.length).fill(0);
    let totalTokens = 0;
    for (const [key, tokens] of columns[index]) {
      const slot = modelIndex.get(key);
      if (slot === undefined) continue;
      byModel[slot] += tokens;
      totalTokens += tokens;
    }
    if (totalTokens > peak) peak = totalTokens;
    return { startMs: start.getTime(), key: localDayKey(start), byModel, totalTokens };
  });

  return { bucket, points, ...niceTicks(peak) };
}

function rangeStartMs(range: UsageRange, now: number): number {
  if (range === "all") return Number.NEGATIVE_INFINITY;
  const days = range === "30d" ? 30 : 7;
  // 「最近 30 天」含今天，所以往回退 29 天再取当天零点。
  return addDays(startOfLocalDay(now), -(days - 1)).getTime();
}

interface DayTotals {
  messages: number;
  tokens: number;
  requests: number;
}

/**
 * 把两组 UTC 整点桶折算成本地日/本地小时，再按区间汇总出卡片要的一切。
 *
 * 换算发生在这里而不是 SQL 里，是因为时区是用户的属性：他换一次时区，历史
 * 不该跟着重算，库里存的整点也不该有一个「当时是哪个时区」的隐含前提。
 */
export function summarizeUsage(
  statistics: UsageStatistics,
  range: UsageRange,
  now = statistics.generatedAtMs || Date.now()
): UsageSummary {
  const from = rangeStartMs(range, now);
  const dayTotals = new Map<string, DayTotals>();
  const hourMessages = new Array<number>(24).fill(0);
  const bumpDay = (timestamp: number, patch: Partial<DayTotals>) => {
    const key = localDayKey(new Date(timestamp));
    const totals = dayTotals.get(key) ?? { messages: 0, tokens: 0, requests: 0 };
    totals.messages += patch.messages ?? 0;
    totals.tokens += patch.tokens ?? 0;
    totals.requests += patch.requests ?? 0;
    dayTotals.set(key, totals);
  };

  let sessions = 0;
  let messages = 0;
  for (const bucket of statistics.activity) {
    const bucketMessages = bucket.userMessages + bucket.assistantMessages;
    // 热力图恒定看全量历史，指标只看区间——所以每一格都记进日表，区间过滤在
    // 汇总量那一侧单独做。
    bumpDay(bucket.hourStartMs, { messages: bucketMessages });
    if (bucket.hourStartMs < from) continue;
    sessions += bucket.sessions;
    messages += bucketMessages;
    hourMessages[new Date(bucket.hourStartMs).getHours()] += bucketMessages;
  }

  let requests = 0;
  let totalTokens = 0;
  let inputTokens = 0;
  let cachedInputTokens = 0;
  let outputTokens = 0;
  const modelRows = new Map<string, ModelUsageRow>();
  // 「全部」的图表窗口从最早那一笔用量开始，而不是从最早那一条消息：模型页画的
  // 是 token，第一次记账之前的活跃日在这张图上本来就没有柱子可画。
  const earliestUsageMs = statistics.usage.reduce(
    (earliest, bucket) => Math.min(earliest, bucket.hourStartMs),
    Number.POSITIVE_INFINITY
  );
  const seriesFrom = Math.min(
    range === "all" ? (Number.isFinite(earliestUsageMs) ? earliestUsageMs : now) : from,
    now
  );
  const seriesBucket = pickBucketSize(seriesFrom, now);
  const seriesTotals = new Map<number, Map<string, number>>();
  for (const bucket of statistics.usage) {
    bumpDay(bucket.hourStartMs, { tokens: bucket.totalTokens, requests: bucket.requests });
    if (bucket.hourStartMs < from) continue;
    requests += bucket.requests;
    totalTokens += bucket.totalTokens;
    inputTokens += bucket.inputTokens;
    cachedInputTokens += bucket.cachedInputTokens;
    outputTokens += bucket.outputTokens;
    const key = `${bucket.providerId}\u0000${bucket.modelId}`;
    const row = modelRows.get(key) ?? {
      key,
      providerName: bucket.providerName,
      modelId: bucket.modelId,
      requests: 0,
      inputTokens: 0,
      cachedInputTokens: 0,
      outputTokens: 0,
      totalTokens: 0,
      share: 0
    };
    // 同一个模型 id 在历史回填里可能没有提供商名；哪一边先有名字就用哪一边。
    if (!row.providerName && bucket.providerName) row.providerName = bucket.providerName;
    row.requests += bucket.requests;
    row.inputTokens += bucket.inputTokens;
    row.cachedInputTokens += bucket.cachedInputTokens;
    row.outputTokens += bucket.outputTokens;
    row.totalTokens += bucket.totalTokens;
    modelRows.set(key, row);
    if (bucket.totalTokens > 0) {
      const startMs = startOfBucket(bucket.hourStartMs, seriesBucket).getTime();
      const column = seriesTotals.get(startMs) ?? new Map<string, number>();
      column.set(key, (column.get(key) ?? 0) + bucket.totalTokens);
      seriesTotals.set(startMs, column);
    }
  }

  const models = [...modelRows.values()]
    .map((row) => ({ ...row, share: totalTokens ? row.totalTokens / totalTokens : 0 }))
    .sort((left, right) => right.totalTokens - left.totalTokens || right.requests - left.requests);

  const activeDayKeys = new Set<string>();
  for (const [day, totals] of dayTotals) {
    if (totals.messages > 0 || totals.requests > 0) activeDayKeys.add(day);
  }
  const rangeActiveDays = [...activeDayKeys].filter((day) => {
    if (from === Number.NEGATIVE_INFINITY) return true;
    return new Date(`${day}T00:00:00`).getTime() >= from;
  });

  const { current, longest } = streaks(activeDayKeys, now);

  const peakHour = messages > 0
    ? hourMessages.reduce(
      (best, count, hour) => (count > hourMessages[best] ? hour : best),
      0
    )
    : null;

  return {
    sessions,
    messages,
    requests,
    totalTokens,
    inputTokens,
    cachedInputTokens,
    outputTokens,
    activeDays: rangeActiveDays.length,
    currentStreak: current,
    longestStreak: longest,
    peakHour,
    favoriteModel: models[0] ?? null,
    models,
    series: buildSeries(seriesTotals, models, seriesFrom, now, seriesBucket),
    heatmapWeeks: heatmap(dayTotals, now),
    hasActivity: activeDayKeys.size > 0,
    hasTokens: statistics.usage.length > 0
  };
}

/**
 * 当前连续天数从今天往回数；今天还没动静时从昨天起算，否则每天零点一到，
 * 昨天为止的连续记录就会被显示成 0——那不是它断了，只是今天还没开始。
 */
function streaks(activeDays: Set<string>, now: number): { current: number; longest: number } {
  if (!activeDays.size) return { current: 0, longest: 0 };
  const today = startOfLocalDay(now);
  let cursor = activeDays.has(localDayKey(today)) ? today : addDays(today, -1);
  let current = 0;
  while (activeDays.has(localDayKey(cursor))) {
    current += 1;
    cursor = addDays(cursor, -1);
  }

  const sorted = [...activeDays].sort();
  let longest = 0;
  let run = 0;
  let previous: Date | null = null;
  for (const day of sorted) {
    const date = new Date(`${day}T00:00:00`);
    run = previous && addDays(previous, 1).getTime() === date.getTime() ? run + 1 : 1;
    if (run > longest) longest = run;
    previous = date;
  }
  return { current, longest };
}

function heatmap(dayTotals: Map<string, DayTotals>, now: number): HeatmapCell[][] {
  const today = startOfLocalDay(now);
  // 最后一列是本周：从本周一起算，往前铺满 26 周。
  const thisMonday = addDays(today, -mondayIndex(today));
  const firstMonday = addDays(thisMonday, -(HEATMAP_WEEKS - 1) * 7);
  const nonZero = [...dayTotals.values()]
    .map((totals) => totals.tokens || totals.messages)
    .filter((value) => value > 0)
    .sort((left, right) => left - right);
  const busiest = nonZero.at(-1) ?? 0;
  // 分位点取自 **除最大值以外**的序位（`length - 1`），再让最大值直接进最深的
  // 一档。天数少的时候 `floor(n * 0.75)` 会正好落在最大值上，于是没有任何一格
  // 够得着第 4 档——热力图看上去永远是半凉的。
  const quartile = (fraction: number) => (
    nonZero.length ? nonZero[Math.floor((nonZero.length - 1) * fraction)] : 0
  );
  const thresholds = [quartile(0.25), quartile(0.5), quartile(0.75)];
  const weeks: HeatmapCell[][] = [];
  for (let week = 0; week < HEATMAP_WEEKS; week += 1) {
    const column: HeatmapCell[] = [];
    for (let weekday = 0; weekday < 7; weekday += 1) {
      const date = addDays(firstMonday, week * 7 + weekday);
      const day = localDayKey(date);
      const totals = dayTotals.get(day);
      const weight = totals ? totals.tokens || totals.messages : 0;
      const level = weight <= 0
        ? 0
        : weight >= busiest
          ? 4
          : weight <= thresholds[0]
            ? 1
            : weight <= thresholds[1]
              ? 2
              : weight <= thresholds[2]
                ? 3
                : 4;
      column.push({
        day,
        startMs: date.getTime(),
        messages: totals?.messages ?? 0,
        tokens: totals?.tokens ?? 0,
        level,
        future: date.getTime() > today.getTime()
      });
    }
    weeks.push(column);
  }
  return weeks;
}

/** 115_400_000 → `115.4M`。指标格要的是一眼可读，不是精确到个位。 */
export function formatCompactCount(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0";
  if (value < 1000) return `${Math.round(value)}`;
  const units: Array<[number, string]> = [
    [1_000_000_000, "B"],
    [1_000_000, "M"],
    [1_000, "K"]
  ];
  for (const [scale, suffix] of units) {
    if (value >= scale) {
      // 一位小数一律保留（`115.4M` 而不是 `115M`）：这一格是「我一共花了多少」，
      // 抹掉那一位就把四百万个 token 抹没了。整数倍时 Number() 顺手去掉 `.0`。
      return `${Number((value / scale).toFixed(1))}${suffix}`;
    }
  }
  return `${Math.round(value)}`;
}

export function formatExactCount(value: number): string {
  return Number.isFinite(value) ? Math.round(value).toLocaleString("en-US") : "0";
}

/** 10 万 token ＝ 一只猫连续呼噜 6 分钟。 */
const PURR_MINUTES_PER_TOKEN = 6 / 100_000;

export type PurrUnit = "minute" | "hour" | "day" | "month";

/** 按分钟数升序；月按 30 天算。 */
const PURR_UNITS: Array<{ unit: PurrUnit; minutes: number }> = [
  { unit: "minute", minutes: 1 },
  { unit: "hour", minutes: 60 },
  { unit: "day", minutes: 60 * 24 },
  { unit: "month", minutes: 60 * 24 * 30 }
];

export interface PurrDuration {
  value: number;
  unit: PurrUnit;
}

/** 把 token 总量换算成呼噜时长，进位到数值不小于 1 的最大单位。 */
export function purrDuration(totalTokens: number): PurrDuration | null {
  if (!Number.isFinite(totalTokens) || totalTokens <= 0) return null;
  const minutes = totalTokens * PURR_MINUTES_PER_TOKEN;
  let chosen = PURR_UNITS[0];
  for (const candidate of PURR_UNITS) {
    if (minutes >= candidate.minutes) chosen = candidate;
  }
  return { value: minutes / chosen.minutes, unit: chosen.unit };
}
