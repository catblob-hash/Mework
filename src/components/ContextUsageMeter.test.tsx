import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { ContextItem } from "../types";
import { ContextUsageMeter, type ContextUsageMeterProps } from "./ContextUsageMeter";

function contexts(): ContextItem[] {
  return [
    // The system prompt is a timeline card now, so it reaches the meter through
    // `contexts` like every other component of the breakdown.
    { id: "s1", kind: "system", content: "s".repeat(200), createdAt: "2026-08-27T00:00:00Z" },
    { id: "u1", kind: "user", content: "u".repeat(400), createdAt: "2026-08-27T00:00:00Z" },
    { id: "a1", kind: "assistant", content: "a".repeat(800), createdAt: "2026-08-27T00:00:00Z" },
    { id: "r1", kind: "reasoning", content: "r".repeat(200), createdAt: "2026-08-27T00:00:00Z" },
    {
      id: "t1",
      kind: "tool",
      toolName: "read_file",
      input: {},
      result: {
        success: true,
        output: "t".repeat(1200),
        executedAt: "2026-08-27T00:00:00Z",
        durationMs: 1
      },
      createdAt: "2026-08-27T00:00:00Z"
    }
  ];
}

function renderMeter(overrides: Partial<ContextUsageMeterProps> = {}) {
  render(
    <ContextUsageMeter
      contexts={contexts()}
      tokens={180_000}
      estimated={false}
      contextWindow={1_000_000}
      counts={{ tools: 38, mcpServers: 2, skills: 4, agentRoles: 5 }}
      {...overrides}
    />
  );
}

