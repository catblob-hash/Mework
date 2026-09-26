/**
 * Back and forward through the conversations opened before, the way a browser walks its
 * history: opening a conversation drops everything ahead of the current entry and appends
 * it; back and forward only move the cursor.
 *
 * Entries are real conversations only. The new-task draft is a place to start from, not a
 * page to return to, so while it is open the cursor still points at the conversation left
 * for it — back returns there, and forward goes on from it.
 */
export interface ConversationHistoryEntry {
  workspaceId: string;
  conversationId: string;
}

export interface ConversationHistory {
  entries: readonly ConversationHistoryEntry[];
  /** The entry last visited; −1 before any. */
  index: number;
}

export const EMPTY_CONVERSATION_HISTORY: ConversationHistory = { entries: [], index: -1 };

/** Older entries fall off the front; nobody walks back a hundred conversations. */
const HISTORY_LIMIT = 100;

/** Records that `entry` was opened. Landing on the current entry (back, forward) changes nothing. */
export function visitConversation(
  history: ConversationHistory,
  entry: ConversationHistoryEntry
): ConversationHistory {
  if (history.entries[history.index]?.conversationId === entry.conversationId) return history;
  const entries = [...history.entries.slice(0, history.index + 1), entry].slice(-HISTORY_LIMIT);
  return { entries, index: entries.length - 1 };
}

/**
 * Where one step back (`-1`) or forward (`1`) lands, or `null` when nothing is there.
 * Entries whose conversation is gone are stepped over, as is the one already open. The
 * conversation may have moved to another project since, so `locate` answers where it is now.
 */
export function conversationHistoryTarget(
  history: ConversationHistory,
  activeConversationId: string | null,
  step: -1 | 1,
  locate: (conversationId: string) => string | null
): { index: number; workspaceId: string; conversationId: string } | null {
  const onCurrent = history.entries[history.index]?.conversationId === activeConversationId;
  const start = step < 0 ? (onCurrent ? history.index - 1 : history.index) : history.index + 1;
  for (let index = start; index >= 0 && index < history.entries.length; index += step) {
    const { conversationId } = history.entries[index];
    if (conversationId === activeConversationId) continue;
    const workspaceId = locate(conversationId);
    if (workspaceId) return { index, workspaceId, conversationId };
  }
  return null;
}
