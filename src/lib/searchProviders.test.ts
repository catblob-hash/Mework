import { describe, expect, it } from "vitest";
import providerSource from "../../src-tauri/src/model.rs?raw";
import {
  capabilityNeedsApiHost,
  isKnownSearchProvider,
  SEARCH_PROVIDERS,
  searchProviderCapability,
  searchProviderSupports
} from "./searchProviders";

/**
 * The Rust table stores one row per line; this regex enforces that shape. Each
 * capability slot is `Some(SearchCapabilitySpec { … })` or `None`.
 */
const CATALOG_ROW =
  /SearchProviderCatalogEntry \{ kind: SearchProviderKind::\w+, slug: "([^"]+)", label: "([^"]+)", search: (None|Some\(SearchCapabilitySpec \{ default_api_host: "([^"]*)", requires_api_key: (true|false) \}\)), fetch: (None|Some\(SearchCapabilitySpec \{ default_api_host: "([^"]*)", requires_api_key: (true|false) \}\)) \}/g;

function capability(literal: string, host: string | undefined, key: string | undefined) {
  return literal === "None" ? null : { defaultApiHost: host ?? "", requiresApiKey: key === "true" };
}

describe("search provider catalog", () => {
  it("mirrors the Rust catalog exactly, including order", () => {
    const rustProviders = Array.from(providerSource.matchAll(CATALOG_ROW), (match) => ({
      kind: match[1],
      label: match[2],
      search: capability(match[3], match[4], match[5]),
      fetch: capability(match[6], match[7], match[8])
    }));
    expect(rustProviders).toHaveLength(10);
    expect(SEARCH_PROVIDERS).toEqual(rustProviders);
  });

  it("answers membership for catalog providers only", () => {
    for (const provider of SEARCH_PROVIDERS) {
      expect(isKnownSearchProvider(provider.kind)).toBe(true);
    }
    for (const retired of ["openai", "anthropic", "deepseek"]) {
      expect(isKnownSearchProvider(retired)).toBe(false);
    }
    expect(isKnownSearchProvider("not-a-provider")).toBe(false);
    expect(isKnownSearchProvider("Tavily")).toBe(false);
    expect(isKnownSearchProvider("")).toBe(false);
  });

  it("keeps every row usable: at least one capability, and only fetch is hostless", () => {
    for (const provider of SEARCH_PROVIDERS) {
      expect(provider.search ?? provider.fetch).not.toBeNull();
    }
    const hostless = SEARCH_PROVIDERS.filter((provider) =>
      [provider.search, provider.fetch].some((spec) => spec && !capabilityNeedsApiHost(spec))
    ).map((provider) => provider.kind);
    // Only fetch runs locally and has no third-party endpoint.
    expect(hostless).toEqual(["fetch"]);
  });

  it("routes each capability to its own endpoint", () => {
    // Jina's capabilities use separate hosts, so endpoints are resolved by
    // capability rather than a shared base URL.
    expect(searchProviderCapability("jina", "searchKeywords")?.defaultApiHost).toBe("https://s.jina.ai");
    expect(searchProviderCapability("jina", "fetchUrls")?.defaultApiHost).toBe("https://r.jina.ai");
    expect(searchProviderSupports("tavily", "fetchUrls")).toBe(false);
    expect(searchProviderSupports("fetch", "searchKeywords")).toBe(false);
    expect(searchProviderSupports("fetch", "fetchUrls")).toBe(true);
  });
});