describe("ContextUsageMeter", () => {
  beforeEach(() => configureI18n("zh-CN"));

  it("puts the numbers on the ring's accessible name instead of into the composer row", async () => {
    const user = userEvent.setup();
    renderMeter();

    const trigger = screen.getByRole("button", { name: "上下文用量：180k / 1m（18%）" });
    // The ring contains no text so the bottom row width remains stable as values change each turn.
    expect(trigger).toHaveTextContent("");

    await user.click(trigger);
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    expect(within(panel).getByText("180k / 1m（18%）")).toBeInTheDocument();
  });

  it("lists the composition, free space and the per-conversation counts", async () => {
    const user = userEvent.setup();
    renderMeter();

    await user.click(screen.getByRole("button", { name: /^上下文用量：/ }));
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });

    for (const label of ["系统提示词", "用户消息", "助手回复", "思考", "工具调用", "其他（工具定义等）", "剩余空间"]) {
      expect(within(panel).getByText(label)).toBeInTheDocument();
    }
    // Counts belong to the conversation's four lists, not global assets.
    const counts = panel.querySelector(".context-usage-panel__counts");
    expect(counts).toHaveTextContent("工具38");
    expect(counts).toHaveTextContent("MCP2");
    expect(counts).toHaveTextContent("技能4");
    expect(counts).toHaveTextContent("角色5");
  });

  it("ends at the counts row, with no settings action or estimate note", async () => {
    const user = userEvent.setup();
    renderMeter();

    const trigger = screen.getByRole("button", { name: /^上下文用量：/ });
    expect(trigger).not.toBeDisabled();
    await user.click(trigger);
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    expect(within(panel).queryByRole("button", { name: "打开上下文管理设置" })).not.toBeInTheDocument();
    expect(panel).not.toHaveTextContent("成分按本地估算拆分");
    expect(panel.lastElementChild).toHaveClass("context-usage-panel__counts");
  });

  it("marks an estimated total and drops the percentage when no window is declared", async () => {
    const user = userEvent.setup();
    renderMeter({ estimated: true, contextWindow: null, tokens: 12_345 });

    await user.click(screen.getByRole("button", { name: "上下文用量：~12.3k" }));
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    expect(within(panel).queryByText("剩余空间")).not.toBeInTheDocument();
  });

  it("says a share is too small to print instead of rounding it to zero", async () => {
    const user = userEvent.setup();
    // At 45 tokens in a 1m window, one-decimal formatting would render every row as "0.0%".
    renderMeter({ tokens: 45 });

    await user.click(screen.getByRole("button", { name: /^上下文用量：/ }));
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    expect(within(panel).getAllByText("<0.1%").length).toBeGreaterThan(0);
    expect(within(panel).queryByText("0.0%")).not.toBeInTheDocument();
    expect(within(panel).getByText("100.0%")).toBeInTheDocument();
  });

  it("says so when the model cannot project usage at all", async () => {
    const user = userEvent.setup();
    renderMeter({ unprojectable: true });

    await user.click(screen.getByRole("button", { name: "上下文用量：不可投影" }));
    const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
    expect(within(panel).getByText("不可投影")).toBeInTheDocument();
  });

  describe("auto-compact", () => {
    async function openAutoCompact(overrides: Partial<ContextUsageMeterProps> = {}) {
      const user = userEvent.setup();
      const onAutoCompactChange = vi.fn();
      renderMeter({
        autoCompact: { enabled: true, thresholdPercent: 80 },
        onAutoCompactChange,
        ...overrides
      });
      await user.click(screen.getByRole("button", { name: /^上下文用量：/ }));
      const panel = await screen.findByRole("dialog", { name: "上下文窗口用量" });
      return { user, panel, onAutoCompactChange };
    }

    it("opens its submenu from a row at the foot of the panel", async () => {
      const { user, panel } = await openAutoCompact();
      expect(panel.lastElementChild).toHaveClass("context-usage-compact");
      const row = within(panel).getByRole("button", { name: /自动压缩/ });
      expect(row).toHaveTextContent("80%");
      expect(row).toHaveAttribute("aria-expanded", "false");
      expect(screen.queryByRole("group", { name: "自动压缩" })).not.toBeInTheDocument();

      await user.click(row);
      expect(row).toHaveAttribute("aria-expanded", "true");
      const menu = screen.getByRole("group", { name: "自动压缩" });
      expect(within(menu).getByRole("switch", { name: "启用自动压缩" })).toHaveAttribute("aria-checked", "true");
      expect(within(menu).getByRole("spinbutton", { name: "压缩阈值（百分比）" })).toHaveValue(80);
      const slider = within(menu).getByRole("slider", { name: "压缩阈值" });
      expect(slider).toHaveValue("80");
      expect(slider).toHaveAttribute("min", "20");
      expect(slider).toHaveAttribute("max", "97");
      // 80% of the 1m window.
      expect(menu).toHaveTextContent("上下文达到 800k tokens 时，模型写好交接文档，在新的交接会话中继续");

      await user.click(row);
      expect(screen.queryByRole("group", { name: "自动压缩" })).not.toBeInTheDocument();
    });

    it("marks the threshold on the usage bar", async () => {
      const { panel } = await openAutoCompact({ autoCompact: { enabled: true, thresholdPercent: 64 } });
      const marker = panel.querySelector<HTMLElement>(".context-usage-panel__threshold");
      expect(marker?.style.left).toBe("64%");
    });

    it("switches auto-compact off without touching the threshold", async () => {
      const { user, panel, onAutoCompactChange } = await openAutoCompact();
      await user.click(within(panel).getByRole("button", { name: /自动压缩/ }));
      await user.click(screen.getByRole("switch", { name: "启用自动压缩" }));
      expect(onAutoCompactChange).toHaveBeenCalledWith({ enabled: false, thresholdPercent: 80 });
    });

    it("commits a typed threshold on Enter, clamped to 20–97", async () => {
      const { user, panel, onAutoCompactChange } = await openAutoCompact();
      await user.click(within(panel).getByRole("button", { name: /自动压缩/ }));
      const field = screen.getByRole("spinbutton", { name: "压缩阈值（百分比）" });

      await user.clear(field);
      await user.type(field, "8");
      // "8" is on its way to something; nothing is saved while typing.
      expect(onAutoCompactChange).not.toHaveBeenCalled();
      await user.type(field, "5{Enter}");
      expect(onAutoCompactChange).toHaveBeenLastCalledWith({ enabled: true, thresholdPercent: 85 });

      await user.clear(field);
      await user.type(field, "150{Enter}");
      expect(onAutoCompactChange).toHaveBeenLastCalledWith({ enabled: true, thresholdPercent: 97 });

      await user.clear(field);
      await user.type(field, "5");
      await user.tab();
      expect(onAutoCompactChange).toHaveBeenLastCalledWith({ enabled: true, thresholdPercent: 20 });
    });

    it("commits the slider when it is released, not at every step", async () => {
      const { user, panel, onAutoCompactChange } = await openAutoCompact();
      await user.click(within(panel).getByRole("button", { name: /自动压缩/ }));
      const slider = screen.getByRole("slider", { name: "压缩阈值" });

      fireEvent.input(slider, { target: { value: "60" } });
      expect(onAutoCompactChange).not.toHaveBeenCalled();
      // The number field follows the drag as it happens.
      expect(screen.getByRole("spinbutton", { name: "压缩阈值（百分比）" })).toHaveValue(60);
      fireEvent.pointerUp(slider);
      expect(onAutoCompactChange).toHaveBeenCalledTimes(1);
      expect(onAutoCompactChange).toHaveBeenCalledWith({ enabled: true, thresholdPercent: 60 });
    });

    it("rounds the token threshold down", async () => {
      const { user, panel } = await openAutoCompact({
        contextWindow: 999,
        tokens: 100,
        autoCompact: { enabled: true, thresholdPercent: 97 }
      });
      await user.click(within(panel).getByRole("button", { name: /自动压缩/ }));
      // 999 × 97% = 969.03.
      expect(screen.getByRole("group", { name: "自动压缩" })).toHaveTextContent("上下文达到 969 tokens 时");
    });

    it("disables the threshold while it is off, and says when there is no window to measure", async () => {
      const { user, panel } = await openAutoCompact({
        contextWindow: null,
        autoCompact: { enabled: false, thresholdPercent: 80 }
      });
      const row = within(panel).getByRole("button", { name: /自动压缩/ });
      expect(row).toHaveTextContent("已关闭");
      expect(panel.querySelector(".context-usage-panel__threshold")).toBeNull();
      await user.click(row);
      const menu = screen.getByRole("group", { name: "自动压缩" });
      expect(within(menu).getByRole("switch", { name: "启用自动压缩" })).toHaveAttribute("aria-checked", "false");
      expect(within(menu).getByRole("slider", { name: "压缩阈值" })).toBeDisabled();
      expect(within(menu).getByRole("spinbutton", { name: "压缩阈值（百分比）" })).toBeDisabled();
      expect(menu).toHaveTextContent("当前模型没有设置上下文窗口，无法自动压缩");
    });
  });

  it("renders in English when the app language is English", async () => {
    configureI18n("en-US");
    const user = userEvent.setup();
    renderMeter();

    await user.click(screen.getByRole("button", { name: /^Context usage:/ }));
    const panel = await screen.findByRole("dialog", { name: "Context window usage" });
    expect(within(panel).getByText("Context window")).toBeInTheDocument();
    expect(within(panel).getByText("Free space")).toBeInTheDocument();
  });
});
