import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureApplicationAppearance } from "../theme";
import { MAX_SYSTEM_PROMPT_BYTES } from "../lib/textLimits";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type {
  ApiProvider,
  GlobalSettings as GlobalSettingsType,
  ModelProfile,
  SettingsView
} from "../types";
import { GlobalSettings } from "./GlobalSettings";

function workspaceProps(workspaces: ReturnType<typeof createSeedDocument>["workspaces"]) {
  return {
    workspaces
  };
}

const runtimeMocks = vi.hoisted(() => ({
  deleteApiKey: vi.fn(),
  saveApiKey: vi.fn(),
  getStoredApiKeyLength: vi.fn(),
  forgetStoredApiKeyLength: vi.fn(),
  revealApiKey: vi.fn(),
  fetchModels: vi.fn()
}));

// Only the credential/discovery calls are stubbed; the fixtures still need the real
// defaults helpers this module also exports.
vi.mock("../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/runtime")>()),
  ...runtimeMocks
}));

function model(id: string, overrides: Partial<ModelProfile> = {}): ModelProfile {
  return {
    id,
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: "encrypted",
    promptCache: true,
    ...overrides
  };
}

/** Conversation presets use `.provider-field` sections; locate the section by title. */
function presetSection(container: HTMLElement, title: string): HTMLElement {
  const heading = Array.from(container.querySelectorAll<HTMLElement>(".provider-field__title"))
    .find((node) => node.textContent === title);
  if (!heading?.parentElement) throw new Error(`No preset section titled ${title}`);
  return heading.parentElement;
}

function renderProviders(
  mutate?: (settings: GlobalSettingsType) => GlobalSettingsType,
  options: { onFlush?: () => Promise<void>; initialView?: SettingsView } = {}
) {
  const document = createSeedDocument();
  const initial = mutate?.(document.globalSettings) ?? document.globalSettings;
  const onFlush = vi.fn(() => options.onFlush?.() ?? Promise.resolve());
  let currentSettings = initial;
  function Harness() {
    const [settings, setSettings] = useState(initial);
    currentSettings = settings;
    return (
      <GlobalSettings
        initialView={options.initialView ?? "providers"}
        settings={settings}
        tools={document.tools}
        capabilities={document.capabilities}
        {...workspaceProps(document.workspaces)}
        onChange={setSettings}
        onFlush={onFlush}
        onClose={vi.fn()}
      />
    );
  }
  return {
    ...render(<Harness />),
    getSettings: () => currentSettings,
    onFlush
  };
}

function renderGeneral(mutate?: (settings: GlobalSettingsType) => GlobalSettingsType) {
  const document = createSeedDocument();
  const initial = mutate?.(document.globalSettings) ?? document.globalSettings;
  let currentSettings = initial;
  function Harness() {
    const [settings, setSettings] = useState(initial);
    currentSettings = settings;
    return (
      <GlobalSettings
        initialView="conversation_presets"
        settings={settings}
        tools={document.tools}
        capabilities={document.capabilities}
        {...workspaceProps(document.workspaces)}
        onChange={setSettings}
        onClose={vi.fn()}
      />
    );
  }
  return {
    ...render(<Harness />),
    getSettings: () => currentSettings
  };
}

/** Historical view IDs redirect to the active settings surface. */
function renderHistoricalView(view: "general" | "advanced" | "memory") {
  const document = createSeedDocument();
  function Harness() {
    const [settings, setSettings] = useState(document.globalSettings);
    return (
      <GlobalSettings
        initialView={view}
        settings={settings}
        tools={document.tools}
        capabilities={document.capabilities}
        {...workspaceProps(document.workspaces)}
        onChange={setSettings}
        onClose={vi.fn()}
      />
    );
  }
  return render(<Harness />);
}

function renderConversationPresets(
  mutate?: (document: ReturnType<typeof createSeedDocument>) => void
) {
  const document = createSeedDocument();
  mutate?.(document);
  let currentSettings = document.globalSettings;
  function Harness() {
    const [settings, setSettings] = useState(document.globalSettings);
    currentSettings = settings;
    return (
      <GlobalSettings
        initialView="conversation_presets"
        settings={settings}
        tools={document.tools}
        capabilities={document.capabilities}
        {...workspaceProps(document.workspaces)}
        onChange={setSettings}
        onClose={vi.fn()}
      />
    );
  }
  return {
    ...render(<Harness />),
    getSettings: () => currentSettings
  };
}

afterEach(() => {
  configureApplicationAppearance({ appLanguage: "zh-CN", theme: "day" });
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("removed general page", () => {
  it("redirects the historical general/advanced/memory ids to Appearance", () => {
    for (const view of ["general", "advanced", "memory"] as const) {
      const { unmount } = renderHistoricalView(view);

      expect(screen.getByRole("button", { name: "外观" })).toHaveClass("settings-nav__item--active");
      // The General view no longer appears in navigation; its default-preset control is
      // represented by the conversation preset page's Set as default action.
      expect(screen.queryByRole("button", { name: "通用" })).not.toBeInTheDocument();
      expect(screen.queryByRole("combobox", { name: "新对话默认预设" })).not.toBeInTheDocument();
      expect(screen.queryByText("新对话默认安全层级")).not.toBeInTheDocument();
      // Language and theme controls remain on Appearance after redirection.
      expect(screen.getByRole("combobox", { name: "应用语言" })).toBeInTheDocument();

      unmount();
    }
  });
});

describe("deleted group views", () => {
  function renderRedirect(
    view: "hooks" | "capability_catalog",
    mutate?: (document: ReturnType<typeof createSeedDocument>) => void
  ) {
    const document = createSeedDocument();
    mutate?.(document);
    let currentSettings = document.globalSettings;
    function Harness() {
      const [settings, setSettings] = useState(document.globalSettings);
      currentSettings = settings;
      return (
        <GlobalSettings
          initialView={view}
          settings={settings}
          tools={document.tools}
          capabilities={document.capabilities}
          {...workspaceProps(document.workspaces)}
          onChange={setSettings}
        onClose={vi.fn()}
        />
      );
    }
    return {
      ...render(<Harness />),
      getSettings: () => currentSettings
    };
  }

  it("redirects the removed hook and catalog views to the flat preset editor", () => {
    // Skills and MCP retain dedicated pages; only these IDs redirect.
    for (const view of ["hooks", "capability_catalog"] as const) {
      const { container, unmount } = renderRedirect(view);
      expect(container.querySelector(".conversation-preset-page > .provider-rail")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "对话预设" })).toHaveClass("settings-nav__item--active");
      // Group editors are gone: no group list, no group creation, no group name field.
      expect(screen.queryByRole("button", { name: "新建组" })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "新建预设" })).toBeInTheDocument();
      unmount();
    }
  });

  it("selects a scanned hook directly on the preset without any manual rescan control", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderRedirect("hooks", (document) => {
      document.capabilities.hooks = [{
        id: "hook_workspace_lint",
        name: "提交前检查",
        description: "代理结束后 · 运行 lint",
        location: "C:/workspace/.naiword/hooks.json#/hooks/lint",
        source: "workspace",
        available: true
      }];
    });

    // The catalog stays a read-only source: no command editor, no scope picker.
    expect(screen.queryByText(/hooks\.json/)).not.toBeInTheDocument();
    expect(screen.queryByText("作用范围")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("命令")).not.toBeInTheDocument();
    expect(screen.queryByText("添加钩子")).not.toBeInTheDocument();
    // Scanning is an automatic background concern now, so no pane offers a manual trigger.
    expect(screen.queryByRole("button", { name: "重新扫描" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: /提交前检查/ }));
    expect(getSettings().conversationPresets[0].settings.hookIds).toEqual(["hook_workspace_lint"]);
  });
});

