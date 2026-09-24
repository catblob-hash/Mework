import {
  Eye,
  FileCode2,
  MousePointerClick,
  Server,
  SquareTerminal,
  type LucideIcon
} from "lucide-react";
import { useState } from "react";
import { useI18n } from "../i18n";
import {
  decisionFormAllowed,
  type DecisionForm,
  type DecisionToolSettings,
  switchToolsOff,
  switchToolsOn
} from "../lib/decisionParameters";
import { backendOfTool, SHELL_BACKENDS, shellBackendLabel } from "../lib/machineShells";
import { DECISION_VARIANT_TOOLS, isPreviewToolName, scoresMisses } from "../lib/taskTools";
import type {
  DecisionParameterModes,
  RememberedDecisionForms,
  ShellBackend,
  ToolDescriptor
} from "../types";
import { Dialog, Switch } from "./Common";
import { lockedInThisConversationHint } from "./ConversationSettingsPages";
import { ToolDocsLink } from "./DocsLink";
import { SettingsPageHeading } from "./SettingsPageHeading";
import "./ToolFamilySettings.css";

type Translate = ReturnType<typeof useI18n>["t"];

/**
 * Where a tool's decision-model form lives.
 *
 * `parameters`: `query` and `threshold` on the tool itself, per the
 * conversation's `decisionParameterModes`. `tool`: a separate catalog tool —
 * `preview_find_logs` for `preview_logs`, `bash_find_output` for `bash` — so
 * the form is read off which of the two names are enabled: the base alone is
 * the direct form, both are `augment`, and the variant alone is `replace`.
 */
type DecisionVariant = { kind: "parameters" } | { kind: "tool"; tool: string };

export interface ToolFamilyEntry {
  name: string;
  variant?: DecisionVariant;
}

interface ToolFamilyPage {
  id: string;
  icon: LucideIcon;
  title: (t: Translate) => string;
  description: (t: Translate) => string;
  tools: readonly ToolFamilyEntry[];
}

export type ToolFamilyId = "files" | "shell" | "preview";

/**
 * Tools that are one capability split into many calls for the model's benefit,
 * not the reader's. The picker shows each family as one row, and that row opens
 * the family's own settings window, where each tool is its own switch and a
 * tool with a decision-model form chooses it. The stored list keeps the real
 * names: nothing downstream of the picker has ever heard of a family.
 */
export interface ToolFamily {
  id: ToolFamilyId;
  /** The picker row's name. */
  name: string;
  category: ToolDescriptor["category"];
  /** Whether a catalog tool is gathered behind this family's row. */
  owns: (name: string) => boolean;
  title: (t: Translate) => string;
  settingsLabel: (t: Translate) => string;
  navLabel: (t: Translate) => string;
  /** Under a page that offers decision-model forms: what they send, and where. */
  decisionNote: (t: Translate) => string;
  /** Every catalog tool the family owns appears here exactly once, as a row or as the variant under one. */
  pages: readonly ToolFamilyPage[];
}

/** The file tools the picker gathers; the ones that go through the decision model keep their own rows. */
const FILE_TOOL_NAMES: readonly string[] = ["ls", "find", "grep", "read", "write", "edit"];
const FILE_TOOL_NAME_SET: ReadonlySet<string> = new Set(FILE_TOOL_NAMES);

const FILES_FAMILY: ToolFamily = {
  id: "files",
  name: "files",
  category: "filesystem",
  owns: (name) => FILE_TOOL_NAME_SET.has(name),
  title: (t) => t("文件工具", "File tools"),
  settingsLabel: (t) => t("文件工具设置", "File tool settings"),
  navLabel: (t) => t("文件工具分类", "File tool categories"),
  decisionNote: () => "",
  pages: [
    {
      id: "files",
      icon: FileCode2,
      title: (t) => t("文件操作", "File operations"),
      description: (t) => t(
        "列出、查找、搜索、读取、写入和编辑工作区里的文件。",
        "List, find, search, read, write and edit the files in the workspace."
      ),
      tools: FILE_TOOL_NAMES.map((name) => ({ name }))
    }
  ]
};

