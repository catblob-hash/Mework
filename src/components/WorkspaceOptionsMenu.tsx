import { MoreHorizontal } from "lucide-react";
import { useI18n } from "../i18n";
import { PopoverMenu } from "./PopoverMenu";

/** A conversation preset eligible as a workspace default. The sidebar needs only its identity and display name. */
export interface WorkspacePresetOption {
  id: string;
  name: string;
}

export function WorkspaceOptionsMenu({
  workspaceName,
  presets,
  selectedPresetId,
  disabled = false,
  onSelectPreset
}: {
  workspaceName: string;
  presets: WorkspacePresetOption[];
  /** The active preset ID; an empty or unresolvable ID follows the most recent settings instead. */
  selectedPresetId: string;
  disabled?: boolean;
  onSelectPreset: (presetId: string) => void;
}) {
  const { t } = useI18n();
  const selected = presets.find((preset) => preset.id === selectedPresetId) ?? null;
  const label = t("{name} 的更多选项", "More options for {name}", { name: workspaceName });

  return (
    <PopoverMenu
      rootClassName="workspace-options"
      triggerClassName="icon-button"
      trigger={<MoreHorizontal size={15} />}
      triggerLabel={label}
      disabled={disabled}
      menuLabel={label}
      dense
      anchorToPointer
      sections={[{
        id: "workspace",
        items: [{
          id: "default-preset",
          label: t("默认对话预设", "Default conversation preset"),
          // Nothing to choose from means nothing to follow; the workspace keeps reusing its most recent settings.
          disabled: presets.length === 0,
          children: presets.map((preset) => ({
            id: preset.id,
            label: preset.name || t("未命名预设", "Untitled preset"),
            checked: preset.id === selected?.id,
            // Selecting the current choice clears it, which is how a workspace goes
            // back to following its most recent settings.
            onSelect: () => onSelectPreset(preset.id === selected?.id ? "" : preset.id)
          }))
        }]
      }]}
    />
  );
}
