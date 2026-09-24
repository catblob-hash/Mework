import { describe, expect, it } from "vitest";
import { splitFrontMatter } from "./frontMatter";

describe("splitFrontMatter", () => {
  it("takes a YAML or TOML block off the top of a document", () => {
    expect(splitFrontMatter("---\ntitle: A\ntags: [x]\n---\n# Body\n")).toEqual({
      language: "yaml",
      source: "title: A\ntags: [x]",
      body: "# Body\n"
    });
    expect(splitFrontMatter("+++\r\ntitle = \"A\"\r\n+++\r\ntext")).toEqual({ language: "toml", source: "title = \"A\"", body: "text" });
  });

  it("leaves a document that only has a rule alone", () => {
    expect(splitFrontMatter("# Title\n\n---\n\ntext")).toBe(null);
    expect(splitFrontMatter("---\nno closing fence")).toBe(null);
  });
});
