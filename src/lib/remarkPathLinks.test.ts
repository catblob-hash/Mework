import { describe, expect, it } from "vitest";
import { inlineCodePath, splitPathRuns } from "./remarkPathLinks";

/** The paths in a prose run, in order. */
function paths(value: string): string[] {
  return splitPathRuns(value)
    .filter((run) => run.kind === "path")
    .map((run) => (run.kind === "path" ? run.target : ""));
}

/** The run sequence rebuilt as text, which must equal the input. */
function rebuilt(value: string): string {
  return splitPathRuns(value)
    .map((run) => (run.kind === "text" ? run.value : run.display))
    .join("");
}

describe("splitPathRuns", () => {
  it("recognizes absolute paths in prose", () => {
    expect(paths("见 /usr/bin/env 就好")).toEqual(["/usr/bin/env"]);
    expect(paths("打开 C:\\Windows\\System32\\drivers\\etc\\hosts")).toEqual([
      "C:\\Windows\\System32\\drivers\\etc\\hosts"
    ]);
    expect(paths("D:/projects/mework/src")).toEqual(["D:/projects/mework/src"]);
    expect(paths("先看 /etc/hosts，再看 /var/log/syslog")).toEqual(["/etc/hosts", "/var/log/syslog"]);
  });

  it("leaves prose that merely contains a slash alone", () => {
    for (const value of [
      "and/or",
      "24/7",
      "TypeScript 5.0/6.0",
      "读写/执行权限",
      "见 /etc 目录",
      "a/b",
      "no separators here"
    ]) {
      expect(paths(value), value).toEqual([]);
    }
  });

  it("does not match inside an address that was not autolinked", () => {
    // The drive-letter branch would otherwise match `s:/` and the POSIX branch
    // the tail of the path.
    expect(paths("ftp://example.com/a/b")).toEqual([]);
    expect(paths("https://example.com/docs/guide")).toEqual([]);
    expect(paths("参见 mailto://x/y/z")).toEqual([]);
  });

  it("drops sentence punctuation but keeps balanced brackets", () => {
    expect(paths("配置在 /etc/hosts。")).toEqual(["/etc/hosts"]);
    expect(paths("配置在 /etc/hosts, 对吧?")).toEqual(["/etc/hosts"]);
    expect(paths("（见 C:\\Program\\a.exe）")).toEqual(["C:\\Program\\a.exe"]);
    expect(paths("见 (/usr/bin/env)")).toEqual(["/usr/bin/env"]);
    expect(paths("C:\\Program Files (x86)\\node\\node.exe".replace(/ /g, "_"))).toEqual([
      "C:\\Program_Files_(x86)\\node\\node.exe"
    ]);
  });

  it("shows a line reference but does not send it to the host", () => {
    const runs = splitPathRuns("崩在 /usr/lib/app.js:12:3 这里");
    const path = runs.find((run) => run.kind === "path");
    expect(path).toEqual({ kind: "path", display: "/usr/lib/app.js:12:3", target: "/usr/lib/app.js" });
  });

  it("never loses or duplicates text", () => {
    for (const value of [
      "见 /usr/bin/env 就好",
      "配置在 /etc/hosts。后面还有字",
      "and/or",
      "/etc/hosts",
      "先看 /etc/hosts，再看 /var/log/syslog"
    ]) {
      expect(rebuilt(value), value).toBe(value);
    }
  });
});

describe("inlineCodePath", () => {
  it("accepts the relative paths models put in backticks", () => {
    for (const value of ["src/lib/foo.ts", "./scripts/run.sh", "../sibling/file.md", "a/b/c"]) {
      expect(inlineCodePath(value)?.target, value).toBe(value);
    }
  });

  it("accepts absolute paths, including ones containing spaces", () => {
    expect(inlineCodePath("C:\\Program Files (x86)\\node\\node.exe")?.target).toBe(
      "C:\\Program Files (x86)\\node\\node.exe"
    );
    expect(inlineCodePath("/usr/local/bin/node")?.target).toBe("/usr/local/bin/node");
  });

  it("splits a line reference off the target", () => {
    expect(inlineCodePath("src/App.tsx:12")).toEqual({
      display: "src/App.tsx:12",
      target: "src/App.tsx"
    });
  });

  it("rejects code spans that are not paths", () => {
    for (const value of [
      "npm run dev",
      "and/or",
      "useMemo",
      "5.0/6.0",
      "npm run dev -- src/foo",
      "ls -la src/lib",
      "https://example.com/a/b",
      "//server/share",
      "\\\\server\\share\\file.txt",
      "echo \"a/b\"",
      ""
    ]) {
      expect(inlineCodePath(value), value).toBeNull();
    }
  });
});
