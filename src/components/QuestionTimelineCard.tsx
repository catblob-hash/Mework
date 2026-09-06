import { CircleHelp, Pencil, Trash2 } from "lucide-react";
import { memo, useMemo } from "react";
import { useI18n } from "../i18n";
import {
  answersFromFormattedContent,
  questionsFromInput
} from "../lib/orchestration";
import type { ToolContext, UserContext } from "../types";
import { IconButton } from "./Common";
import "./QuestionTimelineCard.css";

export interface QuestionTimelineCardProps {
  item: ToolContext;
  index: number;
  answer?: UserContext;
  answerIndex?: number;
  readOnly?: boolean;
  onEdit?: (item: ToolContext, answer?: UserContext) => void;
  onDelete?: (item: ToolContext, answer?: UserContext) => void;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
}

export const QuestionTimelineCard = memo(function QuestionTimelineCard({
  item,
  index,
  answer,
  answerIndex,
  readOnly = false,
  onEdit,
  onDelete,
  onOpenInsert
}: QuestionTimelineCardProps) {
  const { t } = useI18n();
  const questions = useMemo(() => questionsFromInput(item.input), [item.input]);
  const parsedAnswers = useMemo(
    () => answer ? answersFromFormattedContent(answer.content, questions) : null,
    [answer, questions]
  );
  const afterIndex = answerIndex === undefined ? index + 1 : answerIndex + 1;

  return (
    <article
      className={`question-history${answer ? " question-history--answered" : ""}${item.result.success ? "" : " question-history--error"}`}
      data-context-id={item.id}
      data-context-index={index}
      data-context-end-index={afterIndex}
      onContextMenu={readOnly ? undefined : (event) => {
        event.preventDefault();
        const box = event.currentTarget.getBoundingClientRect();
        onOpenInsert?.(event, event.clientY > box.top + box.height / 2 ? afterIndex : index);
      }}
    >
      <header className="question-history__header">
        <CircleHelp size={14} aria-hidden="true" />
        <strong>{t("提问", "Questions")}</strong>
        <span>
          {answer
            ? t("已回答", "Answered")
            : item.result.success ? t("未回答", "Unanswered") : t("失败", "Failed")}
        </span>
        {!readOnly && (
          <div className="question-history__actions">
            <IconButton
              label={answer ? t("编辑提问与回答", "Edit questions and answers") : t("编辑提问", "Edit questions")}
              onClick={() => onEdit?.(item, answer)}
            >
              <Pencil size={13} />
            </IconButton>
            <IconButton
              label={answer ? t("删除整条提问消息", "Delete the complete question message") : t("删除提问", "Delete questions")}
              onClick={() => onDelete?.(item, answer)}
            >
              <Trash2 size={13} />
            </IconButton>
          </div>
        )}
      </header>

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
          </section>
        ))}

        {!item.result.success && (
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

    </article>
  );
});
