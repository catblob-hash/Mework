import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { defaultConversationWebSearchSettings } from "../lib/runtime";
import {
  DEFAULT_SEARCH_COMPRESSION_CUTOFF,
  DEFAULT_SEARCH_MAX_RESULTS
} from "../lib/searchProviders";
import { isHostDerivedToolName, isPreviewLifecycleToolName } from "../lib/taskTools";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { BUILTIN_PRESET_ID } from "../seed";
import type {
  CapabilityCatalog,
  ContextItem,
  Conversation,
  ConversationPresetSettings,
  ConversationSettings as ConversationSettingsType,
  ConversationTemplateSummary,
  GlobalSettings,
  McpProbeReport,
  ResourceDescriptor,
  ToolDescriptor
} from "../types";

import { ConversationSettings } from "./ConversationSettings";

/* Template bodies live in the host's store, never in the document, so the pane
   only ever reaches them through these two. Module-level so a rerender does not
   hand the pane a new identity and re-read the body under the editor. */
const readNothing = async (): Promise<ContextItem[]> => [];
const writeSomewhere = async (templateId: string): Promise<string> => templateId || "template_minted";

/* The two web-backend pickers are menus rather than `<select>`s, because the
   native row opens a second step naming the wire tool version and an option
   list cannot hold one. Drive them the way a user does: open, then click. */
function backendTrigger(field: string): HTMLElement {
  return screen.getByRole("button", { name: new RegExp(`^${field}：`) });
}

async function chooseBackend(
  user: ReturnType<typeof userEvent.setup>,
  field: string,
  row: string | RegExp
): Promise<void> {
  await user.click(backendTrigger(field));
  await user.click(
    within(screen.getByRole("menu", { name: field })).getByRole("menuitemradio", { name: row })
  );
}

function SettingsHarness({
  initialConversation,
  globalSettings,
  tools,
  capabilities,
  onSettingsChange,
  onConversationOnlyChange = vi.fn(),
  onApplyPreset = vi.fn(),
  onRenamePreset = vi.fn(),
  onDeletePreset = vi.fn(),
  onSavePreset = vi.fn(),
  onSavePresetCopy = vi.fn(),
  onSaveAsPreset = vi.fn(),
  onBindPresetTemplate = vi.fn(),
  templates = [],
  onReadTemplate = readNothing,
  onWriteTemplate = writeSomewhere,
  onDeleteCapability,
  workspaceId,
  onRescanCapabilities,
  onRevealCapabilityLocation,
  onProbeMcpServer
}: {
  initialConversation: Conversation;
  globalSettings: GlobalSettings;
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
  onSettingsChange: (settings: ConversationSettingsType) => void;
  onConversationOnlyChange?: (patch: Partial<ConversationSettingsType>) => void;
  onApplyPreset?: (presetId: string) => void;
  onRenamePreset?: (presetId: string, name: string) => void;
  onDeletePreset?: (presetId: string) => void;
  onSavePreset?: (presetId: string, settings: ConversationPresetSettings) => void;
  onSavePresetCopy?: (presetId: string, settings: ConversationPresetSettings) => void;
  onSaveAsPreset?: () => void;
  onBindPresetTemplate?: (presetId: string, templateId: string) => void;
  templates?: ConversationTemplateSummary[];
  onReadTemplate?: (templateId: string) => Promise<ContextItem[]>;
  onWriteTemplate?: (templateId: string, contexts: ContextItem[]) => Promise<string>;
  onDeleteCapability?: (kind: "skills" | "mcp" | "hooks", resource: ResourceDescriptor) => void;
  /** The conversation's workspace. Undefined is "no prop": no narrowing at all. */
  workspaceId?: string | null;
  onRescanCapabilities?: () => void | Promise<void>;
  onRevealCapabilityLocation?: (
    kind: "skills" | "mcp" | "hooks",
    workspaceId: string | null
  ) => void;
  onProbeMcpServer?: (resource: ResourceDescriptor) => Promise<McpProbeReport>;
}) {
  const [conversation, setConversation] = useState(initialConversation);
  return (
    <ConversationSettings
      conversation={conversation}
      globalSettings={globalSettings}
      tools={tools}
      capabilities={capabilities}
      onChange={(settings) => {
        onSettingsChange(settings);
        setConversation((current) => ({ ...current, settings }));
      }}
      onChangeConversationOnly={(patch) => {
        onConversationOnlyChange(patch);
        setConversation((current) => ({
          ...current,
          settings: { ...current.settings, ...patch }
        }));
      }}
      onApplyPreset={onApplyPreset}
      onRenamePreset={onRenamePreset}
      onDeletePreset={onDeletePreset}
      onSavePreset={onSavePreset}
      onSavePresetCopy={onSavePresetCopy}
      onSaveAsPreset={onSaveAsPreset}
      onBindPresetTemplate={onBindPresetTemplate}
      templates={templates}
      onReadTemplate={onReadTemplate}
      onWriteTemplate={onWriteTemplate}
      onDeleteCapability={onDeleteCapability}
      workspaceId={workspaceId}
      onRescanCapabilities={onRescanCapabilities}
      onRevealCapabilityLocation={onRevealCapabilityLocation}
      onProbeMcpServer={onProbeMcpServer}
    />
  );
}

/** Every page is reached from the list on the left, which is the pane's only navigation. */
function navigation(): HTMLElement {
  // Matched by either label so the helper serves the English tests too.
  return screen.getByRole("navigation", {
    name: /对话设置分类|Conversation settings categories/
  });
}

async function openPage(user: ReturnType<typeof userEvent.setup>, name: RegExp): Promise<void> {
  await user.click(within(navigation()).getByRole("button", { name }));
}

/** A stored template, as the pane sees it: a name, and how long its body is. */
function template(
  id: string,
  name: string,
  messageCount: number
): ConversationTemplateSummary {
  return {
    id,
    name,
    messageCount,
    createdAt: "2026-01-02T03:04:05.000Z",
    updatedAt: "2026-01-03T03:04:05.000Z"
  };
}

