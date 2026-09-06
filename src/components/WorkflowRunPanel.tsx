import {
  Check,
  ChevronRight,
  LoaderCircle,
  SkipForward,
  Square,
  X,
  Workflow as WorkflowIcon
} from "lucide-react";
import { useId, useState } from "react";
import { useI18n } from "../i18n";
import type { WorkflowRunPhase, WorkflowRunStep, WorkflowRunView } from "../lib/workflowRuns";
import { formatRunElapsed, formatRunTokens, phaseTone, stepTone } from "../lib/workflowRuns";
import { IconButton } from "./Common";
import { RollingNumber } from "./RollingNumber";
import "./WorkflowRunPanel.css";

export interface WorkflowRunPanelProps {
  view: WorkflowRunView;
  /** Step whose transcript the main area is showing, so its row reads as current. */
  selectedAgentId?: string | null;
  /** True while this run's abort has been asked for and not yet observed. */
  stopping?: boolean;
  /** Opens one step's read-only transcript. A step is a real agent; the run is not. */
  onOpenAgent?: (agentId: string) => void;
  onStop?: () => void;
  /**
   * Run identifier the Skip command addresses, or null on a run that can no
   * longer be addressed — history, or a run whose model turn is gone.
   */
  runId?: string | null;
  onStepControl?: (runId: string, stepIndex: number) => void;
}

function StepIcon({ step }: { step: WorkflowRunStep }) {
  const tone = stepTone(step);
  if (tone === "finished" || tone === "cached") return <Check size={11} aria-hidden="true" />;
  if (tone === "failed" || tone === "skipped") return <X size={11} aria-hidden="true" />;
  if (tone === "running") {
    return <LoaderCircle className="workflow-panel__spinner" size={11} aria-hidden="true" />;
  }
  return <span className="workflow-panel__pending-dot" aria-hidden="true" />;
}

function stepStateLabel(step: WorkflowRunStep, t: ReturnType<typeof useI18n>["t"]): string {
  const tone = stepTone(step);
  if (tone === "skipped") return t("已跳过", "Skipped");
  if (tone === "cached") return t("已缓存", "Cached");
  if (tone === "finished") return t("已完成", "Completed");
  if (tone === "failed") return t("已失败", "Failed");
  if (tone === "pending") return t("等待中", "Waiting");
  return t("运行中", "Running");
}

/** Em dash rather than zero: a step that has reported nothing has not reported a nought. */
const ABSENT = "—";

/**
 * One step, as its own tile inside the run's block.
 *
 * It is deliberately the same shape as a task row — icon, name, a smaller line
 * naming the role underneath, metrics on the right — one size down, because a
 * step *is* a task: it just belongs to a run rather than to the conversation.
 */
function StepTile({
  step,
  selectedAgentId,
  runId,
  onOpenAgent,
  onStepControl
}: {
  step: WorkflowRunStep;
  selectedAgentId: string | null;
  runId: string | null;
  onOpenAgent?: (agentId: string) => void;
  onStepControl?: (runId: string, stepIndex: number) => void;
}) {
  const { t } = useI18n();
  const tone = stepTone(step);
  const selected = step.agentId !== null && step.agentId === selectedAgentId;
  // A step is a real agent with a real transcript, so its tile opens. A slot
  // with no agent has nothing to open, and the run itself is a script.
  const openable = Boolean(step.agentId && onOpenAgent);
  // No plan index, no Skip. The host addresses a slot by number, so a step this
  // projection could not match to one has no truthful number to send and must
  // not offer the control at all.
  const skippable = Boolean(runId && onStepControl)
    && tone === "running"
    && step.planIndex !== null;

  return (
    <li
      className={`workflow-step workflow-step--${tone}${selected ? " workflow-step--selected" : ""}`}
    >
      <div
        className="workflow-step__main"
        role={openable ? "button" : undefined}
        tabIndex={openable ? 0 : undefined}
        aria-current={selected || undefined}
        aria-label={openable
          ? t("打开步骤 {label}", "Open step {label}", { label: step.label })
          : undefined}
        title={step.modelId
          ? t("模型：{model}", "Model: {model}", { model: step.modelId })
          : stepStateLabel(step, t)}
        onClick={openable ? () => onOpenAgent?.(step.agentId as string) : undefined}
        onKeyDown={openable
          ? (event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            if (event.target !== event.currentTarget) return;
            event.preventDefault();
            onOpenAgent?.(step.agentId as string);
          }
          : undefined}
      >
        <span className={`workflow-step__icon workflow-step__icon--${tone}`}>
          <StepIcon step={step} />
        </span>
        <span className="workflow-step__copy">
          <span className={`workflow-step__label${tone === "running" ? " pulse-text" : ""}`}>{step.label}</span>
          {/* The role, which is what the plan actually chose: `agentType` is
              the one thing a script says about who runs a step, and which
              model answers for that name is the user's configuration. The
              model stays reachable as the tile's tooltip. A step that named
              no role has only its model left to report. */}
          <span className="workflow-step__detail">
            {step.role ?? step.modelId ?? ABSENT}
          </span>
        </span>
        <span className="workflow-step__metrics" aria-hidden="true">
          <span><RollingNumber value={step.tokens === null ? ABSENT : formatRunTokens(step.tokens)} /></span>
          <span><RollingNumber value={step.elapsedMs === null ? ABSENT : formatRunElapsed(step.elapsedMs)} /></span>
        </span>
        {skippable && (
          <IconButton
            label={t("跳过 {label}", "Skip {label}", { label: step.label })}
            className="workflow-step__skip"
            onClick={(event) => {
              event.stopPropagation();
              onStepControl?.(runId as string, step.planIndex as number);
            }}
          >
            <SkipForward size={10} />
          </IconButton>
        )}
      </div>
    </li>
  );
}

