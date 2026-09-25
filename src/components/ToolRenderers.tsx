import {
  Blocks,
  Bot,
  Box,
  CircleAlert,
  CircleCheck,
  CircleHelp,
  ClipboardCheck,
  Code2,
  Crosshair,
  File,
  FilePenLine,
  FileSearch,
  FileText,
  Folder,
  FolderTree,
  GitFork,
  Globe,
  Image,
  Inbox,
  ListChecks,
  Logs,
  MessageCircleMore,
  MonitorPlay,
  MonitorUp,
  MonitorX,
  MousePointerClick,
  Network,
  NotebookPen,
  Plug,
  ScanSearch,
  Search,
  Server,
  SquareTerminal,
  TextCursorInput,
  Timer,
  Upload,
  Waypoints,
  Workflow,
  Wrench
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { t as globalT, useI18n } from "../i18n";
import { parseWaitOutput, questionsFromInput } from "../lib/orchestration";
import { webSourceIcon as siteIcon } from "../lib/runtime";
import { agentTimelineRunStatus, initialMessage, isChildMainMessageContext } from "../lib/subagents";
import type { JsonValue, SubagentUpdate, ToolContext, ToolDescriptor } from "../types";
import { DiffOutput, parseUnifiedDiff } from "./DiffOutput";
import { ImageStrip } from "./ImageStrip";
import "./ToolRenderers.css";

export type ToolSurface = "group" | "question" | "workflow";

export type ToolViewFamily =
  | "file-list"
  | "grep"
  | "read"
  | "diff"
  | "terminal"
  | "browser-snapshot"
  | "browser-evaluate"
  | "browser-screenshot"
  | "browser-console"
  | "browser-network"
  | "browser-json"
  | "browser-page"
  | "memory"
  | "persistent"
  | "question"
  | "agent-run"
  | "agent-wait"
  | "agent-note"
  | "web-search"
  | "raw";

/**
 * The bucket one block row is counted in when the block states what it did.
 *
 * `reasoning` and `hook` have no registry entry — they are not tool calls at
 * all — but they share the bucket vocabulary because they share the block.
 */
export type SummaryKind =
  | "commands"
  | "fileChanges"
  | "agents"
  | "mcp"
  | "browser"
  | "fork"
  | "hook"
  | "state"
  | "skill"
  | "memory"
  | "search"
  | "reads"
  | "files"
  | "reasoning"
  | "other";
type Translate = ReturnType<typeof useI18n>["t"];
type PresentationResolver = (item: ToolContext, t: Translate) => string | undefined;
type TitleResolver = (t: Translate) => string;

interface ToolViewConfig {
  surface: ToolSurface;
  family: ToolViewFamily;
  icon: LucideIcon;
  doneTitle: TitleResolver;
  runningTitle?: TitleResolver;
  failedTitle?: TitleResolver;
  /** `undefined` falls back to the phase's static title. */
  resolveTitle?: (item: ToolContext, phase: "done" | "running" | "failed", t: Translate) => string | undefined;
  /**
   * Action-multiplexed tools resolve their icon and detail family per call.
   *
   * `todo` is one wire tool covering four operations; without these, creating a
   * task and reading the list would render as one generic card. Ordinary tools
   * leave both unset and use the static fields.
   */
  resolveIcon?: (item: ToolContext) => LucideIcon;
  resolveFamily?: (item: ToolContext) => ToolViewFamily;
  target?: PresentationResolver;
  stat?: PresentationResolver;
  /**
   * The arguments worth reading, in the order the card should show them.
   *
   * A call carries whatever plumbing its transport needed; the reader only wants
   * the few that say what it did. Naming them per tool keeps that judgement where
   * every other judgement about the tool already lives. Omitted means "show them
   * all" — the honest answer for a tool nobody has curated, which is why MCP and
   * unknown tools fall through to it.
   */
  keys?: readonly string[];
  summaryKind: SummaryKind;
}

export interface ToolPresentation {
  surface: ToolSurface;
  family: ToolViewFamily;
  icon: LucideIcon;
  title: string;
  target?: string;
  stat?: string;
  keys?: readonly string[];
}

interface ParsedJson {
  parsed: boolean;
  value: unknown;
}

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function inputString(item: ToolContext, name: string): string | undefined {
  const value = item.input[name];
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function inputNumber(item: ToolContext, name: string): number | undefined {
  const value = item.input[name];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function inputFlag(item: ToolContext, name: string): boolean {
  return item.input[name] === true;
}

/** `web_fetch` titles show the first URL and summarize later URLs as `+N`. */
function firstUrl(item: ToolContext): string | undefined {
  const urls = item.input.urls;
  if (!Array.isArray(urls)) return undefined;
  const first = urls.find((url): url is string => typeof url === "string" && Boolean(url.trim()));
  if (!first) return undefined;
  const rest = urls.length - 1;
  return rest > 0 ? `${first.trim()} +${rest}` : first.trim();
}

function compact(value: string | undefined, limit = 108): string | undefined {
  if (!value) return undefined;
  const normalized = value.replace(/\s+/g, " ").trim();
  if (!normalized) return undefined;
  return normalized.length > limit ? `${normalized.slice(0, limit - 1).trimEnd()}…` : normalized;
}

function targetSelector(item: ToolContext): string | undefined {
  return compact(inputString(item, "selector") ?? inputString(item, "ref"));
}

/**
 * The one line a call with no registry entry can show about itself. Arguments
 * arrive in the order the schema declares them, so the first scalar is the one
 * the tool's author put first.
 */
function firstArgumentSummary(item: ToolContext): string | undefined {
  for (const value of Object.values(item.input)) {
    if (typeof value === "string") {
      const text = compact(value);
      if (text) return text;
      continue;
    }
    if (typeof value === "number" || typeof value === "boolean") return String(value);
  }
  return undefined;
}

function outputLines(value: string): string[] {
  return value.replace(/\r\n?/g, "\n").split("\n").filter((line, index, lines) => line || index < lines.length - 1);
}

function meaningfulCount(value: string, emptyMessages: string[]): number {
  const trimmed = value.trim();
  if (!trimmed || emptyMessages.includes(trimmed)) return 0;
  return outputLines(trimmed).filter((line) => line.trim() && !line.startsWith("… 已达到")).length; // i18n-audit-ignore: parses a backend truncation sentinel
}

function parseJsonOutput(item: ToolContext): ParsedJson {
  const value = item.result.output.trim();
  if (!value) return { parsed: false, value: null };
  try {
    return { parsed: true, value: JSON.parse(value) as unknown };
  } catch {
    return { parsed: false, value: null };
  }
}

function parsedRecord(item: ToolContext): Record<string, unknown> | null {
  const parsed = parseJsonOutput(item);
  return parsed.parsed ? object(parsed.value) : null;
}

function arrayLength(value: unknown): number | undefined {
  return Array.isArray(value) ? value.length : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function browserDuration(item: ToolContext): string | undefined {
  return item.result.durationMs > 0 ? `${item.result.durationMs} ms` : undefined;
}

function fileListStat(item: ToolContext, t: Translate): string {
  const count = meaningfulCount(item.result.output, ["（目录为空）", "未找到匹配文件"]); // i18n-audit-ignore: parses legacy backend sentinels
  return t("{count} 项", "{count} items", { count });
}

function grepStat(item: ToolContext, t: Translate): string {
  const count = meaningfulCount(item.result.output, ["未找到匹配内容"]); // i18n-audit-ignore: parses a legacy backend sentinel
  return t("{count} 条", "{count} entries", { count });
}

function readStat(item: ToolContext, t: Translate): string | undefined {
  const images = item.result.images ?? [];
  if (images.length === 1) return `${images[0].width}×${images[0].height}`;
  if (images.length > 1) {
    return t("{count} 张图片", "{count} images", { count: images.length });
  }
  const start = Math.max(1, Math.floor(inputNumber(item, "start_line") ?? 1));
  const requestedEnd = inputNumber(item, "end_line");
  const lineCount = outputLines(item.result.output).length;
  const end = requestedEnd === undefined ? (lineCount ? start + lineCount - 1 : start) : Math.floor(requestedEnd);
  return lineCount ? t("{start}–{end} 行", "{start}–{end} lines", { start, end: Math.max(start, end) }) : undefined;
}

function diffStat(item: ToolContext): string | undefined {
  if (!item.result.diff) return undefined;
  const parsed = parseUnifiedDiff(item.result.diff);
  return `+${parsed.additions} −${parsed.deletions}`;
}

function writeTitle(item: ToolContext, phase: "done" | "running" | "failed", t: Translate): string {
  if (phase === "running") return t("正在写入文件", "Writing file");
  if (phase === "failed") return t("写入文件失败", "Failed to write file");
  return item.result.diff?.replace(/\r\n?/g, "\n").split("\n").some((line) => line === "--- /dev/null")
    ? t("创建了文件", "Created file")
    : t("写入了文件", "Wrote file");
}

/** Non-empty output lines, which is how every text-answering preview tool reports its size. */
function outputLineCount(item: ToolContext): number {
  return outputLines(item.result.output).filter((line) => line.trim()).length;
}

/**
 * Text-answering preview tools word their own empty case, and those sentences are
 * the source's — they must not be counted as one entry.
 */
const PREVIEW_EMPTY_ANSWERS = [
  "No console logs.",
  "No network requests recorded.",
  "No failed requests.",
  "No logs yet.",
  "No output yet."
];

function previewEntryStat(item: ToolContext, t: Translate): string | undefined {
  const trimmed = item.result.output.trim();
  if (!trimmed) return browserDuration(item);
  const count = PREVIEW_EMPTY_ANSWERS.includes(trimmed) ? 0 : outputLineCount(item);
  return t("{count} 条", "{count} entries", { count });
}

function screenshotStat(item: ToolContext): string | undefined {
  const result = parsedRecord(item);
  const width = numberValue(result?.width);
  const height = numberValue(result?.height);
  return width !== undefined && height !== undefined ? `${width}×${height}` : browserDuration(item);
}

function uploadImageStat(item: ToolContext): string | undefined {
  const raw = item.input.image_id;
  const reference = typeof raw === "number" ? String(raw) : typeof raw === "string" ? raw.trim() : "";
  const match = reference.match(/^(?:\[Image #|#)?([0-9]{1,9})\]?$/);
  return match ? `#${match[1]}` : browserDuration(item);
}

/** `preview_resize` reports the size it was asked for; a preset carries no numbers. */
function resizeStat(item: ToolContext): string | undefined {
  const width = inputNumber(item, "width");
  const height = inputNumber(item, "height");
  return width !== undefined && height !== undefined ? `${width}×${height}` : browserDuration(item);
}
/**
 * Presentation of one `todo` action.
 *
 * The task-state surface is one wire tool multiplexed by `action`, but creating a
 * task and reading the list are not the same event to a reader. Each action
 * keeps the icon, titles, target, stat AND detail family it had when it was its
 * own catalog entry — writes get the structured state view, reads keep the raw
 * output.
 */
interface StateToolActionView {
  family: ToolViewFamily;
  icon: LucideIcon;
  doneTitle: TitleResolver;
  runningTitle: TitleResolver;
  failedTitle: TitleResolver;
  target?: PresentationResolver;
  stat?: PresentationResolver;
}

/**
 * Task status is a wire enum. The old compact state marker localized
 * it and the ordinary row has to keep doing so: `in_progress` on screen is a
 * protocol value leaking into copy, and it is untranslatable for an English
 * reader who never sees the wire.
 */
function todoStatusLabel(item: ToolContext, t: Translate): string | undefined {
  switch (inputString(item, "status")) {
    case "pending": return t("待处理", "Pending");
    case "in_progress": return t("进行中", "In progress");
    case "completed": return t("已完成", "Completed");
    case "deleted": return t("已删除", "Deleted");
    default: return compact(inputString(item, "status"));
  }
}

const TODO_ACTION_VIEWS: Record<string, StateToolActionView> = {
  create: {
    family: "persistent",
    icon: ListChecks,
    doneTitle: (t) => t("创建了任务", "Created task"),
    runningTitle: (t) => t("正在创建任务", "Creating task"),
    failedTitle: (t) => t("创建任务失败", "Failed to create task"),
    target: (item) => compact(inputString(item, "subject"))
  },
  update: {
    family: "persistent",
    icon: ListChecks,
    doneTitle: (t) => t("更新了任务", "Updated task"),
    runningTitle: (t) => t("正在更新任务", "Updating task"),
    failedTitle: (t) => t("更新任务失败", "Failed to update task"),
    target: (item) => compact(inputString(item, "subject") ?? inputString(item, "taskId")),
    stat: todoStatusLabel
  },
  get: {
    family: "raw",
    icon: ListChecks,
    doneTitle: (t) => t("读取了任务", "Read task"),
    runningTitle: (t) => t("正在读取任务", "Reading task"),
    failedTitle: (t) => t("读取任务失败", "Failed to read task"),
    target: (item) => compact(inputString(item, "taskId"))
  },
  list: {
    family: "raw",
    icon: ListChecks,
    doneTitle: (t) => t("列出了任务", "Listed tasks"),
    runningTitle: (t) => t("正在列出任务", "Listing tasks"),
    failedTitle: (t) => t("列出任务失败", "Failed to list tasks"),
    stat: (item, t) => {
      const count = arrayLength(parsedRecord(item)?.tasks);
      return count === undefined ? undefined : t("{count} 项", "{count} tasks", { count });
    }
  }
};

/**
 * A streamed call can reach the timeline before its arguments finish parsing,
 * so an unresolved action falls back to the write card: it is the surface the
 * call is most likely to be, and it never hides the row.
 */
function stateToolView(
  views: Record<string, StateToolActionView>,
  item: ToolContext
): StateToolActionView {
  const action = inputString(item, "action");
  return (action && views[action]) || views.create;
}

function stateToolTitle(
  views: Record<string, StateToolActionView>,
  item: ToolContext,
  phase: "done" | "running" | "failed",
  t: Translate
): string {
  const view = stateToolView(views, item);
  if (phase === "running") return view.runningTitle(t);
  if (phase === "failed") return view.failedTitle(t);
  return view.doneTitle(t);
}

/**
 * Memory tools take a plain document name and return plain Markdown, so the
 * card shows only that name. There is no owner, scope, version or byte count
 * to project any more: a memory belongs to a location, not to a model.
 */
function memoryTarget(item: ToolContext): string | undefined {
  return compact(inputString(item, "name"), 160);
}

/**
 * How many task envelopes one `task_wait` call actually drained. A wait that
 * returned nothing is a different event from one that collected six results,
 * and the row is the only place that difference shows without expanding.
 *
 * Counted neutrally rather than as "updates": wait blocks through progress to
 * the terminal result, so a normal return carries the result *plus* however many
 * updates piled up behind it. Calling that pile "updates" would mislabel the
 * one envelope the model was actually waiting on.
 */
function waitStat(item: ToolContext, t: Translate): string | undefined {
  if (isLiveCall(item) || !item.result.success) return undefined;
  const count = parseWaitOutput(item.result.output).envelopes.length;
  return count ? t("收到 {count} 条", "{count} collected", { count }) : undefined;
}

/**
 * One site a web call consulted, as the card shows it.
 *
 * `host` is what the chip reads, because a row of page titles is a wall of text
 * while a row of domains is scannable — and the domain is what tells a reader
 * whether the answer came from vendor documentation or a forum.
 */
interface WebSourceChip {
  host: string;
  url: string;
  title?: string;
}

/**
 * The sites a `web_search` or `web_fetch` result names, deduplicated by host.
 *
 * The two backends answer in different shapes and both are load-bearing: a
 * provider-native search returns `{findings, sources}` because its page text is
 * sealed in the provider's own encrypted payload and only the URLs are
 * host-readable, while a catalog provider and every fetch return
 * `{results:[{title,url,content}]}` with real text. Reading both here is what
 * lets one card serve every backend.
 */
function webSourceChips(item: ToolContext): WebSourceChip[] {
  const parsed = parseJsonOutput(item);
  const record = object(parsed.value);
  const entries = [
    ...(Array.isArray(record?.sources) ? record.sources : []),
    ...(Array.isArray(record?.results) ? record.results : [])
  ];
  const chips: WebSourceChip[] = [];
  const seen = new Set<string>();
  for (const entry of entries) {
    const fields = object(entry);
    const url = typeof fields?.url === "string" ? fields.url.trim() : "";
    if (!url) continue;
    let host = "";
    try {
      const parsedUrl = new URL(url);
      if (parsedUrl.protocol !== "http:" && parsedUrl.protocol !== "https:") continue;
      // `www.` is noise on a chip: it never distinguishes two sources and it
      // costs four characters of a very narrow row.
      host = parsedUrl.hostname.replace(/^www\./, "");
    } catch {
      continue;
    }
    if (!host || seen.has(host)) continue;
    seen.add(host);
    const title = typeof fields?.title === "string" && fields.title.trim()
      ? fields.title.trim()
      : undefined;
    chips.push({ host, url, title });
  }
  return chips;
}

/** The collapsed row of a settled `web_search`: how many sites it read. */
function webSearchTitle(
  item: ToolContext,
  phase: "done" | "running" | "failed",
  t: Translate
): string | undefined {
  if (phase !== "done") return undefined;
  const count = webSourceChips(item).length;
  // A search that reports no sources still ran; falling back to the plain title
  // is more honest than announcing zero sites.
  return count ? t("已搜索 {count} 个网站", "Searched {count} sites", { count }) : undefined;
}

/**
 * Every built-in tool is intentionally named here. Similar tools share a
 * renderer family, but no tool silently falls into a category-wide default.
 */
export const TOOL_VIEW_REGISTRY = {
  ls: { surface: "group", family: "file-list", icon: FolderTree, doneTitle: (t) => t("查看了目录", "Viewed directory"), runningTitle: (t) => t("正在查看目录", "Viewing directory"), failedTitle: (t) => t("查看目录失败", "Failed to view directory"), target: (item) => compact(inputString(item, "path") ?? "."), stat: fileListStat, summaryKind: "files" },
  grep: { surface: "group", family: "grep", icon: Search, doneTitle: (t) => t("搜索了文件内容", "Searched file contents"), runningTitle: (t) => t("正在搜索文件内容", "Searching file contents"), failedTitle: (t) => t("搜索文件内容失败", "File-content search failed"), target: (item) => compact(inputString(item, "pattern")), stat: grepStat, summaryKind: "search" },
  powershell: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 PowerShell 命令", "Ran PowerShell command"), runningTitle: (t) => t("正在运行 PowerShell 命令", "Running PowerShell command"), failedTitle: (t) => t("PowerShell 命令失败", "PowerShell command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  bash: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 Bash 命令", "Ran Bash command"), runningTitle: (t) => t("正在运行 Bash 命令", "Running Bash command"), failedTitle: (t) => t("Bash 命令失败", "Bash command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  zsh: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 zsh 命令", "Ran zsh command"), runningTitle: (t) => t("正在运行 zsh 命令", "Running zsh command"), failedTitle: (t) => t("zsh 命令失败", "zsh command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  sh: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 sh 命令", "Ran sh command"), runningTitle: (t) => t("正在运行 sh 命令", "Running sh command"), failedTitle: (t) => t("sh 命令失败", "sh command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  write: { surface: "group", family: "diff", icon: FilePenLine, doneTitle: (t) => t("写入了文件", "Wrote file"), resolveTitle: writeTitle, target: (item) => compact(inputString(item, "path")), stat: diffStat, summaryKind: "fileChanges" },
  edit: { surface: "group", family: "diff", icon: FilePenLine, doneTitle: (t) => t("编辑了文件", "Edited file"), runningTitle: (t) => t("正在编辑文件", "Editing file"), failedTitle: (t) => t("编辑文件失败", "Failed to edit file"), target: (item) => compact(inputString(item, "path")), stat: diffStat, summaryKind: "fileChanges" },
  find: { surface: "group", family: "file-list", icon: FileSearch, doneTitle: (t) => t("查找了文件", "Found files"), runningTitle: (t) => t("正在查找文件", "Finding files"), failedTitle: (t) => t("查找文件失败", "Failed to find files"), target: (item) => compact(inputString(item, "query")), stat: fileListStat, summaryKind: "files" },
  read: { surface: "group", family: "read", icon: FileText, doneTitle: (t) => t("读取了文件", "Read file"), runningTitle: (t) => t("正在读取文件", "Reading file"), failedTitle: (t) => t("读取文件失败", "Failed to read file"), target: (item) => compact(inputString(item, "path")), stat: readStat, summaryKind: "reads" },
  lsp: { surface: "group", family: "raw", icon: Waypoints, doneTitle: (t) => t("查询了代码语义", "Queried code semantics"), runningTitle: (t) => t("正在查询代码语义", "Querying code semantics"), failedTitle: (t) => t("查询代码语义失败", "Code semantics query failed"), target: (item) => compact(inputString(item, "filePath") ?? ""), keys: ["operation", "filePath", "line", "character", "query"], summaryKind: "search" },

  workflow: { surface: "workflow", family: "raw", icon: Workflow, doneTitle: (t) => t("完成了工作流", "Completed workflow"), runningTitle: (t) => t("正在运行工作流", "Running workflow"), failedTitle: (t) => t("工作流运行失败", "Workflow failed"), keys: ["name"], summaryKind: "agents" },
  workflow_step: { surface: "group", family: "agent-run", icon: Bot, doneTitle: (t) => t("完成了工作流步骤", "Completed workflow step"), runningTitle: (t) => t("正在运行工作流步骤", "Running workflow step"), failedTitle: (t) => t("工作流步骤失败", "Workflow step failed"), target: (item) => compact(inputString(item, "label") ?? inputString(item, "name")), summaryKind: "agents" },

  // A search names the sites it read, so the card collapses to that count and
  // opens onto one chip per site. Fetching is told what to read, so it keeps the
  // ordinary card: the URLs are already in the call's own arguments.
  web_search: { surface: "group", family: "web-search", icon: Search, doneTitle: (t) => t("完成了联网搜索", "Completed web search"), runningTitle: (t) => t("正在联网搜索", "Searching the web"), failedTitle: (t) => t("联网搜索失败", "Web search failed"), resolveTitle: webSearchTitle, target: (item) => compact(inputString(item, "query")), keys: ["query"], summaryKind: "search" },
  web_fetch: { surface: "group", family: "raw", icon: Globe, doneTitle: (t) => t("抓取了网页", "Fetched pages"), runningTitle: (t) => t("正在抓取网页", "Fetching pages"), failedTitle: (t) => t("抓取网页失败", "Failed to fetch pages"), target: (item) => compact(firstUrl(item)), keys: ["urls"], summaryKind: "search" },

  // Fifteen flat preview tools, each its own wire tool. Thirteen are copied from
  // Claude Code and answer in prose the tool wrote itself, so most of them render
  // that text as-is; only the calls that answer with JSON or pixels get a
  // structured family.
  preview_start: { surface: "group", family: "raw", icon: MonitorPlay, doneTitle: (t) => t("启动了预览服务器", "Started preview server"), runningTitle: (t) => t("正在启动预览服务器", "Starting preview server"), failedTitle: (t) => t("启动预览服务器失败", "Failed to start preview server"), target: (item) => compact(inputString(item, "name")), stat: browserDuration, keys: ["name"], summaryKind: "browser" },
  preview_stop: { surface: "group", family: "raw", icon: MonitorX, doneTitle: (t) => t("停止了预览服务器", "Stopped preview server"), runningTitle: (t) => t("正在停止预览服务器", "Stopping preview server"), failedTitle: (t) => t("停止预览服务器失败", "Failed to stop preview server"), target: (item) => compact(inputString(item, "serverId")), stat: browserDuration, keys: ["serverId"], summaryKind: "browser" },
  preview_list: { surface: "group", family: "browser-json", icon: Server, doneTitle: (t) => t("列出了预览服务器", "Listed preview servers"), runningTitle: (t) => t("正在列出预览服务器", "Listing preview servers"), failedTitle: (t) => t("列出预览服务器失败", "Failed to list preview servers"), keys: [], stat: (item, t) => {
    const count = arrayLength(parseJsonOutput(item).value);
    return count === undefined ? browserDuration(item) : t("{count} 个服务器", "{count} servers", { count });
  }, summaryKind: "browser" },
  preview_logs: { surface: "group", family: "raw", icon: Logs, doneTitle: (t) => t("读取了服务器日志", "Read server logs"), runningTitle: (t) => t("正在读取服务器日志", "Reading server logs"), failedTitle: (t) => t("读取服务器日志失败", "Failed to read server logs"), target: (item) => compact(inputString(item, "search")), stat: previewEntryStat, keys: ["search", "level", "lines"], summaryKind: "browser" },
  preview_console_logs: { surface: "group", family: "browser-console", icon: Logs, doneTitle: (t) => t("读取了 Console 日志", "Read Console logs"), runningTitle: (t) => t("正在读取 Console 日志", "Reading Console logs"), failedTitle: (t) => t("读取 Console 日志失败", "Failed to read Console logs"), target: (item) => compact(inputString(item, "level")), stat: previewEntryStat, summaryKind: "browser" },
  preview_screenshot: { surface: "group", family: "browser-screenshot", icon: Image, doneTitle: (t) => t("截取了页面", "Captured page"), runningTitle: (t) => t("正在截取页面", "Capturing page"), failedTitle: (t) => t("页面截图失败", "Page capture failed"), stat: screenshotStat, summaryKind: "browser" },
  preview_snapshot: { surface: "group", family: "browser-snapshot", icon: ScanSearch, doneTitle: (t) => t("读取了页面快照", "Read page snapshot"), runningTitle: (t) => t("正在读取页面快照", "Reading page snapshot"), failedTitle: (t) => t("读取页面快照失败", "Failed to read page snapshot"), stat: (item, t) => {
    const count = outputLineCount(item);
    return count ? t("{count} 个节点", "{count} nodes", { count }) : browserDuration(item);
  }, summaryKind: "browser" },
  preview_inspect: { surface: "group", family: "browser-json", icon: Crosshair, doneTitle: (t) => t("检查了页面元素", "Inspected page element"), runningTitle: (t) => t("正在检查页面元素", "Inspecting page element"), failedTitle: (t) => t("检查页面元素失败", "Failed to inspect page element"), target: targetSelector, stat: browserDuration, keys: ["selector", "styles"], summaryKind: "browser" },
  preview_click: { surface: "group", family: "raw", icon: MousePointerClick, doneTitle: (t) => t("点击了页面元素", "Clicked page element"), runningTitle: (t) => t("正在点击页面元素", "Clicking page element"), failedTitle: (t) => t("点击页面元素失败", "Failed to click page element"), target: targetSelector, keys: ["selector", "doubleClick"], stat: (item, t) => (inputFlag(item, "doubleClick") ? t("双击", "Double-click") : browserDuration(item)), summaryKind: "browser" },
  preview_fill: { surface: "group", family: "raw", icon: TextCursorInput, doneTitle: (t) => t("填写了页面输入", "Filled page input"), runningTitle: (t) => t("正在填写页面输入", "Filling page input"), failedTitle: (t) => t("填写页面输入失败", "Failed to fill page input"), target: targetSelector, keys: ["selector", "value"], stat: (item, t) => {
    const value = inputString(item, "value");
    return value === undefined ? browserDuration(item) : t("{count} 字", "{count} characters", { count: value.length });
  }, summaryKind: "browser" },
  preview_eval: { surface: "group", family: "browser-evaluate", icon: Code2, doneTitle: (t) => t("执行了页面脚本", "Ran page script"), runningTitle: (t) => t("正在执行页面脚本", "Running page script"), failedTitle: (t) => t("页面脚本执行失败", "Page script failed"), target: (item) => compact(inputString(item, "expression")), stat: browserDuration, summaryKind: "browser" },
  preview_network: { surface: "group", family: "browser-network", icon: Network, doneTitle: (t) => t("读取了网络请求", "Read network requests"), runningTitle: (t) => t("正在读取网络请求", "Reading network requests"), failedTitle: (t) => t("读取网络请求失败", "Failed to read network requests"), target: (item) => compact(inputString(item, "requestId") ?? inputString(item, "filter")), stat: previewEntryStat, summaryKind: "browser" },
  preview_resize: { surface: "group", family: "raw", icon: MonitorUp, doneTitle: (t) => t("调整了预览视口", "Resized preview viewport"), runningTitle: (t) => t("正在调整预览视口", "Resizing preview viewport"), failedTitle: (t) => t("调整预览视口失败", "Failed to resize preview viewport"), target: (item) => compact(inputString(item, "preset") ?? inputString(item, "colorScheme")), stat: resizeStat, keys: ["preset", "width", "height", "colorScheme"], summaryKind: "browser" },
  preview_upload_image: { surface: "group", family: "browser-page", icon: Upload, doneTitle: (t) => t("上传了对话图片", "Uploaded conversation image"), runningTitle: (t) => t("正在上传对话图片", "Uploading conversation image"), failedTitle: (t) => t("上传对话图片失败", "Failed to upload conversation image"), target: targetSelector, stat: uploadImageStat, summaryKind: "browser" },
  preview_dialog: { surface: "group", family: "browser-page", icon: CircleAlert, doneTitle: (t) => t("回答了页面对话框", "Answered page dialog"), runningTitle: (t) => t("正在回答页面对话框", "Answering page dialog"), failedTitle: (t) => t("回答页面对话框失败", "Failed to answer page dialog"), target: (item) => compact(inputString(item, "prompt_text")), stat: (item, t) => (
    item.input.accept === false ? t("取消", "Dismiss") : t("接受", "Accept")
  ), summaryKind: "browser" },

  // Agent-protocol tools are ordinary rows inside the tool block: one uniform
  // disclosure per contiguous tool run, chevron → tailored detail. `agent-run`
  // carries a child transcript (instruction, updates, returned text) and is the
  // only family that offers the inline "open subagent" affordance; `agent-wait`
  // splits the drained `[name · status]` envelopes; `agent-note` is a one-shot
  // message whose payload already sits in the row's target.
  agent_spawn: { surface: "group", family: "agent-run", icon: Bot, doneTitle: (t) => t("派生了子代理", "Spawned subagent"), runningTitle: (t) => t("正在派生子代理", "Spawning subagent"), failedTitle: (t) => t("派生子代理失败", "Failed to spawn subagent"), target: (item) => compact(inputString(item, "label") ?? inputString(item, "name")), summaryKind: "agents" },
  agent_send: { surface: "group", family: "agent-run", icon: MessageCircleMore, doneTitle: (t) => t("向子代理发送了消息", "Sent message to subagent"), runningTitle: (t) => t("正在向子代理发送消息", "Sending message to subagent"), failedTitle: (t) => t("发送子代理消息失败", "Failed to send subagent message"), target: (item) => compact(inputString(item, "agent")), summaryKind: "agents" },
  send_message: {
    surface: "group",
    family: "agent-run",
    icon: MessageCircleMore,
    doneTitle: (t) => t("消息已加入子代理队列", "Queued message for subagent"),
    runningTitle: (t) => t("正在排队子代理消息", "Queueing message for subagent"),
    failedTitle: (t) => t("排队子代理消息失败", "Failed to queue subagent message"),
    // The child form travels upward and opens nothing, so it is a note rather
    // than an agent run: no target to name, no child transcript to disclose.
    resolveFamily: (item) => (isChildMainMessageContext(item) ? "agent-note" : "agent-run"),
    resolveTitle: (item, phase, t) => {
      if (!isChildMainMessageContext(item)) return undefined;
      if (phase === "running") return t("正在向主代理发送消息", "Sending message to main agent");
      if (phase === "failed") return t("向主代理发送消息失败", "Failed to send message to main agent");
      return t("向主代理发送了消息", "Sent message to main agent");
    },
    target: (item) => compact(
      isChildMainMessageContext(item) ? inputString(item, "message") : inputString(item, "target")
    ),
    summaryKind: "agents"
  },
  followup_task: { surface: "group", family: "agent-run", icon: MessageCircleMore, doneTitle: (t) => t("向子代理追加了任务", "Sent follow-up to subagent"), runningTitle: (t) => t("正在追加子代理任务", "Sending subagent follow-up"), failedTitle: (t) => t("追加子代理任务失败", "Failed to follow up with subagent"), target: (item) => compact(inputString(item, "target")), summaryKind: "agents" },
  task_wait: { surface: "group", family: "agent-wait", icon: Timer, doneTitle: (t) => t("等待了任务", "Waited for tasks"), runningTitle: (t) => t("正在等待任务", "Waiting for tasks"), failedTitle: (t) => t("等待任务失败", "Failed to wait for tasks"), stat: waitStat, summaryKind: "agents" },
  task_list: { surface: "group", family: "agent-note", icon: ListChecks, doneTitle: (t) => t("查看了任务列表", "Viewed task list"), runningTitle: (t) => t("正在查看任务列表", "Viewing task list"), failedTitle: (t) => t("查看任务列表失败", "Failed to view task list"), summaryKind: "agents" },
  // The host fabricates the call and owns the body, so the row only ever
  // settles as done; the notification text is the result, hence raw detail.
  box: { surface: "group", family: "raw", icon: Inbox, doneTitle: (t) => t("送达了后台结果", "Delivered a background result"), keys: [], summaryKind: "agents" },

  // Skills return instruction text, so they use raw detail; the skill name is
  // this call's only input and becomes its target.
  skill: { surface: "group", family: "raw", icon: Box, doneTitle: (t) => t("加载了技能", "Loaded skill"), runningTitle: (t) => t("正在加载技能", "Loading skill"), failedTitle: (t) => t("加载技能失败", "Failed to load skill"), target: (item) => compact(inputString(item, "name")), keys: ["name"], summaryKind: "skill" },
  // Tool discovery returns a `<functions>` block, which is raw text for the same
  // reason a skill body is. Its target is the query, because that is what the
  // reader wants to see the run asked for; the MCP summary groups it with the
  // calls it enables.
  tool_search: { surface: "group", family: "raw", icon: Blocks, doneTitle: (t) => t("取回了工具定义", "Fetched tool definitions"), runningTitle: (t) => t("正在查找工具", "Searching for tools"), failedTitle: (t) => t("查找工具失败", "Tool search failed"), target: (item) => compact(inputString(item, "query")), keys: ["query", "max_results"], summaryKind: "mcp" },
  subagent: { surface: "group", family: "agent-run", icon: Bot, doneTitle: (t) => t("运行了子代理", "Ran subagent"), runningTitle: (t) => t("子代理正在工作", "Subagent is working"), failedTitle: (t) => t("子代理已中断", "Subagent was interrupted"), target: (item) => compact(inputString(item, "label")), summaryKind: "agents" },
  subagent_update: { surface: "group", family: "agent-note", icon: MessageCircleMore, doneTitle: (t) => t("子代理更新了状态", "Updated subagent status"), target: (item) => compact(inputString(item, "message")), summaryKind: "agents" },
  structured_output: { surface: "group", family: "agent-note", icon: MessageCircleMore, doneTitle: (t) => t("子代理交回了结构化结果", "Returned a structured result"), target: () => "", summaryKind: "agents" },
  subagent_activity: { surface: "group", family: "agent-note", icon: Bot, doneTitle: (t) => t("记录了子代理活动", "Recorded subagent activity"), target: (item) => compact(inputString(item, "message")), summaryKind: "agents" },
  update: { surface: "group", family: "agent-note", icon: MessageCircleMore, doneTitle: (t) => t("子代理更新了状态", "Updated subagent status"), target: (item) => compact(inputString(item, "message") ?? inputString(item, "content")), summaryKind: "agents" },

  ask_user: {
    surface: "question",
    family: "question",
    icon: CircleHelp,
    doneTitle: (t) => t("向用户提出了问题", "Asked the user a question"),
    target: (item) => compact(questionsFromInput(item.input)[0]?.question ?? ""),
    summaryKind: "other"
  },
  // A fork call only raises the request and returns; the receipt states that,
  // so the row carries the prompt's first line. An ordinary row — the request
  // has no card of its own.
  fork: {
    surface: "group",
    family: "raw",
    icon: GitFork,
    doneTitle: (t) => t("请求了分叉会话", "Requested a forked conversation"),
    runningTitle: (t) => t("正在请求分叉会话", "Requesting a forked conversation"),
    failedTitle: (t) => t("请求分叉会话失败", "Failed to request a forked conversation"),
    target: (item) => compact(inputString(item, "prompt")?.split("\n", 1)[0]),
    keys: ["prompt"],
    summaryKind: "fork"
  },
  todo: {
    surface: "group",
    family: "persistent",
    icon: ListChecks,
    doneTitle: (t) => t("更新了任务清单", "Updated task list"),
    resolveTitle: (item, phase, t) => stateToolTitle(TODO_ACTION_VIEWS, item, phase, t),
    resolveIcon: (item) => stateToolView(TODO_ACTION_VIEWS, item).icon,
    resolveFamily: (item) => stateToolView(TODO_ACTION_VIEWS, item).family,
    target: (item, t) => stateToolView(TODO_ACTION_VIEWS, item).target?.(item, t),
    stat: (item, t) => stateToolView(TODO_ACTION_VIEWS, item).stat?.(item, t),
    summaryKind: "state"
  },
  // The plan itself has a page of its own, so these rows only say what happened;
  // the body is never repeated inline.
  plan: {
    surface: "group",
    family: "raw",
    icon: NotebookPen,
    doneTitle: (t) => t("已更新计划", "Updated the plan"),
    resolveTitle: (item, phase, t) => (
      phase === "running"
        ? t("正在撰写计划", "Writing the plan")
        : phase === "failed"
          ? t("计划操作失败", "Plan operation failed")
          : inputString(item, "action") === "read"
            ? t("已读取计划", "Read the plan")
            : t("已更新计划", "Updated the plan")
    ),
    keys: ["action"],
    summaryKind: "other"
  },
  exit_plan_mode: {
    surface: "group",
    family: "raw",
    icon: ClipboardCheck,
    doneTitle: (t) => t("已提交计划", "Submitted the plan"),
    runningTitle: (t) => t("等待批准计划", "Waiting for plan approval"),
    failedTitle: (t) => t("提交计划失败", "Failed to submit the plan"),
    keys: [],
    summaryKind: "other"
  },
  read_global_memory: {
    surface: "group",
    family: "memory",
    icon: FileText,
    doneTitle: (t) => t("读取了全局记忆", "Read global memory"),
    runningTitle: (t) => t("正在读取全局记忆", "Reading global memory"),
    failedTitle: (t) => t("读取全局记忆失败", "Failed to read global memory"),
    target: memoryTarget,
    summaryKind: "memory"
  },
  read_project_memory: {
    surface: "group",
    family: "memory",
    icon: FileText,
    doneTitle: (t) => t("读取了项目记忆", "Read project memory"),
    runningTitle: (t) => t("正在读取项目记忆", "Reading project memory"),
    failedTitle: (t) => t("读取项目记忆失败", "Failed to read project memory"),
    target: memoryTarget,
    summaryKind: "memory"
  },
  create_global_memory: {
    surface: "group",
    family: "memory",
    icon: FilePenLine,
    doneTitle: (t) => t("创建了全局记忆", "Created global memory"),
    runningTitle: (t) => t("正在创建全局记忆", "Creating global memory"),
    failedTitle: (t) => t("创建全局记忆失败", "Failed to create global memory"),
    target: memoryTarget,
    summaryKind: "memory"
  },
  create_project_memory: {
    surface: "group",
    family: "memory",
    icon: FilePenLine,
    doneTitle: (t) => t("创建了项目记忆", "Created project memory"),
    runningTitle: (t) => t("正在创建项目记忆", "Creating project memory"),
    failedTitle: (t) => t("创建项目记忆失败", "Failed to create project memory"),
    target: memoryTarget,
    summaryKind: "memory"
  },
  edit_global_memory: {
    surface: "group",
    family: "memory",
    icon: FilePenLine,
    doneTitle: (t) => t("编辑了全局记忆", "Edited global memory"),
    runningTitle: (t) => t("正在编辑全局记忆", "Editing global memory"),
    failedTitle: (t) => t("编辑全局记忆失败", "Failed to edit global memory"),
    target: memoryTarget,
    summaryKind: "memory"
  },
  edit_project_memory: {
    surface: "group",
    family: "memory",
    icon: FilePenLine,
    doneTitle: (t) => t("编辑了项目记忆", "Edited project memory"),
    runningTitle: (t) => t("正在编辑项目记忆", "Editing project memory"),
    failedTitle: (t) => t("编辑项目记忆失败", "Failed to edit project memory"),
    target: memoryTarget,
    summaryKind: "memory"
  }
} as const satisfies Record<string, ToolViewConfig>;

function registryEntry(name: string): ToolViewConfig | undefined {
  return (TOOL_VIEW_REGISTRY as Record<string, ToolViewConfig>)[name];
}

/**
 * The timeline surface a tool renders on.
 *
 * Only two tools leave the ordinary block: `ask_user`, whose history card
 * carries the user's own answer, and `workflow`, which draws a card of its own
 * for the whole life of the run. Everything else — including every
 * agent-protocol and task state call — is an ordinary row inside the one
 * tool block for its contiguous run.
 */
export function toolSurfaceForName(name: string): ToolSurface {
  return registryEntry(name)?.surface ?? "group";
}

/**
 * True while a streamed tool call has not yet reached its terminal status.
 *
 * A persisted context never carries `streaming`, so this is false for every
 * item the timeline reloads from storage — which is the point.
 */
function isLiveCall(item: ToolContext): boolean {
  return item.streaming === true && item.streamStatus !== "completed";
}

/**
 * Whether this call belongs in the ordinary two-level tool disclosure.
 *
 * A `workflow` call never does, running or settled. Its card is built from the
 * agent roster rather than from live stream events, and the roster outlives the
 * model run, so a finished plan keeps the same card it had while it ran instead
 * of collapsing into a row the moment it succeeds. Every filter that assembles
 * the ordinary group must agree, or the call is counted in the summary and then
 * never rendered (or the reverse).
 */
function isOrdinaryToolCall(item: ToolContext): boolean {
  return toolSurfaceForName(item.toolName) === "group";
}

/**
 * Whether the timeline hoists this call onto the end-of-stream waiting
 * indicator instead of drawing a block for it.
 *
 * An ordinary call in flight has nothing a block can show that the one-line
 * activity does not: its result is empty until the host reports it, so the card
 * would be a heading and a spinner occupying a full row, then replaced the
 * moment the real receipt lands. Hoisting it keeps "what is happening right
 * now" in one place — the indicator — and leaves the timeline to settled work.
 *
 * The two non-ordinary surfaces keep their own live representation and hoist
 * only before execution starts, exactly as they always did: `workflow` draws a
 * progress card for the whole run, and `ask_user` is answered in its dock.
 */
export function isHoistedToolCall(item: ToolContext): boolean {
  if (!isLiveCall(item)) return false;
  return isOrdinaryToolCall(item)
    || item.streamStatus === "announced"
    || item.streamStatus === "ready";
}

function executionPhase(item: ToolContext): "done" | "running" | "failed" {
  if (item.streaming && item.streamStatus !== "completed") return "running";
  return item.result.success ? "done" : "failed";
}

/**
 * MCP tools are named by the host, never by this registry:
 * `mcp__<server-slug>__<tool-slug>__<digest>`. The prefix is the whole contract
 * — matching it is what lets a server the user installed today render as an MCP
 * call rather than as an unrecognized one.
 */
export function isMcpToolName(name: string): boolean {
  return name.startsWith("mcp__");
}

/**
 * The server and tool an MCP call names, in the words the user chose.
 *
 * The descriptor's label carries both verbatim (`MCP <server> / <tool>`), so it
 * is preferred; a trailing parenthetical is the host's manual-confirmation
 * notice and belongs in the approval dock, not in a timeline row. Without a
 * descriptor — an archived conversation whose server has since been removed —
 * the wire name still yields readable slugs, and the server slug's trailing
 * digest is dropped because it identifies nothing the reader can act on.
 */
export function mcpToolNaming(item: ToolContext, descriptor?: ToolDescriptor): { server?: string; tool: string } {
  const labelled = /^MCP\s+(.+?)\s+\/\s+(.+)$/.exec(descriptor?.label?.trim() ?? "");
  if (labelled) {
    return { server: labelled[1].trim(), tool: labelled[2].replace(/\s*\([^)]*\)\s*$/, "").trim() };
  }
  const parts = item.toolName.split("__");
  return parts.length >= 3
    ? { server: parts[1].replace(/_[0-9a-f]{10}$/, ""), tool: parts[2] }
    : { tool: item.toolName };
}

/**
 * The bucket this call is counted in by the block heading.
 *
 * MCP is decided by the wire name because those tools are discovered at
 * runtime and have no registry entry to carry a bucket.
 */
export function toolSummaryKind(item: ToolContext): SummaryKind {
  if (isMcpToolName(item.toolName)) return "mcp";
  return registryEntry(item.toolName)?.summaryKind ?? "other";
}

/**
 * What a block row calls the tool: its name, not a sentence about it.
 *
 * The row is one line — icon, name, one line of the call itself — so the
 * localized phrase ("Ran Bash command") would crowd out the argument that says
 * what actually happened. That phrase is not lost: it stays the row's
 * accessible name and its tooltip, and the block heading counts it.
 */
export function toolRowName(item: ToolContext, descriptor?: ToolDescriptor): string {
  if (isMcpToolName(item.toolName)) return mcpToolNaming(item, descriptor).tool;
  return item.toolName;
}

export function getToolPresentation(
  item: ToolContext,
  descriptor?: ToolDescriptor,
  t: Translate = globalT
): ToolPresentation {
  const config = registryEntry(item.toolName);
  if (!config) {
    const phase = executionPhase(item);
    const duration = item.result.durationMs > 0 ? `${item.result.durationMs} ms` : undefined;
    if (isMcpToolName(item.toolName)) {
      const { server, tool } = mcpToolNaming(item, descriptor);
      const target = firstArgumentSummary(item) ?? server;
      return {
        surface: "group",
        family: "raw",
        icon: Plug,
        title: phase === "running"
          ? t("正在调用 MCP 工具 {label}", "Calling MCP tool {label}", { label: tool })
          : phase === "failed"
            ? t("MCP 工具 {label} 调用失败", "MCP tool {label} failed", { label: tool })
            : t("调用了 MCP 工具 {label}", "Called MCP tool {label}", { label: tool }),
        ...(target ? { target } : {}),
        ...(duration ? { stat: duration } : {})
      };
    }
    const label = descriptor?.label?.trim() || item.toolName;
    return {
      surface: "group",
      family: "raw",
      icon: Wrench,
      title: phase === "running"
        ? t("正在使用 {label}", "Using {label}", { label })
        : phase === "failed"
          ? t("{label}执行失败", "{label} failed", { label })
          : t("使用了 {label}", "Used {label}", { label }),
      stat: duration
    };
  }
  const phase = executionPhase(item);
  const title = config.resolveTitle?.(item, phase, t)
    ?? (phase === "running" ? config.runningTitle : phase === "failed" ? config.failedTitle : config.doneTitle)?.(t)
    ?? config.doneTitle(t);
  const target = config.target?.(item, t);
  const stat = config.stat?.(item, t);
  return {
    surface: config.surface,
    family: config.resolveFamily?.(item) ?? config.family,
    icon: config.resolveIcon?.(item) ?? config.icon,
    title,
    ...(target ? { target } : {}),
    ...(stat ? { stat } : {}),
    ...(config.keys ? { keys: config.keys } : {})
  };
}

function summaryPhrase(kind: SummaryKind, count: number, t: Translate): string {
  switch (kind) {
    case "commands": return count === 1
      ? t("运行了 1 个命令", "Ran 1 command")
      : t("运行了 {count} 个命令", "Ran {count} commands", { count });
    case "fileChanges": return count === 1
      ? t("1 次文件操作", "1 file change")
      : t("{count} 次文件操作", "{count} file changes", { count });
    case "agents": return count === 1
      ? t("调用了 1 个子代理", "Called 1 subagent")
      : t("调用了 {count} 个子代理", "Called {count} subagents", { count });
    case "mcp": return count === 1
      ? t("调用了 1 个 MCP 工具", "Called 1 MCP tool")
      : t("调用了 {count} 个 MCP 工具", "Called {count} MCP tools", { count });
    case "browser": return count === 1
      ? t("执行了 1 次浏览器操作", "Performed 1 browser action")
      : t("执行了 {count} 次浏览器操作", "Performed {count} browser actions", { count });
    case "fork": return count === 1
      ? t("请求了 1 次会话分叉", "Requested 1 conversation fork")
      : t("请求了 {count} 次会话分叉", "Requested {count} conversation forks", { count });
    case "hook": return count === 1
      ? t("触发了 1 个 hook", "Triggered 1 hook")
      : t("触发了 {count} 个 hook", "Triggered {count} hooks", { count });
    case "state": return count === 1
      ? t("1 次状态变更", "1 state change")
      : t("{count} 次状态变更", "{count} state changes", { count });
    case "skill": return count === 1
      ? t("读取了 1 个技能", "Loaded 1 skill")
      : t("读取了 {count} 个技能", "Loaded {count} skills", { count });
    case "memory": return count === 1
      ? t("访问了 1 次记忆", "Accessed memory once")
      : t("访问了 {count} 次记忆", "Accessed memory {count} times", { count });
    case "search": return count === 1
      ? t("搜索了 1 次内容", "Performed 1 content search")
      : t("搜索了 {count} 次内容", "Performed {count} content searches", { count });
    case "reads": return count === 1
      ? t("读取了 1 个文件", "Read 1 file")
      : t("读取了 {count} 个文件", "Read {count} files", { count });
    case "files": return count === 1
      ? t("检查了 1 次文件与目录", "Checked files and directories once")
      : t("检查了 {count} 次文件与目录", "Checked files and directories {count} times", { count });
    case "reasoning": return count === 1
      ? t("思考了 1 次", "Thought once")
      : t("思考了 {count} 次", "Thought {count} times", { count });
    default: return count === 1
      ? t("使用了 1 个工具", "Used 1 tool")
      : t("使用了 {count} 个工具", "Used {count} tools", { count });
  }
}

/**
 * Buckets in the order the heading states them: most consequential first.
 *
 * The heading is one line and ellipsizes, so this order decides what survives
 * a narrow window. Work that changed something outside the conversation leads;
 * reads, which are the most numerous and the least consequential, trail.
 */
const SUMMARY_ORDER: readonly SummaryKind[] = [
  "commands",
  "fileChanges",
  "agents",
  "mcp",
  "browser",
  "fork",
  "hook",
  "state",
  "skill",
  "memory",
  "search",
  "reads",
  "files",
  "reasoning",
  "other"
];

/**
 * What one block did, as a sentence of counted clauses.
 *
 * A block mixes reasoning, hooks and every kind of tool call, so no single
 * clause can describe it; the heading names each bucket it contains and lets
 * {@link SUMMARY_ORDER} decide which ones stay visible when it truncates.
 */
export function summarizeBlockKinds(kinds: SummaryKind[], t: Translate = globalT): string {
  const counts = new Map<SummaryKind, number>();
  for (const kind of kinds) counts.set(kind, (counts.get(kind) ?? 0) + 1);
  return SUMMARY_ORDER
    .flatMap((kind) => {
      const count = counts.get(kind) ?? 0;
      return count ? [summaryPhrase(kind, count, t)] : [];
    })
    .join(t("，", ", "));
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

function RawDataDisclosure({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  return (
    <details className="tool-renderer__raw" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>{t("原始数据", "Raw data")}</summary>
      {open && <pre>{safeStringify({
        tool: item.toolName,
        input: item.input,
        result: item.result
      })}</pre>}
    </details>
  );
}

function EmptyResult({ children }: { children?: ReactNode }) {
  const { t } = useI18n();
  return <div className="tool-renderer__empty">{children ?? t("没有可显示的结果", "No results to display")}</div>;
}

function FileListView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const lines = outputLines(item.result.output).filter((line) => line.trim());
  const empty = !lines.length || (lines.length === 1 && ["（目录为空）", "未找到匹配文件"].includes(lines[0].trim())); // i18n-audit-ignore: parses legacy backend sentinels
  if (empty) return <EmptyResult>{lines[0]?.trim() || t("没有文件", "No files")}</EmptyResult>;
  return (
    <ul className="tool-renderer__file-list" aria-label={t("文件结果", "File results")}>
      {lines.map((line, index) => {
        const value = line.trimEnd();
        const meta = value.startsWith("… ");
        const directory = value.endsWith("/");
        const Icon = directory ? Folder : File;
        return (
          <li key={`${index}:${value}`} className={meta ? "tool-renderer__file-meta" : undefined}>
            {!meta && <Icon size={13} aria-hidden="true" />}
            <code>{value}</code>
          </li>
        );
      })}
    </ul>
  );
}

interface GrepLine {
  raw: string;
  path?: string;
  line?: number;
  content?: string;
}

function parseGrepLine(raw: string): GrepLine {
  const match = /^(.+?):(\d+):(.*)$/.exec(raw);
  if (!match) return { raw };
  return { raw, path: match[1], line: Number(match[2]), content: match[3] };
}

function GrepView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const lines = outputLines(item.result.output).filter((line) => line.trim());
  if (!lines.length || (lines.length === 1 && lines[0].trim() === "未找到匹配内容")) { // i18n-audit-ignore: parses a legacy backend sentinel
    return <EmptyResult>{lines[0]?.trim() || t("未找到匹配内容", "No matching content found")}</EmptyResult>;
  }
  return (
    <ol className="tool-renderer__grep-list" aria-label={t("搜索结果", "Search results")}>
      {lines.map((raw, index) => {
        const match = parseGrepLine(raw);
        if (!match.path || match.line === undefined) {
          return <li key={`${index}:${raw}`} className="tool-renderer__grep-meta"><code>{raw}</code></li>;
        }
        return (
          <li key={`${index}:${raw}`}>
            <span className="tool-renderer__grep-location">
              <code>{match.path}</code>
              <span aria-label={t("第 {line} 行", "Line {line}", { line: match.line })}>{match.line}</span>
            </span>
            <code className="tool-renderer__grep-content">{match.content}</code>
          </li>
        );
      })}
    </ol>
  );
}

function ReadView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const start = Math.max(1, Math.floor(inputNumber(item, "start_line") ?? 1));
  const lines = outputLines(item.result.output);
  if ((!lines.length || !item.result.output) && !item.result.images?.length) {
    return <EmptyResult>{t("文件内容为空", "File is empty")}</EmptyResult>;
  }
  if (!item.result.output) return null;
  const gutter = Math.max(2, String(start + Math.max(0, lines.length - 1)).length);
  return (
    <div
      className="diff-output"
      role="region"
      aria-label={t("{path} 内容", "{path} contents", { path: inputString(item, "path") ?? t("文件", "File") })}
      style={{ "--diff-gutter": `${gutter}ch` } as CSSProperties}
    >
      <div className="diff-output__body">
        {lines.map((line, index) => (
          <div key={`${start + index}:${line}`} className={line.startsWith("… ") ? "diff-output__line diff-output__line--meta" : "diff-output__line diff-output__line--context"}>
            <span className="diff-output__line-number" aria-hidden="true">{start + index}</span>
            <span className="diff-output__content">{line || " "}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function DiffView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  if (item.result.diff) {
    return (
      <DiffOutput
        value={item.result.diff}
        path={inputString(item, "path")}
      />
    );
  }
  return (
    <div className="tool-renderer__notice tool-renderer__notice--success">
      <CircleCheck size={14} aria-hidden="true" />
      <span>{item.result.output || t("操作成功，没有行级变化", "Operation succeeded with no line-level changes")}</span>
    </div>
  );
}

function TerminalView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const command = inputString(item, "command") ?? "";
  return (
    <div className="tool-renderer__terminal">
      <div className="tool-renderer__command">
        <span aria-hidden="true">›</span>
        <pre>{command}</pre>
      </div>
      <pre className="tool-renderer__terminal-output">{item.result.output || t("命令没有输出", "Command produced no output")}</pre>
    </div>
  );
}

function scalarText(value: unknown, t: Translate = globalT): string {
  if (typeof value === "boolean") return value ? t("是", "Yes") : t("否", "No");
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (value === null) return "null";
  return safeStringify(value);
}

function InfoGrid({ rows }: { rows: Array<[string, unknown]> }) {
  const { t } = useI18n();
  const visible = rows.filter(([, value]) => value !== undefined && value !== "");
  if (!visible.length) return null;
  return (
    <dl className="tool-renderer__info-grid">
      {visible.map(([label, value]) => (
        <div key={label}>
          <dt>{label}</dt>
          <dd>{scalarText(value, t)}</dd>
        </div>
      ))}
    </dl>
  );
}

function JsonFallback({ item, parsed }: { item: ToolContext; parsed: ParsedJson }) {
  const { t } = useI18n();
  return (
    <div className="tool-renderer__fallback">
      <CircleAlert size={14} aria-hidden="true" />
      <div>
        <strong>{parsed.parsed ? t("结果结构无法识别", "Unrecognized result structure") : t("结果不是有效 JSON", "Result is not valid JSON")}</strong>
        <pre>{item.result.output || t("没有返回内容", "No content returned")}</pre>
      </div>
    </div>
  );
}

/**
 * `preview_snapshot`. The answer is the accessibility tree the tool already
 * formatted — plain text, not JSON — so the card shows it verbatim rather than
 * rebuilding a structure it does not have.
 */
function BrowserSnapshotView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const text = item.result.output.trim();
  if (!text) return <EmptyResult>{t("快照中没有可读内容", "The snapshot has no readable content")}</EmptyResult>;
  return (
    <pre className="tool-renderer__snapshot-tree" aria-label={t("页面快照", "Page snapshot")}>{text}</pre>
  );
}

/** `preview_eval`. The value is already JSON the tool serialized, or the literal `undefined`. */
function BrowserEvaluateView({ item }: { item: ToolContext }) {
  return (
    <KeyValueCard
      rows={[["JavaScript", inputString(item, "expression")]]}
      result={item.result.output || "undefined"}
    />
  );
}

function BrowserScreenshotView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  return (
    <div className="tool-renderer__screenshot">
      <Image size={22} aria-hidden="true" />
      <InfoGrid rows={[
        [t("尺寸", "Dimensions"), value.width !== undefined && value.height !== undefined ? `${scalarText(value.width)}×${scalarText(value.height)}` : undefined]
      ]} />
    </div>
  );
}

/**
 * The trailing `(Showing last N of M entries.)` note the log tools append. It is
 * a sentence about the read, not one of the entries, so it is lifted out of the
 * list instead of being rendered as a row.
 */
function splitLogFooter(output: string): { lines: string[]; footer?: string } {
  const lines = outputLines(output.trim()).filter((line) => line.trim());
  const last = lines[lines.length - 1];
  return last && /^\(.*\)$/.test(last.trim())
    ? { lines: lines.slice(0, -1), footer: last.trim() }
    : { lines };
}

/** `preview_console_logs`. Each entry arrives as one `[level] text` line. */
function BrowserConsoleView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const output = item.result.output.trim();
  if (!output || output === "No console logs.") { // i18n-audit-ignore: the tool's own English answer, copied from Claude Code
    return <EmptyResult>{t("没有 Console 日志", "No Console logs")}</EmptyResult>;
  }
  const { lines, footer } = splitLogFooter(output);
  return (
    <div className="tool-renderer__browser-logs">
      <ol className="tool-renderer__log-list" aria-label={t("Console 日志", "Console logs")}>
        {lines.map((line, index) => {
          const match = /^\[([a-z]+)\]\s?([\s\S]*)$/i.exec(line);
          const level = match ? match[1] : "log";
          return (
            <li key={`${index}:${line}`} className={`tool-renderer__log tool-renderer__log--${level}`}>
              <span>{level}</span>
              <code>{match ? match[2] : line}</code>
            </li>
          );
        })}
      </ol>
      {footer && <p className="tool-renderer__log-footer">{footer}</p>}
    </div>
  );
}

const NETWORK_ENTRY = /^\[([^\]]+)]\s+([A-Z]+)\s+(\S+)(?:\s+→\s+(\d+)\s*(.*?))?\s*(\[FAILED:[\s\S]*])?$/;

/**
 * `preview_network`. Without `requestId` the answer is the request ledger, one
 * line each; with one it is a response body, which is text and stays text.
 */
function BrowserNetworkView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const output = item.result.output.trim();
  if (inputString(item, "requestId")) {
    return output
      ? <pre className="tool-renderer__plain-output">{output}</pre>
      : <EmptyResult>{t("没有响应正文", "No response body")}</EmptyResult>;
  }
  const lines = outputLines(output).filter((line) => line.trim());
  if (!lines.length || !lines.some((line) => NETWORK_ENTRY.test(line))) {
    return <EmptyResult>{lines[0] ?? t("没有网络记录", "No network records")}</EmptyResult>;
  }
  return (
    <ol className="tool-renderer__network-list" aria-label={t("网络请求", "Network requests")}>
      {lines.map((line, index) => {
        const match = NETWORK_ENTRY.exec(line);
        if (!match) return <li key={`${index}:${line}`}><code>{line}</code></li>;
        const [, requestId, method, url, status, statusText, failure] = match;
        return (
          <li key={`${index}:${requestId}`}>
            <span className="tool-renderer__network-method">{method}</span>
            <span className="tool-renderer__network-status">{status ? `${status} ${statusText ?? ""}`.trim() : ""}</span>
            <code>{url}</code>
            {failure && <em>{failure}</em>}
          </li>
        );
      })}
    </ol>
  );
}

/**
 * A preview tool whose whole answer is one JSON document — `preview_list`'s
 * server array, `preview_inspect`'s element. The host serializes those compactly,
 * so the card re-indents rather than printing one very long line.
 */
function BrowserJsonView({ item, presentation }: { item: ToolContext; presentation: ToolPresentation }) {
  const parsed = parseJsonOutput(item);
  if (!parsed.parsed) return <RawView item={item} presentation={presentation} />;
  const names = presentation.keys ?? Object.keys(item.input);
  return (
    <KeyValueCard
      rows={names.map((name) => [name, rawFieldText(item.input[name])])}
      result={safeStringify(parsed.value)}
    />
  );
}

/**
 * Mework's own two preview tools answer with a JSON object that carries the page
 * state after the action: where the page ended up, anything that blocked it, the
 * console errors the action produced, and a bounded accessibility tree. Every
 * other key belongs to the tool itself and is shown above them.
 */
const PAGE_REPORT_KEYS = ["page", "snapshot", "notices", "newConsoleErrors", "modalState"];

function BrowserPageView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const page = object(value.page);
  const snapshot = object(value.snapshot);
  const modal = object(value.modalState);
  const notices = Array.isArray(value.notices) ? value.notices : [];
  const consoleErrors = Array.isArray(value.newConsoleErrors) ? value.newConsoleErrors : [];
  const modalDescription = Array.isArray(modal?.description)
    ? modal.description.map((entry) => scalarText(entry)).join("; ")
    : modal
      ? scalarText(modal.description)
      : undefined;
  const own = Object.entries(value).flatMap<KvRow>(([key, entry]) => (
    PAGE_REPORT_KEYS.includes(key) || object(entry) || Array.isArray(entry) ? [] : [[key, entry]]
  ));
  return (
    <KeyValueCard
      rows={[
        ...own,
        ["URL", page?.url],
        [t("标题", "Title"), page?.title],
        [t("加载中", "Loading"), page?.loading],
        [t("模态状态", "Modal state"), modalDescription],
        [t("提示", "Notices"), notices.map((notice) => scalarText(notice)).join("\n")],
        [t("新增 Console 错误", "New Console errors"), consoleErrors.map((entry) => scalarText(entry)).join("\n")]
      ]}
      result={typeof snapshot?.tree === "string" ? snapshot.tree : undefined}
    />
  );
}

/** An argument as the editor would show it: strings verbatim, anything else as JSON. */
function rawFieldText(value: JsonValue | undefined): string {
  if (value === undefined) return "";
  return typeof value === "string" ? value : safeStringify(value);
}

/** One line of a card: what the value is called, and the value. */
type KvRow = [label: string, value: unknown];

/**
 * The one shape a card takes when it has no picture to draw.
 *
 * Everything worth knowing is named on the left with its value boxed beside it,
 * and whatever the call answered with is a last box of its own spanning the
 * card — the result is the one thing every card has, so it needs no label to
 * say which it is. Families that answer with a diff, a terminal, a file list or
 * pixels keep drawing those; everything else is rows and a result, so a reader
 * who has learned one card has learned all of them.
 */
function KeyValueCard({ rows, result }: { rows: KvRow[]; result?: string }) {
  const visible = rows.flatMap<[string, string]>(([label, value]) => {
    if (value === undefined || value === "") return [];
    return [[label, typeof value === "string" ? value : scalarText(value)]];
  });
  if (!visible.length && !result) return <EmptyResult />;
  return (
    <dl className="tool-kv">
      {visible.map(([label, text], index) => (
        <div className="tool-kv__row" key={`${index}:${label}`}>
          <dt className="tool-kv__key" title={label}>{label}</dt>
          <dd className="tool-kv__value"><pre>{text}</pre></dd>
        </div>
      ))}
      {result ? (
        <div className="tool-kv__row tool-kv__row--result">
          <dd className="tool-kv__value"><pre>{result}</pre></dd>
        </div>
      ) : null}
    </dl>
  );
}

function MemoryView({
  item,
  presentation,
  failed = false
}: {
  item: ToolContext;
  presentation: ToolPresentation;
  failed?: boolean;
}) {
  const { t } = useI18n();
  const tier = item.toolName.includes("_global_")
    ? t("全局记忆", "Global memory")
    : t("项目记忆", "Project memory");
  const rows: KvRow[] = [
    [t("范围", "Tier"), tier],
    [t("文档", "Document"), compact(inputString(item, "name"), 160)],
    [t("索引描述", "Index description"), compact(inputString(item, "description"), 300)]
  ];
  if (failed) {
    return (
      <div className="tool-renderer__error" role="alert">
        <CircleAlert size={16} aria-hidden="true" />
        <div>
          <strong>{presentation.title}</strong>
          <KeyValueCard rows={rows} result={item.result.output} />
        </div>
      </div>
    );
  }
  return <KeyValueCard rows={rows} result={item.result.output} />;
}

/**
 * A call shown as the arguments worth reading and then what it answered.
 *
 * `presentation.keys` names those arguments and their order. A tool that names
 * none has had none curated, so all of them show rather than none — an
 * uncurated argument is still the reader's only account of the call.
 */
function RawView({ item, presentation }: { item: ToolContext; presentation?: ToolPresentation }) {
  const names = presentation?.keys ?? Object.keys(item.input);
  return (
    <KeyValueCard
      rows={names.map((name) => [name, rawFieldText(item.input[name])])}
      result={item.result.output}
    />
  );
}

function runStatusLabel(status: ReturnType<typeof agentTimelineRunStatus>, t: Translate): string {
  switch (status) {
    case "running": return t("运行中", "Running");
    case "completed": return t("已完成", "Completed");
    case "failed": return t("已失败", "Failed");
    case "stopped": return t("已停止", "Stopped");
    case "roundLimit": return t("已达轮次上限", "Round limit reached");
    default: return t("已中断", "Interrupted");
  }
}

/**
 * The updates a child pushed onto this call while it ran.
 *
 * `live.updates` is the streaming feed and `subagent.updates` the persisted
 * record; a call in the handoff window carries both, with the same content in
 * each, so identical text collapses to its latest occurrence.
 */
function runUpdates(item: ToolContext): SubagentUpdate[] {
  const latest = new Map<string, SubagentUpdate>();
  for (const update of [...(item.live?.updates ?? []), ...(item.subagent?.updates ?? [])]) {
    const key = update.content.replace(/\s+/g, " ").trim();
    if (!key) continue;
    const previous = latest.get(key);
    if (!previous || Date.parse(previous.createdAt) <= Date.parse(update.createdAt)) {
      latest.set(key, update);
    }
  }
  return [...latest.values()].sort(
    (left, right) => (Date.parse(left.createdAt) || 0) - (Date.parse(right.createdAt) || 0)
  );
}

/** A call that owns a child run: what it was asked, what it said, what it returned. */
function AgentRunView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const instruction = initialMessage(item) || item.subagent?.task || "";
  const updates = runUpdates(item);
  return (
    <KeyValueCard
      rows={[
        [t("子代理", "Subagent"), item.subagent?.label || item.subagent?.name || ""],
        [t("状态", "Status"), runStatusLabel(agentTimelineRunStatus(item), t)],
        [t("派发指令", "Instruction"), instruction],
        [t("子代理更新", "Subagent updates"), updates.map((update) => update.content).join("\n")]
      ]}
      result={item.result.output}
    />
  );
}

/** `task_wait` output split back into the `[name · status]` envelopes it drained. */
function AgentWaitView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const { envelopes, statusLine, notice } = parseWaitOutput(item.result.output);
  if (!envelopes.length) {
    return notice
      ? <KeyValueCard rows={[]} result={item.result.output} />
      : <EmptyResult>{t("等待已结束，没有可显示的更新", "The wait finished with no updates to show")}</EmptyResult>;
  }
  return (
    <KeyValueCard
      rows={[
        ...(notice ? [[t("说明", "Note"), notice] as KvRow] : []),
        ...envelopes.map<KvRow>((envelope) => [`${envelope.agent} · ${envelope.status}`, envelope.body])
      ]}
      result={statusLine}
    />
  );
}

