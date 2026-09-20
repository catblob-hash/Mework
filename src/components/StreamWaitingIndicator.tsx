import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { ContextItem, ToolDescriptor } from "../types";
import { useI18n } from "../i18n";
import { formatCompactTokenCount } from "../lib/contextTokens";
import type { LiveReasoningView } from "../lib/runContexts";
import { RollingNumber } from "./RollingNumber";
import { STREAM_CAT_FIGURE, STREAM_CAT_LAPTOP, STREAM_CAT_VIEWBOX } from "./streamCatRig";
import type { StreamCatPart } from "./streamCatRig";
import { getToolPresentation, isHoistedToolCall } from "./ToolRenderers";

type Translate = ReturnType<typeof useI18n>["t"];

export const CAT_MOODS = ["walk", "groom", "idle", "laptop"] as const;
export type CatMood = (typeof CAT_MOODS)[number];

/**
 * Loop period of each mood: the shortest interval after which every part of that
 * mood is back on its 0% keyframe, which for all of them is that mood's resting
 * pose. Every `animation` duration under `.stream-waiting__cat--<mood>` in
 * conversation.css divides the entry here, and at least one of them equals it
 * — that one is the clock the dwell below is aligned against.
 */
export const CAT_MOOD_CYCLE_MS: Record<CatMood, number> = {
  walk: 2_200,
  groom: 3_400,
  idle: 3_400,
  laptop: 3_400
};

/** One transition beat. Mirrors the `.72s` the beat rules declare. */
export const CAT_TRANSITION_MS = 720;

/**
 * The one mood the cat does not stand up in.
 *
 * Every other mood rests on the same neutral standing pose, which is what lets a
 * beat be named after where it is going and ignore where it came from. This one
 * rests sitting, with a laptop it has just shut under one paw, so leaving it has
 * to be animated separately from arriving anywhere — see the `from-laptop` rules
 * in conversation.css.
 */
const CAT_SEATED_MOOD: CatMood = "laptop";


/**
 * A beat that never ends would strand the cat between moods, so the machine
 * gives up well after the beat should have finished — and gives up by putting
 * the cat back down on the mood it already had, never by committing a swap that
 * was not animated.
 */
const CAT_TRANSITION_WATCHDOG_MS = CAT_TRANSITION_MS * 2 + 250;

/**
 * Dwell window. Short enough that the cat visibly changes its mind several times
 * during an ordinary round, long enough that it is not competing with the text
 * streaming beside it. These are only the bounds: the dwell actually used is
 * quantised up to whole loop cycles, so the real range is 3.4s-6.8s.
 */
const CAT_DWELL_MIN_MS = 3_200;
const CAT_DWELL_MAX_MS = 7_200;

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/** Close enough to a loop boundary that waiting another frame would show nothing. */
const CAT_ALIGNMENT_TOLERANCE_MS = 16;

/**
 * The dwells available to one mood: every whole number of loop cycles that lands
 * inside the window. A mood whose single cycle already overruns the window gets
 * one cycle rather than a truncated one — cutting a loop short is the jump cut
 * this whole mechanism exists to remove.
 */
export function catDwellChoicesMs(mood: CatMood): number[] {
  const period = CAT_MOOD_CYCLE_MS[mood];
  const first = Math.max(1, Math.ceil(CAT_DWELL_MIN_MS / period));
  const last = Math.max(first, Math.floor(CAT_DWELL_MAX_MS / period));
  const choices: number[] = [];
  for (let count = first; count <= last; count += 1) choices.push(count * period);
  return choices;
}

/**
 * How much longer the mood has to run before it is back on the neutral pose.
 *
 * A dwell that is a whole number of periods is only a whole number of periods on
 * the JavaScript clock. The animation's clock starts at a style resolution this
 * code never sees, timers fire late, and a `display: none` spell restarts the
 * loop at a moment nothing here was told about — so the two drift. Asking the
 * animation itself where it is turns the dwell from an assumption into a
 * measurement.
 *
 * Zero when the answer cannot be measured (no Web Animations API, no animation
 * running, or already on the boundary), which degrades to trusting the clock.
 */
