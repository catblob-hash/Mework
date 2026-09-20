import { useState } from "react";
import { useI18n } from "../i18n";
import type {
  ConversationWebSearchSettings,
  NativeSearchTool,
  SearchDomainFilterMode,
  WebSearchAssets
} from "../types";
import { NATIVE_SEARCH_TOOLS } from "../types";
import { FetchProviderField } from "./FetchProviderField";
import { SearchDomainFilterRow } from "./SearchDomainFilterRow";
import { SearchProviderField } from "./SearchProviderField";
import { SearchResultShapingFields } from "./SearchResultShapingFields";

interface WebSearchBehaviorSettingsProps {
  value: ConversationWebSearchSettings;
  onChange: (patch: Partial<ConversationWebSearchSettings>) => void;
  webSearchAssets: WebSearchAssets;
  /**
   * Whether this conversation's own model exposes page retrieval as a server
   * tool of its own. Only Anthropic-family models do; everywhere else "the
   * model's own provider fetches" means retrieval happens inside the one search
   * tool, so the choice is still offered and simply grants no second web tool.
   */
  nativeFetchAvailable?: boolean;
  /**
   * Whether this conversation's model spells its native web tools the Messages
   * way, with the version written into the tool's own `type`. Only then is
   * there a version for the user to pick, so only then does the native row open
   * into a second step. On any other model the row selects native directly and
   * the versions this conversation is carrying stay untouched, ready for the
   * next Messages model it runs on.
   */
  nativeToolTypeSelectable?: boolean;
  /**
   * Said of a selector a run has already acted on. Both backends are pinned the
   * moment their tool reaches the model: the transcript holds results only that
   * backend could have produced, so a different one cannot take over mid-way.
   */
  lockedHint?: string;
  /** Pins the search backend. Set once `web_search` has gone out. */
  searchLocked?: boolean;
  /** Pins the fetch backend. Set once `web_fetch` has gone out. */
  fetchLocked?: boolean;
}

/**
 * Search and fetch backend selection for conversations and presets, with the
 * result shaping and domain filtering that go with them.
 *
 * The two selectors are independent because upstreams disagree about how many
 * web tools there are. Anthropic splits retrieval into a second server tool;
 * DeepSeek and OpenAI keep it inside their one search tool. So "native" is a
 * legal answer on both sides, and on a family of the second kind choosing it
 * for both simply means the model is handed a single `web_search` — which is
 * that family's own shape, not a missing feature.
 *
 * Both selectors list only backends that are switched on, and both can be
 * turned off outright: a conversation may search without fetching, fetch
 * without searching, or — with web access still on — do neither, which is a
 * conversation whose settings say plainly that it has no web tools rather than
 * one whose menus are full of choices that do not work.
 *
 * Each of the four rows is its own component, shared with the subagent-role
 * editor, which asks the same four questions with a "follow the conversation"
 * answer added to each. One recipe per row is what keeps the two from drifting.
 */
export function WebSearchBehaviorSettings({
  value,
  onChange,
  webSearchAssets,
  nativeFetchAvailable = false,
  nativeToolTypeSelectable = false,
  lockedHint,
  searchLocked = false,
  fetchLocked = false
}: WebSearchBehaviorSettingsProps) {
  const { t } = useI18n();
  const [domainWindowOpen, setDomainWindowOpen] = useState(false);
  /* The fetch version is a Messages spelling, so it is offered only where it is
     sent — and only where the family actually grants a separate fetch tool for
     it to be written onto. */
  const fetchVersionSelectable = nativeToolTypeSelectable && nativeFetchAvailable;

  return <>
    <SearchProviderField
      value={value.provider}
      onChange={(provider) => {
        // `inheritOption` is disabled, so `null` cannot reach this callback.
        if (provider) onChange({ provider });
      }}
      webSearchAssets={webSearchAssets}
      disabled={searchLocked}
      disabledHint={searchLocked ? lockedHint : undefined}
      nativeToolChoice={nativeToolTypeSelectable
        ? {
          offered: NATIVE_SEARCH_TOOLS,
          selected: value.nativeSearchTool,
          onSelect: (version: NativeSearchTool) => onChange({ nativeSearchTool: version })
        }
        : undefined}
    />

    <FetchProviderField
      value={value.fetchProvider}
      onChange={(fetchProvider) => {
        // `inheritOption` is disabled, so `null` cannot reach this callback.
        if (fetchProvider) onChange({ fetchProvider });
      }}
      webSearchAssets={webSearchAssets}
      disabled={fetchLocked}
      disabledHint={fetchLocked ? lockedHint : undefined}
      nativeToolChoice={fetchVersionSelectable
        ? {
          selected: value.nativeFetchTool,
          onSelect: (version) => onChange({ nativeFetchTool: version })
        }
        : undefined}
    />

    <SearchResultShapingFields
      maxResults={value.maxResults}
      compressionCutoff={value.compressionCutoff}
      onChangeMaxResults={(maxResults) => onChange({ maxResults })}
      onChangeCompressionCutoff={(compressionCutoff) => onChange({ compressionCutoff })}
    />

    <SearchDomainFilterRow
      mode={value.domainFilter}
      includeDomains={value.includeDomains}
      excludeDomains={value.excludeDomains}
      windowOpen={domainWindowOpen}
      onOpenWindow={() => setDomainWindowOpen(true)}
      onCloseWindow={() => setDomainWindowOpen(false)}
      onChangeMode={(domainFilter) => {
        // `inheritOption` is off here, so `null` cannot reach this callback.
        if (domainFilter) onChange({ domainFilter: domainFilter as SearchDomainFilterMode });
      }}
      onChangeRules={(list, rules) => onChange(
        list === "include" ? { includeDomains: rules } : { excludeDomains: rules }
      )}
      hint={t(
        "按域名筛掉检索结果。黑名单丢掉命中的，白名单只留下命中的，两者只有一个生效；关掉过滤不会清空已经写好的名单。",
        "Filters results by domain. A blocklist drops what it matches, an allowlist keeps only what it matches, and only one of them is ever in effect. Turning filtering off does not empty either list."
      )}
    />
  </>;
}
