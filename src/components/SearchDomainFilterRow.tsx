import { useI18n } from "../i18n";
import type { SearchDomainFilterMode } from "../types";
import { SearchDomainRulesWindow } from "./SearchDomainRulesWindow";

/** Which of the two lists the rules window edits. */
type DomainList = Exclude<SearchDomainFilterMode, "off">;

interface SearchDomainFilterRowProps {
  /** `null` follows the parent selection and is available only with `inheritOption`. */
  mode: SearchDomainFilterMode | null;
  includeDomains: readonly string[];
  excludeDomains: readonly string[];
  /** Whether to offer the parent-selection option. Conversations have no parent. */
  inheritOption?: boolean;
  inheritLabel?: string;
  hint: string;
  windowOpen: boolean;
  onOpenWindow: () => void;
  onCloseWindow: () => void;
  onChangeMode: (next: SearchDomainFilterMode | null) => void;
  onChangeRules: (list: DomainList, rules: string[]) => void;
}

/** The row id for following the parent selection; not a filter mode. */
const INHERIT_VALUE = "inherit";

/**
 * The domain-filter row: which list is in effect, and the way in to writing
 * both of them.
 *
 * Deliberately NOT a `Field`: that renders a `<label>`, and a label makes the
 * whole row a click target for the first labelable thing inside it — which here
 * is the link that opens a window, not the picker. A row that opens a window
 * when its description is clicked is one nobody asked for.
 *
 * Shared by conversations and subagent roles, so the two cannot drift. A role
 * adds one more answer: follow whatever the calling conversation filters by,
 * lists included — which is why the lists are only reachable once the row names
 * a mode of its own.
 */
export function SearchDomainFilterRow({
  mode,
  includeDomains,
  excludeDomains,
  inheritOption = false,
  inheritLabel,
  hint,
  windowOpen,
  onOpenWindow,
  onCloseWindow,
  onChangeMode,
  onChangeRules
}: SearchDomainFilterRowProps) {
  const { t } = useI18n();
  /* The window opens on the list the selector has in effect. With filtering off
     — or inherited — there is no list in effect, so it opens on the blocklist:
     the one a person reaching for this row almost always means. */
  const openDomainList: DomainList = mode === "include" ? "include" : "exclude";
  const inherited = mode === null;

  return <>
    <div className="field">
      <span className="field__label">{t("域名过滤", "Domain filter")}</span>
      <div className="field__control-pair">
        <button
          type="button"
          className="text-button"
          /* Nothing to write while the row follows its caller: the lists in
             effect then are the caller's, and this window edits neither. */
          disabled={inherited}
          onClick={onOpenWindow}
        >{t("编辑名单", "Edit lists")}</button>
        <select
          className="input"
          aria-label={t("域名过滤", "Domain filter")}
          value={mode ?? INHERIT_VALUE}
          onChange={(event) => onChangeMode(
            event.target.value === INHERIT_VALUE
              ? null
              : (event.target.value as SearchDomainFilterMode)
          )}
        >
          {inheritOption && (
            <option value={INHERIT_VALUE}>
              {inheritLabel ?? t("跟随对话设置", "Follow the conversation")}
            </option>
          )}
          <option value="exclude">{t("启用黑名单", "Use blocklist")}</option>
          <option value="include">{t("启用白名单", "Use allowlist")}</option>
          <option value="off">{t("不启用", "Off")}</option>
        </select>
      </div>
      <span className="field__hint">{hint}</span>
    </div>

    {windowOpen && !inherited && (
      <SearchDomainRulesWindow
        includeDomains={includeDomains}
        excludeDomains={excludeDomains}
        initialList={openDomainList}
        onChange={onChangeRules}
        onClose={onCloseWindow}
      />
    )}
  </>;
}
