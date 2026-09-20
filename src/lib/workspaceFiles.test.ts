import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GitTarget } from "./git";
import type { WorkspaceEntry } from "./workspaceFiles";
import {
  basename,
  joinRelativePath,
  listWorkspaceDirectory,
  parentRelativePath,
  readWorkspaceFile,
  readWorkspaceFileBytes,
  sortEntries,
  workspaceRelativePath
} from "./workspaceFiles";

const backend = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => true),
  invoke: vi.fn()
}));

vi.mock("./backend", () => backend);

const target: GitTarget = { kind: "conversation", conversationId: "conversation-1" };

function entry(name: string, kind: WorkspaceEntry["kind"] = "file"): WorkspaceEntry {
  return { name, kind, size: kind === "directory" ? null : 0 };
}

beforeEach(() => {
  backend.hasBackendRuntime.mockReturnValue(true);
  backend.invoke.mockReset();
});

describe("joinRelativePath", () => {
  it("addresses a child of the workspace root without a leading separator", () => {
    expect(joinRelativePath("", "src")).toBe("src");
    expect(joinRelativePath("", "README.md")).toBe("README.md");
  });

  it("joins nested segments with a single separator", () => {
    expect(joinRelativePath("src", "lib")).toBe("src/lib");
    expect(joinRelativePath("src/lib", "git.ts")).toBe("src/lib/git.ts");
  });

  it("never doubles a separator the caller already supplied", () => {
    expect(joinRelativePath("src/", "lib")).toBe("src/lib");
    expect(joinRelativePath("src", "/lib")).toBe("src/lib");
    expect(joinRelativePath("src/", "/lib")).toBe("src/lib");
  });

  it("keeps the parent when there is no child name", () => {
    expect(joinRelativePath("src", "")).toBe("src");
    expect(joinRelativePath("", "")).toBe("");
  });
});

describe("parentRelativePath", () => {
  it("reports the root itself as having no parent", () => {
    expect(parentRelativePath("")).toBeNull();
    expect(parentRelativePath("/")).toBeNull();
  });

  it("returns the root for a top-level entry", () => {
    expect(parentRelativePath("src")).toBe("");
    expect(parentRelativePath("README.md")).toBe("");
  });

  it("walks up one level for a nested entry", () => {
    expect(parentRelativePath("src/lib/git.ts")).toBe("src/lib");
    expect(parentRelativePath("src/lib/")).toBe("src");
  });
});

describe("basename", () => {
  it("takes the last segment", () => {
    expect(basename("src/lib/git.ts")).toBe("git.ts");
    expect(basename("README.md")).toBe("README.md");
    expect(basename("src/lib/")).toBe("lib");
  });

  it("gives the root no name of its own", () => {
    expect(basename("")).toBe("");
    expect(basename("/")).toBe("");
  });
});

describe("sortEntries", () => {
  it("puts directories before every other kind", () => {
    const sorted = sortEntries([
      entry("readme.md"),
      entry("src", "directory"),
      entry("link", "symlink"),
      entry("docs", "directory")
    ]);

    expect(sorted.map((item) => item.name)).toEqual(["docs", "src", "link", "readme.md"]);
  });

  it("orders names case-insensitively", () => {
    const sorted = sortEntries([entry("Zebra.ts"), entry("apple.ts"), entry("Banana.ts")]);

    expect(sorted.map((item) => item.name)).toEqual(["apple.ts", "Banana.ts", "Zebra.ts"]);
  });

  /** Case folding alone is not a total order; the host's own order must survive the tie. */
  it("leaves a case-folded tie in the order the host gave", () => {
    const forward = sortEntries([entry("readme"), entry("README")]);
    const reversed = sortEntries([entry("README"), entry("readme")]);

    expect(forward.map((item) => item.name)).toEqual(["readme", "README"]);
    expect(reversed.map((item) => item.name)).toEqual(["README", "readme"]);
  });

  it("leaves the caller's array untouched", () => {
    const original = [entry("b.ts"), entry("a.ts")];
    const sorted = sortEntries(original);

    expect(original.map((item) => item.name)).toEqual(["b.ts", "a.ts"]);
    expect(sorted).not.toBe(original);
  });
});

describe("workspace commands", () => {
  it("sends the target and the relative path to the host", async () => {
    backend.invoke.mockResolvedValueOnce({ path: "src", entries: [] });

    await expect(listWorkspaceDirectory(target, "src")).resolves.toEqual({ path: "src", entries: [] });
    expect(backend.invoke).toHaveBeenCalledWith("list_workspace_directory", {
      target,
      relativePath: "src"
    });
  });

  it("reads a file through its own command", async () => {
    const file = { path: "a.txt", content: "hi", truncated: false, size: 2, binary: false };
    backend.invoke.mockResolvedValueOnce(file);

    await expect(readWorkspaceFile(target, "a.txt")).resolves.toEqual(file);
    expect(backend.invoke).toHaveBeenCalledWith("read_workspace_file", {
      target,
      relativePath: "a.txt"
    });
  });

  it("refuses to browse without a host to browse on", async () => {
    backend.hasBackendRuntime.mockReturnValue(false);

    await expect(listWorkspaceDirectory(target, "")).rejects.toThrow();
    await expect(readWorkspaceFile(target, "a.txt")).rejects.toThrow();
    await expect(readWorkspaceFileBytes(target, "a.png")).rejects.toThrow();
    expect(backend.invoke).not.toHaveBeenCalled();
  });
});

