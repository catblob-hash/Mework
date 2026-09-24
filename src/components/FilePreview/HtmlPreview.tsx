import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import type { Root } from "hast";
import { Info } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ImgHTMLAttributes, MouseEvent as ReactMouseEvent, ReactNode } from "react";
import { createPortal } from "react-dom";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { useI18n } from "../../i18n";
import { localLinkTarget, scrollToFragment } from "../../lib/documentLinks";
import { externalHttpUrl } from "../../lib/externalLinks";
import { resolveDocumentReference } from "../../lib/fileViewers";
import {
  HTML_BODY_CLASS,
  HTML_ROOT_CLASS,
  inlineStylesheetImports,
  prepareHtmlPreview,
  rewriteSelector,
  rewriteStylesheetUrls,
  stylesheetImports,
  stylesheetReferences
} from "../../lib/htmlPreview";
import type { HtmlStyleSource } from "../../lib/htmlPreview";

export interface HtmlPreviewResources {
  /** A workspace file's text, or null when it cannot be read as text. */
  readText: (path: string) => Promise<string | null>;
  /** A workspace picture as a `data:` URL, or null when it cannot be shown. */
  readImage: (path: string) => Promise<string | null>;
}

/**
 * What the page looks like before any of its own CSS: the defaults a browser
 * gives a document, which is what the page's authors wrote against.
 *
 * `all: initial` on the host is what stops the app's own inherited font and
 * colour from leaking in; the rest re-creates the canvas a browser tab starts
 * with. The host contains its paint, so a `position: fixed` banner in the page
 * pins to the preview rather than to the app window.
 */
const BASE_SHEET = `
:host { all: initial; display: block; overflow: auto; }
.${HTML_ROOT_CLASS} {
  display: block; min-height: 100%; color-scheme: light;
  background: Canvas; color: CanvasText;
  font-family: ui-serif, Georgia, serif; font-size: 16px; line-height: normal;
}
.${HTML_BODY_CLASS} { display: block; margin: 8px; }
:host([data-inline]) .${HTML_BODY_CLASS} { margin: 0; }
:host([data-inline]) .${HTML_ROOT_CLASS} { min-height: 0; font-family: system-ui, sans-serif; font-size: 13px; }
/* The page's root fills the preview however short the page is, as a document's
   background fills a browser tab. */
.mw-html-frame { display: flex; flex-direction: column; min-height: 100%; }
.mw-html-frame > .${HTML_ROOT_CLASS} { flex: 1 0 auto; }
.mw-embed {
  display: inline-flex; align-items: center; justify-content: center; box-sizing: border-box;
  min-width: 160px; min-height: 60px; max-width: 100%; padding: 8px;
  border: 1px dashed GrayText; color: GrayText; font: 12px system-ui, sans-serif;
}
.mw-embed::before { content: attr(data-label); }
img[data-mw-pending] { visibility: hidden; }
`;

/** Constructable stylesheets are what a strict style CSP still allows; a host without them gets unstyled markup. */
function makeSheet(css: string): CSSStyleSheet | null {
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(css);
    return sheet;
  } catch {
    return null;
  }
}

/** Points every selector at the stand-ins for `<html>` and `<body>`, through nested rules too. */
function rewriteRules(rules: CSSRuleList): void {
  for (const rule of Array.from(rules)) {
    if (typeof CSSStyleRule !== "undefined" && rule instanceof CSSStyleRule) {
      const rewritten = rewriteSelector(rule.selectorText);
      if (rewritten !== rule.selectorText) rule.selectorText = rewritten;
    }
    const nested = (rule as CSSRule & { cssRules?: CSSRuleList }).cssRules;
    if (nested) rewriteRules(nested);
  }
}

