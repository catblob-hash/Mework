import { createId } from "./id";
import { estimateContextsTokens } from "./contextTokens";
import { errorMessage } from "./errors";
import { hasUsableBaseUrl, supportsVision } from "./modelCapabilities";
import { findConversation, modelChoiceForConversation } from "./documentUpdates";
import {
  contextsContainProjectedImages,
  MAX_COMPOSER_IMAGE_BYTES,
  MAX_COMPOSER_IMAGE_PIXELS,
  MAX_COMPOSER_IMAGES,
  MAX_IMAGE_ATTACHMENT_BYTES,
  MAX_IMAGE_ATTACHMENT_PIXELS
} from "./imageBudget";
import {
  backfillRequestedToolInput,
  contextsFromInterruptedRun,
  contextsFromModelRun,
  mergeFinalizedInterruptedRunContexts,
  mergeUniqueContexts,
  runStreamContextPrefixes
} from "./runContexts";
import {
  appendImagePlaceholder,
  imagePlaceholderIds,
  imageShortIdsInUse,
  nextImageShortId,
  renumberImagesForSend,
  reserveQueuedMessageIds,
  stripImagePlaceholder,
  textWithoutImagePlaceholders
} from "./imageShortIds";
import {
  attachModelRun,
  cancelConversationRun,
  listResumableRuns,
  prepareImageAttachment,
  runModel,
  steerModelRun,
  takeRunSettlement,
  type RunSettlementPayload
} from "./runtime";
import {
  createModelStreamCoalescer,
  reduceModelStreamEvent,
  trimLiveContextBudget,
  type ModelRunState,
  type ModelStreamEffect
} from "./modelStream";
import type { ComposerController } from "./composerController";
import type { DocumentStore } from "./documentStore";
import type { ModelRunController } from "./modelRunController";
import type {
  ProviderFamily,
  AppDocument,
  ContextItem,
  Conversation,
  ImageAttachment,
  ModelRunRequest,
  ModelStreamEvent,
  ModelUsage,
  PendingToolPrompt,
  QueuedMessage,
  ToolContext,
  ToolDescriptor,
  UserContext
} from "../types";

export type ModelRunErrorState = Pick<
  ModelRunState,
  "providerName" | "modelName" | "workspaceId" | "request"
> & {
  message: string;
  retryable: boolean;
  /** Set when a turn already renders this failure, so the composer does not repeat it. */
  requestId?: string;
};

export interface ContextUsage {
  tokens: number;
  estimated: boolean;
  unprojectable?: boolean;
  providerId?: string;
  family?: ProviderFamily;
}

interface TurnCompletionMeta {
  modelId: string;
  usage: ModelUsage;
  durationMs: number;
}

/**
 * Host seam for the send pipeline. Each call reads current App facilities through
 * `host()`. Fields must be stable functions (a `useCallback` or thin store-method
 * wrapper); the pipeline holds no render snapshot and reads document and draft state
 * from the store at call time.
 */
export interface SendPipelineHost {
  t(zh: string, en: string, params?: Record<string, string | number>): string;
  openProviderSettings(): void;
  activeWorkspaceId(): string | null;
  activeConversationId(): string | null;
  /** Localized tool catalog for the active document, using the same derivation as App rendering. */
  activeConversationTools(): ToolDescriptor[];
  /** Names enabled for the active conversation and present in the catalog. */
  activeEnabledTools(): string[];
  requestFitsImageBudget(contexts: ContextItem[]): boolean;
  contextMutationIsBlocked(conversationId: string): boolean;
  /** Invalidate a pending timeline undo for this conversation; a new turn makes the old deletion final. */
  clearPendingUndo(conversationId: string): void;
  /** Close the open context editor if this is the active conversation. */
  closeEditorIfActive(conversationId: string): void;
  scrollTimelineToBottom(): void;
  persistDocumentImmediately(document: AppDocument, options?: { durable?: boolean }): Promise<void>;
  flushLatestDocument(options?: { durable?: boolean }): Promise<void>;
  updateConversation(
    workspaceId: string,
    conversationId: string,
    updater: (conversation: Conversation) => Conversation,
    options?: { persist?: boolean }
  ): void;
  startConversationTurn(
    conversationId: string,
    requestId: string,
    anchorContextId: string,
    modelId: string,
    startedAt: string,
    contexts: ContextItem[]
  ): void;
  /**
   * Attach a run that carries no new user message to a round.
   *
   * `"awaiting"` resumes the turn an `ask_user` pause left open; `"continue"`
   * resumes the newest round while it is still unfinished — a stop or a failure
   * does not end one, only a new user message or a normal final reply does.
   * Either way a turn exists afterwards, so no run streams into a timeline with
   * no header over it.
   */
  continueConversationTurn(
    conversationId: string,
    requestId: string,
    modelId: string,
    contexts: ContextItem[],
    mode: "continue" | "awaiting"
  ): void;
  /** Restore an adopted same-generation turn to running if loading marked it interrupted. */
  resumeAdoptedConversationTurn(conversationId: string, requestId: string, contexts: ContextItem[], modelId: string): void;
  splitConversationTurn(
    workspaceId: string,
    conversationId: string,
    runBeforeInput: ModelRunState,
    item: UserContext
  ): void;
  updateRunningTurnUsage(
    conversationId: string,
    requestId: string,
    usage: ModelUsage,
    revision: number
  ): void;
  pauseConversationTurnForUser(
    conversationId: string,
    requestId: string,
    contexts: ContextItem[],
    meta: TurnCompletionMeta
  ): void;
  finishConversationTurns(
    conversationId: string,
    requestId: string,
    contexts: ContextItem[],
    status: "completed" | "interrupted",
    meta: TurnCompletionMeta
  ): void;
  persistInterruptedRun(workspaceId: string, conversationId: string, run: ModelRunState): void;
  /** Record on the turn itself why its run stopped, so the timeline explains the gap. */
  failConversationTurn(
    conversationId: string,
    requestId: string,
    error: { message: string; providerName: string; modelName: string }
  ): void;
  /** Replace the render-side read model with authoritative host prose after turn settlement.
   * The host writes each context to the conversation store when it is produced; this
   * only makes the UI exactly match the persisted state. */
  refreshConversation(workspaceId: string, conversationId: string): void;
  /** Reason the host most recently rejected this conversation's write, or null.
   * Include it in a send-boundary error after confirmation fails. */
  conversationWriteFailure(conversationId: string): string | null;
  /** Replace the persisted copy of a host-settled tool context in place by ID and
   * persist it promptly, so subagent transcripts do not depend on full request settlement. */
  applySettledToolContext(workspaceId: string, conversationId: string, context: ToolContext): void;
  /**
   * Forget this conversation's last failure.
   *
   * `"notice"` retracts only the composer notice — correct when reattaching a
   * surviving host run, which is the *same* attempt continuing rather than a new
   * one. `"failure"` also retracts the turn-level notices and the header-only
   * turns that existed to carry them, which is what makes sending again the
   * dismissal gesture. Adoption must never use `"failure"`: it would delete the
   * record of the very failure the user reloaded to read.
   */
  clearModelRunError(conversationId: string, scope: "notice" | "failure"): void;
  setModelRunError(conversationId: string, error: ModelRunErrorState): void;
  setContextUsage(conversationId: string, usage: ContextUsage): void;
  openToolPrompt(conversationId: string, prompt: PendingToolPrompt): void;
  closeToolPrompt(conversationId: string, promptId: string): void;
  /** Reconcile this conversation's approval cards with the host's pending list at
   * run completion. Background-task cards remain visible while the host retains them. */
  reconcileToolPrompts(conversationId: string): void;
  /** Handle every successfully completed tool; the host filters browser-tab allowlists. */
  reconcileAgentBrowserTabs(conversationId: string, toolName: string, output: string): void;
}

export interface SendPipelineStores {
  documentStore: DocumentStore;
  composerController: ComposerController;
  modelRunController: ModelRunController;
}