describe("conversation preset defaults", () => {

  it("edits individual tools in the preset page, with no bulk or group-level switch", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderGeneral();

    // The default preset has the rail indicator, so its header action is disabled.
    expect(screen.getByRole("button", { name: "已是默认" })).toBeDisabled();
    expect(screen.queryByText("默认系统提示词")).not.toBeInTheDocument();

    // Tools are enabled individually; no bulk or category controls are exposed.
    expect(screen.queryByRole("button", { name: "全部启用" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "全部关闭" })).not.toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: /工具组/ })).not.toBeInTheDocument();
    const seedTools = createSeedDocument().tools;
    const firstTool = seedTools.find((tool) => tool.category === seedTools[0].category)!;
    const enabledBefore = getSettings().conversationPresets[0].settings.enabledTools;
    expect(enabledBefore).toEqual(expect.arrayContaining([firstTool.name]));

    await user.click(screen.getByRole("switch", { name: `${firstTool.label}已启用` }));
    expect(getSettings().conversationPresets[0].settings.enabledTools).toEqual(
      expect.not.arrayContaining([firstTool.name])
    );
    // Keep a category expanded after all its tools are disabled.
    expect(screen.getByRole("button", { name: /文件与搜索/ })).toHaveAttribute("aria-expanded", "true");

    await user.click(screen.getByRole("switch", { name: `${firstTool.label}已关闭` }));
    expect(getSettings().conversationPresets[0].settings.enabledTools).toEqual(
      expect.arrayContaining([firstTool.name])
    );
  });

  it("lays the preset editor out as a rail plus one single-column pane", () => {
    const { container } = renderConversationPresets();

    // Reuse the model-provider layout: full-height rail and fixed header with a
    // scrolling body, without a page heading card.
    expect(container.querySelector(".settings-editor-page.settings-rail-page.conversation-preset-page")).toBeInTheDocument();
    expect(container.querySelector(".conversation-preset-page > .provider-rail")).toBeInTheDocument();
    expect(container.querySelector(".provider-pane__header h1")).toHaveTextContent("默认");
    expect(container.querySelector(".provider-pane__body .provider-pane__stack")).toBeInTheDocument();
    expect(container.querySelector(".settings-page-heading")).toBeNull();
    expect(screen.queryByRole("button", { name: "管理" })).not.toBeInTheDocument();

    // All sections are stacked in one titled column rather than split across tabs.
    expect(container.querySelectorAll('[role="tab"]')).toHaveLength(0);
    const stack = container.querySelector<HTMLElement>(".provider-pane__stack")!;
    expect(Array.from(stack.querySelectorAll(".provider-field__title")).map((node) => node.textContent)).toEqual([
      "预设名称",
      "预设描述",
      "预设类别",
      "系统提示词",
      "联网搜索",
      "记忆",
      "启用工具",
      "代理角色",
      "技能",
      "MCP",
      "钩子",
      "工具描述",
      "安全策略"
    ]);
    expect(within(stack).getByLabelText("预设名称")).toBeInTheDocument();
    expect(within(stack).getByLabelText("预设类别")).toBeInTheDocument();
    expect(within(stack).getByLabelText("预设描述")).toBeInTheDocument();
    expect(within(stack).getByLabelText("系统提示词")).toBeInTheDocument();
    expect(within(presetSection(container, "技能")).getByRole("checkbox", { name: /代码审查/ })).not.toBeChecked();
  });

  // The rail indicator marks the default preset, and the pane header provides the
  // equivalent Set as default action.
  it("marks the default preset with the rail dot and the pane-header Set-as-default button", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderConversationPresets((document) => {
      document.globalSettings.conversationPresets.push({
        ...document.globalSettings.conversationPresets[0],
        id: "conversation_second",
        name: "第二个预设"
      });
    });

    const rows = Array.from(container.querySelectorAll<HTMLElement>(".provider-rail__row"));
    expect(rows.map((row) => row.getAttribute("aria-label"))).toEqual(["默认", "第二个预设"]);
    expect(rows[0].querySelector(".provider-rail__dot")).toBeInTheDocument();
    expect(rows[1].querySelector(".provider-rail__dot")).toBeNull();
    // A default preset cannot be selected again; one must exist when presets exist.
    expect(screen.getByRole("button", { name: "已是默认" })).toBeDisabled();

    await user.click(rows[1]);
    await user.click(screen.getByRole("button", { name: "设为默认" }));
    expect(getSettings().defaultConversationPresetId).toBe("conversation_second");
    expect(container.querySelectorAll(".provider-rail__row")[1].querySelector(".provider-rail__dot")).toBeInTheDocument();
    // After selection, the header action becomes the disabled default state.
    expect(screen.getByRole("button", { name: "已是默认" })).toBeDisabled();
  });

  it("owns both memory tier switches in its own section, without any memory tool row", async () => {
    const { container, getSettings } = renderConversationPresets();
    const memory = presetSection(container, "记忆");
    const tools = presetSection(container, "启用工具");

    // Memory settings are preset fields; individual memory tools are not tool rows.
    expect(within(tools).queryByRole("button", { name: "长期记忆" })).toBeNull();
    expect(within(tools).queryByRole("switch", { name: /读取全局记忆|read_global_memory/ })).toBeNull();

    await userEvent.setup().click(within(memory).getByRole("switch", { name: "项目记忆已关闭" }));
    const settings = getSettings().conversationPresets[0].settings;
    expect(settings.projectMemoryEnabled).toBe(true);
    // Enabling one memory tier must not enable the other.
    expect(settings.globalMemoryEnabled).toBe(false);
  });

  it("persists the preset description and hangs it off the rail row as a tooltip", () => {
    const { container, getSettings } = renderConversationPresets((document) => {
      document.globalSettings.conversationPresets[0].description = "";
    });

    const presetId = getSettings().conversationPresets[0].id;
    const row = () => container.querySelector<HTMLElement>('.provider-rail__row[data-selected="true"]')!;

    // Rail rows are single-line: descriptions appear only in the title attribute.
    expect(row().querySelector("small")).toBeNull();
    expect(row()).not.toHaveAttribute("title");

    fireEvent.change(
      within(presetSection(container, "预设描述")).getByLabelText("预设描述"),
      { target: { value: "评审补丁并给出结论" } }
    );

    expect(getSettings().conversationPresets.find((preset) => preset.id === presetId)?.description)
      .toBe("评审补丁并给出结论");
    expect(row()).toHaveAttribute("title", "评审补丁并给出结论");
    expect(row()).toHaveAttribute("aria-label", getSettings().conversationPresets[0].name);
  });

  it("selects one discovered tool-description file from its own section", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderConversationPresets();

    const pane = presetSection(container, "工具描述");
    const select = within(pane).getByRole("checkbox", { name: /main/ });
    await user.click(select);
    expect(getSettings().conversationPresets[0].settings.toolDescriptionFileId)
      .toBe("tooldesc_user_main_0f0f0f0f");

    // Clicking the selected file again clears the selection and restores built-in descriptions.
    await user.click(within(pane).getByRole("checkbox", { name: /main/ }));
    expect(getSettings().conversationPresets[0].settings.toolDescriptionFileId).toBeNull();
  });

  // The app only discovers and selects these files. Users maintain their contents in
  // `.mework/tool-descriptions/`, and the preset pane must not expose an editor.
  it("offers no way to author a tool description from the preset pane", () => {
    const { container } = renderConversationPresets();

    const pane = presetSection(container, "工具描述");
    expect(within(pane).queryByRole("textbox")).toBeNull();
    expect(within(pane).queryByRole("button", { name: /编辑工具描述/ })).toBeNull();
    expect(within(pane).queryByRole("button", { name: /删除工具描述/ })).toBeNull();
    expect(within(pane).queryByRole("button", { name: /新建工具描述/ })).toBeNull();
  });

  it("does not persist a preset system prompt beyond the UTF-8 storage limit", () => {
    const { getSettings } = renderConversationPresets();
    const systemPrompt = screen.getByPlaceholderText("留空则不发送基础系统提示词");
    const original = getSettings().conversationPresets[0].settings.systemPrompt;

    fireEvent.change(systemPrompt, {
      target: { value: "界".repeat(Math.floor(MAX_SYSTEM_PROMPT_BYTES / 3) + 1) }
    });

    expect(getSettings().conversationPresets[0].settings.systemPrompt).toBe(original);
    expect(systemPrompt).toHaveValue(original);
  });

  it("keeps dangling capability ids checked and picks same-named resources by exact id", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderConversationPresets((document) => {
      document.globalSettings.conversationPresets[0].settings.skillIds = ["skill_missing"];
      document.globalSettings.conversationPresets[0].settings.mcpIds = ["mcp_missing"];
      document.capabilities.skills = ["a", "b"].map((suffix) => ({
        id: `skill_same_${suffix}`,
        name: "同名能力",
        description: suffix.toUpperCase(),
        location: `test://skills/${suffix}/SKILL.md`,
        source: "user" as const,
        available: true
      }));
    });

    const skills = presetSection(container, "技能");
    // A capability that left the catalog stays selected and removable rather than being silently dropped.
    expect(within(skills).getByRole("checkbox", { name: /skill_missing/ })).toBeChecked();
    expect(within(skills).getByText("悬空")).toBeInTheDocument();
    expect(within(presetSection(container, "MCP")).getByRole("checkbox", { name: /mcp_missing/ })).toBeChecked();
    expect(getSettings().conversationPresets[0].settings.skillIds).toEqual(["skill_missing"]);

    const choices = within(skills).getAllByRole("checkbox", { name: /同名能力/ });
    expect(choices).toHaveLength(2);
    await user.click(choices[1]);
    expect(getSettings().conversationPresets[0].settings.skillIds).toEqual(["skill_missing", "skill_same_b"]);
    expect(choices[0]).not.toBeChecked();
    expect(choices[1]).toBeChecked();

    await user.click(within(skills).getByRole("checkbox", { name: /skill_missing/ }));
    expect(getSettings().conversationPresets[0].settings.skillIds).toEqual(["skill_same_b"]);
    expect(within(skills).queryByText("悬空")).not.toBeInTheDocument();
  });
});

