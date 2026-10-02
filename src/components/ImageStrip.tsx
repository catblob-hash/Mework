import {
  Database,
  File as FileGlyph,
  FileCode2,
  FileText,
  LoaderCircle,
  RefreshCw,
  Sheet,
  X
} from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { fileExtension, fileIconKind } from "../lib/fileIcons";
import { useFloatingSurface } from "../lib/floatingSurfaces";
import { memoryPool, pooledFetch } from "../lib/memoryPool";
import { imageAttachmentData, imageAttachmentThumbnail } from "../lib/runtime";
import type { FileAttachment, ImageAttachment } from "../types";
import { FileAttachmentPreview } from "./FileAttachmentPreview";
import { formatBytes } from "./FilePreview/format";
import "./ImageStrip.css";

function imageDataKey(image: ImageAttachment): string {
  return `${image.id}:${image.mime}`;
}

/** The chip's picture: the host's thumbnail, high-priority data in the shared pool. */
function thumbnailData(image: ImageAttachment): Promise<string> {
  return pooledFetch(memoryPool, "imageThumbnail", imageDataKey(image), () => imageAttachmentThumbnail(image.id));
}

/** The viewer's picture: the full image, low-priority data in the shared pool. */
function fullImageData(image: ImageAttachment): Promise<string> {
  return pooledFetch(memoryPool, "imageData", imageDataKey(image), () => imageAttachmentData(image.id));
}

function forgetImageData(image: ImageAttachment): void {
  memoryPool.delete("imageThumbnail", imageDataKey(image));
  memoryPool.delete("imageData", imageDataKey(image));
}

interface ImageViewerState {
  image: ImageAttachment;
  source: string;
  trigger: HTMLButtonElement;
}

function ImageViewer({
  image,
  source,
  returnFocus,
  onClose
}: {
  image: ImageAttachment;
  source: string;
  returnFocus: HTMLButtonElement;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const overlayRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  // The viewer dims the whole window, so it claims the whole window: the built-in browser's native
  // page paints above HTML and would otherwise sit on top of the dimmed backdrop.
  useFloatingSurface(overlayRef, true);

  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      const dialogs = document.querySelectorAll<HTMLElement>('[role="dialog"]');
      if (dialogs.item(dialogs.length - 1) !== dialogRef.current) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key === "Tab") {
        event.preventDefault();
        closeRef.current?.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      if (returnFocus.isConnected) returnFocus.focus();
      else previous?.focus();
    };
  }, [onClose, returnFocus]);

  const dimensions = image.width && image.height ? `${image.width} × ${image.height}` : "";
  // The chip's thumbnail stands in until the full picture arrives.
  const [full, setFull] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    setFull(null);
    fullImageData(image).then(
      (value) => {
        if (active) setFull(value);
      },
      () => undefined
    );
    return () => {
      active = false;
    };
  }, [image]);

  return createPortal(
    <div
      ref={overlayRef}
      className="image-viewer"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="image-viewer__dialog"
        role="dialog"
        aria-modal="true"
        aria-label={t("查看原图 {name}", "View full image {name}", { name: image.name })}
        tabIndex={-1}
      >
        <img
          className="image-viewer__image"
          src={full ?? source}
          alt={t("{name} 原图", "Full image {name}", { name: image.name })}
        />
        <div className="image-viewer__caption">
          <span>{image.name}</span>
          {dimensions ? <span aria-hidden="true">{dimensions}</span> : null}
        </div>
        <button
          ref={closeRef}
          type="button"
          className="image-viewer__close"
          aria-label={t("关闭原图 {name}", "Close full image {name}", { name: image.name })}
          onClick={onClose}
        >
          <X aria-hidden="true" />
        </button>
      </div>
    </div>,
    document.body
  );
}

