import { describe, expect, it } from "vitest";
import { isImeKeyEvent, matchesEvent } from "./shortcuts";

const keydown = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);

describe("isImeKeyEvent", () => {
  it("claims the WebKit keydown that commits a composition after compositionend", () => {
    // macOS WKWebView: Enter confirming Pinyin letters arrives with isComposing false.
    expect(isImeKeyEvent(keydown({ key: "Enter", code: "Enter", keyCode: 229 }))).toBe(true);
    expect(isImeKeyEvent(keydown({ key: "Enter", code: "Enter", keyCode: 229, shiftKey: true }))).toBe(true);
  });

  it("claims Chromium composition keystrokes", () => {
    expect(isImeKeyEvent(keydown({ key: "Enter", code: "Enter", isComposing: true }))).toBe(true);
    expect(isImeKeyEvent(keydown({ key: "Process", code: "KeyN" }))).toBe(true);
  });

  it("leaves ordinary Enter and Shift+Enter to the page", () => {
    expect(isImeKeyEvent(keydown({ key: "Enter", code: "Enter", keyCode: 13 }))).toBe(false);
    expect(isImeKeyEvent(keydown({ key: "Enter", code: "Enter", keyCode: 13, shiftKey: true }))).toBe(false);
  });
});

describe("matchesEvent", () => {
  it("keeps the default send and newline bindings apart", () => {
    const enter = keydown({ key: "Enter", code: "Enter" });
    const shiftEnter = keydown({ key: "Enter", code: "Enter", shiftKey: true });
    expect(matchesEvent(["Enter"], enter)).toBe(true);
    expect(matchesEvent(["Enter"], shiftEnter)).toBe(false);
    expect(matchesEvent(["Shift", "Enter"], shiftEnter)).toBe(true);
    expect(matchesEvent(["Shift", "Enter"], enter)).toBe(false);
  });
});
