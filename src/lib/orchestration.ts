import type { ContextItem, JsonObject, JsonValue, ToolContext } from "../types";

export type TodoStatus = "pending" | "in_progress" | "completed";

export interface TodoItemView {
  /** Stable Claude task id. Legacy `todo` snapshots do not have one. */
  id?: string;
  content: string;
  status: TodoStatus;
  description?: string;
  activeForm?: string;
  blocks?: string[];
  blockedBy?: string[];
  owner?: string;
  metadata?: JsonObject;
}

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

export interface PendingQuestion {
  context: ToolContext;
  questions: QuestionItemView[];
  /** Legacy convenience fields for callers that only need the first question. */
  question: string;
  options: QuestionOptionView[];
}

export interface AgentStatus {
  todo: TodoItemView[] | null;
}

const orchestrationToolNames = new Set([
  "subagent",
  "subagent_update",
  "structured_output",
  "subagent_activity",
  "update",
  "ask_user",
  "todo",
  "agent_spawn",
  "agent_send",
  "send_message",
  "followup_task",
  "task_wait",
  "task_list",
  "workflow",
  "workflow_step"
]);

/**
 * Orchestration calls are host-owned protocol messages.  Recognise them by
 * their stable wire name as well as by catalog metadata so an upgraded or
 * temporarily stale renderer catalog cannot hide an actionable question.
 */
export function isOrchestrationToolName(toolName: string): boolean {
  return orchestrationToolNames.has(toolName);
}

type UnknownRecord = Record<string, unknown>;

interface ParsedTaskCreate {
  id: string;
  subject: string;
}

const TASK_UPDATE_PATCH_FIELDS = [
  "status",
  "subject",
  "description",
  "activeForm",
  "addBlocks",
  "addBlockedBy",
  "owner",
  "metadata"
] as const;

function record(value: unknown): UnknownRecord | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as UnknownRecord
    : null;
}

function parsedResult(item: ToolContext): UnknownRecord | null {
  try {
    return record(JSON.parse(item.result.output));
  } catch {
    return null;
  }
}

function isSettledSuccessfulTool(
  context: ContextItem,
  toolName: string
): context is ToolContext {
  return context.kind === "tool"
    && context.toolName === toolName
    && context.result.success
    && (!context.streaming || context.streamStatus === "completed");
}

/**
 * A settled, successful call of the merged state tool's given action.
 *
 * The tool name alone does not identify the operation: `todo` multiplexes on a
 * required `action`, exactly like `playwright`. Every replay predicate below
 * reads the same discriminator the host executor dispatched on.
 */
function isStateToolAction(
  context: ContextItem,
  toolName: "todo",
  action: string
): context is ToolContext {
  return isSettledSuccessfulTool(context, toolName) && context.input.action === action;
}

function parsedTaskCreate(context: ContextItem): ParsedTaskCreate | null {
  if (!isStateToolAction(context, "todo", "create")) return null;
  const task = record(parsedResult(context)?.task);
  const id = typeof task?.id === "string" ? task.id.trim() : "";
  const subject = typeof task?.subject === "string" ? task.subject.trim() : "";
  return id && subject ? { id, subject } : null;
}

/** Stable task id returned by a successful, settled `todo create` call. */
export function taskIdFromTaskCreate(context: ContextItem): string | null {
  return parsedTaskCreate(context)?.id ?? null;
}

/** Whether a timeline item is a `todo update` directed at the given task. */
export function isTaskUpdateFor(context: ContextItem, taskId: string): boolean {
  return context.kind === "tool"
    && context.toolName === "todo"
    && context.input.action === "update"
    && typeof context.input.taskId === "string"
    && context.input.taskId.trim() === taskId;
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return [...new Set(value.flatMap((entry) => (
    typeof entry === "string" && entry.trim() ? [entry.trim()] : []
  )))];
}

function jsonObject(value: unknown): JsonObject | null {
  const object = record(value);
  return object ? object as JsonObject : null;
}