/**
 * What decides whether a path clicked in a transcript can be shown in the pane
 * at all. Everything it answers null for is revealed on disk instead, so a wrong
 * answer here is the difference between opening a file and opening Explorer.
 */
describe("workspaceRelativePath", () => {
  const posixRoot = "/home/me/mework";
  const windowsRoot = "C:\\Projects\\Mework";

  it("takes a relative path as already workspace-relative", () => {
    expect(workspaceRelativePath("src/App.tsx", posixRoot, posixRoot)).toBe("src/App.tsx");
    expect(workspaceRelativePath("./src/App.tsx", null, posixRoot)).toBe("src/App.tsx");
    expect(workspaceRelativePath("src\\App.tsx", null, windowsRoot)).toBe("src/App.tsx");
  });

  it("strips the workspace root off an absolute path", () => {
    expect(workspaceRelativePath(`${posixRoot}/src/App.tsx`, null, posixRoot)).toBe("src/App.tsx");
    expect(workspaceRelativePath("C:\\Projects\\Mework\\src\\App.tsx", null, windowsRoot))
      .toBe("src/App.tsx");
  });

  /** Windows compares paths without case; the drive letter is what says which rule applies. */
  it("folds case only for a Windows checkout", () => {
    expect(workspaceRelativePath("c:\\projects\\mework\\src\\App.tsx", null, windowsRoot))
      .toBe("src/App.tsx");
    expect(workspaceRelativePath("/HOME/ME/mework/src/App.tsx", null, posixRoot)).toBe(null);
  });

  /**
   * The whole reason both directories are passed: a path written against a
   * working directory that is not the checkout is only workspace-relative once
   * it has been made absolute against the one it was written for.
   */
  it("resolves a relative path against the directory it was written in", () => {
    expect(workspaceRelativePath("App.tsx", `${posixRoot}/src`, posixRoot)).toBe("src/App.tsx");
    expect(workspaceRelativePath("../docs/guide.md", `${posixRoot}/src`, posixRoot))
      .toBe("docs/guide.md");
  });

  it("answers null for anything the pane cannot reach", () => {
    expect(workspaceRelativePath("/etc/hosts", null, posixRoot)).toBe(null);
    expect(workspaceRelativePath(`${posixRoot}-other/src/App.tsx`, null, posixRoot)).toBe(null);
    // The root itself is a directory, not a file the viewer could show.
    expect(workspaceRelativePath(posixRoot, null, posixRoot)).toBe(null);
    expect(workspaceRelativePath("../outside.md", null, posixRoot)).toBe(null);
    expect(workspaceRelativePath("/etc/hosts", null, null)).toBe(null);
    expect(workspaceRelativePath("   ", null, posixRoot)).toBe(null);
  });

  /**
   * The host canonicalizes the workspace to `\\?\C:\…` and a model never writes
   * one, so without stripping the prefix every absolute path looks like it lives
   * on a different disk.
   */
  it("sees through a Windows extended-length prefix on either side", () => {
    const extendedRoot = `\\\\?\\${windowsRoot}`;
    expect(workspaceRelativePath("C:\\Projects\\Mework\\src\\App.tsx", null, extendedRoot))
      .toBe("src/App.tsx");
    expect(workspaceRelativePath(`${extendedRoot}\\src\\App.tsx`, null, windowsRoot))
      .toBe("src/App.tsx");
    expect(workspaceRelativePath("src/App.tsx", extendedRoot, windowsRoot)).toBe("src/App.tsx");
  });

  /** A network location is refused rather than collapsed into something else. */
  it("refuses a UNC or device path", () => {
    expect(workspaceRelativePath("\\\\server\\share\\a.txt", null, windowsRoot)).toBe(null);
    expect(workspaceRelativePath("\\\\?\\UNC\\server\\share\\a.txt", null, windowsRoot)).toBe(null);
    expect(workspaceRelativePath("\\\\.\\PhysicalDrive0", null, windowsRoot)).toBe(null);
    // A root spelled as a share stops absolute paths from resolving, but a
    // relative one is workspace-relative however the root is spelled.
    expect(workspaceRelativePath("C:\\Projects\\Mework\\src\\App.tsx", null, "\\\\server\\share"))
      .toBe(null);
    expect(workspaceRelativePath("src/App.tsx", null, "\\\\server\\share")).toBe("src/App.tsx");
  });

  it("is unbothered by trailing and repeated separators in the root", () => {
    expect(workspaceRelativePath(`${posixRoot}/src/App.tsx`, null, `${posixRoot}/`)).toBe("src/App.tsx");
    expect(workspaceRelativePath("C:\\Projects\\\\Mework\\src\\App.tsx", null, `${windowsRoot}\\`))
      .toBe("src/App.tsx");
  });
});
