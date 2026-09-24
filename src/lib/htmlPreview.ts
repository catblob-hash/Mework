/**
 * A workspace HTML file, reduced to what can be shown without running it.
 *
 * The renderer's CSP allows no frames and no script but its own, so an HTML
 * document cannot be loaded as a page. What it can be is a tree: parsed here,
 * stripped of everything that runs or reaches out — scripts, handlers, frames,
 * forms that post, `javascript:` addresses — and rendered as ordinary elements
 * inside a shadow root, where its own stylesheets apply to it and to nothing
 * else. The result is the page as its markup and CSS describe it, which for a
 * static page is the page, and for an application is its shell.
 *
 * Everything the page references in the workspace — stylesheets, images — is
 * collected here and read by the caller; nothing is fetched by the engine,
 * because the CSP would refuse it anyway.
 */

import type { Element, ElementContent, Properties, Root, RootContent } from "hast";
import { fromHtml } from "hast-util-from-html";

export type HtmlStyleSource =
  | { kind: "inline"; text: string }
  | { kind: "link"; href: string; media: string | null };

export interface HtmlPreviewDocument {
  title: string | null;
  /** Attributes the document put on `<html>` and `<body>`, moved to the elements that stand in for them. */
  rootProperties: Properties;
  bodyProperties: Properties;
  body: ElementContent[];
  /** Every stylesheet, in the order the cascade has to see them. */
  styles: HtmlStyleSource[];
  /** Workspace images the body refers to, as written. */
  images: string[];
  /** Whether the page had scripts, which the preview never runs. */
  hadScripts: boolean;
}

/** Elements that are removed along with everything inside them. */
const DROPPED = new Set([
  "script", "template", "base", "meta", "link", "style", "title", "noframes", "param", "source", "track",
  "slot"
]);

/** Elements that would load or run something of their own; each leaves a placeholder behind. */
const EMBEDS = new Set(["iframe", "frame", "frameset", "object", "embed", "applet", "portal", "video", "audio", "canvas"]);

/** Attributes that act rather than describe. */
const DROPPED_PROPERTIES = new Set([
  "action", "formAction", "srcDoc", "ping", "httpEquiv", "autoFocus", "nonce", "integrity", "crossOrigin",
  "referrerPolicy", "background", "srcSet", "sizes", "target", "is", "popoverTarget", "popoverTargetAction",
  "commandFor", "command", "dynsrc", "lowsrc", "manifest"
]);

/** Schemes that execute or embed rather than point somewhere. */
const DANGEROUS_URL = /^\s*(?:javascript|vbscript|data)\s*:/i;