/**
 * One phase: a caption and its steps, both always on screen.
 *
 * The phase used to be a disclosure with a strip of coloured squares standing
 * in for the steps it hid. Neither survives: nothing is hidden any more, so the
 * squares would be a summary of what is already directly underneath them.
 */
function PhaseBlock({
  phase,
  selectedAgentId,
  runId,
  onOpenAgent,
  onStepControl
}: {
  phase: WorkflowRunPhase;
  selectedAgentId: string | null;
  runId: string | null;
  onOpenAgent?: (agentId: string) => void;
  onStepControl?: (runId: string, stepIndex: number) => void;
}) {
  const { t } = useI18n();
  const heading = phase.heading ?? t("未分组", "Ungrouped");

  return (
    <section className={`workflow-phase workflow-phase--${phaseTone(phase)}`}>
      <h5 className="workflow-phase__head">
        <span className="workflow-phase__title">{heading}</span>
        <span className="workflow-phase__count">{phase.done}/{phase.total}</span>
      </h5>
      <ul className="workflow-steps">
        {phase.steps.map((step) => (
          <StepTile
            key={step.key}
            step={step}
            selectedAgentId={selectedAgentId}
            runId={runId}
            onOpenAgent={onOpenAgent}
            onStepControl={onStepControl}
          />
        ))}
      </ul>
    </section>
  );
}

/**
 * One workflow run, as the task container draws it.
 *
 * A run is one block: its own bar at the top, then every phase and every step
 * inside the same block. There is no disclosure at either level any more —
 * neither on the run nor on a phase — because a run's whole point is the work
 * it is doing right now, and two levels of chevrons meant the answer was
 * usually two clicks away. Containment carries what the nesting used to: the
 * run is a soft block, and each step is a smaller soft block inside it.
 *
 * The steps stay openable; the run itself never was. It is a script, and the
 * transcript behind its view is the driver's own synthetic step list.
 */
export function WorkflowRunPanel({
  view,
  selectedAgentId = null,
  stopping = false,
  onOpenAgent,
  onStop,
  runId = null,
  onStepControl
}: WorkflowRunPanelProps) {
  const { t } = useI18n();
  // The run log is the one thing still behind a disclosure. It is narration,
  // not structure: it can run to hundreds of lines, and it is not what anyone
  // opens the panel for.
  const [logsOpen, setLogsOpen] = useState(false);
  const logsId = useId();

  const elapsed = view.elapsedMs === null ? null : formatRunElapsed(view.elapsedMs);

  return (
    <section
      className={`workflow-panel workflow-panel--${view.state}`}
      aria-label={t("工作流 {name}", "Workflow {name}", { name: view.name })}
      aria-busy={view.running || undefined}
    >
      {/* One bar, exactly like every other task in this panel: the same row
          chrome and the same metric type. The run is named by the model, so
          the bar carries that name rather than the word "工作流" — what kind of
          task it is, the icon already says. */}
      <div className={`task-row task-row--${view.state}`}>
        <div className="task-row__main workflow-panel__bar">
          <span className={`task-row__icon task-row__icon--${view.state}`}>
            <WorkflowIcon size={13} aria-hidden="true" />
          </span>
          <span className="task-row__copy">
            <span className={`task-row__label${view.running ? " pulse-text" : ""}`}>{view.name}</span>
            <span className="task-row__detail">
              {view.scriptName ?? t("{total} 个步骤", "{total} steps", { total: view.stepCount })}
            </span>
          </span>
          <span className="task-metrics workflow-panel__meta">
            {elapsed && <span className="task-metrics__cell"><RollingNumber value={elapsed} /></span>}
            {view.tokens !== null && (
              <span className="task-metrics__cell">
                <RollingNumber
                  value={t("{tokens} token", "{tokens} tokens", { tokens: formatRunTokens(view.tokens) })}
                />
              </span>
            )}
          </span>
          {view.running && onStop && (
            <IconButton
              label={stopping
                ? t("正在中止工作流", "Stopping the workflow")
                : t("中止工作流", "Stop the workflow")}
              className="task-row__stop"
              disabled={stopping}
              onClick={(event) => {
                event.stopPropagation();
                onStop();
              }}
            >
              {stopping
                ? <LoaderCircle size={11} className="workflow-panel__spinner" />
                : <Square size={9} fill="currentColor" />}
            </IconButton>
          )}
        </div>
      </div>

      <div className="workflow-panel__body">
        {view.description && (
          <p className="workflow-panel__description">{view.description}</p>
        )}
        {view.phases.map((phase) => (
          <PhaseBlock
            key={phase.key}
            phase={phase}
            selectedAgentId={selectedAgentId}
            runId={runId}
            onOpenAgent={onOpenAgent}
            onStepControl={onStepControl}
          />
        ))}
        {view.logs.length > 0 && (
          <div className="workflow-panel__logs">
            <button
              type="button"
              className="workflow-panel__logs-toggle"
              aria-expanded={logsOpen}
              aria-controls={logsId}
              onClick={() => setLogsOpen((current) => !current)}
            >
              <ChevronRight
                className={`disclosure-chevron${logsOpen ? " disclosure-chevron--open" : ""}`}
                size={12}
                aria-hidden="true"
              />
              <span>{t("运行日志（{count} 条）", "Run log ({count})", { count: view.logs.length })}</span>
            </button>
            {logsOpen && (
              <ol className="workflow-panel__log-list" id={logsId}>
                {view.logs.map((message, index) => (
                  // Logs are ordered but not addressable: the host assigns no
                  // identity a re-render could key on, so position is the key.
                  <li key={`${index}-${message}`}>{message}</li>
                ))}
              </ol>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
