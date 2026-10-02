import { Maximize, Minus, Plus, Scan } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { PDFDocumentProxy, RenderTask, TextLayer as PdfTextLayer } from "pdfjs-dist/legacy/build/pdf.mjs";
import { useI18n } from "../../i18n";
import { IconButton } from "../Common";
import { loadPdfJs, openPdfDocument, type PdfJs } from "../../lib/pdfDocument";
import { dataUrlBytes, formatBytes } from "./format";
import { attachTextLayerSelection } from "./pdfTextSelection";

/** Past this many device pixels a page is drawn at a lower resolution rather than not at all. */
const MAX_CANVAS_PIXELS = 16_777_216;
const ZOOM_STEPS = [0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3, 4];
const PAGE_GAP = 12;
const PAGE_PADDING = 16;

interface PageSize {
  width: number;
  height: number;
}

/**
 * A PDF, one canvas per page, drawn as the pages scroll near.
 *
 * Each page gets a text layer over its drawing, so a reader can select and copy
 * what the page says. The document opens fitted to the pane's width.
 */
export function PdfViewer({ source, bytes }: { source: string; bytes: number | null }) {
  const { t } = useI18n();
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [library, setLibrary] = useState<PdfJs | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sizes, setSizes] = useState<readonly PageSize[]>([]);
  const [zoom, setZoom] = useState<number | "width">("width");
  const [width, setWidth] = useState(0);
  const [currentPage, setCurrentPage] = useState(1);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    let destroy: (() => void) | null = null;
    setPdf(null);
    setError(null);
    setSizes([]);
    setZoom("width");
    void loadPdfJs().then(async (pdfjs) => {
      if (cancelled) return;
      const task = openPdfDocument(pdfjs, dataUrlBytes(source));
      destroy = () => void task.destroy();
      const loaded = await task.promise;
      if (cancelled) return;
      // Every page's size is read before anything is drawn, so the scroll height
      // is right from the start and jumping to a page lands on it.
      const pageSizes: PageSize[] = [];
      for (let index = 1; index <= loaded.numPages; index += 1) {
        const page = await loaded.getPage(index);
        if (cancelled) return;
        const viewport = page.getViewport({ scale: 1 });
        pageSizes.push({ width: viewport.width, height: viewport.height });
      }
      setLibrary(pdfjs);
      setSizes(pageSizes);
      setPdf(loaded);
    }).catch((reason: unknown) => {
      if (cancelled) return;
      const name = typeof reason === "object" && reason !== null && "name" in reason ? String((reason as { name: unknown }).name) : "";
      setError(name === "PasswordException"
        ? t("这个 PDF 有密码保护，无法预览。", "This PDF is password-protected and cannot be previewed.")
        : t("无法读取这个 PDF：{reason}", "Could not read this PDF: {reason}", {
          reason: reason instanceof Error ? reason.message : String(reason)
        }));
    });
    return () => {
      cancelled = true;
      destroy?.();
    };
  }, [source, t]);

  useEffect(() => {
    const scroller = scrollRef.current;
    if (!scroller || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => setWidth(entry?.contentRect.width ?? 0));
    observer.observe(scroller);
    setWidth(scroller.clientWidth);
    return () => observer.disconnect();
  }, []);

  const widest = useMemo(() => sizes.reduce((max, size) => Math.max(max, size.width), 0), [sizes]);
  const fitScale = widest > 0 && width > 0 ? Math.max(0.1, (width - PAGE_PADDING * 2) / widest) : 1;
  const scale = zoom === "width" ? fitScale : zoom;

  const step = (direction: 1 | -1) => {
    const next = direction > 0
      ? ZOOM_STEPS.find((value) => value > scale + 1e-3) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1]
      : [...ZOOM_STEPS].reverse().find((value) => value < scale - 1e-3) ?? ZOOM_STEPS[0];
    setZoom(next);
  };

  // Which page is on screen is read from where the pages are, not tracked per
  // page: at any moment it is the one whose top was last scrolled past.
  const onScroll = useCallback(() => {
    const scroller = scrollRef.current;
    if (!scroller) return;
    const middle = scroller.scrollTop + scroller.clientHeight / 3;
    let top = PAGE_PADDING;
    let page = 1;
    for (let index = 0; index < sizes.length; index += 1) {
      const height = sizes[index].height * scale;
      if (middle < top + height + PAGE_GAP) {
        page = index + 1;
        break;
      }
      top += height + PAGE_GAP;
      page = index + 1;
    }
    setCurrentPage(page);
  }, [scale, sizes]);

  if (error) return <p className="files-pane__error" role="alert">{error}</p>;

  return (
    <div className="file-preview file-preview--pdf">
      <div className="file-preview__pdf-scroll" ref={scrollRef} onScroll={onScroll}>
        {!pdf || !library
          ? <p className="files-pane__notice">{t("正在读取…", "Loading…")}</p>
          : (
            // One selection region across every page; the space between pages still starts none.
            <div className="file-preview__pdf-pages" data-selection-region>
              {sizes.map((size, index) => (
                <PdfPage
                  key={index}
                  pdf={pdf}
                  library={library}
                  pageNumber={index + 1}
                  size={size}
                  scale={scale}
                  root={scrollRef}
                />
              ))}
            </div>
          )}
      </div>
      <div className="file-preview__toolbar" role="toolbar" aria-label={t("PDF 缩放", "PDF zoom")}>
        <IconButton className="file-preview__tool" label={t("缩小", "Zoom out")} onClick={() => step(-1)}>
          <Minus size={13} aria-hidden="true" />
        </IconButton>
        <button type="button" className="file-preview__zoom-label" title={t("适应宽度", "Fit width")} onClick={() => setZoom("width")}>
          {`${Math.round(scale * 100)}%`}
        </button>
        <IconButton className="file-preview__tool" label={t("放大", "Zoom in")} onClick={() => step(1)}>
          <Plus size={13} aria-hidden="true" />
        </IconButton>
        <IconButton className="file-preview__tool" label={t("实际大小", "Actual size")} aria-pressed={zoom === 1} onClick={() => setZoom(1)}>
          <Scan size={13} aria-hidden="true" />
        </IconButton>
        <IconButton className="file-preview__tool" label={t("适应宽度", "Fit width")} aria-pressed={zoom === "width"} onClick={() => setZoom("width")}>
          <Maximize size={13} aria-hidden="true" />
        </IconButton>
        <span className="file-preview__meta">
          {sizes.length > 0 && t("第 {page} / {total} 页", "Page {page} / {total}", { page: currentPage, total: sizes.length })}
          {sizes.length > 0 && bytes !== null ? " · " : ""}
          {bytes !== null ? formatBytes(bytes) : ""}
        </span>
      </div>
    </div>
  );
}

