import { describe, expect, it } from "vitest";
import {
  anyTruncated,
  describePart,
  ledgerRounds,
  rawPayload,
  readTurn,
  sumWireUsage,
  wireUsageIsEmpty
} from "./wireLedger";
import type { TurnRequest } from "./wireLedger";
import type { WireRequestPart, WireRequestSummary } from "./runtime";

function part(
  ordinal: number,
  kind: WireRequestPart["kind"],
  body: string,
  hash = `h${ordinal}-${body.length}`
): WireRequestPart {
  return { ordinal, kind, hash, body, bytes: new TextEncoder().encode(body).length, truncated: false };
}

function message(ordinal: number, value: unknown, hash?: string): WireRequestPart {
  return part(ordinal, "message", JSON.stringify(value), hash);
}

function summary(
  seq: number,
  requestId: string,
  overrides: Partial<WireRequestSummary> = {}
): WireRequestSummary {
  return {
    seq,
    createdAt: "2026-09-15T10:00:00Z",
    kind: "model",
    requestId,
    round: 1,
    attempt: 1,
    providerName: "Anthropic",
    family: "anthropic",
    modelId: "claude-opus-5",
    partCount: 3,
    bytes: 100,
    ...overrides
  };
}

describe("describePart", () => {
  it("reads a plain user message as its text", () => {
    const described = describePart(message(4, { role: "user", content: "看看这个文件" }));
    expect(described.role).toBe("user");
    expect(described.text).toBe("看看这个文件");
    expect(described.preview).toBe("看看这个文件");
  });

  it("names the tool an assistant turn called and keeps its arguments", () => {
    const described = describePart(
      message(5, {
        role: "assistant",
        content: [
          { type: "text", text: "先读文件" },
          {
            type: "tool-call",
            toolName: "read_file",
            toolCallId: "toolu_abc",
            input: { path: "a.ts" }
          }
        ]
      })
    );
    expect(described.role).toBe("assistant");
    expect(described.detail).toBe("read_file");
    expect(described.text).toContain("先读文件");
    expect(described.text).toContain("→ read_file (toolu_abc)");
    expect(described.text).toContain("\"path\": \"a.ts\"");
  });

  it("reads a tool result as its output rather than its envelope", () => {
    const described = describePart(
      message(6, {
        role: "tool",
        content: [
          {
            type: "tool-result",
            toolName: "read_file",
            toolCallId: "toolu_abc",
            output: { type: "text", value: "file body" }
          }
        ]
      })
    );
    expect(described.role).toBe("tool");
    expect(described.detail).toBe("read_file");
    expect(described.text).toContain("← read_file (toolu_abc)");
    expect(described.text).toContain("file body");
  });

  it("marks a recorded image as a reference rather than the bytes that were sent", () => {
    const described = describePart(
      message(7, {
        role: "user",
        content: [
          {
            type: "image",
            mediaType: "image/png",
            image: { $meworkImageAttachment: { image: { name: "shot.png" } } }
          }
        ]
      })
    );
    expect(described.text).toContain("bytes not recorded");
    expect(described.text).toContain("shot.png");
  });

  it("marks reasoning that carried a provider signature", () => {
    const described = describePart(
      message(8, {
        role: "assistant",
        content: [
          {
            type: "reasoning",
            text: "先想一步",
            providerOptions: { anthropic: { signature: "sig" } }
          }
        ]
      })
    );
    expect(described.text).toContain("[reasoning · signed]");
  });

  it("counts the tool surface and keeps the system prompt verbatim", () => {
    const tools = describePart(
      part(2, "tools", JSON.stringify([{ name: "read_file" }, { name: "shell" }]))
    );
    expect(tools.role).toBe("tools");
    expect(tools.detail).toBe("2");
    expect(tools.preview).toBe("read_file, shell");

    const system = describePart(part(0, "system", "You are Mework.\nSecond line."));
    expect(system.role).toBe("system");
    expect(system.text).toBe("You are Mework.\nSecond line.");
    expect(system.preview).toBe("You are Mework.");
  });

  it("keeps an unparseable body rather than dropping the row", () => {
    const described = describePart(part(9, "message", "{ not json"));
    expect(described.role).toBe("message");
    expect(described.text).toBe("{ not json");
  });
});

