import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { PendingForkRequest } from "../types";
import { ForkRequestTray } from "./ForkRequestTray";

function forkRequest(overrides: Partial<PendingForkRequest> = {}): PendingForkRequest {
  return {
    forkId: "fork-1",
    workspaceId: "workspace-1",
    sourceConversationId: "conversation-1",
    sourceTitle: "父会话",
    prompt: "去查一下侧车协议",
    inheritContext: true,
    requestedAt: "2026-09-05T10:00:00Z",
    ...overrides
  };
}

function cardIds(): string[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".fork-request-card"))
    .map((card) => card.dataset.forkId ?? "");
}

describe("ForkRequestTray", () => {
  it("renders nothing when there is no pending request", () => {
    const { container } = render(<ForkRequestTray requests={[]} onDecide={vi.fn()} />);

    expect(container.innerHTML).toBe("");
    expect(document.querySelector(".fork-request-tray")).toBeNull();
  });

  it("renders one card per request with the newest on top", () => {
    const requests = [
      forkRequest({ forkId: "fork-old", sourceTitle: "较早的会话", prompt: "较早的提示" }),
      forkRequest({ forkId: "fork-new", sourceTitle: "较新的会话", prompt: "较新的提示", inheritContext: false })
    ];

    render(<ForkRequestTray requests={requests} onDecide={vi.fn()} />);

    expect(cardIds()).toEqual(["fork-new", "fork-old"]);
    expect(screen.getByText("来自：较新的会话")).toBeTruthy();
    expect(screen.getByText("来自：较早的会话")).toBeTruthy();
    expect(screen.getByText("较新的提示")).toBeTruthy();
    expect(screen.getByText("较早的提示")).toBeTruthy();
    expect(screen.getByText("继承上下文：否")).toBeTruthy();
    expect(screen.getByText("继承上下文：是")).toBeTruthy();
  });

  it("reports the decision for the card that was acted on", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(<ForkRequestTray requests={[forkRequest({ forkId: "fork-7" })]} onDecide={onDecide} />);

    await user.click(screen.getByRole("button", { name: "批准" }));
    expect(onDecide).toHaveBeenCalledWith("fork-7", true);

    await user.click(screen.getByRole("button", { name: "拒绝" }));
    expect(onDecide).toHaveBeenNthCalledWith(2, "fork-7", false);
  });

  it("offers an expand toggle only for a long prompt", async () => {
    const user = userEvent.setup();
    const longPrompt = "很长的提示".repeat(60);
    render(
      <ForkRequestTray
        requests={[forkRequest({ forkId: "fork-short" }), forkRequest({ forkId: "fork-long", prompt: longPrompt })]}
        onDecide={vi.fn()}
      />
    );

    const longCard = document.querySelector<HTMLElement>('[data-fork-id="fork-long"]')!;
    const shortCard = document.querySelector<HTMLElement>('[data-fork-id="fork-short"]')!;
    expect(shortCard.querySelector(".fork-request-card__toggle")).toBeNull();

    const prompt = longCard.querySelector<HTMLElement>(".fork-request-card__prompt")!;
    expect(prompt.className).not.toContain("fork-request-card__prompt--expanded");

    await user.click(screen.getByRole("button", { name: "展开" }));
    expect(prompt.className).toContain("fork-request-card__prompt--expanded");

    await user.click(screen.getByRole("button", { name: "收起" }));
    expect(prompt.className).not.toContain("fork-request-card__prompt--expanded");
  });

  it("jumps to the source conversation when the title is clicked", async () => {
    const user = userEvent.setup();
    const onOpenSource = vi.fn();
    render(
      <ForkRequestTray
        requests={[forkRequest({ workspaceId: "workspace-9", sourceConversationId: "conversation-9" })]}
        onDecide={vi.fn()}
        onOpenSource={onOpenSource}
      />
    );

    await user.click(screen.getByRole("button", { name: "来自：父会话" }));
    expect(onOpenSource).toHaveBeenCalledWith("workspace-9", "conversation-9");
  });

  it("keeps the source title inert when no jump handler is given", () => {
    render(<ForkRequestTray requests={[forkRequest()]} onDecide={vi.fn()} />);

    expect(screen.queryByRole("button", { name: "来自：父会话" })).toBeNull();
    expect(screen.getByText("来自：父会话")).toBeTruthy();
  });
});
