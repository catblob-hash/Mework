import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { HostedWindow, WindowLayerOutlet, WindowLayerProvider } from "./WindowLayer";

/** An opener deep in a pane that watches every pointer-down inside it, as the side pane does. */
function Opener({ onPaneEvent, label = "打开" }: { onPaneEvent: () => void; label?: string }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  return (
    <section data-testid="pane" onPointerDownCapture={onPaneEvent}>
      <button type="button" onClick={() => setOpen(true)}>{label}</button>
      {open && (
        <HostedWindow>
          <div role="dialog" aria-label={`${label}窗口`}>
            <input aria-label="名称" value={name} onChange={(event) => setName(event.target.value)} />
            <button type="button" onClick={() => setOpen(false)}>关闭</button>
          </div>
        </HostedWindow>
      )}
    </section>
  );
}

describe("WindowLayer", () => {
  it("mounts an opened window at the outlet, outside the pane that opened it", async () => {
    const user = userEvent.setup();
    const onPaneEvent = vi.fn();
    render(
      <WindowLayerProvider>
        <Opener onPaneEvent={onPaneEvent} />
        <div data-testid="root-layer"><WindowLayerOutlet /></div>
      </WindowLayerProvider>
    );

    await user.click(screen.getByRole("button", { name: "打开" }));
    const dialog = screen.getByRole("dialog", { name: "打开窗口" });
    expect(screen.getByTestId("root-layer")).toContainElement(dialog);
    expect(screen.getByTestId("pane")).not.toContainElement(dialog);

    // Its events are the root's, not the pane's.
    onPaneEvent.mockClear();
    await user.click(screen.getByRole("textbox", { name: "名称" }));
    expect(onPaneEvent).not.toHaveBeenCalled();

    // The opener still owns the state, and the window follows it keystroke by keystroke.
    await user.type(screen.getByRole("textbox", { name: "名称" }), "审查");
    expect(screen.getByRole("textbox", { name: "名称" })).toHaveValue("审查");

    await user.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("stacks a window opened later over one already open", async () => {
    const user = userEvent.setup();
    render(
      <WindowLayerProvider>
        <Opener onPaneEvent={vi.fn()} label="甲" />
        <Opener onPaneEvent={vi.fn()} label="乙" />
        <WindowLayerOutlet />
      </WindowLayerProvider>
    );

    await user.click(screen.getByRole("button", { name: "乙" }));
    await user.click(screen.getByRole("button", { name: "甲" }));
    expect(screen.getAllByRole("dialog").map((dialog) => dialog.getAttribute("aria-label")))
      .toEqual(["乙窗口", "甲窗口"]);
  });

  it("renders in place where no layer is provided", async () => {
    const user = userEvent.setup();
    render(<Opener onPaneEvent={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "打开" }));
    expect(screen.getByTestId("pane")).toContainElement(screen.getByRole("dialog"));
  });
});
