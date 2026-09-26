import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import conversationCss from "../styles/conversation.css?raw";
import { CAT_LID_LIFT, CAT_NEAR_PAW_OVERLAP, CAT_WRIST } from "./catArt";
import { CAT_MOODS, CAT_MOOD_CYCLE_MS, StreamWaitingIndicator, catDwellChoicesMs } from "./StreamWaitingIndicator";
import type { CatMood } from "./StreamWaitingIndicator";

/**
 * Taken from the component rather than restated, so a third mood makes every
 * guard below cover it instead of silently skipping it.
 */
const MOODS = CAT_MOODS;

/** Longest dwell any mood can draw, so a single advance always expires one. */
const LONGEST_DWELL_MS = Math.max(...MOODS.flatMap((mood) => catDwellChoicesMs(mood)));

/** The lids close with this much to spare past the eye; `catArt.test.ts` checks the geometry at exactly this depth. */
const LID_OVERSHOOT = 8;

/**
 * Every rule that places something under one mood, as the part it names and its
 * declaration block. Kept deliberately literal: jsdom never loads the
 * stylesheet, so reading the source is the only way these tests can see the
 * animations at all.
 */
function moodRules(mood: CatMood): { part: string; block: string }[] {
  const pattern = new RegExp(`\\.stream-waiting__cat--${mood} \\.([\\w-]+)\\s*\\{([^}]*)\\}`, "g");
  return [...conversationCss.matchAll(pattern)].map((match) => ({ part: match[1]!, block: match[2]! }));
}

/** The declaration block of the one base rule for a part, outside any mood. */
function baseRule(part: string): string {
  const pattern = new RegExp(`(?:^|[\\n}])([^{}]*\\.${part}\\b[^{}]*)\\{([^}]*)\\}`, "g");
  const blocks = [...conversationCss.matchAll(pattern)]
    .filter((match) => !match[1]!.includes("stream-waiting__cat--"))
    .map((match) => match[2]!);
  return blocks.join(";");
}

/** Each loop a mood runs: the part, the keyframes and the duration. */
function moodLoops(mood: CatMood): { part: string; name: string; durationMs: number }[] {
  return moodRules(mood).flatMap(({ part, block }) => {
    const match = block.match(/animation:\s*([\w-]+)\s+(\d*\.?\d+)(ms|s)\b/);
    if (!match) return [];
    const duration = Number(match[2]) * (match[3] === "s" ? 1000 : 1);
    return [{ part, name: match[1]!, durationMs: duration }];
  });
}

/** The body of one `@keyframes` block, brace-matched so nested rules survive. */
function keyframeBody(name: string): string {
  const start = conversationCss.indexOf(`@keyframes ${name} `);
  if (start < 0) return "";
  const opening = conversationCss.indexOf("{", start);
  let depth = 1;
  let index = opening + 1;
  while (depth > 0 && index < conversationCss.length) {
    if (conversationCss[index] === "{") depth += 1;
    else if (conversationCss[index] === "}") depth -= 1;
    index += 1;
  }
  return conversationCss.slice(opening + 1, index - 1);
}

/** Every stop of a keyframes block, with the transform it holds there. */
function keyframeStops(name: string): { stop: number; transform: string }[] {
  return [...keyframeBody(name).matchAll(/([\d%,\s]+)\{\s*transform:\s*([^;}]+);?\s*\}/g)].flatMap((match) =>
    match[1]!
      .split(",")
      .map((stop) => stop.trim())
      .filter(Boolean)
      .map((stop) => ({ stop: Number.parseFloat(stop), transform: match[2]!.trim() }))
  );
}

/** The numbers each transform function in a value is called with, by function name. */
function transformCalls(value: string): { name: string; args: number[] }[] {
  return [...value.matchAll(/([a-zA-Z]+)\(([^)]*)\)/g)].map((match) => ({
    name: match[1]!,
    args: match[2]!.split(",").map((arg) => Number.parseFloat(arg))
  }));
}

