import {
  Bot,
  Box,
  CircleAlert,
  CircleCheck,
  CircleHelp,
  Code2,
  Columns3,
  Compass,
  File,
  FilePenLine,
  FileSearch,
  FileText,
  Folder,
  FolderTree,
  GitFork,
  Globe,
  Globe2,
  Hand,
  Image,
  Keyboard,
  ListChecks,
  ListFilter,
  Logs,
  MessageCircleMore,
  MonitorUp,
  MousePointerClick,
  MoveVertical,
  Network,
  Rows3,
  ScanSearch,
  Search,
  SquarePlus,
  SquareTerminal,
  PowerOff,
  SquareX,
  TextCursorInput,
  Timer,
  Trash2,
  Upload,
  Workflow,
  Wrench
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useState } from "react";
import type { ReactNode } from "react";
import { t as globalT, useI18n } from "../i18n";
import { parseWaitOutput, questionsFromInput } from "../lib/orchestration";
import { agentTimelineRunStatus, initialMessage } from "../lib/subagents";
import type { SubagentUpdate, ToolContext, ToolDescriptor } from "../types";
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
  | "browser-navigation"
  | "browser-snapshot"
  | "browser-action"
  | "browser-evaluate"
  | "browser-screenshot"
  | "browser-console"
  | "browser-network"
  | "browser-dialog"
  | "browser-status"
  | "memory"
  | "persistent"
  | "question"
  | "agent-run"
  | "agent-wait"
  | "agent-note"
  | "raw";

type SummaryKind = "files" | "search" | "fileChanges" | "commands" | "browser" | "memory" | "agents" | "state" | "other";
type Translate = ReturnType<typeof useI18n>["t"];
type PresentationResolver = (item: ToolContext, t: Translate) => string | undefined;
type TitleResolver = (t: Translate) => string;

export interface ToolViewConfig {
  surface: ToolSurface;
  family: ToolViewFamily;
  icon: LucideIcon;
  doneTitle: TitleResolver;
  runningTitle?: TitleResolver;
  failedTitle?: TitleResolver;
  resolveTitle?: (item: ToolContext, phase: "done" | "running" | "failed", t: Translate) => string;
  /**
   * Action-multiplexed tools resolve their icon and detail family per call.
   *
   * `playwright` is one wire tool covering 23 operations; without these a
   * snapshot, a click and a network-log read would all render as one generic
   * browser card. Ordinary tools leave both unset and use the static fields.
   */
  resolveIcon?: (item: ToolContext) => LucideIcon;
  resolveFamily?: (item: ToolContext) => ToolViewFamily;
  target?: PresentationResolver;
  stat?: PresentationResolver;
  summaryKind: SummaryKind;
}