export interface SendPipeline {
  performModelRun(
    workspaceId: string,
    conversationId: string,
    request: ModelRunRequest,
    turnAnchor?: { id: string; startedAt: string },
    /** This run delivers the answer to a pending `ask_user`, so it resumes the turn that paused. */
    answersPendingQuestion?: boolean
  ): Promise<void>;
  /** Reattach surviving host runs, live or awaiting settlement, after startup or loading. */
  adoptResumableRuns(): Promise<void>;
  sendComposer(overrideText?: string): Promise<void>;
  /** Start a run without a new user message to deliver a settled task for the active
   * conversation. `true` means an existing or started run will consume it; `false`
   * means it cannot yet start because configuration or mutual exclusion blocks it. */
  wakeConversation(): Promise<boolean>;
  /** Start the first run of a conversation the host forked on the model's behalf. The
   * prompt is already persisted as the child's last user message, so this is a run over
   * persisted contexts anchored at that message. `false` means configuration or an
   * existing run blocked it; the child then simply waits for the user. */
  startForkedConversationRun(workspaceId: string, conversationId: string): Promise<boolean>;
  queueComposerMessage(
    workspaceId: string,
    conversationId: string,
    content: string,
    images: ImageAttachment[],
    clearComposer?: boolean
  ): void;
  deleteQueuedMessage(workspaceId: string, conversationId: string, messageId: string): void;
  steerQueuedMessage(conversationId: string, message: QueuedMessage): Promise<void>;
  addComposerImages(conversationId: string, files: File[]): Promise<void>;
  /** Drops a draft image and its `[Image #N]` placeholder together. */
  removeComposerImage(conversationId: string, imageId: string): void;
  dispatchNextQueuedMessage(workspaceId: string, conversationId: string): Promise<void>;
  retryFailedQueuedPromotion(workspaceId: string, conversationId: string, messageId: string): void;
}

