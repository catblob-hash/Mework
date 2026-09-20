import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "../components/DiffOutput";
import { lineDiff } from "./lineDiff";

/** Builds "line 1"…"line 20", the fixture every hunk-position test starts from. */
function twentyLines(): string {
  return Array.from({ length: 20 }, (_, index) => `line ${index + 1}`).join("\n");
}

function hunkHeaders(patch: string): string[] {
  return patch.split("\n").filter((line) => line.startsWith("@@"));
}

describe("lineDiff", () => {
  it("returns an empty patch and zero counts for identical strings", () => {
    const result = lineDiff("alpha\nbeta\ngamma", "alpha\nbeta\ngamma");

    expect(result).toEqual({ patch: "", additions: 0, deletions: 0, bailed: false });
  });

  it("returns an empty patch for identical strings that both end in a newline", () => {
    const result = lineDiff("alpha\nbeta\n", "alpha\nbeta\n");

    expect(result).toEqual({ patch: "", additions: 0, deletions: 0, bailed: false });
  });

  it("reports a one-line middle change as one addition and one deletion", () => {
    const before = twentyLines();
    const after = twentyLines().replace("line 10", "line ten");
    const result = lineDiff(before, after);

    expect(result.bailed).toBe(false);
    expect(result.additions).toBe(1);
    expect(result.deletions).toBe(1);
    // Context 3 on each side of line 10: old lines 7-13, new lines 7-13.
    expect(hunkHeaders(result.patch)).toEqual(["@@ -7,7 +7,7 @@"]);
    expect(result.patch).toContain("-line 10");
    expect(result.patch).toContain("+line ten");
  });

  it("reports a pure insertion as additions only", () => {
    const result = lineDiff("alpha\nbeta\ngamma", "alpha\nbeta\ninserted\ngamma");

    expect(result.bailed).toBe(false);
    expect(result.additions).toBe(1);
    expect(result.deletions).toBe(0);
    expect(hunkHeaders(result.patch)).toEqual(["@@ -1,3 +1,4 @@"]);
  });

  it("reports a pure deletion as deletions only", () => {
    const result = lineDiff("alpha\nbeta\ngamma", "alpha\ngamma");

    expect(result.bailed).toBe(false);
    expect(result.additions).toBe(0);
    expect(result.deletions).toBe(1);
    expect(hunkHeaders(result.patch)).toEqual(["@@ -1,3 +1,2 @@"]);
  });

  it("addresses an empty before side as -0,0", () => {
    const result = lineDiff("", "alpha\nbeta\ngamma");

    expect(result.bailed).toBe(false);
    expect(result.additions).toBe(3);
    expect(result.deletions).toBe(0);
    expect(hunkHeaders(result.patch)).toEqual(["@@ -0,0 +1,3 @@"]);
  });

  it("addresses an empty after side as +0,0", () => {
    const result = lineDiff("alpha\nbeta\ngamma", "");

    expect(result.bailed).toBe(false);
    expect(result.additions).toBe(0);
    expect(result.deletions).toBe(3);
    expect(hunkHeaders(result.patch)).toEqual(["@@ -1,3 +0,0 @@"]);
  });

  it("treats two empty strings as identical", () => {
    const result = lineDiff("", "");

    expect(result).toEqual({ patch: "", additions: 0, deletions: 0, bailed: false });
  });

  it("does not report a trailing newline present on only one side", () => {
    // The renderer's parser has no "\ No newline at end of file" marker, so the
    // trailing newline is dropped on both sides alike before comparing.
    const result = lineDiff("alpha\nbeta\n", "alpha\nbeta");

    expect(result).toEqual({ patch: "", additions: 0, deletions: 0, bailed: false });
  });

  it("normalises CRLF and lone CR line endings before comparing", () => {
    const result = lineDiff("alpha\r\nbeta\r\ngamma", "alpha\nline two\ngamma");
    const crlfOnly = lineDiff("alpha\rbeta\rgamma", "alpha\nline two\ngamma");

    expect(result.additions).toBe(1);
    expect(result.deletions).toBe(1);
    expect(result.patch).toBe(crlfOnly.patch);
  });

  it("keeps two distant edits in two hunks", () => {
    const before = twentyLines();
    const after = twentyLines()
      .replace("line 2", "line two")
      .replace("line 19", "line nineteen");
    const result = lineDiff(before, after);

    expect(result.additions).toBe(2);
    expect(result.deletions).toBe(2);
    // Context 3 around line 2, clamped at the start, covers old lines 1-5.
    expect(hunkHeaders(result.patch)).toEqual(["@@ -1,5 +1,5 @@", "@@ -16,5 +16,5 @@"]);
  });

  it("merges two nearby edits into one hunk", () => {
    const before = twentyLines();
    const after = twentyLines()
      .replace("line 10", "line ten")
      .replace("line 12", "line twelve");
    const result = lineDiff(before, after);

    expect(result.additions).toBe(2);
    expect(result.deletions).toBe(2);
    // Context 3 around lines 10 and 12 covers old lines 7-15, nine lines.
    expect(hunkHeaders(result.patch)).toEqual(["@@ -7,9 +7,9 @@"]);
  });

  it("honours a smaller context setting", () => {
    const before = twentyLines();
    const after = twentyLines().replace("line 10", "line ten");
    const result = lineDiff(before, after, { context: 1 });

    // Context 1 around line 10 covers old lines 9-11, three lines.
    expect(hunkHeaders(result.patch)).toEqual(["@@ -9,3 +9,3 @@"]);
  });

  it("bails past maxLines with a whole rewrite and correct counts", () => {
    const before = "a\nb\nc\nd\ne";
    const after = "1\n2\n3\n4\n5";
    const result = lineDiff(before, after, { maxLines: 2 });

    expect(result.bailed).toBe(true);
    expect(result.additions).toBe(5);
    expect(result.deletions).toBe(5);
    expect(hunkHeaders(result.patch)).toEqual(["@@ -1,5 +1,5 @@"]);
  });

  it("bails past maxCells with a whole rewrite and correct counts", () => {
    const before = "a\nb\nc\nd\ne";
    const after = "1\n2\n3\n4\n5";
    const result = lineDiff(before, after, { maxCells: 8 });

    expect(result.bailed).toBe(true);
    expect(result.additions).toBe(5);
    expect(result.deletions).toBe(5);
  });

  it("writes file headers when a path is given and omits them otherwise", () => {
    const withPath = lineDiff("alpha\nbeta", "alpha\ngamma", { path: "m.txt" });
    const withoutPath = lineDiff("alpha\nbeta", "alpha\ngamma");

    expect(withPath.patch.startsWith("--- a/m.txt\n+++ b/m.txt\n")).toBe(true);
    expect(withoutPath.patch.startsWith("@@ ")).toBe(true);
  });

  it("round-trips through parseUnifiedDiff with matching counts and path", () => {
    const cases: Array<[string, string]> = [
      ["alpha\nbeta\ngamma", "alpha\nbeta\ngamma delta"],
      [twentyLines(), twentyLines().replace("line 5", "line five").replace("line 15", "line fifteen")],
      ["", "brand new\ncontent\nhere"],
      ["old\ncontent\nhere", ""],
      ["keep\n- minus\nplus +\nkeep", "keep\n+++ triple\nkeep"],
      ["a\r\nb\r\nc", "a\nb two\nc"]
    ];
    for (const [before, after] of cases) {
      const result = lineDiff(before, after, { path: "m.txt" });
      const parsed = parseUnifiedDiff(result.patch);

      expect(parsed.additions).toBe(result.additions);
      expect(parsed.deletions).toBe(result.deletions);
      expect(parsed.path).toBe("m.txt");
      expect(result.bailed).toBe(false);
    }
  });
});
