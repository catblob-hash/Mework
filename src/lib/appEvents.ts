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
  ForkDecisionRecord,
  LocalModelStatus,
  MachineShells,
  PendingForkRequest,
  PendingToolPrompt
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
  // Neither shell commands nor web searches begin in the renderer. Their
  // registry changes are the only way these background tasks reach the sidebar;
  // end events carry whole snapshots because rows remain as finished history.
  | { type: "shellTaskStarted"; task: ShellTaskSnapshot }
  | { type: "shellTaskEnded"; task: ShellTaskSnapshot }
  // The host dropped a finished row from its bounded retention. The renderer must drop it too:
  // a row it kept would still open, but its output could no longer be read.
  | { type: "shellTaskEvicted"; conversationId: string; shellTaskId: string }
  // The dev-server registry changed: one started, came up, was stopped, or died. The list is read
  // back per conversation, so the event means "read it again"; the host keeps one of these in its
  // backlog and merges the rest into it. `stopped` names only the servers this change ended on
  // purpose — a server that exits by itself is gone from the list but is not in here, because a
  // stop takes down the page it was serving and a crash leaves it there to be looked at.
  | { type: "previewServersChanged"; stopped: string[] }
  // The link to an SSH machine's agent changed state. The heartbeat is what
  // notices a dead network; `reconnecting` and `lost` carry the reason, and
  // `unavailable` means the machine keeps the per-command SSH transport.
  | {
      type: "remoteLinkChanged";
      host: string;
      state: "connecting" | "connected" | "reconnecting" | "lost" | "unavailable";
      detail: string | null;
    }
  // A machine was probed for its shell backends: this machine at startup, an
  // SSH machine when this session first reaches it, a WSL distribution on first
  // use, or from a machine's settings. `key` is the machine's environment key.
  | { type: "machineShellsChanged"; key: string; shells: MachineShells }
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
  // The model called `fork`. The card is the gate at every access level — a fork
  // is never created automatically — and the tool has already returned, so this
  // card is the whole of what asks the user and never rides a run stream.
  | ({ type: "forkRequested" } & PendingForkRequest)
  // A fork request ended: answered by the user, or retracted with its source
  // conversation. `childConversationId` is set exactly when a child exists; the
  // renderer loads it and starts its first run.
  | {
      type: "forkResolved";
      forkId: string;
      workspaceId: string;
      sourceConversationId: string;
      approved: boolean;
      childConversationId: string | null;
      /**
       * What the task bar shows for this request. A retraction decided nothing
       * and carries none: the host records a decision only when the user made
       * one.
       */
      decision?: ForkDecisionRecord | null;
    }
  // The model called `handoff`: the conversation continues in
  // `childConversationId`, a fork armed on the host's opening message. The
  // renderer starts it like any host-made fork and follows it when the source
  // is on screen.
  | {
      type: "conversationHandedOff";
      workspaceId: string;
      sourceConversationId: string;
      childConversationId: string;
    }
  // The conversation's plan document was written, approved, sent back, or cleared.
  | { type: "conversationPlanUpdated"; conversationId: string; plan: ConversationPlan | null }
  // The host moved the conversation's plan-mode switch (an approved plan turns
  // it off). Already persisted; the composer mirrors it.
  | { type: "conversationPlanModeChanged"; conversationId: string; enabled: boolean }
  // The host wrote a conversation's title: the chosen message as a placeholder,
  // then the local helper model's title (`settled`). Mirrored without writing back.
  | { type: "conversationTitleChanged"; conversationId: string; title: string; settled: boolean }
  // The local helper model described a shell command (or titled a subagent),
  // or with `error` said why a failed call failed. `contextId` is the tool
  // card's id (the card may not be saved yet); `callId` the provider's call id.
  | { type: "toolExplained"; conversationId: string; contextId: string; callId: string; text: string; error: boolean }
  // The local helper model's install or runtime status changed.
  | { type: "localModelChanged"; status: LocalModelStatus };

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
