import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
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
