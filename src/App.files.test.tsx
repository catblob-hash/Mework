import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { FileAttachment, ModelRunRequest } from "./types";
import { resetAppMocks, documentWithModel, model, runtimeMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

afterEach(() => configureI18n("zh-CN"));

function resolvedRun() {
  runtimeMocks.runModel.mockResolvedValue({
    contexts: [],
    usage: {},
    model: model.id,
    providerName: "OpenAI Responses",
    durationMs: 1
  });
}

const storedFile = (id: string, name: string): FileAttachment => ({
  id: id.padEnd(64, "0"),
  name,
  format: "text",
  bytes: 24,
  tokens: 6
});

describe("App — file attachments", () => {
  beforeEach(resetAppMocks);

  it("uploads a text file from the add menu and sends it as a files-only message", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    resolvedRun();
    const user = userEvent.setup();
    const { container } = render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;

    const input = container.querySelector<HTMLInputElement>(".composer-add-menu__file-input")!;
    await user.upload(input, new File(["# Notes\nhello"], "notes.md", { type: "text/markdown" }));

    expect(await within(composerRegion).findByRole("button", { name: "预览文件 notes.md" })).toBeInTheDocument();
    expect(runtimeMocks.prepareFileAttachment).toHaveBeenCalledWith("notes.md", expect.any(Uint8Array), "text");
    // A model without image input still reads files, so nothing blocks the send.
    const send = screen.getByRole("button", { name: "发送" });
    await waitFor(() => expect(send).toBeEnabled());
    await user.click(send);

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(request.contexts.at(-1)).toMatchObject({
      kind: "user",
      content: "",
      files: [expect.objectContaining({ name: "notes.md", format: "text" })]
    });
    await waitFor(() => expect(within(composerRegion).queryByRole("button", { name: "预览文件 notes.md" }))
      .not.toBeInTheDocument());
  });

  it("folds a long paste into a tag and sends the text it stands for", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    resolvedRun();
    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText<HTMLTextAreaElement>("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;

    const short = fireEvent.paste(composer, { clipboardData: { files: [], getData: () => "short" } });
    expect(short).toBe(true);

    await user.type(composer, "看看这段：");
    const long = "长".repeat(6000);
    const handled = fireEvent.paste(composer, {
      clipboardData: { files: [], getData: (type: string) => (type === "text/plain" ? long : "") }
    });
    expect(handled).toBe(false);
    expect(composer).toHaveValue("看看这段：\u00a0粘贴文本\u00a0#1\u00a0");
    expect(composerRegion.querySelector(".pasted-text-tag")).toHaveTextContent("粘贴文本 #1", { normalizeWhitespace: true });
    expect(runtimeMocks.prepareFileAttachment).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    const sent = request.contexts.at(-1);
    expect(sent).toMatchObject({ kind: "user", content: `看看这段：${long}` });
    expect(sent && "files" in sent ? sent.files : undefined).toBeUndefined();
    await waitFor(() => expect(composer).toHaveValue(""));
    expect(composerRegion.querySelector(".pasted-text-tag")).toBeNull();
  });

  it("says why a binary file or a folder was not attached", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;

    const archive = new File([new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0x00, 0x00])], "bundle.zip", {
      type: "application/zip"
    });
    const folder = new File([], "photos");
    const items = [
      { kind: "file", type: "application/zip", getAsFile: () => archive, webkitGetAsEntry: () => ({ isDirectory: false, name: "bundle.zip" }) },
      { kind: "file", type: "", getAsFile: () => folder, webkitGetAsEntry: () => ({ isDirectory: true, name: "photos" }) }
    ];
    fireEvent.drop(composerRegion, {
      dataTransfer: { files: [archive, folder], items, types: ["Files"] }
    });

    const notice = await screen.findByText("2 项没有添加");
    const box = notice.closest(".attachment-notice") as HTMLElement;
    expect(box).toHaveTextContent("bundle.zip");
    expect(box).toHaveTextContent("不支持的格式");
    expect(box).toHaveTextContent("photos");
    expect(box).toHaveTextContent("不能添加文件夹");
    expect(runtimeMocks.prepareFileAttachment).not.toHaveBeenCalled();

    await userEvent.setup().click(within(box).getByRole("button", { name: "关闭提示" }));
    expect(screen.queryByText("2 项没有添加")).not.toBeInTheDocument();
  });

  it("marks the composer unavailable while a drag of unsupported files hovers it", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;

    const dataTransfer = {
      types: ["Files"],
      items: [{ kind: "file", type: "application/zip" }],
      dropEffect: "copy"
    };
    fireEvent.dragEnter(composerRegion, { dataTransfer });
    fireEvent.dragOver(composerRegion, { dataTransfer });
    expect(await within(composerRegion).findByText("无法添加这些内容")).toBeInTheDocument();
    expect(dataTransfer.dropEffect).toBe("none");

    fireEvent.dragLeave(composerRegion, { dataTransfer, relatedTarget: null });
    await waitFor(() => expect(within(composerRegion).queryByText("无法添加这些内容")).not.toBeInTheDocument());

    const textDrag = { types: ["Files"], items: [{ kind: "file", type: "text/plain" }], dropEffect: "none" };
    fireEvent.dragEnter(composerRegion, { dataTransfer: textDrag });
    fireEvent.dragOver(composerRegion, { dataTransfer: textDrag });
    expect(await within(composerRegion).findByText("松开以添加 1 个文件")).toBeInTheDocument();
    expect(textDrag.dropEffect).toBe("copy");
    fireEvent.dragLeave(composerRegion, { dataTransfer: textDrag, relatedTarget: null });
  });

  it("shows a user message's files on the timeline and previews one", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].contexts = [{
      id: "with-file",
      kind: "user",
      content: "请看附件",
      files: [storedFile("notes", "notes.md")],
      createdAt: "2026-09-20T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    // "# Heading\n\nbody"
    runtimeMocks.fileAttachmentData.mockResolvedValue("data:text/plain;charset=utf-8;base64,IyBIZWFkaW5nCgpib2R5");
    const user = userEvent.setup();
    render(<App />);

    const tile = await screen.findByRole("button", { name: "预览文件 notes.md" });
    await user.click(tile);
    const dialog = await screen.findByRole("dialog", { name: "notes.md" });
    expect(await within(dialog).findByRole("heading", { name: "Heading" })).toBeInTheDocument();
    expect(runtimeMocks.fileAttachmentData).toHaveBeenCalledWith("notes".padEnd(64, "0"));

    await user.click(within(dialog).getByRole("button", { name: "显示源文本" }));
    expect(within(dialog).getByText("# Heading")).toBeInTheDocument();
  });

  it("lets a user message edited on the timeline take more files through its own add menu", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].contexts = [{
      id: "editable",
      kind: "user",
      content: "原消息",
      files: [storedFile("first", "first.txt")],
      createdAt: "2026-09-20T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    const { container } = render(<App />);

    const card = (await screen.findByText("原消息")).closest("[data-context-id]") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "编辑上下文" }));
    const editor = container.querySelector(".inline-text-editor--user") as HTMLElement;
    expect(within(editor).getByRole("button", { name: "预览文件 first.txt" })).toBeInTheDocument();
    await user.click(within(editor).getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menuitem", { name: /上传文件/ })).toBeEnabled();
    await user.keyboard("{Escape}");

    const input = editor.querySelector<HTMLInputElement>(".composer-add-menu__file-input")!;
    await user.upload(input, new File(["second"], "second.txt", { type: "text/plain" }));
    expect(await within(editor).findByRole("button", { name: "预览文件 second.txt" })).toBeInTheDocument();

    await user.click(within(editor).getByRole("button", { name: "保存" }));
    await waitFor(() => {
      const saved = runtimeMocks.saveDocument.mock.calls.at(-1)?.[0];
      expect(saved?.workspaces[0].conversations[0].contexts[0]).toMatchObject({
        id: "editable",
        files: [
          expect.objectContaining({ name: "first.txt" }),
          expect.objectContaining({ name: "second.txt" })
        ]
      });
    });
  });
});
