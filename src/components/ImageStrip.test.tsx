import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { ImageStrip } from "./ImageStrip";

const runtimeMocks = vi.hoisted(() => ({
  imageAttachmentData: vi.fn()
}));

vi.mock("../lib/runtime", () => runtimeMocks);

const image = {
  id: "image-one",
  name: "screen.png",
  mime: "image/png",
  width: 1280,
  height: 720,
  bytes: 1234
};

describe("ImageStrip", () => {
  beforeEach(() => {
    configureI18n("zh-CN");
    runtimeMocks.imageAttachmentData.mockReset().mockResolvedValue("data:image/png;base64,AAAA");
  });

  it("loads bytes on demand and exposes a removable thumbnail", async () => {
    const user = userEvent.setup();
    const onRemove = vi.fn();

    render(<ImageStrip images={[image]} onRemove={onRemove} />);

    expect(await screen.findByRole("img", { name: "screen.png" })).toHaveAttribute(
      "src",
      "data:image/png;base64,AAAA"
    );
    expect(runtimeMocks.imageAttachmentData).toHaveBeenCalledWith("image-one");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "移除图片 screen.png" }));
    expect(onRemove).toHaveBeenCalledWith("image-one");
  });

  it("opens the full image from the keyboard and restores focus after close or Escape", async () => {
    const user = userEvent.setup();
    render(<ImageStrip images={[{ ...image, id: "keyboard-viewer-image" }]} />);

    await screen.findByRole("img", { name: "screen.png" });
    const trigger = screen.getByRole("button", { name: "查看原图 screen.png" });
    trigger.focus();
    await user.keyboard("{Enter}");

    let dialog = screen.getByRole("dialog", { name: "查看原图 screen.png" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(within(dialog).getByRole("img", { name: "screen.png 原图" }))
      .toHaveClass("image-viewer__image");
    const close = within(dialog).getByRole("button", { name: "关闭原图 screen.png" });
    expect(close).toHaveFocus();

    await user.click(close);
    expect(screen.queryByRole("dialog", { name: "查看原图 screen.png" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();

    await user.keyboard(" ");
    dialog = screen.getByRole("dialog", { name: "查看原图 screen.png" });
    expect(within(dialog).getByRole("button", { name: "关闭原图 screen.png" })).toHaveFocus();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
  });

  it("announces a local failure and retries without failing the whole strip", async () => {
    const user = userEvent.setup();
    runtimeMocks.imageAttachmentData
      .mockRejectedValueOnce(new Error("missing"))
      .mockResolvedValueOnce("data:image/png;base64,BBBB");

    render(<ImageStrip images={[{ ...image, id: "missing-image" }]} />);

    const status = await screen.findByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("screen.png 加载失败");
    expect(screen.getByRole("list", { name: "1 张图片" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试加载图片 screen.png" }));

    const recoveredImage = await screen.findByRole("img", { name: "screen.png" });
    expect(recoveredImage).toHaveAttribute("src", "data:image/png;base64,BBBB");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    fireEvent.load(recoveredImage);
    expect(await screen.findByRole("status")).toHaveTextContent("screen.png 已重新加载");
    expect(runtimeMocks.imageAttachmentData).toHaveBeenCalledTimes(2);
  });

  it("turns a browser decode failure into the same local placeholder", async () => {
    render(<ImageStrip images={[{ ...image, id: "invalid-pixels" }]} />);

    fireEvent.error(await screen.findByRole("img", { name: "screen.png" }));

    expect(await screen.findByRole("status")).toHaveTextContent("screen.png 加载失败");
    expect(screen.queryByRole("img", { name: "screen.png" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试加载图片 screen.png" })).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "1 张图片" })).toBeInTheDocument();
  });
});
