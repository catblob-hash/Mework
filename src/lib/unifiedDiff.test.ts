import { describe, expect, it } from "vitest";
import { diffTotals, parseUnifiedDiff, unquoteGitPath } from "./unifiedDiff";

const MODIFIED_PATCH = `diff --git a/src/app.ts b/src/app.ts
index 1111111..2222222 100644
--- a/src/app.ts
+++ b/src/app.ts
@@ -1,3 +1,3 @@ function main() {
 context
-removed
+added
 context2
`;

describe("parseUnifiedDiff", () => {
  it("reads one hunk of a single-file modification with numbered lines", () => {
    const [file] = parseUnifiedDiff(MODIFIED_PATCH);

    expect(file.path).toBe("src/app.ts");
    expect(file.oldPath).toBeNull();
    expect(file.status).toBe("modified");
    expect(file.binary).toBe(false);
    expect(file.additions).toBe(1);
    expect(file.deletions).toBe(1);
    expect(file.incomplete).toBe(false);

    const [hunk] = file.hunks;
    expect(hunk.oldStart).toBe(1);
    expect(hunk.oldLines).toBe(3);
    expect(hunk.newStart).toBe(1);
    expect(hunk.newLines).toBe(3);
    expect(hunk.heading).toBe("function main() {");
    expect(hunk.lines).toEqual([
      { kind: "context", oldLine: 1, newLine: 1, text: "context", noNewline: false },
      { kind: "deletion", oldLine: 2, newLine: null, text: "removed", noNewline: false },
      { kind: "addition", oldLine: null, newLine: 2, text: "added", noNewline: false },
      { kind: "context", oldLine: 3, newLine: 3, text: "context2", noNewline: false }
    ]);
  });

  it("splits a two-file patch into two files in order", () => {
    const files = parseUnifiedDiff(`diff --git a/one.txt b/one.txt
index 1111111..2222222 100644
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-old
+new
diff --git a/two.txt b/two.txt
index 3333333..4444444 100644
--- a/two.txt
+++ b/two.txt
@@ -1 +1 @@
-alpha
+beta
`);

    expect(files.map((file) => file.path)).toEqual(["one.txt", "two.txt"]);
    expect(files.every((file) => file.status === "modified")).toBe(true);
    expect(files[0].hunks[0].lines[0].text).toBe("old");
    expect(files[1].hunks[0].lines[1].text).toBe("beta");
  });

  it("reads an added file with a null old path", () => {
    const [file] = parseUnifiedDiff(`diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
`);

    expect(file.status).toBe("added");
    expect(file.path).toBe("new.txt");
    expect(file.oldPath).toBeNull();
    expect(file.newMode).toBe("100644");
    expect(file.additions).toBe(1);
    expect(file.deletions).toBe(0);
  });

  it("reads a deleted file under its old path", () => {
    const [file] = parseUnifiedDiff(`diff --git a/old.txt b/old.txt
deleted file mode 100644
index 1111111..0000000
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-goodbye
`);

    expect(file.status).toBe("deleted");
    expect(file.path).toBe("old.txt");
    expect(file.oldPath).toBeNull();
    expect(file.additions).toBe(0);
    expect(file.deletions).toBe(1);
  });

  it("reads a rename with content changes", () => {
    const [file] = parseUnifiedDiff(`diff --git a/old-name.txt b/new-name.txt
similarity index 80%
rename from old-name.txt
rename to new-name.txt
index 1111111..2222222 100644
--- a/old-name.txt
+++ b/new-name.txt
@@ -1 +1 @@
-old content
+new content
`);

    expect(file.status).toBe("renamed");
    expect(file.path).toBe("new-name.txt");
    expect(file.oldPath).toBe("old-name.txt");
    expect(file.hunks).toHaveLength(1);
  });

  it("reads a pure rename with no content hunk", () => {
    const [file] = parseUnifiedDiff(`diff --git a/old-name.txt b/new-name.txt
similarity index 100%
rename from old-name.txt
rename to new-name.txt
`);

    expect(file.status).toBe("renamed");
    expect(file.path).toBe("new-name.txt");
    expect(file.oldPath).toBe("old-name.txt");
    expect(file.hunks).toEqual([]);
  });

  it("reads a copy with the source as the old path", () => {
    const [file] = parseUnifiedDiff(`diff --git a/original.txt b/copy.txt
similarity index 100%
copy from original.txt
copy to copy.txt
`);

    expect(file.status).toBe("copied");
    expect(file.path).toBe("copy.txt");
    expect(file.oldPath).toBe("original.txt");
    expect(file.hunks).toEqual([]);
  });

  it("flags a mode-change-only file", () => {
    const [file] = parseUnifiedDiff(`diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
`);

    expect(file.status).toBe("modified");
    expect(file.modeChangeOnly).toBe(true);
    expect(file.oldMode).toBe("100644");
    expect(file.newMode).toBe("100755");
    expect(file.hunks).toEqual([]);
  });

  it("recognises a type change from the mode pair", () => {
    const [file] = parseUnifiedDiff(`diff --git a/link b/link
old mode 120000
new mode 100644
`);

    expect(file.status).toBe("typeChanged");
    expect(file.oldMode).toBe("120000");
    expect(file.newMode).toBe("100644");
  });

  it("marks a binary file declared by the differ line", () => {
    const [file] = parseUnifiedDiff(`diff --git a/x.png b/x.png
index 1111111..2222222 100644
Binary files a/x.png and b/x.png differ
`);

    expect(file.binary).toBe(true);
    expect(file.hunks).toEqual([]);
    expect(file.status).toBe("modified");
  });

  it("keeps a GIT binary patch payload from leaking into the next file", () => {
    const files = parseUnifiedDiff(`diff --git a/blob.bin b/blob.bin
index 1111111..2222222 100644
GIT binary patch
literal 24
YcmVn#hKq#9F^Rgz}<!{

zcmVnb0jH8m0P2Q5
diff --git a/other.txt b/other.txt
index 3333333..4444444 100644
--- a/other.txt
+++ b/other.txt
@@ -1 +1 @@
-a
+b
`);

    expect(files).toHaveLength(2);
    expect(files[0].path).toBe("blob.bin");
    expect(files[0].binary).toBe(true);
    expect(files[0].hunks).toEqual([]);
    expect(files[1].path).toBe("other.txt");
    expect(files[1].binary).toBe(false);
    expect(files[1].hunks).toHaveLength(1);
    expect(files[1].hunks[0].lines.map((line) => line.text)).toEqual(["a", "b"]);
  });

  it("defaults a countless hunk header to one line on each side", () => {
    const [file] = parseUnifiedDiff(`diff --git a/single.txt b/single.txt
index 1111111..2222222 100644
--- a/single.txt
+++ b/single.txt
@@ -4 +4 @@
-old
+new
`);

    const [hunk] = file.hunks;
    expect(hunk.oldStart).toBe(4);
    expect(hunk.oldLines).toBe(1);
    expect(hunk.newStart).toBe(4);
    expect(hunk.newLines).toBe(1);
    expect(file.incomplete).toBe(false);
  });

  it("keeps the text after the closing @@ as the hunk heading", () => {
    const [file] = parseUnifiedDiff(`diff --git a/code.ts b/code.ts
index 1111111..2222222 100644
--- a/code.ts
+++ b/code.ts
@@ -1,2 +1,3 @@ function foo() {
 a
-b
+c
+d
`);

    expect(file.hunks[0].heading).toBe("function foo() {");
  });

  describe("no-newline markers", () => {
    it("marks a deletion when the marker appears while the hunk is still open", () => {
      const [file] = parseUnifiedDiff(`diff --git a/nl.txt b/nl.txt
index 1111111..2222222 100644
--- a/nl.txt
+++ b/nl.txt
@@ -1,2 +1,2 @@
 x
-y
\\ No newline at end of file
+z
`);

      const lines = file.hunks[0].lines;
      expect(lines[0].noNewline).toBe(false);
      expect(lines[1]).toMatchObject({ kind: "deletion", text: "y", noNewline: true });
      expect(lines[2]).toMatchObject({ kind: "addition", text: "z", noNewline: false });
    });

    // The marker follows the hunk's final body line — by which point the parser has
    // already closed the hunk on its declared counts — so it must still reach the
    // last line of the closed hunk.
    it("marks the addition preceding a marker at the end of the hunk", () => {
      const [file] = parseUnifiedDiff(`diff --git a/nl.txt b/nl.txt
index 1111111..2222222 100644
--- a/nl.txt
+++ b/nl.txt
@@ -1,1 +1,2 @@
 x
+a
\\ No newline at end of file
`);

      const lines = file.hunks[0].lines;
      expect(lines[0].noNewline).toBe(false);
      expect(lines[1]).toMatchObject({ kind: "addition", text: "a", noNewline: true });
    });

    it("marks the context line preceding a marker at the end of the hunk", () => {
      const [file] = parseUnifiedDiff(`diff --git a/nl.txt b/nl.txt
index 1111111..2222222 100644
--- a/nl.txt
+++ b/nl.txt
@@ -1,2 +1,2 @@
-a
+A
 b
\\ No newline at end of file
`);

      const lines = file.hunks[0].lines;
      expect(lines[0].noNewline).toBe(false);
      expect(lines[1].noNewline).toBe(false);
      expect(lines[2]).toMatchObject({ kind: "context", text: "b", noNewline: true });
    });
  });

  it("reports a patch that stops mid-hunk as incomplete without throwing", () => {
    const [file] = parseUnifiedDiff(`diff --git a/truncated.txt b/truncated.txt
index 1111111..2222222 100644
--- a/truncated.txt
+++ b/truncated.txt
@@ -1,5 +1,5 @@
 a
 b
`);

    expect(file.path).toBe("truncated.txt");
    expect(file.incomplete).toBe(true);
    expect(file.hunks).toHaveLength(1);
    expect(file.hunks[0].lines).toHaveLength(2);
  });

  it("keeps a trailing carriage return on a hunk body line while still parsing the structure", () => {
    const patch = [
      "diff --git a/crlf.txt b/crlf.txt",
      "index 1111111..2222222 100644",
      "--- a/crlf.txt",
      "+++ b/crlf.txt\r",
      "@@ -1,2 +1,2 @@",
      "-old\r",
      "+new",
      " ctx"
    ].join("\n");
    const [file] = parseUnifiedDiff(patch);

    expect(file.path).toBe("crlf.txt");
    const [hunk] = file.hunks;
    expect(hunk.lines[0]).toEqual({ kind: "deletion", oldLine: 1, newLine: null, text: "old\r", noNewline: false });
    expect(hunk.lines[1].text).toBe("new");
    expect(hunk.lines[2].text).toBe("ctx");
  });

  it("refuses to number a combined diff and reports it incomplete", () => {
    const [file] = parseUnifiedDiff(`diff --git a/merged.txt b/merged.txt
index 1111111..2222222 100644
--- a/merged.txt
+++ b/merged.txt
@@@ -1,2 -1,2 +1,2 @@@
 a
 b
`);

    expect(file.path).toBe("merged.txt");
    expect(file.hunks).toEqual([]);
    expect(file.incomplete).toBe(true);
  });

  describe("fallbackPath", () => {
    it("replaces the path of a single-file patch", () => {
      const [file] = parseUnifiedDiff(MODIFIED_PATCH, { fallbackPath: "docs/readme.md" });

      expect(file.path).toBe("docs/readme.md");
    });

    it("leaves a multi-file patch alone", () => {
      const files = parseUnifiedDiff(`diff --git a/one.txt b/one.txt
index 1111111..2222222 100644
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-old
+new
diff --git a/two.txt b/two.txt
index 3333333..4444444 100644
--- a/two.txt
+++ b/two.txt
@@ -1 +1 @@
-alpha
+beta
`, { fallbackPath: "docs/readme.md" });

      expect(files.map((file) => file.path)).toEqual(["one.txt", "two.txt"]);
    });
  });

  it("totals additions and deletions across files", () => {
    const files = parseUnifiedDiff(`diff --git a/one.txt b/one.txt
index 1111111..2222222 100644
--- a/one.txt
+++ b/one.txt
@@ -1,2 +1,2 @@
-keep me
+swap
+in
diff --git a/two.txt b/two.txt
index 3333333..4444444 100644
--- a/two.txt
+++ b/two.txt
@@ -1 +0,0 @@
-gone
`);

    expect(diffTotals(files)).toEqual({ additions: 2, deletions: 2 });
  });

  describe("garbage input", () => {
    it("returns nothing for an empty string", () => {
      expect(parseUnifiedDiff("")).toEqual([]);
    });

    it("returns nothing for prose", () => {
      expect(parseUnifiedDiff("hello\nworld")).toEqual([]);
    });

    it("returns nothing for a stray hunk header", () => {
      expect(parseUnifiedDiff("@@ -1 +1 @@")).toEqual([]);
    });

    it("still names a file for a bare diff --git header", () => {
      const [file] = parseUnifiedDiff("diff --git a/x.txt b/x.txt\n");

      expect(file).toMatchObject({ path: "x.txt", status: "modified", hunks: [] });
    });
  });

  it("treats a bare empty line in a hunk body as an empty context line", () => {
    const [file] = parseUnifiedDiff(`diff --git a/blanks.txt b/blanks.txt
index 1111111..2222222 100644
--- a/blanks.txt
+++ b/blanks.txt
@@ -1,2 +1,3 @@
+a

 ctx
`);

    const [hunk] = file.hunks;
    expect(hunk.lines[1]).toEqual({ kind: "context", oldLine: 1, newLine: 2, text: "", noNewline: false });
    expect(hunk.lines[2]).toMatchObject({ kind: "context", oldLine: 2, newLine: 3, text: "ctx" });
  });
});