describe("ledgerRounds", () => {
  it("keeps the rounds of one run inside the turn that issued them", () => {
    const rounds = ledgerRounds([
      summary(1, "run_a", { messagesAdded: 1 }),
      summary(2, "run_a", { messagesAdded: 0 }),
      summary(3, "run_b", { messagesAdded: 1 })
    ]);
    expect(rounds.map((round) => round.turns.map((turn) => turn.summary.seq))).toEqual([
      [1, 2],
      [3]
    ]);
    expect(rounds.map((round) => round.index)).toEqual([1, 2]);
  });

  it("opens a turn only for a run that brought a message the user wrote", () => {
    // A bare Send, a task wake and an `ask_user` answer all start a new run
    // while continuing the same round; none of them adds typed text.
    const rounds = ledgerRounds([
      summary(1, "run_a", { messagesAdded: 1 }),
      summary(2, "run_b", { messagesAdded: 0 }),
      summary(3, "run_c", { messagesAdded: 1 })
    ]);
    expect(rounds.map((round) => round.turns.length)).toEqual([2, 1]);
  });

  it("keeps a host-minted native request in the turn it spent tokens in", () => {
    const rounds = ledgerRounds([
      summary(1, "run_a", { messagesAdded: 1 }),
      summary(2, "", { kind: "search", messagesAdded: 1 })
    ]);
    expect(rounds).toHaveLength(1);
    expect(rounds[0].turns).toHaveLength(2);
  });

  it("falls back to the run boundary for rows recorded before the counts existed", () => {
    const rounds = ledgerRounds([summary(1, "run_a"), summary(2, "run_b")]);
    expect(rounds).toHaveLength(2);
  });

  it("adds up what a turn cost and leaves a counter nobody reported absent", () => {
    const rounds = ledgerRounds([
      summary(1, "run_a", {
        messagesAdded: 1,
        messagesRemoved: 0,
        usage: { inputTokens: 1_000, cachedInputTokens: 800 }
      }),
      summary(2, "run_a", {
        messagesAdded: 0,
        messagesRemoved: 0,
        usage: { inputTokens: 1_400, outputTokens: 60 }
      })
    ]);
    expect(rounds[0].usage).toEqual({
      inputTokens: 2_400,
      cachedInputTokens: 800,
      outputTokens: 60
    });
    expect(rounds[0].messagesAdded).toBe(1);
  });

  it("says a turn recorded no counts rather than saying it changed nothing", () => {
    const rounds = ledgerRounds([summary(1, "run_a")]);
    expect(rounds[0].messagesAdded).toBeNull();
    expect(rounds[0].messagesRemoved).toBeNull();
  });

  it("names the turn after its model, and says so when they differ", () => {
    const rounds = ledgerRounds([
      summary(1, "run_a", { messagesAdded: 1 }),
      summary(2, "run_a", { modelId: "claude-sonnet-5", messagesAdded: 0 })
    ]);
    expect(rounds[0].modelId).toBe("claude-opus-5");
    expect(rounds[0].mixedModels).toBe(true);
  });

  it("returns nothing for an empty ledger", () => {
    expect(ledgerRounds([])).toEqual([]);
  });
});

