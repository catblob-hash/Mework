import { beforeEach, describe, expect, it, vi } from "vitest";

const backendMocks = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => true),
  invoke: vi.fn()
}));

vi.mock("./backend", () => ({
  hasBackendRuntime: backendMocks.hasBackendRuntime,
  invoke: backendMocks.invoke
}));

import {
  deleteSearchApiKey,
  getSearchKeyStatus,
  revealSearchApiKey,
  saveSearchApiKey
} from "./webSearch";

describe("web search credentials", () => {
  beforeEach(() => {
    backendMocks.hasBackendRuntime.mockReturnValue(true);
    backendMocks.invoke.mockReset();
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  it("sends only the catalog id, the slot name and the secret to credential commands", async () => {
    backendMocks.invoke
      .mockResolvedValueOnce({ configured: true, keyLength: 12 })
      .mockResolvedValueOnce({ configured: true, keyLength: 12 })
      .mockResolvedValueOnce("secret-value")
      .mockResolvedValueOnce({ configured: false });

    await expect(getSearchKeyStatus("tavily")).resolves.toMatchObject({ configured: true });
    await expect(saveSearchApiKey("tavily", "apiKey", " secret-value ")).resolves.toEqual({
      configured: true,
      keyLength: 12
    });
    await expect(revealSearchApiKey("tavily")).resolves.toBe("secret-value");
    await expect(deleteSearchApiKey("tavily")).resolves.toEqual({
      configured: false,
      keyLength: undefined
    });

    // The endpoint is not part of the credential identity: changing an API host
    // must not orphan a saved key.
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(1, "get_search_key_status", {
      providerKind: "tavily",
      slot: "apiKey"
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(2, "save_search_api_key", {
      providerKind: "tavily",
      slot: "apiKey",
      apiKey: "secret-value"
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(3, "reveal_search_api_key", {
      providerKind: "tavily",
      slot: "apiKey"
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(4, "delete_search_api_key", {
      providerKind: "tavily",
      slot: "apiKey"
    });
    expect(Object.values(window.localStorage)).not.toContain("secret-value");
  });

  it("keeps the two slots of one provider apart", async () => {
    backendMocks.invoke
      .mockResolvedValueOnce({ configured: true, keyLength: 6 })
      .mockResolvedValueOnce({ configured: true, keyLength: 8 });

    await saveSearchApiKey("searxng", "apiKey", "keykey");
    await saveSearchApiKey("searxng", "basicAuthPassword", "password");

    expect(backendMocks.invoke).toHaveBeenNthCalledWith(2, "save_search_api_key", {
      providerKind: "searxng",
      slot: "basicAuthPassword",
      apiKey: "password"
    });
    // Each slot stores its own length; sharing a cache key would render the mask with the wrong width.
    expect(Object.keys(window.localStorage)).toHaveLength(2);
  });

  it("does not retain plaintext in browser preview", async () => {
    backendMocks.hasBackendRuntime.mockReturnValue(false);

    await expect(saveSearchApiKey("exa", "apiKey", "preview-secret")).resolves.toEqual({
      configured: true,
      keyLength: 14
    });
    await expect(getSearchKeyStatus("exa")).resolves.toEqual({
      configured: true,
      keyLength: 14
    });
    await expect(revealSearchApiKey("exa")).rejects.toThrow("浏览器预览不会保留凭据明文");

    expect(Object.values(window.localStorage)).not.toContain("preview-secret");
    expect(Object.values(window.sessionStorage)).not.toContain("preview-secret");
  });
});