describe("unquoteGitPath", () => {
  it("passes an unquoted path through untouched", () => {
    expect(unquoteGitPath("src/lib/app.ts")).toBe("src/lib/app.ts");
  });

  it("unwraps a quoted path with a space", () => {
    expect(unquoteGitPath('"a/with space"')).toBe("a/with space");
  });

  it("decodes an escaped tab", () => {
    expect(unquoteGitPath('"a/tab\\there"')).toBe("a/tab\there");
  });

  it("decodes octal-escaped UTF-8 bytes together, not per character", () => {
    expect(unquoteGitPath('"a/\\346\\226\\207\\344\\273\\266.txt"')).toBe("a/文件.txt");
  });

  it("uses a quoted path from a real diff --git header", () => {
    const [file] = parseUnifiedDiff(`diff --git "a/with space.txt" "b/with space.txt"
index 1111111..2222222 100644
--- "a/with space.txt"
+++ "b/with space.txt"
@@ -1 +1 @@
-x
+y
`);

    expect(file.path).toBe("with space.txt");
  });

  it("uses a quoted octal path from a real diff --git header", () => {
    const [file] = parseUnifiedDiff(`diff --git "a/\\346\\226\\207\\344\\273\\266.txt" "b/\\346\\226\\207\\344\\273\\266.txt"
index 1111111..2222222 100644
--- "a/\\346\\226\\207\\344\\273\\266.txt"
+++ "b/\\346\\226\\207\\344\\273\\266.txt"
@@ -1 +1 @@
-x
+y
`);

    expect(file.path).toBe("文件.txt");
  });
});
