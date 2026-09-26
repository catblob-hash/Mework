import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import { defaultAppearancePreferences } from "../../lib/appearance";
import type { GlobalSettings } from "../../types";
import { AppearanceSettings } from ".";

const pictureId = "a".repeat(64);
const backgroundMocks = vi.hoisted(() => ({
  importBackgroundImage: vi.fn(),
  backgroundImageData: vi.fn(),
  useBackgroundImportGeneration: () => 0
}));
vi.mock("../../lib/backgroundImage", () => backgroundMocks);

const localModelMocks = vi.hoisted(() => {
  let status: unknown = null;
  const listeners = new Set<() => void>();
  return {
    controller: {
      subscribe: (listener: () => void) => {
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      },
      current: () => status,
      refresh: vi.fn(async () => {}),
      install: vi.fn(async () => {}),
      activate: vi.fn(async () => {}),
      cancelInstall: vi.fn(async () => {}),
      remove: vi.fn(async () => {}),
      promptInfo: vi.fn(),
      defaultPrompts: vi.fn()
    },
    setStatus(next: unknown) {
      status = next;
      for (const listener of [...listeners]) listener();
    }
  };
});
vi.mock("../../lib/localModel", () => ({ localModelController: localModelMocks.controller }));

function modelStatus({
  ane = { phase: "missing" },
  mlx = { phase: "missing" },
  active = null,
  recommended = "ane",
  device = null,
  neuralEngineCores = 16
}: {
  ane?: Record<string, unknown>;
  mlx?: Record<string, unknown>;
  active?: "ane" | "mlx" | null;
  recommended?: "ane" | "mlx" | null;
  device?: string | null;
  neuralEngineCores?: number | null;
} = {}) {
  return {
    machine: { chip: "Apple M4", model: "Mac16,1", osVersion: "15.4", appleSilicon: true, neuralEngineCores },
    variants: [
      { id: "ane", downloadBytes: 1_520_000_000, diskBytes: ane.phase === "ready" ? 3_300_000_000 : 0, ...ane },
      { id: "mlx", downloadBytes: 1_660_000_000, diskBytes: mlx.phase === "ready" ? 1_700_000_000 : 0, ...mlx }
    ],
    active,
    recommended,
    warming: false,
    device,
    loaded: false,
    running: 0,
    queued: 0,
    slots: 0,
    context: 0,
    diskBytes: 3_300_000_000,
    lastError: null
  };
}

function globalSettings(): GlobalSettings {
  return {
    appLanguage: "auto",
    resolvedAppLanguage: "en-US",
    theme: "system",
    conversationPresets: [],
    defaultConversationPresetId: "",
    lastReasoningEffort: "medium",
    apiProviders: [],
    activeProviderId: null,
    webSearch: {
      providers: []
    },
    appearance: defaultAppearancePreferences(),
    shortcuts: {},
    environmentTools: [],
  executionEnvironments: { sshMachines: [], envVars: {} }
  };
}

function renderSettings(initial = globalSettings()) {
  let current = initial;
  const onChange = vi.fn();

  function Harness() {
    const [settings, setSettings] = useState(initial);
    current = settings;
    return (
      <AppearanceSettings
        settings={settings}
        onChange={(change) => {
          onChange(change);
          setSettings((value) =>
            typeof change === "function" ? change(value) : change
          );
        }}
      />
    );
  }

  return { ...render(<Harness />), getSettings: () => current, onChange };
}

