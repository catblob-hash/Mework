import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { defaultConversationWebSearchSettings } from "../lib/runtime";
import { isHostDerivedToolName } from "../lib/taskTools";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type {
  CapabilityCatalog,
  Conversation,
  ConversationSettings as ConversationSettingsType,
  GlobalSettings,
  ToolDescriptor
} from "../types";

import { ConversationSettings } from "./ConversationSettings";

function SettingsHarness({
  initialConversation,
  globalSettings,
  tools,
  capabilities,
  onSettingsChange,
  onConversationOnlyChange = vi.fn(),
  onApplyPreset = vi.fn(),
  onSaveAsPreset = vi.fn()
}: {
  initialConversation: Conversation;
  globalSettings: GlobalSettings;
  tools: ToolDescriptor[];
  capabilities: CapabilityCatalog;
  onSettingsChange: (settings: ConversationSettingsType) => void;
  onConversationOnlyChange?: (patch: Partial<ConversationSettingsType>) => void;
  onApplyPreset?: (presetId: string) => void;
  onSaveAsPreset?: () => void;
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
      onSaveAsPreset={onSaveAsPreset}
      onOpenGlobalSettings={vi.fn()}
      onClose={vi.fn()}
    />
  );
}

describe("ConversationSettings", () => {
  afterEach(() => configureI18n("zh-CN"));

  it("renders English controls and the implicit built-in preset without persisting a name", () => {
    configureI18n("en-US");
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets = [];
    const conversation = seed.workspaces[0].conversations[0];

    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    const panel = screen.getByRole("complementary", { name: "Conversation settings" });
    // A preset is something you apply, not a state the conversation is in: no
    // "following" banner, no lock, and nothing to detach from.
    expect(within(panel).queryByText(/Following:/)).toBeNull();
    expect(within(panel).queryByRole("button", { name: "Customize this conversation" })).toBeNull();
    expect(within(panel).queryByRole("button", { name: "Stop following" })).toBeNull();
    expect(within(panel).getByRole("combobox", { name: "Apply a preset" })).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "Enable all" })).toBeDisabled();
    expect(seed.globalSettings.conversationPresets).toEqual([]);
  });

  it("shows a localized fallback for a blank persisted preset name", () => {
    configureI18n("en-US");
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets[0].name = "";

    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    // The fallback is display-only: the blank name stays blank on disk.
    expect(screen.getByRole("option", { name: "Untitled preset" })).toBeInTheDocument();
    expect(seed.globalSettings.conversationPresets[0].name).toBe("");
  });

  it("applies a preset once instead of establishing a follow relationship", async () => {
    const seed = createSeedDocument();
    seed.globalSettings.conversationPresets.push({
      ...seed.globalSettings.conversationPresets[0],
      id: "conversation_second",
      name: "第二预设"
    });
    const onApplyPreset = vi.fn();
    const user = userEvent.setup();
    render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
        onApplyPreset={onApplyPreset}
      />
    );

    const picker = screen.getByRole("combobox", { name: "套用预设" });
    await user.selectOptions(picker, "conversation_second");
    expect(onApplyPreset).toHaveBeenLastCalledWith("conversation_second");
    // The picker snaps back to its placeholder: applying is an action, and the
    // conversation does not remember which preset the values came from.
    expect(picker).toHaveValue("");
    expect(screen.queryByRole("button", { name: "停止跟随" })).toBeNull();
  });

  it("exposes a save-as-preset entry beside the preset picker", async () => {
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

    await user.click(screen.getByRole("button", { name: "另存为预设" }));
    expect(onSaveAsPreset).toHaveBeenCalledTimes(1);
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

    expect(screen.queryByText("应用数据目录", { exact: true })).not.toBeInTheDocument();
    const toggle = screen.getByRole("switch", { name: "拼接应用数据目录已关闭" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await user.click(toggle);
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({ includeAppDataPath: true });
    expect(onSettingsChange).not.toHaveBeenCalled();
    expect(screen.getByRole("switch", { name: "拼接应用数据目录已开启" })).toHaveAttribute("aria-checked", "true");
  });

  it("offers no credential toggle: neither web search nor the agent can borrow sign-in state", () => {
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

    // The credential channels are gone, not merely off by default: research executors are always
    // anonymous, and every browser tab keeps its sign-in state to itself, so there is nothing for
    // a conversation setting to grant.
    expect(screen.queryByText("允许联网搜索使用我的登录凭证")).not.toBeInTheDocument();
    expect(screen.queryByText("允许 Agent 使用内置浏览器的 Cookie")).not.toBeInTheDocument();
  });

  it("exposes the system prompt editor without a preset-managed zone", () => {
    const seed = createSeedDocument();
    const conversation = {
      ...seed.workspaces[0].conversations[0],
      settings: { ...seed.workspaces[0].conversations[0].settings, systemPrompt: "初始提示" }
    };
    const onSettingsChange = vi.fn();
    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={onSettingsChange}
      />
    );

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    expect(within(panel).getByRole("textbox", { name: "系统提示词" })).toHaveValue("初始提示");
    // A preset is not conversation state, so no preset-managed or conversation-only sections appear.
    expect(within(panel).queryByText("由预设管理")).toBeNull();
    expect(within(panel).queryByText("仅此对话")).toBeNull();
    expect(onSettingsChange).not.toHaveBeenCalled();
  });

  it("shows separate bulk actions, counts only catalog tools, and exposes group expansion state", async () => {
    const seed = createSeedDocument();
    // Memory tools derive from memory-tier switches and are excluded from bulk actions and counts.
    const toolNames = Array.from(new Set(
      seed.tools
        .filter((tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name))
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    const enableAll = within(panel).getByRole("button", { name: "全部启用" });
    const disableAll = within(panel).getByRole("button", { name: "全部关闭" });
    expect(enableAll.parentElement).toBe(disableAll.parentElement);
    expect(enableAll.parentElement).toHaveClass("tool-bulk-actions");
    expect(within(panel).getByText(`1 / ${toolNames.length} 个已选`)).toBeInTheDocument();

    await user.click(enableAll);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({ enabledTools: ["missing-tool", ...toolNames] }));
    expect(enableAll).toBeDisabled();

    await user.click(disableAll);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({ enabledTools: ["missing-tool"] }));
    expect(disableAll).toBeDisabled();

    // Disabling all tools must not collapse groups; disclosure state changes only on user action.
    const filesystemGroup = within(panel).getByRole("button", { name: "文件与搜索" });
    expect(filesystemGroup).toHaveAttribute("aria-expanded", "true");
    await user.click(filesystemGroup);
    expect(filesystemGroup).toHaveAttribute("aria-expanded", "false");
  });

  it("bulk edits the complete user-selectable catalog exposed by temporary workspaces", async () => {
    const seed = createSeedDocument();
    const toolNames = seed.tools
      .filter((tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name))
      .map((tool) => tool.name);
    const conversation = {
      ...seed.workspaces[0].conversations[0],
      settings: {
        ...seed.workspaces[0].conversations[0].settings,
        enabledTools: toolNames
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

    expect(screen.getByText(`${toolNames.length} / ${toolNames.length} 个已选`)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "联网" })).toHaveAttribute("aria-expanded", "true");
    await user.click(screen.getByRole("button", { name: "全部关闭" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      enabledTools: []
    }));

    await user.click(screen.getByRole("button", { name: "全部启用" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      enabledTools: toolNames
    }));
  });

  it("selects skills, MCP servers, and hooks straight from the capability catalog", async () => {
    const seed = createSeedDocument();
    seed.capabilities.hooks.push({
      id: "hook_lint",
      name: "Lint 钩子",
      description: "测试用钩子。",
      location: "test://hooks/lint",
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

    // Selection is by catalog resource ID; there is no group layer left to
    // route it through, and each category is independent of the others.
    expect(screen.queryByText("不使用钩子预设")).not.toBeInTheDocument();
    for (const name of [/技能/, /^MCP/, /钩子/]) {
      await user.click(screen.getByRole("button", { name }));
    }
    expect(screen.getByRole("checkbox", { name: /代码审查/ })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /Workspace Files/ })).toBeChecked();

    await user.click(screen.getByRole("checkbox", { name: /代码审查/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: [],
      mcpIds: ["mcp_workspace"],
      hookIds: []
    }));

    await user.click(screen.getByRole("checkbox", { name: /Workspace Files/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: [],
      mcpIds: []
    }));

    await user.click(screen.getByRole("checkbox", { name: /Lint 钩子/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      hookIds: ["hook_lint"]
    }));

    // Scanning is automatic now: no list offers a manual trigger.
    expect(screen.queryByRole("button", { name: "重新扫描" })).not.toBeInTheDocument();
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
    await user.click(screen.getByRole("button", { name: /技能/ }));
    const dangling = screen.getByRole("checkbox", { name: /skill_uninstalled/ });
    expect(dangling).toBeChecked();
    expect(screen.getByText("悬空")).toBeInTheDocument();
    await user.click(dangling);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      skillIds: ["skill_code_review"]
    }));
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    await user.click(within(panel).getByRole("button", { name: /工具描述/ }));
    const section = within(panel).getByRole("button", { name: /工具描述/ })
      .closest("section") as HTMLElement;
    // Sniffed from disk and selected, exactly like skills and MCP: the app
    // shows what it found and never offers a place to author entries.
    expect(within(section).queryByRole("textbox")).toBeNull();

    await user.click(within(section).getByRole("checkbox", { name: /main/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      toolDescriptionFileId: "tooldesc_user_main_0f0f0f0f"
    }));

    // Single-select: unchecking the file falls back to the built-in descriptions.
    await user.click(within(section).getByRole("checkbox", { name: /main/ }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      toolDescriptionFileId: null
    }));
  });

  it("always offers the two built-in prompt profiles and treats no selection as the English one", async () => {
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    await user.click(within(panel).getByRole("button", { name: /工具描述/ }));
    const section = within(panel).getByRole("button", { name: /工具描述/ })
      .closest("section") as HTMLElement;
    const english = within(section).getByRole("checkbox", { name: /Mework 内置（英文）/ });
    const chinese = within(section).getByRole("checkbox", { name: /Mework 内置（中文）/ });
    // The conversation selects nothing, which the host renders with the English
    // built-in; the row says so instead of showing an empty selection.
    expect(english).toBeChecked();
    expect(chinese).not.toBeChecked();
    expect(within(section).getAllByText("内置")).toHaveLength(2);

    await user.click(chinese);
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      toolDescriptionFileId: "tooldesc_builtin_zh_cn"
    }));
  });

  it("collapses skills, MCP, hooks, and tool descriptions until they are opened", async () => {
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    // A new conversation opens compact: four closed disclosures whose own
    // buttons carry the title, so no separate heading is drawn above them.
    for (const name of [/技能/, /^MCP/, /钩子/, /工具描述/]) {
      const toggle = within(panel).getByRole("button", { name });
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      expect(toggle.closest("section")?.querySelector("h2, h3")).toBeNull();
    }
    // The selected code-review skill remains inert while collapsed.
    expect(within(panel).queryByRole("checkbox", { name: /代码审查/ })).toBeNull();

    await user.click(within(panel).getByRole("button", { name: /技能/ }));
    expect(within(panel).getByRole("button", { name: /技能/ })).toHaveAttribute("aria-expanded", "true");
    expect(within(panel).getByRole("checkbox", { name: /代码审查/ })).toBeChecked();
  });

  it("keeps the provider selector in the last untitled section, with no web-search heading or executor limit", () => {
    const seed = createSeedDocument();
    const { container } = render(
      <SettingsHarness
        initialConversation={seed.workspaces[0].conversations[0]}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    const providerSelect = screen.getByRole("combobox", { name: "搜索提供商" });
    expect(providerSelect).toBeInTheDocument();
    // Executor limits and the web-search heading are absent; the identically named tool row remains.
    expect(screen.queryByRole("spinbutton", { name: /搜索次数/ })).toBeNull();
    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    const headings = Array.from(panel.querySelectorAll(".settings-section__heading"))
      .map((heading) => heading.textContent ?? "");
    expect(headings.some((heading) => heading.includes("联网搜索"))).toBe(false);

    // The provider belongs in the untitled final section with the memory-tier switches.
    const section = providerSelect.closest("section.settings-section")!;
    expect(section.querySelector(".settings-section__heading")).toBeNull();
    expect(within(section as HTMLElement).getByRole("switch", { name: "全局记忆已关闭" })).toBeInTheDocument();
    const sections = Array.from(container.querySelectorAll("section.settings-section"));
    expect(sections.at(-1)).toBe(section);
  });

  it("offers no security level and no memory tool switches in the drawer", () => {
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    // Security policy is configured in the composer; the preset editor retains template controls.
    expect(within(panel).queryByText("安全")).toBeNull();
    for (const label of [/手动/, /允许编辑/, /计划模式/, /完全访问/]) {
      expect(within(panel).queryByRole("checkbox", { name: label })).toBeNull();
    }
    // Memory tools derive from the two tier switches and do not appear in the tool picker.
    expect(within(panel).queryByRole("button", { name: "长期记忆" })).toBeNull();
    expect(within(panel).queryByRole("switch", { name: /读取全局记忆|read_global_memory/ })).toBeNull();
    // The tier switches remain in the drawer and start disabled in the seed.
    expect(within(panel).getByRole("switch", { name: "全局记忆已关闭" })).toBeEnabled();
    expect(within(panel).getByRole("switch", { name: "项目记忆已关闭" })).toBeEnabled();
  });

  it("toggles each memory tier independently as a preset-component edit", async () => {
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    // Tier switches are preset components and use `onChange`, not conversation-only patches.
    await user.click(within(panel).getByRole("switch", { name: "全局记忆已关闭" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ globalMemoryEnabled: true, projectMemoryEnabled: false })
    );
    expect(onConversationOnlyChange).not.toHaveBeenCalled();

    // Enabling one tier must not enable the other.
    await user.click(within(panel).getByRole("switch", { name: "项目记忆已关闭" }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ globalMemoryEnabled: true, projectMemoryEnabled: true })
    );
  });

  it("offers the skill delivery switch only once a skill is selected, as a preset component", async () => {
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

    // Collapsed content is rendered but aria-hidden; presence is meaningful only after expansion.
    const empty = screen.getByRole("complementary", { name: "本对话设置" });
    await user.click(within(empty).getByRole("button", { name: /^技能/ }));
    // With no selected skills, neither delivery path has content to deliver, so omit the switch.
    expect(within(empty).queryByRole("switch", { name: /拼进提示词|按需加载/ })).toBeNull();

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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    await user.click(within(panel).getByRole("button", { name: /^技能/ }));
    await user.click(within(panel).getByRole("switch", { name: "拼进提示词" }));
    // This is a preset component and must use `onChange`.
    expect(onSettingsChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ skillToolEnabled: true })
    );
    expect(onConversationOnlyChange).not.toHaveBeenCalled();
  });

  it("never shows the host-derived skill tool in the tool picker", () => {
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

    // `skill` is cataloged for timeline rendering but derives from its switch rather than individual selection.
    expect(seed.tools.some((tool) => tool.name === "skill")).toBe(true);
    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    expect(within(panel).queryByRole("switch", { name: /^skill$|技能$/ })).toBeNull();
  });

  it("never disables a field on account of a preset", () => {
    const seed = createSeedDocument();
    const conversation = {
      ...seed.workspaces[0].conversations[0],
      presetId: "conversation_default"
    };
    render(
      <SettingsHarness
        initialConversation={conversation}
        globalSettings={seed.globalSettings}
        tools={seed.tools}
        capabilities={seed.capabilities}
        onSettingsChange={vi.fn()}
      />
    );

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    // Every field belongs to the conversation now. A stale presetId on the
    // record must not lock anything, because there is nothing to be linked to.
    expect(within(panel).getByRole("textbox", { name: "系统提示词" })).toBeEnabled();
    expect(within(panel).getByRole("combobox", { name: "搜索提供商" })).toBeEnabled();
    expect(
      within(panel).getByRole("switch", { name: "全局记忆已关闭" })
    ).toBeEnabled();
  });

  it("edits search provider behavior as a conversation-only patch", async () => {
    const seed = createSeedDocument();
    const defaults = defaultConversationWebSearchSettings();
    const onSettingsChange = vi.fn();
    const onConversationOnlyChange = vi.fn();
    const user = userEvent.setup();
    render(<SettingsHarness initialConversation={seed.workspaces[0].conversations[0]} globalSettings={{
      ...seed.globalSettings,
      webSearch: {
        ...seed.globalSettings.webSearch,
        providers: seed.globalSettings.webSearch.providers.map((provider) => provider.kind === "tavily" ? { ...provider, enabled: true } : provider)
      }
    }} tools={seed.tools} capabilities={seed.capabilities} onSettingsChange={onSettingsChange} onConversationOnlyChange={onConversationOnlyChange} />);

    await user.selectOptions(screen.getByRole("combobox", { name: "搜索提供商" }), "tavily");
    expect(onConversationOnlyChange).toHaveBeenLastCalledWith({
      webSearch: { ...defaults, provider: { kind: "explicit", providerKind: "tavily" } }
    });
    expect(onSettingsChange).not.toHaveBeenCalled();
  });

  it("lists this conversation's roles by name and writes a new one onto the conversation", async () => {
    // Roles are a preset component, but like the system prompt and the enabled
    // tools they are editable here and land on the CONVERSATION — applying a
    // preset is a one-way stamp, so an edit made here must not flow back into
    // the preset it came from.
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    await user.click(within(panel).getByRole("button", { name: /代理角色/ }));
    await user.click(within(panel).getByRole("button", { name: "新建角色" }));

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
    // The row is the role name; the preset it may have come from is untouched.
    expect(within(panel).getByRole("button", { name: "打开角色 security-reviewer" }))
      .toBeInTheDocument();
    expect(seed.globalSettings.conversationPresets[0].settings.agentDefinitions).toEqual([]);
  });

  it("counts only the roles it actually lists, ignoring host-injected read-only ones", async () => {
    // A conversation legitimately carries project/plugin roles the host
    // injected. They are read-only here and never drawn as rows, so counting
    // the raw array would announce a role the list then declines to show.
    const seed = createSeedDocument();
    const conversation = seed.workspaces[0].conversations[0];
    conversation.settings.agentDefinitions = [
      {
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
        searchProvider: null
      },
      {
        enabled: true,
        deleted: false,
        name: "from-project",
        description: "",
        source: "project",
        sourceKey: "ws_default",
        revision: 1,
        memoryEpoch: 1,
        modelSelection: { kind: "inherit" },
        memory: "none",
        effort: null,
        tools: null,
        disallowedTools: [],
        searchProvider: null
      }
    ];
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

    const panel = screen.getByRole("complementary", { name: "本对话设置" });
    const toggle = within(panel).getByRole("button", { name: /代理角色/ });
    expect(toggle).toHaveTextContent("1 个角色");

    await user.click(toggle);
    expect(within(panel).getByRole("button", { name: "打开角色 mine" })).toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "打开角色 from-project" })).toBeNull();
  });

});