/**
 * One page: a box of the right size at once, a drawing once it scrolls near.
 *
 * A zoom redraws only the pages that are near; the others keep their old drawing
 * stretched into the new box until they come into view, which is what keeps a
 * zoom on a long document from drawing all of it.
 */
const PdfPage = memo(function PdfPage({
  pdf,
  library,
  pageNumber,
  size,
  scale,
  root
}: {
  pdf: PDFDocumentProxy;
  library: PdfJs;
  pageNumber: number;
  size: PageSize;
  scale: number;
  root: React.RefObject<HTMLDivElement | null>;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const textRef = useRef<HTMLDivElement>(null);
  const [near, setNear] = useState(false);
  const drawnScale = useRef<number | null>(null);

  useEffect(() => {
    const box = boxRef.current;
    if (!box || typeof IntersectionObserver === "undefined") {
      setNear(true);
      return;
    }
    const observer = new IntersectionObserver(([entry]) => setNear(entry?.isIntersecting ?? false), {
      root: root.current,
      rootMargin: "600px 0px"
    });
    observer.observe(box);
    return () => observer.disconnect();
  }, [root]);

  useEffect(() => {
    if (!near || drawnScale.current === scale) return;
    const canvas = canvasRef.current;
    const text = textRef.current;
    if (!canvas || !text) return;
    let cancelled = false;
    const running: { render: RenderTask | null; layer: PdfTextLayer | null; detach: (() => void) | null } = {
      render: null,
      layer: null,
      detach: null
    };
    const timer = window.setTimeout(() => {
      void pdf.getPage(pageNumber).then(async (page) => {
        if (cancelled) return;
        const viewport = page.getViewport({ scale });
        const ratio = Math.min(
          window.devicePixelRatio || 1,
          Math.sqrt(MAX_CANVAS_PIXELS / Math.max(1, viewport.width * viewport.height))
        );
        // Drawn into a fresh canvas and swapped in, so the old drawing stays up
        // until the new one is complete instead of flashing blank.
        const next = document.createElement("canvas");
        next.width = Math.floor(viewport.width * ratio);
        next.height = Math.floor(viewport.height * ratio);
        running.render = page.render({
          canvas: next,
          viewport,
          transform: ratio === 1 ? undefined : [ratio, 0, 0, ratio, 0, 0]
        });
        await running.render.promise;
        if (cancelled) return;
        canvas.width = next.width;
        canvas.height = next.height;
        canvas.getContext("2d")?.drawImage(next, 0, 0);
        drawnScale.current = scale;
        text.replaceChildren();
        running.layer = new library.TextLayer({ textContentSource: page.streamTextContent(), container: text, viewport });
        await running.layer.render();
        if (cancelled) return;
        running.detach = attachTextLayerSelection(text);
      }).catch(() => undefined);
    }, drawnScale.current === null ? 0 : 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      running.render?.cancel();
      running.layer?.cancel();
      running.detach?.();
    };
  }, [pdf, library, near, pageNumber, scale]);

  const style = {
    width: size.width * scale,
    height: size.height * scale,
    "--scale-factor": String(scale),
    "--total-scale-factor": String(scale),
    "--user-unit": "1"
  } as CSSProperties;

  return (
    <div className="file-preview__pdf-page" ref={boxRef} style={style} data-page-number={pageNumber}>
      <canvas ref={canvasRef} className="file-preview__pdf-canvas" />
      <div ref={textRef} className="textLayer" data-native-selection />
    </div>
  );
});
