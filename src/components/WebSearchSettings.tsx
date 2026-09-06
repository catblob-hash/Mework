import {
  Eye,
  EyeOff,
  Plus,
  RefreshCw,
  SlidersHorizontal
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
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
  SearchCompressionMethod,
  SearchProviderConfig,
  SearchProviderKind,
  WebSearchAssets
} from "../types";
import { IconButton, Switch } from "./Common";
import { SettingsRail, SettingsRailRow } from "./SettingsRail";

type WebSearchAssetsChange = WebSearchAssets | ((current: WebSearchAssets) => WebSearchAssets);

interface WebSearchSettingsProps {
  settings: WebSearchAssets;
  onChange: (change: WebSearchAssetsChange) => void;
  onFlush?: () => Promise<void>;
}

/** The first rail row contains global controls that belong to no provider. */
const GENERAL_ROW = "__general__";

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

/**
 * A single credential input.
 *
 * Providers may have multiple secrets. Each field independently handles masking,
 * reveal, blur persistence, and deletion on empty input.
 */
function SecretField({
  providerKind,
  slot,
  label,
  help,
  required,
  onFlush
}: {
  providerKind: SearchProviderKind;
  slot: SearchCredentialSlot;
  label: string;
  help: string;
  required: boolean;
  onFlush?: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState<string | null>(null);
  const [keyLength, setKeyLength] = useState(0);
  const [saving, setSaving] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [visible, setVisible] = useState(false);
  const mountedRef = useRef(true);
  const statusTokenRef = useRef(0);
  const saveTokenRef = useRef(0);
  const savedDraftRef = useRef<string | null>(null);
  const visibleRef = useRef(false);
  visibleRef.current = visible;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      statusTokenRef.current += 1;
    };
  }, []);

  // Refresh credential status when the slot changes. Credential identity excludes endpoints.
  useEffect(() => {
    const token = statusTokenRef.current + 1;
    statusTokenRef.current = token;
    setDraft(null);
    setVisible(false);
    savedDraftRef.current = null;
    void getSearchKeyStatus(providerKind, slot).then((status) => {
      if (!mountedRef.current || statusTokenRef.current !== token) return;
      setKeyLength(status.configured ? status.keyLength ?? 0 : 0);
    }).catch(() => {
      // A status-read failure is handled by explicit credential operations.
    });
    return () => {
      if (statusTokenRef.current === token) statusTokenRef.current += 1;
    };
  }, [providerKind, slot]);

  const persist = useCallback(async (): Promise<boolean> => {
    const secret = draft?.trim() ?? "";
    if (!secret || savedDraftRef.current === secret) return true;
    const token = saveTokenRef.current + 1;
    saveTokenRef.current = token;
    setSaving(true);
    try {
      await onFlush?.();
      const status = await saveSearchApiKey(providerKind, slot, secret);
      if (!mountedRef.current || saveTokenRef.current !== token) return false;
      setKeyLength(status.keyLength ?? Array.from(secret).length);
      if (visibleRef.current) savedDraftRef.current = secret;
      else {
        savedDraftRef.current = null;
        setDraft(null);
      }
      return true;
    } catch {
      return false;
    } finally {
      if (mountedRef.current && saveTokenRef.current === token) setSaving(false);
    }
  }, [draft, onFlush, providerKind, slot]);

  const removeStored = useCallback(async () => {
    const token = saveTokenRef.current + 1;
    saveTokenRef.current = token;
    setSaving(true);
    try {
      await onFlush?.();
      await deleteSearchApiKey(providerKind, slot);
      if (!mountedRef.current || saveTokenRef.current !== token) return;
      savedDraftRef.current = null;
      setDraft(null);
      setKeyLength(0);
    } catch {
      // Preserve the stored credential on deletion failure so blur can retry.
    } finally {
      if (mountedRef.current && saveTokenRef.current === token) setSaving(false);
    }
  }, [onFlush, providerKind, slot]);

  const toggleVisibility = useCallback(async () => {
    if (visibleRef.current) {
      if (!(await persist())) return;
      setVisible(false);
      setDraft(null);
      savedDraftRef.current = null;
      return;
    }
    if (draft !== null) {
      setVisible(true);
      return;
    }
    setRevealing(true);
    try {
      await onFlush?.();
      // Query configuration before revealing the secret; parsing backend copy would
      // couple this path to localized error text.
      const status = await getSearchKeyStatus(providerKind, slot);
      if (!mountedRef.current) return;
      if (!status.configured) {
        setDraft("");
        setVisible(true);
        return;
      }
      const secret = await revealSearchApiKey(providerKind, slot);
      if (!mountedRef.current) return;
      setDraft(secret);
      setKeyLength(Array.from(secret).length);
      savedDraftRef.current = secret.trim();
      setVisible(true);
    } catch {
      // Keep the field masked after a read failure so the user can replace it.
    } finally {
      if (mountedRef.current) setRevealing(false);
    }
  }, [draft, onFlush, persist, providerKind, slot]);

  const mask = "•".repeat(keyLength);
  const busyLabel = revealing
    ? t("正在读取{name}", "Reading {name}", { name: label })
    : visible
      ? t("隐藏{name}", "Hide {name}", { name: label })
      : t("显示{name}", "Show {name}", { name: label });

  return (
    <section className="provider-field">
      <div className="provider-field__title">
        <span>{required ? t("{name}（必填）", "{name} (required)", { name: label }) : label}</span>
      </div>
      <div className="provider-field__row">
        <div className="provider-input-group">
          <input
            className="provider-input provider-input--code"
            type={visible ? "text" : "password"}
            aria-label={label}
            autoComplete="new-password"
            value={draft ?? mask}
            onFocus={(event) => {
              if (draft === null && mask) event.currentTarget.select();
            }}
            onClick={(event) => {
              if (draft === null && mask) event.currentTarget.select();
            }}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => {
              if (draft !== null && !draft.trim()) {
                void removeStored();
                return;
              }
              void persist();
            }}
            aria-busy={saving}
            placeholder={t("输入{name}", "Enter {name}", { name: label })}
          />
          <IconButton
            label={busyLabel}
            className="provider-input__reveal"
            disabled={revealing || saving}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => void toggleVisibility()}
          >{revealing
            ? <RefreshCw size={12} className="spin" />
            : visible ? <Eye size={12} /> : <EyeOff size={12} />}</IconButton>
        </div>
      </div>
      <p className="provider-field__help">{help}</p>
    </section>
  );
}

