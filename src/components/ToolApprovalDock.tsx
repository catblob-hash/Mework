import { Check, ChevronLeft, ChevronRight, ShieldAlert, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { PendingToolPrompt, ToolPromptDecision } from "../types";
import "./ToolApprovalDock.css";

/** Position of the shown card inside its conversation's pending stack. */
export interface ToolApprovalDockStack {
  /** 0-based index of the card currently shown. */
  index: number;
  /** Pending cards in this conversation, the shown one included. */
  total: number;
  /** Flip to a neighbouring card without answering the current one. */
  onNavigate: (delta: number) => void;
}

export interface ToolApprovalDockProps {
  pending: PendingToolPrompt | null;
  /** Absent when the queue holds a single card; the pager only draws for 2+. */
  stack?: ToolApprovalDockStack;
  onDecide: (decision: ToolPromptDecision) => void;
}

interface ToolApprovalDockContentProps extends Omit<ToolApprovalDockProps, "pending"> {
  pending: PendingToolPrompt;
}

function ToolApprovalDockContent({
  pending,
  stack,
  onDecide
}: ToolApprovalDockContentProps) {
  const { t } = useI18n();
  const titleId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const allowRef = useRef<HTMLButtonElement>(null);
  // One card answers once. Without this a double-click would spend the prompt
  // id twice, and the second call fails with a confusing "prompt not found".
  const decidedRef = useRef(false);
  const [decided, setDecided] = useState<ToolPromptDecision | null>(null);
  const locked = decided !== null;

  useEffect(() => {
    (allowRef.current ?? dialogRef.current)?.focus({ preventScroll: true });
  }, []);

  const decide = (decision: ToolPromptDecision) => {
    if (decidedRef.current) return;
    decidedRef.current = true;
    setDecided(decision);
    onDecide(decision);
  };

  return (
    <section
      ref={dialogRef}
      className={`tool-approval-dock${locked ? " tool-approval-dock--decided" : ""}`}
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      data-pending-tool-approval="true"
      tabIndex={-1}
    >
      <header className="tool-approval-dock__header">
        <span className="tool-approval-dock__icon" aria-hidden="true"><ShieldAlert size={15} /></span>
        <div className="tool-approval-dock__title">
          <h2 id={titleId}>{t("需要你的确认", "Your approval is needed")}</h2>
          <span>
            {pending.requester
              ? t("{requester} · {label}", "{requester} · {label}", {
                  requester: pending.requester,
                  label: pending.label
                })
              : pending.label}
          </span>
        </div>
        {stack && stack.total > 1 && (
          <span className="tool-approval-dock__pager" data-pending-approval-pager="true">
            <button
              type="button"
              className="tool-approval-dock__pager-step"
              aria-label={t("上一条待确认", "Previous pending approval")}
              disabled={stack.index <= 0}
              onClick={() => stack.onNavigate(-1)}
            >
              <ChevronLeft size={12} aria-hidden="true" />
            </button>
            <span className="tool-approval-dock__pager-count">
              {stack.index + 1}/{stack.total}
            </span>
            <button
              type="button"
              className="tool-approval-dock__pager-step"
              aria-label={t("下一条待确认", "Next pending approval")}
              disabled={stack.index >= stack.total - 1}
              onClick={() => stack.onNavigate(1)}
            >
              <ChevronRight size={12} aria-hidden="true" />
            </button>
          </span>
        )}
        <span className="tool-approval-dock__risk">
          {t("风险 {level}", "Risk {level}", { level: pending.riskLevel })}
        </span>
      </header>

      <p className="tool-approval-dock__summary">{pending.summary}</p>
      <p className="tool-approval-dock__reason">{pending.reason}</p>
      {pending.mandatory && (
        <p className="tool-approval-dock__reason">
          {t(
            "这一条无论安全层级如何都会询问：完全访问也不会跳过它。",
            "This one is asked at every security level: full access does not skip it."
          )}
        </p>
      )}

      <footer className="tool-approval-dock__footer">
        <button
          type="button"
          className="tool-approval-dock__action tool-approval-dock__action--deny"
          disabled={locked}
          onClick={() => decide("deny")}
        >
          <X size={14} aria-hidden="true" />
          {t("拒绝", "Deny")}
        </button>
        {pending.allowAlwaysOffered && (
          <button
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--always"
            disabled={locked}
            onClick={() => decide("allow_always")}
          >
            {t("总是允许", "Always allow")}
          </button>
        )}
        <button
          ref={allowRef}
          type="button"
          className="tool-approval-dock__action tool-approval-dock__action--allow"
          disabled={locked}
          onClick={() => decide("allow_once")}
        >
          {t("允许", "Allow")}
          <Check size={14} aria-hidden="true" />
        </button>
      </footer>

      <div className="tool-approval-dock__status" aria-live="polite">
        {decided && (
          <span role="status">
            {decided === "deny"
              ? t("已拒绝", "Denied")
              : decided === "allow_always"
                ? t("本对话内的同类调用将不再询问", "Calls of this kind will no longer be asked in this conversation")
                : t("已允许", "Allowed")}
          </span>
        )}
      </div>
    </section>
  );
}

export function ToolApprovalDock(props: ToolApprovalDockProps) {
  if (!props.pending) return null;
  // Keyed remount: a new prompt id must arrive with its own fresh decision
  // state, never inheriting the previous card's spent guard.
  return (
    <ToolApprovalDockContent
      key={props.pending.promptId}
      {...props}
      pending={props.pending}
    />
  );
}