describe("subagent role settings", () => {
  it("gives roles their own section in the conversation preset pane", () => {
    const { container } = renderConversationPresets();
    const roles = presetSection(container, "代理角色");

    expect(screen.queryByRole("button", { name: /子代理角色/ })).not.toBeInTheDocument();
    expect(roles.querySelector(".agent-definitions")).toBeInTheDocument();
    expect(within(roles).getByRole("button", { name: "新建角色" })).toBeInTheDocument();
  });

  it("edits the preset security policy in its own section", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderConversationPresets();

    // Security policy is a preset field; the conversation drawer uses a conversation-level selector.
    const pane = presetSection(container, "安全策略");
    expect(within(pane).getByRole("checkbox", { name: /手动/ })).toBeChecked();
    // Plan mode is a security level of its own, offered wherever the others are.
    expect(within(pane).getAllByRole("checkbox").map((box) => box.textContent))
      .toHaveLength(4);
    expect(within(pane).getByRole("checkbox", { name: /计划模式/ })).not.toBeChecked();

    await user.click(within(pane).getByRole("checkbox", { name: /完全访问/ }));
    expect(getSettings().conversationPresets[0].settings.securityLevel).toBe("full_access");
    expect(within(pane).getByRole("checkbox", { name: /完全访问/ })).toBeChecked();
  });

  it("writes a role the model can name back into the selected preset", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderConversationPresets((document) => {
      document.globalSettings.conversationPresets[0].settings.agentDefinitions = [];
    });

    await user.click(within(presetSection(container, "代理角色")).getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(within(dialog).getByRole("textbox", { name: "角色名称" }), "security-reviewer");
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    const saved = getSettings().conversationPresets[0].settings.agentDefinitions;
    expect(saved.map((definition) => definition.name)).toEqual(["security-reviewer"]);
    expect(saved[0].modelSelection).toEqual({ kind: "inherit" });
  });

  it("edits the preset search provider in its own section, with no search-count control", () => {
    const { container, getSettings } = renderConversationPresets();
    const webSearch = presetSection(container, "联网搜索");

    // The search-count limit is absent; this section only selects a provider.
    expect(screen.queryByRole("spinbutton", { name: /搜索次数/ })).not.toBeInTheDocument();
    expect(within(webSearch).getByRole("combobox", { name: "搜索提供商" })).toBeInTheDocument();
    // Memory and tool categories are adjacent sections, not a shared tab.
    expect(within(presetSection(container, "记忆")).getByRole("switch", { name: /全局记忆已/ })).toBeInTheDocument();
    expect(within(presetSection(container, "启用工具")).getByRole("button", { name: /文件与搜索/ })).toBeInTheDocument();

    fireEvent.change(within(webSearch).getByRole("combobox", { name: "搜索提供商" }), { target: { value: "tavily" } });
    expect(getSettings().conversationPresets[0].settings.webSearch.provider).toEqual({ kind: "explicit", providerKind: "tavily" });
    // Fetch-only providers are excluded because selecting one for search would always fail.
    const options = Array.from(
      within(webSearch).getByRole("combobox", { name: "搜索提供商" }).querySelectorAll("option")
    ).map((option) => option.getAttribute("value"));
    expect(options).toContain("native");
    expect(options).not.toContain("fetch");
  });
});

