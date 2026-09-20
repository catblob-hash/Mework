import { useI18n } from "../i18n";
import type { WorkflowRunView } from "../lib/workflowRuns";
import { formatRunElapsed, formatRunTokens, phaseTone, stepTone } from "../lib/workflowRuns";
import { RollingNumber } from "./RollingNumber";
import "./WorkflowRunDetail.css";

export interface WorkflowRunDetailProps {
  view: WorkflowRunView;
}

/**
 * A workflow run as the body of its own timeline row.
 *
 * The row says *that* a run exists and how far along it is; this says roughly
 * what it is made of, and the task container says everything else. That split
 * is why this stays small: a plan with thirty steps used to push the
 * conversation off the screen while it ran, and every one of those rows was
 * already on screen in the panel beside it.
 *
 * The status strip is one square per plan slot, in plan order. It is decorative
 * — the counts a screen reader needs are in the accessible name, where they are
 * a sentence rather than thirty unlabelled cells.
 */
export function WorkflowRunDetail({ view }: WorkflowRunDetailProps) {
  const { t } = useI18n();
  const done = view.steps.filter((step) => step.state === "finished").length;
  const elapsed = view.elapsedMs === null ? null : formatRunElapsed(view.elapsedMs);
  const summary = t(
    "工作流 {name}：{total} 个代理，已完成 {done} 个",
    "Workflow {name}: {total} agents, {done} done",
    { name: view.name, total: view.stepCount, done }
  );

  return (
    <section className={`workflow-run workflow-run--${view.state}`} aria-label={summary} aria-busy={view.running || undefined}>
      <div className="workflow-run__metrics">
        <span>{t("{count} 个代理", "{count} agents", { count: view.stepCount })}</span>
        {elapsed && <span><RollingNumber value={elapsed} /></span>}
        {view.tokens !== null && (
          <span>
            <RollingNumber value={t("{tokens} token", "{tokens} tokens", { tokens: formatRunTokens(view.tokens) })} />
          </span>
        )}
      </div>
      {view.description && <p className="workflow-run__description">{view.description}</p>}
      {view.steps.length > 0 && (
        <span className="workflow-run__strip" aria-hidden="true">
          {view.steps.map((step) => (
            <span className={`workflow-run__pip workflow-run__pip--${stepTone(step)}`} key={step.key} />
          ))}
        </span>
      )}
      {view.phases.length > 0 && (
        <ul className="workflow-run__phases">
          {view.phases.map((phase) => (
            <li className={`workflow-run__phase workflow-run__phase--${phaseTone(phase)}`} key={phase.key}>
              <span>{phase.heading ?? t("未分组", "Unphased")}</span>
              <span className="workflow-run__phase-count">{phase.done}/{phase.total}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
