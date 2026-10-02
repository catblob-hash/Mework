import { ImageOff, ImagePlus, LoaderCircle } from "lucide-react";
import type { JSX } from "react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import type { Background, BackgroundScheme, BuiltinPicture } from "../../lib/background";
import { BUILTIN_PICTURES, parseBackground, solidBackground, THEME_SOLID } from "../../lib/background";
import {
  backgroundImageData,
  deleteBackgroundImage,
  importBackgroundImage,
  listBackgroundImages,
  useBackgroundLibraryGeneration
} from "../../lib/backgroundImage";
import type { BackgroundImage } from "../../types";
import { ConfirmDeleteButton, Dialog } from "../Common";

type Translate = ReturnType<typeof useI18n>["t"];

/** The smallest tier of an imported picture, which is plenty for a thumbnail. */
const THUMBNAIL_WIDTH = 480;
const THUMBNAIL_HEIGHT = 270;

/**
 * An imported picture's thumbnail: its data URL, `null` while it loads, or `false`
 * when its files can no longer be read.
 */
function useImportedThumbnail(imageId: string): string | null | false {
  const libraryGeneration = useBackgroundLibraryGeneration();
  const [thumbnail, setThumbnail] = useState<{ id: string; dataUrl: string | false } | null>(null);
  useEffect(() => {
    if (!imageId) return;
    // Re-read after any import, which may have restored this very id's files.
    void libraryGeneration;
    let cancelled = false;
    backgroundImageData(imageId, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)
      .then((tier) => {
        if (!cancelled) setThumbnail({ id: imageId, dataUrl: tier.dataUrl });
      })
      .catch(() => {
        if (!cancelled) setThumbnail({ id: imageId, dataUrl: false });
      });
    return () => {
      cancelled = true;
    };
  }, [imageId, libraryGeneration]);
  return thumbnail?.id === imageId ? thumbnail.dataUrl : null;
}

/** The picture a background shows, for drawing it small; `null` for a solid ground. */
export function useBackgroundPicture(background: Background): string | null | false {
  const imported = useImportedThumbnail(background.kind === "imported" ? background.id : "");
  if (background.kind === "builtin") return background.picture.thumbnail;
  if (background.kind === "imported") return imported;
  return null;
}

function builtinName(picture: BuiltinPicture, t: Translate): string {
  switch (picture.name) {
    case "desk": return t("桌子下", "Under the desk");
    case "chair": return t("椅子上", "On the chair");
    case "shelf": return t("书架中", "On the bookshelf");
    case "curtain": return t("窗帘后", "Behind the curtain");
  }
}

function solidName(scheme: BackgroundScheme, t: Translate): string {
  return scheme === "day" ? t("浅色纯色", "Light solid") : t("深色纯色", "Dark solid");
}

/** A background in miniature: its picture, or the solid ground it stands for. */
function BackgroundSurface({
  background,
  picture
}: {
  background: Background;
  picture: string | null | false;
}): JSX.Element {
  if (background.kind === "solid") {
    return (
      <span
        className="background-surface background-swatch"
        data-solid={background.scheme ?? "theme"}
      />
    );
  }
  return (
    <span className="background-surface background-swatch" data-solid="theme">
      {picture && <img src={picture} alt="" draggable={false} decoding="async" />}
      {picture === null && (
        <span className="background-surface__badge"><LoaderCircle size={14} className="spin" /></span>
      )}
      {picture === false && (
        <span className="background-surface__badge"><ImageOff size={14} /></span>
      )}
    </span>
  );
}

/** The background the settings point at, beside the button that opens the library. */
export function BackgroundPreview({ value }: { value: string }): JSX.Element {
  const background = parseBackground(value);
  const picture = useBackgroundPicture(background);
  return (
    <span className="background-preview" aria-hidden="true">
      <BackgroundSurface background={background} picture={picture} />
    </span>
  );
}

