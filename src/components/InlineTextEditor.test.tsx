import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { InlineTextEditor } from "./InlineTextEditor";

const image = (id: string, name: string) => ({ id, name, mime: "image/png", width: 100, height: 80, bytes: 500 });
const file = (id: string, name: string, format: "text" | "pdf" = "text") => ({
  id,
  name,
  format,
  bytes: 1200,
  tokens: 300,
  ...(format === "pdf" ? { pages: 2 } : {})
});

describe("InlineTextEditor", () => {
  it("keeps an image-only user message savable without requiring replacement text", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content=""
        images={[image("image-one", "screen.png")]}
        onCancel={vi.fn()}
        onSave={onSave}
      />
    );

    const save = screen.getByRole("button", { name: "保存" });
    expect(save).toBeEnabled();
    await user.click(save);
    expect(onSave).toHaveBeenCalledWith("", [image("image-one", "screen.png")], []);
  });

  it("can remove one broken attachment without deleting the message", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content="保留文字"
        images={[image("broken-image", "broken.png"), image("good-image", "good.png")]}
        onCancel={vi.fn()}
        onSave={onSave}
      />
    );

    await user.click(screen.getByRole("button", { name: "移除图片 broken.png" }));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).toHaveBeenCalledWith("保留文字", [expect.objectContaining({ id: "good-image" })], []);
  });

  it("refuses to save an empty message and cancels on Escape", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    render(<InlineTextEditor kind="assistant" content="" onCancel={onCancel} onSave={vi.fn()} />);

    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    await user.type(screen.getByRole("textbox"), "{Escape}");
    expect(onCancel).toHaveBeenCalled();
  });

  it("hides the number its own thumbnail stands for and writes it back on save", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content="看看这个 [Image #1]"
        images={[{ ...image("image-one", "screen.png"), shortId: 1 }]}
        onCancel={vi.fn()}
        onSave={onSave}
      />
    );

    expect(screen.getByRole("textbox")).toHaveValue("看看这个");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenCalledWith("看看这个 [Image #1]", [
      expect.objectContaining({ id: "image-one", shortId: 1 })
    ], []);
  });

  it("takes a pasted image and saves it with the number the surface assigned", async () => {
    const pasted = { ...image("pasted-image", "pasted.png"), shortId: 4 };
    const onAddAttachments = vi.fn().mockResolvedValue({ images: [pasted], files: [], rejected: [] });
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content="对比一下"
        onAddAttachments={onAddAttachments}
        imageInput
        onCancel={vi.fn()}
        onSave={onSave}
      />
    );

    const box = screen.getByRole("textbox");
    fireEvent.paste(box, {
      clipboardData: {
        files: [new File([new Uint8Array([1])], "pasted.png", { type: "image/png" })],
        getData: () => ""
      }
    });

    // The thumbnail's own bytes are a runtime read this bare render has no host
    // for; its remove control is what proves the attachment reached the editor.
    expect(await screen.findByRole("button", { name: "移除图片 pasted.png" })).toBeInTheDocument();
    // The box still reads as what was typed; the number rides on the save.
    expect(box).toHaveValue("对比一下");
    await userEvent.setup().click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenCalledWith("对比一下 [Image #4]", [pasted], []);
    expect(onAddAttachments).toHaveBeenCalledWith(
      [expect.objectContaining({ name: "pasted.png" })],
      { images: [], files: [] },
      []
    );
  });

  it("leaves a paste alone where nothing can be attached", () => {
    const onSave = vi.fn();
    render(
      <InlineTextEditor kind="user" content="只是文字" onCancel={vi.fn()} onSave={onSave} />
    );

    fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: {
        files: [new File([new Uint8Array([1])], "blocked.png", { type: "image/png" })],
        getData: () => ""
      }
    });

    expect(screen.queryByRole("button", { name: "移除图片 blocked.png" })).not.toBeInTheDocument();
  });

  it("keeps the files a message carries, lets one be removed, and saves the rest", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content="看附件"
        files={[file("file-one", "notes.md"), file("file-two", "paper.pdf", "pdf")]}
        onCancel={vi.fn()}
        onSave={onSave}
      />
    );

    expect(screen.getByRole("button", { name: "预览文件 notes.md" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "移除文件 notes.md" }));
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenCalledWith("看附件", [], [expect.objectContaining({ id: "file-two" })]);
  });

  it("keeps a files-only message savable", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <InlineTextEditor kind="user" content="" files={[file("file-one", "notes.md")]} onCancel={vi.fn()} onSave={onSave} />
    );

    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenCalledWith("", [], [expect.objectContaining({ id: "file-one" })]);
  });

  it("turns a long paste into a Markdown file instead of prose", async () => {
    const attached = file("pasted", "pasted-text.md");
    const onAddAttachments = vi.fn().mockResolvedValue({ images: [], files: [attached], rejected: [] });
    render(
      <InlineTextEditor kind="user" content="" onAddAttachments={onAddAttachments} onCancel={vi.fn()} onSave={vi.fn()} />
    );

    const long = Array.from({ length: 120 }, (_, index) => `line ${index}`).join("\n");
    const box = screen.getByRole("textbox");
    const pasteEvent = fireEvent.paste(box, {
      clipboardData: { files: [], getData: () => long }
    });

    expect(pasteEvent).toBe(false);
    expect(await screen.findByRole("button", { name: "预览文件 pasted-text.md" })).toBeInTheDocument();
    const [[files]] = onAddAttachments.mock.calls;
    expect(files[0].name).toBe("pasted-text.md");
    expect(await files[0].text()).toBe(long);
    expect(box).toHaveValue("");
  });

  it("leaves a short paste in the box", () => {
    const onAddAttachments = vi.fn();
    render(
      <InlineTextEditor kind="user" content="" onAddAttachments={onAddAttachments} onCancel={vi.fn()} onSave={vi.fn()} />
    );

    const pasteEvent = fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: { files: [], getData: () => "short" }
    });
    expect(pasteEvent).toBe(true);
    expect(onAddAttachments).not.toHaveBeenCalled();
  });

  it("reports what an attempt left out, and why", async () => {
    const onAddAttachments = vi.fn().mockResolvedValue({
      images: [],
      files: [],
      rejected: [{ name: "archive.zip", reason: "unsupported" }]
    });
    render(
      <InlineTextEditor kind="user" content="x" onAddAttachments={onAddAttachments} onCancel={vi.fn()} onSave={vi.fn()} />
    );

    fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: { files: [new File(["PK"], "archive.zip")], getData: () => "" }
    });

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent("archive.zip");
    expect(notice).toHaveTextContent("不支持的格式");
  });

  it("offers the add menu on a user message that can take attachments", async () => {
    const user = userEvent.setup();
    const onAddAttachments = vi.fn().mockResolvedValue({ images: [], files: [file("f", "a.txt")], rejected: [] });
    const { container } = render(
      <InlineTextEditor kind="user" content="x" onAddAttachments={onAddAttachments} onCancel={vi.fn()} onSave={vi.fn()} />
    );

    await user.click(screen.getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menuitem", { name: /上传文件/ })).toBeEnabled();
    const input = container.querySelector<HTMLInputElement>(".composer-add-menu__file-input");
    await user.upload(input as HTMLInputElement, new File(["hello"], "a.txt", { type: "text/plain" }));
    expect(await screen.findByRole("button", { name: "预览文件 a.txt" })).toBeInTheDocument();
  });

  it("has no add menu where nothing can be attached", () => {
    render(<InlineTextEditor kind="assistant" content="x" onCancel={vi.fn()} onSave={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "添加内容" })).not.toBeInTheDocument();
  });
});
