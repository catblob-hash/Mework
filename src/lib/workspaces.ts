import type { Workspace } from "../types";

export const TEMPORARY_WORKSPACE_ID = "__temporary__";

/**
 * Identifies the entry point that creates a conversation. The sidebar's New Task,
 * Ctrl+N, and empty-state button use `global` and always apply the global default
 * preset. A workspace title-row `+` uses `workspace`, first checking its default
 * preset, then its remembered conversation settings.
 */
export type NewConversationSource = "global" | "workspace";

export function isTemporaryWorkspace(workspace: Workspace | null | undefined): boolean {
  return workspace?.kind === "temporary" || workspace?.id === TEMPORARY_WORKSPACE_ID;
}

export function isReservedWorkspace(workspace: Workspace | null | undefined): boolean {
  return isTemporaryWorkspace(workspace);
}

export function createTemporaryWorkspace(conversations: Workspace["conversations"] = []): Workspace {
  return {
    id: TEMPORARY_WORKSPACE_ID,
    name: "临时工作区",
    kind: "temporary",
    path: "",
    createdAt: new Date().toISOString(),
    defaultConversationPresetId: "",
    lastConversationSettings: null,
    conversations
  };
}