const SHELL_FAMILY: ToolFamily = {
  id: "shell",
  name: "shell",
  category: "shell",
  owns: (name) => backendOfTool(name) !== null,
  title: (t) => t("Shell 工具", "Shell tools"),
  settingsLabel: (t) => t("Shell 工具设置", "Shell tool settings"),
  navLabel: (t) => t("Shell 工具分类", "Shell tool categories"),
  decisionNote: (t) => t(
    "决策模型选项会把命令输出发给决策模型提供商（TypeSafe），需要先在「全局设置 → 决策模型提供商」里填好密钥。两个选项二选一：前者保留原来的命令工具，后者只走决策模型。命令输出切块后逐块打分，只返回分数不低于 threshold 的片段。",
    "The decision-model options send command output to the decision model provider (TypeSafe), which needs its key under Global settings → Decision model providers. The two options are exclusive: the first keeps the plain command tool, the second relies on the decision model alone. The output is cut into pieces and each is scored, and only the pieces at or above threshold come back."
  ),
  pages: [
    {
      id: "shell",
      icon: SquareTerminal,
      title: () => "Shell",
      description: (t) => t(
        "在对话所用的机器上运行命令。这里只列出这些机器上找到的 shell；每个 shell 打开后都能另选决策模型形式，只取回输出里和描述相符的片段。",
        "Run commands on the conversation's machines. Only the shells found on those machines are listed; each one, once on, can also take a decision-model form that returns only the parts of its output that match a description."
      ),
      tools: SHELL_BACKENDS.map((backend) => ({
        name: backend,
        variant: { kind: "tool", tool: DECISION_VARIANT_TOOLS[backend] }
      }))
    }
  ]
};

const PREVIEW_FAMILY: ToolFamily = {
  id: "preview",
  name: "preview",
  category: "web",
  // The prefix is the whole test, so a preview tool added to the catalog is
  // gathered behind the row the day it lands.
  owns: isPreviewToolName,
  title: (t) => t("预览工具", "Preview tools"),
  settingsLabel: (t) => t("预览工具设置", "Preview tool settings"),
  navLabel: (t) => t("预览工具分类", "Preview tool categories"),
  decisionNote: (t) => t(
    "决策模型选项会把页面或日志的内容发给决策模型提供商（TypeSafe），需要先在「全局设置 → 决策模型提供商」里填好密钥。两个选项二选一：前者保留原来的直接操作，后者只走决策模型。读取类工具按 threshold 打分筛选；点击、填写、检查元素是让决策模型从页面元素里选一个，宿主总会附上「以上皆非」这一项。",
    "The decision-model options send page or log contents to the decision model provider (TypeSafe), which needs its key under Global settings → Decision model providers. The two options are exclusive: the first keeps the direct form, the second relies on the decision model alone. The reading tools score and cut at threshold; click, fill and inspect have the model choose one element, and the host always offers none of the above."
  ),
  pages: [
    {
      id: "servers",
      icon: Server,
      title: (t) => t("开发服务器管理", "Dev servers"),
      description: (t) => t(
        "启动、停止开发服务器并读取它的输出。服务器按 .mework/launch.json 里的配置启动。",
        "Start and stop dev servers and read their output. Servers start from the configurations in .mework/launch.json."
      ),
      tools: [
        { name: "preview_start" },
        { name: "preview_stop" },
        { name: "preview_list" },
        { name: "preview_logs", variant: { kind: "tool", tool: DECISION_VARIANT_TOOLS.preview_logs } }
      ]
    },
    {
      id: "observe",
      icon: Eye,
      title: (t) => t("查看页面", "Read the page"),
      description: (t) => t(
        "读取预览页上的内容：Console、画面、可访问性树、元素样式与网络请求。",
        "Read what the preview page holds: its console, its pixels, the accessibility tree, element styles and network requests."
      ),
      tools: [
        { name: "preview_console_logs", variant: { kind: "parameters" } },
        { name: "preview_screenshot" },
        { name: "preview_snapshot", variant: { kind: "parameters" } },
        { name: "preview_inspect", variant: { kind: "parameters" } },
        { name: "preview_network" }
      ]
    },
    {
      id: "act",
      icon: MousePointerClick,
      title: (t) => t("操作页面", "Act on the page"),
      description: (t) => t(
        "在预览页上动手：点击、填写、执行脚本、调整视口、上传图片、应答对话框。",
        "Act on the preview page: click, fill, run script, resize the viewport, upload images and answer dialogs."
      ),
      tools: [
        { name: "preview_click", variant: { kind: "parameters" } },
        { name: "preview_fill", variant: { kind: "parameters" } },
        { name: "preview_eval" },
        { name: "preview_resize" },
        { name: "preview_upload_image" },
        { name: "preview_dialog" }
      ]
    }
  ]
};

