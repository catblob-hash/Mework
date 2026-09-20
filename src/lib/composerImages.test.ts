import { describe, expect, it, vi } from "vitest";
import { createComposerController } from "./composerController";
import { createSendPipeline, type SendPipelineHost, type SendPipelineStores } from "./sendPipeline";
import type { AppDocument, ImageAttachment } from "../types";
import { documentWithModel } from "../test/appMocks";

const prepareImageAttachment = vi.hoisted(() => vi.fn());

vi.mock("./runtime", async (importOriginal) => ({
  ...await importOriginal<typeof import("./runtime")>(),
  prepareImageAttachment
}));

function pngFile(name: string): File {
  const file = new File([new Uint8Array([1, 2, 3, 4])], name, { type: "image/png" });
  Object.defineProperty(file, "arrayBuffer", {
    configurable: true,
    value: async () => new Uint8Array([1, 2, 3, 4]).buffer
  });
  return file;
}

/**
 * `addComposerImages` is the one send-path entry that runs without a host: it
 * reads the document, uploads, and numbers. That makes what it RETURNS
 * testable, and the return is load-bearing — an element pick records the
 * attachment id from it, and that id is the only thing tying the chip to the
 * crop it stands for. A resolve that forgets the accepted images puts the crop
 * back in the strip beside its own chip.
 */
function pipeline(document: AppDocument) {
  const composerController = createComposerController();
  const stores = {
    documentStore: { current: () => document },
    composerController,
    modelRunController: {}
  } as unknown as SendPipelineStores;
  return {
    composerController,
    pipeline: createSendPipeline(stores, () => ({}) as SendPipelineHost)
  };
}

function visionDocument(): AppDocument {
  const document = documentWithModel();
  const provider = document.globalSettings.apiProviders[0];
  provider.models = [{ ...provider.models[0], capabilities: ["image_recognition"] }];
  return document;
}

describe("addComposerImages", () => {
  it("resolves with the numbered images it accepted, and stages them without touching the draft", async () => {
    prepareImageAttachment.mockImplementation(async (name: string): Promise<ImageAttachment> => ({
      id: `id-${name}`,
      name,
      mime: "image/png",
      width: 2,
      height: 2,
      bytes: 4
    }));
    const document = visionDocument();
    const conversationId = document.workspaces[0].conversations[0].id;
    const { composerController, pipeline: sendPipeline } = pipeline(document);

    const accepted = await sendPipeline.addComposerImages(conversationId, [pngFile("crop.png")]);

    expect(accepted).toEqual([expect.objectContaining({ id: "id-crop.png", shortId: 1 })]);
    expect(composerController.current().imageDrafts[conversationId])
      .toEqual([expect.objectContaining({ id: "id-crop.png", shortId: 1 })]);
    // The number the model reads is written at the send boundary, not here.
    expect(composerController.current().drafts[conversationId] ?? "").toBe("");
  });

  it("resolves empty when the model has no image input, so nothing is staged", async () => {
    prepareImageAttachment.mockClear();
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    const { composerController, pipeline: sendPipeline } = pipeline(document);

    expect(await sendPipeline.addComposerImages(conversationId, [pngFile("blocked.png")])).toEqual([]);
    expect(prepareImageAttachment).not.toHaveBeenCalled();
    expect(composerController.current().imageDrafts[conversationId]).toBeUndefined();
  });

  it("resolves empty for a duplicate, so a chip never claims an image the user attached", async () => {
    prepareImageAttachment.mockImplementation(async (name: string): Promise<ImageAttachment> => ({
      id: "same-bytes",
      name,
      mime: "image/png",
      width: 2,
      height: 2,
      bytes: 4
    }));
    const document = visionDocument();
    const conversationId = document.workspaces[0].conversations[0].id;
    const { pipeline: sendPipeline } = pipeline(document);

    expect(await sendPipeline.addComposerImages(conversationId, [pngFile("first.png")])).toHaveLength(1);
    expect(await sendPipeline.addComposerImages(conversationId, [pngFile("again.png")])).toEqual([]);
  });
});