describe("ConversationSettings", () => {
  afterEach(() => configureI18n("zh-CN"));

  it("lists seven pages and opens on the features page", () => {
    const seed = createSeedDocument();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    // The order is fixed and not data-sorted: the tool surface first, then one
    // page per catalog of named things the conversation composes with, with the
    // save-as-preset entry point closing the list.
    const pages = within(navigation()).getAllByRole("button")
      .map((button) => (button.textContent ?? "").replace(/\d+$/, ""));
    expect(pages).toEqual(["功能", "沙箱", "技能", "MCP", "钩子", "代理角色", "对话预设", "另存为预设"]);
    // A live conversation's own timeline IS its message queue, editable in
    // place, so the template page belongs to a preset and to a role's window.
    expect(within(navigation()).queryByRole("button", { name: /^对话模板/ })).toBeNull();
    const active = within(navigation()).getByRole("button", { name: /^功能/ });
    expect(active).toHaveAttribute("aria-current", "true");
    // A row is `[category icon] [label] [optional count]`. Selection is the
    // active class alone, so the trailing chevron is gone and the row's one svg
    // is its leading icon — a stray second svg here would mean a stylesheet rule
    // targeting `svg:last-child` had found something to hide instead.
    expect(active.querySelectorAll("svg")).toHaveLength(1);
    // Nothing is a disclosure any more, so the catalogs are not rendered until visited.
    expect(screen.queryByRole("switch", { name: /代码审查/ })).toBeNull();
  });

  it("keeps the sandbox with the conversation, as a page of its own", async () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await user.click(within(navigation()).getByRole("button", { name: /^沙箱/ }));
    const toggle = screen.getByRole("switch", { name: "在沙箱中运行命令" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await user.click(toggle);
    // A preset component, written with the rest of the conversation's body.
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      sandbox: expect.objectContaining({ enabled: true, network: expect.objectContaining({ mode: "allowlist" }) })
    }));
  });

  it("keeps the save-as-preset entry point as the last thing in the nav", async () => {
    const seed = createSeedDocument();
    const onSaveAsPreset = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onSaveAsPreset={onSaveAsPreset}
      />
    );

    const nav = navigation();
    const saveAs = within(nav).getByRole("button", { name: "另存为预设" });
    // Saving is the one preset action that belongs to the whole page rather than
    // to a row, so it is the sole thing under the list — and the picker that used
    // to sit there is gone.
    expect(saveAs.parentElement).toHaveClass("conversation-settings__preset-actions");
    expect(nav.lastElementChild).toHaveClass("conversation-settings__nav-footer");
    expect(saveAs.parentElement?.parentElement).toBe(nav.lastElementChild);
    expect(within(nav).getAllByRole("button").at(-1)).toBe(saveAs);
    expect(within(nav).queryByRole("combobox", { name: "套用预设" })).toBeNull();

    await user.click(saveAs);
    expect(onSaveAsPreset).toHaveBeenCalledTimes(1);
  });

  it("applies a preset, names the one a conversation carries, and forgets a deleted one", async () => {
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets.push({
      ...seed.globalSettings.conversationPresets[0],
      id: "conversation_second",
      name: "第二预设"
    });
    const conversation = seed.workspaces[0].conversations[0];
    const onApplyPreset = vi.fn();
    const user = userEvent.setup();
    const first = render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onApplyPreset={onApplyPreset}
      />
    );

    await openPage(user, /^对话预设/);
    // Applying stays an action: no "following" banner and nothing to detach from.
    expect(screen.queryByText(/Following:/)).toBeNull();
    expect(screen.queryByRole("button", { name: "停止跟随" })).toBeNull();

    const second = screen.getByRole("button", { name: "打开预设 第二预设" })
      .closest(".catalog-row") as HTMLElement;
    await user.click(within(second).getByRole("button", { name: "套用" }));
    expect(onApplyPreset).toHaveBeenLastCalledWith("conversation_second");
    first.unmount();

    const carried = render(
      <SettingsHarness
        initialConversation={{ ...conversation, presetId: "conversation_second", templateId: "" }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );
    await openPage(user, /^对话预设/);
    expect(screen.getByRole("button", { name: "打开预设 第二预设" })
      .closest(".catalog-row")).toHaveClass("catalog-row--on");
    expect(screen.getByRole("button", { name: "打开预设 默认" })
      .closest(".catalog-row")).not.toHaveClass("catalog-row--on");
    carried.unmount();

    render(
      <SettingsHarness
        initialConversation={{ ...conversation, presetId: "conversation_deleted", templateId: "" }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );
    await openPage(user, /^对话预设/);
    // A preset the conversation still names but that is gone leaves no row marked.
    expect(document.querySelectorAll(".catalog-row--on")).toHaveLength(0);
  });

  it("shows a localized fallback for a blank persisted preset name", async () => {
    configureI18n("en-US");
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets[0].name = "";
    const user = userEvent.setup();

    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    await openPage(user, /^Conversation presets/);
    // The fallback is display-only: the blank name stays blank on disk.
    expect(screen.getByText("Untitled preset")).toBeInTheDocument();
    expect(seed.globalSettings.conversationPresets[0].name).toBe("");
  });

  it("renders English controls and offers no rename or delete for the built-in preset", async () => {
    configureI18n("en-US");
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets.unshift({
      ...seed.globalSettings.conversationPresets[0],
      id: BUILTIN_PRESET_ID,
      name: "mework"
    });
    const user = userEvent.setup();

    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    expect(screen.getByRole("link", { name: "Configuration docs" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Conversation settings categories" }))
      .toBeInTheDocument();

    await openPage(user, /^Conversation presets/);
    // The built-in ships with the build, so renaming or deleting it is offered
    // but spent; a preset of the user's own keeps both.
    const builtin = screen.getByRole("button", { name: /^Open preset mework$/ })
      .closest(".catalog-row") as HTMLElement;
    expect(within(builtin).getByRole("button", { name: "Rename" })).toBeDisabled();
    expect(within(builtin).getByRole("button", { name: "Delete preset mework" })).toBeDisabled();
    const own = screen.getByRole("button", { name: /^Open preset 默认$/ })
      .closest(".catalog-row") as HTMLElement;
    expect(within(own).getByRole("button", { name: "Rename" })).toBeEnabled();
  });

  it("saves an edited built-in preset as a new preset, never in place", async () => {
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets.unshift({
      ...seed.globalSettings.conversationPresets[0],
      id: BUILTIN_PRESET_ID,
      name: "mework",
      templateId: "template_preset_mework"
    });
    const onSavePreset = vi.fn();
    const onSavePresetCopy = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onSavePreset={onSavePreset}
        onSavePresetCopy={onSavePresetCopy}
      />
    );

    await openPage(user, /^对话预设/);
    await user.click(screen.getByRole("button", { name: "打开预设 mework" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText(/内置预设随 Mework 版本更新/)).toBeInTheDocument();
    const nestedNav = within(dialog).getByRole("navigation", { name: "对话设置分类" });
    expect(within(nestedNav).queryByRole("button", { name: "保存预设" })).toBeNull();
    await user.click(within(nestedNav).getByRole("button", { name: "另存为新预设" }));
    expect(onSavePresetCopy).toHaveBeenCalledWith(BUILTIN_PRESET_ID, expect.objectContaining({
      enabledTools: seed.globalSettings.conversationPresets[0].settings.enabledTools
    }));
    expect(onSavePreset).not.toHaveBeenCalled();
  });

  it("is a pane, not a dialog, and offers no preset-managed or conversation-only zone", () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    render(
      <SettingsHarness
        initialConversation={{ ...seed.workspaces[0].conversations[0] }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    // The pane's own chrome is the side pane's title bar, so the component draws
    // no modal of its own and nothing closes it from the inside.
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("button", { name: "关闭" })).toBeNull();
    // Nor does it name itself or its page, as a window does.
    expect(document.querySelector(".dialog__sidebar-title, .conversation-settings__page-title")).toBeNull();
    // A preset is not conversation state, so no preset-managed or conversation-only sections appear.
    expect(screen.queryByText("由预设管理")).toBeNull();
    expect(screen.queryByText("仅此对话")).toBeNull();
    // The system prompt is a timeline card now, not a setting.
    expect(screen.queryByRole("textbox", { name: "系统提示词" })).toBeNull();
    expect(onSettingsChange).not.toHaveBeenCalled();
  });

  it("counts only catalog tools, links the docs, and exposes group expansion state", async () => {
    const seed = createSeedDocument();
    // Memory tools derive from memory-tier switches and are excluded from the
    // count, as are the preview lifecycle tools, which follow the other preview tools.
    const toolNames = Array.from(new Set(
      seed.tools
        .filter((tool) => tool.category !== "memory"
          && !isHostDerivedToolName(tool.name)
          && !isPreviewLifecycleToolName(tool.name))
        .map((tool) => tool.name)
    ));
    const conversation = {
      ...seed.workspaces[0].conversations[0],
      settings: {
        ...seed.workspaces[0].conversations[0].settings,
        enabledTools: [toolNames[0], "missing-tool"]
      }
    };
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    // Bulk enable/disable is gone: the heading ends with the documentation link,
    // which is a bare anchor rather than a control wrapped in a container.
    expect(screen.queryByRole("button", { name: "全部启用" })).toBeNull();
    expect(screen.queryByRole("button", { name: "全部关闭" })).toBeNull();
    const docs = screen.getByRole("link", { name: "配置说明文档" });
    expect(docs).toHaveAttribute("href", "https://catblob-hash.github.io/Mework/zh-CN/working.html");
    expect(docs).toHaveAttribute("target", "_blank");
    expect(docs.parentElement).toHaveClass("settings-section__heading--split");
    expect(screen.getByText(`1 / ${toolNames.length} 个已选`)).toBeInTheDocument();

    // Toggling a tool must not collapse groups; disclosure state changes only on user action.
    const filesystemGroup = screen.getByRole("button", { name: "文件与搜索" });
    expect(filesystemGroup).toHaveAttribute("aria-expanded", "true");
    await user.click(filesystemGroup);
    expect(filesystemGroup).toHaveAttribute("aria-expanded", "false");
  });

  it("keeps the web, memory, app-data and tool-description controls on the features page", async () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    expect(backendTrigger("搜索提供商")).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "工具描述" })).toBeInTheDocument();
    // Executor limits and the web-search heading stay gone; the identically named tool row remains.
    expect(screen.queryByRole("spinbutton", { name: /搜索次数/ })).toBeNull();
    // Security policy is configured in the composer, and memory tools derive from the tier switches.
    expect(screen.queryByText("安全")).toBeNull();
    // Memory tools leave the picker along with the tier switches that derive them.
    // A picker row is a button keyed by `data-tool-name` — its accessible name is
    // "{label}已启用/已关闭" and it carries `aria-pressed`, not the switch role — so
    // a role query would stay green no matter what the picker drew. The positive
    // half is what keeps that true: if picker rows ever stop carrying the
    // attribute, this goes red instead of the guard quietly becoming a no-op.
    expect(document.querySelector('[data-tool-name="read"]')).not.toBeNull();
    expect(document.querySelector('[data-tool-name="read_global_memory"]')).toBeNull();
    // The credential channels are gone, not merely off by default.
    expect(screen.queryByText("允许联网搜索使用我的登录凭证")).toBeNull();
    expect(screen.queryByText("允许 Agent 使用内置浏览器的 Cookie")).toBeNull();

    // Tier switches are preset components and use `onChange`, not conversation-only patches.
    await user.click(screen.getByRole("switch", { name: "全局记忆已关闭" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ globalMemoryEnabled: true, projectMemoryEnabled: false })
    );
    expect(onConversationOnlyChange).not.toHaveBeenCalled();
    await user.click(screen.getByRole("switch", { name: "项目记忆已关闭" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ globalMemoryEnabled: true, projectMemoryEnabled: true })
    );
  });

  it("controls whether the application-data path is appended to the system prompt", async () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    const toggle = screen.getByRole("switch", { name: "拼接应用数据目录已关闭" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await user.click(toggle);
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({ includeAppDataPath: true });
    expect(onSettingsChange).not.toHaveBeenCalled();
    expect(screen.getByRole("switch", { name: "拼接应用数据目录已开启" })).toHaveAttribute("aria-checked", "true");
  });

  it("edits search provider behavior as a conversation-only patch", async () => {
    const seed = createSeedDocument();
    const defaults = defaultConversationWebSearchSettings();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={{
          ...seed.globalSettings,
          webSearch: {
            ...seed.globalSettings.webSearch,
            providers: seed.globalSettings.webSearch.providers.map((provider) => (
              provider.kind === "tavily" ? { ...provider, enabled: true } : provider
            ))
          }
        }}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await chooseBackend(user, "搜索提供商", /^Tavily/);
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: { ...defaults, provider: { kind: "explicit", providerKind: "tavily" } }
    });
    expect(onSettingsChange).not.toHaveBeenCalled();
  });

  /* The native row's second step: which version of the Messages server-side
     tool this conversation sends. It is a step under the row rather than a
     sibling of it because it refines a choice already made — you cannot pick a
     `web_search_*` spelling without having picked native first. */
  it("opens the native rows into the Messages tool versions on a Messages model", async () => {
    const seed = createSeedDocument();
    const defaults = defaultConversationWebSearchSettings();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={{ ...seed.globalSettings, activeProviderId: "anthropic_messages" }}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await user.click(backendTrigger("搜索提供商"));
    const searchMenu = screen.getByRole("menu", { name: "搜索提供商" });
    const nativeSearch = within(searchMenu).getByRole("menuitemradio", { name: /^原生/ });
    expect(nativeSearch).toHaveAttribute("aria-haspopup", "menu");
    // Clicking the row only opens the step; the versions are the choices.
    await user.click(nativeSearch);
    expect(onConversationOnlyChange).not.toHaveBeenCalled();
    const versions = within(searchMenu).getByRole("menu", { name: /^原生/ });
    // The list is the wire `type` verbatim — the request carries no other name
    // for these, so neither should the menu.
    expect(within(versions).getAllByRole("menuitemradio").map((row) => row.textContent))
      .toEqual(["web_search_20250305", "web_search_20260209"]);
    await user.click(within(versions).getByRole("menuitemradio", { name: "web_search_20260209" }));
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: {
        ...defaults,
        provider: { kind: "native" },
        nativeSearchTool: "web_search_20260209"
      }
    });

    // The fetch leg has its own versions, because it is its own server tool.
    await user.click(backendTrigger("抓取提供商"));
    const fetchMenu = screen.getByRole("menu", { name: "抓取提供商" });
    await user.click(within(fetchMenu).getByRole("menuitemradio", { name: /^原生/ }));
    const fetchVersions = within(fetchMenu).getByRole("menu", { name: /^原生/ });
    expect(within(fetchVersions).getAllByRole("menuitemradio").map((row) => row.textContent))
      .toEqual(["web_fetch_20250910", "web_fetch_20260209"]);
    await user.click(
      within(fetchVersions).getByRole("menuitemradio", { name: "web_fetch_20260209" })
    );
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: {
        ...defaults,
        provider: { kind: "native" },
        nativeSearchTool: "web_search_20260209",
        fetchProvider: { kind: "native" },
        nativeFetchTool: "web_fetch_20260209"
      }
    });
  });

  /* Moving to a model on another protocol is a silent fall back to plain
     native: the row selects immediately, nothing warns, and the versions the
     conversation is carrying are neither shown nor rewritten — so the next
     Messages model it runs on finds them exactly as they were left. */
  it("falls back to plain native off Messages and keeps the chosen versions", async () => {
    const seed = createSeedDocument();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    const conversation = seed.workspaces[0].conversations[0];
    const carried = {
      ...defaultConversationWebSearchSettings(),
      provider: { kind: "explicit" as const, providerKind: "tavily" as const },
      nativeSearchTool: "web_search_20260209" as const,
      nativeFetchTool: "web_fetch_20260209" as const
    };
    const { unmount } = render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: { ...conversation.settings, webSearch: carried }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await user.click(backendTrigger("搜索提供商"));
    const menu = screen.getByRole("menu", { name: "搜索提供商" });
    const native = within(menu).getByRole("menuitemradio", { name: /^原生/ });
    expect(native).not.toHaveAttribute("aria-haspopup");
    await user.click(native);
    // The patch moves the backend and leaves both versions where they were.
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: { ...carried, provider: { kind: "native" } }
    });
    // Nor are they claimed on the trigger, which would promise an effect this
    // model's requests do not have.
    expect(backendTrigger("搜索提供商")).not.toHaveTextContent("web_search_20260209");

    unmount();
    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            webSearch: { ...carried, provider: { kind: "native" } }
          }
        }}
        globalSettings={{ ...seed.globalSettings, activeProviderId: "anthropic_messages" }}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onConversationOnlyChange={vi.fn()}
      />
    );

    // Back on a Messages model, the version it left with is the one it has.
    expect(backendTrigger("搜索提供商")).toHaveTextContent("web_search_20260209");
  });

  it("offers the model's own provider as a fetch backend and pins both backends once they have been used", async () => {    const seed = createSeedDocument();
    const defaults = defaultConversationWebSearchSettings();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    const conversation = seed.workspaces[0].conversations[0];
    const { unmount } = render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    /* Drawn whatever the search backend is: fetching and searching are separate
       capabilities, so "the model's own provider" is an answer here too — one
       that grants a second web tool on Anthropic and none on OpenAI. */
    await chooseBackend(user, "抓取提供商", /^原生/);
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: { ...defaults, fetchProvider: { kind: "native" } }
    });

    unmount();
    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            toolLock: {
              tools: [],
              mcpIds: [],
              globalMemory: false,
              projectMemory: false,
              skillTool: false,
              mcpToolDiscovery: false,
              webSearch: true,
              skillIds: [],
              promptSkillIds: [],
              searchProvider: { kind: "native" },
              fetchProvider: { kind: "native" }
            }
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onConversationOnlyChange={vi.fn()}
      />
    );

    // A transcript holds results only the backend that produced them can be
    // replayed against, so neither selector moves once its tool has gone out.
    expect(backendTrigger("搜索提供商")).toBeDisabled();
    expect(backendTrigger("抓取提供商")).toBeDisabled();
  });

  it("picks one discovered tool-description file rather than editing entries", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    const picker = screen.getByRole("combobox", { name: "工具描述" });
    // Sniffed from disk and selected, exactly like skills and MCP: the app
    // shows what it found and never offers a place to author entries.
    const section = picker.closest("section") as HTMLElement;
    expect(within(section).queryByRole("textbox")).toBeNull();
    // The conversation selects nothing, which the host renders with the
    // built-in; the picker says so instead of showing an empty selection.
    expect(picker).toHaveValue("tooldesc_builtin_en_us");
    expect(within(picker).getByRole("option", { name: "Mework 内置" })).toBeInTheDocument();
    expect(within(picker).getAllByRole("option")).toHaveLength(2);

    await user.selectOptions(picker, "tooldesc_user_main_0f0f0f0f");
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      toolDescriptionFileId: "tooldesc_user_main_0f0f0f0f"
    }));
  });

  it("never shows host-derived tools or a skill tool in the tool picker", () => {
    const seed = createSeedDocument();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    // `skill` is cataloged for timeline rendering but derives from its switch, so
    // the picker must not draw a row for it. A row is a button keyed by
    // `data-tool-name` whose accessible name is "{label}已启用/已关闭" — neither a
    // switch nor the bare name — so only the attribute can catch a regression.
    expect(seed.tools.some((tool) => tool.name === "skill")).toBe(true);
    expect(document.querySelector('[data-tool-name="read"]')).not.toBeNull();
    expect(document.querySelector('[data-tool-name="skill"]')).toBeNull();
    expect(screen.queryByRole("button", { name: "长期记忆" })).toBeNull();
  });

  it("selects skills, MCP servers, and hooks on their own pages, one catalog per page", async () => {
    const seed = createSeedDocument();
    seed.capabilities.hooks.push({
      id: "hook_lint",
      name: "Lint 钩子",
      description: "测试用钩子。",
      location: "test://hooks/lint.json#/hooks/PreToolUse/0/hooks/0",
      source: "user",
      available: true
    });
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await openPage(user, /^技能/);
    const codeReview = screen.getByRole("switch", { name: /代码审查/ });
    expect(codeReview).toBeChecked();
    // A row names where the resource came from, which is the only way to tell
    // two same-named resources apart — now in the row's tooltip rather than a
    // second line.
    expect(codeReview.closest(".catalog-row")).toHaveAttribute(
      "title", expect.stringContaining("test://skills/code-review/SKILL.md")
    );
    await user.click(codeReview);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: [],
      mcpIds: ["mcp_workspace"],
      hookIds: []
    }));

    await openPage(user, /^MCP/);
    expect(screen.getByRole("switch", { name: /Workspace Files/ })).toBeChecked();
    await user.click(screen.getByRole("switch", { name: /Workspace Files/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({ mcpIds: [] }));

    await openPage(user, /^钩子/);
    // A hook addresses one handler inside a file; the file is what the row's tooltip shows.
    const lintHook = screen.getByRole("switch", { name: /Lint 钩子/ });
    expect(lintHook.closest(".catalog-row"))
      .toHaveAttribute("title", expect.stringContaining("test://hooks/lint.json"));
    await user.click(lintHook);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      hookIds: ["hook_lint"]
    }));

    // Scanning is automatic: no list offers a manual trigger, and the old
    // "no hook preset" group layer is gone.
    expect(screen.queryByRole("button", { name: "重新扫描" })).toBeNull();
    expect(screen.queryByText("不使用钩子预设")).toBeNull();
  });

  it("searches a catalog by name, description and path, and toggles what the search shows", async () => {
    const seed = createSeedDocument();
    seed.capabilities.skills.push({
      id: "skill_release",
      name: "发布流程",
      description: "测试用技能。",
      location: "test://skills/release/SKILL.md",
      source: "workspace",
      available: true
    });
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await openPage(user, /^技能/);
    expect(screen.getByText("1 / 2 个已选")).toBeInTheDocument();

    await user.type(screen.getByRole("textbox", { name: "搜索技能" }), "release");
    expect(screen.queryByRole("switch", { name: /代码审查/ })).toBeNull();
    // A row the search hides keeps its selection; only the visible one is edited.
    await user.click(screen.getByRole("switch", { name: /发布流程/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: ["skill_code_review", "skill_release"]
    }));

    await user.clear(screen.getByRole("textbox", { name: "搜索技能" }));
    await user.type(screen.getByRole("textbox", { name: "搜索技能" }), "没有这个");
    expect(screen.getByText("没有匹配的条目")).toBeInTheDocument();
  });

  it("keeps a selected capability that has vanished from the catalog instead of dropping it", async () => {
    const seed = createSeedDocument();
    const conversation = {
      ...seed.workspaces[0].conversations[0],
      settings: {
        ...seed.workspaces[0].conversations[0].settings,
        skillIds: ["skill_code_review", "skill_uninstalled"]
      }
    };
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    // A resource may just be temporarily unscanned, so the selection survives
    // as a checked dangling row the user can clear on purpose.
    await openPage(user, /^技能/);
    const dangling = screen.getByRole("switch", { name: /skill_uninstalled/ });
    expect(dangling).toBeChecked();
    expect(screen.getByText("悬空")).toBeInTheDocument();
    // The counter is over the catalog, which the dangling row is no longer part of:
    // counting it would read "2 / 1".
    expect(screen.getByText("1 / 1 个已选")).toBeInTheDocument();
    await user.click(dangling);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: ["skill_code_review"]
    }));
  });

  it("draws a skill a run has already delivered as spent and freezes how skills arrive", async () => {
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            skillIds: ["skill_code_review"],
            toolLock: {
              tools: [],
              mcpIds: [],
              globalMemory: false,
              projectMemory: false,
              skillTool: false,
              mcpToolDiscovery: false,
              webSearch: false,
              skillIds: ["skill_code_review"],
              promptSkillIds: ["skill_code_review"],
              searchProvider: null,
              fetchProvider: null
            }
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    await openPage(user, /^技能/);
    // The body is already somewhere in the transcript; unchecking the row would
    // not take it back.
    const locked = screen.getByRole("switch", { name: /代码审查/ });
    expect(locked).toBeChecked();
    expect(locked).toBeDisabled();
    // And the route it took is settled too: moving it behind the tool now would
    // point at a tool the earlier rounds never had.
    expect(screen.getByRole("switch", { name: "拼进提示词" })).toBeDisabled();
  });

  it("draws an MCP selection a run has already exposed as spent rather than removable", async () => {
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            toolLock: {
              tools: [],
              mcpIds: ["mcp_workspace"],
              globalMemory: false,
              projectMemory: false,
              skillTool: false,
              mcpToolDiscovery: false,
              webSearch: false,
              skillIds: [],
              promptSkillIds: [],
              searchProvider: null,
              fetchProvider: null
            }
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    await openPage(user, /^MCP/);
    const locked = screen.getByRole("switch", { name: /Workspace Files/ });
    expect(locked).toBeChecked();
    expect(locked).toBeDisabled();
    // The reason it is inert joined the row's tooltip when its second line went away.
    expect(locked.closest(".catalog-row")).toHaveAttribute(
      "title", expect.stringContaining("已经交给过模型，本对话里不能再移除")
    );
  });

  it("offers the skill delivery switch below the list whether or not a skill is selected", async () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    const withoutSkills = {
      ...seed.workspaces[0].conversations[0],
      settings: { ...seed.workspaces[0].conversations[0].settings, skillIds: [] }
    };
    const { unmount } = render(
      <SettingsHarness
        initialConversation={withoutSkills}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await openPage(user, /^技能/);
    /* How a skill would arrive is worth knowing before installing one, so the
       policy is drawn with an empty selection too — and it reads after the list
       it qualifies rather than above it. */
    const policy = screen.getByRole("switch", { name: /拼进提示词|按需加载/ });
    const list = policy.closest(".conversation-settings__page-stack")
      ?.querySelector(".catalog-list");
    expect(list).not.toBeNull();
    expect(list!.compareDocumentPosition(policy) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBeTruthy();

    // Remount rather than rerender because the harness retains the conversation in local state.
    unmount();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await openPage(user, /^技能/);
    await user.click(screen.getByRole("switch", { name: "拼进提示词" }));
    // This is a preset component and must use `onChange`.
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ skillToolEnabled: true })
    );
    expect(onConversationOnlyChange).not.toHaveBeenCalled();
  });

  it("offers the MCP tool-discovery switch below the list and freezes it once a server has run", async () => {
    const seed = createSeedDocument();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    const withoutServers = {
      ...seed.workspaces[0].conversations[0],
      settings: { ...seed.workspaces[0].conversations[0].settings, mcpIds: [] }
    };
    const { unmount } = render(
      <SettingsHarness
        initialConversation={withoutServers}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
        onConversationOnlyChange={onConversationOnlyChange}
      />
    );

    await openPage(user, /^MCP/);
    /* What a server's tools would cost per request is worth knowing before
       adding the first one, so the policy is drawn with an empty selection too
       — and it reads after the list it qualifies. */
    const policy = screen.getByRole("switch", { name: /全部声明|按需取回/ });
    const list = policy.closest(".conversation-settings__page-stack")
      ?.querySelector(".catalog-list");
    expect(list).not.toBeNull();
    expect(list!.compareDocumentPosition(policy) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBeTruthy();
    await user.click(policy);
    // A preset component, so it goes through `onChange` like skill delivery.
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ mcpToolDiscoveryEnabled: true })
    );
    expect(onConversationOnlyChange).not.toHaveBeenCalled();

    // Once a run has dialed a server, how its tools arrived is settled: the
    // transcript carries them announced or declared, and neither can be redone.
    unmount();
    render(
      <SettingsHarness
        initialConversation={{
          ...seed.workspaces[0].conversations[0],
          settings: {
            ...seed.workspaces[0].conversations[0].settings,
            mcpToolDiscoveryEnabled: true,
            toolLock: {
              tools: [],
              mcpIds: ["mcp_workspace"],
              globalMemory: false,
              projectMemory: false,
              skillTool: false,
              mcpToolDiscovery: true,
              webSearch: false,
              skillIds: [],
              promptSkillIds: [],
              searchProvider: null,
              fetchProvider: null
            }
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );
    await openPage(user, /^MCP/);
    const settled = screen.getByRole("switch", { name: "按需取回" });
    expect(settled).toBeChecked();
    expect(settled).toBeDisabled();
  });

  it("ends every capability page with its own documentation link and nothing else", async () => {
    const seed = createSeedDocument();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    // The jump to a global settings page went away with the pages themselves;
    // each catalog now points at the page documenting how it is configured.
    await openPage(user, /^MCP/);
    expect(screen.queryByRole("button", { name: "管理 MCP" })).toBeNull();
    expect(screen.getByRole("link", { name: "配置说明文档" }))
      .toHaveAttribute("href", "https://catblob-hash.github.io/Mework/zh-CN/mcp.html");

    await openPage(user, /^技能/);
    expect(screen.queryByRole("button", { name: "管理技能" })).toBeNull();
    expect(screen.getByRole("link", { name: "配置说明文档" }))
      .toHaveAttribute("href", "https://catblob-hash.github.io/Mework/zh-CN/skills.html");

    await openPage(user, /^钩子/);
    const hooksDocs = screen.getByRole("link", { name: "配置说明文档" });
    expect(hooksDocs).toHaveAttribute("href", "https://catblob-hash.github.io/Mework/zh-CN/hooks.html");
    expect(hooksDocs.parentElement).toHaveClass("capability-page__toolbar");
  });

  it("lists this conversation's roles by name and writes a new one onto the conversation", async () => {
    // Roles are a preset component, but like the enabled tools they are editable
    // here and land on the CONVERSATION — applying a preset is a one-way stamp,
    // so an edit made here must not flow back into the preset it came from.
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    conversation.settings.agentDefinitions = [];
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();

    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await openPage(user, /^代理角色/);
    await user.click(screen.getByRole("button", { name: "新建角色" }));

    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(within(dialog).getByRole("textbox", { name: "角色名称" }), "security-reviewer");
    // The name is the role's whole model-facing surface; there is no second
    // free-text field to keep in sync with it.
    expect(within(dialog).queryByRole("textbox", { name: "说明" })).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onSettingsChange).toHaveBeenCalled();
    const saved = onSettingsChange.mock.calls.at(-1)![0].agentDefinitions;
    expect(saved.map((definition: { name: string }) => definition.name))
      .toEqual(["security-reviewer"]);
    expect(saved[0].modelSelection).toEqual({ kind: "inherit" });
    expect(screen.getByRole("button", { name: "设置角色 security-reviewer" })).toBeInTheDocument();
    expect(seed.globalSettings.conversationPresets[0].settings.agentDefinitions).toEqual([]);
  });

  it("reorders roles by dragging, rewriting the array the model reads in order", async () => {
    // A role's position IS data — unlike the scanned catalogs, whose order is
    // only ever a view preference — so dragging one writes the conversation.
    const user = userEvent.setup();
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    const role = (name: string, source: "user" | "project") => ({
      enabled: true,
      deleted: false,
      name,
      description: "",
      source,
      sourceKey: source === "project" ? "ws_default" : "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" as const },
      memory: "none" as const,
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null,
      fetchProvider: null,
      maxResults: DEFAULT_SEARCH_MAX_RESULTS,
      compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
      domainFilter: null,
      includeDomains: [],
      excludeDomains: [],
      templateId: null
    });
    /* The read-only project role sits between the two draggable ones on purpose:
       it is never drawn, and it must come out of the drag in the same slot. */
    conversation.settings.agentDefinitions = [
      role("alpha", "user"),
      role("from-project", "project"),
      role("beta", "user")
    ];
    const onSettingsChange = vi.fn();
    const { container } = render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await openPage(user, /^代理角色/);
    const list = container.querySelector<HTMLElement>("[data-sortable-list]")!;
    const rows = Array.from(list.querySelectorAll<HTMLElement>("[data-sortable-id]"));
    expect(rows.map((row) => row.dataset.sortableId)).toEqual(["alpha", "beta"]);

    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 240, bottom: top + height, width: 240, height,
        toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 60);
    rows.forEach((row, index) => setRect(row, index * 30, 29));

    fireEvent.pointerDown(rows[0], { pointerId: 22, button: 0, isPrimary: true, clientX: 40, clientY: 14 });
    fireEvent.pointerMove(window, { pointerId: 22, clientX: 40, clientY: 40 });
    fireEvent.pointerUp(window, { pointerId: 22, clientX: 40, clientY: 55 });

    const saved = onSettingsChange.mock.calls.at(-1)![0].agentDefinitions;
    expect(saved.map((definition: { name: string }) => definition.name))
      .toEqual(["beta", "from-project", "alpha"]);
  });

  it("counts only the roles it actually lists, ignoring host-injected read-only ones", async () => {
    // A conversation legitimately carries project/plugin roles the host
    // injected. They are read-only here and never drawn as rows, so counting
    // the raw array would announce a role the list then declines to show.
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    const role = (name: string, source: "user" | "project") => ({
      enabled: true,
      deleted: false,
      name,
      description: "",
      source,
      sourceKey: source === "project" ? "ws_default" : "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: { kind: "inherit" as const },
      memory: "none" as const,
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null,
      fetchProvider: null,
      maxResults: DEFAULT_SEARCH_MAX_RESULTS,
      compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
      domainFilter: null,
      includeDomains: [],
      excludeDomains: [],
      templateId: null
    });
    conversation.settings.agentDefinitions = [role("mine", "user"), role("from-project", "project")];
    const user = userEvent.setup();

    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    const entry = within(navigation()).getByRole("button", { name: /^代理角色/ });
    expect(entry).toHaveTextContent("1");

    await user.click(entry);
    expect(screen.getByRole("button", { name: "设置角色 mine" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "设置角色 from-project" })).toBeNull();
  });

  it("draws the role-less switch even with no usable role, and writes it as a preset component", async () => {
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    conversation.settings.agentDefinitions = [];
    const onSettingsChange = vi.fn();
    const user = userEvent.setup();
    const { unmount } = render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    // With no usable role the host allows role-less execution regardless, but
    // the switch stays on screen so the policy is never invisible.
    await openPage(user, /^代理角色/);
    expect(screen.getByRole("switch", { name: /角色必填|角色可选/ })).toBeInTheDocument();
    expect(screen.getByText(/当前没有可用角色/)).toBeInTheDocument();
    unmount();

    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            agentDefinitions: [{
              enabled: true,
              deleted: false,
              name: "mine",
              description: "",
              source: "user",
              sourceKey: "",
              revision: 1,
              memoryEpoch: 1,
              modelSelection: { kind: "inherit" },
              memory: "none",
              effort: null,
              tools: null,
              disallowedTools: [],
              searchProvider: null,
              fetchProvider: null,
              maxResults: DEFAULT_SEARCH_MAX_RESULTS,
              compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
              domainFilter: null,
              includeDomains: [],
              excludeDomains: [],
              templateId: null
            }]
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    await openPage(user, /^代理角色/);
    await user.click(screen.getByRole("switch", { name: "角色必填" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ allowRolelessSubagents: true })
    );
  });

  it("renames a saved preset in the row itself, committing when the field is left", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    // A preset of the user's own: only those offer the rename and delete
    // actions, which the built-in draws disabled.
    seed.globalSettings.conversationPresets.push({
      ...seed.globalSettings.conversationPresets[0],
      id: "conversation_team",
      name: "团队预设"
    });
    const onRenamePreset = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onRenamePreset={onRenamePreset}
      />
    );

    await openPage(user, /^对话预设/);
    const openRename = async (): Promise<HTMLElement> => {
      const row = screen.getByRole("button", { name: "打开预设 团队预设" })
        .closest(".catalog-row") as HTMLElement;
      await user.click(within(row).getByRole("button", { name: "重命名" }));
      return screen.getByRole("textbox", { name: "重命名预设 团队预设" });
    };

    // The row becomes the field, the way the sidebar renames a conversation:
    // leaving it is what commits, so there is no Save button to forget.
    const field = await openRename();
    expect(within(field.closest(".catalog-row") as HTMLElement)
      .queryByRole("button", { name: "保存" })).toBeNull();
    await user.clear(field);
    await user.type(field, "发布流程");
    await user.tab();
    expect(onRenamePreset).toHaveBeenCalledWith("conversation_team", "发布流程");
    expect(screen.queryByRole("textbox", { name: /^重命名预设/ })).toBeNull();

    // Enter is that same commit, reached from the keyboard.
    const viaEnter = await openRename();
    await user.clear(viaEnter);
    await user.type(viaEnter, "审查流程{Enter}");
    expect(onRenamePreset).toHaveBeenLastCalledWith("conversation_team", "审查流程");
    expect(onRenamePreset).toHaveBeenCalledTimes(2);

    // A blank name is not a rename, and neither is the name the preset already
    // had: both close the field without writing rather than storing a blank.
    const blank = await openRename();
    await user.clear(blank);
    await user.type(blank, "   ");
    await user.tab();
    expect(onRenamePreset).toHaveBeenCalledTimes(2);

    const unchanged = await openRename();
    expect(unchanged).toHaveValue("团队预设");
    await user.tab();
    expect(onRenamePreset).toHaveBeenCalledTimes(2);

    // Escape discards, so a name typed and then abandoned never reaches disk.
    const abandoned = await openRename();
    await user.clear(abandoned);
    await user.type(abandoned, "不要这个{Escape}");
    expect(onRenamePreset).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("textbox", { name: /^重命名预设/ })).toBeNull();
  });

  it("deletes a saved conversation preset from its own row", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets.push({
      ...seed.globalSettings.conversationPresets[0],
      id: "conversation_team",
      name: "团队预设"
    });
    const onDeletePreset = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onDeletePreset={onDeletePreset}
      />
    );

    await openPage(user, /^对话预设/);
    // Deleting arms in place and needs a second click, the same two steps the
    // conversation list uses.
    const saved = screen.getByRole("button", { name: "打开预设 团队预设" })
      .closest(".catalog-row") as HTMLElement;
    await user.click(within(saved).getByRole("button", { name: "删除预设 团队预设" }));
    expect(onDeletePreset).not.toHaveBeenCalled();
    await user.click(within(saved).getByRole("button", { name: "确认删除预设 团队预设" }));
    expect(onDeletePreset).toHaveBeenCalledWith("conversation_team");
  });

  it("deletes a catalog entry from its own row, in two clicks, and says nothing about its scope", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    const onDeleteCapability = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onDeleteCapability={onDeleteCapability}
      />
    );

    await openPage(user, /^技能/);
    const row = screen.getByText("代码审查").closest(".catalog-row") as HTMLElement;
    // A row says what the entry is called. Where it came from was a badge that
    // only ever repeated what the path in the tooltip already says.
    expect(within(row).queryByText("用户")).not.toBeInTheDocument();
    expect(row.querySelector(".catalog-row__badge")).toBeNull();

    await user.click(within(row).getByRole("button", { name: "删除 代码审查" }));
    expect(onDeleteCapability).not.toHaveBeenCalled();
    await user.click(within(row).getByRole("button", { name: "确认删除 代码审查" }));
    expect(onDeleteCapability).toHaveBeenCalledWith(
      "skills",
      expect.objectContaining({ id: "skill_code_review" })
    );

    // The same row on the MCP page, so the shared page is not skills-only.
    await openPage(user, /^MCP/);
    const server = screen.getByText("Workspace Files").closest(".catalog-row") as HTMLElement;
    await user.click(within(server).getByRole("button", { name: "删除 Workspace Files" }));
    await user.click(within(server).getByRole("button", { name: "确认删除 Workspace Files" }));
    expect(onDeleteCapability).toHaveBeenLastCalledWith(
      "mcp",
      expect.objectContaining({ id: "mcp_workspace" })
    );
  });

  it("offers no delete on a catalog row when the owner cannot route one", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    await openPage(user, /^技能/);
    const row = screen.getByText("代码审查").closest(".catalog-row") as HTMLElement;
    expect(within(row).queryByRole("button", { name: /^删除/ })).not.toBeInTheDocument();
  });

  it("opens a preset into the whole pane in preset mode and saves it back", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    const onSavePreset = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onSavePreset={onSavePreset}
      />
    );

    await openPage(user, /^对话预设/);
    await user.click(screen.getByRole("button", { name: "打开预设 默认" }));

    const dialog = screen.getByRole("dialog", { name: "默认" });
    const nestedNav = within(dialog).getByRole("navigation", { name: "对话设置分类" });
    // A window has no title bar: its name heads the page list, and the page names itself.
    expect(within(nestedNav).getByRole("heading", { level: 2, name: "默认" })).toBeInTheDocument();
    expect(within(dialog).getByRole("heading", { level: 3, name: "功能" })).toBeInTheDocument();
    // A preset body describes a reusable copy, so the one page about a live
    // conversation — its own presets — is withheld, the page only a preset has
    // appears, and the footer saves in place rather than saving a copy.
    expect(within(nestedNav).getAllByRole("button")
      .map((button) => (button.textContent ?? "").replace(/\d+$/, "")))
      .toEqual(["功能", "沙箱", "技能", "MCP", "钩子", "代理角色", "对话模板", "保存预设"]);
    expect(within(nestedNav).queryByRole("button", { name: /对话预设/ })).toBeNull();
    // Conversation-only, so a preset has no room to carry it.
    expect(within(dialog).queryByRole("switch", { name: /拼接应用数据目录/ })).toBeNull();

    await user.click(within(nestedNav).getByRole("button", { name: "保存预设" }));
    expect(onSavePreset).toHaveBeenCalledWith("conversation_default", expect.objectContaining({
      enabledTools: expect.any(Array),
      skillIds: expect.any(Array),
      mcpIds: expect.any(Array),
      hookIds: expect.any(Array)
    }));
    // Saving narrows the pane's whole body back down to exactly what a preset owns.
    expect(Object.keys(onSavePreset.mock.calls.at(-1)![1]).sort()).toEqual([
      "agentDefinitions", "allowRolelessSubagents",
      "enabledTools",
      "globalMemoryEnabled",
      "hookIds", "mcpIds", "mcpToolDiscoveryEnabled", "projectMemoryEnabled", "securityLevel",
      "skillIds",
      "skillToolEnabled", "toolDescriptionFileId", "webSearch", "webSearchEnabled"
    ]);
  });

  it("opens the preset's own message queue on its page and writes the edited body back", async () => {
    const user = userEvent.setup();
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets[0].templateId = "template_preset";
    const onReadTemplate = vi.fn(async (): Promise<ContextItem[]> => ([{
      id: "ctx_seed",
      kind: "user",
      content: "先读一下 README",
      createdAt: "2026-01-02T03:04:05.000Z"
    }]));
    const onWriteTemplate = vi.fn(async (): Promise<string> => "template_preset");
    const onBindPresetTemplate = vi.fn();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        templates={[template("template_preset", "开局", 1)]}
        onReadTemplate={onReadTemplate}
        onWriteTemplate={onWriteTemplate}
        onBindPresetTemplate={onBindPresetTemplate}
      />
    );

    await openPage(user, /^对话预设/);
    await user.click(screen.getByRole("button", { name: "打开预设 默认" }));
    const dialog = screen.getByRole("dialog", { name: "默认" });
    // A body is big enough that reading it on the chance the page is opened
    // would make opening a preset slower for everyone who never looks.
    expect(onReadTemplate).not.toHaveBeenCalled();

    const nestedNav = within(dialog).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(nestedNav).getByRole("button", { name: /^对话模板/ }));
    expect(onReadTemplate).toHaveBeenCalledWith("template_preset");
    expect(await within(dialog).findByText("先读一下 README")).toBeInTheDocument();

    // The queue is edited in place, on the surface a timeline is, and the whole
    // body goes back to the host as the edit lands — the page carries no save of
    // its own, so there is nothing here left to press.
    await user.click(within(dialog).getAllByRole("button", { name: "编辑上下文" })[0]);
    const field = within(dialog).getByRole("textbox", { name: "用户输入" });
    await user.clear(field);
    await user.type(field, "先读一下 AGENTS.md");
    const card = dialog.querySelector(".inline-text-editor") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "保存" }));
    expect(within(dialog).queryByRole("button", { name: "保存模板" })).toBeNull();

    await waitFor(() => expect(onWriteTemplate).toHaveBeenCalledWith("template_preset", [
      expect.objectContaining({ id: "ctx_seed", content: "先读一下 AGENTS.md" })
    ]));
    // The id did not move, so the preset has nothing new to cite.
    expect(onBindPresetTemplate).not.toHaveBeenCalled();
  });

  it("never disables a field on account of a preset", () => {
    const seed = createSeedDocument();
    render(
      <SettingsHarness
        initialConversation={{
          ...seed.workspaces[0].conversations[0],
          presetId: "conversation_default",
          templateId: ""
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    // A preset trace names where the values came from; it grants nobody authority
    // over the conversation, so every field stays editable.
    expect(backendTrigger("搜索提供商")).toBeEnabled();
    expect(screen.getByRole("switch", { name: "全局记忆已关闭" })).toBeEnabled();
  });

  it("narrows a capability page to the global level and the conversation's own workspace", async () => {
    const seed = createSeedDocument();
    // The fixture skill carries no workspace id: it is global, and every
    // conversation may select it.
    seed.capabilities.skills.push(
      {
        id: "skill_here",
        name: "本工作区技能",
        description: "测试用工作区技能。",
        location: "test://ws_a/skills/here/SKILL.md",
        source: "workspace",
        available: true,
        workspaceId: "ws_a"
      },
      {
        id: "skill_elsewhere",
        name: "别处技能",
        description: "另一工作区的技能。",
        location: "test://ws_b/skills/else/SKILL.md",
        source: "workspace",
        available: true,
        workspaceId: "ws_b"
      }
    );
    const user = userEvent.setup();
    const { unmount } = render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId="ws_a"
      />
    );

    await openPage(user, /^技能/);
    // Global plus own workspace; a sibling workspace's entry is not offered,
    // because the host would skip that selection at run time.
    expect(screen.getByRole("switch", { name: /代码审查/ })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: /本工作区技能/ })).toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: /别处技能/ })).toBeNull();
    // The counter divides the population it is drawn over, so it counts the
    // filtered catalog rather than everything the scan found.
    expect(screen.getByText("1 / 2 个已选")).toBeInTheDocument();
    unmount();

    // A draft with no workspace yet sees the global level alone.
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId={null}
      />
    );
    await openPage(user, /^技能/);
    expect(screen.getByRole("switch", { name: /代码审查/ })).toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: /本工作区技能/ })).toBeNull();
    expect(screen.queryByRole("switch", { name: /别处技能/ })).toBeNull();
  });

  it("shows the whole catalog in a preset window, which belongs to no workspace", async () => {
    const seed = createSeedDocument();
    seed.capabilities.skills.push({
      id: "skill_elsewhere",
      name: "别处技能",
      description: "另一工作区的技能。",
      location: "test://ws_b/skills/else/SKILL.md",
      source: "workspace",
      available: true,
      workspaceId: "ws_b"
    });
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId="ws_a"
      />
    );

    await openPage(user, /^对话预设/);
    await user.click(screen.getByRole("button", { name: "打开预设 默认" }));
    const dialog = screen.getByRole("dialog", { name: "默认" });
    const nestedNav = within(dialog).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(nestedNav).getByRole("button", { name: /^技能/ }));

    // A preset is reusable and points at no workspace, so it must see the whole
    // catalog rather than inherit the conversation's narrowing.
    expect(within(dialog).getByRole("switch", { name: /别处技能/ })).toBeInTheDocument();
  });

  it("rescans on mount and from the toolbar of every capability page", async () => {
    const seed = createSeedDocument();
    const onRescanCapabilities = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onRescanCapabilities={onRescanCapabilities}
      />
    );

    // Opening the pane is one of the moments discovery runs.
    expect(onRescanCapabilities).toHaveBeenCalledTimes(1);

    await openPage(user, /^技能/);
    await user.click(screen.getByRole("button", { name: "重新扫描" }));
    expect(onRescanCapabilities).toHaveBeenCalledTimes(2);

    await openPage(user, /^MCP/);
    await user.click(screen.getByRole("button", { name: "重新扫描" }));
    expect(onRescanCapabilities).toHaveBeenCalledTimes(3);

    await openPage(user, /^钩子/);
    await user.click(screen.getByRole("button", { name: "重新扫描" }));
    expect(onRescanCapabilities).toHaveBeenCalledTimes(4);
  });

  it("opens the global or this workspace's config folder from a capability page", async () => {
    const seed = createSeedDocument();
    const onRevealCapabilityLocation = vi.fn();
    const user = userEvent.setup();
    const { unmount } = render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId="ws_a"
        onRevealCapabilityLocation={onRevealCapabilityLocation}
      />
    );

    await openPage(user, /^技能/);
    await user.click(screen.getByRole("button", { name: "打开全局配置目录" }));
    expect(onRevealCapabilityLocation).toHaveBeenLastCalledWith("skills", null);
    await user.click(screen.getByRole("button", { name: "打开工作区配置目录" }));
    expect(onRevealCapabilityLocation).toHaveBeenLastCalledWith("skills", "ws_a");

    await openPage(user, /^MCP/);
    await user.click(screen.getByRole("button", { name: "打开工作区配置目录" }));
    // The kind travels with the click: the host opens the directory that kind lives in.
    expect(onRevealCapabilityLocation).toHaveBeenLastCalledWith("mcp", "ws_a");
    unmount();

    // A draft with no workspace has only the global level to open.
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId={null}
        onRevealCapabilityLocation={onRevealCapabilityLocation}
      />
    );
    await openPage(user, /^钩子/);
    expect(screen.getByRole("button", { name: "打开全局配置目录" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "打开工作区配置目录" })).toBeNull();
  });

  it("tests an MCP server's connection in the row and reports what came back", async () => {
    const seed = createSeedDocument();
    seed.capabilities.mcps.push({
      id: "mcp_dead",
      name: "Dead Server",
      description: "sse is not supported",
      location: "test://mcp/dead",
      source: "user",
      available: false
    });
    const passing: McpProbeReport = {
      ok: true,
      protocolVersion: "2025-06-18",
      serverName: "files",
      serverVersion: "1.2.3",
      tools: [{
        name: "read",
        title: "",
        description: "",
        requiresUserInteraction: false,
        inputSchema: null
      }],
      prompts: [],
      resources: [],
      logs: [],
      error: ""
    };
    let resolveProbe!: (report: McpProbeReport) => void;
    const onProbeMcpServer = vi.fn(() => new Promise<McpProbeReport>((resolve) => {
      resolveProbe = resolve;
    }));
    const user = userEvent.setup();
    const { unmount } = render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onProbeMcpServer={onProbeMcpServer}
      />
    );

    await openPage(user, /^MCP/);
    const row = screen.getByText("Workspace Files").closest(".catalog-row") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: "测试连接 Workspace Files" }));
    expect(onProbeMcpServer).toHaveBeenCalledWith(expect.objectContaining({ id: "mcp_workspace" }));
    // While the test is out, the row says so and the button holds.
    expect(within(row).getByText("测试中…")).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "测试连接 Workspace Files" })).toBeDisabled();

    await act(async () => { resolveProbe(passing); });
    // The row is one line, so the count is the badge and who answered is the tooltip.
    expect(await within(row).findByText("1 个工具")).toBeInTheDocument();
    expect(within(row).queryByText("测试中…")).toBeNull();
    expect(row).toHaveAttribute("title", expect.stringContaining("files 1.2.3"));

    // An unavailable server is not probed: it already carries the reason it is inert.
    const dead = screen.getByText("Dead Server").closest(".catalog-row") as HTMLElement;
    expect(within(dead).queryByRole("button", { name: "测试连接 Dead Server" })).toBeNull();
    expect(within(dead).getByText("不可用")).toBeInTheDocument();
    unmount();

    const failing = vi.fn(async (): Promise<McpProbeReport> => ({
      ok: false,
      protocolVersion: "",
      serverName: "",
      serverVersion: "",
      tools: [],
      prompts: [],
      resources: [],
      logs: ["connecting", "spawn ENOENT"],
      error: "spawn failed"
    }));
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onProbeMcpServer={failing}
      />
    );

    await openPage(user, /^MCP/);
    const failedRow = screen.getByText("Workspace Files").closest(".catalog-row") as HTMLElement;
    await user.click(within(failedRow).getByRole("button", { name: "测试连接 Workspace Files" }));
    expect(await within(failedRow).findByText("连接失败")).toBeInTheDocument();
    // The reason, then the last lines the server wrote while it tried.
    expect(failedRow).toHaveAttribute("title", expect.stringContaining("spawn failed"));
    expect(failedRow).toHaveAttribute("title", expect.stringContaining("spawn ENOENT"));
  });

  it("says per kind whether a dangling selection is skipped or fails the run", async () => {
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={{
          ...conversation,
          settings: {
            ...conversation.settings,
            skillIds: ["skill_code_review", "skill_gone"],
            hookIds: ["hook_gone"]
          }
        }}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        workspaceId="ws_a"
      />
    );

    await openPage(user, /^技能/);
    const skillDangling = screen.getByRole("switch", { name: /skill_gone/ })
      .closest(".catalog-row") as HTMLElement;
    expect(skillDangling).toHaveAttribute("title", expect.stringContaining("运行时跳过"));

    await openPage(user, /^钩子/);
    const hookDangling = screen.getByRole("switch", { name: /hook_gone/ })
      .closest(".catalog-row") as HTMLElement;
    expect(hookDangling).toHaveAttribute("title", expect.stringContaining("运行时会报错"));
  });
});
