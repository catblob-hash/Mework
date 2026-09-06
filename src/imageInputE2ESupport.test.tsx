import { render, act } from "@testing-library/react";
import { Sparkles } from "lucide-react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "./i18n";
import { PopoverMenu } from "./components/PopoverMenu";
import {
  IMAGE_E2E_PROTOCOLS,
  findModelMenuRow,
  imageE2eModelTarget,
  imageE2eProviders,
  modelMenuPanel,
  modelMenuRows,
  modelMenuTrigger,
  openModelMenu,
  selectModelInMenu
} from "./imageInputE2ESupport";
import type { ModelMenuContext, ModelMenuRow } from "./imageInputE2ESupport";
import runnerSource from "./imageInputE2E.tsx?raw";
import hostSource from "../src-tauri/src/browser_dev.rs?raw";
import cliSource from "../scripts/image-input-e2e.mjs?raw";

// The image-input E2E used to switch protocols through `select[aria-label="模型"]`. The composer
// now renders a `PopoverMenu`, so that leg waited 180 s for an element that cannot exist. These
// tests render the real menu with the real props `App` passes it and drive it with the very
// helpers the runner calls, so a runner that regressed to a native select could not stay green.

const RUN_ID = "0123456789abcdef01234567";
// Fail fast: every wait below is expected to settle within a React commit.
const CONTEXT: ModelMenuContext = { timeoutMs: 2_000, pollMs: 5 };

interface Choice {
  value: string;
  providerId: string;
  providerName: string;
  modelId: string;
  disabled?: boolean;
}

function runnerChoices(): Choice[] {
  return imageE2eProviders({ runId: RUN_ID, baseUrl: "http://127.0.0.1:1/" }).map((provider) => ({
    // `App` uses this only as a React key; nothing in the DOM carries it.
    value: JSON.stringify([provider.id, provider.models[0].id]),
    providerId: provider.id,
    providerName: provider.name,
    modelId: provider.models[0].id
  }));
}

/**
 * The composer's model picker, with the props `App.tsx` gives it.
 *
 * Only the surface the helpers touch matters: the `composer__model` root, the popover trigger,
 * and rows whose primary line is the model id and secondary line the provider name.
 */
function ModelPicker({
  choices,
  onSelect
}: {
  choices: Choice[];
  onSelect?: (choice: Choice) => void;
}) {
  const [selectedValue, setSelectedValue] = useState(choices[0]?.value ?? "");
  const label = choices.find((choice) => choice.value === selectedValue)?.modelId ?? "选择模型";
  return (
    <div className="composer__options">
      <PopoverMenu
        rootClassName="composer__model"
        triggerClassName="composer-option"
        trigger={<span className="composer-option__label">{label}</span>}
        triggerLabel={`模型：${label}`}
        triggerTitle={label}
        menuLabel="模型"
        menuWidth={300}
        align="end"
        searchPlaceholder="搜索模型…"
        emptyLabel="没有匹配的模型"
        sections={[{
          id: "models",
          items: choices.map((choice) => ({
            id: choice.value,
            label: choice.modelId,
            description: choice.providerName,
            icon: <Sparkles size={14} />,
            checked: choice.value === selectedValue,
            disabled: choice.disabled,
            onSelect: () => {
              setSelectedValue(choice.value);
              onSelect?.(choice);
            }
          }))
        }]}
      />
    </div>
  );
}

/** The model id the trigger currently advertises, which is what a user sees when closed. */
function triggerLabel(): string | null {
  return modelMenuTrigger()?.getAttribute("title") ?? null;
}

