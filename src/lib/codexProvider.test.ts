import { describe, expect, it } from "vitest";
import type { ApiProvider } from "../types";
import {
  CODEX_PROVIDER_FAMILY,
  CODEX_PROVIDER_NAME,
  ensureCodexProvider,
  isBuiltinProvider,
  isCodexProvider,
} from "./codexProvider";

function provider(id: string, overrides: Partial<ApiProvider> = {}): ApiProvider {
  return {
    id,
    name: id,
    enabled: true,
    family: "openai_chat",
    baseUrl: "https://example.test/v1",
    familySettings: {},
    endpointBaseUrls: {},
    notes: "",
    models: [],
    activeModelId: null,
    ...overrides,
  };
}

describe("ensureCodexProvider", () => {
  it("appends a disabled Codex row when missing", () => {
    const result = ensureCodexProvider([provider("custom")]);

    expect(result).toHaveLength(2);
    expect(result[1]).toMatchObject({
      name: CODEX_PROVIDER_NAME,
      enabled: false,
      family: CODEX_PROVIDER_FAMILY,
      baseUrl: "",
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      models: [],
      activeModelId: null,
    });
    expect(result[1].id).toMatch(/^provider_/u);
    expect(isCodexProvider(result[1])).toBe(true);
    expect(isBuiltinProvider(result[1])).toBe(true);
  });

  it("is idempotent when a Codex row already exists", () => {
    const existing = provider("codex-id", { family: CODEX_PROVIDER_FAMILY, enabled: false });
    const once = ensureCodexProvider([provider("custom"), existing]);

    expect(ensureCodexProvider(once)).toEqual(once);
    expect(once[1].id).toBe("codex-id");
  });

  it("keeps the first duplicate Codex row and drops later duplicates", () => {
    const first = provider("first", { family: CODEX_PROVIDER_FAMILY });
    const later = provider("later", { family: CODEX_PROVIDER_FAMILY });

    expect(ensureCodexProvider([first, provider("custom"), later])).toEqual([first, provider("custom")]);
  });

  it("never reorders existing providers", () => {
    const alpha = provider("alpha");
    const beta = provider("beta");
    const codex = provider("codex", { family: CODEX_PROVIDER_FAMILY });

    expect(ensureCodexProvider([alpha, codex, beta])).toEqual([alpha, codex, beta]);
    expect(ensureCodexProvider([alpha, beta]).slice(0, 2)).toEqual([alpha, beta]);
  });
});
