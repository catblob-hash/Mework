import {
  ArrowLeft,
  ArrowRight,
  Check,
  CheckSquare2,
  CircleCheck,
  CircleHelp,
  Pencil,
  Square,
  X
} from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import type { FormEvent } from "react";
import { useI18n } from "../i18n";
import {
  formatQuestionAnswers,
  type PendingQuestion,
  type QuestionItemView
} from "../lib/orchestration";
import type { ToolContext } from "../types";
import { IconButton } from "./Common";
import "./QuestionDock.css";

export interface QuestionDockProps {
  pending: PendingQuestion | null;
  disabled?: boolean;
  editDisabled?: boolean;
  /** A false result means the answer was not delivered (for example, the host rejected
   * the content change or configuration is missing). Unlock the controls so the user can
   * retry; never present an undelivered answer as submitted. */
  onAnswer: (answer: string) => void | Promise<boolean>;
  onEditQuestion?: (item: ToolContext) => void;
  onDismissQuestion?: (item: ToolContext) => void;
}

interface QuestionDraft {
  selected: string[];
  other: boolean;
  custom: string;
}

interface QuestionDockContentProps extends Omit<QuestionDockProps, "pending"> {
  pending: PendingQuestion;
}

function initialDrafts(questions: QuestionItemView[]): QuestionDraft[] {
  return questions.map(() => ({ selected: [], other: false, custom: "" }));
}

function answerFromDraft(draft: QuestionDraft): string {
  return draft.other ? draft.custom.trim() : draft.selected.join(", ");
}

