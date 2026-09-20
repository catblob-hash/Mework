import { describe, expect, it } from "vitest";
import { codeLanguage, fileViewerKind, imageMediaType, resolveDocumentReference } from "./fileViewers";

describe("fileViewerKind", () => {
  it("renders the Markdown family and nothing that merely looks like it", () => {
    for (const path of ["README.md", "docs/guide.markdown", "a/b.mdx", "SKILL.md"]) {
      expect(fileViewerKind(path), path).toBe("markdown");
    }
    expect(fileViewerKind("notes.mdb")).not.toBe("markdown");
  });

  it("shows the image types a WebView can decode, and no others", () => {
    for (const path of ["a.png", "a.JPG", "logo.svg", "icon.ico", "shot.webp"]) {
      expect(fileViewerKind(path), path).toBe("image");
    }
    // The tree gives these an image icon; nothing decodes them, so the viewer
    // says "binary" rather than drawing a broken picture.
    for (const path of ["scan.tif", "app.icns"]) {
      expect(fileViewerKind(path), path).not.toBe("image");
    }
  });

  it("claims a language only where it has a grammar for one", () => {
    expect(fileViewerKind("src/App.tsx")).toBe("code");
    expect(codeLanguage("src/App.tsx")).toBe("typescript");
    expect(fileViewerKind("Cargo.toml")).toBe("code");
    expect(fileViewerKind("notes.txt")).toBe("text");
    expect(codeLanguage("notes.txt")).toBe(null);
  });

  /** A file whose whole name is its type has no extension to sort it by. */
  it("recognizes the files that are named rather than suffixed", () => {
    expect(codeLanguage("Dockerfile")).toBe("shell");
    expect(codeLanguage("build/Makefile")).toBe("shell");
    expect(codeLanguage(".gitignore")).toBe("shell");
    expect(codeLanguage("LICENSE")).toBe(null);
  });
});

describe("resolveDocumentReference", () => {
  it("resolves against the document, not the workspace root", () => {
    expect(resolveDocumentReference("docs/guide.md", "./images/a.png")).toBe("docs/images/a.png");
    expect(resolveDocumentReference("docs/guide.md", "../assets/a.png")).toBe("assets/a.png");
    expect(resolveDocumentReference("docs/guide.md", "other.md")).toBe("docs/other.md");
    expect(resolveDocumentReference("readme.md", "docs/guide.md")).toBe("docs/guide.md");
  });

  /** A rooted reference in a repository's own prose means the repository's root. */
  it("treats a leading slash as the workspace root", () => {
    expect(resolveDocumentReference("docs/deep/guide.md", "/src/App.tsx")).toBe("src/App.tsx");
  });

  it("refuses a reference that climbs out of the workspace", () => {
    expect(resolveDocumentReference("readme.md", "../outside.md")).toBe(null);
    expect(resolveDocumentReference("docs/guide.md", "../../outside.md")).toBe(null);
    expect(resolveDocumentReference("readme.md", "  ")).toBe(null);
  });
});

describe("imageMediaType", () => {
  it("names the type a data URL has to carry", () => {
    expect(imageMediaType("a.png")).toBe("image/png");
    expect(imageMediaType("a.JPEG")).toBe("image/jpeg");
    expect(imageMediaType("a.svg")).toBe("image/svg+xml");
    expect(imageMediaType("a.txt")).toBe(null);
  });
});
