import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { HistoryPane } from "./HistoryPane";
import type { WireRequestDetail, WireRequestPart, WireRequestSummary } from "../lib/runtime";

const runtime = vi.hoisted(() => ({
  listWireRequests: vi.fn(),
  loadWireRequest: vi.fn()
}));

vi.mock("../lib/runtime", () => runtime);

function summary(seq: number, overrides: Partial<WireRequestSummary> = {}): WireRequestSummary {
  return {
    seq,
    createdAt: "2026-09-15T10:00:00Z",
    kind: "model",
    requestId: "run_a",
    round: 1,
    attempt: 1,
    providerName: "Anthropic",
    family: "anthropic",
    modelId: "claude-opus-5",
    partCount: 2,
    bytes: 2048,
    messagesAdded: 0,
    messagesRemoved: 0,
    ...overrides
  };
}

function message(ordinal: number, value: unknown, hash: string): WireRequestPart {
  const body = JSON.stringify(value);
  return {
    ordinal,
    kind: "message",
    hash,
    body,
    bytes: new TextEncoder().encode(body).length,
    truncated: false
  };
}

function detail(seq: number, parts: WireRequestPart[]): WireRequestDetail {
  return {
    summary: summary(seq, { partCount: parts.length }),
    envelope: { family: "anthropic", modelId: "claude-opus-5", $omitted: ["apiKey"] },
    parts
  };
}

const question = message(0, { role: "user", content: "看看这个文件" }, "h-question");
const answer = message(1, { role: "assistant", content: "好的" }, "h-answer");

beforeEach(() => {
  runtime.listWireRequests.mockReset();
  runtime.loadWireRequest.mockReset();
  // A turn is drawn from every payload it issued, so opening one reads them all.
  // Tests that do not care which say so by answering every read the same way.
  runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
    detail(seq, [question])
  );
});

function paneProps() {
  return { conversationId: "conv_1", contexts: [], streaming: false };
}

/** The turn rows of the ledger, oldest first. */
function turnRows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".history-pane__row--round")];
}

/** The rule one recorded request left inside its turn, or null if unopened. */
function queryRule(seq: number): HTMLElement | null {
  const index = screen
    .queryAllByText(String(seq))
    .find((node) => node.classList.contains("history-pane__index"));
  return index?.closest("button") ?? null;
}

function rule(seq: number): HTMLElement {
  const found = queryRule(seq);
  if (!found) throw new Error(`no rule for request ${seq}`);
  return found;
}

