import { Check, LoaderCircle, Plus, Trash2 } from "lucide-react";
import { useId, useState } from "react";
import type { FormEvent } from "react";
import { useI18n } from "../i18n";
import {
  answersFromFormattedContent,
  formatQuestionAnswers,
  isClaudeQuestionInput,
  type QuestionItemView
} from "../lib/orchestration";
import type { JsonObject, ToolContext, UserContext } from "../types";
import { Dialog, Field, IconButton, Switch } from "./Common";
import "./QuestionEditorDialog.css";

interface QuestionOptionDraft {
  label: string;
  description: string;
  preview: string;
}

interface QuestionDraft {
  header: string;
  question: string;
  multiSelect: boolean;
  options: QuestionOptionDraft[];
}

interface OptionValidationErrors {
  label?: string;
  description?: string;
}

interface QuestionValidationErrors {
  header?: string;
  question?: string;
  options?: string;
  answer?: string;
  optionErrors: OptionValidationErrors[];
}

interface ValidationErrors {
  form?: string;
  questions: QuestionValidationErrors[];
}

export interface QuestionEditorDialogProps {
  item: ToolContext;
  answer?: UserContext;
  onSave: (input: JsonObject, answerContent?: string) => void | Promise<void>;
  onClose: () => void;
}

function recordFromValue(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function stringFromValue(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function optionDraftsFromValue(value: unknown): QuestionOptionDraft[] {
  if (!Array.isArray(value)) return [];
  return value.map((option) => {
    if (typeof option === "string") {
      return { label: option, description: "", preview: "" };
    }
    const record = recordFromValue(option);
    return {
      label: stringFromValue(record.label),
      description: stringFromValue(record.description),
      preview: stringFromValue(record.preview)
    };
  });
}

function questionDraftsFromInput(input: JsonObject): QuestionDraft[] {
  if (Array.isArray(input.questions)) {
    return input.questions.map((question) => {
      const record = recordFromValue(question);
      return {
        header: stringFromValue(record.header),
        question: stringFromValue(record.question),
        multiSelect: record.multiSelect === true,
        options: optionDraftsFromValue(record.options)
      };
    });
  }

  if (typeof input.question === "string") {
    return [{
      header: "Question",
      question: input.question,
      multiSelect: false,
      options: optionDraftsFromValue(input.options)
    }];
  }

  return [];
}

function questionViewsFromDrafts(drafts: QuestionDraft[]): QuestionItemView[] {
  return drafts.map((draft) => ({
    header: draft.header.trim(),
    question: draft.question.trim(),
    multiSelect: draft.multiSelect,
    options: draft.options.map((option) => ({
      label: option.label.trim(),
      description: option.description.trim(),
      ...(option.preview.trim() ? { preview: option.preview.trim() } : {})
    }))
  }));
}

function inputFromQuestions(questions: QuestionItemView[]): JsonObject {
  return {
    questions: questions.map((question) => ({
      header: question.header,
      question: question.question,
      multiSelect: question.multiSelect,
      options: question.options.map((option) => ({
        label: option.label,
        description: option.description,
        ...(option.preview === undefined ? {} : { preview: option.preview })
      }))
    }))
  };
}

function initialAnswers(answer: UserContext | undefined, questions: QuestionDraft[]): string[] {
  if (!answer) return [];
  const parsed = answersFromFormattedContent(answer.content, questionViewsFromDrafts(questions));
  if (parsed) return parsed;
  return questions.map((_, index) => index === 0 ? answer.content : "");
}

function hasValidationErrors(errors: ValidationErrors): boolean {
  return Boolean(
    errors.form
    || errors.questions.some((question) => (
      question.header
      || question.question
      || question.options
      || question.answer
      || question.optionErrors.some((option) => option.label || option.description)
    ))
  );
}

export function QuestionEditorDialog({
  item,
  answer,
  onSave,
  onClose
}: QuestionEditorDialogProps) {
  const { t } = useI18n();
  const formId = useId();
  const [questions, setQuestions] = useState<QuestionDraft[]>(() => questionDraftsFromInput(item.input));
  const [answers, setAnswers] = useState<string[]>(() => initialAnswers(answer, questionDraftsFromInput(item.input)));
  const [validationErrors, setValidationErrors] = useState<ValidationErrors | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const updateQuestion = (index: number, update: (question: QuestionDraft) => QuestionDraft) => {
    setQuestions((current) => current.map((question, questionIndex) => (
      questionIndex === index ? update(question) : question
    )));
    setValidationErrors(null);
    setSaveError(null);
  };

  const updateOption = (
    questionIndex: number,
    optionIndex: number,
    update: (option: QuestionOptionDraft) => QuestionOptionDraft
  ) => {
    updateQuestion(questionIndex, (question) => ({
      ...question,
      options: question.options.map((option, currentOptionIndex) => (
        currentOptionIndex === optionIndex ? update(option) : option
      ))
    }));
  };

  const addOption = (questionIndex: number) => {
    updateQuestion(questionIndex, (question) => (
      question.options.length >= 4
        ? question
        : {
            ...question,
            options: [...question.options, { label: "", description: "", preview: "" }]
          }
    ));
  };

  const removeOption = (questionIndex: number, optionIndex: number) => {
    updateQuestion(questionIndex, (question) => (
      question.options.length <= 2
        ? question
        : {
            ...question,
            options: question.options.filter((_, index) => index !== optionIndex)
          }
    ));
  };

  const validate = (): { input?: JsonObject; answerContent?: string } => {
    const errors: ValidationErrors = {
      questions: questions.map((question, questionIndex) => ({
        optionErrors: question.options.map((option) => ({
          ...(!option.label.trim()
            ? { label: t("选项名称不能为空", "Option label cannot be empty") }
            : {}),
          ...(!option.description.trim()
            ? { description: t("选项说明不能为空", "Option description cannot be empty") }
            : {})
        })),
        ...(!question.header.trim()
          ? { header: t("标题不能为空", "Header cannot be empty") }
          : Array.from(question.header.trim()).length > 12
            ? { header: t("标题不能超过 12 个字符", "Header cannot exceed 12 characters") }
            : {}),
        ...(!question.question.trim()
          ? { question: t("问题内容不能为空", "Question cannot be empty") }
          : {}),
        ...(question.options.length < 2 || question.options.length > 4
          ? { options: t("每个问题必须保留 2 至 4 个选项", "Each question must retain 2 to 4 options") }
          : {}),
        ...(answer && !answers[questionIndex]?.trim()
          ? { answer: t("回答不能为空", "Answer cannot be empty") }
          : {})
      }))
    };

    if (questions.length < 1 || questions.length > 4) {
      errors.form = t("提问消息必须包含 1 至 4 个问题", "A question message must contain 1 to 4 questions");
    }

    const questionViews = questionViewsFromDrafts(questions);
    const input = inputFromQuestions(questionViews);
    if (!isClaudeQuestionInput(input) && !hasValidationErrors(errors)) {
      errors.form = t(
        "提问参数不符合 Claude Code AskUserQuestion 格式",
        "The questions do not match the Claude Code AskUserQuestion format"
      );
    }

    setValidationErrors(errors);
    if (hasValidationErrors(errors)) return {};

    return {
      input,
      ...(answer ? { answerContent: formatQuestionAnswers(questionViews, answers) } : {})
    };
  };

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (saving) return;
    setSaveError(null);
    const validated = validate();
    if (!validated.input) return;

    setSaving(true);
    try {
      await onSave(validated.input, validated.answerContent);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  const close = () => {
    if (!saving) onClose();
  };

  return (
    <Dialog
      title={answer
        ? t("编辑提问与回答", "Edit questions and answers")
        : t("编辑提问", "Edit questions")}
      description={answer
        ? t(
            "问题与回答会作为一条完整消息保存；题目数量保持不变，每题可调整为 2 至 4 个选项。",
            "Questions and answers are saved as one complete message. The question count stays fixed; each question can have 2 to 4 options."
          )
        : t(
            "修改提问内容；题目数量保持不变，每题可调整为 2 至 4 个选项。",
            "Edit the question content. The question count stays fixed; each question can have 2 to 4 options."
          )}
      width="760px"
      onClose={close}
      footer={(
        <>
          <button type="button" className="button button--ghost" disabled={saving} onClick={close}>
            {t("取消", "Cancel")}
          </button>
          <button
            type="submit"
            form={formId}
            className="button button--primary"
            disabled={saving || questions.length === 0}
          >
            {saving ? <LoaderCircle size={15} className="spin" /> : <Check size={15} />}
            {saving ? t("正在保存…", "Saving…") : t("保存", "Save")}
          </button>
        </>
      )}
    >
      <form id={formId} className="question-editor" onSubmit={submit}>
        <div className="question-editor__summary">
          <span>{t("{count} 个问题", "{count} questions", { count: questions.length })}</span>
          <small>
            {t(
              "不能增加或删除问题；选项可在每题 2 至 4 个之间调整。",
              "Questions cannot be added or removed; each question can have 2 to 4 options."
            )}
          </small>
        </div>

        {questions.map((question, questionIndex) => {
          const questionErrors = validationErrors?.questions[questionIndex];
          const sectionId = `${formId}-question-${questionIndex}`;
          return (
            <section
              className="question-editor__question"
              aria-labelledby={sectionId}
              key={questionIndex}
            >
              <header className="question-editor__question-heading">
                <h3 id={sectionId}>
                  {t("问题 {index}", "Question {index}", { index: questionIndex + 1 })}
                </h3>
                <span>{question.header || t("未填写标题", "Untitled")}</span>
              </header>

              <div className={`question-editor__field${questionErrors?.header ? " question-editor__field--error" : ""}`}>
                <Field
                  label={t("标题", "Header")}
                  hint={questionErrors?.header ?? t("最多 12 个字符", "Up to 12 characters")}
                >
                  <input
                    className={`input${questionErrors?.header ? " input--error" : ""}`}
                    value={question.header}
                    disabled={saving}
                    aria-label={t(
                      "第 {index} 题标题",
                      "Question {index} header",
                      { index: questionIndex + 1 }
                    )}
                    aria-invalid={Boolean(questionErrors?.header)}
                    onChange={(event) => updateQuestion(questionIndex, (current) => ({
                      ...current,
                      header: event.target.value
                    }))}
                  />
                </Field>
              </div>

              <div className={`question-editor__field${questionErrors?.question ? " question-editor__field--error" : ""}`}>
                <Field label={t("问题内容", "Question")} hint={questionErrors?.question}>
                  <textarea
                    className={`input question-editor__textarea${questionErrors?.question ? " input--error" : ""}`}
                    rows={3}
                    value={question.question}
                    disabled={saving}
                    aria-label={t(
                      "第 {index} 题问题内容",
                      "Question {index} text",
                      { index: questionIndex + 1 }
                    )}
                    aria-invalid={Boolean(questionErrors?.question)}
                    onChange={(event) => updateQuestion(questionIndex, (current) => ({
                      ...current,
                      question: event.target.value
                    }))}
                  />
                </Field>
              </div>

              <div className="question-editor__switch-row">
                <div>
                  <strong>{t("允许多选", "Allow multiple selections")}</strong>
                  <small>
                    {t(
                      "开启后，用户可以选择多个预设选项。",
                      "When enabled, the user can choose multiple preset options."
                    )}
                  </small>
                </div>
                <Switch
                  checked={question.multiSelect}
                  disabled={saving}
                  label={t(
                    "第 {index} 题允许多选",
                    "Allow multiple selections for question {index}",
                    { index: questionIndex + 1 }
                  )}
                  onChange={(multiSelect) => updateQuestion(questionIndex, (current) => ({
                    ...current,
                    multiSelect
                  }))}
                />
              </div>

              <div className="question-editor__options-heading">
                <strong>{t("选项", "Options")}</strong>
                <span>
                  {t("{count} 个", "{count}", { count: question.options.length })}
                </span>
                <IconButton
                  label={t(
                    "为第 {index} 题增加选项",
                    "Add an option to question {index}",
                    { index: questionIndex + 1 }
                  )}
                  disabled={saving || question.options.length >= 4}
                  onClick={() => addOption(questionIndex)}
                >
                  <Plus size={13} />
                </IconButton>
              </div>
              {questionErrors?.options && (
                <p className="question-editor__section-error">{questionErrors.options}</p>
              )}

              <div className="question-editor__options">
                {question.options.map((option, optionIndex) => {
                  const optionErrors = questionErrors?.optionErrors[optionIndex];
                  return (
                    <div className="question-editor__option" key={optionIndex}>
                      <span className="question-editor__option-index">
                        {t("选项 {index}", "Option {index}", { index: optionIndex + 1 })}
                      </span>
                      <IconButton
                        className="question-editor__option-remove"
                        label={t(
                          "删除第 {question} 题的选项 {option}",
                          "Remove option {option} from question {question}",
                          { question: questionIndex + 1, option: optionIndex + 1 }
                        )}
                        disabled={saving || question.options.length <= 2}
                        onClick={() => removeOption(questionIndex, optionIndex)}
                      >
                        <Trash2 size={12} />
                      </IconButton>
                      <div className={`question-editor__field${optionErrors?.label ? " question-editor__field--error" : ""}`}>
                        <Field label={t("名称", "Label")} hint={optionErrors?.label}>
                          <input
                            className={`input${optionErrors?.label ? " input--error" : ""}`}
                            value={option.label}
                            disabled={saving}
                            aria-label={t(
                              "第 {question} 题选项 {option} 名称",
                              "Question {question} option {option} label",
                              { question: questionIndex + 1, option: optionIndex + 1 }
                            )}
                            aria-invalid={Boolean(optionErrors?.label)}
                            onChange={(event) => updateOption(
                              questionIndex,
                              optionIndex,
                              (current) => ({ ...current, label: event.target.value })
                            )}
                          />
                        </Field>
                      </div>
                      <div className={`question-editor__field${optionErrors?.description ? " question-editor__field--error" : ""}`}>
                        <Field label={t("说明", "Description")} hint={optionErrors?.description}>
                          <input
                            className={`input${optionErrors?.description ? " input--error" : ""}`}
                            value={option.description}
                            disabled={saving}
                            aria-label={t(
                              "第 {question} 题选项 {option} 说明",
                              "Question {question} option {option} description",
                              { question: questionIndex + 1, option: optionIndex + 1 }
                            )}
                            aria-invalid={Boolean(optionErrors?.description)}
                            onChange={(event) => updateOption(
                              questionIndex,
                              optionIndex,
                              (current) => ({ ...current, description: event.target.value })
                            )}
                          />
                        </Field>
                      </div>
                      <div className="question-editor__field question-editor__option-preview">
                        <Field
                          label={t("预览（可选）", "Preview (optional)")}
                          hint={t("保留代码或长文本的原始换行。", "Line breaks in code or long text are preserved.")}
                        >
                          <textarea
                            className="input question-editor__textarea question-editor__textarea--compact"
                            rows={2}
                            value={option.preview}
                            disabled={saving}
                            aria-label={t(
                              "第 {question} 题选项 {option} 预览",
                              "Question {question} option {option} preview",
                              { question: questionIndex + 1, option: optionIndex + 1 }
                            )}
                            onChange={(event) => updateOption(
                              questionIndex,
                              optionIndex,
                              (current) => ({ ...current, preview: event.target.value })
                            )}
                          />
                        </Field>
                      </div>
                    </div>
                  );
                })}
              </div>

              {answer && (
                <div className={`question-editor__answer${questionErrors?.answer ? " question-editor__field--error" : ""}`}>
                  <Field
                    label={t("对应回答", "Answer")}
                    hint={questionErrors?.answer ?? t(
                      "保存时会使用修改后的问题内容重新建立对应关系。",
                      "Saving rebuilds the answer mapping with the edited question text."
                    )}
                  >
                    <textarea
                      className={`input question-editor__textarea${questionErrors?.answer ? " input--error" : ""}`}
                      rows={3}
                      value={answers[questionIndex] ?? ""}
                      disabled={saving}
                      aria-label={t(
                        "第 {index} 题回答",
                        "Answer to question {index}",
                        { index: questionIndex + 1 }
                      )}
                      aria-invalid={Boolean(questionErrors?.answer)}
                      onChange={(event) => {
                        const value = event.target.value;
                        setAnswers((current) => questions.map((_, currentIndex) => (
                          currentIndex === questionIndex ? value : current[currentIndex] ?? ""
                        )));
                        setValidationErrors(null);
                        setSaveError(null);
                      }}
                    />
                  </Field>
                </div>
              )}
            </section>
          );
        })}

        {questions.length === 0 && (
          <p className="question-editor__empty" role="alert">
            {t(
              "这条消息没有可编辑的问题，无法保存。",
              "This message has no editable questions and cannot be saved."
            )}
          </p>
        )}
        {validationErrors && hasValidationErrors(validationErrors) && (
          <div className="question-editor__alert" role="alert">
            {validationErrors.form
              ?? t("请修正标记的字段后再保存。", "Fix the marked fields before saving.")}
          </div>
        )}
        {saveError && (
          <div className="question-editor__alert" role="alert">
            {t("保存失败：{error}", "Failed to save: {error}", { error: saveError })}
          </div>
        )}
      </form>
    </Dialog>
  );
}
