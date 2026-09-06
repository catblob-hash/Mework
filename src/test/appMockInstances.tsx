/**
 * Pure mock instances. Depend only on Vitest and the JSX runtime; never import
 * modules from src/lib.
 *
 * vi.mock factories obtain these objects through
 * `await import("./test/appMockInstances")`. Importing appMocks from a factory
 * would deadlock: appMocks -> fixtures -> lib/runtime (mocked) -> unfinished
 * factory.
 */
import { vi } from "vitest";

export const runtimeMocks = {
  attachModelRun: vi.fn(),
  cancelConversationRun: vi.fn(),
  cancelModelRun: vi.fn(),
  deleteApiKey: vi.fn(),
  executeTool: vi.fn(),
  fetchModels: vi.fn(),
  forkConversationContexts: vi.fn(),
  getStoredApiKeyLength: vi.fn(),
  forgetStoredApiKeyLength: vi.fn(),
  listResumableRuns: vi.fn(),
  listWslDistros: vi.fn(),
  listWakePendingConversations: vi.fn(),
  listPendingToolPrompts: vi.fn(),
  listPendingForkRequests: vi.fn(),
  listPendingForkStarts: vi.fn().mockResolvedValue([]),
  workflowRunHistory: vi.fn().mockResolvedValue([]),
  workflowStepRecord: vi.fn().mockResolvedValue(null),
  resolveForkRequest: vi.fn(),
  revealApiKey: vi.fn(),
  loadDocument: vi.fn(),
  imageAttachmentData: vi.fn(),
  prepareImageAttachment: vi.fn(),
  refreshCapabilities: vi.fn(),
  requestToolApproval: vi.fn(),
  resolveToolPrompt: vi.fn(),
  resetDocument: vi.fn(),
  runModel: vi.fn(),
  saveApiKey: vi.fn(),
  saveDocument: vi.fn(),
  steerModelRun: vi.fn(),
  takeRunSettlement: vi.fn(),
  // Conversation data-plane commands default to off. Most App tests use the
  // no-backend path where the whole document is authoritative; tests that need
  // host gates enable `hasConversationCommands` explicitly.
  hasConversationCommands: vi.fn(() => false),
  createConversationRemote: vi.fn(),
  deleteConversationRemote: vi.fn(),
  updateConversationRemote: vi.fn(),
  reorderConversationsRemote: vi.fn(),
  loadConversationRemote: vi.fn()
};

export const terminalMocks = {
  closeTerminal: vi.fn()
};

/** Host-push-channel test substitute. `onAppPushEvent` registers local
 * listeners and `emitAppPushEvent` simulates a host event. */
const appPushListeners = new Set<(event: unknown) => void>();

export function emitAppPushEvent(event: unknown) {
  for (const listener of [...appPushListeners]) listener(event);
}

export function resetAppPushListeners() {
  appPushListeners.clear();
}

export function appEventsModuleMock() {
  return {
    onAppPushEvent: (listener: (event: unknown) => void) => {
      appPushListeners.add(listener);
      return () => {
        appPushListeners.delete(listener);
      };
    }
  };
}

export const browserMocks = {
  closeBrowserSession: vi.fn(),
  getBrowserStatus: vi.fn(),
  openBrowser: vi.fn(),
  performBrowserAction: vi.fn(),
  setBrowserPanelBounds: vi.fn()
};

export const browserRendererMountMocks = {
  startHeartbeat: vi.fn(),
  stopHeartbeat: vi.fn()
};

export const gitMocks = {
  createConversationWorktree: vi.fn(),
  executeGitAction: vi.fn(),
  getGitBranches: vi.fn(),
  getGitChangePage: vi.fn(),
  getGitDiff: vi.fn(),
  getGitWorkspaceSummary: vi.fn(),
  releaseConversationWorktree: vi.fn()
};

export function closedBrowserDisposition() {
  return {
    status: "closed" as const,
    intentAccepted: true,
    cleanupComplete: true,
    surfaceHidden: true
  };
}

/** Complete module substitute for vi.mock("./components/TerminalPanel", ...). */
export function terminalPanelModuleMock() {
  return {
    terminalPanelId: (conversationId: string, terminalId: string) => `conversation-terminal-${conversationId}-${terminalId}`,
    TerminalPanel: ({
      conversationId,
      terminalId,
      label,
      open,
      initialState,
      onStateChange
    }: {
      conversationId: string;
      terminalId: string;
      label: string;
      open: boolean;
      initialState: {
        phase: "idle" | "running";
        busy: boolean;
        hasHistory: boolean;
        cwd: string;
        shell: string;
        sessionId: string | null;
      };
      onStateChange: (state: unknown) => void;
    }) => (
      <section
        id={`conversation-terminal-${conversationId}-${terminalId}`}
        className={`collapse-region terminal-panel-region${open ? "" : " collapse-region--closed"}`}
        aria-label={label}
        aria-hidden={!open || undefined}
        inert={!open || undefined}
      >
        <div className="collapse-region__inner terminal-panel-region__inner" />
        <button
          type="button"
          onClick={() => onStateChange({
            ...initialState,
            terminalId,
            conversationId,
            label,
            phase: "running",
            busy: false,
            hasHistory: true,
            cwd: "C:/workspace",
            shell: "PowerShell",
            sessionId: `session-${terminalId}`
          })}
        >
          模拟终端历史
        </button>
      </section>
    )
  };
}