describe("readTurn", () => {
  const first = message(1, { role: "user", content: "问题" }, "a");
  const second = message(2, { role: "assistant", content: "回答" }, "b");

  function request(
    seq: number,
    parts: WireRequestPart[],
    overrides: Partial<WireRequestSummary> = {}
  ): TurnRequest {
    return { summary: summary(seq, "run_a", overrides), parts };
  }

  it("folds the history a turn replayed and leaves what it added in front", () => {
    const reading = readTurn([first], [request(2, [first, second])]);
    expect(reading.cached.map((entry) => entry.part.role)).toEqual(["user"]);
    expect(reading.groups[0].entries.map((entry) => entry.status)).toEqual(["added"]);
    expect(reading.groups[0].entries[0].part.role).toBe("assistant");
    expect(reading.comparable).toBe(true);
  });

  it("puts each change under the request that was the first to carry it", () => {
    const result = message(
      3,
      {
        role: "user",
        content: [
          {
            type: "tool-result",
            toolName: "read_file",
            toolCallId: "t1",
            output: { type: "text", value: "x" }
          }
        ]
      },
      "r"
    );
    const reading = readTurn(
      [],
      [request(1, [first]), request(2, [first, second, result])]
    );
    expect(reading.groups.map((group) => group.summary.seq)).toEqual([1, 2]);
    expect(reading.groups.map((group) => group.entries.map((entry) => entry.part.role))).toEqual([
      ["user"],
      ["assistant", "user"]
    ]);
  });

  it("leaves a retry's group empty, because it carried nothing new", () => {
    const reading = readTurn(
      [],
      [request(1, [first]), request(2, [first], { attempt: 2 })]
    );
    expect(reading.groups[1].entries).toEqual([]);
    expect(reading.groups[1].summary.attempt).toBe(2);
  });

  it("lays a prompt that moved mid-turn under the request that moved it", () => {
    const base = part(0, "system", "base");
    const planned = part(0, "system", "base\nplan mode");
    const reading = readTurn(
      [base, first],
      [request(1, [base, first]), request(2, [planned, first])]
    );
    expect(reading.prompts.map((entry) => entry.status)).toEqual(["kept"]);
    expect(reading.groups[0].entries).toEqual([]);
    expect(reading.groups[1].entries.map((entry) => entry.status)).toEqual(["changed"]);
    expect(reading.groups[1].entries[0].part.role).toBe("system");
  });

  it("reports a rewritten message as one change, not a delete and an invention", () => {
    const edited = message(1, { role: "user", content: "问题（改过）" }, "a2");
    const reading = readTurn([first, second], [request(2, [edited, second])]);
    expect(reading.cached.map((entry) => entry.part.role)).toEqual(["assistant"]);
    expect(reading.groups[0].entries.map((entry) => entry.status)).toEqual(["changed"]);
    expect(reading.groups[0].entries[0].patch).toContain("+问题（改过）");
    expect(reading.groups[0].entries[0].patch).toContain("-问题");
  });

  it("keeps a removed message where it stood, in the turn that dropped it", () => {
    const third = message(3, { role: "user", content: "再来" }, "c");
    const reading = readTurn([first, second, third], [request(2, [first, third])]);
    expect(reading.cached.map((entry) => entry.part.role)).toEqual(["user", "user"]);
    expect(reading.groups[0].entries.map((entry) => entry.status)).toEqual(["removed"]);
    expect(reading.groups[0].entries[0].part.role).toBe("assistant");
  });

  it("tells a message the user wrote from the turn's own output", () => {
    const toolResult = message(
      2,
      {
        role: "user",
        content: [
          { type: "tool-result", toolName: "read_file", toolCallId: "t1", output: { type: "text", value: "x" } }
        ]
      },
      "tool"
    );
    const typed = message(3, { role: "user", content: "接着改" }, "typed");
    const reading = readTurn([first], [request(2, [first, toolResult, typed])]);
    expect(reading.groups[0].entries.map((entry) => entry.part.author)).toEqual(["model", "user"]);
  });

  it("attributes nothing when there is no earlier request to compare against", () => {
    const reading = readTurn(null, [request(7, [first, second])]);
    expect(reading.groups[0].entries).toEqual([]);
    expect(reading.cached).toHaveLength(2);
    expect(reading.cached.every((entry) => entry.status === "kept")).toBe(true);
    expect(reading.comparable).toBe(false);
  });

  it("reads a turn that opens the conversation as all of it being new", () => {
    const reading = readTurn([], [request(1, [first])]);
    expect(reading.cached).toEqual([]);
    expect(reading.groups[0].entries.map((entry) => entry.status)).toEqual(["added"]);
    expect(reading.comparable).toBe(true);
  });

  it("notices a prompt that changed while the messages did not", () => {
    const before = [part(0, "system", "base"), first];
    const after = [part(0, "system", "base\nplan mode"), first];
    const reading = readTurn(before, [request(2, after)]);
    expect(reading.prompts.map((entry) => entry.status)).toEqual(["changed"]);
    expect(reading.cached.map((entry) => entry.status)).toEqual(["kept"]);
    expect(reading.groups[0].entries).toEqual([]);
  });

  it("keeps the prompts out of the fold so they are always in view", () => {
    const before = [part(0, "system", "base"), first];
    const after = [part(0, "system", "base"), first, second];
    const reading = readTurn(before, [request(2, after)]);
    expect(reading.prompts.map((entry) => entry.part.role)).toEqual(["system"]);
    expect(reading.cached.map((entry) => entry.part.role)).toEqual(["user"]);
    expect(reading.groups[0].entries.map((entry) => entry.part.role)).toEqual(["assistant"]);
  });

  it("folds all but the tail of a long history that a turn appended to", () => {
    // Every request replays everything before it, so this is the ordinary shape
    // of a request body, not a stress case.
    const history = Array.from({ length: 4000 }, (_, index) =>
      message(index, { role: "user", content: `m${index}` }, `h${index}`)
    );
    const appended = message(4000, { role: "assistant", content: "answer" }, "h-new");
    const reading = readTurn(history, [request(2, [...history, appended])]);
    expect(reading.cached).toHaveLength(4000);
    expect(reading.groups[0].entries).toHaveLength(1);
    expect(reading.groups[0].entries[0].status).toBe("added");
  });

  it("reports a pair too large to align as wholly replaced rather than blocking", () => {
    // Past the table ceiling the alignment is abandoned; the honest answer is
    // that every part changed, which is what the reading then says.
    const before = Array.from({ length: 1200 }, (_, index) =>
      message(index, { role: "user", content: `a${index}` }, `before-${index}`)
    );
    const after = Array.from({ length: 1200 }, (_, index) =>
      message(index, { role: "user", content: `b${index}` }, `after-${index}`)
    );
    const reading = readTurn(before, [request(2, after)]);
    expect(reading.cached).toEqual([]);
    expect(reading.groups[0].entries).toHaveLength(1200);
    expect(reading.groups[0].entries.every((entry) => entry.status === "changed")).toBe(true);
  });

  it("keeps a reading's own line counts on each changed part", () => {
    const edited = message(1, { role: "user", content: "问题（改过）" }, "a2");
    const reading = readTurn([first], [request(2, [edited, second])]);
    expect(reading.groups[0].entries.map((entry) => [entry.additions, entry.deletions])).toEqual([
      [1, 1],
      [1, 0]
    ]);
  });

  it("reads a turn with no request at all as having nothing to attribute", () => {
    expect(readTurn([], [])).toEqual({ prompts: [], cached: [], groups: [], comparable: false });
  });
});

