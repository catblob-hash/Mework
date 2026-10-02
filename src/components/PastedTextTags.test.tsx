import { fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { DRAFT_CLIPBOARD_TYPE, expandPastedTexts, type PastedText } from "../lib/pastedText";
import { usePastedTextTags } from "./PastedTextTags";

const NBSP = " ";
const tagLabel = (id: number, extraLines: number) => (
  `${NBSP}粘贴文本${NBSP}#${id}${extraLines ? `${NBSP}+${extraLines}${NBSP}行` : ""}${NBSP}`
);

function Box({ initial = "", label = "box" }: { initial?: string; label?: string }) {
  const [value, setValue] = useState(initial);
  const [pastes, setPastes] = useState<PastedText[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const tags = usePastedTextTags({
    textareaRef,
    value,
    pastes,
    onPastesChange: (update) => setPastes((current) => update(current))
  });
  return (
    <div className="pasted-text-host" data-testid={`${label}-host`}>
      {tags.layer}
      <textarea
        ref={textareaRef}
        aria-label={label}
        value={value}
        onChange={(event) => setValue(event.target.value)}
        onPaste={(event) => {
          tags.onPaste(event);
        }}
      />
      <output aria-label={`${label} sends`}>{expandPastedTexts(value, pastes)}</output>
    </div>
  );
}

function clipboard(data: Record<string, string>) {
  return { files: [], getData: (type: string) => data[type] ?? "", setData: vi.fn() };
}

function pasteLong(box: HTMLTextAreaElement, text: string) {
  return fireEvent.paste(box, { clipboardData: clipboard({ "text/plain": text }) });
}

const long = Array.from({ length: 30 }, (_, index) => `line ${index} ${"x".repeat(40)}`).join("\n");

describe("usePastedTextTags", () => {
  it("folds a paste over 800 characters into a tag at the caret, and sends it as the text", () => {
    render(<Box initial="ab" />);
    const box = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "box" });
    box.focus();
    box.setSelectionRange(1, 1);

    expect(pasteLong(box, "short")).toBe(true);
    expect(pasteLong(box, long.replace(/\n/g, "\r\n"))).toBe(false);

    const label = tagLabel(1, 29);
    expect(box).toHaveValue(`a${label}b`);
    expect(box.selectionStart).toBe(1 + label.length);
    expect(screen.getByTestId("box-host").querySelector(".pasted-text-tag")?.textContent).toBe(label);
    // CRLF becomes the LF a textarea would have held.
    expect(screen.getByRole("status", { name: "box sends" }).textContent).toBe(`a${long}b`);
  });

  it("deletes a tag whole, and moves the caret over it in one step", () => {
    render(<Box initial="ab" />);
    const box = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "box" });
    box.focus();
    box.setSelectionRange(1, 1);
    pasteLong(box, long);
    const end = 1 + tagLabel(1, 29).length;

    // Backspace just after the tag selects all of it, so the key's own default
    // action removes it as one undoable step.
    box.setSelectionRange(end, end);
    fireEvent.keyDown(box, { key: "Backspace" });
    expect([box.selectionStart, box.selectionEnd]).toEqual([1, end]);
    box.setSelectionRange(1, 1);
    fireEvent.keyDown(box, { key: "Delete" });
    expect([box.selectionStart, box.selectionEnd]).toEqual([1, end]);

    box.setSelectionRange(end, end);
    expect(fireEvent.keyDown(box, { key: "ArrowLeft" })).toBe(false);
    expect([box.selectionStart, box.selectionEnd]).toEqual([1, 1]);
    expect(fireEvent.keyDown(box, { key: "ArrowRight" })).toBe(false);
    expect([box.selectionStart, box.selectionEnd]).toEqual([end, end]);
    fireEvent.keyDown(box, { key: "ArrowLeft", shiftKey: true });
    expect([box.selectionStart, box.selectionEnd, box.selectionDirection]).toEqual([1, end, "backward"]);
    // Away from a tag the keys are the textarea's own.
    box.setSelectionRange(0, 0);
    expect(fireEvent.keyDown(box, { key: "ArrowRight" })).toBe(true);
  });

  it("never leaves the caret or a selection edge inside a tag", () => {
    render(<Box initial="ab" />);
    const box = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "box" });
    box.focus();
    box.setSelectionRange(1, 1);
    pasteLong(box, long);
    const end = 1 + tagLabel(1, 29).length;

    // A click near the start lands before the tag, one near the end after it.
    fireEvent.mouseDown(box);
    box.setSelectionRange(3, 3);
    fireEvent.mouseUp(window);
    expect(box.selectionStart).toBe(1);
    fireEvent.mouseDown(box);
    box.setSelectionRange(end - 2, end - 2);
    fireEvent.mouseUp(window);
    expect(box.selectionStart).toBe(end);
    // The keyboard carries on through in the direction it was going.
    box.setSelectionRange(end - 2, end - 2);
    fireEvent.keyUp(box, { key: "ArrowUp" });
    expect(box.selectionStart).toBe(1);

    box.setSelectionRange(0, 4);
    fireEvent.select(box);
    expect([box.selectionStart, box.selectionEnd]).toEqual([0, end]);
  });

  it("copies what the box shows as text elsewhere, and as tags back into a box", () => {
    render(
      <>
        <Box initial="ab" label="from" />
        <Box initial="" label="to" />
      </>
    );
    const from = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "from" });
    from.focus();
    from.setSelectionRange(1, 1);
    pasteLong(from, long);

    const copied = clipboard({});
    from.setSelectionRange(0, from.value.length);
    expect(fireEvent.copy(from, { clipboardData: copied })).toBe(false);
    expect(copied.setData).toHaveBeenCalledWith("text/plain", `a${long}b`);
    const draft = copied.setData.mock.calls.find(([type]) => type === DRAFT_CLIPBOARD_TYPE)?.[1] as string;
    expect(JSON.parse(draft)).toEqual({
      text: `a${tagLabel(1, 29)}b`,
      pastes: [{ label: tagLabel(1, 29), content: long }]
    });

    const to = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "to" });
    to.focus();
    pasteLong(to, "y".repeat(900));
    expect(fireEvent.paste(to, {
      clipboardData: clipboard({ "text/plain": `a${long}b`, [DRAFT_CLIPBOARD_TYPE]: draft })
    })).toBe(false);
    // The copied tag takes the next number here rather than colliding with #1.
    expect(to).toHaveValue(`${tagLabel(1, 0)}a${tagLabel(2, 29)}b`);
    expect(screen.getByRole("status", { name: "to sends" }).textContent).toBe(`${"y".repeat(900)}a${long}b`);
  });

  it("brings a copy back as it was even when the clipboard kept only its text", () => {
    render(<Box initial="ab" />);
    const box = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "box" });
    box.focus();
    box.setSelectionRange(1, 1);
    pasteLong(box, long);
    const value = box.value;

    box.setSelectionRange(0, value.length);
    fireEvent.cut(box, { clipboardData: clipboard({}) });
    expect(box).toHaveValue("");
    expect(pasteLong(box, `a${long}b`)).toBe(false);
    // The tag's text is already this draft's #1, so it comes back as #1.
    expect(box).toHaveValue(value);
  });

  it("does not fold long plain text copied out of a box", () => {
    const typed = "t".repeat(1_000);
    render(<Box initial={typed} />);
    const box = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "box" });
    box.focus();
    box.setSelectionRange(0, typed.length);
    fireEvent.copy(box, { clipboardData: clipboard({}) });
    // Left to the textarea: it inserts the text itself, as text.
    expect(pasteLong(box, typed)).toBe(true);
    expect(screen.getByTestId("box-host").querySelector(".pasted-text-tag")).toBeNull();
  });
});
