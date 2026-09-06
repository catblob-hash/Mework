import { ChevronRight } from "lucide-react";
import { useEffect, useId, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { formatTurnDuration } from "../lib/conversationTurns";
import type { ConversationTurn } from "../lib/conversationTurns";
import { RollingNumber } from "./RollingNumber";
import "./ConversationTurnDisclosure.css";

export interface ConversationTurnDisclosureProps {
  turn: ConversationTurn;
  onToggle: () => void;
  children: ReactNode;
}

function tokenCount(value: number | undefined): string {
  return value === undefined ? "—" : String(value);
}

export function ConversationTurnDisclosure({
  turn,
  onToggle,
  children
}: ConversationTurnDisclosureProps) {
  const { t } = useI18n();
  const bodyId = useId();
  const [now, setNow] = useState(() => Date.now());
  const live = turn.status === "running";

  useEffect(() => {
    if (!live) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [live, turn.startedAt]);

  const startedAt = new Date(turn.startedAt).getTime();
  const durationMs = Math.max(
    0,
    (turn.durationMs ?? 0)
      + (live ? Math.max(0, now - (Number.isFinite(startedAt) ? startedAt : now)) : 0)
  );
  const duration = formatTurnDuration(durationMs);
  const summary = turn.status === "interrupted"
    ? t(
      "{model} 在 {duration} 后中止",
      "{model} stopped after {duration}",
      { model: turn.modelId, duration }
    )
    : t(
      "{model} 运行了 {duration}",
      "{model} ran for {duration}",
      { model: turn.modelId, duration }
    );
  // Where the elapsed time sits inside the sentence: Chinese puts it in the
  // middle of the interrupted phrasing, English at the end. Search from the
  // right so a model id that happens to end in the same characters cannot claim
  // the slot. The whole sentence still goes to `title` and to the accessible
  // name, so nothing about how it reads changes.
  const durationAt = summary.lastIndexOf(duration);

  return (
    <section className="conversation-turn-disclosure" data-turn-id={turn.id}>
      <button
        type="button"
        className="conversation-turn-disclosure__toggle"
        aria-controls={bodyId}
        aria-expanded={turn.expanded}
        onClick={onToggle}
      >
        <ChevronRight
          className={`disclosure-chevron${turn.expanded ? " disclosure-chevron--open" : ""}`}
          size={14}
          aria-hidden="true"
        />
        <span className="conversation-turn-disclosure__summary" title={summary}>
          {durationAt < 0 ? summary : (
            <>
              {summary.slice(0, durationAt)}
              <RollingNumber value={duration} />
              {summary.slice(durationAt + duration.length)}
            </>
          )}
        </span>
        <span className="conversation-turn-disclosure__usage">
          <span>
            <span className="conversation-turn-disclosure__usage-label">{t("输入", "Input")}</span>
            <span className="conversation-turn-disclosure__usage-value">
              <RollingNumber value={tokenCount(turn.usage.inputTokens)} />
            </span>
          </span>
          <span>
            <span className="conversation-turn-disclosure__usage-label">{t("缓存输入", "Cached input")}</span>
            <span className="conversation-turn-disclosure__usage-value">
              <RollingNumber value={tokenCount(turn.usage.cachedInputTokens)} />
            </span>
          </span>
          <span>
            <span className="conversation-turn-disclosure__usage-label">{t("输出", "Output")}</span>
            <span className="conversation-turn-disclosure__usage-value">
              <RollingNumber value={tokenCount(turn.usage.outputTokens)} />
            </span>
          </span>
        </span>
      </button>

      <div
        id={bodyId}
        className={`collapse-region conversation-turn-disclosure__body${turn.expanded ? "" : " collapse-region--closed"}`}
        aria-hidden={!turn.expanded || undefined}
        inert={!turn.expanded || undefined}
      >
        <div className="collapse-region__inner conversation-turn-disclosure__body-inner">
          {children}
        </div>
      </div>
    </section>
  );
}
