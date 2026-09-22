import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import type {
  AppDocument,
  GlobalSettings as GlobalSettingsType,
  SettingsView
} from "../types";
import { ApiProviderSettings } from "./ProviderSettings";
import { GlobalSettingsNavigation } from "./GlobalSettingsNavigation";
import { AppearanceSettings } from "./AppearanceSettings";
import { DependencySettings } from "./DependencySettings";
import { ShortcutSettings } from "./ShortcutSettings";
import { UsageSettings } from "./UsageSettings";
import { UpdateSettings } from "./UpdateSettings";
import { DecisionProviderSettings } from "./DecisionProviderSettings";
import { WebSearchSettings } from "./WebSearchSettings";

type GlobalSettingsChange = GlobalSettingsType | ((current: GlobalSettingsType) => GlobalSettingsType);
type GlobalSettingsChangeHandler = (change: GlobalSettingsChange) => void;

interface GlobalSettingsProps {
  initialView: SettingsView;
  settings: GlobalSettingsType;
  /** Read only by the usage page, to backfill turns recorded before the ledger existed. */
  document?: AppDocument | null;
  onChange: GlobalSettingsChangeHandler;
  onFlush?: () => Promise<void>;
  onViewChange?: (view: SettingsView) => void;
}

export function GlobalSettings({
  initialView,
  settings,
  document = null,
  onChange,
  onFlush,
  onViewChange = () => undefined
}: GlobalSettingsProps) {
  const { t } = useI18n();
  // Retired view IDs remain valid navigation entry points and redirect to active pages.
  const redirectedView = (nextView: SettingsView): SettingsView => nextView === "web_search"
    ? "search_providers"
    : nextView === "advanced" || nextView === "memory" || nextView === "general"
      ? "appearance"
      : nextView === "hooks" || nextView === "capability_catalog" || nextView === "agents"
          || nextView === "conversation_presets" || nextView === "mcp" || nextView === "skills"
        ? "providers"
        : nextView;
  const [view, setView] = useState<SettingsView>(() => redirectedView(initialView));
  useEffect(() => {
    setView(redirectedView(initialView));
  }, [initialView]);
  const selectView = (nextView: SettingsView) => {
    const redirected = redirectedView(nextView);
    setView(redirected);
    onViewChange(redirected);
  };

  return (
    <section className="global-settings-layout" aria-label={t("全局设置", "Global settings")}>
        <GlobalSettingsNavigation view={view} onSelect={selectView} />
        <div className="global-settings-content">
          {view === "appearance" && <AppearanceSettings settings={settings} onChange={onChange} />}
          {view === "providers" && <ApiProviderSettings settings={settings} onChange={onChange} onFlush={onFlush} />}
          {view === "search_providers" && (
            <WebSearchSettings
              settings={settings.webSearch}
              onFlush={onFlush}
              onChange={(change) => onChange((current) => ({
                ...current,
                webSearch: typeof change === "function" ? change(current.webSearch) : change
              }))}
            />
          )}
          {view === "decision_providers" && <DecisionProviderSettings onFlush={onFlush} />}
          {view === "shortcuts" && (
            <ShortcutSettings
              shortcuts={settings.shortcuts}
              onChange={(shortcuts) => onChange((current) => ({ ...current, shortcuts }))}
            />
          )}
          {view === "usage" && <UsageSettings document={document} />}
          {view === "dependencies" && (
            <DependencySettings
              tools={settings.environmentTools}
              onChange={(environmentTools) => onChange((current) => ({ ...current, environmentTools }))}
            />
          )}
          {view === "updates" && <UpdateSettings />}
        </div>
    </section>
  );
}
