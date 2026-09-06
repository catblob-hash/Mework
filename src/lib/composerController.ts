import type { ImageAttachment } from "../types";

export interface ComposerControllerState {
  /** Composer text drafts per conversation. */
  drafts: Record<string, string>;
  /** Prepared image attachments per conversation. */
  imageDrafts: Record<string, ImageAttachment[]>;
  /** Conversations with an image upload batch still in flight. */
  imageLoadingIds: Set<string>;
  /** Queued messages currently being steered into the running turn. */
  steeringMessageIds: Set<string>;
  /** Queued messages whose promotion to a run failed and needs manual retry. */
  failedQueuedPromotionIds: Set<string>;
}

export interface ComposerController {
  subscribe(listener: () => void): () => void;
  current(): ComposerControllerState;
  updateDrafts(
    updater: (current: Record<string, string>) => Record<string, string>
  ): void;
  updateImageDrafts(
    updater: (current: Record<string, ImageAttachment[]>) => Record<string, ImageAttachment[]>
  ): void;
  updateSteeringMessageIds(updater: (current: Set<string>) => Set<string>): void;
  updateFailedQueuedPromotionIds(updater: (current: Set<string>) => Set<string>): void;
  /**
   * Serializes image upload batches per conversation and keeps the loading
   * flag up while any batch is pending. `uploadStillCurrent` reports whether
   * the conversation's uploads were invalidated after this batch was queued.
   */
  enqueueImageUpload(
    conversationId: string,
    run: (uploadStillCurrent: () => boolean) => Promise<void>
  ): Promise<void>;
  /** Invalidates in-flight uploads and drops image drafts for the conversations. */
  invalidateImages(conversationIds: Iterable<string>): void;
  /**
   * Queued-message save barrier: a live steer can make a queued message part
   * of an in-flight provider request, so the exact message must be durable
   * before the steer acknowledges it.
   */
  markQueuedMessageUnsaved(messageId: string): void;
  trackQueuedMessageSave(messageId: string, save: Promise<void>): void;
  queuedMessageSave(messageId: string): Promise<void> | undefined;
  dropQueuedMessageSaveIfCurrent(messageId: string, save: Promise<void>): void;
  queuedMessageNeedsSave(messageId: string): boolean;
  clearQueuedMessageNeedsSave(messageId: string): void;
  /** Drops every save/needs-save/failed-promotion record for removed messages. */
  forgetQueuedMessages(messageIds: Iterable<string>): void;
}

export function createComposerController(): ComposerController {
  let state: ComposerControllerState = {
    drafts: {},
    imageDrafts: {},
    imageLoadingIds: new Set(),
    steeringMessageIds: new Set(),
    failedQueuedPromotionIds: new Set()
  };
  const listeners = new Set<() => void>();
  const uploadQueues = new Map<string, Promise<void>>();
  const uploadGenerations = new Map<string, number>();
  const queuedSavePromises = new Map<string, Promise<void>>();
  const queuedNeedsSave = new Set<string>();

  const notify = () => {
    for (const listener of [...listeners]) listener();
  };

  const commit = (next: Partial<ComposerControllerState>) => {
    state = { ...state, ...next };
    notify();
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return state;
    },
    updateDrafts(updater) {
      commit({ drafts: updater(state.drafts) });
    },
    updateImageDrafts(updater) {
      commit({ imageDrafts: updater(state.imageDrafts) });
    },
    updateSteeringMessageIds(updater) {
      commit({ steeringMessageIds: updater(state.steeringMessageIds) });
    },
    updateFailedQueuedPromotionIds(updater) {
      commit({ failedQueuedPromotionIds: updater(state.failedQueuedPromotionIds) });
    },
    enqueueImageUpload(conversationId, run) {
      const generation = uploadGenerations.get(conversationId) ?? 0;
      const uploadStillCurrent = () => (
        (uploadGenerations.get(conversationId) ?? 0) === generation
      );
      const previous = uploadQueues.get(conversationId) ?? Promise.resolve();
      const queued = previous.catch(() => undefined).then(() => run(uploadStillCurrent));
      uploadQueues.set(conversationId, queued);
      if (!state.imageLoadingIds.has(conversationId)) {
        commit({ imageLoadingIds: new Set(state.imageLoadingIds).add(conversationId) });
      }
      const finish = () => {
        if (uploadQueues.get(conversationId) !== queued) return;
        uploadQueues.delete(conversationId);
        const next = new Set(state.imageLoadingIds);
        next.delete(conversationId);
        commit({ imageLoadingIds: next });
      };
      void queued.then(finish, finish);
      return queued;
    },
    invalidateImages(conversationIds) {
      const removed = new Set(conversationIds);
      if (!removed.size) return;
      for (const conversationId of removed) {
        uploadGenerations.set(
          conversationId,
          (uploadGenerations.get(conversationId) ?? 0) + 1
        );
        uploadQueues.delete(conversationId);
      }
      commit({
        imageDrafts: Object.fromEntries(
          Object.entries(state.imageDrafts).filter(([conversationId]) => !removed.has(conversationId))
        ),
        imageLoadingIds: new Set(
          [...state.imageLoadingIds].filter((conversationId) => !removed.has(conversationId))
        )
      });
    },
    markQueuedMessageUnsaved(messageId) {
      queuedNeedsSave.add(messageId);
    },
    trackQueuedMessageSave(messageId, save) {
      queuedSavePromises.set(messageId, save);
      save.then(() => {
        if (queuedSavePromises.get(messageId) !== save) return;
        queuedSavePromises.delete(messageId);
        queuedNeedsSave.delete(messageId);
      }, () => {
        // A failed save stays tracked: the steer barrier awaits it, observes
        // the rejection, and decides whether to drop or retry.
      });
    },
    queuedMessageSave(messageId) {
      return queuedSavePromises.get(messageId);
    },
    dropQueuedMessageSaveIfCurrent(messageId, save) {
      if (queuedSavePromises.get(messageId) === save) {
        queuedSavePromises.delete(messageId);
      }
    },
    queuedMessageNeedsSave(messageId) {
      return queuedNeedsSave.has(messageId);
    },
    clearQueuedMessageNeedsSave(messageId) {
      queuedNeedsSave.delete(messageId);
    },
    forgetQueuedMessages(messageIds) {
      const removed = new Set(messageIds);
      if (!removed.size) return;
      for (const messageId of removed) {
        queuedSavePromises.delete(messageId);
        queuedNeedsSave.delete(messageId);
      }
      if ([...removed].some((messageId) => state.failedQueuedPromotionIds.has(messageId))) {
        commit({
          failedQueuedPromotionIds: new Set(
            [...state.failedQueuedPromotionIds].filter((messageId) => !removed.has(messageId))
          )
        });
      }
    }
  };
}
