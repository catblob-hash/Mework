/**
 * Line-level unified diff between two versions of one text.
 *
 * Word-level alignment (wordDiff) explains what changed inside one line pair;
 * this produces the whole unified patch a diff card renders. It runs once per
 * message-version pair, so it refuses pairs too large to align rather than
 * blocking a frame on them, reporting each side as wholly rewritten instead.
 */

export interface LineDiffOptions {
  /** Path label written into the `---`/`+++` headers. Defaults to no file header. */
  path?: string;
  /** Context lines kept around each hunk. Default 3. */
  context?: number;
  /** Above this line count on either side the pair is reported as wholly rewritten. */
  maxLines?: number;
  /** Above this `before × after` cell count the pair is reported as wholly rewritten. */
  maxCells?: number;
}

export interface LineDiffResult {
  /** Unified-diff text, parseable by `parseUnifiedDiff` from DiffOutput. Empty when the sides are identical. */
  patch: string;
  additions: number;
  deletions: number;
  /** True when the pair was too large to align and each side is reported wholly changed. */
  bailed: boolean;
}

const DEFAULT_MAX_LINES = 4000;
/**
 * Ceiling on the alignment table.
 *
 * The table is `before × after` cells, so two texts that each pass the line cap
 * can still multiply into something not worth allocating. A million cells is a
 * few milliseconds; past that a whole-file rewrite is the honest answer anyway,
 * because no reader walks a patch that size line by line.
 */
const DEFAULT_MAX_CELLS = 1_000_000;
const DEFAULT_CONTEXT = 3;

/**
 * Splits text into diff lines after normalising every line-ending flavour to `\n`.
 *
 * A trailing newline splits into a phantom empty final element; that element is
 * dropped on both sides alike, so a trailing newline never shows up as a change
 * of its own. A newline present on only one side is therefore not reported
 * either — the parser this feeds has no "\ No newline at end of file" marker to
 * carry that fact, so the least wrong reading is that the texts agree.
 */
function splitLines(text: string): string[] {
  const lines = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines;
}

type DiffOp = { kind: "common" | "delete" | "insert"; text: string };

/**
 * Aligns two line arrays by longest common subsequence.
 *
 * LCS rather than Myers: with the table already bounded by the guards above, the
 * quadratic table is affordable, and it has no special cases around an empty
 * side. Returns, for each side, a boolean per line saying whether it is part of
 * the common subsequence.
 */
function alignByLcs(
  before: readonly string[],
  after: readonly string[]
): { beforeKept: boolean[]; afterKept: boolean[] } {
  const rows = before.length;
  const columns = after.length;
  // One flat table, row-major, `(rows + 1) × (columns + 1)`.
  const table = new Uint32Array((rows + 1) * (columns + 1));
  for (let row = rows - 1; row >= 0; row -= 1) {
    for (let column = columns - 1; column >= 0; column -= 1) {
      const index = row * (columns + 1) + column;
      table[index] = before[row] === after[column]
        ? table[index + columns + 2] + 1
        : Math.max(table[index + columns + 1], table[index + 1]);
    }
  }
  const beforeKept = new Array<boolean>(rows).fill(false);
  const afterKept = new Array<boolean>(columns).fill(false);
  let row = 0;
  let column = 0;
  while (row < rows && column < columns) {
    const index = row * (columns + 1) + column;
    if (before[row] === after[column]) {
      beforeKept[row] = true;
      afterKept[column] = true;
      row += 1;
      column += 1;
      continue;
    }
    if (table[index + columns + 1] >= table[index + 1]) row += 1;
    else column += 1;
  }
  return { beforeKept, afterKept };
}

/** Turns the head, the aligned middle, and the tail into one ordered op list. */
function buildOps(
  beforeLines: readonly string[],
  afterLines: readonly string[],
  head: number,
  tail: number
): DiffOp[] {
  const beforeMiddle = beforeLines.slice(head, beforeLines.length - tail);
  const afterMiddle = afterLines.slice(head, afterLines.length - tail);
  const { beforeKept, afterKept } = alignByLcs(beforeMiddle, afterMiddle);

  const ops: DiffOp[] = [];
  for (let index = 0; index < head; index += 1) {
    ops.push({ kind: "common", text: beforeLines[index] });
  }
  let beforeIndex = 0;
  let afterIndex = 0;
  while (beforeIndex < beforeMiddle.length || afterIndex < afterMiddle.length) {
    if (
      beforeIndex < beforeMiddle.length
      && afterIndex < afterMiddle.length
      && beforeKept[beforeIndex]
      && afterKept[afterIndex]
    ) {
      ops.push({ kind: "common", text: beforeMiddle[beforeIndex] });
      beforeIndex += 1;
      afterIndex += 1;
      continue;
    }
    if (beforeIndex < beforeMiddle.length && !beforeKept[beforeIndex]) {
      ops.push({ kind: "delete", text: beforeMiddle[beforeIndex] });
      beforeIndex += 1;
      continue;
    }
    ops.push({ kind: "insert", text: afterMiddle[afterIndex] });
    afterIndex += 1;
  }
  for (let index = beforeLines.length - tail; index < beforeLines.length; index += 1) {
    ops.push({ kind: "common", text: beforeLines[index] });
  }
  return ops;
}