/** Whether a transform leaves its element exactly where it would be with none. */
function atRest(value: string): boolean {
  return transformCalls(value).every(({ name, args }) =>
    args.every((arg) => (name.startsWith("scale") ? arg === 1 : arg === 0))
  );
}

/** How far down a transform moves its element, in px. */
function translateYOf(value: string | undefined): number {
  if (!value) return 0;
  return transformCalls(value).reduce((sum, { name, args }) => {
    if (name === "translateY") return sum + args[0]!;
    if (name === "translate") return sum + (args[1] ?? 0);
    return sum;
  }, 0);
}

/** How far across a transform moves its element, in px. */
function translateXOf(value: string | undefined): number {
  if (!value) return 0;
  return transformCalls(value).reduce((sum, { name, args }) => (
    name === "translateX" || name === "translate" ? sum + args[0]! : sum
  ), 0);
}

/** The transform a mood poses a part in, when it poses it at all. */
function moodPose(mood: CatMood, part: string): string | undefined {
  const rule = moodRules(mood).find((entry) => entry.part === part && !entry.block.includes("animation"));
  return rule?.block.match(/transform:\s*([^;]+)/)?.[1]?.trim();
}

function catElement(container: HTMLElement): SVGSVGElement {
  return container.querySelector(".stream-waiting__cat")!;
}

/** The group the machine measures and checks for visibility. */
function rootGroup(container: HTMLElement): SVGGElement {
  return catElement(container).querySelector(".stream-cat")!;
}

function moodOf(container: HTMLElement): string {
  return catElement(container).getAttribute("data-cat-mood")!;
}

