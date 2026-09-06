import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { WorkflowHistory } from "./WorkflowHistory";
import type { WorkflowHistoryRun } from "../lib/workflowRuns";

const runtime = vi.hoisted(() => ({
  workflowRunHistory: vi.fn(), workflowStepRecord: vi.fn(),
  skipWorkflowStep: vi.fn(), stopConversationTask: vi.fn(), runModel: vi.fn()
}));
vi.mock("../lib/runtime", async (original) => ({ ...await original<typeof import("../lib/runtime")>(), ...runtime }));

const history: WorkflowHistoryRun[] = [{ runId: "run1", scriptName: "Recovered script", status: "interrupted", startedAt: null,
  steps: [
    { index: 0, label: "First", state: "completed", bodyAvailable: true, error: null },
    { index: 1, label: "Cached", state: "cached", bodyAvailable: false, error: null },
    { index: 2, label: "Third", state: "failed", bodyAvailable: true, error: "step failed" }
  ] }];

describe("read-only workflow history", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    runtime.workflowRunHistory.mockResolvedValue(history);
    runtime.workflowStepRecord.mockResolvedValue({ task: "archived", status: "completed", updates: [], contexts: [
      { kind: "assistant", id: "disk-only-body", content: "Body from disk without any context shell", createdAt: "2026-09-05T00:00:00Z" }
    ] });
  });

  it("opens disk-only sparse steps without creating a live task or enabling controls", async () => {
    const user = userEvent.setup();
    render(<WorkflowHistory conversationId="cold-conversation" contexts={[]} representedCallIds={[]} />);
    await user.click(await screen.findByText("Recovered script"));
    await user.click(screen.getByRole("button", { name: /Third/ }));
    expect(await screen.findByText("Body from disk without any context shell")).toBeInTheDocument();
    expect(runtime.workflowStepRecord).toHaveBeenCalledWith("cold-conversation", "run1", 2);
    expect(screen.getByRole("button", { name: /Cached/ })).toBeDisabled();
    expect(screen.queryByRole("button", { name: /停止|跳过|继续运行|Stop|Skip|Resume/ })).not.toBeInTheDocument();
    expect(runtime.skipWorkflowStep).not.toHaveBeenCalled();
    expect(runtime.stopConversationTask).not.toHaveBeenCalled();
    expect(runtime.runModel).not.toHaveBeenCalled();
  });

  it("keeps missing disk history absent and reports read failures", async () => {
    runtime.workflowRunHistory.mockResolvedValueOnce([]);
    const first = render(<WorkflowHistory conversationId="missing" contexts={[]} representedCallIds={[]} />);
    await waitFor(() => expect(runtime.workflowRunHistory).toHaveBeenCalledWith("missing"));
    expect(screen.queryByText("Recovered script")).not.toBeInTheDocument();
    first.unmount();
    runtime.workflowStepRecord.mockRejectedValueOnce(new Error("corrupt archive"));
    const user = userEvent.setup();
    render(<WorkflowHistory conversationId="damaged" contexts={[]} representedCallIds={[]} />);
    await user.click(await screen.findByText("Recovered script"));
    await user.click(screen.getByRole("button", { name: /First/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("corrupt archive");
  });

  it("does not show a late response from another conversation", async () => {
    let release!: (rows: WorkflowHistoryRun[]) => void;
    runtime.workflowRunHistory.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const mounted = render(<WorkflowHistory conversationId="old" contexts={[]} representedCallIds={[]} />);
    await waitFor(() => expect(runtime.workflowRunHistory).toHaveBeenCalledWith("old"));
    runtime.workflowRunHistory.mockResolvedValue([]);
    mounted.rerender(<WorkflowHistory conversationId="new" contexts={[]} representedCallIds={[]} />);
    await act(async () => release(history));
    expect(screen.queryByText("Recovered script")).not.toBeInTheDocument();
  });
});
