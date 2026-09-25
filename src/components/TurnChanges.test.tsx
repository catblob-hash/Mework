import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationTurn } from "../lib/conversationTurns";
import { setPathOpenHandler } from "../lib/pathLinks";
import type { PathOpenRequest } from "../lib/pathLinks";
import { subagentViewFixture } from "../test/fixtures";
import type { ContextItem, ToolContext } from "../types";
import { ContextStream } from "./ContextStream";
import { SubagentPanel } from "./SubagentPanel";
import {
  subagentChangeSpans,
  summarizeSpanChanges,
  TurnChanges,
  turnChangeSpans
} from "./TurnChanges";

const at = "2026-09-25T00:00:00.000Z";

function diff(path: string, additions: number, deletions: number, created = false): string {
  return [
    `--- ${created ? "/dev/null" : path}`,
    `+++ ${path}`,
    `@@ -1,${deletions} +1,${additions} @@`,
    ...Array.from({ length: deletions }, (_, index) => `-old ${index}`),
    ...Array.from({ length: additions }, (_, index) => `+new ${index}`)
  ].join("\n") + "\n";
}

function change(
  id: string,
  path: string,
  additions: number,
  deletions: number,
  overrides: Partial<ToolContext> = {}
): ToolContext {
  return {
    id,
    kind: "tool",
    toolName: "edit",
    input: { path, find: "a", replace: "b" },
    result: { success: true, output: "ok", diff: diff(path, additions, deletions), executedAt: at, durationMs: 1 },
    createdAt: at,
    ...overrides
  };
}

const user = (id: string, content = "go"): ContextItem => ({ id, kind: "user", content, createdAt: at });
const reply = (id: string, content = "done"): ContextItem => ({ id, kind: "assistant", content, createdAt: at });

function turn(id: string, status: ConversationTurn["status"], contextIds: string[], anchor: string): ConversationTurn {
  return {
    id,
    requestId: `request-${id}`,
    anchorContextId: anchor,
    modelId: "model",
    startedAt: at,
    durationMs: 0,
    status,
    contextIds,
    usage: {},
    usageOffset: {},
    usageBaseline: {},
    usageRevisionAtStart: 0,
    segmentCount: 1
  };
}

describe("summarizeSpanChanges", () => {
  it("sums every successful change per file, in the order the span first touched them", () => {
    const contexts: ContextItem[] = [
      change("a1", "src/a.ts", 3, 1),
      change("b1", "./src/b.ts", 2, 0),
      change("a2", "src/a.ts", 1, 1),
      change("b2", "src/b.ts", 4, 2),
      change("failed", "src/c.ts", 9, 9, {
        result: { success: false, output: "no match", diff: diff("src/c.ts", 9, 9), executedAt: at, durationMs: 1 }
      }),
      { ...change("read", "src/d.ts", 0, 0), toolName: "read", result: { success: true, output: "text", executedAt: at, durationMs: 1 } },
      change("other-workspace", "src/a.ts", 5, 0, { input: { path: "src/a.ts", workspace: 2 } })
    ];
    const byId = new Map(contexts.map((item) => [item.id, item]));
    const summary = summarizeSpanChanges(
      { key: "span", contextIds: contexts.map((item) => item.id) },
      (id) => byId.get(id)
    );
    expect(summary?.files.map((file) => [file.path, file.workspace, file.additions, file.deletions])).toEqual([
      ["src/a.ts", null, 4, 2],
      ["./src/b.ts", null, 6, 2],
      ["src/a.ts", 2, 5, 0]
    ]);
    expect(summary?.additions).toBe(15);
    expect(summary?.deletions).toBe(4);
  });

  it("has nothing to say for a span that changed no file", () => {
    const contexts: ContextItem[] = [user("u"), reply("r")];
    const byId = new Map(contexts.map((item) => [item.id, item]));
    expect(summarizeSpanChanges({ key: "span", contextIds: ["u", "r"] }, (id) => byId.get(id))).toBeNull();
  });
});

