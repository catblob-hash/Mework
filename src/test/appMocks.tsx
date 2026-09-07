/**
 * Shared mocks and fixtures for App integration tests.
 *
 * Keep `vi.mock` declarations in individual test files because Vitest hoists by file. Factories
 * import mock-only instances from `appMockInstances` to avoid module-loading cycles; tests import
 * the same instances and fixtures from this module.
 */
import { act, screen, waitFor, within } from "@testing-library/react";
import type userEvent from "@testing-library/user-event";
import { expect, vi } from "vitest";
import {
  browserMocks,
  browserRendererMountMocks,
  closedBrowserDisposition,
  gitMocks,
  resetAppPushListeners,
  runtimeMocks,
  terminalMocks
} from "./appMockInstances";
import { createTestDocument as createSeedDocument } from "./fixtures";
import { configureI18n } from "../i18n";
import { CONVERSATION_TURNS_STORAGE_KEY } from "../lib/conversationTurns";
import { SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY } from "../components/Sidebar";
import { ASK_USER_PENDING_OUTPUT } from "../types";
import type {
  AppDocument,
  ModelProfile,
  ModelRunResponse,
  ModelStreamEvent,
  ToolContext,
  UserContext
} from "../types";

export {
  browserMocks,
  browserRendererMountMocks,
  closedBrowserDisposition,
  gitMocks,
  runtimeMocks,
  terminalMocks,
  terminalPanelModuleMock
} from "./appMockInstances";

// The seed document's first provider is the Responses family, so `encrypted` is
// the form that family resolves to.
export const model: ModelProfile = {
  id: "test-model",
  name: "",
  group: "",
  capabilities: [],
  reasoningContent: "encrypted",
  promptCache: true
};

export function documentWithModel(): AppDocument {
  const document = createSeedDocument();
  document.globalSettings.apiProviders[0] = {
    ...document.globalSettings.apiProviders[0],
    enabled: true,
    models: [model],
    activeModelId: model.id
  };
  document.globalSettings.activeProviderId = document.globalSettings.apiProviders[0].id;
  document.workspaces[0].conversations[0] = {
    ...document.workspaces[0].conversations[0],
    title: "新任务",
    contexts: []
  };
  document.workspaces[0].conversations = [document.workspaces[0].conversations[0]];
  return document;
}

export function budgetImages(prefix: string, count: number) {
  return Array.from({ length: count }, (_, index) => ({
    id: `${prefix}-${index}`,
    name: `${prefix}-${index}.png`,
    mime: "image/png",
    width: 1,
    height: 1,
    bytes: 1
  }));
}

export function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

export function answeredQuestionPair(): { ask: ToolContext; answer: UserContext } {
  const ask: ToolContext = {
    id: "question-pair-ask",
    kind: "tool",
    toolName: "ask_user",
    round: 1,
    input: {
      questions: [{
        question: "采用哪个方案？",
        header: "方案",
        options: [
          { label: "方案 A", description: "保持改动最小" },
          { label: "方案 B", description: "完整重构" }
        ],
        multiSelect: false
      }]
    },
    result: {
      success: true,
      output: ASK_USER_PENDING_OUTPUT,
      executedAt: "2026-07-23T00:00:00Z",
      durationMs: 0
    },
    createdAt: "2026-07-23T00:00:00Z"
  };
  const answer: UserContext = {
    id: "question-pair-answer",
    kind: "user",
    content: 'User has answered your questions: "采用哪个方案？"="方案 A"',
    createdAt: "2026-07-23T00:00:01Z"
  };
  return { ask, answer };
}

export function settledStateTool(
  id: string,
  toolName: string,
  input: ToolContext["input"],
  output: unknown
): ToolContext {
  return {
    id,
    kind: "tool",
    toolName,
    round: 1,
    input,
    result: {
      success: true,
      output: JSON.stringify(output),
      executedAt: "2026-07-24T00:00:00Z",
      durationMs: 1
    },
    createdAt: "2026-07-24T00:00:00Z"
  };
}