function ImportedTile({
  image,
  selected,
  onChoose,
  onRemove
}: {
  image: BackgroundImage;
  selected: boolean;
  onChoose: () => void;
  onRemove: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const picture = useImportedThumbnail(image.id);
  const size = t("{width} × {height}", "{width} × {height}", { width: image.width, height: image.height });
  return (
    <div className="background-library__tile">
      <button
        type="button"
        className="background-library__choice"
        aria-pressed={selected}
        aria-label={t("导入的图片 {size}", "Imported picture {size}", { size })}
        onClick={onChoose}
      >
        <BackgroundSurface background={{ kind: "imported", id: image.id }} picture={picture} />
        <span className="background-library__name">{size}</span>
      </button>
      <ConfirmDeleteButton
        className="background-library__remove"
        label={t("移除这张图片", "Remove this picture")}
        confirmLabel={t("确认移除这张图片", "Confirm removing this picture")}
        size={12}
        onDelete={onRemove}
      />
    </div>
  );
}

/**
 * The library of backgrounds: each theme's solid ground, the pictures that ship with
 * the app, and the ones the user imported, which can be added and removed here.
 * Choosing one applies it at once; the window behind the dialog is the preview.
 */
export function BackgroundLibraryDialog({
  value,
  theme,
  onChange,
  onClose
}: {
  value: string;
  /** The scheme on screen, which decides what picking a solid ground saves. */
  theme: BackgroundScheme;
  onChange: (value: string) => void;
  onClose: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const libraryGeneration = useBackgroundLibraryGeneration();
  const [imported, setImported] = useState<BackgroundImage[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const valueRef = useRef(value);
  valueRef.current = value;
  const background = parseBackground(value);

  useEffect(() => {
    // Re-list after any import or removal, wherever it happened.
    void libraryGeneration;
    let cancelled = false;
    listBackgroundImages()
      .then((images) => {
        if (cancelled) return;
        setImported(images);
        setListError(null);
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        setImported([]);
        setListError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [libraryGeneration]);

  const importPicture = async (file: File): Promise<void> => {
    setImporting(true);
    setError(null);
    try {
      const image = await importBackgroundImage(file);
      onChange(image.id);
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(
        message === "unreadable"
          ? t(
            "无法读取这张图片，请换一张 PNG、JPEG、WebP 或 HEIC 图片。",
            "This picture can't be read. Try a PNG, JPEG, WebP or HEIC file."
          )
          : t("背景图片导入失败：{reason}", "Couldn't import the picture: {reason}", { reason: message })
      );
    } finally {
      setImporting(false);
    }
  };

  const removePicture = async (imageId: string): Promise<void> => {
    setError(null);
    try {
      await deleteBackgroundImage(imageId);
      // The window falls back to the theme's ground rather than to a picture that is gone.
      if (valueRef.current === imageId) onChange(THEME_SOLID);
    } catch (reason) {
      setError(t("无法移除这张图片：{reason}", "Couldn't remove the picture: {reason}", {
        reason: reason instanceof Error ? reason.message : String(reason)
      }));
    }
  };

  const solidSelected = (scheme: BackgroundScheme): boolean =>
    background.kind === "solid" && (background.scheme ?? theme) === scheme;

  return (
    <Dialog
      title={t("背景", "Background")}
      description={t(
        "纯色背景是各主题自己的底色，切换主题时会随之切换；图片不随主题变化。",
        "Each theme has a solid ground of its own, and a solid background switches along with the theme. A picture stays as it is."
      )}
      width="620px"
      onClose={onClose}
      footer={(
        <button type="button" className="button button--primary" onClick={onClose}>
          {t("完成", "Done")}
        </button>
      )}
    >
      <section className="background-library__section" aria-label={t("内置背景", "Built-in backgrounds")}>
        <h3 className="background-library__heading">{t("内置", "Built-in")}</h3>
        <div className="background-library__grid">
          {(["day", "night"] as const).map((scheme) => (
            <div key={scheme} className="background-library__tile">
              <button
                type="button"
                className="background-library__choice"
                aria-pressed={solidSelected(scheme)}
                onClick={() => onChange(solidBackground(scheme, theme))}
              >
                <BackgroundSurface background={{ kind: "solid", scheme: scheme === theme ? null : scheme }} picture={null} />
                <span className="background-library__name">{solidName(scheme, t)}</span>
              </button>
            </div>
          ))}
          {BUILTIN_PICTURES.map((picture) => (
            <div key={picture.id} className="background-library__tile">
              <button
                type="button"
                className="background-library__choice"
                aria-pressed={value === picture.id}
                onClick={() => onChange(picture.id)}
              >
                <BackgroundSurface background={{ kind: "builtin", picture }} picture={picture.thumbnail} />
                <span className="background-library__name">{builtinName(picture, t)}</span>
              </button>
            </div>
          ))}
        </div>
      </section>

      <section className="background-library__section" aria-label={t("我的图片", "My pictures")}>
        <h3 className="background-library__heading">{t("我的图片", "My pictures")}</h3>
        <div className="background-library__grid">
          <div className="background-library__tile">
            <button
              type="button"
              className="background-library__choice background-library__choice--add"
              aria-busy={importing || undefined}
              disabled={importing}
              onClick={() => inputRef.current?.click()}
            >
              <span className="background-surface background-library__add-surface">
                {importing ? <LoaderCircle size={18} className="spin" /> : <ImagePlus size={18} />}
              </span>
              <span className="background-library__name">
                {importing ? t("正在导入…", "Importing…") : t("添加图片…", "Add picture…")}
              </span>
            </button>
          </div>
          {imported?.map((image) => (
            <ImportedTile
              key={image.id}
              image={image}
              selected={value === image.id}
              onChoose={() => onChange(image.id)}
              onRemove={() => void removePicture(image.id)}
            />
          ))}
        </div>
        <p className="background-library__hint">
          {t(
            "按窗口比例裁切，不拉伸。导入时按多级分辨率保存，窗口按屏幕像素取刚好够用的一级。",
            "Cropped to the window's shape, never stretched. Importing saves several resolutions, and the window uses the smallest one that covers its pixels."
          )}
        </p>
        {listError && (
          <p className="background-library__error" role="alert">
            {t("无法读取导入的图片：{reason}", "Couldn't read the imported pictures: {reason}", { reason: listError })}
          </p>
        )}
        {error && <p className="background-library__error" role="alert">{error}</p>}
        <input
          ref={inputRef}
          type="file"
          accept="image/*"
          hidden
          tabIndex={-1}
          aria-hidden="true"
          onChange={(event) => {
            const file = event.currentTarget.files?.[0];
            event.currentTarget.value = "";
            if (file) void importPicture(file);
          }}
        />
      </section>
    </Dialog>
  );
}
