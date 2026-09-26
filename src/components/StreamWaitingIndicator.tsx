import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { RefObject } from "react";
import type { ContextItem, ToolDescriptor } from "../types";
import { useI18n } from "../i18n";
import { formatCompactTokenCount } from "../lib/contextTokens";
import { subscribeToolExplanations, toolExplanation, toolExplanationVersion } from "../lib/localModel";
import type { LiveReasoningView } from "../lib/runContexts";
import { RollingNumber } from "./RollingNumber";
import {
  CAT_BODY_WITHOUT_PAWS,
  CAT_EYE_WHITES,
  CAT_FAR_PAW,
  CAT_LAPTOP,
  CAT_LEDGE_Y,
  CAT_LIDS,
  CAT_MUZZLE,
  CAT_NEAR_PAW,
  CAT_ON_LEDGE,
  CAT_PUPILS,
  CAT_SCENE_VIEWBOX,
  CAT_TAIL,
  CAT_WHISKERS
} from "./catArt";
import { getToolPresentation, isHoistedToolCall } from "./ToolRenderers";

type Translate = ReturnType<typeof useI18n>["t"];

export const CAT_MOODS = ["work", "slack"] as const;
export type CatMood = (typeof CAT_MOODS)[number];

/**
 * Loop period of each mood: the shortest interval after which every loop of that
 * mood is back on its 0% keyframe, which for all of them is the resting pose.
 * Every `animation` duration under `.stream-waiting__cat--<mood>` in
 * conversation.css divides the entry here, and at least one of them equals it —
 * that one is the clock a mood change is aligned against.
 */
export const CAT_MOOD_CYCLE_MS: Record<CatMood, number> = {
  work: 4_800,
  slack: 4_800
};

/**
 * How long each mood lasts, as a window. The cat is mostly at its laptop, since
 * that is what the round beside it is doing, and slacks off between spells of
 * it. These are only the bounds: the dwell actually used is quantised up to whole
 * loop cycles, so the real choices are 9.6s or 14.4s at work and 4.8s or 9.6s
 * slacking.
 */
const CAT_DWELL_WINDOW_MS: Record<CatMood, readonly [min: number, max: number]> = {
  work: [9_000, 15_000],
  slack: [4_000, 10_000]
};

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/** Close enough to a loop boundary that waiting another frame would show nothing. */
const CAT_ALIGNMENT_TOLERANCE_MS = 16;

/**
 * The dwells available to one mood: every whole number of loop cycles that lands
 * inside its window. A mood whose single cycle already overruns the window gets
 * one cycle rather than a truncated one — a change mid-loop is exactly the jump
 * cut the alignment below exists to avoid.
 */
export function catDwellChoicesMs(mood: CatMood): number[] {
  const period = CAT_MOOD_CYCLE_MS[mood];
  const [min, max] = CAT_DWELL_WINDOW_MS[mood];
  const first = Math.max(1, Math.ceil(min / period));
  const last = Math.max(first, Math.floor(max / period));
  const choices: number[] = [];
  for (let count = first; count <= last; count += 1) choices.push(count * period);
  return choices;
}

/**
 * How much longer the mood has to run before every loop is on its resting pose.
 *
 * A dwell that is a whole number of periods is only a whole number of periods on
 * the JavaScript clock. The animation's clock starts at a style resolution this
 * code never sees, timers fire late, and a `display: none` spell restarts the
 * loops at a moment nothing here was told about — so the two drift. Asking the
 * animation itself where it is turns the dwell from an assumption into a
 * measurement.
 *
 * Zero when the answer cannot be measured (no Web Animations API, no animation
 * running, or already on the boundary), which degrades to trusting the clock.
 */
function msUntilRest(root: SVGGElement | null, mood: CatMood): number {
  if (!root || typeof root.getAnimations !== "function") return 0;
  const period = CAT_MOOD_CYCLE_MS[mood];
  // The mood declares at least one loop whose duration is the whole period;
  // every other one divides it, so that one alone says where the pose is.
  const master = root.getAnimations({ subtree: true }).find((animation) => {
    const duration = animation.effect?.getTiming().duration;
    return typeof duration === "number" && Math.round(duration) === period;
  });
  if (!master) return 0;
  const remaining = period - (Number(master.currentTime ?? 0) % period);
  return remaining <= CAT_ALIGNMENT_TOLERANCE_MS || remaining >= period ? 0 : remaining;
}

