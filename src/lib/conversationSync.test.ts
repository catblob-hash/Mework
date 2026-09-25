import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Conversation } from "../types";

const remote = {
  hasConversationCommands: vi.fn(() => true),
  createConversationRemote: vi.fn(async (_workspaceId: string, conversation: Conversation) => conversation),
  deleteConversationRemote: vi.fn(async () => undefined),
  updateConversationRemote: vi.fn(async (
    _workspaceId: string,
    conversation: Conversation,
    _expectedContextIds: string[]
  ) => conversation),
  reorderConversationsRemote: vi.fn(async () => undefined),
  loadConversationRemote: vi.fn(async () => null)
};

vi.mock("./runtime", () => remote);

const { createConversationSync } = await import("./conversationSync");

function conversation(id: string, contextIds: string[], title = "t"): Conversation {
  return {
    id,
    title,
    createdAt: "2026-08-25T00:00:00.000Z",
    updatedAt: "2026-08-25T00:00:00.000Z",
    settings: {
      systemPrompt: "",
      includeAppDataPath: false,
      enabledTools: [],
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      toolDescriptionFileId: null,
      agentDefinitions: [],
      webSearch: { maxSearchesPerCall: 0, provider: "native" },
      reasoningEffort: "disabled",
      securityLevel: "request_approval",
      globalMemoryEnabled: false,
      projectMemoryEnabled: false
    } as unknown as Conversation["settings"],
    contexts: contextIds.map((id) => ({
      kind: "user",
      id,
      content: id,
      images: [],
      createdAt: "2026-08-25T00:00:00.000Z"
    })),
    queuedMessages: [],
    branches: [],
    userAbortedTasks: [],
    worktrees: [],
  runTarget: null,
  attachedWorkspaces: [],
    parentConversationId: null,
  presetId: "",
  templateId: ""
  };
}

describe("createConversationSync", () => {
  beforeEach(() => {
    for (const mock of Object.values(remote)) mock.mockClear();
    remote.hasConversationCommands.mockReturnValue(true);
  });

  it("sends one command for a burst of edits, keeping the first base", async () => {
    const applied: Conversation[] = [];
    const sync = createConversationSync((_workspaceId, next) => applied.push(next));
    const base = conversation("conv_a", ["ctx_1"]);
    const middle = conversation("conv_a", ["ctx_1"], "middle");
    const last = conversation("conv_a", ["ctx_1", "ctx_2"], "last");

    sync.changed("ws", base, middle);
    sync.changed("ws", middle, last);
    await sync.flush();

    expect(remote.updateConversationRemote).toHaveBeenCalledTimes(1);
    const [workspaceId, sent, expectedContextIds] = remote.updateConversationRemote.mock.calls[0];
    expect(workspaceId).toBe("ws");
    expect(sent.title).toBe("last");
    expect(expectedContextIds).toEqual(["ctx_1"]);
    expect(applied.at(-1)?.title).toBe("last");
  });

  it("applies the host's authoritative body over the optimistic one", async () => {
    const applied: Conversation[] = [];
    const sync = createConversationSync((_workspaceId, next) => applied.push(next));
    const authority = conversation("conv_a", ["ctx_1", "ctx_host_tail"], "authoritative");
    remote.updateConversationRemote.mockResolvedValueOnce(authority);

    sync.changed("ws", conversation("conv_a", ["ctx_1"]), conversation("conv_a", ["ctx_1"], "local"));
    await sync.flush();

    expect(applied.at(-1)).toBe(authority);
  });

  it("drops a pending edit when the conversation is deleted", async () => {
    const sync = createConversationSync(() => undefined);
    sync.changed("ws", conversation("conv_a", []), conversation("conv_a", [], "edited"));
    sync.deleted("ws", "conv_a");
    await sync.flush();

    expect(remote.updateConversationRemote).not.toHaveBeenCalled();
    expect(remote.deleteConversationRemote).toHaveBeenCalledWith("ws", "conv_a");
  });

  it("stays inert without a backend so the local document remains the only writer", async () => {
    remote.hasConversationCommands.mockReturnValue(false);
    const sync = createConversationSync(() => undefined);

    sync.changed("ws", conversation("conv_a", []), conversation("conv_a", [], "edited"));
    sync.created("ws", conversation("conv_b", []), ["conv_b"]);
    sync.deleted("ws", "conv_c");
    sync.reordered("ws", ["conv_b"]);
    await sync.flush();

    expect(remote.updateConversationRemote).not.toHaveBeenCalled();
    expect(remote.createConversationRemote).not.toHaveBeenCalled();
    expect(remote.deleteConversationRemote).not.toHaveBeenCalled();
    expect(remote.reorderConversationsRemote).not.toHaveBeenCalled();
    expect(await sync.refresh("conv_a")).toBeNull();
  });

  it("holds workspace commands until the host knows the workspace", async () => {
    const order: string[] = [];
    let release: () => void = () => undefined;
    const barrier = vi.fn((workspaceId: string) => {
      order.push(`barrier:${workspaceId}`);
      return new Promise<void>((resolve) => {
        release = () => {
          order.push("workspace saved");
          resolve();
        };
      });
    });
    remote.createConversationRemote.mockImplementationOnce(async (_workspaceId, next) => {
      order.push("create");
      return next;
    });
    const sync = createConversationSync(() => undefined, barrier);

    sync.created("ws_new", conversation("conv_slot", []), ["conv_slot"]);
    const flushed = sync.flush();
    await new Promise((resolve) => setTimeout(resolve, 0));
    // The project was added in the same event; its save has not landed yet.
    expect(remote.createConversationRemote).not.toHaveBeenCalled();

    release();
    await flushed;
    expect(order).toEqual(["barrier:ws_new", "workspace saved", "create"]);
    expect(remote.reorderConversationsRemote).toHaveBeenCalledWith("ws_new", ["conv_slot"]);
  });

  it("still sends the command when the barrier fails, so the host reports why", async () => {
    const sync = createConversationSync(
      () => undefined,
      async () => { throw new Error("save refused"); }
    );

    sync.reordered("ws_new", ["conv_moved"]);
    await sync.flush();

    expect(remote.reorderConversationsRemote).toHaveBeenCalledWith("ws_new", ["conv_moved"]);
  });

  it("diffs a replaced document by object identity", async () => {
    const sync = createConversationSync(() => undefined);
    const untouched = conversation("conv_keep", ["ctx_1"]);
    const before = {
      workspaces: [{ id: "ws", conversations: [untouched, conversation("conv_edit", ["ctx_1"])] }]
    };
    const after = {
      workspaces: [{
        id: "ws",
        conversations: [
          untouched,
          conversation("conv_edit", ["ctx_1"], "edited"),
          conversation("conv_new", [])
        ]
      }]
    };

    sync.syncDocument(before as never, after as never);
    await sync.flush();

    expect(remote.updateConversationRemote).toHaveBeenCalledTimes(1);
    expect(remote.updateConversationRemote.mock.calls[0][1].id).toBe("conv_edit");
    expect(remote.createConversationRemote).toHaveBeenCalledTimes(1);
    expect(remote.createConversationRemote.mock.calls[0][1].id).toBe("conv_new");
  });
});
