import { EllipsisVertical } from "lucide-react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { PopoverMenu } from "./PopoverMenu";
import "./PaneToolbar.css";

/** One pane toggle. `pressed` is "the pane is open", not "the surface is busy". */
export interface PaneToolbarButton {
  id: "terminal" | "review" | "preview";
  label: string;
  /** Replaces `label` as tooltip and accessible name while `activity` holds. */
  activeLabel?: string;
  icon: ReactNode;
  pressed: boolean;
  /** Something is happening behind a closed pane; drawn as an indicator dot. */
  activity?: boolean;
  disabled?: boolean;
  /** Why the button is disabled. Takes the tooltip's place so the reason is reachable. */
  title?: string;
  onToggle: () => void;
}

export interface PaneToolbarMenuItem {
  id: "files" | "tasks" | "history" | "settings";
  label: string;
  icon: ReactNode;
  /** Omit for an action that opens something of its own rather than toggling a pane. */
  checked?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}

export interface PaneToolbarProps {
  buttons: PaneToolbarButton[];
  menuItems: PaneToolbarMenuItem[];
  /** Accessible name of the overflow menu itself. Defaults to "视图 / Views". */
  menuLabel?: string;
}

/**
 * The topbar's pane controls: three toggles plus an overflow menu.
 *
 * The toggles carry no text, so their accessible name is the only place the activity state is
 * spelled out — an open pane is already visible on screen, a command still running behind a
 * closed one is not.
 */
export function PaneToolbar({ buttons, menuItems, menuLabel }: PaneToolbarProps) {
  const { t } = useI18n();
  // A trigger that can only ever open a menu of dead rows is itself dead.
  const menuUnavailable = menuItems.every((item) => item.disabled === true);

  return (
    <div className="pane-toolbar">
      {buttons.map((button) => {
        const name = button.activity && button.activeLabel ? button.activeLabel : button.label;
        return (
          <button
            key={button.id}
            type="button"
            data-drag-exclude
            data-pane-toggle={button.id}
            className={`icon-button pane-toolbar__button${button.pressed ? " pane-toolbar__button--pressed" : ""}`}
            aria-label={name}
            aria-pressed={button.pressed}
            title={button.title ?? name}
            disabled={button.disabled}
            onClick={button.onToggle}
          >
            {button.icon}
            {button.activity && <span className="pane-toolbar__indicator" aria-hidden="true" />}
          </button>
        );
      })}
      <PopoverMenu
        rootClassName="pane-toolbar__menu"
        triggerClassName="icon-button pane-toolbar__button"
        trigger={<EllipsisVertical size={18} aria-hidden="true" />}
        triggerLabel={t("更多选项", "More options")}
        menuLabel={menuLabel ?? t("视图", "Views")}
        align="end"
        dense
        disabled={menuUnavailable}
        sections={[{
          id: "views",
          items: menuItems.map((item) => ({
            id: item.id,
            label: item.label,
            icon: item.icon,
            checked: item.checked,
            disabled: item.disabled,
            onSelect: item.onSelect
          }))
        }]}
      />
    </div>
  );
}
