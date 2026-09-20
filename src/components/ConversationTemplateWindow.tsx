import { useCallback, useEffect, useRef, useState } from "react";
import { FileText, Lock } from "lucide-react";
import { useI18n } from "../i18n";
import type { ContextItem, ConversationTemplateSummary, ToolDescriptor } from "../types";
import { Dialog } from "./Common";
import { ConversationTemplateEditor } from "./ConversationTemplateEditor";

/** A preset as this window lists it: a name, and the one template it opens with. */
export interface TemplateWindowPreset {
  id: string;
  name: string;
  templateId: string;
}

/** The rail's selection. `own` is the entry at the top; anything else is a preset id. */
const OWN = "own";

/**
 * The window a role opens its conversation template in.
 *
 * It is laid out like the conversation-settings pane on purpose — a rail of
 * names down the left, the thing itself on the right — because it is the same
 * move: pick which of several bodies to look at. What differs is that only one
 * of them is yours. The presets underneath are drawn read-only, so the role's
 * opening history can be written next to the presets it will run beside without
 * this window becoming a second way to edit them; a preset's template is edited
 * on that preset's own page, and nowhere else.
 *
 * A role that has never been given a template selects an empty editable body
 * rather than an empty state: the first message is written the same way every
 * later one is, and the id is minted by whoever takes the save.
 */
export function ConversationTemplateWindow({
  ownTemplateId,
  presets,
  templates,
  tools,
  enabledTools,
  imageInputSupported = false,
  onReadTemplate,
  onSaveOwnTemplate,
  onEnableTools,
  onClose
}: {
  /** Empty until the first save mints one. */
  ownTemplateId: string;
  presets: readonly TemplateWindowPreset[];
  /** Summaries, for the message count each rail entry trails. */
  templates: readonly ConversationTemplateSummary[];
  tools: ToolDescriptor[];
  enabledTools: readonly string[];
  /** Whether the role owning this window runs on a model that reads images. */
  imageInputSupported?: boolean;
  onReadTemplate: (templateId: string) => Promise<ContextItem[]>;
  /** Takes the body and returns the id it was stored under, minting one if needed. */
  onSaveOwnTemplate: (contexts: ContextItem[]) => Promise<string>;
  onEnableTools?: (names: string[]) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [selected, setSelected] = useState<string>(OWN);
  const [bodies, setBodies] = useState<Record<string, ContextItem[]>>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  /* Which ids a read has already been started for. Kept out of state so landing
     a body does not re-run the effect that fetched it. */
  const requested = useRef<Set<string>>(new Set());

  const selectedPreset = presets.find((preset) => preset.id === selected) ?? null;
  const openTemplateId = selected === OWN ? ownTemplateId : selectedPreset?.templateId ?? "";
  /* An owner with no template yet has an empty body, not a missing one: there is
     nothing to read, and the editor should come up ready to take a first message. */
  const openBody = openTemplateId ? bodies[openTemplateId] ?? null : [];

  useEffect(() => {
    if (!openTemplateId || requested.current.has(openTemplateId)) return;
    requested.current.add(openTemplateId);
    let abandoned = false;
    void (async () => {
      try {
        const contexts = await onReadTemplate(openTemplateId);
        if (!abandoned) setBodies((current) => ({ ...current, [openTemplateId]: contexts }));
      } catch (reason) {
        // Let it be retried: a read that failed is not a body we know is empty.
        requested.current.delete(openTemplateId);
        if (!abandoned) {
          setLoadError(reason instanceof Error ? reason.message : String(reason));
        }
      }
    })();
    return () => { abandoned = true; };
  }, [onReadTemplate, openTemplateId]);

  const messageCount = (templateId: string): number | null => {
    if (!templateId) return 0;
    return templates.find((template) => template.id === templateId)?.messageCount ?? 0;
  };

  const saveOwn = useCallback(async (contexts: ContextItem[]) => {
    const savedId = await onSaveOwnTemplate(contexts);
    // Cache under the id it actually landed on, so leaving this entry and coming
    // back shows what was stored rather than re-reading a body we just wrote.
    if (savedId) {
      requested.current.add(savedId);
      setBodies((current) => ({ ...current, [savedId]: contexts }));
    }
  }, [onSaveOwnTemplate]);

  const railEntry = (
    key: string,
    label: string,
    count: number | null,
    locked: boolean
  ) => (
    <button
      type="button"
      key={key}
      aria-current={selected === key || undefined}
      className={selected === key
        ? "settings-nav__item settings-nav__item--active"
        : "settings-nav__item"}
      onClick={() => { setSelected(key); setLoadError(null); }}
    >
      {locked
        ? <Lock size={14} aria-hidden="true" />
        : <FileText size={14} aria-hidden="true" />}
      <span>{label}</span>
      {count === null
        ? null
        : <small className="conversation-settings__nav-count">{count}</small>}
    </button>
  );

  return (
    <Dialog
      title={t("对话模板", "Conversation template")}
      width="1040px"
      bodyClassName="dialog__body--flush"
      onClose={onClose}
    >
      <ConversationTemplateEditor
        key={selected}
        templateId={openTemplateId}
        contexts={loadError ? [] : openBody}
        tools={tools}
        enabledTools={enabledTools}
        editable={selected === OWN}
        imageInputSupported={imageInputSupported}
        onSave={selected === OWN ? saveOwn : undefined}
        onEnableTools={selected === OWN ? onEnableTools : undefined}
        aside={(
          <nav
            className="settings-nav template-window__nav"
            aria-label={t("对话模板分类", "Conversation templates")}
          >
            {railEntry(
              OWN,
              t("当前的对话模板", "This conversation template"),
              messageCount(ownTemplateId),
              false
            )}
            {presets.length > 0 && (
              <hr className="template-window__nav-divider" />
            )}
            {/* A preset's body is drawn read-only, and the padlock is what says
                so: an entry is one line, so there is no second line to explain
                itself on — and the rail is short enough that the one editable
                entry above the rule is unmistakable. */}
            {presets.map((preset) => railEntry(
              preset.id,
              preset.name || t("未命名预设", "Untitled preset"),
              messageCount(preset.templateId),
              true
            ))}
          </nav>
        )}
      />
      {loadError && <p className="field__hint field__hint--error">{loadError}</p>}
    </Dialog>
  );
}
