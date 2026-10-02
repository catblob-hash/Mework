import type { ContextItem, JsonObject, JsonValue, ToolContext } from "../types";

export interface QuestionItemView {
  question: string;
  header: string;
  options: QuestionOptionView[];
  multiSelect: boolean;
}

export interface QuestionOptionView {
  label: string;
  description: string;
  preview?: string;
}

function optionsFromValue(value: unknown): QuestionOptionView[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((option) => {
    if (typeof option === "string" && option.trim()) {
      return [{ label: option.trim(), description: "" }];
    }
    if (option === null || typeof option !== "object" || Array.isArray(option)) return [];
    const record = option as { [key: string]: unknown };
    if (typeof record.label !== "string" || !record.label.trim()) return [];
    return [{
      label: record.label.trim(),
      description: typeof record.description === "string" ? record.description.trim() : "",
      ...(typeof record.preview === "string" && record.preview.trim()
        ? { preview: record.preview }
        : {})
    }];
  });
}

export function questionsFromInput(input: JsonObject): QuestionItemView[] {
  if (Array.isArray(input.questions)) {
    const questions = input.questions.flatMap((value) => {
      if (value === null || typeof value !== "object" || Array.isArray(value)) return [];
      const item = value as { [key: string]: unknown };
      if (typeof item.question !== "string" || !item.question.trim()) return [];
      return [{
        question: item.question.trim(),
        header: typeof item.header === "string" && item.header.trim() ? item.header.trim() : "Question",
        options: optionsFromValue(item.options),
        multiSelect: item.multiSelect === true
      }];
    });
    if (questions.length) return questions;
  }

  // Keep old persisted calls readable after the multi-question protocol ships.
  const question = typeof input.question === "string" ? input.question.trim() : "";
  return question ? [{
    question,
    header: "Question",
    options: optionsFromValue(input.options),
    multiSelect: false
  }] : [];
}

export function isClaudeQuestionInput(input: JsonObject): boolean {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) {
    return false;
  }
  return input.questions.every((value) => {
    if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
    const item = value as { [key: string]: unknown };
    if (typeof item.question !== "string" || !item.question.trim()) return false;
    if (
      typeof item.header !== "string"
      || !item.header.trim()
      || Array.from(item.header.trim()).length > 12
    ) return false;
    if (typeof item.multiSelect !== "boolean") return false;
    if (!Array.isArray(item.options) || item.options.length < 2 || item.options.length > 4) return false;
    return item.options.every((option) => {
      if (option === null || typeof option !== "object" || Array.isArray(option)) return false;
      const record = option as { [key: string]: unknown };
      return (
        typeof record.label === "string"
        && Boolean(record.label.trim())
        && typeof record.description === "string"
        && Boolean(record.description.trim())
        && (record.preview === undefined || typeof record.preview === "string")
      );
    });
  });
}

export function formatQuestionAnswers(
  questions: QuestionItemView[],
  answers: string[]
): string {
  const normalized = answers.map((answer) => answer.trim());
  const pairs = questions.map((question, index) => (
    `${JSON.stringify(question.question)}=${JSON.stringify(normalized[index] ?? "")}`
  ));
  return `User has answered your questions: ${pairs.join(", ")}`;
}

export function answersFromFormattedContent(
  content: string,
  questions: QuestionItemView[]
): string[] | null {
  if (!content.startsWith("User has answered your questions: ")) return null;
  const answerMap = new Map<string, string>();
  const pairPattern = /("(?:\\.|[^"\\])*")=("(?:\\.|[^"\\])*")/g;
  for (const match of content.matchAll(pairPattern)) {
    try {
      answerMap.set(JSON.parse(match[1]) as string, JSON.parse(match[2]) as string);
    } catch {
      return null;
    }
  }
  if (!answerMap.size) return null;
  return questions.map((question) => answerMap.get(question.question) ?? "");
}

/** One task-result envelope section. */
export interface WaitEnvelopeView {
  agent: string;
  status: string;
  body: string;
}

export interface WaitOutputView {
  envelopes: WaitEnvelopeView[];
  /** Trailing host-appended status roll-up, if present. */
  statusLine: string;
  /** Text preceding the first envelope — the timeout / nothing-to-drain notice. */
  notice: string;
}

const WAIT_ENVELOPE_HEADER = /^\[([^\]\r\n]+?)\s*·\s*([^\]\r\n]+?)\]$/;
/** The status roll-up heading of the built-in profile (`task.wait_status_heading`),
 * and the Chinese one older transcripts carry.
 * A custom profile that words it differently folds the roll-up into the last
 * envelope body; the card still renders, only less structured. */
const WAIT_STATUS_LINE = /^(当前状态：|Current status:)/; // i18n-audit-ignore: parses localized backend output

/**
 * Splits `task_wait` output into envelopes.
 *
 * The host emits an optional leading notice, blank-line-separated sections with
 * bracketed headers and bodies, then an optional status roll-up. Preserve
 * unrecognized text as `notice` so callers can fall back to raw output.
 */
