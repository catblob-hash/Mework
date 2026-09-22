import { Plus } from "lucide-react";
import { useMemo, useState } from "react";
import { useI18n } from "../i18n";
import { hasBackendRuntime } from "../lib/backend";
import { SEARCH_PROVIDERS, searchProviderEntry } from "../lib/searchProviders";
import {
  deleteSearchApiKey,
  getSearchKeyStatus,
  revealSearchApiKey,
  saveSearchApiKey,
  type SearchCredentialSlot
} from "../lib/webSearch";
import type {
  SearchProviderConfig,
  SearchProviderKind,
  WebSearchAssets
} from "../types";
import { Switch } from "./Common";
import { SecretField } from "./SecretField";
import { SettingsRail, SettingsRailRow } from "./SettingsRail";

type WebSearchAssetsChange = WebSearchAssets | ((current: WebSearchAssets) => WebSearchAssets);

interface WebSearchSettingsProps {
  settings: WebSearchAssets;
  onChange: (change: WebSearchAssetsChange) => void;
  onFlush?: () => Promise<void>;
}

/**
 * An input parsed while typing needs its own draft.
 *
 * Controlled values must not render `parse(value)`: parsing a trailing comma or
 * an empty numeric field would destroy the user's in-progress input. Clear the
 * draft only on blur, when the normalized value may be shown again.
 */
function useEditDraft(value: string) {
  const [draft, setDraft] = useState<string | null>(null);
  return {
    value: draft ?? value,
    onChange: setDraft,
    onBlur: () => setDraft(null)
  };
}

function providerConfig(settings: WebSearchAssets, kind: SearchProviderKind): SearchProviderConfig {
  return settings.providers.find((provider) => provider.kind === kind) ?? {
    kind,
    enabled: false,
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  };
}

function useSearchCredentialPort(providerKind: SearchProviderKind, slot: SearchCredentialSlot) {
  return useMemo(() => ({
    status: () => getSearchKeyStatus(providerKind, slot),
    save: (secret: string) => saveSearchApiKey(providerKind, slot, secret),
    reveal: () => revealSearchApiKey(providerKind, slot),
    remove: () => deleteSearchApiKey(providerKind, slot)
  }), [providerKind, slot]);
}

/**
 * Search provider settings.
 *
 * The shared provider layout has a catalog rail and a detail pane, and nothing
 * else: every setting here belongs to one provider row. How a search behaves —
 * result count, compression, domain rules — is asked of the conversation that
 * runs it, so there is no global page for it to sit on.
 */
export function WebSearchSettings({ settings, onChange, onFlush }: WebSearchSettingsProps) {
  const { t } = useI18n();
  const [selectedRow, setSelectedRow] = useState<SearchProviderKind>(SEARCH_PROVIDERS[0].kind);
  const desktopRuntime = hasBackendRuntime();

  const updateProvider = (kind: SearchProviderKind, patch: Partial<SearchProviderConfig>) => {
    onChange((current) => ({
      ...current,
      providers: SEARCH_PROVIDERS.map((catalogProvider) => {
        const provider = providerConfig(current, catalogProvider.kind);
        return catalogProvider.kind === kind ? { ...provider, ...patch } : provider;
      })
    }));
  };

  return (
    <div className="settings-editor-page settings-rail-page search-provider-page">
      <SettingsRail
        footer={(
          <button
            type="button"
            className="provider-rail__add"
            disabled
            title={t(
              "搜索提供商目录随应用发布，暂不支持自定义条目。",
              "The search-provider catalog ships with the app; custom entries are not supported yet."
            )}
          ><Plus size={13} /> {t("添加提供商", "Add provider")}</button>
        )}
      >
        {SEARCH_PROVIDERS.map((provider) => (
          <SettingsRailRow
            key={provider.kind}
            label={provider.label}
            selected={provider.kind === selectedRow}
            active={providerConfig(settings, provider.kind).enabled}
            onSelect={() => setSelectedRow(provider.kind)}
          />
        ))}
      </SettingsRail>

      <ProviderPane
        entry={searchProviderEntry(selectedRow)}
        config={providerConfig(settings, selectedRow)}
        desktopRuntime={desktopRuntime}
        onChange={updateProvider}
        onFlush={onFlush}
      />
    </div>
  );
}

