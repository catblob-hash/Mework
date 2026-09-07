import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { UsageStatsCard } from "./UsageStatsCard";
import { configureI18n } from "../i18n";
import type { UsageStatistics } from "../lib/usageStatistics";

afterEach(() => configureI18n("zh-CN"));

function hourAt(year: number, month: number, day: number, hour: number): number {
  return new Date(year, month - 1, day, hour, 0, 0, 0).getTime();
}

const now = hourAt(2026, 8, 20, 12);

const statistics: UsageStatistics = {
  generatedAtMs: now,
  ledgerStartedAtMs: 0,
  activity: [
    { hourStartMs: hourAt(2026, 8, 19, 20), userMessages: 4, assistantMessages: 5, sessions: 2 },
    { hourStartMs: hourAt(2026, 8, 20, 9), userMessages: 1, assistantMessages: 1, sessions: 1 }
  ],
  usage: [
    {
      hourStartMs: hourAt(2026, 8, 19, 20),
      providerId: "p",
      providerName: "Provider",
      modelId: "big-model",
      origin: "conversation",
      requests: 3,
      inputTokens: 900_000,
      cachedInputTokens: 100_000,
      outputTokens: 100_000,
      totalTokens: 1_000_000
    },
    {
      hourStartMs: hourAt(2026, 8, 20, 9),
      providerId: "p",
      providerName: "Provider",
      modelId: "small-model",
      origin: "subagent",
      requests: 1,
      inputTokens: 400,
      cachedInputTokens: 0,
      outputTokens: 100,
      totalTokens: 500
    }
  ]
};

function tile(label: string): string {
  const cell = screen.getByText(label).closest(".usage-stats__tile");
  return within(cell as HTMLElement).getByRole("strong").textContent ?? "";
}

