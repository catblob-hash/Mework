import { CircleHelp, Pencil, Trash2 } from "lucide-react";
import { memo, useMemo } from "react";
import { useI18n } from "../i18n";
import {
  answersFromFormattedContent,
  questionAnswersFromInput,
  questionOutcome,
  questionsFromInput
} from "../lib/orchestration";
import type { JsonObject, ToolContext, UserContext } from "../types";
import { IconButton } from "./Common";
import { InlineQuestionEditor } from "./InlineQuestionEditor";
import { useTimelineSelected } from "./timelineSelection";
import "./QuestionTimelineCard.css";

export interface QuestionTimelineCardProps {
  item: ToolContext;
  index: number;
  answer?: UserContext;
  answerIndex?: number;
  readOnly?: boolean;
  /** Replaces the card body with the question form while true. */
  editing?: boolean;
  onEdit?: (item: ToolContext, answer?: UserContext) => void;
  onDelete?: (item: ToolContext, answer?: UserContext) => void;
  onCancelEdit?: () => void;
  onSaveQuestion?: (input: JsonObject, answerContent?: string) => void | Promise<void>;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
}

export const QuestionTimelineCard = memo(function QuestionTimelineCard({
  item,
  index,
  answer,
  answerIndex,
  readOnly = false,
  editing = false,
  onEdit,
  onDelete,
  onCancelEdit,
  onSaveQuestion,
  onOpenInsert
}: QuestionTimelineCardProps) {
  const { t } = useI18n();
  const questions = useMemo(() => questionsFromInput(item.input), [item.input]);
  const outcome = questionOutcome(item);
  // A question from before questions blocked is answered by the user message
  // after it; a newer one carries its answers in its own executed input.
  const legacy = outcome === "legacy";
  const parsedAnswers = useMemo(
    () => answer ? answersFromFormattedContent(answer.content, questions) : null,
    [answer, questions]
  );
  const blockingAnswers = useMemo(
    () => (outcome === "answered" ? questionAnswersFromInput(item.input, questions) : null),
    [item.input, outcome, questions]
  );
  const status = legacy
    ? answer
      ? t("已回答", "Answered")
      : t("未回答", "Unanswered")
    : outcome === "answered"
      ? t("已回答", "Answered")
      : outcome === "unanswered"
        ? t("未回答", "Unanswered")
        : outcome === "closed"
          ? t("已关闭", "Closed")
          : outcome === "declined"
            ? t("想先聊聊", "Wanted to chat first")
            : t("失败", "Failed");
  const afterIndex = answerIndex === undefined ? index + 1 : answerIndex + 1;
  const editingInPlace = editing && Boolean(onCancelEdit && onSaveQuestion);
  const selected = useTimelineSelected(item.id);

  return (
    <article
      className={`question-history${answer || outcome === "answered" ? " question-history--answered" : ""}${outcome === "failed" ? " question-history--error" : ""}`}
      data-context-id={item.id}
      data-context-index={index}
      data-context-end-index={afterIndex}
      data-timeline-selected={selected || undefined}
      onContextMenu={readOnly ? undefined : (event) => {
        event.preventDefault();
        const box = event.currentTarget.getBoundingClientRect();
        onOpenInsert?.(event, event.clientY > box.top + box.height / 2 ? afterIndex : index);
      }}
    >
      <header className="question-history__header">
        <CircleHelp size={14} aria-hidden="true" />
        <strong>{t("提问", "Questions")}</strong>
        <span>{status}</span>
        {!readOnly && !editingInPlace && (
          <div className="question-history__actions">
            {/* A timeline with nowhere to commit a rewritten question gets no
                pencil rather than one that opens an editor with no Save. */}
            {onEdit && legacy && (
              <IconButton
                label={answer ? t("编辑提问与回答", "Edit questions and answers") : t("编辑提问", "Edit questions")}
                onClick={() => onEdit(item, answer)}
              >
                <Pencil size={13} />
              </IconButton>
            )}
            <IconButton
              label={answer ? t("删除整条提问消息", "Delete the complete question message") : t("删除提问", "Delete questions")}
              onClick={() => onDelete?.(item, answer)}
            >
              <Trash2 size={13} />
            </IconButton>
          </div>
        )}
      </header>

      {editingInPlace ? (
        <div className="question-history__body question-history__body--editing">
          <InlineQuestionEditor
            item={item}
            answer={answer}
            onClose={onCancelEdit!}
            onSave={onSaveQuestion!}
          />
        </div>
      ) : (
      <div className="question-history__body">
        {questions.map((question, questionIndex) => (
          <section className="question-history__question" key={`${questionIndex}-${question.question}`}>
            <div className="question-history__prompt">
              <span>{question.header}</span>
              <p>{question.question}</p>
            </div>
            {parsedAnswers && (
              <div className="question-history__output">
                <small>{t("你的回答", "Your answer")}</small>
                <p>{parsedAnswers[questionIndex]}</p>
              </div>
            )}
            {blockingAnswers?.[questionIndex]?.answer && (
              <div className="question-history__output">
                <small>{t("你的回答", "Your answer")}</small>
                <p>{blockingAnswers[questionIndex].answer}</p>
              </div>
            )}
            {blockingAnswers?.[questionIndex]?.notes && (
              <div className="question-history__output">
                <small>{t("备注", "Notes")}</small>
                <p>{blockingAnswers[questionIndex].notes}</p>
              </div>
            )}
          </section>
        ))}

        {outcome === "failed" && (
          <div className="question-history__raw-output question-history__raw-output--error">
            {item.result.output}
          </div>
        )}

        {answer && !parsedAnswers && (
          <div className="question-history__raw-output">
            <small>{t("你的回答", "Your answer")}</small>
            <p>{answer.content}</p>
          </div>
        )}
      </div>
      )}

    </article>
  );
});