/**
 * A one-shot agent message (`subagent_update`, `structured_output`,
 * `task_list`, …). Its payload is already the row's target, so the detail only
 * has to show the full untruncated text and the receipt the host wrote back.
 */
function AgentNoteView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const message = inputString(item, "message")
    ?? inputString(item, "content")
    ?? inputString(item, "status");
  return <KeyValueCard rows={[[t("内容", "Message"), message]]} result={item.result.output} />;
}

/** A `todo` write: the fields the call set, plus the host's receipt. */
function StateToolView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  return (
    <KeyValueCard
      rows={[
        [t("标题", "Subject"), inputString(item, "subject")],
        [t("状态", "Status"), todoStatusLabel(item, t)],
        [t("任务 ID", "Task id"), inputString(item, "taskId")],
        [t("说明", "Description"), inputString(item, "description")]
      ]}
      result={item.result.output}
    />
  );
}

/**
 * One site chip: its icon, or the first letter of its domain while the icon is
 * still being fetched or when the site has none.
 *
 * The host does the fetching (`webSourceIcon`), because the application's CSP
 * allows `img-src 'self' data:` and nothing outbound. The placeholder is not a
 * loading state worth animating — most chips resolve in one paint from cache,
 * and a spinner on each of six chips would be the loudest thing on the card.
 */
