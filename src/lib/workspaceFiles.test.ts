import { describe, expect, it } from "vitest";
import { workspaceRelativePath } from "./workspaceFiles";

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
