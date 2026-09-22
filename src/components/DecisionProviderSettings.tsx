import { Plus } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { useI18n } from "../i18n";
import {
  DECISION_PROVIDERS,
  decisionProviderEntry,
  deleteDecisionApiKey,
  getDecisionKeyStatus,
  revealDecisionApiKey,
  saveDecisionApiKey,
  type DecisionProviderKind
} from "../lib/decisionProviders";
import { hasBackendRuntime } from "../lib/backend";
import type { ApiKeyStatus } from "../types";
import { SecretField } from "./SecretField";
import { SettingsRail, SettingsRailRow } from "./SettingsRail";

interface DecisionProviderSettingsProps {
  onFlush?: () => Promise<void>;
}

/**
 * Decision-model provider settings are a fixed catalog: TypeSafe owns the JEV
 * model credential, and the renderer does not expose endpoint or model choices.
 */
export function DecisionProviderSettings({ onFlush }: DecisionProviderSettingsProps) {
  const { t } = useI18n();
  const [selectedRow, setSelectedRow] = useState<DecisionProviderKind>(DECISION_PROVIDERS[0].kind);
  const [configured, setConfigured] = useState(false);
  const desktopRuntime = hasBackendRuntime();
  const entry = decisionProviderEntry(selectedRow);
  const credentials = useMemo(() => ({
    status: () => getDecisionKeyStatus(selectedRow),
    save: (secret: string) => saveDecisionApiKey(selectedRow, secret),
    reveal: () => revealDecisionApiKey(selectedRow),
    remove: () => deleteDecisionApiKey(selectedRow)
  }), [selectedRow]);
  const onStatusChange = useCallback((status: ApiKeyStatus) => {
    setConfigured(status.configured);
  }, []);
  const secretHelp = desktopRuntime
    ? t(
      "输入后失去焦点会自动保存；清空后失焦即删除。明文存在系统凭据库里，不会写进对话文档。",
      "Changes save on blur; clearing the field and blurring deletes it. The secret lives in the system credential store and is never written to conversation documents."
    )
    : t(
      "浏览器预览不会发起真实请求，也不会保存凭据明文。",
      "Browser preview does not send real requests or store credentials in plain text."
    );
  const help = `${secretHelp} ${t(
    "该密钥用于 JEV 决策模型（jev-latest）的 find/choice 工具。",
    "This key is used by the JEV decision model (jev-latest) for the find/choice tools."
  )}`;

  return (
    <div className="settings-editor-page settings-rail-page decision-provider-page">
      <SettingsRail
        footer={(
          <button
            type="button"
            className="provider-rail__add"
            disabled
            title={t(
              "决策模型提供商目录随应用发布，暂不支持自定义条目。",
              "The decision-model provider catalog ships with the app; custom entries are not supported yet."
            )}
          ><Plus size={13} /> {t("添加提供商", "Add provider")}</button>
        )}
      >
        {DECISION_PROVIDERS.map((provider) => (
          <SettingsRailRow
            key={provider.kind}
            label={provider.label}
            selected={provider.kind === selectedRow}
            active={configured}
            onSelect={() => setSelectedRow(provider.kind)}
          />
        ))}
      </SettingsRail>

      <div className="provider-pane">
        <header className="provider-pane__header">
          <div className="provider-pane__identity">
            <h1>{entry.label}</h1>
          </div>
        </header>
        <div className="provider-pane__body">
          <div className="provider-pane__stack">
            <SecretField
              identity={`${selectedRow}:apiKey`}
              credentials={credentials}
              label="API Key"
              required
              help={help}
              onFlush={onFlush}
              onStatusChange={onStatusChange}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