function ImageThumbnail({
  image,
  compact,
  onRemove,
  onOpen
}: {
  image: ImageAttachment;
  compact: boolean;
  onRemove?: () => void;
  onOpen: (image: ImageAttachment, source: string, trigger: HTMLButtonElement) => void;
}) {
  const { t } = useI18n();
  const statusId = useId();
  const [source, setSource] = useState("");
  const [failed, setFailed] = useState(false);
  const [loadVersion, setLoadVersion] = useState(0);
  const [announcement, setAnnouncement] = useState<"failed" | "recovered" | null>(null);
  const recoveryPendingRef = useRef(false);

  useEffect(() => {
    let active = true;
    setSource("");
    setFailed(false);
    setAnnouncement(null);
    void thumbnailData(image).then(
      (value) => {
        if (!active) return;
        setSource(value);
      },
      () => {
        if (!active) return;
        recoveryPendingRef.current = false;
        setFailed(true);
        setAnnouncement("failed");
      }
    );
    return () => {
      active = false;
    };
  }, [image.id, image.mime, loadVersion]);

  const dimensions = image.width && image.height ? `${image.width} × ${image.height}` : "";
  const title = [
    image.shortId !== undefined ? `[Image #${image.shortId}]` : "",
    image.name,
    dimensions
  ].filter(Boolean).join(" · ");
  const status = source
    ? t("{name} 已加载", "{name} loaded", { name: image.name })
    : failed
      ? t("{name} 加载失败", "{name} failed to load", { name: image.name })
      : t("{name} 正在加载", "{name} is loading", { name: image.name });
  const actionLabel = source
    ? t("查看原图 {name}", "View full image {name}", { name: image.name })
    : failed
      ? t("重试加载图片 {name}", "Retry loading image {name}", { name: image.name })
      : t("图片 {name} 正在加载", "Image {name} is loading", { name: image.name });

  const retry = () => {
    forgetImageData(image);
    recoveryPendingRef.current = true;
    setSource("");
    setFailed(false);
    setAnnouncement(null);
    setLoadVersion((version) => version + 1);
  };

  return (
    <li className={`image-strip__item${compact ? " image-strip__item--compact" : ""}`} title={title}>
      <button
        type="button"
        className="image-strip__open"
        aria-label={actionLabel}
        aria-describedby={statusId}
        disabled={!source && !failed}
        onClick={(event) => {
          if (source) onOpen(image, source, event.currentTarget);
          else if (failed) retry();
        }}
      >
        {source ? (
          // No loading="lazy": the bytes are already in memory as a data URL, and
          // embedded webviews can report a zero-size viewport, which would defer
          // a lazy image forever.
          <img
            className="image-strip__image"
            src={source}
            alt={image.name}
            onLoad={() => {
              if (!recoveryPendingRef.current) return;
              recoveryPendingRef.current = false;
              setAnnouncement("recovered");
            }}
            onError={() => {
              forgetImageData(image);
              recoveryPendingRef.current = false;
              setSource("");
              setFailed(true);
              setAnnouncement("failed");
            }}
          />
        ) : (
          <span className={`image-strip__placeholder${failed ? " image-strip__placeholder--failed" : ""}`}>
            {failed ? (
              <>
                <RefreshCw aria-hidden="true" />
                <span className="image-strip__retry-label">{t("重试", "Retry")}</span>
              </>
            ) : (
              <LoaderCircle className="spin" aria-hidden="true" />
            )}
          </span>
        )}
      </button>
      <span id={statusId} className="sr-only">
        {status}
      </span>
      {announcement ? (
        <span className="sr-only" role="status" aria-live="polite" aria-atomic="true">
          {announcement === "failed"
            ? t("{name} 加载失败", "{name} failed to load", { name: image.name })
            : t("{name} 已重新加载", "{name} reloaded", { name: image.name })}
        </span>
      ) : null}
      {image.shortId !== undefined ? (
        <span className="image-strip__short-id">
          <span aria-hidden="true">#{image.shortId}</span>
          <span className="sr-only">
            {t("对话编号 [Image #{id}]", "Conversation number [Image #{id}]", { id: image.shortId })}
          </span>
        </span>
      ) : null}
      {onRemove ? (
        <button
          type="button"
          className="image-strip__remove"
          aria-label={t("移除图片 {name}", "Remove image {name}", { name: image.name })}
          onClick={onRemove}
        >
          <X aria-hidden="true" />
        </button>
      ) : null}
    </li>
  );
}

function FileKindGlyph({ file }: { file: FileAttachment }) {
  if (file.format === "pdf") return <FileText aria-hidden="true" />;
  switch (fileIconKind(file.name)) {
    case "code":
      return <FileCode2 aria-hidden="true" />;
    case "data":
      return <Database aria-hidden="true" />;
    case "sheet":
      return <Sheet aria-hidden="true" />;
    case "doc":
    case "skill":
      return <FileText aria-hidden="true" />;
    default:
      return <FileGlyph aria-hidden="true" />;
  }
}

/** The short type label a file tile carries, the way an image carries its number. */
function fileTypeLabel(file: FileAttachment): string {
  if (file.format === "pdf") return "PDF";
  const extension = fileExtension(file.name);
  return extension ? extension.slice(0, 6).toUpperCase() : "TXT";
}

