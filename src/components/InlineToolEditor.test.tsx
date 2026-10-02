import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { ToolContext } from "../types";
import { draftToolContext, InlineToolEditor } from "./InlineToolEditor";

const document = createSeedDocument();
const conversation = document.workspaces[0].conversations[0];
const recordedFind = conversation.contexts.find((context): context is ToolContext => context.kind === "tool")!;
const descriptorFor = (name: string) => document.tools.find((tool) => tool.name === name);

const image = (id: string, name: string) => ({ id, name, mime: "image/png", width: 100, height: 80, bytes: 500 });

function writeCall(): ToolContext {
  return {
    id: "write-call",
    kind: "tool",
    toolName: "write",
    input: { path: "notes.txt", content: "hello" },
    result: { success: true, output: "written", executedAt: "2026-07-24T00:00:00Z", durationMs: 1 },
    createdAt: "2026-07-24T00:00:00Z"
  };
}

describe("InlineToolEditor", () => {
  it("edits the arguments of a side-effect-free call and reruns it", async () => {
    const user = userEvent.setup();
    const onRerun = vi.fn().mockResolvedValue(undefined);
    render(
      <InlineToolEditor
        item={recordedFind}
        descriptor={descriptorFor(recordedFind.toolName)}
        onCancel={vi.fn()}
        onRun={onRerun}
        onSave={vi.fn().mockResolvedValue(undefined)}
      />
    );

    expect(recordedFind.toolName).toBe("find");
    const query = screen.getByLabelText("文件名模式 *");
    await user.clear(query);
    await user.type(query, "*.rs");
    await user.click(screen.getByRole("button", { name: "重新执行" }));

    expect(onRerun).toHaveBeenCalledWith("find", expect.objectContaining({ query: "*.rs" }));
  });

  it("saves edited arguments and result without executing anything", async () => {
    const user = userEvent.setup();
    const onRerun = vi.fn();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <InlineToolEditor
        item={writeCall()}
        descriptor={descriptorFor("write")}
        onCancel={vi.fn()}
        onRun={onRerun}
        onSave={onSave}
      />
    );

    const result = screen.getByLabelText("返回值");
    await user.clear(result);
    await user.type(result, "rewritten by hand");
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({ path: "notes.txt" }),
      "rewritten by hand",
      []
    );
    expect(onRerun).not.toHaveBeenCalled();
  });

  it("saves an untouched call exactly as recorded, adding no defaults and dropping no explicit values", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    const read: ToolContext = {
      id: "read-call",
      kind: "tool",
      toolName: "read",
      input: { path: "shot.png" },
      result: { success: true, output: "image", executedAt: "2026-07-24T00:00:00Z", durationMs: 1 },
      createdAt: "2026-07-24T00:00:00Z"
    };
    const { unmount } = render(
      <InlineToolEditor item={read} descriptor={descriptorFor("read")} onCancel={vi.fn()} onSave={onSave} />
    );
    // The declared default is a hint in the empty control, not a value.
    expect(screen.getByLabelText("起始行")).toHaveValue("");
    expect(screen.getByLabelText("起始行")).toHaveAttribute("placeholder", "1");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenLastCalledWith({ path: "shot.png" }, "image", []);
    unmount();

    const shell = descriptorFor("zsh") ?? descriptorFor("bash")!;
    const explicit: ToolContext = {
      ...read,
      id: "shell-call",
      toolName: shell.name,
      input: { command: "ls", run_in_background: false }
    };
    render(<InlineToolEditor item={explicit} descriptor={shell} onCancel={vi.fn()} onSave={onSave} />);
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenLastCalledWith({ command: "ls", run_in_background: false }, "image", []);
  });

  it("edits the call as the model made it and keeps untouched values exactly", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    const rewritten: ToolContext = {
      ...writeCall(),
      // A hook ran it with another path; the model wrote `notes.txt`, a number
      // where text belongs, and an explicit null.
      requestedInput: { path: "notes.txt", content: 5, mode: null },
      input: { path: "safe/notes.txt", content: "5" }
    };
    render(<InlineToolEditor item={rewritten} descriptor={descriptorFor("write")} onCancel={vi.fn()} onSave={onSave} />);
    expect(screen.getByLabelText("文件路径 *")).toHaveValue("notes.txt");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenLastCalledWith({ path: "notes.txt", content: 5, mode: null }, "written", []);
  });

  it("offers no rerun for a call the host only runs inside a model turn", () => {
    render(
      <InlineToolEditor
        item={{
          id: "plan-call",
          kind: "tool",
          toolName: "plan",
          input: { action: "set", content: "步骤一" },
          result: { success: true, output: "plan updated", executedAt: "2026-07-24T00:00:00Z", durationMs: 1 },
          createdAt: "2026-07-24T00:00:00Z"
        }}
        descriptor={descriptorFor("plan")}
        onCancel={vi.fn()}
        onRun={vi.fn()}
        onSave={vi.fn()}
      />
    );

    expect(screen.queryByRole("button", { name: "重新执行" })).not.toBeInTheDocument();
    // Its arguments stay editable and saveable, one row per recorded key.
    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
    expect(screen.getByText("action")).toBeInTheDocument();
  });

  it("shows a side-effecting call's recorded arguments without a rerun button", () => {
    render(
      <InlineToolEditor
        item={writeCall()}
        descriptor={descriptorFor("write")}
        onCancel={vi.fn()}
        onRun={vi.fn()}
        onSave={vi.fn()}
      />
    );

    expect(screen.queryByRole("button", { name: "重新执行" })).not.toBeInTheDocument();
    expect(screen.getByText("path")).toBeInTheDocument();
    expect(screen.getByLabelText("文件路径 *")).toHaveValue("notes.txt");
    expect(screen.getByLabelText("返回值")).toHaveValue("written");
  });

  it("saves removal of a broken result image alongside the edited payload", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <InlineToolEditor
        item={{
          ...recordedFind,
          result: { ...recordedFind.result, images: [image("broken-tool-image", "broken.png"), image("good-tool-image", "good.png")] }
        }}
        descriptor={descriptorFor(recordedFind.toolName)}
        onCancel={vi.fn()}
        onRun={vi.fn()}
        onSave={onSave}
      />
    );

    await user.click(screen.getByRole("button", { name: "移除图片 broken.png" }));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).toHaveBeenCalledWith(
      expect.anything(),
      expect.any(String),
      [expect.objectContaining({ id: "good-tool-image" })]
    );
  });

  it("reports a failed rerun without discarding the edited arguments", async () => {
    const user = userEvent.setup();
    const onRerun = vi.fn().mockRejectedValue(new Error("工作区不可用"));
    render(
      <InlineToolEditor
        item={recordedFind}
        descriptor={descriptorFor(recordedFind.toolName)}
        onCancel={vi.fn()}
        onRun={onRerun}
        onSave={vi.fn()}
      />
    );

    await user.clear(screen.getByLabelText("文件名模式 *"));
    await user.type(screen.getByLabelText("文件名模式 *"), "*.rs");
    await user.click(screen.getByRole("button", { name: "重新执行" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("工作区不可用");
    expect(screen.getByLabelText("文件名模式 *")).toHaveValue("*.rs");
  });
});

describe("InlineToolEditor inserting a call", () => {
  it("runs a first execution of a side-effecting tool without a second local confirmation", async () => {
    const user = userEvent.setup();
    const onRun = vi.fn().mockResolvedValue(undefined);
    render(
      <InlineToolEditor
        item={draftToolContext("write")}
        descriptor={descriptorFor("write")}
        inserting
        onCancel={vi.fn()}
        onRun={onRun}
      />
    );

    await user.type(screen.getByLabelText("文件路径 *"), "notes.txt");
    await user.type(screen.getByLabelText("文件内容 *"), "hello");
    await user.click(screen.getByRole("button", { name: "执行并添加" }));

    expect(onRun).toHaveBeenCalledWith("write", { path: "notes.txt", content: "hello" });
  });

  it("writes out a side-effecting call, result and all, without executing it", async () => {
    const user = userEvent.setup();
    const onRun = vi.fn().mockResolvedValue(undefined);
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <InlineToolEditor
        item={draftToolContext("write")}
        descriptor={descriptorFor("write")}
        inserting
        onCancel={vi.fn()}
        onRun={onRun}
        onSave={onSave}
      />
    );

    await user.type(screen.getByLabelText("文件路径 *"), "notes.txt");
    await user.type(screen.getByLabelText("文件内容 *"), "hello");
    await user.type(screen.getByLabelText("返回值"), "已写入 notes.txt");
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSave).toHaveBeenCalledWith({ path: "notes.txt", content: "hello" }, "已写入 notes.txt", []);
    // Saving is the whole point: the file on disk is untouched.
    expect(onRun).not.toHaveBeenCalled();
  });
});
