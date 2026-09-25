import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureApplicationAppearance } from "../theme";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type {
  ApiProvider,
  GlobalSettings as GlobalSettingsType,
  ModelProfile,
  SettingsView
} from "../types";
import { GlobalSettings } from "./GlobalSettings";

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
        onChange={setSettings}
        onFlush={onFlush}
      />
    );
  }
  return {
    ...render(<Harness />),
    getSettings: () => currentSettings,
    onFlush
  };
}

/** Retired view IDs redirect to the active settings surface. */
function renderHistoricalView(view: SettingsView) {
  const document = createSeedDocument();
  function Harness() {
    const [settings, setSettings] = useState(document.globalSettings);
    return (
      <GlobalSettings
        initialView={view}
        settings={settings}
        onChange={setSettings}
      />
    );
  }
  return render(<Harness />);
}

afterEach(() => {
  configureApplicationAppearance({ appLanguage: "zh-CN", theme: "day" });
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("retired view redirects", () => {
  // Two of the retired ids now land on the provider page, which asks the host for each
  // provider's stored key length on mount.
  beforeEach(() => {
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
  });

  it("redirects the historical general/advanced/memory ids to Appearance", () => {
    for (const view of ["general", "advanced", "memory"] as const) {
      const { unmount } = renderHistoricalView(view);

      expect(screen.getByRole("button", { name: "外观" })).toHaveClass("settings-nav__item--active");
      // The General view no longer appears in navigation; its default-preset control is gone.
      expect(screen.queryByRole("button", { name: "通用" })).not.toBeInTheDocument();
      expect(screen.queryByRole("combobox", { name: "新对话默认预设" })).not.toBeInTheDocument();
      expect(screen.queryByText("新对话默认安全层级")).not.toBeInTheDocument();
      // Language and theme controls remain on Appearance after redirection.
      expect(screen.getByRole("combobox", { name: "应用语言" })).toBeInTheDocument();

      unmount();
    }
  });

  it("redirects the retired execution-environments id to Appearance", () => {
    renderHistoricalView("execution_environments");

    expect(screen.getByRole("button", { name: "外观" })).toHaveClass("settings-nav__item--active");
    // Machines are configured from the gear beside them, and variables belong to workspaces,
    // so the page and its navigation row are gone.
    expect(screen.queryByRole("button", { name: "执行环境" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "执行环境" })).not.toBeInTheDocument();
  });

  it("redirects the retired preset, hook, agent and catalog ids to Model providers", () => {
    for (const view of ["conversation_presets", "hooks", "agents", "capability_catalog"] as const) {
      const { container, unmount } = renderHistoricalView(view);

      expect(screen.getByRole("button", { name: "模型提供商" })).toHaveClass("settings-nav__item--active");
      // The preset editor is gone: no preset page, no preset nav row.
      expect(container.querySelector(".conversation-preset-page")).toBeNull();
      expect(screen.queryByRole("button", { name: "对话预设" })).not.toBeInTheDocument();
      // The provider page that the retired ids now land on renders.
      expect(container.querySelector(".api-provider-page")).toBeInTheDocument();

      unmount();
    }
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
    // The fixed ten-provider catalog fills the rail on its own; the enabled
    // toggle is in the pane header.
    expect(within(searchPages[0]).queryByRole("button", { name: "通用" })).not.toBeInTheDocument();
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

  it("removes a provider from its row without exposing a key deletion action", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    expect(screen.queryByRole("button", { name: "删除" })).not.toBeInTheDocument();
    // Deletion is the row's own two-step button; selecting the row itself has no deletion action.
    await user.click(screen.getByRole("button", { name: "删除 OpenAI Responses" }));
    await user.click(screen.getByRole("button", { name: "确认删除 OpenAI Responses" }));

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

    // A single click only arms the button; the row survives until it is confirmed.
    await user.click(screen.getByRole("button", { name: "删除 OpenAI Responses" }));
    expect(getSettings().apiProviders.some((provider) => provider.id === "openai_responses")).toBe(true);

    await user.click(screen.getByRole("button", { name: "确认删除 OpenAI Responses" }));
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

    expect(screen.queryByRole("button", { name: "删除 OpenAI Codex" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "OpenAI Codex" }));
    expect(screen.queryByRole("button", { name: "删除 OpenAI Codex" })).not.toBeInTheDocument();
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
