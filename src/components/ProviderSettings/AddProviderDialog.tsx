import { useState } from "react";
import { useI18n } from "../../i18n";
import type { ProviderFamily } from "../../types";
import { Field } from "../Common";
import { DialogShell } from "./DialogShell";
import { API_FORMAT_OPTIONS } from "./endpointMeta";

/**
 * Adds providers through a settings form; API address and key are configured
 * in the right pane after creation.
 */
export function AddProviderDialog({
  onClose,
  onCreate
}: {
  onClose: () => void;
  onCreate: (draft: { name: string; family: ProviderFamily }) => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  // OpenAI Chat Completions is the default for relays and most upstream providers.
  const [family, setProviderFamily] = useState<ProviderFamily>("openai_chat");
  const trimmedName = name.trim();

  const submit = () => {
    if (!trimmedName) return;
    onCreate({ name: trimmedName, family });
  };

  return (
    <DialogShell
      title={t("添加提供商", "Add provider")}
      subtitle={t(
        "填一个名字，选它说哪种对话协议。API 地址与 Key 在右栏填。",
        "Give it a name and pick the chat protocol it speaks. The API address and key go in the right pane."
      )}
      labelledBy="add-provider-title"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="button button--ghost" onClick={onClose}>
            {t("取消", "Cancel")}
          </button>
          <button type="button" className="button button--primary" disabled={!trimmedName} onClick={submit}>
            {t("添加", "Add")}
          </button>
        </>
      }
    >
      <Field label={t("提供商名称", "Provider name")}>
        <input
          className="input"
          aria-label={t("提供商名称", "Provider name")}
          value={name}
          maxLength={32}
          placeholder={t("例如 我的中转站", "Example: My relay")}
          onChange={(event) => setName(event.target.value)}
          onKeyDown={(event) => {
            // DialogShell handles Escape; Enter submits this form.
            if (event.key !== "Enter") return;
            event.preventDefault();
            submit();
          }}
        />
      </Field>

      <Field
        label={t("对话协议", "Chat protocol")}
        hint={t(
          "宿主按它决定对话请求走哪个端点。建好之后在右栏还能改。",
          "The host uses this to decide which endpoint chat requests go to. It stays editable in the right pane."
        )}
      >
        <select
          className="input"
          aria-label={t("对话协议", "Chat protocol")}
          value={family}
          onChange={(event) => setProviderFamily(event.target.value as ProviderFamily)}
        >
          {API_FORMAT_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>{option.label}</option>
          ))}
        </select>
      </Field>
    </DialogShell>
  );
}
