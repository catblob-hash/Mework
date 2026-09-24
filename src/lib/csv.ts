/**
 * Delimited text — CSV, TSV and their relatives — read into rows.
 *
 * RFC 4180 quoting is honoured: a quoted field may hold the delimiter, a line
 * break, or a doubled quote. Everything else is taken as written, because a
 * table viewer that refuses a slightly malformed file shows less than one that
 * reads it the way a spreadsheet would.
 */

export interface DelimitedTable {
  rows: string[][];
  /** True when parsing stopped at `maxRows` and the rest of the file was not read. */
  truncated: boolean;
  delimiter: string;
}

const CANDIDATES = [",", "\t", ";", "|"];

/**
 * The delimiter a file most plausibly uses: the candidate that appears the same
 * non-zero number of times on the most of its first lines.
 */
export function guessDelimiter(text: string): string {
  const lines = text.split(/\r?\n/, 20).filter((line) => line.trim());
  let best = ",";
  let bestScore = -1;
  for (const candidate of CANDIDATES) {
    const counts = lines.map((line) => line.split(candidate).length - 1);
    const first = counts[0] ?? 0;
    if (first === 0) continue;
    const score = counts.filter((count) => count === first).length * 100 + first;
    if (score > bestScore) {
      best = candidate;
      bestScore = score;
    }
  }
  return best;
}

export function parseDelimited(text: string, delimiter: string, maxRows = Number.POSITIVE_INFINITY): DelimitedTable {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  let index = text.charCodeAt(0) === 0xfeff ? 1 : 0;
  const endRow = () => {
    row.push(field);
    field = "";
    // A trailing newline, or a blank line between records, is not a row of one empty cell.
    if (!(row.length === 1 && row[0] === "")) rows.push(row);
    row = [];
  };
  while (index < text.length) {
    const character = text[index];
    if (quoted) {
      if (character === "\"") {
        if (text[index + 1] === "\"") {
          field += "\"";
          index += 2;
          continue;
        }
        quoted = false;
        index += 1;
        continue;
      }
      field += character;
      index += 1;
      continue;
    }
    if (character === "\"" && field === "") {
      quoted = true;
      index += 1;
      continue;
    }
    if (text.startsWith(delimiter, index)) {
      row.push(field);
      field = "";
      index += delimiter.length;
      continue;
    }
    if (character === "\r" || character === "\n") {
      endRow();
      if (rows.length >= maxRows) return { rows, truncated: true, delimiter };
      index += character === "\r" && text[index + 1] === "\n" ? 2 : 1;
      continue;
    }
    field += character;
    index += 1;
  }
  if (field !== "" || row.length) endRow();
  return { rows, truncated: false, delimiter };
}

/** Whether every non-empty value in a column reads as a number, so it can be right-aligned. */
export function numericColumns(rows: readonly (readonly string[])[], from: number): boolean[] {
  const width = rows.reduce((max, row) => Math.max(max, row.length), 0);
  const result: boolean[] = [];
  for (let column = 0; column < width; column += 1) {
    let seen = 0;
    let numeric = true;
    for (let index = from; index < rows.length && numeric; index += 1) {
      const value = rows[index][column]?.trim() ?? "";
      if (!value) continue;
      seen += 1;
      numeric = /^[-+]?(?:\d{1,3}(?:,\d{3})+|\d+)?(?:\.\d+)?(?:[eE][-+]?\d+)?%?$/.test(value) && /\d/.test(value);
    }
    result.push(numeric && seen > 0);
  }
  return result;
}