export function taskCreateContext(id: string, taskId: string, subject: string): ToolContext {
  return settledStateTool(id, "todo", {
    action: "create",
    subject,
    description: `${subject}的详细说明`,
    activeForm: `正在${subject}`
  }, {
    task: { id: taskId, subject }
  });
}

export function taskUpdateContext(
  id: string,
  taskId: string,
  status: "pending" | "in_progress" | "completed" | "deleted"
): ToolContext {
  return settledStateTool(id, "todo", { action: "update", taskId, status }, {
    success: true,
    taskId,
    updatedFields: ["status"]
  });
}

export function taskGetContext(id: string, taskId: string, subject: string): ToolContext {
  return settledStateTool(id, "todo", { action: "get", taskId }, {
    task: { id: taskId, subject, status: "pending" }
  });
}

export function taskListContext(id: string, taskIds: string[]): ToolContext {
  return settledStateTool(id, "todo", { action: "list" }, {
    tasks: taskIds.map((taskId) => ({ id: taskId, subject: taskId, status: "pending" }))
  });
}

/** Opens the overview's Git card. Only present when the workspace is a repo. */
export async function expandGitStatus(user: ReturnType<typeof userEvent.setup>) {
  const gitStatus = await screen.findByRole("complementary", { name: "Git 状态" });
  if (within(gitStatus).queryByRole("button", { name: "收起 Git 状态卡片" })) return gitStatus;
  await user.click(within(gitStatus).getByRole("button", { name: "展开 Git 状态卡片" }));
  return gitStatus;
}

/**
 * Opens the task sidebar and returns it. A closed container is `aria-hidden`,
 * so it is unreachable by role until the toggle has actually been pressed. The
 * container's own header carries a same-named close button, hence the class
 * filter for the topbar one.
 */
export async function openTaskContainer(user: ReturnType<typeof userEvent.setup>) {
  const toggles = await screen.findAllByRole("button", { name: /^(打开|收起)任务容器$/ });
  const toggle = toggles.find((button) => button.classList.contains("topbar-sidebar-toggle"))!;
  if (toggle.getAttribute("aria-expanded") !== "true") await user.click(toggle);
  return screen.getByRole("complementary", { name: "任务容器" });
}

/**
 * Drives one `playwright` tool call to completion, which is what registers a preview session.
 *
 * The host mints the native page on any playwright action, and the renderer learns about it from
 * that call's completion — there is no manual "open a browser" affordance any more, so this is
 * the only way a preview exists.
 */
export async function registerAgentPreview(
  user: ReturnType<typeof userEvent.setup>,
  options: { output?: string; prompt?: string } = {}
) {
  let emit!: (event: ModelStreamEvent) => void;
  let resolveRun!: (value: ModelRunResponse) => void;
  runtimeMocks.runModel.mockImplementation((_request: unknown, onEvent: (event: ModelStreamEvent) => void) => {
    emit = onEvent;
    return new Promise<ModelRunResponse>((resolve) => { resolveRun = resolve; });
  });

  await user.type(
    await screen.findByLabelText("向 Agent 发送消息"),
    options.prompt ?? "打开文档页面"
  );
  await user.click(screen.getByRole("button", { name: "发送" }));
  await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalled());

  act(() => {
    emit({
      type: "tool_call_announced",
      round: 1,
      callId: "call-playwright",
      toolName: "playwright",
      contextId: "ctx-call-playwright"
    });
    emit({
      type: "tool_call_arguments_ready",
      round: 1,
      callId: "call-playwright",
      input: { action: "navigate", url: "https://example.test/docs" }
    });
    emit({ type: "tool_execution_started", round: 1, callId: "call-playwright" });
    emit({
      type: "tool_execution_completed",
      round: 1,
      callId: "call-playwright",
      result: {
        success: true,
        output: options.output ?? "{}",
        executedAt: "2026-08-27T01:00:00Z",
        durationMs: 12
      }
    });
  });

  return { emit, resolveRun };
}

