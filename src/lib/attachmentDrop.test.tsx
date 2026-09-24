import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AttachmentDropOverlay } from "../components/AttachmentFeedback";
import { configureI18n } from "../i18n";
import type { AttachmentRejection } from "./fileAttachments";
import type { DroppedPathProbe } from "./runtime";

type NativeHandler = (event: { payload: unknown }) => void;

const mocks = vi.hoisted(() => ({
  handler: undefined as NativeHandler | undefined,
  probeDroppedPaths: vi.fn(),
  readDroppedFile: vi.fn()
}));

vi.mock("./backend", () => ({ isTauriRuntime: () => true }));
vi.mock("./runtime", () => ({
  probeDroppedPaths: mocks.probeDroppedPaths,
  readDroppedFile: mocks.readDroppedFile,
  prepareFileAttachment: vi.fn()
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: NativeHandler) => {
      mocks.handler = handler;
      return () => undefined;
    }
  })
}));

const { useAttachmentDropZone } = await import("./attachmentDrop");

function Zone({
  imageInput,
  onDrop
}: {
  imageInput: boolean;
  onDrop: (files: File[], preRejected: AttachmentRejection[]) => void;
}) {
  const drop = useAttachmentDropZone({ imageInput, onDrop });
  return (
    <div ref={drop.ref} data-testid="zone" data-ready={drop.dragging ? "true" : undefined}>
      <AttachmentDropOverlay state={drop} imageInput={imageInput} />
    </div>
  );
}

function probe(path: string, kind: DroppedPathProbe["kind"], sniff: DroppedPathProbe["sniff"], size = 10): DroppedPathProbe {
  return { path, name: path.split("/").pop() ?? path, kind, size, sniff };
}

function emit(payload: unknown) {
  act(() => mocks.handler?.({ payload }));
}

describe("native file drags", () => {
  beforeEach(() => {
    configureI18n("zh-CN");
    mocks.probeDroppedPaths.mockReset();
    mocks.readDroppedFile.mockReset();
  });

  it("shows a dragged folder as unavailable before anything is dropped", async () => {
    render(<Zone imageInput onDrop={vi.fn()} />);
    const zone = screen.getByTestId("zone");
    document.elementFromPoint = vi.fn(() => zone);
    await waitFor(() => expect(mocks.handler).toBeDefined());
    mocks.probeDroppedPaths.mockResolvedValue([probe("/Users/me/photos", "directory", "none", 0)]);

    emit({ type: "enter", paths: ["/Users/me/photos"], position: { x: 4, y: 4 } });
    expect(await screen.findByText("不能添加文件夹")).toBeInTheDocument();
    expect(mocks.probeDroppedPaths).toHaveBeenCalledWith(["/Users/me/photos"]);

    emit({ type: "leave" });
    expect(screen.queryByText("不能添加文件夹")).not.toBeInTheDocument();
  });

  it("says what a mixed drag will take, then reads only that on drop", async () => {
    const onDrop = vi.fn();
    render(<Zone imageInput={false} onDrop={onDrop} />);
    const zone = screen.getByTestId("zone");
    document.elementFromPoint = vi.fn(() => zone);
    await waitFor(() => expect(mocks.handler).toBeDefined());
    const paths = ["/w/notes.md", "/w/bundle.zip", "/w/shot.png"];
    mocks.probeDroppedPaths.mockResolvedValue([
      probe(paths[0], "file", "text"),
      probe(paths[1], "file", "binary"),
      probe(paths[2], "file", "image")
    ]);
    mocks.readDroppedFile.mockImplementation(async (path: string) => new File(["# hi"], path.split("/").pop()!));

    emit({ type: "enter", paths, position: { x: 4, y: 4 } });
    expect(await screen.findByText("松开以添加 1 个文件")).toBeInTheDocument();
    expect(screen.getByText(/1 个不支持的文件/)).toBeInTheDocument();
    expect(screen.getByText(/1 张图片（当前模型不支持图片）/)).toBeInTheDocument();

    emit({ type: "drop", paths, position: { x: 4, y: 4 } });
    await waitFor(() => expect(onDrop).toHaveBeenCalledTimes(1));
    expect(mocks.readDroppedFile).toHaveBeenCalledTimes(1);
    expect(mocks.readDroppedFile).toHaveBeenCalledWith("/w/notes.md");
    const [files, preRejected] = onDrop.mock.calls[0];
    expect(files.map((file: File) => file.name)).toEqual(["notes.md"]);
    expect(preRejected).toEqual([
      { name: "bundle.zip", reason: "unsupported" },
      { name: "shot.png", reason: "imageInputUnavailable" }
    ]);
    expect(screen.queryByText("松开以添加 1 个文件")).not.toBeInTheDocument();
  });

  it("ignores a drop that misses every zone, and a drag that carries no files", async () => {
    const onDrop = vi.fn();
    render(<Zone imageInput onDrop={onDrop} />);
    await waitFor(() => expect(mocks.handler).toBeDefined());
    document.elementFromPoint = vi.fn(() => document.body);
    mocks.probeDroppedPaths.mockResolvedValue([probe("/w/a.md", "file", "text")]);

    emit({ type: "enter", paths: [], position: { x: 1, y: 1 } });
    expect(screen.getByTestId("zone")).not.toHaveAttribute("data-ready");

    emit({ type: "enter", paths: ["/w/a.md"], position: { x: 1, y: 1 } });
    // Somewhere over the window: the zone shows it can be aimed at, without an overlay.
    await waitFor(() => expect(screen.getByTestId("zone")).toHaveAttribute("data-ready", "true"));
    expect(screen.queryByText(/松开以添加/)).not.toBeInTheDocument();

    emit({ type: "drop", paths: ["/w/a.md"], position: { x: 1, y: 1 } });
    await act(async () => {
      await Promise.resolve();
    });
    expect(mocks.readDroppedFile).not.toHaveBeenCalled();
    expect(onDrop).not.toHaveBeenCalled();
  });
});
