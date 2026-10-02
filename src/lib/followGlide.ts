/**
 * Follow-output that glides instead of jumping.
 *
 * A stream grows the page ten times a second, and snapping to the new bottom
 * each time moves the whole transcript in steps: a line's worth of text
 * lurches up every 100 ms. The glide is a critically damped spring pulled
 * toward the bottom, re-aimed every frame, so the growth of successive commits
 * blends into one steady motion — a second-order follower smooths the velocity
 * as well as the position, where an exponential ease would lurch at every
 * commit and then coast.
 *
 * Critical damping never overshoots, so from rest the glide only ever moves
 * down: an upward step is how the scroller recognises the reader leaving the
 * bottom, and the glide must never look like one.
 */

/** Spring rate, per second. A line-high step is 90 % covered in about 200 ms. */
const FOLLOW_GLIDE_RATE = 18;

/** Longest frame the spring integrates, so a stalled tab resumes gently. */
const MAX_STEP_SECONDS = 0.05;

export interface FollowGlide {
  /** Where the glide has the page, unrounded; the scroller keeps whole pixels in WebKit. */
  position: number;
  /** Pixels per second, positive downward. */
  velocity: number;
  /** Timestamp of the frame that produced this state, or null before the first. */
  time: number | null;
}

/**
 * One frame of the glide toward `target`, the scroll offset of the bottom.
 * `done` means it has arrived and nothing is left moving.
 */
export function stepFollowGlide(
  glide: FollowGlide,
  target: number,
  now: number
): FollowGlide & { done: boolean } {
  const seconds = glide.time === null ? 1 / 60 : Math.min(MAX_STEP_SECONDS, Math.max(0, now - glide.time) / 1000);
  const offset = glide.position - target;
  const rate = FOLLOW_GLIDE_RATE;
  const decay = Math.exp(-rate * seconds);
  const drive = glide.velocity + rate * offset;
  const nextOffset = (offset + drive * seconds) * decay;
  const velocity = (glide.velocity - rate * drive * seconds) * decay;
  if (Math.abs(nextOffset) < 0.5 && Math.abs(velocity) < 20) {
    return { position: target, velocity: 0, time: now, done: true };
  }
  return {
    position: Math.min(target, Math.max(glide.position, target + nextOffset)),
    velocity,
    time: now,
    done: false
  };
}