/** The families in the order the picker would meet their first tools. */
export const TOOL_FAMILIES: readonly ToolFamily[] = [FILES_FAMILY, SHELL_FAMILY, PREVIEW_FAMILY];

/** Tools that only ever appear as the variant under another row of `family`. */
export function familyVariantNames(family: ToolFamily): ReadonlySet<string> {
  return new Set(family.pages.flatMap((page) => page.tools)
    .flatMap((entry) => (entry.variant?.kind === "tool" ? [entry.variant.tool] : [])));
}

/** The rows `family`'s window would draw from `available`, page after page. */
function familyEntries(family: ToolFamily, available: ReadonlySet<string>): ToolFamilyEntry[] {
  return family.pages.flatMap((page) => page.tools).filter((entry) => available.has(entry.name));
}

/** The names a row stands for among `available`: its tool, and its variant tool when it has one. */
export function familyRowNames(
  family: ToolFamily,
  row: string,
  available: ReadonlySet<string>
): string[] {
  const entry = familyEntries(family, available).find((candidate) => candidate.name === row);
  if (!entry) return [];
  return entry.variant?.kind === "tool" && available.has(entry.variant.tool)
    ? [entry.name, entry.variant.tool]
    : [entry.name];
}

/**
 * The rows of `family`'s window that are on, each by its row name: on when its
 * tool, its variant or both are.
 */
export function familyRowsOn(
  family: ToolFamily,
  available: ReadonlySet<string>,
  isOn: (name: string) => boolean
): string[] {
  return familyEntries(family, available)
    .filter((entry) => familyRowNames(family, entry.name, available).some(isOn))
    .map((entry) => entry.name);
}

/**
 * How many of the rows `family`'s window would draw are on, out of how many it
 * would draw. A row counts once whether its tool, its variant or both are on.
 */
export function familyRowCounts(
  family: ToolFamily,
  available: ReadonlySet<string>,
  isOn: (name: string) => boolean
): { enabled: number; total: number } {
  return {
    enabled: familyRowsOn(family, available, isOn).length,
    total: familyEntries(family, available).length
  };
}

/** The backend a shell's command tool runs in; `null` for its scoring tool and every other tool. */
function commandBackend(name: string): ShellBackend | null {
  const backend = backendOfTool(name);
  return backend === name ? backend : null;
}

