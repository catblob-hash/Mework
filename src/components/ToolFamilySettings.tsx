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
import { backendOfTool, SHELL_BACKENDS, shellBackendLabel } from "../lib/machineShells";
import { isPreviewToolName } from "../lib/taskTools";
import { type ToolListSettings, switchToolsOff, switchToolsOn } from "../lib/toolFamilies";
import type { ToolDescriptor } from "../types";
import { Dialog, Switch } from "./Common";
import { lockedInThisConversationHint } from "./ConversationSettingsPages";
import { ToolDocsLink } from "./DocsLink";
import { SettingsPageHeading } from "./SettingsPageHeading";
import "./ToolFamilySettings.css";

type Translate = ReturnType<typeof useI18n>["t"];

interface ToolFamilyPage {
  id: string;
  icon: LucideIcon;
  title: (t: Translate) => string;
  description: (t: Translate) => string;
  /** The page's rows, one tool each. */
  tools: readonly string[];
}

export type ToolFamilyId = "files" | "shell" | "preview";

/**
 * Tools that are one capability split into many calls for the model's benefit,
 * not the reader's. The picker shows each family as one row, and that row opens
 * the family's own settings window, where each tool is its own switch. The
 * stored list keeps the real names: nothing downstream of the picker has ever
 * heard of a family.
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
  /** Every catalog tool the family owns appears here exactly once, as a row. */
  pages: readonly ToolFamilyPage[];
}

/** The file tools the picker gathers. */
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
  pages: [
    {
      id: "files",
      icon: FileCode2,
      title: (t) => t("文件操作", "File operations"),
      description: (t) => t(
        "列出、查找、搜索、读取、写入和编辑工作区里的文件。",
        "List, find, search, read, write and edit the files in the workspace."
      ),
      tools: FILE_TOOL_NAMES
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
  pages: [
    {
      id: "shell",
      icon: SquareTerminal,
      title: () => "Shell",
      description: (t) => t(
        "在对话所用的机器上运行命令。这里只列出这些机器上找到的 shell。",
        "Run commands on the conversation's machines. Only the shells found on those machines are listed."
      ),
      tools: SHELL_BACKENDS
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
  pages: [
    {
      id: "servers",
      icon: Server,
      title: (t) => t("开发服务器管理", "Dev servers"),
      description: (t) => t(
        "启动、停止开发服务器并读取它的输出。服务器按 .mework/launch.json 里的配置启动。",
        "Start and stop dev servers and read their output. Servers start from the configurations in .mework/launch.json."
      ),
      tools: ["preview_start", "preview_stop", "preview_list", "preview_logs"]
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
        "preview_console_logs",
        "preview_screenshot",
        "preview_snapshot",
        "preview_inspect",
        "preview_network"
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
        "preview_click",
        "preview_fill",
        "preview_eval",
        "preview_resize",
        "preview_upload_image",
        "preview_dialog"
      ]
    }
  ]
};

/** The families in the order the picker would meet their first tools. */
export const TOOL_FAMILIES: readonly ToolFamily[] = [FILES_FAMILY, SHELL_FAMILY, PREVIEW_FAMILY];

/** The rows `family`'s window would draw from `available`, page after page. */
function familyEntries(family: ToolFamily, available: ReadonlySet<string>): string[] {
  return family.pages.flatMap((page) => page.tools).filter((name) => available.has(name));
}

/** The rows of `family`'s window that are on. */
export function familyRowsOn(
  family: ToolFamily,
  available: ReadonlySet<string>,
  isOn: (name: string) => boolean
): string[] {
  return familyEntries(family, available).filter(isOn);
}

/** How many of the rows `family`'s window would draw are on, out of how many it would draw. */
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

function toolDescription(name: string, t: Translate): string {
  const backend = backendOfTool(name);
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

interface ToolFamilySettingsProps {
  family: ToolFamily;
  /** The family's tools as this owner's catalog has them; a row whose tool is missing is not drawn. */
  tools: readonly ToolDescriptor[];
  enabledTools: readonly string[];
  /** Tools a run has already put in front of the model; they cannot be switched off. */
  lockedTools: readonly string[];
  onChange: (next: ToolListSettings) => void;
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
 * end.
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
  onChange,
  notice
}: ToolFamilySettingsProps & { page: string }) {
  const { t } = useI18n();
  const page = family.pages.find((item) => item.id === pageId) ?? family.pages[0];
  const descriptors = new Map(tools.map((tool) => [tool.name, tool]));
  const locked = new Set(lockedTools);
  const isOn = (name: string) => enabledTools.includes(name) || locked.has(name);
  const entries = page.tools.filter((name) => descriptors.has(name));
  const settings: ToolListSettings = { enabledTools: [...enabledTools] };

  function setOn(name: string, on: boolean) {
    onChange(on ? switchToolsOn(settings, [name]) : switchToolsOff(settings, new Set([name])));
  }

  return (
    <div className="settings-page tool-family-settings__page">
      <SettingsPageHeading title={page.title(t)} description={page.description(t)} />
      {notice && <p className="tool-family-settings__notice" role="note">{notice}</p>}
      <section className="settings-card tool-family-settings__card">
        {entries.map((name) => {
          const descriptor = descriptors.get(name)!;
          const on = isOn(name);
          const rowLocked = locked.has(name);
          return (
            <div className="tool-family-settings__row" key={name} data-tool-name={name}>
              <div className="tool-family-settings__row-copy">
                <span className="tool-family-settings__row-title">
                  <ToolDocsLink name={name} label={descriptor.label} />
                  <strong>{descriptor.label}</strong>
                  <code>{name}</code>
                  {descriptor.dangerous && <em>{t("需审查", "Reviewed")}</em>}
                </span>
                <small>{toolDescription(name, t)}</small>
                {rowLocked && <small className="tool-family-settings__hint">{lockedInThisConversationHint(t)}</small>}
              </div>
              <div className="tool-family-settings__row-control">
                <Switch
                  checked={on}
                  disabled={rowLocked}
                  onChange={(checked) => setOn(name, checked)}
                  label={on
                    ? t("{label}已启用", "{label} enabled", { label: descriptor.label })
                    : t("{label}已关闭", "{label} disabled", { label: descriptor.label })}
                />
              </div>
            </div>
          );
        })}
      </section>
    </div>
  );
}
