import type {
  AttachedWorkspace,
  ContextItem,
  Conversation,
  ConversationSettings,
  RunTarget
} from "../types";
import { conversationHasNoContexts } from "./conversationBodies";

/**
 * Fixed ID for the renderer-owned draft conversation. It is never persisted or
 * sent to the host; a fixed value makes draft detection a string comparison.
 * There is one draft for the whole app, not one per project: which project it
 * belongs to is one of the things it leaves open.
 */
export const DRAFT_CONVERSATION_ID = "__draft__";

/** Renderer-owned draft state. See `draftConversation` in App. */
export interface DraftConversationState {
  /** The project it would materialize into, freely changed until then. Null
   * materializes into the temporary workspace. */
  workspaceId: string | null;
  /**
   * The conversation id the draft will materialize as, minted up front.
   *
   * What the draft opens at the host before then — its terminals and its preview page — is
   * owned by this id from the start, so becoming a real conversation hands them over without
   * moving anything. Aiming the draft at another project ends all of that and mints a new one:
   * a shell opened in one project is never handed to a conversation in another.
   */
  materializesAs: string;
  settings: ConversationSettings;
  createdAt: string;
  /**
   * The project workspaces (1-based) the user ticked the worktree box for.
   *
   * This is intent only: worktrees belong to persisted conversation IDs. They
   * are created after the conversation materializes and before the first
   * message, when all tool calls must already target the isolated checkouts.
   */
  worktreeMembers: number[];
  /** The draft's run location, persisted directly in `Conversation.runTarget` when materialized. */
  runTarget: RunTarget | null;
  /**
   * Workspaces the draft may work in besides its primary one, persisted directly
   * in `Conversation.attachedWorkspaces` when materialized.
   *
   * Unlike `worktreeMembers` this is not just intent: the host authorized each
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
 * Whether a persisted conversation holds nothing yet. New tasks start as the renderer draft, so
 * this is the rare real conversation that is still empty — one that materialized for a tool call
 * the host then refused, or one left from when every project kept an empty slot of its own. The
 * sidebar withholds it until it holds something.
 *
 * Nesting counts as content because hiding a parent would orphan its children in the tree.
 */
export function isUnsentConversation(conversation: Conversation, hasChildren = false): boolean {
  return !hasChildren
    // A body that is not loaded is not an empty one (`conversationBodies.ts`).
    && conversationHasNoContexts(conversation)
    && conversation.queuedMessages.length === 0;
}

/** The workspace's conversations minus the ones that are still empty. */
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
 * or on the first tool the user runs or records in it, not on the first message.
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
    worktrees: [],
    runTarget: draft.runTarget,
    attachedWorkspaces: draft.attachedWorkspaces,
    parentConversationId: null,
    presetId: draft.presetId,
    templateId: draft.templateId
  };
}
