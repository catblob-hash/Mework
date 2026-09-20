import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { QueuedMessage } from "../types";
import { QueuedMessageList } from "./QueuedMessageList";

function queuedImageMessage(
  id: string,
  names: string[]
): QueuedMessage {
  return {
    id,
    content: "",
    images: names.map((name, index) => ({
      id: `${id}-image-${index}`,
      name,
      mime: "image/png",
      width: 10,
      height: 10,
      bytes: 100
    })),
    createdAt: "2026-07-24T00:00:00Z"
  };
}

describe("QueuedMessageList", () => {
  beforeEach(() => configureI18n("zh-CN"));

  it("identifies pure-image rows and their actions by first filename and remaining count", async () => {
    const user = userEvent.setup();
    const first = queuedImageMessage("queued-first", ["first.png"]);
    const second = queuedImageMessage("queued-second", ["second.png", "third.png"]);
    const onSteer = vi.fn();
    const onRetry = vi.fn();
    const onDelete = vi.fn();

    render(
      <QueuedMessageList
        messages={[first, second]}
        canSteer={() => true}
        steeringIds={new Set()}
        failedPromotionIds={new Set()}
        onSteer={onSteer}
        onRetry={onRetry}
        onDelete={onDelete}
      />
    );

    const rows = screen.getAllByRole("listitem");
    expect(within(rows[0]).getByText("first.png")).toBeInTheDocument();
    expect(within(rows[0]).getByText("1 张图片")).toBeInTheDocument();
    expect(within(rows[1]).getByText("second.png + 另外 1 张")).toBeInTheDocument();
    expect(within(rows[1]).getByText("2 张图片")).toBeInTheDocument();

    await user.click(screen.getByRole("button", {
      name: "将“first.png”引导到当前回合"
    }));
    await user.click(screen.getByRole("button", {
      name: "删除排队消息“second.png + 另外 1 张”"
    }));

    expect(onSteer).toHaveBeenCalledWith(first);
    expect(onRetry).not.toHaveBeenCalled();
    expect(onDelete).toHaveBeenCalledWith(second);
  });

  it("offers one quiet explicit retry action only for a failed promotion", async () => {
    const user = userEvent.setup();
    const healthy = queuedImageMessage("queued-healthy", ["healthy.png"]);
    const failed = queuedImageMessage("queued-failed", ["failed.png"]);
    const onRetry = vi.fn();

    render(
      <QueuedMessageList
        messages={[healthy, failed]}
        canSteer={() => false}
        steeringIds={new Set()}
        failedPromotionIds={new Set([failed.id])}
        onSteer={() => undefined}
        onRetry={onRetry}
        onDelete={() => undefined}
      />
    );

    expect(screen.queryByRole("button", {
      name: "重试发送排队消息“healthy.png”"
    })).not.toBeInTheDocument();
    const retry = screen.getByRole("button", {
      name: "重试发送排队消息“failed.png”"
    });
    await user.click(retry);

    expect(onRetry).toHaveBeenCalledTimes(1);
    expect(onRetry).toHaveBeenCalledWith(failed);
  });
});
