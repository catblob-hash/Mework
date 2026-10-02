import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { t } from "../i18n";
import type { ConversationPlan } from "../types";
import { PlanPane, planStatusLabel, planTitle } from "./PlanPage";

function plan(overrides: Partial<ConversationPlan> = {}): ConversationPlan {
  return {
    conversationId: "conversation-1",
    markdown: "# 替换审批闸\n\n分三步完成。",
    status: "draft",
    createdAt: "2026-07-20T01:00:00Z",
    updatedAt: "2026-07-20T01:30:00Z",
    ...overrides
  };
}

describe("PlanPane", () => {
  it("renders the plan document and the dock it is answered from", () => {
    render(
      <PlanPane
        plan={plan()}
        awaitingApproval
        dock={<button type="button">批准计划</button>}
      />
    );

    const pane = document.querySelector(".plan-page") as HTMLElement;
    expect(pane).toBeTruthy();
    expect(within(pane).getByRole("heading", { name: "替换审批闸" })).toBeInTheDocument();
    expect(within(pane).getByText("分三步完成。")).toBeInTheDocument();
    // The dock lives inside the plan block so the card is answered next to what it is about.
    const dock = pane.querySelector(".plan-page__dock") as HTMLElement;
    expect(within(dock).getByRole("button", { name: "批准计划" })).toBeInTheDocument();
  });

  it("says the plan is still unwritten and draws no dock without one", () => {
    render(<PlanPane plan={null} awaitingApproval={false} />);

    expect(screen.getByText("模型还没有写下计划。")).toBeInTheDocument();
    expect(document.querySelector(".plan-page__dock")).toBeNull();
  });

  it("reports the status a pane header shows, with the pending card outranking the stored one", () => {
    expect(planStatusLabel(plan(), true, t)).toBe("待批准");
    expect(planStatusLabel(plan({ status: "approved" }), false, t)).toBe("已批准");
    expect(planStatusLabel(plan({ status: "rejected" }), false, t)).toBe("待修改");
    expect(planStatusLabel(plan(), false, t)).toBe("撰写中");
    expect(planStatusLabel(null, false, t)).toBe("撰写中");
  });

  it("titles the pane with the plan's own first heading", () => {
    expect(planTitle(plan().markdown)).toBe("替换审批闸");
    expect(planTitle("没有标题的正文")).toBeNull();
  });
});