describe("AppearanceSettings", () => {
  beforeEach(() => {
    configureI18n("en-US");
    backgroundMocks.importBackgroundImage.mockReset();
    backgroundMocks.backgroundImageData.mockReset();
    backgroundMocks.backgroundImageData.mockResolvedValue({
      dataUrl: "data:image/jpeg;base64,AAAA",
      width: 640,
      height: 360,
      largest: false
    });
  });
  afterEach(() => configureI18n("zh-CN"));

  it("normalizes valid hex drafts and rejects invalid drafts on blur", async () => {
    const user = userEvent.setup();
    const initial = globalSettings();
    initial.appearance.themeColor = "#356AE6";
    const { getSettings, onChange } = renderSettings(initial);
    const input = screen.getByLabelText("Hex accent color");

    await user.clear(input);
    await user.type(input, "#abc");
    expect(onChange).not.toHaveBeenCalled();
    await user.tab();

    expect(getSettings().appearance.themeColor).toBe("#AABBCC");
    expect(input).toHaveValue("#AABBCC");
    expect(onChange).toHaveBeenCalledTimes(1);

    await user.click(input);
    await user.clear(input);
    await user.type(input, "zzz");
    await user.tab();

    expect(input).toHaveValue("#AABBCC");
    expect(getSettings().appearance.themeColor).toBe("#AABBCC");
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("keeps font-size dragging local until a commit event", () => {
    const onChange = vi.fn();
    render(
      <AppearanceSettings settings={globalSettings()} onChange={onChange} />
    );
    const slider = screen.getByLabelText("Message font size");

    fireEvent.input(slider, { target: { value: "18" } });
    expect(slider).toHaveValue("18");
    expect(onChange).not.toHaveBeenCalled();

    fireEvent.pointerUp(slider, { target: { value: "18" } });
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("removes the send binding from newline choices", async () => {
    const user = userEvent.setup();
    renderSettings();
    const send = screen.getByLabelText("Send shortcut");

    await user.selectOptions(send, "Control+Enter");

    const newline = screen.getByLabelText("Newline shortcut");
    expect(
      within(newline).queryByRole("option", { name: "Ctrl + Enter" })
    ).not.toBeInTheDocument();
  });

  it("writes each theme preview preference", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderSettings();

    await user.click(screen.getByRole("button", { name: "Light" }));
    expect(getSettings().theme).toBe("day");

    await user.click(screen.getByRole("button", { name: "Dark" }));
    expect(getSettings().theme).toBe("night");

    await user.click(screen.getByRole("button", { name: "Follow system" }));
    expect(getSettings().theme).toBe("system");
  });

  it("names the theme card by its one row rather than repeating the title", () => {
    renderSettings();
    const card = screen.getByRole("region", { name: "Theme" });
    expect(within(card).getAllByText("Theme")).toHaveLength(1);
  });

  it("imports a picked picture and switches to the custom background", async () => {
    backgroundMocks.importBackgroundImage.mockResolvedValue({ id: pictureId, width: 3840, height: 2160 });
    const { container, getSettings } = renderSettings();
    const input = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    const click = vi.spyOn(input, "click");

    fireEvent.click(screen.getByRole("button", { name: "Custom background" }));
    expect(click).toHaveBeenCalled();

    const file = new File(["x"], "sea.heic", { type: "image/heic" });
    fireEvent.change(input, { target: { files: [file] } });
    await waitFor(() => expect(getSettings().appearance.backgroundImage).toBe(pictureId));
    expect(backgroundMocks.importBackgroundImage).toHaveBeenCalledWith(file);
    expect(getSettings().appearance.customBackground).toBe(true);
    expect(screen.getByRole("button", { name: "Custom background" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "Follow system" })).toHaveAttribute("aria-pressed", "false");
  });

  it("keeps the picture when a plain theme is chosen and returns to it without re-picking", async () => {
    const user = userEvent.setup();
    const initial = globalSettings();
    initial.appearance = { ...initial.appearance, customBackground: true, backgroundImage: pictureId };
    const { container, getSettings } = renderSettings(initial);
    const click = vi.spyOn(container.querySelector<HTMLInputElement>('input[type="file"]')!, "click");

    await user.selectOptions(screen.getByLabelText("Glass tone"), "night");
    expect(getSettings().theme).toBe("night");
    expect(getSettings().appearance.customBackground).toBe(true);

    await user.click(screen.getByRole("button", { name: "Light" }));
    expect(getSettings().theme).toBe("day");
    expect(getSettings().appearance).toMatchObject({ customBackground: false, backgroundImage: pictureId });
    expect(screen.queryByLabelText("Glass tone")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Custom background" }));
    expect(click).not.toHaveBeenCalled();
    expect(getSettings().appearance.customBackground).toBe(true);

    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(getSettings().appearance).toMatchObject({ customBackground: false, backgroundImage: "" });
  });

  it("asks for a picture again when the saved one can no longer be read", async () => {
    backgroundMocks.backgroundImageData.mockRejectedValue(new Error("背景图片不存在"));
    const initial = globalSettings();
    initial.appearance = { ...initial.appearance, backgroundImage: pictureId };
    const { container, getSettings } = renderSettings(initial);
    const click = vi.spyOn(container.querySelector<HTMLInputElement>('input[type="file"]')!, "click");

    await waitFor(() => expect(backgroundMocks.backgroundImageData).toHaveBeenCalled());
    await waitFor(() => {
      fireEvent.click(screen.getByRole("button", { name: "Custom background" }));
      expect(click).toHaveBeenCalled();
    });
    expect(getSettings().appearance.customBackground).toBe(false);
  });

  it("explains a picture the engine cannot decode", async () => {
    backgroundMocks.importBackgroundImage.mockRejectedValue(new Error("unreadable"));
    const { container, getSettings } = renderSettings();
    const input = container.querySelector<HTMLInputElement>('input[type="file"]')!;

    fireEvent.change(input, { target: { files: [new File(["x"], "broken.png")] } });
    expect(await screen.findByRole("alert")).toHaveTextContent("This picture can't be read");
    expect(getSettings().appearance.customBackground).toBe(false);
  });

  describe("local model", () => {
    beforeEach(() => {
      localModelMocks.controller.install.mockClear();
      localModelMocks.controller.activate.mockClear();
      localModelMocks.controller.remove.mockClear();
      localModelMocks.controller.promptInfo.mockReset();
      localModelMocks.controller.defaultPrompts.mockReset();
      localModelMocks.controller.defaultPrompts.mockResolvedValue({ title: "Default title prompt", shell: "Default shell prompt" });
    });

    it("asks which build to download when a use is turned on without a model", async () => {
      localModelMocks.setStatus(modelStatus());
      const user = userEvent.setup();
      const { getSettings } = renderSettings();
      await user.click(screen.getByRole("switch", { name: "Name conversations automatically" }));
      const dialog = await screen.findByRole("dialog", { name: "Download the local model?" });
      expect(within(dialog).getByText("This Mac: Apple M4 (Mac16,1) · 16-core Neural Engine · macOS 15.4")).toBeInTheDocument();
      expect(within(dialog).getByRole("radio", { name: /Neural Engine build \(Core ML\)/ })).toBeChecked();
      expect(within(dialog).getByRole("radio", { name: /GPU build \(MLX\)/ })).not.toBeChecked();
      expect(getSettings().appearance.localModel.titles).toBe(false);
      await user.click(within(dialog).getByRole("button", { name: "Download" }));
      expect(screen.queryByRole("dialog", { name: "Download from the mirror in mainland China?" })).not.toBeInTheDocument();
      expect(localModelMocks.controller.install).toHaveBeenCalledWith("ane", false);
      expect(getSettings().appearance.localModel.titles).toBe(true);
    });

    describe("on a system set to Chinese for mainland China", () => {
      let language: { mockRestore: () => void };
      beforeEach(() => {
        language = vi.spyOn(navigator, "language", "get").mockReturnValue("zh-CN");
      });
      afterEach(() => language.mockRestore());

      it("asks whether to download from the mirror there", async () => {
        localModelMocks.setStatus(modelStatus());
        const user = userEvent.setup();
        const { getSettings } = renderSettings();
        await user.click(screen.getByRole("switch", { name: "Name conversations automatically" }));
        const chooser = await screen.findByRole("dialog", { name: "Download the local model?" });
        await user.click(within(chooser).getByRole("radio", { name: /GPU build \(MLX\)/ }));
        await user.click(within(chooser).getByRole("button", { name: "Download" }));
        const question = await screen.findByRole("dialog", { name: "Download from the mirror in mainland China?" });
        expect(question).toHaveTextContent("hf-mirror.com");
        expect(localModelMocks.controller.install).not.toHaveBeenCalled();
        expect(getSettings().appearance.localModel.titles).toBe(false);
        await user.click(within(question).getByRole("button", { name: "Use the mirror" }));
        expect(localModelMocks.controller.install).toHaveBeenCalledWith("mlx", true);
        expect(getSettings().appearance.localModel.titles).toBe(true);
      });

      it("downloads directly, or not at all, as answered", async () => {
        localModelMocks.setStatus(modelStatus());
        const user = userEvent.setup();
        const { getSettings } = renderSettings();
        await user.click(screen.getByRole("switch", { name: "Explain shell commands" }));
        await user.click(within(await screen.findByRole("dialog", { name: "Download the local model?" })).getByRole("button", { name: "Download" }));
        const question = await screen.findByRole("dialog", { name: "Download from the mirror in mainland China?" });
        await user.keyboard("{Escape}");
        expect(question).not.toBeInTheDocument();
        expect(localModelMocks.controller.install).not.toHaveBeenCalled();
        expect(getSettings().appearance.localModel.shellExplanations).toBe(false);

        await user.click(screen.getByRole("switch", { name: "Explain shell commands" }));
        await user.click(within(await screen.findByRole("dialog", { name: "Download the local model?" })).getByRole("button", { name: "Download" }));
        const again = await screen.findByRole("dialog", { name: "Download from the mirror in mainland China?" });
        await user.click(within(again).getByRole("button", { name: "Download directly" }));
        expect(localModelMocks.controller.install).toHaveBeenCalledWith("ane", false);
        expect(getSettings().appearance.localModel.shellExplanations).toBe(true);
      });
    });

    it("offers only the MLX build on a Mac without a Neural Engine", async () => {
      localModelMocks.setStatus(
        modelStatus({ ane: { phase: "unsupported", reason: "noNeuralEngine" }, recommended: "mlx", neuralEngineCores: null })
      );
      const user = userEvent.setup();
      renderSettings();
      await user.click(screen.getByRole("switch", { name: "Explain shell commands" }));
      const dialog = await screen.findByRole("dialog", { name: "Download the local model?" });
      const ane = within(dialog).getByRole("radio", { name: /Neural Engine build/ });
      expect(ane).toBeDisabled();
      expect(within(dialog).getByText("No usable Neural Engine on this Mac")).toBeInTheDocument();
      expect(within(dialog).getByRole("radio", { name: /GPU build \(MLX\)/ })).toBeChecked();
      await user.click(within(dialog).getByRole("button", { name: "Download" }));
      expect(localModelMocks.controller.install).toHaveBeenCalledWith("mlx", false);
    });

    it("leaves the use off when the download is declined", async () => {
      localModelMocks.setStatus(modelStatus());
      const user = userEvent.setup();
      const { getSettings } = renderSettings();
      await user.click(screen.getByRole("switch", { name: "Explain shell commands" }));
      const dialog = await screen.findByRole("dialog", { name: "Download the local model?" });
      await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
      expect(localModelMocks.controller.install).not.toHaveBeenCalled();
      expect(getSettings().appearance.localModel.shellExplanations).toBe(false);
    });

    it("turns a use on directly once a build is installed", async () => {
      localModelMocks.setStatus(modelStatus({ ane: { phase: "ready" }, active: "ane", device: "Apple Neural Engine" }));
      const user = userEvent.setup();
      const { getSettings } = renderSettings();
      expect(
        screen.getByText("Neural Engine build (Core ML) · In use · Apple Neural Engine · 3.1 GB on disk")
      ).toBeInTheDocument();
      await user.click(screen.getByRole("switch", { name: "Explain shell commands" }));
      expect(screen.queryByRole("dialog", { name: "Download the local model?" })).not.toBeInTheDocument();
      expect(getSettings().appearance.localModel.shellExplanations).toBe(true);
    });

    it("switches between installed builds and removes one", async () => {
      localModelMocks.setStatus(modelStatus({ ane: { phase: "ready" }, mlx: { phase: "ready" }, active: "ane" }));
      localModelMocks.controller.promptInfo.mockResolvedValue({ tokens: 231, cacheBytes: 13_000_000, maxTokens: 737 });
      const user = userEvent.setup();
      renderSettings();
      await user.click(screen.getByRole("button", { name: "Manage…" }));
      const ane = await screen.findByRole("group", { name: "Neural Engine build (Core ML)" });
      const mlx = screen.getByRole("group", { name: "GPU build (MLX)" });
      expect(within(ane).getByText("In use")).toBeInTheDocument();
      expect(within(ane).queryByRole("button", { name: "Use" })).not.toBeInTheDocument();
      await user.click(within(mlx).getByRole("button", { name: "Use" }));
      expect(localModelMocks.controller.activate).toHaveBeenCalledWith("mlx");
      await user.click(within(ane).getByRole("button", { name: "Remove" }));
      await user.click(within(ane).getByRole("button", { name: "Remove" }));
      expect(localModelMocks.controller.remove).toHaveBeenCalledWith("ane");
    });

    it("reports each prompt's tokens and KV cache and stores edits", async () => {
      localModelMocks.setStatus(modelStatus({ ane: { phase: "ready" }, active: "ane" }));
      localModelMocks.controller.promptInfo.mockResolvedValue({ tokens: 231, cacheBytes: 13_000_000, maxTokens: 737 });
      const user = userEvent.setup();
      const { getSettings } = renderSettings();
      await user.click(screen.getByRole("button", { name: "Manage…" }));
      const titlePrompt = await screen.findByRole("textbox", { name: "Prompt for conversation titles" });
      await waitFor(() => expect(titlePrompt).toHaveValue("Default title prompt"));
      expect(
        await screen.findAllByText("231 tokens · KV cache 12.4 MB (limit 737 tokens)", {}, { timeout: 3000 })
      ).toHaveLength(2);
      expect(localModelMocks.controller.promptInfo).toHaveBeenCalledWith("title", "Default title prompt");

      fireEvent.change(titlePrompt, { target: { value: "Name it." } });
      expect(getSettings().appearance.localModel.titlePrompt).toBe("Name it.");
      fireEvent.change(titlePrompt, { target: { value: "Default title prompt" } });
      expect(getSettings().appearance.localModel.titlePrompt).toBe("");
    });
  });
});
