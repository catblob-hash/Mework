import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  clampSelectionToRegion,
  installSelectionRegions,
  paintedTextRanges,
  selectionRegion
} from "./selectionRegions";

function text(id: string, index = 0): Text {
  return document.getElementById(id)!.childNodes[index] as Text;
}

function selection(): Selection {
  return document.getSelection()!;
}

describe("selection regions", () => {
  beforeEach(() => {
    document.head.innerHTML = `<style>
      body { -webkit-user-select: none; user-select: none; }
      .region { -webkit-user-select: text; user-select: text; }
      .label { -webkit-user-select: none; user-select: none; }
    </style>`;
    document.body.innerHTML = `
      <article id="one">
        <div class="region" id="a"><p id="a1">First message.</p><p id="a2">Second <span class="label">label</span>paragraph.</p></div>
      </article>
      <article id="two">
        <div class="region" id="b"><p id="b1">Other message.</p></div>
      </article>
      <textarea id="field">typed</textarea>`;
  });

  afterEach(() => {
    selection().removeAllRanges();
    document.head.innerHTML = "";
    document.body.innerHTML = "";
  });

  it("takes a region to be the outermost element of a selectable subtree", () => {
    expect(selectionRegion(text("a1"))).toBe(document.getElementById("a"));
    expect(selectionRegion(document.querySelector(".label"))).toBeNull();
    expect(selectionRegion(document.getElementById("one"))).toBeNull();
  });

  it("stops a selection dragged forward into the next region at the end of its own", () => {
    selection().setBaseAndExtent(text("a1"), 2, text("b1"), 3);

    expect(clampSelectionToRegion(selection())).toBe(true);
    expect(selection().anchorNode).toBe(text("a1"));
    expect(selection().anchorOffset).toBe(2);
    expect(selection().focusNode).toBe(text("a2", 2));
    expect(selection().focusOffset).toBe("paragraph.".length);
  });

  it("stops a selection dragged backward out of its region at the region's first word", () => {
    selection().setBaseAndExtent(text("b1"), 3, text("a1"), 2);

    expect(clampSelectionToRegion(selection())).toBe(true);
    expect(selection().focusNode).toBe(text("b1"));
    expect(selection().focusOffset).toBe(0);
  });

  it("leaves a selection that stays inside its region alone", () => {
    selection().setBaseAndExtent(text("a1"), 2, text("a2", 2), 4);

    expect(clampSelectionToRegion(selection())).toBe(false);
    expect(selection().focusNode).toBe(text("a2", 2));
  });

  it("paints the selected words and none of the text that opted back out", () => {
    const range = document.createRange();
    range.setStart(text("a1"), 2);
    range.setEnd(text("a2", 2), 4);

    expect(paintedTextRanges(range).map(String)).toEqual(["rst message.", "Second ", "para"]);
  });

  it("does not paint the placeholder space that holds a blank line open", () => {
    document.body.innerHTML = `<div class="region" id="lines"><div><span>one</span></div><div><span> </span></div><div><span>three</span></div></div>`;
    const range = document.createRange();
    range.selectNodeContents(document.getElementById("lines")!);

    expect(paintedTextRanges(range).map(String)).toEqual(["one", "three"]);
  });

  it("takes a named region whole, across the unselectable space between its boxes", () => {
    document.body.innerHTML = `
      <div id="pages" data-selection-region>
        <div class="page"><div class="region" id="p1"><span id="p1a">Page one.</span></div></div>
        <div class="page"><div class="region" id="p2"><span id="p2a">Page two.</span></div></div>
      </div>
      <div class="region" id="after"><span id="after1">Outside.</span></div>`;

    expect(selectionRegion(text("p2a"))).toBe(document.getElementById("pages"));
    expect(selectionRegion(document.querySelector(".page"))).toBeNull();
    selection().setBaseAndExtent(text("p1a"), 2, text("p2a"), 4);
    expect(clampSelectionToRegion(selection())).toBe(false);
    selection().setBaseAndExtent(text("p1a"), 2, text("after1"), 3);
    expect(clampSelectionToRegion(selection())).toBe(true);
    expect(selection().focusNode).toBe(text("p2a"));
  });

  describe("installed", () => {
    let uninstall: () => void = () => {};
    beforeEach(() => {
      uninstall = installSelectionRegions();
    });
    afterEach(() => uninstall());

    it("selects all of the region holding the caret, and nothing past it", () => {
      selection().collapse(text("a1"), 3);
      const event = new KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true, cancelable: true });
      document.getElementById("a1")!.dispatchEvent(event);

      expect(event.defaultPrevented).toBe(true);
      expect(selection().anchorNode).toBe(text("a1"));
      expect(selection().anchorOffset).toBe(0);
      expect(selection().focusNode).toBe(text("a2", 2));
      expect(selection().focusOffset).toBe("paragraph.".length);
    });

    it("selects nothing with Select All when no region holds the caret", () => {
      const event = new KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true, cancelable: true });
      document.getElementById("one")!.dispatchEvent(event);

      expect(event.defaultPrevented).toBe(true);
      expect(selection().rangeCount).toBe(0);
    });

    it("leaves Select All in a text field to the field", () => {
      const field = document.getElementById("field") as HTMLTextAreaElement;
      field.focus();
      const event = new KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true, cancelable: true });
      field.dispatchEvent(event);

      expect(event.defaultPrevented).toBe(false);
    });

    it("lets go of a selection when the press lands on something that is not text", () => {
      selection().setBaseAndExtent(text("a1"), 0, text("a1"), 5);
      document.getElementById("two")!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));

      expect(selection().rangeCount).toBe(0);
    });

    it("starts no selection outside a region, nor later in a drag that began outside one", () => {
      const selectStart = (target: EventTarget) => {
        const event = new Event("selectstart", { bubbles: true, cancelable: true });
        target.dispatchEvent(event);
        return event.defaultPrevented;
      };
      expect(selectStart(document.getElementById("one")!)).toBe(true);
      expect(selectStart(text("a1"))).toBe(false);
      expect(selectStart(document.getElementById("field")!)).toBe(false);

      document.getElementById("two")!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
      expect(selectStart(text("b1"))).toBe(true);
      document.getElementById("two")!.dispatchEvent(new MouseEvent("pointerup", { bubbles: true, button: 0 }));
      expect(selectStart(text("b1"))).toBe(false);
    });

    it("makes a press in one column the column a selection runs down", () => {
      document.head.insertAdjacentHTML("beforeend", `<style>
        [data-selection-column-active="old"] [data-selection-column="new"],
        [data-selection-column-active="new"] [data-selection-column="old"] { -webkit-user-select: none; user-select: none; }
      </style>`);
      document.body.insertAdjacentHTML("beforeend", `
        <div class="region" id="code" data-selection-columns>${[
          '<span id="old1" data-selection-column="old">old one</span><span id="new1" data-selection-column="new">new one</span>',
          '<span id="old2" data-selection-column="old">old two</span><span id="new2" data-selection-column="new">new two</span>'
        ].join("")}</div>`);
      const code = document.getElementById("code")!;
      const press = (id: string, init: MouseEventInit = {}) => document.getElementById(id)!
        .dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0, ...init }));

      press("old1");
      expect(code.getAttribute("data-selection-column-active")).toBe("old");
      const range = document.createRange();
      range.setStart(text("old1"), 0);
      range.setEnd(text("old2"), 3);
      expect(paintedTextRanges(range).map(String)).toEqual(["old one", "old"]);

      // The other column was unselectable until this press, and is the one it picks.
      press("new2");
      expect(code.getAttribute("data-selection-column-active")).toBe("new");
      expect(selectionRegion(text("new2"))).toBe(code);
      press("old2", { shiftKey: true });
      expect(code.getAttribute("data-selection-column-active")).toBe("new");

      press("one");
      expect(code.hasAttribute("data-selection-column-active")).toBe(false);
    });

    it("keeps the selection when the press lands on text, where the engine starts the next one", () => {
      selection().setBaseAndExtent(text("a1"), 0, text("a1"), 5);
      document.getElementById("b1")!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));

      expect(selection().toString()).toBe("First");
    });
  });
});
