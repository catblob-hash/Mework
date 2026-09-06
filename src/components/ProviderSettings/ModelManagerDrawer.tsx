import { useDeferredValue, useMemo, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronRight, RefreshCw, Search } from "lucide-react";
import { useI18n } from "../../i18n";
import {
  hasUsableBaseUrl,
  modelDisplayName,
  modelGroup,
  normalizeCapabilities
} from "../../lib/modelCapabilities";
import type { ApiProvider, ModelProfile } from "../../types";
import { Drawer } from "./Drawer";
import { CapabilityIcons } from "./capabilityMeta";
import { ProviderAvatar } from "./ProviderAvatar";

/**
 * Model-discovery drawer: what `GET /models` returned, plus the models this
 * provider already carries, grouped by family.
 *
 * Fetching a catalog changes nothing on its own. A row is a toggle: clicking an
 * uninstalled model installs it, clicking an installed one drops it again.
 */
export function ModelManagerDrawer({
  provider,
  discovered,
  discovering,
  discoveryError,
  onDiscover,
  onClose,
  onInstall,
  onRemove
}: {
  provider: ApiProvider;
  /** Results from the most recent `GET /models`; empty before the first fetch. */
  discovered: ModelProfile[];
  discovering: boolean;
  /** Why the most recent `GET /models` failed, or `null` when it did not. */
  discoveryError: string | null;
  onDiscover: () => void;
  onClose: () => void;
  onInstall: (models: ModelProfile[]) => void;
  onRemove: (models: ModelProfile[]) => void;
}) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const deferredQuery = useDeferredValue(query);

  const installedIds = useMemo(
    () => new Set(provider.models.map((model) => model.id)),
    [provider.models]
  );

  /** Installed models and discoveries, deduplicated by ID; installed records take
   * precedence to preserve user curation. */
  const catalog = useMemo(() => {
    const byId = new Map<string, ModelProfile>();
    for (const model of provider.models) byId.set(model.id, model);
    for (const model of discovered) if (!byId.has(model.id)) byId.set(model.id, model);
    return [...byId.values()];
  }, [provider.models, discovered]);

  const visible = useMemo(() => {
    const needle = deferredQuery.trim().toLowerCase();
    if (!needle) return catalog;
    return catalog.filter((model) => model.id.toLowerCase().includes(needle)
      || modelDisplayName(model).toLowerCase().includes(needle));
  }, [catalog, deferredQuery]);

  const groups = useMemo(() => {
    const buckets = new Map<string, ModelProfile[]>();
    for (const model of visible) {
      const group = modelGroup(model) || t("其他", "Other");
      const bucket = buckets.get(group);
      if (bucket) bucket.push(model);
      else buckets.set(group, [model]);
    }
    return [...buckets.entries()].sort((left, right) => left[0].localeCompare(right[0]));
  }, [visible, t]);

  const allVisibleInstalled = visible.length > 0 && visible.every((model) => installedIds.has(model.id));

  return (
    <Drawer
      title={t("发现模型", "Discover models")}
      subtitle={t(
        "{installed} 个已装 · 目录共 {total} 个 · 点一行加入或移出这家提供商",
        "{installed} installed · {total} in the catalog · click a row to add it to or remove it from this provider",
        { installed: provider.models.length, total: catalog.length }
      )}
      labelledBy="model-manager-title"
      width="wide"
      onClose={onClose}
      footer={
        <button
          type="button"
          className="button button--secondary button--small"
          disabled={discovering || !hasUsableBaseUrl(provider)}
          onClick={onDiscover}
        >
          <RefreshCw size={13} className={discovering ? "spin" : ""} /> {t("重新拉取", "Fetch again")}
        </button>
      }
    >
      <div className="model-manager__toolbar">
        <div className="provider-search-box">
          <Search size={13} />
          <input
            aria-label={t("搜索模型", "Search models")}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape" && query) {
                event.stopPropagation();
                setQuery("");
              }
            }}
            placeholder={t("按模型 ID 或名称搜索", "Search by model ID or name")}
          />
        </div>
        <button
          type="button"
          className="button button--secondary button--small"
          disabled={!visible.length}
          onClick={() => (allVisibleInstalled ? onRemove(visible) : onInstall(visible))}
        >
          {allVisibleInstalled
            ? t("移除全部结果", "Remove all results")
            : t("添加全部结果", "Add all results")}
        </button>
      </div>

      <div className="model-manager__list">
        {discoveryError && !discovering && (
          <div className="model-manager__error" role="alert">
            <AlertTriangle size={14} />
            <span>
              <strong>{t("拉取模型列表失败", "Fetching the model list failed")}</strong>
              <small>{discoveryError}</small>
            </span>
          </div>
        )}
        {groups.map(([group, models]) => {
          const isCollapsed = collapsed.has(group);
          const groupInstalled = models.every((model) => installedIds.has(model.id));
          return (
            <section className="model-manager__group" key={group}>
              <div className="model-manager__group-head">
                <button
                  type="button"
                  className="model-manager__group-toggle"
                  aria-expanded={!isCollapsed}
                  onClick={() => setCollapsed((current) => {
                    const next = new Set(current);
                    if (next.has(group)) next.delete(group);
                    else next.add(group);
                    return next;
                  })}
                >
                  {isCollapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
                  <strong>{group}</strong>
                  <em>{models.length}</em>
                </button>
                <button
                  type="button"
                  className="button button--ghost button--small"
                  onClick={(event) => {
                    // Prevent the bulk action from toggling the group disclosure.
                    event.stopPropagation();
                    if (groupInstalled) onRemove(models);
                    else onInstall(models);
                  }}
                >
                  {groupInstalled ? t("整组移除", "Remove group") : t("整组添加", "Add group")}
                </button>
              </div>
              {!isCollapsed && models.map((model) => {
                const installed = installedIds.has(model.id);
                return (
                  <button
                    type="button"
                    className={`model-manager__row${installed ? " model-manager__row--installed" : ""}`}
                    key={model.id}
                    data-model-id={model.id}
                    aria-pressed={installed}
                    // A name distinct from the model list's remove button: a
                    // global role query would otherwise match both.
                    aria-label={installed
                      ? t("从提供商移除 {id}", "Remove {id} from the provider", { id: model.id })
                      : t("添加到提供商 {id}", "Add {id} to the provider", { id: model.id })}
                    onClick={() => (installed ? onRemove([model]) : onInstall([model]))}
                  >
                    <ProviderAvatar name={modelDisplayName(model)} className="provider-avatar--round" />
                    <span className="model-manager__row-main">
                      <strong>{model.id}</strong>
                      {modelDisplayName(model) !== model.id && <small>{modelDisplayName(model)}</small>}
                    </span>
                    <CapabilityIcons capabilities={normalizeCapabilities(model.capabilities)} />
                  </button>
                );
              })}
            </section>
          );
        })}
        {!groups.length && (
          <div className="provider-empty">
            {discovering ? <RefreshCw size={18} className="spin" /> : <Search size={18} />}
            <strong>{discovering
              ? t("正在拉取模型列表", "Fetching the model list")
              : discoveryError
                ? t("没有拿到模型目录", "No catalog was returned")
                : query.trim()
                  ? t("没有匹配的模型", "No matching model")
                  : t("这家上游还没有目录", "No catalog for this upstream yet")}</strong>
            <span>{discovering
              ? t("宿主正在向该提供商请求 GET /models。", "The host is calling GET /models on this provider.")
              : discoveryError
                ? t("上面写着这次请求失败的原因；也可以直接手动添加模型。", "The reason for the failed request is above; you can also add models manually.")
                : query.trim()
                  ? t("换个关键词，或者重新拉取一次。", "Try another keyword, or fetch again.")
                  : t("重新拉取一次，或者直接手动添加模型。", "Fetch again, or add models manually.")}</span>
          </div>
        )}
      </div>
    </Drawer>
  );
}