function ProviderPane({
  entry,
  config,
  desktopRuntime,
  onChange,
  onFlush
}: {
  entry: ReturnType<typeof searchProviderEntry>;
  config: SearchProviderConfig;
  desktopRuntime: boolean;
  onChange: (kind: SearchProviderKind, patch: Partial<SearchProviderConfig>) => void;
  onFlush?: () => Promise<void>;
}) {
  const { t } = useI18n();
  const enginesDraft = useEditDraft(config.engines.join(", "));
  const isSearxng = entry.kind === "searxng";
  const isLocalFetch = entry.kind === "fetch";
  const keyRequired = [entry.search, entry.fetch].some((spec) => spec?.requiresApiKey);
  const secretHelp = desktopRuntime
    ? t(
      "输入后失去焦点会自动保存；清空后失焦即删除。明文存在系统凭据库里，不会写进对话文档，也不随端点变化而失效。",
      "Changes save on blur; clearing the field and blurring deletes it. The secret lives in the system credential store, is never written to conversation documents, and survives an endpoint edit."
    )
    : t(
      "浏览器预览不会发起真实请求，也不会保存凭据明文。",
      "Browser preview does not send real requests or store credentials in plain text."
    );
  const apiCredentials = useSearchCredentialPort(entry.kind, "apiKey");
  const basicAuthCredentials = useSearchCredentialPort(entry.kind, "basicAuthPassword");

  return (
    <div className="provider-pane">
      <header className="provider-pane__header">
        <div className="provider-pane__identity">
          <h1>{entry.label}</h1>
        </div>
        {/* The list-row dot is state; this switch is the only control. */}
        <Switch
          checked={config.enabled}
          onChange={(enabled) => onChange(entry.kind, { enabled })}
          label={t("启用搜索提供商 {name}", "Enable search provider {name}", { name: entry.label })}
        />
      </header>

      <div className="provider-pane__body">
        <div className="provider-pane__stack">
          {isLocalFetch && (
            <section className="provider-field">
              <div className="provider-field__title"><span>{t("本机抓取", "Local fetch")}</span></div>
              <p className="provider-field__help">{t(
                "这一家没有任何配置：它由应用自己去取目标网页并抽出可读正文，不经过第三方服务，因此既没有端点也没有凭据。解析到内网地址的目标会被拒绝。",
                "This one has nothing to configure: the app retrieves the page itself and extracts its readable text, with no third-party service, so it has neither an endpoint nor a credential. Targets that resolve to a private address are refused."
              )}</p>
            </section>
          )}

          {!isLocalFetch && !isSearxng && (
            <SecretField
              identity={`${entry.kind}:apiKey`}
              credentials={apiCredentials}
              label="API Key"
              required={keyRequired}
              help={keyRequired
                ? `${t("没有 API Key 的提供商无法工作。", "A provider without an API key cannot work.")}${secretHelp}`
                : `${t("这一家匿名可用，API Key 只用来抬高配额。", "This one works anonymously; an API key only raises the quota.")}${secretHelp}`}
              onFlush={onFlush}
            />
          )}

          {isSearxng && (
            <>
              <section className="provider-field">
                <div className="provider-field__title"><span>{t("搜索引擎", "Search engines")}</span></div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input provider-input--code"
                      aria-label={t("搜索引擎", "Search engines")}
                      spellCheck={false}
                      value={enginesDraft.value}
                      onChange={(event) => {
                        enginesDraft.onChange(event.target.value);
                        onChange(entry.kind, {
                          engines: event.target.value
                            .split(",")
                            .map((engine) => engine.trim())
                            .filter(Boolean)
                        });
                      }}
                      onBlur={enginesDraft.onBlur}
                      placeholder="google, duckduckgo, brave"
                    />
                  </div>
                </div>
                <p className="provider-field__help">{t(
                  "逗号分隔。留空则读这台实例的 /config，自动挑出 general + web 两个类目下已启用的引擎。",
                  "Comma separated. Leave empty to read the instance's /config and pick the enabled engines in the general + web categories."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("Basic Auth 用户名", "Basic auth username")}</span></div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input provider-input--code"
                      aria-label={t("Basic Auth 用户名", "Basic auth username")}
                      spellCheck={false}
                      value={config.basicAuthUsername}
                      onChange={(event) => onChange(entry.kind, { basicAuthUsername: event.target.value })}
                    />
                  </div>
                </div>
                <p className="provider-field__help">{t(
                  "自托管实例挂在一层 Basic Auth 后面时填。留空则不带鉴权头。",
                  "Fill this in when the self-hosted instance sits behind basic auth. Leave empty to send no auth header."
                )}</p>
              </section>

              <SecretField
                identity={`${entry.kind}:basicAuthPassword`}
                credentials={basicAuthCredentials}
                label={t("Basic Auth 密码", "Basic auth password")}
                required={false}
                help={secretHelp}
                onFlush={onFlush}
              />
            </>
          )}

          {entry.search && entry.search.defaultApiHost && (
            <section className="provider-field">
              <div className="provider-field__title">
                <span>{t("搜索端点", "Search endpoint")}</span>
              </div>
              <div className="provider-field__row">
                <div className="provider-input-group">
                  <input
                    className="provider-input provider-input--code"
                    aria-label={t("搜索端点", "Search endpoint")}
                    type="url"
                    spellCheck={false}
                    value={config.searchApiHost}
                    onChange={(event) => onChange(entry.kind, { searchApiHost: event.target.value })}
                    placeholder={entry.search.defaultApiHost}
                  />
                </div>
              </div>
              <p className="provider-field__help">{t(
                "留空使用目录默认端点。只有回环地址才允许用 http。",
                "Leave empty to use the catalog default. Only loopback addresses may use http."
              )}</p>
            </section>
          )}

          {entry.fetch && entry.fetch.defaultApiHost && (
            <section className="provider-field">
              <div className="provider-field__title">
                <span>{t("抓取端点", "Fetch endpoint")}</span>
              </div>
              <div className="provider-field__row">
                <div className="provider-input-group">
                  <input
                    className="provider-input provider-input--code"
                    aria-label={t("抓取端点", "Fetch endpoint")}
                    type="url"
                    spellCheck={false}
                    value={config.fetchApiHost}
                    onChange={(event) => onChange(entry.kind, { fetchApiHost: event.target.value })}
                    placeholder={entry.fetch.defaultApiHost}
                  />
                </div>
              </div>
              <p className="provider-field__help">{t(
                "抓取与检索是两条独立的能力，可以指向不同主机——Jina 出厂就是 s.jina.ai 与 r.jina.ai 两台。",
                "Fetching and searching are separate capabilities and may point at different hosts — Jina ships with s.jina.ai and r.jina.ai."
              )}</p>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