function toolDescription(name: string, t: Translate): string {
  const backend = commandBackend(name);
  if (backend) {
    return t(
      "用 {shell} 运行命令并返回完整输出，也可以放到后台运行，之后用 task_wait 取回结果。",
      "Runs a command in {shell} and returns its whole output, or runs it in the background for task_wait to collect.",
      { shell: shellBackendLabel(backend) }
    );
  }
  switch (name) {
    case "ls": return t("列出目录里的文件，可以指定递归深度。", "Lists the files in a directory, to the depth asked for.");
    case "find": return t("按文件名模式（如 *.tsx）查找文件。", "Finds files by name pattern, such as *.tsx.");
    case "grep": return t("按正则表达式搜索文件内容，返回匹配的行。", "Searches file contents by regular expression and returns the matching lines.");
    case "read": return t("读取一个文件，可以只读其中一段行。", "Reads a file, or a range of its lines.");
    case "write": return t("新建一个文件，或者整个覆盖已有的文件。", "Creates a file, or overwrites an existing one whole.");
    case "edit": return t("把文件里唯一匹配的一段文本替换成新文本。", "Replaces the one passage in a file that matches the search text with new text.");
    case "preview_start": return t("按 .mework/launch.json 里的配置名启动开发服务器，并把预览页指向它。", "Starts a dev server by its name in .mework/launch.json and points the preview page at it.");
    case "preview_stop": return t("停止一个正在运行的开发服务器。", "Stops a running dev server.");
    case "preview_list": return t("列出本对话正在运行的开发服务器。", "Lists the dev servers running for this conversation.");
    case "preview_logs": return t("读取开发服务器的 stdout 与 stderr，可按级别、行数和文本过滤。", "Reads a dev server's stdout and stderr, filtered by level, line count or text.");
    case "preview_console_logs": return t("读取预览页的 Console 输出，可按 all / error / warn 过滤。", "Reads the preview page's console output, filtered by all / error / warn.");
    case "preview_screenshot": return t("截取预览页的画面。只有能看图的模型会拿到这个工具。", "Captures the preview page as an image. Only models that can see images are offered it.");
    case "preview_snapshot": return t("读取页面的可访问性树：文本、角色和元素 uid。", "Reads the page's accessibility tree: text, roles and element uids.");
    case "preview_inspect": return t("按 CSS 选择器读取一个元素的属性和计算样式。", "Reads one element's attributes and computed styles by CSS selector.");
    case "preview_network": return t("列出页面发出的网络请求，或取回某个请求的响应正文。", "Lists the page's network requests, or reads one response body.");
    case "preview_click": return t("按 CSS 选择器点击元素，也可以双击。", "Clicks an element by CSS selector, or double-clicks it.");
    case "preview_fill": return t("按 CSS 选择器填写输入框。", "Fills an input by CSS selector.");
    case "preview_eval": return t("在页面里执行一段 JavaScript 表达式并返回结果。", "Evaluates a JavaScript expression in the page and returns the result.");
    case "preview_resize": return t("切换设备预设或视口尺寸，并可模拟深色与浅色。", "Switches the device preset or viewport size, and can emulate dark or light.");
    case "preview_upload_image": return t("把对话里的图片放进页面的文件输入框。只有能看图的模型会拿到这个工具。", "Puts an image from the conversation into a file input on the page. Only models that can see images are offered it.");
    case "preview_dialog": return t("接受或取消页面弹出的 alert、confirm、prompt。", "Accepts or dismisses an alert, confirm or prompt the page opened.");
    default: return "";
  }
}

/** What `augment` means for one tool: the direct form stays, the decision model joins it. */
function augmentDescription(name: string, t: Translate): string {
  const backend = commandBackend(name);
  if (backend) {
    return t(
      "同时启用 {tool}：运行命令时用 query 描述要找什么，输出切块交给决策模型打分，只返回分数不低于 threshold 的片段。原来的 {shell} 仍然可用。",
      "Also enables {tool}: a command run with query describing what to look for has its output cut into pieces for the decision model to score, and only the pieces at or above threshold come back. Plain {shell} stays available.",
      { tool: DECISION_VARIANT_TOOLS[backend], shell: shellBackendLabel(backend) }
    );
  }
  switch (name) {
    case "preview_logs": return t(
      "同时启用 preview_find_logs：用一句话描述要找什么，决策模型从 Console 与服务器日志里挑出相关片段。",
      "Also enables preview_find_logs: describe what to look for, and the decision model picks the matching pieces out of the console and server logs."
    );
    case "preview_console_logs": return t(
      "保留原有列表，另外可以传 query 与 threshold：按 level 过滤后的日志交给决策模型，只返回相关片段。",
      "Keeps the plain listing and also takes query and threshold: the entries that pass level go to the decision model, and only the matching pieces come back."
    );
    case "preview_snapshot": return t(
      "保留整份快照，另外可以传 query 与 threshold，只取回和描述相符的元素及其选择器。",
      "Keeps the whole snapshot and also takes query and threshold, returning only the elements that match the description, with their selectors."
    );
    case "preview_inspect": return t(
      "保留 selector，另外可以用 query 描述元素：决策模型从页面元素里选出所指的一个，都不符合时选「以上皆非」，按未找到回答，并列出它看过的元素行。",
      "Keeps selector and also takes query: the decision model chooses the element described, or answers none of the above and the element is reported as not found, with the element lines it was shown."
    );
    case "preview_click": return t(
      "保留 selector，另外可以用 query 描述元素：决策模型从页面元素里选出所指的一个，都不符合时选「以上皆非」，什么也不点，并列出它看过的元素行。",
      "Keeps selector and also takes query: the decision model chooses the element described, or answers none of the above and nothing is clicked, with the element lines it was shown listed back."
    );
    case "preview_fill": return t(
      "保留 selector，另外可以用 query 描述输入框：决策模型从页面元素里选出所指的一个，都不符合时选「以上皆非」，什么也不填，并列出它看过的元素行。",
      "Keeps selector and also takes query: the decision model chooses the input described, or answers none of the above and nothing is filled, with the element lines it was shown listed back."
    );
    default: return "";
  }
}

