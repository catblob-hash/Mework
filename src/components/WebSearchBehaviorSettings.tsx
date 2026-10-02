import { useState } from "react";
import { useI18n } from "../i18n";
import type {
  ConversationWebSearchSettings,
  FetchProviderSelection,
  NativeFetchTool,
  NativeSearchTool,
  SearchDomainFilterMode,
  SearchProviderSelection,
  WebSearchAssets
} from "../types";
import { NATIVE_SEARCH_TOOLS } from "../types";
import { FetchProviderField } from "./FetchProviderField";
import type { BackendLock } from "./LockTone";
import { SearchDomainFilterRow } from "./SearchDomainFilterRow";
import { SearchProviderField } from "./SearchProviderField";
import { SearchResultShapingFields } from "./SearchResultShapingFields";

/**
 * The same four answers as a subagent role gives them, where each row that has
 * a caller to follow may answer `null` — follow the calling conversation.
 *
 * Result shaping is the exception, and on purpose: 0 already means "no cap"
 * there, leaving no value over to spell inheritance with, so a role always
 * answers those two for itself.
 */
export interface InheritableWebSearchSettings {
  provider: SearchProviderSelection | null;
  fetchProvider: FetchProviderSelection | null;
  maxResults: number;
  compressionCutoff: number;
  domainFilter: SearchDomainFilterMode | null;
  includeDomains: string[];
  excludeDomains: string[];
}

interface WebSearchBehaviorCommonProps {
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
   * How the conversation's lock draws the search selector (`backendTone`):
   * gray once a native search has sealed results into the transcript, or on a
   * frozen surface; orange while a host-run backend's cache is warm.
   */
  searchLock?: BackendLock | null;
  /** The same for the fetch selector. */
  fetchLock?: BackendLock | null;
}

export type WebSearchBehaviorSettingsProps = WebSearchBehaviorCommonProps & (
  | {
    /** A conversation or a preset: there is no caller above it to follow. */
    inheritOption?: false;
    value: ConversationWebSearchSettings;
    onChange: (patch: Partial<ConversationWebSearchSettings>) => void;
  }
  | {
    /**
     * A subagent role: every row that can follow the calling conversation
     * offers to, and the hints speak about the role rather than about a
     * conversation.
     */
    inheritOption: true;
    value: InheritableWebSearchSettings;
    onChange: (patch: Partial<InheritableWebSearchSettings>) => void;
  }
);

/** Every field either shape can be patched with. */
type AnyWebSearchPatch = Partial<InheritableWebSearchSettings> & {
  nativeSearchTool?: NativeSearchTool;
  nativeFetchTool?: NativeFetchTool;
};

/**
 * Search and fetch backend selection for conversations, presets and subagent
 * roles, with the result shaping and domain filtering that go with them.
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
 * A role asks the same four questions with a "follow the conversation" answer
 * added to each, through this same component: one recipe for every surface is
 * what keeps them from drifting.
 */
export function WebSearchBehaviorSettings(props: WebSearchBehaviorSettingsProps) {
  const {
    webSearchAssets,
    nativeFetchAvailable = false,
    nativeToolTypeSelectable = false,
    searchLock,
    fetchLock
  } = props;
  const { t } = useI18n();
  const [domainWindowOpen, setDomainWindowOpen] = useState(false);
  const inheritOption = props.inheritOption === true;
  const value: InheritableWebSearchSettings = props.value;
  /* One writer for both shapes. A `null` only ever comes from a "follow the
     conversation" row, and those are only drawn with `inheritOption` — so the
     conversation's shape, which has no such rows, is never handed one. */
  const onChange = props.onChange as (patch: AnyWebSearchPatch) => void;
  /* The version is a Messages spelling, carried by a conversation alone: a role
     picks which backend searches, never how that backend's tool is spelled. */
  const versions = props.inheritOption ? null : props.value;
  /* The fetch version is a Messages spelling, so it is offered only where it is
     sent — and only where the family actually grants a separate fetch tool for
     it to be written onto. */
  const fetchVersionSelectable = nativeToolTypeSelectable && nativeFetchAvailable;

  return <>
    <SearchProviderField
      value={value.provider}
      onChange={(provider) => {
        if (provider || inheritOption) onChange({ provider });
      }}
      webSearchAssets={webSearchAssets}
      inheritOption={inheritOption}
      hint={inheritOption ? t(
        "这个角色的「联网搜索」用哪个后端。留在「跟随对话设置」就用调用方对话的选择。选「原生」时用的是这个角色自己的模型——它所在的协议家族如果不支持模型自带搜索，检索会以可修复的错误失败，而不会悄悄换一家。",
        "Which backend this role's web search goes through. Leave it on \"follow the conversation\" to use the caller's choice. \"Native\" means this role's OWN model — if its protocol family has no built-in search, the search fails with a fixable error rather than quietly switching backends."
      ) : undefined}
      lock={searchLock}
      nativeToolChoice={versions && nativeToolTypeSelectable
        ? {
          offered: NATIVE_SEARCH_TOOLS,
          selected: versions.nativeSearchTool,
          onSelect: (version: NativeSearchTool) => onChange({ nativeSearchTool: version })
        }
        : undefined}
    />

    <FetchProviderField
      value={value.fetchProvider}
      onChange={(fetchProvider) => {
        if (fetchProvider || inheritOption) onChange({ fetchProvider });
      }}
      webSearchAssets={webSearchAssets}
      inheritOption={inheritOption}
      hint={inheritOption ? t(
        "这个角色抓取网页用哪个后端，和上面的搜索后端各答各的。留在「跟随对话设置」就用调用方对话的选择。这里选什么都不能让一个关掉联网的对话联网——联网与否由对话决定，这里只决定由谁去抓。",
        "Which backend this role fetches pages with, answered separately from the search backend above. Leave it on \"follow the conversation\" to use the caller's choice. Nothing chosen here can put a conversation that is offline back on the network: whether to reach the web at all is the conversation's decision, and this one is only who does the fetching."
      ) : undefined}
      lock={fetchLock}
      nativeToolChoice={versions && fetchVersionSelectable
        ? {
          selected: versions.nativeFetchTool,
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
      inheritOption={inheritOption}
      windowOpen={domainWindowOpen}
      onOpenWindow={() => setDomainWindowOpen(true)}
      onCloseWindow={() => setDomainWindowOpen(false)}
      onChangeMode={(domainFilter) => {
        if (domainFilter || inheritOption) onChange({ domainFilter });
      }}
      onChangeRules={(list, rules) => onChange(
        list === "include" ? { includeDomains: rules } : { excludeDomains: rules }
      )}
      hint={inheritOption ? t(
        "这个角色怎么按域名筛检索结果。留在「跟随对话设置」就连名单一起用调用方对话的；一旦自己选了模式，用的就只有下面这两份名单，不再叠加对话的。",
        "How this role filters results by domain. Left on \"follow the conversation\" it uses the caller's mode AND the caller's lists; naming a mode of its own switches to these two lists alone, which are not layered onto the conversation's."
      ) : t(
        "按域名筛掉检索结果。黑名单丢掉命中的，白名单只留下命中的，两者只有一个生效；关掉过滤不会清空已经写好的名单。",
        "Filters results by domain. A blocklist drops what it matches, an allowlist keeps only what it matches, and only one of them is ever in effect. Turning filtering off does not empty either list."
      )}
    />
  </>;
}
