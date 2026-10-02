import { ChevronLeft, ChevronRight, Circle, CircleCheck, X } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { useI18n } from "../i18n";
import {
  joinMultiSelectAnswer,
  questionsFromInput,
  type QuestionItemView
} from "../lib/orchestration";
import type { JsonObject, PendingToolPrompt, QuestionResponse } from "../types";
import { IconButton } from "./Common";
import { MarkdownContent } from "./MarkdownContent";
import type { ToolApprovalDockStack } from "./ToolApprovalDock";
import "./QuestionDock.css";

/**
 * The card an `ask_user` call blocks on, modeled on Claude Code's
 * AskUserQuestion dialog: a tab per question plus a Submit tab, numbered
 * options with an "Other" row, "Chat about this", a preview layout with notes
 * for single-select questions whose options carry previews, and a review
 * screen. Esc, the close button and the review screen's Cancel close the card.
 *
 * The composer stays live while the card is up. `onDraftChange` keeps the page
 * told what the card would hand back if a composer message were sent now: the
 * answers filled in so far, or a close when there are none.
 */
export interface QuestionDockProps {
  prompt: PendingToolPrompt | null;
  /** Absent when the queue holds a single card; the pager only draws for 2+. */
  stack?: ToolApprovalDockStack;
  disabled?: boolean;
  /** `false` means the host did not take the answer; the card unlocks for a retry. */
  onRespond: (response: QuestionResponse) => void | Promise<boolean>;
  onDraftChange?: (promptId: string, response: QuestionResponse) => void;
}

/** Claude Code shows no preview longer than this; the model is told to compare labels. */
const MAX_PREVIEW_CHARS = 2000;

interface QuestionDraft {
  /** Single-select: the picked option label, or `"other"` for the typed answer. */
  selected: string | null;
  /** Multi-select: the checked option labels. */
  checked: string[];
  other: string;
  notes: string;
}

function emptyDraft(): QuestionDraft {
  return { selected: null, checked: [], other: "", notes: "" };
}

/** The single-select slot for the typed "Other" answer; no option label can equal it. */
const OTHER = "\u0000other";

/** Single-select questions with any option preview use the side-by-side layout. */
function usesPreviewLayout(question: QuestionItemView): boolean {
  return !question.multiSelect && question.options.some((option) => option.preview !== undefined);
}

function answerOf(question: QuestionItemView, draft: QuestionDraft): string | null {
  if (question.multiSelect) {
    const items = question.options
      .map((option) => option.label)
      .filter((label) => draft.checked.includes(label));
    if (draft.other.trim()) items.push(draft.other);
    return items.length ? joinMultiSelectAnswer(items) : null;
  }
  if (draft.selected === OTHER) return draft.other.trim() ? draft.other : null;
  return draft.selected;
}

function responseFrom(
  action: QuestionResponse["action"],
  questions: QuestionItemView[],
  drafts: QuestionDraft[]
): QuestionResponse {
  return {
    action,
    answers: questions.map((question, index) => answerOf(question, drafts[index])),
    previews: questions.map((question, index) => {
      if (!usesPreviewLayout(question)) return null;
      const picked = question.options.find((option) => option.label === drafts[index].selected);
      return picked?.preview ?? null;
    }),
    notes: questions.map((question, index) => (
      usesPreviewLayout(question) && drafts[index].notes.trim() ? drafts[index].notes.trim() : null
    ))
  };
}

/** What a composer message sent now would hand back: the answers so far, or a close. */
function composerResponse(questions: QuestionItemView[], drafts: QuestionDraft[]): QuestionResponse {
  const submitted = responseFrom("submit", questions, drafts);
  const anything = submitted.answers.some(Boolean) || submitted.notes?.some(Boolean);
  return anything ? submitted : { action: "close", answers: [] };
}

