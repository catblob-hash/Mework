import { describe, expect, it } from "vitest";
import type { ContextItem, Conversation, JsonObject, ToolContext } from "../types";
import {
  deleteStateToolContext,
  restoreStateToolContexts
} from "./stateToolDeletion";

const NOW = "2026-07-24T12:00:00Z";

function tool(
  id: string,
  toolName: string,
  input: JsonObject,
  output: unknown,
  success = true
): ToolContext {
  return {
    id,
    kind: "tool",
    toolName,
    input,
    result: {
      success,
      output: typeof output === "string" ? output : JSON.stringify(output),
      executedAt: NOW,
      durationMs: 1
    },
    createdAt: NOW
  };
}

function conversation(contexts: ContextItem[]): Conversation {
  return {
    id: "conversation-1",
    title: "State tools",
    createdAt: NOW,
    updatedAt: NOW,
    settings: {} as Conversation["settings"],
    contexts,
    queuedMessages: [],
    branches: [],
    userAbortedTasks: [],
    worktree: null,
  runTarget: null,
    parentConversationId: null
  };
}

function ids(contexts: ContextItem[]): string[] {
  return contexts.map((context) => context.id);
}

describe("state tool cascade deletion", () => {
  it("scopes equal task ids to the create context visible in each branch lane", () => {
    const fork = {
      id: "user-fork",
      kind: "user" as const,
      content: "fork",
      createdAt: NOW
    };
    const activeRoot = tool(
      "active-task-create",
      "todo",
      { action: "create", subject: "Active", description: "Active task" },
      { task: { id: "task-1", subject: "Active" } }
    );
    const activeUpdate = tool(
      "active-task-update",
      "todo",
      { action: "update", taskId: "task-1", status: "completed" },
      { success: true, taskId: "task-1", updatedFields: ["status"] }
    );
    const inactiveRoot = tool(
      "inactive-task-create",
      "todo",
      { action: "create", subject: "Inactive", description: "Inactive task" },
      { task: { id: "task-1", subject: "Inactive" } }
    );
    const inactiveSecond = tool(
      "inactive-task-create-2",
      "todo",
      { action: "create", subject: "Inactive second", description: "Inactive second task" },
      { task: { id: "task-2", subject: "Inactive second" } }
    );
    const inactiveUpdate = tool(
      "inactive-task-update",
      "todo",
      { action: "update", taskId: "task-1", status: "in_progress" },
      { success: true, taskId: "task-1", updatedFields: ["status"] }
    );
    const current = conversation([fork, activeRoot, activeUpdate]);
    current.branches = [
      {
        id: "inactive",
        forkContextId: fork.id,
        active: false,
        contexts: [inactiveRoot, inactiveSecond, inactiveUpdate],
        createdAt: NOW,
        updatedAt: NOW
      },
      {
        id: "active",
        forkContextId: fork.id,
        active: true,
        contexts: [],
        createdAt: NOW,
        updatedAt: NOW
      }
    ];

    const deletion = deleteStateToolContext(current, inactiveRoot, NOW)!;

    expect(deletion.scope).toBe("task-list");
    expect(ids(deletion.conversation.contexts)).toEqual([
      "user-fork",
      "active-task-create",
      "active-task-update"
    ]);
    expect(deletion.conversation.branches[0].contexts).toEqual([]);
    expect(deletion.removed.map((entry) => entry.context.id).sort()).toEqual([
      "inactive-task-create",
      "inactive-task-create-2",
      "inactive-task-update"
    ]);
  });

  it("deletes a later todo create action with only that task's related events", () => {
    const root = tool(
      "task-create-1",
      "todo",
      { action: "create", subject: "First", description: "First task" },
      { task: { id: "task-1", subject: "First" } }
    );
    const second = tool(
      "task-create-2",
      "todo",
      { action: "create", subject: "Second", description: "Second task" },
      { task: { id: "task-2", subject: "Second" } }
    );
    const firstUpdate = tool(
      "task-update-1",
      "todo",
      { action: "update", taskId: "task-1", status: "completed" },
      { success: true, taskId: "task-1", updatedFields: ["status"] }
    );
    const secondUpdate = tool(
      "task-update-2",
      "todo",
      { action: "update", taskId: "task-2", status: "completed" },
      { success: true, taskId: "task-2", updatedFields: ["status"] }
    );
    const secondGet = tool("task-get-2", "todo", { action: "get", taskId: "task-2" }, { task: null });
    const list = tool("task-list", "todo", { action: "list" }, { tasks: [] });
    const current = conversation([root, firstUpdate, second, secondUpdate, secondGet, list]);

    const deletion = deleteStateToolContext(current, second, NOW)!;

    expect(deletion.scope).toBe("task");
    expect(ids(deletion.conversation.contexts)).toEqual([
      "task-create-1",
      "task-update-1",
      "task-list"
    ]);
  });

});