describe("turnChangeSpans", () => {
  it("lists only turns that have ended", () => {
    const spans = turnChangeSpans([
      turn("done", "completed", ["a"], ""),
      turn("stopped", "interrupted", ["b"], "a"),
      turn("live", "running", ["c"], "b"),
      turn("asking", "awaiting_user", ["d"], "c")
    ]);
    expect(spans.map((span) => span.key)).toEqual(["turn:done", "turn:stopped"]);
  });
});

describe("subagentChangeSpans", () => {
  const nudge: ContextItem = { id: "ctx_structured-output-nudge_1", kind: "user", content: "return it", createdAt: at };
  const structured: ToolContext = {
    ...change("result", "", 0, 0),
    toolName: "structured_output",
    input: { value: 1 },
    result: { success: true, output: "accepted", executedAt: at, durationMs: 1 }
  };

  it("opens a run at a message that follows a reply, not at one that lands between calls", () => {
    const contexts: ContextItem[] = [
      user("task"),
      change("e1", "a.ts", 1, 0),
      user("mid-run"),
      reply("r1"),
      user("follow-up"),
      change("e2", "b.ts", 1, 0),
      reply("r2")
    ];
    expect(subagentChangeSpans(contexts, false).map((span) => span.contextIds)).toEqual([
      ["task", "e1", "mid-run", "r1"],
      ["follow-up", "e2", "r2"]
    ]);
  });

  it("keeps a structured-output reminder inside the run it interrupts", () => {
    const contexts: ContextItem[] = [user("task"), change("e1", "a.ts", 1, 0), reply("r1"), nudge, structured];
    expect(subagentChangeSpans(contexts, false)).toHaveLength(1);
  });

  it("withholds a live run until it has returned its structured result", () => {
    const contexts: ContextItem[] = [user("task"), change("e1", "a.ts", 1, 0)];
    expect(subagentChangeSpans(contexts, true)).toEqual([]);
    expect(subagentChangeSpans([...contexts, structured], true)).toHaveLength(1);
  });
});

describe("TurnChanges", () => {
  afterEach(() => vi.restoreAllMocks());

  function summaryOf(count: number) {
    const contexts = Array.from({ length: count }, (_, index) => change(`c${index}`, `src/file-${index}.ts`, index + 1, index));
    const byId = new Map(contexts.map((item) => [item.id, item]));
    return summarizeSpanChanges({ key: "turn:t", contextIds: contexts.map((item) => item.id) }, (id) => byId.get(id))!;
  }

  it("folds the list from its heading and keeps a list opened in full across the fold", async () => {
    const actor = userEvent.setup();
    render(<TurnChanges summary={summaryOf(6)} pathBaseDir={null} />);
    const card = screen.getByRole("region", { name: "编辑了 6 个文件" });
    expect(within(card).getByText("+21")).toBeInTheDocument();
    expect(within(card).getByText("−15")).toBeInTheDocument();
    expect(within(card).getAllByRole("button", { name: /file-\d\.ts/ })).toHaveLength(3);

    await actor.click(within(card).getByRole("button", { name: "再显示 3 个" }));
    expect(within(card).getAllByRole("button", { name: /file-\d\.ts/ })).toHaveLength(6);

    const heading = within(card).getByRole("button", { name: /编辑了 6 个文件/ });
    await actor.click(heading);
    expect(heading).toHaveAttribute("aria-expanded", "false");
    expect(within(card).queryByRole("button", { name: /file-\d\.ts/ })).toBeNull();

    await actor.click(heading);
    expect(within(card).getAllByRole("button", { name: /file-\d\.ts/ })).toHaveLength(6);
    expect(within(card).getByRole("button", { name: "收起" })).toBeInTheDocument();
  });

  it("shows a list one row longer than the fold whole", () => {
    render(<TurnChanges summary={summaryOf(4)} pathBaseDir={null} />);
    expect(screen.getAllByRole("button", { name: /file-\d\.ts/ })).toHaveLength(4);
    expect(screen.queryByRole("button", { name: /再显示/ })).toBeNull();
  });

  it("says where a file is only when another row carries the same name", () => {
    const contexts = [
      change("en", "website/content/en/working.md", 1, 0),
      change("zh", "website/content/zh-CN/working.md", 1, 0),
      change("app", "src/App.tsx", 1, 0)
    ];
    const byId = new Map(contexts.map((item) => [item.id, item]));
    const summary = summarizeSpanChanges({ key: "turn:t", contextIds: ["en", "zh", "app"] }, (id) => byId.get(id))!;
    const { container } = render(<TurnChanges summary={summary} pathBaseDir={null} />);
    expect([...container.querySelectorAll(".turn-changes__directory")].map((node) => node.textContent))
      .toEqual(["website/content/en", "website/content/zh-CN"]);
  });

  it("asks the app to open a row's file, on its diff where Git tracks it", async () => {
    const requests: PathOpenRequest[] = [];
    const remove = setPathOpenHandler((request) => {
      requests.push(request);
      return true;
    });
    const contexts = [
      change("own", "src/a.ts", 1, 0),
      change("other", "lib/b.ts", 1, 0, { input: { path: "lib/b.ts", workspace: 2 } })
    ];
    const byId = new Map(contexts.map((item) => [item.id, item]));
    const summary = summarizeSpanChanges({ key: "turn:t", contextIds: ["own", "other"] }, (id) => byId.get(id))!;
    render(<TurnChanges summary={summary} pathBaseDir="/work/repo" />);

    await userEvent.click(screen.getByRole("button", { name: /a\.ts/ }));
    await userEvent.click(screen.getByRole("button", { name: /b\.ts/ }));
    remove();

    expect(requests).toEqual([
      { path: "src/a.ts", baseDir: "/work/repo", line: null, workspace: null, review: true },
      // Written against another workspace, so this surface's directory says nothing about it.
      { path: "lib/b.ts", baseDir: null, line: null, workspace: 2, review: true }
    ]);
  });
});

