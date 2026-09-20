import { describe, expect, it } from "vitest";
import type {
  ConversationWebSearchSettings,
  SearchProviderKind,
  WebSearchAssets
} from "../types";
import { familySelectsNativeToolType, grantsWebFetch, grantsWebSearch } from "./webSearch";

/**
 * `grantsWebFetch` / `grantsWebSearch` mirror the two halves of Rust
 * `WebSearchSettings::effective`. These cases are the same ones `model.rs`
 * asserts, so a one-sided change to either shows up as a disagreement rather
 * than as a quietly wrong tool lock.
 */
function assets(patch: Partial<WebSearchAssets> = {}): WebSearchAssets {
  const enabled: SearchProviderKind[] = ["tavily", "jina", "firecrawl", "exa"];
  return {
    providers: enabled.map((kind) => ({
      kind,
      enabled: true,
      searchApiHost: "",
      fetchApiHost: "",
      engines: [],
      basicAuthUsername: ""
    })),
    ...patch
  };
}

function conversation(
  patch: Partial<ConversationWebSearchSettings> = {}
): ConversationWebSearchSettings {
  return {
    maxSearchesPerCall: 0,
    provider: { kind: "native" },
    fetchProvider: { kind: "native" },
    nativeSearchTool: "web_search_20250305",
    nativeFetchTool: "web_fetch_20250910",
    maxResults: 5,
    compressionCutoff: 2000,
    domainFilter: "off",
    includeDomains: [],
    excludeDomains: [],
    ...patch
  };
}

function withProvider(kind: SearchProviderKind, enabled: boolean): WebSearchAssets {
  return assets({
    providers: assets().providers.map((provider) => (
      provider.kind === kind ? { ...provider, enabled } : provider
    ))
  });
}

describe("grantsWebFetch", () => {
  it("withholds fetching from a conversation with no web access at all", () => {
    expect(grantsWebFetch(false, conversation(), assets(), "anthropic")).toBe(false);
  });

  it("reads a native selection off the family, not off the provider catalog", () => {
    expect(grantsWebFetch(true, conversation(), assets(), "anthropic")).toBe(true);
    // A family that fetches inside its one search tool grants no second tool,
    // whatever the catalog could have lent it.
    expect(grantsWebFetch(true, conversation(), assets(), "openai_responses"))
      .toBe(false);
  });

  it("refuses an explicit provider that cannot fetch or is switched off", () => {
    const jina = conversation({ fetchProvider: { kind: "explicit", providerKind: "jina" } });
    expect(grantsWebFetch(true, jina, assets(), "openai_responses")).toBe(true);
    // Tavily searches only.
    const tavily = conversation({ fetchProvider: { kind: "explicit", providerKind: "tavily" } });
    expect(grantsWebFetch(true, tavily, assets(), "anthropic")).toBe(false);
    expect(grantsWebFetch(true, jina, withProvider("jina", false), "openai_responses"))
      .toBe(false);
  });

  it("answers a fetch selection on its own, whatever the search backend is", () => {
    // The selection names its own backend, so a family with no native fetch
    // tool can still fetch through a provider that has one.
    const firecrawl = conversation({ fetchProvider: { kind: "explicit", providerKind: "firecrawl" } });
    expect(grantsWebFetch(true, firecrawl, assets(), "openai_responses")).toBe(true);
    // And switching fetching off holds even when the search backend fetches for
    // itself, because what searches has no say in what fetches.
    const off = conversation({
      provider: { kind: "explicit", providerKind: "firecrawl" },
      fetchProvider: { kind: "disabled" }
    });
    expect(grantsWebFetch(true, off, assets(), "anthropic")).toBe(false);
  });

  it("grants nothing at all when the conversation switched fetching off", () => {
    const off = conversation({ fetchProvider: { kind: "disabled" } });
    expect(grantsWebFetch(true, off, assets(), "anthropic")).toBe(false);
  });
});

describe("grantsWebSearch", () => {
  it("withholds searching from a conversation with no web access at all", () => {
    expect(grantsWebSearch(false, conversation(), assets())).toBe(false);
  });

  it("grants nothing when the conversation switched searching off or lost its backend", () => {
    expect(grantsWebSearch(true, conversation({ provider: { kind: "disabled" } }), assets()))
      .toBe(false);
    expect(grantsWebSearch(true, conversation({ provider: { kind: "unavailable" } }), assets()))
      .toBe(false);
  });

  it("grants the native leg without consulting the provider catalog", () => {
    expect(grantsWebSearch(true, conversation(), assets())).toBe(true);
  });

  it("refuses an explicit provider that cannot search or is switched off", () => {
    const jina = conversation({ provider: { kind: "explicit", providerKind: "jina" } });
    expect(grantsWebSearch(true, jina, assets())).toBe(true);
    // The `fetch` provider has no search capability at all.
    const fetchOnly = conversation({ provider: { kind: "explicit", providerKind: "fetch" } });
    expect(grantsWebSearch(true, fetchOnly, assets())).toBe(false);
    expect(grantsWebSearch(true, jina, withProvider("jina", false))).toBe(false);
  });
});

/* The version of a native web tool is a Messages spelling. Every family that
   has native web tools has exactly one shape for them; only Messages writes the
   version into the request, so only there is there a choice to offer. */
describe("familySelectsNativeToolType", () => {
  it("is true only of the families that speak Messages", () => {
    expect(familySelectsNativeToolType("anthropic")).toBe(true);
    expect(familySelectsNativeToolType("bedrock")).toBe(true);
    // Responses searches natively and still has no version to name, which is
    // why this is its own table rather than the native-search one.
    expect(familySelectsNativeToolType("openai_responses")).toBe(false);
    expect(familySelectsNativeToolType("google")).toBe(false);
    expect(familySelectsNativeToolType("xai")).toBe(false);
    // The agent family makes its own Messages calls inside the CLI, so the host
    // never writes a tool definition for a version to ride on.
    expect(familySelectsNativeToolType("claude_agent")).toBe(false);
    expect(familySelectsNativeToolType(undefined)).toBe(false);
  });
});
