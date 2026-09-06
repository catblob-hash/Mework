import { describe, expect, it } from "vitest";
import { createModelRunController } from "./modelRunController";
import type { ModelRunState, StreamingToolState } from "./modelStream";
import type { ModelRunRequest, UserContext } from "../types";

function runFixture(overrides: Partial<ModelRunState> = {}): ModelRunState {
  return {
    requestId: "run_test",
    providerName: "provider",
    modelName: "model",
    workspaceId: "ws",
    request: {
      contexts: [],
      provider: { family: "openai_responses" },
      model: { capabilities: [] }
    } as unknown as ModelRunRequest,
    startedAt: "2026-08-04T00:00:00.000Z",
    streamedTextByRound: {},
    streamedReasoningByRound: {},
    completedReasoningByRound: {},
    reasoningStartedAtByRound: {},
    reasoningDurationByRound: {},
    streamedToolsByRound: {},
    streamedHooksByRound: {},
    steeredInputsByRound: {},
    usageByRound: {},
    subagentUsageByCall: {},
    workflowProgressByCall: {},
    workflowRunIdByCall: {},
    usageRevision: 0,
    ...overrides
  };
}

function toolFixture(overrides: Partial<StreamingToolState> = {}): StreamingToolState {
  return {
    id: "tool-1",
    callId: "call-1",
    toolName: "read_file",
    input: {},
    result: { success: true, output: "", executedAt: "2026-08-04T00:00:01.000Z", durationMs: 1 },
    streamStatus: "running",
    live: { contexts: [], updates: [] },
    createdAt: "2026-08-04T00:00:01.000Z",
    ...overrides
  } as StreamingToolState;
}

function steeredInput(id: string): UserContext {
  return { id, kind: "user", content: id, createdAt: "2026-08-04T00:00:02.000Z" };
}

describe("createModelRunController", () => {
  it("publishes updates synchronously and notifies fine-grained subscribers once", () => {
    const controller = createModelRunController();
    let notified = 0;
    controller.subscribe(() => {
      notified += 1;
    });
    const run = runFixture();
    controller.update((current) => ({ ...current, conv: run }));
    expect(controller.current().conv).toBe(run);
    expect(notified).toBe(1);
  });

  it("does not notify when the updater returns the same reference", () => {
    const controller = createModelRunController();
    let notified = 0;
    controller.subscribe(() => {
      notified += 1;
    });
    controller.update((current) => current);
    expect(notified).toBe(0);
  });

  it("keeps summary identity across streamed-delta-only commits", () => {
    const controller = createModelRunController();
    let summaryNotifications = 0;
    controller.subscribeSummaries(() => {
      summaryNotifications += 1;
    });
    controller.update((current) => ({ ...current, conv: runFixture() }));
    expect(summaryNotifications).toBe(1);
    const before = controller.summaries();
    const beforeEntry = before.conv;
    controller.update((current) => ({
      ...current,
      conv: { ...runFixture(), streamedTextByRound: { 0: "正在输出的文本" } }
    }));
    expect(controller.summaries()).toBe(before);
    expect(controller.summaries().conv).toBe(beforeEntry);
    expect(summaryNotifications).toBe(1);
  });

  it("notifies summaries when a run appears and when it is removed", () => {
    const controller = createModelRunController();
    let summaryNotifications = 0;
    controller.subscribeSummaries(() => {
      summaryNotifications += 1;
    });
    controller.update((current) => ({ ...current, conv: runFixture() }));
    controller.update(() => ({}));
    expect(summaryNotifications).toBe(2);
    expect(controller.summaries().conv).toBeUndefined();
  });

  it("updates the summary when a steered message is delivered", () => {
    const controller = createModelRunController();
    controller.update((current) => ({ ...current, conv: runFixture() }));
    const before = controller.summaries().conv;
    controller.update((current) => ({
      ...current,
      conv: { ...runFixture(), steeredInputsByRound: { 1: [steeredInput("queued-1")] } }
    }));
    const after = controller.summaries().conv;
    expect(after).not.toBe(before);
    expect(after?.steeredMessageIds).toEqual(["queued-1"]);
  });

  it("surfaces browser-automation transitions in the summary", () => {
    const controller = createModelRunController();
    controller.update((current) => ({ ...current, conv: runFixture() }));
    expect(controller.summaries().conv?.browserAutomationTool).toBeNull();
    controller.update((current) => ({
      ...current,
      conv: {
        ...runFixture(),
        streamedToolsByRound: {
          0: [
            toolFixture({ id: "tool-b", toolName: "playwright", input: { action: "click" }, streamStatus: "running" })
          ]
        }
      }
    }));
    expect(controller.summaries().conv?.browserAutomationTool).toBe("click");
  });

  it("preserves untouched conversations' summary objects when another run changes", () => {
    const controller = createModelRunController();
    controller.update((current) => ({
      ...current,
      quiet: runFixture({ requestId: "run_quiet" }),
      busy: runFixture({ requestId: "run_busy" })
    }));
    const quietBefore = controller.summaries().quiet;
    controller.update((current) => ({
      ...current,
      busy: {
        ...runFixture({ requestId: "run_busy" }),
        steeredInputsByRound: { 0: [steeredInput("queued-2")] }
      }
    }));
    expect(controller.summaries().quiet).toBe(quietBefore);
    expect(controller.summaries().busy?.steeredMessageIds).toEqual(["queued-2"]);
  });

  it("tracks run tokens and pipeline guards without notifying subscribers", () => {
    const controller = createModelRunController();
    let notified = 0;
    controller.subscribe(() => {
      notified += 1;
    });
    controller.subscribeSummaries(() => {
      notified += 1;
    });
    controller.setRunToken("conv", "run_1");
    expect(controller.runToken("conv")).toBe("run_1");
    expect(controller.hasRunToken("conv")).toBe(true);
    controller.deleteRunToken("conv");
    expect(controller.hasRunToken("conv")).toBe(false);
    controller.addPreparingRun("conv");
    expect(controller.hasPreparingRun("conv")).toBe(true);
    controller.deletePreparingRun("conv");
    expect(controller.hasPreparingRun("conv")).toBe(false);
    controller.addPerformingRun("conv");
    expect(controller.hasPerformingRun("conv")).toBe(true);
    controller.deletePerformingRun("conv");
    expect(controller.hasPerformingRun("conv")).toBe(false);
    expect(notified).toBe(0);
  });

  it("waits for the exact run generation to leave both live stores", async () => {
    const controller = createModelRunController();
    controller.setRunToken("conv", "run_waiting");
    controller.update((current) => ({
      ...current,
      conv: runFixture({ requestId: "run_waiting" })
    }));

    let exited = false;
    const observedExit = controller.waitForRunExit("conv", "run_waiting").then(() => {
      exited = true;
    });
    controller.update((current) => ({
      ...current,
      unrelated: runFixture({ requestId: "run_unrelated" })
    }));
    await Promise.resolve();
    expect(exited).toBe(false);

    controller.deleteRunToken("conv");
    await Promise.resolve();
    expect(exited).toBe(false);
    controller.update((current) => {
      const next = { ...current };
      delete next.conv;
      return next;
    });
    await observedExit;
    expect(exited).toBe(true);
  });
});
