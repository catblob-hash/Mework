import {
  Cloud,
  Eye,
  EyeOff,
  Plus,
  RefreshCw,
  Settings2
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { createId } from "../../lib/id";
import { hasBackendRuntime } from "../../lib/backend";
import { isBuiltinProvider, isCodexProvider } from "../../lib/codexProvider";
import { isClaudeAgentProvider } from "../../lib/claudeAgentProvider";
import {
  normalizeCapabilities,
  normalizeReasoningContent,
  repairActiveModelId,
  hasUsableBaseUrl
} from "../../lib/modelCapabilities";
import {
  deleteApiKey,
  fetchModels,
  forgetStoredApiKeyLength,
  getStoredApiKeyLength,
  revealApiKey,
  saveApiKey
} from "../../lib/runtime";
import type {
  ProviderFamily,
  ApiProvider,
  GlobalSettings as GlobalSettingsType,
  ModelProfile
} from "../../types";
import { IconButton, Switch } from "../Common";
import { findReorderDropTarget, reorderItems, usePointerDrag } from "../usePointerDrag";
import type { ReorderDropTarget } from "../usePointerDrag";
import { AddProviderDialog } from "./AddProviderDialog";
import { ClaudeAgentLoginPanel } from "./ClaudeAgentLoginPanel";
import { CodexLoginPanel } from "./CodexLoginPanel";
import { API_FORMAT_OPTIONS, familyLabel, chatRequestPreview } from "./endpointMeta";
import { ModelManagerDrawer } from "./ModelManagerDrawer";
import { ModelProfileDrawer } from "./ModelProfileDrawer";
import { ModelSection } from "./ModelSection";
import { ProviderOptionsDrawer } from "./ProviderOptionsDrawer";
import { ProviderRail } from "./ProviderRail";
import type { RailFilter } from "./ProviderRail";
import "./ProviderSettings.css";

type GlobalSettingsChange = GlobalSettingsType | ((current: GlobalSettingsType) => GlobalSettingsType);
type GlobalSettingsChangeHandler = (change: GlobalSettingsChange) => void;

function definedProperties<T extends object>(value: T): Partial<T> {
  return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined)) as Partial<T>;
}

/**
 * Returns properties the user has curated. Empty values are not curated.
 *
 * `definedProperties` excludes only `undefined`; empty legacy names, groups, and
 * capabilities must yield to discovered metadata.
 */
function curatedProperties(model: ModelProfile): Partial<ModelProfile> {
  const curated = definedProperties(model);
  if (!model.name.trim()) delete curated.name;
  if (!model.group.trim()) delete curated.group;
  if (!model.capabilities.length) delete curated.capabilities;
  return curated;
}

/**
 * Merges discovery results into the installed list without overwriting attributes
 * the user has already curated.
 *
 * Fetching a catalog no longer absorbs it: this runs when the user installs a
 * model from the discovery drawer, usually one at a time.
 */
export function mergeModelProfiles(existing: ModelProfile[], discovered: ModelProfile[]): ModelProfile[] {
  const merged = existing.map((model) => ({ ...model }));
  const positions = new Map(merged.map((model, index) => [model.id.trim(), index]));
  for (const incoming of discovered) {
    const id = incoming.id?.trim();
    if (!id) continue;
    const normalized: ModelProfile = {
      ...incoming,
      id,
      capabilities: normalizeCapabilities(incoming.capabilities ?? [])
    };
    const index = positions.get(id);
    if (index === undefined) {
      positions.set(id, merged.length);
      merged.push(normalized);
      continue;
    }
    const current = merged[index];
    // User-curated values take precedence over discovery.
    merged[index] = { ...normalized, ...curatedProperties(current) } as ModelProfile;
  }
  return merged;
}

/** Creates an empty user-defined provider. */
function createProvider(name: string, family: ProviderFamily): ApiProvider {
  return {
    id: createId("provider"),
    name,
    enabled: true,
    family,
    baseUrl: API_FORMAT_OPTIONS.find((option) => option.value === family)?.defaultBaseUrl ?? "",
    familySettings: {},
    endpointBaseUrls: {},
    notes: "",
    models: [],
    activeModelId: null,
  };
}

/**
 * Human-readable reason for a failed credential transaction, with the secret itself
 * scrubbed: host errors may echo the request, and the field is on screen.
 */
function keyErrorReason(error: unknown, secret?: string): string {
  const message = (error instanceof Error ? error.message : String(error)).trim();
  const readable = message || "unknown error";
  return secret ? readable.split(secret).join("***") : readable;
}

