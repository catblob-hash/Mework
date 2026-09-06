import { useI18n } from "../i18n";
import { SEARCH_PROVIDERS } from "../lib/searchProviders";
import type { SearchProviderSelection, WebSearchAssets } from "../types";
import { Field } from "./Common";

/** The `<option value>` for following the parent selection. It is neither a catalog `SearchProviderKind` nor `native`, so it cannot collide with a real choice. */
const INHERIT_VALUE = "inherit";

interface SearchProviderFieldProps {
  /** `null` follows the parent selection and is available only with `inheritOption`. */
  value: SearchProviderSelection | null;
  onChange: (next: SearchProviderSelection | null) => void;
  webSearchAssets: WebSearchAssets;
  /** Whether to offer the parent-selection option. Conversations have no parent, so this defaults to false. */
  inheritOption?: boolean;
  inheritLabel?: string;
  hint?: string;
}

/**
 * Shared search-provider picker for conversations and subagent roles.
 *
 * List only catalog entries with keyword-search capability; fetch providers retrieve specified pages and cannot satisfy this selection. Disabled entries remain visible so existing selections can be repaired.
 */
export function SearchProviderField({
  value,
  onChange,
  webSearchAssets,
  inheritOption = false,
  inheritLabel,
  hint
}: SearchProviderFieldProps) {
  const { t } = useI18n();
  const searchProviders = SEARCH_PROVIDERS.filter((provider) => provider.search);
  const explicitEnabled = value?.kind === "explicit" && webSearchAssets.providers.some(
    (provider) => provider.kind === value.providerKind && provider.enabled
  );
  const unavailable = value?.kind === "unavailable"
    || (value?.kind === "explicit" && !explicitEnabled);
  const selectValue = value === null
    ? INHERIT_VALUE
    : value.kind === "explicit" && explicitEnabled
      ? value.providerKind
      : value.kind === "native" ? "native" : "";

  return <Field
    label={t("搜索提供商", "Search provider")}
    hint={unavailable
      ? t("已选提供商不可用或未启用，联网搜索当前无法执行。请选择原生或一个已启用的提供商；旧选择不会被静默恢复。", "The selected provider is unavailable or disabled, so web search cannot run. Choose native or an enabled provider; the old selection is not silently restored.")
      : hint ?? t("原生表示由当前对话的模型用它自己的联网搜索工具检索，返回的是一段带引用的报告；选一家提供商则由应用自己去检索，返回的是标题、网址与正文摘要。提供商及其凭据在全局设置中管理。", "Native means the conversation's own model searches with its own built-in tool and returns a written report; picking a provider means the app searches itself and returns titles, URLs and snippets. Providers and their credentials are managed in global settings.")}
  >
    <select className={`input${unavailable ? " input--error" : ""}`} value={selectValue} aria-label={t("搜索提供商", "Search provider")} aria-invalid={unavailable} onChange={(event) => {
      if (inheritOption && event.target.value === INHERIT_VALUE) onChange(null);
      else if (event.target.value === "native") onChange({ kind: "native" });
      else {
        const provider = searchProviders.find((candidate) => candidate.kind === event.target.value);
        if (provider) onChange({ kind: "explicit", providerKind: provider.kind });
      }
    }}>
      {unavailable && <option value="" disabled hidden>{t("请修复搜索提供商", "Repair search provider")}</option>}
      {inheritOption && <option value={INHERIT_VALUE}>{inheritLabel ?? t("跟随对话设置", "Follow the conversation")}</option>}
      <option value="native">{t("原生（当前模型）", "Native (current model)")}</option>
      {searchProviders.map((provider) => {
        const enabled = webSearchAssets.providers.some((item) => item.kind === provider.kind && item.enabled);
        return <option key={provider.kind} value={provider.kind}>{enabled ? provider.label : t("{name}（未启用）", "{name} (disabled)", { name: provider.label })}</option>;
      })}
    </select>
  </Field>;
}
