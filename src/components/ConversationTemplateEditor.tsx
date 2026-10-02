import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { createId } from "../lib/id";
import { messageAttachmentAdder } from "../lib/imagePaste";
import { imageShortIdsInUse } from "../lib/imageShortIds";
import { isEncryptedReasoning } from "../lib/modelCapabilities";
import {
  answersFromFormattedContent,
  isClaudeQuestionInput,
  questionsFromInput
} from "../lib/orchestration";
import { isHostDerivedToolName } from "../lib/taskTools";
import type {
  ContextItem,
  FileAttachment,
  ImageAttachment,
  InsertableContextKind,
  JsonObject,
  ToolContext,
  ToolDescriptor,
  UserContext
} from "../types";
import { Dialog } from "./Common";
import type { TimelineEditorState, TimelineQuestionEditorState } from "./ContextStream";
import { ConversationView } from "./ConversationView";

/**
 * The tool names a template may be asked to enable on its owner's behalf.
 *
 * Exactly the set the features page draws switches for: memory tools follow the
 * two memory switches and every other host-derived name follows a switch of its
 * own, so naming one of them here would ask the user to flip something the tool
 * picker does not have. A card calling a tool the catalog no longer carries is
 * likewise nothing anyone can enable, so it is not reported either.
 */
function enableableToolNames(tools: readonly ToolDescriptor[]): Set<string> {
  return new Set(
    tools
      .filter((tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name))
      .map((tool) => tool.name)
  );
}

/**
 * A template's message queue, opened to be worked on rather than merely read.
 *
 * The surface is the ordinary conversation view with its ordinary editors and
 * its ordinary right-click menu, because a template IS a message queue and a
 * second way of editing one would be a second renderer to keep in step. What
 * differs is where the edits go: nothing here touches a conversation, so every
 * change is held in this component until Save writes the whole body back in one
 * call.
 *
 * Tool calls can be placed and rewritten here like any other message, and this
 * is the one thing the surface withholds: there is no way to *run* one. A
 * template belongs to no conversation and no workspace, so there is nothing for
 * a call to execute against — the card is written down, and applying the
 * template is what later makes it a call in some conversation. A placed or
 * rewritten card therefore carries only what was typed: the host owns the rest
 * of the result, and normalizes the body on save so a card authored here is
 * exactly as trustworthy as one authored on a timeline, and no more.
 *
 * This is a page, not a window. Its owner — the 对话模板 page of a preset's
 * window or of a role's — supplies the chrome around it.
 */
