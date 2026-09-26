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

/** Same, for handlers whose return value the caller awaits. Only ever installed
 * when the incoming handler exists, so the ref is non-null when it runs. */
function useStableResultCallback<Args extends unknown[], Result>(callback: ((...args: Args) => Result) | undefined) {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;
  return useCallback((...args: Args) => callbackRef.current!(...args), []);
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
  const stableOnCancelEdit = useStableCallback(timelineProps.onCancelEdit);
  const stableOnSaveText = useStableCallback(timelineProps.onSaveText);
  const stableOnAddAttachments = useStableResultCallback(timelineProps.onAddAttachments);
  const stableOnSaveToolEdit = useStableResultCallback(timelineProps.onSaveToolEdit);
  const stableOnSaveTool = useStableResultCallback(timelineProps.onSaveTool);
  const stableOnSaveQuestion = useStableResultCallback(timelineProps.onSaveQuestion);
  const stableOnBranchFrom = useStableCallback(timelineProps.onBranchFrom);
  const stableOnSelectBranch = useStableCallback(timelineProps.onSelectBranch);
  const stableOnInsert = useStableCallback(timelineProps.onInsert);
  const stableOnForkAt = useStableCallback(timelineProps.onForkAt);
  const stableOnOpenSubagent = useStableCallback(timelineProps.onOpenSubagent);
  const stableOnOpenWorkflowRun = useStableCallback(timelineProps.onOpenWorkflowRun);
  const stableOnRetryTurnError = useStableCallback(timelineProps.onRetryTurnError);
  const stableOnDismissTurnError = useStableCallback(timelineProps.onDismissTurnError);
  const stableOnOpenTasks = useStableCallback(timelineProps.onOpenTasks);

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
        onCancelEdit={timelineProps.onCancelEdit ? stableOnCancelEdit : undefined}
        onSaveText={timelineProps.onSaveText ? stableOnSaveText : undefined}
        onAddAttachments={timelineProps.onAddAttachments ? stableOnAddAttachments : undefined}
        onSaveTool={timelineProps.onSaveTool ? stableOnSaveTool : undefined}
        onSaveToolEdit={timelineProps.onSaveToolEdit ? stableOnSaveToolEdit : undefined}
        onSaveQuestion={timelineProps.onSaveQuestion ? stableOnSaveQuestion : undefined}
        onBranchFrom={timelineProps.onBranchFrom ? stableOnBranchFrom : undefined}
        onSelectBranch={timelineProps.onSelectBranch ? stableOnSelectBranch : undefined}
        onInsert={timelineProps.onInsert ? stableOnInsert : undefined}
        onForkAt={timelineProps.onForkAt ? stableOnForkAt : undefined}
        onOpenSubagent={timelineProps.onOpenSubagent ? stableOnOpenSubagent : undefined}
        onOpenWorkflowRun={timelineProps.onOpenWorkflowRun ? stableOnOpenWorkflowRun : undefined}
        onRetryTurnError={timelineProps.onRetryTurnError ? stableOnRetryTurnError : undefined}
        onDismissTurnError={timelineProps.onDismissTurnError ? stableOnDismissTurnError : undefined}
        onOpenTasks={timelineProps.onOpenTasks ? stableOnOpenTasks : undefined}
      />
      {editable ? composer : null}
    </div>
  );
}
