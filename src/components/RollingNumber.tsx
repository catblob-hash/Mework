import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import "./RollingNumber.css";

/**
 * One roll, in milliseconds. The stylesheet declares the same figure; the timer
 * here only has to outlast it, so the two are allowed to be read separately as
 * long as this one is never the shorter.
 */
export const ROLLING_NUMBER_MS = 420;

/** Slack after the transition before the overlay is taken down. */
const ROLLING_NUMBER_SETTLE_MS = 80;

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/**
 * Two full turns of the wheel. A digit never advances by more than nine cells in
 * one roll, and every roll starts from a cell in the first turn, so eighteen is
 * the furthest any of them can reach.
 */
const CELLS = Array.from({ length: 20 }, (_, index) => index % 10);

type Slot =
  | { kind: "digit"; key: string; from: number; to: number }
  | { kind: "glyph"; key: string; text: string };

function isDigit(character: string | undefined): boolean {
  return character !== undefined && character >= "0" && character <= "9";
}

/**
 * Pairs the outgoing text against the incoming one from the right, the way an
 * odometer's wheels line up: the seconds column stays the seconds column when
 * `59s` becomes `1m0s`, and the digit that appears on the left is simply new.
 *
 * Only the incoming text has slots. A column the number outgrew is gone, which
 * is what the shrinking width already says.
 *
 * Runs of non-digits stay whole. They are laid out beside the wheels rather than
 * inside them, and a word broken into one box per letter loses the kerning the
 * text underneath still has — which is visible as a shimmer at exactly the
 * moment the overlay is supposed to be invisible.
 */
export function rollingSlots(previous: string, next: string): Slot[] {
  const columns: Slot[] = [];
  for (let column = 0; column < next.length; column += 1) {
    const character = next[next.length - 1 - column]!;
    const before = previous[previous.length - 1 - column];
    const key = `column-${column}`;
    if (!isDigit(character)) {
      columns.push({ kind: "glyph", key, text: character });
      continue;
    }
    // A column that was not a digit a moment ago has nothing to roll from, so it
    // arrives already showing its value rather than spinning up from a zero it
    // never held.
    const from = isDigit(before) ? Number(before) : Number(character);
    columns.push({ kind: "digit", key, from, to: Number(character) });
  }
  const slots: Slot[] = [];
  for (const slot of columns.reverse()) {
    const previousSlot = slots[slots.length - 1];
    if (slot.kind === "glyph" && previousSlot?.kind === "glyph") {
      slots[slots.length - 1] = { ...previousSlot, text: `${previousSlot.text}${slot.text}` };
      continue;
    }
    slots.push(slot);
  }
  return slots;
}

/** Where the wheel stops: always forward, so a carry rolls 9 → 0 rather than back through the whole wheel. */
function landing(slot: Extract<Slot, { kind: "digit" }>): number {
  return slot.from + ((slot.to - slot.from + 10) % 10);
}

/**
 * Whether a roll would actually be drawn.
 *
 * The global reduced-motion kill switch squashes every transition to .01ms, so
 * the wheel would jump rather than turn — worse than not rolling at all, because
 * the plain text is hidden while it does. A hidden document is the same story
 * with a different cause. A missing `matchMedia` — jsdom has none — reports
 * nothing, which is not a reduction, so it does not block.
 */
function rollCanPlay(): boolean {
  if (typeof document !== "undefined" && document.visibilityState !== "visible") return false;
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return true;
  return !window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

export interface RollingNumberProps {
  /** The already-formatted text. Digits roll; every other character is carried across unchanged. */
  value: string;
  className?: string;
  title?: string;
}

/**
 * A number whose digits turn like the wheels of a combination lock when it
 * changes, instead of snapping to the new value.
 *
 * The plain text is always in the DOM and is the only thing there between rolls:
 * it defines the width, it is what gets selected and copied, and it is what a
 * test or a screen reader reads. The wheels are a short-lived overlay drawn from
 * CSS generated content on top of it, so they contribute no text of their own
 * and disappear again once the roll is over.
 */
export function RollingNumber({ value, className, title }: RollingNumberProps) {
  const [roll, setRoll] = useState<{ slots: Slot[]; armed: boolean } | null>(null);
  const settled = useRef(value);

  useEffect(() => {
    if (settled.current === value) return;
    const previous = settled.current;
    settled.current = value;
    setRoll(rollCanPlay() ? { slots: rollingSlots(previous, value), armed: false } : null);
  }, [value]);

  // Arm on the next frame, never in the same commit: a wheel that mounts already
  // on its landing cell has no start state to transition away from.
  useEffect(() => {
    if (!roll || roll.armed) return undefined;
    const frame = window.requestAnimationFrame(() => {
      setRoll((current) => (current && !current.armed ? { ...current, armed: true } : current));
    });
    return () => window.cancelAnimationFrame(frame);
  }, [roll]);

  // Take the overlay down on a timer rather than on `transitionend`. Every wheel
  // fires one, a wheel that had nothing to turn fires none at all, and the
  // overlay must come down in both cases.
  useEffect(() => {
    if (!roll?.armed) return undefined;
    const timer = window.setTimeout(() => setRoll(null), ROLLING_NUMBER_MS + ROLLING_NUMBER_SETTLE_MS);
    return () => window.clearTimeout(timer);
  }, [roll]);

  return (
    <span
      className={`rolling-number${roll ? " rolling-number--rolling" : ""}${className ? ` ${className}` : ""}`}
      title={title}
    >
      <span className="rolling-number__value">{value}</span>
      {roll && (
        <span className="rolling-number__roll" aria-hidden="true">
          {roll.slots.map((slot) => (slot.kind === "glyph" ? (
            <span key={slot.key} className="rolling-number__glyph" data-glyph={slot.text} />
          ) : (
            <span key={slot.key} className="rolling-number__digit">
              <span
                className="rolling-number__cells"
                style={{ "--rolling-offset": roll.armed ? landing(slot) : slot.from } as CSSProperties}
              >
                {CELLS.map((digit, index) => (
                  <span key={`cell-${index}`} className="rolling-number__cell" data-digit={digit} />
                ))}
              </span>
            </span>
          )))}
        </span>
      )}
    </span>
  );
}