/**
 * Renders the ops as `@@` hunks, keeping `context` common lines around each run
 * of changes.
 *
 * Two runs whose separating common lines would fit inside the context of both
 * neighbours share one hunk; splitting there would draw the same common lines
 * twice. A side with no lines addresses the line before its would-be start,
 * which is what makes an empty side read as `-0,0` or `+0,0`.
 */
function buildHunks(ops: readonly DiffOp[], context: number): string[] {
  const changedIndices: number[] = [];
  for (let index = 0; index < ops.length; index += 1) {
    if (ops[index].kind !== "common") changedIndices.push(index);
  }
  if (changedIndices.length === 0) return [];

  const groups: number[][] = [];
  let current: number[] = [];
  for (const index of changedIndices) {
    if (current.length > 0 && index - current[current.length - 1] - 1 <= context * 2) {
      current.push(index);
      continue;
    }
    current = [index];
    groups.push(current);
  }

  const hunks: string[] = [];
  for (const group of groups) {
    const from = Math.max(0, group[0] - context);
    const to = Math.min(ops.length, group[group.length - 1] + 1 + context);
    // Starts advance past everything before the hunk; counts cover only the
    // hunk's own lines.
    let oldStart = 1;
    let newStart = 1;
    for (let index = 0; index < from; index += 1) {
      if (ops[index].kind !== "insert") oldStart += 1;
      if (ops[index].kind !== "delete") newStart += 1;
    }
    let oldCount = 0;
    let newCount = 0;
    for (let index = from; index < to; index += 1) {
      if (ops[index].kind !== "insert") oldCount += 1;
      if (ops[index].kind !== "delete") newCount += 1;
    }
    if (oldCount === 0) oldStart -= 1;
    if (newCount === 0) newStart -= 1;
    const lines = [`@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`];
    for (let index = from; index < to; index += 1) {
      const op = ops[index];
      const marker = op.kind === "common" ? " " : op.kind === "delete" ? "-" : "+";
      lines.push(`${marker}${op.text}`);
    }
    hunks.push(lines.join("\n"));
  }
  return hunks;
}

/**
 * Renders a pair too large to align as one hunk that replaces the whole text.
 *
 * No alignment was computed, so no context would be honest: every before line
 * goes out as a deletion and every after line as an addition, and the counts
 * still describe exactly what a reader would have to retype.
 */
function wholeRewrite(beforeLines: readonly string[], afterLines: readonly string[]): string[] {
  const oldStart = beforeLines.length > 0 ? 1 : 0;
  const newStart = afterLines.length > 0 ? 1 : 0;
  const lines = [`@@ -${oldStart},${beforeLines.length} +${newStart},${afterLines.length} @@`];
  for (const line of beforeLines) lines.push(`-${line}`);
  for (const line of afterLines) lines.push(`+${line}`);
  return lines;
}

export function lineDiff(
  before: string,
  after: string,
  options?: LineDiffOptions
): LineDiffResult {
  const maxLines = options?.maxLines ?? DEFAULT_MAX_LINES;
  const maxCells = options?.maxCells ?? DEFAULT_MAX_CELLS;
  const context = options?.context ?? DEFAULT_CONTEXT;
  const beforeLines = splitLines(before);
  const afterLines = splitLines(after);

  if (
    beforeLines.length === afterLines.length
    && beforeLines.every((line, index) => line === afterLines[index])
  ) {
    return { patch: "", additions: 0, deletions: 0, bailed: false };
  }

  // The common head and tail are almost always most of the text; stripping them
  // first is what keeps the table small enough to be worth allocating at all.
  let head = 0;
  const shortest = Math.min(beforeLines.length, afterLines.length);
  while (head < shortest && beforeLines[head] === afterLines[head]) head += 1;
  let tail = 0;
  while (
    tail < shortest - head
    && beforeLines[beforeLines.length - 1 - tail] === afterLines[afterLines.length - 1 - tail]
  ) tail += 1;

  const beforeMiddleLength = beforeLines.length - tail - head;
  const afterMiddleLength = afterLines.length - tail - head;
  if (
    beforeMiddleLength > maxLines
    || afterMiddleLength > maxLines
    || (beforeMiddleLength + 1) * (afterMiddleLength + 1) > maxCells
  ) {
    return {
      patch: wholeRewrite(beforeLines, afterLines).join("\n"),
      additions: afterLines.length,
      deletions: beforeLines.length,
      bailed: true
    };
  }

  const ops = buildOps(beforeLines, afterLines, head, tail);
  const hunks = buildHunks(ops, context);
  const prefix = options?.path ? [`--- a/${options.path}`, `+++ b/${options.path}`] : [];
  const patch = [...prefix, ...hunks].join("\n");
  // Counted from the ops, not by scanning the patch text: a content line that
  // itself starts with `+` or `-` is still just one op, and every op lands in
  // exactly one hunk, so this matches what the renderer's parser reports.
  let additions = 0;
  let deletions = 0;
  for (const op of ops) {
    if (op.kind === "insert") additions += 1;
    else if (op.kind === "delete") deletions += 1;
  }
  return { patch, additions, deletions, bailed: false };
}