function msUntilNeutral(group: SVGGElement | null, mood: CatMood): number {
  if (!group || typeof group.getAnimations !== "function") return 0;
  const period = CAT_MOOD_CYCLE_MS[mood];
  // The mood declares one animation whose duration is the whole period; every
  // other part divides it, so that one alone says where the pose is.
  const master = group.getAnimations({ subtree: true }).find((animation) => {
    const duration = animation.effect?.getTiming().duration;
    return typeof duration === "number" && Math.round(duration) === period;
  });
  if (!master) return 0;
  const remaining = period - (Number(master.currentTime ?? 0) % period);
  return remaining <= CAT_ALIGNMENT_TOLERANCE_MS || remaining >= period ? 0 : remaining;
}

/**
 * Whether a transition beat would actually be painted right now.
 *
 * Three things stop it. A hidden document suspends animation and throttles
 * timers, so a swap made there is one the user never sees happen. Under
 * `prefers-reduced-motion` the global kill switch in feedback.css forces every
 * animation to .01ms with a single iteration — the beat still fires its events,
 * almost immediately, but no intermediate frame of it ever reaches the screen,
 * which would put the mood change back to being the jump cut it used to be. And
 * the conversation pane is `display: none` behind any open task, subagent or
 * preview page, where a subtree runs no animations at all: the beat would never
 * start, so it could never end.
 *
 * A missing `matchMedia` — jsdom has none at all — reports nothing, which is not
 * the same as reporting a reduction, so it does not block. `checkVisibility` is
 * treated the same way.
 */