describe("sumWireUsage", () => {
  it("leaves a counter nobody reported absent rather than calling it zero", () => {
    expect(sumWireUsage([{ inputTokens: 5 }, { inputTokens: 7, outputTokens: 2 }])).toEqual({
      inputTokens: 12,
      outputTokens: 2
    });
    expect(wireUsageIsEmpty(sumWireUsage([undefined, {}]))).toBe(true);
  });
});

describe("rawPayload", () => {
  it("reassembles the request from its envelope and parts", () => {
    const payload = rawPayload({
      summary: summary(1, "run_a"),
      envelope: { family: "anthropic", modelId: "claude-opus-5", $notRecorded: ["apiKey"] },
      parts: [
        part(0, "system", "You are Mework."),
        part(1, "systemDynamic", "Plan mode is on."),
        part(2, "tools", JSON.stringify([{ name: "read_file" }])),
        message(3, { role: "user", content: "问题" })
      ]
    }) as Record<string, unknown>;

    expect(payload.family).toBe("anthropic");
    expect(payload.$notRecorded).toEqual(["apiKey"]);
    expect(payload.system).toBe("You are Mework.");
    expect(payload.systemDynamic).toBe("Plan mode is on.");
    expect(payload.tools).toEqual([{ name: "read_file" }]);
    expect(payload.messages).toEqual([{ role: "user", content: "问题" }]);
  });

  it("puts the fields back in the order the host wrote them", () => {
    const payload = rawPayload({
      summary: summary(1, "run_a"),
      envelope: { maxSteps: 1, modelId: "m", family: "anthropic", $notRecorded: ["apiKey"] },
      parts: [part(0, "system", "prompt"), message(1, { role: "user", content: "q" })]
    }) as Record<string, unknown>;
    expect(Object.keys(payload)).toEqual([
      "$notRecorded",
      "family",
      "modelId",
      "system",
      "messages",
      "maxSteps"
    ]);
  });

  it("keeps an envelope field it does not know the place of", () => {
    const payload = rawPayload({
      summary: summary(1, "run_a"),
      envelope: { family: "anthropic", somethingNew: 7 },
      parts: []
    }) as Record<string, unknown>;
    expect(payload.somethingNew).toBe(7);
  });

  it("carries an empty message list rather than omitting the field", () => {
    const payload = rawPayload({
      summary: summary(1, "run_a"),
      envelope: { family: "anthropic" },
      parts: []
    }) as Record<string, unknown>;
    expect(payload.messages).toEqual([]);
  });
});

describe("anyTruncated", () => {
  it("is true when either side carries a body the store cut", () => {
    const whole = message(0, { role: "user", content: "q" }, "a");
    const cut = { ...whole, truncated: true };
    expect(anyTruncated([whole], [whole])).toBe(false);
    expect(anyTruncated([whole], [cut])).toBe(true);
    expect(anyTruncated([cut], [whole])).toBe(true);
  });
});
