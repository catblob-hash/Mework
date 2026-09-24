import { FolderOpen } from "lucide-react";
import { useI18n } from "../../i18n";

/**
 * What the pane says about a file it has no way to show, and the one thing it
 * can still do with it: point at it in the system file manager.
 */
export function UnsupportedNotice({ message, onReveal }: { message: string; onReveal?: () => void }) {
  const { t } = useI18n();
  return (
    <div className="file-preview__unsupported">
      <p>{message}</p>
      {onReveal && (
        <button type="button" className="file-preview__reveal" onClick={onReveal}>
          <FolderOpen size={13} aria-hidden="true" />
          <span>{t("在文件管理器中显示", "Show in file manager")}</span>
        </button>
      )}
    </div>
  );
}
