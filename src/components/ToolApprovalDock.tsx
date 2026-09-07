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
  /** `feedback` is carried only by a denied plan-exit card. */
  onDecide: (decision: ToolPromptDecision, feedback?: string) => void;
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
  const feedbackRef = useRef<HTMLTextAreaElement>(null);
  // One card answers once. Without this a double-click would spend the prompt
  // id twice, and the second call fails with a confusing "prompt not found".
  const decidedRef = useRef(false);
  const [decided, setDecided] = useState<ToolPromptDecision | null>(null);
  // Revealing the feedback box is not yet an answer: the guard below must stay
  // unspent until the user actually submits it.
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [feedback, setFeedback] = useState("");
  const locked = decided !== null;
  const kind = pending.kind ?? "tool";
  const planExit = kind === "plan_exit";
  const planEnter = kind === "plan_enter";
  const plan = planExit || planEnter;

  useEffect(() => {
    (allowRef.current ?? dialogRef.current)?.focus({ preventScroll: true });
  }, []);

  useEffect(() => {
    if (feedbackOpen) feedbackRef.current?.focus({ preventScroll: true });
  }, [feedbackOpen]);

  const decide = (decision: ToolPromptDecision, text?: string) => {
    if (decidedRef.current) return;
    decidedRef.current = true;
    setDecided(decision);
    onDecide(decision, text);
  };

  const heading = planExit
    ? t("计划已就绪，是否开始实施？", "Ready to code?")
    : planEnter
      ? t("模型请求进入计划模式", "The model wants to enter plan mode")
      : t("需要你的确认", "Your approval is needed");

  const statusText = (): string => {
    if (decided === null) return "";
    if (planExit) {
      return decided === "deny"
        ? t("已退回计划，等待模型修改", "Sent the plan back for changes")
        : decided === "allow_always"
          ? t("开始实施，编辑将自动接受", "Implementing; edits are accepted automatically")
          : t("开始实施，编辑仍逐条批准", "Implementing; edits are still approved one by one");
    }
    if (planEnter) {
      return decided === "deny"
        ? t("保持当前安全层级", "Staying at the current security level")
        : t("已进入计划模式", "Entered plan mode");
    }
    return decided === "deny"
      ? t("已拒绝", "Denied")
      : decided === "allow_always"
        ? t("本对话内的同类调用将不再询问", "Calls of this kind will no longer be asked in this conversation")
        : t("已允许", "Allowed");
  };

  return (
    <section
      ref={dialogRef}
      className={`tool-approval-dock${locked ? " tool-approval-dock--decided" : ""}`}
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      data-pending-tool-approval="true"
      data-approval-kind={kind}
      tabIndex={-1}
    >
      <header className="tool-approval-dock__header">
        <span className="tool-approval-dock__icon" aria-hidden="true"><ShieldAlert size={15} /></span>
        <div className="tool-approval-dock__title">
          <h2 id={titleId}>{heading}</h2>
          {!plan && (
            <span>
              {pending.requester
                ? t("{requester} · {label}", "{requester} · {label}", {
                    requester: pending.requester,
                    label: pending.label
                  })
                : pending.label}
            </span>
          )}
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
        {/* A plan card is a decision about the conversation, not about one call:
            it has no risk level to report. */}
        {!plan && (
          <span className="tool-approval-dock__risk">
            {t("风险 {level}", "Risk {level}", { level: pending.riskLevel })}
          </span>
        )}
      </header>

      {planEnter ? (
        <p className="tool-approval-dock__summary">
          {t(
            "进入后模型只会探索代码并撰写计划，在你批准计划前不会修改任何文件。",
            "It will only explore the code and write a plan; no files change until you approve the plan."
          )}
        </p>
      ) : (
        <>
          <p className="tool-approval-dock__summary">{pending.summary}</p>
          <p className="tool-approval-dock__reason">
            {planExit
              ? t("计划全文见左侧计划页面", "The full plan is shown on the plan page")
              : pending.reason}
          </p>
        </>
      )}
      {/* Plan cards are always mandatory; saying so would only be noise. */}
      {!plan && pending.mandatory && (
        <p className="tool-approval-dock__reason">
          {t(
            "这一条无论安全层级如何都会询问：完全访问也不会跳过它。",
            "This one is asked at every security level: full access does not skip it."
          )}
        </p>
      )}

      {planExit ? (
        <footer className="tool-approval-dock__footer">
          <button
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--deny"
            disabled={locked}
            onClick={() => setFeedbackOpen(true)}
          >
            <X size={14} aria-hidden="true" />
            {t("否，继续规划", "No, keep planning")}
          </button>
          <button
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--always"
            disabled={locked}
            onClick={() => decide("allow_once")}
          >
            {t("是，手动批准编辑", "Yes, manually approve edits")}
          </button>
          <button
            ref={allowRef}
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--allow"
            disabled={locked}
            onClick={() => decide("allow_always")}
          >
            {t("是，自动接受编辑", "Yes, auto-accept edits")}
            <Check size={14} aria-hidden="true" />
          </button>
        </footer>
      ) : planEnter ? (
        <footer className="tool-approval-dock__footer">
          <button
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--deny"
            disabled={locked}
            onClick={() => decide("deny")}
          >
            <X size={14} aria-hidden="true" />
            {t("否，直接开始实现", "No, start implementing now")}
          </button>
          <button
            ref={allowRef}
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--allow"
            disabled={locked}
            onClick={() => decide("allow_once")}
          >
            {t("是，进入计划模式", "Yes, enter plan mode")}
            <Check size={14} aria-hidden="true" />
          </button>
        </footer>
      ) : (
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
      )}

      {planExit && feedbackOpen && (
        <div className="tool-approval-dock__feedback">
          <textarea
            ref={feedbackRef}
            className="tool-approval-dock__feedback-input"
            value={feedback}
            disabled={locked}
            aria-label={t("修改意见", "Plan feedback")}
            placeholder={t("告诉模型需要修改什么", "Tell the model what to change")}
            onChange={(event) => setFeedback(event.target.value)}
          />
          <button
            type="button"
            className="tool-approval-dock__action tool-approval-dock__action--allow"
            disabled={locked || feedback.trim() === ""}
            onClick={() => decide("deny", feedback.trim())}
          >
            {t("提交反馈", "Send feedback")}
          </button>
        </div>
      )}

      <div className="tool-approval-dock__status" aria-live="polite">
        {decided && <span role="status">{statusText()}</span>}
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
