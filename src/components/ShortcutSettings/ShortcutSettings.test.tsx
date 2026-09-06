import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import type { GlobalSettings } from "../../types";
import { ShortcutSettings } from ".";

afterEach(() => configureI18n("zh-CN"));

describe("ShortcutSettings", () => {
  beforeEach(() => configureI18n("en-US"));

  it("refuses a recorded binding that conflicts with another enabled command", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ShortcutSettings shortcuts={{}} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Change shortcut for “New conversation”" }));
    const recorder = screen.getByRole("button", { name: "Record shortcut for “New conversation”" });
    fireEvent.keyDown(recorder, { key: ",", code: "Comma", ctrlKey: true });

    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent("Conflicts with “Open settings”");
  });

  it("persists a valid free binding and enables its command", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ShortcutSettings shortcuts={{}} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Change shortcut for “New conversation”" }));
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Record shortcut for “New conversation”" }),
      { key: "Y", code: "KeyY", ctrlKey: true, shiftKey: true }
    );

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith({
      "conversation.create": {
        binding: ["Control", "Shift", "KeyY"],
        enabled: true
      }
    });
  });

  it("disables the switch when a command has no binding", () => {
    render(<ShortcutSettings shortcuts={{}} onChange={vi.fn()} />);
    expect(screen.getByRole("switch", { name: "Enable “Stop current run”" })).toBeDisabled();
  });

  it("undoes a command by removing its sparse override", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const shortcuts: GlobalSettings["shortcuts"] = {
      "app.settings.open": { binding: ["Control", "KeyK"], enabled: false }
    };
    render(<ShortcutSettings shortcuts={shortcuts} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Undo changes to “Open settings”" }));

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith({});
  });

  it("cancels recording on Escape without changing the binding", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const shortcuts: GlobalSettings["shortcuts"] = {
      "conversation.create": { binding: ["Control", "KeyK"], enabled: true }
    };
    render(<ShortcutSettings shortcuts={shortcuts} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Change shortcut for “New conversation”" }));
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Record shortcut for “New conversation”" }),
      { key: "Escape", code: "Escape" }
    );

    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Change shortcut for “New conversation”" })).toBeInTheDocument();
  });
});
