import { describe, expect, it } from "vitest";
import { toolCatalog } from "../seed";
import { hasEnglishToolDefault, localizeToolDescriptor } from "./toolDefaults";

describe("tool defaults", () => {
  it("does not mutate Chinese defaults or unknown third-party tools", () => {
    const tool = toolCatalog[0];
    expect(localizeToolDescriptor(tool, "zh-CN")).toBe(tool);
    expect(hasEnglishToolDefault(tool.name)).toBe(true);

    const unknown = { ...tool, name: "third_party" };
    expect(localizeToolDescriptor(unknown, "en-US")).toBe(unknown);
    expect(hasEnglishToolDefault(unknown.name)).toBe(false);
  });

  it("localizes the label and parameters but never introduces a tool description", () => {
    // Seed descriptions remain empty; localization must neither create nor display them.
    const tool = toolCatalog[0];
    const localized = localizeToolDescriptor(tool, "en-US");
    expect(localized.label).toBe("List files");
    expect(localized.description).toBe("");
  });
});