/**
 * Creates an empty user-defined model. Its reasoning form starts at the
 * protocol's own default, because the stored value is always concrete.
 */
function createModel(family: ProviderFamily): ModelProfile {
  return {
    id: "",
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: normalizeReasoningContent(undefined, family)
  };
}

type ModelEditorState = {
  mode: "create" | "edit";
  providerId: string;
  originalId: string | null;
  draft: ModelProfile;
};

function providerEndpointFingerprint(
  provider: Pick<ApiProvider, "id" | "family" | "baseUrl">
): string {
  return `${provider.id}\u0000${provider.family}\u0000${provider.baseUrl.trim()}`;
}

/**
 * Select the active provider first, then any enabled provider, then the first
 * provider in the list.
 */
function preferredProviderId(providers: ApiProvider[], activeProviderId: string | null): string | null {
  return providers.find((provider) => provider.id === activeProviderId)?.id
    ?? providers.find((provider) => provider.enabled)?.id
    ?? providers[0]?.id
    ?? null;
}

export function ApiProviderSettings({
  settings,
  onChange,
  onFlush
}: {
  settings: GlobalSettingsType;
  onChange: GlobalSettingsChangeHandler;
  onFlush?: () => Promise<void>;
}) {
  const { t } = useI18n();
  const providers = settings.apiProviders ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(
    () => preferredProviderId(providers, settings.activeProviderId)
  );
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [keyLengths, setKeyLengths] = useState<Record<string, number>>({});
  const [savingKeyIds, setSavingKeyIds] = useState<Set<string>>(() => new Set());
  const [revealingKeyIds, setRevealingKeyIds] = useState<Set<string>>(() => new Set());
  const [visibleKeys, setVisibleKeys] = useState<Set<string>>(() => new Set());
  const [modelEditor, setModelEditor] = useState<ModelEditorState | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [providerQuery, setProviderQuery] = useState("");
  const [railFilter, setRailFilter] = useState<RailFilter>("all");
  const [addingProvider, setAddingProvider] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [modelManagerOpen, setModelManagerOpen] = useState(false);
  /** Latest `GET /models` results by provider ID, used by the model manager drawer. */
  const [discoveredModels, setDiscoveredModels] = useState<Record<string, ModelProfile[]>>({});
  /**
   * Why the most recent `GET /models` failed, by provider ID. A discarded error
   * left a third-party relay indistinguishable from a provider with no models,
   * so the host's diagnosis has to reach the drawer.
   */
  const [discoveryErrors, setDiscoveryErrors] = useState<Record<string, string>>({});
  /**
   * Why the most recent credential transaction failed, by provider ID. Saving,
   * deleting, and reading all used to swallow their rejection and return false, so a
   * refused save looked exactly like a successful one.
   */
  const [keyErrors, setKeyErrors] = useState<Record<string, string>>({});
  const mountedRef = useRef(true);
  const selectedIdRef = useRef(selectedId);
  const settingsRef = useRef(settings);
  const actionTokenRef = useRef(0);
  const keyLengthTokenRef = useRef(0);
  const keySaveTokensRef = useRef(new Map<string, number>());
  const savedKeyDraftsRef = useRef(new Map<string, string>());
  const visibleKeysRef = useRef(visibleKeys);
  const selected = providers.find((provider) => provider.id === selectedId) ?? providers[0] ?? null;
  const desktopRuntime = hasBackendRuntime();
  const providerSort = usePointerDrag<string, ReorderDropTarget>({
    getTarget: (point, providerId) => findReorderDropTarget("api-providers", providerId, point),
    onDrop: (providerId, target) => {
      onChange((current) => ({
        ...current,
        apiProviders: reorderItems(current.apiProviders, providerId, target.id, target.position, (provider) => provider.id)
      }));
    }
  });

  const moveProviderByKeyboard = (providerId: string, direction: -1 | 1) => {
    const index = providers.findIndex((provider) => provider.id === providerId);
    const target = providers[index + direction];
    if (!target) return;
    onChange((current) => ({
      ...current,
      apiProviders: reorderItems(current.apiProviders, providerId, target.id, direction < 0 ? "before" : "after", (provider) => provider.id)
    }));
  };

  settingsRef.current = settings;
  visibleKeysRef.current = visibleKeys;

  useEffect(() => {
    selectedIdRef.current = selectedId;
  }, [selectedId]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      actionTokenRef.current += 1;
      keyLengthTokenRef.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!providers.some((provider) => provider.id === selectedId)) {
      setSelectedId(preferredProviderId(providers, settingsRef.current.activeProviderId));
    }
  }, [providers, selectedId]);

  // Close secondary panels when switching providers so their contents cannot edit the previous provider.
  useEffect(() => {
    setModelEditor(null);
    setOptionsOpen(false);
    setModelManagerOpen(false);
  }, [selected?.id]);

  // Key-length metadata belongs to provider identity; a provider edit must not reload it.
  // biome-ignore lint/correctness/useExhaustiveDependencies: only selection identity changes this lookup.
  useEffect(() => {
    if (!selected || isBuiltinProvider(selected)) return;
    const provider = selected;
    const token = keyLengthTokenRef.current + 1;
    keyLengthTokenRef.current = token;
    void getStoredApiKeyLength(provider.id).then((keyLength) => {
      if (!mountedRef.current || keyLengthTokenRef.current !== token || selectedIdRef.current !== provider.id) return;
      setKeyLengths((current) => {
        const next = { ...current };
        if (keyLength) next[provider.id] = keyLength;
        else delete next[provider.id];
        return next;
      });
    });
    return () => {
      if (keyLengthTokenRef.current === token) keyLengthTokenRef.current += 1;
    };
  }, [selected?.id]);

  const replaceProvider = (
    providerId: string,
    updater: (provider: ApiProvider) => ApiProvider,
    renameActiveModel?: { from: string; to: string | null }
  ) => {
    onChange((currentSettings) => {
      const current = currentSettings.apiProviders.find((provider) => provider.id === providerId);
      if (!current) return currentSettings;
      let next = updater(current);
      if (renameActiveModel && next.activeModelId === renameActiveModel.from) {
        next = { ...next, activeModelId: renameActiveModel.to };
      }
      const repaired = repairActiveModelId(next);
      if (repaired !== next.activeModelId) {
        next = { ...next, activeModelId: repaired };
      }
      return {
        ...currentSettings,
        apiProviders: currentSettings.apiProviders.map((provider) => provider.id === providerId ? next : provider)
      };
    });
  };

  const setProviderEnabled = (providerId: string, enabled: boolean) => {
    onChange((current) => {
      const apiProviders = current.apiProviders.map((provider) => provider.id === providerId
        ? { ...provider, enabled }
        : provider);
      const activeProviderId = apiProviders.some((provider) => (
        provider.id === current.activeProviderId
        && provider.enabled
      ))
        ? current.activeProviderId
        : apiProviders.find((provider) => provider.enabled)?.id ?? null;
      return { ...current, apiProviders, activeProviderId };
    });
  };

  const addProvider = (provider: ApiProvider) => {
    onChange((current) => ({
      ...current,
      apiProviders: [...current.apiProviders, provider],
      activeProviderId: current.activeProviderId ?? provider.id
    }));
    setAddingProvider(false);
    selectProvider(provider.id);
  };

  const addCustomProvider = (draft: { name: string; family: ProviderFamily }) => {
    addProvider(createProvider(draft.name, draft.family));
  };

  const replaceSelectedModels = (update: (models: ModelProfile[]) => ModelProfile[]) => {
    if (!selected) return;
    replaceProvider(selected.id, (provider) => {
      const models = update(provider.models);
      return { ...provider, models };
    });
  };

  const removeModel = (providerId: string, modelId: string) => {
    replaceProvider(providerId, (provider) => ({
      ...provider,
      models: provider.models.filter((model) => model.id !== modelId)
    }));
  };

  /** Installs models the user picked in the discovery drawer, keeping curated values. */
  const installModels = (models: ModelProfile[]) => {
    replaceSelectedModels((current) => mergeModelProfiles(current, models));
  };

  const uninstallModels = (models: ModelProfile[]) => {
    const drop = new Set(models.map((model) => model.id));
    replaceSelectedModels((current) => current.filter((model) => !drop.has(model.id)));
  };

  const beginAsyncAction = (action: string) => {
    const token = actionTokenRef.current + 1;
    actionTokenRef.current = token;
    setBusyAction(action);
    return token;
  };

  const invalidateAsyncAction = () => {
    actionTokenRef.current += 1;
    setBusyAction(null);
  };

  const selectProvider = (providerId: string | null) => {
    if (selectedIdRef.current === providerId) return;
    const previousProviderId = selectedIdRef.current;
    invalidateAsyncAction();
    keyLengthTokenRef.current += 1;
    if (previousProviderId) {
      savedKeyDraftsRef.current.delete(previousProviderId);
      const nextVisibleKeys = new Set(visibleKeysRef.current);
      nextVisibleKeys.delete(previousProviderId);
      visibleKeysRef.current = nextVisibleKeys;
      setKeyDrafts((current) => {
        const next = { ...current };
        delete next[previousProviderId];
        return next;
      });
      setVisibleKeys((current) => {
        const next = new Set(current);
        next.delete(previousProviderId);
        return next;
      });
    }
    selectedIdRef.current = providerId;
    setSelectedId(providerId);
  };

  /** Completes an asynchronous action, discarding stale or unmounted results. */
  const finishAsyncAction = (token: number) => {
    if (!mountedRef.current || actionTokenRef.current !== token) return;
    setBusyAction(null);
  };

  const removeProvider = (providerId: string) => {
    const removed = providers.find((provider) => provider.id === providerId);
    if (!removed || isBuiltinProvider(removed)) return;
    const nextSelectedId = preferredProviderId(
      providers.filter((provider) => provider.id !== providerId),
      settings.activeProviderId
    );
    invalidateAsyncAction();
    keyLengthTokenRef.current += 1;
    keySaveTokensRef.current.set(providerId, (keySaveTokensRef.current.get(providerId) ?? 0) + 1);
    savedKeyDraftsRef.current.delete(providerId);
    void forgetStoredApiKeyLength(providerId);
    onChange((current) => {
      const remaining = current.apiProviders.filter((provider) => provider.id !== providerId);
      const nextActive = current.activeProviderId === providerId
        ? remaining.find((provider) => provider.enabled) ?? null
        : remaining.find((provider) => (
            provider.id === current.activeProviderId && provider.enabled
          )) ?? null;
      return {
        ...current,
        apiProviders: remaining,
        activeProviderId: nextActive?.id ?? null
      };
    });
    selectedIdRef.current = nextSelectedId;
    setSelectedId(nextSelectedId);
    setKeyDrafts((current) => {
      const next = { ...current };
      delete next[providerId];
      return next;
    });
    setKeyLengths((current) => {
      const next = { ...current };
      delete next[providerId];
      return next;
    });
    setVisibleKeys((current) => {
      const next = new Set(current);
      next.delete(providerId);
      return next;
    });
    setSavingKeyIds((current) => {
      const next = new Set(current);
      next.delete(providerId);
      return next;
    });
    setRevealingKeyIds((current) => {
      const next = new Set(current);
      next.delete(providerId);
      return next;
    });
  };

  /**
   * Commits a credential-transaction failure under the same guards the success path
   * uses, so a late rejection cannot describe a provider the user already left.
   */
  const recordKeyError = (providerId: string, saveToken: number, message: string) => {
    if (!mountedRef.current || keySaveTokensRef.current.get(providerId) !== saveToken) return;
    setKeyErrors((current) => ({ ...current, [providerId]: message }));
  };

  const clearKeyError = (providerId: string) => {
    setKeyErrors((current) => {
      if (!(providerId in current)) return current;
      const next = { ...current };
      delete next[providerId];
      return next;
    });
  };

  const persistKey = async (providerId: string): Promise<boolean> => {
    const secret = keyDrafts[providerId]?.trim() ?? "";
    if (!secret || savedKeyDraftsRef.current.get(providerId) === secret) return true;
    const saveToken = (keySaveTokensRef.current.get(providerId) ?? 0) + 1;
    keySaveTokensRef.current.set(providerId, saveToken);
    keyLengthTokenRef.current += 1;
    setSavingKeyIds((current) => new Set(current).add(providerId));
    try {
      // Persist the provider before storing its key: credential names derive from
      // its ID, so an unsaved provider would create an unreadable orphan.
      await onFlush?.();
      const providerSnapshot = settingsRef.current.apiProviders.find((provider) => provider.id === providerId);
      if (!providerSnapshot) {
        recordKeyError(providerId, saveToken, t(
          "保存 API Key 失败：这个提供商已经不在设置里了。",
          "Could not save the API key: this provider is no longer in settings."
        ));
        return false;
      }
      const status = await saveApiKey(providerSnapshot, secret);
      if (!mountedRef.current || keySaveTokensRef.current.get(providerId) !== saveToken) return false;
      setKeyLengths((current) => ({
        ...current,
        [providerId]: status.keyLength ?? Array.from(secret).length
      }));
      if (visibleKeysRef.current.has(providerId)) {
        savedKeyDraftsRef.current.set(providerId, secret);
      } else {
        savedKeyDraftsRef.current.delete(providerId);
        setKeyDrafts((current) => {
          const next = { ...current };
          delete next[providerId];
          return next;
        });
      }
      clearKeyError(providerId);
      return true;
    } catch (error) {
      // The draft stays untouched so the user can retry, and no success metadata is
      // written: a rejected save must not look like a stored key.
      recordKeyError(providerId, saveToken, t(
        "保存 API Key 失败：{reason}",
        "Could not save the API key: {reason}",
        { reason: keyErrorReason(error, secret) }
      ));
      return false;
    } finally {
      if (mountedRef.current && keySaveTokensRef.current.get(providerId) === saveToken) {
        setSavingKeyIds((current) => {
          const next = new Set(current);
          next.delete(providerId);
          return next;
        });
      }
    }
  };

  const removeStoredKey = async (providerId: string): Promise<boolean> => {
    const saveToken = (keySaveTokensRef.current.get(providerId) ?? 0) + 1;
    keySaveTokensRef.current.set(providerId, saveToken);
    setSavingKeyIds((current) => new Set(current).add(providerId));
    try {
      await onFlush?.();
      const providerSnapshot = settingsRef.current.apiProviders.find((provider) => provider.id === providerId);
      if (!providerSnapshot) {
        recordKeyError(providerId, saveToken, t(
          "删除 API Key 失败：这个提供商已经不在设置里了。",
          "Could not delete the API key: this provider is no longer in settings."
        ));
        return false;
      }
      await deleteApiKey(providerSnapshot);
      if (!mountedRef.current || keySaveTokensRef.current.get(providerId) !== saveToken) return false;
      savedKeyDraftsRef.current.delete(providerId);
      setKeyDrafts((current) => {
        const next = { ...current };
        delete next[providerId];
        return next;
      });
      setKeyLengths((current) => {
        const next = { ...current };
        delete next[providerId];
        return next;
      });
      clearKeyError(providerId);
      return true;
    } catch (error) {
      recordKeyError(providerId, saveToken, t(
        "删除 API Key 失败：{reason}",
        "Could not delete the API key: {reason}",
        { reason: keyErrorReason(error) }
      ));
      if (mountedRef.current && keySaveTokensRef.current.get(providerId) === saveToken) {
        setKeyDrafts((current) => {
          const next = { ...current };
          delete next[providerId];
          return next;
        });
      }
      return false;
    } finally {
      if (mountedRef.current && keySaveTokensRef.current.get(providerId) === saveToken) {
        setSavingKeyIds((current) => {
          const next = new Set(current);
          next.delete(providerId);
          return next;
        });
      }
    }
  };

  const toggleKeyVisibility = async (providerId: string) => {
    if (visibleKeysRef.current.has(providerId)) {
      const saved = await persistKey(providerId);
      if (!saved) return;
      setVisibleKeys((current) => {
        const next = new Set(current);
        next.delete(providerId);
        return next;
      });
      setKeyDrafts((current) => {
        const next = { ...current };
        delete next[providerId];
        return next;
      });
      savedKeyDraftsRef.current.delete(providerId);
      return;
    }

    if (Object.hasOwn(keyDrafts, providerId)) {
      setVisibleKeys((current) => new Set(current).add(providerId));
      return;
    }

    setRevealingKeyIds((current) => new Set(current).add(providerId));
    try {
      await onFlush?.();
      const providerSnapshot = settingsRef.current.apiProviders.find((provider) => provider.id === providerId);
      if (!providerSnapshot) return;
      const secret = await revealApiKey(providerSnapshot);
      if (!mountedRef.current || selectedIdRef.current !== providerId) return;
      setKeyDrafts((current) => ({ ...current, [providerId]: secret }));
      setKeyLengths((current) => ({ ...current, [providerId]: Array.from(secret).length }));
      savedKeyDraftsRef.current.set(providerId, secret.trim());
      setVisibleKeys((current) => new Set(current).add(providerId));
      clearKeyError(providerId);
    } catch (error) {
      if (mountedRef.current && selectedIdRef.current === providerId) {
        const message = error instanceof Error ? error.message : String(error);
        // A missing key or browser preview does not make the field unusable; leave
        // it editable and empty. Any other read error is reported, because a silent
        // one leaves the field showing a mask the user cannot explain.
        if (/未配置|浏览器预览不会保留/.test(message)) { // i18n-audit-ignore: classifies legacy backend errors
          setKeyDrafts((current) => ({ ...current, [providerId]: "" }));
          setVisibleKeys((current) => new Set(current).add(providerId));
        } else {
          setKeyErrors((current) => ({
            ...current,
            [providerId]: t(
              "读取 API Key 失败：{reason}",
              "Could not read the API key: {reason}",
              { reason: keyErrorReason(error) }
            )
          }));
        }
      }
    } finally {
      if (mountedRef.current) {
        setRevealingKeyIds((current) => {
          const next = new Set(current);
          next.delete(providerId);
          return next;
        });
      }
    }
  };

  const discoverModels = async () => {
    if (!selected) return;
    const providerId = selected.id;
    const token = beginAsyncAction("models");
    try {
      await onFlush?.();
      if (!mountedRef.current || actionTokenRef.current !== token || selectedIdRef.current !== providerId) return;
      const providerSnapshot = settingsRef.current.apiProviders.find((provider) => provider.id === providerId);
      if (!providerSnapshot) return;
      const endpointFingerprint = providerEndpointFingerprint(providerSnapshot);
      const discovered = await fetchModels(providerSnapshot);
      const latestProvider = settingsRef.current.apiProviders.find((provider) => provider.id === providerId);
      if (
        !mountedRef.current
        || actionTokenRef.current !== token
        || selectedIdRef.current !== providerId
        || !latestProvider
        || providerEndpointFingerprint(latestProvider) !== endpointFingerprint
      ) return;
      // Discovery only fills the catalog. Nothing joins the provider's own model
      // list until the user clicks a row in the drawer.
      setDiscoveredModels((current) => ({ ...current, [providerId]: discovered }));
      setDiscoveryErrors((current) => {
        if (!(providerId in current)) return current;
        const next = { ...current };
        delete next[providerId];
        return next;
      });
    } catch (error) {
      // Keep the reason: a relay that answers 401/404/redirect is otherwise
      // reported as "no matching model", which points at the wrong problem.
      if (mountedRef.current && actionTokenRef.current === token && selectedIdRef.current === providerId) {
        const message = error instanceof Error ? error.message : String(error);
        setDiscoveryErrors((current) => ({ ...current, [providerId]: message }));
      }
    } finally {
      finishAsyncAction(token);
    }
  };

  /**
   * Opens the discovery drawer and refreshes the catalog, which may have changed
   * since it was last opened.
   */
  const openModelManager = () => {
    setModelManagerOpen(true);
    if (!selected || busyAction !== null || !hasUsableBaseUrl(selected)) return;
    void discoverModels();
  };

  const modelEditorProvider = modelEditor
    ? providers.find((provider) => provider.id === modelEditor.providerId) ?? null
    : null;
  const normalizedDraftId = modelEditor?.draft.id.trim() ?? "";
  const modelIdError = modelEditor
    ? !normalizedDraftId
      ? t("模型 ID 不能为空", "Model ID is required")
      : modelEditorProvider?.models.some((model) => (
          model.id.trim() === normalizedDraftId && model.id !== modelEditor.originalId
        ))
        ? t("模型 ID 不能与同一提供商中的其他模型重复", "Model ID must be unique within this provider")
        : null
    : null;

  const saveModelDraft = () => {
    if (!modelEditor || !modelEditorProvider || modelIdError) return;
    const saved: ModelProfile = { ...modelEditor.draft, id: normalizedDraftId };
    if (modelEditor.mode === "create") {
      replaceProvider(modelEditor.providerId, (provider) => ({
        ...provider,
        models: [...provider.models, saved],
        activeModelId: provider.activeModelId ?? saved.id
      }));
    } else if (modelEditor.originalId) {
      replaceProvider(
        modelEditor.providerId,
        (provider) => ({
          ...provider,
          models: provider.models.map((model) => model.id === modelEditor.originalId ? saved : model)
        }),
        { from: modelEditor.originalId, to: saved.id }
      );
    }
    setModelEditor(null);
  };

  const hasSelectedKeyDraft = selected
    ? Object.hasOwn(keyDrafts, selected.id)
    : false;
  const selectedKeyMask = selected ? "•".repeat(keyLengths[selected.id] ?? 0) : "";
  /** Why the selected provider's last credential transaction failed. */
  const selectedKeyError = selected ? keyErrors[selected.id] : undefined;
  const keyErrorId = "provider-api-key-error";
  const selectedName = selected
    ? selected.name.trim() || t("未命名提供商", "Untitled provider")
    : "";
  const requestPreview = selected ? chatRequestPreview(selected.baseUrl, selected.family) : "";

  /** Searches provider IDs and names as well as each provider's model IDs and names. */
  const visibleProviders = (() => {
    const needle = providerQuery.trim().toLowerCase();
    return providers.filter((provider) => {
      if (railFilter === "enabled" && !provider.enabled) return false;
      if (railFilter === "disabled" && provider.enabled) return false;
      if (!needle) return true;
      return provider.id.toLowerCase().includes(needle)
        || provider.name.toLowerCase().includes(needle)
        || provider.models.some((model) => (
          model.id.toLowerCase().includes(needle) || model.name.toLowerCase().includes(needle)
        ));
    });
  })();
  // Filtering changes list indexes, so dragging must be disabled while searching or filtering.
  const reorderable = providerQuery.trim() === "" && railFilter === "all";

  return (
    <div className="settings-editor-page api-provider-page provider-settings-page settings-rail-page">
      <ProviderRail
        providers={visibleProviders}
        totalCount={providers.length}
        selectedId={selected?.id ?? null}
        query={providerQuery}
        onQueryChange={setProviderQuery}
        filter={railFilter}
        onFilterChange={setRailFilter}
        reorderable={reorderable}
        sort={providerSort}
        onSelect={selectProvider}
        onMoveByKeyboard={moveProviderByKeyboard}
        onAdd={() => setAddingProvider(true)}
        onDelete={removeProvider}
        mutationDisabled={busyAction !== null || (selected ? savingKeyIds.has(selected.id) : false)}
      />

      {selected ? (
        <div className="provider-pane">
          <header className="provider-pane__header">
            <div className="provider-pane__identity">
              <h1>{selectedName}</h1>
              <IconButton
                label={t("提供商设置", "Provider settings")}
                className="provider-pane__options"
                onClick={() => setOptionsOpen(true)}
              ><Settings2 size={14} /></IconButton>
            </div>
            <Switch
              checked={selected.enabled}
              onChange={(enabled) => setProviderEnabled(selected.id, enabled)}
              label={t("{name} 启用状态", "{name} enabled state", { name: selectedName })}
            />
          </header>

          <div className="provider-pane__body">
            <div className="provider-pane__stack">
              {isCodexProvider(selected) ? (
                <CodexLoginPanel
                  provider={selected}
                  desktopRuntime={desktopRuntime}
                  onSignedInChange={(signedIn) => setProviderEnabled(selected.id, signedIn)}
                  onBeforeHostCall={onFlush}
                />
              ) : isClaudeAgentProvider(selected) ? (
                <ClaudeAgentLoginPanel
                  provider={selected}
                  desktopRuntime={desktopRuntime}
                  onSignedInChange={(signedIn) => setProviderEnabled(selected.id, signedIn)}
                  onBeforeHostCall={onFlush}
                />
              ) : (
              <section className="provider-field">
                <div className="provider-field__title">
                  <span>API Key</span>
                </div>
                <div className="provider-field__row">
                  <div className={`provider-input-group${selectedKeyError ? " provider-input-group--error" : ""}`}>
                    <input
                      className="provider-input provider-input--code"
                      type={visibleKeys.has(selected.id) ? "text" : "password"}
                      aria-label="API Key"
                      aria-invalid={selectedKeyError ? true : undefined}
                      aria-describedby={selectedKeyError ? keyErrorId : undefined}
                      autoComplete="new-password"
                      value={hasSelectedKeyDraft ? keyDrafts[selected.id] : selectedKeyMask}
                      onFocus={(event) => {
                        if (!hasSelectedKeyDraft && selectedKeyMask) event.currentTarget.select();
                      }}
                      onClick={(event) => {
                        if (!hasSelectedKeyDraft && selectedKeyMask) event.currentTarget.select();
                      }}
                      onChange={(event) => setKeyDrafts((current) => ({ ...current, [selected.id]: event.target.value }))}
                      onBlur={() => {
                        if (hasSelectedKeyDraft && !(keyDrafts[selected.id]?.trim())) {
                          void removeStoredKey(selected.id);
                          return;
                        }
                        void persistKey(selected.id);
                      }}
                      aria-busy={savingKeyIds.has(selected.id)}
                      placeholder={t("输入 API Key", "Enter API key")}
                    />
                    <IconButton
                      label={revealingKeyIds.has(selected.id)
                        ? t("正在读取 API Key", "Reading API key")
                        : visibleKeys.has(selected.id)
                          ? t("隐藏 API Key", "Hide API key")
                          : t("显示 API Key", "Show API key")}
                      className="provider-input__reveal"
                      disabled={revealingKeyIds.has(selected.id) || savingKeyIds.has(selected.id)}
                      onMouseDown={(event) => event.preventDefault()}
                      onClick={() => void toggleKeyVisibility(selected.id)}
                    >{revealingKeyIds.has(selected.id)
                      ? <RefreshCw size={12} className="spin" />
                      : visibleKeys.has(selected.id) ? <Eye size={12} /> : <EyeOff size={12} />}</IconButton>
                  </div>
                </div>
                {selectedKeyError && (
                  <p className="provider-field__error" id={keyErrorId} role="alert">{selectedKeyError}</p>
                )}
                <p className="provider-field__help">{desktopRuntime
                  ? t(
                    "输入后失去焦点会自动保存。明文存在系统凭据库里，不会写进对话文档。",
                    "Saved when the field loses focus. The secret lives in the system credential store and is never written to conversation documents."
                  )
                  : t(
                    "浏览器预览不会发起真实请求，也不会保存 API Key 明文。",
                    "Browser preview does not send real requests or store API keys in plain text."
                  )}</p>
              </section>
              )}

              {/* Both built-in families own their endpoint: Codex's belongs to the
                  ChatGPT backend and Claude Agent's is picked by the local CLI.
                  Codex keeps an overridable address for test stubs, in the drawer. */}
              {!isBuiltinProvider(selected) && (
              <section className="provider-field">
                <div className="provider-field__title">
                  <span>{t("API 地址", "API address")}</span>
                  <button
                    type="button"
                    className="provider-field__link"
                    onClick={() => setOptionsOpen(true)}
                  >{t("更多端点", "More endpoints")}</button>
                </div>
                <div className="provider-field__row">
                  <div className="provider-input-group">
                    <input
                      className="provider-input provider-input--code"
                      aria-label="Base URL"
                      type="url"
                      spellCheck={false}
                      value={selected.baseUrl}
                      onChange={(event) => {
                        const baseUrl = event.target.value;
                        replaceProvider(selected.id, (provider) => ({ ...provider, baseUrl }));
                      }}
                      placeholder="https://api.openai.com/v1"
                    />
                  </div>
                  <IconButton
                    label={t("端点与协议设置", "Endpoint and protocol settings")}
                    className="provider-square-action"
                    onClick={() => setOptionsOpen(true)}
                  ><Settings2 size={14} /></IconButton>
                </div>
                <p className="provider-field__help provider-field__help--code">
                  {requestPreview
                    ? t("对话将请求 {url}", "Chat requests go to {url}", { url: requestPreview })
                    : t("尚未配置 Base URL（{format}）", "Base URL not configured ({format})", {
                      format: familyLabel(selected.family)
                    })}
                </p>
              </section>
              )}

              <ModelSection
                key={selected.id}
                provider={selected}
                busy={busyAction !== null}
                discovering={busyAction === "models"}
                onManage={openModelManager}
                onAddModel={() => setModelEditor({
                  mode: "create",
                  providerId: selected.id,
                  originalId: null,
                  draft: createModel(selected.family)
                })}
                onEditModel={(model) => setModelEditor({
                  mode: "edit",
                  providerId: selected.id,
                  originalId: model.id,
                  draft: { ...model }
                })}
                onRemoveModel={(modelId) => removeModel(selected.id, modelId)}
              />
            </div>
          </div>
        </div>
      ) : (
        <div className="provider-pane provider-pane--empty">
          <Cloud size={24} />
          <strong>{t("添加 API 提供商", "Add an API provider")}</strong>
          <span>{t("配置一个用于对话模型的 API 提供商。", "Configure an API provider for chat models.")}</span>
          <button type="button" className="button button--secondary button--small" onClick={() => setAddingProvider(true)}>
            <Plus size={13} /> {t("添加提供商", "Add provider")}
          </button>
        </div>
      )}

      {addingProvider && (
        <AddProviderDialog
          onClose={() => setAddingProvider(false)}
          onCreate={addCustomProvider}
        />
      )}
      {optionsOpen && selected && (
        <ProviderOptionsDrawer
          provider={selected}
          onChange={(update) => replaceProvider(selected.id, update)}
          onClose={() => setOptionsOpen(false)}
        />
      )}
      {modelManagerOpen && selected && (
        <ModelManagerDrawer
          provider={selected}
          discovered={discoveredModels[selected.id] ?? []}
          discovering={busyAction === "models"}
          discoveryError={discoveryErrors[selected.id] ?? null}
          onDiscover={discoverModels}
          onClose={() => setModelManagerOpen(false)}
          onInstall={installModels}
          onRemove={uninstallModels}
        />
      )}
      {modelEditor && modelEditorProvider && (
        <ModelProfileDrawer
          providerName={modelEditorProvider.name}
          family={modelEditorProvider.family}
          mode={modelEditor.mode}
          draft={modelEditor.draft}
          idError={modelIdError}
          onChange={(draft) => setModelEditor((current) => current ? { ...current, draft } : current)}
          onClose={() => setModelEditor(null)}
          onSave={saveModelDraft}
        />
      )}
    </div>
  );
}