export interface ToolPresentation {
  surface: ToolSurface;
  family: ToolViewFamily;
  icon: LucideIcon;
  title: string;
  target?: string;
  stat?: string;
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

function navigateTitle(item: ToolContext, phase: "done" | "running" | "failed", t: Translate): string {
  const target = inputString(item, "url")?.toLowerCase();
  if (phase === "failed") return t("网页导航失败", "Web navigation failed");
  if (phase === "running") return t("正在导航网页", "Navigating webpage");
  if (target === "back") return t("后退了网页", "Navigated back");
  if (target === "forward") return t("前进了网页", "Navigated forward");
  if (target === "reload") return t("刷新了网页", "Reloaded webpage");
  return t("导航了网页", "Navigated webpage");
}

function snapshotStat(item: ToolContext, t: Translate): string | undefined {
  const count = arrayLength(parsedRecord(item)?.elements);
  return count === undefined ? browserDuration(item) : t("{count} 个元素", "{count} elements", { count });
}

function fillStat(item: ToolContext, t: Translate): string | undefined {
  const result = parsedRecord(item);
  const completed = numberValue(result?.completed);
  const total = numberValue(result?.total) ?? arrayLength(item.input.fields);
  if (completed !== undefined && total !== undefined) {
    return t("{completed}/{total} 项", "{completed}/{total} items", { completed, total });
  }
  return total === undefined ? browserDuration(item) : t("{total} 项", "{total} items", { total });
}

function typeStat(item: ToolContext, t: Translate): string | undefined {
  const text = inputString(item, "text");
  return text === undefined ? browserDuration(item) : t("{count} 字", "{count} characters", { count: text.length });
}

function selectionStat(item: ToolContext, t: Translate): string | undefined {
  const values = item.input.values;
  const count = Array.isArray(values) ? values.length : values === undefined ? undefined : 1;
  return count === undefined ? browserDuration(item) : t("{count} 项", "{count} items", { count });
}

function keyStat(item: ToolContext): string | undefined {
  const repeat = inputNumber(item, "repeat") ?? 1;
  return repeat > 1 ? `×${Math.floor(repeat)}` : browserDuration(item);
}

function scrollStat(item: ToolContext): string | undefined {
  const x = inputNumber(item, "x") ?? 0;
  const y = inputNumber(item, "y") ?? 0;
  return x || y ? `${x}, ${y}` : browserDuration(item);
}

function screenshotStat(item: ToolContext): string | undefined {
  const result = parsedRecord(item);
  const width = numberValue(result?.width);
  const height = numberValue(result?.height);
  return width !== undefined && height !== undefined ? `${width}×${height}` : browserDuration(item);
}

function logStat(item: ToolContext, t: Translate): string | undefined {
  const result = parsedRecord(item);
  const count = numberValue(result?.returned) ?? arrayLength(result?.entries);
  return count === undefined ? browserDuration(item) : t("{count} 条", "{count} entries", { count });
}

function tabStat(item: ToolContext, t: Translate): string | undefined {
  const count = arrayLength(parsedRecord(item)?.tabs);
  return count === undefined ? browserDuration(item) : t("{count} 个标签页", "{count} tabs", { count });
}

function tabTarget(item: ToolContext): string | undefined {
  const result = parsedRecord(item);
  const tab = result?.opened ?? result?.closed ?? result?.current;
  return typeof tab === "string" ? compact(tab) : compact(inputString(item, "tab") ?? inputString(item, "url"));
}

function uploadStat(item: ToolContext, t: Translate): string | undefined {
  const paths = item.input.paths;
  const count = Array.isArray(paths) ? paths.length : typeof paths === "string" ? 1 : undefined;
  return count === undefined ? browserDuration(item) : t("{count} 个文件", "{count} files", { count });
}

function uploadImageStat(item: ToolContext): string | undefined {
  const raw = item.input.image_id;
  const reference = typeof raw === "number" ? String(raw) : typeof raw === "string" ? raw.trim() : "";
  const match = reference.match(/^(?:\[Image #|#)?([0-9]{1,9})\]?$/);
  return match ? `#${match[1]}` : browserDuration(item);
}

function resizeStat(item: ToolContext): string | undefined {
  const width = inputNumber(item, "width");
  const height = inputNumber(item, "height");
  return width !== undefined && height !== undefined ? `${width}×${height}` : browserDuration(item);
}
/**
 * Presentation of one `playwright` action.
 *
 * The browser surface is a single wire tool multiplexed by `action`, but a
 * click and a network-log read are not the same event to a reader, so each
 * action keeps the icon, titles, target and stat it had when it was its own
 * tool. Every action is named explicitly — an unknown one falls back to the
 * generic entry rather than borrowing another action's card.
 */
interface PlaywrightActionView {
  family: ToolViewFamily;
  icon: LucideIcon;
  doneTitle: TitleResolver;
  runningTitle: TitleResolver;
  failedTitle: TitleResolver;
  resolveTitle?: (item: ToolContext, phase: "done" | "running" | "failed", t: Translate) => string;
  target?: PresentationResolver;
  stat?: PresentationResolver;
}

const PLAYWRIGHT_ACTION_VIEWS: Record<string, PlaywrightActionView> = {
  navigate: { family: "browser-navigation", icon: Compass, doneTitle: (t) => t("导航了网页", "Navigated webpage"), runningTitle: (t) => t("正在导航网页", "Navigating webpage"), failedTitle: (t) => t("网页导航失败", "Web navigation failed"), resolveTitle: navigateTitle, target: (item) => compact(inputString(item, "url")), stat: browserDuration },
  snapshot: { family: "browser-snapshot", icon: ScanSearch, doneTitle: (t) => t("读取了页面快照", "Read page snapshot"), runningTitle: (t) => t("正在读取页面快照", "Reading page snapshot"), failedTitle: (t) => t("读取页面快照失败", "Failed to read page snapshot"), stat: snapshotStat },
  click: { family: "browser-action", icon: MousePointerClick, doneTitle: (t) => t("点击了页面元素", "Clicked page element"), runningTitle: (t) => t("正在点击页面元素", "Clicking page element"), failedTitle: (t) => t("点击页面元素失败", "Failed to click page element"), target: targetSelector, stat: browserDuration },
  type: { family: "browser-action", icon: TextCursorInput, doneTitle: (t) => t("填写了页面输入", "Filled page input"), runningTitle: (t) => t("正在填写页面输入", "Filling page input"), failedTitle: (t) => t("填写页面输入失败", "Failed to fill page input"), target: targetSelector, stat: typeStat },
  fill_form: { family: "browser-action", icon: Rows3, doneTitle: (t) => t("填写了表单", "Filled form"), runningTitle: (t) => t("正在填写表单", "Filling form"), failedTitle: (t) => t("填写表单失败", "Failed to fill form"), stat: fillStat },
  select: { family: "browser-action", icon: ListFilter, doneTitle: (t) => t("选择了页面选项", "Selected page option"), runningTitle: (t) => t("正在选择页面选项", "Selecting page option"), failedTitle: (t) => t("选择页面选项失败", "Failed to select page option"), target: targetSelector, stat: selectionStat },
  hover: { family: "browser-action", icon: Hand, doneTitle: (t) => t("悬停了页面元素", "Hovered over page element"), runningTitle: (t) => t("正在悬停页面元素", "Hovering over page element"), failedTitle: (t) => t("悬停页面元素失败", "Failed to hover over page element"), target: targetSelector, stat: browserDuration },
  key: { family: "browser-action", icon: Keyboard, doneTitle: (t) => t("发送了页面按键", "Sent page key"), runningTitle: (t) => t("正在发送页面按键", "Sending page key"), failedTitle: (t) => t("发送页面按键失败", "Failed to send page key"), target: (item) => compact(inputString(item, "key")), stat: keyStat },
  scroll: { family: "browser-action", icon: MoveVertical, doneTitle: (t) => t("滚动了页面", "Scrolled page"), runningTitle: (t) => t("正在滚动页面", "Scrolling page"), failedTitle: (t) => t("滚动页面失败", "Failed to scroll page"), target: targetSelector, stat: scrollStat },
  evaluate: { family: "browser-evaluate", icon: Code2, doneTitle: (t) => t("执行了页面脚本", "Ran page script"), runningTitle: (t) => t("正在执行页面脚本", "Running page script"), failedTitle: (t) => t("页面脚本执行失败", "Page script failed"), target: (item) => compact(inputString(item, "script")), stat: browserDuration },
  wait: { family: "browser-action", icon: Timer, doneTitle: (t) => t("等待了页面状态", "Waited for page state"), runningTitle: (t) => t("正在等待页面状态", "Waiting for page state"), failedTitle: (t) => t("等待页面状态失败", "Failed to wait for page state"), target: (item) => targetSelector(item) ?? compact(inputString(item, "text") ?? inputString(item, "text_gone")) ?? (inputFlag(item, "load") ? "load" : undefined), stat: browserDuration },
  screenshot: { family: "browser-screenshot", icon: Image, doneTitle: (t) => t("截取了网页", "Captured webpage"), runningTitle: (t) => t("正在截取网页", "Capturing webpage"), failedTitle: (t) => t("网页截图失败", "Webpage capture failed"), target: (item) => compact(inputString(item, "path")), stat: screenshotStat },
  console: { family: "browser-console", icon: Logs, doneTitle: (t) => t("读取了 Console 日志", "Read Console logs"), runningTitle: (t) => t("正在读取 Console 日志", "Reading Console logs"), failedTitle: (t) => t("读取 Console 日志失败", "Failed to read Console logs"), stat: logStat },
  network: { family: "browser-network", icon: Network, doneTitle: (t) => t("读取了网络日志", "Read network logs"), runningTitle: (t) => t("正在读取网络日志", "Reading network logs"), failedTitle: (t) => t("读取网络日志失败", "Failed to read network logs"), target: (item) => compact(inputString(item, "filter")), stat: logStat },
  dialog: { family: "browser-dialog", icon: CircleAlert, doneTitle: (t) => t("处理了页面对话框", "Handled page dialog"), runningTitle: (t) => t("正在处理页面对话框", "Handling page dialog"), failedTitle: (t) => t("处理页面对话框失败", "Failed to handle page dialog"), stat: browserDuration },
  file_upload: { family: "browser-action", icon: Upload, doneTitle: (t) => t("上传了网页文件", "Uploaded webpage files"), runningTitle: (t) => t("正在上传网页文件", "Uploading webpage files"), failedTitle: (t) => t("上传网页文件失败", "Failed to upload webpage files"), target: targetSelector, stat: uploadStat },
  upload_image: { family: "browser-action", icon: Upload, doneTitle: (t) => t("上传了对话图片", "Uploaded conversation image"), runningTitle: (t) => t("正在上传对话图片", "Uploading conversation image"), failedTitle: (t) => t("上传对话图片失败", "Failed to upload conversation image"), target: targetSelector, stat: uploadImageStat },
  resize: { family: "browser-status", icon: MonitorUp, doneTitle: (t) => t("调整了浏览器视口", "Resized browser viewport"), runningTitle: (t) => t("正在调整浏览器视口", "Resizing browser viewport"), failedTitle: (t) => t("调整浏览器视口失败", "Failed to resize browser viewport"), stat: resizeStat },
  tab_new: { family: "browser-status", icon: SquarePlus, doneTitle: (t) => t("新建了浏览器标签页", "Opened browser tab"), runningTitle: (t) => t("正在新建浏览器标签页", "Opening browser tab"), failedTitle: (t) => t("新建浏览器标签页失败", "Failed to open browser tab"), target: tabTarget, stat: tabStat },
  tab_list: { family: "browser-status", icon: Columns3, doneTitle: (t) => t("读取了标签页列表", "Listed browser tabs"), runningTitle: (t) => t("正在读取标签页列表", "Listing browser tabs"), failedTitle: (t) => t("读取标签页列表失败", "Failed to list browser tabs"), stat: tabStat },
  tab_select: { family: "browser-status", icon: Columns3, doneTitle: (t) => t("切换了浏览器标签页", "Switched browser tab"), runningTitle: (t) => t("正在切换浏览器标签页", "Switching browser tab"), failedTitle: (t) => t("切换浏览器标签页失败", "Failed to switch browser tab"), target: tabTarget, stat: tabStat },
  tab_close: { family: "browser-status", icon: SquareX, doneTitle: (t) => t("关闭了浏览器标签页", "Closed browser tab"), runningTitle: (t) => t("正在关闭浏览器标签页", "Closing browser tab"), failedTitle: (t) => t("关闭浏览器标签页失败", "Failed to close browser tab"), target: tabTarget, stat: tabStat },
  close: { family: "browser-status", icon: PowerOff, doneTitle: (t) => t("关闭了浏览器", "Closed browser"), runningTitle: (t) => t("正在关闭浏览器", "Closing browser"), failedTitle: (t) => t("关闭浏览器失败", "Failed to close browser"), stat: tabStat }
};

/**
 * A streaming call can reach the timeline before its arguments finish parsing,
 * so an unresolved action is an ordinary state rather than an error: it renders
 * the neutral browser card until the action arrives.
 */
const PLAYWRIGHT_UNKNOWN_ACTION_VIEW: PlaywrightActionView = {
  family: "browser-action",
  icon: Globe,
  doneTitle: (t) => t("执行了浏览器操作", "Performed browser action"),
  runningTitle: (t) => t("正在执行浏览器操作", "Performing browser action"),
  failedTitle: (t) => t("浏览器操作失败", "Browser action failed"),
  stat: browserDuration
};

export function playwrightActionName(item: ToolContext): string | undefined {
  const action = inputString(item, "action");
  return action && Object.hasOwn(PLAYWRIGHT_ACTION_VIEWS, action) ? action : undefined;
}

function playwrightView(item: ToolContext): PlaywrightActionView {
  const action = playwrightActionName(item);
  return action ? PLAYWRIGHT_ACTION_VIEWS[action] : PLAYWRIGHT_UNKNOWN_ACTION_VIEW;
}

function playwrightTitle(
  item: ToolContext,
  phase: "done" | "running" | "failed",
  t: Translate
): string {
  const view = playwrightView(item);
  if (view.resolveTitle) return view.resolveTitle(item, phase, t);
  if (phase === "running") return view.runningTitle(t);
  if (phase === "failed") return view.failedTitle(t);
  return view.doneTitle(t);
}
/**
 * Presentation of one `todo` action.
 *
 * Same merge shape as `playwright`, and the same reason for this table: the
 * task-state surface is one wire tool multiplexed by `action`, but creating a
 * task and reading the list are not the same event to a reader. Each action
 * keeps the icon, titles, target, stat AND detail family it had when it was its
 * own catalog entry — writes get the structured state view, reads keep the raw
 * output.
 */
type StateToolActionView = PlaywrightActionView;

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

type MemoryOperation = "list" | "read" | "search" | "upsert" | "delete";

interface MemoryMetadata {
  operation: MemoryOperation;
  modelId?: string;
  scope?: string;
  name?: string;
  version?: number;
  bytes?: number;
  status?: string;
  documentCount?: number;
  matchCount?: number;
  enabled?: boolean;
  rewriteRequired?: boolean;
  compactionRecommended?: boolean;
  loadedLines?: number;
  loadedBytes?: number;
  limitLines?: number;
  limitBytes?: number;
}

function resultString(record: Record<string, unknown> | null, ...names: string[]): string | undefined {
  for (const name of names) {
    const value = record?.[name];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return undefined;
}

function resultNumber(record: Record<string, unknown> | null, ...names: string[]): number | undefined {
  for (const name of names) {
    const value = numberValue(record?.[name]);
    if (value !== undefined) return value;
  }
  return undefined;
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
 * Every built-in tool is intentionally named here. Similar tools share a
 * renderer family, but no tool silently falls into a category-wide default.
 */
export const TOOL_VIEW_REGISTRY = {
  ls: { surface: "group", family: "file-list", icon: FolderTree, doneTitle: (t) => t("查看了目录", "Viewed directory"), runningTitle: (t) => t("正在查看目录", "Viewing directory"), failedTitle: (t) => t("查看目录失败", "Failed to view directory"), target: (item) => compact(inputString(item, "path") ?? "."), stat: fileListStat, summaryKind: "files" },
  grep: { surface: "group", family: "grep", icon: Search, doneTitle: (t) => t("搜索了文件内容", "Searched file contents"), runningTitle: (t) => t("正在搜索文件内容", "Searching file contents"), failedTitle: (t) => t("搜索文件内容失败", "File-content search failed"), target: (item) => compact(inputString(item, "pattern")), stat: grepStat, summaryKind: "search" },
  powershell: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 PowerShell 命令", "Ran PowerShell command"), runningTitle: (t) => t("正在运行 PowerShell 命令", "Running PowerShell command"), failedTitle: (t) => t("PowerShell 命令失败", "PowerShell command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  bash: { surface: "group", family: "terminal", icon: SquareTerminal, doneTitle: (t) => t("运行了 Bash 命令", "Ran Bash command"), runningTitle: (t) => t("正在运行 Bash 命令", "Running Bash command"), failedTitle: (t) => t("Bash 命令失败", "Bash command failed"), target: (item) => compact(inputString(item, "command")), stat: browserDuration, summaryKind: "commands" },
  write: { surface: "group", family: "diff", icon: FilePenLine, doneTitle: (t) => t("写入了文件", "Wrote file"), resolveTitle: writeTitle, target: (item) => compact(inputString(item, "path")), stat: diffStat, summaryKind: "fileChanges" },
  edit: { surface: "group", family: "diff", icon: FilePenLine, doneTitle: (t) => t("编辑了文件", "Edited file"), runningTitle: (t) => t("正在编辑文件", "Editing file"), failedTitle: (t) => t("编辑文件失败", "Failed to edit file"), target: (item) => compact(inputString(item, "path")), stat: diffStat, summaryKind: "fileChanges" },
  find: { surface: "group", family: "file-list", icon: FileSearch, doneTitle: (t) => t("查找了文件", "Found files"), runningTitle: (t) => t("正在查找文件", "Finding files"), failedTitle: (t) => t("查找文件失败", "Failed to find files"), target: (item) => compact(inputString(item, "query")), stat: fileListStat, summaryKind: "files" },
  read: { surface: "group", family: "read", icon: FileText, doneTitle: (t) => t("读取了文件", "Read file"), runningTitle: (t) => t("正在读取文件", "Reading file"), failedTitle: (t) => t("读取文件失败", "Failed to read file"), target: (item) => compact(inputString(item, "path")), stat: readStat, summaryKind: "files" },

  workflow: { surface: "workflow", family: "raw", icon: Workflow, doneTitle: (t) => t("完成了工作流", "Completed workflow"), runningTitle: (t) => t("正在运行工作流", "Running workflow"), failedTitle: (t) => t("工作流运行失败", "Workflow failed"), summaryKind: "other" },
  workflow_step: { surface: "group", family: "agent-run", icon: Bot, doneTitle: (t) => t("完成了工作流步骤", "Completed workflow step"), runningTitle: (t) => t("正在运行工作流步骤", "Running workflow step"), failedTitle: (t) => t("工作流步骤失败", "Workflow step failed"), target: (item) => compact(inputString(item, "label") ?? inputString(item, "name")), summaryKind: "agents" },

  // Web tools return results directly, so they use the editable, rerunnable group
  // surface shared with bash and read.
  web_search: { surface: "group", family: "raw", icon: Search, doneTitle: (t) => t("完成了联网搜索", "Completed web search"), runningTitle: (t) => t("正在联网搜索", "Searching the web"), failedTitle: (t) => t("联网搜索失败", "Web search failed"), target: (item) => compact(inputString(item, "query")), summaryKind: "search" },
  web_fetch: { surface: "group", family: "raw", icon: Globe, doneTitle: (t) => t("抓取了网页", "Fetched pages"), runningTitle: (t) => t("正在抓取网页", "Fetching pages"), failedTitle: (t) => t("抓取网页失败", "Failed to fetch pages"), target: (item) => compact(firstUrl(item)), summaryKind: "search" },

  // One wire tool, 23 actions. Each action keeps the icon, titles, target and
  // stat it had as its own tool, and `resolveIcon`/`resolveFamily` route the
  // call to the same detail view as before.
  playwright: {
    surface: "group",
    family: "browser-action",
    icon: Globe,
    doneTitle: (t) => t("执行了浏览器操作", "Performed browser action"),
    resolveTitle: playwrightTitle,
    resolveIcon: (item) => playwrightView(item).icon,
    resolveFamily: (item) => playwrightView(item).family,
    target: (item, t) => playwrightView(item).target?.(item, t),
    stat: (item, t) => playwrightView(item).stat?.(item, t),
    summaryKind: "browser"
  },

  // Agent-protocol tools are ordinary rows inside the tool block: one uniform
  // disclosure per contiguous tool run, chevron → tailored detail. `agent-run`
  // carries a child transcript (instruction, updates, returned text) and is the
  // only family that offers the inline "open subagent" affordance; `agent-wait`
  // splits the drained `[name · status]` envelopes; `agent-note` is a one-shot
  // message whose payload already sits in the row's target.
  agent_spawn: { surface: "group", family: "agent-run", icon: Bot, doneTitle: (t) => t("派生了子代理", "Spawned subagent"), runningTitle: (t) => t("正在派生子代理", "Spawning subagent"), failedTitle: (t) => t("派生子代理失败", "Failed to spawn subagent"), target: (item) => compact(inputString(item, "label") ?? inputString(item, "name")), summaryKind: "agents" },
  agent_send: { surface: "group", family: "agent-run", icon: MessageCircleMore, doneTitle: (t) => t("向子代理发送了消息", "Sent message to subagent"), runningTitle: (t) => t("正在向子代理发送消息", "Sending message to subagent"), failedTitle: (t) => t("发送子代理消息失败", "Failed to send subagent message"), target: (item) => compact(inputString(item, "agent")), summaryKind: "agents" },
  send_message: { surface: "group", family: "agent-run", icon: MessageCircleMore, doneTitle: (t) => t("消息已加入子代理队列", "Queued message for subagent"), runningTitle: (t) => t("正在排队子代理消息", "Queueing message for subagent"), failedTitle: (t) => t("排队子代理消息失败", "Failed to queue subagent message"), target: (item) => compact(inputString(item, "target")), summaryKind: "agents" },
  followup_task: { surface: "group", family: "agent-run", icon: MessageCircleMore, doneTitle: (t) => t("向子代理追加了任务", "Sent follow-up to subagent"), runningTitle: (t) => t("正在追加子代理任务", "Sending subagent follow-up"), failedTitle: (t) => t("追加子代理任务失败", "Failed to follow up with subagent"), target: (item) => compact(inputString(item, "target")), summaryKind: "agents" },
  task_wait: { surface: "group", family: "agent-wait", icon: Timer, doneTitle: (t) => t("等待了任务", "Waited for tasks"), runningTitle: (t) => t("正在等待任务", "Waiting for tasks"), failedTitle: (t) => t("等待任务失败", "Failed to wait for tasks"), stat: waitStat, summaryKind: "agents" },
  task_list: { surface: "group", family: "agent-note", icon: ListChecks, doneTitle: (t) => t("查看了任务列表", "Viewed task list"), runningTitle: (t) => t("正在查看任务列表", "Viewing task list"), failedTitle: (t) => t("查看任务列表失败", "Failed to view task list"), summaryKind: "agents" },

  // Skills return instruction text, so they use raw detail; the skill name is
  // this call's only input and becomes its target.
  skill: { surface: "group", family: "raw", icon: Box, doneTitle: (t) => t("加载了技能", "Loaded skill"), runningTitle: (t) => t("正在加载技能", "Loading skill"), failedTitle: (t) => t("加载技能失败", "Failed to load skill"), target: (item) => compact(inputString(item, "name")), summaryKind: "other" },
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
  // so the row carries the prompt's first line and whether the child inherits
  // this conversation. An ordinary row — the request has no card of its own.
  fork: {
    surface: "group",
    family: "raw",
    icon: GitFork,
    doneTitle: (t) => t("请求了分叉会话", "Requested a forked conversation"),
    runningTitle: (t) => t("正在请求分叉会话", "Requesting a forked conversation"),
    failedTitle: (t) => t("请求分叉会话失败", "Failed to request a forked conversation"),
    target: (item) => compact(inputString(item, "prompt")?.split("\n", 1)[0]),
    stat: (item, t) => (inputFlag(item, "inherit_context") ? t("继承上下文", "inherits context") : undefined),
    summaryKind: "other"
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
export function isOrdinaryToolCall(item: ToolContext): boolean {
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

export function getToolPresentation(
  item: ToolContext,
  descriptor?: ToolDescriptor,
  t: Translate = globalT
): ToolPresentation {
  const config = registryEntry(item.toolName);
  if (!config) {
    const phase = executionPhase(item);
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
      stat: item.result.durationMs > 0 ? `${item.result.durationMs} ms` : undefined
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
    ...(stat ? { stat } : {})
  };
}

function summaryPhrase(kind: SummaryKind, count: number, t: Translate): string {
  switch (kind) {
    case "fileChanges": return count === 1
      ? t("编辑了 1 个文件", "Edited 1 file")
      : t("编辑了 {count} 个文件", "Edited {count} files", { count });
    case "commands": return count === 1
      ? t("运行了 1 个命令", "Ran 1 command")
      : t("运行了 {count} 个命令", "Ran {count} commands", { count });
    case "browser": return count === 1
      ? t("执行了 1 次浏览器操作", "Performed 1 browser action")
      : t("执行了 {count} 次浏览器操作", "Performed {count} browser actions", { count });
    case "memory": return count === 1
      ? t("访问了 1 次记忆", "Accessed memory once")
      : t("访问了 {count} 次记忆", "Accessed memory {count} times", { count });
    case "search": return count === 1
      ? t("搜索了 1 次内容", "Performed 1 content search")
      : t("搜索了 {count} 次内容", "Performed {count} content searches", { count });
    case "files": return count === 1
      ? t("检查了 1 次文件与目录", "Checked files and directories once")
      : t("检查了 {count} 次文件与目录", "Checked files and directories {count} times", { count });
    case "agents": return count === 1
      ? t("1 次子代理操作", "1 subagent action")
      : t("{count} 次子代理操作", "{count} subagent actions", { count });
    case "state": return count === 1
      ? t("1 次状态变更", "1 state change")
      : t("{count} 次状态变更", "{count} state changes", { count });
    default: return count === 1
      ? t("使用了 1 个工具", "Used 1 tool")
      : t("使用了 {count} 个工具", "Used {count} tools", { count });
  }
}

export function summarizeToolGroup(items: ToolContext[], t: Translate = globalT): string {
  const ordinary = items.filter(isOrdinaryToolCall);
  if (!ordinary.length) return t("没有普通工具调用", "No regular tool calls");
  if (ordinary.length === 1) {
    const presentation = getToolPresentation(ordinary[0], undefined, t);
    return presentation.target ? `${presentation.title} ${presentation.target}` : presentation.title;
  }

  const counts = new Map<SummaryKind, number>();
  for (const item of ordinary) {
    const kind = registryEntry(item.toolName)?.summaryKind ?? "other";
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  const order: SummaryKind[] = ["fileChanges", "commands", "browser", "memory", "search", "files", "agents", "state", "other"];
  const phrases = order.flatMap((kind) => {
    const count = counts.get(kind) ?? 0;
    return count ? [summaryPhrase(kind, count, t)] : [];
  });
  const running = ordinary.filter((item) => executionPhase(item) === "running").length;
  const failed = ordinary.filter((item) => executionPhase(item) === "failed").length;
  const status = [
    running ? t("{count} 个执行中", "{count} running", { count: running }) : "",
    failed ? t("{count} 个失败", "{count} failed", { count: failed }) : ""
  ].filter(Boolean);
  return `${phrases.join(t("，", ", "))}${status.length ? ` · ${status.join(t("，", ", "))}` : ""}`;
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
  return (
    <div className="tool-renderer__code" role="region" aria-label={t("{path} 内容", "{path} contents", { path: inputString(item, "path") ?? t("文件", "File") })}>
      {lines.map((line, index) => (
        <div key={`${start + index}:${line}`} className={line.startsWith("… ") ? "tool-renderer__code-line tool-renderer__code-line--meta" : "tool-renderer__code-line"}>
          <span className="tool-renderer__line-number" aria-hidden="true">{start + index}</span>
          <code>{line || " "}</code>
        </div>
      ))}
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
        summary={item.result.output}
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

function BrowserNavigationView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const viewport = object(value.viewport);
  return (
    <div className="tool-renderer__browser-status">
      <InfoGrid rows={[
        [t("标题", "Title"), value.title],
        ["URL", value.url],
        [t("已打开", "Open"), value.open],
        [t("加载中", "Loading"), value.loading],
        [t("视口", "Viewport"), viewport ? `${scalarText(viewport.width)}×${scalarText(viewport.height)}` : undefined]
      ]} />
    </div>
  );
}

function BrowserSnapshotView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const tree = typeof value.tree === "string" ? value.tree : typeof value.text === "string" ? value.text : "";
  const elements = Array.isArray(value.elements) ? value.elements.length : 0;
  return (
    <div className="tool-renderer__snapshot">
      <InfoGrid rows={[["URL", value.url], [t("标题", "Title"), value.title], [t("交互元素", "Interactive elements"), elements]]} />
      {tree ? <pre className="tool-renderer__snapshot-tree">{tree}</pre> : <EmptyResult>{t("快照中没有文本树", "Snapshot contains no text tree")}</EmptyResult>}
    </div>
  );
}

function BrowserActionView({ item, presentation }: { item: ToolContext; presentation: ToolPresentation }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const element = object(value.element);
  const fields: Array<[string, unknown]> = [
    [t("目标", "Target"), presentation.target ?? element?.name ?? element?.tag],
    [t("元素", "Element"), element ? [element.tag, element.ref].filter(Boolean).join(" · ") : undefined],
    [t("值", "Value"), element?.value],
    [t("输入模式", "Input mode"), value.input],
    [t("按键", "Key"), value.key],
    [t("匹配", "Matched"), value.matched],
    [t("已完成", "Completed"), value.completed],
    [t("总计", "Total"), value.total],
    [t("选择值", "Selected values"), value.values],
    [t("滚动位置", "Scroll position"), value.scrollY === undefined ? undefined : `${scalarText(value.scrollX ?? 0)}, ${scalarText(value.scrollY)}`],
    [t("文件数", "Files"), value.files]
  ];
  return (
    <div className="tool-renderer__browser-action">
      <InfoGrid rows={fields} />
      {!fields.some(([, field]) => field !== undefined && field !== "") && (
        <pre className="tool-renderer__json-result">{safeStringify(value)}</pre>
      )}
    </div>
  );
}

function BrowserEvaluateView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  if (!parsed.parsed) return <JsonFallback item={item} parsed={parsed} />;
  return (
    <div className="tool-renderer__evaluate">
      <div className="tool-renderer__script">
        <span>JavaScript</span>
        <pre>{inputString(item, "script") ?? ""}</pre>
      </div>
      <div className="tool-renderer__json-result" aria-label={t("JavaScript 返回值", "JavaScript return value")}>
        <pre>{safeStringify(parsed.value)}</pre>
      </div>
    </div>
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
        [t("保存路径", "Saved path"), value.path ?? inputString(item, "path")],
        [t("尺寸", "Dimensions"), value.width !== undefined && value.height !== undefined ? `${scalarText(value.width)}×${scalarText(value.height)}` : undefined],
        [t("字节", "Bytes"), value.bytes],
        [t("完整页面", "Full page"), value.fullPage]
      ]} />
    </div>
  );
}

function logEntryText(entry: Record<string, unknown>): string {
  const direct = [entry.message, entry.text].find((value) => typeof value === "string" && value);
  if (typeof direct === "string") return direct;
  return entry.args === undefined ? safeStringify(entry) : scalarText(entry.args);
}

function BrowserConsoleView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const entries = Array.isArray(value.entries) ? value.entries : [];
  if (!entries.length) return <EmptyResult>{t("没有 Console 日志", "No Console logs")}</EmptyResult>;
  return (
    <ol className="tool-renderer__log-list" aria-label={t("Console 日志", "Console logs")}>
      {entries.map((entry, index) => {
        const row = object(entry) ?? { message: scalarText(entry) };
        const level = typeof row.level === "string" ? row.level : "log";
        return (
          <li key={`${index}:${level}`} className={`tool-renderer__log tool-renderer__log--${level}`}>
            <span>{level}</span>
            <code>{logEntryText(row)}</code>
          </li>
        );
      })}
    </ol>
  );
}

function BrowserNetworkView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const entries = Array.isArray(value.entries) ? value.entries : [];
  if (!entries.length) return <EmptyResult>{t("没有网络记录", "No network records")}</EmptyResult>;
  return (
    <ol className="tool-renderer__network-list" aria-label={t("网络日志", "Network logs")}>
      {entries.map((entry, index) => {
        const row = object(entry) ?? {};
        const status = row.status ?? row.statusCode;
        return (
          <li key={`${index}:${scalarText(row.url ?? "")}`}>
            <span className="tool-renderer__network-method">{scalarText(row.method ?? row.type ?? "GET")}</span>
            {status !== undefined && <span className="tool-renderer__network-status">{scalarText(status)}</span>}
            <code>{scalarText(row.url ?? entry)}</code>
          </li>
        );
      })}
    </ol>
  );
}

function BrowserDialogView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const records = Array.isArray(value.records) ? value.records : [];
  if (!records.length) return <EmptyResult>{t("没有页面对话框记录", "No page-dialog records")}</EmptyResult>;
  return (
    <ol className="tool-renderer__dialog-list" aria-label={t("页面对话框记录", "Page-dialog records")}>
      {records.map((entry, index) => {
        const row = object(entry) ?? {};
        return (
          <li key={`${index}:${scalarText(row.type ?? "dialog")}`}>
            <strong>{scalarText(row.type ?? "dialog")}</strong>
            <span>{scalarText(row.message ?? row.text ?? "")}</span>
            {row.result !== undefined && <em>{scalarText(row.result)}</em>}
          </li>
        );
      })}
    </ol>
  );
}

function BrowserStatusView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const parsed = parseJsonOutput(item);
  const value = parsed.parsed ? object(parsed.value) : null;
  if (!value) return <JsonFallback item={item} parsed={parsed} />;
  const viewport = object(value.viewport);
  return (
    <InfoGrid rows={[
      [t("浏览器已打开", "Browser open"), value.open],
      ["URL", value.url],
      [t("标题", "Title"), value.title],
      [t("视口", "Viewport"), viewport ? `${scalarText(viewport.width)}×${scalarText(viewport.height)}` : resizeStat(item)]
    ]} />
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
  const rows: Array<[string, unknown]> = [
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
          <InfoGrid rows={rows} />
          {item.result.output ? <pre>{item.result.output}</pre> : null}
        </div>
      </div>
    );
  }
  return (
    <div className="tool-renderer__memory">
      <InfoGrid rows={rows} />
      {item.result.output
        ? <pre className="tool-renderer__plain-output">{item.result.output}</pre>
        : null}
    </div>
  );
}

function RawView({ item }: { item: ToolContext }) {
  return item.result.output
    ? <pre className="tool-renderer__plain-output">{item.result.output}</pre>
    : <EmptyResult />;
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
  const status = agentTimelineRunStatus(item);
  if (!instruction && !updates.length && !item.result.output) return <EmptyResult />;
  return (
    <div className="tool-renderer__agent">
      <InfoGrid rows={[
        [t("子代理", "Subagent"), item.subagent?.label || item.subagent?.name || undefined],
        [t("状态", "Status"), runStatusLabel(status, t)]
      ]} />
      {instruction && (
        <div className="tool-renderer__agent-section">
          <small>{t("派发指令", "Instruction")}</small>
          <pre className="tool-renderer__plain-output">{instruction}</pre>
        </div>
      )}
      {updates.length > 0 && (
        <div className="tool-renderer__agent-section">
          <small>{t("子代理更新", "Subagent updates")}</small>
          <ul className="tool-renderer__agent-updates">
            {updates.map((update) => (
              <li key={`${update.createdAt}:${update.content}`}>{update.content}</li>
            ))}
          </ul>
        </div>
      )}
      {item.result.output && (
        <div className="tool-renderer__agent-section">
          <small>{t("回执", "Receipt")}</small>
          <pre className="tool-renderer__plain-output">{item.result.output}</pre>
        </div>
      )}
    </div>
  );
}

/** `task_wait` output split back into the `[name · status]` envelopes it drained. */
function AgentWaitView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  const { envelopes, statusLine, notice } = parseWaitOutput(item.result.output);
  if (!envelopes.length) {
    return notice
      ? <pre className="tool-renderer__plain-output">{item.result.output}</pre>
      : <EmptyResult>{t("等待已结束，没有可显示的更新", "The wait finished with no updates to show")}</EmptyResult>;
  }
  return (
    <div className="tool-renderer__agent">
      {notice && <pre className="tool-renderer__plain-output">{notice}</pre>}
      <ul className="tool-renderer__wait-list" aria-label={t("任务更新", "Task updates")}>
        {envelopes.map((envelope, index) => (
          <li key={`${envelope.agent}:${envelope.status}:${index}`}>
            <span className="tool-renderer__wait-head">
              <Bot size={12} aria-hidden="true" />
              <strong>{envelope.agent}</strong>
              <em>{envelope.status}</em>
            </span>
            {envelope.body && <pre className="tool-renderer__plain-output">{envelope.body}</pre>}
          </li>
        ))}
      </ul>
      {statusLine && <p className="tool-renderer__wait-status">{statusLine}</p>}
    </div>
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
  if (!message && !item.result.output) return <EmptyResult />;
  return (
    <div className="tool-renderer__agent">
      {message && (
        <div className="tool-renderer__agent-section">
          <small>{t("内容", "Message")}</small>
          <pre className="tool-renderer__plain-output">{message}</pre>
        </div>
      )}
      {item.result.output && (
        <div className="tool-renderer__agent-section">
          <small>{t("回执", "Receipt")}</small>
          <pre className="tool-renderer__plain-output">{item.result.output}</pre>
        </div>
      )}
    </div>
  );
}

/** A `todo` write: the fields the call set, plus the host's receipt. */
function StateToolView({ item }: { item: ToolContext }) {
  const { t } = useI18n();
  return (
    <div className="tool-renderer__state">
      <InfoGrid rows={[
        [t("标题", "Subject"), inputString(item, "subject")],
        [t("状态", "Status"), todoStatusLabel(item, t)],
        [t("任务 ID", "Task id"), inputString(item, "taskId")],
        [t("说明", "Description"), inputString(item, "description")]
      ]} />
      {item.result.output
        ? <pre className="tool-renderer__plain-output">{item.result.output}</pre>
        : null}
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
    case "browser-navigation": return <BrowserNavigationView item={item} />;
    case "browser-snapshot": return <BrowserSnapshotView item={item} />;
    case "browser-action": return <BrowserActionView item={item} presentation={presentation} />;
    case "browser-evaluate": return <BrowserEvaluateView item={item} />;
    case "browser-screenshot": return <BrowserScreenshotView item={item} />;
    case "browser-console": return <BrowserConsoleView item={item} />;
    case "browser-network": return <BrowserNetworkView item={item} />;
    case "browser-dialog": return <BrowserDialogView item={item} />;
    case "browser-status": return <BrowserStatusView item={item} />;
    case "memory": return <MemoryView item={item} presentation={presentation} />;
    case "agent-run": return <AgentRunView item={item} />;
    case "agent-wait": return <AgentWaitView item={item} />;
    case "agent-note": return <AgentNoteView item={item} />;
    case "persistent": return <StateToolView item={item} />;
    default: return <RawView item={item} />;
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