function taskUpdateTarget(context: ContextItem): string | null {
  if (!isStateToolAction(context, "todo", "update")) return null;
  const taskId = typeof context.input.taskId === "string" ? context.input.taskId.trim() : "";
  const output = parsedResult(context);
  const outputTaskId = typeof output?.taskId === "string" ? output.taskId.trim() : "";
  if (!taskId || output?.success !== true || outputTaskId !== taskId) return null;
  const expectedFields = TASK_UPDATE_PATCH_FIELDS.filter((field) => (
    Object.hasOwn(context.input, field)
  ));
  if (!expectedFields.length || !Array.isArray(output.updatedFields)) return null;
  const outputFields = output.updatedFields;
  if (
    outputFields.some((field) => typeof field !== "string")
    || new Set(outputFields).size !== outputFields.length
    || outputFields.length !== expectedFields.length
    || expectedFields.some((field) => !outputFields.includes(field))
  ) return null;
  return taskId;
}

function mergeMetadata(current: JsonObject | undefined, patch: JsonObject): JsonObject {
  const next: JsonObject = { ...(current ?? {}) };
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete next[key];
    else next[key] = value as JsonValue;
  }
  return next;
}

function addUnique(current: string[] | undefined, additions: string[]): string[] {
  return [...new Set([...(current ?? []), ...additions])];
}

function removeTaskReferences(tasks: Map<string, TodoItemView>, taskId: string): void {
  for (const [id, task] of tasks) {
    const blocks = task.blocks?.filter((candidate) => candidate !== taskId);
    const blockedBy = task.blockedBy?.filter((candidate) => candidate !== taskId);
    if (
      blocks?.length === task.blocks?.length
      && blockedBy?.length === task.blockedBy?.length
    ) continue;
    tasks.set(id, { ...task, blocks, blockedBy });
  }
}

