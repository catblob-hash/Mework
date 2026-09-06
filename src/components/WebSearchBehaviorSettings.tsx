import type { ConversationWebSearchSettings, WebSearchAssets } from "../types";
import { SearchProviderField } from "./SearchProviderField";

interface WebSearchBehaviorSettingsProps {
  value: ConversationWebSearchSettings;
  onChange: (patch: Partial<ConversationWebSearchSettings>) => void;
  webSearchAssets: WebSearchAssets;
}

/**
 * Search backend selection for conversations and presets.
 *
 * `SearchProviderField` writes `ConversationWebSearchSettings.provider`. A
 * conversation has no parent to inherit from, so `null` is not representable.
 */
export function WebSearchBehaviorSettings({
  value,
  onChange,
  webSearchAssets
}: WebSearchBehaviorSettingsProps) {
  return <SearchProviderField
    value={value.provider}
    onChange={(provider) => {
      // `inheritOption` is disabled, so `null` cannot reach this callback.
      if (provider) onChange({ provider });
    }}
    webSearchAssets={webSearchAssets}
  />;
}
