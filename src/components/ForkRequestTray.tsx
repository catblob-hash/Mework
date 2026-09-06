import { GitFork } from "lucide-react";
import { useState } from "react";
import { useI18n } from "../i18n";
import type { PendingForkRequest } from "../types";

/** Prompts longer than this get an expand toggle; CSS clamps the preview to 5 lines. */
const PROMPT_TOGGLE_CHARS = 240;

export interface ForkRequestTrayProps {
  /** Oldest first; the tray renders the newest card at the top. */
  requests: PendingForkRequest[];
  onDecide: (forkId: string, approved: boolean) => void;
  /** Clicking the source title jumps to the conversation that raised the request. */
  onOpenSource?: (workspaceId: string, conversationId: string) => void;
}

/** Non-blocking stack of fork requests. It never traps focus and never covers
 * the app: the tray itself is pointer-transparent, only the cards are not. */
export function ForkRequestTray({ requests, onDecide, onOpenSource }: ForkRequestTrayProps) {
  const { t } = useI18n();
  const [expandedIds, setExpandedIds] = useState<Record<string, boolean>>({});

  if (requests.length === 0) return null;

  const newestFirst = requests.slice().reverse();

  return (
    <section
      className="fork-request-tray"
      aria-live="polite"
      aria-label={t("分叉请求", "Fork requests")}
    >
      {newestFirst.map((request) => {
        const expanded = expandedIds[request.forkId] === true;
        const sourceLabel = t("来自：{title}", "From: {title}", { title: request.sourceTitle });
        return (
          <section className="fork-request-card" key={request.forkId} data-fork-id={request.forkId}>
            <div className="fork-request-card__title">
              <GitFork size={13} />
              <span>{t("模型请求分叉会话", "The model wants to fork this conversation")}</span>
            </div>
            <div className="fork-request-card__source">
              {onOpenSource ? (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => onOpenSource(request.workspaceId, request.sourceConversationId)}
                >
                  {sourceLabel}
                </button>
              ) : (
                <span>{sourceLabel}</span>
              )}
            </div>
            <p className={`fork-request-card__prompt${expanded ? " fork-request-card__prompt--expanded" : ""}`}>
              {request.prompt}
            </p>
            {request.prompt.length > PROMPT_TOGGLE_CHARS ? (
              <button
                type="button"
                className="text-button fork-request-card__toggle"
                onClick={() => setExpandedIds((current) => ({ ...current, [request.forkId]: !expanded }))}
              >
                {expanded ? t("收起", "Show less") : t("展开", "Show more")}
              </button>
            ) : null}
            <div className="fork-request-card__meta">
              {request.inheritContext
                ? t("继承上下文：是", "Inherits context: yes")
                : t("继承上下文：否", "Inherits context: no")}
            </div>
            <div className="fork-request-card__actions">
              <button type="button" className="button" onClick={() => onDecide(request.forkId, false)}>
                {t("拒绝", "Deny")}
              </button>
              <button type="button" className="button button--primary" onClick={() => onDecide(request.forkId, true)}>
                {t("批准", "Approve")}
              </button>
            </div>
          </section>
        );
      })}
    </section>
  );
}
