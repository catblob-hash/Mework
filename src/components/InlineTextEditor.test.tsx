import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { InlineTextEditor } from "./InlineTextEditor";

const image = (id: string, name: string) => ({ id, name, mime: "image/png", width: 100, height: 80, bytes: 500 });

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
    expect(onSave).toHaveBeenCalledWith("", [image("image-one", "screen.png")]);
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

    expect(onSave).toHaveBeenCalledWith("保留文字", [expect.objectContaining({ id: "good-image" })]);
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
    ]);
  });

  it("takes a pasted image and saves it with the number the surface assigned", async () => {
    const pasted = { ...image("pasted-image", "pasted.png"), shortId: 4 };
    const onPasteImages = vi.fn().mockResolvedValue([pasted]);
    const onSave = vi.fn();
    render(
      <InlineTextEditor
        kind="user"
        content="对比一下"
        onPasteImages={onPasteImages}
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
    expect(onSave).toHaveBeenCalledWith("对比一下 [Image #4]", [pasted]);
  });

  it("leaves a paste alone when the message's model has no image input", () => {
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
});
