import { useCallback, useRef } from "react";
import type { ReactNode } from "react";
import { ContextStream } from "./ContextStream";
import type { ContextStreamProps } from "./ContextStream";

export interface ConversationViewProps extends Omit<ContextStreamProps, "readOnly"> {
  className?: string;
  /** Controls only mutation affordances; message and tool rendering stays identical. */
  editable?: boolean;
  beforeTimeline?: ReactNode;
  composer?: ReactNode;
}

function useStableCallback<Args extends unknown[]>(callback: ((...args: Args) => void) | undefined) {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;
  return useCallback((...args: Args) => callbackRef.current?.(...args), []);
}

/**
 * The single conversation surface used by both the primary and child sessions.
 * A read-only child is the same surface with mutation controls and composer
 * physically omitted, rather than a separate transcript implementation.
 */
export function ConversationView({
  className,
  editable = true,
  beforeTimeline,
  composer,
  ...timelineProps
}: ConversationViewProps) {
  const stableOnEdit = useStableCallback(timelineProps.onEdit);
  const stableOnDelete = useStableCallback(timelineProps.onDelete);
  const stableOnEditQuestion = useStableCallback(timelineProps.onEditQuestion);
  const stableOnDeleteQuestion = useStableCallback(timelineProps.onDeleteQuestion);
  const stableOnBranchFrom = useStableCallback(timelineProps.onBranchFrom);
  const stableOnSelectBranch = useStableCallback(timelineProps.onSelectBranch);
  const stableOnInsert = useStableCallback(timelineProps.onInsert);
  const stableOnOpenSubagent = useStableCallback(timelineProps.onOpenSubagent);
  const stableOnOpenWorkflowRun = useStableCallback(timelineProps.onOpenWorkflowRun);
  const stableOnToggleTurn = useStableCallback(timelineProps.onToggleTurn);
  const stableOnRetryTurnError = useStableCallback(timelineProps.onRetryTurnError);
  const stableOnDismissTurnError = useStableCallback(timelineProps.onDismissTurnError);

  return (
    <div
      className={["conversation-view", className].filter(Boolean).join(" ")}
      data-conversation-view={editable ? "editable" : "readonly"}
    >
      {beforeTimeline}
      <ContextStream
        {...timelineProps}
        readOnly={!editable}
        onEdit={timelineProps.onEdit ? stableOnEdit : undefined}
        onDelete={timelineProps.onDelete ? stableOnDelete : undefined}
        onEditQuestion={timelineProps.onEditQuestion ? stableOnEditQuestion : undefined}
        onDeleteQuestion={timelineProps.onDeleteQuestion ? stableOnDeleteQuestion : undefined}
        onBranchFrom={timelineProps.onBranchFrom ? stableOnBranchFrom : undefined}
        onSelectBranch={timelineProps.onSelectBranch ? stableOnSelectBranch : undefined}
        onInsert={timelineProps.onInsert ? stableOnInsert : undefined}
        onOpenSubagent={timelineProps.onOpenSubagent ? stableOnOpenSubagent : undefined}
        onOpenWorkflowRun={timelineProps.onOpenWorkflowRun ? stableOnOpenWorkflowRun : undefined}
        onToggleTurn={timelineProps.onToggleTurn ? stableOnToggleTurn : undefined}
        onRetryTurnError={timelineProps.onRetryTurnError ? stableOnRetryTurnError : undefined}
        onDismissTurnError={timelineProps.onDismissTurnError ? stableOnDismissTurnError : undefined}
      />
      {editable ? composer : null}
    </div>
  );
}
