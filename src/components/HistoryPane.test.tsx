import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { HistoryPane } from "./HistoryPane";
import type { HistoryEntry, HistoryEntryDetail, HistoryOp, HistoryPart, HistoryUsage } from "../lib/runtime";

const runtime = vi.hoisted(() => ({
  listHistoryEntries: vi.fn(),
  loadHistoryEntry: vi.fn()
}));

vi.mock("../lib/runtime", () => runtime);

interface RequestOverrides {
  requestId?: string;
  kind?: "model" | "search" | "fetch";
  modelId?: string;
  messagesAdded?: number;
  usage?: HistoryUsage;
}

/** A request entry, as the list carries it. */
function summary(seq: number, overrides: RequestOverrides = {}): HistoryEntry {
  const { requestId = "run_a", kind = "model", usage, ...detail } = overrides;
  return {
    seq,
    createdAt: "2026-09-15T10:00:00Z",
    kind: "request",
    requestId,
    round: 1,
    detail: {
      type: kind,
      attempt: 1,
      providerName: "Anthropic",
      family: "anthropic",
      modelId: "claude-opus-5",
      partCount: 2,
      bytes: 2048,
      messagesAdded: 0,
      ...detail
    },
    usage
  };
}

/** Any other entry, as the list carries it. */
function entry(
  seq: number,
  kind: HistoryEntry["kind"],
  detail: Record<string, unknown> = {},
  overrides: Partial<HistoryEntry> = {}
): HistoryEntry {
  return {
    seq,
    createdAt: "2026-09-15T10:00:00Z",
    kind,
    requestId: kind === "edit" ? undefined : "run_a",
    detail,
    ...overrides
  };
}

/** A body read back for an entry. */
function body(of: HistoryEntry, value: unknown): HistoryEntryDetail {
  return { entry: of, body: JSON.stringify(value), truncated: false };
}

/** A timeline change read back as its steps. */
function steps(of: HistoryEntry, ops: HistoryOp[]): HistoryEntryDetail {
  return { entry: of, truncated: false, ops };
}

/** The user's message, as the timeline change that put it there. */
function sent(seq: number, content: string): [HistoryEntry, HistoryEntryDetail] {
  const edit = entry(seq, "edit", { source: "message", inserted: 1 });
  return [
    edit,
    steps(edit, [
      {
        ordinal: 0,
        op: "insert",
        contextId: `ctx_${seq}`,
        position: 0,
        body: JSON.stringify({ kind: "user", content })
      }
    ])
  ];
}

/** A run's settlement: the rows it put on the timeline. */
function settled(seq: number, requestId: string, rows: unknown[]): [HistoryEntry, HistoryEntryDetail] {
  const run = entry(seq, "run", { source: "run", inserted: rows.length }, { requestId });
  return [
    run,
    steps(
      run,
      rows.map((row, ordinal) => ({
        ordinal,
        op: "insert" as const,
        contextId: `ctx_${seq}_${ordinal}`,
        position: ordinal,
        body: JSON.stringify(row)
      }))
    )
  ];
}

/** A request, with the payload it carried: a system prompt and whatever else is given. */
function sentRequest(
  seq: number,
  overrides: RequestOverrides,
  system = "You are Mework.",
  rest: Pick<HistoryPart, "kind" | "body">[] = []
): [HistoryEntry, HistoryEntryDetail] {
  const request = summary(seq, overrides);
  const parts = [{ kind: "system" as const, body: system }, ...rest].map((part, ordinal) => ({
    ordinal,
    ...part,
    hash: `h-${part.body}`,
    bytes: part.body.length,
    truncated: false
  }));
  return [request, { entry: request, truncated: false, body: "{}", parts }];
}

/** The tool list a request declared, as `wire_audit` records it. */
function tools(...names: string[]): Pick<HistoryPart, "kind" | "body"> {
  return {
    kind: "tools",
    body: JSON.stringify(names.map((name) => ({ description: "", inputSchema: { type: "object" }, name })))
  };
}

/** The marker a request carries where tools joined it. */
function joined(...names: string[]): Pick<HistoryPart, "kind" | "body"> {
  return {
    kind: "message",
    body: JSON.stringify({ role: "system", content: "", providerOptions: { mework: { toolAddition: names } } })
  };
}

/** Serves the pane a history and the bodies of its entries. */
function serve(pairs: (HistoryEntry | [HistoryEntry, HistoryEntryDetail])[]) {
  const list = pairs.map((pair) => (Array.isArray(pair) ? pair[0] : pair));
  const bodies = new Map(
    pairs.flatMap((pair) => (Array.isArray(pair) ? [[pair[0].seq, pair[1]] as const] : []))
  );
  runtime.listHistoryEntries.mockResolvedValue(list);
  runtime.loadHistoryEntry.mockImplementation(async (_id: string, seq: number) => bodies.get(seq) ?? null);
}

