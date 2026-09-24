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
    expect(marked("typescript", "  a + b;")).toEqual([["plain:  a + b;"]]);
  });

  it("marks a word directly before `(` as a call, and a capitalised one as a type", () => {
    expect(marked("typescript", "  foo(bar); new Map<Key>()")).toEqual([[
      "plain:  ",
      "function:foo",
      "plain:(bar); ",
      "keyword:new",
      "plain: ",
      "type:Map",
      "plain:<",
      "type:Key",
      "plain:>()"
    ]]);
    // A space between the name and the bracket is not a call.
    expect(marked("typescript", "if (x) y")).toEqual([["keyword:if", "plain: (x) y"]]);
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
      ["keyword:def", "plain: ", "function:f", "plain:():"],
      ["plain:    ", "string:\"\"\"one"],
      ["string:    two\"\"\""],
      ["plain:    ", "keyword:return", "plain: ", "number:1"]
    ]);
  });

  it("marks tags and their attributes in markup rather than reading `<` as an operator", () => {
    expect(marked("xml", "<div class=\"a\">text</div>")).toEqual([[
      "tag:<div",
      "plain: ",
      "attribute:class",
      "plain:=",
      "string:\"a\"",
      "tag:>",
      "plain:text",
      "tag:</div",
      "tag:>"
    ]]);
  });

  /** A tag whose attributes run onto the next line keeps reading them as attributes. */
  it("carries an open tag across lines", () => {
    expect(marked("xml", "<img\n  src=\"a.png\" />")).toEqual([
      ["tag:<img"],
      ["plain:  ", "attribute:src", "plain:=", "string:\"a.png\"", "plain: ", "tag:/>"]
    ]);
  });

  it("reads JSX only where an expression may start, and not a comparison or a type argument", () => {
    expect(marked("tsx", "return <Button onClick={() => go(1)} disabled>go</Button>;")).toEqual([[
      "keyword:return",
      "plain: ",
      "tag:<Button",
      "plain: ",
      "attribute:onClick",
      "plain:={() => ",
      "function:go",
      "plain:(",
      "number:1",
      "plain:)} ",
      "attribute:disabled",
      "tag:>",
      "plain:go",
      "tag:</Button",
      "tag:>",
      "plain:;"
    ]]);
    expect(marked("tsx", "a < b && list<Item>")).toEqual([[
      "plain:a < b && list<",
      "type:Item",
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
    expect(marked("css", "  color: #fff;")).toEqual([["plain:  ", "property:color", "plain:: ", "number:#fff", "plain:;"]]);
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

  it("tells a key from its value in configuration", () => {
    expect(marked("json", "{ \"name\": \"mework\", \"n\": 1 }")).toEqual([[
      "plain:{ ",
      "property:\"name\"",
      "plain:: ",
      "string:\"mework\"",
      "plain:, ",
      "property:\"n\"",
      "plain:: ",
      "number:1",
      "plain: }"
    ]]);
    expect(marked("yaml", "  - name: web # service")).toEqual([[
      "plain:  - ",
      "property:name",
      "plain:: web ",
      "comment:# service"
    ]]);
    expect(marked("toml", "[[bin]]\nname = \"x\"")).toEqual([
      ["type:[[bin]]"],
      ["property:name", "plain: = ", "string:\"x\""]
    ]);
  });

  it("marks decorators, directives, attributes and macros as meta", () => {
    expect(marked("python", "@dataclass(frozen=True)")).toEqual([[
      "meta:@dataclass",
      "plain:(frozen=",
      "keyword:True",
      "plain:)"
    ]]);
    expect(marked("c", "#include <stdio.h>")).toEqual([["meta:#include", "plain: ", "string:<stdio.h>"]]);
    expect(marked("rust", "#[derive(Debug)]\nprintln!(\"{}\", x);")).toEqual([
      ["meta:#[derive(Debug)]"],
      ["function:println!", "plain:(", "string:\"{}\"", "plain:, x);"]
    ]);
  });

  it("marks variables where a sigil introduces them", () => {
    expect(marked("shell", "echo \"$HOME\" ${PATH} $1")).toEqual([[
      "keyword:echo",
      "plain: ",
      "string:\"$HOME\"",
      "plain: ",
      "variable:${PATH}",
      "plain: ",
      "variable:$1"
    ]]);
    expect(marked("batch", "REM note\necho %PATH%")).toEqual([
      ["comment:REM note"],
      ["keyword:echo", "plain: ", "variable:%PATH%"]
    ]);
  });

  it("colours a diff by its lines", () => {
    expect(marked("diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@ fn\n-old\n+new\n same")).toEqual([
      ["meta:--- a/x"],
      ["meta:+++ b/x"],
      ["meta:@@ -1 +1 @@", "plain: fn"],
      ["deleted:-old"],
      ["inserted:+new"],
      ["plain: same"]
    ]);
  });

  it("marks a Markdown source's headings, fences and list markers", () => {
    expect(marked("markdown", "# Title\n- `a`\n```ts\nconst a\n```")).toEqual([
      ["keyword:# Title"],
      ["meta:-", "plain: ", "string:`a`"],
      ["string:```ts"],
      ["string:const a"],
      ["string:```"]
    ]);
  });

  it("says how a script runs on its first line, whatever the language", () => {
    expect(marked("javascript", "#!/usr/bin/env node\nlet a")).toEqual([
      ["meta:#!/usr/bin/env node"],
      ["keyword:let", "plain: a"]
    ]);
  });
});
