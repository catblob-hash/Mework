import { describe, expect, it } from "vitest";
import { tokenizeLine, wordDiff } from "./wordDiff";

/** The two invariants every wordDiff result must hold, on both sides. */
function expectSound(result: ReturnType<typeof wordDiff>, before: string, after: string): void {
  expect(result.before.map((segment) => segment.text).join("")).toBe(before);
  expect(result.after.map((segment) => segment.text).join("")).toBe(after);
  for (const side of [result.before, result.after]) {
    for (let index = 1; index < side.length; index += 1) {
      expect(side[index].changed).not.toBe(side[index - 1].changed);
    }
  }
}

describe("tokenizeLine", () => {
  it("round-trips a variety of lines exactly", () => {
    const lines = [
      "const value = compute(42);",
      "\tindented\tline\t",
      "文件名和 café",
      "snake_case $dollar 123",
      "punct! (a) [b] {c} #d?",
      "emoji 🎉 outside the BMP",
      ""
    ];
    for (const line of lines) {
      expect(tokenizeLine(line).join("")).toBe(line);
    }
  });

  it("keeps a CJK run as one token", () => {
    expect(tokenizeLine("文件名")).toEqual(["文件名"]);
  });

  it("keeps accented Latin letters in one token", () => {
    expect(tokenizeLine("café")).toEqual(["café"]);
  });

  it("splits punctuation into single-character tokens", () => {
    expect(tokenizeLine("a.b")).toEqual(["a", ".", "b"]);
  });

  it("keeps a run of spaces as one token", () => {
    expect(tokenizeLine("a    b")).toEqual(["a", "    ", "b"]);
  });

  it("keeps a surrogate pair whole", () => {
    const tokens = tokenizeLine("x🎉y");
    expect(tokens).toEqual(["x", "🎉", "y"]);
    expect(tokens[1]).toHaveLength(2);
  });
});