beforeEach(() => {
  runtime.listHistoryEntries.mockReset();
  runtime.loadHistoryEntry.mockReset();
  runtime.loadHistoryEntry.mockResolvedValue(null);
});

function paneProps() {
  return { conversationId: "conv_1", contexts: [], streaming: false };
}

/** The bars of the history, oldest first. */
function bars(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".history-pane__row--round")];
}

function barTitles(): (string | null)[] {
  return bars().map((bar) => bar.querySelector(".history-pane__title")?.textContent ?? null);
}

/** The rows of the open bars, top to bottom, each as the word it leads with. */
function listShape(): (string | null | undefined)[] {
  return [...document.querySelectorAll(".history-pane__parts > li > button")].map(
    (row) => row.querySelector(".history-pane__role")?.textContent
  );
}

describe("HistoryPane", () => {
  it("says recording has not started rather than showing an empty list", async () => {
    runtime.listHistoryEntries.mockResolvedValue([]);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByText(/这个对话还没有历史记录/)).toBeInTheDocument();
  });

  it("lays a turn out as the messages it added, with no request among them", async () => {
    serve([
      sent(1, "你看到追加的工具了吗"),
      sentRequest(2, { messagesAdded: 1 }),
      entry(3, "response", { finishReason: "stop" }, { answers: 2 }),
      settled(4, "run_a", [
        { kind: "system", content: "Tools added: shell", localOnly: true, toolsAdded: ["shell"] },
        { kind: "reasoning", content: "想一想" },
        { kind: "tool", toolName: "ls", input: { path: "." }, result: { success: true, output: "a.txt" } },
        { kind: "assistant", content: "只有一个文件" }
      ])
    ]);
    render(<HistoryPane {...paneProps()} />);

    await screen.findByText("只有一个文件");
    // The system prompt it opened with, what the user sent, and what the run put
    // on the timeline — the tool it was handed, its thinking, the call, the answer.
    // Each leads with its role and nothing else: no glyph beside the chevron.
    expect(listShape()).toEqual(["system", "user", "system", "reasoning", "tool", "assistant"]);
    for (const row of document.querySelectorAll(".history-pane__parts > li > button")) {
      expect(row.querySelectorAll("svg")).toHaveLength(1);
    }
    expect(document.querySelector(".history-pane__row--event")).toBeNull();
    expect(screen.queryByText(/请求/)).not.toBeInTheDocument();
  });

  it("stands the user's edits between turns as a bar of their own, with the message a deletion took away", async () => {
    const removal = entry(5, "edit", { source: "edit", removed: 1 });
    const rewrite = entry(6, "edit", { source: "edit", replaced: 1 });
    serve([
      sent(1, "随便玩一下"),
      sentRequest(2, { requestId: "run_a", messagesAdded: 1 }),
      entry(3, "response", {}, { requestId: "run_a", answers: 2 }),
      settled(4, "run_a", [{ kind: "assistant", content: "我先逛逛" }]),
      [
        removal,
        steps(removal, [
          {
            ordinal: 0,
            op: "remove",
            contextId: "ctx_4_0",
            before: JSON.stringify({ kind: "assistant", content: "我先逛逛" })
          }
        ])
      ],
      [
        rewrite,
        steps(rewrite, [
          {
            ordinal: 0,
            op: "replace",
            contextId: "ctx_1",
            body: JSON.stringify({ kind: "user", content: "在两个工作区里看看" }),
            before: JSON.stringify({ kind: "user", content: "随便玩一下" })
          }
        ])
      ],
      // The edited message sent again.
      sentRequest(7, { requestId: "run_b", messagesAdded: 0 }),
      entry(8, "response", {}, { requestId: "run_b", answers: 7 }),
      settled(9, "run_b", [{ kind: "assistant", content: "两个工作区都是 Mework" }])
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(barTitles()).toEqual(["claude-opus-5", "上下文编辑", "claude-opus-5"]));
    // No bar counts whole messages: what happened to each is said on its own row.
    expect(bars().map((bar) => bar.textContent)).not.toContainEqual(expect.stringMatching(/[+−~]1/));

    await userEvent.click(bars()[1]);
    // A message taken away whole is drawn red, and nothing more: no line count.
    const gone = (await screen.findByText("我先逛逛")).closest("button") as HTMLElement;
    expect(gone).toHaveAttribute("data-change", "remove");
    // Tagged as it is in a turn: the edits bar names roles the same way.
    expect(within(gone).getByText("assistant")).toBeInTheDocument();
    expect(gone.querySelector(".history-pane__stat")).toBeNull();
    // One whose content was edited says how many lines the edit changed.
    const rewritten = screen.getByText("在两个工作区里看看").closest("button") as HTMLElement;
    expect(rewritten).toHaveAttribute("data-change", "replace");
    expect(rewritten.querySelector(".history-pane__stat")?.textContent).toBe("+1 −1");
    await userEvent.click(rewritten);
    expect(document.querySelector(".history-pane__diff")?.textContent).toContain("随便玩一下");
  });

  it("folds a turn whose last request never got its answer into a bar titled 意外中断", async () => {
    const reply = {
      role: "assistant",
      content: [
        { type: "text", text: "我先看看" },
        { type: "tool-call", toolCallId: "c1", toolName: "shell", input: { command: "ls" } }
      ]
    };
    const response = entry(5, "response", { finishReason: "tool-calls" }, { requestId: "run_b", answers: 4 });
    const tool = entry(6, "tool", { name: "shell", rewritten: true }, { requestId: "run_b", callId: "c1" });
    serve([
      sent(1, "第一句"),
      sentRequest(2, { requestId: "run_a", messagesAdded: 1 }),
      entry(3, "response", {}, { requestId: "run_a", answers: 2 }),
      sentRequest(4, { requestId: "run_b", messagesAdded: 1 }),
      [response, body(response, reply)],
      [tool, body(tool, { input: { command: "ls -a" }, requestedInput: { command: "ls" } })],
      sentRequest(7, { requestId: "run_b", messagesAdded: 0 })
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(barTitles()).toEqual(["claude-opus-5", "意外中断"]));
    // Nothing settled, so what came back is all there is to show.
    await screen.findByText("我先看看");
    expect(listShape()).toEqual(["assistant", "tool"]);
    await userEvent.click(document.querySelector(".history-pane__row--event[data-tone='tool']") as HTMLElement);
    expect(document.querySelector(".history-pane__diff")?.textContent).toContain("ls -a");
  });

  it("does not call the turn that is still running interrupted", async () => {
    serve([
      sent(1, "第一句"),
      sentRequest(2, { requestId: "run_a", messagesAdded: 1 }),
      entry(3, "response", {}, { requestId: "run_a", answers: 2 }),
      sent(4, "第二句"),
      sentRequest(5, { requestId: "run_b", messagesAdded: 1 })
    ]);
    render(<HistoryPane {...paneProps()} streaming />);
    await waitFor(() => expect(barTitles()).toEqual(["claude-opus-5", "claude-opus-5"]));
  });

  it("draws the system prompt only in the turn that changed it", async () => {
    serve([
      sent(1, "第一句"),
      sentRequest(2, { requestId: "run_a", messagesAdded: 1 }),
      sent(3, "第二句"),
      sentRequest(4, { requestId: "run_b", messagesAdded: 1 }),
      sent(5, "第三句"),
      sentRequest(6, { requestId: "run_c", messagesAdded: 1 }, "You are Mework, careful.")
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(listShape()).toEqual(["system", "user"]));
    const changed = document.querySelector(".history-pane__row--message[data-kind='prompt']");
    expect(changed).toHaveAttribute("data-change", "replace");

    await userEvent.click(bars()[1]);
    await waitFor(() => expect(listShape()).toEqual(["user", "system", "user"]));
  });

  it("draws the tools a turn registered, and a tool a running turn was handed where it joined", async () => {
    const user = { kind: "message" as const, body: JSON.stringify({ role: "user", content: "第一句" }) };
    const first = entry(3, "response", { finishReason: "tool-calls" }, { requestId: "run_a", answers: 2 });
    serve([
      sent(1, "第一句"),
      sentRequest(2, { requestId: "run_a", messagesAdded: 1 }, undefined, [tools("ls", "grep"), user]),
      [first, body(first, { role: "assistant", content: [{ type: "text", text: "先看看" }] })],
      // The next round hands `handoff` over by append: the list carries it, the
      // declared tools did not change.
      sentRequest(4, { requestId: "run_a", messagesAdded: 0 }, undefined, [
        tools("ls", "grep", "handoff"),
        user,
        joined("handoff")
      ])
    ]);
    render(<HistoryPane {...paneProps()} streaming />);

    // The tool list first, as the model reads it, tagged as the field it is.
    await waitFor(() => expect(listShape()).toEqual(["tools", "system", "user", "assistant", "system"]));
    const registered = document.querySelector(".history-pane__row--message[data-kind='tools']") as HTMLElement;
    expect(registered).toHaveAttribute("data-change", "insert");
    expect(within(registered).getByText("注册 2 个工具")).toBeInTheDocument();
    expect(within(registered).getByText("ls, grep")).toBeInTheDocument();
    const appended = document.querySelector(".history-pane__row--message[data-kind='toolsAdded']") as HTMLElement;
    expect(within(appended).getByText("追加 1 个工具")).toBeInTheDocument();
    expect(within(appended).getByText("handoff")).toBeInTheDocument();
  });

  it("gives what nothing has sent yet a bar of its own", async () => {
    serve([sent(1, "第一句"), sentRequest(2, { messagesAdded: 1 }), sent(3, "还没发出去")]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(barTitles()).toEqual(["claude-opus-5", "尚未发出"]));
    expect(await screen.findByText("还没发出去")).toBeInTheDocument();
  });

  it("keeps a run that brought no message of the user's in the turn before it", async () => {
    // A bare Send or a task wake opens a new run while continuing the round.
    runtime.listHistoryEntries.mockResolvedValue([
      summary(1, { requestId: "run_a", messagesAdded: 1 }),
      summary(2, { requestId: "run_b", messagesAdded: 0 })
    ]);
    render(<HistoryPane {...paneProps()} />);
    await waitFor(() => expect(bars()).toHaveLength(1));
  });

  it("shows what a turn cost beside the model it ran on", async () => {
    runtime.listHistoryEntries.mockResolvedValue([
      summary(1, {
        messagesAdded: 1,
        usage: { inputTokens: 12_400, cachedInputTokens: 8_100, outputTokens: 210 }
      })
    ]);
    render(<HistoryPane {...paneProps()} />);

    await waitFor(() => expect(bars()).toHaveLength(1));
    const row = bars()[0];
    expect(within(row).getByText("claude-opus-5")).toBeInTheDocument();
    expect(row.textContent).toContain("↑12k");
    expect(row.textContent).toContain("⚡8.1k");
    expect(row.textContent).toContain("↓210");
  });

  it("says a turn recorded no usage rather than drawing it as zero", async () => {
    runtime.listHistoryEntries.mockResolvedValue([summary(1, { messagesAdded: 1 })]);
    render(<HistoryPane {...paneProps()} />);
    await waitFor(() => expect(bars()).toHaveLength(1));
    expect(within(bars()[0]).getAllByText("—").length).toBeGreaterThan(0);
  });

  it("keeps the history visible when one entry cannot be read, and retries it", async () => {
    const [message, messageBody] = sent(1, "看看这个文件");
    const [request, requestBody] = sentRequest(2, { messagesAdded: 1 });
    const [run, runBody] = settled(3, "run_a", [{ kind: "assistant", content: "好的" }]);
    runtime.listHistoryEntries.mockResolvedValue([message, request, run]);
    let refused = false;
    runtime.loadHistoryEntry.mockImplementation(async (_id: string, seq: number) => {
      if (seq === 3 && !refused) {
        refused = true;
        throw new Error("正文读取失败");
      }
      return seq === 1 ? messageBody : seq === 2 ? requestBody : seq === 3 ? runBody : null;
    });
    render(<HistoryPane {...paneProps()} />);

    // What the user sent still reads; only the settlement says it failed.
    expect(await screen.findByText("看看这个文件")).toBeInTheDocument();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("正文读取失败");

    await userEvent.click(within(alert).getByRole("button", { name: "重试" }));
    expect(await screen.findByText("好的")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("says so when an entry is no longer in the history", async () => {
    serve([entry(1, "edit", { source: "message", inserted: 1 }), sentRequest(2, { messagesAdded: 1 })]);
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("已不在历史记录里");
  });

  it("surfaces a read failure instead of an empty pane", async () => {
    runtime.listHistoryEntries.mockRejectedValue(new Error("对话库不可用"));
    render(<HistoryPane {...paneProps()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("对话库不可用");
  });

  it("reads the conversation's own history when no agent is named", async () => {
    runtime.listHistoryEntries.mockResolvedValue([]);
    render(<HistoryPane {...paneProps()} />);
    await screen.findByText(/这个对话还没有历史记录/);
    expect(runtime.listHistoryEntries).toHaveBeenCalledWith("conv_1", undefined);
  });

  it("reads one agent's history, and says so when that agent has done nothing", async () => {
    runtime.listHistoryEntries.mockResolvedValue([]);
    const { rerender } = render(<HistoryPane {...paneProps()} owners={["reviewer"]} />);
    expect(await screen.findByText(/这个子代理还没有历史记录/)).toBeInTheDocument();
    expect(runtime.listHistoryEntries).toHaveBeenCalledWith("conv_1", ["reviewer"]);

    // The host separates a child's traffic from the session's own, so an agent
    // with nothing recorded must not fall back to the conversation's rows.
    expect(runtime.listHistoryEntries).not.toHaveBeenCalledWith("conv_1", undefined);

    // A list rebuilt on every render is the same history; re-reading it once a
    // render would poll the store for nothing.
    const reads = runtime.listHistoryEntries.mock.calls.length;
    rerender(<HistoryPane {...paneProps()} owners={["reviewer"]} />);
    await waitFor(() => expect(runtime.listHistoryEntries.mock.calls.length).toBe(reads));
  });
});
