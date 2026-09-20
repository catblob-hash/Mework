import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import conversationCss from "../styles/conversation.css?raw";
import { CAT_EYES, CAT_HEAD_CENTER } from "./catArt";
import { STREAM_CAT_BONES, STREAM_CAT_FIGURE, STREAM_CAT_JOINTS, STREAM_CAT_LAPTOP } from "./streamCatRig";
import type { StreamCatPart } from "./streamCatRig";
import {
  CAT_MOODS,
  CAT_MOOD_CYCLE_MS,
  CAT_TRANSITION_MS,
  StreamWaitingIndicator,
  catDwellChoicesMs
} from "./StreamWaitingIndicator";

/**
 * Taken from the component rather than restated, so adding a fourth mood makes
 * every guard below cover it instead of silently skipping it.
 */
const MOODS = CAT_MOODS;

/** Longest dwell any mood can draw, so a single advance always expires one. */
const LONGEST_DWELL_MS = Math.max(...MOODS.flatMap((mood) => catDwellChoicesMs(mood)));

/**
 * The declaration block of every rule whose selector mentions `needle`, as raw
 * CSS text. Kept deliberately literal: jsdom never loads the stylesheet, so
 * reading the source is the only way these tests can see the animations at all.
 */
function rulesMentioning(needle: string): string[] {
  const pattern = new RegExp(`${needle.replace(/[.*+?^${}()|[\\]\\\\]/g, "\\\\$&")}[^{]*\\{([^}]*)\\}`, "g");
  return [...conversationCss.matchAll(pattern)].map((match) => match[1]!);
}

function declaredDurationsMs(block: string): number[] {
  return [...block.matchAll(/animation(?:-duration)?:[^;]*?(\d*\.?\d+)(ms|s)\b/g)]
    .map((match) => (match[2] === "s" ? Number(match[1]) * 1000 : Number(match[1])));
}

function catElement(container: HTMLElement): SVGSVGElement {
  return container.querySelector(".stream-waiting__cat")!;
}

/** The element the beat's own animation runs on, and the one the machine watches. */
function rootGroup(container: HTMLElement): SVGGElement {
  return catElement(container).querySelector(".stream-cat")!;
}

/** The body of one `@keyframes` block, brace-matched so nested rules survive. */
function keyframeBody(name: string): string {
  const opening = conversationCss.indexOf(`{`, conversationCss.indexOf(`@keyframes ${name}`));
  let depth = 1;
  let index = opening + 1;
  while (depth > 0 && index < conversationCss.length) {
    if (conversationCss[index] === "{") depth += 1;
    else if (conversationCss[index] === "}") depth -= 1;
    index += 1;
  }
  return conversationCss.slice(opening + 1, index - 1);
}

