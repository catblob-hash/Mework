import { LoaderCircle, RefreshCw, X } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { imageAttachmentData } from "../lib/runtime";
import type { ImageAttachment } from "../types";
import "./ImageStrip.css";

const imageDataCache = new Map<string, Promise<string>>();
const IMAGE_DATA_CACHE_LIMIT = 24;

function imageDataKey(image: ImageAttachment): string {
  return `${image.id}:${image.mime}`;
}

function imageData(image: ImageAttachment): Promise<string> {
  const key = imageDataKey(image);
  const cached = imageDataCache.get(key);
  if (cached) {
    imageDataCache.delete(key);
    imageDataCache.set(key, cached);
    return cached;
  }
  const request = imageAttachmentData(image.id).catch((error) => {
    imageDataCache.delete(key);
    throw error;
  });
  imageDataCache.set(key, request);
  while (imageDataCache.size > IMAGE_DATA_CACHE_LIMIT) {
    const oldest = imageDataCache.keys().next().value as string | undefined;
    if (!oldest) break;
    imageDataCache.delete(oldest);
  }
  return request;
}

function forgetImageData(image: ImageAttachment): void {
  imageDataCache.delete(imageDataKey(image));
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
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

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

  return createPortal(
    <div
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
          src={source}
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
    void imageData(image).then(
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

export function ImageStrip({
  images,
  compact = false,
  onRemove,
  className = ""
}: {
  images?: ImageAttachment[];
  compact?: boolean;
  onRemove?: (imageId: string) => void;
  className?: string;
}) {
  const { t } = useI18n();
  const [viewer, setViewer] = useState<ImageViewerState | null>(null);
  const closeViewer = useCallback(() => setViewer(null), []);
  if (!images?.length) return null;
  return (
    <>
      <ul
        className={`image-strip${compact ? " image-strip--compact" : ""}${className ? ` ${className}` : ""}`}
        aria-label={t("{count} 张图片", "{count} images", { count: images.length })}
      >
        {images.map((image, index) => (
          <ImageThumbnail
            key={`${image.id}:${index}`}
            image={image}
            compact={compact}
            onRemove={onRemove ? () => onRemove(image.id) : undefined}
            onOpen={(selected, source, trigger) => setViewer({ image: selected, source, trigger })}
          />
        ))}
      </ul>
      {viewer ? (
        <ImageViewer
          image={viewer.image}
          source={viewer.source}
          returnFocus={viewer.trigger}
          onClose={closeViewer}
        />
      ) : null}
    </>
  );
}
