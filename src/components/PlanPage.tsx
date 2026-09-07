import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { ConversationPlan } from "../types";
import { MarkdownContent } from "./MarkdownContent";
import { TaskPage } from "./TaskPage";
import type { TaskPageProps } from "./TaskPage";
import "./PlanPage.css";

export interface PlanPageProps {
  active: boolean;
  plan: ConversationPlan | null;
  /** True while a `plan_exit` card for this conversation is waiting. */
  awaitingApproval: boolean;
  domId?: string;
  attention?: TaskPageProps["attention"];
  onBack: () => void;
  /** The approval dock, mounted here while the plan card is the pending one. */
  dock?: ReactNode;
}

/** The plan's own first heading, which is the title the model gave it. */
export function planTitle(markdown: string): string | null {
  for (const line of markdown.split("\n", 40)) {
    const heading = /^#{1,3}\s+(.+?)\s*$/u.exec(line);
    if (heading) return heading[1];
  }
  return null;
}

/**
 * The conversation's plan, shown as a page over the timeline.
 *
 * The plan is the one artifact of plan mode the user has to read before
 * answering the exit card, so the card is docked at the foot of this page
 * rather than in the composer: approving a plan you cannot see is the failure
 * this page exists to prevent.
 */
export function PlanPage({
  active,
  plan,
  awaitingApproval,
  domId,
  attention = null,
  onBack,
  dock
}: PlanPageProps) {
  const { t } = useI18n();
  const markdown = plan?.markdown ?? "";
  const title = planTitle(markdown) ?? t("实施计划", "Implementation plan");
  const status = awaitingApproval
    ? t("待批准", "Awaiting approval")
    : plan?.status === "approved"
      ? t("已批准", "Approved")
      : plan?.status === "rejected"
        ? t("已退回", "Sent back")
        : t("撰写中", "Drafting");

  return (
    <TaskPage
      active={active}
      eyebrow={t("计划模式", "Plan mode")}
      title={title}
      domId={domId}
      actions={<span className="plan-page__status" data-plan-status={plan?.status ?? "draft"}>{status}</span>}
      attention={dock ? null : attention}
      onBack={onBack}
    >
      <div className="plan-page">
        <div className="plan-page__body">
          {markdown.trim()
            ? <MarkdownContent content={markdown} />
            : (
              <p className="plan-page__empty">
                {t("模型还没有写下计划。", "The model has not written a plan yet.")}
              </p>
            )}
        </div>
        {dock ? <div className="plan-page__dock">{dock}</div> : null}
      </div>
    </TaskPage>
  );
}