describe("advanced settings", () => {
  it("reorders providers by dragging the full list row", () => {
    const { container, getSettings } = renderProviders();
    expect(container.querySelector(".api-provider-page")).toHaveClass("settings-editor-page");
    expect(container.querySelector(".sortable-grip")).not.toBeInTheDocument();
    expect(container.querySelector(".lucide-grip-vertical")).not.toBeInTheDocument();
    const list = container.querySelector<HTMLElement>('[data-sortable-list="api-providers"]')!;
    const rows = Array.from(list.querySelectorAll<HTMLElement>("[data-sortable-id]"));
    const setRect = (element: HTMLElement, top: number, height: number) => {
      vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
        x: 0, y: top, left: 0, top, right: 174, bottom: top + height, width: 174, height, toJSON: () => ({})
      } as DOMRect);
    };
    setRect(list, 0, 180);
    rows.forEach((row, index) => setRect(row, index * 52, 51));
    fireEvent.click(rows[2]);
    expect(container.querySelector(".provider-pane__header h1")).toHaveTextContent("Anthropic Messages");

    fireEvent.pointerDown(rows[0], { pointerId: 10, button: 0, isPrimary: true, clientX: 80, clientY: 20 });
    fireEvent.pointerMove(window, { pointerId: 10, clientX: 80, clientY: 70 });
    fireEvent.pointerUp(window, { pointerId: 10, clientX: 80, clientY: 150 });

    expect(getSettings().apiProviders.map((provider) => provider.id)).toEqual([
      "openai_chat",
      "anthropic_messages",
      "openai_responses"
    ]);
    fireEvent.click(rows[0]);
    expect(container.querySelector(".provider-pane__header h1")).toHaveTextContent("Anthropic Messages");
  });

  beforeEach(() => {
    runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
    runtimeMocks.saveApiKey.mockReset().mockImplementation((_provider: ApiProvider, secret: string) => Promise.resolve({
      configured: true,
      keyLength: Array.from(secret).length
    }));
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.revealApiKey.mockReset().mockResolvedValue("stored-secret-key");
    runtimeMocks.fetchModels.mockReset().mockResolvedValue([]);
  });

  it("offers all wire formats without a current-provider, current-model, or chat-capability picker", async () => {
    const user = userEvent.setup();
    renderProviders();
    // Protocol and endpoint configuration live in the provider settings drawer; the
    // main panel retains only the key and address fields.
    await user.click(screen.getByRole("button", { name: "提供商设置" }));
    const format = within(screen.getByRole("dialog", { name: "提供商设置" })).getByLabelText("API 格式");
    // The nine user-selectable families follow `API_FORMAT_OPTIONS` order. The
    // built-in Codex and Claude Agent families are fixed rows, not choices here.
    expect(within(format).getAllByRole("option").map((option) => (option as HTMLOptionElement).value)).toEqual([
      "openai_responses",
      "openai_chat",
      "anthropic",
      "google",
      "xai",
      "azure",
      "bedrock",
      "vertex",
      "openai_compatible"
    ]);
    expect(screen.queryByLabelText("当前 API 提供商")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("提供商当前模型")).not.toBeInTheDocument();
    // Chat capability is gone; a provider is selectable purely by its enabled state.
    expect(screen.queryByLabelText("聊天能力")).not.toBeInTheDocument();
    expect(screen.queryByText("连接")).not.toBeInTheDocument();
    expect(screen.queryByText("兼容官方 API 或使用相同协议的代理服务")).not.toBeInTheDocument();
    expect(screen.queryByText("作用范围")).not.toBeInTheDocument();
  });

  it("gives search engines their own settings column instead of stacking them under the API providers", () => {
    const { container, unmount } = renderProviders();

    // The model page is now only about API providers: a provider rail plus one detail pane.
    const modelPages = Array.from(container.querySelectorAll<HTMLElement>(".provider-settings-page"));
    expect(modelPages).toHaveLength(1);
    expect(within(modelPages[0]).getByRole("button", { name: "添加提供商" })).toBeInTheDocument();
    expect(modelPages[0].querySelector("h3")).toBeNull();
    expect(screen.queryByText("DuckDuckGo")).not.toBeInTheDocument();
    unmount();

    // `web_search` is a legacy view id: it now lands on the dedicated search column.
    const search = renderProviders(undefined, { initialView: "web_search" });
    const searchPages = Array.from(search.container.querySelectorAll<HTMLElement>(".search-provider-page"));
    expect(searchPages).toHaveLength(1);
    // The search-provider page uses the same rail-and-pane layout without a heading card.
    expect(searchPages[0].querySelector(".provider-rail")).toBeInTheDocument();
    expect(searchPages[0].querySelector("h3")).toBeNull();
    expect(screen.getByRole("button", { name: "搜索提供商" })).toHaveClass("settings-nav__item--active");
    // The fixed ten-provider catalog has a general control followed by provider rows;
    // the enabled toggle is in the pane header.
    expect(within(searchPages[0]).getByRole("button", { name: "通用" })).toBeInTheDocument();
    expect(within(searchPages[0]).getByRole("button", { name: "Tavily" })).toBeInTheDocument();
    fireEvent.click(within(searchPages[0]).getByRole("button", { name: "Tavily" }));
    expect(screen.getByRole("switch", { name: "启用搜索提供商 Tavily" })).toBeInTheDocument();
    // Search behavior moved to conversation settings; only engines and keys live here.
    expect(screen.queryByRole("spinbutton", { name: /研究租约时限/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("radio", { name: /内置浏览器/ })).not.toBeInTheDocument();
  });

  it("lets providers be enabled and disabled", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();
    const enabled = screen.getByRole("switch", { name: "OpenAI Responses 启用状态" });
    expect(enabled).toHaveAttribute("aria-checked", "true");
    await user.click(enabled);
    expect(enabled).toHaveAttribute("aria-checked", "false");
    expect(getSettings().apiProviders[0].enabled).toBe(false);
    expect(getSettings().activeProviderId).toBe("openai_chat");
  });

  it("masks to the stored key length and reads the credential only while explicitly visible", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(17);
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(17)));
    expect(keyInput).toHaveAttribute("type", "password");
    expect(runtimeMocks.revealApiKey).not.toHaveBeenCalled();
    expect(screen.queryByText("已配置")).not.toBeInTheDocument();
    expect(screen.queryByText("未配置")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "显示 API Key" }).querySelector(".lucide-eye-off")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "显示 API Key" }));
    await waitFor(() => expect(runtimeMocks.revealApiKey).toHaveBeenCalledWith(
      expect.objectContaining({ id: "openai_responses" })
    ));
    expect(keyInput).toHaveAttribute("type", "text");
    expect(keyInput).toHaveValue("stored-secret-key");
    expect(screen.getByRole("button", { name: "隐藏 API Key" }).querySelector(".lucide-eye")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "OpenAI Chat Completions" }));
    await user.click(screen.getByRole("button", { name: "OpenAI Responses" }));
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(17)));
    expect(keyInput).toHaveAttribute("type", "password");

    await user.click(screen.getByRole("button", { name: "显示 API Key" }));
    await waitFor(() => expect(keyInput).toHaveValue("stored-secret-key"));
    await user.click(screen.getByRole("button", { name: "隐藏 API Key" }));
    expect(keyInput).toHaveAttribute("type", "password");
    expect(keyInput).toHaveValue("•".repeat(17));
  });

  it("deletes the stored credential when an emptied key field loses focus", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(17);
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(17)));
    await user.clear(keyInput);
    await user.click(screen.getByLabelText("Base URL"));

    await waitFor(() => expect(runtimeMocks.deleteApiKey).toHaveBeenCalledWith(
      expect.objectContaining({ id: "openai_responses" })
    ));
    await waitFor(() => expect(keyInput).toHaveValue(""));
  });

  it("saves a hidden draft, retains only its length, and restores that mask after reopening", async () => {
    const user = userEvent.setup();
    let configured = false;
    runtimeMocks.getStoredApiKeyLength.mockImplementation(() => Promise.resolve(configured ? 9 : undefined));
    runtimeMocks.saveApiKey.mockImplementation(async () => {
      configured = true;
      return { configured: true, keyLength: 9 };
    });
    const first = renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await user.type(keyInput, "sk-masked");
    await user.click(screen.getByLabelText("Base URL"));

    await waitFor(() => expect(runtimeMocks.saveApiKey).toHaveBeenCalledWith(
      expect.objectContaining({ id: "openai_responses" }),
      "sk-masked"
    ));
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(9)));
    expect(keyInput).toHaveAttribute("type", "password");
    expect(screen.queryByText(/API Key 已自动保存|浏览器预览未保存 Key 明文/)).not.toBeInTheDocument();

    first.unmount();
    renderProviders();
    const reopenedInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(reopenedInput).toHaveValue("•".repeat(9)));
    expect(reopenedInput).toHaveAttribute("type", "password");
    expect(runtimeMocks.revealApiKey).not.toHaveBeenCalled();
  });

  it("keeps an edited key temporary while visible and saves it automatically", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(17);
    const { onFlush } = renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(17)));
    await user.click(screen.getByRole("button", { name: "显示 API Key" }));
    await waitFor(() => expect(keyInput).toHaveValue("stored-secret-key"));
    await user.clear(keyInput);
    await user.type(keyInput, "sk-test-plaintext");
    await user.click(screen.getByLabelText("Base URL"));

    await waitFor(() => expect(runtimeMocks.saveApiKey).toHaveBeenCalledWith(
      expect.objectContaining({ id: "openai_responses" }),
      "sk-test-plaintext"
    ));
    expect(onFlush.mock.invocationCallOrder[1]).toBeLessThan(runtimeMocks.saveApiKey.mock.invocationCallOrder[0]);
    expect(keyInput).toHaveValue("sk-test-plaintext");
    expect(keyInput).toHaveAttribute("type", "text");
  });

  it("keeps the configured key when the API format or Base URL changes", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(17);
    renderProviders();

    await waitFor(() => expect(screen.getByLabelText("API Key")).toHaveValue("•".repeat(17)));
    await user.click(screen.getByRole("button", { name: "提供商设置" }));
    const options = screen.getByRole("dialog", { name: "提供商设置" });
    await user.selectOptions(within(options).getByLabelText("API 格式"), "anthropic");
    await user.click(within(options).getByRole("button", { name: "关闭 提供商设置" }));
    expect(screen.getByLabelText("Base URL")).toHaveValue("https://api.anthropic.com/v1");
    expect(screen.queryByText("已配置")).not.toBeInTheDocument();
    expect(screen.queryByText("需重新保存")).not.toBeInTheDocument();

    const baseUrl = screen.getByLabelText("Base URL");
    await user.clear(baseUrl);
    await user.type(baseUrl, "https://gateway.example.test/v1");
    expect(screen.queryByText("已配置")).not.toBeInTheDocument();
    expect(screen.queryByText(/重新保存 API Key/)).not.toBeInTheDocument();
  });

  it("keeps a successful key save after switching providers", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(undefined);
    let resolveSave!: (status: { configured: boolean; keyLength: number }) => void;
    runtimeMocks.saveApiKey.mockReturnValue(new Promise((resolve) => {
      resolveSave = resolve;
    }));
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await user.type(keyInput, "sk-switch-safe");
    await user.click(screen.getByRole("button", { name: "OpenAI Chat Completions" }));
    await waitFor(() => expect(runtimeMocks.saveApiKey).toHaveBeenCalledTimes(1));
    await user.click(screen.getByRole("button", { name: "OpenAI Responses" }));

    await act(async () => resolveSave({ configured: true, keyLength: 14 }));
    await waitFor(() => expect(screen.getByLabelText("API Key")).toHaveValue("•".repeat(14)));
    expect(screen.getByLabelText("API Key")).toHaveAttribute("type", "password");
    expect(screen.queryByText("已配置")).not.toBeInTheDocument();
    expect(screen.queryByText("未配置")).not.toBeInTheDocument();
  });

  it("removes a provider from its row menu without exposing a key deletion action", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    expect(screen.queryByRole("button", { name: "删除" })).not.toBeInTheDocument();
    // Deletion is in the row's kebab menu; selecting the row itself has no deletion action.
    await user.click(screen.getByRole("button", { name: "OpenAI Responses 的更多操作" }));
    await user.click(screen.getByRole("menuitem", { name: "删除提供商" }));

    expect(getSettings().apiProviders.some((provider) => provider.id === "openai_responses")).toBe(false);
    expect(getSettings().activeProviderId).toBe("openai_chat");
  });

  it("opens on the provider in use rather than the first row of the catalog", () => {
    // When the first catalog row is disabled, open the provider currently in use.
    const disableFirst = (settings: GlobalSettingsType) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider, index) => index === 0
        ? { ...provider, enabled: false }
        : provider)
    });
    const inUse = renderProviders((settings) => ({
      ...disableFirst(settings),
      activeProviderId: "anthropic_messages"
    }));
    expect(inUse.container.querySelector(".provider-pane__header h1")).toHaveTextContent("Anthropic Messages");
    inUse.unmount();

    // Without an active provider, select the first enabled row rather than the first row.
    const { container } = renderProviders((settings) => ({
      ...disableFirst(settings),
      activeProviderId: null
    }));
    expect(container.querySelector(".provider-pane__header h1")).toHaveTextContent("OpenAI Chat Completions");
  });

  it("keeps deletion limited to custom provider rows", async () => {
    // These fixture rows are user-created and remain deletable.
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "OpenAI Responses 的更多操作" }));
    const menu = screen.getByRole("menu");
    expect(within(menu).getByRole("menuitem", { name: "删除提供商" })).toBeInTheDocument();
    // Adding another instance is unavailable; create a separate provider instead.
    expect(within(menu).queryByRole("menuitem", { name: /再加一个/ })).not.toBeInTheDocument();

    await user.click(within(menu).getByRole("menuitem", { name: "删除提供商" }));
    expect(getSettings().apiProviders.some((provider) => provider.id === "openai_responses")).toBe(false);
    // Delete only the selected row; preserve all others.
    expect(getSettings().apiProviders.map((provider) => provider.id))
      .toEqual(["openai_chat", "anthropic_messages"]);
  });

  it("does not expose deletion for the built-in Codex row", async () => {
    const user = userEvent.setup();
    renderProviders((settings) => ({
      ...settings,
      apiProviders: [...settings.apiProviders, {
        id: "provider_codex",
        name: "OpenAI Codex",
        enabled: false,
        family: "openai_codex",
        baseUrl: "",
        familySettings: {},
        endpointBaseUrls: {},
        notes: "",
        models: [],
        activeModelId: null
      }]
    }));

    expect(screen.queryByRole("button", { name: "OpenAI Codex 的更多操作" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "OpenAI Codex" }));
    expect(screen.queryByRole("menuitem", { name: "删除提供商" })).not.toBeInTheDocument();
  });

  it("adds a custom provider from a settings form instead of a catalog picker", async () => {
    const user = userEvent.setup();
    const { container, getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "添加提供商" }));
    const dialog = screen.getByRole("dialog", { name: "添加提供商" });
    // The form is the only provider source; no catalog picker is available.
    expect(within(dialog).queryByLabelText("搜索内置提供商")).not.toBeInTheDocument();
    const confirm = within(dialog).getByRole("button", { name: "添加" });
    expect(confirm).toBeDisabled();

    await user.type(within(dialog).getByLabelText("提供商名称"), "我的中转站");
    await user.selectOptions(within(dialog).getByLabelText("对话协议"), "anthropic");
    await user.click(confirm);

    expect(screen.queryByRole("dialog", { name: "添加提供商" })).not.toBeInTheDocument();
    const created = getSettings().apiProviders.at(-1)!;
    expect(created).toMatchObject({
      name: "我的中转站",
      family: "anthropic",
      // The selected protocol supplies the initial endpoint.
      baseUrl: "https://api.anthropic.com/v1",
      enabled: true
    });
    // Select the newly created provider so its key and endpoint fields are visible.
    expect(container.querySelector(".provider-pane__header h1")).toHaveTextContent("我的中转站");
  });

  it("creates models in a secondary dialog and rejects blank or duplicate IDs", async () => {
    const user = userEvent.setup();
    renderProviders((settings) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider, index) => index === 0
        ? { ...provider, models: [model("existing-model")] }
        : provider)
    }));

    await user.click(screen.getByRole("button", { name: "手动添加模型" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    const id = within(dialog).getByLabelText("模型 ID");
    const save = within(dialog).getByRole("button", { name: "保存" });
    expect(id).toHaveValue("");
    expect(save).toBeDisabled();
    expect(within(dialog).queryByLabelText("模型显示名称")).not.toBeInTheDocument();
    expect(within(dialog).queryByLabelText("Temperature")).not.toBeInTheDocument();
    expect(within(dialog).queryByLabelText("Reasoning effort")).not.toBeInTheDocument();

    await user.type(id, "existing-model");
    expect(within(dialog).getByText("模型 ID 不能与同一提供商中的其他模型重复")).toBeInTheDocument();
    expect(save).toBeDisabled();

    await user.clear(id);
    await user.type(id, "new-model");
    expect(save).toBeEnabled();
    await user.click(save);
    expect(screen.getByTitle("new-model")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "模型 new-model 的属性" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "移除模型 new-model" })).toBeInTheDocument();
  });

  it("edits a model from its properties drawer and repairs the active selection on removal", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders((settings) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider, index) => index === 0
        ? {
            ...provider,
            activeModelId: "editable-model",
            models: [model("editable-model", { contextWindow: 64000 }), model("fallback-model")]
          }
        : provider)
    }));

    await user.click(screen.getByRole("button", { name: "模型 editable-model 的属性" }));
    const dialog = screen.getByRole("dialog", { name: "模型属性" });
    expect(within(dialog).getByLabelText("上下文窗口")).toHaveValue(64000);

    await user.click(within(dialog).getByRole("switch", { name: "editable-model 视觉输入" }));
    await user.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(getSettings().apiProviders[0].models[0].capabilities).toEqual(["image_recognition"]);
    expect(getSettings().apiProviders[0].activeModelId).toBe("editable-model");

    // Removing the active model hands the selection to the survivor instead of
    // leaving a dangling id that storage validation would reject on save.
    await user.click(screen.getByRole("button", { name: "移除模型 editable-model" }));
    expect(getSettings().apiProviders[0].activeModelId).toBe("fallback-model");

    await user.click(screen.getByRole("button", { name: "移除模型 fallback-model" }));
    expect(getSettings().apiProviders[0].activeModelId).toBeNull();
  });

  it("waits for the latest provider configuration to flush before fetching models", async () => {
    const user = userEvent.setup();
    let resolveFlush!: () => void;
    const onFlush = vi.fn(() => new Promise<void>((resolve) => {
      resolveFlush = resolve;
    }));
    renderProviders(undefined, { onFlush });

    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    expect(onFlush).toHaveBeenCalledTimes(1);
    expect(runtimeMocks.fetchModels).not.toHaveBeenCalled();

    await act(async () => resolveFlush());
    await waitFor(() => expect(runtimeMocks.fetchModels).toHaveBeenCalledTimes(1));
  });

  it("gates the fetch on the flush and issues no request when it rejects", async () => {
    const user = userEvent.setup();
    const onFlush = vi.fn(() => Promise.reject(new Error("上一次后台保存失败")));
    renderProviders(undefined, { onFlush });

    await user.click(screen.getByRole("button", { name: "拉取模型" }));

    // The flush gates the request, so a save-side fault stops model discovery
    // before anything is sent, and the drawer says why instead of showing an empty
    // catalog that looks like a provider with no models.
    await waitFor(() => expect(screen.getByRole("button", { name: "拉取模型" })).toBeEnabled());
    expect(onFlush).toHaveBeenCalledTimes(1);
    expect(runtimeMocks.fetchModels).not.toHaveBeenCalled();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("拉取模型列表失败");
    expect(alert).toHaveTextContent("上一次后台保存失败");
  });

  it("fetches the catalog for a provider that is still disabled and installs nothing on its own", async () => {
    const user = userEvent.setup();
    runtimeMocks.fetchModels.mockResolvedValue([model("catalog-model")]);
    // Fetching is a configuration-time action, so it must work for disabled providers.
    // Enabling remains an explicit user action.
    const { getSettings } = renderProviders((settings) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider) => ({ ...provider, enabled: false }))
    }));

    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    await waitFor(() => expect(runtimeMocks.fetchModels).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.fetchModels.mock.calls[0][0]).toMatchObject({ enabled: false });

    // Regression: a bare fetch fills the discovery drawer and touches nothing
    // else. Absorbing the upstream catalog into the provider was the old
    // behaviour and must not come back.
    await screen.findByRole("button", { name: "添加到提供商 catalog-model" });
    expect(getSettings().apiProviders[0].models).toEqual([]);

    await user.click(screen.getByRole("button", { name: "添加到提供商 catalog-model" }));
    expect(getSettings().apiProviders[0].models.map((entry) => entry.id)).toEqual(["catalog-model"]);

    await user.click(screen.getByRole("button", { name: "从提供商移除 catalog-model" }));
    expect(getSettings().apiProviders[0].models).toEqual([]);

    await user.click(screen.getByRole("button", { name: "关闭 发现模型" }));
    // Fetching must not implicitly enable the provider.
    expect(getSettings().apiProviders.some((provider) => provider.enabled)).toBe(false);
  });

  it("shows why the catalog request failed instead of reporting no matching model", async () => {
    const user = userEvent.setup();
    // A relay that answers 401 used to be indistinguishable from a provider with
    // an empty catalog, which sent users looking for the wrong problem.
    runtimeMocks.fetchModels.mockRejectedValue(
      new Error("API 请求失败（HTTP 401）：invalid api key。请检查 API Key，以及该中转站要求的鉴权方式。")
    );
    renderProviders();

    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    await waitFor(() => expect(runtimeMocks.fetchModels).toHaveBeenCalledTimes(1));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("拉取模型列表失败");
    expect(alert).toHaveTextContent("HTTP 401");
    expect(alert).toHaveTextContent("该中转站要求的鉴权方式");
  });

  it("clears the previous catalog failure once a fetch succeeds", async () => {
    const user = userEvent.setup();
    runtimeMocks.fetchModels.mockRejectedValueOnce(new Error("API 请求失败（HTTP 404）"));
    renderProviders();

    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("HTTP 404");

    runtimeMocks.fetchModels.mockResolvedValue([model("catalog-model")]);
    await user.click(screen.getByRole("button", { name: "重新拉取" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "添加到提供商 catalog-model" })).toBeInTheDocument();
  });

  it("keeps curated model values when a discovery result is installed over them", async () => {
    const user = userEvent.setup();
    runtimeMocks.fetchModels.mockResolvedValue([
      model("manual-model", {
        name: "上游叫法",
        group: "上游分组",
        contextWindow: 128000,
        maxOutputTokens: 32000,
        capabilities: ["image_recognition"]
      }),
      model("remote-only", { contextWindow: 200000 })
    ]);
    const { onFlush, getSettings } = renderProviders((settings) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider, index) => index === 0
        ? {
            ...provider,
            models: [model("manual-model", {
              name: "我的叫法",
              group: "我的分组",
              contextWindow: 64000,
              maxOutputTokens: 8000
            })]
          }
        : provider)
    }));

    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    expect(onFlush).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(runtimeMocks.fetchModels).toHaveBeenCalledTimes(1));

    // Installing the whole result set re-runs the merge over a row the user has
    // already curated; only the genuinely new model may join.
    await user.click(await screen.findByRole("button", { name: "添加全部结果" }));
    await user.click(screen.getByRole("button", { name: "关闭 发现模型" }));

    // Use title to locate rows because inferred group headings can share a name.
    expect(screen.getByTitle("remote-only")).toBeInTheDocument();
    expect(screen.getByTitle("manual-model")).toBeInTheDocument();
    expect(getSettings().apiProviders[0].models.find((entry) => entry.id === "manual-model"))
      .toMatchObject({ name: "我的叫法", group: "我的分组" });

    await user.click(screen.getByRole("button", { name: "模型 manual-model 的属性" }));
    const dialog = screen.getByRole("dialog", { name: "模型属性" });
    expect(within(dialog).getByLabelText("上下文窗口")).toHaveValue(64000);
    expect(within(dialog).getByLabelText("最大输出 Token")).toHaveValue(8000);
  });

  it("discards discovery results after the endpoint or selected provider changes", async () => {
    const user = userEvent.setup();
    let resolveModels!: (models: ModelProfile[]) => void;
    runtimeMocks.fetchModels.mockReturnValue(new Promise<ModelProfile[]>((resolve) => {
      resolveModels = resolve;
    }));
    renderProviders();

    // A fetch never touches the provider's own list, so the discovery drawer is
    // the only surface a stale result could reach.
    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    fireEvent.change(screen.getByLabelText("Base URL"), {
      target: { value: "https://gateway.example.test/v1" }
    });
    await act(async () => resolveModels([model("stale-endpoint-model")]));
    await waitFor(() => expect(screen.getByRole("button", { name: "重新拉取" })).toBeEnabled());
    expect(screen.queryByRole("button", { name: "添加到提供商 stale-endpoint-model" }))
      .not.toBeInTheDocument();

    // Control: the same row does appear once a result survives its own guard, so
    // the absence above is the endpoint check rather than a query that never matches.
    runtimeMocks.fetchModels.mockResolvedValue([model("stale-endpoint-model")]);
    await user.click(screen.getByRole("button", { name: "重新拉取" }));
    expect(await screen.findByRole("button", { name: "添加到提供商 stale-endpoint-model" }))
      .toBeInTheDocument();

    let resolveSecond!: (models: ModelProfile[]) => void;
    runtimeMocks.fetchModels.mockReturnValue(new Promise<ModelProfile[]>((resolve) => {
      resolveSecond = resolve;
    }));
    await user.click(screen.getByRole("button", { name: "重新拉取" }));
    // Switching providers closes the drawer; the in-flight result belongs to the
    // provider that is no longer selected.
    await user.click(screen.getByRole("button", { name: "OpenAI Chat Completions" }));
    await act(async () => resolveSecond([model("wrong-provider-model")]));
    await user.click(screen.getByRole("button", { name: "OpenAI Responses" }));

    // A fetch that never settles cannot overwrite the stored catalog, so the
    // reopened drawer shows exactly what the discarded result left behind.
    runtimeMocks.fetchModels.mockReturnValue(new Promise<ModelProfile[]>(() => {}));
    await user.click(screen.getByRole("button", { name: "拉取模型" }));
    expect(screen.queryByRole("button", { name: "添加到提供商 wrong-provider-model" }))
      .not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加到提供商 stale-endpoint-model" }))
      .toBeInTheDocument();
  });
});

