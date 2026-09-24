import { describe, expect, it } from "vitest";
import { headingSlug, localLinkTarget, scrollToFragment } from "./documentLinks";

describe("localLinkTarget", () => {
  it("reads the line a link names in each of the forms tools write", () => {
    expect(localLinkTarget("src/App.tsx:42")).toEqual({ path: "src/App.tsx", line: 42, fragment: null });
    expect(localLinkTarget("src/App.tsx:42:7")).toEqual({ path: "src/App.tsx", line: 42, fragment: null });
    expect(localLinkTarget("src/App.tsx#L42")).toEqual({ path: "src/App.tsx", line: 42, fragment: null });
    expect(localLinkTarget("src/App.tsx#L42-L50")).toEqual({ path: "src/App.tsx", line: 42, fragment: null });
    expect(localLinkTarget("src/App.tsx")).toEqual({ path: "src/App.tsx", line: null, fragment: null });
  });

  it("keeps a heading fragment apart from the file it is in", () => {
    expect(localLinkTarget("docs/guide.md#install")).toEqual({ path: "docs/guide.md", line: null, fragment: "install" });
    expect(localLinkTarget("#install")).toEqual({ path: "", line: null, fragment: "install" });
  });

  it("decodes the name and drops a query", () => {
    expect(localLinkTarget("my%20notes.md?plain=1#L3")).toEqual({ path: "my notes.md", line: 3, fragment: null });
    expect(localLinkTarget("100%.md")).toEqual({ path: "100%.md", line: null, fragment: null });
  });

  it("unwraps file URLs, including a Windows drive", () => {
    expect(localLinkTarget("file:///home/me/a.rs:3")).toEqual({ path: "/home/me/a.rs", line: 3, fragment: null });
    expect(localLinkTarget("file:///C:/work/a.rs")).toEqual({ path: "C:/work/a.rs", line: null, fragment: null });
    expect(localLinkTarget("C:\\work\\a.rs:9")).toEqual({ path: "C:\\work\\a.rs", line: 9, fragment: null });
  });

  it("leaves the web, mail and data to whoever handles them", () => {
    for (const href of ["https://example.com/a.md", "mailto:a@b.c", "data:text/plain,x", "//cdn.example/a.js", "javascript:alert(1)", "", "#", null]) {
      expect(localLinkTarget(href), String(href)).toBe(null);
    }
  });
});

describe("scrollToFragment", () => {
  it("finds an element by id before a heading by its text, and only inside its container", () => {
    const outer = document.createElement("div");
    outer.innerHTML = "<div id='a'><h2>Getting Started!</h2><p id='user-content-fn-1'>note</p></div><div id='b'><h2>Other</h2></div>";
    const scrolled: string[] = [];
    for (const element of outer.querySelectorAll("*")) {
      (element as HTMLElement).scrollIntoView = () => scrolled.push(element.textContent ?? "");
    }
    const first = outer.querySelector("#a")!;
    expect(scrollToFragment(first, "fn-1")).toBe(true);
    expect(scrollToFragment(first, "getting-started")).toBe(true);
    expect(scrollToFragment(first, "other")).toBe(false);
    expect(scrolled).toEqual(["note", "Getting Started!"]);
  });

  it("slugs headings the way documents link them", () => {
    expect(headingSlug("  Install & Run: Step 1 ")).toBe("install--run-step-1");
    expect(headingSlug("安装 步骤")).toBe("安装-步骤");
  });
});
