import type { Element } from "hast";
import { describe, expect, it } from "vitest";
import {
  inlineStylesheetImports,
  prepareHtmlPreview,
  rewriteSelector,
  rewriteStylesheetUrls,
  stylesheetImports,
  stylesheetReferences
} from "./htmlPreview";

function find(nodes: readonly unknown[], tagName: string): Element | null {
  for (const node of nodes as Element[]) {
    if (node.type !== "element") continue;
    if (node.tagName === tagName) return node;
    const nested = find(node.children, tagName);
    if (nested) return nested;
  }
  return null;
}

describe("prepareHtmlPreview", () => {
  const page = prepareHtmlPreview(`<!doctype html>
<html lang="en"><head>
  <title> Home </title>
  <meta http-equiv="refresh" content="0;url=https://evil.example">
  <link rel="stylesheet" href="css/site.css" media="screen">
  <style>h1 { color: teal }</style>
  <script src="app.js"></script>
</head>
<body class="page" onload="boot()">
  <h1 style="margin: 0" onclick="steal()">Hi</h1>
  <a href="javascript:alert(1)">bad</a> <a href="docs/next.html#top" target="_blank">next</a>
  <img src="img/logo.png" alt="logo" srcset="a.png 2x"> <img src="https://cdn.example/x.png" alt="remote">
  <iframe src="https://example.com"></iframe>
  <noscript><p>Enable JS</p></noscript>
  <form action="https://evil.example/post"><button formaction="https://evil.example">Go</button></form>
  <svg><a href="#x"><set attributeName="href" to="javascript:alert(1)"/></a><image href="icons/a.svg"/></svg>
</body></html>`);

  it("collects what the head contributes and drops the rest of it", () => {
    expect(page.title).toBe("Home");
    expect(page.hadScripts).toBe(true);
    expect(page.styles).toEqual([
      { kind: "link", href: "css/site.css", media: "screen" },
      { kind: "inline", text: "h1 { color: teal }" }
    ]);
    expect(page.rootProperties.lang).toBe("en");
    expect(page.bodyProperties.className).toEqual(["page"]);
    expect(page.bodyProperties).not.toHaveProperty("onLoad");
  });

  it("strips handlers, executable addresses, frames and posting forms", () => {
    const heading = find(page.body, "h1")!;
    expect(heading.properties).toEqual({ style: "margin: 0" });
    const links = page.body.filter((node): node is Element => node.type === "element" && node.tagName === "a");
    expect(links[0].properties.href).toBeUndefined();
    expect(links[1].properties).toEqual({ href: "docs/next.html#top" });
    expect(find(page.body, "iframe")).toBeNull();
    expect(find(page.body, "mw-embed")?.properties.dataKind).toBe("iframe");
    expect(find(page.body, "form")?.properties.action).toBeUndefined();
    expect(find(page.body, "button")?.properties.formAction).toBeUndefined();
    expect(find(page.body, "set")).toBeNull();
    // What a page says without scripts is what the preview shows.
    expect(find(page.body, "noscript")).toBeNull();
    expect(find(page.body, "p")).not.toBeNull();
  });

  it("hands workspace pictures to the caller and keeps remote ones out of the engine's reach", () => {
    expect(page.images).toEqual(["img/logo.png", "icons/a.svg"]);
    const images = page.body.filter((node): node is Element => node.type === "element" && node.tagName === "img");
    expect(images[0].properties).toEqual({ alt: "logo", dataMwSrc: "img/logo.png" });
    expect(images[1].properties).toEqual({ alt: "remote", dataMwRemoteSrc: "https://cdn.example/x.png" });
  });
});

describe("stylesheets", () => {
  it("points html, body and :root at the elements that stand in for them", () => {
    expect(rewriteSelector("html, body > main, :root, .body, #html, html.dark")).toBe(
      ".mw-html-root, .mw-html-body > main, .mw-html-root, .body, #html, .mw-html-root.dark"
    );
  });

  it("finds and rewrites the addresses a stylesheet uses", () => {
    const css = "a { background: url(img/a.png) } b { background: url('https://x.example/b.png') } @font-face { src: url(\"f.woff2\") }";
    expect(stylesheetReferences(css)).toEqual(["img/a.png", "f.woff2"]);
    expect(rewriteStylesheetUrls(css, (reference) => (reference === "img/a.png" ? "data:image/png;base64,AA" : null))).toBe(
      "a { background: url(\"data:image/png;base64,AA\") } b { background: none } @font-face { src: none }"
    );
  });

  it("inlines imports, keeping the media they were made for", () => {
    const css = "@import url(\"base.css\") screen;\n@import 'print.css';\nbody{}";
    expect(stylesheetImports(css)).toEqual([
      { reference: "base.css", media: "screen" },
      { reference: "print.css", media: "" }
    ]);
    expect(inlineStylesheetImports(css, (reference, media) => (reference === "base.css" ? `/*base ${media}*/` : null))).toBe(
      "/*base screen*/\n\nbody{}"
    );
  });
});
