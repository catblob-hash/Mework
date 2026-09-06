import { CornerUpLeft, Images, RotateCcw, Trash2 } from "lucide-react";
import { useI18n } from "../i18n";
import type { QueuedMessage } from "../types";
import "./QueuedMessageList.css";

export interface QueuedMessageListProps {
  messages: QueuedMessage[];
  canSteer: (message: QueuedMessage) => boolean;
  steeringIds: ReadonlySet<string>;
  failedPromotionIds: ReadonlySet<string>;
  onSteer: (message: QueuedMessage) => void;
  onRetry: (message: QueuedMessage) => void;
  onDelete: (message: QueuedMessage) => void;
}

export function QueuedMessageList({
  messages,
  canSteer,
  steeringIds,
  failedPromotionIds,
  onSteer,
  onRetry,
  onDelete
}: QueuedMessageListProps) {
  const { t } = useI18n();
  if (!messages.length) return null;

  return (
    <section className="queued-messages" aria-label={t("排队消息", "Queued messages")}>
      <ol>
        {messages.map((message) => {
          const steering = steeringIds.has(message.id);
          const promotionFailed = failedPromotionIds.has(message.id);
          const imageCount = message.images?.length ?? 0;
          const contentSummary = message.content.replace(/\s+/g, " ").trim();
          const imageCountSummary = t("{count} 张图片", "{count} images", {
            count: imageCount
          });
          const firstImageName = message.images?.[0]?.name ?? t("图片", "Image");
          const imageIdentity = imageCount > 1
            ? t(
                "{name} + 另外 {count} 张",
                "{name} + {count} more",
                { name: firstImageName, count: imageCount - 1 }
              )
            : firstImageName;
          const visibleSummary = contentSummary || imageIdentity;
          const actionSummary = [
            contentSummary.slice(0, 40),
            imageCount ? imageIdentity : ""
          ].filter(Boolean).join(" · ");
          return (
            <li key={message.id} data-promotion-failed={promotionFailed || undefined}>
              <span title={actionSummary}>
                {imageCount ? <Images size={13} aria-hidden="true" /> : null}
                <span>
                  <span>{visibleSummary}</span>
                  {imageCount ? (
                    <>
                      <span aria-hidden="true"> · </span>
                      <span>{imageCountSummary}</span>
                    </>
                  ) : null}
                </span>
              </span>
              {promotionFailed ? (
                <button
                  type="button"
                  className="queued-messages__retry"
                  aria-label={t(
                    "重试发送排队消息“{message}”",
                    "Retry queued message “{message}”",
                    { message: actionSummary }
                  )}
                  title={t("重试发送", "Retry sending")}
                  disabled={steering}
                  onClick={() => onRetry(message)}
                >
                  <RotateCcw size={13} aria-hidden="true" />
                </button>
              ) : null}
              <button
                type="button"
                aria-label={t(
                   "将“{message}”引导到当前回合",
                   "Steer “{message}” into the current turn",
                   { message: actionSummary }
                 )}
                title={t("引导到当前回合", "Steer into current turn")}
                disabled={!canSteer(message) || steering}
                onClick={() => onSteer(message)}
              >
                <CornerUpLeft size={14} aria-hidden="true" />
              </button>
              <button
                type="button"
                aria-label={t(
                   "删除排队消息“{message}”",
                   "Delete queued message “{message}”",
                   { message: actionSummary }
                 )}
                title={t("删除排队消息", "Delete queued message")}
                disabled={steering}
                onClick={() => onDelete(message)}
              >
                <Trash2 size={13} aria-hidden="true" />
              </button>
            </li>
          );
        })}
      </ol>
    </section>
  );
}