const IMAGE_EXTENSIONS = /\.(?:png|apng|jpe?g|jfif|gif|webp|avif|bmp|ico|cur|svg|tiff?)(?:[?#].*)?$/i;

function withoutQuery(reference: string): string {
  return reference.split(/[?#]/)[0];
}

/**
 * One stylesheet, with its imports read in and its pictures turned into
 * `data:` URLs, so that nothing in it asks the engine for anything.
 */
async function resolveStylesheet(
  css: string,
  sheetPath: string,
  resources: HtmlPreviewResources,
  depth: number
): Promise<string> {
  const imports = new Map<string, string | null>();
  if (depth < 4) {
    await Promise.all(stylesheetImports(css).map(async ({ reference }) => {
      const path = REMOTE.test(reference) ? null : resolveDocumentReference(sheetPath, withoutQuery(reference));
      if (path === null) return;
      const text = await resources.readText(path);
      imports.set(reference, text === null ? null : await resolveStylesheet(text, path, resources, depth + 1));
    }));
  }
  const inlined = inlineStylesheetImports(css, (reference, media) => {
    const text = imports.get(reference);
    if (!text) return null;
    return media ? `@media ${media} {\n${text}\n}` : text;
  });
  const pictures = new Map<string, string | null>();
  await Promise.all(stylesheetReferences(inlined).map(async (reference) => {
    if (!IMAGE_EXTENSIONS.test(reference)) return;
    const path = resolveDocumentReference(sheetPath, withoutQuery(reference));
    pictures.set(reference, path === null ? null : await resources.readImage(path));
  }));
  return rewriteStylesheetUrls(inlined, (reference) => pictures.get(reference) ?? null);
}

const REMOTE = /^\s*(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

async function loadStylesheets(
  styles: readonly HtmlStyleSource[],
  documentPath: string,
  resources: HtmlPreviewResources
): Promise<string[]> {
  return Promise.all(styles.map(async (style) => {
    if (style.kind === "inline") return resolveStylesheet(style.text, documentPath, resources, 0);
    if (REMOTE.test(style.href)) return "";
    const path = resolveDocumentReference(documentPath, withoutQuery(style.href));
    if (path === null) return "";
    const text = await resources.readText(path);
    if (text === null) return "";
    const resolved = await resolveStylesheet(text, path, resources, 0);
    return style.media ? `@media ${style.media} {\n${resolved}\n}` : resolved;
  }));
}

/**
 * A workspace HTML file, drawn from its markup and its own stylesheets with
 * nothing in it running.
 *
 * The page lives in a shadow root so its CSS styles the page and not the app,
 * and the app's CSS stays out of the page. Links are followed by the pane: one to
 * another workspace file opens it beside this one, one to the web goes to the
 * system browser like every other link in the app, and a fragment scrolls.
 */
export function HtmlPreview({
  path,
  content,
  resources,
  onOpenFile,
  inline = false
}: {
  path: string;
  content: string;
  resources: HtmlPreviewResources;
  /** Opens a workspace file a link in the page points at. */
  onOpenFile: (path: string, line: number | null) => void;
  /**
   * A fragment inside something else — a notebook's output — rather than a page
   * filling the pane: as tall as its content, without the page margin.
   */
  inline?: boolean;
}) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const [shadow, setShadow] = useState<ShadowRoot | null>(null);
  const [pictures, setPictures] = useState<ReadonlyMap<string, string | null>>(() => new Map());
  const page = useMemo(() => prepareHtmlPreview(content), [content]);
  const resourcesRef = useRef(resources);
  resourcesRef.current = resources;

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    setShadow(host.shadowRoot ?? host.attachShadow({ mode: "open" }));
  }, []);

  // The page's stylesheets, read and resolved, then adopted by the shadow root in
  // cascade order behind the defaults.
  useEffect(() => {
    if (!shadow) return;
    let cancelled = false;
    const base = makeSheet(BASE_SHEET);
    shadow.adoptedStyleSheets = base ? [base] : [];
    void loadStylesheets(page.styles, path, resourcesRef.current).then((texts) => {
      if (cancelled) return;
      const sheets = texts.map((text) => {
        const sheet = makeSheet(text);
        if (sheet) rewriteRules(sheet.cssRules);
        return sheet;
      }).filter((sheet): sheet is CSSStyleSheet => sheet !== null);
      shadow.adoptedStyleSheets = base ? [base, ...sheets] : sheets;
    }).catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [page, path, shadow]);

  useEffect(() => {
    let cancelled = false;
    setPictures(new Map());
    for (const reference of page.images) {
      const resolved = resolveDocumentReference(path, withoutQuery(reference));
      const load = resolved === null ? Promise.resolve(null) : resourcesRef.current.readImage(resolved);
      void load.catch(() => null).then((source) => {
        if (cancelled) return;
        setPictures((current) => new Map(current).set(reference, source));
      });
    }
    return () => {
      cancelled = true;
    };
  }, [page, path]);

  const tree = useMemo(() => {
    const embedLabels: Record<string, string> = {
      video: t("视频未在预览中播放", "Video is not played in the preview"),
      audio: t("音频未在预览中播放", "Audio is not played in the preview"),
      canvas: t("画布需要脚本绘制", "A canvas is drawn by scripts")
    };
    const components = {
      img: ({ "data-mw-src": workspaceSource, "data-mw-remote-src": remoteSource, ...props }: ImgHTMLAttributes<HTMLImageElement> & {
        "data-mw-src"?: string;
        "data-mw-remote-src"?: string;
      }) => {
        if (remoteSource !== undefined) {
          // The CSP refuses remote pictures; the page's own words for it stand in.
          return props.alt ? <span data-mw-missing-image="">{props.alt}</span> : null;
        }
        if (workspaceSource === undefined) return <img {...props} />;
        const source = pictures.get(workspaceSource);
        if (source === undefined) return <img {...props} data-mw-pending="" />;
        if (source === null) return props.alt ? <span data-mw-missing-image="">{props.alt}</span> : null;
        return <img {...props} src={source} />;
      },
      "mw-embed": ({ "data-kind": kind }: { "data-kind"?: string }) => (
        <span
          className="mw-embed"
          data-kind={kind}
          data-label={embedLabels[kind ?? ""] ?? t("嵌入内容未在预览中显示", "Embedded content is not shown in the preview")}
        />
      )
    };
    const root: Root = {
      type: "root",
      children: [{
        type: "element",
        tagName: "div",
        properties: { ...page.rootProperties, className: [HTML_ROOT_CLASS, ...classList(page.rootProperties.className)] },
        children: [{
          type: "element",
          tagName: "div",
          properties: { ...page.bodyProperties, className: [HTML_BODY_CLASS, ...classList(page.bodyProperties.className)] },
          children: page.body
        }]
      }]
    };
    return toJsxRuntime(root, {
      Fragment,
      jsx,
      jsxs,
      // The runtime's component map is typed for intrinsic elements only; the
      // placeholder is this preview's own tag.
      components: components as unknown as Parameters<typeof toJsxRuntime>[1]["components"],
      ignoreInvalidStyle: true
    }) as ReactNode;
  }, [page, pictures, t]);

  const onClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const anchor = event.target instanceof Element ? event.target.closest("a[href], area[href]") : null;
    if (!anchor) return;
    // Nothing in the page navigates on its own: the app window is not the page's
    // to leave, and an animated `href` could name something the markup did not.
    event.preventDefault();
    if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    const href = anchor.getAttribute("href") ?? "";
    // The web is the document-level interceptor's; it has already been handed over.
    if (externalHttpUrl(href) !== null) return;
    const target = localLinkTarget(href);
    if (!target) return;
    if (!target.path) {
      if (target.fragment) scrollToFragment(shadow, target.fragment);
      return;
    }
    const resolved = resolveDocumentReference(path, target.path);
    if (resolved !== null) onOpenFile(resolved, target.line);
  };

  return (
    <div className={inline ? "file-preview__html-inline" : "file-preview file-preview--html"}>
      {page.hadScripts && !inline && (
        <p className="file-preview__banner">
          <Info size={12} aria-hidden="true" />
          <span>{t("静态预览：页面里的脚本不会运行。", "Static preview: the page's scripts do not run.")}</span>
        </p>
      )}
      <div
        className={`file-preview__html-host${inline ? " file-preview__html-host--inline" : ""}`}
        ref={hostRef}
        aria-label={page.title ?? (path || undefined)}
        role="document"
        data-inline={inline || undefined}
      >
        {shadow && createPortal(
          <div
            className="mw-html-frame"
            onClickCapture={onClick}
            onSubmitCapture={(event) => event.preventDefault()}
          >
            {tree}
          </div>,
          shadow
        )}
      </div>
    </div>
  );
}

function classList(value: unknown): string[] {
  if (Array.isArray(value)) return value.map(String);
  return typeof value === "string" ? value.split(/\s+/).filter(Boolean) : [];
}
