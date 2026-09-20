import { describe, expect, it } from "vitest";
import { grammarFor, highlightCodeLine, highlightCodeLines } from "./codeHighlight";

/** `kind:value` for each token, which is short enough to read as an expectation. */
function marked(language: string, source: string): string[][] {
  return highlightCodeLines(language, source.split("\n"))
    .map((tokens) => tokens.map((token) => `${token.kind}:${token.value}`));
}

describe("highlightCodeLine", () => {
  it("marks comments, strings, numbers, and reserved words apart from the rest", () => {
    expect(marked("typescript", "const n = 42; // why")).toEqual([[
      "keyword:const",
      "plain: n = ",
      "number:42",
      "plain:; ",
      "comment:// why"
    ]]);
  });

  it("leaves a line with nothing to mark as a single plain token", () => {
    expect(marked("typescript", "  foo(bar);")).toEqual([["plain:  foo(bar);"]]);
  });

  /** A block comment is the one thing on a line that outlives the line. */
  it("carries a block comment across lines and closes it exactly once", () => {
    expect(marked("rust", "let a = 1; /* start\nstill comment\nend */ let b = 2;")).toEqual([
      ["keyword:let", "plain: a = ", "number:1", "plain:; ", "comment:/* start"],
      ["comment:still comment"],
      ["comment:end */", "plain: ", "keyword:let", "plain: b = ", "number:2", "plain:;"]
    ]);
  });

  it("carries a template literal across lines but never an ordinary quote", () => {
    expect(marked("typescript", "const a = `one\ntwo`;")).toEqual([
      ["keyword:const", "plain: a = ", "string:`one"],
      ["string:two`", "plain:;"]
    ]);
    // An unterminated single-line literal is a typo, not a continuation: the
    // next line must start clean rather than colouring the rest of the file.
    expect(marked("typescript", "const a = \"oops\nconst b = 1;")).toEqual([
      ["keyword:const", "plain: a = ", "string:\"oops"],
      ["keyword:const", "plain: b = ", "number:1", "plain:;"]
    ]);
  });

  it("does not let an escaped quote close a string", () => {
    expect(marked("typescript", "const a = \"a\\\"b\"; const c = 1;")).toEqual([[
      "keyword:const",
      "plain: a = ",
      "string:\"a\\\"b\"",
      "plain:; ",
      "keyword:const",
      "plain: c = ",
      "number:1",
      "plain:;"
    ]]);
  });

  /** A shell single-quoted string takes no escapes at all. */
  it("honours a grammar that says a backslash is not an escape", () => {
    expect(marked("shell", "echo 'a\\' b")).toEqual([[
      "keyword:echo",
      "plain: ",
      "string:'a\\'",
      "plain: b"
    ]]);
  });

  it("keeps a docstring open to its closing triple quote", () => {
    expect(marked("python", "def f():\n    \"\"\"one\n    two\"\"\"\n    return 1")).toEqual([
      ["keyword:def", "plain: f():"],
      ["plain:    ", "string:\"\"\"one"],
      ["string:    two\"\"\""],
      ["plain:    ", "keyword:return", "plain: ", "number:1"]
    ]);
  });

  it("marks tag names in markup rather than reading `<` as an operator", () => {
    expect(marked("xml", "<div class=\"a\">text</div>")).toEqual([[
      "keyword:<div",
      "plain: class=",
      "string:\"a\"",
      "plain:>text",
      "keyword:</div",
      "plain:>"
    ]]);
  });

  it("reserves SQL words whatever their case, and nothing else's", () => {
    expect(marked("sql", "select * from t")).toEqual([[
      "keyword:select",
      "plain: * ",
      "keyword:from",
      "plain: t"
    ]]);
    // Case folding is SQL's rule alone; a TypeScript identifier is not a keyword
    // because its uppercase form happens to be one.
    expect(marked("typescript", "const CONST = 1;")).toEqual([[
      "keyword:const",
      "plain: CONST = ",
      "number:1",
      "plain:;"
    ]]);
  });

  /** A word boundary the grammar widened must not swallow arithmetic elsewhere. */
  it("widens words only where the grammar asks for it", () => {
    expect(marked("css", "@media (min-width: 40px) { color: red; }")).toEqual([[
      "keyword:@media",
      "plain: (min-width: ",
      "number:40",
      "plain:px) { color: red; }"
    ]]);
    expect(marked("typescript", "count-1")).toEqual([["plain:count-", "number:1"]]);
  });

  it("shows a file it has no grammar for as plain text", () => {
    expect(grammarFor("klingon")).toBe(null);
    expect(marked("klingon", "anything at all")).toEqual([["plain:anything at all"]]);
    expect(highlightCodeLine(null, "// not a comment here", null)).toEqual({
      tokens: [{ kind: "plain", value: "// not a comment here" }],
      block: null
    });
  });

  it("returns no tokens for an empty line, and keeps the block it was in", () => {
    const grammar = grammarFor("rust");
    expect(highlightCodeLine(grammar, "", null)).toEqual({ tokens: [], block: null });
    const opened = highlightCodeLine(grammar, "/* open", null);
    expect(highlightCodeLine(grammar, "", opened.block).block).toEqual(opened.block);
  });
});
