import { describe, expect, it } from "vitest";
import { mergeModelProfiles } from "./ProviderSettings";
import type { ModelProfile } from "../types";

function model(overrides: Partial<ModelProfile> & { id: string }): ModelProfile {
  return {
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: "plaintext",
    promptCache: true,
    ...overrides
  };
}

describe("mergeModelProfiles", () => {
  it("adds models the provider did not have yet, in discovery order", () => {
    const merged = mergeModelProfiles(
      [model({ id: "already-here" })],
      [model({ id: "b-new" }), model({ id: "a-new" })]
    );
    expect(merged.map((entry) => entry.id)).toEqual(["already-here", "b-new", "a-new"]);
  });

  it("keeps what the user curated on a model that already exists", () => {
    const merged = mergeModelProfiles(
      [model({
        id: "gpt-4o",
        name: "我的叫法",
        group: "自定义组",
        capabilities: ["image_recognition"],
        reasoningContent: "encrypted",
        // `false` is a curated choice, not an empty value: discovery must not
        // flip it back to the default it writes for new models.
        promptCache: false
      })],
      [model({ id: "gpt-4o", name: "GPT-4o", group: "gpt", capabilities: [] })]
    );
    expect(merged).toHaveLength(1);
    expect(merged[0].name).toBe("我的叫法");
    expect(merged[0].group).toBe("自定义组");
    expect(merged[0].capabilities).toEqual(["image_recognition"]);
    expect(merged[0].reasoningContent).toBe("encrypted");
    expect(merged[0].promptCache).toBe(false);
  });

  it("lets an empty curated value yield to discovery", () => {
    // Empty strings and arrays are uncurated values, so discovery must fill them.
    const merged = mergeModelProfiles(
      [model({ id: "gpt-4o" })],
      [
        model({
          id: "gpt-4o",
          name: "GPT-4o",
          group: "gpt",
          capabilities: ["image_recognition"],
          contextWindow: 128000
        })
      ]
    );
    expect(merged[0].name).toBe("GPT-4o");
    expect(merged[0].group).toBe("gpt");
    expect(merged[0].capabilities).toEqual(["image_recognition"]);
    expect(merged[0].contextWindow).toBe(128000);
  });

  it("trims a discovered id instead of installing a second entry for it", () => {
    const merged = mergeModelProfiles(
      [model({ id: "gpt-4o", name: "我的叫法" })],
      [model({ id: "  gpt-4o  ", name: "GPT-4o" })]
    );
    expect(merged).toHaveLength(1);
    expect(merged[0].id).toBe("gpt-4o");
    expect(merged[0].name).toBe("我的叫法");
  });

  it("drops a discovered capability slug that is no longer in the catalog", () => {
    // An archived catalog response still names retired capabilities; installing
    // one must not write it into the provider.
    const stale = {
      ...model({ id: "gpt-4o" }),
      capabilities: ["function_call", "image_recognition"]
    } as unknown as ModelProfile;
    expect(mergeModelProfiles([], [stale])[0].capabilities).toEqual(["image_recognition"]);
  });

  it("ignores a discovered entry with a blank id", () => {
    const merged = mergeModelProfiles([], [model({ id: "   " })]);
    expect(merged).toEqual([]);
  });
});