/**
 * Search provider settings.
 *
 * The shared provider layout has a catalog rail and a detail pane. Global search
 * controls occupy the rail's first row because they apply to no individual provider.
 */
export function WebSearchSettings({ settings, onChange, onFlush }: WebSearchSettingsProps) {
  const { t } = useI18n();
  const [selectedRow, setSelectedRow] = useState<string>(GENERAL_ROW);
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
  const updateAssets = (patch: Partial<WebSearchAssets>) =>
    onChange((current) => ({ ...current, ...patch }));

  const fetchCapable = SEARCH_PROVIDERS.filter((provider) => provider.fetch);
  const maxResultsDraft = useEditDraft(String(settings.maxResults));
  const cutoffDraft = useEditDraft(String(settings.compression.cutoffLimit));
  const blacklistDraft = useEditDraft(settings.excludeDomains.join("\n"));

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
        <SettingsRailRow
          label={t("通用", "General")}
          selected={selectedRow === GENERAL_ROW}
          onSelect={() => setSelectedRow(GENERAL_ROW)}
        />
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

      {selectedRow === GENERAL_ROW ? (
        <div className="provider-pane">
          <header className="provider-pane__header">
            <div className="provider-pane__identity">
              <h1>{t("通用", "General")}</h1>
            </div>
            <SlidersHorizontal size={14} />
          </header>
          <div className="provider-pane__body">
            <div className="provider-pane__stack">
              <section className="provider-field">
                <div className="provider-field__title"><span>{t("结果数", "Result count")}</span></div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input"
                      type="number"
                      min={1}
                      max={50}
                      aria-label={t("结果数", "Result count")}
                      value={maxResultsDraft.value}
                      onChange={(event) => {
                        maxResultsDraft.onChange(event.target.value);
                        const value = Number(event.target.value);
                        if (Number.isSafeInteger(value) && value >= 1 && value <= 50) {
                          updateAssets({ maxResults: value });
                        }
                      }}
                      onBlur={maxResultsDraft.onBlur}
                    />
                  </div>
                </div>
                <p className="provider-field__help">{t(
                  "一次检索最多要回几条结果。只对目录提供商生效；原生后端的检索深度由那个模型自己决定。",
                  "How many results one search asks for. Only catalog providers read it; a native backend decides its own search depth."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("抓取提供商", "Fetch provider")}</span></div>
                <select
                  className="input"
                  aria-label={t("抓取提供商", "Fetch provider")}
                  value={settings.fetchProvider ?? ""}
                  onChange={(event) => updateAssets({
                    fetchProvider: (event.target.value || null) as SearchProviderKind | null
                  })}
                >
                  <option value="">{t("未选择", "None selected")}</option>
                  {fetchCapable.map((provider) => {
                    const enabled = providerConfig(settings, provider.kind).enabled;
                    return (
                      <option key={provider.kind} value={provider.kind}>
                        {enabled
                          ? provider.label
                          : t("{name}（未启用）", "{name} (disabled)", { name: provider.label })}
                      </option>
                    );
                  })}
                </select>
                <p className="provider-field__help">{t(
                  "抓取网页正文（web_fetch）用哪一家。它没有「原生」这条腿——本仓库说的两种线协议都没有把「抓这一页」暴露成服务端工具，所以它是一个全局选择而不是随对话走的。",
                  "Which provider fetches page text (web_fetch). It has no native leg — neither wire protocol here exposes \"retrieve this page\" as a server-side tool — so it is a global choice rather than a per-conversation one."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("域名黑名单", "Blocked domains")}</span></div>
                <div className="provider-input-group provider-input-group--multiline">
                  <textarea
                    className="provider-input provider-textarea"
                    aria-label={t("域名黑名单", "Blocked domains")}
                    rows={5}
                    spellCheck={false}
                    placeholder={"*://ads.example/*\n/\\/(login|signup)$/"}
                    value={blacklistDraft.value}
                    onChange={(event) => {
                      blacklistDraft.onChange(event.target.value);
                      updateAssets({
                        excludeDomains: event.target.value
                          .split("\n")
                          .map((rule) => rule.trim())
                          .filter(Boolean)
                      });
                    }}
                    onBlur={blacklistDraft.onBlur}
                  />
                </div>
                <p className="provider-field__help">{t(
                  "一行一条。支持 <all_urls>、scheme://host/path 匹配模式（* 通配，*. 匹配子域），以及前后加斜杠的正则。写错的规则会被忽略而不是让整次检索失败。",
                  "One rule per line: <all_urls>, a scheme://host/path match pattern (* wildcards, *. matches subdomains), or a /regex/. A malformed rule is ignored rather than failing the whole search."
                )}</p>
              </section>

              <section className="provider-field">
                <div className="provider-field__title"><span>{t("结果压缩", "Result compression")}</span></div>
                <select
                  className="input"
                  aria-label={t("结果压缩", "Result compression")}
                  value={settings.compression.method}
                  onChange={(event) => updateAssets({
                    compression: {
                      ...settings.compression,
                      method: event.target.value as SearchCompressionMethod
                    }
                  })}
                >
                  <option value="cutoff">{t("按预算截断", "Truncate to a budget")}</option>
                  <option value="none">{t("不压缩", "None")}</option>
                </select>
                {settings.compression.method === "cutoff" && (
                  <div className="provider-field__row">
                    <div className="provider-input-group">
                      <input
                        className="provider-input"
                        type="number"
                        min={1}
                        max={200000}
                        aria-label={t("截断预算（token）", "Truncation budget (tokens)")}
                        value={cutoffDraft.value}
                        onChange={(event) => {
                          cutoffDraft.onChange(event.target.value);
                          const value = Number(event.target.value);
                          if (Number.isSafeInteger(value) && value >= 1 && value <= 200_000) {
                            updateAssets({
                              compression: { ...settings.compression, cutoffLimit: value }
                            });
                          }
                        }}
                        onBlur={cutoffDraft.onBlur}
                      />
                    </div>
                  </div>
                )}
                <p className="provider-field__help">{t(
                  "预算是整次调用的 token 数，逐条平分后截断。关掉它，上游给多长正文就进多长——一次抓取可能因此占掉一整个上下文窗口。",
                  "The budget is a whole-call token count, split evenly across results. Turn it off and whatever the upstream returns goes in verbatim — one fetch can then take a whole context window."
                )}</p>
              </section>
            </div>
          </div>
        </div>
      ) : (
        <ProviderPane
          entry={searchProviderEntry(selectedRow as SearchProviderKind)}
          config={providerConfig(settings, selectedRow as SearchProviderKind)}
          desktopRuntime={desktopRuntime}
          onChange={updateProvider}
          onFlush={onFlush}
        />
      )}
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
              providerKind={entry.kind}
              slot="apiKey"
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
                providerKind={entry.kind}
                slot="basicAuthPassword"
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
