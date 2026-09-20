import { X } from "lucide-react";
import { useI18n } from "../i18n";
import type { SelectedElement } from "../lib/browser";
import { selectedElementExcerpt, selectedElementLabel } from "../lib/selectedElement";
import { IconButton } from "./Common";
import "./SelectedElementChips.css";

/**
 * The elements the user picked out of a page, waiting to be sent.
 *
 * A chip stands for a block of prompt text the message will carry but the composer never shows:
 * the label is what the user pointed at, not what the model will read.
 */
export function SelectedElementChips({
  elements,
  onRemove
}: {
  elements: readonly SelectedElement[];
  onRemove: (sequence: number) => void;
}) {
  const { t } = useI18n();
  if (!elements.length) return null;
  return (
    <div
      className="selected-element-chips"
      role="list"
      aria-label={t("已选择的页面元素", "Selected page elements")}
    >
      {elements.map((element) => {
        const label = selectedElementLabel(element);
        const excerpt = selectedElementExcerpt(element);
        return (
          <span className="selected-element-chip" role="listitem" key={element.sequence}>
            <code className="selected-element-chip__label">{label}</code>
            {excerpt && <span className="selected-element-chip__text">{excerpt}</span>}
            <IconButton
              className="selected-element-chip__remove"
              label={t("移除 {name}", "Remove {name}", { name: label })}
              onClick={() => onRemove(element.sequence)}
            >
              <X size={11} aria-hidden="true" />
            </IconButton>
          </span>
        );
      })}
    </div>
  );
}
