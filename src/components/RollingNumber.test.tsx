import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import rollingCss from "./RollingNumber.css?raw";
import { RollingNumber, ROLLING_NUMBER_MS, rollingSlots } from "./RollingNumber";

/**
 * The declaration block of every rule whose selector mentions `needle`. jsdom
 * never loads the stylesheet, so reading the source is the only way these tests
 * can see the animation at all.
 */
function rulesMentioning(needle: string): string[] {
  const pattern = new RegExp(`${needle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[^{]*\\{([^}]*)\\}`, "g");
  return [...rollingCss.matchAll(pattern)].map((match) => match[1]!);
}

function offsets(container: HTMLElement): (string | null)[] {
  return [...container.querySelectorAll<HTMLElement>(".rolling-number__cells")]
    .map((cells) => cells.style.getPropertyValue("--rolling-offset") || null);
}

describe("rollingSlots", () => {
  /**
   * Columns line up from the right, the way an odometer's wheels do: the seconds
   * column stays the seconds column when the minutes appear beside it.
   */
  it("pairs columns from the right so a widening number keeps its places", () => {
    expect(rollingSlots("9s", "10s")).toEqual([
      { kind: "digit", key: "column-2", from: 1, to: 1 },
      { kind: "digit", key: "column-1", from: 9, to: 0 },
      { kind: "glyph", key: "column-0", text: "s" }
    ]);
  });

  it("rolls each digit column of a compound duration on its own", () => {
    expect(rollingSlots("1m23s", "1m24s")).toEqual([
      { kind: "digit", key: "column-4", from: 1, to: 1 },
      { kind: "glyph", key: "column-3", text: "m" },
      { kind: "digit", key: "column-2", from: 2, to: 2 },
      { kind: "digit", key: "column-1", from: 3, to: 4 },
      { kind: "glyph", key: "column-0", text: "s" }
    ]);
  });

  /** The leading column held a letter — or nothing — a moment ago, so it has
   * nothing to roll from and arrives already showing its value instead of
   * spinning up from a zero it never held. */
  it("gives a column with no previous digit nothing to roll", () => {
    expect(rollingSlots("59s", "1m0s")).toEqual([
      { kind: "digit", key: "column-3", from: 1, to: 1 },
      { kind: "glyph", key: "column-2", text: "m" },
      // The seconds column is still the seconds column, so it really does roll.
      { kind: "digit", key: "column-1", from: 9, to: 0 },
      { kind: "glyph", key: "column-0", text: "s" }
    ]);
  });

  /**
   * A word split into one box per letter loses the kerning the text underneath
   * keeps, and the two copies then disagree by a fraction of a pixel per letter
   * — a shimmer at exactly the moment the overlay is meant to be invisible.
   */
  it("keeps a run of non-digits whole instead of one box per letter", () => {
    expect(rollingSlots("1.2k tokens", "8.7k tokens")).toEqual([
      { kind: "digit", key: "column-10", from: 1, to: 8 },
      { kind: "glyph", key: "column-9", text: "." },
      { kind: "digit", key: "column-8", from: 2, to: 7 },
      { kind: "glyph", key: "column-7", text: "k tokens" }
    ]);
  });
});

describe("RollingNumber", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  /**
   * The plain text is the only thing in the DOM between rolls. Everything that
   * reads this number — a selection, a copy, a screen reader, a test — reads
   * that text, so it must never be replaced by the wheels.
   */
  it("keeps the value as plain text and draws no wheels while it is not rolling", () => {
    const { container } = render(<RollingNumber value="1m23s" />);

    expect(container.querySelector(".rolling-number__value")).toHaveTextContent("1m23s");
    expect(container.querySelector(".rolling-number__roll")).toBeNull();
    expect(container.querySelector(".rolling-number")).not.toHaveClass("rolling-number--rolling");
  });

  it("turns the wheels forward when the value changes, then takes them down again", () => {
    vi.useFakeTimers();
    const { container, rerender } = render(<RollingNumber value="8" />);

    rerender(<RollingNumber value="1" />);
    // The wheel mounts on the cell it is leaving; arming it in the same commit
    // would give the transition no start state to run from.
    expect(offsets(container)).toEqual(["8"]);

    act(() => {
      vi.advanceTimersToNextFrame();
    });
    // Forward through the carry — 8 → 9 → 0 → 1 — never backwards round the wheel.
    expect(offsets(container)).toEqual(["11"]);
    expect(container.querySelector(".rolling-number")).toHaveClass("rolling-number--rolling");
    // Whatever the wheels are showing, the text underneath already reads the new value.
    expect(container.querySelector(".rolling-number__value")).toHaveTextContent("1");

    act(() => {
      vi.advanceTimersByTime(ROLLING_NUMBER_MS * 2);
    });
    expect(container.querySelector(".rolling-number__roll")).toBeNull();
  });

  /**
   * The wheels carry no text of their own: their glyphs are CSS generated
   * content. Anything else would put a second copy of the number in the DOM and
   * hand it to selections and text queries alongside the real one.
   */
  it("draws the wheels from generated content rather than text nodes", () => {
    vi.useFakeTimers();
    const { container, rerender } = render(<RollingNumber value="8" />);
    rerender(<RollingNumber value="9" />);

    expect(container.querySelector(".rolling-number__roll")).toHaveTextContent("");
    for (const cell of container.querySelectorAll(".rolling-number__cell")) {
      expect(cell.textContent).toBe("");
      expect(cell).toHaveAttribute("data-digit");
    }
    expect(rulesMentioning(".rolling-number__cell::before").join()).toContain("attr(data-digit)");
  });

  /** The global kill switch squashes the transition to .01ms, which would turn
   * the roll into the jump it exists to replace — with the value hidden while it
   * happened. Refuse the roll up front instead. */
  it("does not roll at all under reduced motion", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({
      matches: true,
      addEventListener: () => undefined,
      removeEventListener: () => undefined
    })));
    const { container, rerender } = render(<RollingNumber value="8" />);

    rerender(<RollingNumber value="9" />);

    expect(container.querySelector(".rolling-number__roll")).toBeNull();
    expect(container.querySelector(".rolling-number__value")).toHaveTextContent("9");
    // And the stylesheet has to agree, for a preference that flips mid-roll.
    expect(rollingCss).toMatch(/@media \(prefers-reduced-motion: reduce\)/);
    expect(rulesMentioning(".rolling-number__roll").join()).toContain("display: none");
  });

  /** The wheel is a strip of cells one line tall, translated by whole cells.
   * If the cell height and the translation unit ever disagree the digits stop
   * landing on the baseline, and nothing in jsdom would notice. */
  it("translates the strip by exactly the cell height", () => {
    const cell = rulesMentioning(".rolling-number__cell ").join();
    const cells = rulesMentioning(".rolling-number__cells ").join();
    expect(cell).toContain("height: 1lh");
    expect(cells).toContain("var(--rolling-offset, 0) * -1lh");
  });
});