function applyTaskUpdate(tasks: Map<string, TodoItemView>, context: ToolContext): void {
  const taskId = taskUpdateTarget(context);
  if (!taskId) return;
  const current = tasks.get(taskId);
  if (!current) return;

  const status = context.input.status;
  if (
    status !== undefined
    && status !== "pending"
    && status !== "in_progress"
    && status !== "completed"
    && status !== "deleted"
  ) return;

  for (const field of ["subject", "description", "activeForm", "owner"] as const) {
    if (
      Object.hasOwn(context.input, field)
      && (
        typeof context.input[field] !== "string"
        || !context.input[field].trim()
      )
    ) return;
  }
  for (const field of ["addBlocks", "addBlockedBy"] as const) {
    if (
      Object.hasOwn(context.input, field)
      && (
        !Array.isArray(context.input[field])
        || context.input[field].some((id) => typeof id !== "string" || !id.trim())
      )
    ) return;
  }
  if (
    Object.hasOwn(context.input, "metadata")
    && !jsonObject(context.input.metadata)
  ) return;

  if (status === "deleted") {
    tasks.delete(taskId);
    removeTaskReferences(tasks, taskId);
    return;
  }

  const addBlocks = stringArray(context.input.addBlocks)
    .filter((id) => id !== taskId && tasks.has(id));
  const addBlockedBy = stringArray(context.input.addBlockedBy)
    .filter((id) => id !== taskId && tasks.has(id));
  const metadataPatch = jsonObject(context.input.metadata);
  const next: TodoItemView = {
    ...current,
    ...(typeof context.input.subject === "string"
      ? { content: context.input.subject.trim() }
      : {}),
    ...(typeof context.input.description === "string"
      ? { description: context.input.description.trim() }
      : {}),
    ...(typeof context.input.activeForm === "string"
      ? { activeForm: context.input.activeForm.trim() }
      : {}),
    ...(typeof context.input.owner === "string"
      ? { owner: context.input.owner.trim() }
      : {}),
    ...(status ? { status } : {}),
    ...(addBlocks.length ? { blocks: addUnique(current.blocks, addBlocks) } : {}),
    ...(addBlockedBy.length ? { blockedBy: addUnique(current.blockedBy, addBlockedBy) } : {}),
    ...(metadataPatch ? { metadata: mergeMetadata(current.metadata, metadataPatch) } : {})
  };
  tasks.set(taskId, next);

  for (const blockedId of addBlocks) {
    const blocked = tasks.get(blockedId);
    if (blocked) {
      tasks.set(blockedId, {
        ...blocked,
        blockedBy: addUnique(blocked.blockedBy, [taskId])
      });
    }
  }
  for (const blockerId of addBlockedBy) {
    const blocker = tasks.get(blockerId);
    if (blocker) {
      tasks.set(blockerId, {
        ...blocker,
        blocks: addUnique(blocker.blocks, [taskId])
      });
    }
  }
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

export function questionFromInput(input: JsonObject): QuestionItemView {
  return questionsFromInput(input)[0] ?? {
    question: "",
    header: "Question",
    options: [],
    multiSelect: false
  };
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
/** The status roll-up heading of the two built-in profiles (`task.wait_status_heading`).
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
const HOST_AUTHORED_USER_CONTEXT_PREFIXES = [
  "ctx_agent-result_",
  "ctx_agent-message_",
  "ctx_structured-output-nudge_"
] as const;

export function isHostAuthoredUserContext(context: ContextItem): boolean {
  return (
    context.kind === "user"
    && HOST_AUTHORED_USER_CONTEXT_PREFIXES.some((prefix) => context.id.startsWith(prefix))
  );
}

function isAnsweredBoundary(context: ContextItem): boolean {
  // Only a real user reply closes a pending question; assistant replies and
  // host-authored user contexts do not form an answer boundary.
  return context.kind === "user" && !isHostAuthoredUserContext(context);
}

/**
 * The pending question is the trailing successful `ask_user` call with no real
 * user reply after it. A successful `ask_user` result is by construction the
 * host's "asked, paused" receipt (a rejected question fails), so the marker text
 * itself — which follows the prompt profile — is not matched. Skip same-round
 * leftovers, task-result folds, and assistant replies because none constitutes
 * an answer.
 */
export function findPendingQuestion(contexts: ContextItem[]): PendingQuestion | null {
  for (let index = contexts.length - 1; index >= 0; index -= 1) {
    const context = contexts[index];
    if (context.kind === "system" || context.kind === "reasoning" || context.kind === "assistant") continue;
    if (context.kind === "tool") {
      if (context.toolName === "ask_user" && context.result.success) {
        const questions = questionsFromInput(context.input);
        const first = questions[0] ?? {
          question: "",
          header: "Question",
          options: [],
          multiSelect: false
        };
        return { context, questions, ...first };
      }
      continue;
    }
    if (isAnsweredBoundary(context)) return null;
  }
  return null;
}

/**
 * Replays the host-owned task event log in timeline order.
 *
 * `todo create` obtains its stable id from structured tool
 * results. Updates are patches, so removing any update context and replaying
 * naturally restores the state immediately before that message.
 */
export function deriveAgentStatus(contexts: ContextItem[]): AgentStatus {
  const tasks = new Map<string, TodoItemView>();
  let taskStateSeen = false;

  for (const context of contexts) {
    if (context.kind !== "tool") continue;
    if (!context.result.success) continue;
    if (context.streaming && context.streamStatus !== "completed") continue;
    if (context.toolName !== "todo") continue;
    const action = context.input.action;

    if (context.toolName === "todo" && action === "create") {
      const created = parsedTaskCreate(context);
      if (!created || tasks.has(created.id)) continue;
      const metadata = jsonObject(context.input.metadata);
      taskStateSeen = true;
      tasks.set(created.id, {
        id: created.id,
        content: created.subject,
        status: "pending",
        description: typeof context.input.description === "string"
          ? context.input.description
          : "",
        ...(typeof context.input.activeForm === "string" && context.input.activeForm.trim()
          ? { activeForm: context.input.activeForm.trim() }
          : {}),
        blocks: [],
        blockedBy: [],
        ...(metadata ? { metadata: { ...metadata } } : {})
      });
      continue;
    }

    if (context.toolName === "todo" && action === "update") {
      applyTaskUpdate(tasks, context);
    }
  }

  return {
    todo: taskStateSeen ? [...tasks.values()] : null
  };
}
