import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { ConversationPlan } from "../types";
import { MarkdownContent } from "./MarkdownContent";
import "./PlanPage.css";

export interface PlanPaneProps {
  plan: ConversationPlan | null;
  /** True while a `plan_exit` card for this conversation is waiting. */
  awaitingApproval: boolean;
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
 * Where the plan stands, as one word. A pending exit card outranks the stored
 * status: the document is already written, and what the user is being asked is
 * whether it stands.
 */
export function planStatusLabel(
  plan: ConversationPlan | null,
  awaitingApproval: boolean,
  t: ReturnType<typeof useI18n>["t"]
): string {
  if (awaitingApproval) return t("待批准", "Awaiting approval");
  if (plan?.status === "approved") return t("已批准", "Approved");
  if (plan?.status === "rejected") return t("已退回", "Sent back");
  return t("撰写中", "Drafting");
}

/**
 * The plan document itself: the markdown and, while the exit card is the pending
 * one, the dock that answers it. Approving a plan you cannot see is the failure
 * the co-located dock exists to prevent, so the two stay in one block wherever
 * the plan is shown.
 */
export function PlanPane({ plan, dock }: PlanPaneProps) {
  const { t } = useI18n();
  const markdown = plan?.markdown ?? "";
  return (
    <div className="plan-page">
      <div className="plan-page__body">
        {markdown.trim()
          ? <MarkdownContent content={markdown} renderHtml />
          : (
            <p className="plan-page__empty">
              {t("模型还没有写下计划。", "The model has not written a plan yet.")}
            </p>
          )}
      </div>
      {dock ? <div className="plan-page__dock">{dock}</div> : null}
    </div>
  );
}