/** What `replace` means for one tool: the direct form goes, every call asks the decision model. */
function replaceDescription(name: string, t: Translate): string {
  const backend = commandBackend(name);
  if (backend) {
    return t(
      "只启用 {tool}：每次运行命令都必须带 query 与 threshold，不再提供返回完整输出、可以后台运行的 {shell}。",
      "Enables {tool} alone: every command must carry query and threshold, and plain {shell}, with its whole output and background runs, is no longer offered.",
      { tool: DECISION_VARIANT_TOOLS[backend], shell: shellBackendLabel(backend) }
    );
  }
  switch (name) {
    case "preview_logs": return t(
      "只启用 preview_find_logs，不再提供按级别和文本过滤的原始服务器日志。",
      "Enables preview_find_logs alone; the raw server log with its level and text filters is no longer offered."
    );
    case "preview_console_logs": return t(
      "每次调用都必须带 query 与 threshold，不再返回完整的日志列表。",
      "Every call must carry query and threshold; the plain listing is no longer offered."
    );
    case "preview_snapshot": return t(
      "每次调用都必须带 query 与 threshold，不再返回整份快照。",
      "Every call must carry query and threshold; the whole snapshot is no longer offered."
    );
    case "preview_inspect":
    case "preview_click":
    case "preview_fill": return t(
      "去掉 selector，每次都用 query 描述元素，完全交给决策模型选择。",
      "Withdraws selector: every call describes its element with query, and the decision model does all the choosing."
    );
    default: return "";
  }
}

/** What scoring a miss does, the same on click, fill and inspect. */
function missScoringDescription(t: Translate): string {
  return t(
    "决策模型选了「以上皆非」时，把刚才给它看的每一行元素再逐个对照描述打分，按分数从高到低连同分数一起返回，代替原样列出。每行一次请求，一次未命中最多 254 次。需要先选上面任一种决策模型形式。",
    "When the decision model answers none of the above, every element line it was shown is scored against the description on its own, and the lines come back highest first with their scores instead of as they were shown. One request per line, at most 254 per miss. Needs one of the decision-model forms above."
  );
}

function formLockedHint(t: Translate): string {
  return t(
    "本对话里已经有这种形式的调用，只能改成仍然接受它们的形式。",
    "Calls in this form are already in this conversation; only a form that still accepts them can be chosen."
  );
}

