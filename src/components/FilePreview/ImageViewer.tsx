import { Maximize, Minus, Plus, Scan } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { IconButton } from "../Common";
import { formatBytes } from "./format";
import { UnsupportedNotice } from "./UnsupportedNotice";

/** The zoom steps the buttons walk through, as fractions of the picture's own size. */
const ZOOM_STEPS = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4, 6, 8, 12, 16];

function nextStep(current: number, direction: 1 | -1): number {
  if (direction > 0) return ZOOM_STEPS.find((step) => step > current + 1e-6) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1];
  return [...ZOOM_STEPS].reverse().find((step) => step < current - 1e-6) ?? ZOOM_STEPS[0];
}

/**
 * One picture, shown whole by default and at any size on request.
 *
 * `fit` is the reading most pictures want — the whole thing, never enlarged past
 * its own pixels — and the zoom buttons trade it for an explicit scale the pane
 * scrolls around. The checkerboard is what makes transparency visible, which is
 * most of what a reader of an icon or a screenshot with an alpha channel wants to
 * know.
 */
export function ImageViewer({
  source,
  name,
  bytes,
  onReveal
}: {
  /** A `data:` URL of the picture. */
  source: string;
  name: string;
  /** The file's size, when known. */
  bytes: number | null;
  /** Shows the file in the system file manager, for a format this engine cannot decode. */
  onReveal?: () => void;
}) {
  const { t } = useI18n();
  const [zoom, setZoom] = useState<number | "fit">("fit");
  // Keyed by the picture they describe, so a reload with new bytes starts clean
  // without an effect racing the load event of a `data:` URL that decodes at once.
  const [measured, setMeasured] = useState<{ source: string; width: number; height: number } | null>(null);
  const [failedSource, setFailedSource] = useState<string | null>(null);
  const natural = measured?.source === source ? measured : null;
  const failed = failedSource === source;
  const stageRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement>(null);

  const measure = useCallback((image: HTMLImageElement | null) => {
    if (!image?.complete || !image.naturalWidth) return;
    setMeasured({ source, width: image.naturalWidth, height: image.naturalHeight });
  }, [source]);

  // A picture already decoded by the time it was attached fires no load event to listen for.
  useEffect(() => measure(imageRef.current), [measure]);

  /** The scale `fit` currently draws at, so a zoom step starts from what is on screen. */
  const effectiveZoom = useCallback((): number => {
    if (zoom !== "fit") return zoom;
    const image = imageRef.current;
    if (!image || !natural?.width) return 1;
    return image.getBoundingClientRect().width / natural.width;
  }, [natural, zoom]);

  const step = useCallback((direction: 1 | -1) => {
    setZoom(nextStep(effectiveZoom(), direction));
  }, [effectiveZoom]);

  // A pinch arrives as a wheel with the control key held; a plain wheel scrolls.
  // Registered by hand because React's wheel listener is passive, and a passive
  // listener cannot stop the page from zooming instead of the picture.
  const effectiveZoomRef = useRef(effectiveZoom);
  effectiveZoomRef.current = effectiveZoom;
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const onWheel = (event: WheelEvent) => {
      if (!event.ctrlKey && !event.metaKey) return;
      event.preventDefault();
      const current = effectiveZoomRef.current();
      setZoom(Math.min(16, Math.max(0.05, current * Math.exp(-event.deltaY / 300))));
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => stage.removeEventListener("wheel", onWheel);
  }, [failed]);

  if (failed) {
    return (
      <UnsupportedNotice
        message={t("当前界面引擎无法解码这种图片格式。", "This window's engine cannot decode this image format.")}
        onReveal={onReveal}
      />
    );
  }

  const percent = zoom === "fit" ? null : Math.round(zoom * 100);
  const sized = zoom !== "fit" && natural
    ? { width: natural.width * zoom, height: natural.height * zoom, maxWidth: "none", maxHeight: "none" }
    : undefined;

  return (
    <div className="file-preview file-preview--image">
      <div
        ref={stageRef}
        className={`file-preview__stage${zoom === "fit" ? " file-preview__stage--fit" : ""}`}
      >
        <div className="file-preview__checker">
          {/* Shown through `<img>` rather than inline, so an SVG from the
              workspace cannot run scripts or reach anything of its own. */}
          <img
            ref={imageRef}
            src={source}
            alt={name}
            draggable={false}
            style={sized}
            onLoad={(event) => measure(event.currentTarget)}
            onError={() => setFailedSource(source)}
          />
        </div>
      </div>
      <div className="file-preview__toolbar" role="toolbar" aria-label={t("图片缩放", "Image zoom")}>
        <IconButton className="file-preview__tool" label={t("缩小", "Zoom out")} onClick={() => step(-1)}>
          <Minus size={13} aria-hidden="true" />
        </IconButton>
        <button
          type="button"
          className="file-preview__zoom-label"
          title={t("适应窗口", "Fit to pane")}
          onClick={() => setZoom("fit")}
        >
          {percent === null ? t("适应", "Fit") : `${percent}%`}
        </button>
        <IconButton className="file-preview__tool" label={t("放大", "Zoom in")} onClick={() => step(1)}>
          <Plus size={13} aria-hidden="true" />
        </IconButton>
        <IconButton
          className="file-preview__tool"
          label={t("实际大小", "Actual size")}
          aria-pressed={zoom === 1}
          onClick={() => setZoom(1)}
        >
          <Scan size={13} aria-hidden="true" />
        </IconButton>
        <IconButton
          className="file-preview__tool"
          label={t("适应窗口", "Fit to pane")}
          aria-pressed={zoom === "fit"}
          onClick={() => setZoom("fit")}
        >
          <Maximize size={13} aria-hidden="true" />
        </IconButton>
        <span className="file-preview__meta">
          {natural ? `${natural.width} × ${natural.height}` : ""}
          {natural && bytes !== null ? " · " : ""}
          {bytes !== null ? formatBytes(bytes) : ""}
        </span>
      </div>
    </div>
  );
}
