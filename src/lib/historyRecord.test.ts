import { describe, expect, it } from "vitest";
import {
  appendedTools,
  barItems,
  describeAppended,
  describeChange,
  describeEvent,
  describePrompts,
  describeTools,
  historyBars,
  requestSummary,
  sumUsage,
  usageIsEmpty
} from "./historyRecord";
import type { EventLabels, HistoryBar } from "./historyRecord";
import type { HistoryEntry, HistoryEntryDetail, HistoryOp, HistoryPart } from "./runtime";

function entry(
  seq: number,
  kind: HistoryEntry["kind"],
  overrides: Partial<HistoryEntry> = {}
): HistoryEntry {
  return {
    seq,
    createdAt: "2026-09-30T10:00:00Z",
    kind,
    requestId: kind === "edit" ? undefined : "run_a",
    detail: {},
    ...overrides
  };
}

function request(seq: number, requestId: string, detail: Record<string, unknown> = {}): HistoryEntry {
  return entry(seq, "request", {
    requestId,
    detail: { type: "model", attempt: 1, modelId: "claude-opus-5", messagesAdded: 1, ...detail }
  });
}

/** A response answering request `answers`, of the same run. */
function response(seq: number, requestId: string, answers: number): HistoryEntry {
  return entry(seq, "response", { requestId, answers });
}

/** What the user sent, as the change that put it on the timeline. */
function sent(seq: number): HistoryEntry {
  return entry(seq, "edit", { detail: { source: "message", inserted: 1 } });
}

/** A change the user made by hand. */
function edited(seq: number, counts: Record<string, number>): HistoryEntry {
  return entry(seq, "edit", { detail: { source: "edit", ...counts } });
}

function settled(seq: number, requestId: string): HistoryEntry {
  return entry(seq, "run", { requestId, detail: { source: "run", inserted: 1 } });
}

const labels: EventLabels = {
  toolsAdded: (count) => `${count} tools added`,
  toolsRegistered: (count) => `${count} tools registered`,
  localOnly: "local only",
  interrupted: "cut off",
  unseen: "(unseen)",
  rewroteInput: "rewrote input",
  addedContext: "added context",
  blocked: "blocked",
  halted: "halted",
  failed: "failed",
  denied: "denied",
  permission: (decision) => `permission:${decision}`,
  truncated: "truncated",
  paused: "paused",
  images: (count) => `${count} images`,
  files: (count) => `${count} files`
};

describe("requestSummary", () => {
  it("reads a request's detail as the summary a bar is drawn from", () => {
    const summary = requestSummary(request(4, "run_a", { type: "search" }));
    expect(summary).toMatchObject({
      seq: 4,
      kind: "search",
      requestId: "run_a",
      modelId: "claude-opus-5",
      messagesAdded: 1
    });
  });

  it("says a count it was never told is unknown rather than zero", () => {
    expect(requestSummary(entry(1, "request", { detail: {} })).messagesAdded).toBeUndefined();
  });
});

describe("sumUsage", () => {
  it("leaves a counter nobody reported absent rather than calling it zero", () => {
    expect(sumUsage([{ inputTokens: 5 }, { inputTokens: 7, outputTokens: 2 }])).toEqual({
      inputTokens: 12,
      outputTokens: 2
    });
    expect(usageIsEmpty(sumUsage([undefined, {}]))).toBe(true);
  });
});