/**
 * Opens the preview page for one session from the task bar.
 *
 * Previews are no longer something a user creates: the host mints a session when the model calls
 * `playwright`, the renderer registers it from that tool's completion, and the task row is the
 * only way in. So a test has to register the session first — see `registerAgentPreview` — and
 * then click its row, exactly as a person would.
 */
export async function openPreviewPage(
  user: ReturnType<typeof userEvent.setup>,
  rowName: string | RegExp
) {
  const container = await openTaskContainer(user);
  await user.click(await within(container).findByRole("button", { name: rowName }));
  return screen.getByRole("region", { name: /页面$/ });
}


/**
 * Reads a composer option from its trigger's accessible name. Popover menus replaced native
 * `<select>` controls, so the current value is encoded as `<prefix>：<value>`.
 */
export function composerOptionTrigger(prefix: string): HTMLElement {
  return screen.getByRole("button", { name: new RegExp(`^${prefix}：`) });
}

/** Returns the segment after the full-width colon in the trigger's accessible name. */
export function composerOptionValue(prefix: string): string {
  const label = composerOptionTrigger(prefix).getAttribute("aria-label") ?? "";
  return label.slice(label.indexOf("：") + 1);
}

/** Opens a composer option menu. Only one may be open, and composer controls have no submenus. */
export async function openComposerOption(
  user: ReturnType<typeof userEvent.setup>,
  prefix: string
) {
  const trigger = composerOptionTrigger(prefix);
  if (trigger.getAttribute("aria-expanded") !== "true") await user.click(trigger);
  return screen.getByRole("menu");
}

/** Opens a composer option menu and selects an item by accessible name. */
export async function chooseComposerOption(
  user: ReturnType<typeof userEvent.setup>,
  prefix: string,
  name: string | RegExp
) {
  const menu = await openComposerOption(user, prefix);
  await user.click(within(menu).getByRole("menuitemradio", { name }));
}

