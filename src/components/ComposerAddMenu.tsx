import { ImagePlus, Plus } from "lucide-react";
import { useRef } from "react";
import { useI18n } from "../i18n";
import { PopoverMenu } from "./PopoverMenu";

/**
 * Add menu in the composer's lower-left corner.
 */
export function ComposerAddImages({
  disabled = false,
  imageUnavailableReason,
  onChooseImages
}: {
  disabled?: boolean;
  imageUnavailableReason?: string;
  onChooseImages: (files: File[]) => void;
}) {
  const { t } = useI18n();
  const imageInputRef = useRef<HTMLInputElement>(null);
  const unavailable = Boolean(imageUnavailableReason);

  return (
    <div className="composer-add-menu">
      <input
        ref={imageInputRef}
        className="composer-add-menu__file-input"
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif"
        multiple
        tabIndex={-1}
        aria-hidden="true"
        onChange={(event) => {
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          if (files.length) onChooseImages(files);
        }}
      />
      <PopoverMenu
        trigger={<Plus size={16} />}
        triggerLabel={t("添加内容", "Add content")}
        triggerClassName="composer-add-menu__trigger"
        disabled={disabled}
        menuLabel={t("添加内容", "Add content")}
        menuWidth={252}
        sections={[{
          id: "attach",
          items: [{
            id: "images",
            label: t("添加图片", "Add images"),
            description: imageUnavailableReason
              ?? t(
                "PNG、JPEG、WebP 或静态 GIF；也可粘贴或拖入",
                "PNG, JPEG, WebP, or a non-animated GIF; paste and drop also work"
              ),
            icon: <ImagePlus size={15} />,
            disabled: unavailable,
            onSelect: () => imageInputRef.current?.click()
          }]
        }]}
      />
    </div>
  );
}