describe("historyBars", () => {
  it("opens a turn with what the user sent and keeps every entry of its run with it", () => {
    const bars = historyBars([
      sent(1),
      entry(2, "hook", { requestId: "run_a", detail: { event: "UserPromptSubmit" } }),
      request(3, "run_a"),
      response(4, "run_a", 3),
      entry(5, "tool", { requestId: "run_a", callId: "c1" }),
      entry(6, "result", { requestId: "run_a", callId: "c1" }),
      request(7, "run_a", { messagesAdded: 0 }),
      response(8, "run_a", 7),
      entry(9, "hook", { requestId: "run_a", detail: { event: "Stop" } }),
      settled(10, "run_a")
    ]);
    expect(bars).toHaveLength(1);
    expect(bars[0].kind).toBe("turn");
    expect(bars[0].requests.map((summary) => summary.seq)).toEqual([3, 7]);
    expect(bars[0].entries.map((event) => event.seq)).toEqual([1, 2, 4, 5, 6, 8, 9, 10]);
  });

  it("keeps a run that brought no message of the user's in the turn before it", () => {
    // A bare Send or a task wake opens a new run while continuing the round.
    const bars = historyBars([
      sent(1),
      request(2, "run_a"),
      response(3, "run_a", 2),
      request(4, "run_b", { messagesAdded: 0 }),
      response(5, "run_b", 4)
    ]);
    expect(bars.map((bar) => bar.requests.length)).toEqual([2]);
  });

  it("stands the user's edits between turns as a bar of their own, and starts a new turn after them", () => {
    const bars = historyBars([
      sent(1),
      request(2, "run_a"),
      response(3, "run_a", 2),
      settled(4, "run_a"),
      edited(5, { removed: 1 }),
      edited(6, { replaced: 1 }),
      // The edited message sent again: a new run, and nothing new the user typed.
      request(7, "run_b", { messagesAdded: 0 }),
      response(8, "run_b", 7),
      settled(9, "run_b"),
      edited(10, { inserted: 1 }),
      sent(11),
      request(12, "run_c"),
      response(13, "run_c", 12)
    ]);
    expect(bars.map((bar) => bar.kind)).toEqual(["turn", "edits", "turn", "edits", "turn"]);
    expect(bars[1].entries.map((event) => event.seq)).toEqual([5, 6]);
    expect(bars[2].requests.map((summary) => summary.seq)).toEqual([7]);
    expect(bars[2].entries.map((event) => event.seq)).toEqual([8, 9]);
    expect(bars.filter((bar) => bar.kind === "turn").map((bar) => bar.index)).toEqual([1, 2, 3]);
  });

  it("calls a turn interrupted when its last request never got an answer back", () => {
    const bars = historyBars([
      sent(1),
      request(2, "run_a"),
      response(3, "run_a", 2),
      sent(4),
      request(5, "run_b"),
      response(6, "run_b", 5),
      entry(7, "tool", { requestId: "run_b", callId: "c1" }),
      request(8, "run_b", { messagesAdded: 0 }),
      settled(9, "run_b"),
      sent(10),
      request(11, "run_c"),
      response(12, "run_c", 11)
    ]);
    expect(bars.map((bar) => bar.kind)).toEqual(["turn", "interrupted", "turn"]);
  });

  it("does not call a turn interrupted from before answers were recorded", () => {
    const bars = historyBars([
      sent(1),
      request(2, "run_a"),
      settled(3, "run_a"),
      sent(4),
      request(5, "run_b"),
      response(6, "run_b", 5)
    ]);
    expect(bars.map((bar) => bar.kind)).toEqual(["turn", "turn"]);
  });

  it("does not call the turn that is still running interrupted", () => {
    const history = [sent(1), request(2, "run_a"), response(3, "run_a", 2), sent(4), request(5, "run_b")];
    expect(historyBars(history, { live: true }).map((bar) => bar.kind)).toEqual(["turn", "turn"]);
    expect(historyBars(history).map((bar) => bar.kind)).toEqual(["turn", "interrupted"]);
  });

  it("gives what nothing has sent yet a bar of its own", () => {
    const bars = historyBars([sent(1), request(2, "run_a"), response(3, "run_a", 2), sent(4)]);
    expect(bars.map((bar) => bar.kind)).toEqual(["turn", "pending"]);
    expect(bars[1].entries.map((event) => event.seq)).toEqual([4]);
  });

  it("keeps a run a hook stopped before it sent anything with the turn it followed", () => {
    const bars = historyBars([
      sent(1),
      entry(2, "hook", { requestId: "run_blocked", detail: { event: "UserPromptSubmit", blocked: true } })
    ]);
    expect(bars).toHaveLength(1);
    expect(bars[0].kind).toBe("pending");
    expect(bars[0].entries.map((event) => event.seq)).toEqual([1, 2]);
  });

  it("falls back to the run boundary for requests recorded without a message of their own", () => {
    const bars = historyBars([
      request(1, "run_a", { messagesAdded: undefined }),
      request(2, "run_b", { messagesAdded: undefined }),
      request(3, "", { type: "search", messagesAdded: undefined })
    ]);
    expect(bars.map((bar) => bar.requests.map((summary) => summary.seq))).toEqual([[1], [2, 3]]);
  });

  it("adds up what a turn cost, names its model, and says when the models differ", () => {
    const [bar] = historyBars([
      sent(1),
      entry(2, "request", {
        requestId: "run_a",
        detail: { type: "model", modelId: "claude-opus-5", messagesAdded: 1 },
        usage: { inputTokens: 1_000, cachedInputTokens: 800 }
      }),
      entry(3, "request", {
        requestId: "run_a",
        detail: { type: "model", modelId: "claude-sonnet-5", messagesAdded: 0 },
        usage: { inputTokens: 1_400, outputTokens: 60 }
      })
    ]);
    expect(bar.usage).toEqual({ inputTokens: 2_400, cachedInputTokens: 800, outputTokens: 60 });
    expect(bar.modelId).toBe("claude-opus-5");
    expect(bar.mixedModels).toBe(true);
  });

  it("returns nothing for an empty history", () => {
    expect(historyBars([])).toEqual([]);
  });
});

