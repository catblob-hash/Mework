/**
 * How a path is shortened when it is drawn somewhere too narrow for it.
 *
 * A path is read from both ends — the start says where it lives, the end says what it is — so it
 * gives way in the middle, a whole directory at a time, and keeps its first and last names:
 *
 *     /Users/me/Desktop/projects/mework  →  /Users/…/projects/mework  →  /Users/…/mework
 *
 * Names come back from the tail end first, since the directories nearest the last name say the
 * most about it. When even `first/…/last` does not fit, those two names are shortened in their own
 * middles — the first one before the last, which is the name the path is about:
 *
 *     /Users/…/mework  →  /U…s/…/mework  →  /U…/…/me…rk
 *
 * Every separator is shown as written, `\` or `/`, and a root, a drive or a UNC prefix stays with
 * the first name.
 */

const PATH_ELLIPSIS = "…";

/** `name` cut to `keep` characters around an ellipsis, keeping its start and its end. */
function shortenName(name: string, keep: number): string {
  const characters = Array.from(name);
  if (keep >= characters.length) return name;
  const start = Math.ceil(keep / 2);
  const end = Math.floor(keep / 2);
  return `${characters.slice(0, start).join("")}${PATH_ELLIPSIS}${end ? characters.slice(-end).join("") : ""}`;
}

/** The largest `keep` in `[1, most]` that `fits`, or null when even one character does not. */
function largestFitting(most: number, fits: (keep: number) => boolean): number | null {
  if (most < 1 || !fits(1)) return null;
  let low = 1;
  let high = most;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (fits(middle)) low = middle;
    else high = middle - 1;
  }
  return low;
}

/**
 * The longest shortening of `path` that `fits`. When nothing does, the shortest form there is —
 * a cut first and last name — so the caller's own overflow clips as little as it can.
 */
export function elidePath(path: string, fits: (text: string) => boolean): string {
  if (fits(path)) return path;
  // Separators are kept as the tokens between names, runs and all, so `\\server` and a trailing
  // `/` come back out exactly as they went in.
  const tokens = path.split(/([\\/]+)/);
  const named = tokens.flatMap((token, index) => (index % 2 === 0 && token ? [index] : []));
  if (!named.length) return path;
  const count = named.length;

  /** The first `head` names, a gap, then the last `tail` names, with names replaced as given. */
  const compose = (head: number, tail: number, replaced: ReadonlyMap<number, string>): string => {
    const part = (from: number, to: number) => tokens
      .slice(from, to)
      .map((token, offset) => replaced.get(from + offset) ?? token)
      .join("");
    if (head + tail >= count) return part(0, tokens.length);
    const headEnd = named[head - 1];
    const tailStart = named[count - tail];
    return `${part(0, headEnd + 1)}${tokens[headEnd + 1]}${PATH_ELLIPSIS}${tokens[tailStart - 1]}${part(tailStart, tokens.length)}`;
  };

  const none = new Map<number, string>();
  // Two names leave nothing in between to drop; one name is all there is.
  let head = count >= 3 ? 1 : count;
  let tail = count >= 3 ? 1 : 0;
  if (count >= 3 && fits(compose(head, tail, none))) {
    let grew = true;
    while (grew) {
      grew = false;
      if (head + tail + 1 < count && fits(compose(head, tail + 1, none))) {
        tail += 1;
        grew = true;
      }
      if (head + tail + 1 < count && fits(compose(head + 1, tail, none))) {
        head += 1;
        grew = true;
      }
    }
    return compose(head, tail, none);
  }

  // `first/…/last` is already too long: shorten the first name, then the last.
  const first = named[0];
  const last = named[count - 1];
  const replaced = new Map<number, string>();
  const shorten = (index: number) => {
    const name = tokens[index];
    const length = Array.from(name).length;
    const keep = largestFitting(length - 1, (candidate) => fits(compose(head, tail, new Map(replaced).set(index, shortenName(name, candidate)))));
    replaced.set(index, shortenName(name, keep ?? 1));
    return keep !== null;
  };
  if (first !== last && shorten(first)) return compose(head, tail, replaced);
  shorten(last);
  return compose(head, tail, replaced);
}
