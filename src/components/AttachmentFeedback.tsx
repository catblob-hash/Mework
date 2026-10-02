import { Ban, FilePlus2, LoaderCircle, TriangleAlert, X } from "lucide-react";
import { useI18n } from "../i18n";
import type { DropZoneState } from "../lib/attachmentDrop";
import {
  summarizeDrag,
  type AttachmentRejection,
  type AttachmentRejectionReason,
  type DragSummary
} from "../lib/fileAttachments";
import {
  MAX_FILE_ATTACHMENT_PDF_BYTES,
  MAX_MESSAGE_ATTACHMENT_BYTES,
  MAX_TEXT_FILE_TOKENS,
  MAX_TEXT_FILE_UPLOAD_BYTES,
  TRUNCATED_TEXT_FILE_LINES
} from "../lib/fileBudget";
import { MAX_IMAGE_ATTACHMENT_BYTES } from "../lib/imageBudget";
import { IconButton } from "./Common";
import "./AttachmentFeedback.css";

type Translate = ReturnType<typeof useI18n>["t"];

function dragRejectionLabel(t: Translate, verdict: DragSummary["rejected"][number]["verdict"], count: number): string {
  switch (verdict) {
    case "directory":
      return t("{count} 个文件夹", "{count} folder(s)", { count });
    case "unsupported":
      return t("{count} 个不支持的文件", "{count} unsupported file(s)", { count });
    case "tooLarge":
      return t("{count} 个过大的文件", "{count} file(s) too large", { count });
    case "empty":
      return t("{count} 个空文件", "{count} empty file(s)", { count });
    case "imageInputUnavailable":
      return t("{count} 张图片（当前模型不支持图片）", "{count} image(s) (the model takes none)", { count });
  }
}

/**
 * What a drop here would do, shown over the zone while files hover it.
 *
 * A drag of nothing this message can carry — a folder, an archive, an image for
 * a model without eyes — reads as unavailable before it is dropped, so the
 * gesture is never completed only to be refused.
 */
export function AttachmentDropOverlay({
  state,
  imageInput
}: {
  state: DropZoneState;
  imageInput: boolean;
}) {
  const { t } = useI18n();
  if (!state.over) return null;
  if (state.items === null) {
    return (
      <div className="attachment-drop" role="status">
        <LoaderCircle className="spin" size={18} aria-hidden="true" />
        <strong>{t("正在检查文件…", "Checking files…")}</strong>
      </div>
    );
  }
  const summary = summarizeDrag(state.items, imageInput);
  const takeable = summary.accepted + summary.undecided;
  const ignored = summary.rejected.map((entry) => dragRejectionLabel(t, entry.verdict, entry.count)).join(t("、", ", "));
  if (!takeable) {
    const onlyFolders = summary.rejected.length === 1 && summary.rejected[0].verdict === "directory";
    return (
      <div className="attachment-drop attachment-drop--unavailable" role="status">
        <Ban size={18} aria-hidden="true" />
        <strong>{onlyFolders ? t("不能添加文件夹", "Folders can't be attached") : t("无法添加这些内容", "These can't be attached")}</strong>
        <span>
          {onlyFolders
            ? t("请拖入文件夹里的文件", "Drag the files inside it instead")
            : `${ignored}${t("。可添加图片、PDF 和文本文件", ". Images, PDFs and text files can be attached")}`}
        </span>
      </div>
    );
  }
  return (
    <div className="attachment-drop" role="status">
      <FilePlus2 size={18} aria-hidden="true" />
      <strong>{t("松开以添加 {count} 个文件", "Drop to attach {count} file(s)", { count: takeable })}</strong>
      {ignored ? <span>{t("将忽略 {items}", "Will skip {items}", { items: ignored })}</span> : null}
    </div>
  );
}

const KIB = 1024;
const MIB = 1024 * 1024;

function rejectionReasonLabel(t: Translate, reason: AttachmentRejectionReason): string {
  switch (reason) {
    case "directory":
      return t("不能添加文件夹", "Folders can't be attached");
    case "empty":
      return t("文件是空的", "The file is empty");
    case "unsupported":
      return t("不支持的格式；可添加图片、PDF 和文本文件", "Unsupported format; images, PDFs and text files can be attached");
    case "tooLarge":
      return t(
        "文件过大（文本不超过 {text} KB，PDF 不超过 {pdf} MB，图片不超过 {image} MB）",
        "Too large (text up to {text} KB, PDFs up to {pdf} MB, images up to {image} MB)",
        {
          text: MAX_TEXT_FILE_UPLOAD_BYTES / KIB,
          pdf: MAX_FILE_ATTACHMENT_PDF_BYTES / MIB,
          image: MAX_IMAGE_ATTACHMENT_BYTES / MIB
        }
      );
    case "tooLong":
      return t(
        "内容太长：前 {lines} 行仍超过 {tokens} tokens",
        "Too long: even its first {lines} lines are over {tokens} tokens",
        { lines: TRUNCATED_TEXT_FILE_LINES, tokens: MAX_TEXT_FILE_TOKENS }
      );
    case "imageInputUnavailable":
      return t("当前模型不支持图片输入", "The current model has no image input");
    case "imageRejected":
      return t("图片的格式或尺寸超出限制", "The image format or size is over the limit");
    case "pdfWithoutText":
      return t("PDF 里没有可读取的文字（可能是扫描件）", "The PDF has no readable text (it may be a scan)");
    case "pdfPassword":
      return t("PDF 有密码保护", "The PDF is password-protected");
    case "pdfUnreadable":
      return t("无法读取这个 PDF", "The PDF could not be read");
    case "messageTooLarge":
      return t(
        "一条消息的附件合计不能超过 {size} MB",
        "A message's attachments can come to at most {size} MB in all",
        { size: MAX_MESSAGE_ATTACHMENT_BYTES / MIB }
      );
    case "failed":
      return t("读取或上传失败", "Reading or uploading failed");
  }
}

/** What the last attempt to attach left out, and why, until dismissed or superseded. */
export function AttachmentNotice({
  rejected,
  onDismiss
}: {
  rejected: readonly AttachmentRejection[];
  onDismiss: () => void;
}) {
  const { t } = useI18n();
  if (!rejected.length) return null;
  const groups = new Map<AttachmentRejectionReason, { names: string[]; count: number }>();
  for (const entry of rejected) {
    const group = groups.get(entry.reason) ?? { names: [], count: 0 };
    group.count += 1;
    if (entry.name) group.names.push(entry.name);
    groups.set(entry.reason, group);
  }
  return (
    <div className="attachment-notice" role="alert">
      <TriangleAlert size={14} aria-hidden="true" />
      <div className="attachment-notice__body">
        <strong>{t("{count} 项没有添加", "{count} item(s) not attached", { count: rejected.length })}</strong>
        <ul>
          {[...groups].map(([reason, group]) => (
            <li key={reason}>
              {group.names.length ? (
                <span className="attachment-notice__names">{group.names.join(t("、", ", "))}</span>
              ) : null}
              <span>{rejectionReasonLabel(t, reason)}</span>
            </li>
          ))}
        </ul>
      </div>
      <IconButton label={t("关闭提示", "Dismiss")} className="attachment-notice__dismiss" onClick={onDismiss}>
        <X size={13} />
      </IconButton>
    </div>
  );
}
