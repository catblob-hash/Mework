import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { SEARCH_PROVIDERS } from "../lib/searchProviders";
import { defaultConversationWebSearchSettings } from "../lib/runtime";
import type { ConversationWebSearchSettings, WebSearchAssets } from "../types";
import { WebSearchBehaviorSettings } from "./WebSearchBehaviorSettings";

afterEach(() => configureI18n("zh-CN"));

/** Jina searches and fetches; Tavily only searches; Exa is left switched off. */
const assets = (): WebSearchAssets => ({
  providers: SEARCH_PROVIDERS.map((provider) => ({
    kind: provider.kind,
    enabled: provider.kind === "jina" || provider.kind === "tavily",
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  }))
});

function renderBehavior(initial: Partial<ConversationWebSearchSettings> = {}) {
  const onChange = vi.fn();
  const start = { ...defaultConversationWebSearchSettings(), ...initial };
  function Harness() {
    const [value, setValue] = useState(start);
    return (
      <div className="web-search-provider-field">
        <WebSearchBehaviorSettings
          value={value}
          webSearchAssets={assets()}
          onChange={(patch) => {
            onChange(patch);
            setValue((current) => ({ ...current, ...patch }));
          }}
        />
      </div>
    );
  }
  render(<Harness />);
  return { onChange, lastPatch: () => onChange.mock.calls.at(-1)?.[0] };
}

async function openMenu(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByRole("button", { name: new RegExp(`^${label}：`) }));
  return screen.getByRole("menu", { name: label });
}

describe("WebSearchBehaviorSettings", () => {
  beforeEach(() => configureI18n("zh-CN"));

  /* A provider that is switched off is not a choice. Offering it greyed out
     would make the menu a list of things that do not work, and the global page
     is where a provider is switched on. */
  it("lists only enabled providers, and names the native row without a parenthetical", async () => {
    const user = userEvent.setup();
    renderBehavior();

    const search = await openMenu(user, "搜索提供商");
    expect(within(search).getByRole("menuitemradio", { name: "原生" })).toBeInTheDocument();
    expect(within(search).getByRole("menuitemradio", { name: "Tavily" })).toBeInTheDocument();
    expect(within(search).queryByRole("menuitemradio", { name: /Exa/ })).not.toBeInTheDocument();
    await user.keyboard("{Escape}");

    const fetch = await openMenu(user, "抓取提供商");
    expect(within(fetch).getByRole("menuitemradio", { name: "原生" })).toBeInTheDocument();
    expect(within(fetch).getByRole("menuitemradio", { name: "Jina" })).toBeInTheDocument();
    // Tavily searches but cannot fetch, so it is not a fetch backend at all.
    expect(within(fetch).queryByRole("menuitemradio", { name: "Tavily" })).not.toBeInTheDocument();
    // Nothing resolves to a backend chosen elsewhere.
    expect(within(fetch).queryByRole("menuitemradio", { name: /自动/ })).not.toBeInTheDocument();
  });

  it("turns either leg off on its own", async () => {
    const user = userEvent.setup();
    const { lastPatch } = renderBehavior();

    const search = await openMenu(user, "搜索提供商");
    await user.click(within(search).getByRole("menuitemradio", { name: "不启用" }));
    expect(lastPatch()).toEqual({ provider: { kind: "disabled" } });

    const fetch = await openMenu(user, "抓取提供商");
    await user.click(within(fetch).getByRole("menuitemradio", { name: "不启用" }));
    expect(lastPatch()).toEqual({ fetchProvider: { kind: "disabled" } });
  });

  it("takes the two result-shaping numbers, with 0 as a legal answer", async () => {
    const user = userEvent.setup();
    const { lastPatch } = renderBehavior();

    const count = screen.getByRole("spinbutton", { name: "结果数" });
    expect(count).toHaveValue(5);
    await user.clear(count);
    await user.type(count, "12");
    expect(lastPatch()).toEqual({ maxResults: 12 });

    const compression = screen.getByRole("spinbutton", { name: "结果压缩" });
    expect(compression).toHaveValue(2000);
    await user.clear(compression);
    await user.type(compression, "0");
    expect(lastPatch()).toEqual({ compressionCutoff: 0 });
  });

  /* The row is a choice between the two lists rather than a switch on each: a
     result admitted by one and refused by the other has no obvious answer. */
  it("selects which domain list is in effect without emptying either", async () => {
    const user = userEvent.setup();
    const { lastPatch } = renderBehavior({ excludeDomains: ["*://ads.example/*"] });

    const filter = screen.getByRole("combobox", { name: "域名过滤" });
    expect(filter).toHaveValue("off");
    await user.selectOptions(filter, "include");
    expect(lastPatch()).toEqual({ domainFilter: "include" });
    await user.selectOptions(filter, "off");
    expect(lastPatch()).toEqual({ domainFilter: "off" });
  });

  it("writes each list on its own page of the rules window", async () => {
    const user = userEvent.setup();
    const { lastPatch } = renderBehavior();

    await user.click(screen.getByRole("button", { name: "编辑名单" }));
    const dialog = screen.getByRole("dialog", { name: "域名名单" });

    // With filtering off the window opens on the blocklist, the list a person
    // reaching for this row almost always means.
    await user.type(
      within(dialog).getByRole("textbox", { name: "黑名单" }),
      "*://ads.example/*"
    );
    expect(lastPatch()).toEqual({ excludeDomains: ["*://ads.example/*"] });

    await user.click(within(dialog).getByRole("button", { name: /白名单/ }));
    await user.type(
      within(dialog).getByRole("textbox", { name: "白名单" }),
      "*://docs.example/*"
    );
    expect(lastPatch()).toEqual({ includeDomains: ["*://docs.example/*"] });
  });
});