/**
 * Whether a mood change would actually be seen easing from one pose to the other.
 *
 * Three things stop it. A hidden document suspends animation and throttles
 * timers, so a change made there is one the user never sees happen. Under
 * `prefers-reduced-motion` the global kill switch in feedback.css cuts every
 * transition to .01ms, which would turn the change into a jump cut. And the
 * conversation pane is `display: none` behind any open task, subagent or preview
 * page, where nothing transitions at all.
 *
 * A missing `matchMedia` — jsdom has none at all — reports nothing, which is not
 * the same as reporting a reduction, so it does not block. `checkVisibility` is
 * treated the same way.
 */
function changeCanPlay(root: SVGGElement | null): boolean {
  if (typeof document !== "undefined" && document.visibilityState !== "visible") return false;
  if (root && typeof root.checkVisibility === "function" && !root.checkVisibility()) return false;
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return true;
  return !window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

/** The document-wide half of {@link changeCanPlay}, which is the part that has events. */
function documentAllowsMotion(): boolean {
  return changeCanPlay(null);
}

function useMotionAllowed(): boolean {
  const [allowed, setAllowed] = useState(documentAllowsMotion);
  useEffect(() => {
    const sync = () => setAllowed(documentAllowsMotion());
    sync();
    const media = typeof window.matchMedia === "function" ? window.matchMedia(REDUCED_MOTION_QUERY) : null;
    media?.addEventListener("change", sync);
    document.addEventListener("visibilitychange", sync);
    return () => {
      media?.removeEventListener("change", sync);
      document.removeEventListener("visibilitychange", sync);
    };
  }, []);
  return allowed;
}

/**
 * Work, slack, work: the cat alternates between its two moods.
 *
 * Only the change is timed here. How it looks is the stylesheet's: the two moods
 * are two poses (where the pupils sit, how far the lids hang) that the change
 * eases between, each with its own loops running on top. Those loops are torn
 * down and restarted by the change, which is why it waits for them all to come
 * round to rest first — the pose then moves smoothly and nothing else jumps.
 *
 * The dwell is drawn in the effect, never in render: the app mounts under
 * StrictMode, which replays render, and a replayed draw would be a choice the
 * timer never made.
 */
function useCatMood(): { mood: CatMood; root: RefObject<SVGGElement | null> } {
  const motionAllowed = useMotionAllowed();
  const [mood, setMood] = useState<CatMood>(CAT_MOODS[0]);
  const root = useRef<SVGGElement | null>(null);

  useEffect(() => {
    if (!motionAllowed) return undefined;
    let timer = 0;
    const drawDwell = () => {
      const choices = catDwellChoicesMs(mood);
      return choices[Math.floor(Math.random() * choices.length)]!;
    };
    const schedule = (delay: number) => {
      timer = window.setTimeout(() => {
        // Whether the change can be seen has to be re-read here, not trusted
        // from the last render. The document and the preference both announce
        // themselves, but a pane going `display: none` announces nothing at all
        // — looking again on each dwell is the only way back from it.
        if (!changeCanPlay(root.current)) {
          schedule(drawDwell());
          return;
        }
        // Land on the loop boundary rather than near it: the dwell counted whole
        // periods on the JavaScript clock, and the animation's clock has its own
        // idea of where it is.
        const wait = msUntilRest(root.current, mood);
        if (wait > 0) {
          schedule(wait);
          return;
        }
        setMood(mood === "work" ? "slack" : "work");
      }, Math.max(0, delay));
    };
    schedule(drawDwell());
    return () => window.clearTimeout(timer);
  }, [motionAllowed, mood]);

  return { mood, root };
}

/**
 * The composer's cat, at its laptop beside a streaming round: the same drawing
 * in the same scene (`catArt.ts`), on a ledge of its own instead of the
 * composer's border.
 *
 * It has two moods and nothing else. At work it types with its far paw, works
 * the mouse with the near one, and keeps its eyes on the screen; slacking, it
 * leaves both paws where they are, looks away from the screen, and gets drowsy.
 * The laptop stays open either way.
 *
 * Draw order is load-bearing: the paws, pupils and lids are drawn after the face
 * because they are the head's own colour laid over it, visible only where they
 * cross a hole — `catArt.ts` explains why each is freed that way. The laptop and
 * the ledge sit outside the breathing group, so they stay still while the cat
 * breathes against them.
 */
function StreamWaitingCat() {
  const { mood, root } = useCatMood();
  return (
    <svg
      className={`stream-waiting__cat stream-waiting__cat--${mood}`}
      viewBox={CAT_SCENE_VIEWBOX}
      aria-hidden="true"
      data-cat-mood={mood}
    >
      <g className="stream-cat" ref={root}>
        {/* One pixel thick at the size the stylesheet draws the cat — 16px of a 610-unit scene. */}
        <rect className="stream-cat-ledge" x={300} y={CAT_LEDGE_Y} width={690} height={38.125} rx={19.0625} />
        <path className="stream-cat-tail" d={CAT_TAIL} />
        <path className="stream-cat-laptop" transform={CAT_ON_LEDGE} d={CAT_LAPTOP} />
        <g className="stream-cat-figure">
          <g transform={CAT_ON_LEDGE}>
            <path
              className="stream-cat-body"
              fillRule="evenodd"
              d={`${CAT_BODY_WITHOUT_PAWS}${CAT_EYE_WHITES}${CAT_WHISKERS}${CAT_MUZZLE}`}
            />
            <path className="stream-cat-paw stream-cat-paw--far" d={CAT_FAR_PAW} />
            <path className="stream-cat-paw stream-cat-paw--near" d={CAT_NEAR_PAW} />
            <g className="stream-cat-gaze">
              <path className="stream-cat-pupils" d={CAT_PUPILS} />
            </g>
            <g className="stream-cat-lids">
              <path className="stream-cat-blink" d={CAT_LIDS} />
            </g>
          </g>
        </g>
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
  /**
   * Rows the tasks pane lists as running. Narrated as one more line, which
   * opens that pane; drawn only when there is a pane to open.
   */
  runningTaskCount?: number;
  onOpenTasks?: () => void;
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
    const presentation = getToolPresentation(context, descriptor, t, toolExplanation(context.id));
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
 * onto it as one line each, for as long as each one lasts, and the tasks still
 * running beside the round as one more, which opens the tasks pane.
 */
export function StreamWaitingIndicator({
  contexts,
  tools,
  thinking = null,
  retryNotice = null,
  runningTaskCount = 0,
  onOpenTasks
}: StreamWaitingIndicatorProps) {
  const { t } = useI18n();
  useSyncExternalStore(subscribeToolExplanations, toolExplanationVersion);
  const activities = toolActivities(contexts, tools, t);
  const thinkingLabel = t("正在思考", "Thinking");
  const tasksLabel = runningTaskCount > 0 && onOpenTasks
    ? runningTaskCount === 1
      ? t("1 个任务正在运行", "1 task running")
      : t("{count} 个任务正在运行", "{count} tasks running", { count: runningTaskCount })
    : null;

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
  const generatingLabel = t("模型正在生成", "Model is generating");
  const announced = [
    ...(narrated.length ? narrated : [generatingLabel]),
    ...(tasksLabel ? [tasksLabel] : [])
  ];

  return (
    <div
      className={`stream-waiting${narrated.length ? " stream-waiting--tool" : ""}`}
      role="status"
      aria-live="polite"
      aria-label={announced.join(t("；", "; "))}
      data-stream-waiting="true"
      data-stream-thinking={thinking ? "true" : undefined}
      data-pending-tool={activities[activities.length - 1]?.toolName}
    >
      <StreamWaitingCat />
      {narrated.length || tasksLabel ? (
        <span className="stream-waiting__activities">
          {thinking && <ThinkingActivityLine thinking={thinking} label={thinkingLabel} />}
          {activities.map((activity) => (
            <ToolActivityLine activity={activity} key={activity.id} />
          ))}
          {tasksLabel && (
            <button type="button" className="stream-waiting__tasks" onClick={onOpenTasks}>
              {tasksLabel}
            </button>
          )}
        </span>
      ) : (
        <span className="sr-only">{generatingLabel}</span>
      )}
    </div>
  );
}