describe("HistoryPane", () => {
  it("says recording has not started rather than showing an empty list", async () => {
    runtime.listWireRequests.mockResolvedValue([]);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByText(/还没有记录到发出去的请求/)).toBeInTheDocument();
  });

  it("groups the requests into the turns that issued them, newest turn open", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_a" }),
      summary(3, { requestId: "run_b", messagesAdded: 1 })
    ]);
    render(<HistoryPane {...paneProps()} />);

    // Two turns, and only the newest is open, so request 3 is the one on screen.
    await waitFor(() => expect(turnRows()).toHaveLength(2));
    await waitFor(() => expect(rule(3)).toBeInTheDocument());
    expect(queryRule(1)).toBeNull();

    await userEvent.click(turnRows()[0]);
    await waitFor(() => expect(rule(1)).toBeInTheDocument());
    expect(rule(2)).toBeInTheDocument();
  });

  it("keeps a run that brought no message of the user's in the turn before it", async () => {
    // A bare Send or a task wake opens a new run while continuing the round.
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 0 })
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(turnRows()).toHaveLength(1));
    await waitFor(() => expect(rule(1)).toBeInTheDocument());
    expect(rule(2)).toBeInTheDocument();
  });

  it("shows what a turn cost beside the model it ran on", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, {
        messagesAdded: 1,
        usage: { inputTokens: 12_400, cachedInputTokens: 8_100, outputTokens: 210 }
      })
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(turnRows()).toHaveLength(1));
    const row = turnRows()[0];
    expect(within(row).getByText("claude-opus-5")).toBeInTheDocument();
    expect(row.textContent).toContain("↑12k");
    expect(row.textContent).toContain("⚡8.1k");
    expect(row.textContent).toContain("↓210");
  });

  it("says a turn recorded no usage rather than drawing it as zero", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { messagesAdded: 1 })]);
    render(<HistoryPane {...paneProps()} />);
    await waitFor(() => expect(turnRows()).toHaveLength(1));
    expect(within(turnRows()[0]).getAllByText("—").length).toBeGreaterThan(0);
  });

  it("counts the messages the user added and removed on the turn", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { messagesAdded: 2, messagesRemoved: 1 })
    ]);
    render(<HistoryPane {...paneProps()} />);

    const added = await screen.findAllByText("+2");
    const removed = screen.getAllByText("−1");
    expect(added[0]).toHaveAttribute("data-status", "added");
    expect(removed[0]).toHaveAttribute("data-status", "removed");
  });

  it("draws no counts at all for a turn that changed nothing", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1)]);
    render(<HistoryPane {...paneProps()} />);
    await waitFor(() => expect(turnRows()).toHaveLength(1));
    expect(document.querySelector(".history-pane__counts")).toBeNull();
  });

  it("names a host-minted native request for what it is on its own rule", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { kind: "search", requestId: "", messagesAdded: 1 })
    ]);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByText("原生搜索")).toBeInTheDocument();
  });

  it("marks a retry of the same payload as another attempt", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { attempt: 2, messagesAdded: 1 })]);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByText("第 2 次尝试")).toBeInTheDocument();
  });

  it("opens a turn into one row per part, and a part into its text", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { messagesAdded: 1 })]);
    runtime.loadWireRequest.mockResolvedValue(detail(1, [question, answer]));
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledWith("conv_1", 1));

    expect(await screen.findByText("user")).toBeInTheDocument();
    expect(screen.getByText("assistant")).toBeInTheDocument();
    // The preview is on the closed row; the body only appears once it opens.
    await userEvent.click(screen.getByText("user").closest("button") as HTMLElement);
    const body = document.querySelector(".history-pane__text");
    expect(body?.textContent).toContain("看看这个文件");
  });

  it("folds the history a turn replayed and leaves what it added in front", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 1 })
    ]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      seq === 1 ? detail(1, [question]) : detail(2, [question, answer])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));

    // The replayed question is behind the rule; the appended answer is not.
    const fold = await screen.findByText("此前 1 条消息");
    expect(screen.queryByText("user")).not.toBeInTheDocument();
    expect(screen.getByText("assistant")).toBeInTheDocument();

    await userEvent.click(fold.closest("button") as HTMLElement);
    expect(screen.getByText("user")).toBeInTheDocument();
  });

  it("rules off each request of a turn under what it was the first to carry", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_a" })
    ]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      seq === 1 ? detail(1, [question]) : detail(2, [question, answer])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));
    // One list: the user's message, the rule request 1 left on, the answer it
    // brought back, and the rule request 2 left on.
    const rows = [...document.querySelectorAll(".history-pane__parts > li button")];
    await waitFor(() => expect(rows.length).toBeGreaterThan(0));
    const shape = [...document.querySelectorAll(".history-pane__parts > li button")].map((row) =>
      row.classList.contains("history-pane__row--rule")
        ? `rule:${row.querySelector(".history-pane__index")?.textContent}`
        : row.querySelector(".history-pane__role")?.textContent
    );
    expect(shape).toEqual(["user", "rule:1", "assistant", "rule:2"]);
  });

  it("paints a message the user added green and one it removed red", async () => {
    const typed = message(1, { role: "user", content: "再看看这个" }, "h-typed");
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 1, messagesRemoved: 1 })
    ]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      seq === 1 ? detail(1, [question, answer]) : detail(2, [question, typed])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));

    await waitFor(() => {
      const tones = [...document.querySelectorAll(".history-pane__row--part[data-tone]")].map(
        (row) => row.getAttribute("data-tone")
      );
      expect(tones).toContain("user-added");
      expect(tones).toContain("removed");
    });
  });

  it("leaves the turn's own output uncoloured, so only the user's edits stand out", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { messagesAdded: 1 }), summary(2)]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      seq === 1 ? detail(1, [question]) : detail(2, [question, answer])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));

    const assistant = await screen.findByText("assistant");
    expect(assistant.closest("button")).toHaveAttribute("data-tone", "output");
  });

  it("opens a rewritten message into its own line diff", async () => {
    const edited = message(0, { role: "user", content: "看看这两个文件" }, "h-edited");
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 1 })
    ]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      seq === 1 ? detail(1, [question]) : detail(2, [edited])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));

    const stat = await screen.findByText("+1 −1 行");
    await userEvent.click(stat.closest("button") as HTMLElement);
    const diff = document.querySelector(".history-pane__diff")?.textContent ?? "";
    expect(diff).toContain("看看这两个文件");
  });

  it("says a turn carried exactly what the request before it carried", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 1, attempt: 2 })
    ]);
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) =>
      detail(seq, [question])
    );
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/完全相同/)).toBeInTheDocument();
  });

  it("attributes nothing in a turn whose predecessor the ledger no longer holds", async () => {
    // Retention prunes from the front, so the oldest surviving turn has nothing
    // to be read against and must not report its whole history as new.
    runtime.listWireRequests.mockResolvedValue([summary(7, { messagesAdded: 1 })]);
    runtime.loadWireRequest.mockResolvedValue(detail(7, [question, answer]));
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(runtime.loadWireRequest).toHaveBeenCalledWith("conv_1", 7));

    expect(await screen.findByText("此前 2 条消息")).toBeInTheDocument();
    expect(screen.queryByText(/完全相同/)).not.toBeInTheDocument();
  });

  it("shows the payload that was actually sent under the rule the request left", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { messagesAdded: 1 })]);
    runtime.loadWireRequest.mockResolvedValue(detail(1, [question]));
    render(<HistoryPane {...paneProps()} />);

    await userEvent.click(await waitFor(() => rule(1)));

    const json = document.querySelector(".history-pane__text--json")?.textContent ?? "";
    expect(json).toContain("\"messages\"");
    expect(json).toContain("看看这个文件");
    // Credentials are absent by construction, and the payload says so.
    expect(json).toContain("$omitted");
    expect(json).not.toContain("apiKey\":");
  });

  it("keeps the ledger visible when one request's parts cannot be read, and retries", async () => {
    runtime.listWireRequests.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 1 })
    ]);
    let refused = false;
    runtime.loadWireRequest.mockImplementation(async (_id: string, seq: number) => {
      if (seq === 2 && !refused) {
        refused = true;
        throw new Error("分段读取失败");
      }
      return detail(seq, [question]);
    });
    render(<HistoryPane {...paneProps()} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("分段读取失败");
    // The other turn is still there; only this payload failed.
    expect(turnRows()).toHaveLength(2);

    await userEvent.click(within(alert).getByRole("button", { name: "重试" }));
    expect(await waitFor(() => rule(2))).toBeInTheDocument();
  });

  it("says so when a payload a turn is drawn from is no longer in the ledger", async () => {
    runtime.listWireRequests.mockResolvedValue([summary(1, { messagesAdded: 1 })]);
    runtime.loadWireRequest.mockResolvedValue(null);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("已不在账本里");
  });

  it("surfaces a read failure instead of an empty pane", async () => {
    runtime.listWireRequests.mockRejectedValue(new Error("对话库不可用"));
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("对话库不可用");
  });

  it("reads the conversation's own ledger when no agent is named", async () => {
    runtime.listWireRequests.mockResolvedValue([]);
    render(<HistoryPane {...paneProps()} />);
    await screen.findByText(/这个对话还没有记录到发出去的请求/);
    expect(runtime.listWireRequests).toHaveBeenCalledWith("conv_1", undefined);
  });

  it("reads one agent's ledger, and says so when that agent has sent nothing", async () => {
    runtime.listWireRequests.mockResolvedValue([]);
    const { rerender } = render(<HistoryPane {...paneProps()} owners={["reviewer"]} />);
    expect(await screen.findByText(/这个子代理还没有记录到发出去的请求/)).toBeInTheDocument();
    expect(runtime.listWireRequests).toHaveBeenCalledWith("conv_1", ["reviewer"]);

    // The host separates a child's traffic from the session's own, so an agent
    // with nothing recorded must not fall back to the conversation's rows.
    expect(runtime.listWireRequests).not.toHaveBeenCalledWith("conv_1", undefined);

    // A list rebuilt on every render is the same ledger; re-reading it once a
    // render would poll the store for nothing.
    const reads = runtime.listWireRequests.mock.calls.length;
    rerender(<HistoryPane {...paneProps()} owners={["reviewer"]} />);
    await waitFor(() => expect(runtime.listWireRequests.mock.calls.length).toBe(reads));
  });
});
