import { ChevronRight } from "lucide-react";
import { useI18n } from "../i18n";
import type { WorkflowRunView } from "../lib/workflowRuns";
import { formatRunElapsed, formatRunTokens, stepTone } from "../lib/workflowRuns";
import { RollingNumber } from "./RollingNumber";
import "./WorkflowRunCard.css";

export interface WorkflowRunCardProps {
  view: WorkflowRunView;
  /**
   * Opens this run's panel in the task container. Absent on a surface that has
   * no task container to open — a read-only transcript — and the card then
   * renders inert rather than as a control that cannot do what it offers.
   */
  onOpen?: (runId: string) => void;
}

/**
 * A workflow run as one line in the message stream.
 *
 * The stream says *that* a run exists and roughly how far along it is; the task
 * container says everything else. That split is the whole point of the card
 * being this small: a plan with thirty steps used to push the conversation off
 * the screen while it ran, and every one of those rows was already on screen in
 * the panel beside it.
 *
 * It titles itself the same way the panel's bar does — the kind of task, then
 * its wall time and what it has spent. The run's own name is not that title:
 * the agent pool auto-names a workflow driver ("a1"), so the name says nothing
 * about the run and read as a stray label where a title belongs. It survives in
 * the card's accessible name, which is where two runs a few lines apart still
 * need telling apart.
 *
 * The status strip is one square per plan slot, in plan order. It is decorative
 * — the counts a screen reader needs are in the card's accessible name, where
 * they are a sentence rather than thirty unlabelled cells.
 */
export function WorkflowRunCard({ view, onOpen }: WorkflowRunCardProps) {
  const { t } = useI18n();
  const done = view.steps.filter((step) => step.state === "finished").length;
  const elapsed = view.elapsedMs === null ? null : formatRunElapsed(view.elapsedMs);
  const summary = t(
    "工作流 {name}：{total} 个代理，已完成 {done} 个",
    "Workflow {name}: {total} agents, {done} done",
    { name: view.name, total: view.stepCount, done }
  );

  const body = (
    <>
      <span className="workflow-run-card__title">
        <span className="workflow-run-card__kind">{t("工作流", "Workflow")}</span>
        <span className="workflow-run-card__metrics">
          {elapsed && <span><RollingNumber value={elapsed} /></span>}
          {view.tokens !== null && (
            <span>
              <RollingNumber
                value={t("{tokens} token", "{tokens} tokens", { tokens: formatRunTokens(view.tokens) })}
              />
            </span>
          )}
        </span>
        {onOpen && (
          <ChevronRight className="workflow-run-card__chevron" size={14} aria-hidden="true" />
        )}
      </span>
      <span className="workflow-run-card__meta">
        <span>{t("{count} 个代理", "{count} agents", { count: view.stepCount })}</span>
      </span>
      {view.steps.length > 0 && (
        <span className="workflow-run-card__strip" aria-hidden="true">
          {view.steps.map((step) => (
            <span
              className={`workflow-run-card__pip workflow-run-card__pip--${stepTone(step)}`}
              key={step.key}
            />
          ))}
        </span>
      )}
    </>
  );

  if (!onOpen) {
    return (
      <section
        className={`workflow-run-card workflow-run-card--${view.state}`}
        aria-label={summary}
        aria-busy={view.running || undefined}
      >
        {body}
      </section>
    );
  }

  return (
    <button
      type="button"
      className={`workflow-run-card workflow-run-card--${view.state} workflow-run-card--openable`}
      aria-label={t("在任务面板中打开 {summary}", "Open in the task panel — {summary}", { summary })}
      aria-busy={view.running || undefined}
      onClick={() => onOpen(view.id)}
    >
      {body}
    </button>
  );
}