describe("the composer's model picker", () => {
  beforeEach(() => configureI18n("zh-CN"));

  it("offers no native select for the old interaction to find", () => {
    render(<ModelPicker choices={runnerChoices()} />);
    expect(document.querySelector('select[aria-label="模型"]')).toBeNull();
    expect(document.querySelectorAll("select")).toHaveLength(0);
    expect(modelMenuTrigger()).not.toBeNull();
  });

  it("portals its panel outside the composer, where only document.body finds it", async () => {
    const { container } = render(<ModelPicker choices={runnerChoices()} />);
    let panel!: HTMLElement;
    await act(async () => {
      panel = await openModelMenu(CONTEXT);
    });
    expect(container.contains(panel)).toBe(false);
    expect(panel.parentElement).toBe(document.body);
    expect(panel.getAttribute("aria-label")).toBe("模型");
    expect(modelMenuRows(panel).map((row) => row.modelId)).toEqual(
      runnerChoices().map((choice) => choice.modelId)
    );
  });

  it("reports the rows by their two visible lines", async () => {
    render(<ModelPicker choices={runnerChoices()} />);
    let rows: ModelMenuRow[] = [];
    await act(async () => {
      rows = modelMenuRows(await openModelMenu(CONTEXT));
    });
    expect(rows.map((row) => ({ modelId: row.modelId, providerName: row.providerName }))).toEqual(
      runnerChoices().map((choice) => ({
        modelId: choice.modelId,
        providerName: choice.providerName
      }))
    );
    expect(rows.filter((row) => row.checked)).toHaveLength(1);
    expect(rows[0].checked).toBe(true);
  });

  it("switches to each of the three protocols in turn", async () => {
    const selected = vi.fn<(choice: Choice) => void>();
    render(<ModelPicker choices={runnerChoices()} onSelect={selected} />);

    for (const protocol of IMAGE_E2E_PROTOCOLS) {
      const target = imageE2eModelTarget(protocol, RUN_ID);
      await act(async () => {
        await selectModelInMenu(target, CONTEXT);
      });
      const choice = selected.mock.lastCall?.[0];
      expect(choice?.providerId, protocol).toBe(`image-e2e-${protocol}-${RUN_ID}`);
      expect(choice?.modelId, protocol).toBe(target.modelId);
      // The menu closes itself on selection and the trigger names the new model.
      expect(modelMenuPanel(CONTEXT)).toBeNull();
      expect(triggerLabel()).toBe(target.modelId);

      // Reopen and read the menu's own account of the selection.
      let rows: ModelMenuRow[] = [];
      await act(async () => {
        rows = modelMenuRows(await openModelMenu(CONTEXT));
      });
      expect(rows.filter((row) => row.checked).map((row) => row.modelId)).toEqual([target.modelId]);
      await act(async () => {
        modelMenuTrigger()?.click();
      });
    }
    expect(selected).toHaveBeenCalledTimes(IMAGE_E2E_PROTOCOLS.length);
  });

  it("picks the right provider when two of them expose the same model id", async () => {
    const shared: Choice[] = [
      { value: "first", providerId: "p-first", providerName: "Image E2E · 甲", modelId: "same-model" },
      { value: "second", providerId: "p-second", providerName: "Image E2E · 乙", modelId: "same-model" }
    ];
    const selected = vi.fn<(choice: Choice) => void>();
    render(<ModelPicker choices={shared} onSelect={selected} />);

    await act(async () => {
      await selectModelInMenu({ modelId: "same-model", providerName: "Image E2E · 乙" }, CONTEXT);
    });

    expect(selected).toHaveBeenCalledTimes(1);
    expect(selected.mock.lastCall?.[0].providerId).toBe("p-second");
  });

  it("fails by name when the target is not in the menu, leaving the selection alone", async () => {
    const choices = runnerChoices();
    const selected = vi.fn<(choice: Choice) => void>();
    render(<ModelPicker choices={choices} onSelect={selected} />);

    await expect(act(async () => {
      await selectModelInMenu({ modelId: "absent-model", providerName: "Image E2E · 不存在" }, CONTEXT);
    })).rejects.toThrow(/模型菜单里没有「Image E2E · 不存在 · absent-model」/u);
    expect(selected).not.toHaveBeenCalled();
    expect(triggerLabel()).toBe(choices[0].modelId);
  });

  it("fails when the target row is disabled instead of running the previous model", async () => {
    const choices = runnerChoices().map((choice, index) => (
      index === 2 ? { ...choice, disabled: true } : choice
    ));
    const selected = vi.fn<(choice: Choice) => void>();
    render(<ModelPicker choices={choices} onSelect={selected} />);
    const target = imageE2eModelTarget("anthropic", RUN_ID);

    await expect(act(async () => {
      await selectModelInMenu(target, CONTEXT);
    })).rejects.toThrow(/被禁用/u);
    expect(selected).not.toHaveBeenCalled();
    expect(triggerLabel()).toBe(choices[0].modelId);
  });

  it("refuses to read rows while another popover is also open", async () => {
    render(<ModelPicker choices={runnerChoices()} />);
    await act(async () => {
      await openModelMenu(CONTEXT);
    });
    expect(modelMenuPanel(CONTEXT)).not.toBeNull();

    // A second panel makes the trigger/panel pairing ambiguous: the rows below could be another
    // menu's. Guessing is exactly how the old code would have selected the wrong model.
    const stray = document.createElement("div");
    stray.className = "popover-menu__panel";
    stray.setAttribute("role", "menu");
    document.body.append(stray);
    try {
      expect(modelMenuPanel(CONTEXT)).toBeNull();
    } finally {
      stray.remove();
    }
    expect(modelMenuPanel(CONTEXT)).not.toBeNull();
  });
});

