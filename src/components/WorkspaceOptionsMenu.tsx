import { MessageSquareText, MoreHorizontal } from "lucide-react";
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
  /** The active preset ID; an empty or unresolvable ID displays "Follow the most recent settings". */
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
      menuWidth={252}
      align="end"
      sections={[{
        id: "workspace",
        items: [{
          id: "default-preset",
          label: t("设置默认对话预设", "Set the default conversation preset"),
          description: selected
            ? selected.name || t("未命名预设", "Untitled preset")
            : t("跟随最近一次设置", "Follow the most recent settings"),
          icon: <MessageSquareText size={14} />,
          children: [
            {
              id: "",
              label: t("跟随最近一次设置", "Follow the most recent settings"),
              description: t(
                "本工作区「+」沿用它最近一次的对话设置",
                "This workspace's + reuses its most recent conversation settings"
              ),
              checked: !selected,
              onSelect: () => onSelectPreset("")
            },
            ...presets.map((preset) => ({
              id: preset.id,
              label: preset.name || t("未命名预设", "Untitled preset"),
              checked: preset.id === selected?.id,
              onSelect: () => onSelectPreset(preset.id)
            }))
          ]
        }]
      }]}
    />
  );
}