describe("barItems", () => {
  it("draws a settled run as its settlement, and one that never settled as its entries", () => {
    const [done, cut] = historyBars([
      sent(1),
      request(2, "run_a"),
      response(3, "run_a", 2),
      entry(4, "tool", { requestId: "run_a", callId: "c1" }),
      entry(5, "result", { requestId: "run_a", callId: "c1" }),
      settled(6, "run_a"),
      sent(7),
      request(8, "run_b"),
      response(9, "run_b", 8),
      entry(10, "tool", { requestId: "run_b", callId: "c2" }),
      request(11, "run_b", { messagesAdded: 0 })
    ]);
    const seqs = (bar: HistoryBar) =>
      barItems(bar).map((item) => [item.type, item.type === "request" ? item.request.seq : item.entry.seq]);
    expect(seqs(done)).toEqual([
      ["change", 1],
      ["change", 6]
    ]);
    expect(cut.kind).toBe("interrupted");
    // Its requests too, in place: a tool it was handed shows nowhere else.
    expect(seqs(cut)).toEqual([
      ["change", 7],
      ["request", 8],
      ["event", 9],
      ["event", 10],
      ["request", 11]
    ]);
  });
});

function op(ordinal: number, change: HistoryOp["op"], value: unknown, before?: unknown): HistoryOp {
  return {
    ordinal,
    op: change,
    contextId: `ctx_${ordinal}`,
    body: value === undefined ? undefined : JSON.stringify(value),
    before: before === undefined ? undefined : JSON.stringify(before)
  };
}

function change(of: HistoryEntry, ops: HistoryOp[]): HistoryEntryDetail {
  return { entry: of, truncated: false, ops };
}