describe("UsageStatsCard", () => {
  it("fills the eight tiles from the collected buckets", () => {
    render(<UsageStatsCard statistics={statistics} />);
    expect(tile("会话数")).toBe("3");
    expect(tile("消息数")).toBe("11");
    expect(tile("总 token")).toBe("1M");
    expect(tile("活跃天数")).toBe("2");
    expect(tile("当前连续")).toBe("2 天");
    expect(tile("最长连续")).toBe("2 天");
    expect(tile("高峰时段")).toBe("20 时");
    expect(tile("最常用模型")).toBe("big-model");
  });

  it("narrows every tile when the range switches to 7 days", async () => {
    const user = userEvent.setup();
    render(<UsageStatsCard statistics={{
      ...statistics,
      activity: [
        { hourStartMs: hourAt(2026, 5, 1, 9), userMessages: 40, assistantMessages: 0, sessions: 9 },
        ...statistics.activity
      ]
    }} />);
    expect(tile("消息数")).toBe("51");

    await user.click(screen.getByRole("button", { name: "7 天" }));
    expect(tile("消息数")).toBe("11");
    expect(tile("会话数")).toBe("3");
  });

  it("breaks usage down per model on the models tab", async () => {
    const user = userEvent.setup();
    render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText("big-model")).toBeInTheDocument();
    expect(within(rows[0]).getByText("900K 输入 · 100K 输出 · 100K 缓存")).toBeInTheDocument();
    expect(within(rows[0]).getByText("100.0%")).toBeInTheDocument();
    expect(within(rows[1]).getByText("small-model")).toBeInTheDocument();
    expect(within(rows[1]).getByText("400 输入 · 100 输出 · 0 缓存")).toBeInTheDocument();
    expect(within(rows[1]).getByText("0.0%")).toBeInTheDocument();
    // Requests and exact counts moved off the surface and onto the row title.
    expect(rows[0]).toHaveAttribute(
      "title",
      "Provider · 3 次请求 · 输入 900,000 · 输出 100,000 · 缓存 100,000 · 总计 1,000,000"
    );
  });

  it("stacks one bar per day and names the peak on the axis", async () => {
    const user = userEvent.setup();
    const { container } = render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    // The ledger opens on Aug 19 and `now` is Aug 20, so the window is two days.
    const columns = container.querySelectorAll(".usage-stats__chart-column");
    expect(columns).toHaveLength(2);
    expect(columns[0]).toHaveAttribute("data-bucket", "2026-08-19");
    expect(columns[1]).toHaveAttribute("data-bucket", "2026-08-20");
    expect(columns[0].querySelectorAll(".usage-stats__chart-segment")).toHaveLength(1);
    // 1M rounds up to a 1M axis, so Aug 19 is a full-height bar.
    expect(columns[0].querySelector(".usage-stats__chart-segment"))
      .toHaveStyle({ height: "100%" });
    expect(screen.getByText("1M")).toBeInTheDocument();
    expect(screen.getByText("500K")).toBeInTheDocument();
  });

  it("names the day and its models only while a bar is hovered", async () => {
    const user = userEvent.setup();
    const { container } = render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));
    expect(container.querySelector(".usage-stats__chart-tip")).toBeNull();

    await user.hover(container.querySelectorAll(".usage-stats__chart-column")[0]);
    const tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(tip).not.toBeNull();
    expect(within(tip).getByText("8月19日")).toBeInTheDocument();
    expect(within(tip).getByText("big-model")).toBeInTheDocument();
    expect(within(tip).getByText("1M")).toBeInTheDocument();
    // Only that day's models — small-model did not run until Aug 20.
    expect(within(tip).queryByText("small-model")).toBeNull();

    await user.unhover(container.querySelector(".usage-stats__chart-columns") as HTMLElement);
    expect(container.querySelector(".usage-stats__chart-tip")).toBeNull();
  });

  it("steps through the buckets from the keyboard, not just the mouse", async () => {
    const user = userEvent.setup();
    const { container } = render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    const plot = container.querySelector(".usage-stats__chart-plot") as HTMLElement;
    act(() => plot.focus());
    expect(plot).toHaveFocus();
    // Focusing the chart lands on the most recent bucket rather than nothing.
    let tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(within(tip).getByText("8月20日")).toBeInTheDocument();

    await user.keyboard("{ArrowLeft}");
    tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(within(tip).getByText("8月19日")).toBeInTheDocument();
    // Stops at the oldest bucket rather than wrapping or going out of range.
    await user.keyboard("{ArrowLeft}{ArrowLeft}");
    tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(within(tip).getByText("8月19日")).toBeInTheDocument();

    await user.keyboard("{End}");
    tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(within(tip).getByText("8月20日")).toBeInTheDocument();
    // The spoken copy lives in a region that is mounted whether or not a bucket
    // is selected, so a screen reader is not asked to notice a new node.
    const live = container.querySelector("[aria-live]") as HTMLElement;
    expect(live.textContent).toBe("8月20日 · small-model 500");

    await user.keyboard("{Escape}");
    expect(container.querySelector(".usage-stats__chart-tip")).toBeNull();
    expect(live.textContent).toBe("");
  });

  it("restates the numbers the old table announced through its headers", async () => {
    const user = userEvent.setup();
    render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    expect(screen.getByText(
      "big-model · Provider · 3 次请求 · 输入 900,000 · 输出 100,000 · 缓存 100,000 · 总计 1,000,000 · 占 100.0%"
    )).toBeInTheDocument();
  });

  it("keeps the legend short until the rest are asked for", async () => {
    const user = userEvent.setup();
    render(<UsageStatsCard statistics={{
      ...statistics,
      usage: Array.from({ length: 9 }, (_, index) => ({
        hourStartMs: hourAt(2026, 8, 19, 20),
        providerId: "p",
        providerName: "Provider",
        modelId: `model-${index}`,
        origin: "conversation" as const,
        requests: 1,
        inputTokens: 100 - index,
        cachedInputTokens: 0,
        outputTokens: 0,
        totalTokens: 100 - index
      }))
    }} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    expect(screen.getAllByRole("listitem")).toHaveLength(6);
    expect(screen.queryByText("model-8")).toBeNull();
    await user.click(screen.getByRole("button", { name: "显示其余 3 个" }));
    expect(screen.getAllByRole("listitem")).toHaveLength(9);
    expect(screen.getByText("model-8")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "收起" }));
    expect(screen.getAllByRole("listitem")).toHaveLength(6);
  });

  it("counts the models a crowded hover card could not fit", async () => {
    const user = userEvent.setup();
    const { container } = render(<UsageStatsCard statistics={{
      ...statistics,
      usage: Array.from({ length: 11 }, (_, index) => ({
        hourStartMs: hourAt(2026, 8, 19, 20),
        providerId: "p",
        providerName: "Provider",
        modelId: `model-${index}`,
        origin: "conversation" as const,
        requests: 1,
        inputTokens: 100 - index,
        cachedInputTokens: 0,
        outputTokens: 0,
        totalTokens: 100 - index
      }))
    }} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));

    await user.hover(container.querySelectorAll(".usage-stats__chart-column")[0]);
    const tip = container.querySelector(".usage-stats__chart-tip") as HTMLElement;
    expect(tip.querySelectorAll(".usage-stats__chart-tip-row")).toHaveLength(8);
    // The overflow is stated rather than silently trimmed.
    expect(within(tip).getByText("还有 3 个模型")).toBeInTheDocument();
    // Every model is still reachable in the legend behind the toggle.
    await user.click(screen.getByRole("button", { name: "显示其余 5 个" }));
    expect(screen.getByText("model-10")).toBeInTheDocument();
  });

  it("thins the date labels instead of stacking them on top of each other", async () => {
    const user = userEvent.setup();
    const { container } = render(<UsageStatsCard statistics={statistics} />);
    await user.click(screen.getByRole("tab", { name: "模型" }));
    await user.click(screen.getByRole("button", { name: "30 天" }));

    expect(container.querySelectorAll(".usage-stats__chart-column")).toHaveLength(30);
    const labels = [...container.querySelectorAll(".usage-stats__chart-day")];
    expect(labels.length).toBeLessThanOrEqual(7);
    // The most recent bucket always keeps its date, whatever the stride works out to.
    expect(labels.at(-1)?.textContent).toBe("8月20日");
  });

  it("drops further labels once the plot is measured too narrow for them", async () => {
    let notify: ResizeObserverCallback | null = null;
    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        notify = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    try {
      const user = userEvent.setup();
      const { container } = render(<UsageStatsCard statistics={statistics} />);
      await user.click(screen.getByRole("tab", { name: "模型" }));
      await user.click(screen.getByRole("button", { name: "30 天" }));
      const wide = container.querySelectorAll(".usage-stats__chart-day").length;

      // 230px fits four "8月12日" labels, not the seven a wide card allows.
      act(() => notify?.(
        [{ contentRect: { width: 230 } } as ResizeObserverEntry],
        {} as ResizeObserver
      ));
      const narrow = [...container.querySelectorAll(".usage-stats__chart-day")];
      expect(narrow.length).toBeLessThan(wide);
      expect(narrow.length).toBe(4);
      // Thinning never costs the most recent bucket its date.
      expect(narrow.at(-1)?.textContent).toBe("8月20日");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("reads the total back as a purr duration", () => {
    render(<UsageStatsCard statistics={statistics} />);
    expect(screen.getByText("相当于一只猫连续呼噜了 1.00 小时。")).toBeInTheDocument();
  });

  it("says the ledger is empty instead of inventing a comparison", () => {    render(<UsageStatsCard statistics={{ ...statistics, usage: [] }} />);
    expect(screen.getByText(/还没有记录到任何 token 用量/)).toBeInTheDocument();
    expect(tile("总 token")).toBe("0");
  });

  it("reads the peak hour in 12-hour form under English", () => {
    configureI18n("en-US");
    render(<UsageStatsCard statistics={statistics} />);
    expect(tile("Peak hour")).toBe("8 PM");
    expect(tile("Current streak")).toBe("2d");
  });
});