describe("model capabilities", () => {
  beforeEach(() => {
    runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
    runtimeMocks.saveApiKey.mockReset().mockResolvedValue({ configured: true, keyLength: 8 });
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.revealApiKey.mockReset().mockResolvedValue("stored-secret-key");
    runtimeMocks.fetchModels.mockReset().mockResolvedValue([]);
  });

  it("lets a manually added model declare vision, and shows the chip only then", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "手动添加模型" }));
    const dialog = screen.getByRole("dialog", { name: "添加模型" });
    await user.type(within(dialog).getByLabelText("模型 ID"), "gpt-5");

    // A new model claims nothing until the user says so.
    const vision = within(dialog).getByRole("switch", { name: "gpt-5 视觉输入" });
    expect(vision).toHaveAttribute("aria-checked", "false");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(getSettings().apiProviders[0].models[0].capabilities).toEqual([]);
    expect(screen.queryByRole("img", { name: "视觉输入" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "模型 gpt-5 的属性" }));
    const reopened = screen.getByRole("dialog", { name: "模型属性" });
    await user.click(within(reopened).getByRole("switch", { name: "gpt-5 视觉输入" }));
    await user.click(within(reopened).getByRole("button", { name: "保存" }));

    expect(getSettings().apiProviders[0].models[0].capabilities).toEqual(["image_recognition"]);
    expect(screen.getByRole("img", { name: "视觉输入" })).toBeInTheDocument();
  });

  it("renders no chip for a retired capability an archived model still carries", async () => {
    // Documents written before the capability set shrank still name slugs such
    // as `function_call`; the row must drop them without losing the live one.
    const archived = {
      ...model("legacy-model"),
      capabilities: ["function_call", "image_recognition", "audio_generation"]
    } as unknown as ModelProfile;
    renderProviders((settings) => ({
      ...settings,
      apiProviders: settings.apiProviders.map((provider, index) => index === 0
        ? { ...provider, models: [archived] }
        : provider)
    }));

    const row = screen.getByTitle("legacy-model").closest(".model-row") as HTMLElement;
    expect(within(row).getByRole("img", { name: "视觉输入" })).toBeInTheDocument();
    expect(within(row).queryByRole("img", { name: "工具调用" })).not.toBeInTheDocument();
    expect(within(row).queryByRole("img", { name: "语音合成" })).not.toBeInTheDocument();
  });
});