/**
 * A file on a message, in the same strip as its pictures: a tile rather than a
 * thumbnail, since a file's first look is its name and type, not its pixels.
 */
function FileTile({
  file,
  compact,
  onRemove,
  onOpen
}: {
  file: FileAttachment;
  compact: boolean;
  onRemove?: () => void;
  onOpen: (file: FileAttachment) => void;
}) {
  const { t } = useI18n();
  const size = file.format === "pdf" && file.pages
    ? t("{pages} 页 · {size}", "{pages} pages · {size}", { pages: file.pages, size: formatBytes(file.bytes) })
    : formatBytes(file.bytes);
  return (
    <li
      className={`image-strip__item image-strip__item--file${compact ? " image-strip__item--compact" : ""}`}
      title={`${file.name} · ${size}`}
    >
      <button
        type="button"
        className="image-strip__open image-strip__file"
        aria-label={t("预览文件 {name}", "Preview file {name}", { name: file.name })}
        onClick={() => onOpen(file)}
      >
        <span className="image-strip__file-icon">
          <FileKindGlyph file={file} />
        </span>
        <span className="image-strip__file-text">
          <span className="image-strip__file-name">{file.name}</span>
          <span className="image-strip__file-meta">
            <span className="image-strip__file-type">{fileTypeLabel(file)}</span>
            <span>{size}</span>
          </span>
        </span>
      </button>
      {onRemove ? (
        <button
          type="button"
          className="image-strip__remove"
          aria-label={t("移除文件 {name}", "Remove file {name}", { name: file.name })}
          onClick={onRemove}
        >
          <X aria-hidden="true" />
        </button>
      ) : null}
    </li>
  );
}

/**
 * Everything attached to a message: its images, then its files.
 *
 * `busy` adds a placeholder for attachments still being prepared — a PDF being
 * read, an upload in flight — so the strip shows work in progress where the
 * result will land.
 */
export function ImageStrip({
  images,
  files,
  compact = false,
  busy = false,
  onRemove,
  onRemoveFile,
  className = ""
}: {
  images?: readonly ImageAttachment[];
  files?: readonly FileAttachment[];
  compact?: boolean;
  busy?: boolean;
  onRemove?: (imageId: string) => void;
  onRemoveFile?: (fileId: string) => void;
  className?: string;
}) {
  const { t } = useI18n();
  const [viewer, setViewer] = useState<ImageViewerState | null>(null);
  const [openFile, setOpenFile] = useState<FileAttachment | null>(null);
  const closeViewer = useCallback(() => setViewer(null), []);
  const closeFile = useCallback(() => setOpenFile(null), []);
  const imageCount = images?.length ?? 0;
  const fileCount = files?.length ?? 0;
  if (!imageCount && !fileCount && !busy) return null;
  return (
    <>
      <ul
        className={`image-strip${compact ? " image-strip--compact" : ""}${className ? ` ${className}` : ""}`}
        aria-label={fileCount
          ? t("{count} 个附件", "{count} attachments", { count: imageCount + fileCount })
          : t("{count} 张图片", "{count} images", { count: imageCount })}
        aria-busy={busy || undefined}
      >
        {images?.map((image, index) => (
          <ImageThumbnail
            key={`${image.id}:${index}`}
            image={image}
            compact={compact}
            onRemove={onRemove ? () => onRemove(image.id) : undefined}
            onOpen={(selected, source, trigger) => setViewer({ image: selected, source, trigger })}
          />
        ))}
        {files?.map((file) => (
          <FileTile
            key={file.id}
            file={file}
            compact={compact}
            onRemove={onRemoveFile ? () => onRemoveFile(file.id) : undefined}
            onOpen={setOpenFile}
          />
        ))}
        {busy ? (
          <li className={`image-strip__item image-strip__item--pending${compact ? " image-strip__item--compact" : ""}`}>
            <span className="image-strip__placeholder">
              <LoaderCircle className="spin" aria-hidden="true" />
            </span>
            <span className="sr-only" role="status">{t("正在添加附件…", "Adding attachments…")}</span>
          </li>
        ) : null}
      </ul>
      {viewer ? (
        <ImageViewer
          image={viewer.image}
          source={viewer.source}
          returnFocus={viewer.trigger}
          onClose={closeViewer}
        />
      ) : null}
      {openFile ? <FileAttachmentPreview file={openFile} onClose={closeFile} /> : null}
    </>
  );
}
