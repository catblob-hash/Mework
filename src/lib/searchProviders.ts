import type { SearchCapability, SearchProviderKind } from "../types";

/**
 * The catalog declaration for one capability, mirroring Rust's `SearchCapabilitySpec`.
 *
 * An empty `defaultApiHost` means the capability needs no endpoint. Only `fetch` does this because it retrieves the target URL locally.
 */
export interface SearchCapabilitySpec {
  defaultApiHost: string;
  requiresApiKey: boolean;
}

export interface SearchProviderCatalogEntry {
  kind: SearchProviderKind;
  label: string;
  search: SearchCapabilitySpec | null;
  fetch: SearchCapabilitySpec | null;
}

/**
 * Mirrors Rust's `model.rs::SEARCH_PROVIDER_CATALOG`; the cross-language parity test requires both catalogs to change together.
 */
export const SEARCH_PROVIDERS: readonly SearchProviderCatalogEntry[] = [
  {
    kind: "zhipu",
    label: "Zhipu",
    search: { defaultApiHost: "https://open.bigmodel.cn/api/paas/v4/web_search", requiresApiKey: true },
    fetch: null
  },
  {
    kind: "tavily",
    label: "Tavily",
    search: { defaultApiHost: "https://api.tavily.com", requiresApiKey: true },
    fetch: null
  },
  {
    kind: "searxng",
    label: "Searxng",
    search: { defaultApiHost: "http://localhost:8080", requiresApiKey: false },
    fetch: null
  },
  {
    kind: "exa",
    label: "Exa",
    search: { defaultApiHost: "https://api.exa.ai", requiresApiKey: true },
    fetch: null
  },
  {
    kind: "exa-mcp",
    label: "ExaMCP",
    search: { defaultApiHost: "https://mcp.exa.ai/mcp", requiresApiKey: false },
    fetch: null
  },
  {
    kind: "bocha",
    label: "Bocha",
    search: { defaultApiHost: "https://api.bochaai.com", requiresApiKey: true },
    fetch: null
  },
  {
    kind: "querit",
    label: "Querit",
    search: { defaultApiHost: "https://api.querit.ai", requiresApiKey: true },
    fetch: { defaultApiHost: "https://api.querit.ai", requiresApiKey: true }
  },
  {
    kind: "fetch",
    label: "fetch",
    search: null,
    fetch: { defaultApiHost: "", requiresApiKey: false }
  },
  {
    kind: "jina",
    label: "Jina",
    search: { defaultApiHost: "https://s.jina.ai", requiresApiKey: true },
    fetch: { defaultApiHost: "https://r.jina.ai", requiresApiKey: false }
  },
  {
    kind: "firecrawl",
    label: "Firecrawl",
    search: { defaultApiHost: "https://api.firecrawl.dev", requiresApiKey: false },
    fetch: { defaultApiHost: "https://api.firecrawl.dev", requiresApiKey: false }
  }
];

export function isKnownSearchProvider(kind: string): kind is SearchProviderKind {
  return SEARCH_PROVIDERS.some((provider) => provider.kind === kind);
}

/**
 * Result-shaping bounds, mirroring Rust `model.rs`.
 *
 * Both settings read 0 as "no limit", so 0 is a legal value rather than the
 * bottom of a range: the ceiling is what a number input clamps to, and the
 * floor is 0 for both.
 */
export const DEFAULT_SEARCH_MAX_RESULTS = 5;
export const SEARCH_MAX_RESULTS_CEILING = 50;
export const DEFAULT_SEARCH_COMPRESSION_CUTOFF = 2000;
export const SEARCH_COMPRESSION_CUTOFF_CEILING = 200_000;

export function searchProviderEntry(kind: SearchProviderKind): SearchProviderCatalogEntry {
  const entry = SEARCH_PROVIDERS.find((provider) => provider.kind === kind);
  if (!entry) {
    throw new Error(`Unknown search provider: ${kind}`);
  }
  return entry;
}

export function searchProviderCapability(
  kind: SearchProviderKind,
  capability: SearchCapability
): SearchCapabilitySpec | null {
  const entry = searchProviderEntry(kind);
  return capability === "searchKeywords" ? entry.search : entry.fetch;
}

export function searchProviderSupports(kind: SearchProviderKind, capability: SearchCapability): boolean {
  return searchProviderCapability(kind, capability) !== null;
}

/** Whether this capability requires a usable HTTP(S) endpoint. */
export function capabilityNeedsApiHost(spec: SearchCapabilitySpec): boolean {
  return spec.defaultApiHost.length > 0;
}
