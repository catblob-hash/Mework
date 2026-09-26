import type { LocalModelPromptReport, LocalModelStatus, LocalModelVariantId } from "../types";
import { onAppPushEvent } from "./appEvents";
import {
  getToolExplanations,
  localModelActivate,
  localModelCancelInstall,
  localModelDefaultPrompts,
  localModelInstall,
  localModelPromptInfo,
  localModelRemove,
  localModelStatus
} from "./runtime";

/**
 * Renderer-side state of the local helper model (conversation titles, shell
 * command explanations) and the one-line explanations it wrote.
 *
 * Both live outside any component: the host keeps downloading and building
 * whether or not Appearance settings is open, and explanations arrive for
 * whichever conversation is on screen. Components read with
 * `useSyncExternalStore` and call the verbs.
 */

export interface LocalModelBackend {
  status: () => Promise<LocalModelStatus>;
  install: (variant: LocalModelVariantId, chinaMirror: boolean) => Promise<LocalModelStatus>;
  activate: (variant: LocalModelVariantId) => Promise<LocalModelStatus>;
  cancelInstall: () => Promise<void>;
  remove: (variant: LocalModelVariantId) => Promise<LocalModelStatus>;
  promptInfo: (task: "title" | "shell", prompt?: string) => Promise<LocalModelPromptReport>;
  defaultPrompts: () => Promise<{ title: string; shell: string }>;
  subscribePush: typeof onAppPushEvent;
}

export interface LocalModelController {
  subscribe(listener: () => void): () => void;
  /** `null` until the first status arrives. */
  current(): LocalModelStatus | null;
  refresh(): Promise<void>;
  /** `chinaMirror` downloads from the mirror in mainland China. */
  install(variant: LocalModelVariantId, chinaMirror: boolean): Promise<void>;
  activate(variant: LocalModelVariantId): Promise<void>;
  cancelInstall(): Promise<void>;
  remove(variant: LocalModelVariantId): Promise<void>;
  promptInfo(task: "title" | "shell", prompt?: string): Promise<LocalModelPromptReport>;
  defaultPrompts(): Promise<{ title: string; shell: string }>;
}

export function createLocalModelController(backend: LocalModelBackend): LocalModelController {
  let status: LocalModelStatus | null = null;
  const listeners = new Set<() => void>();
  let pushInstalled = false;
  const set = (next: LocalModelStatus): void => {
    status = next;
    for (const listener of [...listeners]) listener();
  };
  const installPush = (): void => {
    if (pushInstalled) return;
    pushInstalled = true;
    backend.subscribePush((event) => {
      if (event.type === "localModelChanged") set(event.status);
    });
  };
  return {
    subscribe(listener) {
      installPush();
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current: () => status,
    async refresh() {
      installPush();
      set(await backend.status());
    },
    async install(variant, chinaMirror) {
      installPush();
      set(await backend.install(variant, chinaMirror));
    },
    async activate(variant) {
      installPush();
      set(await backend.activate(variant));
    },
    async cancelInstall() {
      await backend.cancelInstall();
    },
    async remove(variant) {
      set(await backend.remove(variant));
    },
    promptInfo: (task, prompt) => backend.promptInfo(task, prompt),
    defaultPrompts: () => backend.defaultPrompts()
  };
}

export const localModelController = createLocalModelController({
  status: localModelStatus,
  install: localModelInstall,
  activate: localModelActivate,
  cancelInstall: localModelCancelInstall,
  remove: localModelRemove,
  promptInfo: localModelPromptInfo,
  defaultPrompts: localModelDefaultPrompts,
  subscribePush: onAppPushEvent
});

// ---------------------------------------------------------------- explanations

/**
 * Explanations by tool card id, plus by `<conversation>\u0000<provider call id>`
 * for a call still running, whose card is not in the timeline yet.
 */
const explanations = new Map<string, string>();
const explanationListeners = new Set<() => void>();
const loadedConversations = new Set<string>();
let explanationVersion = 0;
let explanationPushInstalled = false;

function callKey(conversationId: string, callId: string): string {
  return `${conversationId}\u0000${callId}`;
}

function notifyExplanations(): void {
  explanationVersion += 1;
  for (const listener of [...explanationListeners]) listener();
}

function installExplanationPush(): void {
  if (explanationPushInstalled) return;
  explanationPushInstalled = true;
  onAppPushEvent((event) => {
    if (event.type !== "toolExplained") return;
    explanations.set(event.contextId, event.text);
    explanations.set(callKey(event.conversationId, event.callId), event.text);
    notifyExplanations();
  });
}

export function subscribeToolExplanations(listener: () => void): () => void {
  installExplanationPush();
  explanationListeners.add(listener);
  return () => explanationListeners.delete(listener);
}

/** Changes whenever an explanation arrives; a `useSyncExternalStore` snapshot. */
export function toolExplanationVersion(): number {
  return explanationVersion;
}

/** The explanation for a tool card, by its id or, while running, its call id. */
export function toolExplanation(
  contextId: string,
  conversationId?: string,
  callId?: string
): string | undefined {
  return explanations.get(contextId)
    ?? (conversationId && callId ? explanations.get(callKey(conversationId, callId)) : undefined);
}

/** Loads the stored explanations of a conversation once. */
export async function loadToolExplanations(conversationId: string): Promise<void> {
  installExplanationPush();
  if (loadedConversations.has(conversationId)) return;
  loadedConversations.add(conversationId);
  try {
    const stored = await getToolExplanations(conversationId);
    let changed = false;
    for (const [contextId, text] of Object.entries(stored)) {
      if (explanations.get(contextId) !== text) {
        explanations.set(contextId, text);
        changed = true;
      }
    }
    if (changed) notifyExplanations();
  } catch (error) {
    loadedConversations.delete(conversationId);
    console.error("读取命令说明失败", error);
  }
}

/** Test hook. */
export function resetToolExplanationsForTests(): void {
  explanations.clear();
  loadedConversations.clear();
  explanationVersion = 0;
}

export function recordToolExplanationForTests(contextId: string, text: string): void {
  explanations.set(contextId, text);
  notifyExplanations();
}
