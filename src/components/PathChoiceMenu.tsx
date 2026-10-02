import { LoaderCircle } from "lucide-react";
import { useSyncExternalStore } from "react";
import { useI18n } from "../i18n";
import { ContextMenu } from "./ContextMenu";
import type { ContextMenuAnchor, ContextMenuSection } from "./ContextMenu";

/**
 * The menu a path the transcript names opens at the link when it is in more
 * than one of the conversation's workspaces, or on more than one machine.
 *
 * Its state is a store of its own rather than the app's: the menu is the whole
 * answer to a click, and putting it in the app's state would make every open
 * and every update re-render the entire app before the menu could appear.
 */
interface PathChoice {
  anchor: ContextMenuAnchor;
  /** Empty while the lookup is still out: the menu says it is looking. */
  sections: ContextMenuSection[];
}

let current: PathChoice | null = null;
const listeners = new Set<() => void>();

function publish(next: PathChoice | null): void {
  current = next;
  for (const listener of listeners) listener();
}

/** Opens the menu at `anchor`, or replaces what the open one lists. */
export function showPathChoice(anchor: ContextMenuAnchor, sections: ContextMenuSection[]): void {
  publish({ anchor, sections });
}

export function closePathChoice(): void {
  if (current !== null) publish(null);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function PathChoiceMenu() {
  const { t } = useI18n();
  const choice = useSyncExternalStore(subscribe, () => current, () => null);
  if (!choice) return null;
  return (
    <ContextMenu
      anchor={choice.anchor}
      sections={choice.sections}
      label={t("打开位置", "Open location")}
      placeholder={<><LoaderCircle size={12} className="spin" aria-hidden="true" />{t("正在查找…", "Looking…")}</>}
      onClose={closePathChoice}
    />
  );
}
