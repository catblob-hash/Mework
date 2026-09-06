import type { AppDocument, Conversation } from "../types";
import { errorMessage } from "./errors";
import {
  createConversationRemote,
  deleteConversationRemote,
  hasConversationCommands,
  loadConversationRemote,
  reorderConversationsRemote,
  updateConversationRemote
} from "./runtime";

/** Debounce window that combines consecutive edits to one conversation command. */
const COMMIT_DEBOUNCE_MS = 250;

/** Destination for host-authoritative conversation bodies. It wins over divergent renderer optimism. */
export type ConversationAuthoritySink = (
  workspaceId: string,
  conversation: Conversation
) => void;

interface PendingCommit {
  workspaceId: string;
  /** Main-timeline context IDs visible to the renderer before this debounce window. */
  baseContextIds: string[];
  next: Conversation;
  timer: ReturnType<typeof setTimeout> | null;
}

export interface ConversationSync {
  /** A conversation, including a fork destination, was created. `orderedIds` is the workspace's new sidebar order. */
  created(workspaceId: string, conversation: Conversation, orderedIds: string[]): void;
  /** A conversation was deleted. */
  deleted(workspaceId: string, conversationId: string): void;
  /** A conversation changed. `base` is the renderer-visible version before the change. */
  changed(workspaceId: string, base: Conversation, next: Conversation): void;
  /** The sidebar order changed or a conversation moved workspaces. */
  reordered(workspaceId: string, orderedIds: string[]): void;
  /** Diff entry point for an in-place document replacement. Compares object identity so unchanged conversations issue no commands. */
  syncDocument(previous: AppDocument | null, next: AppDocument): void;
  /** Retrieves the host-authoritative body and passes it to the sink after turn settlement. */
  refresh(conversationId: string): Promise<Conversation | null>;
  /** Waits for every in-flight command. Call before starting a run so the user message is persisted. */
  flush(): Promise<void>;
  /** Reason the host most recently rejected a write for this conversation; clears after success. */
  lastFailure(conversationId: string): string | null;
}

/**
 * Command channel for conversation data.
 *
 * The host is the sole writer of conversation bodies; the renderer sends intent.
 * Without a backend, browser previews and frontend tests keep the document in
 * localStorage and have no second writer.
 */
export function createConversationSync(
  applyAuthority: ConversationAuthoritySink
): ConversationSync {
  const pending = new Map<string, PendingCommit>();
  /** Commands execute in send order so creation, mutation, and deletion cannot overtake each other. */
  let tail: Promise<unknown> = Promise.resolve();
  /** Host-rejected write reasons by conversation, cleared after successful writes. */
  const failures = new Map<string, string>();

  const enqueue = <T>(operation: () => Promise<T>): Promise<T> => {
    const next = tail.then(operation, operation);
    tail = next.catch(() => undefined);
    return next;
  };

  /**
   * On a rejected write, the renderer's optimistic copy is no longer valid.
   * Reload the host-authoritative body so a run never uses a context the host
   * did not accept.
   */
  const adoptAuthorityAfterRefusal = async (
    workspaceId: string,
    conversationId: string,
    error: unknown
  ) => {
    failures.set(conversationId, errorMessage(error, "宿主拒收了这次对话写入"));
    const authoritative = await loadConversationRemote(conversationId).catch(() => null);
    if (authoritative) applyAuthority(workspaceId, authoritative);
  };

  const commit = (conversationId: string) => {
    const entry = pending.get(conversationId);
    if (!entry) return;
    pending.delete(conversationId);
    if (entry.timer) clearTimeout(entry.timer);
    void enqueue(async () => {
      let authoritative: Conversation | null;
      try {
        authoritative = await updateConversationRemote(
          entry.workspaceId,
          entry.next,
          entry.baseContextIds
        );
      } catch (error) {
        await adoptAuthorityAfterRefusal(entry.workspaceId, conversationId, error);
        throw error;
      }
      failures.delete(conversationId);
      if (authoritative) applyAuthority(entry.workspaceId, authoritative);
    }).catch((error) => {
      console.error("对话改动未能写入宿主", error);
    });
  };

  const active = (): boolean => hasConversationCommands();

  return {
    created(workspaceId, conversation, orderedIds) {
      if (!active()) return;
      void enqueue(async () => {
        let stored: Conversation | null;
        try {
          stored = await createConversationRemote(workspaceId, conversation);
        } catch (error) {
          await adoptAuthorityAfterRefusal(workspaceId, conversation.id, error);
          throw error;
        }
        failures.delete(conversation.id);
        if (stored) applyAuthority(workspaceId, stored);
        await reorderConversationsRemote(workspaceId, orderedIds);
      }).catch((error) => {
        console.error("新建对话未能写入宿主", error);
      });
    },

    deleted(workspaceId, conversationId) {
      if (!active()) return;
      const entry = pending.get(conversationId);
      if (entry?.timer) clearTimeout(entry.timer);
      pending.delete(conversationId);
      failures.delete(conversationId);
      void enqueue(() => deleteConversationRemote(workspaceId, conversationId)).catch((error) => {
        console.error("删除对话未能写入宿主", error);
      });
    },

    changed(workspaceId, base, next) {
      if (!active()) return;
      const existing = pending.get(next.id);
      if (existing?.timer) clearTimeout(existing.timer);
      const entry: PendingCommit = {
        workspaceId,
        // The first baseline in a debounce window is the host-visible one;
        // renderer-created intermediate states cannot be used as its baseline.
        baseContextIds: existing?.baseContextIds
          ?? base.contexts.map((context) => context.id),
        next,
        timer: null
      };
      entry.timer = setTimeout(() => commit(next.id), COMMIT_DEBOUNCE_MS);
      pending.set(next.id, entry);
    },

    reordered(workspaceId, orderedIds) {
      if (!active()) return;
      void enqueue(() => reorderConversationsRemote(workspaceId, orderedIds)).catch((error) => {
        console.error("对话顺序未能写入宿主", error);
      });
    },

    syncDocument(previous, next) {
      if (!active() || !previous) return;
      const before = new Map<string, Conversation>();
      for (const workspace of previous.workspaces) {
        for (const conversation of workspace.conversations) before.set(conversation.id, conversation);
      }
      for (const workspace of next.workspaces) {
        for (const conversation of workspace.conversations) {
          const base = before.get(conversation.id);
          if (base === conversation) continue;
          if (base) this.changed(workspace.id, base, conversation);
          else {
            this.created(
              workspace.id,
              conversation,
              workspace.conversations.map((candidate) => candidate.id)
            );
          }
        }
      }
    },

    async refresh(conversationId) {
      if (!active()) return null;
      await this.flush();
      return loadConversationRemote(conversationId);
    },

    async flush() {
      if (!active()) return;
      for (const conversationId of [...pending.keys()]) commit(conversationId);
      await tail;
    },

    lastFailure(conversationId) {
      return failures.get(conversationId) ?? null;
    }
  };
}