describe("describeChange", () => {
  it("reads what a run settled as the messages it put on the timeline", () => {
    const run = settled(9, "run_a");
    const rows = describeChange(
      run,
      change(run, [
        op(0, "insert", {
          kind: "system",
          content: "Tools added: shell, grep",
          localOnly: true,
          toolsAdded: ["shell", "grep"]
        }),
        op(1, "insert", { kind: "reasoning", content: "先看看目录" }),
        // The shell of a round that only called tools says nothing.
        op(2, "insert", { kind: "assistant", content: "" }),
        op(3, "insert", {
          kind: "tool",
          toolName: "shell",
          input: { command: "ls -a" },
          requestedInput: { command: "ls" },
          result: { success: false, output: "denied" }
        }),
        op(4, "insert", { kind: "assistant", content: "只写到一半", interrupted: true }),
        op(5, "insert", {
          kind: "system",
          content: "branch: main",
          localOnly: true,
          hookExecution: { hookName: "context" }
        })
      ]),
      labels
    );
    // Each is tagged with its role, whatever it is; what else it is follows.
    expect(rows.map((row) => [row.kind, row.label, row.detail, row.preview])).toEqual([
      ["toolsAdded", "system", "2 tools added", "shell, grep"],
      ["reasoning", "reasoning", "", "先看看目录"],
      ["tool", "tool", "shell", "{\"command\":\"ls -a\"}"],
      ["assistant", "assistant", "", "只写到一半"],
      ["system", "system", "context", "branch: main"]
    ]);
    expect(rows[2].badges.map((badge) => badge.label)).toEqual(["rewrote input", "failed"]);
    // The result follows the arguments untitled; the rewrite comes last.
    expect(rows[2].text).toBe(
      '{\n  "command": "ls -a"\n}\n\ndenied\n\n[requestedInput]\n{\n  "command": "ls"\n}'
    );
    expect(rows[3].badges).toEqual([{ label: "cut off", tone: "warning" }]);
    expect(rows[4]).toMatchObject({ detail: "context", badges: [{ label: "local only", tone: "neutral" }] });
    expect(rows.every((row) => row.change === "insert")).toBe(true);
  });

  it("draws a removed message as the message that went", () => {
    const edit = edited(5, { removed: 1 });
    const [row] = describeChange(
      edit,
      change(edit, [op(0, "remove", undefined, { kind: "assistant", content: "原来的回复" })]),
      labels
    );
    // A message taken away whole is said by its colour; it has no lines to count.
    expect(row).toMatchObject({
      change: "remove",
      kind: "assistant",
      label: "assistant",
      preview: "原来的回复",
      additions: 0,
      deletions: 0
    });
  });

  it("says so when a removed row was never seen by the record", () => {
    const edit = edited(5, { removed: 1 });
    const [row] = describeChange(edit, change(edit, [op(0, "remove", undefined)]), labels);
    // Nor does it guess a role for it.
    expect(row).toMatchObject({ change: "remove", label: "", preview: "(unseen)" });
  });

  it("opens a rewritten message into its diff", () => {
    const edit = edited(6, { replaced: 1 });
    const [row] = describeChange(
      edit,
      change(edit, [
        op(0, "replace", { kind: "user", content: "看看这两个文件" }, { kind: "user", content: "看看这个文件" })
      ]),
      labels
    );
    expect(row).toMatchObject({
      change: "replace",
      kind: "user",
      label: "user",
      preview: "看看这两个文件",
      additions: 1,
      deletions: 1
    });
    expect(row.patch).toContain("--- a/user#ctx_0");
    expect(row.patch).toContain("-看看这个文件");
    expect(row.patch).toContain("+看看这两个文件");
  });

  it("reads what the user sent with the attachments it carried", () => {
    const edit = sent(1);
    const [row] = describeChange(
      edit,
      change(edit, [op(0, "insert", { kind: "user", content: "看图\n第二行", images: [{}], files: [{}, {}] })]),
      labels
    );
    expect(row).toMatchObject({ kind: "user", label: "user", preview: "看图" });
    expect(row.badges.map((badge) => badge.label)).toEqual(["1 images", "2 files"]);
  });
});

function prompt(kind: HistoryPart["kind"], body: string, hash = body): HistoryPart {
  return { ordinal: 0, kind, hash, body, bytes: body.length, truncated: false };
}

