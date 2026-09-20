import { Bot, BrainCircuit, Check, Shield, UserRound, X } from "lucide-react";
import { useState } from "react";
import { useI18n } from "../i18n";
import {
  textWithoutAppendedImagePlaceholders,
  withImagePlaceholders
} from "../lib/imageShortIds";
import type { ImageAttachment, InsertableContextKind } from "../types";
import { IconButton, PlainField } from "./Common";
import { ImageStrip } from "./ImageStrip";

type TextKind = Exclude<InsertableContextKind, "tool">;

const textMeta: Record<TextKind, {
  title: (t: ReturnType<typeof useI18n>["t"]) => string;
  placeholder: (t: ReturnType<typeof useI18n>["t"]) => string;
  icon: typeof Shield;
}> = {
  system: {
    title: (t) => t("系统提示词上下文", "System prompt context"),
    placeholder: (t) => t("输入要在这个位置生效的系统指令…", "Enter a system instruction to apply at this point…"),
    icon: Shield
  },
  user: {
    title: (t) => t("用户输入", "User input"),
    placeholder: (t) => t("输入用户消息…", "Enter a user message…"),
    icon: UserRound
  },
  reasoning: {
    title: (t) => t("明文思考字段", "Plain-text reasoning field"),
    placeholder: (t) => t("输入推理过程或计划…", "Enter reasoning or a plan…"),
    icon: BrainCircuit
  },
  assistant: {
    title: (t) => t("模型回复", "Model reply"),
    placeholder: (t) => t("输入模型回复…", "Enter a model reply…"),
    icon: Bot
  }
};

export interface InlineTextEditorProps {
  kind: TextKind;
  /** Shown only when inserting; an edited card already names itself. */
  showKind?: boolean;
  content?: string;
  images?: ImageAttachment[];
  /**
   * Takes images pasted into the box, alongside what this message already
   * carries, and returns the ones that were accepted, numbered. Absent means
   * this message's model has no image input, and the paste falls through to the
   * browser as ordinary text.
   */
  onPasteImages?: (
    files: File[],
    existing: readonly ImageAttachment[]
  ) => Promise<ImageAttachment[]>;
  onCancel: () => void;
  onSave: (content: string, images?: ImageAttachment[]) => void;
}

/** Replaces a text card's body while it is being edited, and stands in for a card while one is inserted. */
export function InlineTextEditor({
  kind,
  showKind = false,
  content: initialContent = "",
  images: initialImages,
  onPasteImages,
  onCancel,
  onSave
}: InlineTextEditorProps) {
  const { t } = useI18n();
  const [images, setImages] = useState<ImageAttachment[]>(initialImages ?? []);
  // `[Image #N]` is the model's way of pointing at a thumbnail this box already
  // shows, so the box never shows the token itself — it is put back on save.
  const [content, setContent] = useState(() => (
    kind === "user"
      ? textWithoutAppendedImagePlaceholders(initialContent, initialImages)
      : initialContent
  ));
  const meta = textMeta[kind];
  const Icon = meta.icon;
  const keepsImages = kind === "user" && images.length > 0;
  const savable = Boolean(content.trim()) || keepsImages;

  const save = () => {
    if (!savable) return;
    const text = content.trim();
    if (kind !== "user") {
      onSave(text);
      return;
    }
    onSave(withImagePlaceholders(text, images), images);
  };

  return (
    <div className={`inline-text-editor inline-text-editor--${kind}`}>
      {showKind && (
        <div className={`editor-kind editor-kind--${kind}`}>
          <Icon size={16} />
          <span>{meta.title(t)}</span>
        </div>
      )}
      {kind === "user" && images.length > 0 && (
        <ImageStrip
          images={images}
          compact
          className="context-editor__images"
          onRemove={(imageId) => setImages((current) => current.filter((image) => image.id !== imageId))}
        />
      )}
      <PlainField
        className="context-text-editor"
        value={content}
        autoFocus
        label={meta.title(t)}
        placeholder={meta.placeholder(t)}
        onChange={setContent}
        onPaste={onPasteImages ? (event) => {
          const files = Array.from(event.clipboardData.files);
          if (!files.length) return;
          // A clipboard carrying both keeps its text; the image rides along.
          if (!event.clipboardData.getData("text/plain")) event.preventDefault();
          void onPasteImages(files, images).then((accepted) => {
            if (!accepted.length) return;
            setImages((current) => {
              const known = new Set(current.map((image) => image.id));
              return [...current, ...accepted.filter((image) => !known.has(image.id))];
            });
          });
        } : undefined}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === "Enter") save();
          if (event.key === "Escape") onCancel();
        }}
      />
      <div className="inline-text-editor__footer">
        <IconButton label={t("取消", "Cancel")} onClick={onCancel}><X size={14} /></IconButton>
        <IconButton label={t("保存", "Save")} disabled={!savable} onClick={save}><Check size={14} /></IconButton>
      </div>
    </div>
  );
}