interface ToolFamilySettingsProps {
  family: ToolFamily;
  /** The family's tools as this owner's catalog has them; a row whose tool is missing is not drawn. */
  tools: readonly ToolDescriptor[];
  enabledTools: readonly string[];
  /** Tools a run has already put in front of the model; they cannot be switched off. */
  lockedTools: readonly string[];
  /**
   * The conversation's decision-parameter modes. Omitted where the owner has
   * none to edit — a role's tool list follows the conversation's modes — which
   * hides the sub-options that set them.
   */
  decisionParameterModes?: DecisionParameterModes;
  /** The forms the lock says each exposed tool's schema has already covered. */
  lockedDecisionParameterModes?: DecisionParameterModes;
  /**
   * The element tools whose misses the conversation scores. Edited only where
   * the modes are, since only a tool with a form can miss.
   */
  decisionMissScoring?: readonly string[];
  /** The sub-option choices the conversation keeps for its switched-off tools. */
  rememberedDecisionForms?: RememberedDecisionForms;
  /**
   * One edit, one write, carrying every field it may touch: a tool switched
   * off takes its form and miss scoring into the remembered choices, and one
   * switched back on takes them out again. An owner that keeps no modes reads
   * the tool list alone.
   */
  onChange: (next: DecisionToolSettings) => void;
  onClose: () => void;
  /**
   * Said above every page when the window is not editing live tools: the
   * family is off, and what the window chooses is what switching it on uses.
   */
  notice?: string;
}

/**
 * A tool family's own settings window, laid out like the global settings:
 * pages down the left, one row per tool on the right with its switch at the
 * end. A tool with a decision-model form slides its two mutually exclusive
 * forms out under its row while it is on.
 */
