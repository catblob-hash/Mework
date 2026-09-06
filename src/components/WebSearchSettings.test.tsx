import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { SEARCH_PROVIDERS } from "../lib/searchProviders";
import type { WebSearchAssets } from "../types";

const searchMocks = vi.hoisted(() => ({
  deleteSearchApiKey: vi.fn(),
  getSearchKeyStatus: vi.fn(),
  revealSearchApiKey: vi.fn(),
  saveSearchApiKey: vi.fn()
}));
vi.mock("../lib/webSearch", () => searchMocks);
import { WebSearchSettings } from "./WebSearchSettings";

afterEach(() => configureI18n("zh-CN"));

const assets = (): WebSearchAssets => ({
  providers: SEARCH_PROVIDERS.map((provider) => ({
    kind: provider.kind,
    enabled: false,
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  })),
  fetchProvider: null,
  maxResults: 5,
  excludeDomains: [],
  compression: { method: "cutoff", cutoffLimit: 2000 }
});

function renderSettings(initial = assets()) {
  let current = initial;
  const onFlush = vi.fn(() => Promise.resolve());
  function Harness() {
    const [settings, setSettings] = useState(initial);
    current = settings;
    return <WebSearchSettings settings={settings} onFlush={onFlush} onChange={(change) => setSettings((value) => typeof change === "function" ? change(value) : change)} />;
  }
  return { ...render(<Harness />), getSettings: () => current, onFlush };
}

/** The General row is first in the rail, followed by provider rows. */
async function selectProvider(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByRole("button", { name: label }));
}

