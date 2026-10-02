import type {
  LocalModelDefaultPrompts,
  LocalModelPromptReport,
  LocalModelStatus,
  LocalModelTask,
  LocalModelVariantId
} from "../types";
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
 * command and error explanations) and the one-line explanations it wrote.
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
  promptInfo: (task: LocalModelTask, prompt?: string, build?: boolean) => Promise<LocalModelPromptReport>;
  defaultPrompts: () => Promise<LocalModelDefaultPrompts>;
  subscribePush: typeof onAppPushEvent;
}

export interface LocalModelController {
  subscribe(listener: () => void): () => void;
  /** `null` until the first status arrives. */
  current(): LocalModelStatus | null;
  /** Asks for the whole status; calls made while one is in flight share it. */
  refresh(): Promise<void>;
  /** `chinaMirror` downloads from the mirror in mainland China. */
  install(variant: LocalModelVariantId, chinaMirror: boolean): Promise<void>;
  activate(variant: LocalModelVariantId): Promise<void>;
  cancelInstall(): Promise<void>;
  remove(variant: LocalModelVariantId): Promise<void>;
  /** `build` caches the prompt's state when none is on disk, loading the model if needed. */
  promptInfo(task: LocalModelTask, prompt?: string, build?: boolean): Promise<LocalModelPromptReport>;
  defaultPrompts(): Promise<LocalModelDefaultPrompts>;
}

export function createLocalModelController(backend: LocalModelBackend): LocalModelController {
  let status: LocalModelStatus | null = null;
  const listeners = new Set<() => void>();
  let pushInstalled = false;
  let refreshing: Promise<void> | null = null;
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
    refresh() {
      installPush();
      refreshing ??= backend
        .status()
        .then(set)
        .finally(() => {
          refreshing = null;
        });
      return refreshing;
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
    promptInfo: (task, prompt, build = false) => backend.promptInfo(task, prompt, build),
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
 * for a call still running, whose card is not in the timeline yet. `errors`
 * holds why failed calls failed, keyed the same way: a failed command has an
 * entry in both.
 */
const explanations = new Map<string, string>();
const errors = new Map<string, string>();
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
    const map = event.error ? errors : explanations;
    map.set(event.contextId, event.text);
    map.set(callKey(event.conversationId, event.callId), event.text);
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

/** Why a failed tool card failed, in the local helper model's words. */
export function toolErrorExplanation(contextId: string): string | undefined {
  return errors.get(contextId);
}

function adopt(map: Map<string, string>, stored: Record<string, string>): boolean {
  let changed = false;
  for (const [contextId, text] of Object.entries(stored)) {
    if (map.get(contextId) !== text) {
      map.set(contextId, text);
      changed = true;
    }
  }
  return changed;
}

/** Loads the stored explanations of a conversation once. */
export async function loadToolExplanations(conversationId: string): Promise<void> {
  installExplanationPush();
  if (loadedConversations.has(conversationId)) return;
  loadedConversations.add(conversationId);
  try {
    const stored = await getToolExplanations(conversationId);
    const changed = adopt(explanations, stored.explanations);
    if (adopt(errors, stored.errors) || changed) notifyExplanations();
  } catch (error) {
    loadedConversations.delete(conversationId);
    console.error("读取命令说明失败", error);
  }
}

/** Test hook. */
export function resetToolExplanationsForTests(): void {
  explanations.clear();
  errors.clear();
  loadedConversations.clear();
  explanationVersion = 0;
}

export function recordToolErrorExplanationForTests(contextId: string, text: string): void {
  errors.set(contextId, text);
  notifyExplanations();
}