function QuestionDockContent({
  pending,
  disabled = false,
  editDisabled = false,
  onAnswer,
  onEditQuestion,
  onDismissQuestion
}: QuestionDockContentProps) {
  const { t } = useI18n();
  const titleId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const firstOptionRef = useRef<HTMLButtonElement>(null);
  const customRefs = useRef<Array<HTMLTextAreaElement | null>>([]);
  const submittedRef = useRef(false);
  const questions = pending.questions.length
    ? pending.questions
    : [{
        question: pending.question,
        header: t("问题", "Question"),
        options: pending.options,
        multiSelect: false
      }];
  const [drafts, setDrafts] = useState(() => initialDrafts(questions));
  const [activeIndex, setActiveIndex] = useState(0);
  const [preview, setPreview] = useState<string | null>(null);
  const [submittedAnswer, setSubmittedAnswer] = useState<string | null>(null);
  const locked = submittedAnswer !== null;
  const controlsDisabled = disabled || locked;
  const answers = drafts.map(answerFromDraft);
  const answeredCount = answers.filter(Boolean).length;
  const activeQuestion = questions[activeIndex];
  const activeDraft = drafts[activeIndex];
  const isFirst = activeIndex === 0;
  const isLast = activeIndex === questions.length - 1;
  const canContinue = !controlsDisabled && Boolean(answers[activeIndex]);

  useEffect(() => {
    setPreview(null);
    (disabled ? dialogRef.current : firstOptionRef.current ?? customRefs.current[activeIndex] ?? dialogRef.current)
      ?.focus({ preventScroll: true });
  }, [activeIndex, disabled]);

  const submitAnswers = (nextDrafts = drafts) => {
    if (disabled || submittedRef.current) return;
    const nextAnswers = nextDrafts.map(answerFromDraft);
    if (nextAnswers.some((answer) => !answer)) return;
    const answer = formatQuestionAnswers(questions, nextAnswers);
    submittedRef.current = true;
    setSubmittedAnswer(answer);
    void Promise.resolve(onAnswer(answer)).then((delivered) => {
      if (delivered !== false) return;
      // Keep the draft and unlock the controls when delivery fails so the user can retry.
      submittedRef.current = false;
      setSubmittedAnswer(null);
    });
  };

  const selectOption = (questionIndex: number, label: string) => {
    if (controlsDisabled) return;
    setDrafts((current) => current.map((draft, index) => {
      if (index !== questionIndex) return draft;
      const question = questions[index];
      const selected = question.multiSelect
        ? draft.selected.includes(label)
          ? draft.selected.filter((item) => item !== label)
          : [...draft.selected, label]
        : [label];
      return { selected, other: false, custom: "" };
    }));
  };

  const chooseOther = (questionIndex: number) => {
    if (controlsDisabled) return;
    setDrafts((current) => current.map((draft, index) => (
      index === questionIndex
        ? { selected: [], other: true, custom: draft.custom }
        : draft
    )));
    window.requestAnimationFrame(() => customRefs.current[questionIndex]?.focus());
  };

  const setCustomAnswer = (questionIndex: number, value: string) => {
    setDrafts((current) => current.map((draft, index) => (
      index === questionIndex
        ? { selected: [], other: true, custom: value }
        : draft
    )));
  };

  const submitForm = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!isLast) {
      if (canContinue) setActiveIndex((index) => Math.min(index + 1, questions.length - 1));
      return;
    }
    submitAnswers();
  };

  return (
    <section
      ref={dialogRef}
      className={`question-dock${locked ? " question-dock--submitted" : ""}`}
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      aria-disabled={disabled || undefined}
      data-pending-question="true"
      tabIndex={-1}
    >
      <header className="question-dock__header">
        <span className="question-dock__icon" aria-hidden="true"><CircleHelp size={15} /></span>
        <div className="question-dock__title">
          <h2 id={titleId}>{t("需要你的回答", "Your input is needed")}</h2>
          <span>
            {questions.length > 1
              ? t("{count} 个问题", "{count} questions", { count: questions.length })
              : questions[0]?.header}
          </span>
        </div>
        {onEditQuestion && (
          <IconButton
            label={t("编辑提问", "Edit questions")}
            disabled={editDisabled || locked}
            onClick={() => onEditQuestion(pending.context)}
          >
            <Pencil size={13} />
          </IconButton>
        )}
        {onDismissQuestion && (
          <IconButton
            label={t("关闭并删除提问", "Close and delete questions")}
            disabled={editDisabled || locked}
            onClick={() => onDismissQuestion(pending.context)}
          >
            <X size={14} />
          </IconButton>
        )}
      </header>

      <form onSubmit={submitForm}>
        <div className="question-dock__page">
          {activeQuestion && activeDraft && (
            <fieldset className="question-dock__question" key={`${activeIndex}-${activeQuestion.question}`}>
              <legend>
                <span>{activeQuestion.header}</span>
                <strong>{activeQuestion.question}</strong>
                {activeQuestion.multiSelect && <small>{t("可多选", "Select multiple")}</small>}
              </legend>

              <div className="question-dock__options">
                {activeQuestion.options.map((option, optionIndex) => {
                  const selected = activeDraft.selected.includes(option.label);
                  return (
                    <button
                      key={`${optionIndex}-${option.label}`}
                      ref={optionIndex === 0 ? firstOptionRef : undefined}
                      type="button"
                      className={`question-dock__option${selected ? " question-dock__option--selected" : ""}`}
                      aria-pressed={selected}
                      disabled={controlsDisabled}
                      onFocus={() => setPreview(option.preview ?? null)}
                      onMouseEnter={() => setPreview(option.preview ?? null)}
                      onClick={() => selectOption(activeIndex, option.label)}
                    >
                      <span className="question-dock__option-mark" aria-hidden="true">
                        {activeQuestion.multiSelect
                          ? selected ? <CheckSquare2 size={15} /> : <Square size={15} />
                          : selected ? <CircleCheck size={15} /> : <span />}
                      </span>
                      <span>
                        <strong>{option.label}</strong>
                        {option.description && <small>{option.description}</small>}
                      </span>
                    </button>
                  );
                })}

                <button
                  type="button"
                  className={`question-dock__option question-dock__option--other${activeDraft.other ? " question-dock__option--selected" : ""}`}
                  aria-pressed={activeDraft.other}
                  disabled={controlsDisabled}
                  onClick={() => chooseOther(activeIndex)}
                >
                  <span className="question-dock__option-mark" aria-hidden="true">
                    {activeDraft.other ? <CircleCheck size={15} /> : <span />}
                  </span>
                  <span>
                    <strong>{t("其他", "Other")}</strong>
                    <small>{t("输入自定义回答", "Type a custom answer")}</small>
                  </span>
                </button>
              </div>

              {preview && (
                <pre className="question-dock__preview">{preview}</pre>
              )}

              {activeDraft.other && (
                <label className="question-dock__custom">
                  <span className="sr-only">
                    {t("{header}的自定义回答", "Custom answer for {header}", { header: activeQuestion.header })}
                  </span>
                  <textarea
                    ref={(element) => { customRefs.current[activeIndex] = element; }}
                    rows={2}
                    value={activeDraft.custom}
                    placeholder={t("输入你的回答…", "Type your answer…")}
                    disabled={controlsDisabled}
                    onChange={(event) => setCustomAnswer(activeIndex, event.target.value)}
                  />
                </label>
              )}
            </fieldset>
          )}
        </div>

        <footer className="question-dock__footer">
          <button
            type="button"
            className="question-dock__nav question-dock__nav--previous"
            disabled={controlsDisabled || isFirst}
            onClick={() => setActiveIndex((index) => Math.max(0, index - 1))}
          >
            <ArrowLeft size={14} aria-hidden="true" />
            {t("上一项", "Previous")}
          </button>
          <div className="question-dock__progress" aria-live="polite">
            {submittedAnswer ? (
              <span className="question-dock__submitted" role="status">
                <Check size={13} aria-hidden="true" />
                {t("回答已提交", "Answers submitted")}
              </span>
            ) : disabled ? (
              <span>{t("暂时无法提交回答", "Answers cannot be submitted right now")}</span>
            ) : (
              <span>
                {t(
                  "第 {current} / {total} 项 · 已回答 {answered} 项",
                  "{current} of {total} · {answered} answered",
                  { current: activeIndex + 1, total: questions.length, answered: answeredCount }
                )}
              </span>
            )}
          </div>
          <button type="submit" className="question-dock__nav question-dock__nav--next" disabled={!canContinue}>
            {isLast ? t("完成", "Done") : t("下一项", "Next")}
            {isLast ? <Check size={14} aria-hidden="true" /> : <ArrowRight size={14} aria-hidden="true" />}
          </button>
        </footer>
      </form>
    </section>
  );
}

export function QuestionDock(props: QuestionDockProps) {
  if (!props.pending) return null;
  const key = `${props.pending.context.id}:${JSON.stringify(props.pending.context.input)}`;
  return <QuestionDockContent key={key} {...props} pending={props.pending} />;
}
