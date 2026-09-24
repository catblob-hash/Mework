import { describe, expect, it } from "vitest";
import {
  binaryMediaType,
  codeLanguage,
  fenceLanguage,
  fileViewerKind,
  hasSourceForm,
  imageMediaType,
  readsBytes,
  resolveDocumentReference
} from "./fileViewers";

describe("fileViewerKind", () => {
  it("renders the Markdown family and nothing that merely looks like it", () => {
    for (const path of ["README.md", "docs/guide.markdown", "a/b.mdx", "SKILL.md"]) {
      expect(fileViewerKind(path), path).toBe("markdown");
    }
    expect(fileViewerKind("notes.mdb")).not.toBe("markdown");
  });

  it("shows the image types a WebView can decode, and no others", () => {
    for (const path of ["a.png", "a.JPG", "logo.svg", "icon.ico", "shot.webp", "photo.heic", "scan.tif"]) {
      expect(fileViewerKind(path), path).toBe("image");
    }
    // The tree gives these an image icon; nothing decodes them, so the viewer
    // says "binary" rather than drawing a broken picture.
    for (const path of ["app.icns", "layers.psd"]) {
      expect(fileViewerKind(path), path).not.toBe("image");
    }
  });

  it("gives every kind of file a viewer of its own", () => {
    expect(fileViewerKind("site/index.html")).toBe("html");
    expect(fileViewerKind("paper.pdf")).toBe("pdf");
    expect(fileViewerKind("voice.mp3")).toBe("audio");
    expect(fileViewerKind("clip.mp4")).toBe("video");
    expect(fileViewerKind("Inter.woff2")).toBe("font");
    expect(fileViewerKind("data/table.tsv")).toBe("csv");
    expect(fileViewerKind("analysis.ipynb")).toBe("notebook");
  });

  it("knows which files have a source to switch to, and which are read as bytes", () => {
    for (const path of ["a.md", "a.html", "a.csv", "a.ipynb", "a.svg"]) expect(hasSourceForm(path), path).toBe(true);
    for (const path of ["a.png", "a.pdf", "a.ts", "a.txt"]) expect(hasSourceForm(path), path).toBe(false);
    for (const path of ["a.png", "a.pdf", "a.mp3", "a.ttf"]) expect(readsBytes(path), path).toBe(true);
    for (const path of ["a.md", "a.mp4", "a.html"]) expect(readsBytes(path), path).toBe(false);
    expect(binaryMediaType("a.pdf")).toBe("application/pdf");
    expect(binaryMediaType("a.flac")).toBe("audio/flac");
    expect(binaryMediaType("a.otf")).toBe("font/otf");
  });

  it("claims a language only where it has a grammar for one", () => {
    expect(fileViewerKind("src/App.tsx")).toBe("code");
    expect(codeLanguage("src/App.tsx")).toBe("tsx");
    expect(codeLanguage("src/main.ts")).toBe("typescript");
    for (const [path, language] of [
      ["lib/a.dart", "dart"], ["a.kt", "kotlin"], ["a.scala", "scala"], ["build.gradle", "groovy"],
      ["a.hs", "haskell"], ["a.ex", "elixir"], ["a.clj", "lisp"], ["a.zig", "zig"], ["a.ps1", "powershell"],
      ["run.bat", "batch"], ["main.tf", "hcl"], ["flake.nix", "nix"], ["paper.tex", "latex"],
      ["fix.patch", "diff"], ["shader.frag", "shader"], ["a.sol", "solidity"], ["a.jl", "julia"],
      ["setup.cfg", "ini"], ["a.scss", "scss"], ["server.log", "log"], ["a.m", "objectivec"]
    ] as const) {
      expect(codeLanguage(path), path).toBe(language);
      expect(fileViewerKind(path), path).toBe("code");
    }
    expect(fileViewerKind("Cargo.toml")).toBe("code");
    expect(fileViewerKind("notes.txt")).toBe("text");
    expect(codeLanguage("notes.txt")).toBe(null);
  });

  /** A file whose whole name is its type has no extension to sort it by. */
  it("recognizes the files that are named rather than suffixed", () => {
    expect(codeLanguage("Dockerfile")).toBe("dockerfile");
    expect(codeLanguage("Dockerfile.dev")).toBe("dockerfile");
    expect(codeLanguage("build/Makefile")).toBe("makefile");
    expect(codeLanguage("CMakeLists.txt")).toBe("cmake");
    expect(codeLanguage("Cargo.lock")).toBe("toml");
    expect(codeLanguage(".gitignore")).toBe("shell");
    expect(codeLanguage(".env.local")).toBe("shell");
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

describe("fenceLanguage", () => {
  it("reads the name a fence gives, as an alias or as an extension", () => {
    expect(fenceLanguage("ts")).toBe("typescript");
    expect(fenceLanguage("tsx title=\"a.tsx\"")).toBe("tsx");
    expect(fenceLanguage("Python")).toBe("python");
    expect(fenceLanguage("console")).toBe("shell");
    expect(fenceLanguage("c#")).toBe("csharp");
    expect(fenceLanguage("diff")).toBe("diff");
    expect(fenceLanguage("klingon")).toBe(null);
    expect(fenceLanguage("")).toBe(null);
    expect(fenceLanguage(null)).toBe(null);
  });
});