describe("wordDiff", () => {
  it("aligns a one-word substitution as three segments per side", () => {
    const before = "const value = compute();";
    const after = "const result = compute();";
    const result = wordDiff(before, after);

    expect(result.bailed).toBe(false);
    expect(result.before).toEqual([
      { text: "const ", changed: false },
      { text: "value", changed: true },
      { text: " = compute();", changed: false }
    ]);
    expect(result.after).toEqual([
      { text: "const ", changed: false },
      { text: "result", changed: true },
      { text: " = compute();", changed: false }
    ]);
  });

  it("marks an insertion at the start as a changed prefix on the after side", () => {
    const result = wordDiff("world", "hello world");

    expect(result.before).toEqual([{ text: "world", changed: false }]);
    expect(result.after).toEqual([
      { text: "hello ", changed: true },
      { text: "world", changed: false }
    ]);
  });

  it("marks an insertion at the end as a changed suffix on the after side", () => {
    const result = wordDiff("hello", "hello world");

    expect(result.before).toEqual([{ text: "hello", changed: false }]);
    expect(result.after).toEqual([
      { text: "hello", changed: false },
      { text: " world", changed: true }
    ]);
  });

  it("marks an insertion in the middle on the after side", () => {
    const result = wordDiff("a c", "a b c");

    expect(result.before).toEqual([{ text: "a c", changed: false }]);
    expect(result.after).toEqual([
      { text: "a ", changed: false },
      { text: "b ", changed: true },
      { text: "c", changed: false }
    ]);
  });

  it("marks a deletion as the mirror of an insertion", () => {
    const result = wordDiff("a b c", "a c");

    expect(result.before).toEqual([
      { text: "a ", changed: false },
      { text: "b ", changed: true },
      { text: "c", changed: false }
    ]);
    expect(result.after).toEqual([{ text: "a c", changed: false }]);
  });

  it("reports identical lines as one unchanged segment per side", () => {
    const result = wordDiff("same line", "same line");

    expect(result.bailed).toBe(false);
    expect(result.before).toEqual([{ text: "same line", changed: false }]);
    expect(result.after).toEqual([{ text: "same line", changed: false }]);
  });

  it("reports an empty before side with one wholly changed after segment", () => {
    const result = wordDiff("", "hello world");

    expect(result.bailed).toBe(false);
    expect(result.before).toEqual([]);
    expect(result.after).toEqual([{ text: "hello world", changed: true }]);
  });

  it("reports an empty after side with one wholly changed before segment", () => {
    const result = wordDiff("hello world", "");

    expect(result.bailed).toBe(false);
    expect(result.before).toEqual([{ text: "hello world", changed: true }]);
    expect(result.after).toEqual([]);
  });

  it("ignores pure re-indentation when ignoreWhitespace is on", () => {
    const result = wordDiff("  foo()", "\t\tfoo()", { ignoreWhitespace: true });

    expect(result.bailed).toBe(false);
    expect(result.before.some((segment) => segment.changed)).toBe(false);
    expect(result.after.some((segment) => segment.changed)).toBe(false);
    expectSound(result, "  foo()", "\t\tfoo()");
  });

  it("highlights the leading whitespace when ignoreWhitespace is off", () => {
    const result = wordDiff("  foo()", "\t\tfoo()");

    expect(result.bailed).toBe(false);
    expect(result.before[0]).toEqual({ text: "  ", changed: true });
    expect(result.after[0]).toEqual({ text: "\t\t", changed: true });
  });

  it("bails on a pair above maxTokens and still reproduces both sides", () => {
    const before = "one two three four five";
    const after = "1 2 3 4 5";
    const result = wordDiff(before, after, { maxTokens: 2 });

    expect(result.bailed).toBe(true);
    expect(result.before).toEqual([{ text: before, changed: true }]);
    expect(result.after).toEqual([{ text: after, changed: true }]);
    expectSound(result, before, after);
  });

  it("lands the LCS on the later common token and segments exactly", () => {
    // Tokens "a b c d e" vs "a c b d e". The common head is "a"+space, the common
    // tail is space+"d"+space+"e". The remaining ["b"," ","c"] vs ["c"," ","b"]
    // share only one token, and the table walk keeps "c" on both sides. The kept
    // "c" on the after side abuts the head, so the two merge into one segment.
    const result = wordDiff("a b c d e", "a c b d e");

    expect(result.bailed).toBe(false);
    expect(result.before).toEqual([
      { text: "a ", changed: false },
      { text: "b ", changed: true },
      { text: "c d e", changed: false }
    ]);
    expect(result.after).toEqual([
      { text: "a c", changed: false },
      { text: " b", changed: true },
      { text: " d e", changed: false }
    ]);
  });

  it("holds the text and alternation invariants on every case", () => {
    const cases: Array<[string, string, Parameters<typeof wordDiff>[2]]> = [
      ["const value = compute();", "const result = compute();", undefined],
      ["world", "hello world", undefined],
      ["hello", "hello world", undefined],
      ["a c", "a b c", undefined],
      ["a b c", "a c", undefined],
      ["", "hello world", undefined],
      ["hello world", "", undefined],
      ["  foo()", "\t\tfoo()", { ignoreWhitespace: true }],
      ["  foo()", "\t\tfoo()", undefined],
      ["a b c d e", "a c b d e", undefined],
      ["x = [1, 2, 3];", "x = [1, 2, 3, 4];", undefined],
      ["return foo(a, b);", "return foo(a, c);", undefined]
    ];
    for (const [before, after, options] of cases) {
      expectSound(wordDiff(before, after, options), before, after);
    }
  });

  it("is deterministic for the same inputs", () => {
    const first = wordDiff("const alpha = beta(1);", "const alpha = gamma(2);");
    const second = wordDiff("const alpha = beta(1);", "const alpha = gamma(2);");

    expect(second).toEqual(first);
  });
});
