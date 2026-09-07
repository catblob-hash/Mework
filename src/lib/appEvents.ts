import {
  Channel,
  hasBackendRuntime,
  invoke,
  isBrowserDevRuntime,
  onBrowserDevReconnected
} from "./backend";
import type {
  ContextItem,
  ConversationPlan,
  PendingForkRequest,
  PendingToolPrompt,
  SecurityLevel
} from "../types";
import type { ShellTaskSnapshot } from "./shellTasks";

/** One tool card the host could not attest and therefore did not persist. */
export interface QuarantinedToolContext {
  workspaceId: string;
  conversationId: string;
  contextId: string;
  toolName: string;
  replacement: ContextItem;
}

/** Mirror of Rust `push_events::AppPushEvent` (serde tag `type`, camelCase). */
export type AppPushEvent =
  | { type: "documentWriteFailure"; message: string }
  | { type: "documentWriteRecovered" }
  // The host could not prove these cards came from its own execution, so it
  // replaced each one with an exact local-only marker and saved the rest. The
  // renderer installs the same replacement to keep the audit evidence.
  | { type: "toolContextsQuarantined"; contexts: QuarantinedToolContext[] }
  // When a proposed conversation state fails conversation-domain validation, the host restores its last committed snapshot (or discards a newly tainted conversation) while saving the remaining document. The host deduplicates by conversation, rejectedUpdatedAt, and error.
  | {
      type: "conversationSaveRejected";
      workspaceId: string;
      conversationId: string;
      rejectedUpdatedAt: string;
      error: string;
    }
  // Neither shell commands nor web searches begin in the renderer. Their
  // registry changes are the only way these background tasks reach the sidebar;
  // end events carry whole snapshots because rows remain as finished history.
  | { type: "shellTaskStarted"; task: ShellTaskSnapshot }
  | { type: "shellTaskEnded"; task: ShellTaskSnapshot }
  // The host dropped a finished row from its bounded retention. The renderer must drop it too:
  // a row it kept would still open, but its output could no longer be read.
  | { type: "shellTaskEvicted"; conversationId: string; shellTaskId: string }
  // A terminal background-task result without an active model run requires a message-less wake run to fold it into the first round boundary. Every terminal result qualifies, a task the user closed included.
  | { type: "taskSettled"; conversationId: string }
  // Background tasks can require dangerous-tool approval when no unsettled run can carry its card. Deliver the prompt through this push event and resolve it with `resolveToolPrompt`.
  | { type: "toolApprovalRequested"; conversationId: string } & PendingToolPrompt
  | {
      type: "toolApprovalResolved";
      conversationId: string;
      promptId: string;
      approved: boolean;
    }
  // The model called `fork` below full access. The tool has already returned; this
  // card is the whole of what asks the user, so it never rides a run stream.
  | ({ type: "forkRequested" } & PendingForkRequest)
  // A fork request ended: answered, auto-approved under full access, or retracted
  // with its source conversation. `childConversationId` is set exactly when a child
  // exists; the renderer loads it and starts its first run.
  | {
      type: "forkResolved";
      forkId: string;
      workspaceId: string;
      sourceConversationId: string;
      approved: boolean;
      childConversationId: string | null;
    }
  // The host changed a conversation's security level on its own — entering plan
  // mode, or leaving it once the plan was approved. The renderer mirrors it into
  // the conversation without writing back: the host already committed it.
  | { type: "conversationSecurityLevelChanged"; conversationId: string; securityLevel: SecurityLevel }
  // The conversation's plan document was written, approved, sent back, or cleared.
  | { type: "conversationPlanUpdated"; conversationId: string; plan: ConversationPlan | null };

type AppPushEventListener = (event: AppPushEvent) => void;

const listeners = new Set<AppPushEventListener>();
let channel: Channel<AppPushEvent> | null = null;
let installed = false;

/**
 * The one channel object is reused across re-subscriptions: the backend keys
 * push messages by channel id, so after a browser-dev reconnect the fresh
 * backend-side bridge keeps delivering into the same renderer callback.
 */
async function subscribe(): Promise<void> {
  if (!channel) {
    channel = new Channel<AppPushEvent>();
    channel.onmessage = (event) => {
      for (const listener of [...listeners]) listener(event);
    };
  }
  await invoke("subscribe_app_events", { onEvent: channel });
}

/**
 * Registers a listener for backend-initiated push events, installing the
 * single long-lived backend subscription on first use. Without a backend
 * runtime (pure-web preview) listeners simply never fire.
 */
export function onAppPushEvent(listener: AppPushEventListener): () => void {
  listeners.add(listener);
  if (!installed && hasBackendRuntime()) {
    installed = true;
    if (isBrowserDevRuntime()) {
      onBrowserDevReconnected(() => {
        subscribe().catch((error) => console.error("重新订阅后端推送事件失败", error));
      });
    }
    subscribe().catch((error) => console.error("订阅后端推送事件失败", error));
  }
  return () => {
    listeners.delete(listener);
  };
}
