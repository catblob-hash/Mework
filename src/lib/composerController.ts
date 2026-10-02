import type { SelectedElement } from "./browser";
import type { AttachmentRejection } from "./fileAttachments";
import type { PastedText } from "./pastedText";
import type { FileAttachment, ImageAttachment } from "../types";

export interface ComposerControllerState {
  /** Composer text drafts per conversation. */
  drafts: Record<string, string>;
  /**
   * The text behind each long paste's label in a draft, per conversation
   * (`lib/pastedText.ts`). Kept until the draft is sent, not only while its
   * label is in the box: an undo can bring a deleted label back.
   */
  pastedTexts: Record<string, PastedText[]>;
  /** Prepared image attachments per conversation. */
  imageDrafts: Record<string, ImageAttachment[]>;
  /** Prepared non-image file attachments per conversation. */
  fileDrafts: Record<string, FileAttachment[]>;
  /** What the last attempt to attach to a conversation's composer left out, until dismissed. */
  attachmentNotices: Record<string, AttachmentRejection[]>;
  /**
   * Elements the user picked out of a page, per conversation. Kept beside the image drafts
   * rather than inside them: the crop travels as an ordinary attachment, but the description
   * is prompt text, and `ImageAttachment` is persisted.
   */
  elementPicks: Record<string, SelectedElement[]>;
  /** Conversations with an attachment upload batch (images or files) still in flight. */
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
  updatePastedTexts(
    updater: (current: Record<string, PastedText[]>) => Record<string, PastedText[]>
  ): void;
  updateImageDrafts(
    updater: (current: Record<string, ImageAttachment[]>) => Record<string, ImageAttachment[]>
  ): void;
  updateFileDrafts(
    updater: (current: Record<string, FileAttachment[]>) => Record<string, FileAttachment[]>
  ): void;
  updateAttachmentNotices(
    updater: (current: Record<string, AttachmentRejection[]>) => Record<string, AttachmentRejection[]>
  ): void;
  updateElementPicks(
    updater: (current: Record<string, SelectedElement[]>) => Record<string, SelectedElement[]>
  ): void;
  updateSteeringMessageIds(updater: (current: Set<string>) => Set<string>): void;
  updateFailedQueuedPromotionIds(updater: (current: Set<string>) => Set<string>): void;
  /**
   * Serializes image upload batches per conversation and keeps the loading
   * flag up while any batch is pending. `uploadStillCurrent` reports whether
   * the conversation's uploads were invalidated after this batch was queued.
   */
  enqueueImageUpload<Result>(
    conversationId: string,
    run: (uploadStillCurrent: () => boolean) => Promise<Result>
  ): Promise<Result>;
  /** Invalidates in-flight uploads and drops attachment drafts and element picks for the conversations. */
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
    pastedTexts: {},
    imageDrafts: {},
    fileDrafts: {},
    attachmentNotices: {},
    elementPicks: {},
    imageLoadingIds: new Set(),
    steeringMessageIds: new Set(),
    failedQueuedPromotionIds: new Set()
  };
  const listeners = new Set<() => void>();
  const uploadQueues = new Map<string, Promise<unknown>>();
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
    updatePastedTexts(updater) {
      commit({ pastedTexts: updater(state.pastedTexts) });
    },
    updateImageDrafts(updater) {
      commit({ imageDrafts: updater(state.imageDrafts) });
    },
    updateFileDrafts(updater) {
      commit({ fileDrafts: updater(state.fileDrafts) });
    },
    updateAttachmentNotices(updater) {
      commit({ attachmentNotices: updater(state.attachmentNotices) });
    },
    updateElementPicks(updater) {
      commit({ elementPicks: updater(state.elementPicks) });
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
        fileDrafts: Object.fromEntries(
          Object.entries(state.fileDrafts).filter(([conversationId]) => !removed.has(conversationId))
        ),
        attachmentNotices: Object.fromEntries(
          Object.entries(state.attachmentNotices).filter(([conversationId]) => !removed.has(conversationId))
        ),
        elementPicks: Object.fromEntries(
          Object.entries(state.elementPicks).filter(([conversationId]) => !removed.has(conversationId))
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