/** Whether a keyframe body ever leaves the neutral pose. */
function movesAwayFromNeutral(body: string): boolean {
  for (const call of body.matchAll(/(rotate|translateX|translateY|scaleX|scaleY)\(\s*(-?[\d.]+)/g)) {
    const value = Number(call[2]);
    if (call[1]!.startsWith("scale") ? value !== 1 : value !== 0) return true;
  }
  return false;
}

function moodOf(container: HTMLElement): string {
  return catElement(container).getAttribute("data-cat-mood")!;
}

function phaseOf(container: HTMLElement): string {
  return catElement(container).getAttribute("data-cat-phase")!;
}

/**
 * Ends one animation the way a browser does. The component listens natively, so
 * this is the same path the app takes; `AnimationEvent` itself comes from the
 * stand-in in the test setup, because jsdom ships none.
 */
function endAnimation(element: Element, animationName: string): void {
  element.dispatchEvent(new AnimationEvent("animationend", { animationName, bubbles: true }));
}

/** Ends the beat the way a browser does: on the root group, by name. */
function finishBeat(container: HTMLElement): void {
  const target = catElement(container).getAttribute("data-cat-next-mood");
  endAnimation(rootGroup(container), `stream-cat-to-${target}`);
}

/**
 * Reports the mood loop as being `elapsed` ms into a `period` ms cycle, the way
 * the Web Animations API would. jsdom has no `getAnimations`, so the component's
 * alignment step is otherwise never exercised.
 */
function stubMoodLoop(container: HTMLElement, period: number, elapsed: () => number) {
  // Cast through unknown: the stand-in only carries the two properties the
  // component reads, which is not enough to satisfy the real Animation type.
  const group = rootGroup(container) as unknown as { getAnimations: () => unknown[] };
  group.getAnimations = () => [{
    get currentTime() {
      return elapsed();
    },
    effect: { getTiming: () => ({ duration: period }) }
  }];
}

/** Reports the cat as laid out or not, the way `display: none` would. */
function stubRendered(container: HTMLElement, rendered: () => boolean) {
  const group = rootGroup(container) as SVGGElement & { checkVisibility: () => boolean };
  group.checkVisibility = () => rendered();
}

/** Drives one whole mood change: dwell out, then let the beat finish. */
async function advanceOneMoodChange(container: HTMLElement): Promise<void> {
  await act(async () => {
    vi.advanceTimersByTime(LONGEST_DWELL_MS);
  });
  await act(async () => {
    finishBeat(container);
  });
}

/**
 * Installs a controllable `prefers-reduced-motion` query. jsdom 29 has no
 * `matchMedia` at all, so without this the component sees the "nothing reported"
 * branch — which is exactly what the other tests exercise.
 */
function stubReducedMotion(initial: boolean) {
  const listeners = new Set<() => void>();
  const media = {
    matches: initial,
    addEventListener: (_: string, listener: () => void) => void listeners.add(listener),
    removeEventListener: (_: string, listener: () => void) => void listeners.delete(listener)
  };
  vi.stubGlobal("matchMedia", vi.fn(() => media));
  return {
    media,
    set(matches: boolean) {
      media.matches = matches;
      for (const listener of listeners) listener();
    },
    listenerCount: () => listeners.size
  };
}

function stubVisibility(initial: DocumentVisibilityState) {
  let value = initial;
  const spy = vi.spyOn(document, "visibilityState", "get").mockImplementation(() => value);
  return {
    set(next: DocumentVisibilityState) {
      value = next;
      document.dispatchEvent(new Event("visibilitychange"));
    },
    spy
  };
}

describe("StreamWaitingIndicator", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Encrypted reasoning has no card in the timeline while it runs, so this line
   * is the only place the user can see that the model is thinking at all. It has
   * to look like the line a tool call gets, because from outside they are the
   * same kind of wait.
   */
  it("narrates live reasoning beside the cat in the same shape as a tool call", () => {
    const { container } = render(
      <StreamWaitingIndicator
        contexts={[]}
        tools={[]}
        thinking={{ startedAt: "2026-09-04T00:00:00.000Z", tokens: 1_240 }}
      />
    );

    const activity = container.querySelector(".stream-waiting__activity")!;
    expect(activity.querySelector(".stream-waiting__activity-title")).toHaveTextContent("正在思考");
    // The verb pulses; the count beside it does not, exactly as with a tool's target.
    expect(activity.querySelector(".stream-waiting__activity-title")).toHaveClass("pulse-text");
    expect(activity.querySelector(".stream-waiting__activity-count")).toHaveTextContent("1.2k");
    expect(activity.querySelector(".stream-waiting__dots")).toBeInTheDocument();
    expect(container.querySelector("[data-stream-waiting]")).toHaveAttribute("aria-label", "正在思考");
  });

  /** Most providers only report reasoning tokens at the end of the round; an
   * absent count must show nothing rather than a zero. */
  it("omits the token count until the provider reports one", () => {
    const { container } = render(
      <StreamWaitingIndicator contexts={[]} tools={[]} thinking={{ startedAt: "2026-09-04T00:00:00.000Z" }} />
    );

    expect(container.querySelector(".stream-waiting__activity-title")).toHaveTextContent("正在思考");
    expect(container.querySelector(".stream-waiting__activity-count")).toBeNull();
  });

  /** Nothing thinking and nothing running leaves the cat alone with its
   * screen-reader label, which is what it did before this line existed. */
  it("says nothing beside the cat when no reasoning is running", () => {
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);

    expect(container.querySelector(".stream-waiting__activity")).toBeNull();
    expect(container.querySelector("[data-stream-waiting]")).not.toHaveAttribute("data-stream-thinking");
  });

  it("marks the live round with a cat holding one of the three moods", () => {
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);

    const cat = catElement(container);
    const mood = cat.getAttribute("data-cat-mood")!;
    expect(MOODS).toContain(mood);
    expect(cat).toHaveAttribute("data-cat-phase", "hold");
    // The mood modifier is the only channel the keyframes read; a drift between
    // the attribute and the class leaves a frozen cat that still passes a
    // presence check.
    expect(cat).toHaveClass(`stream-waiting__cat--${mood}`);
    expect(cat).not.toHaveAttribute("data-cat-next-mood");
  });

  it("holds a mood for whole loop cycles before proposing a change", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const shortest = Math.min(...MOODS.map((mood) => catDwellChoicesMs(mood)[0]!));

    // A twitchy mascot would pull the eye off the streaming text, so even the
    // shortest dwell the range can draw has to outlast a couple of loop cycles.
    expect(shortest).toBeGreaterThanOrEqual(3_000);
    await act(async () => {
      vi.advanceTimersByTime(shortest - 100);
    });
    expect(phaseOf(container)).toBe("hold");

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");
  });

  it("dwells only for whole numbers of the mood's own loop period", () => {
    for (const mood of MOODS) {
      const choices = catDwellChoicesMs(mood);
      expect(choices.length, `${mood} has no dwell`).toBeGreaterThan(0);
      // A dwell that is not a whole number of cycles expires with the mood
      // mid-stride, and the beat then has to snap to neutral before it can start
      // — the jump cut the beat exists to remove.
      for (const dwell of choices) expect(dwell % CAT_MOOD_CYCLE_MS[mood]).toBe(0);
    }
  });

  it("keeps every dwell inside an ambient window", () => {
    // Absolute bounds on purpose. Derived ones move with the constants they are
    // meant to police, so widening the window back towards the old 9-18s — the
    // thing this change set out to shorten — would go unnoticed.
    for (const mood of MOODS) {
      for (const dwell of catDwellChoicesMs(mood)) {
        expect(dwell, `${mood} dwell too short`).toBeGreaterThanOrEqual(3_000);
        expect(dwell, `${mood} dwell too long`).toBeLessThanOrEqual(7_200);
      }
    }
    // Every mood also has to be able to draw more than one dwell, or its timing
    // is a metronome.
    for (const mood of MOODS) expect(catDwellChoicesMs(mood).length, `${mood} has one dwell`).toBeGreaterThan(1);
  });

  it("waits exactly the dwell it drew", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    // Math.random pinned to 0 picks the first choice for the mood on screen.
    const dwell = catDwellChoicesMs(moodOf(container) as (typeof MOODS)[number])[0]!;

    await act(async () => {
      vi.advanceTimersByTime(dwell - 1);
    });
    expect(phaseOf(container)).toBe("hold");
    await act(async () => {
      vi.advanceTimersByTime(1);
    });
    // Pins the delay handed to the timer, not just the helper that computes it.
    expect(phaseOf(container)).toBe("transition");
  });

  it("waits for the mood loop to reach neutral before starting the beat", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const mood = moodOf(container) as (typeof MOODS)[number];
    const period = CAT_MOOD_CYCLE_MS[mood];
    const dwell = catDwellChoicesMs(mood)[0]!;

    // The loop is reported half a cycle behind the timer, which is what a late
    // timer or a spell of `display: none` leaves behind.
    let elapsed = period / 2;
    stubMoodLoop(container, period, () => elapsed);

    await act(async () => {
      vi.advanceTimersByTime(dwell);
    });
    // Switching here would tear the loop off mid-pose, so the machine holds.
    expect(phaseOf(container)).toBe("hold");

    elapsed = period;
    await act(async () => {
      vi.advanceTimersByTime(period / 2);
    });
    expect(phaseOf(container)).toBe("transition");
  });

  it("holds still while the cat is not laid out, and recovers without an event", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);
    // App.tsx hides the whole conversation pane behind any open task or preview
    // page, and a `display: none` subtree runs no animation at all.
    let laidOut = false;
    stubRendered(container, () => laidOut);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    expect(phaseOf(container)).toBe("hold");
    expect(moodOf(container)).toBe(before);

    // Nothing announces a pane being shown again, so the machine has to find its
    // own way back rather than waiting for an event that never comes.
    laidOut = true;
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");
  });

  it("runs a beat toward the next mood without committing it yet", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });

    const cat = catElement(container);
    const target = cat.getAttribute("data-cat-next-mood")!;
    expect(MOODS).toContain(target);
    expect(target).not.toBe(before);
    // The cat has not changed mood yet, and the mood loop is off so it cannot
    // fight the beat for the same `transform`.
    expect(moodOf(container)).toBe(before);
    expect(cat).toHaveClass(`stream-waiting__cat--to-${target}`);
    expect(cat).not.toHaveClass(`stream-waiting__cat--${before}`);
  });

  it("commits the new mood only when the beat's own animation ends", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    const target = catElement(container).getAttribute("data-cat-next-mood")!;

    // Time alone must not settle it: a timer firing says time passed, not that
    // anything was drawn.
    await act(async () => {
      vi.advanceTimersByTime(CAT_TRANSITION_MS + 20);
    });
    expect(phaseOf(container)).toBe("transition");
    expect(moodOf(container)).toBe(before);

    await act(async () => {
      finishBeat(container);
    });
    expect(moodOf(container)).toBe(target);
    expect(phaseOf(container)).toBe("hold");
    expect(catElement(container)).toHaveClass(`stream-waiting__cat--${target}`);
    expect(catElement(container)).not.toHaveClass(`stream-waiting__cat--to-${target}`);
  });

  it("ignores animation ends from other parts of the cat and other animations", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    const cat = catElement(container);
    const target = cat.getAttribute("data-cat-next-mood")!;

    await act(async () => {
      // Right name, wrong element: every part of the cat is animated and its
      // events bubble through the root group's handler.
      endAnimation(cat.querySelector(".stream-cat-tail")!, `stream-cat-to-${target}`);
      // Right element, wrong animation.
      endAnimation(cat.querySelector(".stream-cat")!, "stream-cat-bob");
    });
    expect(phaseOf(container)).toBe("transition");
  });

  it("aborts back to the mood it had when the beat never ends", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");

    await act(async () => {
      vi.advanceTimersByTime(CAT_TRANSITION_MS * 4);
    });
    // Giving up must put the cat back, never commit a swap that was not drawn.
    expect(phaseOf(container)).toBe("hold");
    expect(moodOf(container)).toBe(before);
  });

  it("reaches every mood and never repeats the one already showing", async () => {
    vi.useFakeTimers();
    // Smallest ring step, so the walk visits the moods in declaration order.
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);

    const seen = [moodOf(container)];
    for (let step = 0; step < MOODS.length; step += 1) {
      await advanceOneMoodChange(container);
      seen.push(moodOf(container));
    }
    // Every mood reachable, no mood following itself, and the machine still
    // armed after a full lap — dropping a mood or stalling both show up here.
    expect(new Set(seen)).toEqual(new Set(MOODS));
    expect(seen.slice(1).every((value, index) => value !== seen[index])).toBe(true);
  });

  it("never leaves the mood it has while reduced motion is on", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    stubReducedMotion(true);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    // The global kill switch squashes the beat to .01ms, so it would still fire
    // its events almost at once — the swap has to be refused up front instead.
    expect(phaseOf(container)).toBe("hold");
    expect(moodOf(container)).toBe(before);
  });

  it("abandons a beat when reduced motion is turned on mid-transition", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const reduced = stubReducedMotion(false);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");

    await act(async () => {
      reduced.set(true);
    });
    expect(phaseOf(container)).toBe("hold");
    expect(moodOf(container)).toBe(before);

    // A completion arriving after the abort must not resurrect the swap.
    await act(async () => {
      endAnimation(rootGroup(container), `stream-cat-to-${before === "walk" ? "groom" : "walk"}`);
    });
    expect(moodOf(container)).toBe(before);
  });

  it("refuses a completion that races ahead of the reduced-motion listener", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const reduced = stubReducedMotion(false);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");

    // Flip the preference WITHOUT notifying the listener, so React still thinks
    // the beat is playable. That is the real ordering: the kill switch squashes
    // the beat to .01ms and its completion can land before the media event has
    // been processed. Only re-reading the query at commit time catches it.
    reduced.media.matches = true;
    await act(async () => {
      finishBeat(container);
    });
    expect(moodOf(container)).toBe(before);
  });

  it("holds still while the document is hidden and starts a fresh dwell on return", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const visibility = stubVisibility("hidden");

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const before = moodOf(container);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    // A hidden document suspends animation, so a swap made there is one nobody
    // saw happen.
    expect(phaseOf(container)).toBe("hold");
    expect(moodOf(container)).toBe(before);

    await act(async () => {
      visibility.set("visible");
    });
    // Coming back must not immediately spend the deadline that expired while
    // hidden; the mood only moves after a whole fresh dwell.
    expect(phaseOf(container)).toBe("hold");
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");
  });

  it("drops its media and visibility listeners on unmount", () => {
    const reduced = stubReducedMotion(false);
    const removeDocumentListener = vi.spyOn(document, "removeEventListener");

    const { unmount } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    expect(reduced.listenerCount()).toBeGreaterThan(0);

    unmount();
    expect(reduced.listenerCount()).toBe(0);
    expect(removeDocumentListener).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
  });

  it("gives every mood its own animation in the stylesheet", () => {
    // jsdom never loads the stylesheet, so the moods the component names could
    // all be silently unanimated. Read the source instead: this is the only
    // guard that a mood still has motion attached to it.
    const keyframes = new Set([...conversationCss.matchAll(/@keyframes\s+([\w-]+)/g)].map((match) => match[1]));

    for (const mood of MOODS) {
      const rules = [...conversationCss.matchAll(
        new RegExp(`\\.stream-waiting__cat--${mood}\\b[^{]*\\{([^}]*)\\}`, "g")
      )];
      const names = rules.flatMap((rule) => [...rule[1]!.matchAll(/animation(?:-name)?:\s*([\w-]+)/g)].map((m) => m[1]));
      expect(names.length, `${mood} has no animated part`).toBeGreaterThan(0);
      for (const name of names) expect(keyframes, `${mood} -> ${name}`).toContain(name);
      // The mood selectors must not also sweep up the `--to-<mood>` beat rules:
      // the `\b` in the pattern above matches before a hyphen, so a beat named
      // `--<mood>-in` would be read as part of the mood and silently checked as
      // if it looped.
      for (const rule of rules) expect(rule[0]).not.toContain(`--to-${mood}`);
    }
  });

  it("keeps every part of a mood on one shared loop period", () => {
    for (const mood of MOODS) {
      const durations = rulesMentioning(`.stream-waiting__cat--${mood} `).flatMap(declaredDurationsMs);
      expect(durations.length, `${mood} declares no duration`).toBeGreaterThan(0);
      // If one part's loop does not divide the mood period, that part is not
      // back on the neutral pose when the dwell expires, and the beat starts by
      // snapping it there.
      for (const duration of durations) {
        expect(CAT_MOOD_CYCLE_MS[mood] % duration, `${mood} has a ${duration}ms part`).toBe(0);
      }
      // And one part must run for the whole period: that animation is the clock
      // the component reads to find the loop boundary. Without it there is
      // nothing to align against and the dwell falls back to trusting timers.
      expect(durations, `${mood} has no full-period part`).toContain(CAT_MOOD_CYCLE_MS[mood]);
    }
  });

  it("gives every destination a one-shot beat the component can wait on", () => {
    const keyframes = new Set([...conversationCss.matchAll(/@keyframes\s+([\w-]+)/g)].map((match) => match[1]));

    for (const mood of MOODS) {
      // The component commits on the beat named after the destination, fired by
      // the root group. Both halves of that contract live here.
      expect(keyframes, `no beat toward ${mood}`).toContain(`stream-cat-to-${mood}`);
      const rootRule = rulesMentioning(`.stream-waiting__cat--to-${mood} .stream-cat `);
      expect(rootRule.length, `${mood} beat is not on the root group`).toBeGreaterThan(0);
      expect(rootRule[0]).toContain(`stream-cat-to-${mood}`);

      const blocks = rulesMentioning(`.stream-waiting__cat--to-${mood} `);
      for (const block of blocks) {
        // An infinite beat never ends, so the mood would never commit.
        expect(block, `${mood} beat loops`).not.toContain("infinite");
        // A beat longer than the watchdog would be aborted every single time.
        for (const duration of declaredDurationsMs(block)) expect(duration).toBe(CAT_TRANSITION_MS);
      }
    }
  });

  it("makes every beat keyframe actually leave the neutral pose", () => {
    // Names and durations can all be right while the beat is a 560ms hold. The
    // machine would still commit on it, and the swap would be the jump cut it
    // was supposed to replace — with a wait in front of it.
    const beats = [...conversationCss.matchAll(/@keyframes\s+(stream-cat-to-[\w-]+)/g)].map((match) => match[1]!);
    expect(beats.length, "no beat keyframes at all").toBeGreaterThanOrEqual(MOODS.length);
    for (const beat of beats) {
      const body = keyframeBody(beat);
      expect(movesAwayFromNeutral(body), `${beat} never leaves neutral`).toBe(true);
      // And it has to hand the cat back where the mood loop picks it up.
      expect(body, `${beat} does not start neutral`).toMatch(/0%[^{]*\{[^}]*\}/);
    }
  });

  it("keeps both bodies clear of the near eye", () => {
    // The head is one even-odd path, so its eyes are holes rather than paint: any part of
    // the silhouette drawn under one plugs it and the cat goes blind, with nothing in the
    // source overlapping to show for it. Two shapes are drawn under the head — the standing
    // body and the seated one — and each clears the eyes on a different axis.
    //
    // jsdom lays out no SVG, so both numbers are read off the markup. The paths are written
    // with absolute cubic segments only, and a cubic never leaves the hull of its control
    // points, so the extreme control point is a safe over-estimate of the real edge.
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const body = container.querySelector(".stream-cat-body");
    const seat = container.querySelector(".stream-cat-seat");
    const face = container.querySelector(".stream-cat-face");
    expect(body).not.toBeNull();
    expect(seat).not.toBeNull();
    expect(face).not.toBeNull();

    const placement = face!.getAttribute("transform") ?? "";
    const [, tx, ty, scale] = placement.match(/translate\(([-\d.]+) ([-\d.]+)\) scale\(([-\d.]+)\)/) ?? [];
    expect(scale, `unreadable head transform: ${placement}`).toBeDefined();
    const eyeX = [...CAT_EYES.matchAll(/(-?[\d.]+) (-?[\d.]+)/g)].map((match) => Number(match[1]));
    const eyeY = [...CAT_EYES.matchAll(/(-?[\d.]+) (-?[\d.]+)/g)].map((match) => Number(match[2]));
    const eyeLeft = Number(tx) + Number(scale) * (Math.min(...eyeX) - CAT_HEAD_CENTER.x);
    const eyeBottom = Number(ty) + Number(scale) * (Math.max(...eyeY) - CAT_HEAD_CENTER.y);

    // The standing body reaches up into the band the eyes occupy, so it has to stop short
    // of them along x.
    const points = (element: Element) =>
      [...element.getAttribute("d")!.matchAll(/(-?[\d.]+) (-?[\d.]+)/g)].map((match) => ({
        x: Number(match[1]),
        y: Number(match[2])
      }));
    const bodyRight = Math.max(...points(body!).map((point) => point.x));
    expect(bodyRight, `body reaches x=${bodyRight}, eye starts at x=${eyeLeft}`).toBeLessThan(eyeLeft - 0.5);

    // The seated body is taller and comes further forward than that, and clears the eyes by
    // staying below them instead.
    const seatTop = Math.min(...points(seat!).map((point) => point.y));
    expect(seatTop, `seat reaches y=${seatTop}, eyes end at y=${eyeBottom}`).toBeGreaterThan(eyeBottom + 0.5);
  });

  it("hangs every tube bone on the joint it turns about, at the width its neighbour ends on", () => {
    // Both of these fail silently, and only in motion.
    //
    // A bone is drawn as a tube through a run of discs, and the disc at a joint is what fuses
    // it to the bone above: a circle centred exactly on the pivot is the one shape rotation
    // cannot move, so the two stay welded at any angle. Move the first node off the joint and
    // the resting pose is unchanged — the parts still overlap — but the limb tears open at the
    // ends of its swing. Change its radius without changing its neighbour's and the two discs
    // no longer coincide, which puts a step in the outline: that step, three times over, is
    // the beads-on-a-string tail this drawing replaced.
    //
    // Neither is visible in a still frame of the neutral pose, which is all a snapshot sees.
    for (const bone of STREAM_CAT_BONES) {
      expect(bone.run.length, `${bone.className} is not a run at all`).toBeGreaterThan(1);
      for (const node of bone.run) expect(node.r, `${bone.className} has a node of width ${node.r}`).toBeGreaterThan(0);

      const joint = STREAM_CAT_JOINTS[bone.className];
      if (joint) {
        const [first] = bone.run;
        expect(
          { x: first!.x, y: first!.y },
          `${bone.className} starts at (${first!.x}, ${first!.y}) but turns about (${joint.x}, ${joint.y})`
        ).toEqual(joint);
      }

      // Whatever hangs off this bone's far end has to pick it up at the same width. Matching
      // by position rather than by a declared chain: the drawing is what says which bones meet.
      const last = bone.run[bone.run.length - 1]!;
      for (const other of STREAM_CAT_BONES) {
        if (other === bone) continue;
        const [start] = other.run;
        if (start!.x !== last.x || start!.y !== last.y) continue;
        expect(
          start!.r,
          `${other.className} starts ${start!.r} wide where ${bone.className} ends ${last.r} wide`
        ).toBe(last.r);
      }
    }
  });

  it("pivots every moving part on the joint the rig names for it", () => {
    // A pivot that drifts away from its joint is the quietest failure the cat has: the limb
    // still moves, still for the right length of time, just around the wrong point — an
    // elbow that swings from the shoulder reads as a broken toy, and nothing here would
    // otherwise notice. The rig owns the geometry, so the stylesheet is checked against it.
    // Every class the cat can put on screen: the rig's own two trees — the laptop is only
    // mounted while the cat is dealing with it, and its hinge is a joint like any other —
    // plus the root group, which the component owns because it carries the beat.
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const classes = new Set([
      ...[...container.querySelectorAll("[class]")].flatMap((element) =>
        element.getAttribute("class")!.split(/\s+/)
      ),
      ...[STREAM_CAT_LAPTOP, ...STREAM_CAT_FIGURE].flatMap(function walk(part: StreamCatPart): string[] {
        return [...(part.className ?? "").split(/\s+/).filter(Boolean), ...(part.children ?? []).flatMap(walk)];
      })
    ]);

    for (const [name, joint] of Object.entries(STREAM_CAT_JOINTS)) {
      expect(classes, `${name} has a joint but nothing renders with that class`).toContain(name);
      // The class appears in more than one rule — every moving part is also in the
      // `transform-box` list — so this takes the rule that sets an origin at all.
      const origin = [...conversationCss.matchAll(new RegExp(`\\.${name}\\s*\\{([^}]*)\\}`, "g"))]
        .map((rule) => rule[1]!.match(/transform-origin:\s*(-?[\d.]+)px\s+(-?[\d.]+)px/))
        .find(Boolean);
      expect(origin, `${name} declares no transform-origin`).toBeTruthy();
      expect(Number(origin![1]), `${name} pivots on x=${origin![1]}, joint is at ${joint.x}`).toBe(joint.x);
      expect(Number(origin![2]), `${name} pivots on y=${origin![2]}, joint is at ${joint.y}`).toBe(joint.y);
    }
  });

  it("renders exactly the rig, node for node", () => {
    // The silhouette is verified in a browser harness that renders this same tree out of the
    // rig module. Anything the component drew that the rig does not carry would be invisible
    // to that harness — the drawing would be verified and the application still wrong.
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const figure = container.querySelector(".stream-cat")!;

    const compare = (part: StreamCatPart, node: Element, path: string) => {
      expect(node.tagName, `${path}: wrong element`).toBe(part.d === undefined ? "g" : "path");
      expect(node.getAttribute("class"), `${path}: class`).toBe(part.className ?? null);
      expect(node.getAttribute("d"), `${path}: d`).toBe(part.d ?? null);
      expect(node.getAttribute("transform"), `${path}: transform`).toBe(part.transform ?? null);
      expect(node.getAttribute("opacity"), `${path}: opacity`).toBe(
        part.opacity === undefined ? null : String(part.opacity)
      );
      expect(node.getAttribute("fill-rule"), `${path}: fill-rule`).toBe(part.fillRule ?? null);
      const children = part.children ?? [];
      expect(node.children.length, `${path}: child count`).toBe(children.length);
      children.forEach((child, index) => {
        compare(child, node.children[index]!, `${path}/${child.className ?? index}`);
      });
    };

    expect(figure.children.length).toBe(STREAM_CAT_FIGURE.length);
    STREAM_CAT_FIGURE.forEach((part, index) => {
      compare(part, figure.children[index]!, part.className ?? String(index));
    });
  });

  it("mounts the laptop for the whole of the cat's business with it", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const laptop = () => container.querySelector(".stream-cat-laptop");
    expect(moodOf(container)).not.toBe("laptop");
    expect(laptop(), "a laptop with no cat business is a prop lying in the timeline").toBeNull();

    // It has to be on screen before the mood it belongs to is committed, because the beat
    // into that mood is where the cat knocks it shut — a laptop that arrived afterwards
    // would be slammed by nothing.
    for (let lap = 0; lap < CAT_MOODS.length; lap += 1) {
      await act(async () => {
        vi.advanceTimersByTime(LONGEST_DWELL_MS);
      });
      expect(phaseOf(container)).toBe("transition");
      if (catElement(container).getAttribute("data-cat-next-mood") === "laptop") break;
      expect(laptop(), "a laptop during a beat that is going somewhere else").toBeNull();
      await act(async () => {
        finishBeat(container);
      });
    }
    expect(catElement(container).getAttribute("data-cat-next-mood")).toBe("laptop");
    expect(moodOf(container)).not.toBe("laptop");
    expect(laptop(), "no laptop to slam during the beat toward it").not.toBeNull();

    await act(async () => {
      finishBeat(container);
    });
    expect(moodOf(container)).toBe("laptop");
    expect(laptop()).not.toBeNull();

    // And it has to stay for the beat out, which is the one that puts it away.
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(phaseOf(container)).toBe("transition");
    expect(moodOf(container)).toBe("laptop");
    expect(laptop(), "nothing left to put away on the way out").not.toBeNull();

    await act(async () => {
      finishBeat(container);
    });
    expect(moodOf(container)).not.toBe("laptop");
    expect(laptop()).toBeNull();
  });

  it("lands the seated beat on the pose the seated mood holds", () => {
    // The seated mood is the one mood whose resting pose is not the standing one, which is
    // the whole reason it needs rules of its own. Three things have to agree on that pose,
    // and none of them is derived from the others: the pose rules, the 100% frame of the
    // beat that arrives on it, and the frames the mood's own loop starts and ends on. Where
    // they disagree the cat twitches on the exact frame the beat commits — a single frame,
    // which is precisely what nobody notices while looking for it.
    const pose = new Map<string, string>();
    for (const match of conversationCss.matchAll(
      /\.stream-waiting__cat\[data-cat-mood="laptop"\]\s+\.([\w-]+)\s*\{\s*transform:\s*([^;]+);/g
    )) {
      // Later rules win at equal specificity, so the last one for a class is its pose.
      pose.set(match[1]!, match[2]!.trim());
    }
    expect(pose.size, "the seated mood declares no pose at all").toBeGreaterThan(3);

    const animations = (selector: string) =>
      [...conversationCss.matchAll(new RegExp(`${selector}\\s+\\.([\\w-]+)\\s*\\{\\s*animation:\\s*([\\w-]+)`, "g"))].map(
        (match) => ({ part: match[1]!, keyframes: match[2]! })
      );
    const frame = (keyframes: string, stop: string) => {
      for (const match of keyframeBody(keyframes).matchAll(/([\d%,\s.]+)\{([^}]*)\}/g)) {
        const stops = match[1]!.split(",").map((value) => value.trim());
        if (stops.includes(stop)) return (match[2]!.match(/transform:\s*([^;]+)/)?.[1] ?? "").replace(/\s+/g, " ").trim();
      }
      return null;
    };
    const same = (left: string | null, right: string | null) =>
      left === right || (!movesAwayFromNeutral(left ?? "") && !movesAwayFromNeutral(right ?? ""));

    const arriving = animations("\\.stream-waiting__cat--to-laptop");
    for (const [part, value] of pose) {
      // A part that is posed differently but not animated into that pose snaps into it the
      // instant the beat ends.
      const beat = arriving.find((entry) => entry.part === part);
      expect(beat, `${part} is posed for sitting but the beat toward it never moves it`).toBeDefined();
      expect(
        same(frame(beat!.keyframes, "100%"), value),
        `${part}: beat ends on ${frame(beat!.keyframes, "100%")}, mood holds ${value}`
      ).toBe(true);
    }
    for (const { part, keyframes } of animations("\\.stream-waiting__cat--laptop")) {
      const rest = pose.get(part) ?? "none";
      for (const stop of ["0%", "100%"]) {
        expect(
          same(frame(keyframes, stop), rest === "none" ? null : rest),
          `${part}: ${keyframes} is at ${frame(keyframes, stop)} on ${stop}, mood rests on ${rest}`
        ).toBe(true);
      }
    }
  });
});
