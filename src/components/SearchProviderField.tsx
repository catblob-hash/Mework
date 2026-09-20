import { ChevronDown } from "lucide-react";
import { useI18n } from "../i18n";
import { SEARCH_PROVIDERS } from "../lib/searchProviders";
import type { NativeSearchTool, SearchProviderSelection, WebSearchAssets } from "../types";
import { Field } from "./Common";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem, PopoverMenuSection } from "./PopoverMenu";

/** The row id for following the parent selection. It is neither a catalog `SearchProviderKind` nor `native`, so it cannot collide with a real choice. */
const INHERIT_VALUE = "inherit";

/**
 * The Messages `web_search` versions this conversation may pick between, and
 * which one it is carrying.
 *
 * Absent on every surface that has no version to offer — a subagent role, or a
 * conversation whose model speaks another protocol — and the native row is then
 * a plain choice that selects immediately. The version is deliberately not
 * cleared in that case: it stays on the conversation so that coming back to a
 * Messages model comes back to the same version.
 */
export interface NativeSearchToolChoice {
  offered: readonly NativeSearchTool[];
  selected: NativeSearchTool;
  onSelect: (next: NativeSearchTool) => void;
}

interface SearchProviderFieldProps {
  /** `null` follows the parent selection and is available only with `inheritOption`. */
  value: SearchProviderSelection | null;
  onChange: (next: SearchProviderSelection | null) => void;
  webSearchAssets: WebSearchAssets;
  /** Whether to offer the parent-selection option. Conversations have no parent, so this defaults to false. */
  inheritOption?: boolean;
  inheritLabel?: string;
  hint?: string;
  /** Draws the selection as settled rather than removable. */
  disabled?: boolean;
  /** Replaces the standing advice while disabled, to say why it cannot move. */
  disabledHint?: string;
  /** Opens the native row into a second step naming the wire tool version. */
  nativeToolChoice?: NativeSearchToolChoice;
}

/**
 * Shared search-provider picker for conversations and subagent roles.
 *
 * Lists only catalog entries that both do keyword search and are switched on:
 * a fetch-only provider cannot satisfy this selection, and a disabled one is
 * not a choice — offering it greyed out would make the menu a list of things
 * that do not work. A selection naming a provider that has since been switched
 * off is not silently repaired; the trigger says so and the menu is where a
 * working one is picked instead.
 *
 * It is a menu rather than a `<select>` because the native choice has a second
 * step under it on Messages models — which version of the server-side tool to
 * send — and a second step is exactly what an option list cannot hold.
 */
export function SearchProviderField({
  value,
  onChange,
  webSearchAssets,
  inheritOption = false,
  inheritLabel,
  hint,
  disabled = false,
  disabledHint,
  nativeToolChoice
}: SearchProviderFieldProps) {
  const { t } = useI18n();
  const searchProviders = SEARCH_PROVIDERS.filter((provider) => provider.search
    && webSearchAssets.providers.some((item) => item.kind === provider.kind && item.enabled));
  const explicitEnabled = value?.kind === "explicit" && webSearchAssets.providers.some(
    (provider) => provider.kind === value.providerKind && provider.enabled
  );
  const unavailable = value?.kind === "unavailable"
    || (value?.kind === "explicit" && !explicitEnabled);
  const isNative = value?.kind === "native";
  const nativeLabel = t("原生", "Native");
  const offLabel = t("不启用", "Off");
  const providerLabel = value?.kind === "explicit" && explicitEnabled
    ? searchProviders.find((provider) => provider.kind === value.providerKind)?.label
    : undefined;
  const triggerLabel = unavailable
    ? t("请修复搜索提供商", "Repair search provider")
    : value === null
      ? inheritLabel ?? t("跟随对话设置", "Follow the conversation")
      : isNative
        ? nativeLabel
        : value?.kind === "disabled" ? offLabel : providerLabel ?? "";
  /* The version only belongs on the trigger where it is actually sent. On every
     other model the conversation still carries it, and saying so here would
     claim an effect this request does not have. */
  const triggerVersion = isNative && nativeToolChoice ? nativeToolChoice.selected : null;

  const nativeRow: PopoverMenuItem = {
    id: "native",
    label: nativeLabel,
    checked: isNative,
    hint: triggerVersion ?? undefined,
    children: nativeToolChoice?.offered.map((version) => ({
      id: version,
      label: version,
      // Checked by the version this conversation carries, not by whether native
      // is the backend right now: the second step answers "which spelling", and
      // it keeps its answer while another backend searches.
      checked: nativeToolChoice.selected === version,
      onSelect: () => {
        onChange({ kind: "native" });
        nativeToolChoice.onSelect(version);
      }
    })),
    onSelect: nativeToolChoice ? undefined : () => onChange({ kind: "native" })
  };

  const sections: PopoverMenuSection[] = [{
    id: "backends",
    items: [
      ...(inheritOption
        ? [{
          id: INHERIT_VALUE,
          label: inheritLabel ?? t("跟随对话设置", "Follow the conversation"),
          checked: value === null,
          onSelect: () => onChange(null)
        }]
        : []),
      nativeRow,
      ...searchProviders.map((provider) => ({
        id: provider.kind,
        label: provider.label,
        checked: value?.kind === "explicit" && value.providerKind === provider.kind,
        onSelect: () => onChange({ kind: "explicit", providerKind: provider.kind })
      })),
      {
        id: "disabled",
        label: offLabel,
        checked: value?.kind === "disabled",
        onSelect: () => onChange({ kind: "disabled" })
      }
    ]
  }];

  return <Field
    label={t("搜索提供商", "Search provider")}
    hint={disabled && disabledHint
      ? disabledHint
      : unavailable
        ? t("已选提供商不可用或未启用，联网搜索当前无法执行。请选择原生或一个已启用的提供商；旧选择不会被静默恢复。", "The selected provider is unavailable or disabled, so web search cannot run. Choose native or an enabled provider; the old selection is not silently restored.")
        : hint ?? t("原生表示由当前对话的模型用它自己的联网搜索工具检索，返回的是一段带引用的报告；选一家提供商则由应用自己去检索，返回的是标题、网址与正文摘要。选「不启用」则模型看不到联网搜索这个工具，但仍可以用抓取网页。提供商及其凭据在全局设置中管理。", "Native means the conversation's own model searches with its own built-in tool and returns a written report; picking a provider means the app searches itself and returns titles, URLs and snippets. Choose “Off” and the model sees no web-search tool, though it may still fetch pages. Providers and their credentials are managed in global settings.")}
  >
    <PopoverMenu
      rootClassName="popover-select"
      triggerClassName={`input popover-select__trigger${unavailable ? " input--error" : ""}`}
      triggerLabel={t("搜索提供商：{value}", "Search provider: {value}", { value: triggerLabel })}
      trigger={<>
        <span className="popover-select__value">{triggerLabel}</span>
        {triggerVersion && <span className="popover-select__note">{triggerVersion}</span>}
        <ChevronDown size={14} className="popover-select__chevron" aria-hidden="true" />
      </>}
      disabled={disabled}
      submenu="flyout"
      sections={sections}
      menuLabel={t("搜索提供商", "Search provider")}
    />
  </Field>;
}