const REMOTE_URL = /^\s*(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

function propertyString(properties: Properties, key: string): string | null {
  const value = properties[key];
  if (Array.isArray(value)) return value.join(" ");
  if (value === undefined || value === null || value === false) return null;
  return String(value);
}

function textContent(node: ElementContent | Root): string {
  if (node.type === "text") return node.value;
  if (node.type !== "element" && node.type !== "root") return "";
  return node.children.map((child) => textContent(child as ElementContent)).join("");
}

interface Collector {
  styles: HtmlStyleSource[];
  images: Set<string>;
  title: string | null;
  hadScripts: boolean;
}

function isStylesheetLink(element: Element): boolean {
  const rel = propertyString(element.properties, "rel") ?? "";
  return rel.toLowerCase().split(/\s+/).includes("stylesheet");
}

/** Records what a dropped element contributes before it goes. */
function collect(element: Element, collector: Collector): void {
  switch (element.tagName) {
    case "script":
      collector.hadScripts = true;
      break;
    case "style":
      collector.styles.push({ kind: "inline", text: textContent(element) });
      break;
    case "link": {
      const href = propertyString(element.properties, "href");
      if (href && isStylesheetLink(element)) {
        collector.styles.push({ kind: "link", href, media: propertyString(element.properties, "media") });
      }
      break;
    }
    case "title":
      collector.title ??= textContent(element).trim() || null;
      break;
    default:
      break;
  }
}

function cleanProperties(element: Element, collector: Collector): Properties {
  const properties: Properties = {};
  for (const [key, value] of Object.entries(element.properties)) {
    if (/^on/i.test(key) || DROPPED_PROPERTIES.has(key)) continue;
    properties[key] = value;
  }
  for (const key of ["href", "xLinkHref"]) {
    const value = propertyString(properties, key);
    if (value === null) continue;
    // A link is followed by the preview's own click handler, never by the engine;
    // one that would execute is not kept even as text to follow.
    if (DANGEROUS_URL.test(value)) delete properties[key];
    // An SVG reference outside its own document would be a request.
    else if (key === "xLinkHref" && !value.startsWith("#") && element.tagName !== "a") delete properties[key];
  }
  const source = propertyString(properties, "src");
  if (source !== null) {
    delete properties.src;
    const isImage = element.tagName === "img" || (element.tagName === "input" && propertyString(properties, "type") === "image");
    if (isImage && /^\s*data:image\//i.test(source)) {
      properties.src = source;
    } else if (isImage && !REMOTE_URL.test(source)) {
      // Read from the workspace by the caller; drawn once it has the bytes.
      properties.dataMwSrc = source;
      collector.images.add(source);
    } else if (isImage) {
      properties.dataMwRemoteSrc = source;
    }
  }
  // SVG `<image href="…">` is a picture too.
  if (element.tagName === "image") {
    const href = propertyString(properties, "href") ?? propertyString(properties, "xLinkHref");
    delete properties.href;
    delete properties.xLinkHref;
    if (href && /^\s*data:image\//i.test(href)) properties.href = href;
    else if (href && !REMOTE_URL.test(href)) {
      properties.dataMwSrc = href;
      collector.images.add(href);
    }
  }
  return properties;
}

/** An SVG animation that rewrites a link could make a static `href` execute when clicked. */
function animatesLink(element: Element): boolean {
  if (element.tagName !== "animate" && element.tagName !== "set") return false;
  return /href/i.test(propertyString(element.properties, "attributeName") ?? "");
}

function sanitizeChildren(children: readonly RootContent[], collector: Collector): ElementContent[] {
  const result: ElementContent[] = [];
  for (const child of children) {
    if (child.type === "text") {
      result.push(child);
      continue;
    }
    if (child.type !== "element") continue;
    const element = child;
    if (DROPPED.has(element.tagName)) {
      collect(element, collector);
      continue;
    }
    if (animatesLink(element)) continue;
    if (EMBEDS.has(element.tagName)) {
      result.push({
        type: "element",
        tagName: "mw-embed",
        properties: { dataKind: element.tagName },
        children: []
      });
      continue;
    }
    // Scripts never run here, so what a page says in their absence is exactly
    // what it should show.
    if (element.tagName === "noscript") {
      result.push(...sanitizeChildren(element.children, collector));
      continue;
    }
    result.push({
      type: "element",
      tagName: element.tagName,
      properties: cleanProperties(element, collector),
      // `<template>` content lives on `.content`, not `.children`, and is dropped above.
      children: sanitizeChildren(element.children, collector)
    });
  }
  return result;
}

function findElement(node: Root | Element, tagName: string): Element | null {
  for (const child of node.children) {
    if (child.type !== "element") continue;
    if (child.tagName === tagName) return child;
    const found = findElement(child, tagName);
    if (found) return found;
  }
  return null;
}

/** Parses and strips a document, and lists what it needs from the workspace. */
export function prepareHtmlPreview(source: string): HtmlPreviewDocument {
  const tree = fromHtml(source);
  const collector: Collector = { styles: [], images: new Set(), title: null, hadScripts: false };
  const html = findElement(tree, "html");
  const head = html ? findElement(html, "head") : null;
  const body = html ? findElement(html, "body") : null;
  // The head is walked only for what it contributes: its stylesheets and title.
  if (head) sanitizeChildren(head.children, collector);
  const content = sanitizeChildren(body?.children ?? tree.children, collector);
  return {
    title: collector.title,
    rootProperties: html ? cleanProperties(html, collector) : {},
    bodyProperties: body ? cleanProperties(body, collector) : {},
    body: content,
    styles: collector.styles,
    images: [...collector.images],
    hadScripts: collector.hadScripts
  };
}

/** Class names that stand in for `<html>` and `<body>`, which cannot exist inside a shadow root. */
export const HTML_ROOT_CLASS = "mw-html-root";
export const HTML_BODY_CLASS = "mw-html-body";

/**
 * Points a selector at the stand-ins: `html`, `:root` and `body` become the
 * classes of the elements that play them.
 */
export function rewriteSelector(selector: string): string {
  return selector
    .replace(/:root\b/gi, `.${HTML_ROOT_CLASS}`)
    .replace(/(^|[\s>+~,(])html(?=$|[\s>+~,.:#[)])/gi, `$1.${HTML_ROOT_CLASS}`)
    .replace(/(^|[\s>+~,(])body(?=$|[\s>+~,.:#[)])/gi, `$1.${HTML_BODY_CLASS}`);
}

const CSS_URL = /url\(\s*(?:"([^"]*)"|'([^']*)'|([^)\s]*))\s*\)/gi;
const CSS_IMPORT = /@import\s+(?:url\(\s*)?(?:"([^"]*)"|'([^']*)'|([^)\s;]+))\s*\)?\s*([^;]*);/gi;

/** Every `url(…)` in a stylesheet that names something in the workspace. */
export function stylesheetReferences(css: string): string[] {
  const references = new Set<string>();
  for (const match of css.matchAll(CSS_URL)) {
    const reference = (match[1] ?? match[2] ?? match[3] ?? "").trim();
    if (reference && !REMOTE_URL.test(reference) && !reference.startsWith("#")) references.add(reference);
  }
  return [...references];
}

/** Every `@import` a stylesheet makes, with the media it was made for. */
export function stylesheetImports(css: string): { reference: string; media: string }[] {
  const imports: { reference: string; media: string }[] = [];
  for (const match of css.matchAll(CSS_IMPORT)) {
    const reference = (match[1] ?? match[2] ?? match[3] ?? "").trim();
    if (reference) imports.push({ reference, media: (match[4] ?? "").trim() });
  }
  return imports;
}

/**
 * Rewrites a stylesheet's addresses: a workspace reference becomes what
 * `resolve` answers — a `data:` URL, or null for something that cannot be shown
 * — and anything remote is removed, since the CSP would refuse it and report
 * every refusal.
 */
export function rewriteStylesheetUrls(css: string, resolve: (reference: string) => string | null): string {
  return css.replace(CSS_URL, (whole, double?: string, single?: string, bare?: string) => {
    const reference = (double ?? single ?? bare ?? "").trim();
    if (!reference || reference.startsWith("#") || /^data:/i.test(reference)) return whole;
    if (REMOTE_URL.test(reference)) return "none";
    const resolved = resolve(reference);
    return resolved === null ? "none" : `url("${resolved}")`;
  });
}

/** Replaces each `@import` with what `inline` answers for it, or with nothing. */
export function inlineStylesheetImports(css: string, inline: (reference: string, media: string) => string | null): string {
  return css.replace(CSS_IMPORT, (_whole, double?: string, single?: string, bare?: string, media?: string) => {
    const reference = (double ?? single ?? bare ?? "").trim();
    return inline(reference, (media ?? "").trim()) ?? "";
  });
}