describe("describePrompts", () => {
  it("draws the system prompt a conversation opened with as new, both parts of it tagged system", () => {
    const rows = describePrompts(null, [prompt("system", "You are Mework."), prompt("systemDynamic", "Today")], "p1", labels);
    expect(rows.map((row) => [row.change, row.label, row.preview])).toEqual([
      ["insert", "system", "You are Mework."],
      ["insert", "system", "Today"]
    ]);
  });

  it("draws nothing for a prompt the request before already carried", () => {
    const parts = [prompt("system", "You are Mework."), prompt("tools", "[]")];
    expect(describePrompts(parts, parts, "p2", labels)).toEqual([]);
  });

  it("opens a prompt that changed into its diff", () => {
    const [row] = describePrompts(
      [prompt("system", "You are Mework.")],
      [prompt("system", "You are Mework, careful.")],
      "p3",
      labels
    );
    expect(row.change).toBe("replace");
    expect(row.patch).toContain("+You are Mework, careful.");
  });
});

/** A tool list part, as `wire_audit` records it. */
function toolList(...tools: { name: string; description?: string }[]): HistoryPart {
  return prompt(
    "tools",
    JSON.stringify(tools.map((tool) => ({ description: tool.description ?? "", inputSchema: {}, name: tool.name })))
  );
}

/** The marker message a request carries where tools joined (`tool_append::marker_message`). */
function marker(...tools: string[]): HistoryPart {
  return prompt(
    "message",
    JSON.stringify({ role: "system", content: "", providerOptions: { mework: { toolAddition: tools } } })
  );
}

describe("describeTools", () => {
  it("draws the tools a conversation opened with as one row tagged as the field, naming them", () => {
    const [row] = describeTools(null, [prompt("system", "You are Mework."), toolList({ name: "ls" }, { name: "grep" })], "p1", labels);
    expect(row).toMatchObject({
      change: "insert",
      kind: "tools",
      label: "tools",
      detail: "2 tools registered",
      preview: "ls, grep",
      patch: ""
    });
    expect(row.text).toContain("ls\n{");
    expect(row.text).toContain("grep\n{");
  });

  it("draws nothing for a list the request before already declared", () => {
    const parts = [toolList({ name: "ls" })];
    expect(describeTools(parts, [toolList({ name: "ls" })], "p2", labels)).toEqual([]);
  });

  it("names what a changed list added, took away and rewrote, and opens into its diff", () => {
    const [row] = describeTools(
      [toolList({ name: "ls" }, { name: "plan" }, { name: "grep", description: "old" })],
      [toolList({ name: "ls" }, { name: "grep", description: "new" }, { name: "handoff" })],
      "p3",
      labels
    );
    expect(row).toMatchObject({ change: "replace", detail: "3 tools registered", preview: "+handoff −plan ~grep" });
    expect(row.patch).toContain('+  "description": "new"');
    expect(row.additions).toBeGreaterThan(0);
    expect(row.deletions).toBeGreaterThan(0);
  });

  it("leaves a tool handed over by append out of the declared list", () => {
    // The list still carries it — the sidecar needs its definition — but the
    // declared list did not change: that is what appending is for.
    expect(
      describeTools(
        [toolList({ name: "ls" }), prompt("message", '{"role":"user","content":"hi"}')],
        [toolList({ name: "ls" }, { name: "handoff" }), prompt("message", '{"role":"user","content":"hi"}'), marker("handoff")],
        "p4",
        labels
      )
    ).toEqual([]);
  });
});

describe("describeAppended", () => {
  it("reads the tools a request's markers hand over, in order and once", () => {
    expect(appendedTools([marker("handoff", "note"), prompt("message", '{"role":"user","content":"toolAddition"}'), marker("note", "mcp__x")])).toEqual([
      "handoff",
      "note",
      "mcp__x"
    ]);
  });

  it("draws only what this request was the first to hand over", () => {
    const before = [toolList({ name: "ls" }), marker("handoff")];
    const after = [toolList({ name: "ls" }), marker("handoff"), marker("mcp__x", "mcp__y")];
    expect(describeAppended(before, after, "a9", labels).map((row) => [row.change, row.kind, row.label, row.detail, row.preview])).toEqual([
      ["insert", "toolsAdded", "system", "2 tools added", "mcp__x, mcp__y"]
    ]);
    expect(describeAppended(after, after, "a10", labels)).toEqual([]);
  });
});