describe("WebSearchSettings", () => {
  beforeEach(() => {
    configureI18n("en-US");
    searchMocks.deleteSearchApiKey.mockReset().mockResolvedValue({ configured: false });
    searchMocks.getSearchKeyStatus.mockReset().mockResolvedValue({ configured: false });
    searchMocks.revealSearchApiKey.mockReset().mockRejectedValue(new Error("not configured"));
    searchMocks.saveSearchApiKey.mockReset().mockImplementation((_kind: string, _slot: string, secret: string) => Promise.resolve({ configured: true, keyLength: secret.length }));
  });

  it("lists the fixed Cherry-Studio catalog behind a General row", () => {
    const { container } = renderSettings();
    // The page uses the model-provider rail layout without a title card.
    expect(container.querySelector(".settings-rail-page > .provider-rail")).toBeInTheDocument();
    expect(container.querySelector(".provider-pane__body .provider-pane__stack")).toBeInTheDocument();
    expect(container.querySelector("h3")).toBeNull();
    expect(screen.getByRole("button", { name: "General" })).toBeInTheDocument();
    for (const provider of SEARCH_PROVIDERS) {
      expect(screen.getByRole("button", { name: provider.label })).toBeInTheDocument();
    }
    // The fixed catalog keeps the Add provider position disabled.
    expect(screen.getByRole("button", { name: "Add provider" })).toBeDisabled();
    // General owns the shared configuration controls.
    expect(screen.getByLabelText("Result count")).toHaveValue(5);
    expect(screen.getByLabelText("Fetch provider")).toHaveValue("");
    expect(screen.getByLabelText("Result compression")).toHaveValue("cutoff");
  });

  it("only offers fetch-capable providers as the fetch backend", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderSettings();
    const options = Array.from(
      screen.getByLabelText("Fetch provider").querySelectorAll("option")
    ).map((option) => option.getAttribute("value"));
    // Tavily supports search only and must not be offered as a fetch backend.
    expect(options).toEqual(["", "querit", "fetch", "jina", "firecrawl"]);

    await user.selectOptions(screen.getByLabelText("Fetch provider"), "jina");
    expect(getSettings().fetchProvider).toBe("jina");
  });

  it("keeps the global knobs bounded", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderSettings();
    await user.clear(screen.getByLabelText("Result count"));
    await user.type(screen.getByLabelText("Result count"), "8");
    expect(getSettings().maxResults).toBe(8);

    await user.type(screen.getByLabelText("Blocked domains"), "*://ads.example/*\n  \n/login$/");
    // Empty lines compile to rules that match nothing and are discarded.
    expect(getSettings().excludeDomains).toEqual(["*://ads.example/*", "/login$/"]);
  });

  // The enable switch exists only in the selected pane header; rail dots are status indicators.
  it("keeps the enable switch to the selected provider's pane header", async () => {
    const user = userEvent.setup();
    renderSettings();
    await selectProvider(user, "Tavily");
    expect(screen.getByRole("heading", { level: 1, name: "Tavily" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Enable search provider Tavily" })).toBeInTheDocument();
    for (const provider of SEARCH_PROVIDERS.filter((entry) => entry.kind !== "tavily")) {
      expect(screen.queryByRole("switch", { name: `Enable search provider ${provider.label}` })).toBeNull();
    }
  });

  it("updates a provider configuration and enablement", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderSettings();
    await selectProvider(user, "Tavily");
    await user.click(screen.getByRole("switch", { name: "Enable search provider Tavily" }));
    await user.type(screen.getByLabelText("Search endpoint"), "https://proxy.example/tavily");
    expect(getSettings().providers.find((provider) => provider.kind === "tavily")).toEqual({
      kind: "tavily",
      enabled: true,
      searchApiHost: "https://proxy.example/tavily",
      fetchApiHost: "",
      engines: [],
      basicAuthUsername: ""
    });
  });

  // Jina has separate search and fetch hosts; sharing a base URL routes fetches incorrectly.
  it("gives each capability its own endpoint field", async () => {
    const user = userEvent.setup();
    renderSettings();
    await selectProvider(user, "Jina");
    expect(screen.getByLabelText("Search endpoint")).toHaveAttribute("placeholder", "https://s.jina.ai");
    expect(screen.getByLabelText("Fetch endpoint")).toHaveAttribute("placeholder", "https://r.jina.ai");

    await selectProvider(user, "Tavily");
    expect(screen.queryByLabelText("Fetch endpoint")).toBeNull();
  });

  // The local fetch provider has neither endpoint nor credentials.
  it("shows no endpoint or credential for the local fetch provider", async () => {
    const user = userEvent.setup();
    renderSettings();
    await selectProvider(user, "fetch");
    expect(screen.queryByLabelText("Search endpoint")).toBeNull();
    expect(screen.queryByLabelText("Fetch endpoint")).toBeNull();
    expect(screen.queryByLabelText("API Key")).toBeNull();
  });

  // SearXNG uses an engine list and Basic Auth credentials instead of an API key.
  it("swaps the api key for engines and basic auth on searxng", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderSettings();
    await selectProvider(user, "Searxng");
    expect(screen.queryByLabelText("API Key")).toBeNull();
    expect(screen.getByLabelText("Basic auth password")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Search engines"), "google, brave");
    await user.type(screen.getByLabelText("Basic auth username"), "searx");
    const searxng = getSettings().providers.find((provider) => provider.kind === "searxng");
    expect(searxng?.engines).toEqual(["google", "brave"]);
    expect(searxng?.basicAuthUsername).toBe("searx");

    // The password uses the credential store path with a distinct slot name.
    await user.type(screen.getByLabelText("Basic auth password"), "hunter2");
    await user.tab();
    await waitFor(() => expect(searchMocks.saveSearchApiKey).toHaveBeenCalledWith("searxng", "basicAuthPassword", "hunter2"));
  });

  it("flushes before saving and deletes an emptied provider key", async () => {
    const user = userEvent.setup();
    const { onFlush } = renderSettings();
    await selectProvider(user, "Tavily");
    const key = screen.getByLabelText("API Key");
    await user.type(key, "provider-secret");
    await user.tab();
    await waitFor(() => expect(searchMocks.saveSearchApiKey).toHaveBeenCalledWith("tavily", "apiKey", "provider-secret"));
    expect(onFlush.mock.invocationCallOrder[0]).toBeLessThan(searchMocks.saveSearchApiKey.mock.invocationCallOrder[0]);
    await user.clear(key);
    await user.tab();
    await waitFor(() => expect(searchMocks.deleteSearchApiKey).toHaveBeenCalledWith("tavily", "apiKey"));
  });

  it("loads key status for the provider the user selects", async () => {
    const user = userEvent.setup();
    searchMocks.getSearchKeyStatus.mockImplementation((kind: string) => Promise.resolve(
      kind === "exa" ? { configured: true, keyLength: 9 } : { configured: false }
    ));
    renderSettings();
    await selectProvider(user, "Exa");
    await waitFor(() => expect(searchMocks.getSearchKeyStatus).toHaveBeenCalledWith("exa", "apiKey"));
    await waitFor(() => expect(screen.getByLabelText("API Key")).toHaveValue("•".repeat(9)));
  });

  // A stored secret must stay masked until the user asks for it by name. This is
  // the one assertion standing between "masked field" and "plaintext key sitting
  // in the DOM of a settings page nobody opened deliberately".
  it("reveals the stored key only when explicitly asked", async () => {
    const user = userEvent.setup();
    searchMocks.getSearchKeyStatus.mockResolvedValue({ configured: true, keyLength: 12 });
    searchMocks.revealSearchApiKey.mockResolvedValue("stored-provider-key");
    renderSettings();
    await selectProvider(user, "Tavily");

    const keyInput = screen.getByLabelText("API Key");
    await waitFor(() => expect(keyInput).toHaveValue("•".repeat(12)));
    expect(searchMocks.revealSearchApiKey).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Show API Key" }));
    await waitFor(() => expect(keyInput).toHaveValue("stored-provider-key"));
    expect(keyInput).toHaveAttribute("type", "text");

    await user.click(screen.getByRole("button", { name: "Hide API Key" }));
    await waitFor(() => expect(keyInput).toHaveAttribute("type", "password"));
    expect(keyInput).toHaveValue("•".repeat("stored-provider-key".length));
  });

  // An unconfigured provider must open an empty editable field without ever
  // calling reveal — the old code decided this by regex-matching the backend's
  // Chinese error text, which broke the moment that copy was reworded.
  it("opens an empty field for an unconfigured provider without asking to reveal", async () => {
    const user = userEvent.setup();
    searchMocks.getSearchKeyStatus.mockResolvedValue({ configured: false });
    renderSettings();
    await selectProvider(user, "Tavily");
    await user.click(screen.getByRole("button", { name: "Show API Key" }));
    await waitFor(() => expect(screen.getByLabelText("API Key")).toHaveValue(""));
    expect(searchMocks.revealSearchApiKey).not.toHaveBeenCalled();
  });
});
