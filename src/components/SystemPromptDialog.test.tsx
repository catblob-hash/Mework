import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SystemPromptDialog } from "./SystemPromptDialog";
import { MAX_SYSTEM_PROMPT_BYTES } from "../lib/textLimits";

describe("SystemPromptDialog", () => {
  it("keeps edits local until Save and restores the global default into the draft", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(
      <SystemPromptDialog
        value="初始提示"
        defaultValue="全局默认"
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    const prompt = screen.getByRole("textbox", { name: "主系统提示词" });
    await user.clear(prompt);
    await user.type(prompt, "  保留首尾空格  ");
    expect(onSave).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenLastCalledWith("  保留首尾空格  ");

    await user.click(screen.getByRole("button", { name: "恢复全局默认" }));
    expect(prompt).toHaveValue("全局默认");
  });

  it("blocks saving a system prompt that exceeds the backend UTF-8 byte limit", () => {
    const onSave = vi.fn();
    render(
      <SystemPromptDialog
        value=""
        defaultValue=""
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    const prompt = screen.getByRole("textbox", { name: "主系统提示词" });
    fireEvent.change(prompt, { target: { value: "界".repeat(Math.floor(MAX_SYSTEM_PROMPT_BYTES / 3) + 1) } });

    expect(screen.getByRole("alert")).toHaveTextContent("超过 1 MiB");
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    fireEvent.keyDown(prompt, { key: "Enter", ctrlKey: true });
    expect(onSave).not.toHaveBeenCalled();
  });

  it("allows saving a base prompt exactly at the storage byte limit", () => {
    const onSave = vi.fn();
    render(
      <SystemPromptDialog
        value={"x".repeat(MAX_SYSTEM_PROMPT_BYTES)}
        defaultValue=""
        onSave={onSave}
        onClose={vi.fn()}
      />
    );

    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(onSave).toHaveBeenCalledTimes(1);
  });
});