/** Resets mocks and browser state before each App integration test. */
export function resetAppMocks() {
  configureI18n("zh-CN");
  resetAppPushListeners();
  runtimeMocks.attachModelRun.mockReset().mockResolvedValue({ status: "none" });
  // Default to no host-owned run so cancellation falls back to the request-ID path.
  runtimeMocks.cancelConversationRun.mockReset().mockResolvedValue(false);
  runtimeMocks.cancelModelRun.mockReset().mockResolvedValue(true);
  runtimeMocks.listResumableRuns.mockReset().mockResolvedValue([]);
  runtimeMocks.listWslDistros.mockReset().mockResolvedValue([]);
  runtimeMocks.listWakePendingConversations.mockReset().mockResolvedValue([]);
  runtimeMocks.listPendingToolPrompts.mockReset().mockResolvedValue([]);
  runtimeMocks.listPendingForkRequests.mockReset().mockResolvedValue([]);
  runtimeMocks.listPendingForkStarts.mockReset().mockResolvedValue([]);
  runtimeMocks.listForkDecisions.mockReset().mockResolvedValue([]);
  runtimeMocks.loadConversationPlan.mockReset().mockResolvedValue(null);
  runtimeMocks.workflowStepRecord.mockReset().mockResolvedValue(null);
  runtimeMocks.resolveForkRequest.mockReset().mockResolvedValue(null);
  runtimeMocks.takeRunSettlement.mockReset().mockResolvedValue(null);
  runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
  runtimeMocks.executeTool.mockReset();
  runtimeMocks.fetchModels.mockReset();
  runtimeMocks.forkConversationContexts.mockReset().mockResolvedValue([]);
  runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
  runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
  runtimeMocks.revealApiKey.mockReset().mockResolvedValue("test-secret");
  runtimeMocks.loadDocument.mockReset();
  runtimeMocks.imageAttachmentData.mockReset().mockResolvedValue("data:image/png;base64,AAAA");
  runtimeMocks.prepareImageAttachment.mockReset();
  runtimeMocks.refreshCapabilities.mockReset();
  runtimeMocks.requestToolApproval.mockReset().mockResolvedValue({ nonce: "approval-once", expiresInMs: 90_000 });
  runtimeMocks.resolveToolPrompt.mockReset().mockResolvedValue({ nonce: "approval-once", expiresInMs: 90_000 });
  runtimeMocks.resetDocument.mockReset();
  runtimeMocks.runModel.mockReset();
  runtimeMocks.saveApiKey.mockReset().mockResolvedValue({ configured: true });
  runtimeMocks.executeTool.mockResolvedValue({
    success: true,
    output: "approved",
    executedAt: "2026-07-11T00:00:00Z",
    durationMs: 1
  });
  runtimeMocks.saveDocument.mockReset().mockResolvedValue(undefined);
  runtimeMocks.steerModelRun.mockReset().mockResolvedValue(undefined);
  runtimeMocks.hasConversationCommands.mockReset().mockReturnValue(false);
  runtimeMocks.createConversationRemote.mockReset().mockResolvedValue(null);
  runtimeMocks.deleteConversationRemote.mockReset().mockResolvedValue(undefined);
  runtimeMocks.updateConversationRemote.mockReset().mockResolvedValue(null);
  runtimeMocks.reorderConversationsRemote.mockReset().mockResolvedValue(undefined);
  runtimeMocks.loadConversationRemote.mockReset().mockResolvedValue(null);
  terminalMocks.closeTerminal.mockReset().mockResolvedValue(undefined);
  browserMocks.openBrowser.mockReset().mockResolvedValue({
    hasPage: true,
    open: true,
    loading: false,
    url: "about:blank",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 560, height: 720 }
  });
  gitMocks.executeGitAction.mockReset().mockResolvedValue({ snapshot: null });
  gitMocks.getGitBranches.mockReset().mockResolvedValue({ branches: [], defaultBranch: null });
  gitMocks.createConversationWorktree.mockReset();
  gitMocks.releaseConversationWorktree.mockReset().mockResolvedValue(true);
  gitMocks.getGitChangePage.mockReset().mockResolvedValue({
    kind: "page",
    revision: "",
    files: [],
    matchedCount: 0,
    nextCursor: null,
    selection: null
  });
  gitMocks.getGitDiff.mockReset().mockResolvedValue({
    patch: "",
    path: null,
    additions: 0,
    deletions: 0,
    binary: false,
    truncated: false,
    files: []
  });
  gitMocks.getGitWorkspaceSummary.mockReset().mockResolvedValue({ kind: "notRepository" });
  browserMocks.getBrowserStatus.mockReset().mockResolvedValue({
    hasPage: true,
    open: true,
    loading: false,
    url: "about:blank",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 560, height: 720 }
  });
  browserMocks.closeBrowserSession.mockReset().mockResolvedValue(closedBrowserDisposition());
  browserMocks.performBrowserAction.mockReset().mockImplementation(async (
    _conversationId: string,
    action: string
  ) => ({
    hasPage: action !== "close",
    open: false,
    loading: false,
    url: action === "close" ? "" : "about:blank",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 560, height: 720 }
  }));
  browserMocks.setBrowserPanelBounds.mockReset().mockResolvedValue({
    hasPage: true,
    open: true,
    loading: false,
    url: "about:blank",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 560, height: 720 }
  });
  browserRendererMountMocks.startHeartbeat.mockReset().mockResolvedValue(undefined);
  browserRendererMountMocks.stopHeartbeat.mockReset();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  window.localStorage.removeItem("mework.sidebar-width");
  window.localStorage.removeItem("mework.right-sidebar-width");
  window.localStorage.removeItem("naiword.sidebar-width");
  window.localStorage.removeItem("naiword.right-sidebar-width");
  window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY);
  window.localStorage.removeItem(SIDEBAR_COLLAPSED_PARENTS_STORAGE_KEY);
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: 1024
  });
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: vi.fn()
  });
}