export function ToolFamilySettingsDialog(props: ToolFamilySettingsProps) {
  const { t } = useI18n();
  const { family } = props;
  const [page, setPage] = useState(family.pages[0].id);
  return (
    <Dialog
      title={family.title(t)}
      width="880px"
      bodyClassName="dialog__body--flush"
      onClose={props.onClose}
    >
      <section
        className="global-settings-layout tool-family-settings"
        data-tool-family={family.id}
        aria-label={family.settingsLabel(t)}
      >
        <nav className="settings-nav" aria-label={family.navLabel(t)}>
          {family.pages.map((item) => {
            const Icon = item.icon;
            return (
              <button
                type="button"
                key={item.id}
                aria-current={page === item.id || undefined}
                className={page === item.id ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"}
                onClick={() => setPage(item.id)}
              >
                <Icon size={16} aria-hidden="true" /><span>{item.title(t)}</span>
              </button>
            );
          })}
        </nav>
        <div className="global-settings-content">
          <ToolFamilyPageView page={page} {...props} />
        </div>
      </section>
    </Dialog>
  );
}

function ToolFamilyPageView({
  page: pageId,
  family,
  tools,
  enabledTools,
  lockedTools,
  decisionParameterModes,
  lockedDecisionParameterModes,
  decisionMissScoring,
  rememberedDecisionForms,
  onChange,
  notice
}: ToolFamilySettingsProps & { page: string }) {
  const { t } = useI18n();
  const page = family.pages.find((item) => item.id === pageId) ?? family.pages[0];
  const descriptors = new Map(tools.map((tool) => [tool.name, tool]));
  const locked = new Set(lockedTools);
  const isOn = (name: string) => enabledTools.includes(name) || locked.has(name);
  const modesEditable = decisionParameterModes !== undefined;
  const entries = page.tools.filter((entry) => descriptors.has(entry.name));
  const hasVariants = entries.some((entry) => variantShown(entry));

  function variantShown(entry: ToolFamilyEntry): boolean {
    if (!entry.variant) return false;
    return entry.variant.kind === "tool" ? descriptors.has(entry.variant.tool) : modesEditable;
  }

  /** The names a row stands for: the tool, and its variant tool when it has one. */
  function namesOf(entry: ToolFamilyEntry): string[] {
    return entry.variant?.kind === "tool" && descriptors.has(entry.variant.tool)
      ? [entry.name, entry.variant.tool]
      : [entry.name];
  }

  function formOf(entry: ToolFamilyEntry): DecisionForm {
    if (entry.variant?.kind === "tool") {
      const base = isOn(entry.name);
      const variant = isOn(entry.variant.tool);
      return base && variant ? "augment" : variant ? "replace" : null;
    }
    return decisionParameterModes?.[entry.name] ?? null;
  }

  /** What the lock says this row's calls have already covered; `undefined` while nothing has. */
  function floorOf(entry: ToolFamilyEntry): DecisionForm | undefined {
    if (entry.variant?.kind === "tool") {
      const base = locked.has(entry.name);
      const variant = locked.has(entry.variant.tool);
      if (!base && !variant) return undefined;
      return base && variant ? "augment" : variant ? "replace" : null;
    }
    if (!locked.has(entry.name)) return undefined;
    return lockedDecisionParameterModes?.[entry.name] ?? null;
  }

  const scoring = decisionMissScoring ?? [];
  const settings: DecisionToolSettings = {
    enabledTools: [...enabledTools],
    decisionParameterModes: decisionParameterModes ?? {},
    decisionMissScoring: [...scoring],
    rememberedDecisionForms: rememberedDecisionForms ?? {}
  };

  function without(tool: string): string[] {
    return scoring.filter((name) => name !== tool);
  }

  function setOn(entry: ToolFamilyEntry, on: boolean) {
    // The row comes back to the choices it was switched off with, and going
    // off keeps them rather than starting the row over from the direct form.
    if (on) onChange(switchToolsOn(settings, [entry.name], new Set(descriptors.keys())));
    else onChange(switchToolsOff(settings, new Set(namesOf(entry).filter((name) => !locked.has(name)))));
  }

  function setForm(entry: ToolFamilyEntry, form: DecisionForm) {
    if (entry.variant?.kind === "tool") {
      const variant = entry.variant.tool;
      const keep = enabledTools.filter((name) => name !== entry.name && name !== variant);
      const next = form === null ? [entry.name] : form === "augment" ? [entry.name, variant] : [variant];
      onChange({ ...settings, enabledTools: Array.from(new Set([...keep, ...next])) });
      return;
    }
    const nextModes = { ...settings.decisionParameterModes };
    if (form === null) delete nextModes[entry.name];
    else nextModes[entry.name] = form;
    /* Back to the direct form takes miss scoring with it: nothing is left to miss. */
    onChange({
      ...settings,
      decisionParameterModes: nextModes,
      decisionMissScoring: form === null ? without(entry.name) : settings.decisionMissScoring
    });
  }

  function setMissScoring(entry: ToolFamilyEntry, on: boolean) {
    onChange({
      ...settings,
      decisionMissScoring: on ? [...without(entry.name), entry.name] : without(entry.name)
    });
  }

  return (
    <div className="settings-page tool-family-settings__page">
      <SettingsPageHeading title={page.title(t)} description={page.description(t)} />
      {notice && <p className="tool-family-settings__notice" role="note">{notice}</p>}
      <section className="settings-card tool-family-settings__card">
        {entries.map((entry) => {
          const descriptor = descriptors.get(entry.name)!;
          const on = entry.variant?.kind === "tool"
            ? namesOf(entry).some(isOn)
            : isOn(entry.name);
          const rowLocked = namesOf(entry).some((name) => locked.has(name));
          const form = formOf(entry);
          const floor = floorOf(entry);
          const shown = variantShown(entry);
          const regionId = `tool-family-forms-${entry.name}`;
          const formSwitch = (target: Exclude<DecisionForm, null>) => {
            const checked = form === target;
            const next: DecisionForm = checked ? null : target;
            return {
              checked,
              disabled: !decisionFormAllowed(floor, next),
              onChange: () => setForm(entry, next)
            };
          };
          const augment = formSwitch("augment");
          const replace = formSwitch("replace");
          const formsLocked = shown && on && (augment.disabled || replace.disabled);
          const missScoring = entry.variant?.kind === "parameters" && scoresMisses(entry.name);
          /* Both forms switch the variant tool on, so each is also the way in to its page. */
          const variantTool = entry.variant?.kind === "tool" ? descriptors.get(entry.variant.tool) : undefined;
          const variantDocs = variantTool && <ToolDocsLink name={variantTool.name} label={variantTool.label} />;
          return (
            <div className="tool-family-settings__entry" key={entry.name} data-tool-name={entry.name}>
              <div className="tool-family-settings__row">
                <div className="tool-family-settings__row-copy">
                  <span className="tool-family-settings__row-title">
                    <ToolDocsLink name={entry.name} label={descriptor.label} />
                    <strong>{descriptor.label}</strong>
                    <code>{entry.name}</code>
                    {descriptor.dangerous && <em>{t("需审查", "Reviewed")}</em>}
                  </span>
                  <small>{toolDescription(entry.name, t)}</small>
                  {rowLocked && <small className="tool-family-settings__hint">{lockedInThisConversationHint(t)}</small>}
                </div>
                <div className="tool-family-settings__row-control">
                  <Switch
                    checked={on}
                    disabled={rowLocked && on}
                    onChange={(checked) => setOn(entry, checked)}
                    label={on
                      ? t("{label}已启用", "{label} enabled", { label: descriptor.label })
                      : t("{label}已关闭", "{label} disabled", { label: descriptor.label })}
                  />
                </div>
              </div>
              {shown && (
                <div
                  id={regionId}
                  className={`collapse-region ${on ? "" : "collapse-region--closed"}`}
                  aria-hidden={!on || undefined}
                  inert={!on || undefined}
                >
                  <div className="collapse-region__inner">
                    <div className="tool-family-settings__forms">
                      <div className="tool-family-settings__row tool-family-settings__row--form">
                        <div className="tool-family-settings__row-copy">
                          <span className="tool-family-settings__row-title">
                            {variantDocs}
                            <strong>{t("加上决策模型参数", "Add decision-model parameters")}</strong>
                            <DecisionMark />
                          </span>
                          <small>{augmentDescription(entry.name, t)}</small>
                        </div>
                        <div className="tool-family-settings__row-control">
                          <Switch
                            checked={augment.checked}
                            disabled={augment.disabled}
                            onChange={augment.onChange}
                            label={t("{label}：加上决策模型参数", "{label}: add decision-model parameters", { label: descriptor.label })}
                          />
                        </div>
                      </div>
                      <div className="tool-family-settings__row tool-family-settings__row--form">
                        <div className="tool-family-settings__row-copy">
                          <span className="tool-family-settings__row-title">
                            {variantDocs}
                            <strong>{t("只用决策模型", "Decision model only")}</strong>
                            <DecisionMark />
                          </span>
                          <small>{replaceDescription(entry.name, t)}</small>
                        </div>
                        <div className="tool-family-settings__row-control">
                          <Switch
                            checked={replace.checked}
                            disabled={replace.disabled}
                            onChange={replace.onChange}
                            label={t("{label}：只用决策模型", "{label}: decision model only", { label: descriptor.label })}
                          />
                        </div>
                      </div>
                      {missScoring && (
                        <div className="tool-family-settings__row tool-family-settings__row--form">
                          <div className="tool-family-settings__row-copy">
                            <span className="tool-family-settings__row-title">
                              <strong>{t("未命中时逐个打分", "Score each element on a miss")}</strong>
                              <DecisionMark />
                            </span>
                            <small>{missScoringDescription(t)}</small>
                          </div>
                          <div className="tool-family-settings__row-control">
                            <Switch
                              checked={form !== null && scoring.includes(entry.name)}
                              disabled={form === null}
                              onChange={(checked) => setMissScoring(entry, checked)}
                              label={t("{label}：未命中时逐个打分", "{label}: score each element on a miss", { label: descriptor.label })}
                            />
                          </div>
                        </div>
                      )}
                      {formsLocked && <small className="tool-family-settings__hint">{formLockedHint(t)}</small>}
                    </div>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </section>
      {hasVariants && <p className="tool-family-settings__note">{family.decisionNote(t)}</p>}
    </div>
  );
}

/** The blue mark of an option that sends content to the decision model, beside the amber review mark's place. */
function DecisionMark() {
  const { t } = useI18n();
  return <em className="tool-family-settings__decision">{t("决策模型", "Decision model")}</em>;
}