export function ConversationTemplateEditor({
  templateId,
  contexts,
  tools,
  enabledTools,
  editable = true,
  imageInputSupported = false,
  autosave = false,
  onSave,
  onEnableTools
}: {
  /** The id this body is written back under. Empty until its owner mints one. */
  templateId: string;
  /** Null while the host is still reading the body — a real state on a big template. */
  contexts: ContextItem[] | null;
  tools: ToolDescriptor[];
  /**
   * The tools the right-click menu may place. A template can only call what its
   * owner hands the model, so this is the owner's enabled set rather than the
   * whole catalog — a card nobody could ever execute is not worth writing down.
   */
  enabledTools: readonly string[];
  /** False draws the same body with every mutation affordance withheld. */
  editable?: boolean;
  /**
   * Whether the model this body will run on reads images. A template belongs to
   * no conversation, so the owner answers for it: a preset's page asks the
   * conversation's model, and a role's window asks the model bound to that role.
   * False withholds the paste rather than storing bytes the run would drop.
   */
  imageInputSupported?: boolean;
  /**
   * Writes each committed edit back as it lands, instead of drawing a save
   * button under the page.
   *
   * For an owner whose window already saves — a preset's page sits in a window
   * with its own Save, and the body is written to the host either way — where a
   * second button a floor below the first is a second thing to forget. Edits
   * here are discrete acts (a card saved, placed or deleted), never keystrokes,
   * so this is one write per act.
   */
  autosave?: boolean;
  /** Resolves when the host has taken the body; rejects with the reason it did not. */
  onSave?: (contexts: ContextItem[]) => Promise<void>;
  /**
   * Widens the owner's enabled set to cover the tools a stored body still calls.
   * Absent where the owner cannot be widened from here, and its absence is what
   * makes saving skip the question entirely.
   */
  onEnableTools?: (names: string[]) => void;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState<ContextItem[] | null>(contexts);
  const [editor, setEditor] = useState<TimelineEditorState | null>(null);
  const [questionEditor, setQuestionEditor] = useState<TimelineQuestionEditorState | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /* The tools a save would leave uncallable, held while the user answers for
     them. Never empty: an empty list is simply not a question. */
  const [unenabledTools, setUnenabledTools] = useState<string[] | null>(null);
  /* The body arrives once, after the page is already mounted. Seeding on every
     change of the prop would discard the edits made since it landed. */
  const seeded = useRef(contexts !== null);
  /* The body a write was last started for, compared by identity. Autosave keys
     off this rather than `dirty` so a refused write is reported once instead of
     retried forever, and so seeding a freshly read body writes nothing back. */
  const written = useRef<ContextItem[] | null>(contexts);
  /* Tool names the user has already been asked about here. Autosave has no one
     save moment to raise the question at, and re-raising it on every later edit
     would make the page unusable. */
  const asked = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (seeded.current || !contexts) return;
    seeded.current = true;
    written.current = contexts;
    setDraft(contexts);
  }, [contexts]);

  const amend = (change: (contexts: ContextItem[]) => ContextItem[]) => {
    setDraft((current) => (current ? change(current) : current));
    setDirty(true);
    setError(null);
  };

  const beginEdit = (item: ContextItem) => {
    if (!draft) return;
    // Encrypted reasoning is delete-only: its body never reached this client, so
    // anything typed in its place would be invented history.
    if (item.kind === "reasoning" && isEncryptedReasoning(item)) return;
    const index = draft.findIndex((context) => context.id === item.id);
    if (index < 0) return;
    setEditor({ mode: "edit", kind: item.kind, item, index });
  };

  const removeIds = (ids: string[]) => {
    const dropped = new Set(ids);
    setEditor((current) => (
      current?.mode === "edit" && dropped.has(current.item.id) ? null : current
    ));
    setQuestionEditor((current) => (current && dropped.has(current.item.id) ? null : current));
    amend((contexts) => contexts.filter((context) => !dropped.has(context.id)));
  };

  const closeEditors = () => {
    setEditor(null);
    setQuestionEditor(null);
  };

  const saveText = (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => {
    if (!editor) return;
    if (editor.mode === "edit") {
      const { id } = editor.item;
      amend((contexts) => contexts.map((item) => {
        if (item.id !== id || item.kind === "tool") return item;
        if (item.kind === "assistant" || item.kind === "reasoning") {
          return { ...item, content, interrupted: false };
        }
        if (item.kind === "user" && images) return { ...item, content, images, files: files?.length ? files : undefined };
        return { ...item, content };
      }));
    } else {
      const base = {
        id: createId("ctx"),
        createdAt: new Date().toISOString(),
        content
      };
      const item: ContextItem = editor.kind === "reasoning"
        // Hand-written reasoning is plaintext by construction: the user supplied
        // every word of it.
        ? { ...base, kind: "reasoning", form: "plaintext" }
        : editor.kind === "user"
          // A placed message carries what was pasted into it; every other kind
          // has nowhere to put an image and is never handed one.
          ? { ...base, kind: "user", ...(images?.length ? { images } : {}), ...(files?.length ? { files } : {}) }
          : { ...base, kind: editor.kind as "system" | "assistant" };
      const { index } = editor;
      amend((contexts) => {
        const next = [...contexts];
        next.splice(index, 0, item);
        return next;
      });
    }
    setEditor(null);
  };

  /**
   * Writes a placed or rewritten tool card into the draft. Nothing is executed
   * and nothing is attested here: the body is not a conversation yet, and the
   * host re-issues every card's credential when the template is applied. What
   * this does have to do is stop claiming what it cannot know — a rewritten card
   * drops the stored conversation's attestation and the model's original
   * arguments, and a placed one starts with the result the host would mint for
   * a call nobody ran: text, and nothing else.
   */
  const saveTool = async (input: JsonObject, output: string, images: ImageAttachment[]): Promise<void> => {
    if (!editor) throw new Error(t("没有正在编辑的上下文", "No context is being edited"));
    if (editor.mode === "insert") {
      if (editor.kind !== "tool" || !editor.toolName) {
        throw new Error(t("没有选择工具", "No tool was chosen"));
      }
      const item: ToolContext = {
        id: createId("ctx"),
        kind: "tool",
        toolName: editor.toolName,
        input,
        result: { success: true, output, images: [], executedAt: "", durationMs: 0 },
        createdAt: new Date().toISOString()
      };
      const { index } = editor;
      amend((contexts) => {
        const next = [...contexts];
        next.splice(Math.min(index, next.length), 0, item);
        return next;
      });
      setEditor(null);
      return;
    }
    if (editor.item.kind !== "tool") {
      throw new Error(t("这条上下文不是工具调用", "This context is not a tool call"));
    }
    const { id } = editor.item;
    amend((contexts) => contexts.map((item) => (item.id === id && item.kind === "tool"
      ? {
        ...item,
        requestedInput: undefined,
        attestation: undefined,
        input,
        result: { ...item.result, output, images }
      }
      : item)));
    setEditor(null);
  };

  /**
   * Rewrites a question card and, when it has one, the answer that settled it.
   * A question is a tool card like any other, so it follows the same rule: the
   * host owns the result, the draft owns the arguments. The shape rules are the
   * question card's own — the same count of questions, and no answer emptied —
   * because a card whose answer no longer covers its questions renders as a
   * broken one wherever the template is applied.
   */
  const saveQuestion = async (input: JsonObject, answerContent?: string): Promise<void> => {
    if (!questionEditor) throw new Error(t("没有活动提问", "No active question"));
    const originalQuestions = questionsFromInput(questionEditor.item.input);
    const nextQuestions = questionsFromInput(input);
    if (!isClaudeQuestionInput(input) || nextQuestions.length !== originalQuestions.length) {
      throw new Error(t(
        "提问数量必须保持不变，且每一项都必须符合提问格式",
        "The number of questions must stay unchanged and every item must remain valid"
      ));
    }
    if (questionEditor.answer) {
      const nextAnswers = answersFromFormattedContent(answerContent ?? "", nextQuestions);
      if (!nextAnswers || nextAnswers.length !== nextQuestions.length || nextAnswers.some((answer) => !answer.trim())) {
        throw new Error(t(
          "每一个问题都必须保留非空回答",
          "Every question must keep a non-empty answer"
        ));
      }
    }
    const questionId = questionEditor.item.id;
    const answerId = questionEditor.answer?.id;
    amend((contexts) => contexts.map((item) => {
      if (item.id === questionId && item.kind === "tool") {
        return { ...item, requestedInput: undefined, attestation: undefined, input };
      }
      if (answerId && item.id === answerId && item.kind === "user") {
        return { ...item, content: answerContent ?? item.content };
      }
      return item;
    }));
    setQuestionEditor(null);
  };

  /**
   * The tools this body calls that its owner would not let the model call.
   *
   * The right-click menu only ever places an enabled tool, so nothing here can
   * be reached by writing a card today. It is reached by turning a switch off
   * afterwards — which is a thing the user is entitled to do, and a thing that
   * quietly turns part of a template into prose the model is shown but can never
   * act on. Saying so at save time is the only moment either half is in view.
   */
  const unenabled = useMemo(() => {
    if (!draft) return [];
    const enabled = new Set(enabledTools);
    const enableable = enableableToolNames(tools);
    const missing: string[] = [];
    draft.forEach((item) => {
      if (item.kind !== "tool") return;
      const { toolName } = item;
      if (enabled.has(toolName) || !enableable.has(toolName) || missing.includes(toolName)) return;
      missing.push(toolName);
    });
    return missing;
  }, [draft, enabledTools, tools]);

  const commit = async (body: ContextItem[]) => {
    if (!onSave) return;
    setSaving(true);
    setError(null);
    try {
      await onSave(body);
      setDirty(false);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSaving(false);
    }
  };

  const requestSave = () => {
    if (!draft || saving || !onSave) return;
    // Nothing to ask when the answer could not be acted on: an owner this page
    // cannot widen leaves "enable them" with nothing to do.
    const pending = autosave
      ? unenabled.filter((name) => !asked.current.has(name))
      : unenabled;
    if (pending.length && onEnableTools) {
      pending.forEach((name) => asked.current.add(name));
      setUnenabledTools(pending);
      return;
    }
    void commit(draft);
  };

  /* Autosave writes what the last committed edit produced. An emptied body is
     skipped rather than written: the host refuses it, and there is no button
     here to draw disabled instead. `written` is what makes this fire once per
     edit — `requestSave` reads the very draft this effect observed, so keying
     off the closure's identity would write the same body twice. */
  useEffect(() => {
    if (!autosave || !onSave || saving || !draft?.length) return;
    if (written.current === draft) return;
    written.current = draft;
    requestSave();
  }, [autosave, draft, onSave, saving]);

  const toolLabel = (name: string) => tools.find((tool) => tool.name === name)?.label ?? name;

  /* Without autosave the page has nowhere else to put its save, so it takes a
     strip along the bottom. */
  const actions = editable && onSave && !autosave && (
    <div className="template-editor__actions">
      <button
        type="button"
        className="text-button"
        /* An emptied template is refused by the host, and saying so before
           the round trip is kinder than saying it after. */
        disabled={!draft?.length || !dirty || saving}
        title={draft && !draft.length ? t(
          "模板至少要保留一条消息。",
          "A template must keep at least one message."
        ) : undefined}
        onClick={requestSave}
      >
        {saving ? t("正在保存…", "Saving…") : t("保存模板", "Save template")}
      </button>
      {error && <p className="template-editor__error" role="alert">{error}</p>}
    </div>
  );

  return (
    <div className="template-editor">
      {draft === null ? (
        <p className="template-preview__loading">{t("正在读取…", "Loading…")}</p>
      ) : (
        <ConversationView
          className="template-preview"
          editable={editable}
          contexts={draft}
          tools={tools}
          enabledTools={enabledTools as string[]}
          timelineId={templateId || "template:unsaved"}
          ariaLabel={t("模板消息队列", "Template message queue")}
          editor={editable ? editor : null}
          questionEditor={editable ? questionEditor : null}
          onEdit={editable ? beginEdit : undefined}
          onDelete={editable ? (item: ContextItem) => removeIds([item.id]) : undefined}
          onEditQuestion={editable
            ? (item: ToolContext, answer?: UserContext) => setQuestionEditor({ item, answer })
            : undefined}
          onDeleteQuestion={editable
            ? (item: ToolContext, answer?: UserContext) => removeIds(answer ? [item.id, answer.id] : [item.id])
            : undefined}
          onDeleteContexts={editable ? removeIds : undefined}
          onInsert={editable
            ? (index: number, kind: InsertableContextKind, toolName?: string) => setEditor({
              mode: "insert",
              kind,
              index,
              toolName
            })
            : undefined}
          onCancelEdit={editable ? closeEditors : undefined}
          onSaveText={editable ? saveText : undefined}
          onAddAttachments={editable
            // A template has no transcript behind it, so its own body is the
            // whole of what a number can already be spoken for by.
            ? messageAttachmentAdder(imageInputSupported, () => imageShortIdsInUse(draft ?? []))
            : undefined}
          attachmentImageInput={imageInputSupported}
          /* No `onSaveTool`: that handler is the one that executes, and there is
             nothing here to execute against. Its absence is what keeps the run
             button off the card editor. */
          onSaveToolEdit={editable ? saveTool : undefined}
          onSaveQuestion={editable ? saveQuestion : undefined}
        />
      )}

      {actions}

      {/* With no action row there is no free space to report into, so a refused
          write takes a strip of its own rather than vanishing. */}
      {autosave && error && (
        <p className="template-editor__error template-editor__error--strip" role="alert">{error}</p>
      )}

      {/* Turning a tool off after a template was written for it is a thing the
          user is entitled to do, so this asks rather than refuses — and offers
          the repair, because hunting the switches down by hand is the tedious
          half of the answer. */}
      {unenabledTools && (
        <Dialog
          title={t("模板里有未启用的工具", "This template uses tools that are not enabled")}
          description={t(
            "保存没问题，但这些工具在当前设置里是关着的，套用之后模型看得见这些调用却用不了它们。",
            "Saving is fine, but these tools are switched off here: once the template is applied the model can see the calls and cannot make them."
          )}
          onClose={() => setUnenabledTools(null)}
          footer={(
            <>
              <button
                type="button"
                className="button button--secondary"
                onClick={() => {
                  setUnenabledTools(null);
                  if (draft) void commit(draft);
                }}
              >{t("正常保存", "Save anyway")}</button>
              <button
                type="button"
                className="button button--primary"
                onClick={() => {
                  onEnableTools?.(unenabledTools);
                  setUnenabledTools(null);
                  if (draft) void commit(draft);
                }}
              >{t("自动启用所需工具", "Enable the tools it needs")}</button>
            </>
          )}
        >
          <ul className="template-editor__missing-tools">
            {unenabledTools.map((name) => (
              <li key={name}>
                <strong>{toolLabel(name)}</strong>
                <small>{name}</small>
              </li>
            ))}
          </ul>
        </Dialog>
      )}
    </div>
  );
}