/**
 * Reports the mood's loops as being `elapsed` ms into a `period` ms cycle, the
 * way the Web Animations API would. jsdom has no `getAnimations`, so the
 * component's alignment step is otherwise never exercised.
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

  it("narrates the tasks still running as a line that opens the tasks pane", () => {
    const onOpenTasks = vi.fn();
    const { container, getByRole } = render(
      <StreamWaitingIndicator
        contexts={[]}
        tools={[]}
        thinking={{ startedAt: "2026-09-04T00:00:00.000Z" }}
        runningTaskCount={2}
        onOpenTasks={onOpenTasks}
      />
    );

    const link = getByRole("button", { name: "2 个任务正在运行" });
    // Below what the round is doing, not instead of it.
    expect(container.querySelector(".stream-waiting__activities")!.lastElementChild).toBe(link);
    expect(container.querySelector(".stream-waiting__activity-title")).toHaveTextContent("正在思考");
    expect(container.querySelector("[data-stream-waiting]")).toHaveAttribute("aria-label", "正在思考；2 个任务正在运行");
    act(() => link.click());
    expect(onOpenTasks).toHaveBeenCalledTimes(1);
  });

  it("draws the task line with nothing else to say, and none without a pane to open", () => {
    const { container, getByRole, rerender } = render(
      <StreamWaitingIndicator contexts={[]} tools={[]} runningTaskCount={1} onOpenTasks={() => {}} />
    );

    expect(getByRole("button", { name: "1 个任务正在运行" })).toBeInTheDocument();
    expect(container.querySelector("[data-stream-waiting]")).toHaveAttribute("aria-label", "模型正在生成；1 个任务正在运行");

    rerender(<StreamWaitingIndicator contexts={[]} tools={[]} runningTaskCount={1} />);
    expect(container.querySelector(".stream-waiting__tasks")).toBeNull();
    rerender(<StreamWaitingIndicator contexts={[]} tools={[]} runningTaskCount={0} onOpenTasks={() => {}} />);
    expect(container.querySelector(".stream-waiting__tasks")).toBeNull();
  });

  it("starts the round at work, and names its mood on the drawing", () => {
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);

    const cat = catElement(container);
    expect(moodOf(container)).toBe("work");
    // The mood modifier is the only channel the stylesheet reads; a drift between
    // the attribute and the class leaves a frozen cat that still passes a
    // presence check.
    expect(cat).toHaveClass("stream-waiting__cat--work");
    expect(cat).not.toHaveClass("stream-waiting__cat--slack");
  });

  it("dwells only for whole numbers of the mood's own loop period", () => {
    for (const mood of MOODS) {
      const choices = catDwellChoicesMs(mood);
      expect(choices.length, `${mood} has no dwell`).toBeGreaterThan(0);
      // A dwell that is not a whole number of cycles expires with the loops
      // mid-move, and the change then has to wait out the rest of the cycle.
      for (const dwell of choices) expect(dwell % CAT_MOOD_CYCLE_MS[mood]).toBe(0);
    }
  });

  it("keeps every dwell long enough not to distract and short enough to be seen changing", () => {
    // Absolute bounds on purpose: derived ones move with the constants they are
    // meant to police.
    for (const mood of MOODS) {
      for (const dwell of catDwellChoicesMs(mood)) {
        expect(dwell, `${mood} dwell too short`).toBeGreaterThanOrEqual(4_000);
        expect(dwell, `${mood} dwell too long`).toBeLessThanOrEqual(15_000);
      }
      // More than one dwell per mood, or the timing is a metronome.
      expect(catDwellChoicesMs(mood).length, `${mood} has one dwell`).toBeGreaterThan(1);
    }
    // The round beside it is working, so the cat mostly is too.
    expect(catDwellChoicesMs("work")[0]).toBeGreaterThan(catDwellChoicesMs("slack")[0]!);
  });

  it("waits exactly the dwell it drew, then changes to the other mood", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    // Math.random pinned to 0 picks the first choice for the mood on screen.
    const dwell = catDwellChoicesMs("work")[0]!;

    await act(async () => {
      vi.advanceTimersByTime(dwell - 1);
    });
    expect(moodOf(container)).toBe("work");
    await act(async () => {
      vi.advanceTimersByTime(1);
    });
    // Pins the delay handed to the timer, not just the helper that computes it.
    expect(moodOf(container)).toBe("slack");
    expect(catElement(container)).toHaveClass("stream-waiting__cat--slack");
    expect(catElement(container)).not.toHaveClass("stream-waiting__cat--work");
  });

  it("waits for its loops to come round to rest before changing", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const period = CAT_MOOD_CYCLE_MS.work;
    const dwell = catDwellChoicesMs("work")[0]!;

    // The loops are reported half a cycle behind the timer, which is what a late
    // timer or a spell of `display: none` leaves behind.
    let elapsed = period / 2;
    stubMoodLoop(container, period, () => elapsed);

    await act(async () => {
      vi.advanceTimersByTime(dwell);
    });
    // Changing here would tear the loops off mid-move, so the machine holds.
    expect(moodOf(container)).toBe("work");

    elapsed = period;
    await act(async () => {
      vi.advanceTimersByTime(period / 2);
    });
    expect(moodOf(container)).toBe("slack");
  });

  it("holds still while the cat is not laid out, and recovers without an event", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    // App.tsx hides the whole conversation pane behind any open task or preview
    // page, and a `display: none` subtree transitions nothing.
    let laidOut = false;
    stubRendered(container, () => laidOut);

    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    expect(moodOf(container)).toBe("work");

    // Nothing announces a pane being shown again, so the machine has to find its
    // own way back rather than waiting for an event that never comes.
    laidOut = true;
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS);
    });
    expect(moodOf(container)).toBe("slack");
  });

  it("alternates between its two moods for as long as the round lasts", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const seen = [moodOf(container)];
    for (let step = 0; step < 5; step += 1) {
      await act(async () => {
        vi.advanceTimersByTime(LONGEST_DWELL_MS);
      });
      seen.push(moodOf(container));
    }
    // The machine re-arms after every change; stalling or skipping shows up here.
    expect(seen).toEqual(["work", "slack", "work", "slack", "work", "slack"]);
  });

  it("never changes mood while reduced motion is on", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    stubReducedMotion(true);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    // The global kill switch cuts the pose transition to .01ms, which would make
    // every change a jump cut.
    expect(moodOf(container)).toBe("work");
  });

  it("stops changing mood when reduced motion is turned on", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const reduced = stubReducedMotion(false);

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(catDwellChoicesMs("work")[0]! / 2);
      reduced.set(true);
    });
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    expect(moodOf(container)).toBe("work");
  });

  it("holds still while the document is hidden and starts a fresh dwell on return", async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const visibility = stubVisibility("hidden");

    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(LONGEST_DWELL_MS * 4);
    });
    // A hidden document suspends animation, so a change made there is one nobody
    // saw happen.
    expect(moodOf(container)).toBe("work");

    await act(async () => {
      visibility.set("visible");
    });
    // Coming back must not immediately spend the deadline that expired while
    // hidden; the mood only moves after a whole fresh dwell.
    await act(async () => {
      vi.advanceTimersByTime(catDwellChoicesMs("work")[0]! - 1);
    });
    expect(moodOf(container)).toBe("work");
    await act(async () => {
      vi.advanceTimersByTime(1);
    });
    expect(moodOf(container)).toBe("slack");
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

  it("draws the paws, pupils and lids over the face, and keeps the laptop out of the breath", () => {
    const { container } = render(<StreamWaitingIndicator contexts={[]} tools={[]} />);
    const cat = catElement(container);
    const order = [...cat.querySelectorAll("[class^='stream-cat-']")].map((node) => node.getAttribute("class")!);
    const at = (name: string) => order.findIndex((entry) => entry.split(" ").includes(name));

    // The overlays are the head's own colour laid over it: under the face they
    // would be hidden by it, and the lids have to come down over the pupils.
    for (const overlay of ["stream-cat-paw--far", "stream-cat-paw--near", "stream-cat-pupils", "stream-cat-blink"]) {
      expect(at(overlay), overlay).toBeGreaterThan(at("stream-cat-body"));
    }
    expect(at("stream-cat-blink")).toBeGreaterThan(at("stream-cat-pupils"));

    // A laptop or a ledge that breathed with the cat would read as held, not lain against.
    const figure = cat.querySelector(".stream-cat-figure")!;
    expect(figure.querySelector(".stream-cat-body")).not.toBeNull();
    expect(figure.querySelector(".stream-cat-laptop")).toBeNull();
    expect(figure.querySelector(".stream-cat-ledge")).toBeNull();
  });

  it("gives each mood its own loops, all on one shared period", () => {
    for (const mood of MOODS) {
      const loops = moodLoops(mood);
      expect(loops.length, `${mood} has no motion`).toBeGreaterThan(2);
      const period = CAT_MOOD_CYCLE_MS[mood];
      for (const { part, name, durationMs } of loops) {
        expect(keyframeBody(name), `${mood}: @keyframes ${name} is missing`).not.toBe("");
        // A loop that does not divide the period is somewhere mid-move when the
        // others come round to rest, and gets torn off there by the change.
        expect(period % durationMs, `${mood}: ${part} runs ${durationMs}ms`).toBe(0);
      }
      // One of them has to be the whole period: it is the clock the change is
      // aligned against.
      expect(loops.some(({ durationMs }) => durationMs === period), `${mood} has no ${period}ms loop`).toBe(true);
    }
  });

  it("starts and ends every loop at rest, so a change never tears one off mid-move", () => {
    for (const mood of MOODS) {
      for (const { part, name } of moodLoops(mood)) {
        const stops = keyframeStops(name);
        for (const edge of [0, 100]) {
          const frame = stops.find(({ stop }) => stop === edge);
          expect(frame, `${name} declares no ${edge}%`).toBeDefined();
          expect(atRest(frame!.transform), `${mood}: ${part} is at ${frame!.transform} on ${edge}%`).toBe(true);
        }
        // And actually moves in between, or it is a loop in name only.
        expect(stops.some(({ transform }) => !atRest(transform)), `${name} never moves`).toBe(true);
      }
    }
  });

  it("eases the pose across a change, with the eyes on the screen only at work", () => {
    const posed = new Set(MOODS.flatMap((mood) => moodRules(mood).filter((rule) => !rule.block.includes("animation")).map((rule) => rule.part)));
    expect(posed.size).toBeGreaterThan(0);
    for (const part of posed) {
      // A pose with no transition snaps the moment the class changes.
      expect(baseRule(part), `${part} is posed but never transitions`).toMatch(/transition:\s*transform\b/);
    }
    // The laptop is to the cat's left as drawn — the viewer's right — so looking at
    // it is looking right.
    expect(translateXOf(moodPose("work", "stream-cat-gaze"))).toBeGreaterThan(0);
    expect(translateXOf(moodPose("slack", "stream-cat-gaze"))).toBeLessThan(0);
  });

  it("types and mouses only at work", () => {
    const moving = (mood: CatMood) => new Set(moodLoops(mood).map(({ part }) => part));
    expect(moving("work")).toContain("stream-cat-paw--far");
    expect(moving("work")).toContain("stream-cat-paw--near");
    expect(moving("slack")).not.toContain("stream-cat-paw--far");
    expect(moving("slack")).not.toContain("stream-cat-paw--near");
    // Both moods wag the tail.
    for (const mood of MOODS) expect(moving(mood)).toContain("stream-cat-tail");
  });

  it("turns the typing paw about the wrist it was cut round", () => {
    // The cut is an arc about CAT_WRIST; turning the paw about anywhere else
    // opens the joint as soon as it lifts.
    const origin = baseRule("stream-cat-paw--far").match(/transform-origin:\s*([\d.]+)px\s+([\d.]+)px/);
    expect(origin).not.toBeNull();
    expect(Number(origin![1])).toBe(CAT_WRIST.x);
    expect(Number(origin![2])).toBe(CAT_WRIST.y);
    // It only ever lifts: pressing down would push it through the keys.
    for (const { transform } of keyframeStops("stream-cat-type")) {
      for (const { name, args } of transformCalls(transform)) {
        expect(name).toBe("rotate");
        expect(args[0]).toBeLessThanOrEqual(0);
      }
    }
  });

  it("slides the mouse paw no further than its overlap covers", () => {
    // Past the overlap, the cut behind the paw opens as a slot in the chest.
    const loop = moodLoops("work").find(({ part }) => part === "stream-cat-paw--near")!;
    for (const { transform } of keyframeStops(loop.name)) {
      expect(Math.abs(translateXOf(transform))).toBeLessThan(CAT_NEAR_PAW_OVERLAP);
    }
  });

  it("closes the eyes fully on every blink, and never lowers the lids further", () => {
    for (const mood of MOODS) {
      const pose = translateYOf(moodPose(mood, "stream-cat-lids"));
      // A drowsy lid still leaves some eye showing, and a working one none.
      expect(pose, `${mood} lids`).toBeGreaterThanOrEqual(0);
      expect(pose, `${mood} lids`).toBeLessThan(CAT_LID_LIFT);
      const blink = moodLoops(mood).find(({ part }) => part === "stream-cat-blink");
      expect(blink, `${mood} never blinks`).toBeDefined();
      const deepest = Math.max(...keyframeStops(blink!.name).map(({ transform }) => translateYOf(transform)));
      // Short of this the eye's outline stays as a ghost; past it the lid runs
      // beyond the depth catArt.test.ts checks it against.
      expect(pose + deepest, `${mood} blink`).toBe(CAT_LID_LIFT + LID_OVERSHOOT);
    }
    expect(translateYOf(moodPose("work", "stream-cat-lids"))).toBe(0);
    expect(translateYOf(moodPose("slack", "stream-cat-lids"))).toBeGreaterThan(0);
  });
});