describe("findModelMenuRow", () => {
  function row(modelId: string, providerName: string, extra: Partial<ModelMenuRow> = {}): ModelMenuRow {
    return {
      element: document.createElement("button"),
      modelId,
      providerName,
      checked: false,
      disabled: false,
      ...extra
    };
  }

  it("refuses to guess between two identical rows", () => {
    const rows = [row("m", "P"), row("m", "P")];
    expect(() => findModelMenuRow(rows, { modelId: "m", providerName: "P" })).toThrow(
      /有 2 个/u
    );
  });

  it("names what the menu did offer when the target is missing", () => {
    const rows = [row("m", "P"), row("n", "Q", { disabled: true })];
    expect(() => findModelMenuRow(rows, { modelId: "n", providerName: "P" })).toThrow(
      /当前可选：P · m；Q · n（禁用）/u
    );
  });

  it("returns the one row matching both lines", () => {
    const rows = [row("m", "P"), row("m", "Q")];
    expect(findModelMenuRow(rows, { modelId: "m", providerName: "Q" })).toBe(rows[1]);
  });
});

// The tests above prove the helpers are right; they cannot prove the runner uses them, and the
// runner is the only thing the E2E command executes.
describe("src/imageInputE2E.tsx", () => {
  it("drives the model menu through the shared helpers", () => {
    expect(runnerSource).toMatch(/from "\.\/imageInputE2ESupport"/u);
    expect(runnerSource).toContain("selectModelInMenu(imageE2eModelTarget(protocol, configuredRunId))");
  });

  it("keeps no trace of the retired native select interaction", () => {
    // `new Event("change")` stays out of this list: the file-upload leg legitimately dispatches
    // one at an `input[type=file]`, and pinning it would only forbid a correct thing.
    for (const pattern of [
      /select\[aria-label/u,
      /HTMLSelectElement/u,
      /\.options\b/u
    ]) {
      expect(runnerSource, String(pattern)).not.toMatch(pattern);
    }
  });

  it("verifies the persisted protocol after selecting, not just the menu", () => {
    expect(runnerSource).toContain("persisted.globalSettings.activeProviderId === expectedProviderId");
    expect(runnerSource).toContain("provider.activeModelId === modelId(protocol)");
  });
});

// The runner registers one provider per protocol and stores a fake key under each provider id.
// Two other surfaces spell those ids out again: the host command that deletes the keys, and the
// CLI that checks what the host reported. All three must agree, or the leftover key is never
// deleted and `cleanupProviderSecrets` aborts the run on a mismatch it cannot explain.
describe("the provider ids of the image E2E", () => {
  const expectedPrefixes = IMAGE_E2E_PROTOCOLS.map((protocol) => `image-e2e-${protocol}-`);

  it("are spelled the same way by the host cleanup command", () => {
    const ids = [...hostSource.matchAll(/format!\("(image-e2e-[a-z_]+-)\{run_id\}"\)/gu)]
      .map(([, prefix]) => prefix);
    // Without this the assertion below would also pass on a regex that matched nothing.
    expect(ids).toHaveLength(IMAGE_E2E_PROTOCOLS.length);
    expect(ids).toEqual(expectedPrefixes);
  });

  it("are spelled the same way by the CLI that drives the run", () => {
    const ids = [...cliSource.matchAll(/`(image-e2e-[a-z_]+-)\$\{runId\}`/gu)].map(([, prefix]) => prefix);
    expect(ids).toHaveLength(IMAGE_E2E_PROTOCOLS.length);
    expect(ids).toEqual(expectedPrefixes);
  });
});
