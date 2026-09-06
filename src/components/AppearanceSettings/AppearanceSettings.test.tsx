import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import { defaultAppearancePreferences } from "../../lib/appearance";
import type { GlobalSettings } from "../../types";
import { AppearanceSettings } from ".";

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
      providers: [],
      fetchProvider: null,
      maxResults: 5,
      excludeDomains: [],
      compression: { method: "cutoff", cutoffLimit: 2000 }
    },
    mcpServers: [],
    skills: [],
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
  beforeEach(() => configureI18n("en-US"));
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
});
