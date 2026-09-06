import type { Conversation, ConversationSettings, RunTarget } from "../types";

/**
 * Fixed ID for the renderer-owned draft conversation. It is never persisted or
 * sent to the host; a fixed value makes draft detection a string comparison.
 */
export const DRAFT_CONVERSATION_ID = "__draft__";

/** Renderer-owned draft state. See `draftConversation` in App. */
export interface DraftConversationState {
  /** Null until a workspace is selected; sending uses the temporary workspace. */
  workspaceId: string | null;
  settings: ConversationSettings;
  createdAt: string;
  /**
   * Whether the user requested a worktree for this draft.
   *
   * This is intent only: worktrees belong to persisted conversation IDs. Create
   * it after materializing the conversation and before the first message, when
   * all tool calls must already target the isolated checkout.
   */
  worktreeRequested: boolean;
  /** The draft's run location, persisted directly in `Conversation.runTarget` when materialized. */
  runTarget: RunTarget | null;
}

export function isDraftConversationId(conversationId: string | null | undefined): boolean {
  return conversationId === DRAFT_CONVERSATION_ID;
}

/**
 * Project a draft as a `Conversation` so the normal timeline, composer,
 * workspace, model, reasoning, and security controls render unchanged. Drafts
 * always have empty content and must materialize before content exists.
 */
export function draftAsConversation(
  draft: DraftConversationState,
  title: string
): Conversation {
  return {
    id: DRAFT_CONVERSATION_ID,
    title,
    createdAt: draft.createdAt,
    updatedAt: draft.createdAt,
    settings: draft.settings,
    contexts: [],
    queuedMessages: [],
    branches: [],
    userAbortedTasks: [],
    worktree: null,
    runTarget: draft.runTarget,
    parentConversationId: null
  };
}