export function parseWaitOutput(output: string): WaitOutputView {
  const envelopes: WaitEnvelopeView[] = [];
  const noticeLines: string[] = [];
  let statusLine = "";
  let currentAgent = "";
  let currentStatus = "";
  let open = false;
  let bodyLines: string[] = [];

  const flush = () => {
    if (!open) return;
    envelopes.push({ agent: currentAgent, status: currentStatus, body: bodyLines.join("\n").trim() });
    open = false;
    bodyLines = [];
  };

  for (const raw of output.split("\n")) {
    const line = raw.trim();
    const header = WAIT_ENVELOPE_HEADER.exec(line);
    if (header) {
      flush();
      currentAgent = header[1].trim();
      currentStatus = header[2].trim();
      open = true;
      continue;
    }
    if (WAIT_STATUS_LINE.test(line)) {
      flush();
      statusLine = line;
      continue;
    }
    if (open) {
      bodyLines.push(raw.trimEnd());
      continue;
    }
    if (!statusLine && line) noticeLines.push(line);
  }
  flush();

  return { envelopes, statusLine, notice: noticeLines.join("\n") };
}

/**
 * Host-authored user contexts include agent-mailbox prose, structured-output
 * nudges, and legacy task-result folds. They appear in the timeline but do not
 * answer pending questions.
 *
 * Keep `ctx_agent-result_` to exclude persisted legacy folds represented as
 * user contexts; current task-result folds are tool contexts.
 */
const STRUCTURED_OUTPUT_NUDGE_PREFIX = "ctx_structured-output-nudge_";
const HOST_AUTHORED_USER_CONTEXT_PREFIXES = [
  "ctx_agent-result_",
  "ctx_agent-message_",
  STRUCTURED_OUTPUT_NUDGE_PREFIX
] as const;

/** The reminder a schema-bound child gets mid-run when a round ends without its result. */
export function isStructuredOutputNudge(context: ContextItem): boolean {
  return context.kind === "user" && context.id.startsWith(STRUCTURED_OUTPUT_NUDGE_PREFIX);
}

export function isHostAuthoredUserContext(context: ContextItem): boolean {
  return (
    context.kind === "user"
    && HOST_AUTHORED_USER_CONTEXT_PREFIXES.some((prefix) => context.id.startsWith(prefix))
  );
}

/** The result of a question card the user closed without answering (host `ask_user.rs`). */
export const ASK_USER_CLOSED_OUTPUT = "The user closed the question card without answering.";
/** Claude Code's result for a card submitted with nothing answered. */
export const ASK_USER_NO_ANSWER_OUTPUT = "The user did not answer the questions.";
const ASK_USER_ANSWERED_PREFIXES = [
  "Your questions have been answered: ",
  "The user answered: "
] as const;
const ASK_USER_DECLINED_PREFIX = "The user doesn't want to proceed with this tool use.";

/**
 * How an `ask_user` call ended.
 *
 * `legacy` is a call from before questions blocked: its result is only the
 * "asked, paused" receipt, and the answer — if any — is the next real user
 * message. Every newer call carries its outcome in its own result.
 */
export type QuestionOutcome = "answered" | "unanswered" | "closed" | "declined" | "failed" | "legacy";

export function questionOutcome(context: ToolContext): QuestionOutcome {
  const output = context.result.output;
  if (!context.result.success) {
    return output.startsWith(ASK_USER_DECLINED_PREFIX) ? "declined" : "failed";
  }
  if (output === ASK_USER_CLOSED_OUTPUT) return "closed";
  if (output === ASK_USER_NO_ANSWER_OUTPUT) return "unanswered";
  if (
    isJsonRecord(context.input.answers)
    || ASK_USER_ANSWERED_PREFIXES.some((prefix) => output.startsWith(prefix))
  ) {
    return "answered";
  }
  return "legacy";
}

/** A settled pre-blocking `ask_user` receipt, whose answer is the next user message. */
export function isLegacyPausedQuestion(context: ContextItem): context is ToolContext {
  return context.kind === "tool"
    && context.toolName === "ask_user"
    && !context.streaming
    && questionOutcome(context) === "legacy";
}

export interface QuestionAnswerView {
  answer?: string;
  notes?: string;
}

/**
 * The answers a blocking `ask_user` call ran with, read from the `answers` and
 * `annotations` the host merged into its input (keyed by question text, as in
 * Claude Code), one slot per question.
 */
export function questionAnswersFromInput(
  input: JsonObject,
  questions: QuestionItemView[]
): QuestionAnswerView[] {
  const answers = isJsonRecord(input.answers) ? input.answers : {};
  const annotations = isJsonRecord(input.annotations) ? input.annotations : {};
  return questions.map((question) => {
    const answer = answers[question.question];
    const annotation = annotations[question.question];
    const notes = isJsonRecord(annotation) && typeof annotation.notes === "string"
      ? annotation.notes
      : undefined;
    return {
      ...(typeof answer === "string" && answer ? { answer } : {}),
      ...(notes ? { notes } : {})
    };
  });
}

/**
 * Claude Code's multi-select join: an item containing `", "` or a quote is
 * written as a JSON string, the rest verbatim.
 */
export function joinMultiSelectAnswer(items: string[]): string {
  return items
    .map((item) => (item.includes(", ") || item.includes('"') ? JSON.stringify(item) : item))
    .join(", ");
}

function isJsonRecord(value: JsonValue | undefined): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
