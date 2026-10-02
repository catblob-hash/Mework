import { Bot, BrainCircuit, Check, Shield, UserRound, X } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { useAttachmentDropZone } from "../lib/attachmentDrop";
import {
  mergeFileAttachments,
  type AddMessageAttachments,
  type AttachmentRejection
} from "../lib/fileAttachments";
import { expandPastedTexts, type PastedText } from "../lib/pastedText";
import {
  textWithoutAppendedImagePlaceholders,
  withImagePlaceholders
} from "../lib/imageShortIds";
import type { FileAttachment, ImageAttachment, InsertableContextKind } from "../types";
import { AttachmentDropOverlay, AttachmentNotice } from "./AttachmentFeedback";
import { ComposerAddFiles } from "./ComposerAddMenu";
import { IconButton, PlainField } from "./Common";
import { ImageStrip } from "./ImageStrip";
import { usePastedTextTags } from "./PastedTextTags";

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
  files?: FileAttachment[];
  /**
   * Attaches picked, pasted or dropped files to this user message, alongside
   * what it already carries. Absent where nothing can be attached (a template
   * that is read-only); what the message already has can still be removed.
   */
  onAddAttachments?: AddMessageAttachments;
  /** Whether this message's model can see images; decides how a drag of pictures reads. */
  imageInput?: boolean;
  onCancel: () => void;
  onSave: (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => void;
}

/** Replaces a text card's body while it is being edited, and stands in for a card while one is inserted. */
export function InlineTextEditor({
  kind,
  showKind = false,
  content: initialContent = "",
  images: initialImages,
  files: initialFiles,
  onAddAttachments,
  imageInput = false,
  onCancel,
  onSave
}: InlineTextEditorProps) {
  const { t } = useI18n();
  const [images, setImages] = useState<ImageAttachment[]>(initialImages ?? []);
  const [files, setFiles] = useState<FileAttachment[]>(initialFiles ?? []);
  const [pending, setPending] = useState(0);
  const [rejected, setRejected] = useState<readonly AttachmentRejection[]>([]);
  // `[Image #N]` is the model's way of pointing at a thumbnail this box already
  // shows, so the box never shows the token itself — it is put back on save.
  const [content, setContent] = useState(() => (
    kind === "user"
      ? textWithoutAppendedImagePlaceholders(initialContent, initialImages)
      : initialContent
  ));
  // A long paste folds into a tag here as it does in the composer, and saves as its text.
  const [pastes, setPastes] = useState<PastedText[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const updatePastes = useCallback(
    (update: (current: readonly PastedText[]) => PastedText[]) => setPastes((current) => update(current)),
    []
  );
  const pasteTags = usePastedTextTags({ textareaRef, value: content, pastes, onPastesChange: updatePastes });
  // The latest attachments, for a batch that lands after others were added or removed.
  const attachmentsRef = useRef({ images, files });
  attachmentsRef.current = { images, files };
  const meta = textMeta[kind];
  const Icon = meta.icon;
  const acceptsAttachments = kind === "user" && Boolean(onAddAttachments);
  const keepsAttachments = kind === "user" && (images.length > 0 || files.length > 0);
  const savable = pending === 0 && (Boolean(content.trim()) || keepsAttachments);

  const attach = (incoming: File[], preRejected: readonly AttachmentRejection[] = []) => {
    if (!onAddAttachments || (!incoming.length && !preRejected.length)) return;
    setPending((count) => count + 1);
    void onAddAttachments(incoming, attachmentsRef.current, preRejected).then(
      (result) => {
        setImages((current) => {
          const known = new Set(current.map((image) => image.id));
          return [...current, ...result.images.filter((image) => !known.has(image.id))];
        });
        setFiles((current) => mergeFileAttachments(current, result.files));
        setRejected(result.rejected);
      },
      () => setRejected(incoming.map((file) => ({ name: file.name || undefined, reason: "failed" as const })))
    ).finally(() => setPending((count) => count - 1));
  };

  const drop = useAttachmentDropZone({
    imageInput,
    disabled: !acceptsAttachments,
    onDrop: (dropped, preRejected) => attach(dropped, preRejected)
  });

  const save = () => {
    if (!savable) return;
    const text = expandPastedTexts(content, pastes).trim();
    if (kind !== "user") {
      onSave(text);
      return;
    }
    onSave(withImagePlaceholders(text, images), images, files);
  };

  return (
    <div
      ref={drop.ref}
      className={`inline-text-editor inline-text-editor--${kind}`}
      data-attachment-drop-ready={drop.dragging && !drop.over ? "true" : undefined}
    >
      {showKind && (
        <div className={`editor-kind editor-kind--${kind}`}>
          <Icon size={16} />
          <span>{meta.title(t)}</span>
        </div>
      )}
      {kind === "user" && (
        <AttachmentNotice rejected={rejected} onDismiss={() => setRejected([])} />
      )}
      {kind === "user" && (images.length > 0 || files.length > 0 || pending > 0) && (
        <ImageStrip
          images={images}
          files={files}
          compact
          busy={pending > 0}
          className="context-editor__images"
          onRemove={(imageId) => setImages((current) => current.filter((image) => image.id !== imageId))}
          onRemoveFile={(fileId) => setFiles((current) => current.filter((file) => file.id !== fileId))}
        />
      )}
      <PlainField
        className="context-text-editor"
        value={content}
        autoFocus
        label={meta.title(t)}
        placeholder={meta.placeholder(t)}
        onChange={setContent}
        textareaRef={textareaRef}
        pasteLayer={kind === "user" ? pasteTags.layer : undefined}
        onPaste={kind === "user" ? (event) => {
          const pasted = Array.from(event.clipboardData.files);
          const text = event.clipboardData.getData("text/plain");
          if (pasted.length && acceptsAttachments) {
            // A clipboard carrying both keeps its text; the file rides along.
            if (!text) event.preventDefault();
            attach(pasted);
            return;
          }
          pasteTags.onPaste(event);
        } : undefined}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === "Enter") save();
          if (event.key === "Escape") onCancel();
        }}
      />
      <div className="inline-text-editor__footer">
        {acceptsAttachments && (
          <div className="inline-text-editor__attach">
            <ComposerAddFiles
              imageInput={imageInput}
              onChooseFiles={(chosen) => attach(chosen)}
            />
          </div>
        )}
        <IconButton label={t("取消", "Cancel")} onClick={onCancel}><X size={14} /></IconButton>
        <IconButton label={t("保存", "Save")} disabled={!savable} onClick={save}><Check size={14} /></IconButton>
      </div>
      {kind === "user" && <AttachmentDropOverlay state={drop} imageInput={imageInput} />}
    </div>
  );
}