export function createSendPipeline(
  stores: SendPipelineStores,
  host: () => SendPipelineHost
): SendPipeline {
  const { documentStore, composerController, modelRunController } = stores;

  /** Register a run in the controller using the shared shape for started and adopted runs. */
  const registerRunState = (
    workspaceId: string,
    conversationId: string,
    requestId: string,
    request: ModelRunRequest,
    startedAt: string
  ) => {
    modelRunController.update((current) => ({
      ...current,
      [conversationId]: {
        requestId,
        providerName: request.provider.name,
        modelName: request.model.id,
        workspaceId,
        request,
        startedAt,
        streamedTextByRound: {},
        streamedReasoningByRound: {},
        completedReasoningByRound: {},
        reasoningStartedAtByRound: {},
        reasoningDurationByRound: {},
        streamedToolsByRound: {},
        streamedHooksByRound: {},
        steeredInputsByRound: {},
        usageByRound: {},
        subagentUsageByCall: {},
        workflowProgressByCall: {},
        workflowRunIdByCall: {},
        usageRevision: 0
      }
    }));
  };

  /** Event source: started runs invoke `runModel`; adopted runs use the attach channel
   * and settlement slot. Both use the same lifecycle below. */
  type RunDriver = (
    handleEvent: (event: ModelStreamEvent) => void
  ) => Promise<Awaited<ReturnType<typeof runModel>>>;

  const runTurnLifecycle = async (
    workspaceId: string,
    conversationId: string,
    requestId: string,
    request: ModelRunRequest,
    drive: RunDriver
  ) => {
    // Each run gets its own coalescer. It commits batches at 10 FPS while retaining
    // every event, folds events individually in order, then performs effects in order.
    const coalescer = createModelStreamCoalescer((batch) => {
      const effects: ModelStreamEffect[] = [];
      modelRunController.update((current) => {
        const running = current[conversationId];
        if (!running || running.requestId !== requestId) return current;
        let next = running;
        for (const { event, at } of batch) {
          const reduction = reduceModelStreamEvent(next, event, at);
          next = reduction.run;
          effects.push(...reduction.effects);
        }
        // Trim once per batch: the budget only needs to hold before a commit, and
        // per-event trimming would turn one traversal into dozens.
        next = trimLiveContextBudget(next);
        return next === running ? current : { ...current, [conversationId]: next };
      });
      for (const effect of effects) {
        switch (effect.kind) {
          case "turn_split":
            host().splitConversationTurn(workspaceId, conversationId, effect.runBeforeInput, effect.item);
            break;
          case "steering_delivered":
            composerController.updateSteeringMessageIds((current) => {
              if (!current.has(effect.contextId)) return current;
              const next = new Set(current);
              next.delete(effect.contextId);
              return next;
            });
            break;
          case "turn_usage":
            host().updateRunningTurnUsage(conversationId, requestId, effect.usage, effect.revision);
            break;
          case "tool_completed":
            // A tab tool result is the roster itself, so the sidebar learns about the
            // Agent tabs from the same event that reports them instead of a separate poll.
            if (effect.success && effect.toolName) {
              host().reconcileAgentBrowserTabs(conversationId, effect.toolName, effect.output);
            }
            break;
          case "tool_context_settled":
            host().applySettledToolContext(workspaceId, conversationId, effect.context);
            break;
        }
      }
      // The host owns persistence: it writes each canonical context when produced
      // and throttles streaming prose into `streaming` rows. The renderer has no
      // write path and cannot overwrite end-of-turn output with a stale read model.
    });
    // The App stop path cannot see this buffer. Register its flusher so interrupted
    // persistence captures content visible when the user stops, not an older commit.
    modelRunController.setPendingEventFlusher(conversationId, () => coalescer.flush());

    const handleEvent = (event: ModelStreamEvent) => {
      if (modelRunController.runToken(conversationId) !== requestId) return;
      // The channel layer intercepts `run_concluded` for adopted runs; started runs
      // receive their result from the invoke promise, so ignore it with pings here.
      if (event.type === "ping" || event.type === "run_concluded") return;
      // Approval cards are raised by the tool executor, which does not know
      // the surrounding round, so they are handled before any run-state fold.
      if (event.type === "tool_approval_requested") {
        host().openToolPrompt(conversationId, {
          promptId: event.promptId,
          toolName: event.toolName,
          label: event.label,
          summary: event.summary,
          riskLevel: event.riskLevel,
          reason: event.reason,
          requester: event.requester,
          sourceAgent: event.sourceAgent,
          sourceCallId: event.sourceCallId,
          allowAlwaysOffered: event.allowAlwaysOffered,
          mandatory: event.mandatory
        });
        return;
      }
      if (event.type === "tool_approval_resolved") {
        host().closeToolPrompt(conversationId, event.promptId);
        return;
      }
      coalescer.push(event);
    };

    try {
      // Starting a turn used to release credential protection here, which silently answered the
      // takeover question on the user's behalf before it was ever asked. Control is now released
      // only by an explicit user action: the trusted handoff button, or the per-tab authorization
      // prompt the backend raises when a browser tool actually targets a signed-in user tab.
      const response = await drive(handleEvent);
      // The stream ended: commit the final buffered delta before reading run state.
      coalescer.flush();
      if (modelRunController.runToken(conversationId) !== requestId) return;
      const completedRun = modelRunController.current()[conversationId];
      const userCancelled = ["cancelled", "canceled", "stopped"].includes(response.stopReason ?? "");
      const finalizedResponseContexts = completedRun?.requestId === requestId
        ? backfillRequestedToolInput(response.contexts, completedRun)
        : response.contexts;
      const responseContexts = userCancelled && completedRun?.requestId === requestId
        ? mergeFinalizedInterruptedRunContexts(completedRun, finalizedResponseContexts)
        : finalizedResponseContexts;
      const persistedContexts = findConversation(
        documentStore.current(),
        workspaceId,
        conversationId
      ).conversation?.contexts ?? [];
      const completedContexts = mergeUniqueContexts(
        request.contexts,
        persistedContexts,
        responseContexts
      );
      modelRunController.update((current) => {
        const running = current[conversationId];
        if (!running || running.requestId !== requestId) return current;
        return {
          ...current,
          [conversationId]: {
            ...running,
            streamedTextByRound: {},
            streamedReasoningByRound: {},
            completedReasoningByRound: {},
            reasoningStartedAtByRound: {},
            reasoningDurationByRound: {},
            streamedToolsByRound: {},
            streamedHooksByRound: {},
            steeredInputsByRound: {}
          }
        };
      });
      host().updateConversation(workspaceId, conversationId, (conversation) => {
        // The host already wrote canonical contexts. Align the read model immediately
        // without waiting for IPC, so do not persist this projection.
        // Replace same-ID streamed contexts with final canonical contexts and discard
        // unmatched streamed residue. User contexts always survive even if absent
        // from settlement.
        const canonicalById = new Map(responseContexts.map((context) => [context.id, context] as const));
        const streamedRunContextIds = new Set(
          (completedRun?.requestId === requestId ? contextsFromInterruptedRun(completedRun) : [])
            .filter((context) => context.kind !== "user")
            .map((context) => context.id)
        );
        const supersededStreamPrefixes = runStreamContextPrefixes(requestId);
        const supersededByFinalRun = (id: string) => (
          !canonicalById.has(id)
          && (streamedRunContextIds.has(id) || supersededStreamPrefixes.some((prefix) => id.startsWith(prefix)))
        );
        const deliveredQueueIds = new Set(responseContexts.flatMap((context) => (
          context.kind === "user" ? [context.id] : []
        )));
        // Replace this run's document span with the complete canonical sequence at
        // the first matching context, preserving authoritative ordering.
        const belongsToRun = (id: string) => canonicalById.has(id) || supersededByFinalRun(id);
        const firstRunIndex = conversation.contexts.findIndex((context) => belongsToRun(context.id));
        const kept = conversation.contexts.filter((context) => !belongsToRun(context.id));
        const insertAt = firstRunIndex === -1
          ? kept.length
          : conversation.contexts
              .slice(0, firstRunIndex)
              .filter((context) => !belongsToRun(context.id)).length;
        const seenIds = new Set<string>();
        const settledRun = responseContexts.filter((context) => {
          if (seenIds.has(context.id)) return false;
          seenIds.add(context.id);
          return true;
        });
        return {
          ...conversation,
          contexts: [...kept.slice(0, insertAt), ...settledRun, ...kept.slice(insertAt)],
          queuedMessages: conversation.queuedMessages.filter(
            (message) => !deliveredQueueIds.has(message.id)
          ),
          updatedAt: new Date().toISOString()
        };
      }, { persist: false });
      // The host conversation store is authoritative: restore omitted host contexts
      // and remove local residue from the read model.
      host().refreshConversation(workspaceId, conversationId);
      const hookStopped = response.stopReason?.startsWith("hook_") === true;
      const awaitingUser = response.stopReason === "awaiting_user";
      let failure: { message: string; providerName: string; modelName: string } | undefined;
      if (response.stopReason === "error") {
        const t = host().t;
        const attempts = response.error?.attempts ?? 1;
        const detail = response.error?.message ?? t(
          "模型请求失败；可以检查上下文后重试。",
          "The model request failed. Check the context and try again."
        );
        failure = {
          message: attempts > 1
            ? t(
              "{detail}（已自动重试 {count} 次）",
              "{detail} (automatically retried {count} times)",
              { detail, count: attempts - 1 }
            )
            : detail,
          providerName: request.provider.name,
          modelName: request.model.id
        };
        // Why it stopped is recorded before that it stopped: settlement discards
        // a round that produced nothing, and for one that broke before its first
        // message this notice is the entire record of it.
        host().failConversationTurn(conversationId, requestId, failure);
      }
      if (awaitingUser) {
        host().pauseConversationTurnForUser(
          conversationId,
          requestId,
          completedContexts,
          { modelId: response.model, usage: response.usage, durationMs: response.durationMs }
        );
      } else {
        host().finishConversationTurns(
          conversationId,
          requestId,
          completedContexts,
          response.stopReason === "error" || hookStopped || userCancelled ? "interrupted" : "completed",
          { modelId: response.model, usage: response.usage, durationMs: response.durationMs }
        );
      }
      const fallbackContextTokens = estimateContextsTokens([...request.contexts, ...responseContexts]);
      const measuredContextTokens = response.contextTokens ?? fallbackContextTokens;
      host().setContextUsage(conversationId, {
        tokens: measuredContextTokens,
        estimated: response.contextTokens === undefined,
        providerId: request.provider.id,
        family: request.provider.family
      });
      if (failure) {
        host().setModelRunError(conversationId, {
          ...failure,
          retryable: true,
          requestId,
          workspaceId,
          request
        });
      }
    } catch (error) {
      // Flush buffered deltas on failure so an interrupted snapshot includes the
      // final content before the stream broke.
      coalescer.flush();
      if (modelRunController.runToken(conversationId) === requestId) {
        const t = host().t;
        const message = errorMessage(error, t("未知错误", "Unknown error"));
        // A first round that never connected produces no context at all, so the
        // turn is the only place the failure can be read — and a round with
        // neither output nor a notice is dropped when it settles, so the notice
        // has to land before the interrupted run is persisted.
        host().failConversationTurn(conversationId, requestId, {
          message,
          providerName: request.provider.name,
          modelName: request.model.id
        });
        const running = modelRunController.current()[conversationId];
        if (running?.requestId === requestId) {
          host().persistInterruptedRun(workspaceId, conversationId, running);
        }
        host().setModelRunError(conversationId, {
          message,
          retryable: true,
          requestId,
          providerName: request.provider.name,
          modelName: request.model.id,
          workspaceId,
          request
        });
      }
    } finally {
      coalescer.dispose();
      modelRunController.deletePendingEventFlusher(conversationId);
      const remainingQueuedIds = new Set(
        findConversation(documentStore.current(), workspaceId, conversationId)
          .conversation?.queuedMessages.map((message) => message.id) ?? []
      );
      composerController.updateSteeringMessageIds((current) => {
        if (![...current].some((id) => remainingQueuedIds.has(id))) return current;
        const next = new Set(current);
        for (const id of remainingQueuedIds) next.delete(id);
        return next;
      });
      if (modelRunController.runToken(conversationId) === requestId) {
        modelRunController.deleteRunToken(conversationId);
        modelRunController.update((current) => {
          const next = { ...current };
          delete next[conversationId];
          return next;
        });
      }
      // A run that ends without retracting its cards — a dropped channel, a
      // crashed worker — must not leave a card the user can still click. But a
      // background task's card is deliberately still alive host-side, so this
      // reconciles against the host's pending list instead of clearing.
      host().reconcileToolPrompts(conversationId);
      modelRunController.deletePerformingRun(conversationId);
    }
  };

  const performModelRun = async (
    workspaceId: string,
    conversationId: string,
    request: ModelRunRequest,
    turnAnchor?: { id: string; startedAt: string },
    answersPendingQuestion = false
  ) => {
    if (!host().requestFitsImageBudget(request.contexts)) return;
    if (host().contextMutationIsBlocked(conversationId)) return;
    host().clearPendingUndo(conversationId);
    host().closeEditorIfActive(conversationId);
    const requestId = createId("run");
    modelRunController.addPerformingRun(conversationId);
    modelRunController.setRunToken(conversationId, requestId);
    // A new attempt retires the previous failure from every surface it reached.
    host().clearModelRunError(conversationId, "failure");
    registerRunState(
      workspaceId,
      conversationId,
      requestId,
      request,
      turnAnchor?.startedAt ?? new Date().toISOString()
    );
    if (turnAnchor) {
      host().startConversationTurn(
        conversationId,
        requestId,
        turnAnchor.id,
        request.model.id,
        turnAnchor.startedAt,
        request.contexts
      );
    } else {
      // No new user message of its own: this run continues the round it follows,
      // or opens one anchored at the timeline tail when that round is finished.
      host().continueConversationTurn(
        conversationId,
        requestId,
        request.model.id,
        request.contexts,
        answersPendingQuestion ? "awaiting" : "continue"
      );
    }
    await runTurnLifecycle(workspaceId, conversationId, requestId, request, (handleEvent) => (
      runModel(request, handleEvent, requestId)
    ));
  };

  const settlementToResponse = (
    settlement: RunSettlementPayload | null | undefined,
    missingMessage: string
  ): Promise<Awaited<ReturnType<typeof runModel>>> => (
    settlement?.response
      ? Promise.resolve(settlement.response)
      : Promise.reject(new Error(settlement?.error ?? missingMessage))
  );

  /** Per-conversation adoption deduplication. Reject a second StrictMode or duplicate
   * load invocation before the first attach returns, or its channel replaces the subscription. */
  const adoptionsInFlight = new Set<string>();

  /** Adopt a surviving host run after reload. Attach replays buffered live events;
   * `run_concluded` and the settlement slot provide its result. Resolve after the
   * run is registered and advancing in the background, not after it finishes. */
  const adoptConversationRun = async (
    workspaceId: string,
    conversationId: string
  ): Promise<void> => {
    if (
      adoptionsInFlight.has(conversationId)
      || modelRunController.hasRunToken(conversationId)
      || modelRunController.hasPerformingRun(conversationId)
      || modelRunController.hasPreparingRun(conversationId)
    ) return;
    adoptionsInFlight.add(conversationId);
    try {
      const { t } = host();
      let deliver: ((event: ModelStreamEvent) => void) | undefined;
      let concludeSettlement: (() => void) | undefined;
      let concluded = false;
      const early: ModelStreamEvent[] = [];
      let attach: Awaited<ReturnType<typeof attachModelRun>>;
      try {
        attach = await attachModelRun(conversationId, (event) => {
          if (event.type === "run_concluded") {
            concluded = true;
            concludeSettlement?.();
            return;
          }
          if (deliver) deliver(event);
          else early.push(event);
        });
      } catch {
        return;
      }
      if (attach.status === "none") return;
      // If a local run started while awaiting attach, abandon adoption. The host
      // permits one run per conversation; this is local race defense.
      if (
        modelRunController.hasRunToken(conversationId)
        || modelRunController.hasPerformingRun(conversationId)
      ) return;
      const requestId = attach.requestId;
      const request = attach.request;
      modelRunController.addPerformingRun(conversationId);
      modelRunController.setRunToken(conversationId, requestId);
      // Reattaching is the same attempt continuing, so the turn keeps its
      // failure record until the settlement either replaces or clears it.
      host().clearModelRunError(conversationId, "notice");
      registerRunState(workspaceId, conversationId, requestId, request, new Date().toISOString());
      // Restore a same-generation turn marked interrupted during loading because
      // the host is still running it or awaiting its settlement.
      host().resumeAdoptedConversationTurn(conversationId, requestId, attach.request.contexts, attach.request.model.id);
      const missingMessage = t(
        "运行已结束，但结算结果不可用",
        "The run has ended, but its settlement is unavailable"
      );
      const outcome = attach.status === "finished"
        ? settlementToResponse(attach.settlement, missingMessage)
        : new Promise<Awaited<ReturnType<typeof runModel>>>((resolve, reject) => {
          concludeSettlement = () => {
            takeRunSettlement(conversationId)
              .then((settlement) => settlementToResponse(settlement, missingMessage))
              .then(resolve, reject);
          };
        });
      // If the marker arrived before `outcome` exists, trigger settlement now.
      if (attach.status === "running" && concluded) concludeSettlement?.();
      // The lifecycle consumes rejection. Attach an empty catch now to avoid an
      // unhandled-rejection window before it awaits `outcome`.
      void outcome.catch(() => {});
      void runTurnLifecycle(workspaceId, conversationId, requestId, request, (handleEvent) => {
        deliver = handleEvent;
        for (const event of early.splice(0)) handleEvent(event);
        return outcome;
      });
    } finally {
      adoptionsInFlight.delete(conversationId);
    }
  };

  /** Reattach every host run that is live or awaiting settlement after startup/load.
   * After resolution they are visible in the controller, so guards requiring no run
   * cannot race adoption. */
  const adoptResumableRuns = async (): Promise<void> => {
    // Propagate enumeration failure so the App adoption latch distinguishes completed
    // adoption from retryable failure. Otherwise wake and queue dispatch could proceed
    // without attaching any host runs.
    const rows = await listResumableRuns();
    const adoptions: Promise<void>[] = [];
    for (const row of rows) {
      const workspaceId = documentStore.current()?.workspaces.find((workspace) => (
        workspace.conversations.some((conversation) => conversation.id === row.conversationId)
      ))?.id;
      if (!workspaceId) {
        // The conversation no longer exists, so no UI can adopt this run. Stop it
        // and discard any settlement.
        void cancelConversationRun(row.conversationId).catch(() => {});
        void takeRunSettlement(row.conversationId).catch(() => {});
        continue;
      }
      adoptions.push(adoptConversationRun(workspaceId, row.conversationId));
    }
    await Promise.allSettled(adoptions);
  };

  const queueComposerMessage = (
    workspaceId: string,
    conversationId: string,
    content: string,
    images: ImageAttachment[],
    clearComposer = true
  ) => {
    const latest = documentStore.current();
    const located = findConversation(latest, workspaceId, conversationId);
    if (!latest || !located.conversation || (!content.trim() && !images.length)) return;
    if (Array.from(content.trim()).length > 100_000) return;
    if (located.conversation.queuedMessages.length >= 100) return;
    // Queued numbers must be unique against the transcript *and* the queue
    // ahead of this message; a second repair happens at delivery (steer goes
    // through the Rust mailbox renumber, promotion re-checks below).
    const queueReady = renumberImagesForSend(
      reserveQueuedMessageIds(
        imageShortIdsInUse(located.conversation.contexts),
        located.conversation.queuedMessages
      ),
      content.trim(),
      images
    );
    const message: QueuedMessage = {
      id: createId("queued"),
      content: queueReady.content,
      images: queueReady.images.length ? queueReady.images : undefined,
      createdAt: new Date().toISOString()
    };
    const next: AppDocument = {
      ...latest,
      workspaces: latest.workspaces.map((workspace) => workspace.id === workspaceId ? {
        ...workspace,
        conversations: workspace.conversations.map((conversation) => conversation.id === conversationId ? {
          ...conversation,
          queuedMessages: [...conversation.queuedMessages, message],
          updatedAt: message.createdAt
        } : conversation)
      } : workspace)
    };
    composerController.markQueuedMessageUnsaved(message.id);
    // A live steer can make this queued message part of an in-flight provider request. Do not
    // acknowledge that ordering barrier until the exact message (including image sidecar refs) is
    // durable across a process crash.
    const savePromise = host().persistDocumentImmediately(next, { durable: true });
    composerController.trackQueuedMessageSave(message.id, savePromise);
    // ComposerController's unsaved marker and title-bar save state handle failures;
    // prevent the rejection from becoming unhandled here.
    void savePromise.catch(() => {});
    if (clearComposer) {
      composerController.updateDrafts((current) => ({ ...current, [conversationId]: "" }));
      composerController.updateImageDrafts((current) => ({ ...current, [conversationId]: [] }));
    }
  };

  const deleteQueuedMessage = (
    workspaceId: string,
    conversationId: string,
    messageId: string
  ) => {
    if (composerController.current().steeringMessageIds.has(messageId)) return;
    const latest = documentStore.current();
    const located = findConversation(latest, workspaceId, conversationId);
    if (!latest || !located.conversation) return;
    const queuedMessages = located.conversation.queuedMessages.filter(
      (message) => message.id !== messageId
    );
    if (queuedMessages.length === located.conversation.queuedMessages.length) return;
    const next: AppDocument = {
      ...latest,
      workspaces: latest.workspaces.map((workspace) => workspace.id === workspaceId ? {
        ...workspace,
        conversations: workspace.conversations.map((conversation) => (
          conversation.id === conversationId ? {
            ...conversation,
            queuedMessages,
            updatedAt: new Date().toISOString()
          } : conversation
        ))
      } : workspace)
    };
    composerController.forgetQueuedMessages([messageId]);
    void host().persistDocumentImmediately(next).catch(() => {});
  };

  const steerQueuedMessage = async (conversationId: string, message: QueuedMessage) => {
    const runningAtClick = modelRunController.current()[conversationId];
    if (!runningAtClick || composerController.current().steeringMessageIds.has(message.id)) return;
    const pendingSave = composerController.queuedMessageSave(message.id);
    if (pendingSave) {
      try {
        await pendingSave;
      } catch {
        composerController.dropQueuedMessageSaveIfCurrent(message.id, pendingSave);
        return;
      }
    } else if (composerController.queuedMessageNeedsSave(message.id)) {
      try {
        await host().flushLatestDocument({ durable: true });
        composerController.clearQueuedMessageNeedsSave(message.id);
      } catch {
        // Do not steer before durable persistence. Leave the message queued for an
        // explicit retry.
        return;
      }
    }
    const running = modelRunController.current()[conversationId];
    const currentMessage = findConversation(
      documentStore.current(),
      runningAtClick.workspaceId,
      conversationId
    ).conversation?.queuedMessages.find((queued) => queued.id === message.id);
    if (
      !running
      || running.requestId !== runningAtClick.requestId
      || !currentMessage
      || composerController.current().steeringMessageIds.has(message.id)
    ) return;
    if (currentMessage.images?.length) {
      if (!supportsVision(running.request.model)) return;
      const pendingSteers = findConversation(
        documentStore.current(),
        running.workspaceId,
        conversationId
      ).conversation?.queuedMessages.flatMap((queued) => (
        composerController.current().steeringMessageIds.has(queued.id) && queued.id !== message.id
          ? [{
              id: queued.id,
              kind: "user" as const,
              content: queued.content,
              images: queued.images,
              createdAt: queued.createdAt
            }]
          : []
      )) ?? [];
      const steeredContext: UserContext = {
        id: currentMessage.id,
        kind: "user",
        content: currentMessage.content,
        images: currentMessage.images,
        createdAt: currentMessage.createdAt
      };
      const requestContexts = mergeUniqueContexts(
        running.request.contexts,
        contextsFromModelRun(running, true),
        pendingSteers,
        [steeredContext]
      );
      if (!host().requestFitsImageBudget(requestContexts)) return;
    }
    composerController.updateSteeringMessageIds((current) => new Set(current).add(currentMessage.id));
    try {
      await steerModelRun(running.requestId, currentMessage);
    } catch {
      // The turn already ended: retract the steering marker and keep the message queued.
      composerController.updateSteeringMessageIds((current) => {
        if (!current.has(currentMessage.id)) return current;
        const next = new Set(current);
        next.delete(currentMessage.id);
        return next;
      });
    }
  };

  const addComposerImages = (conversationId: string, files: File[]): Promise<void> => {
    if (!files.length) return Promise.resolve();
    return composerController.enqueueImageUpload(conversationId, async (uploadStillCurrent) => {
      const uploadTargetIsCurrent = () => (
        uploadStillCurrent()
        && Boolean(documentStore.current()?.workspaces.some((workspace) => (
          workspace.conversations.some((conversation) => conversation.id === conversationId)
        )))
      );
      if (!uploadTargetIsCurrent()) return;
      const latest = documentStore.current();
      const choice = latest ? modelChoiceForConversation(latest) : undefined;
      const provider = choice?.provider;
      const model = choice?.model;
      if (!provider?.enabled || !model || !model.id.trim()) return;
      if (!supportsVision(model)) return;
      const existingImages = composerController.current().imageDrafts[conversationId] ?? [];
      let selectedBytes = existingImages.reduce((total, image) => total + image.bytes, 0);
      const acceptedFiles: File[] = [];
      for (const file of files) {
        if (
          file.size <= 0
          || file.size > MAX_IMAGE_ATTACHMENT_BYTES
          || existingImages.length + acceptedFiles.length >= MAX_COMPOSER_IMAGES
          || selectedBytes + file.size > MAX_COMPOSER_IMAGE_BYTES
        ) {
          continue;
        }
        acceptedFiles.push(file);
        selectedBytes += file.size;
      }
      if (!acceptedFiles.length) return;
      const results = await Promise.allSettled(acceptedFiles.map(async (file) => (
        prepareImageAttachment(file.name, new Uint8Array(await file.arrayBuffer()))
      )));
      if (!uploadTargetIsCurrent()) return;
      const uploaded = results.flatMap((result) => result.status === "fulfilled" ? [result.value] : []);
      const assignedShortIds: number[] = [];
      if (uploaded.length) {
        composerController.updateImageDrafts((current) => {
          const existing = current[conversationId] ?? [];
          const known = new Set(existing.map((image) => image.id));
          let selectedPixels = existing.reduce(
            (total, image) => total + image.width * image.height,
            0
          );
          const unique = uploaded.filter((image) => {
            if (known.has(image.id)) return false;
            const pixels = image.width * image.height;
            if (
              !Number.isSafeInteger(pixels)
              || pixels <= 0
              || pixels > MAX_IMAGE_ATTACHMENT_PIXELS
              || selectedPixels + pixels > MAX_COMPOSER_IMAGE_PIXELS
            ) {
              return false;
            }
            known.add(image.id);
            selectedPixels += pixels;
            return true;
          });
          if (!unique.length) return current;
          // Number the accepted images the way Claude Code does: the composer
          // shows the same `[Image #N]` the model will see. Numbers already
          // taken anywhere visible — transcript, queued messages, this draft's
          // images and its literal placeholder text — are never reissued.
          const uploadDocument = documentStore.current();
          const conversation = uploadDocument?.workspaces
            .flatMap((workspace) => workspace.conversations)
            .find((candidate) => candidate.id === conversationId);
          const taken = reserveQueuedMessageIds(
            imageShortIdsInUse(conversation?.contexts ?? []),
            conversation?.queuedMessages ?? []
          );
          for (const image of existing) {
            if (image.shortId !== undefined) taken.add(image.shortId);
          }
          for (const id of imagePlaceholderIds(composerController.current().drafts[conversationId] ?? "")) {
            taken.add(id);
          }
          const numbered = unique.map((image) => {
            const shortId = nextImageShortId(taken);
            taken.add(shortId);
            assignedShortIds.push(shortId);
            return { ...image, shortId };
          });
          return { ...current, [conversationId]: [...existing, ...numbered] };
        });
      }
      if (assignedShortIds.length) {
        composerController.updateDrafts((current) => {
          let text = current[conversationId] ?? "";
          for (const shortId of assignedShortIds) text = appendImagePlaceholder(text, shortId);
          return { ...current, [conversationId]: text };
        });
      }
    });
  };

  const removeComposerImage = (conversationId: string, imageId: string) => {
    let removedShortId: number | undefined;
    composerController.updateImageDrafts((current) => {
      const existing = current[conversationId] ?? [];
      const removed = existing.find((image) => image.id === imageId);
      if (!removed) return current;
      removedShortId = removed.shortId;
      return { ...current, [conversationId]: existing.filter((image) => image.id !== imageId) };
    });
    // The placeholder goes with the image; the reverse is deliberately not
    // true — deleting placeholder text in the draft never drops the image.
    if (removedShortId !== undefined) {
      const shortId = removedShortId;
      composerController.updateDrafts((current) => ({
        ...current,
        [conversationId]: stripImagePlaceholder(current[conversationId] ?? "", shortId)
      }));
    }
  };

  /** Confirm the host accepted the prose append; otherwise replay it once on top of
   * authoritative prose.
   *
   * `update_conversation` silently preserves host prose when the render read model
   * differs item-by-item from the store, then writes authoritative prose back to the
   * read model. Presence confirms acceptance only if no explicit write refusal
   * remains: reloading authority after refusal is best-effort and may fail. */
  const commitUserTurn = async (
    workspaceId: string,
    conversationId: string,
    userContext: UserContext,
    /** A title rule, not a title value: replay operates on authoritative prose whose
     * title may differ from the one observed when this send started. */
    titleFor: (conversation: Conversation) => string
  ): Promise<Conversation | null> => {
    const committed = (): Conversation | null => {
      if (host().conversationWriteFailure(conversationId)) return null;
      const located = findConversation(documentStore.current(), workspaceId, conversationId);
      return located.conversation?.contexts.some((context) => context.id === userContext.id)
        ? located.conversation
        : null;
    };
    const accepted = committed();
    if (accepted) return accepted;
    if (host().conversationWriteFailure(conversationId)) return null;
    const latest = documentStore.current();
    const located = findConversation(latest, workspaceId, conversationId);
    if (!latest || !located.conversation) return null;
    const retried: Conversation = {
      ...located.conversation,
      title: titleFor(located.conversation),
      contexts: [...located.conversation.contexts, userContext],
      updatedAt: userContext.createdAt
    };
    const retriedDocument: AppDocument = {
      ...latest,
      workspaces: latest.workspaces.map((workspace) => workspace.id === workspaceId ? {
        ...workspace,
        conversations: workspace.conversations.map((conversation) => (
          conversation.id === conversationId ? retried : conversation
        ))
      } : workspace)
    };
    try {
      await host().persistDocumentImmediately(retriedDocument, { durable: true });
    } catch {
      return null;
    }
    return committed();
  };

  const sendComposer = async (overrideText?: string) => {
    const { t } = host();
    const document = documentStore.current();
    const workspaceId = host().activeWorkspaceId();
    const conversationId = host().activeConversationId();
    // Snapshot the tool catalog at send time so an active-conversation switch during
    // a durable wait cannot mix another conversation's tools into this request.
    const enabledToolsAtSend = [...host().activeEnabledTools()];
    const conversationToolsAtSend = host().activeConversationTools();
    const active = findConversation(document, workspaceId, conversationId);
    if (!document || !workspaceId || !conversationId || !active.workspace || !active.conversation) return;
    const activeWorkspace = active.workspace;
    const activeConversation = active.conversation;
    const draft = (overrideText ?? composerController.current().drafts[conversationId])?.trim() ?? "";
    const images = overrideText === undefined
      ? composerController.current().imageDrafts[conversationId] ?? []
      : [];
    const composerChoice = modelChoiceForConversation(document);
    const provider = composerChoice.provider;
    const model = composerChoice.model;
    const requestContainsImages = images.length > 0 || contextsContainProjectedImages(activeConversation.contexts);
    if (overrideText === undefined && composerController.current().imageLoadingIds.has(conversationId)) return;
    if (!provider?.enabled) {
      host().openProviderSettings();
      return;
    }
    if (!hasUsableBaseUrl(provider)) {
      host().openProviderSettings();
      return;
    }
    if (!model || !model.id.trim()) {
      host().openProviderSettings();
      return;
    }
    if (requestContainsImages && !supportsVision(model)) return;
    // Queuing means waiting for a busy model. An answer to a pending question must
    // bypass queued messages: the model is paused for that answer, and an older
    // queued message would otherwise consume the question and invalidate the answer.
    const answeringPendingQuestion = overrideText !== undefined;
    const queueIsBlocking = Boolean(modelRunController.current()[conversationId])
      || (!answeringPendingQuestion && activeConversation.queuedMessages.length > 0);
    if ((draft || images.length) && queueIsBlocking) {
      queueComposerMessage(
        workspaceId,
        conversationId,
        draft,
        images,
        overrideText === undefined
      );
      return;
    }
    if (!answeringPendingQuestion && activeConversation.queuedMessages.length) return;
    if (host().contextMutationIsBlocked(conversationId)) return;

    // Draft numbers were allocated when the images were added; the transcript
    // may have grown since (tool screenshots take numbers too), so repair any
    // collision at the send boundary — the last point before numbers freeze.
    const sendReady = renumberImagesForSend(
      imageShortIdsInUse(activeConversation.contexts),
      draft,
      images
    );
    const pendingUserContext: UserContext | undefined = draft || images.length ? {
      id: createId("ctx"),
      kind: "user",
      content: sendReady.content,
      images: sendReady.images.length ? sendReady.images : undefined,
      createdAt: new Date().toISOString()
    } : undefined;
    let requestContexts = pendingUserContext
      ? [...activeConversation.contexts, pendingUserContext]
      : [...activeConversation.contexts];
    if (!host().requestFitsImageBudget(requestContexts)) return;

    {
      modelRunController.addPreparingRun(conversationId);
      let launchConversation = activeConversation;
      try {
        if (pendingUserContext) {
          const latest = documentStore.current();
          const located = findConversation(latest, workspaceId, conversationId);
          if (!latest || !located.conversation) return;
          launchConversation = located.conversation;
          requestContexts = [...located.conversation.contexts, pendingUserContext];
          if (!host().requestFitsImageBudget(requestContexts)) return;
          const titleFor = (conversation: Conversation) => (
            conversation.title === "新任务" || conversation.title === "New task" // i18n-audit-ignore: recognizes both localized default-title markers
              ? (textWithoutImagePlaceholders(sendReady.content).slice(0, 32) || sendReady.images[0]?.name || t("图片", "Image"))
              : conversation.title
          );
          const nextTitle = titleFor(located.conversation);
          const nextConversation: Conversation = {
            ...located.conversation,
            title: nextTitle,
            contexts: requestContexts,
            updatedAt: pendingUserContext.createdAt
          };
          const nextDocument: AppDocument = {
            ...latest,
            workspaces: latest.workspaces.map((workspace) => workspace.id === workspaceId ? {
              ...workspace,
              conversations: workspace.conversations.map((conversation) => (
                conversation.id === conversationId ? nextConversation : conversation
              ))
            } : workspace)
          };
          try {
            // The user turn and provider snapshot must be durable before Rust is
            // allowed to construct or send the network request.
            await host().persistDocumentImmediately(nextDocument, { durable: true });
            launchConversation = nextConversation;
            // The host is the sole prose writer. Its `expected_context_ids` gate
            // silently retains host prose when the read model is stale, so a durable
            // write may succeed without storing this user turn. Confirm it reached
            // the store before running; otherwise the model would receive history
            // missing the message, especially fatal for an `ask_user` answer.
            const committed = await commitUserTurn(
              workspaceId,
              conversationId,
              pendingUserContext,
              titleFor
            );
            if (!committed) {
              const refusal = host().conversationWriteFailure(conversationId);
              host().setModelRunError(conversationId, {
                message: t(
                  "这条消息没能写进对话库，已取消发送——请重新发送，不要让模型收到一份缺了它的历史。",
                  "This message could not be written to the conversation store, so the request was not sent. Send it again rather than letting the model receive a history without it."
                ) + (refusal ? t("（宿主拒收原因：{reason}）", " Host refused it: {reason}", { reason: refusal }) : ""),
                retryable: false,
                providerName: provider.name,
                modelName: model.id,
                workspaceId,
                request: {
                  provider,
                  model,
                  reasoningEffort: nextConversation.settings.reasoningEffort,
                  conversationId,
                  workspacePath: activeWorkspace.path,
                  systemPrompt: nextConversation.settings.systemPrompt,
                  enabledTools: enabledToolsAtSend,
                  contexts: requestContexts,
                  tools: conversationToolsAtSend
                }
              });
              return;
            }
            launchConversation = committed;
            requestContexts = [...committed.contexts];
            if (!host().requestFitsImageBudget(requestContexts)) return;
          } catch {
            const optimistic = documentStore.current();
            const optimisticLocated = findConversation(optimistic, workspaceId, conversationId);
            if (
              optimistic
              && optimisticLocated.conversation?.contexts.some(
                (context) => context.id === pendingUserContext.id
              )
            ) {
              const promotionStillOwnsScalarFields = (
                optimisticLocated.conversation === nextConversation
              );
              const rolledBackConversation: Conversation = {
                ...optimisticLocated.conversation,
                title: promotionStillOwnsScalarFields
                  ? located.conversation.title
                  : optimisticLocated.conversation.title,
                contexts: optimisticLocated.conversation.contexts.filter(
                  (context) => context.id !== pendingUserContext.id
                ),
                updatedAt: promotionStillOwnsScalarFields
                  ? located.conversation.updatedAt
                  : optimisticLocated.conversation.updatedAt
              };
              const rolledBack: AppDocument = {
                ...optimistic,
                workspaces: optimistic.workspaces.map((workspace) => workspace.id === workspaceId ? {
                  ...workspace,
                  conversations: workspace.conversations.map((conversation) => (
                    conversation.id === conversationId ? rolledBackConversation : conversation
                  ))
                } : workspace)
              };
              try {
                // The durable command reports failure only before commit or
                // when the disk barrier itself failed. Persist the inverse
                // transition too, so a restart cannot reveal an unsent turn.
                await host().persistDocumentImmediately(rolledBack, { durable: true });
              } catch {
                // If rollback cannot persist, the optimistic state remains rolled
                // back and the store continues retrying.
              }
            }
            return;
          }
        } else {
          // Persist the provider/model snapshot before invoking Rust, where network destinations are
          // checked against the trusted document to prevent credential redirection from the WebView.
          await host().flushLatestDocument({ durable: true });
        }
      } catch {
        // Do not start a request until API configuration is durably persisted.
        return;
      } finally {
        modelRunController.deletePreparingRun(conversationId);
      }

      let turnAnchor: { id: string; startedAt: string } | undefined;
      if (pendingUserContext) {
        if (overrideText === undefined) {
          turnAnchor = { id: pendingUserContext.id, startedAt: pendingUserContext.createdAt };
        }
      }
      if (overrideText === undefined) {
        composerController.updateDrafts((current) => ({ ...current, [conversationId]: "" }));
        composerController.updateImageDrafts((current) => ({ ...current, [conversationId]: [] }));
      }
      host().scrollTimelineToBottom();
      await performModelRun(workspaceId, conversationId, {
        provider,
        model,
        reasoningEffort: launchConversation.settings.reasoningEffort,
        conversationId,
        workspacePath: activeWorkspace.path,
        systemPrompt: launchConversation.settings.systemPrompt,
        enabledTools: enabledToolsAtSend,
        contexts: requestContexts,
        tools: conversationToolsAtSend
      }, turnAnchor, overrideText !== undefined);
    }
  };

  /** Wake the active conversation with no new user message when a settled task can
   * be delivered. An existing run or queue consumes it at its turn boundary; a wake
   * never impersonates an answer or revives an awaiting turn. Return `false` when
   * configuration or mutual exclusion prevents a wake so a later signal may retry. */
  const wakeConversation = async (): Promise<boolean> => {
    const document = documentStore.current();
    const workspaceId = host().activeWorkspaceId();
    const conversationId = host().activeConversationId();
    const enabledToolsAtSend = [...host().activeEnabledTools()];
    const conversationToolsAtSend = host().activeConversationTools();
    const active = findConversation(document, workspaceId, conversationId);
    if (!document || !workspaceId || !conversationId || !active.workspace || !active.conversation) return false;
    const activeWorkspace = active.workspace;
    const activeConversation = active.conversation;
    if (
      modelRunController.current()[conversationId]
      || modelRunController.hasRunToken(conversationId)
      || modelRunController.hasPerformingRun(conversationId)
      || activeConversation.queuedMessages.length
    ) return true;
    // A concurrent wake or send that is merely preparing is not consumption: it may
    // fail during persistence. Preserve wake accounting until a run actually appears,
    // then consume it when the wake effect reruns.
    if (modelRunController.hasPreparingRun(conversationId)) return false;
    if (host().contextMutationIsBlocked(conversationId)) return false;
    const wakeChoice = modelChoiceForConversation(document);
    const provider = wakeChoice.provider;
    const model = wakeChoice.model;
    if (!provider?.enabled || !hasUsableBaseUrl(provider) || !model || !model.id.trim()) return false;
    const requestContexts = [...activeConversation.contexts];
    if (!host().requestFitsImageBudget(requestContexts)) return false;
    modelRunController.addPreparingRun(conversationId);
    try {
      // As on the send path, persist the provider snapshot before issuing a network request.
      await host().flushLatestDocument({ durable: true });
    } catch {
      return false;
    } finally {
      modelRunController.deletePreparingRun(conversationId);
    }
    await performModelRun(workspaceId, conversationId, {
      provider,
      model,
      reasoningEffort: activeConversation.settings.reasoningEffort,
      conversationId,
      workspacePath: activeWorkspace.path,
      systemPrompt: activeConversation.settings.systemPrompt,
      enabledTools: enabledToolsAtSend,
      contexts: requestContexts,
      tools: conversationToolsAtSend
    });
    return true;
  };

  const startForkedConversationRun = async (
    workspaceId: string,
    conversationId: string
  ): Promise<boolean> => {
    const document = documentStore.current();
    const located = findConversation(document, workspaceId, conversationId);
    if (!document || !located.workspace || !located.conversation) return false;
    const conversation = located.conversation;
    if (
      modelRunController.current()[conversationId]
      || modelRunController.hasRunToken(conversationId)
      || modelRunController.hasPerformingRun(conversationId)
      || modelRunController.hasPreparingRun(conversationId)
    ) return false;
    if (host().contextMutationIsBlocked(conversationId)) return false;
    const choice = modelChoiceForConversation(document);
    const provider = choice.provider;
    const model = choice.model;
    if (!provider?.enabled || !hasUsableBaseUrl(provider) || !model || !model.id.trim()) return false;
    // The host appended the prompt as the last user context; without one there is
    // nothing to answer and the run would open an empty round.
    const anchor = [...conversation.contexts].reverse().find((context) => context.kind === "user");
    if (!anchor) return false;
    const requestContexts = [...conversation.contexts];
    if (!host().requestFitsImageBudget(requestContexts)) return false;
    // The child is not the active conversation, so its tool set comes from its own
    // settings rather than the active-conversation seams. The catalog itself is
    // document-wide; the host re-derives the enabled list from the store anyway.
    const tools = host().activeConversationTools();
    const available = new Set(tools.map((tool) => tool.name));
    const enabledTools = conversation.settings.enabledTools.filter((name) => available.has(name));
    modelRunController.addPreparingRun(conversationId);
    try {
      await host().flushLatestDocument({ durable: true });
    } catch {
      return false;
    } finally {
      modelRunController.deletePreparingRun(conversationId);
    }
    await performModelRun(workspaceId, conversationId, {
      provider,
      model,
      reasoningEffort: conversation.settings.reasoningEffort,
      conversationId,
      workspacePath: located.workspace.path,
      systemPrompt: conversation.settings.systemPrompt,
      enabledTools,
      contexts: requestContexts,
      tools,
      forkPromptContextId: anchor.id
    }, { id: anchor.id, startedAt: new Date().toISOString() });
    return true;
  };

  const dispatchNextQueuedMessage = async (workspaceId: string, conversationId: string) => {
    const { t } = host();
    // Snapshot the tool catalog at dispatch time so an active-conversation switch
    // during flush or persistence cannot contaminate this request.
    const conversationToolsAtDispatch = host().activeConversationTools();
    if (
      modelRunController.hasRunToken(conversationId)
      || modelRunController.hasPerformingRun(conversationId)
      || modelRunController.hasPreparingRun(conversationId)
    ) return;
    const latest = documentStore.current();
    const located = findConversation(latest, workspaceId, conversationId);
    const message = located.conversation?.queuedMessages[0];
    if (!latest || !located.workspace || !located.conversation || !message) return;
    if (composerController.current().failedQueuedPromotionIds.has(message.id)) return;
    const queuedChoice = modelChoiceForConversation(latest);
    const provider = queuedChoice.provider;
    const model = queuedChoice.model;
    if (
      !provider?.enabled
      || !hasUsableBaseUrl(provider)
      || !model
      || !model.id.trim()
    ) return;
    if (
      !supportsVision(model)
      && (Boolean(message.images?.length) || contextsContainProjectedImages(located.conversation.contexts))
    ) return;
    try {
      modelRunController.addPreparingRun(conversationId);
      let queuedConversation: Conversation;
      let requestContexts: ContextItem[];
      try {
        await host().flushLatestDocument();
        const current = documentStore.current();
        const currentLocated = findConversation(current, workspaceId, conversationId);
        const currentMessage = currentLocated.conversation?.queuedMessages[0];
        if (
          !current
          || !currentLocated.conversation
          || !currentMessage
          || currentMessage.id !== message.id
        ) return;
        // The transcript may have grown while this message sat in the queue;
        // repair number collisions now, against both the contexts and the
        // messages still queued behind this one.
        const promoted = renumberImagesForSend(
          reserveQueuedMessageIds(
            imageShortIdsInUse(currentLocated.conversation.contexts),
            currentLocated.conversation.queuedMessages.filter(
              (candidate) => candidate.id !== currentMessage.id
            )
          ),
          currentMessage.content,
          currentMessage.images ?? []
        );
        const userContext: UserContext = {
          id: currentMessage.id,
          kind: "user",
          content: promoted.content,
          images: promoted.images.length ? promoted.images : undefined,
          createdAt: currentMessage.createdAt
        };
        requestContexts = [...currentLocated.conversation.contexts, userContext];
        if (!host().requestFitsImageBudget(requestContexts)) return;
        queuedConversation = {
          ...currentLocated.conversation,
          title: currentLocated.conversation.title === "新任务" || currentLocated.conversation.title === "New task" // i18n-audit-ignore: recognizes both localized default-title markers
            ? (textWithoutImagePlaceholders(currentMessage.content).slice(0, 32) || currentMessage.images?.[0]?.name || t("图片", "Image"))
            : currentLocated.conversation.title,
          contexts: requestContexts,
          queuedMessages: currentLocated.conversation.queuedMessages.filter(
            (candidate) => candidate.id !== currentMessage.id
          ),
          updatedAt: new Date().toISOString()
        };
        const next: AppDocument = {
          ...current,
          workspaces: current.workspaces.map((workspace) => workspace.id === workspaceId ? {
            ...workspace,
            conversations: workspace.conversations.map((conversation) => (
              conversation.id === conversationId ? queuedConversation : conversation
            ))
          } : workspace)
        };
        try {
          // This promotion is transactional: if the durable write fails, the
          // queued message must remain a queued intent rather than becoming an
          // unsent user turn. Suppress the generic optimistic echo and perform
          // an ID-scoped rollback below so concurrent edits are preserved.
          await host().persistDocumentImmediately(next, { durable: true });
          // The host may silently preserve its prose when read-model and store
          // contexts differ. Treat an unpromoted queued message as a write failure
          // and use the same ID-scoped rollback, rather than send history without it.
          const promoted = findConversation(documentStore.current(), workspaceId, conversationId)
            .conversation?.contexts.some((context) => context.id === userContext.id);
          if (!promoted) throw new Error("对话库未接受这次排队消息的提升"); // i18n-audit-ignore: internal control flow, never shown in the UI
        } catch (error) {
          // A promptly rejected save can be rolled back before React consumes
          // the skip marker. Do not retain the abandoned snapshot indefinitely.
          composerController.updateFailedQueuedPromotionIds((current) => {
            if (current.has(currentMessage.id)) return current;
            return new Set(current).add(currentMessage.id);
          });
          const optimistic = documentStore.current();
          const optimisticLocated = findConversation(optimistic, workspaceId, conversationId);
          if (optimistic && optimisticLocated.conversation) {
            const currentOptimisticConversation = optimisticLocated.conversation;
            const promotionStillOwnsScalarFields = (
              currentOptimisticConversation === queuedConversation
            );
            const rolledBackConversation: Conversation = {
              ...currentOptimisticConversation,
              title: promotionStillOwnsScalarFields
                ? currentLocated.conversation.title
                : currentOptimisticConversation.title,
              contexts: currentOptimisticConversation.contexts.filter(
                (context) => context.id !== currentMessage.id
              ),
              queuedMessages: [
                currentMessage,
                ...currentOptimisticConversation.queuedMessages.filter(
                  (candidate) => candidate.id !== currentMessage.id
                )
              ],
              updatedAt: promotionStillOwnsScalarFields
                ? currentLocated.conversation.updatedAt
                : currentOptimisticConversation.updatedAt
            };
            const rolledBack: AppDocument = {
              ...optimistic,
              workspaces: optimistic.workspaces.map((workspace) => workspace.id === workspaceId ? {
                ...workspace,
                conversations: workspace.conversations.map((conversation) => (
                  conversation.id === conversationId ? rolledBackConversation : conversation
                ))
              } : workspace)
            };
            try {
              // A save rejection can still mean Rust installed the promotion
              // before reporting an older writer failure. Publish and durably
              // acknowledge the inverse transition before exposing retry.
              await host().persistDocumentImmediately(rolledBack, { durable: true });
            } catch {
              // Permit one ordinary debounced retry of the safe queued state.
              // The failed-message latch below still prevents auto promotion.
            }
          }
          throw error;
        }
      } finally {
        modelRunController.deletePreparingRun(conversationId);
      }
      await performModelRun(workspaceId, conversationId, {
        provider,
        model,
        reasoningEffort: queuedConversation.settings.reasoningEffort,
        conversationId,
        workspacePath: located.workspace.path,
        systemPrompt: queuedConversation.settings.systemPrompt,
        enabledTools: queuedConversation.settings.enabledTools.filter((name) => (
          conversationToolsAtDispatch.some((tool) => tool.name === name)
        )),
        contexts: requestContexts,
        tools: conversationToolsAtDispatch
      }, { id: message.id, startedAt: new Date().toISOString() });
    } catch {
      // Promotion failure already completed the ID-scoped rollback. Keep the message
      // queued and require an explicit retry.
    } finally {
      modelRunController.deletePreparingRun(conversationId);
    }
  };

  const retryFailedQueuedPromotion = (
    workspaceId: string,
    conversationId: string,
    messageId: string
  ) => {
    const queuedMessageStillExists = findConversation(
      documentStore.current(),
      workspaceId,
      conversationId
    ).conversation?.queuedMessages.some((message) => message.id === messageId);
    composerController.updateFailedQueuedPromotionIds((current) => {
      if (!current.has(messageId)) return current;
      const nextIds = new Set(current);
      nextIds.delete(messageId);
      return nextIds;
    });
    if (queuedMessageStillExists) {
      void dispatchNextQueuedMessage(workspaceId, conversationId);
    }
  };

  return {
    performModelRun,
    adoptResumableRuns,
    sendComposer,
    wakeConversation,
    startForkedConversationRun,
    queueComposerMessage,
    deleteQueuedMessage,
    steerQueuedMessage,
    addComposerImages,
    removeComposerImage,
    dispatchNextQueuedMessage,
    retryFailedQueuedPromotion
  };
}
