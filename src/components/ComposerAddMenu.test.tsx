import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { ComposerAddImages } from "./ComposerAddMenu";

describe("ComposerAddImages", () => {
  beforeEach(() => configureI18n("zh-CN"));
  afterEach(() => vi.restoreAllMocks());

  it("opens the file picker from the add menu", async () => {
    const user = userEvent.setup();
    const fileInputClick = vi
      .spyOn(HTMLInputElement.prototype, "click")
      .mockImplementation(() => undefined);
    render(<ComposerAddImages onChooseImages={vi.fn()} />);

    expect(screen.queryByRole("menu")).toBeNull();
    await user.click(screen.getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menu", { name: "添加内容" })).toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: /添加图片/ }));
    expect(fileInputClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("disables the image entry and explains why when images are unavailable", async () => {
    const user = userEvent.setup();
    const fileInputClick = vi
      .spyOn(HTMLInputElement.prototype, "click")
      .mockImplementation(() => undefined);
    render(
      <ComposerAddImages
        imageUnavailableReason="当前模型不支持图片输入"
        onChooseImages={vi.fn()}
      />
    );

    await user.click(screen.getByRole("button", { name: "添加内容" }));
    const item = screen.getByRole("menuitem", { name: /添加图片/ });
    expect(item).toBeDisabled();
    expect(item).toHaveTextContent("当前模型不支持图片输入");
    await user.click(item);
    expect(fileInputClick).not.toHaveBeenCalled();
  });

  it("disables the whole control while the composer is locked", async () => {
    const user = userEvent.setup();
    render(<ComposerAddImages disabled onChooseImages={vi.fn()} />);

    const trigger = screen.getByRole("button", { name: "添加内容" });
    expect(trigger).toBeDisabled();
    await user.click(trigger);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("passes chosen files up and clears the input so the same file can be re-picked", async () => {
    const user = userEvent.setup();
    const onChooseImages = vi.fn();
    const { container } = render(<ComposerAddImages onChooseImages={onChooseImages} />);

    const input = container.querySelector<HTMLInputElement>(".composer-add-menu__file-input");
    expect(input).not.toBeNull();
    await user.upload(input as HTMLInputElement, new File(["x"], "shot.png", { type: "image/png" }));

    expect(onChooseImages).toHaveBeenCalledTimes(1);
    expect(onChooseImages.mock.calls[0][0][0].name).toBe("shot.png");
    expect((input as HTMLInputElement).value).toBe("");
  });
});
