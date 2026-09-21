import type {
  AttachedWorkspace,
  ContextItem,
  Conversation,
  ConversationSettings,
  RunTarget
} from "../types";

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
  /**
   * Workspaces the draft may work in besides its primary one, persisted directly
   * in `Conversation.attachedWorkspaces` when materialized.
   *
   * Unlike `worktreeRequested` this is not just intent: the host authorized each
   * one when its picker returned it, so the list is already the real grant.
   */
  attachedWorkspaces: AttachedWorkspace[];
  /** Preset the draft's settings came from; empty means an unnamed draft. */
  presetId: string;
  /** Conversation template whose message queue this draft is showing; empty
   * means none. Same trace semantics as `presetId`. */
  templateId: string;
  /**
   * Content the user wrote by hand before sending anything, such as a
   * right-click inserted message. It travels into the conversation at
   * materialization, ahead of the first sent message.
   */
  contexts: ContextItem[];
}

export function isDraftConversationId(conversationId: string | null | undefined): boolean {
  return conversationId === DRAFT_CONVERSATION_ID;
}

/**
 * Whether a persisted conversation is still an unsent draft. Each workspace keeps at most one:
 * opening a new task there returns to it rather than stacking another, and the sidebar withholds
 * it until it holds something.
 *
 * Nesting counts as content because hiding a parent would orphan its children in the tree.
 */
export function isUnsentConversation(conversation: Conversation, hasChildren = false): boolean {
  return !hasChildren
    && conversation.contexts.length === 0
    && conversation.queuedMessages.length === 0;
}

/** The workspace's unsent draft slot, if it currently holds one. */
export function draftSlotOf(conversations: Conversation[]): Conversation | null {
  const parentIds = new Set(
    conversations.map((conversation) => conversation.parentConversationId).filter(Boolean)
  );
  return conversations.find(
    (conversation) => isUnsentConversation(conversation, parentIds.has(conversation.id))
  ) ?? null;
}

/** The workspace's conversations minus its unsent draft slot. */
export function visibleConversations(conversations: Conversation[]): Conversation[] {
  const parentIds = new Set(
    conversations.map((conversation) => conversation.parentConversationId).filter(Boolean)
  );
  return conversations.filter(
    (conversation) => !isUnsentConversation(conversation, parentIds.has(conversation.id))
  );
}

/**
 * Project a draft as a `Conversation` so the normal timeline, composer,
 * workspace, model, reasoning, and security controls render unchanged. A draft
 * can already hold hand-written content; it materializes on the first request,
 * not on the first message.
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
    contexts: draft.contexts,
    queuedMessages: [],
    branches: [],
    userAbortedTasks: [],
    worktree: null,
    runTarget: draft.runTarget,
    attachedWorkspaces: draft.attachedWorkspaces,
    parentConversationId: null,
    presetId: draft.presetId,
    templateId: draft.templateId
  };
}