describe("describeEvent", () => {
  it("reads a response as its text and the tools it called", () => {
    const body = JSON.stringify({
      role: "assistant",
      content: [
        { type: "text", text: "先看看" },
        { type: "tool-call", toolCallId: "c1", toolName: "shell", input: { command: "ls" } }
      ]
    });
    const described = describeEvent(
      entry(3, "response", { detail: { finishReason: "length" } }),
      { entry: entry(3, "response"), body, truncated: false },
      labels
    );
    expect(described.label).toBe("assistant");
    expect(described.detail).toBe("shell");
    expect(described.preview).toBe("先看看");
    expect(described.badges).toEqual([{ label: "truncated", tone: "warning" }]);
  });

  it("marks reasoning a response carried with a provider signature", () => {
    const body = JSON.stringify({
      role: "assistant",
      content: [
        { type: "reasoning", text: "先想一步", providerOptions: { anthropic: { signature: "sig" } } },
        { type: "text", text: "好" }
      ]
    });
    const described = describeEvent(
      entry(3, "response"),
      { entry: entry(3, "response"), body, truncated: false },
      labels
    );
    expect(described.text).toContain("[reasoning · signed]");
    expect(described.text).toContain("\"signature\": \"sig\"");
  });

  it("keeps a response body it cannot parse rather than dropping it", () => {
    const described = describeEvent(
      entry(3, "response"),
      { entry: entry(3, "response"), body: "{ not json", truncated: false },
      labels
    );
    expect(described.text).toBe("{ not json");
  });

  it("says what a hook decided, and opens into its rewrite", () => {
    const hook = entry(4, "hook", {
      detail: {
        event: "PreToolUse",
        hookName: "guard",
        matcher: "shell",
        blocked: false,
        halted: false,
        permission: "allow",
        rewroteInput: true,
        addedContext: false,
        success: true
      }
    });
    const described = describeEvent(
      hook,
      {
        entry: hook,
        body: JSON.stringify({ output: "", updatedInput: { command: "ls -a" } }),
        truncated: false
      },
      labels
    );
    expect(described.label).toBe("hook");
    expect(described.detail).toBe("PreToolUse · guard · shell");
    expect(described.badges.map((badge) => badge.label)).toEqual([
      "permission:allow",
      "rewrote input"
    ]);
    expect(described.text).toContain("\"command\": \"ls -a\"");
  });

  it("opens a call a hook rewrote into the diff between what was sent and what ran", () => {
    const tool = entry(5, "tool", { callId: "c1", detail: { name: "shell", rewritten: true } });
    const described = describeEvent(
      tool,
      {
        entry: tool,
        body: JSON.stringify({ input: { command: "ls -a" }, requestedInput: { command: "ls" } }),
        truncated: false
      },
      labels
    );
    expect([described.label, described.detail]).toEqual(["tool", "shell"]);
    expect(described.badges.map((badge) => badge.label)).toEqual(["rewrote input"]);
    expect(described.patch).toContain("-  \"command\": \"ls\"");
    expect(described.patch).toContain("+  \"command\": \"ls -a\"");
  });

  it("marks a failed result and keeps its output", () => {
    const result = entry(6, "result", { callId: "c1", detail: { name: "shell", success: false } });
    const described = describeEvent(
      result,
      { entry: result, body: JSON.stringify({ output: "boom\nstack" }), truncated: false },
      labels
    );
    expect([described.label, described.detail]).toEqual(["tool", "shell"]);
    expect(described.preview).toBe("boom");
    expect(described.badges).toEqual([{ label: "failed", tone: "warning" }]);
  });

  it("still says what an entry is before its body has been read", () => {
    const described = describeEvent(
      entry(8, "tool", { detail: { name: "shell", denied: "no" } }),
      undefined,
      labels
    );
    expect([described.label, described.detail]).toEqual(["tool", "shell"]);
    expect(described.badges).toEqual([{ label: "denied", tone: "warning" }]);
  });
});
