import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { ToolContext } from "../types";
import { ContextEditor } from "./ContextEditor";

describe("ContextEditor", () => {
  it("keeps an image-only user message editable without requiring replacement text", async () => {
    const user = userEvent.setup();
    const document = createSeedDocument();
    const onSaveText = vi.fn();
    render(
      <ContextEditor
        mode="edit"
        kind="user"
        item={{
          id: "image-user",
          kind: "user",
          content: "",
          images: [{
            id: "image-one",
            name: "screen.png",
            mime: "image/png",
            width: 100,
            height: 80,
            bytes: 500
          }],
          createdAt: "2026-07-24T00:00:00Z"
        }}
        tools={document.tools}
        enabledTools={[]}
        onClose={vi.fn()}
        onSaveText={onSaveText}
        onSaveTool={vi.fn()}
      />
    );

    const save = screen.getByRole("button", { name: "保存" });
    expect(save).toBeEnabled();
    await user.click(save);
    expect(onSaveText).toHaveBeenCalledWith("", [{
      id: "image-one",
      name: "screen.png",
      mime: "image/png",
      width: 100,
      height: 80,
      bytes: 500
    }]);
  });

  it("can remove one broken user attachment without deleting the message", async () => {
    const user = userEvent.setup();
    const document = createSeedDocument();
    const onSaveText = vi.fn();
    render(
      <ContextEditor
        mode="edit"
        kind="user"
        item={{
          id: "image-user",
          kind: "user",
          content: "保留文字",
          images: [
            {
              id: "broken-image",
              name: "broken.png",
              mime: "image/png",
              width: 100,
              height: 80,
              bytes: 500
            },
            {
              id: "good-image",
              name: "good.png",
              mime: "image/png",
              width: 100,
              height: 80,
              bytes: 500
            }
          ],
          createdAt: "2026-07-24T00:00:00Z"
        }}
        tools={document.tools}
        enabledTools={[]}
        onClose={vi.fn()}
        onSaveText={onSaveText}
        onSaveTool={vi.fn()}
      />
    );

    await user.click(screen.getByRole("button", { name: "移除图片 broken.png" }));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSaveText).toHaveBeenCalledWith("保留文字", [
      expect.objectContaining({ id: "good-image" })
    ]);
  });

  it("locks an existing tool and reruns with edited model input", async () => {
    const user = userEvent.setup();
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const item = conversation.contexts.find((context): context is ToolContext => context.kind === "tool")!;
    const onSaveTool = vi.fn().mockResolvedValue({ success: true, output: "new", executedAt: new Date().toISOString(), durationMs: 1 });
    render(
      <ContextEditor
        mode="edit"
        kind="tool"
        item={item}
        tools={document.tools}
        enabledTools={conversation.settings.enabledTools}
        onClose={vi.fn()}
        onSaveText={vi.fn()}
        onSaveTool={onSaveTool}
      />
    );
    expect(screen.getByText("工具已锁定")).toBeInTheDocument();
    expect(screen.getByText("当前返回结果 · 保存成功后会被覆盖")).toBeInTheDocument();
    const query = screen.getByLabelText("文件名模式 *");
    await user.clear(query);
    await user.type(query, "*.rs");
    await user.click(screen.getByRole("button", { name: "保存并重新执行" }));
    expect(onSaveTool).toHaveBeenCalledWith("find", expect.objectContaining({ query: "*.rs" }));
  });

  it("saves removal of a broken tool-result image without rerunning the tool", async () => {
    const user = userEvent.setup();
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const item = conversation.contexts.find((context): context is ToolContext => context.kind === "tool")!;
    const onSaveTool = vi.fn();
    const onSaveToolImages = vi.fn();
    render(
      <ContextEditor
        mode="edit"
        kind="tool"
        item={{
          ...item,
          result: {
            ...item.result,
            images: [
              {
                id: "broken-tool-image",
                name: "broken.png",
                mime: "image/png",
                width: 100,
                height: 80,
                bytes: 500
              },
              {
                id: "good-tool-image",
                name: "good.png",
                mime: "image/png",
                width: 100,
                height: 80,
                bytes: 500
              }
            ]
          }
        }}
        tools={document.tools}
        enabledTools={conversation.settings.enabledTools}
        onClose={vi.fn()}
        onSaveText={vi.fn()}
        onSaveTool={onSaveTool}
        onSaveToolImages={onSaveToolImages}
      />
    );

    await user.click(screen.getByRole("button", { name: "移除图片 broken.png" }));
    await user.click(screen.getByRole("button", { name: "保存图片修改" }));

    expect(onSaveToolImages).toHaveBeenCalledWith([
      expect.objectContaining({ id: "good-tool-image" })
    ]);
    expect(onSaveTool).not.toHaveBeenCalled();
  });

  it("executes a dangerous tool without a second local confirmation", async () => {
    const user = userEvent.setup();
    const document = createSeedDocument();
    const conversation = document.workspaces[0].conversations[0];
    const onSaveTool = vi.fn().mockResolvedValue({
      success: true,
      output: "written",
      executedAt: new Date().toISOString(),
      durationMs: 1
    });
    render(
      <ContextEditor
        mode="insert"
        kind="tool"
        tools={document.tools}
        enabledTools={conversation.settings.enabledTools}
        onClose={vi.fn()}
        onSaveText={vi.fn()}
        onSaveTool={onSaveTool}
      />
    );

    await user.click(screen.getByRole("button", { name: /写入文件/ }));
    expect(screen.getByText("需安全审查")).toBeInTheDocument();
    expect(screen.queryByText("确认执行高风险工具")).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();

    await user.type(screen.getByLabelText("文件路径 *"), "notes.txt");
    await user.type(screen.getByLabelText("文件内容 *"), "hello");
    await user.click(screen.getByRole("button", { name: "执行并添加" }));

    expect(onSaveTool).toHaveBeenCalledWith("write", { path: "notes.txt", content: "hello" });
  });

});
