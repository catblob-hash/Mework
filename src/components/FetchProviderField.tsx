import { ChevronDown } from "lucide-react";
import { useI18n } from "../i18n";
import { SEARCH_PROVIDERS } from "../lib/searchProviders";
import type { FetchProviderSelection, NativeFetchTool, WebSearchAssets } from "../types";
import { NATIVE_FETCH_TOOLS } from "../types";
import { Field } from "./Common";
import { LockMark, lockedFieldHint, lockToneClass, type BackendLock } from "./LockTone";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem, PopoverMenuSection } from "./PopoverMenu";

/** The row id for following the parent selection; not a `SearchProviderKind`. */
const INHERIT_VALUE = "inherit";

/**
 * The Messages `web_fetch` versions this surface may pick between, and which
 * one it is carrying. Absent wherever there is no version to offer.
 */
export interface NativeFetchToolChoice {
  selected: NativeFetchTool;
  onSelect: (next: NativeFetchTool) => void;
}

interface FetchProviderFieldProps {
  /** `null` follows the parent selection and is available only with `inheritOption`. */
  value: FetchProviderSelection | null;
  onChange: (next: FetchProviderSelection | null) => void;
  webSearchAssets: WebSearchAssets;
  /** Whether to offer the parent-selection option. Conversations have no parent. */
  inheritOption?: boolean;
  hint?: string;
  /**
   * How the conversation's lock draws this selector: gray cannot move, and its
   * note replaces the standing advice to say why; orange moves, and its note
   * says what moving it costs.
   */
  lock?: BackendLock | null;
  /** Opens the native row into a second step naming the wire tool version. */
  nativeToolChoice?: NativeFetchToolChoice;
}

/**
 * Shared fetch-provider picker for conversations and subagent roles.
 *
 * The twin of `SearchProviderField`, and deliberately its own component rather
 * than a second spelling inside the conversation's web settings: a role picks
 * its fetch backend on the same terms a conversation does, and one recipe is
 * what keeps the two from drifting. Lists only catalog entries that can fetch
 * and are switched on; a selection naming a provider that has since been
 * switched off is said plainly rather than silently repaired.
 */
export function FetchProviderField({
  value,
  onChange,
  webSearchAssets,
  inheritOption = false,
  hint,
  lock,
  nativeToolChoice
}: FetchProviderFieldProps) {
  const { t } = useI18n();
  const fetchProviders = SEARCH_PROVIDERS.filter((provider) => provider.fetch
    && webSearchAssets.providers.some((item) => item.kind === provider.kind && item.enabled));
  const nativeLabel = t("原生", "Native");
  const offLabel = t("不启用", "Off");
  const isNative = value?.kind === "native";
  /* The version only belongs on the trigger where it is actually sent. */
  const triggerVersion = isNative && nativeToolChoice ? nativeToolChoice.selected : null;
  /* A named provider that is no longer switched on. Said plainly rather than
     quietly redrawn as "off": the selection is still carried, and showing it as
     a deliberate choice would hide a leg that has stopped working. */
  const unavailable = value?.kind === "explicit"
    && !fetchProviders.some((provider) => provider.kind === value.providerKind);
  const triggerLabel = value === null
    ? t("跟随对话设置", "Follow the conversation")
    : value.kind === "native"
      ? nativeLabel
      : value.kind === "disabled"
        ? offLabel
        : unavailable
          ? t("请修复抓取提供商", "Repair fetch provider")
          : fetchProviders.find((provider) => provider.kind === value.providerKind)?.label ?? "";

  const nativeRow: PopoverMenuItem = {
    id: "native",
    label: nativeLabel,
    checked: isNative,
    hint: triggerVersion ?? undefined,
    children: nativeToolChoice
      ? NATIVE_FETCH_TOOLS.map((version) => ({
        id: version,
        label: version,
        // Checked by the version this surface carries, not by whether native is
        // the backend right now: the second step answers "which spelling", and
        // it keeps its answer while another backend fetches.
        checked: nativeToolChoice.selected === version,
        onSelect: () => {
          onChange({ kind: "native" });
          nativeToolChoice.onSelect(version);
        }
      }))
      : undefined,
    onSelect: nativeToolChoice ? undefined : () => onChange({ kind: "native" })
  };

  const sections: PopoverMenuSection[] = [{
    id: "backends",
    items: [
      ...(inheritOption
        ? [{
          id: INHERIT_VALUE,
          label: t("跟随对话设置", "Follow the conversation"),
          checked: value === null,
          onSelect: () => onChange(null)
        }]
        : []),
      nativeRow,
      ...fetchProviders.map((provider) => ({
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

  const disabled = lock?.tone === "hard";
  return <Field
    label={t("抓取提供商", "Fetch provider")}
    hint={lockedFieldHint(lock, unavailable
      ? t(
        "已选抓取提供商未启用，抓取网页当前无法执行。请改选原生、一个已启用的提供商，或「不启用」；旧选择不会被静默恢复。",
        "The selected fetch provider is disabled, so page fetching cannot run. Choose native, an enabled provider, or “Off”; the old selection is not silently restored."
      )
      : hint ?? t(
        "抓取网页与联网搜索是两件事，可以分别指定后端。选「原生」是交给当前模型自己的提供商：像 Anthropic 那样把抓取拆成独立服务端工具的，会多出抓取网页这个工具；像 OpenAI 那样把抓取并进搜索的，则不会多出工具，抓取在搜索内部完成。选「不启用」则模型只看得到联网搜索。",
        "Fetching a page and searching the web are two capabilities, and each can name its own backend. “Native” hands it to the conversation's own model provider: a provider that splits retrieval into its own server tool, as Anthropic does, gains a page-fetch tool; one that folds retrieval into search, as OpenAI does, gains no second tool and fetches inside the search call. Choose “Off” and the model sees web search alone."
      ))}
  >
    <PopoverMenu
      rootClassName="popover-select"
      triggerClassName={`input popover-select__trigger${unavailable ? " input--error" : ""}${lockToneClass("popover-select__trigger", lock?.tone)}`}
      triggerLabel={t("抓取提供商：{value}", "Fetch provider: {value}", { value: triggerLabel })}
      trigger={<>
        <span className="popover-select__value">{triggerLabel}</span>
        {triggerVersion && <span className="popover-select__note">{triggerVersion}</span>}
        <LockMark tone={lock?.tone} />
        <ChevronDown size={14} className="popover-select__chevron" aria-hidden="true" />
      </>}
      disabled={disabled}
      submenu="flyout"
      menuLabel={t("抓取提供商", "Fetch provider")}
      sections={sections}
    />
  </Field>;
}
