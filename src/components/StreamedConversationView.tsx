import { useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { ModelRunController } from "../lib/modelRunController";
import { liveReasoningFromModelRun, mergeContextsWithStreamingRun } from "../lib/runContexts";
import { materializeRunTurnContexts } from "../lib/conversationTurns";
import type { ConversationTurn } from "../lib/conversationTurns";
import { deriveWorkflowProgress } from "../lib/workflowProgress";
import type { WorkflowProgressView } from "../lib/workflowProgress";
import { deriveWorkflowItems } from "../lib/taskContainer";
import type { TaskContainerMessages } from "../lib/taskContainer";
import { deriveWorkflowRun } from "../lib/workflowRuns";
import type { WorkflowRunView } from "../lib/workflowRuns";
import {
  deriveSubagentViews,
  findOpenableSubagentView,
  graftExternalStepBodies
} from "../lib/subagents";
import type { SubagentView, SubagentViewMessages } from "../lib/subagents";
import { ConversationView } from "./ConversationView";
import type { ConversationViewProps } from "./ConversationView";
import { SubagentPanel } from "./SubagentPanel";
import type { ContextItem, SubagentRunRecord, ToolDescriptor } from "../types";

const NO_TURNS: ConversationTurn[] = [];
const NO_AGENTS: SubagentView[] = [];

type Translate = ReturnType<typeof useI18n>["t"];

/** Localized subagent-view copy; the App summary selector and drawer use the same derivation. */
export function subagentViewMessages(t: Translate): SubagentViewMessages {
  return {
    fallbackLabel: (index) => t("子代理 {index}", "Subagent {index}", { index }),
    workflowStepLabel: t("工作流步骤", "Workflow step"),
    missingTask: t("未提供任务说明", "No task description provided"),
    updateReturned: t("状态已返回给主智能体", "Status returned to the primary agent"),
    running: t("正在工作", "Working"),
    interrupted: t("子代理已中断", "Subagent interrupted"),
    failed: t("子代理已失败", "Subagent failed"),
    stopped: t("子代理已停止", "Subagent stopped"),
    roundLimit: t("子代理已达轮次上限", "Subagent hit its round limit"),
    completed: t("子代理已完成", "Subagent completed")
  };
}

/** Persistent conversation contexts plus the active run's streaming projection. */
interface StreamedConversation {
  id: string;
  contexts: ContextItem[];
}

function useRenderedContexts(
  conversation: StreamedConversation,
  modelRunController: ModelRunController
) {
  const runs = useSyncExternalStore(modelRunController.subscribe, modelRunController.current);
  const run = runs[conversation.id];
  const renderedContexts = useMemo<ContextItem[]>(() => {
    if (!run) return conversation.contexts;
    return mergeContextsWithStreamingRun(conversation.contexts, run);
  }, [conversation, run]);
  return { run, renderedContexts };
}

export interface StreamedConversationViewProps extends Omit<
  ConversationViewProps,
  "contexts" | "turns" | "streaming" | "thinking" | "retryNotice"
  | "workflowRunByCall" | "timelineId"
> {
  conversation: StreamedConversation;
  conversationTurns?: ConversationTurn[];
  modelRunController: ModelRunController;
  /**
   * The conversation's agent roster, as App already derives it for the task
   * container. Passed in rather than re-derived here so both surfaces describe
   * the same run, and so a text delta does not rebuild the whole tree: this
   * projection only changes when a nameplate field does.
   */
  agents?: SubagentView[];
  /** Localized task-row copy, shared with the task container's own derivation. */
  taskMessages: TaskContainerMessages;
}

/**
 * Keep fine-grained model-stream subscriptions in this projection layer so each flush rerenders only this subtree; App consumes only the coarse `modelRunController` summary.
 */
export function StreamedConversationView({
  conversation,
  conversationTurns = NO_TURNS,
  modelRunController,
  agents = NO_AGENTS,
  taskMessages,
  ...viewProps
}: StreamedConversationViewProps) {
  const { t } = useI18n();
  const { run, renderedContexts } = useRenderedContexts(conversation, modelRunController);
  // Derived from run state, not from the rendered contexts: encrypted reasoning
  // with no summary is deliberately absent from them while it streams.
  const liveReasoning = useMemo(() => liveReasoningFromModelRun(run), [run]);
  const renderedTurns = useMemo<ConversationTurn[]>(() => (
    run
      ? materializeRunTurnContexts(conversationTurns, renderedContexts, run.requestId)
      : conversationTurns
  ), [conversationTurns, renderedContexts, run]);
  /**
   * Folded progress views for every workflow call streaming in this
   * conversation, keyed by call id so the timeline node can look itself up.
   *
   * Only live runs appear here: a settled run's entries went away with its
   * `ModelRunState`. The card does not depend on them — it is built from the
   * roster, which outlives the run — so this only contributes the plan slots
   * that never got an agent, and the run's narration lines.
   */
  const workflowProgressByCall = useMemo<Record<string, WorkflowProgressView>>(() => {
    if (!run) return {};
    const messages = {
      fallbackStepLabel: (index: number) => t("步骤 {number}", "Step {number}", { number: index + 1 }),
      unphasedHeading: t("未分组", "Ungrouped"),
      cachedBadge: t("已缓存", "Cached"),
      blockedBadge: t("等待中", "Waiting"),
      skippedBadge: t("已跳过", "Skipped"),
      progressLabel: (done: number, total: number) => t(
        "工作流进度：{total} 步中已完成 {done} 步",
        "Workflow progress: {done} of {total} steps completed",
        { done, total }
      )
    };
    return Object.fromEntries(
      Object.entries(run.workflowProgressByCall).map(([callId, entries]) => [
        callId,
        deriveWorkflowProgress(entries, messages)
      ])
    );
  }, [run, t]);

  /**
   * One card view per workflow call. A run is registered under every call id
   * that touched it, so the timeline node finds itself whichever call the
   * projection settled on.
   */
  const workflowRunByCall = useMemo<Record<string, WorkflowRunView>>(() => {
    const byCall: Record<string, WorkflowRunView> = {};
    for (const item of deriveWorkflowItems(agents, taskMessages, Date.now())) {
      const progress = item.agent.callIds
        .map((callId) => workflowProgressByCall[callId])
        .find(Boolean) ?? null;
      const view = deriveWorkflowRun(item, progress);
      for (const callId of item.agent.callIds) byCall[callId] = view;
    }
    return byCall;
  }, [agents, taskMessages, workflowProgressByCall]);

  return (
    <ConversationView
      {...viewProps}
      timelineId={conversation.id}
      contexts={renderedContexts}
      turns={renderedTurns}
      streaming={Boolean(run)}
      thinking={liveReasoning}
      retryNotice={run?.retry ?? null}
      workflowRunByCall={workflowRunByCall}
    />
  );
}

export interface StreamedSubagentPanelProps {
  conversation: StreamedConversation;
  modelRunController: ModelRunController;
  /** On-demand loaded workflow step bodies, keyed `${conversationId}/${runId}/${stepIndex}`. */
  externalStepBodies: Record<string, SubagentRunRecord | null>;
  selectedSubagentId: string | null;
  tools: ToolDescriptor[];
  onSelectAgent: (agentId: string) => void;
  onClose: () => void;
  /** A bottom-docked confirmation card must remain visible and answerable after navigation reveals this page while the conversation panel is hidden. */
  dock?: ReactNode;
}

/**
 * Streaming-subscription host for the subagent drawer. It exists only while open and derives complete views with live text; App retains a trimmed summary for gating and chips.
 */
export function StreamedSubagentPanel({
  conversation,
  modelRunController,
  externalStepBodies,
  selectedSubagentId,
  tools,
  onSelectAgent,
  onClose,
  dock
}: StreamedSubagentPanelProps) {
  const { t } = useI18n();
  const { renderedContexts } = useRenderedContexts(conversation, modelRunController);
  /**
   * The context tree the drawer views are derived from: the rendered timeline
   * with every on-demand loaded workflow step body grafted back onto its
   * externalized context. Untouched branches keep their identity.
   */
  const sourceContexts = useMemo(() => graftExternalStepBodies(
    renderedContexts,
    (ref) => externalStepBodies[`${conversation.id}/${ref.runId}/${ref.stepIndex}`]
  ), [conversation.id, renderedContexts, externalStepBodies]);
  const subagents = useMemo(
    () => deriveSubagentViews(sourceContexts, subagentViewMessages(t)),
    [sourceContexts, t]
  );
  const agent = findOpenableSubagentView(subagents, selectedSubagentId);
  if (!agent) return null;
  return (
    <div className="main-pane__subagent">
      <SubagentPanel
        agent={agent}
        agents={subagents}
        tools={tools}
        onSelectAgent={onSelectAgent}
        onClose={onClose}
      />
      {dock}
    </div>
  );
}
