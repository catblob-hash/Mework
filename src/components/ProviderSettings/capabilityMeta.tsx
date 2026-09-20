import { Eye } from "lucide-react";
import { useI18n } from "../../i18n";
import type { TranslationFunction } from "../../i18n";
import type { ModelCapability } from "../../types";

/**
 * Labels and icons for capability chips.
 *
 * These functions only localize and select icons; omit a `default` case so the
 * compiler catches added capabilities.
 */
export function capabilityLabel(t: TranslationFunction, capability: ModelCapability): string {
  switch (capability) {
    case "image_recognition": return t("视觉输入", "Vision");
  }
}

export function capabilityIcon(capability: ModelCapability, size = 13) {
  switch (capability) {
    case "image_recognition": return <Eye size={size} />;
  }
}

/**
 * Icon-only capability chips.
 *
 * Model and drawer rows need names, capabilities, and action buttons. Icons with
 * `title` and `aria-label` fit without displacing names.
 */
export function CapabilityIcons({ capabilities }: { capabilities: ModelCapability[] }) {
  const { t } = useI18n();
  if (!capabilities.length) return null;
  return (
    <span className="model-tags">
      {capabilities.map((capability) => {
        const label = capabilityLabel(t, capability);
        return (
          <span className="model-tag" key={capability} title={label} aria-label={label} role="img">
            {capabilityIcon(capability, 11)}
          </span>
        );
      })}
    </span>
  );
}
