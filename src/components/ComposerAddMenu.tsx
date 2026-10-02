import { Plus } from "lucide-react";
import { useRef } from "react";
import { useI18n } from "../i18n";
import { PopoverMenu } from "./PopoverMenu";

/**
 * Add menu in the lower-left corner of a message being written — the composer,
 * or a user message edited in place on the timeline.
 *
 * Pictures are one kind of file, so there is one way in: the picker takes
 * anything, and what the message cannot carry is turned away with a reason
 * once the files are read, the same as a paste or a drop.
 */
export function ComposerAddFiles({
  disabled = false,
  imageInput = true,
  unavailableReason,
  onChooseFiles
}: {
  disabled?: boolean;
  /** Whether the model this message goes to can see images; only changes the hint. */
  imageInput?: boolean;
  /** Why nothing can be attached right now (no model, attachments still being prepared). */
  unavailableReason?: string;
  onChooseFiles: (files: File[]) => void;
}) {
  const { t } = useI18n();
  const inputRef = useRef<HTMLInputElement>(null);

  return (
    <div className="composer-add-menu">
      <input
        ref={inputRef}
        className="composer-add-menu__file-input"
        type="file"
        multiple
        tabIndex={-1}
        aria-hidden="true"
        onChange={(event) => {
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          if (files.length) onChooseFiles(files);
        }}
      />
      <PopoverMenu
        trigger={<Plus size={16} />}
        triggerLabel={t("添加内容", "Add content")}
        triggerClassName="composer-add-menu__trigger"
        disabled={disabled}
        menuLabel={t("添加内容", "Add content")}
        dense
        sections={[{
          id: "attach",
          items: [{
            id: "files",
            label: t("上传文件", "Upload files"),
            title: unavailableReason
              ?? (imageInput
                ? t(
                  "图片、PDF 或文本文件；也可粘贴或拖入",
                  "Images, PDFs, or text files; paste and drop also work"
                )
                : t(
                  "PDF 或文本文件（当前模型不支持图片）；也可粘贴或拖入",
                  "PDFs or text files (the current model takes no images); paste and drop also work"
                )),
            disabled: Boolean(unavailableReason),
            onSelect: () => inputRef.current?.click()
          }]
        }]}
      />
    </div>
  );
}