export function QuestionDock(props: QuestionDockProps) {
  if (!props.prompt || props.prompt.kind !== "question") return null;
  return <QuestionDockContent key={props.prompt.promptId} {...props} prompt={props.prompt} />;
}

function QuestionDockContent({
  prompt,
  stack,
  disabled = false,
  onRespond,
  onDraftChange
}: QuestionDockProps & { prompt: PendingToolPrompt }) {
  const { t } = useI18n();
  const titleId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const rowRefs = useRef<Array<HTMLElement | null>>([]);
  const questions = useMemo(
    () => questionsFromInput({ questions: prompt.questions ?? [] } as JsonObject),
    [prompt.questions]
  );
  const [drafts, setDrafts] = useState<QuestionDraft[]>(() => questions.map(emptyDraft));
  const [index, setIndex] = useState(0);
  const [focus, setFocus] = useState(0);
  const [notesOpen, setNotesOpen] = useState(false);
  const [notesFocusMoved, setNotesFocusMoved] = useState(false);
  const [responded, setResponded] = useState(false);
  const locked = disabled || responded;

  const hideSubmitTab = questions.length === 1 && !questions[0]?.multiSelect;
  const maxIndex = hideSubmitTab ? questions.length - 1 : questions.length;
  const onReview = index === questions.length;
  const question = onReview ? undefined : questions[index];
  const draft = onReview ? undefined : drafts[index];
  const previewLayout = question ? usesPreviewLayout(question) : false;
  const answers = questions.map((item, at) => answerOf(item, drafts[at]));

  useEffect(() => {
    onDraftChange?.(prompt.promptId, composerResponse(questions, drafts));
  }, [drafts, onDraftChange, prompt.promptId, questions]);

  useEffect(() => {
    setFocus(0);
    setNotesOpen(false);
  }, [index]);

  useEffect(() => {
    dialogRef.current?.focus({ preventScroll: true });
  }, []);

  const respond = (response: QuestionResponse) => {
    if (locked) return;
    setResponded(true);
    void Promise.resolve(onRespond(response)).then((taken) => {
      if (taken === false) setResponded(false);
    });
  };
  const close = () => respond({ action: "close", answers: [] });
  const submit = (nextDrafts = drafts) => respond(responseFrom("submit", questions, nextDrafts));
  const chat = () => respond(responseFrom("chat", questions, drafts));

  const updateDraft = (at: number, change: (current: QuestionDraft) => QuestionDraft) => {
    const next = drafts.map((current, draftIndex) => (draftIndex === at ? change(current) : current));
    setDrafts(next);
    return next;
  };

  /** Picking a single-select option answers it and moves on — or, for a lone
   * single-select question, submits the card outright. */
  const pick = (label: string) => {
    if (locked || !question) return;
    const next = updateDraft(index, (current) => ({ ...current, selected: label }));
    if (hideSubmitTab) {
      submit(next);
      return;
    }
    setIndex((current) => Math.min(current + 1, maxIndex));
  };

  const toggle = (label: string) => {
    if (locked) return;
    updateDraft(index, (current) => ({
      ...current,
      checked: current.checked.includes(label)
        ? current.checked.filter((item) => item !== label)
        : [...current.checked, label]
    }));
  };

  /** Checks the single-select Other circle without answering or moving on:
   * the answer is whatever is typed in its box. */
  const chooseOther = () => {
    if (locked || !question || question.multiSelect || drafts[index]?.selected === OTHER) return;
    updateDraft(index, (current) => ({ ...current, selected: OTHER }));
  };

  const optionCount = question?.options.length ?? 0;
  // Row layout: options, then (standard layout only) the Other row, then the chat row.
  const otherRow = previewLayout ? -1 : optionCount;
  const chatRow = previewLayout ? optionCount : optionCount + 1;
  const nextButtonRow = question?.multiSelect ? chatRow + 1 : -1;

  const focusRow = (row: number) => {
    setFocus(row);
    if (previewLayout && notesOpen) setNotesFocusMoved(true);
    window.requestAnimationFrame(() => rowRefs.current[row]?.focus({ preventScroll: true }));
  };

  const activateRow = (row: number) => {
    if (!question || locked) return;
    if (row === chatRow) {
      chat();
      return;
    }
    if (row === otherRow) {
      rowRefs.current[row]?.focus();
      return;
    }
    const option = question.options[row];
    if (!option) return;
    if (question.multiSelect) toggle(option.label);
    else pick(option.label);
  };

  const inTextInput = (target: EventTarget) => (
    target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement
  );

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (locked || event.nativeEvent.isComposing) return;
    const typing = inTextInput(event.target);
    if (event.key === "Escape") {
      event.preventDefault();
      if (notesOpen) {
        setNotesOpen(false);
        dialogRef.current?.focus();
        return;
      }
      close();
      return;
    }
    if (typing) return;
    if (event.key === "Tab" || event.key === "ArrowRight" || event.key === "ArrowLeft") {
      const step = event.key === "ArrowLeft" || (event.key === "Tab" && event.shiftKey) ? -1 : 1;
      const next = Math.max(0, Math.min(maxIndex, index + step));
      if (next !== index || event.key === "Tab") event.preventDefault();
      setIndex(next);
      return;
    }
    if (!question) return;
    const lastRow = Math.max(chatRow, nextButtonRow);
    if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) {
      event.preventDefault();
      focusRow(Math.min(lastRow, focus + 1));
      return;
    }
    if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
      event.preventDefault();
      focusRow(Math.max(0, focus - 1));
      return;
    }
    if (previewLayout && event.key === "n" && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      setNotesOpen(true);
      setNotesFocusMoved(false);
      return;
    }
    if (/^[1-9]$/.test(event.key)) {
      const row = Number(event.key) - 1;
      event.preventDefault();
      // In the preview layout a digit only moves focus.
      if (previewLayout) {
        if (row < optionCount) focusRow(row);
        return;
      }
      if (row === otherRow) {
        if (question.multiSelect || !draft?.other.trim()) focusRow(row);
        else pick(OTHER);
        return;
      }
      if (row < optionCount || row === chatRow) activateRow(row);
    }
  };

  const onOtherKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "Enter" || event.nativeEvent.isComposing || !question || !draft) return;
    event.preventDefault();
    if (question.multiSelect) {
      if (event.ctrlKey || event.metaKey) setIndex((current) => Math.min(current + 1, maxIndex));
      return;
    }
    // Claude Code: Enter on a blank "Other" dismisses the dialog.
    if (!draft.other.trim()) {
      close();
      return;
    }
    pick(OTHER);
  };

  const onNotesKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "Enter" || event.nativeEvent.isComposing || !question || !draft) return;
    event.preventDefault();
    setNotesOpen(false);
    const focused = question.options[focus];
    if (draft.notes.trim() && notesFocusMoved && focused) {
      pick(focused.label);
    } else if (draft.selected) {
      pick(draft.selected);
    } else if (draft.notes.trim()) {
      // Notes with nothing selected still answer the question ("(notes only)").
      setIndex((current) => Math.min(current + 1, maxIndex));
    }
    dialogRef.current?.focus();
  };

  const hints = previewLayout
    ? [
        t("Enter 选择", "Enter to select"),
        t("↑/↓ 移动", "↑/↓ to navigate"),
        t("n 添加备注", "n to add notes"),
        ...(questions.length > 1 ? [t("Tab 切换问题", "Tab to switch questions")] : []),
        t("Esc 取消", "Esc to cancel")
      ]
    : [
        t("Enter 选择", "Enter to select"),
        questions.length === 1 ? t("↑/↓ 移动", "↑/↓ to navigate") : t("Tab/方向键切换", "Tab/Arrow keys to navigate"),
        t("Esc 取消", "Esc to cancel")
      ];

  const focusedOption = question?.options[Math.min(focus, optionCount - 1)];
  const previewText = focusedOption?.preview;

  return (
    <section
      ref={dialogRef}
      className={`question-dock${locked ? " question-dock--locked" : ""}`}
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      aria-disabled={locked || undefined}
      data-pending-question="true"
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      <header className="question-dock__tabs">
        {stack && stack.total > 1 && (
          <span className="tool-approval-dock__pager" data-pending-approval-pager="true">
            <button
              type="button"
              className="tool-approval-dock__pager-step"
              aria-label={t("上一张卡片", "Previous card")}
              disabled={stack.index <= 0}
              onClick={() => stack.onNavigate(-1)}
            >
              <ChevronLeft size={12} aria-hidden="true" />
            </button>
            <span className="tool-approval-dock__pager-count">{stack.index + 1}/{stack.total}</span>
            <button
              type="button"
              className="tool-approval-dock__pager-step"
              aria-label={t("下一张卡片", "Next card")}
              disabled={stack.index >= stack.total - 1}
              onClick={() => stack.onNavigate(1)}
            >
              <ChevronRight size={12} aria-hidden="true" />
            </button>
          </span>
        )}
        <div className="question-dock__chips" role="tablist" aria-label={t("问题", "Questions")}>
          {maxIndex > 0 && (
            <button
              type="button"
              className="question-dock__arrow"
              aria-label={t("切换到上一个问题", "Previous question")}
              disabled={index === 0}
              onClick={() => setIndex((current) => Math.max(0, current - 1))}
            >
              ←
            </button>
          )}
          {questions.map((item, at) => (
            <button
              key={`${at}-${item.question}`}
              type="button"
              role="tab"
              aria-selected={at === index}
              aria-label={answers[at]
                ? t("{header}（已回答）", "{header} (answered)", { header: item.header || `Q${at + 1}` })
                : item.header || `Q${at + 1}`}
              className={`question-dock__chip${at === index ? " question-dock__chip--current" : ""}`}
              onClick={() => setIndex(at)}
            >
              <span aria-hidden="true">{answers[at] ? "☒" : "☐"}</span>
              <span>{item.header || `Q${at + 1}`}</span>
            </button>
          ))}
          {!hideSubmitTab && (
            <button
              type="button"
              role="tab"
              aria-selected={onReview}
              className={`question-dock__chip${onReview ? " question-dock__chip--current" : ""}`}
              onClick={() => setIndex(questions.length)}
            >
              <span aria-hidden="true">✓</span>
              <span>{t("提交", "Submit")}</span>
            </button>
          )}
          {maxIndex > 0 && (
            <button
              type="button"
              className="question-dock__arrow"
              aria-label={t("切换到下一个问题", "Next question")}
              disabled={index >= maxIndex}
              onClick={() => setIndex((current) => Math.min(maxIndex, current + 1))}
            >
              →
            </button>
          )}
        </div>
        <IconButton label={t("关闭提问", "Close questions")} disabled={locked} onClick={close}>
          <X size={14} />
        </IconButton>
      </header>

      {question && draft ? (
        <div className="question-dock__page">
          <h2 id={titleId} className="question-dock__prompt">{question.question}</h2>

          {previewLayout ? (
            <>
              <div className="question-dock__split">
                <div className="question-dock__list" role="listbox" aria-labelledby={titleId}>
                  {question.options.map((option, row) => {
                    const selected = draft.selected === option.label;
                    return (
                      <button
                        key={`${row}-${option.label}`}
                        ref={(element) => { rowRefs.current[row] = element; }}
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={`question-dock__row${focus === row ? " question-dock__row--focused" : ""}${selected ? " question-dock__row--selected" : ""}`}
                        disabled={locked}
                        onFocus={() => setFocus(row)}
                        onMouseEnter={() => setFocus(row)}
                        onClick={() => pick(option.label)}
                      >
                        <span className="question-dock__pointer" aria-hidden="true">{focus === row ? "❯" : ""}</span>
                        <span className="question-dock__index">{row + 1}.</span>
                        <span className="question-dock__label">{option.label}</span>
                        {selected && <span className="question-dock__tick" aria-hidden="true">✔</span>}
                      </button>
                    );
                  })}
                </div>
                <div className="question-dock__preview-box">
                  {previewText === undefined ? (
                    <span className="question-dock__muted">{t("没有预览", "No preview available")}</span>
                  ) : previewText.length > MAX_PREVIEW_CHARS ? (
                    <span className="question-dock__muted">
                      {t(
                        "（预览过长，无法完整显示——请对比选项名称和说明）",
                        "(preview cannot be shown in full — compare the option labels and descriptions instead)"
                      )}
                    </span>
                  ) : (
                    <MarkdownContent className="question-dock__preview" content={previewText} />
                  )}
                </div>
              </div>
              <div className="question-dock__notes">
                <span className="question-dock__notes-label">{t("备注：", "Notes:")}</span>
                {notesOpen ? (
                  <input
                    autoFocus
                    value={draft.notes}
                    placeholder={t("为这个方案添加备注…", "Add notes on this design…")}
                    disabled={locked}
                    onChange={(event) => updateDraft(index, (current) => ({ ...current, notes: event.target.value }))}
                    onKeyDown={onNotesKeyDown}
                  />
                ) : (
                  <button
                    type="button"
                    className="question-dock__notes-text"
                    disabled={locked}
                    onClick={() => { setNotesOpen(true); setNotesFocusMoved(false); }}
                  >
                    {draft.notes.trim() || t("按 n 添加备注", "press n to add notes")}
                  </button>
                )}
              </div>
            </>
          ) : (
            <div className="question-dock__list" role={question.multiSelect ? "group" : "listbox"} aria-labelledby={titleId}>
              {question.options.map((option, row) => {
                const selected = question.multiSelect
                  ? draft.checked.includes(option.label)
                  : draft.selected === option.label;
                return (
                  <button
                    key={`${row}-${option.label}`}
                    ref={(element) => { rowRefs.current[row] = element; }}
                    type="button"
                    role={question.multiSelect ? "checkbox" : "option"}
                    aria-checked={question.multiSelect ? selected : undefined}
                    aria-selected={question.multiSelect ? undefined : selected}
                    className={`question-dock__row question-dock__row--described${focus === row ? " question-dock__row--focused" : ""}${selected ? " question-dock__row--selected" : ""}`}
                    disabled={locked}
                    onFocus={() => setFocus(row)}
                    onClick={() => activateRow(row)}
                  >
                    <span className="question-dock__pointer" aria-hidden="true">{focus === row ? "❯" : ""}</span>
                    <span className="question-dock__index">{row + 1}.</span>
                    {question.multiSelect ? (
                      <span className="question-dock__checkbox" aria-hidden="true">{selected ? "[✔]" : "[ ]"}</span>
                    ) : (
                      <RadioMark checked={selected} />
                    )}
                    <span className="question-dock__option-text">
                      <span className="question-dock__label">{option.label}</span>
                      {option.description && <small>{option.description}</small>}
                    </span>
                  </button>
                );
              })}
              <div
                className={`question-dock__row question-dock__row--other${focus === otherRow ? " question-dock__row--focused" : ""}${(question.multiSelect ? draft.other.trim() : draft.selected === OTHER) ? " question-dock__row--selected" : ""}`}
              >
                <span className="question-dock__pointer" aria-hidden="true">{focus === otherRow ? "❯" : ""}</span>
                <span className="question-dock__index">{otherRow + 1}.</span>
                {question.multiSelect ? (
                  <span className="question-dock__checkbox" aria-hidden="true">{draft.other.trim() ? "[✔]" : "[ ]"}</span>
                ) : (
                  // Other is picked only while its circle is checked; clicking
                  // into the box checks it, and picking an option clears it.
                  <button
                    type="button"
                    role="radio"
                    aria-checked={draft.selected === OTHER}
                    aria-label={t("选择其他", "Choose Other")}
                    className="question-dock__radio-button"
                    disabled={locked}
                    onClick={() => {
                      chooseOther();
                      rowRefs.current[otherRow]?.focus();
                    }}
                  >
                    <RadioMark checked={draft.selected === OTHER} />
                  </button>
                )}
                <input
                  ref={(element) => { rowRefs.current[otherRow] = element; }}
                  value={draft.other}
                  aria-label={t("其他", "Other")}
                  placeholder={question.multiSelect ? t("输入其他内容", "Type something") : t("输入其他内容。", "Type something.")}
                  disabled={locked}
                  onFocus={() => {
                    setFocus(otherRow);
                    if (!question.multiSelect) chooseOther();
                  }}
                  onChange={(event) => {
                    const value = event.target.value;
                    updateDraft(index, (current) => ({
                      ...current,
                      other: value,
                      ...(question.multiSelect ? {} : { selected: OTHER })
                    }));
                  }}
                  onKeyDown={onOtherKeyDown}
                />
              </div>
              {question.multiSelect && (
                <button
                  ref={(element) => { rowRefs.current[nextButtonRow] = element; }}
                  type="button"
                  className="question-dock__advance"
                  disabled={locked}
                  onFocus={() => setFocus(nextButtonRow)}
                  onClick={() => setIndex((current) => Math.min(current + 1, maxIndex))}
                >
                  {index === questions.length - 1 ? t("提交", "Submit") : t("下一题", "Next")}
                </button>
              )}
            </div>
          )}

          <hr className="question-dock__divider" />
          <button
            ref={(element) => { rowRefs.current[chatRow] = element; }}
            type="button"
            className={`question-dock__row question-dock__row--chat${focus === chatRow ? " question-dock__row--focused" : ""}`}
            disabled={locked}
            onFocus={() => setFocus(chatRow)}
            onClick={chat}
          >
            <span className="question-dock__pointer" aria-hidden="true">{focus === chatRow ? "❯" : ""}</span>
            {!previewLayout && <span className="question-dock__index">{chatRow + 1}.</span>}
            <span className="question-dock__label">{t("先聊聊这个", "Chat about this")}</span>
          </button>
        </div>
      ) : (
        <div className="question-dock__page question-dock__review">
          <h2 id={titleId} className="question-dock__prompt">{t("检查你的回答", "Review your answers")}</h2>
          {answers.some((answer) => !answer) && (
            <p className="question-dock__warning" role="status">
              {t("你还有问题没有回答", "You have not answered all questions")}
            </p>
          )}
          <ul className="question-dock__summary">
            {questions.map((item, at) => answers[at] ? (
              <li key={`${at}-${item.question}`}>
                <span>{item.question}</span>
                <span className="question-dock__summary-answer">→ {answers[at]}</span>
              </li>
            ) : null)}
          </ul>
          <p className="question-dock__muted">{t("确定提交这些回答？", "Ready to submit your answers?")}</p>
          <div className="question-dock__review-actions">
            <button type="button" className="question-dock__advance" disabled={locked} onClick={() => submit()}>
              {t("提交回答", "Submit answers")}
            </button>
            <button type="button" className="question-dock__secondary" disabled={locked} onClick={close}>
              {t("取消", "Cancel")}
            </button>
          </div>
        </div>
      )}

      <footer className="question-dock__hints">{hints.join(" · ")}</footer>
    </section>
  );
}

/** The single-select check circle: empty, or ticked when the row is picked. */
function RadioMark({ checked }: { checked: boolean }) {
  return (
    <span className={`question-dock__radio${checked ? " question-dock__radio--checked" : ""}`} aria-hidden="true">
      {checked ? <CircleCheck size={14} /> : <Circle size={14} />}
    </span>
  );
}
