import { RotateCcw } from "lucide-react";
import { useState } from "react";
import { useI18n } from "../i18n";
import { MAX_SYSTEM_PROMPT_BYTES, utf8ByteLength } from "../lib/textLimits";
import { Dialog } from "./Common";

interface SystemPromptDialogProps {
  value: string;
  defaultValue: string;
  onSave: (value: string) => void;
  onClose: () => void;
}

export function SystemPromptDialog({ value, defaultValue, onSave, onClose }: SystemPromptDialogProps) {
  const { resolvedLanguage, t } = useI18n();
  const [draft, setDraft] = useState(value);
  const draftBytes = utf8ByteLength(draft);
  const draftTooLarge = draftBytes > MAX_SYSTEM_PROMPT_BYTES;

  return (
    <Dialog
      title={t("编辑主系统提示词", "Edit main system prompt")}
      description={t("在每次模型请求的最前方生效，只影响当前对话。", "Applied at the beginning of every model request and only affects this conversation.")}
      width="640px"
      onClose={onClose}
      footer={(
        <>
          <button type="button" className="button button--ghost" onClick={onClose}>{t("取消", "Cancel")}</button>
          <button type="button" className="button button--primary" disabled={draftTooLarge} onClick={() => onSave(draft)}>{t("保存", "Save")}</button>
        </>
      )}
    >
      <textarea
        className="input system-prompt-dialog__input"
        rows={12}
        autoFocus
        aria-label={t("主系统提示词", "Main system prompt")}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        placeholder={t(
          "留空则不发送基础系统提示词，只发送能力增补段…",
          "Leave empty to send no base system prompt — only the capability sections…"
        )}
        onKeyDown={(event) => {
          if (!draftTooLarge && (event.ctrlKey || event.metaKey) && event.key === "Enter") {
            event.preventDefault();
            onSave(draft);
          }
        }}
      />
      <div className="prompt-actions">
        <button type="button" className="text-button" onClick={() => setDraft(defaultValue)}>
          <RotateCcw size={13} /> {t("恢复全局默认", "Restore global default")}
        </button>
        <span>{t(
          "{current} / {maximum} 字节",
          "{current} / {maximum} bytes",
          { current: draftBytes.toLocaleString(resolvedLanguage), maximum: MAX_SYSTEM_PROMPT_BYTES.toLocaleString(resolvedLanguage) }
        )}</span>
      </div>
      {draftTooLarge && <p className="scope-note scope-note--error" role="alert">{t("系统提示词超过 1 MiB UTF-8 字节限制，请缩短后再保存。", "The system prompt exceeds the 1 MiB UTF-8 limit. Shorten it before saving.")}</p>}
      <p className="scope-note">{t("时间线里的“系统提示词上下文”是独立历史项，不会覆盖这里的主提示词。", "System prompt contexts in the timeline are separate history items and do not override this main prompt.")}</p>
    </Dialog>
  );
}
