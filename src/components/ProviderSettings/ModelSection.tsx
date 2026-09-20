import { useDeferredValue, useMemo, useRef, useState } from "react";
import {
  Bot,
  ChevronRight,
  ChevronsDownUp,
  ChevronsUpDown,
  Minus,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  X
} from "lucide-react";
import { useI18n } from "../../i18n";
import {
  hasUsableBaseUrl,
  modelDisplayName,
  modelGroup,
  normalizeCapabilities
} from "../../lib/modelCapabilities";
import type { ApiProvider, ModelProfile } from "../../types";
import { IconButton } from "../Common";
import { CapabilityIcons } from "./capabilityMeta";
import { ProviderAvatar } from "./ProviderAvatar";

/**
 * Model section in the right pane.
 *
 * Every listed model is an enabled model, so rows expose properties and removal
 * only. Search and bulk expand/collapse stay behind header icons to avoid an
 * unnecessary filter bar for small providers.
 */
export function ModelSection({
  provider,
  busy,
  discovering,
  onManage,
  onAddModel,
  onEditModel,
  onRemoveModel
}: {
  provider: ApiProvider;
  busy: boolean;
  discovering: boolean;
  onManage: () => void;
  onAddModel: () => void;
  onEditModel: (model: ModelProfile) => void;
  onRemoveModel: (modelId: string) => void;
}) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const searchRef = useRef<HTMLInputElement>(null);
  const deferredQuery = useDeferredValue(query);
  const hasModels = provider.models.length > 0;
  const searchExpanded = searchOpen || query !== "";

  const groups = useMemo(() => {
    const needle = deferredQuery.trim().toLowerCase();
    const buckets = new Map<string, ModelProfile[]>();
    for (const model of provider.models) {
      if (needle
        && !model.id.toLowerCase().includes(needle)
        && !modelDisplayName(model).toLowerCase().includes(needle)) continue;
      const group = modelGroup(model) || t("其他", "Other");
      const bucket = buckets.get(group);
      if (bucket) bucket.push(model);
      else buckets.set(group, [model]);
    }
    return [...buckets.entries()].sort((left, right) => left[0].localeCompare(right[0]));
  }, [provider.models, deferredQuery, t]);

  // Force matching groups open while searching so results remain visible.
  const searching = deferredQuery.trim() !== "";
  const allExpanded = collapsed.size === 0;

  const toggleAll = () => {
    setCollapsed(allExpanded ? new Set(groups.map(([group]) => group)) : new Set());
  };

  return (
    <section className="provider-models">
      <div className="provider-models__head">
        <div className="provider-models__title">
          <h2>{t("模型", "Models")}</h2>
          <IconButton
            label={allExpanded ? t("全部收起", "Collapse all") : t("全部展开", "Expand all")}
            className="provider-models__tool"
            disabled={!hasModels}
            onClick={toggleAll}
          >{allExpanded ? <ChevronsDownUp size={13} /> : <ChevronsUpDown size={13} />}</IconButton>
          {searchExpanded ? (
            <div className="provider-search-box provider-search-box--compact">
              <Search size={13} />
              <input
                ref={searchRef}
                // biome-ignore lint/a11y/noAutofocus: The input opened by the search icon must receive focus.
                autoFocus
                aria-label={t("搜索模型", "Search models")}
                value={query}
                placeholder={t("搜索模型", "Search models")}
                onChange={(event) => setQuery(event.target.value)}
                onBlur={() => {
                  if (!query) setSearchOpen(false);
                }}
                onKeyDown={(event) => {
                  if (event.key !== "Escape") return;
                  event.stopPropagation();
                  setQuery("");
                  setSearchOpen(false);
                }}
              />
              {query && (
                <IconButton
                  label={t("清空搜索", "Clear search")}
                  className="provider-search-box__clear"
                  onClick={() => {
                    setQuery("");
                    setSearchOpen(false);
                  }}
                ><X size={11} /></IconButton>
              )}
            </div>
          ) : (
            <IconButton
              label={t("搜索模型", "Search models")}
              className="provider-models__tool"
              onClick={() => setSearchOpen(true)}
            ><Search size={13} /></IconButton>
          )}
        </div>
        <div className="provider-button-group">
          <button
            type="button"
            className="provider-button-group__item"
            disabled={busy || !hasUsableBaseUrl(provider)}
            onClick={onManage}
          >
            <RefreshCw size={12} className={discovering ? "spin" : ""} /> {t("拉取模型", "Fetch models")}
          </button>
          <button
            type="button"
            className="provider-button-group__item provider-button-group__item--icon"
            aria-label={t("手动添加模型", "Add a model manually")}
            title={t("手动添加模型", "Add a model manually")}
            onClick={onAddModel}
          ><Plus size={13} /></button>
        </div>
      </div>

      <div className="provider-models__list">
        {groups.map(([group, models]) => {
          const open = searching || !collapsed.has(group);
          return (
            <div className={open ? "model-group model-group--open" : "model-group"} key={group}>
              {/* biome-ignore lint/a11y/useSemanticElements: The header contains a nested group-removal button, so its container cannot be a button. */}
              <div
                className="model-group__head"
                role="button"
                tabIndex={0}
                aria-expanded={open}
                onClick={() => setCollapsed((current) => {
                  const next = new Set(current);
                  if (next.has(group)) next.delete(group);
                  else next.add(group);
                  return next;
                })}
                onKeyDown={(event) => {
                  if (event.currentTarget !== event.target) return;
                  if (event.key !== "Enter" && event.key !== " ") return;
                  event.preventDefault();
                  setCollapsed((current) => {
                    const next = new Set(current);
                    if (next.has(group)) next.delete(group);
                    else next.add(group);
                    return next;
                  });
                }}
              >
                <ChevronRight size={13} className={open ? "model-group__chevron model-group__chevron--open" : "model-group__chevron"} />
                <span className="model-group__title">{group}</span>
                <IconButton
                  label={t("移除 {group} 分组下的全部模型", "Remove every model in {group}", { group })}
                  className="model-group__remove"
                  onClick={(event) => {
                    event.stopPropagation();
                    for (const model of models) onRemoveModel(model.id);
                  }}
                ><Minus size={13} /></IconButton>
              </div>
              {open && (
                <div className="model-group__body">
                  {models.map((model) => (
                    <div className="model-row" key={model.id}>
                      <ProviderAvatar name={modelDisplayName(model)} className="provider-avatar--round" />
                      <span className="model-row__name" title={model.id}>{modelDisplayName(model)}</span>
                      <span className="model-row__meta">
                        <CapabilityIcons capabilities={normalizeCapabilities(model.capabilities)} />
                      </span>
                      <IconButton
                        label={t("模型 {name} 的属性", "Properties for model {name}", { name: model.id })}
                        className="model-row__action"
                        onClick={() => onEditModel(model)}
                      ><Settings2 size={13} /></IconButton>
                      <IconButton
                        label={t("移除模型 {name}", "Remove model {name}", { name: model.id })}
                        className="model-row__action icon-button--danger"
                        onClick={() => onRemoveModel(model.id)}
                      ><Minus size={13} /></IconButton>
                    </div>
                  ))}
                </div>
              )}
            </div>
          );
        })}
        {!hasModels && (
          <div className="provider-empty provider-empty--dashed">
            <Bot size={20} />
            <strong>{t("还没有模型", "No models yet")}</strong>
            <span>{t("用上面的「拉取模型」看看这家上游有哪些模型，或者手动加一个。", "Use “Fetch models” above to see what this upstream offers, or add one manually.")}</span>
          </div>
        )}
        {hasModels && !groups.length && (
          <div className="provider-empty provider-empty--dashed">
            <Search size={20} />
            <strong>{t("没有匹配的模型", "No matching model")}</strong>
            <span>{t("换个关键词试试。", "Try another keyword.")}</span>
          </div>
        )}
      </div>
    </section>
  );
}
