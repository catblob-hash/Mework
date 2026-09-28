import { describe, expect, it } from "vitest";
import { toolCatalog } from "../seed";
import { localizeToolDescriptor } from "./toolDefaults";

describe("tool defaults", () => {
  it("does not mutate Chinese defaults or unknown third-party tools", () => {
    const tool = toolCatalog[0];
    expect(localizeToolDescriptor(tool, "zh-CN")).toBe(tool);

    const unknown = { ...tool, name: "third_party" };
    expect(localizeToolDescriptor(unknown, "en-US")).toBe(unknown);
  });

  it("localizes the label and parameters but never introduces a tool description", () => {
    // Seed descriptions remain empty; localization must neither create nor display them.
    const tool = toolCatalog[0];
    const localized = localizeToolDescriptor(tool, "en-US");
    expect(localized.label).toBe("List files");
    expect(localized.description).toBe("");
  });

  it("leaves no Chinese in any built-in tool under the English UI", () => {
    // The renderer's half of catalog.rs's `english_tool_catalog_localizes_every_visible_default…`.
    const han = /[㐀-䶿一-鿿豈-﫿]/;
    const leftovers: string[] = [];
    for (const tool of toolCatalog) {
      const localized = localizeToolDescriptor(tool, "en-US");
      if (han.test(localized.label)) leftovers.push(`${tool.name}: ${localized.label}`);
      for (const parameter of localized.parameters) {
        const where = `${tool.name}.${parameter.name}`;
        if (!parameter.label || han.test(parameter.label)) leftovers.push(`${where} label: ${parameter.label}`);
        if (parameter.help && han.test(parameter.help)) leftovers.push(`${where} help: ${parameter.help}`);
        if (parameter.placeholder && han.test(parameter.placeholder)) {
          leftovers.push(`${where} placeholder: ${parameter.placeholder}`);
        }
      }
    }
    expect(leftovers).toEqual([]);
  });
});
