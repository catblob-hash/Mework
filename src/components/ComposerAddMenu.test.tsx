import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { ComposerAddFiles } from "./ComposerAddMenu";

describe("ComposerAddFiles", () => {
  beforeEach(() => configureI18n("zh-CN"));
  afterEach(() => vi.restoreAllMocks());

  it("opens the file picker from the add menu, which offers files rather than images", async () => {
    const user = userEvent.setup();
    const fileInputClick = vi
      .spyOn(HTMLInputElement.prototype, "click")
      .mockImplementation(() => undefined);
    render(<ComposerAddFiles onChooseFiles={vi.fn()} />);

    expect(screen.queryByRole("menu")).toBeNull();
    await user.click(screen.getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menu", { name: "添加内容" })).toBeInTheDocument();
    // Pictures are one kind of file; there is no separate entry for them.
    expect(screen.queryByRole("menuitem", { name: /添加图片/ })).toBeNull();

    const item = screen.getByRole("menuitem", { name: /上传文件/ });
    expect(item).toHaveAttribute("title", "图片、PDF 或文本文件；也可粘贴或拖入");
    await user.click(item);
    expect(fileInputClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("keeps uploads open for a model without image input and says what it takes", async () => {
    const user = userEvent.setup();
    render(<ComposerAddFiles imageInput={false} onChooseFiles={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "添加内容" }));
    const item = screen.getByRole("menuitem", { name: /上传文件/ });
    expect(item).toBeEnabled();
    expect(item).toHaveAttribute("title", "PDF 或文本文件（当前模型不支持图片）；也可粘贴或拖入");
  });

  it("disables the entry and explains why when nothing can be attached", async () => {
    const user = userEvent.setup();
    const fileInputClick = vi
      .spyOn(HTMLInputElement.prototype, "click")
      .mockImplementation(() => undefined);
    render(<ComposerAddFiles unavailableReason="正在处理附件…" onChooseFiles={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "添加内容" }));
    const item = screen.getByRole("menuitem", { name: /上传文件/ });
    expect(item).toBeDisabled();
    // The row is single-line, so the reason lives in its tooltip.
    expect(item).toHaveAttribute("title", "正在处理附件…");
    await user.click(item);
    expect(fileInputClick).not.toHaveBeenCalled();
  });

  it("disables the whole control while the composer is locked", async () => {
    const user = userEvent.setup();
    render(<ComposerAddFiles disabled onChooseFiles={vi.fn()} />);

    const trigger = screen.getByRole("button", { name: "添加内容" });
    expect(trigger).toBeDisabled();
    await user.click(trigger);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("takes any file type and clears the input so the same file can be re-picked", async () => {
    const user = userEvent.setup({ applyAccept: false });
    const onChooseFiles = vi.fn();
    const { container } = render(<ComposerAddFiles onChooseFiles={onChooseFiles} />);

    const input = container.querySelector<HTMLInputElement>(".composer-add-menu__file-input");
    expect(input).not.toBeNull();
    expect(input).not.toHaveAttribute("accept");
    await user.upload(input as HTMLInputElement, [
      new File(["x"], "shot.png", { type: "image/png" }),
      new File(["# notes"], "notes.md", { type: "text/markdown" })
    ]);

    expect(onChooseFiles).toHaveBeenCalledTimes(1);
    expect(onChooseFiles.mock.calls[0][0].map((file: File) => file.name)).toEqual(["shot.png", "notes.md"]);
    expect((input as HTMLInputElement).value).toBe("");
  });
});