describe("provider key transaction failures", () => {
  beforeEach(() => {
    runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
    runtimeMocks.saveApiKey.mockReset().mockImplementation((_provider: ApiProvider, secret: string) => Promise.resolve({
      configured: true,
      keyLength: Array.from(secret).length
    }));
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.revealApiKey.mockReset().mockResolvedValue("stored-secret-key");
    runtimeMocks.fetchModels.mockReset().mockResolvedValue([]);
  });

  it("reports a refused key save instead of returning silently", async () => {
    const user = userEvent.setup();
    runtimeMocks.saveApiKey.mockRejectedValue(
      new Error("凭据库拒绝写入：sk-secret-value 无法保存")
    );
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await user.type(keyInput, "sk-secret-value");
    await user.click(screen.getByLabelText("Base URL"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("保存 API Key 失败");
    // The reason survives, but the secret itself never reaches the screen.
    expect(alert).toHaveTextContent("凭据库拒绝写入");
    expect(alert.textContent).not.toContain("sk-secret-value");
    expect(keyInput).toHaveAttribute("aria-describedby", alert.id);
    expect(keyInput).toHaveAttribute("aria-invalid", "true");
    // The draft stays so the user can retry, and no success metadata is written.
    expect(keyInput).toHaveValue("sk-secret-value");
    expect(keyInput).toHaveAttribute("aria-busy", "false");
  });

  it("clears the save failure once a retry succeeds", async () => {
    const user = userEvent.setup();
    runtimeMocks.saveApiKey.mockRejectedValueOnce(new Error("凭据库暂时不可用"));
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await user.type(keyInput, "sk-first-try");
    await user.click(screen.getByLabelText("Base URL"));
    await screen.findByText(/保存 API Key 失败/);

    await user.clear(keyInput);
    await user.type(keyInput, "sk-second-try");
    await user.click(screen.getByLabelText("Base URL"));

    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(13)));
    expect(screen.queryByText(/保存 API Key 失败/)).not.toBeInTheDocument();
  });

  it("reports a flush failure without ever reaching the credential store", async () => {
    const user = userEvent.setup();
    renderProviders(undefined, { onFlush: () => Promise.reject(new Error("设置保存失败")) });

    const keyInput = screen.getByLabelText("API Key");
    await user.type(keyInput, "sk-needs-flush");
    await user.click(screen.getByLabelText("Base URL"));

    expect(await screen.findByText(/保存 API Key 失败：设置保存失败/)).toBeInTheDocument();
    expect(runtimeMocks.saveApiKey).not.toHaveBeenCalled();
  });

  it("drives the key field's invalid state from the save result alone", async () => {
    const user = userEvent.setup();
    runtimeMocks.saveApiKey.mockRejectedValueOnce(new Error("凭据库拒绝写入"));
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    // Precondition: nothing else marks the field, so the failure below is the
    // only thing that can turn it invalid.
    expect(keyInput).not.toHaveAttribute("aria-invalid");

    await user.type(keyInput, "sk-first-try");
    await user.click(screen.getByLabelText("Base URL"));
    expect(await screen.findByText(/保存 API Key 失败/)).toBeInTheDocument();
    expect(keyInput).toHaveAttribute("aria-invalid", "true");

    await user.clear(keyInput);
    await user.type(keyInput, "sk-second-try");
    await user.click(screen.getByLabelText("Base URL"));
    await waitFor(() => expect(keyInput).not.toHaveAttribute("aria-invalid"));
  });

  it("keeps a late failure attached to the provider that produced it", async () => {
    const user = userEvent.setup();
    let rejectSave!: (reason: Error) => void;
    runtimeMocks.saveApiKey.mockReturnValueOnce(new Promise((_resolve, reject) => {
      rejectSave = reject;
    }));
    renderProviders();

    await user.type(screen.getByLabelText("API Key"), "sk-slow-fail");
    await user.click(screen.getByRole("button", { name: "OpenAI Chat Completions" }));
    await waitFor(() => expect(runtimeMocks.saveApiKey).toHaveBeenCalledTimes(1));

    await act(async () => { rejectSave(new Error("凭据库拒绝写入")); });
    // The second provider is untouched by the first provider's rejection.
    expect(screen.queryByText(/保存 API Key 失败/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "OpenAI Responses" }));
    expect(screen.getByText(/保存 API Key 失败：凭据库拒绝写入/)).toBeInTheDocument();
  });

  it("reports a refused key deletion and a refused key read", async () => {
    const user = userEvent.setup();
    runtimeMocks.getStoredApiKeyLength.mockResolvedValue(9);
    runtimeMocks.deleteApiKey.mockRejectedValue(new Error("凭据库拒绝删除"));
    runtimeMocks.revealApiKey.mockRejectedValue(new Error("凭据库拒绝读取"));
    renderProviders();

    const keyInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(9)));

    await user.click(screen.getByRole("button", { name: "显示 API Key" }));
    expect(await screen.findByText(/读取 API Key 失败：凭据库拒绝读取/)).toBeInTheDocument();

    await user.clear(keyInput);
    await user.click(screen.getByLabelText("Base URL"));
    expect(await screen.findByText(/删除 API Key 失败：凭据库拒绝删除/)).toBeInTheDocument();
  });

  it("does not surface a key error after the settings page unmounts", async () => {
    const user = userEvent.setup();
    let rejectSave!: (reason: Error) => void;
    runtimeMocks.saveApiKey.mockReturnValueOnce(new Promise((_resolve, reject) => {
      rejectSave = reject;
    }));
    const { unmount } = renderProviders();

    await user.type(screen.getByLabelText("API Key"), "sk-unmount");
    await user.click(screen.getByLabelText("Base URL"));
    await waitFor(() => expect(runtimeMocks.saveApiKey).toHaveBeenCalledTimes(1));

    unmount();
    await act(async () => { rejectSave(new Error("凭据库拒绝写入")); });
    expect(screen.queryByText(/保存 API Key 失败/)).not.toBeInTheDocument();
  });
});