function WebSourceIcon({ chip }: { chip: WebSourceChip }) {
  const [source, setSource] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    siteIcon(chip.url).then((icon) => {
      if (active) setSource(icon);
    });
    return () => { active = false; };
  }, [chip.url]);
  if (!source) {
    return <span className="tool-renderer__source-letter" aria-hidden="true">
      {chip.host.slice(0, 1).toUpperCase()}
    </span>;
  }
  return <img
    className="tool-renderer__source-icon"
    src={source}
    alt=""
    aria-hidden="true"
    loading="lazy"
    decoding="async"
  />;
}

/**
 * A settled `web_search` card: the sites it consulted, then what it found.
 *
 * The chips come first because they are the part a reader can check. The prose
 * under them is a model's summary for a native backend and the providers' own
 * snippets for a catalog backend; either way the domains say where it came from,
 * which is the question a search result actually has to answer.
 */
function WebSearchView({ item, presentation }: { item: ToolContext; presentation: ToolPresentation }) {
  const { t } = useI18n();
  const chips = webSourceChips(item);
  const parsed = parseJsonOutput(item);
  const envelope = object(parsed.value);
  const findings = typeof envelope?.findings === "string" ? envelope.findings.trim() : "";
  const results = Array.isArray(envelope?.results) ? envelope.results : [];
  if (!chips.length && !findings && !results.length) {
    return <RawView item={item} presentation={presentation} />;
  }
  return (
    <div className="tool-renderer__web-search">
      {chips.length > 0 && (
        <nav className="tool-renderer__sources" aria-label={t("搜索到的网站", "Sites searched")}>
          {chips.map((chip) => (
            <a
              key={chip.host}
              className="tool-renderer__source"
              href={chip.url}
              // The title carries the page title and full URL: the chip itself
              // shows only the domain, and a reader who wants the rest should
              // not have to open the page to see it.
              title={chip.title ? `${chip.title}\n${chip.url}` : chip.url}
              target="_blank"
              rel="noreferrer noopener"
            >
              <WebSourceIcon chip={chip} />
              <span className="tool-renderer__source-host">{chip.host}</span>
            </a>
          ))}
        </nav>
      )}
      {findings && <div className="tool-renderer__findings">{findings}</div>}
      {results.length > 0 && (
        <ol className="tool-renderer__result-list">
          {results.map((entry, index) => {
            const fields = object(entry);
            const title = typeof fields?.title === "string" ? fields.title.trim() : "";
            const url = typeof fields?.url === "string" ? fields.url.trim() : "";
            const content = typeof fields?.content === "string" ? fields.content.trim() : "";
            return (
              <li key={`${index}:${url}`}>
                <strong>{title || url || t("无标题", "Untitled")}</strong>
                {content && <p>{content}</p>}
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

function RunningView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  return (
    <div className="tool-renderer__running" role="status">
      <Timer size={14} aria-hidden="true" />
      <span>{runningLabel(item, t)}</span>
    </div>
  );
}

function runningLabel(item: ToolContext, t: Translate): string {
  if (item.streamStatus === "announced") return t("正在准备参数", "Preparing arguments");
  if (item.streamStatus === "ready") return t("等待执行", "Waiting to run");
  return t("正在执行", "Running");
}

function ErrorView({ item, presentation }: { item: ToolContext; presentation: ToolPresentation }) {
  const { t } = useI18n();
  return (
    <div className="tool-renderer__error" role="alert">
      <CircleAlert size={16} aria-hidden="true" />
      <div>
        <strong>{presentation.title}</strong>
        <pre>{item.result.output || t("工具执行失败，未返回错误详情", "The tool failed without returning error details")}</pre>
      </div>
    </div>
  );
}

function detailBody(
  item: ToolContext,
  presentation: ToolPresentation
): ReactNode {
  switch (presentation.family) {
    case "file-list": return <FileListView item={item} />;
    case "grep": return <GrepView item={item} />;
    case "read": return <ReadView item={item} />;
    case "diff": return <DiffView item={item} />;
    case "terminal": return <TerminalView item={item} />;
    case "browser-snapshot": return <BrowserSnapshotView item={item} />;
    case "browser-evaluate": return <BrowserEvaluateView item={item} />;
    case "browser-screenshot": return <BrowserScreenshotView item={item} />;
    case "browser-console": return <BrowserConsoleView item={item} />;
    case "browser-network": return <BrowserNetworkView item={item} />;
    case "browser-json": return <BrowserJsonView item={item} presentation={presentation} />;
    case "browser-page": return <BrowserPageView item={item} />;
    case "memory": return <MemoryView item={item} presentation={presentation} />;
    case "agent-run": return <AgentRunView item={item} />;
    case "agent-wait": return <AgentWaitView item={item} />;
    case "agent-note": return <AgentNoteView item={item} />;
    case "persistent": return <StateToolView item={item} />;
    case "web-search": return <WebSearchView item={item} presentation={presentation} />;
    default: return <RawView item={item} presentation={presentation} />;
  }
}

export function ToolDetailRenderer({
  item,
  descriptor
}: {
  item: ToolContext;
  descriptor?: ToolDescriptor;
}) {
  const { t } = useI18n();
  const presentation = getToolPresentation(item, descriptor, t);
  const phase = executionPhase(item);
  // A result carrying images is the image: its receipt text and screenshot
  // metadata only repeat the thumbnail and collapsed summary. Render only the
  // thumbnail and let its viewer carry detail. Failures retain their messages.
  const imageOnly = phase === "done" && (item.result.images?.length ?? 0) > 0;
  const label = `${presentation.title}${presentation.target ? `：${presentation.target}` : ""}`;
  return (
    <section
      className={`tool-renderer tool-renderer--${presentation.family}${phase === "failed" ? " tool-renderer--failed" : ""}`}
      aria-label={label}
      data-tool-name={item.toolName}
      data-tool-family={presentation.family}
    >
      <div className="tool-renderer__body">
        {phase !== "running" && (
          <ImageStrip images={item.result.images} className="tool-renderer__images" />
        )}
        {phase === "running"
          ? <RunningView item={item} />
          : phase === "failed"
            ? presentation.family === "memory"
              ? <MemoryView item={item} presentation={presentation} failed />
              : <ErrorView item={item} presentation={presentation} />
            : imageOnly
              ? null
              : detailBody(item, presentation)}
      </div>
      {presentation.family !== "memory" && <RawDataDisclosure item={item} />}
    </section>
  );
}
