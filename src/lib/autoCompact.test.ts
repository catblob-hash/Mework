import { describe, expect, it } from "vitest";
import type { ModelProfile } from "../types";
import {
  AUTO_COMPACT_BUFFER_TOKENS,
  OUTPUT_RESERVE_CAP_TOKENS,
  autoCompactThreshold,
  shouldAutoCompact
} from "./autoCompact";

/** Only the two fields the threshold reads. */
function model(contextWindow?: number, maxOutputTokens?: number): ModelProfile {
  return { id: "m", name: "m", contextWindow, maxOutputTokens } as ModelProfile;
}

describe("autoCompactThreshold", () => {
  it("subtracts the output reserve and the buffer from the window", () => {
    expect(autoCompactThreshold(model(200_000, 8_000))).toBe(200_000 - 8_000 - AUTO_COMPACT_BUFFER_TOKENS);
  });

  it("caps the output reserve so a large ceiling cannot eat the window", () => {
    expect(autoCompactThreshold(model(200_000, 128_000)))
      .toBe(200_000 - OUTPUT_RESERVE_CAP_TOKENS - AUTO_COMPACT_BUFFER_TOKENS);
  });

  it("reserves the cap when the model declares no output ceiling", () => {
    expect(autoCompactThreshold(model(200_000)))
      .toBe(200_000 - OUTPUT_RESERVE_CAP_TOKENS - AUTO_COMPACT_BUFFER_TOKENS);
  });

  it("returns null when the context window is unset, zero or absent", () => {
    expect(autoCompactThreshold(model(undefined, 8_000))).toBeNull();
    expect(autoCompactThreshold(model(0, 8_000))).toBeNull();
    expect(autoCompactThreshold(null)).toBeNull();
    expect(autoCompactThreshold(undefined)).toBeNull();
  });

  it("returns null when the reserves consume the whole window", () => {
    expect(autoCompactThreshold(model(OUTPUT_RESERVE_CAP_TOKENS + AUTO_COMPACT_BUFFER_TOKENS))).toBeNull();
    expect(autoCompactThreshold(model(1_000))).toBeNull();
  });
});

describe("shouldAutoCompact", () => {
  it("fires once the free space the buffer protected is gone", () => {
    const profile = model(200_000, 8_000);
    const threshold = autoCompactThreshold(profile)!;
    expect(shouldAutoCompact(profile, threshold - 1)).toBe(false);
    expect(shouldAutoCompact(profile, threshold)).toBe(true);
    expect(shouldAutoCompact(profile, threshold + 50_000)).toBe(true);
  });

  it("never fires for a model with no declared window, however full it is", () => {
    expect(shouldAutoCompact(model(undefined), 10_000_000)).toBe(false);
  });
});