describe("change lists on the timeline", () => {
  it("closes a finished turn with its changes and leaves a running one alone", () => {
    const contexts: ContextItem[] = [
      user("u1"),
      change("e1", "src/a.ts", 2, 1),
      reply("r1"),
      user("u2"),
      change("e2", "src/b.ts", 1, 0)
    ];
    const { container } = render(
      <ContextStream
        contexts={contexts}
        turns={[
          turn("first", "completed", ["e1", "r1"], "u1"),
          turn("second", "running", ["e2"], "u2")
        ]}
        tools={[]}
        enabledTools={[]}
        streaming
      />
    );
    const cards = container.querySelectorAll(".turn-changes");
    expect(cards).toHaveLength(1);
    // After the reply that ended the turn, not inside its work.
    const replyCard = screen.getByText("done").closest(".context-slot")!;
    expect(replyCard.nextElementSibling).toBe(cards[0]);
    expect(within(cards[0] as HTMLElement).getByRole("button", { name: /a\.ts/ })).toBeInTheDocument();
  });

  it("closes every run of a subagent, including one that ended on its structured result", () => {
    const structured: ToolContext = {
      ...change("result", "", 0, 0),
      toolName: "structured_output",
      input: { value: 1 },
      result: { success: true, output: "accepted", executedAt: at, durationMs: 1 }
    };
    const agent = subagentViewFixture("step", {
      // The step's own status has not caught up with the host ending it.
      status: "running",
      contexts: [user("task"), change("e1", "src/a.ts", 3, 0), structured]
    });
    const { container, rerender } = render(<SubagentPanel agent={agent} />);
    expect(container.querySelectorAll(".turn-changes")).toHaveLength(1);
    expect(screen.getByRole("region", { name: "编辑了 1 个文件" })).toBeInTheDocument();

    rerender(<SubagentPanel agent={{ ...agent, contexts: [user("task"), change("e1", "src/a.ts", 3, 0)] }} />);
    expect(container.querySelectorAll(".turn-changes")).toHaveLength(0);

    rerender(<SubagentPanel agent={{ ...agent, status: "failed", contexts: [user("task"), change("e1", "src/a.ts", 3, 0)] }} />);
    expect(container.querySelectorAll(".turn-changes")).toHaveLength(1);
  });
});