function beatCanPlay(group: SVGGElement | null): boolean {
  if (typeof document !== "undefined" && document.visibilityState !== "visible") return false;
  if (group && typeof group.checkVisibility === "function" && !group.checkVisibility()) return false;
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return true;
  return !window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

/** The document-wide half of {@link beatCanPlay}, which is the part that has events. */
function documentAllowsMotion(): boolean {
  return beatCanPlay(null);
}

function useTransitionPlayable(): boolean {
  const [playable, setPlayable] = useState(documentAllowsMotion);
  useEffect(() => {
    const sync = () => setPlayable(documentAllowsMotion());
    sync();
    const media = typeof window.matchMedia === "function" ? window.matchMedia(REDUCED_MOTION_QUERY) : null;
    media?.addEventListener("change", sync);
    document.addEventListener("visibilitychange", sync);
    return () => {
      media?.removeEventListener("change", sync);
      document.removeEventListener("visibilitychange", sync);
    };
  }, []);
  return playable;
}

interface CatMoodState {
  /** The mood the cat is in. A running beat has not changed it yet. */
  mood: CatMood;
  /** Where the beat now running is heading, or null while the cat is just holding a mood. */
  next: CatMood | null;
}

/**
 * Random mood walk, punctuated by a transition beat.
 *
 * The mood is committed by the beat's own `animationend` and by nothing else: a
 * dwell expiring only proposes a target. That is the strictest reading of "only
 * change while the change can be animated", and it is why the watchdog aborts
 * rather than commits — a timer firing is evidence that time passed, not that
 * anything was drawn.
 *
 * Both random draws happen in the effect, never in render or inside a state
 * updater: the app mounts under StrictMode, which replays both, and a replayed
 * draw would commit a choice the timer never made.
 */
function useCatMood(): CatMoodState & { rootGroup: RefObject<SVGGElement | null> } {
  const playable = useTransitionPlayable();
  const [state, setState] = useState<CatMoodState>({ mood: CAT_MOODS[0], next: null });
  const rootGroup = useRef<SVGGElement | null>(null);

  useEffect(() => {
    if (state.next !== null) {
      // Losing playability mid-beat means the rest of it will not be drawn, so
      // put the cat back down on the mood it still has. Re-arming from there is
      // what keeps the next dwell aligned to the loop that just restarted.
      if (!playable) {
        setState({ mood: state.mood, next: null });
        return undefined;
      }
      const watchdog = window.setTimeout(() => setState({ mood: state.mood, next: null }), CAT_TRANSITION_WATCHDOG_MS);
      return () => window.clearTimeout(watchdog);
    }
    if (!playable) return undefined;

    let timer = 0;
    const drawDwell = () => {
      const choices = catDwellChoicesMs(state.mood);
      return choices[Math.floor(Math.random() * choices.length)]!;
    };
    const schedule = (delay: number) => {
      timer = window.setTimeout(() => {
        // Whether the beat can play has to be re-read here, not trusted from the
        // last render. The document and the preference both announce themselves
        // and would have re-rendered eventually, but a pane going `display: none`
        // announces nothing at all — looking again on each dwell is the only way
        // back from it.
        if (!beatCanPlay(rootGroup.current)) {
          schedule(drawDwell());
          return;
        }
        // Land on the loop boundary rather than near it: the dwell counted whole
        // periods on the JavaScript clock, and the animation's clock has its own
        // idea of where it is.
        const wait = msUntilNeutral(rootGroup.current, state.mood);
        if (wait > 0) {
          schedule(wait);
          return;
        }
        // A non-zero step around the ring never lands on the mood already showing;
        // repeating one would read as the animation having stalled.
        const step = 1 + Math.floor(Math.random() * (CAT_MOODS.length - 1));
        setState({ mood: state.mood, next: CAT_MOODS[(CAT_MOODS.indexOf(state.mood) + step) % CAT_MOODS.length]! });
      }, Math.max(0, delay));
    };
    schedule(drawDwell());
    return () => window.clearTimeout(timer);
  }, [playable, state]);

  useEffect(() => {
    const group = rootGroup.current;
    if (!group) return undefined;
    // Deliberately a native listener rather than `onAnimationEnd`. react-dom
    // decides once, at import, which animation event name to listen for, and
    // because jsdom exposes no `AnimationEvent` constructor it settles on
    // `webkitAnimationEnd` there — so a React handler is dead in tests while
    // being live in the app. Listening directly makes both the same.
    const onAnimationEnd = (event: AnimationEvent) => {
      // Only the beat on the root group settles the swap. Every other part of
      // the cat is animated too and its events bubble through here, so both the
      // element and the animation name have to match or a tail flick would
      // commit the mood halfway through the beat.
      if (event.target !== group) return;
      if (!beatCanPlay(group)) return;
      setState((current) => (
        current.next !== null && event.animationName === `stream-cat-to-${current.next}`
          ? { mood: current.next, next: null }
          : current
      ));
    };
    group.addEventListener("animationend", onAnimationEnd);
    return () => group.removeEventListener("animationend", onAnimationEnd);
  }, []);

  return { ...state, rootGroup };
}

/**
 * One node of the rig, and every node under it.
 *
 * The drawing lives in `streamCatRig.ts` as data so that the browser harness the
 * silhouette is verified in renders the same tree this does, rather than its own
 * restatement of it — a harness with its own copy passes while the cat is broken.
 */
function StreamCatPartNode({ part }: { part: StreamCatPart }) {
  const shared = {
    ...(part.className ? { className: part.className } : {}),
    ...(part.transform ? { transform: part.transform } : {}),
    ...(part.opacity !== undefined ? { opacity: part.opacity } : {})
  };
  if (part.d !== undefined) {
    return <path {...shared} d={part.d} {...(part.fillRule ? { fillRule: part.fillRule } : {})} />;
  }
  return (
    <g {...shared}>
      {(part.children ?? []).map((child) => (
        <StreamCatPartNode part={child} key={child.className ?? child.d} />
      ))}
    </g>
  );
}

/**
 * Cat silhouette that stands in for the old pulsing square. Every mood keyframe
 * starts and ends on that mood's resting pose so mood changes — and the global
 * reduced-motion kill switch — land on a cat that is standing, or sitting, but
 * never halfway through a step.
 *
 * The head is the brand cat's own face, lifted out of the drawing the application
 * icon is cut from, on the flat-jawed skull `catArt.ts` cuts for surfaces that
 * hold their head up. Everything below the neck is drawn in `streamCatRig.ts`,
 * because the artwork has no standing cat to borrow it from.
 *
 * Three things about the markup are load-bearing, and all three are pinned by
 * `StreamWaitingIndicator.test.tsx`:
 *
 * - The tree is the rig's, node for node. Nothing may be drawn here that the
 *   harness cannot see.
 * - The laptop is a sibling of the figure, drawn before it, and is mounted for
 *   as long as the cat is either going to the seated mood or still in it — which
 *   is what gives the beat out of that mood something to put away.
 * - While a transition beat runs the mood class is off: the beat and the mood
 *   loop both drive `transform` on the same elements, and only one of them may
 *   own it. `data-cat-mood` keeps naming the mood the cat still has, so the beat
 *   reads as the cat leaving that mood rather than as having already left it —
 *   and so the stylesheet can tell a cat that has to stand up first.
 */
export function StreamWaitingCat() {
  const { mood, next, rootGroup } = useCatMood();
  const phase = next ? `stream-waiting__cat--to-${next}` : `stream-waiting__cat--${mood}`;
  const laptop = mood === CAT_SEATED_MOOD || next === CAT_SEATED_MOOD;
  return (
    <svg
      className={`stream-waiting__cat ${phase}`}
      viewBox={STREAM_CAT_VIEWBOX}
      aria-hidden="true"
      data-cat-mood={mood}
      data-cat-phase={next ? "transition" : "hold"}
      {...(next ? { "data-cat-next-mood": next } : {})}
    >
      {laptop && <StreamCatPartNode part={STREAM_CAT_LAPTOP} />}
      <g className="stream-cat" ref={rootGroup}>
        {STREAM_CAT_FIGURE.map((part) => (
          <StreamCatPartNode part={part} key={part.className ?? part.d} />
        ))}
      </g>
    </svg>
  );
}

export interface StreamWaitingIndicatorProps {
  contexts: ContextItem[];
  tools: ToolDescriptor[];
  /**
   * Reasoning the live round is doing right now. Encrypted reasoning with no
   * summary never reaches the timeline as a card, so this line is the only place
   * it is visible while it happens; plaintext reasoning is narrated here too, so
   * the two read the same from outside.
   */
  thinking?: LiveReasoningView | null;
  /** Transient "request failed, retrying" hint for the live round. */
  retryNotice?: { attempt: number; maxAttempts: number; message: string } | null;
}

/** One in-flight call, reduced to the line the indicator narrates it with. */
interface ToolActivity {
  id: string;
  toolName: string;
  title: string;
  target?: string;
}

/**
 * Every call the timeline hoisted onto this indicator, in timeline order.
 *
 * Usually there is exactly one: the host runs a round's calls one after the
 * other. Async tools are the exception that makes this a list — `web_search`
 * and `web_fetch` are dispatched and only collected at the round's settlement
 * point, so a later call really can be running beside them. Narrating only the
 * newest would leave the others with no surface at all, since their blocks are
 * hoisted too.
 */
function toolActivities(contexts: ContextItem[], tools: ToolDescriptor[], t: Translate): ToolActivity[] {
  const activities: ToolActivity[] = [];
  for (const context of contexts) {
    if (context.kind !== "tool" || !isHoistedToolCall(context)) continue;
    const descriptor = tools.find((candidate) => candidate.name === context.toolName);
    const presentation = getToolPresentation(context, descriptor, t);
    activities.push({
      id: context.id,
      toolName: context.toolName,
      title: presentation.title,
      ...(presentation.target ? { target: presentation.target } : {})
    });
  }
  return activities;
}

/**
 * The waiting line for one call: what it is doing, what it is doing it to, and
 * the dots that say it has not finished.
 *
 * Only the verb pulses. The sweep is a gradient clipped to the glyphs, so
 * everything under it is transparent — the target and the dots stay siblings so
 * they keep painting in their own colour.
 */
function ToolActivityLine({ activity }: { activity: ToolActivity }) {
  return (
    <span className="stream-waiting__activity">
      <span className="stream-waiting__activity-title pulse-text">{activity.title}</span>
      {activity.target && (
        <code className="stream-waiting__activity-target" title={activity.target}>
          {activity.target}
        </code>
      )}
      <span className="stream-waiting__dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
    </span>
  );
}

function activityLabel(activity: ToolActivity): string {
  return activity.target ? `${activity.title} ${activity.target}` : activity.title;
}

/**
 * The waiting line for reasoning. Same shape as a call's line — pulsing verb,
 * then the figure it has to report, then the dots — because from the outside
 * thinking and calling a tool are the same kind of wait.
 *
 * The token count only appears once the provider reports one, which for most of
 * them is at the end of the round.
 */
function ThinkingActivityLine({ thinking, label }: { thinking: LiveReasoningView; label: string }) {
  return (
    <span className="stream-waiting__activity">
      <span className="stream-waiting__activity-title pulse-text">{label}</span>
      {thinking.tokens !== undefined && (
        <RollingNumber
          className="stream-waiting__activity-count"
          value={formatCompactTokenCount(thinking.tokens)}
        />
      )}
      <span className="stream-waiting__dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
    </span>
  );
}

/**
 * Stable end-of-timeline activity surface for a live model round. The cat never
 * changes identity while the run is active; the calls in flight are layered
 * onto it as one line each, for as long as each one lasts.
 */
export function StreamWaitingIndicator({ contexts, tools, thinking = null, retryNotice = null }: StreamWaitingIndicatorProps) {
  const { t } = useI18n();
  const activities = toolActivities(contexts, tools, t);
  const thinkingLabel = t("正在思考", "Thinking");

  if (retryNotice) {
    const label = t(
      "连接中断，正在第 {attempt}/{max} 次重试",
      "Connection interrupted; retrying {attempt}/{max}",
      { attempt: retryNotice.attempt, max: retryNotice.maxAttempts }
    );
    return (
      <div
        className="stream-waiting stream-waiting--retry"
        role="status"
        aria-live="polite"
        aria-label={label}
        data-stream-waiting="true"
        data-stream-retry={String(retryNotice.attempt)}
      >
        <StreamWaitingCat />
        <span className="stream-waiting__activity">
          <span className="stream-waiting__activity-title pulse-text">{label}</span>
          <span className="stream-waiting__dots" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
        </span>
        <span className="stream-waiting__retry-message" title={retryNotice.message}>
          {retryNotice.message}
        </span>
      </div>
    );
  }

  const narrated = [
    ...(thinking ? [thinkingLabel] : []),
    ...activities.map(activityLabel)
  ];

  return (
    <div
      className={`stream-waiting${narrated.length ? " stream-waiting--tool" : ""}`}
      role="status"
      aria-live="polite"
      aria-label={
        narrated.length
          ? narrated.join(t("；", "; "))
          : t("模型正在生成", "Model is generating")
      }
      data-stream-waiting="true"
      data-stream-thinking={thinking ? "true" : undefined}
      data-pending-tool={activities[activities.length - 1]?.toolName}
    >
      <StreamWaitingCat />
      {narrated.length ? (
        <span className="stream-waiting__activities">
          {thinking && <ThinkingActivityLine thinking={thinking} label={thinkingLabel} />}
          {activities.map((activity) => (
            <ToolActivityLine activity={activity} key={activity.id} />
          ))}
        </span>
      ) : (
        <span className="sr-only">{t("模型正在生成", "Model is generating")}</span>
      )}
    </div>
  );
}
